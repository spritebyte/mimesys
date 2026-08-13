use crate::nes::mappers::{Mapper, Mirroring};
use serde::{Serialize, Deserialize};

use std::cell::Cell; // Import Cell for interior mutability on latches

// Mapper 9 (MMC2) - Punch-Out
// Mapper 10 (MMC4) - Fire Emblem Gaiden
pub struct Mapper9 {
    prg_banks: usize,
    prg_banks_16k: usize,  // For MMC4 (16KB chunks)
    prg_banks_8k: usize,   // For MMC2 (8KB chunks)
    chr_banks_4k: usize,
    prg_bank: u8,
    chr_rom_size: usize,
    // CHR Registers (two sets of 4KB banks for Left Page, two for Right Page)
    chr_fd_0: u8,
    chr_fe_0: u8,
    chr_fd_1: u8,
    chr_fe_1: u8,
    
    // Wrapped in Cell so we can mutate them inside the immutable &self ppu_read function
    latch_0: Cell<u8>,
    latch_1: Cell<u8>,
    
    mirroring_mode: Mirroring,
    has_four_screen: bool,

    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    prg_ram: Vec<u8>,
    chr_ram: Vec<u8>,
    current_cycle: i64,
    sram_dirty: bool,
    is_mmc4: bool,
//    has_battery: bool,
}

#[derive(Serialize, Deserialize)]
struct Mapper9StateVariables {
    prg_banks: u8,
    prg_banks_16k: usize,
    prg_banks_8k: usize,
    mirroring_mode: Mirroring,
    prg_ram: Vec<u8>, 
    chr_ram: Vec<u8>,
    is_mmc4: bool,
}

impl Mapper9 {
    pub fn new(prg_banks: usize, chr_banks: usize, prg_rom: Vec<u8>, chr_rom: Vec<u8>, initial_mirroring: Mirroring, four_screen_bit: bool, is_mmc4: bool) -> Self {
        let prg_ram = vec![0; 8192];
        let chr_ram = if chr_banks == 0 { vec![0; 8192] } else { vec![] };
        let chr_rom_size = chr_rom.len();
        let prg_len = prg_rom.len();

        Self {
            prg_banks,
            prg_banks_8k: prg_len / 0x2000,
            prg_banks_16k: prg_len / 0x4000,
            prg_bank: 0,
            chr_banks_4k: chr_rom_size / 0x1000,
            chr_rom_size,
            mirroring_mode: initial_mirroring,
            has_four_screen: four_screen_bit,
            prg_rom,
            prg_ram,
            chr_rom,
            chr_ram,
            current_cycle: 0,
            sram_dirty: false,
            chr_fd_0: 0,
            chr_fd_1: 0,
            chr_fe_0: 0,
            chr_fe_1: 0,
            latch_0: Cell::new(0xFD),
            latch_1: Cell::new(0xFD),
            is_mmc4,
        }
    }
    fn update_latches(&self, addr: u16) {
        if addr >= 0x0FD8 && addr <= 0x0FDF { self.latch_0.set(0xFD); }
        if addr >= 0x0FE8 && addr <= 0x0FEF { self.latch_0.set(0xFE); }
        if addr >= 0x1FD8 && addr <= 0x1FDF { self.latch_1.set(0xFD); }
        if addr >= 0x1FE8 && addr <= 0x1FEF { self.latch_1.set(0xFE); }
    }

    // Changed to &self to keep reads non-mutating
    fn _chr_read(&self, addr: usize) -> u8 {
        if self.chr_rom_size > 0 {
            self.chr_rom[addr % self.chr_rom_size]
        } else {
            self.chr_ram[addr % self.chr_ram.len()]
        }
    }
}

impl Mapper for Mapper9 {
    fn step_cycles(&mut self, cycles: u64) {
        self.current_cycle += cycles as i64;
    }

