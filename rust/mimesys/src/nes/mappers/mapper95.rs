use crate::nes::mappers::{Mapper, Mirroring, encode_state, decode_state};
use crate::nes::mapper206::Mapper206;
use serde::{Serialize, Deserialize};

// Mapper 95 (Namcot 3425: Dragon Buster). A Namco 108 (mapper 206) whose CHR A15 line
// (bit 5 of the two 2K CHR registers R0/R1) drives CIRAM A10:
//   nametables $2000/$2400 use R0 bit 5, $2800/$2C00 use R1 bit 5.
pub struct Mapper95 {
    inner: Mapper206,
    s: State,
}

#[derive(Serialize, Deserialize, Default, Clone)]
struct State { select: u8, nt: [u8; 2], inner: Vec<u8> }

impl Mapper95 {
    pub fn new(prg_banks: usize, chr_banks: usize, prg_rom: Vec<u8>, chr_rom: Vec<u8>, mirroring: Mirroring) -> Self {
        Self { inner: Mapper206::new(prg_banks, chr_banks, prg_rom, chr_rom, mirroring, false),
               s: State::default() }
    }
}

impl Mapper for Mapper95 {
    fn cpu_read(&self, addr: u16) -> u8 { self.inner.cpu_read(addr) }
    fn cpu_write(&mut self, addr: u16, value: u8) {
        if (0x8000..=0x9FFF).contains(&addr) {
            if addr & 1 == 0 {
                self.s.select = value & 7;
            } else if self.s.select < 2 {
                self.s.nt[self.s.select as usize] = (value >> 5) & 1;
            }
        }
        self.inner.cpu_write(addr, value & if (0x8000..=0x9FFF).contains(&addr) && addr & 1 == 1 && self.s.select < 6 { 0x1F } else { 0xFF });
    }
    fn ppu_read(&self, addr: u16) -> u8 { self.inner.ppu_read(addr & 0x1FFF) }
    fn ppu_write(&mut self, addr: u16, value: u8) { self.inner.ppu_write(addr, value) }
    fn mirror_vram_address(&self, addr: u16) -> usize {
        let v = addr as usize & 0x0FFF;
        let page = self.s.nt[(v >> 11) & 1] as usize;     // $2000-$27FF -> R0, $2800-$2FFF -> R1
        (page << 10) | (v & 0x3FF)
    }
    fn save_state(&self) -> Vec<u8> {
        let mut s = self.s.clone();
        s.inner = self.inner.save_state();
        encode_state(&s)
    }
    fn load_state(&mut self, data: &[u8]) {
        if let Some(s) = decode_state::<State>(data) {
            self.inner.load_state(&s.inner);
            self.s = s;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chr_bit5_picks_nametable() {
        let mut m = Mapper95::new(4, 32, vec![0; 0x8000], vec![0; 0x8000], Mirroring::Vertical);
        m.cpu_write(0x8000, 0); m.cpu_write(0x8001, 0x20);   // R0 bit 5 = 1
        m.cpu_write(0x8000, 1); m.cpu_write(0x8001, 0x00);   // R1 bit 5 = 0
        assert_eq!(m.mirror_vram_address(0x2000), 0x400);
        assert_eq!(m.mirror_vram_address(0x2400), 0x400);
        assert_eq!(m.mirror_vram_address(0x2800), 0x000);
        assert_eq!(m.mirror_vram_address(0x2C05), 0x005);
    }
}
