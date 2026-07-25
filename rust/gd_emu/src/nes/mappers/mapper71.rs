use crate::nes::mappers::{Mapper,Mirroring};
use serde::{Serialize, Deserialize};

// Mapper 71 (Camerica)
pub struct Mapper71 {
    prg_banks: u8,
    prg_bank: u8,
    chr_banks: u8,
    mirroring_mode: Mirroring,
    has_four_screen: bool,
//    submapper: u8,
    has_bus_conflicts: bool,
    prg_rom: Vec<u8>,
    prg_ram: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_ram: Vec<u8>,
    current_cycle: i64,
    sram_dirty: bool,
}

#[derive(Serialize, Deserialize)]
pub struct Mapper71StateVariables {
    prg_banks: u8,
    mirroring_mode: Mirroring,
    prg_ram: Vec<u8>, 
    chr_ram: Vec<u8>,
}

impl Mapper71 {
    pub fn new(prg_banks: usize, chr_banks: usize, prg_rom: Vec<u8>, chr_rom: Vec<u8>, initial_mirroring: Mirroring, four_screen_bit: bool, submapper: u8) -> Self {
        let prg_ram = vec![0; 8192];
        let chr_ram = if chr_banks == 0 { vec![0; 8192] } else { vec![] };
        let has_bus_conflicts = if submapper == 0 || submapper == 2 { true } else { false };
        let num_banks = prg_banks as u8;
        let num_chr_banks = chr_banks as u8;
        Self {
            prg_banks: num_banks,
            prg_bank: 0,
            chr_banks: num_chr_banks,
            mirroring_mode: initial_mirroring,
            has_four_screen: four_screen_bit,
            has_bus_conflicts,
            prg_rom,
            prg_ram,
            chr_rom,
            chr_ram,
            current_cycle: 0,
            sram_dirty: false,
        }
    }

    fn get_rom_index(&self, addr: u16) -> usize {
        let offset = (addr - 0x8000) as usize;

        if addr < 0xC000 {
            (self.prg_bank as usize * 0x4000) + offset
        } else {
            let local_offset = offset - 0x4000;
            ((self.prg_banks - 1) as usize * 0x4000) as usize + local_offset
        }
    }
}

impl Mapper for Mapper71 {
    fn step_cycles(&mut self, cycles: u64) {
        self.current_cycle += cycles as i64;
    }

    fn cpu_read(&self, addr: u16) -> u8 {
        if addr < 0x2000 {
            return 0;
        }
        else if addr >= 0x6000 && addr <= 0x7FFF {
            return self.prg_ram[(addr - 0x6000) as usize];
        }
        else if addr >= 0x8000 && addr <= 0xBFFF {
            // Switchable bank
            let bank = (self.prg_bank % self.prg_banks) as usize;
            let offset = (addr - 0x8000) as usize;
            let target = (bank * 0x4000) as usize + offset;
            return self.prg_rom[target];
        }
        else if addr >= 0xC000 && addr <= 0xFFFF {
            // FIXED bank (always the last bank)
            let bank = (self.prg_banks - 1) as usize;
            let offset = (addr - 0xC000) as usize;
            let target = (bank * 0x4000) as usize + offset;
            return self.prg_rom[target]
        }
        0
    }

    fn cpu_write(&mut self, addr: u16, value: u8) {
        if addr >= 0x9000 && addr <= 0x9FFF {
            // Camerica 1-Screen Mirroring (Fire Hawk)
            let page = (value >> 4) & 1;
            self.mirroring_mode = if page == 0 {
                Mirroring::SingleLower
            } else {
                Mirroring::SingleUpper
            };
        } else if addr >= 0xC000 {
            self.prg_bank = value % self.prg_banks;
        }
    }

    fn ppu_read(&self, addr: u16) -> u8 {
        if addr < 0x2000 {
            if self.chr_banks == 0 {
                return self.chr_ram[addr as usize];
            } else {
                return self.chr_rom[addr as usize];
            }
        }
        0
    }

    fn ppu_write(&mut self, p_addr: u16, value: u8) {
        let addr = p_addr & 0x3FFF;

        if addr < 0x2000 {
            if self.chr_banks == 0 {
                self.chr_ram[addr as usize] = value;
            }
        }
    }


    fn mirror_vram_address(&self, addr: u16) -> usize {
        let normalized = (addr & 0x0FFF) as usize; // Map $2000-$2FFF to $000-$FFF
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
        let variables = Mapper71StateVariables {
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
        if let Ok((variables, _bytes_read)) = bincode::serde::decode_from_slice::<Mapper71StateVariables, _>(state_bytes, config) {
            self.prg_banks = variables.prg_banks;
            self.mirroring_mode = variables.mirroring_mode;
            self.prg_ram = variables.prg_ram;
            self.chr_ram = variables.chr_ram;
        }
    }
}