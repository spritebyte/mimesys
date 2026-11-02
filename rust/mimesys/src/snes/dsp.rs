//! S-DSP: the SNES sound generator. Eight voices play BRR-compressed samples from the
//! SMP's 64 KB RAM with Gaussian interpolation, ADSR / GAIN envelopes, pitch modulation
//! and noise, mixed with an 8-tap FIR echo that lives in the same RAM.
//!
//! Written for mimesys from the hardware descriptions in nocash's fullsnes and anomie's
//! DSP notes (not a port of an emulator). It works a whole 32 kHz sample at a time, not
//! cycle by cycle; the hardware tables (envelope rates and their counter offsets, the
//! Gaussian ROM, which is rebuilt here from a closed formula that reproduces it) are
//! hardware facts.
//!
//! Number domains, as fullsnes: decoded BRR samples are 15-bit; a voice's output after
//! interpolation is doubled to 16 bits (bit 0 clear), and the mix is 16-bit.
//!
//! Registers: per voice x0 VOLL, x1 VOLR, x2/x3 PITCH, x4 SRCN, x5 ADSR1, x6 ADSR2, x7 GAIN,
//! x8 ENVX, x9 OUTX. Global 0C/1C MVOL, 2C/3C EVOL, 4C KON, 5C KOFF, 6C FLG, 7C ENDX,
//! 0D EFB, 2D PMON, 3D NON, 4D EON, 5D DIR, 6D ESA, 7D EDL, xF FIR.

use serde::{Deserialize, Serialize};

/// Samples per envelope / noise event at each rate (0 = never).
const RATE: [u32; 32] = [
    0, 2048, 1536, 1280, 1024, 768, 640, 512, 384, 320, 256, 192, 160, 128, 96, 80,
    64, 48, 40, 32, 24, 20, 16, 12, 10, 8, 6, 5, 4, 3, 2, 1,
];
/// The rates aren't all in phase with the shared counter.
const OFFSET: [u32; 32] = [
    0, 0, 1040, 536, 0, 1040, 536, 0, 1040, 536, 0, 1040, 536, 0, 1040, 536,
    0, 1040, 536, 0, 1040, 536, 0, 1040, 536, 0, 1040, 536, 0, 1040, 0, 0,
];
const COUNTER_RANGE: u32 = 2048 * 5 * 3;
/// Where the shared rate counter starts. Its phase against key-on isn't knowable from
/// a register; this one lines the envelope steps up with reference recordings.
const COUNTER_START: u32 = 2032;

fn gauss_table() -> [i32; 512] {
    let mut t = [0f64; 512];
    for n in 0..512 {
        let k = 0.5 + n as f64;
        let s = (std::f64::consts::PI * k * 1.280 / 1024.0).sin();
        let a = ((std::f64::consts::PI * k * 2.0 / 1023.0).cos() - 1.0) * 0.50;
        let b = ((std::f64::consts::PI * k * 4.0 / 1023.0).cos() - 1.0) * 0.08;
        t[511 - n] = s * (a + b + 1.0) / k;
    }
    let mut g = [0i32; 512];
    for p in 0..128 {
        let idx = [p, p + 256, 511 - p, 255 - p];
        let sum: f64 = idx.iter().map(|&i| t[i]).sum();
        for i in idx { g[i] = (t[i] * 2048.0 / sum + 0.5) as i32; }
    }
    g
}

