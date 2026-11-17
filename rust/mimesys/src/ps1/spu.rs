//! The SPU (1F801C00-1F801FFF), written from Nocash PSX-SPX (the psx-spx.github.io
//! edition): 24 ADPCM voices over 512 KB of sound RAM, mixed at 44100 Hz (one sample
//! every 768 CPU cycles), with CD audio and reverb.
//!
//! - Voices (1C00 + 16 n): volume L / R (fixed, or a sweep), pitch (1000h = 44100 Hz),
//!   start address, ADSR, the envelope level (ENVX) and the repeat address.
//! - A sample is 16-byte blocks: shift / filter, flags (bit 0 end: jump to the repeat
//!   address, bit 1 repeat (else release at level 0), bit 2 loop start: the repeat
//!   address becomes this block), then 28 4-bit samples. Decoding: (nibble << 12) >> shift
//!   plus the filtered last two samples. Output: 4-point Gaussian interpolation on the
//!   pitch counter's bits 4-11, times the envelope.
//! - ADSR and the volume sweeps share one envelope step (rate shift / step, linear or
//!   "exponential", up or down).
//! - Flags: KON / KOFF, PMON (the previous voice's output bends the pitch), NON (the
//!   shared noise generator instead of samples), EON (to reverb too), ENDX (reached an
//!   end flag).
//! - Mixing: the voices x their volume, x the main volume; plus CD audio (CD volume
//!   1DB0) and the reverb's output (1D84). Reverb runs at 22050 Hz on a work area from
//!   ESA (1DA2) to the end of RAM: same-side and cross reflections, four comb taps and
//!   two all-pass filters (1DC0-1DFF).
//! - Capture: CD audio L / R and voices 1 / 3 are written to RAM 0000-0FFF.
//! - IRQ9: a voice reading the block at IRQA (1DA4), a capture write or a transfer
//!   there, when SPUCNT bits 15 and 6 are set; clearing bit 6 acknowledges it.
//! - Transfers: the address (1DA6, in 8-byte units), then halfwords to the FIFO (1DA8),
//!   or DMA channel 4, which writes / reads words the same way.
//! - Simplifications: register writes take effect at once; reverb input / output are
//!   averaged / held over its two 44.1 kHz samples instead of the 39-tap filter.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// CPU cycles per 44100 Hz sample
const CYCLES_PER_SAMPLE: u32 = 768;

