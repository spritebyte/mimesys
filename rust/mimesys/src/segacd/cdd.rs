//! The CD drive's controller (NEC uPD75006, "CDD"), emulated at the level of its
//! commands: the sub CPU writes a 10-nibble command at FF8042-FF804B, and 75 times a
//! second the drive answers with a 10-nibble status at FF8038-FF8041 and a level-4
//! interrupt. Ported from ares md/mcd/cdd.cpp (ISC license), without the LaserActive
//! parts; subcode isn't read (our images have none).
//!
//! Sector numbers here are LBAs (0 = track 1's INDEX 01); the lead-in is the 150 sectors
//! before 0. Times are reported as MSF = LBA + 150.

use crate::common::disc::Disc;
use serde::{Deserialize, Serialize};

pub mod status {
    pub const STOPPED: u8 = 0x0;
    pub const PLAYING: u8 = 0x1;
    pub const SEEKING: u8 = 0x2;
    pub const PAUSED: u8 = 0x4;
    pub const COMMAND_ERROR: u8 = 0x7;
    pub const CHECKSUM_ERROR: u8 = 0x6;
    pub const READING_TOC: u8 = 0x9;
    pub const TRACKING: u8 = 0xA;
    pub const NO_DISC: u8 = 0xB;
    pub const LEAD_OUT: u8 = 0xC;
    pub const LEAD_IN: u8 = 0xD;
}
use status::*;

/// sectors of lead-in before LBA 0
const LEAD_IN_SECTORS: i32 = 150;

/// The disc's table of contents, as the drive reads it from the lead-in.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Toc {
    /// (track number, pregap LBA, start LBA, audio)
    pub tracks: Vec<(u8, i32, i32, bool)>,
    pub leadout: i32,
}

impl Toc {
    pub fn from_disc(d: &Disc) -> Self {
        Self {
            tracks: d.tracks.iter().map(|t| (t.number, t.pregap as i32, t.start as i32, t.is_audio())).collect(),
            leadout: d.leadout as i32,
        }
    }
    fn present(&self) -> bool { !self.tracks.is_empty() }
    fn first(&self) -> u8 { self.tracks.first().map_or(0, |t| t.0) }
    fn last(&self) -> u8 { self.tracks.last().map_or(0, |t| t.0) }
    fn get(&self, n: u8) -> Option<&(u8, i32, i32, bool)> { self.tracks.iter().find(|t| t.0 == n) }
    pub fn is_audio(&self, n: u8) -> bool { self.get(n).is_some_and(|t| t.3) }
    pub fn is_data(&self, n: u8) -> bool { self.get(n).is_some_and(|t| !t.3) }
    fn start(&self, n: u8) -> i32 { self.get(n).map_or(0, |t| t.2) }
    /// The track a sector is in (its pregap included; track 1's pregap is the 2 seconds
    /// before LBA 0).
    pub fn track_at(&self, sector: i32) -> Option<u8> {
        if sector < -LEAD_IN_SECTORS || sector >= self.leadout { return None; }
        self.tracks.iter().rev().find(|t| sector >= t.1).or(self.tracks.first()).map(|t| t.0)
    }
}

/// (minutes, seconds, frames) of a count of sectors.
fn msf(frames: i32) -> (i32, i32, i32) {
    let f = frames.max(0);
    (f / 4500, f / 75 % 60, f % 75)
}

/// Where a sector is on the disc surface (0-1), for seek times.
fn position(sector: i32) -> f64 {
    const SECTORS: f64 = 7500.0 + 330000.0 + 6750.0;
    const RADIUS: f64 = 0.058 - 0.024;
    const INNER: f64 = 0.024 * 0.024;
    const OUTER: f64 = 0.058 * 0.058;
    let s = (sector + LEAD_IN_SECTORS) as f64;
    (s / SECTORS * (OUTER - INNER) + INNER).sqrt() / RADIUS
}

