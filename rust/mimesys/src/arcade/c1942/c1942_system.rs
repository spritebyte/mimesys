//! Capcom's 1942 (1984). Hardware facts from MAME's 1942 driver (BSD-3-Clause, Paul
//! Leaman, Couriersud).
//!
//! - Main CPU: Z80 at 3 MHz; interrupts from the same scanline PROM as Commando (RST 10 at
//!   vblank, RST 08 at line 109, four sound IRQs a frame), held until taken.
//! - Sound CPU: Z80 at 3 MHz (IM 1), two AY-3-8910 at 1.5 MHz, two latches from the main
//!   CPU.
//! - Video: 6 MHz pixel clock, 384 x 262, picture lines 16-239.
//!   - 8x8 text (2 bits, pen 0 see-through), 64 colour sets through a lookup PROM
//!   - 16x16 background, 32 x 16 tiles (512 x 256, scrolled horizontally), 3 bits, 32
//!     colour sets through a lookup PROM, four palette banks
//!   - 32 sprites of 16x16, 4 bits (pen 15 see-through), 16 colour sets through a PROM;
//!     a sprite can be 1, 2, 4 or 16 tiles tall; the hardware walks the list per line in a
//!     fixed order in which sprites 16-23 only show on lines 16-127 and 24-31 on 128-239
//!   - 256 colours from three 4-bit PROMs (2.2k / 1k / 470 / 220 ohm)
//!
//! Main map: 0000-7FFF ROM, 8000-BFFF banked ROM (4 x 16 KB), C000-C004 R inputs / DIPs,
//!   C800 / C801 W sound latches, C802-C803 W scroll, C804 W control (bit 7 flip, bit 4
//!   holds the sound CPU in reset), C805 W palette bank, C806 W ROM bank,
//!   CC00-CC7F W sprites, D000-D7FF text, D800-DBFF background, E000-EFFF RAM.
//! Sound map: 0000-3FFF ROM, 4000-47FF RAM, 6000 / 6001 R latches, 8000-8001 AY 1,
//!   C000-C001 AY 2.

use crate::common::audio::ay8910::Ay8910;
use crate::common::cpu::z80::{Bus, Z80Cpu, Z80State};
use crate::common::input as pad;
use crate::common::system::{DisplayInfo, Rotation, System};
use crate::arcade::gng::gng_system::decode;
use crate::arcade::hiscore::HiScoreMem;
use crate::arcade::{state, take_region};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const STATE_VERSION: u32 = 1;

pub const SCREEN_W: usize = 256;
pub const SCREEN_H: usize = 224;
const LINES: usize = 262;
const FIRST_LINE: usize = 16;
const VBLANK_LINE: usize = 240;
const CPU_PER_LINE: i32 = 192;              // 3 MHz, 64 us lines
const AY_CLOCK: u32 = 1_500_000;
const CPU_CYCLES_PER_AY_TICK: i32 = 16;     // an AY step is 8 of its clocks

#[derive(Serialize, Deserialize)]
pub struct C1942Board {
    #[serde(skip)]
    rom: Vec<u8>,
    #[serde(skip)]
    snd_rom: Vec<u8>,
    ram: Vec<u8>,          // E000-EFFF
    fg: Vec<u8>,           // D000-D7FF
    bg: Vec<u8>,           // D800-DBFF
    sprites: Vec<u8>,      // CC00-CC7F
    scroll: [u8; 2],
    control: u8,
    pal_bank: u8,
    bank: u8,
    latch: [u8; 2],
    snd_ram: Vec<u8>,
    pub ay: [Ay8910; 2],
    #[serde(skip)]
    pub inputs: [u8; 3],
    #[serde(skip)]
    pub dsw: [u8; 2],
}

impl C1942Board {
    fn sound_running(&self) -> bool { self.control & 0x10 == 0 }
    fn flip(&self) -> bool { self.control & 0x80 != 0 }
}

