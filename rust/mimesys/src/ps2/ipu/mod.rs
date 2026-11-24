//! The IPU (image processing unit): the MPEG-2 decoder games play their movies with.
//!
//! The EE gives it commands at 1000_2000 (IPU_CMD); the bitstream comes in through its
//! input FIFO (DMA channel 4, IPU_TO, or the CPU at 1000_7010) and pictures go out through
//! its output FIFO (channel 3, IPU_FROM, or the CPU at 1000_7000). Commands (ps2tek):
//! - BCLR: empty the input FIFO, set the bit pointer
//! - IDEC: decode a whole intra slice to RGB (macroblock types, quantiser codes, DCT blocks,
//!   colour conversion), until a start code
//! - BDEC: decode one macroblock's blocks to 16-bit YCbCr (the EE's libmpeg does motion
//!   compensation itself)
//! - VDEC: decode one VLC symbol (address increment, macroblock type, motion code, dmvector)
//! - FDEC: show the next 32 bits; SETIQ / SETVQ: load a quantiser matrix / the VQ palette
//! - CSC: 8-bit YCbCr macroblocks to RGB32 / RGB16; PACK: RGB32 to RGB16 / 4-bit VQ indices
//! - SETTH: the alpha thresholds for CSC
//!
//! Decoding follows the hardware as PCSX2 and Play! (BSD-2) model it: matrices in scan
//! order, libmpeg2's dequantisation, the IPU's fixed-point BT.601 conversion.
//!
//! The input: while a command runs it pulls quadwords from channel 4 as it needs them (the
//! hardware refills its 8-quadword FIFO the same way). If the stream runs dry, the command
//! is rolled back to where it started (IDEC: to its last finished macroblock) and tried
//! again once more data arrives. Output is kept until channel 3 takes it.

pub mod vlc;
#[cfg(test)]
mod tests;

use std::collections::VecDeque;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use super::dmac::Dmac;
use vlc::Vlc;

// IPU_CTRL fields
const CTRL_ECD: u32 = 1 << 14;
const CTRL_SCD: u32 = 1 << 15;
const CTRL_RST: u32 = 1 << 30;

const BCLR: u32 = 0;
const IDEC: u32 = 1;
const BDEC: u32 = 2;
const VDEC: u32 = 3;
const FDEC: u32 = 4;
const SETIQ: u32 = 5;
const SETVQ: u32 = 6;
const CSC: u32 = 7;
const PACK: u32 = 8;
const SETTH: u32 = 9;

struct Tables {
    dc_luma: Vlc,
    dc_chroma: Vlc,
    cbp: Vlc,
    dct0: Vlc,
    dct1: Vlc,
    mbai: Vlc,
    mbtype: [Vlc; 3],
    motion: Vlc,
    dmv: Vlc,
    idct: [[f32; 8]; 8],
}

fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| {
        let mut idct = [[0f32; 8]; 8];
        for (u, row) in idct.iter_mut().enumerate() {
            for (x, c) in row.iter_mut().enumerate() {
                let cu = if u == 0 { std::f32::consts::FRAC_1_SQRT_2 } else { 1.0 };
                *c = cu / 2.0 * (((2 * x + 1) as f32 * u as f32 * std::f32::consts::PI) / 16.0).cos();
            }
        }
        Tables {
            dc_luma: Vlc::from_strs(vlc::DC_LUMA),
            dc_chroma: Vlc::from_strs(vlc::DC_CHROMA),
            cbp: Vlc::from_strs(vlc::CBP),
            dct0: vlc::dct_zero(),
            dct1: vlc::dct_one(),
            mbai: vlc::vdec(vlc::MBAI),
            mbtype: [vlc::vdec(vlc::MBTYPE_I), vlc::vdec(vlc::MBTYPE_P), vlc::vdec(vlc::MBTYPE_B)],
            motion: vlc::vdec(vlc::MOTION),
            dmv: vlc::vdec(vlc::DMV),
            idct,
        }
    })
}

/// Why a command stopped
#[derive(Debug, PartialEq)]
enum Stop {
    /// not enough data yet
    Stall,
    /// a code that isn't in the table (ECD)
    Error,
}

type R<T> = Result<T, Stop>;

