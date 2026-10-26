// Votrax SC-01 / SC-01-A speech synthesizer: a port of MAME's votrax.cpp simulation
// (Olivier Galibert, BSD-3-Clause). The chip's 64 phonemes are described by an internal
// 512-byte ROM (sc01a.bin for the SC-01-A); the rest is a digital sequencer driving a chain
// of switched-capacitor filters, simulated here with bilinear-transformed IIR filters.
//
// Interface:
//   write(phone)        6-bit phoneme; A/R (request) drops until the phoneme ends
//   set_inflection(n)   2-bit pitch inflection
//   set_clock(hz)       main clock (Gottlieb boards drive it from a DAC, ~720 kHz)
//   advance(main_clocks) run the chip; samples (at clock/18) are pushed to `out`
//   ar()                true = ready for the next phoneme

use std::f64::consts::PI;

const GLOTTAL_WAVE: [f64; 9] = [
    0.0, -4.0 / 7.0, 7.0 / 7.0, 6.0 / 7.0, 5.0 / 7.0, 4.0 / 7.0, 3.0 / 7.0, 2.0 / 7.0, 1.0 / 7.0,
];

#[allow(dead_code)]
pub const PHONE_NAMES: [&str; 64] = [
    "EH3", "EH2", "EH1", "PA0", "DT", "A1", "A2", "ZH",
    "AH2", "I3", "I2", "I1", "M", "N", "B", "V",
    "CH", "SH", "Z", "AW1", "NG", "AH1", "OO1", "OO",
    "L", "K", "J", "H", "G", "F", "D", "S",
    "A", "AY", "Y1", "UH3", "AH", "P", "O", "I",
    "U", "Y", "T", "R", "E", "W", "AE", "AE1",
    "AW2", "UH2", "UH1", "UH", "O2", "O1", "IU", "U1",
    "THV", "TH", "ER", "EH", "E1", "AW", "PA1", "STOP",
];

#[derive(Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
enum Timer { None, Commit, EndOfPhone }

fn bits_to_caps(mut value: u32, caps: &[f64]) -> f64 {
    let mut total = 0.0;
    for &d in caps {
        if value & 1 != 0 { total += d; }
        value >>= 1;
    }
    total
}

#[inline]
fn shift_hist<const N: usize>(val: f64, h: &mut [f64; N]) {
    for i in (1..N).rev() { h[i] = h[i - 1]; }
    h[0] = val;
}

#[inline]
fn apply_filter(x: &[f64], y: &[f64], a: &[f64], b: &[f64]) -> f64 {
    let mut total = 0.0;
    for i in 0..a.len() { total += x[i] * a[i]; }
    for i in 1..b.len() { total -= y[i - 1] * b[i]; }
    total / b[0]
}

#[inline]
fn interpolate(reg: &mut u8, target: u8) {
    *reg = reg.wrapping_sub(*reg >> 3).wrapping_add(target << 1);
}

