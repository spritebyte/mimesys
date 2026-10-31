//! ARM7TDMI (ARMv4T): ARM and Thumb instruction sets, banked registers for the
//! FIQ/IRQ/SVC/ABT/UND modes, exceptions, and the 2-stage prefetch pipeline.
//!
//! Each `step()` fetches the next opcode into the pipeline and runs one whole
//! instruction (taking a pending IRQ first). Each instruction advances r15 at the
//! point the real chip does, which is why some operands read r15 as +12. Timing
//! comes from the bus: every access says whether it is code or data and sequential
//! or not, and `idle()` marks internal cycles, so the bus adds its own wait states
//! and counts cycles.
//!
//! With `v5` set it is an ARM946E-S (ARMv5TE, the Nintendo DS's main CPU): BLX, CLZ,
//! the saturating Q instructions, the 16-bit signed multiplies, LDRD / STRD, PLD, BKPT,
//! loads into r15 that switch to Thumb on bit 0, multiplies that leave C alone, and
//! coprocessor 15 (handed to the bus). Checked against Unicorn's ARM946 model.
//!
//! Behaviour follows NanoBoyAdvance closely (r15 keeps address bit 1 after jumps,
//! loads pass unaligned addresses to the bus, the Booth-multiplier carry flag, the
//! empty register list quirks...). Checked against the SingleStepTests ARM7TDMI
//! suite, which is generated from NanoBoyAdvance: registers, flags, pipeline
//! contents and memory writes.

use serde::{Deserialize, Serialize};

// ---- CPSR bit layout ----
//  31 30 29 28 ... 7 6 5     4-0
//   N  Z  C  V      I F T   mode
pub const FLAG_N: u32 = 1 << 31;
pub const FLAG_Z: u32 = 1 << 30;
pub const FLAG_C: u32 = 1 << 29;
pub const FLAG_V: u32 = 1 << 28;
pub const FLAG_Q: u32 = 1 << 27; // sticky saturation (ARMv5)
pub const FLAG_I: u32 = 1 << 7; // IRQ disable
pub const FLAG_F: u32 = 1 << 6; // FIQ disable
pub const FLAG_T: u32 = 1 << 5; // Thumb state

// ---- Processor modes (low 5 bits of CPSR) ----
pub const MODE_USR: u32 = 0x10;
pub const MODE_FIQ: u32 = 0x11;
pub const MODE_IRQ: u32 = 0x12;
pub const MODE_SVC: u32 = 0x13;
pub const MODE_ABT: u32 = 0x17;
pub const MODE_UND: u32 = 0x1B;
pub const MODE_SYS: u32 = 0x1F;

/// Bus access kinds (bit flags).
pub const ACCESS_NSEQ: u8 = 0;
pub const ACCESS_SEQ: u8 = 1;
pub const ACCESS_CODE: u8 = 2;
pub const ACCESS_LOCK: u8 = 8;

/// The CPU's view of memory. Addresses come as the CPU computed them, possibly
/// unaligned: the bus ignores the low bits (the CPU rotates misaligned loads itself).
pub trait ArmBus {
    fn read8(&mut self, addr: u32, access: u8) -> u8;
    fn read16(&mut self, addr: u32, access: u8) -> u16;
    fn read32(&mut self, addr: u32, access: u8) -> u32;
    fn write8(&mut self, addr: u32, val: u8, access: u8);
    fn write16(&mut self, addr: u32, val: u16, access: u8);
    fn write32(&mut self, addr: u32, val: u32, access: u8);
    /// One internal (no bus) cycle.
    fn idle(&mut self) {}
    /// Coprocessor 15 (ARMv5): `reg` = opcode1 << 12 | CRn << 8 | CRm << 4 | opcode2.
    fn cp15_read(&mut self, _reg: u32) -> u32 { 0 }
    fn cp15_write(&mut self, _reg: u32, _val: u32) {}
    /// Where the exception vectors are: 0, or 0xFFFF0000 with CP15's high-vectors bit.
    fn exception_base(&self) -> u32 { 0 }
}

