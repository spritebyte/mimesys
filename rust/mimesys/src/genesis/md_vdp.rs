//! Sega Genesis / Mega Drive VDP (YM7101), mode 5. After Charles MacDonald's "Sega Genesis
//! VDP documentation" and Genesis Plus GX (Eke-Eke) for the corner cases.
//!
//! Memory: 64 KB VRAM (tiles, name tables, sprite table, H scroll table), 64 colours of
//! CRAM (----BBB-GGG-RRR-), 40 words of VSRAM (vertical scroll).
//!
//! The 68000 talks to it through two ports. Control (C00004):
//!   100r rrrr dddd dddd           register r = d
//!   CCAA AAAA AAAA AAAA, then     address and access code, in two words
//!   0000 0000 CCCC 00AA           (code 0x20 bit = start a DMA)
//! Data (C00000) then reads or writes VRAM / CRAM / VSRAM, adding register 15 each time.
//!
//! Rendering is per line: planes A and B (32/64/128 cells, full / per cell / per line H
//! scroll, full / 2-cell V scroll), the window (replaces plane A in a corner), up to 80
//! sprites (20 a line, 320 pixels a line), priorities and shadow / highlight. H32 frames
//! (256 wide) are stretched to the 320-wide frame buffer.

use serde::{Deserialize, Serialize};

pub const SCREEN_W: usize = 320;
pub const SCREEN_H: usize = 224;
pub const LINES: u16 = 262;
/// first vblank line
pub const VBLANK_LINE: u16 = 224;

#[derive(Clone, Serialize, Deserialize)]
pub struct Vdp {
    #[serde(with = "serde_bytes")]
    pub vram: Vec<u8>,
    pub cram: Vec<u16>,
    pub vsram: Vec<u16>,
    pub reg: [u8; 24],
    pending: bool,
    code: u8,
    addr: u16,
    /// a VRAM fill waits for its data word
    fill_pending: bool,
    /// a 68000 -> VDP transfer for the bus to run: (source byte address, words)
    pub dma_68k: Option<(u32, u32)>,
    pub vint_pending: bool,
    pub hint_pending: bool,
    hint_counter: i32,
    vblank: bool,
    odd_frame: bool,
    sprite_overflow: bool,
    sprite_collision: bool,
    pub line: u16,
    /// position in the line, 0.0-1.0 (for the H counter and hblank flag)
    pub line_pos: f32,
    #[serde(skip, default = "blank_frame")]
    pub framebuffer: Vec<u8>,
}

fn blank_frame() -> Vec<u8> { vec![0; SCREEN_W * SCREEN_H * 4] }

/// 3-bit DAC levels for shadow (0-7), normal (0-14 even), highlight (7-14), as 0-255.
fn level(l: u16) -> u8 { ((l as u32 * 255 + 7) / 14) as u8 }

const TRANSPARENT: u8 = 0;
const PRI: u8 = 0x80;

impl Default for Vdp { fn default() -> Self { Self::new() } }

impl Vdp {
    pub fn new() -> Self {
        Self {
            vram: vec![0; 0x10000], cram: vec![0; 64], vsram: vec![0; 40], reg: [0; 24],
            pending: false, code: 0, addr: 0, fill_pending: false, dma_68k: None,
            vint_pending: false, hint_pending: false, hint_counter: 0,
            vblank: true, odd_frame: false, sprite_overflow: false, sprite_collision: false,
            line: 0, line_pos: 0.0, framebuffer: blank_frame(),
        }
    }

    pub fn h40(&self) -> bool { self.reg[12] & 0x81 != 0 }
    fn width(&self) -> usize { if self.h40() { 320 } else { 256 } }
    pub fn display_on(&self) -> bool { self.reg[1] & 0x40 != 0 }
    fn increment(&self) -> u16 { self.reg[15] as u16 }

    // ------------------------------------------------------------ ports

