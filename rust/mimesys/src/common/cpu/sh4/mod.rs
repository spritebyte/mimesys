//! Hitachi SH-4 CPU core (the Dreamcast's SH7091 at 200 MHz): the integer unit, register
//! banks, control registers and exceptions here, the FPU in `fpu.rs`.
//!
//! The integer instructions are the SH-2's (ported for `common/cpu/sh2.rs` from ares, ISC)
//! plus the SH-4 additions (SHAD/SHLD, banked registers, SSR/SPC/SGR/DBR, cache and
//! store-queue ops, LDTLB). Behaviour follows the Hitachi SH-4 programming manual and is
//! checked against SingleStepTests/sh4 (MIT).
//!
//! - `pc` is the address of the next instruction to run.
//! - A delayed branch stores its target in `pending`; the next `step` runs the delay slot
//!   and then jumps. PC-relative loads in a delay slot use the branch target + 2 (manual).
//! - R0-R7 are banked: `r` always holds the bank in use (SR.MD && SR.RB picks bank 1),
//!   `r_bank` the other one; changing SR swaps them.
//! - Exceptions go to VBR + 0x100 (general) / 0x600 (interrupts), saving SPC / SSR / SGR.
//!   EXPEVT, INTEVT and TRA live in the chip's CCN registers, so they're handed to the bus
//!   (`Sh4Bus::event`).
//! - The MMU, address errors and privilege checks are the bus's job (or not done yet).

use serde::{Deserialize, Serialize};

mod fparith;
mod fpu;

pub trait Sh4Bus {
    fn read8(&mut self, a: u32) -> u8;
    fn read16(&mut self, a: u32) -> u16;
    fn read32(&mut self, a: u32) -> u32;
    /// 64-bit FMOV: the word at `a` is the low half.
    fn read64(&mut self, a: u32) -> u64 { self.read32(a) as u64 | (self.read32(a.wrapping_add(4)) as u64) << 32 }
    fn write8(&mut self, a: u32, v: u8);
    fn write16(&mut self, a: u32, v: u16);
    fn write32(&mut self, a: u32, v: u32);
    fn write64(&mut self, a: u32, v: u64) { self.write32(a, v as u32); self.write32(a.wrapping_add(4), (v >> 32) as u32); }
    /// An instruction fetch.
    fn fetch16(&mut self, a: u32) -> u16 { self.read16(a) }
    /// PREF @Rn: flushes a store queue when `a` is in E0000000-E3FFFFFF.
    fn pref(&mut self, _a: u32) {}
    /// LDTLB: PTEH / PTEL go into the TLB entry at MMUCR.URC.
    fn ldtlb(&mut self) {}
    /// An exception or interrupt was taken: the value for the CCN register (TRA, EXPEVT or
    /// INTEVT).
    fn event(&mut self, _reg: Event, _value: u32) {}
    /// For `Sh4::run`: the interrupt asking (level, INTEVT code), and the cycle clock.
    fn irq(&self) -> Option<(u32, u32)> { None }
    fn now(&self) -> u64 { 0 }
    fn tick(&mut self, _cycles: u64) {}
}

