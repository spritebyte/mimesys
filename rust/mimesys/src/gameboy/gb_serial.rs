// Game Boy serial port (link cable).
//
//   FF01 SB  the byte to send; after a transfer, the byte received
//   FF02 SC  bit 7 = transfer start / busy, bit 0 = clock (1 = we drive it, 0 = the other side)
//            bit 1 (CGB only) = fast clock
//
// Two Game Boys exchange a byte at a time: the one with bit 0 = 1 (internal clock) drives
// the transfer, 8 bits at 8192 Hz (= 4096 T-cycles per byte; 32x faster with the CGB fast bit,
// 2x faster in double speed). The other side must have SB loaded and SC = $80 (external clock,
// ready); its byte comes back to us and ours goes to it. Both get the serial interrupt.
//
// This emulates whole bytes, not single bits (games only look at SB after the interrupt):
//   * an internal-clock transfer counts down its duration, then stops at `awaiting = Some(out)`
//   * whoever owns the cable answers with `finish(byte_from_partner)`
//     (no cable: finish(0xFF), which is what an unplugged port reads)
//   * the partner side is driven with `external_clock(byte)`, which returns its byte
// That split lets the same code work for two Game Boys in one emulator and over a network
// (where the answer arrives later and the driving side waits for it).

use serde::{Deserialize, Serialize};

/// T-cycles per bit at the normal 8192 Hz clock.
const BIT_CYCLES: u64 = 512;
/// T-cycles per bit with the CGB fast clock (262144 Hz).
const FAST_BIT_CYCLES: u64 = 16;

#[derive(Clone, Serialize, Deserialize)]
pub struct Serial {
    pub sb: u8,
    pub sc: u8,
    cgb: bool,
    /// T-cycles left in an internal-clock transfer (0 = none running)
    remaining: u64,
    /// Internal-clock transfer finished: this byte went out, waiting for the partner's byte.
    pub awaiting: Option<u8>,
    irq: bool,
}

impl Serial {
    pub fn new(cgb: bool) -> Self {
        Self { sb: 0x00, sc: 0x00, cgb, remaining: 0, awaiting: None, irq: false }
    }

    pub fn read_sb(&self) -> u8 { self.sb }

    pub fn read_sc(&self) -> u8 {
        // unused bits read as 1 (bit 1 exists only on CGB)
        if self.cgb { self.sc | 0x7C } else { self.sc | 0x7E }
    }

    pub fn write_sb(&mut self, v: u8) { self.sb = v; }

    /// `double_speed`: CGB double-speed mode doubles the serial clock too.
    pub fn write_sc(&mut self, v: u8, double_speed: bool) {
        self.sc = v & if self.cgb { 0x83 } else { 0x81 };
        self.awaiting = None;
        self.remaining = 0;
        if self.sc & 0x81 == 0x81 {
            let fast = self.cgb && self.sc & 0x02 != 0;
            let bit = if fast { FAST_BIT_CYCLES } else { BIT_CYCLES };
            self.remaining = 8 * bit / if double_speed { 2 } else { 1 };
        }
        // bit 7 with external clock: just wait, external_clock() completes it
    }

    /// Advance by `cycles` T-cycles (real time: 4.19 MHz units even in double speed).
    pub fn tick(&mut self, cycles: u64) {
        if self.remaining == 0 {
            return;
        }
        if cycles >= self.remaining {
            self.remaining = 0;
            self.awaiting = Some(self.sb);
        } else {
            self.remaining -= cycles;
        }
    }

    /// We drove the transfer: here is the partner's byte. Completes it and raises the IRQ.
    pub fn finish(&mut self, incoming: u8) {
        self.awaiting = None;
        self.sb = incoming;
        self.sc &= !0x80;
        self.irq = true;
    }

    /// The partner drove a transfer to us with `incoming`; returns what we sent back.
    /// Only completes if we're waiting for one (SC = $80); otherwise nothing shifts and the
    /// partner just sees our output line, which holds SB bit 7.
    pub fn external_clock(&mut self, incoming: u8) -> u8 {
        if self.sc & 0x81 == 0x80 {
            let out = self.sb;
            self.sb = incoming;
            self.sc &= !0x80;
            self.irq = true;
            out
        } else if self.sb & 0x80 != 0 {
            0xFF
        } else {
            0x00
        }
    }

    /// True once per completed transfer (the bus ORs bit 3 into IF).
    pub fn take_irq(&mut self) -> bool {
        std::mem::take(&mut self.irq)
    }

    /// True while a transfer is set up and not finished (either clock).
    pub fn is_busy(&self) -> bool { self.sc & 0x80 != 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unconnected_transfer_takes_4096_cycles_and_reads_ff() {
        let mut s = Serial::new(false);
        s.write_sb(0x42);
        s.write_sc(0x81, false);
        assert_eq!(s.read_sc(), 0xFF);          // busy bit + unused bits
        s.tick(4095);
        assert_eq!(s.awaiting, None);
        s.tick(1);
        assert_eq!(s.awaiting, Some(0x42));
        assert!(!s.take_irq(), "no IRQ until the byte comes back");
        s.finish(0xFF);
        assert_eq!((s.sb, s.read_sc(), s.take_irq()), (0xFF, 0x7F, true));
        assert!(!s.take_irq(), "IRQ is reported once");
    }

    #[test]
    fn fast_clock_and_double_speed() {
        let mut s = Serial::new(true);
        s.write_sc(0x83, false);                // CGB fast: 8 * 16
        s.tick(127);
        assert_eq!(s.awaiting, None);
        s.tick(1);
        assert!(s.awaiting.is_some());
        let mut d = Serial::new(true);
        d.write_sc(0x81, true);                 // double speed: 2048
        d.tick(2048);
        assert!(d.awaiting.is_some());
        let mut dmg = Serial::new(false);
        dmg.write_sc(0x83, false);              // DMG has no fast bit: normal speed
        dmg.tick(128);
        assert!(dmg.awaiting.is_none());
    }

    #[test]
    fn external_side_swaps_only_when_ready() {
        let mut slave = Serial::new(false);
        slave.write_sb(0x99);
        // not ready (SC bit 7 clear): nothing shifts, master sees SB bit 7 repeated
        assert_eq!(slave.external_clock(0x42), 0xFF);
        assert_eq!(slave.sb, 0x99);
        assert!(!slave.take_irq());
        slave.write_sc(0x80, false);
        assert_eq!(slave.external_clock(0x42), 0x99);
        assert_eq!((slave.sb, slave.is_busy(), slave.take_irq()), (0x42, false, true));
    }

    #[test]
    fn rewriting_sc_cancels_a_transfer() {
        let mut s = Serial::new(false);
        s.write_sc(0x81, false);
        s.tick(5000);
        assert!(s.awaiting.is_some());
        s.write_sc(0x00, false);
        assert!(s.awaiting.is_none());
        s.tick(10_000);
        assert!(s.awaiting.is_none());
    }
}
