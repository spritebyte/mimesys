//! The CDVD drive (1F40_2000), from ps2tek's CDVD pages; behaviour checked against
//! PCSX2's notes (GPL: nothing copied).
//!
//! - N commands (the drive's work): NOP / sync, standby, stop, pause, seek, CD read (2048 /
//!   2328 / 2340-byte blocks), CDDA read (2352 / 2368), DVD read (2064-byte blocks: a
//!   4-byte ID with the sector + 30000h, 8 zeros, the data, 4 zeros), TOC, disc key,
//!   spindle control. Reads seek first (spin-up 333 ms, short / 30 ms / 100 ms seeks), then
//!   deliver one block per sector time (CD 24x: 153 600 B/s per x; DVD 4x: 1 382 400 B/s
//!   per x) into IOP DMA channel 3, waiting while it isn't ready; the end raises the
//!   CDVD interrupt (CDVD I_STAT bit 1, IOP I_STAT bit 2). Errors: 12h no disc, 14h wrong
//!   disc type, 32h past the end, 10h unknown command.
//! - Registers: 05 ready (40h ready, 80h busy, bit 0 error), 06 error (read clears), 0A
//!   status (00 stop, 02 spin, 06 read, 0A pause, 12 seek) and 0B its sticky copy, 0C-0E the
//!   position (BCD MSF), 0F disc type, 13 speed, 20-38 the disc key, 39 key XOR, 3A the
//!   sector decryption setting (bit 0 XOR with key byte 4, bit 1 rotate right by bits 4-6).
//! - The disc key (N 0C) comes from the boot file's serial, the scheme the BIOS's disc
//!   check expects.
//! - S commands answer the drive version, the clock (the host's, in BCD), the config /
//!   NVRAM area (zeros), sub-Q, tray state, MagicGate and the rest with a 0.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

use super::hw::IOP_CDVD;
use super::iop_dma::IopDma;
use crate::common::disc::{lba_to_msf, Disc};

/// IOP cycles per second
const IOP_HZ: u64 = 36_864_000;

/// What the drive is doing
#[derive(Serialize, Deserialize, Default, Clone, Copy, PartialEq, Debug)]
pub enum Op {
    #[default]
    Idle,
    /// a seek / standby / stop / error finishing when the timer runs out
    Finish { status: u8, pos: u32, error: u8 },
    /// a read: seeking, then a block every sector time
    Read { lba: u32, left: u32, block: u32 },
}

#[derive(Serialize, Deserialize)]
pub struct Cdvd {
    pub n_cmd: u8,
    n_params: Vec<u8>,
    pub s_cmd: u8,
    s_params: Vec<u8>,
    s_result: VecDeque<u8>,
    /// CDVD I_STAT (the reason for the IRQ)
    pub istat: u8,
    pub sticky: u8,
    /// the config area open (S 40): 0 read / 1 write, which area, blocks, next block
    config_left: u8,
    #[serde(default)]
    cfg: [u8; 4],
    /// the mechacon's NVRAM (1 KB: settings, IDs); kept in <data>/GD_EMU/PS2/nvram.bin,
    /// not in save states
    #[serde(skip)]
    pub nvm: Vec<u8>,
    #[serde(skip)]
    nvm_loaded: bool,
    /// debugging: register reads by offset
    #[serde(skip)]
    pub reads: Vec<u32>,
    /// debugging: S 03 sub-commands seen
    #[serde(skip)]
    pub sub03: Vec<u8>,
    /// debugging: S commands seen
    #[serde(skip)]
    pub log: Vec<u8>,
    /// ReadRegionParams (S 36): the encryption zone bit and the 8 region bytes, from the
    /// BIOS's ROMVER / OSDVER files
    #[serde(default)]
    pub region: Vec<u8>,
    /// the disc (not in save states: the image is the disc)
    #[serde(skip)]
    pub disc: Option<Disc>,
    #[serde(default)]
    pub disc_type: u8,
    #[serde(default)]
    pub dvd: bool,
    #[serde(default)]
    pub serial: String,
    #[serde(default = "ready_default")]
    pub ready: u8,
    #[serde(default)]
    pub status: u8,
    #[serde(default)]
    pub error: u8,
    /// the head's position (LBA) and whether the disc spins
    #[serde(default)]
    pub pos: u32,
    #[serde(default)]
    spinning: bool,
    #[serde(default)]
    pub op: Op,
    /// IOP cycles until the op's next step
    #[serde(default)]
    timer: i64,
    /// read speed (the x factor) and the sector time it gives (IOP cycles)
    #[serde(default)]
    speed: u32,
    #[serde(default)]
    pub key: [u8; 16],
    #[serde(default)]
    key_xor: u8,
    #[serde(default)]
    pub dec_set: u8,
    #[serde(default)]
    toc_due: bool,
    /// debugging: N commands seen, sectors delivered
    #[serde(skip)]
    pub n_log: Vec<u8>,
    /// debugging: the last N commands with their parameters and outcome
    #[serde(skip)]
    pub n_hist: VecDeque<String>,
    /// debugging: IOP cycles since the current N command, and whether it was waiting
    /// for DMA room
    #[serde(skip)]
    pub n_age: u64,
    #[serde(skip)]
    pub n_dma_waits: u32,
    #[serde(skip)]
    pub sectors: u64,
    /// the disc's end: the image's, or the volume size its ISO 9660 descriptor gives if
    /// that's bigger (images get trimmed; a disc doesn't end early)
    #[serde(default)]
    pub volume_end: u32,
    /// reads that started past the image's end (zeros)
    #[serde(skip)]
    pub past_end: u64,
    /// a dual-layer (PTP) DVD image: where layer 1's volume starts (0: single layer)
    #[serde(default)]
    pub layer1: u32,
}

