//! The three root counters (1F801100-1F80112F), from Nocash PSX-SPX.
//!
//! - Counter 0: system clock or the GPU dot clock. Counter 1: system clock or hblanks.
//!   Counter 2: system clock or system clock / 8.
//! - Mode bits: 0 sync enable, 1-2 sync mode, 3 reset at target (else at FFFF), 4 / 5 IRQ
//!   at target / FFFF, 6 repeat, 7 toggle (else pulse), 8-9 clock source, 10 IRQ line
//!   (active low), 11 / 12 reached target / FFFF (cleared by reading the mode).
//! - Sync modes for counter 2 (0 and 3 stop it) are done; counters 0 and 1's blank syncs
//!   are approximated: mode 0 / 1 / 2 / 3 counted freely except "pause during blank"
//!   (0) and "count only during blank" (3), which follow the blank flags.

use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Timer {
    pub count: u32,
    pub mode: u32,
    pub target: u32,
    /// one IRQ per mode write unless repeating
    fired: bool,
    /// sub-unit remainder (dot clock / 8)
    frac: u32,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Timers {
    pub t: [Timer; 3],
    /// counter 0 runs on dots when its source says so: GPU cycles per dot
    pub dot_div: u32,
    pub in_hblank: bool,
    pub in_vblank: bool,
    /// IRQ bits (4 + n) waiting for the interrupt controller
    pub irq: u32,
}

impl Timers {
    pub fn new() -> Self { Self { dot_div: 10, ..Default::default() } }

    pub fn read(&mut self, a: u32) -> u32 {
        let n = ((a >> 4) & 3) as usize;
        if n > 2 { return 0; }
        let t = &mut self.t[n];
        match a & 0xF {
            0 => t.count & 0xFFFF,
            4 => {
                let v = t.mode;
                t.mode &= !(0x1800);
                v
            }
            8 => t.target,
            _ => 0,
        }
    }

    pub fn write(&mut self, a: u32, v: u32) {
        let n = ((a >> 4) & 3) as usize;
        if n > 2 { return; }
        let t = &mut self.t[n];
        match a & 0xF {
            0 => t.count = v & 0xFFFF,
            4 => {
                t.mode = (v & 0x3FF) | 0x400;
                t.count = 0;
                t.fired = false;
                t.frac = 0;
            }
            8 => t.target = v & 0xFFFF,
            _ => {}
        }
    }

    fn paused(&self, n: usize) -> bool {
        let m = self.t[n].mode;
        if m & 1 == 0 { return false; }
        match (n, (m >> 1) & 3) {
            (2, 0) | (2, 3) => true,
            (0, 0) => self.in_hblank,
            (1, 0) => self.in_vblank,
            (0, 3) => !self.in_hblank,
            (1, 3) => !self.in_vblank,
            _ => false,
        }
    }

    /// `cycles` of the system clock
    pub fn tick(&mut self, cycles: u32) {
        for n in 0..3 {
            let src = (self.t[n].mode >> 8) & 3;
            let units = match (n, src) {
                (0, 1) | (0, 3) => {
                    // GPU clock = CPU x 1.585 (about 111 / 70)
                    let t = &mut self.t[0];
                    t.frac += cycles * 111;
                    let per = 70 * self.dot_div.max(1);
                    let u = t.frac / per;
                    t.frac %= per;
                    u
                }
                (1, 1) | (1, 3) => 0,                       // hblanks come from `hblank`
                (2, 2) | (2, 3) => {
                    let t = &mut self.t[2];
                    t.frac += cycles;
                    let u = t.frac / 8;
                    t.frac %= 8;
                    u
                }
                _ => cycles,
            };
            if units > 0 && !self.paused(n) { self.advance(n, units); }
        }
    }

    /// One hblank: counter 1's clock when it's on that source.
    pub fn hblank(&mut self) {
        let src = (self.t[1].mode >> 8) & 3;
        if (src == 1 || src == 3) && !self.paused(1) { self.advance(1, 1); }
    }

    fn advance(&mut self, n: usize, units: u32) {
        let t = &mut self.t[n];
        let target = t.target;
        let old = t.count;
        let mut c = old + units;
        let mut hit = false;
        if old <= target && c >= target {
            t.mode |= 0x800;
            if t.mode & 0x10 != 0 { hit = true; }
            // reset at target: target, then 0 (a period of target + 1)
            if t.mode & 0x08 != 0 && c > target { c = (c - target - 1) % (target + 1); }
        }
        if c > 0xFFFF {
            t.mode |= 0x1000;
            if t.mode & 0x20 != 0 { hit = true; }
            c &= 0xFFFF;
        }
        t.count = c;
        if hit {
            let repeat = t.mode & 0x40 != 0;
            if repeat || !t.fired {
                t.fired = true;
                if t.mode & 0x80 != 0 {
                    // toggle: the line flips, an IRQ when it goes low
                    t.mode ^= 0x400;
                    if t.mode & 0x400 == 0 { self.irq |= 1 << (4 + n); }
                } else {
                    // pulse: low for a moment, then back up
                    self.irq |= 1 << (4 + n);
                    t.mode |= 0x400;
                }
            }
        }
    }
}
