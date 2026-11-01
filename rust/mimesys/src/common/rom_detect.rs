// Which system is a ROM for? Looks at the data first, the file name last.
//
// 1. Headers that settle it (score 100):
//      NES "NES\x1A"            7800 "ATARI7800" at byte 1      Game Boy logo at 0x104
//      SMS / Game Gear "TMR SEGA" at 0x1FF0 / 0x3FF0 / 0x7FF0 (region nibble picks which)
//      ColecoVision 0xAA55 / 0x55AA at the start (95)
// 2. Fingerprints, for headerless dumps (.bin): what the code does and where it starts
//      2600  size 2K-512K, reset vector at the end of a 4K bank pointing into 0x1000-0x1FFF,
//            lots of STA WSYNC (85 02)
//      7800  size 8K-512K, reset vector >= 0x4000, writes to MARIA registers (CTRL 3C,
//            DPPH 2C, DPPL 30, WSYNC 24), the cart signature nibble at 0xFFF9
//      PC Engine  8K multiple (+512 copier header), reset vector at the end of bank 0 >= 0xE000
// 3. Hints: the file extension (+30) and folder names like ".../Atari 2600/..." (+20).
//
// Returns every plausible system, best first. The frontend loads the best one straight away
// when it's clear (score >= 80, or 30+ ahead of the next) and asks otherwise.
// System ids are the ones the system factory takes: nes gb gbc gba sms gg md coleco pce a2600 a7800 a5200 lynx snes.
//      Genesis "SEGA" at 0x100 (or an interleaved .smd dump)

#[derive(Debug, Clone, PartialEq)]
pub struct Guess {
    pub system: &'static str,
    pub score: u32,
    pub why: String,
}

const GB_LOGO: [u8; 16] = [0xCE, 0xED, 0x66, 0x66, 0xCC, 0x0D, 0x00, 0x0B, 0x03, 0x73, 0x00, 0x83, 0x00, 0x0C, 0x00, 0x0D];

pub const SYSTEMS: [&str; 15] = ["nes", "gb", "gbc", "gba", "nds", "sms", "gg", "md", "coleco", "pce", "a2600", "a7800", "a5200", "lynx", "snes"];

fn count(data: &[u8], pattern: &[u8]) -> usize {
    data.windows(pattern.len()).filter(|w| *w == pattern).count()
}

fn word(data: &[u8], at: usize) -> u16 {
    data[at] as u16 | (data[at + 1] as u16) << 8
}

struct Scores(Vec<Guess>);

impl Scores {
    fn add(&mut self, system: &'static str, points: u32, why: &str) {
        match self.0.iter_mut().find(|g| g.system == system) {
            Some(g) => {
                g.score += points;
                g.why = format!("{}, {why}", g.why);
            }
            None => self.0.push(Guess { system, score: points, why: why.to_string() }),
        }
    }
}

