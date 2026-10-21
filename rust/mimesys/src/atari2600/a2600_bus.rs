// Atari 2600 address decoding. The 6507 has only 13 address lines (0x0000-0x1FFF, mirrored
// through the whole 64K), and the chips are picked by just three of them:
//
//   A12 = 1                     cartridge (0x1000-0x1FFF)
//   A12 = 0, A7 = 0             TIA        (writes: A0-A5 = register, reads: A0-A3)
//   A12 = 0, A7 = 1, A9 = 0     RIOT RAM   (0x80-0xFF, mirrored at 0x180 = the stack)
//   A12 = 0, A7 = 1, A9 = 1     RIOT ports and timer (0x280-0x29F)
//
// TIA reads only drive data bits 7 and 6; bits 5-0 keep whatever was last on the data bus
// (usually the instruction's own operand byte). Games rely on it: Haunted House divides
// by 15 with "SBC $0F" (an unused TIA address), which reads back $0F from the operand.

use crate::common::cpu::m6502::AddressBus;
use crate::atari2600::cart::Cart;
use crate::atari2600::riot::Riot;
use crate::atari2600::tia::Tia;

pub struct A2600Bus {
    pub tia: Tia,
    pub riot: Riot,
    pub cart: Cart,
    pub cycles: u64,
    /// Last byte on the data bus (for the bits TIA reads don't drive)
    data_bus: u8,
}

impl A2600Bus {
    pub fn new(cart: Cart, out_rate: u32) -> Self {
        Self { tia: Tia::new(out_rate), riot: Riot::new(), cart, cycles: 0, data_bus: 0 }
    }

    /// Everything that happens in one CPU cycle besides the CPU itself:
    /// 3 TIA color clocks and 1 RIOT timer tick.
    pub fn tick(&mut self) {
        self.tia.clock();
        self.tia.clock();
        self.tia.clock();
        self.riot.tick();
        self.cycles += 1;
    }
}

impl AddressBus for A2600Bus {
    fn is_nmi_line_asserted(&mut self) -> bool { false }   // the 6507 has no NMI or IRQ pins
    fn is_nmi_enabled(&mut self) -> bool { false }
    fn total_cycles(&self) -> u64 { self.cycles }
    fn is_irq_line_asserted(&mut self) -> bool { false }

    fn read_byte(&mut self, addr: u16) -> u8 {
        let a = addr & 0x1FFF;
        let v = if a & 0x1000 != 0 {
            self.cart.read(a & 0x0FFF)
        } else if a & 0x80 == 0 {
            (self.tia.read(a as u8) & 0xC0) | (self.data_bus & 0x3F)
        } else if a & 0x200 == 0 {
            self.riot.ram[(a & 0x7F) as usize]
        } else {
            self.riot.read(a)
        };
        self.data_bus = v;
        v
    }

    fn write_byte(&mut self, addr: u16, value: u8) {
        self.data_bus = value;
        let a = addr & 0x1FFF;
        if a & 0x1000 != 0 {
            self.cart.write(a & 0x0FFF, value);
        } else if a & 0x80 == 0 {
            self.cart.tia_write(a, value);
            self.tia.write(a as u8, value);
        } else if a & 0x200 == 0 {
            self.riot.ram[(a & 0x7F) as usize] = value;
        } else {
            self.riot.write(a, value);
        }
    }

    // Used by the CPU's power-on sequence
    fn step_cycles(&mut self, cycles: u64) {
        for _ in 0..cycles { self.tick(); }
    }

    fn begin_cpu_cycle(&mut self) {}
}
