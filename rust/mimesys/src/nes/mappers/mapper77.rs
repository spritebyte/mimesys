use crate::nes::mappers::{decode_state, encode_state, mirror_nt, Mapper, Mirroring};
use serde::{Deserialize, Serialize};

// iNES Mapper 77: Napoleon Senki (Irem)
// PRG: 32 KiB switchable bank at $8000-$FFFF
// CHR: 2 KiB switchable CHR-ROM bank at $0000-$07FF; CHR-RAM at $0800-$1FFF

pub struct Mapper77 {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_ram: Vec<u8>,
    s: State,
}

#[derive(Serialize, Deserialize, Clone)]
struct State {
    prg_bank: usize,
    chr_bank: usize,
    mirroring: u8,
    prg_ram: Vec<u8>,
    sram_dirty: bool,
}

impl Mapper77 {
    pub fn new(
        _mapper: u16,
        _submapper: u8,
        prg_rom: Vec<u8>,
        chr_rom: Vec<u8>,
        mirroring: Mirroring,
    ) -> Self {
        // Mapper 77 uses on-board CHR-RAM for $0800-$1FFF
        let chr_ram = vec![0; 8192];

        Self {
            prg_rom,
            chr_rom,
            chr_ram,
            s: State {
                prg_bank: 0,
                chr_bank: 0,
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
        self.prg_rom.len() / 0x8000 // 32 KiB banks
    }

    fn chr_bank_count(&self) -> usize {
        if self.chr_rom.is_empty() {
            0
        } else {
            self.chr_rom.len() / 0x800 // 2 KiB banks
        }
    }
}

impl Mapper for Mapper77 {
    fn cpu_read(&self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => self.s.prg_ram[(addr - 0x6000) as usize],
            0x8000..=0xFFFF => {
                let prg_count = self.prg_bank_count();
                if prg_count == 0 {
                    return 0;
                }
                let bank = self.s.prg_bank % prg_count;
                let offset = (addr as usize - 0x8000) & 0x7FFF;
                let index = (bank * 0x8000) + offset;
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
                // Register format: [CCCC PPPP]
                // Bits 3-0 (PPPP): 32K PRG bank select
                // Bits 7-4 (CCCC): 2K CHR bank select at $0000
                self.s.prg_bank = (value & 0x0F) as usize;
                self.s.chr_bank = ((value >> 4) & 0x0F) as usize;
            }
            _ => {}
        }
    }

    fn ppu_read(&self, addr: u16) -> u8 {
        let addr = addr as usize & 0x1FFF;
        if addr < 0x0800 {
            // Switchable 2K CHR-ROM bank at $0000-$07FF
            let chr_count = self.chr_bank_count();
            if chr_count == 0 {
                return 0;
            }
            let bank = self.s.chr_bank % chr_count;
            let index = (bank * 0x800) + (addr & 0x7FF);
            self.chr_rom.get(index % self.chr_rom.len()).copied().unwrap_or(0)
        } else {
            // Fixed CHR-RAM at $0800-$1FFF
            self.chr_ram.get(addr).copied().unwrap_or(0)
        }
    }

    fn ppu_write(&mut self, addr: u16, value: u8) {
        let addr = addr as usize & 0x1FFF;
        if addr >= 0x0800 {
            // CHR-RAM is writable at $0800-$1FFF
            self.chr_ram[addr] = value;
        }
        // $0000-$07FF maps to CHR-ROM and is read-only.
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