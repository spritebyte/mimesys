//! Capcom's Ghosts'n Goblins board (1985), and Diamond Run (1989) on the same board.
//! Hardware facts from MAME's gng driver (BSD-3-Clause, Pierpaolo Prazzoli).
//!
//! - Main CPU: MC6809 at 1.5 MHz (12 MHz / 8), IRQ at vblank (held until taken).
//! - Sound CPU: Z80 at 3 MHz, IRQ four times a frame; two YM2203 at 1.5 MHz.
//! - Video: 6 MHz pixel clock, 384 x 262 (59.64 Hz), picture lines 16-239.
//!   - 8x8 text layer (2 bits, pen 3 see-through), colours 0x80-0xBF
//!   - 16x16 scrolling background (3 bits, 512 x 512 wrapping), colours 0x00-0x3F; a tile
//!     attribute bit puts all but pens 0 and 6 in front of the sprites
//!   - 128 sprites of 16x16 (4 bits, pen 15 see-through) from a buffer the game fills by
//!     writing 3C00, colours 0x40-0x7F
//!   - 256 colours, RGB 4:4:4 split over two byte arrays
//!
//! Main CPU map:
//!   0000-1DFF RAM   1E00-1FFF sprite RAM   2000-27FF text   2800-2FFF background
//!   3000-3004 R: system, P1, P2, DIP 1, DIP 2
//!   3800-38FF / 3900-39FF W: palette (red-green / blue)   3A00 W: sound latch
//!   3B08-3B0B W: background scroll X, Y   3C00 W: copy sprites to the buffer
//!   3D00-3D07 W: latch (0 flip off, 1 sound CPU and YMs out of reset, 2-3 coin counters)
//!   3E00 W: bank   4000-5FFF banked ROM   6000-FFFF ROM
//! Sound CPU map: 0000-7FFF ROM, C000-C7FF RAM, C800 R latch, E000-E003 W the YM2203s.

use crate::common::audio::ym2203::Ym2203;
use crate::common::cpu::m6809::{M6809, M6809Bus, CC_I};
use crate::common::cpu::z80::{Bus, Z80Cpu, Z80State};
use crate::common::input as pad;
use crate::common::system::{DisplayInfo, Rotation, System};
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
/// per line: 384 pixels at 6 MHz = 64 us
const MAIN_PER_LINE: i32 = 96;     // 1.5 MHz
const SOUND_PER_LINE: i32 = 192;   // 3 MHz
const YM_PER_LINE: u32 = 96;       // 1.5 MHz
const YM_CLOCK: u32 = 1_500_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Variant {
    Gng,
    /// no latch at 3D00 (the sound CPU always runs), 6000 reads 0
    DiamondRun,
}

/// MAME gfx_layout decode: `planes`, `xs`, `ys` as bit offsets (the first plane is the
/// high bit of a pixel), `inc` bits from one element to the next.
fn decode(gfx: &[u8], count: usize, planes: &[usize], xs: &[usize], ys: &[usize], inc: usize) -> Vec<u8> {
    let (w, h) = (xs.len(), ys.len());
    let bit = |b: usize| gfx.get(b / 8).map_or(0, |v| v >> (7 - b % 8) & 1);
    let mut out = vec![0u8; count * w * h];
    for e in 0..count {
        for (y, &yo) in ys.iter().enumerate() {
            for (x, &xo) in xs.iter().enumerate() {
                let mut p = 0;
                for &po in planes { p = p << 1 | bit(e * inc + po + yo + xo); }
                out[(e * h + y) * w + x] = p;
            }
        }
    }
    out
}

#[derive(Serialize, Deserialize)]
pub struct GngBoard {
    #[serde(skip)]
    rom: Vec<u8>,
    #[serde(skip)]
    snd_rom: Vec<u8>,
    ram: Vec<u8>,          // 0000-1FFF (sprites at 1E00)
    fg: Vec<u8>,           // 2000-27FF
    bg: Vec<u8>,           // 2800-2FFF
    pal_rg: Vec<u8>,
    pal_b: Vec<u8>,
    sprite_buf: Vec<u8>,
    scroll: [u8; 4],       // x lo, x hi, y lo, y hi
    latch: u8,
    bank: u8,
    sound_latch: u8,
    snd_ram: Vec<u8>,
    pub ym: [Ym2203; 2],
    #[serde(skip)]
    pub inputs: [u8; 3],
    #[serde(skip)]
    pub dsw: [u8; 2],
    #[serde(skip)]
    variant: Option<Variant>,
}

