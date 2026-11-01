//! CMOS 6502 (65C02 family), one instruction at a time, with cycle counts.
//!
//! Two flavours:
//!   * `Variant::Sc02` - the 65SC02 (Synertek / VLSI; the Atari Lynx's CPU): the 65C02
//!     additions (BRA, PHX/PHY/PLX/PLY, STZ, TSB/TRB, INC A / DEC A, (zp), BIT #/zp,X/abs,X,
//!     JMP (abs,X)) but no Rockwell bit instructions: RMB/SMB/BBR/BBS are 1-byte NOPs.
//!   * `Variant::Wdc` - the WDC 65C02: adds RMB/SMB/BBR/BBS, WAI and STP.
//!
//! Checked against the SingleStepTests 65x02 suite (synertek65c02 and wdc65c02 sets):
//! final registers, memory and cycle count (see tests at the end of this file).
//! The bus sees the instruction's real reads and writes; the dummy reads some cycles
//! make on hardware are not repeated here.

use serde::{Deserialize, Serialize};

pub trait Bus65 {
    fn read(&mut self, addr: u16) -> u8;
    fn write(&mut self, addr: u16, value: u8);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Variant { Sc02, Wdc }

const C: u8 = 0x01;
const Z: u8 = 0x02;
const I: u8 = 0x04;
const D: u8 = 0x08;
const B: u8 = 0x10;
const U: u8 = 0x20;
const V: u8 = 0x40;
const N: u8 = 0x80;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode { Imp, Acc, Imm, Zp, ZpX, ZpY, Abs, AbsX, AbsY, IndX, IndY, IndZp, Rel }

#[derive(Clone, Serialize, Deserialize)]
pub struct Cpu65c02 {
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub s: u8,
    pub p: u8,
    pub pc: u16,
    pub variant: Variant,
    /// WAI: waiting for an interrupt (WDC only).
    pub waiting: bool,
    /// STP: stopped until reset (WDC only).
    pub stopped: bool,
    pub cycles: u64,
}

impl Cpu65c02 {
    pub fn new(variant: Variant) -> Self {
        Self { a: 0, x: 0, y: 0, s: 0xFD, p: U | I, pc: 0, variant, waiting: false, stopped: false, cycles: 0 }
    }

    pub fn reset(&mut self, bus: &mut dyn Bus65) {
        self.s = 0xFD;
        self.p = (self.p | I | U) & !D;
        self.waiting = false;
        self.stopped = false;
        self.pc = rd16(bus, 0xFFFC);
        self.cycles += 7;
    }

    /// Take an IRQ if `line` is asserted and I is clear; NMI always. Returns the cycles
    /// used (0 if nothing happened). A waiting CPU wakes on an IRQ even with I set.
    pub fn interrupt(&mut self, bus: &mut dyn Bus65, irq: bool, nmi: bool) -> u32 {
        if (irq || nmi) && self.waiting { self.waiting = false; }
        if nmi { return self.vector(bus, 0xFFFA, false); }
        if irq && self.p & I == 0 { return self.vector(bus, 0xFFFE, false); }
        0
    }

    fn vector(&mut self, bus: &mut dyn Bus65, v: u16, brk: bool) -> u32 {
        let pc = self.pc;
        self.push(bus, (pc >> 8) as u8);
        self.push(bus, pc as u8);
        let p = if brk { self.p | B | U } else { (self.p | U) & !B };
        self.push(bus, p);
        self.p = (self.p | I) & !D;
        self.pc = rd16(bus, v);
        self.cycles += 7;
        7
    }