impl Bus for C1942Board {
    fn read(&mut self, a: u16) -> u8 {
        match a {
            0x0000..=0x7FFF => self.rom[a as usize],
            0x8000..=0xBFFF => self.rom.get(0x10000 + self.bank as usize * 0x4000 + (a as usize & 0x3FFF)).copied().unwrap_or(0xFF),
            0xC000..=0xC002 => self.inputs[(a & 3) as usize],
            0xC003 | 0xC004 => self.dsw[(a - 0xC003) as usize],
            0xD000..=0xD7FF => self.fg[(a & 0x7FF) as usize],
            0xD800..=0xDBFF => self.bg[(a & 0x3FF) as usize],
            0xE000..=0xEFFF => self.ram[(a & 0xFFF) as usize],
            _ => 0xFF,
        }
    }
    fn write(&mut self, a: u16, v: u8) {
        match a {
            0xC800 | 0xC801 => self.latch[(a & 1) as usize] = v,
            0xC802 | 0xC803 => self.scroll[(a & 1) as usize] = v,
            0xC804 => {
                if self.sound_running() && v & 0x10 != 0 {
                    for ay in self.ay.iter_mut() { ay.reset(); }
                }
                self.control = v;
            }
            0xC805 => self.pal_bank = v & 3,
            0xC806 => self.bank = v & 3,
            0xCC00..=0xCC7F => self.sprites[(a & 0x7F) as usize] = v,
            0xD000..=0xD7FF => self.fg[(a & 0x7FF) as usize] = v,
            0xD800..=0xDBFF => self.bg[(a & 0x3FF) as usize] = v,
            0xE000..=0xEFFF => self.ram[(a & 0xFFF) as usize] = v,
            _ => {}
        }
    }
    fn port_in(&mut self, _p: u16) -> u8 { 0xFF }
    fn port_out(&mut self, _p: u16, _v: u8) {}
}

struct SoundBus<'a>(&'a mut C1942Board);

impl Bus for SoundBus<'_> {
    fn read(&mut self, a: u16) -> u8 {
        let b = &mut *self.0;
        match a {
            0x0000..=0x3FFF => b.snd_rom.get(a as usize).copied().unwrap_or(0xFF),
            0x4000..=0x47FF => b.snd_ram[(a & 0x7FF) as usize],
            0x6000 | 0x6001 => b.latch[(a & 1) as usize],
            _ => 0xFF,
        }
    }
    fn write(&mut self, a: u16, v: u8) {
        let b = &mut *self.0;
        let chip = match a { 0x8000 | 0x8001 => 0, 0xC000 | 0xC001 => 1, _ => 2 };
        match a {
            0x4000..=0x47FF => b.snd_ram[(a & 0x7FF) as usize] = v,
            _ if chip < 2 => {
                if a & 1 == 0 { b.ay[chip].write_address(v); } else { b.ay[chip].write_data(v); }
            }
            _ => {}
        }
    }
    fn port_in(&mut self, _p: u16) -> u8 { 0xFF }
    fn port_out(&mut self, _p: u16, _v: u8) {}
}

pub struct C1942System {
    pub main: Z80Cpu,
    pub snd: Z80Cpu,
    pub board: C1942Board,
    irq_prom: Vec<u8>,
    colors: Vec<[u8; 3]>,
    char_lut: Vec<u8>,
    tile_lut: Vec<u8>,
    spr_lut: Vec<u8>,
    chars: Vec<u8>,
    tiles: Vec<u8>,
    sprite_gfx: Vec<u8>,
    debt: [i32; 2],
    ay_cycles: i32,
    main_irq: Option<u8>,
    snd_irq: bool,
    snd_was_reset: bool,
    pub framebuffer: Vec<u8>,
    current_frame: u64,
    save_filename: String,
    save_path: String,
}

