//! Williams board tests with small hand-written programs (no game ROMs needed).

use std::collections::HashMap;
use crate::arcade::williams::williams_system::{Game, WilliamsSystem, SCREEN_W};
use crate::common::system::System;

/// Main program at D000 (6809, IRQ handler at D800), sound program at F000 (6800, IRQ
/// handler at F800).
fn board(main: &[u8], irq: &[u8], sound: &[u8], sound_irq: &[u8]) -> WilliamsSystem {
    board_for(Game::Joust, main, irq, sound, sound_irq)
}

fn board_for(game: Game, main: &[u8], irq: &[u8], sound: &[u8], sound_irq: &[u8]) -> WilliamsSystem {
    // Sinistar has RAM at D000-DFFF: its code goes at E000
    let base: usize = if game == Game::Sinistar { 0xE000 } else { 0xD000 };
    let mut rom = vec![0u8; 0x10000];
    rom[base..base + main.len()].copy_from_slice(main);
    rom[base + 0x800..base + 0x800 + irq.len()].copy_from_slice(irq);
    rom[0xFFF8..0xFFFA].copy_from_slice(&[(base >> 8) as u8 + 8, 0x00]);
    rom[0xFFFE..0x10000].copy_from_slice(&[(base >> 8) as u8, 0x00]);
    let mut snd = vec![0u8; 0x10000];
    snd[0xF000..0xF000 + sound.len()].copy_from_slice(sound);
    snd[0xF800..0xF800 + sound_irq.len()].copy_from_slice(sound_irq);
    snd[0xFFF8..0xFFFA].copy_from_slice(&[0xF8, 0x00]);
    snd[0xFFFE..0x10000].copy_from_slice(&[0xF0, 0x00]);
    let mut r = HashMap::new();
    if game == Game::Defender {
        // D000-FFFF in a 12 KB region, plus 7 banked 4 KB pages (page n filled with n+1)
        r.insert("maincpu".to_string(), rom[0xD000..].to_vec());
        r.insert("banked".to_string(), (0..0x7000).map(|i| (i / 0x1000 + 1) as u8).collect());
    } else {
        r.insert("maincpu".to_string(), rom);
    }
    r.insert("soundcpu".to_string(), snd);
    WilliamsSystem::new(game, "test", r).unwrap()
}

const SPIN: [u8; 2] = [0x20, 0xFE];

#[test]
fn blitter_draws_and_palette_shows_it() {
    let main = [
        0x86, 0x07, 0xB7, 0xC0, 0x01,   // LDA #07; STA C001 (pen 1 = full red)
        0x86, 0x11, 0xB7, 0xCA, 0x01,   // solid colour 11
        0xCC, 0x00, 0x00, 0xFD, 0xCA, 0x02, // source 0000
        0xCC, 0x10, 0x20, 0xFD, 0xCA, 0x04, // dest 1020 (x = 0x20, y = 0x20)
        0xCC, 0x06, 0x0C, 0xFD, 0xCA, 0x06, // 2 wide x 8 high (XOR 4)
        0x86, 0x12, 0xB7, 0xCA, 0x00,   // go: solid, dest in screen layout
        0x20, 0xFE,                     // BRA *
    ];
    let mut s = board(&main, &[], &SPIN, &[]);
    s.run_frame(0);
    assert_eq!(s.bus.ram[0x1020], 0x11);
    assert_eq!(s.bus.ram[0x1120], 0x11);
    assert_eq!(s.bus.ram[0x1027], 0x11);
    assert_eq!(s.bus.ram[0x1028], 0x00, "8 rows");
    assert_eq!(s.bus.ram[0x1220], 0x00, "2 columns");
    s.run_frame(0);
    // screen pixel (x 0x20 - 6, y 0x20 - 7) is pen 1
    let (x, y) = (0x20 - 6, 0x20 - 7);
    let o = (y * SCREEN_W + x) * 4;
    assert_eq!(&s.framebuffer()[o..o + 3], &[255, 0, 0]);
}

#[test]
fn scanline_irqs_and_sound_command() {
    // main: PIA 1 B side: CB1 rising IRQ; A side: DDR B = FF (sound command out)
    let main = [
        0x10, 0xCE, 0xBF, 0x00,         // LDS #BF00
        0x7F, 0xC8, 0x0F,               // CLR C80F (DDR B)
        0x86, 0xFF, 0xB7, 0xC8, 0x0E,   // DDR B = FF
        0x86, 0x07, 0xB7, 0xC8, 0x0F,   // port B, CB1 rising, IRQ on
        0x86, 0x3F, 0xB7, 0xC8, 0x0E,   // idle sound command
        0x1C, 0xEF,                     // ANDCC #EF (enable IRQ)
        0x20, 0xFE,
    ];
    let irq = [
        0xB6, 0xC8, 0x0E,               // LDA C80E (clears the flag)
        0x7C, 0x10, 0x00,               // INC 1000 (counts IRQs)
        0x86, 0x05, 0xB7, 0xC8, 0x0E,   // send command 05
        0x3B,                           // RTI
    ];
    // sound: PIA A = DAC output, B side CB1 rising IRQ; IRQ handler copies port B to the DAC
    let sound = [
        0x8E, 0x00, 0x7F,               // LDS #007F
        0x7F, 0x04, 0x01,               // CLR 0401
        0x86, 0xFF, 0xB7, 0x04, 0x00,   // DDR A = FF
        0x86, 0x04, 0xB7, 0x04, 0x01,   // port A
        0x86, 0x07, 0xB7, 0x04, 0x03,   // port B, CB1 rising, IRQ on
        0x0E,                           // CLI
        0x20, 0xFE,
    ];
    let sound_irq = [
        0xB6, 0x04, 0x02,               // LDA 0402
        0xB7, 0x04, 0x00,               // STA 0400 (DAC)
        0x3B,
    ];
    let mut s = board(&main, &irq, &sound, &sound_irq);
    s.run_frame(0);
    s.run_frame(0);
    let n = s.bus.ram[0x1000];
    s.run_frame(0);
    assert_eq!(s.bus.ram[0x1000] - n, 4, "VA11 rises 4 times a frame (lines 32, 96, 160, 224)");
    assert_eq!(s.bus.sound.dac(), 0xC5, "command with the top bits set reached the DAC");
    assert!(!s.drain_audio().is_empty());
}