    fn push(&mut self, bus: &mut dyn Bus65, v: u8) {
        bus.write(0x100 | self.s as u16, v);
        self.s = self.s.wrapping_sub(1);
    }
    fn pull(&mut self, bus: &mut dyn Bus65) -> u8 {
        self.s = self.s.wrapping_add(1);
        bus.read(0x100 | self.s as u16)
    }
    fn fetch(&mut self, bus: &mut dyn Bus65) -> u8 {
        let v = bus.read(self.pc);
        self.pc = self.pc.wrapping_add(1);
        v
    }
    fn fetch16(&mut self, bus: &mut dyn Bus65) -> u16 {
        let lo = self.fetch(bus) as u16;
        lo | (self.fetch(bus) as u16) << 8
    }
    fn nz(&mut self, v: u8) {
        self.p = (self.p & !(N | Z)) | (v & N) | if v == 0 { Z } else { 0 };
    }

    /// Effective address and whether indexing crossed a page.
    fn addr(&mut self, bus: &mut dyn Bus65, m: Mode) -> (u16, bool) {
        match m {
            Mode::Zp => (self.fetch(bus) as u16, false),
            Mode::ZpX => (self.fetch(bus).wrapping_add(self.x) as u16, false),
            Mode::ZpY => (self.fetch(bus).wrapping_add(self.y) as u16, false),
            Mode::Abs => (self.fetch16(bus), false),
            Mode::AbsX => { let b = self.fetch16(bus); let a = b.wrapping_add(self.x as u16); (a, a & 0xFF00 != b & 0xFF00) }
            Mode::AbsY => { let b = self.fetch16(bus); let a = b.wrapping_add(self.y as u16); (a, a & 0xFF00 != b & 0xFF00) }
            Mode::IndX => { let z = self.fetch(bus).wrapping_add(self.x); (zp16(bus, z), false) }
            Mode::IndY => {
                let z = self.fetch(bus);
                let b = zp16(bus, z);
                let a = b.wrapping_add(self.y as u16);
                (a, a & 0xFF00 != b & 0xFF00)
            }
            Mode::IndZp => { let z = self.fetch(bus); (zp16(bus, z), false) }
            _ => { let a = self.pc; self.pc = self.pc.wrapping_add(1); (a, false) }   // Imm
        }
    }

    /// Cycles for a read instruction in mode `m` (with the page-cross penalty).
    fn read_cycles(m: Mode, crossed: bool) -> u32 {
        (match m {
            Mode::Imm => 2, Mode::Zp => 3, Mode::ZpX | Mode::ZpY | Mode::Abs => 4,
            Mode::AbsX | Mode::AbsY => 4, Mode::IndX => 6, Mode::IndY => 5, Mode::IndZp => 5, _ => 2,
        }) + crossed as u32
    }

    fn store_cycles(m: Mode) -> u32 {
        match m { Mode::Zp => 3, Mode::ZpX | Mode::ZpY | Mode::Abs => 4, Mode::AbsX | Mode::AbsY => 5,
                  Mode::IndX | Mode::IndY => 6, Mode::IndZp => 5, _ => 2 }
    }

    fn adc(&mut self, v: u8) -> u32 {
        let c = (self.p & C) as u16;
        if self.p & D == 0 {
            let r = self.a as u16 + v as u16 + c;
            let o = (!(self.a ^ v) & (self.a ^ r as u8) & 0x80) != 0;
            self.p = (self.p & !(C | V)) | if r > 0xFF { C } else { 0 } | if o { V } else { 0 };
            self.a = r as u8;
            self.nz(self.a);
            0
        } else {
            let a = self.a as u16;
            let mut lo = (a & 0x0F) + (v as u16 & 0x0F) + c;
            if lo > 9 { lo += 6; }
            let mut r = (a & 0xF0) + (v as u16 & 0xF0) + if lo > 0x0F { 0x10 } else { 0 } + (lo & 0x0F);
            let o = (!(self.a ^ v) & (self.a ^ r as u8) & 0x80) != 0;
            if r > 0x9F { r += 0x60; }
            self.p = (self.p & !(C | V)) | if r > 0xFF { C } else { 0 } | if o { V } else { 0 };
            self.a = r as u8;
            self.nz(self.a);
            1
        }
    }

