//! Konami custom CPU (052001 / 052526 / 053248): a gate-array HD6309 relative with its own
//! opcode map, Konami-only instructions (block moves and fills, multi-bit shifts, 32-bit
//! multiply, divide, DBJNZ/DXJNZ, ABS, SETLINES for bank switching) and its own indexed
//! postbyte format. Used by The Simpsons, Vendetta, Crime Fighters, Aliens, Parodius...
//!
//! A cycle-for-cycle translation of MAME's konami.cpp / konami.lst (BSD-3-Clause, Nathan
//! Woods; measured timings and flag behaviour by Jose Tejada, Oct 2026): every bus access
//! costs one clock, internal steps add their own, and instructions with a measured clock
//! count are padded up to it. Checked against MAME's generated core with random test
//! vectors (tools/konami_ref).

use serde::{Deserialize, Serialize};

pub trait KonamiBus {
    fn read(&mut self, addr: u16) -> u8;
    fn write(&mut self, addr: u16, val: u8);
    /// opcode / operand fetch (the same as `read` on every board so far)
    fn read_opcode(&mut self, addr: u16) -> u8 { self.read(addr) }
    /// the SETLINES instruction's output (boards use it for ROM banking)
    fn set_lines(&mut self, _val: u8) {}
}

const CC_C: u8 = 0x01;
const CC_V: u8 = 0x02;
const CC_Z: u8 = 0x04;
const CC_N: u8 = 0x08;
const CC_I: u8 = 0x10;
const CC_H: u8 = 0x20;
const CC_F: u8 = 0x40;
const CC_E: u8 = 0x80;
const CC_NZ: u8 = CC_N | CC_Z;
const CC_NZV: u8 = CC_N | CC_Z | CC_V;
const CC_NZVC: u8 = CC_N | CC_Z | CC_V | CC_C;
const CC_HNZVC: u8 = CC_H | CC_NZVC;

const VEC_FIRQ: u16 = 0xFFF6;
const VEC_IRQ: u16 = 0xFFF8;
const VEC_SWI: u16 = 0xFFFA;
const VEC_NMI: u16 = 0xFFFC;

#[derive(Clone, Copy, PartialEq)]
enum Mode { Imm, Ea, A, B, D }

#[derive(Clone, Copy, PartialEq)]
enum R16 { D, X, Y, U, S }

#[derive(Clone, Serialize, Deserialize)]
pub struct Konami {
    pub a: u8,
    pub b: u8,
    pub x: u16,
    pub y: u16,
    pub u: u16,
    pub s: u16,
    pub pc: u16,
    pub dp: u8,
    pub cc: u8,
    /// low byte of the "DP" word EXG/TFR register 4 (stack ops only see DP)
    pub dp_low: u8,
    /// after EXG PC,X the next opcode is the stale one already fetched
    pub pc_transfer: Option<u8>,
    pub irq_line: bool,
    pub firq_line: bool,
    /// an NMI edge waiting to be taken
    pub nmi_pending: bool,
    // per-instruction scratch
    #[serde(skip)] c: u32,
    #[serde(skip)] ea: u16,
    #[serde(skip, default = "imm")] mode: Mode,
    #[serde(skip)] temp_im: u8,
    #[serde(skip)] postbyte: u8,
}

fn imm() -> Mode { Mode::Imm }

impl Default for Konami { fn default() -> Self { Self::new() } }

/// MAME's set_flags<T>(mask, a, b, r) for a `bits`-wide T.
fn flags(cc: &mut u8, mask: u8, a: u32, b: u32, r: u32, bits: u32) -> u32 {
    let hi = 1u32 << (bits - 1);
    let m = if bits == 32 { u32::MAX } else { (1u32 << bits) - 1 };
    *cc &= !mask;
    if mask & CC_H != 0 && (a ^ b ^ r) & 0x10 != 0 { *cc |= CC_H; }
    if mask & CC_N != 0 && r & hi != 0 { *cc |= CC_N; }
    if mask & CC_Z != 0 && r & m == 0 { *cc |= CC_Z; }
    if mask & CC_V != 0 && (a ^ b ^ r ^ (r >> 1)) & hi != 0 { *cc |= CC_V; }
    if mask & CC_C != 0 && bits < 32 && r & (hi << 1) != 0 { *cc |= CC_C; }
    r & m
}

impl Konami {
    pub fn new() -> Self {
        Self {
            a: 0, b: 0, x: 0, y: 0, u: 0, s: 0, pc: 0, dp: 0, cc: CC_I | CC_F, dp_low: 0,
            pc_transfer: None, irq_line: false, firq_line: false, nmi_pending: false,
            c: 0, ea: 0, mode: Mode::Imm, temp_im: 0, postbyte: 0,
        }
    }

    pub fn d(&self) -> u16 { ((self.a as u16) << 8) | self.b as u16 }
    fn set_d(&mut self, v: u16) { self.a = (v >> 8) as u8; self.b = v as u8; }

    /// Reset: DP 0, interrupts masked, PC from the reset vector.
    pub fn reset(&mut self, bus: &mut dyn KonamiBus) {
        self.dp = 0;
        self.dp_low = 0;
        self.cc |= CC_I | CC_F;
        self.pc_transfer = None;
        self.nmi_pending = false;
        self.pc = ((bus.read(0xFFFE) as u16) << 8) | bus.read(0xFFFF) as u16;
    }

    pub fn set_irq(&mut self, on: bool) { self.irq_line = on; }
    pub fn set_firq(&mut self, on: bool) { self.firq_line = on; }
    /// NMI is edge triggered: call on the rising edge.
    pub fn nmi(&mut self) { self.nmi_pending = true; }

    // ---- bus helpers (one clock each) ----
    fn rd(&mut self, bus: &mut dyn KonamiBus, a: u16) -> u8 { self.c += 1; bus.read(a) }
    fn wr(&mut self, bus: &mut dyn KonamiBus, a: u16, v: u8) { self.c += 1; bus.write(a, v); }
    fn arg(&mut self, bus: &mut dyn KonamiBus) -> u8 {
        self.c += 1;
        let v = bus.read_opcode(self.pc);
        self.pc = self.pc.wrapping_add(1);
        v
    }
    fn arg16(&mut self, bus: &mut dyn KonamiBus) -> u16 {
        let h = self.arg(bus) as u16;
        (h << 8) | self.arg(bus) as u16
    }
    fn eat(&mut self, n: u32) { self.c += n; }

