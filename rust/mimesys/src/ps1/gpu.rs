//! The PlayStation GPU (CXD8561), from Nocash PSX-SPX: 1 MB of VRAM (1024 x 512, 16-bit
//! pixels) that it draws into and shows a window of.
//!
//! - GP0 (1F801810 write, DMA channel 2): drawing commands and VRAM transfers.
//!   - polygons (20-3F): flat / Gouraud, textured (4 / 8 / 15-bit, CLUTs), semi-transparent,
//!     triangles and quads (two triangles)
//!   - lines (40-5F), polylines ending at 5xxx5xxx
//!   - rectangles (60-7F): variable or 1 / 8 / 16 pixel squares, textured or not
//!   - fill (02), VRAM to VRAM (80), CPU to VRAM (A0), VRAM to CPU (C0)
//!   - settings: draw mode E1, texture window E2, drawing area E3 / E4, offset E5, mask E6
//! - GP1 (1F801814 write): reset, display on / off, DMA direction, display start, ranges,
//!   display mode, and the GPU info queries (GP1 10).
//! - Pixels: texel 0000 is see-through; modulation (texel x colour / 128); semi-transparency
//!   B/2+F/2, B+F, B-F, B+F/4 (textured: only texels with bit 15); 4x4 dithering for
//!   Gouraud and modulated pixels; the mask bit (check before, set after).
//! - Rasterising uses edge functions with the top-left rule (the right and bottom edges
//!   aren't drawn, as on the console). Colours and UVs are interpolated per pixel.

use serde::{Deserialize, Serialize};

pub const VRAM_W: usize = 1024;
pub const VRAM_H: usize = 512;

const DITHER: [[i32; 4]; 4] = [[-4, 0, -3, 1], [2, -2, 3, -1], [-3, 1, -4, 0], [3, -1, 2, -2]];

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
struct Transfer {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    cx: u32,
    cy: u32,
}

impl Transfer {
    /// The next pixel's VRAM address, or None once done
    fn next(&mut self) -> Option<usize> {
        if self.cy >= self.h { return None; }
        let a = ((self.y + self.cy) as usize & (VRAM_H - 1)) * VRAM_W + ((self.x + self.cx) as usize & (VRAM_W - 1));
        self.cx += 1;
        if self.cx >= self.w { self.cx = 0; self.cy += 1; }
        Some(a)
    }
    fn done(&self) -> bool { self.cy >= self.h }
}

#[derive(Clone, Copy, Default)]
struct Vtx {
    x: i32,
    y: i32,
    r: i32,
    g: i32,
    b: i32,
    u: i32,
    v: i32,
}

#[derive(Clone, Copy, Default)]
struct Tex {
    on: bool,
    raw: bool,
    clut_x: u32,
    clut_y: u32,
    page_x: u32,
    page_y: u32,
    depth: u32,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
struct PolyLine {
    gouraud: bool,
    semi: bool,
    x: i32,
    y: i32,
    color: u32,
    /// Gouraud: the colour word for the next vertex
    next_color: Option<u32>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Gpu {
    pub vram: Vec<u16>,
    fifo: Vec<u32>,
    need: usize,
    load: Option<Transfer>,
    store: Option<Transfer>,
    poly: Option<PolyLine>,
    gpuread: u32,
    // drawing state
    /// draw mode (E1) bits 0-13, as GPUSTAT shows them
    draw_mode: u32,
    tw_mask_x: u32,
    tw_mask_y: u32,
    tw_off_x: u32,
    tw_off_y: u32,
    area_x1: i32,
    area_y1: i32,
    area_x2: i32,
    area_y2: i32,
    off_x: i32,
    off_y: i32,
    set_mask: bool,
    check_mask: bool,
    // display
    pub disp_x: u32,
    pub disp_y: u32,
    pub h_range: (u32, u32),
    pub v_range: (u32, u32),
    /// GP1(08)
    pub mode: u32,
    pub display_off: bool,
    dma_dir: u32,
    pub irq: bool,
    /// GPUSTAT bit 31 (odd line) and bit 13 (interlace field)
    pub odd_line: bool,
    pub field: bool,
}

impl Default for Gpu {
    fn default() -> Self { Self::new() }
}

impl Gpu {
    pub fn new() -> Self {
        let mut g = Self {
            vram: vec![0; VRAM_W * VRAM_H],
            fifo: Vec::with_capacity(16),
            need: 0,
            load: None,
            store: None,
            poly: None,
            gpuread: 0,
            draw_mode: 0,
            tw_mask_x: 0,
            tw_mask_y: 0,
            tw_off_x: 0,
            tw_off_y: 0,
            area_x1: 0,
            area_y1: 0,
            area_x2: 0,
            area_y2: 0,
            off_x: 0,
            off_y: 0,
            set_mask: false,
            check_mask: false,
            disp_x: 0,
            disp_y: 0,
            h_range: (0x200, 0xC00),
            v_range: (0x10, 0x100),
            mode: 0,
            display_off: true,
            dma_dir: 0,
            irq: false,
            odd_line: false,
            field: false,
        };
        g.reset();
        g
    }

