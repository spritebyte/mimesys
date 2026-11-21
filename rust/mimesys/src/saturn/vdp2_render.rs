//! Drawing one VDP2 line: the four normal scroll screens (NBG0-3, cells or bitmaps), the
//! sprite layer (VDP1's frame buffer on display), the back screen, priorities, colour
//! calculation and colour offset. Written from Sega's "VDP2 User's Manual"; pattern name
//! and supplement layouts as libyaul's vdp2_scrn_cell.c / scrn_macros.h (MIT) set them.
//!
//! - NBGn (BGON bits 0-3): CHCTLA / CHCTLB pick 1x1 or 2x2-cell characters, cells or a
//!   bitmap, and the colours (16 / 256 / 2048 palette, 32768 / 16M RGB). NBG0 / NBG1 scroll
//!   in 1/256 dots and zoom; NBG2 / NBG3 scroll in whole dots.
//! - A cell screen is 2x2 planes, a plane 1, 2x1 or 2x2 pages (PLSZ), a page 64x64 cells
//!   (512x512 dots). Plane addresses: (MPOFN << 6 | MPxx) pages; pattern names are 1 word
//!   (with PNCN's supplement bits) or 2 words.
//! - Colour RAM (RAMCTL bits 13-12): 1024 or 2048 RGB555 colours, or 1024 RGB888.
//! - Priorities 1-7 (0 = hidden); equal ones: sprite, then NBG0 ... NBG3. The top dot mixes
//!   with the one below it when its layer has colour calculation on (ratio or add).
//! - Not yet: rotation (RBG0), line / vertical cell scroll, windows, mosaic, shadows,
//!   special priority / colour calculation, line colour screen.

use super::vdp1::Vdp1;
use super::vdp2::{Vdp2, VRAM_SIZE};

/// A dot of one layer: colour (RGB888), priority, the layer's order, colour calculation
#[derive(Clone, Copy)]
struct Dot { rgb: [u8; 3], prio: u8, order: u8, cc: Option<u8>, offset: Option<bool> }

/// NBGn's settings for one line
struct Nbg {
    ccc: u8,
    char2x2: bool,
    bitmap: Option<(u32, u32, u32)>,    // width, height, base
    bmp_pal: u32,
    pn1: bool,
    cnsm: bool,
    scn: u32,
    splt: u32,
    plsz: u16,
    maps: [u32; 4],
    x: i64,
    dx: i64,
    y: i64,
    /// vertical cell scroll: a vertical offset per 8-dot screen column (24.8)
    vcs: Vec<i64>,
    caos: u32,
    opaque0: bool,
    prio: u8,
    cc: Option<u8>,
    offset: Option<bool>,
}

impl Vdp2 {
    fn reg(&self, off: usize) -> u16 { self.regs[off / 2] }

    fn vword(&self, a: u32) -> u16 {
        let a = a as usize & (VRAM_SIZE - 2);
        u16::from_be_bytes([self.vram[a], self.vram[a + 1]])
    }

    /// A table address from an upper / lower register pair (line scroll, vertical cell scroll)
    fn table_addr(&self, r: usize) -> u32 {
        (((self.reg(r) & 7) as u32) << 16 | (self.reg(r + 2) & 0xFFFE) as u32) << 1
    }

    fn vbyte(&self, a: u32) -> u8 { self.vram[a as usize & (VRAM_SIZE - 1)] }

    /// A colour RAM entry as RGB888
    fn cram_rgb(&self, i: u32) -> [u8; 3] {
        match (self.reg(0x0E) >> 12) & 3 {
            2 | 3 => {
                let a = (i as usize & 0x3FF) * 4;
                [self.cram[a + 3], self.cram[a + 2], self.cram[a + 1]]
            }
            mode => {
                let a = (i as usize & if mode == 0 { 0x3FF } else { 0x7FF }) * 2;
                rgb555(u16::from_be_bytes([self.cram[a], self.cram[a + 1]]))
            }
        }
    }

    /// The colour offset for a layer (CLOFEN bit `bit`): Some(B?) when on
    fn offset_of(&self, bit: u16) -> Option<bool> {
        (self.reg(0x110) & (1 << bit) != 0).then(|| self.reg(0x112) & (1 << bit) != 0)
    }

