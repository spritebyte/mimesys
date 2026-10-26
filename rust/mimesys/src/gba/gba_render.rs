//! GBA scanline renderer (called by the PPU at the start of HBlank on lines 0-159).
//!
//! Each line is drawn in three passes:
//!   1. backgrounds into their own line buffers: text (tile) layers in modes 0/1,
//!      affine (rotate/scale) layers in modes 1/2, the bitmap layer in modes 3/4/5
//!   2. sprites into one OBJ line buffer (colour, priority, semi-transparent, OBJ window)
//!   3. per pixel: the window picks which layers and effects are allowed, the two
//!      front-most layers are found by priority, then alpha blending or fading
//!
//! Colours are BGR555; `TRANSPARENT` (bit 15) marks an empty pixel.

use crate::gba::gba_ppu::{rgb555, Ppu, SCREEN_H, SCREEN_W};

const W: usize = SCREEN_W;
const TRANSPARENT: u16 = 0x8000;
/// layer ids used by BLDCNT: 0-3 backgrounds, 4 OBJ, 5 backdrop
const L_OBJ: usize = 4;
const L_BACKDROP: usize = 5;

/// OBJ sizes in pixels, [shape][size] (square, wide, tall)
const OBJ_SIZES: [[(i32, i32); 4]; 3] = [
    [(8, 8), (16, 16), (32, 32), (64, 64)],
    [(16, 8), (32, 8), (32, 16), (64, 32)],
    [(8, 16), (8, 32), (16, 32), (32, 64)],
];

#[derive(Clone, Copy, PartialEq)]
enum BgKind { Off, Text, Affine, Bitmap }

struct ObjLine {
    color: [u16; W],
    prio: [u8; W],
    semi: [bool; W],
    win: [bool; W],
}

impl ObjLine {
    fn new() -> Self { Self { color: [TRANSPARENT; W], prio: [4; W], semi: [false; W], win: [false; W] } }
}

impl Ppu {
    fn vram16(&self, a: usize) -> u16 { u16::from_le_bytes([self.vram[a], self.vram[a + 1]]) }
    fn oam16(&self, a: usize) -> u16 { u16::from_le_bytes([self.oam[a], self.oam[a + 1]]) }
    /// palette entry (0-255 backgrounds, 256-511 sprites)
    fn pal(&self, i: usize) -> u16 { u16::from_le_bytes([self.palette[i * 2], self.palette[i * 2 + 1]]) & 0x7FFF }

    /// MOSAIC sizes: (bg h, bg v, obj h, obj v), each 1-16
    fn mosaic(&self) -> (usize, usize, usize, usize) {
        let m = self.reg16(0x4C) as usize;
        ((m & 15) + 1, ((m >> 4) & 15) + 1, ((m >> 8) & 15) + 1, ((m >> 12) & 15) + 1)
    }

    /// Draws visible line `y` into the framebuffer, then steps the affine reference points.
    pub fn render_line(&mut self, y: usize) {
        if y >= SCREEN_H { return; }
        let dispcnt = self.dispcnt();
        // affine layers with vertical mosaic keep the reference point of the block's top line
        let (_, mv, _, _) = self.mosaic();
        if y % mv == 0 { self.mosaic_ref = self.bg_ref; }
        let mut out = [0x7FFFu16; W];
        if dispcnt & 0x80 == 0 {
            self.compose_line(y, dispcnt, &mut out);
        }
        // "green swap" (undocumented): swaps green between each pair of pixels
        if self.regs[2] & 1 != 0 {
            for x in (0..W).step_by(2) {
                let (a, b) = (out[x], out[x + 1]);
                out[x] = (a & !0x3E0) | (b & 0x3E0);
                out[x + 1] = (b & !0x3E0) | (a & 0x3E0);
            }
        }
        let row = &mut self.framebuffer[y * W * 4..(y + 1) * W * 4];
        for (x, c) in out.iter().enumerate() { row[x * 4..x * 4 + 4].copy_from_slice(&rgb555(*c)); }
        // BG2/BG3 reference point moves by (PB, PD) every line
        for i in 0..2 {
            let base = 0x20 + i * 0x10;
            self.bg_ref[i][0] = self.bg_ref[i][0].wrapping_add(self.reg16(base + 2) as i16 as i32);
            self.bg_ref[i][1] = self.bg_ref[i][1].wrapping_add(self.reg16(base + 6) as i16 as i32);
        }
    }

