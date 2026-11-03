//! Hitachi SH-2 CPU core (the 32X has two, as SH7604 chips at 23 MHz).
//! Ported from ares component/processor/sh2 (sh2.cpp, instruction.cpp, instructions.cpp,
//! decoder.hpp, exceptions.cpp; the interpreter, not the recompiler), ISC license; see
//! THIRD_PARTY_NOTICES.md.
//!
//! 16 32-bit registers, 16-bit instructions, a delay slot after every branch except
//! BT / BF. As in ares, `pc` runs 4 ahead of the instruction being executed (the
//! pipeline): the instruction is fetched from pc - 4. `ppm` / `ppc` hold a pending
//! branch: Slot = execute one more instruction (the delay slot) and then jump to ppc.
//!
//! This is the bare CPU: the SH7604's cache, on-chip registers (timers, DMA, divider,
//! interrupt controller) and address areas are the bus's job (`Sh2Bus`).

use serde::{Deserialize, Serialize};

pub trait Sh2Bus {
    fn read8(&mut self, a: u32) -> u8;
    fn read16(&mut self, a: u32) -> u16;
    fn read32(&mut self, a: u32) -> u32;
    fn write8(&mut self, a: u32, v: u8);
    fn write16(&mut self, a: u32, v: u16);
    fn write32(&mut self, a: u32, v: u32);
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Branch { Step, Slot, Take }

/// Status register bits.
#[derive(Clone, Copy, Default, Debug, Serialize, Deserialize)]
pub struct Sr { pub t: bool, pub s: bool, pub i: u8, pub q: bool, pub m: bool }

impl Sr {
    pub fn get(&self) -> u32 {
        self.t as u32 | (self.s as u32) << 1 | (self.i as u32) << 4 | (self.q as u32) << 8 | (self.m as u32) << 9
    }
    pub fn set(&mut self, v: u32) {
        self.t = v & 1 != 0;
        self.s = v & 2 != 0;
        self.i = (v >> 4 & 15) as u8;
        self.q = v & 0x100 != 0;
        self.m = v & 0x200 != 0;
    }
}

const RESET_COLD: u32 = 1;
const RESET_WARM: u32 = 2;

#[derive(Clone, Serialize, Deserialize)]
pub struct Sh2 {
    pub r: [u32; 16],
    /// the instruction being executed is at pc - 4
    pub pc: u32,
    pub pr: u32,
    pub gbr: u32,
    pub vbr: u32,
    pub mach: u32,
    pub macl: u32,
    pub sr: Sr,
    pub ppc: u32,
    pub ppm: Branch,
    /// an exception (interrupt) was taken: wakes SLEEP
    pub et: bool,
    /// interrupts held off for one instruction (after LDC / LDS / STC / STS)
    pub id: bool,
    /// pending reset
    pub exceptions: u32,
}

impl Default for Sh2 {
    fn default() -> Self { Self::new() }
}

impl Sh2 {
    pub fn new() -> Self {
        let mut s = Self { r: [0; 16], pc: 0, pr: 0, gbr: 0, vbr: 0, mach: 0, macl: 0, sr: Sr::default(), ppc: 0,
                           ppm: Branch::Step, et: false, id: false, exceptions: 0 };
        s.power(false);
        s
    }

    /// Power on (cold reset) or the reset line (warm): the vectors are read before the
    /// next instruction.
    pub fn power(&mut self, warm: bool) {
        self.r = [0; 16];
        self.pc = 0;
        self.vbr = 0;
        self.sr = Sr { i: 15, ..Sr::default() };
        self.ppc = 0;
        self.ppm = Branch::Step;
        self.et = false;
        self.id = false;
        self.exceptions = if warm { RESET_WARM } else { RESET_COLD };
    }

    /// Address of the instruction about to run.
    pub fn instruction_pc(&self) -> u32 { self.pc.wrapping_sub(4) }

    pub fn in_delay_slot(&self) -> bool { self.ppm != Branch::Step }

    /// Can a level-`level` interrupt be taken now?
    pub fn accepts(&self, level: u8) -> bool { !self.id && !self.in_delay_slot() && self.sr.i < level }

    /// Take an interrupt (the caller checked `accepts`).
    pub fn interrupt(&mut self, bus: &mut dyn Sh2Bus, level: u8, vector: u8) {
        self.push(bus, self.sr.get());
        self.push(bus, self.pc.wrapping_sub(4));
        let to = bus.read32(self.vbr.wrapping_add(vector as u32 * 4)).wrapping_add(4);
        self.jump(to);
        self.sr.i = level;
        self.et = true;
    }

