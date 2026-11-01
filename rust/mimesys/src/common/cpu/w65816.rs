//! WDC 65C816 (the SNES's Ricoh 5A22 core, and the SA-1's).
//!
//! A port of ares's wdc65816 (Near et al, ISC license; see THIRD_PARTY_NOTICES): every
//! instruction makes the same sequence of bus reads, writes and internal ("idle")
//! cycles, so a machine can charge each its own time (the SNES's memory speeds).
//! Checked against SingleStepTests 65816 (final state and cycle count; m65816_tests.rs).
//!
//! The bus sees 24-bit addresses. `step` runs one instruction (or the interrupt
//! sequence when one is pending); WAI and STP leave the CPU idling.

use serde::{Deserialize, Serialize};

pub trait Bus816 {
    fn read(&mut self, addr: u32) -> u8;
    fn write(&mut self, addr: u32, v: u8);
    /// An internal cycle (no memory access).
    fn idle(&mut self);
    /// Whether an interrupt will be taken after this instruction (turns the "idle" of
    /// some two-cycle instructions into a read, as on hardware).
    fn interrupt_pending(&self) -> bool { false }
}

pub const C: u8 = 0x01;
pub const Z: u8 = 0x02;
pub const I: u8 = 0x04;
pub const D: u8 = 0x08;
pub const X: u8 = 0x10;
pub const M: u8 = 0x20;
pub const V: u8 = 0x40;
pub const N: u8 = 0x80;

#[derive(Clone, Serialize, Deserialize)]
pub struct W65816 {
    /// program counter (bank in bits 16-23)
    pub pc: u32,
    pub a: u16,
    pub x: u16,
    pub y: u16,
    pub s: u16,
    pub d: u16,
    pub b: u8,
    pub p: u8,
    /// emulation mode
    pub e: bool,
    pub wai: bool,
    pub stp: bool,
}

type Alu8 = fn(&mut W65816, u8) -> u8;
type Alu16 = fn(&mut W65816, u16) -> u16;

#[derive(Clone, Copy)]
enum Reg { A, X, Y, Z, S, D }

impl W65816 {
    pub fn new() -> Self {
        Self { pc: 0, a: 0, x: 0, y: 0, s: 0x01FF, d: 0, b: 0, p: 0x34, e: true, wai: false, stp: false }
    }

    /// Reset: emulation mode, vector at FFFC.
    pub fn reset(&mut self, bus: &mut dyn Bus816) {
        self.e = true;
        self.p = (self.p | 0x34) & !D;
        self.x &= 0xFF;
        self.y &= 0xFF;
        self.s = 0x0100 | (self.s & 0xFF);
        self.d = 0;
        self.b = 0;
        self.wai = false;
        self.stp = false;
        let lo = bus.read(0xFFFC) as u32;
        let hi = bus.read(0xFFFD) as u32;
        self.pc = hi << 8 | lo;
    }

    fn mf(&self) -> bool { self.p & M != 0 }
    fn xf(&self) -> bool { self.p & X != 0 }
    fn flag(&mut self, f: u8, on: bool) { if on { self.p |= f } else { self.p &= !f } }
    fn nz8(&mut self, v: u8) { self.flag(Z, v == 0); self.flag(N, v & 0x80 != 0); }
    fn nz16(&mut self, v: u16) { self.flag(Z, v == 0); self.flag(N, v & 0x8000 != 0); }

    /// Load P, applying the emulation / index-width rules.
    fn set_p(&mut self, v: u8) {
        self.p = v;
        if self.e { self.p |= M | X; }
        if self.xf() { self.x &= 0xFF; self.y &= 0xFF; }
    }

    fn get(&self, r: Reg) -> u16 {
        match r { Reg::A => self.a, Reg::X => self.x, Reg::Y => self.y, Reg::Z => 0, Reg::S => self.s, Reg::D => self.d }
    }
    fn set_lo(&mut self, r: Reg, v: u8) {
        let f = |x: u16| (x & 0xFF00) | v as u16;
        match r { Reg::A => self.a = f(self.a), Reg::X => self.x = f(self.x), Reg::Y => self.y = f(self.y),
                  Reg::S => self.s = f(self.s), Reg::D => self.d = f(self.d), Reg::Z => {} }
    }
    fn set_w(&mut self, r: Reg, v: u16) {
        match r { Reg::A => self.a = v, Reg::X => self.x = v, Reg::Y => self.y = v, Reg::S => self.s = v, Reg::D => self.d = v, Reg::Z => {} }
    }

    // ---------------- memory ----------------

    fn pcb(&self) -> u32 { self.pc & 0xFF0000 }
    fn pcw(&self) -> u16 { self.pc as u16 }
    fn set_pcw(&mut self, w: u16) { self.pc = self.pcb() | w as u32; }

    fn fetch(&mut self, bus: &mut dyn Bus816) -> u8 {
        let v = bus.read(self.pc);
        self.set_pcw(self.pcw().wrapping_add(1));
        v
    }
    fn fetch16(&mut self, bus: &mut dyn Bus816) -> u16 { let l = self.fetch(bus) as u16; l | (self.fetch(bus) as u16) << 8 }

    fn idle_irq(&mut self, bus: &mut dyn Bus816) {
        if bus.interrupt_pending() { bus.read(self.pc); } else { bus.idle(); }
    }
    fn idle2(&mut self, bus: &mut dyn Bus816) { if self.d & 0xFF != 0 { bus.idle(); } }
    fn idle4(&mut self, bus: &mut dyn Bus816, x: u16, y: u16) { if !self.xf() || x >> 8 != y >> 8 { bus.idle(); } }
    fn idle6(&mut self, bus: &mut dyn Bus816, addr: u16) { if self.e && (self.pcw() >> 8) != addr >> 8 { bus.idle(); } }

    fn push(&mut self, bus: &mut dyn Bus816, v: u8) {
        bus.write(self.s as u32, v);
        if self.e { self.s = (self.s & 0xFF00) | (self.s as u8).wrapping_sub(1) as u16; } else { self.s = self.s.wrapping_sub(1); }
    }
    fn pull(&mut self, bus: &mut dyn Bus816) -> u8 {
        if self.e { self.s = (self.s & 0xFF00) | (self.s as u8).wrapping_add(1) as u16; } else { self.s = self.s.wrapping_add(1); }
        bus.read(self.s as u32)
    }
    fn push_n(&mut self, bus: &mut dyn Bus816, v: u8) { bus.write(self.s as u32, v); self.s = self.s.wrapping_sub(1); }
    fn pull_n(&mut self, bus: &mut dyn Bus816) -> u8 { self.s = self.s.wrapping_add(1); bus.read(self.s as u32) }
    fn fix_s(&mut self) { if self.e { self.s = 0x0100 | (self.s & 0xFF); } }

    fn read_direct(&mut self, bus: &mut dyn Bus816, a: u32) -> u8 {
        if self.e && self.d & 0xFF == 0 { return bus.read(self.d as u32 | (a & 0xFF)); }
        bus.read((self.d as u32).wrapping_add(a) & 0xFFFF)
    }
    fn write_direct(&mut self, bus: &mut dyn Bus816, a: u32, v: u8) {
        if self.e && self.d & 0xFF == 0 { return bus.write(self.d as u32 | (a & 0xFF), v); }
        bus.write((self.d as u32).wrapping_add(a) & 0xFFFF, v)
    }
    /// The (dp,X) pointer bytes. (ares wraps the second byte within the page in emulation
    /// mode when D's low byte is set; the SingleStepTests data says it doesn't.)
    fn read_direct_x(&mut self, bus: &mut dyn Bus816, a: u32, off: u32) -> u8 {
        self.read_direct(bus, a + off)
    }
    fn read_direct_n(&mut self, bus: &mut dyn Bus816, a: u32) -> u8 { bus.read((self.d as u32 + a) & 0xFFFF) }
    fn read_bank(&mut self, bus: &mut dyn Bus816, a: u32) -> u8 { bus.read(((self.b as u32) << 16).wrapping_add(a) & 0xFFFFFF) }
    fn write_bank(&mut self, bus: &mut dyn Bus816, a: u32, v: u8) { bus.write(((self.b as u32) << 16).wrapping_add(a) & 0xFFFFFF, v) }
    fn read_stack(&mut self, bus: &mut dyn Bus816, a: u32) -> u8 { bus.read((self.s as u32 + a) & 0xFFFF) }
    fn write_stack(&mut self, bus: &mut dyn Bus816, a: u32, v: u8) { bus.write((self.s as u32 + a) & 0xFFFF, v) }

