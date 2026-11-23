//! Toshiba / Sony R5900 "Emotion Engine" core (PS2, 294.912 MHz): MIPS III plus some MIPS
//! IV, 128-bit general registers with the MMI SIMD instructions (`mmi.rs`), COP0 with a
//! 48-entry TLB, the non-IEEE single-precision FPU (`fpu.rs`) and a 16 KB scratchpad.
//! Written from the EE Core user's / instruction manuals as summarised by ps2tek.
//!
//! - Branch delay as in `r3000`: `pc` is the instruction about to run, `next_pc` the one
//!   after; a branch rewrites `next_pc`. A *branch likely* that isn't taken skips its delay
//!   slot. There are no load delay slots.
//! - Registers are 128 bits. Ordinary instructions write the low 64 (32-bit results are
//!   sign-extended to 64) and leave the high 64 alone; MMI and LQ write all 128.
//!   LO / HI are 128 bits too: MULT1 / DIV1 and friends use their high halves.
//! - Addresses: KSEG0 (8000_0000) and KSEG1 (A000_0000) map straight to physical memory;
//!   KUSEG and KSEG2/3 go through the TLB, flattened into a 4 KB page table (`map`). A
//!   TLB entry with EntryLo0.S maps the scratchpad (16 KB, inside the CPU).
//! - Exceptions: TLB refill at base + 000, common at + 180, interrupt at + 200; base is
//!   8000_0000 or (Status.BEV) BFC0_0200. ERET returns from EPC, or ErrorEPC when ERL.
//! - Interrupts: Cause.IP2 = INTC (INT0), IP3 = DMAC (INT1), IP7 = Count / Compare; taken
//!   when Status IE, EIE, not EXL / ERL, and the IP bit unmasked.
//! - COP2 (VU0 in macro mode) is reached through the bus (`EeBus::cop2_*`), so the PS2 side
//!   owns the VU.
//! - Timing: one cycle an instruction (the EE dual-issues; this is a rough average).
//!   Caches aren't modelled: CACHE and SYNC do nothing.

// the PS2 system (stage 2) is the first user
#![allow(dead_code)]

use serde::{Deserialize, Serialize};

pub mod fpu;
mod mmi;
#[cfg(test)]
mod tests;

#[allow(unused_imports)]
pub use fpu::{ps2_add, ps2_div, ps2_mul, ps2_sub, to_f64, from_f64};

/// Physical addresses (29 bits).
pub trait EeBus {
    fn read8(&mut self, pa: u32) -> u8;
    fn read16(&mut self, pa: u32) -> u16;
    fn read32(&mut self, pa: u32) -> u32;
    fn read64(&mut self, pa: u32) -> u64 { self.read32(pa) as u64 | (self.read32(pa + 4) as u64) << 32 }
    fn read128(&mut self, pa: u32) -> u128 { self.read64(pa) as u128 | (self.read64(pa + 8) as u128) << 64 }
    fn write8(&mut self, pa: u32, v: u8);
    fn write16(&mut self, pa: u32, v: u16);
    fn write32(&mut self, pa: u32, v: u32);
    fn write64(&mut self, pa: u32, v: u64) { self.write32(pa, v as u32); self.write32(pa + 4, (v >> 32) as u32); }
    fn write128(&mut self, pa: u32, v: u128) { self.write64(pa, v as u64); self.write64(pa + 8, (v >> 64) as u64); }
    fn fetch32(&mut self, pa: u32) -> u32 { self.read32(pa) }
    /// BC0F / BC0T test CPCOND0 (the DMAC: all channels named in D_PCR finished)
    fn cpcond0(&mut self) -> bool { true }
    /// A COP2 (VU0 macro mode) instruction other than LQC2 / SQC2: QMFC2 / QMTC2 / CFC2 /
    /// CTC2 read and write `gpr`. Returns extra cycles.
    fn cop2_op(&mut self, op: u32, gpr: &mut [u128; 32]) -> u32 { let _ = (op, gpr); 0 }
    /// BC2F / BC2T test this (VU0 running)
    fn cop2_cond(&mut self) -> bool { false }
    fn cop2_vf_read(&mut self, reg: u32) -> u128 { let _ = reg; 0 }
    fn cop2_vf_write(&mut self, reg: u32, v: u128) { let _ = (reg, v); }
}

// COP0 registers
pub const INDEX: usize = 0;
pub const RANDOM: usize = 1;
pub const ENTRYLO0: usize = 2;
pub const ENTRYLO1: usize = 3;
pub const CONTEXT: usize = 4;
pub const PAGEMASK: usize = 5;
pub const WIRED: usize = 6;
pub const BADVADDR: usize = 8;
pub const COUNT: usize = 9;
pub const ENTRYHI: usize = 10;
pub const COMPARE: usize = 11;
pub const STATUS: usize = 12;
pub const CAUSE: usize = 13;
pub const EPC: usize = 14;
pub const PRID: usize = 15;
pub const CONFIG: usize = 16;
pub const BADPADDR: usize = 23;
pub const ERROREPC: usize = 30;

// exception codes
pub const EXC_INT: u32 = 0;
pub const EXC_MOD: u32 = 1;
pub const EXC_TLBL: u32 = 2;
pub const EXC_TLBS: u32 = 3;
pub const EXC_ADEL: u32 = 4;
pub const EXC_ADES: u32 = 5;
pub const EXC_SYSCALL: u32 = 8;
pub const EXC_BREAK: u32 = 9;
pub const EXC_RI: u32 = 10;
pub const EXC_CPU: u32 = 11;
pub const EXC_OV: u32 = 12;
pub const EXC_TRAP: u32 = 13;

pub const ST_IE: u32 = 1;
pub const ST_EXL: u32 = 1 << 1;
pub const ST_ERL: u32 = 1 << 2;
pub const ST_EIE: u32 = 1 << 16;
pub const ST_BEV: u32 = 1 << 22;
pub const ST_CU1: u32 = 1 << 29;

