//! Williams first-generation 6809 boards: Defender, Stargate, Robotron, Joust, Bubbles,
//! Splat and Sinistar. After MAME williams.cpp / williams_m.cpp / williams_v.cpp
//! (Aaron Giles, BSD-3-Clause).
//!
//! A 1 MHz 6809 draws into a 4-bit-per-pixel bitmap that fills 0000-97FF of a 48 KB RAM.
//! The bitmap is column-major: byte (x/2)*256 + y holds pixels x and x+1 (high nibble
//! first). 16 palette registers (BBGGGRRR). Two PIAs: inputs and the scanline interrupts
//! (CB1 = VA11, toggling every 32 lines; CA1 = line >= 240), coins and the sound command.
//! A battery-backed CMOS RAM holds settings, audits and high scores.
//!
//! Two memory maps:
//! - Stargate and later: C900 bit 0 banks ROM over 0000-8FFF for reads; I/O at C000-CFFF
//!   (palette C000, PIAs C804 / C80C, blitter CA00, video counter CB00, CMOS CC00);
//!   ROM D000-FFFF (Sinistar: D000-DFFF is RAM).
//! - Defender: writes to D000-DFFF pick what C000-CFFF shows: bank 0 = I/O (palette C000,
//!   CMOS C400, video counter C800, PIAs CC00 / CC04), banks 1-9 = 4 KB ROM pages.
//!
//! The blitter ("special chip") came with Robotron: SC1 (sizes XOR 4) on most, SC2 on
//! Splat. Screen: 8 MHz pixel clock, 512 x 260 total, 292 x 240 visible, 64 CPU cycles a
//! line, ~60.1 frames a second.

use std::collections::HashMap;
use serde::{Deserialize, Serialize};

use crate::arcade::{state, take_region};
use crate::arcade::williams::williams_blitter::{Blitter, BlitterBus};
use crate::arcade::williams::williams_sound::WilliamsSound;
use crate::common::cpu::m6809::{M6809, M6809Bus};
use crate::common::input as pad;
use crate::common::io::pia6821::{Pia6821, PiaOut};
use crate::common::system::{DisplayInfo, Rotation, System};

const STATE_VERSION: u32 = 2;
const CYCLES_PER_LINE: i32 = 64;
const LINES_PER_FRAME: u16 = 260;
const LINE_RATE: u64 = 15_625;
const VIS_Y: u16 = 7;
pub const SCREEN_W: usize = 292;
pub const SCREEN_H: usize = 240;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Game { Defender, Stargate, Robotron, Joust, Bubbles, Splat, Sinistar }

impl Game {
    pub fn from_driver(d: &str) -> Option<Self> {
        Some(match d {
            "defender" => Game::Defender,
            "stargate" => Game::Stargate,
            "robotron" => Game::Robotron,
            "joust" => Game::Joust,
            "bubbles" => Game::Bubbles,
            "splat" => Game::Splat,
            "sinistar" => Game::Sinistar,
            _ => return None,
        })
    }
    fn blitter(self) -> Option<Blitter> {
        match self {
            Game::Defender | Game::Stargate => None,
            Game::Splat => Some(Blitter::sc2(0xC000)),
            Game::Sinistar => Some(Blitter::sc1(0x7400)),
            _ => Some(Blitter::sc1(0xC000)),
        }
    }
    fn vis_x(self) -> usize { if self == Game::Defender { 12 } else { 6 } }
}

pub struct WilliamsBus {
    game: Game,
    rom: Vec<u8>,
    /// Defender: the 4 KB pages behind banks 1-9
    banked: Vec<u8>,
    pub ram: Vec<u8>,
    /// Sinistar: D000-DFFF
    ram_d000: Vec<u8>,
    palette_ram: [u8; 16],
    pub cmos: Vec<u8>,
    pub cmos_dirty: bool,
    rom_enabled: bool,
    /// Defender C000-CFFF page
    bank: u8,
    pia: [Pia6821; 2],
    blitter: Option<Blitter>,
    pub sound: WilliamsSound,
    /// cycles the blitter held the CPU for, not yet counted
    stall: u32,
    scanline: u16,
    // inputs (set_input), per player: stick (UP DOWN LEFT RIGHT bits as in pad::),
    // buttons, right stick
    input: [u32; 2],
    starts: (bool, bool),
    in2: u8,
}

