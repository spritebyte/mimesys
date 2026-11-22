//! Bagman board (Valadon Automation, 1982): Z80 at 3.072 MHz, AY-3-8910 (which also reads
//! the two joysticks), a 32x32 tilemap of 8x8 characters from two character sets, eight
//! 16x16 sprites, a 64-colour PROM palette, and a PAL16R6 that the game reads as a
//! protection check. Hardware facts from MAME's bagman driver (BSD-3-Clause, Nicola
//! Salmoria); the PAL's logic is worked out here from its fuse map.
//!
//! Memory map:
//!   0000-5FFF ROM            6000-67FF RAM
//!   9000-93FF tile codes     9800-9BFF tile attributes (9800-981F: the sprites)
//!   A000 R: PAL16R6          A000-A007 W: latch (IRQ enable, flip X, flip Y, video enable)
//!   A800-A807 W: speech state machine (TMS5110; not emulated yet)
//!   B000 R: DIP switches     B800 R: watchdog (not emulated)
//!   C000-FFFF ROM (Super Bagman)
//! I/O: 08 AY address, 09 AY data, 0C AY data read (port A = player 1, port B = player 2).
//!
//! Speech (the TMS5110 saying "Bagman!", "Oh! You've got me!") isn't emulated yet: the
//! game runs the same without it.

use crate::common::cpu::z80::{Bus, Z80Cpu, Z80State};
use crate::common::audio::ay8910::Ay8910;
use crate::common::input as pad;
use crate::common::system::{DisplayInfo, Rotation, System};
use crate::arcade::hiscore::HiScoreMem;
use crate::arcade::{state, take_region};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const STATE_VERSION: u32 = 1;

// 18.432 MHz crystal: pixel clock / 3 = 6.144 MHz, Z80 at / 6, AY at / 12.
const CPU_CLOCK: u32 = 3_072_000;
const AY_CLOCK: u32 = 1_536_000;
/// 384 pixel clocks a line = 192 CPU cycles; 264 lines a frame (60.6 Hz).
const CYCLES_PER_LINE: i32 = 192;
const LINES: usize = 264;
/// Vertical blank (and the interrupt) starts at line 240; the picture is lines 16-239.
const VBLANK_LINE: usize = 240;
const FIRST_LINE: usize = 16;
pub const SCREEN_W: usize = 256;
pub const SCREEN_H: usize = 224;
/// The AY steps once every 8 of its clocks = 16 CPU cycles.
const CPU_CYCLES_PER_AY_TICK: i32 = 16;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Variant {
    /// Bagman and Le Bagnard: no video-enable latch
    Bagman,
    /// Super Bagman: latch bit 3 blanks the screen, more ROM at C000
    SuperBagman,
}

/// The PAL16R6 at A000. Its inputs are wired to the address bus but the fuse map leaves
/// them unused: six registered outputs (q1-q6) and one combinatorial feedback (o19) run
/// as a counter clocked by each read. With o19 = !(q1 & !q2 & q3 & !q4 & q5 & q6), the
/// next state of each q_k is the inverse of
///   o19 & ((q1 & ... & q_k) | (!q_j & !q_k for some j < k))
/// (for q1: o19 & q1). All outputs are high after reset. The game reads q1-q6 on bits
/// 5-0 (q6 on bit 0).
#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct Pal16r6 {
    q: [bool; 7],    // q[1..=6]
    o19: bool,
}

impl Pal16r6 {
    pub fn new() -> Self { Self { q: [true; 7], o19: true } }

    /// One read: clock the registers, return the outputs.
    pub fn read(&mut self) -> u8 {
        let q = self.q;
        let o19 = self.o19;
        let mut next = q;
        for k in 1..=6 {
            let all = (1..=k).all(|j| q[j]);
            let pair = (1..k).any(|j| !q[j] && !q[k]);
            next[k] = !(o19 && (all || pair));
        }
        self.o19 = !(q[1] && !q[2] && q[3] && !q[4] && q[5] && q[6]);
        self.q = next;
        (1..=6).fold(0u8, |acc, k| acc | (self.q[k] as u8) << (6 - k))
    }
}

