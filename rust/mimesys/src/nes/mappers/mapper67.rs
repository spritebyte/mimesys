use crate::nes::mappers::{
    decode_state, encode_state, mirror_nt, Mapper, Mirroring,
};
use serde::{Deserialize, Serialize};

// iNES Mapper 67: Sunsoft-3 (Fantasy Zone II, etc.)

pub struct Mapper67 {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_ram: Vec<u8>,
    s: State,
}

#[derive(Serialize, Deserialize, Clone)]
struct State {
    chr_regs: [usize; 4],
    prg_bank: usize,
    mirroring: u8,

    // IRQ State
    irq_counter: u16,
    irq_enabled: bool,
    irq_pending: bool,
    write_toggle: bool, // false = high byte, true = low byte

    prg_ram: Vec<u8>,
    sram_dirty: bool,
}

impl Mapper67 {
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
                chr_regs: [0; 4],
                prg_bank: 0,
                mirroring: match mirroring {
                    Mirroring::Vertical => 0,
                    Mirroring::Horizontal => 1,
                    Mirroring::SingleLower => 2,
                    Mirroring::SingleUpper => 3,
                    Mirroring::FourScreen => 4,
                },
                irq_counter: 0,
                irq_enabled: false,
                irq_pending: false,
                write_toggle: false,
                prg_ram: vec![0; 8192],
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
        len / 0x800 // 2 KiB banks
    }

    fn chr_index(&self, addr: usize) -> Option<usize> {
        let len = if self.chr_rom.is_empty() {
            self.chr_ram.len()
        } else {
            self.chr_rom.len()
        };
        if len == 0 {
            return None;
        }

        let slot = (addr & 0x1FFF) / 0x800;
        let offset = addr & 0x7FF;
        let bank_count = self.chr_bank_count().max(1);
        let bank = self.s.chr_regs[slot] % bank_count;
        Some((bank * 0x800 + offset) % len)
    }
}

impl Mapper for Mapper67 {
    fn cpu_read(&self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => self.s.prg_ram[(addr - 0x6000) as usize],
            0x8000..=0xFFFF => {
                let count = self.prg_bank_count();
                if count == 0 {
                    return 0;
                }
                let (bank, offset) = if addr < 0xC000 {
                    (self.s.prg_bank % count, (addr as usize - 0x8000))
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
            0x6000..=0x7FFF => {
                self.s.prg_ram[(addr - 0x6000) as usize] = value;
                self.s.sram_dirty = true;
            }

            0x8000..=0xFFFF => {
                match addr & 0xF800 {
                    // IRQ acknowledge
                    0x8000 => {
                        self.s.irq_pending = false;
                    }

                    // CHR bank 0: $0000-$07FF
                    0x8800 => {
                        self.s.chr_regs[0] = (value & 0x3F) as usize;
                    }

                    // CHR bank 1: $0800-$0FFF
                    0x9800 => {
                        self.s.chr_regs[1] = (value & 0x3F) as usize;
                    }

                    // CHR bank 2: $1000-$17FF
                    0xA800 => {
                        self.s.chr_regs[2] = (value & 0x3F) as usize;
                    }

                    // CHR bank 3: $1800-$1FFF
                    0xB800 => {
                        self.s.chr_regs[3] = (value & 0x3F) as usize;
                    }

                    // IRQ counter: high byte, then low byte
                    0xC800 => {
                        if !self.s.write_toggle {
                            self.s.irq_counter =
                                (self.s.irq_counter & 0x00FF)
                                | ((value as u16) << 8);

                            self.s.write_toggle = true;
                        } else {
                            self.s.irq_counter =
                                (self.s.irq_counter & 0xFF00)
                                | (value as u16);

                            self.s.write_toggle = false;
                        }
                    }

                    // IRQ control
                    0xD800 => {
                        self.s.irq_enabled = (value & 0x10) != 0;

                        // Reset counter write toggle
                        self.s.write_toggle = false;
                    }

                    // Mirroring
                    0xE800 => {
                        self.s.mirroring = value & 0x03;
                    }

                    // PRG bank select
                    0xF800 => {
                        self.s.prg_bank = (value & 0x0F) as usize;
                    }

                    _ => {}
                }
            }

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
            let target = index % self.chr_ram.len();
            self.chr_ram[target] = value;
        }
    }

    fn step_cycles(&mut self, cycles: u64) {
        if !self.s.irq_enabled { return; }

        for _ in 0..cycles {
            if self.s.irq_counter == 0 {
                self.s.irq_counter = 0xFFFF;

                self.s.irq_pending = true;
                self.s.irq_enabled = false;
            } else {
                self.s.irq_counter -= 1;
            }
        }
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