use crate::nes::mappers::{Mapper, Mirroring};
use serde::{Serialize, Deserialize};
use std::cell::Cell;
//use godot::global::godot_print;
/*
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mmc3Revision {
    RevA,
    RevB,
}
*/
// Mapper 64 (Rambo-1)
pub struct Mapper64 {
    prg_banks: usize, // Stored as count of 8KB banks
    chr_banks: usize,
    bank_registers: [usize; 8],
    bank_select: u8,
    prg_mode: u8,
    chr_mode: u8,
    prg_offsets: [usize; 4],
    chr_offsets: [usize; 8],

    cpu_cycle_prescaler: u32,
    irq_mode: u8, // 0 = Scanline, 1 = CPU Cycle
    ppu_a12_filter: u8,
    // Scanline IRQ counter fields wrapped in Cell for interior mutability
    last_a12: Cell<u8>,
    a12_low_counter: Cell<u32>,
    last_a12_state: bool,
    irq_counter: Cell<u8>,
    irq_latch: Cell<u8>,
    irq_reload_flag: Cell<bool>,
    irq_enabled: Cell<bool>,
    irq_active: Cell<bool>,
    irq_pending: bool,
    last_clock_cycle: Cell<i64>,
    irq_delay_cycles: i8,

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
struct Mapper64StateVariables {
    bank_registers: [usize; 8],
    bank_select: u8,
    prg_mode: u8,
    chr_mode: u8,
    last_a12: u8,
    last_a12_state: bool,
    ppu_a12_filter: u8,
    irq_mode: u8,
    a12_low_counter: u32,
    irq_counter: u8,
    irq_latch: u8,
    irq_reload_flag: bool,
    irq_enabled: bool,
    irq_pending: bool,
    irq_active: bool,
    irq_delay_cycles: i8,
    last_clock_cycle: i64,
    mirroring_mode: Mirroring,
    current_cycle: i64,
    prg_ram: Vec<u8>,
}

impl Mapper64 {
    pub fn new(_prg_banks: usize, chr_banks: usize, prg_rom: Vec<u8>, chr_rom: Vec<u8>, initial_mirroring: Mirroring, four_screen_bit: bool, _submapper: u8) -> Self {
        let prg_ram = vec![0; 8192];
        let chr_ram = if chr_banks == 0 { vec![0; 8192] } else { vec![] };

        // Robustly determine the actual number of 8KB PRG banks from the ROM size.
        let prg_banks_8kb = prg_rom.len() / 8192;

        let mut mapper = Self {
            prg_banks: prg_banks_8kb,
            chr_banks,
            bank_registers: [0; 8],
            bank_select: 0,
            prg_mode: 0,
            chr_mode: 0,
            prg_offsets: [0; 4],
            chr_offsets: [0; 8],
            irq_mode: 0,
            cpu_cycle_prescaler: 0,
            ppu_a12_filter: 0,
            irq_latch: Cell::new(0),
            a12_low_counter: Cell::new(0),
            irq_counter: Cell::new(0),
            irq_reload_flag: Cell::new(false),
            irq_enabled: Cell::new(false),
            irq_active: Cell::new(false),
            irq_pending: false,
            irq_delay_cycles: -1,
            last_a12: Cell::new(0),
            last_a12_state: false,
            last_clock_cycle: Cell::new(0),
            mirroring_mode: initial_mirroring,
            has_four_screen: four_screen_bit,
            prg_rom,
            prg_ram,
            chr_rom,
            chr_ram,
            current_cycle: 0,
            sram_dirty: false,
        };
        
        mapper.recalculate_banks();
        mapper
    }

   pub fn write_register(&mut self, address: u16, data: u8) {
        match address {
            0xC000..=0xDFFE if address % 2 == 0 => {
                self.irq_latch.set(data);
            }
            0xC001..=0xDFFF if address % 2 != 0 => {
                self.irq_mode = data & 0x01;
                self.irq_reload_flag.set(true);
                self.cpu_cycle_prescaler = 0; // Reset cycle mode prescaler
            }
            0xE000..=0xFFFE if address % 2 == 0 => {
                self.irq_enabled.set(false);
                self.irq_pending = false;
                self.irq_delay_cycles = -1;
            }
            0xE001..=0xFFFF if address % 2 != 0 => {
                self.irq_enabled.set(true);
            }
            _ => {}
        }
    }
    
    fn recalculate_banks(&mut self) {
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
    
    fn clock_irq_counter(&mut self) {
        if self.irq_reload_flag.get() {
            self.irq_counter.set(self.irq_latch.get());
            if self.irq_latch.get() != 0 {
                let counter = self.irq_counter.get();
                self.irq_counter.set(counter | 1); // Rambo-1 odd quirk
            }
            self.irq_reload_flag.set(false);
        } else if self.irq_counter.get() == 0 {
            self.irq_counter.set(self.irq_latch.get());
        } else {
            let counter = self.irq_counter.get();
            self.irq_counter.set(counter.wrapping_sub(1));
        }

        if self.irq_counter.get() == 0 && self.irq_enabled.get() {
            self.irq_delay_cycles = 4; // Trigger IRQ 4 CPU cycles later
        }
    }
}

impl Mapper for Mapper64 {
    fn step_cycles(&mut self, cycles: u64) {
        self.current_cycle += cycles as i64;
        if self.irq_delay_cycles > 0 {
            self.irq_delay_cycles -= 1;
            if self.irq_delay_cycles == 0 {
                self.irq_pending = true;
                self.irq_delay_cycles = -1;
            }
        }
        if self.irq_mode == 1 {
            self.cpu_cycle_prescaler += 1;
            if self.cpu_cycle_prescaler >= 4 {
                self.cpu_cycle_prescaler = 0;
                self.clock_irq_counter();
            }
        }
    }

