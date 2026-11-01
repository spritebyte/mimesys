//! The DS 3D engine: the geometry engine (commands, matrices, lighting, clipping) and the
//! rendering engine (a scanline rasterizer). Its picture is engine A's BG0.
//!
//! Commands arrive through the GX FIFO (0x04000400, packed: one word of up to four command
//! bytes, then their parameters) or the command ports (0x04000440 + cmd * 4 - 0x100, one
//! parameter per write). They run as soon as all their parameters are in, except after
//! SWAP_BUFFERS: then the engine waits for vblank and later commands queue up, as on the
//! console. At vblank the polygon list becomes the one being drawn and the frame is
//! rendered in one go (the console draws it line by line during the next frame).
//!
//! Numbers: matrices and coordinates are 20.12 fixed point (4096 = 1.0); colours 5 bits;
//! texture coordinates 12.4 (1/16 texel). Clipping and rasterizing use f32.
//!
//! Done: all geometry commands, the matrix stacks, lighting with the shininess table,
//! texture coordinate generation, frustum clipping (far-plane option), culling, the 2048
//! polygon / 6144 vertex limits, opaque/translucent sorting, z and w depth, all 7 texture
//! formats with repeat/flip/clamp, modulate/decal/toon/highlight, alpha test and blending,
//! translucent polygon IDs, wireframe, fog, box/position/vector tests, shadow polygons
//! (stencil masks), edge marking, anti-aliasing.
//! Anti-aliasing smooths object outlines (by edge distance, blended with the layer behind).
//! Not yet: the rear-plane bitmap,
//! exact hardware interpolation and depth rounding.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use crate::nds::nds_vram::{Region, Vram};

pub const W: usize = 256;
pub const H: usize = 192;
type Mat = [[i32; 4]; 4];
const IDENT: Mat = [[4096, 0, 0, 0], [0, 4096, 0, 0], [0, 0, 4096, 0], [0, 0, 0, 4096]];
const MAX_POLYS: usize = 2048;
const MAX_VERTS: usize = 6144;

fn mul(a: &Mat, b: &Mat) -> Mat {
    let mut r = [[0; 4]; 4];
    for i in 0..4 {
        for j in 0..4 {
            let mut s = 0i64;
            for k in 0..4 { s += a[i][k] as i64 * b[k][j] as i64; }
            r[i][j] = (s >> 12) as i32;
        }
    }
    r
}

/// Parameter words a command takes (0 for unknown commands).
pub fn param_count(cmd: u8) -> usize {
    match cmd {
        0x10 | 0x12 | 0x13 | 0x14 => 1,
        0x16 | 0x18 => 16,
        0x17 | 0x19 => 12,
        0x1A => 9,
        0x1B | 0x1C => 3,
        0x20..=0x22 | 0x24..=0x2B | 0x30..=0x33 | 0x40 | 0x50 | 0x60 | 0x72 => 1,
        0x23 | 0x71 => 2,
        0x34 => 32,
        0x70 => 3,
        _ => 0,
    }
}

fn valid(cmd: u8) -> bool { matches!(cmd, 0x10..=0x1C | 0x20..=0x2B | 0x30..=0x34 | 0x40 | 0x41 | 0x50 | 0x60 | 0x70..=0x72) }

/// 5-bit to 6-bit colour channel, as the hardware expands it.
fn c6(c: u32) -> u32 { if c == 0 { 0 } else { c * 2 + 1 } }
fn rgb6(c: u16) -> [u32; 3] { [c6(c as u32 & 31), c6((c as u32 >> 5) & 31), c6((c as u32 >> 10) & 31)] }

/// A vertex: clip space while clipping, screen space (x, y in pixels, z = z/w) after.
#[derive(Clone, Copy, Default, Serialize, Deserialize)]
pub struct Vtx {
    pub x: f32, pub y: f32, pub z: f32, pub w: f32,
    /// colour 0-63 per channel
    pub r: f32, pub g: f32, pub b: f32,
    /// texture coordinates in texels
    pub s: f32, pub t: f32,
}