    fn reset(&mut self) {
        self.fifo.clear();
        self.load = None;
        self.store = None;
        self.poly = None;
        self.draw_mode = 0;
        self.tw_mask_x = 0;
        self.tw_mask_y = 0;
        self.tw_off_x = 0;
        self.tw_off_y = 0;
        self.area_x1 = 0;
        self.area_y1 = 0;
        self.area_x2 = 0;
        self.area_y2 = 0;
        self.off_x = 0;
        self.off_y = 0;
        self.set_mask = false;
        self.check_mask = false;
        self.disp_x = 0;
        self.disp_y = 0;
        self.h_range = (0x200, 0xC00);
        self.v_range = (0x10, 0x100);
        self.mode = 0;
        self.display_off = true;
        self.dma_dir = 0;
        self.irq = false;
    }

    // ------------------------------------------------------------------ registers

    pub fn status(&self) -> u32 {
        let mut s = self.draw_mode & 0x7FF;
        s |= ((self.draw_mode >> 11) & 1) << 15;            // texture disable
        s |= (self.set_mask as u32) << 11 | (self.check_mask as u32) << 12;
        s |= (self.field as u32) << 13;
        s |= ((self.mode >> 6) & 1) << 16;
        s |= (self.mode & 3) << 17;
        s |= ((self.mode >> 2) & 1) << 19;
        s |= ((self.mode >> 3) & 1) << 20;
        s |= ((self.mode >> 4) & 1) << 21;
        s |= ((self.mode >> 5) & 1) << 22;
        s |= (self.display_off as u32) << 23;
        s |= (self.irq as u32) << 24;
        // always ready: commands, VRAM reads, DMA blocks
        let ready_cmd = self.load.is_none() && self.fifo.is_empty();
        let ready_read = self.store.is_some();
        s |= (ready_cmd as u32) << 26 | (ready_read as u32) << 27 | 1 << 28;
        s |= self.dma_dir << 29;
        let dreq = match self.dma_dir { 0 => 0, 1 => 1, 2 => 1, _ => ready_read as u32 };
        s |= dreq << 25;
        s |= (self.odd_line as u32) << 31;
        s
    }

    pub fn read(&mut self) -> u32 {
        if let Some(mut t) = self.store {
            let mut v = 0u32;
            for k in 0..2 {
                if let Some(a) = t.next() { v |= (self.vram[a] as u32) << (16 * k); }
            }
            self.store = if t.done() { None } else { Some(t) };
            self.gpuread = v;
        }
        self.gpuread
    }

    pub fn gp1(&mut self, v: u32) {
        match v >> 24 {
            0x00 => self.reset(),
            0x01 => { self.fifo.clear(); self.load = None; self.poly = None; }
            0x02 => self.irq = false,
            0x03 => self.display_off = v & 1 != 0,
            0x04 => self.dma_dir = v & 3,
            0x05 => { self.disp_x = v & 0x3FE; self.disp_y = (v >> 10) & 0x1FF; }
            0x06 => self.h_range = (v & 0xFFF, (v >> 12) & 0xFFF),
            0x07 => self.v_range = (v & 0x3FF, (v >> 10) & 0x3FF),
            0x08 => self.mode = v & 0x7F,
            0x09 => self.draw_mode = (self.draw_mode & !0x800) | (v & 1) << 11,
            0x10..=0x1F => {
                self.gpuread = match v & 7 {
                    2 => self.tw_mask_x | self.tw_mask_y << 5 | self.tw_off_x << 10 | self.tw_off_y << 15,
                    3 => (self.area_x1 as u32) | (self.area_y1 as u32) << 10,
                    4 => (self.area_x2 as u32) | (self.area_y2 as u32) << 10,
                    5 => (self.off_x as u32 & 0x7FF) | (self.off_y as u32 & 0x7FF) << 11,
                    7 => 2,
                    _ => self.gpuread,
                };
            }
            _ => {}
        }
    }

