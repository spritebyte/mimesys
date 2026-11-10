//! CPS1 Q-Sound board (Warriors of Fate, Cadillacs and Dinosaurs, Punisher, Slam Masters,
//! Muscle Bomber Duo) and the CPS2 sound section, which is the same minus the Kabuki.
//! After MAME capcom/cps1.cpp qsound_sub_map (BSD-3-Clause).
//!
//! Kabuki Z80 at 8 MHz with a 250 Hz IRQ (held until taken). There are no sound latches:
//! the 68000 and the Z80 share two RAMs (the 68000 sees one byte per word).
//!
//!   0000-7FFF ROM (Kabuki: separate opcode / data views)   8000-BFFF bank (D003, 16K
//!   pages from ROM offset 10000)   C000-CFFF shared RAM 0   D000-D002 W Q-Sound
//!   D003 W bank   D007 R Q-Sound ready   F000-FFFF shared RAM 1

use serde::{Deserialize, Serialize};

use super::kabuki::{self, Key};
use crate::common::audio::qsound::{self, QSound};
use crate::common::cpu::z80::{Bus, Z80Cpu, Z80State};

pub const CLOCK: u64 = 8_000_000;
/// MAME: 8 MHz / 32000 = 250 Hz
const IRQ_PERIOD: u64 = 32_000;
const OUT_RATE: u64 = 44_100;

#[derive(Serialize, Deserialize)]
pub struct QBus {
    #[serde(skip)] rom: Vec<u8>,
    #[serde(skip)] ops: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub shared0: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub shared1: Vec<u8>,
    bank: u8,
    pub qs: QSound,
}

impl QBus {
    fn bank_base(&self) -> usize {
        let b = (self.bank & 0x0F) as usize;
        // MAME: a bank past the end of the ROM falls back to bank 0
        if 0x10000 + b * 0x4000 >= self.rom.len() { 0x10000 } else { 0x10000 + b * 0x4000 }
    }
}

impl Bus for QBus {
    fn read(&mut self, a: u16) -> u8 {
        match a {
            0x0000..=0x7FFF => self.rom.get(a as usize).copied().unwrap_or(0xFF),
            0x8000..=0xBFFF => self.rom.get(self.bank_base() + (a as usize - 0x8000)).copied().unwrap_or(0xFF),
            0xC000..=0xCFFF => self.shared0[(a - 0xC000) as usize],
            0xD007 => self.qs.read(),
            0xF000..=0xFFFF => self.shared1[(a - 0xF000) as usize],
            _ => 0xFF,
        }
    }

    fn read_opcode(&mut self, a: u16) -> u8 {
        if a < 0x8000 && !self.ops.is_empty() { self.ops[a as usize] } else { self.read(a) }
    }

    fn write(&mut self, a: u16, v: u8) {
        match a {
            0xC000..=0xCFFF => self.shared0[(a - 0xC000) as usize] = v,
            0xD000..=0xD002 => self.qs.write((a - 0xD000) as u8, v),
            0xD003 => self.bank = v,
            0xF000..=0xFFFF => self.shared1[(a - 0xF000) as usize] = v,
            _ => {}
        }
    }

    fn port_in(&mut self, _port: u16) -> u8 { 0xFF }
    fn port_out(&mut self, _port: u16, _val: u8) {}
}

pub struct QSoundBoard {
    cpu: Z80Cpu,
    pub bus: QBus,
    owed: i64,
    phase: u64,
    irq_count: u64,
    irq: bool,
    /// CPS2: the 68000 holds the Z80 in reset (port 804040 bit 3 low)
    pub in_reset: bool,
    dsp_phase: u64,
    /// last two DSP samples and the position between them, for the 44.1 kHz output
    prev: [f32; 2],
    cur: [f32; 2],
    out_phase: u64,
    pub out: Vec<f32>,
}

#[derive(Serialize)]
pub struct QStateRef<'a> {
    cpu: Z80State, bus: &'a QBus, owed: i64, phase: u64, irq_count: u64, irq: bool, in_reset: bool,
    dsp_phase: u64, prev: [f32; 2], cur: [f32; 2], out_phase: u64,
}

#[derive(Deserialize)]
pub struct QState {
    cpu: Z80State, bus: QBus, owed: i64, phase: u64, irq_count: u64, irq: bool, in_reset: bool,
    dsp_phase: u64, prev: [f32; 2], cur: [f32; 2], out_phase: u64,
}