impl C1942System {
    pub fn new(set_name: &str, mut regions: HashMap<String, Vec<u8>>) -> Result<Self, String> {
        let mut rom = take_region(&mut regions, "maincpu")?;
        if rom.len() < 0x20000 { rom.resize(0x20000, 0xFF); }
        let snd_rom = take_region(&mut regions, "audiocpu")?;
        let gfx1 = take_region(&mut regions, "gfx1")?;
        let gfx2 = take_region(&mut regions, "gfx2")?;
        let gfx3 = take_region(&mut regions, "gfx3")?;
        let palproms = take_region(&mut regions, "palproms")?;
        let char_lut = take_region(&mut regions, "charprom")?;
        let tile_lut = take_region(&mut regions, "tileprom")?;
        let spr_lut = take_region(&mut regions, "sprprom")?;
        let irq_prom = take_region(&mut regions, "irqprom")?;
        if gfx1.len() < 0x2000 || gfx2.len() < 0xC000 || gfx3.len() < 0x10000 || palproms.len() < 0x300
            || char_lut.len() < 0x100 || tile_lut.len() < 0x100 || spr_lut.len() < 0x100 || irq_prom.len() < 0x100 {
            return Err("1942: graphics or PROM regions too small".into());
        }
        let level = |v: u8| -> u8 {
            let w = [0x0E, 0x1F, 0x43, 0x8F];
            (0..4).filter(|b| v >> b & 1 != 0).map(|b| w[b] as u32).sum::<u32>() as u8
        };
        let colors = (0..256).map(|i| [level(palproms[i]), level(palproms[0x100 + i]), level(palproms[0x200 + i])]).collect();

        let char_ys: Vec<usize> = (0..8).map(|y| y * 16).collect();
        let chars = decode(&gfx1, 512, &[4, 0], &[0, 1, 2, 3, 8, 9, 10, 11], &char_ys, 128);
        let third = 0x4000 * 8;
        let tile_xs: Vec<usize> = (0..8).chain(128..136).collect();
        let tile_ys: Vec<usize> = (0..16).map(|y| y * 8).collect();
        let tiles = decode(&gfx2, 512, &[0, third, 2 * third], &tile_xs, &tile_ys, 256);
        let half = 0x8000 * 8;
        let spr_xs = [0, 1, 2, 3, 8, 9, 10, 11, 256, 257, 258, 259, 264, 265, 266, 267];
        let spr_ys: Vec<usize> = (0..16).map(|y| y * 16).collect();
        let sprite_gfx = decode(&gfx3, 512, &[half + 4, half, 4, 0], &spr_xs, &spr_ys, 512);

        let board = C1942Board {
            rom,
            snd_rom,
            ram: vec![0; 0x1000],
            fg: vec![0; 0x800],
            bg: vec![0; 0x400],
            sprites: vec![0; 0x80],
            scroll: [0; 2],
            control: 0,
            pal_bank: 0,
            bank: 0,
            latch: [0; 2],
            snd_ram: vec![0; 0x800],
            ay: [Ay8910::new(AY_CLOCK, 44100), Ay8910::new(AY_CLOCK, 44100)],
            inputs: [0xFF; 3],
            // 1 coin 1 credit, upright, 20K 80K 80K+, 3 lives; normal, no flip, no screen stop
            dsw: [0xF7, 0xFF],
        };
        Ok(Self {
            main: Z80Cpu::new(),
            snd: Z80Cpu::new(),
            board,
            irq_prom,
            colors,
            char_lut,
            tile_lut,
            spr_lut,
            chars,
            tiles,
            sprite_gfx,
            debt: [0; 2],
            ay_cycles: 0,
            main_irq: None,
            snd_irq: false,
            snd_was_reset: false,
            framebuffer: vec![0; SCREEN_W * SCREEN_H * 4],
            current_frame: 0,
            save_filename: set_name.to_string(),
            save_path: "user://GD_EMU/Arcade/Capcom/State".to_string(),
        })
    }

