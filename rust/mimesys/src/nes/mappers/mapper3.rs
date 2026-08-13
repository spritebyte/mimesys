use crate::nes::mappers::{Mapper, Mirroring};
use serde::{Serialize, Deserialize};

// Mapper 3 (CNROM)
// Implements Bus Conflicts

pub struct Mapper3 {
    prg_banks: usize,
    chr_bank: u8,
    chr_banks: usize,
    mirroring_mode: Mirroring,
    has_four_screen: bool,

    prg_rom: Vec<u8>,
    prg_ram: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_ram: Vec<u8>,
    current_cycle: i64,
    sram_dirty: bool,
}

#[derive(Serialize, Deserialize)]
struct Mapper3StateVariables {
    prg_banks: u8,
    mirroring_mode: Mirroring,
    prg_ram: Vec<u8>, 
    chr_ram: Vec<u8>,
}

impl Mapper3 {
    pub fn new(prg_banks: usize, chr_banks: usize, prg_rom: Vec<u8>, chr_rom: Vec<u8>, initial_mirroring: Mirroring, four_screen_bit: bool) -> Self {
        let prg_ram = vec![0; 8192];
        let chr_ram = if chr_banks == 0 { vec![0; 8192] } else { vec![] };

        Self {
            prg_banks,
            chr_bank: 0,
            chr_banks,
            mirroring_mode: initial_mirroring,
            has_four_screen: four_screen_bit,
            prg_rom,
            prg_ram,
            chr_rom,
            chr_ram,
            current_cycle: 0,
            sram_dirty: false,
        }
    }
}

impl Mapper for Mapper3 {
    fn step_cycles(&mut self, cycles: u64) {
        self.current_cycle += cycles as i64;
    }

    fn cpu_read(&self, addr: u16) -> u8 {
        if addr >= 0x8000 && addr <= 0xFFFF {
            let mask = if self.prg_banks > 1 { 0x7FFF } else { 0x3FFF };
            return self.prg_rom[(addr & mask) as usize];
        }
        0
    }

    fn cpu_write(&mut self, addr: u16, value: u8) {
        if addr >= 0x8000 {
            let mask = if self.prg_banks > 1 { 0x7FFF } else { 0x3FFF };
            let rom_value = self.prg_rom[(addr & mask) as usize];
            
            // CNROM Bus Conflict emulation (ANDing value with ROM data)
            let final_value = value & rom_value;
            
            if self.chr_banks > 0 {
                self.chr_bank = final_value & 0x03;
            }
        }
    }

    fn ppu_read(&self, p_addr: u16) -> u8 {
        let addr = p_addr & 0x3FFF;

        if addr < 0x2000 {
            if self.chr_banks == 0 {
                return self.chr_ram[addr as usize];
            } else {
                let bank = self.chr_bank as usize % self.chr_banks;
                let mapped = (bank * 0x2000) + addr as usize;
                return self.chr_rom[mapped];
            }
        }
        0
    }

    fn ppu_write(&mut self, p_addr: u16, value: u8) {
        let addr = p_addr & 0x3FFF;
        // Support writing to CHR-RAM if the game requires it
        if addr < 0x2000 && self.chr_banks == 0 {
            self.chr_ram[addr as usize] = value;
        }
    }

    fn mirror_vram_address(&self, addr: u16) -> usize {
        let normalized = (addr & 0x0FFF) as usize; // Standardized with Mapper 2
        if self.has_four_screen {
            return normalized;
        }

        match self.mirroring_mode {
            Mirroring::Horizontal => {
                if normalized < 0x800 {
                    normalized % 0x400
                } else {
                    0x400 + (normalized % 0x400)
                }
            }
            Mirroring::Vertical => {
                normalized % 0x800
            }
            Mirroring::SingleLower => normalized % 0x400,
            Mirroring::SingleUpper => 0x400 + (normalized % 0x400),
            _ => normalized,
        }
    }
    fn save_state(&self) -> Vec<u8> {
        let variables = Mapper3StateVariables {
            prg_banks: self.prg_banks as u8,
            mirroring_mode: self.mirroring_mode,
            prg_ram: self.prg_ram.clone(),
            chr_ram: self.chr_ram.clone(),
        };
        
        let config = bincode::config::standard().with_fixed_int_encoding();
        bincode::serde::encode_to_vec(&variables, config).unwrap_or_default()
    }

    fn load_state(&mut self, state_bytes: &[u8]) {
        let config = bincode::config::standard().with_fixed_int_encoding();
        if let Ok((variables, _bytes_read)) = bincode::serde::decode_from_slice::<Mapper3StateVariables, _>(state_bytes, config) {
            self.prg_banks = variables.prg_banks as usize;
            self.mirroring_mode = variables.mirroring_mode;
            self.prg_ram = variables.prg_ram;
            self.chr_ram = variables.chr_ram;
        }
    }
}