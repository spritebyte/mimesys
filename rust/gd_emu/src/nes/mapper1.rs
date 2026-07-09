use crate::nes::mappers::{Mapper,Mirroring};
use godot::global::godot_print;

// Mapper 1 (MMC1) 
pub struct Mapper1 {
    prg_banks: usize,
    chr_banks: usize,
    mirroring_mode: Mirroring,
    has_four_screen: bool,

    prg_rom: Vec<u8>,
    prg_ram: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_ram: Vec<u8>,

    shift_reg: u8,
    write_count: u8,
    control: u8,
    chr_bank_0: u8,
    chr_bank_1: u8,
    prg_bank: u8,
    sram_dirty: bool,
    last_write_cycle:i64,
    current_cycle: i64,
}

impl Mapper1 {
    pub fn new(prg_banks: usize, chr_banks: usize, prg_rom: Vec<u8>, chr_rom: Vec<u8>, initial_mirroring: Mirroring, four_screen_bit: bool) -> Self {
        let prg_ram = vec![0; 8192];
        let chr_ram = if chr_banks == 0 { vec![0; 8192] } else { vec![] };

        Self {
            prg_banks,
            chr_banks,
            mirroring_mode: initial_mirroring,
            has_four_screen: four_screen_bit,
            prg_rom,
            prg_ram,
            chr_rom,
            chr_ram,
            shift_reg: 0x10,
            control: 0x0C,
            chr_bank_0: 0,
            chr_bank_1: 0,
            prg_bank: 0,
            write_count: 0,
            sram_dirty: false,
            last_write_cycle: -1,
            current_cycle: 0,
        }
    }

    fn update_mirroring(&mut self) {
        let old_mirroring_mode = self.mirroring_mode;
        match self.control & 0x03 {
            0 => self.mirroring_mode = Mirroring::SingleLower,
            1 => self.mirroring_mode = Mirroring::SingleUpper,
            2 => self.mirroring_mode = Mirroring::Vertical,
            3 => self.mirroring_mode = Mirroring::Horizontal,
            _ => unreachable!(),
        }
    }
}

impl Mapper for Mapper1 {
    fn step_cycles(&mut self, cycles: u64) {
        self.current_cycle += cycles as i64;
    }

    fn total_cycles(&self) -> u64 { self.current_cycle as u64 }

    fn get_sram(&self) -> Option<&[u8]> {
        Some(&self.prg_ram)
    }

    fn load_sram(&mut self, data: &[u8]) {
        if data.len() == self.prg_ram.len() {
            self.prg_ram.copy_from_slice(data);
        }
    }

    fn is_sram_dirty(&self) -> bool {
        self.sram_dirty
    }

    fn clear_sram_dirty(&mut self) {
        self.sram_dirty = false;
    }

    fn cpu_read(&self, addr: u16) -> u8 {
        if addr >= 0x6000 && addr < 0x8000 {
            let ram_disabled = if self.prg_rom.len() == 524288 {
                false // SUROM ignores RAM disable; PRG-RAM is always on
            } else {
                (self.prg_bank & 0x10) != 0 // Standard MMC1 behavior
            };
            if ram_disabled {
                return 0x00;
            }
            if !self.prg_ram.is_empty() {
                return self.prg_ram[(addr - 0x6000) as usize];
            }
            return 0x00;
        }

        if addr >= 0x8000 && addr <= 0xFFFF {
            let prg_mode = (self.control >> 2) & 0x03;
//            godot_print!("prg_mode={:02X}, control={:02X}", prg_mode, self.control);
            let bank_size: usize = 16384;

            let mut surom_bank_ext = 0;
            if self.prg_rom.len() == 524288 {
                let chr_mode = (self.control >> 4) & 1;
                if chr_mode == 0 {
                    // 8KB CHR Mode: Bit 4 of CHR bank 0 controls the block selection
                    surom_bank_ext = ((self.chr_bank_0 & 0x10) >> 4) as usize;
                } else {
                    // 4KB CHR Mode: CHR bank 0 controls lower slot, CHR bank 1 controls upper slot
                    if addr < 0xC000 {
                        surom_bank_ext = ((self.chr_bank_0 & 0x10) >> 4) as usize;
                    } else {
                        surom_bank_ext = ((self.chr_bank_1 & 0x10) >> 4) as usize;
                    }
                }
            }                
            let prg_base_bank = surom_bank_ext << 4;

            let bank_idx = match prg_mode {
                0 | 1 => {
                    // Mode 0 & 1: Switch 32KB at $8000 (ignore lowest bit of bank selection)
                    let base = self.prg_bank as usize & 0xFE;
                    if addr < 0xC000 {
                        prg_base_bank + base
                    } else {
                        prg_base_bank + base + 1
                    }
                }
                2 => {
                    // Mode 2: Fix first bank at $8000, switch 16KB bank at $C000
                    if addr < 0xC000 {
                        prg_base_bank
                    } else {
                        prg_base_bank + (self.prg_bank & 0x0F) as usize
                    }
                }
                3 => {
                    // Mode 3: Switch 16KB bank at $8000, fix last bank at $C000
                    if addr < 0xC000 {
                        prg_base_bank + (self.prg_bank & 0x0F) as usize
                    } else {
                        if self.prg_rom.len() == 524288 {
                            prg_base_bank + 15 // Last bank of the 256KB SUROM block
                        } else {
                            self.prg_banks - 1 // Last bank of a standard sized ROM
                        }
                    }
                }
                _ => unreachable!(),
            };
            let offset = (bank_idx * 16384) + (addr & 0x3FFF) as usize;
            return self.prg_rom[offset % self.prg_rom.len()];
        }
        0
    }

