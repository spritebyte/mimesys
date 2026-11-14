//! The fast path of `Sh4::run`: the instructions programs spend their time in, decoded
//! with one table lookup (`KIND`, built at compile time by `kind`) and one jump, without
//! the bookkeeping the general `execute` needs (current address, delay-slot state,
//! exception flags).
//!
//! Each arm does exactly what `execute` does for that instruction; the test
//! `fast_path_matches_the_reference` runs every opcode through both and compares the CPU
//! and memory. Nothing here can raise an exception, so there is no state to roll back
//! (with the MMU on, `run` doesn't come here at all).
//!
//! Kinds from `K_FIRST_SENSITIVE` on behave differently in a delay slot (PC-relative
//! addressing, branches, FPU exceptions): a slot holding one goes through the general path.
//! Kind 0 is "not here": the general path runs it.

use super::{Sh4, Sh4Bus};

/// What the instruction did to the program counter.
pub(super) enum Flow {
    /// on to the next instruction
    Next,
    /// jump now (BT / BF)
    Jump(u32),
    /// jump after the delay slot
    Delayed(u32),
    /// the PC is already set (FPU: may have trapped)
    Done,
    /// not a fast-path instruction
    Slow,
}

pub(super) const K_MOV: u8 = 1;
pub(super) const K_MOVI: u8 = 2;
pub(super) const K_LDB: u8 = 3;
pub(super) const K_LDW: u8 = 4;
pub(super) const K_LDL: u8 = 5;
pub(super) const K_LDBP: u8 = 6;
pub(super) const K_LDWP: u8 = 7;
pub(super) const K_LDLP: u8 = 8;
pub(super) const K_LDLD: u8 = 9;
pub(super) const K_LDBX: u8 = 10;
pub(super) const K_LDWX: u8 = 11;
pub(super) const K_LDLX: u8 = 12;
pub(super) const K_LDB0: u8 = 13;
pub(super) const K_LDW0: u8 = 14;
pub(super) const K_LDBG: u8 = 15;
pub(super) const K_LDWG: u8 = 16;
pub(super) const K_LDLG: u8 = 17;
pub(super) const K_STB: u8 = 18;
pub(super) const K_STW: u8 = 19;
pub(super) const K_STL: u8 = 20;
pub(super) const K_STBM: u8 = 21;
pub(super) const K_STWM: u8 = 22;
pub(super) const K_STLM: u8 = 23;
pub(super) const K_STLD: u8 = 24;
pub(super) const K_STBX: u8 = 25;
pub(super) const K_STWX: u8 = 26;
pub(super) const K_STLX: u8 = 27;
pub(super) const K_STB0: u8 = 28;
pub(super) const K_STW0: u8 = 29;
pub(super) const K_STBG: u8 = 30;
pub(super) const K_STWG: u8 = 31;
pub(super) const K_STLG: u8 = 32;
pub(super) const K_ADD: u8 = 33;
pub(super) const K_ADDI: u8 = 34;
pub(super) const K_SUB: u8 = 35;
pub(super) const K_NEG: u8 = 36;
pub(super) const K_NOT: u8 = 37;
pub(super) const K_AND: u8 = 38;
pub(super) const K_XOR: u8 = 39;
pub(super) const K_OR: u8 = 40;
pub(super) const K_ANDI: u8 = 41;
pub(super) const K_XORI: u8 = 42;
pub(super) const K_ORI: u8 = 43;
pub(super) const K_TSTI: u8 = 44;
pub(super) const K_ADDC: u8 = 45;
pub(super) const K_SUBC: u8 = 46;
pub(super) const K_NEGC: u8 = 47;
pub(super) const K_CMPEQ: u8 = 48;
pub(super) const K_CMPHS: u8 = 49;
pub(super) const K_CMPGE: u8 = 50;
pub(super) const K_CMPHI: u8 = 51;
pub(super) const K_CMPGT: u8 = 52;
pub(super) const K_CMPIM: u8 = 53;
pub(super) const K_CMPPZ: u8 = 54;
pub(super) const K_CMPPL: u8 = 55;
pub(super) const K_TST: u8 = 56;
pub(super) const K_EXTUB: u8 = 57;
pub(super) const K_EXTUW: u8 = 58;
pub(super) const K_EXTSB: u8 = 59;
pub(super) const K_EXTSW: u8 = 60;
pub(super) const K_SWAPB: u8 = 61;
pub(super) const K_SWAPW: u8 = 62;
pub(super) const K_XTRCT: u8 = 63;
pub(super) const K_SHLL: u8 = 64;
pub(super) const K_SHAL: u8 = 65;
pub(super) const K_SHLR: u8 = 66;
pub(super) const K_SHAR: u8 = 67;
pub(super) const K_SHLL2: u8 = 68;
pub(super) const K_SHLR2: u8 = 69;
pub(super) const K_SHLL8: u8 = 70;
pub(super) const K_SHLR8: u8 = 71;
pub(super) const K_SHLL16: u8 = 72;
pub(super) const K_SHLR16: u8 = 73;
pub(super) const K_ROTL: u8 = 74;
pub(super) const K_ROTR: u8 = 75;
pub(super) const K_ROTCL: u8 = 76;
pub(super) const K_ROTCR: u8 = 77;
pub(super) const K_SHAD: u8 = 78;
pub(super) const K_SHLD: u8 = 79;
pub(super) const K_DT: u8 = 80;
pub(super) const K_MOVT: u8 = 81;
pub(super) const K_MULL: u8 = 82;
pub(super) const K_MULU: u8 = 83;
pub(super) const K_MULS: u8 = 84;
pub(super) const K_DMULU: u8 = 85;
pub(super) const K_DMULS: u8 = 86;
pub(super) const K_STSMACH: u8 = 87;
pub(super) const K_STSMACL: u8 = 88;
pub(super) const K_STSPR: u8 = 89;
pub(super) const K_LDSMACH: u8 = 90;
pub(super) const K_LDSMACL: u8 = 91;
pub(super) const K_LDSPR: u8 = 92;
pub(super) const K_STSLMACH: u8 = 93;
pub(super) const K_STSLMACL: u8 = 94;
pub(super) const K_STSLPR: u8 = 95;
pub(super) const K_LDSLMACH: u8 = 96;
pub(super) const K_LDSLMACL: u8 = 97;
pub(super) const K_LDSLPR: u8 = 98;
pub(super) const K_CLRT: u8 = 99;
pub(super) const K_SETT: u8 = 100;
pub(super) const K_NOP: u8 = 101;
pub(super) const K_PREF: u8 = 102;
pub(super) const K_MOVWPC: u8 = 103;
pub(super) const K_MOVLPC: u8 = 104;
pub(super) const K_MOVA: u8 = 105;
pub(super) const K_BT: u8 = 106;
pub(super) const K_BF: u8 = 107;
pub(super) const K_BTS: u8 = 108;
pub(super) const K_BFS: u8 = 109;
pub(super) const K_BRA: u8 = 110;
pub(super) const K_BSR: u8 = 111;
pub(super) const K_JSR: u8 = 112;
pub(super) const K_JMP: u8 = 113;
pub(super) const K_RTS: u8 = 114;
pub(super) const K_BRAF: u8 = 115;
pub(super) const K_BSRF: u8 = 116;
pub(super) const K_FPU: u8 = 117;
/// First kind that needs the general path in a delay slot.
pub(super) const K_FIRST_SENSITIVE: u8 = K_MOVWPC;

