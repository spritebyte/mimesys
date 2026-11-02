//! SNES PPU: VRAM / OAM / CGRAM ports and a line renderer: background modes 0-7
//! (2 / 4 / 8 bits a pixel, 8x8 or 16x16 tiles, offset-per-tile in modes 2 / 4 / 6,
//! Mode 7 rotation / scaling with EXTBG, direct colour), sprites with the 32-sprite /
//! 34-sliver line limits and priority rotation, two windows, main + sub screens with
//! colour math (add / subtract / half, fixed colour, clip and prevent regions), mosaic,
//! hi-res (modes 5 / 6 and pseudo hi-res), brightness and forced blank. One line is drawn
//! at a time (at the start of its horizontal blank), so values HDMA writes between lines
//! take effect, mid-line writes don't.
//!
//! Not done: interlace is drawn one field only (448-line hi-res pictures show every
//! other line), OBJ interlace, and the 239-line overscan.
//!
//! Register behaviour follows ares (ISC): the VRAM address remapping and prefetch, the
//! write-twice scroll registers, CGRAM's two-write latch, the OAM low-table latch, and
//! VRAM / OAM writes ignored outside blanking.

use serde::{Deserialize, Serialize};

/// Output width: every pixel twice, or hi-res columns.
pub const WIDTH: usize = 512;
/// Pixels in a (low-res) line.
const SCREEN_W: usize = 256;
pub const HEIGHT: usize = 224;

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
pub struct Bg {
    pub map_base: u16,      // word address
    pub map_size: u8,       // 0 32x32, 1 64x32, 2 32x64, 3 64x64
    pub char_base: u16,     // word address
    pub hofs: u16,
    pub vofs: u16,
    pub big: bool,          // 16x16 tiles
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Ppu {
    #[serde(with = "serde_bytes")]
    pub vram: Vec<u8>,      // 64 KB (word n = bytes 2n, 2n+1)
    #[serde(with = "serde_bytes")]
    pub oam: Vec<u8>,       // 544 bytes
    pub cgram: Vec<u16>,    // 256 colours, 15-bit BGR
    pub inidisp: u8,
    pub obsel: u8,
    pub oam_base: u16,
    pub oam_addr: u16,
    pub oam_priority: bool,
    oam_latch: u8,
    pub bgmode: u8,
    pub mosaic: u8,
    pub bg: [Bg; 4],
    ofs_latch1: u8,
    ofs_latch2: u8,
    m7_latch: u8,
    pub vmain: u8,
    pub vaddr: u16,
    vlatch: u16,
    pub cgaddr: u8,
    cg_hi: bool,
    cg_latch: u8,
    pub m7: [i16; 6],       // A B C D X Y
    pub m7sel: u8,
    pub m7_hofs: u16,
    pub m7_vofs: u16,
    pub win: [u8; 12],      // 2123-212B (window settings, stage 2)
    pub tm: u8,
    pub ts: u8,
    pub tmw: u8,
    pub tsw: u8,
    pub cgwsel: u8,
    pub cgadsub: u8,
    pub coldata: u16,       // fixed colour, 15-bit BGR
    pub setini: u8,
    // latched counters (2137 / 213C-213F)
    pub hcount: u16,
    pub vcount: u16,
    hlatch_hi: bool,
    vlatch_hi: bool,
    pub counters_latched: bool,
    pub mdr1: u8,
    pub mdr2: u8,
    /// the multiply result M7A * (M7B >> 8), read at 2134-2136
    mpy: i32,
    /// sprite overflow flags (213E bits 6 and 7)
    #[serde(default)]
    range_over: bool,
    #[serde(default)]
    time_over: bool,
}

impl Ppu {
    pub fn new() -> Self {
        Self {
            vram: vec![0; 0x10000], oam: vec![0; 544], cgram: vec![0; 256],
            inidisp: 0x80, obsel: 0, oam_base: 0, oam_addr: 0, oam_priority: false, oam_latch: 0,
            bgmode: 0, mosaic: 0, bg: [Bg::default(); 4], ofs_latch1: 0, ofs_latch2: 0, m7_latch: 0,
            vmain: 0, vaddr: 0, vlatch: 0, cgaddr: 0, cg_hi: false, cg_latch: 0,
            m7: [0; 6], m7sel: 0, m7_hofs: 0, m7_vofs: 0, win: [0; 12],
            tm: 0, ts: 0, tmw: 0, tsw: 0, cgwsel: 0, cgadsub: 0, coldata: 0, setini: 0,
            hcount: 0, vcount: 0, hlatch_hi: false, vlatch_hi: false, counters_latched: false,
            mdr1: 0, mdr2: 0, mpy: 0, range_over: false, time_over: false,
        }
    }