    fn cpu_write(&mut self, addr: u16, value: u8) {
        if addr >= 0x6000 && addr < 0x8000 {
            let ram_disabled = if self.prg_rom.len() == 524288 {
                false // SUROM ignores RAM disable; PRG-RAM is always on
            } else {
                (self.prg_bank & 0x10) != 0 // Standard MMC1 behavior
            };
            if !ram_disabled && !self.prg_ram.is_empty() {
                let index = (addr - 0x6000) as usize % self.prg_ram.len();
                self.prg_ram[index] = value;
                self.sram_dirty = true;
            }
            return;
        }
        if addr < 0x8000 { return }

        let mut suppressed:bool = false;
        if self.last_write_cycle < 0 || self.last_write_cycle >= 0 && (self.current_cycle - self.last_write_cycle) <= 1 {
            if self.last_write_cycle < 0 { self.last_write_cycle = 0; }
            suppressed = true;
        }
/*        godot_print!("MMC1 write: cycle={} last_write_cycle={} addr={:04X} val={:02X} suppressed={} write_count={} -> control={:02X} chr0={:02X} chr1={:02X} prg={:02X}",
            self.current_cycle, self.last_write_cycle, addr, value, suppressed, self.write_count,
            self.control, self.chr_bank_0, self.chr_bank_1, self.prg_bank);
*/

        if suppressed {
            return; 
        }
        self.last_write_cycle = self.current_cycle;

        // 1. Reset check: If bit 7 is written, reset the shift register instantly
        if (value & 0x80) != 0 {
            self.shift_reg = 0x10;
            self.write_count = 0;
            self.control |= 0x0C;
            self.update_mirroring();
            return;
        }

        // 2. Otherwise, shift bit 0 of the value into our 5-bit register
        let bit = value & 0x01;
        // Shift right, inserting the new bit at position 4
        self.shift_reg = (self.shift_reg >> 1) | (bit << 4);
        self.write_count += 1;

        // 3. Once 5 bits are accumulated, write to the target internal register
        if self.write_count == 5 {
            // Determine register by looking at the CPU write address bits 13 and 14
            let target_reg = (addr >> 13) & 0x03;

            match target_reg {
                0 => { // $8000-$9FFF: Control Register
//                    godot_print!("Updating control register from {:02X} to {:02X}", self.control, self.shift_reg);
                    self.control = self.shift_reg;
                    self.update_mirroring();
                }
                1 => { // $A000-$BFFF: CHR Bank 0
                    self.chr_bank_0 = self.shift_reg;
                }
                2 => { // $C000-$DFFF: CHR Bank 1
                    self.chr_bank_1 = self.shift_reg;
                }
                3 => { // $E000-$FFFF: PRG Bank
//                    godot_print!("prg_bank updated from {:02X} to {:02X}", self.prg_bank, self.shift_reg);
                    self.prg_bank = self.shift_reg;
                }
                _ => unreachable!(),
            }

            // Reset loop
            self.shift_reg = 0x10;
            self.write_count = 0;
        }
    }

    fn ppu_read(&self, addr: u16) -> u8 {
        if addr < 0x2000 {
            // IF CHR-RAM exists, bypass all banking
            if self.chr_banks == 0 {
                return self.chr_ram[addr as usize % self.chr_ram.len()];
            }

            // CHR-ROM handling (4KB switching vs 8KB switching)
            let chr_mode = (self.control >> 4) & 0x01;
            if chr_mode == 1 {
                // Two separate 4KB banks
                if addr < 0x1000 {
                    let bank = self.chr_bank_0 as usize;
                    let offset = (bank * 4096) + addr as usize;
                    return self.chr_rom[offset % self.chr_rom.len()];
                } else {
                    let bank = self.chr_bank_1 as usize;
                    let offset = (bank * 4096) + (addr - 0x1000) as usize;
                    return self.chr_rom[offset % self.chr_rom.len()];
                }
            } else {
                // One unified 8KB bank (ignore lowest bit of bank register)
                let bank = (self.chr_bank_0 & 0xFE) as usize;
                let offset = (bank * 4096) + addr as usize;
                return self.chr_rom[offset % self.chr_rom.len()];
            }
        }
        0
    }

    fn ppu_write(&mut self, addr: u16, value: u8) {
        if addr < 0x2000 && !self.chr_ram.is_empty() {
            self.chr_ram[addr as usize] = value;
        }
    }


    fn mirror_vram_address(&self, addr: u16) -> usize {
        let normalized = (addr & 0x0FFF) as usize; // Map $2000-$2FFF to $000-$FFF
        if self.has_four_screen {
            return normalized;
        }

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
            Mirroring::SingleLower => normalized % 0x400,
            Mirroring::SingleUpper => 0x400 + (normalized % 0x400),
            _ => normalized,
        }
    }
}