fn clamp16(v: i32) -> i32 { v.clamp(-0x8000, 0x7FFF) }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
enum Env { Attack, Decay, Sustain, #[default] Release }

#[derive(Clone, Default, Serialize, Deserialize)]
struct Voice {
    /// ring of decoded 15-bit samples, filled 4 at a time
    buf: [i32; 12],
    buf_pos: usize,
    /// 12.12 position: integer part = sample past the oldest in the ring
    interp: u32,
    brr_addr: u16,
    brr_offset: u16,
    env: i32,
    /// the level worked out last sample, stored or not (bent-line GAIN looks at it)
    hidden: i32,
    mode: Env,
    kon_delay: u8,
    /// output after envelope (16-bit), for the next voice's pitch modulation
    out: i32,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Dsp {
    pub regs: Vec<u8>,
    voices: Vec<Voice>,
    #[serde(skip, default = "gauss_table")]
    gauss: [i32; 512],
    counter: u32,
    every_other: bool,
    new_kon: u8,
    noise: i32,
    echo_hist: [[i32; 2]; 8],
    echo_pos: usize,
    echo_offset: u32,
    echo_length: u32,
    /// stereo samples made and not collected yet
    #[serde(skip)]
    pub out: Vec<i16>,
    /// voices to silence (testing)
    #[serde(skip)]
    pub mute: u8,
}

impl Dsp {
    pub fn new() -> Self {
        let mut regs = vec![0u8; 128];
        regs[0x6C] = 0xE0;     // FLG after reset: soft reset, mute, echo writes off
        Self {
            regs, voices: vec![Voice::default(); 8], gauss: gauss_table(), counter: COUNTER_START,
            every_other: true, new_kon: 0, noise: 0x4000, echo_hist: [[0; 2]; 8],
            echo_pos: 0, echo_offset: 0, echo_length: 0, out: Vec::new(), mute: 0,
        }
    }

    /// Registers from a snapshot; voices start silent, KON pending as written.
    pub fn load(&mut self, regs: &[u8]) {
        *self = Self::new();
        self.regs.copy_from_slice(&regs[..128]);
        self.new_kon = self.regs[0x4C];
        self.every_other = true;
    }

    pub fn read(&self, r: u8) -> u8 { self.regs[(r & 0x7F) as usize] }

    pub fn write(&mut self, r: u8, v: u8) {
        let r = r as usize;
        match r {
            0x4C => { self.new_kon = v; self.regs[r] = v; }
            0x7C => self.regs[r] = 0,       // ENDX: any write clears it
            _ => self.regs[r] = v,
        }
    }

    fn poll(&self, rate: u32) -> bool {
        let rate = rate as usize;
        rate != 0 && (self.counter + OFFSET[rate]) % RATE[rate] == 0
    }

    fn r16(ram: &[u8], a: u16) -> u16 { ram[a as usize] as u16 | (ram[a.wrapping_add(1) as usize] as u16) << 8 }

    /// Directory entry for a source: (start, loop).
    fn dir(&self, ram: &[u8], srcn: u8) -> (u16, u16) {
        let a = (self.regs[0x5D] as u16) << 8 | 0;
        let a = a.wrapping_add(srcn as u16 * 4);
        (Self::r16(ram, a), Self::r16(ram, a.wrapping_add(2)))
    }

    /// Decode the next four samples of voice `n` into its ring.
    fn decode(&mut self, n: usize, ram: &[u8]) {
        let srcn = self.regs[n * 0x10 + 4];
        let (_, loop_addr) = self.dir(ram, srcn);
        let v = &mut self.voices[n];
        let header = ram[v.brr_addr as usize];
        let shift = header >> 4;
        let filter = header >> 2 & 3;
        for i in 0..4 {
            let byte = ram[v.brr_addr.wrapping_add(v.brr_offset + i / 2) as usize];
            let nib = if i & 1 == 0 { byte >> 4 } else { byte & 15 };
            let nib = ((nib as i32) << 28) >> 28;
            let mut s = if shift <= 12 { (nib << shift) >> 1 } else if nib < 0 { -2048 } else { 0 };
            let p1 = v.buf[(v.buf_pos + i as usize + 11) % 12];
            let p2 = v.buf[(v.buf_pos + i as usize + 10) % 12];
            s += match filter {
                0 => 0,
                1 => p1 + ((-p1) >> 4),
                2 => p1 * 2 + ((-p1 * 3) >> 5) - p2 + (p2 >> 4),
                _ => p1 * 2 + ((-p1 * 13) >> 6) - p2 + ((p2 * 3) >> 4),
            };
            // clip to 16 bits, then keep 15 (the hardware drops the top bit)
            let s = ((clamp16(s) << 1) as i16 as i32) >> 1;
            v.buf[(v.buf_pos + i as usize) % 12] = s;
        }
        v.buf_pos = (v.buf_pos + 4) % 12;
        v.brr_offset += 2;
        if v.brr_offset >= 9 {
            v.brr_offset = 1;
            if header & 1 != 0 {
                v.brr_addr = loop_addr;
                self.regs[0x7C] |= 1 << n;
                if header & 2 == 0 { v.mode = Env::Release; v.env = 0; }
            } else {
                v.brr_addr = v.brr_addr.wrapping_add(9);
            }
        }
    }

