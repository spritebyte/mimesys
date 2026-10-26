//! Atari POKEY (C012294): 4 audio channels, polynomial noise, timers, pots.
//! Port of MAME's pokey.cpp (Juergen Buchmueller, Couriersud), clocked one
//! chip cycle at a time. Not ported: serial I/O and keyboard scanning
//! (no arcade board here uses them); SEROC reads as "complete".
//!
//! `step()` once per chip clock; `output()` is MAME's LEGACY_LINEAR level, 0.0-1.0.

use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

const NOTPOLY5: u8 = 0x80;
const POLY4: u8 = 0x40;
const PURE: u8 = 0x20;
const VOLUME_ONLY: u8 = 0x10;

const POLY9: u8 = 0x80;
const CH1_HICLK: u8 = 0x40;
const CH3_HICLK: u8 = 0x20;
const CH12_JOINED: u8 = 0x10;
const CH34_JOINED: u8 = 0x08;
const CH1_FILTER: u8 = 0x04;
const CH2_FILTER: u8 = 0x02;
const CLK_15KHZ: u8 = 0x01;

const IRQ_SEROC: u8 = 0x08;
const IRQ_TIMR4: u8 = 0x04;
const IRQ_TIMR2: u8 = 0x02;
const IRQ_TIMR1: u8 = 0x01;

const SK_RESET: u8 = 0x03;
const SK_PADDLE: u8 = 0x04;
const SK_TWOTONE: u8 = 0x08;
const SK_ASYNC: u8 = 0x10;
const SK_FRAME: u8 = 0x80;
const SK_KBERR: u8 = 0x40;
const SK_OVERRUN: u8 = 0x20;
const SK_BUSY: u8 = 0x02;
const SK_SERIN: u8 = 0x10;

const DIV_64: i32 = 28;
const DIV_15: i32 = 114;

struct Polys { p4: Vec<u8>, p5: Vec<u8>, p9: Vec<u32>, p17: Vec<u32> }

fn polys() -> &'static Polys {
    static P: OnceLock<Polys> = OnceLock::new();
    P.get_or_init(|| {
        let p45 = |size: u32| -> Vec<u8> {
            let mask = (1u32 << size) - 1;
            let mut lfsr = 0u32;
            (0..mask).map(|_| {
                lfsr = (lfsr << 1) | (!((lfsr >> 2) ^ (lfsr >> (size - 1))) & 1);
                (lfsr & mask) as u8
            }).collect()
        };
        let mut lfsr = 0x1FFu32;
        let p9 = (0..0x1FF).map(|_| {
            let inb = (lfsr & 1) ^ ((lfsr >> 5) & 1);
            lfsr = (lfsr >> 1) | (inb << 8);
            lfsr
        }).collect();
        let mut lfsr = 0x1FFFFu32;
        let p17 = (0..0x1FFFF).map(|_| {
            let in8 = ((lfsr >> 8) ^ (lfsr >> 13)) & 1;
            let inb = lfsr & 1;
            lfsr >>= 1;
            lfsr = (lfsr & 0xFF7F) | (in8 << 7);
            lfsr |= inb << 16;
            lfsr
        }).collect();
        Polys { p4: p45(4), p5: p45(5), p9, p17 }
    })
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Channel {
    int_mask: u8,
    audf: u8,
    audc: u8,
    borrow_cnt: i32,
    counter: i32,
    output: u8,
    filter_sample: u8,
}

