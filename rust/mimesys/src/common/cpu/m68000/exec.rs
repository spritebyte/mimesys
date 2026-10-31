//! 68000 instruction execution: Clock Signal's bus programs, one function per state
//! family, with address errors propagated by `?`.

use super::decode::{decode, Inst, Mode, Op, Shift, Size};
use super::{vector, M68000, M68kBus, R, SR_C, SR_N, SR_S, SR_V, SR_X, SR_Z};

const FETCH0: u8 = 1;
const FETCH1: u8 = 2;
const STORE0: u8 = 4;
const STORE1: u8 = 8;

/// Which operands go through the generic fetch / store sequences (CLK operand_flags).
fn operand_flags(op: Op) -> u8 {
    match op {
        Op::Pea | Op::Jmp | Op::Jsr | Op::MovepToR | Op::MovepToM | Op::Tas | Op::Rtr | Op::Rts | Op::Rte
        | Op::Trap | Op::Reset | Op::Nop | Op::Stop | Op::Trapv => 0,
        Op::MoveToSr | Op::MoveToCcr | Op::MoveToUsp | Op::OriSr | Op::OriCcr | Op::AndiSr | Op::AndiCcr
        | Op::EoriSr | Op::EoriCcr | Op::Bcc | Op::Bsr | Op::Tst | Op::MovemToM | Op::MovemToR => FETCH0,
        Op::MoveFromUsp => STORE0,
        Op::Nbcd | Op::Not | Op::Neg | Op::Negx | Op::Ext | Op::Swap | Op::Unlk | Op::ShiftM(..) | Op::Scc
        | Op::MoveFromSr | Op::Clr => FETCH0 | STORE0,
        Op::Cmp | Op::Cmpa | Op::Chk | Op::Btst | Op::Link => FETCH0 | FETCH1,
        Op::Move | Op::Movea => FETCH0 | STORE1,
        Op::Abcd | Op::Sbcd | Op::Add | Op::Adda | Op::Addx | Op::Sub | Op::Suba | Op::Subx | Op::Or | Op::And
        | Op::Eor | Op::Divu | Op::Divs | Op::Mulu | Op::Muls | Op::ShiftR(..) | Op::Bchg | Op::Bclr | Op::Bset
        => FETCH0 | FETCH1 | STORE1,
        Op::Dbcc => FETCH0 | FETCH1 | STORE0,
        Op::Exg => FETCH0 | FETCH1 | STORE0 | STORE1,
        Op::Lea => STORE1,
        Op::Undefined => 0,
    }
}

fn privileged(op: Op) -> bool {
    matches!(op, Op::OriSr | Op::AndiSr | Op::EoriSr | Op::Rte | Op::Reset | Op::Stop | Op::MoveToUsp | Op::MoveFromUsp | Op::MoveToSr)
}

/// (An)+ / -(An) step: bytes on A7 move by 2.
fn increment(size: Size, reg: u8) -> u32 {
    match size { Size::Byte => if reg == 7 { 2 } else { 1 }, Size::Word => 2, Size::Long => 4 }
}

#[inline] fn sext8(v: u32) -> u32 { v as u8 as i8 as i32 as u32 }
#[inline] fn sext16(v: u32) -> u32 { v as u16 as i16 as i32 as u32 }

#[derive(Clone, Copy, PartialEq, Eq)]
enum Perform { Np, NpN, NpNn }

impl M68000 {
    fn flag(&self, f: u16) -> bool { self.sr & f != 0 }
    fn set_flag(&mut self, f: u16, on: bool) { if on { self.sr |= f } else { self.sr &= !f } }

    fn set_nz(&mut self, v: u32, size: Size) {
        let v = v & size.mask();
        self.set_flag(SR_Z, v == 0);
        self.set_flag(SR_N, v & size.msb() != 0);
    }

    fn condition(&self, c: u8) -> bool {
        let (cf, v, z, n) = (self.flag(SR_C), self.flag(SR_V), self.flag(SR_Z), self.flag(SR_N));
        match c {
            0 => true, 1 => false,
            2 => !cf && !z, 3 => cf || z,
            4 => !cf, 5 => cf,
            6 => !z, 7 => z,
            8 => !v, 9 => v,
            10 => !n, 11 => n,
            12 => n == v, 13 => n != v,
            14 => !z && n == v, _ => z || n != v,
        }
    }

    fn d8xn(&self, base: u32) -> u32 {
        let ext = self.prefetch[1] as u32;
        let x = self.r[(ext >> 12) as usize];
        let idx = if ext & 0x800 != 0 { x } else { sext16(x) };
        base.wrapping_add(idx).wrapping_add(sext8(ext))
    }

    fn quick(&self, op: Op) -> u32 {
        match op {
            Op::Bcc | Op::Bsr | Op::Move => sext8(self.opcode as u32),
            Op::Trap => (self.opcode & 15) as u32,
            _ => { let v = ((self.opcode >> 9) & 7) as u32; if v == 0 { 8 } else { v } }
        }
    }

    /// Write the operand into a data register, keeping the bits above `size`.
    fn set_dn(&mut self, reg: usize, v: u32, size: Size) {
        let m = size.mask();
        self.r[reg] = (self.r[reg] & !m) | (v & m);
    }

    // ================================================================ dispatch

