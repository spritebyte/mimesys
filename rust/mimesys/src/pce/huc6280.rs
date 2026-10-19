// Hudson HuC6280: the PC Engine / TurboGrafx-16 CPU.
// A 65C02 core at 7.16 MHz (or 1.79 MHz) with extras built in:
//   - an MMU: 8 mapping registers (MPR0-7), each maps an 8 KB slice of the 64 KB logical
//     space to one of 256 8 KB banks of a 2 MB physical space (TAM / TMA)
//   - zero page lives at logical 0x2000 and the stack at 0x2100 (normally bank 0xF8 = RAM)
//   - block-copy instructions (TII, TDD, TIN, TIA, TAI)
//   - ST0/ST1/ST2: store immediate straight to the video chip (physical 0x1FE000/2/3)
//   - the T flag (SET): the next ORA/AND/EOR/ADC works on zero-page[X] instead of A
//   - CSL/CSH: switch between 1.79 and 7.16 MHz
// The timer, interrupt controller, sound and I/O are on the chip too; they live in the bus.
//
// Cycle counts follow the HuC6280's own table (every memory access costs one more cycle than
// on a 6502, and there are no page-crossing penalties). step() returns cycles at 7.16 MHz,
// so slow-mode instructions count 4x.
//
// Reference: "HuC6280 CMOS 8-bit Microprocessor Hardware Manual" (Hudson), Mednafen pce,
// "pcetech.txt" (Charles MacDonald)

use serde::{Deserialize, Serialize};

const C: u8 = 0x01;
const Z: u8 = 0x02;
const I: u8 = 0x04;
const D: u8 = 0x08;
const B: u8 = 0x10;
const T: u8 = 0x20;
const V: u8 = 0x40;
const N: u8 = 0x80;

pub const IRQ2: u8 = 0x01;    // external (CD-ROM) / also the BRK vector
pub const IRQ1: u8 = 0x02;    // VDC
pub const TIMER: u8 = 0x04;

pub trait PceBus {
    /// 21-bit physical address
    fn read(&mut self, phys: u32) -> u8;
    fn write(&mut self, phys: u32, v: u8);
    /// Interrupt lines that are asserted and not disabled (IRQ2 | IRQ1 | TIMER bits).
    fn irq_pending(&mut self) -> u8;
}

#[derive(Clone, Copy)]
enum Mode { Zp, ZpX, ZpY, Abs, AbsX, AbsY, IzX, IzY, Izp }

