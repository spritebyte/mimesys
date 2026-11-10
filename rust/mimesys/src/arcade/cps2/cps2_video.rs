//! CPS2 video. After MAME capcom/cps2.cpp + cps1_v.cpp (BSD-3-Clause).
//!
//! The tilemaps, palette and CPS-A / CPS-B registers are CPS1's (cps1_video.rs, with the
//! "cps2" configuration: CPS-B-21 layout, all tiles in the second half of the ROM). What's
//! new is the sprite chip:
//! - 1024 sprites in object RAM (two 8K banks; 8040E0 bit 0 swaps them), latched at vblank
//!   with the layer priority register (400004).
//! - Each sprite has a 3-bit priority. The three tilemaps write 1 / 2 / 4 into a priority
//!   buffer as they're drawn; the sprite priority and the tilemaps' priority nibbles pick a
//!   mask of priority-buffer values that hide the sprite (MAME cps2 render_layers).
//! - Sprites are drawn from the end of the table back; a pixel is drawn once, so the
//!   later table entries end up in front.
//! - 400008 / 40000A: sprite X / Y offsets.
//!
//! The graphics ROMs are also stored shuffled (MAME unshuffle) and are put back in order at
//! load time.

use serde::{Deserialize, Serialize};

use crate::arcade::cps1::cps1_video::{LayerMode, Video, SCREEN_H, SCREEN_W, X0, Y0};

pub const OBJ_WORDS: usize = 0x1000;
/// priority-buffer value marking a pixel a sprite already drew (MAME's priority 31)
const DRAWN: u8 = 0x80;

/// MAME cps2_state::unshuffle, on 8-byte units.
pub(crate) fn unshuffle(buf: &mut [[u8; 8]]) {
    let len = buf.len();
    if len == 2 { return; }
    let half = len / 2;
    unshuffle(&mut buf[..half]);
    unshuffle(&mut buf[half..]);
    for i in 0..half / 2 {
        buf.swap(half / 2 + i, half + i);
    }
}

/// MAME cps2_gfx_decode: unshuffles each 2 MB bank of the graphics ROM.
pub fn gfx_decode(gfx: &mut [u8]) {
    for bank in gfx.chunks_mut(0x200000) {
        if bank.len() < 0x200000 { break; }
        let mut units: Vec<[u8; 8]> = bank.chunks(8).map(|c| c.try_into().unwrap()).collect();
        unshuffle(&mut units);
        for (i, u) in units.iter().enumerate() { bank[i * 8..i * 8 + 8].copy_from_slice(u); }
    }
}

/// MAME cps2 render_layers: for each sprite priority 0-7, the priority-buffer values
/// (bit n = value n; the three layers drawn add 1, 2 and 4) a sprite is hidden behind,
/// given the three layers' priority nibbles in draw order.
pub fn primasks(lp: [u16; 3]) -> [u8; 8] {
    let mut m = [0u8; 8];
    let mut mask0: u8 = 0xAA;
    let mut mask1: u8 = 0xCC;
    if lp[0] > lp[1] { mask0 &= !0x88; }
    if lp[0] > lp[2] { mask0 &= !0xA0; }
    if lp[1] > lp[2] { mask1 &= !0xC0; }
    m[0] = 0xFF;
    for i in 1..8u16 {
        if i <= lp[0] && i <= lp[1] && i <= lp[2] { m[i as usize] = 0xFE; continue; }
        let mut v = 0;
        if i <= lp[0] { v |= mask0; }
        if i <= lp[1] { v |= mask1; }
        if i <= lp[2] { v |= 0xF0; }
        m[i as usize] = v;
    }
    m
}

#[derive(Serialize, Deserialize)]
pub struct Cps2Video {
    pub base: Video,
    /// object RAM banks 0 / 1
    pub objram: [Vec<u16>; 2],
    pub objram_bank: u8,
    /// 400000-40000B: object base (unused), unknown, layer priorities, unknown, X / Y offsets
    pub output: [u16; 6],
    obj_buf: Vec<u16>,
    pri_ctrl: u16,
    /// screen lines already drawn this frame
    rendered: usize,
}

impl Cps2Video {
    pub fn new(base: Video) -> Self {
        Self {
            base,
            objram: [vec![0; OBJ_WORDS], vec![0; OBJ_WORDS]],
            objram_bank: 0,
            output: [0; 6],
            obj_buf: vec![0; OBJ_WORDS],
            pri_ctrl: 0,
            rendered: 0,
        }
    }

    pub fn reset(&mut self) {
        self.base.reset();
        for b in &mut self.objram { b.fill(0); }
        self.objram_bank = 0;
        self.output = [0; 6];
        self.obj_buf.fill(0);
        self.pri_ctrl = 0;
        self.rendered = 0;
    }

    pub fn keep_static(&mut self, old: &mut Cps2Video) { self.base.keep_static(&mut old.base); }

    /// Object RAM as seen at 700000 (`xor` 0) or 708000 (`xor` 1).
    pub fn obj_read(&self, xor: u8, word: usize) -> u16 { self.objram[((self.objram_bank ^ xor) & 1) as usize][word & 0xFFF] }

    pub fn obj_write(&mut self, xor: u8, word: usize, v: u16, mask: u16) {
        let b = &mut self.objram[((self.objram_bank ^ xor) & 1) as usize][word & 0xFFF];
        *b = (*b & !mask) | (v & mask);
    }

    /// Start of a frame (raster line 0).
    pub fn frame_start(&mut self) { self.rendered = 0; }

