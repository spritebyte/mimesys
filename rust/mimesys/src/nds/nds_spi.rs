//! The ARM7's SPI bus (SPICNT 0x040001C0, SPIDATA 0x040001C2): power management chip,
//! firmware flash and touchscreen (TSC2046), picked by SPICNT bits 8-9. SPICNT bit 11
//! keeps the chip selected after the byte; clearing it ends the command.
//!
//! Without a firmware dump, one is made: header plus the user settings (name, language,
//! touchscreen calibration), twice at the end, as the real ones keep them.

use serde::{Deserialize, Serialize};

/// The DS's CRC16 (reflected 0xA001 polynomial).
pub fn crc16(data: &[u8], init: u16) -> u16 {
    let mut crc = init;
    for &b in data {
        crc ^= b as u16;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xA001 } else { crc >> 1 };
        }
    }
    crc
}

pub const FIRMWARE_SIZE: usize = 0x40000;
/// where the user settings live in a 256 KB firmware (two copies, 0x100 apart)
pub const USER_SETTINGS: usize = 0x3FE00;

/// User settings (0x70 bytes + counter + CRC): English, "mimesys", calibration that maps
/// the ADC 1:16 onto screen pixels.
pub fn user_settings() -> Vec<u8> {
    let mut u = vec![0u8; 0x100];
    u[0x00] = 5;                     // version
    u[0x02] = 11;                    // favourite colour
    u[0x03] = 1;                     // birthday month
    u[0x04] = 1;                     // birthday day
    let name: Vec<u16> = "mimesys".encode_utf16().collect();
    for (i, c) in name.iter().enumerate() { u[0x06 + i * 2..0x08 + i * 2].copy_from_slice(&c.to_le_bytes()); }
    u[0x1A] = name.len() as u8;
    // touchscreen calibration: (ADC x1, y1, screen x1, y1, ADC x2, y2, screen x2, y2)
    let (sx1, sy1, sx2, sy2) = (0x20u16, 0x20u16, 0xE0u16, 0xA0u16);
    u[0x58..0x5A].copy_from_slice(&(sx1 << 4).to_le_bytes());
    u[0x5A..0x5C].copy_from_slice(&(sy1 << 4).to_le_bytes());
    u[0x5C] = sx1 as u8;
    u[0x5D] = sy1 as u8;
    u[0x5E..0x60].copy_from_slice(&(sx2 << 4).to_le_bytes());
    u[0x60..0x62].copy_from_slice(&(sy2 << 4).to_le_bytes());
    u[0x62] = sx2 as u8;
    u[0x63] = sy2 as u8;
    u[0x64] = 0x01 | (3 << 4);       // English, backlight max
    u[0x70] = 1;                     // update counter
    let crc = crc16(&u[..0x70], 0xFFFF);
    u[0x72..0x74].copy_from_slice(&crc.to_le_bytes());
    u
}

/// Wifi calibration defaults (the values melonDS generates for a DS Lite).
const BB_INIT: [u8; 0x69] = [
    0x03, 0x17, 0x40, 0x00, 0x1B, 0x6C, 0x48, 0x80, 0x38, 0x00, 0x35, 0x07, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0xB0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC7, 0xBB, 0x01, 0x24, 0x7F,
    0x5A, 0x01, 0x3F, 0x01, 0x3F, 0x36, 0x1D, 0x00, 0x78, 0x35, 0x55, 0x12, 0x34, 0x1C, 0x00, 0x01,
    0x0E, 0x38, 0x03, 0x70, 0xC5, 0x2A, 0x0A, 0x08, 0x04, 0x01, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFE,
    0xFE, 0xFE, 0xFE, 0xFC, 0xFC, 0xFA, 0xFA, 0xFA, 0xFA, 0xFA, 0xF8, 0xF8, 0xF6, 0x00, 0x12, 0x14,
    0x12, 0x41, 0x23, 0x03, 0x04, 0x70, 0x35, 0x0E, 0x2C, 0x2C, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x0E, 0x00, 0x00, 0x12, 0x28, 0x1C,
];
const RF_INIT: [u8; 0x29] = [
    0x31, 0x4C, 0x4F, 0x21, 0x00, 0x10, 0xB0, 0x08, 0xFA, 0x15, 0x26, 0xE6, 0xC1, 0x01, 0x0E, 0x50,
    0x05, 0x00, 0x6D, 0x12, 0x00, 0x00, 0x01, 0xFF, 0x0E, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x06,
    0x06, 0x00, 0x00, 0x00, 0x18, 0x00, 0x02, 0x00, 0x00,
];
const CHAN_DATA: [u8; 0x3C] = [
    0x1E, 0x0C, 0x0C, 0x0C, 0x0C, 0x0C, 0x0C, 0x0E, 0x0E, 0x0E, 0x0E, 0x0E, 0x0E, 0x0E, 0x16,
    0x26, 0x1C, 0x1C, 0x1C, 0x1D, 0x1D, 0x1D, 0x1E, 0x1E, 0x1E, 0x1E, 0x1F, 0x1E, 0x1F, 0x18,
    0x01, 0x4B, 0x4B, 0x4B, 0x4B, 0x4C, 0x4C, 0x4C, 0x4C, 0x4C, 0x4C, 0x4C, 0x4D, 0x4D, 0x4D,
    0x02, 0x6C, 0x71, 0x76, 0x5B, 0x40, 0x45, 0x4A, 0x2F, 0x34, 0x39, 0x3E, 0x03, 0x08, 0x14,
];
/// the three wifi access point blocks (256 bytes each) sit just below the user settings
pub const ACCESS_POINTS: usize = USER_SETTINGS - 0x400;

