//! Motorola 6809 (MC6809 / MC6809E), bus-cycle counted like MAME's m6809 core
//! (Nathan Woods, Olivier Galibert): every memory access, dummy read and idle
//! "VMA" cycle is one cycle, so instruction timings come out exactly as MAME's.
//! Undocumented opcodes follow MAME too (XNC, XDEC, XCLR, XST, XADD, XRES, XSWI2,
//! XFIRQ, X18). Verified instruction by instruction against MAME's own generated
//! core (tools/m6809_ref).
//!
//! `step()` runs one instruction or interrupt entry and returns E-clock cycles.
//! SYNC / CWAI wait one cycle per call until an interrupt comes.

use serde::{Deserialize, Serialize};

pub trait M6809Bus {
    fn read(&mut self, addr: u16) -> u8;
    fn write(&mut self, addr: u16, val: u8);
    /// Opcode fetch (MAME's decrypted-opcode space). Defaults to `read`.
    fn read_opcode(&mut self, addr: u16) -> u8 { self.read(addr) }
    /// Operand fetch and dummy cycles. Defaults to `read`.
    fn read_arg(&mut self, addr: u16) -> u8 { self.read(addr) }
}

pub const CC_C: u8 = 0x01;
pub const CC_V: u8 = 0x02;
pub const CC_Z: u8 = 0x04;
pub const CC_N: u8 = 0x08;
pub const CC_I: u8 = 0x10;
pub const CC_H: u8 = 0x20;
pub const CC_F: u8 = 0x40;
pub const CC_E: u8 = 0x80;

const VECTOR_SWI3: u16 = 0xFFF2;
const VECTOR_SWI2: u16 = 0xFFF4;
const VECTOR_FIRQ: u16 = 0xFFF6;
const VECTOR_IRQ: u16 = 0xFFF8;
const VECTOR_SWI: u16 = 0xFFFA;
const VECTOR_NMI: u16 = 0xFFFC;
const VECTOR_RESET: u16 = 0xFFFE;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode { Imm, Ea, A, B }

#[derive(Clone, Copy, PartialEq, Eq)]
enum R8 { A, B }

#[derive(Clone, Copy, PartialEq, Eq)]
enum R16 { D, X, Y, U, S }

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
enum Wait { None, Sync, Cwai }

#[derive(Clone, Serialize, Deserialize)]
pub struct M6809 {
    pub a: u8,
    pub b: u8,
    pub x: u16,
    pub y: u16,
    pub u: u16,
    pub s: u16,
    pub pc: u16,
    pub dp: u8,
    pub cc: u8,
    pub nmi_line: bool,
    nmi_asserted: bool,
    pub firq_line: bool,
    pub irq_line: bool,
    lds_encountered: bool,
    free_run: bool,
    wait: Wait,
    #[serde(skip)] cycles: u32,
    #[serde(skip, default = "default_mode")] mode: Mode,
    #[serde(skip)] ea: u16,
    #[serde(skip)] opcode: u8,
}

fn default_mode() -> Mode { Mode::Imm }

impl Default for M6809 { fn default() -> Self { Self::new() } }

impl M6809 {
    pub fn new() -> Self {
        Self {
            a: 0, b: 0, x: 0, y: 0, u: 0, s: 0, pc: 0, dp: 0, cc: 0,
            nmi_line: false, nmi_asserted: false, firq_line: false, irq_line: false,
            lds_encountered: false, free_run: false, wait: Wait::None,
            cycles: 0, mode: Mode::Imm, ea: 0, opcode: 0,
        }
    }

    pub fn d(&self) -> u16 { ((self.a as u16) << 8) | self.b as u16 }
    pub fn set_d(&mut self, v: u16) { self.a = (v >> 8) as u8; self.b = v as u8; }

    /// Reset: DP = 0, I and F set, PC from the reset vector. Returns cycles.
    pub fn reset(&mut self, bus: &mut dyn M6809Bus) -> u32 {
        self.cycles = 0;
        self.nmi_asserted = false;
        self.lds_encountered = false;
        self.free_run = false;
        self.wait = Wait::None;
        self.dp = 0;
        self.cc |= CC_I | CC_F;
        self.ea = VECTOR_RESET;
        self.interrupt_vector(bus);
        self.cycles
    }

    /// NMI input (edge triggered; ignored until S has been loaded, like the real chip).
    pub fn set_nmi(&mut self, state: bool) {
        self.nmi_asserted = self.nmi_asserted || (state && !self.nmi_line && self.lds_encountered);
        self.nmi_line = state;
    }
    pub fn set_irq(&mut self, state: bool) { self.irq_line = state; }
    pub fn set_firq(&mut self, state: bool) { self.firq_line = state; }

    // ---- bus with cycle counting ----
    fn rd(&mut self, bus: &mut dyn M6809Bus, a: u16) -> u8 { self.cycles += 1; bus.read(a) }
    fn wr(&mut self, bus: &mut dyn M6809Bus, a: u16, v: u8) { self.cycles += 1; bus.write(a, v) }
    fn fetch_op(&mut self, bus: &mut dyn M6809Bus) -> u8 {
        self.cycles += 1;
        let v = bus.read_opcode(self.pc);
        self.pc = self.pc.wrapping_add(1);
        v
    }
    fn arg(&mut self, bus: &mut dyn M6809Bus) -> u8 {
        self.cycles += 1;
        let v = bus.read_arg(self.pc);
        self.pc = self.pc.wrapping_add(1);
        v
    }
    fn dummy_arg(&mut self, bus: &mut dyn M6809Bus, delta: u16) {
        self.cycles += 1;
        bus.read_arg(self.pc.wrapping_add(delta));
    }
    fn vma(&mut self, bus: &mut dyn M6809Bus, n: u32) {
        for _ in 0..n { self.cycles += 1; bus.read_arg(0xFFFF); }
    }

    fn r8(&self, r: R8) -> u8 { match r { R8::A => self.a, R8::B => self.b } }
    fn set_r8(&mut self, r: R8, v: u8) { match r { R8::A => self.a = v, R8::B => self.b = v } }
    fn r16(&self, r: R16) -> u16 {
        match r { R16::D => self.d(), R16::X => self.x, R16::Y => self.y, R16::U => self.u, R16::S => self.s }
    }
    fn set_r16(&mut self, r: R16, v: u16) {
        match r { R16::D => self.set_d(v), R16::X => self.x = v, R16::Y => self.y = v, R16::U => self.u = v, R16::S => self.s = v }
    }

