//! The MDEC (motion decoder, 1F801820 / 1F801824): it turns run-length coded DCT blocks
//! into pixels for FMVs and compressed images. Written from Nocash PSX-SPX's MDEC chapter.
//!
//! - Commands (1F801820 write): 1 decode macroblocks (bits 27-28 output depth: 4 / 8 /
//!   24 / 15-bit; 26 signed; 25 bit 15 set; 0-15 the parameter word count), 2 set the
//!   quantisation tables (luminance, then chrominance when bit 0 is set), 3 set the IDCT
//!   scale table (64 signed halfwords).
//! - Decoding: each block is a DC value with its quantiser scale, then (skip, AC) pairs,
//!   ending at FE00; values are dequantised, put back in zigzag order, and run through a
//!   two-pass 8x8 IDCT with the uploaded scale table. Colour macroblocks are Cr, Cb, Y1-Y4
//!   (16x16 pixels); monochrome ones a single Y block (8x8).
//! - Input arrives through DMA channel 0 (or the CPU), output leaves through DMA channel 1
//!   (or reads of 1F801820). A macroblock is decoded as soon as its data is in, but its
//!   output only becomes readable after the time the real chip takes (BLOCK_CYCLES per
//!   8x8 block): movie players start the next frame's work while the MDEC is busy, and
//!   instant output finishes the frame before they are ready for it (Xenogears).
//!   Channel 1 waits until enough output is readable.
//! - Status (1F801824 read): 31 output empty, 29 busy, 28 / 27 DMA in / out request,
//!   23-26 the command's format bits, 16-18 current block, 0-15 parameters left - 1.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

const ZIGZAG: [usize; 64] = [
    0, 1, 5, 6, 14, 15, 27, 28, 2, 4, 7, 13, 16, 26, 29, 42, 3, 8, 12, 17, 25, 30, 41, 43, 9, 11, 18, 24, 31, 40, 44, 53,
    10, 19, 23, 32, 39, 45, 52, 54, 20, 22, 33, 38, 46, 51, 55, 60, 21, 34, 37, 47, 50, 56, 59, 61, 35, 36, 48, 49, 57, 58, 62, 63,
];

fn zagzig() -> [usize; 64] {
    let mut z = [0; 64];
    for (i, &v) in ZIGZAG.iter().enumerate() { z[v] = i; }
    z
}

/// CPU cycles to decode one 8x8 block (about 12,000 colour macroblocks a second)
const BLOCK_CYCLES: u32 = 448;

#[derive(Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
enum Cmd {
    #[default]
    Idle,
    Decode,
    Quant,
    Scale,
}

#[derive(Serialize, Deserialize)]
pub struct Mdec {
    ctrl: u32,
    cmd: Cmd,
    /// the command word (format bits)
    word: u32,
    remaining: u32,
    /// halfwords of coded data not yet decoded
    input: Vec<u16>,
    params: Vec<u32>,
    out: VecDeque<u32>,
    /// words of `out` that are readable
    avail: usize,
    /// decoded macroblocks still "in the chip": (cycles, words)
    busy: VecDeque<(u32, u32)>,
    budget: u32,
    quant_y: Vec<u8>,
    quant_c: Vec<u8>,
    scale: Vec<i16>,
    block: u32,
    #[serde(skip)]
    logged: u32,
}

impl Default for Mdec {
    fn default() -> Self { Self::new() }
}

impl Mdec {
    pub fn new() -> Self {
        Self {
            ctrl: 0,
            cmd: Cmd::Idle,
            word: 0,
            remaining: 0,
            input: Vec::new(),
            params: Vec::new(),
            out: VecDeque::new(),
            avail: 0,
            busy: VecDeque::new(),
            budget: 0,
            quant_y: vec![0; 64],
            quant_c: vec![0; 64],
            scale: vec![0; 64],
            block: 4,
            logged: 0,
        }
    }

    pub fn status(&self) -> u32 {
        let mut s = 0;
        if self.avail == 0 { s |= 1 << 31; }
        if self.cmd != Cmd::Idle || !self.out.is_empty() { s |= 1 << 29; }
        if self.ctrl & (1 << 30) != 0 && self.cmd != Cmd::Idle { s |= 1 << 28; }
        if self.ctrl & (1 << 29) != 0 && self.avail != 0 { s |= 1 << 27; }
        s |= ((self.word >> 25) & 0xF) << 23;
        s |= (self.block & 7) << 16;
        s |= self.remaining.wrapping_sub(1) & 0xFFFF;
        s
    }

    pub fn control(&mut self, v: u32) {
        if v & (1 << 31) != 0 {
            self.cmd = Cmd::Idle;
            self.remaining = 0;
            self.input.clear();
            self.params.clear();
            self.out.clear();
            self.avail = 0;
            self.busy.clear();
            self.budget = 0;
            self.word = 0;
            self.block = 4;
        }
        self.ctrl = v & 0x6000_0000;
    }

    pub fn debug(&self) -> String {
        format!("word {:08X} remaining {} input {} halfwords (first {:04X?}) out {} words ({} readable, {} macroblocks in the chip) status {:08X}",
            self.word, self.remaining, self.input.len(), &self.input[..self.input.len().min(8)], self.out.len(), self.avail, self.busy.len(), self.status())
    }