    pub(crate) fn execute(&mut self, bus: &mut dyn M68kBus) -> R<()> {
        let opcode = self.prefetch[0];
        self.opcode = opcode;
        let i = decode(opcode);

        if i.op == Op::Undefined {
            let v = match opcode & 0xF000 { 0xA000 => vector::LINE_A, 0xF000 => vector::LINE_F, _ => vector::ILLEGAL };
            self.idle(2);
            return self.exception(bus, v);
        }
        if privileged(i.op) && self.sr & SR_S == 0 {
            self.idle(2);
            return self.exception(bus, vector::PRIVILEGE);
        }

        match i.op {
            // ---- special sequences
            Op::Dbcc => return self.dbcc(bus, &i),
            Op::Bcc => return self.bcc(bus, &i),
            Op::Bsr => return self.bsr(bus, &i),
            Op::Jmp | Op::Jsr => return self.jsr_jmp(bus, &i),
            Op::Lea => {
                self.calc_ea_idle(bus, &i, 0)?;
                self.r[8 + i.reg[1] as usize] = self.ea[0];
                return self.prefetch(bus);
            }
            Op::Pea => return self.pea(bus, &i),
            Op::Tas if i.m[0] != Mode::Dn => return self.tas_mem(bus, &i),
            Op::MovepToM | Op::MovepToR => return self.movep(bus, &i),
            Op::Rts => return self.rts(bus),
            Op::Rte | Op::Rtr => return self.rte_rtr(bus, i.op == Op::Rte),
            Op::Link => return self.link(bus, &i),
            Op::Unlk => return self.unlk(bus, &i),
            Op::Reset => {
                self.idle(2);
                self.cycles += 124;
                bus.reset_devices();
                return self.prefetch(bus);
            }
            Op::Nop => return self.prefetch(bus),
            Op::Stop => {
                let v = self.prefetch[1];
                self.set_sr(v);
                self.instruction_address = self.pc;
                self.stopped = true;
                return Ok(());
            }
            Op::Trap => {
                self.idle(2);
                self.instruction_address = self.instruction_address.wrapping_add(2);
                return self.exception(bus, vector::TRAP_BASE + (opcode & 15) as u32);
            }
            Op::Trapv => {
                self.prefetch(bus)?;
                if !self.flag(SR_V) { return Ok(()); }
                self.instruction_address = self.instruction_address.wrapping_add(2);
                return self.exception(bus, vector::TRAPV);
            }
            Op::Abcd | Op::Sbcd | Op::Addx | Op::Subx if i.m[0] == Mode::PreDec => {
                return if i.size == Size::Long { self.two_op_predec_l(bus, &i) } else { self.two_op_predec_bw(bus, &i) };
            }
            _ => {}
        }

        // ---- generic: fetch operands, perform, store
        let flags = operand_flags(i.op);
        for n in 0..2 {
            if flags & (1 << n) != 0 {
                self.fetch_operand(bus, &i, n)?;
            }
        }

        match i.op {
            Op::Move | Op::Movea => {
                self.perform(&i);
                return self.move_store(bus, &i);
            }
            Op::Chk => return self.chk(bus, &i),
            Op::Scc if i.m[0] == Mode::Dn => {
                self.prefetch(bus)?;
                let c = self.condition(i.cond);
                self.operand[0] = if c { 0xFF } else { 0 };
                if c { self.idle(1); }
                return self.store(bus, &i, flags);
            }
            Op::Bchg | Op::Bset | Op::Bclr if matches!(i.m[1], Mode::Dn | Mode::Imm) => {
                self.perform(&i);
                self.prefetch(bus)?;
                let extra = if i.op == Op::Bclr { 2 } else { 1 };
                self.idle(extra + self.dynamic_length);
                self.r[i.reg[1] as usize] = self.operand[1];
                return Ok(());
            }
            Op::OriSr | Op::AndiSr | Op::EoriSr | Op::OriCcr | Op::AndiCcr | Op::EoriCcr => {
                self.idle(4);
                self.perform(&i);
                self.pc = self.pc.wrapping_sub(2);
                self.prefetch(bus)?;
                return self.prefetch(bus);
            }
            Op::MoveToSr | Op::MoveToCcr => {
                self.perform(&i);
                self.idle(2);
                self.pc = self.pc.wrapping_sub(2);
                self.prefetch(bus)?;
                return self.prefetch(bus);
            }
            Op::MovemToR => return self.movem_to_r(bus, &i),
            Op::MovemToM => return self.movem_to_m(bus, &i),
            Op::Divu | Op::Divs => {
                let exception = self.perform(&i);
                self.idle(self.dynamic_length);
                if exception { return self.exception(bus, vector::ZERO_DIVIDE); }
                self.r[i.reg[1] as usize] = self.operand[1];
                return self.prefetch(bus);
            }
            Op::Mulu | Op::Muls | Op::ShiftR(..) => {
                self.prefetch(bus)?;
                self.perform(&i);
                self.idle(self.dynamic_length);
                self.r[i.reg[1] as usize] = self.operand[1];
                return Ok(());
            }
            _ => {}
        }

        let ps = self.perform_state(&i);
        self.perform(&i);
        self.prefetch(bus)?;
        match ps { Perform::Np => {}, Perform::NpN => self.idle(1), Perform::NpNn => self.idle(2) }
        self.store(bus, &i, flags)
    }

    /// CLK's choice of Perform_np / np_n / np_nn per operation.
    fn perform_state(&self, i: &Inst) -> Perform {
        use Perform::*;
        let (m0, m1) = (i.m[0], i.m[1]);
        let long = i.size == Size::Long;
        match i.op {
            Op::Nbcd => if m0 == Mode::Dn { NpN } else { Np },
            Op::Clr | Op::Neg | Op::Negx | Op::Not => if long && m0 == Mode::Dn { NpN } else { Np },
            Op::Exg => NpN,
            Op::Cmp => if long && m1 == Mode::Dn { NpN } else { Np },
            Op::Cmpa => NpN,
            Op::And | Op::Or => {
                if long && m1 == Mode::Dn {
                    if matches!(m0, Mode::Dn | Mode::Imm) { NpNn } else { NpN }
                } else { Np }
            }
            Op::Eor => if long && m1 == Mode::Dn { NpNn } else { Np },
            Op::Abcd | Op::Sbcd => NpN,
            Op::Add | Op::Sub => {
                if !long { return Np; }
                if m0 == Mode::Quick {
                    if matches!(m1, Mode::An | Mode::Dn) { NpNn } else { Np }
                } else if m1 != Mode::Dn {
                    Np
                } else if matches!(m0, Mode::Dn | Mode::An | Mode::Imm) { NpNn } else { NpN }
            }
            Op::Adda | Op::Suba => {
                if !long { return NpNn; }
                if matches!(m0, Mode::Dn | Mode::An | Mode::Imm) { NpNn } else { NpN }
            }
            Op::Addx | Op::Subx => if long { NpNn } else { Np },
            Op::Btst => if matches!(m1, Mode::Dn | Mode::Imm) { NpN } else { Np },
            Op::MoveFromSr => if m0 == Mode::Dn { NpN } else { Np },
            _ => Np,
        }
    }

    // ================================================================ operand fetch / store

