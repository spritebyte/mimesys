use super::cart::{Cart, Map};
use super::snes_system::SnesSystem;
use crate::common::system::System;

const TEST_ROM: &[u8] = include_bytes!("test_rom/test.sfc");

fn px(s: &SnesSystem, x: usize, y: usize) -> [u8; 3] {
    let i = (y * 512 + x * 2 + 1) * 4;
    [s.bus.framebuffer[i], s.bus.framebuffer[i + 1], s.bus.framebuffer[i + 2]]
}

/// snes/test_rom/test.s (ca65): waits for the APU's AA BB, uploads tiles by hand and by
/// DMA, a BG1 tile, a sprite, an HDMA backdrop gradient, NMI + auto joypad.
#[test]
fn test_rom_draws_bg_sprite_and_hdma() {
    let mut s = SnesSystem::from_rom(TEST_ROM, "test", &[]).unwrap();
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
    let mut s = SnesSystem::from_rom(TEST_ROM, "test", &[]).unwrap();
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
    let mut s = SnesSystem::from_rom(TEST_ROM, "test", &[]).unwrap();
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
            let mut s = SnesSystem::from_rom(&rom, "t", &[]).unwrap();
            for _ in 0..120 { s.run_frame(0); }
            s.bus.framebuffer.clone()
        });
        match r {
            Ok(fb) => {
                let mut ppm = b"P6 512 224 255\n".to_vec();
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
    let mut s = SnesSystem::from_rom(&rom, "t", &[]).unwrap();
    for f in 0..600 {
        s.run_frame(if f % 60 < 5 && f > 200 { crate::common::input::START } else { 0 });
        if f % 100 == 99 {
            println!("frame {f}: cpu pc {:06X} smp pc {:04X} ports to_cpu {:02X?} from_cpu {:02X?} kon {:02X} flg {:02X}",
                     s.cpu.pc, s.bus.apu.cpu.pc, s.bus.apu.mem.to_cpu, s.bus.apu.mem.from_cpu,
                     s.bus.apu.mem.dsp.regs[0x4C], s.bus.apu.mem.dsp.regs[0x6C]);
        }
    }
    let mut ppm = b"P6 512 224 255\n".to_vec();
    for p in s.bus.framebuffer.chunks(4) { ppm.extend(&p[..3]); }
    std::fs::write(std::env::var("SNES_SHOT").unwrap_or("/dev/null".into()), ppm).unwrap();
}

/// The boot ROM's upload protocol, driven from the CPU side as a game does: one block
/// to 0200, then "jump to 0200". The uploaded code puts 5A on port 2 and uses a timer.
#[test]
fn smp_ipl_upload_and_run() {
    use crate::snes::smp::Smp;
    let mut s = Smp::new();
    let wait_for = |s: &mut Smp, port: usize, v: u8| {
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

/// Local only: SNES_ROM=file SNES_WAV=out.wav [SNES_FRAMES=n] records the sound.
#[test]
#[ignore]
fn zz_wav() {
    let rom = std::fs::read(std::env::var("SNES_ROM").unwrap()).unwrap();
    let frames: usize = std::env::var("SNES_FRAMES").ok().and_then(|f| f.parse().ok()).unwrap_or(600);
    let mut s = SnesSystem::from_rom(&rom, "t", &[]).unwrap();
    let mut pcm: Vec<i16> = Vec::new();
    for _ in 0..frames {
        s.run_frame(0);
        pcm.extend(s.drain_audio().iter().map(|&x| (x * 32767.0) as i16));
    }
    let mut w = Vec::new();
    let data = pcm.len() as u32 * 2;
    w.extend(b"RIFF"); w.extend((36 + data).to_le_bytes()); w.extend(b"WAVEfmt ");
    w.extend(16u32.to_le_bytes()); w.extend(1u16.to_le_bytes()); w.extend(2u16.to_le_bytes());
    w.extend(44100u32.to_le_bytes()); w.extend((44100u32 * 4).to_le_bytes()); w.extend(4u16.to_le_bytes()); w.extend(16u16.to_le_bytes());
    w.extend(b"data"); w.extend(data.to_le_bytes());
    for p in pcm { w.extend(p.to_le_bytes()); }
    std::fs::write(std::env::var("SNES_WAV").unwrap(), w).unwrap();
}

/// Local only: SNES_SPC=file.spc SNES_RAW=out.raw [SNES_SAMPLES=n] renders an .spc.
#[test]
#[ignore]
fn zz_spc() {
    let data = std::fs::read(std::env::var("SNES_SPC").unwrap()).unwrap();
    let n: usize = std::env::var("SNES_SAMPLES").ok().and_then(|f| f.parse().ok()).unwrap_or(320_400);
    let mut s = crate::snes::smp::Smp::load_spc(&data).unwrap();
    s.mem.dsp.mute = std::env::var("SNES_MUTE").ok().and_then(|f| f.parse().ok()).unwrap_or(0);
    let pcm = s.run_samples(n);
    let bytes: Vec<u8> = pcm.iter().flat_map(|v| v.to_le_bytes()).collect();
    std::fs::write(std::env::var("SNES_RAW").unwrap(), bytes).unwrap();
}

/// A tiny .spc: voice 0 plays a 9-byte looping BRR block (`dc`: a constant level) with
/// direct GAIN 7F; the SPC700 program keys it on, then runs `extra`, then loops.
fn test_spc(pitch: u16, dc: bool, extra: &[u8]) -> Vec<u8> {
    let mut f = vec![0u8; 0x10200];
    f[..33].copy_from_slice(b"SNES-SPC700 Sound File Data v0.30");
    f[33] = 0x1A; f[34] = 0x1A;
    f[0x25] = 0x00; f[0x26] = 0x04; f[0x2B] = 0xEF;
    let ram = &mut f[0x100..0x10100];
    ram[0x200..0x204].copy_from_slice(&[0x00, 0x03, 0x00, 0x03]);
    let brr: [u8; 9] = if dc { [0xC3, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77] }
                       else { [0xC3, 0x77, 0x77, 0x12, 0x34, 0x99, 0x99, 0xAB, 0xCD] };
    ram[0x300..0x309].copy_from_slice(&brr);
    let mut prog = vec![0x8F, 0x4C, 0xF2, 0x8F, 0x01, 0xF3];
    prog.extend_from_slice(extra);
    prog.extend_from_slice(&[0x2F, 0xFE]);
    ram[0x400..0x400 + prog.len()].copy_from_slice(&prog);
    let d = &mut f[0x10100..0x10180];
    d[0x00] = 0x7F; d[0x01] = 0x40; d[0x02] = pitch as u8; d[0x03] = (pitch >> 8) as u8; d[0x07] = 0x7F;
    d[0x0C] = 0x7F; d[0x1C] = 0x7F; d[0x5D] = 0x02; d[0x6C] = 0x20; d[0x6D] = 0xF0;
    f
}

/// Left channel from the first non-zero sample on.
fn spc_from_onset(spc: &[u8], n: usize) -> (Vec<i32>, Vec<i32>) {
    let mut s = crate::snes::smp::Smp::load_spc(spc).unwrap();
    let pcm = s.run_samples(n);
    let l: Vec<i32> = pcm.iter().step_by(2).map(|&v| v as i32).collect();
    let r: Vec<i32> = pcm.iter().skip(1).step_by(2).map(|&v| v as i32).collect();
    let on = l.iter().position(|&v| v != 0).expect("silent");
    (l[on..].to_vec(), r[on..].to_vec())
}

/// within 4: the reference rounds master and echo volume in one step (fast DSP), the
/// hardware (and we) round each, which can differ by a few LSB
fn close(got: &[i32], want: &[i32], what: &str) {
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        assert!((g - w).abs() <= 4, "{what}: sample {i}: {g} want {w}\n got {:?}\nwant {want:?}", &got[..want.len()]);
    }
}

/// BRR decode (filter 0, shift 12), Gaussian interpolation at an odd pitch, envelope,
/// volume. Reference: blargg's DSP (as in libgme) on the same .spc, values only.
#[test]
fn dsp_voice_matches_reference() {
    let (l, _) = spc_from_onset(&test_spc(0x0B3D, false, &[]), 200);
    let want = [28018, 28018, 27618, 22288, 11304, 6779, 8845, 11671, 12136, -716, -20550, -27727, -28006,
                -28003, -27563, -25777, -23050, -20230, -17428, -14229, -4394, 15013, 26578, 28018, 28018,
                27102, 20001, 9464, 6956, 9474, 12267, 10752];
    close(&l, &want, "voice");
}

/// Echo: EON, EDL 1 (512 samples), feedback and a 4-tap FIR; the first echo comes
/// back 516 samples after the note starts, then feedback drives it into clipping.
#[test]
fn dsp_echo_matches_reference() {
    let mut extra = Vec::new();
    for (r, v) in [(0x4D, 1), (0x2C, 0x60), (0x3C, 0x50), (0x0D, 0x40), (0x7D, 1),
                   (0x0F, 0x40), (0x1F, 0x20), (0x2F, 0x10), (0x3F, 0xF0), (0x6C, 0)] {
        extra.extend_from_slice(&[0x8F, r, 0xF2, 0x8F, v, 0xF3]);
    }
    let (l, r) = spc_from_onset(&test_spc(0x1000, true, &extra), 1200);
    close(&l[512..], &[28018, 28018, 28018, 28018, 25370, 28018, 32767, 32767, 32767, 32767], "echo L");
    close(&r[512..], &[14119, 14119, 14119, 14119, 13007, 14119, 16342, 20789, 20789, 20789], "echo R");
}

/// Local only: SNES_ROM=file prints PPU state after 120 frames.
#[test]
#[ignore]
fn zz_ppu() {
    let rom = std::fs::read(std::env::var("SNES_ROM").unwrap()).unwrap();
    let mut s = SnesSystem::from_rom(&rom, "t", &[]).unwrap();
    for _ in 0..120 { s.run_frame(0); }
    let p = &s.bus.ppu;
    println!("inidisp {:02X} mode {:02X} tm {:02X} ts {:02X} tmw {:02X} tsw {:02X} cgwsel {:02X} cgadsub {:02X} coldata {:04X} cg0 {:04X} setini {:02X} win {:02X?} hdmaen {:02X}",
             p.inidisp, p.bgmode, p.tm, p.ts, p.tmw, p.tsw, p.cgwsel, p.cgadsub, p.coldata, p.cgram[0], p.setini, p.win, s.bus.hdmaen);
    for c in 0..1 { let d = &s.bus.dma[c]; println!("dma{c}: {:?}", d); }
    println!("wram {:02X?}", &s.bus.wram[..16]);
    let fb = &s.bus.framebuffer; println!("px row 5 {:?} row 100 {:?}", &fb[(5*512+1)*4..(5*512+1)*4+3], &fb[(100*512+1)*4..(100*512+1)*4+3]);
}

// ---------------- PPU stage 2: driven straight through the registers ----------------

use super::ppu::{Ppu, WIDTH};

fn ppu_on() -> Ppu {
    let mut p = Ppu::new();
    p.write(0x00, 0x0F, false);            // display on, full brightness
    p
}

fn set_color(p: &mut Ppu, i: u8, c: u16) {
    p.write(0x21, i, false);
    p.write(0x22, c as u8, false);
    p.write(0x22, (c >> 8) as u8, false);
}

/// RGB of output column 2x+1 (the main-screen pixel) of row 0.
fn line_px(p: &mut Ppu, x: usize) -> [u8; 3] {
    let mut fb = vec![0u8; WIDTH * 224 * 4];
    p.render_line(0, &mut fb);
    let i = (x * 2 + 1) * 4;
    [fb[i], fb[i + 1], fb[i + 2]]
}

fn rgb(c: u16) -> [u8; 3] {
    let e = |v: u16| ((v << 3) | (v >> 2)) as u8;
    [e(c & 31), e(c >> 5 & 31), e(c >> 10 & 31)]
}

#[test]
fn ppu_color_math_add_sub_half() {
    let mut p = ppu_on();
    set_color(&mut p, 0, 10 | 20 << 5 | 30 << 10);        // backdrop
    p.write(0x32, 0x20 | 8, false);                        // fixed colour R = 8
    p.write(0x32, 0x40 | 15, false);                       //             G = 15
    p.write(0x32, 0x80 | 4, false);                        //             B = 4
    p.write(0x31, 0x20, false);                            // add, backdrop
    assert_eq!(line_px(&mut p, 50), rgb(18 | 31 << 5 | 31 << 10), "add clamps at 31");
    p.write(0x31, 0x20 | 0x40, false);                     // add, half
    assert_eq!(line_px(&mut p, 50), rgb(9 | 17 << 5 | 17 << 10), "add then halve");
    p.write(0x31, 0x20 | 0x80, false);                     // subtract
    assert_eq!(line_px(&mut p, 50), rgb(2 | 5 << 5 | 26 << 10), "subtract");
    p.write(0x31, 0x00, false);                            // math off for the backdrop
    assert_eq!(line_px(&mut p, 50), rgb(10 | 20 << 5 | 30 << 10));
}

#[test]
fn ppu_color_window_clips_to_black() {
    let mut p = ppu_on();
    set_color(&mut p, 0, 0x7FFF);
    p.write(0x26, 10, false);                              // window 1: 10..20
    p.write(0x27, 20, false);
    p.write(0x25, 0x20, false);                            // colour window uses W1
    p.write(0x30, 0x80, false);                            // clip to black inside
    assert_eq!(line_px(&mut p, 15), [0, 0, 0], "inside");
    assert_eq!(line_px(&mut p, 9), [255, 255, 255], "left of it");
    assert_eq!(line_px(&mut p, 21), [255, 255, 255], "right of it");
    p.write(0x25, 0x30, false);                            // W1 inverted
    assert_eq!(line_px(&mut p, 15), [255, 255, 255]);
    assert_eq!(line_px(&mut p, 5), [0, 0, 0]);
}

#[test]
fn ppu_sprite_range_limit_sets_flag() {
    let mut p = ppu_on();
    p.write(0x2C, 0x10, false);                            // OBJ on the main screen
    p.write(0x00, 0x8F, false);                            // forced blank to write OAM
    p.write(0x02, 0, false);
    p.write(0x03, 0, false);
    for i in 0..128u8 {
        // 33 sprites on line 0 (Y = 0), the rest off it
        let y = if i < 33 { 0 } else { 240 };
        for b in [i, y, 0, 0] { p.write(0x04, b, false); }
    }
    p.write(0x00, 0x0F, false);
    p.frame_start();
    line_px(&mut p, 0);
    assert_eq!(p.read(0x3E, false).unwrap() & 0x40, 0x40, "range over: 33 sprites on a line");
    p.frame_start();
    assert_eq!(p.read(0x3E, false).unwrap() & 0xC0, 0, "cleared at the end of vblank");
}

/// Local only: SNES_ROM=file runs 120 frames with the debug report on.
#[test]
#[ignore]
fn zz_debug_report() {
    let rom = std::fs::read(std::env::var("SNES_ROM").unwrap()).unwrap();
    let mut s = SnesSystem::from_rom(&rom, "t", &[]).unwrap();
    s.set_option("debug_report", 1);
    for i in 0..120 {
        let (c0, f0) = (s.bus.clocks, s.bus.frame);
        s.run_frame(0);
        if i < 12 { println!("run {i}: frames {}->{} clocks +{} pc {:06X} gsu running {}", f0, s.bus.frame, s.bus.clocks - c0, s.cpu.pc, s.bus.gsu.as_ref().map_or(false, |g| g.running())); }
    }
    println!("after 120 run_frame: bus.frame {} v {} clocks {} cpu pc {:06X} wai {} stp {}", s.bus.frame, s.bus.v, s.bus.clocks, s.cpu.pc, s.cpu.wai, s.cpu.stp);
}

/// A line with too many sprite slivers sets the time-over flag but must not stop
/// sprites on later lines (Tiny Toon Adventures' title).
#[test]
fn ppu_time_over_is_per_line() {
    let mut p = ppu_on();
    p.write(0x2C, 0x10, false);
    p.write(0x01, 0x60, false);                            // sizes 16x32 / 32x32 (small 16 wide)
    p.write(0x00, 0x8F, false);
    p.write(0x02, 0, false);
    p.write(0x03, 0, false);
    // sprites 0 and 1: 16 wide on row 40; 2-21: 16 wide on row 0 (40 slivers)
    for i in 0..128u8 {
        let (x, y) = match i { 0 => (100, 40), 1 => (150, 40), 2..=21 => (i * 11, 0), _ => (0, 240) };
        for b in [x, y, 1, 0] { p.write(0x04, b, false); }
    }
    for _ in 0..32 { p.write(0x04, 0, false); }            // high table: all small, X < 256
    // tile 1: every pixel colour 1 (plane 0 set)
    p.write(0x15, 0x80, false);
    p.write(0x16, 0x10, false); p.write(0x17, 0x00, false);
    for _ in 0..8 { p.write(0x18, 0xFF, false); p.write(0x19, 0x00, false); }
    p.write(0x00, 0x0F, false);
    set_color(&mut p, 129, 0x001F);                        // sprite palette 0, colour 1: red
    let mut fb = vec![0u8; WIDTH * 224 * 4];
    p.frame_start();
    p.render_line(0, &mut fb);
    assert_ne!(p.read(0x3E, false).unwrap() & 0x80, 0, "row 0 runs out of time");
    p.render_line(40, &mut fb);
    let i = (40 * WIDTH + 100 * 2 + 1) * 4;
    assert_eq!(&fb[i..i + 3], &[255, 0, 0], "the sprite on row 40 still draws");
}

/// Two blocks through the boot ROM, the way a game's loader does it: the second block's
/// first byte is 0 and lands in port 1 before index 0 lands in port 0, while port 0 still
/// holds the "next block" value (below 80). The ROM must wait for index 0, not take the
/// 0 in port 1 for "jump" (Contra III hung here).
#[test]
fn smp_ipl_two_blocks() {
    use crate::snes::smp::Smp;
    let mut s = Smp::new();
    let wait_for = |s: &mut Smp, port: usize, v: u8| {
        for _ in 0..100_000 { if s.cpu_read(port) == v { return; } s.run(20); }
        panic!("port {port} never became {v:02X} (smp pc {:04X})", s.cpu.pc);
    };
    wait_for(&mut s, 0, 0xAA);
    wait_for(&mut s, 1, 0xBB);
    // block 1: 14 bytes at 0300 (last index 0D, so the next kick is 0F)
    let b1: Vec<u8> = (1..=14).collect();
    s.cpu_write(2, 0x00); s.cpu_write(3, 0x03); s.cpu_write(1, 1); s.cpu_write(0, 0xCC);
    wait_for(&mut s, 0, 0xCC);
    for (i, &b) in b1.iter().enumerate() { s.cpu_write(1, b); s.cpu_write(0, i as u8); wait_for(&mut s, 0, i as u8); }
    // block 2 at 0200: mov a,#$5A ; mov $F6,a ; stop  (starts with E8, then a 0 byte later)
    let b2 = [0x00, 0xE8, 0x5A, 0xC4, 0xF6, 0xFF];
    s.cpu_write(2, 0x00); s.cpu_write(3, 0x02); s.cpu_write(1, 1); s.cpu_write(0, 0x0F);
    wait_for(&mut s, 0, 0x0F);
    for (i, &b) in b2.iter().enumerate() {
        s.cpu_write(1, b);
        s.run(3000);                                // the game does other work between the two writes
        assert!(s.cpu.pc >= 0xFFC0, "byte {i}: the boot ROM jumped away to {:04X} on the data in port 1", s.cpu.pc);
        s.cpu_write(0, i as u8);
        wait_for(&mut s, 0, i as u8);
    }
    // run from 0201 (the first byte, 00, is a NOP-free pad: jump past it)
    s.cpu_write(2, 0x01); s.cpu_write(3, 0x02); s.cpu_write(1, 0); s.cpu_write(0, 0x08);
    wait_for(&mut s, 0, 0x08);
    wait_for(&mut s, 2, 0x5A);
    assert_eq!(&s.mem.ram[0x300..0x30E], &b1[..]);
    assert_eq!(&s.mem.ram[0x200..0x206], &b2);
}

/// Super FX: a tiny GSU program plots pixel (3, 2) in colour 1 (2 bits a pixel),
/// flushes the pixel cache with RPIX and stops: the pixel lands in tile 0, row 2,
/// plane 0, bit 4; STOP raises the IRQ, reading 3031 clears it.
#[test]
fn superfx_plot_and_stop() {
    use crate::snes::superfx::{Gsu, Mem};
    let mut rom = vec![0u8; 0x8000];
    let prog = [0xF1, 0x03, 0x00,   // iwt r1,#3
                0xF2, 0x02, 0x00,   // iwt r2,#2
                0xA0, 0x01,         // ibt r0,#1
                0x4E,               // color
                0x4C,               // plot          (r1 -> 4)
                0x3D, 0x4C,         // rpix          (flushes the pixel cache)
                0x00, 0x01];        // stop ; nop
    rom[..prog.len()].copy_from_slice(&prog);
    let mut ram = vec![0u8; 0x10000];
    let mut g = Gsu::new();
    g.write_io(0x303A, 0x18);       // RON, RAN, 2 bits a pixel, 128 high
    g.write_io(0x3034, 0x00);       // program bank 0
    g.write_io(0x301E, 0x00);       // R15 = 8000, and go
    g.write_io(0x301F, 0x80);
    assert!(g.running());
    g.run(20_000, &mut Mem { rom: &rom, ram: &mut ram });
    assert!(!g.running(), "STOP");
    assert_eq!(ram[4], 0x10, "plane 0 of row 2 of tile 0: bit 7 - 3");
    assert_eq!(ram[5], 0x00, "plane 1 untouched");
    assert!(g.irq, "STOP raises the IRQ");
    g.read_io(0x3031);
    assert!(!g.irq, "reading 3031 clears it");
}

/// SA-1: the S-CPU points the SA-1's reset vector at a small program and lets it go.
/// The program uses the multiplier, stores to I-RAM, writes a pixel through the 4-bit
/// BW-RAM bitmap view and interrupts the S-CPU.
#[test]
fn sa1_runs_and_interrupts_the_cpu() {
    use crate::snes::sa1::Sa1;
    let prog = [
        0xA9, 0xFF, 0x8D, 0x2A, 0x22,       // lda #$FF : sta $222A   I-RAM writable
        0xA9, 0x80, 0x8D, 0x27, 0x22,       // lda #$80 : sta $2227   BW-RAM writable
        0xA9, 0x03, 0x8D, 0x51, 0x22,       // MA = 3
        0x9C, 0x52, 0x22,
        0xA9, 0x07, 0x8D, 0x53, 0x22,       // MB = 7 (writing 2254 multiplies)
        0x9C, 0x54, 0x22,
        0xAD, 0x06, 0x23,                   // lda $2306              MR low
        0x8D, 0x00, 0x30,                   // sta $3000              I-RAM 0
        0xA9, 0x0A, 0x8F, 0x01, 0x00, 0x60, // lda #$0A : sta $600001 bitmap pixel 1
        0xA9, 0x80, 0x8D, 0x09, 0x22,       // lda #$80 : sta $2209   IRQ to the S-CPU
        0xDB,                               // stp
    ];
    let mut rom = vec![0u8; 0x8000];
    rom[..prog.len()].copy_from_slice(&prog);
    let mut bw = vec![0u8; 0x2000];
    let mut s = Sa1::new();
    for (a, v) in [(0x2201u32, 0x80u8), (0x2203, 0x00), (0x2204, 0x80), (0x2200, 0x00)] {
        assert!(s.cpu_write(&rom, &mut bw, a, v));
    }
    s.run(10_000, &rom, &mut bw, 0);
    assert!(s.cpu.stp, "the program ran to STP");
    assert_eq!(s.st.iram[0], 21, "3 x 7 from the multiplier");
    assert_eq!(bw[0], 0xA0, "4-bit bitmap pixel 1 = high nibble of byte 0");
    assert!(s.st.cpu_irq_line, "SCNT raised the S-CPU's IRQ");
    assert_eq!(s.cpu_read(&rom, &bw, 0x2300, 0).unwrap() & 0x80, 0x80, "SFR shows it");
    s.cpu_write(&rom, &mut bw, 0x2202, 0x80);
    assert!(!s.st.cpu_irq_line, "SIC clears it");
    // the S-CPU sees I-RAM at 3000
    assert_eq!(s.cpu_read(&rom, &bw, 0x003000, 0), Some(21));
}

/// A register written late in a line (here at H 1076, as Final Fantasy II's IRQ handler
/// does with BG3's scroll) shows from the next line, not on the line already drawn.
#[test]
fn late_register_write_lands_on_the_next_line() {
    use crate::common::cpu::w65816::Bus816;
    let mut s = SnesSystem::from_rom(TEST_ROM, "test", &[]).unwrap();
    for _ in 0..10 { s.run_frame(0); }
    let b = &mut s.bus;
    while !(b.v == 50 && b.h == 0) { b.tick(1); }
    b.tick(1028);                               // + the 40-clock DRAM refresh = H 1068
    assert_eq!(b.h, 1068);
    Bus816::write(b, 0x002100, 0x80);           // forced blank, landing at H 1074
    while b.v != 53 { b.tick(4); }
    assert_eq!(px(&s, 200, 49), [0, 0, 255], "line 50 (row 49) was drawn before the write");
    assert_eq!(px(&s, 200, 50), [0, 0, 0], "line 51 is blank");
}

/// A hand-assembled µPD7725 program (not Nintendo's firmware): sends 1234, then FF + 1,
/// then asks for a word and sends it back + 1, using DR and its RQM handshake.
fn necdsp_test_firmware() -> Vec<u8> {
    let ld = |imm: u32, dst: u32| 0xC00000 | imm << 6 | dst;
    let op = |alu: u32, src: u32, dst: u32| alu << 16 | src << 4 | dst;
    let jp = |brch: u32, na: u32| 0x800000 | brch << 13 | na << 2;
    const JRQM: u32 = 0x0BE;
    let prog = [
        ld(0x1234, 6),        // 0  DR = 1234 (RQM up: data for the CPU)
        jp(JRQM, 1),          // 1  wait until the CPU has read both bytes
        ld(0x00FF, 1),        // 2  A = FF
        op(9, 0, 0),          // 3  A = A + 1
        op(0, 1, 6),          // 4  DR = A
        jp(JRQM, 5),          // 5  wait
        op(0, 8, 0),          // 6  ask for data (RQM up)
        jp(JRQM, 7),          // 7  wait until the CPU has written both bytes
        op(0, 9, 1),          // 8  A = DR
        op(9, 0, 0),          // 9  A = A + 1
        op(0, 1, 6),          // 10 DR = A
        jp(JRQM, 11),         // 11 wait
        jp(0x100, 12),        // 12 stay
    ];
    let mut fw = vec![0u8; 0x2000];
    for (i, w) in prog.iter().enumerate() { fw[i * 3..i * 3 + 3].copy_from_slice(&w.to_le_bytes()[..3]); }
    fw
}

#[test]
fn necdsp_handshake_and_alu() {
    use crate::snes::necdsp::NecDsp;
    let mut d = NecDsp::new(&necdsp_test_firmware());
    d.run(200);
    assert_eq!(d.read(true) & 0x80, 0x80, "SR: RQM up, data ready");
    assert_eq!((d.read(false), d.read(false)), (0x34, 0x12));
    d.run(200);
    assert_eq!((d.read(false), d.read(false)), (0x00, 0x01), "FF + 1");
    d.run(200);
    assert_eq!(d.read(true) & 0x80, 0x80, "asking for data");
    d.write(false, 0xCD);
    d.write(false, 0xAB);
    d.run(200);
    assert_eq!((d.read(false), d.read(false)), (0xCE, 0xAB), "ABCD + 1");
}

/// A DSP-1 LoROM board (512 KB, battery RAM: Mario Kart's layout) with the firmware
/// appended to the ROM image: the S-CPU finds DR at 20:8000 and SR at 20:C000.
#[test]
fn necdsp_board_map_and_appended_firmware() {
    let mut rom = vec![0u8; 0x80000];
    rom[0x7FC0..0x7FD5].copy_from_slice(b"DSP BOARD TEST       ");
    rom[0x7FD5] = 0x20;                 // LoROM
    rom[0x7FD6] = 0x05;                 // ROM + RAM + battery + DSP
    rom[0x7FD8] = 0x01;                 // 2 KB RAM
    rom[0x7FDC..0x7FE0].copy_from_slice(&[0x00, 0x00, 0xFF, 0xFF]);
    rom[0x7FFC] = 0x00; rom[0x7FFD] = 0x80;
    rom.extend(necdsp_test_firmware());
    let mut s = SnesSystem::from_rom(&rom, "t", &[]).unwrap();
    assert_eq!(s.bus.cart.rom.len(), 0x80000, "the firmware isn't part of the ROM");
    assert!(s.bus.nec.is_some());
    use crate::common::cpu::w65816::Bus816;
    s.bus.tick(2000);
    assert_eq!(Bus816::read(&mut s.bus, 0x20C000) & 0x80, 0x80, "SR");
    assert_eq!(Bus816::read(&mut s.bus, 0x208000), 0x34, "DR low");
    assert_eq!(Bus816::read(&mut s.bus, 0xA0BFFF), 0x12, "DR high, through the A0-BF mirror");
}

#[test]
fn necdsp_firmware_bundle() {
    use crate::snes::snes_system::find_firmware;
    let mut b = b"SNFW".to_vec();
    for (name, data) in [("dsp1b.program.rom", vec![1u8; 0x1800]), ("dsp1b.data.rom", vec![2u8; 0x800]), ("dsp2.rom", vec![3u8; 0x2000])] {
        b.push(name.len() as u8);
        b.extend(name.as_bytes());
        b.extend((data.len() as u32).to_le_bytes());
        b.extend(data);
    }
    let f = find_firmware(&b, "dsp1b").unwrap();
    assert_eq!((f.len(), f[0], f[0x1800]), (0x2000, 1, 2), "split files joined");
    assert_eq!(find_firmware(&b, "dsp2").unwrap()[0], 3);
    assert!(find_firmware(&b, "dsp4").is_none());
}

/// Local only: how long 600 frames of a DSP board take (the DSP never sleeps).
#[test]
#[ignore]
fn zz_necdsp_speed() {
    let mut rom = vec![0u8; 0x80000];
    rom[0x7FC0..0x7FD5].copy_from_slice(b"DSP BOARD TEST       ");
    rom[0x7FD5] = 0x20; rom[0x7FD6] = 0x05; rom[0x7FD8] = 0x01;
    rom[0x7FDC..0x7FE0].copy_from_slice(&[0x00, 0x00, 0xFF, 0xFF]);
    rom[0x7FFC] = 0x00; rom[0x7FFD] = 0x80;
    rom[0] = 0x80; rom[1] = 0xFE;                 // bra *
    rom.extend(necdsp_test_firmware());
    let mut s = SnesSystem::from_rom(&rom, "t", &[]).unwrap();
    let t = std::time::Instant::now();
    for _ in 0..600 { s.run_frame(0); }
    println!("600 frames with a DSP: {:?}", t.elapsed());
}

// ---------------- Cx4 ----------------

/// A Cx4 data ROM where word n = n * 0x010101 + 0x000102 (and word 5 = 123456).
fn cx4_test_firmware() -> Vec<u8> {
    let mut fw = Vec::new();
    for n in 0..1024u32 {
        let w = if n == 5 { 0x123456 } else { (n * 0x010101 + 0x000102) & 0xFFFFFF };
        fw.extend_from_slice(&w.to_le_bytes()[..3]);
    }
    fw
}

/// A hand-assembled HG51B program at ROM 0 (00:8000): registers, the data ROM, the
/// multiplier, data RAM, a subroutine, the far jump into the second cache page, HALT.
fn cx4_test_program() -> Vec<u16> {
    vec![
        0x6412,         // 0  LD A,#12
        0xDC08,         // 1  SHL A,8
        0xBC34,         // 2  OR A,#34              A = 1234
        0xE060,         // 3  ST R0,A
        0x7405,         // 4  RDROM #5
        0x6008,         // 5  LD A,ROM
        0xE061,         // 6  ST R1,A               R1 = data ROM word 5
        0x6051,         // 7  LD A,#FFFFFF (constant register 51) = -1
        0x9C05,         // 8  MUL #5                -5
        0x6002,         // 9  LD A,MUL low
        0xE062,         // 10 ST R2,A               R2 = FFFFFB
        0x6456,         // 11 LD A,#56
        0xE00C,         // 12 ST RAM,A
        0xEC10,         // 13 WRRAM 0,#10           data RAM 010 = 56
        0x2810,         // 14 JSR 16
        0x0A00,         // 15 JMP far 00 (P = 1: the next 512 bytes)
        0x6477,         // 16 LD A,#77
        0xE063,         // 17 ST R3,A
        0x5408,         // 18 CMP A,#08             77 - 08: carry set (no borrow)
        0x2501,         // 19 SKIP C (when set)
        0x6400,         //    (skipped)
        0xE064,         // 21 ST R4,A               R4 = 77
        0x7D00,         // 22 LD PH,#00
        0x7C01,         // 23 LD PL,#01             P = 1
        0x3C00,         // 24 RTS
    ]
}

fn cx4_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x80000];
    for (i, w) in cx4_test_program().iter().enumerate() { rom[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes()); }
    // bank 1 of the program (ROM 0200): R5 = 99, HALT
    for (i, w) in [0x6499u16, 0xE065, 0xFC00].iter().enumerate() { rom[0x200 + i * 2..0x202 + i * 2].copy_from_slice(&w.to_le_bytes()); }
    rom[0x300..0x304].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
    rom
}

