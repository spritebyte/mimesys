//! DEC T-11 (DC310): a single-chip PDP-11 used as the main CPU of Atari System 2
//! (Paperboy, 720°, Super Sprint, APB).
//!
//! Port of MAME's t11 core (Aaron Giles), T-11 instruction set only:
//! the basic PDP-11 set plus SOB, SXT, XOR, RTT, MFPT, MFPS/MTPS
//! (no MARK, no MUL/DIV/ASH/ASHC, no floating point).
//! Cycle counts are MAME's (clock cycles), so 10 MHz on System 2.
//! Bus quirks kept: word accesses ignore address bit 0, and MOV/MOVB/CLR/CLRB/SXT
//! to memory read the destination before writing it, like the real chip.
//! Verified instruction by instruction against MAME's core: 300,000 random
//! instructions (tools/t11_ref/make_vectors.sh; a 3,000-vector sample runs in the tests).
//! One deliberate difference: for byte instructions with an immediate source
//! (CMPB #x, ...) MAME uses the whole immediate word; the real chip uses the low byte.

use serde::{Deserialize, Serialize};

pub trait T11Bus {
    /// `addr` is always even.
    fn read_word(&mut self, addr: u16) -> u16;
    fn write_word(&mut self, addr: u16, val: u16);
    fn read_byte(&mut self, addr: u16) -> u8;
    fn write_byte(&mut self, addr: u16, val: u8);
}

const C: u8 = 1;
const V: u8 = 2;
const Z: u8 = 4;
const N: u8 = 8;

const VEC_TIMEOUT: u16 = 0o004;
const VEC_ILLINST: u16 = 0o010;
const VEC_BPT: u16 = 0o014;
const VEC_IOT: u16 = 0o020;
const VEC_EMT: u16 = 0o030;
const VEC_TRAP: u16 = 0o034;

/// (priority, vector) for each value of the 4 CP lines (non-vectored interrupts).
const IRQ_TABLE: [(u8, u16); 16] = [
    (0 << 5, 0o000), (4 << 5, 0o070), (4 << 5, 0o064), (4 << 5, 0o060),
    (5 << 5, 0o134), (5 << 5, 0o130), (5 << 5, 0o124), (5 << 5, 0o120),
    (6 << 5, 0o114), (6 << 5, 0o110), (6 << 5, 0o104), (6 << 5, 0o100),
    (7 << 5, 0o154), (7 << 5, 0o150), (7 << 5, 0o144), (7 << 5, 0o140),
];

/// Start addresses selected by the top 3 bits of the mode word.
const INITIAL_PC: [u16; 8] = [0xC000, 0x8000, 0x4000, 0x2000, 0x1000, 0x0000, 0xF600, 0xF400];

// Cycle tables by addressing mode (rg, rgd, in, ind, de, ded, ix, ixd).
const SRC_CYC: [u32; 8] = [9, 15, 15, 21, 18, 24, 24, 30];
const DST_RMW: [u32; 8] = [3, 12, 12, 18, 15, 21, 21, 27];
const DST_RD: [u32; 8] = [3, 9, 9, 15, 12, 18, 18, 24];
const ONE_RMW: [u32; 8] = [12, 21, 21, 27, 24, 30, 30, 36];
const ONE_RD: [u32; 8] = [12, 18, 18, 24, 21, 27, 27, 33];
const MTPS_CYC: [u32; 8] = [24, 30, 30, 36, 33, 39, 39, 45];
const JMP_CYC: [u32; 8] = [0, 15, 18, 18, 18, 21, 21, 27];
const JSR_CYC: [u32; 8] = [0, 27, 30, 30, 30, 33, 33, 39];

#[derive(Clone, Copy)]
enum Loc { Reg(usize), Mem(u16) }

#[derive(Clone, Serialize, Deserialize)]
pub struct T11 {
    pub reg: [u16; 8],
    pub psw: u8,
    initial_pc: u16,
    pub waiting: bool,
    /// The 4 CP interrupt lines (bit n = CPn asserted). Set by the board.
    pub cp_state: u8,
    pub halt_req: bool,
}

