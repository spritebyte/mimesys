use crate::nes::mappers::{
    decode_state, encode_state, mirror_nt, Mapper, Mirroring,
};
use serde::{Deserialize, Serialize};

// iNES Mapper 78: Irem 74HC161/32 (Holy Diver, Uchuusen - Cosmo Carrier)

pub struct Mapper78 {
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
    submapper: u8,
    sram_dirty: bool,
    prg_ram: Vec<u8>,
}

impl Mapper78 {
    pub fn new(
        _mapper: u16,
        submapper: u8,
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
                submapper,
                sram_dirty: false,
                prg_ram: vec![0; 8192],
            },
        }
    }

    fn prg_bank_count(&self) -> usize {
        self.prg_rom.len() / 0x4000 // 16 KiB banks
    }

    fn chr_bank_count(&self) -> usize {
        let len = if self.chr_rom.is_empty() {
            self.chr_ram.len()
        } else {
            self.chr_rom.len()
        };
        len / 0x2000 // 8 KiB banks
    }
}

impl Mapper for Mapper78 {
    fn cpu_read(&self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => self.s.prg_ram[(addr - 0x6000) as usize],
            0x8000..=0xFFFF => {
                let count = self.prg_bank_count();
                if count == 0 {
                    return 0;
                }
                let (bank, offset) = if addr < 0xC000 {
                    (self.s.prg_bank % count, (addr as usize - 0x8000))
                } else {
                    (count - 1, (addr as usize - 0xC000))
                };
                let index = (bank * 0x4000) + offset;
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
            0x8000..=0xFFFF => {
                // Implement AND bus conflicts: value written is bitwise ANDed with the ROM byte at addr
                let rom_val = if !self.prg_rom.is_empty() {
                    let index = (addr as usize - 0x8000) % self.prg_rom.len();
                    self.prg_rom[index]
                } else {
                    0xFF
                };
                let actual_value = value & rom_val;

                // Bit layout: CCCC MPPP
                // PPP (bits 0-2): 16K PRG bank
                // M (bit 3): Mirroring control
                // CCCC (bits 4-7): 8K CHR bank
                self.s.prg_bank = (actual_value & 0x07) as usize;
                self.s.mirroring_bit = (actual_value & 0x08) != 0;
                self.s.chr_bank = ((actual_value >> 4) & 0x0F) as usize;
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
        let bank = self.s.chr_bank % count;
        let index = (bank * 0x2000) + addr;
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
        if count > 0 {
            let bank = self.s.chr_bank % count;
            let index = (bank * 0x2000) + addr;
            let target = index % self.chr_ram.len();
            self.chr_ram[target] = value;
        }
    }

    fn mirror_vram_address(&self, addr: u16) -> usize {
        let mirroring = match self.s.submapper {
            1 => {
                // Uchuusen - Cosmo Carrier: 0 = SingleLower, 1 = SingleUpper
                if self.s.mirroring_bit {
                    Mirroring::SingleUpper
                } else {
                    Mirroring::SingleLower
                }
            }
            _ => {
                // Holy Diver (Submapper 3 or default): 0 = Horizontal, 1 = Vertical
                if self.s.mirroring_bit {
                    Mirroring::Vertical
                } else {
                    Mirroring::Horizontal
                }
            }
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