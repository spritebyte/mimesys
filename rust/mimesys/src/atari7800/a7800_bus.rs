// Atari 7800 memory map (6502 "Sally" at 1.79 MHz):
//
//   0000-001F  TIA (sound, buttons)          0100-011F  mirror
//   0020-003F  MARIA registers               0120-013F  mirror
//   0040-00FF  RAM (zero page) = 2040-20FF   0140-01FF  RAM (stack) = 2140-21FF
//   0280-02FF  RIOT ports and timer          0480-04FF  RIOT's 128 bytes of RAM
//   1800-1FFF  RAM (2K)                      2000-27FF  RAM (2K), mirrored up to 3FFF
//   4000-FFFF  cartridge
//
// Time is counted in MARIA clocks (7.16 MHz): a CPU cycle takes 4, or 6 when it touches the
// TIA or RIOT (the CPU slows to 1.19 MHz for those chips).
// Buttons: in 7800 mode each joystick has two (INPT0 = right, INPT1 = left, bit 7 = 1 when
// pressed); INPT4 also shows "either button" the 2600 way (bit 7 = 0 when pressed).
// TIA reads only drive bits 7-6; the rest is the last byte on the data bus (as on the 2600).
// The BIOS isn't used: games start straight from the cartridge's reset vector.

use crate::common::cpu::m6502::AddressBus;
use crate::atari2600::riot::Riot;
use crate::atari2600::tia::Tia;
use crate::atari7800::cart::Cart7800;
use crate::atari7800::maria::Maria;

pub const CLOCKS_PER_LINE: u32 = 454;

pub struct A7800Bus {
    pub maria: Maria,
    pub tia: Tia,
    pub riot: Riot,
    pub cart: Cart7800,
    pub ram: Vec<u8>,            // 0x000-0x7FF = 1800-1FFF, 0x800-0xFFF = 2000-27FF
    /// MARIA clocks used by CPU accesses since the system last looked
    pub clocks: u32,
    pub cycles: u64,
    /// WSYNC written: the CPU waits for the end of the line
    pub wsync: bool,
    pub nmi_line: bool,
    /// player 1 buttons: [right (INPT0), left (INPT1)]
    pub buttons: [bool; 2],
    data_bus: u8,
}

fn ram_index(a: u16) -> Option<usize> {
    match a {
        0x0040..=0x00FF | 0x0140..=0x01FF => Some(0x800 + a as usize),
        0x1800..=0x1FFF => Some(a as usize - 0x1800),
        0x2000..=0x3FFF => Some(0x800 + (a as usize & 0x7FF)),
        _ => None,
    }
}

fn is_slow(a: u16) -> bool {
    matches!(a, 0x0000..=0x001F | 0x0100..=0x011F | 0x0280..=0x02FF | 0x0480..=0x04FF)
}

impl A7800Bus {
    pub fn new(cart: Cart7800, out_rate: u32) -> Self {
        Self {
            maria: Maria::new(), tia: Tia::new(out_rate), riot: Riot::new(), cart,
            ram: vec![0; 0x1000], clocks: 0, cycles: 0, wsync: false, nmi_line: false,
            buttons: [false; 2], data_bus: 0,
        }
    }

    /// What MARIA's DMA sees: RAM and cartridge, no side effects.
    pub fn peek(ram: &[u8], cart: &Cart7800, a: u16) -> u8 {
        match ram_index(a) {
            Some(i) => ram[i],
            None => cart.read(a),
        }
    }

    fn tia_read(&self, a: u16) -> u8 {
        let hi = match a & 0x0F {
            0x08 => if self.buttons[0] { 0x80 } else { 0 },                   // INPT0: right
            0x09 => if self.buttons[1] { 0x80 } else { 0 },                   // INPT1: left
            0x0C => if self.buttons[0] || self.buttons[1] { 0 } else { 0x80 }, // INPT4
            0x0D => 0x80,                                                     // INPT5
            _ => 0,
        };
        hi | (self.data_bus & 0x3F)
    }
}

impl AddressBus for A7800Bus {
    fn is_nmi_line_asserted(&mut self) -> bool { self.nmi_line }
    fn is_nmi_enabled(&mut self) -> bool { true }
    fn total_cycles(&self) -> u64 { self.cycles }
    fn is_irq_line_asserted(&mut self) -> bool { false }

    fn read_byte(&mut self, a: u16) -> u8 {
        self.clocks += if is_slow(a) { 6 } else { 4 };
        self.cycles += 1;
        let v = if let Some(i) = ram_index(a) {
            self.ram[i]
        } else {
            match a {
                0x0000..=0x001F | 0x0100..=0x011F => self.tia_read(a),
                0x0020..=0x003F | 0x0120..=0x013F => self.maria.read(a as u8),
                0x0280..=0x02FF => self.riot.read(a),
                0x0480..=0x04FF => self.riot.ram[(a & 0x7F) as usize],
                0x4000..=0xFFFF => self.cart.read(a),
                _ => self.data_bus,
            }
        };
        self.data_bus = v;
        v
    }

    fn write_byte(&mut self, a: u16, v: u8) {
        self.clocks += if is_slow(a) { 6 } else { 4 };
        self.cycles += 1;
        self.data_bus = v;
        if let Some(i) = ram_index(a) {
            self.ram[i] = v;
            return;
        }
        match a {
            // only the TIA's sound registers matter here (MARIA makes the picture)
            0x0015..=0x001A | 0x0115..=0x011A => self.tia.write(a as u8 & 0x1F, v),
            0x0020..=0x003F | 0x0120..=0x013F => {
                if self.maria.write(a as u8, v) { self.wsync = true; }
            }
            0x0280..=0x02FF => self.riot.write(a, v),
            0x0480..=0x04FF => self.riot.ram[(a & 0x7F) as usize] = v,
            0x4000..=0xFFFF => self.cart.write(a, v),
            _ => {}
        }
    }

    fn step_cycles(&mut self, cycles: u64) {
        self.clocks += 4 * cycles as u32;
        self.cycles += cycles;
    }

    fn begin_cpu_cycle(&mut self) {}
}
