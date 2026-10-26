//! Double Dragon video (MAME ddragon_v.cpp): a 512x512 background of 16x16 tiles
//! (scrolled), sprites of 16x16 to 32x32, and a 256x256 foreground of 8x8 characters
//! on top. 512 colours, xBGR 4-4-4 split over two RAMs (low byte 1000, high 1200).
//!
//! Drawn a line at a time as the beam gets there, so mid-frame changes show where
//! they happen (MAME updates the screen every scanline too). Screen line y shows
//! tilemap row y + 8 (+ scroll for the background).

use serde::{Deserialize, Serialize};

pub const SCREEN_W: usize = 256;
pub const SCREEN_H: usize = 240;

#[derive(Serialize, Deserialize)]
pub struct DdVideo {
    pub palette: Vec<u8>,      // 1000-11FF: green / red
    pub palette_ext: Vec<u8>,  // 1200-13FF: blue
    pub fgram: Vec<u8>,        // 1800-1FFF
    pub spriteram: Vec<u8>,    // 2800-2FFF
    pub bgram: Vec<u8>,        // 3000-37FF
    pub scrollx_lo: u8,
    pub scrolly_lo: u8,
    pub scrollx_hi: u8,
    pub scrolly_hi: u8,
    pub flip: bool,
    /// Double Dragon II sprite format: 3-bit colour, 13-bit code
    #[serde(skip)] pub dd2: bool,
    #[serde(skip)] chars: Vec<[u8; 64]>,
    #[serde(skip)] tiles: Vec<[u8; 256]>,
    #[serde(skip)] sprites: Vec<[u8; 256]>,
    #[serde(skip)] pub framebuffer: Vec<u8>,
}

impl DdVideo {
    pub fn new(chars: &[u8], tiles: &[u8], sprites: &[u8]) -> Self {
        Self {
            palette: vec![0; 0x200], palette_ext: vec![0; 0x200],
            fgram: vec![0; 0x800], spriteram: vec![0; 0x800], bgram: vec![0; 0x800],
            scrollx_lo: 0, scrolly_lo: 0, scrollx_hi: 0, scrolly_hi: 0, flip: false, dd2: false,
            chars: decode_chars(chars),
            tiles: decode_16x16(tiles),
            sprites: decode_16x16(sprites),
            framebuffer: vec![0; SCREEN_W * SCREEN_H * 4],
        }
    }

    /// After loading a state: take the decoded graphics back.
    pub fn keep_static(&mut self, old: &mut Self) {
        self.chars = std::mem::take(&mut old.chars);
        self.tiles = std::mem::take(&mut old.tiles);
        self.sprites = std::mem::take(&mut old.sprites);
        self.framebuffer = std::mem::take(&mut old.framebuffer);
        self.dd2 = old.dd2;
    }

    fn color(&self, i: usize) -> [u8; 4] {
        let v = self.palette[i] as u16 | ((self.palette_ext[i] as u16) << 8);
        let c = |n: u16| ((n & 0xF) * 17) as u8;
        [c(v), c(v >> 4), c(v >> 8), 255]
    }

