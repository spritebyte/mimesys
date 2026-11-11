//! GD-ROM drive: the ATA register interface, the packet-command protocol and the disc
//! (GDI / CHD / CUE / ISO through `common::disc`). Commands and replies after MAME
//! devices/bus/ata/gdrom.cpp (BSD-3-Clause), including the security reply table (0x71).
//!
//! Registers (offset from 5F7000): 18 alternate status / device control, 80 data,
//! 84 error / features, 88 interrupt reason / sector count, 8C sector number (drive status
//! in bits 0-3, disc type in 4-7), 90 / 94 byte count, 98 drive select, 9C status / command.
//! The drive's interrupt is Holly's external line 0, held until the status is read.
//!
//! Positions are FADs (frame addresses): LBA + 150. A GD-ROM's single-density area holds
//! tracks 1-2 from FAD 150; the high-density area starts at FAD 45150 (track 3).
//! Data goes to the host by PIO (the data register, in pieces no longer than the byte-count
//! limit the host set) or, when the features register's bit 0 was set for CD_READ, by
//! G1 DMA (`dma_take` / `dma_finish`, driven by the bus).

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::common::disc::{Disc, TrackKind, SECTOR};

#[allow(dead_code)]
pub const BSY: u8 = 0x80;
pub const DRDY: u8 = 0x40;
pub const DSC: u8 = 0x10;
pub const DRQ: u8 = 0x08;
pub const CHECK: u8 = 0x01;

/// drive states (sector number bits 0-3)
pub const ST_PAUSE: u8 = 0x01;
pub const ST_STANDBY: u8 = 0x02;
pub const ST_PLAY: u8 = 0x03;
pub const ST_NODISC: u8 = 0x07;

/// The disc in the drive, shared so the drive state stays cheap to clone; never saved in
/// a state (the frontend opens it again).
#[derive(Clone, Default)]
pub struct DiscSlot(pub Option<Arc<Mutex<Disc>>>);

