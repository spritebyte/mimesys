use crate::nes::mappers::{
    decode_state, encode_state, mirror_nt, Mapper, Mirroring,
};
use serde::{Deserialize, Serialize};

// iNES Mapper 48: Taito TC0190FMC + PAL16R4 / TC0690

pub struct Mapper48 {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_ram: Vec<u8>,
    s: State,
}

#[derive(Serialize, Deserialize, Clone)]
struct State {
    prg_regs: [u8; 2],
    chr_regs: [u8; 6],
    mirroring: u8,

    irq_latch: u8,
    irq_counter: u8,
    irq_enabled: bool,
    irq_pending: bool,
    irq_reload: bool,

    a12_high: bool,
    last_a12_low_cycle: u64,

    prg_ram: Vec<u8>,
    sram_dirty: bool,
}

impl Mapper48 {
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
                prg_regs: [0, 1],
                chr_regs: [0; 6], // Properly initialized to zero on power-on

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
                irq_reload: false,

                a12_high: false,
                last_a12_low_cycle: 0,

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

    fn chr_bank_for_addr(&self, addr: usize) -> (usize, usize, usize) {
        match addr & 0x1FFF {
            // Two 2 KiB CHR banks ($0000-$0FFF)
            0x0000..=0x07FF => (self.s.chr_regs[0] as usize, addr & 0x7FF, 0x800),
            0x0800..=0x0FFF => (self.s.chr_regs[1] as usize, addr & 0x7FF, 0x800),
            
            // Four 1 KiB CHR banks ($1000-$1FFF)
            0x1000..=0x13FF => (self.s.chr_regs[2] as usize, addr & 0x3FF, 0x400),
            0x1400..=0x17FF => (self.s.chr_regs[3] as usize, addr & 0x3FF, 0x400),
            0x1800..=0x1BFF => (self.s.chr_regs[4] as usize, addr & 0x3FF, 0x400),
            _               => (self.s.chr_regs[5] as usize, addr & 0x3FF, 0x400),
        }
    }

    fn chr_index(&self, addr: usize) -> Option<usize> {
        let len = self.chr_memory_len();
        if len == 0 {
            return None;
        }

        let addr = addr as usize & 0x1FFF;
        let (reg_val, offset, stride) = self.chr_bank_for_addr(addr);
        let target = reg_val * stride + offset;

        Some(target % len)
    }

    fn clock_irq(&mut self) {
        if !self.s.irq_enabled {
            return;
        }

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

impl Mapper for Mapper48 {
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
                    2 => count.saturating_sub(2),
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

            0x8000..=0xFFFF => {
                match addr & 0x6003 {
                    // $8000: Switchable PRG bank 0
                    0x0000 => {
                        self.s.prg_regs[0] = value;
                    }
                    // $8001: Switchable PRG bank 1
                    0x0001 => {
                        self.s.prg_regs[1] = value;
                    }

                    // Two 2 KiB CHR banks ($8002, $8003)
                    0x0002 => {
                        self.s.chr_regs[0] = value;
                    }
                    0x0003 => {
                        self.s.chr_regs[1] = value;
                    }

                    // Four 1 KiB CHR banks ($A000, $A001, $A002, $A003 -> Evaluates to 0x2000..=0x2003)
                    0x2000 => {
                        self.s.chr_regs[2] = value;
                    }
                    0x2001 => {
                        self.s.chr_regs[3] = value;
                    }
                    0x2002 => {
                        self.s.chr_regs[4] = value;
                    }
                    0x2003 => {
                        self.s.chr_regs[5] = value;
                    }

                    // IRQ Latch ($C000 -> Evaluates to 0x4000)
                    0x4000 => {
                        self.s.irq_latch = value ^ 0xFF;
                    }

                    // IRQ Reload ($C001 -> Evaluates to 0x4001)
                    0x4001 => {
                    //    self.s.irq_counter = self.s.irq_latch;
                        self.s.irq_reload = false;
                    }

                    // IRQ Enable ($C002 -> Evaluates to 0x4002)
                    0x4002 => {
                        self.s.irq_enabled = true;
                    }

                    // IRQ Disable & Acknowledge ($C003 -> Evaluates to 0x4003)
                    0x4003 => {
                        self.s.irq_enabled = false;
                        self.s.irq_pending = false;
                    }

                    // Mirroring control ($E000 bit 6 -> Evaluates to 0x6000)
                    0x6000 => {
                        self.s.mirroring = if (value & 0x40) != 0 { 1 } else { 0 };
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

    fn update_a12(&mut self, addr: u16, ppu_cycle: u64) {
        let high = addr & 0x1000 != 0;

        if !high {
            if self.s.a12_high {
                self.s.a12_high = false;
                self.s.last_a12_low_cycle = ppu_cycle;
            }
            return;
        }

        if !self.s.a12_high {
            self.s.a12_high = true;

            if ppu_cycle.wrapping_sub(self.s.last_a12_low_cycle) >= 8 {
                self.clock_irq();
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