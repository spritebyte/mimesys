//! MIPS R3000A (the PlayStation's CW33300 at 33.87 MHz): MIPS I, no FPU, COP0 for
//! exceptions and COP2 for the GTE (`gte.rs`). Written from the MIPS R3000 manuals and
//! Nocash's PSX-SPX.
//!
//! - Branch delay: `pc` is the instruction about to run and `next_pc` the one after; a
//!   branch rewrites `next_pc`, so its delay slot runs first. An exception in a delay slot
//!   sets Cause.BD and points EPC at the branch.
//! - Load delay: a load's result (LW and friends, MFC0, MFC2, CFC2) lands one instruction
//!   late, in `load`. The next instruction still sees the old value; if it writes the same
//!   register itself, its write wins. LWL / LWR merge with the in-flight value.
//! - Addresses: KUSEG / KSEG0 / KSEG1 all mirror the 512 MB physical space (KSEG1 is the
//!   uncached view); KSEG2 has only the cache-control register at FFFE0130. The 1 KB
//!   scratchpad (1F800000) lives here, outside KSEG1.
//! - SR.IsC (cache isolated): stores go to the cache, i.e. nowhere here. The BIOS uses it
//!   to flush the I-cache.
//! - An interrupt taken on a GTE command runs the command first (the console has, and the
//!   BIOS's handler steps over it).
//! - Timing: one cycle an instruction, plus multiply / divide stalls when HI / LO are read
//!   too early. Memory wait states are the bus's to add.

use serde::{Deserialize, Serialize};

pub mod gte;
pub use gte::Gte;

/// Physical addresses (already masked to 29 bits, or FFFE0000+ for KSEG2 never reaches it).
pub trait R3000Bus {
    fn read8(&mut self, pa: u32) -> u8;
    fn read16(&mut self, pa: u32) -> u16;
    fn read32(&mut self, pa: u32) -> u32;
    fn write8(&mut self, pa: u32, v: u8);
    fn write16(&mut self, pa: u32, v: u16);
    fn write32(&mut self, pa: u32, v: u32);
    /// An instruction fetch; `cached`: through KUSEG / KSEG0 (the I-cache), not KSEG1
    fn fetch32(&mut self, pa: u32, cached: bool) -> u32 { let _ = cached; self.read32(pa) }
}

// COP0 registers
pub const BADVADDR: usize = 8;
pub const SR: usize = 12;
pub const CAUSE: usize = 13;
pub const EPC: usize = 14;
pub const PRID: usize = 15;

// exception codes
pub const EXC_INT: u32 = 0;
pub const EXC_ADEL: u32 = 4;
pub const EXC_ADES: u32 = 5;
pub const EXC_SYSCALL: u32 = 8;
pub const EXC_BREAK: u32 = 9;
pub const EXC_RI: u32 = 10;
pub const EXC_CPU: u32 = 11;
pub const EXC_OV: u32 = 12;

const SR_IEC: u32 = 1;
const SR_KUC: u32 = 2;
const SR_ISC: u32 = 1 << 16;
const SR_BEV: u32 = 1 << 22;
const SR_CU0: u32 = 1 << 28;
const SR_CU2: u32 = 1 << 30;

/// KUSEG (x4), KSEG0, KSEG1, KSEG2 (x2)
const SEG_MASK: [u32; 8] = [0x1FFF_FFFF, 0x1FFF_FFFF, 0x1FFF_FFFF, 0x1FFF_FFFF, 0x1FFF_FFFF, 0x1FFF_FFFF, 0xFFFF_FFFF, 0xFFFF_FFFF];

#[derive(Clone, Serialize, Deserialize)]
pub struct R3000 {
    pub regs: [u32; 32],
    pub hi: u32,
    pub lo: u32,
    /// the instruction about to run
    pub pc: u32,
    pub next_pc: u32,
    /// the one running now (for EPC)
    cur_pc: u32,
    /// the instruction just run was a branch or jump: the next one is its delay slot
    branch_pending: bool,
    delay_slot: bool,
    /// a load landing after the current instruction / after the next one
    load: (u8, u32),
    next_load: (u8, u32),
    pub cop0: [u32; 32],
    pub gte: Gte,
    pub scratch: Vec<u8>,
    pub cache_ctrl: u32,
    /// the interrupt controller's line, Cause bit 10
    irq_line: bool,
    /// when the multiplier / divider finishes
    muldiv_done: u64,
    pub cycles: u64,
}

