//! Motorola 6821 PIA (Peripheral Interface Adapter). After MAME 6821pia.cpp (BSD-3-Clause).
//!
//! Two 8-bit ports (A and B), each with a data direction register (DDR) and a control
//! register. Control bit 2 picks whether offset 0/2 reaches the DDR (0) or the port (1).
//! CA1/CB1 are interrupt inputs (bit 1: rising or falling edge, bit 0: IRQ enable);
//! CA2/CB2 are inputs like C1 (bit 5 clear) or outputs (bit 5 set: bit 4 set = level
//! from bit 3, clear = strobe). Reading a port's data clears its IRQ flags.
//!
//! The owner passes the port pin levels to `read` (so they can depend on outputs, like a
//! multiplexer on CB2) and looks at what `write` returns for port output changes.

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct Pia6821 {
    out_a: u8,
    out_b: u8,
    ddr_a: u8,
    ddr_b: u8,
    ctl_a: u8,
    ctl_b: u8,
    irq_a1: bool,
    irq_a2: bool,
    irq_b1: bool,
    irq_b2: bool,
    in_ca1: bool,
    in_ca2: bool,
    in_cb1: bool,
    in_cb2: bool,
    out_ca2: bool,
    out_cb2: bool,
}

/// A port output that changed because of a write.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PiaOut { A(u8), B(u8), None }

impl Default for Pia6821 {
    fn default() -> Self { Self::new() }
}

impl Pia6821 {
    pub fn new() -> Self {
        Self {
            out_a: 0, out_b: 0, ddr_a: 0, ddr_b: 0, ctl_a: 0, ctl_b: 0,
            irq_a1: false, irq_a2: false, irq_b1: false, irq_b2: false,
            in_ca1: true, in_ca2: true, in_cb1: false, in_cb2: false,
            out_ca2: false, out_cb2: false,
        }
    }

    pub fn reset(&mut self) { *self = Self { in_ca1: self.in_ca1, in_ca2: self.in_ca2, in_cb1: self.in_cb1, in_cb2: self.in_cb2, ..Self::new() }; }

    fn c2_output(c: u8) -> bool { c & 0x20 != 0 }
    fn c2_set_mode(c: u8) -> bool { c & 0x10 != 0 }
    fn strobe_e_reset(c: u8) -> bool { c & 0x08 != 0 }

    pub fn irq_a(&self) -> bool {
        (self.irq_a1 && self.ctl_a & 0x01 != 0) || (self.irq_a2 && self.ctl_a & 0x08 != 0)
    }
    pub fn irq_b(&self) -> bool {
        (self.irq_b1 && self.ctl_b & 0x01 != 0) || (self.irq_b2 && self.ctl_b & 0x08 != 0)
    }
    pub fn irq(&self) -> bool { self.irq_a() || self.irq_b() }

    /// Port A as the outside sees it (inputs pulled high).
    pub fn out_a(&self) -> u8 { (self.out_a & self.ddr_a) | !self.ddr_a }
    /// Port B outputs (input pins read as 0).
    pub fn out_b(&self) -> u8 { self.out_b & self.ddr_b }
    /// CA2 as the outside sees it (an input is pulled high).
    pub fn ca2(&self) -> bool { self.out_ca2 || !Self::c2_output(self.ctl_a) }
    /// CB2's output latch. Like MAME's cb2 callback this follows the control register
    /// even while CB2 is an input (it starts low).
    pub fn cb2(&self) -> bool { self.out_cb2 }

    /// `pins_a` / `pins_b`: what the outside drives on the port pins.
    pub fn read(&mut self, offset: u16, pins_a: u8, pins_b: u8) -> u8 {
        match offset & 3 {
            0 if self.ctl_a & 0x04 != 0 => {
                let v = (pins_a & !self.ddr_a) | (self.out_a & self.ddr_a);
                self.irq_a1 = false;
                self.irq_a2 = false;
                if Self::c2_output(self.ctl_a) && !Self::c2_set_mode(self.ctl_a) {
                    self.out_ca2 = false;                           // read strobe
                    if Self::strobe_e_reset(self.ctl_a) { self.out_ca2 = true; }
                }
                v
            }
            0 => self.ddr_a,
            1 => {
                let mut v = self.ctl_a;
                if self.irq_a1 { v |= 0x80; }
                if self.irq_a2 && !Self::c2_output(self.ctl_a) { v |= 0x40; }
                v
            }
            2 if self.ctl_b & 0x04 != 0 => {
                let v = if self.ddr_b == 0xFF { self.out_b } else { (self.out_b & self.ddr_b) | (pins_b & !self.ddr_b) };
                if self.irq_b1 && !Self::c2_set_mode(self.ctl_b) && !Self::strobe_e_reset(self.ctl_b) {
                    self.out_cb2 = true;
                }
                self.irq_b1 = false;
                self.irq_b2 = false;
                v
            }
            2 => self.ddr_b,
            _ => {
                let mut v = self.ctl_b;
                if self.irq_b1 { v |= 0x80; }
                if self.irq_b2 && !Self::c2_output(self.ctl_b) { v |= 0x40; }
                v
            }
        }
    }

