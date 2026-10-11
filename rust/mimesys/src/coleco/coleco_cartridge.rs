use std::cell::UnsafeCell;

pub struct ColecoCartridge {
    pub prg_rom: Vec<u8>,
    pub base_filename: String,
}

impl ColecoCartridge {
    pub fn new(prg_rom: Vec<u8>, base_name:String) -> ColecoCartridge {
        Self {
            prg_rom,
            base_filename: base_name,
        }
    }

    pub fn get_sram(&self) -> Option<&[u8]> { None }
    pub fn load_sram(&mut self, data: &[u8]) { }
    pub fn is_sram_dirty(&self) -> bool { false }
    pub fn clear_sram_dirty(&mut self) { }
}