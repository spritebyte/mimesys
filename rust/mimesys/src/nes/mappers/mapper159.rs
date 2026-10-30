use crate::nes::mappers::{
    decode_state, encode_state, mirror_nt, Mapper, Mirroring,
};
use serde::{Deserialize, Serialize};

// iNES Mapper 159: Bandai FCG (LZ93D50 + 128-byte X24C01 serial EEPROM)

pub struct Mapper159 {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_ram: Vec<u8>,
    s: State,
}

#[derive(Serialize, Deserialize, Clone)]
struct State {
    chr_regs: [u8; 8],
    prg_reg: usize,
    mirroring: u8,

    // IRQ State (LZ93D50 style)
    irq_latch: u16,
    irq_counter: u16,
    irq_enabled: bool,
    irq_pending: bool,

    // EEPROM State (128 bytes for X24C01)
    eeprom: Vec<u8>,
    scl: bool,
    sda_out: bool,
    sda_in: bool,

    sram_dirty: bool,
}

impl Mapper159 {
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
                chr_regs: [0; 8],
                prg_reg: 0,
                mirroring: match mirroring {
                    Mirroring::Vertical => 0,
                    Mirroring::Horizontal => 1,
                    Mirroring::SingleLower => 2,
                    Mirroring::SingleUpper => 3,
                    Mirroring::FourScreen => 4,
                },
                irq_latch: 0,
                irq_counter: 0,
                irq_enabled: false,
                irq_pending: false,
                eeprom: vec![0; 128], // 128-byte X24C01 serial EEPROM
                scl: false,
                sda_out: false,
                sda_in: false,
                sram_dirty: false,
            },
        }
    }

    fn prg_bank_count(&self) -> usize {
        self.prg_rom.len() / 0x4000 // 16 KiB banks
    }

    fn chr_bank_count(&self) -> usize {
        let len = if self.chr_rom.is_empty() {
            self.chr_ram.len()
        } else {
            self.chr_rom.len()
        };
        len / 0x400 // 1 KiB banks
    }

    fn chr_index(&self, addr: usize) -> Option<usize> {
        let slot = (addr & 0x1FFF) / 0x400;
        let offset = addr & 0x3FF;
        let count = self.chr_bank_count();
        if count == 0 {
            return None;
        }
        let bank = self.s.chr_regs[slot] as usize % count;
        Some(bank * 0x400 + offset)
    }

    fn clock_irq(&mut self, cycles: u64) {
        if !self.s.irq_enabled {
            return;
        }
        for _ in 0..cycles {
            if self.s.irq_counter == 0 {
                self.s.irq_pending = true;
                self.s.irq_enabled = false;
            } else {
                self.s.irq_counter = self.s.irq_counter.wrapping_sub(1);
            }
        }
    }
}

impl Mapper for Mapper159 {
    fn cpu_read(&self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => {
                if !self.s.eeprom.is_empty() {
                    if self.s.sda_in { 1 } else { 0 }
                } else {
                    0
                }
            }
            0x8000..=0xFFFF => {
                let count = self.prg_bank_count();
                if count == 0 {
                    return 0;
                }
                let (bank, offset) = if addr < 0xC000 {
                    (self.s.prg_reg % count, (addr as usize - 0x8000))
                } else {
                    (count - 1, (addr as usize - 0xC000))
                };
                let index = (bank * 0x4000) + offset;
                self.prg_rom.get(index).copied().unwrap_or(0)
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, value: u8) {
        match addr {
            0x8000..=0x800F => {
                match addr & 0x000F {
                    0x0..=0x7 => {
                        let slot = (addr & 0x0007) as usize;
                        self.s.chr_regs[slot] = value;
                    }
                    0x8 => {
                        self.s.prg_reg = value as usize;
                    }
                    0x9 => {
                        self.s.mirroring = value & 0x03;
                    }
                    0xA => {
                        self.s.irq_enabled = (value & 0x01) != 0;
                        self.s.irq_pending = false;
                        self.s.irq_counter = self.s.irq_latch;
                    }
                    0xB => {
                        self.s.irq_latch = (self.s.irq_latch & 0xFF00) | (value as u16);
                    }
                    0xC => {
                        self.s.irq_latch = (self.s.irq_latch & 0x00FF) | ((value as u16) << 8);
                    }
                    0xD => {
                        self.s.scl = (value & 0x01) != 0;
                        self.s.sda_out = (value & 0x02) != 0;
                        self.s.sram_dirty = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn ppu_read(&self, addr: u16) -> u8 {
        let addr = addr as usize & 0x1FFF;
        if let Some(index) = self.chr_index(addr) {
            if self.chr_rom.is_empty() {
                self.chr_ram.get(index % self.chr_ram.len()).copied().unwrap_or(0)
            } else {
                self.chr_rom.get(index % self.chr_rom.len()).copied().unwrap_or(0)
            }
        } else {
            0
        }
    }

    fn ppu_write(&mut self, addr: u16, value: u8) {
        if !self.chr_rom.is_empty() {
            return;
        }
        let addr = addr as usize & 0x1FFF;
        if let Some(index) = self.chr_index(addr) {
            let target = index % self.chr_ram.len();
            self.chr_ram[target] = value;
        }
    }

    fn step_cycles(&mut self, cycles: u64) {
        self.clock_irq(cycles);
    }

    fn is_irq_asserted(&self) -> bool {
        self.s.irq_pending
    }

    fn mirror_vram_address(&self, addr: u16) -> usize {
        let mirroring = match self.s.mirroring & 0x03 {
            0 => Mirroring::Vertical,
            1 => Mirroring::Horizontal,
            2 => Mirroring::SingleLower,
            3 => Mirroring::SingleUpper,
            _ => Mirroring::Horizontal,
        };
        mirror_nt(mirroring, addr)
    }

    fn get_sram(&self) -> Option<&[u8]> {
        Some(&self.s.eeprom)
    }

    fn load_sram(&mut self, data: &[u8]) {
        let n = data.len().min(self.s.eeprom.len());
        self.s.eeprom[..n].copy_from_slice(&data[..n]);
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