    fn nbg(&self, n: usize, line: i64) -> Option<Nbg> {
        if self.reg(0x20) & (1 << n) == 0 { return None; }
        let prio = ((if n < 2 { self.reg(0xF8) } else { self.reg(0xFA) } >> if n & 1 == 0 { 0 } else { 8 }) & 7) as u8;
        if prio == 0 { return None; }
        let (a, b) = (self.reg(0x28), self.reg(0x2A));
        let (ccc, char2x2, bmen, bmsz) = match n {
            0 => ((a >> 4) & 7, a & 1 != 0, a & 2 != 0, (a >> 2) & 3),
            1 => ((a >> 12) & 3, a & 0x100 != 0, a & 0x200 != 0, (a >> 10) & 3),
            2 => ((b >> 1) & 1, b & 1 != 0, false, 0),
            _ => ((b >> 5) & 1, b & 0x10 != 0, false, 0),
        };
        let mpof = ((self.reg(0x3C) >> (n * 4)) & 7) as u32;
        let pncn = self.reg(0x30 + n * 2);
        let (mab, mcd) = (self.reg(0x40 + n * 4), self.reg(0x42 + n * 4));
        let maps = [mab & 0x3F, (mab >> 8) & 0x3F, mcd & 0x3F, (mcd >> 8) & 0x3F].map(|m| mpof << 6 | m as u32);
        let fixed = |int: u16, frac: u16, mask: u16| ((int & mask) as i64) << 8 | (frac >> 8) as i64;
        let mut vcs = Vec::new();
        let (x, dx, y) = if n < 2 {
            let r = 0x70 + n * 0x10;
            let zx = fixed(self.reg(r + 8), self.reg(r + 10), 7);
            let zy = fixed(self.reg(r + 12), self.reg(r + 14), 7);
            let zx = if zx == 0 { 0x100 } else { zx };
            let zy = if zy == 0 { 0x100 } else { zy };
            let (scx, scy) = (fixed(self.reg(r), self.reg(r + 2), 0x7FF), fixed(self.reg(r + 4), self.reg(r + 6), 0x7FF));
            // line scroll (SCRCTL): per-line values from a table, added to the scroll registers
            let ctl = self.reg(0x9A) >> (n * 8);
            let (lscx, lscy, lzmx) = (ctl & 2 != 0, ctl & 4 != 0, ctl & 8 != 0);
            let (mut x, mut dx, mut y) = (scx, zx, scy + line * zy);
            if lscx || lscy || lzmx {
                let interval = 1i64 << ((ctl >> 4) & 3);
                let entry = 4 * (lscx as u32 + lscy as u32 + lzmx as u32);
                let base = self.table_addr(0xA0 + n * 4);
                let mut a = base + (line / interval) as u32 * entry;
                let mut read = |mask: u16| { let v = fixed(self.vword(a), self.vword(a + 2), mask); a += 4; v };
                if lscx { x = scx + read(0x7FF); }
                if lscy { y = scy + read(0x7FF) + (line % interval) * zy; }
                if lzmx { dx = read(7); }
            }
            // vertical cell scroll: one entry per screen column (NBG0 / NBG1 interleave when both use it)
            if ctl & 1 != 0 {
                let both = self.reg(0x9A) & 0x101 == 0x101;
                let (stride, first) = if both { (8, n as u32 * 4) } else { (4, 0) };
                let base = self.table_addr(0x9C) + first;
                vcs = (0..90u32).map(|c| fixed(self.vword(base + c * stride), self.vword(base + c * stride + 2), 0x7FF)).collect();
            }
            (x, dx, y)
        } else {
            let r = 0x90 + (n - 2) * 4;
            (((self.reg(r) & 0x7FF) as i64) << 8, 0x100, (((self.reg(r + 2) & 0x7FF) as i64) << 8) + line * 0x100)
        };
        let bitmap = bmen.then(|| {
            let (w, h) = [(512, 256), (512, 512), (1024, 256), (1024, 512)][bmsz as usize];
            (w, h, mpof * 0x2_0000)
        });
        let bmpna = self.reg(0x2C) >> if n == 1 { 8 } else { 0 };
        let ccr = ((if n < 2 { self.reg(0x108) } else { self.reg(0x10A) } >> if n & 1 == 0 { 0 } else { 8 }) & 31) as u8;
        Some(Nbg {
            ccc: ccc as u8, char2x2, bitmap, bmp_pal: (bmpna & 7) as u32,
            pn1: pncn & 0x8000 != 0, cnsm: pncn & 0x4000 != 0, scn: (pncn & 0x1F) as u32, splt: ((pncn >> 5) & 7) as u32,
            plsz: (self.reg(0x3A) >> (n * 2)) & 3, maps, x, dx, y, vcs,
            caos: ((self.reg(0xE4) >> (n * 4)) & 7) as u32,
            opaque0: self.reg(0x20) & (0x100 << n) != 0,
            prio,
            cc: (self.reg(0xEC) & (1 << n) != 0).then_some(ccr),
            offset: self.offset_of(n as u16),
        })
    }

