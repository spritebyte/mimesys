//! Sony SPC700, the SNES sound CPU. A port of ares's `component/processor/spc700`
//! (ISC, Near and the ares team; see THIRD_PARTY_NOTICES.md), rewritten in Rust.
//!
//! Every bus access and idle cycle goes through `SpcBus`, one call per CPU cycle, so the
//! owner can count time (the SMP adds wait states and clocks its timers from these).

use serde::{Deserialize, Serialize};

pub trait SpcBus {
    fn read(&mut self, addr: u16) -> u8;
    fn write(&mut self, addr: u16, v: u8);
    fn idle(&mut self);
}

pub const C: u8 = 0x01;
pub const Z: u8 = 0x02;
pub const I: u8 = 0x04;
pub const H: u8 = 0x08;
pub const B: u8 = 0x10;
pub const P: u8 = 0x20;
pub const V: u8 = 0x40;
pub const N: u8 = 0x80;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Spc700 {
    pub pc: u16,
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub s: u8,
    pub p: u8,
    pub wait: bool,
    pub stop: bool,
}

#[derive(Clone, Copy)]
enum R { A, X, Y, S }

type Alu = fn(&mut Spc700, u8, u8) -> u8;
type Alu1 = fn(&mut Spc700, u8) -> u8;
type AluW = fn(&mut Spc700, u16, u16) -> u16;

impl Spc700 {
    pub fn new() -> Self { Self { s: 0xEF, p: Z, ..Default::default() } }

    fn flag(&mut self, f: u8, on: bool) { if on { self.p |= f } else { self.p &= !f } }
    fn fl(&self, f: u8) -> bool { self.p & f != 0 }
    fn zn(&mut self, v: u8) { self.flag(Z, v == 0); self.flag(N, v & 0x80 != 0); }
    fn get(&self, r: R) -> u8 { match r { R::A => self.a, R::X => self.x, R::Y => self.y, R::S => self.s } }
    fn set(&mut self, r: R, v: u8) { match r { R::A => self.a = v, R::X => self.x = v, R::Y => self.y = v, R::S => self.s = v } }
    fn ya(&self) -> u16 { (self.y as u16) << 8 | self.a as u16 }
    fn set_ya(&mut self, v: u16) { self.a = v as u8; self.y = (v >> 8) as u8; }

    // ---- memory
    fn fetch<Bu: SpcBus>(&mut self, b: &mut Bu) -> u8 { let v = b.read(self.pc); self.pc = self.pc.wrapping_add(1); v }
    fn fetch16<Bu: SpcBus>(&mut self, b: &mut Bu) -> u16 { let lo = self.fetch(b) as u16; lo | (self.fetch(b) as u16) << 8 }
    fn page(&self) -> u16 { ((self.p & P) as u16) << 3 }
    fn load<Bu: SpcBus>(&mut self, b: &mut Bu, a: u8) -> u8 { b.read(self.page() | a as u16) }
    fn store<Bu: SpcBus>(&mut self, b: &mut Bu, a: u8, v: u8) { b.write(self.page() | a as u16, v) }
    fn pull<Bu: SpcBus>(&mut self, b: &mut Bu) -> u8 { self.s = self.s.wrapping_add(1); b.read(0x100 | self.s as u16) }
    fn push<Bu: SpcBus>(&mut self, b: &mut Bu, v: u8) { b.write(0x100 | self.s as u16, v); self.s = self.s.wrapping_sub(1); }
    fn dummy<Bu: SpcBus>(&mut self, b: &mut Bu) { b.read(self.pc); }

