//! Atari System 2 video (MAME atarisy2_v.cpp + atarimo.cpp, rendered line by line).
//!
//! Three layers, 512x384:
//!   playfield  128x64 tiles of 8x8, 4bpp, scrolling, 2 priority bits per tile
//!   motion objects  16x16 4bpp, linked list of up to 256, pen 15 transparent
//!   alphanumerics  64x48 tiles of 8x8, 2bpp, on top, pen 0 transparent
//! Palette: 256 words RRRRGGGGBBBBIIII. MO 0-63, alpha 64-95, playfield 128-255.

use serde::{Deserialize, Serialize};

pub const SCREEN_W: usize = 512;
pub const SCREEN_H: usize = 384;

#[derive(Serialize, Deserialize)]
pub struct Sy2Video {
    pub palette_ram: Vec<u16>,   // 256
    pub alpha_ram: Vec<u16>,     // 0x2000-0x37FF: 3072 words (64x48)
    pub mo_ram: Vec<u16>,        // 0x3800-0x3FFF: 1024 words (256 x 4)
    pub pf_ram: Vec<u16>,        // 8192 words: top half (view 2) then bottom half (view 3)
    pub xscroll: u16,
    pub yscroll: u16,
    /// Playfield Y offset: tile row for screen line L is L + scroll_y_offset.
    scroll_y_offset: i32,
    /// Y scroll waiting for the start of the next frame (yscroll bit 4 set).
    pending_y: Option<i32>,
    last_xpos: i32,

    #[serde(skip)] tiles: Vec<[u8; 64]>,
    #[serde(skip)] chars: Vec<[u8; 64]>,
    #[serde(skip)] sprites: Vec<[u8; 256]>,
    #[serde(skip)] colors: Vec<[u8; 4]>,
    #[serde(skip)] pub framebuffer: Vec<u8>,
}

impl Sy2Video {
    pub fn new(tiles: &[u8], sprites: &[u8], chars: &[u8]) -> Result<Self, String> {
        if tiles.len() < 0x40 || sprites.len() < 0x100 || chars.len() < 0x10 {
            return Err("System 2 graphics ROMs missing".into());
        }
        let mut v = Self {
            palette_ram: vec![0; 256],
            alpha_ram: vec![0; 0xC00],
            mo_ram: vec![0; 0x400],
            pf_ram: vec![0; 0x2000],
            xscroll: 0, yscroll: 0, scroll_y_offset: 0, pending_y: None, last_xpos: 0,
            tiles: decode_tiles(tiles),
            chars: decode_chars(chars),
            sprites: decode_sprites(sprites),
            colors: vec![[0, 0, 0, 255]; 256],
            framebuffer: vec![0; SCREEN_W * SCREEN_H * 4],
        };
        for i in 0..256 { v.update_color(i); }
        Ok(v)
    }

    /// After loading a state: take the decoded graphics back from the running machine.
    pub fn keep_static(&mut self, old: &mut Self) {
        self.tiles = std::mem::take(&mut old.tiles);
        self.chars = std::mem::take(&mut old.chars);
        self.sprites = std::mem::take(&mut old.sprites);
        self.framebuffer = std::mem::take(&mut old.framebuffer);
        self.colors = vec![[0, 0, 0, 255]; 256];
        for i in 0..256 { self.update_color(i); }
    }

    pub fn write_palette(&mut self, index: usize, val: u16) {
        self.palette_ram[index] = val;
        self.update_color(index);
    }

