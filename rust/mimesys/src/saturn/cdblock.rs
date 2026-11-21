//! The CD block's host interface (A-bus CS2, 0x05800000) and a high-level model of the
//! block itself: the drive, 200 sector buffers in 24 partitions, 24 filters (selectors),
//! the ISO 9660 file system commands and CD-DA. Ported from MAME's
//! sega/saturn_cd_hle.cpp (R. Belmont, Angelo Salese; BSD-3-Clause), with the power-on
//! HIRQ from Sega's CD interface programmer's guide.
//!
//! - Registers: HIRQ 0x90008, its mask 0x9000C, CR1-CR4 0x90018-0x90024 (mirrors every
//!   0x8000 from 0x80000); the data port at 0x18000 (and 0x98000).
//! - After reset CR1-CR4 read "CDBLOCK"; HIRQ is 0BE1 (CMOK, the command end flags ESEL /
//!   EHST / ECPY / EFLS / MPED, and DCHG, which the BIOS clears).
//! - A command: the host clears CMOK, writes CR1-CR4 (the command in CR1's top byte); about
//!   60 us later the block answers in CR1-CR4 (CR1's top byte is the drive status) and sets
//!   CMOK plus the command's "done" bits in HIRQ. Status changes go through BUSY for one
//!   sector time; commands wait for them.
//! - The drive reads 150 sectors a second (75 at single speed, and for audio). A data sector
//!   goes through the filter the "CD device" connects to (FAD range, file / channel /
//!   submode / coding info), and lands in that filter's partition, or the next filter's.
//! - Once CR4 has been read, the status is reported every sector (PERI, SCDQ).
//! - CD-DA: audio sectors become 44.1 kHz stereo samples for the SCSP's EXTS inputs.

use crate::common::disc::{Disc, SECTOR};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

pub const CMOK: u16 = 0x0001;
pub const DRDY: u16 = 0x0002;
pub const CSCT: u16 = 0x0004;
pub const BFUL: u16 = 0x0008;
pub const PEND: u16 = 0x0010;
pub const DCHG: u16 = 0x0020;
pub const ESEL: u16 = 0x0040;
pub const EHST: u16 = 0x0080;
pub const ECPY: u16 = 0x0100;
pub const EFLS: u16 = 0x0200;
pub const SCDQ: u16 = 0x0400;
pub const MPED: u16 = 0x0800;
pub const MPCM: u16 = 0x1000;

const STAT_BUSY: u16 = 0x0000;
const STAT_PAUSE: u16 = 0x0100;
const STAT_STANDBY: u16 = 0x0200;
const STAT_PLAY: u16 = 0x0300;
const STAT_SEEK: u16 = 0x0400;
const STAT_NODISC: u16 = 0x0700;
const STAT_PERI: u16 = 0x2000;
const STAT_TRANS: u16 = 0x4000;
const STAT_WAIT: u16 = 0x8000;
const STAT_REJECT: u16 = 0xFF00;

const MAX_FILTERS: usize = 24;
const MAX_BLOCKS: usize = 200;
const MAX_DIR_SIZE: usize = 256 * 1024;
const NONE: u8 = 0xFF;

/// Picoseconds
const US: i64 = 1_000_000;

#[derive(Clone, Default, Serialize, Deserialize)]
struct DirEntry { firstfad: u32, length: u32, flags: u8, file_unit_size: u8, interleave_gap: u8 }

#[derive(Clone, Copy, Serialize, Deserialize)]
struct Filter { mode: u8, chan: u8, smmask: u8, cimask: u8, fid: u8, smval: u8, cival: u8, condtrue: u8, condfalse: u8, fad: u32, range: u32 }

impl Default for Filter {
    fn default() -> Self { Self { mode: 0, chan: 0, smmask: 0, cimask: 0, fid: 0, smval: 0, cival: 0, condtrue: 0, condfalse: NONE, fad: 0, range: 0xFFFF_FFFF } }
}

#[derive(Clone, Serialize, Deserialize)]
struct Block {
    /// bytes held (-1 = free)
    size: i32,
    fad: u32,
    #[serde(with = "serde_bytes")]
    data: Vec<u8>,
    fnum: u8,
    chan: u8,
    subm: u8,
    cinf: u8,
}

impl Block {
    fn new() -> Self { Self { size: -1, fad: 0, data: vec![0; SECTOR], fnum: 0, chan: 0, subm: 0, cinf: 0 } }
}

#[derive(Clone, Serialize, Deserialize)]
struct Partition {
    /// bytes held (-1 = never used)
    size: i32,
    /// block numbers, packed at the front (NONE = empty)
    blk: Vec<u8>,
    numblks: usize,
}

impl Partition {
    fn new() -> Self { Self { size: -1, blk: vec![NONE; MAX_BLOCKS], numblks: 0 } }
}

/// What the data port is sending
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Xfer { None, Toc, FileInfo1, FileInfo254, SubQ, SubRw, GetSector, GetDeleteSector, PutSector }

#[derive(Clone, Serialize, Deserialize)]
pub struct CdBlock {
    pub hirq: u16,
    pub hirq_mask: u16,
    pub cr: [u16; 4],
    stat: u16,
    next_stat: u16,
    seek_stat: u16,
    changing: bool,
    /// which of CR1-CR4 the host has written (bit n)
    written: u8,
    /// picoseconds until the command runs / the next sector
    cmd_left: i64,
    sector_left: i64,
    curfad: u32,
    fad_seek: u32,
    fadstoplay: u32,
    cur_track: u8,
    playtype: u8,
    speed: u8,
    maxrepeat: u8,
    repeat: u8,
    buffull: bool,
    buffull_pause: bool,
    sectorstore: bool,
    freeblocks: usize,
    sectlenin: usize,
    sectlenout: usize,
    lastbuf: u8,
    filters: Vec<Filter>,
    device: u8,
    partitions: Vec<Partition>,
    blocks: Vec<Block>,
    xfer: Xfer,
    xfercount: usize,
    xferoffs: usize,
    xfersect: usize,
    xferpos: usize,
    xfernum: usize,
    xferdnum: u32,
    xferpart: usize,
    calcsize: u32,
    toc: Vec<u8>,
    small: Vec<u8>,
    dir: Vec<DirEntry>,
    root: DirEntry,
    firstfile: usize,
    /// CD-DA samples for the SCSP (L, R)
    #[serde(skip)]
    pub audio: VecDeque<(i16, i16)>,
    #[serde(skip)]
    pub disc: Option<Arc<Mutex<Disc>>>,
    #[serde(skip)]
    pub log: Vec<[u16; 4]>,
    #[serde(skip)]
    pub hirq_reads: u64,
    #[serde(skip)]
    pub answers: Vec<String>,
}