    /// Draws everything up to raster line `line` (exclusive) not drawn yet.
    pub fn render_to(&mut self, line: i32) {
        let end = (line - Y0).clamp(0, SCREEN_H as i32) as usize;
        if end > self.rendered {
            self.render_lines(self.rendered, end);
            self.rendered = end;
        }
    }

    /// Vblank: finish the frame, then latch the sprite table and the layer priorities.
    pub fn vblank(&mut self) {
        self.render_to(Y0 + SCREEN_H as i32);
        self.pri_ctrl = self.output[2];
        // MAME cps2_objbase: bank 1 when the bank bit is set
        self.obj_buf.copy_from_slice(&self.objram[(self.objram_bank & 1) as usize]);
        self.base.vblank();
    }

    /// MAME cps2_state::render_layers for screen lines y0..y1.
    fn render_lines(&mut self, y0: usize, y1: usize) {
        let v = &mut self.base;
        v.clear_lines(y0, y1);
        let ctrl = v.layer_control();
        let mut l = [(ctrl >> 6) & 3, (ctrl >> 8) & 3, (ctrl >> 10) & 3, (ctrl >> 12) & 3];
        let pri_ctrl = self.pri_ctrl;
        let pri_of = |layer: u16| (pri_ctrl >> (4 * layer)) & 0xF;
        let mut lp = [pri_of(l[0]), pri_of(l[1]), pri_of(l[2]), pri_of(l[3])];
        // take the sprite layer out of the order
        if l[0] == 0 { l[0] = l[1]; l[1] = 0; lp[0] = lp[1]; }
        if l[1] == 0 { l[1] = l[2]; l[2] = 0; lp[1] = lp[2]; }
        if l[2] == 0 { l[2] = l[3]; l[3] = 0; lp[2] = lp[3]; }

        let primasks = primasks([lp[0], lp[1], lp[2]]);

        let enabled = v.layer_enables();
        for (k, &layer) in l[..3].iter().enumerate() {
            if (1..=3).contains(&layer) && enabled[layer as usize - 1] {
                v.draw_scroll(layer as usize, LayerMode::Pri(1 << k), y0, y1);
            }
        }
        self.draw_sprites(&primasks, y0, y1);
        self.base.output_lines(y0, y1);
    }

    /// MAME find_last_sprite (CPS2): the table ends at a Y with bit 15 set or an
    /// attribute word of FF00 or more.
    fn last_sprite(&self) -> Option<usize> {
        for i in 0..OBJ_WORDS / 4 {
            if self.obj_buf[i * 4 + 1] >= 0x8000 || self.obj_buf[i * 4 + 3] >= 0xFF00 {
                return i.checked_sub(1);
            }
        }
        Some(OBJ_WORDS / 4 - 1)
    }

    fn draw_sprites(&mut self, primasks: &[u8; 8], y0: usize, y1: usize) {
        let Some(last) = self.last_sprite() else { return };
        let xoffs = 64 - self.output[4] as i32;
        let yoffs = 16 - self.output[5] as i32;
        for i in (0..=last).rev() {
            let e = &self.obj_buf[i * 4..i * 4 + 4];
            let (mut x, mut y, colour) = (e[0] as i32, e[1] as i32, e[3]);
            let pri = ((x >> 13) & 7) as usize;
            let code = e[2] as u32 + (((y as u32) & 0x6000) << 3);
            let col = colour & 0x1F;
            let flipx = colour & 0x20 != 0;
            let flipy = colour & 0x40 != 0;
            if colour & 0x80 != 0 {
                x += self.output[4] as i32;
                y += self.output[5] as i32;
            }
            let pmask = primasks[pri];
            let (nx, ny) = if colour & 0xFF00 != 0 { (((colour >> 8) & 0xF) as u32 + 1, ((colour >> 12) & 0xF) as u32 + 1) } else { (1, 1) };
            for nys in 0..ny {
                let sy = (y + nys as i32 * 16 + yoffs) & 0x3FF;
                for nxs in 0..nx {
                    let sx = (x + nxs as i32 * 16 + xoffs) & 0x3FF;
                    let c = if nx * ny == 1 {
                        code
                    } else {
                        let cy = if flipy { ny - 1 - nys } else { nys };
                        if flipx {
                            code + (nx - 1) - nxs + 0x10 * cy
                        } else if flipy {
                            code + nxs + 0x10 * cy
                        } else {
                            // MAME: "pgear fix, same as CPS1?" (wraps within 16 tiles)
                            (code & !0xF) + ((code + nxs) & 0xF) + 0x10 * cy
                        }
                    };
                    self.draw_tile(c, col, flipx, flipy, sx, sy, pmask, y0, y1);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_tile(&mut self, code: u32, col: u16, flipx: bool, flipy: bool, sx: i32, sy: i32, pmask: u8, y0: usize, y1: usize) {
        let v = &mut self.base;
        for py in 0..16 {
            let y = sy + py - Y0;
            if y < y0 as i32 || y >= y1 as i32 { continue; }
            for px in 0..16 {
                let x = sx + px - X0;
                if x < 0 || x >= SCREEN_W as i32 { continue; }
                let o = y as usize * SCREEN_W + x as usize;
                let p = v.prio[o];
                if p == DRAWN || (pmask >> (p & 7)) & 1 != 0 { continue; }
                let tx = if flipx { 15 - px } else { px } as usize;
                let ty = if flipy { 15 - py } else { py } as usize;
                let pen = v.tile_pen(code, 16, 0, tx, ty);
                if pen == 15 { continue; }
                v.pens[o] = col * 16 + pen as u16;
                v.prio[o] = DRAWN;
            }
        }
    }
}
