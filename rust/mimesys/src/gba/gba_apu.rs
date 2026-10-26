//! GBA sound (0x04000060-0x040000A7).
//!
//! * Four Game Boy channels (PSG): 2 squares (one with sweep), wave (two 32-sample
//!   banks), noise. Clocked at 16.78 MHz: square step = 16*(2048-f), wave step =
//!   8*(2048-f), noise = (64 << s) * r (r = 0 counts as 1/2). Frame sequencer 512 Hz.
//! * Direct Sound A/B: 32-byte FIFOs of signed 8-bit samples. A timer (0 or 1)
//!   overflow plays the next byte; when 16 bytes or fewer remain, DMA 1/2 is asked to
//!   refill (`timer_overflow` returns the request bits).
//! * Mixer (as NanoBoyAdvance): PSG sum * {25,50,100%} * (master+1)/8, plus FIFO *
//!   {50,100%}, plus SOUNDBIAS, clamped to 10 bits. Evaluated every 64 clocks and
//!   averaged down to 44.1 kHz stereo.

use serde::{Deserialize, Serialize};

pub const OUT_RATE: u64 = 44_100;
const CLOCK: u64 = 16_777_216;
const MIX_STEP: u32 = 64;
const SEQ_STEP: i32 = 32_768;

const DUTY: [u8; 4] = [0b0000_0001, 0b0000_0011, 0b0000_1111, 0b0011_1111];

#[derive(Clone, Default, Serialize, Deserialize)]
struct Env { init: u8, up: bool, period: u8, timer: u8, vol: u8 }

