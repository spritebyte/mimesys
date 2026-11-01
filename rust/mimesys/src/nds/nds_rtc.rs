//! The DS real-time clock (Seiko S-35180 family) on ARM7 register 0x04000138: bit 0 data,
//! bit 1 clock, bit 2 select, bit 4 data direction (1 = the CPU writes). Bits go LSB first,
//! one per clock-low write. The first byte is the command: 0110 in the low nibble, the
//! register in bits 4-6, read in bit 7 (games may also send it bit-reversed).
//!
//! Registers: status 1 (24-hour mode bit 1, power-on flag bit 7 cleared by reading),
//! status 2, date and time (7 BCD bytes: year, month, day, weekday, hour (bit 6 = PM),
//! minute, second), time (the last 3), alarms, clock adjust, a free byte. The time is the
//! host's UTC plus an offset in minutes (option "rtc_utc_offset"); writes to it are ignored.

use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Rtc {
    pub io: u16,
    status1: u8,
    status2: u8,
    free: u8,
    adjust: u8,
    cmd: u8,
    in_byte: u8,
    in_bit: u8,
    in_pos: u8,
    out: [u8; 8],
    out_bit: u8,
    out_pos: u8,
    /// minutes to add to UTC
    pub utc_offset: i32,
    /// fixed time for tests (seconds since 1970), else the host clock
    #[serde(skip)]
    pub fixed_time: Option<i64>,
}

fn bcd(v: u32) -> u8 { ((v / 10) << 4 | v % 10) as u8 }

/// Days since 1970-01-01 to (year, month, day). (Howard Hinnant's algorithm.)
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + if m <= 2 { 1 } else { 0 }, m, d)
}

impl Rtc {
    pub fn new() -> Self { Self { status1: 0x82, ..Default::default() } }

    fn now(&self) -> i64 {
        let t = self.fixed_time.unwrap_or_else(|| {
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
        });
        t + self.utc_offset as i64 * 60
    }

    /// The 7 date/time bytes.
    pub fn date_time(&self) -> [u8; 7] {
        let t = self.now();
        let days = t.div_euclid(86400);
        let secs = t.rem_euclid(86400);
        let (y, m, d) = civil(days);
        let weekday = (days + 4).rem_euclid(7) as u32;           // 1970-01-01 was a Thursday
        let (h, mi, s) = ((secs / 3600) as u32, ((secs / 60) % 60) as u32, (secs % 60) as u32);
        let hour = if self.status1 & 2 != 0 {
            bcd(h) | if h >= 12 { 0x40 } else { 0 }
        } else {
            bcd(h % 12) | if h >= 12 { 0x40 } else { 0 }
        };
        [bcd((y.rem_euclid(100)) as u32), bcd(m), bcd(d), bcd(weekday), hour, bcd(mi), bcd(s)]
    }

    fn command(&mut self, v: u8) {
        // accept the bit-reversed form too
        self.cmd = if v & 0xF0 == 0x60 { v.reverse_bits() } else { v };
        if self.cmd & 0x80 == 0 { return; }
        self.out = [0; 8];
        match self.cmd & 0x70 {
            0x00 => { self.out[0] = self.status1; self.status1 &= 0x0F; }
            0x40 => self.out[0] = self.status2,
            0x20 => { let dt = self.date_time(); self.out[..7].copy_from_slice(&dt) }
            0x60 => { let dt = self.date_time(); self.out[..3].copy_from_slice(&dt[4..]) }
            0x30 => self.out[0] = self.adjust,
            0x70 => self.out[0] = self.free,
            _ => {}
        }
    }

    fn data_byte(&mut self, v: u8) {
        if self.cmd & 0x0F != 0x06 || self.in_pos != 1 { return; }
        match self.cmd & 0x70 {
            0x00 => self.status1 = (self.status1 & 0xF0) | (v & 0x0E),
            0x40 => self.status2 = v,
            0x30 => self.adjust = v,
            0x70 => self.free = v,
            _ => {}                     // setting the clock: ignored
        }
    }

    pub fn write(&mut self, v: u16, mask: u16) {
        let v = (self.io & !mask) | (v & mask);
        if v & 0x04 != 0 {
            if self.io & 0x04 == 0 {
                self.in_byte = 0;
                self.in_bit = 0;
                self.in_pos = 0;
                self.out = [0; 8];
                self.out_bit = 0;
                self.out_pos = 0;
            } else if v & 0x02 == 0 {
                if v & 0x10 != 0 {
                    if v & 1 != 0 { self.in_byte |= 1 << self.in_bit; }
                    self.in_bit += 1;
                    if self.in_bit == 8 {
                        let b = self.in_byte;
                        if self.in_pos == 0 { self.command(b) } else { self.data_byte(b) }
                        self.in_pos = self.in_pos.saturating_add(1);
                        self.in_bit = 0;
                        self.in_byte = 0;
                    }
                } else {
                    let bit = (self.out[self.out_pos as usize] >> self.out_bit) & 1;
                    self.io = (self.io & !1) | bit as u16;
                    self.out_bit += 1;
                    if self.out_bit == 8 {
                        self.out_bit = 0;
                        if self.out_pos < 7 { self.out_pos += 1; }
                    }
                }
            }
        }
        self.io = if v & 0x10 != 0 { v } else { (self.io & 1) | (v & !1) };
    }
}
