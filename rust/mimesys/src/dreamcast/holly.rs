//! Holly's system bus block (5F6800-5F7FFF): interrupt status / masks and their routing to
//! the SH-4's IRL lines, the channel-2 DMA start, and the register files of Maple
//! (5F6C00), G1 (GD-ROM DMA, 5F7400), G2 (AICA DMA, 5F7800) and PVR DMA (5F7C00).
//! Facts from the Holly documentation and MAME mame/sega/dc.cpp (read for behaviour only).

use serde::{Deserialize, Serialize};

pub const SB_C2DSTAT: u32 = 0x00;
pub const SB_C2DLEN: u32 = 0x04;
pub const SB_C2DST: u32 = 0x08;
pub const SB_SDST: u32 = 0x20;
pub const SB_LMMODE0: u32 = 0x84;
pub const SB_LMMODE1: u32 = 0x88;
pub const SB_FFST: u32 = 0x8C;
pub const SB_SFRES: u32 = 0x90;
pub const SB_SBREV: u32 = 0x9C;
pub const SB_ISTNRM: u32 = 0x100;
pub const SB_ISTEXT: u32 = 0x104;
pub const SB_ISTERR: u32 = 0x108;
/// IML2 / IML4 / IML6 NRM / EXT / ERR: 0x110 + 0x10 * (0, 1, 2) + 4 * kind
pub const SB_IML2NRM: u32 = 0x110;

pub const IST_DMA_PVR: u32 = 1 << 11;
pub const IST_DMA_MAPLE: u32 = 1 << 12;
#[allow(dead_code)] // stage 4
pub const IST_DMA_GDROM: u32 = 1 << 14;
pub const IST_DMA_AICA: u32 = 1 << 15;
pub const IST_DMA_CH2: u32 = 1 << 19;
pub const IST_DMA_SORT: u32 = 1 << 20;
/// external interrupt bits
pub const EXT_GDROM: u32 = 1 << 0;
#[allow(dead_code)] // stage 6
pub const EXT_AICA: u32 = 1 << 1;

/// What a system-block write starts.
#[derive(Debug, PartialEq)]
pub enum SbAction { None, Ch2Dma, SoftReset }

#[derive(Clone, Serialize, Deserialize)]
pub struct Holly {
    /// 5F6800-5F69FF
    pub sb: Vec<u32>,
    /// 5F6C00-5F6CFF Maple, 5F7400 G1, 5F7800 G2, 5F7C00 PVR DMA (64 words each)
    pub maple: Vec<u32>,
    pub g1: Vec<u32>,
    pub g2: Vec<u32>,
    pub pvrdma: Vec<u32>,
}

impl Default for Holly {
    fn default() -> Self { Self::new() }
}

impl Holly {
    pub fn new() -> Self {
        let mut h = Self { sb: vec![0; 0x80], maple: vec![0; 0x40], g1: vec![0; 0x40], g2: vec![0; 0x40], pvrdma: vec![0; 0x40] };
        h.reset();
        h
    }

    pub fn reset(&mut self) {
        for v in [&mut self.sb, &mut self.maple, &mut self.g1, &mut self.g2, &mut self.pvrdma] { v.fill(0); }
        self.sb[(SB_SBREV / 4) as usize] = 0x0B;
    }

    pub fn r(&self, off: u32) -> u32 { self.sb[(off as usize / 4) & 0x7F] }
    fn w(&mut self, off: u32) -> &mut u32 { &mut self.sb[(off as usize / 4) & 0x7F] }

    /// Normal interrupts happened.
    pub fn raise(&mut self, bits: u32) { *self.w(SB_ISTNRM) |= bits & 0x3FFF_FFFF; }

    /// External interrupt lines (GD-ROM, AICA, modem, expansion): level, held by the source.
    pub fn set_ext(&mut self, bit: u32, on: bool) {
        let e = self.w(SB_ISTEXT);
        if on { *e |= bit } else { *e &= !bit }
    }

    /// ISTNRM with its summary bits: 30 = an external interrupt, 31 = an error.
    pub fn istnrm(&self) -> u32 {
        self.r(SB_ISTNRM) & 0x3FFF_FFFF | ((self.r(SB_ISTEXT) != 0) as u32) << 30 | ((self.r(SB_ISTERR) != 0) as u32) << 31
    }

    /// The IRL priority Holly drives: 6, 4, 2 (IML6 first) or 0.
    pub fn level(&self) -> u32 {
        let (n, x, e) = (self.istnrm(), self.r(SB_ISTEXT), self.r(SB_ISTERR));
        for (k, level) in [(2, 6), (1, 4), (0, 2)] {
            let base = SB_IML2NRM + 0x10 * k;
            if n & self.r(base) != 0 || x & self.r(base + 4) != 0 || e & self.r(base + 8) != 0 { return level; }
        }
        0
    }

    pub fn read_sb(&self, off: u32) -> u32 {
        match off {
            SB_ISTNRM => self.istnrm(),
            SB_FFST => 0, // FIFOs empty
            _ => self.r(off),
        }
    }

    pub fn write_sb(&mut self, off: u32, v: u32) -> SbAction {
        match off {
            // writing 1 clears a normal / error interrupt; external ones clear at the source
            SB_ISTNRM => { *self.w(SB_ISTNRM) &= !v; SbAction::None }
            SB_ISTERR => { *self.w(SB_ISTERR) &= !v; SbAction::None }
            SB_ISTEXT | SB_SBREV | SB_FFST => SbAction::None,
            SB_C2DST => {
                *self.w(off) = v & 1;
                if v & 1 != 0 { SbAction::Ch2Dma } else { SbAction::None }
            }
            SB_SDST => {
                // sort DMA: not emulated, report it done
                if v & 1 != 0 { self.raise(IST_DMA_SORT); }
                SbAction::None
            }
            SB_SFRES => if v & 0xFFFF == 0x7611 { SbAction::SoftReset } else { SbAction::None },
            _ => { *self.w(off) = v; SbAction::None }
        }
    }
}
