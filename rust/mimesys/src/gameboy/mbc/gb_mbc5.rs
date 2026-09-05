// Mbc 5
use crate::gameboy::gb_mbc::Mbc;

pub struct Mbc5 {
    prg_rom: Vec<u8>,
    cart_ram: Vec<u8>,   // MBC5 supports up to 128KB of RAM (16 banks)
    rom_bank_lo: u8,
    rom_bank_hi: u8,
    ram_bank: u8,
    ram_enabled: bool,
    bank_mask: u16,
}

impl Mbc5 {
    pub fn new(prg_rom: Vec<u8>, cart_ram: Vec<u8>) -> Self {
        let total_banks = (prg_rom.len() / 16384).max(1);
        let bank_mask = (total_banks.next_power_of_two() - 1) as u16;

        Self {
            prg_rom,
            cart_ram,
            rom_bank_lo: 1,
            rom_bank_hi: 0,
            ram_bank: 0,
            ram_enabled: false,
            bank_mask,
        }
    }

    fn _get_selected_rom_bank(&self) -> u16 {
        let bank = ((self.rom_bank_hi as u16) << 8) | self.rom_bank_lo as u16;
//        println!("MBC5 DEBUG: using rom bank {:02X}", bank & self.bank_mask);
        bank & self.bank_mask
    }
}

impl Mbc for Mbc5 {
    fn read(&self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x3FFF => {
                if self.prg_rom.is_empty() { return 0xFF; }
                return self.prg_rom[(addr as usize) % self.prg_rom.len()];
            }
            0x4000..=0x7FFF => {
                if self.prg_rom.is_empty() { return 0xFF; }
                let bank = self._get_selected_rom_bank();
                let offset:usize = (bank as usize * 0x4000) + (addr as usize - 0x4000);
                return self.prg_rom[offset % self.prg_rom.len()];
            }
            0xA000..=0xBFFF => {
                if self.ram_enabled && !self.cart_ram.is_empty() {
                    let ram_offset:usize = (self.ram_bank as usize * 0x2000) + (addr as usize - 0xA000);
                    return self.cart_ram[ram_offset % self.cart_ram.len()];
                }
            }
            _=> { return 0xFF; }
        }
        0xFF
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            0x0000..=0x1FFF => { self.ram_enabled = (value & 0x0F) == 0x0A; }
            0x2000..=0x2FFF => { self.rom_bank_lo = value; }
            0x3000..=0x3FFF => { self.rom_bank_hi = value & 0x01; }
            0x4000..=0x5FFF => { self.ram_bank = value & 0x0F; }
            0xA000..=0xBFFF => {
                if self.ram_enabled && !self.cart_ram.is_empty() {
                    let ram_offset:usize = (self.ram_bank as usize * 0x2000) + (addr as usize - 0xA000);
                    let len = self.cart_ram.len();
                    self.cart_ram[ram_offset % len] = value;
                }
            }
            _=> { }
        }
    }
}