impl Default for R3000 {
    fn default() -> Self { Self::new() }
}

/// A fault inside an instruction: the exception has been taken, stop executing it.
struct Fault;
type Res<T> = Result<T, Fault>;

impl R3000 {
    pub fn new() -> Self {
        let mut c = Self {
            regs: [0; 32],
            hi: 0,
            lo: 0,
            pc: 0,
            next_pc: 0,
            cur_pc: 0,
            branch_pending: false,
            delay_slot: false,
            load: (0, 0),
            next_load: (0, 0),
            cop0: [0; 32],
            gte: Gte::default(),
            scratch: vec![0; 0x400],
            cache_ctrl: 0,
            irq_line: false,
            muldiv_done: 0,
            cycles: 0,
        };
        c.reset();
        c
    }

    pub fn reset(&mut self) {
        self.pc = 0xBFC0_0000;
        self.next_pc = self.pc + 4;
        self.branch_pending = false;
        self.load = (0, 0);
        self.next_load = (0, 0);
        self.cop0[SR] = SR_BEV;
        self.cop0[CAUSE] = 0;
        self.cop0[PRID] = 0x0000_0002;
    }

    /// Start running at `pc` (tests, or a PS-EXE side-load).
    pub fn jump(&mut self, pc: u32) {
        self.pc = pc;
        self.next_pc = pc.wrapping_add(4);
        self.branch_pending = false;
    }

    pub fn set_irq(&mut self, on: bool) { self.irq_line = on; }

    pub fn cause(&self) -> u32 { self.cop0[CAUSE] | (self.irq_line as u32) << 10 }

    // ------------------------------------------------------------------ registers

    #[inline]
    fn set(&mut self, r: u32, v: u32) {
        let r = r as usize;
        if r != 0 { self.regs[r] = v; }
        if self.load.0 as usize == r { self.load.0 = 0; }
    }

    #[inline]
    fn set_delayed(&mut self, r: u32, v: u32) {
        if self.load.0 as u32 == r { self.load.0 = 0; }
        self.next_load = (r as u8, v);
    }

    #[inline]
    fn commit_load(&mut self) {
        let (r, v) = self.load;
        if r != 0 { self.regs[r as usize] = v; }
        self.load = self.next_load;
        self.next_load = (0, 0);
    }

    // ------------------------------------------------------------------ exceptions

    pub fn exception(&mut self, code: u32, cop: u32) {
        let sr = self.cop0[SR];
        // push the KU/IE stack
        self.cop0[SR] = (sr & !0x3F) | ((sr << 2) & 0x3F);
        let mut cause = self.cop0[CAUSE] & !(0x8000_0000 | 0x3000_0000 | 0x7C);
        cause |= code << 2 | (cop & 3) << 28;
        self.cop0[EPC] = if self.delay_slot {
            cause |= 0x8000_0000;
            self.cur_pc.wrapping_sub(4)
        } else {
            self.cur_pc
        };
        self.cop0[CAUSE] = cause;
        let vec = if sr & SR_BEV != 0 { 0xBFC0_0180 } else { 0x8000_0080 };
        // the load in flight still lands
        let (r, v) = self.load;
        if r != 0 { self.regs[r as usize] = v; }
        self.load = (0, 0);
        self.next_load = (0, 0);
        self.pc = vec;
        self.next_pc = vec + 4;
        self.branch_pending = false;
    }

    fn fault(&mut self, code: u32) -> Fault { self.exception(code, 0); Fault }

    fn addr_fault(&mut self, code: u32, va: u32) -> Fault {
        self.cop0[BADVADDR] = va;
        self.fault(code)
    }

    // ------------------------------------------------------------------ memory

    #[inline]
    fn scratch_hit(va: u32) -> bool {
        // not through KSEG1
        va >> 29 != 5 && (va & 0x1FFF_FC00) == 0x1F80_0000
    }