fn bcd(v: u32) -> u8 { ((v / 10) << 4 | (v % 10)) as u8 }

impl CdBlock {
    pub fn new(disc: Option<Arc<Mutex<Disc>>>) -> Self {
        let mut c = Self {
            hirq: 0x0BE1, hirq_mask: 0, cr: [0x0043, 0x4442, 0x4C4F, 0x434B], stat: STAT_NODISC, next_stat: STAT_NODISC,
            seek_stat: STAT_BUSY, changing: false, written: 0, cmd_left: 0, sector_left: 0,
            curfad: 150, fad_seek: 150, fadstoplay: 0, cur_track: NONE, playtype: 0, speed: 2, maxrepeat: 0, repeat: 0,
            buffull: false, buffull_pause: false, sectorstore: false, freeblocks: MAX_BLOCKS, sectlenin: 2048, sectlenout: 2048,
            lastbuf: NONE, filters: vec![Filter::default(); MAX_FILTERS], device: NONE,
            partitions: vec![Partition::new(); MAX_FILTERS], blocks: vec![Block::new(); MAX_BLOCKS],
            xfer: Xfer::None, xfercount: 0, xferoffs: 0, xfersect: 0, xferpos: 0, xfernum: 0, xferdnum: 0, xferpart: 0, calcsize: 0,
            toc: vec![0xFF; 102 * 4], small: vec![0; 254 * 12], dir: Vec::new(), root: DirEntry::default(), firstfile: 0,
            audio: VecDeque::new(), disc, log: Vec::new(), hirq_reads: 0, answers: Vec::new(),
        };
        if c.disc.is_some() {
            c.read_new_dir(0xFF_FFFF);
            c.curfad = 150;
            c.change_status(STAT_PAUSE);
        } else {
            c.change_status(STAT_NODISC);
        }
        c
    }

    fn has_disc(&self) -> bool { self.disc.is_some() }

    // ---------------- the disc ----------------

    fn with_disc<T>(&self, f: impl FnOnce(&mut Disc) -> T) -> Option<T> {
        self.disc.as_ref().map(|d| f(&mut d.lock().unwrap()))
    }

    /// 0-based track index at a FAD (LBA + 150)
    fn track_of(&self, fad: u32) -> Option<usize> {
        self.with_disc(|d| {
            let lba = fad.saturating_sub(150);
            d.tracks.iter().rposition(|t| t.start <= lba)
        }).flatten()
    }

    fn track_start(&self, t: usize) -> u32 {
        self.with_disc(|d| d.tracks.get(t).map_or(d.leadout, |tr| tr.start)).unwrap_or(0)
    }

    fn leadout(&self) -> u32 { self.with_disc(|d| d.leadout).unwrap_or(0) }

    fn ntracks(&self) -> usize { self.with_disc(|d| d.tracks.len()).unwrap_or(0) }

    fn is_audio(&self, t: usize) -> bool { self.with_disc(|d| d.tracks.get(t).is_some_and(|t| t.is_audio())).unwrap_or(false) }

    /// Control / ADR as the TOC gives it: 41 data, 01 audio
    fn adr_control(&self, t: usize) -> u16 { if self.is_audio(t) { 0x01 } else { 0x41 } }

    fn track_index(&self, fad: u32) -> u16 {
        match self.track_of(fad) {
            Some(t) if self.is_audio(t) && fad.saturating_sub(150) < self.track_start(t) + 150 && t > 0 => 0,
            _ => 1,
        }
    }

    fn read_raw(&self, fad: u32) -> Option<[u8; SECTOR]> {
        self.with_disc(|d| { let mut s = [0u8; SECTOR]; d.read_sector(fad.saturating_sub(150), &mut s).then_some(s) }).flatten()
    }

    fn read_block(&self, fad: u32) -> [u8; 2048] {
        self.with_disc(|d| d.read_data(fad.saturating_sub(150))).flatten().unwrap_or([0; 2048])
    }

    // ---------------- registers ----------------

    /// A word at 0x05800000 + `off`
    pub fn read(&mut self, off: u32) -> u16 {
        // the data port: 0x05818000 (and 0x05890000's mirrors)
        if off & 0x7FFC == 0 && (off & 0x18000 == 0x18000 || off & 0x80000 != 0) { return self.data_word(); }
        if off & 0x80000 == 0 { return 0xFFFF; }
        match off & 0x7FFC {
            0x08 => {
                self.hirq_reads += 1;
                self.hirq &= !DCHG;
                if self.buffull { self.hirq |= BFUL; } else { self.hirq &= !BFUL; }
                if self.sectorstore { self.hirq |= CSCT; } else { self.hirq &= !CSCT; }
                self.hirq
            }
            0x0C => self.hirq_mask,
            0x18 => self.cr[0],
            0x1C => self.cr[1],
            0x20 => self.cr[2],
            0x24 => {
                self.written = 0;
                self.stat |= STAT_PERI;
                self.cr[3]
            }
            _ => 0xFFFF,
        }
    }

    pub fn write(&mut self, off: u32, v: u16) {
        if off & 0x7FFC == 0 && (off & 0x18000 == 0x18000 || off & 0x80000 != 0) { self.data_write(v); return; }
        if off & 0x80000 == 0 { return; }
        match off & 0x7FFC {
            0x08 => {
                if self.answers.len() < 64 { self.answers.push(format!("hirq&={v:04X}")); }
                self.hirq &= v
            }
            0x0C => self.hirq_mask = v,
            o @ (0x18 | 0x1C | 0x20 | 0x24) => {
                let i = (o as usize - 0x18) / 4;
                self.cr[i] = v;
                self.written |= 1 << i;
                if i == 0 { self.stat &= !STAT_PERI; }
                if i == 3 { self.cmd_left = 60 * US; }
            }
            _ => {}
        }
    }

    // ---------------- the data port ----------------