/// Cause.IP bits: INTC, DMAC, the COP0 timer
pub const IP_INT0: u32 = 1 << 10;
pub const IP_INT1: u32 = 1 << 11;
pub const IP_TIMER: u32 = 1 << 15;

const TRACE_LEN: usize = 256;

// page-table entry flags (the low 12 bits; the high 20 are the physical page)
const PG_MAPPED: u32 = 1;
const PG_VALID: u32 = 2;
const PG_DIRTY: u32 = 4;
const PG_SCRATCH: u32 = 8;

#[derive(Clone, Copy, Default, Serialize, Deserialize, Debug, PartialEq)]
pub struct TlbEntry {
    pub mask: u32,
    pub hi: u32,
    pub lo0: u32,
    pub lo1: u32,
}

impl TlbEntry {
    /// The size of each of its two pages
    fn page_size(&self) -> u32 { ((self.mask >> 13 & 0xFFF) + 1) << 12 }
    fn global(&self) -> bool { self.lo0 & self.lo1 & 1 != 0 }
    fn vbase(&self) -> u32 { self.hi & !((self.mask | 0x1FFF) & 0x01FF_FFFF) }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct R5900 {
    pub gpr: [u128; 32],
    pub hi: u128,
    pub lo: u128,
    /// QFSRV's shift, in bytes
    pub sa: u32,
    pub pc: u32,
    pub next_pc: u32,
    cur_pc: u32,
    branch_pending: bool,
    delay_slot: bool,
    pub cop0: [u32; 32],
    /// COP1: the registers (raw bits), the accumulator, FCR31
    pub fpr: [u32; 32],
    pub acc: u32,
    pub fcr31: u32,
    pub tlb: Vec<TlbEntry>,
    /// the TLB flattened to 4 KB pages (rebuilt on load: `rebuild_tlb`)
    #[serde(skip)]
    map: Vec<u32>,
    #[serde(skip)]
    mapped: Vec<(u32, u32)>,
    pub scratch: Vec<u8>,
    /// INT0 / INT1 lines (Cause.IP2 / IP3)
    irq: u32,
    pub cycles: u64,
    #[serde(skip)]
    trace: Vec<(u32, u32, u32, u32)>,
    #[serde(skip)]
    trace_pos: usize,
    #[serde(skip)]
    pub trace_dumped: bool,
    /// debugging: the last exception taken (code, EPC, BadVAddr)
    #[serde(skip)]
    pub last_exception: Option<(u32, u32, u32)>,
}

impl Default for R5900 {
    fn default() -> Self { Self::new() }
}

/// A fault inside an instruction: the exception has been taken, stop executing it.
pub(crate) struct Fault;
pub(crate) type Res<T> = Result<T, Fault>;

/// Where a virtual address lands
enum Target {
    Phys(u32),
    Scratch(usize),
}

#[inline]
fn sext32(v: u32) -> u64 { v as i32 as i64 as u64 }

impl R5900 {
    pub fn new() -> Self {
        let mut c = Self {
            gpr: [0; 32], hi: 0, lo: 0, sa: 0,
            pc: 0, next_pc: 0, cur_pc: 0, branch_pending: false, delay_slot: false,
            cop0: [0; 32], fpr: [0; 32], acc: 0, fcr31: 0,
            tlb: vec![TlbEntry::default(); 48],
            map: vec![0; 1 << 20], mapped: Vec::new(),
            scratch: vec![0; 0x4000], irq: 0, cycles: 0,
            trace: Vec::new(), trace_pos: 0, trace_dumped: false, last_exception: None,
        };
        c.reset();
        c
    }

    pub fn reset(&mut self) {
        self.pc = 0xBFC0_0000;
        self.next_pc = self.pc + 4;
        self.branch_pending = false;
        self.cop0 = [0; 32];
        self.cop0[STATUS] = ST_ERL | ST_BEV;
        self.cop0[PRID] = 0x0000_2E20;
        self.cop0[CONFIG] = 0x0000_0440;
        self.cop0[RANDOM] = 47;
        self.fcr31 = 0x0100_0000;
        self.tlb = vec![TlbEntry::default(); 48];
        self.rebuild_tlb();
    }

    /// Start running at `pc` (tests, or an ELF side-load)
    pub fn jump(&mut self, pc: u32) {
        self.pc = pc;
        self.next_pc = pc.wrapping_add(4);
        self.branch_pending = false;
    }

    /// The INTC's (INT0) and DMAC's (INT1) lines
    pub fn set_int0(&mut self, on: bool) { self.irq = (self.irq & !IP_INT0) | if on { IP_INT0 } else { 0 }; }
    pub fn set_int1(&mut self, on: bool) { self.irq = (self.irq & !IP_INT1) | if on { IP_INT1 } else { 0 }; }

    pub fn cause(&self) -> u32 { self.cop0[CAUSE] | self.irq }

    // ------------------------------------------------------------------ registers

    #[inline]
    pub fn r64(&self, r: u32) -> u64 { self.gpr[r as usize] as u64 }
    #[inline]
    fn r32(&self, r: u32) -> u32 { self.gpr[r as usize] as u32 }
    /// the low 64 bits; the high 64 stay
    #[inline]
    fn set64(&mut self, r: u32, v: u64) {
        if r != 0 { let g = &mut self.gpr[r as usize]; *g = (*g & !(u64::MAX as u128)) | v as u128; }
    }
    #[inline]
    fn set32(&mut self, r: u32, v: u32) { self.set64(r, sext32(v)); }
    #[inline]
    fn set128(&mut self, r: u32, v: u128) { if r != 0 { self.gpr[r as usize] = v; } }

    fn lo64(&self, half: usize) -> u64 { (self.lo >> (64 * half)) as u64 }
    fn hi64(&self, half: usize) -> u64 { (self.hi >> (64 * half)) as u64 }
    fn set_lo64(&mut self, half: usize, v: u64) {
        let s = 64 * half;
        self.lo = (self.lo & !((u64::MAX as u128) << s)) | (v as u128) << s;
    }
    fn set_hi64(&mut self, half: usize, v: u64) {
        let s = 64 * half;
        self.hi = (self.hi & !((u64::MAX as u128) << s)) | (v as u128) << s;
    }

