use crate::nes::mappers::{Mapper};
use std::cell::{UnsafeCell};

pub struct Cartridge {
    pub prg_rom: Vec<u8>,
    pub chr_rom: Vec<u8>,
    pub mapper: UnsafeCell<Box<dyn Mapper>>, // Dynamic trait object
    pub mapper_id: u16,
    pub base_filename: String,
    pub has_battery: bool,
}

impl Cartridge {
    pub fn new(prg_rom: Vec<u8>, chr_rom: Vec<u8>, mapper: Box<dyn Mapper>, base_name:String) -> Cartridge {
        Self {
            prg_rom,
            chr_rom,
            mapper: UnsafeCell::new(mapper),
            mapper_id: 0,
            base_filename: base_name,
            has_battery: false,
        }
    }

    pub fn mapper_number(&self) -> u16 {
        self.mapper_id
    }

    pub fn mapper(&self) -> &dyn Mapper {
        unsafe { &**self.mapper.get() }
    }

    pub fn mapper_mut(&self) -> &mut dyn Mapper {
        unsafe { &mut **self.mapper.get() }
    }

    pub fn get_sram(&self) -> Option<&[u8]> { self.mapper().get_sram() }
    pub fn load_sram(&mut self, data: &[u8]) { self.mapper_mut().load_sram(data); }
    pub fn is_sram_dirty(&self) -> bool { self.mapper().is_sram_dirty() }
    pub fn clear_sram_dirty(&mut self) { self.mapper_mut().clear_sram_dirty(); }
}