    fn data_word(&mut self) -> u16 {
        match self.xfer {
            Xfer::GetSector | Xfer::GetDeleteSector => {
                if self.xfersect < self.xfernum {
                    let p = &self.partitions[self.xferpart];
                    let b = p.blk[(self.xferpos + self.xfersect).min(MAX_BLOCKS - 1)];
                    if b == NONE { self.xfersect = self.xfernum; return 0xFFFF; }
                    let blk = &self.blocks[b as usize];
                    let o = self.xferoffs.min(SECTOR - 2);
                    let v = u16::from_be_bytes([blk.data[o], blk.data[o + 1]]);
                    self.xferdnum += 2;
                    self.xferoffs += 2;
                    if self.xferoffs as i32 >= blk.size {
                        self.xferoffs = 0;
                        self.xfersect += 1;
                    }
                    v
                } else {
                    if self.xfer == Xfer::GetDeleteSector {
                        self.delete_transferred();
                        self.xfer = Xfer::None;
                    }
                    0xFFFF
                }
            }
            Xfer::Toc => self.small_word(|s| (&s.toc, 102 * 4)),
            Xfer::FileInfo1 | Xfer::SubQ | Xfer::SubRw => {
                let len = match self.xfer { Xfer::FileInfo1 => 12, Xfer::SubQ => 10, _ => 24 };
                self.small_word(|s| (&s.small, len))
            }
            Xfer::FileInfo254 => {
                if self.xfercount % 12 == 0 {
                    let i = 2 + self.xfercount / 12;
                    let e = self.dir.get(i).cloned().unwrap_or_default();
                    self.small[0..4].copy_from_slice(&e.firstfad.to_be_bytes());
                    self.small[4..8].copy_from_slice(&e.length.to_be_bytes());
                    self.small[8] = e.interleave_gap;
                    self.small[9] = e.file_unit_size;
                    self.small[10] = i as u8;
                    self.small[11] = e.flags;
                }
                let o = self.xfercount % 12;
                let v = u16::from_be_bytes([self.small[o], self.small[o + 1]]);
                self.xfercount += 2;
                self.xferdnum += 2;
                if self.xfercount > 254 * 12 { self.xfercount = 0; self.xfer = Xfer::None; }
                v
            }
            _ => 0xFFFF,
        }
    }

    fn small_word(&mut self, buf: impl Fn(&Self) -> (&Vec<u8>, usize)) -> u16 {
        let (b, len) = buf(self);
        let o = self.xfercount;
        let v = if o + 1 < b.len() { u16::from_be_bytes([b[o], b[o + 1]]) } else { 0xFFFF };
        self.xfercount += 2;
        self.xferdnum += 2;
        if self.xfercount >= len { self.xfercount = 0; self.xfer = Xfer::None; }
        v
    }

    fn data_write(&mut self, v: u16) {
        if self.xfer != Xfer::PutSector || self.xfersect >= self.xfernum { return; }
        let b = self.partitions[self.xferpart].blk[(self.xferpos + self.xfersect).min(MAX_BLOCKS - 1)];
        if b == NONE { return; }
        let blk = &mut self.blocks[b as usize];
        let o = self.xferoffs.min(SECTOR - 2);
        blk.data[o..o + 2].copy_from_slice(&v.to_be_bytes());
        self.xferdnum += 2;
        self.xferoffs += 2;
        if self.xferoffs as i32 >= blk.size { self.xferoffs = 0; self.xfersect += 1; }
    }

    fn delete_transferred(&mut self) {
        let part = self.xferpart;
        for i in self.xferpos..(self.xferpos + self.xfernum).min(MAX_BLOCKS) {
            let b = self.partitions[part].blk[i];
            self.free_block(b);
            self.partitions[part].blk[i] = NONE;
        }
        self.defrag(part);
        let p = &mut self.partitions[part];
        p.size -= self.xferdnum as i32;
        p.numblks = p.numblks.saturating_sub(self.xfernum);
        if self.freeblocks == MAX_BLOCKS { self.sectorstore = false; }
    }

    // ---------------- buffers ----------------

    fn alloc_block(&mut self) -> u8 {
        for i in 0..MAX_BLOCKS {
            if self.blocks[i].size == -1 {
                self.freeblocks -= 1;
                if self.freeblocks == 0 { self.buffull = true; }
                self.blocks[i].size = self.sectlenin as i32;
                return i as u8;
            }
        }
        self.buffull = true;
        NONE
    }

    fn free_block(&mut self, b: u8) {
        if b == NONE || self.blocks[b as usize].size == -1 { return; }
        self.blocks[b as usize].size = -1;
        self.freeblocks += 1;
        self.buffull = false;
        self.hirq &= !BFUL;
    }

    fn defrag(&mut self, part: usize) {
        let p = &mut self.partitions[part];
        let kept: Vec<u8> = p.blk.iter().copied().filter(|&b| b != NONE).collect();
        p.blk.fill(NONE);
        p.blk[..kept.len()].copy_from_slice(&kept);
    }

    fn clear_partition(&mut self, part: usize) {
        for i in 0..MAX_BLOCKS {
            let b = self.partitions[part].blk[i];
            self.free_block(b);
        }
        self.partitions[part] = Partition::new();
    }

    /// Sector offset / count: FFFF means "the last one" / "all to the end"
    fn sector_range(&self, part: usize, offs: usize, num: usize) -> (usize, usize) {
        let n = self.partitions[part].numblks;
        if offs == 0xFFFF { (n.saturating_sub(1), num.min(1).max(1)) }
        else if num == 0xFFFF { (offs, n.saturating_sub(offs)) }
        else { (offs, num) }
    }

    // ---------------- status ----------------

    fn change_status(&mut self, s: u16) {
        self.stat = STAT_BUSY;
        self.next_stat = s;
        self.changing = true;
    }

    /// The usual answer: status, track, index, FAD
    fn standard(&mut self, cur: u16) {
        if !self.has_disc() {
            self.cr[0] = self.stat | (self.cr[0] & 0xFF);
            return;
        }
        let fad = if self.stat & 0x0F00 == STAT_SEEK { self.fad_seek } else { self.curfad };
        let t = if self.stat & 0x0F00 == STAT_SEEK || self.cur_track != NONE { self.track_of(fad) } else { None };
        self.cr[0] = cur | (self.playtype as u16) << 7 | (self.repeat as u16 & 0xF);
        self.cr[1] = t.map_or(0xFFFF, |t| self.adr_control(t) << 8 | (t as u16 + 1));
        self.cr[2] = self.track_index(fad) << 8 | (fad >> 16) as u16 & 0xFF;
        self.cr[3] = fad as u16;
    }

    fn mpeg_standard(&mut self) { self.cr = [self.stat | 1, 0, 0x10, 0x1000]; }

