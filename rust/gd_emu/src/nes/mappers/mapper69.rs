use crate::nes::mappers::Mapper;
use crate::nes::mappers::Mirroring;
use std::cell::Cell;

pub struct Mapper69 {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    prg_ram: [u8; 8192],

    // Command Latch Registers
    command_reg: u8,

    // Internal Registers
    chr_regs: [u8; 8],
    prg_regs: [u8; 4], // 0: $6000, 1: $8000, 2: $A000, 3: $C000
    mirroring_mode: u8,
    has_four_screen: bool,

    // IRQ System
    irq_enable: bool,
    irq_counter_enable: bool,
    irq_counter: Cell<u16>,
    irq_pending: Cell<bool>,
}

impl Mapper69 {
    pub fn new(_prg_banks: usize,
        _chr_banks: usize,
        prg_rom: Vec<u8>,
        chr_rom: Vec<u8>,
        _initial_mirroring: Mirroring,
        four_screen_bit: bool,submapper: u8) -> Self {
        let mut mapper = Self {
            prg_rom,
            chr_rom,
            prg_ram: [0; 8192],
            has_four_screen: four_screen_bit,
            command_reg: 0,
            chr_regs: [0, 1, 2, 3, 4, 5, 6, 7],
            prg_regs: [0, 0, 1, 2], // Default bank allocations
            mirroring_mode: 0,
            irq_enable: false,
            irq_counter_enable: false,
            irq_counter: Cell::new(0),
            irq_pending: std::cell::Cell::new(false),
        };
        mapper
    }

    fn read_chr(&self, addr: u16, _is_bg_fetch: bool) -> u8 {
        if addr < 0x2000 {
            let bank_idx = (addr / 0x0400) as usize;
            let bank = self.chr_regs[bank_idx] as usize;
            let chr_addr = (bank * 0x0400) + (addr & 0x03FF) as usize;
            
            if !self.chr_rom.is_empty() {
                self.chr_rom[chr_addr % self.chr_rom.len()]
            } else {
                0
            }
        } else {
            0
        }
    }
}

impl Mapper for Mapper69 {
    fn cpu_read(&self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => {
                let reg_val = self.prg_regs[0];
                let is_ram = (reg_val & 0x40) != 0;
                let ram_enabled = (reg_val & 0x80) != 0;

                if is_ram {
                    if ram_enabled {
                        self.prg_ram[(addr & 0x1FFF) as usize]
                    } else {
                        0 // Open bus fallback
                    }
                } else {
                    // Mapped PRG-ROM bank at $6000-$7FFF
                    let bank = (reg_val & 0x3F) as usize;
                    let prg_addr = (bank * 0x2000) + (addr & 0x1FFF) as usize;
                    self.prg_rom[prg_addr % self.prg_rom.len()]
                }
            }
            0x8000..=0x9FFF => {
                let bank = (self.prg_regs[1] & 0x3F) as usize;
                let prg_addr = (bank * 0x2000) + (addr & 0x1FFF) as usize;
                self.prg_rom[prg_addr % self.prg_rom.len()]
            }
            0xA000..=0xBFFF => {
                let bank = (self.prg_regs[2] & 0x3F) as usize;
                let prg_addr = (bank * 0x2000) + (addr & 0x1FFF) as usize;
                self.prg_rom[prg_addr % self.prg_rom.len()]
            }
            0xC000..=0xDFFF => {
                let bank = (self.prg_regs[3] & 0x3F) as usize;
                let prg_addr = (bank * 0x2000) + (addr & 0x1FFF) as usize;
                self.prg_rom[prg_addr % self.prg_rom.len()]
            }
            0xE000..=0xFFFF => {
                // Hardwired to the absolute last 8KB PRG-ROM bank
                let last_bank_offset = self.prg_rom.len() - 0x2000;
                let prg_addr = last_bank_offset + (addr & 0x1FFF) as usize;
                self.prg_rom[prg_addr]
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, value: u8) {
        match addr {
            0x6000..=0x7FFF => {
                let reg_val = self.prg_regs[0];
                if (reg_val & 0xC0) == 0xC0 { // RAM select + RAM enabled bits active
                    self.prg_ram[(addr & 0x1FFF) as usize] = value;
                }
            }
            0x8000..=0x9FFF => {
                self.command_reg = value & 0x0F;
            }
            0xA000..=0xBFFF => {
                match self.command_reg {
                    0x00..=0x07 => {
                        // CHR Banks 0-7 (1KB each)
                        self.chr_regs[self.command_reg as usize] = value;
                    }
                    0x08..=0x0B => {
                        // PRG Banks 0-3
                        self.prg_regs[(self.command_reg - 0x08) as usize] = value;
                    }
                    0x0C => {
                        // Nametable Mirroring control
                        self.mirroring_mode = value & 0x03;
                    }
                    0x0D => {
                        // IRQ Control
                        self.irq_counter_enable = (value & 0x01) != 0;
                        self.irq_enable = (value & 0x80) != 0;
                        self.irq_pending.set(false); // Any write to $0D clears IRQ
                    }
                    0x0E => {
                        // IRQ Counter Low Byte
                        let current = self.irq_counter.get();
                        self.irq_counter.set((current & 0xFF00) | (value as u16));
                    }
                    0x0F => {
                        // IRQ Counter High Byte
                        let current = self.irq_counter.get();
                        self.irq_counter.set((current & 0x00FF) | ((value as u16) << 8));
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn ppu_read(&self, addr: u16) -> u8 {
        if addr < 0x2000 {
            self.read_chr(addr, false)
        } else {
            0
        }
    }

    fn ppu_write(&mut self, addr: u16, value: u8) {

    }

    fn ppu_read_ctx(&self, addr: u16, is_bg_fetch: bool) -> u8 {
        if addr < 0x2000 {
            self.read_chr(addr, is_bg_fetch)
        } else {
            0
        }
    }

    fn read_nametable_byte(&self, addr: u16, ppu_vram: &[u8; 4096], is_attribute_byte: bool) -> u8 {
        let offset = (addr & 0x03FF) as usize;
        let nt_index = match self.mirroring_mode {
            0 => ((addr - 0x2000) / 0x0400) as usize % 2, // Vertical
            1 => ((addr - 0x2000) / 0x0800) as usize,     // Horizontal
            2 => 0,                                       // One Screen (Page A)
            3 => 1,                                       // One Screen (Page B)
            _ => 0,
        };
        ppu_vram[(nt_index * 0x0400) + offset]
    }

    fn is_irq_asserted(&self) -> bool {
        self.irq_pending.get()
    }

    fn step_cycles(&mut self, cycles: u64) {
        if self.irq_counter_enable {
            let mut counter = self.irq_counter.get();
            for _ in 0..cycles {
                if counter == 0 {
                    if self.irq_enable {
                        self.irq_pending.set(true);
                    }
                    counter = 0xFFFF; // Wrap around on cycle countdown completion
                } else {
                    counter -= 1;
                }
            }
            self.irq_counter.set(counter);
        }
    }

    fn mirror_vram_address(&self, addr: u16) -> usize {
        let normalized = (addr & 0x0FFF) as usize;
        if self.has_four_screen {
            return normalized;
        }
        normalized
    }
}
