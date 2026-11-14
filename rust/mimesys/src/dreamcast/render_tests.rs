//! Stage 3 tests: TA parameter parsing, texture fetch, the rasterizer and framebuffer
//! write-out, driven through the PVR register file and video RAM.

use super::pvr::{self, map32, Pvr};
use super::render::{self, *};
use super::ta::*;
use super::texture::*;

const FB: u32 = 0x20_0000;
const REGION: u32 = 0x10_0000;
const BG: u32 = 0x1000;

fn put32(vram: &mut [u8], a: u32, v: u32) {
    let i = map32(a);
    vram[i..i + 4].copy_from_slice(&v.to_le_bytes());
}

fn f(x: f32) -> u32 { x.to_bits() }

/// 565 pixel of the written framebuffer (linestride 640 pixels).
fn px565(vram: &[u8], x: u32, y: u32) -> u16 {
    let a = FB + y * 1280 + x * 2;
    u16::from_le_bytes([vram[map32(a)], vram[map32(a + 1)]])
}

/// A PVR set up for a 640x480 565 render with every tile listed, and a background plane of
/// `bg` (packed ARGB).
struct Rig { pvr: Pvr, vram: Vec<u8> }

impl Rig {
    fn new(bg: u32) -> Self {
        let mut r = Rig { pvr: Pvr::new(), vram: vec![0; 8 << 20] };
        r.set(render::PARAM_BASE, 0);
        r.set(render::REGION_BASE, REGION);
        r.set(render::FB_W_SOF1, FB);
        r.set(render::FB_W_CTRL, 1);
        r.set(render::FB_W_LINESTRIDE, 1280 / 8);
        r.set(render::FB_X_CLIP, 639 << 16);
        r.set(render::FB_Y_CLIP, 479 << 16);
        r.set(render::ISP_BACKGND_D, f(0.0001));
        r.set(render::ISP_BACKGND_T, 1 << 24 | (BG << 1));
        r.set(pvr::TA_ISP_BASE, 0);
        r.tiles(&(0..15).flat_map(|y| (0..20).map(move |x| (x, y))).collect::<Vec<_>>());
        r.background(bg);
        r.pvr.write(pvr::TA_LIST_INIT, 0x8000_0000);
        r
    }
    fn set(&mut self, off: u32, v: u32) { self.pvr.write(off, v); }
    fn tiles(&mut self, t: &[(u32, u32)]) {
        for (k, &(x, y)) in t.iter().enumerate() {
            let last = if k + 1 == t.len() { 0x8000_0000 } else { 0 };
            put32(&mut self.vram, REGION + k as u32 * 20, last | y << 8 | x << 2);
        }
    }
    fn background(&mut self, c: u32) {
        let words = [0u32, 0, 0, f(0.0), f(0.0), f(0.0001), c, f(640.0), f(0.0), f(0.0001), c, f(0.0), f(480.0), f(0.0001), c];
        for (k, w) in words.iter().enumerate() { put32(&mut self.vram, BG + 4 * k as u32, *w); }
    }
    fn send(&mut self, words: &[u32]) {
        assert_eq!(words.len() % 8, 0);
        for b in words.chunks(8) { self.pvr.ta.block(b.try_into().unwrap()); }
    }
    fn end_list(&mut self) { self.send(&[0; 8]); }
    fn render(&mut self) -> Stats {
        let s = self.pvr.ta.scene(0).cloned();
        render::render(&self.pvr.regs, s.as_ref(), &mut self.vram)
    }
}

/// Polygon header PCW: list, object control bits.
fn hdr(list: u32, obj: u32) -> u32 { 4 << 29 | list << 24 | 0x0080_0000 | obj }
/// Vertex PCW, end of strip when `eos`.
fn vtx(eos: bool) -> u32 { 7 << 29 | (eos as u32) << 28 }
/// Packed-colour untextured vertex (type 0).
fn v0(x: f32, y: f32, z: f32, c: u32, eos: bool) -> [u32; 8] { [vtx(eos), f(x), f(y), f(z), 0, 0, c, 0] }

const ISP_GEQ: u32 = 6 << 29;
const GOURAUD: u32 = 2;

/// A rectangle as a 4-vertex strip.
fn rect(r: &mut Rig, x0: f32, y0: f32, x1: f32, y1: f32, z: f32, c: u32) {
    for (k, (x, y)) in [(x0, y0), (x1, y0), (x0, y1), (x1, y1)].into_iter().enumerate() {
        r.send(&v0(x, y, z, c, k == 3));
    }
}

#[test]
fn twiddled_index_interleaves_y_low() {
    assert_eq!(twiddle(0, 1, 3), 1);
    assert_eq!(twiddle(1, 0, 3), 2);
    assert_eq!(twiddle(1, 1, 3), 3);
    assert_eq!(twiddle(2, 0, 3), 8);
    assert_eq!(twiddle(0, 2, 3), 4);
    // 16 x 8: x bit 3 sits above the 3 interleaved pairs
    assert_eq!(twiddle(8, 0, 3), 64);
    assert_eq!(twiddle(9, 1, 3), 64 + 3);
}

#[test]
fn colour_formats_expand_with_bit_copies() {
    assert_eq!(cv_565(0xFFFF), 0xFFFF_FFFF);
    assert_eq!(cv_565(0xF800), 0xFFFF_0000);
    assert_eq!(cv_565(0x07E0), 0xFF00_FF00);
    assert_eq!(cv_1555(0x7C00), 0x00FF_0000);
    assert_eq!(cv_1555(0x801F), 0xFF00_00FF);
    assert_eq!(cv_4444(0xF84C), 0xFF88_44CC);
    assert_eq!(cv_pal(0x1234_5678, 3), 0x1234_5678);
}

#[test]
fn ta_builds_strips_with_header_words() {
    let mut r = Rig::new(0xFF00_0000);
    r.send(&[hdr(0, GOURAUD), ISP_GEQ, 0x1234, 0, 0, 0, 0, 0]);
    r.send(&v0(1.0, 2.0, 0.5, 0xFF10_2030, false));
    r.send(&v0(3.0, 2.0, 0.5, 0xFF10_2030, false));
    r.send(&v0(1.0, 5.0, 0.5, 0x8000_00FF, true));
    r.end_list();
    let s = r.pvr.ta.scene(0).unwrap().clone();
    assert_eq!(s.polys.len(), 1);
    let p = s.polys[0];
    assert_eq!((p.list, p.first, p.count, p.tsp), (0, 0, 3, 0x1234));
    assert_eq!(p.isp, ISP_GEQ | 1 << 23, "Gouraud bit from the PCW");
    assert_eq!(s.verts[1].x, 3.0);
    assert!((s.verts[2].base[0] - 128.0 / 255.0).abs() < 1e-6);
    assert_eq!(s.verts[2].base[3], 1.0);
}