const GAUSS: [i32; 512] = [
    -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
    0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 3, 3,
    3, 4, 4, 5, 5, 6, 7, 7, 8, 9, 9, 10, 11, 12, 13, 14,
    15, 16, 17, 18, 19, 21, 22, 24, 25, 27, 28, 30, 32, 33, 35, 37,
    39, 41, 44, 46, 48, 51, 53, 56, 58, 61, 64, 67, 70, 73, 77, 80,
    84, 87, 91, 95, 99, 103, 107, 111, 116, 120, 125, 130, 135, 140, 145, 150,
    156, 161, 167, 173, 179, 186, 192, 199, 205, 212, 219, 227, 234, 242, 250, 257,
    266, 274, 283, 291, 300, 309, 319, 328, 338, 348, 358, 369, 379, 390, 401, 412,
    424, 436, 448, 460, 473, 485, 498, 512, 525, 539, 553, 567, 582, 597, 612, 627,
    643, 659, 675, 692, 708, 726, 743, 761, 779, 797, 816, 835, 854, 874, 894, 914,
    935, 956, 977, 999, 1020, 1043, 1066, 1089, 1112, 1136, 1160, 1184, 1209, 1234, 1260, 1286,
    1312, 1339, 1366, 1394, 1422, 1450, 1479, 1508, 1537, 1567, 1598, 1628, 1660, 1691, 1723, 1756,
    1789, 1822, 1856, 1890, 1924, 1959, 1995, 2031, 2067, 2104, 2141, 2179, 2217, 2256, 2295, 2334,
    2374, 2415, 2456, 2497, 2539, 2582, 2624, 2668, 2712, 2756, 2801, 2846, 2892, 2938, 2985, 3032,
    3079, 3128, 3176, 3225, 3275, 3325, 3376, 3427, 3479, 3531, 3584, 3637, 3691, 3745, 3799, 3855,
    3910, 3967, 4023, 4081, 4138, 4197, 4255, 4315, 4374, 4435, 4495, 4557, 4619, 4681, 4744, 4807,
    4871, 4935, 5000, 5065, 5131, 5197, 5264, 5332, 5399, 5468, 5536, 5606, 5676, 5746, 5817, 5888,
    5959, 6032, 6104, 6177, 6251, 6325, 6400, 6475, 6550, 6626, 6702, 6779, 6856, 6934, 7012, 7091,
    7170, 7249, 7329, 7409, 7490, 7571, 7653, 7735, 7817, 7900, 7983, 8066, 8150, 8234, 8319, 8404,
    8489, 8575, 8661, 8748, 8834, 8922, 9009, 9097, 9185, 9273, 9362, 9451, 9541, 9630, 9720, 9811,
    9901, 9992, 10083, 10174, 10266, 10358, 10450, 10542, 10635, 10727, 10820, 10913, 11007, 11100, 11194, 11288,
    11382, 11476, 11571, 11665, 11760, 11855, 11950, 12045, 12140, 12236, 12331, 12427, 12522, 12618, 12714, 12809,
    12905, 13001, 13097, 13193, 13289, 13385, 13481, 13577, 13673, 13769, 13865, 13961, 14056, 14152, 14248, 14343,
    14439, 14534, 14630, 14725, 14820, 14915, 15010, 15104, 15199, 15293, 15387, 15481, 15575, 15669, 15762, 15855,
    15948, 16041, 16133, 16226, 16317, 16409, 16500, 16592, 16682, 16773, 16863, 16953, 17042, 17131, 17220, 17308,
    17396, 17484, 17571, 17658, 17744, 17830, 17916, 18001, 18086, 18170, 18254, 18337, 18420, 18502, 18584, 18665,
    18746, 18826, 18905, 18985, 19063, 19141, 19219, 19295, 19372, 19447, 19522, 19597, 19671, 19744, 19816, 19888,
    19959, 20030, 20100, 20169, 20238, 20306, 20373, 20439, 20505, 20570, 20634, 20698, 20760, 20822, 20884, 20944,
    21004, 21063, 21121, 21178, 21235, 21290, 21345, 21399, 21452, 21505, 21556, 21607, 21657, 21706, 21754, 21801,
    21848, 21893, 21938, 21982, 22025, 22066, 22107, 22148, 22187, 22225, 22262, 22299, 22334, 22369, 22402, 22435,
    22467, 22498, 22527, 22556, 22584, 22611, 22637, 22662, 22686, 22709, 22731, 22752, 22772, 22791, 22809, 22826,
    22842, 22857, 22872, 22885, 22897, 22908, 22918, 22927, 22935, 22942, 22948, 22953, 22957, 22960, 22962, 22963,
];

/// SPU-ADPCM filters: (old, older) weights / 64
const FILTERS: [(i32, i32); 5] = [(0, 0), (60, 0), (115, -52), (98, -55), (122, -60)];

fn clamp16(v: i32) -> i32 { v.clamp(-0x8000, 0x7FFF) }

/// One envelope step (ADSR phases and volume sweeps), from PSX-SPX's "Envelope Operation".
/// `rate` is shift << 2 | step (7 bits); returns the new level.
fn envelope(level: i32, counter: &mut u32, rate: u32, exp: bool, dec: bool, neg: bool) -> i32 {
    let shift = (rate >> 2) as i32;
    let step_val = (rate & 3) as i32;
    let mut step = 7 - step_val;
    if dec != neg { step = !step; }
    step <<= (11 - shift).max(0);
    let mut inc: u32 = 0x8000 >> (shift - 11).max(0);
    if exp && !dec && level > 0x6000 {
        if shift < 10 { step >>= 2; }
        else if shift >= 11 { inc >>= 2; }
        else { step >>= 1; inc >>= 1; }
    } else if exp && dec {
        step = (step * level) >> 15;
    }
    if rate != 0x7F { inc = inc.max(1); }
    *counter += inc;
    if *counter & 0x8000 == 0 { return level; }
    *counter = 0;
    let l = level + step;
    if !dec { l.clamp(-0x8000, 0x7FFF) } else if neg { l.clamp(-0x8000, 0) } else { l.max(0) }
}

