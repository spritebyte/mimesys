//! The DS sound unit (ARM7 0x04000400-0x0400051F): 16 channels, a mixer, 2 capture units.
//!
//! Channel x at 0x400 + x * 16:
//!   CNT (+0): 0-6 volume, 8-9 divider (/1, /2, /4, /16), 15 hold, 16-22 pan (0 left,
//!             64 centre, 127 right), 24-26 PSG duty, 27-28 repeat (0 manual, 1 loop,
//!             2 one-shot), 29-30 format (PCM8, PCM16, IMA-ADPCM, PSG / noise), 31 busy
//!   SAD (+4) source, TMR (+8) timer reload, PNT (+A) loop start in words, LEN (+C) words
//!     after the loop start.
//! A channel's timer counts up from TMR at 16.76 MHz (the bus clock / 2); each overflow
//! moves to the next sample. Channels 8-13 can also be square waves (duty in 1/8ths),
//! 14-15 noise (15-bit LFSR). Formats on other channels make no sound.
//!
//! The mixer runs every 1024 bus cycles (32.7 kHz). A channel contributes
//! sample << shift * volume, panned by pan / 128; SOUNDCNT (0x500) has the master volume,
//! the output sources (mixer, or channel 1 / 3 alone) and "keep channel 1 / 3 out of the
//! mixer" (for capture effects). Capture unit 0 / 1 (0x508 / 0x509, DAD 0x510 / 0x518,
//! LEN 0x514 / 0x51C) records the left / right mix, or channel 0 / 2, as PCM16 or PCM8 at
//! channel 1 / 3's rate, for one buffer or looping. The 32.7 kHz output is resampled to
//! 44.1 kHz stereo for the frontend.
//!
//! Scaling and channel behaviour follow GBATEK and melonDS's measurements; the code is
//! our own.

use serde::{Deserialize, Serialize};

pub const MIX_CYCLES: u64 = 1024;
const BUS_CLOCK: u64 = 33_513_982;
const OUT_RATE: u64 = 44_100;

const ADPCM_STEP: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88, 97,
    107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724, 796,
    876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871,
    5358, 5894, 6484, 7132, 7845, 8630, 9493, 10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623,
    27086, 29794, 32767,
];
const ADPCM_INDEX: [i32; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];

/// Memory as the sound unit sees it (the ARM7's map).
pub trait SoundMem {
    fn read8(&self, a: u32) -> u8;
    fn write8(&mut self, a: u32, v: u8);
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Channel {
    pub cnt: u32,
    pub sad: u32,
    pub tmr: u16,
    pub pnt: u16,
    pub len: u32,
    timer: u32,
    /// samples (PCM), nibbles after the header (ADPCM) or steps (PSG) played
    pos: u32,
    /// current output sample, 16-bit
    cur: i32,
    adpcm: (i32, i32),
    adpcm_loop: (i32, i32),
    noise: u16,
}

impl Channel {
    fn format(&self) -> u32 { (self.cnt >> 29) & 3 }

    /// Samples in the whole sound and where the loop starts.
    fn bounds(&self) -> (u32, u32) {
        let (pnt, len) = (self.pnt as u32, self.len);
        match self.format() {
            0 => ((pnt + len) * 4, pnt * 4),
            1 => ((pnt + len) * 2, pnt * 2),
            _ => (((pnt + len) * 8).saturating_sub(8), (pnt * 8).saturating_sub(8)),
        }
    }

    fn start(&mut self, mem: &dyn SoundMem) {
        self.timer = self.tmr as u32;
        self.pos = 0;
        self.noise = 0x7FFF;
        self.cur = 0;
        if self.format() == 2 {
            let h = u32::from_le_bytes([mem.read8(self.sad), mem.read8(self.sad + 1), mem.read8(self.sad + 2), mem.read8(self.sad + 3)]);
            self.adpcm = (h as u16 as i16 as i32, ((h >> 16) & 0x7F).min(88) as i32);
            self.adpcm_loop = self.adpcm;
            self.cur = self.adpcm.0;
        }
    }