    /// The width of the picture, from the display mode
    pub fn display_width(&self) -> usize {
        if self.mode & 0x40 != 0 { return 368; }
        [256, 320, 512, 640][(self.mode & 3) as usize]
    }

    /// GPU clocks per dot (counter 0's dot clock)
    pub fn dot_div(&self) -> u32 {
        if self.mode & 0x40 != 0 { return 7; }
        [10, 8, 5, 4][(self.mode & 3) as usize]
    }

    pub fn interlaced_480(&self) -> bool { self.mode & 0x24 == 0x24 }

    pub fn pal(&self) -> bool { self.mode & 8 != 0 }

    pub fn display_height(&self) -> usize {
        let lines = self.v_range.1.saturating_sub(self.v_range.0) as usize;
        let max = if self.pal() { 288 } else { 240 };
        let lines = lines.clamp(1, max);
        if self.interlaced_480() { lines * 2 } else { lines }
    }

    /// The display window as RGBA
    pub fn render(&self, out: &mut Vec<u8>) -> (usize, usize) {
        let w = self.display_width();
        let h = self.display_height();
        out.resize(w * h * 4, 0);
        if self.display_off {
            out.fill(0);
            for p in out.chunks_exact_mut(4) { p[3] = 0xFF; }
            return (w, h);
        }
        let rgb24 = self.mode & 0x10 != 0;
        for y in 0..h {
            let vy = (self.disp_y as usize + y) & (VRAM_H - 1);
            let row = &self.vram[vy * VRAM_W..vy * VRAM_W + VRAM_W];
            let o = &mut out[y * w * 4..(y + 1) * w * 4];
            if rgb24 {
                for x in 0..w {
                    let byte = self.disp_x as usize * 2 + x * 3;
                    let b = |i: usize| -> u8 {
                        let px = row[(i / 2) & (VRAM_W - 1)];
                        if i & 1 == 0 { px as u8 } else { (px >> 8) as u8 }
                    };
                    o[x * 4..x * 4 + 4].copy_from_slice(&[b(byte), b(byte + 1), b(byte + 2), 0xFF]);
                }
            } else {
                for x in 0..w {
                    let px = row[(self.disp_x as usize + x) & (VRAM_W - 1)];
                    let c = |s: u16| -> u8 { let v = ((px >> s) & 31) as u8; v << 3 | v >> 2 };
                    o[x * 4..x * 4 + 4].copy_from_slice(&[c(0), c(5), c(10), 0xFF]);
                }
            }
        }
        (w, h)
    }

    // ------------------------------------------------------------------ GP0

    fn cmd_len(c: u32) -> usize {
        match c {
            0x02 => 3,
            0x20..=0x3F => {
                let verts = if c & 8 != 0 { 4 } else { 3 };
                let tex = (c & 4 != 0) as usize;
                let gouraud = c & 0x10 != 0;
                1 + verts * (1 + tex) + if gouraud { verts - 1 } else { 0 }
            }
            0x40..=0x5F => if c & 0x10 != 0 { 4 } else { 3 },
            0x60..=0x7F => 2 + ((c & 4 != 0) as usize) + ((c & 0x18 == 0) as usize),
            0x80..=0x9F => 4,
            0xA0..=0xDF => 3,
            _ => 1,
        }
    }

    pub fn gp0(&mut self, v: u32) {
        if let Some(mut t) = self.load {
            for k in 0..2 {
                if let Some(a) = t.next() {
                    let px = (v >> (16 * k)) as u16;
                    if !(self.check_mask && self.vram[a] & 0x8000 != 0) {
                        self.vram[a] = px | (self.set_mask as u16) << 15;
                    }
                }
            }
            self.load = if t.done() { None } else { Some(t) };
            return;
        }
        if self.poly.is_some() {
            self.polyline_word(v);
            return;
        }
        if self.fifo.is_empty() { self.need = Self::cmd_len(v >> 24); }
        self.fifo.push(v);
        if self.fifo.len() >= self.need {
            let words = std::mem::take(&mut self.fifo);
            self.execute(&words);
            self.fifo = words;
            self.fifo.clear();
        }
    }