    fn check(&mut self, va: u32, align: u32, code: u32) -> Res<()> {
        if va & align != 0 || (self.cop0[SR] & SR_KUC != 0 && va >= 0x8000_0000) {
            return Err(self.addr_fault(code, va));
        }
        Ok(())
    }

    fn load32(&mut self, bus: &mut dyn R3000Bus, va: u32) -> Res<u32> {
        self.check(va, 3, EXC_ADEL)?;
        if Self::scratch_hit(va) {
            let o = (va & 0x3FC) as usize;
            return Ok(u32::from_le_bytes(self.scratch[o..o + 4].try_into().unwrap()));
        }
        if va >= 0xC000_0000 { return Ok(if va == 0xFFFE_0130 { self.cache_ctrl } else { 0 }); }
        Ok(bus.read32(va & SEG_MASK[(va >> 29) as usize]))
    }

    fn load16(&mut self, bus: &mut dyn R3000Bus, va: u32) -> Res<u16> {
        self.check(va, 1, EXC_ADEL)?;
        if Self::scratch_hit(va) {
            let o = (va & 0x3FE) as usize;
            return Ok(u16::from_le_bytes([self.scratch[o], self.scratch[o + 1]]));
        }
        if va >= 0xC000_0000 { return Ok(0); }
        Ok(bus.read16(va & SEG_MASK[(va >> 29) as usize]))
    }

    fn load8(&mut self, bus: &mut dyn R3000Bus, va: u32) -> Res<u8> {
        self.check(va, 0, EXC_ADEL)?;
        if Self::scratch_hit(va) { return Ok(self.scratch[(va & 0x3FF) as usize]); }
        if va >= 0xC000_0000 { return Ok(0); }
        Ok(bus.read8(va & SEG_MASK[(va >> 29) as usize]))
    }

    fn store32(&mut self, bus: &mut dyn R3000Bus, va: u32, v: u32) -> Res<()> {
        self.check(va, 3, EXC_ADES)?;
        if va >= 0xC000_0000 {
            if va == 0xFFFE_0130 { self.cache_ctrl = v; }
            return Ok(());
        }
        if self.cop0[SR] & SR_ISC != 0 { return Ok(()); }
        if Self::scratch_hit(va) {
            let o = (va & 0x3FC) as usize;
            self.scratch[o..o + 4].copy_from_slice(&v.to_le_bytes());
            return Ok(());
        }
        bus.write32(va & SEG_MASK[(va >> 29) as usize], v);
        Ok(())
    }

    fn store16(&mut self, bus: &mut dyn R3000Bus, va: u32, v: u16) -> Res<()> {
        self.check(va, 1, EXC_ADES)?;
        if va >= 0xC000_0000 || self.cop0[SR] & SR_ISC != 0 { return Ok(()); }
        if Self::scratch_hit(va) {
            let o = (va & 0x3FE) as usize;
            self.scratch[o..o + 2].copy_from_slice(&v.to_le_bytes());
            return Ok(());
        }
        bus.write16(va & SEG_MASK[(va >> 29) as usize], v);
        Ok(())
    }

    fn store8(&mut self, bus: &mut dyn R3000Bus, va: u32, v: u8) -> Res<()> {
        self.check(va, 0, EXC_ADES)?;
        if va >= 0xC000_0000 || self.cop0[SR] & SR_ISC != 0 { return Ok(()); }
        if Self::scratch_hit(va) {
            self.scratch[(va & 0x3FF) as usize] = v;
            return Ok(());
        }
        bus.write8(va & SEG_MASK[(va >> 29) as usize], v);
        Ok(())
    }

    // ------------------------------------------------------------------ running

