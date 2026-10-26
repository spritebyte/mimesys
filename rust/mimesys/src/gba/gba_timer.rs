//! GBA timers 0-3 (0x04000100-0x0400010F): 16-bit up-counters that reload on
//! overflow, clocked by the system clock / 1, 64, 256 or 1024, or (count-up mode)
//! by the previous timer's overflows. Overflows can raise IRQs (IF bits 3-6) and
//! clock the Direct Sound FIFOs.
//!
//! Exact to the cycle, on absolute time (as NanoBoyAdvance): a control write takes
//! effect 1 cycle later; a newly enabled timer loads its reload value then and
//! counts from the next prescaler edge (the prescaler runs freely off the system
//! clock). The counter is `base + (now - start) >> shift`.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
pub struct Timer {
    pub reload: u16,
    pub control: u16,
    /// counter value at `start`
    base: u32,
    /// system clock where counting from `base` begins (may be in the future)
    start: u64,
    /// counting off the clock (enabled, not count-up)
    running: bool,
    /// count-up timers keep their value here
    count: u32,
    /// a reload write takes effect one cycle later: (value, from clock)
    pending_reload: Option<(u16, u64)>,
}

const SHIFT: [u32; 4] = [0, 6, 8, 10];

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Timers {
    pub ch: [Timer; 4],
}

impl Timer {
    fn shift(&self) -> u32 { SHIFT[(self.control & 3) as usize] }
    fn enabled(&self) -> bool { self.control & 0x80 != 0 }
    fn cascade(&self, i: usize) -> bool { i > 0 && self.control & 4 != 0 }

    /// Reload value in effect at clock `at`.
    fn reload_at(&self, at: u64) -> u16 {
        match self.pending_reload { Some((v, t)) if at >= t => v, _ => self.reload }
    }

    /// Counter value at `now`, wrapping through any overflows not yet handled.
    fn value(&self, now: u64) -> u16 {
        if !self.running { return self.count as u16; }
        let ticks = if now > self.start { (now - self.start) >> self.shift() } else { 0 };
        let mut v = self.base as u64 + ticks;
        if v >= 0x1_0000 {
            let r = self.reload_at(now) as u64;
            v = r + (v - 0x1_0000) % (0x1_0000 - r);
        }
        v as u16
    }
}

impl Timers {
    pub fn new() -> Self { Self::default() }

    /// Byte read of 0x100-0x10F at time `now`.
    pub fn read8(&self, off: u32, now: u64) -> u8 {
        let t = &self.ch[(off as usize >> 2) & 3];
        let v = if off & 2 == 0 { t.value(now) } else { t.control };
        (v >> ((off & 1) * 8)) as u8
    }

    /// Byte write of 0x100-0x10F at time `now` (end of the bus access).
    pub fn write8(&mut self, off: u32, v: u8, now: u64) {
        let i = (off as usize >> 2) & 3;
        let shift = (off & 1) * 8;
        let mask = !(0xFFu16 << shift);
        let v = (v as u16) << shift;
        if off & 2 == 0 {
            let t = &mut self.ch[i];
            let old = t.reload_at(now + 1);
            if let Some((p, at)) = t.pending_reload { if at <= now { t.reload = p; } }
            t.pending_reload = Some(((old & mask) | v, now + 1));
            return;
        }
        // all control bits are in the low byte; a write to the high byte changes nothing
        if off & 1 != 0 { return; }
        let apply = now + 1;
        let t = &mut self.ch[i];
        // freeze the current value
        let cur = t.value(apply) as u32;
        let was_on = t.enabled();
        t.control = ((t.control & mask) | v) & if i == 0 { 0x00C3 } else { 0x00C7 };
        t.running = false;
        t.count = cur;
        if !t.enabled() { return; }
        let pmask = (1u64 << t.shift()) - 1;
        let offset = apply & pmask;
        if t.cascade(i) {
            if !was_on { t.count = t.reload_at(apply) as u32; }
            return;
        }
        t.running = true;
        if was_on {
            t.start = apply - offset;
        } else if cur == 0xFFFF && offset == 0 {
            // the reload takes a cycle, and an old counter of FFFF ticks over during it
            // (NanoBoyAdvance issue #331)
            t.count = 0xFFFF;
            t.start = apply;
        } else {
            // a new start counts from the reload value, one cycle later
            t.count = t.reload_at(apply) as u32;
            t.start = apply + 1 - offset;
        }
        t.base = t.count;
    }

    /// Runs the timers up to `now`. Returns each timer's overflow count, and the
    /// time of the first overflow that requests an IRQ.
    pub fn advance_to(&mut self, now: u64) -> ([u32; 4], Option<u64>) {
        let mut over = [0u32; 4];
        let mut first_irq: Option<u64> = None;
        for i in 0..4 {
            let irq = self.ch[i].control & 0x40 != 0;
            let t = &mut self.ch[i];
            let commit = |t: &mut Timer| {
                if let Some((v, at)) = t.pending_reload { if now >= at { t.reload = v; t.pending_reload = None; } }
            };
            if !t.enabled() { continue; }
            if t.cascade(i) {
                let mut left = over[i - 1];
                while left > 0 {
                    let room = 0x1_0000 - t.count;
                    if left >= room { left -= room; t.count = t.reload_at(now) as u32; over[i] += 1; }
                    else { t.count += left; left = 0; }
                }
                if over[i] > 0 && irq { first_irq = first_irq.or(Some(now)); }
                commit(t);
                continue;
            }
            if !t.running { commit(t); continue; }
            let shift = t.shift();
            loop {
                // time at which the counter wraps
                let wrap_at = t.start + (((0x1_0000 - t.base) as u64) << shift);
                if wrap_at > now { break; }
                over[i] += 1;
                if irq && first_irq.is_none_or(|f| wrap_at < f) { first_irq = Some(wrap_at); }
                t.base = t.reload_at(wrap_at) as u32;
                t.start = wrap_at;
            }
            commit(t);
        }
        (over, first_irq)
    }

    /// Clocks from `now` until the next overflow of a clock-driven timer.
    pub fn cycles_to_overflow(&self, now: u64) -> Option<u64> {
        self.ch.iter().filter(|t| t.running).map(|t| {
            let wrap_at = t.start + (((0x1_0000 - t.base) as u64) << t.shift());
            wrap_at.saturating_sub(now)
        }).min()
    }

    /// IF bits for overflow counts.
    pub fn irq_bits(&self, over: &[u32; 4]) -> u16 {
        let mut bits = 0;
        for i in 0..4 {
            if over[i] > 0 && self.ch[i].control & 0x40 != 0 { bits |= 1 << (3 + i); }
        }
        bits
    }
}