    fn cpu_read(&self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => {
                if self.is_mmc4 {
                    self.prg_ram[(addr - 0x6000) as usize]
                } else {
                    0 // MMC2 has no PRG RAM
                }
            }
            0x8000..=0xFFFF => {
                if self.is_mmc4 {
                    if addr < 0xC000 { // MMC4: 16KB Swappable at $8000, 16KB Fixed at $C000-$FFFF
                        let bank = (self.prg_bank as usize) % self.prg_banks_16k;
                        let offset = (addr - 0x8000) as usize;
                        self.prg_rom[(bank * 0x4000) + offset] as u8
                    } else {
                            // Fixed to the last 16KB bank of the PRG ROM
                        let last_bank_start = (self.prg_banks_16k - 1) * 0x4000;
                        let offset = (addr - 0xC000) as usize;
                        self.prg_rom[last_bank_start + offset] as u8
                    }
                }
                else {
                    if addr < 0xA000 { // MMC2: 8KB Swappable at $8000, 24KB Fixed at $A000-$FFFF
                        let bank = (self.prg_bank as usize) % self.prg_banks_8k;
                        let offset = (addr - 0x8000) as usize;
                        self.prg_rom[(bank * 0x2000) + offset] as u8
                    } else {
                        // Fixed to the last three 8KB banks of the PRG ROM
                        let last_three_start = (self.prg_banks_8k - 3) * 0x2000;
                        let offset = (addr - 0xA000) as usize;
                        self.prg_rom[last_three_start + offset] as u8
                    }
                }
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, value: u8) {
        if self.is_mmc4 && (0x6000..=0x7FFF).contains(&addr) {
            self.sram_dirty = true;
            self.prg_ram[(addr - 0x6000) as usize] = value;
            return;
        }

        match addr {
            0xA000..=0xAFFF => {
                // Select PRG Bank (value & 0x0F for MMC2, & 0x1F for MMC4)
                let mask = if !self.is_mmc4 { 0x0F } else { 0x1F };
                self.prg_bank = value & mask;
            }
            0xB000..=0xBFFF => self.chr_fd_0 = value & 0x1F, // Left page ($0000), FD bank
            0xC000..=0xCFFF => self.chr_fe_0 = value & 0x1F, // Left page ($0000), FE bank
            0xD000..=0xDFFF => self.chr_fd_1 = value & 0x1F, // Right page ($1000), FD bank
            0xE000..=0xEFFF => self.chr_fe_1 = value & 0x1F, // Right page ($1000), FE bank
            0xF000..=0xFFFF => {
                self.mirroring_mode = if (value & 1) != 0 {
                    Mirroring::Horizontal
                } else {
                    Mirroring::Vertical
                };
            }
            _ => {}
        }
    }

    fn ppu_read(&self, p_addr: u16) -> u8 {
        let addr = p_addr & 0x1FFF;
        let bank_idx: usize;

        if addr < 0x1000 {
            // --- LEFT CHR PAGE ($0000-$0FFF) ---
            let use_fd = self.latch_0.get() == 0xFD;
            let bank = if use_fd { self.chr_fd_0 } else { self.chr_fe_0 } as usize;
            bank_idx = bank % self.chr_banks_4k;

            // Check if this read updates the latch state AFTER the read completes
            if addr == 0x0FD8 {
                self.latch_0.set(0xFD);  // Future reads will use the FD bank
            } else if addr == 0x0FE8 {
                self.latch_0.set(0xFE); // Future reads will use the FE bank
            }
        } else {
            // --- RIGHT CHR PAGE ($1000-$1FFF) ---
            let use_fd = self.latch_1.get() == 0xFD;
            let bank = if use_fd { self.chr_fd_1 } else { self.chr_fe_1 } as usize;
            bank_idx = bank % self.chr_banks_4k;

            // Check if this read updates the latch state
            if (0x1FD8..=0x1FDF).contains(&addr) {
                self.latch_1.set(0xFD);
            } else if (0x1FE8..=0x1FEF).contains(&addr) {
                self.latch_1.set(0xFE);
            }
        }

        let offset = (addr & 0x0FFF) as usize;
        self.chr_rom[(bank_idx * 0x1000) + offset]
    }

    fn ppu_write(&mut self, p_addr: u16, value: u8) {
        let addr = p_addr & 0x3FFF;

        if addr < 0x2000 && self.chr_banks_4k == 0 {
            let bank = if addr < 0x1000 {
                if self.latch_0.get() == 0xFD { self.chr_fd_0 } else { self.chr_fe_0 }
            } else {
                if self.latch_1.get() == 0xFD { self.chr_fd_1 } else { self.chr_fe_1 }
            };
            let ram_addr = (bank as usize * 0x1000) + (addr % 0x1000) as usize;
            if ram_addr < self.chr_ram.len() {
                self.chr_ram[ram_addr] = value;
            }
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
            Mirroring::Vertical => normalized % 0x800,
            Mirroring::SingleLower => normalized % 0x400,
            Mirroring::SingleUpper => 0x400 + (normalized % 0x400),
            _ => normalized,
        }
    }

    fn save_state(&self) -> Vec<u8> {
        let variables = Mapper9StateVariables {
            prg_banks: self.prg_banks as u8,
            prg_banks_8k: self.prg_banks_8k,
            prg_banks_16k: self.prg_banks_16k,
            mirroring_mode: self.mirroring_mode,
            prg_ram: self.prg_ram.clone(),
            chr_ram: self.chr_ram.clone(),
            is_mmc4: self.is_mmc4,
        };
        
        let config = bincode::config::standard().with_fixed_int_encoding();
        bincode::serde::encode_to_vec(&variables, config).unwrap_or_default()
    }

    fn load_state(&mut self, state_bytes: &[u8]) {
        let config = bincode::config::standard().with_fixed_int_encoding();
        if let Ok((variables, _bytes_read)) = bincode::serde::decode_from_slice::<Mapper9StateVariables, _>(state_bytes, config) {
            self.prg_banks = variables.prg_banks as usize;
            self.prg_banks_16k = variables.prg_banks_16k;
            self.prg_banks_8k = variables.prg_banks_8k;
            self.mirroring_mode = variables.mirroring_mode;
            self.prg_ram = variables.prg_ram;
            self.chr_ram = variables.chr_ram;
            self.is_mmc4 = variables.is_mmc4;
        }
    }
}