    pub fn forced_blank(&self) -> bool { self.inidisp & 0x80 != 0 }

    fn vram_word_addr(&self) -> usize {
        let a = self.vaddr;
        let m = match (self.vmain >> 2) & 3 {
            0 => a,
            1 => (a & 0xFF00) | (a & 0x1F) << 3 | (a >> 5) & 7,
            2 => (a & 0xFE00) | (a & 0x3F) << 3 | (a >> 6) & 7,
            _ => (a & 0xFC00) | (a & 0x7F) << 3 | (a >> 7) & 7,
        };
        (m & 0x7FFF) as usize
    }

    fn vram_word(&self, w: usize) -> u16 { u16::from_le_bytes([self.vram[w * 2], self.vram[w * 2 + 1]]) }

    fn inc_size(&self) -> u16 { [1, 32, 128, 128][(self.vmain & 3) as usize] }

    fn oam_reset(&mut self) { self.oam_addr = self.oam_base; }

    /// A write to 21xx. `in_display`: the PPU is drawing (VRAM / OAM writes are lost).
    pub fn write(&mut self, reg: u8, v: u8, in_display: bool) {
        let blocked = in_display && !self.forced_blank();
        match reg {
            0x00 => self.inidisp = v,
            0x01 => self.obsel = v,
            0x02 => { self.oam_base = (self.oam_base & 0x200) | (v as u16) << 1; self.oam_reset(); }
            0x03 => { self.oam_base = (self.oam_base & 0x1FE) | ((v & 1) as u16) << 9; self.oam_priority = v & 0x80 != 0; self.oam_reset(); }
            0x04 => {
                let a = self.oam_addr & 0x3FF;
                let lo = a & 1;
                self.oam_addr = (self.oam_addr + 1) & 0x3FF;
                if lo == 0 { self.oam_latch = v; }
                if blocked { return; }
                if a & 0x200 != 0 {
                    self.oam[0x200 + (a as usize & 0x1F)] = v;
                } else if lo == 1 {
                    self.oam[(a & !1) as usize] = self.oam_latch;
                    self.oam[(a | 1) as usize] = v;
                }
            }
            0x05 => {
                self.bgmode = v;
                for i in 0..4 { self.bg[i].big = v & (0x10 << i) != 0; }
            }
            0x06 => self.mosaic = v,
            0x07..=0x0A => {
                let b = &mut self.bg[(reg - 7) as usize];
                b.map_base = ((v & 0xFC) as u16) << 8;
                b.map_size = v & 3;
            }
            0x0B => { self.bg[0].char_base = ((v & 0x0F) as u16) << 12; self.bg[1].char_base = ((v >> 4) as u16) << 12; }
            0x0C => { self.bg[2].char_base = ((v & 0x0F) as u16) << 12; self.bg[3].char_base = ((v >> 4) as u16) << 12; }
            0x0D..=0x14 => {
                let n = ((reg - 0x0D) / 2) as usize;
                if reg == 0x0D { self.m7_hofs = (v as u16) << 8 | self.m7_latch as u16; self.m7_latch = v; }
                if reg == 0x0E { self.m7_vofs = (v as u16) << 8 | self.m7_latch as u16; self.m7_latch = v; }
                if reg & 1 == 1 {
                    self.bg[n].hofs = (v as u16) << 8 | (self.ofs_latch1 & !7) as u16 | (self.ofs_latch2 & 7) as u16;
                    self.ofs_latch1 = v;
                    self.ofs_latch2 = v;
                } else {
                    self.bg[n].vofs = (v as u16) << 8 | self.ofs_latch1 as u16;
                    self.ofs_latch1 = v;
                }
            }
            0x15 => self.vmain = v,
            0x16 => { self.vaddr = (self.vaddr & 0xFF00) | v as u16; self.vlatch = self.vram_word(self.vram_word_addr()); }
            0x17 => { self.vaddr = (self.vaddr & 0x00FF) | (v as u16) << 8; self.vlatch = self.vram_word(self.vram_word_addr()); }
            0x18 | 0x19 => {
                let hi = reg == 0x19;
                if !blocked {
                    let w = self.vram_word_addr();
                    self.vram[w * 2 + hi as usize] = v;
                }
                if (self.vmain & 0x80 != 0) == hi { self.vaddr = self.vaddr.wrapping_add(self.inc_size()); }
            }
            0x1A => self.m7sel = v,
            0x1B..=0x20 => {
                let i = (reg - 0x1B) as usize;
                self.m7[i] = ((v as u16) << 8 | self.m7_latch as u16) as i16;
                self.m7_latch = v;
                if reg <= 0x1C { self.mpy = self.m7[0] as i32 * ((self.m7[1] >> 8) as i8) as i32; }
            }
            0x21 => { self.cgaddr = v; self.cg_hi = false; }
            0x22 => {
                if !self.cg_hi { self.cg_latch = v; } else {
                    self.cgram[self.cgaddr as usize] = ((v & 0x7F) as u16) << 8 | self.cg_latch as u16;
                    self.cgaddr = self.cgaddr.wrapping_add(1);
                }
                self.cg_hi = !self.cg_hi;
            }
            0x23..=0x2B => self.win[(reg - 0x23) as usize] = v,
            0x2C => self.tm = v,
            0x2D => self.ts = v,
            0x2E => self.tmw = v,
            0x2F => self.tsw = v,
            0x30 => self.cgwsel = v,
            0x31 => self.cgadsub = v,
            0x32 => {
                let c = (v & 0x1F) as u16;
                if v & 0x20 != 0 { self.coldata = (self.coldata & !0x001F) | c; }
                if v & 0x40 != 0 { self.coldata = (self.coldata & !0x03E0) | c << 5; }
                if v & 0x80 != 0 { self.coldata = (self.coldata & !0x7C00) | c << 10; }
            }
            0x33 => self.setini = v,
            _ => {}
        }
    }

