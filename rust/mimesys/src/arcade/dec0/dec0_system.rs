//! Data East "dec0": Robocop (1988). Hardware facts and maps from MAME's dec0 driver
//! (`dataeast/dec0.cpp`, `dec0_m.cpp`, `dec0_v.cpp`; BSD-3-Clause, Bryan McPhail and others)
//! and its MB8421 dual-port RAM (`machine/mb8421.cpp`, BSD-3-Clause, hap, AJR).
//!
//! - Main CPU: 68000 at 10 MHz. VBlank is IRQ 6, held until a write to 30C018. IRQ 4 comes
//!   from the dual-port RAM (the HuC6280 wrote its mailbox).
//! - Sound CPU: 65C02 at 1.5 MHz. A sound-latch write (30C014) is its NMI; the YM2203 /
//!   YM3812 drive its IRQ.
//! - Protection CPU (Robocop's "DEM-01" board): a HuC6280 at 21.477 MHz / 16 running a
//!   512-byte PROM (at 0x1E00, so the reset vector sits in it), with 8 KB of RAM at
//!   1F0000 and the MB8421 at 1F2000.
//! - MB8421 (2 KB): the 68000 sees it on the low bytes of 180000-180FFF. A write to the
//!   last byte (7FF) from the 68000 interrupts the HuC6280 until it reads 7FF; a write to
//!   7FE from the HuC6280 raises the 68000's IRQ 4 until the 68000 reads 7FE.
//! - Screen: 6 MHz pixel clock, 384 x 272, picture 256 x 240 (lines 8-247), ~57.4 Hz.
//!
//! Main map (24 bits): 000000-05FFFF ROM; 180000-180FFF dual-port RAM; 240000-24D7FF the
//!   three BAC06 tilemap chips (control, scroll, column / row scroll, VRAM) and RAM at
//!   242800-243FFF; 300000 / 300008 rotary (unused); 30C000 inputs, 30C002 system,
//!   30C004 DIPs; 30C010-30C01F writes: +0 priority, +2 sprite DMA, +4 sound latch,
//!   +8 VBlank ack; 310000 / 314000 palette (RG / B); 318000-31BFFF = FF8000-FFBFFF RAM;
//!   31C000-31C7FF = FFC000-FFC7FF sprite RAM.
//! Sound map: 0000-07FF RAM, 0800 YM2203, 1000 YM3812, 3000 latch, 3800 OKI, 8000- ROM.

use crate::common::audio::okim6295::Okim6295;
use crate::common::audio::ym2203::Ym2203;
use crate::common::audio::ym3812::Ym3812;
use crate::common::cpu::m65c02::{Bus65, Cpu65c02, Variant};
use crate::common::cpu::m68000::{M68000, M68kBus};
use crate::common::input as pad;
use crate::common::system::{DisplayInfo, Rotation, System};
use crate::pce::huc6280::{Huc6280, PceBus, IRQ1, IRQ2, TIMER};
use crate::arcade::{state, take_region};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const STATE_VERSION: u32 = 1;

pub const SCREEN_W: usize = 256;
pub const SCREEN_H: usize = 240;
const LINES: usize = 272;
const FIRST_LINE: usize = 8;
const VBLANK_LINE: usize = 248;
/// 384 dots at 6 MHz: 15625 lines a second
const LINE_RATE: u64 = 15_625;
const MAIN_CLOCK: u64 = 10_000_000;
const SND_CLOCK: u64 = 1_500_000;
const SUB_CLOCK: u64 = 21_477_272 / 16;
const OKI_CLOCK: f64 = 1_000_000.0;
/// MAME's 0.32 for the YM3812, scaled like the rest of the mix
const OPL_MIX: f32 = 0.16;
const OUT_RATE: u32 = 44_100;

/// One BAC06 tilemap chip's memory (stage 2 draws them)
#[derive(Serialize, Deserialize, Clone)]
pub struct Bac06 {
    pub ctrl: [u16; 4],
    pub scroll: [u16; 4],
    pub colscroll: Vec<u8>,
    pub rowscroll: Vec<u8>,
    pub vram: Vec<u8>,
}

impl Bac06 {
    fn new(vram: usize) -> Self {
        Self { ctrl: [0; 4], scroll: [0; 4], colscroll: vec![0; 0x80], rowscroll: vec![0; 0x400], vram: vec![0; vram] }
    }
}