    // ------------------------------------------------------------------ exceptions

    fn kernel(&self) -> bool { let s = self.cop0[STATUS]; s & (ST_EXL | ST_ERL) != 0 || s >> 3 & 3 == 0 }

    /// A level 1 exception (`refill`: a TLB miss, which has its own vector unless EXL)
    pub fn exception_l1(&mut self, code: u32, refill: bool) {
        if !matches!(code, EXC_INT | EXC_SYSCALL | EXC_BREAK) {
            self.dump_trace(&format!("exception {code} at {:08X} (BadVAddr {:08X})", self.cur_pc, self.cop0[BADVADDR]), 64);
        }
        let st = self.cop0[STATUS];
        let mut cause = self.cop0[CAUSE] & !(0x8000_0000 | 0x7C | 0x3000_0000);
        cause |= code << 2;
        let off = if st & ST_EXL == 0 {
            self.cop0[EPC] = if self.delay_slot {
                cause |= 0x8000_0000;
                self.cur_pc.wrapping_sub(4)
            } else {
                self.cur_pc
            };
            if refill { 0x000 } else if code == EXC_INT { 0x200 } else { 0x180 }
        } else if code == EXC_INT { 0x200 } else { 0x180 };
        self.cop0[CAUSE] = cause;
        self.cop0[STATUS] = st | ST_EXL;
        let base = if st & ST_BEV != 0 { 0xBFC0_0200 } else { 0x8000_0000 };
        self.last_exception = Some((code, self.cop0[EPC], self.cop0[BADVADDR]));
        self.jump(base + off);
    }

    fn fault(&mut self, code: u32) -> Fault { self.exception_l1(code, false); Fault }

    /// A coprocessor-unusable fault: Cause.CE = the unit
    fn cop_unusable(&mut self, unit: u32) -> Fault {
        self.exception_l1(EXC_CPU, false);
        self.cop0[CAUSE] = (self.cop0[CAUSE] & !0x3000_0000) | (unit & 3) << 28;
        Fault
    }

    fn tlb_fault(&mut self, code: u32, va: u32, refill: bool) -> Fault {
        self.cop0[BADVADDR] = va;
        self.cop0[CONTEXT] = (self.cop0[CONTEXT] & 0xFF80_0000) | (va >> 9 & 0x007F_FFF0);
        self.cop0[ENTRYHI] = (va & 0xFFFF_E000) | (self.cop0[ENTRYHI] & 0xFF);
        self.exception_l1(code, refill);
        Fault
    }

    fn addr_fault(&mut self, code: u32, va: u32) -> Fault {
        self.cop0[BADVADDR] = va;
        self.fault(code)
    }

    /// Debugging: keep the last instructions (for `dump_trace`), or stop
    pub fn set_trace(&mut self, on: bool) {
        self.trace = if on { vec![(0, 0, 0, 0); TRACE_LEN] } else { Vec::new() };
        self.trace_pos = 0;
    }

    pub fn dump_trace(&mut self, why: &str, n: usize) {
        if self.trace_dumped || self.trace.len() != TRACE_LEN { return; }
        self.trace_dumped = true;
        crate::dlog!("R5900: {why}; the last {n} instructions (pc instr sp ra):");
        let n = n.min(TRACE_LEN);
        for k in TRACE_LEN - n..TRACE_LEN {
            let (pc, i, sp, ra) = self.trace[(self.trace_pos + k) % TRACE_LEN];
            crate::dlog!("  {pc:08X} {i:08X} sp {sp:08X} ra {ra:08X}");
        }
    }

    /// Debugging: the last `n` instructions as (pc, instr), oldest first (empty with the
    /// trace off)
    pub fn recent(&self, n: usize) -> Vec<(u32, u32)> {
        if self.trace.len() != TRACE_LEN { return Vec::new(); }
        let n = n.min(TRACE_LEN);
        (TRACE_LEN - n..TRACE_LEN).map(|k| { let t = self.trace[(self.trace_pos + k) % TRACE_LEN]; (t.0, t.1) }).collect()
    }

    // ------------------------------------------------------------------ TLB

    /// Flatten the TLB into the page table (after a TLB write, an ASID change, or a load)
    pub fn rebuild_tlb(&mut self) {
        if self.map.len() != 1 << 20 { self.map = vec![0; 1 << 20]; self.mapped.clear(); }
        for &(a, b) in &self.mapped { self.map[a as usize..b as usize].fill(0); }
        self.mapped.clear();
        let asid = self.cop0[ENTRYHI] & 0xFF;
        // the lowest index wins where entries overlap (fill from the top down)
        for e in self.tlb.iter().rev() {
            if !e.global() && e.hi & 0xFF != asid { continue; }
            // a scratchpad entry (EntryLo0.S) always maps the whole 16 KB, whatever
            // PageMask says (the Sony BIOS sets PageMask 0)
            let sp = e.lo0 >> 31 != 0;
            let size = if sp { 0x4000 } else { e.page_size() };
            let base = if sp { e.hi & 0xFFFF_C000 } else { e.vbase() };
            for (k, lo) in [e.lo0, e.lo1].into_iter().enumerate() {
                let vstart = base.wrapping_add(size * k as u32);
                // KSEG0 / KSEG1 are never mapped
                if (0x8000_0000..0xC000_0000).contains(&vstart) { continue; }
                let scratch = k == 0 && e.lo0 >> 31 != 0;
                if k == 1 && e.lo0 >> 31 != 0 { continue; }
                let pfn = (lo >> 6 & 0xF_FFFF) << 12;
                let mut flags = PG_MAPPED;
                if lo & 2 != 0 { flags |= PG_VALID; }
                if lo & 4 != 0 { flags |= PG_DIRTY; }
                if scratch { flags |= PG_SCRATCH; }
                let first = vstart >> 12;
                let pages = size >> 12;
                for p in 0..pages {
                    let pa = if scratch { p << 12 & 0x3000 } else { pfn.wrapping_add(p << 12) & 0xFFFF_F000 };
                    self.map[(first + p) as usize & 0xF_FFFF] = pa | flags;
                }
                self.mapped.push((first, (first + pages).min(1 << 20)));
            }
        }
    }