/// The input bitstream: the FIFO's quadwords (bytes in memory order, each MSB first) from
/// bit `pos`, topped up from IPU_TO as needed
struct Bits<'a> {
    fifo: &'a mut VecDeque<u128>,
    pos: u32,
    pull: &'a mut dyn FnMut() -> Option<u128>,
}

impl Bits<'_> {
    fn avail(&self) -> u32 { (self.fifo.len() as u32 * 128).saturating_sub(self.pos) }

    fn ensure(&mut self, n: u32) -> bool {
        while self.avail() < n {
            match (self.pull)() {
                Some(q) => self.fifo.push_back(q),
                None => return false,
            }
        }
        true
    }

    fn byte(&self, k: u32) -> u32 {
        self.fifo.get((k / 16) as usize).map_or(0, |q| (q >> (8 * (k % 16)) & 0xFF) as u32)
    }

    /// The next n (<= 32) bits, zeros past the end
    fn raw(&self, n: u32) -> u32 {
        if n == 0 { return 0; }
        let first = self.pos / 8;
        let last = (self.pos + n - 1) / 8;
        let mut acc = 0u64;
        for k in first..=last { acc = acc << 8 | self.byte(k) as u64; }
        let total = (last - first + 1) * 8;
        ((acc >> (total - self.pos % 8 - n)) & ((1u64 << n) - 1)) as u32
    }

    fn peek(&mut self, n: u32) -> R<u32> {
        if !self.ensure(n) { return Err(Stop::Stall); }
        Ok(self.raw(n))
    }

    fn get(&mut self, n: u32) -> R<u32> {
        let v = self.peek(n)?;
        self.pos += n;
        Ok(v)
    }

    fn skip(&mut self, n: u32) -> R<()> {
        if !self.ensure(n) { return Err(Stop::Stall); }
        self.pos += n;
        Ok(())
    }

    /// n bits as a signed number
    fn sget(&mut self, n: u32) -> R<i32> {
        let v = self.get(n)?;
        Ok(((v << (32 - n)) as i32) >> (32 - n))
    }

    fn align(&mut self) { self.pos = (self.pos + 7) & !7; }

    /// One VLC symbol: (code length, value)
    fn vlc(&mut self, t: &Vlc) -> R<(u32, i32)> {
        self.ensure(t.bits);
        let have = self.avail();
        let (len, val) = t.lut[self.raw(t.bits) as usize];
        if len == 0 {
            return Err(if have < t.bits { Stop::Stall } else { Stop::Error });
        }
        if len as u32 > have { return Err(Stop::Stall); }
        self.pos += len as u32;
        Ok((len as u32, val))
    }
}

/// The decoding settings from IPU_CTRL and the command
#[derive(Clone, Copy)]
struct Mode {
    mpeg1: bool,
    alt_scan: bool,
    intra_vlc: bool,
    q_nonlinear: bool,
    idp: u32,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Ipu {
    /// IPU_CTRL's writable bits (16-29) and ECD / SCD
    pub ctrl: u32,
    /// the command running (BUSY)
    pub busy: bool,
    pub cmd: u32,
    /// IPU_CMD's data (VDEC / FDEC results)
    pub data: u32,
    last: u32,
    /// the coded block pattern of the last BDEC (IPU_CTRL CBP)
    cbp: u32,
    iq: Vec<u8>,
    niq: Vec<u8>,
    vq: Vec<u16>,
    th0: u32,
    th1: u32,
    dc: [i32; 3],
    /// IDEC's progress: macroblocks done, the quantiser scale code
    idec_mbs: u32,
    idec_qsc: u32,
    /// the input FIFO (bit `bp` of its first quadword is next) and the output FIFO
    #[serde(skip)]
    fifo_in: VecDeque<u128>,
    pub bp: u32,
    #[serde(skip)]
    pub fifo_out: VecDeque<u128>,
    /// debugging: commands run, by number
    #[serde(skip)]
    pub counts: [u64; 10],
    #[serde(skip)]
    pub errors: u64,
}

impl Default for Ipu {
    fn default() -> Self {
        Self {
            ctrl: 0, busy: false, cmd: 0, data: 0, last: u32::MAX, cbp: 0, iq: vec![16; 64], niq: vec![16; 64],
            vq: vec![0; 16], th0: 0, th1: 0, dc: [128; 3], idec_mbs: 0, idec_qsc: 0, fifo_in: VecDeque::new(), bp: 0,
            fifo_out: VecDeque::new(), counts: [0; 10], errors: 0,
        }
    }
}

impl Ipu {
    fn mode(&self) -> Mode {
        Mode {
            mpeg1: self.ctrl >> 23 & 1 != 0,
            alt_scan: self.ctrl >> 20 & 1 != 0,
            intra_vlc: self.ctrl >> 21 & 1 != 0,
            q_nonlinear: self.ctrl >> 22 & 1 != 0,
            idp: (self.ctrl >> 16 & 3).min(2),
        }
    }

