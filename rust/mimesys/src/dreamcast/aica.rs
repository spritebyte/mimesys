//! AICA: its 2 MB sound RAM (SH-4 00800000, ARM 00000000), the register file (SH-4
//! 00700000, ARM 00800000), the ARM7's reset (2C00), timers A-C, the interrupt
//! controllers to the ARM (SCIEB / SCIPD / SCIRE, levels SCILV0-2, FIQ, IRQL 2D00, clear
//! 2D04) and to the SH-4 (MCIEB / MCIPD / MCIRE → Holly external bit 1), and the
//! real-time clock (00710000). The 64 channels and the DSP (sound) come in stage 6.
//! Register behaviour after MAME devices/sound/aica.cpp (BSD-3-Clause).
//!
//! The clock counts seconds since 1950-01-01 00:00; 710000 = high 16 bits, 710004 = low 16
//! bits, 710008 bit 0 = write enable.

use serde::{Deserialize, Serialize};

use crate::common::cpu::arm::arm7tdmi::ArmBus;

use super::aica_sound::Sound;

pub const WAVE_SIZE: usize = 0x20_0000;
/// seconds from 1950-01-01 to 1970-01-01
pub const EPOCH_1950: u64 = 631_152_000;
/// the AICA's sample rate (22.5792 MHz / 512)
pub const SAMPLE_RATE: u64 = 44_100;

// common registers (offsets in the register file)
const TIMA: usize = 0x2890;
const SCIEB: usize = 0x289C;
const SCIPD: usize = 0x28A0;
const SCIRE: usize = 0x28A4;
const SCILV0: usize = 0x28A8;
const MCIEB: usize = 0x28B4;
const MCIPD: usize = 0x28B8;
const MCIRE: usize = 0x28BC;
const ARMRST: usize = 0x2C00;
const IRQL: usize = 0x2D00;
const IRQR: usize = 0x2D04;

/// interrupt bits (SCIPD / MCIPD): 5 = the other CPU, 6-8 = timers A-C, 10 = one sample
pub const INT_CPU: u16 = 1 << 5;
pub const INT_TIMA: u16 = 1 << 6;
pub const INT_SAMPLE: u16 = 1 << 10;

/// A timer: an 8-bit counter stepping every 2^prescale samples; overflow sets its bit.
#[derive(Clone, Copy, Default, Serialize, Deserialize)]
struct Timer {
    count: u32,
    shift: u32,
    /// samples since the last step
    acc: u32,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Aica {
    pub wave: Vec<u8>,
    /// 00700000-00707FFF, as bytes
    pub regs: Vec<u8>,
    /// the ARM7 is held in reset
    pub arm_reset: bool,
    /// clock: seconds at `rtc_cycle`
    pub rtc_base: u64,
    pub rtc_cycle: u64,
    rtc_we: bool,
    #[serde(default)]
    #[allow(dead_code)] // kept so older save states load
    rtc_hi: u16,
    #[serde(default)]
    timers: [Timer; 3],
    /// pending interrupts to the ARM / the SH-4
    #[serde(default)]
    pub scipd: u16,
    #[serde(default)]
    pub mcipd: u16,
    /// samples counted so far (timers)
    #[serde(default)]
    pub samples: u64,
    /// the 64 channels and the DSP
    #[serde(default)]
    pub snd: Sound,
    /// output samples (stereo, interleaved) waiting for the frontend
    #[serde(skip)]
    pub out: Vec<f32>,
    /// the ARM's register reads by address (debug)
    #[serde(skip)]
    pub arm_reads: std::collections::BTreeMap<u32, u64>,
    /// interrupts asked for through MCIPD (to the SH-4) and SCIPD (to the ARM) bit 5
    #[serde(skip)]
    pub cpu_ints: [u64; 2],
}

impl Default for Aica {
    fn default() -> Self { Self::new(0) }
}

impl Aica {
    /// `unix_now`: the host clock (seconds since 1970).
    pub fn new(unix_now: u64) -> Self {
        Self {
            wave: vec![0; WAVE_SIZE], regs: vec![0; 0x8000], arm_reset: true,
            rtc_base: unix_now + EPOCH_1950, rtc_cycle: 0, rtc_we: false, rtc_hi: 0,
            timers: [Timer::default(); 3], scipd: 0, mcipd: 0, samples: 0, snd: Sound::default(), out: Vec::new(), arm_reads: Default::default(), cpu_ints: [0; 2],
        }
    }