/// The HuC6280's own timer and interrupt controller (as on the PC Engine)
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct H6280Io {
    timer_reload: u8,
    timer_counter: u8,
    timer_enabled: bool,
    timer_sub: u32,
    timer_irq: bool,
    irq_disable: u8,
}

impl H6280Io {
    /// `clocks` of the CPU's input clock pass (the timer counts every 1024)
    fn tick(&mut self, clocks: u32) {
        if !self.timer_enabled { return; }
        let mut c = clocks;
        while c > 0 {
            let step = c.min(self.timer_sub.max(1));
            self.timer_sub = self.timer_sub.saturating_sub(step);
            c -= step;
            if self.timer_sub == 0 {
                self.timer_sub = 1024;
                if self.timer_counter == 0 {
                    self.timer_counter = self.timer_reload;
                    self.timer_irq = true;
                } else {
                    self.timer_counter -= 1;
                }
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Dec0Board {
    #[serde(skip)]
    rom: Vec<u8>,
    #[serde(skip)]
    snd_rom: Vec<u8>,
    #[serde(skip)]
    sub_rom: Vec<u8>,
    pub ram: Vec<u8>,          // FF8000-FFBFFF
    pub spriteram: Vec<u8>,    // FFC000-FFC7FF
    pub sprite_buf: Vec<u8>,
    pub palette: Vec<u8>,      // 310000-3107FF: R / G
    pub palette_ext: Vec<u8>,  // 314000-3147FF: B
    pub extra_ram: Vec<u8>,    // 242800-243FFF
    pub bac: Vec<Bac06>,
    pub pri: u16,
    irq6: bool,
    // the dual-port RAM and its mailboxes
    pub dpram: Vec<u8>,
    intl: bool,
    intr: bool,
    has_sub: bool,
    sub_ram: Vec<u8>,
    h6280: H6280Io,
    // sound
    latch: u8,
    snd_nmi: bool,
    snd_ram: Vec<u8>,
    pub ym1: Ym2203,
    pub ym2: Ym3812,
    pub oki: Okim6295,
    #[serde(skip)]
    pub inputs: u16,
    #[serde(skip)]
    pub system: u16,
    #[serde(skip)]
    pub dsw: u16,
    #[serde(skip)]
    vblank: bool,
    /// debug counters
    #[serde(skip)]
    pub latches: u32,
    #[serde(skip)]
    pub mail: [u32; 2],
}


impl Dec0Board {
    pub fn word(v: &[u8], i: usize) -> u16 { (v[2 * i] as u16) << 8 | v[2 * i + 1] as u16 }

    /// The BAC06 at `a`, if any: (chip, area, offset). Areas: 0 control, 1 scroll,
    /// 2 column scroll, 3 row scroll, 4 VRAM.
    fn bac_area(a: u32) -> Option<(usize, u8, usize)> {
        let o = |base: u32| (a - base) as usize;
        Some(match a {
            0x240000..=0x240007 => (0, 0, o(0x240000)),
            0x240010..=0x240017 => (0, 1, o(0x240010)),
            0x242000..=0x24207F => (0, 2, o(0x242000)),
            0x242400..=0x2427FF => (0, 3, o(0x242400)),
            0x244000..=0x245FFF => (0, 4, o(0x244000)),
            0x246000..=0x246007 => (1, 0, o(0x246000)),
            0x246010..=0x246017 => (1, 1, o(0x246010)),
            0x248000..=0x24807F => (1, 2, o(0x248000)),
            0x248400..=0x2487FF => (1, 3, o(0x248400)),
            0x24A000..=0x24A7FF => (1, 4, o(0x24A000)),
            0x24C000..=0x24C007 => (2, 0, o(0x24C000)),
            0x24C010..=0x24C017 => (2, 1, o(0x24C010)),
            0x24C800..=0x24C87F => (2, 2, o(0x24C800)),
            0x24CC00..=0x24CFFF => (2, 3, o(0x24CC00)),
            0x24D000..=0x24D7FF => (2, 4, o(0x24D000)),
            _ => return None,
        })
    }

    fn read8(&mut self, a: u32) -> u8 {
        let a = a & 0xFF_FFFF;
        match a {
            0x000000..=0x05FFFF => self.rom.get(a as usize).copied().unwrap_or(0xFF),
            0x180000..=0x180FFF => {
                if a & 1 == 0 { return 0xFF; }
                let i = ((a & 0xFFF) >> 1) as usize;
                if i == 0x7FE { self.intl = false; }
                self.dpram[i]
            }
            0x242800..=0x243FFF => self.extra_ram[(a - 0x242800) as usize],
            0x240000..=0x24D7FF => match Self::bac_area(a) {
                Some((n, 2, o)) => self.bac[n].colscroll[o],
                Some((n, 3, o)) => self.bac[n].rowscroll[o],
                Some((n, 4, o)) => self.bac[n].vram[o],
                _ => 0xFF,
            },
            0x300000..=0x30000F => 0xFF,   // rotary joysticks: not on Robocop
            0x30C000..=0x30C00F => {
                let w = match a & 0xE {
                    0 => self.inputs,
                    2 => (self.system & 0x7F) | if self.vblank { 0x80 } else { 0 },
                    4 => self.dsw,
                    _ => if a & 0xE == 8 { 0 } else { 0xFFFF },
                };
                if a & 1 == 0 { (w >> 8) as u8 } else { w as u8 }
            }
            0x310000..=0x3107FF => self.palette[(a & 0x7FF) as usize],
            0x314000..=0x3147FF => self.palette_ext[(a & 0x7FF) as usize],
            0x318000..=0x31BFFF | 0xFF8000..=0xFFBFFF => self.ram[(a & 0x3FFF) as usize],
            0x31C000..=0x31C7FF | 0xFFC000..=0xFFC7FF => self.spriteram[(a & 0x7FF) as usize],
            _ => 0xFF,
        }
    }

    fn write8(&mut self, a: u32, v: u8) {
        let a = a & 0xFF_FFFF;
        match a {
            0x180000..=0x180FFF => {
                if a & 1 == 0 { return; }
                let i = ((a & 0xFFF) >> 1) as usize;
                self.dpram[i] = v;
                if i == 0x7FF { self.intr = true; self.mail[0] += 1; }
            }
            0x242800..=0x243FFF => self.extra_ram[(a - 0x242800) as usize] = v,
            0x240000..=0x24D7FF => match Self::bac_area(a) {
                Some((n, k @ (0 | 1), o)) => {
                    let r = if k == 0 { &mut self.bac[n].ctrl } else { &mut self.bac[n].scroll };
                    let w = &mut r[o >> 1];
                    *w = if o & 1 == 0 { (*w & 0xFF) | (v as u16) << 8 } else { (*w & 0xFF00) | v as u16 };
                }
                Some((n, 2, o)) => self.bac[n].colscroll[o] = v,
                Some((n, 3, o)) => self.bac[n].rowscroll[o] = v,
                Some((n, 4, o)) => self.bac[n].vram[o] = v,
                _ => {}
            },
            0x30C010..=0x30C01F => self.control(a, v as u16, a & 1 == 1),
            0x310000..=0x3107FF => self.palette[(a & 0x7FF) as usize] = v,
            0x314000..=0x3147FF => self.palette_ext[(a & 0x7FF) as usize] = v,
            0x318000..=0x31BFFF | 0xFF8000..=0xFFBFFF => self.ram[(a & 0x3FFF) as usize] = v,
            0x31C000..=0x31C7FF | 0xFFC000..=0xFFC7FF => self.spriteram[(a & 0x7FF) as usize] = v,
            _ => {}
        }
    }

    /// 30C010-30C01F: `low` = the write reached the low byte
    fn control(&mut self, a: u32, v: u16, low: bool) {
        match a & 0xE {
            0x0 => self.pri = v,
            0x2 => self.sprite_buf.copy_from_slice(&self.spriteram),
            0x4 => if low { self.latch = v as u8; self.snd_nmi = true; self.latches += 1; },
            0x8 => self.irq6 = false,
            _ => {}   // 6 / E: the i8751 of other dec0 games
        }
    }

    fn sub_read(&mut self, phys: u32) -> u8 {
        match phys {
            0x000000..=0x00FFFF => self.sub_rom.get(phys as usize).copied().unwrap_or(0xFF),
            0x1F0000..=0x1F1FFF => self.sub_ram[(phys & 0x1FFF) as usize],
            0x1F2000..=0x1F27FF => {
                let i = (phys & 0x7FF) as usize;
                if i == 0x7FF { self.intr = false; }
                self.dpram[i]
            }
            0x1FEC00..=0x1FEFFF => self.h6280.timer_counter,
            0x1FF400..=0x1FF7FF => match phys & 3 {
                2 => self.h6280.irq_disable,
                3 => (if self.intr { IRQ1 } else { 0 }) | (if self.h6280.timer_irq { TIMER } else { 0 }),
                _ => 0,
            },
            _ => 0xFF,
        }
    }

    fn sub_write(&mut self, phys: u32, v: u8) {
        match phys {
            0x1F0000..=0x1F1FFF => self.sub_ram[(phys & 0x1FFF) as usize] = v,
            0x1F2000..=0x1F27FF => {
                let i = (phys & 0x7FF) as usize;
                self.dpram[i] = v;
                if i == 0x7FE { self.intl = true; self.mail[1] += 1; }
            }
            0x1FEC00..=0x1FEFFF => {
                let t = &mut self.h6280;
                if phys & 1 == 0 {
                    t.timer_reload = v & 0x7F;
                } else {
                    let on = v & 1 != 0;
                    if on && !t.timer_enabled { t.timer_counter = t.timer_reload; t.timer_sub = 1024; }
                    t.timer_enabled = on;
                }
            }
            0x1FF400..=0x1FF7FF => match phys & 3 {
                2 => self.h6280.irq_disable = v & 7,
                3 => self.h6280.timer_irq = false,
                _ => {}
            },
            _ => {}
        }
    }
}

impl Dec0Board {
    #[cfg(test)]
    pub fn latch_ram(&self, a: usize) -> u8 { self.snd_ram[a] }
}

impl M68kBus for Dec0Board {
    fn read_byte(&mut self, a: u32) -> u8 { self.read8(a) }
    fn read_word(&mut self, a: u32) -> u16 { (self.read8(a) as u16) << 8 | self.read8(a | 1) as u16 }
    fn write_byte(&mut self, a: u32, v: u8) { self.write8(a, v) }
    fn write_word(&mut self, a: u32, v: u16) {
        if (0x30C010..=0x30C01F).contains(&(a & 0xFF_FFFF)) { return self.control(a & 0xFF_FFFF, v, true); }
        self.write8(a, (v >> 8) as u8);
        self.write8(a | 1, v as u8);
    }
}

struct SubBus<'a>(&'a mut Dec0Board);

impl PceBus for SubBus<'_> {
    fn read(&mut self, phys: u32) -> u8 { self.0.sub_read(phys & 0x1F_FFFF) }
    fn write(&mut self, phys: u32, v: u8) { self.0.sub_write(phys & 0x1F_FFFF, v) }
    fn irq_pending(&mut self) -> u8 {
        let b = &*self.0;
        let on = (if b.intr { IRQ1 } else { 0 }) | (if b.h6280.timer_irq { TIMER } else { 0 });
        on & !(b.h6280.irq_disable & (IRQ1 | IRQ2 | TIMER))
    }
}

struct SoundBus<'a>(&'a mut Dec0Board);

impl Bus65 for SoundBus<'_> {
    fn read(&mut self, a: u16) -> u8 {
        let b = &mut *self.0;
        match a {
            0x0000..=0x07FF => b.snd_ram[a as usize],
            0x0800..=0x0801 => b.ym1.read(a as u8 & 1, 0xFF, 0xFF),
            0x1000..=0x1001 => b.ym2.read(a as u8 & 1),
            0x3000 => b.latch,
            0x3800 => b.oki.read(),
            0x8000..=0xFFFF => b.snd_rom.get(a as usize).copied().unwrap_or(0xFF),
            _ => 0xFF,
        }
    }
    fn write(&mut self, a: u16, v: u8) {
        let b = &mut *self.0;
        match a {
            0x0000..=0x07FF => b.snd_ram[a as usize] = v,
            0x0800..=0x0801 => b.ym1.write(a as u8 & 1, v),
            0x1000..=0x1001 => b.ym2.write(a as u8 & 1, v),
            0x3800 => b.oki.write(v),
            _ => {}
        }
    }
}