    fn execute(&mut self, w: &[u32]) {
        let c = w[0] >> 24;
        match c {
            0x02 => self.fill(w),
            0x20..=0x3F => self.polygon(w),
            0x40..=0x5F => self.line_cmd(w),
            0x60..=0x7F => self.rect(w),
            0x80..=0x9F => self.copy(w),
            0xA0..=0xBF => {
                let (x, y, wd, h) = Self::xywh(w[1], w[2]);
                self.load = Some(Transfer { x, y, w: wd, h, cx: 0, cy: 0 });
            }
            0xC0..=0xDF => {
                let (x, y, wd, h) = Self::xywh(w[1], w[2]);
                self.store = Some(Transfer { x, y, w: wd, h, cx: 0, cy: 0 });
            }
            0xE1 => self.draw_mode = (self.draw_mode & 0x800) | (w[0] & 0x37FF),
            0xE2 => {
                let v = w[0];
                self.tw_mask_x = v & 31;
                self.tw_mask_y = (v >> 5) & 31;
                self.tw_off_x = (v >> 10) & 31;
                self.tw_off_y = (v >> 15) & 31;
            }
            0xE3 => { self.area_x1 = (w[0] & 0x3FF) as i32; self.area_y1 = ((w[0] >> 10) & 0x1FF) as i32; }
            0xE4 => { self.area_x2 = (w[0] & 0x3FF) as i32; self.area_y2 = ((w[0] >> 10) & 0x1FF) as i32; }
            0xE5 => {
                self.off_x = ((w[0] as i32) << 21) >> 21;
                self.off_y = ((w[0] as i32) << 10) >> 21;
            }
            0xE6 => { self.set_mask = w[0] & 1 != 0; self.check_mask = w[0] & 2 != 0; }
            0x1F => self.irq = true,
            _ => {}
        }
    }

    fn xywh(xy: u32, wh: u32) -> (u32, u32, u32, u32) {
        let x = xy & 0x3FF;
        let y = (xy >> 16) & 0x1FF;
        let w = ((wh & 0xFFFF).wrapping_sub(1) & 0x3FF) + 1;
        let h = ((wh >> 16).wrapping_sub(1) & 0x1FF) + 1;
        (x, y, w, h)
    }

    fn rgb15(c: u32) -> u16 {
        ((c >> 3) & 31 | ((c >> 11) & 31) << 5 | ((c >> 19) & 31) << 10) as u16
    }

    fn fill(&mut self, w: &[u32]) {
        let color = Self::rgb15(w[0]);
        let x0 = w[1] & 0x3F0;
        let y0 = (w[1] >> 16) & 0x1FF;
        let wd = ((w[2] & 0x3FF) + 15) & !15;
        let h = (w[2] >> 16) & 0x1FF;
        for y in 0..h {
            let yy = ((y0 + y) as usize) & (VRAM_H - 1);
            for x in 0..wd {
                self.vram[yy * VRAM_W + (((x0 + x) as usize) & (VRAM_W - 1))] = color;
            }
        }
    }

    fn copy(&mut self, w: &[u32]) {
        let (sx, sy, wd, h) = Self::xywh(w[1], w[3]);
        let (dx, dy, _, _) = Self::xywh(w[2], w[3]);
        for y in 0..h {
            for x in 0..wd {
                let s = (((sy + y) as usize) & (VRAM_H - 1)) * VRAM_W + (((sx + x) as usize) & (VRAM_W - 1));
                let d = (((dy + y) as usize) & (VRAM_H - 1)) * VRAM_W + (((dx + x) as usize) & (VRAM_W - 1));
                if self.check_mask && self.vram[d] & 0x8000 != 0 { continue; }
                self.vram[d] = self.vram[s] | (self.set_mask as u16) << 15;
            }
        }
    }

    // ------------------------------------------------------------------ pixels