#[test]
fn cx4_runs_a_program_and_interrupts() {
    use crate::snes::cx4::{Cx4, CpuRead, Mem};
    let rom = cx4_rom();
    let mut ram = vec![];
    let mut c = Cx4::new(&cx4_test_firmware());
    let rd = |c: &mut Cx4, a: u32| match c.cpu_read(a) { CpuRead::Value(v) => v, _ => panic!("{a:06X} isn't the Cx4's") };
    for (a, v) in [(0x7F49, 0x00), (0x7F4A, 0x80), (0x7F4B, 0x00), (0x7F4D, 0), (0x7F4E, 0)] { assert!(c.cpu_write(a, v)); }
    c.cpu_write(0x7F4F, 0x00);              // go
    assert_eq!(rd(&mut c, 0x7F5E) & 0x40, 0x40, "running");
    c.run(100_000, &mut Mem { rom: &rom, ram: &mut ram });
    let reg = |c: &mut Cx4, n: u32| (0..3).map(|k| (rd(c, 0x7F80 + n * 3 + k) as u32) << (k * 8)).sum::<u32>();
    assert_eq!(rd(&mut c, 0x7F5E) & 0x40, 0, "stopped");
    assert_eq!(reg(&mut c, 0), 0x1234);
    assert_eq!(reg(&mut c, 1), 0x123456, "data ROM");
    assert_eq!(reg(&mut c, 2), 0xFFFFFB, "-1 * 5");
    assert_eq!(rd(&mut c, 0x006010), 0x56, "data RAM, as the S-CPU sees it");
    assert_eq!(reg(&mut c, 3), 0x77, "subroutine");
    assert_eq!(reg(&mut c, 4), 0x77, "SKIP C skipped the LD");
    assert_eq!(reg(&mut c, 5), 0x99, "far jump to the next program bank");
    assert!(c.irq_line, "HALT raises the IRQ");
    assert_eq!(rd(&mut c, 0x7F5E) & 2, 2);
    c.cpu_write(0x7F51, 1);
    assert!(!c.irq_line, "acknowledged");
    // the 6C00 mirror of the registers
    assert_eq!(rd(&mut c, 0x806F80), 0x34);
}

