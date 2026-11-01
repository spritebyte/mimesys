//! The DS 2D engines. Engine A (registers 0x04000000) and engine B (0x04001000) are
//! GBA picture units with extras; this draws one 256-pixel line of either.
//!
//! DISPCNT (32 bits):
//!   0-2 BG mode, 3 BG0 is the 3D layer (A), 4 tile OBJ 1D, 5 bitmap OBJ 256 wide (2D),
//!   6 bitmap OBJ 1D, 7 forced blank, 8-12 BG0-3/OBJ on, 13-15 windows,
//!   16-17 display mode (0 white, 1 graphics, 2 VRAM (A), 3 main memory (A)),
//!   18-19 VRAM block (A), 20-21 tile OBJ 1D boundary (32 << n bytes),
//!   22 bitmap OBJ 1D boundary (A, 128 << n), 24-26 / 27-29 character / screen base in
//!   64 KB steps (A), 30 BG extended palettes, 31 OBJ extended palettes.
//!
//! BG modes (BG0, BG1, BG2, BG3): 0 text x4; 1 text x3 + affine; 2 text x2 + affine x2;
//! 3 text x3 + extended; 4 text x2 + affine + extended; 5 text x2 + extended x2;
//! 6 (A) 3D, -, large bitmap, -. An "extended" BG is, by BGxCNT bits 7 and 2: affine with
//! 16-bit map entries (flips, extended palettes), a 256-colour bitmap or a direct-colour
//! bitmap (bit 15 = opaque).
//!
//! Sprites add bitmap OBJs (mode 3: direct colour, attr 2 bits 12-15 = alpha) and the 1D
//! boundaries. BG0 can be the 3D picture (nds_gpu3d.rs), whose translucent pixels blend
//! with the layer behind when that's a second target.
//! Master brightness (0x6C) is applied by the caller after display capture.

use serde::{Deserialize, Serialize};
use crate::nds::nds_vram::{Region, Vram};

pub const W: usize = 256;
pub const H: usize = 192;
const TRANSPARENT: u16 = 0x8000;
/// layer ids used by BLDCNT: 0-3 backgrounds, 4 OBJ, 5 backdrop
const L_OBJ: usize = 4;
const L_BACKDROP: usize = 5;

const OBJ_SIZES: [[(i32, i32); 4]; 3] = [
    [(8, 8), (16, 16), (32, 32), (64, 64)],
    [(16, 8), (32, 8), (32, 16), (64, 32)],
    [(8, 16), (8, 32), (16, 32), (32, 64)],
];

/// Per-engine state that isn't a register: the affine reference points (20.8).
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Engine2d {
    pub bg_ref: [[i32; 2]; 2],
}

impl Engine2d {
    /// Copy BG2/BG3 X/Y (28-bit signed) into the internal reference points.
    pub fn reload(&mut self, regs: &[u8]) {
        for i in 0..2 {
            for k in 0..2 {
                let o = 0x28 + i * 0x10 + k * 4;
                let v = u32::from_le_bytes([regs[o], regs[o + 1], regs[o + 2], regs[o + 3]]);
                self.bg_ref[i][k] = ((v << 4) as i32) >> 4;
            }
        }
    }