#[test]
fn ta_parameter_sizes() {
    let mut r = Rig::new(0xFF00_0000);
    // untextured float colour vertices are 32 bytes
    r.send(&[hdr(0, 1 << 4), ISP_GEQ, 0, 0, 0, 0, 0, 0]);
    for k in 0..3 {
        r.send(&[vtx(k == 2), f(k as f32), f(0.0), f(1.0), f(1.0), f(0.25), f(0.5), f(0.75)]);
    }
    // textured float colour vertices are 64 bytes
    r.send(&[hdr(0, 1 << 4 | 8), ISP_GEQ, 0, 0, 0, 0, 0, 0]);
    for k in 0..3 {
        let mut w = [0u32; 16];
        w[0] = vtx(k == 2);
        w[1] = f(10.0 + k as f32);
        w[4] = f(0.5);
        w[8..12].copy_from_slice(&[f(1.0), f(1.0), f(0.0), f(0.0)]);
        r.send(&w);
    }
    // intensity + textured offset header is 64 bytes
    let mut h = [0u32; 16];
    h[0] = hdr(0, 2 << 4 | 8 | 4);
    h[1] = ISP_GEQ;
    h[8..12].copy_from_slice(&[f(1.0), f(0.0), f(1.0), f(0.0)]);
    h[12..16].copy_from_slice(&[f(0.0), f(0.0), f(0.0), f(1.0)]);
    r.send(&h);
    for k in 0..3 { r.send(&[vtx(k == 2), f(20.0), f(0.0), f(1.0), 0, 0, f(0.5), f(1.0)]); }
    // the offset bit without a texture: still a 32-byte header
    r.send(&[hdr(0, 2 << 4 | 4), ISP_GEQ, 0, 0, f(1.0), f(1.0), f(1.0), f(1.0)]);
    for k in 0..3 { r.send(&[vtx(k == 2), f(30.0), f(0.0), f(1.0), 0, 0, f(0.25), 0]); }
    r.end_list();
    let s = r.pvr.ta.scene(0).unwrap().clone();
    assert_eq!(s.polys.len(), 4);
    assert_eq!(s.verts.len(), 12);
    assert_eq!(s.verts[9].x, 30.0);
    assert_eq!(s.verts[9].base, [1.0, 0.25, 0.25, 0.25]);
    assert_eq!(s.verts[0].base, [1.0, 0.25, 0.5, 0.75]);
    assert_eq!(s.verts[4].x, 11.0);
    assert_eq!(s.verts[4].u, 0.5);
    assert_eq!(s.verts[4].base, [1.0, 1.0, 0.0, 0.0]);
    // intensity 0.5 of face colour green, offset intensity 1 of the offset face colour
    assert_eq!(s.verts[6].base, [1.0, 0.0, 0.5, 0.0]);
    assert_eq!(s.verts[6].off, [0.0, 0.0, 0.0, 1.0]);
}

#[test]
fn list_end_raises_its_interrupt() {
    let mut r = Rig::new(0xFF00_0000);
    r.send(&[hdr(2, 0), 0, 0, 0, 0, 0, 0, 0]);
    let mut bits = 0;
    for b in [[0u32; 8]] { bits = r.pvr.ta.block(&b); }
    assert_eq!(bits, pvr::IST_LIST_END[2]);
}

#[test]
fn flat_triangle_over_the_background() {
    let mut r = Rig::new(0xFF00_00FF);
    r.send(&[hdr(0, 0), ISP_GEQ, 0, 0, 0, 0, 0, 0]);
    r.send(&v0(100.0, 100.0, 1.0, 0xFFFF_0000, false));
    r.send(&v0(200.0, 100.0, 1.0, 0xFFFF_0000, false));
    r.send(&v0(100.0, 200.0, 1.0, 0xFFFF_0000, true));
    r.end_list();
    let st = r.render();
    assert_eq!(st.tiles, 300);
    assert_eq!(px565(&r.vram, 120, 120), 0xF800, "inside");
    assert_eq!(px565(&r.vram, 190, 190), 0x001F, "outside the hypotenuse");
    assert_eq!(px565(&r.vram, 5, 470), 0x001F, "background");
    assert_eq!(px565(&r.vram, 99, 150), 0x001F, "left of the left edge");
    assert_eq!(px565(&r.vram, 100, 150), 0xF800, "first pixel column");
}

#[test]
fn shared_edges_draw_once() {
    // two translucent halves of a square adding 1/4 white each: a pixel on the diagonal
    // must get exactly one of them
    let mut r = Rig::new(0xFF00_0000);
    r.send(&[hdr(2, 0), 0, 1 << 29 | 1 << 26, 0, 0, 0, 0, 0]);
    rect(&mut r, 0.0, 0.0, 64.0, 64.0, 1.0, 0xFF40_4040);
    r.end_list();
    r.render();
    for (x, y) in [(10, 10), (32, 31), (31, 32), (40, 23), (63, 0)] {
        assert_eq!(px565(&r.vram, x, y) >> 11, 0x40 >> 3, "pixel {x},{y}");
    }
}

#[test]
fn depth_test_keeps_the_nearer_surface() {
    let mut r = Rig::new(0xFF00_0000);
    r.send(&[hdr(0, 0), ISP_GEQ, 0, 0, 0, 0, 0, 0]);
    rect(&mut r, 0.0, 0.0, 100.0, 100.0, 2.0, 0xFFFF_0000); // near (1/w = 2)
    rect(&mut r, 50.0, 50.0, 150.0, 150.0, 1.0, 0xFF00_FF00); // far, drawn later
    r.end_list();
    r.render();
    assert_eq!(px565(&r.vram, 75, 75), 0xF800);
    assert_eq!(px565(&r.vram, 125, 125), 0x07E0);
    // greater-or-equal: a coplanar polygon drawn later wins
    let mut r = Rig::new(0xFF00_0000);
    r.send(&[hdr(0, 0), ISP_GEQ, 0, 0, 0, 0, 0, 0]);
    rect(&mut r, 0.0, 0.0, 100.0, 100.0, 1.0, 0xFFFF_0000);
    rect(&mut r, 0.0, 0.0, 100.0, 100.0, 1.0, 0xFF00_FF00);
    r.end_list();
    r.render();
    assert_eq!(px565(&r.vram, 75, 75), 0x07E0);
    // z write disabled: a later far polygon is not hidden by it
    let mut r = Rig::new(0xFF00_0000);
    r.send(&[hdr(0, 0), ISP_GEQ | 1 << 26, 0, 0, 0, 0, 0, 0]);
    rect(&mut r, 0.0, 0.0, 100.0, 100.0, 2.0, 0xFFFF_0000);
    r.send(&[hdr(0, 0), ISP_GEQ, 0, 0, 0, 0, 0, 0]);
    rect(&mut r, 50.0, 50.0, 150.0, 150.0, 1.0, 0xFF00_FF00);
    r.end_list();
    r.render();
    assert_eq!(px565(&r.vram, 75, 75), 0x07E0);
}

#[test]
fn hidden_opaque_pixels_are_not_shaded() {
    let px = |both: bool| {
        let mut r = Rig::new(0xFF00_0000);
        r.send(&[hdr(0, 0), ISP_GEQ, 0, 0, 0, 0, 0, 0]);
        rect(&mut r, 0.0, 0.0, 100.0, 100.0, 2.0, 0xFFFF_0000);
        if both { rect(&mut r, 0.0, 0.0, 100.0, 100.0, 1.0, 0xFF00_FF00); }
        r.end_list();
        let st = r.render();
        assert_eq!(px565(&r.vram, 50, 50), 0xF800);
        st.pixels
    };
    assert_eq!(px(true), px(false), "the far rectangle is shaded nowhere");
}