impl Channel {
    fn reset_channel(&mut self) { self.counter = (self.audf ^ 0xFF) as i32; self.borrow_cnt = 0; }
    /// Returns the IRQ bit to raise, if any.
    fn inc(&mut self, cycles: i32, irqen: u8) -> u8 {
        self.counter = (self.counter + 1) & 0xFF;
        if self.counter == 0 && self.borrow_cnt == 0 {
            self.borrow_cnt = cycles;
            if irqen & self.int_mask != 0 { return self.int_mask; }
        }
        0
    }
    fn check_borrow(&mut self) -> bool {
        if self.borrow_cnt > 0 {
            self.borrow_cnt -= 1;
            return self.borrow_cnt == 0;
        }
        false
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Pokey {
    ch: [Channel; 4],
    out_raw: u32,
    clock_cnt: [i32; 3],
    p4: usize, p5: usize, p9: usize, p17: usize,
    pot: [u8; 8],
    /// Pot input values (what the pins would measure); 228 = nothing connected.
    pub pot_in: [u8; 8],
    /// When set, ALLPOT reads this (MAME allpot_r callback: DIP switches on arcade boards).
    pub allpot_in: Option<u8>,
    audctl: u8,
    allpot: u8,
    kbcode: u8,
    irqst: u8,
    irqen: u8,
    skstat: u8,
    skctl: u8,
    pot_counter: u8,
}

impl Default for Pokey { fn default() -> Self { Self::new() } }

impl Pokey {
    pub fn new() -> Self {
        let mut ch: [Channel; 4] = Default::default();
        ch[0].int_mask = IRQ_TIMR1;
        ch[1].int_mask = IRQ_TIMR2;
        ch[3].int_mask = IRQ_TIMR4;
        for c in ch.iter_mut() { c.audc = 0xB0; }
        polys();
        Self {
            ch, out_raw: 0, clock_cnt: [0; 3], p4: 0, p5: 0, p9: 0, p17: 0,
            pot: [0; 8], pot_in: [228; 8], allpot_in: None,
            audctl: 0, allpot: 0, kbcode: 0x09, irqst: IRQ_SEROC, irqen: 0,
            skstat: 0, skctl: 0, pot_counter: 0,
        }
    }

    /// IRQ output (active high here).
    pub fn irq(&self) -> bool { self.irqen & self.irqst != 0 }

    /// Output level 0.0-1.0 (sum of the 4 channel volumes, MAME's linear mode).
    pub fn output(&self) -> f32 {
        let mut sum = 0u32;
        for i in 0..4 { sum += (self.out_raw >> (4 * i)) & 0x0F; }
        let out = (sum as i32 * (32767 / 11 / 4)).min(0x7FFF);
        out as f32 / 32768.0
    }

    fn process_channel(&mut self, c: usize) {
        let p = polys();
        let audc = self.ch[c].audc;
        if audc & NOTPOLY5 != 0 || p.p5[self.p5] & 1 != 0 {
            self.ch[c].output = if audc & PURE != 0 { self.ch[c].output ^ 1 }
                else if audc & POLY4 != 0 { p.p4[self.p4] & 1 }
                else if self.audctl & POLY9 != 0 { (p.p9[self.p9] & 1) as u8 }
                else { (p.p17[self.p17] & 1) as u8 };
        }
    }

    fn step_pot(&mut self) {
        self.pot_counter = self.pot_counter.wrapping_add(1);
        for i in 0..8 {
            if self.pot[i] < self.pot_counter || self.pot_counter == 228 { self.allpot |= 1 << i; }
        }
    }

    fn potgo(&mut self) {
        self.allpot = 0;
        self.pot_counter = 0;
        for i in 0..8 {
            let r = self.pot_in[i].min(228);
            if r == 0 { self.allpot |= 1 << i; }
            self.pot[i] = r;
        }
    }

    /// One chip clock.
    pub fn step(&mut self) {
        let mut irq = 0u8;
        let irqen = self.irqen;
        if self.skctl & SK_RESET != 0 {
            self.p4 += 1; if self.p4 == 0x0F { self.p4 = 0; }
            self.p5 += 1; if self.p5 == 0x1F { self.p5 = 0; }
            self.p9 += 1; if self.p9 == 0x1FF { self.p9 = 0; }
            self.p17 += 1; if self.p17 == 0x1FFFF { self.p17 = 0; }

            let mut trig = [true, false, false];
            self.clock_cnt[1] += 1;
            if self.clock_cnt[1] >= DIV_64 { self.clock_cnt[1] = 0; trig[1] = true; }
            self.clock_cnt[2] += 1;
            if self.clock_cnt[2] >= DIV_15 { self.clock_cnt[2] = 0; trig[2] = true; }

            let a = self.audctl;
            if a & CH1_HICLK != 0 {
                irq |= self.ch[0].inc(if a & CH12_JOINED != 0 { 7 } else { 4 }, irqen);
            }
            let base = if a & CLK_15KHZ != 0 { 2 } else { 1 };
            if a & CH1_HICLK == 0 && trig[base] { irq |= self.ch[0].inc(1, irqen); }

            let async_reset = self.skctl & SK_ASYNC != 0 && self.skstat & (SK_BUSY | SK_SERIN) == 0;
            if a & CH3_HICLK != 0 && !async_reset {
                irq |= self.ch[2].inc(if a & CH34_JOINED != 0 { 7 } else { 4 }, irqen);
            }
            if a & CH3_HICLK == 0 && trig[base] && !async_reset { irq |= self.ch[2].inc(1, irqen); }

            if trig[base] {
                if a & CH12_JOINED == 0 { irq |= self.ch[1].inc(1, irqen); }
                if a & CH34_JOINED == 0 && !async_reset { irq |= self.ch[3].inc(1, irqen); }
            }

            if (trig[2] || self.skctl & SK_PADDLE != 0) && self.pot_counter < 228 { self.step_pot(); }
        }

        if self.ch[2].check_borrow() {
            if self.audctl & CH34_JOINED != 0 { irq |= self.ch[3].inc(1, irqen); }
            else { self.ch[2].reset_channel(); }
            self.process_channel(2);
            if self.audctl & CH1_FILTER != 0 { self.ch[0].filter_sample = self.ch[0].output; }
            else { self.ch[0].filter_sample = 1; }
        }
        if self.ch[3].check_borrow() {
            if self.audctl & CH34_JOINED != 0 { self.ch[2].reset_channel(); }
            self.ch[3].reset_channel();
            self.process_channel(3);
            if self.audctl & CH2_FILTER != 0 { self.ch[1].filter_sample = self.ch[1].output; }
            else { self.ch[1].filter_sample = 1; }
        }
        if self.skctl & SK_TWOTONE != 0 && self.ch[1].borrow_cnt == 1 {
            self.ch[0].reset_channel();
        }
        if self.ch[0].check_borrow() {
            if self.audctl & CH12_JOINED != 0 { irq |= self.ch[1].inc(1, irqen); }
            else {
                self.ch[0].reset_channel();
                // two-tone with the serial output idle (SEROC set, no BREAK) restarts channel 2
                if self.skctl & SK_TWOTONE != 0 && self.skctl & 0x80 == 0 { self.ch[1].reset_channel(); }
            }
            self.process_channel(0);
        }
        if self.ch[1].check_borrow() {
            if self.audctl & CH12_JOINED != 0 { self.ch[0].reset_channel(); }
            self.ch[1].reset_channel();
            self.process_channel(1);
        }

        let mut sum = 0u32;
        for (i, c) in self.ch.iter().enumerate() {
            if (c.output ^ c.filter_sample) != 0 || c.audc & VOLUME_ONLY != 0 {
                sum |= ((c.audc & 0x0F) as u32) << (i * 4);
            }
        }
        self.out_raw = sum;
        if irq != 0 { self.irqst |= irq & self.irqen; }
    }

    pub fn read(&mut self, offset: u8) -> u8 {
        let p = polys();
        match offset & 15 {
            o @ 0..=7 => {
                let i = o as usize;
                if self.allpot & (1 << i) != 0 { self.pot[i] } else { self.pot_counter }
            }
            0x08 => {
                if self.skctl & SK_RESET == 0 { self.allpot }
                else if let Some(v) = self.allpot_in { self.allpot = v; v }
                else { self.allpot ^ 0xFF }
            }
            0x09 => self.kbcode,
            0x0A => {
                if self.audctl & POLY9 != 0 { (p.p9[self.p9] & 0xFF) as u8 }
                else { ((p.p17[self.p17] >> 8) & 0xFF) as u8 }
            }
            0x0D => 0,                       // SERIN
            0x0E => self.irqst ^ 0xFF,
            0x0F => self.skstat ^ 0xFF,
            _ => 0xFF,
        }
    }

    pub fn write(&mut self, offset: u8, data: u8) {
        match offset & 15 {
            o @ 0..=7 => {
                let c = (o >> 1) as usize;
                if o & 1 == 0 { self.ch[c].audf = data; } else { self.ch[c].audc = data; }
            }
            0x08 => self.audctl = data,
            0x09 => { // STIMER
                for (i, c) in self.ch.iter_mut().enumerate() {
                    c.reset_channel();
                    c.output = 0;
                    c.filter_sample = if i < 2 { 1 } else { 0 };
                }
            }
            0x0A => self.skstat &= !(SK_FRAME | SK_OVERRUN | SK_KBERR),
            0x0B => if self.skctl & SK_RESET != 0 { self.potgo(); },
            0x0D => {}                       // SEROUT
            0x0E => {
                if self.irqst & !data != 0 { self.irqst &= IRQ_SEROC | data; }
                self.irqen = data;
            }
            _ => { // 0x0F SKCTL
                if data == self.skctl { return; }
                self.skctl = data;
                if data & SK_RESET == 0 {
                    self.write(0x0E, 0);
                    self.write(0x0A, 0);
                    self.p4 = 0; self.p5 = 0; self.p9 = 0; self.p17 = 0;
                    self.clock_cnt = [0; 3];
                    self.skstat &= !SK_BUSY;
                    self.irqst |= IRQ_SEROC;
                }
                if data & SK_ASYNC != 0 && self.skstat & SK_BUSY == 0 {
                    self.ch[2].reset_channel();
                    self.ch[3].reset_channel();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_tone_period() {
        // channel 1: 64 kHz base clock, AUDF = 9 -> toggles every 10 base ticks (28 clocks each)
        let mut p = Pokey::new();
        p.write(0x0F, 0x03);
        p.write(0x08, 0x00);
        p.write(0x00, 9);
        p.write(0x01, 0xAF);      // pure tone, volume 15
        p.write(0x09, 0);         // STIMER
        let mut edges = vec![];
        let mut last = p.output();
        for t in 0..28 * 10 * 6 {
            p.step();
            let o = p.output();
            if o != last { edges.push(t); last = o; }
        }
        let gaps: Vec<i32> = edges.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(gaps.len() >= 4 && gaps[1..].iter().all(|&g| g == 280), "{gaps:?}");
    }

    #[test]
    fn allpot_and_random() {
        let mut p = Pokey::new();
        p.allpot_in = Some(0x5A);
        assert_eq!(p.read(0x08), 0x00, "ALLPOT reads its latch while SKCTL holds reset");
        p.write(0x0F, 0x03);
        assert_eq!(p.read(0x08), 0x5A);
        let a = p.read(0x0A);
        for _ in 0..1000 { p.step(); }
        assert_ne!(a, p.read(0x0A));
    }
}