    fn run_line(&mut self) {
        self.debt[0] += CPU_PER_LINE;
        while self.debt[0] > 0 {
            let mut c = 0;
            if let Some(v) = self.main_irq {
                c = self.main.irq(&mut self.board, v) as i32;
                if c > 0 { self.main_irq = None; }
            }
            if c == 0 { c = self.main.step(&mut self.board) as i32; }
            self.debt[0] -= c;
        }
        if self.board.sound_running() {
            if self.snd_was_reset {
                self.snd = Z80Cpu::new();
                self.snd_irq = false;
                self.snd_was_reset = false;
            }
            self.debt[1] += CPU_PER_LINE;
            while self.debt[1] > 0 {
                let mut c = 0;
                if self.snd_irq {
                    c = self.snd.irq(&mut SoundBus(&mut self.board), 0xFF) as i32;
                    if c > 0 { self.snd_irq = false; }
                }
                if c == 0 { c = self.snd.step(&mut SoundBus(&mut self.board)) as i32; }
                self.debt[1] -= c;
            }
        } else {
            self.snd_was_reset = true;
            self.debt[1] = 0;
        }
        self.ay_cycles += CPU_PER_LINE;
        while self.ay_cycles >= CPU_CYCLES_PER_AY_TICK {
            self.ay_cycles -= CPU_CYCLES_PER_AY_TICK;
            for ay in self.board.ay.iter_mut() { ay.tick(); }
        }
    }

    /// The sprites on bitmap line `y` (MAME's per-line walk of the list), into `line`.
    fn sprite_line(&self, y: usize, line: &mut [u8]) {
        let b = &self.board;
        let flip = b.flip();
        let v: u8 = if flip { !((y as u8).wrapping_sub(1)) } else { (y as u8).wrapping_sub(1) };
        let bit = |x: u32, n: u32| x >> n & 1 != 0;
        for h in (128u32..=496).rev().step_by(16) {
            let objcnt4 = bit(h, 8) != bit(!h, 7);
            let objcnt3 = (bit(v as u32, 7) && objcnt4) != bit(!h, 7);
            let idx = (((h >> 4) & 7) | if objcnt3 { 8 } else { 0 } | if objcnt4 { 0x10 } else { 0 }) as usize * 4;
            let o = &b.sprites[idx..idx + 4];
            let mut code = (o[0] as usize & 0x7F) | ((o[1] as usize >> 5 & 1) << 7) | ((o[0] as usize >> 7) << 8);
            let col = (o[1] & 0x0F) as usize;
            let mut sx = o[3] as i32 - ((o[1] as i32 >> 4 & 1) << 8);
            let mut sy = o[2] as i32;
            // is this line inside the sprite's height?
            let valpha = o[2];
            let v2c = (!v).wrapping_add(if flip { 0x01 } else { 0xFF });
            let lvbeta = v2c.wrapping_add(valpha);
            let vbeta = !lvbeta;
            let vleq = vbeta <= !valpha;
            let vlen = o[1] >> 6;
            let top = |n: u32| (0..n).all(|k| lvbeta >> (7 - k) & 1 != 0);
            let vinlen = match vlen { 0 => top(4), 1 => top(3), 2 => top(2), _ => true };
            if !(vleq && vinlen) { continue; }
            let dir = if flip { sx = 240 - sx; sy = 240 - sy; -1 } else { 1 };
            let rows = if vlen == 3 { 16 } else { 1usize << vlen };
            code &= !(rows - 1);
            for i in 0..rows {
                let ty = sy + 16 * i as i32 * dir;
                let r = y as i32 - ty;
                if !(0..16).contains(&r) { continue; }
                let r = if flip { 15 - r } else { r } as usize;
                let g = &self.sprite_gfx[((code + i) & 511) * 256 + r * 16..][..16];
                for x in 0..16 {
                    let px = sx + x as i32;
                    if !(0..256).contains(&px) { continue; }
                    let p = g[if flip { 15 - x } else { x }];
                    if p != 15 { line[px as usize] = 0x40 | self.spr_lut[col * 16 + p as usize]; }
                }
            }
        }
    }