    pub fn control_write(&mut self, w: u16) {
        if !self.pending {
            if w & 0xC000 == 0x8000 {
                let r = ((w >> 8) & 0x1F) as usize;
                if r < 24 { self.reg[r] = w as u8; }
                return;
            }
            self.code = (self.code & 0x3C) | (w >> 14) as u8;
            self.addr = (self.addr & 0xC000) | (w & 0x3FFF);
            self.pending = true;
            return;
        }
        self.pending = false;
        self.code = (self.code & 0x03) | ((w >> 2) & 0x3C) as u8;
        self.addr = (self.addr & 0x3FFF) | ((w & 3) << 14);
        if self.code & 0x20 != 0 && self.reg[1] & 0x10 != 0 {
            match self.reg[23] >> 6 {
                0 | 1 => {
                    let src = ((self.reg[23] as u32 & 0x7F) << 17) | (self.reg[22] as u32) << 9 | (self.reg[21] as u32) << 1;
                    self.dma_68k = Some((src, self.dma_length()));
                }
                2 => self.fill_pending = true,
                _ => self.dma_copy(),
            }
        }
    }

    fn dma_length(&self) -> u32 {
        let n = self.reg[19] as u32 | (self.reg[20] as u32) << 8;
        if n == 0 { 0x10000 } else { n }
    }

    /// Reading the status port.
    pub fn status(&mut self) -> u16 {
        self.pending = false;
        let hblank = self.line_pos > 0.82 || self.line_pos < 0.02;
        let v = 0x3400 | 0x0200
            | (self.vint_pending as u16) << 7
            | (self.sprite_overflow as u16) << 6
            | (self.sprite_collision as u16) << 5
            | (self.odd_frame as u16) << 4
            | ((self.vblank || !self.display_on()) as u16) << 3
            | (hblank as u16) << 2;
        self.sprite_overflow = false;
        self.sprite_collision = false;
        v
    }

    pub fn data_write(&mut self, data: u16) {
        self.pending = false;
        self.write_target(data);
        if self.fill_pending {
            self.fill_pending = false;
            let fill = (data >> 8) as u8;
            for _ in 0..self.dma_length() {
                self.vram[(self.addr ^ 1) as usize] = fill;
                self.addr = self.addr.wrapping_add(self.increment());
            }
            self.dma_done(0);
        }
    }

    fn write_target(&mut self, data: u16) {
        let a = self.addr;
        match self.code & 0x0F {
            1 => {
                let d = if a & 1 != 0 { data.swap_bytes() } else { data };
                let i = (a & 0xFFFE) as usize;
                self.vram[i] = (d >> 8) as u8;
                self.vram[i + 1] = d as u8;
            }
            3 => self.cram[((a >> 1) & 0x3F) as usize] = data & 0x0EEE,
            5 => {
                let i = ((a >> 1) & 0x3F) as usize;
                if i < 40 { self.vsram[i] = data & 0x07FF; }
            }
            _ => {}
        }
        self.addr = a.wrapping_add(self.increment());
    }

    pub fn data_read(&mut self) -> u16 {
        self.pending = false;
        let a = self.addr;
        let v = match self.code & 0x0F {
            0 => {
                let i = (a & 0xFFFE) as usize;
                (self.vram[i] as u16) << 8 | self.vram[i + 1] as u16
            }
            4 => self.vsram.get(((a >> 1) & 0x3F) as usize).copied().unwrap_or(0),
            8 => self.cram[((a >> 1) & 0x3F) as usize],
            _ => 0,
        };
        self.addr = a.wrapping_add(self.increment());
        v
    }

    /// One word of a 68000 -> VDP transfer.
    pub fn dma_word(&mut self, data: u16) { self.write_target(data); }

    /// Bookkeeping after a transfer: length counts down to 0, the source moves on.
    pub fn dma_done(&mut self, words: u32) {
        self.reg[19] = 0;
        self.reg[20] = 0;
        let src = (self.reg[22] as u32) << 8 | self.reg[21] as u32;
        let src = src.wrapping_add(words) & 0xFFFF;
        self.reg[21] = src as u8;
        self.reg[22] = (src >> 8) as u8;
    }

