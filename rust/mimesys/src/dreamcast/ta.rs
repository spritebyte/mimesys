//! The tile accelerator: the parameter stream the CPU sends to 10000000 (32-byte blocks)
//! turned into polygon strips. The real chip bins them into per-tile object lists in video
//! RAM; we keep them as a "scene" in host memory, keyed by TA_ISP_BASE at list init, and the
//! renderer draws a scene when STARTRENDER names it with PARAM_BASE.
//! Parameter formats from the PowerVR2 / Holly documentation; sizes cross-checked with MAME
//! mame/sega/powervr2.cpp (BSD-3-Clause) pvr_wordsvertex / pvr_wordspolygon.

use serde::{Deserialize, Serialize};

use super::pvr::IST_LIST_END;

/// A vertex: screen x / y, z = 1/w, texture u / v, base and offset colours as A R G B 0..1.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Vert {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub u: f32,
    pub v: f32,
    pub base: [f32; 4],
    pub off: [f32; 4],
    /// the second parameter set of a two-volume polygon (used inside modifier volumes)
    pub u1: f32,
    pub v1: f32,
    pub base1: [f32; 4],
    pub off1: [f32; 4],
}

/// One strip (sprites are 4-vertex strips) with the words of its global parameter.
/// `isp` carries the object-control bits from the PCW in 25-22 (texture, offset, Gouraud,
/// 16-bit UV), the way the TA writes them into the ISP/TSP instruction word.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Poly {
    pub list: u8,
    pub isp: u32,
    pub tsp: u32,
    pub tcw: u32,
    pub first: u32,
    pub count: u32,
    /// user tile clip in pixels (x0, y0, x1, y1) and mode (2 = draw inside, 3 = outside)
    pub clip: [u16; 4],
    pub clip_mode: u8,
    /// the PCW (object control: bit 7 = affected by modifier volumes, bit 6 = two volumes)
    pub pcw: u32,
    /// TSP / TCW of the second volume
    pub tsp1: u32,
    pub tcw1: u32,
}

/// A modifier-volume triangle: its list (1 opaque, 3 translucent), the volume instruction
/// of its header (0 normal, 1 inside last polygon, 2 outside last polygon), the header it
/// came after (a volume closes after the triangles of a header with instruction 1 or 2).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModTri {
    pub list: u8,
    pub mode: u8,
    pub group: u32,
    pub v: [[f32; 3]; 3],
}

/// Lists the TA built for one TA_ISP_BASE.
#[derive(Clone, Default, Debug)]
pub struct Scene {
    pub isp_base: u32,
    pub polys: Vec<Poly>,
    pub verts: Vec<Vert>,
    pub mods: Vec<ModTri>,
}

pub const LIST_OPAQUE: u8 = 0;
pub const LIST_OPAQUE_MOD: u8 = 1;
pub const LIST_TRANS: u8 = 2;
pub const LIST_TRANS_MOD: u8 = 3;
pub const LIST_PUNCH: u8 = 4;

/// The global parameter in effect.
#[derive(Clone, Copy, Default, Serialize, Deserialize)]
struct Obj {
    pcw: u32,
    isp: u32,
    tsp: u32,
    tcw: u32,
    sprite: bool,
    modvol: bool,
    face: [f32; 4],
    face_off: [f32; 4],
    face1: [f32; 4],
    tsp1: u32,
    tcw1: u32,
    clip_mode: u8,
    /// modifier volume instruction (ISP bits 29-27) and header count
    vol_mode: u8,
    group: u32,
}