    // ---------------- ALU ----------------

    fn adc8(&mut self, data: u8) -> u8 {
        let a = self.a as u8 as i32;
        let d = data as i32;
        let c = (self.p & C) as i32;
        let mut r;
        if self.p & D == 0 { r = a + d + c; } else {
            r = (a & 0x0F) + (d & 0x0F) + c;
            if r > 0x09 { r += 0x06; }
            let cf = (r > 0x0F) as i32;
            r = (a & 0xF0) + (d & 0xF0) + (cf << 4) + (r & 0x0F);
        }
        self.flag(V, !(a ^ d) & (a ^ r) & 0x80 != 0);
        if self.p & D != 0 && r > 0x9F { r += 0x60; }
        self.flag(C, r > 0xFF);
        self.nz8(r as u8);
        self.a = (self.a & 0xFF00) | (r as u8) as u16;
        r as u8
    }
    fn adc16(&mut self, data: u16) -> u16 {
        let a = self.a as i32;
        let d = data as i32;
        let c = (self.p & C) as i32;
        let mut r;
        if self.p & D == 0 { r = a + d + c; } else {
            r = (a & 0x000F) + (d & 0x000F) + c;
            if r > 0x0009 { r += 0x0006; }
            let mut cf = (r > 0x000F) as i32;
            r = (a & 0x00F0) + (d & 0x00F0) + (cf << 4) + (r & 0x000F);
            if r > 0x009F { r += 0x0060; }
            cf = (r > 0x00FF) as i32;
            r = (a & 0x0F00) + (d & 0x0F00) + (cf << 8) + (r & 0x00FF);
            if r > 0x09FF { r += 0x0600; }
            cf = (r > 0x0FFF) as i32;
            r = (a & 0xF000) + (d & 0xF000) + (cf << 12) + (r & 0x0FFF);
        }
        self.flag(V, !(a ^ d) & (a ^ r) & 0x8000 != 0);
        if self.p & D != 0 && r > 0x9FFF { r += 0x6000; }
        self.flag(C, r > 0xFFFF);
        self.nz16(r as u16);
        self.a = r as u16;
        r as u16
    }
    fn sbc8(&mut self, data: u8) -> u8 {
        let a = self.a as u8 as i32;
        let d = (!data) as i32;
        let c = (self.p & C) as i32;
        let mut r;
        if self.p & D == 0 { r = a + d + c; } else {
            r = (a & 0x0F) + (d & 0x0F) + c;
            if r <= 0x0F { r -= 0x06; }
            let cf = (r > 0x0F) as i32;
            r = (a & 0xF0) + (d & 0xF0) + (cf << 4) + (r & 0x0F);
        }
        self.flag(V, !(a ^ d) & (a ^ r) & 0x80 != 0);
        if self.p & D != 0 && r <= 0xFF { r -= 0x60; }
        self.flag(C, r > 0xFF);
        self.nz8(r as u8);
        self.a = (self.a & 0xFF00) | (r as u8) as u16;
        r as u8
    }
    fn sbc16(&mut self, data: u16) -> u16 {
        let a = self.a as i32;
        let d = (!data) as i32;
        let c = (self.p & C) as i32;
        let mut r;
        if self.p & D == 0 { r = a + d + c; } else {
            r = (a & 0x000F) + (d & 0x000F) + c;
            if r <= 0x000F { r -= 0x0006; }
            let mut cf = (r > 0x000F) as i32;
            r = (a & 0x00F0) + (d & 0x00F0) + (cf << 4) + (r & 0x000F);
            if r <= 0x00FF { r -= 0x0060; }
            cf = (r > 0x00FF) as i32;
            r = (a & 0x0F00) + (d & 0x0F00) + (cf << 8) + (r & 0x00FF);
            if r <= 0x0FFF { r -= 0x0600; }
            cf = (r > 0x0FFF) as i32;
            r = (a & 0xF000) + (d & 0xF000) + (cf << 12) + (r & 0x0FFF);
        }
        self.flag(V, !(a ^ d) & (a ^ r) & 0x8000 != 0);
        if self.p & D != 0 && r <= 0xFFFF { r -= 0x6000; }
        self.flag(C, r > 0xFFFF);
        self.nz16(r as u16);
        self.a = r as u16;
        r as u16
    }
    fn and8(&mut self, d: u8) -> u8 { let r = self.a as u8 & d; self.a = (self.a & 0xFF00) | r as u16; self.nz8(r); r }
    fn and16(&mut self, d: u16) -> u16 { self.a &= d; let r = self.a; self.nz16(r); r }
    fn ora8(&mut self, d: u8) -> u8 { let r = self.a as u8 | d; self.a = (self.a & 0xFF00) | r as u16; self.nz8(r); r }
    fn ora16(&mut self, d: u16) -> u16 { self.a |= d; let r = self.a; self.nz16(r); r }
    fn eor8(&mut self, d: u8) -> u8 { let r = self.a as u8 ^ d; self.a = (self.a & 0xFF00) | r as u16; self.nz8(r); r }
    fn eor16(&mut self, d: u16) -> u16 { self.a ^= d; let r = self.a; self.nz16(r); r }
    fn lda8(&mut self, d: u8) -> u8 { self.a = (self.a & 0xFF00) | d as u16; self.nz8(d); d }
    fn lda16(&mut self, d: u16) -> u16 { self.a = d; self.nz16(d); d }
    fn ldx8(&mut self, d: u8) -> u8 { self.x = d as u16; self.nz8(d); d }
    fn ldx16(&mut self, d: u16) -> u16 { self.x = d; self.nz16(d); d }
    fn ldy8(&mut self, d: u8) -> u8 { self.y = d as u16; self.nz8(d); d }
    fn ldy16(&mut self, d: u16) -> u16 { self.y = d; self.nz16(d); d }
    fn bit8(&mut self, d: u8) -> u8 { self.flag(Z, d & self.a as u8 == 0); self.flag(V, d & 0x40 != 0); self.flag(N, d & 0x80 != 0); d }
    fn bit16(&mut self, d: u16) -> u16 { self.flag(Z, d & self.a == 0); self.flag(V, d & 0x4000 != 0); self.flag(N, d & 0x8000 != 0); d }
    fn cmp_with8(&mut self, r: u8, d: u8) -> u8 { let x = (r as i32) - (d as i32); self.flag(C, x >= 0); self.nz8(x as u8); x as u8 }
    fn cmp_with16(&mut self, r: u16, d: u16) -> u16 { let x = (r as i32) - (d as i32); self.flag(C, x >= 0); self.nz16(x as u16); x as u16 }
    fn cmp8(&mut self, d: u8) -> u8 { self.cmp_with8(self.a as u8, d) }
    fn cmp16(&mut self, d: u16) -> u16 { self.cmp_with16(self.a, d) }
    fn cpx8(&mut self, d: u8) -> u8 { self.cmp_with8(self.x as u8, d) }
    fn cpx16(&mut self, d: u16) -> u16 { self.cmp_with16(self.x, d) }
    fn cpy8(&mut self, d: u8) -> u8 { self.cmp_with8(self.y as u8, d) }
    fn cpy16(&mut self, d: u16) -> u16 { self.cmp_with16(self.y, d) }
    fn inc8(&mut self, d: u8) -> u8 { let r = d.wrapping_add(1); self.nz8(r); r }
    fn inc16(&mut self, d: u16) -> u16 { let r = d.wrapping_add(1); self.nz16(r); r }
    fn dec8(&mut self, d: u8) -> u8 { let r = d.wrapping_sub(1); self.nz8(r); r }
    fn dec16(&mut self, d: u16) -> u16 { let r = d.wrapping_sub(1); self.nz16(r); r }
    fn asl8(&mut self, d: u8) -> u8 { self.flag(C, d & 0x80 != 0); let r = d << 1; self.nz8(r); r }
    fn asl16(&mut self, d: u16) -> u16 { self.flag(C, d & 0x8000 != 0); let r = d << 1; self.nz16(r); r }
    fn lsr8(&mut self, d: u8) -> u8 { self.flag(C, d & 1 != 0); let r = d >> 1; self.nz8(r); r }
    fn lsr16(&mut self, d: u16) -> u16 { self.flag(C, d & 1 != 0); let r = d >> 1; self.nz16(r); r }
    fn rol8(&mut self, d: u8) -> u8 { let c = self.p & C; self.flag(C, d & 0x80 != 0); let r = d << 1 | c; self.nz8(r); r }
    fn rol16(&mut self, d: u16) -> u16 { let c = (self.p & C) as u16; self.flag(C, d & 0x8000 != 0); let r = d << 1 | c; self.nz16(r); r }
    fn ror8(&mut self, d: u8) -> u8 { let c = self.p & C; self.flag(C, d & 1 != 0); let r = c << 7 | d >> 1; self.nz8(r); r }
    fn ror16(&mut self, d: u16) -> u16 { let c = (self.p & C) as u16; self.flag(C, d & 1 != 0); let r = c << 15 | d >> 1; self.nz16(r); r }
    fn tsb8(&mut self, d: u8) -> u8 { self.flag(Z, d & self.a as u8 == 0); d | self.a as u8 }
    fn tsb16(&mut self, d: u16) -> u16 { self.flag(Z, d & self.a == 0); d | self.a }
    fn trb8(&mut self, d: u8) -> u8 { self.flag(Z, d & self.a as u8 == 0); d & !(self.a as u8) }
    fn trb16(&mut self, d: u16) -> u16 { self.flag(Z, d & self.a == 0); d & !self.a }

