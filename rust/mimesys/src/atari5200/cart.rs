//! Atari 5200 cartridges: 4, 8, 16 or 32 KB in 4000-BFFF. Small carts sit at the top
//! (the OS reads the cartridge's vectors at BFFE/BFFF) and repeat below:
//!   4 KB    8000-BFFF (four copies)          8 KB   8000-BFFF (two copies)
//!   16 KB one chip   8000-BFFF                16 KB two chips  4000-7FFF = first 8 KB twice,
//!                                                              8000-BFFF = second 8 KB twice
//!   32 KB   4000-BFFF
//! The two 16 KB wirings can't be told apart from the size. A `CART` header (Atari800's
//! .car format) names the type; then a CRC list of the known 16 KB carts (from MAME's
//! software list); otherwise the code is scanned: absolute JMP / JSR / loads into
//! 4000-7FFF only make sense on a two-chip cart.

use serde::{Deserialize, Serialize};

/// CRC-32s of 16 KB carts wired as two 8 KB chips (MAME's a5200 software list, slot
/// a5200_2chips). Checked before the code scan.
const TWO_CHIP_16K: &[u32] = &[
    0x04807705, // Buck Rogers - Planet of Zoom
    0x04B299A4, // Frisky Tom (prototype)
    0x0AF19345, // Frogger II: Threeedeep!
    0x0F996184, // RealSports Basketball (prototype, 19821105)
    0x10F33C90, // RealSports Tennis
    0x1D1CEE27, // Stargate (prototype)
    0x2A640143, // Montezuma's Revenge
    0x2C676662, // Jungle Hunt
    0x4019ECEC, // Astro Chase
    0x4336C2CC, // RealSports Football
    0x536A70FE, // Centipede
    0x59983C40, // Jr Pacman (prototype)
    0x69F23548, // Star Trek
    0x6A687F9C, // Dig Dug
    0x73B5B6FB, // Sport Goofy (prototype)
    0x752F5EFD, // Ms. Pac-Man
    0x75F566DF, // Star Wars - The Arcade Game
    0x7D819A9F, // Star Raiders
    0x84DF4925, // Looney Tunes Hotel (prototype)
    0x8873EF51, // Pac-Man
    0x931A454A, // Microgammon SB (prototype)
    0xA18A9A40, // Popeye
    0xA97606AB, // Road Runner (prototype)
    0xABC2D1E4, // Pole Position
    0xAEA6D2C2, // Qix
    0xB3B8E314, // Battlezone (prototype)
    0xB68D61E8, // Space Dungeon
    0xB8FAAEC3, // Xari Arena (prototype)
    0xBD52623B, // Defender
    0xBFD30C01, // Joust
    0xC597C087, // Miniature Golf (prototype)
    0xCE07D9AD, // Atari PAM Diagnostics (rev 2.3)
    0xCFD4A7F9, // Gyruss
    0xD9AE4518, // James Bond
    0xE8B130C4, // PAM Diag 2.0
    0xECBD1853, // RealSports Soccer
    0xECFA624F, // Kangaroo
    0xF1F42BBD, // Congo Bongo
    0xFD541C80, // Countermeasure
];

/// CRC-32s of 16 KB carts with one 16 KB chip (MAME's list, slot a5200_rom).
const ONE_CHIP_16K: &[u32] = &[
    0x0624E6E7, // Blue-Print
    0x0A4DDB1E, // Super Pac-Man (prototype)
    0x0BA22ECE, // Track and Field (prototype)
    0x1187342F, // Tempest (prototype)
    0x18A73AF3, // H.E.R.O.
    0x2959D827, // Zone Ranger
    0x35484751, // A.E. (prototype)
    0x387365DC, // Space Shuttle - A Journey Into Space
    0x4252ABD9, // Robotron: 2084
    0x4B910461, // David Crane's Pitfall II - Lost Caverns
    0x7DF1ADFB, // Miner 2049er
    0x83517703, // The Last Starfighter (prototype)
    0x969CFE1A, // Millipede (prototype)
    0x9AD53BBC, // Choplifter!
    0x9BAE58DC, // Beamrider
    0xAB8E035B, // Meteorites
    0xB5F3402B, // Quest for Quintana Roo
    0xBE3CD348, // Berzerk
    0xC8F9C094, // Blaster (prototype)
    0xD0B2F285, // Moon Patrol
    0xD3BD3221, // Final Legacy (prototype)
    0xD6F7DDFD, // Wizard of Wor
    0xF43E7CD0, // The Activision Decathlon
];