/// The tile accelerator's parser state.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Ta {
    /// list being sent (None between lists)
    pub list: Option<u8>,
    /// vertices of the current object are 64 bytes
    vert64: bool,
    /// the first half of a 64-byte parameter is waiting in `buf`
    second_half: bool,
    buf: [u32; 16],
    obj: Obj,
    /// face colours of the last intensity-mode-1 header (intensity mode 2 reuses them)
    last_face: [f32; 4],
    last_face_off: [f32; 4],
    clip: [u16; 4],
    /// index in `cur.verts` where the open strip starts
    #[serde(skip)]
    strip: Option<usize>,
    /// parameters / vertices seen (debug)
    pub params: u64,
    pub vertices: u64,
    /// the scene being built and the finished ones (not saved: a state loads between
    /// frames, the next frame sends its lists again)
    #[serde(skip)]
    pub cur: Scene,
    #[serde(skip)]
    pub scenes: Vec<Scene>,
}

fn packed(c: u32) -> [f32; 4] {
    [(c >> 24) as f32 / 255.0, (c >> 16 & 0xFF) as f32 / 255.0, (c >> 8 & 0xFF) as f32 / 255.0, (c & 0xFF) as f32 / 255.0]
}

fn floats(w: &[u32]) -> [f32; 4] {
    let mut c = [0.0; 4];
    for (k, x) in c.iter_mut().enumerate() { *x = f32::from_bits(w[k]).clamp(0.0, 1.0); }
    c
}

fn scale(face: &[f32; 4], i: f32) -> [f32; 4] {
    let i = i.clamp(0.0, 1.0);
    [face[0], face[1] * i, face[2] * i, face[3] * i]
}

/// A 16-bit float (the top half of an f32).
fn f16(h: u32) -> f32 { f32::from_bits(h << 16) }

impl Ta {
    /// TA_LIST_INIT: a new scene for `isp_base` (an older one with that base is replaced).
    pub fn list_init(&mut self, isp_base: u32) {
        self.finish();
        self.list = None;
        self.second_half = false;
        self.strip = None;
        self.cur = Scene { isp_base, ..Scene::default() };
    }

    /// TA_LIST_CONT: more lists for the current scene.
    pub fn list_cont(&mut self) { self.list = None; self.second_half = false; self.strip = None; }

    /// Keep the scene being built (a new list init starts another).
    fn finish(&mut self) {
        if self.cur.polys.is_empty() && self.cur.mods.is_empty() && self.cur.isp_base == 0 { return; }
        let s = std::mem::take(&mut self.cur);
        self.scenes.retain(|o| o.isp_base != s.isp_base);
        if self.scenes.len() >= 4 { self.scenes.remove(0); }
        self.scenes.push(s);
    }

    /// The scene whose ISP base is `base` (the current one included).
    pub fn scene(&mut self, base: u32) -> Option<&Scene> {
        if self.cur.isp_base == base { return Some(&self.cur); }
        self.scenes.iter().find(|s| s.isp_base == base)
    }

    /// The list a parameter belongs to: the open one, or the one its PCW names.
    fn list_of(&self, pcw: u32) -> u8 { self.list.unwrap_or((pcw >> 24 & 7) as u8) }

    /// One 32-byte block of the parameter stream; returns the interrupt bits it ends.
    pub fn block(&mut self, w: &[u32; 8]) -> u32 {
        if self.second_half {
            self.second_half = false;
            self.buf[8..].copy_from_slice(w);
            return self.param();
        }
        self.buf[..8].copy_from_slice(w);
        let pcw = w[0];
        let long = match pcw >> 29 {
            4 => {
                let list = self.list_of(pcw);
                let (tex, offset, col, volume) = (pcw & 8 != 0, pcw & 4 != 0, pcw >> 4 & 3, pcw & 0x40 != 0);
                // 64-byte headers: intensity colour with a (textured) offset colour, or
                // intensity with two volumes
                list != LIST_OPAQUE_MOD && list != LIST_TRANS_MOD && col == 2 && (volume || tex && offset)
            }
            7 => self.vert64,
            _ => false,
        };
        if long { self.second_half = true; return 0; }
        self.param()
    }