    fn update_color(&mut self, i: usize) {
        const ZB: i32 = 115; const Z3: i32 = 78; const Z2: i32 = 37; const Z1: i32 = 17; const Z0: i32 = 9;
        const INTENSITY: [i32; 16] = [
            0, ZB + Z0, ZB + Z1, ZB + Z1 + Z0, ZB + Z2, ZB + Z2 + Z0, ZB + Z2 + Z1, ZB + Z2 + Z1 + Z0,
            ZB + Z3, ZB + Z3 + Z0, ZB + Z3 + Z1, ZB + Z3 + Z1 + Z0, ZB + Z3 + Z2, ZB + Z3 + Z2 + Z0,
            ZB + Z3 + Z2 + Z1, ZB + Z3 + Z2 + Z1 + Z0,
        ];
        const COLOR: [i32; 16] = [0x0, 0x3, 0x4, 0x5, 0x6, 0x7, 0x8, 0x9, 0xA, 0xB, 0xC, 0xD, 0xE, 0xE, 0xF, 0xF];
        let raw = self.palette_ram[i] as usize;
        let k = INTENSITY[raw & 15];
        let c = |s: usize| ((COLOR[(raw >> s) & 15] * k) >> 4) as u8;
        self.colors[i] = [c(12), c(8), c(4), 255];
    }

    pub fn xscroll_w(&mut self, v: u16) { self.xscroll = v; }

    /// Bit 4 clear: the new scroll takes effect at the current line; set: at the next frame.
    pub fn yscroll_w(&mut self, v: u16, line: u16) {
        self.yscroll = v;
        let y = (v >> 6) as i32;
        if v & 0x10 == 0 {
            self.scroll_y_offset = y - line as i32;
            self.pending_y = None;
        } else {
            self.pending_y = Some(y);
        }
    }

    pub fn start_frame(&mut self) {
        if let Some(y) = self.pending_y.take() { self.scroll_y_offset = y; }
    }

    pub fn render_line(&mut self, line: usize) {
        let mut pf = [0u16; SCREEN_W];
        let mut pri = [0u8; SCREEN_W];

        // ---- playfield ----
        let banks = [(self.xscroll & 0x0F) as usize, (self.yscroll & 0x0F) as usize];
        let sx = (self.xscroll >> 6) as usize;
        let row = ((line as i32 + self.scroll_y_offset) & 511) as usize;
        let ntiles = self.tiles.len();
        for (x, (p, pr)) in pf.iter_mut().zip(pri.iter_mut()).enumerate() {
            let col = (x + sx) & 1023;
            let data = self.pf_ram[(row / 8) * 128 + col / 8];
            let code = ((banks[((data >> 10) & 1) as usize] << 10) | (data & 0x3FF) as usize) % ntiles;
            let color = ((data >> 11) & 7) as u16;
            let pen = self.tiles[code][(row & 7) * 8 + (col & 7)] as u16;
            *p = 128 + color * 16 + pen;
            *pr = ((!data >> 14) & 3) as u8;
        }

        // ---- motion objects ----
        let mut mo = [0xFFFFu16; SCREEN_W];
        self.draw_mo_line(line as i32, &mut mo);
        for x in 0..SCREEN_W {
            let m = mo[x];
            if m == 0xFFFF { continue; }
            let mopri = (m >> 12) as u8;
            if (mopri + pri[x]) & 2 != 0 {
                if pf[x] & 0x08 == 0 { pf[x] = m & 0x0FFF; }
            } else {
                pf[x] = m & 0x0FFF;
            }
        }

        // ---- alphanumerics ----
        let arow = line / 8;
        let nchars = self.chars.len();
        for x in 0..SCREEN_W {
            let data = self.alpha_ram[arow * 64 + x / 8];
            let code = (data & 0x3FF) as usize % nchars;
            let pen = self.chars[code][(line & 7) * 8 + (x & 7)] as u16;
            if pen != 0 { pf[x] = 64 + ((data >> 13) & 7) * 4 + pen; }
        }

        let out = &mut self.framebuffer[line * SCREEN_W * 4..(line + 1) * SCREEN_W * 4];
        for (x, &p) in pf.iter().enumerate() {
            out[x * 4..x * 4 + 4].copy_from_slice(&self.colors[(p & 0xFF) as usize]);
        }
    }