fn ready_default() -> u8 { 0x40 }

/// The answer length of each S command (ps2tek's MECHACON table); 1 when unknown
fn result_len(c: u8) -> usize {
    match c {
        0x02 => 11, 0x04 => 4, 0x08 => 8, 0x0A => 3, 0x12 | 0x17 | 0x30 | 0x37 => 9, 0x1D | 0x1E => 5,
        0x20 | 0x26 => 3, 0x22 => 10, 0x27 => 13,
        // MagicGate card authentication (SECRMAN): status 0 = fine; 84h / 85h add 12 bytes
        0x84 | 0x85 => 13, 0x32 => 2, 0x36 => 15, 0x41 => 16,
        _ => 1,
    }
}

/// A file in the BIOS's ROMDIR (the table after "RESET": 10-byte name, 2-byte ext info
/// size, 4-byte size; files follow each other from offset 0, 16-byte aligned)
pub fn romdir_file<'a>(bios: &'a [u8], name: &str) -> Option<&'a [u8]> {
    let start = bios.windows(10).take(0x10000).position(|w| w == b"RESET\0\0\0\0\0")?;
    let mut off = 0usize;
    let mut e = start;
    while e + 16 <= bios.len() {
        let n = &bios[e..e + 10];
        if n[0] == 0 { break; }
        let size = u32::from_le_bytes(bios[e + 12..e + 16].try_into().unwrap()) as usize;
        let len = n.iter().position(|&b| b == 0).unwrap_or(10);
        if &n[..len] == name.as_bytes() { return bios.get(off..off + size); }
        off += (size + 15) & !15;
        e += 16;
    }
    None
}

impl Default for Cdvd {
    fn default() -> Self {
        Self {
            n_cmd: 0, n_params: Vec::new(), s_cmd: 0, s_params: Vec::new(), s_result: VecDeque::new(),
            istat: 0, sticky: 0, config_left: 0, cfg: [0; 4], nvm: Vec::new(), nvm_loaded: false, reads: vec![0; 64], sub03: Vec::new(), log: Vec::new(), region: Vec::new(),
            disc: None, disc_type: 0, dvd: false, serial: String::new(), ready: 0x40, status: 0, error: 0,
            pos: 0, spinning: false, op: Op::Idle, timer: 0, speed: 0, key: [0; 16], key_xor: 0, dec_set: 0, toc_due: false,
            n_log: Vec::new(), n_hist: VecDeque::new(), n_age: 0, n_dma_waits: 0, sectors: 0, volume_end: 0, past_end: 0, layer1: 0,
        }
    }
}