    /// Input FIFO state: (BP, IFC, FP) as IPU_BP shows it
    fn fifo_state(&self) -> (u32, u32, u32) {
        let mut ifc = (self.fifo_in.len() as u32).min(8);
        let mut fp = 0;
        if self.bp != 0 && ifc != 0 { fp = 1; ifc -= 1; }
        (self.bp, ifc, fp)
    }

    /// The next 32 bits in the FIFO (zeros past what's there)
    fn top(&mut self) -> u32 {
        let mut none = || None;
        let b = Bits { fifo: &mut self.fifo_in, pos: self.bp, pull: &mut none };
        b.raw(32)
    }

    fn bits_ready(&self) -> u32 { (self.fifo_in.len() as u32 * 128).saturating_sub(self.bp) }

    pub fn read(&mut self, a: u32) -> u32 {
        match a & 0xFF {
            0x00 => if self.last == VDEC || self.last == FDEC { self.data } else { self.top() },
            0x04 => if self.busy { 0x8000_0000 } else { 0 },
            0x10 => {
                let (_, ifc, _) = self.fifo_state();
                let ofc = (self.fifo_out.len() as u32).min(8);
                (self.ctrl & 0x3FFF_C000) | ifc | ofc << 4 | (self.cbp & 0x3F) << 8 | if self.busy { 0x8000_0000 } else { 0 }
            }
            0x20 => { let (bp, ifc, fp) = self.fifo_state(); bp | ifc << 8 | fp << 16 }
            0x30 => if self.busy { 0 } else { self.top() },
            0x34 => if self.busy || self.bits_ready() < 32 { 0x8000_0000 } else { 0 },
            _ => 0,
        }
    }

    /// A register write; true when it finished a command at once (an IPU interrupt)
    pub fn write(&mut self, a: u32, v: u32) -> bool {
        match a & 0xFF {
            0x00 => {
                self.ctrl &= !(CTRL_ECD | CTRL_SCD);
                let c = v >> 28;
                self.last = c;
                if (c as usize) < self.counts.len() { self.counts[c as usize] += 1; }
                match c {
                    BCLR => {
                        self.fifo_in.clear();
                        self.bp = v & 0x7F;
                        self.busy = false;
                        true
                    }
                    SETTH => {
                        self.th0 = v & 0x1FF;
                        self.th1 = v >> 16 & 0x1FF;
                        self.busy = false;
                        true
                    }
                    _ => {
                        self.cmd = v;
                        self.busy = true;
                        self.idec_mbs = 0;
                        self.idec_qsc = v >> 16 & 0x1F;
                        false
                    }
                }
            }
            0x10 => {
                if v & CTRL_RST != 0 { self.reset(); }
                self.ctrl = (self.ctrl & !0x3FFF_0000) | (v & 0x3FFF_0000 & !CTRL_RST);
                false
            }
            _ => false,
        }
    }

    fn reset(&mut self) {
        self.busy = false;
        self.last = u32::MAX;
        self.th0 = 0;
        self.th1 = 0;
        self.fifo_in.clear();
        self.fifo_out.clear();
        self.bp = 0;
        self.data = 0;
        self.ctrl &= !(CTRL_ECD | CTRL_SCD);
    }

    pub fn fifo_in_len(&self) -> usize { self.fifo_in.len() }

    /// The CPU writing the input FIFO (1000_7010)
    pub fn fifo_write(&mut self, q: u128) { self.fifo_in.push_back(q); }

    /// The CPU reading the output FIFO (1000_7000)
    pub fn fifo_read(&mut self) -> u128 { self.fifo_out.pop_front().unwrap_or(0) }