    fn texel(&self, t: &Tex, u: i32, v: i32) -> u16 {
        let u = ((u as u32 & 0xFF) & !(self.tw_mask_x * 8)) | ((self.tw_off_x & self.tw_mask_x) * 8);
        let v = ((v as u32 & 0xFF) & !(self.tw_mask_y * 8)) | ((self.tw_off_y & self.tw_mask_y) * 8);
        let row = ((t.page_y + v) as usize & (VRAM_H - 1)) * VRAM_W;
        let vx = |x: u32| (x as usize) & (VRAM_W - 1);
        let clut = (t.clut_y as usize & (VRAM_H - 1)) * VRAM_W;
        match t.depth {
            0 => {
                let w = self.vram[row + vx(t.page_x + u / 4)];
                let i = (w >> ((u & 3) * 4)) & 0xF;
                self.vram[clut + vx(t.clut_x + i as u32)]
            }
            1 => {
                let w = self.vram[row + vx(t.page_x + u / 2)];
                let i = (w >> ((u & 1) * 8)) & 0xFF;
                self.vram[clut + vx(t.clut_x + i as u32)]
            }
            _ => self.vram[row + vx(t.page_x + u)],
        }
    }

    /// One pixel: colour in 8-bit RGB (Gouraud / flat / modulation colour)
    #[allow(clippy::too_many_arguments)]
    fn pixel(&mut self, x: i32, y: i32, r: i32, g: i32, b: i32, tex: &Tex, u: i32, v: i32, semi: bool, dither: bool) {
        if x < self.area_x1 || x > self.area_x2 || y < self.area_y1 || y > self.area_y2 { return; }
        let a = (y as usize & (VRAM_H - 1)) * VRAM_W + (x as usize & (VRAM_W - 1));
        let back = self.vram[a];
        if self.check_mask && back & 0x8000 != 0 { return; }
        let (mut cr, mut cg, mut cb, mask, blend) = if tex.on {
            let t = self.texel(tex, u, v);
            if t == 0 { return; }
            let tr = (t & 31) as i32;
            let tg = ((t >> 5) & 31) as i32;
            let tb = ((t >> 10) & 31) as i32;
            if tex.raw {
                (tr << 3, tg << 3, tb << 3, t & 0x8000, semi && t & 0x8000 != 0)
            } else {
                ((tr << 3) * r >> 7, (tg << 3) * g >> 7, (tb << 3) * b >> 7, t & 0x8000, semi && t & 0x8000 != 0)
            }
        } else {
            (r, g, b, 0, semi)
        };
        if dither && self.draw_mode & 0x200 != 0 {
            let d = DITHER[(y & 3) as usize][(x & 3) as usize];
            cr += d;
            cg += d;
            cb += d;
        }
        let q = |c: i32| (c.clamp(0, 255) >> 3) as u16;
        let (mut fr, mut fg, mut fb) = (q(cr), q(cg), q(cb));
        if blend {
            let br = back & 31;
            let bg = (back >> 5) & 31;
            let bb = (back >> 10) & 31;
            let f = |b: u16, f: u16| -> u16 {
                let (b, f) = (b as i32, f as i32);
                (match (self.draw_mode >> 5) & 3 {
                    0 => (b + f) >> 1,
                    1 => (b + f).min(31),
                    2 => (b - f).max(0),
                    _ => (b + (f >> 2)).min(31),
                }) as u16
            };
            fr = f(br, fr);
            fg = f(bg, fg);
            fb = f(bb, fb);
        }
        self.vram[a] = fr | fg << 5 | fb << 10 | mask | (self.set_mask as u16) << 15;
    }

    // ------------------------------------------------------------------ polygons

    fn sxy(w: u32) -> (i32, i32) {
        (((w as i32) << 21) >> 21, ((w as i32) << 5) >> 21)
    }

    fn polygon(&mut self, w: &[u32]) {
        let c = w[0] >> 24;
        let gouraud = c & 0x10 != 0;
        let quad = c & 8 != 0;
        let textured = c & 4 != 0;
        let semi = c & 2 != 0;
        let raw = c & 1 != 0;
        let n = if quad { 4 } else { 3 };
        let mut vs = [Vtx::default(); 4];
        let mut k = 0;
        let mut clut = 0;
        let mut page = self.draw_mode;
        for (i, vt) in vs.iter_mut().enumerate().take(n) {
            let color = if i == 0 || !gouraud { w[0] } else { k += 1; w[k] };
            k += 1;
            let (x, y) = Self::sxy(w[k]);
            vt.x = x;
            vt.y = y;
            vt.r = (color & 0xFF) as i32;
            vt.g = ((color >> 8) & 0xFF) as i32;
            vt.b = ((color >> 16) & 0xFF) as i32;
            if textured {
                k += 1;
                let uv = w[k];
                vt.u = (uv & 0xFF) as i32;
                vt.v = ((uv >> 8) & 0xFF) as i32;
                if i == 0 { clut = uv >> 16; }
                if i == 1 { page = uv >> 16; }
            }
        }
        if textured {
            // the polygon's page becomes the draw mode's
            self.draw_mode = (self.draw_mode & !0x9FF) | (page & 0x9FF);
        }
        let tex = Tex {
            on: textured && self.draw_mode & 0x800 == 0,
            raw,
            clut_x: (clut & 0x3F) * 16,
            clut_y: (clut >> 6) & 0x1FF,
            page_x: (page & 0xF) * 64,
            page_y: ((page >> 4) & 1) * 256,
            depth: (page >> 7) & 3,
        };
        let dither = gouraud || (textured && !raw);
        for v in vs.iter_mut().take(n) { v.x += self.off_x; v.y += self.off_y; }
        self.triangle([vs[0], vs[1], vs[2]], gouraud, &tex, semi, dither);
        if quad { self.triangle([vs[1], vs[2], vs[3]], gouraud, &tex, semi, dither); }
    }