impl Default for Pal16r6 {
    fn default() -> Self { Self::new() }
}

#[derive(Serialize, Deserialize)]
pub struct BagmanBus {
    #[serde(skip)]
    rom: Vec<u8>,
    ram: Vec<u8>,
    pub videoram: Vec<u8>,
    pub colorram: Vec<u8>,
    pub ay: Ay8910,
    pal: Pal16r6,
    /// latch A000-A007: bit n = the last value written to A00n
    latch: u8,
    #[serde(skip)]
    pub p1: u8,
    #[serde(skip)]
    pub p2: u8,
    #[serde(skip)]
    pub dsw: u8,
    #[serde(skip)]
    variant: Option<Variant>,
    /// the IRQ line, held from vblank until the game clears the enable
    irq: bool,
}

impl BagmanBus {
    fn irq_enabled(&self) -> bool { self.latch & 1 != 0 }
    fn flip_x(&self) -> bool { self.latch & 2 != 0 }
    fn flip_y(&self) -> bool { self.latch & 4 != 0 }
    fn video_on(&self) -> bool { self.variant != Some(Variant::SuperBagman) || self.latch & 8 != 0 }
}

impl Bus for BagmanBus {
    fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x5FFF | 0xC000..=0xFFFF => self.rom[addr as usize],
            0x6000..=0x67FF => self.ram[(addr & 0x7FF) as usize],
            0x9000..=0x93FF => self.videoram[(addr & 0x3FF) as usize],
            0x9800..=0x9BFF => self.colorram[(addr & 0x3FF) as usize],
            0xA000 => self.pal.read(),
            0xB000 => self.dsw,
            _ => 0xFF,
        }
    }

    fn write(&mut self, addr: u16, val: u8) {
        match addr {
            0x6000..=0x67FF => self.ram[(addr & 0x7FF) as usize] = val,
            0x9000..=0x93FF => self.videoram[(addr & 0x3FF) as usize] = val,
            0x9800..=0x9BFF => self.colorram[(addr & 0x3FF) as usize] = val,
            0xA000..=0xA007 => {
                let bit = 1u8 << (addr & 7);
                if val & 1 != 0 { self.latch |= bit; } else { self.latch &= !bit; }
                if !self.irq_enabled() { self.irq = false; }
            }
            _ => {}
        }
    }

    fn port_in(&mut self, port: u16) -> u8 {
        match port & 0xFF {
            0x0C => self.ay.read_data_ab(self.p1, self.p2),
            _ => 0xFF,
        }
    }

    fn port_out(&mut self, port: u16, val: u8) {
        match port & 0xFF {
            0x08 => self.ay.write_address(val),
            0x09 => { self.ay.write_data(val); }
            _ => {}
        }
    }
}

/// The colour PROM's resistor network: red and green 1 k / 470 / 220 ohm, blue 470 / 220
/// ohm, each with a 470 ohm pull-down, scaled together so the brightest is 255 (MAME
/// compute_resistor_weights).
fn palette(prom: &[u8]) -> [[u8; 3]; 64] {
    let rg = [1000.0f64, 470.0, 220.0];
    let b = [470.0f64, 220.0];
    let weights = |rs: &[f64]| -> Vec<f64> {
        let total: f64 = rs.iter().map(|r| 1.0 / r).sum::<f64>() + 1.0 / 470.0;
        rs.iter().map(|r| (1.0 / r) / total).collect()
    };
    let (wrg, wb) = (weights(&rg), weights(&b));
    let max = wrg.iter().sum::<f64>().max(wb.iter().sum::<f64>());
    let scale = 255.0 / max;
    let combine = |w: &[f64], bits: u8| -> u8 {
        let v: f64 = w.iter().enumerate().filter(|(i, _)| bits >> i & 1 != 0).map(|(_, x)| x * scale).sum();
        (v + 0.5).min(255.0) as u8
    };
    let mut out = [[0u8; 3]; 64];
    for (i, c) in out.iter_mut().enumerate() {
        let p = prom.get(i).copied().unwrap_or(0);
        *c = [combine(&wrg, p & 7), combine(&wrg, p >> 3 & 7), combine(&wb, p >> 6 & 3)];
    }
    out
}

