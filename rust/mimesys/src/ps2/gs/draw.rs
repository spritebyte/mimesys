//! Drawing: points, lines, triangles and sprites through the GS pixel pipeline —
//! scissor, texture (point sampled, CLAMP wrap modes, CLUTs, TEXA), the texture function
//! (TFX), fog, alpha test, destination alpha test, Z test, alpha blending (PABE, COLCLAMP),
//! FBA, FBMSK / ZMSK. Pixels are sampled at integer positions with a top-left fill rule.
//! Bilinear filtering, mipmaps and dithering come later (stage 5).

use super::mem::{self, *};
use super::{bits, Gs, Vertex, ALPHA_1, CLAMP_1, COLCLAMP, FBA_1, FOGCOL, FRAME_1, PABE, SCISSOR_1, TEST_1, TEX0_1, TEXA, ZBUF_1};

/// A primitive's settings, decoded once (everything the pixel pipeline needs per pixel)
#[derive(Clone, Copy)]
pub struct Ctx {
    fbp: u32, fbw: u32, fpsm: u32, f16: bool,
    /// FBMSK as stored for the frame format, and with the alpha bits kept too (AFAIL 3)
    fmask: u32, fmask_keep_a: u32,
    zbp: u32, zpsm: u32, zmsk: bool, zmax: u32,
    sc: [i32; 4],
    // TEST
    ate: bool, atst: u32, aref: i32, afail: u32,
    date: bool, datm: u32,
    zte: bool, ztst: u32,
    // ALPHA
    abe: bool, pabe: bool, clamp_col: bool, fba: bool,
    asel: [u32; 3], csel: u32, fix: i32,
    tme: bool, fge: bool, fst: bool, iip: bool,
    // the texture
    tbp: u32, tbw: u32, tpsm: u32, tcc: bool, tfx: u32,
    cpsm: u32, clut_base: u32,
    wms: u32, wmt: u32, minu: u32, maxu: u32, minv: u32, maxv: u32,
    ta0: i32, ta1: i32, aem: bool,
    tw: u32, th: u32,
    fogcol: [i32; 3],
}

/// A pixel's colour (RGB) and alpha from a stored value (display and blending)
pub fn to_rgba(p: u32, psm: u32) -> ([u8; 3], u8) {
    match psm {
        PSMCT32 | PSMZ32 => ([p as u8, (p >> 8) as u8, (p >> 16) as u8], (p >> 24) as u8),
        PSMCT24 | PSMZ24 => ([p as u8, (p >> 8) as u8, (p >> 16) as u8], 0x80),
        _ => {
            let e = |v: u32| ((v << 3) | (v >> 2)) as u8;
            ([e(p & 31), e(p >> 5 & 31), e(p >> 10 & 31)], if p & 0x8000 != 0 { 0x80 } else { 0 })
        }
    }
}

fn is16(psm: u32) -> bool { matches!(psm, PSMCT16 | PSMCT16S | PSMZ16 | PSMZ16S) }

fn pack16(c: u32) -> u32 { (c >> 3 & 31) | (c >> 11 & 31) << 5 | (c >> 19 & 31) << 10 | (c >> 31) << 15 }

