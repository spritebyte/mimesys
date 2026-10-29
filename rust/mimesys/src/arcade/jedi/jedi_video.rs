//! Return of the Jedi video. After MAME jedi.cpp (Dan Boris, Aaron Giles).
//!
//! There's no priority logic: each layer supplies some address bits of a 1024-entry
//! palette and the game fills the palette so the result looks layered.
//! - Text (64x30 8x8 chars, 2bpp): pen bits 8-9.
//! - Background (32x32 tiles, 8x8 pixels drawn 2x, 4bpp, scrolling): pen bits 0-3,
//!   smoothed through two PROMs: one blends with the pixel to the left, the other with
//!   the pixel above.
//! - Sprites (48 of them, 8x16 or 8x32, 4bpp): pen bits 4-7 where non-zero.
//!
//! Palette entries are 12 bits (2800 low byte, 2C00 high nibble): IRGB 3:3:3:3, each
//! gun = 5 x colour x intensity.

use serde::{Deserialize, Serialize};

pub const SCREEN_W: usize = 296;
pub const SCREEN_H: usize = 240;

#[derive(Clone, Serialize, Deserialize)]
pub struct JediVideo {
    pub background: Vec<u8>,   // 2000-27FF
    pub palette_lo: Vec<u8>,   // 2800-2BFF
    pub palette_hi: Vec<u8>,   // 2C00-2FFF
    pub foreground: Vec<u8>,   // 3000-37BF
    pub sprites: Vec<u8>,      // 37C0-3BFF
    pub vscroll: u16,
    pub hscroll: u16,
    pub smoothing: u8,
    pub foreground_bank: bool,
    pub video_off: bool,
    /// background pixels of the previous line (RAM at 2A)
    line_buffer: Vec<u8>,
    #[serde(skip)] tx_gfx: Vec<u8>,
    #[serde(skip)] bg_gfx: Vec<u8>,
    #[serde(skip)] spr_gfx: Vec<u8>,
    #[serde(skip)] proms: Vec<u8>,
    #[serde(skip)] pub framebuffer: Vec<u8>,
}

impl JediVideo {
    pub fn new(tx_gfx: Vec<u8>, bg_gfx: Vec<u8>, spr_gfx: Vec<u8>, proms: Vec<u8>) -> Result<Self, String> {
        if tx_gfx.len() < 0x2000 || bg_gfx.len() < 0x10000 || spr_gfx.len() < 0x20000 || proms.len() < 0x1000 {
            return Err("Jedi graphics regions too small".into());
        }
        Ok(Self {
            background: vec![0; 0x800],
            palette_lo: vec![0; 0x400],
            palette_hi: vec![0; 0x400],
            foreground: vec![0; 0x7C0],
            sprites: vec![0; 0x440],
            vscroll: 0, hscroll: 0, smoothing: 0, foreground_bank: false, video_off: false,
            line_buffer: vec![0; 0x200],
            tx_gfx, bg_gfx, spr_gfx, proms,
            framebuffer: vec![0; SCREEN_W * SCREEN_H * 4],
        })
    }

    /// Keep the ROMs and framebuffer when loading a state.
    pub fn keep_roms(&mut self, old: &mut JediVideo) {
        self.tx_gfx = std::mem::take(&mut old.tx_gfx);
        self.bg_gfx = std::mem::take(&mut old.bg_gfx);
        self.spr_gfx = std::mem::take(&mut old.spr_gfx);
        self.proms = std::mem::take(&mut old.proms);
        self.framebuffer = std::mem::take(&mut old.framebuffer);
    }

    fn color(&self, pen: u16) -> [u8; 4] {
        let raw = self.palette_lo[pen as usize] as u32 | (self.palette_hi[pen as usize] as u32 & 0x0F) << 8;
        let i = (raw >> 9) & 7;
        let c = |bits: u32| (5 * bits * i) as u8;
        [c((raw >> 6) & 7), c((raw >> 3) & 7), c(raw & 7), 255]
    }