/// What the drive did in one 75 Hz tick, for the rest of the CD unit.
#[derive(Default)]
pub struct Tick {
    /// raise the drive's interrupt (level 4)
    pub irq: bool,
    /// the decoder gets this data sector
    pub decode: Option<i32>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Cdd {
    pub toc: Toc,
    pub state: u8,
    /// the state to go to after a seek (playing or paused)
    pub seeking: u8,
    pub latency: u16,
    pub sector: i32,
    /// byte position in the current audio sector (4 per stereo sample)
    pub sample: u16,
    pub track: u8,
    /// sub-clock ticks (each 384 sub clocks) into the current 75 Hz period
    pub counter: u16,
    pub host_clock: bool,
    pub status_pending: bool,
    pub status: [u8; 10],
    pub command: [u8; 10],
    /// print each command (debug report on)
    #[serde(skip)]
    pub log: bool,
}

impl Cdd {
    pub fn new(toc: Toc) -> Self {
        let mut c = Self { toc, state: NO_DISC, seeking: 0, latency: 0, sector: 0, sample: 0, track: 0, counter: 0,
                           host_clock: false, status_pending: false, status: [0; 10], command: [0; 10], log: false };
        c.insert();
        c.checksum();
        c
    }

    /// Power on / the peripheral reset: the drive reads the TOC again.
    pub fn power(&mut self) {
        let toc = std::mem::take(&mut self.toc);
        let log = self.log;
        *self = Cdd::new(toc);
        self.log = log;
    }

    fn insert(&mut self) {
        if !self.toc.present() { self.state = NO_DISC; return; }
        self.state = READING_TOC;
        self.sector = -LEAD_IN_SECTORS;
        self.sample = 0;
        self.track = 0;
    }

    pub fn current_is_data(&self) -> bool { self.toc.is_data(self.track) }

    /// Called every 384 sub clocks; does its work 75 times a second.
    pub fn clock(&mut self) -> Tick {
        let mut t = Tick::default();
        self.counter += 1;
        if self.counter < 434 { return t; }
        self.counter = 0;
        if !self.host_clock { return t; }
        if self.status_pending {
            self.status_pending = false;
            t.irq = true;
        }
        match self.state {
            STOPPED => {
                self.state = if self.toc.present() { READING_TOC } else { NO_DISC };
                self.sector = 0;
                self.sample = 0;
                self.track = 0;
            }
            READING_TOC => {
                self.sector += 1;
                if self.sector >= 0 {
                    self.state = PAUSED;
                    if let Some(n) = self.toc.track_at(self.sector) { self.track = n; }
                }
            }
            PLAYING => {
                if !self.toc.is_audio(self.track) {
                    t.decode = Some(self.sector);
                    self.advance();
                }
            }
            TRACKING | SEEKING => {
                if self.latency > 0 {
                    self.latency -= 1;
                    if self.latency > 0 { return t; }
                }
                self.state = self.seeking;
                if let Some(n) = self.toc.track_at(self.sector) { self.track = n; }
            }
            _ => {}
        }
        t
    }

    pub fn advance(&mut self) {
        if let Some(n) = self.toc.track_at(self.sector + 1) {
            self.track = n;
            self.sector += 1;
            self.sample = 0;
        } else if self.sector + 1 < 0 {
            self.state = LEAD_IN;
            self.track = 0xA0;
        } else {
            self.state = LEAD_OUT;
            self.track = 0xAA;
        }
    }

    /// One CD audio sample time (44.1 kHz): audio tracks move on by a sample. Returns the
    /// byte offset of the sample in the current sector when one is playing.
    pub fn sample(&mut self) -> Option<(i32, u16)> {
        if self.state != PLAYING || !self.toc.is_audio(self.track) { return None; }
        let at = (self.sector, self.sample);
        self.sample += 4;
        if self.sample >= 2352 { self.advance(); }
        Some(at)
    }

    fn valid(&self) -> bool {
        let sum = self.command[..9].iter().fold(0u8, |s, &n| s.wrapping_add(n));
        (!sum & 15) == self.command[9]
    }

    fn checksum(&mut self) {
        let sum = self.status[..9].iter().fold(0u8, |s, &n| s.wrapping_add(n));
        self.status[9] = !sum & 15;
    }

    fn set_time(&mut self, kind: u8, frames: i32, last: u8) {
        let (m, s, f) = msf(frames);
        self.status[1] = kind;
        self.status[2] = (m / 10) as u8; self.status[3] = (m % 10) as u8;
        self.status[4] = (s / 10) as u8; self.status[5] = (s % 10) as u8;
        self.status[6] = (f / 10) as u8; self.status[7] = (f % 10) as u8;
        self.status[8] = last;
    }