impl DiscSlot {
    pub fn new(d: Disc) -> Self { Self(Some(Arc::new(Mutex::new(d)))) }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum Phase {
    Idle,
    /// receiving the 12-byte packet
    Packet,
    /// the host reads `data`
    DataIn,
    /// the host writes this many bytes (SET_MODE)
    DataOut(usize),
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Gdrom {
    /// packet commands by opcode since the last speed line, and the unhandled ones seen
    #[serde(skip)]
    pub cmd_counts: std::collections::BTreeMap<u8, u64>,
    #[serde(skip)]
    unknown: std::collections::BTreeSet<u8>,
    pub status: u8,
    pub error: u8,
    pub ireason: u8,
    /// drive state (ST_*)
    pub state: u8,
    pub features: u8,
    pub byte_count: u16,
    pub drive_select: u8,
    pub devctl: u8,
    /// interrupt line to Holly
    pub irq: bool,
    phase: Phase,
    packet: Vec<u8>,
    #[serde(with = "serde_bytes")]
    data: Vec<u8>,
    pos: usize,
    /// end of the piece the host is reading now (PIO)
    chunk_end: usize,
    /// byte-count limit the host set before the packet
    limit: u16,
    /// the data waits for a G1 DMA
    pub dma: bool,
    /// the sense key / additional code of the last error (REQ_ERROR)
    sense: u8,
    asc: u8,
    /// the drive's mode table (REQ_MODE / SET_MODE)
    mode: Vec<u8>,
    /// the current position (FAD)
    pub fad: u32,
    /// sectors read (debug)
    pub sectors: u64,
    /// commands received (debug)
    pub log: Vec<String>,
    #[serde(skip)]
    pub disc: DiscSlot,
    /// CD audio: end FAD (exclusive), start (for repeats), repeats left (15 = forever),
    /// audio status for GET_SCD (0x11 playing, 0x12 paused, 0x13 done, 0x15 none)
    #[serde(default)]
    play_end: u32,
    #[serde(default)]
    play_start: u32,
    #[serde(default)]
    repeat: u8,
    #[serde(default)]
    audio_status: u8,
    /// the sector being played, as stereo frames, and the next frame
    #[serde(skip)]
    cd_buf: Vec<[i16; 2]>,
    #[serde(skip)]
    cd_pos: usize,
}

impl Default for Gdrom {
    fn default() -> Self { Self::new() }
}

const MODE_DEFAULT: [u8; 32] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0xB4, 0x19, 0x00, 0x00, 0x08, 0x53, 0x45, 0x20, 0x20, 0x20, 0x20,
    0x20, 0x20, 0x52, 0x65, 0x76, 0x20, 0x36, 0x2E, 0x34, 0x32, 0x39, 0x39, 0x30, 0x33, 0x31, 0x36,
];

/// The obfuscated reply to SYS_REQ_SECU (0x71), from MAME. Its hidden first byte (at 10)
/// is 0x10 (a MIL-CD); a GD-ROM gets 0x1F there.
const CMD71_REPLY: [u8; 1012] = [
    0x96, 0x0B, 0x45, 0xF0, 0x7E, 0xFF, 0x3D, 0x06, 0x4D, 0x7D, 0x10, 0xBF, 0x07, 0x00, 0x73, 0xCF,
    0x9C, 0x00, 0xBC, 0x0C, 0x1C, 0xAF, 0x1C, 0x30, 0xE7, 0xA7, 0x03, 0xA8, 0x98, 0x00, 0xBD, 0x0F,
    0xBD, 0x5B, 0xAA, 0x50, 0x23, 0x39, 0x31, 0x10, 0x0E, 0x69, 0x13, 0xE5, 0x00, 0xD2, 0x0D, 0x66,
    0x54, 0xBF, 0x5F, 0xFD, 0x37, 0x74, 0xF4, 0x5B, 0x22, 0x00, 0xC6, 0x09, 0x0F, 0xCA, 0x93, 0xE8,
    0xA4, 0xAB, 0x00, 0x61, 0x0E, 0x2E, 0xE1, 0x4B, 0x76, 0x8B, 0x6A, 0xA5, 0x9C, 0xE6, 0x23, 0xC4,
    0x00, 0x4B, 0x06, 0x1B, 0x91, 0x01, 0x00, 0xE2, 0x0D, 0xCF, 0xCA, 0x38, 0x3A, 0xB9, 0xE7, 0x91,
    0xE5, 0xEF, 0x4B, 0x00, 0xD6, 0x09, 0xD3, 0x68, 0x3E, 0xC4, 0xAF, 0x2D, 0x00, 0x2A, 0x0D, 0xF9,
    0xFC, 0x78, 0xED, 0xAE, 0x99, 0xB3, 0x32, 0x5A, 0xE7, 0x00, 0x4C, 0x0A, 0x22, 0x97, 0x5B, 0x82,
    0x06, 0x7A, 0x4C, 0x00, 0x42, 0x0E, 0x57, 0x78, 0x46, 0xF5, 0x20, 0xFC, 0x6B, 0xCB, 0x01, 0x5B,
    0x86, 0x00, 0xE4, 0x0E, 0xB2, 0x26, 0xCD, 0x71, 0xE3, 0xA5, 0x33, 0x06, 0x8E, 0x9A, 0x50, 0x00,
    0x07, 0x07, 0xF5, 0x34, 0xEF, 0xE6, 0x00, 0x32, 0x0F, 0x13, 0x41, 0x59, 0x56, 0x0F, 0x02, 0x38,
    0x2A, 0x64, 0x2A, 0x07, 0x3E, 0x00, 0x52, 0x11, 0x2A, 0x1D, 0x5F, 0x76, 0x66, 0xA0, 0xB2, 0x2F,
    0x97, 0xC7, 0x5E, 0x6E, 0x52, 0xE2, 0x00, 0x58, 0x09, 0xCA, 0x89, 0xA5, 0xDF, 0x0A, 0xDE, 0x00,
    0x50, 0x06, 0x49, 0xB8, 0xB4, 0x00, 0x77, 0x05, 0x24, 0xE8, 0x00, 0xBB, 0x0C, 0x91, 0x89, 0xA2,
    0x8B, 0x62, 0xDE, 0x6A, 0xC6, 0x60, 0x00, 0xE7, 0x0F, 0x0F, 0x11, 0x96, 0x55, 0xD2, 0xBF, 0xE6,
    0x48, 0x0B, 0x5C, 0xAB, 0xDC, 0x00, 0xBA, 0x0A, 0x30, 0xD7, 0x48, 0x0E, 0x78, 0x63, 0x0C, 0x00,
    0xD2, 0x0D, 0xFB, 0x8A, 0xA3, 0xFE, 0xF8, 0x3A, 0xDD, 0x88, 0xA9, 0x4B, 0x00, 0xA2, 0x0A, 0x75,
    0x5D, 0x0D, 0x37, 0x24, 0xC5, 0x9D, 0x00, 0xF7, 0x0B, 0x25, 0xEF, 0xDB, 0x41, 0xE0, 0x52, 0x3E,
    0x4E, 0x00, 0xB7, 0x03, 0x00, 0xE5, 0x11, 0xB9, 0xDE, 0x5A, 0x57, 0xCF, 0xB9, 0x1A, 0xFC, 0x7F,
    0x26, 0xEE, 0x7B, 0xCD, 0x2B, 0x00, 0x4B, 0x08, 0xB8, 0x09, 0x70, 0x6A, 0x9F, 0x00, 0x4B, 0x11,
    0x8C, 0x15, 0x87, 0xA3, 0x05, 0x4F, 0x37, 0x8E, 0x63, 0xDE, 0xEF, 0x39, 0xFC, 0x4B, 0x00, 0xAB,
    0x10, 0x0B, 0x91, 0xAA, 0x0F, 0xE1, 0xE9, 0xAE, 0x69, 0x3A, 0xF8, 0x03, 0x69, 0xD2, 0x00, 0xE2,
    0x07, 0xC1, 0x5C, 0x3D, 0x82, 0x00, 0xA9, 0x08, 0x68, 0xC4, 0xAD, 0x2E, 0xD1, 0x00, 0xF7, 0x0E,
    0xC6, 0x47, 0xC8, 0xCD, 0x8E, 0x7C, 0x00, 0x5C, 0x95, 0xB9, 0xF4, 0x00, 0xE3, 0x04, 0x5B, 0x00,
    0x74, 0x07, 0x65, 0xC7, 0x84, 0x8E, 0x00, 0xC6, 0x07, 0x61, 0x80, 0x44, 0x3F, 0x00, 0xC8, 0x0E,
    0x72, 0x78, 0x47, 0xD3, 0xC2, 0x4D, 0xAF, 0xC0, 0x54, 0x13, 0x31, 0x00, 0xF7, 0x0D, 0x48, 0xD8,
    0xE2, 0x92, 0x9F, 0x7F, 0x2F, 0x44, 0x68, 0x33, 0x00, 0x0D, 0x10, 0xAB, 0xFE, 0xEA, 0x8E, 0x19,
    0x81, 0xF8, 0x6F, 0x7C, 0xDE, 0xE1, 0xB3, 0x06, 0x00, 0x4D, 0x11, 0x66, 0xAE, 0x4C, 0xF9, 0xB7,
    0x2F, 0xEE, 0xB0, 0x8E, 0x7E, 0xE1, 0x8D, 0x95, 0x6F, 0x00, 0xF4, 0x0D, 0x88, 0x9D, 0xCA, 0xE3,
    0xC4, 0xB2, 0x47, 0xBB, 0xA0, 0x69, 0x00, 0xF3, 0x0B, 0x48, 0x17, 0x41, 0x64, 0xA0, 0x0E, 0x71,
    0x82, 0x00, 0x34, 0x1E, 0x18, 0x4D, 0x85, 0x80, 0x4C, 0xA9, 0x0B, 0x66, 0x9B, 0x75, 0x13, 0x61,
    0x70, 0x27, 0x81, 0x7A, 0x02, 0xCD, 0x57, 0xAB, 0xDF, 0x02, 0x93, 0x52, 0x83, 0xDF, 0x48, 0xA8,
    0xA6, 0x9E, 0x74, 0x6F, 0x89, 0x03, 0x28, 0x25, 0x52, 0x96, 0xFF, 0x67, 0x7A, 0xD8, 0x3C, 0xB1,
    0x2C, 0x46, 0x84, 0xEF, 0xE1, 0xC1, 0xC6, 0xC9, 0xDC, 0x96, 0xAA, 0xA9, 0xC4, 0x82, 0x58, 0x27,
    0x57, 0x75, 0x67, 0x34, 0xFB, 0x3B, 0x25, 0xBF, 0xFB, 0x3B, 0xF6, 0x13, 0xEC, 0x96, 0xE5, 0x16,
    0x26, 0xFD, 0xA8, 0xDA, 0x1B, 0xC6, 0x50, 0x7F, 0x47, 0xFF, 0x08, 0x55, 0x08, 0xED, 0x00, 0x93,
    0x9B, 0xC4, 0x71, 0x67, 0xEC, 0xA6, 0xCC, 0x16, 0x20, 0x87, 0x47, 0x07, 0xA6, 0x00, 0x79, 0x5D,
    0x4F, 0xAB, 0xA1, 0x6F, 0x7A, 0x6B, 0x27, 0xC4, 0xDA, 0xA3, 0xC3, 0x94, 0x4F, 0x7F, 0xF3, 0xE5,
    0x1B, 0x6F, 0xCC, 0xE5, 0xF0, 0xE5, 0x9D, 0xC9, 0xAE, 0xFD, 0x39, 0xAC, 0x4C, 0xE5, 0x58, 0x83,
    0x25, 0x65, 0x92, 0x74, 0x9E, 0x81, 0xA0, 0xB6, 0xA9, 0x02, 0x9B, 0x07, 0xB6, 0xE7, 0x79, 0x57,
    0xD9, 0x4A, 0xCE, 0xFA, 0xB4, 0x94, 0x05, 0xCC, 0x86, 0x3C, 0xDD, 0x06, 0xCD, 0xA6, 0x24, 0x24,
    0xFA, 0xC1, 0xF9, 0x48, 0xC9, 0x0C, 0x6C, 0xC4, 0x96, 0x82, 0x17, 0xF6, 0x31, 0x09, 0xC4, 0xE2,
    0x77, 0xFD, 0xCF, 0x46, 0x18, 0xB2, 0x5F, 0x01, 0x6B, 0xD1, 0x7B, 0x56, 0xB8, 0x94, 0x4A, 0xE5,
    0x6C, 0x19, 0xF0, 0xC0, 0xB6, 0x70, 0x93, 0xF7, 0xD3, 0xD1, 0x2B, 0x6E, 0x7C, 0x53, 0x6D, 0x85,
    0xD1, 0x0C, 0x8B, 0x77, 0xEE, 0x90, 0xDA, 0x15, 0x55, 0xE0, 0x58, 0x09, 0x56, 0xFC, 0x31, 0x9F,
    0xAF, 0x46, 0xCB, 0xC3, 0x8D, 0x71, 0x75, 0xF2, 0x2C, 0xC3, 0xBB, 0xA1, 0xC4, 0xCF, 0x27, 0x56,
    0x7C, 0x9B, 0xFE, 0xAF, 0x3E, 0x4E, 0xB4, 0xCD, 0x6A, 0xAA, 0xF5, 0xF3, 0xE3, 0x22, 0x82, 0xE1,
    0xA5, 0x68, 0xB3, 0xDB, 0x8F, 0x9E, 0x5E, 0x7B, 0x90, 0xF0, 0x79, 0x3F, 0x52, 0x8C, 0x61, 0x88,
    0x76, 0xAE, 0x14, 0x63, 0x19, 0x0F, 0x1D, 0xCE, 0xA1, 0x63, 0x10, 0xB2, 0xE2, 0xD7, 0x94, 0xB1,
    0x33, 0xCB, 0x28, 0x85, 0x7D, 0x9B, 0xF5, 0xF4, 0x25, 0x50, 0x9B, 0xDB, 0x35, 0xA5, 0xB0, 0x9C,
    0x09, 0x92, 0xE3, 0x31, 0x40, 0xAB, 0x4D, 0xF4, 0x35, 0xE8, 0xB3, 0x0A, 0x21, 0xC3, 0x86, 0x9C,
    0xCB, 0x29, 0xA4, 0x77, 0x57, 0xBC, 0xD8, 0xDA, 0xA5, 0x82, 0x80, 0xE8, 0xCF, 0x72, 0x81, 0xAD,
    0x2E, 0x28, 0xFF, 0xD8, 0xB6, 0xD1, 0x2B, 0x97, 0x00, 0xFF, 0xE1, 0x06, 0x44, 0x39, 0x1C, 0x4B,
    0xAB, 0x19, 0x5B, 0x4D, 0xD6, 0x3E, 0x1B, 0x5C, 0x64, 0xBB, 0x32, 0x68, 0xF5, 0x7C, 0xC9, 0x9E,
    0xE8, 0xB4, 0x29, 0x1B, 0x7F, 0x4D, 0x80, 0x80, 0x7E, 0x8B, 0x1C, 0x0A, 0xE6, 0x9A, 0xBF, 0x49,
    0x1E, 0xC5, 0xB6, 0x67, 0x7D, 0x05, 0xE4, 0x90, 0x40, 0x4B, 0xAF, 0x9B, 0x52, 0xDE, 0x17, 0x80,
    0x81, 0x56, 0xEA, 0x3A, 0x53, 0x82, 0x8C, 0x62, 0xFB, 0x96, 0x97, 0x6F, 0xC1, 0x16, 0x78, 0xD4,
    0x7B, 0xE7, 0xB9, 0x5A, 0x2A, 0xEB, 0x87, 0x68, 0x33, 0xD3, 0x31, 0x45, 0xFA, 0xFE, 0xF4, 0x1C,
    0x90, 0x86, 0x73, 0x77, 0xD9, 0xA9, 0xD1, 0x4A, 0x4A, 0xCF, 0xAE, 0x23, 0xDB, 0xF9, 0x09, 0xD8,
    0x18, 0xDC, 0x6A, 0x0D, 0xE4, 0x19, 0x8C, 0x65, 0xC6, 0x64, 0xC7, 0xDC, 0xA9, 0xE3, 0x91, 0xB1,
    0x4C, 0xC8, 0xC1, 0x9E, 0x3B, 0x7F, 0xCB, 0xA3, 0xCF, 0xDD, 0xF0, 0x1D, 0x07, 0x6E, 0xDC, 0xCE,
    0x0D, 0xCD, 0x7E, 0x1E, 0x55, 0x11, 0x8B, 0xDF, 0x3A, 0xAB, 0xB6, 0x3B, 0x6E, 0x52, 0x7F, 0xA7,
    0x00, 0xD1, 0x33, 0xBE, 0xF2, 0x9B, 0xFC, 0x4A, 0xCF, 0x9D, 0x8F, 0xC6, 0xC4, 0x7B, 0xDA, 0xE7,
    0x2A, 0x1C, 0x26, 0x6E,
];

impl Gdrom {
    pub fn new() -> Self {
        Self {
            status: DRDY | DSC, error: 0, ireason: 3, state: ST_STANDBY, features: 0, byte_count: 0,
            drive_select: 0, devctl: 0, irq: false, phase: Phase::Idle, packet: Vec::new(), data: Vec::new(),
            pos: 0, chunk_end: 0, limit: 0, dma: false, sense: 0, asc: 0, mode: MODE_DEFAULT.to_vec(), fad: 150,
            sectors: 0, log: Vec::new(), disc: DiscSlot::default(), cmd_counts: Default::default(), unknown: Default::default(),
            play_end: 0, play_start: 0, repeat: 0, audio_status: 0x15, cd_buf: Vec::new(), cd_pos: 0,
        }
    }