impl Mode {
    /// Cycles for a plain read or write using this mode.
    fn cycles(self) -> u32 {
        match self {
            Mode::Zp | Mode::ZpX | Mode::ZpY => 4,
            Mode::Abs | Mode::AbsX | Mode::AbsY => 5,
            Mode::IzX | Mode::IzY | Mode::Izp => 7,
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Huc6280 {
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub s: u8,
    pub p: u8,
    pub pc: u16,
    pub mpr: [u8; 8],
    pub fast: bool,
}

impl Default for Huc6280 {
    fn default() -> Self { Self::new() }
}

impl Huc6280 {
    pub fn new() -> Self {
        Self { a: 0, x: 0, y: 0, s: 0xFF, p: I, pc: 0, mpr: [0xFF, 0xF8, 0, 0, 0, 0, 0, 0], fast: false }
    }

    pub fn reset(&mut self, bus: &mut dyn PceBus) {
        self.mpr[7] = 0;              // only MPR7 is defined at reset: bank 0 holds the vectors
        self.p = I;
        self.fast = false;
        self.pc = self.rd16(bus, 0xFFFE);
    }

    // ---------------- memory ----------------

    fn phys(&self, addr: u16) -> u32 {
        ((self.mpr[(addr >> 13) as usize] as u32) << 13) | (addr & 0x1FFF) as u32
    }
    fn rd(&self, bus: &mut dyn PceBus, addr: u16) -> u8 { bus.read(self.phys(addr)) }
    fn wr(&self, bus: &mut dyn PceBus, addr: u16, v: u8) { bus.write(self.phys(addr), v) }
    fn rd16(&self, bus: &mut dyn PceBus, addr: u16) -> u16 {
        self.rd(bus, addr) as u16 | (self.rd(bus, addr.wrapping_add(1)) as u16) << 8
    }
    fn zp_rd16(&self, bus: &mut dyn PceBus, zp: u8) -> u16 {
        self.rd(bus, 0x2000 | zp as u16) as u16 | (self.rd(bus, 0x2000 | zp.wrapping_add(1) as u16) as u16) << 8
    }
    fn fetch(&mut self, bus: &mut dyn PceBus) -> u8 {
        let v = self.rd(bus, self.pc);
        self.pc = self.pc.wrapping_add(1);
        v
    }
    fn fetch16(&mut self, bus: &mut dyn PceBus) -> u16 {
        let lo = self.fetch(bus) as u16;
        lo | (self.fetch(bus) as u16) << 8
    }
    fn push(&mut self, bus: &mut dyn PceBus, v: u8) {
        self.wr(bus, 0x2100 | self.s as u16, v);
        self.s = self.s.wrapping_sub(1);
    }
    fn pull(&mut self, bus: &mut dyn PceBus) -> u8 {
        self.s = self.s.wrapping_add(1);
        self.rd(bus, 0x2100 | self.s as u16)
    }

    fn ea(&mut self, bus: &mut dyn PceBus, mode: Mode) -> u16 {
        match mode {
            Mode::Zp => 0x2000 | self.fetch(bus) as u16,
            Mode::ZpX => 0x2000 | self.fetch(bus).wrapping_add(self.x) as u16,
            Mode::ZpY => 0x2000 | self.fetch(bus).wrapping_add(self.y) as u16,
            Mode::Abs => self.fetch16(bus),
            Mode::AbsX => self.fetch16(bus).wrapping_add(self.x as u16),
            Mode::AbsY => self.fetch16(bus).wrapping_add(self.y as u16),
            Mode::IzX => { let zp = self.fetch(bus).wrapping_add(self.x); self.zp_rd16(bus, zp) }
            Mode::IzY => { let zp = self.fetch(bus); self.zp_rd16(bus, zp).wrapping_add(self.y as u16) }
            Mode::Izp => { let zp = self.fetch(bus); self.zp_rd16(bus, zp) }
        }
    }

    // ---------------- flags & ALU ----------------

    fn set_flag(&mut self, f: u8, on: bool) { if on { self.p |= f } else { self.p &= !f } }
    fn nz(&mut self, v: u8) -> u8 { self.set_flag(Z, v == 0); self.set_flag(N, v & 0x80 != 0); v }

    fn adc(&mut self, a: u8, v: u8) -> u8 {
        let c = (self.p & C) as u16;
        if self.p & D != 0 {
            let mut lo = (a & 0x0F) as u16 + (v & 0x0F) as u16 + c;
            let mut hi = (a >> 4) as u16 + (v >> 4) as u16;
            if lo > 9 { lo += 6; }
            if lo > 0x0F { hi += 1; }
            if hi > 9 { hi += 6; }
            self.set_flag(C, hi > 0x0F);
            self.nz(((hi << 4) | (lo & 0x0F)) as u8)
        } else {
            let sum = a as u16 + v as u16 + c;
            let r = sum as u8;
            self.set_flag(V, (!(a ^ v) & (a ^ r) & 0x80) != 0);
            self.set_flag(C, sum > 0xFF);
            self.nz(r)
        }
    }

    fn sbc(&mut self, a: u8, v: u8) -> u8 {
        if self.p & D != 0 {
            let borrow: i16 = if self.p & C != 0 { 0 } else { 1 };
            let binary = a as i16 - v as i16 - borrow;
            let mut lo = (a & 0x0F) as i16 - (v & 0x0F) as i16 - borrow;
            let mut hi = (a >> 4) as i16 - (v >> 4) as i16;
            if lo < 0 { lo -= 6; hi -= 1; }
            if hi < 0 { hi -= 6; }
            self.set_flag(C, binary >= 0);
            self.nz((((hi & 0x0F) << 4) | (lo & 0x0F)) as u8)
        } else {
            self.adc(a, !v)
        }
    }

    fn cmp(&mut self, r: u8, v: u8) {
        self.set_flag(C, r >= v);
        self.nz(r.wrapping_sub(v));
    }

    fn bit(&mut self, v: u8) {
        self.set_flag(Z, self.a & v == 0);
        self.set_flag(N, v & 0x80 != 0);
        self.set_flag(V, v & 0x40 != 0);
    }

    fn asl(&mut self, v: u8) -> u8 { self.set_flag(C, v & 0x80 != 0); self.nz(v << 1) }
    fn lsr(&mut self, v: u8) -> u8 { self.set_flag(C, v & 1 != 0); self.nz(v >> 1) }
    fn rol(&mut self, v: u8) -> u8 { let c = self.p & C; self.set_flag(C, v & 0x80 != 0); self.nz((v << 1) | c) }
    fn ror(&mut self, v: u8) -> u8 { let c = self.p & C; self.set_flag(C, v & 1 != 0); self.nz((v >> 1) | (c << 7)) }

    /// ORA/AND/EOR/ADC: with T set they operate on zero-page[X] instead of A.
    fn alu(&mut self, bus: &mut dyn PceBus, t: bool, op: u8, v: u8) -> u32 {
        let target = if t { self.rd(bus, 0x2000 | self.x as u16) } else { self.a };
        let r = match op {
            0 => self.nz(target | v),
            1 => self.nz(target & v),
            2 => self.nz(target ^ v),
            _ => self.adc(target, v),
        };
        if t { self.wr(bus, 0x2000 | self.x as u16, r); 3 } else { self.a = r; 0 }
    }

    fn branch(&mut self, bus: &mut dyn PceBus, cond: bool) -> u32 {
        let rel = self.fetch(bus) as i8;
        if cond { self.pc = self.pc.wrapping_add(rel as u16); 4 } else { 2 }
    }

    fn rmw(&mut self, bus: &mut dyn PceBus, mode: Mode, f: fn(&mut Self, u8) -> u8) -> u32 {
        let addr = self.ea(bus, mode);
        let v = self.rd(bus, addr);
        let r = f(self, v);
        self.wr(bus, addr, r);
        match mode { Mode::Zp | Mode::ZpX => 6, _ => 7 }
    }

    // ---------------- interrupts ----------------

    fn interrupt(&mut self, bus: &mut dyn PceBus, vector: u16, brk: bool) -> u32 {
        self.push(bus, (self.pc >> 8) as u8);
        self.push(bus, self.pc as u8);
        let pushed = if brk { self.p | B } else { self.p & !B };
        self.push(bus, pushed);
        self.p = (self.p | I) & !(D | T);
        self.pc = self.rd16(bus, vector);
        8
    }

    // ---------------- block transfers ----------------

    /// TII/TDD/TIN/TIA/TAI src, dst, len. `src_step`/`dst_step`: +1, -1, 0 (fixed),
    /// or 2 = alternate (+0, +1, +0, +1 ...).
    fn block(&mut self, bus: &mut dyn PceBus, src_step: i8, dst_step: i8) -> u32 {
        let src = self.fetch16(bus);
        let dst = self.fetch16(bus);
        let len = match self.fetch16(bus) { 0 => 0x10000u32, n => n as u32 };
        for i in 0..len {
            let s = match src_step { 2 => src.wrapping_add((i & 1) as u16), k => src.wrapping_add((i as i32 * k as i32) as u16) };
            let d = match dst_step { 2 => dst.wrapping_add((i & 1) as u16), k => dst.wrapping_add((i as i32 * k as i32) as u16) };
            let v = self.rd(bus, s);
            self.wr(bus, d, v);
        }
        17 + 6 * len
    }

    // ---------------- execute ----------------

    /// Run one instruction (or take an interrupt). Returns cycles at 7.16 MHz.
    pub fn step(&mut self, bus: &mut dyn PceBus) -> u32 {
        let cycles = self.step_native(bus);
        if self.fast { cycles } else { cycles * 4 }
    }

    fn step_native(&mut self, bus: &mut dyn PceBus) -> u32 {
        if self.p & I == 0 {
            let pending = bus.irq_pending();
            if pending & TIMER != 0 { return self.interrupt(bus, 0xFFFA, false); }
            if pending & IRQ1 != 0 { return self.interrupt(bus, 0xFFF8, false); }
            if pending & IRQ2 != 0 { return self.interrupt(bus, 0xFFF6, false); }
        }
        let t = self.p & T != 0;
        self.p &= !T;                       // T only lasts for one instruction
        let op = self.fetch(bus);
        let dec = if self.p & D != 0 { 1 } else { 0 };   // BCD arithmetic takes a cycle more

        // the regular "group 1" ALU block: ORA AND EOR ADC STA LDA CMP SBC
        if op & 0x03 == 0x01 || op & 0x1F == 0x12 {
            let aaa = op >> 5;
            let mode = if op & 0x1F == 0x12 { Some(Mode::Izp) } else {
                match (op >> 2) & 7 {
                    0 => Some(Mode::IzX), 1 => Some(Mode::Zp), 2 => None, 3 => Some(Mode::Abs),
                    4 => Some(Mode::IzY), 5 => Some(Mode::ZpX), 6 => Some(Mode::AbsY), _ => Some(Mode::AbsX),
                }
            };
            if aaa == 4 {
                // STA (0x89 = STA # doesn't exist; it's BIT #, handled below)
                if let Some(m) = mode {
                    let addr = self.ea(bus, m);
                    self.wr(bus, addr, self.a);
                    return m.cycles();
                }
            } else {
                let (v, cyc) = match mode {
                    Some(m) => { let addr = self.ea(bus, m); (self.rd(bus, addr), m.cycles()) }
                    None => (self.fetch(bus), 2),
                };
                return cyc + match aaa {
                    0 | 1 | 2 => self.alu(bus, t, aaa, v),
                    3 => self.alu(bus, t, 3, v) + dec,
                    5 => { self.a = self.nz(v); 0 }
                    6 => { self.cmp(self.a, v); 0 }
                    _ => { self.a = self.sbc(self.a, v); dec }
                };
            }
        }

        match op {
            // ---- RMBn / SMBn zp, BBRn / BBSn zp,rel ----
            _ if op & 0x0F == 0x07 => {
                let zp = 0x2000 | self.fetch(bus) as u16;
                let bit = 1 << ((op >> 4) & 7);
                let v = self.rd(bus, zp);
                self.wr(bus, zp, if op & 0x80 != 0 { v | bit } else { v & !bit });
                7
            }
            _ if op & 0x0F == 0x0F => {
                let zp = 0x2000 | self.fetch(bus) as u16;
                let bit = 1 << ((op >> 4) & 7);
                let set = self.rd(bus, zp) & bit != 0;
                let want_set = op & 0x80 != 0;
                4 + self.branch(bus, set == want_set)
            }

            // ---- loads / stores not in group 1 ----
            0xA2 => { let v = self.fetch(bus); self.x = self.nz(v); 2 }
            0xA0 => { let v = self.fetch(bus); self.y = self.nz(v); 2 }
            0xA6 | 0xB6 | 0xAE | 0xBE => {
                let m = match op { 0xA6 => Mode::Zp, 0xB6 => Mode::ZpY, 0xAE => Mode::Abs, _ => Mode::AbsY };
                let addr = self.ea(bus, m); let v = self.rd(bus, addr); self.x = self.nz(v); m.cycles()
            }
            0xA4 | 0xB4 | 0xAC | 0xBC => {
                let m = match op { 0xA4 => Mode::Zp, 0xB4 => Mode::ZpX, 0xAC => Mode::Abs, _ => Mode::AbsX };
                let addr = self.ea(bus, m); let v = self.rd(bus, addr); self.y = self.nz(v); m.cycles()
            }
            0x86 | 0x96 | 0x8E => {
                let m = match op { 0x86 => Mode::Zp, 0x96 => Mode::ZpY, _ => Mode::Abs };
                let addr = self.ea(bus, m); self.wr(bus, addr, self.x); m.cycles()
            }
            0x84 | 0x94 | 0x8C => {
                let m = match op { 0x84 => Mode::Zp, 0x94 => Mode::ZpX, _ => Mode::Abs };
                let addr = self.ea(bus, m); self.wr(bus, addr, self.y); m.cycles()
            }
            0x64 | 0x74 | 0x9C | 0x9E => {
                let m = match op { 0x64 => Mode::Zp, 0x74 => Mode::ZpX, 0x9C => Mode::Abs, _ => Mode::AbsX };
                let addr = self.ea(bus, m); self.wr(bus, addr, 0); m.cycles()
            }

            // ---- compares ----
            0xE0 => { let v = self.fetch(bus); self.cmp(self.x, v); 2 }
            0xC0 => { let v = self.fetch(bus); self.cmp(self.y, v); 2 }
            0xE4 | 0xEC => {
                let m = if op == 0xE4 { Mode::Zp } else { Mode::Abs };
                let addr = self.ea(bus, m); let v = self.rd(bus, addr); self.cmp(self.x, v); m.cycles()
            }
            0xC4 | 0xCC => {
                let m = if op == 0xC4 { Mode::Zp } else { Mode::Abs };
                let addr = self.ea(bus, m); let v = self.rd(bus, addr); self.cmp(self.y, v); m.cycles()
            }

            // ---- BIT / TSB / TRB / TST ----
            0x89 => { let v = self.fetch(bus); self.bit(v); 2 }
            0x24 | 0x34 | 0x2C | 0x3C => {
                let m = match op { 0x24 => Mode::Zp, 0x34 => Mode::ZpX, 0x2C => Mode::Abs, _ => Mode::AbsX };
                let addr = self.ea(bus, m); let v = self.rd(bus, addr); self.bit(v); m.cycles()
            }
            0x04 | 0x0C | 0x14 | 0x1C => {
                let m = if op & 0x08 == 0 { Mode::Zp } else { Mode::Abs };
                let addr = self.ea(bus, m);
                let v = self.rd(bus, addr);
                self.set_flag(Z, self.a & v == 0);
                self.set_flag(N, v & 0x80 != 0);
                self.set_flag(V, v & 0x40 != 0);
                self.wr(bus, addr, if op & 0x10 == 0 { v | self.a } else { v & !self.a });
                if m as u8 == Mode::Zp as u8 { 6 } else { 7 }
            }
            0x83 | 0xA3 | 0x93 | 0xB3 => {
                let imm = self.fetch(bus);
                let m = match op { 0x83 => Mode::Zp, 0xA3 => Mode::ZpX, 0x93 => Mode::Abs, _ => Mode::AbsX };
                let addr = self.ea(bus, m);
                let v = self.rd(bus, addr);
                self.set_flag(Z, imm & v == 0);
                self.set_flag(N, v & 0x80 != 0);
                self.set_flag(V, v & 0x40 != 0);
                if op & 0x10 == 0 { 7 } else { 8 }
            }

            // ---- shifts / inc / dec ----
            0x0A => { self.a = self.asl(self.a); 2 }
            0x4A => { self.a = self.lsr(self.a); 2 }
            0x2A => { self.a = self.rol(self.a); 2 }
            0x6A => { self.a = self.ror(self.a); 2 }
            0x1A => { self.a = self.nz(self.a.wrapping_add(1)); 2 }
            0x3A => { self.a = self.nz(self.a.wrapping_sub(1)); 2 }
            0x06 => self.rmw(bus, Mode::Zp, Self::asl),
            0x16 => self.rmw(bus, Mode::ZpX, Self::asl),
            0x0E => self.rmw(bus, Mode::Abs, Self::asl),
            0x1E => self.rmw(bus, Mode::AbsX, Self::asl),
            0x46 => self.rmw(bus, Mode::Zp, Self::lsr),
            0x56 => self.rmw(bus, Mode::ZpX, Self::lsr),
            0x4E => self.rmw(bus, Mode::Abs, Self::lsr),
            0x5E => self.rmw(bus, Mode::AbsX, Self::lsr),
            0x26 => self.rmw(bus, Mode::Zp, Self::rol),
            0x36 => self.rmw(bus, Mode::ZpX, Self::rol),
            0x2E => self.rmw(bus, Mode::Abs, Self::rol),
            0x3E => self.rmw(bus, Mode::AbsX, Self::rol),
            0x66 => self.rmw(bus, Mode::Zp, Self::ror),
            0x76 => self.rmw(bus, Mode::ZpX, Self::ror),
            0x6E => self.rmw(bus, Mode::Abs, Self::ror),
            0x7E => self.rmw(bus, Mode::AbsX, Self::ror),
            0xE6 => self.rmw(bus, Mode::Zp, |c, v| c.nz(v.wrapping_add(1))),
            0xF6 => self.rmw(bus, Mode::ZpX, |c, v| c.nz(v.wrapping_add(1))),
            0xEE => self.rmw(bus, Mode::Abs, |c, v| c.nz(v.wrapping_add(1))),
            0xFE => self.rmw(bus, Mode::AbsX, |c, v| c.nz(v.wrapping_add(1))),
            0xC6 => self.rmw(bus, Mode::Zp, |c, v| c.nz(v.wrapping_sub(1))),
            0xD6 => self.rmw(bus, Mode::ZpX, |c, v| c.nz(v.wrapping_sub(1))),
            0xCE => self.rmw(bus, Mode::Abs, |c, v| c.nz(v.wrapping_sub(1))),
            0xDE => self.rmw(bus, Mode::AbsX, |c, v| c.nz(v.wrapping_sub(1))),
            0xE8 => { self.x = self.nz(self.x.wrapping_add(1)); 2 }
            0xCA => { self.x = self.nz(self.x.wrapping_sub(1)); 2 }
            0xC8 => { self.y = self.nz(self.y.wrapping_add(1)); 2 }
            0x88 => { self.y = self.nz(self.y.wrapping_sub(1)); 2 }

            // ---- transfers ----
            0xAA => { self.x = self.nz(self.a); 2 }
            0x8A => { self.a = self.nz(self.x); 2 }
            0xA8 => { self.y = self.nz(self.a); 2 }
            0x98 => { self.a = self.nz(self.y); 2 }
            0xBA => { self.x = self.nz(self.s); 2 }
            0x9A => { self.s = self.x; 2 }
            0x02 => { std::mem::swap(&mut self.x, &mut self.y); 3 }    // SXY
            0x22 => { std::mem::swap(&mut self.a, &mut self.x); 3 }    // SAX
            0x42 => { std::mem::swap(&mut self.a, &mut self.y); 3 }    // SAY
            0x62 => { self.a = 0; 2 }                                  // CLA
            0x82 => { self.x = 0; 2 }                                  // CLX
            0xC2 => { self.y = 0; 2 }                                  // CLY

            // ---- stack ----
            0x48 => { self.push(bus, self.a); 3 }
            0xDA => { self.push(bus, self.x); 3 }
            0x5A => { self.push(bus, self.y); 3 }
            0x08 => { self.push(bus, self.p | B); 3 }
            0x68 => { let v = self.pull(bus); self.a = self.nz(v); 4 }
            0xFA => { let v = self.pull(bus); self.x = self.nz(v); 4 }
            0x7A => { let v = self.pull(bus); self.y = self.nz(v); 4 }
            0x28 => { self.p = self.pull(bus) & !B; 4 }

            // ---- flags ----
            0x18 => { self.p &= !C; 2 }
            0x38 => { self.p |= C; 2 }
            0x58 => { self.p &= !I; 2 }
            0x78 => { self.p |= I; 2 }
            0xB8 => { self.p &= !V; 2 }
            0xD8 => { self.p &= !D; 2 }
            0xF8 => { self.p |= D; 2 }
            0xF4 => { self.p |= T; 2 }                                 // SET
            0x54 => { self.fast = false; 3 }                           // CSL
            0xD4 => { self.fast = true; 3 }                            // CSH

            // ---- branches / jumps ----
            0x10 => self.branch(bus, self.p & N == 0),
            0x30 => self.branch(bus, self.p & N != 0),
            0x50 => self.branch(bus, self.p & V == 0),
            0x70 => self.branch(bus, self.p & V != 0),
            0x90 => self.branch(bus, self.p & C == 0),
            0xB0 => self.branch(bus, self.p & C != 0),
            0xD0 => self.branch(bus, self.p & Z == 0),
            0xF0 => self.branch(bus, self.p & Z != 0),
            0x80 => self.branch(bus, true),                            // BRA
            0x44 => {                                                  // BSR
                let rel = self.fetch(bus) as i8;
                let ret = self.pc.wrapping_sub(1);
                self.push(bus, (ret >> 8) as u8);
                self.push(bus, ret as u8);
                self.pc = self.pc.wrapping_add(rel as u16);
                8
            }
            0x4C => { self.pc = self.fetch16(bus); 4 }
            0x6C => { let a = self.fetch16(bus); self.pc = self.rd16(bus, a); 7 }
            0x7C => { let a = self.fetch16(bus).wrapping_add(self.x as u16); self.pc = self.rd16(bus, a); 7 }
            0x20 => {
                let target = self.fetch16(bus);
                let ret = self.pc.wrapping_sub(1);
                self.push(bus, (ret >> 8) as u8);
                self.push(bus, ret as u8);
                self.pc = target;
                7
            }
            0x60 => {
                let lo = self.pull(bus) as u16;
                let hi = self.pull(bus) as u16;
                self.pc = ((hi << 8) | lo).wrapping_add(1);
                7
            }
            0x40 => {
                self.p = self.pull(bus) & !B;
                let lo = self.pull(bus) as u16;
                let hi = self.pull(bus) as u16;
                self.pc = (hi << 8) | lo;
                7
            }
            0x00 => { self.pc = self.pc.wrapping_add(1); self.interrupt(bus, 0xFFF6, true) }

            // ---- HuC6280 specials ----
            0x53 => {                                                  // TAM
                let mask = self.fetch(bus);
                for i in 0..8 { if mask & (1 << i) != 0 { self.mpr[i] = self.a; } }
                5
            }
            0x43 => {                                                  // TMA
                let mask = self.fetch(bus);
                if let Some(i) = (0..8).find(|i| mask & (1 << i) != 0) { self.a = self.mpr[i]; }
                4
            }
            0x03 => { let v = self.fetch(bus); bus.write(0x1FE000, v); 5 }   // ST0
            0x13 => { let v = self.fetch(bus); bus.write(0x1FE002, v); 5 }   // ST1
            0x23 => { let v = self.fetch(bus); bus.write(0x1FE003, v); 5 }   // ST2
            0x73 => self.block(bus, 1, 1),                             // TII
            0xC3 => self.block(bus, -1, -1),                           // TDD
            0xD3 => self.block(bus, 1, 0),                             // TIN
            0xE3 => self.block(bus, 1, 2),                             // TIA
            0xF3 => self.block(bus, 2, 1),                             // TAI

            0xEA => 2,
            _ => 2,                                                    // undefined opcodes are NOPs
        }
    }
}
