//! Harris HC-55516 CVSD (continuously variable slope delta) speech decoder, clocked by
//! the host (no oscillator). After MAME hc55516.cpp (Aaron Giles, Jonathan Gevaryahu;
//! BSD-3-Clause).
//!
//! The host sets the data bit (`digit_w`) and toggles the clock (`clock_w`). On a rising
//! edge the bit is shifted in, the syllabic filter adapts the step size (it grows while
//! the last three bits agree), and the integrator leaks toward zero; on a falling edge
//! the integrator moves one step up or down. The output is the integrator, scaled to
//! 16 bits, held until the next edge.

use serde::{Deserialize, Serialize};

const SYLMASK: i32 = 0xFC0;
const SYLSHIFT: i32 = 6;
const SYLADD: i32 = 0xFC1;
const INTSHIFT: i32 = 4;
const SHIFTREG_MASK: u8 = 0x7;

#[inline]
fn sext10(v: i32) -> i32 { (v << 22) >> 22 }

#[derive(Clone, Serialize, Deserialize)]
pub struct Hc55516 {
    last_clock: bool,
    buffered_bit: bool,
    shiftreg: u8,
    sylfilter: i32,
    intfilter: i32,
    next_sample: i32,
}

impl Default for Hc55516 {
    fn default() -> Self { Self::new() }
}

impl Hc55516 {
    pub fn new() -> Self {
        Self { last_clock: false, buffered_bit: false, shiftreg: 0, sylfilter: 0x3F, intfilter: 0, next_sample: 0 }
    }

    pub fn digit_w(&mut self, bit: bool) { self.buffered_bit = bit; }

    pub fn clock_w(&mut self, state: bool) {
        if state != self.last_clock {
            self.process_bit(self.buffered_bit, state);
        }
        self.last_clock = state;
    }

    /// Current output, -32768..32767.
    pub fn sample(&self) -> i32 { self.next_sample }

    fn process_bit(&mut self, bit: bool, clock: bool) {
        let frozen = (self.intfilter >= 0x180 && !bit) || (self.intfilter <= -0x180 && bit);
        let sum;
        if clock {
            // active (rising) edge
            self.shiftreg = (self.shiftreg << 1) | bit as u8;
            let run = self.shiftreg & SHIFTREG_MASK;
            if !frozen {
                self.sylfilter += ((!self.sylfilter) & SYLMASK) >> SYLSHIFT;
                if run != 0 && run != SHIFTREG_MASK { self.sylfilter += SYLADD; }
            }
            self.sylfilter &= 0xFFF;
            sum = sext10(((!self.intfilter) >> INTSHIFT) + 1);
        } else {
            let step = 2.max(self.sylfilter >> 6);
            sum = if self.shiftreg & 1 != 0 { sext10((!step) + 1) } else { sext10(step) };
        }
        if !frozen {
            self.intfilter = sext10(self.intfilter + sum);
        }
        self.next_sample = (self.intfilter << 6) | (((self.intfilter & 0x3FF) ^ 0x200) >> 4);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock_bits(c: &mut Hc55516, bits: &[bool]) -> Vec<i32> {
        bits.iter().map(|&b| {
            c.digit_w(b);
            c.clock_w(true);
            c.clock_w(false);
            c.sample()
        }).collect()
    }

    #[test]
    fn ones_and_zeros_slope_opposite_ways() {
        // (a 1 steps the integrator down, a 0 steps it up, as MAME's)
        let mut c = Hc55516::new();
        let ones = clock_bits(&mut c, &[true; 16]);
        assert!(*ones.last().unwrap() < -1000, "1s: {ones:?}");
        let zeros = clock_bits(&mut c, &[false; 40]);
        assert!(*zeros.last().unwrap() > 1000, "0s: {zeros:?}");
    }

    #[test]
    fn alternating_bits_stay_near_zero() {
        let mut c = Hc55516::new();
        let bits: Vec<bool> = (0..200).map(|i| i % 2 == 0).collect();
        let s = clock_bits(&mut c, &bits);
        assert!(s[100..].iter().all(|v| v.abs() < 4000), "idle pattern is quiet");
    }
}