    /// The next sample, after a timer overflow.
    fn step(&mut self, num: usize, mem: &dyn SoundMem) {
        match self.format() {
            3 => {
                if (8..14).contains(&num) {
                    let duty = (self.cnt >> 24) & 7;
                    let p = self.pos & 7;
                    self.cur = if duty != 7 && p > 6 - duty { 0x7FFF } else { -0x7FFF };
                    self.pos = self.pos.wrapping_add(1);
                } else if num >= 14 {
                    if self.noise & 1 != 0 { self.noise = (self.noise >> 1) ^ 0x6000; self.cur = -0x7FFF; }
                    else { self.noise >>= 1; self.cur = 0x7FFF; }
                }
                return;
            }
            _ => {}
        }
        let (total, loop_start) = self.bounds();
        if self.pos >= total {
            match (self.cnt >> 27) & 3 {
                1 => {
                    self.pos = loop_start;
                    if self.format() == 2 { self.adpcm = self.adpcm_loop; }
                }
                2 => {
                    self.cnt &= !0x8000_0000;
                    if self.cnt & 0x8000 == 0 { self.cur = 0; }
                    return;
                }
                _ => {}                                     // manual: keeps going
            }
        }
        let p = self.pos;
        match self.format() {
            0 => self.cur = (mem.read8(self.sad.wrapping_add(p)) as i8 as i32) << 8,
            1 => {
                let a = self.sad.wrapping_add(p * 2);
                self.cur = i16::from_le_bytes([mem.read8(a), mem.read8(a + 1)]) as i32;
            }
            _ => {
                if p == loop_start { self.adpcm_loop = self.adpcm; }
                let b = mem.read8(self.sad.wrapping_add(4 + p / 2));
                let n = if p & 1 != 0 { b >> 4 } else { b & 15 } as i32;
                let (mut val, mut idx) = self.adpcm;
                let step = ADPCM_STEP[idx as usize];
                let mut diff = step >> 3;
                if n & 1 != 0 { diff += step >> 2; }
                if n & 2 != 0 { diff += step >> 1; }
                if n & 4 != 0 { diff += step; }
                val = if n & 8 != 0 { (val - diff).max(-0x7FFF) } else { (val + diff).min(0x7FFF) };
                idx = (idx + ADPCM_INDEX[(n & 7) as usize]).clamp(0, 88);
                self.adpcm = (val, idx);
                self.cur = val;
            }
        }
        self.pos = p.wrapping_add(1);
    }

    /// Advance by one mixer tick (512 timer counts); returns the volume-scaled output.
    fn run(&mut self, num: usize, mem: &dyn SoundMem) -> i64 {
        if self.cnt & 0x8000_0000 == 0 { return 0; }
        if self.format() == 3 && num < 8 { return 0; }
        self.timer += 512;
        let reload = self.tmr as u32;
        while self.timer >= 0x10000 {
            self.timer = self.timer - 0x10000 + reload;
            self.step(num, mem);
            if self.cnt & 0x8000_0000 == 0 { break; }
        }
        const SHIFT: [u32; 4] = [4, 3, 2, 0];
        ((self.cur as i64) << SHIFT[((self.cnt >> 8) & 3) as usize]) * (self.cnt & 0x7F) as i64
    }