#[test]
fn cx4_dma_and_the_vectors_while_busy() {
    use crate::snes::cx4::{Cx4, CpuRead, Mem};
    let rom = cx4_rom();
    let mut ram = vec![];
    let mut c = Cx4::new(&cx4_test_firmware());
    // DMA 4 bytes from ROM 0300 (00:8300) to data RAM 0020
    for (a, v) in [(0x7F40, 0x00), (0x7F41, 0x83), (0x7F42, 0x00), (0x7F43, 4), (0x7F44, 0), (0x7F45, 0x20), (0x7F46, 0x60)] { c.cpu_write(a, v); }
    c.cpu_write(0x7F6A, 0x34);               // the NMI vector the S-CPU sees while the chip is busy
    c.cpu_write(0x7F47, 0x00);
    assert!(c.busy());
    assert!(matches!(c.cpu_read(0x00FFEA), CpuRead::Value(0x34)), "vector from 7F6A");
    assert!(matches!(c.cpu_read(0x008000), CpuRead::OpenBus), "the chip has the ROM");
    c.run(10_000, &mut Mem { rom: &rom, ram: &mut ram });
    assert!(!c.busy());
    assert!(matches!(c.cpu_read(0x00FFEA), CpuRead::Cart));
    let got: Vec<u8> = (0..4).map(|i| match c.cpu_read(0x6020 + i) { CpuRead::Value(v) => v, _ => 0 }).collect();
    assert_eq!(got, [0xDE, 0xAD, 0xBE, 0xEF]);
}