    fn r16(&self, r: R16) -> u16 {
        match r { R16::D => self.d(), R16::X => self.x, R16::Y => self.y, R16::U => self.u, R16::S => self.s }
    }
    fn set_r16(&mut self, r: R16, v: u16) {
        match r { R16::D => self.set_d(v), R16::X => self.x = v, R16::Y => self.y = v, R16::U => self.u = v, R16::S => self.s = v }
    }

    // ---- operands ----
    fn read_op(&mut self, bus: &mut dyn KonamiBus) -> u8 {
        match self.mode {
            Mode::Ea => { let a = self.ea; self.rd(bus, a) }
            Mode::Imm => self.arg(bus),
            Mode::A => self.a,
            Mode::B => self.b,
            Mode::D => 0,
        }
    }
    fn read_op_n(&mut self, bus: &mut dyn KonamiBus, n: u16) -> u8 {
        match self.mode {
            Mode::Ea => { let a = self.ea.wrapping_add(n); self.rd(bus, a) }
            Mode::Imm => self.arg(bus),
            Mode::D => if n & 1 != 0 { self.b } else { self.a },
            _ => 0,
        }
    }
    fn read_op16(&mut self, bus: &mut dyn KonamiBus) -> u16 {
        let h = self.read_op_n(bus, 0) as u16;
        (h << 8) | self.read_op_n(bus, 1) as u16
    }
    fn write_op(&mut self, bus: &mut dyn KonamiBus, v: u8) {
        match self.mode {
            Mode::Ea => { let a = self.ea; self.wr(bus, a, v) }
            Mode::A => self.a = v,
            Mode::B => self.b = v,
            _ => {}
        }
    }
    fn write_op_n(&mut self, bus: &mut dyn KonamiBus, n: u16, v: u8) {
        match self.mode {
            Mode::Ea => { let a = self.ea.wrapping_add(n); self.wr(bus, a, v) }
            Mode::D => if n & 1 != 0 { self.b = v } else { self.a = v },
            _ => {}
        }
    }
    fn write_op16(&mut self, bus: &mut dyn KonamiBus, v: u16) {
        self.write_op_n(bus, 0, (v >> 8) as u8);
        self.write_op_n(bus, 1, v as u8);
    }
    fn is_reg_mode(&self) -> bool { !matches!(self.mode, Mode::Imm | Mode::Ea) }

    fn f8(&mut self, mask: u8, a: u32, b: u32, r: u32) -> u8 { flags(&mut self.cc, mask, a, b, r, 8) as u8 }
    fn f16(&mut self, mask: u8, a: u32, b: u32, r: u32) -> u16 { flags(&mut self.cc, mask, a, b, r, 16) as u16 }

    fn ireg(&mut self, pb: u8) -> &mut u16 {
        match pb & 0x70 { 0x20 => &mut self.x, 0x30 => &mut self.y, 0x50 => &mut self.u, 0x60 => &mut self.s, _ => &mut self.pc }
    }

    /// Indexed addressing: the postbyte and its extras; sets the effective address.
    fn indexed(&mut self, bus: &mut dyn KonamiBus) {
        let pb = self.arg(bus);
        self.postbyte = pb;
        let mut t: u16 = match pb & 0xF7 {
            0x07 => { let v = self.arg16(bus); self.eat(1); v }
            0x20 | 0x30 | 0x50 | 0x60 | 0x70 => {
                let r = self.ireg(pb); let v = *r; *r = v.wrapping_add(1); self.eat(2); v
            }
            0x21 | 0x31 | 0x51 | 0x61 | 0x71 => {
                let r = self.ireg(pb); let v = *r; *r = v.wrapping_add(2); self.eat(3); v
            }
            0x22 | 0x32 | 0x52 | 0x62 | 0x72 => {
                let r = self.ireg(pb); *r = r.wrapping_sub(1); let v = *r; self.eat(2); v
            }
            0x23 | 0x33 | 0x53 | 0x63 | 0x73 => {
                let r = self.ireg(pb); *r = r.wrapping_sub(2); let v = *r; self.eat(3); v
            }
            0x24 | 0x34 | 0x54 | 0x64 | 0x74 => {
                let base = *self.ireg(pb);
                let o = self.arg(bus) as i8;
                self.eat(2);
                base.wrapping_add(o as u16)
            }
            0x25 | 0x35 | 0x55 | 0x65 | 0x75 => {
                let base = *self.ireg(pb);
                let o = self.arg16(bus);
                self.eat(2);
                base.wrapping_add(o)
            }
            0x26 | 0x36 | 0x56 | 0x66 | 0x76 => { let v = *self.ireg(pb); self.eat(1); v }
            0xC4 => { let l = self.arg(bus); self.eat(1); ((self.dp as u16) << 8) | l as u16 }
            0xA0 | 0xB0 | 0xD0 | 0xE0 | 0xF0 => {
                let v = self.ireg(pb).wrapping_add(self.a as i8 as u16); self.eat(2); v
            }
            0xA1 | 0xB1 | 0xD1 | 0xE1 | 0xF1 => {
                let v = self.ireg(pb).wrapping_add(self.b as i8 as u16); self.eat(2); v
            }
            0xA7 | 0xB7 | 0xD7 | 0xE7 | 0xF7 => {
                let d = self.d(); let v = self.ireg(pb).wrapping_add(d); self.eat(2); v
            }
            _ => 0,
        };
        if pb & 0x08 != 0 {
            let h = self.rd(bus, t) as u16;
            let l = self.rd(bus, t.wrapping_add(1)) as u16;
            t = (h << 8) | l;
            self.eat(1);
        }
        self.ea = t;
        self.mode = Mode::Ea;
    }

    fn other_stack(&self, s_stack: bool) -> u16 { if s_stack { self.u } else { self.s } }