    fn sbc(&mut self, v: u8) -> u32 {
        if self.p & D == 0 { return self.adc(!v); }
        let c = (self.p & C) as i16;
        let a = self.a as i16;
        let bin = a - v as i16 - (1 - c);
        let o = ((self.a ^ v) & (self.a ^ bin as u8) & 0x80) != 0;
        let mut lo = (a & 0x0F) - (v as i16 & 0x0F) - (1 - c);
        let mut r = a - v as i16 - (1 - c);
        if r < 0 { r -= 0x60; }
        if lo < 0 { r -= 6; lo = 0; }
        let _ = lo;
        self.p = (self.p & !(C | V)) | if bin >= 0 { C } else { 0 } | if o { V } else { 0 };
        self.a = r as u8;
        self.nz(self.a);
        1
    }

    fn cmp(&mut self, r: u8, v: u8) {
        let d = r.wrapping_sub(v);
        self.p = (self.p & !C) | if r >= v { C } else { 0 };
        self.nz(d);
    }

    fn branch(&mut self, bus: &mut dyn Bus65, cond: bool) -> u32 {
        let off = self.fetch(bus) as i8;
        if !cond { return 2; }
        let t = self.pc.wrapping_add(off as u16);
        let cross = t & 0xFF00 != self.pc & 0xFF00;
        self.pc = t;
        3 + cross as u32
    }

    /// Read-modify-write on memory or A.
    fn rmw(&mut self, bus: &mut dyn Bus65, m: Mode, f: fn(&mut Self, u8) -> u8, incdec: bool) -> u32 {
        if m == Mode::Acc {
            let r = f(self, self.a);
            self.a = r;
            return 2;
        }
        let (a, cross) = self.addr(bus, m);
        let v = bus.read(a);
        let r = f(self, v);
        bus.write(a, r);
        match m {
            Mode::Zp => 5, Mode::ZpX | Mode::Abs => 6,
            _ => if incdec { 7 } else { 6 + cross as u32 },   // abs,X
        }
    }

    fn asl(&mut self, v: u8) -> u8 { self.p = (self.p & !C) | (v >> 7); let r = v << 1; self.nz(r); r }
    fn lsr(&mut self, v: u8) -> u8 { self.p = (self.p & !C) | (v & 1); let r = v >> 1; self.nz(r); r }
    fn rol(&mut self, v: u8) -> u8 { let r = v << 1 | (self.p & C); self.p = (self.p & !C) | (v >> 7); self.nz(r); r }
    fn ror(&mut self, v: u8) -> u8 { let r = v >> 1 | (self.p & C) << 7; self.p = (self.p & !C) | (v & 1); self.nz(r); r }
    fn inc(&mut self, v: u8) -> u8 { let r = v.wrapping_add(1); self.nz(r); r }
    fn dec(&mut self, v: u8) -> u8 { let r = v.wrapping_sub(1); self.nz(r); r }

    /// Run one instruction; returns the cycles it took (1 while WAI / STP holds the CPU).
    pub fn step(&mut self, bus: &mut dyn Bus65) -> u32 {
        if self.waiting || self.stopped { self.cycles += 1; return 1; }
        let op = self.fetch(bus);
        let n = self.exec(bus, op);
        self.cycles += n as u64;
        n
    }