    /// Walks the linked list from entry 0 and draws the objects crossing `line`
    /// (later objects drawn over earlier ones).
    fn draw_mo_line(&mut self, line: i32, mo: &mut [u16; SCREEN_W]) {
        let mut visited = [false; 256];
        let mut link = 0usize;
        let nspr = self.sprites.len();
        while !visited[link] {
            visited[link] = true;
            let e = &self.mo_ram[link * 4..link * 4 + 4];
            let (w0, w1, w2, w3) = (e[0], e[1], e[2], e[3]);
            link = ((w3 >> 3) & 0xFF) as usize;

            let code = (((w0 & 7) as usize) << 11) | (w1 & 0x7FF) as usize;
            let color = ((w3 >> 12) & 3) as u16;
            let priority = (w3 >> 14) as u16;
            let mut xpos = (w2 >> 6) as i32;
            let height = (((w1 >> 11) & 7) + 1) as i32;
            let hflip = w1 & 0x4000 != 0;
            if w1 & 0x8000 != 0 { xpos = self.last_xpos + 16; }   // "hold": continue from the last object
            self.last_xpos = xpos;
            let mut ypos = -((w0 >> 6) as i32 & 0x1FF) - height * 16;
            xpos &= 1023;
            ypos &= 511;
            if xpos >= SCREEN_W as i32 { xpos -= 1024; }
            if ypos >= SCREEN_H as i32 { ypos -= 512; }

            if line < ypos || line >= ypos + height * 16 { continue; }
            let tile_row = (line - ypos) / 16;
            let py = ((line - ypos) & 15) as usize;
            let tile = &self.sprites[(code + tile_row as usize) % nspr];
            let base = (color * 16) | (priority << 12);
            for i in 0..16 {
                let px = xpos + i;
                if !(0..SCREEN_W as i32).contains(&px) { continue; }
                let sxp = if hflip { 15 - i as usize } else { i as usize };
                let pen = tile[py * 16 + sxp] as u16;
                if pen != 15 { mo[px as usize] = base + pen; }
            }
        }
    }
}

/// MAME gfx bit order: bit offset 0 = MSB of byte 0. Pixel x of a row starting at
/// `row_off` (bytes): byte row_off + x/4, plane bits at offsets (x&3) and 4+(x&3).
fn nib_bits(b: u8, x: usize) -> (u8, u8) {
    ((b >> (7 - (x & 3))) & 1, (b >> (3 - (x & 3))) & 1)
}

/// Playfield: 8x8, 4 planes {0, 4, half+0, half+4}, 16 bytes per tile per half.
fn decode_tiles(rom: &[u8]) -> Vec<[u8; 64]> {
    let half = rom.len() / 2;
    (0..half / 16).map(|t| {
        let mut px = [0u8; 64];
        for y in 0..8 {
            for x in 0..8 {
                let o = t * 16 + y * 2 + x / 4;
                let (p0, p1) = nib_bits(rom[o], x);
                let (p2, p3) = nib_bits(rom[half + o], x);
                px[y * 8 + x] = (p0 << 3) | (p1 << 2) | (p2 << 1) | p3;
            }
        }
        px
    }).collect()
}

/// Alphanumerics: 8x8, 2 planes {0, 4}, 16 bytes per tile.
fn decode_chars(rom: &[u8]) -> Vec<[u8; 64]> {
    (0..rom.len() / 16).map(|t| {
        let mut px = [0u8; 64];
        for y in 0..8 {
            for x in 0..8 {
                let (p0, p1) = nib_bits(rom[t * 16 + y * 2 + x / 4], x);
                px[y * 8 + x] = (p0 << 1) | p1;
            }
        }
        px
    }).collect()
}

/// Motion objects: 16x16, 4 planes {0, 4, half+0, half+4}, 64 bytes per tile per half.
/// The ROMs are stored inverted (MAME ROMREGION_INVERT).
fn decode_sprites(rom: &[u8]) -> Vec<[u8; 256]> {
    let half = rom.len() / 2;
    (0..half / 64).map(|t| {
        let mut px = [0u8; 256];
        for y in 0..16 {
            for x in 0..16 {
                let o = t * 64 + y * 4 + x / 4;
                let (p0, p1) = nib_bits(!rom[o], x);
                let (p2, p3) = nib_bits(!rom[half + o], x);
                px[y * 16 + x] = (p0 << 3) | (p1 << 2) | (p2 << 1) | p3;
            }
        }
        px
    }).collect()
}
