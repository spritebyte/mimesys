//! VDP1, the sprite and polygon processor: 512 KB of VRAM holding the command list and
//! textures, two 256 KB frame buffers (one drawn while VDP2 shows the other) and registers
//! at 0x05D00000. Written from Sega's "VDP1 User's Manual" (command table layout as in
//! libyaul's vdp1/cmdt.h, MIT).
//!
//! - PTMR (004h): 1 = draw now, 2 = draw at each frame change.
//! - FBCR (002h): FCM (bit 1) / FCT (bit 0): FCM 0 swaps and erases every frame; FCM 1
//!   swaps when FCT is 1, else erases only.
//! - EWDR / EWLR / EWRR: the erase colour and window (x in units of 8 dots).
//! - A command is 32 bytes. CMDCTRL: END bit 15, jump mode bits 14-12 (next, jump, call,
//!   return; bit 14 skips the command), zoom point bits 11-8, flip bits 5-4, the command in
//!   bits 3-0 (0 sprite, 1 scaled sprite, 2/3 distorted sprite, 4 polygon, 5/7 polyline,
//!   6 line, 8/B user clip, 9 system clip, A local coordinates). CMDLINK (+2) / 8 is the
//!   jump target. CMDPMOD (+4): MSB on 15, user clip 10-9, mesh 8, end code off 7,
//!   transparent pixel off 6, colour mode 5-3, colour calculation 2-0. CMDCOLR (+6),
//!   CMDSRCA (+8) / 8, CMDSIZE (+A: width / 8, height), XA..YD (+C..+1A), CMDGRDA (+1C) / 8.
//! - Quads (distorted sprites, polygons) are drawn the way the chip does: a line from the
//!   A-D edge to the B-C edge for each step down the longer edge, with an extra dot where a
//!   line steps diagonally, so no gaps.
//! - Drawing is instant (no command timing yet).

use serde::{Deserialize, Serialize};

pub const VRAM_SIZE: usize = 0x8_0000;
pub const FB_SIZE: usize = 0x4_0000;

#[derive(Clone, Serialize, Deserialize)]
pub struct Vdp1 {
    #[serde(with = "serde_bytes")]
    pub vram: Vec<u8>,
    /// the two frame buffers; `draw` is the one being drawn (and seen by the CPU)
    #[serde(with = "serde_bytes")]
    pub fb0: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub fb1: Vec<u8>,
    pub draw: usize,
    pub tvmr: u16,
    pub fbcr: u16,
    pub ptmr: u16,
    pub ewdr: u16,
    pub ewlr: u16,
    pub ewrr: u16,
    pub edsr: u16,
    pub lopr: u16,
    pub copr: u16,
    /// FBCR as written for the next frame change
    swap_asked: bool,
    erase_asked: bool,
    /// clipping and the local origin (they last between lists)
    sys_clip: (i32, i32),
    user_clip: (i32, i32, i32, i32),
    local: (i32, i32),
    /// the command list ran (raise the sprite draw end interrupt)
    #[serde(skip)]
    pub ended: bool,
    #[serde(skip)]
    pub draws: u32,
}

/// One command's drawing state
struct Cmd {
    pmod: u16,
    colr: u16,
    /// texture address, width and height
    src: usize,
    w: i32,
    h: i32,
    flip_h: bool,
    flip_v: bool,
    /// gouraud corner colours (RGB555), A B C D
    gouraud: Option<[u16; 4]>,
    textured: bool,
}

impl Vdp1 {
    pub fn new() -> Self {
        Self {
            vram: vec![0; VRAM_SIZE], fb0: vec![0; FB_SIZE], fb1: vec![0; FB_SIZE], draw: 0,
            tvmr: 0, fbcr: 0, ptmr: 0, ewdr: 0, ewlr: 0, ewrr: 0, edsr: 0, lopr: 0, copr: 0,
            swap_asked: false, erase_asked: false, sys_clip: (319, 223), user_clip: (0, 0, 319, 223), local: (0, 0),
            ended: false, draws: 0,
        }
    }

    pub fn fb(&mut self) -> &mut Vec<u8> { if self.draw == 0 { &mut self.fb0 } else { &mut self.fb1 } }

    /// The frame buffer VDP2 shows
    pub fn display_fb(&self) -> &[u8] { if self.draw == 0 { &self.fb1 } else { &self.fb0 } }