/// The wiring of a 16 KB cart in the CRC lists.
pub fn listed_16k(crc: u32) -> Option<Mapping> {
    if TWO_CHIP_16K.contains(&crc) { Some(Mapping::K16Two) }
    else if ONE_CHIP_16K.contains(&crc) { Some(Mapping::K16One) }
    else { None }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mapping { K4, K8, K16One, K16Two, K32 }

#[derive(Clone, Serialize, Deserialize)]
pub struct Cart5200 {
    #[serde(skip)]
    pub rom: Vec<u8>,
    pub mapping: Mapping,
}

impl Cart5200 {
    pub fn new(data: &[u8]) -> Result<Self, String> {
        let (rom, from_header) = if data.len() >= 16 && &data[0..4] == b"CART" {
            let kind = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
            let m = match kind {
                4 => Some(Mapping::K32),
                6 => Some(Mapping::K16Two),
                16 => Some(Mapping::K16One),
                19 => Some(Mapping::K8),
                20 => Some(Mapping::K4),
                _ => return Err(format!("CART type {kind} isn't a 5200 cartridge this core knows")),
            };
            (data[16..].to_vec(), m)
        } else {
            (data.to_vec(), None)
        };
        let mapping = match from_header {
            Some(m) => m,
            None => match rom.len() {
                0x1000 => Mapping::K4,
                0x2000 => Mapping::K8,
                0x4000 => {
                match listed_16k(crate::nes::rom_db::crc32(&[&rom])) {
                    Some(m) => m,
                    None => if Self::looks_two_chip(&rom) { Mapping::K16Two } else { Mapping::K16One },
                }
            }
                0x8000 => Mapping::K32,
                n => return Err(format!("{} bytes: a 5200 cartridge is 4, 8, 16 or 32 KB", n)),
            },
        };
        Ok(Self { rom, mapping })
    }

    /// 16 KB: count absolute-address instructions that point into 4000-7FFF.
    fn looks_two_chip(rom: &[u8]) -> bool {
        let (mut low, mut high) = (0u32, 0u32);
        for i in 0..rom.len().saturating_sub(2) {
            // JMP abs, JSR abs, LDA/LDX/LDY/STA abs, LDA abs,X / abs,Y
            if matches!(rom[i], 0x4C | 0x20 | 0xAD | 0xAE | 0xAC | 0x8D | 0xBD | 0xB9) {
                let a = u16::from_le_bytes([rom[i + 1], rom[i + 2]]);
                match a { 0x4000..=0x7FFF => low += 1, 0x8000..=0xBFFF => high += 1, _ => {} }
            }
        }
        low > 16 && low * 4 > high
    }

    /// A read from 4000-BFFF.
    pub fn read(&self, a: u16) -> u8 {
        let off = (a - 0x4000) as usize;
        let i = match self.mapping {
            Mapping::K32 => off,
            Mapping::K16One => if a >= 0x8000 { off - 0x4000 } else { return 0xFF },
            Mapping::K16Two => if a < 0x8000 { off & 0x1FFF } else { 0x2000 + (off & 0x1FFF) },
            Mapping::K8 => if a >= 0x8000 { off & 0x1FFF } else { return 0xFF },
            Mapping::K4 => if a >= 0x8000 { off & 0x0FFF } else { return 0xFF },
        };
        self.rom.get(i).copied().unwrap_or(0xFF)
    }
}
