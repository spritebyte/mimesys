use crate::nes::mappers::{Mapper, Mirroring};
use serde::{Serialize, Deserialize};

// Mapper 185 (CNROM with Security Diodes / Chip Selects)
// Implements AND-type bus conflicts and CHR-ROM disabling based on submapper / security check.

pub struct Mapper185 {
    prg_banks: usize,
    chr_bank: u8,
    chr_banks: usize,
    submapper: u8,
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
struct Mapper185StateVariables {
    prg_banks: u8,
    chr_bank: u8,
    submapper: u8,
    mirroring_mode: Mirroring,
    prg_ram: Vec<u8>, 
    chr_ram: Vec<u8>,
}

impl Mapper185 {
    pub fn new(prg_banks: usize, chr_banks: usize, prg_rom: Vec<u8>, chr_rom: Vec<u8>, initial_mirroring: Mirroring, four_screen_bit: bool, submapper: u8) -> Self {
        let prg_ram = vec![0; 8192];
        let chr_ram = if chr_banks == 0 { vec![0; 8192] } else { vec![] };

        Self {
            prg_banks,
            chr_bank: 0,
            chr_banks,
            submapper,
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

impl Mapper for Mapper185 {
    fn step_cycles(&mut self, cycles: u64) {
        self.current_cycle += cycles as i64;
    }

    fn cpu_read(&self, addr: u16) -> u8 {
        if addr >= 0x8000 {
            let mask = if self.prg_banks > 1 { 0x7FFF } else { 0x3FFF };
            return self.prg_rom[(addr & mask) as usize];
        }
        0
    }

    fn cpu_write(&mut self, addr: u16, value: u8) {
        if addr >= 0x8000 {
            let mask = if self.prg_banks > 1 { 0x7FFF } else { 0x3FFF };
            let rom_value = self.prg_rom[(addr & mask) as usize];
            
            // Mapper 185 always has AND-type bus conflicts
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
                // Determine the allowed bank value based on submapper (security diodes)
                // Submapper 4: bank 0, Submapper 5: bank 1, Submapper 6: bank 2, Submapper 7: bank 3
                // For submapper 0 (unheadered ROMs like Mighty Bomb Jack), default to bank 1.
                let allowed_bank = match self.submapper {
                    4 => 0,
                    5 | 0 => 1, // Mighty Bomb Jack and standard unheadered Mapper 185 games expect bank 1
                    6 => 2,
                    7 => 3,
                    _ => 1,
                };

                // Enforce the hardware security diode check: CHR-ROM is only enabled on the correct bank
                if self.chr_bank == allowed_bank {
                    let mapped = (self.chr_bank as usize * 0x2000 + addr as usize) % self.chr_rom.len();
                    return self.chr_rom[mapped];
                } else {
                    return 0; // Disabled by security diodes
                }
            }
        }
        0
    }
    
    fn ppu_write(&mut self, p_addr: u16, value: u8) {
        let addr = p_addr & 0x3FFF;
        // Support writing to CHR-RAM if needed
        if addr < 0x2000 && self.chr_banks == 0 {
            self.chr_ram[addr as usize] = value;
        }
    }

    fn mirror_vram_address(&self, addr: u16) -> usize {
        let normalized = (addr & 0x0FFF) as usize;
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
        let variables = Mapper185StateVariables {
            prg_banks: self.prg_banks as u8,
            chr_bank: self.chr_bank,
            submapper: self.submapper,
            mirroring_mode: self.mirroring_mode,
            prg_ram: self.prg_ram.clone(),
            chr_ram: self.chr_ram.clone(),
        };
        
        let config = bincode::config::standard().with_fixed_int_encoding();
        bincode::serde::encode_to_vec(&variables, config).unwrap_or_default()
    }

    fn load_state(&mut self, state_bytes: &[u8]) {
        let config = bincode::config::standard().with_fixed_int_encoding();
        if let Ok((variables, _bytes_read)) = bincode::serde::decode_from_slice::<Mapper185StateVariables, _>(state_bytes, config) {
            self.prg_banks = variables.prg_banks as usize;
            self.chr_bank = variables.chr_bank;
            self.submapper = variables.submapper;
            self.mirroring_mode = variables.mirroring_mode;
            self.prg_ram = variables.prg_ram;
            self.chr_ram = variables.chr_ram;
        }
    }
}