    pub fn write(&mut self, offset: u16, data: u8) -> PiaOut {
        match offset & 3 {
            0 => {
                if self.ctl_a & 0x04 != 0 { self.out_a = data; } else { self.ddr_a = data; }
                PiaOut::A(self.out_a())
            }
            1 => {
                let data = data & 0x3F;
                self.ctl_a = data;
                if Self::c2_output(data) {
                    self.out_ca2 = if Self::c2_set_mode(data) { data & 0x08 != 0 } else { true };
                }
                PiaOut::None
            }
            2 => {
                if self.ctl_b & 0x04 != 0 {
                    self.out_b = data;
                    if !Self::c2_set_mode(self.ctl_b) {
                        self.out_cb2 = false;                       // write strobe
                        if Self::strobe_e_reset(self.ctl_b) { self.out_cb2 = true; }
                    }
                } else {
                    self.ddr_b = data;
                }
                PiaOut::B(self.out_b())
            }
            _ => {
                let data = data & 0x3F;
                self.ctl_b = data;
                self.out_cb2 = if Self::c2_set_mode(data) { data & 0x08 != 0 } else { true };
                PiaOut::None
            }
        }
    }

    pub fn ca1_w(&mut self, state: bool) {
        let rising = self.ctl_a & 0x02 != 0;
        if self.in_ca1 != state && state == rising {
            self.irq_a1 = true;
            if Self::c2_output(self.ctl_a) && !Self::c2_set_mode(self.ctl_a) && !Self::strobe_e_reset(self.ctl_a) {
                self.out_ca2 = true;
            }
        }
        self.in_ca1 = state;
    }

    pub fn ca2_w(&mut self, state: bool) {
        let rising = self.ctl_a & 0x10 != 0;
        if !Self::c2_output(self.ctl_a) && self.in_ca2 != state && state == rising { self.irq_a2 = true; }
        self.in_ca2 = state;
    }

    pub fn cb1_w(&mut self, state: bool) {
        let rising = self.ctl_b & 0x02 != 0;
        if self.in_cb1 != state && state == rising { self.irq_b1 = true; }
        self.in_cb1 = state;
    }

    pub fn cb2_w(&mut self, state: bool) {
        let rising = self.ctl_b & 0x10 != 0;
        if !Self::c2_output(self.ctl_b) && self.in_cb2 != state && state == rising { self.irq_b2 = true; }
        self.in_cb2 = state;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ddr_ports_and_edge_irqs() {
        let mut p = Pia6821::new();
        p.write(0, 0x0F);                 // DDR A
        p.write(1, 0x04 | 0x02 | 0x01);   // port select, CA1 rising, IRQ on
        assert_eq!(p.write(0, 0xA5), PiaOut::A(0xF5));
        assert_eq!(p.read(0, 0x30, 0), 0x35, "inputs from the pins, outputs from the latch");

        p.ca1_w(false);
        assert!(!p.irq());
        p.ca1_w(true);
        assert!(p.irq_a());
        assert_eq!(p.read(1, 0, 0) & 0x80, 0x80);
        p.read(0, 0, 0);
        assert!(!p.irq(), "reading port A clears the flag");

        // CB2 as a set/reset output
        p.write(3, 0x38);
        assert!(p.cb2());
        p.write(3, 0x30);
        assert!(!p.cb2());

        // CB1 falling edge with IRQ disabled: flag set, no IRQ out
        p.write(3, 0x04);
        p.cb1_w(true);
        p.cb1_w(false);
        assert!(!p.irq());
        assert_eq!(p.read(3, 0, 0) & 0x80, 0x80);
        p.write(3, 0x05);
        assert!(p.irq_b(), "enabling raises the pending flag");
    }
}