impl Gs {
    fn ctx(&self) -> Ctx {
        let attr = self.prim_attr();
        let k = bits(attr, 9, 1) as usize;
        let r = &self.regs;
        let frame = r[FRAME_1 + k];
        let zbuf = r[ZBUF_1 + k];
        let sc = r[SCISSOR_1 + k];
        let tex0 = r[TEX0_1 + k];
        let fog = r[FOGCOL];
        let t = r[TEST_1 + k];
        let al = r[ALPHA_1 + k];
        let cl = r[CLAMP_1 + k];
        let texa = r[TEXA];
        let fpsm = bits(frame, 24, 6);
        let fbmsk = (frame >> 32) as u32;
        let f16 = is16(fpsm);
        let zpsm = bits(zbuf, 24, 4) | 0x30;
        let cpsm = bits(tex0, 51, 4);
        let csa = bits(tex0, 56, 5);
        Ctx {
            fbp: bits(frame, 0, 9) * 32, fbw: bits(frame, 16, 6), fpsm, f16,
            fmask: if f16 { pack16(fbmsk) } else { fbmsk },
            fmask_keep_a: if f16 { pack16(fbmsk | 0xFF00_0000) } else { fbmsk | 0xFF00_0000 },
            zbp: bits(zbuf, 0, 9) * 32, zpsm, zmsk: zbuf >> 32 & 1 != 0,
            zmax: match zpsm { PSMZ32 => u32::MAX, PSMZ24 => 0xFF_FFFF, _ => 0xFFFF },
            sc: [bits(sc, 0, 11) as i32, bits(sc, 16, 11) as i32, bits(sc, 32, 11) as i32, bits(sc, 48, 11) as i32],
            ate: t & 1 != 0, atst: bits(t, 1, 3), aref: bits(t, 4, 8) as i32, afail: bits(t, 12, 2),
            date: t >> 14 & 1 != 0, datm: (t >> 15 & 1) as u32,
            zte: t >> 16 & 1 != 0, ztst: bits(t, 17, 2),
            abe: attr >> 6 & 1 != 0 || attr >> 7 & 1 != 0,
            pabe: r[PABE] & 1 != 0,
            clamp_col: r[COLCLAMP] & 1 != 0,
            fba: r[FBA_1 + k] & 1 != 0,
            asel: [bits(al, 0, 2), bits(al, 2, 2), bits(al, 6, 2)], csel: bits(al, 4, 2), fix: bits(al, 32, 8) as i32,
            tme: attr >> 4 & 1 != 0,
            fge: attr >> 5 & 1 != 0,
            fst: attr >> 8 & 1 != 0,
            iip: attr >> 3 & 1 != 0,
            tbp: bits(tex0, 0, 14), tbw: bits(tex0, 14, 6), tpsm: bits(tex0, 20, 6),
            tcc: tex0 >> 34 & 1 != 0, tfx: bits(tex0, 35, 2),
            cpsm, clut_base: if cpsm == 0 { (csa & 15) * 16 } else { csa * 16 },
            wms: bits(cl, 0, 2), wmt: bits(cl, 2, 2),
            minu: bits(cl, 4, 10), maxu: bits(cl, 14, 10), minv: bits(cl, 24, 10), maxv: bits(cl, 34, 10),
            ta0: bits(texa, 0, 8) as i32, ta1: bits(texa, 32, 8) as i32, aem: texa >> 15 & 1 != 0,
            tw: 1 << bits(tex0, 26, 4).min(10),
            th: 1 << bits(tex0, 30, 4).min(10),
            fogcol: [bits(fog, 0, 8) as i32, bits(fog, 8, 8) as i32, bits(fog, 16, 8) as i32],
        }
    }

    // ------------------------------------------------------------------ batching

    pub fn draw_point(&mut self, v: &Vertex) { self.push(Kind::Point(*v)); }
    pub fn draw_line(&mut self, a: &Vertex, b: &Vertex) { self.push(Kind::Line(*a, *b)); }
    pub fn draw_sprite(&mut self, a: &Vertex, b: &Vertex) { self.push(Kind::Sprite(*a, *b)); }
    pub fn draw_triangle(&mut self, a: &Vertex, b: &Vertex, c: &Vertex) { self.push(Kind::Tri(*a, *b, *c)); }