impl QSoundBoard {
    /// `rom` = the audiocpu region (still encrypted when there's a Kabuki key), `samples`
    /// = the qsound region, `dsp_rom` = dl-1425.bin.
    pub fn new(mut rom: Vec<u8>, key: Option<Key>, samples: Vec<u8>, dsp_rom: &[u8]) -> Self {
        let ops = match key { Some(k) => kabuki::decode(&mut rom, k), None => Vec::new() };
        Self {
            cpu: Z80Cpu::new(),
            bus: QBus {
                rom, ops, shared0: vec![0; 0x1000], shared1: vec![0; 0x1000], bank: 0,
                qs: QSound::new(samples, dsp_rom),
            },
            owed: 0, phase: 0, irq_count: 0, irq: false, in_reset: false, dsp_phase: 0,
            prev: [0.0; 2], cur: [0.0; 2], out_phase: 0, out: Vec::new(),
        }
    }

    pub fn save(&self) -> QStateRef<'_> {
        QStateRef {
            cpu: self.cpu.save_state(), bus: &self.bus, owed: self.owed, phase: self.phase,
            irq_count: self.irq_count, irq: self.irq, in_reset: self.in_reset, dsp_phase: self.dsp_phase,
            prev: self.prev, cur: self.cur, out_phase: self.out_phase,
        }
    }

    pub fn load(&mut self, s: QState) {
        let mut bus = s.bus;
        bus.rom = std::mem::take(&mut self.bus.rom);
        bus.ops = std::mem::take(&mut self.bus.ops);
        bus.qs.keep_static(&mut self.bus.qs);
        self.bus = bus;
        self.cpu.load_state(&s.cpu);
        self.owed = s.owed;
        self.phase = s.phase;
        self.irq_count = s.irq_count;
        self.irq = s.irq;
        self.in_reset = s.in_reset;
        self.dsp_phase = s.dsp_phase;
        self.prev = s.prev;
        self.cur = s.cur;
        self.out_phase = s.out_phase;
    }

    pub fn reset(&mut self) {
        self.cpu = Z80Cpu::new();
        self.bus.shared0.fill(0);
        self.bus.shared1.fill(0);
        self.bus.bank = 0;
        self.bus.qs.reset();
        self.owed = 0;
        self.irq_count = 0;
        self.irq = false;
    }

    /// CPS2 port 804040 bit 3: reset line (held while low). Releasing it restarts the Z80.
    pub fn set_reset(&mut self, on: bool) {
        if on && !self.in_reset { self.cpu = Z80Cpu::new(); self.irq = false; }
        self.in_reset = on;
    }

    /// Runs `num / den` seconds.
    pub fn run(&mut self, num: u64, den: u64) {
        self.phase += num * CLOCK;
        self.owed += (self.phase / den) as i64;
        self.phase %= den;
        if self.in_reset {
            // the Q-Sound chip keeps running; the Z80 doesn't
            let c = self.owed.max(0) as u64;
            self.owed = 0;
            self.clocks(c);
            self.irq = false;
            return;
        }
        while self.owed > 0 {
            let mut c = 0;
            if self.irq {
                c = self.cpu.irq(&mut self.bus, 0xFF);
                if c > 0 { self.irq = false; }   // HOLD_LINE: cleared when taken
            }
            if c == 0 { c = self.cpu.step(&mut self.bus); }
            self.owed -= c as i64;
            self.clocks(c as u64);
        }
    }

    fn clocks(&mut self, c: u64) {
        self.irq_count += c;
        if self.irq_count >= IRQ_PERIOD {
            self.irq_count -= IRQ_PERIOD;
            self.irq = true;
        }
        // Q-Sound samples: 60 MHz / 2496 per sample, counted in Z80 clocks
        self.dsp_phase += c * qsound::CLOCK;
        let per_sample = CLOCK * qsound::CYCLES_PER_SAMPLE;
        while self.dsp_phase >= per_sample {
            self.dsp_phase -= per_sample;
            self.bus.qs.update();
            self.prev = self.cur;
            self.cur = [self.bus.qs.out[0] as f32 / 32768.0, self.bus.qs.out[1] as f32 / 32768.0];
            // output samples falling between the last two DSP samples (linear)
            // time in units of 1 / (60 MHz x 44100) s: a DSP sample lasts 2496 x 44100,
            // an output sample 60 M
            let dsp_period = qsound::CYCLES_PER_SAMPLE * OUT_RATE;
            self.out_phase += dsp_period;
            while self.out_phase >= qsound::CLOCK {
                self.out_phase -= qsound::CLOCK;
                // how far the output point lies before the newest DSP sample
                let t = self.out_phase as f32 / dsp_period as f32;
                for ch in 0..2 {
                    let v = self.cur[ch] + (self.prev[ch] - self.cur[ch]) * t;
                    self.out.push(v.clamp(-1.0, 1.0));
                }
            }
        }
    }
}
