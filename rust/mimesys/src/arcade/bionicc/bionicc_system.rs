//! Capcom's Bionic Commando / Top Secret (1987). Hardware facts from MAME's bionicc driver
//! (BSD-3-Clause, Phil Stroffolino, Paul Leaman, Dirk Best) and its Tiger Road sprite chip
//! (BSD-3-Clause, David Haywood).
//!
//! - Main CPU: 68000 at 12 MHz; IRQ 2 at vblank (line 240), IRQ 4 at line 128 (inputs),
//!   both autovectored and held until taken.
//! - Sound CPU: Z80 at 3.58 MHz with a YM2151; the main CPU only pokes its NMI.
//! - Protection MCU: an i8751 at 6 MHz between the two. A write to E801A raises its INT0
//!   and halts the 68000; the MCU copies data through a window onto the 68000's RAM (odd
//!   bytes of E3E01-FFFFF, with P3.5 low), then lowers P3.0 to let the 68000 go. Sound
//!   commands travel 68000 RAM -> MCU -> P1 latched by P3.6 -> the Z80 at A000; the Z80
//!   answers through the other latch into the MCU's P1.
//! - Video: 6 MHz, 384 x 260, picture lines 16-239.
//!   - text: 8x8, 2 bits, pen 3 see-through, colours 768-831
//!   - background ("scroll 2"): 64 x 64 tiles of 8x8, 4 bits, pen 15 see-through, 0-127
//!   - foreground ("scroll 1"): 64 x 64 tiles of 16x16, 4 bits, 256-319, in two halves
//!     around the sprites (attribute bit 5 picks which pens sit in front); tiles with both
//!     flip bits set go behind the background instead
//!   - 80 buffered sprites of 16x16, 4 bits, pen 15 see-through, 512-767
//!   - 1024 colours, RRRRGGGGBBBBIIII
//!
//! Main map (20 bits): 00000-3FFFF ROM, E0000-E3FFF RAM (4 KB mirrored; sprites at
//!   E0800-E0CFF), E4000 R inputs / W output (bit 0 flip), E4002 R DIPs / W sound NMI,
//!   E8010-E8017 scroll, E8018 W copy sprites, E801A W start the MCU's DMA,
//!   EC000-EFFFF text (4 KB mirrored), F0000-F3FFF foreground, F4000-F7FFF background,
//!   F8000-F87FF W palette, FC000-FFFFF RAM.
//!
//! Older ROM sets (MAME before 0.18x) have no dump of the MCU. For those the driver falls
//! back to MAME 0.139's stand-in: E801A copies the controls (active high) to FFFFA-FFFFF,
//! and a word written to FFFF8 goes straight to the Z80's latch.

use crate::common::audio::ym2151::Ym2151;
use crate::common::cpu::m68000::{M68000, M68kBus};
use crate::common::cpu::mcs51::{Mcs51, Mcs51Bus};
use crate::common::cpu::z80::{Bus, Z80Cpu, Z80State};
use crate::common::input as pad;
use crate::common::system::{DisplayInfo, Rotation, System};
use crate::arcade::gng::gng_system::decode;
use crate::arcade::{state, take_region};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const STATE_VERSION: u32 = 1;

pub const SCREEN_W: usize = 256;
pub const SCREEN_H: usize = 224;
const LINES: usize = 260;
const FIRST_LINE: usize = 16;
/// 384 pixels at 6 MHz = 64 us a line
const MAIN_PER_LINE: i64 = 768;          // 12 MHz
const MCU_PER_LINE: i64 = 32;            // 6 MHz / 12
const SND_CLOCK: u64 = 3_579_545;
const LINE_RATE: u64 = 15_625;
const OUT_RATE: u64 = 44_100;
/// YM2151 busy after a data write: 64 of its clocks (= Z80 cycles here)
const YM_BUSY: u32 = 64;