#[derive(Serialize, Deserialize)]
struct BusState {
    ram: Vec<u8>,
    ram_d000: Vec<u8>,
    palette_ram: [u8; 16],
    cmos: Vec<u8>,
    rom_enabled: bool,
    bank: u8,
    pia: [Pia6821; 2],
    blitter: Option<Blitter>,
    sound: WilliamsSound,
}

/// UP/DOWN/LEFT/RIGHT as 1/2/4/8 (Robotron, Bubbles, Splat order).
fn udlr(up: bool, down: bool, left: bool, right: bool) -> u8 {
    up as u8 | (down as u8) << 1 | (left as u8) << 2 | (right as u8) << 3
}
fn left_stick(m: u32) -> u8 { udlr(m & pad::UP != 0, m & pad::DOWN != 0, m & pad::LEFT != 0, m & pad::RIGHT != 0) }
fn right_stick(m: u32) -> u8 { udlr(m & pad::RS_UP != 0, m & pad::RS_DOWN != 0, m & pad::RS_LEFT != 0, m & pad::RS_RIGHT != 0) }
fn fire(m: u32) -> bool { m & (pad::A | pad::B) != 0 }

impl WilliamsBus {
    fn starts(&self, s1: u8, s2: u8) -> u8 {
        (if self.starts.0 { s1 } else { 0 }) | if self.starts.1 { s2 } else { 0 }
    }

    /// PIA 0 port A / port B pins.
    fn in0_in1(&self) -> (u8, u8) {
        let p1 = self.input[0];
        match self.game {
            Game::Defender | Game::Stargate => {
                let on = |b: u32| p1 & b != 0;
                let mut a = self.starts(0x20, 0x10);
                if on(pad::A) { a |= 0x01; }                      // fire
                if on(pad::B) { a |= 0x02; }                      // thrust
                if on(pad::X) { a |= 0x04; }                      // smart bomb
                if on(pad::Y) { a |= 0x08; }                      // hyperspace
                if on(pad::Z) { a |= 0x40; }                      // reverse
                if on(pad::DOWN) { a |= 0x80; }
                let mut b = on(pad::UP) as u8;
                if self.game == Game::Stargate && on(pad::R) { b |= 0x02; }   // inviso
                (a, b)
            }
            Game::Robotron => {
                let rs = right_stick(p1);
                (left_stick(p1) | self.starts(0x10, 0x20) | (rs & 3) << 6, rs >> 2)
            }
            Game::Bubbles => (left_stick(p1) | self.starts(0x20, 0x10), 0),
            Game::Joust | Game::Splat => {
                // two LS157s on PIA 0 CB2: high = player 1's controls, low = player 2's
                let m = if self.pia[0].cb2() { p1 } else { self.input[1] };
                let (lo, hi) = if self.game == Game::Joust {
                    let on = |b: u32| m & b != 0;
                    (on(pad::LEFT) as u8 | (on(pad::RIGHT) as u8) << 1 | (fire(m) as u8) << 2, 0)
                } else {
                    (left_stick(m), right_stick(m))
                };
                let starts = if self.game == Game::Joust { self.starts(0x20, 0x10) } else { self.starts(0x10, 0x20) };
                (lo | starts | (hi & 3) << 6, hi >> 2)
            }
            Game::Sinistar => {
                // 49-way stick: 4 bits per axis, digital pad = full deflection
                const CENTER: u8 = 0x7;
                let on = |b: u32| p1 & b != 0;
                let x = if on(pad::LEFT) { 0x0 } else if on(pad::RIGHT) { 0x8 } else { CENTER };
                let y = if on(pad::UP) { 0x8 } else if on(pad::DOWN) { 0x0 } else { CENTER };
                let mut b = self.starts(0x10, 0x20);
                if on(pad::A) { b |= 0x01; }                      // fire
                if on(pad::B) || on(pad::X) { b |= 0x02; }        // sinibomb
                (x << 4 | y, b)
            }
        }
    }

