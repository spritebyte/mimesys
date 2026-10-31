//! Motorola 68000.
//!
//! Bus-cycle accurate in the same way as Clock Signal's 68000 (Thomas Harte, MIT licence),
//! whose behaviour the SingleStepTests 680x0 suite was generated from: every instruction
//! is the same sequence of program fetches (np), data reads/writes (nr/nw) and idle
//! microcycles (n = 2 clocks) as CLK's bus programs, so cycle counts, the prefetch queue,
//! and the stack frame of an address error (which depends on exactly how far the
//! instruction got) all come out the same.
//!
//! Model:
//! - `pc` is the address of the next prefetch, i.e. 4 past the instruction being
//!   decoded; `prefetch[0]` holds the opcode (or the extension word consumed last) and
//!   `prefetch[1]` the next word (IRC).
//! - `r[0..8]` are D0-D7, `r[8..16]` A0-A7 with A7 the active stack pointer;
//!   `other_sp` is the inactive one (USP in supervisor mode, SSP in user mode).
//! - A word or long access to an odd address raises an address error before the access
//!   (no bus cycle), keeping whatever the instruction had already changed.
//!
//! `step()` runs one instruction (or exception) and returns the clocks it took.

mod decode;
mod exec;
#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};

pub use decode::{Inst, Mode, Op, Size};

/// The system side. Addresses are 24-bit (already masked).
pub trait M68kBus {
    fn read_byte(&mut self, addr: u32) -> u8;
    fn read_word(&mut self, addr: u32) -> u16;
    fn write_byte(&mut self, addr: u32, v: u8);
    fn write_word(&mut self, addr: u32, v: u16);
    /// Interrupt acknowledge for `level` (1-7): `Some(vector)` for a vectored interrupt,
    /// `None` to autovector (vector 24 + level).
    fn interrupt_ack(&mut self, _level: u8) -> Option<u8> { None }
    /// The RESET instruction pulsed the reset line.
    fn reset_devices(&mut self) {}
}

pub const SR_C: u16 = 0x0001;
pub const SR_V: u16 = 0x0002;
pub const SR_Z: u16 = 0x0004;
pub const SR_N: u16 = 0x0008;
pub const SR_X: u16 = 0x0010;
pub const SR_S: u16 = 0x2000;
pub const SR_T: u16 = 0x8000;
const SR_MASK: u16 = 0xA71F;

/// Exception vector numbers.
pub mod vector {
    pub const ADDRESS_ERROR: u32 = 3;
    pub const ILLEGAL: u32 = 4;
    pub const ZERO_DIVIDE: u32 = 5;
    pub const CHK: u32 = 6;
    pub const TRAPV: u32 = 7;
    pub const PRIVILEGE: u32 = 8;
    pub const TRACE: u32 = 9;
    pub const LINE_A: u32 = 10;
    pub const LINE_F: u32 = 11;
    pub const SPURIOUS: u32 = 24;
    pub const AUTOVECTOR_BASE: u32 = 24;
    pub const TRAP_BASE: u32 = 32;
}

/// An address error: the access that faulted.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Fault {
    addr: u32,
    read: bool,
    program: bool,
}

pub(crate) type R<T> = Result<T, Fault>;

#[derive(Clone, Serialize, Deserialize)]
pub struct M68000 {
    /// D0-D7, A0-A7 (A7 = active stack pointer)
    pub r: [u32; 16],
    /// the inactive stack pointer
    pub other_sp: u32,
    pub sr: u16,
    /// address of the next prefetch
    pub pc: u32,
    /// [older, newer]: the opcode / last extension word, then IRC
    pub prefetch: [u16; 2],
    /// stopped by STOP until an interrupt
    pub stopped: bool,
    /// halted by a double fault
    pub halted: bool,
    /// interrupt priority level input (0-7)
    pub ipl: u8,
    nmi_seen: bool,
    should_trace: bool,
    // per-instruction scratch
    opcode: u16,
    instruction_address: u32,
    operand: [u32; 2],
    ea: [u32; 2],
    dynamic_length: u32,
    cycles: u32,
}