    fn read_operand(&mut self, bus: &mut dyn M6809Bus) -> u8 {
        match self.mode {
            Mode::Ea => { let a = self.ea; self.rd(bus, a) }
            Mode::Imm => self.arg(bus),
            Mode::A => self.a,
            Mode::B => self.b,
        }
    }
    fn read_operand_n(&mut self, bus: &mut dyn M6809Bus, n: u16) -> u8 {
        match self.mode {
            Mode::Ea => { let a = self.ea.wrapping_add(n); self.rd(bus, a) }
            _ => self.arg(bus),
        }
    }
    fn write_operand(&mut self, bus: &mut dyn M6809Bus, v: u8) {
        match self.mode {
            Mode::Imm => {}
            Mode::Ea => { let a = self.ea; self.wr(bus, a, v) }
            Mode::A => self.a = v,
            Mode::B => self.b = v,
        }
    }
    fn write_operand_n(&mut self, bus: &mut dyn M6809Bus, n: u16, v: u8) {
        if self.mode == Mode::Ea { let a = self.ea.wrapping_add(n); self.wr(bus, a, v) }
    }
    fn is_reg_mode(&self) -> bool { matches!(self.mode, Mode::A | Mode::B) }
    fn set_ea(&mut self, ea: u16) { self.ea = ea; self.mode = Mode::Ea; }

    // ---- flags (MAME set_flags) ----
    fn flags8(&mut self, mask: u8, a: u8, b: u8, r: u32) -> u8 {
        let (a, b) = (a as u32, b as u32);
        self.cc &= !mask;
        if mask & CC_H != 0 && (a ^ b ^ r) & 0x10 != 0 { self.cc |= CC_H; }
        if mask & CC_N != 0 && r & 0x80 != 0 { self.cc |= CC_N; }
        if mask & CC_Z != 0 && r as u8 == 0 { self.cc |= CC_Z; }
        if mask & CC_V != 0 && (a ^ b ^ r ^ (r >> 1)) & 0x80 != 0 { self.cc |= CC_V; }
        if mask & CC_C != 0 && r & 0x100 != 0 { self.cc |= CC_C; }
        r as u8
    }
    fn flags16(&mut self, mask: u8, a: u16, b: u16, r: u32) -> u16 {
        let (a, b) = (a as u32, b as u32);
        self.cc &= !mask;
        if mask & CC_H != 0 && (a ^ b ^ r) & 0x10 != 0 { self.cc |= CC_H; }
        if mask & CC_N != 0 && r & 0x8000 != 0 { self.cc |= CC_N; }
        if mask & CC_Z != 0 && r as u16 == 0 { self.cc |= CC_Z; }
        if mask & CC_V != 0 && (a ^ b ^ r ^ (r >> 1)) & 0x8000 != 0 { self.cc |= CC_V; }
        if mask & CC_C != 0 && r & 0x10000 != 0 { self.cc |= CC_C; }
        r as u16
    }
    fn nz8(&mut self, mask: u8, r: u8) -> u8 { self.flags8(mask, 0, r, r as u32) }
    fn nz16(&mut self, mask: u8, r: u16) -> u16 { self.flags16(mask, 0, r, r as u32) }

    fn pending_interrupt(&self) -> u16 {
        if self.nmi_asserted { VECTOR_NMI }
        else if self.cc & CC_F == 0 && self.firq_line { VECTOR_FIRQ }
        else if self.cc & CC_I == 0 && self.irq_line { VECTOR_IRQ }
        else { 0 }
    }

    // ---- stack ----
    fn push_registers(&mut self, bus: &mut dyn M6809Bus, sp: R16, mask: u8) {
        let other = if sp == R16::S { self.u } else { self.s };
        let mut p = self.r16(sp);
        let mut push = |cpu: &mut Self, v: u8| { p = p.wrapping_sub(1); cpu.wr(bus, p, v); };
        if mask & 0x80 != 0 { let pc = self.pc; push(self, pc as u8); push(self, (pc >> 8) as u8); }
        if mask & 0x40 != 0 { push(self, other as u8); push(self, (other >> 8) as u8); }
        if mask & 0x20 != 0 { let y = self.y; push(self, y as u8); push(self, (y >> 8) as u8); }
        if mask & 0x10 != 0 { let x = self.x; push(self, x as u8); push(self, (x >> 8) as u8); }
        if mask & 0x08 != 0 { let v = self.dp; push(self, v); }
        if mask & 0x04 != 0 { let v = self.b; push(self, v); }
        if mask & 0x02 != 0 { let v = self.a; push(self, v); }
        if mask & 0x01 != 0 { let v = self.cc; push(self, v); }
        self.set_r16(sp, p);
    }

    fn pull_registers(&mut self, bus: &mut dyn M6809Bus, sp: R16, mask: u8) {
        let mut pull = |cpu: &mut Self| -> u8 {
            let p = cpu.r16(sp);
            let v = cpu.rd(bus, p);
            cpu.set_r16(sp, p.wrapping_add(1));
            v
        };
        if mask & 0x01 != 0 { self.cc = pull(self); }
        if mask & 0x02 != 0 { self.a = pull(self); }
        if mask & 0x04 != 0 { self.b = pull(self); }
        if mask & 0x08 != 0 { self.dp = pull(self); }
        if mask & 0x10 != 0 { let h = pull(self); let l = pull(self); self.x = u16::from_be_bytes([h, l]); }
        if mask & 0x20 != 0 { let h = pull(self); let l = pull(self); self.y = u16::from_be_bytes([h, l]); }
        if mask & 0x40 != 0 {
            let h = pull(self); let l = pull(self);
            let v = u16::from_be_bytes([h, l]);
            if sp == R16::S { self.u = v } else { self.s = v }
        }
        if mask & 0x80 != 0 { let h = pull(self); let l = pull(self); self.pc = u16::from_be_bytes([h, l]); }
        let p = self.r16(sp);
        self.rd(bus, p);
    }