pub fn detect(data: &[u8], path: &str) -> Vec<Guess> {
    let mut s = Scores(Vec::new());
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();

    // ---- 1. headers ----
    if data.starts_with(b"NES\x1A") {
        return vec![Guess { system: "nes", score: 100, why: "iNES header".into() }];
    }
    if data.len() > 128 && &data[1..10] == b"ATARI7800" {
        return vec![Guess { system: "a7800", score: 100, why: "A78 header".into() }];
    }
    // Atari800 .car header with a 5200 cartridge type
    if data.len() > 16 && &data[0..4] == b"CART" && matches!(u32::from_be_bytes([data[4], data[5], data[6], data[7]]), 4 | 6 | 16 | 19 | 20) {
        return vec![Guess { system: "a5200", score: 100, why: "CART header, 5200 type".into() }];
    }
    // Lynx: "LYNX" .lnx header, or a BLL homebrew program (80 08 ... "BS93")
    if data.len() >= 64 && &data[0..4] == b"LYNX" {
        return vec![Guess { system: "lynx", score: 100, why: "LYNX header".into() }];
    }
    if data.len() >= 10 && data[0] == 0x80 && data[1] == 0x08 && &data[6..10] == b"BS93" {
        return vec![Guess { system: "lynx", score: 100, why: "Lynx homebrew (BS93) header".into() }];
    }
    // DS: the logo's CRC16 (CF56) at 0x15C, and the header's own CRC
    if data.len() >= 0x200 && data[0x15C] == 0x56 && data[0x15D] == 0xCF
        && crate::nds::nds_spi::crc16(&data[0..0x15E], 0xFFFF) == u16::from_le_bytes([data[0x15E], data[0x15F]])
    {
        return vec![Guess { system: "nds", score: 100, why: "Nintendo DS header".into() }];
    }
    // GBA: fixed byte 0x96 at 0xB2 and the start of the Nintendo logo at 0x04
    if data.len() >= 0xC0 && data[0xB2] == 0x96 && data[4..8] == [0x24, 0xFF, 0xAE, 0x51] {
        return vec![Guess { system: "gba", score: 100, why: "Game Boy Advance header".into() }];
    }
    if data.len() > 0x150 && data[0x104..0x114] == GB_LOGO {
        let cgb = data[0x143] & 0x80 != 0;
        return vec![Guess { system: if cgb { "gbc" } else { "gb" }, score: 100,
                            why: if cgb { "Game Boy Color header" } else { "Game Boy header" }.into() }];
    }
    for at in [0x7FF0usize, 0x3FF0, 0x1FF0] {
        if data.len() >= at + 16 && &data[at..at + 8] == b"TMR SEGA" {
            let region = data[at + 15] >> 4;
            let gg = matches!(region, 5..=7) || ext == "gg";
            return vec![Guess { system: if gg { "gg" } else { "sms" }, score: 100,
                                why: format!("Sega header (region {region})") }];
        }
    }
    // Genesis: "SEGA" at 0x100 (also inside an interleaved .smd dump)
    if data.len() >= 0x200 && (data[0x100..0x104] == *b"SEGA" || data[0x101..0x105] == *b"SEGA")
        || crate::genesis::md_cart::is_smd(data)
    {
        return vec![Guess { system: "md", score: 100, why: "Sega Genesis header".into() }];
    }
    if data.len() >= 0x2000 && matches!((data[0], data[1]), (0xAA, 0x55) | (0x55, 0xAA)) {
        s.add("coleco", 95, "ColecoVision cart header");
    }

    // ---- 2. fingerprints ----
    // SNES: an internal header at 7FC0 / FFC0 / 40FFC0 (after any 512-byte copier header)
    // with checksum + complement = FFFF, a printable title and a matching map mode
    {
        let body = if data.len() % 1024 == 512 { &data[512..] } else { data };
        for (at, mode) in [(0x7FC0usize, 0u8), (0xFFC0, 1), (0x40FFC0, 5)] {
            if body.len() < at + 0x40 { continue; }
            let h = &body[at..at + 0x40];
            let sum_ok = (word(h, 0x1C) ^ word(h, 0x1E)) == 0xFFFF;
            let title_ok = h[..21].iter().all(|&c| (0x20..0x7F).contains(&c));
            let mode_ok = h[0x15] & 0xE0 == 0x20 && (h[0x15] & 0x0F == mode || mode == 0 && matches!(h[0x15] & 0x0F, 2 | 3));
            let reset_ok = word(h, 0x3C) >= 0x8000;
            let p = sum_ok as u32 * 35 + title_ok as u32 * 15 + mode_ok as u32 * 25 + reset_ok as u32 * 10;
            if p >= 60 { s.add("snes", p, &format!("SNES header at {at:X}")); break; }
        }
    }
    let len = data.len();
    let kb = len / 1024;
    if len >= 2048 && len <= 512 * 1024 && len % 2048 == 0 {
        // the 6507 sees 0x1000-0x1FFF: every 2600 vector points there (often as 0xF000 mirrors)
        let reset = word(data, len - 4);
        let wsync = count(data, &[0x85, 0x02]);
        if reset & 0x1000 != 0 {
            let mut p = 35;
            let mut why = format!("{kb}K, reset vector {reset:04X}");
            if wsync >= 4 {
                p += 30;
                why += &format!(", {wsync} x STA WSYNC");
            }
            s.add("a2600", p, &why);
        }
    }
    if len >= 8 * 1024 && len <= 512 * 1024 && len % 1024 == 0 {
        let reset = word(data, len - 4);
        let maria = count(data, &[0x85, 0x3C]) + count(data, &[0x8D, 0x3C, 0x00])
            + count(data, &[0x85, 0x2C]) + count(data, &[0x85, 0x30]) + count(data, &[0x85, 0x24]);
        let signature = data[len - 7] & 0x0F == 0x07;     // 0xFFF9: "7800 cart"
        if reset >= 0x4000 && (maria >= 2 || signature) {
            let mut p = 40;
            let mut why = format!("{kb}K, reset vector {reset:04X}");
            if maria >= 2 { p += 30; why += &format!(", {maria} MARIA register writes"); }
            if signature { p += 15; why += ", 7800 cart signature"; }
            s.add("a7800", p, &why);
        }
    }
    let body = if len % 8192 == 512 { &data[512..] } else { data };
    if body.len() >= 8192 && body.len() % 8192 == 0 && body.len() <= 0x280000 {
        let v = body[0x1FFF];
        if v >= 0xE0 || v.reverse_bits() >= 0xE0 {
            s.add("pce", 20, "8K banks, reset vector in bank 0");
        }
    }

    // 5200: 4-32 KB, the start address (last two bytes) inside the cartridge, and stores to
    // the 5200's GTIA (C0xx), ANTIC (D4xx) or POKEY (E8xx)
    if matches!(data.len(), 0x1000 | 0x2000 | 0x4000 | 0x8000) {
        let start = word(data, data.len() - 2);
        let hw = count(data, &[0x8D, 0x00, 0xD4]) + count(data, &[0x8D, 0x0E, 0xD4]) + count(data, &[0x8D, 0x0A, 0xD4])
            + data.windows(3).filter(|w| w[0] == 0x8D && (w[2] == 0xC0 || w[2] == 0xE8)).count();
        if (0x4000..0xC000).contains(&start) && hw >= 3 {
            s.add("a5200", 40 + (hw as u32).min(20), &format!("start {start:04X}, {hw} stores to GTIA/ANTIC/POKEY"));
        }
    }

    // ---- 3. hints ----
    let by_ext: &[&'static str] = match ext.as_str() {
        "nes" => &["nes"], "gb" => &["gb"], "gbc" | "cgb" => &["gbc"], "sms" => &["sms"],
        "gg" => &["gg"], "col" | "cv" => &["coleco"], "pce" => &["pce"], "a26" => &["a2600"],
        "a78" => &["a7800"], "gba" | "agb" => &["gba"],
        "md" | "gen" | "smd" => &["md"], "nds" | "srl" => &["nds"], "a52" => &["a5200"], "lnx" | "lyx" => &["lynx"], "sfc" | "smc" | "swc" | "fig" => &["snes"], _ => &[],
    };
    for &sys in by_ext {
        s.add(sys, 30, &format!(".{ext} file"));
    }
    let folders = path.to_ascii_lowercase().replace('\\', "/");
    let mut parts: Vec<&str> = folders.split('/').collect();
    parts.pop();                                          // the file name itself
    const FOLDER_HINTS: [(&str, &str); 22] = [
        ("snes", "snes"), ("super nintendo", "snes"), ("super famicom", "snes"),
        ("genesis", "md"), ("mega drive", "md"), ("megadrive", "md"), ("5200", "a5200"),
        ("2600", "a2600"), ("vcs", "a2600"), ("7800", "a7800"), ("nes", "nes"), ("famicom", "nes"),
        ("gameboy color", "gbc"), ("gbc", "gbc"), ("gameboy", "gb"), ("game boy", "gb"),
        ("master system", "sms"), ("game gear", "gg"), ("coleco", "coleco"),
        ("turbografx", "pce"), ("pc engine", "pce"), ("lynx", "lynx"),
    ];
    for part in parts {
        if let Some(&(_, sys)) = FOLDER_HINTS.iter().find(|(k, _)| part.contains(k)) {
            s.add(sys, 20, &format!("folder \"{part}\""));
        }
    }

    let mut out = s.0;
    out.sort_by(|a, b| b.score.cmp(&a.score));
    out
}