impl T11 {
    /// `mode` = the mode word the board presents at reset (System 2: 0x36FF).
    pub fn new(mode: u16) -> Self {
        let mut cpu = Self { reg: [0; 8], psw: 0, initial_pc: INITIAL_PC[(mode >> 13) as usize],
                             waiting: false, cp_state: 0, halt_req: false };
        cpu.reset();
        cpu
    }

    pub fn reset(&mut self) {
        self.reg[6] = 0o376;
        self.reg[7] = self.initial_pc;
        self.psw = 0o340;
        self.waiting = false;
        self.halt_req = false;
    }

    pub fn pc(&self) -> u16 { self.reg[7] }

    // ---- bus helpers ----
    fn rword(&mut self, bus: &mut dyn T11Bus, addr: u16) -> u16 { bus.read_word(addr & 0xFFFE) }
    fn wword(&mut self, bus: &mut dyn T11Bus, addr: u16, v: u16) { bus.write_word(addr & 0xFFFE, v) }
    fn fetch(&mut self, bus: &mut dyn T11Bus) -> u16 {
        let v = bus.read_word(self.reg[7] & 0xFFFE);
        self.reg[7] = self.reg[7].wrapping_add(2);
        v
    }
    fn push(&mut self, bus: &mut dyn T11Bus, v: u16) {
        self.reg[6] = self.reg[6].wrapping_sub(2);
        let sp = self.reg[6];
        self.wword(bus, sp, v);
    }
    fn pop(&mut self, bus: &mut dyn T11Bus) -> u16 {
        let sp = self.reg[6];
        let v = self.rword(bus, sp);
        self.reg[6] = sp.wrapping_add(2);
        v
    }

    /// Effective address for mode/register. Byte (auto-inc/dec by 1 except SP/PC).
    fn ea(&mut self, bus: &mut dyn T11Bus, mode: u16, r: usize, byte: bool) -> Loc {
        let step = if byte && r < 6 { 1 } else { 2 };
        match mode {
            0 => Loc::Reg(r),
            1 => Loc::Mem(self.reg[r]),
            2 => { let a = self.reg[r]; self.reg[r] = a.wrapping_add(step); Loc::Mem(a) }
            3 => { let a = self.reg[r]; self.reg[r] = a.wrapping_add(2); Loc::Mem(self.rword(bus, a)) }
            4 => { self.reg[r] = self.reg[r].wrapping_sub(step); Loc::Mem(self.reg[r]) }
            5 => { self.reg[r] = self.reg[r].wrapping_sub(2); let a = self.reg[r]; Loc::Mem(self.rword(bus, a)) }
            6 => { let x = self.fetch(bus); Loc::Mem(x.wrapping_add(self.reg[r])) }
            _ => { let x = self.fetch(bus); let a = x.wrapping_add(self.reg[r]); Loc::Mem(self.rword(bus, a)) }
        }
    }

    fn rd_w(&mut self, bus: &mut dyn T11Bus, l: Loc) -> u32 {
        match l { Loc::Reg(r) => self.reg[r] as u32, Loc::Mem(a) => self.rword(bus, a) as u32 }
    }
    fn rd_b(&mut self, bus: &mut dyn T11Bus, l: Loc) -> u32 {
        match l { Loc::Reg(r) => (self.reg[r] & 0xFF) as u32, Loc::Mem(a) => bus.read_byte(a) as u32 }
    }
    fn wr_w(&mut self, bus: &mut dyn T11Bus, l: Loc, v: u32) {
        match l { Loc::Reg(r) => self.reg[r] = v as u16, Loc::Mem(a) => self.wword(bus, a, v as u16) }
    }
    /// Byte write; a register keeps its high byte.
    fn wr_b(&mut self, bus: &mut dyn T11Bus, l: Loc, v: u32) {
        match l {
            Loc::Reg(r) => self.reg[r] = (self.reg[r] & 0xFF00) | (v as u16 & 0xFF),
            Loc::Mem(a) => bus.write_byte(a, v as u8),
        }
    }
    /// T-11 writes of MOV/CLR/SXT: read the destination first (bus cycle only).
    fn wr_w_t(&mut self, bus: &mut dyn T11Bus, l: Loc, v: u32) {
        if let Loc::Mem(a) = l { self.rword(bus, a); }
        self.wr_w(bus, l, v);
    }
    fn wr_b_t(&mut self, bus: &mut dyn T11Bus, l: Loc, v: u32) {
        if let Loc::Mem(a) = l { bus.read_byte(a); }
        self.wr_b(bus, l, v);
    }