#[test]
fn translucent_blends_and_sorts_back_to_front() {
    let mut r = Rig::new(0xFF00_0000);
    // src alpha / inverse src alpha, use alpha
    let tsp = 4 << 29 | 5 << 26 | 1 << 20;
    r.send(&[hdr(2, 0), 0, tsp, 0, 0, 0, 0, 0]);
    rect(&mut r, 0.0, 0.0, 64.0, 64.0, 2.0, 0x80FF_FFFF); // nearer, sent first
    rect(&mut r, 0.0, 0.0, 64.0, 64.0, 1.0, 0xFFFF_0000); // farther, opaque red
    r.end_list();
    r.render();
    // sorted: red first, then half white on top → (255+128)/2... r = 255, g = b = 128
    let p = px565(&r.vram, 10, 10);
    assert_eq!(p >> 11, 31);
    assert_eq!(p >> 5 & 0x3F, 128 >> 2);
    // presort keeps the order sent: white then red over it
    let mut r = Rig::new(0xFF00_0000);
    r.set(render::ISP_FEED_CFG, 1);
    r.send(&[hdr(2, 0), 0, tsp, 0, 0, 0, 0, 0]);
    rect(&mut r, 0.0, 0.0, 64.0, 64.0, 2.0, 0x80FF_FFFF);
    rect(&mut r, 0.0, 0.0, 64.0, 64.0, 1.0, 0xFFFF_0000);
    r.end_list();
    r.render();
    // presort uses the polygon's depth mode (0 = never here): nothing drawn
    assert_eq!(px565(&r.vram, 10, 10), 0);
}

#[test]
fn opaque_hides_translucent_behind_it() {
    let mut r = Rig::new(0xFF00_0000);
    r.send(&[hdr(0, 0), ISP_GEQ, 0, 0, 0, 0, 0, 0]);
    rect(&mut r, 0.0, 0.0, 32.0, 32.0, 2.0, 0xFF00_00FF);
    r.end_list();
    r.send(&[hdr(2, 0), 0, 1 << 29, 0, 0, 0, 0, 0]);
    rect(&mut r, 0.0, 0.0, 64.0, 64.0, 1.0, 0xFFFF_0000);
    r.end_list();
    r.render();
    assert_eq!(px565(&r.vram, 10, 10), 0x001F);
    assert_eq!(px565(&r.vram, 40, 40), 0xF800);
}

#[test]
fn punch_through_tests_alpha() {
    let mut r = Rig::new(0xFF00_0000);
    r.set(render::PT_ALPHA_REF, 0x80);
    r.send(&[hdr(4, 0), ISP_GEQ, 1 << 20, 0, 0, 0, 0, 0]);
    rect(&mut r, 0.0, 0.0, 32.0, 32.0, 1.0, 0x7FFF_0000);
    rect(&mut r, 32.0, 0.0, 64.0, 32.0, 1.0, 0x80FF_0000);
    r.end_list();
    r.render();
    assert_eq!(px565(&r.vram, 10, 10), 0, "alpha below the reference: dropped");
    assert_eq!(px565(&r.vram, 40, 10), 0xF800);
}

#[test]
fn gouraud_interpolates_and_flat_takes_the_last_vertex() {
    for (obj, mid) in [(GOURAUD, 0x10u16), (0, 0)] {
        let mut r = Rig::new(0xFF00_0000);
        r.send(&[hdr(0, obj), ISP_GEQ, 0, 0, 0, 0, 0, 0]);
        // left edge red 0, right edge red 255 → red 5-bit ≈ 16 in the middle
        for (k, (x, y, c)) in [(0.0, 0.0, 0xFF00_0000u32), (256.0, 0.0, 0xFFFF_0000), (0.0, 64.0, 0xFF00_0000), (256.0, 64.0, 0xFFFF_0000)].into_iter().enumerate() {
            r.send(&v0(x, y, 1.0, c, k == 3));
        }
        r.end_list();
        r.render();
        let p = px565(&r.vram, 128, 10) >> 11;
        if obj == GOURAUD {
            assert!((p as i32 - mid as i32).abs() <= 1, "red {p}");
        } else {
            assert!(p == 0 || p == 31);
        }
    }
    // flat: each triangle takes its last vertex: red, black, black, red
    let mut r = Rig::new(0xFF00_0000);
    r.send(&[hdr(0, 0), ISP_GEQ, 0, 0, 0, 0, 0, 0]);
    for (k, (x, y, c)) in [(0.0, 0.0, 0xFFFF_0000u32), (256.0, 0.0, 0xFF00_0000), (0.0, 64.0, 0xFF00_0000), (256.0, 64.0, 0xFFFF_0000)].into_iter().enumerate() {
        r.send(&v0(x, y, 1.0, c, k == 3));
    }
    r.end_list();
    r.render();
    assert_eq!(px565(&r.vram, 10, 10) >> 11, 0, "v0 v1 v2 ends on black");
    assert_eq!(px565(&r.vram, 250, 60) >> 11, 31, "v1 v2 v3 ends on red");
}

#[test]
fn colours_are_perspective_correct() {
    // left edge near (1/w = 1) red 0, right edge far (1/w = 0.25) red 255: halfway across
    // the screen the far side weighs 0.5 * 0.25 / (0.5 + 0.125) = 0.2 → red ≈ 51 (5-bit 6)
    let mut r = Rig::new(0xFF00_0000);
    r.send(&[hdr(0, GOURAUD), 7 << 29, 0, 0, 0, 0, 0, 0]);
    for (k, (x, y, z, c)) in [(0.0, 0.0, 1.0, 0xFF00_0000u32), (256.0, 0.0, 0.25, 0xFFFF_0000), (0.0, 64.0, 1.0, 0xFF00_0000), (256.0, 64.0, 0.25, 0xFFFF_0000)].into_iter().enumerate() {
        r.send(&v0(x, y, z, c, k == 3));
    }
    r.end_list();
    r.render();
    let p = px565(&r.vram, 128, 10) >> 11;
    assert!((5..=7).contains(&p), "red {p}");
}

