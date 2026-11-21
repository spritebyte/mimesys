//! The SMPC (System Manager and Peripheral Control): the Saturn's power and reset
//! switchboard, real-time clock, 4 bytes of battery memory and the pad reader. Ported from
//! MAME's sega/smpc.cpp (Angelo Salese, R. Belmont; BSD-3-Clause), simplified.
//!
//! - Registers (odd bytes at 0x00100000): IREG0-6 0x01-0x0D, COMREG 0x1F, OREG0-31
//!   0x21-0x5F, SR 0x61, SF 0x63, PDR1/2 0x75/0x77, DDR1/2 0x79/0x7B, IOSEL 0x7D, EXLE 0x7F.
//! - A command: the CPU sets SF, writes COMREG, waits for SF to clear. OREG31 echoes it.
//! - INTBACK (10h): IREG0 != 0 asks for the status (RTC, region, SMEM ...), IREG1 bit 3 the
//!   pads. Each part ends with the SMPC interrupt; the CPU asks for the next with IREG0
//!   bit 7 (continue) or stops it with bit 6 (break).
//! - Standard pad: 2 bytes, active low: R L D U Start A C B, then R X Y Z L 1 1 1.

use serde::{Deserialize, Serialize};

/// What the SMPC asks of the rest of the machine (drained by the system)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    /// the slave SH-2: true = running (SSHON), false = held in reset (SSHOFF)
    Slave(bool),
    /// the 68000: true = running (SNDON)
    Sound(bool),
    /// SYSRES: reset everything but the master SH-2 ... and the master too
    SysRes,
    /// CKCHG352 (true) / CKCHG320 (false): change the dot clock, then NMI the master
    Clock(bool),
    MasterNmi,
    /// the SMPC interrupt (SCU IST bit 7)
    Irq,
}

/// Microseconds in picoseconds
const US: i64 = 1_000_000;