    fn push(&mut self, bus: &mut dyn Sh2Bus, v: u32) {
        self.r[15] = self.r[15].wrapping_sub(4);
        bus.write32(self.r[15], v);
    }

    fn exception(&mut self, bus: &mut dyn Sh2Bus, vector: u32, ret: u32) {
        self.push(bus, self.sr.get());
        self.push(bus, ret);
        let to = bus.read32(self.vbr.wrapping_add(vector * 4)).wrapping_add(4);
        self.jump(to);
    }

    fn jump(&mut self, pc: u32) { self.pc = pc; self.ppm = Branch::Step; }
    fn branch(&mut self, pc: u32) { self.ppc = pc; self.ppm = Branch::Take; }
    fn delay_slot(&mut self, pc: u32) { self.ppc = pc; self.ppm = Branch::Slot; }

    fn illegal(&mut self, bus: &mut dyn Sh2Bus) {
        if self.in_delay_slot() { return self.illegal_slot(bus); }
        let ret = self.pc.wrapping_sub(4);
        self.exception(bus, 4, ret);
    }

    fn illegal_slot(&mut self, bus: &mut dyn Sh2Bus) {
        let ret = self.ppc.wrapping_sub(4);
        self.exception(bus, 6, ret);
    }

    /// Run one instruction.
    pub fn step(&mut self, bus: &mut dyn Sh2Bus) {
        if self.exceptions != 0 && !self.in_delay_slot() {
            let (sp, pc) = if self.exceptions & RESET_COLD != 0 { (4, 0) } else { (12, 8) };
            self.exceptions = 0;
            self.pc = bus.read32(pc).wrapping_add(4);
            self.r[15] = bus.read32(sp);
        }
        self.id = false;
        let op = bus.read16(self.pc.wrapping_sub(4));
        self.execute(bus, op);
        match self.ppm {
            Branch::Step => self.pc = self.pc.wrapping_add(2),
            Branch::Slot => { self.pc = self.pc.wrapping_add(2); self.ppm = Branch::Take; }
            Branch::Take => { self.pc = self.ppc; self.ppm = Branch::Step; }
        }
    }