/// 2-bit planar graphics: two bitplanes `half` bytes apart; the first is the high bit.
fn pixel(gfx: &[u8], half: usize, byte: usize, x: usize) -> u8 {
    let hi = gfx.get(byte).copied().unwrap_or(0) >> (7 - x) & 1;
    let lo = gfx.get(byte + half).copied().unwrap_or(0) >> (7 - x) & 1;
    hi << 1 | lo
}

/// 512 characters of 8x8 from a 0x2000-byte set.
fn decode_chars(gfx: &[u8]) -> Vec<[u8; 64]> {
    (0..512).map(|c| {
        let mut t = [0u8; 64];
        for y in 0..8 { for x in 0..8 { t[y * 8 + x] = pixel(gfx, 0x1000, c * 8 + y, x); } }
        t
    }).collect()
}

/// 128 sprites of 16x16 from the same set: 32 bytes each, in four 8x8 quarters (left
/// top, right top, left bottom, right bottom).
fn decode_sprites(gfx: &[u8]) -> Vec<[u8; 256]> {
    (0..128).map(|s| {
        let mut t = [0u8; 256];
        for y in 0..16 {
            for x in 0..16 {
                let byte = s * 32 + (y & 7) + if x >= 8 { 8 } else { 0 } + if y >= 8 { 16 } else { 0 };
                t[y * 16 + x] = pixel(gfx, 0x1000, byte, x & 7);
            }
        }
        t
    }).collect()
}

pub struct BagmanSystem {
    pub cpu: Z80Cpu,
    pub bus: BagmanBus,
    chars: [Vec<[u8; 64]>; 2],
    sprites: Vec<[u8; 256]>,
    colors: [[u8; 3]; 64],
    /// MAME ROT90 (the Itisa boards) instead of ROT270
    rot90: bool,
    ay_cycles: i32,
    /// cycles the last frame ran past its end
    carry: i32,
    pub framebuffer: Vec<u8>,
    current_frame: u64,
    save_filename: String,
    save_path: String,
}

impl BagmanSystem {
    pub fn new(variant: Variant, set_name: &str, mut regions: HashMap<String, Vec<u8>>) -> Result<Self, String> {
        let mut rom = take_region(&mut regions, "maincpu")?;
        rom.resize(0x10000, 0xFF);
        let gfx1 = take_region(&mut regions, "gfx1")?;
        let gfx2 = take_region(&mut regions, "gfx2")?;
        let proms = take_region(&mut regions, "proms")?;
        if gfx1.len() < 0x2000 || gfx2.len() < 0x2000 || proms.len() < 0x40 {
            return Err("bagman: graphics or colour PROMs too small".into());
        }
        // the Spanish (Itisa) boards are mounted the other way round
        let rot90 = set_name.starts_with("bagnardi");
        let bus = BagmanBus {
            rom,
            ram: vec![0; 0x800],
            videoram: vec![0; 0x400],
            colorram: vec![0; 0x400],
            ay: Ay8910::new(AY_CLOCK, 44100),
            pal: Pal16r6::new(),
            latch: 0,
            p1: 0xFF,
            p2: 0xFF,
            // 3 lives, 1C/1C, easy, English, bonus at 30000, upright
            dsw: 0xFE,
            variant: Some(variant),
            irq: false,
        };
        Ok(Self {
            cpu: Z80Cpu::new(),
            bus,
            chars: [decode_chars(&gfx1), decode_chars(&gfx2)],
            sprites: decode_sprites(&gfx1),
            colors: palette(&proms),
            rot90,
            ay_cycles: 0,
            carry: 0,
            framebuffer: vec![0; SCREEN_W * SCREEN_H * 4],
            current_frame: 0,
            save_filename: set_name.to_string(),
            save_path: "user://GD_EMU/Arcade/Valadon/State".to_string(),
        })
    }