    /// Time passes (picoseconds)
    pub fn tick(&mut self, ps: i64) {
        if self.cmd_left > 0 {
            self.cmd_left -= ps;
            if self.cmd_left <= 0 {
                if self.changing {
                    self.cmd_left = 60 * US;
                } else {
                    self.cmd_left = 0;
                    if self.written == 0xF && self.hirq & CMOK == 0 { self.command(); }
                }
            }
        }
        self.sector_left -= ps;
        if self.sector_left <= 0 {
            let audio = self.track_of(self.curfad).is_some_and(|t| self.is_audio(t));
            let rate = if audio { 75 } else { 75 * self.speed.max(1) as i64 };
            self.sector_left += 1_000_000 * US / rate;
            self.play();
            if !self.buffull { self.hirq |= SCDQ; } else { self.hirq &= !SCDQ; }
            if self.stat & STAT_PERI != 0 { let s = self.stat; self.standard(s); }
        }
    }

    /// One sector time (MAME cd_playdata)
    fn play(&mut self) {
        match self.stat & 0x0F00 {
            STAT_BUSY => {
                self.stat = self.next_stat;
                self.changing = false;
            }
            STAT_SEEK => {
                if !self.has_disc() { return; }
                let step = 75 * 10 * self.speed.max(1) as i64;
                let diff = self.fad_seek as i64 - self.curfad as i64;
                if diff > step { self.curfad += step as u32; }
                else if diff < -step { self.curfad -= step as u32; }
                else {
                    self.curfad = self.fad_seek;
                    self.cur_track = self.track_of(self.curfad).map_or(NONE, |t| t as u8);
                    let s = self.seek_stat;
                    self.change_status(s);
                }
            }
            STAT_PAUSE => {
                if self.buffull_pause && !self.buffull && self.fadstoplay != 0 {
                    self.buffull_pause = false;
                    self.change_status(STAT_PLAY);
                }
            }
            STAT_PLAY => {
                if !self.has_disc() || self.fadstoplay == 0 { return; }
                let audio = self.track_of(self.curfad).is_some_and(|t| self.is_audio(t));
                let ok = if audio {
                    if let Some(raw) = self.read_raw(self.curfad) {
                        for f in raw.chunks_exact(4) {
                            if self.audio.len() < 44100 { self.audio.push_back((i16::from_le_bytes([f[0], f[1]]), i16::from_le_bytes([f[2], f[3]]))); }
                        }
                    }
                    true
                } else {
                    self.read_filtered()
                };
                if ok {
                    self.curfad += 1;
                    self.fadstoplay -= 1;
                    self.hirq |= CSCT;
                    self.sectorstore = true;
                    if self.fadstoplay == 0 {
                        if self.repeat >= self.maxrepeat {
                            self.change_status(STAT_PAUSE);
                            self.hirq |= PEND;
                            if self.playtype == 1 { self.hirq |= EFLS; }
                        } else {
                            if self.repeat < 0xE { self.repeat += 1; }
                            let t = self.cur_track as usize;
                            self.fad_seek = self.track_start(t) + 150;
                            self.fadstoplay = self.track_start(t + 1).saturating_sub(self.track_start(t));
                            self.change_status(STAT_SEEK);
                            self.seek_stat = STAT_PLAY;
                        }
                    }
                } else if self.buffull {
                    self.buffull_pause = true;
                    self.change_status(STAT_PAUSE);
                }
            }
            _ => {}
        }
    }

    /// Read the sector at curfad through the CD device's filter into a partition
    fn read_filtered(&mut self) -> bool {
        if self.device == NONE || self.buffull { return false; }
        let Some(raw) = self.read_raw(self.curfad) else { return false };
        let mode2 = raw[15] == 2;
        let mut size = self.sectlenin;
        let (fnum, chan, subm, cinf) = if mode2 { (raw[16], raw[17], raw[18], raw[19]) } else { (0, 0, 0, 0) };
        if mode2 && subm & 0x20 != 0 && size == 2048 { size = 2324; }
        // the filters: up to 3 tries through the false connections
        let mut f = self.device as usize;
        let mut tries = 2;
        self.lastbuf = self.filters[f].condtrue;
        loop {
            let flt = self.filters[f];
            let mut m = true;
            if flt.mode & 0x40 != 0 && (self.curfad < flt.fad || self.curfad as u64 >= flt.fad as u64 + flt.range as u64) { m = false; }
            if mode2 {
                if flt.mode & 1 != 0 && fnum != flt.fid { m = false; }
                if flt.mode & 2 != 0 && chan != flt.chan { m = false; }
                if flt.mode & 4 != 0 && subm & flt.smmask != flt.smval { m = false; }
                if flt.mode & 8 != 0 && cinf & flt.cimask != flt.cival { m = false; }
                if flt.mode & 0x10 != 0 { m = !m; }
            }
            if m { self.lastbuf = flt.condtrue; break; }
            self.lastbuf = flt.condfalse;
            if self.lastbuf == NONE || tries == 0 || self.lastbuf as usize >= MAX_FILTERS { return false; }
            f = self.lastbuf as usize;
            tries -= 1;
        }
        let part = self.lastbuf as usize;
        if part >= MAX_FILTERS { return false; }
        let b = self.alloc_block();
        if b == NONE { return false; }
        let at = match size {
            2048 => if mode2 { 24 } else { 16 },
            2324 => 24,
            2336 => 16,
            2340 => 12,
            _ => 0,
        };
        let blk = &mut self.blocks[b as usize];
        blk.size = size as i32;
        blk.fad = self.curfad;
        blk.fnum = fnum;
        blk.chan = chan;
        blk.subm = subm;
        blk.cinf = cinf;
        blk.data[..SECTOR - at].copy_from_slice(&raw[at..]);
        let p = &mut self.partitions[part];
        if p.numblks >= MAX_BLOCKS { self.free_block(b); return false; }
        p.blk[p.numblks] = b;
        if p.size == -1 { p.size = 0; }
        p.size += size as i32;
        p.numblks += 1;
        true
    }

    // ---------------- the file system ----------------

    fn read_new_dir(&mut self, fileno: u32) {
        if fileno == 0xFF_FFFF {
            // the primary volume descriptor from sector 16
            for fad in 166..200 {
                let s = self.read_block(fad);
                if &s[1..6] != b"CD001" { continue; }
                if s[0] == 0xFF { break; }
                if s[0] == 1 {
                    let le = |o: usize| u32::from_le_bytes([s[o], s[o + 1], s[o + 2], s[o + 3]]);
                    self.root = DirEntry { firstfad: le(158) + 150, length: le(166), flags: s[181], ..Default::default() };
                    let r = self.root.clone();
                    self.make_dir_current(r.firstfad, r.length);
                    return;
                }
            }
        } else if let Some(e) = self.dir.get(fileno as usize).cloned() {
            self.make_dir_current(e.firstfad, e.length);
        }
    }

