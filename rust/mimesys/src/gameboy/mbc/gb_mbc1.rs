// Mbc 1
use crate::gameboy::gb_mbc::Mbc;

pub struct Mbc1 {
    prg_rom: Vec<u8>,
    cart_ram: Vec<u8>,
    bank_reg_1: u8,    // lower 5 bits of ROM bank
    bank_reg_2: u8,    // upper 2 bits (used for ROM or RAM)
    mode: u8,          // 0 = ROM Banking Mode, 1 = RAM Banking Mode
    bank_mask: usize,
    total_banks: usize,
    ram_enabled: bool,
}

impl Mbc1 {
    pub fn new(prg_rom: Vec<u8>, cart_ram: Vec<u8>) -> Self {
        let total_banks:usize = (prg_rom.len() / 16384).max(1) as usize;
        let bank_mask = (total_banks.next_power_of_two() - 1) as usize;

        Self {
            prg_rom,
            cart_ram,
            bank_reg_1: 1,       
            bank_reg_2: 0,
            mode: 0,
            bank_mask,
            total_banks,
            ram_enabled: false,
        }
    }

    fn _get_selected_rom_bank(&self) -> usize {
        let mut bank:usize = self.bank_reg_1 as usize;
        if bank == 0 { bank = 1; }
        bank |= (self.bank_reg_2 as usize) << 5;
        return bank & self.bank_mask;
    }
}

impl Mbc for Mbc1 {
    fn read(&self, addr: u16) -> u8 {
        if addr <= 0x3FFF {
            if self.mode == 1 {
                let bank:usize = (self.bank_reg_2 << 5) as usize & self.bank_mask;
                return self.prg_rom[(bank as usize * 0x4000) + addr as usize];
            }
            return self.prg_rom[addr as usize];
        }
        else if addr >= 0x4000 && addr <= 0x7FFF {
            let bank = self._get_selected_rom_bank();
            let offset = (bank as usize * 0x4000) + (addr as usize - 0x4000);
            return self.prg_rom[offset];
        }
        else if addr >= 0xA000 && addr <= 0xBFFF {
            if self.ram_enabled && self.cart_ram.len() > 0 {
                return self.cart_ram[(addr - 0xA000) as usize];
            }
        }
        0xFF
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            0x0000..=0x1FFF => {
                self.ram_enabled = (value & 0x0F) == 0x0A;
            }
            0x2000..=0x3FFF => {
                self.bank_reg_1 = value & 0x1F;
                if self.bank_reg_1 == 0 { self.bank_reg_1 = 1; }
            }
            0x4000..=0x5FFF => {
                self.bank_reg_2 = value & 0x03;
            }
            0x6000..=0x7FFF => {
                self.mode = value & 0x01;
            }
            0xA000..=0xBFFF => {
                // TODO: finish this part
                if self.ram_enabled && self.cart_ram.len() > 0 {
                    let r_bank = if self.mode == 1 { self.bank_reg_2 } else { 0 };
                    let ram_offset = (r_bank as usize * 0x2000) + (addr as usize - 0xA000);
                    let offset:usize = ram_offset % self.cart_ram.len();
                    if ram_offset < self.cart_ram.len() {
                        self.cart_ram[ram_offset] = value;
                    } else {
                        self.cart_ram[offset] = value;
                    }
                }
            }
            _=> { }
        }
    }
}