    /// Run one instruction (or take an interrupt). Returns cycles.
    pub fn step(&mut self, bus: &mut dyn R3000Bus) -> u32 {
        self.cur_pc = self.pc;
        self.delay_slot = self.branch_pending;
        self.branch_pending = false;
        let start = self.cycles;

        // interrupts: IEc and a pending, unmasked Cause.IP bit
        if self.cop0[SR] & SR_IEC != 0 && self.cop0[SR] & self.cause() & 0xFF00 != 0 {
            // On the console a GTE command at EPC has already run when the interrupt is
            // taken, and the BIOS's handler skips it on return: run it here first.
            let pc = self.pc;
            if pc & 3 == 0 && pc < 0xC000_0000 && !Self::scratch_hit(pc) && self.cop0[SR] & SR_CU2 != 0 {
                let i = bus.fetch32(pc & SEG_MASK[(pc >> 29) as usize], pc >> 29 != 5);
                if i >> 25 == 0x25 { self.gte.command(i & 0x01FF_FFFF); }
            }
            self.exception(EXC_INT, 0);
            self.cycles += 1;
            return 1;
        }
        let pc = self.pc;
        if pc & 3 != 0 || (self.cop0[SR] & SR_KUC != 0 && pc >= 0x8000_0000) {
            self.addr_fault(EXC_ADEL, pc);
            self.cycles += 1;
            return 1;
        }
        let instr = if Self::scratch_hit(pc) { 0 } else { bus.fetch32(pc & SEG_MASK[(pc >> 29) as usize], pc >> 29 != 5) };
        self.pc = self.next_pc;
        self.next_pc = self.pc.wrapping_add(4);
        if self.execute(bus, instr).is_ok() {
            self.commit_load();
        }
        self.cycles += 1;
        (self.cycles - start) as u32
    }

    /// HI / LO read or a new multiply: wait for the one running
    fn wait_muldiv(&mut self) {
        if self.muldiv_done > self.cycles { self.cycles = self.muldiv_done; }
    }

