//! Sega CD / Mega-CD.
//!
//! Step 1 (this file): find the disc and the BIOS. The frontend can't hand a whole disc
//! over as bytes (hundreds of MB), so for a .cue / .iso it sends "MCDP" + the file's path
//! and the core reads sectors from disk itself (`common::disc`). The BIOS files come in
//! an "MCDB" bundle (`common::firmware`): every Sega CD BIOS the player has; the core
//! picks the one for the disc's region.
//!
//! Next steps: the second 68000 and the gate array, the CD drive and decoder, PCM sound
//! and CD audio, the graphics chip (ported from ares, ISC license).

pub mod cdc;
pub mod cdd;
pub mod gpu;
pub mod mcd;
pub mod pcm;

use crate::common::disc::{sega_cd_header, Disc, SegaCdHeader};
use std::path::PathBuf;

pub const PATH_MAGIC: &[u8; 4] = b"MCDP";
pub const BIOS_MAGIC: &[u8; 4] = b"MCDB";
pub const BIOS_SIZE: usize = 0x20000;

/// The disc path from the frontend's "MCDP" blob.
pub fn disc_path(blob: &[u8]) -> Option<PathBuf> {
    let rest = blob.strip_prefix(PATH_MAGIC.as_slice())?;
    Some(PathBuf::from(String::from_utf8_lossy(rest).into_owned()))
}

/// Open the disc and read its header; an error when it isn't a Sega CD disc.
pub fn open_disc(blob: &[u8]) -> Result<(Disc, SegaCdHeader), String> {
    let path = disc_path(blob).ok_or("not a disc image path")?;
    let mut disc = Disc::open(&path)?;
    let first = disc.tracks.first().ok_or("the disc has no tracks")?;
    if first.is_audio() { return Err("track 1 is audio: not a Sega CD game disc".into()); }
    let s0 = disc.read_data(0).ok_or("can't read sector 0")?;
    let head = sega_cd_header(&s0).ok_or("sector 0 doesn't start with SEGADISCSYSTEM: not a Sega CD disc")?;
    Ok((disc, head))
}

/// Which region a BIOS is for: its file name first (bios_CD_U.bin, "(USA)", "_jp" ...),
/// then the region byte of its header.
pub fn bios_region(name: &str, data: &[u8]) -> Option<char> {
    let n = name.to_lowercase();
    let stem = n.rsplit_once('.').map_or(n.as_str(), |(s, _)| s);
    let has = |keys: &[&str]| keys.iter().any(|k| stem.contains(k));
    if has(&["_j", "(j", "japan", " jp"]) { return Some('J'); }
    if has(&["_e", "(e", "europe", " eu", "pal"]) { return Some('E'); }
    if has(&["_u", "(u", "usa", " us", "america"]) { return Some('U'); }
    if stem.contains("segacd") || stem.contains("sega cd") { return Some('U'); } // the US name
    let r = data.get(0x1F0..0x1F3)?;
    if r.contains(&b'U') { Some('U') } else if r.contains(&b'E') { Some('E') } else if r.contains(&b'J') { Some('J') } else { None }
}

/// The BIOS for `region` from the bundle: an exact region match, else any (with a
/// warning: a BIOS from another region refuses most discs). (name, data, its region)
pub fn pick_bios(bundle: &[u8], region: char) -> Result<(String, Vec<u8>, char), String> {
    let files: Vec<(String, Vec<u8>, char)> = crate::common::firmware::files(bundle, BIOS_MAGIC)
        .into_iter()
        .filter(|(_, d)| d.len() == BIOS_SIZE)
        .map(|(n, d)| { let r = bios_region(&n, &d).unwrap_or('?'); (n, d, r) })
        .collect();
    if files.is_empty() {
        return Err("no Sega CD BIOS: put one in user://BIOS/ (e.g. bios_CD_U.bin, 128 KB)".into());
    }
    let pos = files.iter().position(|f| f.2 == region).unwrap_or(0);
    Ok(files.into_iter().nth(pos).unwrap())
}

/// A region letter as words.
pub fn region_name(r: char) -> &'static str {
    match r { 'U' => "Americas", 'E' => "Europe", 'J' => "Japan", _ => "unknown" }
}