/// 4-bit RGB plus a brightness nibble: with its top bit clear, the colour is halved and
/// the brightness added (MAME RRRRGGGGBBBBIIII).
fn color(raw: u16) -> [u8; 3] {
    let bright = (raw & 15) as f32;
    let c = |v: u16| -> u8 {
        let x = ((v & 15) * 0x11) as f32;
        if raw & 8 == 0 { (x as u8 / 2) as f32 + bright * 18.15 } else { x }.min(255.0) as u8
    };
    [c(raw >> 12), c(raw >> 8), c(raw >> 4)]
}

#[derive(Serialize, Deserialize)]
pub struct BioniccBoard {
    #[serde(skip)]
    rom: Vec<u8>,
    #[serde(skip)]
    snd_rom: Vec<u8>,
    ram: Vec<u8>,          // E0000-E0FFF
    tx: Vec<u8>,           // EC000-ECFFF
    fg: Vec<u8>,           // F0000-F3FFF
    bg: Vec<u8>,           // F4000-F7FFF
    palette: Vec<u8>,      // F8000-F87FF
    wram: Vec<u8>,         // FC000-FFFFF
    sprite_buf: Vec<u8>,
    scroll: [u16; 4],
    output: u8,
    irq2: bool,
    irq4: bool,
    /// the 68000 waits for the MCU's DMA
    halted: bool,
    snd_nmi: bool,
    snd_ram: Vec<u8>,
    pub ym: Ym2151,
    ym_busy: u32,
    // the MCU's latches and ports
    audio_to_mcu: u8,
    mcu_to_audio: u8,
    mcu_p1: u8,
    mcu_p3: u8,
    mcu_int0: bool,
    /// no MCU dump: MAME 0.139's stand-in
    hle: bool,
    hle_inp: [u16; 3],
    hle_snd: u16,
    #[serde(skip)]
    pub inputs: u16,
    #[serde(skip)]
    pub dsw: u16,
}

impl BioniccBoard {
    fn flip(&self) -> bool { self.output & 1 != 0 }

    fn read8(&mut self, a: u32) -> u8 {
        let a = a & 0xFFFFF;
        match a {
            0x00000..=0x3FFFF => self.rom.get(a as usize).copied().unwrap_or(0xFF),
            0xE0000..=0xE3FFF => self.ram[(a & 0xFFF) as usize],
            0xE4000..=0xE7FFF => {
                let w = if a & 2 == 0 { self.inputs } else { self.dsw };
                if a & 1 == 0 { (w >> 8) as u8 } else { w as u8 }
            }
            0xEC000..=0xEFFFF => self.tx[(a & 0xFFF) as usize],
            0xF0000..=0xF3FFF => self.fg[(a & 0x3FFF) as usize],
            0xF4000..=0xF7FFF => self.bg[(a & 0x3FFF) as usize],
            0xFFFF8..=0xFFFFF if self.hle => {
                let w = if a < 0xFFFFA { self.hle_snd } else { self.hle_inp[((a - 0xFFFFA) >> 1) as usize] };
                if a & 1 == 0 { (w >> 8) as u8 } else { w as u8 }
            }
            0xFC000..=0xFFFFF => self.wram[(a & 0x3FFF) as usize],
            _ => 0xFF,   // the palette reads open bus
        }
    }