    /// A reset keeps the disc and the debug log.
    pub fn reset(&mut self) {
        let log = std::mem::take(&mut self.log);
        let disc = std::mem::take(&mut self.disc);
        *self = Self::new();
        self.log = log;
        self.disc = disc;
    }

    pub fn has_disc(&self) -> bool { self.disc.0.is_some() }

    fn is_gd(&self) -> bool { self.disc.0.as_ref().is_some_and(|d| d.lock().map(|d| d.gd).unwrap_or(false)) }

    /// Sector number register: disc type (8 GD-ROM, 1 CD-ROM, 0 audio CD) and state.
    pub fn sector_number(&self) -> u8 {
        let Some(d) = &self.disc.0 else { return ST_NODISC };
        let d = d.lock().unwrap();
        let ty = if d.gd { 0x80 } else if d.tracks.iter().any(|t| !t.is_audio()) { 0x10 } else { 0x00 };
        ty | self.state
    }

    /// Keep the last 32 commands (the start of the boot is in the first report).
    fn note(&mut self, s: String) {
        if self.log.len() >= 32 { self.log.remove(0); }
        self.log.push(s);
    }

    fn interrupt(&mut self) { if self.devctl & 2 == 0 { self.irq = true; } }

    /// The command finished: good, or an error with a sense key and code.
    fn complete(&mut self, sense: Option<(u8, u8)>) {
        self.phase = Phase::Idle;
        self.ireason = 3;
        self.dma = false;
        match sense {
            None => { self.status = DRDY | DSC; self.error = 0; }
            Some((k, asc)) => { self.status = DRDY | DSC | CHECK; self.error = k << 4; self.sense = k; self.asc = asc; }
        }
        self.interrupt();
    }