impl Vtx {
    fn lerp(&self, o: &Vtx, f: f32) -> Vtx {
        let l = |a: f32, b: f32| a + (b - a) * f;
        Vtx { x: l(self.x, o.x), y: l(self.y, o.y), z: l(self.z, o.z), w: l(self.w, o.w),
              r: l(self.r, o.r), g: l(self.g, o.g), b: l(self.b, o.b), s: l(self.s, o.s), t: l(self.t, o.t) }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Poly {
    pub v: Vec<Vtx>,
    pub attr: u32,
    pub tex: u32,
    pub pal: u32,
    pub translucent: bool,
    pub ymin: f32,
    pub ymax: f32,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Gpu3d {
    mtx_mode: u8,
    proj: Mat, pos: Mat, vec: Mat, tex: Mat, clip: Mat,
    proj_stack: Mat, proj_sp: u8,
    pos_stack: Vec<Mat>, vec_stack: Vec<Mat>, pos_sp: u8,
    tex_stack: Mat, tex_sp: u8,
    stack_error: bool,
    // command input
    cmd: u8,
    params: Vec<u32>,
    packed: u32,
    packed_left: usize,
    queue: VecDeque<(u8, u32)>,
    pub swap_pending: bool,
    swap_param: u32,
    // vertex state
    color: u16,
    raw_st: [i16; 2],
    st: [i32; 2],
    last: [i16; 3],
    poly_attr: u32,
    cur_attr: u32,
    tex_param: u32,
    pltt_base: u32,
    prim: u8,
    in_begin: bool,
    strip: Vec<Vtx>,
    odd: bool,
    dif_amb: u32,
    spe_emi: u32,
    light_dir: [[i32; 3]; 4],
    spec_recip: [i32; 4],
    light_color: [u16; 4],
    shininess: Vec<u8>,
    viewport: [u8; 4],
    // polygon lists: being built, being drawn
    polys: Vec<Poly>,
    vert_count: usize,
    render_polys: Vec<Poly>,
    render_param: u32,
    // test results
    box_result: bool,
    pos_result: [i32; 4],
    vec_result: [i16; 3],
    /// the rendered frame: BGR555 in bits 0-14, alpha (0 = nothing) in bits 16-20
    #[serde(skip, default = "blank")]
    pub frame: Vec<u32>,
    /// render again at the next vblank (lists swapped or a 3D register written)
    #[serde(skip)]
    pub dirty: bool,
}

fn blank() -> Vec<u32> { vec![0; W * H] }

impl Default for Gpu3d { fn default() -> Self { Self::new() } }

impl Gpu3d {
    pub fn new() -> Self {
        Self {
            mtx_mode: 0, proj: IDENT, pos: IDENT, vec: IDENT, tex: IDENT, clip: IDENT,
            proj_stack: IDENT, proj_sp: 0, pos_stack: vec![IDENT; 32], vec_stack: vec![IDENT; 32], pos_sp: 0,
            tex_stack: IDENT, tex_sp: 0, stack_error: false,
            cmd: 0, params: Vec::new(), packed: 0, packed_left: 0, queue: VecDeque::new(),
            swap_pending: false, swap_param: 0,
            color: 0x7FFF, raw_st: [0; 2], st: [0; 2], last: [0; 3], poly_attr: 0, cur_attr: 0, tex_param: 0, pltt_base: 0,
            prim: 0, in_begin: false, strip: Vec::new(), odd: false,
            dif_amb: 0, spe_emi: 0, light_dir: [[0; 3]; 4], spec_recip: [0; 4], light_color: [0; 4], shininess: vec![0; 128],
            viewport: [0, 0, 255, 191],
            polys: Vec::new(), vert_count: 0, render_polys: Vec::new(), render_param: 0,
            box_result: false, pos_result: [0; 4], vec_result: [0; 3],
            frame: blank(), dirty: false,
        }
    }

    // ------------------------------------------------------------ input

    /// A write to the GX FIFO (0x400-0x43F) or a command port (0x440-0x5FF).
    pub fn write(&mut self, off: u32, v: u32) {
        if off < 0x440 {
            self.write_packed(v);
        } else {
            let cmd = ((off - 0x400) >> 2) as u8;
            if valid(cmd) { self.push(cmd, v); }
        }
    }

    fn write_packed(&mut self, v: u32) {
        if self.packed_left > 0 {
            self.push(self.packed as u8, v);
            self.packed_left -= 1;
            if self.packed_left == 0 { self.packed >>= 8; self.next_packed(); }
            return;
        }
        self.packed = v;
        self.next_packed();
    }

    /// Run the packet's zero-parameter commands up to the next one that needs parameters.
    fn next_packed(&mut self) {
        while self.packed != 0 {
            let cmd = self.packed as u8;
            let n = if valid(cmd) { param_count(cmd) } else { 0 };
            if n > 0 { self.packed_left = n; return; }
            if valid(cmd) { self.push(cmd, 0); }
            self.packed >>= 8;
        }
    }

    /// One FIFO entry (command, parameter).
    fn push(&mut self, cmd: u8, p: u32) {
        if self.swap_pending || !self.queue.is_empty() {
            self.queue.push_back((cmd, p));
        } else {
            self.feed(cmd, p);
        }
    }

    fn feed(&mut self, cmd: u8, p: u32) {
        if cmd != self.cmd { self.params.clear(); self.cmd = cmd; }
        self.params.push(p);
        if self.params.len() >= param_count(cmd).max(1) {
            let params = std::mem::take(&mut self.params);
            self.exec(cmd, &params);
        }
    }

    pub fn fifo_len(&self) -> usize { self.queue.len() }

    /// Vblank: swap in the new polygon list if asked, then let queued commands run.
    /// Returns true when there's a new frame to draw.
    pub fn vblank(&mut self) -> bool {
        if !self.swap_pending { return false; }
        self.swap_pending = false;
        self.render_polys = std::mem::take(&mut self.polys);
        self.render_param = self.swap_param;
        self.vert_count = 0;
        while !self.swap_pending {
            let Some((cmd, p)) = self.queue.pop_front() else { break };
            self.feed(cmd, p);
        }
        self.dirty = true;
        true
    }

    pub fn gxstat(&self, irq_mode: u32) -> u32 {
        let n = self.queue.len().min(256) as u32;
        (self.box_result as u32) << 1
            | ((self.pos_sp & 31) as u32) << 8 | (self.proj_sp as u32 & 1) << 13 | (self.stack_error as u32) << 14
            | n << 16 | ((n >= 256) as u32) << 24 | ((n < 128) as u32) << 25 | ((n == 0) as u32) << 26
            | ((self.swap_pending || n > 0) as u32) << 27 | irq_mode << 30
    }

    pub fn ack_error(&mut self) { self.stack_error = false; }

    /// The last vertex's raw coordinates (tests).
    #[cfg(test)]
    pub fn last_vertex(&self) -> [i16; 3] { self.last }

    /// The current vertex colour (tests).
    #[cfg(test)]
    pub fn read_color(&self) -> u16 { self.color }

    /// RAM_COUNT, the test results and the matrix reads (0x604-0x6A3).
    pub fn read(&self, off: u32) -> u32 {
        match off {
            0x604 => self.polys.len() as u32 | (self.vert_count as u32) << 16,
            0x620..=0x62F => self.pos_result[((off - 0x620) / 4) as usize] as u32,
            0x630..=0x637 => {
                let i = ((off - 0x630) / 2) as usize;
                let lo = self.vec_result.get(i).copied().unwrap_or(0) as u16 as u32;
                let hi = self.vec_result.get(i + 1).copied().unwrap_or(0) as u16 as u32;
                lo | hi << 16
            }
            0x640..=0x67F => { let i = ((off - 0x640) / 4) as usize; self.clip[i / 4][i % 4] as u32 }
            0x680..=0x6A3 => { let i = ((off - 0x680) / 4) as usize; self.vec[i / 3][i % 3] as u32 }
            _ => 0,
        }
    }

    // ------------------------------------------------------------ commands

    fn exec(&mut self, cmd: u8, p: &[u32]) {
        match cmd {
            0x10 => self.mtx_mode = (p[0] & 3) as u8,
            0x11 => self.mtx_push(),
            0x12 => self.mtx_pop(((p[0] << 26) as i32) >> 26),
            0x13 => self.mtx_store(p[0] & 31),
            0x14 => self.mtx_restore(p[0] & 31),
            0x15 => self.load(IDENT, true),
            0x16 => { let m = std::array::from_fn(|i| std::array::from_fn(|j| p[i * 4 + j] as i32)); self.load(m, true) }
            0x17 => self.load(m43(p), true),
            0x18 => { let m = std::array::from_fn(|i| std::array::from_fn(|j| p[i * 4 + j] as i32)); self.mult(m, true) }
            0x19 => self.mult(m43(p), true),
            0x1A => {
                let mut m = IDENT;
                for i in 0..3 { for j in 0..3 { m[i][j] = p[i * 3 + j] as i32; } }
                self.mult(m, true)
            }
            0x1B => {
                let mut m = IDENT;
                for i in 0..3 { m[i][i] = p[i] as i32; }
                self.mult(m, false)                       // scaling leaves the vector matrix
            }
            0x1C => {
                let mut m = IDENT;
                for j in 0..3 { m[3][j] = p[j] as i32; }
                self.mult(m, true)
            }
            0x20 => self.color = (p[0] & 0x7FFF) as u16,
            0x21 => self.normal(p[0]),
            0x22 => self.texcoord(p[0]),
            0x23 => self.vertex(p[0] as i16, (p[0] >> 16) as i16, p[1] as i16),
            0x24 => {
                let f = |sh: u32| (((p[0] >> sh) << 22) as i32 >> 22 << 6) as i16;
                self.vertex(f(0), f(10), f(20))
            }
            0x25 => self.vertex(p[0] as i16, (p[0] >> 16) as i16, self.last[2]),
            0x26 => self.vertex(p[0] as i16, self.last[1], (p[0] >> 16) as i16),
            0x27 => self.vertex(self.last[0], p[0] as i16, (p[0] >> 16) as i16),
            0x28 => {
                let f = |sh: u32| (((p[0] >> sh) << 22) as i32) >> 22;      // added in 4.12 units as is
                let l = self.last;
                self.vertex((l[0] as i32 + f(0)) as i16, (l[1] as i32 + f(10)) as i16, (l[2] as i32 + f(20)) as i16)
            }
            0x29 => self.poly_attr = p[0],
            0x2A => self.tex_param = p[0],
            0x2B => self.pltt_base = p[0] & 0x1FFF,
            0x30 => { self.dif_amb = p[0]; if p[0] & 0x8000 != 0 { self.color = (p[0] & 0x7FFF) as u16; } }
            0x31 => self.spe_emi = p[0],
            0x32 => {
                // stored negated, as signed 11-bit, plus the specular reciprocal
                let d = [p[0], p[0] >> 10, p[0] >> 20].map(|v| ((v << 22) as i32) >> 22);
                let l = (p[0] >> 30) as usize;
                let dot = |j: usize| d[0].wrapping_mul(self.vec[0][j]).wrapping_add(d[1].wrapping_mul(self.vec[1][j]))
                    .wrapping_add(d[2].wrapping_mul(self.vec[2][j]));
                for j in 0..3 { self.light_dir[l][j] = ((-(dot(j) >> 12)) << 21) >> 21; }
                let den = -((dot(2) << 9) >> 21) + (1 << 9);
                self.spec_recip[l] = if den == 0 { 0 } else { (1 << 18) / den };
            }
            0x33 => self.light_color[(p[0] >> 30) as usize] = (p[0] & 0x7FFF) as u16,
            0x34 => for (i, w) in p.iter().enumerate() { self.shininess[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes()); },
            0x40 => {
                self.prim = (p[0] & 3) as u8;
                self.strip.clear();
                self.odd = false;
                self.cur_attr = self.poly_attr;
                self.in_begin = true;
            }
            0x41 => {}
            0x50 => { self.swap_pending = true; self.swap_param = p[0] & 3; }
            0x60 => self.viewport = p[0].to_le_bytes(),
            0x70 => self.box_test(p),
            0x71 => {
                self.last = [p[0] as i16, (p[0] >> 16) as i16, p[1] as i16];
                self.pos_result = self.transform(self.last);
            }
            0x72 => {
                let n = [p[0], p[0] >> 10, p[0] >> 20].map(|v| ((v << 22) as i32) >> 22);
                for j in 0..3 {
                    let s = (n[0] as i64 * self.vec[0][j] as i64 + n[1] as i64 * self.vec[1][j] as i64 + n[2] as i64 * self.vec[2][j] as i64) >> 9;
                    self.vec_result[j] = s as i16;
                }
            }
            _ => {}
        }
    }

    fn update_clip(&mut self) { self.clip = mul(&self.pos, &self.proj); }

    fn load(&mut self, m: Mat, with_vec: bool) {
        match self.mtx_mode {
            0 => self.proj = m,
            1 => self.pos = m,
            2 => { self.pos = m; if with_vec { self.vec = m; } }
            _ => self.tex = m,
        }
        self.update_clip();
    }

    fn mult(&mut self, m: Mat, with_vec: bool) {
        match self.mtx_mode {
            0 => self.proj = mul(&m, &self.proj),
            1 => self.pos = mul(&m, &self.pos),
            2 => { self.pos = mul(&m, &self.pos); if with_vec { self.vec = mul(&m, &self.vec); } }
            _ => self.tex = mul(&m, &self.tex),
        }
        self.update_clip();
    }

    fn mtx_push(&mut self) {
        match self.mtx_mode {
            0 => { if self.proj_sp != 0 { self.stack_error = true; } self.proj_stack = self.proj; self.proj_sp = 1; }
            1 | 2 => {
                let i = (self.pos_sp & 31) as usize;
                if self.pos_sp >= 31 { self.stack_error = true; }
                self.pos_stack[i] = self.pos;
                self.vec_stack[i] = self.vec;
                self.pos_sp = (self.pos_sp + 1) & 63;
            }
            _ => { self.tex_stack = self.tex; self.tex_sp = 1; }
        }
    }

    fn mtx_pop(&mut self, n: i32) {
        match self.mtx_mode {
            0 => { if self.proj_sp == 0 { self.stack_error = true; } self.proj_sp = 0; self.proj = self.proj_stack; }
            1 | 2 => {
                self.pos_sp = ((self.pos_sp as i32 - n) & 63) as u8;
                if self.pos_sp >= 31 { self.stack_error = true; }
                let i = (self.pos_sp & 31) as usize;
                self.pos = self.pos_stack[i];
                self.vec = self.vec_stack[i];
            }
            _ => { self.tex_sp = 0; self.tex = self.tex_stack; }
        }
        self.update_clip();
    }

    fn mtx_store(&mut self, i: u32) {
        match self.mtx_mode {
            0 => self.proj_stack = self.proj,
            1 | 2 => {
                if i == 31 { self.stack_error = true; }
                self.pos_stack[i as usize] = self.pos;
                self.vec_stack[i as usize] = self.vec;
            }
            _ => self.tex_stack = self.tex,
        }
    }

    fn mtx_restore(&mut self, i: u32) {
        match self.mtx_mode {
            0 => self.proj = self.proj_stack,
            1 | 2 => {
                if i == 31 { self.stack_error = true; }
                self.pos = self.pos_stack[i as usize];
                self.vec = self.vec_stack[i as usize];
            }
            _ => self.tex = self.tex_stack,
        }
        self.update_clip();
    }

    /// (x, y, z, 1) through the clip matrix.
    fn transform(&self, v: [i16; 3]) -> [i32; 4] {
        let m = &self.clip;
        std::array::from_fn(|j| {
            ((v[0] as i64 * m[0][j] as i64 + v[1] as i64 * m[1][j] as i64 + v[2] as i64 * m[2][j] as i64
                + 4096 * m[3][j] as i64) >> 12) as i32
        })
    }

    fn texcoord(&mut self, p: u32) {
        self.raw_st = [p as i16, (p >> 16) as i16];
        self.st = [self.raw_st[0] as i32, self.raw_st[1] as i32];
        if self.tex_param >> 30 == 1 {
            let m = &self.tex;
            let (s, t) = (self.raw_st[0] as i64, self.raw_st[1] as i64);
            for k in 0..2 {
                self.st[k] = ((s * m[0][k] as i64 + t * m[1][k] as i64 + m[2][k] as i64 + m[3][k] as i64) >> 12) as i32;
            }
        }
    }

    /// NORMAL: lighting (lights on in the polygon attributes) and normal texture coordinates.
    /// The lighting arithmetic, with its truncations, follows melonDS's hardware research.
    fn normal(&mut self, p: u32) {
        let n = [p, p >> 10, p >> 20].map(|v| ((v << 22) as i32) >> 22);
        if self.tex_param >> 30 == 2 {
            let m = &self.tex;
            for k in 0..2 {
                self.st[k] = self.raw_st[k] as i32
                    + ((n[0] as i64 * m[0][k] as i64 + n[1] as i64 * m[1][k] as i64 + n[2] as i64 * m[2][k] as i64) >> 21) as i32;
            }
        }
        let v = &self.vec;
        // the transformed normal: 1 sign bit, 10 fraction bits
        let nt: [i32; 3] = std::array::from_fn(|j| {
            let d = n[0].wrapping_mul(v[0][j]).wrapping_add(n[1].wrapping_mul(v[1][j])).wrapping_add(n[2].wrapping_mul(v[2][j]));
            (d << 9) >> 21
        });
        let ch = |c: u32, i: usize| (c >> (i * 5)) & 31;
        let (dif, amb) = (self.dif_amb & 0x7FFF, (self.dif_amb >> 16) & 0x7FFF);
        let (spe, emi) = (self.spe_emi & 0x7FFF, (self.spe_emi >> 16) & 0x7FFF);
        let mut acc: [u32; 3] = std::array::from_fn(|i| ch(emi, i) << 14);
        for l in 0..4 {
            if self.cur_attr & (1 << l) == 0 { continue; }
            let ld = self.light_dir[l];
            let mut dot = ((ld[0] * nt[0]) >> 9) + ((ld[1] * nt[1]) >> 9) + ((ld[2] * nt[2]) >> 9);
            let lc = self.light_color[l] as u32;
            let mut shine: i32 = 0;
            if dot > 0 {
                let diff = ((dot << 21) >> 21) as u32;
                for i in 0..3 { acc[i] = acc[i].wrapping_add((ch(dif, i) * ch(lc, i)).wrapping_mul(diff) & 0xFFFFF); }
                dot += nt[2];
                dot = (dot << 21) >> 21;
                dot = ((dot * dot) >> 10) & 0x3FF;
                shine = ((dot * self.spec_recip[l]) >> 8) - (1 << 9);
                shine = if shine < 0 { 0 } else { ((shine << 18) >> 18).clamp(0, 0x1FF) };
            }
            if self.spe_emi & 0x8000 != 0 { shine = (self.shininess[(shine >> 2) as usize] as i32) << 1; }
            for i in 0..3 {
                let t = (ch(spe, i) as i32 * shine + ((ch(amb, i) as i32) << 9)) as u32;
                acc[i] = acc[i].wrapping_add(t.wrapping_mul(ch(lc, i)));
            }
        }
        let c = acc.map(|x| (x >> 14).min(31) as u16);
        self.color = c[0] | c[1] << 5 | c[2] << 10;
    }

    fn vertex(&mut self, x: i16, y: i16, z: i16) {
        self.last = [x, y, z];
        if !self.in_begin { return; }
        if self.tex_param >> 30 == 3 {
            let m = &self.tex;
            let v = [x as i64, y as i64, z as i64];
            for k in 0..2 {
                self.st[k] = self.raw_st[k] as i32 + ((v[0] * m[0][k] as i64 + v[1] * m[1][k] as i64 + v[2] * m[2][k] as i64) >> 24) as i32;
            }
        }
        let c = self.transform([x, y, z]);
        let col = rgb6(self.color);
        let f = 1.0 / 4096.0;
        self.strip.push(Vtx {
            x: c[0] as f32 * f, y: c[1] as f32 * f, z: c[2] as f32 * f, w: c[3] as f32 * f,
            r: col[0] as f32, g: col[1] as f32, b: col[2] as f32,
            s: self.st[0] as f32 / 16.0, t: self.st[1] as f32 / 16.0,
        });
        let s = &self.strip;
        match self.prim {
            0 if s.len() == 3 => { let v = s.clone(); self.add_poly(v); self.strip.clear(); }
            1 if s.len() == 4 => { let v = s.clone(); self.add_poly(v); self.strip.clear(); }
            2 if s.len() == 3 => {
                let v = if self.odd { vec![s[1], s[0], s[2]] } else { s.clone() };
                self.odd = !self.odd;
                self.add_poly(v);
                self.strip.remove(0);
            }
            3 if s.len() == 4 => {
                let v = vec![s[0], s[1], s[3], s[2]];
                self.add_poly(v);
                self.strip.drain(0..2);
            }
            _ => {}
        }
    }

    /// Cull, clip, project and keep a polygon.
    fn add_poly(&mut self, v: Vec<Vtx>) {
        let attr = self.cur_attr;
        // facing: the sign of the homogeneous determinant of the first three vertices
        // (positive = counter-clockwise with y up = the front)
        let (a, b, c) = (&v[0], &v[1], &v[2]);
        let det = a.x * (b.y * c.w - c.y * b.w) - a.y * (b.x * c.w - c.x * b.w) + a.w * (b.x * c.y - c.x * b.y);
        let front = det >= 0.0;
        if (front && attr & 0x80 == 0) || (!front && attr & 0x40 == 0) { return; }
        if attr & 0x1000 == 0 && v.iter().any(|p| p.z > p.w) { return; }   // crosses the far plane
        let v = clip(v);
        if v.len() < 3 || self.polys.len() >= MAX_POLYS || self.vert_count + v.len() > MAX_VERTS { return; }
        self.vert_count += v.len();

        let [x1, y1, x2, y2] = self.viewport.map(|b| b as f32);
        let (vw, vh) = (x2 - x1 + 1.0, y2 - y1 + 1.0);
        let top = 191.0 - y2;
        let v: Vec<Vtx> = v.into_iter().map(|p| {
            let iw = 1.0 / p.w;
            Vtx { x: x1 + (p.x * iw + 1.0) * vw * 0.5, y: top + (1.0 - p.y * iw) * vh * 0.5, z: p.z * iw, ..p }
        }).collect();
        let ymin = v.iter().map(|p| p.y).fold(f32::MAX, f32::min);
        let ymax = v.iter().map(|p| p.y).fold(f32::MIN, f32::max);
        let alpha = (attr >> 16) & 31;
        let fmt = (self.tex_param >> 26) & 7;
        // A3I5 / A5I3 textures make a polygon translucent unless it's in decal mode
        let translucent = (alpha > 0 && alpha < 31) || ((fmt == 1 || fmt == 6) && attr & 0x30 != 0x10);
        self.polys.push(Poly { v, attr, tex: self.tex_param, pal: self.pltt_base, translucent, ymin, ymax });
    }

    fn box_test(&mut self, p: &[u32]) {
        let (x, y, z) = (p[0] as i16, (p[0] >> 16) as i16, p[1] as i16);
        let (w, h, d) = ((p[1] >> 16) as i16, p[2] as i16, (p[2] >> 16) as i16);
        let mut out_all = [true; 6];
        for k in 0..8 {
            let c = self.transform([
                x.wrapping_add(if k & 1 != 0 { w } else { 0 }),
                y.wrapping_add(if k & 2 != 0 { h } else { 0 }),
                z.wrapping_add(if k & 4 != 0 { d } else { 0 }),
            ]);
            let tests = [c[0] < -c[3], c[0] > c[3], c[1] < -c[3], c[1] > c[3], c[2] < -c[3], c[2] > c[3]];
            for i in 0..6 { out_all[i] &= tests[i]; }
        }
        self.box_result = !out_all.iter().any(|&o| o);
    }
}

fn m43(p: &[u32]) -> Mat {
    let mut m = IDENT;
    for i in 0..4 { for j in 0..3 { m[i][j] = p[i * 3 + j] as i32; } }
    m
}

/// Sutherland-Hodgman against the six planes -w <= x, y, z <= w.
fn clip(mut v: Vec<Vtx>) -> Vec<Vtx> {
    let planes: [fn(&Vtx) -> f32; 6] = [
        |p| p.w + p.z, |p| p.w - p.z, |p| p.w + p.x, |p| p.w - p.x, |p| p.w + p.y, |p| p.w - p.y,
    ];
    for d in planes {
        if v.iter().all(|p| d(p) >= 0.0) { continue; }
        let mut out = Vec::with_capacity(v.len() + 2);
        for i in 0..v.len() {
            let (a, b) = (&v[i], &v[(i + 1) % v.len()]);
            let (da, db) = (d(a), d(b));
            if da >= 0.0 { out.push(*a); }
            if (da >= 0.0) != (db >= 0.0) { out.push(a.lerp(b, da / (da - db))); }
        }
        v = out;
        if v.len() < 3 { return v; }
    }
    v
}

// ---------------------------------------------------------------- rendering

/// The registers the rendering engine reads, from ARM9 I/O.
pub struct RenderRegs<'a>(pub &'a [u8]);

impl RenderRegs<'_> {
    fn r16(&self, o: usize) -> u16 { u16::from_le_bytes([self.0[o], self.0[o + 1]]) }
    fn r32(&self, o: usize) -> u32 { u32::from_le_bytes([self.0[o], self.0[o + 1], self.0[o + 2], self.0[o + 3]]) }
}

struct Target {
    /// 6-bit r, g, b and 5-bit alpha
    color: Vec<[u8; 4]>,
    depth: Vec<u32>,
    /// polygon id (bits 0-5), drawn by a translucent polygon (bit 6), fog (bit 7)
    attr: Vec<u8>,
    /// the opaque polygon's id, and whether the pixel is on its edge (edge marking)
    oid: Vec<u8>,
    edge: Vec<bool>,
    /// set by shadow mask polygons where their depth test fails
    stencil: Vec<u8>,
    /// anti-aliasing: how much of the pixel the opaque polygon covers (0-31), and what's
    /// behind a partly covered pixel (colour, depth); whether an opaque polygon is there
    cov: Vec<u8>,
    below: Vec<[u8; 4]>,
    below_depth: Vec<u32>,
    filled: Vec<bool>,
}

#[derive(Clone, Copy, PartialEq)]
enum Kind { Normal, ShadowMask, Shadow }

impl Gpu3d {
    /// Draw the current polygon list into `frame`.
    pub fn render(&mut self, regs: RenderRegs, vram: &Vram) {
        self.dirty = false;
        let disp = regs.r16(0x60) as u32;
        let clear = regs.r32(0x350);
        let cd = (regs.r16(0x354) & 0x7FFF) as u32;
        let clear_depth = cd * 0x200 + 0x1FF;
        let cc = rgb6(clear as u16);
        let mut t = Target {
            color: vec![[cc[0] as u8, cc[1] as u8, cc[2] as u8, ((clear >> 16) & 31) as u8]; W * H],
            depth: vec![clear_depth; W * H],
            attr: vec![(((clear >> 24) & 63) | if clear & 0x8000 != 0 { 0x80 } else { 0 }) as u8; W * H],
            oid: vec![((clear >> 24) & 63) as u8; W * H],
            edge: vec![false; W * H],
            stencil: vec![0; W * H],
            cov: vec![31; W * H],
            below: vec![[cc[0] as u8, cc[1] as u8, cc[2] as u8, ((clear >> 16) & 31) as u8]; W * H],
            below_depth: vec![clear_depth; W * H],
            filled: vec![false; W * H],
        };
        let mut opaque: Vec<&Poly> = self.render_polys.iter().filter(|p| !p.translucent).collect();
        let mut trans: Vec<&Poly> = self.render_polys.iter().filter(|p| p.translucent).collect();
        let key = |p: &&Poly| (p.ymax as i32, p.ymin as i32);
        opaque.sort_by_key(key);
        if self.render_param & 1 == 0 { trans.sort_by_key(key); }
        let wbuf = self.render_param & 2 != 0;
        let toon: Vec<u16> = (0..32).map(|i| regs.r16(0x380 + i * 2)).collect();
        let ctx = Ctx { disp, wbuf, alpha_ref: (regs.r16(0x340) & 31) as u32, toon, vram, aa: disp & 0x10 != 0,
                        tex_pages: vram.page_slices(Region::Texture), pal_pages: vram.page_slices(Region::TexPal) };
        // shadows: polygons in mode 3 with id 0 are masks (they mark the stencil where they
        // fail the depth test; a run of masks starts with a clear stencil), other mode-3
        // polygons draw only on marked pixels
        let mut prev_mask = false;
        for p in opaque.iter().chain(trans.iter()) {
            let kind = match ((p.attr >> 4) & 3, (p.attr >> 24) & 63) {
                (3, 0) => Kind::ShadowMask,
                (3, _) => Kind::Shadow,
                _ => Kind::Normal,
            };
            if kind == Kind::ShadowMask && !prev_mask { t.stencil.fill(0); }
            prev_mask = kind == Kind::ShadowMask;
            raster(&ctx, p, &mut t, kind);
        }
        if disp & 0x20 != 0 { edge_marking(&regs, clear, clear_depth, &mut t); }
        if disp & 0x80 != 0 { fog(&regs, disp, &mut t); }
        if disp & 0x10 != 0 { anti_alias(&mut t); }
        for (o, (c, _)) in self.frame.iter_mut().zip(t.color.iter().zip(t.depth.iter())) {
            *o = (c[0] as u32 >> 1) | (c[1] as u32 >> 1) << 5 | (c[2] as u32 >> 1) << 10 | (c[3] as u32) << 16;
        }
    }