impl Default for M68000 {
    fn default() -> Self { Self::new() }
}

impl M68000 {
    pub fn new() -> Self {
        Self {
            r: [0; 16], other_sp: 0, sr: 0x2700, pc: 0, prefetch: [0; 2],
            stopped: false, halted: false, ipl: 0, nmi_seen: false, should_trace: false,
            opcode: 0, instruction_address: 0, operand: [0; 2], ea: [0; 2],
            dynamic_length: 0, cycles: 0,
        }
    }

    pub fn supervisor(&self) -> bool { self.sr & SR_S != 0 }

    /// The user stack pointer, whichever mode the CPU is in.
    pub fn usp(&self) -> u32 { if self.supervisor() { self.other_sp } else { self.r[15] } }
    pub fn ssp(&self) -> u32 { if self.supervisor() { self.r[15] } else { self.other_sp } }

    /// Address of the instruction about to run (the "formal" PC).
    pub fn instruction_pc(&self) -> u32 { self.pc.wrapping_sub(4) }

    /// Set the whole status register, swapping stack pointers on an S change.
    pub fn set_sr(&mut self, v: u16) {
        let v = v & SR_MASK;
        if (v ^ self.sr) & SR_S != 0 {
            std::mem::swap(&mut self.r[15], &mut self.other_sp);
        }
        self.sr = v;
    }

    fn set_ccr(&mut self, v: u16) { self.sr = (self.sr & 0xFF00) | (v & 0x1F); }

    /// Set registers as the test suite and debuggers describe them.
    pub fn set_state(&mut self, d: [u32; 8], a: [u32; 7], usp: u32, ssp: u32, sr: u16, pc: u32, prefetch: [u16; 2]) {
        self.r[..8].copy_from_slice(&d);
        self.r[8..15].copy_from_slice(&a);
        self.sr = sr & SR_MASK;
        if self.supervisor() { self.r[15] = ssp; self.other_sp = usp; } else { self.r[15] = usp; self.other_sp = ssp; }
        self.pc = pc.wrapping_add(4);
        self.prefetch = prefetch;
        self.stopped = false;
        self.halted = false;
    }

    // ---------------------------------------------------------------- bus primitives

    fn idle(&mut self, n: u32) { self.cycles += 2 * n; }

    fn read_w(&mut self, bus: &mut dyn M68kBus, addr: u32) -> R<u16> {
        if addr & 1 != 0 { return Err(Fault { addr, read: true, program: false }); }
        self.cycles += 4;
        Ok(bus.read_word(addr & 0xFF_FFFF))
    }
    fn read_b(&mut self, bus: &mut dyn M68kBus, addr: u32) -> u8 {
        self.cycles += 4;
        bus.read_byte(addr & 0xFF_FFFF)
    }
    fn write_w(&mut self, bus: &mut dyn M68kBus, addr: u32, v: u16) -> R<()> {
        if addr & 1 != 0 { return Err(Fault { addr, read: false, program: false }); }
        self.cycles += 4;
        bus.write_word(addr & 0xFF_FFFF, v);
        Ok(())
    }
    fn write_b(&mut self, bus: &mut dyn M68kBus, addr: u32, v: u8) {
        self.cycles += 4;
        bus.write_byte(addr & 0xFF_FFFF, v);
    }

    /// Read a byte or word operand (`size` is byte or word).
    fn read_bw(&mut self, bus: &mut dyn M68kBus, addr: u32, size: Size) -> R<u32> {
        if size == Size::Byte { Ok(self.read_b(bus, addr) as u32) } else { Ok(self.read_w(bus, addr)? as u32) }
    }
    fn write_bw(&mut self, bus: &mut dyn M68kBus, addr: u32, size: Size, v: u32) -> R<()> {
        if size == Size::Byte { self.write_b(bus, addr, v as u8); Ok(()) } else { self.write_w(bus, addr, v as u16) }
    }