impl Cdvd {
    /// Put a disc in (closed tray, not spinning yet)
    pub fn insert(&mut self, mut d: Disc) {
        match super::iso::disc_info(&mut d) {
            Some(i) => {
                println!("PS2: disc {} ({}, {})", i.boot, if i.dvd { "DVD" } else { "CD" }, if i.serial.is_empty() { "no serial" } else { &i.serial });
                self.disc_type = i.disc_type;
                self.dvd = i.dvd;
                self.serial = i.serial;
            }
            None => {
                // not a game disc: a PS2 DVD if it's big, else a CD
                self.dvd = super::iso::is_dvd(&mut d);
                self.disc_type = if self.dvd { 0xFE } else { 0xFD };
            }
        }
        // the ISO 9660 volume size (sector 16, bytes 80-83)
        let pvd = d.read_data(16).filter(|s| &s[1..6] == b"CD001").map_or(0, |s| u32::from_le_bytes([s[80], s[81], s[82], s[83]]));
        self.volume_end = d.leadout.max(pvd);
        if pvd > d.leadout { println!("PS2: the image ends at sector {}, its volume at {pvd}: reads past the image get zeros", d.leadout); }
        // a dual-layer DVD image (PCSX2's test): the first volume ends before the image
        // does and a second volume descriptor sits there (layer 1, numbered from 0 again)
        self.layer1 = 0;
        if self.dvd && pvd > 0 && pvd < d.leadout {
            let is_pvd = |s: Option<[u8; 2048]>| s.is_some_and(|s| &s[..6] == b"\x01CD001");
            if is_pvd(d.read_data(pvd)) || is_pvd(d.read_data(pvd + 16)) {
                self.layer1 = pvd;
                println!("PS2: dual-layer DVD, layer 1 from sector {pvd}");
            }
        }
        self.disc = Some(d);
        // a disc in the drive at power-on is spun up and identified by the time the BIOS
        // asks: paused, spinning (PCSX2's tray sequence ends the same way)
        self.status = 0x0A;
        self.sticky |= 0x0A;
        self.spinning = true;
    }

