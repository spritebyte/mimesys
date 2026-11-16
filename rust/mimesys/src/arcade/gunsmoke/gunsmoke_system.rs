//! Capcom's Gun.Smoke (1985). Hardware facts from MAME's gunsmoke driver (BSD-3-Clause,
//! Paul Leaman).
//!
//! - Main CPU: Z80 at 3 MHz, IRQ at vblank (held until taken), 4 x 16 KB ROM banks.
//! - Sound CPU: Z80 at 3 MHz, IRQ four times a frame; two YM2203 at 1.5 MHz.
//! - Video: 6 MHz pixel clock, 384 x 262, picture lines 16-239, rotated.
//!   - background of 32x32 tiles (4 bits) laid out by a ROM: 2048 x 8 tiles (65536 x 256
//!     pixels), scrolled 16 bits across and 8 down
//!   - 128 buffered sprites of 16x16 (4 bits, pen 0 see-through); bank 3 picks one of
//!     eight banks from the layer register
//!   - 8x8 text (2 bits); see-through where its looked-up colour is 0x4F
//!   - every layer through lookup PROMs into 256 colours from three 4-bit PROMs
//!   - the text, background and sprites each switch off
//!
//! Main map: 0000-7FFF ROM, 8000-BFFF banked ROM, C000-C004 R inputs / DIPs, C4C9-C4CB R
//!   protection (reads FF 00 00), C800 W sound latch, C804 W control (bits 2-3 bank, 6 flip,
//!   7 text on), C806 W copy sprites to the buffer, D000-D7FF text, D800-D802 scroll,
//!   D806 W layers (bits 0-2 sprite bank, 4 background on, 5 sprites on), E000-EFFF RAM,
//!   F000-FFFF sprites.
//! Sound map: 0000-7FFF ROM, C000-C7FF RAM, C800 R latch, E000-E003 W the YM2203s.

use crate::common::audio::ym2203::Ym2203;
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
const CPU_PER_LINE: i32 = 192;    // 3 MHz, 64 us lines
const YM_PER_LINE: u32 = 96;      // 1.5 MHz
const YM_CLOCK: u32 = 1_500_000;
/// "black" for a switched-off background
const BLACK: u16 = 0x100;

#[derive(Serialize, Deserialize)]
pub struct GunsmokeBoard {
    #[serde(skip)]
    rom: Vec<u8>,
    #[serde(skip)]
    snd_rom: Vec<u8>,
    ram: Vec<u8>,          // E000-EFFF
    sprite_ram: Vec<u8>,   // F000-FFFF
    sprite_buf: Vec<u8>,
    vram: Vec<u8>,         // D000-D3FF
    cram: Vec<u8>,         // D400-D7FF
    scroll: [u8; 3],
    control: u8,
    layers: u8,
    sound_latch: u8,
    snd_ram: Vec<u8>,
    pub ym: [Ym2203; 2],
    #[serde(skip)]
    pub inputs: [u8; 3],
    #[serde(skip)]
    pub dsw: [u8; 2],
}

impl GunsmokeBoard {
    fn bank(&self) -> usize { (self.control as usize >> 2) & 3 }
    fn flip(&self) -> bool { self.control & 0x40 != 0 }
    fn text_on(&self) -> bool { self.control & 0x80 != 0 }
    fn bg_on(&self) -> bool { self.layers & 0x10 != 0 }
    fn sprites_on(&self) -> bool { self.layers & 0x20 != 0 }
}

impl Bus for GunsmokeBoard {
    fn read(&mut self, a: u16) -> u8 {
        match a {
            0x0000..=0x7FFF => self.rom[a as usize],
            0x8000..=0xBFFF => self.rom.get(0x8000 + self.bank() * 0x4000 + (a as usize & 0x3FFF)).copied().unwrap_or(0xFF),
            0xC000..=0xC002 => self.inputs[(a & 3) as usize],
            0xC003 | 0xC004 => self.dsw[(a - 0xC003) as usize],
            // read by the routine at 0E69: a zero first byte would make it jump through
            // the next two (MAME's fix)
            0xC4C9 => 0xFF,
            0xC4CA | 0xC4CB => 0x00,
            0xD000..=0xD3FF => self.vram[(a & 0x3FF) as usize],
            0xD400..=0xD7FF => self.cram[(a & 0x3FF) as usize],
            0xD800..=0xD802 => self.scroll[(a & 3) as usize],
            0xE000..=0xEFFF => self.ram[(a & 0xFFF) as usize],
            0xF000..=0xFFFF => self.sprite_ram[(a & 0xFFF) as usize],
            _ => 0xFF,
        }
    }
    fn write(&mut self, a: u16, v: u8) {
        match a {
            0xC800 => self.sound_latch = v,
            0xC804 => self.control = v,
            0xC806 => self.sprite_buf.copy_from_slice(&self.sprite_ram),
            0xD000..=0xD3FF => self.vram[(a & 0x3FF) as usize] = v,
            0xD400..=0xD7FF => self.cram[(a & 0x3FF) as usize] = v,
            0xD800..=0xD802 => self.scroll[(a & 3) as usize] = v,
            0xD806 => self.layers = v,
            0xE000..=0xEFFF => self.ram[(a & 0xFFF) as usize] = v,
            0xF000..=0xFFFF => self.sprite_ram[(a & 0xFFF) as usize] = v,
            _ => {}
        }
    }
    fn port_in(&mut self, _p: u16) -> u8 { 0xFF }
    fn port_out(&mut self, _p: u16, _v: u8) {}
}

