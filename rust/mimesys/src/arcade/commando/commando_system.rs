//! Capcom's Commando board (1985). Hardware facts from MAME's commando driver
//! (BSD-3-Clause, Nicola Salmoria).
//!
//! - Main CPU: Z80 at 3 MHz with encrypted opcodes (data bits swapped on M1 fetches).
//!   Its interrupts come from a PROM read every scanline: bit 3 raises an IRQ with the
//!   RST vector from bits 0-1 (RST 10 at vblank, line 240), bit 2 the sound CPU's (four a
//!   frame). Both are held until taken.
//! - Sound CPU: Z80 at 3 MHz, two YM2203 at 1.5 MHz.
//! - Video: as Ghosts'n Goblins without the priority split: 16x16 scrolling background
//!   (3 bits, 16 colour sets), 96 buffered sprites of 16x16 (4 bits, pen 15 see-through,
//!   bank 3 not drawn), 8x8 text (2 bits, pen 3 see-through); 256 colours from three
//!   4-bit PROMs.
//!
//! Main map: 0000-BFFF ROM, C000-C004 R inputs / DIPs, C800 W sound latch,
//!   C804 W control (bit 4 holds the sound CPU in reset, bit 7 flips the screen),
//!   C806 W copy sprites to the buffer, C808-C80B W background scroll X, Y,
//!   D000-D7FF text codes / attributes, D800-DFFF background codes / attributes,
//!   E000-FFFF RAM (FE00-FF7F the sprites).
//! Sound map: 0000-3FFF ROM, 4000-47FF RAM, 6000 R latch, 8000-8003 W the YM2203s.

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

/// Opcode decryption: bits 0 and 4 stay, 7-5 go to 3-1, 3-1 go to 7-5.
fn decrypt(b: u8) -> u8 { (b & 0x11) | (b & 0xE0) >> 4 | (b & 0x0E) << 4 }

#[derive(Serialize, Deserialize)]
pub struct CommandoBoard {
    #[serde(skip)]
    rom: Vec<u8>,
    #[serde(skip)]
    opcodes: Vec<u8>,
    #[serde(skip)]
    snd_rom: Vec<u8>,
    ram: Vec<u8>,          // E000-FFFF
    fg: Vec<u8>,           // D000-D7FF
    bg: Vec<u8>,           // D800-DFFF
    sprite_buf: Vec<u8>,
    scroll: [u8; 4],
    control: u8,
    sound_latch: u8,
    snd_ram: Vec<u8>,
    pub ym: [Ym2203; 2],
    #[serde(skip)]
    pub inputs: [u8; 3],
    #[serde(skip)]
    pub dsw: [u8; 2],
}

impl CommandoBoard {
    fn sound_running(&self) -> bool { self.control & 0x10 == 0 }
    fn flip(&self) -> bool { self.control & 0x80 != 0 }
}

impl Bus for CommandoBoard {
    fn read(&mut self, a: u16) -> u8 {
        match a {
            0x0000..=0xBFFF => self.rom[a as usize],
            0xC000..=0xC002 => self.inputs[(a & 3) as usize],
            0xC003 | 0xC004 => self.dsw[(a - 0xC003) as usize],
            0xD000..=0xD7FF => self.fg[(a & 0x7FF) as usize],
            0xD800..=0xDFFF => self.bg[(a & 0x7FF) as usize],
            0xE000..=0xFFFF => self.ram[(a & 0x1FFF) as usize],
            _ => 0xFF,
        }
    }
    fn read_opcode(&mut self, a: u16) -> u8 {
        if a < 0xC000 { self.opcodes[a as usize] } else { self.read(a) }
    }
    fn write(&mut self, a: u16, v: u8) {
        match a {
            0xC800 => self.sound_latch = v,
            0xC804 => {
                if self.sound_running() && v & 0x10 != 0 {
                    for y in self.ym.iter_mut() { y.reset(); }
                }
                self.control = v;
            }
            0xC806 => self.sprite_buf.copy_from_slice(&self.ram[0x1E00..0x1F80]),
            0xC808..=0xC80B => self.scroll[(a & 3) as usize] = v,
            0xD000..=0xD7FF => self.fg[(a & 0x7FF) as usize] = v,
            0xD800..=0xDFFF => self.bg[(a & 0x7FF) as usize] = v,
            0xE000..=0xFFFF => self.ram[(a & 0x1FFF) as usize] = v,
            _ => {}
        }
    }
    fn port_in(&mut self, _p: u16) -> u8 { 0xFF }
    fn port_out(&mut self, _p: u16, _v: u8) {}
}

struct SoundBus<'a>(&'a mut CommandoBoard);

impl Bus for SoundBus<'_> {
    fn read(&mut self, a: u16) -> u8 {
        let b = &mut *self.0;
        match a {
            0x0000..=0x3FFF => b.snd_rom.get(a as usize).copied().unwrap_or(0xFF),
            0x4000..=0x47FF => b.snd_ram[(a & 0x7FF) as usize],
            0x6000 => b.sound_latch,
            _ => 0xFF,
        }
    }
    fn write(&mut self, a: u16, v: u8) {
        let b = &mut *self.0;
        match a {
            0x4000..=0x47FF => b.snd_ram[(a & 0x7FF) as usize] = v,
            0x8000..=0x8003 => b.ym[(a as usize >> 1) & 1].write(a as u8 & 1, v),
            _ => {}
        }
    }
    fn port_in(&mut self, _p: u16) -> u8 { 0xFF }
    fn port_out(&mut self, _p: u16, _v: u8) {}
}