    /// NBG dot at screen coordinates (x, y): its colour, or None for transparent
    fn nbg_dot(&self, g: &Nbg, x: u32, y: u32) -> Option<[u8; 3]> {
        let (dot, palnum) = if let Some((w, h, base)) = g.bitmap {
            let (bx, by) = (x & (w - 1), y & (h - 1));
            (self.read_dot(base, by * w + bx, g.ccc), g.bmp_pal << 4)
        } else {
            let (pw, ph) = match g.plsz { 0 => (1, 1), 1 => (2, 1), _ => (2, 2) };
            let (plw, plh) = (512 * pw, 512 * ph);
            let (sx, sy) = (x & (2 * plw - 1), y & (2 * plh - 1));
            let plane = (sx / plw + 2 * (sy / plh)) as usize;
            let mut map = g.maps[plane];
            if pw == 2 { map &= !1; }
            if ph == 2 { map &= !2; }
            let pn_bytes = if g.pn1 { 2 } else { 4 };
            let page_bytes = if g.char2x2 { 32 * 32 } else { 64 * 64 } * pn_bytes;
            let (px, py) = (sx % plw, sy % plh);
            let page = (py / 512) * pw + px / 512;
            let (lx, ly) = (px % 512, py % 512);
            let index = if g.char2x2 { (ly / 16) * 32 + lx / 16 } else { (ly / 8) * 64 + lx / 8 };
            let a = (map + page) * page_bytes + index * pn_bytes;
            let (charno, pal, hf, vf) = self.pattern_name(g, a);
            let (mut cx, mut cy) = ((lx >> 3) & 1, (ly >> 3) & 1);
            let (mut dx, mut dy) = (lx & 7, ly & 7);
            if hf { cx ^= 1; dx = 7 - dx; }
            if vf { cy ^= 1; dy = 7 - dy; }
            let cell_bytes = [32, 64, 128, 128, 256][g.ccc.min(4) as usize];
            let cell = if g.char2x2 { cy * 2 + cx } else { 0 };
            (self.read_dot(charno * 0x20 + cell * cell_bytes, dy * 8 + dx, g.ccc), pal)
        };
        let bpp_ccc = g.ccc;
        match bpp_ccc {
            0 | 1 => {
                if dot == 0 && !g.opaque0 { return None; }
                let idx = if bpp_ccc == 0 { palnum * 16 + dot } else { (palnum & 0x70) * 16 + dot };
                Some(self.cram_rgb((g.caos << 8) + idx))
            }
            2 => {
                let d = dot & 0x7FF;
                if d == 0 && !g.opaque0 { return None; }
                Some(self.cram_rgb((g.caos << 8) + d))
            }
            3 => {
                if dot & 0x8000 == 0 { return None; }
                Some(rgb555(dot as u16))
            }
            _ => {
                if dot & 0x8000_0000 == 0 { return None; }
                Some([dot as u8, (dot >> 8) as u8, (dot >> 16) as u8])
            }
        }
    }

