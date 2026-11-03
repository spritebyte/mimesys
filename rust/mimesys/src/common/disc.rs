//! CD images: CUE sheets (with BIN / WAV files) and ISO files, read a sector at a time
//! straight from disk (a disc is hundreds of MB, so it isn't loaded into memory).
//!
//! Addresses are LBAs: sector 0 is the first track's INDEX 01. A drive reports
//! positions as MSF (minutes:seconds:frames, 75 frames a second) counted from the start
//! of the lead-in, which is 150 frames (2 seconds) earlier: MSF = LBA + 150.
//!
//! CUE support: FILE "name" BINARY | WAVE; TRACK n MODE1/2048 | MODE1/2352 | MODE2/2352 |
//! AUDIO; INDEX 00 / 01; PREGAP (silence that isn't in the file). Not supported yet: MP3 /
//! OGG / FLAC audio files, and CHD images.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub const SECTOR: usize = 2352;
pub const LEAD_IN: u32 = 150;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackKind {
    /// 2048 bytes of user data per sector in the file (ISO, MODE1/2048)
    Mode1Cooked,
    /// raw 2352-byte data sectors (MODE1/2352, MODE2/2352)
    DataRaw,
    Audio,
}

#[derive(Clone, Debug)]
pub struct Track {
    pub number: u8,
    pub kind: TrackKind,
    /// LBA of INDEX 01: where the track starts
    pub start: u32,
    /// LBA where its pregap starts (= `start` when there's none)
    pub pregap: u32,
    /// sectors from `start` to the next track's pregap (or the lead-out)
    pub length: u32,
    file: usize,
    /// byte offset in the file of the track's INDEX 01
    offset: u64,
    /// LBAs before this one in the pregap are silence that isn't in the file
    silent_until: u32,
    sector_size: u32,
}

impl Track {
    pub fn is_audio(&self) -> bool { self.kind == TrackKind::Audio }
}

#[derive(Debug)]
pub struct Disc {
    files: Vec<File>,
    pub tracks: Vec<Track>,
    /// LBA of the lead-out (the disc's length)
    pub leadout: u32,
}

/// LBA to (minutes, seconds, frames), counting the 2-second lead-in.
pub fn lba_to_msf(lba: u32) -> (u8, u8, u8) {
    let f = lba + LEAD_IN;
    ((f / 4500) as u8, (f / 75 % 60) as u8, (f % 75) as u8)
}

fn parse_msf(s: &str) -> Result<u32, String> {
    let p: Vec<&str> = s.split(':').collect();
    if p.len() != 3 { return Err(format!("'{s}' isn't mm:ss:ff")); }
    let n = |x: &str| x.parse::<u32>().map_err(|_| format!("'{s}' isn't mm:ss:ff"));
    Ok((n(p[0])? * 60 + n(p[1])?) * 75 + n(p[2])?)
}

/// The "data" chunk of a RIFF WAVE file: (offset, length).
fn wave_data(f: &mut File) -> Result<(u64, u64), String> {
    let mut head = [0u8; 12];
    f.seek(SeekFrom::Start(0)).and_then(|_| f.read_exact(&mut head)).map_err(|e| e.to_string())?;
    if &head[0..4] != b"RIFF" || &head[8..12] != b"WAVE" { return Err("not a WAVE file".into()); }
    let mut at = 12u64;
    loop {
        let mut ch = [0u8; 8];
        f.seek(SeekFrom::Start(at)).and_then(|_| f.read_exact(&mut ch)).map_err(|_| "WAVE file without a data chunk".to_string())?;
        let len = u32::from_le_bytes([ch[4], ch[5], ch[6], ch[7]]) as u64;
        if &ch[0..4] == b"data" { return Ok((at + 8, len)); }
        at += 8 + len + (len & 1);
    }
}

/// One FILE line of a cue sheet and the tracks under it.
struct CueFile { name: String, wave: bool, tracks: Vec<CueTrack> }
struct CueTrack { number: u8, kind: TrackKind, size: u32, pregap: u32, index0: Option<u32>, index1: Option<u32> }

