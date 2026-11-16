//! AICA sound generation: 64 slots (16-bit / 8-bit PCM, Yamaha 4-bit ADPCM) with
//! envelopes, LFOs, loops, pan / level, and the effect DSP (128 steps, ring buffer in sound
//! RAM). Ported from MAME devices/sound/aica.cpp and aicadsp.cpp (BSD-3-Clause, ElSemi,
//! Deunan Knute, R. Belmont). The slot and DSP program registers stay in the AICA register
//! file (`regs`); this module keeps the running state.
//!
//! Register file: slot s at s × 0x80 (words every 4 bytes): 00 KYONEX 15, KYONB 14, SSCTL
//! 10, LPCTL 9, PCMS 8-7, SA high 6-0; 04 SA low; 08 LSA; 0C LEA; 10 D2R 15-11, D1R 10-6,
//! AR 4-0; 14 LPSLNK 14, KRS 13-10, DL 9-5, RR 4-0; 18 OCT 14-11, FNS 9-0; 1C LFORE 15, LFOF
//! 14-10, PLFOWS 9-8, PLFOS 7-5, ALFOWS 4-3, ALFOS 2-0; 20 IMXL 7-4, ISEL 3-0; 24 DISDL
//! 11-8, DIPAN 4-0; 28 TL 15-8. Common: 2000 + 4n EFSDL 11-8 / EFPAN 4-0 (16 DSP outputs, 2
//! CD inputs); 2800 MONO 15, DAC18B 8, MVOL 3-0; 2804 RBL 14-13, RBP 11-0; 280C MSLC 13-8,
//! AFSEL 14; 2810 envelope / loop monitor; 2814 play position. DSP: 3000 COEF, 3200 MADRS,
//! 3400 MPRO (4 words a step), 4000 TEMP, 4400 MEMS, 4500 MIXS, 4580 EFREG, 45C0 EXTS.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

const SHIFT: u32 = 12;
const EG_SHIFT: u32 = 16;
const LFO_SHIFT: u32 = 8;

const ATTACK: u8 = 0;
const DECAY1: u8 = 1;
const DECAY2: u8 = 2;
const RELEASE: u8 = 3;

const AR_TIMES: [f64; 64] = [100000.0, 100000.0, 8100.0, 6900.0, 6000.0, 4800.0, 4000.0, 3400.0, 3000.0, 2400.0, 2000.0, 1700.0, 1500.0,
    1200.0, 1000.0, 860.0, 760.0, 600.0, 500.0, 430.0, 380.0, 300.0, 250.0, 220.0, 190.0, 150.0, 130.0, 110.0, 95.0,
    76.0, 63.0, 55.0, 47.0, 38.0, 31.0, 27.0, 24.0, 19.0, 15.0, 13.0, 12.0, 9.4, 7.9, 6.8, 6.0, 4.7, 3.8, 3.4, 3.0, 2.4,
    2.0, 1.8, 1.6, 1.3, 1.1, 0.93, 0.85, 0.65, 0.53, 0.44, 0.40, 0.35, 0.0, 0.0];
const DR_TIMES: [f64; 64] = [100000.0, 100000.0, 118200.0, 101300.0, 88600.0, 70900.0, 59100.0, 50700.0, 44300.0, 35500.0, 29600.0, 25300.0, 22200.0, 17700.0,
    14800.0, 12700.0, 11100.0, 8900.0, 7400.0, 6300.0, 5500.0, 4400.0, 3700.0, 3200.0, 2800.0, 2200.0, 1800.0, 1600.0, 1400.0, 1100.0,
    920.0, 790.0, 690.0, 550.0, 460.0, 390.0, 340.0, 270.0, 230.0, 200.0, 170.0, 140.0, 110.0, 98.0, 85.0, 68.0, 57.0, 49.0, 43.0, 34.0,
    28.0, 25.0, 22.0, 18.0, 14.0, 12.0, 11.0, 8.5, 7.1, 6.1, 5.4, 4.3, 3.6, 3.1];
const SDLT: [f32; 16] = [-1000000.0, -42.0, -39.0, -36.0, -33.0, -30.0, -27.0, -24.0, -21.0, -18.0, -15.0, -12.0, -9.0, -6.0, -3.0, 0.0];
const LFO_FREQ: [f32; 32] = [0.17, 0.19, 0.23, 0.27, 0.34, 0.39, 0.45, 0.55, 0.68, 0.78, 0.92, 1.10, 1.39, 1.60, 1.87, 2.27,
    2.87, 3.31, 3.92, 4.79, 6.15, 7.18, 8.60, 10.8, 14.4, 17.2, 21.5, 28.7, 43.1, 57.4, 86.1, 172.3];
const ASCALE: [f32; 8] = [0.0, 0.4, 0.8, 1.5, 3.0, 6.0, 12.0, 24.0];
const PSCALE: [f32; 8] = [0.0, 7.0, 13.5, 27.0, 55.0, 112.0, 230.0, 494.0];
const TABLE_QUANT: [i32; 8] = [230, 230, 230, 230, 307, 409, 512, 614]; // ×256: 0.8984375 .. 2.3984375
const QUANT_MUL: [i32; 8] = [1, 3, 5, 7, 9, 11, 13, 15];

/// Fixed tables (MAME aica Init / LFO_Init).
struct Tables {
    eg: Vec<i32>,
    lpan: Vec<i32>,
    rpan: Vec<i32>,
    ar: [i32; 64],
    dr: [i32; 64],
    /// [waveform saw / square / triangle / noise][phase]
    alfo: [[i32; 256]; 4],
    plfo: [[i32; 256]; 4],
    ascales: [[i32; 256]; 8],
    pscales: [[i32; 256]; 8],
}

fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| {
        let fix = |v: f32| ((1u32 << SHIFT) as f32 * v) as i32;
        let lfix = |v: f32| ((1u32 << LFO_SHIFT) as f32 * v) as i32;
        let eg = (0..0x400).map(|i| {
            let db = (3 * (i - 0x3FF)) as f32 / 32.0;
            (10f32.powf(db / 20.0) * (1 << SHIFT) as f32) as i32
        }).collect();
        let mut lpan = vec![0; 0x20000];
        let mut rpan = vec![0; 0x20000];
        for i in 0..0x20000usize {
            let (itl, ipan, isdl) = (i & 0xFF, i >> 8 & 0x1F, i >> 13 & 0xF);
            let mut db = 0.0f32;
            for (b, d) in [0.4, 0.8, 1.5, 3.0, 6.0, 12.0, 24.0, 48.0].iter().enumerate() { if itl & (1 << b) != 0 { db -= d; } }
            let tl = 10f32.powf(db / 20.0);
            let mut db = 0.0f32;
            for (b, d) in [3.0, 6.0, 12.0, 24.0].iter().enumerate() { if ipan & (1 << b) != 0 { db -= d; } }
            let pan = if ipan & 0xF == 0xF { 0.0 } else { 10f32.powf(db / 20.0) };
            let (l, r) = if ipan < 0x10 { (pan, 1.0) } else { (1.0, pan) };
            let sdl = if isdl != 0 { 10f32.powf(SDLT[isdl] / 20.0) } else { 0.0 };
            lpan[i] = fix(4.0 * l * tl * sdl);
            rpan[i] = fix(4.0 * r * tl * sdl);
        }
        let mut ar = [0; 64];
        let mut dr = [0; 64];
        for i in 2..64 {
            let t = AR_TIMES[i];
            ar[i] = if t != 0.0 { ((1023.0 * 1000.0) / (44100.0 * t) * (1 << EG_SHIFT) as f64) as i32 } else { 1024 << EG_SHIFT };
            dr[i] = ((1023.0 * 1000.0) / (44100.0 * DR_TIMES[i]) * (1 << EG_SHIFT) as f64) as i32;
        }
        let mut alfo = [[0; 256]; 4];
        let mut plfo = [[0; 256]; 4];
        let mut seed = 0x1234_5678u32;
        for i in 0..256i32 {
            alfo[0][i as usize] = 255 - i;
            plfo[0][i as usize] = if i < 128 { i } else { i - 256 };
            alfo[1][i as usize] = if i < 128 { 255 } else { 0 };
            plfo[1][i as usize] = if i < 128 { 127 } else { -128 };
            alfo[2][i as usize] = if i < 128 { 255 - i * 2 } else { i * 2 - 256 };
            plfo[2][i as usize] = if i < 64 { i * 2 } else if i < 128 { 255 - i * 2 } else if i < 192 { 256 - i * 2 } else { i * 2 - 511 };
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            let a = (seed >> 16 & 0xFF) as i32;
            alfo[3][i as usize] = a;
            plfo[3][i as usize] = 128 - a;
        }
        let mut ascales = [[0; 256]; 8];
        let mut pscales = [[0; 256]; 8];
        for s in 0..8 {
            for i in -128..128i32 {
                pscales[s][(i + 128) as usize] = lfix(2f32.powf((PSCALE[s] * i as f32 / 128.0) / 1200.0));
            }
            for i in 0..256 {
                ascales[s][i] = lfix(10f32.powf((-ASCALE[s] * i as f32 / 256.0) / 20.0));
            }
        }
        Tables { eg, lpan, rpan, ar, dr, alfo, plfo, ascales, pscales }
    })
}

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
struct Lfo {
    phase: u16,
    step: u32,
    wave: u8,
    scale: u8,
}

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
pub struct Slot {
    pub active: bool,
    pub cur: u32,
    nxt: u32,
    step: u32,
    backwards: bool,
    pub eg_state: u8,
    pub eg_vol: i32,
    ar: i32,
    d1r: i32,
    d2r: i32,
    rr: i32,
    dl: i32,
    plfo: Lfo,
    alfo: Lfo,
    cur_sample: i32,
    cur_quant: i32,
    curstep: u32,
    lp_sample: i32,
    lp_quant: i32,
    adbase: u32,
    pub lpend: bool,
}

/// The effect DSP's running state.
#[derive(Clone, Serialize, Deserialize)]
struct Dsp {
    temp: Vec<i32>,
    mems: Vec<i32>,
    mixs: [i32; 16],
    efreg: [i16; 16],
    dec: u32,
    last_step: usize,
}

impl Default for Dsp {
    fn default() -> Self { Self { temp: vec![0; 128], mems: vec![0; 32], mixs: [0; 16], efreg: [0; 16], dec: 0, last_step: 0 } }
}

/// What the channels and the DSP did since the last speed line.
#[derive(Clone, Default)]
pub struct SndStats {
    pub keyons: u64,
    pub samples: u64,
    /// channels playing, summed over samples
    pub playing: u64,
    /// squared levels (full scale = 1) of the direct outputs, the DSP outputs, the CD input
    /// and the final mix; the final mix's peak
    pub direct: f64,
    pub dsp: f64,
    pub cd: f64,
    pub out: f64,
    pub peak: f32,
}

