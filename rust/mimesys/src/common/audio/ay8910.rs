// General Instrument AY-3-8910 PSG: 3 square-wave channels, one noise generator,
// one envelope generator, two 8-bit I/O ports. Used by Popeye and a lot of other
// arcade games, plus the MSX, ZX Spectrum 128, Amstrad CPC, Vectrex...
//
// Registers:
//   0-5   tone period A, B, C (12 bits: fine, coarse)
//   6     noise period (5 bits)
//   7     mixer: bits 0-2 tone off A-C, bits 3-5 noise off A-C, bit 6/7 port A/B is output
//   8-10  volume A-C (bit 4 = use envelope)
//   11-12 envelope period (16 bits)
//   13    envelope shape (continue, attack, alternate, hold)
//   14-15 I/O port A, B
//
// Timing: everything runs from clock / 8 ("ticks"). A tone flips every `period`
// ticks, so its frequency is clock / (16 * period). Noise and envelope advance at
// half the tick rate.
//
// Reference: MAME src/devices/sound/ay8910.cpp, AY-3-8910 datasheet

/// Output level for volume 0-15, normalized. Measured AY-3-8910 curve (roughly 3 dB per step).
const VOLUME: [f32; 16] = [
    0.0, 0.00999, 0.01445, 0.02105, 0.03070, 0.04555, 0.06450, 0.10736,
    0.12658, 0.20498, 0.29221, 0.37283, 0.49253, 0.63532, 0.80558, 1.0,
];

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Ay8910 {
    regs: [u8; 16],
    address: u8,

    tone_count: [u16; 3],
    tone_out: [bool; 3],
    noise_count: u8,
    noise_lfsr: u32,
    noise_out: bool,
    prescale: bool,          // noise/envelope run every other tick

    env_count: u32,
    env_step: i8,            // 15 down to 0
    env_attack: u8,          // 0x00 or 0x0F, XORed with the step
    env_hold: bool,
    env_alternate: bool,
    env_holding: bool,

    // ticks -> output rate by averaging
    tick_rate: f64,
    out_rate: f64,
    phase: f64,
    acc: f32,
    acc_n: u32,
    lp_alpha: [f32; 3],      // optional per-channel low-pass (board RC filters); 1.0 = off
    lp_state: [f32; 3],
    dc: (f32, f32),          // DC blocker state (last input, last output)
    pub gain: f32,
    #[serde(skip)]
    pub out: Vec<f32>,       // interleaved stereo L, R
}

impl Ay8910 {
    /// `clock` is the chip clock (Popeye: 2 MHz).
    pub fn new(clock: u32, out_rate: u32) -> Self {
        let mut ay = Self {
            regs: [0; 16],
            address: 0,
            tone_count: [0; 3],
            tone_out: [false; 3],
            noise_count: 0,
            noise_lfsr: 1,
            noise_out: false,
            prescale: false,
            env_count: 0,
            env_step: 15,
            env_attack: 0,
            env_hold: true,
            env_alternate: false,
            env_holding: false,
            tick_rate: clock as f64 / 8.0,
            out_rate: out_rate as f64,
            phase: 0.0,
            acc: 0.0,
            acc_n: 0,
            lp_alpha: [1.0; 3],
            lp_state: [0.0; 3],
            dc: (0.0, 0.0),
            gain: 0.6,
            out: Vec::with_capacity(2048),
        };
        ay.reset();
        ay
    }

    pub fn reset(&mut self) {
        self.regs = [0; 16];
        self.address = 0;
        self.tone_count = [0; 3];
        self.tone_out = [false; 3];
        self.noise_lfsr = 1;
        self.set_envelope_shape(0);
    }

    pub fn write_address(&mut self, v: u8) { self.address = v & 0x0F; }

    /// Write the selected register. Returns Some(value) when port B (register 15)
    /// is written while set as an output, so the board can react to it.
    pub fn write_data(&mut self, v: u8) -> Option<u8> {
        let r = self.address as usize;
        // unused bits read back as 0 on a real AY-3-8910
        const MASK: [u8; 16] = [0xFF, 0x0F, 0xFF, 0x0F, 0xFF, 0x0F, 0x1F, 0xFF,
                                0x1F, 0x1F, 0x1F, 0xFF, 0xFF, 0x0F, 0xFF, 0xFF];
        self.regs[r] = v & MASK[r];
        match r {
            13 => self.set_envelope_shape(v & 0x0F),
            15 if self.regs[7] & 0x80 != 0 => return Some(v),
            _ => {}
        }
        None
    }

    /// Read the selected register. `port_a` is what the board drives onto port A.
    pub fn read_data(&self, port_a: u8) -> u8 {
        self.read_data_ab(port_a, self.regs[15])
    }

