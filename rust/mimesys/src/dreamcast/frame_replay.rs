//! Replays a dc_frame.bin a player saved (option "Save the next 3D frame"): renders it and
//! writes the picture, with a listing of the polygons and their textures.
//!
//! DC_FRAME=path/dc_frame.bin [DC_OUT=out.ppm] [DC_ONLY=3,7,10-20] [DC_LIST=1]
//! cargo test --release --lib replay_frame -- --ignored --nocapture

use super::render::{self, FrameDump, TexCache};
use super::ta::*;

fn parse_set(s: &str) -> Vec<usize> {
    let mut v = Vec::new();
    for part in s.split(',').filter(|p| !p.is_empty()) {
        if let Some((a, b)) = part.split_once('-') {
            v.extend(a.trim().parse::<usize>().unwrap()..=b.trim().parse::<usize>().unwrap());
        } else {
            v.push(part.trim().parse().unwrap());
        }
    }
    v
}

#[test]
#[ignore]
fn replay_frame() {
    let path = std::env::var("DC_FRAME").expect("DC_FRAME=path to dc_frame.bin");
    let d = FrameDump::load(&std::fs::read(&path).unwrap()).expect("a frame dump");
    let mut scene = d.scene.clone();
    if let (Ok(only), Some(s)) = (std::env::var("DC_ONLY"), scene.as_mut()) {
        let keep = parse_set(&only);
        for (n, p) in s.polys.iter_mut().enumerate() {
            if !keep.contains(&n) { p.count = 0; } else if std::env::var("DC_VERTS").is_ok() {
                for v in &s.verts[p.first as usize..(p.first + p.count) as usize] {
                    println!("poly {n}: bits {:08X} {:08X} {:08X} x {:.2} y {:.2} 1/w {:.6} u {:.4} v {:.4} base {:?} clip {:?} {}", v.x.to_bits(), v.y.to_bits(), v.z.to_bits(), v.x, v.y, v.z, v.u, v.v, v.base, p.clip, p.clip_mode);
                }
            }
        }
    }
    // DC_NOTEX=1: drop textures (experiments)
    if let (Ok(_), Some(s)) = (std::env::var("DC_NOTEX"), scene.as_mut()) {
        for p in s.polys.iter_mut() { p.isp &= !(1 << 25); p.pcw &= !8; }
    }
    // DC_FLIP=1: swap cull modes 2 and 3 (experiments)
    if let (Ok(_), Some(s)) = (std::env::var("DC_FLIP"), scene.as_mut()) {
        for p in s.polys.iter_mut() {
            let c = p.isp >> 27 & 3;
            if c >= 2 { p.isp ^= 1 << 27; }
        }
    }
    if std::env::var("DC_LIST").is_ok() {
        if let Some(s) = d.scene.as_ref() {
            for (n, p) in s.polys.iter().enumerate() {
                let vs = &s.verts[p.first as usize..(p.first + p.count) as usize];
                let (mut x0, mut y0, mut x1, mut y1, mut z0, mut z1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN, f32::MAX, f32::MIN);
                for v in vs { x0 = x0.min(v.x); x1 = x1.max(v.x); y0 = y0.min(v.y); y1 = y1.max(v.y); z0 = z0.min(v.z); z1 = z1.max(v.z); }
                println!("{n:5} list {} n {:3} isp {:08X} tsp {:08X} tcw {:08X} pcw {:08X} x {x0:.0}-{x1:.0} y {y0:.0}-{y1:.0} 1/w {z0:.4}-{z1:.4} base {:?}",
                         p.list, p.count, p.isp, p.tsp, p.tcw, p.pcw, vs.first().map(|v| v.base).unwrap_or_default());
            }
            println!("{} modifier triangles", s.mods.len());
        }
    }
    // DC_PROBE=x,y: every triangle covering that pixel centre, with its 1/w there (f64)
    if let (Ok(pr), Some(s)) = (std::env::var("DC_PROBE"), d.scene.as_ref()) {
        let (px, py) = pr.split_once(',').unwrap();
        let (px, py): (f64, f64) = (px.parse::<f64>().unwrap() + 0.5, py.parse::<f64>().unwrap() + 0.5);
        for (n, p) in s.polys.iter().enumerate() {
            let vs = &s.verts[p.first as usize..(p.first + p.count) as usize];
            for k in 0..vs.len().saturating_sub(2) {
                let t = [&vs[k], &vs[k + 1], &vs[k + 2]];
                let (x0, y0, x1, y1, x2, y2) = (t[0].x as f64, t[0].y as f64, t[1].x as f64, t[1].y as f64, t[2].x as f64, t[2].y as f64);
                let a = (x1 - x0) * (y2 - y0) - (y1 - y0) * (x2 - x0);
                if a == 0.0 { continue; }
                let l1 = ((px - x0) * (y2 - y0) - (py - y0) * (x2 - x0)) / a;
                let l2 = ((x1 - x0) * (py - y0) - (y1 - y0) * (px - x0)) / a;
                let l0 = 1.0 - l1 - l2;
                if l0 >= 0.0 && l1 >= 0.0 && l2 >= 0.0 {
                    let z = l0 * t[0].z as f64 + l1 * t[1].z as f64 + l2 * t[2].z as f64;
                    println!("probe: poly {n} tri {k} list {} isp {:08X} tsp {:08X} 1/w {z:.6} area {a:.0}", p.list, p.isp, p.tsp);
                }
            }
        }
    }
    let out = render::draw(&d.regs, scene.as_ref(), &d.vram, &TexCache::default());
    let (col, w, h) = out.pixels();
    let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
    for &c in col { ppm.extend([(c >> 16) as u8, (c >> 8) as u8, c as u8]); }
    let o = std::env::var("DC_OUT").unwrap_or_else(|_| "frame.ppm".into());
    std::fs::write(&o, ppm).unwrap();
    println!("{} polygons, {w}x{h} -> {o}; stats {:?}", d.scene.as_ref().map_or(0, |s| s.polys.len()), out.stats);
    let _ = LIST_OPAQUE;
}