impl Disc {
    /// Open a .cue or .iso.
    pub fn open(path: &Path) -> Result<Self, String> {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        match ext.as_str() {
            "cue" => Self::open_cue(path),
            "iso" => Self::open_iso(path),
            _ => Err(format!("{}: only .cue and .iso disc images are supported", path.display())),
        }
    }

    pub fn open_iso(path: &Path) -> Result<Self, String> {
        let f = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let len = f.metadata().map_err(|e| e.to_string())?.len();
        let sectors = (len / 2048) as u32;
        let t = Track { number: 1, kind: TrackKind::Mode1Cooked, start: 0, pregap: 0, length: sectors, file: 0, offset: 0, silent_until: 0, sector_size: 2048 };
        Ok(Self { files: vec![f], tracks: vec![t], leadout: sectors })
    }

    pub fn open_cue(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        Self::from_cue(&text, &dir)
    }

    /// A cue sheet's text; file names are relative to `dir`.
    pub fn from_cue(text: &str, dir: &Path) -> Result<Self, String> {
        let mut cue: Vec<CueFile> = Vec::new();
        for (n, raw) in text.lines().enumerate() {
            let line = raw.trim();
            let err = |m: &str| format!("cue line {}: {m}", n + 1);
            let (cmd, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
            let rest = rest.trim();
            match cmd.to_ascii_uppercase().as_str() {
                "FILE" => {
                    // FILE "name with spaces.bin" BINARY  (or unquoted)
                    let (name, kind) = if let Some(r) = rest.strip_prefix('"') {
                        let end = r.find('"').ok_or_else(|| err("unterminated file name"))?;
                        (r[..end].to_string(), r[end + 1..].trim().to_ascii_uppercase())
                    } else {
                        let (a, b) = rest.rsplit_once(char::is_whitespace).ok_or_else(|| err("FILE needs a name and a type"))?;
                        (a.trim().to_string(), b.to_ascii_uppercase())
                    };
                    let wave = match kind.as_str() {
                        "BINARY" | "MOTOROLA" => false,
                        "WAVE" => true,
                        other => return Err(err(&format!("{other} audio files aren't supported yet (convert to BIN or WAV)"))),
                    };
                    cue.push(CueFile { name, wave, tracks: Vec::new() });
                }
                "TRACK" => {
                    let f = cue.last_mut().ok_or_else(|| err("TRACK before FILE"))?;
                    let mut it = rest.split_whitespace();
                    let number = it.next().and_then(|s| s.parse().ok()).ok_or_else(|| err("bad track number"))?;
                    let (kind, size) = match it.next().unwrap_or("").to_ascii_uppercase().as_str() {
                        "MODE1/2048" => (TrackKind::Mode1Cooked, 2048),
                        "MODE1/2352" | "MODE2/2352" => (TrackKind::DataRaw, 2352),
                        "AUDIO" => (TrackKind::Audio, 2352),
                        other => return Err(err(&format!("track type '{other}' isn't supported"))),
                    };
                    f.tracks.push(CueTrack { number, kind, size, pregap: 0, index0: None, index1: None });
                }
                "INDEX" | "PREGAP" => {
                    let t = cue.last_mut().and_then(|f| f.tracks.last_mut()).ok_or_else(|| err("INDEX / PREGAP before TRACK"))?;
                    if cmd.eq_ignore_ascii_case("PREGAP") {
                        t.pregap = parse_msf(rest).map_err(|e| err(&e))?;
                    } else {
                        let (i, at) = rest.split_once(char::is_whitespace).ok_or_else(|| err("INDEX needs a number and a time"))?;
                        let at = parse_msf(at.trim()).map_err(|e| err(&e))?;
                        match i.parse::<u32>() {
                            Ok(0) => t.index0 = Some(at),
                            Ok(1) => t.index1 = Some(at),
                            _ => {} // later indexes don't move anything
                        }
                    }
                }
                _ => {} // REM, TITLE, PERFORMER, CATALOG, FLAGS, POSTGAP, ...
            }
        }
        if cue.iter().all(|f| f.tracks.is_empty()) { return Err("the cue sheet has no tracks".into()); }

        let mut files = Vec::new();
        let mut tracks: Vec<Track> = Vec::new();
        let mut base = 0u32; // LBA of the current file's first byte
        for cf in &cue {
            let path: PathBuf = dir.join(&cf.name);
            let mut f = File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let (data_at, data_len) = if cf.wave { wave_data(&mut f)? } else { (0, f.metadata().map_err(|e| e.to_string())?.len()) };
            let fi = files.len();
            files.push(f);
            let mut shift = 0u32; // PREGAP silence inserted so far in this file
            for ct in &cf.tracks {
                let index1 = ct.index1.ok_or_else(|| format!("track {} has no INDEX 01", ct.number))?;
                shift += ct.pregap;
                let start = base + shift + index1;
                let silent_until = start - (index1 - ct.index0.unwrap_or(index1).min(index1));
                let pregap = silent_until - ct.pregap;
                tracks.push(Track {
                    number: ct.number, kind: ct.kind, start, pregap, length: 0, file: fi,
                    offset: data_at + index1 as u64 * ct.size as u64, silent_until, sector_size: ct.size,
                });
            }
            let size = cf.tracks.last().map_or(SECTOR as u32, |t| t.size);
            base += shift + (data_len / size as u64) as u32;
        }
        for i in 0..tracks.len() {
            let end = tracks.get(i + 1).map_or(base, |n| n.pregap);
            tracks[i].length = end.saturating_sub(tracks[i].start);
        }
        Ok(Self { files, tracks, leadout: base })
    }

    /// The track a sector belongs to (its pregap counts as part of it).
    pub fn track_at(&self, lba: u32) -> Option<&Track> {
        self.tracks.iter().rev().find(|t| lba >= t.pregap).filter(|_| lba < self.leadout)
    }

    /// One raw 2352-byte sector. Data tracks stored as 2048-byte sectors get a sync
    /// pattern and header made up for them; pregap silence is zeros. False past the end.
    pub fn read_sector(&mut self, lba: u32, out: &mut [u8; SECTOR]) -> bool {
        let Some(t) = self.track_at(lba).cloned() else { out.fill(0); return false; };
        out.fill(0);
        if lba < t.silent_until {
            if t.kind != TrackKind::Audio { make_header(lba, out); }
            return true;
        }
        let rel = lba as i64 - t.start as i64; // negative inside an INDEX 00 pregap
        let at = t.offset as i64 + rel * t.sector_size as i64;
        let f = &mut self.files[t.file];
        let ok = |r: std::io::Result<()>| r.is_ok();
        match t.kind {
            TrackKind::Mode1Cooked => {
                make_header(lba, out);
                ok(f.seek(SeekFrom::Start(at as u64)).and_then(|_| f.read_exact(&mut out[16..16 + 2048])))
            }
            _ => ok(f.seek(SeekFrom::Start(at as u64)).and_then(|_| f.read_exact(&mut out[..]))),
        }
    }

    /// 2048 bytes of user data from a data sector (mode 1: at 16; mode 2 form 1: at 24).
    pub fn read_data(&mut self, lba: u32) -> Option<[u8; 2048]> {
        let mut raw = [0u8; SECTOR];
        if !self.read_sector(lba, &mut raw) { return None; }
        let at = if raw[15] == 2 { 24 } else { 16 };
        let mut out = [0u8; 2048];
        out.copy_from_slice(&raw[at..at + 2048]);
        Some(out)
    }
}

fn bcd(v: u8) -> u8 { (v / 10) << 4 | (v % 10) }

/// Sync pattern + header (BCD MSF, mode 1) of a data sector.
fn make_header(lba: u32, out: &mut [u8; SECTOR]) {
    out[0] = 0;
    out[1..11].fill(0xFF);
    out[11] = 0;
    let (m, s, f) = lba_to_msf(lba);
    out[12] = bcd(m);
    out[13] = bcd(s);
    out[14] = bcd(f);
    out[15] = 1;
}

// ---------------- Sega CD disc header ----------------

/// What sector 0 says about a Sega CD / Mega-CD disc.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SegaCdHeader {
    pub title: String,
    /// 'U' (Americas), 'J' (Japan), 'E' (Europe)
    pub region: char,
}