    /// Queue a primitive with its settings. A texture reading pages the batch has drawn
    /// to flushes the batch first; one reading its own frame / Z pages is drawn alone.
    fn push(&mut self, k: Kind) {
        let c = self.ctx();
        let (vs, nv) = match &k {
            Kind::Point(a) => ([*a, *a, *a], 1),
            Kind::Line(a, b) | Kind::Sprite(a, b) => ([*a, *b, *b], 2),
            Kind::Tri(a, b, d) => ([*a, *b, *d], 3),
        };
        let (mut x0, mut x1, mut y0, mut y1) = (i32::MAX, i32::MIN, i32::MAX, i32::MIN);
        for v in &vs[..nv] { x0 = x0.min(v.x >> 4); x1 = x1.max((v.x + 15) >> 4); y0 = y0.min(v.y >> 4); y1 = y1.max((v.y + 15) >> 4); }
        let y0 = y0.max(c.sc[2]).max(0);
        let y1 = y1.min(c.sc[3]);
        let x0 = x0.max(c.sc[0]).max(0);
        let x1 = x1.min(c.sc[1]);
        if y0 > y1 || x0 > x1 { return; }
        // the buffers it draws to; a different buffer sharing pages with one the batch
        // already draws to (or this frame sharing pages with this Z buffer) could put one
        // byte in two threads' rows, so that flushes / draws alone
        let fr = Target { bp: c.fbp, bw: c.fbw, psm: c.fpsm, pages: pages_of(c.fbp, c.fbw, ph(c.fpsm), y0, y1) };
        let zr = c.zte.then(|| Target { bp: c.zbp, bw: c.fbw, psm: c.zpsm, pages: pages_of(c.zbp, c.fbw, ph(c.zpsm), y0, y1) });
        let mut alone = zr.as_ref().is_some_and(|z| z.pages.overlaps(&fr.pages));
        let clash = |t: &Target, b: &Batch| b.targets.iter().any(|e| (e.bp, e.bw, e.psm) != (t.bp, t.bw, t.psm) && e.pages.overlaps(&t.pages));
        if clash(&fr, &self.batch) || zr.as_ref().is_some_and(|z| clash(z, &self.batch)) { self.flush(); }
        let own = fr.pages | zr.as_ref().map_or(Pages::default(), |z| z.pages);
        if c.tme {
            let tex = pages_of(c.tbp, c.tbw, ph(c.tpsm), 0, c.th as i32 - 1);
            if tex.overlaps(&self.batch.written) { self.flush(); }
            alone |= tex.overlaps(&own);
        }
        self.batch.written = self.batch.written | own;
        for t in [Some(fr), zr].into_iter().flatten() {
            match self.batch.targets.iter_mut().find(|e| (e.bp, e.bw, e.psm) == (t.bp, t.bw, t.psm)) {
                Some(e) => e.pages = e.pages | t.pages,
                None => self.batch.targets.push(t),
            }
        }
        self.batch.area += ((x1 - x0 + 1) as u64) * ((y1 - y0 + 1) as u64);
        self.batch.cmds.push(Cmd { c, k, y0, y1 });
        if alone || self.batch.cmds.len() >= 4096 { self.flush_with(if alone { 1 } else { 0 }); }
    }

    /// Draw everything queued
    pub fn flush(&mut self) { self.flush_with(0); }

    /// `threads`: 0 picks (one thread for small batches)
    fn flush_with(&mut self, threads: usize) {
        if self.batch.cmds.is_empty() { return; }
        let t0 = std::time::Instant::now();
        let batch = std::mem::take(&mut self.batch);
        let n = if threads != 0 { threads } else if batch.area < 40_000 { 1 } else { draw_threads() };
        let clut = &self.clut[..];
        if n <= 1 {
            let mut r = Raster { vram: &mut self.vram, clut, y0: 0, y1: 2048, pixels: 0 };
            for cmd in &batch.cmds { r.draw(cmd); }
            self.pixels += r.pixels;
            self.draw_ns += t0.elapsed().as_nanos() as u64;
            return;
        }
        // 16-row bands handed out as threads finish; each band runs every primitive that
        // reaches it, in order, so every pixel sees the same sequence as one thread would
        const BAND: i32 = 16;
        let (lo, hi) = batch.cmds.iter().fold((i32::MAX, i32::MIN), |(l, h), c| (l.min(c.y0), h.max(c.y1)));
        let nb = ((hi - lo) / BAND + 1) as usize;
        let next = std::sync::atomic::AtomicUsize::new(0);
        let total = std::sync::atomic::AtomicU64::new(0);
        let vram = VramPtr(self.vram.as_mut_ptr());
        let cmds = &batch.cmds;
        std::thread::scope(|sc| {
            for _ in 0..n.min(nb) {
                let (next, total, vram) = (&next, &total, &vram);
                sc.spawn(move || {
                    // SAFETY: bands are disjoint rows, so no two threads touch one pixel of a
                    // frame or Z buffer; textures they read aren't written in this batch
                    let mem = unsafe { std::slice::from_raw_parts_mut(vram.0, mem::VRAM_SIZE) };
                    let mut r = Raster { vram: mem, clut, y0: 0, y1: 0, pixels: 0 };
                    loop {
                        let k = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if k >= nb { break; }
                        r.y0 = lo + k as i32 * BAND;
                        r.y1 = r.y0 + BAND;
                        for cmd in cmds { if cmd.y1 >= r.y0 && cmd.y0 < r.y1 { r.draw(cmd); } }
                    }
                    total.fetch_add(r.pixels, std::sync::atomic::Ordering::Relaxed);
                });
            }
        });
        self.pixels += total.into_inner();
        self.draw_ns += t0.elapsed().as_nanos() as u64;
    }
}