    fn push8(&mut self, bus: &mut dyn KonamiBus, sp: &mut u16, v: u8) {
        *sp = sp.wrapping_sub(1);
        self.wr(bus, *sp, v);
    }
    fn push16(&mut self, bus: &mut dyn KonamiBus, sp: &mut u16, v: u16) {
        self.push8(bus, sp, v as u8);
        self.push8(bus, sp, (v >> 8) as u8);
    }
    fn pull8(&mut self, bus: &mut dyn KonamiBus, sp: &mut u16) -> u8 {
        let v = self.rd(bus, *sp);
        *sp = sp.wrapping_add(1);
        v
    }
    fn pull16(&mut self, bus: &mut dyn KonamiBus, sp: &mut u16) -> u16 {
        let h = self.pull8(bus, sp) as u16;
        (h << 8) | self.pull8(bus, sp) as u16
    }

    fn push_regs(&mut self, bus: &mut dyn KonamiBus, mask: u8, s_stack: bool) {
        let mut sp = if s_stack { self.s } else { self.u };
        if mask & 0x80 != 0 { let v = self.pc; self.push16(bus, &mut sp, v); }
        if mask & 0x40 != 0 { let v = self.other_stack(s_stack); self.push16(bus, &mut sp, v); }
        if mask & 0x20 != 0 { let v = self.y; self.push16(bus, &mut sp, v); }
        if mask & 0x10 != 0 { let v = self.x; self.push16(bus, &mut sp, v); }
        if mask & 0x08 != 0 { let v = self.dp; self.push8(bus, &mut sp, v); }
        if mask & 0x04 != 0 { let v = self.b; self.push8(bus, &mut sp, v); }
        if mask & 0x02 != 0 { let v = self.a; self.push8(bus, &mut sp, v); }
        if mask & 0x01 != 0 { let v = self.cc; self.push8(bus, &mut sp, v); }
        if s_stack { self.s = sp } else { self.u = sp }
    }

    fn pull_regs(&mut self, bus: &mut dyn KonamiBus, mask: u8, s_stack: bool) {
        let mut sp = if s_stack { self.s } else { self.u };
        if mask & 0x01 != 0 { self.cc = self.pull8(bus, &mut sp); }
        if mask & 0x02 != 0 { self.a = self.pull8(bus, &mut sp); }
        if mask & 0x04 != 0 { self.b = self.pull8(bus, &mut sp); }
        if mask & 0x08 != 0 { self.dp = self.pull8(bus, &mut sp); }
        if mask & 0x10 != 0 { self.x = self.pull16(bus, &mut sp); }
        if mask & 0x20 != 0 { self.y = self.pull16(bus, &mut sp); }
        if mask & 0x40 != 0 {
            let v = self.pull16(bus, &mut sp);
            if s_stack { self.u = v } else { self.s = v }
        }
        if mask & 0x80 != 0 { self.pc = self.pull16(bus, &mut sp); }
        if s_stack { self.s = sp } else { self.u = sp }
        self.eat(1);
    }

    fn vector(&mut self, bus: &mut dyn KonamiBus, v: u16) {
        self.eat(1);
        let h = self.rd(bus, v) as u16;
        let l = self.rd(bus, v.wrapping_add(1)) as u16;
        self.pc = (h << 8) | l;
        self.eat(1);
    }

    fn take_nmi(&mut self, bus: &mut dyn KonamiBus) {
        self.nmi_pending = false;
        self.eat(3);
        self.cc |= CC_E;
        self.push_regs(bus, 0xFF, true);
        self.cc |= CC_I | CC_F;
        self.vector(bus, VEC_NMI);
    }
    fn take_firq(&mut self, bus: &mut dyn KonamiBus) {
        self.eat(3);
        self.cc &= !CC_E;
        self.push_regs(bus, 0x81, true);
        self.cc |= CC_I | CC_F;
        self.vector(bus, VEC_FIRQ);
    }
    fn take_irq(&mut self, bus: &mut dyn KonamiBus) {
        self.eat(3);
        self.cc |= CC_E;
        self.push_regs(bus, 0xFF, true);
        self.cc |= CC_I;
        self.vector(bus, VEC_IRQ);
    }

    fn cond(&self, op: u8) -> bool {
        let c = self.cc & CC_C != 0;
        let v = self.cc & CC_V != 0;
        let z = self.cc & CC_Z != 0;
        let n = self.cc & CC_N != 0;
        let t = match op & 7 {
            0 => true,
            1 => !(c || z),
            2 => !c,
            3 => !z,
            4 => !v,
            5 => !n,
            6 => n == v,
            _ => n == v && !z,
        };
        if op & 0x10 != 0 { !t } else { t }
    }

    fn branch(&mut self, bus: &mut dyn KonamiBus, taken: bool) {
        let o = self.arg(bus) as i8;
        self.eat(1);
        if taken { self.pc = self.pc.wrapping_add(o as u16); }
    }
    fn lbranch(&mut self, bus: &mut dyn KonamiBus, taken: bool) {
        let o = self.arg16(bus);
        self.eat(1);
        if taken { self.pc = self.pc.wrapping_add(o); }
    }
    fn subroutine(&mut self, bus: &mut dyn KonamiBus, target: u16) {
        let pc = self.pc;
        self.s = self.s.wrapping_sub(1);
        let s = self.s;
        self.wr(bus, s, pc as u8);
        self.s = self.s.wrapping_sub(1);
        let s = self.s;
        self.wr(bus, s, (pc >> 8) as u8);
        self.pc = target;
    }

    fn exg_read(&self, r: u8) -> u16 {
        match r & 7 {
            0 => self.a as u16 | 0x1000,
            1 => self.d(),
            2 => self.x,
            3 => self.y,
            4 => ((self.dp as u16) << 8) | self.dp_low as u16,
            5 => self.u,
            6 => self.s,
            _ => self.pc,
        }
    }
    fn exg_write(&mut self, r: u8, v: u16) {
        match r & 7 {
            0 => self.a = v as u8,
            1 => self.b = v as u8,
            2 => self.x = v,
            3 => self.y = v,
            4 => { self.dp = (v >> 8) as u8; self.dp_low = v as u8; }
            5 => self.u = v,
            6 => self.s = v,
            _ => self.pc = v,
        }
    }