/// Load a Sega CD game: check the disc and the BIOS, report them, and start the console.
pub fn load(blob: &[u8], filename: &str, bios: &[u8]) -> Result<Box<dyn crate::common::system::System>, String> {
    let (disc, head) = open_disc(blob)?;
    println!("Sega CD: \"{}\", region {} ({}), {} tracks, {} sectors", head.title, head.region, region_name(head.region), disc.tracks.len(), disc.leadout);
    for t in &disc.tracks {
        let (m, s, f) = crate::common::disc::lba_to_msf(t.start);
        println!("  track {:2} {:5} at {:02}:{:02}:{:02} (LBA {}), {} sectors{}", t.number,
                 if t.is_audio() { "audio" } else { "data" }, m, s, f, t.start, t.length,
                 if t.pregap < t.start { format!(", pregap {}", t.start - t.pregap) } else { String::new() });
    }
    let (name, data, r) = pick_bios(bios, head.region)?;
    if r != head.region {
        println!("Sega CD: no {} BIOS; using {name} ({}): the disc will probably refuse to start", region_name(head.region), region_name(r));
    } else {
        println!("Sega CD: BIOS {name} ({})", region_name(r));
    }
    println!("Sega CD: work in progress: the CD drive isn't emulated yet, so games won't start");
    let sys = crate::genesis::md_system::GenesisSystem::from_cd(data, disc, head.region, &head.title, filename)?;
    Ok(Box::new(sys))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bios_region_from_name_then_header() {
        let mut d = vec![0u8; BIOS_SIZE];
        assert_eq!(bios_region("bios_CD_U.bin", &d), Some('U'));
        assert_eq!(bios_region("bios_CD_E.bin", &d), Some('E'));
        assert_eq!(bios_region("bios_CD_J.bin", &d), Some('J'));
        assert_eq!(bios_region("Mega-CD 2 (Japan).bin", &d), Some('J'));
        assert_eq!(bios_region("segacd_jp.bin", &d), Some('J'));
        assert_eq!(bios_region("Sega CD 2.bin", &d), Some('U'));
        assert_eq!(bios_region("whatever.bin", &d), None);
        d[0x1F0] = b'E';
        assert_eq!(bios_region("whatever.bin", &d), Some('E'));
    }

    #[test]
    fn pick_bios_prefers_the_disc_region() {
        let u = vec![1u8; BIOS_SIZE];
        let j = vec![2u8; BIOS_SIZE];
        let small = vec![3u8; 100];
        let b = crate::common::firmware::bundle(BIOS_MAGIC, &[("bios_cd_u.bin", &u), ("bios_cd_j.bin", &j), ("bios_cd_e.bin", &small)]);
        assert_eq!(pick_bios(&b, 'J').unwrap().1[0], 2);
        assert_eq!(pick_bios(&b, 'U').unwrap().1[0], 1);
        let (_, d, r) = pick_bios(&b, 'E').unwrap();
        assert_eq!((d[0], r), (1, 'U'), "no (full-size) European BIOS: falls back");
        assert!(pick_bios(b"MCDB", 'U').unwrap_err().contains("BIOS"));
    }

    #[test]
    fn disc_path_blob() {
        assert_eq!(disc_path(b"MCDP/games/Sonic CD.cue").unwrap(), PathBuf::from("/games/Sonic CD.cue"));
        assert!(disc_path(b"SEGA").is_none());
    }

    /// An .iso with a Sega CD header, through the detector and the loader.
    #[test]
    fn detect_and_load_a_disc() {
        let d = std::env::temp_dir().join(format!("mimesys_mcd_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let mut s0 = vec![0u8; 2048 * 4];
        s0[..16].copy_from_slice(b"SEGADISCSYSTEM  ");
        s0[0x150..0x157].copy_from_slice(b"MY GAME");
        s0[0x1F0] = b'J';
        let iso = d.join("my game.iso");
        std::fs::write(&iso, &s0).unwrap();
        let mut blob = PATH_MAGIC.to_vec();
        blob.extend(iso.to_string_lossy().as_bytes());
        let g = crate::common::rom_detect::detect(&blob, "my game.iso");
        assert_eq!((g[0].system, g[0].score), ("mcd", 100));
        assert!(g[0].why.contains("MY GAME"));
        // no BIOS: says so
        assert!(load(&blob, "my game", b"").err().unwrap().contains("no Sega CD BIOS"));
        // with one: disc + BIOS fine, console not there yet
        let b = crate::common::firmware::bundle(BIOS_MAGIC, &[("bios_cd_j.bin", &vec![0u8; BIOS_SIZE])]);
        assert!(load(&blob, "my game", &b).is_ok(), "starts (the BIOS is all zeros: it won't get far)");
        // not a Sega CD disc
        std::fs::write(&iso, vec![0u8; 2048]).unwrap();
        assert!(crate::common::rom_detect::detect(&blob, "my game.iso").is_empty());
    }
}

#[cfg(test)]
mod cpu_tests {
    use super::mcd::*;
    use super::BIOS_SIZE;
    use crate::common::system::System;

    /// move.w #imm,(abs).l
    fn move_w(out: &mut Vec<u16>, imm: u16, addr: u32) { out.extend([0x33FC, imm, (addr >> 16) as u16, addr as u16]); }

    fn iso() -> crate::common::disc::Disc {
        let d = std::env::temp_dir().join(format!("mimesys_mcdcpu_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let mut s0 = vec![0u8; 2048];
        s0[..16].copy_from_slice(b"SEGADISCSYSTEM  ");
        std::fs::write(d.join("t.iso"), &s0).unwrap();
        crate::common::disc::Disc::open(&d.join("t.iso")).unwrap()
    }

    /// A made-up BIOS: the Genesis copies a program into PRG-RAM, lets the sub CPU run,
    /// passes it a command word and interrupts it; the sub answers in its status words.
    #[test]
    fn both_cpus_talk() {
        // the sub CPU's program (PRG-RAM 0): vectors, main code at 100, level-2 handler at 200
        let mut sub: Vec<(u32, u16)> = vec![(0x0, 0x0007), (0x2, 0xFFF0), (0x4, 0x0000), (0x6, 0x0100), (0x68, 0x0000), (0x6A, 0x0200)];
        let code = [
            0x33FC, 0x0004, 0x00FF, 0x8032,          // 100 move.w #4,$FF8032    (level 2 on)
            0x46FC, 0x2000,                          // 108 move #$2000,sr
            0x33FC, 0x1234, 0x00FF, 0x8020,          // 10C move.w #$1234,$FF8020 (status 0)
            0x33F9, 0x00FF, 0x8010, 0x00FF, 0x8022,  // 114 move.w $FF8010,$FF8022 (command 0 -> status 1)
            0x60F4,                                  // 11E bra 114
        ];
        for (i, w) in code.iter().enumerate() { sub.push((0x100 + i as u32 * 2, *w)); }
        for (i, w) in [0x33FC, 0x5555, 0x00FF, 0x8024, 0x4E73].iter().enumerate() { sub.push((0x200 + i as u32 * 2, *w)); } // status 2 = 5555; rte

        let mut main: Vec<u16> = vec![0x13FC, 0x0002, 0x00A1, 0x2001];  // move.b #2,$A12001 (hold reset, BUSREQ)
        for (a, w) in &sub { move_w(&mut main, *w, 0x020000 + a); }   // PRG-RAM through the 020000 window
        main.extend([0x13FC, 0x0001, 0x00A1, 0x2001]);                  // move.b #1,$A12001 (run)
        move_w(&mut main, 0xBEEF, 0xA12010);                             // command 0
        main.extend([0x303C, 0x0FFF, 0x51C8, 0xFFFE]);                  // a short wait
        main.extend([0x13FC, 0x0001, 0x00A1, 0x2000]);                  // move.b #1,$A12000 (interrupt the sub)
        main.push(0x60FE);                                                // bra *

        let mut bios = vec![0u8; BIOS_SIZE];
        bios[0..8].copy_from_slice(&[0xFF, 0xFF, 0xFE, 0x00, 0x00, 0x00, 0x02, 0x00]); // SSP, PC = 200
        bios[0x100..0x104].copy_from_slice(b"SEGA");
        for (i, w) in main.iter().enumerate() { bios[0x200 + i * 2..0x202 + i * 2].copy_from_slice(&w.to_be_bytes()); }

        let mut sys = crate::genesis::md_system::GenesisSystem::from_cd(bios, iso(), 'U', "test", "test").unwrap();
        for _ in 0..10 { sys.run_frame(0); }
        let m = sys.bus.mcd.as_ref().unwrap();
        assert_eq!(m.st.status[0], 0x1234, "the sub CPU ran its program");
        assert_eq!(m.st.status[1], 0xBEEF, "and saw the command word");
        assert_eq!(m.st.status[2], 0x5555, "and took the level-2 interrupt from the Genesis");
        assert!(!m.st.irq[2].pending, "acknowledged");
        use crate::common::cpu::m68000::M68kBus;
        assert_eq!(sys.bus.read_word(0xA12020), 0x1234, "the Genesis reads the status words");
        assert_eq!(sys.bus.read_byte(0xA10001) & 0x20, 0, "version register: expansion unit present");
        assert_eq!(sys.bus.read_word(0x000100), 0x5345, "BIOS at 0");
        let n = sys.drain_audio().len();
        assert!(n > 1400 && n % 2 == 0, "the CD unit's sound joins the mix ({n})");
        assert!(sys.bus.mcd.as_ref().unwrap().st.audio.len() < 200, "and is used up");
    }

    /// Local only: 600 frames with both CPUs busy.
    #[test]
    #[ignore]
    fn zz_mcd_speed() {
        let mut bios = vec![0u8; BIOS_SIZE];
        bios[0..8].copy_from_slice(&[0xFF, 0xFF, 0xFE, 0x00, 0x00, 0x00, 0x02, 0x00]);
        bios[0x100..0x104].copy_from_slice(b"SEGA");
        // main: hand the sub a program that loops (bra *), run it, then loop on a register read
        let mut main: Vec<u16> = vec![0x13FC, 0x0002, 0x00A1, 0x2001];
        for (a, w) in [(0u32, 0x0007u16), (2, 0xFFF0), (4, 0), (6, 0x0100), (0x100, 0x4E71), (0x102, 0x60FC)] { move_w(&mut main, w, 0x020000 + a); }
        main.extend([0x13FC, 0x0001, 0x00A1, 0x2001, 0x3039, 0x00A1, 0x2020, 0x60F8]);
        for (i, w) in main.iter().enumerate() { bios[0x200 + i * 2..0x202 + i * 2].copy_from_slice(&w.to_be_bytes()); }
        let mut sys = crate::genesis::md_system::GenesisSystem::from_cd(bios, iso(), 'U', "t", "t").unwrap();
        let t = std::time::Instant::now();
        for _ in 0..600 { sys.run_frame(0); }
        println!("600 frames: {:?} ({:.2} ms a frame)", t.elapsed(), t.elapsed().as_secs_f64() * 1000.0 / 600.0);
    }

    #[test]
    fn blank_save_gets_formatted_real_one_kept() {
        let mut bios = vec![0u8; BIOS_SIZE];
        bios[0x100..0x104].copy_from_slice(b"SEGA");
        let mut sys = crate::genesis::md_system::GenesisSystem::from_cd(bios, iso(), 'U', "t", "t").unwrap();
        sys.load_sram(&[0u8; 0x2000]);
        assert!(bram_formatted(sys.get_sram().unwrap()), "an all-zero save from before");
        let mut save = sys.get_sram().unwrap().to_vec();
        save[0x10] = 0x77;                    // a formatted save with something in it
        sys.load_sram(&save);
        assert_eq!(sys.get_sram().unwrap()[0x10], 0x77);
    }

    #[test]
    fn word_ram_2m_handover() {
        let mut m = Mcd::new(vec![0; BIOS_SIZE], None);
        m.ext_write(0x200010, true, true, 0xCAFE);
        assert_eq!(m.ext_read(0x200010, false), Some(0xCAFE), "the Genesis has it at power on");
        m.ext_io_write(0xA12002, false, true, 0x0002); // DMNA: give it to the sub CPU
        assert_eq!(m.ext_read(0x200010, false), None, "now the Genesis can't see it");
        assert_eq!(m.ext_io_read(0xA12002) & 3, 2, "A12002: sub has it");
        let mut sub = SubBus(&mut m.st);
        use crate::common::cpu::m68000::M68kBus;
        assert_eq!(sub.read_word(0x080010), 0xCAFE);
        sub.write_word(0x080012, 0x1111);
        sub.write_word(0xFF8002, 0x0001);              // RET: back to the Genesis
        assert_eq!(m.ext_read(0x200012, false), Some(0x1111));
        // a VDP DMA gets word RAM one access late
        assert_eq!(m.ext_read(0x200010, true), Some(0x0000));
        assert_eq!(m.ext_read(0x200012, true), Some(0xCAFE));
    }

    #[test]
    fn word_ram_1m_banks_and_dot_map() {
        use crate::common::cpu::m68000::M68kBus;
        let mut m = Mcd::new(vec![0; BIOS_SIZE], None);
        SubBus(&mut m.st).write_word(0xFF8002, 0x0004); // 1M mode, Genesis has bank 0
        m.ext_write(0x200000, true, true, 0xAAAA);
        SubBus(&mut m.st).write_word(0x0C0000, 0x5BCD); // the sub's bank (1)
        assert_eq!(m.ext_read(0x200000, false), Some(0xAAAA), "separate banks");
        assert_eq!(SubBus(&mut m.st).read_word(0x0C0000), 0x5BCD);
        // dot-mapped: 080000 = the sub bank's word 0 high byte as two pixels, 080002 the low byte
        assert_eq!(SubBus(&mut m.st).read_word(0x080000), 0x050B);
        assert_eq!(SubBus(&mut m.st).read_word(0x080002), 0x0C0D);
        // over write (priority 2): pixel 0 doesn't write
        SubBus(&mut m.st).write_word(0xFF8002, 0x0004 | 2 << 3);
        SubBus(&mut m.st).write_word(0x080000, 0x0007);
        assert_eq!(SubBus(&mut m.st).read_word(0x0C0000), 0x57CD);
        // cell-mapped window (V32 at 220000): 220004 is the 1M bank's word 100
        m.ext_write(0x200100, true, true, 0x7777);
        assert_eq!(m.ext_read(0x220004, false), Some(0x7777));
        m.ext_write(0x200004, true, true, 0x8888);
        assert_eq!(m.ext_read(0x220400, false), Some(0x8888), "the next tile column");
        // swap banks: the Genesis now sees the sub's
        SubBus(&mut m.st).write_word(0xFF8002, 0x0005);
        assert_eq!(m.ext_read(0x200000, false), Some(0x57CD));
    }

    #[test]
    fn backup_ram_comes_formatted() {
        let m = Mcd::new(vec![0; BIOS_SIZE], None);
        let f = &m.st.bram[0x1FC0..];
        assert_eq!(&f[..11], b"___________");
        assert_eq!(&f[0x0B..0x18], &[0, 0, 0, 0, 0x40, 0, 125, 0, 125, 0, 125, 0, 125], "125 free blocks of 128");
        assert_eq!(&f[0x18..0x20], &[0; 8], "no files");
        assert_eq!(&f[0x20..0x30], b"SEGA_CD_ROM\0\x01\0\0\0");
        assert_eq!(&f[0x30..0x40], b"RAM_CARTRIDGE___");
        assert!(m.st.bram[..0x1FC0].iter().all(|&b| b == 0));
        assert!(bram_formatted(&m.st.bram));
        assert!(!bram_formatted(&[0u8; 0x2000]));
    }

    #[test]
    fn prg_ram_window_needs_busreq_and_banks() {
        let mut m = Mcd::new(vec![0; BIOS_SIZE], None);
        m.ext_write(0x020000, true, true, 0x1111);
        assert_eq!(m.ext_read(0x020000, false), None, "no BUSREQ: no access");
        m.ext_io_write(0xA12000, false, true, 0x0002);
        m.ext_io_write(0xA12002, false, true, 0x0040 | 0x0002); // bank 1
        m.ext_write(0x020000, true, true, 0x2222);
        assert_eq!(m.st.pram[0x20000..0x20002], [0x22, 0x22], "bank 1 = PRG-RAM 20000");
        // write protect for the sub CPU below protect * 512
        m.ext_io_write(0xA12002, true, false, 0x0200);
        use crate::common::cpu::m68000::M68kBus;
        SubBus(&mut m.st).write_word(0x0001FE, 0x3333);
        SubBus(&mut m.st).write_word(0x000400, 0x4444);
        assert_eq!(SubBus(&mut m.st).read_word(0x0001FE), 0x0000, "protected");
        assert_eq!(SubBus(&mut m.st).read_word(0x000400), 0x4444);
    }

    #[test]
    fn timer_interrupt() {
        use crate::common::cpu::m68000::M68kBus;
        let mut m = Mcd::new(vec![0; BIOS_SIZE], None);
        SubBus(&mut m.st).write_word(0xFF8032, 0x0008); // level 3 on
        SubBus(&mut m.st).write_word(0xFF8030, 0x0002); // every 3 ticks
        m.st.io.halt = true;                             // (no program: just the clock)
        m.run(384 * 4 * 5, SUB_HZ);                      // 4 ticks of 384 sub clocks
        assert!(m.st.irq[3].pending);
    }
}

#[cfg(test)]
mod cd_tests {
    use super::mcd::*;
    use super::BIOS_SIZE;
    use crate::common::cpu::m68000::M68kBus;

    /// sector 1's user data
    fn data1(i: usize) -> u8 { (i as u8) ^ 0x5A }

    /// (each test its own file: tests run in parallel)
    fn disc(name: &str) -> crate::common::disc::Disc {
        let d = std::env::temp_dir().join(format!("mimesys_cdd_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let mut img = vec![0u8; 2048 * 3];
        img[..16].copy_from_slice(b"SEGADISCSYSTEM  ");
        for i in 0..2048 { img[2048 + i] = data1(i); }
        let f = d.join(format!("{name}.iso"));
        std::fs::write(&f, &img).unwrap();
        crate::common::disc::Disc::open(&f).unwrap()
    }

    const TICK: i64 = 384 * 434; // sub clocks per 75 Hz drive tick

    struct T { m: Mcd }
    impl T {
        fn new(name: &str) -> Self {
            let mut m = Mcd::new(vec![0; BIOS_SIZE], Some(disc(name)));
            m.st.io.halt = true; // no sub program: just the hardware
            Self { m }
        }
        fn w(&mut self, a: u32, v: u16) { SubBus(&mut self.m.st).write_word(a, v) }
        fn r(&mut self, a: u32) -> u16 { SubBus(&mut self.m.st).read_word(a) }
        fn ticks(&mut self, n: i64) { self.m.run(n * TICK, SUB_HZ) }
        /// send a drive command (nine nibbles + checksum)
        fn cmd(&mut self, c: [u8; 9]) {
            let sum = !c.iter().fold(0u8, |s, &n| s.wrapping_add(n)) & 15;
            let n: Vec<u8> = c.iter().copied().chain([sum]).collect();
            for i in 0..5 { self.w(0xFF8042 + i * 2, (n[i as usize * 2] as u16) << 8 | n[i as usize * 2 + 1] as u16); }
        }
        fn status(&mut self) -> Vec<u8> {
            (0..5).flat_map(|i| { let w = self.r(0xFF8038 + i * 2); [(w >> 8) as u8, w as u8] }).collect()
        }
        // the register select is a byte write to FF8005 (FF8004 holds the DMA destination)
        fn cdc_w(&mut self, reg: u16, v: u8) { SubBus(&mut self.m.st).write_byte(0xFF8005, reg as u8); self.w(0xFF8006, v as u16); }
        fn cdc_r(&mut self, reg: u16) -> u8 { SubBus(&mut self.m.st).write_byte(0xFF8005, reg as u8); self.r(0xFF8006) as u8 }
    }

    #[test]
    fn drive_reads_toc_and_answers() {
        let mut t = T::new("toc");
        t.w(0xFF8032, 0x10);         // level 4 on
        t.w(0xFF8036, 0x04);         // host clock on: the drive starts answering
        assert!(t.m.st.irq[4].pending);
        t.m.st.irq[4].pending = false;
        t.ticks(160);                 // 2 s of lead-in while it reads the TOC
        t.cmd([2, 0, 0, 4, 0, 0, 0, 0, 0]); // request: first / last track
        let s = t.status();
        assert_eq!(s[0], super::cdd::status::PAUSED, "TOC read, paused");
        assert_eq!(&s[1..6], &[4, 0, 1, 0, 1], "tracks 01-01");
        let sum = !s[..9].iter().fold(0u8, |a, &n| a.wrapping_add(n)) & 15;
        assert_eq!(s[9], sum, "status checksum");
        t.ticks(1);
        assert!(t.m.st.irq[4].pending, "the answer comes with an interrupt");
        t.cmd([2, 0, 0, 3, 0, 0, 0, 0, 0]); // disc length: 3 sectors + 2 s = 00:02:03
        assert_eq!(&t.status()[1..8], &[3, 0, 0, 0, 2, 0, 3]);
        // a bad checksum
        t.w(0xFF804A, 0x0000);
        assert_eq!(t.status()[0], super::cdd::status::CHECKSUM_ERROR);
    }

    /// Seek + play: the decoder stores sectors; the sub CPU reads one through the host data
    /// register, then has the DMA copy it into PRG-RAM.
    #[test]
    fn read_a_sector_both_ways() {
        let mut t = T::new("read");
        t.w(0xFF8032, 0x20);                 // level 5 on
        t.w(0xFF8036, 0x04);
        t.ticks(160);
        t.cdc_w(0x1, 0x22);                  // IFCTRL: data out on, decoder interrupt on
        t.cdc_w(0xA, 0x84);                  // CTRL0: decoder on, store sectors (WRRQ)
        t.cmd([3, 0, 0, 0, 0, 2, 0, 1, 0]);  // seek + play 00:02:01 (LBA 1)
        let mut ptr = None;
        for _ in 0..60 {
            t.ticks(1);
            let head: Vec<u8> = (4..8).map(|r| t.cdc_r(r)).collect();
            if head == [0x00, 0x02, 0x01, 0x01] { ptr = Some(t.m.st.cdc.transfer.pointer); break; }
        }
        let ptr = ptr.expect("sector 1 was decoded");
        assert!(t.m.st.irq[5].pending, "decoder interrupt");
        let src = ptr.wrapping_add(4);       // past the 4 header bytes
        // host read by the sub CPU (destination 3), 16 bytes
        t.w(0xFF8004, 0x0300);
        t.cdc_w(0x2, 15); t.cdc_w(0x3, 0);
        t.cdc_w(0x4, src as u8); t.cdc_w(0x5, (src >> 8) as u8);
        t.cdc_w(0x6, 0);                     // DTRG: go
        assert_eq!(t.r(0xFF8004) & 0x4000, 0x4000, "data ready");
        let words: Vec<u16> = (0..8).map(|_| t.r(0xFF8008)).collect();
        let want: Vec<u16> = (0..8).map(|i| (data1(i * 2) as u16) << 8 | data1(i * 2 + 1) as u16).collect();
        assert_eq!(words, want);
        assert_eq!(t.r(0xFF8004) & 0x8000, 0x8000, "transfer done");
        // DMA into PRG-RAM 1000 (destination 5)
        t.w(0xFF8004, 0x0500);
        t.w(0xFF800A, 0x1000 >> 3);
        t.cdc_w(0x2, 31); t.cdc_w(0x3, 0);
        t.cdc_w(0x4, src as u8); t.cdc_w(0x5, (src >> 8) as u8);
        t.cdc_w(0x6, 0);
        t.m.run(1000, SUB_HZ);
        let got: Vec<u8> = t.m.st.pram[0x1000..0x1020].to_vec();
        let want: Vec<u8> = (0..32).map(data1).collect();
        assert_eq!(got, want);
        assert!(t.m.st.cdc.transfer.completed);
    }
}

#[cfg(test)]
mod gpu_tests {
    use super::mcd::*;
    use super::BIOS_SIZE;
    use crate::common::cpu::m68000::M68kBus;

    fn set_nib(w: &mut [u8], a: usize, v: u8) {
        let b = &mut w[a >> 1];
        *b = if a & 1 == 0 { (*b & 0x0F) | v << 4 } else { (*b & 0xF0) | v };
    }
    fn nib(w: &[u8], a: usize) -> u8 { if a & 1 == 0 { w[a >> 1] >> 4 } else { w[a >> 1] & 15 } }

    /// Stamp 1's top row is 1, 2, ... 15, 1 (x = 0-15). Draw 8 dots of one line
    /// through a 1x1 map whose only entry is stamp 1 (flipped when `hflip`).
    fn draw(hflip: bool) -> (Vec<u8>, bool) {
        let mut m = Mcd::new(vec![0; BIOS_SIZE], None);
        m.st.io.halt = true;
        let w = &mut m.st.wram;
        for x in 0..16usize {
            // a stamp is two 8-dot-wide columns of 16 rows, 4 bits a dot
            let at = 256 + (x & 8) * 16 + (x & 7);
            set_nib(w, at, (x % 15) as u8 + 1);
        }
        let entry: u16 = 1 | if hflip { 0x8000 } else { 0 };
        w[0x2000..0x2002].copy_from_slice(&entry.to_be_bytes());          // map at word 1000
        w[0x4000..0x4008].copy_from_slice(&[0, 0, 0, 0, 0x08, 0x00, 0, 0]); // x 0, y 0, step x 1 dot
        let mut sub = SubBus(&mut m.st);
        sub.write_word(0xFF8032, 0x0002);  // level 1 on
        sub.write_word(0xFF8058, 0x0000);  // 16x16 stamps, 1x1 map, no repeat
        sub.write_word(0xFF805A, 0x0800);  // map base 2000
        sub.write_word(0xFF805C, 0x0000);  // image 1 cell tall
        sub.write_word(0xFF805E, 0x2000);  // image base 8000
        sub.write_word(0xFF8060, 0x0000);
        sub.write_word(0xFF8062, 8);        // 8 dots wide
        sub.write_word(0xFF8064, 1);        // 1 line
        sub.write_word(0xFF8066, 0x1000);  // vectors at 4000: go
        assert_eq!(sub.read_word(0xFF8058) & 0x8000, 0x8000, "busy");
        m.run(100, SUB_HZ);
        let dots = (0..8).map(|i| nib(&m.st.wram, 0x10000 + i)).collect();
        (dots, m.st.irq[1].pending)
    }

    #[test]
    fn draws_a_line_of_a_stamp() {
        let (dots, irq) = draw(false);
        assert_eq!(dots, vec![1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(irq, "done: level-1 interrupt");
        let (dots, _) = draw(true);
        assert_eq!(dots, vec![1, 15, 14, 13, 12, 11, 10, 9], "flipped");
    }
}

#[cfg(test)]
mod sound_tests {
    use super::mcd::*;
    use super::BIOS_SIZE;
    use crate::common::cpu::m68000::M68kBus;

    /// Channel 0 plays +64, -16 (sign + magnitude), then a loop marker back to the start;
    /// full volume, pan left 15 / right 3.
    #[test]
    fn pcm_plays_and_loops() {
        let mut m = Mcd::new(vec![0; BIOS_SIZE], None);
        let mut sub = SubBus(&mut m.st);
        let reg = |sub: &mut SubBus, r: u32, v: u8| sub.write_byte(0xFF0001 + r * 2, v);
        reg(&mut sub, 7, 0x40);                       // select channel 0
        reg(&mut sub, 0, 0xFF);                       // envelope
        reg(&mut sub, 1, 0x3F);                       // pan: left 15, right 3
        reg(&mut sub, 2, 0x00); reg(&mut sub, 3, 0x08); // step 1.0
        reg(&mut sub, 4, 0x00); reg(&mut sub, 5, 0x00); // loop to 0
        reg(&mut sub, 6, 0x00);                       // start 0
        reg(&mut sub, 7, 0x00);                       // wave RAM bank 0 (chip off)
        for (i, b) in [0xC0u8, 0x10, 0xFF].iter().enumerate() { sub.write_byte(0xFF2001 + i as u32 * 2, *b); }
        assert_eq!(sub.read_byte(0xFF2003), 0x10, "wave RAM through the window");
        reg(&mut sub, 7, 0x80);                       // chip on
        reg(&mut sub, 8, 0xFE);                       // channel 0 on
        let out: Vec<(i32, i32)> = (0..4).map(|_| m.st.pcm.clock()).collect();
        // left: 64 * 255 * 15 >> 5 = 7650; the 10-bit DAC: >> 2, low 4 bits off = 1904
        // right: 64 * 255 * 3 >> 5 = 1530 -> 368; -16: -1913 -> -480, -383 -> -96
        // at FF it jumps to the loop point and plays that byte without moving on (ares)
        assert_eq!(out, vec![(1904, 368), (-480, -96), (1904, 368), (1904, 368)], "loops at FF");
        assert_eq!(SubBus(&mut m.st).read_byte(0xFF0021), 0x01, "channel 0 position, low byte (reg 10)");
        assert_eq!(SubBus(&mut m.st).read_byte(0xFF0023), 0x00, "high byte (reg 11)");
    }

    /// A disc with an audio track of constant samples: seek + play into it, and the CD
    /// audio comes out at half scale (the DAC's >> 1), mixed into the 44.1 kHz stream.
    #[test]
    fn cd_audio_plays() {
        let d = std::env::temp_dir().join(format!("mimesys_cdda_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let mut data = vec![0u8; 2352 * 3];
        data[16..32].copy_from_slice(b"SEGADISCSYSTEM  ");
        std::fs::write(d.join("d.bin"), &data).unwrap();
        let mut audio = Vec::new();
        for _ in 0..2352 * 100 / 4 { audio.extend(0x2000i16.to_le_bytes()); audio.extend((-0x2000i16).to_le_bytes()); }
        std::fs::write(d.join("a.bin"), &audio).unwrap();
        std::fs::write(d.join("g.cue"), "FILE d.bin BINARY\n TRACK 01 MODE1/2352\n  INDEX 01 00:00:00\nFILE a.bin BINARY\n TRACK 02 AUDIO\n  INDEX 01 00:00:00\n").unwrap();
        let disc = crate::common::disc::Disc::open(&d.join("g.cue")).unwrap();
        let mut m = Mcd::new(vec![0; BIOS_SIZE], Some(disc));
        m.st.io.halt = true;
        let tick = 384 * 434;
        SubBus(&mut m.st).write_word(0xFF8036, 4);
        m.run(160 * tick, SUB_HZ);                      // TOC
        // seek + play 00:02:06 = track 2 + 3 sectors (LBA 6)
        let c = [3u8, 0, 0, 0, 0, 2, 0, 6, 0];
        let sum = !c.iter().fold(0u8, |s, &n| s.wrapping_add(n)) & 15;
        let n: Vec<u8> = c.iter().copied().chain([sum]).collect();
        for i in 0..5 { SubBus(&mut m.st).write_word(0xFF8042 + i * 2, (n[i as usize * 2] as u16) << 8 | n[i as usize * 2 + 1] as u16); }
        m.st.audio.clear();
        m.run(20 * tick, SUB_HZ);
        assert_eq!(m.st.cdd.state, super::cdd::status::PLAYING);
        m.st.audio.clear();
        m.run(5 * tick, SUB_HZ);
        let last = &m.st.audio[m.st.audio.len() - 2..];
        assert_eq!(last, &[0x1000 as f32 / 32768.0, -0x1000 as f32 / 32768.0]);
        assert!((m.st.audio.len() as i64 - 5 * tick * 44100 / SUB_HZ * 2).abs() <= 2, "44.1 kHz stereo");
        // the volume register turns it down (it slides there, one step a sample)
        SubBus(&mut m.st).write_word(0xFF8034, 0x2000);
        m.run(20 * tick, SUB_HZ); // 8192 samples to slide from 4000 to 2000
        let last = &m.st.audio[m.st.audio.len() - 2..];
        assert_eq!(last, &[0x800 as f32 / 32768.0, -0x800 as f32 / 32768.0], "half volume");
    }
}
