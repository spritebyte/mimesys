use super::cart::{Cart, Map};
use super::snes_system::SnesSystem;
use crate::common::system::System;

const TEST_ROM: &[u8] = include_bytes!("test_rom/test.sfc");

fn px(s: &SnesSystem, x: usize, y: usize) -> [u8; 3] {
    let i = (y * 256 + x) * 4;
    [s.bus.framebuffer[i], s.bus.framebuffer[i + 1], s.bus.framebuffer[i + 2]]
}

/// snes/test_rom/test.s (ca65): waits for the APU's AA BB, uploads tiles by hand and by
/// DMA, a BG1 tile, a sprite, an HDMA backdrop gradient, NMI + auto joypad.
#[test]
fn test_rom_draws_bg_sprite_and_hdma() {
    let mut s = SnesSystem::from_rom(TEST_ROM, "test").unwrap();
    assert_eq!(s.bus.cart.map, Map::LoRom);
    for _ in 0..10 { s.run_frame(0); }
    let frames = u16::from_le_bytes([s.bus.wram[0], s.bus.wram[1]]);
    assert!((8..=10).contains(&frames), "one NMI a frame: {frames}");
    // BG1 tile 1 at map (5, 5) = pixels 40-47 x, screen rows 39-46 (row = BG line - 1)
    let red = [255, 0, 0];
    assert_eq!(px(&s, 40, 39), red, "BG tile");
    assert_eq!(px(&s, 47, 46), red);
    // sprite at (100, 50), 8x8, green
    assert_eq!(px(&s, 100, 50), [0, 255, 0], "sprite");
    assert_eq!(px(&s, 107, 57), [0, 255, 0]);
    // HDMA backdrop: blue for the first 100 lines, then black
    assert_eq!(px(&s, 200, 10), [0, 0, 255], "HDMA blue backdrop");
    assert_eq!(px(&s, 200, 150), [0, 0, 0], "then black");
}

#[test]
fn auto_joypad_read() {
    use crate::common::input::{A, START};
    let mut s = SnesSystem::from_rom(TEST_ROM, "test").unwrap();
    for _ in 0..5 { s.run_frame(0); }
    s.run_frame(A | START);
    s.run_frame(A | START);
    let j = u16::from_le_bytes([s.bus.wram[2], s.bus.wram[3]]);
    assert_eq!(j, 0x1080, "JOY1: Start (bit 12) + A (bit 7)");
}

#[test]
fn header_detection_lorom_hirom() {
    // HiROM: header at 00FFC0 with map mode 21 and a good checksum pair
    let mut rom = vec![0u8; 0x20000];
    rom[0xFFC0..0xFFD5].copy_from_slice(b"HIROM TEST           ");
    rom[0xFFD5] = 0x21;
    rom[0xFFDC..0xFFE0].copy_from_slice(&[0x00, 0x00, 0xFF, 0xFF]);
    rom[0xFFFC] = 0x00; rom[0xFFFD] = 0x80;
    rom[0x1_2345] = 0xAB;
    let c = Cart::new(&rom).unwrap();
    assert_eq!(c.map, Map::HiRom);
    assert_eq!(c.read(0xC1_2345), Some(0xAB), "C1:2345 = ROM 012345");
    assert_eq!(c.read(0x01_A345), Some(rom[0x1_A345]), "01:A345 mirrors the upper half");
    // a copier header is skipped
    let mut with_hdr = vec![0u8; 512];
    with_hdr.extend(TEST_ROM);
    let c = Cart::new(&with_hdr).unwrap();
    assert_eq!((c.map, c.title.as_str()), (Map::LoRom, "MIMESYS SNES TEST"));
}

#[test]
fn save_state_round_trip() {
    let mut s = SnesSystem::from_rom(TEST_ROM, "test").unwrap();
    for _ in 0..3 { s.run_frame(0); }
    let st = s.save_state();
    for _ in 0..3 { s.run_frame(0); }
    let a = s.bus.wram[0];
    s.load_state(&st).unwrap();
    for _ in 0..3 { s.run_frame(0); }
    assert_eq!(s.bus.wram[0], a);
}

#[test]
fn detection() {
    let g = crate::common::rom_detect::detect(TEST_ROM, "game.bin");
    assert_eq!(g[0].system, "snes", "{g:?}");
    assert!(g[0].score >= 80, "{g:?}");
}