    #[inline]
    fn translate(&mut self, va: u32, store: bool) -> Res<Target> {
        let seg = va >> 29;
        if seg == 4 || seg == 5 { return Ok(Target::Phys(va & 0x1FFF_FFFF)); }
        let e = self.map[(va >> 12) as usize];
        let code = if store { EXC_TLBS } else { EXC_TLBL };
        if e & PG_MAPPED == 0 { return Err(self.tlb_fault(code, va, true)); }
        if e & PG_VALID == 0 { return Err(self.tlb_fault(code, va, false)); }
        if store && e & PG_DIRTY == 0 { return Err(self.tlb_fault(EXC_MOD, va, false)); }
        if e & PG_SCRATCH != 0 { return Ok(Target::Scratch(((e & 0x3000) | (va & 0xFFF)) as usize)); }
        Ok(Target::Phys((e & 0xFFFF_F000) | (va & 0xFFF)))
    }

    /// The physical address a virtual one maps to, if any (debugging, DMA from the CPU's view)
    pub fn virt_to_phys(&self, va: u32) -> Option<u32> {
        let seg = va >> 29;
        if seg == 4 || seg == 5 { return Some(va & 0x1FFF_FFFF); }
        let e = self.map[(va >> 12) as usize];
        if e & PG_MAPPED == 0 || e & PG_SCRATCH != 0 { return None; }
        Some((e & 0xFFFF_F000) | (va & 0xFFF))
    }

    fn tlb_write(&mut self, i: usize) {
        let i = i % 48;
        self.tlb[i] = TlbEntry {
            mask: self.cop0[PAGEMASK] & 0x01FF_E000,
            hi: self.cop0[ENTRYHI] & !(self.cop0[PAGEMASK] & 0x01FF_E000) & 0xFFFF_E0FF,
            lo0: self.cop0[ENTRYLO0] & 0x83FF_FFFF,
            lo1: self.cop0[ENTRYLO1] & 0x03FF_FFFF,
        };
        self.rebuild_tlb();
    }

    // ------------------------------------------------------------------ memory

    fn check(&mut self, va: u32, align: u32, code: u32) -> Res<()> {
        if va & align != 0 { return Err(self.addr_fault(code, va)); }
        Ok(())
    }

    fn sp_read(&self, o: usize, n: usize) -> u128 {
        let mut b = [0u8; 16];
        b[..n].copy_from_slice(&self.scratch[o..o + n]);
        u128::from_le_bytes(b)
    }

    fn sp_write(&mut self, o: usize, n: usize, v: u128) {
        self.scratch[o..o + n].copy_from_slice(&v.to_le_bytes()[..n]);
    }

    pub(crate) fn load(&mut self, bus: &mut dyn EeBus, va: u32, n: u32) -> Res<u128> {
        self.check(va, n - 1, EXC_ADEL)?;
        Ok(match self.translate(va, false)? {
            Target::Scratch(o) => self.sp_read(o & (0x4000 - n as usize), n as usize),
            Target::Phys(pa) => match n {
                1 => bus.read8(pa) as u128,
                2 => bus.read16(pa) as u128,
                4 => bus.read32(pa) as u128,
                8 => bus.read64(pa) as u128,
                _ => bus.read128(pa),
            },
        })
    }

    pub(crate) fn store(&mut self, bus: &mut dyn EeBus, va: u32, n: u32, v: u128) -> Res<()> {
        self.check(va, n - 1, EXC_ADES)?;
        match self.translate(va, true)? {
            Target::Scratch(o) => self.sp_write(o & (0x4000 - n as usize), n as usize, v),
            Target::Phys(pa) => match n {
                1 => bus.write8(pa, v as u8),
                2 => bus.write16(pa, v as u16),
                4 => bus.write32(pa, v as u32),
                8 => bus.write64(pa, v as u64),
                _ => bus.write128(pa, v),
            },
        }
        Ok(())
    }

    // ------------------------------------------------------------------ running

    /// Run one instruction (or take an interrupt). Returns cycles.
    pub fn step(&mut self, bus: &mut dyn EeBus) -> u32 {
        self.cur_pc = self.pc;
        self.delay_slot = self.branch_pending;
        self.branch_pending = false;
        let cycles = self.step_inner(bus);
        self.advance(cycles);
        cycles
    }

    /// Time passing: the COP0 timer (Count / Compare) and the cycle count
    #[inline]
    fn advance(&mut self, cycles: u32) {
        let old = self.cop0[COUNT];
        self.cop0[COUNT] = old.wrapping_add(cycles);
        let cmp = self.cop0[COMPARE];
        if cmp.wrapping_sub(old.wrapping_add(1)) < cycles { self.cop0[CAUSE] |= IP_TIMER; }
        self.cycles += cycles as u64;
    }

    /// `cycles` go by without running anything (the CPU is in an idle loop that only an
    /// interrupt ends)
    pub fn idle(&mut self, cycles: u32) { self.advance(cycles); }

    /// The next step would take an interrupt
    pub fn irq_ready(&self) -> bool {
        let st = self.cop0[STATUS];
        st & (ST_IE | ST_EIE) == ST_IE | ST_EIE && st & (ST_EXL | ST_ERL) == 0
            && st & self.cause() & (IP_INT0 | IP_INT1 | IP_TIMER) != 0
    }