    fn fetch_operand(&mut self, bus: &mut dyn M68kBus, i: &Inst, n: usize) -> R<()> {
        let long = i.size == Size::Long;
        let reg = i.reg[n] as usize;
        let an = 8 + reg;
        match i.m[n] {
            Mode::Dn | Mode::An => { self.operand[n] = self.r[i.lreg(n)]; return Ok(()); }
            Mode::Quick => { self.operand[n] = self.quick(i.op); return Ok(()); }
            Mode::Imm => {
                if long {
                    self.prefetch(bus)?;
                    self.operand[n] = self.prefetch_l();
                } else {
                    self.operand[n] = (self.operand[n] & 0xFFFF_0000) | self.prefetch[1] as u32;
                }
                return self.prefetch(bus);
            }
            Mode::ExtWord => {
                self.operand[n] = (self.operand[n] & 0xFFFF_0000) | self.prefetch[1] as u32;
                return self.prefetch(bus);
            }
            Mode::Ind => self.ea[n] = self.r[an],
            Mode::PostInc => {
                self.ea[n] = self.r[an];
                self.r[an] = self.r[an].wrapping_add(increment(i.size, reg as u8));
            }
            Mode::PreDec => {
                self.r[an] = self.r[an].wrapping_sub(increment(i.size, reg as u8));
                self.ea[n] = self.r[an];
                self.idle(1);
            }
            Mode::Disp => {
                self.ea[n] = self.r[an].wrapping_add(sext16(self.prefetch[1] as u32));
                self.prefetch(bus)?;
            }
            Mode::PcDisp => {
                self.ea[n] = self.pc.wrapping_sub(2).wrapping_add(sext16(self.prefetch[1] as u32));
                self.prefetch(bus)?;
            }
            Mode::Index => {
                self.ea[n] = self.d8xn(self.r[an]);
                self.idle(1);
                self.prefetch(bus)?;
            }
            Mode::PcIndex => {
                self.ea[n] = self.d8xn(self.pc.wrapping_sub(2));
                self.idle(1);
                self.prefetch(bus)?;
            }
            Mode::AbsW => {
                self.ea[n] = sext16(self.prefetch[1] as u32);
                self.prefetch(bus)?;
            }
            Mode::AbsL => {
                self.prefetch(bus)?;
                self.ea[n] = self.prefetch_l();
                self.prefetch(bus)?;
            }
            Mode::None => return Ok(()),
        }
        // the memory read
        if long {
            let hi = self.read_w(bus, self.ea[n])? as u32;
            self.operand[n] = (self.operand[n] & 0xFFFF) | hi << 16;
            self.ea[n] = self.ea[n].wrapping_add(2);
            let lo = self.read_w(bus, self.ea[n])? as u32;
            self.operand[n] = hi << 16 | lo;
        } else if i.size == Size::Byte {
            let v = self.read_b(bus, self.ea[n]) as u32;
            self.operand[n] = (self.operand[n] & !0xFF) | v;
        } else {
            let v = self.read_w(bus, self.ea[n])? as u32;
            self.operand[n] = (self.operand[n] & !0xFFFF) | v;
        }
        Ok(())
    }

    fn store(&mut self, bus: &mut dyn M68kBus, i: &Inst, flags: u8) -> R<()> {
        if flags & (STORE0 | STORE1) == 0 { return Ok(()); }
        for n in 0..2 {
            if flags & (STORE0 << n) == 0 { continue; }
            match i.m[n] {
                Mode::Dn => {
                    let v = self.operand[n];
                    self.set_dn(i.reg[n] as usize, v, i.size);
                }
                Mode::An => self.r[8 + i.reg[n] as usize] = self.operand[n],
                _ => {
                    if i.size == Size::Long {
                        self.write_w(bus, self.ea[n], self.operand[n] as u16)?;
                        self.ea[n] = self.ea[n].wrapping_sub(2);
                        self.write_w(bus, self.ea[n], (self.operand[n] >> 16) as u16)?;
                    } else {
                        self.write_bw(bus, self.ea[n], i.size, self.operand[n])?;
                    }
                }
            }
        }
        Ok(())
    }

    /// CalcEffectiveAddress (no data access), for operand `n`.
    fn calc_ea(&mut self, bus: &mut dyn M68kBus, i: &Inst, n: usize) -> R<()> {
        let reg = i.reg[n] as usize;
        let an = 8 + reg;
        match i.m[n] {
            Mode::Ind => self.ea[n] = self.r[an],
            Mode::PostInc => {
                self.ea[n] = self.r[an];
                self.r[an] = self.r[an].wrapping_add(increment(i.size, reg as u8));
            }
            Mode::PreDec => {
                self.r[an] = self.r[an].wrapping_sub(increment(i.size, reg as u8));
                self.ea[n] = self.r[an];
            }
            Mode::Disp => { self.ea[n] = self.r[an].wrapping_add(sext16(self.prefetch[1] as u32)); self.prefetch(bus)?; }
            Mode::PcDisp => { self.ea[n] = self.pc.wrapping_sub(2).wrapping_add(sext16(self.prefetch[1] as u32)); self.prefetch(bus)?; }
            Mode::Index => { self.ea[n] = self.d8xn(self.r[an]); self.prefetch(bus)?; self.idle(1); }
            Mode::PcIndex => { self.ea[n] = self.d8xn(self.pc.wrapping_sub(2)); self.prefetch(bus)?; self.idle(1); }
            Mode::AbsW => { self.ea[n] = sext16(self.prefetch[1] as u32); self.prefetch(bus)?; }
            Mode::AbsL => { self.prefetch(bus)?; self.ea[n] = self.prefetch_l(); self.prefetch(bus)?; }
            _ => {}
        }
        Ok(())
    }

    /// CalcEffectiveAddressIdleFor8bitDisplacementAndPreDec (LEA, TAS).
    fn calc_ea_idle(&mut self, bus: &mut dyn M68kBus, i: &Inst, n: usize) -> R<()> {
        if matches!(i.m[n], Mode::PreDec | Mode::Index | Mode::PcIndex) { self.idle(1); }
        self.calc_ea(bus, i, n)
    }

    /// ..._n_np form of (d8, An/PC, Xn) used by MOVEM and TAS.
    fn calc_ea_index_n_np(&mut self, bus: &mut dyn M68kBus, i: &Inst, n: usize) -> R<()> {
        let base = if i.m[n] == Mode::PcIndex { self.pc.wrapping_sub(2) } else { self.r[8 + i.reg[n] as usize] };
        self.ea[n] = self.d8xn(base);
        self.idle(1);
        self.prefetch(bus)
    }

    // ================================================================ MOVE

    fn move_store(&mut self, bus: &mut dyn M68kBus, i: &Inst) -> R<()> {
        let reg = i.reg[1] as usize;
        let an = 8 + reg;
        let v = self.operand[1];
        let size = i.size;
        let long = size == Size::Long;
        match i.m[1] {
            Mode::An => { self.r[an] = v; return self.prefetch(bus); }
            Mode::Dn => {
                if long { self.r[reg] = v } else { self.set_dn(reg, v, size) }
                return self.prefetch(bus);
            }
            Mode::Ind => {
                self.ea[1] = self.r[an];
                if long {
                    self.write_w(bus, self.ea[1], (v >> 16) as u16)?;
                    self.ea[1] = self.ea[1].wrapping_add(2);
                    self.write_w(bus, self.ea[1], v as u16)?;
                } else {
                    self.write_bw(bus, self.r[an], size, v)?;
                }
                return self.prefetch(bus);
            }
            Mode::PostInc => {
                if long {
                    self.write_w(bus, self.r[an], (v >> 16) as u16)?;
                    self.r[an] = self.r[an].wrapping_add(2);
                    self.write_w(bus, self.r[an], v as u16)?;
                    self.r[an] = self.r[an].wrapping_add(2);
                } else {
                    self.write_bw(bus, self.r[an], size, v)?;
                    self.r[an] = self.r[an].wrapping_add(increment(size, reg as u8));
                }
                return self.prefetch(bus);
            }
            Mode::PreDec => {
                if long {
                    self.prefetch(bus)?;
                    self.r[an] = self.r[an].wrapping_sub(2);
                    self.write_w(bus, self.r[an], v as u16)?;
                    self.r[an] = self.r[an].wrapping_sub(2);
                    self.write_w(bus, self.r[an], (v >> 16) as u16)?;
                } else {
                    self.r[an] = self.r[an].wrapping_sub(increment(size, reg as u8));
                    self.ea[1] = self.r[an];
                    self.prefetch(bus)?;
                    self.write_bw(bus, self.ea[1], size, v)?;
                }
                return Ok(());
            }
            Mode::AbsL => {
                self.prefetch(bus)?;
                self.ea[1] = self.prefetch_l();
                let early = matches!(i.m[0], Mode::An | Mode::Dn | Mode::Imm | Mode::Quick);
                if early { self.prefetch(bus)?; }
                self.move_write(bus, size, v)?;
                self.prefetch(bus)?;
                if !early { self.prefetch(bus)?; }
                return Ok(());
            }
            Mode::Disp => {
                self.ea[1] = self.r[an].wrapping_add(sext16(self.prefetch[1] as u32));
                self.prefetch(bus)?;
            }
            Mode::Index => {
                self.ea[1] = self.d8xn(self.r[an]);
                self.idle(1);
                self.prefetch(bus)?;
            }
            Mode::AbsW => {
                self.ea[1] = sext16(self.prefetch[1] as u32);
                self.prefetch(bus)?;
            }
            _ => unreachable!("MOVE destination"),
        }
        self.move_write(bus, size, v)?;
        self.prefetch(bus)
    }

