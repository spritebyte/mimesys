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
//
// The bus calls `patch_read` on cartridge reads (cheap when there are no patches) and the
// system calls `ram_writes()` after each frame.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CheatSystem {
    Nes,
    GameBoy,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RomPatch {
    pub addr: u16,
    pub value: u8,
    pub compare: Option<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RamWrite {
    pub addr: u16,
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
        for p in &self.rom {
            if p.addr == addr && p.compare.map_or(true, |c| c == value) {
                return p.value;
            }
        }
        value
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
        out.push(parse_one(system, part).map_err(|e| format!("{part}: {e}"))?);
    }
    if out.is_empty() {
        return Err("empty code".into());
    }
    Ok(out)
}

fn parse_one(system: CheatSystem, code: &str) -> Result<Effect, String> {
    let code = code.trim().to_ascii_uppercase();
    if code.contains(':') {
        return parse_raw(system, &code);
    }
    match system {
        CheatSystem::Nes => nes_game_genie(&code),
        CheatSystem::GameBoy => {
            let digits: String = code.chars().filter(|&c| c != '-').collect();
            match (digits.len(), code.contains('-')) {
                (8, false) => gb_gameshark(&digits),
                (6, _) | (9, _) => gb_game_genie(&digits),
                _ => Err("expected a Game Genie (ABC-DEF-GHI) or GameShark (01VVAAAA) code".into()),
            }
        }
    }
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
    let (addr, value, compare) = (addr as u16, value as u8, compare.map(|c| c as u8));
    let is_rom = match system {
        CheatSystem::Nes => addr >= 0x8000,
        CheatSystem::GameBoy => addr < 0x8000,
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
    Ok(Effect::Rom(RomPatch { addr, value: value as u8, compare }))
}

/// GB Game Genie, digits ABCDEF[GHI]: value = AB, address = (~F << 12) | CDE,
/// compare = (G << 4 | I) rotated right 2, XOR $BA. (H is a check digit, ignored.)
fn gb_game_genie(d: &str) -> Result<Effect, String> {
    let value = hex(&d[0..2])? as u8;
    let low = hex(&d[2..5])? as u16;
    let high = (!hex(&d[5..6])? & 0xF) as u16;
    let addr = (high << 12) | low;
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
    let addr = (hex(&d[6..8])? << 8 | hex(&d[4..6])?) as u16;
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
}
