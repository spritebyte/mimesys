use crate::nes::mappers::{Mapper, Mirroring, mirror_nt, encode_state, decode_state};
use serde::{Serialize, Deserialize};

// Mapper 68 (Sunsoft-4: After Burner, Maharaja, Nantettatte!! Baseball)
//   $8000/$9000/$A000/$B000  2K CHR banks at $0000/$0800/$1000/$1800
//   $C000/$D000              nametable ROM banks (1K CHR bank = $80 | value)
//   $E000  bits 0-1 mirroring (V, H, one-screen A, one-screen B)
//          bit 4: nametables come from CHR ROM (the two banks above) instead of CIRAM
//   $F000  bits 0-3 16K PRG bank at $8000, bit 4 PRG-RAM enable. $C000 = last bank.
pub struct Mapper68 {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_ram: Vec<u8>,
    s: State,
}

#[derive(Serialize, Deserialize, Clone)]
struct State {
    chr: [u8; 4],
    nt: [u8; 2],
    ctrl: u8,
    prg: u8,
    prg_ram: Vec<u8>,
}

impl Mapper68 {
    pub fn new(prg_rom: Vec<u8>, chr_rom: Vec<u8>, mirroring: Mirroring) -> Self {
        let chr_ram = if chr_rom.is_empty() { vec![0; 8192] } else { vec![] };
        let ctrl = if mirroring == Mirroring::Horizontal { 1 } else { 0 };
        Self { prg_rom, chr_rom, chr_ram,
               s: State { chr: [0; 4], nt: [0; 2], ctrl, prg: 0, prg_ram: vec![0; 8192] } }
    }
    fn mirroring(&self) -> Mirroring {
        match self.s.ctrl & 3 {
            0 => Mirroring::Vertical,
            1 => Mirroring::Horizontal,
            2 => Mirroring::SingleLower,
            _ => Mirroring::SingleUpper,
        }
    }
    fn chr_byte(&self, bank_1k: usize, offset: usize) -> u8 {
        if self.chr_rom.is_empty() { return self.chr_ram[(bank_1k * 0x400 + offset) % self.chr_ram.len()]; }
        self.chr_rom[(bank_1k * 0x400 + offset) % self.chr_rom.len()]
    }
}

impl Mapper for Mapper68 {
    fn cpu_read(&self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => self.s.prg_ram[addr as usize - 0x6000],
            0x8000..=0xBFFF => {
                let banks = (self.prg_rom.len() / 0x4000).max(1);
                self.prg_rom[((self.s.prg & 0x0F) as usize % banks) * 0x4000 + (addr as usize & 0x3FFF)]
            }
            0xC000..=0xFFFF => self.prg_rom[self.prg_rom.len() - 0x4000 + (addr as usize & 0x3FFF)],
            _ => 0,
        }
    }
    fn cpu_write(&mut self, addr: u16, value: u8) {
        match addr & 0xF000 {
            0x6000 | 0x7000 => self.s.prg_ram[addr as usize - 0x6000] = value,
            0x8000 => self.s.chr[0] = value,
            0x9000 => self.s.chr[1] = value,
            0xA000 => self.s.chr[2] = value,
            0xB000 => self.s.chr[3] = value,
            0xC000 => self.s.nt[0] = value,
            0xD000 => self.s.nt[1] = value,
            0xE000 => self.s.ctrl = value,
            0xF000 => self.s.prg = value,
            _ => {}
        }
    }
    fn ppu_read(&self, addr: u16) -> u8 {
        let addr = addr as usize & 0x1FFF;
        let reg = addr / 0x800;
        self.chr_byte(self.s.chr[reg] as usize * 2 + (addr & 0x400) / 0x400, addr & 0x3FF)
    }
    fn ppu_write(&mut self, addr: u16, value: u8) {
        if self.chr_rom.is_empty() && addr < 0x2000 { self.chr_ram[addr as usize] = value; }
    }
    fn mirror_vram_address(&self, addr: u16) -> usize { mirror_nt(self.mirroring(), addr) }
    fn read_nametable_byte(&self, addr: u16, vram: &[u8; 4096], _attr: bool) -> u8 {
        let ciram = self.mirror_vram_address(addr);
        if self.s.ctrl & 0x10 != 0 {
            let page = (ciram >> 10) & 1;               // which CIRAM page it would have been
            return self.chr_byte(0x80 | (self.s.nt[page] & 0x7F) as usize, ciram & 0x3FF);
        }
        vram[ciram]
    }
    fn write_nametable_byte(&mut self, addr: u16, value: u8, vram: &mut [u8; 4096]) {
        if self.s.ctrl & 0x10 == 0 {
            vram[self.mirror_vram_address(addr)] = value;
        }
    }
    fn save_state(&self) -> Vec<u8> { encode_state(&self.s) }
    fn load_state(&mut self, data: &[u8]) { if let Some(s) = decode_state(data) { self.s = s; } }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nametables_from_chr_rom() {
        let mut chr = vec![0u8; 256 * 0x400];
        for b in 0..256 { chr[b * 0x400 + 5] = b as u8; }
        let mut prg = vec![0u8; 8 * 0x4000];
        for b in 0..8 { prg[b * 0x4000] = b as u8; }
        let mut m = Mapper68::new(prg, chr, Mirroring::Vertical);
        let vram = [0xAAu8; 4096];
        m.cpu_write(0xF000, 3);
        assert_eq!((m.cpu_read(0x8000), m.cpu_read(0xC000)), (3, 7));
        m.cpu_write(0xC000, 0x01);
        m.cpu_write(0xD000, 0x02);
        m.cpu_write(0xE000, 0x11);                        // horizontal, NT from ROM
        assert_eq!(m.read_nametable_byte(0x2005, &vram, false), 0x81);
        assert_eq!(m.read_nametable_byte(0x2805, &vram, false), 0x82);
        m.cpu_write(0xE000, 0x01);                        // back to CIRAM
        assert_eq!(m.read_nametable_byte(0x2005, &vram, false), 0xAA);
    }
}