    fn move_write(&mut self, bus: &mut dyn M68kBus, size: Size, v: u32) -> R<()> {
        if size == Size::Long {
            self.write_w(bus, self.ea[1], (v >> 16) as u16)?;
            self.ea[1] = self.ea[1].wrapping_add(2);
            self.write_w(bus, self.ea[1], v as u16)
        } else {
            self.write_bw(bus, self.ea[1], size, v)
        }
    }

    // ================================================================ special states

    fn two_op_predec_bw(&mut self, bus: &mut dyn M68kBus, i: &Inst) -> R<()> {
        self.idle(1);
        let (a0, a1) = (8 + i.reg[0] as usize, 8 + i.reg[1] as usize);
        self.r[a0] = self.r[a0].wrapping_sub(increment(i.size, i.reg[0]));
        let v = self.read_bw(bus, self.r[a0], i.size)?;
        self.operand[0] = (self.operand[0] & !i.size.mask()) | v;
        self.r[a1] = self.r[a1].wrapping_sub(increment(i.size, i.reg[1]));
        let v = self.read_bw(bus, self.r[a1], i.size)?;
        self.operand[1] = (self.operand[1] & !i.size.mask()) | v;
        self.prefetch(bus)?;
        self.perform(i);
        self.write_bw(bus, self.r[a1], i.size, self.operand[1])
    }

    fn two_op_predec_l(&mut self, bus: &mut dyn M68kBus, i: &Inst) -> R<()> {
        self.idle(1);
        let (a0, a1) = (8 + i.reg[0] as usize, 8 + i.reg[1] as usize);
        self.r[a0] = self.r[a0].wrapping_sub(2);
        let lo = self.read_w(bus, self.r[a0])? as u32;
        self.r[a0] = self.r[a0].wrapping_sub(2);
        let hi = self.read_w(bus, self.r[a0])? as u32;
        self.operand[0] = hi << 16 | lo;
        self.r[a1] = self.r[a1].wrapping_sub(2);
        let lo = self.read_w(bus, self.r[a1])? as u32;
        self.r[a1] = self.r[a1].wrapping_sub(2);
        let hi = self.read_w(bus, self.r[a1])? as u32;
        self.operand[1] = hi << 16 | lo;
        self.perform(i);
        self.r[a1] = self.r[a1].wrapping_add(2);
        self.write_w(bus, self.r[a1], self.operand[1] as u16)?;
        self.prefetch(bus)?;
        self.r[a1] = self.r[a1].wrapping_sub(2);
        self.write_w(bus, self.r[a1], (self.operand[1] >> 16) as u16)
    }

    fn chk(&mut self, bus: &mut dyn M68kBus, i: &Inst) -> R<()> {
        self.prefetch(bus)?;
        let bound = self.operand[0] as u16 as i16;
        let value = self.operand[1] as u16 as i16;
        let under = value < 0;
        let over = value > bound;
        self.sr &= !(SR_V | SR_C);
        self.set_flag(SR_Z, value == 0);
        if over { self.sr &= !SR_N; }
        if under { self.sr |= SR_N; }
        let _ = i;
        if over {
            self.idle(2);
        } else if under {
            self.idle(3);
        } else {
            self.idle(3);
            return Ok(());
        }
        self.instruction_address = self.pc.wrapping_sub(4);
        self.exception(bus, vector::CHK)
    }

    fn dbcc(&mut self, bus: &mut dyn M68kBus, i: &Inst) -> R<()> {
        let reg = i.reg[0] as usize;
        let disp = sext16(self.prefetch[1] as u32);
        if self.condition(i.cond) {
            self.idle(2);
            self.prefetch(bus)?;
            return self.prefetch(bus);
        }
        let count = (self.r[reg] as u16).wrapping_sub(1);
        self.r[reg] = (self.r[reg] & 0xFFFF_0000) | count as u32;
        self.pc = self.instruction_address.wrapping_add(disp).wrapping_add(2);
        self.idle(1);
        if count == 0xFFFF {
            self.prefetch(bus)?;
            self.pc = self.instruction_address.wrapping_add(4);
            self.prefetch(bus)?;
            return self.prefetch(bus);
        }
        self.prefetch(bus)?;
        self.prefetch(bus)
    }

    fn bcc(&mut self, bus: &mut dyn M68kBus, i: &Inst) -> R<()> {
        let word = i.size == Size::Word;
        let disp = if word { sext16(self.prefetch[1] as u32) } else { sext8(self.opcode as u32) };
        if self.condition(i.cond) {
            self.pc = self.instruction_address.wrapping_add(disp).wrapping_add(2);
            self.idle(1);
            self.prefetch(bus)?;
            return self.prefetch(bus);
        }
        self.idle(2);
        self.prefetch(bus)?;
        if word { self.prefetch(bus)?; }
        Ok(())
    }

    fn push_l(&mut self, bus: &mut dyn M68kBus, v: u32) -> R<()> {
        self.r[15] = self.r[15].wrapping_sub(4);
        self.write_w(bus, self.r[15], (v >> 16) as u16)?;
        self.r[15] = self.r[15].wrapping_add(2);
        self.write_w(bus, self.r[15], v as u16)?;
        self.r[15] = self.r[15].wrapping_sub(2);
        Ok(())
    }

    fn pop_l(&mut self, bus: &mut dyn M68kBus) -> R<u32> {
        let hi = self.read_w(bus, self.r[15])? as u32;
        self.r[15] = self.r[15].wrapping_add(2);
        let lo = self.read_w(bus, self.r[15])? as u32;
        self.r[15] = self.r[15].wrapping_add(2);
        Ok(hi << 16 | lo)
    }