#[test]
fn cmos_is_4_bit_and_battery_backed() {
    let main = [0x86, 0x12, 0xB7, 0xCC, 0x05, 0x20, 0xFE];
    let mut s = board(&main, &[], &SPIN, &[]);
    s.run_frame(0);
    assert_eq!(s.get_sram().unwrap()[5], 0xF2);
    assert!(s.is_sram_dirty());
}

#[test]
fn save_state_round_trip() {
    let main = [0x7C, 0x10, 0x00, 0x20, 0xFB];   // INC 1000; BRA back
    let mut s = board(&main, &[], &SPIN, &[]);
    for _ in 0..3 { s.run_frame(0); }
    let st = s.save_state();
    for _ in 0..3 { s.run_frame(0); }
    let a = (s.bus.ram[0x1000], s.framebuffer().to_vec());
    s.load_state(&st).unwrap();
    for _ in 0..3 { s.run_frame(0); }
    assert_eq!((s.bus.ram[0x1000], s.framebuffer().to_vec()), a);
}

#[test]
fn defender_banks_rom_over_io() {
    let main = [
        0x86, 0x05, 0xB7, 0xC4, 0x10,   // CMOS C410 = 5 (bank 0: I/O)
        0x86, 0x3F, 0xB7, 0xC0, 0x03,   // palette 3
        0x86, 0x03, 0xB7, 0xD0, 0x00,   // bank 3
        0xB6, 0xC1, 0x23,               // LDA C123 -> page 3's data
        0xB7, 0x10, 0x00,               // STA 1000
        0x86, 0x00, 0xB7, 0xD0, 0x00,   // back to I/O
        0xB6, 0xC4, 0x10,               // LDA C410 (CMOS)
        0xB7, 0x10, 0x01,
        0x20, 0xFE,
    ];
    let mut s = board_for(Game::Defender, &main, &[], &SPIN, &[]);
    s.run_frame(0);
    assert_eq!(s.bus.ram[0x1000], 3);
    assert_eq!(s.bus.ram[0x1001], 0xF5, "4-bit CMOS");
    assert_eq!(s.get_sram().unwrap().len(), 0x100);
}

#[test]
fn robotron_right_stick_and_bubbles_8bit_cmos() {
    use crate::common::input as pad;
    // read PIA 0 A and B (port select first) into RAM
    let main = [
        0x86, 0x04, 0xB7, 0xC8, 0x05, 0xB7, 0xC8, 0x07,   // CRA = CRB = 04
        0xB6, 0xC8, 0x04, 0xB7, 0x10, 0x00,
        0xB6, 0xC8, 0x06, 0xB7, 0x10, 0x01,
        0x86, 0x9A, 0xB7, 0xCC, 0x00,                     // CMOS
        0x20, 0xED,                                       // back to the reads
    ];
    let mut s = board_for(Game::Robotron, &main, &[], &SPIN, &[]);
    s.run_frame(pad::UP | pad::RS_DOWN | pad::RS_RIGHT | pad::START);
    assert_eq!(s.bus.ram[0x1000], 0x01 | 0x10 | 0x80);
    assert_eq!(s.bus.ram[0x1001], 0x02);
    assert_eq!(s.get_sram().unwrap()[0], 0xFA);
    let mut b = board_for(Game::Bubbles, &main, &[], &SPIN, &[]);
    b.run_frame(0);
    assert_eq!(b.get_sram().unwrap()[0], 0x9A, "Bubbles keeps all 8 bits");
}

#[test]
fn sinistar_window_clips_blits_and_speech_runs() {
    let main = [
        0x86, 0x04, 0xB7, 0xC9, 0x00,   // window on
        0x86, 0x55, 0xB7, 0xD1, 0x23,   // D000 RAM
        0x86, 0x11, 0xB7, 0xCA, 0x01,   // solid colour
        0xCC, 0x73, 0xFE, 0xFD, 0xCA, 0x04, // dest 73FE
        0xCC, 0x00, 0x00, 0xFD, 0xCA, 0x02,
        0xCC, 0x00, 0x05, 0xFD, 0xCA, 0x06, // 4 wide x 1 high (XOR 4)
        0x86, 0x10, 0xB7, 0xCA, 0x00,   // solid, linear
        0x20, 0xFE,
    ];
    // sound: CB2 as a set/reset output toggled forever (the CVSD clock), CA2 high (data)
    let sound = [
        0x86, 0x3C, 0xB7, 0x04, 0x01,   // CA2 output, high
        0x86, 0x38, 0xB7, 0x04, 0x03,   // CB2 high
        0x86, 0x30, 0xB7, 0x04, 0x03,   // CB2 low
        0x20, 0xF4,
    ];
    let mut s = board_for(Game::Sinistar, &main, &[], &sound, &[]);
    s.run_frame(0);
    assert_eq!(&s.bus.ram[0x73FE..0x7402], &[0x11, 0x11, 0, 0], "clipped at 7400");
    assert_eq!(crate::common::cpu::m6809::M6809Bus::read(&mut s.bus, 0xD123), 0x55);
    let audio = s.drain_audio();
    assert!(audio.iter().any(|v| v.abs() > 0.05), "speech output moves");
}