    fn interrupt_vector(&mut self, bus: &mut dyn M6809Bus) {
        self.vma(bus, 1);
        let ea = self.ea;
        let h = self.rd(bus, ea);
        let l = self.rd(bus, ea.wrapping_add(1));
        self.pc = u16::from_be_bytes([h, l]);
        self.vma(bus, 1);
    }

    fn take_interrupt(&mut self, bus: &mut dyn M6809Bus, vector: u16) {
        self.dummy_arg(bus, 0);
        self.dummy_arg(bus, 0);
        self.vma(bus, 1);
        match vector {
            VECTOR_NMI => {
                self.nmi_asserted = false;
                self.cc |= CC_E;
                self.push_registers(bus, R16::S, 0xFF);
                self.cc |= CC_I | CC_F;
            }
            VECTOR_FIRQ => {
                self.cc &= !CC_E;
                self.push_registers(bus, R16::S, 0x81);
                self.cc |= CC_I | CC_F;
            }
            _ => {
                self.cc |= CC_E;
                self.push_registers(bus, R16::S, 0xFF);
                self.cc |= CC_I;
            }
        }
        self.set_ea(vector);
        self.interrupt_vector(bus);
    }

    /// Runs one instruction (or takes an interrupt). Returns cycles used.
    pub fn step(&mut self, bus: &mut dyn M6809Bus) -> u32 {
        self.cycles = 0;
        match self.wait {
            Wait::Sync => {
                if !self.nmi_asserted && !self.firq_line && !self.irq_line { return 1; }
                self.wait = Wait::None;
                self.cycles += 1;     // MAME eat(1) on leaving SYNC
                return self.cycles;
            }
            Wait::Cwai => {
                let v = self.pending_interrupt();
                if v == 0 { return 1; }
                self.wait = Wait::None;
                if self.nmi_asserted { self.nmi_asserted = false; }
                self.cc |= CC_I | if v != VECTOR_IRQ { CC_F } else { 0 };
                self.set_ea(v);
                self.interrupt_vector(bus);
                return self.cycles;
            }
            Wait::None => {}
        }
        if self.free_run {
            // test mode: the address bus counts up until reset
            return 1;
        }
        let v = self.pending_interrupt();
        if v != 0 {
            self.take_interrupt(bus, v);
            return self.cycles;
        }
        self.opcode = self.fetch_op(bus);
        self.dispatch01(bus);
        self.cycles
    }

    // ---- addressing ----
    fn direct(&mut self, bus: &mut dyn M6809Bus) {
        let lo = self.arg(bus);
        self.set_ea(((self.dp as u16) << 8) | lo as u16);
        self.vma(bus, 1);
    }
    fn extended(&mut self, bus: &mut dyn M6809Bus) {
        let h = self.arg(bus);
        let l = self.arg(bus);
        self.set_ea(u16::from_be_bytes([h, l]));
        self.vma(bus, 1);
    }

    fn ireg(&self, post: u8) -> u16 {
        match post & 0x60 { 0x00 => self.x, 0x20 => self.y, 0x40 => self.u, _ => self.s }
    }
    fn set_ireg(&mut self, post: u8, v: u16) {
        match post & 0x60 { 0x00 => self.x = v, 0x20 => self.y = v, 0x40 => self.u = v, _ => self.s = v }
    }

    fn indexed(&mut self, bus: &mut dyn M6809Bus) {
        let post = self.arg(bus);
        let temp: u16;
        if post & 0x80 != 0 {
            let r = self.ireg(post);
            temp = match post & 0x0F {
                0x00 => { self.set_ireg(post, r.wrapping_add(1)); self.dummy_arg(bus, 0); self.vma(bus, 2); r }
                0x01 => { self.set_ireg(post, r.wrapping_add(2)); self.dummy_arg(bus, 0); self.vma(bus, 3); r }
                0x02 => { let v = r.wrapping_sub(1); self.set_ireg(post, v); self.dummy_arg(bus, 0); self.vma(bus, 2); v }
                0x03 => { let v = r.wrapping_sub(2); self.set_ireg(post, v); self.dummy_arg(bus, 0); self.vma(bus, 3); v }
                0x04 => { self.dummy_arg(bus, 0); r }
                0x05 => { let v = r.wrapping_add(self.b as i8 as u16); self.dummy_arg(bus, 0); self.vma(bus, 1); v }
                0x06 => { let v = r.wrapping_add(self.a as i8 as u16); self.dummy_arg(bus, 0); self.vma(bus, 1); v }
                0x08 => { let o = self.arg(bus) as i8 as u16; self.dummy_arg(bus, 0); r.wrapping_add(o) }
                0x09 => {
                    let h = self.arg(bus); let l = self.arg(bus);
                    self.vma(bus, 3);
                    r.wrapping_add(u16::from_be_bytes([h, l]))
                }
                0x0B => { let v = r.wrapping_add(self.d()); self.dummy_arg(bus, 0); self.dummy_arg(bus, 1); self.vma(bus, 3); v }
                0x0C => { let o = self.arg(bus) as i8 as u16; let v = self.pc.wrapping_add(o); self.vma(bus, 1); v }
                0x0D => {
                    let h = self.arg(bus); let l = self.arg(bus);
                    let v = self.pc.wrapping_add(u16::from_be_bytes([h, l]));
                    self.vma(bus, 4);
                    v
                }
                0x0F => { let h = self.arg(bus); let l = self.arg(bus); self.vma(bus, 1); u16::from_be_bytes([h, l]) }
                _ => 0,
            };
            if post & 0x10 != 0 {
                let h = self.rd(bus, temp);
                let l = self.rd(bus, temp.wrapping_add(1));
                self.vma(bus, 1);
                self.set_ea(u16::from_be_bytes([h, l]));
                return;
            }
        } else {
            let off = ((post & 0x0F) | if post & 0x10 != 0 { 0xF0 } else { 0 }) as i8 as u16;
            temp = self.ireg(post).wrapping_add(off);
            self.dummy_arg(bus, 0);
            self.vma(bus, 1);
        }
        self.set_ea(temp);
    }