    // ---------------- addressing: reads ----------------

    fn imm_r8(&mut self, bus: &mut dyn Bus816, op: Alu8) { let w = self.fetch(bus); op(self, w); }
    fn imm_r16(&mut self, bus: &mut dyn Bus816, op: Alu16) { let w = self.fetch16(bus); op(self, w); }
    fn bank_r8(&mut self, bus: &mut dyn Bus816, op: Alu8, i: Option<Reg>) {
        let v = self.fetch16(bus) as u32;
        let iw = i.map(|r| self.get(r)).unwrap_or(0) as u32;
        if i.is_some() { self.idle4(bus, v as u16, (v + iw) as u16); }
        let w = self.read_bank(bus, v + iw);
        op(self, w);
    }
    fn bank_r16(&mut self, bus: &mut dyn Bus816, op: Alu16, i: Option<Reg>) {
        let v = self.fetch16(bus) as u32;
        let iw = i.map(|r| self.get(r)).unwrap_or(0) as u32;
        if i.is_some() { self.idle4(bus, v as u16, (v + iw) as u16); }
        let l = self.read_bank(bus, v + iw) as u16;
        let h = self.read_bank(bus, v + iw + 1) as u16;
        op(self, l | h << 8);
    }
    fn long_addr(&mut self, bus: &mut dyn Bus816) -> u32 { let l = self.fetch16(bus) as u32; l | (self.fetch(bus) as u32) << 16 }
    fn long_r8(&mut self, bus: &mut dyn Bus816, op: Alu8, i: Option<Reg>) {
        let v = self.long_addr(bus);
        let iw = i.map(|r| self.get(r)).unwrap_or(0) as u32;
        let w = bus.read((v + iw) & 0xFFFFFF);
        op(self, w);
    }
    fn long_r16(&mut self, bus: &mut dyn Bus816, op: Alu16, i: Option<Reg>) {
        let v = self.long_addr(bus);
        let iw = i.map(|r| self.get(r)).unwrap_or(0) as u32;
        let l = bus.read((v + iw) & 0xFFFFFF) as u16;
        let h = bus.read((v + iw + 1) & 0xFFFFFF) as u16;
        op(self, l | h << 8);
    }
    fn direct_r8(&mut self, bus: &mut dyn Bus816, op: Alu8, i: Option<Reg>) {
        let u = self.fetch(bus) as u32;
        self.idle2(bus);
        let iw = match i { Some(r) => { bus.idle(); self.get(r) as u32 } None => 0 };
        let w = self.read_direct(bus, u + iw);
        op(self, w);
    }
    fn direct_r16(&mut self, bus: &mut dyn Bus816, op: Alu16, i: Option<Reg>) {
        let u = self.fetch(bus) as u32;
        self.idle2(bus);
        let iw = match i { Some(r) => { bus.idle(); self.get(r) as u32 } None => 0 };
        let l = self.read_direct(bus, u + iw) as u16;
        let h = self.read_direct(bus, u + iw + 1) as u16;
        op(self, l | h << 8);
    }
    fn indirect_ptr(&mut self, bus: &mut dyn Bus816) -> u32 {
        let u = self.fetch(bus) as u32;
        self.idle2(bus);
        let l = self.read_direct(bus, u) as u32;
        let h = self.read_direct(bus, u + 1) as u32;
        l | h << 8
    }
    fn indirect_r8(&mut self, bus: &mut dyn Bus816, op: Alu8) { let v = self.indirect_ptr(bus); let w = self.read_bank(bus, v); op(self, w); }
    fn indirect_r16(&mut self, bus: &mut dyn Bus816, op: Alu16) {
        let v = self.indirect_ptr(bus);
        let l = self.read_bank(bus, v) as u16; let h = self.read_bank(bus, v + 1) as u16;
        op(self, l | h << 8);
    }
    fn indexed_indirect_ptr(&mut self, bus: &mut dyn Bus816) -> u32 {
        let u = self.fetch(bus) as u32;
        self.idle2(bus);
        bus.idle();
        let a = u + self.x as u32;
        let l = self.read_direct_x(bus, a, 0) as u32;
        let h = self.read_direct_x(bus, a, 1) as u32;
        l | h << 8
    }
    fn indexed_indirect_r8(&mut self, bus: &mut dyn Bus816, op: Alu8) { let v = self.indexed_indirect_ptr(bus); let w = self.read_bank(bus, v); op(self, w); }
    fn indexed_indirect_r16(&mut self, bus: &mut dyn Bus816, op: Alu16) {
        let v = self.indexed_indirect_ptr(bus);
        let l = self.read_bank(bus, v) as u16; let h = self.read_bank(bus, v + 1) as u16;
        op(self, l | h << 8);
    }
    fn indirect_indexed_r8(&mut self, bus: &mut dyn Bus816, op: Alu8) {
        let v = self.indirect_ptr(bus);
        self.idle4(bus, v as u16, (v + self.y as u32) as u16);
        let w = self.read_bank(bus, v + self.y as u32);
        op(self, w);
    }
    fn indirect_indexed_r16(&mut self, bus: &mut dyn Bus816, op: Alu16) {
        let v = self.indirect_ptr(bus);
        self.idle4(bus, v as u16, (v + self.y as u32) as u16);
        let a = v + self.y as u32;
        let l = self.read_bank(bus, a) as u16; let h = self.read_bank(bus, a + 1) as u16;
        op(self, l | h << 8);
    }
    fn indirect_long_ptr(&mut self, bus: &mut dyn Bus816) -> u32 {
        let u = self.fetch(bus) as u32;
        self.idle2(bus);
        let l = self.read_direct_n(bus, u) as u32;
        let h = self.read_direct_n(bus, u + 1) as u32;
        let b = self.read_direct_n(bus, u + 2) as u32;
        l | h << 8 | b << 16
    }
    fn indirect_long_r8(&mut self, bus: &mut dyn Bus816, op: Alu8, i: Option<Reg>) {
        let v = self.indirect_long_ptr(bus);
        let iw = i.map(|r| self.get(r)).unwrap_or(0) as u32;
        let w = bus.read((v + iw) & 0xFFFFFF);
        op(self, w);
    }
    fn indirect_long_r16(&mut self, bus: &mut dyn Bus816, op: Alu16, i: Option<Reg>) {
        let v = self.indirect_long_ptr(bus);
        let iw = i.map(|r| self.get(r)).unwrap_or(0) as u32;
        let l = bus.read((v + iw) & 0xFFFFFF) as u16;
        let h = bus.read((v + iw + 1) & 0xFFFFFF) as u16;
        op(self, l | h << 8);
    }
    fn stack_r8(&mut self, bus: &mut dyn Bus816, op: Alu8) {
        let u = self.fetch(bus) as u32; bus.idle();
        let w = self.read_stack(bus, u); op(self, w);
    }
    fn stack_r16(&mut self, bus: &mut dyn Bus816, op: Alu16) {
        let u = self.fetch(bus) as u32; bus.idle();
        let l = self.read_stack(bus, u) as u16; let h = self.read_stack(bus, u + 1) as u16;
        op(self, l | h << 8);
    }
    fn indirect_stack_ptr(&mut self, bus: &mut dyn Bus816) -> u32 {
        let u = self.fetch(bus) as u32; bus.idle();
        let l = self.read_stack(bus, u) as u32; let h = self.read_stack(bus, u + 1) as u32;
        bus.idle();
        (l | h << 8) + self.y as u32
    }
    fn indirect_stack_r8(&mut self, bus: &mut dyn Bus816, op: Alu8) { let a = self.indirect_stack_ptr(bus); let w = self.read_bank(bus, a); op(self, w); }
    fn indirect_stack_r16(&mut self, bus: &mut dyn Bus816, op: Alu16) {
        let a = self.indirect_stack_ptr(bus);
        let l = self.read_bank(bus, a) as u16; let h = self.read_bank(bus, a + 1) as u16;
        op(self, l | h << 8);
    }