    fn step_inner(&mut self, bus: &mut dyn EeBus) -> u32 {
        let st = self.cop0[STATUS];
        if st & (ST_IE | ST_EIE) == ST_IE | ST_EIE && st & (ST_EXL | ST_ERL) == 0
            && st & self.cause() & (IP_INT0 | IP_INT1 | IP_TIMER) != 0
        {
            self.exception_l1(EXC_INT, false);
            return 1;
        }
        let pc = self.pc;
        if pc & 3 != 0 { let _ = self.addr_fault(EXC_ADEL, pc); return 1; }
        let instr = match self.translate(pc, false) {
            Ok(Target::Phys(pa)) => bus.fetch32(pa),
            Ok(Target::Scratch(o)) => self.sp_read(o & 0x3FFC, 4) as u32,
            Err(_) => return 1,
        };
        if !self.trace.is_empty() {
            self.trace[self.trace_pos] = (pc, instr, self.r32(29), self.r32(31));
            self.trace_pos = (self.trace_pos + 1) % TRACE_LEN;
        }
        self.pc = self.next_pc;
        self.next_pc = self.pc.wrapping_add(4);
        match self.execute(bus, instr) {
            Ok(c) => c,
            Err(Fault) => 1,
        }
    }

    // ------------------------------------------------------------------ branches

    #[inline]
    fn branch(&mut self, taken: bool, instr: u32) {
        self.branch_pending = true;
        if taken {
            let off = ((instr as i16 as i32) << 2) as u32;
            self.next_pc = self.cur_pc.wrapping_add(4).wrapping_add(off);
        }
    }

    /// A branch likely: not taken, the delay slot is skipped
    #[inline]
    fn branch_likely(&mut self, taken: bool, instr: u32) {
        if taken {
            self.branch(true, instr);
        } else {
            self.pc = self.next_pc;
            self.next_pc = self.pc.wrapping_add(4);
        }
    }

    #[inline]
    fn link(&mut self, r: u32) { self.set64(r, self.cur_pc.wrapping_add(8) as i32 as i64 as u64); }

    // ------------------------------------------------------------------ the instructions

