//! CPS1 sound board. After MAME capcom/cps1.cpp sub_map (BSD-3-Clause).
//!
//! Z80 at 3.579545 MHz; YM2151 on the same clock (its IRQ drives the Z80 INT line);
//! OKI MSM6295 at 1 MHz (16 MHz / 16), pin 7 high at power on.
//!
//!   0000-7FFF ROM   8000-BFFF bank (2 x 16K from ROM offset 10000, F004 bit 0)
//!   D000-D7FF RAM   F000-F001 YM2151   F002 OKI   F004 W bank   F006 W OKI pin 7
//!   F008 R sound command   F00A R second latch ("timer fade")
//!
//! The Z80 polls both latches: no NMI, nothing is cleared on a read.

use serde::{Deserialize, Serialize};

use crate::common::audio::okim6295::Okim6295;
use crate::common::audio::ym2151::Ym2151;
use crate::common::cpu::z80::{Bus, Z80Cpu, Z80State};

pub const CLOCK: u64 = 3_579_545;
const OKI_CLOCK: u64 = 1_000_000;
const OUT_RATE: u64 = 44_100;
const YM_BUSY: u32 = 64;
/// MAME's route levels
const GAIN_YM: f32 = 0.35;
const GAIN_OKI: f32 = 0.30;

#[derive(Serialize, Deserialize)]
pub struct SoundBus {
    #[serde(skip)] rom: Vec<u8>,
    #[serde(with = "serde_bytes")]
    ram: Vec<u8>,
    pub latch: [u8; 2],
    bank: u8,
    pub ym: Ym2151,
    ym_busy: u32,
    pub oki: Okim6295,
}

impl SoundBus {
    #[cfg(test)]
    pub fn ram_byte(&self, a: u16) -> u8 { self.ram[(a - 0xD000) as usize] }
}

impl Bus for SoundBus {
    fn read(&mut self, a: u16) -> u8 {
        match a {
            0x0000..=0x7FFF => self.rom.get(a as usize).copied().unwrap_or(0xFF),
            0x8000..=0xBFFF => {
                let o = 0x10000 + self.bank as usize * 0x4000 + (a as usize - 0x8000);
                self.rom.get(o).copied().unwrap_or(0xFF)
            }
            0xD000..=0xD7FF => self.ram[(a - 0xD000) as usize],
            0xF001 => self.ym.status() | if self.ym_busy > 0 { 0x80 } else { 0 },
            0xF000 => self.ym.status() | if self.ym_busy > 0 { 0x80 } else { 0 },
            0xF002 => self.oki.read(),
            0xF008 => self.latch[0],
            0xF00A => self.latch[1],
            _ => 0xFF,
        }
    }

    fn write(&mut self, a: u16, v: u8) {
        match a {
            0xD000..=0xD7FF => self.ram[(a - 0xD000) as usize] = v,
            0xF000..=0xF001 => {
                self.ym.write((a & 1) as u8, v);
                if a & 1 != 0 { self.ym_busy = YM_BUSY; }
            }
            0xF002 => self.oki.write(v),
            0xF004 => self.bank = v & 1,
            0xF006 => self.oki.pin7_high = v & 1 != 0,
            _ => {}
        }
    }

    fn port_in(&mut self, _port: u16) -> u8 { 0xFF }
    fn port_out(&mut self, _port: u16, _val: u8) {}
}

pub struct Sound {
    cpu: Z80Cpu,
    pub bus: SoundBus,
    owed: i64,
    phase: u64,
    ym_div: u32,
    ym_out: f32,
    oki_phase: u64,
    oki_out: f32,
    resample: u64,
    acc: f32,
    acc_n: u32,
    pub out: Vec<f32>,
}

#[derive(Serialize)]
pub struct SoundStateRef<'a> {
    cpu: Z80State, bus: &'a SoundBus, owed: i64, phase: u64, ym_div: u32, ym_out: f32,
    oki_phase: u64, oki_out: f32, resample: u64,
}