    fn pan(&self) -> i64 { ((self.cnt >> 16) & 0x7F) as i64 }
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Capture {
    pub cnt: u8,
    pub dad: u32,
    pub len: u16,
    timer: u32,
    pos: u32,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Sound {
    pub ch: [Channel; 16],
    pub soundcnt: u16,
    pub bias: u16,
    pub cap: [Capture; 2],
    /// bus cycle the unit has run up to
    pub time: u64,
    /// resampler: input samples made, the next output time (in bus cycles x 44100),
    /// the previous mixed sample
    made: u64,
    next_out: u64,
    prev: [f32; 2],
    #[serde(skip)]
    out: Vec<f32>,
}

impl Default for Sound { fn default() -> Self { Self::new() } }

impl Sound {
    pub fn new() -> Self {
        Self { ch: Default::default(), soundcnt: 0, bias: 0x200, cap: Default::default(), time: 0, made: 0,
               next_out: 0, prev: [0.0; 2], out: Vec::new() }
    }

    pub fn drain(&mut self) -> Vec<f32> { std::mem::take(&mut self.out) }

    pub fn read(&self, off: u32) -> u32 {
        match off {
            0x400..=0x4FF => {
                let c = &self.ch[((off - 0x400) >> 4) as usize];
                match off & 0xC {
                    0x0 => c.cnt,
                    _ => 0,                                          // SAD/TMR/PNT/LEN are write-only
                }
            }
            0x500 => self.soundcnt as u32,
            0x504 => self.bias as u32,
            0x508 => self.cap[0].cnt as u32 | (self.cap[1].cnt as u32) << 8,
            0x510 => self.cap[0].dad,
            0x518 => self.cap[1].dad,
            _ => 0,
        }
    }

    /// A 32-bit-aligned write with byte mask.
    pub fn write(&mut self, off: u32, v: u32, mask: u32, mem: &dyn SoundMem) {
        let merge = |old: u32| (old & !mask) | (v & mask);
        match off {
            0x400..=0x4FF => {
                let n = ((off - 0x400) >> 4) as usize;
                let c = &mut self.ch[n];
                match off & 0xC {
                    0x0 => {
                        let was = c.cnt & 0x8000_0000 != 0;
                        c.cnt = merge(c.cnt) & 0xFF7F_837F;
                        if !was && c.cnt & 0x8000_0000 != 0 { c.start(mem); }
                    }
                    0x4 => c.sad = merge(c.sad) & 0x07FF_FFFC,
                    0x8 => {
                        let w = merge(c.tmr as u32 | (c.pnt as u32) << 16);
                        c.tmr = w as u16;
                        c.pnt = (w >> 16) as u16;
                    }
                    _ => c.len = merge(c.len) & 0x3F_FFFF,
                }
            }
            0x500 => self.soundcnt = (merge(self.soundcnt as u32) & 0xBF7F) as u16,
            0x504 => self.bias = (merge(self.bias as u32) & 0x3FF) as u16,
            0x508 => {
                for k in 0..2 {
                    if mask >> (k * 8) & 0xFF == 0 { continue; }
                    let b = (v >> (k * 8)) as u8 & 0x8F;
                    let cap = &mut self.cap[k];
                    let starting = cap.cnt & 0x80 == 0 && b & 0x80 != 0;
                    cap.cnt = b;
                    if starting {
                        cap.pos = 0;
                        cap.timer = self.ch[1 + k * 2].tmr as u32;
                    }
                }
            }
            0x510 => self.cap[0].dad = merge(self.cap[0].dad) & 0x07FF_FFFC,
            0x514 => self.cap[0].len = merge(self.cap[0].len as u32) as u16,
            0x518 => self.cap[1].dad = merge(self.cap[1].dad) & 0x07FF_FFFC,
            0x51C => self.cap[1].len = merge(self.cap[1].len as u32) as u16,
            _ => {}
        }
    }

    /// Run the unit up to bus cycle `now`.
    pub fn run(&mut self, now: u64, mem: &mut dyn SoundMem) {
        while self.time + MIX_CYCLES <= now {
            self.time += MIX_CYCLES;
            let s = self.mix(mem);
            self.resample(s);
        }
    }

    /// One 32.7 kHz stereo sample (16-bit range).
    fn mix(&mut self, mem: &mut dyn SoundMem) -> [i32; 2] {
        if self.soundcnt & 0x8000 == 0 { return [0, 0]; }
        let mut out = [0i64; 16];
        for i in 0..16 { out[i] = self.ch[i].run(i, &*mem); }
        let (mut l, mut r) = (0i64, 0i64);
        for i in 0..16 {
            if (i == 1 && self.soundcnt & 0x1000 != 0) || (i == 3 && self.soundcnt & 0x2000 != 0) { continue; }
            let p = self.ch[i].pan();
            l += (out[i] * (128 - p)) >> 10;
            r += (out[i] * p) >> 10;
        }
        // capture: the mix (before the master volume) or channel 0 / 2
        for k in 0..2 {
            if self.cap[k].cnt & 0x80 == 0 { continue; }
            let src = if self.cap[k].cnt & 2 != 0 { (out[k * 2] * 64) >> 10 } else if k == 0 { l } else { r };
            let sample = (src >> 8).clamp(-0x8000, 0x7FFF) as i16;
            self.capture(k, sample, mem);
        }
        let side = |sel: u16, left: bool, mix: i64| -> i64 {
            let one = |c: usize| { let p = self.ch[c].pan(); (out[c] * if left { 128 - p } else { p }) >> 10 };
            match sel { 0 => mix, 1 => one(1), 2 => one(3), _ => one(1) + one(3) }
        };
        let l = side((self.soundcnt >> 8) & 3, true, l);
        let r = side((self.soundcnt >> 10) & 3, false, r);
        let master = (self.soundcnt & 0x7F) as i64;
        let fin = |x: i64| (((x * master) >> 7) >> 8).clamp(-0x8000, 0x7FFF) as i32;
        [fin(l), fin(r)]
    }

    fn capture(&mut self, k: usize, sample: i16, mem: &mut dyn SoundMem) {
        let reload = self.ch[1 + k * 2].tmr as u32;
        let cap = &mut self.cap[k];
        let bytes = (cap.len as u32).max(1) * 4;
        cap.timer += 512;
        while cap.timer >= 0x10000 {
            cap.timer = cap.timer - 0x10000 + reload;
            let a = cap.dad.wrapping_add(cap.pos);
            if cap.cnt & 8 != 0 {
                mem.write8(a, (sample >> 8) as u8);
                cap.pos += 1;
            } else {
                mem.write8(a, sample as u8);
                mem.write8(a + 1, (sample >> 8) as u8);
                cap.pos += 2;
            }
            if cap.pos >= bytes {
                cap.pos = 0;
                if cap.cnt & 4 != 0 { cap.cnt &= 0x7F; return; }
            }
        }
    }

    /// 32.7 kHz to 44.1 kHz, linear interpolation.
    fn resample(&mut self, s: [i32; 2]) {
        let cur = [s[0] as f32 / 32768.0, s[1] as f32 / 32768.0];
        // input sample k sits at bus time k * 1024; outputs every BUS_CLOCK / 44100 cycles
        let t_prev = self.made * MIX_CYCLES * OUT_RATE;
        self.made += 1;
        let t_cur = self.made * MIX_CYCLES * OUT_RATE;
        while self.next_out <= t_cur {
            let f = (self.next_out.saturating_sub(t_prev)) as f32 / (t_cur - t_prev) as f32;
            for c in 0..2 { self.out.push(self.prev[c] + (cur[c] - self.prev[c]) * f); }
            self.next_out += BUS_CLOCK;
        }
        self.prev = cur;
    }
}