    fn execute(&mut self, bus: &mut dyn EeBus, i: u32) -> Res<u32> {
        let op = i >> 26;
        let rs = i >> 21 & 31;
        let rt = i >> 16 & 31;
        let rd = i >> 11 & 31;
        let sa = i >> 6 & 31;
        let imm = i as i16 as i64 as u64;
        let uimm = (i & 0xFFFF) as u64;
        match op {
            0x00 => return self.special(i, rs, rt, rd, sa),
            0x01 => return self.regimm(i, rs, rt),
            0x02 | 0x03 => {
                if op == 3 { self.link(31); }
                self.next_pc = (self.cur_pc.wrapping_add(4) & 0xF000_0000) | (i & 0x03FF_FFFF) << 2;
                self.branch_pending = true;
            }
            0x04 => self.branch(self.r64(rs) == self.r64(rt), i),
            0x05 => self.branch(self.r64(rs) != self.r64(rt), i),
            0x06 => self.branch(self.r64(rs) as i64 <= 0, i),
            0x07 => self.branch(self.r64(rs) as i64 > 0, i),
            0x08 => {
                let (v, o) = (self.r32(rs) as i32).overflowing_add(imm as i32);
                if o { return Err(self.fault(EXC_OV)); }
                self.set32(rt, v as u32);
            }
            0x09 => self.set32(rt, self.r32(rs).wrapping_add(imm as u32)),
            0x0A => self.set64(rt, ((self.r64(rs) as i64) < imm as i64) as u64),
            0x0B => self.set64(rt, (self.r64(rs) < imm) as u64),
            0x0C => self.set64(rt, self.r64(rs) & uimm),
            0x0D => self.set64(rt, self.r64(rs) | uimm),
            0x0E => self.set64(rt, self.r64(rs) ^ uimm),
            0x0F => self.set32(rt, (i & 0xFFFF) << 16),
            0x10 => return self.cop0_op(bus, i, rs, rt, rd),
            0x11 => {
                if self.cop0[STATUS] & ST_CU1 == 0 { return Err(self.cop_unusable(1)); }
                return self.cop1(i, rs, rt);
            }
            0x12 => return self.cop2(bus, i, rs),
            0x14 => self.branch_likely(self.r64(rs) == self.r64(rt), i),
            0x15 => self.branch_likely(self.r64(rs) != self.r64(rt), i),
            0x16 => self.branch_likely(self.r64(rs) as i64 <= 0, i),
            0x17 => self.branch_likely(self.r64(rs) as i64 > 0, i),
            0x18 => {
                let (v, o) = (self.r64(rs) as i64).overflowing_add(imm as i64);
                if o { return Err(self.fault(EXC_OV)); }
                self.set64(rt, v as u64);
            }
            0x19 => self.set64(rt, self.r64(rs).wrapping_add(imm)),
            0x1A => {
                // LDL
                let a = self.r32(rs).wrapping_add(imm as u32);
                let m = self.load(bus, a & !7, 8)? as u64;
                let s = (a & 7) * 8;
                let keep = if s == 56 { 0 } else { u64::MAX >> (s + 8) };
                self.set64(rt, (self.r64(rt) & keep) | m << (56 - s));
            }
            0x1B => {
                // LDR
                let a = self.r32(rs).wrapping_add(imm as u32);
                let m = self.load(bus, a & !7, 8)? as u64;
                let s = (a & 7) * 8;
                let keep = if s == 0 { 0 } else { u64::MAX << (64 - s) };
                self.set64(rt, (self.r64(rt) & keep) | m >> s);
            }
            0x1C => return self.mmi(i, rs, rt, rd, sa),
            0x1E => {
                let a = self.r32(rs).wrapping_add(imm as u32) & !15;
                let v = self.load(bus, a, 16)?;
                self.set128(rt, v);
            }
            0x1F => {
                let a = self.r32(rs).wrapping_add(imm as u32) & !15;
                self.store(bus, a, 16, self.gpr[rt as usize])?;
            }
            0x20 => { let v = self.load(bus, self.r32(rs).wrapping_add(imm as u32), 1)? as u8; self.set64(rt, v as i8 as i64 as u64); }
            0x21 => { let v = self.load(bus, self.r32(rs).wrapping_add(imm as u32), 2)? as u16; self.set64(rt, v as i16 as i64 as u64); }
            0x22 => {
                // LWL
                let a = self.r32(rs).wrapping_add(imm as u32);
                let m = self.load(bus, a & !3, 4)? as u32;
                let s = (a & 3) * 8;
                let keep = if s == 24 { 0 } else { u32::MAX >> (s + 8) };
                self.set32(rt, (self.r32(rt) & keep) | m << (24 - s));
            }
            0x23 => { let v = self.load(bus, self.r32(rs).wrapping_add(imm as u32), 4)? as u32; self.set32(rt, v); }
            0x24 => { let v = self.load(bus, self.r32(rs).wrapping_add(imm as u32), 1)? as u64; self.set64(rt, v); }
            0x25 => { let v = self.load(bus, self.r32(rs).wrapping_add(imm as u32), 2)? as u64; self.set64(rt, v); }
            0x26 => {
                // LWR: a whole word sign-extends; otherwise the high half of the 64 stays
                let a = self.r32(rs).wrapping_add(imm as u32);
                let m = self.load(bus, a & !3, 4)? as u32;
                let s = (a & 3) * 8;
                if s == 0 {
                    self.set32(rt, m);
                } else {
                    let v = (self.r32(rt) & (u32::MAX << (32 - s))) | m >> s;
                    self.set64(rt, (self.r64(rt) & 0xFFFF_FFFF_0000_0000) | v as u64);
                }
            }
            0x27 => { let v = self.load(bus, self.r32(rs).wrapping_add(imm as u32), 4)? as u64; self.set64(rt, v); }
            0x28 => self.store(bus, self.r32(rs).wrapping_add(imm as u32), 1, self.gpr[rt as usize])?,
            0x29 => self.store(bus, self.r32(rs).wrapping_add(imm as u32), 2, self.gpr[rt as usize])?,
            0x2A => {
                // SWL
                let a = self.r32(rs).wrapping_add(imm as u32);
                let m = self.load(bus, a & !3, 4)? as u32;
                let s = (a & 3) * 8;
                let keep = if s == 24 { 0 } else { u32::MAX << (s + 8) };
                self.store(bus, a & !3, 4, ((m & keep) | self.r32(rt) >> (24 - s)) as u128)?;
            }
            0x2B => self.store(bus, self.r32(rs).wrapping_add(imm as u32), 4, self.gpr[rt as usize])?,
            0x2C => {
                // SDL
                let a = self.r32(rs).wrapping_add(imm as u32);
                let m = self.load(bus, a & !7, 8)? as u64;
                let s = (a & 7) * 8;
                let keep = if s == 56 { 0 } else { u64::MAX << (s + 8) };
                self.store(bus, a & !7, 8, ((m & keep) | self.r64(rt) >> (56 - s)) as u128)?;
            }
            0x2D => {
                // SDR
                let a = self.r32(rs).wrapping_add(imm as u32);
                let m = self.load(bus, a & !7, 8)? as u64;
                let s = (a & 7) * 8;
                let keep = if s == 0 { 0 } else { u64::MAX >> (64 - s) };
                self.store(bus, a & !7, 8, ((m & keep) | self.r64(rt) << s) as u128)?;
            }
            0x2E => {
                // SWR
                let a = self.r32(rs).wrapping_add(imm as u32);
                let m = self.load(bus, a & !3, 4)? as u32;
                let s = (a & 3) * 8;
                let keep = if s == 0 { 0 } else { u32::MAX >> (32 - s) };
                self.store(bus, a & !3, 4, ((m & keep) | self.r32(rt) << s) as u128)?;
            }
            0x2F => {} // CACHE
            0x31 => {
                if self.cop0[STATUS] & ST_CU1 == 0 { return Err(self.cop_unusable(1)); }
                let v = self.load(bus, self.r32(rs).wrapping_add(imm as u32), 4)? as u32;
                self.fpr[rt as usize] = v;
            }
            0x33 => {} // PREF
            0x36 => {
                // LQC2
                let a = self.r32(rs).wrapping_add(imm as u32) & !15;
                let v = self.load(bus, a, 16)?;
                bus.cop2_vf_write(rt, v);
            }
            0x37 => { let v = self.load(bus, self.r32(rs).wrapping_add(imm as u32), 8)? as u64; self.set64(rt, v); }
            0x39 => {
                if self.cop0[STATUS] & ST_CU1 == 0 { return Err(self.cop_unusable(1)); }
                self.store(bus, self.r32(rs).wrapping_add(imm as u32), 4, self.fpr[rt as usize] as u128)?;
            }
            0x3E => {
                // SQC2
                let a = self.r32(rs).wrapping_add(imm as u32) & !15;
                let v = bus.cop2_vf_read(rt);
                self.store(bus, a, 16, v)?;
            }
            0x3F => self.store(bus, self.r32(rs).wrapping_add(imm as u32), 8, self.gpr[rt as usize])?,
            _ => return Err(self.fault(EXC_RI)),
        }
        Ok(1)
    }

