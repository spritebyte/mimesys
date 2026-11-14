//! Visual Memory Unit: the memory card that plugs into a controller's first slot. It is a
//! Maple sub-unit with three functions: storage (256 blocks of 512 bytes), a 48x32 LCD
//! and a clock.
//!
//! Storage file system (block numbers): 255 = root block (0x55 x16 format mark, then the
//! volume info at 0x40), 254 = FAT (one u16 per block: FFFC free, FFFA last block of a
//! chain, else the next block), 253-241 = directory (13 blocks, 32-byte entries), 0-199 =
//! user saves. Blocks 200-240 are unused on a standard card.

use serde::{Deserialize, Serialize};

pub const SIZE: usize = 128 * 1024;
pub const BLOCK: usize = 512;

pub const FUNC_STORAGE: u32 = 0x0200_0000;
pub const FUNC_LCD: u32 = 0x0400_0000;
pub const FUNC_CLOCK: u32 = 0x0800_0000;

const ROOT: usize = 255;
const FAT: usize = 254;
const DIR: usize = 253;
const DIR_BLOCKS: usize = 13;
const USER_BLOCKS: u16 = 200;

#[derive(Clone, Serialize, Deserialize)]
pub struct Vmu {
    pub data: Vec<u8>,
    /// the 48x32 LCD as the game sent it (6 bytes per row, 1 bit per dot)
    pub lcd: Vec<u8>,
    /// written since the save file was last updated
    pub dirty: bool,
    /// debug counters: blocks read, blocks (phases) written
    pub reads: u64,
    pub writes: u64,
}

impl Default for Vmu {
    fn default() -> Self { Self::new() }
}

/// (days since 1970-01-01) -> (year, month 1-12, day 1-31), proleptic Gregorian.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + (m <= 2) as i64, m, d)
}

fn bcd(v: u32) -> u8 { ((v / 10 % 10) << 4 | v % 10) as u8 }

/// Date and time now (UTC): year, month, day, hour, minute, second, weekday (0 = Monday).
fn now() -> (i64, u32, u32, u32, u32, u32, u32) {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let t = secs.rem_euclid(86_400) as u32;
    let (y, m, d) = civil(days);
    // 1970-01-01 was a Thursday
    let wd = (days + 3).rem_euclid(7) as u32;
    (y, m, d, t / 3600, t / 60 % 60, t % 60, wd)
}

impl Vmu {
    /// A freshly formatted card: 200 free blocks, no saves.
    pub fn new() -> Self {
        let mut v = Self { data: vec![0; SIZE], lcd: vec![0; 192], dirty: false, reads: 0, writes: 0 };
        v.format();
        v
    }

    pub fn format(&mut self) {
        self.data.fill(0);
        let r = ROOT * BLOCK;
        self.data[r..r + 16].fill(0x55);
        // standard colours off; format date (BCD century, year, month, day, h, m, s, weekday)
        let (y, mo, d, h, mi, s, wd) = now();
        let date = [bcd((y / 100) as u32), bcd((y % 100) as u32), bcd(mo), bcd(d), bcd(h), bcd(mi), bcd(s), wd as u8];
        self.data[r + 0x30..r + 0x38].copy_from_slice(&date);
        // volume info, the 12 halfwords the storage media-info reply hands back
        let info: [u16; 10] = [0xFF, 0, ROOT as u16, FAT as u16, 1, DIR as u16, DIR_BLOCKS as u16, 0, USER_BLOCKS, 0x1F];
        for (k, w) in info.iter().enumerate() {
            self.data[r + 0x40 + 2 * k..r + 0x42 + 2 * k].copy_from_slice(&w.to_le_bytes());
        }
        // FAT: everything free but the system blocks
        let f = FAT * BLOCK;
        for b in 0..256 {
            let e: u16 = match b {
                ROOT | FAT => 0xFFFA,
                b if b <= DIR && b > DIR - DIR_BLOCKS => if b == DIR - DIR_BLOCKS + 1 { 0xFFFA } else { b as u16 - 1 },
                _ => 0xFFFC,
            };
            self.data[f + 2 * b..f + 2 * b + 2].copy_from_slice(&e.to_le_bytes());
        }
    }

    /// Load a 128 KB image; an all-zero one (never written) is formatted.
    pub fn load(&mut self, img: &[u8]) {
        if img.len() != SIZE { return; }
        self.data.copy_from_slice(img);
        if img.iter().all(|&b| b == 0) { self.format(); }
    }

