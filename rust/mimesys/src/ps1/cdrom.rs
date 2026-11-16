//! The CD-ROM controller (1F801800-1F801803), from Nocash PSX-SPX.
//!
//! - 1F801800: index (bits 0-1) and status: bit 3 parameter FIFO empty, 4 parameter FIFO
//!   not full, 5 response FIFO not empty, 6 data FIFO not empty, 7 busy.
//! - Index 0: 1801 command, 1802 parameter, 1803 request (bit 7 loads the sector buffer
//!   into the data FIFO). Index 1: 1802 interrupt enable, 1803 acknowledge. 1801 reads the
//!   response FIFO, 1802 the data FIFO, 1803 the enable / flag registers.
//! - Answers: INT3 acknowledge, INT2 second "complete" answer, INT1 a sector (or a CD-DA
//!   report) ready, INT4 end of disc, INT5 error. One interrupt at a time: the next waits
//!   for the CPU to acknowledge the last.
//! - The status byte: bit 1 motor on, 4 shell open, 5 reading, 6 seeking, 7 playing.
//! - Reading: Setloc, then ReadN / ReadS seek and deliver sectors at 75 a second (150 in
//!   double speed). Mode bit 5 picks 2048 bytes of user data or 2340 (all but the sync).
//!   With XA-ADPCM on (mode bit 6), audio sectors go to the decoder (stage 6), not the CPU.
//!   Sectors queue in the drive's buffer (8 here) until the CPU acknowledges each INT1;
//!   the current sector (data FIFO, GetlocL) changes only when its INT1 is raised.
//! - Play (CD-DA) moves the position along and answers GetlocP; the sound is stage 6.
//! - GetID reports a licensed disc with the BIOS's own region, so every region's discs
//!   start (a modchip, in effect).

use crate::common::disc::{Disc, LEAD_IN, SECTOR};
use serde::{Deserialize, Serialize};

const STAT_MOTOR: u8 = 0x02;
const STAT_SHELL_OPEN: u8 = 0x10;
const STAT_READING: u8 = 0x20;
const STAT_SEEKING: u8 = 0x40;
const STAT_PLAYING: u8 = 0x80;

const CPU_HZ: i64 = 33_868_800;
/// first answer (INT3) after a command, in CPU cycles
const ACK_DELAY: i64 = 25_000;
const SEEK_DELAY: i64 = 150_000;
/// sectors the drive can hold before the CPU takes them
const SECTOR_BUFFERS: usize = 8;

#[derive(Clone, Serialize, Deserialize)]
struct Response {
    delay: i64,
    int: u8,
    bytes: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
enum Drive {
    #[default]
    Idle,
    Seeking { then: After },
    Reading,
    Playing,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum After { Read, Play, Complete }

#[derive(Default, Serialize, Deserialize)]
pub struct Cdrom {
    index: u8,
    params: Vec<u8>,
    response: Vec<u8>,
    resp_pos: usize,
    int_enable: u8,
    int_flag: u8,
    /// the INT3 for the last command, then any second answer
    first: Option<Response>,
    second: Option<Response>,
    stat: u8,
    mode: u8,
    filter: (u8, u8),
    /// Setloc's target, until a seek uses it
    setloc: Option<u32>,
    lba: u32,
    drive: Drive,
    /// cycles until the seek ends / the next sector or report
    timer: i64,
    /// the last sector read (raw) and the data FIFO
    sector: Vec<u8>,
    data: Vec<u8>,
    data_pos: usize,
    /// sectors read but not yet announced (INT1 waits for the last interrupt's
    /// acknowledge); the drive's buffer holds a few, the oldest is lost beyond that
    pending: std::collections::VecDeque<Vec<u8>>,
    report_count: u32,
    region: u8,
    #[serde(skip)]
    pub disc: Option<Disc>,
    pub irq: bool,
    #[serde(skip)]
    logged: u32,
    #[serde(skip)]
    sector_logged: u32,
}

fn bcd(v: u32) -> u8 { ((v / 10) << 4 | (v % 10)) as u8 }
fn from_bcd(v: u8) -> u32 { ((v >> 4) * 10 + (v & 15)) as u32 }

impl Cdrom {
    /// `region`: the BIOS's letter (A, E, J)
    pub fn new(disc: Option<Disc>, region: u8) -> Self {
        let stat = if disc.is_some() { STAT_MOTOR } else { STAT_SHELL_OPEN };
        Self { int_enable: 0x1F, stat, disc, region, sector: vec![0; SECTOR], ..Default::default() }
    }

