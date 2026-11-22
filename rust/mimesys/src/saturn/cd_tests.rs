//! The CD block with a disc: TOC, the file system, reading a file through a filter and
//! the data port, CD-DA
use super::cdblock::{self, CdBlock};
use crate::common::disc::Disc;
use std::sync::{Arc, Mutex};

const FILE_LBA: u32 = 30;

fn dir() -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("mimesys_sscd_{}_{:?}", std::process::id(), std::thread::current().id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A data track of 40 cooked sectors: the system ID, a PVD, a root directory with one
/// file of 4096 bytes at sector 30 (byte i of the file = i * 7)
fn data_track() -> Vec<u8> {
    let mut img = vec![0u8; 40 * 2048];
    img[..16].copy_from_slice(b"SEGA SEGASATURN ");
    let pvd = &mut img[16 * 2048..17 * 2048];
    pvd[0] = 1;
    pvd[1..6].copy_from_slice(b"CD001");
    pvd[158..162].copy_from_slice(&20u32.to_le_bytes());     // root at 20
    pvd[166..170].copy_from_slice(&2048u32.to_le_bytes());
    pvd[181] = 2;
    let term = 17 * 2048;
    img[term] = 0xFF;
    img[term + 1..term + 6].copy_from_slice(b"CD001");
    let root = &mut img[20 * 2048..21 * 2048];
    let mut at = 0;
    for (lba, len, flags, name) in [(20u32, 2048u32, 2u8, &b"\0"[..]), (20, 2048, 2, &b"\x01"[..]), (FILE_LBA, 4096, 0, &b"FILE.BIN;1"[..])] {
        let n = 33 + name.len() + (name.len() + 1) % 2;
        root[at] = n as u8;
        root[at + 2..at + 6].copy_from_slice(&lba.to_le_bytes());
        root[at + 10..at + 14].copy_from_slice(&len.to_le_bytes());
        root[at + 25] = flags;
        root[at + 32] = name.len() as u8;
        root[at + 33..at + 33 + name.len()].copy_from_slice(name);
        at += n;
    }
    for i in 0..4096 { img[FILE_LBA as usize * 2048 + i] = (i * 7) as u8; }
    img
}

fn iso() -> Arc<Mutex<Disc>> {
    let p = dir().join("game.iso");
    std::fs::write(&p, data_track()).unwrap();
    Arc::new(Mutex::new(Disc::open(&p).unwrap()))
}

/// Data then a 10-sector audio track whose samples count up
fn cue_with_audio() -> Arc<Mutex<Disc>> {
    let d = dir();
    std::fs::write(d.join("data.bin"), data_track()).unwrap();
    let mut audio = Vec::new();
    for i in 0..10 * 588u32 { audio.extend_from_slice(&(i as i16).to_le_bytes()); audio.extend_from_slice(&(-(i as i16)).to_le_bytes()); }
    std::fs::write(d.join("audio.bin"), audio).unwrap();
    let cue = "FILE \"data.bin\" BINARY\n  TRACK 01 MODE1/2048\n    INDEX 01 00:00:00\nFILE \"audio.bin\" BINARY\n  TRACK 02 AUDIO\n    INDEX 01 00:00:00\n";
    std::fs::write(d.join("game.cue"), cue).unwrap();
    Arc::new(Mutex::new(Disc::open(&d.join("game.cue")).unwrap()))
}

/// Let time pass in 1 ms steps
fn run(c: &mut CdBlock, ms: u32) { for _ in 0..ms { c.tick(1_000_000_000); } }

/// Issue a command and wait for CMOK
fn cmd(c: &mut CdBlock, cr: [u16; 4]) -> [u16; 4] {
    c.write(0x90008, !cdblock::CMOK);
    for (k, v) in cr.into_iter().enumerate() { c.write(0x90018 + k as u32 * 4, v); }
    for _ in 0..200 {
        c.tick(100_000_000);
        if c.hirq & cdblock::CMOK != 0 { break; }
    }
    assert_ne!(c.hirq & cdblock::CMOK, 0, "command {:04X} answered", cr[0]);
    let out = [c.read(0x90018), c.read(0x9001C), c.read(0x90020), c.read(0x90024)];
    out
}

fn data(c: &mut CdBlock, words: usize) -> Vec<u16> { (0..words).map(|_| c.read(0x18000)).collect() }

#[test]
fn with_a_disc_the_drive_pauses_and_gives_its_toc() {
    let mut c = CdBlock::new(Some(cue_with_audio()));
    run(&mut c, 20);
    let r = cmd(&mut c, [0x0000, 0, 0, 0]);
    assert_eq!(r[0] & 0x0F00, 0x0100, "PAUSE");
    let r = cmd(&mut c, [0x0200, 0, 0, 0]);
    assert_eq!(r[1], 204);
    let toc = data(&mut c, 204);
    assert_eq!((toc[0], toc[1]), (0x4100, 150), "track 1: data at FAD 150");
    assert_eq!((toc[2], toc[3]), (0x0100, 150 + 40), "track 2: audio");
    assert_eq!(toc[99 * 2] >> 8, 0x41);
    assert_eq!(toc[100 * 2], 0x0102, "last track 2");
    assert_eq!(toc[101 * 2 + 1], 150 + 50, "lead-out");
}

#[test]
fn the_file_system_lists_and_reads_a_file() {
    let mut c = CdBlock::new(Some(iso()));
    run(&mut c, 20);
    cmd(&mut c, [0x7000, 0, 0x00FF, 0xFFFF]);       // change to the root
    let r = cmd(&mut c, [0x7200, 0, 0, 0]);         // file scope
    assert_eq!((r[1], r[3]), (3, 2), "3 entries, the first file is #2");
    let r = cmd(&mut c, [0x7300, 0, 0, 2]);         // file info #2
    assert_eq!(r[1], 6);
    let fi = data(&mut c, 6);
    assert_eq!(((fi[0] as u32) << 16 | fi[1] as u32, (fi[2] as u32) << 16 | fi[3] as u32), (FILE_LBA + 150, 4096));
    // read it through filter 0 into partition 0, then get and delete both sectors
    cmd(&mut c, [0x4800, 0, 0x0000, 0]);            // clear partition 0
    cmd(&mut c, [0x7400, 0, 0x0000, 2]);
    run(&mut c, 40);
    let r = cmd(&mut c, [0x5100, 0, 0x0000, 0]);
    assert_eq!(r[3], 2, "two sectors buffered");
    assert_ne!(c.read(0x90008) & cdblock::EFLS, 0, "file read done");
    cmd(&mut c, [0x6300, 0, 0x0000, 2]);
    let words = data(&mut c, 2048);
    for (i, w) in words.iter().enumerate() {
        let (a, b) = (((i * 2) * 7) as u8, ((i * 2 + 1) * 7) as u8);
        assert_eq!(*w, (a as u16) << 8 | b as u16, "word {i}");
    }
    let r = cmd(&mut c, [0x0600, 0, 0, 0]);
    assert_eq!(r[1], 2048, "words moved");
    let r = cmd(&mut c, [0x5100, 0, 0x0000, 0]);
    assert_eq!(r[3], 0, "deleted");
}

#[test]
fn playing_an_audio_track_feeds_cd_da() {
    let mut c = CdBlock::new(Some(cue_with_audio()));
    run(&mut c, 20);
    cmd(&mut c, [0x1000, 0x0201, 0x0000, 0x0201]);  // track 2 index 1 to the end of track 2
    run(&mut c, 200);
    assert!(c.audio.len() > 588 * 3, "{} samples", c.audio.len());
    let (l, r) = c.audio[1];
    assert_eq!((l, r), (1, -1), "the first sector's samples");
    let r = cmd(&mut c, [0x0000, 0, 0, 0]);
    assert_eq!(r[1] & 0xFF, 2, "on track 2");
}

#[test]
fn no_disc_still_answers() {
    let mut c = CdBlock::new(None);
    run(&mut c, 20);
    let r = cmd(&mut c, [0x0100, 0, 0, 0]);
    assert_eq!(r[0] & 0x0F00, 0x0700, "NODISC");
}

#[test]
fn play_after_a_stop_seeks_from_the_start() {
    let mut c = CdBlock::new(Some(cue_with_audio()));
    run(&mut c, 20);
    cmd(&mut c, [0x1100, 0, 0, 0]);             // seek with no position: stop (standby)
    run(&mut c, 50);
    cmd(&mut c, [0x1080, 160, 0x0080, 4]);      // play FAD 160, 4 sectors
    run(&mut c, 200);
    let r = cmd(&mut c, [0x0000, 0, 0, 0]);
    assert_ne!(r[0] & 0x0F00, 0x0400, "not stuck seeking (status {:04X})", r[0]);
}