struct VramPtr(*mut u8);
unsafe impl Sync for VramPtr {}
unsafe impl Send for VramPtr {}

/// Drawing threads: all cores but one (the emulated CPUs wait while the GS draws)
pub fn draw_threads() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        // PS2_GS_THREADS=n overrides (testing)
        if let Some(n) = std::env::var("PS2_GS_THREADS").ok().and_then(|v| v.parse::<usize>().ok()) { return n.clamp(1, 32); }
        std::thread::available_parallelism().map_or(1, |n| n.get()).saturating_sub(1).clamp(1, 12)
    })
}

/// A primitive waiting to be drawn
#[derive(Clone, Copy)]
pub enum Kind { Point(Vertex), Line(Vertex, Vertex), Sprite(Vertex, Vertex), Tri(Vertex, Vertex, Vertex) }

#[derive(Clone, Copy)]
pub struct Cmd { c: Ctx, k: Kind, y0: i32, y1: i32 }

/// The queue (not saved: it's empty between frames)
#[derive(Default)]
pub struct Batch { cmds: Vec<Cmd>, area: u64, written: Pages, targets: Vec<Target> }

/// A buffer the batch draws to
#[derive(Clone, Copy)]
struct Target { bp: u32, bw: u32, psm: u32, pages: Pages }

/// A set of the 512 8 KB pages
#[derive(Clone, Copy, Default)]
pub struct Pages([u64; 8]);

impl std::ops::BitOr for Pages {
    type Output = Pages;
    fn bitor(self, o: Pages) -> Pages { Pages(std::array::from_fn(|k| self.0[k] | o.0[k])) }
}

impl Pages {
    fn overlaps(&self, o: &Pages) -> bool { (0..8).any(|k| self.0[k] & o.0[k] != 0) }
}

/// Page height of a format (pages are 64 or 128 pixels wide; `bw` is in 64s)
fn ph(psm: u32) -> i32 {
    match mem::bpp(psm) { 32 | 24 => 32, 16 => 64, 8 => 64, _ => 128 }
}

/// The pages rows y0..=y1 of a buffer at block `bp` with width `bw` cover (whole rows of
/// pages: an overestimate is only a needless flush)
fn pages_of(bp: u32, bw: u32, ph: i32, y0: i32, y1: i32) -> Pages {
    let mut p = Pages::default();
    let w = bw.max(1);
    let (r0, r1) = ((y0.max(0) / ph) as u32, (y1.max(0) / ph) as u32);
    for r in r0..=r1.min(r0 + 64) {
        for k in 0..w {
            let n = ((bp / 32 + r * w + k) & 511) as usize;
            p.0[n >> 6] |= 1 << (n & 63);
        }
    }
    p
}

/// Where a batch draws: the memory, the CLUT, the rows this thread owns
struct Raster<'a> {
    vram: &'a mut [u8],
    clut: &'a [u32],
    y0: i32,
    y1: i32,
    pixels: u64,
}