/// The fast-path kind of an opcode (0: none).
const fn kind(op: u16) -> u8 {
    if op & 0xF00F == 0x6003 { return K_MOV; } // MOV Rm,Rn
    if op & 0xF000 == 0xE000 { return K_MOVI; } // MOV #imm,Rn
    if op & 0xF00F == 0x6000 { return K_LDB; } // MOV.B @Rm,Rn
    if op & 0xF00F == 0x6001 { return K_LDW; } // MOV.W @Rm,Rn
    if op & 0xF00F == 0x6002 { return K_LDL; } // MOV.L @Rm,Rn
    if op & 0xF00F == 0x6004 { return K_LDBP; } // MOV.B @Rm+,Rn
    if op & 0xF00F == 0x6005 { return K_LDWP; } // MOV.W @Rm+,Rn
    if op & 0xF00F == 0x6006 { return K_LDLP; } // MOV.L @Rm+,Rn
    if op & 0xF000 == 0x5000 { return K_LDLD; } // MOV.L @(disp,Rm),Rn
    if op & 0xF00F == 0x000C { return K_LDBX; } // MOV.B @(R0,Rm),Rn
    if op & 0xF00F == 0x000D { return K_LDWX; } // MOV.W @(R0,Rm),Rn
    if op & 0xF00F == 0x000E { return K_LDLX; } // MOV.L @(R0,Rm),Rn
    if op & 0xFF00 == 0x8400 { return K_LDB0; } // MOV.B @(disp,Rm),R0
    if op & 0xFF00 == 0x8500 { return K_LDW0; } // MOV.W @(disp,Rm),R0
    if op & 0xFF00 == 0xC400 { return K_LDBG; } // MOV.B @(disp,GBR),R0
    if op & 0xFF00 == 0xC500 { return K_LDWG; } // MOV.W @(disp,GBR),R0
    if op & 0xFF00 == 0xC600 { return K_LDLG; } // MOV.L @(disp,GBR),R0
    if op & 0xF00F == 0x2000 { return K_STB; } // MOV.B Rm,@Rn
    if op & 0xF00F == 0x2001 { return K_STW; } // MOV.W Rm,@Rn
    if op & 0xF00F == 0x2002 { return K_STL; } // MOV.L Rm,@Rn
    if op & 0xF00F == 0x2004 { return K_STBM; } // MOV.B Rm,@-Rn
    if op & 0xF00F == 0x2005 { return K_STWM; } // MOV.W Rm,@-Rn
    if op & 0xF00F == 0x2006 { return K_STLM; } // MOV.L Rm,@-Rn
    if op & 0xF000 == 0x1000 { return K_STLD; } // MOV.L Rm,@(disp,Rn)
    if op & 0xF00F == 0x0004 { return K_STBX; } // MOV.B Rm,@(R0,Rn)
    if op & 0xF00F == 0x0005 { return K_STWX; } // MOV.W Rm,@(R0,Rn)
    if op & 0xF00F == 0x0006 { return K_STLX; } // MOV.L Rm,@(R0,Rn)
    if op & 0xFF00 == 0x8000 { return K_STB0; } // MOV.B R0,@(disp,Rn)
    if op & 0xFF00 == 0x8100 { return K_STW0; } // MOV.W R0,@(disp,Rn)
    if op & 0xFF00 == 0xC000 { return K_STBG; } // MOV.B R0,@(disp,GBR)
    if op & 0xFF00 == 0xC100 { return K_STWG; } // MOV.W R0,@(disp,GBR)
    if op & 0xFF00 == 0xC200 { return K_STLG; } // MOV.L R0,@(disp,GBR)
    if op & 0xF00F == 0x300C { return K_ADD; } // ADD Rm,Rn
    if op & 0xF000 == 0x7000 { return K_ADDI; } // ADD #imm,Rn
    if op & 0xF00F == 0x3008 { return K_SUB; } // SUB Rm,Rn
    if op & 0xF00F == 0x600B { return K_NEG; } // NEG Rm,Rn
    if op & 0xF00F == 0x6007 { return K_NOT; } // NOT Rm,Rn
    if op & 0xF00F == 0x2009 { return K_AND; } // AND Rm,Rn
    if op & 0xF00F == 0x200A { return K_XOR; } // XOR Rm,Rn
    if op & 0xF00F == 0x200B { return K_OR; } // OR Rm,Rn
    if op & 0xFF00 == 0xC900 { return K_ANDI; } // AND #imm,R0
    if op & 0xFF00 == 0xCA00 { return K_XORI; } // XOR #imm,R0
    if op & 0xFF00 == 0xCB00 { return K_ORI; } // OR #imm,R0
    if op & 0xFF00 == 0xC800 { return K_TSTI; } // TST #imm,R0
    if op & 0xF00F == 0x300E { return K_ADDC; } // ADDC Rm,Rn
    if op & 0xF00F == 0x300A { return K_SUBC; } // SUBC Rm,Rn
    if op & 0xF00F == 0x600A { return K_NEGC; } // NEGC Rm,Rn
    if op & 0xF00F == 0x3000 { return K_CMPEQ; } // CMP/EQ Rm,Rn
    if op & 0xF00F == 0x3002 { return K_CMPHS; } // CMP/HS Rm,Rn
    if op & 0xF00F == 0x3003 { return K_CMPGE; } // CMP/GE Rm,Rn
    if op & 0xF00F == 0x3006 { return K_CMPHI; } // CMP/HI Rm,Rn
    if op & 0xF00F == 0x3007 { return K_CMPGT; } // CMP/GT Rm,Rn
    if op & 0xFF00 == 0x8800 { return K_CMPIM; } // CMP/EQ #imm,R0
    if op & 0xF0FF == 0x4011 { return K_CMPPZ; } // CMP/PZ Rn
    if op & 0xF0FF == 0x4015 { return K_CMPPL; } // CMP/PL Rn
    if op & 0xF00F == 0x2008 { return K_TST; } // TST Rm,Rn
    if op & 0xF00F == 0x600C { return K_EXTUB; } // EXTU.B Rm,Rn
    if op & 0xF00F == 0x600D { return K_EXTUW; } // EXTU.W Rm,Rn
    if op & 0xF00F == 0x600E { return K_EXTSB; } // EXTS.B Rm,Rn
    if op & 0xF00F == 0x600F { return K_EXTSW; } // EXTS.W Rm,Rn
    if op & 0xF00F == 0x6008 { return K_SWAPB; } // SWAP.B Rm,Rn
    if op & 0xF00F == 0x6009 { return K_SWAPW; } // SWAP.W Rm,Rn
    if op & 0xF00F == 0x200D { return K_XTRCT; } // XTRCT Rm,Rn
    if op & 0xF0FF == 0x4000 { return K_SHLL; } // SHLL Rn
    if op & 0xF0FF == 0x4020 { return K_SHAL; } // SHAL Rn
    if op & 0xF0FF == 0x4001 { return K_SHLR; } // SHLR Rn
    if op & 0xF0FF == 0x4021 { return K_SHAR; } // SHAR Rn
    if op & 0xF0FF == 0x4008 { return K_SHLL2; } // SHLL2 Rn
    if op & 0xF0FF == 0x4009 { return K_SHLR2; } // SHLR2 Rn
    if op & 0xF0FF == 0x4018 { return K_SHLL8; } // SHLL8 Rn
    if op & 0xF0FF == 0x4019 { return K_SHLR8; } // SHLR8 Rn
    if op & 0xF0FF == 0x4028 { return K_SHLL16; } // SHLL16 Rn
    if op & 0xF0FF == 0x4029 { return K_SHLR16; } // SHLR16 Rn
    if op & 0xF0FF == 0x4004 { return K_ROTL; } // ROTL Rn
    if op & 0xF0FF == 0x4005 { return K_ROTR; } // ROTR Rn
    if op & 0xF0FF == 0x4024 { return K_ROTCL; } // ROTCL Rn
    if op & 0xF0FF == 0x4025 { return K_ROTCR; } // ROTCR Rn
    if op & 0xF00F == 0x400C { return K_SHAD; } // SHAD Rm,Rn
    if op & 0xF00F == 0x400D { return K_SHLD; } // SHLD Rm,Rn
    if op & 0xF0FF == 0x4010 { return K_DT; } // DT Rn
    if op & 0xF0FF == 0x0029 { return K_MOVT; } // MOVT Rn
    if op & 0xF00F == 0x0007 { return K_MULL; } // MUL.L Rm,Rn
    if op & 0xF00F == 0x200E { return K_MULU; } // MULU.W Rm,Rn
    if op & 0xF00F == 0x200F { return K_MULS; } // MULS.W Rm,Rn
    if op & 0xF00F == 0x3005 { return K_DMULU; } // DMULU.L Rm,Rn
    if op & 0xF00F == 0x300D { return K_DMULS; } // DMULS.L Rm,Rn
    if op & 0xF0FF == 0x000A { return K_STSMACH; } // STS MACH,Rn
    if op & 0xF0FF == 0x001A { return K_STSMACL; } // STS MACL,Rn
    if op & 0xF0FF == 0x002A { return K_STSPR; } // STS PR,Rn
    if op & 0xF0FF == 0x400A { return K_LDSMACH; } // LDS Rn,MACH
    if op & 0xF0FF == 0x401A { return K_LDSMACL; } // LDS Rn,MACL
    if op & 0xF0FF == 0x402A { return K_LDSPR; } // LDS Rn,PR
    if op & 0xF0FF == 0x4002 { return K_STSLMACH; } // STS.L MACH,@-Rn
    if op & 0xF0FF == 0x4012 { return K_STSLMACL; } // STS.L MACL,@-Rn
    if op & 0xF0FF == 0x4022 { return K_STSLPR; } // STS.L PR,@-Rn
    if op & 0xF0FF == 0x4006 { return K_LDSLMACH; } // LDS.L @Rn+,MACH
    if op & 0xF0FF == 0x4016 { return K_LDSLMACL; } // LDS.L @Rn+,MACL
    if op & 0xF0FF == 0x4026 { return K_LDSLPR; } // LDS.L @Rn+,PR
    if op & 0xFFFF == 0x0008 { return K_CLRT; } // CLRT
    if op & 0xFFFF == 0x0018 { return K_SETT; } // SETT
    if op & 0xFFFF == 0x0009 { return K_NOP; } // NOP
    if op & 0xF0FF == 0x0083 { return K_PREF; } // PREF @Rn
    if op & 0xF000 == 0x9000 { return K_MOVWPC; } // MOV.W @(disp,PC),Rn
    if op & 0xF000 == 0xD000 { return K_MOVLPC; } // MOV.L @(disp,PC),Rn
    if op & 0xFF00 == 0xC700 { return K_MOVA; } // MOVA @(disp,PC),R0
    if op & 0xFF00 == 0x8900 { return K_BT; } // BT label
    if op & 0xFF00 == 0x8B00 { return K_BF; } // BF label
    if op & 0xFF00 == 0x8D00 { return K_BTS; } // BT/S label
    if op & 0xFF00 == 0x8F00 { return K_BFS; } // BF/S label
    if op & 0xF000 == 0xA000 { return K_BRA; } // BRA label
    if op & 0xF000 == 0xB000 { return K_BSR; } // BSR label
    if op & 0xF0FF == 0x400B { return K_JSR; } // JSR @Rn
    if op & 0xF0FF == 0x402B { return K_JMP; } // JMP @Rn
    if op & 0xFFFF == 0x000B { return K_RTS; } // RTS
    if op & 0xF0FF == 0x0023 { return K_BRAF; } // BRAF Rn
    if op & 0xF0FF == 0x0003 { return K_BSRF; } // BSRF Rn
    if op & 0xF000 == 0xF000 { return K_FPU; } // every FPU instruction
    0
}