    fn dma_copy(&mut self) {
        let mut src = (self.reg[22] as u16) << 8 | self.reg[21] as u16;
        let n = self.dma_length();
        for _ in 0..n {
            self.vram[(self.addr ^ 1) as usize] = self.vram[(src ^ 1) as usize];
            src = src.wrapping_add(1);
            self.addr = self.addr.wrapping_add(self.increment());
        }
        self.reg[19] = 0;
        self.reg[20] = 0;
        self.reg[21] = src as u8;
        self.reg[22] = (src >> 8) as u8;
    }

    /// 68000 clocks a transfer of `words` holds the bus (slots per line from the VDP docs).
    pub fn dma_cost(&self, words: u32) -> u32 {
        let target_vram = self.code & 0x0F == 1;
        let slots = words * if target_vram { 2 } else { 1 };
        let per_line = match (self.vblank || !self.display_on(), self.h40()) {
            (true, true) => 205, (true, false) => 167, (false, true) => 18, (false, false) => 16,
        };
        slots * 488 / per_line
    }

    /// H and V counters (C00008).
    pub fn hv_counter(&self) -> u16 {
        let v = if self.line <= 0xEA { self.line } else { self.line - 6 } & 0xFF;
        let (n, last, jump) = if self.h40() { (210.0, 0xB6, 0xE4) } else { (171.0, 0x93, 0xE9) };
        let idx = (self.line_pos * n) as u16;
        let h = if idx <= last { idx } else { (idx - last - 1 + jump).min(0xFF) };
        v << 8 | h
    }

    // ------------------------------------------------------------ interrupts and lines

    /// Interrupt level wanted from the 68000.
    pub fn ipl(&self) -> u8 {
        if self.vint_pending && self.reg[1] & 0x20 != 0 { 6 }
        else if self.hint_pending && self.reg[0] & 0x10 != 0 { 4 }
        else { 0 }
    }

    pub fn acknowledge(&mut self, level: u8) {
        match level {
            6 => self.vint_pending = false,
            4 => self.hint_pending = false,
            _ => {}
        }
    }

    /// Start of a line: the line counter (H interrupt) and vblank.
    pub fn begin_line(&mut self, line: u16) {
        self.line = line;
        if line <= VBLANK_LINE {
            if self.hint_counter == 0 {
                self.hint_counter = self.reg[10] as i32;
                self.hint_pending = true;
            } else {
                self.hint_counter -= 1;
            }
        } else {
            self.hint_counter = self.reg[10] as i32;
        }
        if line == 0 {
            self.vblank = false;
            self.odd_frame = !self.odd_frame;
        }
        if line == VBLANK_LINE {
            self.vblank = true;
            self.vint_pending = true;
        }
    }

    // ------------------------------------------------------------ rendering

    fn vram_word(&self, a: usize) -> u16 {
        let a = a & 0xFFFE;
        (self.vram[a] as u16) << 8 | self.vram[a + 1] as u16
    }

    /// Pixel of tile `entry` (name table word) at (x, y) inside the tile: colour index with
    /// PRI set from the entry, 0 when transparent.
    fn tile_pixel(&self, entry: u16, x: usize, y: usize) -> u8 {
        let tile = (entry & 0x7FF) as usize;
        let tx = if entry & 0x0800 != 0 { 7 - x } else { x };
        let ty = if entry & 0x1000 != 0 { 7 - y } else { y };
        let b = self.vram[(tile * 32 + ty * 4 + tx / 2) & 0xFFFF];
        let p = if tx & 1 == 0 { b >> 4 } else { b & 15 };
        if p == 0 { return TRANSPARENT | if entry & 0x8000 != 0 { PRI } else { 0 }; }
        p | ((entry >> 9) & 0x30) as u8 | if entry & 0x8000 != 0 { PRI } else { 0 }
    }

    fn plane_size(&self) -> (usize, usize) {
        let cells = |v: u8| match v & 3 { 1 => 64, 3 => 128, _ => 32 };
        let mut w = cells(self.reg[16]);
        let mut h = cells(self.reg[16] >> 4);
        // the plane can't be bigger than 8 KB: 64x64, 128x32, 32x128
        if w == 128 { h = 32; } else if w == 64 && h == 128 { h = 64; }
        if h == 128 { w = 32; }
        (w, h)
    }