    /// Dot `i` of the pattern / bitmap at `base`, by colour mode
    fn read_dot(&self, base: u32, i: u32, ccc: u8) -> u32 {
        match ccc {
            0 => { let b = self.vbyte(base + i / 2); if i & 1 == 0 { (b >> 4) as u32 } else { (b & 15) as u32 } }
            1 => self.vbyte(base + i) as u32,
            2 | 3 => self.vword(base + i * 2) as u32,
            _ => (self.vword(base + i * 4) as u32) << 16 | self.vword(base + i * 4 + 2) as u32,
        }
    }

    /// (character number, palette number, h flip, v flip)
    fn pattern_name(&self, g: &Nbg, a: u32) -> (u32, u32, bool, bool) {
        if !g.pn1 {
            let w = (self.vword(a) as u32) << 16 | self.vword(a + 2) as u32;
            return (w & 0x7FFF, (w >> 16) & 0x7F, w & 0x4000_0000 != 0, w & 0x8000_0000 != 0);
        }
        let w = self.vword(a) as u32;
        let pal = if g.ccc == 0 { (w >> 12) & 0xF | g.splt << 4 } else { ((w >> 12) & 7) << 4 };
        let (cn, hf, vf) = match (g.cnsm, g.char2x2) {
            (false, false) => ((w & 0x3FF) | (g.scn & 0x1F) << 10, w & 0x400 != 0, w & 0x800 != 0),
            (false, true) => ((w & 0x3FF) << 2 | (g.scn & 3) | (g.scn & 0x1C) << 10, w & 0x400 != 0, w & 0x800 != 0),
            (true, false) => ((w & 0xFFF) | (g.scn & 0x1C) << 10, false, false),
            (true, true) => ((w & 0xFFF) << 2 | (g.scn & 3) | (g.scn & 0x10) << 10, false, false),
        };
        (cn, pal, hf, vf)
    }

    /// The sprite layer's dot at (x, y) of VDP1's display buffer
    fn sprite_dot(&self, vdp1: &Vdp1, x: u32, y: u32) -> Option<Dot> {
        let fb = vdp1.display_fb();
        let spctl = self.reg(0xE0);
        let ty = spctl & 15;
        let px: u16 = if vdp1.bpp8() {
            if x >= 1024 || y >= 256 { return None; }
            fb[(y * 1024 + x) as usize] as u16
        } else {
            if x >= 512 || y >= 256 { return None; }
            let a = ((y * 512 + x) * 2) as usize;
            u16::from_be_bytes([fb[a], fb[a + 1]])
        };
        if px == 0 { return None; }
        let rgb = spctl & 0x20 != 0 && px & 0x8000 != 0 && ty < 8;
        let (pr, cc, dc) = if rgb { (0, 0, 0) } else {
            let b = px & 0xFF;
            match ty {
                0 => (px >> 14 & 3, px >> 11 & 7, px & 0x7FF),
                1 => (px >> 13 & 7, px >> 11 & 3, px & 0x7FF),
                2 => (px >> 14 & 1, px >> 11 & 7, px & 0x7FF),
                3 => (px >> 13 & 3, px >> 11 & 3, px & 0x7FF),
                4 => (px >> 13 & 3, px >> 10 & 7, px & 0x3FF),
                5 => (px >> 12 & 7, px >> 11 & 1, px & 0x7FF),
                6 => (px >> 12 & 7, px >> 10 & 3, px & 0x3FF),
                7 => (px >> 12 & 7, px >> 9 & 7, px & 0x1FF),
                8 => (b >> 7, 0, b & 0x7F),
                9 => (b >> 7, b >> 6 & 1, b & 0x3F),
                0xA => (b >> 6, 0, b & 0x3F),
                0xB => (0, b >> 6, b & 0x3F),
                0xC => (b >> 7, 0, b),
                0xD => (b >> 7, b >> 6 & 1, b),
                0xE => (b >> 6, 0, b),
                _ => (0, b >> 6, b),
            }
        };
        if !rgb && dc == 0 { return None; }
        let prio = ((self.reg(0xF0 + (pr as usize / 2) * 2) >> if pr & 1 != 0 { 8 } else { 0 }) & 7) as u8;
        if prio == 0 { return None; }
        let color = if rgb { rgb555(px) } else {
            let caos = ((self.reg(0xE6) >> 4) & 7) as u32;
            self.cram_rgb((caos << 8) + dc as u32)
        };
        let ccctl = self.reg(0xEC);
        let spccn = ((spctl >> 8) & 7) as u8;
        let cond = match (spctl >> 12) & 3 {
            0 => prio <= spccn,
            1 => prio == spccn,
            2 => prio >= spccn,
            _ => px & 0x8000 != 0,
        };
        let ratio = ((self.reg(0x100 + (cc as usize / 2) * 2) >> if cc & 1 != 0 { 8 } else { 0 }) & 31) as u8;
        Some(Dot { rgb: color, prio, order: 0, cc: (ccctl & 0x40 != 0 && cond).then_some(ratio), offset: self.offset_of(6) })
    }

