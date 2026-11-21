//! The SCU DSP: a 32-bit fixed-point coprocessor inside the SCU, used by games for 3D maths.
//!
//! Ported from MAME's `scudsp.cpp` (BSD-3-Clause, Angelo Salese & Mariusz Wojcieszek).
//!
//! - 256 words of program RAM, four 64-word data RAMs (MD0-3) with counters CT0-3.
//! - RX * RY -> MUL (48 bits); P and A are 48 bits; the ALU works on ACL / PL (32 bits) or
//!   A + P (AD2, 48 bits).
//! - One instruction word drives the ALU, the X bus, the Y bus and the D1 bus at once.
//! - DMA between the D0 bus (the rest of the machine) and the data / program RAM, done at once.
//! - JMP, BTM and LPS have a delay slot (the next instruction runs before the jump).
//! - ENDI stops and raises the SCU's "DSP end" interrupt.
//!
//! Ports at SCU 0x80 (PPAF: control and flags), 0x84 (PPD: program data), 0x88 (PDA: data
//! RAM address), 0x8C (PDD: data RAM data).

use serde::{Deserialize, Serialize};

/// The D0 bus: 16-bit accesses to the rest of the machine
pub trait DspBus {
    fn read16(&mut self, a: u32) -> u16;
    fn write16(&mut self, a: u32, v: u16);
}

const EX: u32 = 1 << 16;
const ES: u32 = 1 << 17;
const EF: u32 = 1 << 18;
const VF: u32 = 1 << 19;
const CF: u32 = 1 << 20;
const ZF: u32 = 1 << 21;
const SF: u32 = 1 << 22;
const T0F: u32 = 1 << 23;
const LE: u32 = 1 << 15;
/// the PPAF bits that read back (EX, ES, E, V, C, Z, S, T0, PR, EP)
const FLAGS_MASK: u32 = 0x06FF_0000;

const MASK48: i64 = 0xFFFF_FFFF_FFFF;

fn sx48(v: i64) -> i64 { (v << 16) >> 16 }

#[derive(Clone, Serialize, Deserialize)]
pub struct Dsp {
    prog: Vec<u32>,
    /// MD0-3, 64 words each
    md: Vec<u32>,
    ct: [u8; 4],
    rx: u32,
    ry: u32,
    /// P, A, ALU and MUL: 48-bit values, sign-extended
    p: i64,
    a: i64,
    alu: i64,
    mul: i64,
    top: u8,
    lop: u16,
    ra0: u32,
    wa0: u32,
    pc: u8,
    /// the delay slot to run next (after JMP / BTM / LPS)
    delay: Option<u8>,
    /// PPAF flags (EX, ES, E, V, C, Z, S, T0, PR, EP)
    flags: u32,
    /// PDA: the data RAM picked for the PDD port
    ra: u8,
    /// the upper halves of 32-bit port writes, waiting for their lower half
    hi: [u16; 4],
    /// clocks owed to the DSP (it runs at half the SH-2's clock)
    frac: u32,
    /// ENDI happened: the SCU raises its interrupt
    #[serde(skip)]
    pub irq: bool,
}

impl Default for Dsp {
    fn default() -> Self { Self::new() }
}

impl Dsp {
    pub fn new() -> Self {
        Self { prog: vec![0; 256], md: vec![0; 256], ..Self::new_regs() }
    }

    /// A placeholder with no RAM (to move the DSP out while it runs on the bus)
    pub fn empty() -> Self { Self { prog: Vec::new(), md: Vec::new(), ..Self::new_regs() } }

    fn new_regs() -> Self {
        Self { prog: Vec::new(), md: Vec::new(), ct: [0; 4], rx: 0, ry: 0, p: 0, a: 0, alu: 0, mul: 0,
               top: 0, lop: 0, ra0: 0, wa0: 0, pc: 0, delay: None, flags: 0, ra: 0, hi: [0; 4], frac: 0, irq: false }
    }

    pub fn running(&self) -> bool { self.flags & EX != 0 }