    // ---------------- addressing: writes ----------------

    fn bank_w(&mut self, bus: &mut dyn Bus816, f: Reg, i: Option<Reg>, wide: bool) {
        let v = self.fetch16(bus) as u32;
        let iw = match i { Some(r) => { bus.idle(); self.get(r) as u32 } None => 0 };
        let val = self.get(f);
        self.write_bank(bus, v + iw, val as u8);
        if wide { self.write_bank(bus, v + iw + 1, (val >> 8) as u8); }
    }
    fn long_w(&mut self, bus: &mut dyn Bus816, i: Option<Reg>, wide: bool) {
        let v = self.long_addr(bus);
        let iw = i.map(|r| self.get(r)).unwrap_or(0) as u32;
        bus.write((v + iw) & 0xFFFFFF, self.a as u8);
        if wide { bus.write((v + iw + 1) & 0xFFFFFF, (self.a >> 8) as u8); }
    }
    fn direct_w(&mut self, bus: &mut dyn Bus816, f: Reg, i: Option<Reg>, wide: bool) {
        let u = self.fetch(bus) as u32;
        self.idle2(bus);
        let iw = match i { Some(r) => { bus.idle(); self.get(r) as u32 } None => 0 };
        let val = self.get(f);
        self.write_direct(bus, u + iw, val as u8);
        if wide { self.write_direct(bus, u + iw + 1, (val >> 8) as u8); }
    }
    fn indirect_w(&mut self, bus: &mut dyn Bus816, wide: bool) {
        let v = self.indirect_ptr(bus);
        self.write_bank(bus, v, self.a as u8);
        if wide { self.write_bank(bus, v + 1, (self.a >> 8) as u8); }
    }
    fn indexed_indirect_w(&mut self, bus: &mut dyn Bus816, wide: bool) {
        let v = self.indexed_indirect_ptr(bus);
        self.write_bank(bus, v, self.a as u8);
        if wide { self.write_bank(bus, v + 1, (self.a >> 8) as u8); }
    }
    fn indirect_indexed_w(&mut self, bus: &mut dyn Bus816, wide: bool) {
        let v = self.indirect_ptr(bus);
        bus.idle();
        let a = v + self.y as u32;
        self.write_bank(bus, a, self.a as u8);
        if wide { self.write_bank(bus, a + 1, (self.a >> 8) as u8); }
    }
    fn indirect_long_w(&mut self, bus: &mut dyn Bus816, i: Option<Reg>, wide: bool) {
        let v = self.indirect_long_ptr(bus);
        let iw = i.map(|r| self.get(r)).unwrap_or(0) as u32;
        bus.write((v + iw) & 0xFFFFFF, self.a as u8);
        if wide { bus.write((v + iw + 1) & 0xFFFFFF, (self.a >> 8) as u8); }
    }
    fn stack_w(&mut self, bus: &mut dyn Bus816, wide: bool) {
        let u = self.fetch(bus) as u32; bus.idle();
        self.write_stack(bus, u, self.a as u8);
        if wide { self.write_stack(bus, u + 1, (self.a >> 8) as u8); }
    }
    fn indirect_stack_w(&mut self, bus: &mut dyn Bus816, wide: bool) {
        let a = self.indirect_stack_ptr(bus);
        self.write_bank(bus, a, self.a as u8);
        if wide { self.write_bank(bus, a + 1, (self.a >> 8) as u8); }
    }

    // ---------------- read-modify-write ----------------

    fn implied_m8(&mut self, bus: &mut dyn Bus816, op: Alu8, r: Reg) {
        self.idle_irq(bus);
        let v = op(self, self.get(r) as u8);
        self.set_lo(r, v);
    }
    fn implied_m16(&mut self, bus: &mut dyn Bus816, op: Alu16, r: Reg) {
        self.idle_irq(bus);
        let v = op(self, self.get(r));
        self.set_w(r, v);
    }
    fn bank_m8(&mut self, bus: &mut dyn Bus816, op: Alu8, indexed: bool) {
        let v = self.fetch16(bus) as u32;
        if indexed { bus.idle(); }
        let a = v + if indexed { self.x as u32 } else { 0 };
        let w = self.read_bank(bus, a);
        bus.idle();
        let w = op(self, w);
        self.write_bank(bus, a, w);
    }
    fn bank_m16(&mut self, bus: &mut dyn Bus816, op: Alu16, indexed: bool) {
        let v = self.fetch16(bus) as u32;
        if indexed { bus.idle(); }
        let a = v + if indexed { self.x as u32 } else { 0 };
        let l = self.read_bank(bus, a) as u16; let h = self.read_bank(bus, a + 1) as u16;
        bus.idle();
        let w = op(self, l | h << 8);
        self.write_bank(bus, a + 1, (w >> 8) as u8);
        self.write_bank(bus, a, w as u8);
    }
    fn direct_m8(&mut self, bus: &mut dyn Bus816, op: Alu8, indexed: bool) {
        let u = self.fetch(bus) as u32;
        self.idle2(bus);
        if indexed { bus.idle(); }
        let a = u + if indexed { self.x as u32 } else { 0 };
        let w = self.read_direct(bus, a);
        bus.idle();
        let w = op(self, w);
        self.write_direct(bus, a, w);
    }
    fn direct_m16(&mut self, bus: &mut dyn Bus816, op: Alu16, indexed: bool) {
        let u = self.fetch(bus) as u32;
        self.idle2(bus);
        if indexed { bus.idle(); }
        let a = u + if indexed { self.x as u32 } else { 0 };
        let l = self.read_direct(bus, a) as u16; let h = self.read_direct(bus, a + 1) as u16;
        bus.idle();
        let w = op(self, l | h << 8);
        self.write_direct(bus, a + 1, (w >> 8) as u8);
        self.write_direct(bus, a, w as u8);
    }