    pub fn render(&mut self) {
        let b = &self.board;
        let mut out = vec![0u8; 256 * 256];
        // background: 32 x 16 tiles scanned by columns, two bytes 16 apart per tile
        let sx = ((b.scroll[0] as usize) | (b.scroll[1] as usize) << 8) & 0x1FF;
        for y in 0..256 {
            for x in 0..256 {
                let tx = (x + sx) & 0x1FF;
                let ti = (tx >> 4) * 16 + (y >> 4);
                let off = (ti & 0x0F) | ((ti & 0x1F0) << 1);
                let attr = b.bg[off + 0x10];
                let code = b.bg[off] as usize | ((attr as usize & 0x80) << 1);
                let cx = if attr & 0x20 != 0 { 15 - (tx & 15) } else { tx & 15 };
                let cy = if attr & 0x40 != 0 { 15 - (y & 15) } else { y & 15 };
                let pen = self.tiles[(code * 16 + cy) * 16 + cx] as usize;
                out[y * 256 + x] = (b.pal_bank << 4) | self.tile_lut[(attr as usize & 0x1F) * 8 + pen];
            }
        }
        // text
        let mut text = vec![0xFFFFu16; 256 * 256];
        for ty in 0..32 {
            for tx in 0..32 {
                let i = ty * 32 + tx;
                let attr = b.fg[i + 0x400];
                let code = b.fg[i] as usize | ((attr as usize & 0x80) << 1);
                let col = (attr & 0x3F) as usize;
                for y in 0..8 {
                    for x in 0..8 {
                        let p = self.chars[(code * 8 + y) * 8 + x] as usize;
                        if p != 0 { text[(ty * 8 + y) * 256 + tx * 8 + x] = (0x80 | self.char_lut[col * 4 + p]) as u16; }
                    }
                }
            }
        }
        let flip = b.flip();
        if flip { out.reverse(); text.reverse(); }
        for y in FIRST_LINE..FIRST_LINE + SCREEN_H {
            let line = &mut out[y * 256..y * 256 + 256];
            self.sprite_line(y, line);
            for x in 0..256 {
                let t = text[y * 256 + x];
                if t != 0xFFFF { line[x] = t as u8; }
            }
        }
        for y in 0..SCREEN_H {
            for x in 0..SCREEN_W {
                let c = self.colors[out[(y + FIRST_LINE) * 256 + x] as usize];
                let o = (y * SCREEN_W + x) * 4;
                self.framebuffer[o..o + 4].copy_from_slice(&[c[0], c[1], c[2], 0xFF]);
            }
        }
    }
}

