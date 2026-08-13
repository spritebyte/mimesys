use crate::gameboy::gb_mbc::Mbc;
use std::cell::{UnsafeCell, Cell};

pub struct GbCartridge {
    pub prg_rom: Vec<u8>,
//    pub eeprom_ram: Vec<u8>,
    pub mbc: UnsafeCell<Box<dyn Mbc>>, // Dynamic trait object
    pub mbc_id: u8,
    pub cart_type_code: u8,
    pub supports_gbc: bool,
    pub backwards_compatible: bool,
    pub base_filename: String,
    pub has_battery: bool,
}

pub struct CartType {
    pub mbc_id: u8,
    pub has_battery: bool,
}

impl GbCartridge {
    pub fn new(prg_rom: Vec<u8>, chr_rom: Vec<u8>, mbc: Box<dyn Mbc>, base_name:String) -> GbCartridge {
        Self {
            prg_rom,
            mbc: UnsafeCell::new(mbc),
            mbc_id: 0,
            cart_type_code: 0,
            base_filename: base_name,
            backwards_compatible: false,
            supports_gbc: false,
            has_battery: false,
        }
    }

    pub fn mapper(&self) -> &dyn Mbc {
        unsafe { &**self.mbc.get() }
    }

    pub fn mapper_mut(&self) -> &mut dyn Mbc {
        unsafe { &mut **self.mbc.get() }
    }
    pub fn get_sram(&self) -> Option<&[u8]> { self.mapper().get_sram() }
    pub fn load_sram(&mut self, data: &[u8]) { self.mapper_mut().load_sram(data); }
    pub fn is_sram_dirty(&self) -> bool { self.mapper().is_sram_dirty() }
    pub fn clear_sram_dirty(&mut self) { self.mapper_mut().clear_sram_dirty(); }
}