pub struct CommandoSystem {
    pub main: Z80Cpu,
    pub snd: Z80Cpu,
    pub board: CommandoBoard,
    irq_prom: Vec<u8>,
    colors: Vec<[u8; 3]>,
    chars: Vec<u8>,
    tiles: Vec<u8>,
    sprites: Vec<u8>,
    debt: [i32; 2],
    /// held IRQs: the main CPU's (with its RST vector) and the sound CPU's
    main_irq: Option<u8>,
    snd_irq: bool,
    snd_was_reset: bool,
    pub framebuffer: Vec<u8>,
    current_frame: u64,
    save_filename: String,
    save_path: String,
}

impl CommandoSystem {
    /// `first_encrypted`: the bootlegs encrypt the first byte too (MAME init_spaceinv).
    pub fn new(set_name: &str, mut regions: HashMap<String, Vec<u8>>, first_encrypted: bool) -> Result<Self, String> {
        let mut rom = take_region(&mut regions, "maincpu")?;
        rom.resize(0xC000.max(rom.len()), 0xFF);
        let snd_rom = take_region(&mut regions, "audiocpu")?;
        let chars = take_region(&mut regions, "chars")?;
        let tiles = take_region(&mut regions, "tiles")?;
        let sprites = take_region(&mut regions, "sprites")?;
        let irq_prom = take_region(&mut regions, "irqprom")?;
        let proms = take_region(&mut regions, "proms")?;
        if chars.len() < 0x4000 || tiles.len() < 0x18000 || sprites.len() < 0x18000 || irq_prom.len() < 0x100 || proms.len() < 0x300 {
            return Err("commando: graphics or PROM regions too small".into());
        }
        let mut opcodes: Vec<u8> = rom[..0xC000].iter().map(|&b| decrypt(b)).collect();
        if !first_encrypted { opcodes[0] = rom[0]; }
        let colors = (0..256).map(|i| [(proms[i] & 15) * 17, (proms[0x100 + i] & 15) * 17, (proms[0x200 + i] & 15) * 17]).collect();

        let char_xs = [0, 1, 2, 3, 8, 9, 10, 11];
        let char_ys: Vec<usize> = (0..8).map(|y| y * 16).collect();
        let chars = decode(&chars, 1024, &[4, 0], &char_xs, &char_ys, 128);
        let third = 0x8000 * 8;
        let tile_xs: Vec<usize> = (0..8).chain(128..136).collect();
        let tile_ys: Vec<usize> = (0..16).map(|y| y * 8).collect();
        let tiles = decode(&tiles, 1024, &[0, third, 2 * third], &tile_xs, &tile_ys, 256);
        let half = 0xC000 * 8;
        let spr_xs = [0, 1, 2, 3, 8, 9, 10, 11, 256, 257, 258, 259, 264, 265, 266, 267];
        let spr_ys: Vec<usize> = (0..16).map(|y| y * 16).collect();
        let sprites = decode(&sprites, 768, &[half + 4, half, 4, 0], &spr_xs, &spr_ys, 512);

        let mut ym = [Ym2203::new(YM_CLOCK, 44100), Ym2203::new(YM_CLOCK, 44100)];
        for y in ym.iter_mut() { y.set_mix(0.15, 0.15); }
        let board = CommandoBoard {
            rom,
            opcodes,
            snd_rom,
            ram: vec![0; 0x2000],
            fg: vec![0; 0x800],
            bg: vec![0; 0x800],
            sprite_buf: vec![0; 0x180],
            scroll: [0; 4],
            control: 0,
            sound_latch: 0,
            snd_ram: vec![0; 0x800],
            ym,
            inputs: [0xFF; 3],
            // starting area 0, 3 lives, 1 coin 1 credit; 10K 50K+, demo sounds, normal, upright
            dsw: [0xFF, 0x1F],
        };
        Ok(Self {
            main: Z80Cpu::new(),
            snd: Z80Cpu::new(),
            board,
            irq_prom,
            colors,
            chars,
            tiles,
            sprites,
            debt: [0; 2],
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
        for y in self.board.ym.iter_mut() { y.run(YM_PER_LINE); }
    }

    pub fn render(&mut self) {
        let b = &self.board;
        let mut out = vec![0u8; 256 * 256];
        let sx = ((b.scroll[0] as usize) | (b.scroll[1] as usize) << 8) & 0x1FF;
        let sy = ((b.scroll[2] as usize) | (b.scroll[3] as usize) << 8) & 0x1FF;
        for y in 0..256 {
            let ty = (y + sy) & 0x1FF;
            for x in 0..256 {
                let tx = (x + sx) & 0x1FF;
                let i = (tx >> 4) * 32 + (ty >> 4);   // scanned by columns
                let attr = b.bg[i + 0x400];
                let code = b.bg[i] as usize | ((attr as usize & 0xC0) << 2);
                let cx = if attr & 0x10 != 0 { 15 - (tx & 15) } else { tx & 15 };
                let cy = if attr & 0x20 != 0 { 15 - (ty & 15) } else { ty & 15 };
                out[y * 256 + x] = (attr & 0x0F) * 8 + self.tiles[(code * 16 + cy) * 16 + cx];
            }
        }
        for offs in (0..0x180).step_by(4).rev() {
            let s = &b.sprite_buf[offs..offs + 4];
            let attr = s[1];
            let bank = (attr >> 6) as usize;
            if bank == 3 { continue; }
            let code = s[0] as usize + 256 * bank;
            let x0 = s[3] as i32 - if attr & 1 != 0 { 0x100 } else { 0 };
            let y0 = s[2] as i32;
            let (fx, fy) = (attr & 4 != 0, attr & 8 != 0);
            let color = 128 + ((attr >> 4) & 3) * 16;
            for y in 0..16 {
                let py = y0 + y as i32;
                if !(0..256).contains(&py) { continue; }
                for x in 0..16 {
                    let px = x0 + x as i32;
                    if !(0..256).contains(&px) { continue; }
                    let p = self.sprites[(code * 16 + if fy { 15 - y } else { y }) * 16 + if fx { 15 - x } else { x }];
                    if p != 15 { out[py as usize * 256 + px as usize] = color + p; }
                }
            }
        }
        for ty in 0..32 {
            for tx in 0..32 {
                let i = ty * 32 + tx;
                let attr = b.fg[i + 0x400];
                let code = b.fg[i] as usize | ((attr as usize & 0xC0) << 2);
                let color = 192 + (attr & 0x0F) * 4;
                for y in 0..8 {
                    for x in 0..8 {
                        let cx = if attr & 0x10 != 0 { 7 - x } else { x };
                        let cy = if attr & 0x20 != 0 { 7 - y } else { y };
                        let p = self.chars[(code * 8 + cy) * 8 + cx];
                        if p != 3 { out[(ty * 8 + y) * 256 + tx * 8 + x] = color + p; }
                    }
                }
            }
        }
        let flip = b.flip();
        for y in 0..SCREEN_H {
            for x in 0..SCREEN_W {
                let (fy, fx) = if flip { (255 - (y + FIRST_LINE), 255 - x) } else { (y + FIRST_LINE, x) };
                let c = self.colors[out[fy * 256 + fx] as usize];
                let o = (y * SCREEN_W + x) * 4;
                self.framebuffer[o..o + 4].copy_from_slice(&[c[0], c[1], c[2], 0xFF]);
            }
        }
    }
}

impl System for CommandoSystem {
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
    ///   system: 0x01 start 1, 0x02 start 2, 0x40 coin 2, 0x80 coin 1
    ///   P1 / P2: 0x01 right, 0x02 left, 0x04 down, 0x08 up, 0x10 fire, 0x20 grenade
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
        if on(pad::COIN) || on(pad::SELECT) { sys |= 0x80; }
        self.board.inputs = [!sys, joy(mask), joy(pad::player2(mask))];
    }

    fn drain_audio(&mut self) -> Vec<f32> {
        let [a, b] = &mut self.board.ym;
        let n = a.out.len().min(b.out.len());
        let mut out = Vec::with_capacity(n * 2);
        for k in 0..n {
            let v = a.out[k] + b.out[k];
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
        b.scroll = [0; 4];
        b.control = 0;
        for y in b.ym.iter_mut() { y.reset(); }
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
        let s = (self.main.save_state(), self.snd.save_state(), &self.board, self.debt, self.main_irq, self.snd_irq, self.snd_was_reset, self.current_frame);
        state::encode(&self.save_filename, STATE_VERSION, &s)
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let (main, snd, mut board, debt, main_irq, snd_irq, snd_was_reset, frame): (Z80State, Z80State, CommandoBoard, [i32; 2], Option<u8>, bool, bool, u64) =
            state::decode(&self.save_filename, STATE_VERSION, data)?;
        if board.ram.len() != 0x2000 || board.fg.len() != 0x800 || board.bg.len() != 0x800 || board.sprite_buf.len() != 0x180 || board.snd_ram.len() != 0x800 {
            return Err("save state doesn't fit this board".into());
        }
        board.rom = std::mem::take(&mut self.board.rom);
        board.opcodes = std::mem::take(&mut self.board.opcodes);
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
        self.snd_was_reset = snd_was_reset;
        self.current_frame = frame;
        Ok(())
    }
}

impl CommandoSystem {
    #[cfg(test)]
    pub fn sound_bus(&mut self) -> impl Bus + '_ { SoundBus(&mut self.board) }
}

impl HiScoreMem for CommandoSystem {
    fn hs_read(&mut self, addr: u16) -> u8 { self.board.read(addr) }
    fn hs_write(&mut self, addr: u16, v: u8) { self.board.write(addr, v) }
}
