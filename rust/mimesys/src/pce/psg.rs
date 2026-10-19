// HuC6280 PSG: 6 wavetable channels (each plays a 32-sample, 5-bit waveform), noise on
// channels 5 and 6, and "DDA" mode for playing samples by writing values directly.
//
// CPU side (physical 0x1FE800-0x1FEBFF, A0-A3):
//   0 channel select   1 main volume (left high nibble, right low nibble)
//   2/3 frequency low/high (12 bits; output = 3.58 MHz / (32 * freq))
//   4 control: bit 7 on, bit 6 DDA, bits 0-4 volume (bit 6 set + bit 7 clear resets the write index)
//   5 balance (left/right nibbles)   6 waveform data (or the DDA value)
//   7 noise (channels 5-6): bit 7 on, bits 0-4 frequency   8/9 LFO (not emulated)
// Volume steps: channel 1.5 dB, main and balance 3 dB.
//
// Reference: Mednafen pce_psg, "pcetech.txt"

use serde::{Deserialize, Serialize};

const PSG_CLOCK: f64 = 3_579_545.0;
const CHUNK: u32 = 16;            // PSG ticks per mixing step

/// Run a down-counter that reloads with `period` when it reaches zero for `ticks` ticks.
/// Returns how many times it reloaded (= waveform steps).
fn fast_forward(counter: &mut u32, period: u32, ticks: u32) -> u32 {
    if *counter > ticks {
        *counter -= ticks;
        return 0;
    }
    let left = ticks - *counter;
    *counter = period - left % period;
    1 + left / period
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct Channel {
    freq: u16,
    control: u8,
    balance: u8,
    wave: [u8; 32],
    write_index: u8,
    phase: u8,
    counter: u32,
    dda: u8,
    noise_ctrl: u8,
    noise_lfsr: u32,
    noise_counter: u32,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Psg {
    select: usize,
    main_volume: u8,
    ch: Vec<Channel>,
    clock_debt: u32,              // 7.16 MHz cycles not yet turned into PSG ticks
    out_rate: f64,
    phase: f64,
    acc: (f32, f32),
    acc_n: u32,
    dc: [(f32, f32); 2],
    #[serde(skip)]
    pub out: Vec<f32>,
    #[serde(skip, default = "level_table")]
    levels: [f32; 92],
}

/// Linear level for an attenuation of `steps` x 1.5 dB (channel volume is 1.5 dB per step,
/// main volume and balance 3 dB = 2 steps). 31 + 2*15 + 2*15 = 91 steps max.
fn level_table() -> [f32; 92] {
    std::array::from_fn(|i| 10f32.powf(-(i as f32 * 1.5) / 20.0))
}

impl Psg {
    pub fn new(out_rate: u32) -> Self {
        let mut ch = vec![Channel::default(); 6];
        for c in ch.iter_mut() { c.noise_lfsr = 1; }
        Self {
            select: 0, main_volume: 0, ch, clock_debt: 0,
            out_rate: out_rate as f64, phase: 0.0, acc: (0.0, 0.0), acc_n: 0, dc: [(0.0, 0.0); 2],
            out: Vec::with_capacity(2048),
            levels: level_table(),
        }
    }

    pub fn load_state(&mut self, s: Psg) {
        let out = std::mem::take(&mut self.out);
        *self = s;
        self.out = out;
    }

    pub fn write(&mut self, port: u32, v: u8) {
        let c = &mut self.ch[self.select.min(5)];
        match port & 0x0F {
            0 => self.select = (v & 7) as usize,
            1 => self.main_volume = v,
            2 => c.freq = (c.freq & 0x0F00) | v as u16,
            3 => c.freq = (c.freq & 0x00FF) | ((v as u16 & 0x0F) << 8),
            4 => {
                if v & 0xC0 == 0x40 { c.write_index = 0; }
                c.control = v;
            }
            5 => c.balance = v,
            6 => {
                if c.control & 0x40 != 0 {
                    c.dda = v & 0x1F;
                } else if c.control & 0x80 == 0 {
                    c.wave[c.write_index as usize] = v & 0x1F;
                    c.write_index = (c.write_index + 1) & 31;
                }
            }
            7 => c.noise_ctrl = v,
            _ => {}                    // LFO
        }
    }

    /// Advance by `cycles` of the 7.16 MHz CPU clock (the PSG ticks at half that).
    /// Work is done in chunks of CHUNK ticks: each channel's counters are fast-forwarded and
    /// the mix is computed once per chunk (~224 kHz), then averaged down to the output rate.
    pub fn run(&mut self, cycles: u32) {
        self.clock_debt += cycles;
        while self.clock_debt >= 2 * CHUNK {
            self.clock_debt -= 2 * CHUNK;
            self.advance(CHUNK);
        }
    }

    fn advance(&mut self, ticks: u32) {
        let (ml, mr) = ((self.main_volume >> 4) as usize, (self.main_volume & 0x0F) as usize);
        let levels = self.levels;
        let (mut l, mut r) = (0.0f32, 0.0f32);
        for (i, c) in self.ch.iter_mut().enumerate() {
            let noise = i >= 4 && c.noise_ctrl & 0x80 != 0;
            if noise {
                let period = (((!c.noise_ctrl) & 0x1F) as u32 * 64).max(32);
                let steps = fast_forward(&mut c.noise_counter, period, ticks);
                for _ in 0..steps {
                    let x = c.noise_lfsr;
                    let bit = (x ^ (x >> 1) ^ (x >> 11) ^ (x >> 12) ^ (x >> 17)) & 1;
                    c.noise_lfsr = (x >> 1) | (bit << 17);
                }
            } else if c.control & 0xC0 == 0x80 {
                let period = if c.freq == 0 { 0x1000 } else { c.freq as u32 };
                let steps = fast_forward(&mut c.counter, period, ticks);
                c.phase = ((c.phase as u32 + steps) & 31) as u8;
            }
            if c.control & 0x80 == 0 { continue; }
            let sample = if noise {
                if c.noise_lfsr & 1 != 0 { 15.5 } else { -15.5 }
            } else if c.control & 0x40 != 0 {
                c.dda as f32 - 15.5
            } else {
                c.wave[c.phase as usize] as f32 - 15.5
            };
            let vol = (c.control & 0x1F) as usize;
            let (bl, br) = ((c.balance >> 4) as usize, (c.balance & 0x0F) as usize);
            let level = |m: usize, b: usize| if vol == 0 || m == 0 || b == 0 { 0.0 }
                else { levels[(31 - vol) + 2 * (15 - m) + 2 * (15 - b)] };
            l += sample / 15.5 * level(ml, bl);
            r += sample / 15.5 * level(mr, br);
        }
        self.acc.0 += l / 6.0;
        self.acc.1 += r / 6.0;
        self.acc_n += 1;
        self.phase += self.out_rate * ticks as f64;
        while self.phase >= PSG_CLOCK {
            self.phase -= PSG_CLOCK;
            let n = self.acc_n.max(1) as f32;
            let (sl, sr) = (self.acc.0 / n, self.acc.1 / n);
            self.acc = (0.0, 0.0);
            self.acc_n = 0;
            for (side, x) in [(0, sl), (1, sr)] {
                let (xp, yp) = self.dc[side];
                let y = x - xp + 0.995 * yp;
                self.dc[side] = (x, y);
                self.out.push(y * 1.5);
            }
        }
    }
}
