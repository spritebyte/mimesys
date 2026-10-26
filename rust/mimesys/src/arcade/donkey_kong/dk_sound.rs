//! Donkey Kong sound board: an MB8884 (Intel 8035 clone, MCS-48 family) that
//! plays music and Kong's sounds through an 8-bit DAC, plus analog circuits for
//! walk / jump / stomp.

pub use crate::common::cpu::mcs48::{Mcs48, Mcs48Io};

pub trait SoundCpu: Send {
    /// Run one instruction (taking an interrupt first if one is due).
    /// Returns machine cycles used: 1 or 2. (1 machine cycle = 15 clocks.)
    fn step(&mut self, io: &mut dyn Mcs48Io) -> u32;
    /// For save states: the CPU when it is an MCS-48.
    fn mcs48(&self) -> Option<&Mcs48> { None }
    fn mcs48_mut(&mut self) -> Option<&mut Mcs48> { None }
}

impl SoundCpu for Mcs48 {
    fn step(&mut self, io: &mut dyn Mcs48Io) -> u32 { Mcs48::step(self, io) }
    fn mcs48(&self) -> Option<&Mcs48> { Some(self) }
    fn mcs48_mut(&mut self) -> Option<&mut Mcs48> { Some(self) }
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct DkSoundIo {
    #[serde(skip)]
    rom: Vec<u8>,            // "soundcpu" region, 0x1800 bytes
    pub cmd_latch: u8,       // main CPU writes 0x7C00 (music / sound number)
    pub sound_bits: u8,      // main CPU writes 0x7D00-0x7D07, one bit each
    pub irq: bool,           // main CPU writes 0x7D80
    p2_out: u8,              // last value the 8035 wrote to port 2
    pub dac: u8,             // port 1 -> 8-bit DAC
    pub cpu_attached: bool,
    /// DK Jr board: different 8035 wiring (see read_bus / read_p2)
    pub jr: bool,
    /// DK Jr: LS259 at 4H, main CPU writes 0x7C80-0x7C87 (bit 1 -> 8035 port 2 bit 6)
    pub latch_4h: u8,
    /// LS259 at 5H, main CPU writes 0x7D80-0x7D87 (DK Jr: bit 1 = falling sound)
    pub latch_5h: u8,
}

impl DkSoundIo {
    pub fn new(rom: Vec<u8>, jr: bool) -> Self {
        let mut rom = rom;
        rom.resize(0x1800, 0);
        Self { rom, cmd_latch: 0, sound_bits: 0, irq: false, p2_out: 0xFF, dac: 0x80, cpu_attached: false,
               jr, latch_4h: 0, latch_5h: 0 }
    }

