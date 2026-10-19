use super::a2600_bus::A2600Bus;
use super::a2600_system::A2600System;
use super::cart::{Cart, Scheme};
use super::test_cpu;
use super::riot::Riot;
use super::tia::{rgb, Tia, SCREEN_W};
use crate::common::cpu::m6502::AddressBus;
use crate::common::input as pad;
use crate::common::system::System;

const NO_KEY: u32 = 0x0F << pad::KEYPAD_SHIFT;

// ---------------- CPU: decimal mode ----------------

#[test]
fn decimal_mode_adc_and_sbc() {
    let prog = [
        0xF8,
        0x18, 0xA9, 0x15, 0x69, 0x27, 0x85, 0x10,   // 15 + 27 = 42
        0x38, 0xA9, 0x42, 0xE9, 0x15, 0x85, 0x11,   // 42 - 15 = 27
        0x38, 0xA9, 0x10, 0xE9, 0x01, 0x85, 0x12,   // 10 - 01 = 09
        0x18, 0xA9, 0x99, 0x69, 0x01, 0x85, 0x13,   // 99 + 01 = 00, carry
        0x38, 0xA9, 0x00, 0xE9, 0x01, 0x85, 0x14,   // 00 - 01 = 99, borrow (C=0)
        0x08, 0x68, 0x85, 0x15,                     // flags after the last SBC
        0xD8, 0x38, 0xA9, 0x42, 0xE9, 0x15, 0x85, 0x16,   // binary again: 0x42 - 0x15 = 0x2D
        0x4C, 0x30, 0x02,
    ];
    let m = test_cpu::run(&prog, 300);
    assert_eq!(&m[0x10..0x15], &[0x42, 0x27, 0x09, 0x00, 0x99]);
    assert_eq!(m[0x15] & 0x01, 0, "borrow clears carry");
    assert_eq!(m[0x16], 0x2D, "binary SBC unchanged");
}

// ---------------- bus decoding ----------------

fn bus_4k() -> A2600Bus {
    let mut rom = vec![0xEAu8; 4096];
    rom[0] = 0x11; rom[0xFFF] = 0x22;
    A2600Bus::new(Cart::new(rom).unwrap(), 44_100)
}

#[test]
fn address_decoding_and_mirrors() {
    let mut b = bus_4k();
    assert_eq!(b.read_byte(0x1000), 0x11);
    assert_eq!(b.read_byte(0xF000), 0x11, "13 address lines: F000 = 1000");
    assert_eq!(b.read_byte(0xFFFF), 0x22);
    b.write_byte(0x0080, 0x5A);
    assert_eq!(b.read_byte(0x0180), 0x5A, "stack page mirrors zero page RAM");
    b.write_byte(0x0049, 0x1E);                  // TIA COLUBK through a mirror (0x40 + 0x09)
    b.tia.clock();
    b.write_byte(0x0282, 0xFF);                  // SWCHB output latch, but DDR = input
    assert_eq!(b.read_byte(0x0282), 0x0B, "console switches read through");
}

// ---------------- RIOT timer ----------------

#[test]
fn riot_timer_intervals_and_underflow() {
    let mut r = Riot::new();
    r.write(0x296, 2);                           // TIM64T = 2
    r.tick();
    assert_eq!(r.read(0x284), 1, "decrements right after the write");
    for _ in 0..64 { r.tick(); }
    assert_eq!(r.read(0x284), 0);
    assert_eq!(r.read(0x285) & 0x80, 0);
    for _ in 0..64 { r.tick(); }
    assert_eq!(r.read(0x285) & 0x80, 0x80, "underflow flag");
    assert_eq!(r.timer_value(), 0xFF);
    r.tick();
    assert_eq!(r.timer_value(), 0xFE, "after underflow it counts every cycle");
    r.read(0x284);                               // reading INTIM restores the interval
    r.tick();
    assert_eq!(r.timer_value(), 0xFE);
}

// ---------------- cartridges ----------------

