// Cheat codes, shared by every system.
//
// Two kinds of effect:
//   RomPatch  when the CPU reads `addr` from the cartridge, it gets `value` instead
//             (only if the real byte equals `compare`, when there is one: that keeps a
//             patch from hitting the wrong bank in bank-switched games)
//   RamWrite  `value` is written to `addr` once per frame (GameShark / Action Replay style)
//
// Accepted formats (several codes can be pasted together, separated by spaces, '+' or ',')
//   NES   Game Genie   6 or 8 letters                          SXIOPO, AEUZZYEP
//         raw          AAAA:VV  (RAM if < $8000, else ROM)     0075:09
//                      AAAA?CC:VV (ROM patch with compare)     91D9?CE:AD
//   GB    Game Genie   ABC-DEF or ABC-DEF-GHI (dashes optional)
//         GameShark    01VVAAAA (address little-endian)        0163B0D1 = $D1B0 <- 63
//                      9nVVAAAA writes WRAM bank n (CGB)
//         raw          same as NES: AAAA:VV, AAAA?CC:VV (ROM if < $8000)
//   SNES  Game Genie   XXXX-XXXX (the dash tells it from PAR)
//         Pro Action Replay  AAAAAAVV (8 hex digits)       7E0DBE09 = $7E0DBE <- 09
//         raw          AAAAAA:VV / AAAAAA?CC:VV
//         (RAM = banks 7E-7F, 70-7D:0000-7FFF and 00-3F/80-BF:0000-7FFF; else ROM)
//   Genesis Game Genie XXXX-XXXX (dash optional)               SCRA-MBLE = 059C44:B078
//         raw / Pro Action Replay  AAAAAA:VVVV (word) or AAAAAA:VV (byte),
//                      AAAAAA?CC:VV (byte with compare)     FF0A12:0009
//         (RAM = E00000-FFFFFF; else ROM)
//
// The bus calls `patch_read` on cartridge reads (cheap when there are no patches) and the
// system calls `ram_writes()` after each frame.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CheatSystem {
    Nes,
    GameBoy,
    Snes,
    Genesis,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RomPatch {
    pub addr: u32,
    pub value: u8,
    pub compare: Option<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RamWrite {
    pub addr: u32,
    pub value: u8,
    pub bank: Option<u8>,     // GB: WRAM bank for $D000-$DFFF (CGB)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Effect {
    Rom(RomPatch),
    Ram(RamWrite),
}

#[derive(Clone, Debug)]
pub struct Cheats {
    pub system: CheatSystem,
    rom: Vec<RomPatch>,
    ram: Vec<RamWrite>,
}

impl Cheats {
    pub fn new(system: CheatSystem) -> Self {
        Self { system, rom: Vec::new(), ram: Vec::new() }
    }

    /// Replace the active cheats. Returns one result per entry ("" = ok, else the error),
    /// bad entries are skipped and the rest still apply.
    pub fn set(&mut self, codes: &[String]) -> Vec<String> {
        self.rom.clear();
        self.ram.clear();
        let mut errors = Vec::with_capacity(codes.len());
        for code in codes {
            match parse(self.system, code) {
                Ok(effects) => {
                    for e in effects {
                        match e {
                            Effect::Rom(p) => self.rom.push(p),
                            Effect::Ram(w) => self.ram.push(w),
                        }
                    }
                    errors.push(String::new());
                }
                Err(e) => errors.push(e),
            }
        }
        errors
    }

    pub fn is_empty(&self) -> bool { self.rom.is_empty() && self.ram.is_empty() }

    /// Called on every cartridge read: returns the patched byte, or `value` unchanged.
    #[inline]
    pub fn patch_read(&self, addr: u16, value: u8) -> u8 {
        if self.rom.is_empty() {
            return value;
        }
        self.patched(addr as u32, value).unwrap_or(value)
    }

    /// The patched byte for `addr`, if a patch applies (24-bit addresses).
    #[inline]
    pub fn patched(&self, addr: u32, value: u8) -> Option<u8> {
        if self.rom.is_empty() {
            return None;
        }
        self.rom.iter().find(|p| p.addr == addr && p.compare.map_or(true, |c| c == value)).map(|p| p.value)
    }

    pub fn ram_writes(&self) -> &[RamWrite] { &self.ram }
}

/// Decode one entry (which may hold several codes).
pub fn parse(system: CheatSystem, text: &str) -> Result<Vec<Effect>, String> {
    let mut out = Vec::new();
    for part in text.split(|c: char| c.is_whitespace() || c == '+' || c == ',') {
        if part.is_empty() {
            continue;
        }
        out.extend(parse_one(system, part).map_err(|e| format!("{part}: {e}"))?);
    }
    if out.is_empty() {
        return Err("empty code".into());
    }
    Ok(out)
}

fn parse_one(system: CheatSystem, code: &str) -> Result<Vec<Effect>, String> {
    let code = code.trim().to_ascii_uppercase();
    if code.contains(':') {
        return match system {
            CheatSystem::Snes | CheatSystem::Genesis => parse_raw24(system, &code),
            _ => parse_raw(system, &code).map(|e| vec![e]),
        };
    }
    let one = match system {
        CheatSystem::Snes => {
            if code.len() == 9 && code.as_bytes()[4] == b'-' {
                snes_game_genie(&code)
            } else if code.len() == 8 {
                let addr = hex(&code[0..6])?;
                let value = hex(&code[6..8])? as u8;
                Ok(snes_effect(addr, value, None))
            } else {
                Err("expected a Game Genie (XXXX-XXXX) or Pro Action Replay (AAAAAAVV) code".into())
            }
        }
        CheatSystem::Genesis => return genesis_game_genie(&code),
        CheatSystem::Nes => nes_game_genie(&code),
        CheatSystem::GameBoy => {
            let digits: String = code.chars().filter(|&c| c != '-').collect();
            match (digits.len(), code.contains('-')) {
                (8, false) => gb_gameshark(&digits),
                (6, _) | (9, _) => gb_game_genie(&digits),
                _ => Err("expected a Game Genie (ABC-DEF-GHI) or GameShark (01VVAAAA) code".into()),
            }
        }
    };
    one.map(|e| vec![e])
}

/// SNES: RAM (written every frame) or ROM (patched on read), by address.
fn snes_effect(addr: u32, value: u8, compare: Option<u8>) -> Effect {
    let bank = addr >> 16;
    let off = addr & 0xFFFF;
    let b = bank & 0x7F;
    let ram = bank == 0x7E || bank == 0x7F || (b < 0x40 && off < 0x8000) || ((0x70..0x7E).contains(&b) && off < 0x8000);
    if ram && compare.is_none() {
        Effect::Ram(RamWrite { addr, value, bank: None })
    } else {
        Effect::Rom(RomPatch { addr, value, compare })
    }
}

/// SNES / Genesis raw codes with 24-bit addresses: AAAAAA:VV, AAAAAA?CC:VV, and on the
/// Genesis AAAAAA:VVVV (a word: two bytes, high first).
fn parse_raw24(system: CheatSystem, code: &str) -> Result<Vec<Effect>, String> {
    let (left, v) = code.split_once(':').unwrap();
    let (addr, compare) = match left.split_once('?') {
        Some((a, c)) => (hex(a)?, Some(hex(c)?)),
        None => (hex(left)?, None),
    };
    if addr > 0xFFFFFF || compare.map_or(false, |c| c > 0xFF) {
        return Err("the address is up to 6 hex digits, the compare value 2".into());
    }
    let value = hex(v)?;
    let compare = compare.map(|c| c as u8);
    match system {
        CheatSystem::Snes => {
            if v.len() > 2 { return Err("SNES values are 2 hex digits".into()); }
            Ok(vec![snes_effect(addr, value as u8, compare)])
        }
        _ => {
            let bytes: Vec<(u32, u8)> = match v.len() {
                1 | 2 => vec![(addr, value as u8)],
                3 | 4 if compare.is_none() => {
                    if addr & 1 != 0 { return Err("a word code needs an even address".into()); }
                    vec![(addr, (value >> 8) as u8), (addr + 1, value as u8)]
                }
                _ => return Err("Genesis values are 2 or 4 hex digits (compare: 2)".into()),
            };
            Ok(bytes.into_iter().map(|(a, b)| genesis_effect(a, b, compare)).collect())
        }
    }
}

/// Genesis: work RAM (E00000-FFFFFF, written every frame) or ROM (patched on read).
fn genesis_effect(addr: u32, value: u8, compare: Option<u8>) -> Effect {
    if addr >= 0xE00000 && compare.is_none() {
        Effect::Ram(RamWrite { addr, value, bank: None })
    } else {
        Effect::Rom(RomPatch { addr, value, compare })
    }
}

/// SNES Game Genie, VVAA-AAAA: each character stands for a hex digit (DF4709156BC8A23E =
/// 0-F); VV is the value and the 24 address bits are shuffled: the code's bits
/// ijklqrst opabcduv wxefghmn are the address abcdefgh ijklmnop qrstuvwx.
fn snes_game_genie(code: &str) -> Result<Effect, String> {
    const DIGITS: &str = "DF4709156BC8A23E";
    let mut n: u32 = 0;
    for c in code.chars().filter(|&c| c != '-') {
        n = n << 4 | DIGITS.find(c).ok_or_else(|| format!("'{c}' isn't a SNES Game Genie character"))? as u32;
    }
    let value = (n >> 24) as u8;
    let x = n & 0xFFFFFF;
    // the code's bit for each address letter, counting from the top (bit 23) down
    let code_bit = |name: char| -> u32 {
        let order = "ijklqrstopabcduvwxefghmn";
        let i = order.find(name).unwrap() as u32;
        (x >> (23 - i)) & 1
    };
    let addr = "abcdefghijklmnopqrstuvwx".chars().fold(0u32, |a, name| a << 1 | code_bit(name));
    Ok(snes_effect(addr, value, None))
}

/// Genesis Game Genie, XXXX-XXXX: each character is 5 bits (ABCDEFGHJKLMNPRSTVWXYZ0123456789),
/// 40 bits in all. The code's bits ijklm nopIJ KLMNO PABCD EFGHd efgha bcQRS TUVWX are the
/// address ABCDEFGH IJKLMNOP QRSTUVWX and the word abcdefgh ijklmnop.
fn genesis_game_genie(code: &str) -> Result<Vec<Effect>, String> {
    const CHARS: &str = "ABCDEFGHJKLMNPRSTVWXYZ0123456789";
    let chars: Vec<char> = code.chars().filter(|&c| c != '-').collect();
    if chars.len() != 8 {
        return Err("Genesis Game Genie codes are 8 characters (XXXX-XXXX); raw codes are AAAAAA:VVVV".into());
    }
    let mut n: u64 = 0;
    for c in chars {
        n = n << 5 | CHARS.find(c).ok_or_else(|| format!("'{c}' isn't a Genesis Game Genie character"))? as u64;
    }
    const ORDER: &str = "ijklmnopIJKLMNOPABCDEFGHdefghabcQRSTUVWX";
    let bit = |name: char| -> u32 { ((n >> (39 - ORDER.find(name).unwrap())) & 1) as u32 };
    let addr = "ABCDEFGHIJKLMNOPQRSTUVWX".chars().fold(0u32, |a, c| a << 1 | bit(c));
    let data = "abcdefghijklmnop".chars().fold(0u32, |a, c| a << 1 | bit(c));
    // the 68000 has no A0 line: the word is at the even address
    let addr = addr & !1;
    Ok(vec![genesis_effect(addr, (data >> 8) as u8, None), genesis_effect(addr + 1, data as u8, None)])
}

fn hex(s: &str) -> Result<u32, String> {
    u32::from_str_radix(s, 16).map_err(|_| format!("'{s}' isn't hex"))
}

/// AAAA:VV or AAAA?CC:VV
fn parse_raw(system: CheatSystem, code: &str) -> Result<Effect, String> {
    let (left, value) = code.split_once(':').unwrap();
    let value = hex(value)?;
    let (addr, compare) = match left.split_once('?') {
        Some((a, c)) => (hex(a)?, Some(hex(c)?)),
        None => (hex(left)?, None),
    };
    if addr > 0xFFFF || value > 0xFF || compare.map_or(false, |c| c > 0xFF) {
        return Err("address must be 4 hex digits, values 2".into());
    }
    let (addr, value, compare) = (addr, value as u8, compare.map(|c| c as u8));
    let is_rom = match system {
        CheatSystem::Nes => addr >= 0x8000,
        _ => addr < 0x8000,
    };
    if is_rom || compare.is_some() {
        Ok(Effect::Rom(RomPatch { addr, value, compare }))
    } else {
        Ok(Effect::Ram(RamWrite { addr, value, bank: None }))
    }
}

/// NES Game Genie: each letter is 4 bits, scrambled into address / value / compare.
fn nes_game_genie(code: &str) -> Result<Effect, String> {
    const LETTERS: &str = "APZLGITYEOXUKSVN";
    if code.len() != 6 && code.len() != 8 {
        return Err("NES Game Genie codes are 6 or 8 letters".into());
    }
    let mut n = [0u16; 8];
    for (i, c) in code.chars().enumerate() {
        n[i] = LETTERS.find(c).ok_or_else(|| format!("'{c}' isn't a Game Genie letter"))? as u16;
    }
    let addr = 0x8000
        | ((n[3] & 7) << 12)
        | ((n[5] & 7) << 8) | ((n[4] & 8) << 8)
        | ((n[2] & 7) << 4) | ((n[1] & 8) << 4)
        | (n[4] & 7) | (n[3] & 8);
    let mut value = ((n[1] & 7) << 4) | ((n[0] & 8) << 4) | (n[0] & 7);
    let compare = if code.len() == 8 {
        value |= n[7] & 8;
        Some((((n[7] & 7) << 4) | ((n[6] & 8) << 4) | (n[6] & 7) | (n[5] & 8)) as u8)
    } else {
        value |= n[5] & 8;
        None
    };
    Ok(Effect::Rom(RomPatch { addr: addr as u32, value: value as u8, compare }))
}

/// GB Game Genie, digits ABCDEF[GHI]: value = AB, address = (~F << 12) | CDE,
/// compare = (G << 4 | I) rotated right 2, XOR $BA. (H is a check digit, ignored.)
fn gb_game_genie(d: &str) -> Result<Effect, String> {
    let value = hex(&d[0..2])? as u8;
    let low = hex(&d[2..5])? as u16;
    let high = (!hex(&d[5..6])? & 0xF) as u16;
    let addr = ((high << 12) | low) as u32;
    if addr >= 0x8000 {
        return Err("Game Genie codes patch ROM ($0000-$7FFF); this one points elsewhere".into());
    }
    let compare = if d.len() == 9 {
        let raw = ((hex(&d[6..7])? << 4) | hex(&d[8..9])?) as u8;
        Some(raw.rotate_right(2) ^ 0xBA)
    } else {
        None
    };
    Ok(Effect::Rom(RomPatch { addr, value, compare }))
}

/// GB GameShark: TTVVLLHH. Type 01 (and 0x/8x) = write, 9n = write to WRAM bank n.
fn gb_gameshark(d: &str) -> Result<Effect, String> {
    let kind = hex(&d[0..2])? as u8;
    let value = hex(&d[2..4])? as u8;
    let addr = hex(&d[6..8])? << 8 | hex(&d[4..6])?;
    let bank = match kind & 0xF0 {
        0x00 | 0x80 => None,
        0x90 => Some(kind & 0x07),
        _ => return Err(format!("unsupported GameShark type {kind:02X}")),
    };
    Ok(Effect::Ram(RamWrite { addr, value, bank }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(system: CheatSystem, code: &str) -> Effect {
        let v = parse(system, code).unwrap();
        assert_eq!(v.len(), 1);
        v[0]
    }

    #[test]
    fn nes_game_genie_6_letters() {
        // Super Mario Bros. infinite lives: DEC $075A at $91D9 becomes LDA (opcode $AD)
        assert_eq!(one(CheatSystem::Nes, "SXIOPO"),
            Effect::Rom(RomPatch { addr: 0x91D9, value: 0xAD, compare: None }));
        // lower case and surrounding spaces are fine
        assert_eq!(one(CheatSystem::Nes, " sxiopo "), one(CheatSystem::Nes, "SXIOPO"));
    }

    #[test]
    fn nes_game_genie_8_letters_has_compare() {
        let Effect::Rom(p) = one(CheatSystem::Nes, "SXIOPOAA") else { panic!() };
        assert_eq!(p.addr, 0x91D9);
        // letters 6/7 = A (0): compare = 0 | (n5 & 8) = 8; value takes bit 3 from n7 = 0
        assert_eq!((p.value, p.compare), (0xA5, Some(0x08)));
    }

    #[test]
    fn nes_raw_codes() {
        assert_eq!(one(CheatSystem::Nes, "0075:09"),
            Effect::Ram(RamWrite { addr: 0x0075, value: 0x09, bank: None }));
        assert_eq!(one(CheatSystem::Nes, "91D9?CE:AD"),
            Effect::Rom(RomPatch { addr: 0x91D9, value: 0xAD, compare: Some(0xCE) }));
    }

    #[test]
    fn nes_bad_codes_are_reported() {
        assert!(parse(CheatSystem::Nes, "SXIOP").is_err());
        assert!(parse(CheatSystem::Nes, "SXIOPB").unwrap_err().contains("'B'"));
        assert!(parse(CheatSystem::Nes, "").is_err());
    }

    #[test]
    fn gb_game_genie() {
        // ABC-DEF-GHI = 00A-17B-C49: value $00, address (~B << 12) | A17 = $4A17,
        // compare = $C9 rotated right 2, XOR $BA
        let Effect::Rom(p) = one(CheatSystem::GameBoy, "00A-17B-C49") else { panic!() };
        assert_eq!((p.addr, p.value), (0x4A17, 0x00));
        assert_eq!(p.compare, Some(0xC9u8.rotate_right(2) ^ 0xBA));
        assert!(parse(CheatSystem::GameBoy, "00A-17").is_err());
    }

    #[test]
    fn gb_game_genie_short_form() {
        let Effect::Rom(p) = one(CheatSystem::GameBoy, "3EA-17B") else { panic!() };
        assert_eq!((p.addr, p.value, p.compare), (0x4A17, 0x3E, None));
        assert_eq!(one(CheatSystem::GameBoy, "3EA17B"), one(CheatSystem::GameBoy, "3EA-17B"));
    }

    #[test]
    fn gb_gameshark() {
        // 01 63 B0D1 -> $D1B0 = $63 (address is little-endian)
        assert_eq!(one(CheatSystem::GameBoy, "0163B0D1"),
            Effect::Ram(RamWrite { addr: 0xD1B0, value: 0x63, bank: None }));
        assert_eq!(one(CheatSystem::GameBoy, "910100D0"),
            Effect::Ram(RamWrite { addr: 0xD000, value: 0x01, bank: Some(1) }));
        assert!(parse(CheatSystem::GameBoy, "A1000000").is_err());
    }

    #[test]
    fn several_codes_in_one_entry() {
        let v = parse(CheatSystem::GameBoy, "0163B0D1 + 0199B1D1").unwrap();
        assert_eq!(v.len(), 2);
    }

    #[test]
    fn patch_read_honours_compare() {
        let mut c = Cheats::new(CheatSystem::Nes);
        let errs = c.set(&["91D9?CE:AD".to_string(), "nonsense!".to_string(), "C000:EA".to_string()]);
        assert_eq!(errs[0], "");
        assert!(!errs[1].is_empty());
        assert_eq!(c.patch_read(0x91D9, 0xCE), 0xAD);   // compare matches
        assert_eq!(c.patch_read(0x91D9, 0x12), 0x12);   // other bank: untouched
        assert_eq!(c.patch_read(0xC000, 0x00), 0xEA);   // no compare: always
        assert_eq!(c.patch_read(0x8000, 0x55), 0x55);
        c.set(&[]);
        assert!(c.is_empty());
    }

    fn rom(addr: u32, value: u8) -> Effect { Effect::Rom(RomPatch { addr, value, compare: None }) }
    fn ram(addr: u32, value: u8) -> Effect { Effect::Ram(RamWrite { addr, value, bank: None }) }

    #[test]
    fn snes_game_genie() {
        // worked example from gamehacking.org's "Game Genie Encryption Schemes"
        assert_eq!(one(CheatSystem::Snes, "ABCD-EFFF"), rom(0xC4A704, 0xC9));
        assert!(parse(CheatSystem::Snes, "ABCD-EFFG").unwrap_err().contains("'G'"));
    }

    #[test]
    fn snes_action_replay_and_raw() {
        assert_eq!(one(CheatSystem::Snes, "7E0DBE09"), ram(0x7E0DBE, 0x09), "WRAM");
        assert_eq!(one(CheatSystem::Snes, "700010FF"), ram(0x700010, 0xFF), "LoROM SRAM");
        assert_eq!(one(CheatSystem::Snes, "00800CEA"), rom(0x00800C, 0xEA), "ROM");
        assert_eq!(one(CheatSystem::Snes, "C08000:EA"), rom(0xC08000, 0xEA));
        assert_eq!(one(CheatSystem::Snes, "008000?A9:EA"),
            Effect::Rom(RomPatch { addr: 0x008000, value: 0xEA, compare: Some(0xA9) }));
        assert!(parse(CheatSystem::Snes, "7E0DBE:1234").is_err());
        assert!(parse(CheatSystem::Snes, "7E0DBE0").is_err());
    }

    #[test]
    fn genesis_game_genie() {
        // examples from dgen's decode.c and gamehacking.org
        assert_eq!(parse(CheatSystem::Genesis, "SCRA-MBLE").unwrap(), vec![rom(0x059C44, 0xB0), rom(0x059C45, 0x78)]);
        // decodes to BE47BD:1F00; the word lands on the even address
        assert_eq!(parse(CheatSystem::Genesis, "ABD5-78F7").unwrap(), vec![rom(0xBE47BC, 0x1F), rom(0xBE47BD, 0x00)]);
        assert_eq!(parse(CheatSystem::Genesis, "scramble").unwrap(), parse(CheatSystem::Genesis, "SCRA-MBLE").unwrap());
        assert!(parse(CheatSystem::Genesis, "SCRA-MBLI").unwrap_err().contains("'I'"));
    }

    #[test]
    fn genesis_raw_codes() {
        assert_eq!(parse(CheatSystem::Genesis, "FF0A12:0009").unwrap(), vec![ram(0xFF0A12, 0x00), ram(0xFF0A13, 0x09)]);
        assert_eq!(parse(CheatSystem::Genesis, "FF0A13:63").unwrap(), vec![ram(0xFF0A13, 0x63)]);
        assert_eq!(parse(CheatSystem::Genesis, "000200:4E71").unwrap(), vec![rom(0x000200, 0x4E), rom(0x000201, 0x71)]);
        assert!(parse(CheatSystem::Genesis, "000201:4E71").is_err(), "odd word address");
    }
}