    pub fn reset(&mut self) {
        self.regs.fill(0);
        self.arm_reset = true;
        self.timers = [Timer::default(); 3];
        self.scipd = 0;
        self.mcipd = 0;
        self.snd = Sound::default();
    }

    fn r16(&self, o: usize) -> u16 { u16::from_le_bytes([self.regs[o], self.regs[o + 1]]) }

    /// The interrupts the ARM has enabled and pending.
    fn arm_pending(&self) -> u16 { self.scipd & self.r16(SCIEB) & 0x7FF }

    /// The ARM's FIQ line.
    pub fn arm_fiq(&self) -> bool { self.arm_pending() != 0 }

    /// The SH-4's interrupt (Holly external bit 1).
    pub fn sh4_irq(&self) -> bool { self.mcipd & self.r16(MCIEB) & 0x7FF != 0 }

    /// IRQL: the level (0-7) of the interrupt the ARM should handle, from the SCILV bits
    /// (interrupts above bit 7 use bit 7's level). Timer A first, then B, C, the rest.
    fn irql(&self) -> u16 {
        let p = self.arm_pending();
        let Some(bit) = [6u32, 7, 8, 3, 5, 4, 9, 10, 0, 1, 2].into_iter().find(|&b| p & (1 << b) != 0) else { return 0 };
        let b = bit.min(7);
        (0..3).map(|k| ((self.r16(SCILV0 + 4 * k) >> b) & 1) << k).sum()
    }

    /// Advance the timers to `samples` (since power-on).
    pub fn advance(&mut self, samples: u64) { self.advance_cd(samples, &mut |_| [0, 0]); }

    /// Advance to `samples`, generating sound; `cd(k)` gives the CD audio for each sample.
    pub fn advance_cd(&mut self, samples: u64, cd: &mut dyn FnMut(u64) -> [i16; 2]) {
        let n = samples.saturating_sub(self.samples);
        if n == 0 { return; }
        for k in 0..n.min(SAMPLE_RATE) {
            let (l, r) = self.snd.sample(&mut self.regs, &mut self.wave, cd(k));
            if self.out.len() < 2 * SAMPLE_RATE as usize { self.out.push(l); self.out.push(r); }
        }
        self.samples = samples;
        self.scipd |= INT_SAMPLE;
        self.mcipd |= INT_SAMPLE;
        for (i, t) in self.timers.iter_mut().enumerate() {
            let total = t.acc as u64 + n;
            let steps = total >> t.shift;
            t.acc = (total & ((1 << t.shift) - 1)) as u32;
            if steps == 0 { continue; }
            if t.count as u64 + steps >= 256 {
                self.scipd |= INT_TIMA << i;
                self.mcipd |= INT_TIMA << i;
            }
            t.count = ((t.count as u64 + steps) & 0xFF) as u32;
        }
    }

    /// Samples until the next timer overflow (for scheduling), if a timer can interrupt.
    pub fn next_timer(&self) -> Option<u64> {
        let en = self.r16(SCIEB) | self.r16(MCIEB);
        self.timers.iter().enumerate().filter(|(i, _)| en & (INT_TIMA << i) != 0)
            .map(|(_, t)| ((256 - t.count as u64) << t.shift).saturating_sub(t.acc as u64).max(1)).min()
    }

    /// A 16-bit register was written: act on it.
    fn update(&mut self, o: usize, old: u16) {
        let v = self.r16(o);
        match o {
            TIMA | 0x2894 | 0x2898 => {
                let t = &mut self.timers[(o - TIMA) / 4];
                t.count = (v & 0xFF) as u32;
                t.shift = (v >> 8 & 7) as u32;
                t.acc = 0;
            }
            SCIPD => {
                // only the CPU interrupt can be set by writing
                if v & INT_CPU != 0 { self.scipd |= INT_CPU; self.cpu_ints[1] += 1; }
                self.put16(o, old);
            }
            SCIRE => { self.scipd &= !v; self.put16(o, 0); }
            MCIPD => {
                if v & INT_CPU != 0 { self.mcipd |= INT_CPU; self.cpu_ints[0] += 1; }
                self.put16(o, old);
            }
            MCIRE => { self.mcipd &= !v; self.put16(o, 0); }
            0x288C if v & 1 != 0 => self.dma(),
            0..=0x1FFF => self.snd.slot_write(&mut self.regs, o / 0x80, o % 0x80),
            0x3400..=0x3BFF => self.snd.dsp_changed(),
            _ => {}
        }
    }