    /// A read of 2134-213F (others are open bus: `None`).
    pub fn read(&mut self, reg: u8, in_display: bool) -> Option<u8> {
        let blocked = in_display && !self.forced_blank();
        Some(match reg {
            0x34 => { self.mdr1 = self.mpy as u8; self.mdr1 }
            0x35 => { self.mdr1 = (self.mpy >> 8) as u8; self.mdr1 }
            0x36 => { self.mdr1 = (self.mpy >> 16) as u8; self.mdr1 }
            0x38 => {
                let a = self.oam_addr & 0x3FF;
                self.oam_addr = (self.oam_addr + 1) & 0x3FF;
                self.mdr1 = if a & 0x200 != 0 { self.oam[0x200 + (a as usize & 0x1F)] } else { self.oam[a as usize] };
                self.mdr1
            }
            0x39 | 0x3A => {
                let hi = reg == 0x3A;
                self.mdr1 = if hi { (self.vlatch >> 8) as u8 } else { self.vlatch as u8 };
                if (self.vmain & 0x80 != 0) == hi {
                    self.vlatch = if blocked { 0 } else { self.vram_word(self.vram_word_addr()) };
                    self.vaddr = self.vaddr.wrapping_add(self.inc_size());
                }
                self.mdr1
            }
            0x3B => {
                let c = self.cgram[self.cgaddr as usize];
                if !self.cg_hi { self.mdr2 = c as u8; } else {
                    self.mdr2 = (self.mdr2 & 0x80) | ((c >> 8) as u8 & 0x7F);
                    self.cgaddr = self.cgaddr.wrapping_add(1);
                }
                self.cg_hi = !self.cg_hi;
                self.mdr2
            }
            0x3C => {
                self.mdr2 = if !self.hlatch_hi { self.hcount as u8 } else { (self.mdr2 & 0xFE) | (self.hcount >> 8) as u8 & 1 };
                self.hlatch_hi = !self.hlatch_hi;
                self.mdr2
            }
            0x3D => {
                self.mdr2 = if !self.vlatch_hi { self.vcount as u8 } else { (self.mdr2 & 0xFE) | (self.vcount >> 8) as u8 & 1 };
                self.vlatch_hi = !self.vlatch_hi;
                self.mdr2
            }
            0x3E => {
                self.mdr1 = (self.mdr1 & 0x10) | 0x01 | (self.time_over as u8) << 7 | (self.range_over as u8) << 6;
                self.mdr1
            }
            0x3F => {
                self.hlatch_hi = false;
                self.vlatch_hi = false;
                let latched = self.counters_latched;
                self.counters_latched = false;
                self.mdr2 = (self.mdr2 & 0x20) | 0x03 | (latched as u8) << 6;
                self.mdr2
            }
            _ => return None,
        })
    }