    // ---- algorithms
    fn adc(&mut self, x: u8, y: u8) -> u8 {
        let z = x as u32 + y as u32 + self.fl(C) as u32;
        self.flag(C, z > 0xFF);
        self.flag(H, (x as u32 ^ y as u32 ^ z) & 0x10 != 0);
        self.flag(V, !(x ^ y) & (x ^ z as u8) & 0x80 != 0);
        self.zn(z as u8);
        z as u8
    }
    fn and(&mut self, x: u8, y: u8) -> u8 { let v = x & y; self.zn(v); v }
    fn or(&mut self, x: u8, y: u8) -> u8 { let v = x | y; self.zn(v); v }
    fn eor(&mut self, x: u8, y: u8) -> u8 { let v = x ^ y; self.zn(v); v }
    fn ld(&mut self, _x: u8, y: u8) -> u8 { self.zn(y); y }
    fn cmp(&mut self, x: u8, y: u8) -> u8 {
        let z = x as i32 - y as i32;
        self.flag(C, z >= 0);
        self.zn(z as u8);
        x
    }
    fn sbc(&mut self, x: u8, y: u8) -> u8 { self.adc(x, !y) }
    fn asl(&mut self, x: u8) -> u8 { self.flag(C, x & 0x80 != 0); let v = x << 1; self.zn(v); v }
    fn lsr(&mut self, x: u8) -> u8 { self.flag(C, x & 1 != 0); let v = x >> 1; self.zn(v); v }
    fn rol(&mut self, x: u8) -> u8 { let c = self.fl(C) as u8; self.flag(C, x & 0x80 != 0); let v = x << 1 | c; self.zn(v); v }
    fn ror(&mut self, x: u8) -> u8 { let c = self.fl(C) as u8; self.flag(C, x & 1 != 0); let v = c << 7 | x >> 1; self.zn(v); v }
    fn inc(&mut self, x: u8) -> u8 { let v = x.wrapping_add(1); self.zn(v); v }
    fn dec(&mut self, x: u8) -> u8 { let v = x.wrapping_sub(1); self.zn(v); v }
    fn adw(&mut self, x: u16, y: u16) -> u16 {
        self.flag(C, false);
        let lo = self.adc(x as u8, y as u8) as u16;
        let z = lo | (self.adc((x >> 8) as u8, (y >> 8) as u8) as u16) << 8;
        self.flag(Z, z == 0);
        z
    }
    fn sbw(&mut self, x: u16, y: u16) -> u16 {
        self.flag(C, true);
        let lo = self.sbc(x as u8, y as u8) as u16;
        let z = lo | (self.sbc((x >> 8) as u8, (y >> 8) as u8) as u16) << 8;
        self.flag(Z, z == 0);
        z
    }
    fn cpw(&mut self, x: u16, y: u16) -> u16 {
        let z = x as i32 - y as i32;
        self.flag(C, z >= 0);
        self.flag(Z, z as u16 == 0);
        self.flag(N, z & 0x8000 != 0);
        x
    }
    fn ldw(&mut self, _x: u16, y: u16) -> u16 { self.flag(Z, y == 0); self.flag(N, y & 0x8000 != 0); y }