    /// A complete parameter in `buf`.
    fn param(&mut self) -> u32 {
        let w = self.buf;
        let pcw = w[0];
        match pcw >> 29 {
            0 => {
                // end of list
                self.strip = None;
                let bits = self.list.map_or(0, |l| IST_LIST_END[l as usize]);
                self.list = None;
                bits
            }
            1 => {
                // user tile clip
                self.clip = [((w[4] & 0x3F) * 32) as u16, ((w[5] & 0xF) * 32) as u16,
                             (((w[6] & 0x3F) + 1) * 32) as u16, (((w[7] & 0xF) + 1) * 32) as u16];
                0
            }
            4 | 5 => { self.header(&w); 0 }
            7 => { self.vertex(&w); 0 }
            _ => 0,
        }
    }

    fn header(&mut self, w: &[u32; 16]) {
        let pcw = w[0];
        self.params += 1;
        self.strip = None;
        if self.list.is_none() {
            let l = (pcw >> 24 & 7) as u8;
            if l <= 4 { self.list = Some(l); }
        }
        let list = self.list.unwrap_or(0);
        let sprite = pcw >> 29 == 5;
        let modvol = !sprite && (list == LIST_OPAQUE_MOD || list == LIST_TRANS_MOD);
        let tex = pcw & 8 != 0;
        let col = pcw >> 4 & 3;
        let volume = pcw & 0x40 != 0;
        // object-control bits into the ISP word (offset only counts with a texture)
        let ctrl = pcw & 0xB | if tex { pcw & 4 } else { 0 };
        let mut obj = Obj {
            pcw,
            isp: (w[1] & !0x03C0_0000) | ctrl << 22,
            tsp: w[2],
            tcw: w[3],
            sprite,
            modvol,
            clip_mode: if pcw & 0x0080_0000 != 0 { (pcw >> 16 & 3) as u8 } else { self.obj.clip_mode },
            tsp1: if pcw & 0x40 != 0 { w[4] } else { 0 },
            tcw1: if pcw & 0x40 != 0 { w[5] } else { 0 },
            vol_mode: (w[1] >> 27 & 3) as u8,
            group: self.obj.group + 1,
            ..Obj::default()
        };
        if sprite {
            obj.face = packed(w[4]);
            obj.face_off = if ctrl & 4 != 0 { packed(w[5]) } else { [0.0; 4] };
            self.vert64 = true;
        } else if modvol {
            self.vert64 = true;
        } else {
            match col {
                2 => {
                    // with two volumes the second face colour follows (ignored for now)
                    if volume { obj.face1 = floats(&w[12..16]); }
                    let (f, o) = if volume { (floats(&w[8..12]), [0.0; 4]) }
                        else if tex && pcw & 4 != 0 { (floats(&w[8..12]), floats(&w[12..16])) }
                        else { (floats(&w[4..8]), [0.0; 4]) };
                    obj.face = f;
                    obj.face_off = o;
                    self.last_face = f;
                    self.last_face_off = o;
                }
                3 => { obj.face = self.last_face; obj.face_off = self.last_face_off; }
                _ => {}
            }
            self.vert64 = if volume { tex } else { col == 1 && tex };
        }
        self.obj = obj;
    }

