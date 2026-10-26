//! GBA DMA channel registers (0x040000B0-0x040000DF). The transfers themselves run
//! in the bus (`GbaBus::run_dma`), which owns the memory.
//!
//! Per channel: SAD, DAD (write only), CNT_L word count (write only), CNT_H control:
//!   bits 5-6 dest step (inc, dec, fixed, inc+reload)  7-8 source step (inc, dec, fixed)
//!   9 repeat  10 32-bit  12-13 timing (now, VBlank, HBlank, special)  14 IRQ  15 enable

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
pub struct DmaChan {
    pub sad: u32,
    pub dad: u32,
    pub cnt: u16,
    pub ctrl: u16,
    /// internal registers, latched when the channel is enabled
    pub src: u32,
    pub dst: u32,
    pub count: u32,
    /// waiting to run
    pub pending: bool,
}

pub const TIMING_NOW: u16 = 0;
pub const TIMING_VBLANK: u16 = 1;
pub const TIMING_HBLANK: u16 = 2;
pub const TIMING_SPECIAL: u16 = 3;

/// Address masks: DMA0 can only reach internal memory; DMA3 can write to the cart.
const SRC_MASK: [u32; 4] = [0x07FF_FFFF, 0x0FFF_FFFF, 0x0FFF_FFFF, 0x0FFF_FFFF];
const DST_MASK: [u32; 4] = [0x07FF_FFFF, 0x07FF_FFFF, 0x07FF_FFFF, 0x0FFF_FFFF];

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Dma {
    pub ch: [DmaChan; 4],
    /// last value a DMA moved: what a DMA read of BIOS or unmapped memory returns
    pub latch: u32,
}

impl DmaChan {
    pub fn timing(&self) -> u16 { (self.ctrl >> 12) & 3 }
    pub fn enabled(&self) -> bool { self.ctrl & 0x8000 != 0 }
}

impl Dma {
    pub fn new() -> Self { Self::default() }

    /// Word count to use: CNT_L, 0 meaning the maximum (0x4000, or 0x10000 for DMA3).
    pub fn full_count(&self, n: usize) -> u32 {
        let max = if n == 3 { 0x1_0000 } else { 0x4000 };
        let c = self.ch[n].cnt as u32 & (max - 1);
        if c == 0 { max } else { c }
    }

    /// Byte read of 0xB0-0xDF (only CNT_H reads back).
    pub fn read8(&self, off: u32) -> u8 {
        let n = (off / 12) as usize;
        let r = off % 12;
        match r {
            10 => self.ch[n].ctrl as u8,
            11 => (self.ch[n].ctrl >> 8) as u8,
            _ => 0,
        }
    }

    /// Byte write of 0xB0-0xDF. Enabling a channel latches its addresses and count;
    /// an "immediate" channel becomes pending.
    pub fn write8(&mut self, off: u32, v: u8) {
        let n = (off / 12) as usize;
        let r = off % 12;
        let set32 = |x: &mut u32, b: u32| { *x = (*x & !(0xFF << (b * 8))) | ((v as u32) << (b * 8)); };
        match r {
            0..=3 => set32(&mut self.ch[n].sad, r),
            4..=7 => set32(&mut self.ch[n].dad, r - 4),
            8 => self.ch[n].cnt = (self.ch[n].cnt & 0xFF00) | v as u16,
            9 => self.ch[n].cnt = (self.ch[n].cnt & 0x00FF) | ((v as u16) << 8),
            10 => self.ch[n].ctrl = (self.ch[n].ctrl & 0xFF00) | (v as u16 & 0xE0),
            _ => {
                let was_on = self.ch[n].enabled();
                let mask = if n == 3 { 0xFF } else { 0xF7 };   // bit 11 (game pak DRQ): DMA3 only
                self.ch[n].ctrl = (self.ch[n].ctrl & 0x00FF) | (((v & mask) as u16) << 8);
                if !was_on && self.ch[n].enabled() {
                    let count = self.full_count(n);
                    let c = &mut self.ch[n];
                    c.src = c.sad & SRC_MASK[n];
                    c.dst = c.dad & DST_MASK[n];
                    c.count = count;
                    c.pending = c.timing() == TIMING_NOW;
                } else if !self.ch[n].enabled() {
                    self.ch[n].pending = false;
                }
            }
        }
    }

    /// VBlank / HBlank start: channels waiting for that timing become pending.
    pub fn trigger(&mut self, timing: u16) {
        for c in &mut self.ch {
            if c.enabled() && c.timing() == timing { c.pending = true; }
        }
    }

    /// Sound FIFO A (0) / B (1) wants data: DMA 1/2 set to special timing and
    /// pointed at that FIFO become pending.
    pub fn trigger_fifo(&mut self, fifo: usize) {
        let addr = 0x0400_00A0 + fifo as u32 * 4;
        for n in 1..3 {
            let c = &mut self.ch[n];
            if c.enabled() && c.timing() == TIMING_SPECIAL && c.dad & !3 == addr { c.pending = true; }
        }
    }

    pub fn any_pending(&self) -> bool { self.ch.iter().any(|c| c.pending) }
}