impl Env {
    fn write(&mut self, v: u8) { self.period = v & 7; self.up = v & 8 != 0; self.init = v >> 4; }
    fn trigger(&mut self) { self.vol = self.init; self.timer = self.period; }
    fn tick(&mut self) {
        if self.period == 0 { return; }
        self.timer = self.timer.saturating_sub(1);
        if self.timer == 0 {
            self.timer = self.period;
            if self.up && self.vol < 15 { self.vol += 1; }
            if !self.up && self.vol > 0 { self.vol -= 1; }
        }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Len { left: u16, enabled: bool }

impl Len {
    /// returns false when the length runs out
    fn tick(&mut self) -> bool {
        if self.enabled && self.left > 0 {
            self.left -= 1;
            return self.left > 0;
        }
        true
    }
    fn trigger(&mut self, max: u16) { if self.left == 0 { self.left = max; } }
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Square {
    on: bool, dac: bool, duty: u8, freq: u16, timer: i32, phase: u8,
    len: Len, env: Env,
    // sweep (channel 1)
    sw_period: u8, sw_down: bool, sw_shift: u8, sw_timer: u8, sw_on: bool, shadow: u16,
}

impl Square {
    fn period(&self) -> i32 { 16 * (2048 - self.freq as i32) }
    fn sweep_calc(&self) -> u16 {
        let d = self.shadow >> self.sw_shift;
        if self.sw_down { self.shadow.wrapping_sub(d) } else { self.shadow + d }
    }
    fn trigger(&mut self) {
        self.on = self.dac;
        self.len.trigger(64);
        self.env.trigger();
        self.timer = self.period();
        self.phase = 0;
        self.shadow = self.freq;
        self.sw_timer = if self.sw_period == 0 { 8 } else { self.sw_period };
        self.sw_on = self.sw_period != 0 || self.sw_shift != 0;
        if self.sw_shift != 0 && self.sweep_calc() > 2047 { self.on = false; }
    }
    fn sweep_tick(&mut self) {
        self.sw_timer = self.sw_timer.saturating_sub(1);
        if self.sw_timer != 0 { return; }
        self.sw_timer = if self.sw_period == 0 { 8 } else { self.sw_period };
        if !self.sw_on || self.sw_period == 0 { return; }
        let f = self.sweep_calc();
        if f > 2047 { self.on = false; return; }
        if self.sw_shift != 0 {
            self.shadow = f;
            self.freq = f;
            if self.sweep_calc() > 2047 { self.on = false; }
        }
    }
    fn run(&mut self, n: i32) {
        self.timer -= n;
        while self.timer <= 0 {
            self.timer += self.period();
            self.phase = (self.phase + 1) & 7;
        }
    }
    fn out(&self) -> i32 {
        if !self.on { return 0; }
        let high = DUTY[self.duty as usize] >> self.phase & 1 != 0;
        8 * self.env.vol as i32 * if high { 1 } else { -1 }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Wave {
    on: bool, dac: bool, two_banks: bool, bank: u8, len: Len,
    vol: u8, force75: bool, freq: u16, timer: i32, pos: u8, sample: i32,
    ram: [[u8; 16]; 2],
}

impl Wave {
    fn period(&self) -> i32 { 8 * (2048 - self.freq as i32) }
    fn trigger(&mut self) {
        self.on = self.dac;
        self.len.trigger(256);
        self.timer = self.period();
        self.pos = 0;
        if self.two_banks { self.bank = 0; }
    }
    fn run(&mut self, n: i32) {
        self.timer -= n;
        while self.timer <= 0 {
            self.timer += self.period();
            let b = self.ram[self.bank as usize][self.pos as usize / 2];
            let nib = if self.pos & 1 == 0 { b >> 4 } else { b & 15 } as i32;
            let mul = if self.force75 { 3 } else { [0, 4, 2, 1][self.vol as usize] };
            self.sample = (nib - 8) * 4 * mul;
            self.pos += 1;
            if self.pos == 32 {
                self.pos = 0;
                if self.two_banks { self.bank ^= 1; }
            }
        }
    }
    fn out(&self) -> i32 { if self.on { self.sample } else { 0 } }
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Noise {
    on: bool, dac: bool, len: Len, env: Env,
    ratio: u8, width7: bool, shift: u8, timer: i32, lfsr: u16, high: bool,
}

impl Noise {
    fn period(&self) -> i32 {
        let p = 64i32 << self.shift;
        if self.ratio == 0 { p / 2 } else { p * self.ratio as i32 }
    }
    fn trigger(&mut self) {
        self.on = self.dac;
        self.len.trigger(64);
        self.env.trigger();
        self.timer = self.period();
        self.lfsr = if self.width7 { 0x40 } else { 0x4000 };
    }
    fn run(&mut self, n: i32) {
        self.timer -= n;
        while self.timer <= 0 {
            self.timer += self.period();
            let carry = self.lfsr & 1 != 0;
            self.lfsr >>= 1;
            if carry { self.lfsr ^= if self.width7 { 0x60 } else { 0x6000 }; }
            self.high = carry;
        }
    }
    fn out(&self) -> i32 {
        if !self.on { return 0; }
        8 * self.env.vol as i32 * if self.high { 1 } else { -1 }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Fifo { buf: [u8; 32], rd: u8, count: u8, latch: i8 }

impl Fifo {
    fn push(&mut self, v: u8) {
        if self.count < 32 {
            self.buf[((self.rd + self.count) & 31) as usize] = v;
            self.count += 1;
        }
    }
    fn pop(&mut self) {
        if self.count > 0 {
            self.latch = self.buf[self.rd as usize] as i8;
            self.rd = (self.rd + 1) & 31;
            self.count -= 1;
        }
    }
    fn clear(&mut self) { self.rd = 0; self.count = 0; }
}

#[derive(Serialize, Deserialize)]
pub struct Apu {
    /// raw register bytes 0x60-0xAF (for reading back)
    regs: Vec<u8>,
    sq: [Square; 2],
    wave: Wave,
    noise: Noise,
    pub fifo: [Fifo; 2],
    seq_timer: i32,
    seq_step: u8,
    pending: u32,
    // output
    out_phase: u64,
    acc: [f32; 2],
    acc_n: u32,
    #[serde(skip)] pub samples: Vec<f32>,
}

impl Default for Apu { fn default() -> Self { Self::new() } }

impl Apu {
    pub fn new() -> Self {
        let mut a = Self {
            regs: vec![0; 0x50],
            sq: Default::default(), wave: Wave::default(), noise: Noise::default(),
            fifo: Default::default(),
            seq_timer: SEQ_STEP, seq_step: 0, pending: 0,
            out_phase: 0, acc: [0.0; 2], acc_n: 0,
            samples: Vec::with_capacity(2048),
        };
        a.regs[0x88 - 0x60] = 0x00;
        a.regs[0x89 - 0x60] = 0x02;   // SOUNDBIAS = 0x200
        a
    }

    fn r(&self, off: u32) -> u8 { self.regs[(off - 0x60) as usize] }
    fn master_on(&self) -> bool { self.r(0x84) & 0x80 != 0 }

    /// Byte read of 0x60-0xAF (write-only bits read as 0).
    pub fn read8(&self, off: u32) -> u8 {
        let mask: u8 = match off {
            0x60 => 0x7F, 0x62 | 0x68 => 0xC0, 0x63 | 0x69 | 0x79 | 0x7C | 0x81 => 0xFF,
            0x65 | 0x6D | 0x75 | 0x7D => 0x40, 0x70 => 0xE0, 0x73 => 0xE0,
            0x80 => 0x77, 0x82 => 0x0F, 0x83 => 0x77, 0x88 => 0xFE, 0x89 => 0xC3,
            0x84 => {
                return (self.r(0x84) & 0x80) | (self.sq[0].on as u8) | ((self.sq[1].on as u8) << 1)
                    | ((self.wave.on as u8) << 2) | ((self.noise.on as u8) << 3);
            }
            0x90..=0x9F => {
                // the CPU sees the bank that isn't playing
                return self.wave.ram[(self.wave.bank ^ 1) as usize][(off - 0x90) as usize];
            }
            _ => 0,
        };
        self.r(off) & mask
    }

    /// Byte write of 0x60-0xAF.
    pub fn write8(&mut self, off: u32, v: u8) {
        if (0x60..0x82).contains(&off) && !self.master_on() { return; }
        if (0x60..0xB0).contains(&off) { self.regs[(off - 0x60) as usize] = v; }
        match off {
            0x60 => { let s = &mut self.sq[0]; s.sw_shift = v & 7; s.sw_down = v & 8 != 0; s.sw_period = (v >> 4) & 7; }
            0x62 | 0x68 => { let s = &mut self.sq[((off - 0x62) / 6) as usize]; s.len.left = 64 - (v & 63) as u16; s.duty = v >> 6; }
            0x63 | 0x69 => {
                let s = &mut self.sq[((off - 0x63) / 6) as usize];
                s.env.write(v);
                s.dac = v & 0xF8 != 0;
                if !s.dac { s.on = false; }
            }
            0x64 | 0x6C => { let s = &mut self.sq[(off == 0x6C) as usize]; s.freq = (s.freq & 0x700) | v as u16; }
            0x65 | 0x6D => {
                let s = &mut self.sq[(off == 0x6D) as usize];
                s.freq = (s.freq & 0xFF) | ((v as u16 & 7) << 8);
                s.len.enabled = v & 0x40 != 0;
                if v & 0x80 != 0 { s.trigger(); }
            }
            0x70 => {
                let w = &mut self.wave;
                w.two_banks = v & 0x20 != 0;
                w.bank = (v >> 6) & 1;
                w.dac = v & 0x80 != 0;
                if !w.dac { w.on = false; }
            }
            0x72 => self.wave.len.left = 256 - v as u16,
            0x73 => { self.wave.vol = (v >> 5) & 3; self.wave.force75 = v & 0x80 != 0; }
            0x74 => self.wave.freq = (self.wave.freq & 0x700) | v as u16,
            0x75 => {
                let w = &mut self.wave;
                w.freq = (w.freq & 0xFF) | ((v as u16 & 7) << 8);
                w.len.enabled = v & 0x40 != 0;
                if v & 0x80 != 0 { w.trigger(); }
            }
            0x78 => self.noise.len.left = 64 - (v & 63) as u16,
            0x79 => {
                let n = &mut self.noise;
                n.env.write(v);
                n.dac = v & 0xF8 != 0;
                if !n.dac { n.on = false; }
            }
            0x7C => { let n = &mut self.noise; n.ratio = v & 7; n.width7 = v & 8 != 0; n.shift = v >> 4; }
            0x7D => {
                self.noise.len.enabled = v & 0x40 != 0;
                if v & 0x80 != 0 { self.noise.trigger(); }
            }
            0x83 => {
                if v & 0x08 != 0 { self.fifo[0].clear(); }
                if v & 0x80 != 0 { self.fifo[1].clear(); }
                self.regs[0x83 - 0x60] = v & 0x77;
            }
            0x84 => {
                if v & 0x80 == 0 {
                    // master off: the PSG and its registers reset
                    let ram = self.wave.ram;
                    self.sq = Default::default();
                    self.wave = Wave { ram, ..Default::default() };
                    self.noise = Noise::default();
                    for b in &mut self.regs[..0x22] { *b = 0; }
                }
            }
            0x90..=0x9F => self.wave.ram[(self.wave.bank ^ 1) as usize][(off - 0x90) as usize] = v,
            0xA0..=0xA3 => self.fifo[0].push(v),
            0xA4..=0xA7 => self.fifo[1].push(v),
            _ => {}
        }
    }

    /// Timer `t` overflowed `n` times: the FIFOs fed by it play their next samples.
    /// Returns DMA refill requests (bit 0 FIFO A, bit 1 FIFO B).
    pub fn timer_overflow(&mut self, t: usize, n: u32) -> u8 {
        if n == 0 || !self.master_on() { return 0; }
        let cnt = self.r(0x83);
        let mut req = 0;
        for f in 0..2 {
            let timer = ((cnt >> (2 + f * 4)) & 1) as usize;
            if timer != t { continue; }
            for _ in 0..n { self.fifo[f].pop(); }
            if self.fifo[f].count <= 16 { req |= 1 << f; }
        }
        req
    }

    /// Runs the sound for `cycles` system clocks.
    pub fn advance(&mut self, cycles: u32) {
        self.pending += cycles;
        while self.pending >= MIX_STEP {
            self.pending -= MIX_STEP;
            self.step(MIX_STEP as i32);
            self.mix();
        }
    }

    fn step(&mut self, n: i32) {
        self.seq_timer -= n;
        if self.seq_timer <= 0 {
            self.seq_timer += SEQ_STEP;
            let s = self.seq_step;
            if s & 1 == 0 {
                for q in &mut self.sq { if !q.len.tick() { q.on = false; } }
                if !self.wave.len.tick() { self.wave.on = false; }
                if !self.noise.len.tick() { self.noise.on = false; }
            }
            if s == 2 || s == 6 { self.sq[0].sweep_tick(); }
            if s == 7 {
                for q in &mut self.sq { q.env.tick(); }
                self.noise.env.tick();
            }
            self.seq_step = (s + 1) & 7;
        }
        if self.sq[0].on { self.sq[0].run(n); }
        if self.sq[1].on { self.sq[1].run(n); }
        if self.wave.on { self.wave.run(n); }
        if self.noise.on { self.noise.run(n); }
    }

    fn mix(&mut self) {
        let mut out = [0.0f32; 2];
        if self.master_on() {
            let cnt_l = self.r(0x80);
            let enables = self.r(0x81);
            let cnt_h = self.r(0x82) as u16 | ((self.r(0x83) as u16) << 8);
            let psg_vol = [1, 2, 4, 0][(cnt_h & 3) as usize];
            let ch = [self.sq[0].out(), self.sq[1].out(), self.wave.out(), self.noise.out()];
            let bias = ((self.r(0x88) as i32 | ((self.r(0x89) as i32) << 8)) >> 1) & 0x1FF;
            for (side, o) in out.iter_mut().enumerate() {
                // side 0 = left (SOUNDCNT_L bits 12-15 / 4-6), 1 = right (8-11 / 0-2)
                let en = if side == 0 { enables >> 4 } else { enables & 15 };
                let master = if side == 0 { (cnt_l >> 4) & 7 } else { cnt_l & 7 } as i32;
                let psg: i32 = (0..4).filter(|&i| en >> i & 1 != 0).map(|i| ch[i]).sum();
                let mut s = psg * psg_vol * (master + 1) >> 5;
                for f in 0..2 {
                    let on = cnt_h >> (8 + f * 4 + if side == 0 { 1 } else { 0 }) & 1 != 0;
                    if on {
                        let vol = if cnt_h >> (2 + f) & 1 != 0 { 4 } else { 2 };
                        s += self.fifo[f].latch as i32 * vol;
                    }
                }
                s = (s + bias * 2).clamp(0, 0x3FF) - 0x200;
                *o = s as f32 / 512.0;
            }
        }
        self.acc[0] += out[0];
        self.acc[1] += out[1];
        self.acc_n += 1;
        self.out_phase += MIX_STEP as u64 * OUT_RATE;
        if self.out_phase >= CLOCK {
            self.out_phase -= CLOCK;
            let n = self.acc_n.max(1) as f32;
            self.samples.push(self.acc[0] / n);
            self.samples.push(self.acc[1] / n);
            self.acc = [0.0; 2];
            self.acc_n = 0;
        }
    }

    pub fn drain(&mut self) -> Vec<f32> { std::mem::take(&mut self.samples) }
}