    // ---------------- ports ----------------

    /// A 16-bit read of the port at `off` (0x80-0x8C), `low` = the low half
    pub fn read(&mut self, off: u32, low: bool) -> u16 {
        let v = match off & 0xC {
            0x0 => (self.pc as u32) | (self.flags & FLAGS_MASK),
            0xC => {
                // PDD: MC(bank) - the counter steps once per 32-bit read (on its low half)
                let bank = (self.ra >> 6) as usize & 3;
                let v = self.md[bank * 64 + self.ct[bank] as usize];
                if low { self.ct[bank] = (self.ct[bank] + 1) & 63; }
                v
            }
            _ => 0,
        };
        if low { v as u16 } else { (v >> 16) as u16 }
    }

    /// A 16-bit write of the port at `off`, `low` = the low half. The ports act when their
    /// low half is written (an SH-2 32-bit write sends the upper half first).
    pub fn write(&mut self, off: u32, v: u16, low: bool) {
        let i = (off as usize >> 2) & 3;
        if !low {
            self.hi[i] = v;
            // PPAF: EX / ES / pause sit in the upper half: act at once
            if i == 0 {
                let nv = (v as u32) << 16;
                self.flags = (self.flags & !FLAGS_MASK) | (nv & FLAGS_MASK);
            }
            return;
        }
        let full = (self.hi[i] as u32) << 16 | v as u32;
        match i {
            0 => {
                self.flags = (self.flags & !FLAGS_MASK) | (full & FLAGS_MASK);
                if full & LE != 0 { self.pc = full as u8; self.delay = None; }
            }
            1 => { self.prog[self.pc as usize] = full; self.pc = self.pc.wrapping_add(1); }
            2 => { self.ra = full as u8; self.ct[(self.ra >> 6) as usize & 3] = self.ra & 63; }
            _ => {
                let bank = (self.ra >> 6) as usize & 3;
                self.md[bank * 64 + self.ct[bank] as usize] = full;
                self.ct[bank] = (self.ct[bank] + 1) & 63;
            }
        }
    }

    // ---------------- running ----------------

    /// `cycles` SH-2 clocks pass: run the program while EX is set
    pub fn run<B: DspBus>(&mut self, cycles: u32, bus: &mut B) {
        if !self.running() { self.frac = 0; return; }
        self.frac += cycles;
        while self.frac >= 2 && self.running() {
            self.frac -= 2;
            self.step(bus);
            if self.flags & ES != 0 { self.flags &= !(EX | ES); }
        }
    }

    pub fn step<B: DspBus>(&mut self, bus: &mut B) {
        let op = if let Some(d) = self.delay.take() {
            self.prog[d as usize]
        } else {
            let op = self.prog[self.pc as usize];
            self.pc = self.pc.wrapping_add(1);
            op
        };
        match op >> 28 {
            0x0..=0x3 => self.operation(op),
            0x8..=0xB => self.move_immediate(op),
            0xC => self.dma(op, bus),
            0xD => self.jump(op),
            0xE => self.lp(op),
            0xF => self.end(op),
            _ => {}
        }
        self.mul = sx48((self.rx as i32 as i64) * (self.ry as i32 as i64));
    }

    fn set_flag(&mut self, f: u32, on: bool) { if on { self.flags |= f } else { self.flags &= !f } }

    fn condition(&self, c: u32) -> bool {
        let z = self.flags & ZF != 0;
        let s = self.flags & SF != 0;
        let r = match c & 0xF {
            0x1 => z,
            0x2 => s,
            0x3 => z || s,
            0x4 => self.flags & CF != 0,
            0x8 => self.flags & T0F != 0,
            _ => false,
        };
        if c & 0x20 == 0 { !r } else { r }
    }

    /// Data RAM source 0-7 (M0-3, MC0-3); MCn marks CTn to step
    fn source(&mut self, s: u32, step: &mut [bool; 4]) -> u32 {
        let n = (s & 3) as usize;
        let v = self.md[n * 64 + self.ct[n] as usize];
        if s & 4 != 0 { step[n] = true; }
        v
    }