    /// The region answer from a BIOS image: ROMVER (e.g. "0170EC20040614") gives the
    /// letter (J A E O H R C = zones 0-6), OSDVER (newer BIOSes) the language bytes
    pub fn with_bios(bios: &[u8]) -> Self {
        let mut c = Self::default();
        let letter = romdir_file(bios, "ROMVER").and_then(|r| r.get(4).copied()).unwrap_or(b'J');
        let zone = b"JAEOHRC".iter().position(|&l| l == letter).unwrap_or(0);
        let mut region = vec![1u8 << zone, 0, letter];
        match romdir_file(bios, "OSDVER") {
            Some(o) if o.len() >= 8 => region.extend_from_slice(&o[4..8]),
            _ => region.extend_from_slice(&[letter, b'e', b'n', b'g']),
        }
        region.resize(12, 0);
        c.region = region;
        c
    }
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
        if let Some(n) = self.reads.get_mut((a & 0x3F) as usize) { *n += 1; }
        match a & 0xFF {
            0x04 => self.n_cmd,
            // 08h mechacon initialised, 04h DEV9 connected: always set (CDVDMAN waits
            // 60000h reads for 08h, then gives up on the drive)
            0x05 => self.ready | 0x0C,
            0x06 => std::mem::take(&mut self.error),
            0x08 => self.istat,
            0x0A => self.status,
            0x0B => self.sticky,
            0x0C => { let (m, _, _) = lba_to_msf(self.pos); bcd(m as u32) }
            0x0D => { let (_, sec, _) = lba_to_msf(self.pos); bcd(sec as u32) }
            0x0E => { let (_, _, f) = lba_to_msf(self.pos); bcd(f as u32) }
            0x0F => if self.disc.is_some() { self.disc_type } else { 0 },
            0x13 => {
                if self.disc.is_none() || !self.spinning { 0 } else if self.dvd { 0x0F + self.speed_code() } else { self.speed_code().saturating_sub(1) }
            }
            0x20..=0x24 => self.key[(a & 0xFF) as usize - 0x20],
            0x28..=0x2C => self.key[(a & 0xFF) as usize - 0x23],
            0x30..=0x34 => self.key[(a & 0xFF) as usize - 0x26],
            0x38 => self.key[15],
            0x39 => self.key_xor,
            0x3A => self.dec_set,
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
                self.n_command(v, i_stat);
                self.n_params.clear();
            }
            0x05 => if self.n_params.len() < 16 { self.n_params.push(v) },
            0x07 => {
                // BREAK: abandon a read in progress
                if self.ready & 0x80 != 0 {
                    let stage = match self.op { Op::Read { lba, .. } => if lba == self.pos { "reading" } else { "seeking" }, _ => "busy" };
                    let (age, waits, pos) = (self.n_age, self.n_dma_waits, self.pos);
                    if let Some(h) = self.n_hist.back_mut() {
                        h.push_str(&format!(" -> BREAK after {:.1} ms ({stage}, pos {pos}, {waits} DMA waits)", age as f64 * 1000.0 / IOP_HZ as f64));
                    }
                    self.op = Op::Idle;
                    self.error = 0x01;
                    self.ready = 0x41;
                    self.set_status(0x0A);
                    self.irq(i_stat);
                }
            }
            0x08 => self.istat &= !v,
            0x3A => self.dec_set = v,
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
            0x03 => match { let sub = p(0, self); if self.sub03.len() < 64 { self.sub03.push(sub); } sub } {
                0x00 => vec![0x03, 0x06, 0x02, 0x00],     // mechacon version (as PCSX2: 00020603)
                // tray check: the status, and 8 when the tray is open
                0x30 => vec![self.status, if self.status & 1 != 0 { 8 } else { 0 }],
                // console temperature 30.5 C; renewal date (as PCSX2)
                0xEF => vec![0, 0x0F, 0x05],
                0xFD => vec![0, 0x04, 0x12, 0x10, 0x01, 0x30],
                _ => vec![0],
            },
            0x02 => {
                // sub-Q at the head: control / ADR, track 1, index 1, relative and absolute MSF
                let (m, sec, f) = lba_to_msf(self.pos);
                let rel = self.pos;
                let (m2, s2, f2) = ((rel / 4500) as u32, (rel / 75 % 60) as u32, (rel % 75) as u32);
                vec![0, 0x41, 0x01, 0x01, bcd(m2), bcd(s2), bcd(f2), 0, bcd(m as u32), bcd(sec as u32), bcd(f as u32)]
            }
            0x05 => { self.sticky = self.status & 1; vec![0] }
            0x06 => vec![0],                               // tray control: stays shut
            0x08 => {
                let [s, m, h, d, mo, y] = now();
                vec![0, bcd(s), bcd(m), bcd(h), 0, bcd(d), bcd(mo), bcd(y)]
            }
            0x12 => vec![0; 9],                            // i.LINK ID
            0x15 => vec![5],                               // forbid DVD
            0x1A => vec![1],                               // boot certify
            0x0A => {
                // ReadNVM: one 16-bit word (address in words, big-endian), high byte first
                let w = (p(0, self) as usize) << 8 | p(1, self) as usize;
                self.nvm_ready();
                if w < 512 { vec![0, self.nvm[w * 2 + 1], self.nvm[w * 2]] } else { vec![0xFF] }
            }
            0x0B => {
                let w = (p(0, self) as usize) << 8 | p(1, self) as usize;
                self.nvm_ready();
                if w < 512 {
                    self.nvm[w * 2] = p(3, self);
                    self.nvm[w * 2 + 1] = p(2, self);
                    self.nvm_save();
                    vec![0]
                } else { vec![0xFF] }
            }
            0x40 => {
                // OpenConfig: read (0) / write (1), area 0-2, block count
                self.cfg = [p(0, self), p(1, self), p(2, self), 0];
                vec![0]
            }
            0x41 => {
                let [rw, area, count, idx] = self.cfg;
                if rw != 0 { let mut v = vec![0x80]; v.resize(16, 0); v }
                else if idx >= count { vec![0; 16] }
                else {
                    self.cfg[3] += 1;
                    match self.config_offset(area, idx) {
                        Some(o) => { self.nvm_ready(); self.nvm[o..o + 16].to_vec() }
                        None => vec![0; 16],
                    }
                }
            }
            0x42 => {
                let [rw, area, count, idx] = self.cfg;
                if rw == 1 && idx < count {
                    self.cfg[3] += 1;
                    if let Some(o) = self.config_offset(area, idx) {
                        self.nvm_ready();
                        for k in 0..16 { self.nvm[o + k] = p(k, self); }
                    }
                }
                vec![0]
            }
            0x43 => {
                if self.cfg[0] == 1 { self.nvm_save(); }
                self.cfg = [0; 4];
                vec![0]
            }
            0x36 => {
                // status, 1 << zone, 0, then ROMVER's region letter and OSDVER's
                let mut r = vec![0];
                r.extend_from_slice(&self.region);
                r
            }
            _ => vec![0],
        };
        let mut r = r;
        r.resize(result_len(c).max(r.len()), 0);
        self.s_result.extend(r);
    }
}