    /// Runs one instruction (or takes one interrupt). Returns the clocks used.
    pub fn step(&mut self, bus: &mut dyn KonamiBus) -> u32 {
        self.c = 0;
        if self.nmi_pending { self.take_nmi(bus); return self.c; }
        if self.cc & CC_F == 0 && self.firq_line { self.take_firq(bus); return self.c; }
        if self.cc & CC_I == 0 && self.irq_line { self.take_irq(bus); return self.c; }

        let op = match self.pc_transfer.take() {
            Some(o) => { self.pc = self.pc.wrapping_add(1); self.eat(1); o }
            None => self.arg(bus),
        };
        // the measured timings look at the state right after the opcode fetch
        let t = Timing {
            op,
            pb: bus.read_opcode(self.pc),
            op1: bus.read_opcode(self.pc.wrapping_add(1)),
            cc: self.cc, a: self.a, b: self.b, u: self.u, x: self.x,
        };
        self.execute(bus, op);
        if let Some(target) = t.cycles(self.x, self.cc) {
            if self.c < target { self.c = target; }
        }
        self.c
    }

    fn execute(&mut self, bus: &mut dyn KonamiBus, op: u8) {
        use Mode::*;
        // 8-bit register operand of most ALU ops: bit 0 picks A / B
        let r8_b = op & 1 != 0;
        match op {
            0x00 => self.vector(bus, 0xFFFE),
            0x02 => { self.pc = self.pc.wrapping_sub(1); self.take_nmi(bus); }
            0x03 => { self.pc = self.pc.wrapping_sub(1); self.take_firq(bus); }
            0x04 => { self.pc = self.pc.wrapping_sub(1); self.take_irq(bus); }
            0x05..=0x09 => {
                self.indexed(bus);
                let r = if op == 0x09 { R16::Y } else { R16::X };
                let ea = self.ea;
                let v = self.f16(CC_Z, 0, ea as u32, ea as u32);
                self.set_r16(r, v);
                self.eat(1);
            }
            0x0A | 0x0B => {
                self.indexed(bus);
                let ea = self.ea;
                self.set_r16(if op == 0x0A { R16::U } else { R16::S }, ea);
                self.eat(1);
            }
            0x0C | 0x0D => {
                let m = self.arg(bus);
                self.eat(2);
                self.push_regs(bus, m, op == 0x0C);
            }
            0x0E | 0x0F => {
                let m = self.arg(bus);
                self.eat(1);
                self.pull_regs(bus, m, op == 0x0E);
            }
            // LD / ADD / ADC / SUB / SBC / AND / BIT / EOR / OR / CMP (8-bit)
            0x10..=0x37 => {
                if op & 2 != 0 { self.indexed(bus) } else { self.mode = Imm }
                let reg = if r8_b { self.b } else { self.a };
                let res: Option<u8> = match op & 0xFC {
                    0x10 => { let v = self.read_op(bus); Some(self.f8(CC_NZV, 0, v as u32, v as u32)) }
                    0x14 => { let m = self.read_op(bus); Some(self.f8(CC_HNZVC, reg as u32, m as u32, reg as u32 + m as u32)) }
                    0x18 => {
                        let m = self.read_op(bus);
                        let c = (self.cc & CC_C) as u32;
                        Some(self.f8(CC_HNZVC, reg as u32, m as u32, reg as u32 + m as u32 + c))
                    }
                    0x1C => { let m = self.read_op(bus); Some(self.f8(CC_NZVC, reg as u32, m as u32, (reg as u32).wrapping_sub(m as u32))) }
                    0x20 => {
                        let m = self.read_op(bus);
                        let c = (self.cc & CC_C) as u32;
                        Some(self.f8(CC_NZVC, reg as u32, m as u32, (reg as u32).wrapping_sub(m as u32).wrapping_sub(c)))
                    }
                    0x24 => { self.cc &= !CC_V; let m = self.read_op(bus); Some(self.f8(CC_NZ, 0, reg as u32, (reg & m) as u32)) }
                    0x28 => { self.cc &= !CC_V; let m = self.read_op(bus); self.f8(CC_NZ, 0, reg as u32, (reg & m) as u32); None }
                    0x2C => { self.cc &= !CC_V; let m = self.read_op(bus); Some(self.f8(CC_NZ, 0, reg as u32, (reg ^ m) as u32)) }
                    0x30 => { self.cc &= !CC_V; let m = self.read_op(bus); Some(self.f8(CC_NZ, 0, reg as u32, (reg | m) as u32)) }
                    _ => { let m = self.read_op(bus); self.f8(CC_NZVC, reg as u32, m as u32, (reg as u32).wrapping_sub(m as u32)); None }
                };
                if let Some(v) = res { if r8_b { self.b = v } else { self.a = v } }
            }
            0x38 | 0x39 => {
                if op == 0x39 { self.indexed(bus) } else { self.mode = Imm }
                let v = self.read_op(bus);
                bus.set_lines(v);
            }
            0x3A | 0x3B => {
                self.indexed(bus);
                let reg = if r8_b { self.b } else { self.a };
                let v = self.f8(CC_NZV, 0, reg as u32, reg as u32);
                let ea = self.ea;
                self.wr(bus, ea, v);
            }
            0x3C => { self.mode = Imm; let v = self.read_op(bus); self.cc &= v; }
            0x3D => { self.mode = Imm; let v = self.read_op(bus); self.cc |= v; }
            0x3E | 0x3F => {
                let p = self.arg(bus);
                let dst = (p >> 4) & 7;
                let src = p & 7;
                let pc_x = p & 0x80 == 0 && ((dst == 7 && src == 2) || (dst == 2 && src == 7));
                let stale = if pc_x { bus.read_opcode(self.pc) } else { 0 };
                let reg = self.exg_read(p);
                if p & 0x80 == 0 {
                    let reg2 = self.exg_read(p >> 4);
                    self.exg_write(p, reg2);
                    self.eat(1);
                }
                self.exg_write(p >> 4, reg);
                if pc_x { self.pc_transfer = Some(stale); }
                self.eat(2);
            }
            // 16-bit LD / CMP / ADD / SUB, ST
            0x40..=0x57 => {
                if op & 1 != 0 { self.indexed(bus) } else { self.mode = Imm }
                let r = match op {
                    0x40 | 0x41 | 0x4A | 0x4B | 0x54..=0x57 => R16::D,
                    0x42 | 0x43 | 0x4C | 0x4D => R16::X,
                    0x44 | 0x45 | 0x4E | 0x4F => R16::Y,
                    0x46 | 0x47 | 0x50 | 0x51 => R16::U,
                    _ => R16::S,
                };
                let m = self.read_op16(bus) as u32;
                let reg = self.r16(r) as u32;
                match op {
                    0x40..=0x49 => { let v = self.f16(CC_NZV, 0, m, m); self.set_r16(r, v); }
                    0x54 | 0x55 => { let v = self.f16(CC_NZVC, reg, m, reg + m); self.set_r16(r, v); }
                    0x56 | 0x57 => { let v = self.f16(CC_NZVC, reg, m, reg.wrapping_sub(m)); self.set_r16(r, v); }
                    _ => { self.f16(CC_NZVC, reg, m, reg.wrapping_sub(m)); }
                }
            }
            0x58..=0x5C => {
                self.indexed(bus);
                let r = [R16::D, R16::X, R16::Y, R16::U, R16::S][(op - 0x58) as usize];
                let v = self.r16(r);
                self.write_op16(bus, v);
                self.f16(CC_NZV, 0, v as u32, v as u32);
            }
            0x5D => {
                self.cc |= CC_E;
                self.eat(2);
                self.push_regs(bus, 0xFF, true);
                self.cc |= CC_I | CC_F;
                self.vector(bus, VEC_SWI);
            }
            0x60..=0x67 | 0x70..=0x77 => { let t = self.cond(op); self.branch(bus, t); }
            0x68..=0x6F | 0x78..=0x7F => { let t = self.cond(op); self.lbranch(bus, t); }
            // CLR / COM / NEG / INC / DEC / TST / LSR / ROR / ASR / ASL (8-bit), RTS, RTI
            0x80..=0x9E if op != 0x8F => {
                // 80-8E: CLR COM NEG INC DEC; 90-9E (after RTS at 8F): TST LSR ROR ASR ASL
                let (kind, slot) = if op < 0x8F { ((op - 0x80) / 3, (op - 0x80) % 3) } else { (6 + (op - 0x90) / 3, (op - 0x90) % 3) };
                match slot { 0 => self.mode = A, 1 => self.mode = B, _ => self.indexed(bus) }
                self.rmw8(bus, kind);
            }
            0x8F => self.pull_regs(bus, 0x80, true),
            0x9F => {
                self.eat(1);
                let s = self.s;
                self.cc = self.rd(bus, s);
                self.s = s.wrapping_add(1);
                let mask = if self.cc & CC_E != 0 { 0xFE } else { 0x80 };
                self.pull_regs(bus, mask, true);
            }
            0xA0..=0xA2 => {
                match op { 0xA0 => self.mode = A, 0xA1 => self.mode = B, _ => self.indexed(bus) }
                let old = self.cc;
                let v = self.read_op(bus);
                let r = (v << 1) | (old & CC_C);
                self.cc = (self.cc & !(CC_C | CC_Z)) | if v & 0x80 != 0 { CC_C } else { 0 };
                if old & CC_C != 0 { self.cc |= old & CC_Z; } else if r == 0 { self.cc |= CC_Z; }
                self.write_op(bus, r);
            }
            0xA3..=0xA7 => {
                self.indexed(bus);
                let t = self.read_op16(bus);
                let r = match op {
                    0xA3 => { self.cc = (self.cc & !CC_C) | (t & 1) as u8; self.f16(CC_NZ, 0, (t >> 1) as u32, (t >> 1) as u32) }
                    0xA4 => {
                        let c = self.cc & CC_C != 0;
                        self.cc = (self.cc & !CC_C) | (t & 1) as u8;
                        let v = (t >> 1) | if c { 0x8000 } else { 0 };
                        self.f16(CC_NZ, 0, v as u32, v as u32)
                    }
                    0xA5 => {
                        self.cc = (self.cc & !CC_C) | (t & 1) as u8;
                        let v = ((t as i16) >> 1) as u16;
                        self.f16(CC_NZ, 0, v as u32, v as u32)
                    }
                    0xA6 => self.f16(CC_NZVC, t as u32, t as u32, (t as u32) << 1),
                    _ => {
                        let v = (t << 1) | (self.cc & CC_C) as u16;
                        self.f16(CC_NZ, 0, v as u32, v as u32);
                        self.cc &= !(CC_V | CC_C);
                        if (t ^ v) & 0x8000 != 0 { self.cc |= CC_V; }
                        if t & 0x8000 != 0 { self.cc |= CC_C; }
                        v
                    }
                };
                self.write_op16(bus, r);
            }
            0xA8 => { self.indexed(bus); self.pc = self.ea; }
            0xA9 => { self.indexed(bus); self.eat(2); let ea = self.ea; self.subroutine(bus, ea); }
            0xAA => {
                let o = self.arg(bus) as i8;
                let ea = self.pc.wrapping_add(o as u16);
                self.eat(2);
                self.subroutine(bus, ea);
            }
            0xAB => {
                let o = self.arg16(bus);
                let ea = self.pc.wrapping_add(o);
                self.eat(2);
                self.subroutine(bus, ea);
            }
            0xAC => {
                let b = self.b as u32;
                self.b = self.f8(CC_NZV, b, 1, b.wrapping_sub(1));
                self.eat(1);
                let t = self.cc & CC_Z == 0;
                self.branch(bus, t);
            }
            0xAD => {
                let x = self.x as u32;
                self.x = self.f16(CC_NZV, x, 1, x.wrapping_sub(1));
                self.eat(1);
                let t = self.cc & CC_Z == 0;
                self.branch(bus, t);
            }
            0xAE => {}
            0xB0 => self.x = self.x.wrapping_add(self.b as u16),
            0xB1 => { self.daa(); self.eat(1); }
            0xB2 => {
                let v = self.b as i8 as u16;
                let r = self.f16(CC_NZ, 0, v as u32, v as u32);
                self.set_d(r);
                self.cc &= !CC_V;
            }
            0xB3 => {
                let r = self.a as u16 * self.b as u16;
                let r = self.f16(CC_Z, 0, r as u32, r as u32);
                self.set_d(r);
                if r & 0x80 != 0 { self.cc |= CC_C } else { self.cc &= !CC_C }
                self.eat(9);
            }
            0xB4 => {
                let r = self.x as u32 * self.y as u32;
                self.x = (r >> 16) as u16;
                self.y = r as u16;
                flags(&mut self.cc, CC_Z, 0, r, r, 32);
                self.eat(21);
            }
            0xB5 => {
                let (q, rem) = if self.b != 0 {
                    (self.x / self.b as u16, (self.x % self.b as u16) as u8)
                } else {
                    (0xFFFF, self.x as u8)
                };
                self.x = self.f16(CC_NZ, 0, q as u32, q as u32);
                self.b = rem;
                self.eat(10);
            }
            0xB6 => {
                loop {
                    let y = self.y;
                    let v = self.rd(bus, y);
                    self.y = y.wrapping_add(1);
                    let x = self.x;
                    self.wr(bus, x, v);
                    self.x = x.wrapping_add(1);
                    self.u = self.u.wrapping_sub(1);
                    if self.u == 0 { break; }
                }
                self.cc |= CC_Z;
            }
            0xB7 => {
                let y = self.y;
                let v = self.rd(bus, y);
                self.y = y.wrapping_add(1);
                let x = self.x;
                self.wr(bus, x, v);
                self.x = x.wrapping_add(1);
                self.u = self.u.wrapping_sub(1);
                self.cc = (self.cc & !CC_Z) | if self.u == 0 { CC_Z } else { 0 };
            }
            0xB8..=0xC1 => {
                if op & 1 == 0 { self.temp_im = self.arg(bus); self.mode = D } else { self.indexed(bus) }
                let count = if self.is_reg_mode() { self.temp_im } else { self.a } & 0x0F;
                let mut t = self.read_op16(bus);
                for _ in 0..count {
                    self.eat(1);
                    match op & 0xFE {
                        0xB8 => { self.cc = (self.cc & !CC_C) | (t & 1) as u8; t >>= 1; }
                        0xBA => t = t.rotate_right(1),
                        0xBC => { self.cc = (self.cc & !CC_C) | (t & 1) as u8; t = ((t as i16) >> 1) as u16; }
                        0xBE => t <<= 1,
                        _ => t = t.rotate_left(1),
                    }
                }
                let t = self.f16(CC_NZ, 0, t as u32, t as u32);
                self.cc &= !(CC_V | CC_C);
                self.write_op16(bus, t);
            }
            // 16-bit CLR / NEG / INC / DEC / TST / ABS on D or memory
            0xC2..=0xCB => {
                if op & 1 == 0 { self.mode = D } else { self.indexed(bus) }
                match op & 0xFE {
                    0xC2 => {
                        self.cc = (self.cc & !CC_NZVC) | CC_Z;
                        self.write_op16(bus, 0);
                    }
                    0xC4 => {
                        let t = self.read_op16(bus) as u32;
                        let v = self.f16(CC_NZVC, 0, t, 0u32.wrapping_sub(t));
                        self.write_op16(bus, v);
                    }
                    0xC6 => {
                        let t = self.read_op16(bus) as u32;
                        let v = self.f16(CC_NZVC, t, 1, t + 1);
                        self.write_op16(bus, v);
                    }
                    0xC8 => {
                        let t = self.read_op16(bus) as u32;
                        let v = self.f16(CC_NZVC, t, 1, t.wrapping_sub(1));
                        self.write_op16(bus, v);
                    }
                    _ => {
                        let t = self.read_op16(bus) as u32;
                        self.f16(CC_NZV, 0, t, t);
                        if !self.is_reg_mode() { self.eat(1); }
                    }
                }
            }
            0xCC | 0xCD => {
                self.mode = if op == 0xCC { A } else { B };
                let o = self.read_op(bus);
                let v = if (o as i8) >= 0 { o } else { o.wrapping_neg() };
                let v = self.f8(CC_NZ, 0, v as u32, v as u32);
                self.cc = (self.cc & !(CC_V | CC_C)) | if o & 0x80 != 0 { CC_C } else { 0 };
                if o == 0x80 { self.cc |= CC_V; }
                self.write_op(bus, v);
            }
            0xCE => {
                self.mode = D;
                let o = self.read_op16(bus);
                let v = if (o as i16) >= 0 { o } else { o.wrapping_neg() };
                let v = self.f16(CC_NZ, 0, v as u32, v as u32);
                self.cc = (self.cc & !(CC_V | CC_C)) | if o & 0x8000 != 0 { CC_C } else { 0 };
                if o == 0x8000 { self.cc |= CC_V; }
                self.write_op16(bus, v);
            }
            0xCF => {
                loop {
                    self.eat(1);
                    let x = self.x;
                    let a = self.a;
                    self.wr(bus, x, a);
                    self.x = x.wrapping_add(1);
                    self.u = self.u.wrapping_sub(1);
                    if self.u == 0 { break; }
                }
                self.cc |= CC_Z;
            }
            0xD0 => {
                loop {
                    self.eat(1);
                    let (x, a, b) = (self.x, self.a, self.b);
                    self.wr(bus, x, a);
                    self.wr(bus, x.wrapping_add(1), b);
                    self.x = x.wrapping_add(2);
                    self.u = self.u.wrapping_sub(1);
                    if self.u == 0 { break; }
                }
                self.cc |= CC_Z;
            }
            0xD1 => { self.indexed(bus); let ea = self.ea; self.wr(bus, ea, 0); }
            _ => {}   // illegal: nothing
        }
    }

