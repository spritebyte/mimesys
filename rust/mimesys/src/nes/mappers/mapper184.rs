use crate::nes::mappers::{
    decode_state, encode_state, mirror_nt, Mapper, Mirroring,
};
use serde::{Deserialize, Serialize};

// iNES Mapper 184: Sunsoft-1 (Atlantis no Nazo, The Wing of Madoola)

pub struct Mapper184 {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_ram: Vec<u8>,
    s: State,
}

#[derive(Serialize, Deserialize, Clone)]
struct State {
    chr_bank_low: usize,
    chr_bank_high: usize,
    mirroring: u8,
}

impl Mapper184 {
    pub fn new(
        _mapper: u16,
        _submapper: u8,
        prg_rom: Vec<u8>,
        chr_rom: Vec<u8>,
        mirroring: Mirroring,
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
                chr_bank_low: 0,
                chr_bank_high: 0,
                mirroring: match mirroring {
                    Mirroring::Vertical => 0,
                    Mirroring::Horizontal => 1,
                    Mirroring::SingleLower => 2,
                    Mirroring::SingleUpper => 3,
                    Mirroring::FourScreen => 4,
                },
            },
        }
    }

    fn chr_bank_count(&self) -> usize {
        let len = if self.chr_rom.is_empty() {
            self.chr_ram.len()
        } else {
            self.chr_rom.len()
        };
        len / 0x1000 // 4 KiB banks
    }
}

impl Mapper for Mapper184 {
    fn cpu_read(&self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => {
                // Register range; no PRG-RAM on this board
                0
            }
            0x8000..=0xFFFF => {
                let prg_len = self.prg_rom.len();
                if prg_len == 0 {
                    return 0;
                }
                // 32 KiB fixed PRG ROM at $8000-$FFFF
                let index = (addr as usize - 0x8000) % prg_len;
                self.prg_rom[index]
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, value: u8) {
        match addr {
            0x6000..=0x7FFF => {
                // CHR ROM bank select register mapped into $6000-$7FFF
                // Bits 0-2 (LLL): Select 4K CHR bank from $0000-$0FFF
                // Bits 4-6 (HHH): Select 4K CHR bank from $1000-$1FFF
                self.s.chr_bank_low = (value & 0x07) as usize;
                self.s.chr_bank_high = ((value >> 4) & 0x07) as usize;
            }
            _ => {}
        }
    }

    fn ppu_read(&self, addr: u16) -> u8 {
        let addr = addr as usize & 0x1FFF;
        let count = self.chr_bank_count();
        if count == 0 {
            return 0;
        }

        let (bank, offset) = if addr < 0x1000 {
            (self.s.chr_bank_low % count, addr & 0x0FFF)
        } else {
            (self.s.chr_bank_high % count, addr & 0x0FFF)
        };

        let index = (bank * 0x1000) + offset;
        if self.chr_rom.is_empty() {
            self.chr_ram.get(index % self.chr_ram.len()).copied().unwrap_or(0)
        } else {
            self.chr_rom.get(index % self.chr_rom.len()).copied().unwrap_or(0)
        }
    }

    fn ppu_write(&mut self, addr: u16, value: u8) {
        if !self.chr_rom.is_empty() {
            return;
        }
        let addr = addr as usize & 0x1FFF;
        let count = self.chr_bank_count();
        if count == 0 {
            return;
        }

        let (bank, offset) = if addr < 0x1000 {
            (self.s.chr_bank_low % count, addr & 0x0FFF)
        } else {
            (self.s.chr_bank_high % count, addr & 0x0FFF)
        };

        let index = (bank * 0x1000) + offset;
        let target = index % self.chr_ram.len();
        self.chr_ram[target] = value;
    }

    fn mirror_vram_address(&self, addr: u16) -> usize {
        let mirroring = match self.s.mirroring {
            0 => Mirroring::Vertical,
            1 => Mirroring::Horizontal,
            2 => Mirroring::SingleLower,
            3 => Mirroring::SingleUpper,
            4 => Mirroring::FourScreen,
            _ => Mirroring::Horizontal,
        };
        mirror_nt(mirroring, addr)
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