//! Return of the Jedi sound board: a 6502 at 12.096 MHz / 8 (1.512 MHz), four POKEYs at
//! the same clock and a TMS5220 at 12.096 MHz / 18. After MAME jedi.cpp (audio_map).
//!
//! 0000-07FF RAM; 0800-083F POKEYs (mirrored through 0FFF); 1000 IRQ ack; 1100 speech
//! data; 1200 / 1300 speech /WS low / high; 1400 reply latch to the main CPU; 1500 bit 0
//! speech power; 1800 command latch from the main CPU; 1C00 bit 7 speech /READY;
//! 1C01 bits 7-6 latch flags; 8000-FFFF ROM. Mix: POKEYs 1 and 2 to both sides,
//! 3 left, 4 right (0.30 each); speech to both.

use serde::{Deserialize, Serialize};
use crate::common::audio::pokey::Pokey;
use crate::common::audio::tms5220::Tms5220;
use crate::common::cpu::m6502::{AddressBus, CpuState, CpuVariant, M6502Cpu};

pub const CLOCK: u64 = 12_096_000 / 8;
const TMS_CLOCK: u64 = 12_096_000 / 18;
const OUT_RATE: u64 = 44_100;

/// The two latches between the CPUs (MAME generic_latch_8: a write sets "pending",
/// the other side's read clears it).
#[derive(Clone, Copy, Default, Serialize, Deserialize)]
pub struct Latch { pub value: u8, pub pending: bool }

impl Latch {
    pub fn write(&mut self, v: u8) { self.value = v; self.pending = true; }
    pub fn read(&mut self) -> u8 { self.pending = false; self.value }
}

#[derive(Serialize, Deserialize)]
pub struct SoundBus {
    ram: Vec<u8>,
    #[serde(skip)]
    rom: Vec<u8>,
    pub pokey: [Pokey; 4],
    pub tms: Tms5220,
    speech_on: bool,
    /// main -> sound (soundlatch) and sound -> main (sacklatch)
    pub cmd: Latch,
    pub ack: Latch,
    pub irq: bool,
}

impl AddressBus for SoundBus {
    fn read_byte(&mut self, a: u16) -> u8 {
        match a {
            0x0000..=0x07FF => self.ram[a as usize],
            0x0800..=0x0FFF => self.pokey[((a >> 4) & 3) as usize].read((a & 0x0F) as u8),
            0x1800..=0x1BFF => self.cmd.read(),
            0x1C00..=0x1FFF if a & 1 == 0 => (self.tms.readyq() as u8) << 7,
            0x1C00..=0x1FFF => (self.cmd.pending as u8) << 7 | (self.ack.pending as u8) << 6,
            0x8000..=0xFFFF => self.rom[(a as usize - 0x8000) % self.rom.len().max(1)],
            _ => 0,
        }
    }

    fn write_byte(&mut self, a: u16, v: u8) {
        match a {
            0x0000..=0x07FF => self.ram[a as usize] = v,
            0x0800..=0x0FFF => self.pokey[((a >> 4) & 3) as usize].write((a & 0x0F) as u8, v),
            0x1000..=0x10FF => self.irq = false,
            0x1100..=0x11FF => self.tms.data_w(v),
            0x1200..=0x13FF => self.tms.wsq_w(a & 0x100 != 0),
            0x1400..=0x14FF => self.ack.write(v),
            0x1500..=0x15FF => self.speech_on = v & 1 != 0,
            _ => {}
        }
    }

    fn is_nmi_line_asserted(&mut self) -> bool { false }
    fn is_irq_line_asserted(&mut self) -> bool { self.irq }
    fn is_nmi_enabled(&mut self) -> bool { true }
    fn step_cycles(&mut self, _c: u64) {}
    fn begin_cpu_cycle(&mut self) {}
}

pub struct JediSound {
    cpu: M6502Cpu,
    pub bus: SoundBus,
    /// main board's LS259 Q6: low holds the CPU in reset
    running: bool,
    tms_phase: u64,
    tms_out: f32,
    resample: u64,
    acc: [f32; 2],
    count: u32,
    dc_in: [f32; 2],
    dc_out: [f32; 2],
    pub out: Vec<f32>,
}

