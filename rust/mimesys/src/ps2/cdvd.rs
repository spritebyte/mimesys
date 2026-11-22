//! The CDVD drive's registers (1F40_2000), enough for a BIOS with no disc: N commands
//! finish at once (IRQ, I_STAT bit 2); S commands answer the drive version, the clock
//! (the host's, in BCD), the config / NVRAM area (zeros) and the rest with a 0. Reading
//! discs comes in stage 6. From ps2tek's CDVD pages.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

use super::hw::IOP_CDVD;

#[derive(Serialize, Deserialize, Default)]
pub struct Cdvd {
    pub n_cmd: u8,
    n_params: Vec<u8>,
    pub s_cmd: u8,
    s_params: Vec<u8>,
    s_result: VecDeque<u8>,
    /// CDVD I_STAT (the reason for the IRQ)
    pub istat: u8,
    pub sticky: u8,
    /// the config area being read (OpenConfig)
    config_left: u8,
    /// debugging: S commands seen
    #[serde(skip)]
    pub log: Vec<u8>,
}

/// Binary-coded decimal
fn bcd(v: u32) -> u8 { ((v / 10) << 4 | v % 10) as u8 }

/// The host clock as (sec, min, hour, day, month, year % 100), UTC
fn now() -> [u32; 6] {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (sec, min, hour) = ((t % 60) as u32, (t / 60 % 60) as u32, (t / 3600 % 24) as u32);
    // days -> civil date (Howard Hinnant's algorithm)
    let z = (t / 86400) as i64 + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = (yoe + era * 400 + (m <= 2) as i64) as u32;
    [sec, min, hour, d, m, y % 100]
}

impl Cdvd {
    pub fn read(&mut self, a: u32) -> u8 {
        match a & 0xFF {
            0x04 => self.n_cmd,
            0x05 => 0x40,                 // ready, not busy
            0x06 => 0,
            0x08 => self.istat,
            0x0A => 0,                    // tray closed, stopped
            0x0B => self.sticky,
            0x0F => 0,                    // no disc
            0x13 => 0,
            0x16 => self.s_cmd,
            0x17 => if self.s_result.is_empty() { 0x40 } else { 0 },
            0x18 => self.s_result.pop_front().unwrap_or(0),
            _ => 0,
        }
    }

    pub fn write(&mut self, a: u32, v: u8, i_stat: &mut u32) {
        match a & 0xFF {
            0x04 => {
                self.n_cmd = v;
                self.n_params.clear();
                // done at once: "command complete"
                self.istat |= 2;
                *i_stat |= IOP_CDVD;
            }
            0x05 => if self.n_params.len() < 16 { self.n_params.push(v) },
            0x07 => {}
            0x08 => self.istat &= !v,
            0x16 => {
                self.s_cmd = v;
                self.s_command(v);
                self.s_params.clear();
            }
            0x17 => if self.s_params.len() < 16 { self.s_params.push(v) },
            _ => {}
        }
    }

    fn s_command(&mut self, c: u8) {
        if self.log.len() < 64 { self.log.push(c); }
        self.s_result.clear();
        let p = |k: usize, s: &Self| s.s_params.get(k).copied().unwrap_or(0);
        let r: Vec<u8> = match c {
            0x03 => match p(0, self) {
                0x00 => vec![0x03, 0x06, 0x02, 0x00],     // mechacon version (as PCSX2: 00020603)
                _ => vec![0],
            },
            0x05 => { self.sticky = 0; vec![0] }
            0x08 => {
                let [s, m, h, d, mo, y] = now();
                vec![0, bcd(s), bcd(m), bcd(h), 0, bcd(d), bcd(mo), bcd(y)]
            }
            0x12 => vec![0; 9],                            // i.LINK ID
            0x15 => vec![5],                               // forbid DVD
            0x1A => vec![1],                               // boot certify
            0x40 => { self.config_left = p(2, self).max(1); vec![0] }
            0x41 => {
                // config blocks: 16 bytes each
                if self.config_left > 0 { self.config_left -= 1; }
                vec![0; 16]
            }
            0x43 => { self.config_left = 0; vec![0] }
            _ => vec![0],
        };
        self.s_result.extend(r);
    }
}