/// A 256 KB firmware image with the parts games check: header and wifi calibration (with
/// its CRC at 0x2A), three unconfigured access points (CRC at +0xFE each) and two copies of
/// the user settings.
pub fn make_firmware() -> Vec<u8> {
    let mut f = vec![0xFFu8; FIRMWARE_SIZE];
    f[..0x1D].fill(0);
    f[0x08..0x0C].copy_from_slice(b"MACP");                 // marks a generated image
    f[0x1D] = 0x20;                                         // console type: DS Lite
    f[0x1E..0x20].fill(0);
    f[0x20..0x22].copy_from_slice(&((USER_SETTINGS / 8) as u16).to_le_bytes());
    f[0x22..0x2A].fill(0);
    f[0x2C..0x2E].copy_from_slice(&0x138u16.to_le_bytes()); // wifi config length
    f[0x2E] = 0;
    f[0x2F] = 3;                                            // wifi version W006
    f[0x30..0x36].copy_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]);
    f[0x36..0x3C].copy_from_slice(&[0x00, 0x09, 0xBF, 0x12, 0x34, 0x56]);   // MAC address
    f[0x3C..0x3E].copy_from_slice(&0x3FFEu16.to_le_bytes());                // channels 1-13
    f[0x3E..0x40].fill(0xFF);
    f[0x40] = 3;                                            // RF chip type 3
    f[0x41] = 0x94;
    f[0x42] = 0x29;
    f[0x43] = 0x02;
    let init: [u16; 16] = [0x0002, 0x0017, 0x0026, 0x1818, 0x0048, 0x4840, 0x0058, 0x0042,
                           0x0146, 0x8064, 0xE6E6, 0x2443, 0x000E, 0x0001, 0x0001, 0x0402];
    for (i, v) in init.iter().enumerate() { f[0x44 + i * 2..0x46 + i * 2].copy_from_slice(&v.to_le_bytes()); }
    f[0x64..0x64 + BB_INIT.len()].copy_from_slice(&BB_INIT);
    f[0xCD] = 0;
    f[0xCE..0xCE + RF_INIT.len()].copy_from_slice(&RF_INIT);
    f[0xF7] = 0x02;
    f[0xF8..0xF8 + CHAN_DATA.len()].copy_from_slice(&CHAN_DATA);
    let crc = crc16(&f[0x2C..0x2C + 0x138], 0);
    f[0x2A..0x2C].copy_from_slice(&crc.to_le_bytes());
    f[0x2FF] = 0x80;
    for k in 0..3 {
        let ap = &mut f[ACCESS_POINTS + k * 0x100..ACCESS_POINTS + (k + 1) * 0x100];
        ap.fill(0);
        ap[0xE7] = 0xFF;                                    // not configured
        ap[0xEF] = 0x01;
        let crc = crc16(&ap[..0xFE], 0);
        ap[0xFE..0x100].copy_from_slice(&crc.to_le_bytes());
    }
    let u = user_settings();
    f[USER_SETTINGS..USER_SETTINGS + 0x100].copy_from_slice(&u);
    f[USER_SETTINGS + 0x100..USER_SETTINGS + 0x200].copy_from_slice(&u);
    f
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Spi {
    pub cnt: u16,
    pub data: u8,
    #[serde(skip)]
    pub firmware: Vec<u8>,
    // firmware command state
    fw_cmd: u8,
    fw_pos: u32,
    fw_addr: u32,
    /// status register: bit 1 = write enabled
    #[serde(default)]
    fw_status: u8,
    // power management
    pm_index: u8,
    pm_pos: u8,
    pub pm_regs: [u8; 8],
    // touchscreen: the 12-bit sample being shifted out
    tsc_out: u16,
    /// stylus position (pixels on the bottom screen) and contact
    pub touch: (u16, u16, bool),
}

impl Spi {
    pub fn new(firmware: Vec<u8>) -> Self {
        Self {
            cnt: 0, data: 0, firmware, fw_cmd: 0, fw_pos: 0, fw_addr: 0, fw_status: 0,
            pm_index: 0, pm_pos: 0, pm_regs: [0x0D, 0x00, 0x01, 0x00, 0x03, 0, 0, 0], tsc_out: 0,
            touch: (0, 0, false),
        }
    }

