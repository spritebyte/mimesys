//! PowerVR2 renderer (software): draws a TA scene into an accumulation buffer the size of
//! the tiles in the region array, then writes those tiles to the framebuffer per FB_W_*.
//! - background plane from ISP_BACKGND_T / ISP_BACKGND_D
//! - opaque list: depth compare modes, z write disable
//! - punch-through list: alpha test against PT_ALPHA_REF
//! - translucent list: blend factors, the secondary accumulation buffer (TSP src / dst
//!   select); without ISP_FEED_CFG presort, fragments are sorted per pixel back to front
//!   (the hardware's auto-sort), ties in the order sent
//! - Gouraud / flat, perspective-correct u v and colours, offset colour, the four texture
//!   shading instructions, user tile clip
//! - culling: 1 small (|area| < FPU_CULL_VAL), 2 / 3 by winding: mode 2 culls triangles
//!   clockwise on screen (y down; counter-clockwise with y up, KallistiOS's "CCW"), mode 3
//!   the others; odd triangles of a strip count reversed; small ones culled in 2 and 3 too.
//!   (Patch 0101 had 2 / 3 the other way round: Sonic Adventure lost walls.)
//! - mipmaps: a level per pixel from the texel / pixel ratio (u v one pixel right and down),
//!   scaled by the TSP "D adjust"
//! - fog: table (FOG_DENSITY, FOG_TABLE, FOG_COL_RAM), per vertex (offset alpha,
//!   FOG_COL_VERT), table mode 2; colour clamp (FOG_CLAMP_MIN / MAX)
//! - modifier volumes on the opaque and punch-through lists: each volume's triangles toggle
//!   a per-pixel parity where they lie in front of the drawn surface; "inside" (1) or
//!   "outside" (2) volumes mark pixels; marked pixels of polygons with the modifier bit get
//!   the second parameter set (two-volume polygons) or the FPU_SHAD_SCALE shadow
//! - render to texture (FB_W_SOF1 bit 24: the 64-bit path)
//! Not yet: modifier volumes on translucent polygons, bump maps, trilinear filtering.
//! Behaviour from the PowerVR2 documentation and KallistiOS's pvr.h (culling names);
//! texture and parameter details after MAME mame/sega/powervr2.cpp (BSD-3-Clause). The
//! rasterizer is our own.

use super::pvr::map32;
use super::ta::{ModTri, Poly, Scene, Vert, LIST_OPAQUE, LIST_OPAQUE_MOD, LIST_PUNCH, LIST_TRANS};
use super::texture::Tex;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A texture level decoded to ARGB8888.
type Texels = Arc<Vec<u32>>;

/// Textures decoded during one render, shared by the bands: (TCW, TSP size bits, level).
#[derive(Default)]
pub struct TexCache(Mutex<HashMap<(u32, u32, u32), Texels>>);

impl TexCache {
    fn get(&self, t: &Tex, key: (u32, u32, u32), vram: &[u8], pal: &[u32]) -> Texels {
        if let Some(d) = self.0.lock().unwrap().get(&key) { return d.clone(); }
        let d = Arc::new(t.decode(vram, pal));
        self.0.lock().unwrap().entry(key).or_insert(d).clone()
    }
}

pub const PARAM_BASE: u32 = 0x020;
pub const REGION_BASE: u32 = 0x02C;
pub const FB_W_CTRL: u32 = 0x048;
pub const FB_W_LINESTRIDE: u32 = 0x04C;
pub const FB_W_SOF1: u32 = 0x060;
pub const FB_W_SOF2: u32 = 0x064;
pub const FB_X_CLIP: u32 = 0x068;
pub const FB_Y_CLIP: u32 = 0x06C;
pub const FPU_SHAD_SCALE: u32 = 0x074;
pub const FPU_CULL_VAL: u32 = 0x078;
pub const FPU_PARAM_CFG: u32 = 0x07C;
pub const ISP_BACKGND_D: u32 = 0x088;
pub const ISP_BACKGND_T: u32 = 0x08C;
pub const ISP_FEED_CFG: u32 = 0x098;
pub const FOG_COL_RAM: u32 = 0x0B0;
pub const FOG_COL_VERT: u32 = 0x0B4;
pub const FOG_DENSITY: u32 = 0x0B8;
pub const FOG_CLAMP_MAX: u32 = 0x0BC;
pub const FOG_CLAMP_MIN: u32 = 0x0C0;
pub const TEXT_CONTROL: u32 = 0x0E4;
pub const SCALER_CTL: u32 = 0x0F4;
pub const PAL_RAM_CTRL: u32 = 0x108;
pub const PT_ALPHA_REF: u32 = 0x11C;
pub const FOG_TABLE: u32 = 0x200;
pub const PALETTE: u32 = 0x1000;

/// Largest accumulation buffer (pixels).
const MAX_W: usize = 2048;
const MAX_H: usize = 1024;

/// What a render did (debug / tests).
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Stats {
    pub tiles: u32,
    pub polys: u32,
    pub pixels: u64,
    pub culled: u32,
    pub volumes: u32,
}

