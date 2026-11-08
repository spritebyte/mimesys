//! Mapper 75: Konami VRC1 (VS Goonies, VS Gradius, a few Famicom games; old VS dumps
//! call it 151). Three 8 KB PRG banks + the last one fixed, two 4 KB CHR banks (5 bits,
//! the top bit in $9000), mirroring in $9000 bit 0.

use crate::nes::mappers::{decode_state, encode_state, mirror_nt, Mapper, Mirroring};
use serde::{Deserialize, Serialize};

pub struct Mapper75 {
    prg: Vec<u8>,
    chr: Vec<u8>,
    chr_ram: bool,
    four_screen: bool,
    st: State,
}

#[derive(Serialize, Deserialize)]
struct State { prg: [u8; 3], chr: [u8; 2], horizontal: bool }

impl Mapper75 {
    pub fn new(prg: Vec<u8>, chr: Vec<u8>, mirroring: Mirroring, four_screen: bool) -> Self {
        let chr_ram = chr.is_empty();
        let chr = if chr_ram { vec![0; 0x2000] } else { chr };
        Self { prg, chr, chr_ram, four_screen,
               st: State { prg: [0, 1, 2], chr: [0, 1], horizontal: mirroring == Mirroring::Horizontal } }
    }
    fn prg_banks(&self) -> usize { (self.prg.len() / 0x2000).max(1) }
}

impl Mapper for Mapper75 {
    fn cpu_read(&self, addr: u16) -> u8 {
        if addr < 0x8000 { return 0; }
        let slot = (addr as usize - 0x8000) >> 13;
        let bank = if slot < 3 { self.st.prg[slot] as usize } else { self.prg_banks() - 1 };
        self.prg[(bank % self.prg_banks()) * 0x2000 + (addr as usize & 0x1FFF)]
    }
    fn cpu_write(&mut self, addr: u16, value: u8) {
        let s = &mut self.st;
        match addr & 0xF000 {
            0x8000 => s.prg[0] = value & 0x0F,
            0x9000 => {
                s.horizontal = value & 1 != 0;
                s.chr[0] = s.chr[0] & 0x0F | (value << 3 & 0x10);
                s.chr[1] = s.chr[1] & 0x0F | (value << 2 & 0x10);
            }
            0xA000 => s.prg[1] = value & 0x0F,
            0xC000 => s.prg[2] = value & 0x0F,
            0xE000 => s.chr[0] = s.chr[0] & 0x10 | value & 0x0F,
            0xF000 => s.chr[1] = s.chr[1] & 0x10 | value & 0x0F,
            _ => {}
        }
    }
    fn ppu_read(&self, addr: u16) -> u8 {
        let bank = self.st.chr[(addr as usize >> 12) & 1] as usize;
        self.chr[(bank * 0x1000 + (addr as usize & 0x0FFF)) % self.chr.len()]
    }
    fn ppu_write(&mut self, addr: u16, value: u8) {
        if self.chr_ram { self.chr[addr as usize & 0x1FFF] = value; }
    }
    fn mirror_vram_address(&self, addr: u16) -> usize {
        let m = if self.four_screen { Mirroring::FourScreen }
            else if self.st.horizontal { Mirroring::Horizontal } else { Mirroring::Vertical };
        mirror_nt(m, addr)
    }
    fn save_state(&self) -> Vec<u8> { encode_state(&self.st) }
    fn load_state(&mut self, data: &[u8]) { if let Some(s) = decode_state(data) { self.st = s; } }
}