    /// Bring-up: reading with XA mode on
    pub fn streaming(&self) -> bool { self.mode & 0x40 != 0 && self.drive == Drive::Reading }

    /// Bring-up: data FIFO position / size and the current sector's LBA
    pub fn fifo_state(&self) -> String {
        let m = (self.sector[12] >> 4) as u32 * 10 + (self.sector[12] & 15) as u32;
        let s = (self.sector[13] >> 4) as u32 * 10 + (self.sector[13] & 15) as u32;
        let f = (self.sector[14] >> 4) as u32 * 10 + (self.sector[14] & 15) as u32;
        format!("{}/{} sector {} first {:02X?}", self.data_pos, self.data.len(), ((m * 60 + s) * 75 + f).wrapping_sub(150) as i32, &self.data[self.data_pos.min(self.data.len())..(self.data_pos + 4).min(self.data.len())])
    }

    pub fn debug(&self) -> String {
        let drive = match self.drive { Drive::Idle => "idle", Drive::Seeking { .. } => "seeking", Drive::Reading => "reading", Drive::Playing => "playing" };
        format!("drive {drive} lba {} timer {} mode {:02X} stat {:02X} int_flag {} int_enable {:02X} first {:?} second {:?} pending {} data {}/{} index {} params {:02X?}",
            self.lba, self.timer, self.mode, self.stat(), self.int_flag, self.int_enable,
            self.first.as_ref().map(|r| (r.int, r.delay)), self.second.as_ref().map(|r| (r.int, r.delay)),
            self.pending.len(), self.data_pos, self.data.len(), self.index, self.params)
    }

    fn stat(&self) -> u8 {
        let mut s = self.stat & !(STAT_READING | STAT_SEEKING | STAT_PLAYING);
        match self.drive {
            Drive::Seeking { .. } => s |= STAT_SEEKING,
            Drive::Reading => s |= STAT_READING,
            Drive::Playing => s |= STAT_PLAYING,
            Drive::Idle => {}
        }
        s
    }

    fn sector_cycles(&self) -> i64 { if self.mode & 0x80 != 0 { CPU_HZ / 150 } else { CPU_HZ / 75 } }

    // ------------------------------------------------------------------ registers

    pub fn read(&mut self, a: u32) -> u8 {
        match a & 3 {
            0 => {
                let mut s = self.index;
                if self.params.is_empty() { s |= 0x08; }
                if self.params.len() < 16 { s |= 0x10; }
                if self.resp_pos < self.response.len() { s |= 0x20; }
                if self.data_pos < self.data.len() { s |= 0x40; }
                if self.first.is_some() { s |= 0x80; }
                s
            }
            1 => {
                let v = self.response.get(self.resp_pos).copied().unwrap_or(0);
                if self.resp_pos < self.response.len() { self.resp_pos += 1; }
                v
            }
            2 => self.data_byte(),
            _ => match self.index & 1 {
                0 => self.int_enable | 0xE0,
                _ => self.int_flag | 0xE0,
            },
        }
    }

    fn data_byte(&mut self) -> u8 {
        let v = self.data.get(self.data_pos).copied().unwrap_or(0);
        if self.data_pos < self.data.len() { self.data_pos += 1; }
        v
    }

    /// DMA channel 3: a word from the data FIFO
    pub fn dma_word(&mut self) -> u32 {
        u32::from_le_bytes([self.data_byte(), self.data_byte(), self.data_byte(), self.data_byte()])
    }

    pub fn write(&mut self, a: u32, v: u8) {
        match (a & 3, self.index) {
            (0, _) => self.index = v & 3,
            (1, 0) => self.command(v),
            (2, 0) => if self.params.len() < 16 { self.params.push(v) },
            (3, 0) => {
                // request: bit 7 loads the sector into the data FIFO, clear empties it
                if v & 0x80 != 0 {
                    if self.data_pos >= self.data.len() { self.load_data(); }
                } else {
                    self.data.clear();
                    self.data_pos = 0;
                }
            }
            (2, 1) => self.int_enable = v & 0x1F,
            (3, 1) => {
                self.int_flag &= !(v & 0x1F);
                if v & 0x40 != 0 { self.params.clear(); }
                if self.int_flag == 0 { self.irq = false; }
            }
            _ => {}                                           // volumes: stage 6
        }
    }

