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
//!   With XA-ADPCM on (mode bit 6), audio sectors (submode audio + real time, matching
//!   Setfilter when mode bit 3 is on) go to the XA decoder, not the CPU.
//!   Sectors queue in the drive's buffer (8 here) until the CPU acknowledges each INT1;
//!   the current sector (data FIFO, GetlocL) changes only when its INT1 is raised.
//! - Play (CD-DA) streams the sectors' 16-bit stereo samples to the SPU and answers GetlocP.
//! - XA-ADPCM: 18 128-byte portions of 4-bit (or 8-bit) samples, decoded like SPU-ADPCM
//!   (four filters), 37800 or 18900 Hz, mono or stereo, resampled to 44100 Hz with the
//!   hardware's 25-point zigzag tables (7 outputs per 6 inputs).
//! - CD audio out: the ATV volumes (L->L, L->R, R->R, R->L, index 2 / 3 registers,
//!   applied by ADPCTL bit 5), Mute / Demute, and ADPCTL bit 0 muting XA.
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
/// CD audio queued for the SPU (44100 Hz stereo frames): a quarter second
const AUDIO_QUEUE: usize = 11_025;

const XA_POS: [i32; 4] = [0, 60, 115, 98];
const XA_NEG: [i32; 4] = [0, 0, -52, -55];

const ZIGZAG: [[i32; 29]; 7] = [
    [0, 0, 0, 0, 0, -2, 10, -34, 65, -84, 52, 9, -266, 1024, -2680, 9036, 26516, -6016, 3021, -1571, 848, -365, 107, 10, -16, 17, -8, 3, -1],
    [0, 0, 0, -2, 0, 3, -19, 60, -75, 162, -227, 306, -67, -615, 3229, 29883, -4532, 2488, -1471, 882, -424, 166, -27, 5, 6, -8, 3, -1, 0],
    [0, 0, -1, 3, -2, -5, 31, -74, 179, -402, 689, -926, 1272, -1446, 31033, -1446, 1272, -926, 689, -402, 179, -74, 31, -5, -2, 3, -1, 0, 0],
    [0, -1, 3, -8, 6, 5, -27, 166, -424, 882, -1471, 2488, -4532, 29883, 3229, -615, -67, 306, -227, 162, -75, 60, -19, 3, 0, -2, 0, 0, 0],
    [-1, 3, -8, 17, -16, 10, 107, -365, 848, -1571, 3021, -6016, 26516, 9036, -2680, 1024, -266, 9, 52, -84, 65, -34, 10, -1, 0, 1, 0, 0, 0],
    [2, -8, 16, -35, 43, 26, -235, 635, -1352, 2810, -5882, 21472, 15367, -4681, 2062, -839, 347, -68, -23, 70, -35, 17, -5, 0, 0, 0, 0, 0, 0],
    [-5, 17, -35, 70, -23, -68, 347, -839, 2062, -4681, 15367, 21472, -5882, 2810, -1352, 635, -235, 26, 43, -35, 16, -8, 2, 0, 0, 0, 0, 0, 0],
];


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
    #[serde(skip)]
    cpu_bytes: u32,
    #[serde(skip)]
    req_logged: u32,
    /// bring-up: ask for the CPU's instruction trace
    #[serde(skip)]
    pub want_trace: bool,
    /// CD audio for the SPU (44100 Hz stereo)
    #[serde(skip)]
    pub audio: std::collections::VecDeque<(i16, i16)>,
    muted: bool,
    xa_mute: bool,
    /// applied / written CD volumes: L->L, L->R, R->R, R->L
    atv: [u8; 4],
    atv_next: [u8; 4],
    xa: Xa,
}

/// XA-ADPCM decoder state: the filters' history, the resampler's ring and counter
#[derive(Clone, Serialize, Deserialize)]
struct Xa {
    old: [[i32; 2]; 2],
    ring: Vec<[i16; 2]>,
    p: usize,
    six: u32,
}

impl Default for Xa {
    fn default() -> Self { Self { old: [[0; 2]; 2], ring: vec![[0; 2]; 32], p: 0, six: 6 } }
}

fn bcd(v: u32) -> u8 { ((v / 10) << 4 | (v % 10)) as u8 }
fn from_bcd(v: u8) -> u32 { ((v >> 4) * 10 + (v & 15)) as u32 }

