pub trait Mbc {
    fn read(&self, addr: u16) -> u8;
    fn write(&mut self, addr: u16, val: u8);
    fn get_sram(&self) -> Option<&[u8]> { None }
    fn load_sram(&mut self, _data: &[u8]) {}
    fn is_sram_dirty(&self) -> bool { false }
    fn clear_sram_dirty(&mut self) {}
    /// Everything but the ROM, for save states.
    fn save_state(&self) -> Vec<u8>;
    fn load_state(&mut self, data: &[u8]) -> Result<(), String>;
    /// Emulated time passing (master clocks, 4.19 MHz), for cartridge clocks.
    fn clock(&mut self, _master_cycles: u64) {}
}

/// save_state / load_state for a mapper struct whose ROM field is `prg_rom` (not saved).
#[macro_export]
macro_rules! mbc_state {
    () => {
        fn save_state(&self) -> Vec<u8> {
            bincode::serde::encode_to_vec(self, bincode::config::standard()).unwrap_or_default()
        }
        fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
            let (mut s, _): (Self, usize) = bincode::serde::decode_from_slice(data, bincode::config::standard())
                .map_err(|e| format!("mapper state: {e:?}"))?;
            s.prg_rom = std::mem::take(&mut self.prg_rom);
            *self = s;
            Ok(())
        }
    };
}

pub fn make_mbc(
    mbc_id: u8,
    prg_rom: Vec<u8>,
    chr_ram: Vec<u8>,
    has_rtc: bool,
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
            // MBC2 has 512 half-bytes of RAM built in (the header says none)
            Ok(Box::new(super::gb_mbc2::Mbc2::new(prg_rom, vec![0; 0x200])))
        }
        3 => {
            println!("MBC3 created");
            Ok(Box::new(super::gb_mbc3::Mbc3::new(prg_rom, chr_ram, has_rtc)))
        }
        5 => {
            println!("MBC5 created");
            Ok(Box::new(super::gb_mbc5::Mbc5::new(prg_rom, chr_ram)))
        }
        // Add future mappers here. The system file never has to change!
        _ => Err(format!("MBC {} not implemented yet", mbc_id)),
    }
}
