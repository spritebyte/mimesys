//! The SCSP (Yamaha YMF292-F, Saturn Custom Sound Processor): 32 slots of PCM / FM sound
//! with envelopes and LFOs, an effect DSP, three timers and the interrupt controllers for
//! the 68000 and the main SH-2, all in front of 512 KB of sound RAM. Ported from MAME's
//! devices/sound/scsp.cpp and scspdsp.cpp (ElSemi, R. Belmont, kingshriek; BSD-3-Clause).
//!
//! - Registers (68000 0x100000, SH-2 0x05B00000): slot s at s x 0x20 (16 words: key on
//!   0, SA 0-2, LSA 4, LEA 6, envelope 8 / A, TL C, modulation E, pitch 10, LFO 12,
//!   DSP send 14, direct / effect level 16); common 0x400-0x42F (MVOL 400, ring buffer 402,
//!   MIDI 404, MSLC / CA 408, DMA 412-416, timers A-C 418-41C, 68000 interrupts SCIEB /
//!   SCIPD / SCIRE 41E-422 and their levels SCILV0-2 424-428, main CPU interrupts MCIEB /
//!   MCIPD / MCIRE 42A-42E); the FM ring buffer 600; DSP COEF 700, MADRS 780, MPRO 800,
//!   TEMP C00, MEMS E00, MIXS E80, EFREG EC0, EXTS EE0.
//! - One sample every 512 SCSP clocks: 44100 Hz.
//! - The timers count in samples (prescaled by 1 << TxCTL) from their 8-bit start value
//!   and interrupt once at the end.
//! - Not yet: MIDI (always empty), the DMA end interrupt, EXTS (CD audio) input.

use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

pub const RAM_SIZE: usize = 0x8_0000;

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
const SDLT: [f32; 8] = [-1000000.0, -36.0, -30.0, -24.0, -18.0, -12.0, -6.0, 0.0];
const LFO_FREQ: [f32; 32] = [0.17, 0.19, 0.23, 0.27, 0.34, 0.39, 0.45, 0.55, 0.68, 0.78, 0.92, 1.10, 1.39, 1.60, 1.87, 2.27,
    2.87, 3.31, 3.92, 4.79, 6.15, 7.18, 8.60, 10.8, 14.4, 17.2, 21.5, 28.7, 43.1, 57.4, 86.1, 172.3];
const ASCALE: [f32; 8] = [0.0, 0.4, 0.8, 1.5, 3.0, 6.0, 12.0, 24.0];
const PSCALE: [f32; 8] = [0.0, 7.0, 13.5, 27.0, 55.0, 112.0, 230.0, 494.0];

/// SCIPD / SCIEB bits
const SCI_MIDI: u16 = 0x08;
const SCI_CPU: u16 = 0x20;
const SCI_TIMA: u16 = 0x40;
const SCI_TIMB: u16 = 0x80;
const SCI_TIMC: u16 = 0x100;

