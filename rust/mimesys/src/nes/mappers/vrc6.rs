use crate::nes::mappers::{Mapper, Mirroring};
use serde::{Serialize, Deserialize};
use std::cell::Cell;

// VRC6 Mapper (iNES Mappers 24 and 26)
// Used in games like Akumajou Densetsu (Castlevania III - JP) and Madara.

pub struct MapperVRC6 {
    prg_rom: Vec<u8>,
    prg_ram: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_ram: Vec<u8>,

    prg_bank_16k: usize,
    prg_bank_8k: usize,
    chr_regs: [u8; 8],

    b003: u8,
    initial_mirroring: Mirroring,
    has_four_screen: bool,
    is_mapper_26: bool, // true for mapper 26 (VRC6b), false for mapper 24 (VRC6a)

    // IRQ State
    irq_latch: u8,
    irq_counter: Cell<u8>,
    irq_enabled: bool,
    irq_mode: bool, // 0 = scanline, 1 = cycle
    irq_reload: bool,
    irq_pending: Cell<bool>,
    prescaler: Cell<u16>,

    sram_dirty: bool,
}

#[derive(Clone, Serialize, Deserialize)]
struct MapperVRC6StateVariables {
    prg_bank_16k: usize,
    prg_bank_8k: usize,
    chr_regs: [u8; 8],
    b003: u8,
    irq_latch: u8,
    irq_counter: u8,
    irq_enabled: bool,
    irq_mode: bool,
    irq_reload: bool,
    irq_pending: bool,
    prescaler: u16,
    prg_ram: Vec<u8>,
}

impl MapperVRC6 {
    pub fn new(
        _prg_banks: usize,
        _chr_banks: usize,
        prg_rom: Vec<u8>,
        chr_rom: Vec<u8>,
        initial_mirroring: Mirroring,
        four_screen_bit: bool,
        is_mapper_26: bool,
    ) -> Self {
        let chr_ram = if chr_rom.is_empty() { vec![0; 8192] } else { vec![] };
        
        Self {
            prg_rom,
            prg_ram: vec![0; 8192], // Standard 8K PRG RAM for VRC6 games that support it
            chr_rom,
            chr_ram,
            prg_bank_16k: 0,
            prg_bank_8k: 0,
            chr_regs: [0; 8],
            b003: 0,
            initial_mirroring,
            has_four_screen: four_screen_bit,
            is_mapper_26,
            irq_latch: 0,
            irq_counter: Cell::new(0),
            irq_enabled: false,
            irq_mode: false,
            irq_reload: false,
            irq_pending: Cell::new(false),
            prescaler: Cell::new(341),
            sram_dirty: false,
        }
    }

    /// VRC6b (Mapper 26) swaps address lines A0 and A1 for register access compared to VRC6a (Mapper 24).
    fn map_reg_addr(&self, addr: u16) -> u16 {
        if self.is_mapper_26 {
            (addr & 0xFFFC) | ((addr & 1) << 1) | ((addr >> 1) & 1)
        } else {
            addr
        }
    }

    fn resolve_prg(&self, addr: u16) -> (bool, usize, usize) {
        let offset = (addr & 0x1FFF) as usize;
        match addr {
            0x8000..=0x9FFF => {
                // 16KB bank split into two 8KB windows
                let sub_bank = (addr & 0x2000) != 0;
                let bank = (self.prg_bank_16k * 2) + if sub_bank { 1 } else { 0 };
                (false, bank, offset)
            }
            0xA000..=0xBFFF => {
                let sub_bank = (addr & 0x2000) != 0;
                let bank = (self.prg_bank_16k * 2) + if sub_bank { 1 } else { 0 };
                (false, bank, offset)
            }
            0xC000..=0xDFFF => {
                (false, self.prg_bank_8k, offset)
            }
            0xE000..=0xFFFF => {
                // Fixed to last 8KB of PRG ROM
                let total_8k_banks = self.prg_rom.len() / 0x2000;
                let bank = if total_8k_banks > 0 { total_8k_banks - 1 } else { 0 };
                (false, bank, offset)
            }
            0x6000..=0x7FFF => {
                (true, 0, (addr & 0x1FFF) as usize)
            }
            _ => (false, 0, 0),
        }
    }

    fn read_chr_bank(&self, addr: u16) -> u8 {
        let bank_idx = ((addr & 0x1FFF) / 0x0400) as usize;
        let reg_val = self.chr_regs[bank_idx] as usize;
        let chr_offset = (addr & 0x03FF) as usize;

        if !self.chr_rom.is_empty() {
            let final_addr = (reg_val * 0x400) + chr_offset;
            self.chr_rom[final_addr % self.chr_rom.len()]
        } else if !self.chr_ram.is_empty() {
            let final_addr = (reg_val * 0x400) + chr_offset;
            self.chr_ram[final_addr % self.chr_ram.len()]
        } else {
            0
        }
    }

    fn clock_irq(&self) {
        if !self.irq_enabled {
            return;
        }
        if self.irq_counter.get() == 0xFF {
            self.irq_counter.set(self.irq_latch);
            self.irq_pending.set(true);
        } else {
            self.irq_counter.set(self.irq_counter.get().wrapping_add(1));
        }
    }
}

impl Mapper for MapperVRC6 {
    fn step_cycles(&mut self, cycles: u64) {
        if !self.irq_enabled {
            return;
        }
        for _ in 0..cycles {
            if self.irq_mode {
                // Cycle mode: clock every CPU cycle
                self.clock_irq();
            } else {
                // Scanline mode: VRC prescaler counts down by 1 each CPU cycle, 
                // and reloads by +341 when it hits/drops below 0.
                let mut prescaler = self.prescaler.get() as i32 - 1;
                if prescaler <= 0 {
                    prescaler += 341;
                    self.clock_irq();
                }
                self.prescaler.set(prescaler as u16);
            }
        }
    }

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