    fn make_dir_current(&mut self, fad: u32, length: u32) {
        let len = (length as usize).clamp(2048, MAX_DIR_SIZE);
        let mut sect = vec![0u8; MAX_DIR_SIZE];
        for i in 0..len / 2048 {
            let b = self.read_block(fad + i as u32);
            sect[i * 2048..i * 2048 + 2048].copy_from_slice(&b);
        }
        let mut dir = Vec::new();
        let mut at = 0usize;
        let mut sector = 0usize;
        while at < len && dir.len() < 4096 {
            let n = sect[at] as usize;
            if n == 0 {
                sector += 0x800;
                if sector >= len { break; }
                at = sector;
                continue;
            }
            let le = |o: usize| u32::from_le_bytes([sect[o], sect[o + 1], sect[o + 2], sect[o + 3]]);
            dir.push(DirEntry {
                firstfad: le(at + 2) + 150, length: le(at + 10), flags: sect[at + 25],
                file_unit_size: sect[at + 26], interleave_gap: sect[at + 27],
            });
            at += n;
        }
        self.firstfile = dir.iter().position(|e| e.flags & 2 == 0).unwrap_or(0);
        self.dir = dir;
    }

    fn read_toc(&mut self) {
        let n = self.ntracks();
        let mut toc = vec![0xFFu8; 102 * 4];
        for t in 0..n.min(99) {
            let fad = self.track_start(t) + 150;
            toc[t * 4] = self.adr_control(t) as u8;
            toc[t * 4 + 1..t * 4 + 4].copy_from_slice(&fad.to_be_bytes()[1..]);
        }
        let first = toc[0];
        let last = if n > 0 { toc[(n - 1) * 4] } else { 0xFF };
        let p = 99 * 4;
        toc[p..p + 4].copy_from_slice(&[first, 1, 0, 0]);
        toc[p + 4..p + 8].copy_from_slice(&[last, n as u8, 0, 0]);
        let lo = self.leadout() + 150;
        toc[p + 8] = first;
        toc[p + 9..p + 12].copy_from_slice(&lo.to_be_bytes()[1..]);
        self.toc = toc;
        self.xfer = Xfer::Toc;
        self.xfercount = 0;
    }

    // ---------------- commands ----------------