    /// Sets up the operand for an 0x80-0xFF style opcode (imm / direct / indexed / extended).
    fn operand_mode(&mut self, bus: &mut dyn M6809Bus, op: u8) {
        match op & 0x30 {
            0x00 => self.mode = Mode::Imm,
            0x10 => self.direct(bus),
            0x20 => self.indexed(bus),
            _ => self.extended(bus),
        }
    }

    /// Sets up the operand for a 0x00-0x7F read-modify-write opcode.
    fn rmw_mode(&mut self, bus: &mut dyn M6809Bus, op: u8) {
        match op & 0xF0 {
            0x00 => self.direct(bus),
            0x40 => self.mode = Mode::A,
            0x50 => self.mode = Mode::B,
            0x60 => self.indexed(bus),
            _ => self.extended(bus),
        }
    }

    fn dispatch01(&mut self, bus: &mut dyn M6809Bus) {
        let op = self.opcode;
        match op {
            0x00..=0x0F | 0x40..=0x7F => {
                if op & 0x0F == 0x0E && (op == 0x4E || op == 0x5E) {
                    self.rmw_mode(bus, op);
                    self.xclr8(bus);
                    return;
                }
                self.rmw_mode(bus, op);
                match op & 0x0F {
                    0x0 | 0x1 => self.neg8(bus),
                    0x2 => self.xnc8(bus),
                    0x3 => self.com8(bus),
                    0x4 | 0x5 => self.lsr8(bus),
                    0x6 => self.ror8(bus),
                    0x7 => self.asr8(bus),
                    0x8 => self.asl8(bus),
                    0x9 => self.rol8(bus),
                    0xA => self.dec8(bus),
                    0xB => self.xdec8(bus),
                    0xC => self.inc8(bus),
                    0xD => self.tst8(bus),
                    0xE => self.pc = self.ea,        // JMP (direct / indexed / extended)
                    _ => self.clr8(bus),
                }
            }
            0x10 => self.dispatch10(bus),
            0x11 => self.dispatch11(bus),
            0x12 | 0x1B => self.nop(bus),
            0x13 => { self.dummy_arg(bus, 0); self.wait = Wait::Sync; }
            0x14 | 0x15 | 0xCD => self.free_run = true,
            0x16 => self.lbranch(bus, true),
            0x17 => {
                let h = self.arg(bus); let l = self.arg(bus);
                self.ea = self.pc.wrapping_add(u16::from_be_bytes([h, l]));
                self.vma(bus, 4);
                self.goto_subroutine(bus);
            }
            0x18 => {
                let t = bus.read_arg(self.pc); self.cycles += 1;
                let t = (self.cc & t) << 1;
                self.cc = t | ((self.cc & 0x04) >> 1);
            }
            0x19 => { self.daa(); self.dummy_arg(bus, 0); }
            0x1A => { self.mode = Mode::Imm; let v = self.read_operand(bus); self.cc |= v; self.dummy_arg(bus, 0); }
            0x1C => { self.mode = Mode::Imm; self.andcc(bus); }
            0x1D => { let v = self.b as i8 as i16 as u16; let r = self.nz16(CC_N | CC_Z, v); self.set_d(r); self.dummy_arg(bus, 0); }
            0x1E => self.exg(bus),
            0x1F => self.tfr(bus),
            0x20..=0x2F => { let c = self.cond(op); self.branch(bus, c); }
            0x30 | 0x31 => {
                self.indexed(bus);
                let ea = self.ea;
                let v = self.nz16(CC_Z, ea);
                if op == 0x30 { self.x = v } else { self.y = v }
                self.vma(bus, 1);
            }
            0x32 | 0x33 => {
                self.indexed(bus);
                if op == 0x32 { self.lds_encountered = true; self.s = self.ea } else { self.u = self.ea }
                self.vma(bus, 1);
            }
            0x34 | 0x36 => {
                let m = self.arg(bus);
                self.vma(bus, 2);
                let sp = if op == 0x34 { R16::S } else { R16::U };
                let p = self.r16(sp);
                self.rd(bus, p);
                self.push_registers(bus, sp, m);
            }
            0x35 | 0x37 => {
                let m = self.arg(bus);
                self.vma(bus, 2);
                self.pull_registers(bus, if op == 0x35 { R16::S } else { R16::U }, m);
            }
            0x38 => { self.mode = Mode::Imm; self.cycles += 1; self.andcc(bus); }
            0x39 => { self.dummy_arg(bus, 0); self.pull_registers(bus, R16::S, 0x80); }
            0x3A => { self.x = self.x.wrapping_add(self.b as u16); self.dummy_arg(bus, 0); self.vma(bus, 1); }
            0x3B => {
                self.dummy_arg(bus, 0);
                let p = self.s;
                self.cc = self.rd(bus, p);
                self.s = p.wrapping_add(1);
                let m = if self.cc & CC_E != 0 { 0xFF } else { 0x81 } & !0x01;
                self.pull_registers(bus, R16::S, m);
            }
            0x3C => {
                let v = self.arg(bus);
                self.cc &= v;
                self.dummy_arg(bus, 0);
                self.vma(bus, 1);
                self.cc |= CC_E;
                self.push_registers(bus, R16::S, 0xFF);
                self.wait = Wait::Cwai;
            }
            0x3D => {
                let r = (self.a as u16) * (self.b as u16);
                let r = self.nz16(CC_Z, r);
                self.set_d(r);
                if r & 0x80 != 0 { self.cc |= CC_C } else { self.cc &= !CC_C }
                self.dummy_arg(bus, 0);
                self.vma(bus, 9);
            }
            0x3E => { self.software_interrupt(bus, VECTOR_RESET, false); }
            0x3F => {
                self.set_ea(VECTOR_SWI);
                self.cc |= CC_E;
                self.dummy_arg(bus, 0);
                self.vma(bus, 1);
                self.push_registers(bus, R16::S, 0xFF);
                self.cc |= CC_I | CC_F;
                self.interrupt_vector(bus);
            }
            0x8D => {
                let o = self.arg(bus) as i8 as u16;
                self.ea = self.pc.wrapping_add(o);
                self.vma(bus, 3);
                self.goto_subroutine(bus);
            }
            0x80..=0xFF => if !self.page1_16(bus, op) { self.alu(bus, op) },
        }
    }

