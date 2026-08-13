// Mbc 0 - Standard flat cartridge, no Mbc
use crate::gameboy::gb_mbc::Mbc;

pub struct Mbc0 {
    prg_rom: Vec<u8>,
    prg_ram: Vec<u8>,
}

impl Mbc0 {
    pub fn new(prg_rom: Vec<u8>, cart_ram: Vec<u8>) -> Self {
        Self {
            prg_rom,
            prg_ram: cart_ram,
        }
    }
}

impl Mbc for Mbc0 {
    fn read_rom(&self, addr: u16) -> u8 {
        if addr < 0x8000 {
            if addr >= 0x6000 {
                return self.prg_ram[(addr - 0x6000) as usize];
            }
            return 0;
        }
        let mut rom_addr = addr - 0x8000;
        if self.prg_rom.len() == 16384 {
            rom_addr %= 16384; // Mirroring for 16KB games
        }
        self.prg_rom[rom_addr as usize]
    }

    fn write_rom(&mut self, addr: u16, value: u8) {
        // handle chr_ram writes or modifications if needed
    }

    fn write_ram(&mut self, addr: u16, value: u8) {
        if addr < 0x8000 {
            if addr >= 0x6000 {
                self.prg_ram[(addr - 0x6000) as usize] = value;
            }
            return;
        }
    }

    fn read_ram(&self, addr: u16) -> u8 {
        self.prg_ram[(addr - 0x6000) as usize]
    }
}