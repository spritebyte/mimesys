//! 68000 opcode decoding into an operation, a size and up to two operands, with the
//! operand order Clock Signal uses (operand 0 is fetched first). Invalid encodings
//! decode to `Op::Undefined` (illegal instruction / line A / line F).

use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Size { Byte, Word, Long }

impl Size {
    pub fn mask(self) -> u32 { match self { Size::Byte => 0xFF, Size::Word => 0xFFFF, Size::Long => 0xFFFF_FFFF } }
    pub fn msb(self) -> u32 { match self { Size::Byte => 0x80, Size::Word => 0x8000, Size::Long => 0x8000_0000 } }
    pub fn bits(self) -> u32 { match self { Size::Byte => 8, Size::Word => 16, Size::Long => 32 } }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    None,
    Dn, An, Ind, PostInc, PreDec, Disp, Index,
    AbsW, AbsL, PcDisp, PcIndex, Imm,
    /// a value from the opcode (ADDQ / shift count / MOVEQ / TRAP / Bcc.b)
    Quick,
    /// a word from the instruction stream that isn't immediate data (MOVEM mask)
    ExtWord,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shift { As, Ls, Rox, Ro }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    Undefined,
    Abcd, Sbcd, Nbcd,
    Add, Adda, Addx, Sub, Suba, Subx,
    And, Or, Eor, Cmp, Cmpa,
    Btst, Bchg, Bclr, Bset,
    Bcc, Bsr, Dbcc, Scc,
    Clr, Neg, Negx, Not, Tst,
    Move, Movea,
    Lea, Pea, Jmp, Jsr,
    MoveToSr, MoveFromSr, MoveToCcr, MoveToUsp, MoveFromUsp,
    AndiSr, OriSr, EoriSr, AndiCcr, OriCcr, EoriCcr,
    Mulu, Muls, Divu, Divs, Chk,
    Ext, Swap, Exg, Link, Unlk, Tas,
    Trap, Trapv, Rte, Rts, Rtr, Reset, Nop, Stop,
    MovemToR, MovemToM, MovepToR, MovepToM,
    /// register shift / rotate: operand 0 = count (Quick or Dn), operand 1 = Dn;
    /// left = true for ASL/LSL/ROXL/ROL
    ShiftR(Shift, bool),
    /// memory shift by one (word): operand 0 = ea
    ShiftM(Shift, bool),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Inst {
    pub op: Op,
    pub size: Size,
    pub m: [Mode; 2],
    pub reg: [u8; 2],
    pub cond: u8,
}

impl Inst {
    const UNDEFINED: Inst = Inst { op: Op::Undefined, size: Size::Word, m: [Mode::None; 2], reg: [0; 2], cond: 0 };