/// Register bank for a mode, in storage order: 0 FIQ, 1 SVC, 2 ABT, 3 IRQ, 4 UND.
/// User / System (and invalid modes) have none.
pub fn bank_of(mode: u32) -> Option<usize> {
    match mode & 0x1F {
        MODE_FIQ => Some(0),
        MODE_SVC => Some(1),
        MODE_ABT => Some(2),
        MODE_IRQ => Some(3),
        MODE_UND => Some(4),
        _ => None,
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Arm7tdmi {
    /// Active registers r0-r15. While an instruction runs r15 is 8 (ARM) / 4 (Thumb)
    /// bytes past it.
    pub r: [u32; 16],
    pub cpsr: u32,
    /// SPSR per bank: FIQ, SVC, ABT, IRQ, UND
    pub spsr: [u32; 5],
    /// r13/r14 of each bank while that mode is not active
    pub bank_sp_lr: [[u32; 2]; 5],
    /// User/System r13/r14 while another mode is active
    pub usr_sp_lr: [u32; 2],
    /// r8-r12: FIQ's set and the everyone-else set, whichever is not active
    pub fiq_r8_12: [u32; 5],
    pub usr_r8_12: [u32; 5],
    /// Prefetched opcodes: [0] executes next, [1] after it
    pub pipe: [u32; 2],
    /// Access kind of the next code fetch
    next_fetch: u8,
    /// IRQ input (level)
    pub irq_line: bool,
    /// The I flag as it was when the current instruction started (IRQs see the
    /// old value for one instruction after it changes)
    irq_latch: bool,
    /// ARMv5TE (ARM946E-S) instead of ARMv4T
    #[serde(default)]
    pub v5: bool,
}

impl Default for Arm7tdmi { fn default() -> Self { Self::new() } }

#[inline] fn bit(v: u32, n: u32) -> bool { (v >> n) & 1 != 0 }
#[inline] fn bitb(v: u32, n: u32) -> u32 { (v >> n) & 1 }

impl Arm7tdmi {
    pub fn new() -> Self {
        Self {
            r: [0; 16],
            cpsr: MODE_SVC | FLAG_I | FLAG_F,
            spsr: [0; 5],
            bank_sp_lr: [[0; 2]; 5],
            usr_sp_lr: [0; 2],
            fiq_r8_12: [0; 5],
            usr_r8_12: [0; 5],
            pipe: [0; 2],
            next_fetch: ACCESS_CODE,
            irq_line: false,
            irq_latch: true,
            v5: false,
        }
    }

    /// An ARM946E-S (ARMv5TE).
    pub fn new_v5() -> Self { Self { v5: true, ..Self::new() } }

    /// Reset: SVC mode, IRQ/FIQ off, ARM state, execution from 0.
    pub fn reset(&mut self, bus: &mut dyn ArmBus) {
        let old = self.cpsr;
        self.set_mode(MODE_SVC);
        self.spsr[1] = old;
        self.cpsr = (self.cpsr & !FLAG_T) | FLAG_I | FLAG_F;
        self.r[15] = 0;
        self.reload(bus);
    }

    /// Starts execution at `pc` (e.g. skipping the BIOS), in the current state.
    pub fn jump(&mut self, bus: &mut dyn ArmBus, pc: u32) {
        self.r[15] = pc;
        self.reload(bus);
    }

    #[inline] pub fn thumb(&self) -> bool { self.cpsr & FLAG_T != 0 }
    #[inline] pub fn mode(&self) -> u32 { self.cpsr & 0x1F }
    #[inline] fn c(&self) -> u32 { bitb(self.cpsr, 29) }

    /// Address of the instruction that executes next.
    pub fn pc(&self) -> u32 { self.r[15].wrapping_sub(if self.thumb() { 4 } else { 8 }) }

    /// Switches the register banks to a new mode (and sets the CPSR mode bits).
    pub fn set_mode(&mut self, new: u32) {
        let old = self.cpsr & 0x1F;
        let (ob, nb) = (bank_of(old), bank_of(new));
        self.cpsr = (self.cpsr & !0x1F) | (new & 0x1F);
        if ob == nb { return; }
        if ob == Some(0) {
            self.fiq_r8_12.copy_from_slice(&self.r[8..13]);
            self.r[8..13].copy_from_slice(&self.usr_r8_12);
        } else if nb == Some(0) {
            self.usr_r8_12.copy_from_slice(&self.r[8..13]);
            self.r[8..13].copy_from_slice(&self.fiq_r8_12);
        }
        let sp_lr = [self.r[13], self.r[14]];
        match ob { Some(i) => self.bank_sp_lr[i] = sp_lr, None => self.usr_sp_lr = sp_lr }
        let sp_lr = match nb { Some(i) => self.bank_sp_lr[i], None => self.usr_sp_lr };
        self.r[13] = sp_lr[0];
        self.r[14] = sp_lr[1];
    }

    /// Writes the whole CPSR, switching banks if the mode changes.
    pub fn set_cpsr(&mut self, v: u32) {
        self.set_mode(v & 0x1F);
        self.cpsr = v;
    }

    /// SPSR of the current mode (User / System read the CPSR).
    fn spsr(&self) -> u32 {
        match bank_of(self.mode()) { Some(i) => self.spsr[i], None => self.cpsr }
    }

    /// CPSR = SPSR, as exception returns do (no change in User / System).
    fn restore_cpsr(&mut self) {
        let sp = self.spsr();
        self.set_cpsr(sp);
    }

    // ---- pipeline ----
    /// r15 moves on by one ARM instruction (even if the instruction just set T).
    #[inline] fn advance(&mut self, access: u8) {
        self.r[15] = self.r[15].wrapping_add(4);
        self.next_fetch = ACCESS_CODE | access;
    }
    #[inline] fn advance16(&mut self, access: u8) {
        self.r[15] = self.r[15].wrapping_add(2);
        self.next_fetch = ACCESS_CODE | access;
    }

    /// Refills the pipeline from r15 in the current state (after r15 was written).
    fn reload(&mut self, bus: &mut dyn ArmBus) {
        let pc = self.r[15];
        if self.thumb() {
            self.pipe[0] = bus.read16(pc, ACCESS_CODE | ACCESS_NSEQ) as u32;
            self.pipe[1] = bus.read16(pc.wrapping_add(2), ACCESS_CODE | ACCESS_SEQ) as u32;
            self.r[15] = pc.wrapping_add(4);
        } else {
            self.pipe[0] = bus.read32(pc, ACCESS_CODE | ACCESS_NSEQ);
            self.pipe[1] = bus.read32(pc.wrapping_add(4), ACCESS_CODE | ACCESS_SEQ);
            self.r[15] = pc.wrapping_add(8);
        }
        self.next_fetch = ACCESS_CODE | ACCESS_SEQ;
        self.irq_latch = self.cpsr & FLAG_I != 0;
    }

    /// Writes r15 and jumps in the current state.
    fn jump_to(&mut self, bus: &mut dyn ArmBus, addr: u32) {
        self.r[15] = addr;
        self.reload(bus);
    }

    /// Exception entry: SPSR, bank switch, IRQs off, LR, ARM state, vector.
    fn exception(&mut self, bus: &mut dyn ArmBus, vector: u32, mode: u32, lr: u32) {
        let old = self.cpsr;
        if let Some(i) = bank_of(mode) { self.spsr[i] = old; }
        self.set_mode(mode);
        self.cpsr = (self.cpsr & !FLAG_T) | FLAG_I;
        self.r[14] = lr;
        let base = bus.exception_base();
        self.jump_to(bus, base.wrapping_add(vector));
    }

    /// Jump to `v`; on ARMv5 (loads into r15) bit 0 picks Thumb.
    fn jump_interwork(&mut self, bus: &mut dyn ArmBus, v: u32) {
        if self.v5 {
            if v & 1 != 0 { self.cpsr |= FLAG_T; self.jump_to(bus, v & !1); }
            else { self.cpsr &= !FLAG_T; self.jump_to(bus, v & !3); }
        } else {
            self.jump_to(bus, v);
        }
    }

    /// Runs one instruction, entering the IRQ handler first if an IRQ is pending.
    pub fn step(&mut self, bus: &mut dyn ArmBus) {
        if self.irq_line && !self.irq_latch {
            // the next fetch happens (and is thrown away) before the jump
            let acc = self.next_fetch;
            if self.thumb() { bus.read16(self.r[15] & !1, acc); } else { bus.read32(self.r[15] & !3, acc); }
            let lr = if self.thumb() { self.r[15] } else { self.r[15].wrapping_sub(4) };
            self.exception(bus, 0x18, MODE_IRQ, lr);
        }
        self.irq_latch = self.cpsr & FLAG_I != 0;
        self.r[15] &= !1;
        let op = self.pipe[0];
        self.pipe[0] = self.pipe[1];
        let acc = self.next_fetch;
        if self.thumb() {
            self.pipe[1] = bus.read16(self.r[15], acc) as u32;
            self.thumb_op(bus, op as u16);
        } else {
            self.pipe[1] = bus.read32(self.r[15], acc);
            if self.cond(op >> 28) { self.arm_op(bus, op) }
            else if op >> 28 == 0xF && self.v5 { self.arm_unconditional(bus, op) }
            else { self.advance(ACCESS_SEQ) }
        }
    }

    fn cond(&self, c: u32) -> bool {
        let f = self.cpsr;
        let (n, z, cy, v) = (bit(f, 31), bit(f, 30), bit(f, 29), bit(f, 28));
        match c {
            0x0 => z, 0x1 => !z, 0x2 => cy, 0x3 => !cy,
            0x4 => n, 0x5 => !n, 0x6 => v, 0x7 => !v,
            0x8 => cy && !z, 0x9 => !cy || z,
            0xA => n == v, 0xB => n != v,
            0xC => !z && n == v, 0xD => z || n != v,
            0xE => true,
            _ => false,
        }
    }

    // ---- flags / ALU ----
    fn set_nz(&mut self, r: u32) {
        self.cpsr = (self.cpsr & !(FLAG_N | FLAG_Z)) | (r & FLAG_N) | if r == 0 { FLAG_Z } else { 0 };
    }
    fn set_c(&mut self, c: u32) { self.cpsr = (self.cpsr & !FLAG_C) | ((c & 1) << 29); }
    fn set_v(&mut self, v: u32) { self.cpsr = (self.cpsr & !FLAG_V) | ((v & 1) << 28); }

    fn add(&mut self, a: u32, b: u32, s: bool) -> u32 {
        let r = a.wrapping_add(b);
        if s {
            self.set_nz(r);
            self.set_c((r < a) as u32);
            self.set_v((!(a ^ b) & (b ^ r)) >> 31);
        }
        r
    }
    fn adc(&mut self, a: u32, b: u32, s: bool) -> u32 {
        let r64 = a as u64 + b as u64 + self.c() as u64;
        let r = r64 as u32;
        if s {
            self.set_nz(r);
            self.set_c((r64 >> 32) as u32);
            self.set_v((!(a ^ b) & (b ^ r)) >> 31);
        }
        r
    }
    fn sub(&mut self, a: u32, b: u32, s: bool) -> u32 {
        let r = a.wrapping_sub(b);
        if s {
            self.set_nz(r);
            self.set_c((a >= b) as u32);
            self.set_v(((a ^ b) & (a ^ r)) >> 31);
        }
        r
    }
    fn sbc(&mut self, a: u32, b: u32, s: bool) -> u32 {
        let borrow = self.c() ^ 1;
        let r = a.wrapping_sub(b).wrapping_sub(borrow);
        if s {
            self.set_nz(r);
            self.set_c((a as u64 >= b as u64 + borrow as u64) as u32);
            self.set_v(((a ^ b) & (a ^ r)) >> 31);
        }
        r
    }

    /// Barrel shifter. `imm` = amount from the opcode (LSR/ASR #0 mean 32, ROR #0 is
    /// RRX); otherwise from a register's bottom byte (0 leaves value and carry alone).
    fn shift(&self, kind: u32, v: u32, amount: u32, imm: bool) -> (u32, u32) {
        let mut c = self.c();
        let mut amt = amount & 0xFF;
        let r = match kind {
            0 => {
                let a = amt.min(33);
                if a != 0 { c = (((v as u64) << (a - 1)) >> 31) as u32 & 1; }
                ((v as u64) << a) as u32
            }
            1 => {
                if imm && amt == 0 { amt = 32; }
                let a = amt.min(33);
                if a != 0 { c = ((v as u64 >> (a - 1)) & 1) as u32; }
                (v as u64 >> a) as u32
            }
            2 => {
                if imm && amt == 0 { amt = 32; }
                let a = amt.min(33);
                if a != 0 { c = ((v as i32 as i64 >> (a - 1)) & 1) as u32; }
                (v as i32 as i64 >> a) as u32
            }
            _ => {
                if imm && amt == 0 {
                    let r = (v >> 1) | (c << 31);
                    c = v & 1;
                    r
                } else if amt == 0 {
                    v
                } else {
                    let r = v.rotate_right(amt & 31);
                    c = r >> 31;
                    r
                }
            }
        };
        (r, c)
    }

    /// Internal cycles of a multiply: 1, plus one per further multiplier byte that
    /// isn't all zeros (or all ones, for signed). Returns true if all 4 were used.
    fn tick_multiply(bus: &mut dyn ArmBus, multiplier: u32, signed: bool) -> bool {
        let mut mask = 0xFFFF_FF00u32;
        let mut m = multiplier;
        bus.idle();
        loop {
            m &= mask;
            if m == 0 || (signed && m == mask) { break; }
            mask <<= 8;
            bus.idle();
        }
        mask == 0
    }

    // ================= ARM =================

    fn arm_op(&mut self, bus: &mut dyn ArmBus, op: u32) {
        match (op >> 26) & 3 {
            0 => {
                if self.v5 && !bit(op, 25) && self.arm_v5_misc(bus, op) {
                    // handled
                } else if bit(op, 25) {
                    self.arm_data_or_psr(bus, op);
                } else if op & 0x0FF0_00F0 == 0x0120_0010 {
                    self.arm_bx(bus, op);
                } else if op & 0x0100_00F0 == 0x0000_0090 {
                    if bit(op, 23) { self.arm_mull(bus, op) } else { self.arm_mul(bus, op) }
                } else if op & 0x0100_00F0 == 0x0100_0090 {
                    self.arm_swp(bus, op);
                } else if op & 0xF0 == 0xB0 || op & 0xD0 == 0xD0 {
                    self.arm_halfword(bus, op);
                } else {
                    self.arm_data_or_psr(bus, op);
                }
            }
            1 => if op & 0x0200_0010 == 0x0200_0010 { self.undefined(bus) } else { self.arm_single(bus, op) },
            2 => if bit(op, 25) { self.arm_branch(bus, op) } else { self.arm_block(bus, op) },
            _ => if op & 0x0300_0000 == 0x0300_0000 { self.swi(bus) }
                 else if self.v5 && op & 0x0F00_0F10 == 0x0E00_0F10 { self.arm_cp15(bus, op) }
                 else { self.undefined(bus) },
        }
    }

    // ---- ARMv5 ----

    /// Condition 1111: BLX with an immediate, PLD (a hint: nothing to do).
    fn arm_unconditional(&mut self, bus: &mut dyn ArmBus, op: u32) {
        if op & 0x0E00_0000 == 0x0A00_0000 {
            let off = (((op & 0x00FF_FFFF) << 8) as i32 >> 6) as u32 | ((op >> 23) & 2);
            self.r[14] = self.r[15].wrapping_sub(4);
            let target = self.r[15].wrapping_add(off);
            self.cpsr |= FLAG_T;
            self.jump_to(bus, target);
        } else {
            self.advance(ACCESS_SEQ);
        }
    }

    /// The ARMv5 additions in the data-processing space. Returns false if `op` isn't one.
    fn arm_v5_misc(&mut self, bus: &mut dyn ArmBus, op: u32) -> bool {
        let rm = (op & 0xF) as usize;
        let rd = ((op >> 12) & 0xF) as usize;
        let rn = ((op >> 16) & 0xF) as usize;
        if op & 0x0FFF_FFF0 == 0x012F_FF30 {
            // BLX Rm
            let v = self.r[rm];
            self.r[14] = self.r[15].wrapping_sub(4);
            if v & 1 != 0 { self.cpsr |= FLAG_T; self.jump_to(bus, v & !1) } else { self.jump_to(bus, v & !3) }
        } else if op & 0x0FFF_0FF0 == 0x016F_0F10 {
            // CLZ
            self.r[rd] = self.r[rm].leading_zeros();
            self.advance(ACCESS_SEQ);
        } else if op & 0x0F90_0FF0 == 0x0100_0050 {
            // QADD / QSUB / QDADD / QDSUB
            let a = self.r[rm] as i32;
            let mut b = self.r[rn] as i32;
            let mut sat = false;
            if bit(op, 22) {
                let (d, o) = b.overflowing_mul(2);
                b = if o { sat = true; if b < 0 { i32::MIN } else { i32::MAX } } else { d };
            }
            let r = if bit(op, 21) { a.checked_sub(b) } else { a.checked_add(b) };
            let r = r.unwrap_or_else(|| {
                sat = true;
                if bit(op, 21) { if a < 0 { i32::MIN } else { i32::MAX } } else if b < 0 { i32::MIN } else { i32::MAX }
            });
            if sat { self.cpsr |= FLAG_Q; }
            self.r[rd] = r as u32;
            self.advance(ACCESS_SEQ);
            if rd == 15 { self.reload(bus); }
        } else if op & 0x0F90_0090 == 0x0100_0080 {
            self.arm_mul_halfword(bus, op);
        } else if op & 0x0FF0_00F0 == 0x0120_0070 {
            // BKPT: prefetch abort
            let lr = self.r[15].wrapping_sub(4);
            self.exception(bus, 0x0C, MODE_ABT, lr);
        } else {
            return false;
        }
        true
    }

    /// SMLAxy / SMLAWy / SMULWy / SMLALxy / SMULxy: 16-bit halves, signed.
    fn arm_mul_halfword(&mut self, bus: &mut dyn ArmBus, op: u32) {
        let rd = ((op >> 16) & 0xF) as usize;
        let rn = ((op >> 12) & 0xF) as usize;
        let rs = self.r[((op >> 8) & 0xF) as usize];
        let rm = self.r[(op & 0xF) as usize];
        let half = |v: u32, top: bool| -> i32 { if top { (v >> 16) as i16 as i32 } else { v as i16 as i32 } };
        let x = half(rm, bit(op, 5));
        let y = half(rs, bit(op, 6));
        self.advance(ACCESS_SEQ);
        match (op >> 21) & 3 {
            0 => {
                let (r, o) = (x * y).overflowing_add(self.r[rn] as i32);
                if o { self.cpsr |= FLAG_Q; }
                self.r[rd] = r as u32;
            }
            1 => {
                let p = ((rm as i32 as i64 * y as i64) >> 16) as i32;
                if bit(op, 5) {
                    self.r[rd] = p as u32;
                } else {
                    let (r, o) = p.overflowing_add(self.r[rn] as i32);
                    if o { self.cpsr |= FLAG_Q; }
                    self.r[rd] = r as u32;
                }
            }
            2 => {
                let acc = ((self.r[rd] as u64) << 32 | self.r[rn] as u64) as i64;
                let r = acc.wrapping_add((x * y) as i64) as u64;
                self.r[rn] = r as u32;
                self.r[rd] = (r >> 32) as u32;
                bus.idle();
            }
            _ => self.r[rd] = (x * y) as u32,
        }
        if rd == 15 || rn == 15 { self.reload(bus); }
    }

    /// MCR / MRC to coprocessor 15 (anything else is undefined).
    fn arm_cp15(&mut self, bus: &mut dyn ArmBus, op: u32) {
        let rd = ((op >> 12) & 0xF) as usize;
        let reg = ((op >> 21) & 7) << 12 | ((op >> 16) & 0xF) << 8 | (op & 0xF) << 4 | ((op >> 5) & 7);
        self.advance(ACCESS_SEQ);
        if bit(op, 20) {
            let v = bus.cp15_read(reg);
            if rd == 15 { self.cpsr = (self.cpsr & 0x0FFF_FFFF) | (v & 0xF000_0000) } else { self.r[rd] = v }
        } else {
            let v = if rd == 15 { self.r[15].wrapping_add(4) } else { self.r[rd] };
            bus.cp15_write(reg, v);
        }
    }

    fn undefined(&mut self, bus: &mut dyn ArmBus) {
        let lr = self.r[15].wrapping_sub(4);
        self.exception(bus, 0x04, MODE_UND, lr);
    }

    fn swi(&mut self, bus: &mut dyn ArmBus) {
        let lr = self.r[15].wrapping_sub(4);
        self.exception(bus, 0x08, MODE_SVC, lr);
    }

    fn arm_branch(&mut self, bus: &mut dyn ArmBus, op: u32) {
        let off = (((op & 0x00FF_FFFF) << 8) as i32 >> 6) as u32;
        if bit(op, 24) { self.r[14] = self.r[15].wrapping_sub(4); }
        let target = self.r[15].wrapping_add(off);
        self.jump_to(bus, target);
    }

    fn arm_bx(&mut self, bus: &mut dyn ArmBus, op: u32) {
        let v = self.r[(op & 0xF) as usize];
        if v & 1 != 0 {
            self.cpsr |= FLAG_T;
            self.jump_to(bus, v & !1);
        } else {
            self.jump_to(bus, v);
        }
    }

    fn arm_data_or_psr(&mut self, bus: &mut dyn ArmBus, op: u32) {
        let opc = (op >> 21) & 0xF;
        if !bit(op, 20) && (8..=11).contains(&opc) {
            if bit(op, 21) { self.arm_msr(op) } else { self.arm_mrs(op) }
        } else {
            self.arm_data(bus, op);
        }
    }

    /// Data processing. With an immediate or immediate shift, operands read r15 as
    /// +8 and r15 advances at the end. With a register shift: the shift register is
    /// read, r15 advances, one internal cycle, then Rn / Rm read r15 as +12.
    fn arm_data(&mut self, bus: &mut dyn ArmBus, op: u32) {
        let opc = (op >> 21) & 0xF;
        let s = bit(op, 20);
        let rn = ((op >> 16) & 0xF) as usize;
        let rd = ((op >> 12) & 0xF) as usize;
        let imm = bit(op, 25);
        let reg_shift = !imm && bit(op, 4);
        let mut carry = self.c();
        let (a, b);
        if imm {
            let rot = ((op >> 8) & 0xF) * 2;
            let v = op & 0xFF;
            b = v.rotate_right(rot);
            if rot != 0 { carry = (v >> (rot - 1)) & 1; }
            a = self.r[rn];
        } else {
            let amount = if reg_shift {
                let amt = self.r[((op >> 8) & 0xF) as usize];
                self.r[15] = self.r[15].wrapping_add(4);
                bus.idle();
                self.next_fetch = ACCESS_CODE | ACCESS_NSEQ;
                amt
            } else {
                (op >> 7) & 0x1F
            };
            a = self.r[rn];
            let (v, c) = self.shift((op >> 5) & 3, self.r[(op & 0xF) as usize], amount, !reg_shift);
            b = v;
            carry = c;
        }
        if !reg_shift { self.next_fetch = ACCESS_CODE | ACCESS_SEQ; }
        let logical = |cpu: &mut Self, r: u32| { if s { cpu.set_nz(r); cpu.set_c(carry); } r };
        let (res, write) = match opc {
            0x0 => (logical(self, a & b), true),
            0x1 => (logical(self, a ^ b), true),
            0x2 => (self.sub(a, b, s), true),
            0x3 => (self.sub(b, a, s), true),
            0x4 => (self.add(a, b, s), true),
            0x5 => (self.adc(a, b, s), true),
            0x6 => (self.sbc(a, b, s), true),
            0x7 => (self.sbc(b, a, s), true),
            0x8 => (logical(self, a & b), false),
            0x9 => (logical(self, a ^ b), false),
            0xA => (self.sub(a, b, true), false),
            0xB => (self.add(a, b, true), false),
            0xC => (logical(self, a | b), true),
            0xD => (logical(self, b), true),
            0xE => (logical(self, a & !b), true),
            _ => (logical(self, !b), true),
        };
        if write { self.r[rd] = res; }
        if rd == 15 {
            if s { self.restore_cpsr(); }
            if write {
                self.reload(bus);
            } else if !reg_shift {
                self.r[15] = self.r[15].wrapping_add(4);
            }
        } else if !reg_shift {
            self.r[15] = self.r[15].wrapping_add(4);
        }
    }

    fn arm_mrs(&mut self, op: u32) {
        let rd = ((op >> 12) & 0xF) as usize;
        self.r[rd] = if bit(op, 22) { self.spsr() } else { self.cpsr };
        self.advance(ACCESS_SEQ);
    }

    fn arm_msr(&mut self, op: u32) {
        let mut v = if bit(op, 25) {
            (op & 0xFF).rotate_right(((op >> 8) & 0xF) * 2)
        } else {
            self.r[(op & 0xF) as usize]
        };
        let mut mask = 0u32;
        if bit(op, 16) { mask |= 0x0000_00FF; }
        if bit(op, 17) { mask |= 0x0000_FF00; }
        if bit(op, 18) { mask |= 0x00FF_0000; }
        if bit(op, 19) { mask |= 0xFF00_0000; }
        if !bit(op, 22) {
            if self.mode() == MODE_USR { mask &= 0xFF00_0000; }
            if mask & 0xFF != 0 {
                v |= 0x10;   // mode bit 4 always reads 1 on the ARM7TDMI
                self.set_mode(v & 0x1F);
            }
            self.cpsr = (self.cpsr & !mask) | (v & mask);
        } else if let Some(i) = bank_of(self.mode()) {
            self.spsr[i] = (self.spsr[i] & !mask) | (v & mask);
        }
        self.advance(ACCESS_SEQ);
    }

    fn arm_mul(&mut self, bus: &mut dyn ArmBus, op: u32) {
        let rd = ((op >> 16) & 0xF) as usize;
        self.advance(ACCESS_NSEQ);
        let lhs = self.r[(op & 0xF) as usize];
        let rhs = self.r[((op >> 8) & 0xF) as usize];
        let mut r = lhs.wrapping_mul(rhs);
        let full = Self::tick_multiply(bus, rhs, true);
        let mut acc = 0;
        if bit(op, 21) {
            acc = self.r[((op >> 12) & 0xF) as usize];
            r = r.wrapping_add(acc);
            bus.idle();
        }
        if bit(op, 20) {
            self.set_nz(r);
            if !self.v5 {
                let c = if full { mul_carry_simple(rhs) } else { mul_carry_lo(lhs, rhs, acc) };
                self.set_c(c as u32);
            }
        }
        self.r[rd] = r;
        if rd == 15 { self.reload(bus); }
    }

    fn arm_mull(&mut self, bus: &mut dyn ArmBus, op: u32) {
        let hi = ((op >> 16) & 0xF) as usize;
        let lo = ((op >> 12) & 0xF) as usize;
        let signed = bit(op, 22);
        self.advance(ACCESS_NSEQ);
        let lhs = self.r[(op & 0xF) as usize];
        let rhs = self.r[((op >> 8) & 0xF) as usize];
        let mut r = if signed {
            (lhs as i32 as i64).wrapping_mul(rhs as i32 as i64) as u64
        } else {
            lhs as u64 * rhs as u64
        };
        let full = Self::tick_multiply(bus, rhs, signed);
        bus.idle();
        let (mut acc_lo, mut acc_hi) = (0, 0);
        if bit(op, 21) {
            acc_lo = self.r[lo];
            acc_hi = self.r[hi];
            r = r.wrapping_add(((acc_hi as u64) << 32) | acc_lo as u64);
            bus.idle();
        }
        if bit(op, 20) {
            self.cpsr = (self.cpsr & !(FLAG_N | FLAG_Z))
                | ((r >> 32) as u32 & FLAG_N)
                | if r == 0 { FLAG_Z } else { 0 };
            if !self.v5 {
                let c = if full { mul_carry_hi(lhs, rhs, acc_hi, signed) } else { mul_carry_lo(lhs, rhs, acc_lo) };
                self.set_c(c as u32);
            }
        }
        self.r[lo] = r as u32;
        self.r[hi] = (r >> 32) as u32;
        if lo == 15 || hi == 15 { self.reload(bus); }
    }

    fn arm_swp(&mut self, bus: &mut dyn ArmBus, op: u32) {
        let src = (op & 0xF) as usize;
        let rd = ((op >> 12) & 0xF) as usize;
        let rn = ((op >> 16) & 0xF) as usize;
        self.advance(ACCESS_NSEQ);
        let v = if bit(op, 22) {
            let v = bus.read8(self.r[rn], ACCESS_NSEQ) as u32;
            bus.write8(self.r[rn], self.r[src] as u8, ACCESS_NSEQ | ACCESS_LOCK);
            v
        } else {
            let a = self.r[rn];
            let v = bus.read32(a, ACCESS_NSEQ).rotate_right((a & 3) * 8);
            bus.write32(self.r[rn], self.r[src], ACCESS_NSEQ | ACCESS_LOCK);
            v
        };
        bus.idle();
        self.r[rd] = v;
        if rd == 15 { self.reload(bus); }
    }

    fn read_half_rotate(bus: &mut dyn ArmBus, a: u32) -> u32 {
        let v = bus.read16(a, ACCESS_NSEQ) as u32;
        if a & 1 != 0 { v.rotate_right(8) } else { v }
    }
    /// LDRSH: at an odd address only the top byte of the halfword is used, sign-extended.
    fn read_half_signed(bus: &mut dyn ArmBus, a: u32) -> u32 {
        let v = bus.read16(a, ACCESS_NSEQ);
        if a & 1 != 0 { (v >> 8) as u8 as i8 as i32 as u32 } else { v as i16 as i32 as u32 }
    }

    /// LDRH / STRH / LDRSB / LDRSH (and the ARMv5 LDRD / STRD encodings, which
    /// only take internal cycles here).
    fn arm_halfword(&mut self, bus: &mut dyn ArmBus, op: u32) {
        let pre = bit(op, 24);
        let wb = bit(op, 21) || !pre;
        let load = bit(op, 20);
        let rd = ((op >> 12) & 0xF) as usize;
        let rn = ((op >> 16) & 0xF) as usize;
        let mut addr = self.r[rn];
        let mut off = if bit(op, 22) { (op & 0xF) | ((op >> 4) & 0xF0) } else { self.r[(op & 0xF) as usize] };
        self.advance(ACCESS_NSEQ);
        if !bit(op, 23) { off = off.wrapping_neg(); }
        if pre { addr = addr.wrapping_add(off); }
        match (op >> 5) & 3 {
            1 => {
                if load {
                    let v = Self::read_half_rotate(bus, addr);
                    if wb { self.r[rn] = self.r[rn].wrapping_add(off); }
                    bus.idle();
                    self.r[rd] = v;
                } else {
                    bus.write16(addr, self.r[rd] as u16, ACCESS_NSEQ);
                    if wb { self.r[rn] = self.r[rn].wrapping_add(off); }
                }
            }
            2 => {
                if load {
                    let v = bus.read8(addr, ACCESS_NSEQ) as i8 as i32 as u32;
                    if wb { self.r[rn] = self.r[rn].wrapping_add(off); }
                    bus.idle();
                    self.r[rd] = v;
                } else if self.v5 {
                    // LDRD
                    let r = rd & !1;
                    let lo = bus.read32(addr, ACCESS_NSEQ);
                    let hi = bus.read32(addr.wrapping_add(4), ACCESS_SEQ);
                    if wb { self.r[rn] = self.r[rn].wrapping_add(off); }
                    self.r[r] = lo;
                    self.r[r + 1] = hi;
                    if r + 1 == 15 { self.reload(bus); return; }
                } else {
                    bus.idle();
                    if wb { self.r[rn] = self.r[rn].wrapping_add(off); }
                    bus.idle();
                }
            }
            _ => {
                if load {
                    let v = Self::read_half_signed(bus, addr);
                    if wb { self.r[rn] = self.r[rn].wrapping_add(off); }
                    bus.idle();
                    self.r[rd] = v;
                } else if self.v5 {
                    // STRD
                    let r = rd & !1;
                    bus.write32(addr, self.r[r], ACCESS_NSEQ);
                    let hi = if r + 1 == 15 { self.r[15].wrapping_add(4) } else { self.r[r + 1] };
                    bus.write32(addr.wrapping_add(4), hi, ACCESS_SEQ);
                    if wb { self.r[rn] = self.r[rn].wrapping_add(off); }
                } else {
                    bus.idle();
                    if wb { self.r[rn] = self.r[rn].wrapping_add(off); }
                }
            }
        }
        if load && rd == 15 && self.v5 { let v = self.r[15]; self.jump_interwork(bus, v); return; }
        if (load && rd == 15) || (wb && rn == 15) { self.reload(bus); }
    }

    /// LDR / STR / LDRB / STRB
    fn arm_single(&mut self, bus: &mut dyn ArmBus, op: u32) {
        let pre = bit(op, 24);
        let byte = bit(op, 22);
        let wb = bit(op, 21) || !pre;
        let load = bit(op, 20);
        let rd = ((op >> 12) & 0xF) as usize;
        let rn = ((op >> 16) & 0xF) as usize;
        let mut addr = self.r[rn];
        let mut off = if !bit(op, 25) {
            op & 0xFFF
        } else {
            self.shift((op >> 5) & 3, self.r[(op & 0xF) as usize], (op >> 7) & 0x1F, true).0
        };
        self.advance(ACCESS_NSEQ);
        if !bit(op, 23) { off = off.wrapping_neg(); }
        if pre { addr = addr.wrapping_add(off); }
        if load {
            let v = if byte {
                bus.read8(addr, ACCESS_NSEQ) as u32
            } else {
                bus.read32(addr, ACCESS_NSEQ).rotate_right((addr & 3) * 8)
            };
            if wb { self.r[rn] = self.r[rn].wrapping_add(off); }
            bus.idle();
            self.r[rd] = v;
            if rd == 15 && self.v5 { self.jump_interwork(bus, v); }
            else if rd == 15 || (wb && rn == 15) { self.reload(bus); }
        } else {
            if byte { bus.write8(addr, self.r[rd] as u8, ACCESS_NSEQ) } else { bus.write32(addr, self.r[rd], ACCESS_NSEQ) }
            if wb { self.r[rn] = self.r[rn].wrapping_add(off); }
            if wb && rn == 15 { self.reload(bus); }
        }
    }

    /// LDM / STM. Registers go in ascending order at ascending addresses; an empty
    /// list moves r15 and steps the base by 0x40. With the S bit (and no r15 in an
    /// LDM) the transfer uses the User bank.
    fn arm_block(&mut self, bus: &mut dyn ArmBus, op: u32) {
        let mut pre = bit(op, 24);
        let up = bit(op, 23);
        let user = bit(op, 22);
        let wb = bit(op, 21);
        let load = bit(op, 20);
        let rn = ((op >> 16) & 0xF) as usize;
        let mut list = op & 0xFFFF;
        let mode = self.mode();
        let mut transfer_pc = bit(list, 15);
        let bytes;
        if list != 0 {
            bytes = list.count_ones() * 4;
        } else {
            list = 1 << 15;
            transfer_pc = true;
            bytes = 64;
        }
        let first = list.trailing_zeros() as usize;
        let switch_mode = user && (!load || !transfer_pc) && mode != MODE_USR && mode != MODE_SYS;
        // the base is read in the original mode; the transfer (and writeback) use User
        let mut addr = self.r[rn];
        if switch_mode { self.set_mode(MODE_USR); }
        let mut base_new = addr;
        if up {
            base_new = base_new.wrapping_add(bytes);
        } else {
            pre = !pre;
            addr = addr.wrapping_sub(bytes);
            base_new = base_new.wrapping_sub(bytes);
        }
        self.advance(ACCESS_NSEQ);
        let mut acc = ACCESS_NSEQ;
        for i in first..16 {
            if !bit(list, i as u32) { continue; }
            if pre { addr = addr.wrapping_add(4); }
            if load {
                let v = bus.read32(addr, acc);
                if wb && i == first { self.r[rn] = base_new; }
                self.r[i] = v;
            } else {
                bus.write32(addr, self.r[i], acc);
                if wb && i == first { self.r[rn] = base_new; }
            }
            if !pre { addr = addr.wrapping_add(4); }
            acc = ACCESS_SEQ;
        }
        let mut reloaded = false;
        if load {
            bus.idle();
            if transfer_pc {
                if user {
                    self.restore_cpsr();
                    self.reload(bus);
                } else {
                    let v = self.r[15];
                    self.jump_interwork(bus, v);
                }
                reloaded = true;
            }
        }
        if switch_mode { self.set_mode(mode); }
        if wb && rn == 15 && !reloaded { self.reload(bus); }
    }

    // ================= Thumb =================

    fn thumb_op(&mut self, bus: &mut dyn ArmBus, op: u16) {
        let op = op as u32;
        let lo = |n: u32| ((op >> n) & 7) as usize;
        match op >> 11 {
            0x00..=0x02 => {
                // LSL / LSR / ASR Rd, Rs, #imm
                let (v, c) = self.shift(op >> 11, self.r[lo(3)], (op >> 6) & 0x1F, true);
                self.r[lo(0)] = v;
                self.set_nz(v);
                self.set_c(c);
                self.advance16(ACCESS_SEQ);
            }
            0x03 => {
                // ADD / SUB Rd, Rs, Rn|#imm3
                let a = self.r[lo(3)];
                let b = if bit(op, 10) { (op >> 6) & 7 } else { self.r[lo(6)] };
                self.r[lo(0)] = if bit(op, 9) { self.sub(a, b, true) } else { self.add(a, b, true) };
                self.advance16(ACCESS_SEQ);
            }
            0x04..=0x07 => {
                // MOV / CMP / ADD / SUB Rd, #imm8
                let rd = lo(8);
                let imm = op & 0xFF;
                match (op >> 11) & 3 {
                    0 => { self.r[rd] = imm; self.set_nz(imm); }
                    1 => { self.sub(self.r[rd], imm, true); }
                    2 => self.r[rd] = self.add(self.r[rd], imm, true),
                    _ => self.r[rd] = self.sub(self.r[rd], imm, true),
                }
                self.advance16(ACCESS_SEQ);
            }
            0x08 => if op & 0x0400 == 0 { self.thumb_alu(bus, op) } else { self.thumb_hireg(bus, op) },
            0x09 => {
                // LDR Rd, [PC, #imm]
                let addr = (self.r[15] & !2).wrapping_add((op & 0xFF) << 2);
                self.advance16(ACCESS_NSEQ);
                self.r[lo(8)] = bus.read32(addr, ACCESS_NSEQ);
                bus.idle();
            }
            0x0A | 0x0B => {
                let addr = self.r[lo(3)].wrapping_add(self.r[lo(6)]);
                let rd = lo(0);
                self.advance16(ACCESS_NSEQ);
                let v = self.r[rd];
                match (bit(op, 9), (op >> 10) & 3) {
                    (false, 0) => bus.write32(addr, v, ACCESS_NSEQ),
                    (false, 1) => bus.write8(addr, v as u8, ACCESS_NSEQ),
                    (false, 2) => { self.r[rd] = bus.read32(addr, ACCESS_NSEQ).rotate_right((addr & 3) * 8); bus.idle(); }
                    (false, _) => { self.r[rd] = bus.read8(addr, ACCESS_NSEQ) as u32; bus.idle(); }
                    (true, 0) => bus.write16(addr, v as u16, ACCESS_NSEQ),
                    (true, 1) => { self.r[rd] = bus.read8(addr, ACCESS_NSEQ) as i8 as i32 as u32; bus.idle(); }
                    (true, 2) => { self.r[rd] = Self::read_half_rotate(bus, addr); bus.idle(); }
                    (true, _) => { self.r[rd] = Self::read_half_signed(bus, addr); bus.idle(); }
                }
            }
            0x0C..=0x0F => {
                // LDR/STR word (imm*4) and byte (imm)
                let rd = lo(0);
                let imm = (op >> 6) & 0x1F;
                let byte = bit(op, 12);
                self.advance16(ACCESS_NSEQ);
                let addr = self.r[lo(3)].wrapping_add(if byte { imm } else { imm * 4 });
                match (bit(op, 11), byte) {
                    (false, false) => bus.write32(addr, self.r[rd], ACCESS_NSEQ),
                    (true, false) => { self.r[rd] = bus.read32(addr, ACCESS_NSEQ).rotate_right((addr & 3) * 8); bus.idle(); }
                    (false, true) => bus.write8(addr, self.r[rd] as u8, ACCESS_NSEQ),
                    (true, true) => { self.r[rd] = bus.read8(addr, ACCESS_NSEQ) as u32; bus.idle(); }
                }
            }
            0x10 | 0x11 => {
                // STRH / LDRH [Rb, #imm*2]
                let rd = lo(0);
                let addr = self.r[lo(3)].wrapping_add(((op >> 6) & 0x1F) * 2);
                self.advance16(ACCESS_NSEQ);
                if bit(op, 11) {
                    self.r[rd] = Self::read_half_rotate(bus, addr);
                    bus.idle();
                } else {
                    bus.write16(addr, self.r[rd] as u16, ACCESS_NSEQ);
                }
            }
            0x12 | 0x13 => {
                // STR / LDR [SP, #imm*4]
                let rd = lo(8);
                let addr = self.r[13].wrapping_add((op & 0xFF) * 4);
                self.advance16(ACCESS_NSEQ);
                if bit(op, 11) {
                    self.r[rd] = bus.read32(addr, ACCESS_NSEQ).rotate_right((addr & 3) * 8);
                    bus.idle();
                } else {
                    bus.write32(addr, self.r[rd], ACCESS_NSEQ);
                }
            }
            0x14 | 0x15 => {
                // ADD Rd, PC|SP, #imm*4
                let base = if bit(op, 11) { self.r[13] } else { self.r[15] & !2 };
                self.r[lo(8)] = base.wrapping_add((op & 0xFF) << 2);
                self.advance16(ACCESS_SEQ);
            }
            0x16 | 0x17 => {
                if op & 0xFF00 == 0xB000 {
                    // ADD SP, #+-imm*4
                    let imm = (op & 0x7F) * 4;
                    self.r[13] = if bit(op, 7) { self.r[13].wrapping_sub(imm) } else { self.r[13].wrapping_add(imm) };
                    self.advance16(ACCESS_SEQ);
                } else if op & 0xF600 == 0xB400 {
                    self.thumb_push_pop(bus, op);
                } else if self.v5 && op & 0xFF00 == 0xBE00 {
                    // BKPT
                    let lr = self.r[15];
                    self.exception(bus, 0x0C, MODE_ABT, lr);
                } else {
                    self.thumb_undefined();
                }
            }
            0x18 | 0x19 => self.thumb_ldm_stm(bus, op),
            0x1A | 0x1B => {
                let cond = (op >> 8) & 0xF;
                if cond == 0xF {
                    let lr = self.r[15].wrapping_sub(2);
                    self.exception(bus, 0x08, MODE_SVC, lr);
                } else if cond == 0xE || self.cond(cond) {
                    // (condition 1110 runs as "always" here)
                    let off = ((op & 0xFF) as i8 as i32 as u32).wrapping_mul(2);
                    let t = self.r[15].wrapping_add(off);
                    self.jump_to(bus, t);
                } else {
                    self.advance16(ACCESS_SEQ);
                }
            }
            0x1C => {
                // B
                let mut imm = (op & 0x3FF) * 2;
                if bit(op, 10) { imm |= 0xFFFF_F800; }
                let t = self.r[15].wrapping_add(imm);
                self.jump_to(bus, t);
            }
            0x1D => {
                if self.v5 && op & 1 == 0 {
                    // BLX suffix: to ARM, at LR + offset (word aligned)
                    let next = self.r[15].wrapping_sub(2);
                    let t = self.r[14].wrapping_add((op & 0x7FF) * 2) & !3;
                    self.r[14] = next | 1;
                    self.cpsr &= !FLAG_T;
                    self.jump_to(bus, t);
                } else {
                    self.thumb_undefined();
                }
            }
            0x1E => {
                // BL prefix: LR = PC + (offset << 12)
                let mut imm = (op & 0x7FF) << 12;
                if imm & 0x40_0000 != 0 { imm |= 0xFF80_0000; }
                self.r[14] = self.r[15].wrapping_add(imm);
                self.advance16(ACCESS_SEQ);
            }
            _ => {
                // BL suffix: PC = LR + (offset << 1), LR = next instruction | 1
                let next = self.r[15].wrapping_sub(2);
                let t = self.r[14].wrapping_add((op & 0x7FF) * 2) & !1;
                self.r[14] = next | 1;
                self.jump_to(bus, t);
            }
        }
    }

    /// Unused Thumb encodings do nothing at all (not even advance r15).
    fn thumb_undefined(&mut self) {}

    /// Thumb format 4: ALU operations on low registers.
    fn thumb_alu(&mut self, bus: &mut dyn ArmBus, op: u32) {
        let rd = (op & 7) as usize;
        let rs = ((op >> 3) & 7) as usize;
        self.advance16(ACCESS_SEQ);
        let (a, b) = (self.r[rd], self.r[rs]);
        match (op >> 6) & 0xF {
            0x0 => { self.r[rd] = a & b; self.set_nz(a & b); }
            0x1 => { self.r[rd] = a ^ b; self.set_nz(a ^ b); }
            k @ (0x2 | 0x3 | 0x4 | 0x7) => {
                bus.idle();
                self.next_fetch = ACCESS_CODE | ACCESS_NSEQ;
                let kind = match k { 0x2 => 0, 0x3 => 1, 0x4 => 2, _ => 3 };
                let (r, c) = self.shift(kind, a, b & 0xFF, false);
                self.r[rd] = r;
                self.set_nz(r);
                self.set_c(c);
            }
            0x5 => self.r[rd] = self.adc(a, b, true),
            0x6 => self.r[rd] = self.sbc(a, b, true),
            0x8 => self.set_nz(a & b),
            0x9 => self.r[rd] = self.sub(0, b, true),
            0xA => { self.sub(a, b, true); }
            0xB => { self.add(a, b, true); }
            0xC => { self.r[rd] = a | b; self.set_nz(a | b); }
            0xD => {
                let full = Self::tick_multiply(bus, a, true);
                self.next_fetch = ACCESS_CODE | ACCESS_NSEQ;
                let r = b.wrapping_mul(a);
                self.r[rd] = r;
                self.set_nz(r);
                if !self.v5 {
                    let c = if full { mul_carry_simple(a) } else { mul_carry_lo(b, a, 0) };
                    self.set_c(c as u32);
                }
            }
            0xE => { self.r[rd] = a & !b; self.set_nz(a & !b); }
            _ => { self.r[rd] = !b; self.set_nz(!b); }
        }
    }

    /// Thumb format 5: ADD / CMP / MOV with high registers, BX.
    fn thumb_hireg(&mut self, bus: &mut dyn ArmBus, op: u32) {
        let rd = ((op & 7) | ((op >> 4) & 8)) as usize;
        let rs = ((op >> 3) & 0xF) as usize;
        let mut v = self.r[rs];
        if rs == 15 { v &= !1; }
        match (op >> 8) & 3 {
            3 => {
                if self.v5 && bit(op, 7) {
                    // BLX Rm
                    self.r[14] = self.r[15].wrapping_sub(2) | 1;
                }
                if v & 1 != 0 {
                    self.jump_to(bus, v & !1);
                } else {
                    self.cpsr &= !FLAG_T;
                    self.jump_to(bus, v);
                }
            }
            1 => { self.sub(self.r[rd], v, true); self.advance16(ACCESS_SEQ); }
            k => {
                if k == 0 { self.r[rd] = self.r[rd].wrapping_add(v) } else { self.r[rd] = v }
                if rd == 15 {
                    self.r[15] &= !1;
                    self.reload(bus);
                } else {
                    self.advance16(ACCESS_SEQ);
                }
            }
        }
    }

    fn thumb_push_pop(&mut self, bus: &mut dyn ArmBus, op: u32) {
        let pop = bit(op, 11);
        let rbit = bit(op, 8);
        let list = op & 0xFF;
        self.advance16(ACCESS_NSEQ);
        if list == 0 && !rbit {
            // empty list: r15 is moved, SP steps by 0x40
            if pop {
                self.r[15] = bus.read32(self.r[13], ACCESS_NSEQ);
                self.reload(bus);
                self.r[13] = self.r[13].wrapping_add(0x40);
            } else {
                self.r[13] = self.r[13].wrapping_sub(0x40);
                bus.write32(self.r[13], self.r[15], ACCESS_NSEQ);
            }
            return;
        }
        let mut addr = self.r[13];
        let mut acc = ACCESS_NSEQ;
        if pop {
            for i in 0..8 {
                if bit(list, i) {
                    self.r[i as usize] = bus.read32(addr, acc);
                    acc = ACCESS_SEQ;
                    addr = addr.wrapping_add(4);
                }
            }
            if rbit {
                let v = bus.read32(addr, acc);
                self.r[13] = addr.wrapping_add(4);
                bus.idle();
                if self.v5 {
                    self.jump_interwork(bus, v);
                } else {
                    self.r[15] = v & !1;
                    self.reload(bus);
                }
                return;
            }
            bus.idle();
            self.r[13] = addr;
        } else {
            addr = addr.wrapping_sub(list.count_ones() * 4 + if rbit { 4 } else { 0 });
            self.r[13] = addr;
            for i in 0..8 {
                if bit(list, i) {
                    bus.write32(addr, self.r[i as usize], acc);
                    acc = ACCESS_SEQ;
                    addr = addr.wrapping_add(4);
                }
            }
            if rbit { bus.write32(addr, self.r[14], acc); }
        }
    }

    fn thumb_ldm_stm(&mut self, bus: &mut dyn ArmBus, op: u32) {
        let load = bit(op, 11);
        let rb = ((op >> 8) & 7) as usize;
        let list = op & 0xFF;
        self.advance16(ACCESS_NSEQ);
        if list == 0 {
            if load {
                self.r[15] = bus.read32(self.r[rb], ACCESS_NSEQ);
                self.reload(bus);
            } else {
                bus.write32(self.r[rb], self.r[15], ACCESS_NSEQ);
            }
            self.r[rb] = self.r[rb].wrapping_add(0x40);
            return;
        }
        let mut addr = self.r[rb];
        if load {
            let mut acc = ACCESS_NSEQ;
            for i in 0..8 {
                if bit(list, i) {
                    self.r[i as usize] = bus.read32(addr, acc);
                    acc = ACCESS_SEQ;
                    addr = addr.wrapping_add(4);
                }
            }
            bus.idle();
            if !bit(list, rb as u32) { self.r[rb] = addr; }
        } else {
            let first = list.trailing_zeros();
            let base_new = addr.wrapping_add(list.count_ones() * 4);
            bus.write32(addr, self.r[first as usize], ACCESS_NSEQ);
            self.r[rb] = base_new;
            addr = addr.wrapping_add(4);
            for i in first + 1..8 {
                if bit(list, i) {
                    bus.write32(addr, self.r[i as usize], ACCESS_SEQ);
                    addr = addr.wrapping_add(4);
                }
            }
        }
    }
}

