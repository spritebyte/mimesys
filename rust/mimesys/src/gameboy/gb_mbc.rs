pub trait Mbc {
    fn read(&self, addr: u16) -> u8;
    fn write(&mut self, addr: u16, val: u8);
    fn get_sram(&self) -> Option<&[u8]> { None }
    fn load_sram(&mut self, _data: &[u8]) {}
    fn is_sram_dirty(&self) -> bool { false }
    fn clear_sram_dirty(&mut self) {}
}

pub fn make_mbc(
    mbc_id: u8,
    prg_rom: Vec<u8>,
    chr_ram: Vec<u8>,
) -> Result<Box<dyn Mbc>, String> {
    match mbc_id {
        0 => {
            println!("Mbc0 (No MBC) created");
            Ok(Box::new(super::gb_mbc0::Mbc0::new(prg_rom, chr_ram)))
        }
        1 => {
            println!("MBC1 created");
            Ok(Box::new(super::gb_mbc1::Mbc1::new(prg_rom, chr_ram)))
        }
        2 => {
            println!("MBC2 created");
            Ok(Box::new(super::gb_mbc2::Mbc2::new(prg_rom, chr_ram)))
        }
        3 => {
            println!("MBC3 created");
            Ok(Box::new(super::gb_mbc3::Mbc3::new(prg_rom, chr_ram)))
        }
        5 => {
            println!("MBC5 created");
            Ok(Box::new(super::gb_mbc5::Mbc5::new(prg_rom, chr_ram)))
        }
        // Add future mappers here. The system file never has to change!
        _ => Err(format!("Mapper {} not implemented yet", mbc_id)),
    }
}