    /// 8 bits a dot (the high-resolution and rotation modes)
    pub fn bpp8(&self) -> bool { self.tvmr & 1 != 0 }

    pub fn read_reg(&self, off: u32) -> u16 {
        match off & 0x1E {
            0x10 => self.edsr,
            0x12 => self.lopr,
            0x14 => self.copr,
            // MODR: version 1, and the mode bits the CPU set
            0x16 => 0x1000 | (self.ptmr & 2) << 7 | (self.fbcr & 0x1F) << 3 | (self.tvmr & 0xF),
            _ => 0,
        }
    }

    pub fn write_reg(&mut self, off: u32, v: u16) {
        match off & 0x1E {
            0x00 => self.tvmr = v & 0xF,
            0x02 => {
                self.fbcr = v & 0x1F;
                if v & 2 != 0 {
                    if v & 1 != 0 { self.swap_asked = true; } else { self.erase_asked = true; }
                }
            }
            0x04 => {
                self.ptmr = v & 3;
                if v & 3 == 1 { self.run(); }
            }
            0x06 => self.ewdr = v,
            0x08 => self.ewlr = v,
            0x0A => self.ewrr = v,
            0x0C => {} // ENDR: stop drawing (it never runs for long)
            _ => {}
        }
    }

    /// The frame changes (VBlank-IN): swap the buffers if it's time, erase, then the
    /// automatic draw
    pub fn frame_change(&mut self) {
        let one_cycle = self.fbcr & 2 == 0;
        if one_cycle || self.swap_asked {
            self.swap_asked = false;
            self.erase_asked = false;
            self.draw ^= 1;
            self.edsr = (self.edsr & 2) >> 1;     // CEF -> BEF
            self.erase();
            if self.ptmr == 2 { self.run(); }
        } else if self.erase_asked {
            // manual erase: the buffer on display, which is drawn after the next swap
            self.erase_asked = false;
            self.draw ^= 1;
            self.erase();
            self.draw ^= 1;
        }
    }

    /// Fill the erase window of the draw buffer with EWDR
    fn erase(&mut self) {
        let bpp8 = self.bpp8();
        let (x1, y1) = (((self.ewlr >> 9) & 0x3F) as usize * 8, (self.ewlr & 0x1FF) as usize);
        let (x3, y3) = (((self.ewrr >> 9) & 0x7F) as usize * 8, (self.ewrr & 0x1FF) as usize);
        let v = self.ewdr;
        let fb = if self.draw == 0 { &mut self.fb0 } else { &mut self.fb1 };
        // in 8-bit mode the window counts 16-bit pairs of dots
        let (stride, x1, x3) = if bpp8 { (1024, x1 * 2, x3 * 2) } else { (512, x1, x3) };
        for y in y1..=y3.min(255) {
            for x in x1..x3.min(stride) {
                if bpp8 {
                    fb[y * 1024 + x] = if x & 1 == 0 { (v >> 8) as u8 } else { v as u8 };
                } else {
                    let a = (y * 512 + x) * 2;
                    fb[a] = (v >> 8) as u8;
                    fb[a + 1] = v as u8;
                }
            }
        }
    }

    fn word(&self, a: usize) -> u16 {
        u16::from_be_bytes([self.vram[a & (VRAM_SIZE - 1)], self.vram[(a + 1) & (VRAM_SIZE - 1)]])
    }

    /// Run the command list from 0 to its END
    fn run(&mut self) {
        let mut a = 0usize;
        let mut ret: Option<usize> = None;
        let mut last = 0;
        self.edsr &= !2;
        for _ in 0..20_000 {
            let ctrl = self.word(a);
            if ctrl & 0x8000 != 0 { break; }
            last = a;
            if ctrl & 0x4000 == 0 { self.command(a, ctrl); }
            let link = (self.word(a + 2) as usize) << 3;
            a = match (ctrl >> 12) & 3 {
                0 => a + 32,
                1 => link,
                2 => { if ret.is_none() { ret = Some(a + 32); } link }
                _ => ret.take().unwrap_or(a + 32),
            } & (VRAM_SIZE - 1);
        }
        self.lopr = (last >> 3) as u16;
        self.copr = (a >> 3) as u16;
        self.edsr |= 2;
        self.ended = true;
        self.draws += 1;
    }