    fn load_data(&mut self) {
        self.data = if self.mode & 0x20 != 0 {
            self.sector[12..12 + 0x924].to_vec()
        } else {
            let at = if self.sector[15] == 1 { 16 } else { 24 };
            self.sector[at..at + 0x800].to_vec()
        };
        self.data_pos = 0;
    }

    // ------------------------------------------------------------------ commands

    fn ack(&mut self, bytes: Vec<u8>) {
        self.first = Some(Response { delay: ACK_DELAY, int: 3, bytes });
    }

    fn error(&mut self, code: u8) {
        self.first = Some(Response { delay: ACK_DELAY, int: 5, bytes: vec![self.stat() | 1, code] });
    }

    fn then(&mut self, delay: i64, int: u8, bytes: Vec<u8>) {
        self.second = Some(Response { delay, int, bytes });
    }

    fn command(&mut self, c: u8) {
        // GetStat polls are left out: some games send hundreds
        if self.logged < 400 && c != 0x01 {
            self.logged += 1;
            println!("PS1 CD: command {c:02X} params {:02X?}", self.params);
        }
        let p = std::mem::take(&mut self.params);
        let have_disc = self.disc.is_some();
        // most commands need a disc
        let needs_disc = !matches!(c, 0x01 | 0x0A | 0x0B | 0x0C | 0x0D | 0x0E | 0x0F | 0x19 | 0x1C);
        if needs_disc && !have_disc {
            if c == 0x1A {
                self.ack(vec![self.stat()]);
                self.then(ACK_DELAY * 4, 5, vec![0x08, 0x40, 0, 0, 0, 0, 0, 0]);
            } else {
                self.error(0x80);
            }
            return;
        }
        let arg = |i: usize| p.get(i).copied().unwrap_or(0);
        match c {
            0x01 => {                                         // GetStat
                self.ack(vec![self.stat()]);
                if have_disc { self.stat &= !STAT_SHELL_OPEN; }
            }
            0x02 => {                                         // Setloc mm ss ff (BCD)
                let msf = (from_bcd(arg(0)) * 60 + from_bcd(arg(1))) * 75 + from_bcd(arg(2));
                self.setloc = Some(msf.saturating_sub(LEAD_IN));
                self.ack(vec![self.stat()]);
            }
            0x03 => {                                         // Play [track]
                if let Some(t) = p.first().filter(|&&t| t != 0) {
                    let n = from_bcd(*t) as u8;
                    if let Some(tr) = self.disc.as_ref().and_then(|d| d.tracks.iter().find(|x| x.number == n)) {
                        self.setloc = Some(tr.start);
                    }
                }
                self.start_seek(After::Play);
                self.ack(vec![self.stat() | STAT_PLAYING]);
            }
            0x04 | 0x05 => self.ack(vec![self.stat()]),     // Forward / Backward
            0x06 | 0x1B => {                                  // ReadN / ReadS
                self.start_seek(After::Read);
                self.ack(vec![self.stat()]);
            }
            0x07 => {                                         // MotorOn
                self.stat |= STAT_MOTOR;
                self.ack(vec![self.stat()]);
                self.then(ACK_DELAY * 4, 2, vec![self.stat()]);
            }
            0x08 => {                                         // Stop
                self.drive = Drive::Idle;
                self.ack(vec![self.stat()]);
                self.stat &= !STAT_MOTOR;
                self.then(ACK_DELAY * 20, 2, vec![self.stat()]);
            }
            0x09 => {                                         // Pause
                let was_busy = self.drive != Drive::Idle;
                self.ack(vec![self.stat()]);
                self.drive = Drive::Idle;
                self.pending.clear();
                let d = if was_busy { self.sector_cycles() } else { ACK_DELAY * 4 };
                self.then(d, 2, vec![self.stat()]);
            }
            0x0A => {                                         // Init
                self.mode = 0;
                self.drive = Drive::Idle;
                self.pending.clear();
                self.stat = if have_disc { STAT_MOTOR } else { STAT_SHELL_OPEN };
                self.ack(vec![self.stat()]);
                self.then(ACK_DELAY * 4, 2, vec![self.stat()]);
            }
            0x0B | 0x0C => self.ack(vec![self.stat()]),     // Mute / Demute
            0x0D => {                                         // Setfilter file channel
                self.filter = (arg(0), arg(1));
                self.ack(vec![self.stat()]);
            }
            0x0E => {                                         // Setmode
                self.mode = arg(0);
                self.ack(vec![self.stat()]);
            }
            0x0F => self.ack(vec![self.stat(), self.mode, 0, self.filter.0, self.filter.1]),
            0x10 => {                                         // GetlocL: the last sector's header
                let s = &self.sector;
                self.ack(vec![s[12], s[13], s[14], s[15], s[16], s[17], s[18], s[19]]);
            }
            0x11 => self.ack(self.getloc_p()),
            0x12 => {                                         // SetSession
                self.ack(vec![self.stat()]);
                self.then(ACK_DELAY * 4, 2, vec![self.stat()]);
            }
            0x13 => {                                         // GetTN
                let d = self.disc.as_ref().unwrap();
                let first = d.tracks.first().map_or(1, |t| t.number) as u32;
                let last = d.tracks.last().map_or(1, |t| t.number) as u32;
                self.ack(vec![self.stat(), bcd(first), bcd(last)]);
            }
            0x14 => {                                         // GetTD track
                let n = from_bcd(arg(0));
                let d = self.disc.as_ref().unwrap();
                let lba = if n == 0 { Some(d.leadout) } else { d.tracks.iter().find(|t| t.number as u32 == n).map(|t| t.start) };
                match lba {
                    Some(l) => {
                        let msf = l + LEAD_IN;
                        self.ack(vec![self.stat(), bcd(msf / 75 / 60), bcd(msf / 75 % 60)]);
                    }
                    None => self.error(0x10),
                }
            }
            0x15 | 0x16 => {                                  // SeekL / SeekP
                self.start_seek(After::Complete);
                self.ack(vec![self.stat()]);
            }
            0x19 => {
                let r = match arg(0) {
                    0x20 => vec![0x94, 0x09, 0x19, 0xC0],     // the controller's date / version
                    0x04 | 0x05 => vec![self.stat()],
                    _ => vec![self.stat()],
                };
                self.ack(r);
            }
            0x1A => {                                         // GetID: a licensed mode 2 disc
                self.ack(vec![self.stat()]);
                let audio = self.disc.as_ref().unwrap().tracks.first().is_some_and(|t| t.is_audio());
                if audio {
                    self.then(ACK_DELAY * 4, 5, vec![self.stat() | 8, 0x90, 0, 0, 0, 0, 0, 0]);
                } else {
                    let r = match self.region { b'E' => b'E', b'J' | b'I' => b'I', _ => b'A' };
                    self.then(ACK_DELAY * 4, 2, vec![self.stat(), 0x00, 0x20, 0x00, b'S', b'C', b'E', r]);
                }
            }
            0x1C => {                                         // Reset
                self.drive = Drive::Idle;
                self.mode = 0;
                self.ack(vec![self.stat()]);
            }
            0x1E => {                                         // ReadTOC
                self.ack(vec![self.stat()]);
                self.then(CPU_HZ / 2, 2, vec![self.stat()]);
            }
            _ => self.error(0x40),
        }
    }

