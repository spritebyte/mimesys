//! Genesis memory maps.
//!
//! 68000:
//!   000000-3FFFFF  cartridge ROM (backup RAM where the header puts it, usually 200001-)
//!   A00000-A0FFFF  Z80 space, while the 68000 holds the Z80 bus
//!   A10000-A1001F  version register, controller data / control ports
//!   A11100         Z80 bus request (bit 8), A11200 Z80 reset (bit 8, 0 = held in reset)
//!   A130F1         backup RAM enable; A130F3-A130FF bank registers (carts over 4 MB)
//!   C00000-C0001F  VDP: data, control, HV counter, PSG at C00011
//!   E00000-FFFFFF  64 KB work RAM (mirrored)
//! Z80:
//!   0000-1FFF RAM (mirrored to 3FFF), 4000-4003 YM2612 (status / address / data), 6000 bank register (one bit a
//!   write), 7F11 PSG, 8000-FFFF a 32 KB window into 68000 space at bank * 0x8000.

use serde::{Deserialize, Serialize};
use crate::common::audio::sn76489::Sn76489;
use crate::common::audio::ym2612::Ym2612;
use crate::common::cpu::m68000::M68kBus;
use crate::common::cpu::z80::Bus as Z80Bus;
use crate::genesis::md_eeprom::Eeprom;
use crate::genesis::md_vdp::Vdp;

/// A 3- or 6-button pad. Buttons as bits: up, down, left, right, A, B, C, start, then
/// X, Y, Z, mode.
///
/// The 6-button pad answers the usual TH high / TH low reads, but counts TH pulses: on
/// the 3rd TH low it reports up and down both pressed (its ID), on the 4th TH high it
/// shows X, Y, Z and mode, on the 4th TH low all ones. 1.5 ms without a TH change resets
/// the count, so a game that reads it like a 3-button pad (once a frame) sees one.
#[derive(Clone, Copy, Default, Serialize, Deserialize)]
pub struct Pad {
    pub buttons: u16,
    pub six_button: bool,
    /// TH highs seen in this sequence
    highs: u8,
    th: bool,
    /// line count at the last TH change
    last_edge: u32,
}

pub const PAD_UP: u16 = 0x01;
pub const PAD_DOWN: u16 = 0x02;
pub const PAD_LEFT: u16 = 0x04;
pub const PAD_RIGHT: u16 = 0x08;
pub const PAD_A: u16 = 0x10;
pub const PAD_B: u16 = 0x20;
pub const PAD_C: u16 = 0x40;
pub const PAD_START: u16 = 0x80;
pub const PAD_X: u16 = 0x100;
pub const PAD_Y: u16 = 0x200;
pub const PAD_Z: u16 = 0x400;
pub const PAD_MODE: u16 = 0x800;
/// lines in 1.5 ms
const PAD_TIMEOUT_LINES: u32 = 24;

impl Pad {
    /// TH idles high: that's the first high of a sequence.
    pub fn new() -> Self { Self { th: true, highs: 1, ..Self::default() } }

    fn timed_out(&self, now: u32) -> bool { now.wrapping_sub(self.last_edge) > PAD_TIMEOUT_LINES }

    fn set_th(&mut self, th: bool, now: u32) {
        if th == self.th { return; }
        // after a timeout the sequence starts over; a TH high that's ending counts as the first
        if self.timed_out(now) { self.highs = self.th as u8; }
        if th { self.highs = self.highs.saturating_add(1); }
        self.th = th;
        self.last_edge = now;
    }