    fn execute(&mut self, bus: &mut dyn R3000Bus, i: u32) -> Res<()> {
        let op = i >> 26;
        let rs = (i >> 21) & 31;
        let rt = (i >> 16) & 31;
        let rd = (i >> 11) & 31;
        let sa = (i >> 6) & 31;
        let imm = i & 0xFFFF;
        let simm = imm as i16 as i32 as u32;
        let s = self.regs[rs as usize];
        let t = self.regs[rt as usize];
        match op {
            0x00 => match i & 0x3F {
                0x00 => self.set(rd, t << sa),
                0x02 => self.set(rd, t >> sa),
                0x03 => self.set(rd, ((t as i32) >> sa) as u32),
                0x04 => self.set(rd, t << (s & 31)),
                0x06 => self.set(rd, t >> (s & 31)),
                0x07 => self.set(rd, ((t as i32) >> (s & 31)) as u32),
                0x08 => self.jump_to(s),                       // JR
                0x09 => {                                      // JALR
                    let link = self.cur_pc.wrapping_add(8);
                    self.jump_to(s);
                    self.set(rd, link);
                }
                0x0C => return Err(self.fault(EXC_SYSCALL)),
                0x0D => return Err(self.fault(EXC_BREAK)),
                0x10 => { self.wait_muldiv(); self.set(rd, self.hi); }
                0x11 => self.hi = s,
                0x12 => { self.wait_muldiv(); self.set(rd, self.lo); }
                0x13 => self.lo = s,
                0x18 => {                                      // MULT
                    self.wait_muldiv();
                    let r = (s as i32 as i64) * (t as i32 as i64);
                    self.lo = r as u32;
                    self.hi = (r >> 32) as u32;
                    let a = if (s as i32) < 0 { !s } else { s };
                    self.muldiv_done = self.cycles + if a < 0x800 { 6 } else if a < 0x10_0000 { 9 } else { 13 };
                }
                0x19 => {                                      // MULTU
                    self.wait_muldiv();
                    let r = s as u64 * t as u64;
                    self.lo = r as u32;
                    self.hi = (r >> 32) as u32;
                    self.muldiv_done = self.cycles + if s < 0x800 { 6 } else if s < 0x10_0000 { 9 } else { 13 };
                }
                0x1A => {                                      // DIV
                    self.wait_muldiv();
                    let (n, d) = (s as i32, t as i32);
                    if d == 0 {
                        self.hi = n as u32;
                        self.lo = if n >= 0 { 0xFFFF_FFFF } else { 1 };
                    } else if n == i32::MIN && d == -1 {
                        self.hi = 0;
                        self.lo = 0x8000_0000;
                    } else {
                        self.hi = (n % d) as u32;
                        self.lo = (n / d) as u32;
                    }
                    self.muldiv_done = self.cycles + 36;
                }
                0x1B => {                                      // DIVU
                    self.wait_muldiv();
                    if t == 0 {
                        self.hi = s;
                        self.lo = 0xFFFF_FFFF;
                    } else {
                        self.hi = s % t;
                        self.lo = s / t;
                    }
                    self.muldiv_done = self.cycles + 36;
                }
                0x20 => match (s as i32).checked_add(t as i32) {
                    Some(v) => self.set(rd, v as u32),
                    None => return Err(self.fault(EXC_OV)),
                },
                0x21 => self.set(rd, s.wrapping_add(t)),
                0x22 => match (s as i32).checked_sub(t as i32) {
                    Some(v) => self.set(rd, v as u32),
                    None => return Err(self.fault(EXC_OV)),
                },
                0x23 => self.set(rd, s.wrapping_sub(t)),
                0x24 => self.set(rd, s & t),
                0x25 => self.set(rd, s | t),
                0x26 => self.set(rd, s ^ t),
                0x27 => self.set(rd, !(s | t)),
                0x2A => self.set(rd, ((s as i32) < (t as i32)) as u32),
                0x2B => self.set(rd, (s < t) as u32),
                _ => return Err(self.fault(EXC_RI)),
            },
            0x01 => {                                          // BLTZ / BGEZ (/ AL)
                let ge = rt & 1 != 0;
                let take = ((s as i32) < 0) != ge;
                // only rt = 10 / 11 link (the others in the group act as plain BLTZ / BGEZ)
                if rt & 0x1E == 0x10 { self.set(31, self.cur_pc.wrapping_add(8)); }
                self.branch(take, simm);
            }
            0x02 => { self.branch_pending = true; self.next_pc = (self.cur_pc.wrapping_add(4) & 0xF000_0000) | (i & 0x03FF_FFFF) << 2; }
            0x03 => {
                self.branch_pending = true;
                let link = self.cur_pc.wrapping_add(8);
                self.next_pc = (self.cur_pc.wrapping_add(4) & 0xF000_0000) | (i & 0x03FF_FFFF) << 2;
                self.set(31, link);
            }
            0x04 => self.branch(s == t, simm),
            0x05 => self.branch(s != t, simm),
            0x06 => self.branch((s as i32) <= 0, simm),
            0x07 => self.branch((s as i32) > 0, simm),
            0x08 => match (s as i32).checked_add(simm as i32) {
                Some(v) => self.set(rt, v as u32),
                None => return Err(self.fault(EXC_OV)),
            },
            0x09 => self.set(rt, s.wrapping_add(simm)),
            0x0A => self.set(rt, ((s as i32) < (simm as i32)) as u32),
            0x0B => self.set(rt, (s < simm) as u32),
            0x0C => self.set(rt, s & imm),
            0x0D => self.set(rt, s | imm),
            0x0E => self.set(rt, s ^ imm),
            0x0F => self.set(rt, imm << 16),
            0x10 => return self.cop0_op(i, rs, rt, rd),
            0x11 | 0x13 => return Err({ self.exception(EXC_CPU, op & 3); Fault }),
            0x12 => return self.cop2_op(i, rs, rt, rd),
            0x20 => { let v = self.load8(bus, s.wrapping_add(simm))? as i8 as i32 as u32; self.set_delayed(rt, v); }
            0x21 => { let v = self.load16(bus, s.wrapping_add(simm))? as i16 as i32 as u32; self.set_delayed(rt, v); }
            0x22 | 0x26 => {                                   // LWL / LWR
                let va = s.wrapping_add(simm);
                let w = self.load32(bus, va & !3)?;
                let cur = if self.load.0 as u32 == rt { self.load.1 } else { t };
                let sh = (va & 3) * 8;
                let v = if op == 0x22 {
                    (cur & (0x00FF_FFFF >> sh)) | (w << (24 - sh))
                } else {
                    (cur & !(0xFFFF_FFFFu32 >> sh)) | (w >> sh)
                };
                self.set_delayed(rt, v);
            }
            0x23 => { let v = self.load32(bus, s.wrapping_add(simm))?; self.set_delayed(rt, v); }
            0x24 => { let v = self.load8(bus, s.wrapping_add(simm))? as u32; self.set_delayed(rt, v); }
            0x25 => { let v = self.load16(bus, s.wrapping_add(simm))? as u32; self.set_delayed(rt, v); }
            0x28 => self.store8(bus, s.wrapping_add(simm), t as u8)?,
            0x29 => self.store16(bus, s.wrapping_add(simm), t as u16)?,
            0x2A | 0x2E => {                                   // SWL / SWR
                let va = s.wrapping_add(simm);
                let mem = self.load32(bus, va & !3)?;
                let sh = (va & 3) * 8;
                let v = if op == 0x2A {
                    (mem & !(0xFFFF_FFFFu32 >> (24 - sh))) | (t >> (24 - sh))
                } else {
                    (mem & !(0xFFFF_FFFFu32 << sh)) | (t << sh)
                };
                self.store32(bus, va & !3, v)?;
            }
            0x2B => self.store32(bus, s.wrapping_add(simm), t)?,
            0x32 => {                                          // LWC2
                self.cop2_usable()?;
                let v = self.load32(bus, s.wrapping_add(simm))?;
                self.gte.write_data(rt as usize, v);
            }
            0x3A => {                                          // SWC2
                self.cop2_usable()?;
                let v = self.gte.read_data(rt as usize);
                self.store32(bus, s.wrapping_add(simm), v)?;
            }
            0x30 | 0x31 | 0x33 | 0x38 | 0x39 | 0x3B => {
                // LWC0/1/3, SWC0/1/3: coprocessor unusable (0 and 1 always, 3 is absent)
                return Err({ self.exception(EXC_CPU, op & 3); Fault });
            }
            _ => return Err(self.fault(EXC_RI)),
        }
        Ok(())
    }

