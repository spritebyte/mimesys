// 2600 cartridges. The 6507 only sees 4 KB of cartridge space (0x1000-0x1FFF), so anything
// bigger switches banks, usually by *touching* an address near the top of the ROM (read or write).
//
//   2K / 4K   no switching (2K mirrors)
//   F8  8K    0x1FF8 / 0x1FF9 select 4K bank 0 / 1           (most 8K games)
//   F6  16K   0x1FF6-0x1FF9 select banks 0-3
//   F4  32K   0x1FF4-0x1FFB select banks 0-7
//   +SC       "Superchip": 128 bytes of extra RAM, written at 0x1000-0x107F, read at 0x1080-0x10FF
//   E0  8K    Parker Bros: four 1K slices, 0x1FE0-0x1FF7 select what's in slices 0-2, slice 3 fixed
//   3F        Tigervision: writing to 0x00-0x3F (TIA space) selects the 2K bank at 0x1000,
//             0x1800-0x1FFF is fixed to the last 2K
//
// Detection follows Stella's heuristics. Not supported yet: FE (Activision 8K, e.g. Decathlon),
// E7 (M-Network 16K), FA, F0, CV, UA, AR (Supercharger).

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scheme { Plain, F8, F6, F4, E0, Tigervision3F }

#[derive(Serialize, Deserialize, Clone)]
pub struct Cart {
    pub scheme: Scheme,
    pub superchip: bool,
    pub bank: usize,
    pub slices: [usize; 4],          // E0
    #[serde(with = "serde_bytes")]
    pub sc_ram: Vec<u8>,
    #[serde(skip)]
    rom: Vec<u8>,
}

impl Cart {
    pub fn new(rom: Vec<u8>) -> Result<Self, String> {
        let (scheme, superchip) = detect(&rom)?;
        let banks = rom.len() / 4096;
        let bank = match scheme {
            Scheme::F8 => banks - 1,            // F8 boards power up in their last bank
            _ => 0,
        };
        Ok(Self { scheme, superchip, bank, slices: [4, 5, 6, 7], sc_ram: vec![0; 128], rom })
    }

    pub fn load_state(&mut self, s: Cart) {
        let rom = std::mem::take(&mut self.rom);
        *self = s;
        self.rom = rom;
    }

    pub fn rom_len(&self) -> usize { self.rom.len() }

    /// Bank-switch hotspots react to any access, read or write.
    fn touch(&mut self, a: u16) {
        match self.scheme {
            Scheme::F8 if (0xFF8..=0xFF9).contains(&a) => self.bank = (a - 0xFF8) as usize,
            Scheme::F6 if (0xFF6..=0xFF9).contains(&a) => self.bank = (a - 0xFF6) as usize,
            Scheme::F4 if (0xFF4..=0xFFB).contains(&a) => self.bank = (a - 0xFF4) as usize,
            Scheme::E0 if (0xFE0..=0xFF7).contains(&a) => {
                let slot = ((a - 0xFE0) / 8) as usize;
                self.slices[slot] = (a & 7) as usize;
            }
            _ => {}
        }
    }

    /// `a` = address within the cartridge (0x000-0xFFF).
    pub fn read(&mut self, a: u16) -> u8 {
        self.touch(a);
        if self.superchip && a < 0x100 {
            // 0x000-0x07F is the write port: reading it returns junk (and real carts corrupt RAM)
            return if a >= 0x80 { self.sc_ram[(a - 0x80) as usize] } else { 0xFF };
        }
        let len = self.rom.len();
        let i = match self.scheme {
            Scheme::Plain => a as usize % len,
            Scheme::F8 | Scheme::F6 | Scheme::F4 => self.bank * 4096 + a as usize,
            Scheme::E0 => self.slices[(a >> 10) as usize] * 1024 + (a & 0x3FF) as usize,
            Scheme::Tigervision3F => {
                if a < 0x800 { self.bank * 2048 + a as usize } else { len - 2048 + (a - 0x800) as usize }
            }
        };
        self.rom[i % len]
    }

    pub fn write(&mut self, a: u16, v: u8) {
        self.touch(a);
        if self.superchip && a < 0x80 {
            self.sc_ram[a as usize] = v;
        }
    }

    /// Writes to TIA space (0x00-0x3F): Tigervision's bank register.
    pub fn tia_write(&mut self, a: u16, v: u8) {
        if self.scheme == Scheme::Tigervision3F && a < 0x40 {
            self.bank = v as usize % (self.rom.len() / 2048).max(1);
        }
    }
}

fn contains(rom: &[u8], pattern: &[u8]) -> usize {
    rom.windows(pattern.len()).filter(|w| *w == pattern).count()
}

/// Superchip carts leave the RAM area of every bank filled with one value.
fn is_superchip(rom: &[u8]) -> bool {
    let first = rom[0];
    rom.chunks(4096).all(|bank| bank[..256].iter().all(|&b| b == first))
}

fn is_e0(rom: &[u8]) -> bool {
    const SIGS: [[u8; 3]; 8] = [
        [0x8D, 0xE0, 0x1F], [0x8D, 0xE0, 0x5F], [0x8D, 0xE9, 0xFF], [0x0C, 0xE0, 0x1F],
        [0xAD, 0xE0, 0x1F], [0xAD, 0xE9, 0xFF], [0xAD, 0xED, 0xFF], [0xAD, 0xF3, 0xBF],
    ];
    SIGS.iter().any(|s| contains(rom, s) > 0)
}

fn is_3f(rom: &[u8]) -> bool { contains(rom, &[0x85, 0x3F]) >= 2 }   // STA $3F, twice or more

fn detect(rom: &[u8]) -> Result<(Scheme, bool), String> {
    let len = rom.len();
    Ok(match len {
        0x800 | 0x1000 => (Scheme::Plain, false),
        0x2000 => {
            if is_superchip(rom) { (Scheme::F8, true) }
            else if is_e0(rom) { (Scheme::E0, false) }
            else if is_3f(rom) { (Scheme::Tigervision3F, false) }
            else { (Scheme::F8, false) }
        }
        0x4000 => {
            if is_superchip(rom) { (Scheme::F6, true) }
            else if is_3f(rom) { (Scheme::Tigervision3F, false) }
            else { (Scheme::F6, false) }
        }
        0x8000 => {
            if is_superchip(rom) { (Scheme::F4, true) }
            else if is_3f(rom) { (Scheme::Tigervision3F, false) }
            else { (Scheme::F4, false) }
        }
        _ if len % 2048 == 0 && len > 0 && is_3f(rom) => (Scheme::Tigervision3F, false),
        _ => return Err(format!("unsupported 2600 cartridge size ({} bytes)", len)),
    })
}
