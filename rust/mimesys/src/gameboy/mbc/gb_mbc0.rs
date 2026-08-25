// Mbc 0 - Standard flat cartridge, no Mbc
use crate::gameboy::gb_mbc::Mbc;

pub struct Mbc0 {
    prg_rom: Vec<u8>,
    cart_ram: Vec<u8>,
}

impl Mbc0 {
    pub fn new(prg_rom: Vec<u8>, cart_ram: Vec<u8>) -> Self {
        Self {
            prg_rom,
            cart_ram,
        }
    }
}

impl Mbc for Mbc0 {
    fn read(&self, addr: u16) -> u8 {
        if addr < 0x8000 {
            return self.prg_rom[addr as usize];
        }
        else if addr >= 0xA000 && addr <= 0xBFFF {
            if self.cart_ram.len() > 0 {
                return self.cart_ram[(addr - 0xA000) as usize];
            }
        }
        0xFF
    }

    fn write(&mut self, addr: u16, value: u8) {
        if addr >= 0xA000 && addr <= 0xBFFF {
            if self.cart_ram.len() > 0 {
                self.cart_ram[(addr - 0xA000) as usize] = value;
            }
        }
    }
}