/// A polygon's drawing state for one parameter set.
struct Mode {
    textured: bool,
    tsp: u32,
    tcw: u32,
    depth: u32,
    zwrite: bool,
    gouraud: bool,
    offset: bool,
    use_alpha: bool,
    tex_alpha: bool,
    shading: u32,
    fog: u32,
    clamp: bool,
    list: u8,
    /// translucent polygons use their own depth mode and z write (ISP_FEED_CFG presort)
    presort: bool,
    /// draw with the second parameter set (u1 v1 base1 off1)
    set1: bool,
    /// multiply RGB by this (cheap shadow)
    shadow: f32,
}

/// Which pixels a draw may touch.
#[derive(Clone, Copy, PartialEq)]
enum Pass {
    /// normal drawing; opaque writes record the strip number
    Draw(u32),
    /// re-shade the pixels of strip n that a modifier volume marked
    Modify(u32),
}

/// The accumulation buffers.
struct Bufs {
    col: Vec<u32>,
    col2: Vec<u32>,
    z: Vec<f32>,
    /// strip number + 1 that drew each pixel (opaque / punch-through), 0 = background
    id: Vec<u32>,
    /// modifier volumes marked the pixel
    mask: Vec<bool>,
    /// collecting translucent fragments (pixel, 1/w, colour, TSP) for the per-pixel sort
    sorting: bool,
    tfrags: Vec<(u32, f32, u32, u32)>,
}

impl Bufs {
    /// Blend a translucent fragment by its TSP's factors and buffer selects.
    fn blend(&mut self, i: usize, col: [f32; 4], tsp: u32) {
        let (sf_sel, df_sel) = (tsp >> 29, tsp >> 26 & 7);
        let src = if tsp & (1 << 25) != 0 { unpack(self.col2[i]) } else { col };
        let target = if tsp & (1 << 24) != 0 { &mut self.col2 } else { &mut self.col };
        let d = unpack(target[i]);
        let sf = factor(sf_sel, d, src[0], d[0]);
        let df = factor(df_sel, src, src[0], d[0]);
        let mut o = [0f32; 4];
        for ch in 0..4 { o[ch] = src[ch] * sf[ch] + d[ch] * df[ch]; }
        target[i] = pack(o);
    }
}

/// Renders one horizontal band (rows y0 .. y0 + h) of the accumulation buffer; bands run on
/// threads of their own. Buffers are band-local (index = (y - y0) * w + x).
pub struct Renderer<'a> {
    regs: &'a [u32],
    vram: &'a [u8],
    w: usize,
    y0: usize,
    h: usize,
    b: Bufs,
    cache: &'a TexCache,
    /// FOG_DENSITY as a number
    fog_den: f32,
    pub stats: Stats,
}

#[inline]
fn r(regs: &[u32], off: u32) -> u32 { regs[(off as usize / 4) & 0x7FF] }

fn read32(vram: &[u8], a: u32) -> u32 {
    let i = map32(a & 0x7F_FFFC);
    u32::from_le_bytes([vram[i], vram[i + 1], vram[i + 2], vram[i + 3]])
}

#[inline]
fn depth_pass(mode: u32, new: f32, old: f32) -> bool {
    match mode {
        0 => false,
        1 => new < old,
        2 => new == old,
        3 => new <= old,
        4 => new > old,
        5 => new != old,
        6 => new >= old,
        _ => true,
    }
}

#[inline]
fn unpack(c: u32) -> [f32; 4] {
    [(c >> 24) as f32 / 255.0, (c >> 16 & 0xFF) as f32 / 255.0, (c >> 8 & 0xFF) as f32 / 255.0, (c & 0xFF) as f32 / 255.0]
}