/// A HITACHI-LOROM header (chipset F3, subtype 10) with the data ROM appended.
#[test]
fn cx4_board_and_appended_firmware() {
    let mut rom = cx4_rom();
    rom[0x7FC0..0x7FD5].copy_from_slice(b"CX4 BOARD TEST       ");
    rom[0x7FD5] = 0x20;
    rom[0x7FD6] = 0xF3;
    rom[0x7FDA] = 0x33;
    rom[0x7FBF] = 0x10;
    rom[0x7FDC..0x7FE0].copy_from_slice(&[0x00, 0x00, 0xFF, 0xFF]);
    rom[0x7FFC] = 0x00; rom[0x7FFD] = 0x80;
    rom.extend(cx4_test_firmware());
    let mut s = SnesSystem::from_rom(&rom, "t", &[]).unwrap();
    assert_eq!(s.bus.cart.rom.len(), 0x80000);
    assert!(s.bus.cx4.is_some());
    use crate::common::cpu::w65816::Bus816;
    Bus816::write(&mut s.bus, 0x7F4A, 0x80); // program at 00:8000
    Bus816::write(&mut s.bus, 0x7F4F, 0);    // run it
    s.bus.tick(200_000);
    assert_eq!(Bus816::read(&mut s.bus, 0x7F80 + 15), 0x99, "R5");
    assert_eq!(Bus816::read(&mut s.bus, 0x6010), 0x56);
    assert!(s.bus.irq_line());
}

