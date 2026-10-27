// MBC3 / MBC30, with the real-time clock some carts have (Pokémon Gold / Silver / Crystal).
//
//   0000-1FFF  RAM and clock enable (0A)
//   2000-3FFF  ROM bank (7 bits, 0 -> 1)
//   4000-5FFF  RAM bank 0-3, or clock register 08-0C (seconds, minutes, hours, day low,
//              day high: bit 0 day 8, bit 6 halt, bit 7 day counter overflow)
//   6000-7FFF  write 00 then 01: latch the clock into the registers the CPU reads
//
// The clock runs on emulated time (4194304 master clocks a second). The battery file is
// the RAM followed by the common 48-byte clock block (BGB / VBA-M / mGBA): the live and
// latched registers as five u32 each, then the u64 Unix time they were saved at; on load
// the clock catches up on the time that passed while the emulator was closed.

use crate::gameboy::gb_mbc::Mbc;
use serde::{Deserialize, Serialize};

const CLOCKS_PER_SECOND: u64 = 4_194_304;
const RTC_BLOCK: usize = 48;

#[derive(Clone, Copy, Default, PartialEq, Debug, Serialize, Deserialize)]
pub struct Rtc {
    pub s: u8,
    pub m: u8,
    pub h: u8,
    pub dl: u8,
    pub dh: u8,
}

impl Rtc {
    fn halted(&self) -> bool { self.dh & 0x40 != 0 }

    /// One second. Counters past their normal range keep counting up to their bit width
    /// and wrap to 0 without a carry, like the real chip.
    fn tick(&mut self) {
        self.s = (self.s + 1) & 0x3F;
        if self.s != 60 { return; }
        self.s = 0;
        self.m = (self.m + 1) & 0x3F;
        if self.m != 60 { return; }
        self.m = 0;
        self.h = (self.h + 1) & 0x1F;
        if self.h != 24 { return; }
        self.h = 0;
        let days = (((self.dh & 1) as u16) << 8 | self.dl as u16) + 1;
        self.dl = days as u8;
        self.dh = (self.dh & 0xFE) | ((days >> 8) as u8 & 1);
        if days > 0x1FF { self.dh |= 0x80; }   // overflow sticks until the game clears it
    }

    /// Whole seconds at once (catching up after the emulator was closed).
    fn advance(&mut self, mut secs: u64) {
        if self.halted() { return; }
        // whole days first, then tick through the rest
        let days_now = ((self.dh & 1) as u64) << 8 | self.dl as u64;
        let day_secs = secs / 86400;
        if day_secs > 0 && self.s < 60 && self.m < 60 && self.h < 24 {
            let days = days_now + day_secs;
            self.dl = days as u8;
            self.dh = (self.dh & 0xFE) | ((days >> 8) as u8 & 1);
            if days > 0x1FF { self.dh |= 0x80; }
            secs %= 86400;
        }
        for _ in 0..secs { self.tick(); }
    }

    fn to_bytes(self, out: &mut Vec<u8>) {
        for v in [self.s, self.m, self.h, self.dl, self.dh] {
            out.extend_from_slice(&(v as u32).to_le_bytes());
        }
    }