pub struct Dec0System {
    pub main: M68000,
    pub snd: Cpu65c02,
    pub sub: Huc6280,
    pub board: Dec0Board,
    main_phase: u64,
    snd_phase: u64,
    sub_phase: u64,
    main_owed: i64,
    snd_owed: i64,
    sub_owed: i64,
    oki_acc: f64,
    oki_last: f32,
    opl_q: std::collections::VecDeque<f32>,
    opl_last: f32,
    pub(crate) out: Vec<f32>,
    pub framebuffer: Vec<u8>,
    pub current_frame: u64,
    debug: bool,
    save_filename: String,
    save_path: String,
    pub gfx: super::dec0_video::Gfx,
    pixels: Vec<u16>,
}

impl Dec0System {
    pub fn new(set_name: &str, mut regions: HashMap<String, Vec<u8>>) -> Result<Self, String> {
        let mut rom = take_region(&mut regions, "maincpu")?;
        if rom.len() < 0x60000 { rom.resize(0x60000, 0xFF); }
        let mut snd_rom = take_region(&mut regions, "audiocpu")?;
        if snd_rom.len() < 0x10000 { snd_rom.resize(0x10000, 0xFF); }
        // the bootleg has no HuC6280: its 68000 code doesn't need it
        let sub_rom = regions.remove("sub");
        let has_sub = sub_rom.is_some();
        let mut sub_rom = sub_rom.unwrap_or_default();
        sub_rom.truncate(0x10000);
        if sub_rom.len() < 0x10000 { sub_rom.resize(0x10000, 0xFF); }
        let oki_rom = regions.remove("oki").unwrap_or_default();
        let (c, t1, t2, sp) = (take_region(&mut regions, "char")?, take_region(&mut regions, "tiles1")?,
                               take_region(&mut regions, "tiles2")?, take_region(&mut regions, "sprites")?);
        let gfx = super::dec0_video::Gfx::new(&c, &t1, &t2, &sp);
        let mut ym1 = Ym2203::new(SND_CLOCK as u32, OUT_RATE);
        // MAME: SSG 0.81 each, FM 0.32 (scaled down with the rest of the mix)
        ym1.set_mix(0.40, 0.16);
        let mut board = Dec0Board {
            rom,
            snd_rom,
            sub_rom,
            ram: vec![0; 0x4000],
            spriteram: vec![0; 0x800],
            sprite_buf: vec![0; 0x800],
            palette: vec![0; 0x800],
            palette_ext: vec![0; 0x800],
            extra_ram: vec![0; 0x1800],
            bac: vec![Bac06::new(0x2000), Bac06::new(0x800), Bac06::new(0x800)],
            pri: 0,
            irq6: false,
            dpram: vec![0; 0x800],
            intl: false,
            intr: false,
            has_sub,
            sub_ram: vec![0; 0x2000],
            h6280: H6280Io::default(),
            latch: 0,
            snd_nmi: false,
            snd_ram: vec![0; 0x800],
            ym1,
            ym2: Ym3812::new(OUT_RATE).with_clock(SND_CLOCK as u32 * 2),
            oki: Okim6295::new(oki_rom, true),
            inputs: 0xFFFF,
            system: 0xFF7F,
            dsw: 0xFF7F,
            vblank: false,
            latches: 0,
            mail: [0; 2],
        };
        let mut main = M68000::new();
        main.reset(&mut board);
        let mut snd = Cpu65c02::new(Variant::Sc02);
        snd.reset(&mut SoundBus(&mut board));
        let mut sub = Huc6280::new();
        if has_sub { sub.reset(&mut SubBus(&mut board)); }
        Ok(Self {
            main, snd, sub, board,
            main_phase: 0, snd_phase: 0, sub_phase: 0,
            main_owed: 0, snd_owed: 0, sub_owed: 0,
            oki_acc: 0.0, oki_last: 0.0, opl_q: Default::default(), opl_last: 0.0,
            out: Vec::new(),
            framebuffer: vec![0; SCREEN_W * SCREEN_H * 4],
            current_frame: 0,
            debug: false,
            save_filename: set_name.to_string(),
            save_path: "user://GD_EMU/Arcade/DataEast/State".to_string(),
            gfx,
            pixels: vec![0; SCREEN_W * SCREEN_H],
        })
    }