    fn triangle(&mut self, v: [Vtx; 3], gouraud: bool, tex: &Tex, semi: bool, dither: bool) {
        let minx = v[0].x.min(v[1].x).min(v[2].x);
        let maxx = v[0].x.max(v[1].x).max(v[2].x);
        let miny = v[0].y.min(v[1].y).min(v[2].y);
        let maxy = v[0].y.max(v[1].y).max(v[2].y);
        // too big: the GPU skips it
        if maxx - minx >= 1024 || maxy - miny >= 512 { return; }
        let mut v = v;
        let mut area = Self::edge(&v[0], &v[1], v[2].x, v[2].y);
        if area == 0 { return; }
        if area < 0 { v.swap(1, 2); area = -area; }
        let x0 = minx.max(self.area_x1);
        let x1 = maxx.min(self.area_x2 + 1);
        let y0 = miny.max(self.area_y1);
        let y1 = maxy.min(self.area_y2 + 1);
        let tl = |a: &Vtx, b: &Vtx| (a.y == b.y && b.x > a.x) || b.y < a.y;
        let tl12 = tl(&v[1], &v[2]);
        let tl20 = tl(&v[2], &v[0]);
        let tl01 = tl(&v[0], &v[1]);
        let area64 = area as i64;
        let lerp = |w0: i64, w1: i64, w2: i64, a: i32, b: i32, c: i32| -> i32 {
            ((w0 * a as i64 + w1 * b as i64 + w2 * c as i64) / area64) as i32
        };
        for y in y0..y1 {
            for x in x0..x1 {
                let w0 = Self::edge(&v[1], &v[2], x, y);
                let w1 = Self::edge(&v[2], &v[0], x, y);
                let w2 = Self::edge(&v[0], &v[1], x, y);
                if w0 < 0 || w1 < 0 || w2 < 0 { continue; }
                if (w0 == 0 && !tl12) || (w1 == 0 && !tl20) || (w2 == 0 && !tl01) { continue; }
                let (w0, w1, w2) = (w0 as i64, w1 as i64, w2 as i64);
                let (r, g, b) = if gouraud {
                    (lerp(w0, w1, w2, v[0].r, v[1].r, v[2].r), lerp(w0, w1, w2, v[0].g, v[1].g, v[2].g), lerp(w0, w1, w2, v[0].b, v[1].b, v[2].b))
                } else {
                    (v[0].r, v[0].g, v[0].b)
                };
                let (u, tv) = if tex.on {
                    (lerp(w0, w1, w2, v[0].u, v[1].u, v[2].u), lerp(w0, w1, w2, v[0].v, v[1].v, v[2].v))
                } else {
                    (0, 0)
                };
                self.pixel(x, y, r, g, b, tex, u, tv, semi, dither);
            }
        }
    }

    #[inline]
    fn edge(a: &Vtx, b: &Vtx, x: i32, y: i32) -> i32 {
        (b.x - a.x) * (y - a.y) - (b.y - a.y) * (x - a.x)
    }

    // ------------------------------------------------------------------ rectangles