/// Fixed tables (MAME init / LFO_Init)
struct Tables {
    eg: Vec<i32>,
    lpan: Vec<i32>,
    rpan: Vec<i32>,
    ar: [i32; 64],
    dr: [i32; 64],
    /// [saw, square, triangle, noise][phase]
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
        let mut lpan = vec![0; 0x10000];
        let mut rpan = vec![0; 0x10000];
        for i in 0..0x10000usize {
            let (itl, ipan, isdl) = (i & 0xFF, i >> 8 & 0x1F, i >> 13 & 7);
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
            let u = i as usize;
            alfo[0][u] = 255 - i;
            plfo[0][u] = if i < 128 { i } else { i - 256 };
            alfo[1][u] = if i < 128 { 255 } else { 0 };
            plfo[1][u] = if i < 128 { 127 } else { -128 };
            alfo[2][u] = if i < 128 { 255 - i * 2 } else { i * 2 - 256 };
            plfo[2][u] = if i < 64 { i * 2 } else if i < 128 { 255 - i * 2 } else if i < 192 { 256 - i * 2 } else { i * 2 - 511 };
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            let a = (seed >> 16 & 0xFF) as i32;
            alfo[3][u] = a;
            plfo[3][u] = 128 - a;
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
struct Lfo { phase: u16, step: u32, wave: u8, scale: u8 }

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
struct Slot {
    active: bool,
    backwards: bool,
    cur: u32,
    nxt: u32,
    step: u32,
    eg_state: u8,
    eg_vol: i32,
    ar: i32,
    d1r: i32,
    d2r: i32,
    rr: i32,
    dl: i32,
    eghold: bool,
    plfo: Lfo,
    alfo: Lfo,
}

/// The effect DSP (MAME scspdsp.cpp)
#[derive(Clone, Serialize, Deserialize)]
struct Dsp {
    coef: Vec<i16>,
    madrs: Vec<u16>,
    mpro: Vec<u16>,
    temp: Vec<i32>,
    mems: Vec<i32>,
    mixs: [i32; 16],
    exts: [i16; 2],
    efreg: [i16; 16],
    dec: u32,
    rbp: u32,
    rbl: u32,
    stopped: bool,
    last_step: usize,
}

impl Default for Dsp {
    fn default() -> Self {
        Self { coef: vec![0; 64], madrs: vec![0; 32], mpro: vec![0; 512], temp: vec![0; 128], mems: vec![0; 32],
               mixs: [0; 16], exts: [0; 2], efreg: [0; 16], dec: 0, rbp: 0, rbl: 8 * 1024, stopped: true, last_step: 0 }
    }
}

fn sext(v: i32, bits: u32) -> i32 { (v << (32 - bits)) >> (32 - bits) }

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

#[derive(Clone, Serialize, Deserialize)]
pub struct Scsp {
    #[serde(with = "serde_bytes")]
    pub ram: Vec<u8>,
    /// slot registers: 32 x 16 words
    sregs: Vec<u16>,
    /// common registers 0x400-0x42F
    cregs: [u16; 0x18],
    slots: Vec<Slot>,
    ringbuf: Vec<i16>,
    bufptr: usize,
    /// samples until timers A-C fire (0 = stopped)
    tim_left: [u32; 3],
    tim_done: [bool; 3],
    mcieb: u16,
    mcipd: u16,
    latched_mslc: usize,
    mslc_data: u16,
    /// 68000 interrupt lines asserted (bit n = level n)
    lines: u8,
    dsp: Dsp,
    #[serde(skip)]
    pub out: Vec<f32>,
    /// debug: key-ons and the loudest output since the last report
    #[serde(skip)]
    pub keyons: u32,
    #[serde(skip)]
    pub peak: f32,
    /// the main CPU's sound request went up
    #[serde(skip)]
    pub main_irq: bool,
    noise: u32,
}

impl Scsp {
    pub fn new() -> Self {
        let mut slots = vec![Slot::default(); 32];
        for s in &mut slots { s.eg_state = RELEASE; }
        Self {
            ram: vec![0; RAM_SIZE], sregs: vec![0; 32 * 16], cregs: [0; 0x18], slots, ringbuf: vec![0; 128], bufptr: 0,
            tim_left: [0; 3], tim_done: [true; 3], mcieb: 0, mcipd: 0, latched_mslc: 0, mslc_data: 0, lines: 0,
            dsp: Dsp::default(), out: Vec::new(), main_irq: false, noise: 1, keyons: 0, peak: 0.0,
        }
    }

    /// The 68000's interrupt level (the highest line asserted)
    pub fn ipl(&self) -> u8 { if self.lines == 0 { 0 } else { 7 - self.lines.leading_zeros() as u8 } }

    // ---------------- sound RAM (big-endian) ----------------

    pub fn ram_word(&self, a: u32) -> u16 {
        let a = a as usize & (RAM_SIZE - 2);
        u16::from_be_bytes([self.ram[a], self.ram[a + 1]])
    }

    fn ram_set(&mut self, a: u32, v: u16) {
        let a = a as usize & (RAM_SIZE - 2);
        self.ram[a..a + 2].copy_from_slice(&v.to_be_bytes());
    }

    // ---------------- registers ----------------

    fn c(&self, off: usize) -> u16 { self.cregs[(off & 0x3F) / 2] }

    fn sv(&self, s: usize, w: usize) -> u16 { self.sregs[s * 16 + w] }

    /// A register word (with read side effects: the MSLC monitor)
    pub fn read_reg(&mut self, off: u32) -> u16 {
        let a = off as usize & 0xFFE;
        match a {
            0..=0x3FF => self.sregs[a / 2],
            0x400..=0x42F => match a & 0x3F {
                0x04 => 0x0800 | 0x0100,    // MIDI: out empty, in empty
                0x08 => self.mslc_data,
                0x2A => self.mcieb,
                0x2C => self.mcipd,
                o => self.cregs[o / 2],
            },
            0x430..=0x5FF => 0,
            0x600..=0x6FF => self.ringbuf[(a - 0x600) / 2] as u16,
            0x700..=0x77F => self.dsp.coef[(a - 0x700) / 2] as u16,
            0x780..=0x7BF => self.dsp.madrs[(a - 0x780) / 2],
            0x7C0..=0x7FF => self.dsp.madrs[(a - 0x7C0) / 2],
            0x800..=0xBFF => self.dsp.mpro[(a - 0x800) / 2],
            0xC00..=0xDFF => { let t = self.dsp.temp[(a >> 2) & 0x7F]; if a & 2 != 0 { t as u16 } else { (t >> 16) as u16 } }
            0xE00..=0xE7F => { let t = self.dsp.mems[(a >> 2) & 0x1F]; if a & 2 != 0 { t as u16 } else { (t >> 16) as u16 } }
            0xE80..=0xEBF => { let t = self.dsp.mixs[(a >> 2) & 0xF]; if a & 2 != 0 { t as u16 } else { (t >> 16) as u16 } }
            0xEC0..=0xEDF => self.dsp.efreg[(a - 0xEC0) / 2] as u16,
            0xEE0..=0xEE3 => self.dsp.exts[(a - 0xEE0) / 2] as u16,
            _ => 0,
        }
    }

    /// A register word written (`mask`: the bytes written)
    pub fn write_reg(&mut self, off: u32, v: u16, mask: u16) {
        let a = off as usize & 0xFFE;
        let old = match a {
            0x400..=0x42F => self.cregs[(a & 0x3F) / 2],
            0x430..=0x5FF => 0,
            _ => self.read_reg(a as u32),
        };
        let v = (old & !mask) | (v & mask);
        match a {
            0..=0x3FF => {
                self.sregs[a / 2] = v;
                self.slot_reg(a / 0x20, a & 0x1F);
            }
            0x400..=0x42F => {
                let o = a & 0x3F;
                match o {
                    // SCIPD / MCIPD: only the CPU interrupt bit can be set
                    0x20 => self.cregs[o / 2] |= v & SCI_CPU,
                    0x2C => {}
                    _ => self.cregs[o / 2] = v,
                }
                self.common_reg(o, v);
            }
            0x600..=0x6FF => self.ringbuf[(a - 0x600) / 2] = v as i16,
            0x700..=0x77F => self.dsp.coef[(a - 0x700) / 2] = v as i16,
            0x780..=0x7BF => self.dsp.madrs[(a - 0x780) / 2] = v,
            0x800..=0xBFF => {
                self.dsp.mpro[(a - 0x800) / 2] = v;
                if a == 0xBF0 { self.dsp_start(); }
            }
            _ => {}
        }
    }

    fn slot_reg(&mut self, s: usize, r: usize) {
        match r {
            0 | 1 => {
                if self.sv(s, 0) & 0x1000 != 0 {
                    // KEYONEX: every slot's KEYONB takes effect
                    for k in 0..32 {
                        let on = self.sv(k, 0) & 0x0800 != 0;
                        if on && self.slots[k].eg_state == RELEASE { self.start_slot(k); }
                        if !on { self.stop_slot(k, true); }
                    }
                    self.sregs[s * 16] &= !0x1000;
                }
            }
            0x10 | 0x11 => self.slots[s].step = self.step_of(s),
            0x0A | 0x0B => {
                self.slots[s].rr = Self::get_dr(&tables().dr, 0, (self.sv(s, 5) & 0x1F) as i32);
                self.slots[s].dl = 0x1F - ((self.sv(s, 5) >> 5) & 0x1F) as i32;
            }
            0x12 | 0x13 => self.compute_lfo(s),
            _ => {}
        }
    }

    fn common_reg(&mut self, o: usize, v: u16) {
        match o {
            0x02 => {
                self.dsp.rbl = (8 * 1024) << ((v >> 7) & 3);
                self.dsp.rbp = (v & 0x3F) as u32;
            }
            0x08 => self.latched_mslc = ((v >> 11) & 0x1F) as usize,
            0x16 => if v & 0x1000 != 0 { self.dma(); },
            0x18 | 0x1A | 0x1C => {
                let t = (o - 0x18) / 2;
                let pris = 1u32 << ((v >> 8) & 7);
                let val = (v & 0xFF) as u32;
                self.tim_done[t] = false;
                self.tim_left[t] = if val != 255 { (255 - val) * pris } else { 0 };
            }
            0x1E => self.check_irq(),
            0x20 => if self.c(0x1E) & self.c(0x20) & SCI_CPU != 0 { self.check_irq(); },
            0x22 => {
                let pd = self.c(0x20) & !v;
                self.cregs[0x20 / 2] = pd;
                self.reset_irqs(v);
                // a timer that has run out comes straight back (MAME: real hardware)
                let bits = [SCI_TIMA, SCI_TIMB, SCI_TIMC];
                for k in 0..3 { if self.tim_done[k] { self.cregs[0x20 / 2] |= bits[k]; } }
            }
            0x24..=0x29 => {}
            0x2A => { self.mcieb = v; self.main_check(0); }
            0x2C => if v & 0x20 != 0 { self.main_check(0x20); },
            0x2E => { self.mcipd &= !v; self.main_check(0); }
            _ => {}
        }
    }

    /// The level SCILV0-2 give an interrupt bit
    fn level_of(&self, bit: u32) -> u8 {
        let b = |r: usize| ((self.c(r) >> bit) & 1) as u8;
        b(0x24) | b(0x26) << 1 | b(0x28) << 2
    }

    fn assert_line(&mut self, level: u8) { if level != 0 { self.lines |= 1 << level; } }

    fn clear_line(&mut self, level: u8) { if level != 0 { self.lines &= !(1 << level); } }

    /// MAME CheckPendingIRQ: the first pending, enabled source asserts its level
    fn check_irq(&mut self) {
        let pend = self.c(0x20);
        let en = self.c(0x1E);
        if pend == 0 { return; }
        if pend & en & SCI_CPU != 0 { let l = self.level_of(5); self.assert_line(l); return; }
        if pend & en & SCI_TIMA != 0 { let l = self.level_of(6); self.assert_line(l); return; }
        if pend & en & (SCI_TIMB | SCI_TIMC) != 0 { let l = self.level_of(7); self.assert_line(l); return; }
        if pend & en & SCI_MIDI != 0 { let l = self.level_of(3); self.assert_line(l); }
    }

    fn reset_irqs(&mut self, reset: u16) {
        if reset & SCI_TIMA != 0 { let l = self.level_of(6); self.clear_line(l); }
        if reset & (SCI_TIMB | SCI_TIMC) != 0 { let l = self.level_of(7); self.clear_line(l); }
        if reset & SCI_CPU != 0 { let l = self.level_of(5); self.clear_line(l); }
        if reset & SCI_MIDI != 0 { let l = self.level_of(3); self.clear_line(l); }
        self.check_irq();
    }

    fn main_check(&mut self, bits: u16) {
        self.mcipd |= bits;
        if self.mcipd & self.mcieb != 0 { self.main_irq = true; }
    }

    /// SCSP DMA between sound RAM and the registers, at once
    fn dma(&mut self) {
        let (r12, r14, r16) = (self.c(0x12), self.c(0x14), self.c(0x16));
        let mut dmea = (r12 & 0xFFFE) as u32 | ((r14 & 0xF000) as u32) << 4;
        let mut drga = (r14 & 0x0FFE) as u32;
        let dtlg = (r16 & 0x0FFE) as u32;
        let ddir = r16 & 0x2000 != 0;
        let dgate = r16 & 0x4000 != 0;
        for _ in (0..dtlg).step_by(2) {
            if ddir {
                let v = if dgate { 0 } else { self.read_reg(drga) };
                self.ram_set(dmea, v);
            } else {
                let v = if dgate { 0 } else { self.ram_word(dmea) };
                self.write_reg(drga, v, 0xFFFF);
            }
            dmea += 2;
            drga += 2;
        }
        self.cregs[0x16 / 2] &= !0x1000;
    }

    // ---------------- slots ----------------

    fn get_ar(t: &[i32; 64], base: i32, r: i32) -> i32 { t[(base + (r << 1)).clamp(0, 63) as usize] }
    fn get_dr(t: &[i32; 64], base: i32, r: i32) -> i32 { t[(base + (r << 1)).clamp(0, 63) as usize] }

    fn compute_eg(&mut self, s: usize) {
        let t = tables();
        let oct = ((self.sv(s, 8) >> 11 & 0xF) as i32 ^ 8) - 8;
        let krs = ((self.sv(s, 5) >> 10) & 0xF) as i32;
        let fns = (self.sv(s, 8) & 0x3FF) as i32;
        let rate = if krs != 0xF { oct + 2 * krs + ((fns >> 9) & 1) } else { 0 };
        let e = self.sv(s, 4);
        let (ar, d1r, d2r) = ((e & 0x1F) as i32, ((e >> 6) & 0x1F) as i32, ((e >> 11) & 0x1F) as i32);
        let rr = (self.sv(s, 5) & 0x1F) as i32;
        let dl = 0x1F - ((self.sv(s, 5) >> 5) & 0x1F) as i32;
        let sl = &mut self.slots[s];
        sl.eg_vol = 0x17F << EG_SHIFT;
        sl.ar = Self::get_ar(&t.ar, rate, ar);
        sl.d1r = Self::get_dr(&t.dr, rate, d1r);
        sl.d2r = Self::get_dr(&t.dr, rate, d2r);
        sl.rr = Self::get_dr(&t.dr, rate, rr);
        sl.dl = dl;
        sl.eghold = e & 0x20 != 0;
    }

    fn step_of(&self, s: usize) -> u32 {
        let oct = ((self.sv(s, 8) >> 11 & 0xF) as i32 ^ 8) - 8 + SHIFT as i32 - 10;
        let f = (self.sv(s, 8) & 0x3FF) as u32 + 0x400;
        if oct >= 0 { f << oct } else { f >> -oct }
    }

    fn compute_lfo(&mut self, s: usize) {
        let l = self.sv(s, 9);
        let step = ((1u32 << LFO_SHIFT) as f32 * LFO_FREQ[((l >> 10) & 0x1F) as usize] * 256.0 / 44100.0) as u32;
        let sl = &mut self.slots[s];
        sl.plfo.step = step;
        sl.plfo.wave = ((l >> 8) & 3) as u8;
        sl.plfo.scale = ((l >> 5) & 7) as u8;
        sl.alfo.step = step;
        sl.alfo.wave = ((l >> 3) & 3) as u8;
        sl.alfo.scale = (l & 7) as u8;
    }

    fn start_slot(&mut self, s: usize) {
        self.keyons += 1;
        let step = self.step_of(s);
        self.compute_eg(s);
        self.compute_lfo(s);
        let sl = &mut self.slots[s];
        sl.active = true;
        sl.cur = 0;
        sl.nxt = 1 << SHIFT;
        sl.step = step;
        sl.eg_state = ATTACK;
        sl.eg_vol = 0x17F << EG_SHIFT;
        sl.backwards = false;
    }

    fn stop_slot(&mut self, s: usize, keyoff: bool) {
        if keyoff { self.slots[s].eg_state = RELEASE; } else { self.slots[s].active = false; }
        self.sregs[s * 16] &= !0x0800;
    }

    fn eg_update(&mut self, s: usize) -> i32 {
        let lpslnk = self.sv(s, 5) & 0x4000 != 0;
        let d2r_zero = (self.sv(s, 4) >> 11) & 0x1F == 0;
        let sl = &mut self.slots[s];
        match sl.eg_state {
            ATTACK => {
                sl.eg_vol += sl.ar;
                if sl.eg_vol >= 0x3FF << EG_SHIFT {
                    if !lpslnk {
                        sl.eg_state = DECAY1;
                        if sl.d1r >= 1024 << EG_SHIFT { sl.eg_state = DECAY2; }
                    }
                    sl.eg_vol = 0x3FF << EG_SHIFT;
                }
                if sl.eghold { return 0x3FF << (SHIFT - 10); }
            }
            DECAY1 => {
                sl.eg_vol -= sl.d1r;
                if sl.eg_vol <= 0 { sl.eg_vol = 0; }
                if sl.eg_vol >> (EG_SHIFT + 5) <= sl.dl { sl.eg_state = DECAY2; }
            }
            DECAY2 => {
                if d2r_zero { return (sl.eg_vol >> EG_SHIFT) << (SHIFT - 10); }
                sl.eg_vol -= sl.d2r;
                if sl.eg_vol <= 0 { sl.eg_vol = 0; }
            }
            _ => {
                sl.eg_vol -= sl.rr;
                if sl.eg_vol <= 0 {
                    sl.eg_vol = 0;
                    self.stop_slot(s, false);
                }
            }
        }
        (self.slots[s].eg_vol >> EG_SHIFT) << (SHIFT - 10)
    }

    fn lfo_step(l: &mut Lfo, alfo: bool) -> i32 {
        let t = tables();
        l.phase = l.phase.wrapping_add(l.step as u16);
        let i = (l.phase >> LFO_SHIFT) as usize & 0xFF;
        let p = if alfo {
            t.ascales[l.scale as usize][t.alfo[l.wave as usize][i] as usize & 0xFF]
        } else {
            t.pscales[l.scale as usize][(t.plfo[l.wave as usize][i] + 128) as usize & 0xFF]
        };
        p << (SHIFT - LFO_SHIFT)
    }

    fn ram_byte(&self, a: u32) -> u8 { self.ram[a as usize & (RAM_SIZE - 1)] }

    /// One slot's sample (MAME UpdateSlot)
    fn update_slot(&mut self, s: usize) -> i32 {
        let w0 = self.sv(s, 0);
        let ssctl = (w0 >> 7) & 3;
        if ssctl == 3 { return 0; }
        let pcm8 = w0 & 0x10 != 0;
        let sa = ((w0 & 0xF) as u32) << 16 | self.sv(s, 1) as u32;
        let lsa = self.sv(s, 2) as u32;
        let lea = self.sv(s, 3) as u32;
        let lpctl = (w0 >> 5) & 3;
        let sbctl = (w0 >> 9) & 3;
        let w7 = self.sv(s, 7);
        let (mdl, mdxsl, mdysl) = ((w7 >> 12) & 0xF, ((w7 >> 6) & 0x3F) as usize, (w7 & 0x3F) as usize);
        let w9 = self.sv(s, 9);
        let (plfos, alfos) = ((w9 >> 5) & 7, w9 & 7);
        let w6 = self.sv(s, 6);
        let (sdir, stwinh, tl) = (w6 & 0x100 != 0, w6 & 0x200 != 0, w6 & 0xFF);
        let lpslnk = self.sv(s, 5) & 0x4000 != 0;

        let mut step = self.slots[s].step as i64;
        if plfos != 0 {
            let mut l = self.slots[s].plfo;
            step = (step * Self::lfo_step(&mut l, false) as i64) >> SHIFT;
            self.slots[s].plfo = l;
        }
        let step = step as u32;
        let sl = self.slots[s];
        let (mut a1, mut a2) = if pcm8 { (sl.cur >> SHIFT, sl.nxt >> SHIFT) } else { ((sl.cur >> (SHIFT - 1)) & !1, (sl.nxt >> (SHIFT - 1)) & !1) };
        if mdl != 0 || mdxsl != 0 || mdysl != 0 {
            let mut smp = (self.ringbuf[(self.bufptr + mdxsl) & 63] as i32 + self.ringbuf[(self.bufptr + mdysl) & 63] as i32) / 2;
            smp <<= 0xA;
            smp >>= 0x1A - mdl as i32;
            if !pcm8 { smp <<= 1; }
            a1 = a1.wrapping_add(smp as u32);
            a2 = a2.wrapping_add(smp as u32);
        }
        let mut sample: i32 = match ssctl {
            0 => {
                let fpart = (sl.cur & ((1 << SHIFT) - 1)) as i32;
                let (p1, p2) = if pcm8 {
                    ((self.ram_byte(sa.wrapping_add(a1)) as i8 as i32) << 8, (self.ram_byte(sa.wrapping_add(a2)) as i8 as i32) << 8)
                } else {
                    (self.ram_word(sa.wrapping_add(a1)) as i16 as i32, self.ram_word(sa.wrapping_add(a2)) as i16 as i32)
                };
                (p1 * ((1 << SHIFT) - fpart) + p2 * fpart) >> SHIFT
            }
            1 => {
                self.noise = self.noise.wrapping_mul(1_103_515_245).wrapping_add(12345);
                (self.noise >> 16) as i16 as i32
            }
            _ => 0,
        };
        if sbctl & 1 != 0 { sample ^= 0x7FFF; }
        if sbctl & 2 != 0 { sample = (sample ^ 0x8000) as i16 as i32; }

        let sl = &mut self.slots[s];
        if sl.backwards { sl.cur = sl.cur.wrapping_sub(step); } else { sl.cur = sl.cur.wrapping_add(step); }
        sl.nxt = sl.cur.wrapping_add(1 << SHIFT);
        let (ad1, ad2) = (sl.cur >> SHIFT, sl.nxt >> SHIFT);
        if ad1 >= lsa && !sl.backwards && lpslnk && sl.eg_state == ATTACK { sl.eg_state = DECAY1; }
        let mut stop = false;
        for k in 0..2 {
            let ad = if k == 0 { ad1 } else { ad2 };
            let sa_ref = if k == 0 { &mut sl.cur } else { &mut sl.nxt };
            match lpctl {
                0 => if ad >= lsa && ad >= lea { stop = true; },
                1 => if ad >= lea {
                    let rem = sa_ref.wrapping_sub(lea << SHIFT);
                    *sa_ref = (lsa << SHIFT).wrapping_add(rem);
                },
                2 => {
                    if ad >= lsa && !sl.backwards {
                        let rem = sa_ref.wrapping_sub(lsa << SHIFT);
                        *sa_ref = (lea << SHIFT).wrapping_sub(rem);
                        sl.backwards = true;
                    } else if (ad < lsa || *sa_ref & 0x8000_0000 != 0) && sl.backwards {
                        let rem = (lsa << SHIFT).wrapping_sub(*sa_ref);
                        *sa_ref = (lea << SHIFT).wrapping_sub(rem);
                    }
                }
                _ => {
                    if ad >= lea {
                        let rem = sa_ref.wrapping_sub(lea << SHIFT);
                        *sa_ref = (lea << SHIFT).wrapping_sub(rem);
                        sl.backwards = true;
                    } else if (ad < lsa || *sa_ref & 0x8000_0000 != 0) && sl.backwards {
                        let rem = (lsa << SHIFT).wrapping_sub(*sa_ref);
                        *sa_ref = (lsa << SHIFT).wrapping_add(rem);
                        sl.backwards = false;
                    }
                }
            }
        }
        if stop { self.stop_slot(s, false); }

        if !sdir {
            if alfos != 0 {
                let mut l = self.slots[s].alfo;
                sample = (sample as i64 * Self::lfo_step(&mut l, true) as i64 >> SHIFT) as i32;
                self.slots[s].alfo = l;
            }
            let attack = self.slots[s].eg_state == ATTACK;
            let e = self.eg_update(s);
            sample = if attack { (sample as i64 * e as i64 >> SHIFT) as i32 } else {
                (sample as i64 * tables().eg[((e >> (SHIFT - 10)) as usize).min(0x3FF)] as i64 >> SHIFT) as i32
            };
        }
        if !stwinh {
            let enc = if sdir { 7 << 13 } else { tl as usize | 7 << 13 };
            self.ringbuf[self.bufptr] = ((sample as i64 * tables().lpan[enc] as i64) >> (SHIFT + 1)) as i16;
        }
        sample
    }

    /// One 44.1 kHz sample: the slots, the DSP, the mix; then the timers
    pub fn sample(&mut self) {
        let t = tables();
        let (mut l, mut r) = (0i64, 0i64);
        for s in 0..32 {
            if self.slots[s].active {
                let smp = self.update_slot(s) as i64;
                let w6 = self.sv(s, 6);
                let tl = if w6 & 0x100 != 0 { 0 } else { (w6 & 0xFF) as usize };
                let wa = self.sv(s, 10);
                let (isel, imxl) = (((wa >> 3) & 0xF) as usize, (wa & 7) as usize);
                let enc = tl | imxl << 13;
                self.dsp.mixs[isel] += ((smp * t.lpan[enc] as i64) >> (SHIFT - 2)) as i32;
                let wb = self.sv(s, 11);
                let (disdl, dipan) = (((wb >> 13) & 7) as usize, ((wb >> 8) & 0x1F) as usize);
                let enc = tl | dipan << 8 | disdl << 13;
                l += (smp * t.lpan[enc] as i64) >> SHIFT;
                r += (smp * t.rpan[enc] as i64) >> SHIFT;
            }
            self.bufptr = (self.bufptr + 1) & 63;
        }
        self.dsp_step();
        for i in 0..16 {
            let wb = self.sv(i, 11);
            let (efsdl, efpan) = (((wb >> 5) & 7) as usize, (wb & 0x1F) as usize);
            if efsdl != 0 {
                let enc = efpan << 8 | efsdl << 13;
                l += (self.dsp.efreg[i] as i64 * t.lpan[enc] as i64) >> SHIFT;
                r += (self.dsp.efreg[i] as i64 * t.rpan[enc] as i64) >> SHIFT;
            }
        }
        // EXTS (CD-DA) through slots 16 / 17's effect send, else their direct send
        for i in 0..2 {
            let wb = self.sv(16 + i, 11);
            let (efsdl, efpan) = (((wb >> 5) & 7) as usize, (wb & 0x1F) as usize);
            let enc = if efsdl != 0 { efpan << 8 | efsdl << 13 } else { (((wb >> 8) & 0x1F) as usize) << 8 | (((wb >> 13) & 7) as usize) << 13 };
            let x = self.dsp.exts[i] as i64;
            l += (x * t.lpan[enc] as i64) >> SHIFT;
            r += (x * t.rpan[enc] as i64) >> SHIFT;
        }
        let mvol = (self.c(0) & 0xF) as f32 / 15.0;
        let (l, r) = if self.c(0) & 0x100 != 0 {
            ((l.clamp(-131072, 131071) as f32) / 131072.0, (r.clamp(-131072, 131071) as f32) / 131072.0)
        } else {
            (((l >> 2).clamp(-32768, 32767) as f32) / 32768.0, ((r >> 2).clamp(-32768, 32767) as f32) / 32768.0)
        };
        self.out.push(l * mvol);
        self.out.push(r * mvol);
        self.peak = self.peak.max((l * mvol).abs()).max((r * mvol).abs());

        // the slot monitor (MSLC / CA / SGC / EG)
        let sl = &self.slots[self.latched_mslc];
        let sgc = (sl.eg_state & 3) as u16;
        let ca = ((sl.cur >> (SHIFT + 12)) & 0xF) as u16;
        let eg = ((0x1F - (sl.eg_vol >> (EG_SHIFT + 5))) & 0x1F) as u16;
        self.mslc_data = ca << 7 | sgc << 5 | eg;

        // timers
        for k in 0..3 {
            if self.tim_left[k] > 0 {
                self.tim_left[k] -= 1;
                if self.tim_left[k] == 0 {
                    self.tim_done[k] = true;
                    let reg = (0x18 + k * 2) / 2;
                    self.cregs[reg] = (self.cregs[reg] & 0xFF00) | 0xFF;
                    self.cregs[0x20 / 2] |= [SCI_TIMA, SCI_TIMB, SCI_TIMC][k];
                    self.check_irq();
                    if k == 0 { self.main_check(0x40); }
                }
            }
        }
    }

    /// The CD-DA sample for the EXTS inputs
    pub fn set_exts(&mut self, l: i16, r: i16) { self.dsp.exts = [l, r]; }

    // ---------------- DSP ----------------

    fn dsp_start(&mut self) {
        self.dsp.stopped = false;
        let mut i = 127i32;
        while i >= 0 {
            let p = &self.dsp.mpro[i as usize * 4..i as usize * 4 + 4];
            if p.iter().any(|&w| w != 0) { break; }
            i -= 1;
        }
        self.dsp.last_step = (i + 1) as usize;
    }

    fn dsp_step(&mut self) {
        if self.dsp.stopped { self.dsp.mixs = [0; 16]; return; }
        self.dsp.efreg = [0; 16];
        let mut acc: i32 = 0;
        let mut memval: i32 = 0;
        let mut frc_reg: i32 = 0;
        let mut y_reg: i32 = 0;
        let mut adrs_reg: u32 = 0;
        for step in 0..self.dsp.last_step {
            let ip = [self.dsp.mpro[step * 4], self.dsp.mpro[step * 4 + 1], self.dsp.mpro[step * 4 + 2], self.dsp.mpro[step * 4 + 3]];
            let tra = ((ip[0] >> 8) & 0x7F) as u32;
            let twt = (ip[0] >> 7) & 1 != 0;
            let twa = (ip[0] & 0x7F) as u32;
            let xsel = (ip[1] >> 15) & 1 != 0;
            let ysel = (ip[1] >> 13) & 3;
            let ira = ((ip[1] >> 6) & 0x3F) as usize;
            let iwt = (ip[1] >> 5) & 1 != 0;
            let iwa = (ip[1] & 0x1F) as usize;
            let table = (ip[2] >> 15) & 1 != 0;
            let mwt = (ip[2] >> 14) & 1 != 0;
            let mrd = (ip[2] >> 13) & 1 != 0;
            let ewt = (ip[2] >> 12) & 1 != 0;
            let ewa = ((ip[2] >> 8) & 0xF) as usize;
            let adrl = (ip[2] >> 7) & 1 != 0;
            let frcl = (ip[2] >> 6) & 1 != 0;
            let shift = (ip[2] >> 4) & 3;
            let yrl = (ip[2] >> 3) & 1 != 0;
            let negb = (ip[2] >> 2) & 1 != 0;
            let zero = (ip[2] >> 1) & 1 != 0;
            let bsel = ip[2] & 1 != 0;
            let nofl = (ip[3] >> 15) & 1 != 0;
            let coef = ((ip[3] >> 9) & 0x3F) as usize;
            let masa = ((ip[3] >> 2) & 0x1F) as usize;
            let adreb = (ip[3] >> 1) & 1 != 0;
            let nxadr = ip[3] & 1 != 0;
            let d = &mut self.dsp;
            let mut inputs = if ira <= 0x1F { d.mems[ira] }
                else if ira <= 0x2F { d.mixs[ira - 0x20] << 4 }
                else if ira <= 0x31 { (d.exts[ira - 0x30] as i32) << 8 }
                else { return; };
            inputs = sext(inputs, 24);
            if iwt {
                d.mems[iwa] = memval;
                if ira == iwa { inputs = memval; }
            }
            let tval = |d: &Dsp, a: u32| sext(d.temp[((a + d.dec) & 0x7F) as usize], 24);
            let b = if !zero {
                let v = if bsel { acc } else { tval(d, tra) };
                if negb { v.wrapping_neg() } else { v }
            } else { 0 };
            let x = if xsel { inputs } else { tval(d, tra) };
            let mut y = match ysel {
                0 => frc_reg,
                1 => (d.coef[coef] >> 3) as i32,
                2 => (y_reg >> 11) & 0x1FFF,
                _ => (y_reg >> 4) & 0x0FFF,
            };
            if yrl { y_reg = inputs; }
            let shifted = match shift {
                0 => acc.clamp(-0x80_0000, 0x7F_FFFF),
                1 => acc.wrapping_mul(2).clamp(-0x80_0000, 0x7F_FFFF),
                2 => sext(acc.wrapping_mul(2), 24),
                _ => sext(acc, 24),
            };
            y = sext(y, 13);
            acc = (((x as i64 * y as i64) >> 12) + b as i64) as i32;
            if twt { let i = ((twa + d.dec) & 0x7F) as usize; d.temp[i] = shifted; }
            if frcl { frc_reg = if shift == 3 { shifted & 0x0FFF } else { (shifted >> 11) & 0x1FFF }; }
            if mrd || mwt {
                let mut addr = d.madrs[masa] as u32;
                if !table { addr = addr.wrapping_add(d.dec); }
                if adreb { addr = addr.wrapping_add(adrs_reg & 0x0FFF); }
                if nxadr { addr = addr.wrapping_add(1); }
                if !table { addr &= d.rbl - 1; } else { addr &= 0xFFFF; }
                addr = addr.wrapping_add(d.rbp << 12);
                addr <<= 1;
                if mrd && step & 1 != 0 {
                    let w = self.ram_word(addr);
                    memval = if nofl { (w as i32) << 8 } else { unpack(w) };
                }
                if mwt && step & 1 != 0 {
                    let v = if nofl { (shifted >> 8) as u16 } else { pack(shifted) };
                    self.ram_set(addr, v);
                }
            }
            let d = &mut self.dsp;
            if adrl { adrs_reg = if shift == 3 { ((shifted >> 12) & 0xFFF) as u32 } else { (inputs >> 16) as u32 }; }
            if ewt { d.efreg[ewa] = d.efreg[ewa].wrapping_add((shifted >> 8) as i16); }
        }
        self.dsp.dec = self.dsp.dec.wrapping_sub(1);
        self.dsp.mixs = [0; 16];
    }
}

impl Default for Scsp {
    fn default() -> Self { Self::new() }
}

/// The 68000's view: sound RAM (0-0xFFFFF, mirrored) and the SCSP's registers (0x100000)
pub struct SoundBus<'a> {
    pub scsp: &'a mut Scsp,
}

impl crate::common::cpu::m68000::M68kBus for SoundBus<'_> {
    fn read_byte(&mut self, a: u32) -> u8 {
        if a < 0x10_0000 { return self.scsp.ram_byte(a); }
        if a < 0x10_1000 { let w = self.scsp.read_reg(a & 0xFFE); return if a & 1 == 0 { (w >> 8) as u8 } else { w as u8 }; }
        0
    }
    fn read_word(&mut self, a: u32) -> u16 {
        if a < 0x10_0000 { return self.scsp.ram_word(a); }
        if a < 0x10_1000 { return self.scsp.read_reg(a & 0xFFE); }
        0
    }
    fn write_byte(&mut self, a: u32, v: u8) {
        if a < 0x10_0000 { self.scsp.ram[a as usize & (RAM_SIZE - 1)] = v; return; }
        if a < 0x10_1000 {
            let (w, m) = if a & 1 == 0 { ((v as u16) << 8, 0xFF00) } else { (v as u16, 0x00FF) };
            self.scsp.write_reg(a & 0xFFE, w, m);
        }
    }
    fn write_word(&mut self, a: u32, v: u16) {
        if a < 0x10_0000 { self.scsp.ram_set(a, v); return; }
        if a < 0x10_1000 { self.scsp.write_reg(a & 0xFFE, v, 0xFFFF); }
    }
}
