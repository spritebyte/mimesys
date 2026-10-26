use crate::nes::mappers::{Mapper, Mirroring};
use serde::{Serialize, Deserialize};
use std::cell::Cell;
// Mapper 5 (MMC5)
// Used in games like Castlevania 3 and Romance of the 3 Kingdoms 2.

pub struct Mapper5 {
    prg_rom: Vec<u8>,
    prg_ram: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_ram: Vec<u8>,
    ex_ram: [u8; 1024],

    prg_bank_count: usize,

    prg_mode: u8,
    chr_mode: u8,

    ram_protect_1: u8,
    ram_protect_2: u8,

    exram_mode: u8,

    nt_map: [u8; 4],
    fill_tile: u8,
    fill_attr: u8,

    prg_regs: [u8; 5],

    chr_regs_a: [u16; 8],
    chr_regs_b: [u16; 4],
    chr_upper_bits: u16,

    split_ctrl: u8,    // $5200
    split_scroll: u8,  // $5201
    split_chr_page: u8,// $5202

    latched_exram_byte: Cell<u8>,

    irq_target: u8,
    irq_enabled: bool,
    irq_pending: Cell<bool>,
    in_frame: Cell<bool>,
    irq_counter: Cell<u16>,

    mult_a: u8,
    mult_b: u8,

    has_four_screen: bool,
    current_cycle: i64,
    sram_dirty: bool,
    /// PPUCTRL bit 5 (seen through notify_ppu_write): 8x16 sprites use set A for sprites
    /// and set B for the background; with 8x8 sprites the last-written set is used for both
    sprite_8x16: bool,
    last_set_b: bool,
}

#[derive(Clone, Serialize, Deserialize)]
struct Mapper5StateVariables {
    prg_mode: u8,
    chr_mode: u8,
    ram_protect_1: u8,
    ram_protect_2: u8,
    exram_mode: u8,
    nt_map: [u8; 4],
    fill_tile: u8,
    fill_attr: u8,
    prg_regs: [u8; 5],
    chr_regs_a: [u16; 8],
    chr_regs_b: [u16; 4],
    chr_upper_bits: u16,
    latched_exram_byte: u8,
    irq_target: u8,
    irq_enabled: bool,
    irq_pending: bool,
    in_frame: bool,
    irq_counter: u16,
    mult_a: u8,
    mult_b: u8,
    current_cycle: i64,
    prg_ram: Vec<u8>,
    #[serde(with = "serde_bytes")]
    ex_ram: [u8; 1024],
    split_ctrl: u8,
    split_scroll: u8,
    split_chr_page: u8,
}

impl Mapper5 {
    pub fn new(
        _prg_banks: usize,
        _chr_banks: usize,
        prg_rom: Vec<u8>,
        chr_rom: Vec<u8>,
        _initial_mirroring: Mirroring,
        four_screen_bit: bool,
    ) -> Self {
        let chr_ram = if chr_rom.is_empty() { vec![0; 8192] } else { vec![] };
        let prg_bank_count = prg_rom.len() / 0x2000;

        let mut chr_regs_a = [0u16; 8];
        for (i, reg) in chr_regs_a.iter_mut().enumerate() {
            *reg = i as u16;
        }

        Self {
            prg_rom,
            prg_ram: vec![0; 0x10000],
            chr_rom,
            chr_ram,
            ex_ram: [0; 1024],
            prg_bank_count,
            prg_mode: 3,
            chr_mode: 3,
            ram_protect_1: 0,
            ram_protect_2: 0,
            exram_mode: 0,
            nt_map: [0, 1, 0, 1],
            fill_tile: 0,
            fill_attr: 0,
            prg_regs: [0, 0xFF, 0xFF, 0xFF, 0xFF],
            chr_regs_a,
            chr_regs_b: [0; 4],
            chr_upper_bits: 0,
            latched_exram_byte: Cell::new(0),
            split_ctrl: 0, split_scroll: 0, split_chr_page: 0,
            irq_target: 0,
            irq_enabled: false,
            irq_pending: Cell::new(false),
            in_frame: Cell::new(false),
            irq_counter: Cell::new(0),
            mult_a: 0,
            mult_b: 0,
            has_four_screen: four_screen_bit,
            current_cycle: 0,
            sram_dirty: false,
            sprite_8x16: false,
            last_set_b: false,
        }
    }

