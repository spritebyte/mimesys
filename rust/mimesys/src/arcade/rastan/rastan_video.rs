//! Rastan video: Taito PC080SN (two 64x64 tilemaps of 8x8 tiles) and PC090OJ (256
//! 16x16 sprites), 2048 colours xBGR 5:5:5. After MAME pc080sn.cpp / pc090oj.cpp
//! (Nicola Salmoria) and rastan.cpp (Jarek Burczynski).
//!
//! PC080SN RAM (words): 0000-1FFF background (attr, code pairs), 2000-20FF background
//! row scroll, 4000-5FFF foreground, 6000-60FF foreground row scroll. Attr: bits 0-8
//! colour, 14 flip X, 15 flip Y; code 14 bits. Scroll registers are negated on write;
//! the screen starts 16 pixels into the tilemap.
//!
//! PC090OJ RAM (words): 4 words a sprite (flags/colour, Y, code, X), first sprite on top;
//! word 0xDFF bit 0 clear = flipped screen. Sprite colour bank from 0x380000 bits 5-7.
//! Screen 320x256 with lines 8-247 shown.

use serde::{Deserialize, Serialize};

pub const SCREEN_W: usize = 320;
pub const SCREEN_H: usize = 240;
const FIRST_LINE: usize = 8;

#[derive(Serialize, Deserialize)]
pub struct Video {
    pub tile_ram: Vec<u16>,
    pub sprite_ram: Vec<u16>,
    pub palette: Vec<u16>,
    /// PC080SN: x scroll [bg, fg], y scroll [bg, fg], control
    pub xscroll: [u16; 2],
    pub yscroll: [u16; 2],
    pub ctrl: u16,
    /// 0x380000: sprite colour bank (bits 5-7)
    pub sprite_ctrl: u16,
    #[serde(skip)] tiles: Vec<u8>,
    #[serde(skip)] sprites: Vec<u8>,
    #[serde(skip)] pub framebuffer: Vec<u8>,
}

impl Video {
    pub fn new(tiles: Vec<u8>, sprites: Vec<u8>) -> Result<Self, String> {
        if tiles.len() < 32 || sprites.len() < 128 { return Err("Rastan graphics regions missing".into()); }
        Ok(Self {
            tile_ram: vec![0; 0x8000],
            sprite_ram: vec![0; 0x2000],
            palette: vec![0; 0x800],
            xscroll: [0; 2], yscroll: [0; 2], ctrl: 0, sprite_ctrl: 0,
            tiles, sprites,
            framebuffer: vec![0; SCREEN_W * SCREEN_H * 4],
        })
    }

    pub fn keep_static(&mut self, old: &mut Video) {
        self.tiles = std::mem::take(&mut old.tiles);
        self.sprites = std::mem::take(&mut old.sprites);
        self.framebuffer = std::mem::take(&mut old.framebuffer);
    }

    fn color(&self, pen: usize) -> [u8; 4] {
        let c = self.palette[pen & 0x7FF];
        let x = |v: u16| ((v & 31) << 3 | (v & 31) >> 2) as u8;
        [x(c), x(c >> 5), x(c >> 10), 255]
    }

    /// Tilemap `layer` at screen (x, y): (colour group * 16, pen 0-15).
    fn tile_pixel(&self, layer: usize, x: usize, y: usize) -> (u16, u16) {
        let base = layer * 0x4000;
        let rowscroll = self.tile_ram[base + 0x2000 + (y & 0xFF)];
        let tx = (x as u32 + 16).wrapping_sub(self.xscroll[layer] as u32).wrapping_sub(rowscroll as u32) & 0x1FF;
        let ty = (y as u32).wrapping_sub(self.yscroll[layer] as u32) & 0x1FF;
        let idx = (ty as usize >> 3) * 64 + (tx as usize >> 3);
        let attr = self.tile_ram[base + 2 * idx];
        let code = (self.tile_ram[base + 2 * idx + 1] & 0x3FFF) as usize;
        let (mut px, mut py) = (tx as usize & 7, ty as usize & 7);
        if attr & 0x4000 != 0 { px = 7 - px; }
        if attr & 0x8000 != 0 { py = 7 - py; }
        let b = self.tiles[(code * 32 + py * 4 + px / 2) % self.tiles.len()];
        let pen = if px & 1 == 0 { b >> 4 } else { b & 15 } as u16;
        ((attr & 0x1FF) * 16, pen)
    }

    pub fn render(&mut self) {
        let flip = self.ctrl & 1 != 0;
        let mut pens = vec![0u16; SCREEN_W * SCREEN_H];
        for oy in 0..SCREEN_H {
            for ox in 0..SCREEN_W {
                let (x, y) = if flip { (SCREEN_W - 1 - ox, 255 - (oy + FIRST_LINE)) } else { (ox, oy + FIRST_LINE) };
                // background opaque, foreground with pen 0 transparent
                let (bc, bp) = self.tile_pixel(0, x, y);
                let (fc, fp) = self.tile_pixel(1, x, y);
                pens[oy * SCREEN_W + ox] = if fp != 0 { fc + fp } else { bc + bp };
            }
        }
        self.draw_sprites(&mut pens);
        for (i, &p) in pens.iter().enumerate() {
            let c = self.color(p as usize);
            self.framebuffer[i * 4..i * 4 + 4].copy_from_slice(&c);
        }
    }

    fn draw_sprites(&self, pens: &mut [u16]) {
        let colbank = ((self.sprite_ctrl & 0xE0) >> 1) as u32;
        let flipscreen = self.sprite_ram[0xDFF] & 1 == 0;
        let count = self.sprites.len() / 128;
        let mut drawn = vec![false; pens.len()];
        for offs in (0..0x400).step_by(4) {
            let data = self.sprite_ram[offs];
            let mut flipy = data & 0x8000 != 0;
            let mut flipx = data & 0x4000 != 0;
            let color = (data as u32 & 0x0F) | colbank;
            let code = (self.sprite_ram[offs + 2] & 0x1FFF) as usize % count.max(1);
            let mut x = (self.sprite_ram[offs + 3] & 0x1FF) as i32;
            let mut y = (self.sprite_ram[offs + 1] & 0x1FF) as i32;
            if x > 0x140 { x -= 0x200; }
            if y > 0x140 { y -= 0x200; }
            if flipscreen {
                x = 320 - x - 16;
                y = 256 - y - 16;
                flipx = !flipx;
                flipy = !flipy;
            }
            for row in 0..16 {
                let sy = y + row - FIRST_LINE as i32;
                if sy < 0 || sy >= SCREEN_H as i32 { continue; }
                let src_row = if flipy { 15 - row } else { row } as usize;
                for col in 0..16 {
                    let sx = x + col;
                    if sx < 0 || sx >= SCREEN_W as i32 { continue; }
                    let src_col = if flipx { 15 - col } else { col } as usize;
                    let b = self.sprites[code * 128 + src_row * 8 + src_col / 2];
                    let pen = if src_col & 1 == 0 { b >> 4 } else { b & 15 };
                    if pen == 0 { continue; }
                    let i = sy as usize * SCREEN_W + sx as usize;
                    if drawn[i] { continue; }     // earlier sprites win
                    drawn[i] = true;
                    pens[i] = (color * 16 + pen as u32) as u16;
                }
            }
        }
    }
}