    fn write8(&mut self, a: u32, v: u8) {
        let a = a & 0xFFFFF;
        match a {
            0xE0000..=0xE3FFF => self.ram[(a & 0xFFF) as usize] = v,
            0xE4000..=0xE7FFF => match a & 3 {
                0 => self.output = v,
                2 => self.snd_nmi = true,
                _ => {}
            },
            0xE8010..=0xE8017 => {
                let i = ((a - 0xE8010) >> 1) as usize;
                self.scroll[i] = if a & 1 == 0 { (self.scroll[i] & 0xFF) | (v as u16) << 8 } else { (self.scroll[i] & 0xFF00) | v as u16 };
            }
            0xE8018 | 0xE8019 => self.sprite_buf.copy_from_slice(&self.ram[0x800..0xD00]),
            0xE801A | 0xE801B if self.hle => {
                let on = !self.inputs;
                self.hle_inp = [(on >> 12) & 0xF, on & 0x3F, (on >> 6) & 0x3F];
            }
            0xE801A | 0xE801B => { self.mcu_int0 = true; self.halted = true; }
            0xEC000..=0xEFFFF => self.tx[(a & 0xFFF) as usize] = v,
            0xF0000..=0xF3FFF => self.fg[(a & 0x3FFF) as usize] = v,
            0xF4000..=0xF7FFF => self.bg[(a & 0x3FFF) as usize] = v,
            0xF8000..=0xF87FF => self.palette[(a & 0x7FF) as usize] = v,
            0xFFFF8..=0xFFFFF if self.hle => {
                let w = if a < 0xFFFFA { &mut self.hle_snd } else { &mut self.hle_inp[((a - 0xFFFFA) >> 1) as usize] };
                *w = if a & 1 == 0 { (*w & 0xFF) | (v as u16) << 8 } else { (*w & 0xFF00) | v as u16 };
                if a == 0xFFFF9 { self.mcu_to_audio = v; }
            }
            0xFC000..=0xFFFFF => self.wram[(a & 0x3FFF) as usize] = v,
            _ => {}
        }
    }

    fn word(v: &[u8], i: usize) -> u16 { (v[2 * i] as u16) << 8 | v[2 * i + 1] as u16 }

    /// The MCU's window onto the 68000: odd bytes of E3E01-FFFFF, its three upper address
    /// bits (P2.0-2) on A14-A16.
    fn mcu_window(off: u16) -> u32 { 0xE3E01 | ((off as u32 & 0x700) << 6) | ((off as u32 & 0xFF) << 1) }
}

impl M68kBus for BioniccBoard {
    fn read_byte(&mut self, a: u32) -> u8 { self.read8(a) }
    fn read_word(&mut self, a: u32) -> u16 { (self.read8(a) as u16) << 8 | self.read8(a | 1) as u16 }
    fn write_byte(&mut self, a: u32, v: u8) { self.write8(a, v) }
    fn write_word(&mut self, a: u32, v: u16) {
        // the scroll registers and strobes take the whole word
        if (0xE8010..=0xE801B).contains(&(a & 0xFFFFF)) && (a & 0xFFFFF) < 0xE8018 {
            self.scroll[((a & 0xFFFFF) - 0xE8010) as usize >> 1] = v;
            return;
        }
        self.write8(a, (v >> 8) as u8);
        if !(0xE8018..=0xE801B).contains(&(a & 0xFFFFF)) { self.write8(a | 1, v as u8); }
    }
    fn interrupt_ack(&mut self, level: u8) -> Option<u8> {
        if level == 4 { self.irq4 = false; }
        if level == 2 { self.irq2 = false; }
        None
    }
}

struct McuBus<'a>(&'a mut BioniccBoard);

impl Mcs51Bus for McuBus<'_> {
    fn xdata_read(&mut self, a: u16) -> u8 {
        if self.0.mcu_p3 & 0x20 != 0 { return 0xFF; }
        let addr = BioniccBoard::mcu_window(a & 0x7FF);
        self.0.read8(addr)
    }
    fn xdata_write(&mut self, a: u16, v: u8) {
        if self.0.mcu_p3 & 0x20 != 0 { return; }
        let addr = BioniccBoard::mcu_window(a & 0x7FF);
        self.0.write8(addr, v);
    }
    fn port_in(&mut self, n: usize) -> u8 { if n == 1 { self.0.audio_to_mcu } else { 0xFF } }
    fn port_out(&mut self, n: usize, v: u8) {
        let b = &mut *self.0;
        match n {
            1 => b.mcu_p1 = v,
            3 => {
                let old = b.mcu_p3;
                // bit 0 falling: INT0 acknowledged, the 68000 runs again
                if old & 0x01 != 0 && v & 0x01 == 0 { b.mcu_int0 = false; b.halted = false; }
                // bit 6 falling: P1 latched for the Z80
                if old & 0x40 != 0 && v & 0x40 == 0 { b.mcu_to_audio = b.mcu_p1; }
                b.mcu_p3 = v;
            }
            _ => {}
        }
    }
}