/// Local only: SNES_ROM_DIR=dir runs every .sfc for 120 frames and writes .ppm shots.
#[test]
#[ignore]
fn zz_shots() {
    let dir = std::env::var("SNES_ROM_DIR").unwrap();
    let out = std::env::var("SNES_SHOT_DIR").unwrap();
    let mut files = vec![];
    fn walk(p: &std::path::Path, v: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(p).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() { walk(&p, v); } else if p.extension().map(|x| x == "sfc").unwrap_or(false) { v.push(p); }
        }
    }
    walk(std::path::Path::new(&dir), &mut files);
    files.sort();
    for f in files {
        let rom = std::fs::read(&f).unwrap();
        let name = f.file_stem().unwrap().to_string_lossy().to_string();
        let r = std::panic::catch_unwind(move || {
            let mut s = SnesSystem::from_rom(&rom, "t").unwrap();
            for _ in 0..120 { s.run_frame(0); }
            s.bus.framebuffer.clone()
        });
        match r {
            Ok(fb) => {
                let mut ppm = b"P6 256 224 255\n".to_vec();
                for p in fb.chunks(4) { ppm.extend(&p[..3]); }
                std::fs::write(format!("{out}/{name}.ppm"), ppm).unwrap();
            }
            Err(_) => eprintln!("PANIC {name}"),
        }
    }
}

/// Local only: SNES_ROM=file, prints where the CPUs are after some frames.
#[test]
#[ignore]
fn zz_where() {
    let rom = std::fs::read(std::env::var("SNES_ROM").unwrap()).unwrap();
    let mut s = SnesSystem::from_rom(&rom, "t").unwrap();
    for f in 0..600 {
        s.run_frame(if f % 60 < 5 && f > 200 { crate::common::input::START } else { 0 });
        if f % 100 == 99 {
            println!("frame {f}: cpu pc {:06X} smp pc {:04X} ports to_cpu {:02X?} from_cpu {:02X?} kon {:02X} flg {:02X}",
                     s.cpu.pc, s.bus.apu.cpu.pc, s.bus.apu.mem.to_cpu, s.bus.apu.mem.from_cpu,
                     s.bus.apu.mem.dsp_regs[0x4C], s.bus.apu.mem.dsp_regs[0x6C]);
        }
    }
    let mut ppm = b"P6 256 224 255\n".to_vec();
    for p in s.bus.framebuffer.chunks(4) { ppm.extend(&p[..3]); }
    std::fs::write(std::env::var("SNES_SHOT").unwrap_or("/dev/null".into()), ppm).unwrap();
}

/// The boot ROM's upload protocol, driven from the CPU side as a game does: one block
/// to 0200, then "jump to 0200". The uploaded code puts 5A on port 2 and uses a timer.
#[test]
fn smp_ipl_upload_and_run() {
    use crate::snes::smp::Smp;
    let mut s = Smp::new();
    let mut wait_for = |s: &mut Smp, port: usize, v: u8| {
        for _ in 0..100_000 { if s.cpu_read(port) == v { return; } s.run(20); }
        panic!("port {port} never became {v:02X} (smp pc {:04X})", s.cpu.pc);
    };
    wait_for(&mut s, 0, 0xAA);
    wait_for(&mut s, 1, 0xBB);
    // mov $F4.. : mov a,#$5A ; mov $F6,a ; mov $FA,#2 ; mov $F1,#1 ;
    // wait: mov a,$FD ; beq wait ; mov $F7,a ; stop
    let code = [0xE8, 0x5A, 0xC4, 0xF6, 0x8F, 0x02, 0xFA, 0x8F, 0x01, 0xF1, 0xE4, 0xFD, 0xF0, 0xFC, 0xC4, 0xF7, 0xFF];
    s.cpu_write(2, 0x00); s.cpu_write(3, 0x02); s.cpu_write(1, 1); s.cpu_write(0, 0xCC);
    wait_for(&mut s, 0, 0xCC);
    for (i, &b) in code.iter().enumerate() {
        s.cpu_write(1, b); s.cpu_write(0, i as u8);
        wait_for(&mut s, 0, i as u8);
    }
    let next = (code.len() as u8).wrapping_add(1);
    s.cpu_write(2, 0x00); s.cpu_write(3, 0x02); s.cpu_write(1, 0); s.cpu_write(0, next);
    wait_for(&mut s, 0, next);
    wait_for(&mut s, 2, 0x5A);
    wait_for(&mut s, 3, 1);          // timer 0 ticked (8 kHz, target 2)
    assert_eq!(&s.mem.ram[0x200..0x200 + code.len()], &code);
}
