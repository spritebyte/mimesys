//! Stage 4 tests: GDI / CHD images, and the drive's packet commands with a disc.

use super::*;
use crate::common::disc::{Disc, TrackKind, SECTOR};
use std::path::PathBuf;

fn packet(g: &mut Gdrom, p: [u8; 12]) {
    g.write(0x9C, 0xA0);
    assert_eq!((g.status & DRQ, g.ireason), (DRQ, 1));
    for c in p.chunks(2) { g.write(0x80, u16::from_le_bytes([c[0], c[1]]) as u32); }
}

/// Read everything the drive offers by PIO, checking each piece's byte count and interrupt.
fn pio_all(g: &mut Gdrom) -> Vec<u8> {
    let mut out = Vec::new();
    while g.status & DRQ != 0 {
        assert!(g.irq, "each piece interrupts");
        assert_eq!(g.read(0x9C) as u8 & DRQ, DRQ);
        let n = g.byte_count as usize;
        assert!(n > 0);
        for _ in 0..n / 2 { out.extend((g.read(0x80) as u16).to_le_bytes()); }
    }
    out
}

/// User data byte `i` of sector `lba` in the test images.
fn byte(lba: u32, i: usize) -> u8 { (lba as usize * 7 + i * 13) as u8 ^ (i >> 8) as u8 }

/// A raw mode-1 sector (sync, header, data).
fn raw(lba: u32) -> [u8; SECTOR] {
    let mut s = [0u8; SECTOR];
    s[1..11].fill(0xFF);
    let f = lba + 150;
    let bcd = |v: u32| ((v / 10) << 4 | (v % 10)) as u8;
    s[12] = bcd(f / 75 / 60);
    s[13] = bcd(f / 75 % 60);
    s[14] = bcd(f % 75);
    s[15] = 1;
    for i in 0..2048 { s[16 + i] = byte(lba, i); }
    s
}

/// Audio sector: 16-bit little-endian samples.
fn audio(k: u32) -> [u8; SECTOR] {
    let mut s = [0u8; SECTOR];
    for (i, b) in s.iter_mut().enumerate() { *b = (i as u32 * 3 + k) as u8; }
    s
}