    /// Engine A's BG0 when 3D is off: nothing.
    pub fn clear_frame(&mut self) { self.frame.iter_mut().for_each(|p| *p = 0); }
}

struct Ctx<'a> {
    disp: u32,
    /// DISP3DCNT bit 4: anti-aliased polygon edges
    aa: bool,
    wbuf: bool,
    alpha_ref: u32,
    toon: Vec<u16>,
    vram: &'a Vram,
    tex_pages: Vec<&'a [u8]>,
    pal_pages: Vec<&'a [u8]>,
}

impl Ctx<'_> {
    #[inline(always)]
    fn t8(&self, a: u32) -> u8 {
        let a = a & 0x7FFFF;
        let p = self.tex_pages[(a >> 14) as usize];
        if !p.is_empty() { p[(a & 0x3FFF) as usize] } else { self.vram.read8(Region::Texture, a) }
    }
    #[inline(always)]
    fn t16(&self, a: u32) -> u16 { let a = a & !1; self.t8(a) as u16 | (self.t8(a + 1) as u16) << 8 }
    #[inline(always)]
    fn pal16(&self, a: u32) -> u16 {
        let a = a & 0x1FFFE;                      // 96 KB of palettes: keep every address bit
        let p = self.pal_pages.get((a >> 14) as usize).copied().unwrap_or(&[]);
        let v = if !p.is_empty() {
            let o = (a & 0x3FFF) as usize;
            u16::from_le_bytes([p[o], p[o + 1]])
        } else {
            self.vram.read16(Region::TexPal, a)
        };
        v & 0x7FFF
    }
}

