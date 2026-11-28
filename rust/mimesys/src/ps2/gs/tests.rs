use super::mem::{self, *};
use super::*;

fn gif_tag(nloop: u64, eop: bool, prim: Option<u64>, flg: u64, regs: &[u64]) -> u128 {
    let mut lo = nloop | (eop as u64) << 15 | flg << 58 | ((regs.len() as u64) & 15) << 60;
    if let Some(p) = prim { lo |= 1 << 46 | p << 47; }
    let hi = regs.iter().enumerate().fold(0u64, |a, (k, &r)| a | r << (4 * k));
    lo as u128 | (hi as u128) << 64
}

fn ad(reg: usize, v: u64) -> u128 { v as u128 | (reg as u128) << 64 }

/// A 64x64 PSMCT32 frame at block 0, scissor over it, no Z
fn setup(gs: &mut Gs) {
    gs.write_reg(FRAME_1, 1 << 16);
    gs.write_reg(SCISSOR_1, 63 << 16 | 63 << 48);
    gs.write_reg(TEST_1, 0);
}

fn px(gs: &mut Gs, x: u32, y: u32) -> u32 { gs.flush(); mem::read(&gs.vram, PSMCT32, x, y, 0, 1) }

#[test]
fn every_layout_round_trips_and_pixels_dont_collide() {
    let mut vram = vec![0u8; VRAM_SIZE];
    for &psm in &[PSMCT32, PSMCT16, PSMCT16S, PSMT8, PSMT4, PSMZ32, PSMZ16] {
        vram.iter_mut().for_each(|b| *b = 0);
        let mask = if mem::bpp(psm) == 32 { u32::MAX } else { (1u32 << mem::bpp(psm)) - 1 };
        for y in 0..128 { for x in 0..128 { mem::write(&mut vram, psm, x, y, 0, 2, (x * 131 + y * 7 + 1) & mask); } }
        for y in 0..128 { for x in 0..128 {
            assert_eq!(mem::read(&vram, psm, x, y, 0, 2), (x * 131 + y * 7 + 1) & mask, "psm {psm:X} at {x},{y}");
        } }
    }
}

#[test]
fn psmct32_page_layout_matches_the_manual() {
    // block 1 of a page is at (8, 0); pixel (1, 0) is word 1, (0, 1) is word 2
    assert_eq!(mem::addr32(8, 0, 0, 1, false), 256);
    assert_eq!(mem::addr32(1, 0, 0, 1, false), 4);
    assert_eq!(mem::addr32(0, 1, 0, 1, false), 8);
    assert_eq!(mem::addr32(0, 32, 0, 1, false), 8192, "the next page down");
    assert_eq!(mem::addr32(0, 0, 0, 1, true), 0x18 * 256, "Z32: block XOR 18h");
}

#[test]
fn a_32_bit_upload_through_gif_image_lands_where_it_should() {
    let mut gs = Gs::new();
    gs.feed(gif_tag(4, false, None, 0, &[0xE]));
    gs.feed(ad(BITBLTBUF, (0u64 << 32) | 1 << 48 | 0 << 56));   // DBP 0, DBW 1, PSMCT32
    gs.feed(ad(TRXPOS, 4 << 32 | 2 << 48));                     // to (4, 2)
    gs.feed(ad(TRXREG, 2 | 2 << 32));                           // 2 x 2
    gs.feed(ad(TRXDIR, 0));
    gs.feed(gif_tag(1, true, None, 2, &[]));
    gs.feed(0x0000_0004_0000_0003_0000_0002_0000_0001);
    assert_eq!([px(&mut gs, 4, 2), px(&mut gs, 5, 2), px(&mut gs, 4, 3), px(&mut gs, 5, 3)], [1, 2, 3, 4]);
}