    fn start_seek(&mut self, then: After) {
        self.second = None;
        self.pending.clear();
        self.stat |= STAT_MOTOR;
        if let Some(t) = self.setloc.take() {
            self.lba = t;
            self.drive = Drive::Seeking { then };
            self.timer = SEEK_DELAY;
        } else {
            self.begin(then);
        }
    }

    fn begin(&mut self, then: After) {
        match then {
            After::Read => { self.drive = Drive::Reading; self.timer = self.sector_cycles(); }
            After::Play => { self.drive = Drive::Playing; self.timer = self.sector_cycles(); self.report_count = 0; }
            After::Complete => { self.drive = Drive::Idle; self.then(1, 2, vec![self.stat()]); }
        }
    }

    /// GetlocP: track, index, position in the track and on the disc (BCD)
    fn getloc_p(&self) -> Vec<u8> {
        let lba = self.lba;
        let Some(t) = self.disc.as_ref().and_then(|d| d.track_at(lba)) else { return vec![0; 8] };
        let index = if lba < t.start { 0 } else { 1 };
        let rel = if lba < t.start { t.start - lba } else { lba - t.start };
        let abs = lba + LEAD_IN;
        vec![bcd(t.number as u32), bcd(index), bcd(rel / 75 / 60), bcd(rel / 75 % 60), bcd(rel % 75),
             bcd(abs / 75 / 60), bcd(abs / 75 % 60), bcd(abs % 75)]
    }