    pub fn write_cnt(&mut self, v: u16, mask: u16) {
        // clearing the hold bit releases the chip after the next byte, not now
        let was_dev = (self.cnt >> 8) & 3;
        self.cnt = (self.cnt & !mask) | (v & mask & 0xCF03);
        if self.cnt & 0x8000 == 0 || (self.cnt >> 8) & 3 != was_dev { self.deselect(); }
    }

    fn deselect(&mut self) {
        // a finished write or erase drops write-enable, as the flash chip does
        if self.fw_pos > 0 && matches!(self.fw_cmd, 0x02 | 0x0A | 0xD8 | 0xDB) { self.fw_status &= !2; }
        self.fw_pos = 0;
        self.pm_pos = 0;
    }

    /// SPIDATA write: one byte out, one back. Returns true to raise the SPI IRQ.
    pub fn write_data(&mut self, v: u8) -> bool {
        if self.cnt & 0x8000 == 0 { return false; }
        let out = match (self.cnt >> 8) & 3 {
            0 => self.power(v),
            1 => self.firmware_byte(v),
            2 => self.touchscreen(v),
            _ => 0,
        };
        self.data = out;
        if self.cnt & 0x0800 == 0 { self.deselect(); }
        self.cnt & 0x4000 != 0
    }

    fn power(&mut self, v: u8) -> u8 {
        let out;
        if self.pm_pos == 0 {
            self.pm_index = v;
            out = 0;
        } else {
            let reg = (self.pm_index & 7) as usize;
            if self.pm_index & 0x80 != 0 { out = self.pm_regs[reg]; } else { self.pm_regs[reg] = v; out = 0; }
        }
        self.pm_pos = self.pm_pos.saturating_add(1);
        out
    }

    fn firmware_byte(&mut self, v: u8) -> u8 {
        let mut out = 0xFF;
        if self.fw_pos == 0 {
            self.fw_cmd = v;
            self.fw_addr = 0;
            match v {
                0x06 => self.fw_status |= 2,                        // write enable
                0x04 => self.fw_status &= !2,                       // write disable
                _ => {}
            }
        } else {
            match self.fw_cmd {
                0x03 | 0x0B => {
                    let fast = self.fw_cmd == 0x0B;
                    if self.fw_pos <= 3 {
                        self.fw_addr = (self.fw_addr << 8) | v as u32;
                    } else if !(fast && self.fw_pos == 4) {
                        let len = self.firmware.len().max(1) as u32;
                        out = self.firmware.get((self.fw_addr % len) as usize).copied().unwrap_or(0xFF);
                        self.fw_addr = self.fw_addr.wrapping_add(1);
                    }
                }
                0x05 => out = self.fw_status,                         // status: never busy
                // page write (erase + program), page program (bits can only clear), page /
                // sector erase. Writes last until power-off; the image isn't saved to disk.
                0x0A | 0x02 => {
                    if self.fw_pos <= 3 {
                        self.fw_addr = (self.fw_addr << 8) | v as u32;
                    } else {
                        let len = self.firmware.len().max(1) as u32;
                        let i = (self.fw_addr % len) as usize;
                        if let Some(b) = self.firmware.get_mut(i) { *b = if self.fw_cmd == 0x0A { v } else { *b & v }; }
                        // page writes wrap within the 256-byte page
                        self.fw_addr = (self.fw_addr & !0xFF) | ((self.fw_addr + 1) & 0xFF);
                    }
                }
                0xD8 | 0xDB => {
                    if self.fw_pos <= 3 { self.fw_addr = (self.fw_addr << 8) | v as u32; }
                    if self.fw_pos == 3 {
                        let size = if self.fw_cmd == 0xD8 { 0x10000 } else { 0x100 };
                        let start = (self.fw_addr as usize & !(size - 1)) % self.firmware.len().max(1);
                        let end = (start + size).min(self.firmware.len());
                        self.firmware[start..end].fill(0xFF);
                    }
                }
                0x9F => out = [0x20, 0x40, 0x12][(self.fw_pos as usize - 1).min(2)],
                _ => {}
            }
        }
        self.fw_pos += 1;
        out
    }

    /// TSC2046: a control byte picks the channel; the 12-bit sample comes out over the
    /// next two bytes (bits 11-5, then bits 4-0 in the top of the second).
    fn touchscreen(&mut self, v: u8) -> u8 {
        let out = (self.tsc_out >> 8) as u8;
        self.tsc_out <<= 8;
        if v & 0x80 != 0 {
            let (x, y, down) = self.touch;
            let sample = match (v >> 4) & 7 {
                1 => if down { (y.min(191) << 4) + 8 } else { 0xFFF },
                5 => if down { (x.min(255) << 4) + 8 } else { 0 },
                _ => 0,
            };
            self.tsc_out = (sample & 0xFFF) << 3;
        }
        out
    }
}
