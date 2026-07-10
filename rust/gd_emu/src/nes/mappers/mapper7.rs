use crate::nes::mappers::{Mapper, Mirroring};
use std::cell::Cell; // Import Cell for interior mutability on latches

// Mapper 7 (AxROM)
pub struct Mapper7 {
    prg_banks: usize,
    prg_bank_count: usize,
    prg_bank: u8,
    chr_banks: usize,
    chr_rom_size: usize,

    
    mirroring_mode: Mirroring,
    has_four_screen: bool,

    prg_rom: Vec<u8>,
    prg_ram: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_ram: Vec<u8>,

    current_cycle: i64,
    sram_dirty: bool,
}

impl Mapper7 {
    pub fn new(prg_banks: usize, chr_banks: usize, prg_rom: Vec<u8>, chr_rom: Vec<u8>, initial_mirroring: Mirroring, four_screen_bit: bool) -> Self {
        let prg_ram = vec![0; 8192];
        let chr_ram = if chr_banks == 0 { vec![0; 8192] } else { vec![] };
        let chr_rom_size = chr_rom.len();
        let prg_bank_count = prg_rom.len() / 0x8000; // 32KB banks

        Self {
            prg_banks,
            prg_bank_count,
            prg_bank: 0,
            chr_banks,
            chr_rom_size,
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

    fn _chr_read(&self, addr: usize) -> u8 {
        if self.chr_rom_size > 0 {
            self.chr_rom[addr % self.chr_rom_size]
        } else {
            self.chr_ram[addr % self.chr_ram.len()]
        }
    }
}

impl Mapper for Mapper7 {
    fn step_cycles(&mut self, cycles: u64) {
        self.current_cycle += cycles as i64;
    }

    fn total_cycles(&self) -> u64 {
        self.current_cycle as u64
    }

    fn cpu_read(&self, addr: u16) -> u8 {
        if addr < 0x8000 { return 0; }
        let bank = self.prg_bank as usize % self.prg_bank_count;
        let offset = (addr & 0x7FFF) as usize;

        return self.prg_rom[(bank * 0x8000) as usize + offset];
    }

    fn cpu_write(&mut self, addr: u16, value: u8) {
        if addr < 0x8000 { return; }

        self.prg_bank = value & 0x07;

        if (value & 0x10) == 0 {
            self.mirroring_mode = Mirroring::SingleLower;
        } else {
            self.mirroring_mode = Mirroring::SingleUpper;
        }
    }

    fn ppu_read(&self, p_addr: u16) -> u8 {
        let addr = p_addr & 0x3FFF;
        
        if addr < 0x2000 {
            return self._chr_read(addr as usize);
        }

        0
    }

    fn ppu_write(&mut self, p_addr: u16, value: u8) {
        let addr = p_addr & 0x3FFF;

        if addr < 0x2000 && self.chr_banks == 0 {
            self.chr_ram[(addr & 0x1FFF) as usize] = value;
        }
    }

    fn mirror_vram_address(&self, addr: u16) -> usize {
        let relative_addr = (addr & 0x0FFF) as usize;
        if self.has_four_screen {
            return relative_addr;
        }

        match self.mirroring_mode {
            Mirroring::Horizontal => {
                if relative_addr < 0x800 {
                    relative_addr % 0x400
                } else {
                    0x400 + (relative_addr % 0x400)
                }
            }
            Mirroring::Vertical => relative_addr % 0x800,
            Mirroring::SingleLower => relative_addr & 0x03FF,
            Mirroring::SingleUpper => 0x400 | (relative_addr & 0x03FF),
            _ => relative_addr,
        }
    }
}