/// Per-pixel values interpolated across a polygon (the /w ones in perspective).
#[derive(Clone, Copy)]
struct Span { x: f32, iw: f32, z: f32, r: f32, g: f32, b: f32, s: f32, t: f32 }

impl Span {
    fn of(v: &Vtx) -> Span {
        let iw = 1.0 / v.w;
        Span { x: v.x, iw, z: v.z, r: v.r * iw, g: v.g * iw, b: v.b * iw, s: v.s * iw, t: v.t * iw }
    }
    fn lerp(&self, o: &Span, f: f32) -> Span {
        let l = |a: f32, b: f32| a + (b - a) * f;
        Span { x: l(self.x, o.x), iw: l(self.iw, o.iw), z: l(self.z, o.z), r: l(self.r, o.r), g: l(self.g, o.g),
               b: l(self.b, o.b), s: l(self.s, o.s), t: l(self.t, o.t) }
    }
    /// (o - self) / d: the per-pixel step across a row.
    fn step(&self, o: &Span, d: f32) -> Span {
        let k = |a: f32, b: f32| (b - a) / d;
        Span { x: 1.0, iw: k(self.iw, o.iw), z: k(self.z, o.z), r: k(self.r, o.r), g: k(self.g, o.g),
               b: k(self.b, o.b), s: k(self.s, o.s), t: k(self.t, o.t) }
    }
    #[inline(always)]
    fn add(&mut self, d: &Span) {
        self.iw += d.iw; self.z += d.z; self.r += d.r; self.g += d.g; self.b += d.b; self.s += d.s; self.t += d.t;
    }
}