    /// A vertex: 13-bit signed, plus the local origin
    fn vertex(&self, a: usize, k: usize) -> (i32, i32) {
        let s = |v: u16| ((v << 3) as i16 >> 3) as i32;
        (s(self.word(a + 0x0C + k * 4)) + self.local.0, s(self.word(a + 0x0E + k * 4)) + self.local.1)
    }

    fn command(&mut self, a: usize, ctrl: u16) {
        let raw = |k: usize| { let v = self.word(a + 0x0C + k * 2); ((v << 3) as i16 >> 3) as i32 };
        let op = ctrl & 0xF;
        match op {
            0x8 | 0xB => { self.user_clip = (raw(0), raw(1), raw(4), raw(5)); return; }
            0x9 => { self.sys_clip = (raw(4), raw(5)); return; }
            0xA => { self.local = (raw(0), raw(1)); return; }
            0x0..=0x7 => {}
            _ => return,
        }
        let pmod = self.word(a + 4);
        let size = self.word(a + 0x0A);
        let gouraud = if pmod & 4 != 0 {
            let g = (self.word(a + 0x1C) as usize) << 3;
            Some([self.word(g), self.word(g + 2), self.word(g + 4), self.word(g + 6)])
        } else { None };
        let c = Cmd {
            pmod, colr: self.word(a + 6), src: (self.word(a + 8) as usize) << 3,
            w: ((size >> 8) & 0x3F) as i32 * 8, h: (size & 0xFF) as i32,
            flip_h: ctrl & 0x10 != 0, flip_v: ctrl & 0x20 != 0, gouraud, textured: op < 4,
        };
        match op {
            0x0 => {
                let (x, y) = self.vertex(a, 0);
                self.rect(&c, x, y, x + c.w - 1, y + c.h - 1);
            }
            0x1 => {
                let (xa, ya) = self.vertex(a, 0);
                let zp = (ctrl >> 8) & 0xF;
                let (x0, y0, x1, y1) = if zp == 0 {
                    let (xc, yc) = self.vertex(a, 2);
                    (xa, ya, xc, yc)
                } else {
                    // the zoom point: XA, YA is that point of the sprite, XB, YB its size
                    let w = raw(2);
                    let h = raw(3);
                    let ox = match zp & 3 { 2 => w / 2, 3 => w, _ => 0 };
                    let oy = match (zp >> 2) & 3 { 2 => h / 2, 3 => h, _ => 0 };
                    (xa - ox, ya - oy, xa - ox + w, ya - oy + h)
                };
                self.rect(&c, x0, y0, x1, y1);
            }
            0x2 | 0x3 | 0x4 => {
                let v = [self.vertex(a, 0), self.vertex(a, 1), self.vertex(a, 2), self.vertex(a, 3)];
                self.quad(&c, v);
            }
            0x5 | 0x7 => {
                let v = [self.vertex(a, 0), self.vertex(a, 1), self.vertex(a, 2), self.vertex(a, 3)];
                let g = c.gouraud.unwrap_or([0; 4]);
                for k in 0..4 {
                    let n = (k + 1) & 3;
                    self.line(&c, v[k], v[n], (g[k], g[n]), None);
                }
            }
            _ => {
                let (p, q) = (self.vertex(a, 0), self.vertex(a, 1));
                let g = c.gouraud.unwrap_or([0; 4]);
                self.line(&c, p, q, (g[0], g[1]), None);
            }
        }
    }

    /// The texel at (u, v): None = transparent; `end` = an end code
    fn texel(&self, c: &Cmd, u: i32, v: i32) -> (Option<u16>, bool) {
        let mode = (c.pmod >> 3) & 7;
        let spd = c.pmod & 0x40 != 0;
        let ecd = c.pmod & 0x80 != 0;
        let i = (v * c.w + u) as usize;
        let (dot, end, zero) = match mode {
            0 | 1 => {
                let b = self.vram[(c.src + i / 2) & (VRAM_SIZE - 1)];
                let d = if i & 1 == 0 { b >> 4 } else { b & 15 } as u16;
                (d, d == 0xF, d == 0)
            }
            2..=4 => {
                let d = self.vram[(c.src + i) & (VRAM_SIZE - 1)] as u16;
                (d, d == 0xFF, d == 0)
            }
            _ => {
                let d = self.word(c.src + i * 2);
                (d, d == 0x7FFF, d == 0)
            }
        };
        if end && !ecd { return (None, true); }
        if zero && !spd { return (None, false); }
        let px = match mode {
            0 => (c.colr & 0xFFF0) | dot,
            1 => self.word(((c.colr as usize) << 3) + dot as usize * 2),
            2 => (c.colr & 0xFFC0) | (dot & 0x3F),
            3 => (c.colr & 0xFF80) | (dot & 0x7F),
            4 => (c.colr & 0xFF00) | dot,
            _ => dot,
        };
        (Some(px), false)
    }