impl GngBoard {
    fn flip(&self) -> bool { self.variant == Some(Variant::Gng) && self.latch & 1 == 0 }
    fn sound_running(&self) -> bool { self.variant != Some(Variant::Gng) || self.latch & 2 != 0 }

    fn banked(&self, a: u16) -> u8 {
        let off = if self.bank == 4 { 0x4000 } else { 0x10000 + (self.bank as usize & 3) * 0x2000 };
        self.rom.get(off + (a as usize & 0x1FFF)).copied().unwrap_or(0xFF)
    }

    fn color(&self, i: usize) -> [u8; 3] {
        let (rg, b) = (self.pal_rg[i], self.pal_b[i]);
        [(rg >> 4) * 17, (rg & 15) * 17, (b >> 4) * 17]
    }

    /// The power-on palette (MAME: black, dark grey, light grey, white over and over).
    fn reset_palette(&mut self) {
        for i in 0..256 {
            let v = [0x00, 0x55, 0xAA, 0xFF][i & 3];
            self.pal_rg[i] = v;
            self.pal_b[i] = v;
        }
    }
}

impl M6809Bus for GngBoard {
    fn read(&mut self, a: u16) -> u8 {
        match a {
            0x0000..=0x1FFF => self.ram[a as usize],
            0x2000..=0x27FF => self.fg[(a & 0x7FF) as usize],
            0x2800..=0x2FFF => self.bg[(a & 0x7FF) as usize],
            0x3000..=0x3002 => self.inputs[(a & 3) as usize],
            0x3003 | 0x3004 => self.dsw[(a - 0x3003) as usize],
            0x4000..=0x5FFF => self.banked(a),
            0x6000 if self.variant == Some(Variant::DiamondRun) => 0,
            0x6000..=0xFFFF => self.rom[a as usize],
            _ => 0,
        }
    }

    fn write(&mut self, a: u16, v: u8) {
        match a {
            0x0000..=0x1FFF => self.ram[a as usize] = v,
            0x2000..=0x27FF => self.fg[(a & 0x7FF) as usize] = v,
            0x2800..=0x2FFF => self.bg[(a & 0x7FF) as usize] = v,
            0x3800..=0x38FF => self.pal_rg[(a & 0xFF) as usize] = v,
            0x3900..=0x39FF => self.pal_b[(a & 0xFF) as usize] = v,
            0x3A00 => self.sound_latch = v,
            0x3B08..=0x3B0B => self.scroll[(a & 3) as usize] = v,
            0x3C00 => self.sprite_buf.copy_from_slice(&self.ram[0x1E00..0x2000]),
            0x3D00..=0x3D07 if self.variant == Some(Variant::Gng) => {
                let bit = 1u8 << (a & 7);
                let was_running = self.sound_running();
                if v & 1 != 0 { self.latch |= bit; } else { self.latch &= !bit; }
                if was_running && !self.sound_running() {
                    for y in self.ym.iter_mut() { y.reset(); }
                }
            }
            0x3E00 => self.bank = if v == 4 { 4 } else { v & 3 },
            _ => {}
        }
    }
}

/// The sound CPU's view.
struct SoundBus<'a>(&'a mut GngBoard);

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

pub struct GngSystem {
    pub main: M6809,
    pub snd: Z80Cpu,
    pub board: GngBoard,
    chars: Vec<u8>,
    tiles: Vec<u8>,
    sprites: Vec<u8>,
    debt: [i32; 2],
    /// the sound CPU's interrupt, held until taken
    snd_irq: bool,
    /// the sound CPU was held in reset
    snd_was_reset: bool,
    pub framebuffer: Vec<u8>,
    current_frame: u64,
    save_filename: String,
    save_path: String,
}