    fn prg_ram_writable(&self) -> bool {
        self.ram_protect_1 == 0b10 && self.ram_protect_2 == 0b01
    }

    /// (is_ram, 8K bank, offset). $6000 is always RAM ($5113). In the bigger modes the
    /// whole window follows ONE register, including its ROM/RAM bit:
    ///   mode 0: $8000-$FFFF 32K from $5117 (ROM)
    ///   mode 1: $8000-$BFFF 16K from $5115, $C000-$FFFF 16K from $5117 (ROM)
    ///   mode 2: $8000-$BFFF 16K from $5115, $C000 8K $5116, $E000 8K $5117 (ROM)
    ///   mode 3: 8K each from $5114-$5117 ($E000 always ROM)
    fn resolve_prg(&self, addr: u16) -> (bool, usize, usize) {
        let offset = (addr & 0x1FFF) as usize;
        if addr < 0x8000 {
            return (true, (self.prg_regs[0] & 0x07) as usize, offset);
        }
        let slot = ((addr - 0x8000) / 0x2000) as usize;          // 0-3 for $8000/$A000/$C000/$E000
        let (reg, size_8k, rom_only) = match (self.prg_mode, slot) {
            (0, _) => (4, 4, true),
            (1, 0 | 1) => (2, 2, false),
            (1, _) => (4, 2, true),
            (2, 0 | 1) => (2, 2, false),
            (2, 2) => (3, 1, false),
            (2, _) => (4, 1, true),
            (_, 3) => (4, 1, true),
            (_, s) => (s + 1, 1, false),
        };
        let v = self.prg_regs[reg];
        let is_ram = !rom_only && v & 0x80 == 0;
        let first = slot - slot % size_8k;                       // first 8K slot of this window
        let bank = (v & 0x7F) as usize & !(size_8k - 1);
        let bank = bank + (slot - first);
        if is_ram { (true, bank & 0x07, offset) } else { (false, bank, offset) }
    }

    fn read_prg(&self, addr: u16) -> u8 {
        let (is_ram, bank, offset) = self.resolve_prg(addr);
        if is_ram {
            let len = self.prg_ram.len();
            self.prg_ram[(bank * 0x2000 + offset) % len]
        } else {
            let len = self.prg_rom.len();
            if len == 0 { return 0; }
            self.prg_rom[(bank * 0x2000 + offset) % len]
        }
    }

    /// CHR bank lookup honouring the bank size ($5101: 0 = 8K, 1 = 4K, 2 = 2K, 3 = 1K).
    /// Set A ($5120-$5127) maps the whole 8K with the last register of each window;
    /// set B ($5128-$512B) is 4K wide and repeats for $1000-$1FFF.
    fn read_chr(&self, addr: u16, is_bg_fetch: bool) -> u8 {
        let a = (addr & 0x1FFF) as usize;
        let use_b = if self.sprite_8x16 { is_bg_fetch } else { self.last_set_b };
        let (size, reg) = match self.chr_mode {
            0 => (0x2000, if use_b { self.chr_regs_b[3] } else { self.chr_regs_a[7] }),
            1 => (0x1000, if use_b { self.chr_regs_b[3] } else { self.chr_regs_a[3 + 4 * (a / 0x1000)] }),
            2 => (0x0800, if use_b { self.chr_regs_b[1 + 2 * ((a / 0x800) & 1)] }
                          else { self.chr_regs_a[1 + 2 * (a / 0x800)] }),
            _ => (0x0400, if use_b { self.chr_regs_b[(a / 0x400) & 3] } else { self.chr_regs_a[a / 0x400] }),
        };
        let bank = (reg | (self.chr_upper_bits << 8)) as usize;
        self.chr_fetch(addr, bank * size + (a & (size - 1)), is_bg_fetch)
    }