    /// Where a texture row's end code is (dots from the row's start), if any
    fn row_end(&self, c: &Cmd, v: i32) -> i32 {
        if c.pmod & 0x80 != 0 { return c.w; }
        for u in 0..c.w {
            if self.texel(c, u, v).1 { return u; }
        }
        c.w
    }

    /// A sprite on screen from (x0, y0) to (x1, y1), the texture stretched over it
    fn rect(&mut self, c: &Cmd, x0: i32, y0: i32, x1: i32, y1: i32) {
        if c.w == 0 || c.h == 0 { return; }
        let (fh, fv) = (c.flip_h ^ (x1 < x0), c.flip_v ^ (y1 < y0));
        let (xl, xr) = (x0.min(x1), x0.max(x1));
        let (yt, yb) = (y0.min(y1), y0.max(y1));
        let (dw, dh) = (xr - xl + 1, yb - yt + 1);
        if dw > 4096 || dh > 4096 { return; }
        let (cx0, cy0, cx1, cy1) = self.clip_box();
        for y in yt.max(cy0)..=yb.min(cy1) {
            let mut v = (y - yt) * c.h / dh;
            if fv { v = c.h - 1 - v; }
            let end = self.row_end(c, v);
            for x in xl.max(cx0)..=xr.min(cx1) {
                let mut u = (x - xl) * c.w / dw;
                if fh { u = c.w - 1 - u; }
                if u >= end { continue; }
                if let (Some(px), _) = self.texel(c, u, v) {
                    let g = c.gouraud.map(|g| {
                        let fx = (x - xl) * 256 / dw.max(1);
                        let fy = (y - yt) * 256 / dh.max(1);
                        bilerp(g, fx, fy)
                    });
                    self.plot(c, x, y, px, g);
                }
            }
        }
    }

    /// A distorted sprite or polygon: lines from the A-D edge to the B-C edge
    fn quad(&mut self, c: &Cmd, v: [(i32, i32); 4]) {
        let [a, b, cc, d] = v;
        let len = |p: (i32, i32), q: (i32, i32)| (q.0 - p.0).abs().max((q.1 - p.1).abs());
        let n = len(a, d).max(len(b, cc)) + 1;
        if n > 4096 || len(a, b).max(len(d, cc)) > 4096 { return; }
        let g = c.gouraud.unwrap_or([0; 4]);
        for i in 0..n {
            let t = |p: i32, q: i32| p + (q - p) * i / (n - 1).max(1);
            let p = (t(a.0, d.0), t(a.1, d.1));
            let q = (t(b.0, cc.0), t(b.1, cc.1));
            let gp = lerp_rgb(g[0], g[3], i * 256 / (n - 1).max(1));
            let gq = lerp_rgb(g[1], g[2], i * 256 / (n - 1).max(1));
            let row = if c.textured {
                let mut tv = i * c.h / n;
                if c.flip_v { tv = c.h - 1 - tv; }
                Some(tv)
            } else { None };
            self.line(c, p, q, (gp, gq), row);
        }
    }

    /// A line (with the gap-filling extra dot). `row`: the texture row to stretch along it
    fn line(&mut self, c: &Cmd, p: (i32, i32), q: (i32, i32), g: (u16, u16), row: Option<i32>) {
        let (dx, dy) = (q.0 - p.0, q.1 - p.1);
        let m = dx.abs().max(dy.abs());
        if m > 4096 { return; }
        let end = row.map(|v| self.row_end(c, v));
        let (cx0, cy0, cx1, cy1) = self.clip_box();
        let mut prev = p;
        for j in 0..=m {
            let x = p.0 + if m == 0 { 0 } else { (dx * j * 2 + m * dx.signum()) / (2 * m) };
            let y = p.1 + if m == 0 { 0 } else { (dy * j * 2 + m * dy.signum()) / (2 * m) };
            let px = match row {
                Some(v) => {
                    let mut u = if m == 0 { 0 } else { j * c.w / (m + 1) };
                    if c.flip_h { u = c.w - 1 - u; }
                    if u >= end.unwrap_or(c.w) { None } else { self.texel(c, u, v).0 }
                }
                None => Some(c.colr),
            };
            let Some(px) = px else { prev = (x, y); continue; };
            let gc = c.gouraud.map(|_| lerp_rgb(g.0, g.1, if m == 0 { 0 } else { j * 256 / m }));
            let inside = |x: i32, y: i32| x >= cx0 && x <= cx1 && y >= cy0 && y <= cy1;
            // a diagonal step: fill the corner too
            if j > 0 && x != prev.0 && y != prev.1 && inside(x, prev.1) {
                self.plot(c, x, prev.1, px, gc);
            }
            if inside(x, y) { self.plot(c, x, y, px, gc); }
            prev = (x, y);
        }
    }

