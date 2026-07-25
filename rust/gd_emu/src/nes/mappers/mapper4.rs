use crate::nes::mappers::{Mapper, Mirroring};
use serde::{Serialize, Deserialize};
use std::cell::Cell;
use godot::global::godot_print;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mmc3Revision {
    RevA,
    RevB,
    Tlsrom,
}

// Mapper 4 (MMC3)
// Mapper 118 (Tlsrom)
pub struct Mapper4 {
    prg_banks: usize, // Stored as count of 8KB banks
    chr_banks: usize,
    bank_registers: [usize; 8],
    bank_select: u8,
    prg_mode: u8,
    chr_mode: u8,
    prg_offsets: [usize; 4],
    chr_offsets: [usize; 8],
    
    debug_frame: u64,
    debug_frame_start_cycle: u64,
    debug_frame_started: bool,
    // Scanline IRQ counter fields wrapped in Cell for interior mutability
    a12_low_counter: Cell<u32>,
    last_a12_low_cycle: i64,
    last_a12_state: bool,   
    irq_counter: Cell<u8>,
    irq_latch: Cell<u8>,
    irq_reload_flag: Cell<bool>,
    irq_enabled: Cell<bool>,
    irq_active: Cell<bool>,
    last_clock_cycle: Cell<i64>,
    
    revision: Mmc3Revision,
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
struct Mapper4StateVariables {
    bank_registers: [usize; 8],
    bank_select: u8,
    prg_mode: u8,
    chr_mode: u8,
    last_a12_state: bool,
    a12_low_counter: u32,
    irq_counter: u8,
    irq_latch: u8,
    irq_reload_flag: bool,
    irq_enabled: bool,
    irq_active: bool,
    last_clock_cycle: i64,
    mirroring_mode: Mirroring,
    current_cycle: i64,
    prg_ram: Vec<u8>,
}

impl Mapper4 {
    pub fn new(_prg_banks: usize, chr_banks: usize, prg_rom: Vec<u8>, chr_rom: Vec<u8>, initial_mirroring: Mirroring, four_screen_bit: bool, mapper_id: u8, _submapper: u8) -> Self {
        let prg_ram = vec![0; 8192];
        let chr_ram = if chr_banks == 0 { vec![0; 8192] } else { vec![] };
        let variant = if mapper_id == 118 { Mmc3Revision::Tlsrom } else { Mmc3Revision::RevB };
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
            irq_latch: Cell::new(0),
            a12_low_counter: Cell::new(0),
            irq_counter: Cell::new(0),
            irq_reload_flag: Cell::new(false),
            irq_enabled: Cell::new(false),
            irq_active: Cell::new(false),
            last_a12_state: false,
            last_clock_cycle: Cell::new(0),
            last_a12_low_cycle: 0,
            revision: variant,
            mirroring_mode: initial_mirroring,
            has_four_screen: four_screen_bit,
            prg_rom,
            prg_ram,
            chr_rom,
            chr_ram,
            current_cycle: 0,
            sram_dirty: false,
            debug_frame: 0,
            debug_frame_start_cycle: 0,
            debug_frame_started: false,
        };
        
        mapper.recalculate_banks();
        mapper
    }