fn raster(ctx: &Ctx, p: &Poly, t: &mut Target, kind: Kind) {
    let n = p.v.len();
    let spans: Vec<Span> = p.v.iter().map(Span::of).collect();
    let y0 = ((p.ymin - 0.5).ceil().max(0.0)) as i32;
    let y1 = ((p.ymax - 0.5).ceil().min(H as f32)) as i32;
    let alpha = (p.attr >> 16) & 31;
    let wire = alpha == 0;
    let pa = if wire { 31 } else { alpha };
    let mode = (p.attr >> 4) & 3;
    let id = ((p.attr >> 24) & 63) as u8;
    let fmt = if ctx.disp & 1 != 0 { (p.tex >> 26) & 7 } else { 0 };
    let depth_equal = p.attr & 0x4000 != 0;
    let blend = ctx.disp & 8 != 0;
    // anti-aliasing: each edge as a line, with its sign chosen so the inside is positive
    // (the polygon is convex, so its centroid is inside every edge)
    let aa = ctx.aa && kind == Kind::Normal && !wire;
    let edges: Vec<(f32, f32, f32, f32)> = if aa {
        let (cx, cy) = (p.v.iter().map(|v| v.x).sum::<f32>() / n as f32, p.v.iter().map(|v| v.y).sum::<f32>() / n as f32);
        (0..n).filter_map(|i| {
            let (a, b) = (&p.v[i], &p.v[(i + 1) % n]);
            let (ex, ey) = (b.x - a.x, b.y - a.y);
            let len = (ex * ex + ey * ey).sqrt();
            if len < 1e-6 { return None; }
            let d = |x: f32, y: f32| ((x - a.x) * ey - (y - a.y) * ex) / len;
            let sgn = if d(cx, cy) >= 0.0 { 1.0 } else { -1.0 };
            Some((a.x, a.y, ey * sgn / len, -ex * sgn / len))
        }).collect()
    } else { Vec::new() };
    let coverage = |px: f32, py: f32| -> u8 {
        let mut m = f32::MAX;
        for &(ax, ay, nx, ny) in &edges { m = m.min((px - ax) * nx + (py - ay) * ny); }
        ((0.5 + m).clamp(0.0, 1.0) * 31.0 + 0.5) as u8
    };
    for y in y0..y1 {
        let yc = y as f32 + 0.5;
        let mut l: Option<Span> = None;
        let mut r: Option<Span> = None;
        for i in 0..n {
            let (a, b) = (&p.v[i], &p.v[(i + 1) % n]);
            if (a.y <= yc) == (b.y <= yc) { continue; }
            let f = (yc - a.y) / (b.y - a.y);
            let s = spans[i].lerp(&spans[(i + 1) % n], f);
            if l.map_or(true, |q| s.x < q.x) { l = Some(s); }
            if r.map_or(true, |q| s.x > q.x) { r = Some(s); }
        }
        let (Some(l), Some(r)) = (l, r) else { continue };
        let x0 = ((l.x - 0.5).ceil().max(0.0)) as i32;
        let x1 = ((r.x - 0.5).ceil().min(W as f32)) as i32;
        if x0 >= x1 { continue; }
        let dx = r.x - l.x;
        let d = if dx > 0.0 { l.step(&r, dx) } else { Span { x: 1.0, iw: 0.0, z: 0.0, r: 0.0, g: 0.0, b: 0.0, s: 0.0, t: 0.0 } };
        let mut s = if dx > 0.0 { l.lerp(&r, ((x0 as f32 + 0.5 - l.x) / dx).clamp(0.0, 1.0)) } else { l };
        for x in x0..x1 {
            if x > x0 { s.add(&d); }
            if wire && !(x == x0 || x == x1 - 1 || y == y0 || y == y1 - 1) { continue; }
            let w = 1.0 / s.iw;
            let depth = if ctx.wbuf {
                (w * 4096.0).clamp(0.0, 16777215.0) as u32
            } else {
                ((s.z * 0.5 + 0.5) * 16777215.0).clamp(0.0, 16777215.0) as u32
            };
            let i = y as usize * W + x as usize;
            let dst = t.depth[i];
            let pass = if depth_equal { depth.abs_diff(dst) <= 0x200 } else { depth < dst };
            match kind {
                Kind::ShadowMask => { if !pass { t.stencil[i] = 1; } continue; }
                Kind::Shadow => if t.stencil[i] == 0 || t.attr[i] & 63 == id { continue; },
                Kind::Normal => {}
            }
            // behind a partly covered edge pixel but in front of what's under it: this
            // polygon becomes the layer the edge blends with
            let under = !pass && aa && t.cov[i] < 31 && depth < t.below_depth[i];
            if !pass && !under { continue; }
            // colour
            let ch = |v: f32| (v * w).max(0.0).min(63.0) as i32 as u32;
            let vc = [ch(s.r), ch(s.g), ch(s.b)];
            // toon replaces the vertex colour; highlight greys it (all channels = red) and
            // adds the toon colour at the end
            let highlight = mode == 2 && ctx.disp & 2 != 0;
            let mut v = vc;
            if mode == 2 {
                if highlight { v = [vc[0]; 3]; } else { v = rgb6(ctx.toon[(vc[0] >> 1) as usize]); }
            }
            let (mut c, mut a) = (v, pa);
            if fmt != 0 {
                let (tcol, ta) = texel(ctx, p.tex, p.pal, floor(s.s * w), floor(s.t * w));
                let tc = rgb6(tcol);
                if mode & 1 != 0 {
                    // decal: a transparent texel shows the vertex colour
                    c = match ta { 0 => v, 31 => tc, _ => [0, 1, 2].map(|k| (tc[k] * ta + v[k] * (31 - ta)) >> 5) };
                } else {
                    c = [0, 1, 2].map(|k| ((tc[k] + 1) * (v[k] + 1) - 1) >> 6);
                    a = ((ta + 1) * (pa + 1) - 1) >> 5;
                }
            }
            if highlight {
                let toon = rgb6(ctx.toon[(vc[0] >> 1) as usize]);
                c = [0, 1, 2].map(|k| (c[k] + toon[k]).min(63));
            }
            if wire { a = 31; }
            if a == 0 || (ctx.disp & 4 != 0 && a <= ctx.alpha_ref) { continue; }
            let fogbit = if p.attr & 0x8000 != 0 { 0x80 } else { 0 };
            if under {
                if a == 31 { t.below[i] = [c[0] as u8, c[1] as u8, c[2] as u8, 31]; t.below_depth[i] = depth; }
                continue;
            }
            if a == 31 && kind != Kind::Shadow {
                if aa {
                    let cv = coverage(x as f32 + 0.5, yc);
                    if cv < 31 { t.below[i] = t.color[i]; t.below_depth[i] = t.depth[i]; }
                    t.cov[i] = cv;
                } else {
                    t.cov[i] = 31;
                }
                t.filled[i] = true;
                t.color[i] = [c[0] as u8, c[1] as u8, c[2] as u8, 31];
                t.depth[i] = depth;
                t.attr[i] = id | fogbit;
                t.oid[i] = id;
                t.edge[i] = x == x0 || x == x1 - 1 || y == y0 || y == y1 - 1;
            } else {
                let da = t.attr[i];
                // this translucent polygon was here (shadows check opaque ids too)
                if (da & 0x40 != 0 || kind == Kind::Shadow) && da & 63 == id { continue; }
                let d = t.color[i];
                if blend && d[3] != 0 {
                    for k in 0..3 { t.color[i][k] = ((c[k] * (a + 1) + d[k] as u32 * (31 - a)) / 32) as u8; }
                    t.color[i][3] = a.max(d[3] as u32) as u8;
                } else {
                    t.color[i] = [c[0] as u8, c[1] as u8, c[2] as u8, a as u8];
                }
                if p.attr & 0x800 != 0 { t.depth[i] = depth; }
                t.attr[i] = id | 0x40 | (da & fogbit);
            }
        }
    }
}