    pub fn render_line(&mut self, y: usize) {
        if y == 0 { self.line_buffer.fill(0); }
        let out_row = y * SCREEN_W * 4;
        if self.video_off {
            self.framebuffer[out_row..out_row + SCREEN_W * 4].chunks_mut(4).for_each(|p| p.copy_from_slice(&[0, 0, 0, 255]));
            return;
        }
        let mut pens = [0u16; SCREEN_W];

        // background + text
        let page = ((self.smoothing & 3) as usize) << 8;
        let mut bg_last = 0u8;
        for x in (0..SCREEN_W).step_by(2) {
            let sy = y + self.vscroll as usize;
            let mut sx = x + self.hscroll as usize;
            let tx_offs = ((y & 0xF8) << 3) | (x >> 3);
            let bg_offs = ((sy & 0x1F0) << 1) | ((sx & 0x1F0) >> 4);
            let tx_code = (self.foreground_bank as usize) << 8 | self.foreground[tx_offs] as usize;
            let bank = self.background[0x400 | bg_offs] as usize;
            let bg_code = self.background[bg_offs] as usize | (bank & 1) << 8 | (bank & 8) << 6 | (bank & 2) << 9;
            if bank & 4 != 0 { sx ^= 0x0F; }       // flip X

            let tx_data = self.tx_gfx[(tx_code << 4) | ((y & 7) << 1) | ((x & 4) >> 2)];
            let g = (bg_code << 4) | (sy & 0x0E) | ((sx & 8) >> 3);
            let (d1, d2) = (self.bg_gfx[g], self.bg_gfx[0x8000 | g]);

            let (tx1, tx2) = if x & 2 != 0 {
                (((tx_data & 0x0C) as u16) << 6, ((tx_data & 0x03) as u16) << 8)
            } else {
                (((tx_data & 0xC0) as u16) << 2, ((tx_data & 0x30) as u16) << 4)
            };
            let sh = 3 - ((sx & 6) >> 1);           // which pixel of the 4 in each byte
            let bg = ((d1 >> (4 + sh)) & 1) << 3 | ((d1 >> sh) & 1) << 2 | ((d2 >> (4 + sh)) & 1) << 1 | ((d2 >> sh) & 1);

            // horizontal smoothing on the first pixel, then vertical on both
            let tmp = self.proms[page | (bg_last as usize) << 4 | bg as usize];
            let p2 = 0x800 | page;
            pens[x] = tx1 | self.proms[p2 | (self.line_buffer[x] as usize) << 4 | tmp as usize] as u16;
            pens[x + 1] = tx2 | self.proms[p2 | (self.line_buffer[x + 1] as usize) << 4 | bg as usize] as u16;
            self.line_buffer[x] = tmp;
            self.line_buffer[x + 1] = bg;
            bg_last = bg;
        }

        // sprites
        let s = &self.sprites;
        for offs in 0..0x30 {
            let flags = s[offs + 0x40];
            let (flip_x, flip_y, tall) = (flags & 0x10 != 0, flags & 0x20 != 0, flags & 0x08 != 0);
            let mut code = s[offs] as usize | ((flags & 0x04) as usize) << 8 | ((flags & 0x40) as usize) << 3 | ((flags & 0x02) as usize) << 7;
            let mut top = (240u8).wrapping_sub(s[offs + 0x80]).wrapping_add(1);
            let size: u8 = if tall { code &= !1; top = top.wrapping_sub(0x10); 0x20 } else { 0x10 };
            // the row of this sprite on line y
            let row = if flip_y {
                top.wrapping_add(size - 1).wrapping_sub(y as u8)
            } else {
                (y as u8).wrapping_sub(top)
            };
            if row >= size { continue; }
            let gfx = (code << 5) + row as usize * 2;
            let mut x = (s[offs + 0x100] as u16 + (((flags & 1) as u16) << 8)).wrapping_sub(2);
            if flip_x { x = x.wrapping_add(7); }
            for i in 0..2 {
                let (mut d1, mut d2) = (self.spr_gfx[gfx + i], self.spr_gfx[0x10000 + gfx + i]);
                for _ in 0..4 {
                    let col = (d1 & 0x80) as u16 | ((d1 & 0x08) as u16) << 3 | ((d2 & 0x80) as u16) >> 2 | ((d2 & 0x08) as u16) << 1;
                    x &= 0x1FF;
                    if col != 0 && (x as usize) < SCREEN_W {
                        pens[x as usize] = (pens[x as usize] & 0x30F) | col;
                    }
                    x = if flip_x { x.wrapping_sub(1) } else { x.wrapping_add(1) };
                    d1 <<= 1;
                    d2 <<= 1;
                }
            }
        }

        for (x, &pen) in pens.iter().enumerate() {
            let c = self.color(pen);
            self.framebuffer[out_row + x * 4..out_row + x * 4 + 4].copy_from_slice(&c);
        }
    }
}