    /// Free user blocks (FAT entries 0-199 marked free).
    pub fn free_blocks(&self) -> usize {
        let f = FAT * BLOCK;
        (0..USER_BLOCKS as usize).filter(|b| u16::from_le_bytes([self.data[f + 2 * b], self.data[f + 2 * b + 1]]) == 0xFFFC).count()
    }

    /// Names of the saves in the directory (the 12-byte VMS file names).
    pub fn files(&self) -> Vec<String> {
        let mut out = Vec::new();
        for k in 0..DIR_BLOCKS {
            let b = (DIR - k) * BLOCK;
            for e in 0..BLOCK / 32 {
                let d = &self.data[b + 32 * e..b + 32 * e + 32];
                if d[0] == 0x33 || d[0] == 0xCC {
                    out.push(String::from_utf8_lossy(&d[4..16]).trim_end().to_string());
                }
            }
        }
        out
    }

    /// The 28-word device information block.
    pub fn info() -> Vec<u32> {
        let mut b = Vec::with_capacity(112);
        b.extend((FUNC_STORAGE | FUNC_LCD | FUNC_CLOCK).to_le_bytes());
        // function data, highest function first: clock, LCD, storage
        b.extend(0x403F_7E7Eu32.to_le_bytes());
        b.extend(0x0010_0500u32.to_le_bytes());
        b.extend(0x0041_0F00u32.to_le_bytes());
        b.extend([0xFF, 0]); // area code (every region), connector direction
        super::maple::put_str(&mut b, "Visual Memory", 30);
        super::maple::put_str(&mut b, "Produced By or Under License From SEGA ENTERPRISES,LTD.", 60);
        b.extend(0x0082_007Cu32.to_le_bytes()); // standby 12.4 mA, max 13 mA
        super::maple::words(&b)
    }

