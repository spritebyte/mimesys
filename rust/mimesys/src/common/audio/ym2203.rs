//! Yamaha YM2203 (OPN): 3 FM channels (the OPN engine shared with the YM2612,
//! `Ym2612::new_opn`, ported from ymfm) plus an SSG that is an AY-3-8910 (registers
//! 00-0F), and two timers. As ymfm's ym2203: the prescaler (written by addressing
//! 2D / 2E / 2F) sets the FM sample rate to clock / (12 x prescale) and the SSG to what
//! an AY-3-8910 would do at clock x 2 / (prescale x 2 / 3), i.e. clock / 2 at the default
//! prescale of 6. The busy flag isn't emulated (always ready).
//!
//! `run(clocks)` advances the chip; output comes out at the rate given to `new`, mono,
//! the FM held between its samples and averaged over each output sample. Levels follow
//! MAME's usual YM2203 routing (each SSG channel 0.40, FM 0.20).

use serde::{Deserialize, Serialize};
use crate::common::audio::ay8910::Ay8910;
use crate::common::audio::ym2612::Ym2612;

#[derive(Serialize, Deserialize)]
pub struct Ym2203 {
    fm: Ym2612,
    ssg: Ay8910,
    clock: u32,
    address: u8,
    prescale: u8,
    /// input clocks x 3 to the next FM sample and the next SSG step
    fm_left: i32,
    ssg_left: i32,
    fm_last: i32,
    fm_acc: i64,
    fm_n: u32,
    #[serde(skip)]
    pub out: Vec<f32>,
}

impl Ym2203 {
    pub fn new(clock: u32, out_rate: u32) -> Self {
        let mut ssg = Ay8910::new(clock / 2, out_rate);
        ssg.gain = 1.0;
        let mut y = Self { fm: Ym2612::new_opn(), ssg, clock, address: 0, prescale: 6, fm_left: 0, ssg_left: 0,
                           fm_last: 0, fm_acc: 0, fm_n: 0, out: Vec::with_capacity(2048) };
        y.set_prescale(6);
        y
    }

    pub fn reset(&mut self) {
        self.fm.reset();
        self.ssg.reset();
        self.address = 0;
        self.set_prescale(6);
    }

    fn set_prescale(&mut self, p: u8) {
        self.prescale = p;
        // the SSG as an AY-3-8910 at clock x 3 / prescale
        self.ssg.set_clock(self.clock * 3 / p as u32);
    }

    fn fm_period_x3(&self) -> i32 { 36 * self.prescale as i32 }
    /// an AY step is 8 of its clocks
    fn ssg_period_x3(&self) -> i32 { 8 * self.prescale as i32 }

    /// Bus write: offset 0 address, 1 data.
    pub fn write(&mut self, offset: u8, data: u8) {
        if offset & 1 == 0 {
            self.address = data;
            match data {
                0x2D => self.set_prescale(6),
                0x2E if self.prescale == 6 => self.set_prescale(3),
                0x2F => self.set_prescale(2),
                _ => {}
            }
        } else if self.address < 0x10 {
            self.ssg.write_address(self.address);
            self.ssg.write_data(data);
        } else {
            self.fm.write_opn(self.address, data);
        }
    }

    /// Bus read: offset 0 status (timer flags), 1 the SSG register (`port_a` / `port_b`:
    /// what the board drives on the SSG's I/O ports).
    pub fn read(&mut self, offset: u8, port_a: u8, port_b: u8) -> u8 {
        if offset & 1 == 0 { return self.fm.status(); }
        if self.address >= 0x10 { return 0; }
        self.ssg.write_address(self.address);
        self.ssg.read_data_ab(port_a, port_b)
    }

    /// The IRQ pin: a timer flag is up.
    pub fn irq(&self) -> bool { self.fm.status() & 3 != 0 }

