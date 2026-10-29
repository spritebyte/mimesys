use crate::nes::mappers::{Mapper, Mirroring, mirror_nt, encode_state, decode_state};
use serde::{Serialize, Deserialize};

// iNES Mapper 13: CPROM
// Features 32K unbanked PRG ROM and 16K CHR RAM with a 4KB swappable bank at PPU $1000-$1FFF.

pub struct Mapper13 {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_ram: Vec<u8>,
    s: State,
}

#[derive(Serialize, Deserialize, Clone)]
struct State {
    chr_bank: u8,          // 4KB CHR bank selector for PPU $1000-$1FFF (bits 0-1)
    mirroring: u8,
}

impl Mapper13 {
    pub fn new(_mapper: u16, _submapper: u8, prg_rom: Vec<u8>, chr_rom: Vec<u8>, mirroring: Mirroring) -> Self {
        // CPROM uses 16 KiB CHR RAM total (four 4 KiB banks)
        let chr_ram = if chr_rom.is_empty() { vec![0; 16384] } else { vec![] };
        Self {
            prg_rom, chr_rom, chr_ram,
            s: State {
                chr_bank: 0,
                mirroring: match mirroring {
                    Mirroring::Horizontal => 0,
                    Mirroring::Vertical => 1,
                    Mirroring::SingleLower => 2,
                    Mirroring::SingleUpper | Mirroring::FourScreen => 3,
                },
            },
        }
    }
}

impl Mapper for Mapper13 {
    fn cpu_read(&self, addr: u16) -> u8 {
        match addr {
            0x8000..=0xFFFF => {
                let mask = if self.prg_rom.len() <= 16384 { 0x3FFF } else { 0x7FFF };
                self.prg_rom[(addr as usize - 0x8000) & mask]
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, value: u8) {
        match addr {
            0x8000..=0xFFFF => {
                // Any write to $8000-$FFFF selects the 4KB CHR bank for $1000-$1FFF via bits 0-1
                self.s.chr_bank = value & 0x03;
            }
            _ => {}
        }
    }

    fn ppu_read(&self, addr: u16) -> u8 {
        let addr = addr as usize & 0x1FFF;
        let (bank, offset) = if addr < 0x1000 {
            (0, addr & 0xFFF) // PPU $0000-$0FFF: Fixed first 4 KiB CHR bank
        } else {
            ((self.s.chr_bank & 0x03) as usize, addr & 0xFFF) // PPU $1000-$1FFF: Swappable 4 KiB CHR bank
        };

        if self.chr_rom.is_empty() {
            let target = (bank * 0x1000 + offset) % self.chr_ram.len();
            self.chr_ram[target]
        } else {
            let target = (bank * 0x1000 + offset) % self.chr_rom.len();
            self.chr_rom[target]
        }
    }

    fn ppu_write(&mut self, addr: u16, value: u8) {
        let addr = addr as usize & 0x1FFF;
        let (bank, offset) = if addr < 0x1000 {
            (0, addr & 0xFFF)
        } else {
            ((self.s.chr_bank & 0x03) as usize, addr & 0xFFF)
        };

        if self.chr_rom.is_empty() {
            let target = (bank * 0x1000 + offset) % self.chr_ram.len();
            self.chr_ram[target] = value;
        }
    }

    fn mirror_vram_address(&self, addr: u16) -> usize {
        let m = match self.s.mirroring & 3 {
            0 => Mirroring::Horizontal,
            1 => Mirroring::Vertical,
            2 => Mirroring::SingleLower,
            _ => Mirroring::SingleUpper,
        };
        mirror_nt(m, addr)
    }

    fn save_state(&self) -> Vec<u8> { encode_state(&self.s) }
    fn load_state(&mut self, data: &[u8]) { if let Some(s) = decode_state(data) { self.s = s; } }
}