    // ---------------- flow ----------------

    fn branch(&mut self, bus: &mut dyn Bus816, take: bool) {
        if !take { self.fetch(bus); return; }
        let u = self.fetch(bus) as i8;
        let v = self.pcw().wrapping_add(u as u16);
        self.idle6(bus, v);
        bus.idle();
        self.set_pcw(v);
    }

    fn interrupt_seq(&mut self, bus: &mut dyn Bus816, vector: u16, brk: bool) {
        if brk { self.fetch(bus); } else { bus.read(self.pc); bus.idle(); }
        if !self.e { let b = (self.pc >> 16) as u8; self.push(bus, b); }
        let pc = self.pcw();
        self.push(bus, (pc >> 8) as u8);
        self.push(bus, pc as u8);
        let p = if self.e && !brk { self.p & !0x10 } else { self.p };
        self.push(bus, p);
        self.p |= I;
        self.p &= !D;
        let l = bus.read(vector as u32) as u32;
        let h = bus.read(vector as u32 + 1) as u32;
        self.pc = l | h << 8;
    }

    /// Take an interrupt: NMI (vector FFEA / FFFA) or IRQ (FFEE / FFFE). The caller
    /// decides whether IRQs are masked (`p & I`).
    pub fn interrupt(&mut self, bus: &mut dyn Bus816, nmi: bool) {
        self.wai = false;
        let v = match (nmi, self.e) { (true, false) => 0xFFEA, (true, true) => 0xFFFA, (false, false) => 0xFFEE, (false, true) => 0xFFFE };
        self.interrupt_seq(bus, v, false);
    }

    /// One instruction (WAI / STP: one idle cycle).
    pub fn step(&mut self, bus: &mut dyn Bus816) {
        if self.stp || self.wai { bus.idle(); return; }
        let op = self.fetch(bus);
        self.exec(bus, op);
    }