impl GngSystem {
    pub fn new(variant: Variant, set_name: &str, mut regions: HashMap<String, Vec<u8>>) -> Result<Self, String> {
        let mut rom = take_region(&mut regions, "maincpu")?;
        if rom.len() < 0x18000 { rom.resize(0x18000, 0xFF); }
        let snd_rom = take_region(&mut regions, "audiocpu")?;
        let chars = take_region(&mut regions, "chars")?;
        let tiles = take_region(&mut regions, "tiles")?;
        let sprites = take_region(&mut regions, "sprites")?;
        if chars.len() < 0x4000 || tiles.len() < 0x18000 || sprites.len() < 0x20000 {
            return Err("gng: graphics regions too small".into());
        }
        let char_xs = [0, 1, 2, 3, 8, 9, 10, 11];
        let char_ys: Vec<usize> = (0..8).map(|y| y * 16).collect();
        let chars = decode(&chars, 1024, &[4, 0], &char_xs, &char_ys, 128);
        let third = 0x8000 * 8;
        let tile_xs: Vec<usize> = (0..8).chain(128..136).collect();
        let tile_ys: Vec<usize> = (0..16).map(|y| y * 8).collect();
        let tiles = decode(&tiles, 1024, &[2 * third, third, 0], &tile_xs, &tile_ys, 256);
        let half = 0x10000 * 8;
        let spr_xs = [0, 1, 2, 3, 8, 9, 10, 11, 256, 257, 258, 259, 264, 265, 266, 267];
        let spr_ys: Vec<usize> = (0..16).map(|y| y * 16).collect();
        let sprites = decode(&sprites, 1024, &[half + 4, half, 4, 0], &spr_xs, &spr_ys, 512);

        let mut board = GngBoard {
            rom,
            snd_rom,
            ram: vec![0; 0x2000],
            fg: vec![0; 0x800],
            bg: vec![0; 0x800],
            pal_rg: vec![0; 256],
            pal_b: vec![0; 256],
            sprite_buf: vec![0; 0x200],
            scroll: [0; 4],
            latch: 0,
            bank: 0,
            sound_latch: 0,
            snd_ram: vec![0; 0x800],
            ym: [Ym2203::new(YM_CLOCK, 44100), Ym2203::new(YM_CLOCK, 44100)],
            inputs: [0xFF; 3],
            // 1 coin 1 credit, demo sounds on, no flip; 3 lives, upright, 20K 70K, normal
            dsw: [0xDF, 0xFB],
            variant: Some(variant),
        };
        board.reset_palette();
        let mut main = M6809::new();
        main.reset(&mut board);
        Ok(Self {
            main,
            snd: Z80Cpu::new(),
            board,
            chars,
            tiles,
            sprites,
            debt: [0; 2],
            snd_irq: false,
            snd_was_reset: true,
            framebuffer: vec![0; SCREEN_W * SCREEN_H * 4],
            current_frame: 0,
            save_filename: set_name.to_string(),
            save_path: "user://GD_EMU/Arcade/Capcom/State".to_string(),
        })
    }

    fn run_line(&mut self) {
        // main CPU; a held IRQ goes away once taken
        self.debt[0] += MAIN_PER_LINE;
        while self.debt[0] > 0 {
            let taking = self.main.irq_line && self.main.cc & CC_I == 0;
            self.debt[0] -= self.main.step(&mut self.board) as i32;
            if taking { self.main.set_irq(false); }
        }
        // sound CPU
        if self.board.sound_running() {
            if self.snd_was_reset {
                self.snd = Z80Cpu::new();
                self.snd_irq = false;
                self.snd_was_reset = false;
            }
            self.debt[1] += SOUND_PER_LINE;
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
        // background pens in two passes: behind the sprites, and the front half
        let mut px = vec![0u8; 256 * 256];
        let mut front = vec![false; 256 * 256];
        let sx = ((b.scroll[0] as usize) | (b.scroll[1] as usize) << 8) & 0x1FF;
        let sy = ((b.scroll[2] as usize) | (b.scroll[3] as usize) << 8) & 0x1FF;
        for y in 0..256 {
            let ty = (y + sy) & 0x1FF;
            for x in 0..256 {
                let tx = (x + sx) & 0x1FF;
                let i = (tx >> 4) * 32 + (ty >> 4);   // tilemap scanned by columns
                let attr = b.bg[i + 0x400];
                let code = b.bg[i] as usize | ((attr as usize & 0xC0) << 2);
                let (mut cx, mut cy) = (tx & 15, ty & 15);
                if attr & 0x10 != 0 { cx = 15 - cx; }
                if attr & 0x20 != 0 { cy = 15 - cy; }
                let pen = self.tiles[(code * 16 + cy) * 16 + cx];
                px[y * 256 + x] = (attr & 7) * 8 + pen;
                front[y * 256 + x] = attr & 0x08 != 0 && pen != 0 && pen != 6;
            }
        }
        let mut out = px.clone();
        // sprites, the first in the list on top
        for offs in (0..0x200).step_by(4).rev() {
            let s = &b.sprite_buf[offs..offs + 4];
            let attr = s[1];
            let code = s[0] as usize | ((attr as usize) << 2 & 0x300);
            let x0 = s[3] as i32 - if attr & 1 != 0 { 0x100 } else { 0 };
            let y0 = s[2] as i32;
            let (fx, fy) = (attr & 4 != 0, attr & 8 != 0);
            let color = 0x40 + ((attr >> 4) & 3) * 16;
            for y in 0..16 {
                let py = y0 + y as i32;
                if !(0..256).contains(&py) { continue; }
                for x in 0..16 {
                    let pxx = x0 + x as i32;
                    if !(0..256).contains(&pxx) { continue; }
                    let p = self.sprites[(code * 16 + if fy { 15 - y } else { y }) * 16 + if fx { 15 - x } else { x }];
                    if p != 15 { out[py as usize * 256 + pxx as usize] = color + p; }
                }
            }
        }
        // the background's front half, then the text layer
        for i in 0..256 * 256 { if front[i] { out[i] = px[i]; } }
        for ty in 0..32 {
            for tx in 0..32 {
                let i = ty * 32 + tx;
                let attr = b.fg[i + 0x400];
                let code = b.fg[i] as usize | ((attr as usize & 0xC0) << 2);
                let color = 0x80 + (attr & 0x0F) * 4;
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
                let (fy, fx) = (y + FIRST_LINE, x);
                let (fy, fx) = if flip { (255 - fy, 255 - fx) } else { (fy, fx) };
                let c = b.color(out[fy * 256 + fx] as usize);
                let o = (y * SCREEN_W + x) * 4;
                self.framebuffer[o..o + 4].copy_from_slice(&[c[0], c[1], c[2], 0xFF]);
            }
        }
    }
}

impl System for GngSystem {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        for line in 0..LINES {
            // the sound CPU's timer: four times a frame
            if line % 66 == 0 { self.snd_irq = true; }
            if line == VBLANK_LINE {
                self.render();
                self.main.set_irq(true);
            }
            self.run_line();
        }
        self.current_frame += 1;
    }