    fn compose_line(&self, y: usize, dispcnt: u16, out: &mut [u16; W]) {
        use BgKind::*;
        let mode = dispcnt & 7;
        let kinds = match mode {
            0 => [Text, Text, Text, Text],
            1 => [Text, Text, Affine, Off],
            2 => [Off, Off, Affine, Affine],
            3..=5 => [Off, Off, Bitmap, Off],
            _ => [Off; 4],
        };
        let mut bg = [[TRANSPARENT; W]; 4];
        let mut on = [false; 4];
        for i in 0..4 {
            if dispcnt & (0x100 << i) == 0 || kinds[i] == Off { continue; }
            on[i] = true;
            match kinds[i] {
                Text => self.text_bg(i, y, &mut bg[i]),
                Affine => self.affine_bg(i, &mut bg[i]),
                Bitmap => self.bitmap_bg(mode, &mut bg[i]),
                Off => {}
            }
        }
        let mut obj = ObjLine::new();
        if dispcnt & 0x1000 != 0 { self.render_objs(y, dispcnt, &mut obj); }

        // backgrounds front to back: by priority, then by number
        let mut order: Vec<usize> = (0..4).filter(|&i| on[i]).collect();
        order.sort_by_key(|&i| (self.reg16(8 + i * 2) & 3, i));
        let bg_prio = |i: usize| (self.reg16(8 + i * 2) & 3) as u8;

        // windows
        let any_win = dispcnt & 0xE000 != 0;
        let winin = self.reg16(0x48);
        let winout = self.reg16(0x4A);
        let win0 = dispcnt & 0x2000 != 0 && self.win_in(0x44, y, SCREEN_H);
        let win1 = dispcnt & 0x4000 != 0 && self.win_in(0x46, y, SCREEN_H);
        let objwin = dispcnt & 0x8000 != 0;

        // blending
        let bldcnt = self.reg16(0x50);
        let bmode = (bldcnt >> 6) & 3;
        let bldalpha = self.reg16(0x52);
        let eva = (bldalpha & 31).min(16) as u32;
        let evb = ((bldalpha >> 8) & 31).min(16) as u32;
        let evy = (self.reg16(0x54) & 31).min(16) as u32;
        let backdrop = self.pal(0);

        for x in 0..W {
            let ctrl = if !any_win {
                0x3F
            } else if win0 && self.win_in(0x40, x, W) {
                winin & 0x3F
            } else if win1 && self.win_in(0x42, x, W) {
                (winin >> 8) & 0x3F
            } else if objwin && obj.win[x] {
                (winout >> 8) & 0x3F
            } else {
                winout & 0x3F
            };

            // the two front-most visible layers (backdrop behind everything)
            let mut layers = [(L_BACKDROP, backdrop); 2];
            let mut found = 0;
            let obj_here = ctrl & 0x10 != 0 && obj.color[x] != TRANSPARENT;
            'p: for p in 0..4u8 {
                if obj_here && obj.prio[x] == p {
                    layers[found] = (L_OBJ, obj.color[x]);
                    found += 1;
                    if found == 2 { break 'p; }
                }
                for &i in &order {
                    if bg_prio(i) == p && ctrl & (1 << i) != 0 && bg[i][x] != TRANSPARENT {
                        layers[found] = (i, bg[i][x]);
                        found += 1;
                        if found == 2 { break 'p; }
                    }
                }
            }
            let [(top, c1), (second, c2)] = layers;

            let effects = ctrl & 0x20 != 0;
            let second_target = bldcnt & (0x100 << second) != 0;
            let mut c = c1;
            if top == L_OBJ && obj.semi[x] && second_target {
                // semi-transparent sprites blend whatever BLDCNT's mode is
                c = blend(c1, c2, eva, evb);
            } else if effects && bldcnt & (1 << top) != 0 {
                c = match bmode {
                    1 if second_target => blend(c1, c2, eva, evb),
                    2 => fade(c1, evy, true),
                    3 => fade(c1, evy, false),
                    _ => c1,
                };
            }
            out[x] = c;
        }
    }

    /// Window range check for WINxH (horizontal) or WINxV (vertical) at register `off`:
    /// high byte = start, low byte = end (exclusive); start > end wraps around.
    fn win_in(&self, off: usize, p: usize, limit: usize) -> bool {
        let r = self.reg16(off);
        let (a, mut b) = ((r >> 8) as usize, (r & 0xFF) as usize);
        if a <= b {
            if b > limit { b = limit; }
            p >= a && p < b
        } else {
            p >= a || p < b
        }
    }

    /// Text (tiled, scrolling) background.
    fn text_bg(&self, i: usize, y: usize, line: &mut [u16; W]) {
        let cnt = self.reg16(8 + i * 2);
        let hofs = (self.reg16(0x10 + i * 4) & 0x1FF) as usize;
        let vofs = (self.reg16(0x12 + i * 4) & 0x1FF) as usize;
        let char_base = ((cnt >> 2) & 3) as usize * 0x4000;
        let screen_base = ((cnt >> 8) & 31) as usize * 0x800;
        let c256 = cnt & 0x80 != 0;
        let size = cnt >> 14;
        let wide = size & 1 != 0;
        let wmask = if wide { 511 } else { 255 };
        let hmask = if size & 2 != 0 { 511 } else { 255 };
        let mosaic = cnt & 0x40 != 0;
        let (mh, mv, _, _) = self.mosaic();
        let my = if mosaic { y - y % mv } else { y };
        let py = (my + vofs) & hmask;

        for (x, px_out) in line.iter_mut().enumerate() {
            let mx = if mosaic { x - x % mh } else { x };
            let px = (mx + hofs) & wmask;
            let block = (px >> 8) + (py >> 8) * if wide { 2 } else { 1 };
            let ea = screen_base + block * 0x800 + ((py >> 3) & 31) * 64 + ((px >> 3) & 31) * 2;
            if ea >= 0x10000 { continue; }
            let e = self.vram16(ea) as usize;
            let tile = e & 0x3FF;
            let tx = if e & 0x400 != 0 { 7 - (px & 7) } else { px & 7 };
            let ty = if e & 0x800 != 0 { 7 - (py & 7) } else { py & 7 };
            let color = if c256 {
                let a = char_base + tile * 64 + ty * 8 + tx;
                if a >= 0x10000 { continue; }
                let idx = self.vram[a] as usize;
                if idx == 0 { continue; }
                self.pal(idx)
            } else {
                let a = char_base + tile * 32 + ty * 4 + tx / 2;
                if a >= 0x10000 { continue; }
                let idx = (self.vram[a] >> ((tx & 1) * 4)) as usize & 15;
                if idx == 0 { continue; }
                self.pal((e >> 12) * 16 + idx)
            };
            *px_out = color;
        }
    }

    /// Texture coordinates for screen x on BG2 (`ri` 0) or BG3 (`ri` 1): 8.8 fixed point
    /// steps from the internal reference point.
    fn affine_coords(&self, ri: usize, x: usize) -> (i32, i32) {
        let base = 0x20 + ri * 0x10;
        let pa = self.reg16(base) as i16 as i32;
        let pc = self.reg16(base + 4) as i16 as i32;
        // vertical mosaic repeats the first line of each block
        let mosaic = self.reg16(8 + (ri + 2) * 2) & 0x40 != 0;
        let [rx, ry] = if mosaic { self.mosaic_ref[ri] } else { self.bg_ref[ri] };
        (rx.wrapping_add(pa * x as i32) >> 8, ry.wrapping_add(pc * x as i32) >> 8)
    }

    /// Affine (rotate/scale) tiled background: 8-bit map entries, 256-colour tiles.
    fn affine_bg(&self, i: usize, line: &mut [u16; W]) {
        let cnt = self.reg16(8 + i * 2);
        let char_base = ((cnt >> 2) & 3) as usize * 0x4000;
        let screen_base = ((cnt >> 8) & 31) as usize * 0x800;
        let size = 128i32 << (cnt >> 14);
        let wrap = cnt & 0x2000 != 0;
        let mosaic = cnt & 0x40 != 0;
        let (mh, _, _, _) = self.mosaic();
        for (x, px_out) in line.iter_mut().enumerate() {
            let mx = if mosaic { x - x % mh } else { x };
            let (mut tx, mut ty) = self.affine_coords(i - 2, mx);
            if wrap {
                tx &= size - 1;
                ty &= size - 1;
            } else if tx < 0 || ty < 0 || tx >= size || ty >= size {
                continue;
            }
            let ma = screen_base + (ty as usize / 8) * (size as usize / 8) + tx as usize / 8;
            if ma >= 0x10000 { continue; }
            let tile = self.vram[ma] as usize;
            let a = char_base + tile * 64 + (ty as usize & 7) * 8 + (tx as usize & 7);
            if a >= 0x10000 { continue; }
            let idx = self.vram[a] as usize;
            if idx != 0 { *px_out = self.pal(idx); }
        }
    }

    /// BG2 in the bitmap modes: 3 = 240x160 direct colour, 4 = 240x160 paletted (two
    /// pages), 5 = 160x128 direct colour (two pages). Uses the BG2 affine transform.
    fn bitmap_bg(&self, mode: u16, line: &mut [u16; W]) {
        let page = if self.dispcnt() & 0x10 != 0 { 0xA000 } else { 0 };
        let cnt = self.reg16(0x0C);
        let mosaic = cnt & 0x40 != 0;
        let (mh, _, _, _) = self.mosaic();
        let (w, h) = if mode == 5 { (160, 128) } else { (240, 160) };
        for (x, px_out) in line.iter_mut().enumerate() {
            let mx = if mosaic { x - x % mh } else { x };
            let (tx, ty) = self.affine_coords(0, mx);
            if tx < 0 || ty < 0 || tx >= w || ty >= h { continue; }
            let p = (ty * w + tx) as usize;
            *px_out = match mode {
                3 => self.vram16(p * 2) & 0x7FFF,
                4 => {
                    let idx = self.vram[page + p] as usize;
                    if idx == 0 { continue; }
                    self.pal(idx)
                }
                _ => self.vram16(page + p * 2) & 0x7FFF,
            };
        }
    }

    /// All 128 sprites for line `y` into the OBJ line buffer.
    fn render_objs(&self, y: usize, dispcnt: u16, obj: &mut ObjLine) {
        let one_d = dispcnt & 0x40 != 0;
        let bitmap_mode = (dispcnt & 7) >= 3;
        let (_, _, mh, mv) = self.mosaic();
        let y = y as i32;
        // the sprite unit has 1210 clocks a line (954 with "H-blank interval free");
        // a sprite on this line costs its width, or 10 + 2 x box width when affine
        let mut budget: i32 = if dispcnt & 0x20 != 0 { 954 } else { 1210 };

        for n in 0..128 {
            let a0 = self.oam16(n * 8);
            let a1 = self.oam16(n * 8 + 2);
            let a2 = self.oam16(n * 8 + 4);
            let affine = a0 & 0x100 != 0;
            if !affine && a0 & 0x200 != 0 { continue; }    // hidden
            let mode = (a0 >> 10) & 3;
            if mode == 3 { continue; }
            let shape = (a0 >> 14) as usize;
            if shape == 3 { continue; }
            let (w, h) = OBJ_SIZES[shape][(a1 >> 14) as usize];
            let double = affine && a0 & 0x200 != 0;
            let (bw, bh) = if double { (w * 2, h * 2) } else { (w, h) };

            let sy = (a0 & 0xFF) as i32;
            let mut rel_y = (y - sy) & 0xFF;               // Y wraps at 256
            if rel_y >= bh { continue; }
            let cost = if affine { 10 + 2 * bw } else { bw };
            if cost > budget { break; }
            budget -= cost;
            let mosaic = a0 & 0x1000 != 0;
            if mosaic {
                let r = ((y - y % mv as i32) - sy) & 0xFF;
                if r < bh { rel_y = r; }
            }
            let sx = { let v = (a1 & 0x1FF) as i32; if v >= 256 { v - 512 } else { v } };
            let c256 = a0 & 0x2000 != 0;
            let tile_base = (a2 & 0x3FF) as i32;
            if bitmap_mode && tile_base < 512 { continue; }   // that OBJ VRAM holds the bitmap
            let prio = ((a2 >> 10) & 3) as u8;
            let bank = (a2 >> 12) as usize;
            let (pa, pb, pc, pd) = if affine {
                let g = ((a1 >> 9) & 31) as usize * 32;
                (self.oam16(g + 6) as i16 as i32, self.oam16(g + 14) as i16 as i32,
                 self.oam16(g + 22) as i16 as i32, self.oam16(g + 30) as i16 as i32)
            } else {
                (0x100, 0, 0, 0x100)
            };

            for bx in 0..bw {
                let x = sx + bx;
                if !(0..W as i32).contains(&x) { continue; }
                let mut ox = bx;
                if mosaic { ox = (x - x % mh as i32 - sx).max(0); }
                let (tx, ty) = if affine {
                    let cx = ox - bw / 2;
                    let cy = rel_y - bh / 2;
                    (((pa * cx + pb * cy) >> 8) + w / 2, ((pc * cx + pd * cy) >> 8) + h / 2)
                } else {
                    (if a1 & 0x1000 != 0 { w - 1 - ox } else { ox },
                     if a1 & 0x2000 != 0 { h - 1 - rel_y } else { rel_y })
                };
                if tx < 0 || ty < 0 || tx >= w || ty >= h { continue; }

                let (tcol, trow) = (tx / 8, ty / 8);
                let tile = if c256 {
                    if one_d { tile_base + (trow * (w / 8) + tcol) * 2 } else { (tile_base & !1) + trow * 32 + tcol * 2 }
                } else if one_d {
                    tile_base + trow * (w / 8) + tcol
                } else {
                    tile_base + trow * 32 + tcol
                } as usize & 0x3FF;
                let (px, py) = ((tx & 7) as usize, (ty & 7) as usize);
                let color_idx = if c256 {
                    let idx = self.vram[0x10000 + ((tile * 32 + py * 8 + px) & 0x7FFF)] as usize;
                    if idx == 0 { continue; }
                    256 + idx
                } else {
                    let b = self.vram[0x10000 + ((tile * 32 + py * 4 + px / 2) & 0x7FFF)];
                    let idx = (b >> ((px & 1) * 4)) as usize & 15;
                    if idx == 0 { continue; }
                    256 + bank * 16 + idx
                };

                let x = x as usize;
                if mode == 2 {
                    obj.win[x] = true;                      // OBJ window: shape only
                } else if obj.color[x] == TRANSPARENT || prio < obj.prio[x] {
                    obj.color[x] = self.pal(color_idx);
                    obj.prio[x] = prio;
                    obj.semi[x] = mode == 1;
                }
            }
        }
    }
}

/// Alpha blend: a*eva/16 + b*evb/16 per channel, capped at 31.
fn blend(a: u16, b: u16, eva: u32, evb: u32) -> u16 {
    let mut out = 0u16;
    for s in [0, 5, 10] {
        let ca = (a >> s) as u32 & 31;
        let cb = (b >> s) as u32 & 31;
        out |= (((ca * eva + cb * evb) >> 4).min(31) as u16) << s;
    }
    out
}

/// Brightness increase (towards white) or decrease (towards black) by evy/16.
fn fade(c: u16, evy: u32, up: bool) -> u16 {
    let mut out = 0u16;
    for s in [0, 5, 10] {
        let v = (c >> s) as u32 & 31;
        let v = if up { v + (((31 - v) * evy) >> 4) } else { v - ((v * evy) >> 4) };
        out |= (v as u16) << s;
    }
    out
}