    /// One line of plane A (0) or B (1) into `out`.
    fn plane_line(&self, layer: usize, line: usize, out: &mut [u8]) {
        let (pw, ph) = self.plane_size();
        let base = if layer == 0 { (self.reg[2] as usize & 0x38) << 10 } else { (self.reg[4] as usize & 7) << 13 };
        let hs_base = (self.reg[13] as usize & 0x3F) << 10;
        let hs_off = match self.reg[11] & 3 { 0 => 0, 1 => (line & 7) * 4, 2 => (line & !7) * 4, _ => line * 4 };
        let hscroll = (self.vram_word(hs_base + hs_off + layer * 2) & 0x3FF) as usize;
        let col_mode = self.reg[11] & 4 != 0;
        let (wmask, hmask) = (pw * 8 - 1, ph * 8 - 1);
        for (x, o) in out.iter_mut().enumerate() {
            let vs = if col_mode {
                self.vsram.get((x / 16) * 2 + layer).copied().unwrap_or(0)
            } else {
                self.vsram[layer]
            } as usize;
            let px = (x.wrapping_sub(hscroll)) & wmask;
            let py = (line + vs) & hmask;
            let entry = self.vram_word(base + ((py / 8) * pw + px / 8) * 2);
            *o = self.tile_pixel(entry, px & 7, py & 7);
        }
    }

    /// Window over plane A, where the window registers say.
    fn window_line(&self, line: usize, a: &mut [u8]) {
        let (wh, right) = ((self.reg[17] & 0x1F) as usize * 16, self.reg[17] & 0x80 != 0);
        let (wv, down) = ((self.reg[18] & 0x1F) as usize * 8, self.reg[18] & 0x80 != 0);
        let in_v = if down { line >= wv } else { line < wv };
        let h40 = self.h40();
        let base = (self.reg[3] as usize & if h40 { 0x3C } else { 0x3E }) << 10;
        let cols = if h40 { 64 } else { 32 };
        for (x, o) in a.iter_mut().enumerate() {
            let in_h = if right { x >= wh } else { x < wh };
            if in_v || in_h {
                let entry = self.vram_word(base + ((line / 8) * cols + x / 8) * 2);
                *o = self.tile_pixel(entry, x & 7, line & 7);
            }
        }
    }

    fn sprite_line(&mut self, line: usize, out: &mut [u8]) {
        let h40 = self.h40();
        let width = out.len() as i32;
        let sat = (self.reg[5] as usize & if h40 { 0x7E } else { 0x7F }) << 9;
        let (max_total, max_line) = if h40 { (80, 20) } else { (64, 16) };
        let mut dots_left = width;
        let mut on_line = 0;
        let mut seen_nonzero_x = false;
        let mut n = 0usize;
        let mut link = 0usize;
        loop {
            let e = sat + link * 8;
            let y = (self.vram_word(e) & 0x1FF) as i32 - 128;
            let size = self.vram[(e + 2) & 0xFFFF];
            let next = (self.vram[(e + 3) & 0xFFFF] & 0x7F) as usize;
            let (cw, ch) = (((size >> 2) & 3) as i32 + 1, (size & 3) as i32 + 1);
            let ly = line as i32 - y;
            if ly >= 0 && ly < ch * 8 {
                on_line += 1;
                if on_line > max_line { self.sprite_overflow = true; break; }
                let attr = self.vram_word(e + 4);
                let xr = (self.vram_word(e + 6) & 0x1FF) as i32;
                if xr == 0 {
                    if seen_nonzero_x { break; }   // sprite masking
                } else {
                    seen_nonzero_x = true;
                }
                let sx = xr - 128;
                let row = if attr & 0x1000 != 0 { ch * 8 - 1 - ly } else { ly };
                let hflip = attr & 0x0800 != 0;
                let tile_base = (attr & 0x7FF) as i32;
                for cx in 0..cw * 8 {
                    if dots_left == 0 { self.sprite_overflow = true; break; }
                    dots_left -= 1;
                    let x = sx + cx;
                    if x < 0 || x >= width { continue; }
                    let col = if hflip { cw * 8 - 1 - cx } else { cx };
                    let tile = tile_base + (col / 8) * ch + row / 8;
                    // build an entry with no flips: the flips are done above
                    let entry = (attr & 0xE000) | (tile as u16 & 0x7FF);
                    let p = self.tile_pixel(entry, (col & 7) as usize, (row & 7) as usize);
                    if p & 0x3F & 0x0F == 0 { continue; }
                    let o = &mut out[x as usize];
                    if *o & 0x0F != 0 { self.sprite_collision = true; } else { *o = p; }
                }
                if dots_left == 0 { break; }
            }
            n += 1;
            if next == 0 || next >= max_total || n >= max_total { break; }
            link = next;
        }
    }

