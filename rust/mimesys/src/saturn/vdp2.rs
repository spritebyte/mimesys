//! VDP2, the background processor: 512 KB of VRAM, 4 KB of colour RAM and 288 bytes of
//! registers (Sega's "VDP2 User's Manual"; register names as in libyaul's vdp2/map.h).
//!
//! The registers and the video timing are here; drawing (the scroll screens, the sprite
//! layer from VDP1, priorities, colour calculation) is in `vdp2_render.rs`.
//!
//! - TVMD (000h): DISP bit 15, BDCLMD bit 8, LSMD bits 7-6 (3 = double-density interlace),
//!   VRESO bits 5-4 (224 / 240 / 256 lines), HRESO bits 2-0 (320 / 352 / 640 / 704 dots).
//! - TVSTAT (004h): VBLANK bit 3, HBLANK bit 2, ODD bit 1, PAL bit 0.
//! - BKTAU / BKTAL (0ACh / 0AEh): the back screen's colour (RGB555, blue in the top bits)
//!   at VRAM word address (BKTA), one colour or one per line (BKCLMD, bit 15).

use serde::{Deserialize, Serialize};

pub const VRAM_SIZE: usize = 0x8_0000;
pub const CRAM_SIZE: usize = 0x1000;

#[derive(Clone, Serialize, Deserialize)]
pub struct Vdp2 {
    #[serde(with = "serde_bytes")]
    pub vram: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub cram: Vec<u8>,
    /// the registers, as 16-bit words (offset / 2)
    pub regs: Vec<u16>,
    pub pal: bool,
    /// the current line and whether it's in hblank / vblank (the system's timing loop)
    pub line: u32,
    pub hblank: bool,
    pub vblank: bool,
    pub odd: bool,
    /// the dot within the line, for HCNT (set by the timing loop)
    pub dot: u32,
}

impl Vdp2 {
    pub fn new(pal: bool) -> Self {
        Self {
            vram: vec![0; VRAM_SIZE], cram: vec![0; CRAM_SIZE], regs: vec![0; 0x100], pal,
            line: 0, hblank: false, vblank: false, odd: false, dot: 0,
        }
    }

    pub fn tvmd(&self) -> u16 { self.regs[0] }

    /// Dots per line: 320 / 352 (x2 in the hi-res modes)
    pub fn width(&self) -> u32 {
        match self.tvmd() & 7 {
            0 | 4 => 320,
            1 | 5 => 352,
            2 | 6 => 640,
            _ => 704,
        }
    }

    /// Displayed lines in one field: 224 / 240 / 256 (256 only on PAL)
    pub fn field_lines(&self) -> u32 {
        match (self.tvmd() >> 4) & 3 {
            0 => 224,
            1 => 240,
            _ => if self.pal { 256 } else { 240 },
        }
    }

    pub fn interlaced(&self) -> bool { (self.tvmd() >> 6) & 3 == 3 }

    /// Lines in a frame (one field)
    pub fn total_lines(&self) -> u32 { if self.pal { 313 } else { 263 } }

    /// The 352-dot clock (HRESO bit 0): 28.6 MHz instead of 26.8 MHz
    pub fn dot352(&self) -> bool { self.tvmd() & 1 != 0 }

    pub fn read_reg(&mut self, off: u32) -> u16 {
        let i = (off as usize >> 1) & 0xFF;
        match i {
            // TVSTAT
            2 => (self.vblank as u16) << 3 | (self.hblank as u16) << 2 | (self.odd as u16) << 1 | self.pal as u16,
            // VRSIZE: VRAM size 4 Mbit, version 0
            3 => self.regs[3] & 0x8000,
            // HCNT (dots x 2 in the normal modes), VCNT
            4 => ((self.dot << 1) & 0x3FF) as u16,
            5 => {
                let l = if self.interlaced() { self.line << 1 | self.odd as u32 } else { self.line };
                (l & 0x3FF) as u16
            }
            _ => self.regs[i],
        }
    }

    pub fn write_reg(&mut self, off: u32, v: u16, mask: u16) {
        let i = (off as usize >> 1) & 0xFF;
        if matches!(i, 2 | 4 | 5) { return; }   // read-only: TVSTAT, HCNT, VCNT
        self.regs[i] = (self.regs[i] & !mask) | (v & mask);
    }
}

/// A Saturn RGB555 colour (bits 14-10 blue, 9-5 green, 4-0 red) as RGBA
#[allow(dead_code)]
pub fn rgb555_rgba(c: u16) -> [u8; 4] {
    let e = |v: u16| { let v = (v & 31) as u8; v << 3 | v >> 2 };
    [e(c), e(c >> 5), e(c >> 10), 0xFF]
}