// ====================================================================== NVRAM

impl Cdvd {
    /// Where a config block lives in the NVRAM (BIOS 1.70+ layout: areas at 270h, 2B0h,
    /// 200h with 4 / 2 / 7 blocks)
    fn config_offset(&self, area: u8, idx: u8) -> Option<usize> {
        let (base, max) = match area { 0 => (0x270, 4), 1 => (0x2B0, 2), _ => (0x200, 7) };
        (idx < max).then(|| base + idx as usize * 16)
    }

    fn nvm_path() -> Option<std::path::PathBuf> {
        crate::common::debug_log::data_dir().map(|d| d.join("GD_EMU").join("PS2").join("nvram.bin"))
    }

    /// Load the NVRAM the first time it's needed (a new one is zeros: the BIOS runs its
    /// first-time setup and writes its settings)
    fn nvm_ready(&mut self) {
        if self.nvm_loaded { return; }
        self.nvm_loaded = true;
        self.nvm = Self::nvm_path().and_then(|p| std::fs::read(p).ok()).filter(|v| v.len() == 1024).unwrap_or_else(|| vec![0; 1024]);
    }

    fn nvm_save(&mut self) {
        let Some(p) = Self::nvm_path() else { return };
        if let Some(dir) = p.parent() { let _ = std::fs::create_dir_all(dir); }
        if let Err(e) = std::fs::write(&p, &self.nvm) { println!("PS2: can't save the NVRAM to {}: {e}", p.display()); }
    }
}

// ====================================================================== N commands

impl Cdvd {
    fn irq(&mut self, i_stat: &mut u32) {
        self.istat |= 2;
        *i_stat |= IOP_CDVD;
    }

    fn set_status(&mut self, s: u8) {
        self.status = s;
        self.sticky |= s;
    }

    /// A little-endian parameter word
    fn param(&self, at: usize) -> u32 {
        (0..4).fold(0, |a, k| a | (self.n_params.get(at + k).copied().unwrap_or(0) as u32) << (8 * k))
    }

    /// SpindlCtrl's speed field as the 13h register counts it (1 = x1 .. 5 = x24)
    fn speed_code(&self) -> u8 {
        match self.speed { 1 => 1, 2 => 2, 4 => 3, 12 => 4, _ => if self.dvd { 3 } else { 5 } }
    }

    /// IOP cycles one block takes at the current speed
    fn block_time(&self, block: u32) -> i64 {
        let x = self.speed.max(1) as u64;
        let rate = if self.dvd { 1_382_400 } else { 153_600 } * x;
        (IOP_HZ * block as u64 / rate) as i64
    }

