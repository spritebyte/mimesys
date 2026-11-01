//! SNES cartridges: finding the header, and the LoROM / HiROM / ExHiROM memory maps.
//!
//! The internal header sits at the end of the first 32 KB bank (LoROM, 007FC0 in the
//! file), the first 64 KB bank (HiROM, 00FFC0) or past 4 MB (ExHiROM, 40FFC0). Each
//! candidate is scored the way most emulators do: map-mode byte that agrees, checksum +
//! complement = FFFF, a reset vector in ROM, a printable title. A 512-byte copier
//! header (file size = n * 1024 + 512) is dropped first.
//!
//! Maps (bank:address, banks 80-FF mirror 00-7F):
//!   LoROM   00-7D:8000-FFFF = ROM in 32 KB pieces; 40-7D:0000-7FFF mirror the same;
//!           SRAM 70-7D:0000-7FFF (and F0-FF)
//!   HiROM   40-7D:0000-FFFF = ROM in 64 KB pieces; 00-3F:8000-FFFF mirror the upper
//!           halves; SRAM 20-3F:6000-7FFF (and A0-BF)
//!   ExHiROM like HiROM, with C0-FF the first 4 MB and 40-7D the rest

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Map { LoRom, HiRom, ExHiRom }

#[derive(Clone, Serialize, Deserialize)]
pub struct Cart {
    #[serde(skip)]
    pub rom: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub sram: Vec<u8>,
    pub map: Map,
    pub title: String,
    pub fast_rom: bool,
    /// the coprocessor byte (header +16), for "this game needs a chip we don't have yet"
    pub chipset: u8,
    pub region: u8,
    #[serde(skip)]
    pub sram_dirty: bool,
}

fn score(rom: &[u8], at: usize, want: Map) -> i32 {
    if rom.len() < at + 0x40 { return -100; }
    let h = &rom[at..at + 0x40];
    let mut s = 0;
    let mode = h[0x15];
    let map_ok = match want {
        Map::LoRom => mode & 0x0F == 0 || mode & 0x0F == 2 || mode & 0x0F == 3,
        Map::HiRom => mode & 0x0F == 1,
        Map::ExHiRom => mode & 0x0F == 5,
    };
    if map_ok { s += 4; }
    if mode & 0xE0 == 0x20 { s += 2; }
    let checksum = u16::from_le_bytes([h[0x1E], h[0x1F]]);
    let complement = u16::from_le_bytes([h[0x1C], h[0x1D]]);
    if checksum ^ complement == 0xFFFF { s += 4; }
    let reset = u16::from_le_bytes([h[0x3C], h[0x3D]]);
    if reset >= 0x8000 { s += 2; } else { s -= 4; }
    if h[0..21].iter().all(|&c| (0x20..0x7F).contains(&c)) { s += 2; }
    if h[0x17] <= 0x0D { s += 1; }                           // ROM size byte
    if h[0x18] <= 0x08 { s += 1; }                           // SRAM size byte
    s
}

impl Cart {
    pub fn new(data: &[u8]) -> Result<Self, String> {
        let rom: Vec<u8> = if data.len() % 1024 == 512 { data[512..].to_vec() } else { data.to_vec() };
        if rom.len() < 0x8000 { return Err(format!("{} bytes is too small for a SNES cartridge", rom.len())); }
        let cands = [(0x7FC0usize, Map::LoRom), (0xFFC0, Map::HiRom), (0x40FFC0, Map::ExHiRom)];
        let (at, map) = cands.iter().map(|&(a, m)| (score(&rom, a, m), a, m))
            .max_by_key(|&(s, a, _)| (s, -(a as i64))).map(|(_, a, m)| (a, m)).unwrap();
        let h = &rom[at..at + 0x40];
        let title = String::from_utf8_lossy(&h[0..21]).trim().to_string();
        let sram_size = if h[0x18] > 0 && h[0x18] <= 0x08 { 1024usize << h[0x18] } else { 0 };
        let fast_rom = h[0x15] & 0x10 != 0;
        let chipset = h[0x16];
        let region = h[0x19];
        Ok(Self { rom, sram: vec![0xFF; sram_size], map, title, fast_rom, chipset, region, sram_dirty: false })
    }

    /// ROM offset with mirroring for sizes that aren't a power of two (as ares's
    /// Bus::mirror: the part past the largest power of two repeats to fill the space).
    fn mirror(&self, mut addr: usize) -> usize {
        let mut size = self.rom.len();
        if size == 0 { return 0; }
        let mut base = 0;
        let mut mask = 1usize << 23;
        while addr >= size {
            while addr & mask == 0 { mask >>= 1; }
            addr -= mask;
            if size > mask { size -= mask; base += mask; }
            mask >>= 1;
        }
        base + addr
    }

    fn rom_at(&self, off: usize) -> u8 { self.rom[self.mirror(off)] }

    /// A cartridge read; `None` for addresses the cart doesn't answer (open bus).
    pub fn read(&self, addr: u32) -> Option<u8> {
        let bank = (addr >> 16) as usize & 0xFF;
        let a = addr as usize & 0xFFFF;
        let b = bank & 0x7F;
        match self.map {
            Map::LoRom => {
                if (0x70..=0x7D).contains(&b) && a < 0x8000 && !self.sram.is_empty() && (bank >= 0xF0 || bank < 0x80) {
                    return Some(self.sram[a % self.sram.len()]);
                }
                if bank >= 0xFE && a < 0x8000 && !self.sram.is_empty() { return Some(self.sram[a % self.sram.len()]); }
                if a >= 0x8000 || (0x40..0x7E).contains(&b) {
                    return Some(self.rom_at(b * 0x8000 + (a & 0x7FFF)));
                }
                None
            }
            Map::HiRom | Map::ExHiRom => {
                if (0x20..=0x3F).contains(&b) && (0x6000..0x8000).contains(&a) && !self.sram.is_empty() {
                    return Some(self.sram[((b - 0x20) * 0x2000 + a - 0x6000) % self.sram.len()]);
                }
                let hi = self.map == Map::ExHiRom && bank & 0x80 == 0;   // ExHiROM: 00-7D = the upper 4 MB
                let base = if hi { 0x400000 } else { 0 };
                if bank >= 0x40 && b < 0x7E || a >= 0x8000 {
                    return Some(self.rom_at(base + ((b & 0x3F) << 16 | a)));
                }
                None
            }
        }
    }

    pub fn write(&mut self, addr: u32, v: u8) {
        let bank = (addr >> 16) as usize & 0xFF;
        let a = addr as usize & 0xFFFF;
        let b = bank & 0x7F;
        if self.sram.is_empty() { return; }
        let n = self.sram.len();
        let at = match self.map {
            Map::LoRom if (0x70..=0x7D).contains(&b) && a < 0x8000 => a % n,
            Map::LoRom if bank >= 0xFE && a < 0x8000 => a % n,
            Map::HiRom | Map::ExHiRom if (0x20..=0x3F).contains(&b) && (0x6000..0x8000).contains(&a) => ((b - 0x20) * 0x2000 + a - 0x6000) % n,
            _ => return,
        };
        if self.sram[at] != v { self.sram[at] = v; self.sram_dirty = true; }
    }

    /// Fast ROM speed applies to banks 80-FF (the header bit only says the game wants it).
    pub fn is_rom(&self, addr: u32) -> bool {
        let bank = (addr >> 16) & 0x7F;
        addr & 0x8000 != 0 || bank >= 0x40
    }
}