impl SndStats {
    pub fn db(sq: f64, n: u64) -> String {
        if n == 0 || sq <= 0.0 { return "silent".into(); }
        format!("{:.0} dB", 10.0 * (sq / n as f64).log10())
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Sound {
    pub slots: Vec<Slot>,
    /// for the speed log
    #[serde(skip)]
    pub stats: SndStats,
    dsp: Dsp,
    /// the DSP program changed: find its last step again
    #[serde(default)]
    dsp_dirty: bool,
}

impl Default for Sound {
    fn default() -> Self {
        let mut slots = vec![Slot::default(); 64];
        for s in &mut slots { s.eg_state = RELEASE; s.lpend = true; }
        Self { slots, stats: SndStats::default(), dsp: Dsp::default(), dsp_dirty: true }
    }
}

#[inline]
fn r16(regs: &[u8], o: usize) -> u32 { u16::from_le_bytes([regs[o], regs[o + 1]]) as u32 }
#[inline]
fn w16(regs: &mut [u8], o: usize, v: u32) { regs[o..o + 2].copy_from_slice(&(v as u16).to_le_bytes()); }

/// A slot register.
#[inline]
fn sr(regs: &[u8], s: usize, off: usize) -> u32 { r16(regs, s * 0x80 + off) }

fn sa(regs: &[u8], s: usize) -> u32 { (sr(regs, s, 0) & 0x7F) << 16 | sr(regs, s, 4) }

fn decode_adpcm(sig: &mut i32, delta: u32, quant: &mut i32) -> i32 {
    let mut x = (*quant * QUANT_MUL[(delta & 7) as usize]) / 8;
    if x > 0x7FFF { x = 0x7FFF; }
    if delta & 8 != 0 { x = -x; }
    x += *sig;
    *sig = x.clamp(-32768, 32767);
    *quant = ((*quant * TABLE_QUANT[(delta & 7) as usize]) >> 8).clamp(0x7F, 0x6000);
    *sig
}

/// DSP float-ish 16-bit memory format.
fn pack(val: i32) -> u16 {
    let sign = (val >> 23) & 1;
    let mut temp = ((val ^ (val << 1)) & 0xFF_FFFF) as u32;
    let mut exponent = 0;
    for _ in 0..12 {
        if temp & 0x80_0000 != 0 { break; }
        temp <<= 1;
        exponent += 1;
    }
    let mut v = if exponent < 12 { (val << exponent) & 0x3F_FFFF } else { val << 11 };
    v >>= 11;
    v &= 0x7FF;
    v |= sign << 15;
    v |= exponent << 11;
    v as u16
}

fn unpack(val: u16) -> i32 {
    let sign = (val >> 15) as u32 & 1;
    let mut exponent = (val >> 11) as u32 & 0xF;
    let mantissa = (val & 0x7FF) as u32;
    let mut uval = mantissa << 11;
    if exponent > 11 { exponent = 11; uval |= sign << 22; } else { uval |= (sign ^ 1) << 22; }
    uval |= sign << 23;
    let v = ((uval << 8) as i32) >> 8;
    v >> exponent
}

impl Sound {
    fn compute_eg(&mut self, regs: &[u8], s: usize) {
        let t = tables();
        let oct = ((sr(regs, s, 0x18) >> 11 & 0xF) as i32 ^ 8) - 8;
        let krs = (sr(regs, s, 0x14) >> 10 & 0xF) as i32;
        let fns = sr(regs, s, 0x18) & 0x3FF;
        let rate = if krs != 0xF { oct + 2 * krs + ((fns >> 9) & 1) as i32 } else { 0 };
        let get = |tab: &[i32; 64], r: u32| tab[(rate + ((r as i32) << 1)).clamp(0, 63) as usize];
        let d = sr(regs, s, 0x10);
        let sl = &mut self.slots[s];
        sl.ar = get(&t.ar, d & 0x1F);
        sl.d1r = get(&t.dr, d >> 6 & 0x1F);
        sl.d2r = get(&t.dr, d >> 11 & 0x1F);
        sl.rr = get(&t.dr, sr(regs, s, 0x14) & 0x1F);
        sl.dl = 0x1F - (sr(regs, s, 0x14) >> 5 & 0x1F) as i32;
    }

    fn step_of(regs: &[u8], s: usize) -> u32 {
        let oct = ((sr(regs, s, 0x18) >> 11 & 0xF) as i32 ^ 8) - 8 + SHIFT as i32 - 10;
        let f = (sr(regs, s, 0x18) & 0x3FF) + 0x400;
        if oct >= 0 { f << oct } else { f >> -oct }
    }

    fn compute_lfo(&mut self, regs: &[u8], s: usize) {
        let l = sr(regs, s, 0x1C);
        let step = ((1u32 << LFO_SHIFT) as f32 * LFO_FREQ[(l >> 10 & 0x1F) as usize] * 256.0 / 44100.0) as u32;
        let sl = &mut self.slots[s];
        sl.plfo.step = step;
        sl.plfo.wave = (l >> 8 & 3) as u8;
        sl.plfo.scale = (l >> 5 & 7) as u8;
        sl.alfo.step = step;
        sl.alfo.wave = (l >> 3 & 3) as u8;
        sl.alfo.scale = (l & 7) as u8;
    }

    fn start(&mut self, regs: &mut [u8], s: usize) {
        self.stats.keyons += 1;
        self.compute_eg(regs, s);
        self.compute_lfo(regs, s);
        let step = Self::step_of(regs, s);
        let pcms = sr(regs, s, 0) >> 7 & 3;
        let base = sa(regs, s);
        let sl = &mut self.slots[s];
        sl.active = true;
        sl.backwards = false;
        sl.cur = 0;
        sl.nxt = 1 << SHIFT;
        sl.step = step;
        sl.eg_state = ATTACK;
        sl.eg_vol = 0x17F << EG_SHIFT;
        if pcms >= 2 {
            sl.curstep = 0;
            sl.adbase = base;
            sl.cur_sample = 0;
            sl.cur_quant = 0x7F;
            sl.lp_sample = 0;
            sl.lp_quant = 0x7F;
            if sr(regs, s, 0x08) > sr(regs, s, 0x0C) { w16(regs, s * 0x80 + 0x0C, 0xFFFF); }
        }
    }

    fn stop(&mut self, regs: &mut [u8], s: usize, keyoff: bool) {
        let sl = &mut self.slots[s];
        if keyoff { sl.eg_state = RELEASE; } else { sl.active = false; sl.lpend = true; }
        let v = sr(regs, s, 0) & !0x4000;
        w16(regs, s * 0x80, v);
    }

    /// A slot register was written.
    pub fn slot_write(&mut self, regs: &mut [u8], s: usize, off: usize) {
        match off & 0x7C {
            0x00 => {
                let v = sr(regs, s, 0);
                if v & 0x8000 != 0 {
                    // KYONEX: every slot follows its KYONB
                    for k in 0..64 {
                        let on = sr(regs, k, 0) & 0x4000 != 0;
                        if on && self.slots[k].eg_state == RELEASE { self.slots[k].lpend = false; self.start(regs, k); }
                        if !on { self.stop(regs, k, true); }
                    }
                    let v = sr(regs, s, 0) & !0x8000;
                    w16(regs, s * 0x80, v);
                }
            }
            0x18 => self.slots[s].step = Self::step_of(regs, s),
            0x14 => {
                let t = tables();
                let rr = sr(regs, s, 0x14) & 0x1F;
                self.slots[s].rr = t.dr[(rr << 1).min(63) as usize];
                self.slots[s].dl = 0x1F - (sr(regs, s, 0x14) >> 5 & 0x1F) as i32;
            }
            0x1C => self.compute_lfo(regs, s),
            _ => {}
        }
    }

    pub fn dsp_changed(&mut self) { self.dsp_dirty = true; }

    /// Steps in the DSP program (0 = none).
    pub fn dsp_steps(&self) -> usize { self.dsp.last_step }

    /// 2810: envelope / loop monitor of slot MSLC (reading clears the loop-end flag).
    pub fn monitor(&mut self, regs: &[u8]) -> u16 {
        let s = (r16(regs, 0x280C) >> 8 & 0x3F) as usize;
        let sl = &mut self.slots[s];
        let lp: u32 = if sl.lpend { 0x8000 } else { 0 };
        sl.lpend = false;
        if r16(regs, 0x280C) & 0x4000 != 0 { return lp as u16; }
        let sgc = ((sl.eg_state as u32) << 13) & 0x6000;
        let eg = if sl.active { sl.eg_vol } else { 0 } >> (EG_SHIFT - 13);
        let eg = (0x1FFF - eg).max(0) as u32;
        ((eg & 0x1FF8) | sgc | lp) as u16
    }

    /// 2814: play position of slot MSLC.
    pub fn position(&self, regs: &[u8]) -> u16 {
        let s = (r16(regs, 0x280C) >> 8 & 0x3F) as usize;
        (self.slots[s].cur >> SHIFT) as u16
    }

    fn eg_update(&mut self, regs: &mut [u8], s: usize) -> i32 {
        let d2r_zero = sr(regs, s, 0x10) >> 11 & 0x1F == 0;
        let lpslnk = sr(regs, s, 0x14) & 0x4000 != 0;
        let sl = &mut self.slots[s];
        match sl.eg_state {
            ATTACK => {
                sl.eg_vol += sl.ar;
                if sl.eg_vol >= 0x3FF << EG_SHIFT {
                    if !lpslnk && sl.d1r != 0 {
                        sl.eg_state = DECAY1;
                        if sl.d1r >= 1024 << EG_SHIFT && sl.d2r != 0 { sl.eg_state = DECAY2; }
                    }
                    sl.eg_vol = 0x3FF << EG_SHIFT;
                }
            }
            DECAY1 => {
                sl.eg_vol = (sl.eg_vol - sl.d1r).max(0);
                if sl.eg_vol >> (EG_SHIFT + 5) <= sl.dl { sl.eg_state = DECAY2; }
            }
            DECAY2 => {
                if d2r_zero { return (sl.eg_vol >> EG_SHIFT) << (SHIFT - 10); }
                sl.eg_vol = (sl.eg_vol - sl.d2r).max(0);
            }
            _ => {
                sl.eg_vol -= sl.rr;
                if sl.eg_vol <= 0 {
                    sl.eg_vol = 0;
                    self.stop(regs, s, false);
                }
            }
        }
        (self.slots[s].eg_vol >> EG_SHIFT) << (SHIFT - 10)
    }

    fn update_slot(&mut self, regs: &mut [u8], wave: &[u8], s: usize) -> i32 {
        let t = tables();
        let ctl = sr(regs, s, 0);
        if ctl >> 10 & 1 != 0 { return 0; } // noise: not emulated
        let pcms = ctl >> 7 & 3;
        let lpctl = ctl >> 9 & 1;
        let lsa = sr(regs, s, 0x08);
        let mut lea = sr(regs, s, 0x0C);
        if pcms == 3 { lea = (lea + 3) & !3; }
        let base = sa(regs, s);
        let m = wave.len() - 1;
        let rd8 = |a: u32| wave[a as usize & m] as i8 as i32;
        let rd16 = |a: u32| i16::from_le_bytes([wave[a as usize & m], wave[(a as usize + 1) & m]]) as i32;
        let sl = &mut self.slots[s];
        let mut step = sl.step as i32;
        if sl.plfo.scale != 0 {
            sl.plfo.phase = sl.plfo.phase.wrapping_add(sl.plfo.step as u16);
            let p = t.plfo[sl.plfo.wave as usize][(sl.plfo.phase >> LFO_SHIFT) as usize];
            let p = t.pscales[sl.plfo.scale as usize][(p + 128) as usize] << (SHIFT - LFO_SHIFT);
            step = (step * p) >> SHIFT;
        }
        let fpart = (sl.cur & ((1 << SHIFT) - 1)) as i32;
        let (addr1, addr2) = if pcms == 0 {
            ((sl.cur >> (SHIFT - 1)) & !1, (sl.nxt >> (SHIFT - 1)) & !1)
        } else {
            (sl.cur >> SHIFT, sl.nxt >> SHIFT)
        };
        let mut sample = match pcms {
            1 => {
                let (p1, p2) = (rd8(base + addr1) << 8, rd8(base + addr2) << 8);
                (p1 * ((1 << SHIFT) - fpart) + p2 * fpart) >> SHIFT
            }
            0 => {
                let (p1, p2) = (rd16(base + addr1), rd16(base + addr2));
                (p1 * ((1 << SHIFT) - fpart) + p2 * fpart) >> SHIFT
            }
            _ => {
                let mut b = sl.adbase;
                let steps_to_go = if addr1 > addr2 { lea } else { addr2 };
                let mut curstep = sl.curstep;
                let mut cur_sample = sl.cur_sample;
                while curstep < steps_to_go {
                    let shift = 4 & (curstep << 2);
                    let delta = (wave[b as usize & m] as u32 >> shift) & 0xF;
                    decode_adpcm(&mut sl.cur_sample, delta, &mut sl.cur_quant);
                    curstep += 1;
                    if curstep & 1 == 0 { b += 1; }
                    if curstep == addr1 { cur_sample = sl.cur_sample; }
                    if curstep == lsa { sl.lp_sample = sl.cur_sample; sl.lp_quant = sl.cur_quant; }
                }
                let nxt_sample = sl.cur_sample;
                sl.adbase = b;
                sl.curstep = curstep;
                (cur_sample * ((1 << SHIFT) - fpart) + nxt_sample * fpart) >> SHIFT
            }
        };
        sl.cur = sl.cur.wrapping_add(step as u32);
        sl.nxt = sl.cur.wrapping_add(1 << SHIFT);
        let (a1, a2) = (sl.cur >> SHIFT, sl.nxt >> SHIFT);
        let lpslnk = sr(regs, s, 0x14) & 0x4000 != 0;
        let sl = &mut self.slots[s];
        if a1 >= lsa && lpslnk && sl.eg_state == ATTACK && sl.d1r != 0 { sl.eg_state = DECAY1; }
        let mut stop = false;
        for (k, a) in [a1, a2].into_iter().enumerate() {
            match lpctl {
                0 => if a >= lsa && a >= lea { stop = true; },
                _ => {
                    if a >= lea {
                        sl.lpend = true;
                        let addr = if k == 0 { &mut sl.cur } else { &mut sl.nxt };
                        let rem = addr.wrapping_sub(lea << SHIFT);
                        *addr = (lsa << SHIFT).wrapping_add(rem);
                        if pcms >= 2 && k == 0 {
                            sl.adbase = base + lsa / 2;
                            sl.curstep = lsa;
                            if pcms == 2 { sl.cur_sample = sl.lp_sample; sl.cur_quant = sl.lp_quant; }
                        }
                    }
                }
            }
        }
        if stop { self.stop(regs, s, false); }
        let sl = &mut self.slots[s];
        if sl.alfo.scale != 0 {
            sl.alfo.phase = sl.alfo.phase.wrapping_add(sl.alfo.step as u16);
            let p = t.alfo[sl.alfo.wave as usize][(sl.alfo.phase >> LFO_SHIFT) as usize];
            let p = t.ascales[sl.alfo.scale as usize][p as usize] << (SHIFT - LFO_SHIFT);
            sample = (sample * p) >> SHIFT;
        }
        let attack = self.slots[s].eg_state == ATTACK;
        let eg = self.eg_update(regs, s);
        if attack { (sample * eg) >> SHIFT } else { (sample * t.eg[((eg >> (SHIFT - 10)) & 0x3FF) as usize]) >> SHIFT }
    }

    /// One 44.1 kHz output sample (left, right as -1..1), with the CD audio input.
    pub fn sample(&mut self, regs: &mut [u8], wave: &mut [u8], exts: [i16; 2]) -> (f32, f32) {
        let t = tables();
        let common = r16(regs, 0x2800);
        let mono = common & 0x8000 != 0;
        let (mut l, mut r) = (0i32, 0i32);
        let mut playing = 0;
        for s in 0..64 {
            if !self.slots[s].active { continue; }
            playing += 1;
            let smp = self.update_slot(regs, wave, s);
            let tl = sr(regs, s, 0x28) >> 8 & 0xFF;
            let mix = sr(regs, s, 0x20);
            let (isel, imxl) = (mix & 0xF, mix >> 4 & 0xF);
            let enc = (tl | imxl << 13) as usize;
            self.dsp.mixs[isel as usize] += (smp * t.lpan[enc]) >> (SHIFT - 2);
            let d = sr(regs, s, 0x24);
            let pan = if mono { 0 } else { d & 0x1F };
            let enc = (tl | pan << 8 | (d >> 8 & 0xF) << 13) as usize;
            l += (smp * t.lpan[enc]) >> SHIFT;
            r += (smp * t.rpan[enc]) >> SHIFT;
        }
        self.dsp_step(regs, wave, exts);
        let full = (32768i64 << 3) as f64;
        let st = &mut self.stats;
        st.samples += 1;
        st.playing += playing;
        st.direct += ((l as f64) / full).powi(2) + ((r as f64) / full).powi(2);
        let efs = |i: usize| r16(regs, 0x2000 + 4 * i);
        let (mut dsp_lr, mut cd_lr) = ([0i32; 2], [0i32; 2]);
        for i in 0..18 {
            let e = efs(i);
            let sdl = e >> 8 & 0xF;
            if sdl == 0 { continue; }
            let pan = if mono { 0 } else { e & 0x1F };
            let enc = (pan << 8 | sdl << 13) as usize;
            let v = if i < 16 { self.dsp.efreg[i] as i32 } else { exts[i - 16] as i32 };
            let (dl, dr) = ((v * t.lpan[enc]) >> SHIFT, (v * t.rpan[enc]) >> SHIFT);
            let acc = if i < 16 { &mut dsp_lr } else { &mut cd_lr };
            acc[0] += dl;
            acc[1] += dr;
            l += dl;
            r += dr;
        }
        let st = &mut self.stats;
        let sq = |v: [i32; 2]| ((v[0] as f64) / full).powi(2) + ((v[1] as f64) / full).powi(2);
        st.cd += sq(cd_lr);
        st.dsp += sq(dsp_lr);
        let (l, r) = if common & 0x100 != 0 {
            ((l >> 1).clamp(-131072, 131071), (r >> 1).clamp(-131072, 131071))
        } else {
            ((l >> 3).clamp(-32768, 32767), (r >> 3).clamp(-32768, 32767))
        };
        // the pan table carries 4x headroom for the mix; master volume is plain attenuation
        let mv = t.lpan[((common & 0xF) << 13) as usize] as f32 / 4.0;
        // full scale: 2^15 for the 16-bit DAC, 2^17 for the 18-bit one (DAC18B), so both
        // give the same loudness
        let full = if common & 0x100 != 0 { 131072.0 } else { 32768.0 };
        let scale = mv / (full * (1 << SHIFT) as f32);
        let (lo, ro) = (l as f32 * scale, r as f32 * scale);
        let st = &mut self.stats;
        st.out += (lo as f64).powi(2) + (ro as f64).powi(2);
        st.peak = st.peak.max(lo.abs()).max(ro.abs());
        (lo, ro)
    }

    fn dsp_step(&mut self, regs: &[u8], wave: &mut [u8], exts: [i16; 2]) {
        if self.dsp_dirty {
            self.dsp_dirty = false;
            self.dsp.last_step = (0..128).rev()
                .find(|&i| (0..4).any(|k| r16(regs, 0x3400 + i * 16 + k * 4) != 0)).map_or(0, |i| i + 1);
        }
        let d = &mut self.dsp;
        d.efreg = [0; 16];
        if d.last_step == 0 { d.mixs = [0; 16]; return; }
        let rb = r16(regs, 0x2804);
        let rbl = (8 * 1024u32) << (rb >> 13 & 3);
        let rbp = rb & 0xFFF;
        let m = wave.len() - 1;
        let (mut acc, mut memval, mut frc, mut y_reg, mut adrs) = (0i32, 0i32, 0i32, 0i32, 0u32);
        for step in 0..d.last_step {
            let ip = |k: usize| r16(regs, 0x3400 + step * 16 + k * 4);
            let (i0, i1, i2, i3) = (ip(0), ip(1), ip(2), ip(3));
            let tra = (i0 >> 9 & 0x7F) as usize;
            let twt = i0 >> 8 & 1 != 0;
            let twa = (i0 >> 1 & 0x7F) as usize;
            let xsel = i1 >> 15 & 1 != 0;
            let ysel = i1 >> 13 & 3;
            let ira = (i1 >> 7 & 0x3F) as usize;
            let iwt = i1 >> 6 & 1 != 0;
            let iwa = (i1 >> 1 & 0x1F) as usize;
            let table = i2 >> 15 & 1 != 0;
            let mwt = i2 >> 14 & 1 != 0;
            let mrd = i2 >> 13 & 1 != 0;
            let ewt = i2 >> 12 & 1 != 0;
            let ewa = (i2 >> 8 & 0xF) as usize;
            let adrl = i2 >> 7 & 1 != 0;
            let frcl = i2 >> 6 & 1 != 0;
            let shift = i2 >> 4 & 3;
            let yrl = i2 >> 3 & 1 != 0;
            let negb = i2 >> 2 & 1 != 0;
            let zero = i2 >> 1 & 1 != 0;
            let bsel = i2 & 1 != 0;
            let nofl = i3 >> 15 & 1 != 0;
            let masa = (i3 >> 9 & 0x1F) as usize;
            let adreb = i3 >> 8 & 1 != 0;
            let nxadr = i3 >> 7 & 1 != 0;
            let mut inputs = if ira <= 0x1F { d.mems[ira] }
                else if ira <= 0x2F { d.mixs[ira - 0x20] << 4 }
                else if ira <= 0x31 { (exts[ira - 0x30] as i32) << 8 }
                else { 0 };
            inputs = (inputs << 8) >> 8;
            if iwt {
                d.mems[iwa] = memval;
                if ira == iwa { inputs = memval; }
            }
            let tr = |d: &Dsp| (d.temp[(tra + d.dec as usize) & 0x7F] << 8) >> 8;
            let b = if zero { 0 } else {
                let b = if bsel { acc } else { tr(d) };
                if negb { -b } else { b }
            };
            let x = if xsel { inputs } else { tr(d) };
            let mut y = match ysel {
                0 => frc,
                1 => (r16(regs, 0x3000 + step * 4) as i16 as i32) >> 3,
                2 => (y_reg >> 11) & 0x1FFF,
                _ => (y_reg >> 4) & 0x0FFF,
            };
            if yrl { y_reg = inputs; }
            let shifted = match shift {
                0 => acc.clamp(-0x80_0000, 0x7F_FFFF),
                1 => (acc.wrapping_mul(2)).clamp(-0x80_0000, 0x7F_FFFF),
                2 => (acc.wrapping_mul(2) << 8) >> 8,
                _ => (acc << 8) >> 8,
            };
            y = (y << 19) >> 19;
            acc = (((x as i64) * (y as i64)) >> 12) as i32 + b;
            if twt { d.temp[(twa + d.dec as usize) & 0x7F] = shifted; }
            if frcl { frc = if shift == 3 { shifted & 0xFFF } else { (shifted >> 11) & 0x1FFF }; }
            if (mrd || mwt) && step & 1 != 0 {
                let mut a = r16(regs, 0x3200 + masa * 4);
                if !table { a = a.wrapping_add(d.dec); }
                if adreb { a = a.wrapping_add(adrs & 0xFFF); }
                if nxadr { a = a.wrapping_add(1); }
                a = if table { a & 0xFFFF } else { a & (rbl - 1) };
                // word address in the ring buffer (RBP = address bits 22-11)
                let byte = (((rbp << 10) + a) << 1) as usize & m;
                if mrd {
                    let w = u16::from_le_bytes([wave[byte], wave[(byte + 1) & m]]);
                    memval = if nofl { (w as i16 as i32) << 8 } else { unpack(w) };
                }
                if mwt {
                    let v = if nofl { (shifted >> 8) as u16 } else { pack(shifted) };
                    wave[byte..byte + 2].copy_from_slice(&v.to_le_bytes());
                }
            }
            if adrl { adrs = if shift == 3 { ((shifted >> 12) & 0xFFF) as u32 } else { (inputs >> 16) as u32 }; }
            if ewt { d.efreg[ewa] = d.efreg[ewa].wrapping_add((shifted >> 8) as i16); }
        }
        d.dec = d.dec.wrapping_sub(1);
        d.mixs = [0; 16];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn regs() -> Vec<u8> { vec![0u8; 0x8000] }
    fn put(r: &mut [u8], o: usize, v: u32) { w16(r, o, v); }

    /// Slot 0: a 16-bit looping square wave at 0x1000, full level, centre, max volume.
    fn setup(r: &mut [u8], wave: &mut [u8], pcms: u32) {
        for k in 0..64usize {
            let v: i16 = if k < 32 { 16000 } else { -16000 };
            wave[0x1000 + 2 * k..0x1002 + 2 * k].copy_from_slice(&v.to_le_bytes());
        }
        put(r, 0x2800, 0xF); // MVOL 15
        put(r, 0x04, 0x1000); // SA
        put(r, 0x08, 0); // LSA
        put(r, 0x0C, 64); // LEA
        put(r, 0x10, 0x1F); // AR fastest
        put(r, 0x14, 0x1F); // RR fastest
        put(r, 0x18, 0); // pitch: one sample a sample
        put(r, 0x24, 0xF << 8); // DISDL 15, centre
        put(r, 0x28, 0); // TL 0
        put(r, 0x00, 1 << 9 | pcms << 7); // loop
    }

    #[test]
    fn the_18_bit_dac_is_as_loud_as_the_16_bit_one() {
        let play = |dac18: bool| {
            let mut r = regs();
            let mut wave = vec![0u8; 0x20_0000];
            setup(&mut r, &mut wave, 0);
            if dac18 { put(&mut r, 0x2800, 0x10F); }
            let mut snd = Sound::default();
            put(&mut r, 0x00, 0xC000 | 1 << 9);
            snd.slot_write(&mut r, 0, 0);
            (0..20).map(|_| snd.sample(&mut r, &mut wave, [0, 0]).0).fold(0f32, |a, x| a.max(x.abs()))
        };
        let (a, b) = (play(false), play(true));
        assert!(a > 0.1 && (a - b).abs() < 0.01 && b <= 1.0, "{a} {b}");
    }

    #[test]
    fn keyed_slot_plays_its_samples_and_loops() {
        let mut r = regs();
        let mut wave = vec![0u8; 0x20_0000];
        setup(&mut r, &mut wave, 0);
        let mut snd = Sound::default();
        put(&mut r, 0x00, 0xC000 | 1 << 9);
        snd.slot_write(&mut r, 0, 0);
        assert!(snd.slots[0].active);
        assert_eq!(sr(&r, 0, 0) & 0x8000, 0, "KYONEX clears itself");
        let out: Vec<f32> = (0..200).map(|_| snd.sample(&mut r, &mut wave, [0, 0]).0).collect();
        // samples 0-31 high, 32-63 low (one output sample per input sample)
        let hi = out[5..30].iter().all(|&s| s > 0.05);
        let lo = out[35..62].iter().all(|&s| s < -0.05);
        assert!(hi && lo, "{:?}", &out[0..100]);
        assert!(out[66..94].iter().all(|&s| s > 0.05), "looped");
        assert!(snd.slots[0].lpend);
        // the speed log's numbers: one key-on, a loud direct output, nothing from the DSP
        assert_eq!((snd.stats.keyons, snd.stats.samples), (1, 200));
        assert!(snd.stats.direct > 0.0 && snd.stats.out > 0.0 && snd.stats.dsp == 0.0 && snd.stats.peak > 0.05);
        assert_eq!(SndStats::db(0.0, 10), "silent");
        // key off: released, then silent and inactive
        put(&mut r, 0x00, 0x8000 | 1 << 9);
        snd.slot_write(&mut r, 0, 0);
        for _ in 0..5000 { snd.sample(&mut r, &mut wave, [0, 0]); }
        assert!(!snd.slots[0].active);
        assert_eq!(snd.sample(&mut r, &mut wave, [0, 0]), (0.0, 0.0));
    }

    #[test]
    fn non_looping_slot_stops_at_the_end_and_monitor_reports() {
        let mut r = regs();
        let mut wave = vec![0u8; 0x20_0000];
        setup(&mut r, &mut wave, 1);
        put(&mut r, 0x00, 0xC000 | 1 << 7); // 8-bit, no loop
        let mut snd = Sound::default();
        snd.slot_write(&mut r, 0, 0);
        put(&mut r, 0x280C, 0); // monitor slot 0
        assert_eq!(snd.monitor(&r) & 0x6000, 0, "attack");
        for _ in 0..80 { snd.sample(&mut r, &mut wave, [0, 0]); }
        assert!(!snd.slots[0].active, "8-bit: 64 bytes then stop");
        assert_eq!(snd.monitor(&r) & 0x8000, 0x8000, "loop end flag");
        assert_eq!(snd.monitor(&r) & 0x8000, 0, "cleared by the read");
    }

    #[test]
    fn adpcm_decodes_toward_the_signal() {
        let mut sig = 0;
        let mut q = 0x7F;
        // repeated +7 deltas climb, -7 descend
        for _ in 0..20 { decode_adpcm(&mut sig, 7, &mut q); }
        assert!(sig > 10000, "{sig}");
        let top = sig;
        for _ in 0..5 { decode_adpcm(&mut sig, 15, &mut q); }
        assert!(sig < top);
    }

    #[test]
    fn dsp_float_format_round_trips() {
        for v in [0, 1000, -1000, 0x12_3400, -0x40_0000, 0x7F_F800] {
            let u = unpack(pack(v));
            assert!((u - v).abs() <= (v.abs() >> 10).max(16), "{v} → {u}");
        }
    }

    #[test]
    fn cd_input_reaches_the_output_through_efsdl() {
        let mut r = regs();
        let mut wave = vec![0u8; 0x1000];
        let mut snd = Sound::default();
        put(&mut r, 0x2800, 0xF);
        assert_eq!(snd.sample(&mut r, &mut wave, [16000, -16000]), (0.0, 0.0), "EFSDL 0: muted");
        // pan 1F silences the right side (hard left), 0F the left (hard right)
        put(&mut r, 0x2000 + 16 * 4, 0xF << 8 | 0x1F); // CD left channel: hard left
        put(&mut r, 0x2000 + 17 * 4, 0xF << 8 | 0x0F); // CD right channel: hard right
        let (l, rr) = snd.sample(&mut r, &mut wave, [16000, -16000]);
        assert!(l > 0.2 && rr < -0.2, "{l} {rr}");
    }

    #[test]
    fn eight_bit_loop_restarts_at_lsa() {
        let mut r = regs();
        let mut wave = vec![0u8; 0x20_0000];
        // 8-bit: 0-15 = +30, 16-31 = -30 (×256 = ±7680 → ±0.47 out); loop 16..32
        for k in 0..32 { wave[0x3000 + k] = if k < 16 { 30 } else { (-30i8) as u8 }; }
        put(&mut r, 0x2800, 0xF);
        put(&mut r, 0x04, 0x3000);
        put(&mut r, 0x08, 16);
        put(&mut r, 0x0C, 32);
        put(&mut r, 0x10, 0x1F);
        put(&mut r, 0x24, 0xF << 8);
        put(&mut r, 0x00, 0xC000 | 1 << 9 | 1 << 7);
        let mut snd = Sound::default();
        snd.slot_write(&mut r, 0, 0);
        let out: Vec<f32> = (0..80).map(|_| snd.sample(&mut r, &mut wave, [0, 0]).0).collect();
        assert!(out[2..14].iter().all(|&s| (0.1..0.1375).contains(&s)), "{:?}", &out[..20]);
        // after the first pass only the loop part (negative) plays
        assert!(out[18..80].iter().all(|&s| s < -0.1), "{:?}", &out[14..80]);
    }
}