    /// IOP cycles to get the head to `to` (spinning up first if stopped)
    fn seek_time(&mut self, to: u32, block: u32) -> i64 {
        let mut t = 0i64;
        if !self.spinning {
            t += (IOP_HZ / 3) as i64;                   // spin-up, 333 ms
            self.spinning = true;
        }
        let delta = (to as i64 - self.pos as i64).unsigned_abs() as u32;
        let (contiguous, fast) = if self.dvd { (16, 14_764) } else { (8, 4_371) };
        t += if delta < contiguous {
            self.block_time(block) * delta as i64
        } else if delta < fast {
            (IOP_HZ * 30 / 1000) as i64
        } else {
            // a long seek: the sled's travel, 30 ms up to 70 ms across the whole disc.
            // (PCSX2 uses a flat 100 ms; Ico polls the status once a frame during a read
            // and BREAKs after five SEEKs in a row, so a real drive manages its 106 000-
            // sector jump in under ~67 ms)
            let full = if self.dvd { 2_295_104u64 } else { 360_000 };
            let ms = 30 + 40 * (delta as u64).min(full) / full;
            (IOP_HZ * ms / 1000) as i64
        };
        t
    }

    /// Set the speed from a read's SpindlCtrl byte (low 6 bits 0: the fastest)
    fn spindle(&mut self, b: u8) {
        let code = if b & 0x3F != 0 { b & 7 } else if self.dvd { 3 } else { 5 };
        self.speed = match code { 1 => 1, 2 => 2, 3 => 4, 4 => 12, _ => 24 };
        if self.dvd { self.speed = self.speed.min(4); }
    }

    fn fail(&mut self, error: u8, i_stat: &mut u32) {
        if let Some(h) = self.n_hist.back_mut() { h.push_str(&format!(" -> error {error:02X} at {}", self.pos)); }
        self.error = error;
        self.ready = 0x41;
        self.op = Op::Idle;
        self.irq(i_stat);
    }

    fn n_command(&mut self, c: u8, i_stat: &mut u32) {
        if self.n_log.len() < 64 { self.n_log.push(c); }
        if self.n_hist.len() >= 16 { self.n_hist.pop_front(); }
        self.n_age = 0;
        self.n_dma_waits = 0;
        self.n_hist.push_back(match c {
            0x05..=0x08 | 0x0E => format!("{c:02X} lba {} n {} mode {:02X} spd {:02X}", self.param(0), self.param(4), self.n_params.get(10).copied().unwrap_or(0), self.n_params.get(9).copied().unwrap_or(0)),
            _ => format!("{c:02X} {:02X?}", self.n_params),
        });
        if c > 0x01 && self.disc.is_none() { return self.fail(0x12, i_stat); }
        match c {
            0x00 | 0x01 => { self.ready = 0x40; self.irq(i_stat); }
            0x02 => {
                // standby: spin up, head to 0
                let t = self.seek_time(0, 2048);
                self.start(Op::Finish { status: 0x0A, pos: 0, error: 0 }, t, 0x12);
            }
            0x03 => {
                self.spinning = false;
                self.start(Op::Finish { status: 0x00, pos: 0, error: 0 }, (IOP_HZ / 6) as i64, 0x02);
            }
            0x04 => { self.op = Op::Idle; self.ready = 0x40; self.set_status(0x0A); self.irq(i_stat); }
            0x05 => {
                let to = self.param(0);
                let t = self.seek_time(to, 2048);
                self.start(Op::Finish { status: 0x0A, pos: to, error: 0 }, t, 0x12);
            }
            0x06 | 0x07 | 0x08 | 0x0E => {
                let (lba, count) = (self.param(0), self.param(4));
                let mode = self.n_params.get(10).copied().unwrap_or(0);
                self.spindle(self.n_params.get(9).copied().unwrap_or(0));
                let block = match c {
                    0x08 => { if !self.dvd { return self.fail(0x14, i_stat); } 2064 }
                    0x06 => match mode { 1 => 2328, 2 => 2340, _ => 2048 },
                    _ => { if self.dvd { return self.fail(0x14, i_stat); } if mode == 1 { 2368 } else { 2352 } }
                };
                // a read starting past the end fails; one that runs past it gets zeros
                // there (images stop at the last data sector, a disc doesn't). The end is
                // the volume's if the image was trimmed short of it (Metal Gear Solid 2
                // Substance reads DEMO2.DAT, a padding file past its image's end).
                // DVDs also get 10000h sectors of slack: dual-layer images don't always say
                // where their volume ends
                let leadout = self.disc.as_ref().map_or(0, |d| d.leadout);
                let end = leadout.max(self.volume_end).max(if self.dvd { leadout + 0x1_0000 } else { 0 });
                if lba >= end { return self.fail(0x32, i_stat); }
                if lba >= leadout { self.past_end += 1; }
                let t = self.seek_time(lba, block) + self.block_time(block);
                self.start(Op::Read { lba, left: count, block }, t, 0x12);
            }
            0x0C => {
                let arg2 = self.param(3);
                self.key = disc_key(&self.serial, arg2);
                self.key_xor = 0;
                self.ready = 0x40;
                self.irq(i_stat);
            }
            0x09 => { self.ready = 0x80; self.toc_due = true; }     // the TOC goes out on the next tick
            0x0F => { self.ready = 0x40; self.irq(i_stat); }
            _ => self.fail(0x10, i_stat),
        }
    }

