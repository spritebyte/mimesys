//! AICA ARM7 tests: the SH-4 ↔ ARM handshake through sound RAM, timers, and the
//! interrupt controllers in both directions.

use super::aica::*;
use super::bus::{Dc, BIOS_SIZE};
use super::holly;
use crate::common::cpu::sh4::Sh4Bus;

/// A tiny ARM assembler: code from 0, a literal pool from 0x400.
struct Arm { code: Vec<(u32, u32)>, pc: u32, lits: Vec<u32> }

impl Arm {
    fn new() -> Self { Self { code: Vec::new(), pc: 0, lits: Vec::new() } }
    fn at(&mut self, pc: u32) -> &mut Self { self.pc = pc; self }
    fn op(&mut self, w: u32) -> &mut Self { self.code.push((self.pc, w)); self.pc += 4; self }
    /// LDR rd, =v
    fn li(&mut self, rd: u32, v: u32) -> &mut Self {
        let a = 0x400 + 4 * self.lits.len() as u32;
        self.lits.push(v);
        let off = a - (self.pc + 8);
        self.op(0xE59F_0000 | rd << 12 | off)
    }
    fn mov(&mut self, rd: u32, imm: u32) -> &mut Self { self.op(0xE3A0_0000 | rd << 12 | imm) }
    fn str(&mut self, rd: u32, rn: u32) -> &mut Self { self.op(0xE580_0000 | rn << 16 | rd << 12) }
    fn ldr(&mut self, rd: u32, rn: u32) -> &mut Self { self.op(0xE590_0000 | rn << 16 | rd << 12) }
    fn add(&mut self, rd: u32, rn: u32, imm: u32) -> &mut Self { self.op(0xE280_0000 | rn << 16 | rd << 12 | imm) }
    fn b(&mut self, target: u32) -> &mut Self { let pc = self.pc; self.op(0xEA00_0000 | ((target.wrapping_sub(pc + 8) >> 2) & 0xFF_FFFF)) }
    fn halt(&mut self) -> &mut Self { let pc = self.pc; self.b(pc) }
    /// Copy into sound RAM through the SH-4's window.
    fn load(&self, d: &mut Dc) {
        for &(a, w) in &self.code { d.write32(0xA080_0000 + a, w); }
        for (k, &w) in self.lits.iter().enumerate() { d.write32(0xA080_0400 + 4 * k as u32, w); }
    }
}

fn dc() -> Dc { Dc::new(vec![0; BIOS_SIZE], &[], 0) }

/// Let `cycles` SH-4 cycles pass.
fn run(d: &mut Dc, cycles: u64) {
    let end = d.now + cycles;
    while d.now < end {
        d.now = (d.now + 2000).min(end);
        d.service();
    }
}

#[test]
fn arm_answers_the_sh4_through_sound_ram() {
    let mut d = dc();
    let mut a = Arm::new();
    a.mov(0, 1).li(1, 0x5C).str(0, 1).halt();
    a.load(&mut d);
    run(&mut d, 1_000_000);
    assert_eq!(d.read32(0xA080_005C), 0, "held in reset");
    assert_eq!(d.arm_steps, 0);
    d.write32(0xA070_2C00, 0); // release
    run(&mut d, 1_000_000);
    assert_eq!(d.read32(0xA080_005C), 1);
    // ~5.6 M instructions a second: 5 ms ≈ 28200
    assert!((27_000..29_500).contains(&d.arm_steps), "{}", d.arm_steps);
    // reset again: it stops
    d.write32(0xA070_2C00, 1);
    let n = d.arm_steps;
    run(&mut d, 100_000);
    assert_eq!(d.arm_steps, n);
    assert_eq!(d.read32(0xA070_2C00) & 1, 1);
}

#[test]
fn timer_a_raises_fiq_with_its_level() {
    let mut d = dc();
    let mut a = Arm::new();
    // vectors: reset → 0x40, FIQ → 0x100
    a.at(0x00).b(0x40);
    a.at(0x1C).b(0x100);
    // start: level of bit 6 = 5 (SCILV0 + SCILV2), enable timer A, count from 0xF0, FIQ on
    a.at(0x40)
        .li(6, 0x60).mov(5, 0)
        .li(1, 0x80_28A8).mov(0, 0x40).str(0, 1)
        .li(1, 0x80_28B0).str(0, 1)
        .li(1, 0x80_289C).str(0, 1)
        .li(1, 0x80_2890).mov(0, 0xF0).str(0, 1)
        .op(0xE321_F013) // MSR CPSR_c, #0x13
        .halt();
    // FIQ: store IRQL at 0x60, count at 0x64, clear timer A (SCIRE), acknowledge, return
    a.at(0x100)
        .li(8, 0x80_2D00).ldr(9, 8).str(9, 6)
        .add(5, 5, 1).li(8, 0x64).str(5, 8)
        .li(8, 0x80_28A4).mov(9, 0x40).str(9, 8)
        .li(8, 0x80_2D04).mov(9, 1).str(9, 8)
        .op(0xE25E_F004); // SUBS PC, LR, #4
    a.load(&mut d);
    d.write32(0xA070_2C00, 0);
    // 10 ms = 441 samples: overflows after 16, then every 256
    run(&mut d, 2_000_000);
    assert_eq!(d.read32(0xA080_0060), 5, "IRQL");
    assert_eq!(d.read32(0xA080_0064), 2, "two overflows");
    assert_eq!(d.aica.scipd & INT_TIMA, 0, "cleared by SCIRE");
}

