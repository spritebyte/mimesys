//! Double Dragon II sound board (MAME ddragon2_sound_map): Z80 @ 3.579545 MHz,
//! YM2151 on the same clock (IRQ -> INT), OKI MSM6295 @ 1.056 MHz pin 7 high (8 kHz),
//! sound latch from the main CPU (pending -> NMI).
//!
//!   0000-7FFF ROM   8000-87FF RAM   8800-8801 YM2151   9800 OKI   A000 R latch

use serde::{Deserialize, Serialize};

use crate::common::audio::okim6295::Okim6295;
use crate::common::audio::ym2151::Ym2151;
use crate::common::cpu::z80::Bus;

pub const SOUND_CLOCK: f64 = 3_579_545.0;
const OKI_CLOCK: f64 = 1_056_000.0;
const OUT_RATE: f64 = 44_100.0;

#[derive(Serialize, Deserialize)]
pub struct Dd2Sound {
    #[serde(skip)] rom: Vec<u8>,
    ram: Vec<u8>,
    pub latch: u8,
    pub latch_pending: bool,
    pub ym: Ym2151,
    ym_busy: u32,
    ym_div: u32,
    ym_out: f32,
    pub oki: Okim6295,
    oki_phase: f64,
    oki_out: f32,
    out_phase: f64,
    acc: f32,
    acc_n: u32,
    #[serde(skip)] pub out: Vec<f32>,
}

impl Dd2Sound {
    pub fn new(rom: Vec<u8>, oki_rom: Vec<u8>) -> Self {
        let mut ym = Ym2151::new();
        ym.set_running(true);
        Self {
            rom, ram: vec![0; 0x800], latch: 0, latch_pending: false,
            ym, ym_busy: 0, ym_div: 0, ym_out: 0.0,
            oki: Okim6295::new(oki_rom, true), oki_phase: 0.0, oki_out: 0.0,
            out_phase: 0.0, acc: 0.0, acc_n: 0, out: Vec::new(),
        }
    }

    pub fn keep_static(&mut self, old: &mut Self) {
        self.rom = std::mem::take(&mut old.rom);
        self.oki.keep_static(&mut old.oki);
        self.out = std::mem::take(&mut old.out);
    }

    pub fn reset(&mut self) {
        self.latch_pending = false;
        self.ym.reset();
        self.ym_busy = 0;
        self.oki.reset();
    }

    pub fn nmi(&self) -> bool { self.latch_pending }
    pub fn irq(&self) -> bool { self.ym.status() & 0x03 != 0 }

    pub fn write_latch(&mut self, v: u8) {
        self.latch = v;
        self.latch_pending = true;
    }

    /// Runs the sound chips for `cycles` Z80 cycles, making output samples.
    pub fn advance(&mut self, cycles: u32) {
        let oki_step = self.oki.sample_rate(OKI_CLOCK) / SOUND_CLOCK;
        let out_step = OUT_RATE / SOUND_CLOCK;
        for _ in 0..cycles {
            self.ym_busy = self.ym_busy.saturating_sub(1);
            // the YM2151 shares the Z80's clock: one sample every 64 clocks
            self.ym_div += 1;
            if self.ym_div == 64 {
                self.ym_div = 0;
                let (l, r) = self.ym.generate();
                self.ym_out = (l + r) as f32 * (0.35 / 32768.0);
            }
            self.oki_phase += oki_step;
            if self.oki_phase >= 1.0 {
                self.oki_phase -= 1.0;
                self.oki_out = self.oki.sample() * 0.20;
            }
            self.acc += self.ym_out + self.oki_out;
            self.acc_n += 1;
            self.out_phase += out_step;
            if self.out_phase >= 1.0 {
                self.out_phase -= 1.0;
                let s = (self.acc / self.acc_n as f32 * 0.6).clamp(-1.0, 1.0);   // same overall level as DD1
                self.out.push(s);
                self.out.push(s);
                self.acc = 0.0;
                self.acc_n = 0;
            }
        }
    }
}

impl Bus for Dd2Sound {
    fn read(&mut self, a: u16) -> u8 {
        match a {
            0x0000..=0x7FFF => self.rom.get(a as usize).copied().unwrap_or(0xFF),
            0x8000..=0x87FF => self.ram[(a & 0x7FF) as usize],
            0x8800..=0x8801 => {
                if a & 1 == 0 { 0xFF } else { self.ym.status() | if self.ym_busy > 0 { 0x80 } else { 0 } }
            }
            0x9800 => self.oki.read(),
            0xA000 => { self.latch_pending = false; self.latch }
            _ => 0xFF,
        }
    }

    fn write(&mut self, a: u16, v: u8) {
        match a {
            0x8000..=0x87FF => self.ram[(a & 0x7FF) as usize] = v,
            0x8800..=0x8801 => {
                self.ym.write((a & 1) as u8, v);
                if a & 1 != 0 { self.ym_busy = 64; }
            }
            0x9800 => self.oki.write(v),
            _ => {}
        }
    }

    fn port_in(&mut self, _port: u16) -> u8 { 0xFF }
    fn port_out(&mut self, _port: u16, _val: u8) {}
}
