//! The Sega CD's graphics chip ("ASIC"): draws a rotated / scaled image into word RAM,
//! one line at a time, from a map of 16x16 or 32x32 "stamps" (4-bit pixels, also in word
//! RAM). For each line it reads a start point and a step from the "trace vector" table.
//! When the last line is done it raises the sub CPU's level-1 interrupt. The BIOS's
//! spinning logo is drawn with it, and so are many games' scaling effects.
//! Ported from ares md/mcd/gpu.cpp (ISC license).
//!
//! Registers (sub CPU): FF8058 stamp size / repeat (bit 15 = busy), FF805A stamp map base,
//! FF805C image height in cells, FF805E image base, FF8060 pixel offset, FF8062 width
//! in dots, FF8064 height in dots (counts down), FF8066 trace vector base (writing starts).

use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Gpu {
    pub repeat: bool,
    /// false 16x16 stamps, true 32x32
    pub tile_size: bool,
    /// false: a 1x1 screen map (256x256 dots), true: 16x16 screens (4096x4096)
    pub map_size: bool,
    pub map_base: u32,
    map_address: u32,
    pub image_base: u32,
    pub offset: u8,
    pub vcells: u8,
    pub vdots: u8,
    pub hdots: u16,
    image_address: u32,
    pub vector_base: u32,
    vector_address: u32,
    pub active: bool,
    counter: u32,
    period: u32,
}

/// Word RAM as 4-bit pixels: nibble 0 is the top of a word's high byte.
fn nib(wram: &[u8], a: u32) -> u8 {
    let a = a & 0x7FFFF;
    let b = wram[(a >> 1) as usize];
    if a & 1 == 0 { b >> 4 } else { b & 15 }
}

fn set_nib(wram: &mut [u8], a: u32, v: u8) {
    let a = a & 0x7FFFF;
    let b = &mut wram[(a >> 1) as usize];
    *b = if a & 1 == 0 { (*b & 0x0F) | v << 4 } else { (*b & 0xF0) | (v & 15) };
}

fn word(wram: &[u8], i: u32) -> u16 {
    let i = (i & 0x1FFFF) as usize * 2;
    (wram[i] as u16) << 8 | wram[i + 1] as u16
}

impl Gpu {
    /// FF8066 written: start drawing (only in 2M word RAM mode).
    pub fn start(&mut self, wram_1m: bool) {
        if wram_1m { return; }
        self.active = true;
        self.period = 5 * self.hdots as u32;
        self.counter = 0;
        self.image_address = ((self.image_base << 1) + self.offset as u32) & 0x7FFFF;
        self.vector_address = (self.vector_base >> 1) & 0x1FFFF;
        let mut m = self.map_base >> 1;
        m &= match (self.map_size, self.tile_size) {
            (false, false) => 0x1FF00,
            (false, true) => 0x1FFC0,
            (true, false) => 0x10000,
            (true, true) => 0x1C000,
        };
        self.map_address = m;
    }

    /// `clocks` sub clocks pass. True when the image is finished (level-1 interrupt).
    pub fn step(&mut self, clocks: u32, wram: &mut [u8], priority: u8) -> bool {
        if !self.active { return false; }
        self.counter += clocks;
        while self.counter >= self.period.max(1) {
            self.counter -= self.period.max(1);
            self.render(wram, priority);
            self.image_address = (self.image_address + 8) & 0x7FFFF;
            self.vdots = self.vdots.wrapping_sub(1);
            if self.vdots == 0 {
                self.active = false;
                return true;
            }
        }
        false
    }

    /// One line of the image.
    fn render(&mut self, wram: &mut [u8], priority: u8) {
        let mut stamp_shift = 4u32;
        let mut map_shift = 4u32 << self.map_size as u32;
        let mut index_mask: u16 = 0x7FF;
        let mut pixel_mask: u32 = 0x0F;
        if self.tile_size {
            stamp_shift += 1;
            map_shift -= 1;
            index_mask &= !3;
            pixel_mask |= 0x10;
        }
        let image_width = ((self.vcells as u32 & 31) + 1) << 6;
        let map_mask: u32 = if !self.map_size { 0x07FFFF } else { 0x7FFFFF };

        let va = self.vector_address;
        let mut x = (word(wram, va) as u32) << 8 & 0xFFFFFF; // 13.3 -> 13.11
        let mut y = (word(wram, va + 1) as u32) << 8 & 0xFFFFFF;
        let xstep = word(wram, va + 2) as i16 as i32;
        let ystep = word(wram, va + 3) as i16 as i32;
        self.vector_address = (va + 4) & 0x1FFFF;

        let mut address = self.image_address;
        for _ in 0..(self.hdots & 0x1FF) {
            if self.repeat {
                x &= map_mask;
                y &= map_mask;
            }
            let mut output = 0u8;
            if (x | y) & !map_mask == 0 {
                let mut xt = x >> 11;
                let mut yt = y >> 11;
                let xstamp = xt >> stamp_shift;
                let ystamp = yt >> stamp_shift;
                let entry = word(wram, self.map_address + (ystamp << map_shift) + xstamp);
                let index = entry & index_mask;
                if index != 0 {
                    if entry & 0x8000 != 0 { xt = !xt; }               // h flip
                    if entry & 0x4000 != 0 { xt = !xt; yt = !yt; }     // 180 degrees
                    if entry & 0x2000 != 0 { let t = xt; xt = !yt; yt = t; } // 90 degrees
                    let xp = xt & pixel_mask;
                    let yp = yt & pixel_mask;
                    output = nib(wram, (index as u32) << 8 | (xp & !7) << stamp_shift | yp << 3 | (xp & 7));
                }
            }
            let input = nib(wram, address);
            let out = match priority & 3 {
                0 => output,
                1 => if input != 0 { input } else { output }, // under: only into empty pixels
                2 => if output != 0 { output } else { input }, // over: transparent doesn't draw
                _ => input,
            };
            set_nib(wram, address, out);
            address = (address + 1) & 0x7FFFF;
            if address & 7 == 0 { address = (address + image_width - 8) & 0x7FFFF; }
            x = (x as i32).wrapping_add(xstep) as u32 & 0xFFFFFF;
            y = (y as i32).wrapping_add(ystep) as u32 & 0xFFFFFF;
        }
    }
}