struct SoundBus<'a>(&'a mut BioniccBoard);

impl Bus for SoundBus<'_> {
    fn read(&mut self, a: u16) -> u8 {
        let b = &mut *self.0;
        match a {
            0x0000..=0x7FFF => b.snd_rom.get(a as usize).copied().unwrap_or(0xFF),
            0x8000 | 0x8001 => b.ym.status() | if b.ym_busy > 0 { 0x80 } else { 0 },
            0xA000 => b.mcu_to_audio,
            0xC000..=0xC7FF => b.snd_ram[(a & 0x7FF) as usize],
            _ => 0xFF,
        }
    }
    fn write(&mut self, a: u16, v: u8) {
        let b = &mut *self.0;
        match a {
            0x8000 | 0x8001 => {
                b.ym.write(a as u8 & 1, v);
                if a & 1 == 1 { b.ym_busy = YM_BUSY; }
            }
            0xA000 if !b.hle => b.audio_to_mcu = v,
            0xC000..=0xC7FF => b.snd_ram[(a & 0x7FF) as usize] = v,
            _ => {}
        }
    }
    fn port_in(&mut self, _p: u16) -> u8 { 0xFF }
    fn port_out(&mut self, _p: u16, _v: u8) {}
}

pub struct BioniccSystem {
    pub main: M68000,
    pub snd: Z80Cpu,
    pub mcu: Mcs51,
    pub board: BioniccBoard,
    tx_gfx: Vec<u8>,
    bg_gfx: Vec<u8>,
    fg_gfx: Vec<u8>,
    spr_gfx: Vec<u8>,
    main_owed: i64,
    mcu_owed: i64,
    snd_owed: i64,
    snd_phase: u64,
    ym_div: u32,
    ym_out: f32,
    acc: f32,
    acc_n: u32,
    resample: u64,
    out: Vec<f32>,
    pub framebuffer: Vec<u8>,
    current_frame: u64,
    save_filename: String,
    save_path: String,
}