#[inline]
fn pack(c: [f32; 4]) -> u32 {
    let q = |x: f32| (x.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
    q(c[0]) << 24 | q(c[1]) << 16 | q(c[2]) << 8 | q(c[3])
}

/// A blend factor (TSP SRC / DST instruction); `other` is the colour on the other side.
#[inline]
fn factor(sel: u32, other: [f32; 4], src_a: f32, dst_a: f32) -> [f32; 4] {
    match sel {
        0 => [0.0; 4],
        1 => [1.0; 4],
        2 => other,
        3 => [1.0 - other[0], 1.0 - other[1], 1.0 - other[2], 1.0 - other[3]],
        4 => [src_a; 4],
        5 => [1.0 - src_a; 4],
        6 => [dst_a; 4],
        _ => [1.0 - dst_a; 4],
    }
}

/// Signed area ×2 of a screen triangle: positive = clockwise on screen (y down).
#[inline]
fn area(a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> f32 {
    (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)
}

/// Visit the pixels of a triangle in the band of rows y0 .. y0 + h (centres; an edge pixel
/// belongs to one side only). `f(band index, weights)` gets screen-space barycentric
/// weights summing to 1. (Modifier volumes; polygons use `Renderer::tri`.)
fn raster(w: usize, y0: usize, h: usize, p: [(f32, f32); 3], mut f: impl FnMut(usize, [f32; 3])) {
    let [a, b, c] = p;
    let ar = area(a, b, c);
    if !(ar.abs() > 1e-12) { return; }
    let Some((x0, x1, ya, yb)) = bounds(p, w, y0, h, None) else { return };
    let e = edges(p, ar);
    let own = [owns(e[0]), owns(e[1]), owns(e[2])];
    let aabs = ar.abs();
    for y in ya..=yb {
        let py = y as f32 + 0.5;
        for x in x0..=x1 {
            let px = x as f32 + 0.5;
            let mut l = [0f32; 3];
            let mut inside = true;
            for k in 0..3 {
                let v = e[k].0 * px + e[k].1 * py + e[k].2;
                if v < 0.0 || (v == 0.0 && !own[k]) { inside = false; break; }
                l[k] = v / aabs;
            }
            if inside { f((y as usize - y0) * w + x as usize, l); }
        }
    }
}

/// Edge functions of a triangle, positive inside: e_k(x, y) = A x + B y + C is the weight of
/// vertex k times |area|.
fn edges(p: [(f32, f32); 3], ar: f32) -> [(f32, f32, f32); 3] {
    let s = if ar > 0.0 { 1.0 } else { -1.0 };
    let edge = |p: (f32, f32), q: (f32, f32)| {
        let ea = -(q.1 - p.1) * s;
        let eb = (q.0 - p.0) * s;
        (ea, eb, -(ea * p.0 + eb * p.1))
    };
    [edge(p[1], p[2]), edge(p[2], p[0]), edge(p[0], p[1])]
}

/// A pixel exactly on an edge belongs to the side the edge's normal points right (or down).
#[inline]
fn owns(e: (f32, f32, f32)) -> bool { e.0 > 0.0 || (e.0 == 0.0 && e.1 > 0.0) }

/// Pixel bounds (x0, x1, y0, y1 inclusive) of a triangle inside the band and the clip.
fn bounds(p: [(f32, f32); 3], w: usize, by: usize, bh: usize, clip: Option<([u16; 4], bool)>) -> Option<(i32, i32, i32, i32)> {
    let [a, b, c] = p;
    let (wi, top, bot) = (w as i32, by as i32, (by + bh) as i32);
    let minx = a.0.min(b.0).min(c.0);
    let maxx = a.0.max(b.0).max(c.0);
    let miny = a.1.min(b.1).min(c.1);
    let maxy = a.1.max(b.1).max(c.1);
    if !(maxx >= 0.0 && maxy >= top as f32 && minx < wi as f32 && miny < bot as f32) { return None; }
    let (mut x0, mut y0, mut x1, mut y1) = ((minx.floor() as i32).max(0), (miny.floor() as i32).max(top),
                                            (maxx.ceil() as i32).min(wi - 1), (maxy.ceil() as i32).min(bot - 1));
    if let Some((cr, true)) = clip {
        x0 = x0.max(cr[0] as i32); y0 = y0.max(cr[1] as i32);
        x1 = x1.min(cr[2] as i32 - 1); y1 = y1.min(cr[3] as i32 - 1);
    }
    if x0 > x1 || y0 > y1 { None } else { Some((x0, x1, y0, y1)) }
}

/// A screen-space plane: value(x, y) = a x + b y + c.
#[derive(Clone, Copy, Default)]
struct Plane { a: f32, b: f32, c: f32 }

impl Plane {
    /// Through three vertex values, given the barycentric gradients.
    fn new(p: &[(f32, f32); 3], dx: &[f32; 3], dy: &[f32; 3], v: [f32; 3]) -> Self {
        let a = v[0] * dx[0] + v[1] * dx[1] + v[2] * dx[2];
        let b = v[0] * dy[0] + v[1] * dy[1] + v[2] * dy[2];
        Plane { a, b, c: v[0] - a * p[0].0 - b * p[0].1 }
    }
    #[inline]
    fn at(&self, x: f32, y: f32) -> f32 { self.a * x + self.b * y + self.c }
}


/// The region array's tiles (x, y in tiles) that get written out.
pub fn region_tiles(regs: &[u32], vram: &[u8]) -> Vec<(u32, u32)> {
    let words = if r(regs, FPU_PARAM_CFG) & 0x20_0000 != 0 { 6 } else { 5 };
    let mut a = r(regs, REGION_BASE);
    let mut tiles = Vec::new();
    for _ in 0..4096 {
        let c = read32(vram, a);
        let (tx, ty) = (c >> 2 & 0x3F, c >> 8 & 0x3F);
        // bit 28 = no write-out (a pass that keeps accumulating)
        if c & 0x1000_0000 == 0 && !tiles.contains(&(tx, ty)) { tiles.push((tx, ty)); }
        if c & 0x8000_0000 != 0 { break; }
        a = a.wrapping_add(words * 4);
    }
    tiles
}


impl<'a> Renderer<'a> {
    /// A band of rows y0 .. y0 + h, w wide.
    pub fn new(regs: &'a [u32], vram: &'a [u8], cache: &'a TexCache, w: usize, y0: usize, h: usize) -> Self {
        let n = w * h;
        let d = r(regs, FOG_DENSITY);
        Self {
            regs, vram, w, y0, h, cache, stats: Stats::default(),
            fog_den: (d >> 8 & 0xFF) as f32 / 128.0 * 2f32.powi((d & 0xFF) as i8 as i32),
            b: Bufs { col: vec![0; n], col2: vec![0; n], z: vec![0.0; n], id: vec![0; n], mask: Vec::new(), sorting: false, tfrags: Vec::new() },
        }
    }

    fn reg(&self, off: u32) -> u32 { r(self.regs, off) }

    fn mode(&self, p: &Poly, set1: bool) -> Mode {
        let isp = p.isp;
        let (tsp, tcw) = if set1 { (p.tsp1, p.tcw1) } else { (p.tsp, p.tcw) };
        let textured = isp & (1 << 25) != 0;
        Mode {
            textured, tsp, tcw,
            depth: isp >> 29,
            zwrite: isp & (1 << 26) == 0,
            gouraud: isp & (1 << 23) != 0,
            offset: textured && isp & (1 << 24) != 0,
            use_alpha: tsp & (1 << 20) != 0,
            tex_alpha: tsp & (1 << 19) == 0,
            shading: tsp >> 6 & 3,
            fog: tsp >> 22 & 3,
            clamp: tsp & (1 << 21) != 0,
            list: p.list,
            presort: self.reg(ISP_FEED_CFG) & 1 != 0,
            set1,
            shadow: 1.0,
        }
    }

    /// The texture of a parameter set (top level).
    fn texture(&self, m: &Mode) -> Option<Tex> {
        if !m.textured { return None; }
        Some(Tex::new(m.tsp, m.tcw, self.reg(TEXT_CONTROL), self.reg(PAL_RAM_CTRL)))
    }

    /// A texture level with its decoded texels.
    fn decoded(&self, m: &Mode, t: Tex, level: u32) -> (Tex, Texels) {
        let pal = &self.regs[PALETTE as usize / 4..PALETTE as usize / 4 + 1024];
        (t, self.cache.get(&t, (m.tcw, m.tsp & 0x3F, level), self.vram, pal))
    }

    /// Clear to the background plane: ISP_BACKGND_T points (from PARAM_BASE's 1 MB page)
    /// at an ISP word, TSP, TCW and three vertices of 3 + skip words.
    fn background(&mut self) {
        let t = self.reg(ISP_BACKGND_T);
        let depth = f32::from_bits(self.reg(ISP_BACKGND_D));
        let base = (self.reg(PARAM_BASE) & 0xF0_0000) + ((t & 0xFF_FFF8) >> 1);
        let skip = t >> 24 & 7;
        let rd = |k: u32| read32(self.vram, base + 4 * k);
        let isp = rd(0);
        let (tsp, tcw) = (rd(1), rd(2));
        let tex = isp & (1 << 25) != 0;
        let uv16 = isp & (1 << 22) != 0;
        let offset = isp & (1 << 24) != 0;
        let mut v = [Vert::default(); 3];
        for (k, vt) in v.iter_mut().enumerate() {
            let o = 3 + k as u32 * (3 + skip);
            vt.x = f32::from_bits(rd(o));
            vt.y = f32::from_bits(rd(o + 1));
            vt.z = f32::from_bits(rd(o + 2));
            let mut c = o + 3;
            if tex {
                if uv16 {
                    let uv = rd(c);
                    vt.u = f32::from_bits(uv & 0xFFFF_0000);
                    vt.v = f32::from_bits(uv << 16);
                    c += 1;
                } else {
                    vt.u = f32::from_bits(rd(c));
                    vt.v = f32::from_bits(rd(c + 1));
                    c += 2;
                }
            }
            vt.base = unpack(rd(c));
            if tex && offset { vt.off = unpack(rd(c + 1)); }
        }
        let p = Poly { list: LIST_OPAQUE, isp: isp | 7 << 29, tsp, tcw, first: 0, count: 3, clip: [0; 4], clip_mode: 0, pcw: 0, tsp1: 0, tcw1: 0 };
        let m = self.mode(&p, false);
        self.b.col.fill(0);
        let p3 = [(v[0].x, v[0].y), (v[1].x, v[1].y), (v[2].x, v[2].y)];
        if area(p3[0], p3[1], p3[2]).abs() > 1e-12 {
            self.tri(&m, [&v[0], &v[1], &v[2]], None, true, Pass::Draw(0));
        } else {
            // a degenerate background: flat colour
            let tex = self.texture(&m).map(|t| self.decoded(&m, t, 0));
            let c = self.shade(&m, tex.as_ref().map(|(t, d)| (t, &d[..])), v[0].base, v[0].off, v[0].u, v[0].v, v[0].z);
            self.b.col.fill(pack(c));
        }
        self.b.z.fill(depth);
        self.b.id.fill(0);
    }

    /// Draw a scene.
    pub fn draw(&mut self, scene: Option<&Scene>) {
        self.background();
        let Some(s) = scene else { return };
        let presort = self.reg(ISP_FEED_CFG) & 1 != 0;
        for list in [LIST_OPAQUE, LIST_PUNCH] {
            for (n, p) in s.polys.iter().enumerate().filter(|(_, p)| p.list == list) { self.strip(s, p, Pass::Draw(n as u32 + 1)); }
        }
        self.modifiers(s);
        // auto-sort: collect the fragments, then blend each pixel's back to front
        self.b.sorting = !presort;
        for (n, p) in s.polys.iter().enumerate().filter(|(_, p)| p.list == LIST_TRANS) { self.strip(s, p, Pass::Draw(n as u32 + 1)); }
        if self.b.sorting { self.resolve(); }
    }

    /// Blend the collected translucent fragments: per pixel, farthest (smallest 1/w) first,
    /// equal depths in the order sent (the sort is stable).
    fn resolve(&mut self) {
        let f = std::mem::take(&mut self.b.tfrags);
        self.b.sorting = false;
        if f.is_empty() { return; }
        // group by pixel keeping the order sent (counting sort), then sort each pixel's
        // few fragments by depth (insertion sort: stable)
        let n = self.w * self.h;
        let mut start = vec![0u32; n + 1];
        for fr in &f { start[fr.0 as usize + 1] += 1; }
        for i in 0..n { start[i + 1] += start[i]; }
        let mut next = start.clone();
        let mut g = vec![(0u32, 0f32, 0u32, 0u32); f.len()];
        for fr in &f { let k = &mut next[fr.0 as usize]; g[*k as usize] = *fr; *k += 1; }
        for i in 0..n {
            let run = &mut g[start[i] as usize..start[i + 1] as usize];
            if run.is_empty() { continue; }
            for k in 1..run.len() {
                let mut j = k;
                while j > 0 && run[j - 1].1 > run[j].1 { run.swap(j - 1, j); j -= 1; }
            }
            for &(_, _, col, tsp) in run.iter() { self.b.blend(i, unpack(col), tsp); }
        }
    }

    /// Opaque modifier volumes: mark pixels, then re-shade the marked pixels of strips
    /// with the modifier bit.
    fn modifiers(&mut self, s: &Scene) {
        let tris: Vec<&ModTri> = s.mods.iter().filter(|t| t.list == LIST_OPAQUE_MOD).collect();
        let affected = s.polys.iter().any(|p| p.pcw & 0x80 != 0 && (p.list == LIST_OPAQUE || p.list == LIST_PUNCH));
        if tris.is_empty() || !affected { return; }
        let n = self.w * self.h;
        let mut mask = vec![false; n];
        let mut parity = vec![false; n];
        for (k, t) in tris.iter().enumerate() {
            let p = [(t.v[0][0], t.v[0][1]), (t.v[1][0], t.v[1][1]), (t.v[2][0], t.v[2][1])];
            let z = [t.v[0][2], t.v[1][2], t.v[2][2]];
            let depth = &self.b.z;
            raster(self.w, self.y0, self.h, p, |i, l| {
                // the volume's surface in front of what was drawn
                if l[0] * z[0] + l[1] * z[1] + l[2] * z[2] > depth[i] { parity[i] = !parity[i]; }
            });
            let last = tris.get(k + 1).is_none_or(|nx| nx.group != t.group);
            if t.mode != 0 && last {
                for i in 0..n {
                    mask[i] |= if t.mode == 1 { parity[i] } else { !parity[i] };
                }
                parity.fill(false);
                self.stats.volumes += 1;
            }
        }
        self.b.mask = mask;
        for (k, p) in s.polys.iter().enumerate() {
            if p.pcw & 0x80 != 0 && (p.list == LIST_OPAQUE || p.list == LIST_PUNCH) {
                self.strip(s, p, Pass::Modify(k as u32 + 1));
            }
        }
    }

    fn strip(&mut self, s: &Scene, p: &Poly, pass: Pass) {
        let modify = matches!(pass, Pass::Modify(_));
        let two = p.pcw & 0x40 != 0;
        let mut m = self.mode(p, modify && two);
        if modify && !two {
            let sc = self.reg(FPU_SHAD_SCALE);
            m.shadow = if sc & 0x100 != 0 { (sc & 0xFF) as f32 / 256.0 } else { 1.0 };
        }
        let v = &s.verts[p.first as usize..(p.first + p.count).min(s.verts.len() as u32) as usize];
        let clip = match p.clip_mode {
            2 => Some((p.clip, true)),
            3 => Some((p.clip, false)),
            _ => None,
        };
        let cull = p.isp >> 27 & 3;
        let cull_val = f32::from_bits(self.reg(FPU_CULL_VAL)).abs();
        if !modify { self.stats.polys += 1; }
        for k in 0..v.len().saturating_sub(2) {
            let tv = [&v[k], &v[k + 1], &v[k + 2]];
            if cull != 0 {
                let mut ar = area((tv[0].x, tv[0].y), (tv[1].x, tv[1].y), (tv[2].x, tv[2].y));
                if k & 1 != 0 { ar = -ar; }
                if ar.abs() * 0.5 < cull_val || (cull == 2 && ar > 0.0) || (cull == 3 && ar < 0.0) {
                    if !modify { self.stats.culled += 1; }
                    continue;
                }
            }
            self.tri(&m, tv, clip, false, pass);
        }
    }

    /// One triangle. `fill` covers the whole band (the background plane).
    fn tri(&mut self, m: &Mode, v: [&Vert; 3], clip: Option<([u16; 4], bool)>, fill: bool, pass: Pass) {
        let p = [(v[0].x, v[0].y), (v[1].x, v[1].y), (v[2].x, v[2].y)];
        let ar = area(p[0], p[1], p[2]);
        if !(ar.abs() > 1e-12) { return; }
        let w = self.w;
        let (x0, x1, ya, yb) = if fill {
            (0, w as i32 - 1, self.y0 as i32, (self.y0 + self.h) as i32 - 1)
        } else {
            match bounds(p, w, self.y0, self.h, clip) { Some(b) => b, None => return }
        };
        let tex = self.texture(m);
        let levels: Vec<(Tex, Texels)> = match tex {
            Some(t) if t.levels > 0 => (0..=t.levels).map(|k| self.decoded(m, t.level(k), k)).collect(),
            Some(t) => vec![self.decoded(m, t, 0)],
            None => Vec::new(),
        };
        let mip = levels.len() > 1;
        let size = levels.first().map_or(0.0, |t| t.0.sx as f32);
        let adjust = match m.tsp >> 8 & 0xF { 0 => 1.0, d => d as f32 / 4.0 };
        // barycentric gradients, then planes of z (= 1/w) and of attribute × z
        let dx = [(p[1].1 - p[2].1) / ar, (p[2].1 - p[0].1) / ar, (p[0].1 - p[1].1) / ar];
        let dy = [(p[2].0 - p[1].0) / ar, (p[0].0 - p[2].0) / ar, (p[1].0 - p[0].0) / ar];
        let z3 = [v[0].z, v[1].z, v[2].z];
        let pz = Plane::new(&p, &dx, &dy, z3);
        let pl = |f: &dyn Fn(&Vert) -> f32| Plane::new(&p, &dx, &dy, [f(v[0]) * z3[0], f(v[1]) * z3[1], f(v[2]) * z3[2]]);
        let (pu, pv) = if tex.is_none() { (Plane::default(), Plane::default()) }
            else if m.set1 { (pl(&|q| q.u1), pl(&|q| q.v1)) } else { (pl(&|q| q.u), pl(&|q| q.v)) };
        let need_off = m.offset || m.fog == 1;
        let mut pc = [Plane::default(); 8];
        if m.gouraud {
            for ch in 0..4 {
                pc[ch] = if m.set1 { pl(&|q| q.base1[ch]) } else { pl(&|q| q.base[ch]) };
                if need_off { pc[4 + ch] = if m.set1 { pl(&|q| q.off1[ch]) } else { pl(&|q| q.off[ch]) }; }
            }
        }
        let (flat_b, flat_o) = if m.set1 { (v[2].base1, v[2].off1) } else { (v[2].base, v[2].off) };
        let e = edges(p, ar);
        let own = [owns(e[0]), owns(e[1]), owns(e[2])];
        let pt_ref = (self.reg(PT_ALPHA_REF) & 0xFF) as f32 / 255.0;
        let test = if m.list == LIST_TRANS && !m.presort { 6 } else { m.depth };
        let modify = matches!(pass, Pass::Modify(_));
        for y in ya..=yb {
            let py = y as f32 + 0.5;
            let row = (y as usize - self.y0) * w;
            // the row's span from the edges (one pixel of slack each side: the exact test
            // below decides)
            let (mut sx0, mut sx1) = (x0, x1);
            if !fill {
                let mut empty = false;
                for k in 0..3 {
                    let (a, rest) = (e[k].0, e[k].1 * py + e[k].2);
                    if a > 0.0 { sx0 = sx0.max((-rest / a - 0.5).floor() as i32 - 1); }
                    else if a < 0.0 { sx1 = sx1.min((-rest / a - 0.5).ceil() as i32 + 1); }
                    else if rest < 0.0 { empty = true; }
                }
                if empty || sx0 > sx1 { continue; }
            }
            let (x0, x1) = (sx0, sx1);
            let px0 = x0 as f32 + 0.5;
            let mut ev = [e[0].0 * px0 + e[0].1 * py + e[0].2, e[1].0 * px0 + e[1].1 * py + e[1].2, e[2].0 * px0 + e[2].1 * py + e[2].2];
            // plane values at the row start, stepped by a per pixel
            let mut zv = pz.at(px0, py);
            let (mut uz, mut vz) = (pu.at(px0, py), pv.at(px0, py));
            let mut cz = [0f32; 8];
            if m.gouraud { for ch in 0..8 { cz[ch] = pc[ch].at(px0, py); } }
            for x in x0..=x1 {
                let (cur, z, uzc, vzc, czc) = (ev, zv, uz, vz, cz);
                ev[0] += e[0].0; ev[1] += e[1].0; ev[2] += e[2].0;
                zv += pz.a; uz += pu.a; vz += pv.a;
                if m.gouraud { for ch in 0..8 { cz[ch] += pc[ch].a; } }
                if !fill {
                    if cur[0] < 0.0 || cur[1] < 0.0 || cur[2] < 0.0 { continue; }
                    if (cur[0] == 0.0 && !own[0]) || (cur[1] == 0.0 && !own[1]) || (cur[2] == 0.0 && !own[2]) { continue; }
                    if let Some((cr, false)) = clip {
                        if x >= cr[0] as i32 && x < cr[2] as i32 && y >= cr[1] as i32 && y < cr[3] as i32 { continue; }
                    }
                }
                let i = row + x as usize;
                match pass {
                    Pass::Draw(_) if !fill => if !depth_pass(test, z, self.b.z[i]) { continue; },
                    Pass::Modify(id) => {
                        if self.b.id[i] != id || !self.b.mask.get(i).copied().unwrap_or(false) || z != self.b.z[i] { continue; }
                    }
                    _ => {}
                }
                let inv = if z.abs() > 1e-30 { 1.0 / z } else { 0.0 };
                let (bc, oc) = if m.gouraud {
                    ([czc[0] * inv, czc[1] * inv, czc[2] * inv, czc[3] * inv],
                     if need_off { [czc[4] * inv, czc[5] * inv, czc[6] * inv, czc[7] * inv] } else { [0.0; 4] })
                } else { (flat_b, flat_o) };
                let (u, vv) = (uzc * inv, vzc * inv);
                let t = if !mip { levels.first() } else {
                    // texels per pixel: d(u)/dx = (a_u - u a_z) / z, ...
                    let (dudx, dvdx) = ((pu.a - u * pz.a) * inv, (pv.a - vv * pz.a) * inv);
                    let (dudy, dvdy) = ((pu.b - u * pz.b) * inv, (pv.b - vv * pz.b) * inv);
                    let rho = dudx.hypot(dvdx).max(dudy.hypot(dvdy)) * size * adjust;
                    let k = if rho > 1.0 { (rho.log2().round() as usize).min(levels.len() - 1) } else { 0 };
                    levels.get(k)
                };
                let col = self.shade(m, t.map(|(t, d)| (t, &d[..])), bc, oc, u, vv, z);
                let b = &mut self.b;
                match m.list {
                    LIST_PUNCH if !modify => {
                        if col[0] < pt_ref { continue; }
                        b.col[i] = pack(col);
                        if m.zwrite { b.z[i] = z; }
                        if let Pass::Draw(id) = pass { b.id[i] = id; }
                    }
                    LIST_TRANS if b.sorting => b.tfrags.push((i as u32, z, pack(col), m.tsp)),
                    LIST_TRANS => {
                        b.blend(i, col, m.tsp);
                        if m.presort && m.zwrite { b.z[i] = z; }
                    }
                    _ => {
                        b.col[i] = pack(col);
                        if let Pass::Draw(id) = pass {
                            if m.zwrite && !fill { b.z[i] = z; }
                            b.id[i] = id;
                        }
                    }
                }
                self.stats.pixels += 1;
            }
        }
    }

    /// Texture, shading instruction, fog, clamp: the fragment colour (A R G B).
    #[allow(clippy::too_many_arguments)]
    fn shade(&self, m: &Mode, tex: Option<(&Tex, &[u32])>, base: [f32; 4], off: [f32; 4], u: f32, v: f32, z: f32) -> [f32; 4] {
        let mut b = base;
        if !m.use_alpha { b[0] = 1.0; }
        let o = if m.offset { off } else { [0.0; 4] };
        let mut c = match tex {
            None => b,
            Some((t, d)) => {
                let mut tc = unpack(t.sample_decoded(d, u, v));
                if !m.tex_alpha { tc[0] = 1.0; }
                let mut c = match m.shading {
                    0 => tc,
                    1 => [tc[0], tc[1] * b[1], tc[2] * b[2], tc[3] * b[3]],
                    2 => {
                        let ta = tc[0];
                        [b[0], tc[1] * ta + b[1] * (1.0 - ta), tc[2] * ta + b[2] * (1.0 - ta), tc[3] * ta + b[3] * (1.0 - ta)]
                    }
                    _ => [tc[0] * b[0], tc[1] * b[1], tc[2] * b[2], tc[3] * b[3]],
                };
                for ch in 1..4 { c[ch] = (c[ch] + o[ch]).min(1.0); }
                c
            }
        };
        for ch in 1..4 { c[ch] *= m.shadow; }
        match m.fog {
            0 | 3 => {
                let f = self.fog_table(z);
                let fc = unpack(self.reg(FOG_COL_RAM) | 0xFF00_0000);
                if m.fog == 0 {
                    for ch in 1..4 { c[ch] = c[ch] * (1.0 - f) + fc[ch] * f; }
                } else {
                    c = [f, fc[1], fc[2], fc[3]];
                }
            }
            1 => {
                let f = o[0];
                let fc = unpack(self.reg(FOG_COL_VERT) | 0xFF00_0000);
                for ch in 1..4 { c[ch] = c[ch] * (1.0 - f) + fc[ch] * f; }
            }
            _ => {}
        }
        if m.clamp {
            let (lo, hi) = (unpack(self.reg(FOG_CLAMP_MIN)), unpack(self.reg(FOG_CLAMP_MAX)));
            for ch in 0..4 { c[ch] = c[ch].clamp(lo[ch], hi[ch].max(lo[ch])); }
        }
        c
    }

    /// Fog density from the table: FOG_DENSITY (mantissa bits 15-8 as 1.7, signed exponent
    /// bits 7-0) times w (= 1 / the vertex z), clamped to 1..256, indexes the 128-entry
    /// table by exponent (3 bits) and the top 4 mantissa bits; the entry's two bytes
    /// (high, low) are interpolated by the rest.
    fn fog_table(&self, z: f32) -> f32 {
        let x = if z > 0.0 { (self.fog_den / z).clamp(1.0, 255.999) } else { 255.999 };
        // exponent and mantissa straight from the float's bits (x in 1..256)
        let bits = x.to_bits();
        let e = ((bits >> 23) as i32 - 127).clamp(0, 7) as usize;
        let m = (bits & 0x7F_FFFF) as f32 / (1u32 << 19) as f32; // 0..16
        let idx = (e * 16 + m as usize).min(127);
        let frac = m - (m as usize) as f32;
        let entry = self.reg(FOG_TABLE + 4 * idx as u32);
        let (a, b) = ((entry >> 8 & 0xFF) as f32, (entry & 0xFF) as f32);
        (a * (1.0 - frac) + b * frac) / 255.0
    }
}

/// Write the listed tiles of an accumulation buffer (`col`, w x h) to the framebuffer
/// (FB_W_CTRL pack mode, FB_W_SOF1 / LINESTRIDE, FB_X/Y_CLIP; SCALER_CTL horizontal halving
/// and vertical 0x800 halving).
pub fn write_out(regs: &[u32], col: &[u32], w: usize, h: usize, tiles: &[(u32, u32)], vram: &mut [u8]) {
        let reg = |o: u32| r(regs, o);
        let ctrl = reg(FB_W_CTRL);
        let pack_mode = ctrl & 7;
        let stride = (reg(FB_W_LINESTRIDE) & 0x1FF) * 8;
        let scaler = reg(SCALER_CTL);
        let base = if scaler & 0x6_0000 == 0x6_0000 { reg(FB_W_SOF2) } else { reg(FB_W_SOF1) };
        let (xc, yc) = (reg(FB_X_CLIP), reg(FB_Y_CLIP));
        let (cx0, cx1, cy0, cy1) = (xc & 0x7FF, xc >> 16 & 0x7FF, yc & 0x3FF, yc >> 16 & 0x3FF);
        let hs = if scaler & 0x1_0000 != 0 { 2 } else { 1 };
        let vs = if scaler & 0xFFFF == 0x800 { 2 } else { 1 };
        let bpp = match pack_mode { 4 => 3, 5 | 6 => 4, _ => 2 };
        let kval = ctrl >> 16 & 0xFF;
        let athr = ctrl >> 8 & 0xFF;
        let m = vram.len() - 1;
        for &(tx, ty) in tiles {
            let (sx0, sy0) = (tx as usize * 32, ty as usize * 32);
            if sx0 >= w || sy0 >= h { continue; }
            for oy in (sy0 / vs)..((sy0 + 32) / vs) {
                if (oy as u32) < cy0 || oy as u32 > cy1 { continue; }
                for ox in (sx0 / hs)..((sx0 + 32) / hs) {
                    if (ox as u32) < cx0 || ox as u32 > cx1 { continue; }
                    // average the source pixels that scale down into this one
                    let mut acc = [0u32; 4];
                    for yy in 0..vs {
                        for xx in 0..hs {
                            let (x, y) = (ox * hs + xx, oy * vs + yy);
                            let c = if x < w && y < h { col[y * w + x] } else { 0 };
                            for (k, s) in [24u32, 16, 8, 0].iter().enumerate() { acc[k] += c >> s & 0xFF; }
                        }
                    }
                    let n = (hs * vs) as u32;
                    let (a, rr, g, b) = (acc[0] / n, acc[1] / n, acc[2] / n, acc[3] / n);
                    let val = match pack_mode {
                        0 => (kval >> 7) << 15 | (rr >> 3) << 10 | (g >> 3) << 5 | b >> 3,
                        1 => (rr >> 3) << 11 | (g >> 2) << 5 | b >> 3,
                        2 => (a >> 4) << 12 | (rr >> 4) << 8 | (g >> 4) << 4 | b >> 4,
                        3 => ((a >= athr) as u32) << 15 | (rr >> 3) << 10 | (g >> 3) << 5 | b >> 3,
                        4 => rr << 16 | g << 8 | b,
                        5 => kval << 24 | rr << 16 | g << 8 | b,
                        _ => a << 24 | rr << 16 | g << 8 | b,
                    };
                    let addr = base.wrapping_add(oy as u32 * stride + ox as u32 * bpp);
                    for k in 0..bpp {
                        let a = addr.wrapping_add(k) & 0x7F_FFFF;
                        // render to texture (SOF bit 24): the 64-bit (texture) path
                        let i = if base & 0x100_0000 != 0 { a as usize } else { map32(a) };
                        vram[i & m] = (val >> (8 * k)) as u8;
                    }
                }
            }
        }
}

/// STARTRENDER: draw the scene PARAM_BASE names into the framebuffer. The buffer is split
/// into horizontal bands drawn on threads of their own (each band runs the whole scene,
/// clipped to its rows).
pub fn render(regs: &[u32], scene: Option<&Scene>, vram: &mut [u8]) -> Stats {
    let tiles = region_tiles(regs, vram);
    let (mut w, mut h) = (32, 32);
    for &(tx, ty) in &tiles {
        w = w.max(tx as usize * 32 + 32);
        h = h.max(ty as usize * 32 + 32);
    }
    let (w, h) = (w.min(MAX_W), h.min(MAX_H));
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 8);
    let bh = h.div_ceil(threads).max(16);
    let shared: &[u8] = vram;
    let cache = TexCache::default();
    let cache = &cache;
    let bands: Vec<(Vec<u32>, Stats)> = std::thread::scope(|sc| {
        let jobs: Vec<_> = (0..h).step_by(bh).map(|y0| {
            sc.spawn(move || {
                let mut rd = Renderer::new(regs, shared, cache, w, y0, bh.min(h - y0));
                rd.draw(scene);
                (std::mem::take(&mut rd.b.col), rd.stats)
            })
        }).collect();
        jobs.into_iter().map(|j| j.join().expect("render band")).collect()
    });
    let mut col = Vec::with_capacity(w * h);
    let mut stats = Stats { tiles: tiles.len() as u32, ..Stats::default() };
    for (k, (c, st)) in bands.into_iter().enumerate() {
        col.extend_from_slice(&c);
        stats.pixels += st.pixels;
        if k == 0 { stats.polys = st.polys; stats.culled = st.culled; stats.volumes = st.volumes; }
    }
    write_out(regs, &col, w, h, &tiles, vram);
    stats
}
