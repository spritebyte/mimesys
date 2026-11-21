//! The SCU (System Control Unit): the bridge between the SH-2s' bus, the A-bus (CD block,
//! cartridge) and the B-bus (VDP1, VDP2, SCSP). It owns the master SH-2's interrupt
//! controller, two timers, three DMA channels and a DSP. Behaviour from MAME's
//! sega/saturn_scu.cpp (Angelo Salese; BSD-3-Clause); registers at 0x05FE0000.
//!
//! - Interrupts: IST (0A4h) collects sources, IMS (0A0h) masks them. The highest-priority
//!   unmasked one goes to the master SH-2 with its level and vector (40h + bit); taking it
//!   clears its IST bit.
//! - Timer 0 counts HBlank-INs from VBlank-OUT and interrupts when the count equals T0C.
//!   Timer 1 interrupts T1S x 4 CPU clocks after each HBlank-IN (T1MD bit 8: only on timer
//!   0's line). Both only with T1MD bit 0.
//! - DMA levels 0-2 (at 000h / 020h / 040h): direct (source, destination, byte count) or
//!   indirect (a table of count / destination / source, bit 31 of the source ends it),
//!   started by a write (DxEN bit 0, factor 7) or by an event (VBlank-IN / -OUT, HBlank-IN,
//!   the timers, the sound request, VDP1's draw end). Stage 1 moves the data at once.
//! - The DSP (080h-08Ch) is stored only (stage 6).

use serde::{Deserialize, Serialize};

pub const IST_VBLANK_IN: u32 = 1 << 0;
pub const IST_VBLANK_OUT: u32 = 1 << 1;
pub const IST_HBLANK_IN: u32 = 1 << 2;
pub const IST_TIMER0: u32 = 1 << 3;
pub const IST_TIMER1: u32 = 1 << 4;
pub const IST_DSP_END: u32 = 1 << 5;
pub const IST_SOUND: u32 = 1 << 6;
pub const IST_SMPC: u32 = 1 << 7;
pub const IST_PAD: u32 = 1 << 8;
pub const IST_DMA_ILLEGAL: u32 = 1 << 12;
pub const IST_SPRITE_END: u32 = 1 << 13;

/// The interrupt level of each IST bit (0-13)
const LEVEL: [u8; 14] = [15, 14, 13, 12, 11, 10, 9, 8, 8, 6, 6, 5, 3, 2];