impl BioniccSystem {
    pub fn new(set_name: &str, mut regions: HashMap<String, Vec<u8>>) -> Result<Self, String> {
        let mut rom = take_region(&mut regions, "maincpu")?;
        if rom.len() < 0x40000 { rom.resize(0x40000, 0xFF); }
        let snd_rom = take_region(&mut regions, "audiocpu")?;
        // the MCU's dump is missing from older sets
        let mcu_rom = regions.remove("mcu").filter(|m| m.len() >= 0x1000);
        let hle = mcu_rom.is_none();
        let mcu_rom = mcu_rom.unwrap_or_else(|| vec![0xFF; 0x1000]);
        let gfx1 = take_region(&mut regions, "gfx1")?;
        let gfx2 = take_region(&mut regions, "gfx2")?;
        let gfx3 = take_region(&mut regions, "gfx3")?;
        let spr = take_region(&mut regions, "spritegen")?;
        if gfx1.len() < 0x8000 || gfx2.len() < 0x10000 || gfx3.len() < 0x40000 || spr.len() < 0x40000 {
            return Err("bionicc: graphics regions too small".into());
        }
        let x8 = [0, 1, 2, 3, 8, 9, 10, 11];
        let y8: Vec<usize> = (0..8).map(|y| y * 16).collect();
        let tx_gfx = decode(&gfx1, 2048, &[4, 0], &x8, &y8, 128);
        let half2 = 0x8000 * 8;
        let bg_gfx = decode(&gfx2, 2048, &[half2 + 4, half2, 4, 0], &x8, &y8, 128);
        let half3 = 0x20000 * 8;
        let x16 = [0, 1, 2, 3, 8, 9, 10, 11, 256, 257, 258, 259, 264, 265, 266, 267];
        let y16: Vec<usize> = (0..16).map(|y| y * 16).collect();
        let fg_gfx = decode(&gfx3, 2048, &[half3 + 4, half3, 4, 0], &x16, &y16, 512);
        let sx: Vec<usize> = (0..8).chain(512..520).collect();
        let sy: Vec<usize> = (0..16).map(|y| y * 32).collect();
        let spr_gfx = decode(&spr, 2048, &[0, 8, 16, 24], &sx, &sy, 1024);

        let mut board = BioniccBoard {
            rom,
            snd_rom,
            ram: vec![0; 0x1000],
            tx: vec![0; 0x1000],
            fg: vec![0; 0x4000],
            bg: vec![0; 0x4000],
            palette: vec![0; 0x800],
            wram: vec![0; 0x4000],
            sprite_buf: vec![0; 0x500],
            scroll: [0; 4],
            output: 0,
            irq2: false,
            irq4: false,
            halted: false,
            snd_nmi: false,
            snd_ram: vec![0; 0x800],
            ym: Ym2151::new(),
            ym_busy: 0,
            audio_to_mcu: 0,
            mcu_to_audio: 0,
            mcu_p1: 0,
            mcu_p3: 0xFF,
            mcu_int0: false,
            hle,
            hle_inp: [0; 3],
            hle_snd: 0,
            inputs: 0xFFFF,
            // coins 1C1C, service off, no flip; 3 lives, upright, bonus default, easy (MAME default), no freeze
            dsw: 0xDFFF,
        };
        let mut main = M68000::new();
        main.reset(&mut board);
        Ok(Self {
            main,
            snd: Z80Cpu::new(),
            mcu: Mcs51::new(mcu_rom[..0x1000].to_vec()),
            board,
            tx_gfx,
            bg_gfx,
            fg_gfx,
            spr_gfx,
            main_owed: 0,
            mcu_owed: 0,
            snd_owed: 0,
            snd_phase: 0,
            ym_div: 0,
            ym_out: 0.0,
            acc: 0.0,
            acc_n: 0,
            resample: 0,
            out: Vec::new(),
            framebuffer: vec![0; SCREEN_W * SCREEN_H * 4],
            current_frame: 0,
            save_filename: set_name.to_string(),
            save_path: "user://GD_EMU/Arcade/Capcom/State".to_string(),
        })
    }

    fn run_line(&mut self) {
        // the 68000, unless it's waiting for the MCU
        self.main_owed += MAIN_PER_LINE;
        while self.main_owed > 0 {
            if self.board.halted || self.main.halted { self.main_owed = 0; break; }
            self.main.ipl = if self.board.irq4 { 4 } else if self.board.irq2 { 2 } else { 0 };
            self.main_owed -= self.main.step(&mut self.board) as i64;
        }
        // the MCU
        self.mcu_owed += MCU_PER_LINE;
        if self.board.hle { self.mcu_owed = 0; }
        while self.mcu_owed > 0 {
            self.mcu.set_int(0, self.board.mcu_int0);
            self.mcu_owed -= self.mcu.step(&mut McuBus(&mut self.board)) as i64;
        }
        // the sound CPU and the YM2151
        self.snd_phase += SND_CLOCK;
        self.snd_owed += (self.snd_phase / LINE_RATE) as i64;
        self.snd_phase %= LINE_RATE;
        while self.snd_owed > 0 {
            let mut c = 0;
            if self.board.snd_nmi {
                self.board.snd_nmi = false;
                c = self.snd.nmi(&mut SoundBus(&mut self.board));
            }
            if c == 0 { c = self.snd.step(&mut SoundBus(&mut self.board)); }
            self.snd_owed -= c as i64;
            for _ in 0..c { self.tick_sound(); }
        }
    }