    fn key_on(&mut self, n: usize, ram: &[u8]) {
        let (start, _) = self.dir(ram, self.regs[n * 0x10 + 4]);
        let v = &mut self.voices[n];
        v.brr_addr = start;
        v.brr_offset = 1;
        v.buf_pos = 0;
        v.buf = [0; 12];
        v.interp = 0;
        v.kon_delay = 5;
        v.env = 0;
        v.hidden = 0;
        v.mode = Env::Attack;
        self.regs[0x7C] &= !(1 << n);
    }

    fn envelope(&mut self, n: usize) {
        let base = n * 0x10;
        let (adsr1, adsr2, gain) = (self.regs[base + 5], self.regs[base + 6], self.regs[base + 7]);
        let v = &mut self.voices[n];
        let mut env = v.env;
        if v.mode == Env::Release {
            v.env = (env - 8).max(0);
            v.hidden = v.env;
            return;
        }
        let rate;
        if adsr1 & 0x80 != 0 {
            match v.mode {
                Env::Attack => {
                    rate = (adsr1 as u32 & 15) * 2 + 1;
                    env += if rate == 31 { 0x400 } else { 0x20 };
                }
                Env::Decay => {
                    rate = (adsr1 as u32 >> 4 & 7) * 2 + 16;
                    env -= ((env - 1) >> 8) + 1;
                }
                _ => {
                    rate = adsr2 as u32 & 31;
                    env -= ((env - 1) >> 8) + 1;
                }
            }
        } else if gain & 0x80 == 0 {
            rate = 31;
            env = (gain as i32 & 0x7F) * 16;
        } else {
            rate = gain as u32 & 31;
            match gain >> 5 & 3 {
                0 => env -= 32,
                1 => env -= ((env - 1) >> 8) + 1,
                2 => env += 32,
                _ => env += if v.hidden < 0x600 { 32 } else { 8 },
            }
        }
        // the mode changes on the level just worked out, even when the rate doesn't fire
        if v.mode == Env::Decay && (env >> 8) == (adsr2 >> 5) as i32 { v.mode = Env::Sustain; }
        v.hidden = env;
        if !(0..=0x7FF).contains(&env) {
            env = env.clamp(0, 0x7FF);
            if v.mode == Env::Attack { v.mode = Env::Decay; }
        }
        let fire = self.poll(rate);
        if fire { self.voices[n].env = env; }
    }

    fn interpolate(&self, v: &Voice) -> i32 {
        let i = (v.interp >> 4 & 0xFF) as usize;
        let at = v.buf_pos + (v.interp >> 12) as usize;
        let s = |k: usize| v.buf[(at + k) % 12];
        let g = &self.gauss;
        let mut out = (g[0xFF - i] * s(0)) >> 10;
        out += (g[0x1FF - i] * s(1)) >> 10;
        out += (g[0x100 + i] * s(2)) >> 10;
        out = out as i16 as i32;
        out += (g[i] * s(3)) >> 10;
        clamp16(out) >> 1
    }