#[test]
fn textured_sprite_samples_a_twiddled_565_texture() {
    let mut r = Rig::new(0xFF00_0000);
    // 8x8 565 twiddled at 0x8000: texel (x, y) = x * 4 in red, y * 8 in green
    let tex = 0x8000u32;
    for y in 0..8u32 {
        for x in 0..8u32 {
            let c = ((x * 4) << 11 | (y * 8) << 5) as u16;
            let a = (tex + twiddle(x, y, 3) * 2) as usize;
            r.vram[a..a + 2].copy_from_slice(&c.to_le_bytes());
        }
    }
    let pcw = 5 << 29 | 0x0080_0000 | 8; // sprite, textured
    let tcw = 1 << 27 | tex >> 3; // 565, twiddled
    // decal shading instruction (0): the texel alone
    r.send(&[pcw, ISP_GEQ, 0, tcw, 0xFFFF_FFFF, 0, 0, 0]);
    let uv = |u: f32, v: f32| (u.to_bits() & 0xFFFF_0000) | (v.to_bits() >> 16);
    let mut w = [0u32; 16];
    w[0] = vtx(true);
    w[1..4].copy_from_slice(&[f(0.0), f(0.0), f(1.0)]);
    w[4..7].copy_from_slice(&[f(64.0), f(0.0), f(2.0)]);
    w[7..10].copy_from_slice(&[f(64.0), f(64.0), f(3.0)]);
    w[10] = f(0.0);
    w[11] = f(64.0);
    w[13] = uv(0.0, 0.0);
    w[14] = uv(1.0, 0.0);
    w[15] = uv(1.0, 1.0);
    r.send(&w);
    r.end_list();
    // D (strip slot 2) completes the parallelogram: z = A + C - B
    let sc = r.pvr.ta.scene(0).unwrap();
    assert_eq!((sc.verts[2].x, sc.verts[2].y, sc.verts[2].z), (0.0, 64.0, 2.0));
    r.render();
    for (x, y) in [(0u32, 0u32), (3, 5), (7, 7), (6, 1)] {
        let p = px565(&r.vram, x * 8 + 4, y * 8 + 4);
        assert_eq!(p, ((x * 4) << 11 | (y * 8) << 5) as u16, "texel {x},{y}");
    }
}

#[test]
fn texture_addressing_modes() {
    let mut vram = vec![0u8; 1 << 20];
    let mut pal = vec![0u32; 1024];
    // non-twiddled 1555 with stride 32 (TEXT_CONTROL 1), 8 wide
    let t = Tex::new(0, 0 << 27 | 1 << 26 | 1 << 25 | 0x100, 1, 0);
    assert_eq!((t.sx, t.stride, t.twiddled), (8, 32, false));
    let a = 0x800 + (32 * 2 + 5) * 2;
    vram[a..a + 2].copy_from_slice(&0xFC00u16.to_le_bytes());
    assert_eq!(t.texel(&vram, &pal, 5, 2), 0xFFFF_0000);
    // repeat / flip / clamp
    let t = Tex::new(1 << 18 | 1 << 15, 1 << 27 | 1 << 26 | 0x100, 0, 0);
    assert_eq!((t.wu, t.wv), (Wrap::Flip, Wrap::Clamp));
    let a = 0x800 + 6 * 2;
    vram[a..a + 2].copy_from_slice(&0xFFFFu16.to_le_bytes());
    // x = 9 flips to 6; y = -3 clamps to 0
    assert_eq!(t.texel(&vram, &pal, 9, -3), 0xFFFF_FFFF);
    // 4bpp palette, selector 3 → entries 48.., ARGB8888 palette
    pal[48 + 0xA] = 0x1122_3344;
    let t = Tex::new(0, 5 << 27 | 3 << 21 | 0x2000 >> 3, 0, 3);
    let off = twiddle(3, 2, 3);
    vram[0x2000 + (off >> 1) as usize] = if off & 1 != 0 { 0xA0 } else { 0x0A };
    assert_eq!(t.texel(&vram, &pal, 3, 2), 0x1122_3344);
    // VQ 565: codebook at 0x4000, index 7's texel (1, 0) is white; texel (5, 2) of the
    // texture uses index block (2, 1)
    let t = Tex::new(0, 1 << 30 | 1 << 27 | 0x4000 >> 3, 0, 0);
    vram[0x4000 + 0x800 + twiddle(2, 1, 2) as usize] = 7;
    let e = 0x4000 + 7 * 8 + twiddle(1, 0, 1) as usize * 2;
    vram[e..e + 2].copy_from_slice(&0xFFFFu16.to_le_bytes());
    assert_eq!(t.texel(&vram, &pal, 5, 2), 0xFFFF_FFFF);
    assert_eq!(t.texel(&vram, &pal, 4, 2), 0xFF00_0000);
    // mipmapped 16 bpp 8x8: the top level sits after the smaller levels (0x30)
    let t = Tex::new(0, 1 << 31 | 1 << 27 | 0x6000 >> 3, 0, 0);
    assert_eq!(t.addr, 0x6000 + 0x30);
}

#[test]
fn shading_instructions() {
    // texture 565 white-ish red texel everywhere; base colour half grey, alpha 0.5
    let run = |inst: u32, tex_c: u16, base: u32, fmt: u32, extra: u32| -> u16 {
        let mut r = Rig::new(0xFF00_0000);
        for k in 0..64u32 {
            let a = (0x8000 + k * 2) as usize;
            r.vram[a..a + 2].copy_from_slice(&tex_c.to_le_bytes());
        }
        let tsp = 1 << 20 | inst << 6 | extra;
        let tcw = fmt << 27 | 0x8000 >> 3;
        r.send(&[hdr(0, 8), ISP_GEQ, tsp, tcw, 0, 0, 0, 0]);
        for (k, (x, y)) in [(0.0f32, 0.0f32), (32.0, 0.0), (0.0, 32.0), (32.0, 32.0)].into_iter().enumerate() {
            r.send(&[vtx(k == 3), f(x), f(y), f(1.0), f(0.0), f(0.0), base, 0]);
        }
        r.end_list();
        r.render();
        px565(&r.vram, 10, 10)
    };
    // decal: texel
    assert_eq!(run(0, 0xF800, 0x8080_8080, 1, 0), 0xF800);
    // modulate: texel x base → red 255 * 128/255
    assert_eq!(run(1, 0xF800, 0x8080_8080, 1, 0) >> 11, 16);
    // decal alpha with a 4444 texel alpha 0: base colour shows
    assert_eq!(run(2, 0x0F00, 0xFF00_FF00, 2, 0), 0x07E0);
    // modulate alpha with a 4444 opaque blue texel and a green base: black
    assert_eq!(run(3, 0xF00F, 0xFF00_FF00, 2, 0), 0);
    // "ignore texture alpha": the transparent texel counts as opaque
    assert_eq!(run(2, 0x0F00, 0xFF00_FF00, 2, 1 << 19), 0xF800);
}