    /// The 0x80-0xFF block (page 1; also the shared parts of pages 2 and 3).
    fn alu(&mut self, bus: &mut dyn M6809Bus, op: u8) {
        let r8 = if op < 0xC0 { R8::A } else { R8::B };
        let lo = op & 0x0F;
        let imm = op & 0x30 == 0;
        match lo {
            0x0..=0x2 | 0x4..=0xB => {
                // 8-bit ops (0x87/0xC7 immediate = XST8)
                self.operand_mode(bus, op);
                match lo {
                    0x0 => self.sub8(bus, r8, false, true),
                    0x1 => self.sub8(bus, r8, false, false),
                    0x2 => self.sub8(bus, r8, true, true),
                    0x4 => { self.cc &= !CC_V; let v = self.read_operand(bus); let r = self.r8(r8) & v; let r = self.nz8(CC_N | CC_Z, r); self.set_r8(r8, r); }
                    0x5 => { self.cc &= !CC_V; let v = self.read_operand(bus); let r = self.r8(r8) & v; self.nz8(CC_N | CC_Z, r); }
                    0x6 => { let v = self.read_operand(bus); self.set_r8(r8, v); self.nz8(CC_N | CC_Z | CC_V, v); }
                    0x7 => {
                        if imm { self.read_operand(bus); let v = self.r8(r8); self.nz8(CC_N | CC_Z | CC_V, v); }
                        else { let v = self.r8(r8); let v = self.nz8(CC_N | CC_Z | CC_V, v); let a = self.ea; self.wr(bus, a, v); }
                    }
                    0x8 => { self.cc &= !CC_V; let v = self.read_operand(bus); let r = self.r8(r8) ^ v; let r = self.nz8(CC_N | CC_Z, r); self.set_r8(r8, r); }
                    0x9 => self.add8(bus, r8, true),
                    0xA => { self.cc &= !CC_V; let v = self.read_operand(bus); let r = self.r8(r8) | v; let r = self.nz8(CC_N | CC_Z, r); self.set_r8(r8, r); }
                    _ => self.add8(bus, r8, false),
                }
            }
            _ => unreachable!(),
        }
    }

    fn page1_16(&mut self, bus: &mut dyn M6809Bus, op: u8) -> bool {
        // 16-bit ops of page 1: 83/93/A3/B3 SUBD, C3.. ADDD, 8C.. CMPX, 8E LDX, 8F XST,
        // 9D JSR, 9F STX, CC LDD, DD STD, CE LDU, CF XST, DF STU
        let col = op & 0x0F;
        let hi = op >= 0xC0;
        if !matches!(col, 0x3 | 0xC | 0xD | 0xE | 0xF) { return false; }
        if op == 0x8D || op == 0xCD { return false; }
        self.operand_mode(bus, op);
        let imm = op & 0x30 == 0;
        match (col, hi) {
            (0x3, false) => self.arith16(bus, R16::D, 1),
            (0x3, true) => self.arith16(bus, R16::D, 0),
            (0xC, false) => self.arith16(bus, R16::X, 2),
            (0xC, true) => self.ld16(bus, R16::D),
            (0xD, false) => self.jsr(bus),
            (0xD, true) => self.st16(bus, R16::D),
            (0xE, false) => self.ld16(bus, R16::X),
            (0xE, true) => self.ld16(bus, R16::U),
            (0xF, false) => if imm { self.xst16(bus, R16::X) } else { self.st16(bus, R16::X) },
            _ => if imm { self.xst16(bus, R16::U) } else { self.st16(bus, R16::U) },
        }
        true
    }

    fn dispatch10(&mut self, bus: &mut dyn M6809Bus) {
        self.opcode = self.fetch_op(bus);
        let op = self.opcode;
        match op {
            0x10 | 0x11 => self.dispatch10(bus),
            0x20..=0x2F => { let c = self.cond(op); self.lbranch(bus, c); }
            0x3E => self.software_interrupt(bus, VECTOR_SWI2, false),
            0x3F => self.software_interrupt(bus, VECTOR_SWI2, true),
            0x83 | 0x93 | 0xA3 | 0xB3 => { self.operand_mode(bus, op); self.arith16(bus, R16::D, 2) }
            0x8C | 0x9C | 0xAC | 0xBC => { self.operand_mode(bus, op); self.arith16(bus, R16::Y, 2) }
            0x8E | 0x9E | 0xAE | 0xBE => { self.operand_mode(bus, op); self.ld16(bus, R16::Y) }
            0x9F | 0xAF | 0xBF => { self.operand_mode(bus, op); self.st16(bus, R16::Y) }
            0x8F => { self.mode = Mode::Imm; self.xst16(bus, R16::Y) }
            0x87 => { self.mode = Mode::Imm; self.read_operand(bus); let v = self.a; self.nz8(CC_N | CC_Z | CC_V, v); }
            0xC7 => { self.mode = Mode::Imm; self.read_operand(bus); let v = self.b; self.nz8(CC_N | CC_Z | CC_V, v); }
            0xC3 | 0xD3 | 0xE3 | 0xF3 => { self.operand_mode(bus, op); self.xadd16(bus, R16::D) }
            0xCE | 0xDE | 0xEE | 0xFE => { self.operand_mode(bus, op); self.ld16(bus, R16::S) }
            0xDF | 0xEF | 0xFF => { self.operand_mode(bus, op); self.st16(bus, R16::S) }
            0xCF => { self.mode = Mode::Imm; self.xst16(bus, R16::S) }
            // MAME: anything else re-enters the page 1 dispatch with a fresh fetch
            _ => { self.opcode = self.fetch_op(bus); self.dispatch01(bus) }
        }
    }

