//! dec0 video: three BAC06 tilemap chips, the MXC06 sprite chip and the priority mix of
//! Robocop. Ported from MAME's `decbac06.cpp` (tile shapes, row / column scroll),
//! `decmxc06.cpp` (sprite lists) and `dec0_v.cpp` (`screen_update_robocop`,
//! `robocop_colpri_cb`), all BSD-3-Clause.
//!
//! - BAC06 control word 0: bit 0 = 8x8 tiles (else 16x16), bit 1 = row-major (clear:
//!   column-major, tiles flipped in x and x scroll inverted), bit 2 row scroll, bit 3
//!   column scroll, bit 7 flip screen (chip 0's is the screen's). Word 3 bits 0-1 = shape:
//!   0 wide, 1 square, 2 tall (3 = 1). Scroll words: x, y, column-scroll shift, row-scroll
//!   shift. Tile word: colour 15-12, code 11-0; pen 0 see-through.
//! - Colours: text (chars) 0-255, sprites 256-511, tiles1 512-767, tiles2 768-1023.
//! - Robocop's mix: priority bit 0 picks which of chip 1 / 2 is the (opaque) background;
//!   then the foreground, sprites, and the text on top. With priority bit 1 set, sprites
//!   whose colour bit 3 equals priority bit 2 (as 8 / 0) go behind the foreground.

use super::dec0_system::{Bac06, Dec0Board, SCREEN_H, SCREEN_W};
use crate::arcade::gng::gng_system::decode;

/// The first visible line of the 272-line frame (tile and sprite y counts from line 0)
const TOP: u32 = 8;

pub struct Gfx {
    pub chars: Vec<u8>,
    pub tiles1: Vec<u8>,
    pub tiles2: Vec<u8>,
    pub sprites: Vec<u8>,
    n_chars: usize,
    n_tiles1: usize,
    n_tiles2: usize,
    n_sprites: usize,
}

impl Gfx {
    /// MAME's charlayout / tilelayout: four planes, one in each quarter of the region
    pub fn new(chars: &[u8], tiles1: &[u8], tiles2: &[u8], sprites: &[u8]) -> Self {
        let char8 = |r: &[u8]| {
            let q = r.len() / 4 * 8;
            let n = q / 64;
            let xs: Vec<usize> = (0..8).collect();
            let ys: Vec<usize> = (0..8).map(|y| y * 8).collect();
            (decode(r, n, &[0, 2 * q, q, 3 * q], &xs, &ys, 64), n)
        };
        let tile16 = |r: &[u8]| {
            let q = r.len() / 4 * 8;
            let n = q / 256;
            let xs: Vec<usize> = (128..136).chain(0..8).collect();
            let ys: Vec<usize> = (0..16).map(|y| y * 8).collect();
            (decode(r, n, &[q, 3 * q, 0, 2 * q], &xs, &ys, 256), n)
        };
        let (chars, n_chars) = char8(chars);
        let (tiles1, n_tiles1) = tile16(tiles1);
        let (tiles2, n_tiles2) = tile16(tiles2);
        let (sprites, n_sprites) = tile16(sprites);
        Self { chars, tiles1, tiles2, sprites, n_chars, n_tiles1, n_tiles2, n_sprites }
    }

    fn pen8(&self, code: usize, x: usize, y: usize) -> u8 {
        if self.n_chars == 0 { return 0; }
        self.chars[((code % self.n_chars) * 8 + y) * 8 + x]
    }

    fn pen16(set: &[u8], n: usize, code: usize, x: usize, y: usize) -> u8 {
        if n == 0 { return 0; }
        set[((code % n) * 16 + y) * 16 + x]
    }
}

fn word(v: &[u8], i: usize) -> u16 { (v[(2 * i) % v.len()] as u16) << 8 | v[(2 * i + 1) % v.len()] as u16 }

/// One BAC06 with the graphics it shows (8x8 chars, or a 16x16 set) and its colour base
struct Layer<'a> {
    chip: &'a Bac06,
    gfx: &'a Gfx,
    /// which 16x16 set: 0 chars (unused), 1 tiles1, 2 tiles2
    set16: u8,
}