struct SoundBus<'a>(&'a mut GunsmokeBoard);

impl Bus for SoundBus<'_> {
    fn read(&mut self, a: u16) -> u8 {
        let b = &mut *self.0;
        match a {
            0x0000..=0x7FFF => b.snd_rom.get(a as usize).copied().unwrap_or(0xFF),
            0xC000..=0xC7FF => b.snd_ram[(a & 0x7FF) as usize],
            0xC800 => b.sound_latch,
            _ => 0xFF,
        }
    }
    fn write(&mut self, a: u16, v: u8) {
        let b = &mut *self.0;
        match a {
            0xC000..=0xC7FF => b.snd_ram[(a & 0x7FF) as usize] = v,
            0xE000..=0xE003 => b.ym[(a as usize >> 1) & 1].write(a as u8 & 1, v),
            _ => {}
        }
    }
    fn port_in(&mut self, _p: u16) -> u8 { 0xFF }
    fn port_out(&mut self, _p: u16, _v: u8) {}
}

pub struct GunsmokeSystem {
    pub main: Z80Cpu,
    pub snd: Z80Cpu,
    pub board: GunsmokeBoard,
    colors: Vec<[u8; 3]>,
    /// looked-up colour of each (set, pen): text 32 x 4, background 16 x 16, sprites 16 x 16
    char_pal: Vec<u8>,
    tile_pal: Vec<u8>,
    spr_pal: Vec<u8>,
    chars: Vec<u8>,
    tiles: Vec<u8>,
    sprites: Vec<u8>,
    bg_map: Vec<u8>,
    debt: [i32; 2],
    main_irq: bool,
    snd_irq: bool,
    pub framebuffer: Vec<u8>,
    current_frame: u64,
    save_filename: String,
    save_path: String,
}

