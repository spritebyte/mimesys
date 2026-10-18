// Texas Instruments SN76489 Programmable Sound Generator (and Sega's clone).
// Used by the ColecoVision, SG-1000, Master System, Game Gear, Genesis (as part of its VDP),
// BBC Micro, and plenty of arcade boards.
//
// 3 square-wave tone channels + 1 noise channel, each with a 4-bit attenuator.
// The CPU writes single bytes to one port:
//   1 cc t dddd   latch channel cc, t = 1 volume / 0 tone-or-noise, dddd = low 4 bits
//   0 x dddddd    data: upper 6 bits of the latched tone period (or volume/noise again)
// Tone period is 10 bits; the output flips every `period` ticks (tick = clock / 16).
// Noise control (3 bits): bit 2 = white noise (else periodic), bits 0-1 = rate:
//   0/1/2 = clock/512, /1024, /2048; 3 = follow tone channel 2.
// Volume: 0 = loudest, 15 = off, 2 dB per step.
//
// Chip differences:
//   TI (ColecoVision, SG-1000):  15-bit noise shift register, taps bits 0 and 1
//   Sega (SMS, Game Gear, Genesis): 16-bit, taps bits 0 and 3
// Game Gear adds a stereo register (port 0x06): bits 4-7 = channels to the left,
// bits 0-3 = channels to the right.
//
// Reference: SMS Power "SN76489" notes (Maxim), MAME sn76496.cpp

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sn76489Variant { Ti, Sega }

#[derive(Serialize, Deserialize, Clone)]
pub struct Sn76489 {
    variant: Sn76489Variant,
    tone: [u16; 3],
    volume: [u8; 4],
    noise_ctrl: u8,
    latched_channel: usize,
    latched_volume: bool,
    counter: [u16; 4],
    output: [bool; 4],
    noise_flip: bool,
    lfsr: u16,
    pub stereo: u8,          // Game Gear only; 0xFF = everything to both sides

    // ticks -> output rate
    tick_rate: f64,
    out_rate: f64,
    clock_debt: u32,         // CPU clocks not yet turned into ticks
    phase: f64,
    acc: (f32, f32),
    acc_n: u32,
    dc: [(f32, f32); 2],     // DC blocker per side
    pub gain: f32,
    #[serde(skip)]
    pub out: Vec<f32>,       // interleaved stereo L, R
}

/// Linear output level for attenuation 0-15 (2 dB steps, 15 = silent).
fn level(att: u8) -> f32 {
    if att >= 15 { 0.0 } else { 10f32.powf(-0.1 * att as f32) }
}

impl Sn76489 {
    /// `clock` is the chip's input clock (ColecoVision / SMS: 3.579545 MHz).
    pub fn new(variant: Sn76489Variant, clock: u32, out_rate: u32) -> Self {
        let mut s = Self {
            variant,
            tone: [0; 3],
            volume: [15; 4],     // silent at power-on
            noise_ctrl: 0,
            latched_channel: 0,
            latched_volume: false,
            counter: [0; 4],
            output: [false; 4],
            noise_flip: false,
            lfsr: 0,
            stereo: 0xFF,
            tick_rate: clock as f64 / 16.0,
            out_rate: out_rate as f64,
            clock_debt: 0,
            phase: 0.0,
            acc: (0.0, 0.0),
            acc_n: 0,
            dc: [(0.0, 0.0); 2],
            gain: 0.5,
            out: Vec::with_capacity(2048),
        };
        s.reset_lfsr();
        s
    }

    /// Restore from a save state, keeping the output buffer.
    pub fn load_state(&mut self, s: Sn76489) {
        let out = std::mem::take(&mut self.out);
        *self = s;
        self.out = out;
    }

    fn width(&self) -> u32 { if self.variant == Sn76489Variant::Ti { 15 } else { 16 } }
    fn taps(&self) -> u16 { if self.variant == Sn76489Variant::Ti { 0x0003 } else { 0x0009 } }
    fn reset_lfsr(&mut self) { self.lfsr = 1 << (self.width() - 1); }

