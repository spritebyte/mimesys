use crate::nes::mappers::{Mapper, Mirroring, mirror_nt, encode_state, decode_state};
use serde::{Serialize, Deserialize};

// iNES Mapper 32: Irem G-101 ASIC
// Features switchable 8K PRG banking, eight 1K CHR banks, and mirroring/PRG mode control.

pub struct Mapper32 {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_ram: Vec<u8>,
    s: State,
}

#[derive(Serialize, Deserialize, Clone)]
struct State {
    prg_regs: [u8; 2],     // Two switchable 8K PRG banks ($8000 and $A000)
    chr: [u8; 8],          // Eight 1K CHR banks ($0000-$1FFF)
    prg_mode: bool,        // PRG swap mode ($9000 bit 1)
    mirroring: u8,         // Mirroring ($9000 bit 0: 0 = Vertical, 1 = Horizontal)
    prg_ram: Vec<u8>,
    sram_dirty: bool,
}

impl Mapper32 {
    pub fn new(_mapper: u16, _submapper: u8, prg_rom: Vec<u8>, chr_rom: Vec<u8>, mirroring: Mirroring) -> Self {
        let chr_ram = if chr_rom.is_empty() { vec![0; 8192] } else { vec![] };
        Self {
            prg_rom, chr_rom, chr_ram,
            s: State {
                prg_regs: [0, 1],
                chr: [0, 1, 2, 3, 4, 5, 6, 7],
                prg_mode: false,
                mirroring: match mirroring {
                    Mirroring::Vertical => 0,
                    Mirroring::Horizontal => 1,
                    Mirroring::SingleLower => 2,
                    Mirroring::SingleUpper | Mirroring::FourScreen => 3,
                },
                prg_ram: vec![0; 8192],
                sram_dirty: false,
            },
        }
    }

    fn prg_banks_count(&self) -> usize { (self.prg_rom.len() / 0x2000).max(4) }
}

impl Mapper for Mapper32 {
    fn cpu_read(&self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => self.s.prg_ram[(addr - 0x6000) as usize],
            0x8000..=0xFFFF => {
                let n = self.prg_banks_count();
                let slot = (addr as usize - 0x8000) / 0x2000;
                let bank = match slot {
                    0 => {
                        if self.s.prg_mode {
                            n - 2
                        } else {
                            self.s.prg_regs[0] as usize
                        }
                    }
                    1 => self.s.prg_regs[1] as usize,
                    2 => {
                        if self.s.prg_mode {
                            self.s.prg_regs[0] as usize
                        } else {
                            n - 2
                        }
                    }
                    _ => n - 1, // Fixed last 8K bank at $E000
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
            0x8000..=0x8FFF => {
                self.s.prg_regs[0] = value & 0x1F;
            }
            0x9000..=0x9FFF => {
                self.s.mirroring = value & 0x01; // Bit 0: Mirroring
                self.s.prg_mode = (value & 0x02) != 0; // Bit 1: PRG swap mode
            }
            0xA000..=0xAFFF => {
                self.s.prg_regs[1] = value & 0x1F;
            }
            0xB000..=0xBFFF => {
                let idx = (addr & 0x0007) as usize;
                self.s.chr[idx] = value;
            }
            _ => {}
        }
    }

    fn ppu_read(&self, addr: u16) -> u8 {
        let addr = addr as usize & 0x1FFF;
        let bank_idx = addr / 0x400;
        let bank_val = self.s.chr[bank_idx] as usize;
        let offset = addr & 0x3FF;

        if self.chr_rom.is_empty() {
            self.chr_ram[(bank_val * 0x400 + offset) % self.chr_ram.len()]
        } else {
            self.chr_rom[(bank_val * 0x400 + offset) % self.chr_rom.len()]
        }
    }

    fn ppu_write(&mut self, addr: u16, value: u8) {
        let addr = addr as usize & 0x1FFF;
        let bank_idx = addr / 0x400;
        let bank_val = self.s.chr[bank_idx] as usize;
        let offset = addr & 0x3FF;

        if self.chr_rom.is_empty() {
            let target = (bank_val * 0x400 + offset) % self.chr_ram.len();
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