    /// Same, for boards that also drive port B (e.g. Konami's sound timer).
    pub fn read_data_ab(&self, port_a: u8, port_b: u8) -> u8 {
        match self.address {
            14 if self.regs[7] & 0x40 == 0 => port_a,
            15 if self.regs[7] & 0x80 == 0 => port_b,
            r => self.regs[r as usize],
        }
    }

    /// Put a one-pole low-pass on a channel's output (None = no filter).
    /// Some boards switch RC filters in and out under program control.
    pub fn set_channel_lowpass(&mut self, ch: usize, cutoff_hz: Option<f64>) {
        self.lp_alpha[ch] = match cutoff_hz {
            Some(fc) => (1.0 - (-2.0 * std::f64::consts::PI * fc / self.tick_rate).exp()) as f32,
            None => 1.0,
        };
    }

    fn set_envelope_shape(&mut self, shape: u8) {
        self.env_attack = if shape & 0x04 != 0 { 0x0F } else { 0x00 };
        if shape & 0x08 == 0 {
            // shapes 0-7: one ramp, then hold at 0
            self.env_hold = true;
            self.env_alternate = self.env_attack != 0;
        } else {
            self.env_hold = shape & 0x01 != 0;
            self.env_alternate = shape & 0x02 != 0;
        }
        self.env_step = 15;
        self.env_holding = false;
        self.env_count = 0;
    }

    fn tone_period(&self, ch: usize) -> u16 {
        let p = self.regs[ch * 2] as u16 | ((self.regs[ch * 2 + 1] as u16) << 8);
        p.max(1)
    }

    /// One tick = 8 chip clocks.
    pub fn tick(&mut self) {
        for ch in 0..3 {
            self.tone_count[ch] += 1;
            if self.tone_count[ch] >= self.tone_period(ch) {
                self.tone_count[ch] = 0;
                self.tone_out[ch] = !self.tone_out[ch];
            }
        }

        self.prescale = !self.prescale;
        if self.prescale {
            // noise: 17-bit LFSR, taps 0 and 3
            self.noise_count += 1;
            if self.noise_count >= (self.regs[6] & 0x1F).max(1) {
                self.noise_count = 0;
                let bit = (self.noise_lfsr ^ (self.noise_lfsr >> 3)) & 1;
                self.noise_lfsr = (self.noise_lfsr >> 1) | (bit << 16);
                self.noise_out = self.noise_lfsr & 1 != 0;
            }
            // envelope
            self.env_count += 1;
            let period = (self.regs[11] as u32 | ((self.regs[12] as u32) << 8)).max(1);
            if self.env_count >= period {
                self.env_count = 0;
                if !self.env_holding {
                    self.env_step -= 1;
                    if self.env_step < 0 {
                        if self.env_hold {
                            if self.env_alternate { self.env_attack ^= 0x0F; }
                            self.env_holding = true;
                            self.env_step = 0;
                        } else {
                            if self.env_alternate { self.env_attack ^= 0x0F; }
                            self.env_step = 15;
                        }
                    }
                }
            }
        }

        // mix
        let env_volume = (self.env_step as u8) ^ self.env_attack;
        let mut sum = 0.0;
        for ch in 0..3 {
            let tone_on = self.tone_out[ch] || self.regs[7] & (1 << ch) != 0;
            let noise_on = self.noise_out || self.regs[7] & (8 << ch) != 0;
            let mut v = 0.0;
            if tone_on && noise_on {
                let vol = self.regs[8 + ch];
                let level = if vol & 0x10 != 0 { env_volume } else { vol & 0x0F };
                v = VOLUME[level as usize & 0x0F];
            }
            self.lp_state[ch] += self.lp_alpha[ch] * (v - self.lp_state[ch]);
            sum += self.lp_state[ch];
        }
        self.acc += sum / 3.0;
        self.acc_n += 1;

        self.phase += self.out_rate;
        if self.phase >= self.tick_rate {
            self.phase -= self.tick_rate;
            let s = self.acc / self.acc_n as f32;
            self.acc = 0.0;
            self.acc_n = 0;
            self.push(s);
        }
    }

    #[cfg(test)]
    pub(crate) fn envelope_volume_for_test(&self) -> u8 { (self.env_step as u8) ^ self.env_attack }

    /// The AY only outputs positive levels, so remove the DC offset before output.
    /// y = x - x_prev + 0.995 * y_prev  (about a 35 Hz high-pass at 44.1 kHz)
    fn push(&mut self, x: f32) {
        let (x_prev, y_prev) = self.dc;
        let y = x - x_prev + 0.995 * y_prev;
        self.dc = (x, y);
        let v = y * self.gain;
        self.out.push(v);
        self.out.push(v);
    }
}