    fn total_cycles(&self) -> u64 { self.current_cycle as u64 }

    fn is_irq_asserted(&self) -> bool {
        self.irq_active.get()
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
                self.bank_select = value & 0x07;
                self.prg_mode = (value >> 6) & 1;
                self.chr_mode = (value >> 7) & 1;
                self.recalculate_banks();
            } else {
                // $8001: Bank Register Data write
                self.bank_registers[self.bank_select as usize] = value as usize;
                self.recalculate_banks();
            }
        }
        else if addr >= 0xA000 && addr <= 0xBFFF {
            if (addr & 1) == 0 {
                // $A000: Mirroring Mode (0 = Vertical, 1 = Horizontal)
                if !self.has_four_screen {
                    self.mirroring_mode = if value & 1 == 0 {
                        Mirroring::Vertical
                    } else {
                        Mirroring::Horizontal
                    };
                }
            }
        }
        else if addr >= 0xC000 && addr <= 0xDFFF {
            if (addr & 1) == 0 {
                // $C000: IRQ Latch
                self.irq_latch.set(value);
            } else {
                // $C001: IRQ Reload Flag
                self.irq_reload_flag.set(true);
            }
        }
        else if addr >= 0xE000 && addr <= 0xFFFF {
            if (addr & 1) == 0 {
                self.irq_enabled.set(false);
                self.irq_active.set(false);
                self.a12_low_counter.set(0);
            } else {
                self.irq_enabled.set(true);
            }
        }
    }

    fn notify_scanline(&mut self) {
        let current_counter = self.irq_counter.get();
        let is_reload = current_counter == 0 || self.irq_reload_flag.get();

        if is_reload {
            self.irq_counter.set(self.irq_latch.get());
            self.irq_reload_flag.set(false);
        } else {
            self.irq_counter.set(current_counter.saturating_sub(1));
        }
        if self.irq_counter.get() == 0 && self.irq_enabled.get() {
            self.irq_active.set(true);
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
    fn save_state(&self) -> Vec<u8> {
        let variables = Mapper64StateVariables {
            bank_registers: self.bank_registers,
            bank_select: self.bank_select,
            prg_mode: self.prg_mode,
            chr_mode: self.chr_mode,
            irq_mode: self.irq_mode,
            irq_delay_cycles: self.irq_delay_cycles,
            last_a12: self.last_a12.get(),
            last_a12_state: self.last_a12_state,
            a12_low_counter: self.a12_low_counter.get(),
            ppu_a12_filter: self.ppu_a12_filter,
            irq_counter: self.irq_counter.get(),
            irq_latch: self.irq_latch.get(),
            irq_reload_flag: self.irq_reload_flag.get(),
            irq_enabled: self.irq_enabled.get(),
            irq_active: self.irq_active.get(),
            irq_pending: self.irq_pending,
            last_clock_cycle: self.last_clock_cycle.get(),
            mirroring_mode: self.mirroring_mode,
            current_cycle: self.current_cycle,
            prg_ram: self.prg_ram.clone(),
        };
        
        let config = bincode::config::standard().with_fixed_int_encoding();
        bincode::serde::encode_to_vec(&variables, config).unwrap_or_default()
    }

    fn load_state(&mut self, state_bytes: &[u8]) {
        let config = bincode::config::standard().with_fixed_int_encoding();
        if let Ok((state, _bytes_read)) = bincode::serde::decode_from_slice::<Mapper64StateVariables, _>(state_bytes, config) {
            self.bank_registers = state.bank_registers;
            self.bank_select = state.bank_select;
            self.prg_mode = state.prg_mode;
            self.chr_mode = state.chr_mode;
            self.last_a12.set(state.last_a12);
            self.last_a12_state = state.last_a12_state;
            self.irq_enabled.set(state.irq_enabled);
            self.irq_delay_cycles = state.irq_delay_cycles;
            self.irq_mode = state.irq_mode;
            self.a12_low_counter.set(state.a12_low_counter);
            self.ppu_a12_filter = state.ppu_a12_filter;
            self.irq_counter.set(state.irq_counter);
            self.irq_latch.set(state.irq_latch);
            self.irq_reload_flag.set(state.irq_reload_flag);
            self.irq_enabled.set(state.irq_enabled);
            self.irq_active.set(state.irq_active);
            self.last_clock_cycle.set(state.last_clock_cycle);
            self.mirroring_mode = state.mirroring_mode;
            self.current_cycle = state.current_cycle;
            if state.prg_ram.len() == self.prg_ram.len() {
                self.prg_ram = state.prg_ram;
            }
            self.recalculate_banks(); // rebuild derived prg_offsets/chr_offsets from restored registers
        }
    }

    fn update_a12(&mut self, addr: u16) {
        let current_a12 = (addr & 0x1000) != 0;

        if self.irq_mode == 0 {
            // Emulating the M2 falling edge filter evaluation
            if !self.last_a12_state && current_a12 {
                if self.ppu_a12_filter >= 16 {
                    self.clock_irq_counter();
                }
                self.ppu_a12_filter = 0;
            } else if !current_a12 {
                if self.ppu_a12_filter < 16 {
                    self.ppu_a12_filter += 1;
                }
            }
        }
        self.last_a12_state = current_a12;
    }
}