    /// The AICA's own DMA (2880-288C): between sound RAM and the register file.
    fn dma(&mut self) {
        let mut mem = ((self.r16(0x2880) as u32 & 0xFE00) << 7 | (self.r16(0x2884) as u32 & 0xFFFC)) as usize;
        let mut reg = (self.r16(0x2888) & 0x7FFC) as usize;
        let gate = self.r16(0x2888) & 0x8000 != 0;
        let len = (self.r16(0x288C) & 0x7FFC) as usize;
        let to_mem = self.r16(0x288C) & 0x8000 != 0;
        for _ in (0..len).step_by(2) {
            if to_mem {
                let v = if gate { 0 } else { self.r16(reg & 0x7FFE) };
                let m = mem & (WAVE_SIZE - 2);
                self.wave[m..m + 2].copy_from_slice(&v.to_le_bytes());
            } else {
                let m = mem & (WAVE_SIZE - 2);
                let v = if gate { 0 } else { u16::from_le_bytes([self.wave[m], self.wave[m + 1]]) };
                self.put16(reg & 0x7FFE, v);
            }
            mem += 4;
            reg += 4;
        }
        let c = self.r16(0x288C) & !1;
        self.put16(0x288C, c);
        self.mcipd |= 1 << 4;
        self.scipd |= 1 << 4;
    }

    fn put16(&mut self, o: usize, v: u16) { self.regs[o..o + 2].copy_from_slice(&v.to_le_bytes()); }

    pub fn rtc(&self, now: u64) -> u32 { (self.rtc_base + (now.saturating_sub(self.rtc_cycle)) / super::bus::CLOCK) as u32 }

    pub fn rtc_read(&self, off: u32, now: u64) -> u32 {
        match off & 0xC {
            0 => self.rtc(now) >> 16,
            4 => self.rtc(now) & 0xFFFF,
            _ => 0,
        }
    }

    /// Writes need 710008 bit 0 set. Each half replaces its 16 bits of the running count,
    /// so the BIOS may write them in either order (it writes the low half first: setting
    /// the date showed 1950 + a few hours when the low write ended the sequence).
    pub fn rtc_write(&mut self, off: u32, v: u32, now: u64) {
        let cur = self.rtc(now) as u64;
        match off & 0xC {
            8 => self.rtc_we = v & 1 != 0,
            0 if self.rtc_we => { self.rtc_base = (v as u64 & 0xFFFF) << 16 | (cur & 0xFFFF); self.rtc_cycle = now; }
            4 if self.rtc_we => { self.rtc_base = (cur & 0xFFFF_0000) | (v as u64 & 0xFFFF); self.rtc_cycle = now; }
            _ => {}
        }
    }

    /// A register read (offset in the register file, either CPU).
    pub fn reg_read(&mut self, off: u32, size: u32) -> u32 {
        let o = off as usize & 0x7FFF;
        let mut v = 0u32;
        for k in 0..size as usize {
            let a = (o + k) & 0x7FFF;
            let w = self.read_reg16(a & !1);
            v |= ((w >> ((a & 1) * 8)) as u32 & 0xFF) << (8 * k);
        }
        v
    }

    /// A register's value without read side effects (debug).
    pub fn peek16(&self, o: usize) -> u16 {
        match o {
            SCIPD => self.scipd,
            MCIPD => self.mcipd,
            TIMA | 0x2894 | 0x2898 => self.r16(o) & 0xFF00 | self.timers[(o - TIMA) / 4].count as u16,
            _ => self.r16(o & 0x7FFE),
        }
    }

    fn read_reg16(&mut self, o: usize) -> u16 {
        match o {
            0x2810 => self.snd.monitor(&self.regs),
            0x2814 => self.snd.position(&self.regs),
            ARMRST => self.r16(o) & !1 | self.arm_reset as u16,
            TIMA | 0x2894 | 0x2898 => self.r16(o) & 0xFF00 | self.timers[(o - TIMA) / 4].count as u16,
            SCIPD => self.scipd,
            MCIPD => self.mcipd,
            IRQL => self.irql(),
            _ => self.r16(o),
        }
    }

