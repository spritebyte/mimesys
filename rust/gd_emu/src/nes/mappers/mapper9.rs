use crate::nes::mappers::{Mapper, Mirroring};
use std::cell::Cell; // Import Cell for interior mutability on latches

// Mapper 9 (MMC2)
pub struct Mapper9 {
    prg_banks: usize,
    prg_bank_count: usize,
    prg_bank: u8,
    chr_banks: usize,
    chr_rom_size: usize,
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
    prg_ram: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_ram: Vec<u8>,
    current_cycle: i64,
    sram_dirty: bool,
}

impl Mapper9 {
    pub fn new(prg_banks: usize, chr_banks: usize, prg_rom: Vec<u8>, chr_rom: Vec<u8>, initial_mirroring: Mirroring, four_screen_bit: bool) -> Self {
        let prg_ram = vec![0; 8192];
        let chr_ram = if chr_banks == 0 { vec![0; 8192] } else { vec![] };
        let chr_rom_size = chr_rom.len();
        let prg_bank_count = prg_rom.len() / 0x2000;

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
            chr_fd_0: 0,
            chr_fd_1: 0,
            chr_fe_0: 0,
            chr_fe_1: 0,
            latch_0: Cell::new(0xFD),
            latch_1: Cell::new(0xFD),
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
        if addr < 0x8000 { return 0; }
        let offset = (addr & 0x1FFF) as usize;

        if addr < 0xA000 {
            // $8000-$9FFF: 8 KB switchable PRG ROM bank
            let bank = self.prg_bank as usize;
            return self.prg_rom[(bank * 0x2000) + offset];
        }
        else if addr < 0xC000 {
            // $A000-$BFFF: Fixed to third-to-last 8KB bank
            return self.prg_rom[((self.prg_bank_count - 3) * 0x2000) + offset];
        }
        else if addr < 0xE000 {
            // $C000-$DFFF: Fixed to second-to-last 8KB bank
            return self.prg_rom[((self.prg_bank_count - 2) * 0x2000) + offset];
        }
        // $E000-$FFFF: Fixed to the last 8KB bank
        return self.prg_rom[((self.prg_bank_count - 1) * 0x2000) + offset];
    }

    fn cpu_write(&mut self, addr: u16, value: u8) {
        if addr < 0xA000 { return; }

        if addr < 0xB000 {
            self.prg_bank = value & 0x0F; // 4-bit PRG selection register
        }
        else if addr < 0xC000 {
            self.chr_fd_0 = value & 0x1F; // 5-bit CHR selection registers
        }
        else if addr < 0xD000 {
            self.chr_fe_0 = value & 0x1F;
        }
        else if addr < 0xE000 {
            self.chr_fd_1 = value & 0x1F;
        }
        else if addr < 0xF000 {
            self.chr_fe_1 = value & 0x1F;
        }
        else {
            // $F000-$FFFF: Controls Mirroring mode exclusively
            self.mirroring_mode = if (value & 1) == 0 { Mirroring::Vertical } else { Mirroring::Horizontal };
        }
    }

    fn ppu_read(&self, p_addr: u16) -> u8 {
        let addr = p_addr & 0x3FFF;
        if addr < 0x2000 {
            // 1. Determine which 4KB CHR bank to use based on current latches
            let bank = if addr < 0x1000 {
                if self.latch_0.get() == 0xFD { self.chr_fd_0 } else { self.chr_fe_0 }
            } else {
                if self.latch_1.get() == 0xFD { self.chr_fd_1 } else { self.chr_fe_1 }
            };

            // 2. Fetch the target byte from the active bank
            let rom_addr = (bank as usize * 0x1000) + (addr % 0x1000) as usize;
            let val = self._chr_read(rom_addr);
//            let val = self.chr_rom[rom_addr % self.chr_rom_size];

            self.update_latches(addr);

            return val;
        }
        0
    }

    fn ppu_write(&mut self, p_addr: u16, value: u8) {
        let addr = p_addr & 0x3FFF;

        if addr < 0x2000 && self.chr_banks == 0 {
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
}