    fn not_ready(&mut self) { self.complete(Some((2, 0x3A))); }
    fn illegal(&mut self) { self.complete(Some((5, 0x24))); }

    /// Hand `data` to the host (by DMA when `dma`).
    fn send(&mut self, data: Vec<u8>, dma: bool) {
        if data.is_empty() { return self.complete(None); }
        self.data = data;
        self.pos = 0;
        self.phase = Phase::DataIn;
        self.ireason = 2;
        if dma {
            self.dma = true;
            self.status = DRDY | DSC | DRQ;
        } else {
            self.next_chunk();
        }
    }

    /// The next PIO piece: as much as the host's limit allows.
    fn next_chunk(&mut self) {
        let limit = match self.limit & !1 { 0 => 0x8000, l => l as usize };
        let n = (self.data.len() - self.pos).min(limit);
        self.byte_count = n as u16;
        self.chunk_end = self.pos + n;
        self.status = DRDY | DSC | DRQ;
        self.interrupt();
    }

    /// G1 DMA: up to `max` bytes of the waiting data.
    pub fn dma_take(&mut self, max: usize) -> Vec<u8> {
        if !self.dma || self.phase != Phase::DataIn { return Vec::new(); }
        let n = (self.data.len() - self.pos).min(max);
        let out = self.data[self.pos..self.pos + n].to_vec();
        self.pos += n;
        out
    }