    fn branch(&mut self, take: bool, off: u32) {
        self.branch_pending = true;
        if take { self.next_pc = self.cur_pc.wrapping_add(4).wrapping_add(off << 2); }
    }

    fn jump_to(&mut self, target: u32) {
        self.branch_pending = true;
        self.next_pc = target;
    }

    fn cop0_op(&mut self, i: u32, rs: u32, rt: u32, rd: u32) -> Res<()> {
        if self.cop0[SR] & SR_KUC != 0 && self.cop0[SR] & SR_CU0 == 0 {
            self.exception(EXC_CPU, 0);
            return Err(Fault);
        }
        match rs {
            0x00 | 0x02 => {                                   // MFC0 (CFC0 reads the same)
                let v = match rd as usize {
                    CAUSE => self.cause(),
                    r => self.cop0[r],
                };
                self.set_delayed(rt, v);
            }
            0x04 | 0x06 => {                                   // MTC0
                let v = self.regs[rt as usize];
                match rd as usize {
                    CAUSE => self.cop0[CAUSE] = (self.cop0[CAUSE] & !0x300) | (v & 0x300),
                    SR => self.cop0[SR] = v,
                    BADVADDR | PRID | EPC => {}                // read-only
                    r => self.cop0[r] = v,
                }
            }
            0x10..=0x1F if i & 0x3F == 0x10 => {               // RFE
                let sr = self.cop0[SR];
                self.cop0[SR] = (sr & !0xF) | ((sr >> 2) & 0xF);
            }
            _ => { self.exception(EXC_RI, 0); return Err(Fault); }
        }
        Ok(())
    }

    fn cop2_usable(&mut self) -> Res<()> {
        if self.cop0[SR] & SR_CU2 == 0 {
            self.exception(EXC_CPU, 2);
            return Err(Fault);
        }
        Ok(())
    }

    fn cop2_op(&mut self, i: u32, rs: u32, rt: u32, rd: u32) -> Res<()> {
        self.cop2_usable()?;
        if rs & 0x10 != 0 {
            self.gte.command(i & 0x01FF_FFFF);
            return Ok(());
        }
        match rs {
            0x00 => { let v = self.gte.read_data(rd as usize); self.set_delayed(rt, v); }
            0x02 => { let v = self.gte.read_ctrl(rd as usize); self.set_delayed(rt, v); }
            0x04 => self.gte.write_data(rd as usize, self.regs[rt as usize]),
            0x06 => self.gte.write_ctrl(rd as usize, self.regs[rt as usize]),
            _ => { self.exception(EXC_RI, 0); return Err(Fault); }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