    /// After a drawn line the reference points move by (PB, PD).
    pub fn step(&mut self, regs: &[u8]) {
        for i in 0..2 {
            let o = 0x20 + i * 0x10;
            self.bg_ref[i][0] = self.bg_ref[i][0].wrapping_add(i16::from_le_bytes([regs[o + 2], regs[o + 3]]) as i32);
            self.bg_ref[i][1] = self.bg_ref[i][1].wrapping_add(i16::from_le_bytes([regs[o + 6], regs[o + 7]]) as i32);
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum BgKind { Off, Text, Affine, Extended, Large, ThreeD }

struct ObjLine {
    color: [u16; W],
    prio: [u8; W],
    /// 16 = opaque, 0-15 = bitmap OBJ alpha, 17 = semi-transparent
    alpha: [u8; W],
    win: [bool; W],
}

impl ObjLine {
    fn new() -> Self { Self { color: [TRANSPARENT; W], prio: [4; W], alpha: [16; W], win: [false; W] } }
}

/// What one engine sees.
pub struct View<'a> {
    pub engine: usize,
    /// the engine's registers (0x70 bytes from 0x04000000 or 0x04001000)
    pub regs: &'a [u8],
    pub vram: &'a Vram,
    /// 0x400 bytes: BG palette, then OBJ palette
    pub pal: &'a [u8],
    /// 0x400 bytes
    pub oam: &'a [u8],
    pub refs: [[i32; 2]; 2],
    bg_pages: Vec<&'a [u8]>,
    obj_pages: Vec<&'a [u8]>,
    bg_mask: u32,
    obj_mask: u32,
    /// engine A: this line of the 3D picture (BGR555, alpha in bits 16-20)
    pub threed: Option<&'a [u32]>,
}

impl<'a> View<'a> {
    pub fn new(engine: usize, regs: &'a [u8], vram: &'a Vram, pal: &'a [u8], oam: &'a [u8], refs: [[i32; 2]; 2]) -> Self {
        let (bg, obj) = if engine == 0 { (Region::ABg, Region::AObj) } else { (Region::BBg, Region::BObj) };
        Self {
            engine, regs, vram, pal, oam, refs,
            bg_pages: vram.page_slices(bg), obj_pages: vram.page_slices(obj),
            bg_mask: if engine == 0 { 0x7FFFF } else { 0x1FFFF },
            obj_mask: if engine == 0 { 0x3FFFF } else { 0x1FFFF },
            threed: None,
        }
    }

    #[inline(always)]
    fn fetch(&self, pages: &[&[u8]], region: Region, a: u32) -> u8 {
        let p = pages[(a >> 14) as usize];
        if !p.is_empty() { p[(a & 0x3FFF) as usize] } else { self.vram.read8(region, a) }
    }

    fn reg16(&self, o: usize) -> u16 { u16::from_le_bytes([self.regs[o], self.regs[o + 1]]) }
    fn dispcnt(&self) -> u32 { u32::from_le_bytes([self.regs[0], self.regs[1], self.regs[2], self.regs[3]]) }
    fn a(&self) -> bool { self.engine == 0 }

    fn bg_region(&self) -> (Region, u32) { if self.a() { (Region::ABg, 0x7FFFF) } else { (Region::BBg, 0x1FFFF) } }
    fn obj_region(&self) -> (Region, u32) { if self.a() { (Region::AObj, 0x3FFFF) } else { (Region::BObj, 0x1FFFF) } }
    /// 8 BG bytes from an 8-aligned address (one page lookup).
    #[inline]
    fn bg_row(&self, a: usize) -> [u8; 8] {
        let a = a as u32 & self.bg_mask;
        let p = self.bg_pages[(a >> 14) as usize];
        if !p.is_empty() {
            let o = (a & 0x3FF8) as usize;
            p[o..o + 8].try_into().unwrap()
        } else {
            std::array::from_fn(|k| self.vram.read8(self.bg_region().0, a + k as u32))
        }
    }
    #[inline] fn bg8(&self, a: usize) -> u8 { self.fetch(&self.bg_pages, self.bg_region().0, a as u32 & self.bg_mask) }
    #[inline] fn bg16(&self, a: usize) -> u16 { let a = a & !1; self.bg8(a) as u16 | (self.bg8(a + 1) as u16) << 8 }
    #[inline] fn obj8(&self, a: usize) -> u8 { self.fetch(&self.obj_pages, self.obj_region().0, a as u32 & self.obj_mask) }
    #[inline] fn obj16(&self, a: usize) -> u16 { let a = a & !1; self.obj8(a) as u16 | (self.obj8(a + 1) as u16) << 8 }
    fn oam16(&self, a: usize) -> u16 { u16::from_le_bytes([self.oam[a], self.oam[a + 1]]) }
    fn bgpal(&self, i: usize) -> u16 { u16::from_le_bytes([self.pal[i * 2], self.pal[i * 2 + 1]]) & 0x7FFF }
    fn objpal(&self, i: usize) -> u16 { self.bgpal(256 + i) }
    fn ext_bg(&self, slot: usize, i: usize) -> u16 {
        let r = if self.a() { Region::ABgExt } else { Region::BBgExt };
        self.vram.read16(r, (slot * 0x2000 + i * 2) as u32) & 0x7FFF
    }
    fn ext_obj(&self, i: usize) -> u16 {
        let r = if self.a() { Region::AObjExt } else { Region::BObjExt };
        self.vram.read16(r, (i * 2) as u32) & 0x7FFF
    }

    /// DISPCNT character / screen base offsets (engine A only).
    fn bases(&self) -> (usize, usize) {
        if !self.a() { return (0, 0); }
        let d = self.dispcnt();
        (((d >> 24) & 7) as usize * 0x10000, ((d >> 27) & 7) as usize * 0x10000)
    }

    fn mosaic(&self) -> (usize, usize, usize, usize) {
        let m = self.reg16(0x4C) as usize;
        ((m & 15) + 1, ((m >> 4) & 15) + 1, ((m >> 8) & 15) + 1, ((m >> 12) & 15) + 1)
    }

    /// The graphics line `y` in BGR555 (forced blank = white).
    pub fn compose(&self, y: usize) -> [u16; W] {
        let mut out = [0x7FFFu16; W];
        let dispcnt = self.dispcnt();
        if dispcnt & 0x80 != 0 { return out; }
        use BgKind::*;
        let mode = dispcnt & 7;
        let mut kinds = match mode {
            0 => [Text, Text, Text, Text],
            1 => [Text, Text, Text, Affine],
            2 => [Text, Text, Affine, Affine],
            3 => [Text, Text, Text, Extended],
            4 => [Text, Text, Affine, Extended],
            5 => [Text, Text, Extended, Extended],
            6 if self.a() => [ThreeD, Off, Large, Off],
            _ => [Off; 4],
        };
        if self.a() && dispcnt & 8 != 0 { kinds[0] = ThreeD; }

        let mut bg = [[TRANSPARENT; W]; 4];
        let mut alpha3d = [31u8; W];
        let mut on = [false; 4];
        for i in 0..4 {
            if dispcnt & (0x100 << i) == 0 || kinds[i] == Off { continue; }
            on[i] = true;
            match kinds[i] {
                Text => self.text_bg(i, y, &mut bg[i]),
                Affine => self.affine_bg(i, &mut bg[i]),
                Extended => self.extended_bg(i, &mut bg[i]),
                Large => self.large_bg(&mut bg[i]),
                ThreeD => if let Some(l) = self.threed {
                    for x in 0..W {
                        let a = (l[x] >> 16) & 31;
                        if a != 0 { bg[0][x] = (l[x] & 0x7FFF) as u16; alpha3d[x] = a as u8; }
                    }
                },
                Off => {}
            }
        }
        let mut obj = ObjLine::new();
        if dispcnt & 0x1000 != 0 { self.render_objs(y, dispcnt, &mut obj); }

        let prio: [u8; 4] = std::array::from_fn(|i| (self.reg16(8 + i * 2) & 3) as u8);
        let mut order = [0usize; 4];
        let mut n_on = 0;
        for p in 0..4u8 { for i in 0..4 { if on[i] && prio[i] == p { order[n_on] = i; n_on += 1; } } }
        let order = &order[..n_on];

        let any_win = dispcnt & 0xE000 != 0;
        let winin = self.reg16(0x48);
        let winout = self.reg16(0x4A);
        let win0 = dispcnt & 0x2000 != 0 && self.win_in(0x44, y, H);
        let win1 = dispcnt & 0x4000 != 0 && self.win_in(0x46, y, H);
        let objwin = dispcnt & 0x8000 != 0;

        let bldcnt = self.reg16(0x50);
        let bmode = (bldcnt >> 6) & 3;
        let bldalpha = self.reg16(0x52);
        let eva = (bldalpha & 31).min(16) as u32;
        let evb = ((bldalpha >> 8) & 31).min(16) as u32;
        let evy = (self.reg16(0x54) & 31).min(16) as u32;
        let backdrop = self.bgpal(0);

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

            let mut layers = [(L_BACKDROP, backdrop); 2];
            let mut found = 0;
            let obj_here = ctrl & 0x10 != 0 && obj.color[x] != TRANSPARENT;
            // backgrounds come in priority order; a sprite goes before the first BG of
            // equal or lower priority
            let mut obj_done = !obj_here;
            for &i in order {
                if !obj_done && obj.prio[x] <= prio[i] {
                    obj_done = true;
                    layers[found] = (L_OBJ, obj.color[x]);
                    found += 1;
                    if found == 2 { break; }
                }
                if ctrl & (1 << i) != 0 && bg[i][x] != TRANSPARENT {
                    layers[found] = (i, bg[i][x]);
                    found += 1;
                    if found == 2 { break; }
                }
            }
            if !obj_done && found < 2 { layers[found] = (L_OBJ, obj.color[x]); }
            let [(top, c1), (second, c2)] = layers;

            let effects = ctrl & 0x20 != 0;
            let second_target = bldcnt & (0x100 << second) != 0;
            let mut c = c1;
            let oa = obj.alpha[x];
            if top == 0 && kinds[0] == ThreeD && second_target {
                // a 3D pixel over a second target always blends by its own alpha (31 = solid),
                // whatever BLDCNT's mode and EVA/EVB say
                let a = alpha3d[x] as u32;
                c = blend32(c1, c2, a + 1, 31 - a);
            } else if top == L_OBJ && oa != 16 && second_target {
                // semi-transparent sprites blend whatever BLDCNT's mode is; bitmap sprites
                // blend by their own alpha (out of 16)
                c = if oa == 17 { blend(c1, c2, eva, evb) } else { blend(c1, c2, oa as u32, 16 - oa as u32) };
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
        out
    }

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

    fn text_bg(&self, i: usize, y: usize, line: &mut [u16; W]) {
        let cnt = self.reg16(8 + i * 2);
        let hofs = (self.reg16(0x10 + i * 4) & 0x1FF) as usize;
        let vofs = (self.reg16(0x12 + i * 4) & 0x1FF) as usize;
        let (cb, sb) = self.bases();
        let char_base = cb + ((cnt >> 2) & 15) as usize * 0x4000;
        let screen_base = sb + ((cnt >> 8) & 31) as usize * 0x800;
        let c256 = cnt & 0x80 != 0;
        let ext = c256 && self.dispcnt() & (1 << 30) != 0;
        let slot = if i < 2 && cnt & 0x2000 != 0 { i + 2 } else { i };
        let size = cnt >> 14;
        let wide = size & 1 != 0;
        let wmask = if wide { 511 } else { 255 };
        let hmask = if size & 2 != 0 { 511 } else { 255 };
        let mosaic = cnt & 0x40 != 0;
        let (mh, mv, _, _) = self.mosaic();
        let my = if mosaic { y - y % mv } else { y };
        let py = (my + vofs) & hmask;

        if !mosaic {
            // a tile row at a time
            let unit = if c256 { 64 } else { 32 };
            let mut x = 0;
            while x < W {
                let px = (x + hofs) & wmask;
                let block = (px >> 8) + (py >> 8) * if wide { 2 } else { 1 };
                let e = self.bg16(screen_base + block * 0x800 + ((py >> 3) & 31) * 64 + ((px >> 3) & 31) * 2) as usize;
                let ty = if e & 0x800 != 0 { 7 - (py & 7) } else { py & 7 };
                let row = char_base + (e & 0x3FF) * unit + ty * (unit / 8);
                let mut idx = [0u8; 8];
                if c256 {
                    idx = self.bg_row(row);
                } else {
                    let r = self.bg_row(row & !7);
                    let o = row & 4;
                    for k in 0..4 { let b = r[o + k]; idx[k * 2] = b & 15; idx[k * 2 + 1] = b >> 4; }
                }
                if idx == [0; 8] { x += 8 - (px & 7); continue; }
                if e & 0x400 != 0 { idx.reverse(); }
                for &v in &idx[px & 7..] {
                    if x >= W { break; }
                    if v != 0 {
                        let v = v as usize;
                        line[x] = if !c256 { self.bgpal((e >> 12) * 16 + v) }
                                  else if ext { self.ext_bg(slot, (e >> 12) * 256 + v) } else { self.bgpal(v) };
                    }
                    x += 1;
                }
            }
            return;
        }
        let mut last_tile = usize::MAX;
        let mut e = 0usize;
        for (x, px_out) in line.iter_mut().enumerate() {
            let mx = if mosaic { x - x % mh } else { x };
            let px = (mx + hofs) & wmask;
            let block = (px >> 8) + (py >> 8) * if wide { 2 } else { 1 };
            let ea = screen_base + block * 0x800 + ((py >> 3) & 31) * 64 + ((px >> 3) & 31) * 2;
            if ea != last_tile { e = self.bg16(ea) as usize; last_tile = ea; }
            let tile = e & 0x3FF;
            let tx = if e & 0x400 != 0 { 7 - (px & 7) } else { px & 7 };
            let ty = if e & 0x800 != 0 { 7 - (py & 7) } else { py & 7 };
            *px_out = if c256 {
                let idx = self.bg8(char_base + tile * 64 + ty * 8 + tx) as usize;
                if idx == 0 { continue; }
                if ext { self.ext_bg(slot, (e >> 12) * 256 + idx) } else { self.bgpal(idx) }
            } else {
                let b = self.bg8(char_base + tile * 32 + ty * 4 + tx / 2);
                let idx = (b >> ((tx & 1) * 4)) as usize & 15;
                if idx == 0 { continue; }
                self.bgpal((e >> 12) * 16 + idx)
            };
        }
    }

    fn affine_coords(&self, i: usize, x: usize) -> (i32, i32) {
        let base = 0x20 + (i - 2) * 0x10;
        let pa = self.reg16(base) as i16 as i32;
        let pc = self.reg16(base + 4) as i16 as i32;
        let [rx, ry] = self.refs[i - 2];
        let cnt = self.reg16(8 + i * 2);
        let x = if cnt & 0x40 != 0 { let (mh, _, _, _) = self.mosaic(); x - x % mh } else { x };
        (rx.wrapping_add(pa * x as i32) >> 8, ry.wrapping_add(pc * x as i32) >> 8)
    }

    /// Wrap or clip texture coordinates to w x h; None = outside.
    fn fit(tx: i32, ty: i32, w: i32, h: i32, wrap: bool) -> Option<(usize, usize)> {
        if wrap { return Some(((tx & (w - 1)) as usize, (ty & (h - 1)) as usize)); }
        if tx < 0 || ty < 0 || tx >= w || ty >= h { None } else { Some((tx as usize, ty as usize)) }
    }

    /// Affine BG with 8-bit map entries and 256-colour tiles.
    fn affine_bg(&self, i: usize, line: &mut [u16; W]) {
        let cnt = self.reg16(8 + i * 2);
        let (cb, sb) = self.bases();
        let char_base = cb + ((cnt >> 2) & 15) as usize * 0x4000;
        let screen_base = sb + ((cnt >> 8) & 31) as usize * 0x800;
        let size = 128i32 << (cnt >> 14);
        let wrap = cnt & 0x2000 != 0;
        for (x, px_out) in line.iter_mut().enumerate() {
            let (tx, ty) = self.affine_coords(i, x);
            let Some((tx, ty)) = Self::fit(tx, ty, size, size, wrap) else { continue };
            let tile = self.bg8(screen_base + (ty / 8) * (size as usize / 8) + tx / 8) as usize;
            let idx = self.bg8(char_base + tile * 64 + (ty & 7) * 8 + (tx & 7)) as usize;
            if idx != 0 { *px_out = self.bgpal(idx); }
        }
    }

    /// Extended BG: 16-bit-entry affine tiles, or a 256-colour / direct-colour bitmap.
    fn extended_bg(&self, i: usize, line: &mut [u16; W]) {
        let cnt = self.reg16(8 + i * 2);
        let wrap = cnt & 0x2000 != 0;
        if cnt & 0x80 == 0 {
            let (cb, sb) = self.bases();
            let char_base = cb + ((cnt >> 2) & 15) as usize * 0x4000;
            let screen_base = sb + ((cnt >> 8) & 31) as usize * 0x800;
            let size = 128i32 << (cnt >> 14);
            let ext = self.dispcnt() & (1 << 30) != 0;
            for (x, px_out) in line.iter_mut().enumerate() {
                let (tx, ty) = self.affine_coords(i, x);
                let Some((tx, ty)) = Self::fit(tx, ty, size, size, wrap) else { continue };
                let e = self.bg16(screen_base + ((ty / 8) * (size as usize / 8) + tx / 8) * 2) as usize;
                let px = if e & 0x400 != 0 { 7 - (tx & 7) } else { tx & 7 };
                let py = if e & 0x800 != 0 { 7 - (ty & 7) } else { ty & 7 };
                let idx = self.bg8(char_base + (e & 0x3FF) * 64 + py * 8 + px) as usize;
                if idx == 0 { continue; }
                *px_out = if ext { self.ext_bg(i, (e >> 12) * 256 + idx) } else { self.bgpal(idx) };
            }
        } else {
            let (w, h) = [(128, 128), (256, 256), (512, 256), (512, 512)][(cnt >> 14) as usize];
            let base = ((cnt >> 8) & 31) as usize * 0x4000;
            let direct = cnt & 4 != 0;
            for (x, px_out) in line.iter_mut().enumerate() {
                let (tx, ty) = self.affine_coords(i, x);
                let Some((tx, ty)) = Self::fit(tx, ty, w, h, wrap) else { continue };
                let p = ty * w as usize + tx;
                if direct {
                    let c = self.bg16(base + p * 2);
                    if c & 0x8000 != 0 { *px_out = c & 0x7FFF; }
                } else {
                    let idx = self.bg8(base + p) as usize;
                    if idx != 0 { *px_out = self.bgpal(idx); }
                }
            }
        }
    }

    /// Mode 6 BG2: one 256-colour bitmap of 512x1024 or 1024x512 (all 512 KB of BG VRAM).
    fn large_bg(&self, line: &mut [u16; W]) {
        let cnt = self.reg16(0x0C);
        let (w, h) = if cnt & 0x4000 == 0 { (512, 1024) } else { (1024, 512) };
        let wrap = cnt & 0x2000 != 0;
        for (x, px_out) in line.iter_mut().enumerate() {
            let (tx, ty) = self.affine_coords(2, x);
            let Some((tx, ty)) = Self::fit(tx, ty, w, h, wrap) else { continue };
            let idx = self.bg8(ty * w as usize + tx) as usize;
            if idx != 0 { *px_out = self.bgpal(idx); }
        }
    }

    fn render_objs(&self, y: usize, dispcnt: u32, obj: &mut ObjLine) {
        let (_, _, mh, mv) = self.mosaic();
        let y = y as i32;
        let tile_1d = dispcnt & 0x10 != 0;
        let tile_bound = 32usize << ((dispcnt >> 20) & 3);
        let bmp_1d = dispcnt & 0x40 != 0;
        let bmp_bound = if self.a() { 128usize << ((dispcnt >> 22) & 1) } else { 128 };
        let bmp_wide = dispcnt & 0x20 != 0;
        let ext = dispcnt & (1 << 31) != 0;

        for n in 0..128 {
            let a0 = self.oam16(n * 8);
            let a1 = self.oam16(n * 8 + 2);
            let a2 = self.oam16(n * 8 + 4);
            let affine = a0 & 0x100 != 0;
            if !affine && a0 & 0x200 != 0 { continue; }
            let mode = (a0 >> 10) & 3;
            let shape = (a0 >> 14) as usize;
            if shape == 3 { continue; }
            let (w, h) = OBJ_SIZES[shape][(a1 >> 14) as usize];
            let double = affine && a0 & 0x200 != 0;
            let (bw, bh) = if double { (w * 2, h * 2) } else { (w, h) };

            let sy = (a0 & 0xFF) as i32;
            let mut rel_y = (y - sy) & 0xFF;
            if rel_y >= bh { continue; }
            let mosaic = a0 & 0x1000 != 0;
            if mosaic {
                let r = ((y - y % mv as i32) - sy) & 0xFF;
                if r < bh { rel_y = r; }
            }
            let sx = { let v = (a1 & 0x1FF) as i32; if v >= 256 { v - 512 } else { v } };
            let c256 = a0 & 0x2000 != 0;
            let tile = (a2 & 0x3FF) as usize;
            let prio = ((a2 >> 10) & 3) as u8;
            let pal = (a2 >> 12) as usize;
            if mode == 3 && pal == 0 { continue; }        // bitmap OBJ with alpha 0
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
                let (tx, ty) = (tx as usize, ty as usize);

                let (color, alpha) = if mode == 3 {
                    let addr = if bmp_1d {
                        tile * bmp_bound + (ty * w as usize + tx) * 2
                    } else if bmp_wide {
                        (tile & 0x1F) * 0x10 + (tile & 0x3E0) * 0x80 + ty * 512 + tx * 2
                    } else {
                        (tile & 0x0F) * 0x10 + (tile & 0x3F0) * 0x80 + ty * 256 + tx * 2
                    };
                    let c = self.obj16(addr);
                    if c & 0x8000 == 0 { continue; }
                    (c & 0x7FFF, pal as u8)
                } else {
                    let (tcol, trow) = (tx / 8, ty / 8);
                    let (px, py) = (tx & 7, ty & 7);
                    let unit = if c256 { 64 } else { 32 };
                    let tile_addr = if tile_1d {
                        tile * tile_bound + (trow * (w as usize / 8) + tcol) * unit
                    } else if c256 {
                        (((tile & !1) + trow * 32 + tcol * 2) & 0x3FF) * 32
                    } else {
                        ((tile + trow * 32 + tcol) & 0x3FF) * 32
                    };
                    let c = if c256 {
                        let idx = self.obj8(tile_addr + py * 8 + px) as usize;
                        if idx == 0 { continue; }
                        if ext { self.ext_obj(pal * 256 + idx) } else { self.objpal(idx) }
                    } else {
                        let b = self.obj8(tile_addr + py * 4 + px / 2);
                        let idx = (b >> ((px & 1) * 4)) as usize & 15;
                        if idx == 0 { continue; }
                        self.objpal(pal * 16 + idx)
                    };
                    (c, if mode == 1 { 17 } else { 16 })
                };

                let x = x as usize;
                if mode == 2 {
                    obj.win[x] = true;
                } else if obj.color[x] == TRANSPARENT || prio < obj.prio[x] {
                    obj.color[x] = color;
                    obj.prio[x] = prio;
                    obj.alpha[x] = alpha;
                }
            }
        }
    }
}

pub fn blend(a: u16, b: u16, eva: u32, evb: u32) -> u16 {
    let mut out = 0u16;
    for s in [0, 5, 10] {
        let ca = (a >> s) as u32 & 31;
        let cb = (b >> s) as u32 & 31;
        out |= (((ca * eva + cb * evb) >> 4).min(31) as u16) << s;
    }
    out
}

/// a*wa/32 + b*wb/32 per channel.
pub fn blend32(a: u16, b: u16, wa: u32, wb: u32) -> u16 {
    let mut out = 0u16;
    for s in [0, 5, 10] {
        let ca = (a >> s) as u32 & 31;
        let cb = (b >> s) as u32 & 31;
        out |= (((ca * wa + cb * wb) >> 5).min(31) as u16) << s;
    }
    out
}

pub fn fade(c: u16, evy: u32, up: bool) -> u16 {
    let mut out = 0u16;
    for s in [0, 5, 10] {
        let v = (c >> s) as u32 & 31;
        let v = if up { v + (((31 - v) * evy) >> 4) } else { v - ((v * evy) >> 4) };
        out |= (v as u16) << s;
    }
    out
}

/// MASTER_BRIGHT (0x6C): bits 0-4 factor (max 16), bits 14-15 mode (1 up, 2 down).
pub fn master_bright(line: &mut [u16; W], reg: u16) {
    let f = (reg & 31).min(16) as u32;
    let m = reg >> 14;
    if f == 0 || m == 0 || m == 3 { return; }
    for c in line.iter_mut() { *c = fade(*c, f, m == 1); }
}

/// BGR555 to RGBA8.
pub fn to_rgba(c: u16) -> [u8; 4] {
    let ch = |v: u16| ((v & 31) << 3 | (v & 31) >> 2) as u8;
    [ch(c), ch(c >> 5), ch(c >> 10), 255]
}