/// floor() without the library call (texture coordinates stay within +-32768).
#[inline(always)]
fn floor(v: f32) -> i32 { (v + 65536.0) as i32 - 65536 }

/// One texel: (BGR555, alpha 0-31); transparent texels have alpha 0.
#[inline]
fn texel(ctx: &Ctx, tex: u32, pal: u32, s: i32, t: i32) -> (u16, u32) {
    let w = 8i32 << ((tex >> 20) & 7);
    let h = 8i32 << ((tex >> 23) & 7);
    // sizes are powers of two: wrap with a mask, the bit above it says "mirrored"
    let coord = |v: i32, size: i32, repeat: bool, flip: bool| -> i32 {
        if !repeat { return v.clamp(0, size - 1); }
        let m = v & (size - 1);
        if flip && v & size != 0 { size - 1 - m } else { m }
    };
    let s = coord(s, w, tex & 0x10000 != 0, tex & 0x40000 != 0) as u32;
    let t = coord(t, h, tex & 0x20000 != 0, tex & 0x80000 != 0) as u32;
    let base = (tex & 0xFFFF) * 8;
    let w = w as u32;
    let fmt = (tex >> 26) & 7;
    let color0_clear = tex & (1 << 29) != 0;
    let t8 = |a: u32| ctx.t8(a);
    let t16 = |a: u32| ctx.t16(a);
    let pal16 = |a: u32| ctx.pal16(a);
    let i = t * w + s;
    match fmt {
        1 => {
            let b = t8(base + i) as u32;
            let a3 = b >> 5;
            (pal16(pal * 16 + (b & 31) * 2), a3 * 4 + a3 / 2)
        }
        2 => {
            let idx = (t8(base + i / 4) as u32 >> ((i & 3) * 2)) & 3;
            if idx == 0 && color0_clear { return (0, 0); }
            (pal16(pal * 8 + idx * 2), 31)
        }
        3 => {
            let idx = (t8(base + i / 2) as u32 >> ((i & 1) * 4)) & 15;
            if idx == 0 && color0_clear { return (0, 0); }
            (pal16(pal * 16 + idx * 2), 31)
        }
        4 => {
            let idx = t8(base + i) as u32;
            if idx == 0 && color0_clear { return (0, 0); }
            (pal16(pal * 16 + idx * 2), 31)
        }
        5 => {
            let block = base + ((t / 4) * (w / 4) + s / 4) * 4;
            let bits = (t8(block + (t & 3)) as u32 >> ((s & 3) * 2)) & 3;
            let slot = block & 0x7FFFF;
            let pi_addr = 0x20000 + (slot & 0x1FFFF) / 2 + if slot >= 0x40000 { 0x10000 } else { 0 };
            let pi = t16(pi_addr) as u32;
            let pb = pal * 16 + (pi & 0x3FFF) * 4;
            let c = |k: u32| pal16(pb + k * 2);
            let mix = |a: u16, b: u16, wa: u32, wb: u32| -> u16 {
                let mut o = 0u16;
                for sh in [0, 5, 10] {
                    let v = (((a >> sh) & 31) as u32 * wa + ((b >> sh) & 31) as u32 * wb) / (wa + wb);
                    o |= (v as u16) << sh;
                }
                o
            };
            let mode = pi >> 14;
            match (mode, bits) {
                (_, 0) => (c(0), 31),
                (_, 1) => (c(1), 31),
                (0 | 2, 2) => (c(2), 31),
                (2, 3) => (c(3), 31),
                (1, 2) => (mix(c(0), c(1), 1, 1), 31),
                (3, 2) => (mix(c(0), c(1), 5, 3), 31),
                (3, 3) => (mix(c(0), c(1), 3, 5), 31),
                _ => (0, 0),
            }
        }
        6 => {
            let b = t8(base + i) as u32;
            (pal16(pal * 16 + (b & 7) * 2), b >> 3)
        }
        7 => {
            let c = t16(base + i * 2);
            (c & 0x7FFF, if c & 0x8000 != 0 { 31 } else { 0 })
        }
        _ => (0, 0),
    }
}

