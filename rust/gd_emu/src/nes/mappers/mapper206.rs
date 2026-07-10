use crate::nes::mappers::{Mapper, Mirroring};
use std::cell::Cell;

// Mapper 206 (DxROM)
pub struct Mapper206 {
    prg_banks: usize,
    chr_banks: usize,
    bank_registers: [usize; 8],
    bank_select: u8,
    prg_mode: u8,
    chr_mode: u8,
    prg_offsets: [usize; 4],
    chr_offsets: [usize; 8],
    
    mirroring_mode: Mirroring,
    has_four_screen: bool,
    prg_rom: Vec<u8>,
    prg_ram: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_ram: Vec<u8>,
    current_cycle: i64,
    sram_dirty: bool,
}

impl Mapper206 {
    pub fn new(prg_banks: usize, chr_banks: usize, prg_rom: Vec<u8>, chr_rom: Vec<u8>, initial_mirroring: Mirroring, four_screen_bit: bool) -> Self {
        let prg_ram = vec![0; 8192];
        let chr_ram = if chr_banks == 0 { vec![0; 8192] } else { vec![] };

        let mut mapper = Self {
            prg_banks,
            chr_banks,
            bank_registers: [0; 8],
            bank_select: 0,
            prg_mode: 0,
            chr_mode: 0,
            prg_offsets: [0; 4],
            chr_offsets: [0; 8],
            mirroring_mode: initial_mirroring,
            has_four_screen: four_screen_bit,
            prg_rom,
            prg_ram,
            chr_rom,
            chr_ram,
            current_cycle: 0,
            sram_dirty: false,
        };
        
        mapper.update_offsets();
        mapper
    }

    fn update_offsets(&mut self) {
        assert!(self.prg_banks >= 2);
        let last = self.prg_banks - 1;
        let second_last = self.prg_banks - 2;

        if self.prg_mode == 0 {
            self.prg_offsets[0] = self.bank_registers[6] * 0x2000;
            self.prg_offsets[1] = self.bank_registers[7] * 0x2000;
            self.prg_offsets[2] = second_last * 0x2000;
            self.prg_offsets[3] = last * 0x2000;
        } else {
            self.prg_offsets[0] = second_last * 0x2000;
            self.prg_offsets[1] = self.bank_registers[7] * 0x2000;
            self.prg_offsets[2] = self.bank_registers[6] * 0x2000;
            self.prg_offsets[3] = last * 0x2000;
        }

        if self.chr_mode == 0 {
            self.chr_offsets[0] = (self.bank_registers[0] & 0xFE) * 0x0400;
            self.chr_offsets[1] = self.chr_offsets[0] + 0x0400;
            self.chr_offsets[2] = (self.bank_registers[1] & 0xFE) * 0x0400;
            self.chr_offsets[3] = self.chr_offsets[2] + 0x0400;
            self.chr_offsets[4] = self.bank_registers[2] * 0x0400;
            self.chr_offsets[5] = self.bank_registers[3] * 0x0400;
            self.chr_offsets[6] = self.bank_registers[4] * 0x0400;
            self.chr_offsets[7] = self.bank_registers[5] * 0x0400;          
        } else {
            self.chr_offsets[4] = (self.bank_registers[0] & 0xFE) * 0x0400;
            self.chr_offsets[5] = self.chr_offsets[4] + 0x0400;
            self.chr_offsets[6] = (self.bank_registers[1] & 0xFE) * 0x0400;
            self.chr_offsets[7] = self.chr_offsets[6] + 0x0400;
            self.chr_offsets[0] = self.bank_registers[2] * 0x0400;
            self.chr_offsets[1] = self.bank_registers[3] * 0x0400;
            self.chr_offsets[2] = self.bank_registers[4] * 0x0400;
            self.chr_offsets[3] = self.bank_registers[5] * 0x0400;  
        }
    }
}

impl Mapper for Mapper206 {
    fn step_cycles(&mut self, cycles: u64) {
        self.current_cycle += cycles as i64;
    }

    fn cpu_read(&self, addr: u16) -> u8 {
        if addr >= 0x6000 && addr <= 0x7FFF {
            return self.prg_ram[(addr - 0x6000) as usize];
        }
        if addr >= 0x8000 && addr <= 0xFFFF {
            // Find which 8KB bank is targeted
            let bank = ((addr - 0x8000) / 0x2000) as usize;
            let offset = self.prg_offsets[bank] + ((addr - 0x8000) & 0x1FFF) as usize;
            return self.prg_rom[offset % self.prg_rom.len()];
        }
        0
    }

    fn cpu_write(&mut self, addr: u16, value: u8) {
        if addr >= 0x6000 && addr <= 0x7FFF {
            self.sram_dirty = true;
            self.prg_ram[(addr - 0x6000) as usize] = value;
        }
        else if addr >= 0x8000 && addr <= 0x9FFF {
            if (addr & 1) == 0 {
                // $8000: Bank Select configuration
                println!("{value} to {addr}, {0}", self.bank_registers[0]);
                self.bank_select = value & 0x07;
                self.prg_mode = (value >> 6) & 1;
                self.chr_mode = (value >> 7) & 1;
                self.update_offsets();
            } else {
                // $8001: Bank Register Data write
                let reg = self.bank_select as usize;
                
                match reg {
                    0..=5 => {
                        // Mask CHR register values to valid bank indices (e.g., if you have 8 CHR banks)
                        // Assuming your chr_banks is the number of 1KB banks
                        let mask = (self.chr_banks * 8) - 1; 
                        self.bank_registers[reg] = value as usize & mask;
                    }
                    6..=7 => {
                        // Mask PRG register values
                        let mask = (self.prg_banks * 2) - 1;
                        self.bank_registers[reg] = value as usize & mask;
                    }
                    _ => {}
                }
                self.update_offsets();
            }
        }
    }

    fn ppu_read(&self, p_addr: u16) -> u8 {
        let addr = p_addr & 0x3FFF;

        if addr < 0x2000 {
            let bank = (addr / 0x0400) as usize;
            let offset = self.chr_offsets[bank] + (addr & 0x03FF) as usize;
            if self.chr_rom.is_empty() {
                return self.chr_ram[offset % self.chr_ram.len()];
            } else {
                return self.chr_rom[offset % self.chr_rom.len()];
            }
        }
        0
    }

    fn ppu_write(&mut self, p_addr: u16, value: u8) {
        let addr = p_addr & 0x3FFF;

        if addr < 0x2000 {
            if self.chr_rom.is_empty() {
                let bank = (addr / 0x0400) as usize;
                let offset = self.chr_offsets[bank] + (addr & 0x03FF) as usize;
                let len = self.chr_ram.len();
                self.chr_ram[offset % len] = value;
            }
        }
    }

    fn mirror_vram_address(&self, addr: u16) -> usize {
        let v = (addr - 0x2000) as usize & 0x0FFF; 
        if self.has_four_screen {
            return v;
        }
        if self.mirroring_mode == Mirroring::Vertical {
            return v & 0x07FF;
        } else {
            return ((v >> 1) & 0x0400) | (v & 0x03FF);
        }
    }
}