#[test]
fn only_listed_tiles_are_written_and_formats_pack() {
    let mut r = Rig::new(0xFF12_3456);
    r.tiles(&[(1, 0)]);
    r.set(render::FB_W_CTRL, 6); // 8888
    r.set(render::FB_W_LINESTRIDE, 640 * 4 / 8);
    r.render();
    let rd = |vram: &[u8], x: u32, y: u32| { let a = FB + y * 2560 + x * 4; u32::from_le_bytes([vram[map32(a)], vram[map32(a + 1)], vram[map32(a + 2)], vram[map32(a + 3)]]) };
    assert_eq!(rd(&r.vram, 40, 5), 0xFF12_3456);
    assert_eq!(rd(&r.vram, 10, 5), 0, "tile 0 not listed");
    assert_eq!(rd(&r.vram, 40, 40), 0, "row 1 not listed");
    // an entry with bit 28 (no write-out) leaves its tile alone
    let mut r = Rig::new(0xFF12_3456);
    r.tiles(&[(0, 0), (1, 0)]);
    put32(&mut r.vram, REGION, 0x1000_0000);
    r.render();
    assert_eq!(px565(&r.vram, 5, 5), 0);
    assert_ne!(px565(&r.vram, 40, 5), 0);
    // 0555 with K = 1, 888 packed
    let mut r = Rig::new(0xFFFF_0000);
    r.set(render::FB_W_CTRL, 0x80_0000);
    r.render();
    assert_eq!(px565(&r.vram, 3, 3), 0x8000 | 0x7C00);
    let mut r = Rig::new(0xFF01_0203);
    r.set(render::FB_W_CTRL, 4);
    r.set(render::FB_W_LINESTRIDE, 1920 / 8);
    r.render();
    let a = FB + 1920 + 3;
    assert_eq!([r.vram[map32(a)], r.vram[map32(a + 1)], r.vram[map32(a + 2)]], [3, 2, 1]);
    // the clip leaves x > 9 alone
    let mut r = Rig::new(0xFFFF_FFFF);
    r.set(render::FB_X_CLIP, 9 << 16);
    r.render();
    assert_eq!(px565(&r.vram, 9, 0), 0xFFFF);
    assert_eq!(px565(&r.vram, 10, 0), 0);
}

#[test]
fn user_tile_clip_limits_drawing() {
    let mut r = Rig::new(0xFF00_0000);
    // clip to tile column 1 (x 32..63), rows 0..1
    r.send(&[1 << 29, 0, 0, 0, 1, 0, 1, 1]);
    // group control: update, clip inside (2)
    r.send(&[hdr(0, 0) | 2 << 16, ISP_GEQ, 0, 0, 0, 0, 0, 0]);
    rect(&mut r, 0.0, 0.0, 128.0, 128.0, 1.0, 0xFFFF_FFFF);
    r.end_list();
    r.render();
    assert_eq!(px565(&r.vram, 40, 40), 0xFFFF);
    assert_eq!(px565(&r.vram, 20, 40), 0);
    assert_eq!(px565(&r.vram, 40, 70), 0);
}

#[test]
fn background_plane_is_gouraud_shaded() {
    let mut r = Rig::new(0);
    let words = [GOURAUD << 22, 0, 0, f(0.0), f(0.0), f(1.0), 0xFF00_0000, f(640.0), f(0.0), f(1.0), 0xFFFF_0000, f(0.0), f(480.0), f(1.0), 0xFF00_0000];
    for (k, w) in words.iter().enumerate() { put32(&mut r.vram, BG + 4 * k as u32, *w); }
    r.render();
    // red rises left to right across the whole screen, below the triangle too
    let (a, b) = (px565(&r.vram, 20, 400) >> 11, px565(&r.vram, 600, 400) >> 11);
    assert!(a <= 2 && b >= 28, "{a} {b}");
}

fn bus_render(async_: bool) -> super::bus::Dc {
    use crate::common::cpu::sh4::Sh4Bus;
    let r = Rig::new(0xFF00_FF00);
    let mut d = super::bus::Dc::new(vec![0; super::BIOS_SIZE], &[], 0);
    d.render_async = async_;
    d.vram.copy_from_slice(&r.vram);
    for (k, v) in r.pvr.regs.iter().enumerate() {
        if (0x020..0x200).contains(&(k * 4)) && k * 4 != 0x014 && k * 4 != 0x144 { d.write32(0xA05F_8000 + k as u32 * 4, *v); }
    }
    d.write32(0xA05F_8144, 0x8000_0000);
    // the TA FIFO: an opaque header and a triangle
    let mut send = |d: &mut super::bus::Dc, w: &[u32]| for x in w { d.write32(0x1000_0000, *x); };
    send(&mut d, &[hdr(0, 0), ISP_GEQ, 0, 0, 0, 0, 0, 0]);
    send(&mut d, &v0(0.0, 0.0, 1.0, 0xFFFF_0000, false));
    send(&mut d, &v0(64.0, 0.0, 1.0, 0xFFFF_0000, false));
    send(&mut d, &v0(0.0, 64.0, 1.0, 0xFFFF_0000, true));
    send(&mut d, &[0; 8]);
    d.write32(0xA05F_8014, 1);
    d
}

#[test]
fn render_through_the_bus_draws_and_ends() {
    let d = bus_render(false);
    assert_eq!(px565(&d.vram, 5, 5), 0xF800);
    assert_eq!(px565(&d.vram, 600, 5), 0x07E0);
    assert!(d.pvr.last_render.polys == 1 && d.pvr.last_render.pixels > 2000);
}

#[test]
fn render_on_a_thread_lands_at_the_join() {
    let mut d = bus_render(true);
    assert!(d.render_job.0.is_some());
    d.finish_render();
    assert!(d.render_job.0.is_none());
    assert_eq!(px565(&d.vram, 5, 5), 0xF800);
    assert_eq!(px565(&d.vram, 600, 5), 0x07E0);
    assert!(d.pvr.last_render.polys == 1 && d.pvr.last_render.pixels > 2000);
    let s = bus_render(false);
    assert_eq!(&d.vram[..0x10_0000], &s.vram[..0x10_0000]);
}

// ---- stage 5 ----

/// One triangle with the given winding, cull mode in ISP bits 28-27.
fn cull_case(cull: u32, ccw: bool) -> u16 {
    let mut r = Rig::new(0xFF00_0000);
    r.send(&[hdr(0, 0), ISP_GEQ | cull << 27, 0, 0, 0, 0, 0, 0]);
    let pts = if ccw { [(0.0, 0.0), (0.0, 64.0), (64.0, 0.0)] } else { [(0.0, 0.0), (64.0, 0.0), (0.0, 64.0)] };
    for (k, (x, y)) in pts.into_iter().enumerate() { r.send(&v0(x, y, 1.0, 0xFFFF_FFFF, k == 2)); }
    r.end_list();
    r.render();
    px565(&r.vram, 10, 10)
}

#[test]
fn back_face_culling_follows_screen_winding() {
    // mode 2 culls counter-clockwise on screen (y down)
    assert_eq!(cull_case(0, true), 0xFFFF);
    assert_eq!(cull_case(2, true), 0, "mode 2 culls screen-counter-clockwise");
    assert_eq!(cull_case(2, false), 0xFFFF);
    assert_eq!(cull_case(3, false), 0, "mode 3 culls screen-clockwise");
    assert_eq!(cull_case(3, true), 0xFFFF);
    // a strip keeps one facing: every second triangle counts reversed
    for (cull, want) in [(2, 0xFFFF), (3, 0)] {
        let mut r = Rig::new(0xFF00_0000);
        r.send(&[hdr(0, 0), ISP_GEQ | cull << 27, 0, 0, 0, 0, 0, 0]);
        rect(&mut r, 0.0, 0.0, 64.0, 64.0, 1.0, 0xFFFF_FFFF);
        r.end_list();
        r.render();
        assert_eq!(px565(&r.vram, 10, 10), want, "first triangle, mode {cull}");
        assert_eq!(px565(&r.vram, 60, 60), want, "second triangle, mode {cull}");
    }
}

