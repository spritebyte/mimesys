// SMS / Game Gear cartridge mappers. The Z80 sees three 16 KB slots of ROM:
//   slot 0 = 0000-3FFF, slot 1 = 4000-7FFF, slot 2 = 8000-BFFF
//
// Sega mapper (almost every game). Registers live at FFFC-FFFF (the write also lands in RAM):
//   FFFC  bit 3: cartridge RAM replaces slot 2, bit 2: which 16 KB RAM bank
//   FFFD  slot 0 bank (the first 1 KB, 0000-03FF, always stays on bank 0: interrupt vectors)
//   FFFE  slot 1 bank
//   FFFF  slot 2 bank
//
// Codemasters mapper (Micro Machines, Cosmic Spacehead, Fantastic Dizzy, ...):
//   writing to 0000 / 4000 / 8000 selects the bank for that slot. No 1 KB fixed area.
//   Detected by the checksum pair in the header at 7FE6/7FE8 (they add up to 0x10000).

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum MapperKind { Sega, Codemasters }

#[derive(Serialize, Deserialize, Clone)]
pub struct SmsMapper {
    pub kind: MapperKind,
    pub slots: [usize; 3],
    pub ram_control: u8,      // FFFC
    #[serde(with = "serde_bytes")]
    pub cart_ram: Vec<u8>,    // 32 KB: two 16 KB banks
    pub ram_used: bool,       // the game has turned cartridge RAM on at least once
    pub ram_dirty: bool,
    #[serde(skip)]
    rom: Vec<u8>,
}

impl SmsMapper {
    pub fn new(rom: Vec<u8>) -> Self {
        let kind = if is_codemasters(&rom) { MapperKind::Codemasters } else { MapperKind::Sega };
        let slots = if kind == MapperKind::Codemasters { [0, 1, 0] } else { [0, 1, 2] };
        Self { kind, slots, ram_control: 0, cart_ram: vec![0; 0x8000], ram_used: false, ram_dirty: false, rom }
    }

    /// Restore from a save state, keeping the ROM.
    pub fn load_state(&mut self, s: SmsMapper) {
        let rom = std::mem::take(&mut self.rom);
        *self = s;
        self.rom = rom;
    }

    pub fn rom_len(&self) -> usize { self.rom.len() }
    fn banks(&self) -> usize { self.rom.len().div_ceil(0x4000).max(1) }

    fn rom_at(&self, bank: usize, offset: usize) -> u8 {
        let bank = bank % self.banks();
        self.rom.get(bank * 0x4000 + offset).copied().unwrap_or(0xFF)
    }

    fn cart_ram_on(&self) -> bool { self.kind == MapperKind::Sega && self.ram_control & 0x08 != 0 }
    fn cart_ram_offset(&self, addr: u16) -> usize {
        (if self.ram_control & 0x04 != 0 { 0x4000 } else { 0 }) + (addr & 0x3FFF) as usize
    }

    /// 0000-BFFF
    pub fn read(&self, addr: u16) -> u8 {
        let off = (addr & 0x3FFF) as usize;
        match addr {
            0x0000..=0x03FF if self.kind == MapperKind::Sega => self.rom_at(0, off),
            0x0000..=0x3FFF => self.rom_at(self.slots[0], off),
            0x4000..=0x7FFF => self.rom_at(self.slots[1], off),
            _ if self.cart_ram_on() => self.cart_ram[self.cart_ram_offset(addr)],
            _ => self.rom_at(self.slots[2], off),
        }
    }

    /// Writes to 0000-BFFF (cartridge RAM, Codemasters bank registers).
    pub fn write(&mut self, addr: u16, val: u8) {
        match (self.kind, addr) {
            (MapperKind::Codemasters, 0x0000) => self.slots[0] = val as usize,
            (MapperKind::Codemasters, 0x4000) => self.slots[1] = val as usize,
            (MapperKind::Codemasters, 0x8000) => self.slots[2] = val as usize,
            (MapperKind::Sega, 0x8000..=0xBFFF) if self.cart_ram_on() => {
                let o = self.cart_ram_offset(addr);
                if self.cart_ram[o] != val { self.ram_dirty = true; }
                self.cart_ram[o] = val;
            }
            _ => {}
        }
    }

    /// Writes to FFFC-FFFF (Sega mapper registers).
    pub fn write_register(&mut self, addr: u16, val: u8) {
        if self.kind != MapperKind::Sega { return; }
        match addr {
            0xFFFC => {
                self.ram_control = val;
                if val & 0x08 != 0 { self.ram_used = true; }
            }
            0xFFFD => self.slots[0] = val as usize,
            0xFFFE => self.slots[1] = val as usize,
            0xFFFF => self.slots[2] = val as usize,
            _ => {}
        }
    }
}

/// Codemasters header: a checksum at 7FE6 and its inverse at 7FE8 that add up to 0x10000.
fn is_codemasters(rom: &[u8]) -> bool {
    if rom.len() < 0x8000 { return false; }
    let word = |a: usize| rom[a] as u32 | (rom[a + 1] as u32) << 8;
    let (sum, inv) = (word(0x7FE6), word(0x7FE8));
    sum != 0 && sum + inv == 0x10000
}
