// Mbc 2
use crate::gameboy::gb_mbc::Mbc;

pub struct Mbc2 {
    prg_rom: Vec<u8>,
    cart_ram: Vec<u8>, // MBC2 has exactly 512 bytes of internal RAM
    rom_bank: usize,
    ram_enabled: bool,
    bank_mask: usize,
}

impl Mbc2 {
    pub fn new(prg_rom: Vec<u8>, cart_ram: Vec<u8>) -> Self {
        let total_banks:usize = (prg_rom.len() / 16384).max(1);
        let bank_mask:usize = total_banks.next_power_of_two() - 1;
        
        Self {
            prg_rom,
            cart_ram,
            rom_bank: 1,
            ram_enabled: false,
            bank_mask,
        }
    }
}

impl Mbc for Mbc2 {
    fn read(&self, addr: u16) -> u8 {
        if addr <= 0x3FFF {
            return self.prg_rom[addr as usize];
        }
        else if addr >= 0x4000 && addr <= 0x7FFF {
            let bank:usize = self.rom_bank & self.bank_mask;
            let offset:usize = (bank * 0x4000) + (addr as usize - 0x4000);
            return self.prg_rom[offset];
        }
        // MBC2 RAM is mirrored up to $BFFF, but only 512 bytes exist.
        else if addr >= 0xA000 && addr <= 0xBFFF {
            if self.ram_enabled && self.cart_ram.len() > 0 {
                return self.cart_ram[(addr & 0x01FF) as usize] & 0x0F;
            }
        }
        0xFF
    }

    fn write(&mut self, addr: u16, value: u8) {
        if addr >= 0x0000 && addr <= 0x3FFF {
            if (addr & 0x0100) == 0 {
                self.ram_enabled = (value & 0x0F) == 0x0A;
            } else {
                self.rom_bank = (value & 0x0F) as usize;
                if self.rom_bank == 0 { self.rom_bank = 1; }
            }
        }
        else if addr >= 0xA000 && addr <= 0xBFFF {
            if self.ram_enabled && self.cart_ram.len() > 0 {
                self.cart_ram[(addr & 0x01FF) as usize] = value;
            }
        }
    }
}