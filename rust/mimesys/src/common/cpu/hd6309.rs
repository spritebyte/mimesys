//! Hitachi HD6309 (HD6309 / HD6309E), a port of MAME's hd6309 core (Nathan Woods):
//! the 6809 plus E/F/W/Q/V registers, MD (native mode, FIRQ mode, trap flags),
//! register-register ops, bit ops, TFM block moves, MULD/DIVD/DIVQ and the
//! illegal-instruction / divide-by-zero trap at FFF0.
//!
//! Cycles are counted the way MAME counts them, so emulation mode times like a
//! 6809 and native mode drops the dummy cycles. Verified instruction by
//! instruction against MAME's own generated core (tools/hd6309_ref).
//!
//! `step()` runs one instruction or interrupt entry and returns cycles. A long
//! TFM is split over several calls (64 bytes each) so interrupts can break in,
//! exactly where MAME lets them (the TFM restarts after RTI).
//!
//! Reuses `M6809Bus` from the 6809 core.

use serde::{Deserialize, Serialize};

pub use super::m6809::{M6809Bus, CC_C, CC_V, CC_Z, CC_N, CC_I, CC_H, CC_F, CC_E};

const VECTOR_ILLEGAL: u16 = 0xFFF0;
const VECTOR_SWI3: u16 = 0xFFF2;
const VECTOR_SWI2: u16 = 0xFFF4;
const VECTOR_FIRQ: u16 = 0xFFF6;
const VECTOR_IRQ: u16 = 0xFFF8;
const VECTOR_SWI: u16 = 0xFFFA;
const VECTOR_NMI: u16 = 0xFFFC;
const VECTOR_RESET: u16 = 0xFFFE;

const NZ: u8 = CC_N | CC_Z;
const NZV: u8 = CC_N | CC_Z | CC_V;
const NZVC: u8 = CC_N | CC_Z | CC_V | CC_C;

/// Bytes a TFM moves per `step()` call before giving interrupts a chance.
const TFM_CHUNK: u32 = 64;

/// MAME addressing modes (where operands come from).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode { Imm, Ea, A, B, D, E, F, W, X, Y, U, S, Cc, Dp, Pc, V, Zero }

/// 8-bit register operand (MAME regop8). `Temp` is the low byte of the temp register.
#[derive(Clone, Copy, PartialEq, Eq)]
enum R8 { A, B, E, F, Cc, Dp, Md, Temp }

/// 16-bit register operand (MAME regop16). `Temp` is the temp register ("zero").
#[derive(Clone, Copy, PartialEq, Eq)]
enum R16 { D, X, Y, U, S, Pc, W, V, Temp }

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
enum Wait { None, Sync, Cwai }

#[derive(Clone, Serialize, Deserialize)]
pub struct HD6309 {
    pub a: u8,
    pub b: u8,
    pub e: u8,
    pub f: u8,
    pub x: u16,
    pub y: u16,
    pub u: u16,
    pub s: u16,
    pub pc: u16,
    pub v: u16,
    pub dp: u8,
    pub cc: u8,
    /// Mode register: bit 0 native mode, bit 1 FIRQ saves everything,
    /// bit 6 illegal-instruction trap, bit 7 divide-by-zero trap.
    pub md: u8,
    pub nmi_line: bool,
    nmi_asserted: bool,
    pub firq_line: bool,
    pub irq_line: bool,
    lds_encountered: bool,
    wait: Wait,
    /// TFM in progress (its opcode and postbyte).
    tfm: bool,
    tfm_op: u8,
    tfm_post: u8,
    temp: u16,
    temp_im: u8,
    #[serde(skip)] cycles: u32,
    #[serde(skip, default = "default_mode")] mode: Mode,
    #[serde(skip)] ea: u16,
    #[serde(skip)] opcode: u8,
    #[serde(skip, default = "default_r8")] reg8: R8,
    #[serde(skip, default = "default_r16")] reg16: R16,
    #[serde(skip)] reg_is16: bool,
}

fn default_mode() -> Mode { Mode::Imm }
fn default_r8() -> R8 { R8::A }
fn default_r16() -> R16 { R16::D }

impl Default for HD6309 { fn default() -> Self { Self::new() } }

impl HD6309 {
    pub fn new() -> Self {
        Self {
            a: 0, b: 0, e: 0, f: 0, x: 0, y: 0, u: 0, s: 0, pc: 0, v: 0xFFFF, dp: 0, cc: 0, md: 0,
            nmi_line: false, nmi_asserted: false, firq_line: false, irq_line: false,
            lds_encountered: false, wait: Wait::None, tfm: false, tfm_op: 0, tfm_post: 0,
            temp: 0, temp_im: 0, cycles: 0, mode: Mode::Imm, ea: 0, opcode: 0,
            reg8: R8::A, reg16: R16::D, reg_is16: false,
        }
    }

    pub fn d(&self) -> u16 { u16::from_be_bytes([self.a, self.b]) }
    pub fn set_d(&mut self, v: u16) { [self.a, self.b] = v.to_be_bytes(); }
    pub fn w(&self) -> u16 { u16::from_be_bytes([self.e, self.f]) }
    pub fn set_w(&mut self, v: u16) { [self.e, self.f] = v.to_be_bytes(); }
    pub fn q(&self) -> u32 { u32::from_be_bytes([self.a, self.b, self.e, self.f]) }
    pub fn set_q(&mut self, v: u32) { [self.a, self.b, self.e, self.f] = v.to_be_bytes(); }
    pub fn native(&self) -> bool { self.md & 0x01 != 0 }

    /// Reset: MD = 0 (emulation mode), DP = 0, I and F set, PC from the reset vector.
    pub fn reset(&mut self, bus: &mut dyn M6809Bus) -> u32 {
        self.cycles = 0;
        self.md = 0;
        self.nmi_asserted = false;
        self.lds_encountered = false;
        self.wait = Wait::None;
        self.tfm = false;
        self.dp = 0;
        self.cc |= CC_I | CC_F;
        self.set_ea(VECTOR_RESET);
        self.interrupt_vector(bus);
        self.cycles
    }

    /// NMI input (edge triggered; ignored until S has been loaded).
    pub fn set_nmi(&mut self, state: bool) {
        self.nmi_asserted = self.nmi_asserted || (state && !self.nmi_line && self.lds_encountered);
        self.nmi_line = state;
    }
    pub fn set_irq(&mut self, state: bool) { self.irq_line = state; }
    pub fn set_firq(&mut self, state: bool) { self.firq_line = state; }

    /// True while a TFM block move is part way through.
    pub fn busy(&self) -> bool { self.tfm }

    // ---- bus with cycle counting ----
    fn eat(&mut self, n: i32) { self.cycles = self.cycles.wrapping_add(n as u32); }
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
    fn dummy_arg(&mut self, bus: &mut dyn M6809Bus) {
        self.cycles += 1;
        bus.read_arg(self.pc);
    }
    fn vma(&mut self, bus: &mut dyn M6809Bus, n: u32) {
        for _ in 0..n { self.cycles += 1; bus.read_arg(0xFFFF); }
    }
    /// The "if (!hd6309_native_mode()) dummy_read_opcode_arg(0)" that most ops end with.
    fn emu_dummy(&mut self, bus: &mut dyn M6809Bus) { if !self.native() { self.dummy_arg(bus) } }

    // ---- registers ----
    fn set_ea(&mut self, ea: u16) { self.ea = ea; self.mode = Mode::Ea; }
    fn set_reg8(&mut self, r: R8) { self.reg8 = r; self.reg_is16 = false; }
    fn set_reg16(&mut self, r: R16) { self.reg16 = r; self.reg_is16 = true; }
    fn get8(&self, r: R8) -> u8 {
        match r {
            R8::A => self.a, R8::B => self.b, R8::E => self.e, R8::F => self.f,
            R8::Cc => self.cc, R8::Dp => self.dp, R8::Md => self.md, R8::Temp => self.temp as u8,
        }
    }
    fn put8(&mut self, r: R8, v: u8) {
        match r {
            R8::A => self.a = v, R8::B => self.b = v, R8::E => self.e = v, R8::F => self.f = v,
            R8::Cc => self.cc = v, R8::Dp => self.dp = v, R8::Md => self.md = v,
            R8::Temp => self.temp = (self.temp & 0xFF00) | v as u16,
        }
    }
    fn get16(&self, r: R16) -> u16 {
        match r {
            R16::D => self.d(), R16::X => self.x, R16::Y => self.y, R16::U => self.u, R16::S => self.s,
            R16::Pc => self.pc, R16::W => self.w(), R16::V => self.v, R16::Temp => self.temp,
        }
    }
    fn put16(&mut self, r: R16, v: u16) {
        match r {
            R16::D => self.set_d(v), R16::X => self.x = v, R16::Y => self.y = v, R16::U => self.u = v,
            R16::S => self.s = v, R16::Pc => self.pc = v, R16::W => self.set_w(v), R16::V => self.v = v,
            R16::Temp => self.temp = v,
        }
    }
    fn regop8(&self) -> u8 { self.get8(self.reg8) }
    fn set_regop8(&mut self, v: u8) { let r = self.reg8; self.put8(r, v) }
    fn regop16(&self) -> u16 { self.get16(self.reg16) }
    fn set_regop16(&mut self, v: u16) { let r = self.reg16; self.put16(r, v) }
    fn temp_lo(&self) -> u8 { self.temp as u8 }
    fn set_temp_lo(&mut self, v: u8) { self.temp = (self.temp & 0xFF00) | v as u16; }
    fn set_temp_hi(&mut self, v: u8) { self.temp = (self.temp & 0x00FF) | ((v as u16) << 8); }