/// Command times in microseconds (MAME's table)
fn command_us(c: u8) -> i64 {
    match c {
        0x00..=0x03 | 0x06 | 0x07 | 0x0A..=0x0C => 30,
        0x04 | 0x05 => 10,
        0x08 | 0x09 => 40,
        0x0D..=0x0F => 100_000,
        _ => 30,
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Smpc {
    pub ireg: [u8; 7],
    pub oreg: [u8; 32],
    comreg: u8,
    pub sr: u8,
    pub sf: bool,
    cd_sf: bool,
    pdr: [u8; 2],
    ddr: [u8; 2],
    iosel: u8,
    exle: u8,
    busy: bool,
    /// picoseconds until the command in COMREG completes
    cmd_left: i64,
    intback_buf: [u8; 3],
    intback_stage: u8,
    intback_left: i64,
    pmode: u8,
    /// battery memory (SETSMEM) and its "set" flag (smem[4] bit 7)
    pub smem: [u8; 5],
    /// BCD: century, year, weekday << 4 | month, day, hour, minute, second
    pub rtc: [u8; 7],
    rtc_ps: i64,
    nmi_reset: bool,
    pub slave_on: bool,
    pub sound_on: bool,
    cd_on: bool,
    /// 352-dot mode (CKCHG352)
    pub dot352: bool,
    pub region: u8,
    #[serde(skip)]
    pub actions: Vec<Action>,
    /// the pads (active low, as sent) and whether they're plugged in; the frontend's
    #[serde(skip)]
    pub pads: [u16; 2],
    #[serde(skip, default = "plugged")]
    pub connected: [bool; 2],
    /// the last commands, for the debug log
    #[serde(skip)]
    pub log: Vec<u8>,
}

fn plugged() -> [bool; 2] { [true, true] }

impl Smpc {
    pub fn new(region: u8) -> Self {
        Self {
            ireg: [0; 7], oreg: [0; 32], comreg: 0xFF, sr: 0x40, sf: false, cd_sf: false,
            pdr: [0; 2], ddr: [0; 2], iosel: 0, exle: 0, busy: false, cmd_left: 0,
            intback_buf: [0; 3], intback_stage: 0, intback_left: 0, pmode: 0,
            smem: [0; 5], rtc: host_time(), rtc_ps: 0, nmi_reset: false,
            slave_on: false, sound_on: false, cd_on: true, dot352: false, region,
            actions: Vec::new(), pads: [0xFFFF; 2], connected: plugged(), log: Vec::new(),
        }
    }

    /// A byte at 0x00100000 + `r` (r < 0x80; even bytes read FFh)
    pub fn read(&mut self, r: u32) -> u8 {
        if r & 1 == 0 { return 0xFF; }
        match r {
            0x21..=0x5F => self.oreg[(r as usize - 0x21) >> 1],
            0x61 => self.sr,
            0x63 => self.sf as u8 | (self.cd_sf as u8) << 3,
            0x75 => 0x7F & !self.ddr[0] | self.pdr[0] | 0x80 & self.pdr[0],
            0x77 => 0x7F & !self.ddr[1] | self.pdr[1] | 0x80 & self.pdr[1],
            _ => 0xFF,
        }
    }

    pub fn write(&mut self, r: u32, v: u8) {
        if r & 1 == 0 { return; }
        match r {
            0x01..=0x0D => {
                self.ireg[(r as usize - 1) >> 1] = v;
                if r == 1 && self.intback_stage != 0 {
                    if v & 0x40 != 0 {
                        // break
                        self.sr &= 0x0F;
                        self.intback_stage = 0;
                    } else if v & 0x80 != 0 {
                        // continue: the next pad packet
                        self.intback_left = 700 * US;
                        self.oreg[31] = 0x10;
                        self.sf = true;
                    }
                }
            }
            0x1F => self.command(v),
            0x63 => { self.sf = v & 1 != 0; self.cd_sf = false; }
            0x75 => self.pdr[0] = v & self.ddr[0] & 0x7F | v & 0x80,
            0x77 => self.pdr[1] = v & self.ddr[1] & 0x7F | v & 0x80,
            0x79 => self.ddr[0] = v & 0x7F,
            0x7B => self.ddr[1] = v & 0x7F,
            0x7D => self.iosel = v & 3,
            0x7F => self.exle = v & 3,
            _ => {}
        }
    }

    fn command(&mut self, v: u8) {
        if self.busy { return; }
        self.comreg = v & 0x1F;
        self.busy = true;
        if self.log.len() < 64 { self.log.push(self.comreg); }
        let c = self.comreg;
        self.cmd_left = match c {
            0x02 | 0x03 if (!self.slave_on) == (c & 1 != 0) => US,
            0x06 | 0x07 if (!self.sound_on) == (c & 1 != 0) => US,
            0x0E | 0x0F => 100 * US,
            0x10 => {
                self.intback_buf.copy_from_slice(&self.ireg[..3]);
                let mut t = 8;
                if self.ireg[0] != 0 { t += 8; }
                if self.ireg[1] & 8 != 0 { t += 700; }
                t * US
            }
            _ => command_us(c) * US,
        };
    }

    /// Time passes (picoseconds)
    pub fn tick(&mut self, ps: i64) {
        if self.busy {
            self.cmd_left -= ps;
            if self.cmd_left <= 0 { self.finish(); }
        }
        if self.intback_left > 0 {
            self.intback_left -= ps;
            if self.intback_left <= 0 { self.intback_left = 0; self.pad_packet(); }
        }
        self.rtc_ps += ps;
        while self.rtc_ps >= 1_000_000 * US {
            self.rtc_ps -= 1_000_000 * US;
            rtc_second(&mut self.rtc);
        }
    }

    fn done(&mut self) {
        self.busy = false;
        self.oreg[31] = self.comreg;
        self.sf = false;
    }

    fn finish(&mut self) {
        let c = self.comreg;
        match c {
            0x00 | 0x01 => {}
            0x02 | 0x03 => { self.slave_on = c == 0x02; self.actions.push(Action::Slave(self.slave_on)); }
            0x06 | 0x07 => { self.sound_on = c == 0x06; self.actions.push(Action::Sound(self.sound_on)); }
            0x08 | 0x09 => {
                self.cd_on = c == 0x08;
                self.done();
                self.cd_sf = true;
                return;
            }
            0x0A | 0x0B => {}
            0x0D => self.actions.push(Action::SysRes),
            0x0E | 0x0F => {
                // the clock change also stops the slave and the 68000
                self.dot352 = c == 0x0E;
                self.slave_on = false;
                self.sound_on = false;
                self.actions.push(Action::Clock(self.dot352));
                self.actions.push(Action::MasterNmi);
            }
            0x10 => { self.intback(); return; }
            0x16 => self.rtc.copy_from_slice(&self.ireg),
            0x17 => {
                self.smem[..4].copy_from_slice(&self.ireg[..4]);
                self.smem[4] = 0xFF;
            }
            0x18 => self.actions.push(Action::MasterNmi),
            0x19 | 0x1A => self.nmi_reset = c == 0x1A,
            _ => {}
        }
        self.done();
    }

    fn intback(&mut self) {
        self.busy = false;
        let b = self.intback_buf;
        self.pmode = b[1] >> 4;
        if b[0] != 0 {
            let o = &mut self.oreg;
            o[0] = (self.smem[4] & 0x80) | ((!self.nmi_reset) as u8) << 6;
            o[1..8].copy_from_slice(&self.rtc);
            o[8] = 0;
            o[9] = self.region;
            o[10] = (self.dot352 as u8) << 6 | 1 << 5 | (self.slave_on as u8) << 4 | 1 << 2 | self.sound_on as u8;
            o[11] = (self.cd_on as u8) << 6 | 1 << 1;
            o[12..16].copy_from_slice(&self.smem[..4]);
            for v in &mut o[16..31] { *v = 0xFF; }
            o[31] = 0x10;
            self.intback_stage = (b[1] & 8) >> 3;
            self.sr = 0x40 | self.intback_stage << 5;
            self.sf = false;
            self.actions.push(Action::Irq);
        } else if b[1] & 8 != 0 {
            self.intback_stage = 1;
            self.sr = 0x40;
            self.oreg[31] = 0x10;
            self.pad_packet();
        } else {
            self.oreg[31] = 0x10;
            self.sf = false;
        }
    }

    /// One INTBACK pad packet: both ports
    fn pad_packet(&mut self) {
        let mut o = 0;
        for port in 0..2 {
            if self.connected[port] {
                let [hi, lo] = self.pads[port].to_be_bytes();
                for v in [0xF1, 0x02, hi, lo] { self.oreg[o] = v; o += 1; }
            } else {
                self.oreg[o] = 0xF0;
                o += 1;
            }
        }
        if self.intback_stage == 2 {
            self.sr = 0x80 | self.pmode;
            self.intback_stage = 0;
        } else {
            self.sr = 0xC0 | self.pmode;
            self.intback_stage += 1;
        }
        self.oreg[31] = 0x10;
        self.sf = false;
        self.actions.push(Action::Irq);
    }
}

fn bcd(v: u32) -> u8 { ((v / 10 % 10) << 4 | v % 10) as u8 }

/// The host's clock (UTC) as the RTC's 7 BCD bytes
fn host_time() -> [u8; 7] {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let days = secs.div_euclid(86_400);
    let t = secs.rem_euclid(86_400) as u32;
    // civil from days (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = (yoe + era * 400 + (m <= 2) as i64) as u32;
    let weekday = (days + 4).rem_euclid(7) as u8; // 1970-01-01 was a Thursday; 0 = Sunday
    [bcd(y / 100), bcd(y % 100), weekday << 4 | m as u8, bcd(d), bcd(t / 3600), bcd(t / 60 % 60), bcd(t % 60)]
}

/// One second on the RTC (BCD, with month lengths and leap years)
fn rtc_second(r: &mut [u8; 7]) {
    let from = |b: u8| (b >> 4) as u32 * 10 + (b & 15) as u32;
    let mut sec = from(r[6]) + 1;
    let mut min = from(r[5]);
    let mut hour = from(r[4]);
    let mut day = from(r[3]);
    let mut month = (r[2] & 15) as u32;
    let mut weekday = r[2] >> 4;
    let mut year = from(r[0]) * 100 + from(r[1]);
    if sec == 60 { sec = 0; min += 1; }
    if min == 60 { min = 0; hour += 1; }
    if hour == 24 {
        hour = 0;
        day += 1;
        weekday = (weekday + 1) % 7;
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let len = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31][(month.clamp(1, 12) - 1) as usize];
        if day > len { day = 1; month += 1; }
        if month > 12 { month = 1; year += 1; }
    }
    *r = [bcd(year / 100), bcd(year % 100), weekday << 4 | month as u8, bcd(day), bcd(hour), bcd(min), bcd(sec)];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtc_rolls_over_a_leap_day() {
        let mut r = [0x20, 0x24, 0x32, 0x28, 0x23, 0x59, 0x59]; // Wed 2024-02-28 23:59:59
        rtc_second(&mut r);
        assert_eq!(r, [0x20, 0x24, 0x42, 0x29, 0, 0, 0]);
        let mut r = [0x19, 0x99, 0x5C, 0x31, 0x23, 0x59, 0x59]; // 1999-12-31
        rtc_second(&mut r);
        assert_eq!(r, [0x20, 0x00, 0x61, 0x01, 0, 0, 0]);
    }

    #[test]
    fn host_time_is_valid_bcd() {
        let t = host_time();
        assert!(t.iter().enumerate().all(|(i, b)| i == 2 || b & 15 < 10));
        assert!((1..=12).contains(&(t[2] & 15)) && t[2] >> 4 < 7);
    }
}