    /// Clocks of a CPU running at `hz` for one line, with the remainder carried
    fn share(phase: &mut u64, hz: u64) -> i64 {
        *phase += hz;
        let n = *phase / LINE_RATE;
        *phase %= LINE_RATE;
        n as i64
    }

    fn run_line(&mut self) {
        // main CPU
        self.main_owed += Self::share(&mut self.main_phase, MAIN_CLOCK);
        while self.main_owed > 0 {
            if self.main.halted { self.main_owed = 0; break; }
            let b = &self.board;
            self.main.ipl = if b.irq6 { 6 } else if b.intl { 4 } else { 0 };
            self.main_owed -= self.main.step(&mut self.board) as i64;
        }
        // protection CPU
        self.sub_owed += Self::share(&mut self.sub_phase, SUB_CLOCK);
        if !self.board.has_sub { self.sub_owed = 0; }
        while self.sub_owed > 0 {
            let c = self.sub.step(&mut SubBus(&mut self.board));
            self.board.h6280.tick(c);
            self.sub_owed -= c as i64;
        }
        // sound CPU, the YM2203 and the OKI
        self.snd_owed += Self::share(&mut self.snd_phase, SND_CLOCK);
        let mut snd_clocks = 0u32;
        while self.snd_owed > 0 {
            let nmi = std::mem::take(&mut self.board.snd_nmi);
            let irq = self.board.ym1.irq() || self.board.ym2.irq();
            let mut c = self.snd.interrupt(&mut SoundBus(&mut self.board), irq, nmi);
            if c == 0 { c = self.snd.step(&mut SoundBus(&mut self.board)); }
            self.snd_owed -= c as i64;
            snd_clocks += c;
        }
        self.board.ym1.run(snd_clocks);
        self.board.ym2.run(snd_clocks * 2);   // 3 MHz = twice the 65C02's clock
        self.mix();
        // don't let a backlog build up
        while self.opl_q.len() > 64 { self.opl_q.pop_front(); }
    }