    /// Set the specific MMC3 hardware revision (useful for passing specific test ROMs)
    pub fn set_revision(&mut self, revision: Mmc3Revision) {
        self.revision = revision;
    }
    
fn recalculate_banks(&mut self) {
    // --- PRG Banking ---
    let last = self.prg_banks - 1;
    let second_last = self.prg_banks - 2;

    if self.prg_mode == 0 {
        // Mode 0: $8000 = R6, $A000 = R7, $C000 = Fixed to second-to-last, $E000 = Fixed to last
        self.prg_offsets[0] = (self.bank_registers[6] % self.prg_banks) * 0x2000;
        self.prg_offsets[1] = (self.bank_registers[7] % self.prg_banks) * 0x2000;
        self.prg_offsets[2] = second_last * 0x2000;
        self.prg_offsets[3] = last * 0x2000;
    } else {
        // Mode 1: $8000 = Fixed to second-to-last, $A000 = R7, $C000 = R6, $E000 = Fixed to last
        self.prg_offsets[0] = second_last * 0x2000;
        self.prg_offsets[1] = (self.bank_registers[7] % self.prg_banks) * 0x2000;
        self.prg_offsets[2] = (self.bank_registers[6] % self.prg_banks) * 0x2000;
        self.prg_offsets[3] = last * 0x2000;
    }

    // --- CHR Banking ---
    // Ensure 2KB banks explicitly ignore Bit 0 on standard MMC3 hardware
    let chr_mask: usize = if self.revision == Mmc3Revision::Tlsrom { 0x7F } else { 0xFF };
    let r0 = self.bank_registers[0] & 0xFE & chr_mask;
    let r1 = self.bank_registers[1] & 0xFE & chr_mask;
    let r2 = self.bank_registers[2];
    let r3 = self.bank_registers[3];
    let r4 = self.bank_registers[4];
    let r5 = self.bank_registers[5];

    if self.chr_mode == 0 {
        // Mode 0: 2KB banks at $0000-$0FFF, 1KB banks at $1000-$1FFF
        self.chr_offsets[0] = r0 * 0x0400;
        self.chr_offsets[1] = (r0 + 1) * 0x0400;
        self.chr_offsets[2] = r1 * 0x0400;
        self.chr_offsets[3] = (r1 + 1) * 0x0400;
        self.chr_offsets[4] = r2 * 0x0400;
        self.chr_offsets[5] = r3 * 0x0400;
        self.chr_offsets[6] = r4 * 0x0400;
        self.chr_offsets[7] = r5 * 0x0400;
    } else {
        // Mode 1 (Inverted): 1KB banks at $0000-$0FFF, 2KB banks at $1000-$1FFF
        self.chr_offsets[0] = r2 * 0x0400;
        self.chr_offsets[1] = r3 * 0x0400;
        self.chr_offsets[2] = r4 * 0x0400;
        self.chr_offsets[3] = r5 * 0x0400;
        self.chr_offsets[4] = r0 * 0x0400;
        self.chr_offsets[5] = (r0 + 1) * 0x0400;
        self.chr_offsets[6] = r1 * 0x0400;
        self.chr_offsets[7] = (r1 + 1) * 0x0400;
    }
}
    /// Maps a PPU nametable address ($2000-$2FFF) to its corresponding MMC3 CHR register index (0-5).
    /// This mirrors the CHR register lookup that determines the status of CHR A17 (acting as CIRAM A10).
    pub fn get_nametable_chr_register(&self, ppu_addr: u16) -> usize {
        // Standardize nametable address space down to a single pattern-table offset ($0000 - $0FFF)
        let offset = ppu_addr & 0x0FFF; 
    
        // Check MMC3 CHR inversion bit ($8000 bit 7)
//        let chr_inversion = (self.bank_select & 0x80) != 0;
        let chr_inversion = self.chr_mode != 0;

        if !chr_inversion {
        // --- Normal CHR Layout ---
        // $0000-$07FF: 2KB bank 0 (maps both $2000-$23FF and $2400-$27FF to Register 0)
        // $0800-$0FFF: 2KB bank 1 (maps both $2800-$2BFF and $2C00-$2FFF to Register 1)
            if offset < 0x0800 { 0 } else { 1 }
        } else {
            // --- Inverted CHR Layout ---
            // $0000-$0FFF: Divided into four 1KB pages mapping directly to registers 2, 3, 4, and 5
            // $2000-$23FF corresponds to offset $0000-$03FF (Register 2)
            // $2400-$27FF corresponds to offset $0400-$07FF (Register 3)
            // $2800-$2BFF corresponds to offset $0800-$0BFF (Register 4)
            // $2C00-$2FFF corresponds to offset $0C00-$0FFF (Register 5)
            match offset {
                0x0000..=0x03FF => 2,
                0x0400..=0x07FF => 3,
                0x0800..=0x0BFF => 4,
                0x0C00..=0x0FFF => 5,
                _ => unreachable!(),
            }
        }
    }

    fn clock_irq_counter(&mut self, debug_frame: u64, ppu_cycles: u64) {
//        godot_print!("clock_scanline: counter={} latch={} enabled={} active={}", 
//            self.irq_counter.get(), self.irq_latch.get(), 
//            self.irq_enabled.get(), self.irq_active.get());
        if self.debug_frame >= 300 && self.debug_frame < 320 {
            let elapsed = ppu_cycles - self.debug_frame_start_cycle;
            godot_print!("IRQ fire: frame={} scanline={} dot={}",
                self.debug_frame, elapsed / 341, elapsed % 341);
        }
        let current_counter = self.irq_counter.get();
        let is_reload = current_counter == 0 || self.irq_reload_flag.get();

        if is_reload {
            self.irq_counter.set(self.irq_latch.get());
            self.irq_reload_flag.set(false);
        } else {
            self.irq_counter.set(current_counter.saturating_sub(1));
        }
//        godot_print!(
//            "MMC3_DIAG: clock_scanline counter={} reload={} enabled={} active={}",
//            self.irq_counter.get(), is_reload, self.irq_enabled.get(), self.irq_active.get()
//        );

        // --- REVISION SENSITIVE IRQ LOGIC ---
        match self.revision {
            Mmc3Revision::RevA => {
                // Rev A: Only trigger IRQ if we decremented to 0. Reloading with 0 does NOT trigger IRQ.
                if !is_reload && self.irq_counter.get() == 0 && self.irq_enabled.get() {
                    self.irq_active.set(true);
//                    godot_print!("MMC3_DIAG: *** IRQ FIRED (RevA) ***");
                }
            }
            Mmc3Revision::RevB | Mmc3Revision::Tlsrom => {
                // Rev B/C: Trigger IRQ if the counter is exactly 0 after the step (even on reload).
                if self.irq_counter.get() == 0 && self.irq_enabled.get() {
                    self.irq_active.set(true);
//                    godot_print!("MMC3_DIAG: *** IRQ FIRED (RevB) ***");
                }
            }
        }
    }
}