    // ------------------------------------------------------------------ time

    pub fn tick(&mut self, cycles: u32) {
        let c = cycles as i64;
        // the drive mechanism
        match self.drive {
            Drive::Seeking { then } => {
                self.timer -= c;
                if self.timer <= 0 { self.begin(then); }
            }
            Drive::Reading => {
                self.timer -= c;
                if self.timer <= 0 {
                    self.timer += self.sector_cycles();
                    self.read_sector();
                }
            }
            Drive::Playing => {
                self.timer -= c;
                if self.timer <= 0 {
                    self.timer += self.sector_cycles();
                    self.play_sector();
                }
            }
            Drive::Idle => {}
        }
        // answers, one interrupt at a time
        if self.int_flag != 0 { return; }
        if let Some(r) = self.first.as_mut() {
            r.delay -= c;
            if r.delay <= 0 {
                let r = self.first.take().unwrap();
                self.raise(r.int, r.bytes);
            }
            return;
        }
        if let Some(r) = self.second.as_mut() {
            r.delay -= c;
            if r.delay <= 0 {
                let r = self.second.take().unwrap();
                self.raise(r.int, r.bytes);
                return;
            }
        }
        // the oldest buffered sector becomes the current one (data FIFO, GetlocL)
        if let Some(sec) = self.pending.pop_front() {
            self.sector = sec;
            self.raise(1, vec![self.stat()]);
        }
    }

    fn raise(&mut self, int: u8, bytes: Vec<u8>) {
        self.int_flag = int;
        self.response = bytes;
        self.resp_pos = 0;
        if self.int_flag & self.int_enable != 0 { self.irq = true; }
    }

    fn read_sector(&mut self) {
        let Some(d) = self.disc.as_mut() else { return };
        let mut raw = [0u8; SECTOR];
        let ok = d.read_sector(self.lba, &mut raw);
        self.lba += 1;
        if !ok {
            self.drive = Drive::Idle;
            self.then(1, 4, vec![self.stat()]);
            return;
        }
        let submode = raw[18];
        // bring-up: the sectors of streamed reads (XA mode on), with their subheader and
        // first data word
        if self.mode & 0x40 != 0 && self.sector_logged < 150 {
            self.sector_logged += 1;
            let w = u32::from_le_bytes([raw[24], raw[25], raw[26], raw[27]]);
            println!("PS1 CD: sector {} mode {:02X} sub {:02X} {:02X} {:02X} {:02X} data {w:08X} (buffered {})",
                self.lba - 1, raw[15], raw[16], raw[17], raw[18], raw[19], self.pending.len());
        }
        // XA audio sectors go to the ADPCM decoder (stage 6), not the CPU
        if self.mode & 0x40 != 0 && raw[15] == 2 && submode & 0x04 != 0 { return; }
        if self.pending.len() >= SECTOR_BUFFERS { self.pending.pop_front(); }
        self.pending.push_back(raw.to_vec());
    }

    fn play_sector(&mut self) {
        let leadout = self.disc.as_ref().map_or(0, |d| d.leadout);
        self.lba += 1;
        if self.lba >= leadout {
            self.drive = Drive::Idle;
            self.then(1, 4, vec![self.stat()]);
            return;
        }
        // report mode: a position report every 10 sectors
        if self.mode & 0x04 != 0 {
            self.report_count += 1;
            if self.report_count % 10 == 0 {
                let p = self.getloc_p();
                let r = vec![self.stat(), p[0], p[1], p[5], p[6], p[7], 0, 0];
                self.second = Some(Response { delay: 1, int: 1, bytes: r });
            }
        }
    }
}