    fn rect(&mut self, w: &[u32]) {
        let c = w[0] >> 24;
        let textured = c & 4 != 0;
        let semi = c & 2 != 0;
        let raw = c & 1 != 0;
        let (mut x, mut y) = Self::sxy(w[1]);
        x += self.off_x;
        y += self.off_y;
        let mut k = 2;
        let (mut u0, mut v0, mut clut) = (0, 0, 0);
        if textured {
            u0 = (w[k] & 0xFF) as i32;
            v0 = ((w[k] >> 8) & 0xFF) as i32;
            clut = w[k] >> 16;
            k += 1;
        }
        let (wd, h) = match (c >> 3) & 3 {
            0 => ((w[k] & 0x3FF) as i32, ((w[k] >> 16) & 0x1FF) as i32),
            1 => (1, 1),
            2 => (8, 8),
            _ => (16, 16),
        };
        let page = self.draw_mode;
        let tex = Tex {
            on: textured && page & 0x800 == 0,
            raw,
            clut_x: (clut & 0x3F) * 16,
            clut_y: (clut >> 6) & 0x1FF,
            page_x: (page & 0xF) * 64,
            page_y: ((page >> 4) & 1) * 256,
            depth: (page >> 7) & 3,
        };
        let (r, g, b) = ((w[0] & 0xFF) as i32, ((w[0] >> 8) & 0xFF) as i32, ((w[0] >> 16) & 0xFF) as i32);
        let fx = page & 0x1000 != 0;
        let fy = page & 0x2000 != 0;
        for dy in 0..h {
            for dx in 0..wd {
                let u = if fx { u0 - dx } else { u0 + dx };
                let v = if fy { v0 - dy } else { v0 + dy };
                self.pixel(x + dx, y + dy, r, g, b, &tex, u, v, semi, false);
            }
        }
    }

    // ------------------------------------------------------------------ lines

    fn line_cmd(&mut self, w: &[u32]) {
        let c = w[0] >> 24;
        let gouraud = c & 0x10 != 0;
        let semi = c & 2 != 0;
        let (c0, p0, c1, p1) = if gouraud { (w[0], w[1], w[2], w[3]) } else { (w[0], w[1], w[0], w[2]) };
        let (x0, y0) = Self::sxy(p0);
        let (x1, y1) = Self::sxy(p1);
        self.line(x0 + self.off_x, y0 + self.off_y, c0, x1 + self.off_x, y1 + self.off_y, c1, gouraud, semi);
        if c & 8 != 0 {
            self.poly = Some(PolyLine { gouraud, semi, x: x1, y: y1, color: c1, next_color: None });
        }
    }

    fn polyline_word(&mut self, v: u32) {
        let Some(mut p) = self.poly else { return };
        if v & 0xF000_F000 == 0x5000_5000 {
            self.poly = None;
            return;
        }
        if p.gouraud && p.next_color.is_none() {
            p.next_color = Some(v);
            self.poly = Some(p);
            return;
        }
        let color = if p.gouraud { p.next_color.take().unwrap() } else { p.color };
        let (x, y) = Self::sxy(v);
        self.line(p.x + self.off_x, p.y + self.off_y, p.color, x + self.off_x, y + self.off_y, color, p.gouraud, p.semi);
        p.x = x;
        p.y = y;
        p.color = color;
        self.poly = Some(p);
    }

    #[allow(clippy::too_many_arguments)]
    fn line(&mut self, x0: i32, y0: i32, c0: u32, x1: i32, y1: i32, c1: u32, gouraud: bool, semi: bool) {
        let dx = x1 - x0;
        let dy = y1 - y0;
        if dx.abs() >= 1024 || dy.abs() >= 512 { return; }
        let steps = dx.abs().max(dy.abs());
        let ch = |c: u32, s: u32| ((c >> s) & 0xFF) as i32;
        let tex = Tex::default();
        for i in 0..=steps {
            // nearest pixel along the line (steps > 0: rounded half up)
            let rnd = |d: i32| if steps == 0 { 0 } else { (2 * d * i + steps).div_euclid(2 * steps) };
            let (x, y) = (x0 + rnd(dx), y0 + rnd(dy));
            let (r, g, b) = if gouraud && steps > 0 {
                (ch(c0, 0) + (ch(c1, 0) - ch(c0, 0)) * i / steps,
                 ch(c0, 8) + (ch(c1, 8) - ch(c0, 8)) * i / steps,
                 ch(c0, 16) + (ch(c1, 16) - ch(c0, 16)) * i / steps)
            } else {
                (ch(c0, 0), ch(c0, 8), ch(c0, 16))
            };
            self.pixel(x, y, r, g, b, &tex, 0, 0, semi, gouraud);
        }
    }
}
