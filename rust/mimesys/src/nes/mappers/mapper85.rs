use crate::nes::mappers::{
    decode_state, encode_state, mirror_nt, Mapper, Mirroring,
};
use serde::{Deserialize, Serialize};

// iNES Mapper 85: Konami VRC7 (Lagrange Point, Tiny Toon Adventures 2)

pub struct Mapper85 {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_ram: Vec<u8>,
    s: State,
}

#[derive(Serialize, Deserialize, Clone)]
struct State {
    prg_regs: [u8; 3],
    chr_regs: [u8; 8],
    mirroring: u8,
    wram_enabled: bool,
    is_vrc7b: bool,

    // IRQ State
    irq_latch: u8,
    irq_counter: u8,
    irq_enabled: bool,
    irq_acknowledge: bool,
    irq_pending: bool,

    prg_ram: Vec<u8>,
    sram_dirty: bool,
    irq_cycle_mode: bool,
    irq_prescaler: i32,
}

impl Mapper85 {
    pub fn new(
        _mapper: u16,
        submapper: u8,
        prg_rom: Vec<u8>,
        chr_rom: Vec<u8>,
        mirroring: Mirroring,
    ) -> Self {
        let chr_ram = if chr_rom.is_empty() {
            vec![0; 8192]
        } else {
            Vec::new()
        };

        // Submapper 1 = VRC7b (uses A3 / $x008), used by Tiny Toon Adventures 2
        // Submapper 2 = VRC7a (uses A4 / $x010), used by Lagrange Point
        // If submapper is 0 (iNES 1.0), auto-detect: Tiny Toons 2 is 128 KiB, Lagrange Point is 512 KiB.
        let is_vrc7b = match submapper {
            1 => true,
            2 => false,
            _ => prg_rom.len() <= 0x40000, // <= 256K defaults to VRC7b, larger is VRC7a
        };

        Self {
            prg_rom,
            chr_rom,
            chr_ram,
            s: State {
                prg_regs: [0, 1, 2],
                chr_regs: [0, 1, 2, 3, 4, 5, 6, 7],
                mirroring: match mirroring {
                    Mirroring::Vertical => 0,
                    Mirroring::Horizontal => 1,
                    Mirroring::SingleLower => 2,
                    Mirroring::SingleUpper => 3,
                    Mirroring::FourScreen => 4,
                },
                wram_enabled: true,
                is_vrc7b,
                irq_latch: 0,
                irq_counter: 0,
                irq_enabled: false,
                irq_acknowledge: false,
                irq_pending: false,
                prg_ram: vec![0; 8192],
                sram_dirty: false,
                irq_cycle_mode: false,
                irq_prescaler: 341,
            },
        }
    }

    fn prg_bank_count(&self) -> usize {
        self.prg_rom.len() / 0x2000 // 8 KiB banks
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
        let bank_count = len.div_ceil(0x400);

        let bank = self.s.chr_regs[slot] as usize % bank_count;
        Some((bank * 0x400 + offset) % len)
    }

    fn clock_irq_counter(&mut self) {
        if self.s.irq_counter == 0xFF {
            self.s.irq_counter = self.s.irq_latch;
            self.s.irq_pending = true;
        } else {
            self.s.irq_counter = self.s.irq_counter.wrapping_add(1);
        }
    }

    fn clock_irq(&mut self, cycles: u64) {
        if !self.s.irq_enabled {
            return;
        }

        if self.s.irq_cycle_mode {
            for _ in 0..cycles {
                self.clock_irq_counter();
            }
        } else {
            for _ in 0..cycles {
                self.s.irq_prescaler -= 3;

                if self.s.irq_prescaler <= 0 {
                    self.s.irq_prescaler += 341;
                    self.clock_irq_counter();
                }
            }
        }
    }
}

impl Mapper for Mapper85 {
    fn cpu_read(&self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => {
                if self.s.wram_enabled {
                    self.s.prg_ram[(addr - 0x6000) as usize]
                } else {
                    0
                }
            }
            0x8000..=0xFFFF => {
                let count = self.prg_bank_count();
                if count == 0 {
                    return 0;
                }

                let bank = match addr {
                    0x8000..=0x9FFF => self.s.prg_regs[0] as usize,
                    0xA000..=0xBFFF => self.s.prg_regs[1] as usize,
                    0xC000..=0xDFFF => self.s.prg_regs[2] as usize,
                    _ => count - 1, // Fixed last 8K bank at $E000-$FFFF
                } % count;

                let index = bank * 0x2000 + (addr as usize & 0x1FFF);
                self.prg_rom.get(index).copied().unwrap_or(0)
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, value: u8) {
        match addr {
            0x6000..=0x7FFF => {
                if self.s.wram_enabled {
                    self.s.prg_ram[(addr - 0x6000) as usize] = value;
                    self.s.sram_dirty = true;
                }
            }
            0x8000..=0xFFFF => {
                let mask = if self.s.is_vrc7b { 0x0008 } else { 0x0010 };
                let bank = (addr & 0xF000) as usize;
                let variant_bit = addr & mask != 0;
                let sound_bit = addr & 0x0020 != 0;

                match bank {
                    0x8000 => {
                        if variant_bit {
                            self.s.prg_regs[1] = value;
                        } else {
                            self.s.prg_regs[0] = value;
                        }
                    }
                    0x9000 => {
                        if sound_bit {
                            // VRC7 FM data port: $9030 (VRC7a),
                            // $9028 (VRC7b). Connect to your audio core here.
                        } else if variant_bit {
                            // VRC7 FM address port: $9010 (VRC7a),
                            // $9008 (VRC7b). Connect to your audio core here.
                        } else {
                            self.s.prg_regs[2] = value;
                        }
                    }
                    0xA000..=0xD000 => {
                        // FIXED: Properly offset base index from 0xA000 (10)
                        let base_idx = (((addr >> 12) - 10) as usize) * 2;
                        let reg_idx = base_idx + usize::from(variant_bit);
                        if reg_idx < 8 {
                            self.s.chr_regs[reg_idx] = value;
                        }
                    }
                    0xE000 => {
                        if variant_bit {
                            self.s.irq_latch = value;
                        } else {
                            self.s.mirroring = value & 0x03;
                            self.s.wram_enabled = (value & 0x80) != 0;
                        }
                    }
                    0xF000 => {
                        if variant_bit {
                            // IRQ acknowledge: clear pending and restore
                            // the enable state from the acknowledge flag.
                            self.s.irq_pending = false;
                            self.s.irq_enabled = self.s.irq_acknowledge;
                        } else {
                            // IRQ Control
                            self.s.irq_pending = false;
                            self.s.irq_cycle_mode = (value & 0x04) != 0;
                            self.s.irq_enabled = (value & 0x02) != 0;
                            self.s.irq_acknowledge = (value & 0x01) != 0;


                            if self.s.irq_enabled {
                                self.s.irq_counter = self.s.irq_latch;
                                self.s.irq_prescaler = 341;
                            }
                        }
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
            if let Some(byte) = self.chr_ram.get_mut(index) {
                *byte = value;
            }
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