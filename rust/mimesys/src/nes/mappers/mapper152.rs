
use crate::nes::mappers::{
    decode_state, encode_state, mirror_nt, Mapper, Mirroring,
};
use serde::{Deserialize, Serialize};

// iNES Mapper 152
// Bandai 74161/7432
//
// PRG: 16 KB switchable at $8000-$BFFF
//      16 KB fixed at $C000-$FFFF
//
// CHR: 8 KB switchable
//
// Register bits:
//   7    = Single-screen mirroring
//   6-4  = PRG bank
//   3-0  = CHR bank
//
// Bus conflicts: AND-type

pub struct Mapper152 {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_ram: Vec<u8>,
    s: State,
}

#[derive(Serialize, Deserialize, Clone)]
struct State {
    prg_bank: usize,
    chr_bank: usize,
    mirroring_bit: bool,
    sram_dirty: bool,
}

impl Mapper152 {
    pub fn new(
        _mapper: u16,
        _submapper: u8,
        prg_rom: Vec<u8>,
        chr_rom: Vec<u8>,
        _initial_mirroring: Mirroring,
    ) -> Self {
        let chr_ram = if chr_rom.is_empty() {
            vec![0; 8192]
        } else {
            Vec::new()
        };

        Self {
            prg_rom,
            chr_rom,
            chr_ram,
            s: State {
                prg_bank: 0,
                chr_bank: 0,
                mirroring_bit: false,
                sram_dirty: false,
            },
        }
    }

    fn prg_bank_count(&self) -> usize {
        self.prg_rom.len() / 0x4000
    }

    fn chr_bank_count(&self) -> usize {
        let len = if self.chr_rom.is_empty() {
            self.chr_ram.len()
        } else {
            self.chr_rom.len()
        };

        len / 0x2000
    }

    // Read the ROM byte currently visible at the CPU address.
    // This is used for bus-conflict calculation BEFORE
    // the bank register is updated.
    fn mapped_prg_read(&self, addr: u16) -> u8 {
        let count = self.prg_bank_count();

        if count == 0 {
            return 0xFF;
        }

        let bank = if addr < 0xC000 {
            self.s.prg_bank % count
        } else {
            count - 1
        };

        let offset = (addr as usize) & 0x3FFF;
        let index = bank * 0x4000 + offset;

        self.prg_rom.get(index).copied().unwrap_or(0xFF)
    }

    fn chr_index(&self, addr: u16) -> Option<usize> {
        let count = self.chr_bank_count();

        if count == 0 {
            return None;
        }

        let bank = self.s.chr_bank % count;
        let offset = (addr as usize) & 0x1FFF;

        Some(bank * 0x2000 + offset)
    }
}

impl Mapper for Mapper152 {
    fn cpu_read(&self, addr: u16) -> u8 {
        match addr {
            0x8000..=0xFFFF => self.mapped_prg_read(addr),
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, value: u8) {
        if addr < 0x8000 {
            return;
        }

        // Apply AND bus conflicts using the currently
        // mapped PRG ROM byte.
        let rom_value = self.mapped_prg_read(addr);
        let actual_value = value & rom_value;

        // Update all three register fields.
        self.s.chr_bank =
            (actual_value & 0x0F) as usize;

        self.s.prg_bank =
            ((actual_value >> 4) & 0x07) as usize;

        self.s.mirroring_bit =
            (actual_value & 0x80) != 0;
    }

    fn ppu_read(&self, addr: u16) -> u8 {
        let Some(index) = self.chr_index(addr) else {
            return 0;
        };

        if self.chr_rom.is_empty() {
            self.chr_ram.get(index).copied().unwrap_or(0)
        } else {
            self.chr_rom.get(index).copied().unwrap_or(0)
        }
    }

    fn ppu_write(&mut self, addr: u16, value: u8) {
        if !self.chr_rom.is_empty() {
            return;
        }

        if let Some(index) = self.chr_index(addr) {
            if let Some(byte) = self.chr_ram.get_mut(index) {
                *byte = value;
            }
        }
    }

    fn mirror_vram_address(&self, addr: u16) -> usize {
        let mirroring = if self.s.mirroring_bit {
            Mirroring::SingleUpper
        } else {
            Mirroring::SingleLower
        };

        mirror_nt(mirroring, addr)
    }

    // Standard Mapper 152 board has no PRG RAM.
    fn get_sram(&self) -> Option<&[u8]> {
        None
    }

    fn load_sram(&mut self, _data: &[u8]) {
    }

    fn is_sram_dirty(&self) -> bool {
        self.s.sram_dirty
    }

    fn clear_sram_dirty(&mut self) {
        self.s.sram_dirty = false;
    }

    fn save_state(&self) -> Vec<u8> {
        encode_state(&self.s)
    }

    fn load_state(&mut self, data: &[u8]) {
        if let Some(state) = decode_state::<State>(data) {
            self.s = state;
        }
    }
}