    // ---- flags ----
    fn clr(&mut self, f: u8) { self.psw &= !f; }
    fn set_if(&mut self, f: u8, c: bool) { if c { self.psw |= f; } }
    fn nz_w(&mut self, r: u32) { self.set_if(N, r & 0x8000 != 0); self.set_if(Z, r & 0xFFFF == 0); }
    fn nz_b(&mut self, r: u32) { self.set_if(N, r & 0x80 != 0); self.set_if(Z, r & 0xFF == 0); }
    fn nzvc_w(&mut self, s: u32, d: u32, r: u32) {
        self.nz_w(r);
        self.set_if(V, (s ^ d ^ r ^ (r >> 1)) & 0x8000 != 0);
        self.set_if(C, r & 0x10000 != 0);
    }
    fn nzvc_b(&mut self, s: u32, d: u32, r: u32) {
        self.nz_b(r);
        self.set_if(V, (s ^ d ^ r ^ (r >> 1)) & 0x80 != 0);
        self.set_if(C, r & 0x100 != 0);
    }
    /// V = N xor C (shifts and rotates)
    fn v_from_nc(&mut self) {
        let n = self.psw & N != 0;
        let c = self.psw & C != 0;
        self.set_if(V, n != c);
    }

    fn trap_to(&mut self, bus: &mut dyn T11Bus, vector: u16) {
        let psw = self.psw as u16;
        self.push(bus, psw);
        let pc = self.reg[7];
        self.push(bus, pc);
        self.reg[7] = self.rword(bus, vector);
        self.psw = self.rword(bus, vector + 2) as u8;
    }

    fn take_interrupt(&mut self, bus: &mut dyn T11Bus, vector: u16) -> u32 {
        let new_pc = self.rword(bus, vector);
        let new_psw = self.rword(bus, vector + 2);
        let psw = self.psw as u16;
        self.push(bus, psw);
        let pc = self.reg[7];
        self.push(bus, pc);
        self.reg[7] = new_pc;
        self.psw = new_psw as u8;
        self.waiting = false;
        114
    }

    /// Takes a pending interrupt if the CP lines outrank the PSW priority.
    fn check_irqs(&mut self, bus: &mut dyn T11Bus) -> u32 {
        if self.halt_req {
            self.halt_req = false;
            let psw = self.psw as u16;
            self.push(bus, psw);
            let pc = self.reg[7];
            self.push(bus, pc);
            self.reg[7] = self.initial_pc.wrapping_add(4);
            self.psw = 0o340;
            self.waiting = false;
            return 114;
        }
        let (prio, vector) = IRQ_TABLE[(self.cp_state & 15) as usize];
        if prio > self.psw & 0o340 {
            return self.take_interrupt(bus, vector);
        }
        0
    }

    /// Runs one instruction (taking an interrupt first if one is due).
    /// Returns clock cycles used. While in WAIT with nothing pending, returns 12.
    pub fn step(&mut self, bus: &mut dyn T11Bus) -> u32 {
        let irq = self.check_irqs(bus);
        if irq > 0 { return irq; }
        if self.waiting { return 12; }
        let op = self.fetch(bus);
        self.execute(bus, op)
    }

