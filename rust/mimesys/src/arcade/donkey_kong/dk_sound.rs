//! Donkey Kong sound board: an MB8884 (Intel 8035 clone, MCS-48 family) that
//! plays music and Kong's sounds through an 8-bit DAC, plus analog circuits for
//! walk / jump / stomp.

pub use crate::common::cpu::mcs48::{Mcs48, Mcs48Io};

pub trait SoundCpu: Send {
    /// Run one instruction (taking an interrupt first if one is due).
    /// Returns machine cycles used: 1 or 2. (1 machine cycle = 15 clocks.)
    fn step(&mut self, io: &mut dyn Mcs48Io) -> u32;
}

impl SoundCpu for Mcs48 {
    fn step(&mut self, io: &mut dyn Mcs48Io) -> u32 { Mcs48::step(self, io) }
}

pub struct DkSoundIo {
    rom: Vec<u8>,            // "soundcpu" region, 0x1800 bytes
    pub cmd_latch: u8,       // main CPU writes 0x7C00 (music / sound number)
    pub sound_bits: u8,      // main CPU writes 0x7D00-0x7D07, one bit each
    pub irq: bool,           // main CPU writes 0x7D80
    p2_out: u8,              // last value the 8035 wrote to port 2
    pub dac: u8,             // port 1 -> 8-bit DAC
    pub cpu_attached: bool,
}

impl DkSoundIo {
    pub fn new(rom: Vec<u8>) -> Self {
        let mut rom = rom;
        rom.resize(0x1800, 0);
        Self { rom, cmd_latch: 0, sound_bits: 0, irq: false, p2_out: 0xFF, dac: 0x80, cpu_attached: false }
    }

    /// Main CPU reads this as IN2 bit 0x40 ("sound CPU busy").
    /// Reports "not busy" while no sound CPU exists, so the game never waits on it.
    pub fn status_bit(&self) -> bool {
        self.cpu_attached && self.p2_out & 0x10 == 0
    }

    pub fn dac_decay_on(&self) -> bool {
        self.p2_out & 0x80 == 0
    }
}

impl Mcs48Io for DkSoundIo {
    fn read_rom(&mut self, addr: u16) -> u8 {
        self.rom[(addr & 0x0FFF) as usize]     // program: s_3i_b (mirrored) at 0x0000-0x0FFF
    }

    fn read_bus(&mut self, offset: u8) -> u8 {
        let page = self.p2_out & 0x47;
        if page & 0x40 != 0 {
            (self.cmd_latch ^ 0x0F) & 0x0F       // command latch, low nibble, inverted
        } else {
            // music data: s_3j_b at 0x1000, 256-byte page picked by P2 bits 0-2
            self.rom[0x1000 + (page as usize & 7) * 256 + offset as usize]
        }
    }

    fn write_p1(&mut self, val: u8) {
        self.dac = val;
    }

    fn write_p2(&mut self, val: u8) {
        self.p2_out = val;
    }

    fn read_p2(&mut self) -> u8 {
        // bit 5 comes from the main CPU's 0x7D03 write; the whole port reads with bit 5 inverted
        let bit5 = (self.sound_bits >> 3) & 1;
        ((self.p2_out & !0x20) | (bit5 << 5)) ^ 0x20
    }

    fn t0(&mut self) -> bool { self.sound_bits & 0x20 == 0 }  // 0x7D05
    fn t1(&mut self) -> bool { self.sound_bits & 0x10 == 0 }  // 0x7D04
    fn int_requested(&mut self) -> bool { self.irq }
}

/// 8035 at 6 MHz / 15 = 400,000 machine cycles per second.
/// Video lines run at exactly 16,000 per second, so that's 25 cycles per line.
pub const SOUND_CYCLES_PER_LINE: i32 = 25;
const SOUND_CYCLES_PER_SEC: f64 = 400_000.0;

/// Turns the DAC level (changing at up to 400 kHz) into 44.1 kHz samples by
/// averaging, plus a DC-blocking filter so silence sits at 0.
pub struct DacResampler {
    step: f64,
    phase: f64,
    acc: f32,
    count: u32,
    prev_in: f32,
    prev_out: f32,
    pub out: Vec<f32>,       // interleaved L,R
}

impl DacResampler {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            step: sample_rate as f64 / SOUND_CYCLES_PER_SEC,
            phase: 0.0, acc: 0.0, count: 0, prev_in: 0.0, prev_out: 0.0,
            out: Vec::with_capacity(4096),
        }
    }

    /// Call once per sound-CPU machine cycle with the current DAC value.
    pub fn tick(&mut self, dac: u8) -> Option<f32> {
        self.acc += (dac as f32 - 128.0) / 128.0;
        self.count += 1;
        self.phase += self.step;
        if self.phase < 1.0 {
            return None;
        }
        self.phase -= 1.0;
        let x = self.acc / self.count as f32;
        self.acc = 0.0;
        self.count = 0;
        let y = x - self.prev_in + 0.995 * self.prev_out;
        self.prev_in = x;
        self.prev_out = y;
        Some(y * 0.5)
    }

    fn push(&mut self, s: f32) {
        let s = s.clamp(-1.0, 1.0);
        self.out.push(s);
        self.out.push(s);
    }
}