    fn start(&mut self, op: Op, t: i64, status: u8) {
        self.op = op;
        self.timer = t.max(1);
        self.ready = 0x80;
        self.set_status(status);
    }

    /// `cycles` IOP cycles: the op's next steps, blocks into DMA channel 3
    pub fn tick(&mut self, cycles: u32, dma: &mut IopDma, ram: &mut [u8], i_stat: &mut u32) {
        if self.ready & 0x80 != 0 { self.n_age += cycles as u64; }
        // GetToc: written straight to channel 3's buffer (the channel ends if it runs)
        if self.toc_due {
            self.toc_due = false;
            let toc = self.toc();
            dma.cdvd_put(ram, &toc, true);
            self.ready = 0x40;
            self.set_status(0x0A);
            self.irq(i_stat);
        }
        if self.op == Op::Idle { return; }
        self.timer -= cycles as i64;
        let mut guard = 0;
        while self.timer <= 0 && guard < 64 {
            guard += 1;
            match self.op {
                Op::Idle => return,
                Op::Finish { status, pos, error } => {
                    self.pos = pos;
                    self.set_status(status);
                    self.op = Op::Idle;
                    if error != 0 { self.error = error; self.ready = 0x41; } else { self.ready = 0x40; }
                    self.irq(i_stat);
                    return;
                }
                Op::Read { lba, left, block } => {
                    if left == 0 {
                        self.op = Op::Idle;
                        self.ready = 0x40;
                        self.set_status(0x0A);
                        self.irq(i_stat);
                        return;
                    }
                    if !dma.cdvd_room(block as usize) {
                        // the IOP hasn't set up the next DMA yet: wait
                        self.n_dma_waits += 1;
                        self.set_status(0x0A);
                        self.timer += 2000;
                        continue;
                    }
                    self.set_status(0x06);
                    let data = self.block(lba, block);
                    dma.cdvd_put(ram, &data, false);
                    self.sectors += 1;
                    self.pos = lba + 1;
                    self.op = Op::Read { lba: lba + 1, left: left - 1, block };
                    if left - 1 == 0 {
                        self.op = Op::Idle;
                        self.ready = 0x40;
                        self.set_status(0x0A);
                        self.irq(i_stat);
                        return;
                    }
                    self.timer += self.block_time(block);
                }
            }
        }
    }

    /// One block of a read, decrypted as DEC_SET says
    fn block(&mut self, lba: u32, block: u32) -> Vec<u8> {
        let Some(d) = self.disc.as_mut() else { return vec![0; block as usize] };
        let mut out = match block {
            2048 => d.read_data(lba).map_or(vec![0; 2048], |s| s.to_vec()),
            2064 => {
                // the ID: layer number in bit 0, the sector on that layer + 30000h
                let (layer, lsn) = if self.layer1 > 0 && lba >= self.layer1 { (1, lba - self.layer1 + 0x30000) } else { (0, lba + 0x30000) };
                let mut v = vec![0x20 | layer, (lsn >> 16) as u8, (lsn >> 8) as u8, lsn as u8];
                v.extend_from_slice(&[0; 8]);
                v.extend_from_slice(&d.read_data(lba).unwrap_or([0; 2048]));
                v.extend_from_slice(&[0; 4]);
                v
            }
            _ => {
                let mut raw = [0u8; crate::common::disc::SECTOR];
                d.read_sector(lba, &mut raw);
                match block {
                    2328 => raw[24..].to_vec(),
                    2340 => raw[12..].to_vec(),
                    2368 => { let mut v = raw.to_vec(); v.extend_from_slice(&[0; 16]); v }
                    _ => raw.to_vec(),
                }
            }
        };
        let ds = self.dec_set;
        if ds & 3 != 0 {
            let sh = (ds >> 4 & 7) as u32;
            for b in out.iter_mut() {
                if ds & 1 != 0 { *b ^= self.key[4]; }
                if ds & 2 != 0 { *b = b.rotate_right(sh); }
            }
        }
        out
    }