impl System for C1942System {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        for line in 0..LINES {
            if line == VBLANK_LINE { self.render(); }
            let irq = self.irq_prom[line & 0xFF];
            if irq & 8 != 0 { self.main_irq = Some(0xC7 | (irq << 3 & 0x18)); }
            if irq & 4 != 0 { self.snd_irq = true; }
            self.run_line();
        }
        self.current_frame += 1;
    }

    /// Active low.
    ///   system: 0x01 start 1, 0x02 start 2, 0x10 service, 0x40 coin 2, 0x80 coin 1
    ///   P1 / P2: 0x01 right, 0x02 left, 0x04 down, 0x08 up, 0x10 fire, 0x20 loop
    fn set_input(&mut self, mask: u32) {
        let joy = |m: u32| -> u8 {
            let on = |b: u32| m & b != 0;
            let mut v = 0u8;
            if on(pad::RIGHT) { v |= 0x01; }
            if on(pad::LEFT) { v |= 0x02; }
            if on(pad::DOWN) { v |= 0x04; }
            if on(pad::UP) { v |= 0x08; }
            if on(pad::B) { v |= 0x10; }
            if on(pad::A) { v |= 0x20; }
            !v
        };
        let on = |b: u32| mask & b != 0;
        let mut sys = 0u8;
        if on(pad::START) { sys |= 0x01; }
        if on(pad::START2) { sys |= 0x02; }
        if on(pad::X) { sys |= 0x10; }
        if on(pad::COIN) || on(pad::SELECT) { sys |= 0x80; }
        self.board.inputs = [!sys, joy(mask), joy(pad::player2(mask))];
    }

    fn drain_audio(&mut self) -> Vec<f32> {
        let [a, b] = &mut self.board.ay;
        let n = a.out.len().min(b.out.len());
        let out: Vec<f32> = (0..n).map(|k| (a.out[k] + b.out[k]) * 0.6).collect();
        a.out.drain(..n);
        b.out.drain(..n);
        out
    }

    fn sample_rate(&self) -> u32 { 44100 }
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
        self.main = Z80Cpu::new();
        self.snd = Z80Cpu::new();
        let b = &mut self.board;
        b.scroll = [0; 2];
        b.control = 0;
        b.bank = 0;
        b.pal_bank = 0;
        for ay in b.ay.iter_mut() { ay.reset(); }
        self.main_irq = None;
        self.snd_irq = false;
    }

    fn frame_time_usec(&self) -> i64 { (384.0 * 262.0 / 6.0) as i64 }

    fn display_info(&self) -> DisplayInfo {
        DisplayInfo {
            buffer_width: SCREEN_W as i32,
            buffer_height: SCREEN_H as i32,
            visible_x: 0,
            visible_y: 0,
            visible_width: SCREEN_W as i32,
            visible_height: SCREEN_H as i32,
            target_aspect_ratio: 3.0 / 4.0,   // after rotation
            rotation: Rotation::CCW90,        // MAME ROT270
        }
    }

    fn save_state(&self) -> Vec<u8> {
        let s = (self.main.save_state(), self.snd.save_state(), &self.board, self.debt, self.ay_cycles, self.main_irq, self.snd_irq, self.snd_was_reset, self.current_frame);
        state::encode(&self.save_filename, STATE_VERSION, &s)
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let (main, snd, mut board, debt, ay_cycles, main_irq, snd_irq, snd_was_reset, frame): (Z80State, Z80State, C1942Board, [i32; 2], i32, Option<u8>, bool, bool, u64) =
            state::decode(&self.save_filename, STATE_VERSION, data)?;
        if board.ram.len() != 0x1000 || board.fg.len() != 0x800 || board.bg.len() != 0x400 || board.sprites.len() != 0x80 || board.snd_ram.len() != 0x800 {
            return Err("save state doesn't fit this board".into());
        }
        board.rom = std::mem::take(&mut self.board.rom);
        board.snd_rom = std::mem::take(&mut self.board.snd_rom);
        board.inputs = self.board.inputs;
        board.dsw = self.board.dsw;
        for k in 0..2 { board.ay[k].out = std::mem::take(&mut self.board.ay[k].out); }
        self.board = board;
        self.main.load_state(&main);
        self.snd.load_state(&snd);
        self.debt = debt;
        self.ay_cycles = ay_cycles;
        self.main_irq = main_irq;
        self.snd_irq = snd_irq;
        self.snd_was_reset = snd_was_reset;
        self.current_frame = frame;
        Ok(())
    }
}

impl C1942System {
    #[cfg(test)]
    pub fn sound_bus(&mut self) -> impl Bus + '_ { SoundBus(&mut self.board) }
}

impl HiScoreMem for C1942System {
    fn hs_read(&mut self, addr: u16) -> u8 { self.board.read(addr) }
    fn hs_write(&mut self, addr: u16, v: u8) { self.board.write(addr, v) }
}