fn banked(banks: usize, bank_size: usize) -> Vec<u8> {
    let mut rom = vec![0u8; banks * bank_size];
    for b in 0..banks {
        for (i, x) in rom[b * bank_size..(b + 1) * bank_size].iter_mut().enumerate() { *x = (b as u8) << 4 | (i as u8 & 0x0F); }
    }
    rom
}

#[test]
fn f8_f6_f4_hotspots() {
    let mut c = Cart::new(banked(2, 4096)).unwrap();
    assert_eq!(c.scheme, Scheme::F8);
    assert_eq!(c.read(0x010) >> 4, 1, "F8 starts in the last bank");
    c.read(0xFF8);
    assert_eq!(c.read(0x010) >> 4, 0);
    c.write(0xFF9, 0);
    assert_eq!(c.read(0x010) >> 4, 1, "writes switch too");

    let mut c = Cart::new(banked(4, 4096)).unwrap();
    assert_eq!(c.scheme, Scheme::F6);
    c.read(0xFF8);
    assert_eq!(c.read(0x010) >> 4, 2);

    let mut c = Cart::new(banked(8, 4096)).unwrap();
    assert_eq!(c.scheme, Scheme::F4);
    c.read(0xFFB);
    assert_eq!(c.read(0x010) >> 4, 7);
}

#[test]
fn superchip_ram() {
    let mut rom = vec![0xFFu8; 8192];
    rom[0x1000 + 0x300] = 0x42;
    let mut c = Cart::new(rom).unwrap();
    assert!(c.superchip);
    c.write(0x005, 0x99);                        // write port
    assert_eq!(c.read(0x085), 0x99, "read port");
}

#[test]
fn parker_brothers_e0() {
    let mut rom = banked(8, 1024);
    rom[0x100..0x103].copy_from_slice(&[0x8D, 0xE0, 0x1F]);   // STA $1FE0 signature
    let mut c = Cart::new(rom).unwrap();
    assert_eq!(c.scheme, Scheme::E0);
    assert_eq!(c.read(0xC10) >> 4, 7, "slice 3 = last 1K");
    c.read(0xFE2);                               // slice 0 -> bank 2
    c.read(0xFEB);                               // slice 1 -> bank 3
    assert_eq!(c.read(0x010) >> 4, 2);
    assert_eq!(c.read(0x410) >> 4, 3);
}

#[test]
fn tigervision_3f() {
    let mut rom = banked(4, 2048);
    rom[0x10..0x12].copy_from_slice(&[0x85, 0x3F]);
    rom[0x20..0x22].copy_from_slice(&[0x85, 0x3F]);
    let mut c = Cart::new(rom).unwrap();
    assert_eq!(c.scheme, Scheme::Tigervision3F);
    c.tia_write(0x3F, 2);
    assert_eq!(c.read(0x010) >> 4, 2);
    assert_eq!(c.read(0x810) >> 4, 3, "upper 2K fixed to the last bank");
}

// ---------------- TIA rendering ----------------

fn px(t: &Tia, x: usize, y: usize) -> [u8; 3] { let o = (y * SCREEN_W + x) * 4; [t.framebuffer[o], t.framebuffer[o + 1], t.framebuffer[o + 2]] }

/// Clock the TIA through one full line.
fn line(t: &mut Tia) { for _ in 0..228 { t.clock(); } }

#[test]
fn playfield_halves_repeat_and_reflect() {
    let mut t = Tia::new(44_100);
    t.write(0x08, 0x46);                         // COLUPF red
    t.write(0x09, 0x00);
    t.write(0x0D, 0x10);                         // PF0 bit 4 = leftmost 4 pixels
    line(&mut t);
    let red = rgb(0x46);
    assert_eq!(px(&t, 0, 0), red);
    assert_eq!(px(&t, 3, 0), red);
    assert_eq!(px(&t, 4, 0), [0, 0, 0]);
    assert_eq!(px(&t, 80, 0), red, "right half repeats");
    t.write(0x0A, 0x01);                         // reflect
    line(&mut t);
    assert_eq!(px(&t, 80, 1), [0, 0, 0]);
    assert_eq!(px(&t, 159, 1), red, "reflected: right edge");
}