    fn tick_sound(&mut self) {
        let b = &mut self.board;
        b.ym_busy = b.ym_busy.saturating_sub(1);
        self.ym_div += 1;
        if self.ym_div == 64 {
            self.ym_div = 0;
            let (l, r) = b.ym.generate();
            // MAME routes both outputs to mono at 0.60
            self.ym_out = (l + r) as f32 * (0.6 / 32768.0);
        }
        self.acc += self.ym_out;
        self.acc_n += 1;
        self.resample += OUT_RATE;
        if self.resample >= SND_CLOCK {
            self.resample -= SND_CLOCK;
            let s = (self.acc / self.acc_n as f32).clamp(-1.0, 1.0);
            self.out.push(s);
            self.out.push(s);
            self.acc = 0.0;
            self.acc_n = 0;
        }
    }

    pub fn render(&mut self) {
        let b = &self.board;
        const NONE: u16 = 0xFFFF;
        let pal = |i: u16| -> [u8; 3] { color(BioniccBoard::word(&b.palette, i as usize & 0x3FF)) };
        // per pixel: the foreground split in two (behind / in front of the sprites), the
        // background, the sprites
        let mut fg_back = vec![NONE; 256 * 256];
        let mut fg_front = vec![NONE; 256 * 256];
        let mut fg_under = vec![NONE; 256 * 256];   // the "behind the background" tiles
        let mut bgl = vec![NONE; 256 * 256];
        let (fsx, fsy) = (b.scroll[0] as usize, b.scroll[1] as usize);
        let (bsx, bsy) = (b.scroll[2] as usize, b.scroll[3] as usize);
        for y in 0..256 {
            for x in 0..256 {
                // foreground: 64 x 64 tiles of 16x16
                let (tx, ty) = ((x + fsx) & 0x3FF, (y + fsy) & 0x3FF);
                let i = (ty >> 4) * 64 + (tx >> 4);
                let code = b.fg[4 * i + 1] as usize | (b.fg[4 * i + 3] as usize & 7) << 8;
                let attr = b.fg[4 * i + 3];
                let color = 256 + ((attr >> 3) & 3) as u16 * 16;
                let (mut cx, mut cy) = (tx & 15, ty & 15);
                let under = attr & 0xC0 == 0xC0;
                if !under {
                    if attr & 0x80 != 0 { cx = 15 - cx; }
                    if attr & 0x40 != 0 { cy = 15 - cy; }
                }
                let pen = self.fg_gfx[(code * 16 + cy) * 16 + cx] as u16;
                let k = y * 256 + x;
                if under {
                    if pen != 15 { fg_under[k] = color + pen; }
                } else if attr & 0x20 == 0 {
                    // group 0: all of it behind the sprites, pen 15 see-through
                    if pen != 15 { fg_back[k] = color + pen; }
                } else if (1..=5).contains(&pen) {
                    fg_front[k] = color + pen;
                } else if pen != 15 {
                    fg_back[k] = color + pen;
                }
                // background: 64 x 64 tiles of 8x8
                let (tx, ty) = ((x + bsx) & 0x1FF, (y + bsy) & 0x1FF);
                let i = (ty >> 3) * 64 + (tx >> 3);
                let attr = b.bg[4 * i + 3];
                let code = b.bg[4 * i + 1] as usize | (attr as usize & 7) << 8;
                let mut cx = tx & 7;
                let mut cy = ty & 7;
                if attr & 0x80 != 0 { cx = 7 - cx; }
                if attr & 0x40 != 0 { cy = 7 - cy; }
                let pen = self.bg_gfx[(code * 8 + cy) * 8 + cx] as u16;
                if pen != 15 { bgl[k] = ((attr >> 3) & 7) as u16 * 16 + pen; }
            }
        }
        let mut out = vec![NONE; 256 * 256];
        for k in 0..256 * 256 {
            let mut v = fg_under[k];
            if bgl[k] != NONE { v = bgl[k]; }
            if fg_back[k] != NONE { v = fg_back[k]; }
            out[k] = v;
        }
        // sprites: the first in the list on top
        for s in (0..0x500 / 8).rev() {
            let w = |n: usize| BioniccBoard::word(&b.sprite_buf, s * 4 + n);
            let code = (w(0) & 0x7FF) as usize;
            let attr = w(1);
            let sext9 = |v: u16| ((v as i32 & 0x1FF) << 23) >> 23;
            let (y0, x0) = (sext9(w(2)), sext9(w(3)));
            let (fx, fy) = (attr & 2 != 0, attr & 1 != 0);
            let color = 512 + ((attr >> 2) & 15) * 16;
            for y in 0..16 {
                let py = y0 + y as i32;
                if !(0..256).contains(&py) { continue; }
                for x in 0..16 {
                    let px = x0 + x as i32;
                    if !(0..256).contains(&px) { continue; }
                    let p = self.spr_gfx[(code * 16 + if fy { 15 - y } else { y }) * 16 + if fx { 15 - x } else { x }] as u16;
                    if p != 15 { out[py as usize * 256 + px as usize] = color + p; }
                }
            }
        }
        for k in 0..256 * 256 { if fg_front[k] != NONE { out[k] = fg_front[k]; } }
        // text
        for ty in 0..32 {
            for tx in 0..32 {
                let i = ty * 32 + tx;
                let code = b.tx[2 * i + 1] as usize;
                let attr = b.tx[2 * (i + 0x400) + 1];
                let code = code | ((attr as usize & 0xC0) << 2);
                let color = 768 + (attr & 15) as u16 * 4;
                for y in 0..8 {
                    for x in 0..8 {
                        let cx = if attr & 0x10 != 0 { 7 - x } else { x };
                        let cy = if attr & 0x20 != 0 { 7 - y } else { y };
                        let p = self.tx_gfx[(code * 8 + cy) * 8 + cx] as u16;
                        if p != 3 { out[(ty * 8 + y) * 256 + tx * 8 + x] = color + p; }
                    }
                }
            }
        }
        let flip = b.flip();
        for y in 0..SCREEN_H {
            for x in 0..SCREEN_W {
                let (fy, fx) = if flip { (255 - (y + FIRST_LINE), 255 - x) } else { (y + FIRST_LINE, x) };
                let v = out[fy * 256 + fx];
                let c = if v == NONE { [0, 0, 0] } else { pal(v) };
                let o = (y * SCREEN_W + x) * 4;
                self.framebuffer[o..o + 4].copy_from_slice(&[c[0], c[1], c[2], 0xFF]);
            }
        }
    }
}