    /// Run: top up the input FIFO from IPU_TO, work on the command, hand output to IPU_FROM.
    /// True when a command finished (an IPU interrupt).
    pub fn step(&mut self, dmac: &mut Dmac, ram: &mut [u8], spr: &mut [u8]) -> bool {
        while self.fifo_in.len() < 8 {
            match dmac.ipu_to_pull(ram, spr) {
                Some(q) => self.fifo_in.push_back(q),
                None => break,
            }
        }
        let mut done = false;
        if self.busy {
            let mut pull = || dmac.ipu_to_pull(ram, spr);
            done = self.run(&mut pull);
            if done { self.busy = false; }
        }
        while let Some(&q) = self.fifo_out.front() {
            if !dmac.ipu_from_push(ram, spr, q) { break; }
            self.fifo_out.pop_front();
        }
        done
    }

    /// Work on the command; true when it's finished
    fn run(&mut self, pull: &mut dyn FnMut() -> Option<u128>) -> bool {
        let v = self.cmd;
        let c = v >> 28;
        let fb = v & 0x3F;
        let mode = self.mode();
        if c == IDEC { return self.idec(pull, mode); }
        let start = self.bp;
        let mut fifo = std::mem::take(&mut self.fifo_in);
        let mut out: Vec<u128> = Vec::new();
        let result = {
            let mut b = Bits { fifo: &mut fifo, pos: start, pull };
            let r: R<()> = (|| {
                match c {
                    BDEC => {
                        b.skip(fb)?;
                        let mbi = v >> 27 & 1 != 0;
                        let dcr = v >> 26 & 1 != 0;
                        let dt = v >> 25 & 1 != 0;
                        let qsc = v >> 16 & 0x1F;
                        let mut dc = self.dc;
                        if dcr { dc = [128 << mode.idp; 3]; }
                        let (blocks, cbp) = decode_mb(&mut b, mode, mbi, qsc, &mut dc, &self.iq, &self.niq)?;
                        self.dc = dc;
                        self.cbp = cbp;
                        let mb = arrange(&blocks, dt, mbi);
                        out = raw16_out(&mb);
                        self.check_start_code(&mut b)?;
                    }
                    VDEC => {
                        b.skip(fb)?;
                        let t = tables();
                        let table = match v >> 26 & 3 {
                            0 => &t.mbai,
                            1 => &t.mbtype[(((self.ctrl >> 24 & 7).max(1)) - 1).min(2) as usize],
                            2 => &t.motion,
                            _ => &t.dmv,
                        };
                        match b.vlc(table) {
                            Ok((_, val)) => self.data = val as u32,
                            Err(Stop::Error) => { self.data = 0; }
                            Err(e) => return Err(e),
                        }
                        if self.data == 0 { self.ctrl |= CTRL_ECD; }
                    }
                    FDEC => {
                        b.skip(fb)?;
                        self.data = b.peek(32)?;
                    }
                    SETIQ => {
                        b.skip(fb)?;
                        let mut m = vec![0u8; 64];
                        for x in m.iter_mut() { *x = b.get(8)? as u8; }
                        if v >> 27 & 1 != 0 { self.niq = m; } else { self.iq = m; }
                    }
                    SETVQ => {
                        let mut m = vec![0u16; 16];
                        for x in m.iter_mut() { *x = b.get(16)? as u16; }
                        // the palette is little-endian halfwords in memory
                        self.vq = m.iter().map(|h| h.swap_bytes()).collect();
                    }
                    CSC => {
                        let mbc = v & 0x7FF;
                        let (dte, ofm) = (v >> 26 & 1 != 0, v >> 27 & 1 != 0);
                        // all the macroblocks' data must be there first
                        if !b.ensure(mbc * 384 * 8) { return Err(Stop::Stall); }
                        for _ in 0..mbc {
                            let mut mb = [0u8; 384];
                            for x in mb.iter_mut() { *x = b.get(8)? as u8; }
                            let rgb = csc(&mb, self.th0, self.th1, false);
                            out.extend(rgb_out(&rgb, ofm, dte));
                        }
                    }
                    PACK => {
                        let mbc = v & 0x7FF;
                        let (dte, ofm) = (v >> 26 & 1 != 0, v >> 27 & 1 != 0);
                        if !b.ensure(mbc * 1024 * 8) { return Err(Stop::Stall); }
                        for _ in 0..mbc {
                            let mut rgb = [0u32; 256];
                            for x in rgb.iter_mut() {
                                let w = b.get(32)?;
                                *x = w.swap_bytes();
                            }
                            if ofm { out.extend(rgb_out(&rgb, true, dte)); } else { out.extend(self.vq_out(&rgb, dte)); }
                        }
                    }
                    _ => {}
                }
                Ok(())
            })();
            match r {
                Err(Stop::Stall) => None,
                other => Some((other, b.pos)),
            }
        };
        self.fifo_in = fifo;
        match result {
            None => {
                // not enough data: try again from the start later
                self.bp = start;
                false
            }
            Some((r, pos)) => {
                if r == Err(Stop::Error) { self.ctrl |= CTRL_ECD; self.errors += 1; }
                self.commit(pos);
                self.fifo_out.extend(out);
                true
            }
        }
    }

