use crate::nes::mappers::{Mapper, Mirroring, mirror_nt, encode_state, decode_state};
use serde::{Serialize, Deserialize};

// Mapper 87 (Jaleco JF-xx / Konami: Goonies (J), City Connection, Argus...)
// PRG fixed (16K mirrored or 32K). $6000-$7FFF write: 8K CHR bank, with the two low bits
// swapped (bit 0 -> CHR A14, bit 1 -> CHR A13).
pub struct Mapper87 {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_ram: Vec<u8>,
    mirroring: Mirroring,
    s: State,
}

#[derive(Serialize, Deserialize, Default, Clone)]
struct State { chr_bank: u8 }

impl Mapper87 {
    pub fn new(prg_rom: Vec<u8>, chr_rom: Vec<u8>, mirroring: Mirroring) -> Self {
        let chr_ram = if chr_rom.is_empty() { vec![0; 8192] } else { vec![] };
        Self { prg_rom, chr_rom, chr_ram, mirroring, s: State::default() }
    }
}

impl Mapper for Mapper87 {
    fn cpu_read(&self, addr: u16) -> u8 {
        if addr < 0x8000 { return 0; }
        self.prg_rom[(addr as usize & 0x7FFF) % self.prg_rom.len()]
    }
    fn cpu_write(&mut self, addr: u16, value: u8) {
        if (0x6000..=0x7FFF).contains(&addr) {
            self.s.chr_bank = ((value & 1) << 1) | ((value >> 1) & 1);
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