    fn special(&mut self, i: u32, rs: u32, rt: u32, rd: u32, sa: u32) -> Res<u32> {
        let (a, b) = (self.r64(rs), self.r64(rt));
        match i & 0x3F {
            0x00 => self.set32(rd, self.r32(rt) << sa),
            0x02 => self.set32(rd, self.r32(rt) >> sa),
            0x03 => self.set32(rd, ((self.r32(rt) as i32) >> sa) as u32),
            0x04 => self.set32(rd, self.r32(rt) << (self.r32(rs) & 31)),
            0x06 => self.set32(rd, self.r32(rt) >> (self.r32(rs) & 31)),
            0x07 => self.set32(rd, ((self.r32(rt) as i32) >> (self.r32(rs) & 31)) as u32),
            0x08 => { self.next_pc = self.r32(rs); self.branch_pending = true; }
            0x09 => {
                let t = self.r32(rs);
                self.link(rd);
                self.next_pc = t;
                self.branch_pending = true;
            }
            0x0A => if b == 0 { self.set64(rd, a) },
            0x0B => if b != 0 { self.set64(rd, a) },
            0x0C => return Err(self.fault(EXC_SYSCALL)),
            0x0D => return Err(self.fault(EXC_BREAK)),
            0x0F => {} // SYNC
            0x10 => self.set64(rd, self.hi64(0)),
            0x11 => self.set_hi64(0, a),
            0x12 => self.set64(rd, self.lo64(0)),
            0x13 => self.set_lo64(0, a),
            0x14 => self.set64(rd, b << (self.r32(rs) & 63)),
            0x16 => self.set64(rd, b >> (self.r32(rs) & 63)),
            0x17 => self.set64(rd, ((b as i64) >> (self.r32(rs) & 63)) as u64),
            0x18 => { self.mult(0, rd, rs, rt, true); return Ok(2); }
            0x19 => { self.mult(0, rd, rs, rt, false); return Ok(2); }
            0x1A => { self.div(0, rs, rt, true); return Ok(4); }
            0x1B => { self.div(0, rs, rt, false); return Ok(4); }
            0x20 => {
                let (v, o) = (self.r32(rs) as i32).overflowing_add(self.r32(rt) as i32);
                if o { return Err(self.fault(EXC_OV)); }
                self.set32(rd, v as u32);
            }
            0x21 => self.set32(rd, self.r32(rs).wrapping_add(self.r32(rt))),
            0x22 => {
                let (v, o) = (self.r32(rs) as i32).overflowing_sub(self.r32(rt) as i32);
                if o { return Err(self.fault(EXC_OV)); }
                self.set32(rd, v as u32);
            }
            0x23 => self.set32(rd, self.r32(rs).wrapping_sub(self.r32(rt))),
            0x24 => self.set64(rd, a & b),
            0x25 => self.set64(rd, a | b),
            0x26 => self.set64(rd, a ^ b),
            0x27 => self.set64(rd, !(a | b)),
            0x28 => self.set64(rd, self.sa as u64),
            0x29 => self.sa = self.r32(rs) & 0xF,
            0x2A => self.set64(rd, ((a as i64) < b as i64) as u64),
            0x2B => self.set64(rd, (a < b) as u64),
            0x2C => {
                let (v, o) = (a as i64).overflowing_add(b as i64);
                if o { return Err(self.fault(EXC_OV)); }
                self.set64(rd, v as u64);
            }
            0x2D => self.set64(rd, a.wrapping_add(b)),
            0x2E => {
                let (v, o) = (a as i64).overflowing_sub(b as i64);
                if o { return Err(self.fault(EXC_OV)); }
                self.set64(rd, v as u64);
            }
            0x2F => self.set64(rd, a.wrapping_sub(b)),
            0x30 => if a as i64 >= b as i64 { return Err(self.fault(EXC_TRAP)); },
            0x31 => if a >= b { return Err(self.fault(EXC_TRAP)); },
            0x32 => if (a as i64) < b as i64 { return Err(self.fault(EXC_TRAP)); },
            0x33 => if a < b { return Err(self.fault(EXC_TRAP)); },
            0x34 => if a == b { return Err(self.fault(EXC_TRAP)); },
            0x36 => if a != b { return Err(self.fault(EXC_TRAP)); },
            0x38 => self.set64(rd, b << sa),
            0x3A => self.set64(rd, b >> sa),
            0x3B => self.set64(rd, ((b as i64) >> sa) as u64),
            0x3C => self.set64(rd, b << (sa + 32)),
            0x3E => self.set64(rd, b >> (sa + 32)),
            0x3F => self.set64(rd, ((b as i64) >> (sa + 32)) as u64),
            _ => return Err(self.fault(EXC_RI)),
        }
        Ok(1)
    }

    /// MULT / MULTU (and MULT1 / MULTU1 with `half` 1): LO / HI get the product's halves,
    /// sign-extended, and rd gets LO too
    pub(crate) fn mult(&mut self, half: usize, rd: u32, rs: u32, rt: u32, signed: bool) {
        let p = if signed {
            (self.r32(rs) as i32 as i64 * self.r32(rt) as i32 as i64) as u64
        } else {
            self.r32(rs) as u64 * self.r32(rt) as u64
        };
        self.set_lo64(half, sext32(p as u32));
        self.set_hi64(half, sext32((p >> 32) as u32));
        self.set64(rd, sext32(p as u32));
    }

    /// MADD / MADDU (and the "1" forms): {HI, LO} += rs * rt
    pub(crate) fn madd(&mut self, half: usize, rd: u32, rs: u32, rt: u32, signed: bool) {
        let acc = (self.hi64(half) << 32) | (self.lo64(half) & 0xFFFF_FFFF);
        let p = if signed {
            (self.r32(rs) as i32 as i64 * self.r32(rt) as i32 as i64) as u64
        } else {
            self.r32(rs) as u64 * self.r32(rt) as u64
        };
        let r = acc.wrapping_add(p);
        self.set_lo64(half, sext32(r as u32));
        self.set_hi64(half, sext32((r >> 32) as u32));
        self.set64(rd, sext32(r as u32));
    }

    /// DIV / DIVU (and DIV1 / DIVU1). Dividing by zero: LO = -1 (or 1 for a negative
    /// signed dividend), HI = the dividend; 8000_0000 / -1 = 8000_0000 remainder 0
    pub(crate) fn div(&mut self, half: usize, rs: u32, rt: u32, signed: bool) {
        let (n, d) = (self.r32(rs), self.r32(rt));
        let (q, r) = if signed {
            let (n, d) = (n as i32, d as i32);
            if d == 0 { (if n < 0 { 1 } else { -1i32 } as u32, n as u32) }
            else if n == i32::MIN && d == -1 { (n as u32, 0) }
            else { ((n / d) as u32, (n % d) as u32) }
        } else if d == 0 { (u32::MAX, n) } else { (n / d, n % d) };
        self.set_lo64(half, sext32(q));
        self.set_hi64(half, sext32(r));
    }

