//! The SCSP and the 68000
use super::scsp::Scsp;
use super::tests_support::*;
use crate::common::system::System;

fn put(ram: &mut [u8], a: usize, words: &[u16]) {
    for (k, w) in words.iter().enumerate() { ram[a + k * 2..a + k * 2 + 2].copy_from_slice(&w.to_be_bytes()); }
}

#[test]
fn sndon_starts_the_68000_which_answers_the_mailbox() {
    let mut s = spin_system();
    let ram = &mut s.bus.scsp.ram;
    // vectors: SSP 0x7FFF0, PC 0x1000; move.b #0,($700).l ; bra.s *
    put(ram, 0, &[0x0007, 0xFFF0, 0x0000, 0x1000]);
    put(ram, 0x1000, &[0x13FC, 0x0000, 0x0000, 0x0700, 0x60FE]);
    ram[0x700] = 0x55;
    // SNDON through the SMPC, as the BIOS does
    s.bus.write16(0x0010_001E, 0x06, false, true);
    s.run_frame(0);
    assert!(s.bus.smpc.sound_on);
    assert_eq!(s.bus.scsp.ram[0x700], 0, "the 68000 cleared the command byte");
    assert!(!s.drain_audio().is_empty(), "44.1 kHz samples come out");
}

#[test]
fn timer_a_interrupts_the_68000_and_the_main_cpu() {
    let mut c = Scsp::new();
    c.write_reg(0x424, 0x40, 0xFFFF);      // SCILV0: timer A -> bit 0 of the level
    c.write_reg(0x426, 0x40, 0xFFFF);      // SCILV1: and bit 1 -> level 3
    c.write_reg(0x41E, 0x40, 0xFFFF);      // SCIEB: timer A
    c.write_reg(0x42A, 0x40, 0xFFFF);      // MCIEB: timer A to the main CPU too
    c.write_reg(0x418, 0x0100 | 250, 0xFFFF);   // prescale 2, 5 counts: 10 samples
    for _ in 0..9 { c.sample(); }
    assert_eq!(c.ipl(), 0);
    c.sample();
    assert_eq!(c.ipl(), 3);
    assert_ne!(c.read_reg(0x420) & 0x40, 0, "SCIPD");
    assert!(c.main_irq);
    assert_ne!(c.read_reg(0x42C) & 0x40, 0, "MCIPD");
    // SCIRE clears it (the timer is done, so the pending bit comes back, as on hardware)
    c.write_reg(0x422, 0x40, 0xFFFF);
    assert_eq!(c.ipl(), 0);
    c.write_reg(0x42E, 0x40, 0xFFFF);      // MCIRE
    assert_eq!(c.read_reg(0x42C) & 0x40, 0);
}

#[test]
fn a_keyed_on_slot_plays_and_stops() {
    let mut c = Scsp::new();
    // a 16-bit square wave at 0x2000, 64 samples, looped
    for i in 0..64 { let v: u16 = if i < 32 { 0x4000 } else { 0xC000 }; c.ram[0x2000 + i * 2..0x2002 + i * 2].copy_from_slice(&v.to_be_bytes()); }
    c.write_reg(0x400, 0x000F, 0xFFFF);             // MVOL 15
    c.write_reg(0x02, 0x2000, 0xFFFF);              // SA
    c.write_reg(0x04, 0, 0xFFFF);                   // LSA
    c.write_reg(0x06, 64, 0xFFFF);                  // LEA
    c.write_reg(0x08, 0x001F, 0xFFFF);              // AR 31 (instant)
    c.write_reg(0x0A, 0x001F, 0xFFFF);              // RR 31
    c.write_reg(0x0C, 0, 0xFFFF);                   // TL 0
    c.write_reg(0x10, 0, 0xFFFF);                   // pitch 1:1
    c.write_reg(0x16, 0xE000, 0xFFFF);              // DISDL 7, centre
    c.write_reg(0x00, 0x1800 | 0x0020, 0xFFFF);     // key on (KEYONEX + KEYONB), normal loop
    for _ in 0..2000 { c.sample(); }
    let out = std::mem::take(&mut c.out);
    let peak = out.iter().fold(0f32, |m, v| m.max(v.abs()));
    assert!(peak > 0.1, "peak {peak}");
    // key off: the release fades it out
    c.write_reg(0x00, 0x1000 | 0x0020, 0xFFFF);
    for _ in 0..20000 { c.sample(); }
    let out = std::mem::take(&mut c.out);
    assert!(out[out.len() - 100..].iter().all(|v| v.abs() < 1e-3));
}