#[test]
fn a_gouraud_triangle_fills_with_mixed_colour() {
    let mut gs = Gs::new();
    setup(&mut gs);
    gs.write_reg(PRIM, 3 | 1 << 3);                             // triangle, Gouraud
    for (x, y, c) in [(0u64, 0u64, 0x0000_00FFu64), (32, 0, 0x0000_FF00), (0, 32, 0x00FF_0000)] {
        gs.write_reg(RGBAQ, c | 0x8000_0000);
        gs.write_reg(XYZ2, (x * 16) | (y * 16) << 16);
    }
    assert_eq!(px(&mut gs, 0, 0) & 0xFF_FFFF, 0xFF, "a corner keeps its colour");
    let mid = px(&mut gs, 8, 8);
    assert!((mid & 0xFF) > 0x40 && (mid >> 8 & 0xFF) > 0x20 && (mid >> 16 & 0xFF) > 0x20, "mixed: {mid:08X}");
    assert_eq!(px(&mut gs, 31, 31), 0, "outside the hypotenuse");
    assert_eq!(px(&mut gs, 32, 0), 0, "the right edge isn't drawn (top-left rule)");
}

#[test]
fn a_sprite_textured_through_an_8_bit_clut() {
    let mut gs = Gs::new();
    setup(&mut gs);
    // the CLUT at block 64 (CSM1, 32-bit): index 5 -> pure blue, alpha 80
    mem::write(&mut gs.vram, PSMCT32, 5, 0, 64, 1, 0x80FF_0000);
    // an 8-bit 16x16 texture at block 128: all index 5
    for y in 0..16 { for x in 0..16 { mem::write(&mut gs.vram, PSMT8, x, y, 128, 2, 5); } }
    // TEX0: TBP 128, TBW 2, PSMT8, 16x16, TCC, DECAL, CBP 64, CPSM 32, CLD 1
    let tex0 = 128 | 2 << 14 | (PSMT8 as u64) << 20 | 4 << 26 | 4 << 30 | 1 << 34 | 1 << 35 | 64 << 37 | 1 << 61;
    gs.write_reg(TEX0_1, tex0);
    gs.write_reg(PRIM, 6 | 1 << 4 | 1 << 8);                    // sprite, textured, UV
    gs.write_reg(UV, 0);
    gs.write_reg(XYZ2, 10 * 16 | (10 * 16) << 16);
    gs.write_reg(UV, 16 * 16 | (16 * 16) << 16);
    gs.write_reg(XYZ2, 26 * 16 | (26 * 16) << 16);
    assert_eq!(px(&mut gs, 10, 10), 0x80FF_0000, "the CLUT colour");
    assert_eq!(px(&mut gs, 25, 25), 0x80FF_0000);
    assert_eq!(px(&mut gs, 26, 26), 0, "the far edge is exclusive");
}

#[test]
fn alpha_blending_mixes_source_over_destination() {
    let mut gs = Gs::new();
    setup(&mut gs);
    mem::write(&mut gs.vram, PSMCT32, 3, 3, 0, 1, 0x0000_00FF);  // red underneath
    // (Cs - Cd) * As >> 7 + Cd, As = 40h (a half)
    gs.write_reg(ALPHA_1, 0 | 1 << 2 | 0 << 4 | 1 << 6);
    gs.write_reg(PRIM, 0 | 1 << 6);                             // point, blended
    gs.write_reg(RGBAQ, 0x4000_FF00);                           // green, alpha 40h
    gs.write_reg(XYZ2, 3 * 16 | (3 * 16) << 16);
    let p = px(&mut gs, 3, 3);
    // red: (0 - 255) * 64 >> 7 = -128 (an arithmetic shift rounds down), + 255 = 127
    assert_eq!((p & 0xFF, p >> 8 & 0xFF), (0x7F, 0x7F), "half red, half green: {p:08X}");
}

#[test]
fn the_z_test_keeps_nearer_pixels() {
    let mut gs = Gs::new();
    setup(&mut gs);
    gs.write_reg(ZBUF_1, 16 | 0 << 24);                         // Z32 at block 512
    gs.write_reg(TEST_1, 1 << 16 | 2 << 17);                    // ZTE, GEQUAL
    gs.write_reg(PRIM, 0);
    for (z, c) in [(100u64, 0xFFu64), (50, 0xFF00), (200, 0xFF0000)] {
        gs.write_reg(RGBAQ, c);
        gs.write_reg(XYZ2, 7 * 16 | (7 * 16) << 16 | z << 32);
    }
    assert_eq!(px(&mut gs, 7, 7) & 0xFF_FFFF, 0xFF0000, "the largest Z wins under GEQUAL");
    assert_eq!(mem::read(&gs.vram, PSMZ32, 7, 7, 512, 1), 200);
}