    fn mode_reg16(&self, m: Mode) -> u16 {
        match m {
            Mode::D => self.d(), Mode::W => self.w(), Mode::X => self.x, Mode::Y => self.y,
            Mode::U => self.u, Mode::S => self.s, Mode::V => self.v, Mode::Pc => self.pc,
            _ => 0,
        }
    }

    // ---- operands (hd6309_device::read_operand / write_operand) ----
    fn read_operand(&mut self, bus: &mut dyn M6809Bus) -> u8 {
        match self.mode {
            Mode::Ea => { let a = self.ea; self.rd(bus, a) }
            Mode::Imm => self.arg(bus),
            Mode::A => self.a, Mode::B => self.b, Mode::E => self.e, Mode::F => self.f,
            // MAME stops with "Unexpected" here (8-bit register op from CC/DP/0)
            Mode::Cc => self.cc, Mode::Dp => self.dp,
            _ => 0,
        }
    }
    fn read_operand_n(&mut self, bus: &mut dyn M6809Bus, n: u16) -> u8 {
        match self.mode {
            Mode::Ea => { let a = self.ea.wrapping_add(n); self.rd(bus, a) }
            Mode::Imm => self.arg(bus),
            Mode::Zero => 0,
            m => { let v = self.mode_reg16(m); if n & 1 != 0 { v as u8 } else { (v >> 8) as u8 } }
        }
    }
    fn write_operand(&mut self, bus: &mut dyn M6809Bus, v: u8) {
        match self.mode {
            Mode::Ea => { let a = self.ea; self.wr(bus, a, v) }
            Mode::A => self.a = v, Mode::B => self.b = v, Mode::E => self.e = v, Mode::F => self.f = v,
            _ => {}
        }
    }
    fn write_operand_n(&mut self, bus: &mut dyn M6809Bus, n: u16, v: u8) {
        let m = self.mode;
        let byte = |old: u16| if n & 1 != 0 { (old & 0xFF00) | v as u16 } else { (old & 0x00FF) | ((v as u16) << 8) };
        match m {
            Mode::Ea => { let a = self.ea.wrapping_add(n); self.wr(bus, a, v) }
            Mode::D => { let r = byte(self.d()); self.set_d(r) }
            Mode::W => { let r = byte(self.w()); self.set_w(r) }
            Mode::X => self.x = byte(self.x),
            Mode::Y => self.y = byte(self.y),
            Mode::U => self.u = byte(self.u),
            Mode::S => self.s = byte(self.s),
            Mode::V => self.v = byte(self.v),
            Mode::Pc => self.pc = byte(self.pc),
            _ => {}
        }
    }
    fn is_reg_mode(&self) -> bool { !matches!(self.mode, Mode::Imm | Mode::Ea) }

    // ---- flags (MAME set_flags<T>) ----
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
    /// set_flags<uint32_t>(mask, r): N, Z (V always comes out clear).
    fn nz32(&mut self, mask: u8, r: u32) -> u32 {
        self.cc &= !mask;
        if mask & CC_N != 0 && r & 0x8000_0000 != 0 { self.cc |= CC_N; }
        if mask & CC_Z != 0 && r == 0 { self.cc |= CC_Z; }
        r
    }

    fn pending_interrupt(&self) -> u16 {
        if self.nmi_asserted { VECTOR_NMI }
        else if self.cc & CC_F == 0 && self.firq_line { VECTOR_FIRQ }
        else if self.cc & CC_I == 0 && self.irq_line { VECTOR_IRQ }
        else { 0 }
    }
    fn entire_state(&self) -> u16 { if self.native() { 0x3FF } else { 0xFF } }

    // ---- stack ----
    fn push_registers(&mut self, bus: &mut dyn M6809Bus, sp: R16, mask: u16) {
        let other = if sp == R16::S { self.u } else { self.s };
        let mut p = self.get16(sp);
        let mut push = |cpu: &mut Self, v: u8| { p = p.wrapping_sub(1); cpu.wr(bus, p, v); };
        if mask & 0x80 != 0 { let pc = self.pc; push(self, pc as u8); push(self, (pc >> 8) as u8); }
        if mask & 0x40 != 0 { push(self, other as u8); push(self, (other >> 8) as u8); }
        if mask & 0x20 != 0 { let y = self.y; push(self, y as u8); push(self, (y >> 8) as u8); }
        if mask & 0x10 != 0 { let x = self.x; push(self, x as u8); push(self, (x >> 8) as u8); }
        if mask & 0x08 != 0 { let v = self.dp; push(self, v); }
        if mask & 0x200 != 0 { let v = self.f; push(self, v); }
        if mask & 0x100 != 0 { let v = self.e; push(self, v); }
        if mask & 0x04 != 0 { let v = self.b; push(self, v); }
        if mask & 0x02 != 0 { let v = self.a; push(self, v); }
        if mask & 0x01 != 0 { let v = self.cc; push(self, v); }
        self.put16(sp, p);
    }

    fn pull_registers(&mut self, bus: &mut dyn M6809Bus, sp: R16, mask: u16) {
        let pull = |cpu: &mut Self, bus: &mut dyn M6809Bus| -> u8 {
            let p = cpu.get16(sp);
            let v = cpu.rd(bus, p);
            cpu.put16(sp, p.wrapping_add(1));
            v
        };
        if mask & 0x01 != 0 { self.cc = pull(self, bus); }
        if mask & 0x02 != 0 { self.a = pull(self, bus); }
        if mask & 0x04 != 0 { self.b = pull(self, bus); }
        if mask & 0x100 != 0 { self.e = pull(self, bus); }
        if mask & 0x200 != 0 { self.f = pull(self, bus); }
        if mask & 0x08 != 0 { self.dp = pull(self, bus); }
        if mask & 0x10 != 0 { let h = pull(self, bus); let l = pull(self, bus); self.x = u16::from_be_bytes([h, l]); }
        if mask & 0x20 != 0 { let h = pull(self, bus); let l = pull(self, bus); self.y = u16::from_be_bytes([h, l]); }
        if mask & 0x40 != 0 {
            let h = pull(self, bus); let l = pull(self, bus);
            let v = u16::from_be_bytes([h, l]);
            if sp == R16::S { self.u = v } else { self.s = v }
        }
        if mask & 0x80 != 0 { let h = pull(self, bus); let l = pull(self, bus); self.pc = u16::from_be_bytes([h, l]); }
        self.eat(1);
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
        if vector == VECTOR_NMI { self.nmi_asserted = false; }
        self.dummy_arg(bus);
        self.dummy_arg(bus);
        self.vma(bus, 1);
        match vector {
            VECTOR_FIRQ => {
                let mask = if self.md & 0x02 != 0 { self.cc |= CC_E; self.entire_state() }
                           else { self.cc &= !CC_E; 0x81 };
                self.push_registers(bus, R16::S, mask);
                self.cc |= CC_I | CC_F;
            }
            _ => {
                self.cc |= CC_E;
                let m = self.entire_state();
                self.push_registers(bus, R16::S, m);
                self.cc |= if vector == VECTOR_NMI { CC_I | CC_F } else { CC_I };
            }
        }
        self.set_ea(vector);
        self.interrupt_vector(bus);
    }

    /// SWI2 / SWI3 / illegal-instruction and divide-by-zero traps.
    fn software_interrupt(&mut self, bus: &mut dyn M6809Bus, vector: u16) {
        self.set_ea(vector);
        self.cc |= CC_E;
        self.dummy_arg(bus);
        self.vma(bus, 1);
        let m = self.entire_state();
        self.push_registers(bus, R16::S, m);
        self.interrupt_vector(bus);
    }
    fn illegal(&mut self, bus: &mut dyn M6809Bus) {
        self.md |= 0x40;
        self.software_interrupt(bus, VECTOR_ILLEGAL);
    }