impl Cdrom {
    /// `region`: the BIOS's letter (A, E, J)
    pub fn new(disc: Option<Disc>, region: u8) -> Self {
        let stat = if disc.is_some() { STAT_MOTOR } else { STAT_SHELL_OPEN };
        Self { int_enable: 0x1F, stat, disc, region, sector: vec![0; SECTOR], atv: [0x80, 0, 0x80, 0], atv_next: [0x80, 0, 0x80, 0], ..Default::default() }
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
            2 => { self.cpu_bytes += 1; self.data_byte() }
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
                // bring-up: how a streaming game takes its data (CPU or DMA, all of it?)
                if self.streaming() && self.req_logged < 60 {
                    self.req_logged += 1;
                    if self.req_logged == 1 { self.want_trace = true; }
                    println!("PS1 CD: request {v:02X} with FIFO at {}/{}, {} bytes read by the CPU since the last request", self.data_pos, self.data.len(), self.cpu_bytes);
                }
                self.cpu_bytes = 0;
                // request: bit 7 loads the sector into the data FIFO, clear empties it
                if v & 0x80 != 0 {
                    if self.data_pos >= self.data.len() { self.load_data(); }
                } else {
                    self.data.clear();
                    self.data_pos = 0;
                }
            }
            (2, 1) => self.int_enable = v & 0x1F,
            (2, 2) => self.atv_next[0] = v,
            (3, 2) => self.atv_next[1] = v,
            (1, 3) => self.atv_next[2] = v,
            (2, 3) => self.atv_next[3] = v,
            (3, 3) => {
                self.xa_mute = v & 1 != 0;
                if v & 0x20 != 0 { self.atv = self.atv_next; }
            }
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
            0x0B | 0x0C => {                                  // Mute / Demute
                self.muted = c == 0x0B;
                self.ack(vec![self.stat()]);
            }
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
        // XA audio sectors (audio + real time) go to the ADPCM decoder, not the CPU; with
        // the filter on, only the selected file / channel's are played
        if self.mode & 0x40 != 0 && raw[15] == 2 && submode & 0x44 == 0x44 {
            let wanted = self.mode & 0x08 == 0 || (raw[16], raw[17]) == self.filter;
            if wanted { self.decode_xa(&raw); }
            return;
        }
        if self.mode & 0x08 != 0 && raw[15] == 2 && submode & 0x44 == 0x44 { return; }
        if self.pending.len() >= SECTOR_BUFFERS { self.pending.pop_front(); }
        self.pending.push_back(raw.to_vec());
    }

    fn play_sector(&mut self) {
        let leadout = self.disc.as_ref().map_or(0, |d| d.leadout);
        // CD-DA: 588 stereo 16-bit frames
        let mut raw = [0u8; SECTOR];
        let audio = self.disc.as_ref().and_then(|d| d.track_at(self.lba)).is_some_and(|t| t.is_audio());
        if audio && self.disc.as_mut().is_some_and(|d| d.read_sector(self.lba, &mut raw)) {
            for f in raw.chunks_exact(4) {
                let l = i16::from_le_bytes([f[0], f[1]]);
                let r = i16::from_le_bytes([f[2], f[3]]);
                self.push_audio(l, r);
            }
        }
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

    // ------------------------------------------------------------------ CD audio

    /// A frame out to the SPU, through the CD volumes
    fn push_audio(&mut self, l: i16, r: i16) {
        let (l, r) = if self.muted { (0, 0) } else {
            let (l, r) = (l as i32, r as i32);
            let a = self.atv.map(|v| v as i32);
            (((l * a[0] + r * a[3]) >> 7).clamp(-0x8000, 0x7FFF) as i16,
             ((r * a[2] + l * a[1]) >> 7).clamp(-0x8000, 0x7FFF) as i16)
        };
        if self.audio.len() >= AUDIO_QUEUE { self.audio.pop_front(); }
        self.audio.push_back((l, r));
    }

    /// An XA-ADPCM sector (raw, 2352 bytes) to 44100 Hz audio
    fn decode_xa(&mut self, raw: &[u8]) {
        let ci = raw[19];
        let stereo = ci & 3 == 1;
        let half_rate = ci & 4 != 0;
        let eight_bit = ci & 0x30 == 0x10;
        let mut left = Vec::with_capacity(4032);
        let mut right = Vec::with_capacity(2016);
        for portion in 0..18 {
            let src = &raw[24 + portion * 128..24 + portion * 128 + 128];
            for blk in 0..4 {
                for nib in 0..2usize {
                    // 8-bit: one block per byte; 4-bit: two nibbles per byte
                    let (hdr_i, ch) = if eight_bit { (blk, blk & 1) } else { (blk * 2 + nib, if stereo { nib } else { 0 }) };
                    if eight_bit && nib == 1 { continue; }
                    let ch = if stereo { ch } else { 0 };
                    let h = src[4 + hdr_i];
                    let mut shift = h & 0xF;
                    if shift > 12 { shift = 9; }
                    let filter = ((h >> 4) & 3) as usize;
                    let (f0, f1) = (XA_POS[filter], XA_NEG[filter]);
                    let [mut old, mut older] = self.xa.old[ch];
                    let out = if ch == 1 { &mut right } else { &mut left };
                    for j in 0..28 {
                        let w = u32::from_le_bytes(src[16 + j * 4..20 + j * 4].try_into().unwrap());
                        let t = if eight_bit {
                            (((w >> (blk * 8)) & 0xFF) as i32) << 24 >> 16
                        } else {
                            (((w >> ((blk * 2 + nib) * 4)) & 0xF) as i32) << 28 >> 16
                        };
                        let s = ((t >> shift) + ((old * f0 + older * f1 + 32) >> 6)).clamp(-0x8000, 0x7FFF);
                        older = old;
                        old = s;
                        out.push(s as i16);
                    }
                    self.xa.old[ch] = [old, older];
                }
            }
        }
        if self.xa_mute { left.iter_mut().for_each(|s| *s = 0); right.iter_mut().for_each(|s| *s = 0); }
        let n = if stereo { left.len().min(right.len()) } else { left.len() };
        for i in 0..n {
            let pair = if stereo { [left[i], right[i]] } else { [left[i], left[i]] };
            self.resample(pair);
            if half_rate { self.resample(pair); }
        }
    }

    /// 37800 Hz in, 44100 Hz out: every sixth input makes seven outputs
    fn resample(&mut self, s: [i16; 2]) {
        let x = &mut self.xa;
        x.ring[x.p & 31] = s;
        x.p += 1;
        x.six -= 1;
        if x.six != 0 { return; }
        x.six = 6;
        for t in 0..7 {
            let mut sum = [0i32; 2];
            for i in 1..30 {
                let r = self.xa.ring[(self.xa.p.wrapping_sub(i)) & 31];
                for c in 0..2 { sum[c] += (r[c] as i32 * ZIGZAG[t][i - 1]) >> 15; }
            }
            let l = sum[0].clamp(-0x8000, 0x7FFF) as i16;
            let r = sum[1].clamp(-0x8000, 0x7FFF) as i16;
            self.push_audio(l, r);
        }
    }
}