    fn exec(&mut self, bus: &mut dyn Bus816, op: u8) {
        let m8 = self.mf();
        let x8 = self.xf();
        // read-type instructions with the A-width ALU: (op, mode)
        macro_rules! rm {
            ($f8:ident, $f16:ident, $mode:expr) => {{
                let (o8, o16): (Alu8, Alu16) = (Self::$f8, Self::$f16);
                self.read_mode(bus, $mode, m8, o8, o16)
            }};
        }
        macro_rules! rx {
            ($f8:ident, $f16:ident, $mode:expr) => {{
                let (o8, o16): (Alu8, Alu16) = (Self::$f8, Self::$f16);
                self.read_mode(bus, $mode, x8, o8, o16)
            }};
        }
        macro_rules! mm {
            ($f8:ident, $f16:ident, $kind:expr) => {{
                let (o8, o16): (Alu8, Alu16) = (Self::$f8, Self::$f16);
                match ($kind, m8) {
                    (0, true) => self.direct_m8(bus, o8, false), (0, false) => self.direct_m16(bus, o16, false),
                    (1, true) => self.direct_m8(bus, o8, true), (1, false) => self.direct_m16(bus, o16, true),
                    (2, true) => self.bank_m8(bus, o8, false), (2, false) => self.bank_m16(bus, o16, false),
                    (3, true) => self.bank_m8(bus, o8, true), (_, _) => self.bank_m16(bus, o16, true),
                }
            }};
        }
        macro_rules! imp {
            ($f8:ident, $f16:ident, $r:expr, $w8:expr) => {{
                if $w8 { self.implied_m8(bus, Self::$f8, $r) } else { self.implied_m16(bus, Self::$f16, $r) }
            }};
        }
        match op {
            0x00 => { let v = if self.e { 0xFFFE } else { 0xFFE6 }; self.interrupt_seq(bus, v, true); }
            0x02 => { let v = if self.e { 0xFFF4 } else { 0xFFE4 }; self.interrupt_seq(bus, v, true); }
            0x01 => rm!(ora8, ora16, Am::IdxInd), 0x03 => rm!(ora8, ora16, Am::Stack), 0x05 => rm!(ora8, ora16, Am::Dp),
            0x07 => rm!(ora8, ora16, Am::IndLong), 0x09 => rm!(ora8, ora16, Am::Imm), 0x0D => rm!(ora8, ora16, Am::Abs),
            0x0F => rm!(ora8, ora16, Am::Long), 0x11 => rm!(ora8, ora16, Am::IndIdx), 0x12 => rm!(ora8, ora16, Am::Ind),
            0x13 => rm!(ora8, ora16, Am::IndStack), 0x15 => rm!(ora8, ora16, Am::DpX), 0x17 => rm!(ora8, ora16, Am::IndLongY),
            0x19 => rm!(ora8, ora16, Am::AbsY), 0x1D => rm!(ora8, ora16, Am::AbsX), 0x1F => rm!(ora8, ora16, Am::LongX),
            0x21 => rm!(and8, and16, Am::IdxInd), 0x23 => rm!(and8, and16, Am::Stack), 0x25 => rm!(and8, and16, Am::Dp),
            0x27 => rm!(and8, and16, Am::IndLong), 0x29 => rm!(and8, and16, Am::Imm), 0x2D => rm!(and8, and16, Am::Abs),
            0x2F => rm!(and8, and16, Am::Long), 0x31 => rm!(and8, and16, Am::IndIdx), 0x32 => rm!(and8, and16, Am::Ind),
            0x33 => rm!(and8, and16, Am::IndStack), 0x35 => rm!(and8, and16, Am::DpX), 0x37 => rm!(and8, and16, Am::IndLongY),
            0x39 => rm!(and8, and16, Am::AbsY), 0x3D => rm!(and8, and16, Am::AbsX), 0x3F => rm!(and8, and16, Am::LongX),
            0x41 => rm!(eor8, eor16, Am::IdxInd), 0x43 => rm!(eor8, eor16, Am::Stack), 0x45 => rm!(eor8, eor16, Am::Dp),
            0x47 => rm!(eor8, eor16, Am::IndLong), 0x49 => rm!(eor8, eor16, Am::Imm), 0x4D => rm!(eor8, eor16, Am::Abs),
            0x4F => rm!(eor8, eor16, Am::Long), 0x51 => rm!(eor8, eor16, Am::IndIdx), 0x52 => rm!(eor8, eor16, Am::Ind),
            0x53 => rm!(eor8, eor16, Am::IndStack), 0x55 => rm!(eor8, eor16, Am::DpX), 0x57 => rm!(eor8, eor16, Am::IndLongY),
            0x59 => rm!(eor8, eor16, Am::AbsY), 0x5D => rm!(eor8, eor16, Am::AbsX), 0x5F => rm!(eor8, eor16, Am::LongX),
            0x61 => rm!(adc8, adc16, Am::IdxInd), 0x63 => rm!(adc8, adc16, Am::Stack), 0x65 => rm!(adc8, adc16, Am::Dp),
            0x67 => rm!(adc8, adc16, Am::IndLong), 0x69 => rm!(adc8, adc16, Am::Imm), 0x6D => rm!(adc8, adc16, Am::Abs),
            0x6F => rm!(adc8, adc16, Am::Long), 0x71 => rm!(adc8, adc16, Am::IndIdx), 0x72 => rm!(adc8, adc16, Am::Ind),
            0x73 => rm!(adc8, adc16, Am::IndStack), 0x75 => rm!(adc8, adc16, Am::DpX), 0x77 => rm!(adc8, adc16, Am::IndLongY),
            0x79 => rm!(adc8, adc16, Am::AbsY), 0x7D => rm!(adc8, adc16, Am::AbsX), 0x7F => rm!(adc8, adc16, Am::LongX),
            0xA1 => rm!(lda8, lda16, Am::IdxInd), 0xA3 => rm!(lda8, lda16, Am::Stack), 0xA5 => rm!(lda8, lda16, Am::Dp),
            0xA7 => rm!(lda8, lda16, Am::IndLong), 0xA9 => rm!(lda8, lda16, Am::Imm), 0xAD => rm!(lda8, lda16, Am::Abs),
            0xAF => rm!(lda8, lda16, Am::Long), 0xB1 => rm!(lda8, lda16, Am::IndIdx), 0xB2 => rm!(lda8, lda16, Am::Ind),
            0xB3 => rm!(lda8, lda16, Am::IndStack), 0xB5 => rm!(lda8, lda16, Am::DpX), 0xB7 => rm!(lda8, lda16, Am::IndLongY),
            0xB9 => rm!(lda8, lda16, Am::AbsY), 0xBD => rm!(lda8, lda16, Am::AbsX), 0xBF => rm!(lda8, lda16, Am::LongX),
            0xC1 => rm!(cmp8, cmp16, Am::IdxInd), 0xC3 => rm!(cmp8, cmp16, Am::Stack), 0xC5 => rm!(cmp8, cmp16, Am::Dp),
            0xC7 => rm!(cmp8, cmp16, Am::IndLong), 0xC9 => rm!(cmp8, cmp16, Am::Imm), 0xCD => rm!(cmp8, cmp16, Am::Abs),
            0xCF => rm!(cmp8, cmp16, Am::Long), 0xD1 => rm!(cmp8, cmp16, Am::IndIdx), 0xD2 => rm!(cmp8, cmp16, Am::Ind),
            0xD3 => rm!(cmp8, cmp16, Am::IndStack), 0xD5 => rm!(cmp8, cmp16, Am::DpX), 0xD7 => rm!(cmp8, cmp16, Am::IndLongY),
            0xD9 => rm!(cmp8, cmp16, Am::AbsY), 0xDD => rm!(cmp8, cmp16, Am::AbsX), 0xDF => rm!(cmp8, cmp16, Am::LongX),
            0xE1 => rm!(sbc8, sbc16, Am::IdxInd), 0xE3 => rm!(sbc8, sbc16, Am::Stack), 0xE5 => rm!(sbc8, sbc16, Am::Dp),
            0xE7 => rm!(sbc8, sbc16, Am::IndLong), 0xE9 => rm!(sbc8, sbc16, Am::Imm), 0xED => rm!(sbc8, sbc16, Am::Abs),
            0xEF => rm!(sbc8, sbc16, Am::Long), 0xF1 => rm!(sbc8, sbc16, Am::IndIdx), 0xF2 => rm!(sbc8, sbc16, Am::Ind),
            0xF3 => rm!(sbc8, sbc16, Am::IndStack), 0xF5 => rm!(sbc8, sbc16, Am::DpX), 0xF7 => rm!(sbc8, sbc16, Am::IndLongY),
            0xF9 => rm!(sbc8, sbc16, Am::AbsY), 0xFD => rm!(sbc8, sbc16, Am::AbsX), 0xFF => rm!(sbc8, sbc16, Am::LongX),
            0x24 => rm!(bit8, bit16, Am::Dp), 0x2C => rm!(bit8, bit16, Am::Abs),
            0x34 => rm!(bit8, bit16, Am::DpX), 0x3C => rm!(bit8, bit16, Am::AbsX),
            0x89 => {
                if m8 { let u = self.fetch(bus); self.flag(Z, u & self.a as u8 == 0); }
                else { let u = self.fetch16(bus); self.flag(Z, u & self.a == 0); }
            }
            0xA0 => rx!(ldy8, ldy16, Am::Imm), 0xA4 => rx!(ldy8, ldy16, Am::Dp), 0xAC => rx!(ldy8, ldy16, Am::Abs),
            0xB4 => rx!(ldy8, ldy16, Am::DpX), 0xBC => rx!(ldy8, ldy16, Am::AbsX),
            0xA2 => rx!(ldx8, ldx16, Am::Imm), 0xA6 => rx!(ldx8, ldx16, Am::Dp), 0xAE => rx!(ldx8, ldx16, Am::Abs),
            0xB6 => rx!(ldx8, ldx16, Am::DpY), 0xBE => rx!(ldx8, ldx16, Am::AbsY),
            0xC0 => rx!(cpy8, cpy16, Am::Imm), 0xC4 => rx!(cpy8, cpy16, Am::Dp), 0xCC => rx!(cpy8, cpy16, Am::Abs),
            0xE0 => rx!(cpx8, cpx16, Am::Imm), 0xE4 => rx!(cpx8, cpx16, Am::Dp), 0xEC => rx!(cpx8, cpx16, Am::Abs),
            // stores
            0x81 => self.indexed_indirect_w(bus, !m8),
            0x83 => self.stack_w(bus, !m8),
            0x84 => self.direct_w(bus, Reg::Y, None, !x8),
            0x85 => self.direct_w(bus, Reg::A, None, !m8),
            0x86 => self.direct_w(bus, Reg::X, None, !x8),
            0x87 => self.indirect_long_w(bus, None, !m8),
            0x8C => self.bank_w(bus, Reg::Y, None, !x8),
            0x8D => self.bank_w(bus, Reg::A, None, !m8),
            0x8E => self.bank_w(bus, Reg::X, None, !x8),
            0x8F => self.long_w(bus, None, !m8),
            0x91 => self.indirect_indexed_w(bus, !m8),
            0x92 => self.indirect_w(bus, !m8),
            0x93 => self.indirect_stack_w(bus, !m8),
            0x94 => self.direct_w(bus, Reg::Y, Some(Reg::X), !x8),
            0x95 => self.direct_w(bus, Reg::A, Some(Reg::X), !m8),
            0x96 => self.direct_w(bus, Reg::X, Some(Reg::Y), !x8),
            0x97 => self.indirect_long_w(bus, Some(Reg::Y), !m8),
            0x99 => self.bank_w(bus, Reg::A, Some(Reg::Y), !m8),
            0x9D => self.bank_w(bus, Reg::A, Some(Reg::X), !m8),
            0x9F => self.long_w(bus, Some(Reg::X), !m8),
            0x64 => self.direct_w(bus, Reg::Z, None, !m8),
            0x74 => self.direct_w(bus, Reg::Z, Some(Reg::X), !m8),
            0x9C => self.bank_w(bus, Reg::Z, None, !m8),
            0x9E => self.bank_w(bus, Reg::Z, Some(Reg::X), !m8),
            // read-modify-write
            0x04 => mm!(tsb8, tsb16, 0), 0x0C => mm!(tsb8, tsb16, 2),
            0x14 => mm!(trb8, trb16, 0), 0x1C => mm!(trb8, trb16, 2),
            0x06 => mm!(asl8, asl16, 0), 0x0E => mm!(asl8, asl16, 2), 0x16 => mm!(asl8, asl16, 1), 0x1E => mm!(asl8, asl16, 3),
            0x26 => mm!(rol8, rol16, 0), 0x2E => mm!(rol8, rol16, 2), 0x36 => mm!(rol8, rol16, 1), 0x3E => mm!(rol8, rol16, 3),
            0x46 => mm!(lsr8, lsr16, 0), 0x4E => mm!(lsr8, lsr16, 2), 0x56 => mm!(lsr8, lsr16, 1), 0x5E => mm!(lsr8, lsr16, 3),
            0x66 => mm!(ror8, ror16, 0), 0x6E => mm!(ror8, ror16, 2), 0x76 => mm!(ror8, ror16, 1), 0x7E => mm!(ror8, ror16, 3),
            0xC6 => mm!(dec8, dec16, 0), 0xCE => mm!(dec8, dec16, 2), 0xD6 => mm!(dec8, dec16, 1), 0xDE => mm!(dec8, dec16, 3),
            0xE6 => mm!(inc8, inc16, 0), 0xEE => mm!(inc8, inc16, 2), 0xF6 => mm!(inc8, inc16, 1), 0xFE => mm!(inc8, inc16, 3),
            0x0A => imp!(asl8, asl16, Reg::A, m8), 0x2A => imp!(rol8, rol16, Reg::A, m8),
            0x4A => imp!(lsr8, lsr16, Reg::A, m8), 0x6A => imp!(ror8, ror16, Reg::A, m8),
            0x1A => imp!(inc8, inc16, Reg::A, m8), 0x3A => imp!(dec8, dec16, Reg::A, m8),
            0xE8 => imp!(inc8, inc16, Reg::X, x8), 0xC8 => imp!(inc8, inc16, Reg::Y, x8),
            0xCA => imp!(dec8, dec16, Reg::X, x8), 0x88 => imp!(dec8, dec16, Reg::Y, x8),
            // branches and jumps
            0x10 => { let t = self.p & N == 0; self.branch(bus, t) }
            0x30 => { let t = self.p & N != 0; self.branch(bus, t) }
            0x50 => { let t = self.p & V == 0; self.branch(bus, t) }
            0x70 => { let t = self.p & V != 0; self.branch(bus, t) }
            0x90 => { let t = self.p & C == 0; self.branch(bus, t) }
            0xB0 => { let t = self.p & C != 0; self.branch(bus, t) }
            0xD0 => { let t = self.p & Z == 0; self.branch(bus, t) }
            0xF0 => { let t = self.p & Z != 0; self.branch(bus, t) }
            0x80 => self.branch(bus, true),
            0x82 => {
                let u = self.fetch16(bus);
                let v = self.pcw().wrapping_add(u);
                bus.idle();
                self.set_pcw(v);
            }
            0x4C => { let w = self.fetch16(bus); self.set_pcw(w); }
            0x5C => { let v = self.long_addr(bus); self.pc = v; }
            0x6C => {
                let v = self.fetch16(bus);
                let l = bus.read(v as u32) as u16; let h = bus.read(v.wrapping_add(1) as u32) as u16;
                self.set_pcw(l | h << 8);
            }
            0x7C => {
                let v = self.fetch16(bus);
                bus.idle();
                let a = v.wrapping_add(self.x);
                let l = bus.read(self.pcb() | a as u32) as u16;
                let h = bus.read(self.pcb() | a.wrapping_add(1) as u32) as u16;
                self.set_pcw(l | h << 8);
            }
            0xDC => {
                let u = self.fetch16(bus);
                let l = bus.read(u as u32) as u32;
                let h = bus.read(u.wrapping_add(1) as u32) as u32;
                let b = bus.read(u.wrapping_add(2) as u32) as u32;
                self.pc = l | h << 8 | b << 16;
            }
            0x20 => {
                let w = self.fetch16(bus);
                bus.idle();
                let pc = self.pcw().wrapping_sub(1);
                self.push(bus, (pc >> 8) as u8);
                self.push(bus, pc as u8);
                self.set_pcw(w);
            }
            0x22 => {
                let lo = self.fetch16(bus) as u32;
                let pb = (self.pc >> 16) as u8;
                self.push_n(bus, pb);
                bus.idle();
                let b = self.fetch(bus) as u32;
                let pc = self.pcw().wrapping_sub(1);
                self.push_n(bus, (pc >> 8) as u8);
                self.push_n(bus, pc as u8);
                self.pc = lo | b << 16;
                self.fix_s();
            }
            0xFC => {
                let l = self.fetch(bus) as u16;
                let pc = self.pcw();
                self.push_n(bus, (pc >> 8) as u8);
                self.push_n(bus, pc as u8);
                let h = self.fetch(bus) as u16;
                bus.idle();
                let v = l | h << 8;
                let a = v.wrapping_add(self.x);
                let wl = bus.read(self.pcb() | a as u32) as u16;
                let wh = bus.read(self.pcb() | a.wrapping_add(1) as u32) as u16;
                self.set_pcw(wl | wh << 8);
                self.fix_s();
            }
            0x40 => {
                bus.idle(); bus.idle();
                let p = self.pull(bus);
                self.set_p(p);
                let l = self.pull(bus) as u32;
                let h = self.pull(bus) as u32;
                if self.e { self.pc = self.pcb() | h << 8 | l; }
                else { let b = self.pull(bus) as u32; self.pc = b << 16 | h << 8 | l; }
            }
            0x60 => {
                bus.idle(); bus.idle();
                let l = self.pull(bus) as u16; let h = self.pull(bus) as u16;
                bus.idle();
                self.set_pcw((l | h << 8).wrapping_add(1));
            }
            0x6B => {
                bus.idle(); bus.idle();
                let l = self.pull_n(bus) as u32; let h = self.pull_n(bus) as u32; let b = self.pull_n(bus) as u32;
                self.pc = b << 16 | ((((h << 8) | l) as u16).wrapping_add(1)) as u32;
                self.fix_s();
            }
            // stack
            0x08 => { bus.idle(); let p = self.p; self.push(bus, p); }
            0x48 => { bus.idle(); if !m8 { let h = (self.a >> 8) as u8; self.push(bus, h); } let l = self.a as u8; self.push(bus, l); }
            0xDA => { bus.idle(); if !x8 { let h = (self.x >> 8) as u8; self.push(bus, h); } let l = self.x as u8; self.push(bus, l); }
            0x5A => { bus.idle(); if !x8 { let h = (self.y >> 8) as u8; self.push(bus, h); } let l = self.y as u8; self.push(bus, l); }
            0x4B => { bus.idle(); let b = (self.pc >> 16) as u8; self.push(bus, b); }
            0x8B => { bus.idle(); let b = self.b; self.push(bus, b); }
            0x0B => { bus.idle(); let d = self.d; self.push_n(bus, (d >> 8) as u8); self.push_n(bus, d as u8); self.fix_s(); }
            0x28 => { bus.idle(); bus.idle(); let p = self.pull(bus); self.set_p(p); }
            0x68 => self.pull_reg(bus, Reg::A, m8),
            0xFA => self.pull_reg(bus, Reg::X, x8),
            0x7A => self.pull_reg(bus, Reg::Y, x8),
            0x2B => {
                bus.idle(); bus.idle();
                let l = self.pull_n(bus) as u16; let h = self.pull_n(bus) as u16;
                self.d = l | h << 8;
                let d = self.d; self.nz16(d);
                self.fix_s();
            }
            0xAB => {
                bus.idle(); bus.idle();
                self.b = self.pull_n(bus);
                let b = self.b; self.nz8(b);
                self.fix_s();
            }
            0xF4 => { let w = self.fetch16(bus); self.push_n(bus, (w >> 8) as u8); self.push_n(bus, w as u8); self.fix_s(); }
            0xD4 => {
                let u = self.fetch(bus) as u32;
                self.idle2(bus);
                let l = self.read_direct_n(bus, u) as u16; let h = self.read_direct_n(bus, u + 1) as u16;
                let w = l | h << 8;
                self.push_n(bus, (w >> 8) as u8); self.push_n(bus, w as u8);
                self.fix_s();
            }
            0x62 => {
                let v = self.fetch16(bus);
                bus.idle();
                let w = self.pcw().wrapping_add(v);
                self.push_n(bus, (w >> 8) as u8); self.push_n(bus, w as u8);
                self.fix_s();
            }
            // flags and transfers
            0x18 => { self.idle_irq(bus); self.p &= !C; }
            0x38 => { self.idle_irq(bus); self.p |= C; }
            0x58 => { self.idle_irq(bus); self.p &= !I; }
            0x78 => { self.idle_irq(bus); self.p |= I; }
            0xB8 => { self.idle_irq(bus); self.p &= !V; }
            0xD8 => { self.idle_irq(bus); self.p &= !D; }
            0xF8 => { self.idle_irq(bus); self.p |= D; }
            0xC2 => { let w = self.fetch(bus); bus.idle(); let p = self.p & !w; self.set_p(p); }
            0xE2 => { let w = self.fetch(bus); bus.idle(); let p = self.p | w; self.set_p(p); }
            0xFB => {
                self.idle_irq(bus);
                let c = self.p & C != 0;
                self.flag(C, self.e);
                self.e = c;
                if self.e { self.p |= X | M; self.x &= 0xFF; self.y &= 0xFF; self.s = 0x0100 | (self.s & 0xFF); }
            }
            0xAA => self.transfer(bus, Reg::A, Reg::X, x8),
            0xA8 => self.transfer(bus, Reg::A, Reg::Y, x8),
            0x8A => self.transfer(bus, Reg::X, Reg::A, m8),
            0x98 => self.transfer(bus, Reg::Y, Reg::A, m8),
            0x9B => self.transfer(bus, Reg::X, Reg::Y, x8),
            0xBB => self.transfer(bus, Reg::Y, Reg::X, x8),
            0x3B => self.transfer(bus, Reg::S, Reg::A, false),
            0x5B => self.transfer(bus, Reg::A, Reg::D, false),
            0x7B => self.transfer(bus, Reg::D, Reg::A, false),
            0x1B => { self.idle_irq(bus); self.s = self.a; if self.e { self.s = 0x0100 | (self.s & 0xFF); } }
            0xBA => {
                self.idle_irq(bus);
                if x8 { let v = self.s as u8; self.x = v as u16; self.nz8(v); } else { self.x = self.s; let v = self.x; self.nz16(v); }
            }
            0x9A => { self.idle_irq(bus); if self.e { self.s = (self.s & 0xFF00) | (self.x & 0xFF); } else { self.s = self.x; } }
            0xEB => {
                bus.idle(); bus.idle();
                self.a = self.a.rotate_left(8);
                let l = self.a as u8; self.nz8(l);
            }
            // block moves
            0x44 | 0x54 => {
                let dst = self.fetch(bus) as u32;
                let src = self.fetch(bus) as u32;
                self.b = dst as u8;
                let w = bus.read(src << 16 | self.x as u32);
                bus.write(dst << 16 | self.y as u32, w);
                bus.idle();
                let adj: u16 = if op == 0x44 { 0xFFFF } else { 1 };
                if x8 {
                    self.x = (self.x as u8).wrapping_add(adj as u8) as u16;
                    self.y = (self.y as u8).wrapping_add(adj as u8) as u16;
                } else {
                    self.x = self.x.wrapping_add(adj);
                    self.y = self.y.wrapping_add(adj);
                }
                bus.idle();
                let old = self.a;
                self.a = self.a.wrapping_sub(1);
                if old != 0 { self.set_pcw(self.pcw().wrapping_sub(3)); }
            }
            0xEA => self.idle_irq(bus),
            0x42 => { self.fetch(bus); }                                // WDM
            0xCB => { self.wai = true; bus.idle(); }
            0xDB => { self.stp = true; bus.idle(); }
            _ => unreachable!("all 256 opcodes are decoded"),
        }
    }