/// bitswap: first listed bit becomes the result's MSB
fn bitswap(val: u64, bits: &[u32]) -> u8 {
    let mut r = 0u8;
    for &b in bits { r = (r << 1) | ((val >> b) & 1) as u8; }
    r
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct VotraxSc01 {
    #[serde(skip, default = "no_rom")]
    rom: [u64; 64],
    mainclock: f64,
    sclock: f64,
    cclock: f64,
    sample_count: u32,
    clock_phase: f64,
    timer: Timer,
    timer_left: f64,

    inflection: u8,
    phone: u8,
    ar_state: bool,

    rom_duration: u8,
    rom_vd: u8, rom_cld: u8,
    rom_fa: u8, rom_fc: u8, rom_va: u8,
    rom_f1: u8, rom_f2: u8, rom_f2q: u8, rom_f3: u8,
    rom_closure: bool,
    rom_pause: bool,

    cur_fa: u8, cur_fc: u8, cur_va: u8,
    cur_f1: u8, cur_f2: u8, cur_f2q: u8, cur_f3: u8,
    filt_fa: u8, filt_fc: u8, filt_va: u8,
    filt_f1: u8, filt_f2: u8, filt_f2q: u8, filt_f3: u8,

    phonetick: u16,
    ticks: u8,
    pitch: u8,
    closure: u8,
    update_counter: u8,
    cur_closure: bool,
    noise: u16,
    cur_noise: bool,

    voice_1: [f64; 4], voice_2: [f64; 4], voice_3: [f64; 4],
    noise_1: [f64; 3], noise_2: [f64; 3], noise_3: [f64; 2], noise_4: [f64; 2],
    vn_1: [f64; 4], vn_2: [f64; 4], vn_3: [f64; 4], vn_4: [f64; 4], vn_5: [f64; 2], vn_6: [f64; 2],

    f1_a: [f64; 4], f1_b: [f64; 4],
    f2v_a: [f64; 4], f2v_b: [f64; 4],
    f2n_a: [f64; 2], f2n_b: [f64; 2],
    f3_a: [f64; 4], f3_b: [f64; 4],
    f4_a: [f64; 4], f4_b: [f64; 4],
    fx_a: [f64; 1], fx_b: [f64; 2],
    fn_a: [f64; 3], fn_b: [f64; 3],

    /// samples at `sample_rate()` since the last drain
    #[serde(skip)]
    pub out: Vec<f32>,
}

fn no_rom() -> [u64; 64] { [0; 64] }

impl VotraxSc01 {
    /// After loading a state: take the phone ROM back from the chip being replaced.
    pub fn keep_static(&mut self, old: &Self) { self.rom = old.rom; }

    /// `rom`: the 512-byte phone ROM (64 little-endian 64-bit words).
    pub fn new(rom: &[u8], clock: f64) -> Self {
        let mut words = [0u64; 64];
        for (i, w) in words.iter_mut().enumerate() {
            let mut b = [0u8; 8];
            if rom.len() >= (i + 1) * 8 { b.copy_from_slice(&rom[i * 8..i * 8 + 8]); }
            *w = u64::from_le_bytes(b);
        }
        let mut v = Self {
            rom: words, mainclock: clock, sclock: clock / 18.0, cclock: clock / 36.0,
            sample_count: 0, clock_phase: 0.0, timer: Timer::None, timer_left: 0.0,
            inflection: 0, phone: 0x3F, ar_state: true,
            rom_duration: 0, rom_vd: 0, rom_cld: 0, rom_fa: 0, rom_fc: 0, rom_va: 0,
            rom_f1: 0, rom_f2: 0, rom_f2q: 0, rom_f3: 0, rom_closure: false, rom_pause: false,
            cur_fa: 0, cur_fc: 0, cur_va: 0, cur_f1: 0, cur_f2: 0, cur_f2q: 0, cur_f3: 0,
            filt_fa: 0, filt_fc: 0, filt_va: 0, filt_f1: 0, filt_f2: 0, filt_f2q: 0, filt_f3: 0,
            phonetick: 0, ticks: 0, pitch: 0, closure: 0, update_counter: 0,
            cur_closure: true, noise: 0, cur_noise: false,
            voice_1: [0.0; 4], voice_2: [0.0; 4], voice_3: [0.0; 4],
            noise_1: [0.0; 3], noise_2: [0.0; 3], noise_3: [0.0; 2], noise_4: [0.0; 2],
            vn_1: [0.0; 4], vn_2: [0.0; 4], vn_3: [0.0; 4], vn_4: [0.0; 4], vn_5: [0.0; 2], vn_6: [0.0; 2],
            f1_a: [0.0; 4], f1_b: [0.0; 4], f2v_a: [0.0; 4], f2v_b: [0.0; 4],
            f2n_a: [0.0; 2], f2n_b: [0.0; 2], f3_a: [0.0; 4], f3_b: [0.0; 4],
            f4_a: [0.0; 4], f4_b: [0.0; 4], fx_a: [0.0; 1], fx_b: [0.0; 2],
            fn_a: [0.0; 3], fn_b: [0.0; 3],
            out: Vec::new(),
        };
        v.phone_commit();
        v.filters_commit(true);
        v
    }

    #[allow(dead_code)]
    pub fn sample_rate(&self) -> f64 { self.sclock }
    pub fn ar(&self) -> bool { self.ar_state }
    pub fn phone(&self) -> u8 { self.phone }

    pub fn set_clock(&mut self, hz: f64) {
        if hz == self.mainclock { return; }
        // the phone timer counts main clocks, so it carries over as is
        self.mainclock = hz;
        self.sclock = hz / 18.0;
        self.cclock = hz / 36.0;
        self.filters_commit(true);
    }

    pub fn set_inflection(&mut self, data: u8) { self.inflection = data & 3; }

    pub fn write(&mut self, data: u8) {
        self.phone = data & 0x3F;
        self.ar_state = false;
        if self.timer != Timer::Commit {
            self.timer = Timer::Commit;
            self.timer_left = 72.0;
        }
    }

    /// Run for `clocks` main clocks.
    pub fn advance(&mut self, clocks: f64) {
        let mut left = clocks;
        while left > 0.0 {
            // next event: a sample (every 18 clocks) or the timer
            let to_sample = 18.0 - self.clock_phase;
            let to_timer = if self.timer != Timer::None { self.timer_left } else { f64::INFINITY };
            let step = left.min(to_sample).min(to_timer);
            left -= step;
            self.clock_phase += step;
            if self.timer != Timer::None {
                self.timer_left -= step;
                if self.timer_left <= 0.0 { self.timer_fire(); }
            }
            if self.clock_phase >= 18.0 {
                self.clock_phase -= 18.0;
                self.sample_count = self.sample_count.wrapping_add(1);
                if self.sample_count & 1 != 0 { self.chip_update(); }
                let s = self.analog_calc();
                self.out.push(s as f32);
            }
        }
    }

    fn timer_fire(&mut self) {
        match self.timer {
            Timer::Commit => {
                self.phone_commit();
                self.timer = Timer::EndOfPhone;
                self.timer_left = (16 * (self.rom_duration as u32 * 4 + 1) * 4 * 9 + 2) as f64;
            }
            Timer::EndOfPhone => {
                self.ar_state = true;
                self.timer = Timer::None;
            }
            Timer::None => {}
        }
    }

    fn phone_commit(&mut self) {
        self.phonetick = 0;
        self.ticks = 0;
        for i in 0..64 {
            let val = self.rom[i];
            if self.phone as u64 == (val >> 56) & 0x3F {
                self.rom_f1 = bitswap(val, &[0, 7, 14, 21]);
                self.rom_va = bitswap(val, &[1, 8, 15, 22]);
                self.rom_f2 = bitswap(val, &[2, 9, 16, 23]);
                self.rom_fc = bitswap(val, &[3, 10, 17, 24]);
                self.rom_f2q = bitswap(val, &[4, 11, 18, 25]);
                self.rom_f3 = bitswap(val, &[5, 12, 19, 26]);
                self.rom_fa = bitswap(val, &[6, 13, 20, 27]);
                self.rom_cld = bitswap(val, &[34, 32, 30, 28]);
                self.rom_vd = bitswap(val, &[35, 33, 31, 29]);
                self.rom_closure = (val >> 36) & 1 != 0;
                self.rom_duration = bitswap(!val, &[37, 38, 39, 40, 41, 42, 43]);
                self.rom_pause = self.phone == 0x03 || self.phone == 0x3E;
                if self.rom_vd == 0 { self.cur_closure = self.rom_closure; }
                return;
            }
        }
    }

    fn chip_update(&mut self) {
        if self.ticks != 0x10 {
            self.phonetick += 1;
            if self.phonetick == (((self.rom_duration as u16) << 2) | 1) {
                self.phonetick = 0;
                self.ticks += 1;
                if self.ticks == self.rom_vd { self.cur_closure = self.rom_closure; }
            }
        }

        self.update_counter += 1;
        if self.update_counter == 0x30 { self.update_counter = 0; }
        let tick_625 = self.update_counter & 0xF == 0;
        let tick_208 = self.update_counter == 0x28;

        if tick_208 && (!self.rom_pause || (self.filt_fa == 0 && self.filt_va == 0)) {
            interpolate(&mut self.cur_f1, self.rom_f1);
            interpolate(&mut self.cur_f2, self.rom_f2);
            interpolate(&mut self.cur_f2q, self.rom_f2q);
            interpolate(&mut self.cur_f3, self.rom_f3);
        }
        if tick_625 {
            if self.ticks >= self.rom_vd {
                interpolate(&mut self.cur_fa, self.rom_fa);
                interpolate(&mut self.cur_fc, self.rom_fc);
            }
            if self.ticks >= self.rom_cld {
                interpolate(&mut self.cur_va, self.rom_va);
            }
        }

        if !self.cur_closure && (self.filt_fa != 0 || self.filt_va != 0) {
            self.closure = 0;
        } else if self.closure != 7 << 2 {
            self.closure += 1;
        }

        self.pitch = self.pitch.wrapping_add(1);
        let target = ((0xE0u32 ^ ((self.inflection as u32) << 5) ^ ((self.filt_f1 as u32) << 1)) + 2) as u32;
        if self.pitch as u32 == target { self.pitch = 0; }

        if self.pitch & 0xF9 == 0x08 { self.filters_commit(false); }

        let inp = self.cur_noise && self.noise != 0x7FFF;
        self.noise = ((self.noise << 1) & 0x7FFE) | inp as u16;
        self.cur_noise = ((self.noise >> 14) ^ (self.noise >> 13)) & 1 == 0;
    }

    fn filters_commit(&mut self, force: bool) {
        self.filt_fa = self.cur_fa >> 4;
        self.filt_fc = self.cur_fc >> 4;
        self.filt_va = self.cur_va >> 4;

        if force || self.filt_f1 != self.cur_f1 >> 4 {
            self.filt_f1 = self.cur_f1 >> 4;
            let (a, b) = self.standard_filter(11247.0, 11797.0, 949.0, 52067.0,
                2280.0 + bits_to_caps(self.filt_f1 as u32, &[2546.0, 4973.0, 9861.0, 19724.0]), 166272.0);
            self.f1_a = a; self.f1_b = b;
        }
        if force || self.filt_f2 != self.cur_f2 >> 3 || self.filt_f2q != self.cur_f2q >> 4 {
            self.filt_f2 = self.cur_f2 >> 3;
            self.filt_f2q = self.cur_f2q >> 4;
            let q = 829.0 + bits_to_caps(self.filt_f2q as u32, &[1390.0, 2965.0, 5875.0, 11297.0]);
            let f = 2352.0 + bits_to_caps(self.filt_f2 as u32, &[833.0, 1663.0, 3164.0, 6327.0, 12654.0]);
            let (a, b) = self.standard_filter(24840.0, 29154.0, q, 38180.0, f, 34270.0);
            self.f2v_a = a; self.f2v_b = b;
            let (a, b) = self.injection_filter(29154.0, q, 38180.0, f, 34270.0);
            self.f2n_a = a; self.f2n_b = b;
        }
        if force || self.filt_f3 != self.cur_f3 >> 4 {
            self.filt_f3 = self.cur_f3 >> 4;
            let (a, b) = self.standard_filter(0.0, 17594.0, 868.0, 18828.0,
                8480.0 + bits_to_caps(self.filt_f3 as u32, &[2226.0, 4485.0, 9056.0, 18111.0]), 50019.0);
            self.f3_a = a; self.f3_b = b;
        }
        if force {
            let (a, b) = self.standard_filter(0.0, 28810.0, 1165.0, 21457.0, 8558.0, 7289.0);
            self.f4_a = a; self.f4_b = b;
            let (a, b) = self.lowpass_filter(1122.0, 23131.0);
            self.fx_a = a; self.fx_b = b;
            let (a, b) = self.noise_shaper_filter(15500.0, 14854.0, 8450.0, 9523.0, 14083.0);
            self.fn_a = a; self.fn_b = b;
        }
    }

    fn analog_calc(&mut self) -> f64 {
        let mut v = if self.pitch >= (9 << 3) { 0.0 } else { GLOTTAL_WAVE[(self.pitch >> 3) as usize] };
        v = v * self.filt_va as f64 / 15.0;
        shift_hist(v, &mut self.voice_1);
        v = apply_filter(&self.voice_1, &self.voice_2, &self.f1_a, &self.f1_b);
        shift_hist(v, &mut self.voice_2);
        v = apply_filter(&self.voice_2, &self.voice_3, &self.f2v_a, &self.f2v_b);
        shift_hist(v, &mut self.voice_3);

        let mut n = 5e3 * if self.pitch & 0x40 != 0 && self.cur_noise { 1.0 } else { -1.0 };
        n = n * self.filt_fa as f64 / 15.0;
        shift_hist(n, &mut self.noise_1);
        n = apply_filter(&self.noise_1, &self.noise_2, &self.fn_a, &self.fn_b);
        shift_hist(n, &mut self.noise_2);

        let mut n2 = n * self.filt_fc as f64 / 15.0;
        shift_hist(n2, &mut self.noise_3);
        n2 = apply_filter(&self.noise_3, &self.noise_4, &self.f2n_a, &self.f2n_b);
        shift_hist(n2, &mut self.noise_4);

        let mut vn = v + n2;
        shift_hist(vn, &mut self.vn_1);
        vn = apply_filter(&self.vn_1, &self.vn_2, &self.f3_a, &self.f3_b);
        shift_hist(vn, &mut self.vn_2);

        vn += n * (15 ^ self.filt_fc) as f64 / 15.0;
        shift_hist(vn, &mut self.vn_3);
        vn = apply_filter(&self.vn_3, &self.vn_4, &self.f4_a, &self.f4_b);
        shift_hist(vn, &mut self.vn_4);

        vn = vn * (7 ^ (self.closure >> 2)) as f64 / 7.0;
        shift_hist(vn, &mut self.vn_5);
        vn = apply_filter(&self.vn_5, &self.vn_6, &self.fx_a, &self.fx_b);
        shift_hist(vn, &mut self.vn_6);

        vn * 0.35
    }

    fn standard_filter(&self, c1t: f64, c1b: f64, c2t: f64, c2b: f64, c3: f64, c4: f64) -> ([f64; 4], [f64; 4]) {
        let k0 = c1t / (self.cclock * c1b);
        let k1 = c4 * c2t / (self.cclock * c1b * c3);
        let k2 = c4 * c2b / (self.cclock * self.cclock * c1b * c3);
        let fpeak = (k0 * k1 - k2).abs().sqrt() / (2.0 * PI * k2);
        let zc = 2.0 * PI * fpeak / (PI * fpeak / self.sclock).tan();
        let m0 = zc * k0;
        let m1 = zc * k1;
        let m2 = zc * zc * k2;
        ([1.0 + m0, 3.0 + m0, 3.0 - m0, 1.0 - m0],
         [1.0 + m1 + m2, 3.0 + m1 - m2, 3.0 - m1 - m2, 1.0 - m1 + m2])
    }

    fn lowpass_filter(&self, c1t: f64, c1b: f64) -> ([f64; 1], [f64; 2]) {
        let k = c1b / (self.cclock * c1t) * (150.0 / 4000.0);
        let fpeak = 1.0 / (2.0 * PI * k);
        let zc = 2.0 * PI * fpeak / (PI * fpeak / self.sclock).tan();
        let m = zc * k;
        ([1.0], [1.0 + m, 1.0 - m])
    }

    fn noise_shaper_filter(&self, c1: f64, c2t: f64, c2b: f64, c3: f64, c4: f64) -> ([f64; 3], [f64; 3]) {
        let k0 = c2t * c3 * c2b / c4;
        let k1 = c2t * (self.cclock * c2b);
        let k2 = c1 * c2t * c3 / (self.cclock * c4);
        let fpeak = (1.0 / k2).sqrt() / (2.0 * PI);
        let zc = 2.0 * PI * fpeak / (PI * fpeak / self.sclock).tan();
        let m0 = zc * k0;
        let m1 = zc * k1;
        let m2 = zc * zc * k2;
        ([m0, 0.0, -m0], [1.0 + m1 + m2, 2.0 - 2.0 * m2, 1.0 - m1 + m2])
    }

    fn injection_filter(&self, c1b: f64, c2t: f64, c2b: f64, c3: f64, _c4: f64) -> ([f64; 2], [f64; 2]) {
        let k0 = self.cclock * c2t;
        let k1 = self.cclock * (c1b * c3 / c2t + c2t);
        let k2 = c2b;
        let zc = 2.0 * self.sclock;
        let m = zc * k2;
        ([k0 + m, k0 - m], [k1 + m, k1 - m])
    }
}

/// Without the phone ROM: no sound, but A/R still follows a typical phoneme length so
/// the game's speech code (which waits on it) keeps running at the right pace.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct VotraxStub {
    clocks_left: f64,
    pub phone: u8,
}