    /// Executes one opcode (PC already past it). Public for the tests.
    pub fn execute(&mut self, bus: &mut dyn T11Bus, op: u16) -> u32 {
        let dmode = (op >> 3) & 7;
        let dreg = (op & 7) as usize;
        let smode = (op >> 9) & 7;
        let sreg = ((op >> 6) & 7) as usize;
        let byte = op & 0x8000 != 0;
        let dm = dmode as usize;
        let sm = smode as usize;

        match (op >> 12) & 7 {
            // ---- two-operand: MOV CMP BIT BIC BIS ADD/SUB ----
            1..=6 => {
                let kind = (op >> 12) & 7;
                let cyc = SRC_CYC[sm] + if kind == 2 || kind == 3 { DST_RD[dm] } else { DST_RMW[dm] };
                if kind == 6 {
                    // ADD (0o06) / SUB (0o16): always word
                    let sl = self.ea(bus, smode, sreg, false);
                    let s = self.rd_w(bus, sl);
                    let dl = self.ea(bus, dmode, dreg, false);
                    let d = self.rd_w(bus, dl);
                    self.clr(N | Z | V | C);
                    let r = if byte { d.wrapping_sub(s) } else { d + s };
                    self.nzvc_w(s, d, r);
                    self.wr_w(bus, dl, r);
                    return cyc;
                }
                let sl = self.ea(bus, smode, sreg, byte);
                let s = if byte { self.rd_b(bus, sl) } else { self.rd_w(bus, sl) };
                if kind == 1 {
                    // MOV / MOVB
                    self.clr(N | Z | V);
                    if byte { self.nz_b(s); } else { self.nz_w(s); }
                    let dl = self.ea(bus, dmode, dreg, byte);
                    match (byte, dl) {
                        (true, Loc::Reg(r)) => self.reg[r] = s as u8 as i8 as u16, // MOVB to a register sign-extends
                        (true, l) => self.wr_b_t(bus, l, s),
                        (false, l) => self.wr_w_t(bus, l, s),
                    }
                    return cyc;
                }
                let dl = self.ea(bus, dmode, dreg, byte);
                let d = if byte { self.rd_b(bus, dl) } else { self.rd_w(bus, dl) };
                match kind {
                    2 => { // CMP: src - dst
                        self.clr(N | Z | V | C);
                        let r = s.wrapping_sub(d);
                        if byte { self.nzvc_b(s, d, r) } else { self.nzvc_w(s, d, r) }
                    }
                    3 => { // BIT
                        self.clr(N | Z | V);
                        let r = d & s;
                        if byte { self.nz_b(r) } else { self.nz_w(r) }
                    }
                    _ => { // 4 BIC, 5 BIS
                        self.clr(N | Z | V);
                        let r = if kind == 4 { d & !s } else { d | s };
                        if byte { self.nz_b(r); self.wr_b(bus, dl, r); } else { self.nz_w(r); self.wr_w(bus, dl, r); }
                    }
                }
                cyc
            }
            7 => {
                if byte { return self.illegal(bus); } // 17xxxx: floating point
                match (op >> 9) & 7 {
                    4 => { // XOR
                        let s = self.reg[sreg] as u32;
                        let dl = self.ea(bus, dmode, dreg, false);
                        let d = self.rd_w(bus, dl);
                        self.clr(N | Z | V);
                        let r = d ^ s;
                        self.nz_w(r);
                        self.wr_w(bus, dl, r);
                        ONE_RMW[dm]
                    }
                    7 => { // SOB
                        let v = self.reg[sreg].wrapping_sub(1);
                        self.reg[sreg] = v;
                        if v != 0 { self.reg[7] = self.reg[7].wrapping_sub(2 * (op & 0o77)); }
                        18
                    }
                    _ => self.illegal(bus), // MUL DIV ASH ASHC (not on the T-11), FIS
                }
            }
            _ => self.execute_misc(bus, op, byte, dmode, dreg),
        }
    }

    fn illegal(&mut self, bus: &mut dyn T11Bus) -> u32 { self.trap_to(bus, VEC_ILLINST); 48 }

    fn branch(&mut self, op: u16, cond: bool) -> u32 {
        if cond { self.reg[7] = self.reg[7].wrapping_add(((op & 0xFF) as i8 as i16 as u16).wrapping_mul(2)); }
        12
    }