    /// Draws visible line `y` (0-239).
    pub fn render_line(&mut self, y: usize) {
        let mut line = [0u16; SCREEN_W];

        // background (opaque)
        let scrollx = ((self.scrollx_hi as usize) << 8) | self.scrollx_lo as usize;
        let scrolly = ((self.scrolly_hi as usize) << 8) | self.scrolly_lo as usize;
        let by = (y + 8 + scrolly) & 511;
        let row = by >> 4;
        let nt = self.tiles.len().max(1);
        for (x, px) in line.iter_mut().enumerate() {
            let bx = (x + scrollx) & 511;
            let col = bx >> 4;
            let off = (col & 0x0F) | ((row & 0x0F) << 4) | ((col & 0x10) << 4) | ((row & 0x10) << 5);
            let attr = self.bgram[off * 2];
            let code = (self.bgram[off * 2 + 1] as usize | ((attr as usize & 7) << 8)) % nt;
            let tx = if attr & 0x40 != 0 { 15 - (bx & 15) } else { bx & 15 };
            let ty = if attr & 0x80 != 0 { 15 - (by & 15) } else { by & 15 };
            let pen = self.tiles.get(code).map_or(0, |t| t[ty * 16 + tx]) as u16;
            *px = 256 + ((attr as u16 >> 3) & 7) * 16 + pen;
        }

        // sprites, in RAM order (later ones on top), pen 0 transparent
        let ns = self.sprites.len().max(1);
        let yi = y as i32;
        for i in (0..0x800 - 4).step_by(5) {
            let s = &self.spriteram[i..i + 5];
            let attr = s[1];
            if attr & 0x80 == 0 { continue; }
            let sx = 240 - s[4] as i32 + (((attr & 2) as i32) << 7);
            let sy = 232 - s[0] as i32 + (((attr & 1) as i32) << 8);
            let size = ((attr & 0x30) >> 4) as usize;
            let flipx = attr & 8 != 0;
            let flipy = attr & 4 != 0;
            let (color, which) = if self.dd2 {
                ((s[2] >> 5) as u16, s[3] as usize | ((s[2] as usize & 0x1F) << 8))
            } else {
                ((s[2] >> 4) as u16, s[3] as usize | ((s[2] as usize & 0x0F) << 8))
            };
            let which = which & !size;
            let pieces: &[(usize, i32, i32)] = match size {
                0 => &[(0, 0, 0)],
                1 => &[(0, 0, -16), (1, 0, 0)],
                2 => &[(0, -16, 0), (2, 0, 0)],
                _ => &[(0, -16, -16), (1, -16, 0), (2, 0, -16), (3, 0, 0)],
            };
            for &(order, dx, dy) in pieces {
                let top = sy + dy;
                if yi < top || yi >= top + 16 { continue; }
                let r = (yi - top) as usize;
                let r = if flipy { 15 - r } else { r };
                let gfx = &self.sprites[(which + order) % ns];
                let left = sx + dx;
                for c in 0..16 {
                    let x = left + c;
                    if !(0..SCREEN_W as i32).contains(&x) { continue; }
                    let cc = if flipx { 15 - c as usize } else { c as usize };
                    let pen = gfx[r * 16 + cc] as u16;
                    if pen != 0 { line[x as usize] = 128 + color * 16 + pen; }
                }
            }
        }

        // foreground characters, pen 0 transparent
        let fy = (y + 8) & 255;
        let nc = self.chars.len().max(1);
        for (x, px) in line.iter_mut().enumerate() {
            let off = ((fy >> 3) * 32 + (x >> 3)) * 2;
            let attr = self.fgram[off];
            let code = (self.fgram[off + 1] as usize | ((attr as usize & 7) << 8)) % nc;
            let pen = self.chars.get(code).map_or(0, |t| t[(fy & 7) * 8 + (x & 7)]) as u16;
            if pen != 0 { *px = ((attr as u16) >> 5) * 16 + pen; }
        }

        // flip screen: the whole picture turned 180 degrees
        let out_y = if self.flip { SCREEN_H - 1 - y } else { y };
        for (x, &i) in line.iter().enumerate() {
            let out_x = if self.flip { SCREEN_W - 1 - x } else { x };
            let o = (out_y * SCREEN_W + out_x) * 4;
            let c = self.color(i as usize);
            self.framebuffer[o..o + 4].copy_from_slice(&c);
        }
    }
}

/// MAME bit order: offset b is bit 7 - (b & 7) of byte b >> 3. Plane 0 is the top bit.
fn bit(rom: &[u8], b: usize) -> u8 {
    rom.get(b >> 3).map_or(0, |v| (v >> (7 - (b & 7))) & 1)
}

/// char_layout: 8x8, planes 0,2,4,6; x 1,0,65,64,129,128,193,192; 32 bytes a char.
fn decode_chars(rom: &[u8]) -> Vec<[u8; 64]> {
    const XO: [usize; 8] = [1, 0, 65, 64, 129, 128, 193, 192];
    (0..rom.len() / 32).map(|n| {
        let base = n * 256;
        let mut px = [0u8; 64];
        for y in 0..8 {
            for x in 0..8 {
                let mut pen = 0;
                for (p, plane) in [0, 2, 4, 6].iter().enumerate() {
                    pen |= bit(rom, base + plane + XO[x] + y * 8) << (3 - p);
                }
                px[y * 8 + x] = pen;
            }
        }
        px
    }).collect()
}

/// tile_layout: 16x16, planes half+0, half+4, 0, 4; columns of 4 bits in 16-byte strips.
fn decode_16x16(rom: &[u8]) -> Vec<[u8; 256]> {
    let half = rom.len() / 2 * 8;
    let xo = |x: usize| (x / 4) * 128 + (3 - (x & 3));
    let planes = [half, half + 4, 0, 4];
    (0..rom.len() / 128).map(|n| {
        let base = n * 512;
        let mut px = [0u8; 256];
        for y in 0..16 {
            for x in 0..16 {
                let mut pen = 0;
                for (p, plane) in planes.iter().enumerate() {
                    pen |= bit(rom, base + plane + xo(x) + y * 8) << (3 - p);
                }
                px[y * 16 + x] = pen;
            }
        }
        px
    }).collect()
}

#[cfg(test)]
pub(crate) fn decode_for_test(rom: &[u8]) -> (Vec<[u8; 64]>, Vec<[u8; 256]>) {
    (decode_chars(rom), decode_16x16(rom))
}