/// A volume register: a fixed level, or a sweep
#[derive(Clone, Copy, Default, Serialize, Deserialize)]
struct Volume {
    reg: u16,
    level: i32,
    counter: u32,
}

impl Volume {
    fn set(&mut self, v: u16) {
        self.reg = v;
        if v & 0x8000 == 0 { self.level = ((v << 1) as i16) as i32; }
        self.counter = 0;
    }

    fn tick(&mut self) {
        let v = self.reg;
        if v & 0x8000 == 0 { return; }
        let exp = v & 0x4000 != 0;
        let dec = v & 0x2000 != 0;
        let neg = v & 0x1000 != 0;
        let rate = (v & 0x7F) as u32;
        self.level = envelope(self.level, &mut self.counter, rate, exp, dec, neg);
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
enum Phase {
    #[default]
    Off,
    Attack,
    Decay,
    Sustain,
    Release,
}

#[derive(Clone, Serialize, Deserialize)]
struct Voice {
    vol_l: Volume,
    vol_r: Volume,
    pitch: u16,
    start: u16,
    adsr: u32,
    /// the envelope (ENVX)
    level: i32,
    env_counter: u32,
    phase: Phase,
    /// byte addresses: the block being played and the repeat point
    cur: u32,
    repeat: u32,
    /// software wrote the repeat address: loop-start flags don't move it
    ignore_loop: bool,
    counter: u32,
    flags: u8,
    /// the previous block's last three samples, then this block's 28
    buf: Vec<i16>,
    s1: i32,
    s2: i32,
    /// the last output (after the envelope), for PMON and capture
    out: i32,
}

impl Default for Voice {
    fn default() -> Self {
        Self {
            vol_l: Volume::default(), vol_r: Volume::default(), pitch: 0, start: 0, adsr: 0, level: 0,
            env_counter: 0, phase: Phase::Off, cur: 0, repeat: 0, ignore_loop: false, counter: 0, flags: 0,
            buf: vec![0; 31], s1: 0, s2: 0, out: 0,
        }
    }
}

impl Voice {
    fn rate_and_mode(&self) -> Option<(u32, bool, bool, i32)> {
        // (rate, exponential, decreasing, target)
        let lo = self.adsr & 0xFFFF;
        let hi = self.adsr >> 16;
        match self.phase {
            Phase::Off => None,
            Phase::Attack => Some(((lo >> 8) & 0x7F, lo & 0x8000 != 0, false, 0x7FFF)),
            Phase::Decay => {
                let sustain = (((lo & 0xF) + 1) * 0x800).min(0x7FFF) as i32;
                Some((((lo >> 4) & 0xF) << 2, true, true, sustain))
            }
            Phase::Sustain => Some(((hi >> 6) & 0x7F, hi & 0x8000 != 0, hi & 0x4000 != 0, 0)),
            Phase::Release => Some(((hi & 0x1F) << 2, hi & 0x20 != 0, true, 0)),
        }
    }

    fn tick_envelope(&mut self) {
        let Some((rate, exp, dec, target)) = self.rate_and_mode() else { return };
        self.level = envelope(self.level, &mut self.env_counter, rate, exp, dec, false);
        match self.phase {
            Phase::Attack if self.level >= 0x7FFF => { self.phase = Phase::Decay; self.env_counter = 0; }
            Phase::Decay if self.level <= target => { self.phase = Phase::Sustain; self.env_counter = 0; }
            Phase::Release if self.level <= 0 => { self.level = 0; self.phase = Phase::Off; }
            _ => {}
        }
    }

    /// Gaussian interpolation of the current sample
    fn interpolate(&self) -> i32 {
        let s = (self.counter >> 12) as usize;
        let i = ((self.counter >> 4) & 0xFF) as usize;
        let b = &self.buf;
        let (oldest, older, old, new) = (b[s] as i32, b[s + 1] as i32, b[s + 2] as i32, b[s + 3] as i32);
        let mut out = (GAUSS[0xFF - i] * oldest) >> 15;
        out += (GAUSS[0x1FF - i] * older) >> 15;
        out += (GAUSS[0x100 + i] * old) >> 15;
        out += (GAUSS[i] * new) >> 15;
        out
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Spu {
    pub regs: Vec<u16>,
    pub ram: Vec<u8>,
    /// the transfer's current address (bytes)
    addr: u32,
    pub irq: bool,
    voices: Vec<Voice>,
    main_l: Volume,
    main_r: Volume,
    endx: u32,
    /// SPUSTAT bit 6
    irq_flag: bool,
    noise_timer: i32,
    noise_level: u16,
    /// reverb: the buffer address (bytes), and the 22050 Hz half we're in
    rev_cur: u32,
    rev_odd: bool,
    rev_in: (i32, i32),
    rev_out: (i32, i32),
    capture: u32,
    cycles: u32,
    /// 44100 Hz stereo, -1..1
    #[serde(skip)]
    pub out: Vec<f32>,
}

impl Default for Spu {
    fn default() -> Self { Self::new() }
}

impl Spu {
    pub fn new() -> Self {
        Self {
            regs: vec![0; 0x200], ram: vec![0; 0x8_0000], addr: 0, irq: false,
            voices: vec![Voice::default(); 24], main_l: Volume::default(), main_r: Volume::default(),
            endx: 0, irq_flag: false, noise_timer: 0, noise_level: 1, rev_cur: 0, rev_odd: false,
            rev_in: (0, 0), rev_out: (0, 0), capture: 0, cycles: 0, out: Vec::new(),
        }
    }

    fn reg(&self, o: u32) -> u16 { self.regs[(o >> 1) as usize] }
    fn attr(&self) -> u16 { self.reg(0x1AA) }
    fn bits32(&self, o: u32) -> u32 { self.reg(o) as u32 | (self.reg(o + 2) as u32) << 16 }

    pub fn read16(&mut self, a: u32) -> u16 {
        let o = a & 0x3FE;
        match o {
            0x000..=0x17F => {
                let v = &self.voices[(o >> 4) as usize];
                match o & 0xF {
                    0xC => v.level as u16,
                    0xE => (v.repeat >> 3) as u16,
                    _ => self.reg(o),
                }
            }
            0x19C => self.endx as u16,
            0x19E => (self.endx >> 16) as u16,
            0x1AE => {
                let attr = self.attr();
                let dma = match (attr >> 4) & 3 { 2 => 0x100, 3 => 0x200, _ => 0 };
                (attr & 0x3F) | (self.irq_flag as u16) << 6 | (attr & 0x20) << 2 | dma
                    | ((self.capture >= 0x100) as u16) << 11
            }
            0x1B8 => self.main_l.level as u16,
            0x1BA => self.main_r.level as u16,
            0x200..=0x25F => {
                let v = &self.voices[((o - 0x200) >> 2) as usize];
                if o & 2 == 0 { v.vol_l.level as u16 } else { v.vol_r.level as u16 }
            }
            _ => self.reg(o),
        }
    }

    pub fn write16(&mut self, a: u32, v: u16) {
        let o = a & 0x3FE;
        self.regs[(o >> 1) as usize] = v;
        match o {
            0x000..=0x17F => {
                let voice = &mut self.voices[(o >> 4) as usize];
                match o & 0xF {
                    0x0 => voice.vol_l.set(v),
                    0x2 => voice.vol_r.set(v),
                    0x4 => voice.pitch = v,
                    0x6 => voice.start = v,
                    0x8 => voice.adsr = (voice.adsr & 0xFFFF_0000) | v as u32,
                    0xA => voice.adsr = (voice.adsr & 0xFFFF) | (v as u32) << 16,
                    0xC => voice.level = v as i16 as i32,
                    _ => { voice.repeat = (v as u32) << 3; voice.ignore_loop = true; }
                }
            }
            0x180 => self.main_l.set(v),
            0x182 => self.main_r.set(v),
            0x188 => self.key_on(v as u32),
            0x18A => self.key_on((v as u32) << 16),
            0x18C => self.key_off(v as u32),
            0x18E => self.key_off((v as u32) << 16),
            0x1A2 => self.rev_cur = (v as u32) << 3,
            0x1A6 => self.addr = (v as u32) << 3,
            0x1A8 => self.push(v),
            0x1AA => if v & 0x40 == 0 { self.irq_flag = false; },
            _ => {}
        }
    }

    fn key_on(&mut self, bits: u32) {
        for n in 0..24 {
            if bits & (1 << n) == 0 { continue; }
            self.endx &= !(1 << n);
            let v = &mut self.voices[n];
            v.cur = (v.start as u32) << 3;
            v.ignore_loop = false;
            v.counter = 0;
            v.buf.fill(0);
            v.s1 = 0;
            v.s2 = 0;
            v.level = 0;
            v.env_counter = 0;
            v.phase = Phase::Attack;
            self.decode_block(n);
        }
    }

    fn key_off(&mut self, bits: u32) {
        for n in 0..24 {
            if bits & (1 << n) != 0 {
                let v = &mut self.voices[n];
                if v.phase != Phase::Off { v.phase = Phase::Release; v.env_counter = 0; }
            }
        }
    }

    // ------------------------------------------------------------------ RAM

    fn check_irq(&mut self, a: u32, len: u32) {
        if self.attr() & 0x8040 != 0x8040 || self.irq_flag { return; }
        let at = (self.reg(0x1A4) as u32) << 3;
        if at >= a && at < a + len {
            self.irq_flag = true;
            self.irq = true;
        }
    }

    fn push(&mut self, v: u16) {
        let a = self.addr & 0x7FFFE;
        self.check_irq(a, 2);
        self.ram[a as usize..a as usize + 2].copy_from_slice(&v.to_le_bytes());
        self.addr = (a + 2) & 0x7FFFF;
    }

    pub fn dma_write(&mut self, w: u32) {
        self.push(w as u16);
        self.push((w >> 16) as u16);
    }

    pub fn dma_read(&mut self) -> u32 {
        let a = self.addr & 0x7FFFC;
        self.check_irq(a, 4);
        self.addr = (a + 4) & 0x7FFFF;
        u32::from_le_bytes(self.ram[a as usize..a as usize + 4].try_into().unwrap())
    }

    fn ram16(&self, a: u32) -> i16 {
        let a = (a & 0x7FFFE) as usize;
        i16::from_le_bytes([self.ram[a], self.ram[a + 1]])
    }

    fn set_ram16(&mut self, a: u32, v: i32) {
        let a = (a & 0x7FFFE) as usize;
        self.ram[a..a + 2].copy_from_slice(&(clamp16(v) as i16).to_le_bytes());
    }

    // ------------------------------------------------------------------ voices

    /// Decode voice n's block at its current address
    fn decode_block(&mut self, n: usize) {
        let a = self.voices[n].cur & 0x7FFF0;
        self.check_irq(a, 16);
        let blk: [u8; 16] = self.ram[a as usize..a as usize + 16].try_into().unwrap();
        let v = &mut self.voices[n];
        let mut shift = blk[0] & 0xF;
        if shift > 12 { shift = 9; }
        let (f0, f1) = FILTERS[((blk[0] >> 4) & 7).min(4) as usize];
        v.flags = blk[1];
        if blk[1] & 4 != 0 && !v.ignore_loop { v.repeat = a; }
        let tail = [v.buf[28], v.buf[29], v.buf[30]];
        v.buf[..3].copy_from_slice(&tail);
        for i in 0..28 {
            let nib = (blk[2 + i / 2] >> ((i & 1) * 4)) & 0xF;
            let t = (((nib as i32) << 28) >> 28) << 12;
            let s = clamp16((t >> shift) + ((v.s1 * f0 + v.s2 * f1 + 32) >> 6));
            v.s2 = v.s1;
            v.s1 = s;
            v.buf[3 + i] = s as i16;
        }
    }

    /// The end of voice n's block: flags, then the next block
    fn next_block(&mut self, n: usize) {
        let v = &mut self.voices[n];
        if v.flags & 1 != 0 {
            self.endx |= 1 << n;
            v.cur = v.repeat;
            if v.flags & 2 == 0 {
                v.phase = Phase::Release;
                v.level = 0;
            }
        } else {
            v.cur = (v.cur + 16) & 0x7FFF0;
        }
        self.decode_block(n);
    }

    fn voice_sample(&mut self, n: usize, noise: bool, prev_out: i32, pmon: bool) -> i32 {
        let v = &self.voices[n];
        let raw = if noise { self.noise_level as i16 as i32 } else { v.interpolate() };
        let out = (raw * v.level) >> 15;
        // the pitch counter
        let mut step = v.pitch as u32;
        if pmon {
            let factor = (prev_out + 0x8000) as u32;
            step = (((step as i16 as i32) * factor as i32) >> 15) as u32 & 0xFFFF;
        }
        if step > 0x3FFF { step = 0x4000; }
        self.voices[n].counter += step;
        while self.voices[n].counter >= 28 << 12 {
            self.voices[n].counter -= 28 << 12;
            self.next_block(n);
        }
        let v = &mut self.voices[n];
        v.tick_envelope();
        v.vol_l.tick();
        v.vol_r.tick();
        v.out = out;
        out
    }

    fn tick_noise(&mut self) {
        let attr = self.attr();
        let shift = ((attr >> 10) & 0xF) as u32;
        let step = ((attr >> 8) & 3) as i32 + 4;
        self.noise_timer -= step;
        let l = self.noise_level;
        let parity = ((l >> 15) ^ (l >> 12) ^ (l >> 11) ^ (l >> 10) ^ 1) & 1;
        if self.noise_timer < 0 {
            self.noise_level = l.wrapping_mul(2) | parity;
            self.noise_timer += (0x20000 >> shift) as i32;
            if self.noise_timer < 0 { self.noise_timer += (0x20000 >> shift) as i32; }
        }
    }

    // ------------------------------------------------------------------ reverb

    fn rev_addr(&self, off: i32) -> u32 {
        let base = (self.reg(0x1A2) as u32) << 3;
        let size = (0x80000 - base).max(2) as i32;
        let rel = (self.rev_cur as i32 - base as i32 + off).rem_euclid(size);
        base + rel as u32
    }

    fn rr(&self, reg: u32, extra: i32) -> i32 {
        let off = (self.reg(reg) as i32) * 8 + extra;
        self.ram16(self.rev_addr(off)) as i32
    }

    fn rw(&mut self, reg: u32, v: i32) {
        let a = self.rev_addr((self.reg(reg) as i32) * 8);
        self.set_ram16(a, v);
    }

    fn vol(&self, reg: u32) -> i32 { self.reg(reg) as i16 as i32 }

    /// One 22050 Hz reverb step: input (L, R) to output (L, R) before EVOL
    fn reverb(&mut self, lin: i32, rin: i32) -> (i32, i32) {
        let m = |a: i32, b: i32| (a * b) >> 15;
        let lin = m(lin, self.vol(0x1FC));
        let rin = m(rin, self.vol(0x1FE));
        let (iir, wall) = (self.vol(0x1C4), self.vol(0x1CE));
        if self.attr() & 0x80 != 0 {
            let same = |s: &Self, input: i32, d: u32, mm: u32| {
                let prev = s.rr(mm, -2);
                clamp16(m(input + m(s.rr(d, 0), wall) - prev, iir) + prev)
            };
            let ls = same(self, lin, 0x1E0, 0x1D4);
            let rs = same(self, rin, 0x1E2, 0x1D6);
            let ld = same(self, lin, 0x1F2, 0x1E4);
            let rd = same(self, rin, 0x1F0, 0x1E6);
            self.rw(0x1D4, ls);
            self.rw(0x1D6, rs);
            self.rw(0x1E4, ld);
            self.rw(0x1E6, rd);
        }
        let comb = |s: &Self, a: [u32; 4]| {
            m(s.vol(0x1C6), s.rr(a[0], 0)) + m(s.vol(0x1C8), s.rr(a[1], 0))
                + m(s.vol(0x1CA), s.rr(a[2], 0)) + m(s.vol(0x1CC), s.rr(a[3], 0))
        };
        let mut l = comb(self, [0x1D8, 0x1DC, 0x1E8, 0x1EC]);
        let mut r = comb(self, [0x1DA, 0x1DE, 0x1EA, 0x1EE]);
        let enabled = self.attr() & 0x80 != 0;
        for (vreg, dreg, ml, mr) in [(0x1D0u32, 0x1C0u32, 0x1F4u32, 0x1F6u32), (0x1D2, 0x1C2, 0x1F8, 0x1FA)] {
            let v = self.vol(vreg);
            let d = -(self.reg(dreg) as i32) * 8;
            let tl = self.rr(ml, d);
            let tr = self.rr(mr, d);
            l = clamp16(l - m(v, tl));
            r = clamp16(r - m(v, tr));
            if enabled { self.rw(ml, l); self.rw(mr, r); }
            l = clamp16(m(l, v) + tl);
            r = clamp16(m(r, v) + tr);
        }
        let base = (self.reg(0x1A2) as u32) << 3;
        self.rev_cur += 2;
        if self.rev_cur >= 0x80000 || self.rev_cur < base { self.rev_cur = base; }
        (l, r)
    }

    // ------------------------------------------------------------------ mixing

    /// Time passes; `cd` holds CD audio (CD-DA / XA) at 44100 Hz
    pub fn tick(&mut self, cycles: u32, cd: &mut VecDeque<(i16, i16)>) {
        self.cycles += cycles;
        while self.cycles >= CYCLES_PER_SAMPLE {
            self.cycles -= CYCLES_PER_SAMPLE;
            let c = cd.pop_front().unwrap_or((0, 0));
            self.sample(c);
        }
    }

    fn sample(&mut self, cd: (i16, i16)) {
        self.tick_noise();
        let non = self.bits32(0x194);
        let pmon = self.bits32(0x190);
        let eon = self.bits32(0x198);
        let (mut l, mut r, mut el, mut er) = (0i32, 0i32, 0i32, 0i32);
        let mut prev = 0;
        for n in 0..24 {
            let out = self.voice_sample(n, non & (1 << n) != 0, prev, n > 0 && pmon & (1 << n) != 0);
            prev = out;
            let v = &self.voices[n];
            let vl = (out * v.vol_l.level) >> 15;
            let vr = (out * v.vol_r.level) >> 15;
            l += vl;
            r += vr;
            if eon & (1 << n) != 0 { el += vl; er += vr; }
        }
        let attr = self.attr();
        if attr & 0xC000 != 0xC000 { l = 0; r = 0; el = 0; er = 0; }
        // CD audio, with its volume
        let cl = (cd.0 as i32 * self.vol(0x1B0)) >> 15;
        let cr = (cd.1 as i32 * self.vol(0x1B2)) >> 15;
        if attr & 1 != 0 {
            if attr & 4 != 0 { el += cl; er += cr; }
        }
        // capture buffers: CD audio (before volume), voices 1 and 3
        let p = self.capture * 2;
        self.capture_write(p, cd.0 as i32);
        self.capture_write(0x400 + p, cd.1 as i32);
        let (v1, v3) = (self.voices[1].out, self.voices[3].out);
        self.capture_write(0x800 + p, v1);
        self.capture_write(0xC00 + p, v3);
        self.capture = (self.capture + 1) & 0x1FF;
        // reverb at 22050 Hz
        self.rev_in.0 += el;
        self.rev_in.1 += er;
        if self.rev_odd {
            let (il, ir) = (clamp16(self.rev_in.0 / 2), clamp16(self.rev_in.1 / 2));
            self.rev_out = self.reverb(il, ir);
            self.rev_in = (0, 0);
        }
        self.rev_odd = !self.rev_odd;
        self.main_l.tick();
        self.main_r.tick();
        let mut ol = (clamp16(l) * self.main_l.level) >> 15;
        let mut or = (clamp16(r) * self.main_r.level) >> 15;
        ol += (self.rev_out.0 * self.vol(0x184)) >> 15;
        or += (self.rev_out.1 * self.vol(0x186)) >> 15;
        if attr & 1 != 0 { ol += cl; or += cr; }
        if self.out.len() < 2 * 44_100 {
            self.out.push(clamp16(ol) as f32 / 32768.0);
            self.out.push(clamp16(or) as f32 / 32768.0);
        }
    }

    fn capture_write(&mut self, a: u32, v: i32) {
        if self.reg(0x1AC) & 0xC != 0 { self.check_irq(a, 2); }
        self.set_ram16(a, v);
    }
}
