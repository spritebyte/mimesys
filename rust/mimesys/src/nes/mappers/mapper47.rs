
use crate::nes::mappers::{
    decode_state, encode_state, mirror_nt, Mapper, Mirroring,
};
use serde::{Deserialize, Serialize};

// iNES Mapper 47: NES-QJ
// Super Spike V'Ball / Nintendo World Cup multicart.
//
// MMC3-compatible banking with an outer bank bit selecting one of
// two 128 KiB PRG-ROM and 128 KiB CHR-ROM blocks.

pub struct Mapper47 {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_ram: Vec<u8>,
    s: State,
}

#[derive(Serialize, Deserialize, Clone)]
struct State {
    command: u8,
    chr_regs: [u8; 8],
    prg_regs: [u8; 2],
    prg_mode: bool,
    chr_mode: bool,
    mirroring: u8,

    // MMC3 PRG-RAM control. On Mapper 47, writes to $6000-$7FFF
    // select the outer bank when RAM is enabled and not write-protected.
    prg_ram_enabled: bool,
    prg_ram_write_protected: bool,
    outer_bank: u8,

    // MMC3 scanline IRQ state.
    irq_latch: u8,
    irq_counter: u8,
    irq_reload: bool,
    irq_enabled: bool,
    irq_pending: bool,

    // A12 low-to-high edge filtering.
    a12_high: bool,
    last_a12_low_cycle: u64,
}

impl Mapper47 {
    pub fn new(
        _mapper: u16,
        _submapper: u8,
        prg_rom: Vec<u8>,
        chr_rom: Vec<u8>,
        mirroring: Mirroring,
    ) -> Self {
        let chr_ram = if chr_rom.is_empty() {
            vec![0; 8192]
        } else {
            Vec::new()
        };

        Self {
            prg_rom,
            chr_rom,
            chr_ram,
            s: State {
                command: 0,
                chr_regs: [0; 8],
                prg_regs: [0, 1],
                prg_mode: false,
                chr_mode: false,

                mirroring: match mirroring {
                    Mirroring::Vertical => 0,
                    Mirroring::Horizontal => 1,
                    Mirroring::SingleLower => 2,
                    Mirroring::SingleUpper => 3,
                    Mirroring::FourScreen => 4,
                },

                prg_ram_enabled: false,
                prg_ram_write_protected: false,
                outer_bank: 0,

                irq_latch: 0,
                irq_counter: 0,
                irq_reload: false,
                irq_enabled: false,
                irq_pending: false,

                a12_high: false,
                last_a12_low_cycle: 0,
            },
        }
    }

    fn prg_bank_count(&self) -> usize {
        self.prg_rom.len() / 0x2000
    }

    fn chr_memory_len(&self) -> usize {
        if self.chr_rom.is_empty() {
            self.chr_ram.len()
        } else {
            self.chr_rom.len()
        }
    }

    // Return the 1 KiB CHR bank mapped to a PPU slot.
    //
    // CHR mode 0:
    //   R0, R0+1, R1, R1+1, R2, R3, R4, R5
    //
    // CHR mode 1:
    //   R2, R3, R4, R5, R0, R0+1, R1, R1+1
    fn chr_bank_for_slot(&self, slot: usize) -> usize {
        match (self.s.chr_mode, slot) {
            (false, 0..=1) => {
                (self.s.chr_regs[0] as usize & 0xFE) + slot
            }
            (false, 2..=3) => {
                (self.s.chr_regs[1] as usize & 0xFE) + (slot - 2)
            }
            (false, 4..=7) => {
                self.s.chr_regs[slot - 2] as usize
            }

            (true, 0..=3) => {
                self.s.chr_regs[slot + 2] as usize
            }
            (true, 4..=5) => {
                (self.s.chr_regs[0] as usize & 0xFE) + (slot - 4)
            }
            (true, 6..=7) => {
                (self.s.chr_regs[1] as usize & 0xFE) + (slot - 6)
            }

            _ => 0,
        }
    }

    fn mapped_chr_index(&self, addr: usize) -> Option<usize> {
        let memory_len = self.chr_memory_len();
        if memory_len == 0 {
            return None;
        }

        let slot = (addr & 0x1FFF) / 0x400;
        let offset = addr & 0x3FF;

        // The outer bank selects a 128 KiB CHR block:
        // 128 x 1 KiB banks per block.
        let outer = ((self.s.outer_bank & 1) as usize) << 7;
        let inner = self.chr_bank_for_slot(slot) & 0x7F;
        let bank = outer | inner;

        Some((bank * 0x400 + offset) % memory_len)
    }

    fn mapped_prg_bank(&self, slot: usize) -> usize {
        // Two 128 KiB blocks, each containing 16 x 8 KiB banks.
        let outer = ((self.s.outer_bank & 1) as usize) << 4;

        let r6 = self.s.prg_regs[0] as usize & 0x0F;
        let r7 = self.s.prg_regs[1] as usize & 0x0F;

        let inner = match (self.s.prg_mode, slot) {
            (false, 0) => r6,
            (false, 1) => r7,
            (false, 2) => 14,
            (false, 3) => 15,

            (true, 0) => 14,
            (true, 1) => r6,
            (true, 2) => r7,
            (true, 3) => 15,

            _ => 15,
        };

        outer | inner
    }