impl Layer<'_> {
    fn ctrl0(&self) -> u16 { self.chip.ctrl[0] }
    fn shape(&self) -> usize { match self.chip.ctrl[3] & 3 { 3 => 1, s => s as usize } }
    fn small(&self) -> bool { self.ctrl0() & 1 != 0 }

    /// The tilemap's size in dots
    fn dims(&self) -> (u32, u32) {
        let (c, r, t) = if self.small() {
            let (c, r) = [(128, 32), (64, 64), (32, 128)][self.shape()];
            (c, r, 8)
        } else {
            let (c, r) = [(64, 16), (32, 32), (16, 64)][self.shape()];
            (c, r, 16)
        };
        (c * t, r * t)
    }

    /// (pen, palette index) of the tilemap dot at (x, y)
    fn dot(&self, x: u32, y: u32) -> (u8, u16) {
        let row_major = self.ctrl0() & 2 != 0;
        let vram = &self.chip.vram;
        let words = vram.len() / 2;
        if self.small() {
            let (col, row) = (x / 8, y / 8);
            let i = match self.shape() {
                0 => (col & 0x1F) + ((row & 0x1F) << 5) + ((col & 0x60) << 5),
                1 => (col & 0x1F) + ((row & 0x1F) << 5) + ((row & 0x20) << 5) + ((col & 0x20) << 6),
                _ => (col & 0x1F) + ((row & 0x7F) << 5),
            } as usize % words;
            let t = word(vram, i);
            let mut px = (x & 7) as usize;
            if !row_major { px = 7 - px; }
            let pen = self.gfx.pen8((t & 0xFFF) as usize, px, (y & 7) as usize);
            (pen, (t >> 12) * 16 + pen as u16)
        } else {
            let (col, row) = (x / 16, y / 16);
            let i = match self.shape() {
                0 => if !row_major { (row & 0xF) + ((63 - (col & 63)) << 4) } else { (col & 0xF) + ((row & 0xF) << 4) + ((col & 0x1F0) << 4) },
                1 => (col & 0xF) + ((row & 0x1F) << 4) + ((col & 0xF0) << 5),
                _ => (col & 0xF) + ((row & 0x3F) << 4) + ((col & 0x70) << 6),
            } as usize % words;
            let t = word(vram, i);
            let mut px = (x & 15) as usize;
            if !row_major { px = 15 - px; }
            let code = (t & 0xFFF) as usize;
            let g = self.gfx;
            let (set, n, base) = match self.set16 {
                1 => (&g.tiles1, g.n_tiles1, 512),
                2 => (&g.tiles2, g.n_tiles2, 768),
                _ => return (0, 0),
            };
            let pen = Gfx::pen16(set, n, code, px, (y & 15) as usize);
            (pen, base + (t >> 12) * 16 + pen as u16)
        }
    }

    /// Draw over `color` / `pri`: `opaque` draws pen 0 too; drawn dots get `pri` or'ed in
    fn draw(&self, color: &mut [u16], prio: &mut [u8], opaque: bool, pri: u8) {
        let c0 = self.ctrl0();
        let s = &self.chip.scroll;
        let scrollx = if c0 & 2 == 0 { (s[0] as u32).wrapping_neg().wrapping_sub(0x100) } else { s[0] as u32 };
        let scrolly = s[1] as u32;
        let (w, h) = self.dims();
        if w == 0 || h == 0 { return; }
        let rows = c0 & 4 != 0;
        let cols = c0 & 8 != 0;
        let (rshift, cshift) = ((s[3] & 0xF) as u32, (s[2] & 0xF) as u32);
        for r in 0..SCREEN_H as u32 {
            let src_y = scrolly.wrapping_add(TOP + r);
            let mut src_x = scrollx;
            if rows {
                let i = ((src_y >> rshift) & (0x1FF >> rshift)) as usize;
                src_x = src_x.wrapping_add(word(&self.chip.rowscroll, i) as u32);
            }
            for x in 0..SCREEN_W as u32 {
                let sx = src_x.wrapping_add(x);
                let mut sy = src_y;
                if cols {
                    let i = (((sx >> 3) >> cshift) & (0x3F >> cshift)) as usize;
                    sy = sy.wrapping_add(word(&self.chip.colscroll, i) as u32);
                }
                let (pen, c) = self.dot(sx & (w - 1), sy & (h - 1));
                if opaque || pen != 0 {
                    let k = (r as usize) * SCREEN_W + x as usize;
                    color[k] = c;
                    prio[k] |= pri;
                }
            }
        }
    }
}

