//! DS DMA registers (4 channels per CPU). The bus runs the transfers (nds_bus.rs).
//!
//! CNT: bits 0-20 count (ARM9; ARM7: 14 bits, 16 for channel 3, 0 = max), 21-22 dest
//! (inc, dec, fixed, inc+reload), 23-24 source (inc, dec, fixed), 25 repeat, 26 32-bit,
//! 27-29 start (ARM9: now, vblank, hblank, display start, main memory display, DS cart,
//! GBA cart, geometry FIFO; ARM7 uses bits 28-29: now, vblank, DS cart, wifi), 30 IRQ,
//! 31 enable.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Timing { Now, VBlank, HBlank, DisplayStart, MainMemDisplay, Cart, GbaCart, GeometryFifo, Wifi }

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Channel {
    pub sad: u32,
    pub dad: u32,
    pub cnt: u32,
    /// internal registers, latched at enable
    pub src: u32,
    pub dst: u32,
    pub count: u32,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Dma {
    pub ch: [Channel; 4],
    pub arm9: bool,
    /// ARM9 DMA fill registers (0x040000E0)
    pub fill: [u32; 4],
}

impl Dma {
    pub fn new(arm9: bool) -> Self { Self { ch: Default::default(), arm9, fill: [0; 4] } }

    pub fn timing(&self, i: usize) -> Timing {
        let c = self.ch[i].cnt;
        if self.arm9 {
            match (c >> 27) & 7 {
                0 => Timing::Now, 1 => Timing::VBlank, 2 => Timing::HBlank, 3 => Timing::DisplayStart,
                4 => Timing::MainMemDisplay, 5 => Timing::Cart, 6 => Timing::GbaCart, _ => Timing::GeometryFifo,
            }
        } else {
            match (c >> 28) & 3 { 0 => Timing::Now, 1 => Timing::VBlank, 2 => Timing::Cart, _ => Timing::Wifi }
        }
    }

    pub fn max_count(&self, i: usize) -> u32 {
        if self.arm9 { 0x20_0000 } else if i == 3 { 0x1_0000 } else { 0x4000 }
    }

    pub fn word_count(&self, i: usize) -> u32 {
        let mask = self.max_count(i) - 1;
        let n = self.ch[i].cnt & mask;
        if n == 0 { self.max_count(i) } else { n }
    }

    pub fn read(&self, off: u32) -> u32 {
        let i = (off / 12) as usize;
        match off % 12 {
            0 => self.ch[i].sad,
            4 => self.ch[i].dad,
            _ => self.ch[i].cnt,
        }
    }

    /// Returns the channel that was just enabled, if any.
    pub fn write(&mut self, off: u32, v: u32, mask: u32) -> Option<usize> {
        let i = (off / 12) as usize;
        let ch = &mut self.ch[i];
        let addr_mask = if self.arm9 { 0x0FFF_FFFF } else if i == 0 { 0x07FF_FFFF } else { 0x0FFF_FFFF };
        match off % 12 {
            0 => { ch.sad = ((ch.sad & !mask) | (v & mask)) & addr_mask; None }
            4 => { ch.dad = ((ch.dad & !mask) | (v & mask)) & addr_mask; None }
            _ => {
                let was = ch.cnt & 0x8000_0000 != 0;
                ch.cnt = (ch.cnt & !mask) | (v & mask);
                if !was && ch.cnt & 0x8000_0000 != 0 {
                    ch.src = ch.sad;
                    ch.dst = ch.dad;
                    let n = self.word_count(i);
                    self.ch[i].count = n;
                    Some(i)
                } else {
                    None
                }
            }
        }
    }
}