    fn pia_read(&mut self, n: usize, a: u16) -> u8 {
        if n == 0 {
            let (pa, pb) = self.in0_in1();
            self.pia[0].read(a, pa, pb)
        } else {
            let i2 = self.in2;
            self.pia[1].read(a, i2, 0)
        }
    }

    fn pia_write(&mut self, n: usize, a: u16, v: u8) {
        if let PiaOut::B(cmd) = self.pia[n].write(a, v) {
            if n == 1 { self.sound.command(cmd); }
        }
    }

    fn video_counter(&self) -> u8 {
        if self.scanline < 0x100 { (self.scanline & 0xFC) as u8 } else { 0xFC }
    }

    fn cmos_write(&mut self, i: usize, v: u8) {
        let v = if self.game == Game::Bubbles { v } else { v | 0xF0 };   // 4 bits, Bubbles 8
        if self.cmos[i] != v { self.cmos[i] = v; self.cmos_dirty = true; }
    }

    // ---- Stargate-and-later I/O ----
    fn read_io(&mut self, a: u16) -> u8 {
        match a {
            0xC800..=0xC8FF if a & 0x0C == 0x04 => self.pia_read(0, a),
            0xC800..=0xC8FF if a & 0x0C == 0x0C => self.pia_read(1, a),
            0xCB00..=0xCBFF => self.video_counter(),
            0xCC00..=0xCFFF => self.cmos[(a - 0xCC00) as usize],
            0xD000..=0xDFFF if self.game == Game::Sinistar => self.ram_d000[(a - 0xD000) as usize],
            0xD000..=0xFFFF => self.rom[a as usize],
            _ => 0,
        }
    }

    fn write_io(&mut self, a: u16, v: u8) {
        match a {
            0xC000..=0xC3FF => self.palette_ram[(a & 0x0F) as usize] = v,
            0xC800..=0xC8FF if a & 0x0C == 0x04 => self.pia_write(0, a, v),
            0xC800..=0xC8FF if a & 0x0C == 0x0C => self.pia_write(1, a, v),
            0xC900..=0xC9FF => {
                self.rom_enabled = v & 1 != 0;
                if self.game == Game::Sinistar {
                    if let Some(b) = self.blitter.as_mut() { b.window = v & 4 != 0; }
                }
            }
            0xCA00..=0xCAFF => {
                if let Some(mut b) = self.blitter.take() {
                    self.stall += b.write(a, v, self);
                    self.blitter = Some(b);
                }
            }
            0xCC00..=0xCFFF => self.cmos_write((a - 0xCC00) as usize, v),
            0xD000..=0xDFFF if self.game == Game::Sinistar => self.ram_d000[(a - 0xD000) as usize] = v,
            _ => {}   // CBFF watchdog
        }
    }

    // ---- Defender ----
    fn defender_read(&mut self, a: u16) -> u8 {
        match a {
            0xC000..=0xCFFF if self.bank != 0 => {
                let o = (self.bank as usize - 1) * 0x1000 + (a as usize & 0xFFF);
                if self.bank <= 9 { self.banked.get(o).copied().unwrap_or(0) } else { 0 }
            }
            0xC400..=0xC7FF => self.cmos[(a & 0xFF) as usize],
            0xC800..=0xCBFF => self.video_counter(),
            0xCC00..=0xCFFF if a & 0x1C == 0x00 => self.pia_read(1, a),
            0xCC00..=0xCFFF if a & 0x1C == 0x04 => self.pia_read(0, a),
            0xD000..=0xFFFF => self.rom.get((a - 0xD000) as usize).copied().unwrap_or(0),
            _ => 0,
        }
    }