    /// The drawable box: the system clip (and the frame buffer)
    fn clip_box(&self) -> (i32, i32, i32, i32) {
        let w = if self.bpp8() { 1023 } else { 511 };
        // double interlace: coordinates span both fields (512 lines)
        let h = if self.fbcr & 8 != 0 { 511 } else { 255 };
        (0, 0, self.sys_clip.0.min(w), self.sys_clip.1.min(h))
    }

    /// One dot, with user clipping, mesh and the colour calculations
    fn plot(&mut self, c: &Cmd, x: i32, y: i32, px: u16, g: Option<u16>) {
        let pm = c.pmod;
        if pm & 0x400 != 0 {
            let (x0, y0, x1, y1) = self.user_clip;
            let inside = x >= x0 && x <= x1 && y >= y0 && y <= y1;
            if inside == (pm & 0x200 != 0) { return; }
        }
        if pm & 0x100 != 0 && (x ^ y) & 1 != 0 { return; }
        // double interlace (FBCR DIE): only this field's lines (DIL), at half height
        let y = if self.fbcr & 8 != 0 {
            if (y & 1) as u16 != (self.fbcr >> 2) & 1 { return; }
            y >> 1
        } else { y };
        let bpp8 = self.bpp8();
        let fb = if self.draw == 0 { &mut self.fb0 } else { &mut self.fb1 };
        if bpp8 {
            fb[(y as usize) * 1024 + x as usize] = px as u8;
            return;
        }
        let a = ((y as usize) * 512 + x as usize) * 2;
        let dst = u16::from_be_bytes([fb[a], fb[a + 1]]);
        let out = if pm & 0x8000 != 0 {
            dst | 0x8000
        } else if px & 0x8000 == 0 {
            px      // palette dots: no colour calculation
        } else {
            let mut s = px;
            if let Some(g) = g { s = gouraud(s, g); }
            match pm & 3 {
                1 => if dst & 0x8000 != 0 { half(dst) | 0x8000 } else { dst },
                2 => half(s) | 0x8000,
                3 => if dst & 0x8000 != 0 { average(s, dst) | 0x8000 } else { s },
                _ => s,
            }
        };
        fb[a] = (out >> 8) as u8;
        fb[a + 1] = out as u8;
    }
}

fn half(c: u16) -> u16 { (c >> 1) & 0x3DEF }

fn average(a: u16, b: u16) -> u16 { ((a & 0x7BDE) >> 1) + ((b & 0x7BDE) >> 1) }

/// Gouraud: each channel plus (table channel - 16), clamped
fn gouraud(c: u16, g: u16) -> u16 {
    let ch = |s: u16| -> u16 {
        let v = ((c >> s) & 31) as i32 + ((g >> s) & 31) as i32 - 16;
        (v.clamp(0, 31) as u16) << s
    };
    (c & 0x8000) | ch(0) | ch(5) | ch(10)
}

/// Mix two RGB555 colours, t of 256 towards b
fn lerp_rgb(a: u16, b: u16, t: i32) -> u16 {
    let ch = |s: u16| -> u16 {
        let (x, y) = (((a >> s) & 31) as i32, ((b >> s) & 31) as i32);
        ((x + (y - x) * t / 256) as u16 & 31) << s
    };
    ch(0) | ch(5) | ch(10)
}

fn bilerp(g: [u16; 4], fx: i32, fy: i32) -> u16 {
    lerp_rgb(lerp_rgb(g[0], g[1], fx), lerp_rgb(g[3], g[2], fx), fy)
}

impl Default for Vdp1 {
    fn default() -> Self { Self::new() }
}
