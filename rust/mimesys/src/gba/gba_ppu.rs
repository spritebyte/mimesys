//! GBA picture processor: video memory, display registers (0x04000000-0x04000057)
//! and screen timing. The drawing itself is in gba_render.rs (`render_line`).
//!
//! Timing: 1232 system clocks a line, 228 lines (VBlank 160-226, the flag drops on
//! line 227), 280896 clocks a frame (~59.73 Hz). Within a line (as NanoBoyAdvance):
//!   0     VCOUNT steps, HBlank flag clears; line 160: VBlank flag + VBlank DMA
//!   1     VCOUNT match flag; VBlank IRQ        2   VCOUNT IRQ (new match)
//!   1007  HBlank flag + HBlank DMA (visible lines; the line is drawn)
//!   1008  HBlank IRQ

use serde::{Deserialize, Serialize};

pub const SCREEN_W: usize = 240;
pub const SCREEN_H: usize = 160;
pub const CYCLES_PER_LINE: u32 = 1232;
pub const HBLANK_START: u32 = 1007;
const HBLANK_IRQ: u32 = 1008;
pub const LINES: u16 = 228;
pub const CYCLES_PER_FRAME: u32 = CYCLES_PER_LINE * LINES as u32;

/// Events returned by `advance`.
pub const EV_HBLANK: u8 = 1;        // HBlank started (any line)
pub const EV_HBLANK_DMA: u8 = 2;    // HBlank started on a visible line
pub const EV_VBLANK: u8 = 4;        // line 160 started
pub const EV_VCOUNT: u8 = 8;        // VCOUNT matched the DISPSTAT setting
pub const EV_FRAME: u8 = 16;        // a frame was finished (line 160 started)
pub const EV_VBLANK_DMA: u8 = 32;   // VBlank DMA trigger (line 160 started)

#[derive(Serialize, Deserialize)]
pub struct Ppu {
    /// raw display registers 0x00-0x5F (DISPSTAT / VCOUNT are kept separately)
    pub regs: Vec<u8>,
    pub dispstat: u16,
    pub vcount: u16,
    line_cycle: u32,
    pub palette: Vec<u8>,
    pub vram: Vec<u8>,
    pub oam: Vec<u8>,
    /// BG2 / BG3 affine reference point (internal copies, 20.8 fixed point)
    pub bg_ref: [[i32; 2]; 2],
    /// reference point of the current vertical-mosaic block's first line
    pub mosaic_ref: [[i32; 2]; 2],
    /// IRQs due on cycle 1 / 2 of the line
    vblank_irq_due: bool,
    vcount_irq_due: bool,
    #[serde(skip, default = "blank_frame")] pub framebuffer: Vec<u8>,
}

fn blank_frame() -> Vec<u8> { vec![0; SCREEN_W * SCREEN_H * 4] }

impl Default for Ppu { fn default() -> Self { Self::new() } }

impl Ppu {
    pub fn new() -> Self {
        Self {
            regs: vec![0; 0x60],
            dispstat: 0,
            vcount: 0,
            line_cycle: 0,
            palette: vec![0; 0x400],
            vram: vec![0; 0x18000],
            oam: vec![0; 0x400],
            bg_ref: [[0; 2]; 2],
            mosaic_ref: [[0; 2]; 2],
            vblank_irq_due: false,
            vcount_irq_due: false,
            framebuffer: blank_frame(),
        }
    }

    pub fn reg16(&self, off: usize) -> u16 { u16::from_le_bytes([self.regs[off], self.regs[off + 1]]) }
    pub fn dispcnt(&self) -> u16 { self.reg16(0) }

    /// Byte read of a display register.
    pub fn read8(&self, off: u32) -> u8 {
        match off {
            4 => self.dispstat as u8,
            5 => (self.dispstat >> 8) as u8,
            6 => self.vcount as u8,
            7 => 0,
            // readable: DISPCNT, green swap, BGxCNT, WININ/WINOUT, BLDCNT/BLDALPHA
            0x09 | 0x0B => self.regs[off as usize] & 0xDF,      // BG0/1: no wraparound bit
            0x48..=0x4B => self.regs[off as usize] & 0x3F,
            0x51 => self.regs[off as usize] & 0x3F,
            0x52 | 0x53 => self.regs[off as usize] & 0x1F,
            0..=3 | 0x08..=0x0F | 0x50 => self.regs[off as usize],
            _ => 0,
        }
    }