    /// Latch the H / V counters (2137, or WRIO bit 7 going low).
    pub fn latch_counters(&mut self, h_dot: u16, v: u16) {
        self.hcount = h_dot;
        self.vcount = v;
        self.counters_latched = true;
    }

    /// Start of vblank: the OAM address reloads (when not in forced blank).
    pub fn vblank(&mut self) { if !self.forced_blank() { self.oam_reset(); } }


    /// End of vblank: the sprite overflow flags clear.
    pub fn frame_start(&mut self) { self.range_over = false; self.time_over = false; }

    // ---------------- rendering ----------------
    //
    // A line is built as layers of `Px` (a 15-bit colour, a priority, 0 = nothing),
    // stacked by the mode's priority order into a main and a sub screen, then colour
    // math, brightness and output. Output is 512 wide: low-res pixels are doubled,
    // hi-res modes (5, 6, pseudo hi-res) put the sub screen on even columns.

    /// Colour depth of BG n in the current mode (0 = layer absent).
    fn bpp(&self, n: usize) -> u32 {
        const T: [[u32; 4]; 8] = [[2, 2, 2, 2], [4, 4, 2, 0], [4, 4, 0, 0], [8, 4, 0, 0], [8, 2, 0, 0], [4, 2, 0, 0], [4, 0, 0, 0], [0, 0, 0, 0]];
        T[(self.bgmode & 7) as usize][n]
    }

    fn hires(&self) -> bool { matches!(self.bgmode & 7, 5 | 6) }

    fn mosaic_size(&self, n: usize) -> u32 {
        if self.mosaic & (1 << n) != 0 { (self.mosaic >> 4) as u32 + 1 } else { 1 }
    }

    fn tile_size(&self, n: usize) -> (u32, u32) {
        let big = self.bg[n].big;
        (if big || self.hires() { 16 } else { 8 }, if big { 16 } else { 8 })
    }

    /// Tile map entry of BG n under layer pixel (x, y).
    fn map_entry(&self, n: usize, x: u32, y: u32) -> u16 {
        let b = &self.bg[n];
        let (tw, th) = self.tile_size(n);
        let map_w = if b.map_size & 1 != 0 { 64 } else { 32 };
        let map_h = if b.map_size & 2 != 0 { 64 } else { 32 };
        let tx = (x / tw) & (map_w - 1);
        let ty = (y / th) & (map_h - 1);
        let mut scr = (ty & 31) * 32 + (tx & 31);
        if tx >= 32 { scr += 0x400; }
        if ty >= 32 { scr += if map_w == 64 { 0x800 } else { 0x400 }; }
        self.vram_word(((b.map_base as u32 + scr) & 0x7FFF) as usize)
    }

    /// 15-bit colour of an 8-bit-per-pixel BG pixel in direct colour mode.
    fn direct(c: u32, pal: u32) -> u16 {
        let r = (c & 7) << 2 | (pal & 1) << 1;
        let g = (c >> 3 & 7) << 2 | (pal & 2);
        let b = (c >> 6 & 3) << 3 | (pal & 4);
        (r | g << 5 | b << 10) as u16
    }

    /// The 8 pixels of BG n starting at layer x (a multiple of 8) on layer line y.
    fn bg_row8(&self, n: usize, x: u32, y: u32, bpp: u32) -> [Px; 8] {
        let mut out = [Px::default(); 8];
        let entry = self.map_entry(n, x, y);
        let (tw, th) = self.tile_size(n);
        let hflip = entry & 0x4000 != 0;
        let mut py = y % th;
        if entry & 0x8000 != 0 { py = th - 1 - py; }
        // which 8-pixel half of a 16-wide tile, after flipping
        let mut half = (x % tw) / 8;
        if hflip && tw == 16 { half ^= 1; }
        let tile = ((entry & 0x3FF) as u32 + half + if py >= 8 { 16 } else { 0 }) & 0x3FF;
        let base = self.bg[n].char_base as u32 + tile * bpp * 4;
        let mut planes = [0u32; 4];
        for pair in 0..(bpp / 2) as usize {
            planes[pair] = self.vram_word(((base + pair as u32 * 8 + (py & 7)) & 0x7FFF) as usize) as u32;
        }
        let pal = (entry >> 10 & 7) as u32;
        let prio = 1 + (entry >> 13 & 1) as u8;
        let pal_base = if self.bgmode & 7 == 0 { n as u32 * 32 } else { 0 };
        for i in 0..8u32 {
            let bit = if hflip { i } else { 7 - i };
            let mut c = 0u32;
            for pair in 0..(bpp / 2) as usize {
                let w = planes[pair];
                c |= (w >> bit & 1) << (pair * 2) | (w >> (8 + bit) & 1) << (pair * 2 + 1);
            }
            if c == 0 { continue; }
            let color = match bpp {
                2 => self.cgram[(pal_base + pal * 4 + c) as usize],
                4 => self.cgram[(pal * 16 + c) as usize],
                _ if self.cgwsel & 1 != 0 => Self::direct(c, pal),
                _ => self.cgram[c as usize],
            };
            out[i as usize] = Px { color, prio, math: true };
        }
        out
    }

