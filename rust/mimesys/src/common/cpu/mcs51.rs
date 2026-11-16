//! Intel MCS-51 (8051 / 8751): the microcontroller arcade boards used for protection.
//! Written from Intel's MCS-51 instruction set and SFR descriptions.
//!
//! - 4 KB of code (the board's ROM region), 128 bytes of internal RAM (256 for `ram_size`
//!   256, an 8052), the special function registers at 80-FF.
//! - Ports P0-P3 go to the board: a read gets the pins (`Mcs51Bus::port_in` ANDed with the
//!   latch, as a quasi-bidirectional port reads), read-modify-write instructions get the
//!   latch; writes go out through `port_out`.
//! - MOVX reaches the board's external data space (`xdata_read` / `xdata_write`): @DPTR
//!   with the 16-bit DPTR, @Ri with P2 as the high byte.
//! - Timers 0 and 1 (modes 0-3, counting machine cycles, GATE on the INT pins) and the
//!   five interrupts with two priority levels; the serial port isn't emulated.
//!
//! `step()` runs one instruction (or interrupt entry) and returns machine cycles (each
//! 12 oscillator clocks).

use serde::{Deserialize, Serialize};

pub trait Mcs51Bus {
    fn xdata_read(&mut self, addr: u16) -> u8;
    fn xdata_write(&mut self, addr: u16, v: u8);
    /// What the board drives on port `n`'s pins (0xFF: nothing pulls low).
    fn port_in(&mut self, _n: usize) -> u8 { 0xFF }
    fn port_out(&mut self, _n: usize, _v: u8) {}
}

const P0: u8 = 0x80;
const SP: u8 = 0x81;
const DPL: u8 = 0x82;
const DPH: u8 = 0x83;
const TCON: u8 = 0x88;
const TMOD: u8 = 0x89;
const TL0: u8 = 0x8A;
const TL1: u8 = 0x8B;
const TH0: u8 = 0x8C;
const TH1: u8 = 0x8D;
const P1: u8 = 0x90;
const P2: u8 = 0xA0;
const IE: u8 = 0xA8;
const P3: u8 = 0xB0;
const IP: u8 = 0xB8;
const PSW: u8 = 0xD0;
const ACC: u8 = 0xE0;
const B: u8 = 0xF0;

const CY: u8 = 0x80;
const AC: u8 = 0x40;
const OV: u8 = 0x04;

#[derive(Clone, Serialize, Deserialize)]
pub struct Mcs51 {
    pub pc: u16,
    pub iram: Vec<u8>,
    /// SFRs 80-FF (index - 0x80)
    pub sfr: Vec<u8>,
    #[serde(skip)]
    pub rom: Vec<u8>,
    ram_size: usize,
    /// interrupts being serviced, by level (bit 0 low, bit 1 high)
    in_service: u8,
    /// the INT0 / INT1 pins (true = low = asserted), and their last level for edges
    int_pin: [bool; 2],
    /// set by RETI / writes to IE / IP: one more instruction before an interrupt
    hold_irq: bool,
}

impl Mcs51 {
    pub fn new(rom: Vec<u8>) -> Self {
        let mut c = Self { pc: 0, iram: vec![0; 256], sfr: vec![0; 128], rom, ram_size: 128, in_service: 0,
                           int_pin: [false; 2], hold_irq: false };
        c.reset();
        c
    }

    pub fn reset(&mut self) {
        self.pc = 0;
        self.sfr.fill(0);
        for p in [P0, P1, P2, P3] { self.sfr[(p - 0x80) as usize] = 0xFF; }
        self.sfr[(SP - 0x80) as usize] = 0x07;
        self.in_service = 0;
        self.hold_irq = false;
    }

    fn s(&self, a: u8) -> u8 { self.sfr[(a & 0x7F) as usize] }
    fn s_mut(&mut self, a: u8) -> &mut u8 { &mut self.sfr[(a & 0x7F) as usize] }