/// True when the best guess is clear enough to load without asking.
pub fn is_clear(guesses: &[Guess]) -> bool {
    match guesses {
        [] => false,
        [only] => only.score >= 30,
        [best, next, ..] => best.score >= 80 || best.score >= next.score + 30,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn top(g: &[Guess]) -> &'static str { g.first().map(|g| g.system).unwrap_or("none") }

    #[test]
    fn headers_settle_it() {
        let mut nes = b"NES\x1A".to_vec();
        nes.resize(0x6010, 0);
        assert_eq!(top(&detect(&nes, "x.bin")), "nes");
        let mut gb = vec![0u8; 0x8000];
        gb[0x104..0x114].copy_from_slice(&GB_LOGO);
        assert_eq!(top(&detect(&gb, "x.bin")), "gb");
        gb[0x143] = 0xC0;
        assert_eq!(top(&detect(&gb, "x.rom")), "gbc");
        let mut sms = vec![0u8; 0x8000];
        sms[0x7FF0..0x7FF8].copy_from_slice(b"TMR SEGA");
        sms[0x7FFF] = 0x4C;
        assert_eq!(top(&detect(&sms, "x.bin")), "sms");
        sms[0x7FFF] = 0x6C;
        assert_eq!(top(&detect(&sms, "x.bin")), "gg");
        let mut cv = vec![0u8; 0x4000];
        cv[0] = 0x55; cv[1] = 0xAA;
        assert!(is_clear(&detect(&cv, "x.rom")));
        assert_eq!(top(&detect(&cv, "x.rom")), "coleco");
    }

    /// A 4K 2600 game: reset vector into 0x1000-0x1FFF and STA WSYNC everywhere.
    #[test]
    fn atari_2600_bin() {
        let mut rom = vec![0xEAu8; 4096];
        for i in 0..10 { rom[100 + 3 * i] = 0x85; rom[101 + 3 * i] = 0x02; }
        rom[4092] = 0x00; rom[4093] = 0xF0;
        let g = detect(&rom, "/roms/games/Combat.bin");
        assert_eq!(top(&g), "a2600");
        assert!(is_clear(&g), "{g:?}");
        // the folder name helps when the data says less
        let mut bare = vec![0u8; 4096];
        bare[4093] = 0xF0;
        let g = detect(&bare, "/roms/Atari 2600/x.bin");
        assert_eq!((top(&g), g[0].score), ("a2600", 55));
    }

    /// The real 7800 test ROMs built with 7800basic, minus their headers, and the real
    /// Haunted House (2600) assembled from its disassembly (when present on this machine).
    #[test]
    fn real_headerless_roms() {
        let base = concat!(env!("CARGO_MANIFEST_DIR"), "/../a78/7800basic/samples/");
        for name in ["helloworld", "smasteroids", "onehundredandone"] {
            let Ok(file) = std::fs::read(format!("{base}{name}/{name}.bas.a78")) else { continue };
            assert_eq!(top(&detect(&file, "x.a78")), "a7800");
            let g = detect(&file[128..], "x.bin");
            assert_eq!(top(&g), "a7800", "{name}: {g:?}");
            assert!(is_clear(&g), "{name}: {g:?}");
        }
        if let Ok(hh) = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../hh/haunted.bin")) {
            let g = detect(&hh, "Haunted House.bin");
            assert_eq!(top(&g), "a2600", "{g:?}");
            assert!(is_clear(&g), "{g:?}");
        }
    }

    #[test]
    fn unknown_or_ambiguous() {
        assert!(detect(&[0u8; 100], "x.bin").is_empty());
        // .rom with nothing to go on: no guesses, the frontend offers every system
        assert!(!is_clear(&detect(&vec![0u8; 0x8000], "x.rom")));
    }
}