/// Anti-aliasing (DISP3DCNT bit 4): on object outlines (an edge pixel next to another
/// polygon id or to no opaque polygon), blend the partly covering polygon with what's
/// behind it by coverage. Over the clear plane (alpha 0) the pixel's alpha fades instead,
/// so the edge blends with the 2D layers.
fn anti_alias(t: &mut Target) {
    for y in 0..H {
        for x in 0..W {
            let i = y * W + x;
            let cv = t.cov[i] as u32;
            if cv >= 31 || !t.filled[i] || t.attr[i] & 0x40 != 0 { continue; }
            let id = t.oid[i];
            let outline = [(-1i32, 0i32), (1, 0), (0, -1), (0, 1)].iter().any(|&(dx, dy)| {
                let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                if nx < 0 || ny < 0 || nx >= W as i32 || ny >= H as i32 { return false; }
                let j = ny as usize * W + nx as usize;
                !t.filled[j] || t.oid[j] != id
            });
            if !outline { continue; }
            let (top, under) = (t.color[i], t.below[i]);
            if under[3] == 0 {
                t.color[i][3] = ((top[3] as u32 * cv) / 31) as u8;
            } else {
                for k in 0..3 { t.color[i][k] = ((top[k] as u32 * cv + under[k] as u32 * (31 - cv)) / 31) as u8; }
            }
        }
    }
}