    /// One BG line. `phase` picks the even (0) or odd (1) hi-res column in modes 5 / 6.
    fn bg_line(&self, n: usize, y: u32, phase: u32) -> [Px; SCREEN_W] {
        let mut out = [Px::default(); SCREEN_W];
        let bpp = self.bpp(n);
        if bpp == 0 { return out; }
        let mode = self.bgmode & 7;
        let hires = self.hires();
        let b = self.bg[n];
        let m = self.mosaic_size(n);
        let y = y - (y - 1) % m;
        let opt = n < 2 && matches!(mode, 2 | 4 | 6);
        let (bg3h, bg3v) = (self.bg[2].hofs as u32, self.bg[2].vofs as u32);
        let mut cache_key = (u32::MAX, u32::MAX);
        let mut row = [Px::default(); 8];
        for x in 0..SCREEN_W as u32 {
            let xm = x - x % m;
            let mut h = xm + b.hofs as u32;
            let mut v = y + b.vofs as u32;
            if opt {
                // offset-per-tile: BG3's map gives each 8-pixel column its own scroll
                let ox = xm + (b.hofs as u32 & 7);
                if ox >= 8 {
                    let col = (ox - 8) + (bg3h & !7);
                    let hval = self.map_entry(2, col, bg3v) as u32;
                    let vval = self.map_entry(2, col, bg3v + 8) as u32;
                    let valid = if n == 0 { 0x2000 } else { 0x4000 };
                    if mode == 4 {
                        if hval & valid != 0 {
                            if hval & 0x8000 == 0 { h = ox + (hval & 0x3F8); } else { v = y + (hval & 0x3FF); }
                        }
                    } else {
                        if hval & valid != 0 { h = ox + (hval & 0x3F8); }
                        if vval & valid != 0 { v = y + (vval & 0x3FF); }
                    }
                }
            }
            if hires { h = h * 2 + phase; }
            let key = (h >> 3, v);
            if key != cache_key { row = self.bg_row8(n, h & !7, v, bpp); cache_key = key; }
            out[x as usize] = row[(h & 7) as usize];
        }
        out
    }