impl GunsmokeSystem {
    pub fn new(set_name: &str, mut regions: HashMap<String, Vec<u8>>) -> Result<Self, String> {
        let mut rom = take_region(&mut regions, "maincpu")?;
        if rom.len() < 0x18000 { rom.resize(0x18000, 0xFF); }
        let snd_rom = take_region(&mut regions, "audiocpu")?;
        let chars = take_region(&mut regions, "chars")?;
        let tiles = take_region(&mut regions, "tiles")?;
        let sprites = take_region(&mut regions, "sprites")?;
        let bg_map = take_region(&mut regions, "bgtiles")?;
        let proms = take_region(&mut regions, "proms")?;
        if chars.len() < 0x4000 || tiles.len() < 0x40000 || sprites.len() < 0x40000 || bg_map.len() < 0x8000 || proms.len() < 0x800 {
            return Err("gunsmoke: graphics or PROM regions too small".into());
        }
        let colors = (0..256).map(|i| [(proms[i] & 15) * 17, (proms[0x100 + i] & 15) * 17, (proms[0x200 + i] & 15) * 17]).collect();
        let char_pal = (0..0x80).map(|i| proms[0x300 + i] & 15 | 0x40).collect();
        let tile_pal = (0..0x100).map(|i| proms[0x400 + i] & 15 | (proms[0x500 + i] & 3) << 4).collect();
        let spr_pal = (0..0x100).map(|i| proms[0x600 + i] & 15 | (proms[0x700 + i] & 7) << 4 | 0x80).collect();

        let chars = decode(&chars, 1024, &[4, 0], &[11, 10, 9, 8, 3, 2, 1, 0],
                           &[112, 96, 80, 64, 48, 32, 16, 0], 128);
        let tile_xs: Vec<usize> = (0..4).flat_map(|q| [0, 1, 2, 3, 8, 9, 10, 11].map(|x| q * 512 + x)).collect();
        let tile_ys: Vec<usize> = (0..32).map(|y| y * 16).collect();
        let half = 0x20000 * 8;
        let tiles = decode(&tiles, 512, &[half + 4, half, 4, 0], &tile_xs, &tile_ys, 2048);
        let spr_xs = [0, 1, 2, 3, 8, 9, 10, 11, 256, 257, 258, 259, 264, 265, 266, 267];
        let spr_ys: Vec<usize> = (0..16).map(|y| y * 16).collect();
        let sprites = decode(&sprites, 2048, &[half + 4, half, 4, 0], &spr_xs, &spr_ys, 512);

        let mut ym = [Ym2203::new(YM_CLOCK, 44100), Ym2203::new(YM_CLOCK, 44100)];
        for y in ym.iter_mut() { y.set_mix(0.22, 0.14); }
        let board = GunsmokeBoard {
            rom,
            snd_rom,
            ram: vec![0; 0x1000],
            sprite_ram: vec![0; 0x1000],
            sprite_buf: vec![0; 0x1000],
            vram: vec![0; 0x400],
            cram: vec![0; 0x400],
            scroll: [0; 3],
            control: 0,
            layers: 0,
            sound_latch: 0,
            snd_ram: vec![0; 0x800],
            ym,
            inputs: [0xFF; 3],
            // 30K 80K 80K+, demo on, upright, normal, no freeze, service off;
            // 1 coin 1 credit both, continue, demo sounds
            dsw: [0xF7, 0xFF],
        };
        Ok(Self {
            main: Z80Cpu::new(),
            snd: Z80Cpu::new(),
            board,
            colors,
            char_pal,
            tile_pal,
            spr_pal,
            chars,
            tiles,
            sprites,
            bg_map,
            debt: [0; 2],
            main_irq: false,
            snd_irq: false,
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
            if self.main_irq {
                c = self.main.irq(&mut self.board, 0xFF) as i32;
                if c > 0 { self.main_irq = false; }
            }
            if c == 0 { c = self.main.step(&mut self.board) as i32; }
            self.debt[0] -= c;
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
        for y in self.board.ym.iter_mut() { y.run(YM_PER_LINE); }
    }

    pub fn render(&mut self) {
        let b = &self.board;
        let mut out = vec![BLACK; 256 * 256];
        if b.bg_on() {
            let sx = b.scroll[0] as usize | (b.scroll[1] as usize) << 8;
            let sy = b.scroll[2] as usize;
            for y in 0..256 {
                let ty = (y + sy) & 0xFF;
                for x in 0..256 {
                    let tx = (x + sx) & 0xFFFF;
                    let ti = (tx >> 5) * 8 + (ty >> 5);   // 2048 x 8, scanned by columns
                    let attr = self.bg_map[ti * 2 + 1];
                    let code = self.bg_map[ti * 2] as usize | (attr as usize & 1) << 8;
                    let color = ((attr >> 2) & 15) as usize;
                    let cx = if attr & 0x40 != 0 { 31 - (tx & 31) } else { tx & 31 };
                    let cy = if attr & 0x80 != 0 { 31 - (ty & 31) } else { ty & 31 };
                    let pen = self.tiles[(code * 32 + cy) * 32 + cx] as usize;
                    out[y * 256 + x] = self.tile_pal[color * 16 + pen] as u16;
                }
            }
        }
        if b.sprites_on() {
            let bank3 = (b.layers & 7) as usize;
            for offs in (0..0x1000).step_by(32).rev() {
                let s = &b.sprite_buf[offs..offs + 4];
                let attr = s[1];
                let mut bank = (attr >> 6) as usize;
                if bank == 3 { bank += bank3; }
                let code = (s[0] as usize + 256 * bank) & 2047;
                let color = (attr & 0x0F) as usize;
                let fy = attr & 0x10 != 0;
                let x0 = s[3] as i32 - ((attr as i32 & 0x20) << 3);
                let y0 = s[2] as i32;
                for y in 0..16 {
                    let py = y0 + y as i32;
                    if !(0..256).contains(&py) { continue; }
                    for x in 0..16 {
                        let px = x0 + x as i32;
                        if !(0..256).contains(&px) { continue; }
                        let p = self.sprites[(code * 16 + if fy { 15 - y } else { y }) * 16 + x] as usize;
                        if p != 0 { out[py as usize * 256 + px as usize] = self.spr_pal[color * 16 + p] as u16; }
                    }
                }
            }
        }
        if b.text_on() {
            for ty in 0..32 {
                for tx in 0..32 {
                    let i = ty * 32 + tx;
                    let attr = b.cram[i];
                    let code = b.vram[i] as usize | ((attr as usize & 0xE0) << 2);
                    let color = (attr & 0x1F) as usize;
                    for y in 0..8 {
                        for x in 0..8 {
                            let c = self.char_pal[color * 4 + self.chars[(code * 8 + y) * 8 + x] as usize];
                            if c != 0x4F { out[(ty * 8 + y) * 256 + tx * 8 + x] = c as u16; }
                        }
                    }
                }
            }
        }
        let flip = b.flip();
        for y in 0..SCREEN_H {
            for x in 0..SCREEN_W {
                let (fy, fx) = if flip { (255 - (y + FIRST_LINE), 255 - x) } else { (y + FIRST_LINE, x) };
                let v = out[fy * 256 + fx];
                let c = if v == BLACK { [0, 0, 0] } else { self.colors[v as usize] };
                let o = (y * SCREEN_W + x) * 4;
                self.framebuffer[o..o + 4].copy_from_slice(&[c[0], c[1], c[2], 0xFF]);
            }
        }
    }
}

impl System for GunsmokeSystem {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        for line in 0..LINES {
            if line == VBLANK_LINE {
                self.render();
                self.main_irq = true;
            }
            // the sound CPU's timer: a quarter frame
            if line % 66 == 0 { self.snd_irq = true; }
            self.run_line();
        }
        self.current_frame += 1;
    }

    /// Active low.
    ///   system: 0x01 start 1, 0x02 start 2, 0x10 service, 0x40 coin 1, 0x80 coin 2
    ///   P1 / P2: 0x01 right, 0x02 left, 0x04 down, 0x08 up, 0x10 shoot left,
    ///            0x20 shoot ahead, 0x40 shoot right
    fn set_input(&mut self, mask: u32) {
        let joy = |m: u32| -> u8 {
            let on = |b: u32| m & b != 0;
            let mut v = 0u8;
            if on(pad::RIGHT) { v |= 0x01; }
            if on(pad::LEFT) { v |= 0x02; }
            if on(pad::DOWN) { v |= 0x04; }
            if on(pad::UP) { v |= 0x08; }
            if on(pad::Y) { v |= 0x10; }
            if on(pad::B) { v |= 0x20; }
            if on(pad::A) { v |= 0x40; }
            !v
        };
        let on = |b: u32| mask & b != 0;
        let mut sys = 0u8;
        if on(pad::START) { sys |= 0x01; }
        if on(pad::START2) { sys |= 0x02; }
        if on(pad::X) { sys |= 0x10; }
        if on(pad::COIN) || on(pad::SELECT) { sys |= 0x40; }
        let p2 = pad::player2(mask);
        self.board.inputs = [!sys, joy(mask), joy(p2)];
    }

    fn drain_audio(&mut self) -> Vec<f32> {
        let [a, b] = &mut self.board.ym;
        let n = a.out.len().min(b.out.len());
        let mut out = Vec::with_capacity(n * 2);
        for k in 0..n {
            let v = (a.out[k] + b.out[k]) * 0.8;
            out.push(v);
            out.push(v);
        }
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
        b.control = 0;
        b.layers = 0;
        for y in b.ym.iter_mut() { y.reset(); }
        self.main_irq = false;
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
        let s = (self.main.save_state(), self.snd.save_state(), &self.board, self.debt, self.main_irq, self.snd_irq, self.current_frame);
        state::encode(&self.save_filename, STATE_VERSION, &s)
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let (main, snd, mut board, debt, main_irq, snd_irq, frame): (Z80State, Z80State, GunsmokeBoard, [i32; 2], bool, bool, u64) =
            state::decode(&self.save_filename, STATE_VERSION, data)?;
        if board.ram.len() != 0x1000 || board.sprite_ram.len() != 0x1000 || board.sprite_buf.len() != 0x1000
            || board.vram.len() != 0x400 || board.cram.len() != 0x400 || board.snd_ram.len() != 0x800 {
            return Err("save state doesn't fit this board".into());
        }
        board.rom = std::mem::take(&mut self.board.rom);
        board.snd_rom = std::mem::take(&mut self.board.snd_rom);
        board.inputs = self.board.inputs;
        board.dsw = self.board.dsw;
        for k in 0..2 { board.ym[k].out = std::mem::take(&mut self.board.ym[k].out); }
        self.board = board;
        self.main.load_state(&main);
        self.snd.load_state(&snd);
        self.debt = debt;
        self.main_irq = main_irq;
        self.snd_irq = snd_irq;
        self.current_frame = frame;
        Ok(())
    }
}

impl GunsmokeSystem {
    #[cfg(test)]
    pub fn sound_bus(&mut self) -> impl Bus + '_ { SoundBus(&mut self.board) }
}

impl HiScoreMem for GunsmokeSystem {
    fn hs_read(&mut self, addr: u16) -> u8 { self.board.read(addr) }
    fn hs_write(&mut self, addr: u16, v: u8) { self.board.write(addr, v) }
}