    fn bsr(&mut self, bus: &mut dyn M68kBus, i: &Inst) -> R<()> {
        self.idle(1);
        let ia = self.instruction_address;
        let ret = if i.size == Size::Word {
            self.pc = ia.wrapping_add(sext16(self.prefetch[1] as u32)).wrapping_add(2);
            ia.wrapping_add(4)
        } else {
            self.pc = ia.wrapping_add(sext8(self.opcode as u32)).wrapping_add(2);
            ia.wrapping_add(2)
        };
        self.push_l(bus, ret)?;
        self.prefetch(bus)?;
        self.prefetch(bus)
    }

    fn jsr_jmp(&mut self, bus: &mut dyn M68kBus, i: &Inst) -> R<()> {
        let ia = self.instruction_address;
        let an = 8 + i.reg[0] as usize;
        let ret;
        let target = match i.m[0] {
            Mode::Ind => { ret = ia.wrapping_add(2); self.r[an] }
            Mode::Disp => { let t = self.r[an].wrapping_add(sext16(self.prefetch[1] as u32)); self.idle(1); ret = ia.wrapping_add(4); t }
            Mode::Index => { let t = self.d8xn(self.r[an]); self.idle(3); ret = ia.wrapping_add(4); t }
            Mode::PcDisp => { let t = self.pc.wrapping_sub(2).wrapping_add(sext16(self.prefetch[1] as u32)); self.idle(1); ret = ia.wrapping_add(4); t }
            Mode::PcIndex => { let t = self.d8xn(self.pc.wrapping_sub(2)); self.idle(3); ret = ia.wrapping_add(4); t }
            Mode::AbsW => { let t = sext16(self.prefetch[1] as u32); self.idle(1); ret = ia.wrapping_add(4); t }
            _ => { self.prefetch(bus)?; ret = ia.wrapping_add(6); self.prefetch_l() }   // AbsL
        };
        self.pc = target;
        self.prefetch(bus)?;
        if i.op == Op::Jsr { self.push_l(bus, ret)?; }
        self.prefetch(bus)
    }

    fn pea(&mut self, bus: &mut dyn M68kBus, i: &Inst) -> R<()> {
        let an = 8 + i.reg[0] as usize;
        let late_np = match i.m[0] {
            Mode::Ind => { self.ea[0] = self.r[an]; false }
            Mode::Disp => { self.ea[0] = self.r[an].wrapping_add(sext16(self.prefetch[1] as u32)); self.prefetch(bus)?; false }
            Mode::PcDisp => { self.ea[0] = self.pc.wrapping_sub(2).wrapping_add(sext16(self.prefetch[1] as u32)); self.prefetch(bus)?; false }
            Mode::Index => { self.ea[0] = self.d8xn(self.r[an]); self.idle(1); self.prefetch(bus)?; self.idle(1); false }
            Mode::PcIndex => { self.ea[0] = self.d8xn(self.pc.wrapping_sub(2)); self.idle(1); self.prefetch(bus)?; self.idle(1); false }
            Mode::AbsW => { self.ea[0] = sext16(self.prefetch[1] as u32); true }
            _ => { self.prefetch(bus)?; self.ea[0] = self.prefetch_l(); true }   // AbsL
        };
        self.prefetch(bus)?;
        self.push_l(bus, self.ea[0])?;
        if late_np { self.prefetch(bus)?; }
        Ok(())
    }

    fn tas_mem(&mut self, bus: &mut dyn M68kBus, i: &Inst) -> R<()> {
        if i.m[0] == Mode::Index {
            self.calc_ea_index_n_np(bus, i, 0)?;
        } else {
            self.calc_ea_idle(bus, i, 0)?;
        }
        let v = self.read_b(bus, self.ea[0]);
        self.idle(1);
        self.sr &= !(SR_V | SR_C);
        self.set_nz(v as u32, Size::Byte);
        self.write_b(bus, self.ea[0], v | 0x80);
        self.prefetch(bus)
    }

    fn movep(&mut self, bus: &mut dyn M68kBus, i: &Inst) -> R<()> {
        if i.op == Op::MovepToM {
            let mut addr = self.r[8 + i.reg[1] as usize].wrapping_add(sext16(self.prefetch[1] as u32));
            self.prefetch(bus)?;
            let d = self.r[i.reg[0] as usize];
            let shifts: &[u32] = if i.size == Size::Long { &[24, 16, 8, 0] } else { &[8, 0] };
            for (k, s) in shifts.iter().enumerate() {
                if k > 0 { addr = addr.wrapping_add(2); }
                self.write_b(bus, addr, (d >> s) as u8);
            }
        } else {
            let mut addr = self.r[8 + i.reg[0] as usize].wrapping_add(sext16(self.prefetch[1] as u32));
            self.prefetch(bus)?;
            let dn = i.reg[1] as usize;
            if i.size == Size::Long {
                let mut v = 0u32;
                for k in 0..4 {
                    if k > 0 { addr = addr.wrapping_add(2); }
                    let b = self.read_b(bus, addr) as u32;
                    v = if k == 0 { b << 24 } else { v | b << (24 - 8 * k) };
                    self.r[dn] = v;
                }
            } else {
                let hi = self.read_b(bus, addr) as u32;
                self.r[dn] = (self.r[dn] & 0xFFFF_0000) | hi << 8;
                addr = addr.wrapping_add(2);
                let lo = self.read_b(bus, addr) as u32;
                self.r[dn] |= lo;
            }
        }
        self.prefetch(bus)
    }

    fn movem_to_r(&mut self, bus: &mut dyn M68kBus, i: &Inst) -> R<()> {
        // (CLK treats both MOVEM sizes as word for the EA step: (An)+ moves by 2 here,
        // then the final address is written back)
        let w = Inst { size: Size::Word, ..*i };
        if matches!(i.m[1], Mode::Index | Mode::PcIndex) {
            self.calc_ea_index_n_np(bus, &w, 1)?;
        } else {
            self.calc_ea(bus, &w, 1)?;
        }
        let mut mask = self.operand[0] as u16;
        let mut idx = 0usize;
        while mask != 0 {
            while mask & 1 == 0 { mask >>= 1; idx += 1; }
            if i.size == Size::Long {
                let hi = self.read_w(bus, self.ea[1])? as u32;
                self.r[idx] = (self.r[idx] & 0xFFFF) | hi << 16;
                self.ea[1] = self.ea[1].wrapping_add(2);
                let lo = self.read_w(bus, self.ea[1])? as u32;
                self.r[idx] = (self.r[idx] & 0xFFFF_0000) | lo;
                self.ea[1] = self.ea[1].wrapping_add(2);
            } else {
                let v = self.read_w(bus, self.ea[1])? as u32;
                self.r[idx] = sext16(v);
                self.ea[1] = self.ea[1].wrapping_add(2);
            }
            mask >>= 1;
            idx += 1;
        }
        self.read_w(bus, self.ea[1])?;          // the extra read
        if i.m[1] == Mode::PostInc {
            self.r[8 + i.reg[1] as usize] = self.ea[1];
        }
        self.prefetch(bus)
    }

