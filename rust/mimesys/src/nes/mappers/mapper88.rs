
use crate::nes::mappers::{
    decode_state, encode_state, mirror_nt, Mapper, Mirroring,
};
use serde::{Deserialize, Serialize};

// iNES Mapper 88: Namcot-3433
// Namco 108 banking with PPU A12 connected to CHR A16.

pub struct Mapper88 {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_ram: Vec<u8>,
    s: State,
}

#[derive(Serialize, Deserialize, Clone)]
struct State {
    regs: [u8; 8],
    bank_select: u8,
    mirroring: u8,
    prg_ram: Vec<u8>,
    sram_dirty: bool,
}

impl Mapper88 {
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
                regs: [0; 8],
                bank_select: 0,
                mirroring: match mirroring {
                    Mirroring::Vertical => 0,
                    Mirroring::Horizontal => 1,
                    Mirroring::SingleLower => 2,
                    Mirroring::SingleUpper => 3,
                    Mirroring::FourScreen => 4,
                },
                prg_ram: vec![0; 8192],
                sram_dirty: false,
            },
        }
    }

    fn prg_bank_count(&self) -> usize {
        self.prg_rom.len() / 0x2000
    }

    fn chr_len(&self) -> usize {
        if self.chr_rom.is_empty() {
            self.chr_ram.len()
        } else {
            self.chr_rom.len()
        }
    }

    fn chr_bank_count(&self) -> usize {
        self.chr_len() / 0x400
    }

    // Namco 108 CHR register layout:
    //
    // R0: $0000-$07FF (2 KB)
    // R1: $0800-$0FFF (2 KB)
    // R2: $1000-$13FF (1 KB)
    // R3: $1400-$17FF (1 KB)
    // R4: $1800-$1BFF (1 KB)
    // R5: $1C00-$1FFF (1 KB)
    //
    // PPU A12 is connected to CHR A16 on Mapper 88.
    fn chr_index(&self, addr: usize) -> Option<usize> {
        let count = self.chr_bank_count();

        if count == 0 {
            return None;
        }

        let addr = addr & 0x1FFF;
        let slot = addr >> 10;
        let offset = addr & 0x03FF;

        let bank = match slot {
            0 | 1 => {
                // R0: 2 KB bank
                (self.s.regs[0] & 0xFE) as usize + slot
            }
            2 | 3 => {
                // R1: 2 KB bank
                (self.s.regs[1] & 0xFE) as usize + (slot - 2)
            }
            4 => self.s.regs[2] as usize,
            5 => self.s.regs[3] as usize,
            6 => self.s.regs[4] as usize,
            7 => self.s.regs[5] as usize,
            _ => 0,
        };

        // PPU A12 drives CHR A16:
        // $0000-$0FFF accesses the lower 64 KB region.
        // $1000-$1FFF accesses the upper 64 KB region.
        let bank = (bank & 0x3F)
            | if (addr & 0x1000) != 0 {
                0x40
            } else {
                0
            };

        Some((bank % count) * 0x400 + offset)
    }

    fn get_mirroring(&self) -> Mirroring {
        match self.s.mirroring {
            0 => Mirroring::Vertical,
            1 => Mirroring::Horizontal,
            2 => Mirroring::SingleLower,
            3 => Mirroring::SingleUpper,
            4 => Mirroring::FourScreen,
            _ => Mirroring::Horizontal,
        }
    }
}

impl Mapper for Mapper88 {
    fn cpu_read(&self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => {
                self.s.prg_ram[(addr - 0x6000) as usize]
            }

            0x8000..=0xFFFF => {
                let count = self.prg_bank_count();

                if count == 0 {
                    return 0;
                }

                // Namco 108 PRG layout:
                // $8000-$9FFF: R6
                // $A000-$BFFF: R7
                // $C000-$DFFF: second-to-last bank
                // $E000-$FFFF: last bank

                let bank = match addr {
                    0x8000..=0x9FFF => self.s.regs[6] as usize % count,
                    0xA000..=0xBFFF => self.s.regs[7] as usize % count,
                    0xC000..=0xDFFF => count.saturating_sub(2),
                    _ => count - 1,
                };

                let index = bank * 0x2000
                    + (addr as usize & 0x1FFF);

                self.prg_rom.get(index).copied().unwrap_or(0)
            }

            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, value: u8) {
        match addr {
            0x6000..=0x7FFF => {
                let index = (addr - 0x6000) as usize;
                self.s.prg_ram[index] = value;
                self.s.sram_dirty = true;
            }

            // Namco 108 register interface.
            // Only $8000-$9FFF is decoded.
            0x8000..=0x9FFF => {
                if (addr & 1) == 0 {
                    // Bank Select
                    self.s.bank_select = value;
                } else {
                    // Bank Data
                    let reg = (self.s.bank_select & 0x07) as usize;
                    self.s.regs[reg] = value;
                }
            }

            // No MMC3-style mirroring or PRG mode registers.
            _ => {}
        }
    }

    fn ppu_read(&self, addr: u16) -> u8 {
        let addr = addr as usize & 0x1FFF;

        match self.chr_index(addr) {
            Some(index) if self.chr_rom.is_empty() => {
                self.chr_ram[index].clone()
            }

            Some(index) => {
                self.chr_rom.get(index).copied().unwrap_or(0)
            }

            None => 0,
        }
    }

    fn ppu_write(&mut self, addr: u16, value: u8) {
        if !self.chr_rom.is_empty() {
            return;
        }

        let addr = addr as usize & 0x1FFF;

        if let Some(index) = self.chr_index(addr) {
            self.chr_ram[index] = value;
        }
    }

    fn mirror_vram_address(&self, addr: u16) -> usize {
        mirror_nt(self.get_mirroring(), addr)
    }

    fn get_sram(&self) -> Option<&[u8]> {
        Some(&self.s.prg_ram)
    }

    fn load_sram(&mut self, data: &[u8]) {
        let n = data.len().min(self.s.prg_ram.len());
        self.s.prg_ram[..n].copy_from_slice(&data[..n]);
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