    pub fn write(&mut self, v: u8) {
        if v & 0x80 != 0 {
            self.latched_channel = ((v >> 5) & 3) as usize;
            self.latched_volume = v & 0x10 != 0;
            let ch = self.latched_channel;
            if self.latched_volume {
                self.volume[ch] = v & 0x0F;
            } else if ch < 3 {
                self.tone[ch] = (self.tone[ch] & 0x3F0) | (v & 0x0F) as u16;
            } else {
                self.noise_ctrl = v & 0x07;
                self.reset_lfsr();
            }
        } else {
            let ch = self.latched_channel;
            if self.latched_volume {
                self.volume[ch] = v & 0x0F;
            } else if ch < 3 {
                self.tone[ch] = (self.tone[ch] & 0x00F) | (((v & 0x3F) as u16) << 4);
            } else {
                self.noise_ctrl = v & 0x07;
                self.reset_lfsr();
            }
        }
    }

    /// Advance by `cycles` of the input clock (one tick per 16).
    pub fn run(&mut self, cycles: u32) {
        self.clock_debt += cycles;
        while self.clock_debt >= 16 {
            self.clock_debt -= 16;
            self.tick();
        }
    }

    fn tone_period(&self, ch: usize) -> u16 {
        match (self.tone[ch], self.variant) {
            (0, Sn76489Variant::Ti) => 0x400,   // TI: 0 acts like 1024
            (0, Sn76489Variant::Sega) => 1,     // Sega: 0 acts like 1
            (p, _) => p,
        }
    }

    pub fn tick(&mut self) {
        for ch in 0..3 {
            let period = self.tone_period(ch);
            if period <= 1 {
                // too fast to hear: the output just stays high (games use this for samples)
                self.output[ch] = true;
                continue;
            }
            if self.counter[ch] == 0 {
                self.counter[ch] = period;
                self.output[ch] = !self.output[ch];
            }
            self.counter[ch] -= 1;
        }

        // noise
        if self.counter[3] == 0 {
            self.counter[3] = match self.noise_ctrl & 3 {
                0 => 0x10,
                1 => 0x20,
                2 => 0x40,
                _ => self.tone_period(2).max(1),
            };
            self.noise_flip = !self.noise_flip;
            if self.noise_flip {
                // the shift register clocks on every other flip (rising edge)
                let fb = if self.noise_ctrl & 4 != 0 {
                    ((self.lfsr & self.taps()).count_ones() & 1) as u16
                } else {
                    self.lfsr & 1
                };
                self.lfsr = (self.lfsr >> 1) | (fb << (self.width() - 1));
                self.output[3] = self.lfsr & 1 != 0;
            }
        }
        self.counter[3] -= 1;

        // mix
        let (mut l, mut r) = (0.0f32, 0.0f32);
        for ch in 0..4 {
            let v = if self.output[ch] { level(self.volume[ch]) } else { 0.0 };
            if self.stereo & (0x10 << ch) != 0 { l += v; }
            if self.stereo & (0x01 << ch) != 0 { r += v; }
        }
        self.acc.0 += l / 4.0;
        self.acc.1 += r / 4.0;
        self.acc_n += 1;

        self.phase += self.out_rate;
        if self.phase >= self.tick_rate {
            self.phase -= self.tick_rate;
            let n = self.acc_n as f32;
            let (l, r) = (self.acc.0 / n, self.acc.1 / n);
            self.acc = (0.0, 0.0);
            self.acc_n = 0;
            let l = self.dc_block(0, l);
            let r = self.dc_block(1, r);
            self.out.push(l * self.gain);
            self.out.push(r * self.gain);
        }
    }

    /// The chip only outputs positive levels; remove the DC offset (about 35 Hz high-pass).
    fn dc_block(&mut self, side: usize, x: f32) -> f32 {
        let (x_prev, y_prev) = self.dc[side];
        let y = x - x_prev + 0.995 * y_prev;
        self.dc[side] = (x, y);
        y
    }

    #[cfg(test)]
    pub(crate) fn noise_bit_for_test(&self) -> bool { self.output[3] }
    #[cfg(test)]
    pub(crate) fn regs_for_test(&self) -> ([u16; 3], [u8; 4], u8) { (self.tone, self.volume, self.noise_ctrl) }
}
