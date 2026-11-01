//! DS video memory: nine banks (A-D 128 KB, E 64 KB, F/G 16 KB, H 32 KB, I 16 KB) that the
//! ARM9 maps with VRAMCNT_A..I (0x04000240-0x04000249): bit 7 enable, bits 0-2 the use
//! (MST), bits 3-4 an offset. Each use puts the bank somewhere:
//!
//!   LCDC (plain CPU access at 0x06800000+), engine A / B backgrounds and sprites,
//!   ARM7 memory (C, D), 3D textures / texture palettes, and the extended palettes:
//!   BG slots 0-3 (8 KB each; E, F/G for engine A, H for B) and OBJ (8 KB; F/G for A,
//!   I for B). Only the 2D engines see the extended palettes (the CPU uses LCDC).
//!
//! Regions are kept as 16 KB pages listing the banks there. Overlapping banks read as
//! the OR of their bytes and writes go to all of them, as on the console.

use serde::{Deserialize, Serialize};

pub const BANK_SIZE: [usize; 9] = [0x20000, 0x20000, 0x20000, 0x20000, 0x10000, 0x4000, 0x4000, 0x8000, 0x4000];
/// LCDC addresses of each bank (offset from 0x06800000)
const LCDC_BASE: [u32; 9] = [0x00000, 0x20000, 0x40000, 0x60000, 0x80000, 0x90000, 0x94000, 0x98000, 0xA0000];
const PAGE: u32 = 0x4000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Region { ABg, BBg, AObj, BObj, Lcdc, Arm7, Texture, TexPal, ABgExt, AObjExt, BBgExt, BObjExt }

impl Region {
    fn pages(self) -> usize {
        match self {
            Region::ABg => 32, Region::BBg => 8, Region::AObj => 16, Region::BObj => 8,
            Region::Lcdc => 41, Region::Arm7 => 16, Region::Texture => 32, Region::TexPal => 6,
            Region::ABgExt => 2, Region::AObjExt => 1, Region::BBgExt => 2, Region::BObjExt => 1,
        }
    }
    fn index(self) -> usize { self as usize }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Vram {
    pub banks: Vec<Vec<u8>>,
    pub cnt: [u8; 9],
    /// per region, per 16 KB page: (bank, byte offset in the bank)
    #[serde(skip)]
    maps: Vec<Vec<Vec<(u8, u32)>>>,
}

impl Default for Vram { fn default() -> Self { Self::new() } }

impl Vram {
    pub fn new() -> Self {
        let mut v = Self {
            banks: BANK_SIZE.iter().map(|&s| vec![0u8; s]).collect(),
            cnt: [0; 9],
            maps: Vec::new(),
        };
        v.remap();
        v
    }

    pub fn set_cnt(&mut self, bank: usize, v: u8) {
        if self.cnt[bank] != v {
            self.cnt[bank] = v;
            self.remap();
        }
    }

    /// Rebuild every region's page list from VRAMCNT.
    pub fn remap(&mut self) {
        const ALL: [Region; 12] = [Region::ABg, Region::BBg, Region::AObj, Region::BObj, Region::Lcdc, Region::Arm7, Region::Texture, Region::TexPal,
                                   Region::ABgExt, Region::AObjExt, Region::BBgExt, Region::BObjExt];
        self.maps = ALL.iter().map(|r| vec![Vec::new(); r.pages()]).collect();
        for bank in 0..9 {
            let c = self.cnt[bank];
            if c & 0x80 == 0 { continue; }
            let ofs = ((c >> 3) & 3) as u32;
            let size = BANK_SIZE[bank] as u32;
            let mst = c & if bank < 2 || bank >= 7 { 3 } else { 7 };
            // (region, byte address inside the region) or None for unmapped
            let place: Option<(Region, u32)> = match (bank, mst) {
                (_, 0) => Some((Region::Lcdc, LCDC_BASE[bank])),
                (0..=3, 1) => Some((Region::ABg, 0x20000 * ofs)),
                (0 | 1, 2) => Some((Region::AObj, 0x20000 * (ofs & 1))),
                (0..=3, 3) => Some((Region::Texture, 0x20000 * ofs)),
                (2, 2) | (3, 2) => Some((Region::Arm7, 0x20000 * (ofs & 1))),
                (2, 4) => Some((Region::BBg, 0)),
                (3, 4) => Some((Region::BObj, 0)),
                (4, 1) => Some((Region::ABg, 0)),
                (4, 2) => Some((Region::AObj, 0)),
                (4, 3) => Some((Region::TexPal, 0)),
                (4, 4) => Some((Region::ABgExt, 0)),
                (5 | 6, 1) => Some((Region::ABg, 0x4000 * (ofs & 1) + 0x10000 * (ofs >> 1))),
                (5 | 6, 2) => Some((Region::AObj, 0x4000 * (ofs & 1) + 0x10000 * (ofs >> 1))),
                (5 | 6, 3) => Some((Region::TexPal, 0x4000 * ((ofs & 1) + (ofs >> 1) * 4))),
                (5 | 6, 4) => Some((Region::ABgExt, 0x4000 * (ofs & 1))),
                (5 | 6, 5) => Some((Region::AObjExt, 0)),
                (7, 1) => Some((Region::BBg, 0)),
                (7, 2) => Some((Region::BBgExt, 0)),
                (8, 1) => Some((Region::BBg, 0x8000)),
                (8, 2) => Some((Region::BObj, 0)),
                (8, 3) => Some((Region::BObjExt, 0)),
                _ => None,
            };
            if let Some((region, base)) = place {
                let pages = &mut self.maps[region.index()];
                for k in 0..size / PAGE {
                    let page = ((base / PAGE) + k) as usize;
                    if page < pages.len() { pages[page].push((bank as u8, k * PAGE)); }
                }
            }
            // small banks also appear further up the engines' spaces (as melonDS maps them):
            // F/G 32 KB above their place; H again at 64 KB; I at every 16 KB page of engine
            // B's BG pages 2-3 / 6-7 and of all of engine B's sprite space
            let mirrors: Vec<(Region, usize, u32)> = match (bank, mst, place) {
                (5 | 6, 1 | 2, Some((r, base))) => vec![(r, (base / PAGE) as usize + 2, 0)],
                (7, 1, _) => vec![(Region::BBg, 4, 0), (Region::BBg, 5, PAGE)],
                (8, 1, _) => vec![(Region::BBg, 3, 0), (Region::BBg, 6, 0), (Region::BBg, 7, 0)],
                (8, 2, _) => (1..8).map(|p| (Region::BObj, p, 0)).collect(),
                _ => Vec::new(),
            };
            for (region, page, off) in mirrors {
                let pages = &mut self.maps[region.index()];
                if page < pages.len() { pages[page].push((bank as u8, off)); }
            }
        }
    }