    fn defender_write(&mut self, a: u16, v: u8) {
        match a {
            0xC000..=0xCFFF if self.bank != 0 => {}
            0xC000..=0xC3FF if a & 0x10 == 0 => self.palette_ram[(a & 0x0F) as usize] = v,
            0xC400..=0xC7FF => self.cmos_write((a & 0xFF) as usize, v),
            0xCC00..=0xCFFF if a & 0x1C == 0x00 => self.pia_write(1, a, v),
            0xCC00..=0xCFFF if a & 0x1C == 0x04 => self.pia_write(0, a, v),
            0xD000..=0xDFFF => self.bank = v & 0x0F,
            _ => {}   // video control (cocktail flip), watchdog
        }
    }
}

impl M6809Bus for WilliamsBus {
    fn read(&mut self, a: u16) -> u8 {
        if a < 0xC000 {
            if self.rom_enabled && a < 0x9000 { self.rom[a as usize] } else { self.ram[a as usize] }
        } else if self.game == Game::Defender {
            self.defender_read(a)
        } else {
            self.read_io(a)
        }
    }
    fn write(&mut self, a: u16, v: u8) {
        if a < 0xC000 {
            self.ram[a as usize] = v
        } else if self.game == Game::Defender {
            self.defender_write(a, v)
        } else {
            self.write_io(a, v)
        }
    }
}

impl BlitterBus for WilliamsBus {
    fn blit_read(&mut self, a: u16) -> u8 { M6809Bus::read(self, a) }
    fn blit_read_dest(&mut self, a: u16) -> u8 { if a < 0xC000 { self.ram[a as usize] } else { self.read_io(a) } }
    fn blit_write(&mut self, a: u16, v: u8) { M6809Bus::write(self, a, v) }
}

pub struct WilliamsSystem {
    pub cpu: M6809,
    pub bus: WilliamsBus,
    game: Game,
    pens: [[u8; 4]; 256],
    line_cycles: i32,
    current_frame: u64,
    framebuffer: Vec<u8>,
    save_filename: String,
    save_path: String,
    state_path: String,
}

/// MAME palette_init: R and G through 1200/560/330 ohm, B through 560/330, weighted as
/// MAME's compute_resistor_weights (no pull-ups / pull-downs, autoscaled to 255).
fn build_pens() -> [[u8; 4]; 256] {
    fn weights(r: &[f64]) -> Vec<f64> {
        (0..r.len()).map(|n| {
            let mut r0 = 1e-12;                    // conductance to ground (the others)
            let r1 = 1e-12 + 1.0 / r[n];          // to Vcc (this one)
            for (j, rj) in r.iter().enumerate() { if j != n { r0 += 1.0 / rj; } }
            let (r0, r1) = (1.0 / r0, 1.0 / r1);
            255.0 * r0 / (r1 + r0)
        }).collect()
    }
    let rg = weights(&[1200.0, 560.0, 330.0]);
    let b = weights(&[560.0, 330.0]);
    let max = rg.iter().sum::<f64>().max(b.iter().sum::<f64>());
    let scale = 255.0 / max;
    let mut pens = [[0, 0, 0, 255]; 256];
    for (i, p) in pens.iter_mut().enumerate() {
        let bit = |n: usize| ((i >> n) & 1) as f64;
        let r = (rg[0] * bit(0) + rg[1] * bit(1) + rg[2] * bit(2)) * scale;
        let g = (rg[0] * bit(3) + rg[1] * bit(4) + rg[2] * bit(5)) * scale;
        let bb = (b[0] * bit(6) + b[1] * bit(7)) * scale;
        *p = [r as u8, g as u8, bb as u8, 255];
    }
    pens
}

