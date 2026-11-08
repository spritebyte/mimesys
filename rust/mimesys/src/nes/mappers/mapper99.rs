//! Mapper 99: the VS System's own banking. Bit 2 of the $4016 write picks the 8 KB CHR
//! bank (and, on the one 40 KB-PRG game, Gumshoe, the 8 KB PRG bank at $8000).
//! $6000-$7FFF: the VS board's 2 KB of RAM, mirrored. Mirroring is fixed four-screen on
//! the VS (two nametables on the board plus two more), but games use two, as the header says.

use crate::nes::mappers::{decode_state, encode_state, mirror_nt, Mapper, Mirroring};
use serde::{Deserialize, Serialize};

pub struct Mapper99 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_ram: bool,
    st: State,
    mirroring: Mirroring,
}

#[derive(Serialize, Deserialize)]
struct State { bank: u8, ram: Vec<u8> }

impl Mapper99 {
    pub fn new(prg: Vec<u8>, chr: Vec<u8>, mirroring: Mirroring, four_screen: bool) -> Self {
        let chr_ram = chr.is_empty();
        let chr = if chr_ram { vec![0; 0x2000] } else { chr };
        Self { prg, chr, chr_ram, st: State { bank: 0, ram: vec![0; 0x800] },
               mirroring: if four_screen { Mirroring::FourScreen } else { mirroring } }
    }
}

impl Mapper for Mapper99 {
    fn cpu_read(&self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => self.st.ram[addr as usize & 0x7FF],
            0x8000..=0xFFFF => {
                let a = addr as usize - 0x8000;
                let i = if self.prg.len() > 0x8000 && a < 0x2000 {
                    // 40 KB: $8000 is bank 0 or bank 4 (the extra 8 KB)
                    if self.st.bank != 0 { 0x8000 + a } else { a }
                } else {
                    a
                };
                self.prg[i % self.prg.len()]
            }
            _ => 0,
        }
    }
    fn cpu_write(&mut self, addr: u16, value: u8) {
        if let 0x6000..=0x7FFF = addr { self.st.ram[addr as usize & 0x7FF] = value; }
    }
    fn notify_4016(&mut self, value: u8) { self.st.bank = value >> 2 & 1; }
    fn ppu_read(&self, addr: u16) -> u8 {
        let i = (self.st.bank as usize * 0x2000 + (addr as usize & 0x1FFF)) % self.chr.len();
        self.chr[i]
    }
    fn ppu_write(&mut self, addr: u16, value: u8) {
        if self.chr_ram { self.chr[addr as usize & 0x1FFF] = value; }
    }
    fn mirror_vram_address(&self, addr: u16) -> usize { mirror_nt(self.mirroring, addr) }
    fn save_state(&self) -> Vec<u8> { encode_state(&self.st) }
    fn load_state(&mut self, data: &[u8]) { if let Some(s) = decode_state(data) { self.st = s; } }
}