    fn movem_to_m(&mut self, bus: &mut dyn M68kBus, i: &Inst) -> R<()> {
        let mut mask = self.operand[0] as u16;
        let long = i.size == Size::Long;
        if i.m[1] == Mode::PreDec {
            self.ea[1] = self.r[8 + i.reg[1] as usize];
            let mut idx = 15usize;
            while mask != 0 {
                while mask & 1 == 0 { mask >>= 1; idx -= 1; }
                let v = self.r[idx];
                self.ea[1] = self.ea[1].wrapping_sub(2);
                self.write_w(bus, self.ea[1], v as u16)?;
                if long {
                    self.ea[1] = self.ea[1].wrapping_sub(2);
                    self.write_w(bus, self.ea[1], (v >> 16) as u16)?;
                }
                mask >>= 1;
                idx = idx.wrapping_sub(1);
            }
            self.r[8 + i.reg[1] as usize] = self.ea[1];
            return self.prefetch(bus);
        }
        let w = Inst { size: Size::Word, ..*i };
        if matches!(i.m[1], Mode::Index | Mode::PcIndex) {
            self.calc_ea_index_n_np(bus, &w, 1)?;
        } else {
            self.calc_ea(bus, &w, 1)?;
        }
        let mut idx = 0usize;
        while mask != 0 {
            while mask & 1 == 0 { mask >>= 1; idx += 1; }
            let v = self.r[idx];
            if long {
                self.write_w(bus, self.ea[1], (v >> 16) as u16)?;
                self.ea[1] = self.ea[1].wrapping_add(2);
            }
            self.write_w(bus, self.ea[1], v as u16)?;
            self.ea[1] = self.ea[1].wrapping_add(2);
            mask >>= 1;
            idx += 1;
        }
        self.prefetch(bus)
    }

    fn rts(&mut self, bus: &mut dyn M68kBus) -> R<()> {
        let hi = self.read_w(bus, self.r[15])? as u32;
        self.pc = (self.pc & 0xFFFF) | hi << 16;
        self.r[15] = self.r[15].wrapping_add(2);
        let lo = self.read_w(bus, self.r[15])? as u32;
        self.pc = hi << 16 | lo;
        self.r[15] = self.r[15].wrapping_add(2);
        self.prefetch(bus)?;
        self.prefetch(bus)
    }

    fn rte_rtr(&mut self, bus: &mut dyn M68kBus, rte: bool) -> R<()> {
        self.r[15] = self.r[15].wrapping_add(2);
        let hi = self.read_w(bus, self.r[15])? as u32;
        self.pc = (self.pc & 0xFFFF) | hi << 16;
        self.r[15] = self.r[15].wrapping_sub(2);
        let sr = self.read_w(bus, self.r[15])?;
        self.r[15] = self.r[15].wrapping_add(4);
        let lo = self.read_w(bus, self.r[15])? as u32;
        self.pc = hi << 16 | lo;
        self.r[15] = self.r[15].wrapping_add(2);
        if rte { self.set_sr(sr); } else { self.set_ccr(sr); }
        self.prefetch(bus)?;
        self.prefetch(bus)
    }

    fn link(&mut self, bus: &mut dyn M68kBus, i: &Inst) -> R<()> {
        self.prefetch(bus)?;
        let an = 8 + i.reg[0] as usize;
        let value = if an == 15 { self.r[15].wrapping_sub(4) } else { self.r[an] };
        self.push_l(bus, value)?;
        self.r[an] = self.r[15];
        self.r[15] = self.r[15].wrapping_add(sext16(self.prefetch[0] as u32));
        self.prefetch(bus)
    }

    fn unlk(&mut self, bus: &mut dyn M68kBus, i: &Inst) -> R<()> {
        let an = 8 + i.reg[0] as usize;
        self.r[15] = self.r[an];
        let v = self.pop_l(bus)?;
        self.r[an] = v;
        self.prefetch(bus)
    }

    // ================================================================ arithmetic and logic

