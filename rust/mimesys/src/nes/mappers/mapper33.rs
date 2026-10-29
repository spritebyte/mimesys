use crate::nes::mappers::{Mapper, Mirroring, mirror_nt, encode_state, decode_state};
use serde::{Serialize, Deserialize};

// iNES Mapper 33: Taito TC0190
// Features two switchable 8K PRG banks, mixed 2K/1K CHR banking, and $8000 bit 6 mirroring.

pub struct Mapper33 {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_ram: Vec<u8>,
    s: State,
}

#[derive(Serialize, Deserialize, Clone)]
struct State {
    prg_regs: [u8; 2],     // Two switchable 8K PRG banks ($8000, $8001)
    chr_regs: [u8; 6],     // CHR banks: Regs 0-1 (2K), Regs 2-5 (1K)
    mirroring: u8,         // Mirroring ($8000 bit 6: 0 = Vertical, 1 = Horizontal)
    prg_ram: Vec<u8>,
    sram_dirty: bool,
}

impl Mapper33 {
    pub fn new(_mapper: u16, _submapper: u8, prg_rom: Vec<u8>, chr_rom: Vec<u8>, mirroring: Mirroring) -> Self {
        let chr_ram = if chr_rom.is_empty() { vec![0; 8192] } else { vec![] };
        Self {
            prg_rom, chr_rom, chr_ram,
            s: State {
                prg_regs: [0, 1],
                chr_regs: [0, 1, 2, 3, 4, 5],
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

    fn prg_banks_count(&self) -> usize {
        self.prg_rom.len() / 0x2000
    }

    fn chr_read_bank(&self, bank: usize, offset: usize, size: usize) -> u8 {
        let memory = if self.chr_rom.is_empty() {
            &self.chr_ram
        } else {
            &self.chr_rom
        };

        if memory.is_empty() {
            return 0;
        }

        let bank_count = memory.len() / size;
        if bank_count == 0 {
            return 0;
        }

        memory[(bank % bank_count) * size + offset]
    }
}

impl Mapper for Mapper33 {
    fn cpu_read(&self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => self.s.prg_ram[(addr - 0x6000) as usize],
            0x8000..=0xFFFF => {
                let n = self.prg_banks_count();
                let slot = (addr as usize - 0x8000) / 0x2000;
                let bank = match slot {
                    0 => self.s.prg_regs[0] as usize,
                    1 => self.s.prg_regs[1] as usize,
                    2 => n - 2, // Fixed second-to-last 8K bank at $C000
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
            0x8000..=0xFFFF => {
                match addr & 0x6003 {
                    0x0000 => {
                        self.s.prg_regs[0] = value & 0x3F;
                        self.s.mirroring = (value >> 6) & 1;
                    }
                    0x0001 => {
                        self.s.prg_regs[1] = value & 0x3F;
                    }
                    0x0002 => {
                        self.s.chr_regs[0] = value;
                    }
                    0x0003 => {
                        self.s.chr_regs[1] = value;
                    }
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
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn ppu_read(&self, addr: u16) -> u8 {
        let addr = addr as usize & 0x1FFF;
        // Taito TC0190: Two 2K CHR banks ($0000-$0FFF) followed by four 1K CHR banks ($1000-$1FFF)
        let (reg_val, offset, stride) = match addr {
            0x0000..=0x07FF => (self.s.chr_regs[0] as usize, addr & 0x7FF, 0x800), // 2K Bank 0
            0x0800..=0x0FFF => (self.s.chr_regs[1] as usize, addr & 0x7FF, 0x800), // 2K Bank 1
            0x1000..=0x13FF => (self.s.chr_regs[2] as usize, addr & 0x3FF, 0x400), // 1K Bank 0
            0x1400..=0x17FF => (self.s.chr_regs[3] as usize, addr & 0x3FF, 0x400), // 1K Bank 1
            0x1800..=0x1BFF => (self.s.chr_regs[4] as usize, addr & 0x3FF, 0x400), // 1K Bank 2
            _               => (self.s.chr_regs[5] as usize, addr & 0x3FF, 0x400), // 1K Bank 3
        };

        let target = reg_val * stride + offset;
        if self.chr_rom.is_empty() {
            self.chr_ram[target % self.chr_ram.len()]
        } else {
            self.chr_rom[target % self.chr_rom.len()]
        }
    }

    fn ppu_write(&mut self, addr: u16, value: u8) {
        let addr = addr as usize & 0x1FFF;
        let (reg_val, offset, stride) = match addr {
            0x0000..=0x07FF => (self.s.chr_regs[0] as usize, addr & 0x7FF, 0x800),
            0x0800..=0x0FFF => (self.s.chr_regs[1] as usize, addr & 0x7FF, 0x800),
            0x1000..=0x13FF => (self.s.chr_regs[2] as usize, addr & 0x3FF, 0x400),
            0x1400..=0x17FF => (self.s.chr_regs[3] as usize, addr & 0x3FF, 0x400),
            0x1800..=0x1BFF => (self.s.chr_regs[4] as usize, addr & 0x3FF, 0x400),
            _               => (self.s.chr_regs[5] as usize, addr & 0x3FF, 0x400),
        };

        if self.chr_rom.is_empty() {
            let target = (reg_val * stride + offset) % self.chr_ram.len();
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