// ---- multiply carry flag ----
//
// The ARM7TDMI leaves the carry out of its Booth multiplier's last step in C.
// Model by zaydlang and calc84maniac (zlib licence, (C) 2024), as used in
// NanoBoyAdvance; altered here: translated to Rust.

/// All four multiplier bytes used: the carry comes straight from the last Booth step.
fn mul_carry_simple(multiplier: u32) -> bool {
    (multiplier >> 30) == 2
}

fn mul_carry_lo(multiplicand: u32, multiplier: u32, accum: u32) -> bool {
    let multiplicand = multiplicand | 1;
    let mut accum = accum;
    let mut booth = ((multiplier << 31) as i32 >> 31) as u32;
    let mut carry = multiplicand.wrapping_mul(booth);
    let mut sum = carry.wrapping_add(accum);
    let mut shift: i32 = 29;
    loop {
        for _ in 0..4 {
            let next = if shift >= 0 { ((multiplier << shift) as i32 >> shift) as u32 } else { multiplier };
            let factor = next.wrapping_sub(booth);
            booth = next;
            let addend = multiplicand.wrapping_mul(factor);
            accum ^= carry ^ addend;
            sum = sum.wrapping_add(addend);
            carry = sum.wrapping_sub(accum);
            shift -= 2;
        }
        if booth == multiplier { break; }
    }
    carry >> 31 != 0
}

