//! 32X PWM sound: two 3-deep FIFOs of 12-bit pulse widths, played at SH-2 clock / (cycle-1).
//! Every `timer` periods it raises the SH-2s' PWM interrupt and (if asked) a DREQ1 pulse so
//! DMA channel 1 can refill the FIFO. Ported from ares (ISC): ares/md/m32x/pwm.cpp and the
//! PWM parts of io-internal.cpp / io-external.cpp.
//!
//! Output is box-filtered down to the Genesis's 44.1 kHz stereo stream.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

pub const OUT_RATE: f64 = 44_100.0;

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Fifo { q: VecDeque<u16>, last: u16 }

impl Fifo {
    pub fn empty(&self) -> bool { self.q.is_empty() }
    pub fn full(&self) -> bool { self.q.len() >= 3 }
    /// Empty: the last sample again.
    fn read(&mut self) -> u16 { if let Some(v) = self.q.pop_front() { self.last = v; } self.last }
    /// Full: the oldest sample is dropped.
    fn write(&mut self, v: u16) { if self.full() { self.read(); } self.q.push_back(v & 0xFFF); }
}

/// One-pole DC blocker (the samples are pulse widths, all positive).
#[derive(Clone, Copy, Default, Serialize, Deserialize)]
struct DcBlock { x: f32, y: f32 }
impl DcBlock {
    fn run(&mut self, x: f32) -> f32 { self.y = x - self.x + 0.995 * self.y; self.x = x; self.y }
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Pwm {
    pub lmode: u8,
    pub rmode: u8,
    pub mono: bool,
    pub dreq_irq: bool,
    pub timer: u8,
    pub cycle: u16,
    pub lfifo: Fifo,
    pub rfifo: Fifo,
    latch: [u16; 3],
    pub periods: u8,
    lsample: u16,
    rsample: u16,
    dc: [DcBlock; 2],
    /// scaled master-clock time owed (master * 3 - sh2 * 7), like the SH-2s'
    pub debt: i64,
    /// box filter to 44.1 kHz: position in this output sample (master clocks * 44100), sums
    pos: f64,
    sum: (f64, f64),
    #[serde(skip)]
    pub out: Vec<f32>,
}

/// What a PWM period asks of the SH-2s.
#[derive(Default, PartialEq, Debug)]
pub struct Tick { pub irq: bool, pub dreq: bool }

impl Pwm {
    /// SH-2 clocks per period (cycle - 1; 0 counts as 4095 like the 12-bit hardware counter).
    pub fn period(&self) -> u32 { let c = self.cycle.wrapping_sub(1) & 0xFFF; c.max(1) as u32 }

    /// The samples playing now (left, right).
    pub fn playing(&self) -> (u16, u16) { (self.lsample, self.rsample) }

    pub fn read(&self, r: u32) -> u16 {
        match r {
            0x0 => self.lmode as u16 | (self.rmode as u16) << 2 | (self.mono as u16) << 4 | (self.dreq_irq as u16) << 7 | (self.timer as u16) << 8,
            0x2 => self.cycle,
            0x4 => self.latch[0] | (self.lfifo.empty() as u16) << 14 | (self.lfifo.full() as u16) << 15,
            0x6 => self.latch[1] | (self.rfifo.empty() as u16) << 14 | (self.rfifo.full() as u16) << 15,
            _ => self.latch[2] | ((self.lfifo.empty() && self.rfifo.empty()) as u16) << 14
                | ((self.lfifo.full() || self.rfifo.full()) as u16) << 15,
        }
    }

    /// Registers 2-8 (cycle and the FIFOs; the control register differs per side).
    /// A FIFO takes the sample when its low byte is written.
    pub fn write(&mut self, r: u32, upper: bool, lower: bool, v: u16) {
        if r == 0x2 {
            if lower { self.cycle = self.cycle & 0xF00 | v & 0xFF; }
            if upper { self.cycle = self.cycle & 0x0FF | v & 0xF00; }
            return;
        }
        let i = match r { 0x4 => 0, 0x6 => 1, _ => 2 };
        if upper { self.latch[i] = self.latch[i] & 0xFF | v & 0xF00; }
        if lower {
            self.latch[i] = self.latch[i] & 0xF00 | v & 0xFF;
            let s = self.latch[i];
            if i != 1 { self.lfifo.write(s); }
            if i != 0 { self.rfifo.write(s); }
        }
    }

    /// One period: take a sample from each FIFO, count towards the timer interrupt, output.
    pub fn tick(&mut self, master_hz: f64) -> Tick {
        let clocks = self.period();
        let mut t = Tick::default();
        let valid = |m: u8| (m & 1) ^ (m >> 1) != 0;
        if self.cycle != 1 && (valid(self.lmode) || valid(self.rmode)) {
            self.lsample = self.lfifo.read();
            self.rsample = self.rfifo.read();
            self.periods = self.periods.wrapping_add(1);
            if self.periods == (self.timer.wrapping_sub(1) & 15) + 1 {
                self.periods = 0;
                t = Tick { irq: true, dreq: self.dreq_irq };
            }
        }
        let signed = |s: u16| ((s << 4) as i16 >> 4) as f32;
        let pick = |m: u8, own: u16, other: u16| match m { 1 => signed(own), 2 => signed(other), _ => 0.0 };
        let l = pick(self.lmode, self.lsample, self.rsample) / clocks as f32;
        let r = pick(self.rmode, self.rsample, self.lsample) / clocks as f32;
        let l = self.dc[0].run(l).clamp(-1.0, 1.0) as f64;
        let r = self.dc[1].run(r).clamp(-1.0, 1.0) as f64;
        // this value lasts `clocks` SH-2 clocks = clocks * 7/3 master clocks
        let mut d = clocks as f64 * 7.0 / 3.0 * OUT_RATE;
        while d > 0.0 {
            let room = master_hz - self.pos;
            let take = d.min(room);
            self.sum.0 += l * take;
            self.sum.1 += r * take;
            self.pos += take;
            d -= take;
            if self.pos >= master_hz {
                self.out.push((self.sum.0 / master_hz) as f32);
                self.out.push((self.sum.1 / master_hz) as f32);
                self.sum = (0.0, 0.0);
                self.pos = 0.0;
            }
        }
        if self.out.len() > 16384 { self.out.drain(..8192); } // nobody is listening
        t
    }
}