    /// The data part of an instruction (CLK perform). Returns true if it raised a divide
    /// by zero.
    fn perform(&mut self, i: &Inst) -> bool {
        let size = i.size;
        let mask = size.mask();
        let msb = size.msb();
        let src = self.operand[0];
        let dst = self.operand[1];
        self.dynamic_length = 0;
        match i.op {
            Op::Add | Op::Sub | Op::Addx | Op::Subx => {
                let add = matches!(i.op, Op::Add | Op::Addx);
                let extend = matches!(i.op, Op::Addx | Op::Subx);
                let (s, d) = (src & mask, dst & mask);
                let mut r = if add { d.wrapping_add(s) } else { d.wrapping_sub(s) } & mask;
                let mut carry = if add { r < d } else { r > d };
                if extend && self.flag(SR_X) {
                    if add {
                        r = r.wrapping_add(1) & mask;
                        carry |= r == 0;
                    } else {
                        carry |= r == 0;
                        r = r.wrapping_sub(1) & mask;
                    }
                }
                self.set_flag(SR_C, carry);
                self.set_flag(SR_X, carry);
                if extend { if r != 0 { self.sr &= !SR_Z; } } else { self.set_flag(SR_Z, r == 0); }
                self.set_flag(SR_N, r & msb != 0);
                let v = if add { !(d ^ s) & (d ^ r) } else { (d ^ s) & (d ^ r) };
                self.set_flag(SR_V, v & msb != 0);
                self.operand[1] = (dst & !mask) | r;
            }
            Op::Adda => self.operand[1] = dst.wrapping_add(if size == Size::Word { sext16(src) } else { src }),
            Op::Suba => self.operand[1] = dst.wrapping_sub(if size == Size::Word { sext16(src) } else { src }),
            Op::Cmp | Op::Cmpa => {
                let (s, d, m, b) = if i.op == Op::Cmpa {
                    (if size == Size::Word { sext16(src) } else { src }, dst, 0xFFFF_FFFFu32, 0x8000_0000u32)
                } else {
                    (src & mask, dst & mask, mask, msb)
                };
                let r = d.wrapping_sub(s) & m;
                self.set_flag(SR_C, r > d);
                self.set_flag(SR_Z, r == 0);
                self.set_flag(SR_N, r & b != 0);
                self.set_flag(SR_V, (d ^ s) & (d ^ r) & b != 0);
            }
            Op::And | Op::Or | Op::Eor => {
                let r = match i.op { Op::And => dst & src, Op::Or => dst | src, _ => dst ^ src } & mask;
                self.sr &= !(SR_V | SR_C);
                self.set_nz(r, size);
                self.operand[1] = (dst & !mask) | r;
            }
            Op::Not => {
                let r = !src & mask;
                self.sr &= !(SR_V | SR_C);
                self.set_nz(r, size);
                self.operand[0] = (src & !mask) | r;
            }
            Op::Neg | Op::Negx => {
                let s = src & mask;
                let x = i.op == Op::Negx && self.flag(SR_X);
                let r = 0u32.wrapping_sub(s).wrapping_sub(x as u32) & mask;
                if i.op == Op::Negx { if r != 0 { self.sr &= !SR_Z; } } else { self.set_flag(SR_Z, r == 0); }
                // borrow out of 0 - s - x (for NEG: s != 0)
                let borrow = s != 0 || x;
                self.set_flag(SR_C, borrow);
                self.set_flag(SR_X, borrow);
                self.set_flag(SR_N, r & msb != 0);
                self.set_flag(SR_V, s & r & msb != 0);
                self.operand[0] = (src & !mask) | r;
            }
            Op::Clr => {
                self.operand[0] = src & !mask;
                self.sr = (self.sr & !(SR_N | SR_V | SR_C)) | SR_Z;
            }
            Op::Tst => {
                self.sr &= !(SR_V | SR_C);
                self.set_nz(src, size);
            }
            Op::Move => {
                self.operand[1] = src;
                self.sr &= !(SR_V | SR_C);
                self.set_nz(src, size);
            }
            Op::Movea => self.operand[1] = if size == Size::Word { sext16(src) } else { src },
            Op::Ext => {
                let r = if size == Size::Word { (src & 0xFFFF_0000) | (sext8(src) & 0xFFFF) } else { sext16(src) };
                self.sr &= !(SR_V | SR_C);
                self.set_nz(r, size);
                self.operand[0] = r;
            }
            Op::Swap => {
                let r = src.rotate_left(16);
                self.sr &= !(SR_V | SR_C);
                self.set_nz(r, Size::Long);
                self.operand[0] = r;
            }
            Op::Exg => self.operand.swap(0, 1),
            Op::Btst | Op::Bchg | Op::Bclr | Op::Bset => {
                let bit = src & if i.m[1] == Mode::Dn { 31 } else { 7 };
                let b = 1u32 << bit;
                self.set_flag(SR_Z, dst & b == 0);
                self.operand[1] = match i.op { Op::Bchg => dst ^ b, Op::Bclr => dst & !b, Op::Bset => dst | b, _ => dst };
                self.dynamic_length = (bit > 15) as u32;
            }
            Op::Scc => {
                let c = self.condition(i.cond);
                self.operand[0] = (src & !0xFF) | if c { 0xFF } else { 0 };
            }
            Op::MoveFromSr => self.operand[0] = (src & 0xFFFF_0000) | self.sr as u32,
            Op::MoveToSr => self.set_sr(src as u16),
            Op::MoveToCcr => self.set_ccr(src as u16),
            Op::MoveToUsp => self.other_sp = src,
            Op::MoveFromUsp => self.operand[0] = self.other_sp,
            Op::AndiSr => self.set_sr(self.sr & src as u16),
            Op::OriSr => self.set_sr(self.sr | src as u16),
            Op::EoriSr => self.set_sr(self.sr ^ src as u16),
            Op::AndiCcr => self.set_ccr(self.sr & src as u16),
            Op::OriCcr => self.set_ccr(self.sr | src as u16),
            Op::EoriCcr => self.set_ccr(self.sr ^ src as u16),
            Op::Mulu | Op::Muls => {
                let s = src as u16;
                let r = if i.op == Op::Mulu {
                    (s as u32) * (dst as u16 as u32)
                } else {
                    ((s as i16 as i32) * (dst as u16 as i16 as i32)) as u32
                };
                self.sr &= !(SR_V | SR_C);
                self.set_nz(r, Size::Long);
                self.operand[1] = r;
                self.dynamic_length = 17 + if i.op == Op::Mulu {
                    s.count_ones()
                } else {
                    (s ^ (s << 1)).count_ones()
                };
            }
            Op::Divu | Op::Divs => return self.divide(i.op == Op::Divu),
            Op::Abcd => {
                let (s, d) = (src & 0xFF, dst & 0xFF);
                let x = self.flag(SR_X) as i32;
                let unadjusted = d as i32 + s as i32 + x;
                let mut r = (d as i32 & 0xF) + (s as i32 & 0xF) + x;
                r += (d as i32 & 0xF0) + (s as i32 & 0xF0) + (((9 - r) >> 4) & 0x06);
                r += ((0x9F - r) >> 4) & 0x60;
                if r & 0xFF != 0 { self.sr &= !SR_Z; }
                self.set_flag(SR_C, r & !0xFF != 0);
                self.set_flag(SR_X, r & !0xFF != 0);
                self.set_flag(SR_N, r & 0x80 != 0);
                self.set_flag(SR_V, !unadjusted & r & 0x80 != 0);
                self.operand[1] = (dst & !0xFF) | (r as u32 & 0xFF);
            }
            Op::Sbcd => {
                let r = self.sbcd((src & 0xFF) as i32, (dst & 0xFF) as i32);
                self.operand[1] = (dst & !0xFF) | r;
            }
            Op::Nbcd => {
                let r = self.sbcd((src & 0xFF) as i32, 0);
                self.operand[0] = (src & !0xFF) | r;
            }
            Op::ShiftM(kind, left) => {
                let v = src & 0xFFFF;
                let r = match (kind, left) {
                    (Shift::As, true) => {
                        self.set_flag(SR_C, v & 0x8000 != 0); self.set_flag(SR_X, v & 0x8000 != 0);
                        self.set_flag(SR_V, (v ^ (v << 1)) & 0x8000 != 0);
                        (v << 1) & 0xFFFF
                    }
                    (Shift::Ls, true) => {
                        self.set_flag(SR_C, v & 0x8000 != 0); self.set_flag(SR_X, v & 0x8000 != 0);
                        self.sr &= !SR_V;
                        (v << 1) & 0xFFFF
                    }
                    (Shift::As, false) => {
                        self.set_flag(SR_C, v & 1 != 0); self.set_flag(SR_X, v & 1 != 0);
                        self.sr &= !SR_V;
                        (v & 0x8000) | (v >> 1)
                    }
                    (Shift::Ls, false) => {
                        self.set_flag(SR_C, v & 1 != 0); self.set_flag(SR_X, v & 1 != 0);
                        self.sr &= !SR_V;
                        v >> 1
                    }
                    (Shift::Ro, true) => {
                        let r = ((v << 1) | (v >> 15)) & 0xFFFF;
                        self.set_flag(SR_C, r & 1 != 0);
                        self.sr &= !SR_V;
                        r
                    }
                    (Shift::Ro, false) => {
                        let r = ((v >> 1) | (v << 15)) & 0xFFFF;
                        self.set_flag(SR_C, r & 0x8000 != 0);
                        self.sr &= !SR_V;
                        r
                    }
                    (Shift::Rox, true) => {
                        let c = v & 0x8000 != 0;
                        let r = ((v << 1) | self.flag(SR_X) as u32) & 0xFFFF;
                        self.set_flag(SR_C, c); self.set_flag(SR_X, c);
                        self.sr &= !SR_V;
                        r
                    }
                    (Shift::Rox, false) => {
                        let c = v & 1 != 0;
                        let r = (v >> 1) | if self.flag(SR_X) { 0x8000 } else { 0 };
                        self.set_flag(SR_C, c); self.set_flag(SR_X, c);
                        self.sr &= !SR_V;
                        r
                    }
                };
                self.set_nz(r, Size::Word);
                self.operand[0] = (src & 0xFFFF_0000) | r;
            }
            Op::ShiftR(kind, left) => {
                let count = src & 63;
                self.dynamic_length = count + if size == Size::Long { 2 } else { 1 };
                let r = match kind {
                    Shift::As | Shift::Ls => self.shift(kind, left, count, dst & mask, size),
                    Shift::Ro => self.rotate(left, count, dst & mask, size),
                    Shift::Rox => self.rox(left, count, dst & mask, size),
                };
                self.set_nz(r, size);
                self.operand[1] = (dst & !mask) | r;
            }
            Op::Tas => {
                // register form (memory TAS has its own bus sequence)
                let reg = i.reg[0] as usize;
                let v = self.r[reg] as u8;
                self.r[reg] |= 0x80;
                self.sr &= !(SR_V | SR_C);
                self.set_nz(v as u32, Size::Byte);
            }
            _ => {}
        }
        false
    }

