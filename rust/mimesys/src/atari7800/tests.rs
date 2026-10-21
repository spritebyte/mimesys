use super::a7800_system::{A7800System, OUT_H};
use super::cart::{Cart7800, Low};
use super::maria::{Maria, OUT_W};
use crate::common::system::System;

const NO_KEY: u32 = 0x0F << crate::common::input::KEYPAD_SHIFT;

/// Memory for MARIA tests: 64K, anything goes.
fn mem() -> Vec<u8> { vec![0; 0x10000] }

fn maria_with(ctrl: u8) -> Maria {
    let mut m = Maria::new();
    m.regs[0x1C] = ctrl;
    m.regs[0x0C] = 0x20;     // DPPH: DLL at 0x2000
    m.regs[0x10] = 0x00;
    m
}

#[test]
fn display_list_160a_object() {
    let mut m = mem();
    // DLL: one zone of 2 lines (offset 1), DL at 0x2100
    m[0x2000..0x2003].copy_from_slice(&[0x01, 0x21, 0x00]);
    // DL: 4-byte header: graphics at 0x3000 (hi 0x30), palette 2, width 2 bytes, x = 10
    m[0x2100..0x2104].copy_from_slice(&[0x00, (2 << 5) | (0x1F & !1u8.wrapping_sub(0)) & 0xFE | 0x1E, 0x30, 10]);
    m[0x2101] = (2 << 5) | 0x1E;              // width field 0x1E = 2 bytes
    m[0x2104..0x2106].copy_from_slice(&[0x00, 0x00]);   // end of list
    // zone line offset 1 (first line): graphics from 0x3100; offset 0: 0x3000
    m[0x3100] = 0b11_10_01_00;
    m[0x3101] = 0b01_00_00_00;
    m[0x3000] = 0b00_00_00_11;
    let mut ma = maria_with(0x40);           // DMA on, 160A
    ma.regs[0x09] = 0x44;                    // P2C1
    ma.regs[0x0A] = 0x55;                    // P2C2
    ma.regs[0x0B] = 0x66;                    // P2C3
    ma.regs[0x00] = 0x11;                    // background
    let read = |a: u16| m[a as usize];
    let clocks = ma.dma_line(true, &read);
    let cells = ma.line_cells();
    assert_eq!(&cells[10..15], &[(2 << 2) | 3, (2 << 2) | 2, (2 << 2) | 1, 0, (2 << 2) | 1]);
    let mut out = [0u8; OUT_W];
    ma.output_line(&mut out);
    assert_eq!((out[20], out[21], out[22], out[26], out[28]), (0x66, 0x66, 0x55, 0x11, 0x44));
    assert!(clocks > 31, "DMA took {clocks} clocks");
    // second line of the zone reads 0x3000
    ma.dma_line(false, &read);
    assert_eq!(ma.line_cells()[13], (2 << 2) | 3);
}

#[test]
fn indirect_320a_and_dli() {
    let mut m = mem();
    // zone 0: one line, no DLI. zone 1: DLI set -> NMI when moving on to it
    m[0x2000..0x2006].copy_from_slice(&[0x00, 0x21, 0x00, 0x80, 0x21, 0x00]);
    // 5-byte header: char codes at 0x2200, write mode 0, indirect, palette 1, 1 char, x = 0
    m[0x2100..0x2105].copy_from_slice(&[0x00, 0x60, 0x22, (1 << 5) | 0x1F, 0]);
    m[0x2200] = 0x07;                        // character 7
    m[0x4007] = 0b1000_0001;                 // CHARBASE 0x40, offset 0
    let mut ma = maria_with(0x43);           // DMA on, read mode 3 (320A)
    ma.regs[0x14] = 0x40;
    ma.regs[0x06] = 0x77;                    // P1C2
    ma.regs[0x00] = 0x00;
    let read = |a: u16| m[a as usize];
    ma.dma_line(true, &read);
    assert!(ma.nmi, "next zone has DLI");
    let mut out = [0u8; OUT_W];
    ma.output_line(&mut out);
    assert_eq!(&out[0..8], &[0x77, 0, 0, 0, 0, 0, 0, 0x77]);
}

