use crate::nes::mappers::{Mapper,Mirroring};
use serde::{Serialize, Deserialize};

// Mapper 66 (GNROM)
pub struct Mapper66 {
    prg_banks: u8,
    chr_banks: u8,
    mirroring_mode: Mirroring,
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    prg_ram: Vec<u8>,
    chr_ram: Vec<u8>,
    prg_bank: u8,
    chr_bank: u8,
}

#[derive(Serialize, Deserialize)]
struct Mapper66StateVariables {
    prg_banks: u8,
    chr_banks: u8,
    mirroring_mode: Mirroring,
    prg_ram: Vec<u8>, 
    chr_ram: Vec<u8>,
    prg_bank: u8,
    chr_bank: u8,
}

impl Mapper66 {
    pub fn new(prg_banks: usize, chr_banks: usize, prg_rom: Vec<u8>, chr_rom: Vec<u8>, initial_mirroring: Mirroring) -> Self {
        let chr_ram = if chr_banks == 0 { vec![0; 8192] } else { vec![] };
        let prg_bank_count = (prg_rom.len() / 0x8000) as u8;
        let chr_bank_count = chr_banks as u8;
        Self {
            prg_banks: prg_bank_count,
            chr_banks: chr_bank_count,
            mirroring_mode: initial_mirroring,
            prg_rom,
            chr_rom,
            prg_ram: vec![0; 8192],
            chr_ram,
            prg_bank: 0,
            chr_bank: 0,
        }
    }
}

impl Mapper for Mapper66 {
    fn cpu_read(&self, addr: u16) -> u8 {
        if addr < 0x8000 {
            if addr >= 0x6000 {
                return self.prg_ram[(addr - 0x6000) as usize];
            }
            return 0;
        }
        let bank_count = self.prg_banks.max(1) as usize;
        let bank = (self.prg_bank as usize) % bank_count;
        let offset = (addr & 0x7FFF) as usize;
        
        let mapped_addr = (bank * 0x8000) + offset;
        if mapped_addr < self.prg_rom.len() {
            self.prg_rom[mapped_addr]
        } else {
            0
        }
    }

    fn cpu_write(&mut self, addr: u16, value: u8) {
        if addr >= 0x8000 {
            self.chr_bank = value & 0x03;
            self.prg_bank = (value >> 4) & 0x03;
        } else if addr >= 0x6000 {
                self.prg_ram[(addr - 0x6000) as usize] = value;
        }
    }

    fn ppu_read(&self, p_addr: u16) -> u8 {
        let addr = p_addr & 0x3FFF;

        if addr < 0x2000 {
            if self.chr_banks == 0 {
                return self.chr_ram[addr as usize];
            } else {
                let bank_count = self.chr_banks.max(1) as usize;
                let bank = (self.chr_bank as usize) % bank_count;
                let mapped = (bank * 0x2000) + addr as usize;
                
                if mapped < self.chr_rom.len() {
                    return self.chr_rom[mapped];
                }
            }
        }
        0
    }

    fn ppu_write(&mut self, addr: u16, value: u8) {
        let addr = addr & 0x3FFF;
        if addr < 0x2000 {
            if self.chr_banks == 0 {
                self.chr_ram[addr as usize] = value;
            }
            // Real CHR-ROM is read-only, ignore writes otherwise
        }
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
        let variables = Mapper66StateVariables {
            prg_banks: self.prg_banks,
            chr_banks: self.chr_banks,
            mirroring_mode: self.mirroring_mode,
            prg_ram: self.prg_ram.clone(),
            chr_ram: self.chr_ram.clone(),
            chr_bank: self.chr_bank,
            prg_bank: self.prg_bank,
        };
        let config = bincode::config::standard().with_fixed_int_encoding();
        bincode::serde::encode_to_vec(&variables, config).unwrap_or_default()
    }

    fn load_state(&mut self, state_bytes: &[u8]) {
        let config = bincode::config::standard().with_fixed_int_encoding();
        if let Ok((variables, _bytes_read)) = bincode::serde::decode_from_slice::<Mapper66StateVariables, _>(state_bytes, config) {
            self.prg_banks = variables.prg_banks;
            self.chr_banks = variables.chr_banks;
            self.mirroring_mode = variables.mirroring_mode;
            self.prg_ram = variables.prg_ram;
            self.chr_ram = variables.chr_ram;
            self.chr_bank = variables.chr_bank;
            self.prg_bank = variables.prg_bank;
        }
    }
}