/// Edge marking (DISP3DCNT bit 5): an opaque polygon's edge pixel takes EDGE_COLOR
/// (0x330, by polygon id / 8) where a neighbour belongs to another polygon and lies further
/// back. Off-screen neighbours count as the clear plane.
fn edge_marking(regs: &RenderRegs, clear: u32, clear_depth: u32, t: &mut Target) {
    let colors: Vec<[u32; 3]> = (0..8).map(|k| rgb6(regs.r16(0x330 + k * 2))).collect();
    let clear_id = ((clear >> 24) & 63) as u8;
    let mut marks = Vec::new();
    for y in 0..H {
        for x in 0..W {
            let i = y * W + x;
            if !t.edge[i] { continue; }
            let (id, z) = (t.oid[i], t.depth[i]);
            let n = |dx: i32, dy: i32| -> (u8, u32) {
                let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                if nx < 0 || ny < 0 || nx >= W as i32 || ny >= H as i32 { return (clear_id, clear_depth); }
                let j = ny as usize * W + nx as usize;
                (t.oid[j], t.depth[j])
            };
            if [n(-1, 0), n(1, 0), n(0, -1), n(0, 1)].iter().any(|&(nid, nz)| nid != id && z < nz) {
                marks.push((i, colors[(id >> 3) as usize]));
            }
        }
    }
    for (i, c) in marks {
        t.color[i][0] = c[0] as u8;
        t.color[i][1] = c[1] as u8;
        t.color[i][2] = c[2] as u8;
    }
}

/// Fog: FOG_COLOR 0x358, FOG_OFFSET 0x35C, 32-entry density table at 0x360 (0-127),
/// DISP3DCNT bits 8-11 the depth step, bit 6 = fog the alpha only.
fn fog(regs: &RenderRegs, disp: u32, t: &mut Target) {
    let fc = regs.r32(0x358);
    let fcol = rgb6(fc as u16);
    let fa = (fc >> 16) & 31;
    let offset = (regs.r16(0x35C) & 0x7FFF) as i32;
    let shift = (disp >> 8) & 15;
    let step = (0x400 >> shift.min(10)).max(1) as i32;
    let table: Vec<i32> = (0..32).map(|i| (regs.0[0x360 + i] & 127) as i32).collect();
    let density = |z: i32| -> i32 {
        let d = if z < offset { table[0] } else {
            let pos = (z - offset) as f32 / step as f32;
            let k = pos as usize;
            if k >= 31 { table[31] } else {
                let f = pos - k as f32;
                (table[k] as f32 + (table[k + 1] - table[k]) as f32 * f) as i32
            }
        };
        if d >= 127 { 128 } else { d }
    };
    for i in 0..W * H {
        if t.attr[i] & 0x80 == 0 { continue; }
        let d = density((t.depth[i] >> 9) as i32) as u32;
        let c = &mut t.color[i];
        if disp & 0x40 == 0 {
            for k in 0..3 { c[k] = ((fcol[k] * d + c[k] as u32 * (128 - d)) / 128) as u8; }
        }
        c[3] = ((fa * d + c[3] as u32 * (128 - d)) / 128) as u8;
    }
}
