use crate::nes::mappers::{decode_state, encode_state, mirror_nt, Mapper, Mirroring};
use serde::{Deserialize, Serialize};

// iNES Mapper 75: Konami VRC1
// PRG: Three switchable 8K banks at $8000, $A000, $C000; fixed last 8K bank at $E000
// CHR: Two switchable 4K banks at $0000 and $1000 (high bits configured via $9000)
// Mirroring: Controlled by bit 0 of $9000

pub struct Mapper75 {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_ram: Vec<u8>,
    s: State,
}

#[derive(Serialize, Deserialize, Clone)]
struct State {
    prg_regs: [u8; 3],
    chr_low: [u8; 2],
    reg9000: u8,
    mirroring: u8,
    prg_ram: Vec<u8>,
    sram_dirty: bool,
}

impl Mapper75 {
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
                prg_regs: [0, 0, 0],
                chr_low: [0, 0],
                reg9000: 0,
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
        self.prg_rom.len() / 0x2000 // 8 KiB banks
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

impl Mapper for Mapper75 {
    fn cpu_read(&self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => self.s.prg_ram[(addr - 0x6000) as usize],
            0x8000..=0xFFFF => {
                let prg_count = self.prg_bank_count();
                if prg_count == 0 {
                    return 0;
                }

                let slot = (addr as usize - 0x8000) / 0x2000;
                let bank = match slot {
                    0 => self.s.prg_regs[0] as usize,
                    1 => self.s.prg_regs[1] as usize,
                    2 => self.s.prg_regs[2] as usize,
                    _ => prg_count - 1, // Fixed last 8K bank at $E000-$FFFF
                } % prg_count;

                let offset = addr as usize & 0x1FFF;
                let index = (bank * 0x2000) + offset;
                self.prg_rom.get(index).copied().unwrap_or(0)
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, value: u8) {
        match addr {
            0x6000..=0x7FFF => {
                self.s.prg_ram[(addr - 0x6000) as usize] = value;
                self.s.sram_dirty = true;
            }
            0x8000..=0x8FFF => {
                self.s.prg_regs[0] = value & 0x0F;
            }
            0x9000..=0x9FFF => {
                self.s.reg9000 = value;
                self.s.mirroring = value & 0x01; // Bit 0: 0 = Vertical, 1 = Horizontal
            }
            0xA000..=0xAFFF => {
                self.s.prg_regs[1] = value & 0x0F;
            }
            0xC000..=0xCFFF => {
                self.s.prg_regs[2] = value & 0x0F;
            }
            0xE000..=0xEFFF => {
                self.s.chr_low[0] = value & 0x0F;
            }
            0xF000..=0xFFFF => {
                self.s.chr_low[1] = value & 0x0F;
            }
            _ => {}
        }
    }

    fn ppu_read(&self, addr: u16) -> u8 {
        let addr = addr as usize & 0x1FFF;
        let chr_count = self.chr_bank_count();
        if chr_count == 0 {
            return 0;
        }

        let (bank, offset) = if addr < 0x1000 {
            let high = ((self.s.reg9000 & 0x02) << 3) as usize;
            let bank = (self.s.chr_low[0] as usize | high) % chr_count;
            (bank, addr & 0x0FFF)
        } else {
            let high = ((self.s.reg9000 & 0x04) << 2) as usize;
            let bank = (self.s.chr_low[1] as usize | high) % chr_count;
            (bank, addr & 0x0FFF)
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
        let chr_count = self.chr_bank_count();
        if chr_count > 0 {
            let (bank, offset) = if addr < 0x1000 {
                let high = ((self.s.reg9000 & 0x02) << 3) as usize;
                let bank = (self.s.chr_low[0] as usize | high) % chr_count;
                (bank, addr & 0x0FFF)
            } else {
                let high = ((self.s.reg9000 & 0x04) << 2) as usize;
                let bank = (self.s.chr_low[1] as usize | high) % chr_count;
                (bank, addr & 0x0FFF)
            };

            let index = (bank * 0x1000) + offset;
            let target = index % self.chr_ram.len();
            self.chr_ram[target] = value;
        }
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