    fn regimm(&mut self, i: u32, rs: u32, rt: u32) -> Res<u32> {
        let a = self.r64(rs) as i64;
        let imm = i as i16 as i64;
        match rt {
            0x00 => self.branch(a < 0, i),
            0x01 => self.branch(a >= 0, i),
            0x02 => self.branch_likely(a < 0, i),
            0x03 => self.branch_likely(a >= 0, i),
            0x08 => if a >= imm { return Err(self.fault(EXC_TRAP)); },
            0x09 => if a as u64 >= imm as u64 { return Err(self.fault(EXC_TRAP)); },
            0x0A => if a < imm { return Err(self.fault(EXC_TRAP)); },
            0x0B => if (a as u64) < imm as u64 { return Err(self.fault(EXC_TRAP)); },
            0x0C => if a == imm { return Err(self.fault(EXC_TRAP)); },
            0x0E => if a != imm { return Err(self.fault(EXC_TRAP)); },
            0x10 => { self.link(31); self.branch(a < 0, i); }
            0x11 => { self.link(31); self.branch(a >= 0, i); }
            0x12 => { self.link(31); self.branch_likely(a < 0, i); }
            0x13 => { self.link(31); self.branch_likely(a >= 0, i); }
            0x18 => self.sa = (self.r32(rs) ^ i) & 0xF,              // MTSAB
            0x19 => self.sa = ((self.r32(rs) ^ i) & 7) * 2,          // MTSAH
            _ => return Err(self.fault(EXC_RI)),
        }
        Ok(1)
    }

    fn cop0_op(&mut self, bus: &mut dyn EeBus, i: u32, rs: u32, rt: u32, rd: u32) -> Res<u32> {
        if !self.kernel() && self.cop0[STATUS] & (1 << 28) == 0 { return Err(self.cop_unusable(0)); }
        match rs {
            0x00 => {
                // MFC0 (Debug / Perf have sub-registers: read 0)
                let v = match rd as usize {
                    24 | 25 => 0,
                    RANDOM => self.random(),
                    r => self.cop0[r],
                };
                let v = if rd as usize == CAUSE { v | self.irq } else { v };
                self.set32(rt, v);
            }
            0x04 => {
                let v = self.r32(rt);
                let r = rd as usize;
                match r {
                    COMPARE => { self.cop0[COMPARE] = v; self.cop0[CAUSE] &= !IP_TIMER; }
                    CAUSE => self.cop0[CAUSE] = (self.cop0[CAUSE] & !0x300) | (v & 0x300),
                    PRID | RANDOM | BADVADDR => {}
                    ENTRYHI => {
                        let asid = self.cop0[ENTRYHI] & 0xFF;
                        self.cop0[ENTRYHI] = v & 0xFFFF_E0FF;
                        if v & 0xFF != asid && self.tlb.iter().any(|e| !e.global() && e.mask | e.hi | e.lo0 | e.lo1 != 0) {
                            self.rebuild_tlb();
                        }
                    }
                    WIRED => { self.cop0[WIRED] = v & 0x3F; self.cop0[RANDOM] = 47; }
                    24 | 25 => {}
                    _ => self.cop0[r] = v,
                }
            }
            0x08 => {
                // BC0F / BC0T / BC0FL / BC0TL
                let c = bus.cpcond0();
                match rt {
                    0 => self.branch(!c, i),
                    1 => self.branch(c, i),
                    2 => self.branch_likely(!c, i),
                    3 => self.branch_likely(c, i),
                    _ => return Err(self.fault(EXC_RI)),
                }
            }
            0x10 => match i & 0x3F {
                0x01 => {
                    // TLBR
                    let e = self.tlb[(self.cop0[INDEX] & 0x3F) as usize % 48];
                    self.cop0[PAGEMASK] = e.mask;
                    self.cop0[ENTRYHI] = e.hi;
                    self.cop0[ENTRYLO0] = e.lo0 & !1 | (e.global() as u32);
                    self.cop0[ENTRYLO1] = e.lo1 & !1 | (e.global() as u32);
                }
                0x02 => self.tlb_write((self.cop0[INDEX] & 0x3F) as usize),
                0x06 => { let r = self.random() as usize; self.tlb_write(r); }
                0x08 => {
                    // TLBP
                    let hi = self.cop0[ENTRYHI];
                    let found = self.tlb.iter().position(|e| {
                        let m = !((e.mask | 0x1FFF) & 0x01FF_FFFF) & 0xFFFF_E000;
                        (e.hi & m) == (hi & m) && (e.global() || e.hi & 0xFF == hi & 0xFF)
                    });
                    self.cop0[INDEX] = match found { Some(k) => k as u32, None => 0x8000_0000 };
                }
                0x18 => {
                    // ERET
                    let st = self.cop0[STATUS];
                    if st & ST_ERL != 0 {
                        self.cop0[STATUS] = st & !ST_ERL;
                        self.jump(self.cop0[ERROREPC]);
                    } else {
                        self.cop0[STATUS] = st & !ST_EXL;
                        self.jump(self.cop0[EPC]);
                    }
                }
                0x38 => self.cop0[STATUS] |= ST_EIE,
                0x39 => self.cop0[STATUS] &= !ST_EIE,
                _ => return Err(self.fault(EXC_RI)),
            },
            _ => return Err(self.fault(EXC_RI)),
        }
        Ok(1)
    }

    /// Random counts down from 47 to Wired with the cycle counter
    fn random(&self) -> u32 {
        let wired = self.cop0[WIRED].min(47);
        let span = 48 - wired;
        47 - (self.cycles % span as u64) as u32
    }

    fn cop2(&mut self, bus: &mut dyn EeBus, i: u32, rs: u32) -> Res<u32> {
        if self.cop0[STATUS] & (1 << 30) == 0 { return Err(self.cop_unusable(2)); }
        if rs == 0x08 {
            let c = bus.cop2_cond();
            match i >> 16 & 31 {
                0 => self.branch(!c, i),
                1 => self.branch(c, i),
                2 => self.branch_likely(!c, i),
                3 => self.branch_likely(c, i),
                _ => return Err(self.fault(EXC_RI)),
            }
            return Ok(1);
        }
        Ok(1 + bus.cop2_op(i, &mut self.gpr))
    }
}