impl Raster<'_> {
    fn draw(&mut self, cmd: &Cmd) {
        let c = &cmd.c;
        match &cmd.k {
            Kind::Point(v) => self.draw_point(c, v),
            Kind::Line(a, b) => self.draw_line(c, a, b),
            Kind::Sprite(a, b) => self.draw_sprite(c, a, b),
            Kind::Tri(a, b, d) => self.draw_triangle(c, a, b, d),
        }
    }

    // ------------------------------------------------------------------ primitives

    fn draw_point(&mut self, c: &Ctx, v: &Vertex) {
        let c = *c;
        let (u, t) = tex_coord(&c, v.s, v.t, v.q, v.u, v.v);
        self.plot(&c, (v.x + 8) >> 4, (v.y + 8) >> 4, v.z, v.rgba.map(|x| x as i32), u, t, v.f as i32);
    }

    fn draw_line(&mut self, c: &Ctx, a: &Vertex, b: &Vertex) {
        let c = *c;
        let (x0, y0, x1, y1) = (a.x as f32 / 16.0, a.y as f32 / 16.0, b.x as f32 / 16.0, b.y as f32 / 16.0);
        let n = (x1 - x0).abs().max((y1 - y0).abs()).ceil().max(1.0) as i32;
        for i in 0..n {
            let k = i as f32 / n as f32;
            let lerp = |p: f32, q: f32| p + (q - p) * k;
            let rgba = if c.iip { core::array::from_fn(|j| lerp(a.rgba[j] as f32, b.rgba[j] as f32) as i32) } else { b.rgba.map(|x| x as i32) };
            let z = lerp(a.z as f32, b.z as f32) as u32;
            let (u, t) = tex_coord(&c, lerp(a.s, b.s), lerp(a.t, b.t), lerp(a.q, b.q), lerp(a.u as f32, b.u as f32) as u32, lerp(a.v as f32, b.v as f32) as u32);
            self.plot(&c, lerp(x0, x1).floor() as i32, lerp(y0, y1).floor() as i32, z, rgba, u, t, lerp(a.f as f32, b.f as f32) as i32);
        }
    }

    /// Sprites: an axis-aligned rectangle; colour and Z from the second vertex,
    /// texture coordinates interpolated corner to corner
    fn draw_sprite(&mut self, c: &Ctx, a: &Vertex, b: &Vertex) {
        let c = *c;
        let (mut p, mut q) = (*a, *b);
        if p.x > q.x { std::mem::swap(&mut p.x, &mut q.x); std::mem::swap(&mut p.u, &mut q.u); std::mem::swap(&mut p.s, &mut q.s); }
        if p.y > q.y { std::mem::swap(&mut p.y, &mut q.y); std::mem::swap(&mut p.v, &mut q.v); std::mem::swap(&mut p.t, &mut q.t); }
        let x0 = ((p.x + 15) >> 4).max(c.sc[0]).max(0);
        let x1 = ((q.x + 15) >> 4).min(c.sc[1] + 1);
        let y0 = ((p.y + 15) >> 4).max(c.sc[2]).max(0).max(self.y0);
        let y1 = ((q.y + 15) >> 4).min(c.sc[3] + 1).min(self.y1);
        if x0 >= x1 || y0 >= y1 { return; }
        let (w, h) = (((q.x - p.x) as f32).max(1.0), ((q.y - p.y) as f32).max(1.0));
        let rgba = b.rgba.map(|x| x as i32);
        // the column parts of the texture coordinates, once per sprite
        let cols: Vec<(f32, f32)> = if c.tme {
            (x0..x1).map(|x| {
                let kx = ((x * 16 - p.x) as f32 / w).clamp(0.0, 1.0);
                (p.s + (q.s - p.s) * kx, p.u as f32 + (q.u as f32 - p.u as f32) * kx)
            }).collect()
        } else { Vec::new() };
        for y in y0..y1 {
            let ky = ((y * 16 - p.y) as f32 / h).clamp(0.0, 1.0);
            let tt = p.t + (q.t - p.t) * ky;
            let vv = p.v as f32 + (q.v as f32 - p.v as f32) * ky;
            for x in x0..x1 {
                let (u, t) = if c.tme {
                    let (s, uu) = cols[(x - x0) as usize];
                    tex_coord(&c, s, tt, b.q, uu as u32, vv as u32)
                } else { (0, 0) };
                self.plot(&c, x, y, b.z, rgba, u, t, b.f as i32);
            }
        }
    }

    /// Triangles: edge functions in exact integers on the 12.4 grid (top-left rule), one
    /// span per row; attributes from barycentric weights (w / area, as before)
    fn draw_triangle(&mut self, c: &Ctx, a: &Vertex, b: &Vertex, v2: &Vertex) {
        let c = *c;
        let vs = [a, b, v2];
        let px: [i64; 3] = vs.map(|v| v.x as i64);
        let py: [i64; 3] = vs.map(|v| v.y as i64);
        let area = (px[1] - px[0]) * (py[2] - py[0]) - (px[2] - px[0]) * (py[1] - py[0]);
        if area == 0 { return; }
        let ceil16 = |v: i64| (v + 15).div_euclid(16);
        let floor16 = |v: i64| v.div_euclid(16);
        let minx = (ceil16(*px.iter().min().unwrap()) as i32).max(c.sc[0]).max(0);
        let maxx = (floor16(*px.iter().max().unwrap()) as i32).min(c.sc[1]);
        let miny = (ceil16(*py.iter().min().unwrap()) as i32).max(c.sc[2]).max(0).max(self.y0);
        let maxy = (floor16(*py.iter().max().unwrap()) as i32).min(c.sc[3]).min(self.y1 - 1);
        if minx > maxx || miny > maxy { return; }
        let sign: i64 = if area > 0 { 1 } else { -1 };
        let inv_area = 1.0 / (area * sign) as f64;
        // edge k (opposite vertex k) from vertex i to j: E = (xj-xi)(Y-yi) - (yj-yi)(X-xi)
        let ed = [(1usize, 2usize), (2, 0), (0, 1)];
        let owns = |i: usize, j: usize| {
            let (dx, dy) = ((px[j] - px[i]) * sign, (py[j] - py[i]) * sign);
            dy < 0 || (dy == 0 && dx > 0)
        };
        let tl = [owns(1, 2), owns(2, 0), owns(0, 1)];
        // per edge: value at (minx, y) and step per pixel in x (16 sub-units)
        let stepx: [i64; 3] = ed.map(|(i, j)| -(py[j] - py[i]) * 16 * sign);
        let at = |k: usize, x: i32, y: i32| -> i64 {
            let (i, j) = ed[k];
            ((px[j] - px[i]) * (y as i64 * 16 - py[i]) - (py[j] - py[i]) * (x as i64 * 16 - px[i])) * sign
        };
        let flat = v2.rgba.map(|x| x as i32);
        let fr: [[f64; 3]; 4] = std::array::from_fn(|k| vs.map(|v| v.rgba[k] as f64));
        let fz = vs.map(|v| v.z as f64);
        let (fs, ft, fq) = (vs.map(|v| v.s as f64), vs.map(|v| v.t as f64), vs.map(|v| v.q as f64));
        let (fu, fv) = (vs.map(|v| v.u as f64), vs.map(|v| v.v as f64));
        let ff = vs.map(|v| v.f as f64);
        let inside = |w: i64, k: usize| w > 0 || (w == 0 && tl[k]);
        for y in miny..=maxy {
            let mut w = [at(0, minx, y), at(1, minx, y), at(2, minx, y)];
            let mut x = minx;
            // skip to the first pixel inside
            while x <= maxx && !(inside(w[0], 0) && inside(w[1], 1) && inside(w[2], 2)) {
                // jump straight past an edge we're outside of and moving towards
                x += 1;
                for k in 0..3 { w[k] += stepx[k]; }
            }
            while x <= maxx {
                if !(inside(w[0], 0) && inside(w[1], 1) && inside(w[2], 2)) { break; }
                let l = w.map(|e| e as f64 * inv_area);
                let mix = |f: &[f64; 3]| l[0] * f[0] + l[1] * f[1] + l[2] * f[2];
                let rgba = if c.iip { std::array::from_fn(|k| (mix(&fr[k]) + 0.5) as i32) } else { flat };
                let z = mix(&fz).max(0.0) as u32;
                let (u, t) = if c.tme {
                    tex_coord(&c, mix(&fs) as f32, mix(&ft) as f32, mix(&fq) as f32, mix(&fu) as u32, mix(&fv) as u32)
                } else { (0, 0) };
                let f = if c.fge { mix(&ff) as i32 } else { 0 };
                self.plot(&c, x, y, z, rgba, u, t, f);
                x += 1;
                for k in 0..3 { w[k] += stepx[k]; }
            }
        }
    }

    // ------------------------------------------------------------------ textures

    /// A texel as RGBA (0-255)
    #[inline]
    fn texel(&self, c: &Ctx, u: i32, v: i32) -> [i32; 4] {
        #[inline]
        fn wrap(p: i32, size: u32, mode: u32, min: u32, max: u32) -> u32 {
            match mode {
                0 => (p as u32) & (size - 1),
                1 => p.clamp(0, size as i32 - 1) as u32,
                2 => p.clamp(min as i32, max as i32) as u32,
                _ => ((p as u32) & min) | max,
            }
        }
        let u = wrap(u, c.tw, c.wms, c.minu, c.maxu);
        let v = wrap(v, c.th, c.wmt, c.minv, c.maxv);
        let raw = mem::read(&self.vram, c.tpsm, u, v, c.tbp, c.tbw);
        let conv16 = |p: u32| {
            let e = |x: u32| ((x << 3) | (x >> 2)) as i32;
            let a = if p & 0x8000 != 0 { c.ta1 } else if c.aem && p & 0x7FFF == 0 { 0 } else { c.ta0 };
            [e(p & 31), e(p >> 5 & 31), e(p >> 10 & 31), a]
        };
        match c.tpsm {
            PSMCT32 | PSMZ32 => [raw as i32 & 255, (raw >> 8) as i32 & 255, (raw >> 16) as i32 & 255, (raw >> 24) as i32 & 255],
            PSMCT24 | PSMZ24 => {
                let a = if c.aem && raw & 0xFF_FFFF == 0 { 0 } else { c.ta0 };
                [raw as i32 & 255, (raw >> 8) as i32 & 255, (raw >> 16) as i32 & 255, a]
            }
            PSMT8 | PSMT4 | PSMT8H | PSMT4HL | PSMT4HH => {
                let e = self.clut[((c.clut_base + raw) % 512) as usize];
                if c.cpsm == 0 { [e as i32 & 255, (e >> 8) as i32 & 255, (e >> 16) as i32 & 255, (e >> 24) as i32 & 255] } else { conv16(e) }
            }
            _ => conv16(raw),
        }
    }

    // ------------------------------------------------------------------ the pixel pipeline

    #[allow(clippy::too_many_arguments)]
    #[inline]
    fn plot(&mut self, c: &Ctx, x: i32, y: i32, z: u32, rgba: [i32; 4], u: i32, v: i32, fog: i32) {
        if x < c.sc[0] || x > c.sc[1] || y < c.sc[2] || y > c.sc[3] || x < 0 || y < self.y0 || y >= self.y1 { return; }
        let (xu, yu) = (x as u32, y as u32);
        // texture function
        let mut col = rgba;
        if c.tme {
            let tx = self.texel(c, u, v);
            let tcc = c.tcc;
            let m = |t: i32, f: i32| ((t * f) >> 7).min(255);
            col = match c.tfx {
                0 => [m(tx[0], rgba[0]), m(tx[1], rgba[1]), m(tx[2], rgba[2]), if tcc { m(tx[3], rgba[3]) } else { rgba[3] }],
                1 => [tx[0], tx[1], tx[2], if tcc { tx[3] } else { rgba[3] }],
                2 => {
                    let a = rgba[3];
                    [(m(tx[0], rgba[0]) + a).min(255), (m(tx[1], rgba[1]) + a).min(255), (m(tx[2], rgba[2]) + a).min(255),
                        if tcc { (tx[3] + a).min(255) } else { a }]
                }
                _ => {
                    let a = rgba[3];
                    [(m(tx[0], rgba[0]) + a).min(255), (m(tx[1], rgba[1]) + a).min(255), (m(tx[2], rgba[2]) + a).min(255),
                        if tcc { tx[3] } else { a }]
                }
            };
        }
        if c.fge {
            for k in 0..3 { col[k] = (fog * col[k] + (255 - fog) * c.fogcol[k]) >> 8; }
        }
        // alpha test
        let mut write_fb = true;
        let mut write_z = !c.zmsk;
        let mut keep_alpha = false;
        if c.ate {
            let (a, aref) = (col[3], c.aref);
            let pass = match c.atst { 0 => false, 1 => true, 2 => a < aref, 3 => a <= aref, 4 => a == aref, 5 => a >= aref, 6 => a > aref, _ => a != aref };
            if !pass {
                match c.afail {
                    0 => return,
                    1 => write_z = false,
                    2 => write_fb = false,
                    _ => { write_z = false; keep_alpha = true; }
                }
            }
        }
        let dst = mem::read(&self.vram, c.fpsm, xu, yu, c.fbp, c.fbw);
        // destination alpha test
        if c.date {
            let bit = if c.f16 { dst >> 15 & 1 } else { dst >> 31 & 1 };
            if bit != c.datm { return; }
        }
        // Z test
        let z = z.min(c.zmax);
        if c.zte {
            if c.ztst != 1 {
                if c.ztst == 0 { return; }
                let old = mem::read(&self.vram, c.zpsm, xu, yu, c.zbp, c.fbw);
                let pass = if c.ztst == 2 { z >= old } else { z > old };
                if !pass { return; }
            }
        }
        // blending
        if c.abe && !(c.pabe && col[3] < 0x80) {
            let (drgb, da) = to_rgba(dst, c.fpsm);
            let src = col;
            let pick = |sel: u32, k: usize| match sel { 0 => src[k], 1 => drgb[k] as i32, _ => 0 };
            let cc = match c.csel { 0 => col[3], 1 => da as i32, _ => c.fix };
            for k in 0..3 {
                let a = pick(c.asel[0], k);
                let b = pick(c.asel[1], k);
                let d = pick(c.asel[2], k);
                col[k] = (((a - b) * cc) >> 7) + d;
            }
        }
        for k in 0..3 { col[k] = if c.clamp_col { col[k].clamp(0, 255) } else { col[k] & 255 }; }
        let mut a = col[3].clamp(0, 255) as u32;
        if c.fba { a |= 0x80; }
        if write_fb {
            let mut new = col[0] as u32 | (col[1] as u32) << 8 | (col[2] as u32) << 16 | a << 24;
            if c.f16 { new = pack16(new); }
            let mask = if keep_alpha { c.fmask_keep_a } else { c.fmask };
            let out = (new & !mask) | (dst & mask);
            mem::write(&mut self.vram, c.fpsm, xu, yu, c.fbp, c.fbw, out);
            self.pixels += 1;
        }
        if write_z && c.zte {
            mem::write(&mut self.vram, c.zpsm, xu, yu, c.zbp, c.fbw, z);
        }
    }
}

/// floor() without the library call (the default x86-64 target has no SSE4.1 round)
#[inline]
fn ffloor(v: f32) -> i32 { let t = v as i32; if (t as f32) > v { t - 1 } else { t } }

/// Texel coordinates: STQ (perspective, normalised to the texture size) or UV (FST)
#[inline]
fn tex_coord(c: &Ctx, s: f32, t: f32, q: f32, u: u32, v: u32) -> (i32, i32) {
    if !c.tme { return (0, 0); }
    if c.fst {
        ((u >> 4) as i32, (v >> 4) as i32)
    } else {
        let q = if q == 0.0 { 1.0 } else { q };
        (ffloor(s / q * c.tw as f32), ffloor(t / q * c.th as f32))
    }
}
