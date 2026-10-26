//! Green Beret video (MAME gberet.cpp + Konami 005849): one 512x256 tilemap of 8x8
//! 4bpp tiles with per-row X scroll, 48 16x16 sprites, a 32-colour PROM palette
//! looked up through two 256-entry PROMs (characters, sprites).
//!
//! Drawn like MAME's screen_update, once a frame: all tiles opaque, then sprites,
//! then tiles whose attribute bit 7 is clear go back on top (pens whose lookup is
//! colour 0x10 are see-through).

use serde::{Deserialize, Serialize};

pub const SCREEN_W: usize = 256;
pub const SCREEN_H: usize = 224;
pub const FIRST_LINE: usize = 16;

#[derive(Serialize, Deserialize)]
pub struct GberetVideo {
    pub colorram: Vec<u8>,     // c000-c7ff
    pub videoram: Vec<u8>,     // c800-cfff
    pub spriteram2: Vec<u8>,   // d000-d0ff
    pub spriteram: Vec<u8>,    // d100-d1ff
    pub scrollram: Vec<u8>,    // e000-e03f (005849)
    pub ctrl: [u8; 8],         // e040-e047 (005849)
    #[serde(skip)] tiles: Vec<[u8; 64]>,
    #[serde(skip)] sprites: Vec<[u8; 256]>,
    #[serde(skip)] colors: Vec<[u8; 4]>,     // 32 RGB colours
    #[serde(skip)] char_lut: Vec<u8>,        // pen -> colour (0x10-0x1F)
    #[serde(skip)] sprite_lut: Vec<u8>,      // pen -> colour (0x00-0x0F)
    #[serde(skip)] pub framebuffer: Vec<u8>,
}

impl GberetVideo {
    pub fn new(tiles: &[u8], sprites: &[u8], proms: &[u8]) -> Result<Self, String> {
        if proms.len() < 0x220 { return Err("colour PROMs missing".into()); }
        let colors = (0..32).map(|i| {
            let p = proms[i];
            let b = |n: u8| (p >> n) & 1;
            let r = 0x21 * b(0) as u32 + 0x47 * b(1) as u32 + 0x97 * b(2) as u32;
            let g = 0x21 * b(3) as u32 + 0x47 * b(4) as u32 + 0x97 * b(5) as u32;
            let bl = 0x52 * b(6) as u32 + 0xAD * b(7) as u32;
            [r as u8, g as u8, bl as u8, 255]
        }).collect();
        Ok(Self {
            colorram: vec![0; 0x800], videoram: vec![0; 0x800],
            spriteram2: vec![0; 0x100], spriteram: vec![0; 0x100],
            scrollram: vec![0; 0x40], ctrl: [0; 8],
            tiles: decode_tiles(tiles),
            sprites: decode_sprites(sprites),
            colors,
            char_lut: proms[0x20..0x120].iter().map(|v| (v & 0x0F) | 0x10).collect(),
            sprite_lut: proms[0x120..0x220].iter().map(|v| v & 0x0F).collect(),
            framebuffer: vec![0; SCREEN_W * SCREEN_H * 4],
        })
    }

    /// After loading a state: take the decoded graphics back.
    pub fn keep_static(&mut self, old: &mut Self) {
        self.tiles = std::mem::take(&mut old.tiles);
        self.sprites = std::mem::take(&mut old.sprites);
        self.colors = std::mem::take(&mut old.colors);
        self.char_lut = std::mem::take(&mut old.char_lut);
        self.sprite_lut = std::mem::take(&mut old.sprite_lut);
        self.framebuffer = std::mem::take(&mut old.framebuffer);
    }

    /// 005849 control write. Returns the interrupt enables that just went from 1 to 0
    /// (bit 0 NMI, 1 IRQ, 2 FIRQ): those lines get cleared.
    pub fn ctrl_w(&mut self, offset: usize, data: u8) -> u8 {
        let off = offset & 7;
        let cleared = if off == 4 { !data & self.ctrl[4] & 0x07 } else { 0 };
        self.ctrl[off] = data;
        cleared
    }

