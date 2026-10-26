use crate::nes::mappers::{Mapper, Mirroring, mirror_nt, encode_state, decode_state};
use serde::{Serialize, Deserialize};

// Mapper 11 (Color Dreams; also Wisdom Tree)
// $8000-$FFFF write: bits 0-1 = 32K PRG bank, bits 4-7 = 8K CHR bank. Mirroring fixed by
// the board. (Bus conflicts exist on the real carts; games avoid them, so ignored.)
pub struct Mapper11 {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_ram: Vec<u8>,
    mirroring: Mirroring,
    s: State,
}

#[derive(Serialize, Deserialize, Default, Clone)]
struct State { prg_bank: u8, chr_bank: u8 }

impl Mapper11 {
    pub fn new(prg_rom: Vec<u8>, chr_rom: Vec<u8>, mirroring: Mirroring) -> Self {
        let chr_ram = if chr_rom.is_empty() { vec![0; 8192] } else { vec![] };
        Self { prg_rom, chr_rom, chr_ram, mirroring, s: State::default() }
    }
}

impl Mapper for Mapper11 {
    fn cpu_read(&self, addr: u16) -> u8 {
        if addr < 0x8000 { return 0; }
        let banks = (self.prg_rom.len() / 0x8000).max(1);
        let off = (self.s.prg_bank as usize % banks) * 0x8000 + (addr as usize & 0x7FFF);
        self.prg_rom[off % self.prg_rom.len()]
    }
    fn cpu_write(&mut self, addr: u16, value: u8) {
        if addr >= 0x8000 {
            self.s.prg_bank = value & 0x03;
            self.s.chr_bank = value >> 4;
        }
    }
    fn ppu_read(&self, addr: u16) -> u8 {
        let addr = addr as usize & 0x1FFF;
        if self.chr_rom.is_empty() { return self.chr_ram[addr]; }
        let banks = (self.chr_rom.len() / 0x2000).max(1);
        self.chr_rom[((self.s.chr_bank as usize % banks) * 0x2000 + addr) % self.chr_rom.len()]
    }
    fn ppu_write(&mut self, addr: u16, value: u8) {
        if self.chr_rom.is_empty() && addr < 0x2000 { self.chr_ram[addr as usize] = value; }
    }
    fn mirror_vram_address(&self, addr: u16) -> usize { mirror_nt(self.mirroring, addr) }
    fn save_state(&self) -> Vec<u8> { encode_state(&self.s) }
    fn load_state(&mut self, data: &[u8]) { if let Some(s) = decode_state(data) { self.s = s; } }
}