    // ---- addressing helpers (names follow ares)
    fn abs_bit_modify<Bu: SpcBus>(&mut self, b: &mut Bu, mode: u8) {
        let raw = self.fetch16(b);
        let bit = (raw >> 13) as u8;
        let addr = raw & 0x1FFF;
        let mut data = b.read(addr);
        let bv = data >> bit & 1 != 0;
        let c = self.fl(C);
        match mode {
            0 => { b.idle(); self.flag(C, c | bv); }
            1 => { b.idle(); self.flag(C, c | !bv); }
            2 => self.flag(C, c & bv),
            3 => self.flag(C, c & !bv),
            4 => { b.idle(); self.flag(C, c ^ bv); }
            5 => self.flag(C, bv),
            6 => { b.idle(); data = data & !(1 << bit) | (c as u8) << bit; b.write(addr, data); }
            _ => { data ^= 1 << bit; b.write(addr, data); }
        }
    }
    fn abs_bit_set<Bu: SpcBus>(&mut self, b: &mut Bu, bit: u8, v: bool) {
        let a = self.fetch(b);
        let d = self.load(b, a);
        self.store(b, a, if v { d | 1 << bit } else { d & !(1 << bit) });
    }
    fn abs_read<Bu: SpcBus>(&mut self, b: &mut Bu, op: Alu, r: R) {
        let a = self.fetch16(b);
        let d = b.read(a);
        let v = op(self, self.get(r), d);
        self.set(r, v);
    }
    fn abs_modify<Bu: SpcBus>(&mut self, b: &mut Bu, op: Alu1) {
        let a = self.fetch16(b);
        let d = b.read(a);
        let v = op(self, d);
        b.write(a, v);
    }
    fn abs_write<Bu: SpcBus>(&mut self, b: &mut Bu, r: R) {
        let a = self.fetch16(b);
        b.read(a);
        b.write(a, self.get(r));
    }
    fn abs_indexed_read<Bu: SpcBus>(&mut self, b: &mut Bu, op: Alu, idx: R) {
        let a = self.fetch16(b);
        b.idle();
        let d = b.read(a.wrapping_add(self.get(idx) as u16));
        self.a = op(self, self.a, d);
    }
    fn abs_indexed_write<Bu: SpcBus>(&mut self, b: &mut Bu, idx: R) {
        let a = self.fetch16(b).wrapping_add(self.get(idx) as u16);
        b.idle();
        b.read(a);
        b.write(a, self.a);
    }
    fn branch_to<Bu: SpcBus>(&mut self, b: &mut Bu, d: u8) {
        b.idle();
        b.idle();
        self.pc = self.pc.wrapping_add(d as i8 as u16);
    }
    fn branch<Bu: SpcBus>(&mut self, b: &mut Bu, take: bool) {
        let d = self.fetch(b);
        if take { self.branch_to(b, d); }
    }
    fn branch_bit<Bu: SpcBus>(&mut self, b: &mut Bu, bit: u8, m: bool) {
        let a = self.fetch(b);
        let d = self.load(b, a);
        b.idle();
        let disp = self.fetch(b);
        if (d >> bit & 1 != 0) == m { self.branch_to(b, disp); }
    }
    fn call_table<Bu: SpcBus>(&mut self, b: &mut Bu, n: u16) {
        self.dummy(b);
        b.idle();
        self.push(b, (self.pc >> 8) as u8);
        self.push(b, self.pc as u8);
        b.idle();
        let a = 0xFFDE - (n << 1);
        let lo = b.read(a) as u16;
        self.pc = lo | (b.read(a + 1) as u16) << 8;
    }
    fn direct_read<Bu: SpcBus>(&mut self, b: &mut Bu, op: Alu, r: R) {
        let a = self.fetch(b);
        let d = self.load(b, a);
        let v = op(self, self.get(r), d);
        self.set(r, v);
    }
    fn direct_modify<Bu: SpcBus>(&mut self, b: &mut Bu, op: Alu1) {
        let a = self.fetch(b);
        let d = self.load(b, a);
        let v = op(self, d);
        self.store(b, a, v);
    }
    fn direct_write<Bu: SpcBus>(&mut self, b: &mut Bu, r: R) {
        let a = self.fetch(b);
        self.load(b, a);
        self.store(b, a, self.get(r));
    }
    fn dd_compare<Bu: SpcBus>(&mut self, b: &mut Bu, op: Alu) {
        let s = self.fetch(b);
        let rhs = self.load(b, s);
        let t = self.fetch(b);
        let lhs = self.load(b, t);
        op(self, lhs, rhs);
        b.idle();
    }
    fn dd_modify<Bu: SpcBus>(&mut self, b: &mut Bu, op: Alu) {
        let s = self.fetch(b);
        let rhs = self.load(b, s);
        let t = self.fetch(b);
        let lhs = self.load(b, t);
        let v = op(self, lhs, rhs);
        self.store(b, t, v);
    }
    fn di_compare<Bu: SpcBus>(&mut self, b: &mut Bu, op: Alu) {
        let imm = self.fetch(b);
        let a = self.fetch(b);
        let d = self.load(b, a);
        op(self, d, imm);
        b.idle();
    }
    fn di_modify<Bu: SpcBus>(&mut self, b: &mut Bu, op: Alu) {
        let imm = self.fetch(b);
        let a = self.fetch(b);
        let d = self.load(b, a);
        let v = op(self, d, imm);
        self.store(b, a, v);
    }
    fn direct_read_word<Bu: SpcBus>(&mut self, b: &mut Bu, op: AluW, idle: bool) {
        let a = self.fetch(b);
        let lo = self.load(b, a) as u16;
        if idle { b.idle(); }
        let d = lo | (self.load(b, a.wrapping_add(1)) as u16) << 8;
        let v = op(self, self.ya(), d);
        self.set_ya(v);
    }
    fn direct_modify_word<Bu: SpcBus>(&mut self, b: &mut Bu, adj: i32) {
        let a = self.fetch(b);
        let mut d = (self.load(b, a) as i32 + adj) as u16;
        self.store(b, a, d as u8);
        d = d.wrapping_add((self.load(b, a.wrapping_add(1)) as u16) << 8);
        self.store(b, a.wrapping_add(1), (d >> 8) as u8);
        self.flag(Z, d == 0);
        self.flag(N, d & 0x8000 != 0);
    }
    fn direct_indexed_read<Bu: SpcBus>(&mut self, b: &mut Bu, op: Alu, r: R, idx: R) {
        let a = self.fetch(b);
        b.idle();
        let d = self.load(b, a.wrapping_add(self.get(idx)));
        let v = op(self, self.get(r), d);
        self.set(r, v);
    }
    fn direct_indexed_modify<Bu: SpcBus>(&mut self, b: &mut Bu, op: Alu1, idx: R) {
        let a = self.fetch(b).wrapping_add(self.get(idx));
        b.idle();
        let d = self.load(b, a);
        let v = op(self, d);
        self.store(b, a, v);
    }
    fn direct_indexed_write<Bu: SpcBus>(&mut self, b: &mut Bu, r: R, idx: R) {
        let a = self.fetch(b).wrapping_add(self.get(idx));
        b.idle();
        self.load(b, a);
        self.store(b, a, self.get(r));
    }
    fn ptr<Bu: SpcBus>(&mut self, b: &mut Bu, a: u8) -> u16 {
        let lo = self.load(b, a) as u16;
        lo | (self.load(b, a.wrapping_add(1)) as u16) << 8
    }
    fn indexed_indirect_read<Bu: SpcBus>(&mut self, b: &mut Bu, op: Alu) {
        let i = self.fetch(b).wrapping_add(self.x);
        b.idle();
        let a = self.ptr(b, i);
        let d = b.read(a);
        self.a = op(self, self.a, d);
    }
    fn indexed_indirect_write<Bu: SpcBus>(&mut self, b: &mut Bu) {
        let i = self.fetch(b).wrapping_add(self.x);
        b.idle();
        let a = self.ptr(b, i);
        b.read(a);
        b.write(a, self.a);
    }
    fn indirect_indexed_read<Bu: SpcBus>(&mut self, b: &mut Bu, op: Alu) {
        let i = self.fetch(b);
        b.idle();
        let a = self.ptr(b, i).wrapping_add(self.y as u16);
        let d = b.read(a);
        self.a = op(self, self.a, d);
    }
    fn indirect_indexed_write<Bu: SpcBus>(&mut self, b: &mut Bu) {
        let i = self.fetch(b);
        let a = self.ptr(b, i).wrapping_add(self.y as u16);
        b.idle();
        b.read(a);
        b.write(a, self.a);
    }
    fn indirect_x_read<Bu: SpcBus>(&mut self, b: &mut Bu, op: Alu) {
        self.dummy(b);
        let d = self.load(b, self.x);
        self.a = op(self, self.a, d);
    }
    fn xy_compare<Bu: SpcBus>(&mut self, b: &mut Bu, op: Alu) {
        self.dummy(b);
        let rhs = self.load(b, self.y);
        let lhs = self.load(b, self.x);
        op(self, lhs, rhs);
        b.idle();
    }
    fn xy_modify<Bu: SpcBus>(&mut self, b: &mut Bu, op: Alu) {
        self.dummy(b);
        let rhs = self.load(b, self.y);
        let lhs = self.load(b, self.x);
        let v = op(self, lhs, rhs);
        self.store(b, self.x, v);
    }
    fn implied<Bu: SpcBus>(&mut self, b: &mut Bu, op: Alu1, r: R) {
        self.dummy(b);
        let v = op(self, self.get(r));
        self.set(r, v);
    }
    fn transfer<Bu: SpcBus>(&mut self, b: &mut Bu, from: R, to: R) {
        self.dummy(b);
        let v = self.get(from);
        self.set(to, v);
        if !matches!(to, R::S) { self.zn(v); }
    }
    fn flag_set<Bu: SpcBus>(&mut self, b: &mut Bu, f: u8, v: bool) {
        self.dummy(b);
        if f == I { b.idle(); }
        self.flag(f, v);
    }
    fn push_op<Bu: SpcBus>(&mut self, b: &mut Bu, v: u8) { self.dummy(b); self.push(b, v); b.idle(); }
    fn pull_op<Bu: SpcBus>(&mut self, b: &mut Bu) -> u8 { self.dummy(b); b.idle(); self.pull(b) }
    fn tset<Bu: SpcBus>(&mut self, b: &mut Bu, set: bool) {
        let a = self.fetch16(b);
        let d = b.read(a);
        let t = self.a.wrapping_sub(d);
        self.zn(t);
        b.read(a);
        b.write(a, if set { d | self.a } else { d & !self.a });
    }