    /// Main CPU reads this as IN2 bit 0x40 ("sound CPU busy").
    /// Reports "not busy" while no sound CPU exists, so the game never waits on it.
    /// After loading a state: take the sound ROM back from the board being replaced.
    pub fn keep_rom(&mut self, old: &mut Self) { self.rom = std::mem::take(&mut old.rom); }

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
        if self.jr {
            // DK Jr: every bus read is the LS174 command latch (5 bits, not inverted).
            // No music ROM on the bus: the tunes live in the 8035's own program ROM.
            return self.cmd_latch & 0x1F;
        }
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
        if self.jr {
            // DK Jr: bits 6/5/4 come from 4H bit 1, 6H bit 3, 6H bit 6; all three read inverted
            let b6 = (self.latch_4h >> 1) & 1;
            let b5 = (self.sound_bits >> 3) & 1;
            let b4 = (self.sound_bits >> 6) & 1;
            return ((self.p2_out & !0x70) | (b6 << 6) | (b5 << 5) | (b4 << 4)) ^ 0x70;
        }
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
#[derive(serde::Serialize, serde::Deserialize)]
pub struct DacResampler {
    step: f64,
    phase: f64,
    acc: f32,
    count: u32,
    prev_in: f32,
    prev_out: f32,
    #[serde(skip)]
    pub out: Vec<f32>,       // interleaved L,R
}

impl DacResampler {
    /// After loading a state: keep the samples not handed out yet.
    pub fn keep_output(&mut self, old: &mut Self) { self.out = std::mem::take(&mut old.out); }

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

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Voice {
    t: Option<f32>,
    phase: f32,
}

impl Voice {
    fn trigger(&mut self) { self.t = Some(0.0); self.phase = 0.0; }
}

#[derive(serde::Serialize, serde::Deserialize)]
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
    /// DK Jr: bit 2 is "climbing" (short noise tick) instead of DK's stomp,
    /// and the falling whistle comes from latch 5H bit 1.
    jr: bool,
    /// Falling: VCO control voltage, an RC that slides 4.16 V -> 0.13 V while enabled.
    fall_v: f32,
    fall_phase: f32,
}

impl DkSfx {
    pub fn new(sample_rate: u32, jr: bool) -> Self {
        Self {
            jr,
            fall_v: 4.16,
            fall_phase: 0.0,
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

    /// `bits` = sound latch 6H (0x7D00-0x7D07), `latch_5h` = 0x7D80-0x7D87.
    pub fn next_sample(&mut self, bits: u8, latch_5h: u8) -> f32 {
        let rising = bits & !self.prev_bits;
        self.prev_bits = bits;
        if rising & 0x01 != 0 { self.walk.trigger(); }
        if rising & 0x02 != 0 { self.jump.trigger(); }
        if rising & 0x04 != 0 { self.stomp.trigger(); }

        let mut mix = 0.0;
        mix += self.walk_sample();
        mix += self.jump_sample();
        if self.jr {
            mix += self.climb_sample();
            mix += self.fall_sample(latch_5h & 0x02 != 0);
        } else {
            mix += self.stomp_sample();
        }

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

impl DkSfx {
    /// DK Jr climbing: a short, dull noise tick each time bit 2 goes high
    /// (LFSR noise clocked slowly, cut off by a one-shot).
    fn climb_sample(&mut self) -> f32 {
        let dt = self.dt;
        let Some(t) = self.stomp.t else { return 0.0 };
        if t > 0.12 { self.stomp.t = None; return 0.0; }
        self.noise_clock += 2200.0 * dt;
        while self.noise_clock >= 1.0 {
            self.noise_clock -= 1.0;
            let bit = ((self.noise_lfsr >> 15) ^ (self.noise_lfsr >> 2)) & 1;
            self.noise_lfsr = ((self.noise_lfsr << 1) | (bit ^ 1)) & 0xFFFF;
            self.noise_hold = if self.noise_lfsr & 1 != 0 { 1.0 } else { -1.0 };
        }
        self.noise_lp += 0.08 * (self.noise_hold - self.noise_lp);
        self.stomp.t = Some(t + dt);
        self.noise_lp * 0.10 * (-t / 0.035).exp()
    }

    /// DK Jr falling: a whistle that slides from ~2.1 kHz down to ~500 Hz while
    /// 0x7D81 is set. The VCO's control input sits behind a 30k / 47uF RC (~1.4 s),
    /// so the pitch glides; frequencies are MAME's breadboard measurements.
    fn fall_sample(&mut self, on: bool) -> f32 {
        let target = if on { 0.134 } else { 4.16 };
        self.fall_v += (target - self.fall_v) * (self.dt / 1.41);
        if !on { return 0.0; }
        const V: [f32; 9] = [0.134, 0.25, 0.5, 0.75, 1.0, 2.0, 3.0, 4.0, 4.16];
        const HZ: [f32; 9] = [570.0, 538.0, 489.0, 560.0, 636.0, 1003.0, 1484.0, 2016.0, 2111.0];
        let v = self.fall_v.clamp(V[0], V[8]);
        let i = V.windows(2).position(|w| v <= w[1]).unwrap_or(7);
        let f = HZ[i] + (HZ[i + 1] - HZ[i]) * (v - V[i]) / (V[i + 1] - V[i]);
        self.fall_phase = (self.fall_phase + f * self.dt).fract();
        Self::square(self.fall_phase) * 0.06
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
                let s = dac + sfx.next_sample(io.sound_bits, io.latch_5h);
                rs.push(s);
            }
        }
        *budget -= cycles as i32;
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dkongjr_bus_reads_the_command_latch() {
        let mut io = DkSoundIo::new(vec![0xAA; 0x1000], true);
        io.cmd_latch = 0xE7;
        io.p2_out = 0x00;               // on DK this page would read music ROM
        assert_eq!(io.read_bus(0x12), 0x07);
        let mut dk = DkSoundIo::new(vec![0xAA; 0x1800], false);
        dk.p2_out = 0x00;
        assert_eq!(dk.read_bus(0x12), 0xAA);
    }

    #[test]
    fn dkongjr_port2_inputs_are_inverted() {
        let mut io = DkSoundIo::new(vec![], true);
        io.p2_out = 0xFF;
        assert_eq!(io.read_p2(), 0xFF);          // inputs low read back high (inverted)
        io.latch_4h = 0x02;                      // 0x7C81 -> bit 6
        io.sound_bits = 0x08 | 0x40;             // 0x7D03 -> bit 5, 0x7D06 -> bit 4
        assert_eq!(io.read_p2(), 0x8F);          // inputs high read back low
    }

    #[test]
    fn dkongjr_falling_whistle_follows_7d81() {
        let mut sfx = DkSfx::new(44100, true);
        let quiet: f32 = (0..4410).map(|_| sfx.next_sample(0, 0).abs()).sum();
        let loud: f32 = (0..4410).map(|_| sfx.next_sample(0, 0x02).abs()).sum();
        assert!(quiet < 1e-3 && loud > 10.0, "quiet {quiet} loud {loud}");
    }
}