    pub fn read(&mut self) -> u32 {
        // a CPU read waits for the chip: let the next macroblock out now
        if self.avail == 0 {
            if let Some((_, w)) = self.busy.pop_front() { self.avail += w as usize; }
            self.budget = 0;
        }
        self.avail = self.avail.saturating_sub(1);
        self.out.pop_front().unwrap_or(0)
    }

    /// Readable output words
    pub fn out_words(&self) -> usize { self.avail }

    /// Time passes: decoded macroblocks become readable. True when some did.
    pub fn tick(&mut self, cycles: u32) -> bool {
        if self.busy.is_empty() { self.budget = 0; return false; }
        self.budget = self.budget.saturating_add(cycles);
        let mut released = false;
        while let Some(&(cost, w)) = self.busy.front() {
            if self.budget < cost { break; }
            self.budget -= cost;
            self.avail += w as usize;
            self.busy.pop_front();
            released = true;
        }
        if self.busy.is_empty() { self.budget = 0; }
        released
    }

    fn queue_output(&mut self, before: usize, blocks: u32) {
        let w = (self.out.len() - before) as u32;
        self.busy.push_back((blocks * BLOCK_CYCLES, w));
    }

    /// A word to 1F801820: a command, or its parameters
    pub fn write(&mut self, v: u32) {
        if self.cmd == Cmd::Idle {
            if super::debug() && self.logged < 300 {
                self.logged += 1;
                crate::dlog!("PS1 MDEC: command {v:08X} (output {} words waiting)", self.out.len());
            }
            self.word = v;
            self.remaining = v & 0xFFFF;
            self.cmd = match v >> 29 {
                1 => Cmd::Decode,
                2 => { self.remaining = if v & 1 != 0 { 32 } else { 16 }; Cmd::Quant }
                3 => { self.remaining = 32; Cmd::Scale }
                _ => Cmd::Idle,
            };
            if self.remaining == 0 { self.cmd = Cmd::Idle; }
            return;
        }
        self.remaining -= 1;
        match self.cmd {
            Cmd::Decode => {
                self.input.push(v as u16);
                self.input.push((v >> 16) as u16);
                self.decode_ready(self.remaining == 0);
            }
            _ => self.params.push(v),
        }
        if self.remaining == 0 {
            match self.cmd {
                Cmd::Quant => {
                    let bytes: Vec<u8> = self.params.iter().flat_map(|w| w.to_le_bytes()).collect();
                    self.quant_y.copy_from_slice(&bytes[..64]);
                    if bytes.len() >= 128 { self.quant_c.copy_from_slice(&bytes[64..128]); }
                }
                Cmd::Scale => {
                    for (i, w) in self.params.iter().enumerate() {
                        self.scale[2 * i] = *w as i16;
                        self.scale[2 * i + 1] = (*w >> 16) as i16;
                    }
                }
                _ => {}
            }
            self.params.clear();
            self.input.clear();
            self.cmd = Cmd::Idle;
        }
    }

    fn color(&self) -> bool { (self.word >> 27) & 2 != 0 }

    /// Decode every whole macroblock in the input
    fn decode_ready(&mut self, last: bool) {
        loop {
            let blocks = if self.color() { 6 } else { 1 };
            // find where `blocks` blocks end (each ends with an FE00 after its data)
            let Some(end) = Self::blocks_end(&self.input, blocks) else {
                if last { self.input.clear(); }
                return;
            };
            let data: Vec<u16> = self.input.drain(..end).collect();
            self.macroblock(&data);
        }
    }

    /// The index just past `n` complete blocks, or None if they aren't all in yet
    fn blocks_end(d: &[u16], n: usize) -> Option<usize> {
        let mut i = 0;
        for _ in 0..n {
            // padding before the DC value
            while i < d.len() && d[i] == 0xFE00 { i += 1; }
            if i >= d.len() { return None; }
            i += 1;                                  // the DC value
            let mut k = 0usize;
            loop {
                let n = *d.get(i)?;
                i += 1;
                k += ((n >> 10) & 0x3F) as usize + 1;
                if k > 63 { break; }
            }
        }
        Some(i)
    }

    fn macroblock(&mut self, d: &[u16]) {
        let mut pos = 0;
        let mut blk = [[0i32; 64]; 6];
        if self.color() {
            for (b, out) in blk.iter_mut().enumerate() {
                self.block = b as u32;
                let qt = if b < 2 { self.quant_c.clone() } else { self.quant_y.clone() };
                pos = self.rl_block(d, pos, &qt, out);
            }
            self.emit_color(&blk);
        } else {
            let qt = self.quant_y.clone();
            self.rl_block(d, pos, &qt, &mut blk[0]);
            self.emit_mono(&blk[0]);
        }
        self.block = 4;
    }

