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