#[test]
fn interrupts_between_the_cpus() {
    let mut d = dc();
    // ARM → SH-4: MCIPD bit 5, enabled by MCIEB → Holly external bit 1
    let mut a = Arm::new();
    a.li(1, 0x80_28B8).mov(0, 0x20).str(0, 1).halt();
    a.load(&mut d);
    d.write32(0xA070_28B4, 0x20);
    d.write32(0xA070_2C00, 0);
    run(&mut d, 100_000);
    assert_eq!(d.read32(0xA070_28B8) & 0x20, 0x20);
    assert!(d.holly.r(holly::SB_ISTEXT) & holly::EXT_AICA != 0);
    d.write32(0xA070_28BC, 0x20); // MCIRE
    run(&mut d, 2000);
    assert_eq!(d.holly.r(holly::SB_ISTEXT) & holly::EXT_AICA, 0);
    // SH-4 → ARM: SCIPD bit 5 with SCIEB → FIQ line
    d.write32(0xA070_289C, 0x20);
    assert!(!d.aica.arm_fiq());
    d.write32(0xA070_28A0, 0x20);
    assert!(d.aica.arm_fiq());
    d.write32(0xA070_28A4, 0x20);
    assert!(!d.aica.arm_fiq());
}

#[test]
fn timer_counts_with_its_prescale() {
    let mut a = Aica::new(0);
    a.reg_write(0x2894, 0x0310, 4); // timer B: count 0x10, every 8 samples
    a.advance(80);
    assert_eq!(a.reg_read(0x2894, 2), 0x031A);
    a.advance(80 + 8 * (256 - 0x1A) - 1);
    assert_eq!(a.scipd & (INT_TIMA << 1), 0);
    a.advance(80 + 8 * (256 - 0x1A));
    assert_eq!(a.scipd & (INT_TIMA << 1), INT_TIMA << 1);
    assert_eq!(a.reg_read(0x2894, 2) & 0xFF, 0);
    // byte writes reach the same register
    a.reg_write(0x2898, 0x22, 1);
    a.reg_write(0x2899, 0x01, 1);
    assert_eq!(a.reg_read(0x2898, 2), 0x0122);
}

#[test]
fn sh4_keys_a_channel_and_sound_comes_out() {
    let mut d = dc();
    // a 16-bit looping square wave at 0x2000 in sound RAM, slot 3
    for k in 0..64u32 { d.write16(0xA080_2000 + 2 * k, if k < 32 { 12000 } else { (-12000i16) as u16 }); }
    let s = 0xA070_0000 + 3 * 0x80;
    d.write32(0xA070_2800, 0xF); // master volume
    d.write32(s + 0x04, 0x2000);
    d.write32(s + 0x0C, 64);
    d.write32(s + 0x10, 0x1F);
    d.write32(s + 0x14, 0x1F);
    d.write32(s + 0x24, 0xF << 8);
    d.write32(s, 0xC000 | 1 << 9); // KYONEX + KYONB, loop, 16-bit
    d.aica.out.clear();
    run(&mut d, 200_000_000 / 100); // 10 ms
    let out = std::mem::take(&mut d.aica.out);
    assert!((880..=884).contains(&out.len()), "{} values = 441 stereo samples", out.len());
    let peak = out.iter().cloned().fold(0f32, |m, x| m.max(x.abs()));
    assert!(peak > 0.3, "peak {peak}");
    // the monitor shows slot 3 playing, the position moves
    d.write32(0xA070_280C, 3 << 8);
    let p1 = d.read32(0xA070_2814);
    run(&mut d, 20_000);
    assert_ne!(d.read32(0xA070_2814), p1);
}

/// cargo test --release --lib sound_speed -- --ignored --nocapture
#[test]
#[ignore]
fn sound_speed() {
    let mut a = Aica::new(0);
    for k in 0..0x1000usize { a.wave[k] = (k * 7) as u8; }
    a.reg_write(0x2800, 0xF, 2);
    // a 128-step DSP program (non-zero steps)
    for k in 0..128u32 { a.reg_write(0x3400 + k * 16, 0x0100 + k, 2); a.reg_write(0x3404 + k * 16, 0x8000, 2); }
    for s in 0..32u32 {
        let b = s * 0x80;
        a.reg_write(b + 0x04, 0x100, 2);
        a.reg_write(b + 0x0C, 0x400, 2);
        a.reg_write(b + 0x10, 0x1F, 2);
        a.reg_write(b + 0x24, 0xF00, 2);
        a.reg_write(b + 0x1C, 0x8000 | 0x10 << 10 | 3 << 5 | 3, 2);
        a.reg_write(b, 0x4000 | 1 << 9 | (s % 3) << 7, 2);
    }
    a.reg_write(0, 0xC000 | 1 << 9, 2);
    let t = std::time::Instant::now();
    a.advance(SAMPLE_RATE);
    println!("sound: {:.1} ms per emulated second (32 channels + DSP)", t.elapsed().as_secs_f64() * 1000.0);
}