    /// The G1 DMA finished: the command completes once all its data went.
    pub fn dma_finish(&mut self) {
        if self.dma && self.pos >= self.data.len() { self.complete(None); }
    }

    /// The next CD audio frame (44.1 kHz stereo) while playing; silence otherwise.
    pub fn cdda(&mut self) -> [i16; 2] {
        if self.state != ST_PLAY { return [0, 0]; }
        if self.cd_pos >= self.cd_buf.len() {
            if self.fad >= self.play_end {
                if self.repeat == 0 { self.state = ST_PAUSE; self.audio_status = 0x13; return [0, 0]; }
                if self.repeat != 0xF { self.repeat -= 1; }
                self.fad = self.play_start;
            }
            let Some(d) = self.disc.0.clone() else { return [0, 0] };
            let mut sec = [0u8; SECTOR];
            d.lock().unwrap().read_sector(self.fad.saturating_sub(150), &mut sec);
            self.cd_buf = sec.chunks_exact(4).map(|c| [i16::from_le_bytes([c[0], c[1]]), i16::from_le_bytes([c[2], c[3]])]).collect();
            self.cd_pos = 0;
            self.fad += 1;
        }
        let f = self.cd_buf[self.cd_pos];
        self.cd_pos += 1;
        f
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            0x18 => self.status as u32,
            0x80 => self.read_data() as u32,
            0x84 => self.error as u32,
            0x88 => self.ireason as u32,
            0x8C => self.sector_number() as u32,
            0x90 => (self.byte_count & 0xFF) as u32,
            0x94 => (self.byte_count >> 8) as u32,
            0x98 => self.drive_select as u32,
            0x9C => { self.irq = false; self.status as u32 }
            _ => 0,
        }
    }