    fn rgb(&self, idx: u8, mode: u8) -> [u8; 4] {
        let c = self.cram[(idx & 0x3F) as usize];
        let ch = |v: u16| {
            let v = (v >> 1) & 7;
            level(match mode { 0 => v, 1 => v * 2, _ => v + 7 })
        };
        [ch(c), ch(c >> 4), ch(c >> 8), 255]
    }

    /// Draw display line `line` (0-223) into the frame buffer.
    pub fn render_line(&mut self, line: usize) {
        if line >= SCREEN_H { return; }
        let w = self.width();
        let backdrop = self.reg[7] & 0x3F;
        let mut row = [[0u8; 4]; 320];
        if !self.display_on() {
            let c = self.rgb(backdrop, 1);
            row[..w].fill(c);
        } else {
            let mut a = [0u8; 320];
            let mut b = [0u8; 320];
            let mut s = [0u8; 320];
            self.plane_line(0, line, &mut a[..w]);
            self.plane_line(1, line, &mut b[..w]);
            self.window_line(line, &mut a[..w]);
            self.sprite_line(line, &mut s[..w]);
            let sh = self.reg[12] & 0x08 != 0;
            let blank_left = self.reg[0] & 0x20 != 0;
            for x in 0..w {
                if blank_left && x < 8 { row[x] = self.rgb(backdrop, 1); continue; }
                let (pa, pb, ps) = (a[x], b[x], s[x]);
                let op = |p: u8| p & 0x0F != 0;
                let hi = |p: u8| p & PRI != 0;
                // the pixel under the sprites
                let plane = if op(pa) && hi(pa) { pa }
                    else if op(pb) && hi(pb) { pb }
                    else if op(pa) { pa }
                    else if op(pb) { pb }
                    else { backdrop };
                let plane_hi = (op(pa) && hi(pa)) || (op(pb) && hi(pb));
                let sprite_wins = op(ps) && (hi(ps) || !plane_hi);
                if !sh {
                    row[x] = self.rgb(if sprite_wins { ps } else { plane }, 1);
                    continue;
                }
                // shadow / highlight: planes both low priority = shadowed
                let shadow = !hi(pa) && !hi(pb);
                let base_mode = if shadow { 0 } else { 1 };
                row[x] = if sprite_wins {
                    match ps & 0x3F {
                        0x3E => self.rgb(plane, base_mode + 1),     // highlight operator
                        0x3F => self.rgb(plane, 0),                 // shadow operator
                        c if c & 0x0F == 0x0E => self.rgb(c, 1),
                        c => self.rgb(c, if hi(ps) { 1 } else { base_mode }),
                    }
                } else {
                    self.rgb(plane, base_mode)
                };
            }
        }
        let fb = &mut self.framebuffer[line * SCREEN_W * 4..(line + 1) * SCREEN_W * 4];
        for ox in 0..SCREEN_W {
            let sx = if w == 320 { ox } else { ox * 4 / 5 };
            fb[ox * 4..ox * 4 + 4].copy_from_slice(&row[sx]);
        }
    }

    /// Restore from a save state, keeping the frame buffer.
    pub fn load_state(&mut self, s: Vdp) {
        let fb = std::mem::take(&mut self.framebuffer);
        *self = s;
        self.framebuffer = fb;
    }
}