    /// Byte write of a display register.
    pub fn write8(&mut self, off: u32, v: u8) {
        match off {
            4 => self.dispstat = (self.dispstat & 0xFF07) | (v as u16 & 0xF8),
            5 => self.dispstat = (self.dispstat & 0x00FF) | ((v as u16) << 8),
            6 | 7 => {}
            o if (o as usize) < self.regs.len() => {
                self.regs[o as usize] = v;
                // writing a reference point reloads the internal copy
                if (0x28..0x30).contains(&o) { self.latch_ref(0); }
                if (0x38..0x40).contains(&o) { self.latch_ref(1); }
            }
            _ => {}
        }
    }

    /// Internal BG2/BG3 reference point = the 28-bit signed X / Y registers.
    pub fn latch_ref(&mut self, i: usize) {
        let base = 0x28 + i * 0x10;
        for k in 0..2 {
            let o = base + k * 4;
            let v = u32::from_le_bytes([self.regs[o], self.regs[o + 1], self.regs[o + 2], self.regs[o + 3]]);
            self.bg_ref[i][k] = ((v << 4) as i32) >> 4;
        }
    }

    fn set_flag(&mut self, bit: u16, on: bool) {
        if on { self.dispstat |= bit } else { self.dispstat &= !bit }
    }

    /// Clocks until the next timing event.
    pub fn cycles_to_event(&self) -> u32 {
        let lc = self.line_cycle;
        [1, 2, HBLANK_START, HBLANK_IRQ, CYCLES_PER_LINE].iter().find(|&&p| p > lc).unwrap() - lc
    }

    /// Runs the timing for `cycles` clocks; returns the events that happened (EV_*).
    pub fn advance(&mut self, mut cycles: u32) -> u8 {
        let mut ev = 0;
        while cycles > 0 {
            let step = cycles.min(self.cycles_to_event());
            self.line_cycle += step;
            cycles -= step;
            match self.line_cycle {
                1 => {
                    if self.vblank_irq_due { self.vblank_irq_due = false; ev |= EV_VBLANK; }
                    let hit = self.vcount == self.dispstat >> 8;
                    if hit && self.dispstat & 4 == 0 { self.vcount_irq_due = true; }
                    self.set_flag(4, hit);
                }
                2 => if self.vcount_irq_due { self.vcount_irq_due = false; ev |= EV_VCOUNT; },
                HBLANK_START => {
                    self.set_flag(2, true);
                    if self.vcount < SCREEN_H as u16 {
                        self.render_line(self.vcount as usize);
                        ev |= EV_HBLANK_DMA;
                    }
                }
                HBLANK_IRQ => ev |= EV_HBLANK,
                CYCLES_PER_LINE => {
                    self.line_cycle = 0;
                    self.set_flag(2, false);
                    self.vcount = (self.vcount + 1) % LINES;
                    match self.vcount {
                        160 => {
                            self.set_flag(1, true);
                            ev |= EV_VBLANK_DMA | EV_FRAME;
                            self.vblank_irq_due = true;
                            self.latch_ref(0);
                            self.latch_ref(1);
                        }
                        227 => self.set_flag(1, false),
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        ev
    }

    /// IF bits (0 VBlank, 1 HBlank, 2 VCount) for events, by the DISPSTAT enables.
    pub fn irq_bits(&self, ev: u8) -> u16 {
        let mut bits = 0;
        if ev & EV_VBLANK != 0 && self.dispstat & 0x08 != 0 { bits |= 1; }
        if ev & EV_HBLANK != 0 && self.dispstat & 0x10 != 0 { bits |= 2; }
        if ev & EV_VCOUNT != 0 && self.dispstat & 0x20 != 0 { bits |= 4; }
        bits
    }
}

/// BGR555 to RGBA8.
pub fn rgb555(c: u16) -> [u8; 4] {
    let x = |v: u16| { let v = (v & 31) as u8; (v << 3) | (v >> 2) };
    [x(c), x(c >> 5), x(c >> 10), 255]
}