const T1: u32 = 4;
const T2: u32 = 3;
const T3: u32 = 20;

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("mimesys_gd_{name}_{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A 3-track GDI: data at 0, audio at 600, data at 45000 (with a space in its file name).
fn gdi(name: &str) -> PathBuf {
    let d = dir(name);
    let t1: Vec<u8> = (0..T1).flat_map(raw).collect();
    let t2: Vec<u8> = (0..T2).flat_map(audio).collect();
    let t3: Vec<u8> = (0..T3).flat_map(|k| raw(45000 + k)).collect();
    std::fs::write(d.join("track01.bin"), t1).unwrap();
    std::fs::write(d.join("track02.raw"), t2).unwrap();
    std::fs::write(d.join("track 03.bin"), t3).unwrap();
    let g = d.join("disc.gdi");
    std::fs::write(&g, "3\r\n1 0 4 2352 track01.bin 0\r\n2 600 0 2352 track02.raw 0\r\n3 45000 4 2352 \"track 03.bin\" 0\r\n").unwrap();
    g
}

/// The same disc as a CHD with GD-ROM track metadata (chdman's layout: frames of
/// 2352 + 96, tracks padded to 4 frames, audio big-endian, the gap before track 2 a pregap
/// that isn't in the file).
fn chd(name: &str) -> PathBuf {
    use chd::write::{ChdWriteOptions, ChdWriter};
    use std::io::Write;
    let p = dir(name).join("disc.chd");
    let mut frames: Vec<u8> = Vec::new();
    let mut put = |secs: Vec<[u8; SECTOR]>, swap: bool| {
        let n = secs.len();
        for mut s in secs {
            if swap { for c in s.chunks_exact_mut(2) { c.swap(0, 1); } }
            frames.extend_from_slice(&s);
            frames.extend_from_slice(&[0u8; 96]);
        }
        for _ in n..n.div_ceil(4) * 4 { frames.extend_from_slice(&[0u8; 2448]); }
    };
    put((0..T1).map(raw).collect(), false);
    put((0..T2).map(audio).collect(), true);
    put((0..T3).map(|k| raw(45000 + k)).collect(), false);
    let mut w = ChdWriter::new(std::fs::File::create(&p).unwrap(), ChdWriteOptions::cd()).unwrap();
    let meta = [
        format!("TRACK:1 TYPE:MODE1_RAW SUBTYPE:NONE FRAMES:{T1} PAD:0 PREGAP:0 PGTYPE:MODE1 PGSUB:NONE POSTGAP:0"),
        format!("TRACK:2 TYPE:AUDIO SUBTYPE:NONE FRAMES:{T2} PAD:0 PREGAP:{} PGTYPE:AUDIO PGSUB:NONE POSTGAP:0", 600 - T1),
        format!("TRACK:3 TYPE:MODE1_RAW SUBTYPE:NONE FRAMES:{T3} PAD:0 PREGAP:0 PGTYPE:MODE1 PGSUB:NONE POSTGAP:0"),
    ];
    for m in &meta {
        let mut b = m.as_bytes().to_vec();
        b.push(0);
        w.add_metadata(chd::metadata::KnownMetadata::GdRomTrack, b.as_slice(), true).unwrap();
    }
    w.write_all(&frames).unwrap();
    w.finish().unwrap();
    p
}

fn check_layout(d: &mut Disc) {
    assert!(d.gd);
    let starts: Vec<(u8, u32, TrackKind)> = d.tracks.iter().map(|t| (t.number, t.start, t.kind)).collect();
    assert_eq!(starts, vec![(1, 0, TrackKind::DataRaw), (2, 600, TrackKind::Audio), (3, 45000, TrackKind::DataRaw)]);
    assert_eq!(d.leadout, 45000 + T3);
    let s = d.read_data(45007).unwrap();
    assert!((0..2048).all(|i| s[i] == byte(45007, i)), "high-density data");
    assert_eq!(d.read_data(2).unwrap()[100], byte(2, 100));
    let mut a = [0u8; SECTOR];
    assert!(d.read_sector(601, &mut a));
    assert_eq!(a, audio(1), "audio comes back little-endian");
}

#[test]
fn gdi_tracks_and_reads() {
    let mut d = Disc::open(&gdi("layout")).unwrap();
    check_layout(&mut d);
}

#[test]
fn chd_tracks_and_reads() {
    let mut d = Disc::open(&chd("chdlayout")).unwrap();
    check_layout(&mut d);
    // across hunk boundaries (8 frames a hunk) in both directions
    for lba in [45019, 45000, 45008, 3, 45015] {
        assert_eq!(d.read_data(lba).unwrap()[5], byte(lba, 5), "lba {lba}");
    }
}

fn drive(name: &str) -> Gdrom {
    let mut g = Gdrom::new();
    g.disc = DiscSlot::new(Disc::open(&gdi(name)).unwrap());
    g
}

#[test]
fn no_disc_answers_not_ready() {
    let mut g = Gdrom::new();
    assert_eq!(g.read(0x8C), ST_NODISC as u32);
    packet(&mut g, [0x00; 12]);
    assert_eq!((g.status & CHECK, g.error >> 4), (CHECK, 2));
    packet(&mut g, [0x13, 0, 0, 0, 10, 0, 0, 0, 0, 0, 0, 0]);
    let d = pio_all(&mut g);
    assert_eq!((d[2], d[8]), (2, 0x3A));
    // SET_MODE / REQ_MODE round trip
    packet(&mut g, [0x12, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(g.ireason, 0);
    g.write(0x80, 0xBEEF);
    assert_eq!(g.status, DRDY | DSC);
    packet(&mut g, [0x11, 0, 0, 0, 4, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(g.read(0x80), 0xBEEF);
}

#[test]
fn disc_status_toc_and_sessions() {
    let mut g = drive("toc");
    assert_eq!(g.read(0x8C) as u8, 0x80 | ST_STANDBY, "GD-ROM, standby");
    packet(&mut g, [0x00; 12]);
    assert_eq!(g.status & CHECK, 0);
    // high-density TOC
    packet(&mut g, [0x14, 1, 0, 0x01, 0x98, 0, 0, 0, 0, 0, 0, 0]);
    let t = pio_all(&mut g);
    assert_eq!(t.len(), 408);
    assert_eq!(&t[0..4], &[0xFF; 4], "track 1 is in the other area");
    assert_eq!(&t[8..12], &[0x41, 0x00, 0xB0, 0x5E], "track 3 at FAD 45150");
    assert_eq!(&t[396..400], &[0x41, 3, 0, 0]);
    assert_eq!(&t[400..404], &[0x41, 3, 0, 0]);
    let end = 45150 + T3;
    assert_eq!(&t[404..408], &[0x41, (end >> 16) as u8, (end >> 8) as u8, end as u8]);
    // single-density TOC: data track 1, audio track 2
    packet(&mut g, [0x14, 0, 0, 0x01, 0x98, 0, 0, 0, 0, 0, 0, 0]);
    let t = pio_all(&mut g);
    assert_eq!(&t[0..4], &[0x41, 0, 0, 150]);
    assert_eq!(&t[4..8], &[0x01, 0, 0x02, 0xEE]);
    assert_eq!((t[397], t[401]), (1, 2));
    // REQ_SES: two sessions; the second starts at track 3
    packet(&mut g, [0x15, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(pio_all(&mut g)[2], 2);
    packet(&mut g, [0x15, 0, 2, 0, 6, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(&pio_all(&mut g)[2..6], &[3, 0x00, 0xB0, 0x5E]);
    // the security reply carries the GD-ROM result in its hidden first byte
    packet(&mut g, [0x70, 0x1F, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(g.status & CHECK, 0);
    packet(&mut g, [0x71, 0x1F, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let r = pio_all(&mut g);
    let skip = r[1] as usize - 2;
    // offset 1 holds the skip; the byte after the skipped ones is the result
    assert_eq!(r[1 + skip], 0x1F);
}

#[test]
fn cd_read_by_pio_in_limited_pieces() {
    let mut g = drive("pio");
    // a 0x800 limit: 3 sectors arrive as 3 pieces
    g.write(0x90, 0x00);
    g.write(0x94, 0x08);
    packet(&mut g, [0x30, 0x20, 0x00, 0xB0, 0x5E + 2, 0, 0, 0, 0, 0, 3, 0]);
    let mut pieces = 0;
    let mut out = Vec::new();
    while g.status & DRQ != 0 {
        assert_eq!(g.byte_count, 0x800);
        pieces += 1;
        g.read(0x9C);
        for _ in 0..0x400 { out.extend((g.read(0x80) as u16).to_le_bytes()); }
    }
    assert_eq!(pieces, 3);
    assert!(g.irq, "completion interrupt");
    assert_eq!(g.status, DRDY | DSC);
    for k in 0..3u32 {
        assert!((0..2048).all(|i| out[k as usize * 2048 + i] == byte(45002 + k, i)), "sector {k}");
    }
    // REQ_STAT follows the read: paused at the next FAD on track 3
    packet(&mut g, [0x10, 0, 0, 0, 10, 0, 0, 0, 0, 0, 0, 0]);
    let s = pio_all(&mut g);
    assert_eq!(&s[..8], &[ST_PAUSE, 0x80, 0x41, 3, 1, 0x00, 0xB0, 0x63]);
    // raw sectors (data select F)
    packet(&mut g, [0x30, 0xF0, 0x00, 0xB0, 0x5E, 0, 0, 0, 0, 0, 1, 0]);
    let r = pio_all(&mut g);
    assert_eq!(r.len(), SECTOR);
    assert_eq!(r[..], raw(45000)[..]);
    // past the end: an error
    packet(&mut g, [0x30, 0x20, 0x00, 0xB0, 0x5E + T3 as u8, 0, 0, 0, 0, 0, 1, 0]);
    assert_eq!(g.status & CHECK, CHECK);
}

#[test]
fn cd_read_by_dma() {
    let mut g = drive("dma");
    g.write(0x84, 1); // DMA
    packet(&mut g, [0x30, 0x20, 0x00, 0xB0, 0x5E, 0, 0, 0, 0, 0, 2, 0]);
    assert!(!g.irq, "no interrupt until the DMA ends");
    assert_eq!(g.read(0x80), 0, "no PIO data");
    let a = g.dma_take(3000);
    let b = g.dma_take(3000);
    assert_eq!((a.len(), b.len()), (3000, 1096));
    assert_eq!(a[2048 + 7], byte(45001, 7));
    g.dma_finish();
    assert!(g.irq);
    assert_eq!(g.status, DRDY | DSC);
}

#[test]
fn reset_keeps_the_disc() {
    let mut g = drive("reset");
    g.write(0x18, 4);
    assert!(g.has_disc());
    g.write(0x9C, 0x08);
    assert_eq!(g.byte_count, 0xEB14);
    assert!(g.has_disc());
}

#[test]
fn g1_dma_copies_to_ram_and_signals_the_end() {
    use crate::common::cpu::sh4::Sh4Bus;
    use crate::dreamcast::{bus::Dc, holly};
    let mut d = Dc::new(vec![0; crate::dreamcast::bus::BIOS_SIZE], &[], 0);
    d.gd.disc = DiscSlot::new(Disc::open(&gdi("bus")).unwrap());
    d.write8(0xA05F_7084, 1); // features: DMA
    d.write8(0xA05F_709C, 0xA0);
    let p = [0x30u8, 0x20, 0x00, 0xB0, 0x5E, 0, 0, 0, 0, 0, 2, 0];
    for c in p.chunks(2) { d.write16(0xA05F_7080, u16::from_le_bytes([c[0], c[1]])); }
    d.write32(0xA05F_7404, 0x0C01_0000); // GDSTAR
    d.write32(0xA05F_7408, 4096); // GDLEN
    d.write32(0xA05F_740C, 1); // GDDIR: to memory
    d.write32(0xA05F_7414, 1); // GDEN
    d.write32(0xA05F_7418, 1); // GDST
    assert_eq!(d.read32(0xA05F_7418), 1, "busy");
    assert_eq!(d.read32(0xA05F_74F8), 4096, "GDLEND");
    assert_eq!(d.ram[0x1_0000 + 2048 + 9], byte(45001, 9));
    let t0 = d.now;
    while d.read32(0xA05F_7418) == 1 && d.now < t0 + 1_000_000 { d.now = d.next_event(); d.service(); }
    assert_eq!(d.read32(0xA05F_7418), 0, "done");
    assert!(d.now > t0 + 4096, "takes time");
    assert!(d.holly.istnrm() & holly::IST_DMA_GDROM != 0);
    assert!(d.gd.irq, "the drive's completion interrupt");
    assert_eq!(d.gd.status, DRDY | DSC);
}

#[test]
fn redump_cue_puts_the_high_density_area_at_45000() {
    let g = gdi("cue");
    let d = g.parent().unwrap();
    let cue = "REM SINGLE-DENSITY AREA\nFILE \"track01.bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n\
               FILE \"track02.raw\" BINARY\n  TRACK 02 AUDIO\n    INDEX 00 00:00:00\n    INDEX 01 00:02:00\n\
               REM HIGH-DENSITY AREA\nFILE \"track 03.bin\" BINARY\n  TRACK 03 MODE1/2352\n    INDEX 01 00:00:00\n";
    std::fs::write(d.join("disc.cue"), cue).unwrap();
    let mut disc = Disc::open(&d.join("disc.cue")).unwrap();
    assert!(disc.gd);
    assert_eq!(disc.tracks[2].start, 45000);
    assert_eq!(disc.read_data(45011).unwrap()[77], byte(45011, 77));
}

#[test]
fn cd_play_streams_the_audio_track() {
    let mut g = drive("cdda");
    // track 2 (audio) is at LBA 600: FAD 750 .. 753 (3 sectors), no repeat
    packet(&mut g, [0x20, 1, 0x00, 0x02, 0xEE, 0, 0, 0, 0x00, 0x02, 0xF1, 0]);
    assert_eq!(g.read(0x8C) as u8 & 0xF, ST_PLAY);
    let first = g.cdda();
    let a = audio(0);
    assert_eq!(first, [i16::from_le_bytes([a[0], a[1]]), i16::from_le_bytes([a[2], a[3]])]);
    for _ in 1..588 { g.cdda(); }
    let b = audio(1);
    assert_eq!(g.cdda(), [i16::from_le_bytes([b[0], b[1]]), i16::from_le_bytes([b[2], b[3]])], "next sector");
    packet(&mut g, [0x40, 1, 0, 0, 14, 0, 0, 0, 0, 0, 0, 0]);
    let s = pio_all(&mut g);
    assert_eq!((s[1], s[4], s[5]), (0x11, 0x01, 2), "playing, audio track 2");
    for _ in 0..588 * 3 { g.cdda(); }
    assert_eq!(g.cdda(), [0, 0], "finished");
    assert_eq!(g.read(0x8C) as u8 & 0xF, ST_PAUSE);
    packet(&mut g, [0x40, 0, 0, 0, 100, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(pio_all(&mut g)[1], 0x13);
}