    /// One run-length block: dequantise, un-zigzag, IDCT. Returns the next position.
    fn rl_block(&self, d: &[u16], mut pos: usize, qt: &[u8], out: &mut [i32; 64]) -> usize {
        let zz = zagzig();
        let s10 = |n: u16| ((n as i32 & 0x3FF) << 22) >> 22;
        let mut blk = [0i32; 64];
        while pos < d.len() && d[pos] == 0xFE00 { pos += 1; }
        let Some(&n) = d.get(pos) else { return pos };
        pos += 1;
        let q = ((n >> 10) & 0x3F) as i32;
        let mut k = 0usize;
        let mut val = s10(n) * qt[0] as i32;
        loop {
            if q == 0 { val = s10(d[pos - 1]) * 2; }
            let v = val.clamp(-0x400, 0x3FF);
            if q > 0 { blk[zz[k]] = v; } else { blk[k] = v; }
            let Some(&n) = d.get(pos) else { break };
            pos += 1;
            k += ((n >> 10) & 0x3F) as usize + 1;
            if k > 63 { break; }
            val = (s10(n) * qt[k] as i32 * q + 4) / 8;
        }
        self.idct(&blk, out);
        pos
    }

    /// Two passes of the 8x8 IDCT with the scale table (1.0 = 2^15 a pass)
    fn idct(&self, src: &[i32; 64], out: &mut [i32; 64]) {
        let sc = &self.scale;
        let mut tmp = [0i64; 64];
        for x in 0..8 {
            for y in 0..8 {
                let mut sum = 0i64;
                for z in 0..8 { sum += src[y + z * 8] as i64 * sc[x + z * 8] as i64; }
                tmp[x + y * 8] = sum;
            }
        }
        for x in 0..8 {
            for y in 0..8 {
                let mut sum = 0i64;
                for z in 0..8 { sum += tmp[y + z * 8] * sc[x + z * 8] as i64; }
                // 2^15 x 2^15 x the 1/2 the table's scale carries... rounds to 2^32
                let v = (sum + (1 << 31)) >> 32;
                out[x + y * 8] = v.clamp(-128, 127) as i32;
            }
        }
    }

    fn emit_color(&mut self, b: &[[i32; 64]; 6]) {
        let before = self.out.len();
        self.emit_color_words(b);
        self.queue_output(before, 6);
    }

    fn emit_color_words(&mut self, b: &[[i32; 64]; 6]) {
        let (cr, cb) = (&b[0], &b[1]);
        let depth = (self.word >> 27) & 3;
        let signed = self.word & (1 << 26) != 0;
        let bit15 = if self.word & (1 << 25) != 0 { 0x8000u32 } else { 0 };
        let mut px = [[0u8; 3]; 256];
        for (yb, &(xx, yy)) in [(0usize, 0usize), (8, 0), (0, 8), (8, 8)].iter().enumerate() {
            let yblk = &b[2 + yb];
            for y in 0..8 {
                for x in 0..8 {
                    let c = ((x + xx) / 2) + ((y + yy) / 2) * 8;
                    let r = cr[c] as f32;
                    let bl = cb[c] as f32;
                    let g = -0.3437 * bl - 0.7143 * r;
                    let (r, bl) = (1.402 * r, 1.772 * bl);
                    let yv = yblk[x + y * 8] as f32;
                    let cl = |v: f32| -> u8 {
                        let v = (yv + v).round().clamp(-128.0, 127.0) as i32;
                        (if signed { v } else { v + 128 }) as u8
                    };
                    px[(x + xx) + (y + yy) * 16] = [cl(r), cl(g), cl(bl)];
                }
            }
        }
        if depth == 2 {
            // 24-bit: R, G, B bytes
            let bytes: Vec<u8> = px.iter().flatten().copied().collect();
            for w in bytes.chunks_exact(4) { self.out.push_back(u32::from_le_bytes([w[0], w[1], w[2], w[3]])); }
        } else {
            // 15-bit
            let h = |p: &[u8; 3]| -> u32 {
                let c = |v: u8| ((if signed { (v as i8 as i32 + 128) as u8 } else { v }) >> 3) as u32;
                c(p[0]) | c(p[1]) << 5 | c(p[2]) << 10 | bit15
            };
            for pair in px.chunks_exact(2) { self.out.push_back(h(&pair[0]) | h(&pair[1]) << 16); }
        }
    }

    fn emit_mono(&mut self, b: &[i32; 64]) {
        let before = self.out.len();
        self.emit_mono_words(b);
        self.queue_output(before, 1);
    }

    fn emit_mono_words(&mut self, b: &[i32; 64]) {
        let signed = self.word & (1 << 26) != 0;
        let px: Vec<u8> = b.iter().map(|&v| if signed { v as u8 } else { (v + 128) as u8 }).collect();
        if (self.word >> 27) & 3 == 0 {
            // 4-bit: low nibble first
            let nib: Vec<u8> = px.chunks_exact(2).map(|p| (p[0] >> 4) | (p[1] & 0xF0)).collect();
            for w in nib.chunks_exact(4) { self.out.push_back(u32::from_le_bytes([w[0], w[1], w[2], w[3]])); }
        } else {
            for w in px.chunks_exact(4) { self.out.push_back(u32::from_le_bytes([w[0], w[1], w[2], w[3]])); }
        }
    }
}