    /// D1 / MVI destinations
    fn dest(&mut self, d: u32, v: u32, step: &mut [bool; 4]) {
        match d {
            0..=3 => {
                let n = d as usize;
                self.md[n * 64 + self.ct[n] as usize] = v;
                step[n] = true;
            }
            4 => self.rx = v,
            5 => self.p = v as i32 as i64,
            6 => self.ra0 = v,
            7 => self.wa0 = v,
            0xA => self.lop = (v & 0xFFF) as u16,
            0xB => self.top = v as u8,
            0xC..=0xF => { let n = (d - 0xC) as usize; self.ct[n] = (v & 63) as u8; step[n] = false; }
            _ => {}
        }
    }

    fn apply_steps(&mut self, step: [bool; 4]) {
        for n in 0..4 { if step[n] { self.ct[n] = (self.ct[n] + 1) & 63; } }
    }

    fn operation(&mut self, op: u32) {
        let acl = self.a as u32;
        let pl = self.p as u32;
        let flags32 = |d: &mut Self, r: u32| { d.set_flag(ZF, r == 0); d.set_flag(SF, r & 0x8000_0000 != 0); };
        // the ALU (on the old A and P)
        match (op >> 26) & 0xF {
            0x1 | 0x2 | 0x3 => {
                let r = match (op >> 26) & 0xF { 1 => acl & pl, 2 => acl | pl, _ => acl ^ pl };
                self.alu = r as i64;
                flags32(self, r);
                self.set_flag(CF, false);
            }
            0x4 => {
                let wide = acl as u64 + pl as u64;
                let r = wide as u32;
                self.alu = r as i64;
                flags32(self, r);
                self.set_flag(CF, wide >> 32 != 0);
                self.set_flag(VF, (r ^ acl) & (r ^ pl) & 0x8000_0000 != 0);
            }
            0x5 => {
                let wide = (acl as u64).wrapping_sub(pl as u64);
                let r = wide as u32;
                self.alu = r as i64;
                flags32(self, r);
                self.set_flag(CF, wide >> 32 & 1 != 0);
                self.set_flag(VF, (acl ^ pl) & (acl ^ r) & 0x8000_0000 != 0);
            }
            0x6 => {
                let r = (self.a & MASK48) + (self.p & MASK48);
                self.alu = sx48(r);
                self.set_flag(ZF, r & MASK48 == 0);
                self.set_flag(SF, r & (1 << 47) != 0);
                self.set_flag(CF, r & (1 << 48) != 0);
            }
            0x8 => {
                let r = ((acl as i32) >> 1) as u32;
                self.alu = r as i64;
                flags32(self, r);
                self.set_flag(CF, acl & 1 != 0);
            }
            0x9 => {
                let r = acl.rotate_right(1);
                self.alu = r as i64;
                flags32(self, r);
                self.set_flag(CF, acl & 1 != 0);
            }
            0xA => {
                let r = acl << 1;
                self.alu = r as i64;
                flags32(self, r);
                self.set_flag(CF, acl & 0x8000_0000 != 0);
            }
            0xB => {
                let r = acl.rotate_left(1);
                self.alu = r as i64;
                flags32(self, r);
                self.set_flag(CF, acl & 0x8000_0000 != 0);
            }
            0xF => {
                let r = acl.rotate_left(8);
                self.alu = r as i64;
                flags32(self, r);
                self.set_flag(CF, acl & 0x0100_0000 != 0);
            }
            _ => {}
        }
        let mut step = [false; 4];
        // X bus
        if op & 0x0200_0000 != 0 { self.rx = self.source(op >> 20 & 7, &mut step); }
        match (op >> 23) & 3 {
            2 => self.p = self.mul,
            3 => self.p = self.source(op >> 20 & 7, &mut step) as i32 as i64,
            _ => {}
        }
        // Y bus
        if op & 0x8_0000 != 0 { self.ry = self.source(op >> 14 & 7, &mut step); }
        match (op >> 17) & 3 {
            1 => self.a = 0,
            2 => self.a = self.alu,
            3 => self.a = self.source(op >> 14 & 7, &mut step) as i32 as i64,
            _ => {}
        }
        // D1 bus
        match (op >> 12) & 3 {
            1 => self.dest(op >> 8 & 0xF, op as u8 as i8 as i32 as u32, &mut step),
            3 => {
                let s = op & 0xF;
                let v = match s {
                    0..=7 => self.source(s, &mut step),
                    9 => self.alu as u32,
                    0xA => (self.alu >> 16) as u32,
                    _ => 0,
                };
                self.dest(op >> 8 & 0xF, v, &mut step);
            }
            _ => {}
        }
        self.apply_steps(step);
    }