impl WilliamsSystem {
    pub fn new(game: Game, set_name: &str, mut regions: HashMap<String, Vec<u8>>) -> Result<Self, String> {
        let mut rom = take_region(&mut regions, "maincpu")?;
        let banked = if game == Game::Defender { take_region(&mut regions, "banked")? } else { Vec::new() };
        if game != Game::Defender { rom.resize(0x10000, 0); }
        let snd = take_region(&mut regions, "soundcpu")?;
        let mut bus = WilliamsBus {
            game,
            rom,
            banked,
            ram: vec![0; 0xC000],
            ram_d000: vec![0; if game == Game::Sinistar { 0x1000 } else { 0 }],
            palette_ram: [0; 16],
            cmos: vec![0; if game == Game::Defender { 0x100 } else { 0x400 }],
            cmos_dirty: false,
            rom_enabled: false,
            bank: 0,
            pia: [Pia6821::new(), Pia6821::new()],
            blitter: game.blitter(),
            sound: WilliamsSound::with_cvsd(snd, game == Game::Sinistar),
            stall: 0,
            scanline: 0,
            input: [0; 2],
            starts: (false, false),
            in2: 0,
        };
        let mut cpu = M6809::new();
        cpu.reset(&mut bus);
        Ok(Self {
            cpu,
            bus,
            game,
            pens: build_pens(),
            line_cycles: 0,
            current_frame: 0,
            framebuffer: vec![0; SCREEN_W * SCREEN_H * 4],
            save_filename: set_name.to_string(),
            save_path: "user://GD_EMU/Arcade/Williams/Save".to_string(),
            state_path: "user://GD_EMU/Arcade/Williams/State".to_string(),
        })
    }

    fn render_line(&mut self, y: u16) {
        let row = (y - VIS_Y) as usize;
        let mut pens = [[0u8; 4]; 16];
        for (p, &c) in pens.iter_mut().zip(self.bus.palette_ram.iter()) { *p = self.pens[c as usize]; }
        let vis_x = self.game.vis_x();
        let out = &mut self.framebuffer[row * SCREEN_W * 4..(row + 1) * SCREEN_W * 4];
        for x in 0..SCREEN_W {
            let sx = x + vis_x;
            let byte = self.bus.ram[(sx / 2) * 256 + y as usize];
            let pix = if sx & 1 == 0 { byte >> 4 } else { byte & 0x0F };
            out[x * 4..x * 4 + 4].copy_from_slice(&pens[pix as usize]);
        }
    }

    fn timers(&mut self, line: u16) {
        // VA11 into PIA 1 CB1 every 32 lines (not at 256); COUNT240 into CA1
        if line % 32 == 0 && line != 256 { self.bus.pia[1].cb1_w(line & 0x20 != 0); }
        if line == 0 || line == 240 { self.bus.pia[1].ca1_w(line >= 240); }
    }

    fn hard_reset(&mut self) {
        let b = &mut self.bus;
        b.rom_enabled = false;
        b.bank = 0;
        b.pia = [Pia6821::new(), Pia6821::new()];
        b.blitter = self.game.blitter();
        b.sound.reset();
        self.line_cycles = 0;
        self.cpu = M6809::new();
        self.cpu.reset(&mut self.bus);
    }
}

