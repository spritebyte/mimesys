// Mbc 0 - Standard flat cartridge, no Mbc
use crate::gameboy::gb_mbc::Mbc;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct Mbc0 {
    #[serde(skip)]
    prg_rom: Vec<u8>,
    #[serde(with = "serde_bytes")]
    cart_ram: Vec<u8>,
    sram_dirty: bool,
}

impl Mbc0 {
    pub fn new(prg_rom: Vec<u8>, cart_ram: Vec<u8>) -> Self {
        Self {
            prg_rom,
            cart_ram,
            sram_dirty: false,
        }
    }
}

impl Mbc for Mbc0 {
    crate::mbc_state!();

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
                self.sram_dirty = true;
                self.cart_ram[(addr - 0xA000) as usize] = value;
            }
        }
    }

    fn get_sram(&self) -> Option<&[u8]> { Some(&self.cart_ram) }
    fn load_sram(&mut self, data: &[u8]) {
        if data.len() == self.cart_ram.len() {
            self.cart_ram.copy_from_slice(data);
        }
    }
    fn is_sram_dirty(&self) -> bool { self.sram_dirty }
    fn clear_sram_dirty(&mut self) { self.sram_dirty = false; }
}