    /// Active low.
    ///   system: 0x01 start 1, 0x02 start 2, 0x20 service, 0x40 coin 1, 0x80 coin 2
    ///   P1 / P2: 0x01 right, 0x02 left, 0x04 down, 0x08 up, 0x10 fire, 0x20 jump
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
        if on(pad::X) { sys |= 0x20; }
        if on(pad::COIN) || on(pad::SELECT) { sys |= 0x40; }
        self.board.inputs = [!sys, joy(mask), joy(pad::player2(mask))];
    }

    fn drain_audio(&mut self) -> Vec<f32> {
        let [a, b] = &mut self.board.ym;
        let n = a.out.len().min(b.out.len());
        let mut out = Vec::with_capacity(n * 2);
        for k in 0..n {
            let v = (a.out[k] + b.out[k]) * 0.5;
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
        let b = &mut self.board;
        b.scroll = [0; 4];
        b.latch = 0;
        b.bank = 0;
        b.reset_palette();
        for y in b.ym.iter_mut() { y.reset(); }
        self.main = M6809::new();
        self.main.reset(&mut self.board);
        self.snd_was_reset = true;
        self.snd_irq = false;
    }

    fn frame_time_usec(&self) -> i64 {
        // 6 MHz / (384 x 262)
        (384.0 * 262.0 / 6.0) as i64
    }

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
        let s = (&self.main, self.snd.save_state(), &self.board, self.debt, self.snd_irq, self.snd_was_reset, self.current_frame);
        state::encode(&self.save_filename, STATE_VERSION, &s)
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let (main, snd, mut board, debt, snd_irq, snd_was_reset, frame): (M6809, Z80State, GngBoard, [i32; 2], bool, bool, u64) =
            state::decode(&self.save_filename, STATE_VERSION, data)?;
        if board.ram.len() != 0x2000 || board.fg.len() != 0x800 || board.bg.len() != 0x800 || board.sprite_buf.len() != 0x200
            || board.pal_rg.len() != 256 || board.pal_b.len() != 256 || board.snd_ram.len() != 0x800 {
            return Err("save state doesn't fit this board".into());
        }
        board.rom = std::mem::take(&mut self.board.rom);
        board.snd_rom = std::mem::take(&mut self.board.snd_rom);
        board.inputs = self.board.inputs;
        board.dsw = self.board.dsw;
        board.variant = self.board.variant;
        for k in 0..2 { board.ym[k].out = std::mem::take(&mut self.board.ym[k].out); }
        self.board = board;
        self.main = main;
        self.snd.load_state(&snd);
        self.debt = debt;
        self.snd_irq = snd_irq;
        self.snd_was_reset = snd_was_reset;
        self.current_frame = frame;
        Ok(())
    }
}

impl GngSystem {
    /// The sound CPU's bus (tests).
    #[cfg(test)]
    pub fn sound_bus(&mut self) -> impl Bus + '_ { SoundBus(&mut self.board) }
}

impl HiScoreMem for GngSystem {
    fn hs_read(&mut self, addr: u16) -> u8 { self.board.read(addr) }
    fn hs_write(&mut self, addr: u16, v: u8) { self.board.write(addr, v) }
}