    /// np: shift the queue and fetch the next program word.
    fn prefetch(&mut self, bus: &mut dyn M68kBus) -> R<()> {
        self.prefetch[0] = self.prefetch[1];
        if self.pc & 1 != 0 { return Err(Fault { addr: self.pc, read: true, program: true }); }
        self.cycles += 4;
        self.prefetch[1] = bus.read_word(self.pc & 0xFF_FFFF);
        self.pc = self.pc.wrapping_add(2);
        Ok(())
    }

    /// The two prefetch words as one long (after an np that moved the first into [0]).
    fn prefetch_l(&self) -> u32 { (self.prefetch[0] as u32) << 16 | self.prefetch[1] as u32 }

    // ---------------------------------------------------------------- running

    /// Power-on / RESET line: SSP and PC from vectors 0 and 1.
    pub fn reset(&mut self, bus: &mut dyn M68kBus) -> u32 {
        self.cycles = 0;
        self.halted = false;
        self.stopped = false;
        self.idle(7);
        if !self.supervisor() { std::mem::swap(&mut self.r[15], &mut self.other_sp); }
        self.sr = (self.sr & !SR_T) | SR_S | 0x0700;
        self.should_trace = false;
        let res: R<()> = (|| {
            let hi = self.read_w(bus, 0)? as u32;
            let lo = self.read_w(bus, 2)? as u32;
            self.r[15] = hi << 16 | lo;
            let hi = self.read_w(bus, 4)? as u32;
            let lo = self.read_w(bus, 6)? as u32;
            self.pc = hi << 16 | lo;
            self.prefetch(bus)?;
            self.idle(1);
            self.prefetch(bus)
        })();
        if res.is_err() { self.halted = true; }
        self.cycles
    }

    /// Run one instruction, exception or interrupt. Returns clocks used.
    pub fn step(&mut self, bus: &mut dyn M68kBus) -> u32 {
        self.cycles = 0;
        if self.halted { return 4; }

        // interrupts: level 7 is edge triggered (non-maskable)
        let level = self.ipl & 7;
        let nmi = level == 7 && !self.nmi_seen;
        self.nmi_seen = level == 7;
        let mask = ((self.sr >> 8) & 7) as u8;
        if level > mask || nmi {
            // (after STOP, instruction_address already holds the return point)
            if !self.stopped { self.instruction_address = self.pc.wrapping_sub(4); }
            self.stopped = false;
            let r = self.interrupt(bus, level);
            self.finish_exception(bus, r);
            return self.cycles;
        }
        if self.stopped {
            self.idle(1);
            return self.cycles;
        }

        self.instruction_address = self.pc.wrapping_sub(4);
        if self.should_trace {
            self.should_trace = false;
            let r = self.exception(bus, vector::TRACE);
            self.finish_exception(bus, r);
            return self.cycles;
        }
        self.should_trace = self.sr & SR_T != 0;

        let r = self.execute(bus);
        self.finish_exception(bus, r);
        self.cycles
    }

    fn finish_exception(&mut self, bus: &mut dyn M68kBus, r: R<()>) {
        if let Err(f) = r {
            if self.address_error(bus, f).is_err() {
                self.halted = true;      // double fault
            }
        }
    }

    // ---------------------------------------------------------------- exceptions

    /// Enter supervisor mode, clear trace; returns the old SR.
    fn begin_exception(&mut self, level: Option<u8>) -> u16 {
        let old = self.sr;
        let mut sr = (self.sr | SR_S) & !SR_T;
        if let Some(l) = level { sr = (sr & !0x0700) | ((l as u16) << 8); }
        self.set_sr(sr);
        self.should_trace = false;
        old
    }