    /// Answer one frame: (reply code, data words).
    pub fn reply(&mut self, frame: &[u32]) -> (u32, Vec<u32>) {
        let cmd = frame[0] & 0xFF;
        let n = (frame[0] >> 24) as usize;
        let data = &frame[1..(1 + n).min(frame.len())];
        let func = data.first().copied().unwrap_or(0);
        let arg = data.get(1).copied().unwrap_or(0);
        // the second word as bytes: partition, phase, block (big-endian halfword)
        let [_pt, phase, bh, bl] = arg.to_le_bytes();
        let block = (bh as usize) << 8 | bl as usize;
        match cmd {
            0x01 => (0x05, Self::info()),
            0x02 => {
                let mut d = Self::info();
                let mut v = Vec::new();
                super::maple::put_str(&mut v, "Version 1.005,1999/04/15,315-6208-03,SEGA Visual Memory System BIOS Produced by ", 80);
                d.extend(super::maple::words(&v));
                (0x06, d)
            }
            0x03 | 0x04 => (0x07, Vec::new()),
            0x0A => match func {
                FUNC_STORAGE => {
                    let r = ROOT * BLOCK + 0x40;
                    let mut d = vec![FUNC_STORAGE];
                    d.extend(super::maple::words(&self.data[r..r + 24]));
                    (0x08, d)
                }
                // 48x32 dots (minus one), 1 colour, no contrast levels
                FUNC_LCD => (0x08, vec![FUNC_LCD, u32::from_le_bytes([47, 31, 0x10, 2])]),
                _ => (0xFE, Vec::new()),
            },
            0x0B => match func {
                FUNC_STORAGE => {
                    self.reads += 1;
                    let a = (block & 0xFF) * BLOCK;
                    let mut d = vec![FUNC_STORAGE, arg];
                    d.extend(super::maple::words(&self.data[a..a + BLOCK]));
                    (0x08, d)
                }
                FUNC_LCD => {
                    let mut d = vec![FUNC_LCD, arg];
                    d.extend(super::maple::words(&self.lcd));
                    (0x08, d)
                }
                FUNC_CLOCK => {
                    let (y, mo, d, h, mi, s, wd) = now();
                    let y = y as u32;
                    (0x08, vec![FUNC_CLOCK, y & 0xFF | (y >> 8 & 0xFF) << 8 | mo << 16 | d << 24, h | mi << 8 | s << 16 | wd << 24])
                }
                _ => (0xFE, Vec::new()),
            },
            0x0C => match func {
                FUNC_STORAGE => {
                    // a block goes over in 4 phases of 128 bytes
                    let a = block * BLOCK + phase as usize * (BLOCK / 4);
                    let bytes: Vec<u8> = data[2.min(data.len())..].iter().flat_map(|w| w.to_le_bytes()).collect();
                    if a + bytes.len() > SIZE { return (0xFB, vec![0]); }
                    if self.data[a..a + bytes.len()] != bytes[..] {
                        self.data[a..a + bytes.len()].copy_from_slice(&bytes);
                        self.dirty = true;
                    }
                    self.writes += 1;
                    (0x07, Vec::new())
                }
                FUNC_LCD => {
                    let bytes: Vec<u8> = data[2.min(data.len())..].iter().flat_map(|w| w.to_le_bytes()).collect();
                    let k = bytes.len().min(self.lcd.len());
                    self.lcd[..k].copy_from_slice(&bytes[..k]);
                    (0x07, Vec::new())
                }
                // setting the clock: accepted, the card keeps the host's time
                FUNC_CLOCK => (0x07, Vec::new()),
                _ => (0xFE, Vec::new()),
            },
            // complete a write (the BIOS sends it after the 4 phases)
            0x0D => (0x07, Vec::new()),
            // the clock's beeper / alarm
            0x0E => match func {
                FUNC_CLOCK => (0x07, Vec::new()),
                _ => (0xFE, Vec::new()),
            },
            _ => (0xFD, Vec::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(cmd: u32, words: &[u32]) -> Vec<u32> {
        let mut f = vec![cmd | 0x01 << 8 | 0x00 << 16 | (words.len() as u32) << 24];
        f.extend_from_slice(words);
        f
    }

    #[test]
    fn a_new_card_is_formatted_with_200_free_blocks() {
        let v = Vmu::new();
        assert_eq!(&v.data[255 * 512..255 * 512 + 16], &[0x55; 16]);
        assert_eq!(v.free_blocks(), 200);
        assert!(v.files().is_empty());
        // directory chain 253 -> 241
        let fat = |b: usize| u16::from_le_bytes([v.data[254 * 512 + 2 * b], v.data[254 * 512 + 2 * b + 1]]);
        assert_eq!(fat(253), 252);
        assert_eq!(fat(241), 0xFFFA);
        assert_eq!(fat(255), 0xFFFA);
        assert_eq!(fat(240), 0xFFFC);
    }

    #[test]
    fn media_info_comes_from_the_root_block() {
        let mut v = Vmu::new();
        let (code, d) = v.reply(&frame(0x0A, &[FUNC_STORAGE, 0]));
        assert_eq!(code, 0x08);
        assert_eq!(d.len(), 7);
        assert_eq!(d[1], 0x0000_00FF); // total size 255, partition 0
        assert_eq!(d[2], 0x00FE_00FF); // root 255, FAT 254
        assert_eq!(d[3], 0x00FD_0001); // 1 FAT block, directory 253
        assert_eq!(d[4], 0x0000_000D); // 13 directory blocks, icon 0
        assert_eq!(d[5], 0x001F_00C8); // 200 user blocks
    }

    #[test]
    fn a_block_written_in_four_phases_reads_back() {
        let mut v = Vmu::new();
        for phase in 0..4u32 {
            let arg = u32::from_le_bytes([0, phase as u8, 0, 7]);
            let mut w = vec![FUNC_STORAGE, arg];
            w.extend((0..32).map(|k| phase << 24 | k));
            assert_eq!(v.reply(&frame(0x0C, &w)).0, 0x07);
        }
        assert!(v.dirty);
        assert_eq!(v.reply(&frame(0x0D, &[FUNC_STORAGE, 0])).0, 0x07);
        let (code, d) = v.reply(&frame(0x0B, &[FUNC_STORAGE, u32::from_le_bytes([0, 0, 0, 7])]));
        assert_eq!(code, 0x08);
        assert_eq!(d.len(), 2 + 128);
        assert_eq!(d[2], 0);
        assert_eq!(d[2 + 32 + 5], 1 << 24 | 5);
        assert_eq!(d[2 + 127], 3 << 24 | 31);
    }

    #[test]
    fn a_blank_image_is_formatted_on_load() {
        let mut v = Vmu::new();
        v.load(&vec![0; SIZE]);
        assert_eq!(v.free_blocks(), 200);
        let mut img = vec![0; SIZE];
        img[5] = 1;
        v.load(&img);
        assert_eq!(v.data[5], 1);
    }

    #[test]
    fn calendar_dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(11_016), (2000, 2, 29));
        assert_eq!(civil(20_768), (2026, 11, 11));
    }
}
