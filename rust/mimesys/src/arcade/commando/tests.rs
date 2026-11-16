use super::commando_system::*;
use crate::common::cpu::z80::Bus;
use crate::common::input as pad;
use crate::common::system::System;
use std::collections::HashMap;

/// The board's opcode scramble (it undoes itself).
fn scramble(b: u8) -> u8 { (b & 0x11) | (b & 0xE0) >> 4 | (b & 0x0E) << 4 }

fn regions(main: &[u8], snd: &[u8], irq: &[(usize, u8)]) -> HashMap<String, Vec<u8>> {
    let mut rom = vec![0u8; 0xC000];
    rom[..main.len()].copy_from_slice(main);
    let mut s = vec![0u8; 0x4000];
    s[..snd.len()].copy_from_slice(snd);
    let mut p = vec![0u8; 0x100];
    for &(l, v) in irq { p[l] = v; }
    let mut proms = vec![0u8; 0x500];
    for i in 0..0x300 { proms[i] = (i & 15) as u8; }
    let mut r = HashMap::new();
    r.insert("maincpu".into(), rom);
    r.insert("audiocpu".into(), s);
    r.insert("chars".into(), vec![0xFFu8; 0x4000]);
    r.insert("tiles".into(), vec![0u8; 0x18000]);
    r.insert("sprites".into(), vec![0xFFu8; 0x18000]);
    r.insert("irqprom".into(), p);
    r.insert("proms".into(), proms);
    r
}

/// Opcodes come through the scramble (operands don't, nor the very first byte); the
/// PROM's vblank RST 10 arrives once a frame and the sound CPU's four; the inputs read
/// at C001; the sound CPU drives the YM2203s.
#[test]
fn encrypted_opcodes_irqs_and_sound() {
    let mut main = vec![0u8; 0x20];
    let prog: [(usize, &[u8]); 8] = [
        (0x00, &[0x31, 0x00, 0xF0]),        // LD SP,F000 (first byte in the clear)
        (0x03, &[0xED, 0x46]),              // IM 0
        (0x05, &[0xFB]),                    // EI
        (0x06, &[0x18, 0xFE]),              // JR $
        (0x10, &[0x21, 0x00, 0xE0]),        // LD HL,E000
        (0x13, &[0x34]),                    // INC (HL)
        (0x14, &[0x3A, 0x01, 0xC0]),        // LD A,(C001)
        (0x17, &[0x32, 0x01, 0xE0]),        // LD (E001),A
    ];
    for (at, bytes) in prog { main[at..at + bytes.len()].copy_from_slice(bytes); }
    main[0x1A..0x1D].copy_from_slice(&[0xFB, 0xED, 0x4D]); // EI; RETI
    for at in [0x03, 0x04, 0x05, 0x06, 0x10, 0x13, 0x14, 0x17, 0x1A, 0x1B, 0x1C] { main[at] = scramble(main[at]); }
    let mut p = vec![0x31, 0x00, 0x48, 0xED, 0x56, 0xFB];
    for (r, v) in [(0x00u8, 0x80u8), (0x01, 0x00), (0x07, 0x3E), (0x08, 0x0F)] {
        p.extend([0x3E, r, 0x32, 0x00, 0x80, 0x3E, v, 0x32, 0x01, 0x80]);
    }
    p.extend([0x18, 0xFE]);
    let mut snd = vec![0u8; 0x40];
    snd[..p.len()].copy_from_slice(&p);
    snd[0x38..0x3F].copy_from_slice(&[0x21, 0x00, 0x40, 0x34, 0xFB, 0xED, 0x4D]);
    // (lines 256-261 read PROM entries 0-5 again, as on the board: keep those clear)
    let irq = [(240, 0x0A), (8, 0x04), (66, 0x04), (132, 0x04), (198, 0x04)];
    let mut s = CommandoSystem::new("commando", regions(&main, &snd, &irq), false).unwrap();
    for _ in 0..10 { s.run_frame(pad::UP | pad::B); }
    assert_eq!(s.board.read(0xE000), 10, "RST 10 once a frame");
    assert_eq!(s.board.read(0xE001), !(0x08 | 0x10), "up and fire, active low");
    assert_eq!(s.sound_bus().read(0x4000), 40, "four sound IRQs a frame");
    let audio = s.drain_audio();
    assert!(audio.len() > 1400);
    assert!(audio.iter().fold(0f32, |m, v| m.max(v.abs())) > 0.03, "the SSG tone plays");
}

/// The control register's bit 4 holds the sound CPU in reset.
#[test]
fn sound_reset_line() {
    let main = [0x18u8, 0xFE];
    let snd = [0x3E, 0x55, 0x32, 0x00, 0x40, 0x18, 0xFE]; // LD A,55; LD (4000),A; JR $
    let mut s = CommandoSystem::new("commando", regions(&main, &snd, &[]), false).unwrap();
    s.board.write(0xC804, 0x10);
    s.run_frame(0);
    assert_eq!(s.sound_bus().read(0x4000), 0, "held in reset");
    s.board.write(0xC804, 0x00);
    s.run_frame(0);
    assert_eq!(s.sound_bus().read(0x4000), 0x55, "running");
}