    fn command(&mut self) {
        self.written = 0;
        let cr = self.cr;
        let cmd = (cr[0] >> 8) as u8;
        if cmd != 0 && self.log.len() < 256 { self.log.push(cr); }
        self.stat &= !STAT_PERI;
        let st = self.stat;
        let fnum = (cr[2] >> 8) as usize;
        match cmd {
            0x00 => { self.standard(st); self.hirq |= CMOK; }
            0x01 => { self.cr = [st, 0x0201, 0, 0x0400]; self.hirq |= CMOK; }
            0x02 => {
                self.read_toc();
                self.stat |= STAT_TRANS;
                self.cr = [self.stat, 102 * 2, 0, 0];
                self.xferdnum = 0;
                self.hirq |= CMOK | DRDY;
            }
            0x03 => {
                if !self.has_disc() { self.cr = [st, 0, 0, 0]; }
                else {
                    self.read_toc();
                    self.xfer = Xfer::None;
                    match cr[0] & 0xFF {
                        0 => {
                            self.change_status(STAT_PAUSE);
                            let lo = self.leadout() + 150;
                            self.cr = [self.stat, 0, 0x0100 | (lo >> 16) as u16 & 0xFF, lo as u16];
                        }
                        1 => { self.change_status(STAT_PAUSE); self.cr = [self.stat, 0, 0x0100, 0]; }
                        _ => self.cr = [self.stat, 0, 0, 0],
                    }
                }
                self.hirq |= CMOK;
            }
            0x04 => {
                if self.has_disc() {
                    self.fad_seek = 150;
                    self.change_status(STAT_SEEK);
                    self.seek_stat = STAT_PAUSE;
                    self.fadstoplay = 0;
                }
                self.buffull = false;
                self.buffull_pause = false;
                self.hirq &= 0xFFE5;
                self.speed = if cr[0] & 0x10 != 0 { 1 } else { 2 };
                self.hirq |= CMOK | ESEL | EFLS | ECPY | EHST;
                let s = self.stat;
                self.standard(s);
            }
            0x06 => {
                self.stat &= !STAT_TRANS;
                let st = self.stat;
                self.cr = if self.xferdnum != 0 { [st | ((self.xferdnum >> 17) & 0xFF) as u16, (self.xferdnum >> 1) as u16, 0, 0] } else { [st | 0xFF, 0xFFFF, 0, 0] };
                match self.xfer {
                    Xfer::GetSector | Xfer::PutSector => self.hirq |= EHST,
                    Xfer::GetDeleteSector => {
                        if self.partitions[self.xferpart].size > 0 {
                            self.delete_transferred();
                            self.hirq |= EHST;
                        }
                        self.xfer = Xfer::None;
                    }
                    _ => {}
                }
                self.xferdnum = 0;
                self.hirq |= CMOK;
            }
            0x10 => self.cmd_play(),
            0x11 => self.cmd_seek(),
            0x12 => { self.hirq |= CMOK; self.standard(st); }
            0x20 => self.cmd_subcode(),
            0x30 => {
                let p = (cr[2] >> 8) as u8;
                self.device = if p as usize >= MAX_FILTERS { NONE } else { p };
                self.hirq |= CMOK | ESEL;
                self.standard(st);
            }
            0x31 => { self.cr = [st, 0, (self.device as u16) << 8, 0]; self.hirq |= CMOK; }
            0x32 => { self.cr = [st, 0, (self.lastbuf as u16) << 8, 0]; self.hirq |= CMOK; }
            0x40 => {
                if fnum < MAX_FILTERS {
                    self.filters[fnum].fad = ((cr[0] as u32 & 0xFF) << 16) | cr[1] as u32;
                    self.filters[fnum].range = ((cr[2] as u32 & 0xFF) << 16) | cr[3] as u32;
                }
                self.hirq |= CMOK | ESEL;
                self.standard(st);
            }
            0x41 => {
                let f = self.filters.get(fnum).copied().unwrap_or_default();
                self.cr = [st | ((f.fad >> 16) & 0xFF) as u16, f.fad as u16, ((f.range >> 16) & 0xFF) as u16, f.range as u16];
                self.hirq |= CMOK;
            }
            0x42 => {
                if let Some(f) = self.filters.get_mut(fnum) {
                    f.chan = cr[0] as u8;
                    f.smmask = (cr[1] >> 8) as u8;
                    f.cimask = cr[1] as u8;
                    f.fid = cr[2] as u8;
                    f.smval = (cr[3] >> 8) as u8;
                    f.cival = cr[3] as u8;
                }
                self.hirq |= CMOK | ESEL;
                self.standard(st);
            }
            0x43 => {
                let f = self.filters.get(fnum).copied().unwrap_or_default();
                self.cr = [st | f.chan as u16, (f.smmask as u16) << 8 | f.cimask as u16, f.fid as u16, (f.smval as u16) << 8 | f.cival as u16];
                self.hirq |= CMOK | ESEL;
            }
            0x44 => {
                let mode = cr[0] as u8;
                if let Some(f) = self.filters.get_mut(fnum) {
                    if mode & 0x80 != 0 { *f = Filter { condtrue: f.condtrue, condfalse: f.condfalse, ..Filter::default() }; } else { f.mode = mode; }
                }
                self.hirq |= CMOK | ESEL;
                self.standard(st);
            }
            0x45 => {
                let m = self.filters.get(fnum).map_or(0, |f| f.mode);
                self.cr = [st | m as u16, 0, 0, 0];
                self.hirq |= CMOK | ESEL;
            }
            0x46 => {
                if let Some(f) = self.filters.get_mut(fnum) {
                    if cr[0] & 1 != 0 { f.condtrue = (cr[1] >> 8) as u8; }
                    if cr[0] & 2 != 0 { f.condfalse = cr[1] as u8; }
                }
                self.hirq |= CMOK | ESEL;
                self.standard(st);
            }
            0x48 => self.cmd_reset_selector(),
            0x50 => { self.cr = [st, self.freeblocks.min(MAX_BLOCKS) as u16, 0x1800, MAX_BLOCKS as u16]; self.hirq |= CMOK; }
            0x51 => {
                let n = self.partitions.get(fnum).map_or(0, |p| if p.size == -1 { 0 } else { p.numblks });
                self.cr = [st, 0, 0, n as u16];
                self.hirq |= CMOK;
            }
            0x52 => {
                self.calcsize = 0;
                if fnum < MAX_FILTERS && self.partitions[fnum].size != -1 {
                    for i in 0..cr[3] as usize {
                        let b = *self.partitions[fnum].blk.get(cr[1] as usize + i).unwrap_or(&NONE);
                        if b != NONE { self.calcsize += self.blocks[b as usize].size as u32 / 2; }
                    }
                }
                self.hirq |= CMOK | ESEL;
                self.standard(st);
            }
            0x53 => { self.cr = [st | ((self.calcsize >> 16) & 0xFF) as u16, self.calcsize as u16, 0, 0]; self.hirq |= CMOK; }
            0x54 => {
                let b = if fnum < MAX_FILTERS { *self.partitions[fnum].blk.get(cr[1] as usize).unwrap_or(&NONE) } else { NONE };
                if b == NONE {
                    self.standard(STAT_REJECT);
                    self.hirq |= CMOK;
                } else {
                    let k = &self.blocks[b as usize];
                    self.cr = [st | ((k.fad >> 16) & 0xFF) as u16, k.fad as u16, (k.fnum as u16) << 8 | k.chan as u16, (k.subm as u16) << 8 | k.cinf as u16];
                    self.hirq |= CMOK | ESEL;
                }
            }
            0x60 => {
                let len = |v: u16| [2048, 2336, 2340, 2352].get(v as usize).copied();
                if let Some(l) = len(cr[0] & 0xFF) { self.sectlenin = l; }
                if let Some(l) = len(cr[1] >> 8) { self.sectlenout = l; }
                self.hirq |= CMOK | ESEL;
                self.standard(st);
            }
            0x61 | 0x63 => {
                if fnum >= MAX_FILTERS { self.standard(STAT_REJECT); self.hirq |= CMOK; return self.answered(cmd); }
                let (offs, num) = self.sector_range(fnum, cr[1] as usize, cr[3] as usize);
                if self.partitions[fnum].numblks < num || num == 0 && self.partitions[fnum].numblks == 0 {
                    self.standard(st | STAT_WAIT);
                    self.hirq |= CMOK;
                    return self.answered(cmd);
                }
                self.xfer = if cmd == 0x61 { Xfer::GetSector } else { Xfer::GetDeleteSector };
                self.xferoffs = 0;
                self.xfersect = 0;
                self.xferdnum = 0;
                self.xferpos = offs;
                self.xfernum = num;
                self.xferpart = fnum;
                self.stat |= STAT_TRANS;
                let s = self.stat;
                self.standard(s);
                self.hirq |= CMOK | EHST | DRDY;
            }
            0x62 => {
                if fnum >= MAX_FILTERS { self.standard(STAT_REJECT); self.hirq |= CMOK; return self.answered(cmd); }
                if self.partitions[fnum].numblks == 0 { self.standard(st | STAT_WAIT); self.hirq |= CMOK; return self.answered(cmd); }
                let (offs, num) = self.sector_range(fnum, cr[1] as usize, cr[3] as usize);
                for i in offs..(offs + num).min(MAX_BLOCKS) {
                    let b = self.partitions[fnum].blk[i];
                    if b != NONE && self.partitions[fnum].size > 0 {
                        self.partitions[fnum].size -= self.blocks[b as usize].size;
                    }
                    self.free_block(b);
                    self.partitions[fnum].blk[i] = NONE;
                }
                self.defrag(fnum);
                let p = &mut self.partitions[fnum];
                p.numblks = p.numblks.saturating_sub(num);
                if self.freeblocks == MAX_BLOCKS { self.sectorstore = false; }
                self.stat &= !STAT_TRANS;
                let s = self.stat;
                self.standard(s);
                self.hirq |= CMOK | EHST;
            }
            0x64 => {
                if fnum >= MAX_FILTERS { self.standard(STAT_REJECT); self.hirq |= CMOK; return self.answered(cmd); }
                let num = (cr[3] & 0xFF) as usize;
                let offs = self.partitions[fnum].numblks;
                self.xfer = Xfer::PutSector;
                self.stat |= STAT_TRANS;
                self.xferoffs = 0;
                self.xfersect = 0;
                self.xferdnum = 0;
                self.xferpos = offs;
                self.xfernum = num;
                self.xferpart = fnum;
                for i in offs..(offs + num).min(MAX_BLOCKS) {
                    let b = self.alloc_block();
                    if b == NONE { break; }
                    let p = &mut self.partitions[fnum];
                    p.blk[i] = b;
                    if p.size == -1 { p.size = 0; }
                    p.size += self.sectlenin as i32;
                    p.numblks += 1;
                }
                self.hirq |= CMOK | DRDY;
                let s = self.stat;
                self.standard(s);
            }
            0x65 => { self.hirq |= CMOK; self.standard(st); }
            0x66 => {
                let (src, dst) = (fnum, (cr[0] & 0xFF) as usize);
                if src >= MAX_FILTERS || dst >= MAX_FILTERS { self.standard(STAT_REJECT); self.hirq |= CMOK; return self.answered(cmd); }
                let (offs, num) = self.sector_range(src, cr[1] as usize, cr[3] as usize);
                for i in offs..(offs + num).min(MAX_BLOCKS) {
                    let b = self.partitions[src].blk[i];
                    if b == NONE || self.partitions[dst].numblks >= MAX_BLOCKS { continue; }
                    let size = self.blocks[b as usize].size;
                    let d = &mut self.partitions[dst];
                    d.blk[d.numblks] = b;
                    if d.size == -1 { d.size = 0; }
                    d.size += size;
                    d.numblks += 1;
                    let s = &mut self.partitions[src];
                    s.size -= size;
                    s.blk[i] = NONE;
                    s.numblks = s.numblks.saturating_sub(1);
                }
                self.defrag(src);
                self.hirq |= CMOK | ECPY;
                self.standard(st);
            }
            0x67 => { self.cr = [st, 0, 0, 0]; self.hirq |= CMOK; }
            0x70 => {
                self.hirq |= CMOK | EFLS;
                let id = ((cr[2] as u32 & 0xFF) << 16) | cr[3] as u32;
                self.read_new_dir(id);
                self.standard(st);
            }
            0x71 => {
                self.device = if fnum < MAX_FILTERS { fnum as u8 } else { NONE };
                self.standard(st);
                self.hirq |= CMOK | EFLS;
            }
            0x72 => {
                self.hirq |= CMOK | EFLS;
                self.cr = [st, self.dir.len() as u16, 0x0100, self.firstfile as u16];
            }
            0x73 => self.cmd_file_info(),
            0x74 => self.cmd_read_file(),
            0x75 => {
                self.hirq |= CMOK | EFLS;
                self.sectorstore = false;
                if matches!(self.xfer, Xfer::GetSector | Xfer::GetDeleteSector | Xfer::PutSector) { self.xfer = Xfer::None; }
                self.xferdnum = 0;
                if self.has_disc() { self.change_status(STAT_PAUSE); }
                let s = self.stat;
                self.standard(s);
            }
            0x90 | 0x92 | 0x94 => { self.mpeg_standard(); self.hirq |= CMOK; }
            0x91 => { self.cr = [st, 5, 0, 0]; self.hirq |= CMOK; }
            0x93 => {
                self.hirq |= CMOK | MPED;
                if cr[1] == 1 { self.hirq |= MPCM; }
                self.cr = [st, 0, 0, 0];
            }
            0xE0 => {
                if self.has_disc() { self.change_status(STAT_PAUSE); }
                if cr[1] == 1 { self.hirq |= CMOK | MPED; } else if self.has_disc() {
                    self.sectorstore = true;
                    self.hirq = 0x07C5;
                } else {
                    self.hirq |= CMOK | EFLS;
                }
                let s = self.stat;
                self.standard(s);
            }
            0xE1 => {
                if self.has_disc() { self.change_status(STAT_PAUSE); }
                let region = if cr[1] == 1 { 2 } else if self.has_disc() { 4 } else { 0 };
                self.cr = [self.stat, region, 0, 0];
                self.hirq |= CMOK;
            }
            0xE2 => { self.standard(st); self.hirq |= CMOK | MPED; }
            _ => { self.standard(st); self.hirq |= CMOK; }
        }
        self.answered(cmd);
    }

