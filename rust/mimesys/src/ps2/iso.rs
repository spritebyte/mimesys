//! What a disc is, from its ISO 9660 file system: the root directory's SYSTEM.CNF says
//! BOOT2 = ... for a PS2 game and BOOT = ... for a PS1 one; the boot file's name is the
//! serial (SLUS_200.62 -> SLUS-20062), which the drive's disc key comes from.
//!
//! CD or DVD: a disc with several tracks, or a raw (2352-byte) / CD CHD image, is a CD; a
//! 2048-byte image over 452 849 sectors is a DVD; a smaller one is a CD when its primary
//! volume descriptor repeats the block size at bytes 166 / 171 (CD mastering tools do,
//! DVD ones don't) — the same tests PCSX2 makes.

use crate::common::disc::{Disc, TrackKind};

/// Disc type register values (CDVD 1F40_200F)
pub const TYPE_PS1_CD: u8 = 0x10;
pub const TYPE_PS2_CD: u8 = 0x12;
pub const TYPE_PS2_DVD: u8 = 0x14;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscInfo {
    pub dvd: bool,
    /// BOOT2 (PS2) or BOOT (PS1) from SYSTEM.CNF
    pub ps2: bool,
    pub boot: String,
    /// "SLUS-20062" (empty when the boot file isn't named like a serial)
    pub serial: String,
    pub disc_type: u8,
}

/// A file in the root directory (name without ";1", any case): its first sector and size
fn root_file(d: &mut Disc, name: &str) -> Option<(u32, u32)> {
    let pvd = d.read_data(16)?;
    if &pvd[1..6] != b"CD001" { return None; }
    let root = &pvd[156..190];
    let (mut lba, size) = (u32::from_le_bytes(root[2..6].try_into().ok()?), u32::from_le_bytes(root[10..14].try_into().ok()?));
    let end = lba + size.div_ceil(2048);
    while lba < end {
        let s = d.read_data(lba)?;
        let mut at = 0usize;
        while at < 2048 {
            let len = s[at] as usize;
            if len == 0 { break; }
            let rec = &s[at..(at + len).min(2048)];
            if rec.len() >= 33 {
                let nl = rec[32] as usize;
                let n = String::from_utf8_lossy(&rec[33..(33 + nl).min(rec.len())]).to_string();
                let n = n.split(';').next().unwrap_or("");
                if n.eq_ignore_ascii_case(name) {
                    return Some((u32::from_le_bytes(rec[2..6].try_into().ok()?), u32::from_le_bytes(rec[10..14].try_into().ok()?)));
                }
            }
            at += len;
        }
        lba += 1;
    }
    None
}

/// A root-directory file's bytes (small files only)
pub fn read_root_file(d: &mut Disc, name: &str) -> Option<Vec<u8>> {
    let (lba, size) = root_file(d, name)?;
    if size > 1 << 20 { return None; }
    let mut out = Vec::new();
    for k in 0..size.div_ceil(2048) { out.extend_from_slice(&d.read_data(lba + k)?); }
    out.truncate(size as usize);
    Some(out)
}

/// CD or DVD (see the module notes)
pub fn is_dvd(d: &mut Disc) -> bool {
    if d.tracks.len() > 1 || d.tracks.first().is_some_and(|t| t.kind != TrackKind::Mode1Cooked) { return false; }
    if d.leadout > 452_849 { return true; }
    match d.read_data(16) {
        Some(p) => p[166..168] != p[171..173],
        None => true,
    }
}

/// SYSTEM.CNF's boot line: (PS2, path) for "BOOT2 = cdrom0:\SLUS_200.62;1"
fn boot_line(cnf: &[u8]) -> Option<(bool, String)> {
    let text = String::from_utf8_lossy(cnf);
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        let k = k.trim().to_ascii_uppercase();
        if k == "BOOT2" { return Some((true, v.trim().to_string())); }
        if k == "BOOT" { return Some((false, v.trim().to_string())); }
    }
    None
}

/// "cdrom0:\SLUS_200.62;1" -> "SLUS-20062"
pub fn serial_of(boot: &str) -> String {
    let file = boot.rsplit(['\\', '/', ':']).next().unwrap_or("");
    let file = file.split(';').next().unwrap_or("");
    let clean: String = file.chars().filter(|c| *c != '.').collect();
    match clean.split_once('_') {
        Some((a, b)) if a.len() == 4 && b.len() == 5 && b.chars().all(|c| c.is_ascii_digit()) => format!("{}-{b}", a.to_ascii_uppercase()),
        _ => String::new(),
    }
}

/// Everything above for a disc with SYSTEM.CNF; None for anything else
pub fn disc_info(d: &mut Disc) -> Option<DiscInfo> {
    let cnf = read_root_file(d, "SYSTEM.CNF")?;
    let (ps2, boot) = boot_line(&cnf)?;
    let dvd = is_dvd(d);
    let disc_type = if !ps2 { TYPE_PS1_CD } else if dvd { TYPE_PS2_DVD } else { TYPE_PS2_CD };
    Some(DiscInfo { dvd, ps2, serial: serial_of(&boot), boot, disc_type })
}

/// Whether the frontend's disc path blob is a PS2 disc (the reason it isn't goes to the
/// console, for when a disc is taken for something else)
pub fn is_ps2_disc(blob: &[u8]) -> bool {
    let Some(p) = crate::segacd::disc_path(blob) else { return false };
    let mut d = match Disc::open(&p) {
        Ok(d) => d,
        Err(e) => { println!("PS2 disc check: can't open {}: {e}", p.display()); return false; }
    };
    let pvd = d.read_data(16);
    match &pvd {
        None => println!("PS2 disc check: can't read sector 16 of {} ({} tracks, {} sectors)", p.display(), d.tracks.len(), d.leadout),
        Some(s) if &s[1..6] != b"CD001" => println!("PS2 disc check: sector 16 isn't an ISO 9660 volume descriptor ({:02X?})", &s[..8]),
        _ => {}
    }
    match read_root_file(&mut d, "SYSTEM.CNF") {
        None => { println!("PS2 disc check: no SYSTEM.CNF in the root directory"); false }
        Some(cnf) => match boot_line(&cnf) {
            Some((true, boot)) => { println!("PS2 disc check: BOOT2 = {boot}"); true }
            Some((false, _)) => false,
            None => { println!("PS2 disc check: SYSTEM.CNF has no BOOT2 / BOOT line: {:?}", String::from_utf8_lossy(&cnf[..cnf.len().min(80)])); false }
        },
    }
}

/// SYSTEM.CNF says BOOT2 (a PS2 disc), whatever else the detector thinks
pub fn says_boot2(d: &mut Disc) -> bool {
    read_root_file(d, "SYSTEM.CNF").and_then(|c| boot_line(&c)).is_some_and(|(ps2, _)| ps2)
}