    /// Mode 7: BG1 and, with EXTBG, BG2 (bit 7 of the pixel is its priority).
    fn m7_line(&self, y: u32) -> ([Px; SCREEN_W], [Px; SCREEN_W]) {
        let mut bg1 = [Px::default(); SCREEN_W];
        let mut bg2 = [Px::default(); SCREEN_W];
        let s13 = |v: u16| ((v << 3) as i16 >> 3) as i32;
        let clip = |n: i32| if n & 0x2000 != 0 { n | !0x3FF } else { n & 0x3FF };
        let [a, b, c, d] = [self.m7[0] as i32, self.m7[1] as i32, self.m7[2] as i32, self.m7[3] as i32];
        let (cx, cy) = (s13(self.m7[4] as u16), s13(self.m7[5] as u16));
        let (hofs, vofs) = (s13(self.m7_hofs), s13(self.m7_vofs));
        let m = self.mosaic_size(0);
        let y = (y - (y - 1) % m) & 0xFF;
        let yy = if self.m7sel & 2 != 0 { 255 - y as i32 } else { y as i32 };
        let ox = (a * clip(hofs - cx) & !63) + (b * clip(vofs - cy) & !63) + (b * yy & !63) + (cx << 8);
        let oy = (c * clip(hofs - cx) & !63) + (d * clip(vofs - cy) & !63) + (d * yy & !63) + (cy << 8);
        let extbg = self.setini & 0x40 != 0;
        let direct = self.cgwsel & 1 != 0;
        for x in 0..SCREEN_W as i32 {
            let xm = x - x % m as i32;
            let xx = if self.m7sel & 1 != 0 { 255 - xm } else { xm };
            let px = (ox + a * xx) >> 8;
            let py = (oy + c * xx) >> 8;
            let outside = !(0..1024).contains(&px) || !(0..1024).contains(&py);
            let tile = match self.m7sel >> 6 {
                2 if outside => continue,
                3 if outside => 0,
                _ => self.vram[((((py as u32 & 1023) >> 3) * 128 + ((px as u32 & 1023) >> 3)) * 2) as usize] as u32,
            };
            let p = self.vram[((tile * 64 + (py as u32 & 7) * 8 + (px as u32 & 7)) * 2 + 1) as usize] as u32;
            if p != 0 {
                let color = if direct { Self::direct(p, 0) } else { self.cgram[p as usize] };
                bg1[x as usize] = Px { color, prio: 1, math: true };
            }
            if extbg && p & 0x7F != 0 {
                bg2[x as usize] = Px { color: self.cgram[(p & 0x7F) as usize], prio: 1 + (p >> 7) as u8, math: true };
            }
        }
        (bg1, bg2)
    }

    /// Sprites on screen row `row`: priority 1-4, `math` = palettes 4-7. Up to 32 sprites
    /// and 34 eight-pixel slivers a line (the flags in 213E report overflow).
    fn obj_line(&mut self, row: u32) -> [Px; SCREEN_W] {
        const SIZES: [[(u32, u32); 2]; 8] = [
            [(8, 8), (16, 16)], [(8, 8), (32, 32)], [(8, 8), (64, 64)], [(16, 16), (32, 32)],
            [(16, 16), (64, 64)], [(32, 32), (64, 64)], [(16, 32), (32, 64)], [(16, 32), (32, 32)],
        ];
        let mut out = [Px::default(); SCREEN_W];
        let sizes = SIZES[(self.obsel >> 5) as usize];
        let name_base = ((self.obsel & 7) as u32) << 13;
        let name_gap = ((((self.obsel >> 3) & 3) as u32) + 1) << 12;
        let first = if self.oam_priority { (self.oam_base as usize >> 2) & 0x7F } else { 0 };
        // range: the first 32 sprites (from `first`) on this line
        let mut list: Vec<(usize, i32, u32, u32, u32)> = Vec::with_capacity(32);   // (index, x, w, h, dy)
        for k in 0..128 {
            let i = (first + k) & 127;
            let o = &self.oam[i * 4..i * 4 + 4];
            let hi = (self.oam[0x200 + i / 4] >> ((i & 3) * 2)) & 3;
            let mut x = o[0] as i32 | ((hi as i32 & 1) << 8);
            let (w, h) = sizes[(hi >> 1) as usize];
            if x >= 256 { x -= 512; }
            if x + w as i32 <= 0 && x != -256 { continue; }
            let dy = row.wrapping_sub(o[1] as u32) & 0xFF;
            if dy >= h { continue; }
            if list.len() == 32 { self.range_over = true; break; }
            list.push((i, x, w, h, dy));
        }
        // time: slivers are fetched from the last sprite in the list back to the first
        let mut slivers = 0;
        let mut over = false;
        let mut drawn: Vec<(usize, i32, u32, u32, u32, u32)> = Vec::new();  // + slivers allowed
        for &(i, x, w, h, dy) in list.iter().rev() {
            let mut n = 0;
            for t in 0..w / 8 {
                let sx = x + t as i32 * 8;
                if sx + 8 <= 0 || sx >= SCREEN_W as i32 { continue; }
                if slivers == 34 { over = true; break; }
                slivers += 1;
                n = t + 1;
            }
            if n > 0 { drawn.push((i, x, w, h, dy, n)); }
            if over { self.time_over = true; break; }
        }
        // `drawn` runs last-to-first in priority; later writes are higher priority
        for &(i, x, w, h, dy, tiles) in &drawn {
            let o = &self.oam[i * 4..i * 4 + 4];
            let attr = o[3];
            let tile = o[2] as u32 | ((attr & 1) as u32) << 8;
            let (vflip, hflip) = (attr & 0x80 != 0, attr & 0x40 != 0);
            let prio = 1 + ((attr >> 4) & 3);
            let pal = (attr >> 1) & 7;
            let sy = if vflip { h - 1 - dy } else { dy };
            for sx in 0..(tiles * 8).min(w) {
                let px = x + sx as i32;
                if !(0..SCREEN_W as i32).contains(&px) { continue; }
                let tx = if hflip { w - 1 - sx } else { sx };
                let t = (tile & 0x100) | (((tile & 0x0F) + tx / 8) & 0x0F) | (((tile & 0xF0) + (sy / 8) * 16) & 0xF0);
                let base = name_base + if t & 0x100 != 0 { name_gap } else { 0 } + (t & 0xFF) * 16;
                let r = sy & 7;
                let w0 = self.vram_word(((base + r) & 0x7FFF) as usize) as u32;
                let w1 = self.vram_word(((base + 8 + r) & 0x7FFF) as usize) as u32;
                let bit = 7 - (tx & 7);
                let c = (w0 >> bit & 1) | (w0 >> (8 + bit) & 1) << 1 | (w1 >> bit & 1) << 2 | (w1 >> (8 + bit) & 1) << 3;
                if c != 0 {
                    out[px as usize] = Px { color: self.cgram[(128 + pal as u32 * 16 + c) as usize], prio, math: pal >= 4 };
                }
            }
        }
        out
    }