    fn from_bytes(b: &[u8]) -> Self {
        let r = |i: usize| b[i * 4];
        Rtc { s: r(0) & 0x3F, m: r(1) & 0x3F, h: r(2) & 0x1F, dl: r(3), dh: r(4) & 0xC1 }
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

#[derive(Serialize, Deserialize)]
pub struct Mbc3 {
    #[serde(skip)]
    prg_rom: Vec<u8>,
    #[serde(with = "serde_bytes")]
    cart_ram: Vec<u8>,
    rom_bank: u8,
    ram_bank: u8,
    ram_enabled: bool,
    bank_mask: usize,
    sram_dirty: bool,
    has_rtc: bool,
    /// 08-0C selected at A000-BFFF instead of RAM
    rtc_register: Option<u8>,
    rtc: Rtc,
    latched: Rtc,
    latch_prev: u8,
    /// master clocks into the current second
    rtc_cycles: u64,
    /// RAM + clock block, as written to the battery file
    #[serde(with = "serde_bytes")]
    battery: Vec<u8>,
}

impl Mbc3 {
    pub fn new(prg_rom: Vec<u8>, cart_ram: Vec<u8>, has_rtc: bool) -> Self {
        let total_banks: usize = (prg_rom.len() / 16384).max(1);
        let bank_mask: usize = total_banks.next_power_of_two() - 1;
        let mut m = Self {
            prg_rom, cart_ram,
            rom_bank: 1, ram_bank: 0, ram_enabled: false, bank_mask, sram_dirty: false,
            has_rtc, rtc_register: None, rtc: Rtc::default(), latched: Rtc::default(),
            latch_prev: 0xFF, rtc_cycles: 0, battery: Vec::new(),
        };
        m.refresh_battery();
        m
    }

    fn refresh_battery(&mut self) {
        self.battery.clear();
        self.battery.extend_from_slice(&self.cart_ram);
        if self.has_rtc {
            self.rtc.to_bytes(&mut self.battery);
            self.latched.to_bytes(&mut self.battery);
            self.battery.extend_from_slice(&unix_now().to_le_bytes());
        }
    }

    #[cfg(test)]
    pub fn rtc(&self) -> Rtc { self.rtc }
}

impl Mbc for Mbc3 {
    crate::mbc_state!();

    fn read(&self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x3FFF => self.prg_rom.get(addr as usize).copied().unwrap_or(0xFF),
            0x4000..=0x7FFF => {
                let bank = (self.rom_bank as usize) & self.bank_mask;
                self.prg_rom.get(bank * 0x4000 + (addr as usize - 0x4000)).copied().unwrap_or(0xFF)
            }
            0xA000..=0xBFFF if self.ram_enabled => match self.rtc_register {
                Some(r) => {
                    let l = &self.latched;
                    match r {
                        // unused bits read as 0
                        0x08 => l.s,
                        0x09 => l.m,
                        0x0A => l.h,
                        0x0B => l.dl,
                        _ => l.dh,
                    }
                }
                None if !self.cart_ram.is_empty() => {
                    let off = self.ram_bank as usize * 0x2000 + (addr as usize - 0xA000);
                    self.cart_ram[off % self.cart_ram.len()]
                }
                None => 0xFF,
            },
            _ => 0xFF,
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            0x0000..=0x1FFF => self.ram_enabled = (value & 0x0F) == 0x0A,
            0x2000..=0x3FFF => {
                // 7 bits on MBC3; MBC30 (4 MB, Japanese Crystal) has all 8. The ROM size
                // mask keeps a 2 MB cartridge at 7.
                self.rom_bank = value;
                if self.rom_bank == 0 { self.rom_bank = 1; }
            }
            0x4000..=0x5FFF => {
                if value <= 0x07 {   // 0-3 on MBC3, 0-7 on MBC30
                    self.rtc_register = None;
                    self.ram_bank = value;
                } else if self.has_rtc && (0x08..=0x0C).contains(&value) {
                    self.rtc_register = Some(value);
                }
            }
            0x6000..=0x7FFF => {
                if self.latch_prev == 0x00 && value == 0x01 {
                    self.latched = self.rtc;
                }
                self.latch_prev = value;
            }
            0xA000..=0xBFFF if self.ram_enabled => match self.rtc_register {
                Some(r) => {
                    let t = &mut self.rtc;
                    match r {
                        0x08 => { t.s = value & 0x3F; self.rtc_cycles = 0; }
                        0x09 => t.m = value & 0x3F,
                        0x0A => t.h = value & 0x1F,
                        0x0B => t.dl = value,
                        _ => t.dh = value & 0xC1,
                    }
                    // the game set the clock: save it
                    self.sram_dirty = true;
                    self.refresh_battery();
                }
                None if !self.cart_ram.is_empty() => {
                    let len = self.cart_ram.len();
                    let off = (self.ram_bank as usize * 0x2000 + (addr as usize - 0xA000)) % len;
                    self.cart_ram[off] = value;
                    self.battery[off] = value;
                    self.sram_dirty = true;
                }
                None => {}
            },
            _ => {}
        }
    }

    fn clock(&mut self, master_cycles: u64) {
        if !self.has_rtc || self.rtc.halted() { return; }
        self.rtc_cycles += master_cycles;
        if self.rtc_cycles >= CLOCKS_PER_SECOND {
            while self.rtc_cycles >= CLOCKS_PER_SECOND {
                self.rtc_cycles -= CLOCKS_PER_SECOND;
                self.rtc.tick();
            }
            self.refresh_battery();
        }
    }

    fn get_sram(&self) -> Option<&[u8]> {
        if self.battery.is_empty() { None } else { Some(&self.battery) }
    }

    fn load_sram(&mut self, data: &[u8]) {
        let n = self.cart_ram.len();
        if data.len() < n { return; }
        self.cart_ram.copy_from_slice(&data[..n]);
        // the clock block: 48 bytes (64-bit time) or the older 44 (32-bit time)
        let rest = &data[n..];
        if self.has_rtc && rest.len() >= 44 {
            self.rtc = Rtc::from_bytes(&rest[0..20]);
            self.latched = Rtc::from_bytes(&rest[20..40]);
            let saved = if rest.len() >= RTC_BLOCK {
                u64::from_le_bytes(rest[40..48].try_into().unwrap())
            } else {
                u32::from_le_bytes(rest[40..44].try_into().unwrap()) as u64
            };
            let now = unix_now();
            if saved > 0 && now > saved {
                self.rtc.advance(now - saved);
            }
        }
        self.refresh_battery();
    }

    fn is_sram_dirty(&self) -> bool { self.sram_dirty }
    fn clear_sram_dirty(&mut self) { self.sram_dirty = false; }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtc_counts_latches_and_rolls_over() {
        let mut m = Mbc3::new(vec![0; 0x8000], vec![0; 0x2000], true);
        m.write(0x0000, 0x0A);
        // set 23:59:58 on day 511
        for (reg, v) in [(0x08, 58), (0x09, 59), (0x0A, 23), (0x0B, 0xFF), (0x0C, 0x01)] {
            m.write(0x4000, reg);
            m.write(0xA000, v);
        }
        m.write(0x6000, 0);
        m.write(0x6000, 1);
        m.clock(CLOCKS_PER_SECOND * 2);
        assert_eq!(m.rtc(), Rtc { s: 0, m: 0, h: 0, dl: 0, dh: 0x80 }, "day counter overflowed");
        // reads show the latched copy until the next 00 -> 01 latch
        m.write(0x4000, 0x08);
        assert_eq!(m.read(0xA000) & 0x3F, 58);
        m.write(0x6000, 0);
        m.write(0x6000, 1);
        assert_eq!(m.read(0xA000) & 0x3F, 0);
        // halt stops it
        m.write(0x4000, 0x0C);
        m.write(0xA000, 0x40);
        m.clock(CLOCKS_PER_SECOND * 5);
        assert_eq!(m.rtc().s, 0);
    }

    #[test]
    fn battery_file_carries_the_clock_and_catches_up() {
        let mut m = Mbc3::new(vec![0; 0x8000], vec![0; 0x2000], true);
        m.write(0x0000, 0x0A);
        m.write(0x4000, 0x00);
        m.write(0xA123, 0x5A);
        m.write(0x4000, 0x0A);
        m.write(0xA000, 5);   // 05:00:00
        let mut file = m.get_sram().unwrap().to_vec();
        assert_eq!(file.len(), 0x2000 + 48);
        // pretend it was saved 2 days, 1 hour and 3 seconds ago
        let then = unix_now() - (2 * 86400 + 3600 + 3);
        file[0x2000 + 40..].copy_from_slice(&then.to_le_bytes());
        let mut n = Mbc3::new(vec![0; 0x8000], vec![0; 0x2000], true);
        n.load_sram(&file);
        assert_eq!(n.cart_ram[0x123], 0x5A);
        let r = n.rtc();
        assert_eq!((r.dl, r.h), (2, 6));
        assert!((3..=4).contains(&r.s), "seconds {}", r.s);
    }
}