    fn chr_fetch(&self, raw_addr: u16, addr: usize, is_bg_fetch: bool) -> u8 {
        let mut final_addr = addr;

        if self.exram_mode == 1 && self.in_frame.get() && is_bg_fetch {
            let latched = self.latched_exram_byte.get();
            let bank = (latched & 0x3F) as usize | ((self.chr_upper_bits as usize) << 6);
            final_addr = (bank * 0x1000) + (raw_addr & 0x0FFF) as usize;
        }

        if !self.chr_rom.is_empty() {
            self.chr_rom[final_addr % self.chr_rom.len()]
        } else if !self.chr_ram.is_empty() {
            self.chr_ram[final_addr & 0x1FFF]
        } else {
            0
        }
    }

    fn nametable_byte(&self, addr: u16, ppu_vram: &[u8; 4096], is_attribute_byte: bool) -> u8 {
        let nt_index = ((addr - 0x2000) / 0x0400) as usize % 4;
        let mode = self.nt_map[nt_index];
        let offset = (addr & 0x03FF) as usize;

        if self.exram_mode == 1 && is_attribute_byte {
            let latched = self.latched_exram_byte.get();
            let pal = (latched >> 6) & 0x03;
            return pal | (pal << 2) | (pal << 4) | (pal << 6);
        }

        match mode {
            0 => ppu_vram[offset & 0x0FFF],
            1 => ppu_vram[(0x0400 + offset) & 0x0FFF],
            2 => if self.exram_mode <= 1 { self.ex_ram[offset & 0x03FF] } else { 0 },
            3 => {
                if offset < 0x03C0 { self.fill_tile } else { self.fill_attr }
            }
            _ => unreachable!(),
        }
    }

    /// ExRAM mode 1 (extended attributes): every nametable tile fetch also reads ExRAM at
    /// the same offset; that byte gives this tile's 4K CHR bank (bits 0-5) and palette
    /// (bits 6-7). Independent of where the nametable itself is mapped.
    fn maybe_latch_exram(&self, addr: u16) {
        if self.exram_mode == 1 {
            let offset = (addr & 0x03FF) as usize;
            if offset < 0x03C0 {
                self.latched_exram_byte.set(self.ex_ram[offset]);
            }
        }
    }

    fn clock_irq_counter(&mut self) {
        if !self.in_frame.get() {
            return;
        }
        let next = self.irq_counter.get().saturating_add(1);
        self.irq_counter.set(next);
        if next == self.irq_target as u16 && self.irq_enabled {
            self.irq_pending.set(true);
        }
    }
}