    /// Runs one instruction (or takes an interrupt). Returns cycles used.
    pub fn step(&mut self, bus: &mut dyn M6809Bus) -> u32 {
        self.cycles = 0;
        match self.wait {
            Wait::Sync => {
                if !self.nmi_asserted && !self.firq_line && !self.irq_line { return 1; }
                self.wait = Wait::None;
                self.cycles += 1;
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
        if self.tfm {
            if self.pending_interrupt() != 0 {
                // abort: the TFM runs again after the interrupt returns
                self.tfm = false;
                self.pc = self.pc.wrapping_sub(3);
            } else {
                self.tfm_run(bus);
                return self.cycles;
            }
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
        if !self.native() { self.vma(bus, 1); }
    }
    fn extended(&mut self, bus: &mut dyn M6809Bus) {
        let h = self.arg(bus);
        let l = self.arg(bus);
        self.set_ea(u16::from_be_bytes([h, l]));
        if !self.native() { self.vma(bus, 1); }
    }
    fn imm_im(&mut self, bus: &mut dyn M6809Bus) { self.temp_im = self.arg(bus); }

    fn ireg(&self, post: u8) -> u16 {
        match post & 0x60 { 0x00 => self.x, 0x20 => self.y, 0x40 => self.u, _ => self.s }
    }
    fn set_ireg(&mut self, post: u8, v: u16) {
        match post & 0x60 { 0x00 => self.x = v, 0x20 => self.y = v, 0x40 => self.u = v, _ => self.s = v }
    }

    /// Indexed addressing. Returns false if the postbyte was illegal (trap taken).
    fn indexed(&mut self, bus: &mut dyn M6809Bus) -> bool {
        let post = self.arg(bus);
        self.opcode = post;
        let nat = self.native();
        let short = nat && post & 0x10 == 0;
        if post & 0x80 != 0 {
            let r = self.ireg(post);
            let t: u16 = match post & 0x7F {
                0x00 | 0x20 | 0x40 | 0x60 => {
                    self.set_ireg(post, r.wrapping_add(1)); self.eat(if short { 2 } else { 3 }); r
                }
                0x01 | 0x21 | 0x41 | 0x61 | 0x11 | 0x31 | 0x51 | 0x71 => {
                    self.set_ireg(post, r.wrapping_add(2)); self.eat(if short { 3 } else { 4 }); r
                }
                0x02 | 0x22 | 0x42 | 0x62 => {
                    let v = r.wrapping_sub(1); self.set_ireg(post, v); self.eat(if short { 2 } else { 3 }); v
                }
                0x03 | 0x23 | 0x43 | 0x63 | 0x13 | 0x33 | 0x53 | 0x73 => {
                    let v = r.wrapping_sub(2); self.set_ireg(post, v); self.eat(if short { 3 } else { 4 }); v
                }
                p if p & 0x0F == 0x04 => { self.eat(1); r }
                p if p & 0x0F == 0x05 => { self.eat(2); r.wrapping_add(self.b as i8 as u16) }
                p if p & 0x0F == 0x06 => { self.eat(2); r.wrapping_add(self.a as i8 as u16) }
                p if p & 0x0F == 0x07 => { self.eat(2); r.wrapping_add(self.e as i8 as u16) }
                p if p & 0x0F == 0x08 => { let o = self.arg(bus) as i8 as u16; self.eat(1); r.wrapping_add(o) }
                p if p & 0x0F == 0x09 => {
                    let h = self.arg(bus); let l = self.arg(bus);
                    self.eat(if nat { 2 } else { 3 });
                    r.wrapping_add(u16::from_be_bytes([h, l]))
                }
                p if p & 0x0F == 0x0A => { self.eat(2); r.wrapping_add(self.f as i8 as u16) }
                p if p & 0x0F == 0x0B => { self.eat(if short { 3 } else { 5 }); r.wrapping_add(self.d()) }
                p if p & 0x0F == 0x0C => { let o = self.arg(bus) as i8 as u16; self.eat(1); self.pc.wrapping_add(o) }
                p if p & 0x0F == 0x0D => {
                    let h = self.arg(bus); let l = self.arg(bus);
                    self.eat(if short { 2 } else { 4 });
                    self.pc.wrapping_add(u16::from_be_bytes([h, l]))
                }
                p if p & 0x0F == 0x0E => { self.eat(if short { 2 } else { 5 }); r.wrapping_add(self.w()) }
                0x1F | 0x3F | 0x5F | 0x7F => {
                    let h = self.arg(bus); let l = self.arg(bus); self.eat(1); u16::from_be_bytes([h, l])
                }
                0x0F | 0x10 => { self.eat(1); self.w() }
                0x2F | 0x30 => {
                    let h = self.arg(bus); let l = self.arg(bus);
                    self.eat(if nat { 1 } else { 5 });
                    self.w().wrapping_add(u16::from_be_bytes([h, l]))
                }
                0x4F | 0x50 => {
                    let w = self.w(); self.set_w(w.wrapping_add(2)); self.eat(if short { 2 } else { 4 }); w
                }
                0x6F | 0x70 => {
                    let w = self.w().wrapping_sub(2); self.set_w(w); self.eat(if short { 2 } else { 4 }); w
                }
                _ => { self.illegal(bus); return false; }
            };
            self.temp = t;
            if post & 0x10 != 0 {
                self.set_ea(t);
                let h = self.read_operand_n(bus, 0);
                let l = self.read_operand_n(bus, 1);
                self.temp = u16::from_be_bytes([h, l]);
                self.eat(1);
            }
        } else {
            let off = ((post & 0x0F) | if post & 0x10 != 0 { 0xF0 } else { 0 }) as i8 as u16;
            self.temp = self.ireg(post).wrapping_add(off);
            self.eat(2);
        }
        let t = self.temp;
        self.set_ea(t);
        true
    }

    /// Operand setup for the 0x80-0xFF columns (imm / direct / indexed / extended).
    /// Returns false if an illegal indexed postbyte trapped.
    fn operand_mode(&mut self, bus: &mut dyn M6809Bus, op: u8) -> bool {
        match op & 0x30 {
            0x00 => { self.mode = Mode::Imm; true }
            0x10 => { self.direct(bus); true }
            0x20 => self.indexed(bus),
            _ => { self.extended(bus); true }
        }
    }

    /// Operand setup for 0x00-0x7F read-modify-write opcodes.
    fn rmw_mode(&mut self, bus: &mut dyn M6809Bus, op: u8, ra: Mode, rb: Mode) -> bool {
        match op & 0xF0 {
            0x00 => { self.direct(bus); true }
            0x40 => { self.mode = ra; true }
            0x50 => { self.mode = rb; true }
            0x60 => self.indexed(bus),
            _ => { self.extended(bus); true }
        }
    }

    // ---- page 1 ----
    fn dispatch01(&mut self, bus: &mut dyn M6809Bus) {
        let op = self.opcode;
        match op {
            0x00..=0x0F | 0x40..=0x7F => {
                let col = op & 0x0F;
                let reg = matches!(op & 0xF0, 0x40 | 0x50);
                if reg && matches!(col, 0x1 | 0x2 | 0x5 | 0xB | 0xE) { return self.illegal(bus); }
                if matches!(col, 0x1 | 0x2 | 0x5 | 0xB) { self.imm_im(bus); }
                if !self.rmw_mode(bus, op, Mode::A, Mode::B) { return; }
                match col {
                    0x0 => self.neg8(bus),
                    0x1 => self.logic_im(bus, |a, b| a | b),
                    0x2 => self.logic_im(bus, |a, b| a & b),
                    0x3 => self.com8(bus),
                    0x4 => self.lsr8(bus),
                    0x5 => self.logic_im(bus, |a, b| a ^ b),
                    0x6 => self.ror8(bus),
                    0x7 => self.asr8(bus),
                    0x8 => self.asl8(bus),
                    0x9 => self.rol8(bus),
                    0xA => self.dec8(bus),
                    0xB => self.tim(bus),
                    0xC => self.inc8(bus),
                    0xD => self.tst8(bus),
                    0xE => self.pc = self.ea,
                    _ => self.clr8(bus),
                }
            }
            0x10 => self.dispatch10(bus),
            0x11 => self.dispatch11(bus),
            0x12 => self.emu_dummy(bus),
            0x13 => { self.dummy_arg(bus); self.wait = Wait::Sync; }
            0x14 => {
                // SEXW
                let r = if self.w() & 0x8000 != 0 { 0xFFFF } else { 0 };
                let r = self.nz16(CC_N, r);
                self.set_d(r);
                if r == 0 && self.w() == 0 { self.cc |= CC_Z } else { self.cc &= !CC_Z }
            }
            0x16 => self.lbranch(bus, true),
            0x17 => {
                let h = self.arg(bus); let l = self.arg(bus);
                self.ea = self.pc.wrapping_add(u16::from_be_bytes([h, l]));
                let n = if self.native() { 2 } else { 4 };
                self.vma(bus, n);
                self.goto_subroutine(bus);
            }
            0x19 => { self.daa(); self.emu_dummy(bus); }
            0x1A => { self.mode = Mode::Imm; let v = self.read_operand(bus); self.cc |= v; self.emu_dummy(bus); }
            0x1C => { self.mode = Mode::Imm; let v = self.read_operand(bus); self.cc &= v; self.emu_dummy(bus); }
            0x1D => { let v = self.b as i8 as i16 as u16; let r = self.nz16(NZ, v); self.set_d(r); self.emu_dummy(bus); }
            0x1E => {
                let p = self.arg(bus);
                let r1 = self.read_exgtfr(p >> 4);
                let r2 = self.read_exgtfr(p);
                self.write_exgtfr(p >> 4, r2);
                self.write_exgtfr(p, r1);
                self.eat(if self.native() { 3 } else { 6 });
            }
            0x1F => {
                let p = self.arg(bus);
                let r = self.read_exgtfr(p >> 4);
                self.write_exgtfr(p, r);
                self.eat(if self.native() { 2 } else { 4 });
            }
            0x20..=0x2F => { let c = self.cond(op); self.branch(bus, c); }
            0x30..=0x33 => {
                if !self.indexed(bus) { return; }
                let ea = self.ea;
                match op {
                    0x30 => self.x = self.nz16(CC_Z, ea),
                    0x31 => self.y = self.nz16(CC_Z, ea),
                    0x32 => { self.lds_encountered = true; self.s = ea }
                    _ => self.u = ea,
                }
                self.vma(bus, 1);
            }
            0x34 | 0x36 => {
                let m = self.arg(bus) as u16;
                self.vma(bus, 2);
                let sp = if op == 0x34 { R16::S } else { R16::U };
                if !self.native() { let p = self.get16(sp); self.rd(bus, p); }
                self.push_registers(bus, sp, m);
            }
            0x35 | 0x37 => {
                let m = self.arg(bus) as u16;
                let n = if self.native() { 1 } else { 2 };
                self.vma(bus, n);
                self.pull_registers(bus, if op == 0x35 { R16::S } else { R16::U }, m);
            }
            0x39 => { self.emu_dummy(bus); self.pull_registers(bus, R16::S, 0x80); }
            0x3A => {
                self.x = self.x.wrapping_add(self.b as u16);
                if !self.native() { self.dummy_arg(bus); self.vma(bus, 1); }
            }
            0x3B => {
                self.dummy_arg(bus);
                let p = self.s;
                self.cc = self.rd(bus, p);
                self.s = p.wrapping_add(1);
                let m = if self.cc & CC_E != 0 { self.entire_state() } else { 0x81 } & !0x01;
                self.pull_registers(bus, R16::S, m);
            }
            0x3C => {
                let v = self.arg(bus);
                self.cc &= v;
                self.dummy_arg(bus);
                self.vma(bus, 1);
                self.cc |= CC_E;
                let m = self.entire_state();
                self.push_registers(bus, R16::S, m);
                self.wait = Wait::Cwai;
            }
            0x3D => {
                let r = (self.a as u16) * (self.b as u16);
                let r = self.nz16(CC_Z, r);
                self.set_d(r);
                if r & 0x80 != 0 { self.cc |= CC_C } else { self.cc &= !CC_C }
                self.dummy_arg(bus);
                let n = if self.native() { 8 } else { 9 };
                self.vma(bus, n);
            }
            0x3F => {
                self.set_ea(VECTOR_SWI);
                self.cc |= CC_E;
                self.dummy_arg(bus);
                self.vma(bus, 1);
                let m = self.entire_state();
                self.push_registers(bus, R16::S, m);
                self.cc |= CC_I | CC_F;
                self.interrupt_vector(bus);
            }
            0x8D => {
                let o = self.arg(bus) as i8 as u16;
                self.ea = self.pc.wrapping_add(o);
                let n = if self.native() { 2 } else { 3 };
                self.vma(bus, n);
                self.goto_subroutine(bus);
            }
            0x80..=0xFF => self.page1_high(bus, op),
            _ => self.illegal(bus),
        }
    }

    fn page1_high(&mut self, bus: &mut dyn M6809Bus, op: u8) {
        let col = op & 0x0F;
        let hi = op >= 0xC0;
        let imm = op & 0x30 == 0;
        let r8 = if hi { R8::B } else { R8::A };
        // illegal: immediate stores (87, 8F, C7, CF)
        if imm && matches!(col, 0x7 | 0xF) { return self.illegal(bus); }
        match (col, hi) {
            (0x3, _) => { self.set_reg16(R16::D); }
            (0xC, false) => self.set_reg16(R16::X),
            (0xC, true) | (0xD, true) => self.set_reg16(R16::D),
            (0xE, false) | (0xF, false) => self.set_reg16(R16::X),
            (0xE, true) | (0xF, true) => self.set_reg16(R16::U),
            (0xD, false) => {}
            _ => self.set_reg8(r8),
        }
        if op == 0xCD {
            self.mode = Mode::Imm;
            return self.ldq(bus);
        }
        if !self.operand_mode(bus, op) { return; }
        match (col, hi) {
            (0x0, _) => self.sub8(bus, false, true),
            (0x1, _) => self.sub8(bus, false, false),
            (0x2, _) => self.sub8(bus, true, true),
            (0x3, false) => self.arith16(bus, 1),
            (0x3, true) => self.arith16(bus, 0),
            (0x4, _) => self.logic8(bus, |a, b| a & b, true),
            (0x5, _) => self.logic8(bus, |a, b| a & b, false),
            (0x6, _) => self.ld8(bus),
            (0x7, _) => self.st8(bus),
            (0x8, _) => self.logic8(bus, |a, b| a ^ b, true),
            (0x9, _) => self.add8(bus, true),
            (0xA, _) => self.logic8(bus, |a, b| a | b, true),
            (0xB, _) => self.add8(bus, false),
            (0xC, false) => self.arith16(bus, 2),
            (0xC, true) => self.ld16(bus),
            (0xD, false) => {
                // JSR (indexed: MAME takes one cycle off in native mode)
                if op == 0xAD && self.native() { self.eat(-1); }
                self.jsr(bus)
            }
            (0xD, true) => self.st16(bus),
            (0xE, _) => self.ld16(bus),
            _ => self.st16(bus),
        }
    }

    // ---- page 2 (0x10) ----
    fn dispatch10(&mut self, bus: &mut dyn M6809Bus) {
        self.opcode = self.fetch_op(bus);
        let op = self.opcode;
        match op {
            0x21..=0x2F => { let c = self.cond(op); self.lbranch(bus, c); }
            0x30..=0x37 => self.register_register(bus, op),
            0x38..=0x3B => {
                self.eat(2);
                let sp = if op < 0x3A { R16::S } else { R16::U };
                if op & 1 == 0 { self.push_registers(bus, sp, 0x300) } else { self.pull_registers(bus, sp, 0x300) }
            }
            0x3F => self.software_interrupt(bus, VECTOR_SWI2),
            0x40..=0x5F => {
                let col = op & 0x0F;
                if !matches!(col, 0x0 | 0x3 | 0x4 | 0x6 | 0x7 | 0x8 | 0x9 | 0xA | 0xC | 0xD | 0xF) { return self.illegal(bus); }
                self.mode = if op < 0x50 { Mode::D } else { Mode::W };
                match col {
                    0x0 => self.neg16(bus),
                    0x3 => self.com16(bus),
                    0x4 => self.shift16(bus, 0),
                    0x6 => self.shift16(bus, 1),
                    0x7 => self.shift16(bus, 2),
                    0x8 => self.shift16(bus, 3),
                    0x9 => self.shift16(bus, 4),
                    0xA => self.incdec16(bus, false),
                    0xC => self.incdec16(bus, true),
                    0xD => self.tst16(bus),
                    _ => self.clr16(bus),
                }
            }
            0x80..=0xBF => {
                let col = op & 0x0F;
                let imm = op & 0x30 == 0;
                if col == 0xD || (imm && matches!(col, 0x7 | 0xF)) { return self.illegal(bus); }
                self.set_reg16(match col {
                    0x0 | 0x1 | 0x6 | 0x7 | 0xB => R16::W,
                    0xC | 0xE | 0xF => R16::Y,
                    _ => R16::D,
                });
                if !self.operand_mode(bus, op) { return; }
                match col {
                    0x0 => self.arith16(bus, 1),
                    0x1 | 0x3 | 0xC => self.arith16(bus, 2),
                    0x2 => self.adc_sbc16(bus, false),
                    0x4 => self.logic16(bus, |a, b| a & b, true),
                    0x5 => self.logic16(bus, |a, b| a & b, false),
                    0x6 | 0xE => self.ld16(bus),
                    0x7 | 0xF => self.st16(bus),
                    0x8 => self.logic16(bus, |a, b| a ^ b, true),
                    0x9 => self.adc_sbc16(bus, true),
                    0xA => self.logic16(bus, |a, b| a | b, true),
                    _ => self.arith16(bus, 0),
                }
            }
            0xCE | 0xDE | 0xEE | 0xFE | 0xDF | 0xEF | 0xFF => {
                self.set_reg16(R16::S);
                if !self.operand_mode(bus, op) { return; }
                if op & 1 == 0 { self.ld16(bus) } else { self.st16(bus) }
            }
            0xDC | 0xEC | 0xFC | 0xDD | 0xED | 0xFD => {
                if !self.operand_mode(bus, op) { return; }
                if op & 1 == 0 { self.ldq(bus) } else { self.stq(bus) }
            }
            _ => self.illegal(bus),
        }
    }

    // ---- page 3 (0x11) ----
    fn dispatch11(&mut self, bus: &mut dyn M6809Bus) {
        self.opcode = self.fetch_op(bus);
        let op = self.opcode;
        match op {
            0x30..=0x37 => self.bit_op(bus, op),
            0x38..=0x3B => self.tfm_start(bus),
            0x3C | 0x3D => {
                self.set_reg8(R8::Md);
                self.mode = Mode::Imm;
                if op == 0x3C { self.logic8(bus, |a, b| a & b, false) } else { self.ld8(bus) }
            }
            0x3F => self.software_interrupt(bus, VECTOR_SWI3),
            0x43 | 0x4A | 0x4C | 0x4D | 0x4F | 0x53 | 0x5A | 0x5C | 0x5D | 0x5F => {
                self.mode = if op < 0x50 { Mode::E } else { Mode::F };
                match op & 0x0F {
                    0x3 => self.com8(bus),
                    0xA => self.dec8(bus),
                    0xC => self.inc8(bus),
                    0xD => self.tst8(bus),
                    _ => self.clr8(bus),
                }
            }
            0x80..=0xFF => {
                let col = op & 0x0F;
                let imm = op & 0x30 == 0;
                let hi = op >= 0xC0;
                let ok = if hi { matches!(col, 0x0 | 0x1 | 0x6 | 0x7 | 0xB) }
                         else { matches!(col, 0x0 | 0x1 | 0x3 | 0x6 | 0x7 | 0xB | 0xC | 0xD | 0xE | 0xF) };
                if !ok || (imm && col == 0x7) { return self.illegal(bus); }
                match col {
                    0x3 => self.set_reg16(R16::U),
                    0xC => self.set_reg16(R16::S),
                    0xD..=0xF => {}
                    _ => self.set_reg8(if hi { R8::F } else { R8::E }),
                }
                if !self.operand_mode(bus, op) { return; }
                match col {
                    0x0 => self.sub8(bus, false, true),
                    0x1 => self.sub8(bus, false, false),
                    0x3 | 0xC => self.arith16(bus, 2),
                    0x6 => self.ld8(bus),
                    0x7 => self.st8(bus),
                    0xB => self.add8(bus, false),
                    0xD => self.divd(bus),
                    0xE => self.divq(bus),
                    _ => self.muld(bus),
                }
            }
            _ => self.illegal(bus),
        }
    }

    // ---- shared instruction bodies (MAME base6x09.lst / hd6309.lst) ----
    fn cond(&self, op: u8) -> bool {
        let c = self.cc;
        let n = c & CC_N != 0; let z = c & CC_Z != 0; let v = c & CC_V != 0; let cy = c & CC_C != 0;
        let t = match (op >> 1) & 7 {
            0 => true,
            1 => c & (CC_Z | CC_C) == 0,
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
            if !self.native() { self.vma(bus, 1); }
        }
    }
    fn goto_subroutine(&mut self, bus: &mut dyn M6809Bus) {
        let pc = self.pc;
        self.s = self.s.wrapping_sub(1); let s = self.s; self.wr(bus, s, pc as u8);
        self.s = self.s.wrapping_sub(1); let s = self.s; self.wr(bus, s, (pc >> 8) as u8);
        self.pc = self.ea;
    }
    fn jsr(&mut self, bus: &mut dyn M6809Bus) {
        self.dummy_arg(bus);
        self.vma(bus, 1);
        self.goto_subroutine(bus);
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
        self.a = self.nz8(NZ, t as u8);
    }

    fn read_exgtfr(&self, r: u8) -> u16 {
        let dup = |v: u8| ((v as u16) << 8) | v as u16;
        match r & 0x0F {
            0 => self.d(), 1 => self.x, 2 => self.y, 3 => self.u, 4 => self.s, 5 => self.pc,
            6 => self.w(), 7 => self.v,
            8 => dup(self.a), 9 => dup(self.b), 10 => dup(self.cc), 11 => dup(self.dp),
            14 => dup(self.e), 15 => dup(self.f),
            _ => 0,
        }
    }
    fn write_exgtfr(&mut self, r: u8, v: u16) {
        match r & 0x0F {
            0 => self.set_d(v), 1 => self.x = v, 2 => self.y = v, 3 => self.u = v, 4 => self.s = v,
            5 => self.pc = v, 6 => self.set_w(v), 7 => self.v = v,
            8 => self.a = (v >> 8) as u8, 9 => self.b = v as u8,
            10 => self.cc = v as u8, 11 => self.dp = (v >> 8) as u8,
            14 => self.e = (v >> 8) as u8, 15 => self.f = v as u8,
            _ => {}
        }
    }

    /// ADDR/ADCR/SUBR/SBCR/ANDR/ORR/EORR/CMPR (10 30-37).
    fn register_register(&mut self, bus: &mut dyn M6809Bus, op: u8) {
        let p = self.arg(bus);
        let promote = (p & 0x80 != 0) != (p & 0x08 != 0);
        self.temp = 0;
        match p & 0x0F {
            0 => self.set_reg16(R16::D),
            1 => self.set_reg16(R16::X),
            2 => self.set_reg16(R16::Y),
            3 => self.set_reg16(R16::U),
            4 => self.set_reg16(R16::S),
            5 => self.set_reg16(R16::Pc),
            6 => self.set_reg16(R16::W),
            7 => self.set_reg16(R16::V),
            8 => if promote { self.set_reg16(R16::D) } else { self.set_reg8(R8::A) },
            9 => if promote { self.set_reg16(R16::D) } else { self.set_reg8(R8::B) },
            10 => if promote { self.set_reg16(R16::Temp) } else { self.set_reg8(R8::Cc) },
            11 => if promote { self.set_reg16(R16::Temp) } else { self.set_reg8(R8::Dp) },
            12 | 13 => if promote { self.set_reg16(R16::Temp) } else { self.set_reg8(R8::Temp) },
            14 => if promote { self.set_reg16(R16::W) } else { self.set_reg8(R8::E) },
            _ => if promote { self.set_reg16(R16::W) } else { self.set_reg8(R8::F) },
        }
        self.mode = match p >> 4 {
            0 => Mode::D, 1 => Mode::X, 2 => Mode::Y, 3 => Mode::U, 4 => Mode::S, 5 => Mode::Pc,
            6 => Mode::W, 7 => Mode::V,
            8 => if promote { Mode::D } else { Mode::A },
            9 => if promote { Mode::D } else { Mode::B },
            10 => if promote { Mode::Zero } else { Mode::Cc },
            11 => if promote { Mode::Zero } else { Mode::Dp },
            12 | 13 => Mode::Zero,
            14 => if promote { Mode::W } else { Mode::E },
            _ => if promote { Mode::W } else { Mode::F },
        };
        self.eat(1);
        if self.reg_is16 {
            match op {
                0x30 => self.arith16(bus, 0),
                0x31 => self.adc_sbc16(bus, true),
                0x32 => self.arith16(bus, 1),
                0x33 => self.adc_sbc16(bus, false),
                0x34 => self.logic16(bus, |a, b| a & b, true),
                0x35 => self.logic16(bus, |a, b| a | b, true),
                0x36 => self.logic16(bus, |a, b| a ^ b, true),
                _ => self.arith16(bus, 2),
            }
        } else {
            match op {
                0x30 => self.add8(bus, false),
                0x31 => self.add8(bus, true),
                0x32 => self.sub8(bus, false, true),
                0x33 => self.sub8(bus, true, true),
                0x34 => self.logic8(bus, |a, b| a & b, true),
                0x35 => self.logic8(bus, |a, b| a | b, true),
                0x36 => self.logic8(bus, |a, b| a ^ b, true),
                _ => self.sub8(bus, false, false),
            }
        }
    }

    // 8-bit read-modify-write
    fn rmw_finish(&mut self, bus: &mut dyn M6809Bus, r: u8, always_dummy: bool) {
        if !self.native() || (!always_dummy && !self.is_reg_mode()) { self.dummy_arg(bus); }
        self.write_operand(bus, r);
    }
    fn neg8(&mut self, bus: &mut dyn M6809Bus) {
        let t = self.read_operand(bus);
        let r = self.flags8(NZVC, 0, t, (t as u32).wrapping_neg());
        self.rmw_finish(bus, r, true);
    }
    fn com8(&mut self, bus: &mut dyn M6809Bus) {
        let t = self.read_operand(bus);
        self.cc &= !CC_V;
        self.cc |= CC_C;
        let r = self.nz8(NZ, !t);
        self.rmw_finish(bus, r, true);
    }
    fn lsr8(&mut self, bus: &mut dyn M6809Bus) {
        let t = self.read_operand(bus);
        if t & 1 != 0 { self.cc |= CC_C } else { self.cc &= !CC_C }
        let r = self.nz8(NZ, t >> 1);
        self.rmw_finish(bus, r, true);
    }
    fn ror8(&mut self, bus: &mut dyn M6809Bus) {
        let t = self.read_operand(bus);
        let v = (t >> 1) | if self.cc & CC_C != 0 { 0x80 } else { 0 };
        if t & 1 != 0 { self.cc |= CC_C } else { self.cc &= !CC_C }
        let r = self.nz8(NZ, v);
        self.rmw_finish(bus, r, true);
    }
    fn asr8(&mut self, bus: &mut dyn M6809Bus) {
        let t = self.read_operand(bus);
        if t & 1 != 0 { self.cc |= CC_C } else { self.cc &= !CC_C }
        let r = self.nz8(NZ, ((t as i8) >> 1) as u8);
        self.rmw_finish(bus, r, true);
    }
    fn asl8(&mut self, bus: &mut dyn M6809Bus) {
        let t = self.read_operand(bus);
        let r = self.flags8(NZVC, t, t, (t as u32) << 1);
        self.rmw_finish(bus, r, true);
    }
    fn rol8(&mut self, bus: &mut dyn M6809Bus) {
        let t = self.read_operand(bus);
        let v = ((t as u32) << 1) | (self.cc & CC_C) as u32;
        if t & 0x80 != 0 { self.cc |= CC_C } else { self.cc &= !CC_C }
        let r = self.flags8(NZV, t, t, v);
        self.rmw_finish(bus, r, true);
    }
    fn dec8(&mut self, bus: &mut dyn M6809Bus) {
        let t = self.read_operand(bus);
        let r = self.flags8(NZV, t, 1, (t as u32).wrapping_sub(1));
        self.rmw_finish(bus, r, false);
    }
    fn inc8(&mut self, bus: &mut dyn M6809Bus) {
        let t = self.read_operand(bus);
        let r = self.flags8(NZV, t, 1, t as u32 + 1);
        self.rmw_finish(bus, r, false);
    }
    fn tst8(&mut self, bus: &mut dyn M6809Bus) {
        let t = self.read_operand(bus);
        self.nz8(NZV, t);
        if !self.native() { self.vma(bus, 1); }
        if !self.is_reg_mode() { self.vma(bus, 1); }
    }
    fn clr8(&mut self, bus: &mut dyn M6809Bus) {
        self.read_operand(bus);
        self.cc &= !NZVC;
        self.cc |= CC_Z;
        self.rmw_finish(bus, 0, false);
    }
    /// OIM / AIM / EIM
    fn logic_im(&mut self, bus: &mut dyn M6809Bus, f: fn(u8, u8) -> u8) {
        let t = self.read_operand(bus);
        self.cc &= !CC_V;
        let r = f(t, self.temp_im);
        let r = self.flags8(NZ, 0, t, r as u32);
        self.eat(1);
        self.write_operand(bus, r);
    }
    fn tim(&mut self, bus: &mut dyn M6809Bus) {
        let t = self.read_operand(bus);
        self.cc &= !CC_V;
        let r = t & self.temp_im;
        self.flags8(NZ, 0, t, r as u32);
        self.eat(2);
    }

    // 16-bit read-modify-write (D / W)
    fn read16_temp(&mut self, bus: &mut dyn M6809Bus) -> u16 {
        let h = self.read_operand_n(bus, 0);
        self.set_temp_hi(h);
        let l = self.read_operand_n(bus, 1);
        self.set_temp_lo(l);
        self.temp
    }
    fn write16(&mut self, bus: &mut dyn M6809Bus, v: u16) {
        self.write_operand_n(bus, 0, (v >> 8) as u8);
        self.write_operand_n(bus, 1, v as u8);
    }
    fn neg16(&mut self, bus: &mut dyn M6809Bus) {
        let t = self.read16_temp(bus);
        let r = self.flags16(NZVC, 0, t, (t as u32).wrapping_neg());
        self.emu_dummy(bus);
        self.write16(bus, r);
    }
    fn com16(&mut self, bus: &mut dyn M6809Bus) {
        let t = self.read16_temp(bus);
        self.cc &= !CC_V;
        self.cc |= CC_C;
        let r = self.nz16(NZ, !t);
        self.eat(if self.native() { 0 } else { 1 });
        self.write16(bus, r);
    }
    /// kind: 0 LSR, 1 ROR, 2 ASR, 3 ASL, 4 ROL
    fn shift16(&mut self, bus: &mut dyn M6809Bus, kind: u8) {
        let t = self.read16_temp(bus);
        let c = self.cc & CC_C != 0;
        let r = match kind {
            0 => { if t & 1 != 0 { self.cc |= CC_C } else { self.cc &= !CC_C } self.nz16(NZ, t >> 1) }
            1 => {
                let v = (t >> 1) | if c { 0x8000 } else { 0 };
                if t & 1 != 0 { self.cc |= CC_C } else { self.cc &= !CC_C }
                self.nz16(NZ, v)
            }
            2 => { if t & 1 != 0 { self.cc |= CC_C } else { self.cc &= !CC_C } self.nz16(NZ, ((t as i16) >> 1) as u16) }
            3 => self.flags16(NZVC, t, t, (t as u32) << 1),
            _ => {
                // set_flags(mask, (uint16_t) r): V never comes out set
                let v = (t << 1) | c as u16;
                if t & 0x8000 != 0 { self.cc |= CC_C } else { self.cc &= !CC_C }
                self.nz16(NZV, v)
            }
        };
        self.emu_dummy(bus);
        self.write16(bus, r);
    }
    fn incdec16(&mut self, bus: &mut dyn M6809Bus, inc: bool) {
        let t = self.read16_temp(bus);
        let v = if inc { t as u32 + 1 } else { (t as u32).wrapping_sub(1) };
        let r = self.flags16(NZVC, t, 1, v);
        self.emu_dummy(bus);
        self.write16(bus, r);
    }
    fn tst16(&mut self, bus: &mut dyn M6809Bus) {
        let t = self.read16_temp(bus);
        self.nz16(NZV, t);
        if !self.native() { self.vma(bus, 1); }
        if !self.is_reg_mode() { self.vma(bus, 1); }
    }
    fn clr16(&mut self, bus: &mut dyn M6809Bus) {
        self.emu_dummy(bus);
        self.cc &= !NZVC;
        self.cc |= CC_Z;
        self.write16(bus, 0);
    }

    // 8-bit accumulator ops
    fn sub8(&mut self, bus: &mut dyn M6809Bus, with_carry: bool, store: bool) {
        let t = self.read_operand(bus);
        self.set_temp_lo(t);
        let a = self.regop8();
        let c = if with_carry && self.cc & CC_C != 0 { 1 } else { 0 };
        let v = (a as u32).wrapping_sub(t as u32).wrapping_sub(c);
        let r = self.flags8(NZVC, a, t, v);
        if store { self.set_regop8(r); }
    }
    fn add8(&mut self, bus: &mut dyn M6809Bus, with_carry: bool) {
        let t = self.read_operand(bus);
        self.set_temp_lo(t);
        let a = self.regop8();
        let c = if with_carry && self.cc & CC_C != 0 { 1 } else { 0 };
        let mask = if self.opcode & 0xFE != 0x30 { CC_H | NZVC } else { NZVC };
        let r = self.flags8(mask, a, t, a as u32 + t as u32 + c);
        self.set_regop8(r);
    }
    fn logic8(&mut self, bus: &mut dyn M6809Bus, f: fn(u8, u8) -> u8, store: bool) {
        self.cc &= !CC_V;
        let t = self.read_operand(bus);
        let a = self.regop8();
        let r = self.flags8(NZ, 0, a, f(a, t) as u32);
        if store { self.set_regop8(r); }
    }
    fn ld8(&mut self, bus: &mut dyn M6809Bus) {
        let v = self.read_operand(bus);
        self.set_regop8(v);
        let v = self.regop8();
        self.nz8(NZV, v);
    }
    fn st8(&mut self, bus: &mut dyn M6809Bus) {
        let v = self.regop8();
        let v = self.nz8(NZV, v);
        let a = self.ea;
        self.wr(bus, a, v);
    }

    // 16-bit accumulator ops
    /// kind: 0 ADD, 1 SUB, 2 CMP
    fn arith16(&mut self, bus: &mut dyn M6809Bus, kind: u8) {
        let t = self.read16_temp(bus);
        let a = self.regop16();
        let v = if kind == 0 { a as u32 + t as u32 } else { (a as u32).wrapping_sub(t as u32) };
        let r = self.flags16(NZVC, a, t, v);
        if kind != 2 { self.set_regop16(r); }
        self.emu_dummy(bus);
    }
    fn adc_sbc16(&mut self, bus: &mut dyn M6809Bus, add: bool) {
        let t = self.read16_temp(bus);
        let a = self.regop16();
        let c = (self.cc & CC_C) as u32;
        let v = if add { a as u32 + t as u32 + c } else { (a as u32).wrapping_sub(t as u32).wrapping_sub(c) };
        let r = self.flags16(NZVC, a, t, v);
        self.set_regop16(r);
        self.eat(if self.native() { 0 } else { 1 });
    }
    fn logic16(&mut self, bus: &mut dyn M6809Bus, f: fn(u16, u16) -> u16, store: bool) {
        let t = self.read16_temp(bus);
        self.cc &= !CC_V;
        let a = self.regop16();
        let r = self.flags16(NZ, 0, a, f(a, t) as u32);
        if store { self.set_regop16(r); }
    }
    fn ld16(&mut self, bus: &mut dyn M6809Bus) {
        let h = self.read_operand_n(bus, 0);
        let l = self.read_operand_n(bus, 1);
        self.set_regop16(u16::from_be_bytes([h, l]));
        let v = self.regop16();
        self.nz16(NZV, v);
        if self.reg16 == R16::S { self.lds_encountered = true; }
    }
    fn st16(&mut self, bus: &mut dyn M6809Bus) {
        let v = self.regop16();
        self.write_operand_n(bus, 0, (v >> 8) as u8);
        self.write_operand_n(bus, 1, v as u8);
        self.nz16(NZV, v);
    }
    fn ldq(&mut self, bus: &mut dyn M6809Bus) {
        self.a = self.read_operand_n(bus, 0);
        self.b = self.read_operand_n(bus, 1);
        self.e = self.read_operand_n(bus, 2);
        self.f = self.read_operand_n(bus, 3);
        let q = self.q();
        self.nz32(NZV, q);
    }
    fn stq(&mut self, bus: &mut dyn M6809Bus) {
        let [a, b, e, f] = [self.a, self.b, self.e, self.f];
        self.write_operand_n(bus, 0, a);
        self.write_operand_n(bus, 1, b);
        self.write_operand_n(bus, 2, e);
        self.write_operand_n(bus, 3, f);
        let q = self.q();
        self.nz32(NZV, q);
    }

    // multiply / divide
    fn muld(&mut self, bus: &mut dyn M6809Bus) {
        let t = self.read16_temp(bus);
        let r = (self.d() as i16 as i32).wrapping_mul(t as i16 as i32) as u32;
        let r = self.nz32(NZ, r);
        self.set_q(r);
        self.cc &= !(CC_V | CC_C);
    }
    fn divide_by_zero(&mut self, bus: &mut dyn M6809Bus) {
        self.md |= 0x80;
        self.software_interrupt(bus, VECTOR_ILLEGAL);
    }
    fn divd(&mut self, bus: &mut dyn M6809Bus) {
        let t = self.read_operand(bus);
        self.set_temp_lo(t);
        let mut target: i32 = if self.native() { 24 } else { 25 };
        if t == 0 {
            if !self.native() { target -= 2; }
            self.eat(target);
            return self.divide_by_zero(bus);
        }
        let mut tmp1 = self.d();
        let mut tmp2 = t;
        let mut nsign = false;
        let mut vsign = false;
        if tmp1 & 0x8000 != 0 {
            tmp1 = (!tmp1).wrapping_add(1);
            self.set_d(tmp1);
            nsign = true;
            target += 1;
        }
        if tmp2 & 0x80 != 0 {
            tmp2 = (!tmp2).wrapping_add(1);
            vsign = true;
            target += 1;
        }
        let mut quotient: u16 = tmp1 / tmp2 as u16;
        let mut remainder: u8 = (tmp1 % tmp2 as u16) as u8;
        self.cc &= !NZVC;
        if quotient >> 8 != 0 {
            self.cc |= CC_V;
            if nsign { self.cc |= CC_N; }
            target -= 13;
            self.eat(target);
            return;
        }
        if nsign { remainder = (!remainder).wrapping_add(1); }
        if nsign != vsign {
            let nq = (!quotient).wrapping_add(1);
            if nq & 0x80 != 0 && quotient & 0x80 == 0 { quotient = nq; }
        }
        self.a = remainder;
        self.b = quotient as u8;
        if self.b & 1 != 0 { self.cc |= CC_C; }
        if ((quotient >> 15) ^ (self.b as u16 >> 7)) & 1 == 0 {
            if self.b == 0 { self.cc |= CC_Z; }
            if self.b & 0x80 != 0 { self.cc |= CC_N; }
        } else {
            self.cc |= CC_N | CC_V;
            target -= 1;
        }
        self.eat(target);
    }
    fn divq(&mut self, bus: &mut dyn M6809Bus) {
        let t = self.read16_temp(bus);
        let mut target: i32 = if self.native() { 24 } else { 25 };
        if t == 0 {
            if !self.native() { target -= 2; }
            self.eat(target);
            return self.divide_by_zero(bus);
        }
        let mut tmp1 = self.q();
        let mut tmp2 = t;
        let mut nsign = false;
        let mut vsign = false;
        if tmp1 & 0x8000_0000 != 0 {
            tmp1 = (!tmp1).wrapping_add(1);
            self.set_q(tmp1);
            nsign = true;
            target += 1;
        }
        if tmp2 & 0x8000 != 0 {
            tmp2 = (!tmp2).wrapping_add(1);
            vsign = true;
            target += 1;
        }
        let mut quotient: u32 = tmp1 / tmp2 as u32;
        let mut remainder: u16 = (tmp1 % tmp2 as u32) as u16;
        self.cc &= !NZVC;
        if quotient >> 16 != 0 {
            self.cc |= CC_V;
            if nsign { self.cc |= CC_N; }
            target -= 13;
            self.eat(target);
            return;
        }
        if nsign { remainder = (!remainder).wrapping_add(1); }
        if nsign != vsign {
            let nq = (!quotient).wrapping_add(1);
            if nq & 0x8000 != 0 && quotient & 0x8000 == 0 { quotient = nq; }
        }
        self.set_d(remainder);
        self.set_w(quotient as u16);
        let w = self.w();
        if w & 1 != 0 { self.cc |= CC_C; }
        if ((quotient >> 31) ^ (w as u32 >> 15)) & 1 == 0 {
            if w == 0 { self.cc |= CC_Z; }
            if w & 0x8000 != 0 { self.cc |= CC_N; }
        } else {
            self.cc |= CC_N | CC_V;
            target -= 1;
        }
        self.eat(target);
    }

    // bit ops (11 30-37): postbyte = register (7-6), source bit (5-3), dest bit (2-0)
    fn bit_reg(&self) -> u8 {
        match self.temp_im & 0xC0 { 0x00 => self.cc, 0x40 => self.a, 0x80 => self.b, _ => self.temp_lo() }
    }
    fn set_bit_reg(&mut self, v: u8) {
        match self.temp_im & 0xC0 { 0x00 => self.cc = v, 0x40 => self.a = v, 0x80 => self.b = v, _ => self.set_temp_lo(v) }
    }
    fn bit_source(&self) -> bool { self.temp_lo() & (1 << ((self.temp_im >> 3) & 7)) != 0 }
    fn bit_dest(&self) -> bool { self.bit_reg() & (1 << (self.temp_im & 7)) != 0 }
    fn bit_set(&mut self, result: bool) {
        let m = 1u8 << (self.temp_im & 7);
        let r = self.bit_reg();
        self.set_bit_reg(if result { r | m } else { r & !m });
        self.eat(4);
    }
    fn bit_op(&mut self, bus: &mut dyn M6809Bus, op: u8) {
        self.imm_im(bus);
        self.direct(bus);
        if op == 0x37 {
            // STBT
            let r = self.bit_reg();
            self.set_temp_lo(r);
            let mut m = self.read_operand(bus);
            self.set_temp_hi(m);
            let bit = 1u8 << (self.temp_im & 7);
            if self.bit_source() { m |= bit } else { m &= !bit }
            self.set_temp_hi(m);
            self.eat(2);
            self.write_operand(bus, m);
            return;
        }
        let t = self.read_operand(bus);
        self.set_temp_lo(t);
        let (d, s) = (self.bit_dest(), self.bit_source());
        let r = match op {
            0x30 => d && s,
            0x31 => d && !s,
            0x32 => d || s,
            0x33 => d || !s,
            0x34 => d != s,
            0x35 => d == s,
            _ => s,     // LDBT
        };
        self.bit_set(r);
    }

    // TFM (11 38-3B)
    fn tfm_start(&mut self, bus: &mut dyn M6809Bus) {
        let post = self.arg(bus);
        self.set_temp_lo(post);
        self.tfm_op = self.opcode;
        self.tfm_post = post;
        self.tfm = true;
        self.tfm_run(bus);
    }
    fn tfm_reg(&mut self, n: u8) -> Option<R16> {
        match n { 0 => Some(R16::D), 1 => Some(R16::X), 2 => Some(R16::Y), 3 => Some(R16::U), 4 => Some(R16::S), _ => None }
    }
    fn tfm_run(&mut self, bus: &mut dyn M6809Bus) {
        let (op, post) = (self.tfm_op, self.tfm_post);
        let mut n = 0;
        while self.w() != 0 {
            if n == TFM_CHUNK { return; }
            n += 1;
            let Some(src) = self.tfm_reg(post >> 4) else { self.tfm = false; return self.illegal(bus); };
            let sa = self.get16(src);
            let data = self.rd(bus, sa);
            match op & 3 {
                0 | 2 => self.put16(src, sa.wrapping_add(1)),
                1 => self.put16(src, sa.wrapping_sub(1)),
                _ => {}
            }
            let Some(dst) = self.tfm_reg(post & 0x0F) else { self.tfm = false; return self.illegal(bus); };
            let da = self.get16(dst);
            self.wr(bus, da, data);
            match op & 3 {
                0 | 3 => self.put16(dst, da.wrapping_add(1)),
                1 => self.put16(dst, da.wrapping_sub(1)),
                _ => {}
            }
            self.eat(1);
            let w = self.w().wrapping_sub(1);
            self.set_w(w);
        }
        self.eat(3);
        self.tfm = false;
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

    /// Vectors from MAME's generated HD6309 core (tools/hd6309_ref):
    /// `seed pc x y u s a b e f v dp cc md irq firq nmi op01 op23 | pc x y u s a b e f v dp cc md cycles | addr=val...`
    fn run_vectors(text: &str) -> usize {
        let mut n = 0;
        for (lineno, line) in text.lines().enumerate() {
            if line.contains("SKIP") || line.trim().is_empty() { continue; }
            let parts: Vec<&str> = line.split('|').collect();
            let h = |s: &str| u32::from_str_radix(s, 16).unwrap();
            let a: Vec<u32> = parts[0].split_whitespace().map(h).collect();
            let outs: Vec<&str> = parts[1].split_whitespace().collect();
            let b: Vec<u32> = outs[..13].iter().map(|s| h(s)).collect();
            let want_cycles: u32 = outs[13].parse().unwrap();
            let mut s = a[0] | 1;
            let mut mem = Mem { m: (0..0x10000).map(|_| xorshift(&mut s) as u8).collect(), writes: vec![] };
            let mut cpu = HD6309::new();
            cpu.pc = a[1] as u16; cpu.x = a[2] as u16; cpu.y = a[3] as u16; cpu.u = a[4] as u16; cpu.s = a[5] as u16;
            cpu.a = a[6] as u8; cpu.b = a[7] as u8; cpu.e = a[8] as u8; cpu.f = a[9] as u8; cpu.v = a[10] as u16;
            cpu.dp = a[11] as u8; cpu.cc = a[12] as u8; cpu.md = a[13] as u8;
            cpu.irq_line = a[14] != 0; cpu.firq_line = a[15] != 0; cpu.nmi_asserted = a[16] != 0;
            cpu.lds_encountered = true;
            let ops = [a[17] as u8, (a[17] >> 8) as u8, a[18] as u8, (a[18] >> 8) as u8];
            for (i, &o) in ops.iter().enumerate() { mem.m[(cpu.pc.wrapping_add(i as u16)) as usize] = o; }
            let mut cycles = cpu.step(&mut mem);
            while cpu.busy() { cycles += cpu.step(&mut mem); }
            let got = [cpu.pc as u32, cpu.x as u32, cpu.y as u32, cpu.u as u32, cpu.s as u32,
                       cpu.a as u32, cpu.b as u32, cpu.e as u32, cpu.f as u32, cpu.v as u32,
                       cpu.dp as u32, cpu.cc as u32, cpu.md as u32];
            let mut want_w = std::collections::BTreeMap::new();
            for kv in parts.get(2).map(|s| s.split_whitespace().collect::<Vec<_>>()).unwrap_or_default() {
                let (k, v) = kv.split_once('=').unwrap();
                want_w.insert(h(k) as u16, h(v) as u8);
            }
            let mut got_w = std::collections::BTreeMap::new();
            for (k, v) in &mem.writes { got_w.insert(*k, *v); }
            assert!(got[..] == b[..] && cycles == want_cycles && got_w == want_w,
                "line {}: ops {:02x?} md {:x}\n want {:x?} cyc {} {:x?}\n got  {:x?} cyc {} {:x?}",
                lineno + 1, ops, a[13], b, want_cycles, want_w, got, cycles, got_w);
            n += 1;
        }
        n
    }

    #[test]
    fn hd6309_matches_mame_sample() {
        assert!(run_vectors(include_str!("../tests/data/hd6309_vectors.txt")) > 4000);
    }

    /// HD6309_VECTORS=<file> cargo test --release --lib hd6309_matches_mame_full -- --ignored
    #[test]
    #[ignore]
    fn hd6309_matches_mame_full() {
        let path = std::env::var("HD6309_VECTORS").expect("HD6309_VECTORS=<file>");
        println!("{} vectors ok", run_vectors(&std::fs::read_to_string(path).unwrap()));
    }

    /// A TFM longer than one chunk gets interrupted, then restarts after the interrupt.
    #[test]
    fn tfm_is_interruptible() {
        let mut mem = Mem { m: vec![0; 0x10000], writes: vec![] };
        for i in 0..200 { mem.m[0x2000 + i] = i as u8; }
        mem.m[0x1000..0x1003].copy_from_slice(&[0x11, 0x38, 0x12]);   // TFM X+,Y+
        mem.m[0xFFF8..0xFFFA].copy_from_slice(&[0x30, 0x00]);         // IRQ -> 3000
        mem.m[0x3000] = 0x3B;                                          // RTI
        let mut cpu = HD6309::new();
        cpu.pc = 0x1000; cpu.x = 0x2000; cpu.y = 0x4000; cpu.s = 0x8000; cpu.set_w(200);
        cpu.cc = 0;
        cpu.step(&mut mem);
        assert!(cpu.busy());
        cpu.set_irq(true);
        cpu.step(&mut mem);              // takes the IRQ, PC rewound to the TFM
        assert_eq!(cpu.pc, 0x3000);
        cpu.set_irq(false);
        cpu.step(&mut mem);              // RTI
        assert_eq!(cpu.pc, 0x1000);
        while { cpu.step(&mut mem); cpu.busy() } {}
        assert_eq!(cpu.w(), 0);
        assert_eq!(&mem.m[0x4000..0x40C8], &mem.m[0x2000..0x20C8].to_vec()[..]);
    }
}