    fn read_data(&mut self) -> u16 {
        if self.phase != Phase::DataIn || self.dma { return 0; }
        let lo = *self.data.get(self.pos).unwrap_or(&0);
        let hi = *self.data.get(self.pos + 1).unwrap_or(&0);
        self.pos += 2;
        if self.pos >= self.data.len() {
            self.complete(None);
        } else if self.pos >= self.chunk_end {
            self.next_chunk();
        }
        u16::from_le_bytes([lo, hi])
    }

    pub fn write(&mut self, off: u32, v: u32) {
        match off {
            0x18 => {
                self.devctl = v as u8;
                if v & 4 != 0 { self.reset(); }
            }
            0x80 => self.write_data(v as u16),
            0x84 => self.features = v as u8,
            0x88 | 0x8C => {}
            0x90 => self.byte_count = self.byte_count & 0xFF00 | (v & 0xFF) as u16,
            0x94 => self.byte_count = self.byte_count & 0x00FF | ((v & 0xFF) as u16) << 8,
            0x98 => self.drive_select = v as u8,
            0x9C => self.command(v as u8),
            _ => {}
        }
    }

    fn write_data(&mut self, v: u16) {
        match self.phase {
            Phase::Packet => {
                self.packet.extend(v.to_le_bytes());
                if self.packet.len() >= 12 {
                    let p = std::mem::take(&mut self.packet);
                    self.exec_packet(&p);
                }
            }
            Phase::DataOut(n) => {
                self.data.extend(v.to_le_bytes());
                if self.data.len() >= n {
                    let d = std::mem::take(&mut self.data);
                    for (k, b) in d.iter().take(n).enumerate() { if let Some(m) = self.mode.get_mut(k) { *m = *b; } }
                    self.complete(None);
                }
            }
            _ => {}
        }
    }

    fn command(&mut self, cmd: u8) {
        self.note(format!("ATA {cmd:02X}"));
        match cmd {
            0xA0 => {
                // PACKET: take 12 bytes; the byte count is the host's transfer limit
                self.limit = self.byte_count;
                self.phase = Phase::Packet;
                self.packet.clear();
                self.ireason = 1;
                self.status = DRDY | DSC | DRQ;
            }
            0xA1 => self.send(identify(), false),
            0x08 => {
                // soft reset: the ATAPI signature
                self.reset();
                self.byte_count = 0xEB14;
                self.ireason = 1;
            }
            0xEF | 0x00 => self.complete(None), // SET FEATURES, NOP
            _ => self.illegal(),
        }
    }

