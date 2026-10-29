//! Williams video sound board (D-8224 / D-9894): a 6808 at 3.579545 MHz / 4, 256 bytes of
//! RAM, a 6821 PIA and an MC1408 8-bit DAC on PIA port A. The game writes a 6-bit command
//! to its own PIA; it arrives on port B with the top two bits set, and CB1 goes high for
//! anything but FF (the idle value), which interrupts the sound CPU.
//! Sinistar adds an HC-55516 speech decoder: data on PIA CA2, clock on CB2.
//! After MAME williams.cpp (sound_map) and williams_m.cpp (snd_cmd_w).

use serde::{Deserialize, Serialize};
use crate::common::cpu::m6801::{M6801, M6801Bus, Model};
use crate::common::io::pia6821::{Pia6821, PiaOut};
use crate::common::audio::hc55516::Hc55516;

pub const CLOCK: u32 = 3_579_545 / 4;
const OUT_RATE: u32 = 44_100;

struct Mem<'a> {
    ram: &'a mut [u8],
    rom: &'a [u8],
    pia: &'a mut Pia6821,
    cmd: u8,
    dac: &'a mut u8,
}

impl M6801Bus for Mem<'_> {
    fn read(&mut self, a: u16) -> u8 {
        match a {
            0x0000..=0x00FF => self.ram[a as usize],
            _ if a & 0x7FFC == 0x0400 => self.pia.read(a, 0xFF, self.cmd),
            0xB000..=0xFFFF => self.rom[(a as usize - 0xB000) % self.rom.len()],
            _ => 0,
        }
    }
    fn write(&mut self, a: u16, v: u8) {
        match a {
            0x0000..=0x00FF => self.ram[a as usize] = v,
            _ if a & 0x7FFC == 0x0400 => {
                if let PiaOut::A(d) = self.pia.write(a, v) { *self.dac = d; }
            }
            _ => {}
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct WilliamsSound {
    cpu: M6801,
    ram: Vec<u8>,
    #[serde(skip)]
    rom: Vec<u8>,
    pia: Pia6821,
    cmd: u8,
    dac: u8,
    /// CPU cycles still owed to the next line, and the 44.1 kHz resampler state
    owed: i64,
    phase: u64,
    acc: f32,
    acc_n: u32,
    resample: u32,
    dc: (f32, f32),
    cvsd: Option<Hc55516>,
    /// last CA2 / CB2 levels seen (the CVSD's data and clock)
    c2: (bool, bool),
    #[serde(skip)]
    pub out: Vec<f32>,
}

impl WilliamsSound {
    /// `rom`: the B000-FFFF part of the sound region (or the whole 64 KB region).
    pub fn new(rom: Vec<u8>) -> Self { Self::with_cvsd(rom, false) }

    pub fn with_cvsd(rom: Vec<u8>, speech: bool) -> Self {
        let rom = if rom.len() == 0x10000 { rom[0xB000..].to_vec() } else { rom };
        let mut s = Self {
            cpu: M6801::with_model(Model::M6800),
            ram: vec![0; 0x100],
            rom,
            pia: Pia6821::new(),
            cmd: 0,
            dac: 0x80,
            owed: 0,
            phase: 0,
            acc: 0.0,
            acc_n: 0,
            resample: 0,
            dc: (0.0, 0.0),
            cvsd: if speech { Some(Hc55516::new()) } else { None },
            c2: (true, false),
            out: Vec::new(),
        };
        s.reset();
        s
    }

    pub fn reset(&mut self) {
        self.pia.reset();
        if let Some(c) = self.cvsd.as_mut() { *c = Hc55516::new(); }
        self.c2 = (true, false);
        let Self { cpu, ram, rom, pia, cmd, dac, .. } = self;
        let mut m = Mem { ram, rom, pia, cmd: *cmd, dac };
        *cpu = M6801::with_model(Model::M6800);
        cpu.reset(&mut m);
    }

    /// Keep the ROM when loading a state.
    pub fn keep_rom(&mut self, old: &mut WilliamsSound) { self.rom = std::mem::take(&mut old.rom); }

    /// The main board's PIA port B output.
    pub fn command(&mut self, data: u8) {
        self.cmd = data | 0xC0;
        self.pia.cb1_w(self.cmd != 0xFF);
    }

    /// Run for `num / per` seconds' worth of sound-CPU cycles.
    pub fn run(&mut self, num: u64, per: u64) {
        self.phase += num * CLOCK as u64;
        self.owed += (self.phase / per) as i64;
        self.phase %= per;
        while self.owed > 0 {
            self.cpu.irq_line = self.pia.irq();
            let c = {
                let Self { cpu, ram, rom, pia, cmd, dac, .. } = self;
                let mut m = Mem { ram, rom, pia, cmd: *cmd, dac };
                cpu.step(&mut m)
            };
            self.owed -= c as i64;
            let mut speech = 0.0;
            if let Some(cv) = self.cvsd.as_mut() {
                let lines = (self.pia.ca2(), self.pia.cb2());
                if lines.0 != self.c2.0 { cv.digit_w(lines.0); }
                if lines.1 != self.c2.1 { cv.digit_w(lines.0); cv.clock_w(lines.1); }
                self.c2 = lines;
                // MAME: DAC at 0.25, speech at 0.8
                speech = cv.sample() as f32 / 32768.0 * 3.2;
            }
            // box-filter the DAC (and speech) into 44.1 kHz samples
            let level = (self.dac as f32 - 128.0) / 128.0 + speech;
            for _ in 0..c {
                self.acc += level;
                self.acc_n += 1;
                self.resample += OUT_RATE;
                if self.resample >= CLOCK {
                    self.resample -= CLOCK;
                    let x = self.acc / self.acc_n as f32;
                    self.acc = 0.0;
                    self.acc_n = 0;
                    // DC blocker (~20 Hz)
                    let y = x - self.dc.0 + 0.997 * self.dc.1;
                    self.dc = (x, y);
                    let v = (y * 0.5).clamp(-1.0, 1.0);
                    self.out.push(v);
                    self.out.push(v);
                }
            }
        }
    }

    #[cfg(test)]
    pub fn dac(&self) -> u8 { self.dac }
}