    /// The input consumed up to `pos`: whole quadwords leave the FIFO
    fn commit(&mut self, pos: u32) {
        let n = (pos / 128) as usize;
        for _ in 0..n.min(self.fifo_in.len()) { self.fifo_in.pop_front(); }
        self.bp = pos % 128;
    }

    /// After a macroblock: SCD if a start code follows
    /// After BDEC: SCD if a start code follows. A look only: the bits stay where the
    /// macroblock ended (the next macroblock can start with zero bits; skipping them
    /// threw the stream off and Metal Gear Solid 2's movie stalled)
    fn check_start_code(&mut self, b: &mut Bits) -> R<()> {
        let pos = b.pos;
        if start_code_after(b) { self.ctrl |= CTRL_SCD; }
        b.pos = pos;
        Ok(())
    }

    /// IDEC: macroblocks to RGB until a start code, resumable after each one
    fn idec(&mut self, pull: &mut dyn FnMut() -> Option<u128>, mode: Mode) -> bool {
        let v = self.cmd;
        let fb = v & 0x3F;
        let dtd = v >> 24 & 1 != 0;
        let sgn = v >> 25 & 1 != 0;
        let dte = v >> 26 & 1 != 0;
        let ofm = v >> 27 & 1 != 0;
        loop {
            let start = self.bp;
            let mut fifo = std::mem::take(&mut self.fifo_in);
            let mut qsc = self.idec_qsc;
            let mut dc = self.dc;
            let first = self.idec_mbs == 0;
            let (r, pos, finished, out) = {
                let mut b = Bits { fifo: &mut fifo, pos: start, pull: &mut *pull };
                let mut finished = false;
                let mut out = Vec::new();
                let r = (|| -> R<()> {
                    let t = tables();
                    if first {
                        b.skip(fb)?;
                        dc = [128 << mode.idp; 3];
                    } else {
                        // the next macroblock's address increment, or the slice's end
                        let mut inc = 0u32;
                        loop {
                            let full = b.ensure(16);
                            let code = b.raw(16);
                            if !full {
                                // a stream ending in eight zero bits ends the slice
                                if b.avail() >= 8 && b.raw(8) == 0 { finished = true; return Ok(()); }
                                return Err(Stop::Stall);
                            }
                            if code >= 0x0300 {
                                inc += (b.vlc(&t.mbai)?.1 & 0xFFFF) as u32;
                                break;
                            }
                            match code >> 5 {
                                0x08 => { b.pos += 11; inc += 33; }   // escape
                                0x0F => { b.pos += 11; }              // stuffing
                                _ => { finished = true; return Ok(()); }
                            }
                        }
                        if inc > 1 { dc = [128 << mode.idp; 3]; }
                    }
                    let (_, mbtype) = b.vlc(&t.mbtype[0])?;
                    let interlaced = if dtd { b.get(1)? == 1 } else { false };
                    if mbtype & 0x10 != 0 { qsc = b.get(5)?; }
                    let (blocks, _) = decode_mb(&mut b, mode, true, qsc, &mut dc, &self.iq, &self.niq)?;
                    let mb = arrange(&blocks, interlaced, true);
                    let mut raw8 = [0u8; 384];
                    for (k, x) in raw8.iter_mut().enumerate() { *x = mb[k].clamp(0, 255) as u8; }
                    out = rgb_out(&csc(&raw8, self.th0, self.th1, sgn), ofm, dte);
                    Ok(())
                })();
                let mut scd = false;
                if finished || r == Err(Stop::Error) { scd = start_code_after(&mut b); }
                if scd { self.ctrl |= CTRL_SCD; }
                (r, b.pos, finished, out)
            };
            self.fifo_in = fifo;
            match r {
                Err(Stop::Stall) => { self.bp = start; return false; }
                Err(Stop::Error) => { self.commit(pos); self.ctrl |= CTRL_ECD; self.errors += 1; return true; }
                Ok(()) => {
                    self.commit(pos);
                    if finished { return true; }
                    self.dc = dc;
                    self.idec_qsc = qsc;
                    self.idec_mbs += 1;
                    self.fifo_out.extend(out);
                }
            }
        }
    }