    fn exec_packet(&mut self, p: &[u8]) {
        self.note(format!("packet {:02X?}", &p[..12]));
        *self.cmd_counts.entry(p[0]).or_insert(0) += 1;
        let len = p[4] as usize;
        let disc = self.disc.0.clone();
        // commands that need a disc
        if disc.is_none() && matches!(p[0], 0x00 | 0x14 | 0x15 | 0x20 | 0x21 | 0x22 | 0x30 | 0x31 | 0x40 | 0x70 | 0x71) {
            return self.not_ready();
        }
        match p[0] {
            0x00 => self.complete(None), // TEST_UNIT
            0x10 => {
                // REQ_STAT: state, type / repeat, control / ADR, track, index, FAD
                let (ctrl, track) = disc.as_ref().map_or((0, 0), |d| {
                    let d = d.lock().unwrap();
                    d.track_at(self.fad.saturating_sub(150)).map_or((0x41, 1), |t| (ctrl_adr(t.kind), t.number))
                });
                let f = self.fad;
                let d = vec![self.sector_number() & 0xF, self.sector_number() & 0xF0, ctrl, track, 1,
                             (f >> 16) as u8, (f >> 8) as u8, f as u8, 0, 0];
                self.send(window(&d, p[2] as usize, len), false);
            }
            0x11 => {
                // REQ_MODE: part of the mode table
                let d = window(&self.mode, p[2] as usize, len);
                self.send(d, false);
            }
            0x12 => {
                // SET_MODE: the host sends the table
                let n = len.min(0x20);
                self.data.clear();
                self.phase = Phase::DataOut(n);
                self.byte_count = n as u16;
                self.ireason = 0;
                self.status = DRDY | DSC | DRQ;
                self.interrupt();
            }
            0x13 => {
                // REQ_ERROR
                let d = [0xF0, 0, self.sense, 0, 0, 0, 0, 0, self.asc, 0];
                self.send(window(&d, 0, len.max(1)), false);
            }
            0x14 => {
                // GET_TOC: area (bit 0 of byte 1: 1 = high density), allocation length
                let alloc = (p[3] as usize) << 8 | p[4] as usize;
                let toc = toc(&disc.unwrap().lock().unwrap(), p[1] & 1);
                self.send(window(&toc, 0, alloc.max(1)), false);
            }
            0x15 => {
                // REQ_SES
                let d = disc.unwrap();
                let d = d.lock().unwrap();
                let s = p[2];
                let (n, fad) = session(&d, s);
                let r = [self.state, 0, n, (fad >> 16) as u8, (fad >> 8) as u8, fad as u8];
                drop(d);
                self.send(window(&r, 0, len.max(1)), false);
            }
            0x16 => self.complete(None), // CD_OPEN
            0x20 => {
                // CD_PLAY: 1 = FADs, 2 = MSFs, 7 = resume; start, repeat count, end
                match p[1] & 7 {
                    1 => { self.fad = fad_of(p, 2); self.play_end = fad_of(p, 8); }
                    2 => { self.fad = msf_of(p, 2); self.play_end = msf_of(p, 8); }
                    _ => {}
                }
                if p[1] & 7 != 7 { self.play_start = self.fad; self.repeat = p[6] & 0xF; self.cd_buf.clear(); }
                self.state = ST_PLAY;
                self.audio_status = 0x11;
                self.complete(None);
            }
            0x21 => {
                // CD_SEEK: 1 FAD, 2 MSF, 3 stop, 4 pause
                match p[1] & 0xF {
                    1 => { self.fad = fad_of(p, 2); self.state = ST_PAUSE; }
                    2 => { self.fad = msf_of(p, 2); self.state = ST_PAUSE; }
                    3 => self.state = ST_STANDBY,
                    _ => self.state = ST_PAUSE,
                }
                if self.audio_status == 0x11 { self.audio_status = if p[1] & 0xF == 4 { 0x12 } else { 0x15 }; }
                self.complete(None);
            }
            0x22 => self.complete(None), // CD_SCAN
            0x30 | 0x31 => {
                // CD_READ: start (FAD, or MSF with bit 0), count; data select in bits 4-7
                let start = if p[1] & 1 != 0 { msf_of(p, 2) } else { fad_of(p, 2) };
                let count = fad_of(p, 8);
                let raw = p[1] >> 4 == 0xF;
                let d = disc.unwrap();
                let mut d = d.lock().unwrap();
                let mut out = Vec::with_capacity(count as usize * if raw { SECTOR } else { 2048 });
                let mut sec = [0u8; SECTOR];
                let mut ok = true;
                for k in 0..count {
                    let lba = (start + k).saturating_sub(150);
                    let Some(kind) = d.track_at(lba).map(|t| t.kind) else { ok = false; break };
                    if !d.read_sector(lba, &mut sec) { ok = false; break; }
                    if raw || kind == TrackKind::Audio {
                        out.extend_from_slice(&sec);
                    } else {
                        let at = if sec[15] == 2 { 24 } else { 16 };
                        out.extend_from_slice(&sec[at..at + 2048]);
                    }
                }
                drop(d);
                self.sectors += count as u64;
                self.fad = start + count;
                self.state = ST_PAUSE;
                if !ok { return self.complete(Some((5, 0x21))); }
                let dma = self.features & 1 != 0;
                self.send(out, dma);
            }
            0x40 => {
                // GET_SCD: audio status, then (format 1) the position
                let alloc = (p[3] as usize) << 8 | p[4] as usize;
                let st = self.audio_status;
                let d = match p[1] & 0xF {
                    0 => { let mut d = vec![0u8; 100]; d[1] = st; d[3] = 100; d }
                    _ => {
                        let f = self.fad;
                        let (ctrl, track) = self.disc.0.as_ref().map_or((0x41, 1), |d| {
                            let d = d.lock().unwrap();
                            d.track_at(f.saturating_sub(150)).map_or((0x41, 1), |t| (ctrl_adr(t.kind), t.number))
                        });
                        vec![0, st, 0, 14, ctrl, track, 1, 0, 0, 0, 0, (f >> 16) as u8, (f >> 8) as u8, f as u8]
                    }
                };
                self.send(window(&d, 0, alloc.max(1)), false);
            }
            0x70 => self.complete(None), // SYS_CHK_SECU
            0x71 => {
                let mut r = CMD71_REPLY.to_vec();
                if self.is_gd() { r[10] = 0x1F; }
                self.send(r, false);
            }
            c => {
                if self.unknown.insert(c) { crate::dlog!("Dreamcast: GD-ROM packet command {c:02X} not handled: {:02X?}", &p[..12]); }
                self.illegal()
            }
        }
    }
}