#[test]
fn translucent_sorts_per_pixel() {
    // two crossing translucent quads: left half A in front, right half B in front;
    // per-pixel sorting puts the nearer one on top on each side
    let mut r = Rig::new(0xFF00_0000);
    let tsp = 4 << 29 | 5 << 26 | 1 << 20;
    r.send(&[hdr(2, GOURAUD), 0, tsp, 0, 0, 0, 0, 0]);
    // A: red, z 2 at the left → 0.5 at the right
    for (k, (x, y, z)) in [(0.0f32, 0.0f32, 2.0f32), (64.0, 0.0, 0.5), (0.0, 32.0, 2.0), (64.0, 32.0, 0.5)].into_iter().enumerate() {
        r.send(&v0(x, y, z, 0x80FF_0000, k == 3));
    }
    // B: green, z 0.5 at the left → 2 at the right
    for (k, (x, y, z)) in [(0.0f32, 0.0f32, 0.5f32), (64.0, 0.0, 2.0), (0.0, 32.0, 0.5), (64.0, 32.0, 2.0)].into_iter().enumerate() {
        r.send(&v0(x, y, z, 0x8000_FF00, k == 3));
    }
    r.end_list();
    r.render();
    let (l, rt) = (px565(&r.vram, 5, 10), px565(&r.vram, 58, 10));
    // the top colour gets 1/2, the one under it 1/4
    assert!(l >> 11 > (l >> 5 & 0x3F) >> 1, "left: red on top {l:04X}");
    assert!(rt >> 11 < (rt >> 5 & 0x3F) >> 1, "right: green on top {rt:04X}");
}