    /// 8-bit read-modify-write group (opcodes 80-9E): kind 0 CLR, 1 COM, 2 NEG, 3 INC,
    /// 4 DEC, 5 (RTS slot), 6 TST, 7 LSR, 8 ROR, 9 ASR, 10 ASL.
    fn rmw8(&mut self, bus: &mut dyn KonamiBus, kind: u8) {
        let reg = self.is_reg_mode();
        match kind {
            0 => {
                self.read_op(bus);
                self.cc = (self.cc & !CC_NZVC) | CC_Z;
                if !reg { self.eat(1); }
                self.write_op(bus, 0);
            }
            1 => {
                let t = self.read_op(bus);
                self.cc = (self.cc & !CC_V) | CC_C;
                let v = !t;
                let v = self.f8(CC_NZ, 0, v as u32, v as u32);
                self.write_op(bus, v);
            }
            2 => {
                let t = self.read_op(bus) as u32;
                let v = self.f8(CC_NZVC, 0, t, 0u32.wrapping_sub(t));
                self.write_op(bus, v);
            }
            3 | 4 => {
                let t = self.read_op(bus) as u32;
                let r = if kind == 3 { t + 1 } else { t.wrapping_sub(1) };
                let v = self.f8(CC_NZV, t, 1, r);
                if !reg { self.eat(1); }
                self.write_op(bus, v);
            }
            6 => {
                let t = self.read_op(bus) as u32;
                self.f8(CC_NZV, 0, t, t);
                if !reg { self.eat(1); }
            }
            7 => {
                let t = self.read_op(bus);
                self.cc = (self.cc & !CC_C) | (t & 1);
                let v = self.f8(CC_NZ, 0, (t >> 1) as u32, (t >> 1) as u32);
                self.write_op(bus, v);
            }
            8 => {
                let t = self.read_op(bus);
                let c = self.cc & CC_C != 0;
                self.cc = (self.cc & !CC_C) | (t & 1);
                let v = (t >> 1) | if c { 0x80 } else { 0 };
                let v = self.f8(CC_NZ, 0, v as u32, v as u32);
                self.write_op(bus, v);
            }
            9 => {
                let t = self.read_op(bus);
                self.cc = (self.cc & !CC_C) | (t & 1);
                let v = ((t as i8) >> 1) as u8;
                let v = self.f8(CC_NZ, 0, v as u32, v as u32);
                self.write_op(bus, v);
            }
            _ => {
                let t = self.read_op(bus) as u32;
                let v = self.f8(CC_NZVC, t, t, t << 1);
                self.write_op(bus, v);
            }
        }
    }