impl VotraxStub {
    pub fn new() -> Self { Self { clocks_left: 0.0, phone: 0x3F } }
    pub fn ar(&self) -> bool { self.clocks_left <= 0.0 }
    pub fn write(&mut self, data: u8, clock: f64) {
        self.phone = data & 0x3F;
        // ~ 90 ms per phoneme at the nominal clock
        self.clocks_left = 0.09 * clock;
    }
    pub fn advance(&mut self, clocks: f64) {
        if self.clocks_left > 0.0 { self.clocks_left -= clocks; }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A made-up phone ROM: each entry has its phone number in bits 56-61 and some
    /// filter/duration bits. Checks the sequencer and filters stay sane.
    #[test]
    fn phoneme_ends_and_output_is_finite() {
        let mut rom = vec![0u8; 512];
        for i in 0..64u64 {
            let val: u64 = (i << 56) | 0x0000_00F0_0FFF_FFFF ^ (i * 0x1234_5678);
            rom[i as usize * 8..i as usize * 8 + 8].copy_from_slice(&val.to_le_bytes());
        }
        let clock = 720_000.0;
        let mut v = VotraxSc01::new(&rom, clock);
        assert!(v.ar());
        v.write(0x20);
        assert!(!v.ar());
        v.advance(clock * 0.5);    // half a second is longer than any phoneme
        assert!(v.ar());
        assert!(!v.out.is_empty());
        assert!(v.out.iter().all(|s| s.is_finite()));
        let _ = PHONE_NAMES[v.phone() as usize];
    }
}

#[cfg(test)]
mod level_probe {
    use super::*;
    /// Build a ROM word from field values (inverse of phone_commit).
    fn word(phone: u64, f1: u8, va: u8, f2: u8, fc: u8, f2q: u8, f3: u8, fa: u8, cld: u8, vd: u8, closure: bool, dur: u8) -> u64 {
        let mut v: u64 = phone << 56;
        let put = |v: &mut u64, val: u8, bits: &[u32]| {
            for (i, &b) in bits.iter().enumerate() {
                if (val >> (bits.len() - 1 - i)) & 1 != 0 { *v |= 1 << b; }
            }
        };
        put(&mut v, f1, &[0, 7, 14, 21]); put(&mut v, va, &[1, 8, 15, 22]);
        put(&mut v, f2, &[2, 9, 16, 23]); put(&mut v, fc, &[3, 10, 17, 24]);
        put(&mut v, f2q, &[4, 11, 18, 25]); put(&mut v, f3, &[5, 12, 19, 26]);
        put(&mut v, fa, &[6, 13, 20, 27]); put(&mut v, cld, &[34, 32, 30, 28]);
        put(&mut v, vd, &[35, 33, 31, 29]);
        if closure { v |= 1 << 36; }
        // duration is stored inverted
        put(&mut v, !dur & 0x7F, &[37, 38, 39, 40, 41, 42, 43]);
        v
    }
    #[test]
    #[ignore]
    fn votrax_levels() {
        for (name, va, fa) in [("vowel", 15u8, 0u8), ("fricative", 0, 15)] {
            let mut rom = vec![0u8; 512];
            for i in 0..64u64 {
                let w = word(i, 6, va, 8, 8, 8, 8, fa, 0, 0, false, 100);
                rom[i as usize * 8..i as usize * 8 + 8].copy_from_slice(&w.to_le_bytes());
            }
            let clock = 720_000.0;
            let mut v = VotraxSc01::new(&rom, clock);
            v.write(0x20);
            v.advance(clock * 0.3);
            let s = &v.out[v.out.len() / 2..];
            let peak = s.iter().fold(0f32, |m, x| m.max(x.abs()));
            let rms = (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt();
            println!("{name}: peak {peak:.4} rms {rms:.4}");
        }
    }
}