    fn is_irq_asserted(&self) -> bool {
        self.irq_pending.get()
    }

    fn cpu_read(&self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => {
                let offset = (addr & 0x1FFF) as usize;
                self.prg_ram[offset % self.prg_ram.len()]
            }
            0x8000..=0xFFFF => {
                let (_, bank, offset) = self.resolve_prg(addr);
                let len = self.prg_rom.len();
                if len == 0 { return 0; }
                self.prg_rom[(bank * 0x2000 + offset) % len]
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, value: u8) {
        let reg_addr = self.map_reg_addr(addr);
        match reg_addr {
            0x6000..=0x7FFF => {
                let offset = (addr & 0x1FFF) as usize;
                let len = self.prg_ram.len();
                self.prg_ram[offset % len] = value;
                self.sram_dirty = true;
            }
            // 16k PRG Select ($8000-$8003)
            0x8000..=0x8003 => {
                self.prg_bank_16k = (value & 0x7F) as usize;
            }
            // 8k PRG Select ($C000-$C003)
            0xC000..=0xC003 => {
                self.prg_bank_8k = (value & 0x7F) as usize;
            }
            // PPU Banking Style & Mirroring ($B003)
            0xB003 => {
                self.b003 = value;
            }
            // CHR Select 0..3 ($D000-$D003)
            0xD000..=0xD003 => {
                let idx = (reg_addr & 3) as usize;
                self.chr_regs[idx] = value;
            }
            // CHR Select 4..7 ($E000-$E003)
            0xE000..=0xE003 => {
                let idx = 4 + (reg_addr & 3) as usize;
                self.chr_regs[idx] = value;
            }
            // IRQ Latch ($F000)
            0xF000 => {
                self.irq_latch = value;
            }
            // IRQ Control ($F001)
            0xF001 => {
                self.irq_enabled = (value & 0x02) != 0;
                self.irq_mode = (value & 0x04) != 0;
                self.irq_pending.set(false);
                self.prescaler.set(341); // Reset prescaler on control write
                if self.irq_enabled {
                    self.irq_counter.set(self.irq_latch);
                }
            }
            // IRQ Acknowledge ($F002)
            0xF002 => {
                self.irq_pending.set(false);
            }
            // Audio registers ($9000..=$9003, $A000..=$A002, $B000..=$B002) can be handled here or stubbed out
            0x9000..=0x9003 | 0xA000..=0xA002 | 0xB000..=0xB002 => {
                // Stubbed for APU audio integration
            }
            _ => {}
        }
    }

    fn ppu_read(&self, addr: u16) -> u8 {
        if addr < 0x2000 {
            self.read_chr_bank(addr)
        } else {
            0
        }
    }

    fn ppu_write(&mut self, addr: u16, value: u8) {
        if addr < 0x2000 && self.chr_rom.is_empty() && !self.chr_ram.is_empty() {
            let len = self.chr_ram.len();
            self.chr_ram[(addr as usize) % len] = value;
        }
    }

    fn notify_scanline(&mut self) {
        if self.irq_enabled && !self.irq_mode {
            self.clock_irq();
        }
    }

    fn mirror_vram_address(&self, addr: u16) -> usize {
        let normalized = (addr & 0x0FFF) as usize;
        if self.has_four_screen {
            return normalized;
        }
        let mirroring_bits = (self.b003 >> 2) & 0x03;
        match mirroring_bits {
            0 => { // Vertical / 1-screen A
                if normalized >= 0x0800 { normalized - 0x0800 } else { normalized }
            }
            1 => { // Horizontal / 1-screen B
                if (normalized >= 0x0400 && normalized < 0x0800) || (normalized >= 0x0C00) {
                    normalized - 0x0400
                } else {
                    normalized
                }
            }
            2 => normalized % 0x0400, // 1-screen lower bank
            3 => 0x0400 + (normalized % 0x0400), // 1-screen upper bank
            _ => normalized,
        }
    }

    fn save_state(&self) -> Vec<u8> {
        let variables = MapperVRC6StateVariables {
            prg_bank_16k: self.prg_bank_16k,
            prg_bank_8k: self.prg_bank_8k,
            chr_regs: self.chr_regs,
            b003: self.b003,
            irq_latch: self.irq_latch,
            irq_counter: self.irq_counter.get(),
            irq_enabled: self.irq_enabled,
            irq_mode: self.irq_mode,
            irq_reload: self.irq_reload,
            irq_pending: self.irq_pending.get(),
            prescaler: self.prescaler.get(),
            prg_ram: self.prg_ram.clone(),
        };
        let config = bincode::config::standard().with_fixed_int_encoding();
        bincode::serde::encode_to_vec(&variables, config).unwrap_or_default()
    }

    fn load_state(&mut self, state_bytes: &[u8]) {
        let config = bincode::config::standard().with_fixed_int_encoding();
        if let Ok((state, _)) = bincode::serde::decode_from_slice::<MapperVRC6StateVariables, _>(state_bytes, config) {
            self.prg_bank_16k = state.prg_bank_16k;
            self.prg_bank_8k = state.prg_bank_8k;
            self.chr_regs = state.chr_regs;
            self.b003 = state.b003;
            self.irq_latch = state.irq_latch;
            self.irq_counter.set(state.irq_counter);
            self.irq_enabled = state.irq_enabled;
            self.irq_mode = state.irq_mode;
            self.irq_reload = state.irq_reload;
            self.irq_pending.set(state.irq_pending);
            self.prescaler.set(state.prescaler);
            if state.prg_ram.len() == self.prg_ram.len() {
                self.prg_ram = state.prg_ram;
            }
        }
    }
}