    /// A register write (offset in the register file, either CPU).
    pub fn reg_write(&mut self, off: u32, v: u32, size: u32) {
        let o = off as usize & 0x7FFF;
        if o & !3 == ARMRST {
            let reset = v & 1 != 0;
            if self.arm_reset && !reset { crate::dlog!("Dreamcast: the AICA's ARM7 was started"); }
            self.arm_reset = reset;
            return;
        }
        if o == IRQR { return; } // FIQ clear: the line follows the pending bits
        // the 16-bit registers touched, with their values before the write
        let mut touched: [(usize, u16); 4] = [(usize::MAX, 0); 4];
        let mut n = 0;
        for k in 0..size as usize {
            let r = ((o + k) & 0x7FFF) & !1;
            if !touched[..n].iter().any(|t| t.0 == r) { touched[n] = (r, self.r16(r)); n += 1; }
        }
        for k in 0..size as usize { self.regs[(o + k) & 0x7FFF] = (v >> (8 * k)) as u8; }
        for &(r, old) in &touched[..n] { self.update(r, old); }
    }
}

/// The ARM7's view: sound RAM at 0 (2 MB, mirrored to 8 MB), registers at 00800000.
impl ArmBus for Aica {
    fn read8(&mut self, a: u32, _: u8) -> u8 { self.arm_read(a, 1) as u8 }
    fn read16(&mut self, a: u32, _: u8) -> u16 { self.arm_read(a & !1, 2) as u16 }
    fn read32(&mut self, a: u32, _: u8) -> u32 {
        let v = self.arm_read(a & !3, 4);
        v.rotate_right((a & 3) * 8)
    }
    fn write8(&mut self, a: u32, v: u8, _: u8) { self.arm_write(a, v as u32, 1) }
    fn write16(&mut self, a: u32, v: u16, _: u8) { self.arm_write(a & !1, v as u32, 2) }
    fn write32(&mut self, a: u32, v: u32, _: u8) { self.arm_write(a & !3, v, 4) }
}

impl Aica {
    fn arm_read(&mut self, a: u32, size: u32) -> u32 {
        match a & 0x00FF_FFFF {
            0..=0x7F_FFFF => {
                let o = a as usize & (WAVE_SIZE - 1);
                (0..size as usize).fold(0, |v, k| v | (self.wave[(o + k) & (WAVE_SIZE - 1)] as u32) << (8 * k))
            }
            0x80_0000..=0x80_7FFF => {
                if self.arm_reads.len() < 256 || self.arm_reads.contains_key(&(a & 0x7FFF)) {
                    *self.arm_reads.entry(a & 0x7FFF).or_insert(0) += 1;
                }
                self.reg_read(a - 0x80_0000, size)
            }
            _ => 0,
        }
    }

    fn arm_write(&mut self, a: u32, v: u32, size: u32) {
        match a & 0x00FF_FFFF {
            0..=0x7F_FFFF => {
                let o = a as usize & (WAVE_SIZE - 1);
                for k in 0..size as usize { self.wave[(o + k) & (WAVE_SIZE - 1)] = (v >> (8 * k)) as u8; }
            }
            0x80_0000..=0x80_7FFF => self.reg_write(a - 0x80_0000, v, size),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_counts_and_can_be_set() {
        let mut a = Aica::new(0);
        assert_eq!(a.rtc(0), EPOCH_1950 as u32);
        assert_eq!(a.rtc(400_000_000), EPOCH_1950 as u32 + 2);
        a.rtc_write(0x710008, 1, 0);
        a.rtc_write(0x710000, 0x1234, 0);
        a.rtc_write(0x710004, 0x5678, 1_000);
        assert_eq!(a.rtc_read(0x710000, 1_000), 0x1234);
        assert_eq!(a.rtc_read(0x710004, 200_001_000), 0x5679);
        // low half first, then high (the BIOS's order)
        a.rtc_write(0x710004, 0xBEEF, 2_000);
        a.rtc_write(0x710000, 0x8000, 2_000);
        assert_eq!(a.rtc(2_000), 0x8000_BEEF);
        a.rtc_write(0x710008, 0, 0);
        a.rtc_write(0x710000, 0, 0);
        assert_eq!(a.rtc_read(0x710000, 2_000), 0x8000, "write enable off");
    }
}