    /// The YM2203's samples (at the output rate) with the OKI's added
    fn mix(&mut self) {
        let oki_rate = self.board.oki.sample_rate(OKI_CLOCK);
        let ym = std::mem::take(&mut self.board.ym1.out);
        let opl = std::mem::take(&mut self.board.ym2.out);
        self.opl_q.extend(opl);
        for s in ym {
            // the two chips' streams drift by a sample now and then: hold the OPL's last
            if let Some(o) = self.opl_q.pop_front() { self.opl_last = o; }
            self.oki_acc += oki_rate;
            while self.oki_acc >= OUT_RATE as f64 {
                self.oki_acc -= OUT_RATE as f64;
                self.oki_last = self.board.oki.sample();
            }
            let v = (s + self.opl_last * OPL_MIX + self.oki_last * 0.33).clamp(-1.0, 1.0);
            self.out.push(v);
            self.out.push(v);
        }
    }

    pub fn render(&mut self) {
        let mut px = std::mem::take(&mut self.pixels);
        super::dec0_video::render(&self.board, &self.gfx, self.current_frame, &mut px);
        let lut: Vec<[u8; 3]> = (0..1024).map(|i| self.color(i)).collect();
        for (p, &i) in self.framebuffer.chunks_exact_mut(4).zip(px.iter()) {
            let c = lut[i as usize & 0x3FF];
            p.copy_from_slice(&[c[0], c[1], c[2], 0xFF]);
        }
        self.pixels = px;
    }