#[derive(Serialize, Deserialize)]
pub struct JediSoundState {
    cpu: CpuState,
    bus: SoundBus,
    running: bool,
    tms_phase: u64,
    tms_out: f32,
}

impl JediSound {
    pub fn new(rom: Vec<u8>) -> Self {
        let rom = if rom.len() == 0x10000 { rom[0x8000..].to_vec() } else { rom };
        let mut s = Self {
            cpu: M6502Cpu::new(CpuVariant::NMOS6502),
            bus: SoundBus {
                ram: vec![0; 0x800], rom,
                pokey: [Pokey::new(), Pokey::new(), Pokey::new(), Pokey::new()],
                tms: Tms5220::new(), speech_on: false,
                cmd: Latch::default(), ack: Latch::default(), irq: false,
            },
            running: false,
            tms_phase: 0, tms_out: 0.0,
            resample: 0, acc: [0.0; 2], count: 0, dc_in: [0.0; 2], dc_out: [0.0; 2],
            out: Vec::new(),
        };
        s.cpu.power_on(&mut s.bus);
        s
    }

    /// Main board LS259 Q6 (MAME audio_reset_w): 0 holds the sound CPU in reset and
    /// silences the speech.
    pub fn set_running(&mut self, run: bool) {
        if run && !self.running { self.cpu.reset(&mut self.bus); }
        if !run { self.bus.speech_on = false; }
        self.running = run;
    }

    /// One sound-CPU clock (also a POKEY clock).
    pub fn cycle(&mut self) {
        if self.running {
            self.cpu.step_one_cycle(&mut self.bus);
            self.cpu.sample_interrupt_lines(&mut self.bus);
        }
        for p in self.bus.pokey.iter_mut() { p.step(); }
        self.tms_phase += TMS_CLOCK;
        while self.tms_phase >= CLOCK {
            self.tms_phase -= CLOCK;
            if let Some(s) = self.bus.tms.clock() { self.tms_out = s as f32 / 32768.0; }
        }
        let pk = &self.bus.pokey;
        let p = [pk[0].output() * 0.30, pk[1].output() * 0.30, pk[2].output() * 0.30, pk[3].output() * 0.30];
        let speech = if self.bus.speech_on { self.tms_out } else { 0.0 };
        self.acc[0] += p[0] + p[1] + p[2] + speech;
        self.acc[1] += p[0] + p[1] + p[3] + speech;
        self.count += 1;
        self.resample += OUT_RATE;
        if self.resample >= CLOCK {
            self.resample -= CLOCK;
            for side in 0..2 {
                let x = self.acc[side] / self.count as f32;
                // DC blocker: POKEY output is unipolar
                let y = x - self.dc_in[side] + 0.995 * self.dc_out[side];
                self.dc_in[side] = x;
                self.dc_out[side] = y;
                self.out.push((y * 0.9).clamp(-1.0, 1.0));
            }
            self.acc = [0.0; 2];
            self.count = 0;
        }
    }

    pub fn save(&self) -> JediSoundState {
        JediSoundState {
            cpu: self.cpu.get_state(),
            bus: SoundBus {
                ram: self.bus.ram.clone(), rom: Vec::new(), pokey: self.bus.pokey.clone(),
                tms: self.bus.tms.clone(), speech_on: self.bus.speech_on,
                cmd: self.bus.cmd, ack: self.bus.ack, irq: self.bus.irq,
            },
            running: self.running,
            tms_phase: self.tms_phase,
            tms_out: self.tms_out,
        }
    }

    pub fn load(&mut self, s: JediSoundState) {
        let mut bus = s.bus;
        bus.rom = std::mem::take(&mut self.bus.rom);
        self.bus = bus;
        self.cpu.load_state(&s.cpu);
        self.running = s.running;
        self.tms_phase = s.tms_phase;
        self.tms_out = s.tms_out;
    }
}