    /// The Konami DAA (measured flag behaviour).
    fn daa(&mut self) {
        let old = self.a;
        let low = if self.cc & CC_H != 0 || (old & 0x0F) > 9 { 0x06u16 } else { 0 };
        let high = if self.cc & CC_C != 0 || old > 0x99 { 0x60u16 } else { 0 };
        let sum = old as u16 + low + high;
        let adj = sum as u8;
        let carry = sum > 0xFF;
        let old_c = self.cc & CC_C;
        self.a = adj;
        self.cc &= !(CC_H | CC_N | CC_Z | CC_V | CC_C);
        if (old & 0x0F) as u16 + low > 0x0F { self.cc |= CC_H; }
        if high == 0 {
            if adj & 0x80 != 0 { self.cc |= CC_N; }
            if adj == 0 { self.cc |= CC_Z; }
        }
        if (old_c != 0) != carry { self.cc |= CC_C; }
    }
}

/// State captured after the opcode fetch for the measured instruction timings.
struct Timing { op: u8, pb: u8, op1: u8, cc: u8, a: u8, b: u8, u: u16, x: u16 }

impl Timing {
    /// Measured clock count (MAME's documented_instruction_cycles); `x` and `cc` are the
    /// values after the instruction (two entries depend on them).
    fn cycles(&self, x_now: u16, cc_now: u8) -> Option<u32> {
        let (op, pb, cc) = (self.op, self.pb, self.cc);
        let bit = |v: u32, n: u32| (v >> n) & 1;
        if op == 0x92 {
            let base = pb & 0xF0;
            if matches!(base, 0x20 | 0x30 | 0x50 | 0x60) {
                let c = match pb & 7 { 0 | 2 | 4 | 6 => Some(16), 1 | 3 => Some(17), 5 => Some(22), _ => None };
                if let Some(c) = c { return Some(c + if pb & 8 != 0 { 7 } else { 0 }); }
            }
            return match pb {
                0x07 => Some(21), 0xC4 => Some(19), 0xCC => Some(26), 0x0F => Some(28),
                0xA0 | 0xB0 | 0xE0 => Some(18), 0xD0 => Some(20),
                0xA8 | 0xB8 | 0xE8 => Some(25), 0xD8 => Some(27),
                0xE7 => Some(20), 0xEF => Some(27),
                0x76 | 0x70 => Some(16), 0x71 => Some(17), 0x78 => Some(23), 0x79 => Some(24),
                0x74 => Some(21), 0x75 => Some(22), 0x7C => Some(28), 0x7D => Some(29),
                0xF0 | 0xF1 => Some(18), 0xF7 => Some(20), 0xF8 | 0xF9 => Some(25), 0xFF => Some(27),
                _ => None,
            };
        }
        let pb32 = pb as u32;
        let ones = |lo: u32, n: u32| (lo..lo + n).map(|i| bit(pb32, i)).sum::<u32>();
        let fixed = match op {
            0x0C | 0x0D => return Some(21 + 3 * ones(0, 4) + 7 * ones(4, 4)),
            0x0E | 0x0F => return Some(20 + 4 * ones(0, 4) + 8 * ones(4, 3) + if pb & 0x80 != 0 { 7 } else { 0 }),
            0x14 | 0x15 | 0x18 | 0x19 | 0x1C | 0x1D | 0x20 | 0x21 | 0x34 | 0x35 => Some(8),
            0x10 | 0x11 | 0x24 | 0x25 | 0x28 | 0x29 | 0x2C | 0x2D | 0x30 | 0x31 | 0x38 | 0x3C | 0x3D => Some(9),
            0x3E | 0x3F => {
                let d = (pb >> 4) & 7;
                let s = pb & 7;
                if pb & 0x80 == 0 {
                    if d <= 5 && s <= 5 { return Some(8); }
                    if op == 0x3E && pb == 0x26 { return Some(8); }
                } else if pb == 0xA3 || (op == 0x3E && (pb == 0xA6 || pb == 0xA7)) {
                    return Some(8);
                }
                return None;
            }
            0x40 | 0x42 | 0x44 | 0x46 | 0x48 | 0x4C | 0x4E | 0x50 | 0x52 => Some(12),
            0x4A | 0x54 | 0x56 => Some(13),
            0x60 => Some(11),
            0x68 => Some(13),
            0x70 => Some(6),
            0x78 => Some(7),
            0x02 | 0x04 | 0x5D => Some(65),
            0x03 => Some(29),
            0x05 => return match pb { 0x07 => Some(26), 0xC4 => Some(24), 0x26 => Some(21), _ => None },
            0x06 => return match pb { 0x07 => Some(24), 0xC4 => Some(22), 0x26 => Some(19), _ => None },
            0x07 => return match pb { 0x07 => Some(22), 0xC4 => Some(20), 0x26 => Some(17), _ => None },
            0x08..=0x0B => return if pb == 0x07 { Some(20) } else { None },
            0xAA => Some(19),
            0xAB => Some(21),
            0xAC => return if pb == 0 { Some(if self.b == 1 { 9 } else { 13 }) } else { None },
            0xAD => return if pb == 0 { Some(if self.x == 1 { 9 } else { 13 }) } else { None },
            0x80 | 0x81 | 0x83 | 0x84 | 0x89 | 0x8A | 0x8C | 0x8D | 0x90 | 0x91 | 0x93 | 0x94 | 0x96 | 0x97
            | 0x99 | 0x9A | 0x9C | 0x9D | 0xA0 | 0xA1 | 0xAE | 0xB0 | 0xB2 | 0xC2 => Some(4),
            0x86 | 0x87 | 0xCA | 0xCC | 0xCD => Some(5),
            0xC4 => Some(7),
            0xC6 | 0xC8 => Some(6),
            0xCE => Some(8),
            0xB1 => {
                if !matches!(self.a, 0x00 | 0x0A | 0xA0 | 0xFF) { return None; }
                let low = ((self.a & 0x0F) > 9) as u32;
                let high = (self.a > 0x99) as u32;
                let lc = if cc & CC_H != 0 { 3 } else { low };
                let hc = if cc & CC_C != 0 { 3 } else { high };
                return Some(14 - lc - hc);
            }
            0xB3 => Some(16),
            0xB4 => Some(23),
            0xB5 => return Some(if self.b != 0 { 25 + (x_now & 1) as u32 } else { 26 - ((self.x >> 15) & 1) as u32 }),
            0xB6 => return if self.u != 0 { Some(7 + 8 * self.u as u32) } else { None },
            0xB7 => Some(14),
            0xCF => return if self.u != 0 { Some(7 + 4 * self.u as u32) } else { None },
            0xD0 => return if self.u != 0 { Some(7 + 8 * self.u as u32) } else { None },
            0x8F => Some(14),
            0x9F => return Some(if cc_now & CC_E != 0 { 63 } else { 21 }),
            0x39 | 0x3A | 0x3B => return if pb == 0x07 { Some(22) } else { None },
            0xA8 => return if pb == 0x07 { Some(17) } else { None },
            0xA9 => return if pb == 0x07 { Some(27) } else { None },
            0x82 => return if pb == 0x07 { Some(22) } else { None },
            0x85 | 0x88 | 0x8B | 0x8E | 0x95 | 0x98 | 0x9B | 0x9E => return if pb == 0x07 { Some(25) } else { None },
            0xA2 => return if pb == 0x07 { Some(26) } else { None },
            0xA3 | 0xA4 | 0xA5 => return if pb == 0x07 { Some(35) } else { None },
            0xA6 => return if pb == 0x07 { Some(34) } else { None },
            0xA7 => return if pb == 0x07 { Some(33) } else { None },
            0xC3 => return if pb == 0x07 { Some(26) } else { None },
            0xC5 | 0xC7 | 0xC9 => return if pb == 0x07 { Some(33) } else { None },
            0xCB => return if pb == 0x07 { Some(25) } else { None },
            0xB8 | 0xBA | 0xBC | 0xBE | 0xC0 => {
                let n = (pb & 0x0F) as u32;
                return Some(match op { 0xB8 | 0xBA => 15 + n, 0xBC => 17 + n, _ => 30 - n });
            }
            0xB9 | 0xBB | 0xBD | 0xBF | 0xC1 => {
                if pb != 0x07 { return None; }
                let n = (self.a & 0x0F) as u32;
                return Some(match op { 0xB9 | 0xBB => 40 + n, 0xBD => 42 + n, _ => 55 - n });
            }
            0xD1 => return match pb { 0x07 => Some(22), 0xC4 => Some(20), 0x26 => Some(17), _ => None },
            _ => None,
        };
        if fixed.is_some() { return fixed; }

        if pb == 0x07 {
            if (0x12..=0x23).contains(&op) { return Some(21); }
            if (0x26..=0x2F).contains(&op) || (0x32..=0x33).contains(&op) { return Some(22); }
            if (0x36..=0x3B).contains(&op) { return Some(if op == 0x36 || op == 0x37 { 21 } else { 22 }); }
            if (0x41..=0x53).contains(&op) { return if op & 1 != 0 { Some(25) } else { None }; }
            if op == 0x55 || op == 0x57 { return Some(25); }
            if (0x58..=0x5C).contains(&op) { return Some(26); }
        }

        if (0x61..=0x67).contains(&op) || (0x71..=0x77).contains(&op) || (0x69..=0x6F).contains(&op) || (0x79..=0x7F).contains(&op) {
            let long = op & 0x08 != 0;
            if if long { pb != 0 || self.op1 != 0 } else { pb != 0 } { return None; }
            let c = cc & CC_C != 0;
            let v = cc & CC_V != 0;
            let z = cc & CC_Z != 0;
            let n = cc & CC_N != 0;
            let mut taken = match op & 7 {
                1 => !c && !z, 2 => !c, 3 => !z, 4 => !v, 5 => !n, 6 => n == v, 7 => !z && n == v, _ => false,
            };
            if op & 0x10 != 0 { taken = !taken; }
            return Some(if long {
                if op >= 0x79 {
                    if taken { match op { 0x79 => 13, 0x7E => 14, 0x7F => 16, _ => 13 } }
                    else { match op { 0x79 => 10, 0x7E => 10, 0x7F => 12, _ => 8 } }
                } else if taken { match op { 0x69 => 15, 0x6E => 15, 0x6F => 17, _ => 13 } }
                else { match op { 0x69 => 7, 0x6E => 9, 0x6F => 11, _ => 7 } }
            } else if op >= 0x71 {
                if taken { match op { 0x71 => 11, 0x76 => 12, 0x77 => 14, _ => 11 } }
                else { match op { 0x71 => 9, 0x76 => 9, 0x77 => 11, _ => 7 } }
            } else if taken { match op { 0x61 => 13, 0x66 => 13, 0x67 => 15, _ => 11 } }
            else { match op { 0x61 => 7, 0x66 => 9, 0x67 => 11, _ => 7 } });
        }
        None
    }
}

#[cfg(test)]
#[path = "konami_tests.rs"]
mod tests;