impl System for WilliamsSystem {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        for line in 0..LINES_PER_FRAME {
            self.bus.scanline = line;
            self.timers(line);
            while self.line_cycles < CYCLES_PER_LINE {
                self.cpu.set_irq(self.bus.pia[1].irq());
                self.line_cycles += self.cpu.step(&mut self.bus) as i32;
                self.line_cycles += std::mem::take(&mut self.bus.stall) as i32;
            }
            self.line_cycles -= CYCLES_PER_LINE;
            self.bus.sound.run(1, LINE_RATE);
            if (VIS_Y..VIS_Y + SCREEN_H as u16).contains(&line) {
                self.render_line(line);
            }
        }
        self.current_frame += 1;
    }

    fn set_input(&mut self, mask: u32) {
        // player 2 has a stick and A/B/X; the right stick is shared
        let p2 = pad::player2(mask)
            | if mask & pad::P2_X != 0 { pad::X } else { 0 }
            | (mask & (pad::RS_UP | pad::RS_DOWN | pad::RS_LEFT | pad::RS_RIGHT));
        self.bus.input = [mask, p2];
        self.bus.starts = (mask & pad::START != 0, mask & pad::START2 != 0);
        let mut in2 = 0;
        if mask & pad::L != 0 { in2 |= 0x02; }                      // Advance (service menu)
        if mask & (pad::COIN | pad::SELECT) != 0 { in2 |= 0x10; }   // left coin
        self.bus.in2 = in2;
    }

    fn drain_audio(&mut self) -> Vec<f32> { std::mem::take(&mut self.bus.sound.out) }
    fn sample_rate(&self) -> u32 { 44100 }
    fn power_on(&mut self) {}
    fn power_off(&mut self) {}
    fn has_battery(&self) -> bool { true }
    fn save_filename(&self) -> &str { &self.save_filename }
    fn save_battery_path(&self) -> &str { &self.save_path }
    fn save_state_path(&self) -> &str { &self.state_path }
    fn is_sram_dirty(&self) -> bool { self.bus.cmos_dirty }
    fn clear_sram_dirty(&mut self) { self.bus.cmos_dirty = false; }
    fn get_current_frame(&self) -> i64 { self.current_frame as i64 }
    fn get_sram(&self) -> Option<&[u8]> { Some(&self.bus.cmos) }
    fn load_sram(&mut self, data: &[u8]) {
        let n = data.len().min(self.bus.cmos.len());
        self.bus.cmos[..n].copy_from_slice(&data[..n]);
    }

    fn framebuffer(&self) -> &[u8] { &self.framebuffer }

    fn reset(&mut self) { self.hard_reset(); }

    fn display_info(&self) -> DisplayInfo {
        DisplayInfo {
            buffer_width: SCREEN_W as i32,
            buffer_height: SCREEN_H as i32,
            visible_x: 0,
            visible_y: 0,
            visible_width: SCREEN_W as i32,
            visible_height: SCREEN_H as i32,
            target_aspect_ratio: if self.game == Game::Sinistar { 3.0 / 4.0 } else { 4.0 / 3.0 },
            rotation: if self.game == Game::Sinistar { Rotation::CCW90 } else { Rotation::None },
        }
    }

    fn save_state(&self) -> Vec<u8> {
        let b = &self.bus;
        let bus = BusState {
            ram: b.ram.clone(), ram_d000: b.ram_d000.clone(), palette_ram: b.palette_ram,
            cmos: b.cmos.clone(), rom_enabled: b.rom_enabled, bank: b.bank,
            pia: b.pia.clone(), blitter: b.blitter.clone(), sound: b.sound.clone(),
        };
        state::encode(&self.save_filename, STATE_VERSION, &(&self.cpu, bus, self.line_cycles, self.current_frame))
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let (cpu, bus, line_cycles, frame): (M6809, BusState, i32, u64) =
            state::decode(&self.save_filename, STATE_VERSION, data)?;
        let b = &mut self.bus;
        if bus.ram.len() != b.ram.len() || bus.cmos.len() != b.cmos.len() || bus.ram_d000.len() != b.ram_d000.len()
            || bus.blitter.is_some() != b.blitter.is_some() {
            return Err("save state doesn't fit this board".into());
        }
        b.ram = bus.ram;
        b.ram_d000 = bus.ram_d000;
        b.palette_ram = bus.palette_ram;
        b.cmos = bus.cmos;
        b.cmos_dirty = true;
        b.rom_enabled = bus.rom_enabled;
        b.bank = bus.bank;
        b.pia = bus.pia;
        b.blitter = bus.blitter;
        let mut sound = bus.sound;
        sound.keep_rom(&mut b.sound);
        b.sound = sound;
        self.cpu = cpu;
        self.line_cycles = line_cycles;
        self.current_frame = frame;
        Ok(())
    }
}