impl Mapper for Mapper4 {
    fn step_cycles(&mut self, cycles: u64) {
        self.current_cycle += cycles as i64;
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
                if !self.has_four_screen && self.revision != Mmc3Revision::Tlsrom {
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
                // $E000: Disable MMC3 IRQs and acknowledge pending interrupt
                self.irq_enabled.set(false);
                self.irq_active.set(false);
//                godot_print!("MMC3_DIAG: IRQ disabled+acked (counter={})", self.irq_counter.get());
                self.a12_low_counter.set(0);
            } else {
                // $E001: Enable MMC3 IRQs
                self.irq_enabled.set(true);
//                godot_print!("MMC3_DIAG: IRQ enabled (counter={}, latch={})", self.irq_counter.get(), self.irq_latch.get());
            }
        }
    }

    fn ppu_read(&self, p_addr: u16) -> u8 {
        let addr = p_addr & 0x3FFF;

        if addr < 0x2000 {
            let bank = (addr / 0x0400) as usize;
            let offset = self.chr_offsets[bank] + (addr & 0x03FF) as usize;
        
            if !self.chr_rom.is_empty() {
                return self.chr_rom[offset % self.chr_rom.len()];
            } else if !self.chr_ram.is_empty() {
                return self.chr_ram[offset % self.chr_ram.len()];
            }
        }
        0
    }

    fn ppu_write(&mut self, p_addr: u16, value: u8) {
        let addr = p_addr & 0x3FFF;

        if addr < 0x2000 {
            if !self.chr_ram.is_empty() {
                let bank = (addr / 0x0400) as usize;
                let offset = self.chr_offsets[bank] + (addr & 0x03FF) as usize;
                let len = self.chr_ram.len();
                self.chr_ram[offset % len] = value;
            }
        }
    }

    fn mirror_vram_address(&self, addr: u16) -> usize {
        if self.revision == Mmc3Revision::Tlsrom {
            let ppu_addr = addr & 0x2FFF;
    
            if ppu_addr >= 0x2000 {
                // --- MAPPER 118 DYNAMIC ROUTING ---
                // Determine which CHR register is mapped to this nametable window
                // based on standard MMC3 layout config ($8000 bit 7)
                let chr_register = self.get_nametable_chr_register(ppu_addr);
        
                // Extract Bit 7 (A17) from that bank register to feed directly to CIRAM A10
                let a10 = (self.bank_registers[chr_register] & 0x80) != 0;
        
                // Calculate nametable offset
                let base = if a10 { 0x400 } else { 0x000 };
                let offset = ppu_addr & 0x03FF;
        
                (base + offset) as usize
            } else {
                // not sure about this, is it even reachable?
                (ppu_addr & 0x07FF) as usize
            }
        } else {
            let v = (addr - 0x2000) as usize & 0x0FFF; 
            if self.has_four_screen {
                return v;
            }
            if self.mirroring_mode == Mirroring::Vertical {
                return v & 0x07FF;
            }   else {
                return ((v >> 1) & 0x0400) | (v & 0x03FF);
            }
        }
    }

    fn save_state(&self) -> Vec<u8> {
        let variables = Mapper4StateVariables {
            bank_registers: self.bank_registers,
            bank_select: self.bank_select,
            prg_mode: self.prg_mode,
            chr_mode: self.chr_mode,
            last_a12_state: self.last_a12_state,
            a12_low_counter: self.a12_low_counter.get(),
            irq_counter: self.irq_counter.get(),
            irq_latch: self.irq_latch.get(),
            irq_reload_flag: self.irq_reload_flag.get(),
            irq_enabled: self.irq_enabled.get(),
            irq_active: self.irq_active.get(),
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
        if let Ok((state, _bytes_read)) = bincode::serde::decode_from_slice::<Mapper4StateVariables, _>(state_bytes, config) {
            self.bank_registers = state.bank_registers;
            self.bank_select = state.bank_select;
            self.prg_mode = state.prg_mode;
            self.chr_mode = state.chr_mode;
            self.last_a12_state = state.last_a12_state;
            self.a12_low_counter.set(state.a12_low_counter);
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

    fn update_a12(&mut self, addr: u16, _ppu_cycles: u64) {
        if self.debug_frame_started {
            self.debug_frame_start_cycle = _ppu_cycles;
            self.debug_frame_started = false;
        }
        let current_a12 = (addr & 0x1000) != 0;

        if !current_a12 {
            // Note when A12 went low
            if self.last_a12_state {
                self.last_a12_low_cycle = _ppu_cycles as i64;
            }
        } else {
            // A12 transitioned from 0 -> 1 (Rising Edge)
            if !self.last_a12_state {
                // Only clock the counter if A12 has been low for at least 3 CPU cycles
                if _ppu_cycles as i64 - self.last_a12_low_cycle > 10 {
                    self.clock_irq_counter(self.debug_frame, _ppu_cycles);
                }
            }
        }
        self.last_a12_state = current_a12;
    }
    fn notify_frame_start(&mut self) {
        self.debug_frame += 1;
        self.debug_frame_started = true;   // capture the cycle on the next A12 call
    }
}