#[test]
fn player_position_copies_and_hmove() {
    let mut t = Tia::new(44_100);
    t.write(0x06, 0x0E);                         // COLUP0 white
    t.write(0x1B, 0x80);                         // GRP0: leftmost pixel
    t.hpos = 68 + 45;                            // strobe RESP0 at pixel 45 -> appears at 50
    t.write(0x10, 0);
    assert_eq!(t.object_positions().0[0], 50);
    t.hpos = 0;
    t.write(0x04, 0x01);                         // NUSIZ0: two copies, 16 apart
    line(&mut t);
    let white = rgb(0x0E);
    assert_eq!(px(&t, 50, 0), white);
    assert_eq!(px(&t, 66, 0), white);
    assert_eq!(px(&t, 51, 0), rgb(0));
    t.write(0x20, 0x20);                         // HMP0 = +2 -> 2 pixels left
    t.write(0x2A, 0);                            // HMOVE during HBLANK
    assert_eq!(t.object_positions().0[0], 48);
    line(&mut t);
    assert_eq!(px(&t, 3, 1), [0, 0, 0], "HMOVE bar blanks the first 8 pixels");
    t.write(0x2B, 0);                            // HMCLR
    t.hpos = 10;
    t.write(0x10, 0);                            // RESP0 in HBLANK -> left edge (3)
    assert_eq!(t.object_positions().0[0], 3);
}

#[test]
fn priority_score_mode_and_collisions() {
    let mut t = Tia::new(44_100);
    t.write(0x06, 0x0E);
    t.write(0x08, 0x46);
    t.write(0x0D, 0xF0);                         // PF0: pixels 0-15
    t.write(0x1B, 0xFF);
    t.hpos = 68;                                 // RESP0 at pixel 0 -> 5
    t.write(0x10, 0);
    t.hpos = 0;
    line(&mut t);
    assert_eq!(px(&t, 6, 0), rgb(0x0E), "player in front of the playfield");
    assert_eq!(t.read(0x02) & 0x80, 0x80, "P0-PF collision");
    t.write(0x0A, 0x04);                         // playfield priority
    line(&mut t);
    assert_eq!(px(&t, 6, 1), rgb(0x46));
    t.write(0x0A, 0x02);                         // score mode: PF uses P0 color on the left
    t.write(0x1B, 0x00);
    line(&mut t);
    assert_eq!(px(&t, 1, 2), rgb(0x0E));
    t.write(0x2C, 0);                            // CXCLR
    assert_eq!(t.read(0x02), 0);
}

#[test]
fn vdel_uses_the_previous_graphics() {
    let mut t = Tia::new(44_100);
    t.write(0x25, 1);                            // VDELP0
    t.write(0x1B, 0xAA);                         // new GRP0 (not shown yet)
    t.write(0x06, 0x0E);
    t.hpos = 68;
    t.write(0x10, 0);
    t.hpos = 0;
    line(&mut t);
    assert_eq!(px(&t, 5, 0), rgb(0), "old GRP0 still 0");
    t.write(0x1C, 0);                            // writing GRP1 copies GRP0 to the delayed copy
    line(&mut t);
    assert_eq!(px(&t, 5, 1), rgb(0x0E));
}

#[test]
fn audio_pure_tone_frequency() {
    let mut t = Tia::new(44_100);
    t.write(0x15, 0x04);                         // AUDC0: pure tone (div 2)
    t.write(0x17, 9);                            // AUDF0: / 10
    t.write(0x19, 0x0F);
    for _ in 0..262 * 60 { line(&mut t); }       // ~1 second
    let samples: Vec<f32> = t.audio_out.chunks(2).map(|c| c[0]).collect();
    let crossings = samples.windows(2).filter(|w| (w[0] >= 0.0) != (w[1] >= 0.0)).count();
    let hz = crossings as f64 / 2.0 / (262.0 * 60.0 * 228.0 / 3_579_545.0);
    let expected = super::tia::AUDIO_RATE / 20.0;           // 31.4 kHz / (2 * 10) = 1570 Hz
    assert!((hz - expected).abs() < expected * 0.03, "{hz} Hz vs {expected}");
}