impl System for BioniccSystem {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        for line in 0..LINES {
            if line == 128 { self.board.irq4 = true; }
            if line == 240 {
                self.render();
                self.board.irq2 = true;
            }
            self.run_line();
        }
        self.current_frame += 1;
    }

    /// Active low, one word: 0x8000 coin 1, 0x4000 coin 2, 0x2000 start 1, 0x1000 start 2,
    /// player 1 in 0x0F00 (up, down, left, right) / 0x0080 fire / 0x0040 bionic arm,
    /// player 2 the same in the low byte (0x20 up ... 0x02 fire, 0x01 arm).
    fn set_input(&mut self, mask: u32) {
        let joy = |m: u32| -> u16 {
            let on = |b: u32| m & b != 0;
            let mut v = 0u16;
            if on(pad::UP) { v |= 0x20; }
            if on(pad::DOWN) { v |= 0x10; }
            if on(pad::LEFT) { v |= 0x08; }
            if on(pad::RIGHT) { v |= 0x04; }
            if on(pad::B) { v |= 0x02; }
            if on(pad::A) { v |= 0x01; }
            v
        };
        let on = |b: u32| mask & b != 0;
        let p1 = joy(mask);
        // player 1's bits sit higher: the joystick at 0x0F00, the buttons at 0x00C0
        let mut v = (p1 & 0x3C) << 6 | (p1 & 3) << 6 | joy(pad::player2(mask));
        if on(pad::START) { v |= 0x2000; }
        if on(pad::START2) { v |= 0x1000; }
        if on(pad::COIN) || on(pad::SELECT) { v |= 0x8000; }
        self.board.inputs = !v;
    }

    fn drain_audio(&mut self) -> Vec<f32> { std::mem::take(&mut self.out) }
    fn sample_rate(&self) -> u32 { OUT_RATE as u32 }
    fn power_on(&mut self) {}
    fn power_off(&mut self) {}
    fn has_battery(&self) -> bool { false }
    fn save_filename(&self) -> &str { &self.save_filename }
    fn save_battery_path(&self) -> &str { &self.save_path }
    fn save_state_path(&self) -> &str { &self.save_path }
    fn is_sram_dirty(&self) -> bool { false }
    fn clear_sram_dirty(&mut self) {}
    fn get_current_frame(&self) -> i64 { self.current_frame as i64 }
    fn get_sram(&self) -> Option<&[u8]> { None }
    fn load_sram(&mut self, _data: &[u8]) {}
    fn framebuffer(&self) -> &[u8] { &self.framebuffer }

    fn reset(&mut self) {
        let b = &mut self.board;
        b.scroll = [0; 4];
        b.irq2 = false;
        b.irq4 = false;
        b.halted = false;
        b.mcu_int0 = false;
        b.mcu_p3 = 0xFF;
        b.ym.reset();
        self.main = M68000::new();
        self.main.reset(&mut self.board);
        self.snd = Z80Cpu::new();
        self.mcu.reset();
    }

    fn frame_time_usec(&self) -> i64 { (384.0 * 260.0 / 6.0) as i64 }

    fn display_info(&self) -> DisplayInfo {
        DisplayInfo {
            buffer_width: SCREEN_W as i32,
            buffer_height: SCREEN_H as i32,
            visible_x: 0,
            visible_y: 0,
            visible_width: SCREEN_W as i32,
            visible_height: SCREEN_H as i32,
            target_aspect_ratio: 4.0 / 3.0,
            rotation: Rotation::None,
        }
    }

    fn save_state(&self) -> Vec<u8> {
        let s = (&self.main, self.snd.save_state(), &self.mcu, &self.board, [self.main_owed, self.mcu_owed, self.snd_owed],
                 self.snd_phase, self.ym_div, self.current_frame);
        state::encode(&self.save_filename, STATE_VERSION, &s)
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let (main, snd, mut mcu, mut board, owed, snd_phase, ym_div, frame): (M68000, Z80State, Mcs51, BioniccBoard, [i64; 3], u64, u32, u64) =
            state::decode(&self.save_filename, STATE_VERSION, data)?;
        if board.ram.len() != 0x1000 || board.tx.len() != 0x1000 || board.fg.len() != 0x4000 || board.bg.len() != 0x4000
            || board.palette.len() != 0x800 || board.wram.len() != 0x4000 || board.sprite_buf.len() != 0x500
            || board.snd_ram.len() != 0x800 || mcu.iram.len() != 256 || mcu.sfr.len() != 128 {
            return Err("save state doesn't fit this board".into());
        }
        board.rom = std::mem::take(&mut self.board.rom);
        board.snd_rom = std::mem::take(&mut self.board.snd_rom);
        board.inputs = self.board.inputs;
        board.dsw = self.board.dsw;
        if board.hle != self.board.hle { return Err("save state is from a set with a different MCU setup".into()); }
        mcu.rom = std::mem::take(&mut self.mcu.rom);
        self.board = board;
        self.main = main;
        self.snd.load_state(&snd);
        self.mcu = mcu;
        [self.main_owed, self.mcu_owed, self.snd_owed] = owed;
        self.snd_phase = snd_phase;
        self.ym_div = ym_div;
        self.current_frame = frame;
        Ok(())
    }
}

impl BioniccSystem {
    #[cfg(test)]
    pub fn sound_bus(&mut self) -> impl Bus + '_ { SoundBus(&mut self.board) }
}