    /// Group 1 / 2 exception: push PC (instruction_address) and SR, jump through the vector.
    pub(crate) fn exception(&mut self, bus: &mut dyn M68kBus, vector: u32) -> R<()> {
        let old_sr = self.begin_exception(None);
        let pc = self.instruction_address;
        self.r[15] = self.r[15].wrapping_sub(2);
        self.write_w(bus, self.r[15], pc as u16)?;                // ns  pc low
        self.r[15] = self.r[15].wrapping_sub(4);
        self.write_w(bus, self.r[15], old_sr)?;                   // ns  sr
        self.r[15] = self.r[15].wrapping_add(2);
        self.write_w(bus, self.r[15], (pc >> 16) as u16)?;        // nS  pc high
        self.r[15] = self.r[15].wrapping_sub(2);
        self.jump_through(bus, vector)
    }

    fn jump_through(&mut self, bus: &mut dyn M68kBus, vector: u32) -> R<()> {
        let hi = self.read_w(bus, vector << 2)? as u32;           // nV
        let lo = self.read_w(bus, (vector << 2) + 2)? as u32;     // nv
        self.pc = hi << 16 | lo;
        self.prefetch(bus)?;                                       // np
        self.idle(1);                                              // n
        self.prefetch(bus)                                         // np
    }

    /// Address error: the 7-word group 0 frame.
    fn address_error(&mut self, bus: &mut dyn M68kBus, f: Fault) -> R<()> {
        self.idle(2);
        let old_sr = self.begin_exception(None);
        let pc = self.pc.wrapping_sub(4);
        self.r[15] = self.r[15].wrapping_sub(2);
        self.write_w(bus, self.r[15], pc as u16)?;                 // pc low
        self.r[15] = self.r[15].wrapping_sub(4);
        self.write_w(bus, self.r[15], old_sr)?;                    // sr
        self.r[15] = self.r[15].wrapping_add(2);
        self.write_w(bus, self.r[15], (pc >> 16) as u16)?;         // pc high
        self.r[15] = self.r[15].wrapping_sub(4);
        self.write_w(bus, self.r[15], self.opcode)?;               // instruction register
        let status = (self.opcode & !31)
            | if f.read { 0x10 } else { 0 }
            | if f.program { 0x0A } else { 0x01 }
            | if old_sr & SR_S != 0 { 0x04 } else { 0 };
        self.r[15] = self.r[15].wrapping_sub(2);
        self.write_w(bus, self.r[15], f.addr as u16)?;             // access address low
        self.r[15] = self.r[15].wrapping_sub(4);
        self.write_w(bus, self.r[15], status)?;                    // R/W, I/N, FC
        self.r[15] = self.r[15].wrapping_add(2);
        self.write_w(bus, self.r[15], (f.addr >> 16) as u16)?;     // access address high
        self.r[15] = self.r[15].wrapping_sub(2);
        self.jump_through(bus, vector::ADDRESS_ERROR)
    }

    fn interrupt(&mut self, bus: &mut dyn M68kBus, level: u8) -> R<()> {
        self.idle(3);                                               // n nn
        let old_sr = self.begin_exception(Some(level));
        let pc = self.instruction_address;
        self.r[15] = self.r[15].wrapping_sub(2);
        self.write_w(bus, self.r[15], pc as u16)?;                  // ns
        self.cycles += 4;                                           // ni: acknowledge
        let vector = match bus.interrupt_ack(level) {
            Some(v) => v as u32,
            None => vector::AUTOVECTOR_BASE + level as u32,
        };
        self.idle(3);                                               // n- n
        self.r[15] = self.r[15].wrapping_sub(4);
        self.write_w(bus, self.r[15], old_sr)?;                     // ns
        self.r[15] = self.r[15].wrapping_add(2);
        self.write_w(bus, self.r[15], (pc >> 16) as u16)?;          // nS
        self.r[15] = self.r[15].wrapping_sub(2);
        self.jump_through(bus, vector)
    }
}