    pub fn acc(&self) -> u8 { self.s(ACC) }
    fn set_acc(&mut self, v: u8) { *self.s_mut(ACC) = v; }
    fn psw(&self) -> u8 { self.s(PSW) }
    fn cy(&self) -> bool { self.psw() & CY != 0 }
    fn set_cy(&mut self, c: bool) { let p = self.s_mut(PSW); if c { *p |= CY } else { *p &= !CY } }
    fn dptr(&self) -> u16 { (self.s(DPH) as u16) << 8 | self.s(DPL) as u16 }
    fn set_dptr(&mut self, v: u16) { *self.s_mut(DPH) = (v >> 8) as u8; *self.s_mut(DPL) = v as u8; }
    /// A port's latch (what the program last wrote).
    pub fn port_latch(&self, n: usize) -> u8 { self.s(0x80 + 0x10 * n as u8) }

    /// The INT0 / INT1 input (`asserted` = pulled low). Edge-triggered mode latches IEx on
    /// the falling edge; level-triggered follows the pin.
    pub fn set_int(&mut self, n: usize, asserted: bool) {
        let edge_mode = self.s(TCON) & (1 << (2 * n)) != 0;
        let flag = 2 << (2 * n);
        if edge_mode {
            if asserted && !self.int_pin[n] { *self.s_mut(TCON) |= flag; }
        } else if asserted { *self.s_mut(TCON) |= flag; } else { *self.s_mut(TCON) &= !flag; }
        self.int_pin[n] = asserted;
    }

    fn reg_addr(&self, n: u8) -> u8 { (self.psw() & 0x18) + (n & 7) }
    fn r(&self, n: u8) -> u8 { self.iram[self.reg_addr(n) as usize] }
    fn set_r(&mut self, n: u8, v: u8) { let a = self.reg_addr(n) as usize; self.iram[a] = v; }

    fn fetch(&mut self) -> u8 {
        let v = self.rom.get(self.pc as usize).copied().unwrap_or(0xFF);
        self.pc = self.pc.wrapping_add(1);
        v
    }
    fn code(&self, a: u16) -> u8 { self.rom.get(a as usize).copied().unwrap_or(0xFF) }

    /// Direct address read; `rmw`: a read-modify-write instruction (ports read the latch).
    fn read_dir(&mut self, bus: &mut dyn Mcs51Bus, a: u8, rmw: bool) -> u8 {
        if a < 0x80 { return self.iram[a as usize]; }
        match a {
            P0 | P1 | P2 | P3 if !rmw => {
                let n = ((a - 0x80) >> 4) as usize;
                let mut pins = bus.port_in(n);
                if a == P3 {
                    // INT0 / INT1 pins
                    if self.int_pin[0] { pins &= !0x04; }
                    if self.int_pin[1] { pins &= !0x08; }
                }
                self.s(a) & pins
            }
            PSW => self.psw_with_parity(),
            _ => self.s(a),
        }
    }

    fn write_dir(&mut self, bus: &mut dyn Mcs51Bus, a: u8, v: u8) {
        if a < 0x80 { self.iram[a as usize] = v; return; }
        *self.s_mut(a) = v;
        match a {
            P0 | P1 | P2 | P3 => bus.port_out(((a - 0x80) >> 4) as usize, v),
            IE | IP => self.hold_irq = true,
            _ => {}
        }
    }

    fn psw_with_parity(&self) -> u8 { (self.psw() & !1) | (self.acc().count_ones() as u8 & 1) }

    fn ind(&self, a: u8) -> usize { (a as usize) & (self.ram_size - 1) }
    fn read_ind(&self, a: u8) -> u8 { self.iram[self.ind(a)] }
    fn write_ind(&mut self, a: u8, v: u8) { let i = self.ind(a); self.iram[i] = v; }

    fn bit_addr(b: u8) -> (u8, u8) {
        if b < 0x80 { (0x20 + (b >> 3), b & 7) } else { (b & 0xF8, b & 7) }
    }
    fn read_bit(&mut self, bus: &mut dyn Mcs51Bus, b: u8, rmw: bool) -> bool {
        let (a, n) = Self::bit_addr(b);
        self.read_dir(bus, a, rmw) >> n & 1 != 0
    }
    fn write_bit(&mut self, bus: &mut dyn Mcs51Bus, b: u8, v: bool) {
        let (a, n) = Self::bit_addr(b);
        let old = self.read_dir(bus, a, true);
        let new = if v { old | 1 << n } else { old & !(1 << n) };
        self.write_dir(bus, a, new);
    }