    /// The table of contents as GetToc gives it (DVD: 2064 bytes, single layer; CD: 1024)
    fn toc(&mut self) -> Vec<u8> {
        let Some(d) = self.disc.as_ref() else { return vec![0; 1024] };
        if self.dvd {
            let mut t = vec![0u8; 2064];
            if self.layer1 > 0 {
                // dual layer, PTP: layer 1's start (as PCSX2 builds it)
                t[..6].copy_from_slice(&[0x24, 0x02, 0xF2, 0x00, 0x41, 0x95]);
                t[12..20].copy_from_slice(&[0x01, 0x02, 0x21, 0x10, 0x00, 0x03, 0x00, 0x00]);
                t[20..24].copy_from_slice(&(self.layer1 + 0x30000 - 1).to_be_bytes());
            } else {
                t[..6].copy_from_slice(&[0x04, 0x02, 0xF2, 0x00, 0x86, 0x72]);
                t[12..20].copy_from_slice(&[0x01, 0x02, 0x01, 0x00, 0x00, 0x03, 0x00, 0x00]);
                t[20..24].copy_from_slice(&(d.leadout + 0x30000 - 1).to_be_bytes());
            }
            return t;
        }
        let mut t = vec![0u8; 1024];
        let msf = |lba: u32| { let (m, s, f) = lba_to_msf(lba); [bcd(m as u32), bcd(s as u32), bcd(f as u32)] };
        let n = d.tracks.len().max(1) as u32;
        t[0] = 0x41;
        t[2] = 0xA0;
        t[7] = bcd(1);
        t[12] = 0xA1;
        t[17] = bcd(n);
        t[22] = 0xA2;
        t[27..30].copy_from_slice(&msf(d.leadout));
        for (i, tr) in d.tracks.iter().enumerate().take(80) {
            let k = (i + 1) * 10;
            t[k + 30] = if tr.is_audio() { 0x01 } else { 0x41 };
            t[k + 32] = bcd(i as u32 + 1);
            t[k + 37..k + 40].copy_from_slice(&msf(tr.start));
        }
        t
    }
}

/// The disc key the BIOS's disc check reads (N 0C): built from the serial's four letters
/// and five digits; sector argument 75 also sets bytes 14 / 15, 4246 a fixed key
pub fn disc_key(serial: &str, arg2: u32) -> [u8; 16] {
    let mut key = [0u8; 16];
    let b = serial.as_bytes();
    let (numbers, letters) = if b.len() >= 10 {
        let n = std::str::from_utf8(&b[5..10]).ok().and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
        let l = (b[3] as u32 & 0x7F) | (b[2] as u32 & 0x7F) << 7 | (b[1] as u32 & 0x7F) << 14 | (b[0] as u32 & 0x7F) << 21;
        (n, l)
    } else { (0, 0) };
    let k03 = ((numbers & 0x1FC00) >> 10) | ((letters & 0x01FF_FFFF) << 7);
    let k4 = (((numbers & 0x1F) << 3) | ((letters & 0x0E00_0000) >> 25)) as u8;
    let k14 = (((numbers & 0x3E0) >> 2) | 0x04) as u8;
    key[..4].copy_from_slice(&k03.to_le_bytes());
    key[4] = k4;
    match arg2 {
        75 => { key[14] = k14; key[15] = 0x05; }
        4246 => { key[..5].copy_from_slice(&[0x07, 0xF7, 0xF2, 0x01, 0x00]); key[15] = 0x01; }
        _ => key[15] = 0x01,
    }
    key
}
