use crate::nes::mappers::{Mapper, Mirroring, mirror_nt, encode_state, decode_state};
use serde::{Serialize, Deserialize};

// iNES Mapper 76: Namcot 3446 (Namco 109 variant / Megami Tensei)
// MMC3-like architecture modified to support 128 KiB CHR via 2KiB CHR bank selection.

pub struct Mapper76 {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_ram: Vec<u8>,
    s: State,
}

#[derive(Serialize, Deserialize, Clone)]
struct State {
    bank_select: u8,
    prg_regs: [u8; 2],     // Two switchable 8K PRG banks
    chr_regs: [u8; 4],     // Four switchable 2K CHR banks
    mirroring: u8,
    prg_ram: Vec<u8>,
    sram_dirty: bool,
}

impl Mapper76 {
    pub fn new(_mapper: u16, _submapper: u8, prg_rom: Vec<u8>, chr_rom: Vec<u8>, mirroring: Mirroring) -> Self {
        let chr_ram = if chr_rom.is_empty() { vec![0; 8192] } else { vec![] };
        Self {
            prg_rom, chr_rom, chr_ram,
            s: State {
                bank_select: 0,
                prg_regs: [0, 1],
                chr_regs: [0; 4],
                mirroring: if mirroring == Mirroring::Horizontal { 1 } else { 0 },
                prg_ram: vec![0; 8192],
                sram_dirty: false,
            },
        }
    }

    fn prg_banks_count(&self) -> usize { (self.prg_rom.len() / 0x2000).max(4) }
}

impl Mapper for Mapper76 {
    fn cpu_read(&self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => self.s.prg_ram[(addr - 0x6000) as usize],
            0x8000..=0xFFFF => {
                let n = self.prg_banks_count();
                let slot = (addr as usize - 0x8000) / 0x2000;
                let swap = (self.s.bank_select & 0x40) != 0;
                
                let bank = match (slot, swap) {
                    (0, false) => self.s.prg_regs[0] as usize,
                    (0, true)  => n - 2,
                    (1, _)     => self.s.prg_regs[1] as usize,
                    (2, false) => n - 2,
                    (2, true)  => self.s.prg_regs[0] as usize,
                    _          => n - 1, // Slot 3 ($E000-$FFFF) is always last bank
                } % n;

                self.prg_rom[bank * 0x2000 + (addr as usize & 0x1FFF)]
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
            0x8000..=0x9FFE if (addr & 1) == 0 => {
                self.s.bank_select = value;
            }
            0x8001..=0x9FFF if (addr & 1) == 1 => {
                match self.s.bank_select & 0x07 {
                    2 => self.s.chr_regs[0] = value, // 2K CHR at $0000
                    3 => self.s.chr_regs[1] = value, // 2K CHR at $0800
                    4 => self.s.chr_regs[2] = value, // 2K CHR at $1000
                    5 => self.s.chr_regs[3] = value, // 2K CHR at $1800
                    6 => self.s.prg_regs[0] = value & 0x1F, // PRG reg 0
                    7 => self.s.prg_regs[1] = value & 0x1F, // PRG reg 1
                    _ => {}
                }
            }
            0xA000..=0xBFFE if (addr & 1) == 0 => {
                self.s.mirroring = value & 1;
            }
            _ => {}
        }
    }

    fn ppu_read(&self, addr: u16) -> u8 {
        let addr = addr as usize & 0x1FFF;
        let bank_idx = addr / 0x800; // Four 2K windows
        let bank_val = self.s.chr_regs[bank_idx] as usize;
        let offset = addr & 0x7FF;

        if self.chr_rom.is_empty() {
            self.chr_ram[(bank_val * 0x800 + offset) % self.chr_ram.len()]
        } else {
            self.chr_rom[(bank_val * 0x800 + offset) % self.chr_rom.len()]
        }
    }

    fn ppu_write(&mut self, addr: u16, value: u8) {
        let addr = addr as usize & 0x1FFF;
        let bank_idx = addr / 0x800;
        let bank_val = self.s.chr_regs[bank_idx] as usize;
        let offset = addr & 0x7FF;

        if self.chr_rom.is_empty() {
            let target = (bank_val * 0x800 + offset) % self.chr_ram.len();
            self.chr_ram[target] = value;
        }
    }

    fn mirror_vram_address(&self, addr: u16) -> usize {
        let m = if self.s.mirroring == 0 { Mirroring::Vertical } else { Mirroring::Horizontal };
        mirror_nt(m, addr)
    }

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