/// Modifier-volume header (opaque modifier list) with a volume instruction.
fn mod_hdr(mode: u32) -> [u32; 8] { [4 << 29 | 1 << 24 | 0x0080_0000, mode << 27, 0, 0, 0, 0, 0, 0] }
fn mod_tri(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> [u32; 16] {
    let mut w = [0u32; 16];
    w[0] = vtx(true);
    for (k, x) in a.iter().chain(&b).chain(&c).enumerate() { w[1 + k] = f(*x); }
    w
}

/// A box over x, y 0..64 from z = 0.5 (behind 1) to 2 (in front): its front face toggles
/// the surface behind it, its back face doesn't, the sides are edge-on.
fn shadow_box(r: &mut Rig, mode: u32) {
    r.send(&mod_hdr(0));
    r.send(&mod_tri([0.0, 0.0, 2.0], [64.0, 0.0, 2.0], [0.0, 64.0, 2.0]));
    r.send(&mod_tri([64.0, 0.0, 2.0], [0.0, 64.0, 2.0], [64.0, 64.0, 2.0]));
    r.send(&mod_tri([0.0, 0.0, 0.5], [64.0, 0.0, 0.5], [0.0, 64.0, 0.5]));
    r.send(&mod_hdr(mode));
    r.send(&mod_tri([64.0, 0.0, 0.5], [0.0, 64.0, 0.5], [64.0, 64.0, 0.5]));
    r.end_list();
}

#[test]
fn modifier_volume_shadows_the_surface_inside() {
    for (mode, inside, outside) in [(1u32, 16u16, 31u16), (2, 31, 16)] {
        let mut r = Rig::new(0xFF00_0000);
        r.set(render::FPU_SHAD_SCALE, 0x100 | 0x80);
        // opaque quad, modifier bit (PCW 7), cheap shadow
        r.send(&[hdr(0, 0x80), ISP_GEQ, 0, 0, 0, 0, 0, 0]);
        rect(&mut r, 0.0, 0.0, 128.0, 128.0, 1.0, 0xFFFF_0000);
        r.end_list();
        shadow_box(&mut r, mode);
        let st = r.render();
        assert_eq!(st.volumes, 1);
        assert_eq!(px565(&r.vram, 20, 20) >> 11, inside, "mode {mode} inside");
        assert_eq!(px565(&r.vram, 100, 100) >> 11, outside, "mode {mode} outside");
    }
    // a polygon without the modifier bit is left alone
    let mut r = Rig::new(0xFF00_0000);
    r.set(render::FPU_SHAD_SCALE, 0x100 | 0x80);
    r.send(&[hdr(0, 0), ISP_GEQ, 0, 0, 0, 0, 0, 0]);
    rect(&mut r, 0.0, 0.0, 128.0, 128.0, 1.0, 0xFFFF_0000);
    r.end_list();
    shadow_box(&mut r, 1);
    r.render();
    assert_eq!(px565(&r.vram, 20, 20) >> 11, 31);
}

#[test]
fn two_volume_polygon_switches_parameters_in_the_volume() {
    let mut r = Rig::new(0xFF00_0000);
    // packed colour, two volumes: base0 word 4 (red), base1 word 5 (green)
    r.send(&[hdr(0, 0x80 | 0x40), ISP_GEQ, 0, 0, 0, 0, 0, 0]);
    for (k, (x, y)) in [(0.0f32, 0.0f32), (128.0, 0.0), (0.0, 128.0), (128.0, 128.0)].into_iter().enumerate() {
        r.send(&[vtx(k == 3), f(x), f(y), f(1.0), 0xFFFF_0000, 0xFF00_FF00, 0, 0]);
    }
    r.end_list();
    let s = r.pvr.ta.scene(0).unwrap();
    assert_eq!(s.verts[0].base1, [1.0, 0.0, 1.0, 0.0]);
    shadow_box(&mut r, 1);
    r.render();
    assert_eq!(px565(&r.vram, 20, 20), 0x07E0, "second volume inside");
    assert_eq!(px565(&r.vram, 100, 100), 0xF800, "first volume outside");
}

#[test]
fn mipmap_level_follows_the_size_on_screen() {
    // 8x8 mipmapped 565 at 0x8000: level 2^j filled with a colour of its own
    let colours = [0x001Fu16, 0x07E0, 0xF800, 0xFFFF]; // 1x1, 2x2, 4x4, 8x8
    let draw = |size: f32| -> u16 {
        let mut r = Rig::new(0xFF00_0000);
        for j in 0..4u32 {
            let off = [6u32, 8, 0x10, 0x30][j as usize];
            for k in 0..(1u32 << (2 * j)) {
                let a = (0x8000 + off + k * 2) as usize;
                r.vram[a..a + 2].copy_from_slice(&colours[j as usize].to_le_bytes());
            }
        }
        let tcw = 1 << 31 | 1 << 27 | 0x8000 >> 3;
        r.send(&[hdr(0, 8), ISP_GEQ, 0, tcw, 0, 0, 0, 0]);
        for (k, (x, y, u, v)) in [(0.0f32, 0.0f32, 0.0f32, 0.0f32), (size, 0.0, 1.0, 0.0), (0.0, size, 0.0, 1.0), (size, size, 1.0, 1.0)].into_iter().enumerate() {
            r.send(&[vtx(k == 3), f(x), f(y), f(1.0), f(u), f(v), 0xFFFF_FFFF, 0]);
        }
        r.end_list();
        r.render();
        px565(&r.vram, 0, 0)
    };
    assert_eq!(draw(64.0), 0xFFFF, "magnified: top level");
    assert_eq!(draw(8.0), 0xFFFF, "1:1");
    assert_eq!(draw(4.0), 0xF800, "half size: 4x4");
    assert_eq!(draw(2.0), 0x07E0, "quarter: 2x2");
}

#[test]
fn table_fog_and_colour_clamp() {
    let run = |table: u32, tsp: u32| -> u16 {
        let mut r = Rig::new(0xFF00_0000);
        r.set(render::FOG_COL_RAM, 0x0000_FF00);
        r.set(render::FOG_DENSITY, 0x8000); // 1.0
        for k in 0..128 { r.set(render::FOG_TABLE + 4 * k, table); }
        r.set(render::FOG_CLAMP_MAX, 0xFF80_FFFF);
        r.set(render::FOG_CLAMP_MIN, 0x0000_0000);
        r.send(&[hdr(0, 0), ISP_GEQ, tsp, 0, 0, 0, 0, 0]);
        rect(&mut r, 0.0, 0.0, 64.0, 64.0, 0.5, 0xFFFF_0000);
        r.end_list();
        r.render();
        px565(&r.vram, 10, 10)
    };
    assert_eq!(run(0xFFFF, 0 << 22), 0x07E0, "full fog: the fog colour");
    assert_eq!(run(0x0000, 0 << 22), 0xF800, "no fog");
    assert_eq!(run(0xFFFF, 2 << 22), 0xF800, "fog off in the TSP");
    // clamp: red at most 0x80
    assert_eq!(run(0x0000, 2 << 22 | 1 << 21) >> 11, 16);
}

#[test]
fn table_fog_varies_over_a_flat_coloured_slope() {
    // one flat colour but z from near (left) to far (right): fog must follow z per pixel
    let mut r = Rig::new(0xFF00_0000);
    r.set(render::FOG_COL_RAM, 0x0000_FF00);
    r.set(render::FOG_DENSITY, 0x8000);
    for k in 0..128u32 { r.set(render::FOG_TABLE + 4 * k, (2 * k) * 0x101); }
    // translucent ONE / ZERO (drawn directly, not deferred)
    r.send(&[hdr(2, 0), 7 << 29, 1 << 29, 0, 0, 0, 0, 0]);
    for (k, (x, y, z)) in [(0.0, 0.0, 1.0), (256.0, 0.0, 0.002), (0.0, 64.0, 1.0), (256.0, 64.0, 0.002)].into_iter().enumerate() {
        r.send(&v0(x, y, z, 0xFFFF_0000, k == 3));
    }
    r.end_list();
    r.render();
    let (a, b) = (px565(&r.vram, 4, 10), px565(&r.vram, 180, 10)); // both in the first triangle
    assert_ne!(a, b, "{a:04X} {b:04X}");
    assert_eq!(a & 0x07E0, 0, "near: no fog");
}

#[test]
fn secondary_accumulation_buffer_takes_the_write() {
    let mut r = Rig::new(0xFF00_0000);
    // ONE / ZERO into the secondary buffer: the primary stays black
    r.send(&[hdr(2, 0), 7 << 29, 1 << 29 | 1 << 24, 0, 0, 0, 0, 0]);
    rect(&mut r, 0.0, 0.0, 64.0, 64.0, 1.0, 0xFFFF_FFFF);
    // then ONE / ONE from the secondary buffer (source select) into the primary
    r.send(&[hdr(2, 0), 7 << 29, 1 << 29 | 1 << 26 | 1 << 25, 0, 0, 0, 0, 0]);
    rect(&mut r, 0.0, 0.0, 32.0, 64.0, 1.0, 0xFF00_0000);
    r.end_list();
    r.set(render::ISP_FEED_CFG, 1); // keep the order
    r.render();
    assert_eq!(px565(&r.vram, 48, 10), 0, "secondary only");
    assert_eq!(px565(&r.vram, 10, 10), 0xFFFF, "copied over from the secondary buffer");
}

#[test]
fn render_to_texture_uses_the_64_bit_path() {
    let mut r = Rig::new(0xFF12_3456);
    r.tiles(&[(0, 0)]);
    r.set(render::FB_W_SOF1, 0x100_0000 | 0x30_0000);
    r.render();
    let a = 0x30_0000 + 5 * 1280 + 7 * 2;
    assert_eq!(u16::from_le_bytes([r.vram[a], r.vram[a + 1]]), 0x11AA);
}

/// cargo test --release --lib render_speed -- --ignored --nocapture
#[test]
#[ignore]
fn render_speed() {
    let mut r = Rig::new(0xFF20_4060);
    // 8x8 565 texture
    for k in 0..64usize { let c = (k as u16).wrapping_mul(0x1234); r.vram[0x8000 + 2 * k..0x8002 + 2 * k].copy_from_slice(&c.to_le_bytes()); }
    let tcw = 1 << 27 | 0x8000 >> 3;
    let mut seed = 1u32;
    let mut rnd = |m: f32| { seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345); (seed >> 8) as f32 / 16_777_216.0 * m };
    let env = |k: &str, d: usize| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
    // 3000 opaque textured Gouraud strips of 4 (~40 px quads), culling off
    for _ in 0..env("RS_OPAQUE", 3000) {
        r.send(&[hdr(0, 8 | GOURAUD), ISP_GEQ, 1 << 20 | 1 << 13, tcw, 0, 0, 0, 0]);
        let (x, y, z) = (rnd(600.0), rnd(440.0), 0.1 + rnd(5.0));
        for (k, (dx, dy)) in [(0.0, 0.0), (40.0, 0.0), (0.0, 40.0), (40.0, 40.0)].into_iter().enumerate() {
            r.send(&[vtx(k == 3), f(x + dx), f(y + dy), f(z), f(dx / 40.0), f(dy / 40.0), 0xFF80_8080 + k as u32 * 0x10, 0]);
        }
    }
    r.end_list();
    // 40 full-screen translucent layers
    for k in 0..env("RS_TRANS", 40) {
        r.send(&[hdr(2, 0), 0, 4 << 29 | 5 << 26 | 1 << 20, 0, 0, 0, 0, 0]);
        rect(&mut r, 0.0, 0.0, 640.0, 480.0, 6.0 + k as f32 * 0.01, 0x2040_6080);
    }
    r.end_list();
    let t = std::time::Instant::now();
    let n = env("RS_N", 10);
    let mut st = Default::default();
    for _ in 0..n { st = r.render(); }
    println!("render: {:.1} ms each ({} pixels)", t.elapsed().as_secs_f64() * 1000.0 / n as f64, st.pixels);
}

