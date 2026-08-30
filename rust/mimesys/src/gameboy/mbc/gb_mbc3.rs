// Mbc 3
use crate::gameboy::gb_mbc::Mbc;
use std::time::SystemTime;

pub struct Mbc3 {
    prg_rom: Vec<u8>,
    cart_ram: Vec<u8>,
    rom_bank: u8,
    ram_bank: u8,
    ram_enabled: bool,
    bank_mask: usize,
    // RTC registers and variables
    rtc_register: u8,
    rtc_seconds: u8,
    rtc_minutes: u8,
    rtc_hours: u8,
    rtc_days: u8,
    rtc_halt: bool,
    rtc_day_carry: bool,
    last_system_time: u64,
    rtc_latch_value: u8,
    rtc_selected: bool,
}

impl Mbc3 {
    pub fn new(prg_rom: Vec<u8>, cart_ram: Vec<u8>) -> Self {
        let total_banks:usize = (prg_rom.len() / 16384).max(1);
        let bank_mask:usize = total_banks.next_power_of_two() - 1;

        Self {
            prg_rom,
            cart_ram,
            rom_bank: 1,
            ram_bank: 0,
            ram_enabled: false,
            rtc_register: 0,
            rtc_seconds: 0, rtc_minutes: 0, rtc_hours: 0, rtc_days: 0,
            rtc_halt: false, rtc_day_carry: false, last_system_time: 0,
            rtc_latch_value: 0, rtc_selected: false,
            bank_mask,
        }
    }
}

impl Mbc for Mbc3 {
    fn read(&self, addr: u16) -> u8 {
        if addr <= 0x3FFF {
            return self.prg_rom[addr as usize]
        }
        else if addr >= 0x4000 && addr <= 0x7FFF {
            let bank = (self.rom_bank as usize) & self.bank_mask;
            let bank_offset:usize = bank as usize * 0x4000;
            return self.prg_rom[bank_offset + (addr as usize - 0x4000)];
        }
        else if addr >= 0xA000 && addr <= 0xBFFF {
            return self.cart_ram[(addr - 0xA000) as usize];
        }
        0xFF
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            0x0000..=0x1FFF => {
                self.ram_enabled = (value & 0x0F) == 0x0A;
            },
            0x2000..=0x3FFF => {
                self.rom_bank = value & 0x7F;
                if self.rom_bank == 0 { self.rom_bank = 1; }
            },
            0x4000..=0x5FFF => {
                if value <= 0x03 {
                    self.rtc_selected = false;
                    self.ram_bank = value & 0x03;
                } else 
                if value >= 0x08 && value <= 0x0C {
                    self.rtc_selected = true;
                    self.rtc_register = value;
                }
            },
            0xA000..=0xBFFF => {
                self.cart_ram[(addr - 0xA000) as usize] = value;
            },
            _=> { },
        }
    }
}