    fn dispatch11(&mut self, bus: &mut dyn M6809Bus) {
        self.opcode = self.fetch_op(bus);
        let op = self.opcode;
        match op {
            0x10 | 0x11 => self.dispatch11(bus),
            0x3E => self.software_interrupt(bus, VECTOR_FIRQ, false),
            0x3F => self.software_interrupt(bus, VECTOR_SWI3, true),
            0x83 | 0x93 | 0xA3 | 0xB3 => { self.operand_mode(bus, op); self.arith16(bus, R16::U, 2) }
            0x8C | 0x9C | 0xAC | 0xBC => { self.operand_mode(bus, op); self.arith16(bus, R16::S, 2) }
            0x87 => { self.mode = Mode::Imm; self.read_operand(bus); let v = self.a; self.nz8(CC_N | CC_Z | CC_V, v); }
            0xC7 => { self.mode = Mode::Imm; self.read_operand(bus); let v = self.b; self.nz8(CC_N | CC_Z | CC_V, v); }
            0x8F => { self.mode = Mode::Imm; self.xst16(bus, R16::X) }
            0xCF => { self.mode = Mode::Imm; self.xst16(bus, R16::U) }
            0xC3 | 0xD3 | 0xE3 | 0xF3 => { self.operand_mode(bus, op); self.xadd16(bus, R16::U) }
            // MAME: anything else re-enters the page 1 dispatch with a fresh fetch
            _ => { self.opcode = self.fetch_op(bus); self.dispatch01(bus) }
        }
    }

    // ---- instruction bodies (MAME base6x09.lst) ----
    fn cond(&self, op: u8) -> bool {
        let c = self.cc;
        let n = c & CC_N != 0; let z = c & CC_Z != 0; let v = c & CC_V != 0; let cy = c & CC_C != 0;
        let t = match (op >> 1) & 7 {
            0 => true,
            1 => !(c & (CC_Z | CC_C) != 0),
            2 => !cy,
            3 => !z,
            4 => !v,
            5 => !n,
            6 => n == v,
            _ => n == v && !z,
        };
        if op & 1 != 0 { !t } else { t }
    }

    fn branch(&mut self, bus: &mut dyn M6809Bus, taken: bool) {
        let o = self.arg(bus) as i8 as u16;
        self.vma(bus, 1);
        if taken { self.pc = self.pc.wrapping_add(o); }
    }
    fn lbranch(&mut self, bus: &mut dyn M6809Bus, taken: bool) {
        let h = self.arg(bus); let l = self.arg(bus);
        self.vma(bus, 1);
        if taken {
            self.pc = self.pc.wrapping_add(u16::from_be_bytes([h, l]));
            self.vma(bus, 1);
        }
    }
    fn goto_subroutine(&mut self, bus: &mut dyn M6809Bus) {
        let pc = self.pc;
        self.s = self.s.wrapping_sub(1); let s = self.s; self.wr(bus, s, pc as u8);
        self.s = self.s.wrapping_sub(1); let s = self.s; self.wr(bus, s, (pc >> 8) as u8);
        self.pc = self.ea;
    }
    fn jsr(&mut self, bus: &mut dyn M6809Bus) {
        self.dummy_arg(bus, 0);
        self.vma(bus, 1);
        self.goto_subroutine(bus);
    }
    fn software_interrupt(&mut self, bus: &mut dyn M6809Bus, vector: u16, set_e: bool) {
        // SWI2/SWI3 (set_e) and the undocumented XRES/XSWI2/XFIRQ (push all, E unchanged)
        self.set_ea(vector);
        if set_e { self.cc |= CC_E; }
        self.dummy_arg(bus, 0);
        self.vma(bus, 1);
        self.push_registers(bus, R16::S, 0xFF);
        self.interrupt_vector(bus);
    }
    fn nop(&mut self, bus: &mut dyn M6809Bus) { self.dummy_arg(bus, 0); }
    fn andcc(&mut self, bus: &mut dyn M6809Bus) {
        let v = self.read_operand(bus);
        self.cc &= v;
        self.dummy_arg(bus, 0);
    }
    fn daa(&mut self) {
        let mut cf = 0u16;
        let msn = self.a & 0xF0;
        let lsn = self.a & 0x0F;
        if lsn > 0x09 || self.cc & CC_H != 0 { cf |= 0x06; }
        if msn > 0x80 && lsn > 0x09 { cf |= 0x60; }
        if msn > 0x90 || self.cc & CC_C != 0 { cf |= 0x60; }
        let t = self.a as u16 + cf;
        self.cc &= !CC_V;
        if t & 0x100 != 0 { self.cc |= CC_C; }
        self.a = self.nz8(CC_N | CC_Z, t as u8);
    }

    fn read_tfr_816(&self, r: u8) -> u16 {
        match r & 0x0F {
            0 => self.d(), 1 => self.x, 2 => self.y, 3 => self.u, 4 => self.s, 5 => self.pc,
            8 => 0xFF00 | self.a as u16, 9 => 0xFF00 | self.b as u16,
            10 => ((self.cc as u16) << 8) | self.cc as u16,
            11 => ((self.dp as u16) << 8) | self.dp as u16,
            _ => 0xFFFF,
        }
    }
    fn read_exg_168(&self, r: u8) -> u16 {
        match r & 0x0F {
            0 => self.d(), 1 => self.x, 2 => self.y, 3 => self.u, 4 => self.s, 5 => self.pc,
            8 => 0xFF00 | self.a as u16, 9 => 0xFF00 | self.b as u16,
            10 => 0xFF00 | self.cc as u16, 11 => 0xFF00 | self.dp as u16,
            _ => 0xFFFF,
        }
    }
    fn write_exgtfr(&mut self, r: u8, v: u16) {
        match r & 0x0F {
            0 => self.set_d(v), 1 => self.x = v, 2 => self.y = v, 3 => self.u = v, 4 => self.s = v, 5 => self.pc = v,
            8 => self.a = v as u8, 9 => self.b = v as u8, 10 => self.cc = v as u8, 11 => self.dp = v as u8,
            _ => {}
        }
    }
    fn exg(&mut self, bus: &mut dyn M6809Bus) {
        let p = self.arg(bus);
        let (r1, r2) = if p & 0x80 != 0 { (self.read_tfr_816(p >> 4), self.read_tfr_816(p)) }
                       else { (self.read_exg_168(p >> 4), self.read_exg_168(p)) };
        self.write_exgtfr(p, r1);
        self.write_exgtfr(p >> 4, r2);
        self.vma(bus, 6);
    }
    fn tfr(&mut self, bus: &mut dyn M6809Bus) {
        let p = self.arg(bus);
        let r = self.read_tfr_816(p >> 4);
        self.write_exgtfr(p, r);
        if p & 0x0F == 4 { self.lds_encountered = true; }
        self.vma(bus, 4);
    }