#[derive(Deserialize)]
pub struct SoundState {
    cpu: Z80State, bus: SoundBus, owed: i64, phase: u64, ym_div: u32, ym_out: f32,
    oki_phase: u64, oki_out: f32, resample: u64,
}

impl Sound {
    pub fn new(rom: Vec<u8>, oki_rom: Vec<u8>) -> Self {
        let mut ym = Ym2151::new();
        ym.set_running(true);
        Self {
            cpu: Z80Cpu::new(),
            bus: SoundBus {
                rom, ram: vec![0; 0x800], latch: [0; 2], bank: 0, ym, ym_busy: 0,
                oki: Okim6295::new(oki_rom, true),
            },
            owed: 0, phase: 0, ym_div: 0, ym_out: 0.0, oki_phase: 0, oki_out: 0.0,
            resample: 0, acc: 0.0, acc_n: 0, out: Vec::new(),
        }
    }

    pub fn save(&self) -> SoundStateRef<'_> {
        SoundStateRef {
            cpu: self.cpu.save_state(), bus: &self.bus, owed: self.owed, phase: self.phase,
            ym_div: self.ym_div, ym_out: self.ym_out, oki_phase: self.oki_phase,
            oki_out: self.oki_out, resample: self.resample,
        }
    }

    pub fn load(&mut self, s: SoundState) {
        let mut bus = s.bus;
        bus.rom = std::mem::take(&mut self.bus.rom);
        bus.oki.keep_static(&mut self.bus.oki);
        self.bus = bus;
        self.cpu.load_state(&s.cpu);
        self.owed = s.owed;
        self.phase = s.phase;
        self.ym_div = s.ym_div;
        self.ym_out = s.ym_out;
        self.oki_phase = s.oki_phase;
        self.oki_out = s.oki_out;
        self.resample = s.resample;
    }

    pub fn reset(&mut self) {
        let rom = std::mem::take(&mut self.bus.rom);
        let mut fresh = Self::new(rom, Vec::new());
        fresh.bus.oki.keep_static(&mut self.bus.oki);
        fresh.out = std::mem::take(&mut self.out);
        *self = fresh;
    }

    /// Runs `num / den` seconds of the sound board.
    pub fn run(&mut self, num: u64, den: u64) {
        self.phase += num * CLOCK;
        self.owed += (self.phase / den) as i64;
        self.phase %= den;
        while self.owed > 0 {
            let mut c = 0;
            // the YM2151 IRQ is a level on INT (IM 1)
            if self.bus.ym.status() & 0x03 != 0 {
                c = self.cpu.irq(&mut self.bus, 0xFF);
            }
            if c == 0 { c = self.cpu.step(&mut self.bus); }
            self.owed -= c as i64;
            for _ in 0..c { self.tick(); }
        }
    }

    fn tick(&mut self) {
        let b = &mut self.bus;
        b.ym_busy = b.ym_busy.saturating_sub(1);
        self.ym_div += 1;
        if self.ym_div == 64 {
            self.ym_div = 0;
            let (l, r) = b.ym.generate();
            self.ym_out = (l + r) as f32 * (GAIN_YM / 32768.0);
        }
        // OKI sample rate = clock / 132 (pin 7 high) or / 165
        let div = if b.oki.pin7_high { 132 } else { 165 };
        self.oki_phase += OKI_CLOCK;
        if self.oki_phase >= CLOCK * div {
            self.oki_phase -= CLOCK * div;
            self.oki_out = b.oki.sample() * GAIN_OKI;
        }
        self.acc += self.ym_out + self.oki_out;
        self.acc_n += 1;
        self.resample += OUT_RATE;
        if self.resample >= CLOCK {
            self.resample -= CLOCK;
            let s = (self.acc / self.acc_n as f32).clamp(-1.0, 1.0);
            self.out.push(s);
            self.out.push(s);
            self.acc = 0.0;
            self.acc_n = 0;
        }
    }
}
