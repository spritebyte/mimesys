//! Fixes for iNES headers that are known to be wrong, keyed by the CRC32 of the PRG+CHR
//! data (the header itself isn't part of it, like Mesen's game database). The CRC is
//! printed when a ROM loads, so new entries can be added from that line.

use crate::nes::mappers::Mirroring;

pub struct HeaderFix {
    pub name: &'static str,
    pub mirroring: Option<Mirroring>,
    pub mapper: Option<u16>,
    pub submapper: Option<u8>,
}

/// (CRC32 of PRG+CHR, fix)
const FIXES: &[(u32, HeaderFix)] = &[
    // header says horizontal; the game scrolls into $2400 expecting vertical (character
    // select stripes, freeze at race start)
    (0x9235B57B, HeaderFix { name: "Micro Machines (USA) (Unl)", mirroring: Some(Mirroring::Vertical), mapper: None, submapper: None }),
    (0xA7B0536C, HeaderFix { name: "Don Doko Don 2", mirroring: None, mapper: Some(48), submapper: None }),
    (0xAEBD6549, HeaderFix { name: "Bakushou!! Jinsei Gekijou 3", mirroring: None, mapper: Some(48), submapper: None }),
    (0x40C0AD47, HeaderFix { name: "Flintstones, The - The Rescue of Dino & Hoppy (Japan)", mirroring: None, mapper: Some(48), submapper: None }),
    (0x1500E835, HeaderFix { name: "Jetsons, The - Cogswell's Caper (Japan)", mirroring: None, mapper: Some(48), submapper: None }),
    (0x0F141525, HeaderFix { name: "Arkanoid 2 (Japan)", mirroring: None, mapper: Some(152), submapper: None }),
    (0xBDA8F8E4, HeaderFix { name: "Gegege no Kitarou 2 (Japan)", mirroring: None, mapper: Some(152), submapper: None }),
    (0xB1A94B82, HeaderFix { name: "Pocket Zaurus - Juu Ouken no Nazo (Japan)", mirroring: None, mapper: Some(152), submapper: None }),
    (0x026C5FCA, HeaderFix { name: "Saint Seiya - Ougon Densetsu (Japan)", mirroring: None, mapper: Some(152), submapper: None }),
];

pub fn entries() -> usize { FIXES.len() }

pub fn lookup(crc: u32) -> Option<&'static HeaderFix> {
    FIXES.iter().find(|(c, _)| *c == crc).map(|(_, f)| f)
}

/// Standard CRC-32 (IEEE, as used by zip / No-Intro / Mesen).
pub fn crc32(parts: &[&[u8]]) -> u32 {
    let mut table = [0u32; 256];
    for i in 0..256u32 {
        let mut c = i;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB88320 ^ (c >> 1) } else { c >> 1 };
        }
        table[i as usize] = c;
    }
    let mut crc = 0xFFFF_FFFFu32;
    for part in parts {
        for &b in *part {
            crc = table[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    #[test]
    fn crc32_check_value() {
        assert_eq!(super::crc32(&[b"123456789"]), 0xCBF43926);
    }
}
#[cfg(test)]
mod lookup_tests {
    #[test]
    fn micro_machines_entry() {
        assert!(super::lookup(0x9235B57B).is_some());
    }
}