    fn run_cycles(&mut self, n: i32) {
        let mut done = -self.carry;
        while done < n {
            let mut c = 0;
            if self.bus.irq { c = self.cpu.irq(&mut self.bus, 0xFF) as i32; }
            if c == 0 { c = self.cpu.step(&mut self.bus) as i32; }
            done += c;
            self.ay_cycles += c;
            while self.ay_cycles >= CPU_CYCLES_PER_AY_TICK {
                self.ay_cycles -= CPU_CYCLES_PER_AY_TICK;
                self.bus.ay.tick();
            }
        }
        self.carry = done - n;
    }

    /// The picture: the tilemap (opaque), then the sprites, last in the list on top.
    pub fn render(&mut self) {
        let b = &self.bus;
        let mut pens = vec![0u8; 256 * 256];
        if b.video_on() {
            for ty in 0..32 {
                for tx in 0..32 {
                    let i = ty * 32 + tx;
                    let attr = b.colorram[i];
                    let set = (attr >> 4 & 1) as usize;
                    let code = b.videoram[i] as usize + if attr & 0x20 != 0 { 256 } else { 0 };
                    let color = (attr & 0x0F) * 4;
                    let tile = &self.chars[set][code & 511];
                    for y in 0..8 {
                        for x in 0..8 {
                            pens[(ty * 8 + y) * 256 + tx * 8 + x] = color + tile[y * 8 + x];
                        }
                    }
                }
            }
            if b.flip_x() { for row in pens.chunks_mut(256) { row.reverse(); } }
            if b.flip_y() {
                for y in 0..128 {
                    let (top, bottom) = pens.split_at_mut((255 - y) * 256);
                    top[y * 256..y * 256 + 256].swap_with_slice(&mut bottom[..256]);
                }
            }
            for offs in (0..0x20).step_by(4).rev() {
                let s = &b.colorram[offs..offs + 4];
                if s[2] == 0 || s[3] == 0 { continue; }
                let mut sx = s[3] as i32;
                let mut sy = 256 - s[2] as i32 - 16;
                let (mut fx, mut fy) = (s[0] & 0x40 != 0, s[0] & 0x80 != 0);
                if b.flip_x() {
                    sx = 256 - sx - 15;
                    sy = 255 - sy - 15;
                    fx = !fx;
                    fy = !fy;
                }
                let code = ((s[0] & 0x3F) as usize + if s[1] & 0x20 != 0 { 64 } else { 0 }) & 127;
                let color = (s[1] & 0x1F) * 4;
                let spr = &self.sprites[code];
                for y in 0..16 {
                    let py = sy + y as i32;
                    if !(0..256).contains(&py) { continue; }
                    for x in 0..16 {
                        let px = sx + x as i32;
                        if !(0..256).contains(&px) { continue; }
                        let p = spr[(if fy { 15 - y } else { y }) * 16 + if fx { 15 - x } else { x }];
                        if p != 0 { pens[py as usize * 256 + px as usize] = color.wrapping_add(p) & 63; }
                    }
                }
            }
        }
        for y in 0..SCREEN_H {
            for x in 0..SCREEN_W {
                let c = self.colors[(pens[(y + FIRST_LINE) * 256 + x] & 63) as usize];
                let o = (y * SCREEN_W + x) * 4;
                self.framebuffer[o..o + 4].copy_from_slice(&[c[0], c[1], c[2], 0xFF]);
            }
        }
    }
}