    fn move_immediate(&mut self, op: u32) {
        let (go, v) = if op & 0x0200_0000 != 0 {
            let v = op & 0x7_FFFF;
            (self.condition(op >> 19 & 0x7F), if v & 0x4_0000 != 0 { v | 0xFFF8_0000 } else { v })
        } else {
            let v = op & 0x1FF_FFFF;
            (true, if v & 0x100_0000 != 0 { v | 0xFE00_0000 } else { v })
        };
        if !go { return; }
        let d = op >> 26 & 0xF;
        if d == 0xC {
            self.top = self.pc;
            self.pc = v as u8;
        } else {
            let mut step = [false; 4];
            self.dest(d, v, &mut step);
            self.apply_steps(step);
        }
    }

    fn dma<B: DspBus>(&mut self, op: u32, bus: &mut B) {
        let hold = op & 0x4000 != 0;
        let from_d0 = op & 0x1000 == 0;
        let add_field = (op >> 15) & 7;
        let mut step = [false; 4];
        let (count, add) = if op & 0x2000 != 0 {
            let n = self.source(op & 7, &mut step);
            (n, if add_field == 0 { 0 } else { 4 })
        } else {
            (op & 0xFF, [0, 4, 4, 16, 16, 64, 128, 256][add_field as usize])
        };
        self.apply_steps(step);
        let ram = (op >> 8) & 7;
        let (mut src, mut dst) = (self.ra0 << 2, self.wa0 << 2);
        for k in 0..count {
            if from_d0 {
                let v = (bus.read16(src) as u32) << 16 | bus.read16(src.wrapping_add(2)) as u32;
                src = src.wrapping_add(add);
                match ram {
                    0..=3 => { let n = ram as usize; self.md[n * 64 + ((self.ct[n] as u32 + k) & 63) as usize] = v; }
                    4 => self.prog[(k & 0xFF) as usize] = v,
                    _ => {}
                }
                if !hold { self.ra0 = self.ra0.wrapping_add(add >> 2); }
            } else {
                let n = (ram & 3) as usize;
                let v = self.md[n * 64 + ((self.ct[n] as u32 + k) & 63) as usize];
                bus.write16(dst, (v >> 16) as u16);
                bus.write16(dst.wrapping_add(2), v as u16);
                dst = dst.wrapping_add(add);
                if !hold { self.wa0 = self.wa0.wrapping_add(add >> 2); }
            }
        }
    }

    fn jump(&mut self, op: u32) {
        if op & 0x0200_0000 != 0 && !self.condition(op >> 19 & 0x7F) { return; }
        self.delay = Some(self.pc);
        self.pc = op as u8;
    }

    fn lp(&mut self, op: u32) {
        if self.lop == 0 { return; }
        self.lop -= 1;
        self.delay = Some(self.pc);
        if op & 0x0800_0000 != 0 {
            self.pc = self.pc.wrapping_sub(1);   // LPS: back to itself after the next one
        } else {
            self.pc = self.top;                  // BTM
        }
    }

    fn end(&mut self, op: u32) {
        if op & 0x0800_0000 != 0 {
            self.flags |= EF;
            self.irq = true;
        }
        self.flags &= !EX;
    }
}