#[test]
fn signal_and_finish_raise_csr_flags() {
    let mut gs = Gs::new();
    gs.write_reg(SIGNAL, 0x1234 | 0xFFFF << 32);
    gs.write_reg(FINISH, 0);
    assert_eq!(gs.csr & 3, 3);
    assert_eq!(gs.siglblid & 0xFFFF, 0x1234);
    assert!(!gs.irq(), "masked by default (IMR 7F00)");
    gs.imr = 0x7E00;
    assert!(gs.irq());
}

#[test]
fn the_display_reads_circuit_1() {
    let mut gs = Gs::new();
    gs.pregs[PMODE] = 1;
    // DISPFB1: FBP 0, FBW 1, PSMCT32; DISPLAY1: 64 x 32 (MAGH 1)
    gs.pregs[DISPFB1] = 1 << 9;
    gs.pregs[DISPLAY1] = (63u64 << 32) | (31u64 << 44);
    mem::write(&mut gs.vram, PSMCT32, 2, 1, 0, 1, 0x00_33_22_11);
    let mut out = Vec::new();
    assert_eq!(gs.display(&mut out), (64, 32));
    let o = (64 + 2) * 4;
    assert_eq!(&out[o..o + 4], &[0x11, 0x22, 0x33, 0xFF]);
}