    /// Window mask for a 4-bit layer setting (bit 1 W1 on, 0 W1 invert, 3 W2 on, 2 W2
    /// invert) combined with `logic` (0 OR, 1 AND, 2 XOR, 3 XNOR).
    fn window(&self, sel: u8, logic: u8) -> [bool; SCREEN_W] {
        let mut out = [false; SCREEN_W];
        let (e1, e2) = (sel & 2 != 0, sel & 8 != 0);
        if !e1 && !e2 { return out; }
        let (l1, r1, l2, r2) = (self.win[3] as usize, self.win[4] as usize, self.win[5] as usize, self.win[6] as usize);
        for (x, o) in out.iter_mut().enumerate() {
            let a = (l1 <= x && x <= r1) ^ (sel & 1 != 0);
            let b = (l2 <= x && x <= r2) ^ (sel & 4 != 0);
            *o = match (e1, e2) {
                (true, false) => a,
                (false, true) => b,
                _ => match logic & 3 { 0 => a | b, 1 => a & b, 2 => a ^ b, _ => !(a ^ b) },
            };
        }
        out
    }

    /// Draw screen row `row` (scanline row + 1) into `fb` (RGBA, 512 wide).
    pub fn render_line(&mut self, row: usize, fb: &mut [u8]) {
        let line = &mut fb[row * WIDTH * 4..(row + 1) * WIDTH * 4];
        let bright = (self.inidisp & 0x0F) as u32;
        if self.forced_blank() || bright == 0 {
            for p in line.chunks_exact_mut(4) { p.copy_from_slice(&[0, 0, 0, 255]); }
            return;
        }
        let y = row as u32 + 1;
        let mode = self.bgmode & 7;
        let hires = self.hires();
        let pseudo = self.setini & 0x08 != 0;
        // interlaced hi-res BGs have 448 lines; we draw one field (the even lines)
        let by = if hires && self.setini & 1 != 0 { y * 2 - 1 } else { y };

        // layers 0-3 BG, 4 OBJ; main and sub differ only in hi-res modes (column phase)
        let blank = [Px::default(); SCREEN_W];
        let used = self.tm | self.ts;
        let mut main_l = [blank; 5];
        if mode == 7 {
            let (a, b) = self.m7_line(y);
            main_l[0] = a;
            main_l[1] = b;
        } else {
            for n in 0..4 { if used & (1 << n) != 0 { main_l[n] = self.bg_line(n, by, 1); } }
        }
        if used & 0x10 != 0 { main_l[4] = self.obj_line(row as u32); }
        let mut sub_l = main_l;
        if hires {
            for n in 0..4 { if self.ts & (1 << n) != 0 { sub_l[n] = self.bg_line(n, by, 0); } }
        }

        // windows
        let logic = |n: usize| if n < 4 { self.win[7] >> (n * 2) & 3 } else { self.win[8] & 3 };
        let sel = |n: usize| match n { 0 => self.win[0] & 15, 1 => self.win[0] >> 4, 2 => self.win[1] & 15, 3 => self.win[1] >> 4, _ => self.win[2] & 15 };
        let masks: Vec<[bool; SCREEN_W]> = (0..5).map(|n| self.window(sel(n), logic(n))).collect();
        let color_win = self.window(self.win[2] >> 4, self.win[8] >> 2 & 3);

        let bg3_top = self.bgmode & 0x08 != 0;
        let order: &[(usize, u8)] = match mode {
            0 => &[(4, 4), (0, 2), (1, 2), (4, 3), (0, 1), (1, 1), (4, 2), (2, 2), (3, 2), (4, 1), (2, 1), (3, 1)],
            1 if bg3_top => &[(2, 2), (4, 4), (0, 2), (1, 2), (4, 3), (0, 1), (1, 1), (4, 2), (4, 1), (2, 1)],
            1 => &[(4, 4), (0, 2), (1, 2), (4, 3), (0, 1), (1, 1), (4, 2), (2, 2), (4, 1), (2, 1)],
            7 if self.setini & 0x40 != 0 => &[(4, 4), (4, 3), (1, 2), (4, 2), (0, 1), (4, 1), (1, 1)],
            7 => &[(4, 4), (4, 3), (4, 2), (0, 1), (4, 1)],
            _ => &[(4, 4), (0, 2), (4, 3), (1, 2), (4, 2), (0, 1), (4, 1), (1, 1)],
        };
        // the top pixel of a screen: (pixel, layer 0-4, or 5 = backdrop)
        let top = |layers: &[[Px; SCREEN_W]; 5], enable: u8, win_en: u8, x: usize| -> (Px, usize) {
            for &(l, p) in order {
                if enable & (1 << l) == 0 || (win_en & (1 << l) != 0 && masks[l][x]) { continue; }
                let px = layers[l][x];
                if px.prio == p { return (px, l); }
            }
            (Px::default(), 5)
        };

        let clip_mode = self.cgwsel >> 6;
        let prevent_mode = self.cgwsel >> 4 & 3;
        let region = |m: u8, x: usize| match m { 0 => false, 1 => !color_win[x], 2 => color_win[x], _ => true };
        let out = |c: u16| -> [u8; 4] {
            let ch = |s: u32| { let v = ((c as u32 >> s) & 0x1F) * (bright + 1) / 16; ((v << 3) | (v >> 2)) as u8 };
            [ch(0), ch(5), ch(10), 255]
        };
        for x in 0..SCREEN_W {
            let (m, ml) = top(&main_l, self.tm, self.tmw, x);
            let (s, sl) = top(&sub_l, self.ts, self.tsw, x);
            let main_c = if ml == 5 { self.cgram[0] } else { m.color };
            let clip = region(clip_mode, x);
            let mut c = if clip { 0 } else { main_c };
            let math = self.cgadsub & (1 << ml) != 0 && !region(prevent_mode, x) && (ml != 4 || m.math);
            if math {
                let use_sub = self.cgwsel & 2 != 0;
                let sub_backdrop = use_sub && sl == 5;
                let other = if use_sub && !sub_backdrop { s.color } else { self.coldata };
                let half = self.cgadsub & 0x40 != 0 && !clip && !sub_backdrop;
                c = blend(c, other, self.cgadsub & 0x80 != 0, half);
            }
            let odd = out(c);
            let even = if hires || pseudo { out(if sl == 5 { self.cgram[0] } else { s.color }) } else { odd };
            line[x * 8..x * 8 + 4].copy_from_slice(&even);
            line[x * 8 + 4..x * 8 + 8].copy_from_slice(&odd);
        }
    }
}

/// Colour math on two 15-bit colours: add or subtract each 5-bit part, clamped,
/// optionally halved.
fn blend(a: u16, b: u16, sub: bool, half: bool) -> u16 {
    let mut r = 0u16;
    for s in [0, 5, 10] {
        let (x, y) = ((a >> s & 31) as i32, (b >> s & 31) as i32);
        let v = if sub {
            let v = (x - y).max(0);
            if half { v >> 1 } else { v }
        } else if half {
            (x + y) >> 1
        } else {
            (x + y).min(31)
        };
        r |= (v as u16) << s;
    }
    r
}

/// One layer pixel: colour, priority (0 = transparent), and whether colour math may
/// apply (sprites only with palettes 4-7).
#[derive(Clone, Copy, Default)]
struct Px {
    color: u16,
    prio: u8,
    math: bool,
}