    fn push(&mut self, v: u8) {
        let sp = self.s(SP).wrapping_add(1);
        *self.s_mut(SP) = sp;
        self.write_ind(sp, v);
    }
    fn pop(&mut self) -> u8 {
        let sp = self.s(SP);
        *self.s_mut(SP) = sp.wrapping_sub(1);
        self.read_ind(sp)
    }
    fn call(&mut self, to: u16) {
        let pc = self.pc;
        self.push(pc as u8);
        self.push((pc >> 8) as u8);
        self.pc = to;
    }
    fn rel(&mut self, cond: bool, off: u8) {
        if cond { self.pc = self.pc.wrapping_add(off as i8 as i16 as u16); }
    }

    fn add(&mut self, v: u8, carry: bool) {
        let a = self.acc();
        let c = carry as u16;
        let r = a as u16 + v as u16 + c;
        let ac = (a & 15) + (v & 15) + c as u8 > 15;
        let ov = (!(a ^ v) & (a ^ r as u8)) & 0x80 != 0;
        self.set_acc(r as u8);
        self.set_flags(r > 0xFF, ac, ov);
    }
    fn subb(&mut self, v: u8) {
        let a = self.acc();
        let c = self.cy() as i16;
        let r = a as i16 - v as i16 - c;
        let ac = (a & 15) as i16 - (v & 15) as i16 - c < 0;
        let ov = ((a ^ v) & (a ^ r as u8)) & 0x80 != 0;
        self.set_acc(r as u8);
        self.set_flags(r < 0, ac, ov);
    }
    fn set_flags(&mut self, cy: bool, ac: bool, ov: bool) {
        let p = self.s_mut(PSW);
        *p &= !(CY | AC | OV);
        if cy { *p |= CY; }
        if ac { *p |= AC; }
        if ov { *p |= OV; }
    }

    /// Operand of the 0x?5 / 0x?6-7 / 0x?8-F forms.
    fn src(&mut self, bus: &mut dyn Mcs51Bus, op: u8, rmw: bool) -> u8 {
        match op & 15 {
            4 => self.fetch(),
            5 => { let a = self.fetch(); self.read_dir(bus, a, rmw) }
            6 | 7 => { let r = self.r(op & 1); self.read_ind(r) }
            _ => self.r(op & 7),
        }
    }