    fn vertex(&mut self, w: &[u32; 16]) {
        self.vertices += 1;
        if self.list.is_none() { return; }
        let o = self.obj;
        if o.modvol {
            // one triangle: A B C x y z
            let fl = |i: usize| f32::from_bits(w[i]);
            let v = [[fl(1), fl(2), fl(3)], [fl(4), fl(5), fl(6)], [fl(7), fl(8), fl(9)]];
            let list = self.list.unwrap_or(LIST_OPAQUE_MOD);
            self.cur.mods.push(ModTri { list, mode: o.vol_mode, group: o.group, v });
            return;
        }
        let pcw = o.pcw;
        let list = self.list.unwrap_or(0);
        let poly = |first: usize, count: usize, clip: [u16; 4]| Poly {
            list, isp: o.isp, tsp: o.tsp, tcw: o.tcw, first: first as u32, count: count as u32, clip, clip_mode: o.clip_mode,
            pcw: o.pcw, tsp1: o.tsp1, tcw1: o.tcw1,
        };
        if o.sprite {
            // A B C corners with z, D only x / y; D = A + C - B for the rest
            let tex = pcw & 8 != 0;
            let fl = |i: usize| f32::from_bits(w[i]);
            let mut q = [Vert { base: o.face, off: o.face_off, ..Vert::default() }; 4];
            let (a, b, c) = ((fl(1), fl(2), fl(3)), (fl(4), fl(5), fl(6)), (fl(7), fl(8), fl(9)));
            let uv = |i: usize| if tex { (f16(w[i] >> 16), f16(w[i] & 0xFFFF)) } else { (0.0, 0.0) };
            let (ua, ub, uc) = (uv(13), uv(14), uv(15));
            q[0].x = a.0; q[0].y = a.1; q[0].z = a.2; q[0].u = ua.0; q[0].v = ua.1;
            q[1].x = b.0; q[1].y = b.1; q[1].z = b.2; q[1].u = ub.0; q[1].v = ub.1;
            // strip order A B D C
            q[3].x = c.0; q[3].y = c.1; q[3].z = c.2; q[3].u = uc.0; q[3].v = uc.1;
            q[2].x = fl(10); q[2].y = fl(11);
            q[2].z = a.2 + c.2 - b.2;
            q[2].u = ua.0 + uc.0 - ub.0;
            q[2].v = ua.1 + uc.1 - ub.1;
            let first = self.cur.verts.len();
            self.cur.verts.extend_from_slice(&q);
            self.cur.polys.push(poly(first, 4, self.clip));
            return;
        }
        let tex = pcw & 8 != 0;
        let uv16 = pcw & 1 != 0;
        let col = pcw >> 4 & 3;
        let volume = pcw & 0x40 != 0;
        let fl = |i: usize| f32::from_bits(w[i]);
        let mut v = Vert { x: fl(1), y: fl(2), z: fl(3), ..Vert::default() };
        if tex {
            if uv16 { v.u = f16(w[4] >> 16); v.v = f16(w[4] & 0xFFFF); } else { v.u = fl(4); v.v = fl(5); }
        }
        // the first volume's colours (the second one waits for modifier volumes)
        match (tex, volume, col) {
            (false, false, 0) => v.base = packed(w[6]),
            (false, false, 1) => v.base = floats(&w[4..8]),
            (false, false, _) => v.base = scale(&o.face, fl(6)),
            (false, true, 0) | (false, true, 1) => v.base = packed(w[4]),
            (false, true, _) => v.base = scale(&o.face, fl(4)),
            (true, false, 1) => { v.base = floats(&w[8..12]); v.off = floats(&w[12..16]); }
            (true, _, 0) | (true, _, 1) => { v.base = packed(w[6]); v.off = packed(w[7]); }
            (true, _, _) => { v.base = scale(&o.face, fl(6)); v.off = scale(&o.face_off, fl(7)); }
        }
        if volume {
            // the second volume's values
            match (tex, col) {
                (false, 0) | (false, 1) => v.base1 = packed(w[5]),
                (false, _) => v.base1 = scale(&o.face1, fl(5)),
                (true, c) => {
                    if uv16 { v.u1 = f16(w[8] >> 16); v.v1 = f16(w[8] & 0xFFFF); } else { v.u1 = fl(8); v.v1 = fl(9); }
                    if c <= 1 { v.base1 = packed(w[10]); v.off1 = packed(w[11]); }
                    else { v.base1 = scale(&o.face1, fl(10)); v.off1 = scale(&o.face1, fl(11)); }
                }
            }
        }
        if o.isp & (1 << 24) == 0 { v.off = [0.0; 4]; }
        let start = *self.strip.get_or_insert(self.cur.verts.len());
        self.cur.verts.push(v);
        if w[0] & 0x1000_0000 != 0 {
            let n = self.cur.verts.len() - start;
            if n >= 3 { self.cur.polys.push(poly(start, n, self.clip)); }
            self.strip = None;
        }
    }
}