    /// The 7 input pins, given the level of TH (select).
    fn pins(&self, th: bool, now: u32) -> u8 {
        let b = self.buttons;
        let off = |m: u16, bit: u8| if b & m != 0 { 0 } else { bit };
        let step = if !self.six_button || self.timed_out(now) { 1 } else { self.highs };
        match (th, step) {
            (true, 4) => 0x40 | off(PAD_C, 0x20) | off(PAD_B, 0x10) | off(PAD_MODE, 0x08) | off(PAD_X, 0x04)
                | off(PAD_Y, 0x02) | off(PAD_Z, 0x01),
            (true, _) => 0x40 | off(PAD_C, 0x20) | off(PAD_B, 0x10) | off(PAD_RIGHT, 0x08) | off(PAD_LEFT, 0x04)
                | off(PAD_DOWN, 0x02) | off(PAD_UP, 0x01),
            (false, 3) => off(PAD_START, 0x20) | off(PAD_A, 0x10),
            (false, 4) => off(PAD_START, 0x20) | off(PAD_A, 0x10) | 0x0F,
            (false, _) => off(PAD_START, 0x20) | off(PAD_A, 0x10) | off(PAD_DOWN, 0x02) | off(PAD_UP, 0x01),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Io {
    pub version: u8,
    pub data: [u8; 3],
    pub ctrl: [u8; 3],
    #[serde(skip)]
    pub pads: [Pad; 2],
    /// lines since power on (the 6-button pad's timeout)
    #[serde(skip)]
    pub now: u32,
}

impl Io {
    fn th(&self, p: usize) -> bool {
        if self.ctrl[p] & 0x40 != 0 { self.data[p] & 0x40 != 0 } else { true }
    }

    fn read_port(&self, p: usize) -> u8 {
        let (d, c) = (self.data[p], self.ctrl[p]);
        let pins = if p < 2 { self.pads[p].pins(self.th(p), self.now) } else { 0x7F };
        (d & 0x80) | (d & c & 0x7F) | (pins & !c & 0x7F)
    }

    fn read(&self, a: u32) -> u8 {
        match (a & 0x1F) >> 1 {
            0 => self.version,
            1..=3 => self.read_port(((a & 0x1F) >> 1) as usize - 1),
            4..=6 => self.ctrl[((a & 0x1F) >> 1) as usize - 4],
            _ => 0,
        }
    }

    fn write(&mut self, a: u32, v: u8) {
        let p = match (a & 0x1F) >> 1 {
            r @ 1..=3 => { self.data[r as usize - 1] = v; r as usize - 1 }
            r @ 4..=6 => { self.ctrl[r as usize - 4] = v; r as usize - 4 }
            _ => return,
        };
        if p < 2 {
            let (th, now) = (self.th(p), self.now);
            self.pads[p].set_th(th, now);
        }
    }
}

pub struct MdBus {
    pub rom: Vec<u8>,
    pub ram: Vec<u8>,
    pub vdp: Vdp,
    pub io: Io,
    pub psg: Sn76489,
    pub ym: Ym2612,
    pub zram: Vec<u8>,
    pub zbank: u32,
    /// the 68000 asked for the Z80 bus
    pub zbusreq: bool,
    /// the Z80 is held in reset
    pub zreset: bool,
    /// the Z80 was just let out of reset (the system resets the CPU)
    pub zreset_released: bool,
    pub sram: Vec<u8>,
    pub sram_range: Option<(u32, u32)>,
    pub sram_on: bool,
    pub sram_dirty: bool,
    /// serial EEPROM saves (instead of backup RAM)
    pub eeprom: Option<Eeprom>,
    /// 512 KB banks for 080000-3FFFFF (carts over 4 MB)
    pub banks: [u8; 8],
    pub big_cart: bool,
    /// 68000 clocks to stall (DMA)
    pub stall: u32,
}

impl MdBus {
    pub fn new(rom: Vec<u8>, sram_range: Option<(u32, u32)>, version: u8, psg: Sn76489) -> Self {
        let eeprom = Eeprom::detect(&rom);
        let sram_range = if eeprom.is_some() { None } else { sram_range };
        let sram_len = sram_range.map(|(s, e)| ((e | 1) - (s & !1) + 1) as usize).unwrap_or(0);
        let big_cart = rom.len() > 0x400000;
        let sram_on = sram_range.map(|(s, _)| rom.len() as u32 <= s).unwrap_or(false);
        Self {
            rom, ram: vec![0; 0x10000], vdp: Vdp::new(),
            io: Io { version, data: [0x7F; 3], ctrl: [0; 3], pads: [Pad::new(); 2], now: 0 },
            psg, ym: Ym2612::new(), zram: vec![0; 0x2000], zbank: 0, zbusreq: false, zreset: true, zreset_released: false,
            sram: vec![0xFF; sram_len], sram_range, sram_on, sram_dirty: false, eeprom,
            banks: [0, 1, 2, 3, 4, 5, 6, 7], big_cart, stall: 0,
        }
    }

    pub fn z80_running(&self) -> bool { !self.zreset && !self.zbusreq }

    fn sram_index(&self, a: u32) -> Option<usize> {
        let (s, e) = self.sram_range?;
        if self.sram_on && a >= (s & !1) && a <= (e | 1) { Some((a - (s & !1)) as usize) } else { None }
    }

    fn rom_byte(&self, a: u32) -> u8 {
        let a = if self.big_cart && a >= 0x80000 {
            (self.banks[(a >> 19) as usize & 7] as usize) << 19 | (a as usize & 0x7FFFF)
        } else {
            a as usize
        };
        if a < self.rom.len() { return self.rom[a]; }
        // the cart doesn't decode the upper address lines: the ROM repeats
        let mirror = a & (self.rom.len().next_power_of_two() - 1);
        self.rom.get(mirror).copied().unwrap_or(0xFF)
    }

    // ---- Z80 space as the 68000 sees it (byte wide)

    fn z80_space_read(&mut self, a: u16) -> u8 {
        match a {
            0x0000..=0x3FFF => self.zram[(a & 0x1FFF) as usize],
            0x4000..=0x5FFF => self.ym.status(),
            _ => 0xFF,
        }
    }

    fn z80_space_write(&mut self, a: u16, v: u8) {
        match a {
            0x0000..=0x3FFF => self.zram[(a & 0x1FFF) as usize] = v,
            0x6000..=0x60FF => self.zbank = ((self.zbank >> 1) | ((v as u32 & 1) << 8)) & 0x1FF,
            0x4000..=0x5FFF => self.ym.write((a & 3) as u8, v),
            0x7F11 | 0x7F13 | 0x7F15 | 0x7F17 => self.psg.write(v),
            _ => {}
        }
    }

    // ---- the VDP ports

    fn vdp_read(&mut self, a: u32) -> u16 {
        match a & 0x1F {
            0x00..=0x03 => self.vdp.data_read(),
            0x04..=0x07 => self.vdp.status(),
            0x08..=0x0F => self.vdp.hv_counter(),
            _ => 0xFFFF,
        }
    }

    fn vdp_write(&mut self, a: u32, v: u16) {
        match a & 0x1F {
            0x00..=0x03 => self.vdp.data_write(v),
            0x04..=0x07 => {
                self.vdp.control_write(v);
                if let Some((src, words)) = self.vdp.dma_68k.take() { self.run_dma(src, words); }
            }
            _ => {}
        }
    }

    /// 68000 memory -> VRAM / CRAM / VSRAM. Done at once; the 68000 pays for it in stall time.
    fn run_dma(&mut self, mut src: u32, words: u32) {
        for _ in 0..words {
            let w = self.dma_source_word(src);
            self.vdp.dma_word(w);
            src = (src & 0xFE0000) | (src.wrapping_add(2) & 0x1FFFF);
        }
        self.vdp.dma_done(words);
        self.stall += self.vdp.dma_cost(words);
    }

    fn dma_source_word(&self, a: u32) -> u16 {
        let a = a & 0xFFFFFE;
        if a < 0x400000 {
            (self.rom_byte(a) as u16) << 8 | self.rom_byte(a + 1) as u16
        } else if a >= 0xE00000 {
            let i = (a & 0xFFFF) as usize;
            (self.ram[i] as u16) << 8 | self.ram[i + 1] as u16
        } else {
            0
        }
    }

    // ---- control registers at A1xxxx

    fn ctrl_read(&mut self, a: u32) -> u8 {
        match a & 0xFFFF00 {
            0xA11100 => if a & 1 == 0 { if self.zbusreq && !self.zreset { 0x00 } else { 0x01 } } else { 0x00 },
            _ => 0x00,
        }
    }

    fn ctrl_write(&mut self, a: u32, v: u8) {
        match a & 0xFFFF00 {
            0xA11100 if a & 1 == 0 => self.zbusreq = v & 1 != 0,
            0xA11200 if a & 1 == 0 => {
                let reset = v & 1 == 0;
                if self.zreset && !reset { self.zreset_released = true; }
                if reset && !self.zreset { self.ym.reset(); }   // the YM2612 shares the reset line
                self.zreset = reset;
            }
            0xA13000 => match a & 0xFF {
                0xF1 => self.sram_on = v & 1 != 0,
                r @ 0xF3..=0xFF if r & 1 == 1 => self.banks[((r - 0xF1) >> 1) as usize] = v & 0x3F,
                _ => {}
            },
            _ => {}
        }
    }
}

impl M68kBus for MdBus {
    fn read_byte(&mut self, a: u32) -> u8 {
        if let Some(i) = self.sram_index(a) { return self.sram[i]; }
        if let Some(e) = &self.eeprom { if e.reads(a) { return e.read_byte(a); } }
        match a {
            0x000000..=0x3FFFFF => self.rom_byte(a),
            0xA00000..=0xA0FFFF => if self.zbusreq { self.z80_space_read(a as u16) } else { 0xFF },
            0xA10000..=0xA1001F => self.io.read(a),
            0xA10020..=0xA1FFFF => self.ctrl_read(a),
            0xC00000..=0xDFFFFF => {
                let w = self.vdp_read(a);
                if a & 1 == 0 { (w >> 8) as u8 } else { w as u8 }
            }
            0xE00000..=0xFFFFFF => self.ram[(a & 0xFFFF) as usize],
            _ => 0xFF,
        }
    }

    fn read_word(&mut self, a: u32) -> u16 {
        if let Some(e) = &self.eeprom { if e.reads(a) { return e.read_word(); } }
        if self.sram_index(a).is_some() {
            return (self.read_byte(a) as u16) << 8 | self.read_byte(a + 1) as u16;
        }
        match a {
            0x000000..=0x3FFFFF => (self.rom_byte(a) as u16) << 8 | self.rom_byte(a + 1) as u16,
            0xA00000..=0xA0FFFF => { let b = self.read_byte(a) as u16; b << 8 | b }
            0xA10000..=0xA1001F => { let b = self.io.read(a + 1) as u16; b << 8 | b }
            0xA10020..=0xA1FFFF => (self.ctrl_read(a) as u16) << 8 | self.ctrl_read(a + 1) as u16,
            0xC00000..=0xDFFFFF => self.vdp_read(a),
            0xE00000..=0xFFFFFF => {
                let i = (a & 0xFFFF) as usize;
                (self.ram[i] as u16) << 8 | self.ram[i + 1] as u16
            }
            _ => 0xFFFF,
        }
    }

    fn write_byte(&mut self, a: u32, v: u8) {
        if let Some(e) = &mut self.eeprom { if e.writes(a) { e.write_byte(a, v); return; } }
        if let Some(i) = self.sram_index(a) {
            if self.sram[i] != v { self.sram[i] = v; self.sram_dirty = true; }
            return;
        }
        match a {
            0xA00000..=0xA0FFFF => if self.zbusreq { self.z80_space_write(a as u16, v) },
            0xA10000..=0xA1001F => self.io.write(a, v),
            0xA10020..=0xA1FFFF => self.ctrl_write(a, v),
            0xC00000..=0xDFFFFF => match a & 0x1F {
                0x11 | 0x13 | 0x15 | 0x17 => self.psg.write(v),
                0x00..=0x07 => self.vdp_write(a, (v as u16) << 8 | v as u16),
                _ => {}
            },
            0xE00000..=0xFFFFFF => self.ram[(a & 0xFFFF) as usize] = v,
            _ => {}
        }
    }

    fn write_word(&mut self, a: u32, v: u16) {
        if let Some(e) = &mut self.eeprom { if e.writes(a) { e.write_word(v); return; } }
        match a {
            0xA00000..=0xA0FFFF => self.write_byte(a, (v >> 8) as u8),
            0xA10000..=0xA1001F => self.io.write(a + 1, v as u8),
            0xA10020..=0xA1FFFF => self.ctrl_write(a, (v >> 8) as u8),
            0xC00000..=0xDFFFFF => match a & 0x1F {
                0x10..=0x17 => self.psg.write(v as u8),
                _ => self.vdp_write(a, v),
            },
            0xE00000..=0xFFFFFF => {
                let i = (a & 0xFFFF) as usize;
                self.ram[i] = (v >> 8) as u8;
                self.ram[i + 1] = v as u8;
            }
            _ => {
                if self.sram_index(a).is_some() || self.sram_index(a + 1).is_some() {
                    self.write_byte(a, (v >> 8) as u8);
                    self.write_byte(a + 1, v as u8);
                }
            }
        }
    }

    fn interrupt_ack(&mut self, level: u8) -> Option<u8> {
        self.vdp.acknowledge(level);
        None
    }
}

/// The Z80's view of the machine.
pub struct Z80View<'a>(pub &'a mut MdBus);

impl Z80Bus for Z80View<'_> {
    fn read(&mut self, a: u16) -> u8 {
        let b = &mut *self.0;
        match a {
            0x0000..=0x3FFF => b.zram[(a & 0x1FFF) as usize],
            0x4000..=0x5FFF => b.ym.status(),
            0x8000..=0xFFFF => {
                let a68 = (b.zbank << 15) | (a as u32 & 0x7FFF);
                if a68 < 0x400000 { b.rom_byte(a68) }
                else if a68 >= 0xE00000 { b.ram[(a68 & 0xFFFF) as usize] }
                else { 0xFF }
            }
            _ => 0xFF,
        }
    }

    fn write(&mut self, a: u16, v: u8) {
        let b = &mut *self.0;
        match a {
            0x8000..=0xFFFF => {
                let a68 = (b.zbank << 15) | (a as u32 & 0x7FFF);
                if a68 >= 0xE00000 { b.ram[(a68 & 0xFFFF) as usize] = v; }
            }
            _ => b.z80_space_write(a, v),
        }
    }

    fn port_in(&mut self, _p: u16) -> u8 { 0xFF }
    fn port_out(&mut self, _p: u16, _v: u8) {}
}