// ---------------- whole system ----------------

/// A minimal kernel: background color, 3 lines VSYNC, 258 more lines, count frames at $80.
fn test_rom() -> Vec<u8> {
    let mut rom = vec![0xEAu8; 4096];
    let code = [
        0x78, 0xD8, 0xA2, 0xFF, 0x9A,           // SEI CLD LDX #$FF TXS
        0xA9, 0x1E, 0x85, 0x09,                 // COLUBK = $1E
        // frame:                                 F009
        0xA9, 0x02, 0x85, 0x02, 0x85, 0x00,     // LDA #2  STA WSYNC  STA VSYNC
        0x85, 0x02, 0x85, 0x02, 0x85, 0x02,     // 3 lines
        0xA9, 0x00, 0x85, 0x00,                 // VSYNC off
        0xA2, 0xFF,                             // LDX #255
        0x85, 0x02, 0xCA, 0xD0, 0xFB,           // l: STA WSYNC  DEX  BNE l
        0x85, 0x02, 0x85, 0x02, 0x85, 0x02,     // 3 more lines
        0xE6, 0x80,                             // INC $80
        0xA5, 0x0C, 0x85, 0x81,                 // LDA INPT4  STA $81
        0xAD, 0x80, 0x02, 0x85, 0x82,           // LDA SWCHA  STA $82
        0x4C, 0x09, 0xF0,                       // JMP frame
    ];
    rom[..code.len()].copy_from_slice(&code);
    rom[0xFFC] = 0x00; rom[0xFFD] = 0xF0;
    rom
}

#[test]
fn frames_are_262_lines_and_draw_the_background() {
    let mut sys = A2600System::from_rom(&test_rom(), "t", &[]).unwrap();
    for _ in 0..5 { sys.run_frame(NO_KEY); }
    assert_eq!(sys.bus.tia.last_frame_lines, 262);
    assert!(sys.bus.read_byte(0x80) >= 4, "frame counter {}", sys.bus.read_byte(0x80));
    let fb = sys.framebuffer();
    let o = (120 * SCREEN_W + 80) * 4;
    assert_eq!(&fb[o..o + 3], &rgb(0x1E));
    let d = sys.display_info();
    assert_eq!((d.buffer_width, d.buffer_height), (160, 262));
    sys.drain_audio();                           // the first frame after power-on is short
    for _ in 0..5 { sys.run_frame(NO_KEY); }
    let samples = sys.drain_audio().len() / 2 / 5;
    assert!((730..745).contains(&samples), "{samples} samples per frame");
}

#[test]
fn joystick_fire_and_console_switches() {
    let mut sys = A2600System::from_rom(&test_rom(), "t", &[]).unwrap();
    sys.run_frame(NO_KEY | pad::LEFT | pad::A);
    sys.run_frame(NO_KEY | pad::LEFT | pad::A);
    assert_eq!(sys.bus.read_byte(0x81) & 0x80, 0, "INPT4: fire pressed");
    assert_eq!(sys.bus.read_byte(0x82), 0xBF, "SWCHA: P0 left");
    sys.set_input(NO_KEY | pad::START | pad::SELECT);
    assert_eq!(sys.bus.read_byte(0x282) & 0x03, 0x00, "RESET and SELECT pressed");
}

#[test]
fn save_state_round_trip() {
    let mut sys = A2600System::from_rom(&test_rom(), "t", &[]).unwrap();
    for _ in 0..5 { sys.run_frame(NO_KEY); }
    let state = sys.save_state();
    let before = sys.bus.read_byte(0x80);
    for _ in 0..7 { sys.run_frame(NO_KEY); }
    sys.load_state(&state).unwrap();
    assert_eq!(sys.bus.read_byte(0x80), before);
    for _ in 0..7 { sys.run_frame(NO_KEY); }
    assert_eq!(sys.bus.read_byte(0x80), before + 7);
}