    /// Linear register index (D0-D7 = 0-7, A0-A7 = 8-15) for operand `i`.
    pub fn lreg(&self, i: usize) -> usize {
        self.reg[i] as usize + if self.m[i] == Mode::An { 8 } else { 0 }
    }
}

/// The decoded instruction for every opcode.
pub fn decode(op: u16) -> Inst {
    static TABLE: OnceLock<Vec<Inst>> = OnceLock::new();
    TABLE.get_or_init(|| (0..=0xFFFFu32).map(|o| decode_one(o as u16)).collect())[op as usize]
}

/// EA mode from the mode/register fields.
fn ea(mode: u16, reg: u16) -> Mode {
    match mode {
        0 => Mode::Dn, 1 => Mode::An, 2 => Mode::Ind, 3 => Mode::PostInc, 4 => Mode::PreDec,
        5 => Mode::Disp, 6 => Mode::Index,
        _ => match reg { 0 => Mode::AbsW, 1 => Mode::AbsL, 2 => Mode::PcDisp, 3 => Mode::PcIndex, 4 => Mode::Imm, _ => Mode::None },
    }
}

fn is_data(m: Mode) -> bool { !matches!(m, Mode::An | Mode::None) }
fn is_memory(m: Mode) -> bool { !matches!(m, Mode::Dn | Mode::An | Mode::None) }
fn is_alterable(m: Mode) -> bool { matches!(m, Mode::Dn | Mode::An | Mode::Ind | Mode::PostInc | Mode::PreDec | Mode::Disp | Mode::Index | Mode::AbsW | Mode::AbsL) }
fn is_data_alterable(m: Mode) -> bool { is_alterable(m) && m != Mode::An }
fn is_mem_alterable(m: Mode) -> bool { is_alterable(m) && is_memory(m) }
fn is_control(m: Mode) -> bool { matches!(m, Mode::Ind | Mode::Disp | Mode::Index | Mode::AbsW | Mode::AbsL | Mode::PcDisp | Mode::PcIndex) }
fn is_any(m: Mode) -> bool { m != Mode::None }

fn size2(bits: u16) -> Option<Size> {
    match bits { 0 => Some(Size::Byte), 1 => Some(Size::Word), 2 => Some(Size::Long), _ => None }
}

fn inst(op: Op, size: Size, m0: Mode, r0: u16, m1: Mode, r1: u16) -> Inst {
    Inst { op, size, m: [m0, m1], reg: [r0 as u8, r1 as u8], cond: 0 }
}
fn one(op: Op, size: Size, m0: Mode, r0: u16) -> Inst { inst(op, size, m0, r0, Mode::None, 0) }
fn none(op: Op, size: Size) -> Inst { inst(op, size, Mode::None, 0, Mode::None, 0) }

fn decode_one(op: u16) -> Inst {
    let ea_reg = op & 7;
    let ea_mode = (op >> 3) & 7;
    let data_reg = (op >> 9) & 7;
    let opmode = (op >> 6) & 7;
    let m = ea(ea_mode, ea_reg);
    let u = Inst::UNDEFINED;

    match op >> 12 {
        0x0 => {
            match op {
                0x003C => return one(Op::OriCcr, Size::Byte, Mode::Imm, 0),
                0x007C => return one(Op::OriSr, Size::Word, Mode::Imm, 0),
                0x023C => return one(Op::AndiCcr, Size::Byte, Mode::Imm, 0),
                0x027C => return one(Op::AndiSr, Size::Word, Mode::Imm, 0),
                0x0A3C => return one(Op::EoriCcr, Size::Byte, Mode::Imm, 0),
                0x0A7C => return one(Op::EoriSr, Size::Word, Mode::Imm, 0),
                _ => {}
            }
            if op & 0x0100 != 0 {
                if ea_mode == 1 {
                    // MOVEP
                    return match opmode {
                        4 => inst(Op::MovepToR, Size::Word, Mode::Disp, ea_reg, Mode::Dn, data_reg),
                        5 => inst(Op::MovepToR, Size::Long, Mode::Disp, ea_reg, Mode::Dn, data_reg),
                        6 => inst(Op::MovepToM, Size::Word, Mode::Dn, data_reg, Mode::Disp, ea_reg),
                        _ => inst(Op::MovepToM, Size::Long, Mode::Dn, data_reg, Mode::Disp, ea_reg),
                    };
                }
                let bop = [Op::Btst, Op::Bchg, Op::Bclr, Op::Bset][((op >> 6) & 3) as usize];
                let ok = if bop == Op::Btst { is_data(m) } else { is_data_alterable(m) };
                return if ok { inst(bop, Size::Byte, Mode::Dn, data_reg, m, ea_reg) } else { u };
            }
            match data_reg {
                4 => {
                    let bop = [Op::Btst, Op::Bchg, Op::Bclr, Op::Bset][((op >> 6) & 3) as usize];
                    let ok = if bop == Op::Btst { is_data(m) && m != Mode::Imm } else { is_data_alterable(m) };
                    if ok { inst(bop, Size::Byte, Mode::Imm, 0, m, ea_reg) } else { u }
                }
                0 | 1 | 2 | 3 | 5 | 6 => {
                    let Some(size) = size2((op >> 6) & 3) else { return u };
                    if !is_data_alterable(m) { return u; }
                    let o = match data_reg { 0 => Op::Or, 1 => Op::And, 2 => Op::Sub, 3 => Op::Add, 5 => Op::Eor, _ => Op::Cmp };
                    inst(o, size, Mode::Imm, 0, m, ea_reg)
                }
                _ => u,
            }
        }

        0x1 | 0x2 | 0x3 => {
            let size = match op >> 12 { 1 => Size::Byte, 3 => Size::Word, _ => Size::Long };
            let dst = ea(opmode, data_reg);
            if !is_any(m) || (size == Size::Byte && m == Mode::An) { return u; }
            if dst == Mode::An {
                if size == Size::Byte { return u; }
                return inst(Op::Movea, size, m, ea_reg, Mode::An, data_reg);
            }
            if !is_data_alterable(dst) { return u; }
            inst(Op::Move, size, m, ea_reg, dst, data_reg)
        }

        0x4 => decode4(op, m, ea_reg, ea_mode, data_reg),

        0x5 => {
            let cond = ((op >> 8) & 15) as u8;
            if (op >> 6) & 3 == 3 {
                if ea_mode == 1 {
                    let mut i = inst(Op::Dbcc, Size::Word, Mode::Dn, ea_reg, Mode::Imm, 0);
                    i.cond = cond;
                    return i;
                }
                if !is_data_alterable(m) { return u; }
                let mut i = one(Op::Scc, Size::Byte, m, ea_reg);
                i.cond = cond;
                return i;
            }
            let size = size2((op >> 6) & 3).unwrap();
            if !is_alterable(m) || (size == Size::Byte && m == Mode::An) { return u; }
            let sub = op & 0x0100 != 0;
            let o = match (sub, m == Mode::An) {
                (false, false) => Op::Add, (true, false) => Op::Sub,
                (false, true) => Op::Adda, (true, true) => Op::Suba,
            };
            inst(o, size, Mode::Quick, 0, m, ea_reg)
        }

        0x6 => {
            let cond = ((op >> 8) & 15) as u8;
            let word = op & 0xFF == 0;
            let mut i = if cond == 1 {
                one(Op::Bsr, if word { Size::Word } else { Size::Byte }, if word { Mode::Imm } else { Mode::Quick }, 0)
            } else {
                one(Op::Bcc, if word { Size::Word } else { Size::Byte }, if word { Mode::Imm } else { Mode::Quick }, 0)
            };
            i.cond = cond;
            i
        }

        0x7 => if op & 0x0100 == 0 { inst(Op::Move, Size::Long, Mode::Quick, 0, Mode::Dn, data_reg) } else { u },

        0x8 => match opmode {
            3 => if is_data(m) { inst(Op::Divu, Size::Word, m, ea_reg, Mode::Dn, data_reg) } else { u },
            7 => if is_data(m) { inst(Op::Divs, Size::Word, m, ea_reg, Mode::Dn, data_reg) } else { u },
            _ if op & 0x1F0 == 0x100 => bcd(Op::Sbcd, op, ea_reg, data_reg),
            0..=2 => if is_data(m) { inst(Op::Or, size2(opmode).unwrap(), m, ea_reg, Mode::Dn, data_reg) } else { u },
            _ => if is_mem_alterable(m) { inst(Op::Or, size2(opmode - 4).unwrap(), Mode::Dn, data_reg, m, ea_reg) } else { u },
        },

        0x9 | 0xD => {
            let add = op >> 12 == 0xD;
            match opmode {
                3 | 7 => {
                    if !is_any(m) { return u; }
                    let size = if opmode == 3 { Size::Word } else { Size::Long };
                    inst(if add { Op::Adda } else { Op::Suba }, size, m, ea_reg, Mode::An, data_reg)
                }
                _ if op & 0x130 == 0x100 => {
                    let size = size2(opmode - 4).unwrap();
                    let am = if op & 8 != 0 { Mode::PreDec } else { Mode::Dn };
                    inst(if add { Op::Addx } else { Op::Subx }, size, am, ea_reg, am, data_reg)
                }
                0..=2 => {
                    let size = size2(opmode).unwrap();
                    if !is_any(m) || (size == Size::Byte && m == Mode::An) { return u; }
                    inst(if add { Op::Add } else { Op::Sub }, size, m, ea_reg, Mode::Dn, data_reg)
                }
                _ => {
                    if !is_mem_alterable(m) { return u; }
                    inst(if add { Op::Add } else { Op::Sub }, size2(opmode - 4).unwrap(), Mode::Dn, data_reg, m, ea_reg)
                }
            }
        }

        0xA | 0xF => u,

        0xB => match opmode {
            3 | 7 => {
                if !is_any(m) { return u; }
                inst(Op::Cmpa, if opmode == 3 { Size::Word } else { Size::Long }, m, ea_reg, Mode::An, data_reg)
            }
            0..=2 => {
                let size = size2(opmode).unwrap();
                if !is_any(m) || (size == Size::Byte && m == Mode::An) { return u; }
                inst(Op::Cmp, size, m, ea_reg, Mode::Dn, data_reg)
            }
            _ => {
                let size = size2(opmode - 4).unwrap();
                if ea_mode == 1 {
                    return inst(Op::Cmp, size, Mode::PostInc, ea_reg, Mode::PostInc, data_reg);   // CMPM
                }
                if !is_data_alterable(m) { return u; }
                inst(Op::Eor, size, Mode::Dn, data_reg, m, ea_reg)
            }
        },

        0xC => match opmode {
            3 => if is_data(m) { inst(Op::Mulu, Size::Word, m, ea_reg, Mode::Dn, data_reg) } else { u },
            7 => if is_data(m) { inst(Op::Muls, Size::Word, m, ea_reg, Mode::Dn, data_reg) } else { u },
            _ if op & 0x1F0 == 0x100 => bcd(Op::Abcd, op, ea_reg, data_reg),
            _ if op & 0x1F8 == 0x140 => inst(Op::Exg, Size::Long, Mode::Dn, data_reg, Mode::Dn, ea_reg),
            _ if op & 0x1F8 == 0x148 => inst(Op::Exg, Size::Long, Mode::An, data_reg, Mode::An, ea_reg),
            _ if op & 0x1F8 == 0x188 => inst(Op::Exg, Size::Long, Mode::Dn, data_reg, Mode::An, ea_reg),
            0..=2 => if is_data(m) { inst(Op::And, size2(opmode).unwrap(), m, ea_reg, Mode::Dn, data_reg) } else { u },
            4..=6 => if is_mem_alterable(m) { inst(Op::And, size2(opmode - 4).unwrap(), Mode::Dn, data_reg, m, ea_reg) } else { u },
            _ => u,
        },

        _ => {
            // 0xE: shifts and rotates
            if (op >> 6) & 3 == 3 {
                if op & 0x0800 != 0 || !is_mem_alterable(m) { return u; }
                let kind = [Shift::As, Shift::Ls, Shift::Rox, Shift::Ro][((op >> 9) & 3) as usize];
                return one(Op::ShiftM(kind, op & 0x0100 != 0), Size::Word, m, ea_reg);
            }
            let size = size2((op >> 6) & 3).unwrap();
            let kind = [Shift::As, Shift::Ls, Shift::Rox, Shift::Ro][((op >> 3) & 3) as usize];
            let count = if op & 0x20 != 0 { Mode::Dn } else { Mode::Quick };
            inst(Op::ShiftR(kind, op & 0x0100 != 0), size, count, data_reg, Mode::Dn, ea_reg)
        }
    }
}

fn bcd(o: Op, op: u16, ea_reg: u16, data_reg: u16) -> Inst {
    let am = if op & 8 != 0 { Mode::PreDec } else { Mode::Dn };
    inst(o, Size::Byte, am, ea_reg, am, data_reg)
}

fn decode4(op: u16, m: Mode, ea_reg: u16, ea_mode: u16, data_reg: u16) -> Inst {
    let u = Inst::UNDEFINED;
    if op == 0x4AFC { return u; }   // ILLEGAL
    if op & 0x01C0 == 0x01C0 {
        return if is_control(m) { inst(Op::Lea, Size::Long, m, ea_reg, Mode::An, data_reg) } else { u };
    }
    if op & 0x01C0 == 0x0180 {
        return if is_data(m) { inst(Op::Chk, Size::Word, m, ea_reg, Mode::Dn, data_reg) } else { u };
    }
    let size_bits = (op >> 6) & 3;
    match (op >> 8) & 0xF {
        0x0 | 0x2 | 0x4 | 0x6 => {
            let hi = (op >> 8) & 0xF;
            if size_bits == 3 {
                return match hi {
                    0x0 => if is_data_alterable(m) { one(Op::MoveFromSr, Size::Word, m, ea_reg) } else { u },
                    0x4 => if is_data(m) { one(Op::MoveToCcr, Size::Word, m, ea_reg) } else { u },
                    0x6 => if is_data(m) { one(Op::MoveToSr, Size::Word, m, ea_reg) } else { u },
                    _ => u,
                };
            }
            if !is_data_alterable(m) { return u; }
            let o = match hi { 0x0 => Op::Negx, 0x2 => Op::Clr, 0x4 => Op::Neg, _ => Op::Not };
            one(o, size2(size_bits).unwrap(), m, ea_reg)
        }
        0x8 => match size_bits {
            0 => if is_data_alterable(m) { one(Op::Nbcd, Size::Byte, m, ea_reg) } else { u },
            1 => {
                if ea_mode == 0 { return one(Op::Swap, Size::Long, Mode::Dn, ea_reg); }
                if is_control(m) { one(Op::Pea, Size::Long, m, ea_reg) } else { u }
            }
            _ => {
                if ea_mode == 0 {
                    return one(Op::Ext, if size_bits == 2 { Size::Word } else { Size::Long }, Mode::Dn, ea_reg);
                }
                let ok = matches!(m, Mode::Ind | Mode::PreDec | Mode::Disp | Mode::Index | Mode::AbsW | Mode::AbsL);
                if ok { inst(Op::MovemToM, if size_bits == 2 { Size::Word } else { Size::Long }, Mode::ExtWord, 0, m, ea_reg) } else { u }
            }
        },
        0xA => {
            if size_bits == 3 {
                return if is_data_alterable(m) { one(Op::Tas, Size::Byte, m, ea_reg) } else { u };
            }
            if !is_data_alterable(m) { return u; }
            one(Op::Tst, size2(size_bits).unwrap(), m, ea_reg)
        }
        0xC => {
            if size_bits < 2 { return u; }
            let ok = matches!(m, Mode::Ind | Mode::PostInc | Mode::Disp | Mode::Index | Mode::AbsW | Mode::AbsL | Mode::PcDisp | Mode::PcIndex);
            if ok { inst(Op::MovemToR, if size_bits == 2 { Size::Word } else { Size::Long }, Mode::ExtWord, 0, m, ea_reg) } else { u }
        }
        0xE => {
            match op & 0xFFF8 {
                0x4E40 | 0x4E48 => return one(Op::Trap, Size::Byte, Mode::Quick, 0),
                0x4E50 => return inst(Op::Link, Size::Word, Mode::An, ea_reg, Mode::Imm, 0),
                0x4E58 => return one(Op::Unlk, Size::Long, Mode::An, ea_reg),
                0x4E60 => return one(Op::MoveToUsp, Size::Long, Mode::An, ea_reg),
                0x4E68 => return one(Op::MoveFromUsp, Size::Long, Mode::An, ea_reg),
                _ => {}
            }
            match op {
                0x4E70 => return none(Op::Reset, Size::Byte),
                0x4E71 => return none(Op::Nop, Size::Byte),
                0x4E72 => return one(Op::Stop, Size::Word, Mode::Imm, 0),
                0x4E73 => return none(Op::Rte, Size::Byte),
                0x4E75 => return none(Op::Rts, Size::Long),
                0x4E76 => return none(Op::Trapv, Size::Byte),
                0x4E77 => return none(Op::Rtr, Size::Byte),
                _ => {}
            }
            match size_bits {
                2 => if is_control(m) { one(Op::Jsr, Size::Long, m, ea_reg) } else { u },
                3 => if is_control(m) { one(Op::Jmp, Size::Long, m, ea_reg) } else { u },
                _ => u,
            }
        }
        _ => u,
    }
}
