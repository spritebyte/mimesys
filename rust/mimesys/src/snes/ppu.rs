//! SNES PPU (stage 1): VRAM / OAM / CGRAM ports, background modes 0-6 as plain tiled
//! layers (2 / 4 / 8 bits a pixel, 8x8 or 16x16 tiles, 32-64 tile maps, scrolling),
//! sprites, layer priority, main-screen enables, brightness and forced blank. One line is
//! drawn at a time (at the start of its horizontal blank), so values HDMA writes between
//! lines take effect, mid-line writes don't.
//!
//! Not yet (stage 2): Mode 7, windows, colour math / sub screen, mosaic, offset-per-tile,
//! hi-res, interlace, direct colour, sprite range / time limits.
//!
//! Register behaviour follows ares (ISC): the VRAM address remapping and prefetch, the
//! write-twice scroll registers, CGRAM's two-write latch, the OAM low-table latch, and
//! VRAM / OAM writes ignored outside blanking.

use serde::{Deserialize, Serialize};

pub const WIDTH: usize = 256;
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
            mdr1: 0, mdr2: 0, mpy: 0,
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
            0x3E => { self.mdr1 = (self.mdr1 & 0x10) | 0x01; self.mdr1 }
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

    // ---------------- rendering ----------------

    /// Colour depth of BG n in the current mode (0 = layer absent).
    fn bpp(&self, n: usize) -> u32 {
        const T: [[u32; 4]; 8] = [[2, 2, 2, 2], [4, 4, 2, 0], [4, 4, 0, 0], [8, 4, 0, 0], [8, 2, 0, 0], [4, 2, 0, 0], [4, 0, 0, 0], [0, 0, 0, 0]];
        T[(self.bgmode & 7) as usize][n]
    }

    /// One background's pixel at screen x on line `y` (the BG line, after scroll):
    /// (palette index into CGRAM, priority bit), or None when transparent.
    fn bg_line(&self, n: usize, y: u32, out: &mut [(u8, u8); WIDTH]) {
        let bpp = self.bpp(n);
        if bpp == 0 { return; }
        let b = &self.bg[n];
        let tile_px = if b.big { 16 } else { 8 };
        let map_w = if b.map_size & 1 != 0 { 64 } else { 32 };
        let map_h = if b.map_size & 2 != 0 { 64 } else { 32 };
        let yy = (y + b.vofs as u32) & (map_h * tile_px - 1);
        let ty = yy / tile_px;
        let pal_base: u32 = if self.bgmode & 7 == 0 { n as u32 * 32 } else { 0 };
        for x in 0..WIDTH as u32 {
            let xx = (x + b.hofs as u32) & (map_w * tile_px - 1);
            let tx = xx / tile_px;
            // 32x32 screens laid out left-right then top-bottom
            let mut scr = (ty & 31) * 32 + (tx & 31);
            if tx >= 32 { scr += 0x400; }
            if ty >= 32 { scr += if map_w == 64 { 0x800 } else { 0x400 }; }
            let entry = self.vram_word(((b.map_base as u32 + scr) & 0x7FFF) as usize);
            let mut tile = (entry & 0x3FF) as u32;
            let hflip = entry & 0x4000 != 0;
            let vflip = entry & 0x8000 != 0;
            let mut px = xx % tile_px;
            let mut py = yy % tile_px;
            if hflip { px = tile_px - 1 - px; }
            if vflip { py = tile_px - 1 - py; }
            if b.big {
                if px >= 8 { tile += 1; }
                if py >= 8 { tile += 16; }
                px &= 7; py &= 7;
            }
            let words_per_tile = bpp * 4;
            let base = b.char_base as u32 + (tile & 0x3FF) * words_per_tile;
            let mut c = 0u32;
            for plane_pair in 0..bpp / 2 {
                let w = self.vram_word(((base + plane_pair * 8 + py) & 0x7FFF) as usize);
                let bit = 7 - px;
                c |= ((w as u32 >> bit) & 1) << (plane_pair * 2);
                c |= ((w as u32 >> (8 + bit)) & 1) << (plane_pair * 2 + 1);
            }
            if c == 0 { continue; }
            let pal = ((entry >> 10) & 7) as u32;
            let idx = match bpp { 2 => pal_base + pal * 4 + c, 4 => pal * 16 + c, _ => c };
            out[x as usize] = (idx as u8, 1 + ((entry >> 13) & 1) as u8);
        }
    }

    /// Sprites on screen row `row`: (CGRAM index, priority 0-3 + 1). Lower OAM entries win.
    fn obj_line(&self, row: u32, out: &mut [(u8, u8); WIDTH]) {
        const SIZES: [[(u32, u32); 2]; 8] = [
            [(8, 8), (16, 16)], [(8, 8), (32, 32)], [(8, 8), (64, 64)], [(16, 16), (32, 32)],
            [(16, 16), (64, 64)], [(32, 32), (64, 64)], [(16, 32), (32, 64)], [(16, 32), (32, 32)],
        ];
        let sizes = SIZES[(self.obsel >> 5) as usize];
        let name_base = ((self.obsel & 7) as u32) << 13;
        let name_gap = ((((self.obsel >> 3) & 3) as u32) + 1) << 12;
        // draw from the highest index down so lower indexes end on top
        for i in (0..128usize).rev() {
            let o = &self.oam[i * 4..i * 4 + 4];
            let hi = (self.oam[0x200 + i / 4] >> ((i & 3) * 2)) & 3;
            let mut x = o[0] as i32 | ((hi as i32 & 1) << 8);
            if x >= 256 { x -= 512; }
            let (w, h) = sizes[(hi >> 1) as usize];
            let y = o[1] as u32;
            let dy = row.wrapping_sub(y) & 0xFF;
            if dy >= h { continue; }
            let attr = o[3];
            let tile = o[2] as u32 | ((attr & 1) as u32) << 8;
            let vflip = attr & 0x80 != 0;
            let hflip = attr & 0x40 != 0;
            let prio = 1 + ((attr >> 4) & 3);
            let pal = 128 + ((attr >> 1) & 7) as u32 * 16;
            let sy = if vflip { h - 1 - dy } else { dy };
            for sx in 0..w {
                let px = x + sx as i32;
                if !(0..WIDTH as i32).contains(&px) { continue; }
                let tx = if hflip { w - 1 - sx } else { sx };
                let t = (tile & 0x100) | (((tile & 0x0F) + tx / 8) & 0x0F) | (((tile & 0xF0) + (sy / 8) * 16) & 0xF0);
                let base = name_base + if t & 0x100 != 0 { name_gap } else { 0 } + (t & 0xFF) * 16;
                let r = sy & 7;
                let w0 = self.vram_word(((base + r) & 0x7FFF) as usize) as u32;
                let w1 = self.vram_word(((base + 8 + r) & 0x7FFF) as usize) as u32;
                let bit = 7 - (tx & 7);
                let c = (w0 >> bit & 1) | (w0 >> (8 + bit) & 1) << 1 | (w1 >> bit & 1) << 2 | (w1 >> (8 + bit) & 1) << 3;
                if c != 0 { out[px as usize] = ((pal + c) as u8, prio); }
            }
        }
    }

    /// Draw screen row `row` (scanline row + 1) into `fb` (RGBA, 256 wide).
    pub fn render_line(&self, row: usize, fb: &mut [u8]) {
        let line = &mut fb[row * WIDTH * 4..(row + 1) * WIDTH * 4];
        if self.forced_blank() || self.inidisp & 0x0F == 0 {
            line.iter_mut().enumerate().for_each(|(i, b)| *b = if i % 4 == 3 { 255 } else { 0 });
            return;
        }
        let mut layers = [[(0u8, 0u8); WIDTH]; 5];   // BG1-4, OBJ
        let y = row as u32 + 1;
        for n in 0..4 {
            if self.tm & (1 << n) != 0 { self.bg_line(n, y, &mut layers[n]); }
        }
        if self.tm & 0x10 != 0 { self.obj_line(row as u32, &mut layers[4]); }
        // priority, highest first: (layer, priority value)
        let mode = self.bgmode & 7;
        let bg3_top = self.bgmode & 0x08 != 0;
        let order: &[(usize, u8)] = match mode {
            0 => &[(4, 4), (0, 2), (1, 2), (4, 3), (0, 1), (1, 1), (4, 2), (2, 2), (3, 2), (4, 1), (2, 1), (3, 1)],
            1 if bg3_top => &[(2, 2), (4, 4), (0, 2), (1, 2), (4, 3), (0, 1), (1, 1), (4, 2), (4, 1), (2, 1)],
            1 => &[(4, 4), (0, 2), (1, 2), (4, 3), (0, 1), (1, 1), (4, 2), (2, 2), (4, 1), (2, 1)],
            _ => &[(4, 4), (0, 2), (4, 3), (1, 2), (4, 2), (0, 1), (4, 1), (1, 1)],
        };
        let bright = (self.inidisp & 0x0F) as u32;
        for x in 0..WIDTH {
            let mut idx = 0u8;
            for &(l, p) in order {
                let (c, pr) = layers[l][x];
                if pr == p { idx = c; break; }
            }
            let col = self.cgram[idx as usize];
            let ch = |s: u32| -> u8 {
                let v = ((col as u32 >> s) & 0x1F) * (bright + 1) / 16;
                ((v << 3) | (v >> 2)) as u8
            };
            line[x * 4..x * 4 + 4].copy_from_slice(&[ch(0), ch(5), ch(10), 255]);
        }
    }
}