#[test]
fn cheats_patch_rom_and_write_ram() {
    let mut rom = vec![0u8; 0x80000];
    rom[0x7FC0..0x7FD5].copy_from_slice(b"CHEAT TEST           ");
    rom[0x7FD5] = 0x20;
    rom[0x7FDC..0x7FE0].copy_from_slice(&[0x00, 0x00, 0xFF, 0xFF]);
    rom[0x7FFC] = 0x00; rom[0x7FFD] = 0x80;
    rom[0x0010] = 0xA9;
    let mut s = SnesSystem::from_rom(&rom, "t", &[]).unwrap();
    use crate::common::cpu::w65816::Bus816;
    use crate::common::system::System;
    let c = s.cheats_mut().unwrap();
    let errs = c.set(&["808010:EA".to_string(), "008011?00:42".to_string(), "7E0100 55".replace(' ', ""), "7E0DBE09".to_string()]);
    assert!(errs.iter().all(|e| e.is_empty()), "{errs:?}");
    assert_eq!(Bus816::read(&mut s.bus, 0x808010), 0xEA);
    assert_eq!(Bus816::read(&mut s.bus, 0x008010), 0xEA, "FastROM mirror");
    assert_eq!(Bus816::read(&mut s.bus, 0x008011), 0x42, "compare matched");
    s.bus.cheats.set(&["008010?00:EA".to_string()]);
    assert_eq!(Bus816::read(&mut s.bus, 0x008010), 0xA9, "compare didn't match: untouched");
    s.bus.cheats.set(&["7E0DBE09".to_string(), "000100:77".to_string()]);
    s.run_frame(0);
    assert_eq!(s.bus.wram[0x0DBE], 0x09, "Pro Action Replay");
    assert_eq!(s.bus.wram[0x0100], 0x77, "low-RAM mirror");
}