#[test]
fn textures_stay_cached_until_their_video_ram_is_written() {
    use super::render::{RenderCache, TexCache};
    use super::texture::Tex;
    // 8 x 8 RGB565 stride texture at 0x1000
    let t = Tex::new(0, 1 << 27 | 0x0400_0000 | 0x1000 >> 3, 0, 0);
    let mut vram = vec![0u8; 8 << 20];
    let mut rc = RenderCache::default();
    let pal = vec![0u32; 1024];
    let get = |c: &TexCache, v: &[u8]| c.get(&t, (1, 2, 0, 0), v, &pal)[0];
    let _ = rc.prepare(&vram);
    assert_eq!(get(&rc.tex, &vram), 0xFF00_0000);
    vram[0x1000..0x1002].copy_from_slice(&0xF800u16.to_le_bytes());
    // not marked: the cached (old) texels
    let _ = rc.prepare(&vram);
    assert_eq!(get(&rc.tex, &vram), 0xFF00_0000);
    // a write elsewhere leaves it
    rc.mark(0x8000, 0x8003);
    let _ = rc.prepare(&vram);
    assert_eq!(get(&rc.tex, &vram), 0xFF00_0000);
    rc.mark(0x1000, 0x1001);
    let _ = rc.prepare(&vram);
    assert_eq!(get(&rc.tex, &vram), 0xFFFF_0000);
}

#[test]
fn the_render_copy_of_video_ram_follows_marked_writes() {
    let mut rc = super::render::RenderCache::default();
    let mut vram = vec![0u8; 8 << 20];
    let s = rc.prepare(&vram);
    rc.snap = Some(s);
    vram[0x2_3456] = 7;
    vram[0x40_0000] = 9;
    rc.mark(0x2_3456, 0x2_3456);
    let s = rc.prepare(&vram);
    assert_eq!(s[0x2_3456], 7, "the written page is copied");
    assert_eq!(s[0x40_0000], 0, "an unmarked page is not (only marked pages are copied)");
}

#[test]
fn cpu_and_direct_path_writes_mark_video_ram() {
    use crate::common::cpu::sh4::Sh4Bus;
    let mut d = super::bus::Dc::new(vec![0; super::BIOS_SIZE], &[], 0);
    let s = d.rcache.prepare(&d.vram);
    d.rcache.snap = Some(s);
    d.write32(0xA500_1000, 0x1234_5678);     // 32-bit area
    d.write16(0x1100_8000, 0xBEEF);          // direct texture path (64-bit)
    let s = d.rcache.prepare(&d.vram);
    assert_eq!(s, d.vram);
    assert!(d.vram.iter().any(|&b| b == 0x78), "32-bit write landed");
    assert!(d.vram.iter().any(|&b| b == 0xEF), "direct write landed");
}

/// A nearly level edge (Sonic Adventure's floor, clipped at the near plane: two vertices
/// at y 7861.02 a couple of ulps apart) crosses each row billions of pixels away; the span
/// must not wrap round and drop the triangle.
#[test]
fn a_nearly_level_far_edge_keeps_its_triangle() {
    let mut r = Rig::new(0xFF00_0000);
    r.send(&[hdr(0, 0), ISP_GEQ, 0, 0, 0, 0, 0, 0]);
    let b = f32::from_bits;
    r.send(&v0(b(0x4625_795E), b(0x45F5_A825), 1.0, 0xFFFF_FFFF, false));
    r.send(&v0(b(0x470A_74B7), b(0x45F5_A827), 1.0, 0xFFFF_FFFF, false));
    r.send(&v0(b(0x4334_DE9A), b(0x4393_FD9C), b(0x3C91_B86F), 0xFFFF_FFFF, true));
    r.end_list();
    r.render();
    // the triangle widens down from (181, 296): about x 227-338 at row 330, 419-991 at 470
    assert_eq!(px565(&r.vram, 280, 330), 0xFFFF);
    assert_eq!(px565(&r.vram, 500, 470), 0xFFFF);
    assert_eq!(px565(&r.vram, 100, 330), 0);
}

/// The word-at-a-time framebuffer write and picture output give exactly what the per-pixel
/// reference loops give, over pack modes, clips, strides, scaling, line doubling.
#[test]
fn fast_framebuffer_paths_match_the_reference() {
    use super::pvr::{self, Pvr, SLOW_OUTPUT};
    use std::sync::atomic::Ordering;
    let mut seed = 0x1357_9BDFu32;
    let mut next = move || { seed ^= seed << 13; seed ^= seed >> 17; seed ^= seed << 5; seed };
    let (w, h) = (640usize, 480usize);
    let col: Vec<u32> = (0..w * h).map(|_| next()).collect();
    let tiles: Vec<(u32, u32)> = (0..15).flat_map(|y| (0..20).map(move |x| (x, y))).collect();
    for case in 0..24u32 {
        let mut regs = vec![0u32; 0x800];
        let pack = [0, 1, 2, 3, 5, 6][case as usize % 6];
        regs[render::FB_W_CTRL as usize / 4] = pack | (next() & 0xFF) << 8 | (next() & 0xFF) << 16;
        let bpp = if pack >= 5 { 4 } else { 2 };
        regs[render::FB_W_LINESTRIDE as usize / 4] = (640 * bpp + 8 * (case % 3)) / 8;
        regs[render::FB_W_SOF1 as usize / 4] = 0x20_0000 + 4 * (case % 4) + if case % 5 == 0 { 0x100_0000 } else { 0 };
        regs[render::FB_X_CLIP as usize / 4] = if case % 2 == 0 { 639 << 16 } else { 600 << 16 | 17 };
        regs[render::FB_Y_CLIP as usize / 4] = if case % 2 == 0 { 479 << 16 } else { 400 << 16 | 5 };
        regs[render::SCALER_CTL as usize / 4] = if case % 7 == 6 { 0x1_0400 } else { 0x400 };
        let mut a = vec![0u8; 8 << 20];
        let mut b = a.clone();
        SLOW_OUTPUT.store(true, Ordering::Relaxed);
        let ra = render::write_out(&regs, &col, w, h, &tiles, &mut a);
        SLOW_OUTPUT.store(false, Ordering::Relaxed);
        let rb = render::write_out(&regs, &col, w, h, &tiles, &mut b);
        assert!(a == b && ra == rb, "write-out case {case}");
        // read it back out as the display would
        let mut p = Pvr::new();
        p.regs[pvr::FB_R_CTRL as usize / 4] = 1 | [1u32, 0, 3][case as usize % 3] << 2 | (case & 8 != 0) as u32 * 2;
        p.regs[pvr::FB_R_SOF1 as usize / 4] = 0x20_0000 + 4 * (case % 2);
        p.regs[pvr::FB_R_SOF1 as usize / 4 + 1] = 0x20_0000 + 0x500;
        let words = if case % 3 == 2 { 640 } else { 320 } - if case % 4 == 3 { 40 } else { 0 };
        p.regs[pvr::FB_R_SIZE as usize / 4] = (words - 1) | 239 << 10 | 1 << 20;
        if case & 16 != 0 { p.regs[pvr::VO_CONTROL as usize / 4] |= 0x100; } else { p.regs[pvr::VO_CONTROL as usize / 4] &= !0x100; }
        if case % 3 == 1 { p.regs[pvr::SPG_CONTROL as usize / 4] |= 0x10; }
        let (mut oa, mut ob) = (vec![0u8; 640 * 480 * 4], vec![0u8; 640 * 480 * 4]);
        SLOW_OUTPUT.store(true, Ordering::Relaxed);
        p.output(&a, &mut oa);
        SLOW_OUTPUT.store(false, Ordering::Relaxed);
        p.output(&a, &mut ob);
        assert!(oa == ob, "output case {case}");
    }
}