/// A menu-like workload (OSDSYS: PSMCT32 640x448 frame, Z32 GREATER, textured alpha
/// sprites over the screen, gouraud / textured triangles). Returns VRAM's hash.
fn menu_scene(gs: &mut Gs, frames: u32) -> u64 {
    let xy = |x: i32, y: i32, z: u32| ((x as u64 * 16) & 0xFFFF) | ((y as u64 * 16) & 0xFFFF) << 16 | (z as u64) << 32;
    gs.write_reg(FRAME_1, 0x50 | 10 << 16);                     // FBP 50h, 640 wide, PSMCT32
    gs.write_reg(ZBUF_1, 0xDC);                                  // ZBP DCh (after the frame), Z32
    gs.write_reg(SCISSOR_1, 639 << 16 | 447u64 << 48);
    gs.write_reg(TEST_1, 0x30000);                               // ZTE, GREATER
    gs.write_reg(ALPHA_1, 0x44);                                 // (Cs - Cd) * As + Cd
    // a 256x256 PSMCT32 texture at block 0x3200 (page 400)
    for y in 0..256u32 { for x in 0..256u32 {
        mem::write(&mut gs.vram, PSMCT32, x, y, 0x3200, 4, x | y << 8 | ((x ^ y) & 0xFF) << 16 | 0x6000_0000);
    } }
    gs.write_reg(TEX0_1, 0x3200 | 4 << 14 | 8 << 26 | 8 << 30 | 1 << 34);   // 256x256, TCC, MODULATE
    let mut seed = 1u32;
    let mut rnd = |n: u32| { seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345); (seed >> 8) % n };
    for _ in 0..frames {
        // 6 full-screen textured, blended sprites (UV)
        gs.write_reg(PRIM, 6 | 1 << 4 | 1 << 6 | 1 << 8);
        for k in 0..6 {
            gs.write_reg(RGBAQ, 0x4080_8080 | (k as u64) << 8);
            gs.write_reg(UV, 0);
            gs.write_reg(XYZ2, xy(0, 0, 100 + k));
            gs.write_reg(UV, (255 * 16) | (255 * 16) << 16);
            gs.write_reg(XYZ2, xy(640, 448, 100 + k));
        }
        // 600 gouraud triangles, half textured (STQ), half blended
        for t in 0..600 {
            let tex = t & 1 == 0;
            gs.write_reg(PRIM, 3 | 1 << 3 | (tex as u64) << 4 | ((t & 2 != 0) as u64) << 6);
            for _ in 0..3 {
                gs.write_reg(RGBAQ, (rnd(256) | rnd(256) << 8 | rnd(256) << 16 | 0x80 << 24) as u64 | (1.0f32.to_bits() as u64) << 32);
                gs.write_reg(ST, (rnd(1000) as f32 / 1000.0).to_bits() as u64 | ((rnd(1000) as f32 / 1000.0).to_bits() as u64) << 32);
                gs.write_reg(XYZ2, xy(rnd(640) as i32, rnd(448) as i32, 1000 + rnd(1000)));
            }
        }
    }
    gs.flush();
    gs.vram.iter().fold(0xcbf29ce484222325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

/// `cargo test --release --lib gs_menu_bench -- --ignored --nocapture`: pixels per second
#[test]
#[ignore]
fn gs_menu_bench() {
    let mut gs = Gs::new();
    let t = std::time::Instant::now();
    let n = std::env::var("GS_FRAMES").ok().and_then(|v| v.parse().ok()).unwrap_or(10);
    let h = menu_scene(&mut gs, n);
    let s = t.elapsed().as_secs_f64();
    println!("GS bench: {} pixels in {:.2} s = {:.1} M pixels/s ({:.1} ms a frame); VRAM hash {h:016X}",
        gs.pixels, s, gs.pixels as f64 / s / 1e6, s * 1000.0 / n as f64);
}

/// TRXDIR 1 (local -> host) packs the source rectangle for VIF1 to read back
#[test]
fn a_local_to_host_transfer_reads_back_what_was_uploaded() {
    let mut gs = Gs::new();
    gs.feed(gif_tag(4, false, None, 0, &[0xE]));
    gs.feed(ad(BITBLTBUF, (0u64 << 32) | 1 << 48 | 0 << 56));
    gs.feed(ad(TRXPOS, 4 << 32 | 2 << 48));
    gs.feed(ad(TRXREG, 2 | 2 << 32));
    gs.feed(ad(TRXDIR, 0));
    gs.feed(gif_tag(1, true, None, 2, &[]));
    gs.feed(0x0000_0004_0000_0003_0000_0002_0000_0001);
    // read it back: source (4, 2), 2 x 2, PSMCT32 at SBP 0, SBW 1
    gs.feed(gif_tag(4, false, None, 0, &[0xE]));
    gs.feed(ad(BITBLTBUF, 0 | 1 << 16 | 0 << 24));
    gs.feed(ad(TRXPOS, 4 | 2 << 16));
    gs.feed(ad(TRXREG, 2 | 2 << 32));
    gs.feed(ad(TRXDIR, 1));
    assert_eq!(gs.readback.len(), 1);
    assert_eq!(gs.readback[0], 0x0000_0004_0000_0003_0000_0002_0000_0001);
}

/// Dragon Quest 8's font: uploads to PSMT4HH / 4HL / 8H take 4 / 4 / 8 bits a pixel from
/// the stream and land in the top of 32-bit pixels, leaving the rest alone
#[test]
fn uploads_to_the_high_bits_formats_are_4_and_8_bits_a_pixel() {
    let mut gs = Gs::new();
    let up = |gs: &mut Gs, psm: u64, q: u128| {
        gs.feed(gif_tag(4, false, None, 0, &[0xE]));
        gs.feed(ad(BITBLTBUF, 1 << 48 | psm << 56));
        gs.feed(ad(TRXPOS, 0));
        gs.feed(ad(TRXREG, 8 | 1 << 32));
        gs.feed(ad(TRXDIR, 0));
        gs.feed(gif_tag(1, true, None, 2, &[]));
        gs.feed(q);
    };
    // 8 pixels of 4 bits = 4 bytes (the quadword's rest is padding)
    up(&mut gs, 0x2C, 0x8765_4321);
    up(&mut gs, 0x24, 0xFEDC_BA98);
    gs.flush();
    for x in 0..8u32 {
        let p = mem::read(&gs.vram, mem::PSMCT32, x, 0, 0, 1);
        assert_eq!((p >> 28, p >> 24 & 15, p & 0xFF_FFFF), (x + 1, x + 8, 0), "pixel {x}");
    }
    // 8 pixels of 8 bits
    up(&mut gs, 0x1B, 0x0807_0605_0403_0201);
    gs.flush();
    for x in 0..8u32 { assert_eq!(mem::read(&gs.vram, mem::PSMT8H, x, 0, 0, 1), x + 1); }
    // and back out the same size
    gs.feed(gif_tag(4, false, None, 0, &[0xE]));
    gs.feed(ad(BITBLTBUF, 1 << 16 | 0x2C << 24));
    gs.feed(ad(TRXPOS, 0));
    gs.feed(ad(TRXREG, 8 | 1 << 32));
    gs.feed(ad(TRXDIR, 1));
    assert_eq!(gs.readback.len(), 1);
    assert_eq!(gs.readback[0] as u32, 0x0000_0000);
}

/// A recorded frame plays back to the same VRAM
#[test]
fn a_recorded_frame_replays() {
    let mut gs = Gs::new();
    setup(&mut gs);
    let cfg = bincode::config::standard().with_fixed_int_encoding();
    gs.flush();
    gs.rec = Some((bincode::serde::encode_to_vec(&gs, cfg).unwrap(), Vec::new()));
    gs.feed(gif_tag(4, false, None, 0, &[0xE]));
    gs.feed(ad(PRIM, 6));
    gs.feed(ad(RGBAQ, 0x8012_3456));
    gs.feed(ad(XYZ2, 0));
    gs.feed(ad(XYZ2, (16 * 16) | (16 * 16) << 16));
    gs.flush();
    let (state, qs) = gs.rec.take().unwrap();
    let file = super::frame_file(&state, &qs, &gs.pregs);
    let mut back = super::replay(&file).expect("replays");
    back.flush();
    assert!(back.vram == gs.vram, "same VRAM");
    assert_eq!(px(&mut back, 3, 3) & 0xFF_FFFF, 0x12_3456);
}

/// PS2_GS_FRAME=gs_frame.bin [PS2_PPM=out.ppm]: replay a recorded frame, show it
#[test]
#[ignore]
fn gs_frame_replay() {
    let path = std::env::var("PS2_GS_FRAME").expect("PS2_GS_FRAME");
    let file = std::fs::read(path).unwrap();
    let t = std::time::Instant::now();
    let mut gs = super::replay(&file).expect("a GSF1 file");
    gs.flush();
    println!("replayed in {:?} ({} batches, {} waits, {} prims, {} pixels)", t.elapsed(), gs.flushes, gs.syncs, gs.prims, gs.pixels);
    // PS2_VRAM_OUT=file: all of VRAM after the replay (to compare drawing changes)
    if let Ok(p) = std::env::var("PS2_VRAM_OUT") { std::fs::write(p, &gs.vram).unwrap(); }
    let mut out = Vec::new();
    let (w, h) = gs.display(&mut out);
    println!("display {w}x{h} PMODE {:X} SMODE2 {:X} DISPFB1 {:X} DISPLAY1 {:X} DISPFB2 {:X} DISPLAY2 {:X}", gs.pregs[0], gs.pregs[2], gs.pregs[7], gs.pregs[8], gs.pregs[9], gs.pregs[10]);
    if let Ok(p) = std::env::var("PS2_PPM") {
        let mut ppm = format!("P6 {w} {h} 255\n").into_bytes();
        for px in out.chunks(4) { ppm.extend_from_slice(&px[..3]); }
        std::fs::write(p, ppm).unwrap();
    }
}

/// PS2_GS_FRAME + GS_DUMP=bp,bw,psm,w,h,out.ppm: a VRAM region after the replay
#[test]
#[ignore]
fn gs_frame_vram_dump() {
    let file = std::fs::read(std::env::var("PS2_GS_FRAME").unwrap()).unwrap();
    let mut gs = super::replay(&file).unwrap();
    gs.flush();
    let a: Vec<String> = std::env::var("GS_DUMP").unwrap().split(',').map(|s| s.to_string()).collect();
    let n = |s: &str| u32::from_str_radix(s.trim_start_matches("0x"), 16).unwrap();
    let (bp, bw, psm, w, h) = (n(&a[0]), n(&a[1]), n(&a[2]), n(&a[3]), n(&a[4]));
    let mut ppm = format!("P6 {w} {h} 255\n").into_bytes();
    for y in 0..h {
        for x in 0..w {
            let p = super::mem::read(&gs.vram, psm, x, y, bp, bw);
            // GS_DUMP_ALPHA=1: the alpha byte as grey instead
            if std::env::var_os("GS_DUMP_ALPHA").is_some() {
                let a = (p >> 24) as u8;
                ppm.extend_from_slice(&[a, a, a]);
            } else {
                ppm.extend_from_slice(&[p as u8, (p >> 8) as u8, (p >> 16) as u8]);
            }
        }
    }
    std::fs::write(&a[5], ppm).unwrap();
}

/// Persona 3 FES draws its text twice at Z FFFF with ZTST GEQUAL: a Z equal at all three
/// vertices must interpolate to exactly that value, or the second pass loses pixels
#[test]
fn a_constant_z_interpolates_exactly() {
    let mut gs = Gs::new();
    gs.write_reg(FRAME_1, 4 << 16);                             // 256 wide
    gs.write_reg(SCISSOR_1, 255 << 16 | 255 << 48);
    gs.write_reg(ZBUF_1, 0x100 | 0xA << 24);                   // PSMZ16S
    // Persona's glyph quads: 28 px squares at quarter-pixel positions, two triangles each
    let quads: Vec<(u64, u64)> = (0..14).flat_map(|k| (0..6).map(move |r| (0x84 + k * 0xC4 + r * 0x14, 0x40 + r * 0x1C4))).collect();
    let tri = |gs: &mut Gs, t: [(u64, u64); 3], colour: u64| {
        gs.write_reg(PRIM, 3);
        gs.write_reg(RGBAQ, colour);
        for (vx, vy) in t { gs.write_reg(XYZ2, vx | vy << 16 | 0xFFFF << 32); }
    };
    // the panel behind (ALWAYS), then the glyphs (GEQUAL)
    gs.write_reg(TEST_1, 1 << 16 | 1 << 17);
    tri(&mut gs, [(0x13, 0x27), (0x3F35, 0x41), (0x2B, 0x3F59)], 0x11);
    tri(&mut gs, [(0x3F35, 0x41), (0x3F43, 0x3F61), (0x2B, 0x3F59)], 0x11);
    gs.write_reg(TEST_1, 1 << 16 | 2 << 17);
    for &(x, y) in &quads {
        let d = 0x1C0;
        tri(&mut gs, [(x, y), (x + d, y), (x, y + d)], 0x22);
        tri(&mut gs, [(x + d, y), (x + d, y + d), (x, y + d)], 0x22);
    }
    gs.flush();
    for &(x, y) in &quads {
        // a pixel well inside each quad
        let (px, py) = ((x as u32 + 0xE0) >> 4, (y as u32 + 0xE0) >> 4);
        let p = mem::read(&gs.vram, PSMCT32, px, py, 0, 4) & 0xFF;
        assert_eq!(p, 0x22, "glyph pixel {px},{py} lost the Z test");
    }
    let mut lost = 0;
    for y in 0..256 { for x in 0..256 {
        let z = mem::read(&gs.vram, PSMZ16S, x, y, 0x2000, 4);
        if z != 0 && z != 0xFFFF { lost += 1; }
    } }
    assert_eq!(lost, 0, "Z values other than FFFF written");
}

/// TEX2 sets TEX0's PSM / CLUT fields and loads the CLUT like TEX0 (KH2's font CLUT)
#[test]
fn tex2_loads_the_clut() {
    let mut gs = Gs::new();
    // a 16-entry CT32 CLUT at block 100h (8 x 2, bw 1): entry i = i * 0x01010101
    gs.write_reg(BITBLTBUF, (0x100u64 << 32) | 1 << 48);
    gs.write_reg(TRXPOS, 0);
    gs.write_reg(TRXREG, 8 | 2 << 32);
    gs.write_reg(TRXDIR, 0);
    for i in (0..16u64).step_by(2) { gs.write_reg(HWREG, i * 0x0101_0101 | (i + 1) * 0x0101_0101 << 32); }
    gs.write_reg(TEX0_1, 0);
    // TEX2: PSMT4, CBP 100h, CSA 2, CLD 1
    gs.write_reg(TEX2_1, 0x14 << 20 | 0x100u64 << 37 | 2u64 << 56 | 1u64 << 61);
    gs.flush();
    assert_eq!(gs.regs[TEX0_1] >> 20 & 0x3F, 0x14);
    assert_eq!(gs.clut[32 + 5], 5 * 0x0101_0101);
}