/// DMA start factors
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event { VBlankIn = 0, VBlankOut = 1, HBlankIn = 2, Timer0 = 3, Timer1 = 4, Sound = 5, SpriteEnd = 6, Now = 7 }

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Dma {
    pub src: u32,
    pub dst: u32,
    pub count: u32,
    /// source step 0 / 4, destination step 0 / 2 / 4 ... 128 (bytes)
    pub src_add: u32,
    pub dst_add: u32,
    pub enable: bool,
    pub indirect: bool,
    pub rup: bool,
    pub wup: bool,
    pub factor: u8,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Scu {
    pub dma: [Dma; 3],
    pub ims: u32,
    pub ist: u32,
    t0c: u32,
    t1s: u32,
    tenb: bool,
    t1md: bool,
    t0_count: u32,
    /// CPU clocks until timer 1 fires (0 = not armed)
    t1_left: i64,
    /// DMA levels to start now (bit n), for the bus to run
    pub start: u8,
    /// the DSP (ports 0x80-0x8C)
    pub dsp: super::scudsp::Dsp,
    asr: [u32; 3],
    rsel: u32,
}

impl Scu {
    pub fn new() -> Self {
        let mut dma: [Dma; 3] = Default::default();
        for d in &mut dma { d.src_add = 4; d.dst_add = 2; d.factor = 7; }
        Self { dma, ims: 0xBFFF, ist: 0, t0c: 0, t1s: 0, tenb: false, t1md: false, t0_count: 0, t1_left: 0,
               start: 0, dsp: super::scudsp::Dsp::new(), asr: [0; 3], rsel: 0 }
    }

    pub fn read(&self, off: u32) -> u32 {
        match off & 0xFC {
            o @ (0x00 | 0x20 | 0x40) => self.dma[o as usize / 0x20].src,
            o @ (0x04 | 0x24 | 0x44) => self.dma[o as usize / 0x20].dst,
            o @ (0x08 | 0x28 | 0x48) => self.dma[o as usize / 0x20].count,
            0x7C => 0,  // DSTA: no DMA in progress (they finish at once)
            0xA0 => self.ims,
            0xA4 => self.ist,
            0xB0 => self.asr[0],
            0xB4 => self.asr[1],
            0xB8 => self.asr[2],
            0xC4 => self.rsel,
            0xC8 => 4,  // version
            _ => 0,
        }
    }

    /// A write of the bytes in `mask` of the 32-bit register at `off`
    pub fn write(&mut self, off: u32, v: u32, mask: u32) {
        let merge = |old: u32| (old & !mask) | (v & mask);
        let o = off & 0xFC;
        match o {
            0x00..=0x57 => {
                let level = o as usize / 0x20;
                let d = &mut self.dma[level];
                match o & 0x1F {
                    0x00 => d.src = merge(d.src) & 0x07FF_FFFF,
                    0x04 => d.dst = merge(d.dst) & 0x07FF_FFFF,
                    0x08 => d.count = merge(d.count) & if level == 0 { 0xF_FFFF } else { 0xFFF },
                    0x0C => {
                        if mask & 0xFF00 != 0 { d.src_add = (v >> 8 & 1) * 4; }
                        if mask & 0xFF != 0 { d.dst_add = match v & 7 { 0 => 0, k => 1 << k }; }
                    }
                    0x10 => {
                        if mask & 0xFF00 != 0 { d.enable = v & 0x100 != 0; }
                        if mask & 0xFF != 0 && v & 1 != 0 && d.enable && d.factor == 7 { self.start |= 1 << level; }
                    }
                    0x14 => {
                        if mask & 0xFF00_0000 != 0 { d.indirect = v & 0x0100_0000 != 0; }
                        if mask & 0xFF_0000 != 0 { d.rup = v & 0x1_0000 != 0; }
                        if mask & 0xFF00 != 0 { d.wup = v & 0x100 != 0; }
                        if mask & 0xFF != 0 { d.factor = (v & 7) as u8; }
                    }
                    _ => {}
                }
            }
            0x90 => self.t0c = merge(self.t0c) & 0x3FF,
            0x94 => self.t1s = merge(self.t1s) & 0x1FF,
            0x98 => {
                if mask & 0xFFFF != 0 {
                    self.tenb = v & 1 != 0;
                    self.t1md = v & 0x100 != 0;
                    if !self.tenb { self.t0_count = 0; self.t1_left = 0; }
                }
            }
            0xA0 => self.ims = merge(self.ims) & 0xFFFF_BFFF,
            0xA4 => self.ist &= v | !mask,
            0xB0 => self.asr[0] = merge(self.asr[0]),
            0xB4 => self.asr[1] = merge(self.asr[1]),
            0xB8 => self.asr[2] = merge(self.asr[2]),
            0xC4 => self.rsel = merge(self.rsel) & 1,
            _ => {}
        }
    }

    /// The interrupt to offer the master SH-2: (level, vector, IST bit)
    pub fn pending(&self) -> Option<(u8, u8, u32)> {
        let live = self.ist & !self.ims;
        if live == 0 { return None; }
        for i in 0..14 {
            if live & 1 << i != 0 { return Some((LEVEL[i], 0x40 + i as u8, 1 << i)); }
        }
        // A-bus interrupts (16-31), all masked by IMS bit 15
        if self.ims & 0x8000 == 0 {
            for i in 16..32 {
                if self.ist & 1 << i != 0 {
                    let level = if i < 20 { 7 } else if i < 24 { 4 } else { 1 };
                    return Some((level, 0x50 + (i - 16) as u8, 1 << i));
                }
            }
        }
        None
    }

    /// Raise an interrupt and start the DMA levels waiting for this event (the bus runs them)
    pub fn raise(&mut self, bit: u32, ev: Option<Event>) {
        self.ist |= bit;
        if let Some(e) = ev { self.event(e); }
    }

    pub fn event(&mut self, e: Event) {
        for (k, d) in self.dma.iter().enumerate() {
            if d.enable && d.factor == e as u8 { self.start |= 1 << k; }
        }
    }

    pub fn vblank_out(&mut self) {
        self.raise(IST_VBLANK_OUT, Some(Event::VBlankOut));
        self.t0_count = 0;
    }

    pub fn hblank_in(&mut self) {
        self.raise(IST_HBLANK_IN, Some(Event::HBlankIn));
        if self.tenb {
            let hit = self.t0_count == self.t0c;
            if hit { self.raise(IST_TIMER0, Some(Event::Timer0)); }
            if hit || !self.t1md { self.t1_left = self.t1s as i64 * 4 + 1; }
        }
        self.t0_count = (self.t0_count + 1) & 0x1FF;
    }

    /// CPU clocks pass: timer 1
    pub fn tick(&mut self, cycles: u32) {
        if self.t1_left > 0 {
            self.t1_left -= cycles as i64;
            if self.t1_left <= 0 {
                self.t1_left = 0;
                self.raise(IST_TIMER1, Some(Event::Timer1));
            }
        }
    }
}

impl Default for Scu {
    fn default() -> Self { Self::new() }
}