    fn seek(&mut self, then: u8) {
        let c = &self.command;
        let m = c[2] as i32 * 10 + c[3] as i32;
        let s = c[4] as i32 * 10 + c[5] as i32;
        let f = c[6] as i32 * 10 + c[7] as i32;
        let lba = m * 4500 + s * 75 + f - LEAD_IN_SECTORS - 3;
        self.counter = 0;
        self.state = SEEKING;
        self.seeking = then;
        self.latency = (11.0 + 112.5 * (position(self.sector) - position(lba)).abs()) as u16;
        self.sector = lba;
        self.sample = 0;
        self.status[1..9].copy_from_slice(&[0xF, 0, 0, 0, 0, 0, 0, 0]);
    }

    /// The sub CPU finished writing a command (the last nibble, FF804B).
    pub fn process(&mut self) {
        if !self.valid() {
            self.state = CHECKSUM_ERROR;
        } else {
            match self.command[0] {
                0x0 => {
                    // idle (Lunar: The Silver Star needs the track number back after a seek)
                    if self.latency == 0 && self.status[1] == 0xF {
                        self.status[1] = 0x2;
                        self.status[2] = self.track / 10; self.status[3] = self.track % 10;
                    }
                }
                0x1 => {
                    self.state = if self.toc.present() { STOPPED } else { NO_DISC };
                    self.status[1..9].fill(0);
                }
                0x2 => match self.command[3] {
                    0x0 => { let d = (self.toc.is_data(self.track) as u8) << 2; self.set_time(0x0, self.sector + LEAD_IN_SECTORS, d) }
                    0x1 => {
                        let d = (self.toc.is_data(self.track) as u8) << 2;
                        self.set_time(0x1, self.sector - self.toc.start(self.track), d)
                    }
                    0x2 => {
                        self.status[1] = 0x2;
                        self.status[2] = self.track / 10; self.status[3] = self.track % 10;
                        self.status[4..9].fill(0);
                        if self.sector < 0 { self.status[2] = 0; self.status[3] = 0; }
                        if self.sector >= self.toc.leadout { self.status[2] = 0xA; self.status[3] = 0xA; }
                    }
                    0x3 => self.set_time(0x3, self.toc.leadout + LEAD_IN_SECTORS, 0),
                    0x4 => {
                        let (a, b) = (self.toc.first(), self.toc.last());
                        self.status[1..9].copy_from_slice(&[0x4, a / 10, a % 10, b / 10, b % 10, 0, 0, 0]);
                    }
                    0x5 => {
                        if self.command[4] <= 9 && self.command[5] <= 9 {
                            let n = self.command[4] * 10 + self.command[5];
                            self.set_time(0x5, self.toc.start(n) + LEAD_IN_SECTORS, n % 10);
                            if self.toc.is_data(n) { self.status[6] |= 8; }
                        }
                    }
                    0x6 => self.status[1..9].copy_from_slice(&[0x6, 0, 0, 0, 0, 0, 0, 0]),
                    _ => {}
                },
                0x3 => self.seek(PLAYING),
                0x4 => self.seek(PAUSED),
                0x6 => if self.state == SEEKING { self.seeking = PAUSED } else { self.state = PAUSED },
                0x7 => if self.state == SEEKING { self.seeking = PLAYING } else { self.state = PLAYING },
                0xA => {
                    // track skip: treated as a seek that goes nowhere (the BIOS seeks right after)
                    self.counter = 0;
                    self.state = TRACKING;
                    self.seeking = PAUSED;
                    self.latency = 0;
                    self.sample = 0;
                    self.status[1..9].copy_from_slice(&[0xF, 0, 0, 0, 0, 0, 0, 0]);
                }
                _ => self.state = COMMAND_ERROR,
            }
        }
        self.status[0] = self.state;
        self.checksum();
        self.status_pending = true;
        if self.log && self.command[0] != 0 {
            crate::dlog!("CDD command {:X?} -> status {:X?} (sector {}, track {})", &self.command[..9], &self.status[..9], self.sector, self.track);
        }
    }
}
