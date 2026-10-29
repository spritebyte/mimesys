use crate::nes::mappers::{decode_state, encode_state, mirror_nt, Mapper, Mirroring};
use serde::{Deserialize, Serialize};

// iNES Mapper 73: Konami VRC3 (Salamander)
// PRG: 16K switchable at $8000-$BFFF, 16K fixed to last bank at $C000-$FFFF, optional 8K PRG RAM at $6000-$7FFF
// CHR: 8K unbanked (uses CHR RAM or CHR ROM)
// IRQ: 16-bit cycle counter with latches at $8000-$BFFF, control at $C000-$CFFF, acknowledge at $D000-$DFFF

pub struct Mapper73 {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_ram: Vec<u8>,
    s: State,
}

#[derive(Serialize, Deserialize, Clone)]
struct State {
    prg_bank: usize,
    
    // IRQ State
    irq_latch: u16,
    irq_counter: u16,
    irq_enabled: bool,
    irq_pending: bool,
    irq_mode: bool,      // true = 8-bit mode, false = 16-bit mode
    ack_control: bool,   // A bit moved to E on $D000 write

    mirroring: u8,
    prg_ram: Vec<u8>,
    sram_dirty: bool,
}

impl Mapper73 {
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
                prg_bank: 0,
                irq_latch: 0,
                irq_counter: 0,
                irq_enabled: false,
                irq_pending: false,
                irq_mode: false,
                ack_control: false,
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
        self.prg_rom.len() / 0x4000 // 16 KiB banks
    }
}

impl Mapper for Mapper73 {
    fn cpu_read(&self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => self.s.prg_ram[(addr - 0x6000) as usize],
            0x8000..=0xFFFF => {
                let prg_count = self.prg_bank_count();
                if prg_count == 0 {
                    return 0;
                }

                let (bank, offset) = if addr < 0xC000 {
                    // Switchable 16K bank at $8000-$BFFF
                    (self.s.prg_bank % prg_count, (addr as usize - 0x8000))
                } else {
                    // Fixed 16K bank to the last bank at $C000-$FFFF
                    (prg_count - 1, (addr as usize - 0xC000))
                };

                let index = (bank * 0x4000) + offset;
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
                self.s.irq_latch = (self.s.irq_latch & 0xFFF0) | (value as u16 & 0x0F);
            }
            0x9000..=0x9FFF => {
                self.s.irq_latch = (self.s.irq_latch & 0xFF0F) | ((value as u16 & 0x0F) << 4);
            }
            0xA000..=0xAFFF => {
                self.s.irq_latch = (self.s.irq_latch & 0xF0FF) | ((value as u16 & 0x0F) << 8);
            }
            0xB000..=0xBFFF => {
                self.s.irq_latch = (self.s.irq_latch & 0x0FFF) | ((value as u16 & 0x0F) << 12);
            }
            0xC000..=0xCFFF => {
                self.s.irq_pending = false;
                self.s.ack_control = (value & 0x01) != 0;
                self.s.irq_enabled = (value & 0x02) != 0;
                self.s.irq_mode = (value & 0x04) != 0;
                if self.s.irq_enabled {
                    self.s.irq_counter = self.s.irq_latch;
                }
            }
            0xD000..=0xDFFF => {
                self.s.irq_pending = false;
                self.s.irq_enabled = self.s.ack_control;
            }
            0xF000..=0xFFFF => {
                self.s.prg_bank = value as usize;
            }
            _ => {}
        }
    }

    fn ppu_read(&self, addr: u16) -> u8 {
        let addr = addr as usize & 0x1FFF;
        if self.chr_rom.is_empty() {
            self.chr_ram.get(addr % self.chr_ram.len()).copied().unwrap_or(0)
        } else {
            self.chr_rom.get(addr % self.chr_rom.len()).copied().unwrap_or(0)
        }
    }

    fn ppu_write(&mut self, addr: u16, value: u8) {
        if !self.chr_rom.is_empty() {
            return;
        }
        let addr = addr as usize & 0x1FFF;
        let target = addr % self.chr_ram.len();
        self.chr_ram[target] = value;
    }

    fn step_cycles(&mut self, cycles: u64) {
        if !self.s.irq_enabled {
            return;
        }
        for _ in 0..cycles {
            if self.s.irq_mode {
                // 8-bit counter mode
                let (low, overflow) = ((self.s.irq_counter & 0xFF) as u8).overflowing_add(1);
                self.s.irq_counter = (self.s.irq_counter & 0xFF00) | (low as u16);
                if overflow {
                    let low_latch = (self.s.irq_latch & 0xFF) as u16;
                    self.s.irq_counter = (self.s.irq_counter & 0xFF00) | low_latch;
                    self.s.irq_pending = true;
                }
            } else {
                // 16-bit counter mode
                let (counter, overflow) = self.s.irq_counter.overflowing_add(1);
                self.s.irq_counter = counter;
                if overflow {
                    self.s.irq_counter = self.s.irq_latch;
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