    fn neg8(&mut self, bus: &mut dyn M6809Bus) {
        let t = self.read_operand(bus);
        let r = self.flags8(CC_N | CC_Z | CC_V | CC_C, 0, t, (t as u32).wrapping_neg());
        self.dummy_arg(bus, 0);
        self.write_operand(bus, r);
    }
    fn com8(&mut self, bus: &mut dyn M6809Bus) {
        let t = self.read_operand(bus);
        self.cc &= !CC_V;
        self.cc |= CC_C;
        let r = self.nz8(CC_N | CC_Z, !t);
        self.dummy_arg(bus, 0);
        self.write_operand(bus, r);
    }
    fn xnc8(&mut self, bus: &mut dyn M6809Bus) {
        if self.cc & CC_C != 0 { self.com8(bus) } else { self.neg8(bus) }
    }
    fn lsr8(&mut self, bus: &mut dyn M6809Bus) {
        let t = self.read_operand(bus);
        self.cc &= !CC_C;
        if t & 1 != 0 { self.cc |= CC_C; }
        let r = self.nz8(CC_N | CC_Z, t >> 1);
        self.dummy_arg(bus, 0);
        self.write_operand(bus, r);
    }
    fn ror8(&mut self, bus: &mut dyn M6809Bus) {
        let t = self.read_operand(bus);
        let c = self.cc & CC_C != 0;
        let v = (t >> 1) | if c { 0x80 } else { 0 };
        if t & 1 != 0 { self.cc |= CC_C } else { self.cc &= !CC_C }
        let r = self.nz8(CC_N | CC_Z, v);
        self.dummy_arg(bus, 0);
        self.write_operand(bus, r);
    }
    fn asr8(&mut self, bus: &mut dyn M6809Bus) {
        let t = self.read_operand(bus);
        self.cc &= !CC_C;
        if t & 1 != 0 { self.cc |= CC_C; }
        let r = self.nz8(CC_N | CC_Z, ((t as i8) >> 1) as u8);
        self.dummy_arg(bus, 0);
        self.write_operand(bus, r);
    }
    fn asl8(&mut self, bus: &mut dyn M6809Bus) {
        let t = self.read_operand(bus);
        let r = self.flags8(CC_N | CC_Z | CC_V | CC_C, t, t, (t as u32) << 1);
        self.dummy_arg(bus, 0);
        self.write_operand(bus, r);
    }
    fn rol8(&mut self, bus: &mut dyn M6809Bus) {
        let t = self.read_operand(bus);
        let c = self.cc & CC_C != 0;
        let v = ((t as u32) << 1) | c as u32;
        if t & 0x80 != 0 { self.cc |= CC_C } else { self.cc &= !CC_C }
        let r = self.flags8(CC_N | CC_Z | CC_V, t, t, v);
        self.dummy_arg(bus, 0);
        self.write_operand(bus, r);
    }
    fn dec8(&mut self, bus: &mut dyn M6809Bus) {
        let t = self.read_operand(bus);
        let r = self.flags8(CC_N | CC_Z | CC_V, t, 1, (t as u32).wrapping_sub(1));
        self.dummy_arg(bus, 0);
        self.write_operand(bus, r);
    }
    fn xdec8(&mut self, bus: &mut dyn M6809Bus) {
        let t = self.read_operand(bus);
        if t != 0 { self.cc |= CC_C } else { self.cc &= !CC_C }
        let r = self.flags8(CC_N | CC_Z | CC_V, t, 1, (t as u32).wrapping_sub(1));
        self.dummy_arg(bus, 0);
        self.write_operand(bus, r);
    }
    fn inc8(&mut self, bus: &mut dyn M6809Bus) {
        let t = self.read_operand(bus);
        let r = self.flags8(CC_N | CC_Z | CC_V, t, 1, t as u32 + 1);
        self.dummy_arg(bus, 0);
        self.write_operand(bus, r);
    }
    fn tst8(&mut self, bus: &mut dyn M6809Bus) {
        let t = self.read_operand(bus);
        self.nz8(CC_N | CC_Z | CC_V, t);
        self.vma(bus, 1);
        if !self.is_reg_mode() { self.vma(bus, 1); }
    }
    fn clr8(&mut self, bus: &mut dyn M6809Bus) {
        self.read_operand(bus);
        self.cc &= !(CC_N | CC_Z | CC_V | CC_C);
        self.cc |= CC_Z;
        self.dummy_arg(bus, 0);
        self.write_operand(bus, 0);
    }
    fn xclr8(&mut self, bus: &mut dyn M6809Bus) {
        self.read_operand(bus);
        self.cc &= !(CC_N | CC_Z | CC_V);
        self.cc |= CC_Z;
        self.dummy_arg(bus, 0);
        self.write_operand(bus, 0);
    }