    fn answered(&mut self, cmd: u8) {
        if cmd != 0 && self.answers.len() < 64 {
            self.answers.push(format!("{:02X}->{:04X} {:04X} {:04X} {:04X} hirq {:04X}", cmd, self.cr[0], self.cr[1], self.cr[2], self.cr[3], self.hirq));
        }
    }

    fn cmd_play(&mut self) {
        let cr = self.cr;
        let mode = (cr[2] >> 8) & 0x7F;
        let start = ((cr[0] as u32 & 0xFF) << 16) | cr[1] as u32;
        let end = ((cr[2] as u32 & 0xFF) << 16) | cr[3] as u32;
        if cr[2] & 0x8000 == 0 {
            if start & 0x80_0000 != 0 {
                if start != 0xFF_FFFF {
                    self.fad_seek = start & 0x7F_FFFF;
                    self.change_status(STAT_SEEK);
                    self.seek_stat = STAT_PLAY;
                }
                self.cur_track = self.track_of(self.curfad).map_or(NONE, |t| t as u8);
            } else if start >> 8 != 0 {
                self.cur_track = ((start >> 8) as u8).saturating_sub(1);
                self.fad_seek = self.track_start(self.cur_track as usize) + 150;
                self.change_status(STAT_SEEK);
                self.seek_stat = STAT_PLAY;
            } else {
                let s = self.stat;
                self.standard(s);
                self.hirq |= CMOK;
                return;
            }
            if end & 0x80_0000 != 0 {
                if end != 0xFF_FFFF { self.fadstoplay = end & 0x7F_FFFF; }
            } else {
                // up to the start of the track after the end track
                let et = (end >> 8) as usize;
                self.fadstoplay = (self.track_start(et) + 150).saturating_sub(self.fad_seek);
            }
        } else if start != 0xFF_FFFF {
            self.fadstoplay = if end & 0x80_0000 != 0 { end & 0xF_FFFF } else if end == 0 {
                (self.leadout() + 150).saturating_sub(self.curfad)
            } else {
                (self.track_start(((end & 0xFF00) >> 8) as usize) + 150).saturating_sub(self.curfad)
            };
            self.change_status(STAT_SEEK);
            self.seek_stat = STAT_PLAY;
        } else if self.fadstoplay == 0 {
            let t = self.cur_track as usize;
            self.fadstoplay = (self.track_start(t + 1) + 150).saturating_sub(self.curfad);
            self.change_status(STAT_SEEK);
            self.seek_stat = STAT_PLAY;
        }
        let s = self.stat;
        self.standard(s);
        self.hirq |= CMOK;
        self.playtype = 0;
        self.maxrepeat = if mode != 0x7F { (mode & 0xF) as u8 } else { 0 };
        self.repeat = 0;
    }