impl Mapper for Mapper5 {
    fn step_cycles(&mut self, cycles: u64) {
        self.current_cycle += cycles as i64;
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

    fn cpu_read_bus(&self, addr: u16, open_bus: u8) -> u8 {
        match addr {
            0x5204..=0x5206 | 0x5C00..=0x5FFF | 0x6000..=0xFFFF => self.cpu_read(addr),
            _ => open_bus,
        }
    }

    fn cpu_read(&self, addr: u16) -> u8 {
        match addr {
            0x5204 => {
                let mut res = 0u8;
                if self.irq_pending.get() { res |= 0x80; }
                if self.in_frame.get() { res |= 0x40; }
                self.irq_pending.set(false);
                res
            }
            0x5205 => ((self.mult_a as u16 * self.mult_b as u16) & 0xFF) as u8,
            0x5206 => ((self.mult_a as u16 * self.mult_b as u16) >> 8) as u8,
            // ExRAM reads only work in modes 2/3 (as RAM)
            0x5C00..=0x5FFF if self.exram_mode >= 2 => self.ex_ram[(addr - 0x5C00) as usize],
            0x6000..=0xFFFF => self.read_prg(addr),
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, value: u8) {
        match addr {
            0x5100 => self.prg_mode = value & 0x03,
            0x5101 => self.chr_mode = value & 0x03,
            0x5102 => self.ram_protect_1 = value & 0x03,
            0x5103 => self.ram_protect_2 = value & 0x03,
            0x5104 => {
                self.exram_mode = value & 0x03;
            }
            0x5105 => {
                self.nt_map[0] = value & 0x03;
                self.nt_map[1] = (value >> 2) & 0x03;
                self.nt_map[2] = (value >> 4) & 0x03;
                self.nt_map[3] = (value >> 6) & 0x03;
            }
            0x5106 => self.fill_tile = value,
            0x5107 => {
                let p = value & 0x03;
                self.fill_attr = p | (p << 2) | (p << 4) | (p << 6);
            }
            0x5113..=0x5117 => {
                self.prg_regs[(addr - 0x5113) as usize] = value;
            }
            0x5120..=0x5127 => {
                self.chr_regs_a[(addr - 0x5120) as usize] = value as u16;
                self.last_set_b = false;
            }
            0x5128..=0x512B => {
                self.chr_regs_b[(addr - 0x5128) as usize] = value as u16;
                self.last_set_b = true;
            }
            0x5130 => self.chr_upper_bits = (value & 0x03) as u16,
            0x5203 => self.irq_target = value,
            0x5204 => self.irq_enabled = (value & 0x80) != 0,
            0x5205 => self.mult_a = value,
            0x5206 => self.mult_b = value,
            // mode 3 = read-only
            0x5C00..=0x5FFF if self.exram_mode != 3 => {
                self.ex_ram[(addr - 0x5C00) as usize] = value;
            }
            0x6000..=0xFFFF => {
                let (is_ram, bank, offset) = self.resolve_prg(addr);
                if addr < 0x8000 {
                    let len = self.prg_ram.len();
                    self.prg_ram[(bank * 0x2000 + offset) % len] = value;
                    self.sram_dirty = true;
                } else if is_ram && self.prg_ram_writable() {
                    let len = self.prg_ram.len();
                    self.prg_ram[(bank * 0x2000 + offset) % len] = value;
                    self.sram_dirty = true;
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

    fn notify_ppu_write(&mut self, reg: u16, value: u8) {
        if reg == 0 {
            self.sprite_8x16 = value & 0x20 != 0;
        }
    }

    fn ppu_read_ctx(&self, addr: u16, is_bg_fetch: bool) -> u8 {
        if addr < 0x2000 {
            self.read_chr(addr, is_bg_fetch)
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

    fn read_nametable_byte(&self, addr: u16, ppu_vram: &[u8; 4096], is_attribute_byte: bool) -> u8 {
        if !is_attribute_byte {
            self.maybe_latch_exram(addr);
        }
        self.nametable_byte(addr, ppu_vram, is_attribute_byte)
    }

    fn write_nametable_byte(&mut self, addr: u16, value: u8, vram: &mut [u8; 4096]) {
        let nt_index = ((addr - 0x2000) / 0x0400) as usize % 4;
        let mode = self.nt_map[nt_index];
        let offset = (addr & 0x03FF) as usize;

        match mode {
            0 => vram[offset & 0x0FFF] = value,
            1 => vram[(0x0400 + offset) & 0x0FFF] = value,
            2 => self.ex_ram[offset & 0x03FF] = value,
            3 => { /* fill-mode nametable: real hardware ignores writes here */ }
            _ => unreachable!(),
        }
    }

    fn mirror_vram_address(&self, addr: u16) -> usize {
        let normalized = (addr & 0x0FFF) as usize;
        if self.has_four_screen {
            return normalized;
        }
        let nt_index = (normalized / 0x0400) % 4;
        match self.nt_map[nt_index] {
            1 => 0x0400 + (normalized % 0x0400),
            _ => normalized % 0x0400,
        }
    }

    fn notify_scanline(&mut self) {
       self.clock_irq_counter();
    }

    fn notify_frame_start(&mut self) {
        self.in_frame.set(true);
        self.irq_counter.set(0);
    }

    fn split_config(&self) -> Option<(bool, u8)> {
        // returns (side: false=left/true=right, tile_count) if split is enabled
        if (self.split_ctrl & 0x80) != 0 {
            Some(((self.split_ctrl & 0x40) != 0, self.split_ctrl & 0x1F))
        } else {
            None
        }
    }

    fn read_split_tile(&self, screen_x: usize, scanline: usize) -> (u8, u8, u8) {
        // returns (bg_low, bg_high, palette_idx) for one pixel column of the split region
        let effective_row = (scanline + self.split_scroll as usize) & 0xFF;
        let tile_row = effective_row / 8;
        let fine_y = effective_row % 8;
        let tile_col = screen_x / 8;

        let tile_id = self.ex_ram[tile_row * 32 + tile_col];
        let attr_byte = self.ex_ram[0x3C0 + (tile_row / 4) * 8 + (tile_col / 4)];
        let shift = (((tile_row >> 1) & 1) << 2) | (((tile_col >> 1) & 1) << 1);
        let palette_idx = (attr_byte >> shift) & 0x03;

        let pattern_addr = (self.split_chr_page as usize * 0x1000) + (tile_id as usize * 16) + fine_y;
        let bg_low = self.chr_fetch(pattern_addr as u16, pattern_addr, false);
        let bg_high = self.chr_fetch(pattern_addr as u16, pattern_addr + 8, false);

        (bg_low, bg_high, palette_idx)
    }
    fn save_state(&self) -> Vec<u8> {
        let variables = Mapper5StateVariables {
            prg_mode: self.prg_mode,
            chr_mode: self.chr_mode,
            ram_protect_1: self.ram_protect_1,
            ram_protect_2: self.ram_protect_2,
            exram_mode: self.exram_mode,
            nt_map: self.nt_map,
            fill_tile: self.fill_tile,
            fill_attr: self.fill_attr,
            prg_regs: self.prg_regs,
            chr_regs_a: self.chr_regs_a,
            chr_regs_b: self.chr_regs_b,
            chr_upper_bits: self.chr_upper_bits,
            latched_exram_byte: self.latched_exram_byte.get(),
            irq_target: self.irq_target,
            irq_enabled: self.irq_enabled,
            irq_pending: self.irq_pending.get(),
            in_frame: self.in_frame.get(),
            irq_counter: self.irq_counter.get(),
            mult_a: self.mult_a,
            mult_b: self.mult_b,
            current_cycle: self.current_cycle,
            prg_ram: self.prg_ram.clone(),
            ex_ram: self.ex_ram,
            split_ctrl: self.split_ctrl,
            split_scroll: self.split_scroll,
            split_chr_page: self.split_chr_page,
        };
        let config = bincode::config::standard().with_fixed_int_encoding();
        bincode::serde::encode_to_vec(&variables, config).unwrap_or_default()
    }

    fn load_state(&mut self, state_bytes: &[u8]) {
        let config = bincode::config::standard().with_fixed_int_encoding();
        if let Ok((state, _bytes_read)) = bincode::serde::decode_from_slice::<Mapper5StateVariables, _>(state_bytes, config) {
            self.prg_mode = state.prg_mode;
            self.chr_mode = state.chr_mode;
            self.ram_protect_1 = state.ram_protect_1;
            self.ram_protect_2 = state.ram_protect_2;
            self.exram_mode = state.exram_mode;
            self.nt_map = state.nt_map;
            self.fill_tile = state.fill_tile;
            self.fill_attr = state.fill_attr;
            self.prg_regs = state.prg_regs;
            self.chr_regs_a = state.chr_regs_a;
            self.chr_regs_b = state.chr_regs_b;
            self.chr_upper_bits = state.chr_upper_bits;
            self.latched_exram_byte.set(state.latched_exram_byte);
            self.irq_target = state.irq_target;
            self.irq_enabled = state.irq_enabled;
            self.irq_pending.set(state.irq_pending);
            self.in_frame.set(state.in_frame);
            self.irq_counter.set(state.irq_counter);
            self.mult_a = state.mult_a;
            self.mult_b = state.mult_b;
            self.current_cycle = state.current_cycle;
            if state.prg_ram.len() == self.prg_ram.len() {
                self.prg_ram = state.prg_ram;
            }
            self.ex_ram = state.ex_ram;
            self.split_ctrl = state.split_ctrl;
            self.split_scroll = state.split_scroll;
            self.split_chr_page = state.split_chr_page;
        }
    }
}