    /// The back screen's colour on line `line`
    fn back(&self, line: u32) -> [u8; 3] {
        let per_line = self.reg(0xAC) & 0x8000 != 0;
        let base = ((self.reg(0xAC) as u32 & 7) << 16 | self.reg(0xAE) as u32) << 1;
        rgb555(self.vword(base + if per_line { line * 2 } else { 0 }))
    }

    /// Draw output line `y` (0..height) into `out` (RGBA, `width` dots)
    pub fn render_line(&self, y: u32, vdp1: &Vdp1, out: &mut [u8]) {
        let w = out.len() / 4;
        if self.tvmd() & 0x8000 == 0 {
            for p in out.chunks_exact_mut(4) { p.copy_from_slice(&[0, 0, 0, 0xFF]); }
            return;
        }
        let field_line = if self.interlaced() { y >> 1 } else { y };
        let layers: Vec<(usize, Nbg)> = (0..4).filter_map(|n| self.nbg(n, y as i64).map(|g| (n, g))).collect();
        let back = Dot { rgb: self.back(field_line), prio: 0, order: 9, cc: None, offset: self.offset_of(5) };
        let ccctl = self.reg(0xEC);
        let add = ccctl & 0x100 != 0;
        let offs = |b: bool| {
            let s = |r: usize| { let v = (self.reg(r) & 0x1FF) as i32; if v & 0x100 != 0 { v - 0x200 } else { v } };
            if b { [s(0x11A), s(0x11C), s(0x11E)] } else { [s(0x114), s(0x116), s(0x118)] }
        };
        let off_a = offs(false);
        let off_b = offs(true);
        let sprite_y = if self.interlaced() { y >> 1 } else { y };
        for x in 0..w {
            let (mut top, mut second) = (back, back);
            let mut consider = |d: Dot| {
                let better = |a: &Dot, b: &Dot| a.prio > b.prio || (a.prio == b.prio && a.order < b.order);
                if better(&d, &top) { second = top; top = d; } else if better(&d, &second) { second = d; }
            };
            if let Some(d) = self.sprite_dot(vdp1, x as u32, sprite_y) { consider(d); }
            for (n, g) in &layers {
                let sx = ((g.x + g.dx * x as i64) >> 8) as u32;
                let vy = if g.vcs.is_empty() { 0 } else { g.vcs[(x / 8).min(g.vcs.len() - 1)] };
                let sy = ((g.y + vy) >> 8) as u32;
                if let Some(rgb) = self.nbg_dot(g, sx, sy) {
                    consider(Dot { rgb, prio: g.prio, order: 1 + *n as u8, cc: g.cc, offset: g.offset });
                }
            }
            let mut rgb = top.rgb;
            if let Some(r) = top.cc {
                for k in 0..3 {
                    let (a, b) = (top.rgb[k] as u32, second.rgb[k] as u32);
                    rgb[k] = if add { (a + b).min(255) as u8 } else { ((a * (31 - r as u32) + b * (r as u32 + 1)) / 32) as u8 };
                }
            }
            if let Some(b) = top.offset {
                let o = if b { off_b } else { off_a };
                for k in 0..3 { rgb[k] = (rgb[k] as i32 + o[k]).clamp(0, 255) as u8; }
            }
            out[x * 4..x * 4 + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 0xFF]);
        }
    }
}

/// RGB555 (blue in the top bits) as RGB888
pub fn rgb555(c: u16) -> [u8; 3] {
    let e = |v: u16| { let v = (v & 31) as u8; v << 3 | v >> 2 };
    [e(c), e(c >> 5), e(c >> 10)]
}