fn mul_carry_hi(multiplicand: u32, multiplier: u32, accum_hi: u32, signed: bool) -> bool {
    let (mut mc, ml) = if signed {
        (((multiplicand as i32) >> 6) as u32, ((multiplier as i32) >> 26) as u32)
    } else {
        (multiplicand >> 6, multiplier >> 26)
    };
    mc |= 1;
    let carry = !accum_hi & 0x2000_0000;
    let mut accum = accum_hi.wrapping_sub(0x0800_0000);
    let booth0 = ((ml << 27) as i32 >> 27) as u32;
    let booth1 = ((ml << 29) as i32 >> 29) as u32;
    let booth2 = ((ml << 31) as i32 >> 31) as u32;
    let factor0 = ml.wrapping_sub(booth0);
    let factor1 = booth0.wrapping_sub(booth1);
    let factor2 = booth1.wrapping_sub(booth2);
    let mut addend = mc.wrapping_mul(factor2);
    accum = accum.wrapping_sub(addend & 0x1000_0000);
    addend = mc.wrapping_mul(factor1);
    accum = accum.wrapping_sub(addend & 0x4000_0000);
    let mut sum = accum.wrapping_add(addend & 0x2000_0000);
    accum = accum.wrapping_sub(carry);
    addend = mc.wrapping_mul(factor0);
    sum = sum.wrapping_add(addend & 0x4000_0000);
    (sum ^ accum) >> 31 != 0
}