    /// Palette entry: xBGR 888 over two RAMs (low byte R, high byte G in the first; B in the second)
    pub fn color(&self, i: usize) -> [u8; 3] {
        let i = i & 0x3FF;
        let rg = Dec0Board::word(&self.board.palette, i);
        let b = Dec0Board::word(&self.board.palette_ext, i);
        [rg as u8, (rg >> 8) as u8, b as u8]
    }

    fn report(&self) {
        let b = &self.board;
        crate::dlog!("dec0: frame {} | 68000 pc {:06X} sr {:04X} ipl {} | HuC6280 pc {:04X} mpr {:02X?} | 65C02 pc {:04X} | irq6 {} intl {} intr {} | mail 68k->sub {} sub->68k {} | latches {} | pri {:04X}",
            self.current_frame, self.main.instruction_pc(), self.main.sr, self.main.ipl, self.sub.pc, self.sub.mpr, self.snd.pc,
            b.irq6, b.intl, b.intr, b.mail[0], b.mail[1], b.latches, b.pri);
        for (n, t) in b.bac.iter().enumerate() {
            crate::dlog!("dec0:   BAC06 #{n} ctrl {:04X?} scroll {:04X?}", t.ctrl, t.scroll);
        }
        let (keys, mode) = b.ym2.keys();
        let peak = self.out.iter().fold(0f32, |m, v| m.max(v.abs()));
        crate::dlog!("dec0:   sound: YM2203 irq {} | YM3812 status {:02X} irq {} mode {:02X} keys {:04X} | out peak {:.3} ({} samples)",
            b.ym1.irq(), b.ym2.read(0), b.ym2.irq(), mode, keys, peak, self.out.len());
    }
}