    fn execute(&mut self, bus: &mut dyn Sh2Bus, op: u16) {
        let n = (op >> 8 & 15) as usize;
        let m = (op >> 4 & 15) as usize;
        let i = (op & 0xFF) as u32;
        let d4 = (op & 15) as u32;
        let d8 = (op & 0xFF) as u32;
        let d12 = (op & 0xFFF) as u32;
        let simm = i as u8 as i8 as i32 as u32;

        // n m - - and n m d / n imm forms
        match (op >> 8 & 0xF0) | (op & 0x0F) {
            0x04 => return bus.write8(self.r[n].wrapping_add(self.r[0]), self.r[m] as u8),
            0x05 => return bus.write16(self.r[n].wrapping_add(self.r[0]), self.r[m] as u16),
            0x06 => return bus.write32(self.r[n].wrapping_add(self.r[0]), self.r[m]),
            0x07 => { self.macl = self.r[n].wrapping_mul(self.r[m]); return; }
            0x0C => { self.r[n] = bus.read8(self.r[m].wrapping_add(self.r[0])) as i8 as u32; return; }
            0x0D => { self.r[n] = bus.read16(self.r[m].wrapping_add(self.r[0])) as i16 as u32; return; }
            0x0E => { self.r[n] = bus.read32(self.r[m].wrapping_add(self.r[0])); return; }
            0x0F => return self.mac_l(bus, m, n),
            0x10..=0x1F => return bus.write32(self.r[n].wrapping_add(d4 * 4), self.r[m]),
            0x20 => return bus.write8(self.r[n], self.r[m] as u8),
            0x21 => return bus.write16(self.r[n], self.r[m] as u16),
            0x22 => return bus.write32(self.r[n], self.r[m]),
            0x24 => { let t = self.r[m]; self.r[n] = self.r[n].wrapping_sub(1); return bus.write8(self.r[n], t as u8); }
            0x25 => { let t = self.r[m]; self.r[n] = self.r[n].wrapping_sub(2); return bus.write16(self.r[n], t as u16); }
            0x26 => { let t = self.r[m]; self.r[n] = self.r[n].wrapping_sub(4); return bus.write32(self.r[n], t); }
            0x27 => { self.sr.q = self.r[n] >> 31 != 0; self.sr.m = self.r[m] >> 31 != 0; self.sr.t = self.sr.q != self.sr.m; return; }
            0x28 => { self.sr.t = self.r[n] & self.r[m] == 0; return; }
            0x29 => { self.r[n] &= self.r[m]; return; }
            0x2A => { self.r[n] ^= self.r[m]; return; }
            0x2B => { self.r[n] |= self.r[m]; return; }
            0x2C => { let t = self.r[n] ^ self.r[m]; self.sr.t = (0..4).any(|k| t >> (k * 8) & 0xFF == 0); return; }
            0x2D => { self.r[n] = self.r[n] >> 16 | self.r[m] << 16; return; }
            0x2E => { self.macl = (self.r[n] as u16 as u32).wrapping_mul(self.r[m] as u16 as u32); return; }
            0x2F => { self.macl = ((self.r[n] as i16 as i32).wrapping_mul(self.r[m] as i16 as i32)) as u32; return; }
            0x30 => { self.sr.t = self.r[n] == self.r[m]; return; }
            0x32 => { self.sr.t = self.r[n] >= self.r[m]; return; }
            0x33 => { self.sr.t = self.r[n] as i32 >= self.r[m] as i32; return; }
            0x34 => return self.div1(m, n),
            0x35 => { let v = self.r[n] as u64 * self.r[m] as u64; self.set_mac(v); return; }
            0x36 => { self.sr.t = self.r[n] > self.r[m]; return; }
            0x37 => { self.sr.t = self.r[n] as i32 > self.r[m] as i32; return; }
            0x38 => { self.r[n] = self.r[n].wrapping_sub(self.r[m]); return; }
            0x3A => {
                let a = self.r[n];
                let b = a.wrapping_sub(self.r[m]);
                self.r[n] = b.wrapping_sub(self.sr.t as u32);
                self.sr.t = a < b || b < self.r[n];
                return;
            }
            0x3B => {
                let d = (self.r[n] as i32 >= 0) as u32 ^ 1;
                let s = ((self.r[m] as i32) < 0) as u32 + d;
                self.r[n] = self.r[n].wrapping_sub(self.r[m]);
                let res = ((self.r[n] as i32) < 0) as u32 + d;
                self.sr.t = s == 1 && res == 1;
                return;
            }
            0x3C => { self.r[n] = self.r[n].wrapping_add(self.r[m]); return; }
            0x3D => { let v = (self.r[n] as i32 as i64 * self.r[m] as i32 as i64) as u64; self.set_mac(v); return; }
            0x3E => {
                let a = self.r[n];
                let b = a.wrapping_add(self.r[m]);
                self.r[n] = b.wrapping_add(self.sr.t as u32);
                self.sr.t = a > b || b > self.r[n];
                return;
            }
            0x3F => {
                let d = ((self.r[n] as i32) < 0) as u32;
                let s = ((self.r[m] as i32) < 0) as u32 + d;
                self.r[n] = self.r[n].wrapping_add(self.r[m]);
                let res = ((self.r[n] as i32) < 0) as u32 + d;
                self.sr.t = s != 1 && res == 1;
                return;
            }
            0x4F => return self.mac_w(bus, m, n),
            0x50..=0x5F => { self.r[n] = bus.read32(self.r[m].wrapping_add(d4 * 4)); return; }
            0x60 => { self.r[n] = bus.read8(self.r[m]) as i8 as u32; return; }
            0x61 => { self.r[n] = bus.read16(self.r[m]) as i16 as u32; return; }
            0x62 => { self.r[n] = bus.read32(self.r[m]); return; }
            0x63 => { self.r[n] = self.r[m]; return; }
            0x64 => { let v = bus.read8(self.r[m]) as i8 as u32; self.r[n] = v; if n != m { self.r[m] = self.r[m].wrapping_add(1); } return; }
            0x65 => { let v = bus.read16(self.r[m]) as i16 as u32; self.r[n] = v; if n != m { self.r[m] = self.r[m].wrapping_add(2); } return; }
            0x66 => { let v = bus.read32(self.r[m]); self.r[n] = v; if n != m { self.r[m] = self.r[m].wrapping_add(4); } return; }
            0x67 => { self.r[n] = !self.r[m]; return; }
            0x68 => { self.r[n] = self.r[m] & !0xFFFF | self.r[m] >> 8 & 0xFF | self.r[m] << 8 & 0xFF00; return; }
            0x69 => { self.r[n] = self.r[m].rotate_left(16); return; }
            0x6A => {
                let t = 0u32.wrapping_sub(self.r[m]);
                self.r[n] = t.wrapping_sub(self.sr.t as u32);
                self.sr.t = 0 < t || t < self.r[n];
                return;
            }
            0x6B => { self.r[n] = 0u32.wrapping_sub(self.r[m]); return; }
            0x6C => { self.r[n] = self.r[m] & 0xFF; return; }
            0x6D => { self.r[n] = self.r[m] & 0xFFFF; return; }
            0x6E => { self.r[n] = self.r[m] as i8 as u32; return; }
            0x6F => { self.r[n] = self.r[m] as i16 as u32; return; }
            0x70..=0x7F => { self.r[n] = self.r[n].wrapping_add(simm); return; }
            0x90..=0x9F => {
                let pc = if self.in_delay_slot() { self.ppc.wrapping_sub(2) } else { self.pc };
                self.r[n] = bus.read16(pc.wrapping_add(d8 * 2)) as i16 as u32;
                return;
            }
            0xA0..=0xAF => {
                if self.in_delay_slot() { return self.illegal_slot(bus); }
                let d = ((d12 << 20) as i32 >> 20) as u32;
                return self.delay_slot(self.pc.wrapping_add(4).wrapping_add(d.wrapping_mul(2)));
            }
            0xB0..=0xBF => {
                if self.in_delay_slot() { return self.illegal_slot(bus); }
                self.pr = self.pc;
                let d = ((d12 << 20) as i32 >> 20) as u32;
                return self.delay_slot(self.pc.wrapping_add(4).wrapping_add(d.wrapping_mul(2)));
            }
            0xD0..=0xDF => {
                let pc = if self.in_delay_slot() { self.ppc.wrapping_sub(2) } else { self.pc };
                self.r[n] = bus.read32((pc & !3).wrapping_add(d8 * 4));
                return;
            }
            0xE0..=0xEF => { self.r[n] = simm; return; }
            _ => {}
        }

        // 8-bit opcode + immediate / displacement forms
        let gbr = self.gbr;
        match op >> 8 {
            0x80 => return bus.write8(self.r[m].wrapping_add(d4), self.r[0] as u8),
            0x81 => return bus.write16(self.r[m].wrapping_add(d4 * 2), self.r[0] as u16),
            0x84 => { self.r[0] = bus.read8(self.r[m].wrapping_add(d4)) as i8 as u32; return; }
            0x85 => { self.r[0] = bus.read16(self.r[m].wrapping_add(d4 * 2)) as i16 as u32; return; }
            0x88 => { self.sr.t = self.r[0] == simm; return; }
            0x89 | 0x8B | 0x8D | 0x8F => {
                if self.in_delay_slot() { return self.illegal_slot(bus); }
                let take = self.sr.t == (op >> 8 & 2 == 0); // 89/8D on T, 8B/8F on !T
                if take {
                    let to = self.pc.wrapping_add(4).wrapping_add(simm.wrapping_mul(2));
                    if op >> 8 & 4 != 0 { self.delay_slot(to) } else { self.branch(to) }
                }
                return;
            }
            0xC0 => return bus.write8(gbr.wrapping_add(d8), self.r[0] as u8),
            0xC1 => return bus.write16(gbr.wrapping_add(d8 * 2), self.r[0] as u16),
            0xC2 => return bus.write32(gbr.wrapping_add(d8 * 4), self.r[0]),
            0xC3 => {
                // TRAPA
                if self.in_delay_slot() { return self.illegal_slot(bus); }
                self.push(bus, self.sr.get());
                self.push(bus, self.pc.wrapping_sub(2));
                let to = bus.read32(self.vbr.wrapping_add(i * 4)).wrapping_add(4);
                return self.branch(to);
            }
            0xC4 => { self.r[0] = bus.read8(gbr.wrapping_add(d8)) as i8 as u32; return; }
            0xC5 => { self.r[0] = bus.read16(gbr.wrapping_add(d8 * 2)) as i16 as u32; return; }
            0xC6 => { self.r[0] = bus.read32(gbr.wrapping_add(d8 * 4)); return; }
            0xC7 => {
                let pc = if self.in_delay_slot() { self.ppc.wrapping_sub(2) } else { self.pc };
                self.r[0] = (pc & !3).wrapping_add(d8 * 4);
                return;
            }
            0xC8 => { self.sr.t = self.r[0] & i == 0; return; }
            0xC9 => { self.r[0] &= i; return; }
            0xCA => { self.r[0] ^= i; return; }
            0xCB => { self.r[0] |= i; return; }
            0xCC..=0xCF => {
                let a = gbr.wrapping_add(self.r[0]);
                let b = bus.read8(a) as u32;
                match op >> 8 {
                    0xCC => self.sr.t = b & i == 0,
                    0xCD => bus.write8(a, (b & i) as u8),
                    0xCE => bus.write8(a, (b ^ i) as u8),
                    _ => bus.write8(a, (b | i) as u8),
                }
                return;
            }
            _ => {}
        }

        // one-register forms
        match (op >> 4 & 0x0F00) | (op & 0xFF) {
            0x002 => { self.r[n] = self.sr.get(); self.id = true; return; }
            0x003 => {
                if self.in_delay_slot() { return self.illegal_slot(bus); }
                self.pr = self.pc;
                return self.delay_slot(self.pc.wrapping_add(4).wrapping_add(self.r[n]));
            }
            0x00A => { self.r[n] = self.mach; self.id = true; return; }
            0x012 => { self.r[n] = self.gbr; self.id = true; return; }
            0x01A => { self.r[n] = self.macl; self.id = true; return; }
            0x022 => { self.r[n] = self.vbr; self.id = true; return; }
            0x023 => {
                if self.in_delay_slot() { return self.illegal_slot(bus); }
                return self.delay_slot(self.pc.wrapping_add(4).wrapping_add(self.r[n]));
            }
            0x029 => { self.r[n] = self.sr.t as u32; return; }
            0x02A => { self.r[n] = self.pr; self.id = true; return; }
            0x400 | 0x420 => { self.sr.t = self.r[n] >> 31 != 0; self.r[n] <<= 1; return; }
            0x401 => { self.sr.t = self.r[n] & 1 != 0; self.r[n] >>= 1; return; }
            0x402 | 0x403 | 0x412 | 0x413 | 0x422 | 0x423 => {
                let v = match (op >> 4) & 0xF {
                    0x0 => if op & 1 == 0 { self.mach } else { self.sr.get() },
                    0x1 => if op & 1 == 0 { self.macl } else { self.gbr },
                    _ => if op & 1 == 0 { self.pr } else { self.vbr },
                };
                self.r[n] = self.r[n].wrapping_sub(4);
                bus.write32(self.r[n], v);
                self.id = true;
                return;
            }
            0x404 => { self.sr.t = self.r[n] >> 31 != 0; self.r[n] = self.r[n].rotate_left(1); return; }
            0x405 => { self.sr.t = self.r[n] & 1 != 0; self.r[n] = self.r[n].rotate_right(1); return; }
            0x406 | 0x407 | 0x416 | 0x417 | 0x426 | 0x427 => {
                let v = bus.read32(self.r[n]);
                self.r[n] = self.r[n].wrapping_add(4);
                match ((op >> 4) & 0xF, op & 1) {
                    (0, 0) => self.mach = v,
                    (0, _) => self.sr.set(v),
                    (1, 0) => self.macl = v,
                    (1, _) => self.gbr = v,
                    (_, 0) => self.pr = v,
                    _ => self.vbr = v,
                }
                self.id = true;
                return;
            }
            0x408 => { self.r[n] <<= 2; return; }
            0x409 => { self.r[n] >>= 2; return; }
            0x40A => { self.mach = self.r[n]; self.id = true; return; }
            0x40B => {
                if self.in_delay_slot() { return self.illegal_slot(bus); }
                self.pr = self.pc;
                return self.delay_slot(self.r[n].wrapping_add(4));
            }
            0x40E => { let v = self.r[n]; self.sr.set(v); self.id = true; return; }
            0x410 => { self.r[n] = self.r[n].wrapping_sub(1); self.sr.t = self.r[n] == 0; return; }
            0x411 => { self.sr.t = self.r[n] as i32 >= 0; return; }
            0x415 => { self.sr.t = self.r[n] as i32 > 0; return; }
            0x418 => { self.r[n] <<= 8; return; }
            0x419 => { self.r[n] >>= 8; return; }
            0x41A => { self.macl = self.r[n]; self.id = true; return; }
            0x41B => {
                // TAS.B: test and set bit 7
                let b = bus.read8(self.r[n]);
                self.sr.t = b == 0;
                return bus.write8(self.r[n], b | 0x80);
            }
            0x41E => { self.gbr = self.r[n]; self.id = true; return; }
            0x421 => { self.sr.t = self.r[n] & 1 != 0; self.r[n] = (self.r[n] as i32 >> 1) as u32; return; }
            0x424 => { let c = self.r[n] >> 31 != 0; self.r[n] = self.r[n] << 1 | self.sr.t as u32; self.sr.t = c; return; }
            0x425 => { let c = self.r[n] & 1 != 0; self.r[n] = self.r[n] >> 1 | (self.sr.t as u32) << 31; self.sr.t = c; return; }
            0x428 => { self.r[n] <<= 16; return; }
            0x429 => { self.r[n] >>= 16; return; }
            0x42A => { self.pr = self.r[n]; self.id = true; return; }
            0x42B => {
                if self.in_delay_slot() { return self.illegal_slot(bus); }
                return self.delay_slot(self.r[n].wrapping_add(4));
            }
            0x42E => { self.vbr = self.r[n]; self.id = true; return; }
            _ => {}
        }

        match op {
            0x0008 => self.sr.t = false,
            0x0009 => {}
            0x000B => {
                if self.in_delay_slot() { return self.illegal_slot(bus); }
                self.delay_slot(self.pr.wrapping_add(4));
            }
            0x0018 => self.sr.t = true,
            0x0019 => { self.sr.q = false; self.sr.m = false; self.sr.t = false; }
            0x001B => {
                // SLEEP: stay here until an interrupt has been taken
                if !self.et { self.pc = self.pc.wrapping_sub(2); } else { self.et = false; }
            }
            0x0028 => { self.mach = 0; self.macl = 0; }
            0x002B => {
                if self.in_delay_slot() { return self.illegal_slot(bus); }
                let sp = self.r[15];
                let to = bus.read32(sp).wrapping_add(4);
                self.delay_slot(to);
                let sr = bus.read32(sp.wrapping_add(4));
                self.sr.set(sr);
                self.r[15] = sp.wrapping_add(8);
            }
            _ => self.illegal(bus),
        }
    }