    /// Make one stereo sample (32 SMP cycles).
    pub fn sample(&mut self, ram: &mut [u8]) {
        self.counter = if self.counter == 0 { COUNTER_RANGE - 1 } else { self.counter - 1 };
        let flg = self.regs[0x6C];

        // KON / KOFF are looked at every other sample
        self.every_other = !self.every_other;
        if self.every_other {
            let kon = self.new_kon;
            self.new_kon = 0;
            let koff = self.regs[0x5C];
            for n in 0..8 {
                if kon & 1 << n != 0 { self.key_on(n, ram); }
                if koff & 1 << n != 0 { self.voices[n].mode = Env::Release; }
            }
        }
        if flg & 0x80 != 0 {
            for v in &mut self.voices { v.mode = Env::Release; v.env = 0; v.hidden = 0; }
        }

        if self.poll(flg as u32 & 31) {
            let n = self.noise;
            self.noise = (n >> 1) | (((n << 14) ^ (n << 13)) & 0x4000);
        }

        let (pmon, non, eon) = (self.regs[0x2D], self.regs[0x3D], self.regs[0x4D]);
        let (mut main, mut echo_in) = ([0i32; 2], [0i32; 2]);
        let mut prev_out = 0;
        for n in 0..8 {
            let base = n * 0x10;
            if self.voices[n].kon_delay > 0 {
                let v = &mut self.voices[n];
                v.kon_delay -= 1;
                if (1..=3).contains(&v.kon_delay) { self.decode(n, ram); }
                let v = &mut self.voices[n];
                v.interp = 0;
                v.out = 0;
                self.regs[base + 8] = 0;
                self.regs[base + 9] = 0;
                prev_out = 0;
                continue;
            }
            // the envelope steps before it is applied
            self.envelope(n);
            let mut pitch = (self.regs[base + 2] as i32 | (self.regs[base + 3] as i32) << 8) & 0x3FFF;
            if n > 0 && pmon & 1 << n != 0 { pitch += ((prev_out >> 5) * pitch) >> 10; }

            let sample = if non & 1 << n != 0 {
                (self.noise << 1) as i16 as i32
            } else {
                self.interpolate(&self.voices[n]) << 1
            };
            let mut out = ((sample * self.voices[n].env) >> 11) & !1;
            if self.mute & 1 << n != 0 { out = 0; }
            self.voices[n].out = out;
            prev_out = out;

            self.regs[base + 8] = (self.voices[n].env >> 4) as u8;
            self.regs[base + 9] = (out >> 8) as u8;

            for ch in 0..2 {
                let vol = self.regs[base + ch] as i8 as i32;
                let s = (out * vol) >> 7;
                main[ch] = clamp16(main[ch] + s);
                if eon & 1 << n != 0 { echo_in[ch] = clamp16(echo_in[ch] + s); }
            }

            let v = &mut self.voices[n];
            v.interp = (v.interp + pitch.clamp(0, 0x7FFF) as u32).min(0x7FFF);
            if v.interp >= 0x4000 {
                v.interp -= 0x4000;
                self.decode(n, ram);
            }
        }

        // echo: read the oldest frame, filter, write the new one
        let esa = (self.regs[0x6D] as u32) << 8;
        if self.echo_offset == 0 { self.echo_length = (self.regs[0x7D] as u32 & 15) << 11; }
        let addr = (esa + self.echo_offset) as u16;
        self.echo_pos = (self.echo_pos + 1) & 7;
        for ch in 0..2 {
            let s = Self::r16(ram, addr.wrapping_add(ch as u16 * 2)) as i16 as i32;
            self.echo_hist[self.echo_pos][ch] = s >> 1;
        }
        let mut fir = [0i32; 2];
        for ch in 0..2 {
            let mut sum = 0i32;
            for i in 0..8 {
                let h = self.echo_hist[(self.echo_pos + 1 + i) & 7][ch];
                sum += (h * self.regs[i * 0x10 + 0x0F] as i8 as i32) >> 6;
                if i == 6 { sum = sum as i16 as i32; }
            }
            fir[ch] = clamp16(sum) & !1;
        }
        if flg & 0x20 == 0 {
            let efb = self.regs[0x0D] as i8 as i32;
            for ch in 0..2 {
                let v = (clamp16(echo_in[ch] + ((fir[ch] * efb) >> 7)) & !1) as u16;
                let a = addr.wrapping_add(ch as u16 * 2);
                ram[a as usize] = v as u8;
                ram[a.wrapping_add(1) as usize] = (v >> 8) as u8;
            }
        }
        self.echo_offset += 4;
        if self.echo_offset >= self.echo_length { self.echo_offset = 0; }

        for ch in 0..2 {
            let mvol = self.regs[0x0C + ch * 0x10] as i8 as i32;
            let evol = self.regs[0x2C + ch * 0x10] as i8 as i32;
            let mut s = clamp16(((main[ch] * mvol) >> 7) + ((fir[ch] * evol) >> 7));
            if flg & 0x40 != 0 { s = 0; }
            self.out.push(s as i16);
        }
    }
}