/// Why `Sh4::run` stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunExit {
    /// reached the target cycle
    Done,
    /// SLEEP
    Sleep,
    /// a short backward jump (to `to`, from `last`) was taken the 4th time in a row
    Loop { to: u32, last: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event { Tra, Expevt, Intevt }

/// SR bits other than T / S / Q / M.
pub const SR_MD: u32 = 1 << 30;
pub const SR_RB: u32 = 1 << 29;
pub const SR_BL: u32 = 1 << 28;
pub const SR_FD: u32 = 1 << 15;
pub const SR_MASK: u32 = 0x7000_83F3;

/// FPSCR bits.
pub const FPSCR_FR: u32 = 1 << 21;
pub const FPSCR_SZ: u32 = 1 << 20;
pub const FPSCR_PR: u32 = 1 << 19;
pub const FPSCR_DN: u32 = 1 << 18;
pub const FPSCR_MASK: u32 = 0x003F_FFFF;

/// EXPEVT codes.
pub const EXP_TRAPA: u32 = 0x160;
pub const EXP_ILLEGAL: u32 = 0x180;
pub const EXP_ILLEGAL_SLOT: u32 = 0x1A0;
pub const EXP_FPU_DISABLED: u32 = 0x800;
pub const EXP_FPU_DISABLED_SLOT: u32 = 0x820;

#[derive(Clone, Serialize, Deserialize)]
pub struct Sh4 {
    pub r: [u32; 16],
    /// the R0-R7 bank not in use
    pub r_bank: [u32; 8],
    pub pc: u32,
    pub t: bool,
    pub s: bool,
    pub q: bool,
    pub m: bool,
    /// the rest of SR: MD RB BL FD IMASK
    pub sr_hi: u32,
    pub ssr: u32,
    pub spc: u32,
    pub sgr: u32,
    pub dbr: u32,
    pub gbr: u32,
    pub vbr: u32,
    pub mach: u32,
    pub macl: u32,
    pub pr: u32,
    /// FR0-FR15 of the bank in use (FPSCR.FR)
    pub fr: [u32; 16],
    /// XF0-XF15: the other bank
    pub xf: [u32; 16],
    pub fpscr: u32,
    pub fpul: u32,
    /// target of the delayed branch whose slot runs next
    pub pending: Option<u32>,
    /// SR to load after that slot (RTE)
    #[serde(default)]
    pub pending_sr: Option<u32>,
    /// address of that branch (an exception in the slot returns to it)
    pub branch_pc: u32,
    /// halted by SLEEP until an interrupt
    pub sleeping: bool,
    /// SR.FD makes FPU instructions trap (off for the instruction tests, whose reference
    /// doesn't model it)
    #[serde(skip, default = "yes")]
    pub check_fd: bool,
    /// host floating point as it comes (round to nearest, denormals kept) instead of the
    /// FPSCR rounding / denormal modes: much cheaper, rarely visible
    #[serde(skip)]
    pub fast_fpu: bool,
    /// set while the instruction running is a delay slot
    #[serde(skip)]
    in_slot: bool,
    /// address of the instruction running
    #[serde(skip)]
    cur: u32,
    /// the instruction running took an exception (its pending jump is cancelled)
    #[serde(skip)]
    trapped: bool,
}

fn yes() -> bool { true }

impl Default for Sh4 {
    fn default() -> Self { Self::new() }
}

impl Sh4 {
    pub fn new() -> Self {
        let mut s = Self {
            r: [0; 16], r_bank: [0; 8], pc: 0, t: false, s: false, q: false, m: false, sr_hi: 0,
            ssr: 0, spc: 0, sgr: 0, dbr: 0, gbr: 0, vbr: 0, mach: 0, macl: 0, pr: 0,
            fr: [0; 16], xf: [0; 16], fpscr: 0, fpul: 0, pending: None, pending_sr: None, branch_pc: 0, sleeping: false,
            check_fd: true, fast_fpu: false, in_slot: false, cur: 0, trapped: false,
        };
        s.reset();
        s
    }

    /// Power-on / manual reset: PC A0000000, SR MD RB BL + IMASK 15, VBR 0, FPSCR 0x40001.
    pub fn reset(&mut self) {
        self.pc = 0xA000_0000;
        self.vbr = 0;
        self.t = false; self.s = false; self.q = false; self.m = false;
        // the reset value has MD and RB set; R0-R7 bank 1 is in `r`
        self.sr_hi = SR_MD | SR_RB | SR_BL | 0xF0;
        self.fpscr = 0x0004_0001;
        self.pending = None;
        self.sleeping = false;
    }

    // ------------------------------------------------------------ status register

    pub fn sr(&self) -> u32 {
        self.sr_hi | self.t as u32 | (self.s as u32) << 1 | (self.q as u32) << 8 | (self.m as u32) << 9
    }

    fn bank1(sr: u32) -> bool { sr & (SR_MD | SR_RB) == SR_MD | SR_RB }

    /// Writes SR (RB reads as 0 outside privileged mode, as on the reference).
    #[inline(never)]
    pub fn set_sr(&mut self, v: u32) {
        let mut v = v & SR_MASK;
        if v & SR_MD == 0 { v &= !SR_RB; }
        if Self::bank1(v) != Self::bank1(self.sr_hi) {
            for i in 0..8 { std::mem::swap(&mut self.r[i], &mut self.r_bank[i]); }
        }
        self.t = v & 1 != 0;
        self.s = v & 2 != 0;
        self.q = v & 0x100 != 0;
        self.m = v & 0x200 != 0;
        self.sr_hi = v & !0x303;
    }

    pub fn imask(&self) -> u32 { self.sr_hi >> 4 & 15 }

    // ------------------------------------------------------------ exceptions

    /// Can an interrupt of priority `level` (1-15) be taken now?
    pub fn accepts(&self, level: u32) -> bool {
        self.sr_hi & SR_BL == 0 && level > self.imask() && self.pending.is_none()
    }

    #[inline(never)]
    fn enter(&mut self, spc: u32, offset: u32) {
        self.spc = spc;
        self.ssr = self.sr();
        self.sgr = self.r[15];
        let sr = self.sr() | SR_MD | SR_RB | SR_BL;
        self.set_sr(sr);
        self.pc = self.vbr.wrapping_add(offset);
        self.pending = None;
        self.pending_sr = None;
        self.sleeping = false;
        self.trapped = true;
    }

    /// Take an interrupt (the caller checked `accepts`): INTEVT = `code`, PC = VBR + 0x600.
    pub fn interrupt<B: Sh4Bus + ?Sized>(&mut self, bus: &mut B, code: u32) {
        // SLEEP waits on its own address; the interrupt returns after it
        let ret = if self.sleeping { self.pc.wrapping_add(2) } else { self.pc };
        bus.event(Event::Intevt, code);
        self.enter(ret, 0x600);
    }

    /// A general exception on the instruction running (re-run after the handler: SPC = its
    /// address, or the branch's when it's a delay slot).
    fn exception<B: Sh4Bus + ?Sized>(&mut self, bus: &mut B, code: u32) {
        let ret = if self.in_slot { self.branch_pc } else { self.cur };
        bus.event(Event::Expevt, code);
        self.enter(ret, 0x100);
    }

    #[inline(never)]
    fn illegal<B: Sh4Bus + ?Sized>(&mut self, bus: &mut B) {
        let code = if self.in_slot { EXP_ILLEGAL_SLOT } else { EXP_ILLEGAL };
        self.exception(bus, code);
    }

    // ------------------------------------------------------------ branches

    /// A delayed branch to `to` (illegal inside a delay slot).
    fn delayed<B: Sh4Bus + ?Sized>(&mut self, bus: &mut B, to: u32) -> bool {
        if self.in_slot { self.illegal(bus); return false; }
        self.pending = Some(to);
        self.branch_pc = self.cur;
        true
    }

    /// PC as PC-relative addressing sees it: the instruction + 4, or in a delay slot the
    /// branch target + 2.
    fn pc_rel(&self, slot_target: Option<u32>) -> u32 {
        match slot_target {
            Some(t) => t.wrapping_add(2),
            None => self.cur.wrapping_add(4),
        }
    }

    /// Run until the bus clock reaches `target`, `cpi` cycles an instruction, taking
    /// interrupts as the bus asks. With `loops` (the last backward jump target and how many
    /// times in a row), stops at the 4th pass of a loop of up to 64 bytes so the caller
    /// can look at it. One call runs many instructions, so the per-instruction cost stays
    /// in one loop.
    pub fn run<B: Sh4Bus + ?Sized>(&mut self, bus: &mut B, target: u64, cpi: u64, mut loops: Option<&mut (u32, u32)>) -> RunExit {
        while bus.now() < target {
            if let Some((level, code)) = bus.irq() {
                if self.accepts(level) { self.interrupt(bus, code); }
            }
            if self.sleeping { return RunExit::Sleep; }
            let pc = self.pc;
            self.step(bus);
            bus.tick(cpi);
            let to = self.pc;
            if to < pc && pc - to <= 64 {
                if let Some(l) = loops.as_deref_mut() {
                    if to == l.0 { l.1 += 1; } else { *l = (to, 1); }
                    if l.1 == 4 { return RunExit::Loop { to, last: pc }; }
                }
            }
        }
        RunExit::Done
    }

    /// Run one instruction.
    #[inline(always)]
    pub fn step<B: Sh4Bus + ?Sized>(&mut self, bus: &mut B) {
        if self.sleeping { return; }
        let pending = self.pending.take();
        self.in_slot = pending.is_some();
        self.cur = self.pc;
        self.trapped = false;
        let op = bus.fetch16(self.cur);
        self.pc = self.cur.wrapping_add(2);
        self.execute(bus, op, pending);
        if let Some(t) = pending {
            if !self.trapped {
                self.pc = t;
                if let Some(sr) = self.pending_sr.take() { self.set_sr(sr); }
            }
        }
    }

    #[inline(always)]
    fn execute<B: Sh4Bus + ?Sized>(&mut self, bus: &mut B, op: u16, slot_target: Option<u32>) {
        let n = (op >> 8 & 15) as usize;
        let m = (op >> 4 & 15) as usize;
        let i = (op & 0xFF) as u32;
        let d4 = (op & 15) as u32;
        let d8 = (op & 0xFF) as u32;
        let d12 = (op & 0xFFF) as u32;
        let simm = i as u8 as i8 as i32 as u32;

        if op >> 12 == 0xF { return self.fpu(bus, op); }

        // n m - - and n m d / n imm forms
        match (op >> 8 & 0xF0) | (op & 0x0F) {
            0x04 => return bus.write8(self.r[n].wrapping_add(self.r[0]), self.r[m] as u8),
            0x05 => return bus.write16(self.r[n].wrapping_add(self.r[0]), self.r[m] as u16),
            0x06 => return bus.write32(self.r[n].wrapping_add(self.r[0]), self.r[m]),
            0x07 => { self.macl = self.r[n].wrapping_mul(self.r[m]); return; }
            0x0C => { self.r[n] = bus.read8(self.r[m].wrapping_add(self.r[0])) as i8 as u32; return; }
            0x0D => { self.r[n] = bus.read16(self.r[m].wrapping_add(self.r[0])) as i16 as u32; return; }
            0x0E => { self.r[n] = bus.read32(self.r[m].wrapping_add(self.r[0])); return; }
            0x0F => return self.mac_l(bus, m, n),
            0x10..=0x1F => return bus.write32(self.r[n].wrapping_add(d4 * 4), self.r[m]),
            0x20 => return bus.write8(self.r[n], self.r[m] as u8),
            0x21 => return bus.write16(self.r[n], self.r[m] as u16),
            0x22 => return bus.write32(self.r[n], self.r[m]),
            0x24 => { let t = self.r[m]; self.r[n] = self.r[n].wrapping_sub(1); return bus.write8(self.r[n], t as u8); }
            0x25 => { let t = self.r[m]; self.r[n] = self.r[n].wrapping_sub(2); return bus.write16(self.r[n], t as u16); }
            0x26 => { let t = self.r[m]; self.r[n] = self.r[n].wrapping_sub(4); return bus.write32(self.r[n], t); }
            0x27 => { self.q = self.r[n] >> 31 != 0; self.m = self.r[m] >> 31 != 0; self.t = self.q != self.m; return; }
            0x28 => { self.t = self.r[n] & self.r[m] == 0; return; }
            0x29 => { self.r[n] &= self.r[m]; return; }
            0x2A => { self.r[n] ^= self.r[m]; return; }
            0x2B => { self.r[n] |= self.r[m]; return; }
            0x2C => { let t = self.r[n] ^ self.r[m]; self.t = (0..4).any(|k| t >> (k * 8) & 0xFF == 0); return; }
            0x2D => { self.r[n] = self.r[n] >> 16 | self.r[m] << 16; return; }
            0x2E => { self.macl = (self.r[n] as u16 as u32).wrapping_mul(self.r[m] as u16 as u32); return; }
            0x2F => { self.macl = ((self.r[n] as i16 as i32).wrapping_mul(self.r[m] as i16 as i32)) as u32; return; }
            0x30 => { self.t = self.r[n] == self.r[m]; return; }
            0x32 => { self.t = self.r[n] >= self.r[m]; return; }
            0x33 => { self.t = self.r[n] as i32 >= self.r[m] as i32; return; }
            0x34 => return self.div1(m, n),
            0x35 => { let v = self.r[n] as u64 * self.r[m] as u64; self.set_mac(v); return; }
            0x36 => { self.t = self.r[n] > self.r[m]; return; }
            0x37 => { self.t = self.r[n] as i32 > self.r[m] as i32; return; }
            0x38 => { self.r[n] = self.r[n].wrapping_sub(self.r[m]); return; }
            0x3A => {
                let a = self.r[n];
                let b = a.wrapping_sub(self.r[m]);
                self.r[n] = b.wrapping_sub(self.t as u32);
                self.t = a < b || b < self.r[n];
                return;
            }
            0x3B => {
                let d = (self.r[n] as i32 >= 0) as u32 ^ 1;
                let s = ((self.r[m] as i32) < 0) as u32 + d;
                self.r[n] = self.r[n].wrapping_sub(self.r[m]);
                let res = ((self.r[n] as i32) < 0) as u32 + d;
                self.t = s == 1 && res == 1;
                return;
            }
            0x3C => { self.r[n] = self.r[n].wrapping_add(self.r[m]); return; }
            0x3D => { let v = (self.r[n] as i32 as i64 * self.r[m] as i32 as i64) as u64; self.set_mac(v); return; }
            0x3E => {
                let a = self.r[n];
                let b = a.wrapping_add(self.r[m]);
                self.r[n] = b.wrapping_add(self.t as u32);
                self.t = a > b || b > self.r[n];
                return;
            }
            0x3F => {
                let d = ((self.r[n] as i32) < 0) as u32;
                let s = ((self.r[m] as i32) < 0) as u32 + d;
                self.r[n] = self.r[n].wrapping_add(self.r[m]);
                let res = ((self.r[n] as i32) < 0) as u32 + d;
                self.t = s != 1 && res == 1;
                return;
            }
            0x4C => {
                // SHAD Rm,Rn: arithmetic shift by Rm (left if >= 0, else right by -Rm & 31,
                // 32 = all sign)
                let s = self.r[m] as i32;
                self.r[n] = if s >= 0 { self.r[n] << (s & 31) }
                    else if s & 31 == 0 { ((self.r[n] as i32) >> 31) as u32 }
                    else { ((self.r[n] as i32) >> ((!s & 31) + 1)) as u32 };
                return;
            }
            0x4D => {
                // SHLD Rm,Rn: logical
                let s = self.r[m] as i32;
                self.r[n] = if s >= 0 { self.r[n] << (s & 31) }
                    else if s & 31 == 0 { 0 }
                    else { self.r[n] >> ((!s & 31) + 1) };
                return;
            }
            0x4F => return self.mac_w(bus, m, n),
            0x50..=0x5F => { self.r[n] = bus.read32(self.r[m].wrapping_add(d4 * 4)); return; }
            0x60 => { self.r[n] = bus.read8(self.r[m]) as i8 as u32; return; }
            0x61 => { self.r[n] = bus.read16(self.r[m]) as i16 as u32; return; }
            0x62 => { self.r[n] = bus.read32(self.r[m]); return; }
            0x63 => { self.r[n] = self.r[m]; return; }
            0x64 => { let v = bus.read8(self.r[m]) as i8 as u32; if n != m { self.r[m] = self.r[m].wrapping_add(1); } self.r[n] = v; return; }
            0x65 => { let v = bus.read16(self.r[m]) as i16 as u32; if n != m { self.r[m] = self.r[m].wrapping_add(2); } self.r[n] = v; return; }
            0x66 => { let v = bus.read32(self.r[m]); if n != m { self.r[m] = self.r[m].wrapping_add(4); } self.r[n] = v; return; }
            0x67 => { self.r[n] = !self.r[m]; return; }
            0x68 => { self.r[n] = self.r[m] & !0xFFFF | self.r[m] >> 8 & 0xFF | self.r[m] << 8 & 0xFF00; return; }
            0x69 => { self.r[n] = self.r[m].rotate_left(16); return; }
            0x6A => {
                let t = 0u32.wrapping_sub(self.r[m]);
                self.r[n] = t.wrapping_sub(self.t as u32);
                self.t = 0 < t || t < self.r[n];
                return;
            }
            0x6B => { self.r[n] = 0u32.wrapping_sub(self.r[m]); return; }
            0x6C => { self.r[n] = self.r[m] & 0xFF; return; }
            0x6D => { self.r[n] = self.r[m] & 0xFFFF; return; }
            0x6E => { self.r[n] = self.r[m] as i8 as u32; return; }
            0x6F => { self.r[n] = self.r[m] as i16 as u32; return; }
            0x70..=0x7F => { self.r[n] = self.r[n].wrapping_add(simm); return; }
            0x90..=0x9F => {
                let pc = self.pc_rel(slot_target);
                self.r[n] = bus.read16(pc.wrapping_add(d8 * 2)) as i16 as u32;
                return;
            }
            0xA0..=0xAF => {
                let d = ((d12 << 20) as i32 >> 20) as u32;
                let to = self.cur.wrapping_add(4).wrapping_add(d.wrapping_mul(2));
                self.delayed(bus, to);
                return;
            }
            0xB0..=0xBF => {
                let d = ((d12 << 20) as i32 >> 20) as u32;
                let to = self.cur.wrapping_add(4).wrapping_add(d.wrapping_mul(2));
                if self.delayed(bus, to) { self.pr = self.cur.wrapping_add(4); }
                return;
            }
            0xD0..=0xDF => {
                let pc = self.pc_rel(slot_target);
                self.r[n] = bus.read32((pc & !3).wrapping_add(d8 * 4));
                return;
            }
            0xE0..=0xEF => { self.r[n] = simm; return; }
            _ => {}
        }

        // 8-bit opcode + immediate / displacement forms
        let gbr = self.gbr;
        match op >> 8 {
            0x80 => return bus.write8(self.r[m].wrapping_add(d4), self.r[0] as u8),
            0x81 => return bus.write16(self.r[m].wrapping_add(d4 * 2), self.r[0] as u16),
            0x84 => { self.r[0] = bus.read8(self.r[m].wrapping_add(d4)) as i8 as u32; return; }
            0x85 => { self.r[0] = bus.read16(self.r[m].wrapping_add(d4 * 2)) as i16 as u32; return; }
            0x88 => { self.t = self.r[0] == simm; return; }
            0x89 | 0x8B | 0x8D | 0x8F => {
                if self.in_slot { return self.illegal(bus); }
                let take = self.t == (op >> 8 & 2 == 0); // 89/8D on T, 8B/8F on !T
                if take {
                    let to = self.cur.wrapping_add(4).wrapping_add(simm.wrapping_mul(2));
                    if op >> 8 & 4 != 0 { self.delayed(bus, to); } else { self.pc = to; }
                }
                return;
            }
            0xC0 => return bus.write8(gbr.wrapping_add(d8), self.r[0] as u8),
            0xC1 => return bus.write16(gbr.wrapping_add(d8 * 2), self.r[0] as u16),
            0xC2 => return bus.write32(gbr.wrapping_add(d8 * 4), self.r[0]),
            0xC3 => {
                // TRAPA #imm: TRA = imm << 2, back to the next instruction
                if self.in_slot { return self.illegal(bus); }
                bus.event(Event::Tra, i << 2);
                bus.event(Event::Expevt, EXP_TRAPA);
                let ret = self.cur.wrapping_add(2);
                return self.enter(ret, 0x100);
            }
            0xC4 => { self.r[0] = bus.read8(gbr.wrapping_add(d8)) as i8 as u32; return; }
            0xC5 => { self.r[0] = bus.read16(gbr.wrapping_add(d8 * 2)) as i16 as u32; return; }
            0xC6 => { self.r[0] = bus.read32(gbr.wrapping_add(d8 * 4)); return; }
            0xC7 => {
                let pc = self.pc_rel(slot_target);
                self.r[0] = (pc & !3).wrapping_add(d8 * 4);
                return;
            }
            0xC8 => { self.t = self.r[0] & i == 0; return; }
            0xC9 => { self.r[0] &= i; return; }
            0xCA => { self.r[0] ^= i; return; }
            0xCB => { self.r[0] |= i; return; }
            0xCC..=0xCF => {
                let a = gbr.wrapping_add(self.r[0]);
                let b = bus.read8(a) as u32;
                match op >> 8 {
                    0xCC => self.t = b & i == 0,
                    0xCD => bus.write8(a, (b & i) as u8),
                    0xCE => bus.write8(a, (b ^ i) as u8),
                    _ => bus.write8(a, (b | i) as u8),
                }
                return;
            }
            _ => {}
        }

        // banked registers: STC Rm_BANK,Rn / LDC Rm,Rn_BANK / STC.L / LDC.L
        match op & 0xF08F {
            0x0082 => { self.r[n] = self.r_bank[m & 7]; return; }
            0x408E => { self.r_bank[m & 7] = self.r[n]; return; }
            0x4083 => {
                let v = self.r_bank[m & 7];
                self.r[n] = self.r[n].wrapping_sub(4);
                return bus.write32(self.r[n], v);
            }
            0x4087 => {
                let v = bus.read32(self.r[n]);
                self.r[n] = self.r[n].wrapping_add(4);
                self.r_bank[m & 7] = v;
                return;
            }
            _ => {}
        }

        // one-register forms
        match (op >> 4 & 0x0F00) | (op & 0xFF) {
            0x002 => { self.r[n] = self.sr(); return; }
            0x003 => {
                let to = self.cur.wrapping_add(4).wrapping_add(self.r[n]);
                if self.delayed(bus, to) { self.pr = self.cur.wrapping_add(4); }
                return;
            }
            0x00A => { self.r[n] = self.mach; return; }
            0x012 => { self.r[n] = self.gbr; return; }
            0x01A => { self.r[n] = self.macl; return; }
            0x022 => { self.r[n] = self.vbr; return; }
            0x023 => {
                let to = self.cur.wrapping_add(4).wrapping_add(self.r[n]);
                self.delayed(bus, to);
                return;
            }
            0x029 => { self.r[n] = self.t as u32; return; }
            0x02A => { self.r[n] = self.pr; return; }
            0x032 => { self.r[n] = self.ssr; return; }
            0x03A => { self.r[n] = self.sgr; return; }
            0x042 => { self.r[n] = self.spc; return; }
            0x05A => { self.r[n] = self.fpul; return; }
            0x06A => { self.r[n] = self.fpscr; return; }
            0x0FA => { self.r[n] = self.dbr; return; }
            0x083 => { bus.pref(self.r[n]); return; }
            0x093 | 0x0A3 | 0x0B3 => return, // OCBI / OCBP / OCBWB: no cache model
            0x0C3 => return bus.write32(self.r[n], self.r[0]), // MOVCA.L R0,@Rn
            0x400 | 0x420 => { self.t = self.r[n] >> 31 != 0; self.r[n] <<= 1; return; }
            0x401 => { self.t = self.r[n] & 1 != 0; self.r[n] >>= 1; return; }
            0x402 | 0x403 | 0x412 | 0x413 | 0x422 | 0x423 | 0x433 | 0x443 | 0x432 | 0x452 | 0x462 | 0x4F2 => {
                let v = match op & 0xFF {
                    0x02 => self.mach,
                    0x03 => self.sr(),
                    0x12 => self.macl,
                    0x13 => self.gbr,
                    0x22 => self.pr,
                    0x23 => self.vbr,
                    0x33 => self.ssr,
                    0x43 => self.spc,
                    0x32 => self.sgr,
                    0x52 => self.fpul,
                    0x62 => self.fpscr,
                    _ => self.dbr,
                };
                self.r[n] = self.r[n].wrapping_sub(4);
                return bus.write32(self.r[n], v);
            }
            0x404 => { self.t = self.r[n] >> 31 != 0; self.r[n] = self.r[n].rotate_left(1); return; }
            0x405 => { self.t = self.r[n] & 1 != 0; self.r[n] = self.r[n].rotate_right(1); return; }
            0x406 | 0x407 | 0x416 | 0x417 | 0x426 | 0x427 | 0x437 | 0x447 | 0x456 | 0x466 | 0x4F6 => {
                let v = bus.read32(self.r[n]);
                self.r[n] = self.r[n].wrapping_add(4);
                match op & 0xFF {
                    0x06 => self.mach = v,
                    0x07 => self.set_sr(v),
                    0x16 => self.macl = v,
                    0x17 => self.gbr = v,
                    0x26 => self.pr = v,
                    0x27 => self.vbr = v,
                    0x37 => self.ssr = v,
                    0x47 => self.spc = v,
                    0x56 => self.fpul = v,
                    0x66 => self.set_fpscr(v),
                    _ => self.dbr = v,
                }
                return;
            }
            0x408 => { self.r[n] <<= 2; return; }
            0x409 => { self.r[n] >>= 2; return; }
            0x40A => { self.mach = self.r[n]; return; }
            0x40B => {
                let to = self.r[n];
                if self.delayed(bus, to) { self.pr = self.cur.wrapping_add(4); }
                return;
            }
            0x40E => { let v = self.r[n]; self.set_sr(v); return; }
            0x410 => { self.r[n] = self.r[n].wrapping_sub(1); self.t = self.r[n] == 0; return; }
            0x411 => { self.t = self.r[n] as i32 >= 0; return; }
            0x415 => { self.t = self.r[n] as i32 > 0; return; }
            0x418 => { self.r[n] <<= 8; return; }
            0x419 => { self.r[n] >>= 8; return; }
            0x41A => { self.macl = self.r[n]; return; }
            0x41B => {
                // TAS.B: test and set bit 7
                let b = bus.read8(self.r[n]);
                self.t = b == 0;
                return bus.write8(self.r[n], b | 0x80);
            }
            0x41E => { self.gbr = self.r[n]; return; }
            0x421 => { self.t = self.r[n] & 1 != 0; self.r[n] = (self.r[n] as i32 >> 1) as u32; return; }
            0x424 => { let c = self.r[n] >> 31 != 0; self.r[n] = self.r[n] << 1 | self.t as u32; self.t = c; return; }
            0x425 => { let c = self.r[n] & 1 != 0; self.r[n] = self.r[n] >> 1 | (self.t as u32) << 31; self.t = c; return; }
            0x428 => { self.r[n] <<= 16; return; }
            0x429 => { self.r[n] >>= 16; return; }
            0x42A => { self.pr = self.r[n]; return; }
            0x42B => {
                let to = self.r[n];
                self.delayed(bus, to);
                return;
            }
            0x42E => { self.vbr = self.r[n]; return; }
            0x43E => { self.ssr = self.r[n]; return; }
            0x44E => { self.spc = self.r[n]; return; }
            0x45A => { self.fpul = self.r[n]; return; }
            0x46A => { let v = self.r[n]; self.set_fpscr(v); return; }
            0x4FA => { self.dbr = self.r[n]; return; }
            _ => {}
        }

        match op {
            0x0008 => self.t = false,
            0x0009 => {}
            0x000B => { let to = self.pr; self.delayed(bus, to); }
            0x0018 => self.t = true,
            0x0019 => { self.q = false; self.m = false; self.t = false; }
            0x001B => {
                // SLEEP: wait here for an interrupt
                self.pc = self.cur;
                self.sleeping = true;
            }
            0x0028 => { self.mach = 0; self.macl = 0; }
            0x002B => {
                // RTE: PC = SPC, SR = SSR after the delay slot (the slot runs with the old
                // SR and register bank, as on the reference)
                let to = self.spc;
                if self.delayed(bus, to) { self.pending_sr = Some(self.ssr); }
            }
            0x0038 => bus.ldtlb(),
            0x0048 => self.s = false,
            0x0058 => self.s = true,
            _ => self.illegal(bus),
        }
    }

    fn set_mac(&mut self, v: u64) { self.mach = (v >> 32) as u32; self.macl = v as u32; }
    fn mac(&self) -> u64 { (self.mach as u64) << 32 | self.macl as u64 }

    #[inline(never)]
    fn div1(&mut self, m: usize, n: usize) {
        let q = self.q;
        self.q = self.r[n] >> 31 != 0;
        let mut r = ((self.r[n] << 1) | self.t as u32) as u64;
        if q == self.m { r = r.wrapping_sub(self.r[m] as u64); } else { r = r.wrapping_add(self.r[m] as u64); }
        self.r[n] = r as u32;
        self.q = (self.q ^ self.m) ^ (r >> 32 & 1 != 0);
        self.t = !(self.q ^ self.m);
    }

    /// MAC.W @Rm+,@Rn+ (S: 32-bit saturating, overflow sets MACH bit 0)
    #[inline(never)]
    fn mac_w<B: Sh4Bus + ?Sized>(&mut self, bus: &mut B, m: usize, n: usize) {
        let a = bus.read16(self.r[n]) as i16 as i64;
        self.r[n] = self.r[n].wrapping_add(2);
        let b = bus.read16(self.r[m]) as i16 as i64;
        self.r[m] = self.r[m].wrapping_add(2);
        if !self.s {
            let v = self.mac().wrapping_add((a * b) as u64);
            self.set_mac(v);
        } else {
            let p = self.macl as i32 as i64 + a * b;
            let c = p.clamp(i32::MIN as i64, i32::MAX as i64);
            if c != p { self.mach |= 1; }
            self.macl = c as u32;
        }
    }

    /// MAC.L @Rm+,@Rn+ (S: the result saturates to 48 bits)
    #[inline(never)]
    fn mac_l<B: Sh4Bus + ?Sized>(&mut self, bus: &mut B, m: usize, n: usize) {
        let a = bus.read32(self.r[n]) as i32 as i64;
        self.r[n] = self.r[n].wrapping_add(4);
        let b = bus.read32(self.r[m]) as i32 as i64;
        self.r[m] = self.r[m].wrapping_add(4);
        let mut v = self.mac() as i64;
        v = v.wrapping_add(a * b);
        if self.s { v = v.clamp(-(1i64 << 47), (1i64 << 47) - 1); }
        self.set_mac(v as u64);
    }
}

#[cfg(test)]
mod tests;
