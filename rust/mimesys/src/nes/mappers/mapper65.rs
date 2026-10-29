
use crate::nes::mappers::{
    decode_state, encode_state, mirror_nt, Mapper, Mirroring,
};
use serde::{Deserialize, Serialize};

// iNES Mapper 65: Irem H3001
//
// Three switchable 8 KiB PRG banks at $8000, $A000, $C000.
// Fixed final 8 KiB PRG bank at $E000.
// Eight switchable 1 KiB CHR banks.
// Mirroring control at $9001.
// 16-bit CPU-cycle IRQ counter.

pub struct Mapper65 {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_ram: Vec<u8>,
    s: State,
}

#[derive(Serialize, Deserialize, Clone)]
struct State {
    prg_regs: [u8; 3],
    chr: [u8; 8],

    irq_latch: u16,
    irq_counter: u16,
    irq_enabled: bool,
    irq_pending: bool,

    mirroring: u8,

    prg_ram: Vec<u8>,
    sram_dirty: bool,
}

impl Mapper65 {
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

        let prg_count = (prg_rom.len() / 0x2000).max(1);

        Self {
            prg_rom,
            chr_rom,
            chr_ram,
            s: State {
                // Power-on layout: banks 0, 1, second-to-last,
                // and the fixed final bank.
                prg_regs: [
                    0,
                    1,
                    ((prg_count.saturating_sub(2)) & 0xFF) as u8,
                ],
                chr: [0, 1, 2, 3, 4, 5, 6, 7],

                irq_latch: 0,
                irq_counter: 0,
                irq_enabled: false,
                irq_pending: false,

                mirroring: match mirroring {
                    Mirroring::Vertical => 0,
                    Mirroring::Horizontal => 1,
                    Mirroring::SingleLower => 2,
                    Mirroring::SingleUpper => 3,
                    Mirroring::FourScreen => 4,
                },

                prg_ram: vec![0; 8192],
                sram_dirty: false,
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

    fn chr_index(&self, addr: usize) -> Option<usize> {
        let len = self.chr_memory_len();
        if len == 0 {
            return None;
        }

        let slot = (addr & 0x1FFF) / 0x400;
        let offset = addr & 0x3FF;
        let bank = self.s.chr[slot] as usize;

        Some((bank * 0x400 + offset) % len)
    }
}

impl Mapper for Mapper65 {
    fn cpu_read(&self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => {
                self.s.prg_ram[(addr - 0x6000) as usize]
            }

            0x8000..=0xFFFF => {
                let count = self.prg_bank_count();
                if count == 0 {
                    return 0;
                }

                let slot = (addr as usize - 0x8000) / 0x2000;

                let bank = match slot {
                    0 => self.s.prg_regs[0] as usize % count,
                    1 => self.s.prg_regs[1] as usize % count,
                    2 => self.s.prg_regs[2] as usize % count,
                    _ => count - 1,
                };

                let index =
                    bank * 0x2000 + (addr as usize & 0x1FFF);

                self.prg_rom.get(index).copied().unwrap_or(0)
            }

            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, value: u8) {
        match addr {
            0x6000..=0x7FFF => {
                self.s.prg_ram[(addr - 0x6000) as usize] = value;
                self.s.sram_dirty = true;
            }

            0x8000..=0x8FFF => {
                // Select the 8 KiB bank at $8000.
                self.s.prg_regs[0] = value;
            }

            0x9000..=0x9FFF => {
                // The low address nibble selects registers.
                // $9000 itself does not select a PRG mode.
                match addr & 0x000F {
                    0x0001 => {
                        // Bit 7: 0 = vertical, 1 = horizontal.
                        self.s.mirroring =
                            if value & 0x80 != 0 { 1 } else { 0 };
                    }

                    0x0003 => {
                        // A write acknowledges a pending IRQ.
                        // Bit 7 controls IRQ counter enable.
                        self.s.irq_enabled = (value & 0x80) != 0;
                        self.s.irq_pending = false;
                    }

                    0x0004 => {
                        // Reload the counter and acknowledge IRQ.
                        self.s.irq_counter = self.s.irq_latch;
                        self.s.irq_pending = false;
                    }

                    0x0005 => {
                        self.s.irq_latch =
                            (self.s.irq_latch & 0x00FF)
                            | ((value as u16) << 8);
                    }

                    0x0006 => {
                        self.s.irq_latch =
                            (self.s.irq_latch & 0xFF00)
                            | value as u16;
                    }

                    _ => {}
                }
            }

            0xA000..=0xAFFF => {
                // Select the 8 KiB bank at $A000.
                self.s.prg_regs[1] = value;
            }

            0xB000..=0xBFFF => {
                // Eight 1 KiB CHR registers.
                // Higher address bits alias the same registers.
                let reg = (addr & 0x0007) as usize;
                self.s.chr[reg] = value;
            }

            0xC000..=0xCFFF => {
                // Select the 8 KiB bank at $C000.
                self.s.prg_regs[2] = value;
            }

            // Writes to $D000-$FFFF do not change banking.
            _ => {}
        }
    }

    fn ppu_read(&self, addr: u16) -> u8 {
        let addr = addr as usize & 0x1FFF;

        match self.chr_index(addr) {
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

        if let Some(index) = self.chr_index(addr) {
            if let Some(byte) = self.chr_ram.get_mut(index) {
                *byte = value;
            }
        }
    }

    fn step_cycles(&mut self, cycles: u64) {
        if !self.s.irq_enabled {
            return;
        }

        for _ in 0..cycles {
            if self.s.irq_counter > 0 {
                self.s.irq_counter -= 1;

                if self.s.irq_counter == 0 {
                    self.s.irq_pending = true;
                    self.s.irq_enabled = false;
                    break;
                }
            } else {
                // A zero counter does not repeatedly generate IRQs.
                self.s.irq_enabled = false;
                break;
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

    fn get_sram(&self) -> Option<&[u8]> {
        Some(&self.s.prg_ram)
    }

    fn load_sram(&mut self, data: &[u8]) {
        let n = data.len().min(self.s.prg_ram.len());
        self.s.prg_ram[..n].copy_from_slice(&data[..n]);
    }

    fn is_sram_dirty(&self) -> bool {
        self.s.sram_dirty
    }

    fn clear_sram_dirty(&mut self) {
        self.s.sram_dirty = false;
    }

    fn save_state(&self) -> Vec<u8> {
        encode_state(&self.s)
    }

    fn load_state(&mut self, data: &[u8]) {
        if let Some(state) = decode_state::<State>(data) {
            self.s = state;
        }
    }
}