    /// Advance by `clocks` input clocks.
    pub fn run(&mut self, clocks: u32) {
        let mut left = clocks as i32 * 3;
        while left > 0 {
            let step = left.min(self.fm_left).min(self.ssg_left).max(1);
            left -= step;
            self.fm_left -= step;
            self.ssg_left -= step;
            if self.fm_left <= 0 {
                self.fm_left += self.fm_period_x3();
                self.fm_last = self.fm.generate_opn();
                self.fm_acc += self.fm_last as i64;
                self.fm_n += 1;
            }
            if self.ssg_left <= 0 {
                self.ssg_left += self.ssg_period_x3();
                let before = self.ssg.out.len();
                self.ssg.tick();
                if self.ssg.out.len() > before {
                    let fm = if self.fm_n > 0 { self.fm_acc as f32 / self.fm_n as f32 } else { self.fm_last as f32 };
                    self.fm_acc = 0;
                    self.fm_n = 0;
                    // the AY core gives gain x (A + B + C) / 3: x 1.2 makes each SSG channel 0.40
                    let v = self.ssg.out[before] * 1.2 + fm / 32768.0 * 0.2;
                    self.ssg.out.truncate(before);
                    self.out.push(v);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The SSG part plays a tone, the FM part a note: both come out, at the output rate.
    #[test]
    fn ssg_and_fm_both_sound() {
        let mut y = Ym2203::new(1_500_000, 44100);
        // SSG channel A: period 0x100, volume 15, tone only
        for (r, v) in [(0x00, 0x00), (0x01, 0x01), (0x07, 0x3E), (0x08, 0x0F)] { y.write(0, r); y.write(1, v); }
        y.run(1_500_000 / 10);
        let n = y.out.len();
        assert!((4400..=4420).contains(&n), "a tenth of a second at 44.1 kHz: {n}");
        let ssg_peak = y.out.iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!(ssg_peak > 0.1, "SSG tone: {ssg_peak}");
        y.out.clear();
        // silence the SSG; FM channel 1: algorithm 7 (all carriers), operators at full level,
        // fast attack, key on
        y.write(0, 0x08); y.write(1, 0);
        for op in [0x00, 0x04, 0x08, 0x0C] {
            for (r, v) in [(0x30, 0x01), (0x40, 0x00), (0x50, 0x1F), (0x60, 0x00), (0x70, 0x00), (0x80, 0x0F)] {
                y.write(0, r + op); y.write(1, v);
            }
        }
        for (r, v) in [(0xB0, 0x07), (0xA4, 0x22), (0xA0, 0x69), (0x28, 0xF0)] { y.write(0, r); y.write(1, v); }
        y.run(1_500_000 / 10);
        let fm_peak = y.out.iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!(fm_peak > 0.05, "FM note: {fm_peak}");
    }

    /// Timer A runs in FM samples (prescale 6: 72 clocks each) and raises its flag.
    #[test]
    fn timer_a_flag() {
        let mut y = Ym2203::new(1_500_000, 44100);
        // period 1024 - 1000 = 24 samples; load and enable the flag
        for (r, v) in [(0x24, (1000 >> 2) as u8), (0x25, (1000 & 3) as u8), (0x27, 0x05)] { y.write(0, r); y.write(1, v); }
        y.run(72 * 20);
        assert_eq!(y.read(0, 0, 0) & 1, 0, "not yet");
        y.run(72 * 6);
        assert_eq!(y.read(0, 0, 0) & 1, 1, "after 24 samples");
        assert!(y.irq());
    }

    /// The FM part against ymfm's ym2203 itself (tools: scratch ym2203_ref): `w` writes,
    /// `s <count> <hash>` = FNV-1a over the next <count> FM samples.
    #[test]
    fn fm_matches_ymfm() {
        let mut y = Ym2203::new(1_500_000, 44100);
        let mut n = 0;
        for (lineno, line) in include_str!("../tests/data/ym2203_vectors.txt").lines().enumerate() {
            let p: Vec<&str> = line.split_whitespace().collect();
            match p.first() {
                Some(&"w") => y.write(u8::from_str_radix(p[1], 16).unwrap(), u8::from_str_radix(p[2], 16).unwrap()),
                Some(&"s") => {
                    let count: usize = p[1].parse().unwrap();
                    let want = u64::from_str_radix(p[2], 16).unwrap();
                    let mut h: u64 = 0xcbf29ce484222325;
                    for _ in 0..count { h = (h ^ (y.fm.generate_opn() as u32 as u64)).wrapping_mul(0x100000001b3); }
                    assert_eq!(h, want, "line {}: sample block differs", lineno + 1);
                    n += count;
                }
                _ => {}
            }
        }
        assert!(n > 100_000, "{n} samples");
    }

    #[test]
    fn roundtrip_fp_keeps_10_bits() {
        use crate::common::audio::ym2612::roundtrip_fp;
        assert_eq!(roundtrip_fp(1000), 1000);
        assert_eq!(roundtrip_fp(5001), 4992);
        assert_eq!(roundtrip_fp(-5001), -5008);
        assert_eq!(roundtrip_fp(40000), 32767);
    }
}