    fn execute_misc(&mut self, bus: &mut dyn T11Bus, op: u16, byte: bool, dmode: u16, dreg: usize) -> u32 {
        let dm = dmode as usize;
        let n = self.psw & N != 0;
        let z = self.psw & Z != 0;
        let v = self.psw & V != 0;
        let c = self.psw & C != 0;

        let sub = (op >> 6) & 0o77;   // bits 11-6

        if byte {
            // 1000xx-1037xx: BPL BMI BHI BLOS BVC BVS BCC BCS
            if op < 0o104000 {
                let cond = match (op >> 8) & 7 {
                    0 => !n, 1 => n, 2 => !c && !z, 3 => c || z,
                    4 => !v, 5 => v, 6 => !c, _ => c,
                };
                return self.branch(op, cond);
            }
            // 1040xx EMT, 1044xx TRAP
            if op < 0o105000 {
                self.trap_to(bus, if op < 0o104400 { VEC_EMT } else { VEC_TRAP });
                return 48;
            }
        } else {
            // 0004xx-0037xx: BR BNE BEQ BGE BLT BGT BLE
            if (0o000400..0o004000).contains(&op) {
                let cond = match (op >> 8) & 7 {
                    1 => true, 2 => !z, 3 => z, 4 => n == v,
                    5 => n != v, 6 => !z && n == v, _ => z || n != v,
                };
                return self.branch(op, cond);
            }
            match sub {
                0o00 if op < 0o000100 => {
                    return match op & 0o77 {
                        0 => { // HALT
                            let psw = self.psw as u16;
                            self.push(bus, psw);
                            let pc = self.reg[7];
                            self.push(bus, pc);
                            self.reg[7] = self.initial_pc.wrapping_add(4);
                            self.psw = 0o340;
                            48
                        }
                        1 => { self.waiting = true; 0 }                          // WAIT
                        2 | 6 => {                                              // RTI, RTT
                            self.reg[7] = self.pop(bus);
                            self.psw = self.pop(bus) as u8;
                            if op & 0o77 == 2 { 24 } else { 33 }
                        }
                        3 => { self.trap_to(bus, VEC_BPT); 48 }
                        4 => { self.trap_to(bus, VEC_IOT); 48 }
                        5 => 110,                                               // RESET (nothing hooked to it)
                        7 => { self.reg[0] = (self.reg[0] & 0xFF00) | 4; 0 }  // MFPT: "T-11"
                        _ => self.illegal(bus),
                    };
                }
                0o01 => { // JMP
                    if dmode == 0 { self.trap_to(bus, VEC_TIMEOUT); return 48; }
                    if let Loc::Mem(a) = self.ea(bus, dmode, dreg, false) { self.reg[7] = a; }
                    return JMP_CYC[dm];
                }
                0o02 => {
                    return match (op >> 3) & 7 {
                        0 => { // RTS
                            self.reg[7] = self.reg[dreg];
                            self.reg[dreg] = self.pop(bus);
                            21
                        }
                        4 | 5 => { self.psw &= !((op & 15) as u8); 18 } // CCC
                        6 | 7 => { self.psw |= (op & 15) as u8; 18 }    // SCC
                        _ => self.illegal(bus),
                    };
                }
                0o03 => { // SWAB
                    let l = self.ea(bus, dmode, dreg, false);
                    let d = self.rd_w(bus, l);
                    self.clr(N | Z | V | C);
                    let r = ((d >> 8) & 0xFF) | ((d << 8) & 0xFF00);
                    self.nz_b(r);
                    self.wr_w(bus, l, r);
                    return ONE_RMW[dm];
                }
                0o40..=0o47 => { // JSR
                    if dmode == 0 { self.trap_to(bus, VEC_TIMEOUT); return 48; }
                    let sreg = ((op >> 6) & 7) as usize;
                    if let Loc::Mem(a) = self.ea(bus, dmode, dreg, false) {
                        let v = self.reg[sreg];
                        self.push(bus, v);
                        self.reg[sreg] = self.reg[7];
                        self.reg[7] = a;
                    }
                    return JSR_CYC[dm];
                }
                _ => {}
            }
        }

        // one-operand group 0050DD-0067DD / 1050DD-1067DD
        if !(0o50..=0o67).contains(&sub) { return self.illegal(bus); }
        match sub {
            0o64 => { // MARK (illegal on the T-11) / MTPS
                if !byte { return self.illegal(bus); }
                let l = self.ea(bus, dmode, dreg, true);
                let d = self.rd_b(bus, l) as u8;
                self.psw = (self.psw & !0xEF) | (d & 0xEF);
                return MTPS_CYC[dm];
            }
            0o65 | 0o66 => return self.illegal(bus), // MFPI/MTPI, MFPD/MTPD
            0o67 => {
                if byte { // MFPS
                    let r = self.psw as u32;
                    self.clr(N | Z | V);
                    self.nz_b(r);
                    let l = self.ea(bus, dmode, dreg, true);
                    match l {
                        Loc::Reg(x) => self.reg[x] = r as u8 as i8 as u16,
                        l => self.wr_b(bus, l, r),
                    }
                } else { // SXT
                    self.clr(Z | V);
                    let r = if self.psw & N != 0 { 0xFFFF } else { self.psw |= Z; 0 };
                    let l = self.ea(bus, dmode, dreg, false);
                    self.wr_w_t(bus, l, r);
                }
                return ONE_RMW[dm];
            }
            _ => {}
        }

        let l = self.ea(bus, dmode, dreg, byte);
        if sub == 0o50 { // CLR / CLRB
            if byte { self.wr_b_t(bus, l, 0) } else { self.wr_w_t(bus, l, 0) }
            self.clr(N | Z | V | C);
            self.psw |= Z;
            return ONE_RMW[dm];
        }
        let d = if byte { self.rd_b(bus, l) } else { self.rd_w(bus, l) };
        let (sign, mask) = if byte { (0x80u32, 0xFFu32) } else { (0x8000, 0xFFFF) };
        let width_nz = |cpu: &mut Self, r: u32| if byte { cpu.nz_b(r) } else { cpu.nz_w(r) };
        let r = match sub {
            0o51 => { self.clr(N | Z | V | C); let r = !d & mask; width_nz(self, r); self.psw |= C; r } // COM
            0o52 => { self.clr(N | Z | V); let r = (d + 1) & mask; width_nz(self, r); self.set_if(V, d == sign - 1); r } // INC
            0o53 => { self.clr(N | Z | V); let r = d.wrapping_sub(1) & mask; width_nz(self, r); self.set_if(V, d == sign); r } // DEC
            0o54 => { // NEG
                self.clr(N | Z | V | C);
                let r = d.wrapping_neg() & mask;
                width_nz(self, r);
                self.set_if(V, d == sign);
                self.set_if(C, r != 0);
                r
            }
            0o55 | 0o56 => { // ADC / SBC
                let s = c as u32;
                self.clr(N | Z | V | C);
                let r = if sub == 0o55 { d + s } else { d.wrapping_sub(s) };
                if byte { self.nzvc_b(s, d, r) } else { self.nzvc_w(s, d, r) }
                r & mask
            }
            0o57 => { // TST
                self.clr(N | Z | V | C);
                width_nz(self, d);
                return ONE_RD[dm];
            }
            0o60 => { // ROR
                let r = (d >> 1) | if c { sign } else { 0 };
                self.clr(N | Z | V | C); width_nz(self, r); self.set_if(C, d & 1 != 0); self.v_from_nc(); r
            }
            0o61 => { // ROL
                let r = ((d << 1) | c as u32) & mask;
                self.clr(N | Z | V | C); width_nz(self, r); self.set_if(C, d & sign != 0); self.v_from_nc(); r
            }
            0o62 => { // ASR
                let r = (d >> 1) | (d & sign);
                self.clr(N | Z | V | C); width_nz(self, r); self.set_if(C, d & 1 != 0); self.v_from_nc(); r
            }
            _ => { // 0o63 ASL
                let r = (d << 1) & mask;
                self.clr(N | Z | V | C); width_nz(self, r); self.set_if(C, d & sign != 0); self.v_from_nc(); r
            }
        };
        if byte { self.wr_b(bus, l, r) } else { self.wr_w(bus, l, r) }
        ONE_RMW[dm]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Flat 64K memory that logs writes.
    pub struct Mem { pub m: Vec<u8>, pub writes: Vec<(u16, u8)> }
    impl T11Bus for Mem {
        fn read_word(&mut self, a: u16) -> u16 { u16::from_le_bytes([self.m[a as usize], self.m[a as usize + 1]]) }
        fn write_word(&mut self, a: u16, v: u16) {
            let [lo, hi] = v.to_le_bytes();
            self.m[a as usize] = lo; self.m[a as usize + 1] = hi;
            self.writes.push((a, lo)); self.writes.push((a + 1, hi));
        }
        fn read_byte(&mut self, a: u16) -> u8 { self.m[a as usize] }
        fn write_byte(&mut self, a: u16, v: u8) { self.m[a as usize] = v; self.writes.push((a, v)); }
    }

    fn xorshift(s: &mut u32) -> u32 { *s ^= *s << 13; *s ^= *s >> 17; *s ^= *s << 5; *s }

    /// Memory pattern shared with the C++ reference harness (tools/t11_ref).
    fn fill(seed: u32) -> Vec<u8> {
        let mut s = seed | 1;
        (0..0x10000).map(|_| xorshift(&mut s) as u8).collect()
    }

    /// Runs the vectors produced by MAME's T-11 core:
    /// `seed r0..r7 psw op | r0..r7 psw cycles | addr=val ...` (all hex).
    fn run_vectors(text: &str) -> usize {
        let mut n = 0;
        for (lineno, line) in text.lines().enumerate() {
            if line.trim().is_empty() || line.starts_with('#') { continue; }
            let parts: Vec<&str> = line.split('|').collect();
            let h = |s: &str| u32::from_str_radix(s, 16).unwrap();
            let a: Vec<u32> = parts[0].split_whitespace().map(h).collect();
            let b: Vec<u32> = parts[1].split_whitespace().map(h).collect();
            let mut mem = Mem { m: fill(a[0]), writes: vec![] };
            let mut cpu = T11::new(0x36FF);
            for i in 0..8 { cpu.reg[i] = a[1 + i] as u16; }
            cpu.psw = a[9] as u8;
            let op = a[10] as u16;
            let pc = cpu.reg[7];
            mem.m[(pc & 0xFFFE) as usize] = op as u8;
            mem.m[(pc & 0xFFFE) as usize + 1] = (op >> 8) as u8;
            let o = cpu.fetch(&mut mem);
            let cyc = cpu.execute(&mut mem, o);
            let got: Vec<u32> = cpu.reg.iter().map(|&r| r as u32).chain([cpu.psw as u32, cyc]).collect();
            // final memory: compare the last value written to each address
            let mut want_w: std::collections::BTreeMap<u16, u8> = Default::default();
            for kv in parts.get(2).map(|s| s.split_whitespace().collect::<Vec<_>>()).unwrap_or_default() {
                let (k, v) = kv.split_once('=').unwrap();
                want_w.insert(h(k) as u16, h(v) as u8);
            }
            let mut got_w: std::collections::BTreeMap<u16, u8> = Default::default();
            for (k, v) in &mem.writes { got_w.insert(*k, *v); }
            assert!(got == b && got_w == want_w,
                "line {}: op {:06o}\n want {:x?} {:x?}\n got  {:x?} {:x?}", lineno + 1, op, b, want_w, got, got_w);
            n += 1;
        }
        n
    }

    #[test]
    fn t11_matches_mame_sample() {
        let n = run_vectors(include_str!("../tests/data/t11_vectors.txt"));
        assert!(n > 1000);
    }

    /// Larger run: T11_VECTORS=<file> cargo test --lib t11_matches_mame_full -- --ignored
    #[test]
    #[ignore]
    fn t11_matches_mame_full() {
        let path = std::env::var("T11_VECTORS").expect("T11_VECTORS=<file>");
        let n = run_vectors(&std::fs::read_to_string(path).unwrap());
        println!("{n} vectors ok");
    }

    #[test]
    fn interrupts_follow_cp_priority() {
        let mut mem = Mem { m: vec![0; 0x10000], writes: vec![] };
        let mut cpu = T11::new(0x36FF);
        assert_eq!(cpu.pc(), 0x8000);
        mem.m[0o114] = 0x00; mem.m[0o115] = 0x90;   // CP3 (VBLANK) vector -> 0x9000, PSW 0340
        mem.m[0o116] = 0xE0;
        cpu.psw = 0;
        cpu.reg[6] = 0x1000;
        cpu.cp_state = 8;
        assert_eq!(cpu.step(&mut mem), 114);
        assert_eq!((cpu.pc(), cpu.psw, cpu.reg[6]), (0x9000, 0xE0, 0x0FFC));
        // masked now (PSW priority 7)
        mem.m[0x9000] = 0o240 as u8; // NOP (CCC with no bits)
        mem.m[0x9001] = 0;
        assert_eq!(cpu.step(&mut mem), 18);
    }
}