    fn set_mac(&mut self, v: u64) { self.mach = (v >> 32) as u32; self.macl = v as u32; }
    fn mac(&self) -> u64 { (self.mach as u64) << 32 | self.macl as u64 }

    fn div1(&mut self, m: usize, n: usize) {
        let q = self.sr.q;
        self.sr.q = self.r[n] >> 31 != 0;
        let mut r = ((self.r[n] << 1) | self.sr.t as u32) as u64;
        if q == self.sr.m { r = r.wrapping_sub(self.r[m] as u64); } else { r = r.wrapping_add(self.r[m] as u64); }
        self.r[n] = r as u32;
        self.sr.q = (self.sr.q ^ self.sr.m) ^ (r >> 32 & 1 != 0);
        self.sr.t = !(self.sr.q ^ self.sr.m);
    }

    /// MAC.W @Rm+,@Rn+ (S: 32-bit saturating, overflow sets MACH bit 0)
    fn mac_w(&mut self, bus: &mut dyn Sh2Bus, m: usize, n: usize) {
        let a = bus.read16(self.r[n]) as i16 as i64;
        self.r[n] = self.r[n].wrapping_add(2);
        let b = bus.read16(self.r[m]) as i16 as i64;
        self.r[m] = self.r[m].wrapping_add(2);
        if !self.sr.s {
            let v = self.mac().wrapping_add((a * b) as u64);
            self.set_mac(v);
        } else {
            let p = self.macl as i32 as i64 + a * b;
            let c = p.clamp(i32::MIN as i64, i32::MAX as i64);
            if c != p { self.mach |= 1; }
            self.macl = c as u32;
        }
    }

    /// MAC.L @Rm+,@Rn+ (S: the result saturates to 48 bits)
    fn mac_l(&mut self, bus: &mut dyn Sh2Bus, m: usize, n: usize) {
        let a = bus.read32(self.r[n]) as i32 as i64;
        self.r[n] = self.r[n].wrapping_add(4);
        let b = bus.read32(self.r[m]) as i32 as i64;
        self.r[m] = self.r[m].wrapping_add(4);
        let mut v = self.mac() as i64;
        v = v.wrapping_add(a * b);
        if self.sr.s { v = v.clamp(-(1i64 << 47), (1i64 << 47) - 1); }
        self.set_mac(v as u64);
    }
}

#[cfg(test)]
#[path = "sh2_tests.rs"]
mod tests;