    /// PACK to 4-bit indices: the nearest VQ palette colour (RGB555)
    fn vq_out(&self, rgb: &[u32; 256], dte: bool) -> Vec<u128> {
        let c16 = to_rgb16(rgb, dte);
        let pal: Vec<(i32, i32, i32)> = self.vq.iter().map(|&p| ((p & 31) as i32, (p >> 5 & 31) as i32, (p >> 10 & 31) as i32)).collect();
        let near = |p: u16| -> u8 {
            let (r, g, b) = ((p & 31) as i32, (p >> 5 & 31) as i32, (p >> 10 & 31) as i32);
            let mut best = (i32::MAX, 0u8);
            for (k, &(pr, pg, pb)) in pal.iter().enumerate() {
                let d = (r - pr).pow(2) + (g - pg).pow(2) + (b - pb).pow(2);
                if d < best.0 { best = (d, k as u8); }
            }
            best.1
        };
        let mut bytes = Vec::with_capacity(128);
        for i in 0..16 {
            for j in 0..8 { bytes.push(near(c16[i * 16 + 2 * j]) | near(c16[i * 16 + 2 * j + 1]) << 4); }
        }
        bytes.chunks(16).map(|c| u128::from_le_bytes(c.try_into().unwrap())).collect()
    }
}

/// Eight zero bits next mean a start code follows: skip to it (byte-aligned, past zero
/// bytes); true when 00 00 01 is there
fn start_code_after(b: &mut Bits) -> bool {
    if !(b.ensure(8) && b.raw(8) == 0) { return false; }
    b.align();
    loop {
        if !b.ensure(24) { return false; }
        match b.raw(24) {
            0 => b.pos += 8,
            1 => return true,
            _ => return false,
        }
    }
}

/// A macroblock's six blocks (raster order, after the IDCT) and its coded block pattern
fn decode_mb(b: &mut Bits, mode: Mode, intra: bool, qsc: u32, dc: &mut [i32; 3], iq: &[u8], niq: &[u8]) -> R<([[i32; 64]; 6], u32)> {
    let t = tables();
    let cbp = if intra { 0x3F } else { b.vlc(&t.cbp)?.1 as u32 };
    let qs = if mode.q_nonlinear { vlc::NONLINEAR_Q[qsc as usize & 31] } else { (qsc as i32 & 31) * 2 };
    let mut blocks = [[0i32; 64]; 6];
    for (k, blk) in blocks.iter_mut().enumerate() {
        if cbp & (0x20 >> k) == 0 { continue; }
        let cc = if k < 4 { 0 } else { k - 3 };
        let mut coef = [0i32; 64];
        if intra { intra_block(b, mode, cc, qs, dc, iq, &mut coef)?; } else { non_intra_block(b, mode, qs, niq, &mut coef)?; }
        idct(&coef, blk);
    }
    Ok((blocks, cbp))
}

fn saturate(v: i32) -> i32 { v.clamp(-2048, 2047) }

fn intra_block(b: &mut Bits, mode: Mode, cc: usize, qs: i32, dc: &mut [i32; 3], w: &[u8], out: &mut [i32; 64]) -> R<()> {
    let t = tables();
    let scan = if mode.alt_scan { &vlc::ALTERNATE } else { &vlc::ZIGZAG };
    // DC: its size, then the difference
    let size = b.vlc(if cc == 0 { &t.dc_luma } else { &t.dc_chroma })?.1 as u32;
    let diff = if size == 0 {
        0
    } else {
        let v = b.get(size)? as i32;
        if v < 1 << (size - 1) { v + 1 - (1 << size) } else { v }
    };
    dc[cc] += diff;
    out[0] = dc[cc] << (3 - mode.idp);
    let table = if mode.intra_vlc && !mode.mpeg1 { &t.dct1 } else { &t.dct0 };
    let mut i = 1usize;
    loop {
        let (_, sym) = b.vlc(table)?;
        if sym == vlc::EOB { break; }
        let val;
        if sym == vlc::ESCAPE {
            let run = b.get(6)? as usize;
            i += run;
            if i >= 64 { return Err(Stop::Error); }
            let wi = w[i] as i32;
            if !mode.mpeg1 {
                val = (b.sget(12)? * qs * wi) >> 4;
            } else {
                let mut l = b.sget(8)?;
                if l & 0x7F == 0 { l = b.get(8)? as i32 + 2 * l; }
                let v = (l * qs * wi) >> 4;
                val = (v + !(v >> 31)) | 1;
            }
        } else {
            i += (sym >> 8) as usize;
            if i >= 64 { return Err(Stop::Error); }
            let level = sym & 0xFF;
            let mut v = (level * qs * w[i] as i32) >> 4;
            if mode.mpeg1 { v = (v - 1) | 1; }
            if b.get(1)? == 1 { v = -v; }
            val = v;
        }
        out[scan[i]] = saturate(val);
        i += 1;
    }
    Ok(())
}

fn non_intra_block(b: &mut Bits, mode: Mode, qs: i32, w: &[u8], out: &mut [i32; 64]) -> R<()> {
    let t = tables();
    let scan = if mode.alt_scan { &vlc::ALTERNATE } else { &vlc::ZIGZAG };
    let mut i = 0usize;
    loop {
        // the first coefficient: "1s" is run 0, level 1 (there's no EOB before it)
        let sym = if i == 0 && b.peek(1)? == 1 {
            b.pos += 1;
            1
        } else {
            b.vlc(&t.dct0)?.1
        };
        if sym == vlc::EOB { break; }
        let val;
        if sym == vlc::ESCAPE {
            let run = b.get(6)? as usize;
            i += run;
            if i >= 64 { return Err(Stop::Error); }
            let wi = w[i] as i32;
            if !mode.mpeg1 {
                let l = b.sget(12)?;
                val = ((2 * (l + (l >> 31)) + 1) * qs * wi) >> 5;
            } else {
                let mut l = b.sget(8)?;
                if l & 0x7F == 0 { l = b.get(8)? as i32 + 2 * l; }
                let v = ((2 * (l + (l >> 31)) + 1) * qs * wi) / 32;
                val = (v + !(v >> 31)) | 1;
            }
        } else {
            i += (sym >> 8) as usize;
            if i >= 64 { return Err(Stop::Error); }
            let level = sym & 0xFF;
            let mut v = ((2 * level + 1) * qs * w[i] as i32) >> 5;
            if mode.mpeg1 { v = (v - 1) | 1; }
            if b.get(1)? == 1 { v = -v; }
            val = v;
        }
        out[scan[i]] = saturate(val);
        i += 1;
    }
    Ok(())
}

/// The 8×8 inverse DCT (separable, in floats; rounded and clipped as IEEE 1180 asks)
fn idct(input: &[i32; 64], out: &mut [i32; 64]) {
    let c = &tables().idct;
    if input.iter().all(|&x| x == 0) { *out = [0; 64]; return; }
    let mut tmp = [0f32; 64];
    for y in 0..8 {
        for x in 0..8 {
            let mut s = 0f32;
            for u in 0..8 { s += c[u][x] * input[y * 8 + u] as f32; }
            tmp[y * 8 + x] = s;
        }
    }
    for x in 0..8 {
        for y in 0..8 {
            let mut s = 0f32;
            for v in 0..8 { s += c[v][y] * tmp[v * 8 + x]; }
            out[y * 8 + x] = (s.round() as i32).clamp(-256, 255);
        }
    }
}

/// Blocks to a macroblock: Y 16×16 then Cb 8×8, Cr 8×8 (field DCT: blocks 0 / 1 are the
/// even lines, 2 / 3 the odd ones). Intra values clip to 0-255.
fn arrange(blocks: &[[i32; 64]; 6], field: bool, intra: bool) -> [i32; 384] {
    let mut mb = [0i32; 384];
    for y in 0..16 {
        for x in 0..16 {
            let (blk, by) = if field {
                ((y & 1) * 2 + x / 8, y / 2)
            } else {
                ((y / 8) * 2 + x / 8, y % 8)
            };
            mb[y * 16 + x] = blocks[blk][by * 8 + x % 8];
        }
    }
    for k in 0..64 {
        mb[256 + k] = blocks[4][k];
        mb[320 + k] = blocks[5][k];
    }
    if intra { for v in mb.iter_mut() { *v = (*v).clamp(0, 255); } }
    mb
}

fn raw16_out(mb: &[i32; 384]) -> Vec<u128> {
    let bytes: Vec<u8> = mb.iter().flat_map(|&v| (v as i16).to_le_bytes()).collect();
    bytes.chunks(16).map(|c| u128::from_le_bytes(c.try_into().unwrap())).collect()
}

/// YCbCr (8-bit macroblock) to RGBA32 with the IPU's fixed-point coefficients (as PCSX2:
/// Y 1.1640625 above 16, R 1.59375 Cr, G -0.8125 Cr - 0.390625 Cb, B 2.015625 Cb); alpha
/// 80h, 40h under TH1, 0 (the whole pixel) under TH0; SGN flips the colour's top bits
fn csc(mb: &[u8; 384], th0: u32, th1: u32, sgn: bool) -> [u32; 256] {
    let mut out = [0u32; 256];
    for y in 0..16 {
        for x in 0..16 {
            let yy = mb[y * 16 + x] as i32;
            let cb = mb[256 + (y / 2) * 8 + x / 2] as i32 - 128;
            let cr = mb[320 + (y / 2) * 8 + x / 2] as i32 - 128;
            let lum = (0x95 * (yy - 16).max(0)) >> 6;
            let rcr = (0xCC * cr) >> 6;
            let gcr = (-0x68 * cr) >> 6;
            let gcb = (-0x32 * cb) >> 6;
            let bcb = (0x102 * cb) >> 6;
            let r = ((lum + rcr + 1) >> 1).clamp(0, 255) as u32;
            let g = ((lum + gcr + gcb + 1) >> 1).clamp(0, 255) as u32;
            let b = ((lum + bcb + 1) >> 1).clamp(0, 255) as u32;
            let mut p = r | g << 8 | b << 16 | 0x80 << 24;
            if th0 > 0 && r < th0 && g < th0 && b < th0 {
                p = 0;
            } else if th1 > 0 && r < th1 && g < th1 && b < th1 {
                p = (p & 0xFF_FFFF) | 0x40 << 24;
            }
            if sgn { p ^= 0x80_8080; }
            out[y * 16 + x] = p;
        }
    }
    out
}

/// RGBA32 to RGB555 + A (alpha bit when the alpha was 40h), dithered when DTE
fn to_rgb16(rgb: &[u32; 256], dte: bool) -> [u16; 256] {
    const DITHER: [[i32; 4]; 4] = [[-4, 0, -3, 1], [2, -2, 3, -1], [-3, 1, -4, 0], [3, -1, 2, -2]];
    let mut out = [0u16; 256];
    for i in 0..16 {
        for j in 0..16 {
            let p = rgb[i * 16 + j];
            let d = if dte { DITHER[i & 3][j & 3] } else { 0 };
            let c = |s: u32| (((p >> s & 0xFF) as i32 + d).clamp(0, 255) >> 3) as u16;
            let a = (p >> 24 == 0x40) as u16;
            out[i * 16 + j] = c(0) | c(8) << 5 | c(16) << 10 | a << 15;
        }
    }
    out
}

fn rgb_out(rgb: &[u32; 256], ofm: bool, dte: bool) -> Vec<u128> {
    let bytes: Vec<u8> = if ofm {
        to_rgb16(rgb, dte).iter().flat_map(|p| p.to_le_bytes()).collect()
    } else {
        rgb.iter().flat_map(|p| p.to_le_bytes()).collect()
    };
    bytes.chunks(16).map(|c| u128::from_le_bytes(c.try_into().unwrap())).collect()
}