    pub fn render(&mut self) {
        let ntiles = self.tiles.len();
        let nspr = self.sprites.len();
        // colour index per pixel; tile "front" mask (category 0 and not see-through)
        let mut pix = vec![0u8; SCREEN_W * SCREEN_H];
        let mut front = vec![false; SCREEN_W * SCREEN_H];
        let mask_edges = self.ctrl[3] & 0x80 != 0;
        let (xmin, xmax) = if mask_edges { (8, SCREEN_W - 8) } else { (0, SCREEN_W) };

        for sy in 0..SCREEN_H {
            let y = sy + FIRST_LINE;
            let row = y / 8;
            let scroll = self.scrollram[row] as usize | ((self.scrollram[row | 0x20] as usize & 1) << 8);
            for x in 0..SCREEN_W {
                let i = sy * SCREEN_W + x;
                if x < xmin || x >= xmax {
                    pix[i] = self.char_lut[0];      // MAME fills with pen 0
                    continue;
                }
                let tx = (x + scroll) & 511;
                let idx = row * 64 + tx / 8;
                let attr = self.colorram[idx];
                let code = (self.videoram[idx] as usize + ((attr as usize & 0x40) << 2)) % ntiles;
                let px = if attr & 0x10 != 0 { 7 - (tx & 7) } else { tx & 7 };
                let py = if attr & 0x20 != 0 { 7 - (y & 7) } else { y & 7 };
                let pen = self.tiles[code][py * 8 + px] as usize;
                let c = self.char_lut[(attr as usize & 0x0F) * 16 + pen];
                pix[i] = c;
                front[i] = attr & 0x80 == 0 && c != 0x10;
            }
        }

        let sr = if self.ctrl[3] & 0x08 != 0 { &self.spriteram2 } else { &self.spriteram };
        for offs in (0..0xC0).step_by(4) {
            if sr[offs + 3] == 0 { continue; }
            let attr = sr[offs + 1];
            let code = (sr[offs] as usize + ((attr as usize & 0x40) << 2)) % nspr;
            let color = (attr & 0x0F) as usize;
            let sx = sr[offs + 2] as i32 - 2 * (attr & 0x80) as i32;
            let sy = sr[offs + 3] as i32;
            let flipx = attr & 0x10 != 0;
            let flipy = attr & 0x20 != 0;
            let spr = &self.sprites[code];
            for dy in 0..16 {
                let y = sy + dy - FIRST_LINE as i32;
                if !(0..SCREEN_H as i32).contains(&y) { continue; }
                let srow = if flipy { 15 - dy } else { dy } as usize;
                for dx in 0..16 {
                    let x = sx + dx;
                    if x < xmin as i32 || x >= xmax as i32 { continue; }
                    let scol = if flipx { 15 - dx } else { dx } as usize;
                    let c = self.sprite_lut[color * 16 + spr[srow * 16 + scol] as usize];
                    if c == 0 { continue; }
                    let i = y as usize * SCREEN_W + x as usize;
                    if !front[i] { pix[i] = c; }
                }
            }
        }

        for (i, &c) in pix.iter().enumerate() {
            self.framebuffer[i * 4..i * 4 + 4].copy_from_slice(&self.colors[(c & 0x1F) as usize]);
        }
    }
}

/// 8x8 4bpp packed, high nibble = left pixel, 4 bytes a row (MAME gfx_8x8x4_packed_msb).
fn decode_tiles(rom: &[u8]) -> Vec<[u8; 64]> {
    (0..rom.len() / 32).map(|t| {
        let mut px = [0u8; 64];
        for y in 0..8 {
            for x in 0..8 {
                let b = rom[t * 32 + y * 4 + x / 2];
                px[y * 8 + x] = if x & 1 == 0 { b >> 4 } else { b & 0x0F };
            }
        }
        px
    }).collect()
}

/// 16x16 made of four packed 8x8 blocks: top-left, top-right, bottom-left, bottom-right
/// (MAME gfx_8x8x4_row_2x2_group_packed_msb).
fn decode_sprites(rom: &[u8]) -> Vec<[u8; 256]> {
    (0..rom.len() / 128).map(|s| {
        let mut px = [0u8; 256];
        for y in 0..16 {
            for x in 0..16 {
                let block = (y / 8) * 2 + (x / 8);
                let b = rom[s * 128 + block * 32 + (y & 7) * 4 + (x & 7) / 2];
                px[y * 16 + x] = if x & 1 == 0 { b >> 4 } else { b & 0x0F };
            }
        }
        px
    }).collect()
}