#[derive(Default)]
struct Voice {
    t: Option<f32>,
    phase: f32,
}

impl Voice {
    fn trigger(&mut self) { self.t = Some(0.0); self.phase = 0.0; }
}

pub struct DkSfx {
    dt: f32,
    prev_bits: u8,
    walk: Voice,
    jump: Voice,
    stomp: Voice,
    noise_lfsr: u32,
    noise_hold: f32,
    noise_clock: f32,
    noise_lp: f32,
    out_lp: f32,
}

impl DkSfx {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            dt: 1.0 / sample_rate as f32,
            prev_bits: 0,
            walk: Voice::default(),
            jump: Voice::default(),
            stomp: Voice::default(),
            noise_lfsr: 0x1FFFF,
            noise_hold: 0.0,
            noise_clock: 0.0,
            noise_lp: 0.0,
            out_lp: 0.0,
        }
    }

    pub fn next_sample(&mut self, bits: u8) -> f32 {
        let rising = bits & !self.prev_bits;
        self.prev_bits = bits;
        if rising & 0x01 != 0 { self.walk.trigger(); }
        if rising & 0x02 != 0 { self.jump.trigger(); }
        if rising & 0x04 != 0 { self.stomp.trigger(); }

        let mut mix = 0.0;
        mix += self.walk_sample();
        mix += self.jump_sample();
        mix += self.stomp_sample();

        self.out_lp += 0.35 * (mix - self.out_lp);
        self.out_lp
    }

    fn square(phase: f32) -> f32 { if phase < 0.5 { 1.0 } else { -1.0 } }

    fn walk_sample(&mut self) -> f32 {
        let dt = self.dt;
        let Some(t) = self.walk.t else { return 0.0 };
        if t > 0.08 { self.walk.t = None; return 0.0; }
        let freq = 430.0 * (1.0 - 0.35 * t / 0.08);
        self.walk.phase = (self.walk.phase + freq * dt).fract();
        self.walk.t = Some(t + dt);
        Self::square(self.walk.phase) * 0.18 * (-t / 0.025).exp()
    }

    fn jump_sample(&mut self) -> f32 {
        let dt = self.dt;
        let Some(t) = self.jump.t else { return 0.0 };
        if t > 0.5 { self.jump.t = None; return 0.0; }
        let sweep = 300.0 + 320.0 * (1.0 - (-t / 0.08).exp());
        let warble = 18.0 * (2.0 * std::f32::consts::PI * 7.6 * t).sin();
        self.jump.phase = (self.jump.phase + (sweep + warble) * dt).fract();
        self.jump.t = Some(t + dt);
        Self::square(self.jump.phase) * 0.16 * (-t / 0.14).exp()
    }

    fn stomp_sample(&mut self) -> f32 {
        let dt = self.dt;
        let Some(t) = self.stomp.t else { return 0.0 };
        if t > 0.6 { self.stomp.t = None; return 0.0; }
        self.noise_clock += 4000.0 * dt;
        while self.noise_clock >= 1.0 {
            self.noise_clock -= 1.0;
            let bit = ((self.noise_lfsr >> 16) ^ (self.noise_lfsr >> 13)) & 1;
            self.noise_lfsr = ((self.noise_lfsr << 1) | bit) & 0x1FFFF;
            self.noise_hold = if self.noise_lfsr & 1 != 0 { 1.0 } else { -1.0 };
        }
        self.noise_lp += 0.04 * (self.noise_hold - self.noise_lp);
        self.stomp.t = Some(t + dt);
        self.noise_lp * 0.16 * (-t / 0.12).exp()
    }
}

/// Runs the sound CPU for one video line, sampling the DAC as it goes.
/// `budget` carries leftover cycles between lines.
pub fn run_sound_line(cpu: &mut dyn SoundCpu, io: &mut DkSoundIo, rs: &mut DacResampler, sfx: &mut DkSfx, budget: &mut i32) {
    *budget += SOUND_CYCLES_PER_LINE;
    while *budget > 0 {
        let cycles = cpu.step(io);
        for _ in 0..cycles {
            if let Some(dac) = rs.tick(io.dac) {
                let s = dac + sfx.next_sample(io.sound_bits);
                rs.push(s);
            }
        }
        *budget -= cycles as i32;
    }
}