#[test]
fn holey_dma_skips_graphics() {
    let mut m = mem();
    m[0x2000..0x2003].copy_from_slice(&[0x40, 0x21, 0x00]);      // H16
    m[0x2100..0x2104].copy_from_slice(&[0x00, 0x1F, 0x90, 0]);   // graphics at 0x9000: holey
    m[0x9000] = 0xFF;
    let mut ma = maria_with(0x44);                               // kangaroo off
    let read = |a: u16| m[a as usize];
    ma.dma_line(true, &read);
    assert_eq!(&ma.line_cells()[0..4], &[0, 0, 0, 0]);
}

#[test]
fn cart_mapping() {
    let mut a78 = vec![0u8; 128];
    a78[1..10].copy_from_slice(b"ATARI7800");
    a78[17..21].copy_from_slice(b"Test");
    let rom: Vec<u8> = (0..0x8000u32).map(|i| (i >> 8) as u8).collect();
    a78[49..53].copy_from_slice(&(rom.len() as u32).to_be_bytes());
    let mut file = a78.clone();
    file.extend(&rom);
    let c = Cart7800::new(&file).unwrap();
    assert_eq!((c.read(0x8000), c.read(0xFFFF), c.read(0x7FFF)), (0x00, 0x7F, 0xFF));
    assert_eq!(c.title, "Test");
    // SuperGame 128K with RAM at 0x4000
    let rom: Vec<u8> = (0..0x20000u32).map(|i| (i >> 14) as u8).collect();
    let mut a78 = a78.clone();
    a78[49..53].copy_from_slice(&(rom.len() as u32).to_be_bytes());
    a78[54] = 0x06;
    let mut file = a78;
    file.extend(&rom);
    let mut c = Cart7800::new(&file).unwrap();
    assert_eq!(c.low, Low::Ram);
    assert_eq!((c.read(0xC000), c.read(0x8000)), (7, 0));
    c.write(0x8000, 3);
    assert_eq!(c.read(0x9000), 3);
    c.write(0x4123, 0xAB);
    assert_eq!(c.read(0x4123), 0xAB);
}

/// Debug harness: ROM=path FRAMES=n OUT=dir SHOTS=a,b INPUT=frame:hexmask,...
///   cargo test --release run_7800 -- --ignored --nocapture
/// Writes OUT/a78_N.rgba (320 x 243 RGBA).
#[test]
#[ignore]
fn run_7800() {
    let rom = std::fs::read(std::env::var("ROM").unwrap()).unwrap();
    let frames: u64 = std::env::var("FRAMES").map(|s| s.parse().unwrap()).unwrap_or(120);
    let out = std::env::var("OUT").unwrap_or("/tmp".into());
    let shots: Vec<u64> = std::env::var("SHOTS").unwrap_or_default().split(',').filter_map(|s| s.parse().ok()).collect();
    let inputs: Vec<(u64, u32)> = std::env::var("INPUT").unwrap_or_default().split(',')
        .filter_map(|p| p.split_once(':')).map(|(f, m)| (f.parse().unwrap(), u32::from_str_radix(m, 16).unwrap())).collect();
    let mut sys = A7800System::from_rom(&rom, "harness", &[]).unwrap();
    let mut mask = 0;
    let t = std::time::Instant::now();
    for f in 0..frames {
        for &(at, m) in &inputs { if at == f { mask = m; } }
        sys.run_frame(mask | NO_KEY);
        if shots.contains(&f) {
            std::fs::write(format!("{out}/a78_{f}.rgba"), sys.framebuffer()).unwrap();
            let samples = sys.drain_audio().len();
            println!("frame {f}: CTRL={:02X} audio samples {samples}", sys.bus.maria.regs[0x1C]);
        }
    }
    println!("{frames} frames in {:?} ({}x{})", t.elapsed(), OUT_W, OUT_H);
}
