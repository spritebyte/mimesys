use crate::nes::mappers::{Mapper,Mirroring};
use serde::{Serialize, Deserialize};

// Mapper 0 (NROM) - Standard flat cartridge, no bank switching
pub struct Mapper0 {
    prg_banks: u8,
    mirroring_mode: Mirroring,
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    prg_ram: Vec<u8>,
    chr_ram: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
struct Mapper0StateVariables {
    prg_banks: u8,
    mirroring_mode: Mirroring,
    prg_ram: Vec<u8>, 
    chr_ram: Vec<u8>,
}

impl Mapper0 {
    pub fn new(prg_banks: usize, chr_banks: usize, prg_rom: Vec<u8>, chr_rom: Vec<u8>, initial_mirroring: Mirroring) -> Self {
        let chr_ram = if chr_banks == 0 { vec![0; 8192] } else { vec![] };
        let num_prg_banks = prg_banks as u8;
        Self {
            prg_banks: num_prg_banks,
            mirroring_mode: initial_mirroring,
            prg_rom,
            chr_rom,
            prg_ram: vec![0; 8192],
            chr_ram,
        }
    }
}

impl Mapper for Mapper0 {
    fn cpu_read(&self, addr: u16) -> u8 {
        if addr < 0x8000 {
            if addr >= 0x6000 {
                return self.prg_ram[(addr - 0x6000) as usize];
            }
            return 0;
        }
        let mut rom_addr = addr - 0x8000;
        if self.prg_rom.len() == 16384 {
            rom_addr %= 16384; // Mirroring for 16KB games
        }
        self.prg_rom[rom_addr as usize]
    }
    fn cpu_write(&mut self, addr: u16, value: u8) {
        if addr < 0x8000 {
            if addr >= 0x6000 {
                // Write to the 8KB PRG RAM area
                self.prg_ram[(addr - 0x6000) as usize] = value;
            }
            return;
        }
        // ignore writes above $8000 for Mapper 0 since prg-rom is read-only.
    }

    fn ppu_read(&self, addr: u16) -> u8 {
        if self.chr_rom.len() > 0 {
            let masked_addr = (addr & 0x1FFF) as usize;
            return self.chr_rom[masked_addr % self.chr_rom.len()];
        }
        else {
            if addr < 0x2000 {
                return self.chr_ram[addr as usize % self.chr_ram.len()];
            }
        }
        0
    }

    fn ppu_write(&mut self, _addr: u16, _value: u8) {
        // handle chr_ram writes or modifications if needed
    }

    fn mirror_vram_address(&self, addr: u16) -> usize {
        let normalized = (addr & 0x0FFF) as usize; // Map $2000-$2FFF to $000-$FFF
        match self.mirroring_mode {
            Mirroring::Horizontal => {
                // Nametables 0 and 1 map to first 1KB; Nametables 2 and 3 map to second 1KB
                if normalized < 0x800 {
                    normalized % 0x400
                } else {
                    0x400 + (normalized % 0x400)
                }
            }
            Mirroring::Vertical => {
                // Nametables 0 and 2 map to first 1KB; Nametables 1 and 3 map to second 1KB
                normalized % 0x800
            }
            _ => normalized % 2048,
        }
    }
    fn save_state(&self) -> Vec<u8> {
        let variables = Mapper0StateVariables {
            prg_banks: self.prg_banks,
            mirroring_mode: self.mirroring_mode,
            prg_ram: self.prg_ram.clone(),
            chr_ram: self.chr_ram.clone(),
        };
        let config = bincode::config::standard().with_fixed_int_encoding();
        bincode::serde::encode_to_vec(&variables, config).unwrap_or_default()
    }

    fn load_state(&mut self, state_bytes: &[u8]) {
        let config = bincode::config::standard().with_fixed_int_encoding();
        if let Ok((variables, _bytes_read)) = bincode::serde::decode_from_slice::<Mapper0StateVariables, _>(state_bytes, config) {
            self.prg_banks = variables.prg_banks;
            self.mirroring_mode = variables.mirroring_mode;
            self.prg_ram = variables.prg_ram;
            self.chr_ram = variables.chr_ram;
        }
    }
}