/// `len` bytes of `d` from `off` (shorter at the end).
fn window(d: &[u8], off: usize, len: usize) -> Vec<u8> {
    d.iter().skip(off).take(len).copied().collect()
}

fn fad_of(p: &[u8], at: usize) -> u32 { (p[at] as u32) << 16 | (p[at + 1] as u32) << 8 | p[at + 2] as u32 }
fn msf_of(p: &[u8], at: usize) -> u32 { (p[at] as u32 * 60 + p[at + 1] as u32) * 75 + p[at + 2] as u32 }

/// Control (4 = data) and ADR (1) of a track, as the TOC shows them.
fn ctrl_adr(k: TrackKind) -> u8 { if k == TrackKind::Audio { 0x01 } else { 0x41 } }

/// The tracks of an area: a GD-ROM's single-density (LBA < 45000) or high-density area;
/// a CD has one.
fn area_tracks(d: &Disc, area: u8) -> Vec<(u8, u32, TrackKind)> {
    d.tracks.iter().filter(|t| !d.gd || (area == 1) == (t.start >= 45000)).map(|t| (t.number, t.start, t.kind)).collect()
}

/// GET_TOC: 99 entries (control/ADR, FAD 24-bit big-endian) by track number, unused
/// 0xFF; then first track, last track, lead-out.
pub fn toc(d: &Disc, area: u8) -> Vec<u8> {
    let mut out = vec![0xFF; 408];
    let tracks = area_tracks(d, area);
    let (Some(first), Some(last)) = (tracks.first(), tracks.last()) else { return out };
    for &(n, start, kind) in &tracks {
        let i = (n as usize - 1) * 4;
        if i + 4 > 396 { continue; }
        let f = start + 150;
        out[i..i + 4].copy_from_slice(&[ctrl_adr(kind), (f >> 16) as u8, (f >> 8) as u8, f as u8]);
    }
    let end = d.tracks.iter().find(|t| t.number == last.0).map_or(d.leadout, |t| t.start + t.length) + 150;
    out[396..400].copy_from_slice(&[ctrl_adr(first.2), first.0, 0, 0]);
    out[400..404].copy_from_slice(&[ctrl_adr(last.2), last.0, 0, 0]);
    out[404..408].copy_from_slice(&[ctrl_adr(last.2), (end >> 16) as u8, (end >> 8) as u8, end as u8]);
    out
}

/// REQ_SES: session 0 = (session count, lead-out FAD); n = (first track, its FAD).
fn session(d: &Disc, s: u8) -> (u8, u32) {
    let hd = d.gd && d.tracks.iter().any(|t| t.start >= 45000);
    let count = if hd { 2 } else { 1 };
    match s {
        0 => (count, d.leadout + 150),
        1 => d.tracks.first().map_or((1, 150), |t| (t.number, t.start + 150)),
        _ => d.tracks.iter().find(|t| t.start >= 45000).map_or((0, 0), |t| (t.number, t.start + 150)),
    }
}

/// ATA IDENTIFY PACKET DEVICE (MAME's words): "SE" maker, "CD-ROM DRIVE" model, "6.42".
fn identify() -> Vec<u8> {
    let mut w = [0u16; 80];
    w[0] = 0x8600;
    let put = |w: &mut [u16; 80], at: usize, s: &[u8]| for (k, c) in s.chunks(2).enumerate() { w[at + k] = c[0] as u16 | (c[1] as u16) << 8; };
    put(&mut w, 8, b"SE      ");
    put(&mut w, 16, b"CD-ROM DRIVE    6.42");
    for k in 26..47 { w[k] = 0x2020; }
    w[49] = 0x0400;
    w[63] = 7;
    w[64] = 1;
    w.iter().flat_map(|v| v.to_le_bytes()).collect()
}

#[cfg(test)]
#[path = "gd_tests.rs"]
mod tests;