const fn build() -> [u8; 65536] {
    let mut t = [0u8; 65536];
    let mut op = 0usize;
    while op < 65536 {
        t[op] = kind(op as u16);
        op += 1;
    }
    t
}

/// Opcode -> kind.
pub(super) static KIND: [u8; 65536] = build();

impl Sh4 {
    /// Run the instruction `op` (of kind `k`) at `pc`. Doesn't touch `self.pc` unless it
    /// returns `Flow::Done`. (Each arm picks its own fields out of the opcode, so nothing
    /// is computed ahead of the jump.)
    #[inline(always)]
    pub(super) fn fast<B: Sh4Bus + ?Sized>(&mut self, bus: &mut B, pc: u32, op: u16, k: u8) -> Flow {
        match k & 127 {
            K_MOV => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; self.r[n] = self.r[m]; }
            K_MOVI => { let n = (op >> 8 & 15) as usize; let simm = op as u8 as i8 as i32 as u32; self.r[n] = simm; }
            K_LDB => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; self.r[n] = bus.read8(self.r[m]) as i8 as u32; }
            K_LDW => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; self.r[n] = bus.read16(self.r[m]) as i16 as u32; }
            K_LDL => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; self.r[n] = bus.read32(self.r[m]); }
            K_LDBP => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; let v = bus.read8(self.r[m]) as i8 as u32; if n != m { self.r[m] = self.r[m].wrapping_add(1); } self.r[n] = v; }
            K_LDWP => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; let v = bus.read16(self.r[m]) as i16 as u32; if n != m { self.r[m] = self.r[m].wrapping_add(2); } self.r[n] = v; }
            K_LDLP => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; let v = bus.read32(self.r[m]); if n != m { self.r[m] = self.r[m].wrapping_add(4); } self.r[n] = v; }
            K_LDLD => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; let d4 = (op & 15) as u32; self.r[n] = bus.read32(self.r[m].wrapping_add(d4 * 4)); }
            K_LDBX => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; self.r[n] = bus.read8(self.r[m].wrapping_add(self.r[0])) as i8 as u32; }
            K_LDWX => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; self.r[n] = bus.read16(self.r[m].wrapping_add(self.r[0])) as i16 as u32; }
            K_LDLX => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; self.r[n] = bus.read32(self.r[m].wrapping_add(self.r[0])); }
            K_LDB0 => { let m = (op >> 4 & 15) as usize; let d4 = (op & 15) as u32; self.r[0] = bus.read8(self.r[m].wrapping_add(d4)) as i8 as u32; }
            K_LDW0 => { let m = (op >> 4 & 15) as usize; let d4 = (op & 15) as u32; self.r[0] = bus.read16(self.r[m].wrapping_add(d4 * 2)) as i16 as u32; }
            K_LDBG => { let d8 = (op & 0xFF) as u32; self.r[0] = bus.read8(self.gbr.wrapping_add(d8)) as i8 as u32; }
            K_LDWG => { let d8 = (op & 0xFF) as u32; self.r[0] = bus.read16(self.gbr.wrapping_add(d8 * 2)) as i16 as u32; }
            K_LDLG => { let d8 = (op & 0xFF) as u32; self.r[0] = bus.read32(self.gbr.wrapping_add(d8 * 4)); }
            K_STB => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; bus.write8(self.r[n], self.r[m] as u8); }
            K_STW => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; bus.write16(self.r[n], self.r[m] as u16); }
            K_STL => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; bus.write32(self.r[n], self.r[m]); }
            K_STBM => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; let t = self.r[m]; self.r[n] = self.r[n].wrapping_sub(1); bus.write8(self.r[n], t as u8); }
            K_STWM => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; let t = self.r[m]; self.r[n] = self.r[n].wrapping_sub(2); bus.write16(self.r[n], t as u16); }
            K_STLM => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; let t = self.r[m]; self.r[n] = self.r[n].wrapping_sub(4); bus.write32(self.r[n], t); }
            K_STLD => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; let d4 = (op & 15) as u32; bus.write32(self.r[n].wrapping_add(d4 * 4), self.r[m]); }
            K_STBX => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; bus.write8(self.r[n].wrapping_add(self.r[0]), self.r[m] as u8); }
            K_STWX => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; bus.write16(self.r[n].wrapping_add(self.r[0]), self.r[m] as u16); }
            K_STLX => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; bus.write32(self.r[n].wrapping_add(self.r[0]), self.r[m]); }
            K_STB0 => { let m = (op >> 4 & 15) as usize; let d4 = (op & 15) as u32; bus.write8(self.r[m].wrapping_add(d4), self.r[0] as u8); }
            K_STW0 => { let m = (op >> 4 & 15) as usize; let d4 = (op & 15) as u32; bus.write16(self.r[m].wrapping_add(d4 * 2), self.r[0] as u16); }
            K_STBG => { let d8 = (op & 0xFF) as u32; bus.write8(self.gbr.wrapping_add(d8), self.r[0] as u8); }
            K_STWG => { let d8 = (op & 0xFF) as u32; bus.write16(self.gbr.wrapping_add(d8 * 2), self.r[0] as u16); }
            K_STLG => { let d8 = (op & 0xFF) as u32; bus.write32(self.gbr.wrapping_add(d8 * 4), self.r[0]); }
            K_ADD => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; self.r[n] = self.r[n].wrapping_add(self.r[m]); }
            K_ADDI => { let n = (op >> 8 & 15) as usize; let simm = op as u8 as i8 as i32 as u32; self.r[n] = self.r[n].wrapping_add(simm); }
            K_SUB => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; self.r[n] = self.r[n].wrapping_sub(self.r[m]); }
            K_NEG => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; self.r[n] = 0u32.wrapping_sub(self.r[m]); }
            K_NOT => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; self.r[n] = !self.r[m]; }
            K_AND => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; self.r[n] &= self.r[m]; }
            K_XOR => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; self.r[n] ^= self.r[m]; }
            K_OR => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; self.r[n] |= self.r[m]; }
            K_ANDI => { let d8 = (op & 0xFF) as u32; self.r[0] &= d8; }
            K_XORI => { let d8 = (op & 0xFF) as u32; self.r[0] ^= d8; }
            K_ORI => { let d8 = (op & 0xFF) as u32; self.r[0] |= d8; }
            K_TSTI => { let d8 = (op & 0xFF) as u32; self.t = self.r[0] & d8 == 0; }
            K_ADDC => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; let a = self.r[n]; let b = a.wrapping_add(self.r[m]); self.r[n] = b.wrapping_add(self.t as u32); self.t = a > b || b > self.r[n]; }
            K_SUBC => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; let a = self.r[n]; let b = a.wrapping_sub(self.r[m]); self.r[n] = b.wrapping_sub(self.t as u32); self.t = a < b || b < self.r[n]; }
            K_NEGC => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; let t = 0u32.wrapping_sub(self.r[m]); self.r[n] = t.wrapping_sub(self.t as u32); self.t = 0 < t || t < self.r[n]; }
            K_CMPEQ => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; self.t = self.r[n] == self.r[m]; }
            K_CMPHS => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; self.t = self.r[n] >= self.r[m]; }
            K_CMPGE => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; self.t = self.r[n] as i32 >= self.r[m] as i32; }
            K_CMPHI => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; self.t = self.r[n] > self.r[m]; }
            K_CMPGT => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; self.t = self.r[n] as i32 > self.r[m] as i32; }
            K_CMPIM => { let simm = op as u8 as i8 as i32 as u32; self.t = self.r[0] == simm; }
            K_CMPPZ => { let n = (op >> 8 & 15) as usize; self.t = self.r[n] as i32 >= 0; }
            K_CMPPL => { let n = (op >> 8 & 15) as usize; self.t = self.r[n] as i32 > 0; }
            K_TST => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; self.t = self.r[n] & self.r[m] == 0; }
            K_EXTUB => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; self.r[n] = self.r[m] & 0xFF; }
            K_EXTUW => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; self.r[n] = self.r[m] & 0xFFFF; }
            K_EXTSB => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; self.r[n] = self.r[m] as i8 as u32; }
            K_EXTSW => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; self.r[n] = self.r[m] as i16 as u32; }
            K_SWAPB => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; self.r[n] = self.r[m] & !0xFFFF | self.r[m] >> 8 & 0xFF | self.r[m] << 8 & 0xFF00; }
            K_SWAPW => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; self.r[n] = self.r[m].rotate_left(16); }
            K_XTRCT => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; self.r[n] = self.r[n] >> 16 | self.r[m] << 16; }
            K_SHLL => { let n = (op >> 8 & 15) as usize; self.t = self.r[n] >> 31 != 0; self.r[n] <<= 1; }
            K_SHAL => { let n = (op >> 8 & 15) as usize; self.t = self.r[n] >> 31 != 0; self.r[n] <<= 1; }
            K_SHLR => { let n = (op >> 8 & 15) as usize; self.t = self.r[n] & 1 != 0; self.r[n] >>= 1; }
            K_SHAR => { let n = (op >> 8 & 15) as usize; self.t = self.r[n] & 1 != 0; self.r[n] = (self.r[n] as i32 >> 1) as u32; }
            K_SHLL2 => { let n = (op >> 8 & 15) as usize; self.r[n] <<= 2; }
            K_SHLR2 => { let n = (op >> 8 & 15) as usize; self.r[n] >>= 2; }
            K_SHLL8 => { let n = (op >> 8 & 15) as usize; self.r[n] <<= 8; }
            K_SHLR8 => { let n = (op >> 8 & 15) as usize; self.r[n] >>= 8; }
            K_SHLL16 => { let n = (op >> 8 & 15) as usize; self.r[n] <<= 16; }
            K_SHLR16 => { let n = (op >> 8 & 15) as usize; self.r[n] >>= 16; }
            K_ROTL => { let n = (op >> 8 & 15) as usize; self.t = self.r[n] >> 31 != 0; self.r[n] = self.r[n].rotate_left(1); }
            K_ROTR => { let n = (op >> 8 & 15) as usize; self.t = self.r[n] & 1 != 0; self.r[n] = self.r[n].rotate_right(1); }
            K_ROTCL => { let n = (op >> 8 & 15) as usize; let c = self.r[n] >> 31 != 0; self.r[n] = self.r[n] << 1 | self.t as u32; self.t = c; }
            K_ROTCR => { let n = (op >> 8 & 15) as usize; let c = self.r[n] & 1 != 0; self.r[n] = self.r[n] >> 1 | (self.t as u32) << 31; self.t = c; }
            K_SHAD => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; let s = self.r[m] as i32; self.r[n] = if s >= 0 { self.r[n] << (s & 31) } else if s & 31 == 0 { ((self.r[n] as i32) >> 31) as u32 } else { ((self.r[n] as i32) >> ((!s & 31) + 1)) as u32 }; }
            K_SHLD => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; let s = self.r[m] as i32; self.r[n] = if s >= 0 { self.r[n] << (s & 31) } else if s & 31 == 0 { 0 } else { self.r[n] >> ((!s & 31) + 1) }; }
            K_DT => { let n = (op >> 8 & 15) as usize; self.r[n] = self.r[n].wrapping_sub(1); self.t = self.r[n] == 0; }
            K_MOVT => { let n = (op >> 8 & 15) as usize; self.r[n] = self.t as u32; }
            K_MULL => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; self.macl = self.r[n].wrapping_mul(self.r[m]); }
            K_MULU => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; self.macl = (self.r[n] as u16 as u32).wrapping_mul(self.r[m] as u16 as u32); }
            K_MULS => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; self.macl = ((self.r[n] as i16 as i32).wrapping_mul(self.r[m] as i16 as i32)) as u32; }
            K_DMULU => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; let v = self.r[n] as u64 * self.r[m] as u64; self.mach = (v >> 32) as u32; self.macl = v as u32; }
            K_DMULS => { let n = (op >> 8 & 15) as usize; let m = (op >> 4 & 15) as usize; let v = (self.r[n] as i32 as i64 * self.r[m] as i32 as i64) as u64; self.mach = (v >> 32) as u32; self.macl = v as u32; }
            K_STSMACH => { let n = (op >> 8 & 15) as usize; self.r[n] = self.mach; }
            K_STSMACL => { let n = (op >> 8 & 15) as usize; self.r[n] = self.macl; }
            K_STSPR => { let n = (op >> 8 & 15) as usize; self.r[n] = self.pr; }
            K_LDSMACH => { let n = (op >> 8 & 15) as usize; self.mach = self.r[n]; }
            K_LDSMACL => { let n = (op >> 8 & 15) as usize; self.macl = self.r[n]; }
            K_LDSPR => { let n = (op >> 8 & 15) as usize; self.pr = self.r[n]; }
            K_STSLMACH => { let n = (op >> 8 & 15) as usize; let v = self.mach; self.r[n] = self.r[n].wrapping_sub(4); bus.write32(self.r[n], v); }
            K_STSLMACL => { let n = (op >> 8 & 15) as usize; let v = self.macl; self.r[n] = self.r[n].wrapping_sub(4); bus.write32(self.r[n], v); }
            K_STSLPR => { let n = (op >> 8 & 15) as usize; let v = self.pr; self.r[n] = self.r[n].wrapping_sub(4); bus.write32(self.r[n], v); }
            K_LDSLMACH => { let n = (op >> 8 & 15) as usize; let v = bus.read32(self.r[n]); self.r[n] = self.r[n].wrapping_add(4); self.mach = v; }
            K_LDSLMACL => { let n = (op >> 8 & 15) as usize; let v = bus.read32(self.r[n]); self.r[n] = self.r[n].wrapping_add(4); self.macl = v; }
            K_LDSLPR => { let n = (op >> 8 & 15) as usize; let v = bus.read32(self.r[n]); self.r[n] = self.r[n].wrapping_add(4); self.pr = v; }
            K_CLRT => { self.t = false; }
            K_SETT => { self.t = true; }
            K_NOP => {}
            K_PREF => { let n = (op >> 8 & 15) as usize; bus.pref(self.r[n]); }
            K_MOVWPC => { let n = (op >> 8 & 15) as usize; let d8 = (op & 0xFF) as u32; self.r[n] = bus.read16(pc.wrapping_add(4).wrapping_add(d8 * 2)) as i16 as u32; }
            K_MOVLPC => { let n = (op >> 8 & 15) as usize; let d8 = (op & 0xFF) as u32; self.r[n] = bus.read32((pc.wrapping_add(4) & !3).wrapping_add(d8 * 4)); }
            K_MOVA => { let d8 = (op & 0xFF) as u32; self.r[0] = (pc.wrapping_add(4) & !3).wrapping_add(d8 * 4); }
            K_BT => { let simm = op as u8 as i8 as i32 as u32; if self.t { return Flow::Jump(pc.wrapping_add(4).wrapping_add(simm.wrapping_mul(2))); } }
            K_BF => { let simm = op as u8 as i8 as i32 as u32; if !self.t { return Flow::Jump(pc.wrapping_add(4).wrapping_add(simm.wrapping_mul(2))); } }
            K_BTS => { let simm = op as u8 as i8 as i32 as u32; if self.t { return Flow::Delayed(pc.wrapping_add(4).wrapping_add(simm.wrapping_mul(2))); } }
            K_BFS => { let simm = op as u8 as i8 as i32 as u32; if !self.t { return Flow::Delayed(pc.wrapping_add(4).wrapping_add(simm.wrapping_mul(2))); } }
            K_BRA => { let d12s = (((op as u32) << 20) as i32 >> 20) as u32; return Flow::Delayed(pc.wrapping_add(4).wrapping_add(d12s.wrapping_mul(2))); }
            K_BSR => { let d12s = (((op as u32) << 20) as i32 >> 20) as u32; self.pr = pc.wrapping_add(4); return Flow::Delayed(pc.wrapping_add(4).wrapping_add(d12s.wrapping_mul(2))); }
            K_JSR => { let n = (op >> 8 & 15) as usize; let to = self.r[n]; self.pr = pc.wrapping_add(4); return Flow::Delayed(to); }
            K_JMP => { let n = (op >> 8 & 15) as usize; return Flow::Delayed(self.r[n]); }
            K_RTS => { return Flow::Delayed(self.pr); }
            K_BRAF => { let n = (op >> 8 & 15) as usize; return Flow::Delayed(pc.wrapping_add(4).wrapping_add(self.r[n])); }
            K_BSRF => { let n = (op >> 8 & 15) as usize; let to = pc.wrapping_add(4).wrapping_add(self.r[n]); self.pr = pc.wrapping_add(4); return Flow::Delayed(to); }
            K_FPU => {
                // the FPU has its own decoder, and can trap (FPU disabled): give it the
                // state the general path keeps
                self.cur = pc;
                self.in_slot = false;
                self.trapped = false;
                self.pc = pc.wrapping_add(2);
                self.fpu(bus, op);
                return Flow::Done;
            }
            // every value of `k & 127` has an arm, so the jump needs no range check
            0 | 118..=255 => return Flow::Slow,
        }

        Flow::Next
    }
}