impl crate::arcade::hiscore::HiScoreMem for Dec0System {
    fn hs_read(&mut self, addr: u32) -> u8 { self.board.read8(addr & 0xFF_FFFF) }
    fn hs_write(&mut self, addr: u32, v: u8) { self.board.write8(addr & 0xFF_FFFF, v) }
}

impl System for Dec0System {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        for line in 0..LINES {
            self.board.vblank = !(FIRST_LINE..VBLANK_LINE).contains(&line);
            if line == VBLANK_LINE {
                self.render();
                self.board.irq6 = true;
            }
            self.run_line();
        }
        self.current_frame += 1;
        if self.debug && self.current_frame % 300 == 0 { self.report(); }
    }

    /// INPUTS (active low): player 1 up / down / left / right 0x0F, attack 0x10, jump 0x20;
    /// player 2 the same in the high byte. SYSTEM: start 1 0x04, start 2 0x08, coin 1 0x10,
    /// coin 2 0x20, service 0x40 (VBlank 0x80 is added on reading).
    fn set_input(&mut self, mask: u32) {
        let joy = |m: u32| -> u16 {
            let on = |b: u32| m & b != 0;
            let mut v = 0u16;
            if on(pad::UP) { v |= 0x01; }
            if on(pad::DOWN) { v |= 0x02; }
            if on(pad::LEFT) { v |= 0x04; }
            if on(pad::RIGHT) { v |= 0x08; }
            if on(pad::B) { v |= 0x10; }
            if on(pad::A) { v |= 0x20; }
            v
        };
        let on = |b: u32| mask & b != 0;
        self.board.inputs = !(joy(mask) | joy(pad::player2(mask)) << 8);
        let mut s = 0u16;
        if on(pad::START) { s |= 0x04; }
        if on(pad::START2) { s |= 0x08; }
        if on(pad::COIN) || on(pad::SELECT) { s |= 0x10; }
        self.board.system = !s & 0xFF7F;
    }

    fn set_option(&mut self, key: &str, value: i64) {
        if key == "debug_report" { self.debug = value != 0; }
    }

    fn drain_audio(&mut self) -> Vec<f32> { std::mem::take(&mut self.out) }
    fn sample_rate(&self) -> u32 { OUT_RATE }
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
        b.irq6 = false;
        b.intl = false;
        b.intr = false;
        b.snd_nmi = false;
        b.h6280 = H6280Io::default();
        b.ym1.reset();
        b.ym2.reset();
        b.oki.reset();
        self.main = M68000::new();
        self.main.reset(&mut self.board);
        self.snd = Cpu65c02::new(Variant::Sc02);
        self.snd.reset(&mut SoundBus(&mut self.board));
        self.sub = Huc6280::new();
        if self.board.has_sub { self.sub.reset(&mut SubBus(&mut self.board)); }
    }

    fn frame_time_usec(&self) -> i64 { (LINES as u64 * 1_000_000 / LINE_RATE) as i64 }

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
        let s = (&self.main, &self.snd, &self.sub, &self.board,
                 [self.main_phase, self.snd_phase, self.sub_phase], [self.main_owed, self.snd_owed, self.sub_owed],
                 self.current_frame);
        state::encode(&self.save_filename, STATE_VERSION, &s)
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        type S = (M68000, Cpu65c02, Huc6280, Dec0Board, [u64; 3], [i64; 3], u64);
        let (main, snd, sub, mut board, phase, owed, frame): S = state::decode(&self.save_filename, STATE_VERSION, data)?;
        if board.ram.len() != 0x4000 || board.dpram.len() != 0x800 || board.bac.len() != 3 || board.has_sub != self.board.has_sub {
            return Err("save state doesn't fit this board".into());
        }
        board.rom = std::mem::take(&mut self.board.rom);
        board.snd_rom = std::mem::take(&mut self.board.snd_rom);
        board.sub_rom = std::mem::take(&mut self.board.sub_rom);
        board.oki.keep_static(&mut self.board.oki);
        board.inputs = self.board.inputs;
        board.system = self.board.system;
        board.dsw = self.board.dsw;
        self.board = board;
        self.main = main;
        self.snd = snd;
        self.sub = sub;
        [self.main_phase, self.snd_phase, self.sub_phase] = phase;
        [self.main_owed, self.snd_owed, self.sub_owed] = owed;
        self.current_frame = frame;
        Ok(())
    }
}