    fn sbcd(&mut self, rhs: i32, lhs: i32) -> u32 {
        let x = self.flag(SR_X) as i32;
        let unadjusted = lhs - rhs - x;
        let top = (lhs & 0xF0) - (rhs & 0xF0) - (0x60 & (unadjusted >> 4));
        let mut r = (lhs & 0xF) - (rhs & 0xF) - x;
        let low_adj = 0x06 & (r >> 4);
        let c = (unadjusted - low_adj) & 0x300 != 0;
        self.set_flag(SR_C, c);
        self.set_flag(SR_X, c);
        r = r + top - low_adj;
        let d = (r as u32) & 0xFF;
        if d != 0 { self.sr &= !SR_Z; }
        self.set_flag(SR_N, d & 0x80 != 0);
        self.set_flag(SR_V, unadjusted & !r & 0x80 != 0);
        d
    }

    fn shift(&mut self, kind: Shift, left: bool, count: u32, d: u32, size: Size) -> u32 {
        let bits = size.bits();
        let msb = size.msb();
        let mask = size.mask();
        if count == 0 {
            self.sr &= !(SR_C | SR_V);
            return d;
        }
        if left {
            let c = if count > bits { false } else { (d << (count - 1)) & msb != 0 };
            self.set_flag(SR_C, c);
            self.set_flag(SR_X, c);
            if kind == Shift::Ls {
                self.sr &= !SR_V;
            } else if count >= bits {
                self.set_flag(SR_V, d != 0);
            } else {
                let affected = !((msb >> count).wrapping_sub(1)) & mask;
                let a = d & affected;
                self.set_flag(SR_V, a != 0 && a != affected);
            }
            if count >= bits { 0 } else { (d << count) & mask }
        } else {
            let neg = d & msb != 0;
            let c = if count > bits { kind == Shift::As && neg } else { (d >> (count - 1)) & 1 != 0 };
            self.set_flag(SR_C, c);
            self.set_flag(SR_X, c);
            self.sr &= !SR_V;
            let sign = if kind == Shift::As && neg { mask } else { 0 };
            if count >= bits { sign } else { ((d >> count) | (sign << (bits - count))) & mask }
        }
    }

    fn rotate(&mut self, left: bool, count: u32, d: u32, size: Size) -> u32 {
        let bits = size.bits();
        self.sr &= !SR_V;
        if count == 0 {
            self.sr &= !SR_C;
            return d;
        }
        let n = count & (bits - 1);
        let mask = size.mask();
        let r = if n == 0 { d } else if left {
            ((d << n) | (d >> (bits - n))) & mask
        } else {
            ((d >> n) | (d << (bits - n))) & mask
        };
        let c = if left { r & 1 != 0 } else { r & size.msb() != 0 };
        self.set_flag(SR_C, c);
        r
    }

    fn rox(&mut self, left: bool, count: u32, d: u32, size: Size) -> u32 {
        let bits = size.bits();
        let mask = size.mask();
        let msb = size.msb();
        let n = count % (bits + 1);
        self.sr &= !SR_V;
        let x = self.flag(SR_X);
        if n == 0 {
            self.set_flag(SR_C, x);
            return d;
        }
        // rotate through a (bits+1)-bit value with X on top
        let wide = (d as u64) | ((x as u64) << bits);
        let w = bits as u64 + 1;
        let full = (1u64 << w) - 1;
        let n = n as u64;
        let rot = if left {
            ((wide << n) | (wide >> (w - n))) & full
        } else {
            ((wide >> n) | (wide << (w - n))) & full
        };
        let r = (rot as u32) & mask;
        let newx = (rot >> bits) & 1 != 0;
        let _ = msb;
        self.set_flag(SR_C, newx);
        self.set_flag(SR_X, newx);
        r
    }

    fn divide(&mut self, unsigned: bool) -> bool {
        let src = self.operand[0] as u16;
        let dst = self.operand[1];
        self.sr &= !SR_C;
        if src == 0 {
            self.sr &= !(SR_N | SR_V | SR_Z);
            self.dynamic_length = 4;
            return true;
        }
        if unsigned {
            let divisor = src as u32;
            let q = dst / divisor;
            if q > 0xFFFF {
                self.sr |= SR_V;
                self.dynamic_length = 3;
                return false;
            }
            let rem = dst % divisor;
            self.operand[1] = rem << 16 | q;
            self.sr &= !SR_V;
            self.set_flag(SR_Z, q == 0);
            self.set_flag(SR_N, q & 0x8000 != 0);
            // timing: CLK did_divu (yacht.txt flowchart)
            let mut len = 3 + 30 + 3;
            let mut dividend = dst;
            let div = divisor << 16;
            for _ in 0..15 {
                if dividend & 0x8000_0000 != 0 {
                    dividend = (dividend << 1).wrapping_sub(div);
                } else {
                    dividend <<= 1;
                    if dividend >= div { dividend -= div; len += 1; } else { len += 2; }
                }
            }
            self.dynamic_length = len;
        } else {
            let dividend = dst as i32;
            let divisor = src as i16 as i32;
            self.dynamic_length = 6 + (dividend < 0) as u32;
            let q = dividend as i64 / divisor as i64;
            if q != q as i16 as i64 {
                self.sr |= SR_V;
                return false;
            }
            let rem = (dividend as i64 % divisor as i64) as i16 as u16 as u32;
            self.operand[1] = rem << 16 | (q as u16 as u32);
            self.sr &= !SR_V;
            self.set_flag(SR_Z, q as u16 == 0);
            self.set_flag(SR_N, q as u16 & 0x8000 != 0);
            let mut len = self.dynamic_length + 49;
            let bits = !((q as i32).unsigned_abs() as u16) & 0xFFFE;
            len += bits.count_ones();
            len += if divisor < 0 { 4 } else if dividend < 0 { 5 } else { 3 };
            self.dynamic_length = len;
        }
        false
    }
}