    fn write_mmc3_register(&mut self, addr: u16, value: u8) {
        match addr & 0xE001 {
            0x8000 => {
                self.s.command = value;
                self.s.prg_mode = (value & 0x40) != 0;
                self.s.chr_mode = (value & 0x80) != 0;
            }

            0x8001 => {
                let reg = (self.s.command & 0x07) as usize;
                match reg {
                    0..=5 => self.s.chr_regs[reg] = value,
                    6 => self.s.prg_regs[0] = value,
                    7 => self.s.prg_regs[1] = value,
                    _ => {}
                }
            }

            0xA000 => {
                // Bit 0: 0 = vertical, 1 = horizontal.
                self.s.mirroring = value & 1;
            }

            0xA001 => {
                self.s.prg_ram_enabled = (value & 0x80) != 0;
                self.s.prg_ram_write_protected = (value & 0x40) != 0;
            }

            0xC000 => {
                self.s.irq_latch = value;
            }

            0xC001 => {
                self.s.irq_reload = true;
            }

            0xE000 => {
                self.s.irq_enabled = false;
                self.s.irq_pending = false;
            }

            0xE001 => {
                self.s.irq_enabled = true;
            }

            _ => {}
        }
    }
}

impl Mapper for Mapper47 {
    fn cpu_read(&self, addr: u16) -> u8 {
        match addr {
            // Mapper 47 uses this range for its outer-bank register,
            // not ordinary PRG RAM.
            0x6000..=0x7FFF => 0,

            0x8000..=0xFFFF => {
                let count = self.prg_bank_count();
                if count == 0 {
                    return 0;
                }

                let slot = (addr as usize - 0x8000) / 0x2000;
                let bank = self.mapped_prg_bank(slot) % count;
                let offset = addr as usize & 0x1FFF;
                let index = bank * 0x2000 + offset;

                self.prg_rom.get(index).copied().unwrap_or(0)
            }

            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, value: u8) {
        match addr {
            0x6000..=0x7FFF => {
                // Mapper 47 outer block register.
                // Writes are gated by MMC3 PRG-RAM control.
                if self.s.prg_ram_enabled
                    && !self.s.prg_ram_write_protected
                {
                    self.s.outer_bank = value & 1;
                }
            }

            0x8000..=0xFFFF => {
                self.write_mmc3_register(addr, value);
            }

            _ => {}
        }
    }

    fn ppu_read(&self, addr: u16) -> u8 {
        let addr = addr as usize & 0x1FFF;

        match self.mapped_chr_index(addr) {
            Some(index) if self.chr_rom.is_empty() => {
                self.chr_ram.get(index).copied().unwrap_or(0)
            }
            Some(index) => {
                self.chr_rom.get(index).copied().unwrap_or(0)
            }
            None => 0,
        }
    }

    fn ppu_write(&mut self, addr: u16, value: u8) {
        if !self.chr_rom.is_empty() {
            return;
        }

        let addr = addr as usize & 0x1FFF;

        if let Some(index) = self.mapped_chr_index(addr) {
            if let Some(byte) = self.chr_ram.get_mut(index) {
                *byte = value;
            }
        }
    }

    fn update_a12(&mut self, addr: u16, ppu_cycle: u64) {
        let high = (addr & 0x1000) != 0;

        if !high {
            if self.s.a12_high {
                self.s.a12_high = false;
                self.s.last_a12_low_cycle = ppu_cycle;
            }
            return;
        }

        // Only clock the IRQ counter on a low-to-high transition,
        // after A12 has remained low for at least 8 PPU cycles.
        if !self.s.a12_high {
            self.s.a12_high = true;

            if ppu_cycle.wrapping_sub(self.s.last_a12_low_cycle) >= 8 {
                if self.s.irq_counter == 0 || self.s.irq_reload {
                    self.s.irq_counter = self.s.irq_latch;
                    self.s.irq_reload = false;
                } else {
                    self.s.irq_counter = self.s.irq_counter.wrapping_sub(1);
                }

                if self.s.irq_counter == 0 && self.s.irq_enabled {
                    self.s.irq_pending = true;
                }
            }
        }
    }

    fn is_irq_asserted(&self) -> bool {
        self.s.irq_pending
    }

    fn mirror_vram_address(&self, addr: u16) -> usize {
        let mirroring = match self.s.mirroring {
            0 => Mirroring::Vertical,
            1 => Mirroring::Horizontal,
            2 => Mirroring::SingleLower,
            3 => Mirroring::SingleUpper,
            4 => Mirroring::FourScreen,
            _ => Mirroring::Horizontal,
        };

        mirror_nt(mirroring, addr)
    }

    // Mapper 47 has no ordinary PRG RAM; $6000-$7FFF is the
    // outer-bank register range.
    fn get_sram(&self) -> Option<&[u8]> {
        None
    }

    fn load_sram(&mut self, _data: &[u8]) {}

    fn is_sram_dirty(&self) -> bool {
        false
    }

    fn clear_sram_dirty(&mut self) {}

    fn save_state(&self) -> Vec<u8> {
        encode_state(&self.s)
    }

    fn load_state(&mut self, data: &[u8]) {
        if let Some(state) = decode_state::<State>(data) {
            if state.outer_bank <= 1 {
                self.s = state;
            }
        }
    }
}