/// Sector 0 of a Sega CD disc starts "SEGADISCSYSTEM"; the Genesis-style header at 0x100
/// has the titles (0x120 domestic, 0x150 overseas) and the region at 0x1F0.
pub fn sega_cd_header(sector0: &[u8; 2048]) -> Option<SegaCdHeader> {
    if !sector0.starts_with(b"SEGADISCSYSTEM") { return None; }
    let text = |a: usize| String::from_utf8_lossy(&sector0[a..a + 48]).replace('\0', " ").split_whitespace().collect::<Vec<_>>().join(" ");
    let title = { let t = text(0x150); if t.is_empty() { text(0x120) } else { t } };
    let r = &sector0[0x1F0..0x1F3];
    // letters (JUE), or the newer hex bitmask (1 = Japan, 4 = Americas, 8 = Europe)
    let region = if r.contains(&b'U') { 'U' }
        else if r.contains(&b'E') { 'E' }
        else if r.contains(&b'J') { 'J' }
        else {
            match (r[0] as char).to_digit(16).unwrap_or(4) {
                m if m & 4 != 0 => 'U',
                m if m & 8 != 0 => 'E',
                m if m & 1 != 0 => 'J',
                _ => 'U',
            }
        };
    Some(SegaCdHeader { title, region })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch folder for one test's files.
    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mimesys_disc_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn sector0() -> Vec<u8> {
        let mut s = vec![0u8; 2048];
        s[..16].copy_from_slice(b"SEGADISCSYSTEM  ");
        s[0x150..0x15A].copy_from_slice(b"TEST  DISC");
        s[0x1F0] = b'U';
        s
    }

    #[test]
    fn msf() {
        assert_eq!(lba_to_msf(0), (0, 2, 0));
        assert_eq!(lba_to_msf(75 * 60 - 150), (1, 0, 0));
        assert_eq!(parse_msf("01:02:03").unwrap(), (62 * 75) + 3);
    }

    #[test]
    fn iso_gets_a_made_up_header() {
        let d = tmp("iso");
        let mut img = sector0();
        img.extend(vec![0xAB; 2048]);
        std::fs::write(d.join("game.iso"), &img).unwrap();
        let mut disc = Disc::open(&d.join("game.iso")).unwrap();
        assert_eq!((disc.tracks.len(), disc.leadout), (1, 2));
        let mut raw = [0u8; SECTOR];
        assert!(disc.read_sector(1, &mut raw));
        assert_eq!(&raw[0..12], &[0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0]);
        assert_eq!(&raw[12..16], &[0x00, 0x02, 0x01, 0x01], "MSF 00:02:01, mode 1");
        assert_eq!(raw[16], 0xAB);
        let h = sega_cd_header(&disc.read_data(0).unwrap()).unwrap();
        assert_eq!(h, SegaCdHeader { title: "TEST DISC".into(), region: 'U' });
        assert!(!disc.read_sector(2, &mut raw), "past the end");
    }

    /// Data track (raw 2352) in one BIN, then two audio tracks: one in a WAV with a
    /// PREGAP, one sharing a BIN with an INDEX 00 pregap.
    #[test]
    fn cue_with_bin_wav_and_pregaps() {
        let d = tmp("cue");
        // track 1: 3 raw sectors
        let mut bin1 = Vec::new();
        for i in 0..3u8 {
            let mut s = vec![0u8; SECTOR];
            s[15] = 1;
            if i == 0 { s[16..16 + 2048].copy_from_slice(&sector0()); } else { s[16] = i; }
            bin1.extend(s);
        }
        std::fs::write(d.join("Game (Track 1).bin"), &bin1).unwrap();
        // track 2: WAV, 2 sectors of samples 0x22
        let pcm = vec![0x22u8; 2 * SECTOR];
        let mut wav = b"RIFF".to_vec();
        wav.extend(((36 + pcm.len()) as u32).to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16u32.to_le_bytes());
        wav.extend([1, 0, 2, 0, 0x44, 0xAC, 0, 0, 0x10, 0xB1, 2, 0, 4, 0, 16, 0]);
        wav.extend(b"data");
        wav.extend((pcm.len() as u32).to_le_bytes());
        wav.extend(&pcm);
        std::fs::write(d.join("t2.wav"), &wav).unwrap();
        // tracks 3 + 4 in one BIN: track 3 = 2 sectors (0x33), track 4 = 1 pregap (0x40) + 2 (0x44)
        let mut bin3 = vec![0x33u8; 2 * SECTOR];
        bin3.extend(vec![0x40u8; SECTOR]);
        bin3.extend(vec![0x44u8; 2 * SECTOR]);
        std::fs::write(d.join("t34.bin"), &bin3).unwrap();
        let cue = "REM test\n\
            FILE \"Game (Track 1).bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n\
            FILE \"t2.wav\" WAVE\n  TRACK 02 AUDIO\n    PREGAP 00:00:02\n    INDEX 01 00:00:00\n\
            FILE t34.bin BINARY\n  TRACK 03 AUDIO\n    INDEX 01 00:00:00\n  TRACK 04 AUDIO\n    INDEX 00 00:00:02\n    INDEX 01 00:00:03\n";
        std::fs::write(d.join("game.cue"), cue).unwrap();
        let mut disc = Disc::open(&d.join("game.cue")).unwrap();
        let starts: Vec<(u32, u32, u32)> = disc.tracks.iter().map(|t| (t.pregap, t.start, t.length)).collect();
        // track 1: 0-2; track 2: 2 silent pregap sectors (3, 4), audio at 5-6;
        // track 3: 7-8; track 4: pregap 9, audio 10-11; lead-out 12
        assert_eq!(starts, vec![(0, 0, 3), (3, 5, 2), (7, 7, 2), (9, 10, 2)]);
        assert_eq!(disc.leadout, 12);
        let mut raw = [0u8; SECTOR];
        let mut at = |disc: &mut Disc, lba: u32| { disc.read_sector(lba, &mut raw); (raw[0], raw[16], raw[SECTOR - 1]) };
        assert_eq!(at(&mut disc, 2).1, 2, "data sector 2");
        assert_eq!(at(&mut disc, 3), (0, 0, 0), "PREGAP silence");
        assert_eq!(at(&mut disc, 5), (0x22, 0x22, 0x22), "WAV data, past its header");
        assert_eq!(at(&mut disc, 8), (0x33, 0x33, 0x33));
        assert_eq!(at(&mut disc, 9), (0x40, 0x40, 0x40), "INDEX 00 pregap is in the file");
        assert_eq!(at(&mut disc, 11), (0x44, 0x44, 0x44));
        assert_eq!(disc.track_at(9).unwrap().number, 4);
        assert!(disc.track_at(12).is_none());
        assert_eq!(sega_cd_header(&disc.read_data(0).unwrap()).unwrap().region, 'U');
    }

    #[test]
    fn bad_cues_say_why() {
        let d = tmp("bad");
        assert!(Disc::from_cue("FILE \"x.mp3\" MP3\n  TRACK 01 AUDIO\n", &d).unwrap_err().contains("MP3"));
        assert!(Disc::from_cue("TRACK 01 AUDIO\n", &d).unwrap_err().contains("before FILE"));
        assert!(Disc::from_cue("FILE \"missing.bin\" BINARY\n  TRACK 01 MODE1/2352\n  INDEX 01 00:00:00\n", &d).unwrap_err().contains("missing.bin"));
    }
}