    fn cmd_seek(&mut self) {
        let cr = self.cr;
        self.fadstoplay = 0;
        self.repeat = 0;
        self.playtype = 0;
        if cr[0] & 0x80 != 0 {
            let fad = ((cr[0] as u32 & 0xFF) << 16) | cr[1] as u32;
            self.fad_seek = if fad == 0xFF_FFFF { self.curfad } else { ((cr[0] as u32 & 0x7F) << 16) | cr[1] as u32 };
            self.change_status(STAT_SEEK);
            self.seek_stat = STAT_PAUSE;
        } else if cr[1] >> 8 != 0 {
            self.cur_track = ((cr[1] >> 8) as u8).saturating_sub(1);
            self.fad_seek = self.track_start(self.cur_track as usize) + 150;
            self.change_status(STAT_SEEK);
            self.seek_stat = STAT_PAUSE;
        } else {
            self.change_status(STAT_STANDBY);
            self.curfad = 0xFFFF_FFFF;
            self.cur_track = NONE;
            self.audio.clear();
        }
        self.hirq |= CMOK;
        let s = self.stat;
        self.standard(s);
    }

    fn cmd_subcode(&mut self) {
        self.stat |= STAT_TRANS;
        let st = self.stat & 0xFF00;
        match self.cr[0] & 0xFF {
            0 => {
                self.cr = [st, 5, 0, 0];
                let t = self.track_of(self.curfad).unwrap_or(0);
                let abs = self.curfad.saturating_sub(150);
                let rel = abs.saturating_sub(self.track_start(t));
                let msf = |l: u32| (l / 4500, (l / 75) % 60, l % 75);
                let (am, as_, af) = msf(abs + 150);
                let (rm, rs, rf) = msf(rel);
                let ctl = if self.is_audio(t) { 0x01 } else { 0x41 };
                let idx = bcd(self.track_index(self.curfad) as u32);
                self.small[..10].copy_from_slice(&[ctl, bcd(t as u32 + 1), idx, bcd(rm), bcd(rs), bcd(rf), 0, bcd(am), bcd(as_), bcd(af)]);
                self.xfer = Xfer::SubQ;
            }
            _ => {
                self.cr = [st, 12, 0, 0];
                self.small[..24].fill(0xFF);
                self.xfer = Xfer::SubRw;
            }
        }
        self.xfercount = 0;
        self.hirq |= CMOK | DRDY;
    }

    fn cmd_reset_selector(&mut self) {
        let cr = self.cr;
        let st = self.stat;
        if cr[0] & 0xFF == 0 {
            let b = (cr[2] >> 8) as usize;
            if b < MAX_FILTERS { self.clear_partition(b); }
            if self.freeblocks == MAX_BLOCKS { self.sectorstore = false; }
            self.hirq |= CMOK | ESEL;
            self.standard(st);
            return;
        }
        if cr[0] & 0x04 != 0 {
            for i in 0..MAX_FILTERS { self.clear_partition(i); }
            self.buffull = false;
            self.sectorstore = false;
            self.buffull_pause = false;
        }
        if cr[0] & 0x10 != 0 {
            for f in self.filters.iter_mut() {
                *f = Filter { condtrue: f.condtrue, condfalse: f.condfalse, ..Filter::default() };
            }
        }
        if cr[0] & 0x20 != 0 {
            for i in 0..MAX_FILTERS {
                if i == self.device as usize { self.device = NONE; }
                if (self.filters[i].condfalse as usize) < MAX_FILTERS { self.filters[i].condfalse = NONE; }
            }
        }
        if cr[0] & 0x40 != 0 { for (i, f) in self.filters.iter_mut().enumerate() { f.condtrue = i as u8; } }
        if cr[0] & 0x80 != 0 { for f in self.filters.iter_mut() { f.condfalse = NONE; } }
        self.hirq |= CMOK | ESEL;
        self.standard(st);
    }

    fn cmd_file_info(&mut self) {
        self.stat |= STAT_TRANS;
        self.stat &= 0xFF00;
        self.playtype = 0;
        self.repeat = 0;
        self.hirq |= CMOK | DRDY;
        let id = ((self.cr[2] as u32 & 0xFF) << 16) | self.cr[3] as u32;
        self.xfercount = 0;
        if id == 0xFF_FFFF {
            self.xfer = Xfer::FileInfo254;
            self.cr = [self.stat, 0x5F4, 0, 0];
        } else {
            let e = self.dir.get(id as usize).cloned().unwrap_or_default();
            self.small[0..4].copy_from_slice(&e.firstfad.to_be_bytes());
            self.small[4..8].copy_from_slice(&e.length.to_be_bytes());
            self.small[8] = e.interleave_gap;
            self.small[9] = e.file_unit_size;
            self.small[10] = id as u8;
            self.small[11] = e.flags;
            self.xfer = Xfer::FileInfo1;
            self.cr = [self.stat, 6, 0, 0];
        }
    }

    fn cmd_read_file(&mut self) {
        let cr = self.cr;
        let offset = ((cr[0] as u32 & 0xFF) << 16) | cr[1] as u32;
        let id = (((cr[2] as u32 & 0xFF) << 16) | cr[3] as u32) as usize;
        let filter = (cr[2] >> 8) as usize;
        if filter >= MAX_FILTERS || id >= self.dir.len() {
            self.standard(STAT_REJECT);
            self.hirq |= CMOK;
            return;
        }
        let e = self.dir[id].clone();
        let size = (e.length as usize).div_ceil(self.sectlenin) as u32;
        self.curfad = (e.firstfad + offset) & 0xFF_FFFF;
        self.fadstoplay = size.saturating_sub(offset);
        self.device = filter as u8;
        self.change_status(STAT_PLAY | 0x80);
        let s = self.stat;
        self.standard(s);
        self.playtype = 1;
        self.hirq |= CMOK | EHST;
    }
}