impl System for BagmanSystem {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        self.run_cycles(VBLANK_LINE as i32 * CYCLES_PER_LINE);
        self.render();
        if self.bus.irq_enabled() { self.bus.irq = true; }
        self.run_cycles((LINES - VBLANK_LINE) as i32 * CYCLES_PER_LINE);
        self.current_frame += 1;
    }

    /// Active low, through the AY's ports.
    ///   P1 / P2: 0x01 coin, 0x02 coin 2, 0x04 start, 0x08 left, 0x10 right, 0x20 up,
    ///            0x40 down, 0x80 button (Super Bagman: start doubles as button 2)
    fn set_input(&mut self, mask: u32) {
        let port = |m: u32, coin: bool, start: bool| -> u8 {
            let on = |b: u32| m & b != 0;
            let mut v = 0u8;
            if coin { v |= 0x01; }
            if start { v |= 0x04; }
            if on(pad::LEFT) { v |= 0x08; }
            if on(pad::RIGHT) { v |= 0x10; }
            if on(pad::UP) { v |= 0x20; }
            if on(pad::DOWN) { v |= 0x40; }
            if on(pad::A) || on(pad::B) { v |= 0x80; }
            !v
        };
        let on = |b: u32| mask & b != 0;
        let super_b = self.bus.variant == Some(Variant::SuperBagman);
        self.bus.p1 = port(mask, on(pad::COIN) || on(pad::SELECT), on(pad::START) || (super_b && on(pad::X)));
        let m2 = pad::player2(mask);
        self.bus.p2 = port(m2, false, on(pad::START2) || (super_b && m2 & pad::X != 0));
    }

    fn drain_audio(&mut self) -> Vec<f32> { std::mem::take(&mut self.bus.ay.out) }
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
        self.cpu = Z80Cpu::new();
        self.bus.ay.reset();
        self.bus.latch = 0;
        self.bus.irq = false;
        self.bus.pal = Pal16r6::new();
    }

    fn frame_time_usec(&self) -> i64 {
        // 6.144 MHz / (384 x 264)
        (384.0 * 264.0 / 6.144) as i64
    }

    fn display_info(&self) -> DisplayInfo {
        DisplayInfo {
            buffer_width: SCREEN_W as i32,
            buffer_height: SCREEN_H as i32,
            visible_x: 0,
            visible_y: 0,
            visible_width: SCREEN_W as i32,
            visible_height: SCREEN_H as i32,
            target_aspect_ratio: 3.0 / 4.0,   // after rotation
            rotation: if self.rot90 { Rotation::CW90 } else { Rotation::CCW90 },
        }
    }

    fn save_state(&self) -> Vec<u8> {
        let s = (self.cpu.save_state(), &self.bus, self.ay_cycles, self.carry, self.current_frame);
        state::encode(&self.save_filename, STATE_VERSION, &s)
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let (cpu, mut bus, ay_cycles, carry, frame): (Z80State, BagmanBus, i32, i32, u64) =
            state::decode(&self.save_filename, STATE_VERSION, data)?;
        if bus.ram.len() != 0x800 || bus.videoram.len() != 0x400 || bus.colorram.len() != 0x400 {
            return Err("save state doesn't fit this board".into());
        }
        bus.rom = std::mem::take(&mut self.bus.rom);
        bus.p1 = self.bus.p1;
        bus.p2 = self.bus.p2;
        bus.dsw = self.bus.dsw;
        bus.variant = self.bus.variant;
        bus.ay.out = std::mem::take(&mut self.bus.ay.out);
        self.bus = bus;
        self.cpu.load_state(&cpu);
        self.ay_cycles = ay_cycles;
        self.carry = carry;
        self.current_frame = frame;
        Ok(())
    }
}

impl HiScoreMem for BagmanSystem {
    fn hs_read(&mut self, addr: u32) -> u8 { self.bus.read(addr as u16) }
    fn hs_write(&mut self, addr: u32, v: u8) { self.bus.write(addr as u16, v) }
}