    fn pull_reg(&mut self, bus: &mut dyn Bus816, r: Reg, w8: bool) {
        bus.idle(); bus.idle();
        if w8 { let l = self.pull(bus); self.set_lo(r, l); self.nz8(l); }
        else { let l = self.pull(bus) as u16; let h = self.pull(bus) as u16; self.set_w(r, l | h << 8); self.nz16(l | h << 8); }
    }

    fn transfer(&mut self, bus: &mut dyn Bus816, f: Reg, t: Reg, w8: bool) {
        self.idle_irq(bus);
        if w8 { let v = self.get(f) as u8; self.set_lo(t, v); self.nz8(v); }
        else { let v = self.get(f); self.set_w(t, v); self.nz16(v); }
    }

    fn read_mode(&mut self, bus: &mut dyn Bus816, mode: Am, w8: bool, o8: Alu8, o16: Alu16) {
        match (mode, w8) {
            (Am::Imm, true) => self.imm_r8(bus, o8), (Am::Imm, false) => self.imm_r16(bus, o16),
            (Am::Abs, true) => self.bank_r8(bus, o8, None), (Am::Abs, false) => self.bank_r16(bus, o16, None),
            (Am::AbsX, true) => self.bank_r8(bus, o8, Some(Reg::X)), (Am::AbsX, false) => self.bank_r16(bus, o16, Some(Reg::X)),
            (Am::AbsY, true) => self.bank_r8(bus, o8, Some(Reg::Y)), (Am::AbsY, false) => self.bank_r16(bus, o16, Some(Reg::Y)),
            (Am::Long, true) => self.long_r8(bus, o8, None), (Am::Long, false) => self.long_r16(bus, o16, None),
            (Am::LongX, true) => self.long_r8(bus, o8, Some(Reg::X)), (Am::LongX, false) => self.long_r16(bus, o16, Some(Reg::X)),
            (Am::Dp, true) => self.direct_r8(bus, o8, None), (Am::Dp, false) => self.direct_r16(bus, o16, None),
            (Am::DpX, true) => self.direct_r8(bus, o8, Some(Reg::X)), (Am::DpX, false) => self.direct_r16(bus, o16, Some(Reg::X)),
            (Am::DpY, true) => self.direct_r8(bus, o8, Some(Reg::Y)), (Am::DpY, false) => self.direct_r16(bus, o16, Some(Reg::Y)),
            (Am::Ind, true) => self.indirect_r8(bus, o8), (Am::Ind, false) => self.indirect_r16(bus, o16),
            (Am::IdxInd, true) => self.indexed_indirect_r8(bus, o8), (Am::IdxInd, false) => self.indexed_indirect_r16(bus, o16),
            (Am::IndIdx, true) => self.indirect_indexed_r8(bus, o8), (Am::IndIdx, false) => self.indirect_indexed_r16(bus, o16),
            (Am::IndLong, true) => self.indirect_long_r8(bus, o8, None), (Am::IndLong, false) => self.indirect_long_r16(bus, o16, None),
            (Am::IndLongY, true) => self.indirect_long_r8(bus, o8, Some(Reg::Y)), (Am::IndLongY, false) => self.indirect_long_r16(bus, o16, Some(Reg::Y)),
            (Am::Stack, true) => self.stack_r8(bus, o8), (Am::Stack, false) => self.stack_r16(bus, o16),
            (Am::IndStack, true) => self.indirect_stack_r8(bus, o8), (Am::IndStack, false) => self.indirect_stack_r16(bus, o16),
        }
    }
}

#[derive(Clone, Copy)]
enum Am { Imm, Abs, AbsX, AbsY, Long, LongX, Dp, DpX, DpY, Ind, IdxInd, IndIdx, IndLong, IndLongY, Stack, IndStack }

#[cfg(test)]
#[path = "w65816_tests.rs"]
mod tests;