    fn sub8(&mut self, bus: &mut dyn M6809Bus, r: R8, with_carry: bool, store: bool) {
        let t = self.read_operand(bus);
        let a = self.r8(r);
        let c = if with_carry && self.cc & CC_C != 0 { 1 } else { 0 };
        let v = (a as u32).wrapping_sub(t as u32).wrapping_sub(c);
        let res = self.flags8(CC_N | CC_Z | CC_V | CC_C, a, t, v);
        if store { self.set_r8(r, res); }
    }
    fn add8(&mut self, bus: &mut dyn M6809Bus, r: R8, with_carry: bool) {
        let t = self.read_operand(bus);
        let a = self.r8(r);
        let c = if with_carry && self.cc & CC_C != 0 { 1 } else { 0 };
        let v = a as u32 + t as u32 + c;
        let res = self.flags8(CC_H | CC_N | CC_Z | CC_V | CC_C, a, t, v);
        self.set_r8(r, res);
    }
    /// kind: 0 = ADD, 1 = SUB, 2 = CMP
    fn arith16(&mut self, bus: &mut dyn M6809Bus, r: R16, kind: u8) {
        let h = self.read_operand_n(bus, 0);
        let l = self.read_operand_n(bus, 1);
        let t = u16::from_be_bytes([h, l]);
        let a = self.r16(r);
        let v = if kind == 0 { a as u32 + t as u32 } else { (a as u32).wrapping_sub(t as u32) };
        let res = self.flags16(CC_N | CC_Z | CC_V | CC_C, a, t, v);
        if kind != 2 { self.set_r16(r, res); }
        self.dummy_arg(bus, 0);
    }
    fn xadd16(&mut self, bus: &mut dyn M6809Bus, r: R16) {
        let h = self.read_operand_n(bus, 0);
        let l = self.read_operand_n(bus, 1);
        let t = u16::from_be_bytes([h, l]);
        let a = self.r16(r);
        self.flags16(CC_N | CC_Z | CC_V | CC_C, a, t, a as u32 + t as u32);
        self.dummy_arg(bus, 0);
    }
    fn ld16(&mut self, bus: &mut dyn M6809Bus, r: R16) {
        let h = self.read_operand_n(bus, 0);
        let l = self.read_operand_n(bus, 1);
        let v = u16::from_be_bytes([h, l]);
        self.set_r16(r, v);
        self.nz16(CC_N | CC_Z | CC_V, v);
        if r == R16::S { self.lds_encountered = true; }
    }
    fn st16(&mut self, bus: &mut dyn M6809Bus, r: R16) {
        let v = self.r16(r);
        self.write_operand_n(bus, 0, (v >> 8) as u8);
        self.write_operand_n(bus, 1, v as u8);
        self.nz16(CC_N | CC_Z | CC_V, v);
    }
    fn xst16(&mut self, bus: &mut dyn M6809Bus, r: R16) {
        // undocumented "store immediate": reads one byte, then writes the low byte over the next
        self.read_operand_n(bus, 0);
        let v = self.r16(r);
        let pc = self.pc;
        self.wr(bus, pc, v as u8);
        self.pc = pc.wrapping_add(1);
        self.nz16(CC_N | CC_Z | CC_V, v);
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    struct Mem { m: Vec<u8>, writes: Vec<(u16, u8)> }
    impl M6809Bus for Mem {
        fn read(&mut self, a: u16) -> u8 { self.m[a as usize] }
        fn write(&mut self, a: u16, v: u8) { self.m[a as usize] = v; self.writes.push((a, v)); }
    }

    fn xorshift(s: &mut u32) -> u32 { *s ^= *s << 13; *s ^= *s >> 17; *s ^= *s << 5; *s }

    /// Vectors from MAME's generated 6809 core (tools/m6809_ref):
    /// `seed pc x y u s a b dp cc irq firq nmi op01 op23 | pc x y u s a b dp cc cycles | addr=val...`
    fn run_vectors(text: &str) -> usize {
        let mut n = 0;
        for (lineno, line) in text.lines().enumerate() {
            if line.contains("SKIP") || line.trim().is_empty() { continue; }
            let parts: Vec<&str> = line.split('|').collect();
            let h = |s: &str| u32::from_str_radix(s, 16).unwrap();
            let a: Vec<u32> = parts[0].split_whitespace().map(h).collect();
            let b: Vec<u32> = parts[1].split_whitespace().map(|s| if s.len() < 8 && s.chars().all(|c| c.is_ascii_hexdigit()) { h(s) } else { 0 }).collect();
            let mut s = a[0] | 1;
            let mut mem = Mem { m: (0..0x10000).map(|_| xorshift(&mut s) as u8).collect(), writes: vec![] };
            let mut cpu = M6809::new();
            cpu.pc = a[1] as u16; cpu.x = a[2] as u16; cpu.y = a[3] as u16; cpu.u = a[4] as u16; cpu.s = a[5] as u16;
            cpu.a = a[6] as u8; cpu.b = a[7] as u8; cpu.dp = a[8] as u8; cpu.cc = a[9] as u8;
            cpu.irq_line = a[10] != 0; cpu.firq_line = a[11] != 0; cpu.nmi_asserted = a[12] != 0;
            cpu.lds_encountered = true;
            let ops = [a[13] as u8, (a[13] >> 8) as u8, a[14] as u8, (a[14] >> 8) as u8];
            for (i, &o) in ops.iter().enumerate() { mem.m[(cpu.pc.wrapping_add(i as u16)) as usize] = o; }
            let cycles = cpu.step(&mut mem);
            // decimal cycle count in the vectors
            let want_cycles: u32 = parts[1].split_whitespace().nth(9).unwrap().parse().unwrap();
            let got = [cpu.pc as u32, cpu.x as u32, cpu.y as u32, cpu.u as u32, cpu.s as u32,
                       cpu.a as u32, cpu.b as u32, cpu.dp as u32, cpu.cc as u32];
            let mut want_w = std::collections::BTreeMap::new();
            for kv in parts.get(2).map(|s| s.split_whitespace().collect::<Vec<_>>()).unwrap_or_default() {
                let (k, v) = kv.split_once('=').unwrap();
                want_w.insert(h(k) as u16, h(v) as u8);
            }
            let mut got_w = std::collections::BTreeMap::new();
            for (k, v) in &mem.writes { got_w.insert(*k, *v); }
            assert!(got[..] == b[..9] && cycles == want_cycles && got_w == want_w,
                "line {}: ops {:02x?}\n want {:x?} cyc {} {:x?}\n got  {:x?} cyc {} {:x?}",
                lineno + 1, ops, &b[..9], want_cycles, want_w, got, cycles, got_w);
            n += 1;
        }
        n
    }

    #[test]
    fn m6809_matches_mame_sample() {
        assert!(run_vectors(include_str!("../tests/data/m6809_vectors.txt")) > 4000);
    }

    /// M6809_VECTORS=<file> cargo test --release --lib m6809_matches_mame_full -- --ignored
    #[test]
    #[ignore]
    fn m6809_matches_mame_full() {
        let path = std::env::var("M6809_VECTORS").expect("M6809_VECTORS=<file>");
        println!("{} vectors ok", run_vectors(&std::fs::read_to_string(path).unwrap()));
    }
}
