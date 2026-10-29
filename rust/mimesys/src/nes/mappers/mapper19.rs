use crate::nes::mappers::{Mapper, Mirroring, mirror_nt, encode_state, decode_state};
use serde::{Serialize, Deserialize};

// iNES Mapper 19: Namcot 106 / Namco 163 (N163)
// Features 128 bytes of internal RAM, 15-bit cycle-based IRQ, and 16 CHR/NT banking registers.

pub struct Mapper19 {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_ram: Vec<u8>,
    s: State,
}

#[derive(Serialize, Deserialize, Clone)]
struct State {
    prg: [u8; 3],          // Three 8K PRG banks at $8000, $A000, $C000
    chr: [u8; 16],         // Sixteen 1K CHR/NT banks (covering 0x0000-0x3FFF)
    #[serde(with = "serde_bytes")]
    internal_ram: [u8; 128], // 128 bytes internal chip RAM ($4800-$4FFF)
    irq_counter: u16,      // 15-bit cycle up-counter
    irq_enabled: bool,
    irq_pending: bool,
    mirroring: u8,
    prg_ram: Vec<u8>,
    sram_dirty: bool,
}

impl Mapper19 {
    pub fn new(_mapper: u16, _submapper: u8, prg_rom: Vec<u8>, chr_rom: Vec<u8>, mirroring: Mirroring) -> Self {
        let chr_ram = if chr_rom.is_empty() { vec![0; 8192] } else { vec![] };
        Self {
            prg_rom, chr_rom, chr_ram,
            s: State {
                prg: [0, 1, 2],
                chr: [0; 16],
                internal_ram: [0; 128],
                irq_counter: 0,
                irq_enabled: false,
                irq_pending: false,
                mirroring: if mirroring == Mirroring::Horizontal { 1 } else { 0 },
                prg_ram: vec![0; 8192],
                sram_dirty: false,
            },
        }
    }

    fn prg_banks_count(&self) -> usize { (self.prg_rom.len() / 0x2000).max(4) }
}

impl Mapper for Mapper19 {
    fn cpu_read(&self, addr: u16) -> u8 {
        match addr {
            0x4800..=0x4FFF => self.s.internal_ram[(addr - 0x4800) as usize & 0x7F],
            0x5000..=0x57FF => (self.s.irq_counter & 0xFF) as u8,
            0x5800..=0x5FFF => {
                let high = ((self.s.irq_counter >> 8) & 0x7F) as u8;
                let enable = if self.s.irq_enabled { 0x80 } else { 0 };
                high | enable
            }
            0x6000..=0x7FFF => self.s.prg_ram[(addr - 0x6000) as usize],
            0x8000..=0xFFFF => {
                let n = self.prg_banks_count();
                let slot = (addr as usize - 0x8000) / 0x2000;
                let bank = match slot {
                    0 => self.s.prg[0] as usize,
                    1 => self.s.prg[1] as usize,
                    2 => self.s.prg[2] as usize,
                    _ => n - 1, // Fixed last 8K bank at $E000
                } % n;
                self.prg_rom[bank * 0x2000 + (addr as usize & 0x1FFF)]
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, value: u8) {
        match addr {
            0x4800..=0x4FFF => {
                self.s.internal_ram[(addr - 0x4800) as usize & 0x7F] = value;
            }
            0x5000..=0x57FF => {
                self.s.irq_counter = (self.s.irq_counter & 0x7F00) | (value as u16);
                self.s.irq_pending = false;
            }
            0x5800..=0x5FFF => {
                self.s.irq_counter = (self.s.irq_counter & 0x00FF) | (((value as u16) & 0x7F) << 8);
                self.s.irq_enabled = (value & 0x80) != 0;
                self.s.irq_pending = false;
            }
            0x6000..=0x7FFF => {
                self.s.prg_ram[(addr - 0x6000) as usize] = value;
                self.s.sram_dirty = true;
            }
            0x8000..=0xDFFF => {
                let idx = ((addr - 0x8000) / 0x0800) as usize;
                if idx < 16 {
                    self.s.chr[idx] = value;
                }
            }
            0xE000..=0xE7FF => self.s.prg[0] = value & 0x3F,
            0xE800..=0xEFFF => self.s.prg[1] = value & 0x3F,
            0xF000..=0xF7FF => self.s.prg[2] = value & 0x3F,
            _ => {}
        }
    }

    fn step_cycles(&mut self, cycles: u64) {
        if !self.s.irq_enabled { return; }
        for _ in 0..cycles {
            if self.s.irq_counter < 0x7FFF {
                self.s.irq_counter += 1;
                if self.s.irq_counter == 0x7FFF {
                    self.s.irq_pending = true;
                }
            }
        }
    }

    fn is_irq_asserted(&self) -> bool { self.s.irq_pending }

    fn ppu_read(&self, addr: u16) -> u8 {
        let addr = addr as usize & 0x3FFF;
        let bank_idx = addr / 0x400;
        let bank_val = self.s.chr[bank_idx] as usize;
        let offset = addr & 0x3FF;

        if bank_val >= 0xE0 {
            0
        } else if self.chr_rom.is_empty() {
            self.chr_ram[(bank_val * 0x400 + offset) % self.chr_ram.len()]
        } else {
            self.chr_rom[(bank_val * 0x400 + offset) % self.chr_rom.len()]
        }
    }

    fn ppu_write(&mut self, addr: u16, value: u8) {
        let addr = addr as usize & 0x3FFF;
        let bank_idx = addr / 0x400;
        let bank_val = self.s.chr[bank_idx] as usize;
        let offset = addr & 0x3FF;
        if self.chr_rom.is_empty() && bank_val < 0xE0 {
            let target = (bank_val * 0x400 + offset) % self.chr_ram.len();
            self.chr_ram[target] = value;
        }
    }

    fn mirror_vram_address(&self, addr: u16) -> usize { mirror_nt(Mirroring::Vertical, addr) }

    fn get_sram(&self) -> Option<&[u8]> { Some(&self.s.prg_ram) }
    fn load_sram(&mut self, data: &[u8]) {
        let n = data.len().min(self.s.prg_ram.len());
        self.s.prg_ram[..n].copy_from_slice(&data[..n]);
    }
    fn is_sram_dirty(&self) -> bool { self.s.sram_dirty }
    fn clear_sram_dirty(&mut self) { self.s.sram_dirty = false; }

    fn save_state(&self) -> Vec<u8> { encode_state(&self.s) }
    fn load_state(&mut self, data: &[u8]) { if let Some(s) = decode_state(data) { self.s = s; } }
}