    fn locate(&self, region: Region, a: u32) -> &[(u8, u32)] {
        let pages = &self.maps[region.index()];
        let page = (a / PAGE) as usize;
        pages.get(page).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// For each 16 KB page of `region`: the bank bytes when exactly one bank is there,
    /// else an empty slice (unmapped or overlapping: use `read8`).
    pub fn page_slices(&self, region: Region) -> Vec<&[u8]> {
        self.maps[region.index()].iter().map(|l| {
            if l.len() == 1 {
                let (b, off) = l[0];
                &self.banks[b as usize][off as usize..(off + PAGE) as usize]
            } else {
                &[][..]
            }
        }).collect()
    }

    #[inline]
    pub fn read8(&self, region: Region, a: u32) -> u8 {
        let l = self.locate(region, a);
        if l.len() == 1 { return self.banks[l[0].0 as usize][(l[0].1 + (a & (PAGE - 1))) as usize]; }
        let mut v = 0;
        for &(b, off) in self.locate(region, a) {
            v |= self.banks[b as usize][(off + (a & (PAGE - 1))) as usize];
        }
        v
    }

    pub fn read16(&self, region: Region, a: u32) -> u16 {
        let a = a & !1;
        self.read8(region, a) as u16 | (self.read8(region, a + 1) as u16) << 8
    }

    pub fn read32(&self, region: Region, a: u32) -> u32 {
        let a = a & !3;
        self.read16(region, a) as u32 | (self.read16(region, a + 2) as u32) << 16
    }

    pub fn write8(&mut self, region: Region, a: u32, v: u8) {
        let targets: Vec<(u8, u32)> = self.locate(region, a).to_vec();
        for (b, off) in targets {
            self.banks[b as usize][(off + (a & (PAGE - 1))) as usize] = v;
        }
    }

    pub fn write16(&mut self, region: Region, a: u32, v: u16) {
        let a = a & !1;
        self.write8(region, a, v as u8);
        self.write8(region, a + 1, (v >> 8) as u8);
    }

    pub fn write32(&mut self, region: Region, a: u32, v: u32) {
        let a = a & !3;
        self.write16(region, a, v as u16);
        self.write16(region, a + 2, (v >> 16) as u16);
    }

    /// The ARM9's 0x06xxxxxx window: which region and offset.
    pub fn arm9_region(addr: u32) -> (Region, u32) {
        match (addr >> 21) & 7 {
            0 => (Region::ABg, addr & 0x7FFFF),
            1 => (Region::BBg, addr & 0x1FFFF),
            2 => (Region::AObj, addr & 0x3FFFF),
            3 => (Region::BObj, addr & 0x1FFFF),
            _ => (Region::Lcdc, addr & 0xFFFFF),
        }
    }

    /// VRAMSTAT (ARM7 0x04000240): bit 0 = bank C is ARM7 memory, bit 1 = bank D.
    pub fn arm7_stat(&self) -> u8 {
        let is7 = |bank: usize| self.cnt[bank] & 0x87 == 0x82;
        is7(2) as u8 | (is7(3) as u8) << 1
    }

    /// Make the page lists again after loading a save state.
    pub fn after_load(&mut self) { self.remap(); }
}