    /// Run one instruction (or one 2-cycle slice of SLEEP / STOP).
    pub fn step<Bu: SpcBus>(&mut self, b: &mut Bu) {
        if self.wait || self.stop { self.dummy(b); b.idle(); return; }
        use R::*;
        let op = self.fetch(b);
        match op {
            0x00 => self.dummy(b),
            0x01 | 0x11 | 0x21 | 0x31 | 0x41 | 0x51 | 0x61 | 0x71 | 0x81 | 0x91 | 0xA1 | 0xB1 | 0xC1 | 0xD1 | 0xE1 | 0xF1 =>
                self.call_table(b, (op >> 4) as u16),
            0x02 | 0x22 | 0x42 | 0x62 | 0x82 | 0xA2 | 0xC2 | 0xE2 => self.abs_bit_set(b, op >> 5, true),
            0x12 | 0x32 | 0x52 | 0x72 | 0x92 | 0xB2 | 0xD2 | 0xF2 => self.abs_bit_set(b, op >> 5, false),
            0x03 | 0x23 | 0x43 | 0x63 | 0x83 | 0xA3 | 0xC3 | 0xE3 => self.branch_bit(b, op >> 5, true),
            0x13 | 0x33 | 0x53 | 0x73 | 0x93 | 0xB3 | 0xD3 | 0xF3 => self.branch_bit(b, op >> 5, false),
            0x0A | 0x2A | 0x4A | 0x6A | 0x8A | 0xAA | 0xCA | 0xEA => self.abs_bit_modify(b, op >> 5),

            // the eight ALU groups: x4 dp, x5 abs, x6 (X), x7 [dp+X], x8 #, x9 dp,dp,
            // x14 dp+X, x15 abs+X, x16 abs+Y, x17 [dp]+Y, x18 dp,#, x19 (X),(Y)
            0x04 | 0x24 | 0x44 | 0x64 | 0x84 | 0xA4 | 0xE4 => self.direct_read(b, alu(op), A),
            0x05 | 0x25 | 0x45 | 0x65 | 0x85 | 0xA5 | 0xE5 => self.abs_read(b, alu(op), A),
            0x06 | 0x26 | 0x46 | 0x66 | 0x86 | 0xA6 | 0xE6 => self.indirect_x_read(b, alu(op)),
            0x07 | 0x27 | 0x47 | 0x67 | 0x87 | 0xA7 | 0xE7 => self.indexed_indirect_read(b, alu(op)),
            0x08 | 0x28 | 0x48 | 0x68 | 0x88 | 0xA8 | 0xE8 => { let d = self.fetch(b); self.a = alu(op)(self, self.a, d); }
            0x09 | 0x29 | 0x49 | 0x89 | 0xA9 => self.dd_modify(b, alu(op)),
            0x69 => self.dd_compare(b, Spc700::cmp),
            0x14 | 0x34 | 0x54 | 0x74 | 0x94 | 0xB4 | 0xF4 => self.direct_indexed_read(b, alu(op), A, X),
            0x15 | 0x35 | 0x55 | 0x75 | 0x95 | 0xB5 | 0xF5 => self.abs_indexed_read(b, alu(op), X),
            0x16 | 0x36 | 0x56 | 0x76 | 0x96 | 0xB6 | 0xF6 => self.abs_indexed_read(b, alu(op), Y),
            0x17 | 0x37 | 0x57 | 0x77 | 0x97 | 0xB7 | 0xF7 => self.indirect_indexed_read(b, alu(op)),
            0x18 | 0x38 | 0x58 | 0x98 | 0xB8 => self.di_modify(b, alu(op)),
            0x78 => self.di_compare(b, Spc700::cmp),
            0x19 | 0x39 | 0x59 | 0x99 | 0xB9 => self.xy_modify(b, alu(op)),
            0x79 => self.xy_compare(b, Spc700::cmp),

            0x0B => self.direct_modify(b, Spc700::asl),
            0x2B => self.direct_modify(b, Spc700::rol),
            0x4B => self.direct_modify(b, Spc700::lsr),
            0x6B => self.direct_modify(b, Spc700::ror),
            0x8B => self.direct_modify(b, Spc700::dec),
            0xAB => self.direct_modify(b, Spc700::inc),
            0x0C => self.abs_modify(b, Spc700::asl),
            0x2C => self.abs_modify(b, Spc700::rol),
            0x4C => self.abs_modify(b, Spc700::lsr),
            0x6C => self.abs_modify(b, Spc700::ror),
            0x8C => self.abs_modify(b, Spc700::dec),
            0xAC => self.abs_modify(b, Spc700::inc),
            0x1B => self.direct_indexed_modify(b, Spc700::asl, X),
            0x3B => self.direct_indexed_modify(b, Spc700::rol, X),
            0x5B => self.direct_indexed_modify(b, Spc700::lsr, X),
            0x7B => self.direct_indexed_modify(b, Spc700::ror, X),
            0x9B => self.direct_indexed_modify(b, Spc700::dec, X),
            0xBB => self.direct_indexed_modify(b, Spc700::inc, X),
            0x1C => self.implied(b, Spc700::asl, A),
            0x3C => self.implied(b, Spc700::rol, A),
            0x5C => self.implied(b, Spc700::lsr, A),
            0x7C => self.implied(b, Spc700::ror, A),
            0x9C => self.implied(b, Spc700::dec, A),
            0xBC => self.implied(b, Spc700::inc, A),
            0x1D => self.implied(b, Spc700::dec, X),
            0x3D => self.implied(b, Spc700::inc, X),
            0xDC => self.implied(b, Spc700::dec, Y),
            0xFC => self.implied(b, Spc700::inc, Y),

            0x0D => self.push_op(b, self.p),
            0x2D => self.push_op(b, self.a),
            0x4D => self.push_op(b, self.x),
            0x6D => self.push_op(b, self.y),
            0x8E => self.p = self.pull_op(b),
            0xAE => self.a = self.pull_op(b),
            0xCE => self.x = self.pull_op(b),
            0xEE => self.y = self.pull_op(b),

            0x0E => self.tset(b, true),
            0x4E => self.tset(b, false),
            0x0F => {
                self.dummy(b);
                self.push(b, (self.pc >> 8) as u8);
                self.push(b, self.pc as u8);
                self.push(b, self.p);
                b.idle();
                let lo = b.read(0xFFDE) as u16;
                self.pc = lo | (b.read(0xFFDF) as u16) << 8;
                self.flag(I, false);
                self.flag(B, true);
            }
            0x10 => self.branch(b, !self.fl(N)),
            0x30 => self.branch(b, self.fl(N)),
            0x50 => self.branch(b, !self.fl(V)),
            0x70 => self.branch(b, self.fl(V)),
            0x90 => self.branch(b, !self.fl(C)),
            0xB0 => self.branch(b, self.fl(C)),
            0xD0 => self.branch(b, !self.fl(Z)),
            0xF0 => self.branch(b, self.fl(Z)),
            0x2F => self.branch(b, true),

            0x1A => self.direct_modify_word(b, -1),
            0x3A => self.direct_modify_word(b, 1),
            0x5A => self.direct_read_word(b, Spc700::cpw, false),
            0x7A => self.direct_read_word(b, Spc700::adw, true),
            0x9A => self.direct_read_word(b, Spc700::sbw, true),
            0xBA => self.direct_read_word(b, Spc700::ldw, true),
            0xDA => {
                let a = self.fetch(b);
                self.load(b, a);
                self.store(b, a, self.a);
                self.store(b, a.wrapping_add(1), self.y);
            }

            0x1E => self.abs_read(b, Spc700::cmp, X),
            0x3E => self.direct_read(b, Spc700::cmp, X),
            0x5E => self.abs_read(b, Spc700::cmp, Y),
            0x7E => self.direct_read(b, Spc700::cmp, Y),
            0xAD => { let d = self.fetch(b); self.cmp(self.y, d); }
            0xC8 => { let d = self.fetch(b); self.cmp(self.x, d); }
            0x8D => { let d = self.fetch(b); self.y = self.ld(0, d); }
            0xCD => { let d = self.fetch(b); self.x = self.ld(0, d); }
            0xE9 => self.abs_read(b, Spc700::ld, X),
            0xEC => self.abs_read(b, Spc700::ld, Y),
            0xEB => self.direct_read(b, Spc700::ld, Y),
            0xF8 => self.direct_read(b, Spc700::ld, X),
            0xF9 => self.direct_indexed_read(b, Spc700::ld, X, Y),
            0xFB => self.direct_indexed_read(b, Spc700::ld, Y, X),

            0x1F => {
                let a = self.fetch16(b);
                b.idle();
                let a = a.wrapping_add(self.x as u16);
                let lo = b.read(a) as u16;
                self.pc = lo | (b.read(a.wrapping_add(1)) as u16) << 8;
            }
            0x5F => self.pc = self.fetch16(b),
            0x3F => {
                let a = self.fetch16(b);
                b.idle();
                self.push(b, (self.pc >> 8) as u8);
                self.push(b, self.pc as u8);
                b.idle();
                b.idle();
                self.pc = a;
            }
            0x4F => {
                let a = self.fetch(b);
                b.idle();
                self.push(b, (self.pc >> 8) as u8);
                self.push(b, self.pc as u8);
                b.idle();
                self.pc = 0xFF00 | a as u16;
            }
            0x6F => {
                self.dummy(b);
                b.idle();
                let lo = self.pull(b) as u16;
                self.pc = lo | (self.pull(b) as u16) << 8;
            }
            0x7F => {
                self.dummy(b);
                b.idle();
                self.p = self.pull(b);
                let lo = self.pull(b) as u16;
                self.pc = lo | (self.pull(b) as u16) << 8;
            }

            0x20 => self.flag_set(b, P, false),
            0x40 => self.flag_set(b, P, true),
            0x60 => self.flag_set(b, C, false),
            0x80 => self.flag_set(b, C, true),
            0xA0 => self.flag_set(b, I, true),
            0xC0 => self.flag_set(b, I, false),
            0xE0 => { self.dummy(b); self.flag(H, false); self.flag(V, false); }
            0xED => { self.dummy(b); b.idle(); let c = self.fl(C); self.flag(C, !c); }

            0x2E => {
                let a = self.fetch(b);
                let d = self.load(b, a);
                b.idle();
                let disp = self.fetch(b);
                if self.a != d { self.branch_to(b, disp); }
            }
            0xDE => {
                let a = self.fetch(b);
                b.idle();
                let d = self.load(b, a.wrapping_add(self.x));
                b.idle();
                let disp = self.fetch(b);
                if self.a != d { self.branch_to(b, disp); }
            }
            0x6E => {
                let a = self.fetch(b);
                let d = self.load(b, a).wrapping_sub(1);
                self.store(b, a, d);
                let disp = self.fetch(b);
                if d != 0 { self.branch_to(b, disp); }
            }
            0xFE => {
                self.dummy(b);
                b.idle();
                let disp = self.fetch(b);
                self.y = self.y.wrapping_sub(1);
                if self.y != 0 { self.branch_to(b, disp); }
            }

            0x5D => self.transfer(b, A, X),
            0x7D => self.transfer(b, X, A),
            0x9D => self.transfer(b, S, X),
            0xBD => self.transfer(b, X, S),
            0xDD => self.transfer(b, Y, A),
            0xFD => self.transfer(b, A, Y),

            0x8F => {
                let imm = self.fetch(b);
                let a = self.fetch(b);
                self.load(b, a);
                self.store(b, a, imm);
            }
            0xFA => {
                let s = self.fetch(b);
                let d = self.load(b, s);
                let t = self.fetch(b);
                self.store(b, t, d);
            }

            0xC4 => self.direct_write(b, A),
            0xCB => self.direct_write(b, Y),
            0xD8 => self.direct_write(b, X),
            0xC5 => self.abs_write(b, A),
            0xC9 => self.abs_write(b, X),
            0xCC => self.abs_write(b, Y),
            0xC6 => { self.dummy(b); self.load(b, self.x); self.store(b, self.x, self.a); }
            0xC7 => self.indexed_indirect_write(b),
            0xD4 => self.direct_indexed_write(b, A, X),
            0xD5 => self.abs_indexed_write(b, X),
            0xD6 => self.abs_indexed_write(b, Y),
            0xD7 => self.indirect_indexed_write(b),
            0xD9 => self.direct_indexed_write(b, X, Y),
            0xDB => self.direct_indexed_write(b, Y, X),
            0xAF => { self.dummy(b); b.idle(); self.store(b, self.x, self.a); self.x = self.x.wrapping_add(1); }
            0xBF => {
                self.dummy(b);
                self.a = self.load(b, self.x);
                self.x = self.x.wrapping_add(1);
                b.idle();
                self.zn(self.a);
            }

            0x9E => {
                self.dummy(b);
                for _ in 0..10 { b.idle(); }
                let ya = self.ya() as u32;
                let (x, y) = (self.x as u32, self.y as u32);
                self.flag(H, (y & 15) >= (x & 15));
                self.flag(V, y >= x);
                if y < x << 1 {
                    self.a = (ya / x) as u8;
                    self.y = (ya % x) as u8;
                } else {
                    self.a = (255 - (ya - (x << 9)) / (256 - x)) as u8;
                    self.y = (x + (ya - (x << 9)) % (256 - x)) as u8;
                }
                self.zn(self.a);
            }
            0xCF => {
                self.dummy(b);
                for _ in 0..7 { b.idle(); }
                let ya = self.y as u16 * self.a as u16;
                self.set_ya(ya);
                self.zn(self.y);
            }
            0x9F => {
                self.dummy(b);
                b.idle(); b.idle(); b.idle();
                self.a = self.a.rotate_left(4);
                self.zn(self.a);
            }
            0xDF => {
                self.dummy(b);
                b.idle();
                if self.fl(C) || self.a > 0x99 { self.a = self.a.wrapping_add(0x60); self.flag(C, true); }
                if self.fl(H) || self.a & 15 > 9 { self.a = self.a.wrapping_add(6); }
                self.zn(self.a);
            }
            0xBE => {
                self.dummy(b);
                b.idle();
                if !self.fl(C) || self.a > 0x99 { self.a = self.a.wrapping_sub(0x60); self.flag(C, false); }
                if !self.fl(H) || self.a & 15 > 9 { self.a = self.a.wrapping_sub(6); }
                self.zn(self.a);
            }
            0xEF => { self.wait = true; self.dummy(b); b.idle(); }
            0xFF => { self.stop = true; self.dummy(b); b.idle(); }
        }
    }
}

/// The ALU for an opcode in one of the eight read groups (by its top three bits).
fn alu(op: u8) -> Alu {
    match op >> 5 {
        0 => Spc700::or,
        1 => Spc700::and,
        2 => Spc700::eor,
        3 => Spc700::cmp,
        4 => Spc700::adc,
        5 => Spc700::sbc,
        _ => Spc700::ld,     // 7 (E4..F7); 6 is the store group, never asked
    }
}

#[cfg(test)]
#[path = "spc700_tests.rs"]
mod tests;