    fn exec(&mut self, bus: &mut dyn Bus65, op: u8) -> u32 {
        use Mode::*;
        // the regular ALU groups: column / row decoding
        let alu_mode = |op: u8| match op & 0x1F {
            0x01 => IndX, 0x05 => Zp, 0x09 => Imm, 0x0D => Abs,
            0x11 => IndY, 0x12 => IndZp, 0x15 => ZpX, 0x19 => AbsY, 0x1D => AbsX, _ => Imp,
        };
        match op {
            // ORA AND EOR ADC (STA) LDA CMP SBC
            _ if matches!(op & 0x1F, 0x01 | 0x05 | 0x09 | 0x0D | 0x11 | 0x12 | 0x15 | 0x19 | 0x1D) => {
                let m = alu_mode(op);
                if op & 0xE0 == 0x80 {                              // STA
                    if m == Imm {                                   // 89 is BIT # (Z only)
                        let v = self.fetch(bus);
                        self.p = (self.p & !Z) | if self.a & v == 0 { Z } else { 0 };
                        return 2;
                    }
                    let (a, _) = self.addr(bus, m);
                    bus.write(a, self.a);
                    return Self::store_cycles(m);
                }
                let (a, cross) = self.addr(bus, m);
                let v = bus.read(a);
                let cyc = Self::read_cycles(m, cross);
                match op >> 5 {
                    0 => { self.a |= v; self.nz(self.a); }
                    1 => { self.a &= v; self.nz(self.a); }
                    2 => { self.a ^= v; self.nz(self.a); }
                    3 => return cyc + self.adc(v),
                    5 => { self.a = v; self.nz(v); }
                    6 => self.cmp(self.a, v),
                    _ => return cyc + self.sbc(v),
                }
                cyc
            }
            0x24 | 0x2C | 0x34 | 0x3C => {                          // BIT
                let m = match op { 0x24 => Zp, 0x2C => Abs, 0x34 => ZpX, _ => AbsX };
                let (a, cross) = self.addr(bus, m);
                let v = bus.read(a);
                self.p = (self.p & !(Z | V | N)) | (v & (V | N)) | if self.a & v == 0 { Z } else { 0 };
                Self::read_cycles(m, cross)
            }
            // shifts and INC / DEC
            0x06 | 0x0A | 0x0E | 0x16 | 0x1E => { let m = shift_mode(op); self.rmw(bus, m, Self::asl, false) }
            0x26 | 0x2A | 0x2E | 0x36 | 0x3E => { let m = shift_mode(op); self.rmw(bus, m, Self::rol, false) }
            0x46 | 0x4A | 0x4E | 0x56 | 0x5E => { let m = shift_mode(op); self.rmw(bus, m, Self::lsr, false) }
            0x66 | 0x6A | 0x6E | 0x76 | 0x7E => { let m = shift_mode(op); self.rmw(bus, m, Self::ror, false) }
            0xC6 | 0xCE | 0xD6 | 0xDE => { let m = shift_mode(op); self.rmw(bus, m, Self::dec, true) }
            0xE6 | 0xEE | 0xF6 | 0xFE => { let m = shift_mode(op); self.rmw(bus, m, Self::inc, true) }
            0x1A => { self.a = self.inc(self.a); 2 }
            0x3A => { self.a = self.dec(self.a); 2 }
            // TSB / TRB
            0x04 | 0x0C | 0x14 | 0x1C => {
                let m = if op & 0x08 != 0 { Abs } else { Zp };
                let (a, _) = self.addr(bus, m);
                let v = bus.read(a);
                self.p = (self.p & !Z) | if self.a & v == 0 { Z } else { 0 };
                bus.write(a, if op & 0x10 == 0 { v | self.a } else { v & !self.a });
                if m == Abs { 6 } else { 5 }
            }
            // loads / stores / compares of X and Y
            0xA2 | 0xA6 | 0xAE | 0xB6 | 0xBE => {
                let m = match op { 0xA2 => Imm, 0xA6 => Zp, 0xAE => Abs, 0xB6 => ZpY, _ => AbsY };
                let (a, cross) = self.addr(bus, m);
                self.x = bus.read(a); self.nz(self.x);
                Self::read_cycles(m, cross)
            }
            0xA0 | 0xA4 | 0xAC | 0xB4 | 0xBC => {
                let m = match op { 0xA0 => Imm, 0xA4 => Zp, 0xAC => Abs, 0xB4 => ZpX, _ => AbsX };
                let (a, cross) = self.addr(bus, m);
                self.y = bus.read(a); self.nz(self.y);
                Self::read_cycles(m, cross)
            }
            0x86 | 0x8E | 0x96 => { let m = match op { 0x86 => Zp, 0x8E => Abs, _ => ZpY }; let (a, _) = self.addr(bus, m); bus.write(a, self.x); Self::store_cycles(m) }
            0x84 | 0x8C | 0x94 => { let m = match op { 0x84 => Zp, 0x8C => Abs, _ => ZpX }; let (a, _) = self.addr(bus, m); bus.write(a, self.y); Self::store_cycles(m) }
            0x64 | 0x74 | 0x9C | 0x9E => {                          // STZ
                let m = match op { 0x64 => Zp, 0x74 => ZpX, 0x9C => Abs, _ => AbsX };
                let (a, _) = self.addr(bus, m); bus.write(a, 0); Self::store_cycles(m)
            }
            0xE0 | 0xE4 | 0xEC | 0xC0 | 0xC4 | 0xCC => {
                let m = match op & 0x0F { 0x00 => Imm, 0x04 => Zp, _ => Abs };
                let (a, cross) = self.addr(bus, m);
                let v = bus.read(a);
                if op & 0x20 != 0 { self.cmp(self.x, v) } else { self.cmp(self.y, v) }
                Self::read_cycles(m, cross)
            }
            // branches
            0x10 => { let c = self.p & N == 0; self.branch(bus, c) }
            0x30 => { let c = self.p & N != 0; self.branch(bus, c) }
            0x50 => { let c = self.p & V == 0; self.branch(bus, c) }
            0x70 => { let c = self.p & V != 0; self.branch(bus, c) }
            0x90 => { let c = self.p & C == 0; self.branch(bus, c) }
            0xB0 => { let c = self.p & C != 0; self.branch(bus, c) }
            0xD0 => { let c = self.p & Z == 0; self.branch(bus, c) }
            0xF0 => { let c = self.p & Z != 0; self.branch(bus, c) }
            0x80 => self.branch(bus, true),                         // BRA
            // jumps and calls
            0x4C => { self.pc = self.fetch16(bus); 3 }
            0x6C => { let a = self.fetch16(bus); self.pc = rd16(bus, a); 6 }
            0x7C => { let a = self.fetch16(bus).wrapping_add(self.x as u16); self.pc = rd16(bus, a); 6 }
            0x20 => {
                let lo = self.fetch(bus) as u16;
                let ret = self.pc;
                self.push(bus, (ret >> 8) as u8);
                self.push(bus, ret as u8);
                let hi = bus.read(self.pc) as u16;
                self.pc = lo | hi << 8;
                6
            }
            0x60 => { let lo = self.pull(bus) as u16; let hi = self.pull(bus) as u16; self.pc = (lo | hi << 8).wrapping_add(1); 6 }
            0x40 => {
                let p = self.pull(bus);
                self.p = (p | U) & !B;
                let lo = self.pull(bus) as u16; let hi = self.pull(bus) as u16;
                self.pc = lo | hi << 8;
                6
            }
            0x00 => { self.pc = self.pc.wrapping_add(1); self.vector(bus, 0xFFFE, true); 7 }
            // stack
            0x48 => { self.push(bus, self.a); 3 }
            0xDA => { self.push(bus, self.x); 3 }
            0x5A => { self.push(bus, self.y); 3 }
            0x08 => { self.push(bus, self.p | B | U); 3 }
            0x68 => { self.a = self.pull(bus); self.nz(self.a); 4 }
            0xFA => { self.x = self.pull(bus); self.nz(self.x); 4 }
            0x7A => { self.y = self.pull(bus); self.nz(self.y); 4 }
            0x28 => { self.p = (self.pull(bus) | U) & !B; 4 }
            // flags and transfers
            0x18 => { self.p &= !C; 2 } 0x38 => { self.p |= C; 2 }
            0x58 => { self.p &= !I; 2 } 0x78 => { self.p |= I; 2 }
            0xB8 => { self.p &= !V; 2 }
            0xD8 => { self.p &= !D; 2 } 0xF8 => { self.p |= D; 2 }
            0xAA => { self.x = self.a; self.nz(self.x); 2 }
            0xA8 => { self.y = self.a; self.nz(self.y); 2 }
            0x8A => { self.a = self.x; self.nz(self.a); 2 }
            0x98 => { self.a = self.y; self.nz(self.a); 2 }
            0xBA => { self.x = self.s; self.nz(self.x); 2 }
            0x9A => { self.s = self.x; 2 }
            0xE8 => { self.x = self.x.wrapping_add(1); self.nz(self.x); 2 }
            0xC8 => { self.y = self.y.wrapping_add(1); self.nz(self.y); 2 }
            0xCA => { self.x = self.x.wrapping_sub(1); self.nz(self.x); 2 }
            0x88 => { self.y = self.y.wrapping_sub(1); self.nz(self.y); 2 }
            0xEA => 2,
            // WDC only: WAI, STP, and the Rockwell bit instructions
            0xCB if self.variant == Variant::Wdc => { self.waiting = true; 3 }
            0xDB if self.variant == Variant::Wdc => { self.stopped = true; 3 }
            _ if op & 0x0F == 0x07 && self.variant == Variant::Wdc => {   // RMB / SMB
                let z = self.fetch(bus) as u16;
                let v = bus.read(z);
                let bit = 1 << ((op >> 4) & 7);
                bus.write(z, if op & 0x80 == 0 { v & !bit } else { v | bit });
                5
            }
            _ if op & 0x0F == 0x0F && self.variant == Variant::Wdc => {   // BBR / BBS
                let z = self.fetch(bus) as u16;
                let v = bus.read(z);
                let bit = v & (1 << ((op >> 4) & 7)) != 0;
                let set = op & 0x80 != 0;
                5 + self.branch(bus, bit == set) - 2
            }
            // the rest are NOPs of various lengths
            0x02 | 0x22 | 0x42 | 0x62 | 0x82 | 0xC2 | 0xE2 => { self.fetch(bus); 2 }
            0x44 => { self.fetch(bus); 3 }
            0x54 | 0xD4 | 0xF4 => { self.fetch(bus); 4 }
            0x5C => { self.fetch16(bus); if self.variant == Variant::Wdc { 8 } else { 4 } }
            // 65SC02: the bit-instruction slots are NOPs (x7: 2 bytes, xF: 3 bytes; 3 cycles
            // in even rows, 4 in odd ones), CB 1 byte / 2 cycles, DB 2 bytes / 4 cycles
            _ if op & 0x0F == 0x07 => { self.fetch(bus); 3 + (op >> 4 & 1) as u32 }
            _ if op & 0x0F == 0x0F => { self.fetch16(bus); 3 + (op >> 4 & 1) as u32 }
            0xCB => 2,
            0xDB => { self.fetch(bus); 4 }
            0xDC | 0xFC => { self.fetch16(bus); 4 }
            _ => 1,                                                 // x3, xB
        }
    }
}

fn shift_mode(op: u8) -> Mode {
    match op & 0x1F {
        0x06 => Mode::Zp, 0x0A => Mode::Acc, 0x0E => Mode::Abs, 0x16 => Mode::ZpX, _ => Mode::AbsX,
    }
}

fn rd16(bus: &mut dyn Bus65, a: u16) -> u16 {
    bus.read(a) as u16 | (bus.read(a.wrapping_add(1)) as u16) << 8
}

fn zp16(bus: &mut dyn Bus65, z: u8) -> u16 {
    bus.read(z as u16) as u16 | (bus.read(z.wrapping_add(1) as u16) as u16) << 8
}

#[cfg(test)]
#[path = "m65c02_tests.rs"]
mod tests;