/// MAME's GFX_PMASK_2: hidden where the priority has bit 1 (the foreground drew there)
const PMASK_2: u32 = 0xCCCC;

/// Robocop's whole screen as palette indices (0-1023)
pub fn render(b: &Dec0Board, g: &Gfx, frame: u64, out: &mut [u16]) {
    let mut prio = vec![0u8; SCREEN_W * SCREEN_H];
    let layer = |n: usize| Layer { chip: &b.bac[n], gfx: g, set16: n as u8 };
    let (fg, bg) = if b.pri & 1 != 0 { (2, 1) } else { (1, 2) };
    layer(bg).draw(out, &mut prio, true, 1);
    layer(fg).draw(out, &mut prio, false, 2);
    sprites(b, g, frame, out, &mut prio);
    layer(0).draw(out, &mut prio, false, 0);
    // flip screen (the Flip Screen DIP sets it in chip 0): MAME flips every tilemap and
    // mirrors the sprites (x = 240 - x, y = 240 - y) about the 256 x 256 bitmap, whose
    // visible lines 8-247 swap with each other, so it's the finished frame turned 180
    if b.bac[0].ctrl[0] & 0x80 != 0 { out.reverse(); }
}

/// The MXC06 sprite list (buffered sprite RAM, 0x400 words, 4 per sprite)
fn sprites(b: &Dec0Board, g: &Gfx, frame: u64, out: &mut [u16], prio: &mut [u8]) {
    struct Part { code: u16, colour: u16, fx: bool, fy: bool, x: i32, y: i32, pmask: u32 }
    let ram = &b.sprite_buf;
    let w = |i: usize| word(ram, i);
    let size = ram.len() / 2;
    let mut list: Vec<Part> = Vec::new();
    let mut offs = 0;
    while offs < size {
        let (d0, d2) = (w(offs), w(offs + 2));
        let colour = d2 >> 12;
        // robocop_colpri_cb
        let mut pmask = 0;
        if b.pri & 2 != 0 {
            let trans = if b.pri & 4 != 0 { 8 } else { 0 };
            if colour & 8 == trans { pmask = PMASK_2; }
        }
        let flash = d2 & 0x800 != 0;
        let fx = d0 & 0x2000 != 0;
        let fy = d0 & 0x4000 != 0;
        let h = 1 << ((d0 & 0x1800) >> 11);
        let wd = 1 << ((d0 & 0x0600) >> 9);
        let mut sx = (d2 & 0x1FF) as i32;
        let mut sy = (d0 & 0x1FF) as i32;
        if sx >= 256 { sx -= 512; }
        if sy >= 256 { sy -= 512; }
        sx = 240 - sx;
        sy = 240 - sy;
        if d0 & 0x8000 == 0 { offs += 4; continue; }
        for x in 0..wd {
            if offs < size {
                let mut code = (w(offs + 1) & 0x1FFF) & !(h as u16 - 1);
                let incy: i32 = if fy { -1 } else { code += h as u16 - 1; 1 };
                // flashing sprites show on odd frames only
                if flash && frame & 1 == 0 { offs += 4; continue; }
                for y in 0..h {
                    list.push(Part {
                        code: (code as i32 - y as i32 * incy) as u16,
                        colour, fx, fy,
                        x: sx - 16 * x,
                        y: sy - 16 * y,
                        pmask,
                    });
                }
            }
            offs += 4;
        }
    }
    // drawn last-first; a dot taken by a sprite stays taken (MAME's priority 31)
    for p in list.iter().rev() {
        let pmask = p.pmask | 1 << 31;
        for py in 0..16 {
            let yy = p.y + py - TOP as i32;
            if !(0..SCREEN_H as i32).contains(&yy) { continue; }
            for px in 0..16 {
                let xx = p.x + px;
                if !(0..SCREEN_W as i32).contains(&xx) { continue; }
                let tx = if p.fx { 15 - px } else { px } as usize;
                let ty = if p.fy { 15 - py } else { py } as usize;
                let pen = Gfx::pen16(&g.sprites, g.n_sprites, p.code as usize, tx, ty);
                if pen == 0 { continue; }
                let k = yy as usize * SCREEN_W + xx as usize;
                if (pmask >> prio[k]) & 1 == 0 {
                    out[k] = 256 + p.colour * 16 + pen as u16;
                    prio[k] = 31;
                }
            }
        }
    }
}
