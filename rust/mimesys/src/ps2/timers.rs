//! The EE's four timers (bus clock, /16, /256 or HBLANK; compare / overflow interrupts,
//! edge-triggered into INTC 9-12) and the IOP's six (T0-T2 16-bit, T3-T5 32-bit; IOP
//! clock or the external signals, prescalers; I_STAT 4-6 and 14-16). From ps2tek.
//! Gates aren't modelled yet.

use serde::{Deserialize, Serialize};

#[derive(Default, Clone, Serialize, Deserialize)]
pub struct EeTimer {
    pub count: u32,
    pub mode: u32,
    pub comp: u32,
    pub hold: u32,
    /// bus cycles not yet counted (for /16 and /256)
    frac: u32,
}

const EE_CMPE: u32 = 1 << 8;
const EE_OVFE: u32 = 1 << 9;
const EE_EQUF: u32 = 1 << 10;
const EE_OVFF: u32 = 1 << 11;

impl EeTimer {
    pub fn read(&self, off: u32) -> u32 {
        match off & 0x30 {
            0x00 => self.count,
            0x10 => self.mode,
            0x20 => self.comp,
            _ => self.hold,
        }
    }

    pub fn write(&mut self, off: u32, v: u32) {
        match off & 0x30 {
            0x00 => self.count = v & 0xFFFF,
            0x10 => {
                // the two flags clear on 1
                let flags = self.mode & (EE_EQUF | EE_OVFF) & !v;
                self.mode = (v & 0x3FF) | flags;
            }
            0x20 => self.comp = v & 0xFFFF,
            _ => self.hold = v & 0xFFFF,
        }
    }

    /// `bus` bus-clock cycles passed (and `hblank` HBLANKs); returns true for a new
    /// interrupt (a flag went 0 -> 1)
    pub fn tick(&mut self, bus: u32, hblank: u32) -> bool {
        if self.mode & 0x80 == 0 { return false; }
        let n = match self.mode & 3 {
            0 => bus,
            1 => { self.frac += bus; let n = self.frac >> 4; self.frac &= 15; n }
            2 => { self.frac += bus; let n = self.frac >> 8; self.frac &= 255; n }
            _ => hblank,
        };
        if n == 0 { return false; }
        let before = self.mode & (EE_EQUF | EE_OVFF);
        let old = self.count;
        let mut new = old + n;
        // compare: COUNT passes COMP
        if self.comp > old && self.comp <= new {
            if self.mode & EE_CMPE != 0 { self.mode |= EE_EQUF; }
            // ZRET: back to 0 on reaching COMP
            if self.mode & 0x40 != 0 { new -= self.comp; }
        }
        if new > 0xFFFF {
            if self.mode & EE_OVFE != 0 { self.mode |= EE_OVFF; }
            new &= 0xFFFF;
        }
        self.count = new;
        (self.mode & (EE_EQUF | EE_OVFF)) & !before != 0
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct IopTimer {
    pub count: u64,
    pub mode: u32,
    pub target: u64,
    frac: u32,
}

impl Default for IopTimer {
    fn default() -> Self { Self { count: 0, mode: 0x400, target: 0, frac: 0 } }
}

/// I_STAT bits of T0..T5
pub const IOP_TIMER_IRQ: [u32; 6] = [1 << 4, 1 << 5, 1 << 6, 1 << 14, 1 << 15, 1 << 16];

impl IopTimer {
    pub fn read(&mut self, off: u32) -> u32 {
        match off & 0xC {
            0x0 => self.count as u32,
            0x4 => { let v = self.mode; self.mode &= !(3 << 11); v }
            _ => self.target as u32,
        }
    }

    pub fn write(&mut self, off: u32, v: u32) {
        match off & 0xC {
            0x0 => self.count = v as u64,
            0x4 => { self.mode = (v & 0x63FF) | 0x400; self.count = 0; }
            _ => {
                self.target = v as u64;
                if self.mode & 0x80 == 0 { self.mode |= 0x400; }
            }
        }
    }

    /// `n`: which timer (0-5); `cycles` IOP cycles and `hblank` HBLANKs passed. True when
    /// it raises its interrupt.
    pub fn tick(&mut self, n: usize, cycles: u32, hblank: u32) -> bool {
        let ext = self.mode & 0x100 != 0;
        let mut inc = match (n, ext) {
            (1 | 3, true) => hblank,
            // the 13.5 MHz pixel clock against the IOP's 36.864 MHz: 3 / 8 is close
            (0, true) => { self.frac += cycles * 3; let k = self.frac >> 3; self.frac &= 7; k }
            _ => cycles,
        };
        let div = match n {
            2 if self.mode & 0x200 != 0 => 8,
            4 | 5 => [1, 8, 16, 256][(self.mode >> 13 & 3) as usize],
            _ => 1,
        };
        if div > 1 {
            self.frac += inc;
            inc = self.frac / div;
            self.frac %= div;
        }
        if inc == 0 { return false; }
        let max: u64 = if n < 3 { 0xFFFF } else { 0xFFFF_FFFF };
        let old = self.count;
        let mut new = old + inc as u64;
        let mut irq = false;
        if self.target > old && self.target <= new {
            self.mode |= 1 << 11;
            if self.mode & 0x10 != 0 { irq = true; }
            if self.mode & 0x08 != 0 { new -= self.target; }
        }
        if new > max {
            self.mode |= 1 << 12;
            if self.mode & 0x20 != 0 { irq = true; }
            new &= max;
        }
        self.count = new;
        if irq && self.mode & 0x400 != 0 {
            // one-shot unless "repeat"; "toggle" flips bit 10 instead
            if self.mode & 0x40 == 0 { self.mode &= !0x400; }
            else if self.mode & 0x80 != 0 { self.mode ^= 0x400; }
            return true;
        }
        false
    }
}
