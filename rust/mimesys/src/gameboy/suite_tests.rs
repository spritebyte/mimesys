//! Runs public Game Boy test ROMs (c-sp/game-boy-test-roms) when GB_SUITE points at them:
//!   GB_SUITE=/path/to/game-boy-test-roms cargo test --release gb_suite -- --ignored --nocapture
//! Optional: GB_OUT=dir writes a .ppm screenshot of each ROM (compare with the suite's PNGs);
//! GB_BIOS_DMG / GB_BIOS_CGB = boot ROMs to run first.
//! Results: blargg by serial text, mooneye by the Fibonacci registers, others screenshot only.

use crate::common::system::System;
use crate::gameboy::gb_palette::PALETTE_POCKET_GRAY;
use crate::gameboy::gb_system::{BootConfig, GbSystem, Model};

fn run(sys: &mut GbSystem, frames: usize, serial: &mut Vec<u8>) {
    for _ in 0..frames {
        unsafe { (*sys.bus.ppu.get()).clear_slice_complete_flag() };
        while !unsafe { (*sys.bus.ppu.get()).is_slice_complete() } {
            sys.tick();
            if let Some(b) = sys.bus.serial.awaiting {
                serial.push(b);
                sys.bus.serial.finish(0xFF);
            }
        }
        sys.refresh_frame_cache();
        let c = &sys.cpu;
        if [c.b, c.c, c.d, c.e, c.h, c.l] == [3, 5, 8, 13, 21, 34] { break; }
        let text = String::from_utf8_lossy(serial);
        if text.contains("Passed") || text.contains("Failed") { break; }
    }
}

#[test]
#[ignore]
fn gb_suite() {
    let Ok(dir) = std::env::var("GB_SUITE") else { println!("GB_SUITE not set"); return; };
    let out = std::env::var("GB_OUT").ok();
    let list = std::env::var("GB_LIST").unwrap_or_else(|_| "gb_suite.txt".into());
    let model = std::env::var("GB_MODEL").unwrap_or_default();
    let bios = std::env::var("GB_BIOS").ok().map(|p| std::fs::read(p).unwrap()).unwrap_or_default();
    let names = std::fs::read_to_string(&list).expect("GB_LIST file (one ROM path per line, then frames)");
    let (mut pass, mut total) = (0, 0);
    for line in names.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')) {
        let mut parts = line.split_whitespace();
        let rel = parts.next().unwrap();
        let frames: usize = parts.next().and_then(|f| f.parse().ok()).unwrap_or(600);
        let rom = std::fs::read(format!("{dir}/{rel}")).unwrap();
        let mut cfg = BootConfig::parse(&bios);
        cfg.skip = std::env::var("GB_DIRECT").is_err();
        cfg.model = match model.as_str() { "dmg" => Some(Model::Dmg), "cgb" => Some(Model::Cgb), _ => None };
        let built = if std::env::var("GB_DIRECT").is_ok() {
            GbSystem::build_direct(&rom, "suite", cfg)
        } else {
            GbSystem::build(&rom, "suite", cfg)
        };
        let mut sys = match built {
            Ok(s) => s, Err(e) => { println!("SKIP {rel}: {e}"); continue; }
        };
        unsafe { (*sys.bus.ppu.get()).set_palette(PALETTE_POCKET_GRAY) };
        let mut serial = Vec::new();
        run(&mut sys, frames, &mut serial);
        let text = String::from_utf8_lossy(&serial).replace('\n', " ");
        let c = &sys.cpu;
        let verdict = if text.contains("Passed") || [c.b, c.c, c.d, c.e, c.h, c.l] == [3, 5, 8, 13, 21, 34] { "PASS" }
            else if text.contains("Failed") || c.b == 0x42 { "FAIL" } else { "----" };
        if verdict != "----" { total += 1; if verdict == "PASS" { pass += 1; } }
        println!("{verdict} {rel} {}", text.chars().take(120).collect::<String>());
        if let Some(o) = &out {
            let name = rel.replace('/', "__");
            let fb = sys.framebuffer();
            let mut ppm = format!("P6\n160 144\n255\n").into_bytes();
            for px in fb.chunks_exact(4) { ppm.extend_from_slice(&px[..3]); }
            std::fs::write(format!("{o}/{name}.ppm"), ppm).unwrap();
        }
    }
    println!("{pass}/{total} pass/fail tests passed");
}

/// rtc3test's three subtests (menu: A / down A / down down A), screenshots to GB_OUT.
#[test]
#[ignore]
fn gb_rtc3test() {
    use crate::common::input as pad;
    let (Ok(dir), Ok(out)) = (std::env::var("GB_SUITE"), std::env::var("GB_OUT")) else { return; };
    let rom = std::fs::read(format!("{dir}/rtc3test/rtc3test.gb")).unwrap();
    for (name, presses, secs) in [("basic", vec![pad::A], 15), ("range", vec![pad::DOWN, pad::A], 10),
                                  ("subsec", vec![pad::DOWN, pad::DOWN, pad::A], 28)] {
        let cfg = BootConfig { model: Some(Model::Dmg), skip: true, ..Default::default() };
        let mut sys = GbSystem::build(&rom, "rtc", cfg).unwrap();
        unsafe { (*sys.bus.ppu.get()).set_palette(PALETTE_POCKET_GRAY) };
        for _ in 0..60 { sys.run_frame(0); }
        for p in presses {
            for _ in 0..6 { sys.run_frame(p); }
            for _ in 0..12 { sys.run_frame(0); }
        }
        for _ in 0..secs * 60 { sys.run_frame(0); }
        let mut ppm = b"P6\n160 144\n255\n".to_vec();
        for px in sys.framebuffer().chunks_exact(4) { ppm.extend_from_slice(&px[..3]); }
        std::fs::write(format!("{out}/rtc3test-{name}.ppm"), ppm).unwrap();
    }
}