/// How long a frame takes to draw: DC_FRAME=dc_frame.bin [DC_N=20] [DC_THREADS=1]
/// cargo test --release --lib render_bench -- --ignored --nocapture
#[test]
#[ignore]
fn render_bench() {
    let path = std::env::var("DC_FRAME").expect("DC_FRAME=path to dc_frame.bin");
    let d = FrameDump::load(&std::fs::read(&path).unwrap()).expect("a frame dump");
    let n: usize = std::env::var("DC_N").ok().and_then(|v| v.parse().ok()).unwrap_or(20);
    let th: usize = std::env::var("DC_THREADS").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
    render::THREADS.store(th, std::sync::atomic::Ordering::Relaxed);
    let cache = TexCache::default();
    // the first fills the texture cache, as in a game from frame to frame
    let first = render::draw(&d.regs, d.scene.as_ref(), &d.vram, &cache);
    let t = std::time::Instant::now();
    for _ in 0..n { std::hint::black_box(render::draw(&d.regs, d.scene.as_ref(), &d.vram, &cache)); }
    let ms = t.elapsed().as_secs_f64() * 1000.0 / n as f64;
    println!("{path}: {ms:.2} ms a frame on {th} thread(s) (first, with textures to decode: {:.2} ms); stats {:?}", first.took.as_secs_f64() * 1000.0, first.stats);
}

/// The picture the dump's frame buffer registers show (what was on screen when the render
/// started): DC_FRAME=dc_frame.bin DC_OUT=shown.ppm
/// cargo test --release --lib frame_shown -- --ignored --nocapture
#[test]
#[ignore]
fn frame_shown() {
    let path = std::env::var("DC_FRAME").expect("DC_FRAME=path to dc_frame.bin");
    let d = FrameDump::load(&std::fs::read(&path).unwrap()).expect("a frame dump");
    let mut p = super::pvr::Pvr::new();
    p.regs = d.regs.clone();
    let mut out = vec![0u8; super::pvr::OUT_W * super::pvr::OUT_H * 4];
    p.output(&d.vram, &mut out);
    let mut ppm = format!("P6\n{} {}\n255\n", super::pvr::OUT_W, super::pvr::OUT_H).into_bytes();
    for px in out.chunks(4) { ppm.extend_from_slice(&px[..3]); }
    let o = std::env::var("DC_OUT").unwrap_or("shown.ppm".into());
    std::fs::write(&o, ppm).unwrap();
    println!("shown picture -> {o} (FB_R_SOF1 {:08X} SOF2 {:08X})", d.regs[0x50 / 4], d.regs[0x54 / 4]);
}

/// A 16-bit texture from a frame dump, three ways side by side (twiddled, linear,
/// twiddled through the 32-bit view): DC_FRAME=... DC_TEX=52C000,128,128 DC_OUT=tex.ppm
#[test]
#[ignore]
fn frame_tex() {
    let path = std::env::var("DC_FRAME").expect("DC_FRAME");
    let d = FrameDump::load(&std::fs::read(&path).unwrap()).expect("a frame dump");
    let a: Vec<u32> = std::env::var("DC_TEX").unwrap().split(',').map(|v| u32::from_str_radix(v, 16).unwrap_or(0)).collect();
    let (base, w, h) = (a[0], if a.len() > 1 { a[1].max(8) } else { 128 }, if a.len() > 2 { a[2].max(8) } else { 128 });
    let (w, h) = (if w == 0x128 { 128 } else { w }, if h == 0x128 { 128 } else { h });
    let cd = w.min(h).trailing_zeros();
    let rd64 = |i: u32| { let o = (base + 2 * i) as usize & 0x7F_FFFE; u16::from_le_bytes([d.vram[o], d.vram[o + 1]]) };
    let rd32 = |i: u32| { let o = super::pvr::map32((base + 2 * i) & 0x7F_FFFE) & 0x7F_FFFE; u16::from_le_bytes([d.vram[o], d.vram[o + 1]]) };
    let mut ppm = format!("P6\n{} {}\n255\n", w * 3, h).into_bytes();
    for y in 0..h {
        for v in 0..3 {
            for x in 0..w {
                let c = match v { 0 => rd64(super::texture::twiddle(x, y, cd)), 1 => rd64(y * w + x), _ => rd32(super::texture::twiddle(x, y, cd)) };
                let p = super::texture::cv_1555(c);
                ppm.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8]);
            }
        }
    }
    std::fs::write(std::env::var("DC_OUT").unwrap_or("tex.ppm".into()), ppm).unwrap();
}

/// Raw 16-bit words of a frame dump's VRAM: DC_FRAME=... DC_HEX=52C000,40 (hex address, count)
#[test]
#[ignore]
fn frame_hex() {
    let d = FrameDump::load(&std::fs::read(std::env::var("DC_FRAME").unwrap()).unwrap()).unwrap();
    let a: Vec<usize> = std::env::var("DC_HEX").unwrap().split(',').map(|v| usize::from_str_radix(v, 16).unwrap()).collect();
    for row in 0..a[1].div_ceil(16) {
        let o = a[0] + row * 32;
        println!("{o:06X}: {}", (0..16).map(|k| format!("{:04X}", u16::from_le_bytes([d.vram[o + 2 * k], d.vram[o + 2 * k + 1]]))).collect::<Vec<_>>().join(" "));
    }
}