    /// Advance the timers by `cycles` machine cycles.
    fn timers(&mut self, cycles: u32) {
        let tmod = self.s(TMOD);
        let tcon = self.s(TCON);
        for _ in 0..cycles {
            for t in 0..2 {
                let mode = (tmod >> (4 * t)) & 0xF;
                let run = tcon & (0x10 << (2 * t)) != 0;
                let gate = mode & 8 != 0;
                if !run || (gate && self.int_pin[t]) || mode & 4 != 0 { continue; }
                let (tl, th) = if t == 0 { (TL0, TH0) } else { (TL1, TH1) };
                let tf = 0x20 << (2 * t);
                match mode & 3 {
                    0 => {
                        let l = (self.s(tl) & 0x1F) + 1;
                        *self.s_mut(tl) = (self.s(tl) & 0xE0) | (l & 0x1F);
                        if l > 0x1F {
                            let h = self.s(th).wrapping_add(1);
                            *self.s_mut(th) = h;
                            if h == 0 { *self.s_mut(TCON) |= tf; }
                        }
                    }
                    1 => {
                        let v = ((self.s(th) as u16) << 8 | self.s(tl) as u16).wrapping_add(1);
                        *self.s_mut(tl) = v as u8;
                        *self.s_mut(th) = (v >> 8) as u8;
                        if v == 0 { *self.s_mut(TCON) |= tf; }
                    }
                    2 => {
                        let l = self.s(tl).wrapping_add(1);
                        *self.s_mut(tl) = if l == 0 { self.s(th) } else { l };
                        if l == 0 { *self.s_mut(TCON) |= tf; }
                    }
                    _ => {
                        // mode 3: timer 0 splits in two 8-bit timers; timer 1 stops
                        if t == 0 {
                            let l = self.s(TL0).wrapping_add(1);
                            *self.s_mut(TL0) = l;
                            if l == 0 { *self.s_mut(TCON) |= 0x20; }
                            if tcon & 0x40 != 0 {
                                let h = self.s(TH0).wrapping_add(1);
                                *self.s_mut(TH0) = h;
                                if h == 0 { *self.s_mut(TCON) |= 0x80; }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Take a pending interrupt, if one can be.
    fn interrupt(&mut self) -> bool {
        let ie = self.s(IE);
        if ie & 0x80 == 0 { return false; }
        let tcon = self.s(TCON);
        let ip = self.s(IP);
        // (enable bit, flag test, vector) in polling order
        let pending = [
            (0x01, tcon & 0x02 != 0, 0x03u16),
            (0x02, tcon & 0x20 != 0, 0x0B),
            (0x04, tcon & 0x08 != 0, 0x13),
            (0x08, tcon & 0x80 != 0, 0x1B),
            (0x10, self.s(0x98) & 3 != 0, 0x23),
        ];
        for high in [true, false] {
            let level = if high { 2 } else { 1 };
            // blocked by one of the same or a higher level
            if self.in_service & 2 != 0 || (!high && self.in_service != 0) { continue; }
            for (k, &(en, flag, vec)) in pending.iter().enumerate() {
                if ie & en == 0 || !flag || (ip & en != 0) != high { continue; }
                // the hardware clears edge-triggered external flags and the timer flags
                match k {
                    0 if tcon & 0x01 != 0 => *self.s_mut(TCON) &= !0x02,
                    1 => *self.s_mut(TCON) &= !0x20,
                    2 if tcon & 0x04 != 0 => *self.s_mut(TCON) &= !0x08,
                    3 => *self.s_mut(TCON) &= !0x80,
                    _ => {}
                }
                self.in_service |= level;
                self.call(vec);
                return true;
            }
        }
        false
    }

    pub fn step(&mut self, bus: &mut dyn Mcs51Bus) -> u32 {
        if !self.hold_irq && self.interrupt() {
            self.timers(2);
            return 2;
        }
        self.hold_irq = false;
        let op = self.fetch();
        let c = self.execute(bus, op);
        self.timers(c);
        c
    }

    fn execute(&mut self, bus: &mut dyn Mcs51Bus, op: u8) -> u32 {
        match op {
            0x00 => 1,
            _ if op & 0x1F == 0x01 => {
                let lo = self.fetch();
                self.pc = (self.pc & 0xF800) | ((op as u16 >> 5) << 8) | lo as u16;
                2
            }
            _ if op & 0x1F == 0x11 => {
                let lo = self.fetch();
                let to = (self.pc & 0xF800) | ((op as u16 >> 5) << 8) | lo as u16;
                self.call(to);
                2
            }
            0x02 => { let h = self.fetch(); let l = self.fetch(); self.pc = (h as u16) << 8 | l as u16; 2 }
            0x12 => { let h = self.fetch(); let l = self.fetch(); self.call((h as u16) << 8 | l as u16); 2 }
            0x03 => { let a = self.acc(); self.set_acc(a.rotate_right(1)); 1 }
            0x13 => { let a = self.acc(); let c = self.cy(); self.set_cy(a & 1 != 0); self.set_acc(a >> 1 | (c as u8) << 7); 1 }
            0x23 => { let a = self.acc(); self.set_acc(a.rotate_left(1)); 1 }
            0x33 => { let a = self.acc(); let c = self.cy(); self.set_cy(a & 0x80 != 0); self.set_acc(a << 1 | c as u8); 1 }
            0x04 => { let a = self.acc(); self.set_acc(a.wrapping_add(1)); 1 }
            0x05 => { let a = self.fetch(); let v = self.read_dir(bus, a, true); self.write_dir(bus, a, v.wrapping_add(1)); 1 }
            0x06 | 0x07 => { let r = self.r(op & 1); let v = self.read_ind(r); self.write_ind(r, v.wrapping_add(1)); 1 }
            0x08..=0x0F => { let v = self.r(op & 7); self.set_r(op & 7, v.wrapping_add(1)); 1 }
            0x14 => { let a = self.acc(); self.set_acc(a.wrapping_sub(1)); 1 }
            0x15 => { let a = self.fetch(); let v = self.read_dir(bus, a, true); self.write_dir(bus, a, v.wrapping_sub(1)); 1 }
            0x16 | 0x17 => { let r = self.r(op & 1); let v = self.read_ind(r); self.write_ind(r, v.wrapping_sub(1)); 1 }
            0x18..=0x1F => { let v = self.r(op & 7); self.set_r(op & 7, v.wrapping_sub(1)); 1 }
            0x10 => {
                let b = self.fetch();
                let off = self.fetch();
                let v = self.read_bit(bus, b, true);
                if v { self.write_bit(bus, b, false); }
                self.rel(v, off);
                2
            }
            0x20 | 0x30 => {
                let b = self.fetch();
                let off = self.fetch();
                let v = self.read_bit(bus, b, false);
                self.rel(v == (op == 0x20), off);
                2
            }
            0x22 => { let h = self.pop(); let l = self.pop(); self.pc = (h as u16) << 8 | l as u16; 2 }
            0x32 => {
                let h = self.pop();
                let l = self.pop();
                self.pc = (h as u16) << 8 | l as u16;
                // leave the highest level in service
                if self.in_service & 2 != 0 { self.in_service &= !2; } else { self.in_service = 0; }
                self.hold_irq = true;
                2
            }
            0x24..=0x2F => { let v = self.src(bus, op, false); self.add(v, false); 1 }
            0x34..=0x3F => { let v = self.src(bus, op, false); let c = self.cy(); self.add(v, c); 1 }
            0x40 | 0x50 => { let off = self.fetch(); let c = self.cy(); self.rel(c == (op == 0x40), off); 2 }
            0x60 | 0x70 => { let off = self.fetch(); let z = self.acc() == 0; self.rel(z == (op == 0x60), off); 2 }
            // ORL / ANL / XRL
            0x42 | 0x43 | 0x52 | 0x53 | 0x62 | 0x63 => {
                let a = self.fetch();
                let v = if op & 1 == 0 { self.acc() } else { self.fetch() };
                let old = self.read_dir(bus, a, true);
                let r = match op >> 4 { 4 => old | v, 5 => old & v, _ => old ^ v };
                self.write_dir(bus, a, r);
                if op & 1 == 0 { 1 } else { 2 }
            }
            0x44..=0x4F | 0x54..=0x5F | 0x64..=0x6F => {
                let v = self.src(bus, op, false);
                let a = self.acc();
                self.set_acc(match op >> 4 { 4 => a | v, 5 => a & v, _ => a ^ v });
                1
            }
            0x72 | 0x82 | 0xA0 | 0xB0 => {
                let b = self.fetch();
                let v = self.read_bit(bus, b, false);
                let c = self.cy();
                self.set_cy(match op { 0x72 => c | v, 0x82 => c & v, 0xA0 => c | !v, _ => c & !v });
                2
            }
            0x73 => { self.pc = (self.acc() as u16).wrapping_add(self.dptr()); 2 }
            0x74 => { let v = self.fetch(); self.set_acc(v); 1 }
            0x75 => { let a = self.fetch(); let v = self.fetch(); self.write_dir(bus, a, v); 2 }
            0x76 | 0x77 => { let v = self.fetch(); let r = self.r(op & 1); self.write_ind(r, v); 1 }
            0x78..=0x7F => { let v = self.fetch(); self.set_r(op & 7, v); 1 }
            0x80 => { let off = self.fetch(); self.rel(true, off); 2 }
            0x83 => { let a = self.pc.wrapping_add(self.acc() as u16); let v = self.code(a); self.set_acc(v); 2 }
            0x93 => { let a = self.dptr().wrapping_add(self.acc() as u16); let v = self.code(a); self.set_acc(v); 2 }
            0x84 => {
                let (a, b) = (self.acc(), self.s(B));
                if b == 0 { self.set_flags(false, self.psw() & AC != 0, true); } else {
                    self.set_acc(a / b);
                    *self.s_mut(B) = a % b;
                    self.set_flags(false, self.psw() & AC != 0, false);
                }
                4
            }
            0xA4 => {
                let p = self.acc() as u16 * self.s(B) as u16;
                self.set_acc(p as u8);
                *self.s_mut(B) = (p >> 8) as u8;
                self.set_flags(false, self.psw() & AC != 0, p > 0xFF);
                4
            }
            0x85 => { let s = self.fetch(); let d = self.fetch(); let v = self.read_dir(bus, s, false); self.write_dir(bus, d, v); 2 }
            0x86 | 0x87 => { let d = self.fetch(); let r = self.r(op & 1); let v = self.read_ind(r); self.write_dir(bus, d, v); 2 }
            0x88..=0x8F => { let d = self.fetch(); let v = self.r(op & 7); self.write_dir(bus, d, v); 2 }
            0x90 => { let h = self.fetch(); let l = self.fetch(); self.set_dptr((h as u16) << 8 | l as u16); 2 }
            0x92 => { let b = self.fetch(); let c = self.cy(); self.write_bit(bus, b, c); 2 }
            0x94..=0x9F => { let v = self.src(bus, op, false); self.subb(v); 1 }
            0xA2 => { let b = self.fetch(); let v = self.read_bit(bus, b, false); self.set_cy(v); 1 }
            0xA3 => { let d = self.dptr().wrapping_add(1); self.set_dptr(d); 2 }
            0xA5 => 1,
            0xA6 | 0xA7 => { let s = self.fetch(); let v = self.read_dir(bus, s, false); let r = self.r(op & 1); self.write_ind(r, v); 2 }
            0xA8..=0xAF => { let s = self.fetch(); let v = self.read_dir(bus, s, false); self.set_r(op & 7, v); 2 }
            0xB2 => { let b = self.fetch(); let v = self.read_bit(bus, b, true); self.write_bit(bus, b, !v); 1 }
            0xB3 => { let c = self.cy(); self.set_cy(!c); 1 }
            0xB4..=0xBF => {
                let (x, y) = match op {
                    0xB4 => { let v = self.fetch(); (self.acc(), v) }
                    0xB5 => { let a = self.fetch(); let v = self.read_dir(bus, a, false); (self.acc(), v) }
                    0xB6 | 0xB7 => { let r = self.r(op & 1); let v = self.fetch(); (self.read_ind(r), v) }
                    _ => { let v = self.fetch(); (self.r(op & 7), v) }
                };
                let off = self.fetch();
                self.set_cy(x < y);
                self.rel(x != y, off);
                2
            }
            0xC0 => { let a = self.fetch(); let v = self.read_dir(bus, a, false); self.push(v); 2 }
            0xD0 => { let a = self.fetch(); let v = self.pop(); self.write_dir(bus, a, v); 2 }
            0xC2 => { let b = self.fetch(); self.write_bit(bus, b, false); 1 }
            0xC3 => { self.set_cy(false); 1 }
            0xC4 => { let a = self.acc(); self.set_acc(a.rotate_left(4)); 1 }
            0xC5 => { let a = self.fetch(); let v = self.read_dir(bus, a, true); let acc = self.acc(); self.write_dir(bus, a, acc); self.set_acc(v); 1 }
            0xC6 | 0xC7 => { let r = self.r(op & 1); let v = self.read_ind(r); let acc = self.acc(); self.write_ind(r, acc); self.set_acc(v); 1 }
            0xC8..=0xCF => { let v = self.r(op & 7); let acc = self.acc(); self.set_r(op & 7, acc); self.set_acc(v); 1 }
            0xD2 => { let b = self.fetch(); self.write_bit(bus, b, true); 1 }
            0xD3 => { self.set_cy(true); 1 }
            0xD4 => {
                let mut a = self.acc() as u16;
                if a & 15 > 9 || self.psw() & AC != 0 { a += 6; }
                let mut cy = self.cy() || a > 0xFF;
                if (a & 0x1F0) > 0x90 || cy { a += 0x60; }
                if a > 0xFF { cy = true; }
                self.set_acc(a as u8);
                self.set_cy(cy);
                1
            }
            0xD5 => {
                let a = self.fetch();
                let off = self.fetch();
                let v = self.read_dir(bus, a, true).wrapping_sub(1);
                self.write_dir(bus, a, v);
                self.rel(v != 0, off);
                2
            }
            0xD6 | 0xD7 => {
                let r = self.r(op & 1);
                let v = self.read_ind(r);
                let a = self.acc();
                self.write_ind(r, (v & 0xF0) | (a & 15));
                self.set_acc((a & 0xF0) | (v & 15));
                1
            }
            0xD8..=0xDF => { let off = self.fetch(); let v = self.r(op & 7).wrapping_sub(1); self.set_r(op & 7, v); self.rel(v != 0, off); 2 }
            0xE0 => { let v = bus.xdata_read(self.dptr()); self.set_acc(v); 2 }
            0xE2 | 0xE3 => { let a = (self.s(P2) as u16) << 8 | self.r(op & 1) as u16; let v = bus.xdata_read(a); self.set_acc(v); 2 }
            0xE4 => { self.set_acc(0); 1 }
            0xE5 => { let a = self.fetch(); let v = self.read_dir(bus, a, false); self.set_acc(v); 1 }
            0xE6 | 0xE7 => { let r = self.r(op & 1); let v = self.read_ind(r); self.set_acc(v); 1 }
            0xE8..=0xEF => { let v = self.r(op & 7); self.set_acc(v); 1 }
            0xF0 => { let a = self.acc(); bus.xdata_write(self.dptr(), a); 2 }
            0xF2 | 0xF3 => { let ad = (self.s(P2) as u16) << 8 | self.r(op & 1) as u16; let a = self.acc(); bus.xdata_write(ad, a); 2 }
            0xF4 => { let a = self.acc(); self.set_acc(!a); 1 }
            0xF5 => { let a = self.fetch(); let v = self.acc(); self.write_dir(bus, a, v); 1 }
            0xF6 | 0xF7 => { let r = self.r(op & 1); let v = self.acc(); self.write_ind(r, v); 1 }
            0xF8..=0xFF => { let v = self.acc(); self.set_r(op & 7, v); 1 }
            _ => 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestBus { xram: Vec<u8>, p1_in: u8, out: Vec<(usize, u8)> }
    impl Mcs51Bus for TestBus {
        fn xdata_read(&mut self, a: u16) -> u8 { self.xram[a as usize & 0xFFF] }
        fn xdata_write(&mut self, a: u16, v: u8) { self.xram[a as usize & 0xFFF] = v; }
        fn port_in(&mut self, n: usize) -> u8 { if n == 1 { self.p1_in } else { 0xFF } }
        fn port_out(&mut self, n: usize, v: u8) { self.out.push((n, v)); }
    }

    fn run(prog: &[u8], steps: usize) -> (Mcs51, TestBus) {
        let mut rom = vec![0u8; 0x1000];
        rom[..prog.len()].copy_from_slice(prog);
        let mut c = Mcs51::new(rom);
        let mut b = TestBus { xram: vec![0; 0x1000], p1_in: 0x5A, out: Vec::new() };
        for _ in 0..steps { c.step(&mut b); }
        (c, b)
    }

    /// Arithmetic and flags: ADD's carry / auxiliary carry / overflow, SUBB's borrow, DA A,
    /// MUL, DIV.
    #[test]
    fn arithmetic() {
        // MOV A,#0x7F; ADD A,#1 -> 0x80, OV, AC, no CY
        let (c, _) = run(&[0x74, 0x7F, 0x24, 0x01], 2);
        assert_eq!(c.acc(), 0x80);
        assert_eq!(c.psw() & (CY | AC | OV), AC | OV);
        // MOV A,#0x10; CLR C; SUBB A,#0x20 -> 0xF0, borrow
        let (c, _) = run(&[0x74, 0x10, 0xC3, 0x94, 0x20], 3);
        assert_eq!((c.acc(), c.cy()), (0xF0, true));
        // BCD: MOV A,#0x19; ADD A,#0x28; DA A -> 0x47
        let (c, _) = run(&[0x74, 0x19, 0x24, 0x28, 0xD4], 3);
        assert_eq!(c.acc(), 0x47);
        // MOV A,#0x80; MOV B,#3; MUL AB -> B:A = 0x0180, OV
        let (c, _) = run(&[0x74, 0x80, 0x75, 0xF0, 0x03, 0xA4], 3);
        assert_eq!((c.s(B), c.acc(), c.psw() & OV), (0x01, 0x80, OV));
        // MOV A,#200; MOV B,#7; DIV AB -> 28 r 4
        let (c, _) = run(&[0x74, 200, 0x75, 0xF0, 7, 0x84], 3);
        assert_eq!((c.acc(), c.s(B)), (28, 4));
    }

    /// Register banks, indirect addressing, bit addressing, DJNZ / CJNE loops, calls.
    #[test]
    fn registers_bits_loops_calls() {
        let prog = [
            0xD2, 0xD3,             // SETB PSW.3 (bank 1)
            0x78, 0x30,             // MOV R0,#30
            0x7A, 0x05,             // MOV R2,#5
            0x74, 0x00,             // MOV A,#0
            0x24, 0x03,             // loop: ADD A,#3
            0xF6,                   // MOV @R0,A
            0x08,                   // INC R0
            0xDA, 0xFA,             // DJNZ R2,loop
            0xD2, 0x07,             // SETB 20.7
            0x12, 0x00, 0x20,       // LCALL 0020
            0x80, 0xFE,             // SJMP $
        ];
        let mut p = prog.to_vec();
        p.resize(0x20, 0);
        p.extend([0x75, 0x40, 0x99, 0x22]); // 0020: MOV 40,#99; RET
        let (c, _) = run(&p, 30);
        assert_eq!(&c.iram[0x30..0x35], &[3, 6, 9, 12, 15]);
        assert_eq!(c.iram[0x08 + 2], 0, "R2 of bank 1 counted down");
        assert_eq!(c.iram[0x20], 0x80, "bit 07 = 20.7");
        assert_eq!(c.iram[0x40], 0x99, "the call ran and came back");
        assert_eq!(c.pc, 0x13, "back at SJMP $");
    }

    /// MOVX through DPTR and through P2:Ri; ports read the pins, write out.
    #[test]
    fn movx_and_ports() {
        let prog = [
            0x90, 0x01, 0x23,       // MOV DPTR,#0123
            0x74, 0xAB, 0xF0,       // MOV A,#AB; MOVX @DPTR,A
            0x75, 0xA0, 0x02,       // MOV P2,#02
            0x78, 0x10,             // MOV R0,#10
            0xE2,                   // MOVX A,@R0 (0210)
            0xF5, 0x31,             // MOV 31,A
            0xE5, 0x90,             // MOV A,P1 (pins)
            0xF5, 0x32,             // MOV 32,A
        ];
        let mut rom = vec![0u8; 0x1000];
        rom[..prog.len()].copy_from_slice(&prog);
        let mut c = Mcs51::new(rom);
        let mut b = TestBus { xram: vec![0; 0x1000], p1_in: 0x5A, out: Vec::new() };
        b.xram[0x210] = 0x77;
        for _ in 0..9 { c.step(&mut b); }
        assert_eq!(b.xram[0x123], 0xAB);
        assert_eq!(c.iram[0x31], 0x77);
        assert_eq!(c.iram[0x32], 0x5A, "P1 pins");
        assert!(b.out.contains(&(2, 0x02)));
    }

    /// INT0 (edge-triggered) and timer 0 (mode 2) interrupts, RETI.
    #[test]
    fn interrupts_and_timer() {
        let mut p = vec![0u8; 0x50];
        let main = [
            0x75, 0x89, 0x02,       // MOV TMOD,#02 (timer 0 mode 2)
            0x75, 0x8C, 0xF0,       // MOV TH0,#F0 (16-cycle period)
            0x75, 0x8A, 0xF0,       // MOV TL0,#F0
            0xD2, 0x88,             // SETB IT0 (edge)
            0x75, 0xA8, 0x83,       // MOV IE,#83 (EA, ET0, EX0)
            0xD2, 0x8C,             // SETB TR0
            0x80, 0xFE,             // SJMP $
        ];
        p[0x30..0x30 + main.len()].copy_from_slice(&main);
        p[0..3].copy_from_slice(&[0x02, 0x00, 0x30]);        // LJMP 0030
        p[0x03..0x06].copy_from_slice(&[0x05, 0x50, 0x32]);  // INT0: INC 50; RETI
        p[0x0B..0x0E].copy_from_slice(&[0x05, 0x51, 0x32]);  // T0: INC 51; RETI
        let mut rom = vec![0u8; 0x1000];
        rom[..p.len()].copy_from_slice(&p);
        let mut c = Mcs51::new(rom);
        let mut b = TestBus { xram: vec![0; 0x1000], p1_in: 0xFF, out: Vec::new() };
        let mut cycles = 0;
        while cycles < 1600 { cycles += c.step(&mut b); }
        let t0 = c.iram[0x51];
        assert!((90..=100).contains(&t0), "timer 0 every 16 cycles: {t0}");
        c.set_int(0, true);
        for _ in 0..10 { c.step(&mut b); }
        c.set_int(0, false);
        c.set_int(0, true);
        for _ in 0..10 { c.step(&mut b); }
        assert_eq!(c.iram[0x50], 2, "one INT0 per falling edge");
    }
}
