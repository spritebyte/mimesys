//! Atari 5200 SuperSystem (1982): 6502C at 1.79 MHz, ANTIC + GTIA video, POKEY sound and
//! controller scanning, 16 KB RAM.
//!
//! Map: 0000-3FFF RAM, 4000-BFFF cartridge, C000-CFFF GTIA, D400-D5FF ANTIC,
//! E800-EFFF POKEY, F800-FFFF BIOS (AltirraOS bundled; a real 5200 BIOS can replace it).
//!
//! Frame: 262 lines of 114 CPU cycles. ANTIC takes cycles for its DMA (refresh, display
//! list, screen data, player / missile graphics); WSYNC holds the CPU to cycle 105. The
//! DLI NMI comes at cycle 8 of a mode line's last line, the VBI at line 248.
//!
//! Controllers: the stick is analog (POKEY pots: 1 left/up, 114 centre, 227 right/down,
//! as Altirra feeds a digital stick). CONSOL bit 2 powers the pots: while it's clear a
//! joystick reads 228, which is how games tell a joystick (228) from a Trak-Ball (114);
//! Pengo and Centipede check this. The BIOS leaves it clear, games set it. A big jump in
//! a pot value gets +-1 of jitter for a few frames (Altirra: Galaxian's calibration
//! needs to see both odd and even values). The bottom fire buttons are GTIA triggers, the keypad, START / PAUSE / RESET and the
//! top fire buttons come through POKEY's keyboard scan (one controller at a time,
//! picked by CONSOL bits 0-1). Each 5200 key closes four key codes and games turn
//! debounce off, so a held key raises key IRQs several times a frame, as on the console.
//! Option "trackball": controller 1 is a Trak-Ball rolled with the mouse (`set_pointer`).
//!
//! Unified input mask: D-pad stick, A bottom fire, B top fire, START start, SELECT pause,
//! P2 START reset, keypad nibble 0-9 * # (bits 8-11).

use serde::{Deserialize, Serialize};
use crate::common::audio::pokey::Pokey;
use crate::common::cpu::m6502::{AddressBus, CpuState, CpuVariant, M6502Cpu};
use crate::common::input as pad;
use crate::common::system::{DisplayInfo, Rotation, System};
use crate::atari5200::cart::Cart5200;
use crate::atari5200::video::{palette, Antic, Gtia, VideoMem, FIRST_LINE, OUT_H, OUT_W};

pub const ALTIRRA_OS: &[u8] = include_bytes!("altirra_5200_os.bin");
const LINES: u32 = 262;
const CYCLES: u32 = 114;
const WSYNC_END: u32 = 105;
const DRAW_AT: u32 = 24;
const VBI_LINE: u32 = 248;
const CPU_CLOCK: f64 = 1_789_772.5;
const SAMPLE_RATE: u32 = 44_100;
const STATE_VERSION: u32 = 3;
/// Pot readings: a joystick pushed left / up, centred, right / down, and unpowered.
const POT_MIN: u8 = 1;
const POT_CENTRE: u8 = 114;
const POT_MAX: u8 = 227;
const POT_OFF: u8 = 228;

/// KBCODE values of the keypad 0-9, *, # and START / PAUSE / RESET.
const KEYS: [u8; 12] = [0x25, 0x3F, 0x3D, 0x3B, 0x37, 0x35, 0x33, 0x2F, 0x2D, 0x2B, 0x27, 0x23];
const KEY_START: u8 = 0x39;
const KEY_PAUSE: u8 = 0x31;
const KEY_RESET: u8 = 0x29;

#[derive(Serialize, Deserialize)]
pub struct Bus {
    #[serde(with = "serde_bytes")]
    pub ram: Vec<u8>,
    #[serde(skip)]
    pub bios: Vec<u8>,
    pub cart: Cart5200,
    pub antic: Antic,
    pub gtia: Gtia,
    pub pokey: Pokey,
    /// What the controllers' pots read when CONSOL bit 2 powers them (4-7: no controller).
    pub stick: [u8; 8],
    /// Port 1 is a Trak-Ball (its pots read 114 while unpowered, not 228).
    pub trackball: bool,
    /// Keypads (64 POKEY key codes) and top buttons of controllers 1 and 2; POKEY sees
    /// the one CONSOL bits 0-1 select.
    #[serde(skip)]
    keypads: [u64; 2],
    #[serde(skip)]
    top: [bool; 2],
    #[serde(skip)]
    pub wsync: bool,
    #[serde(skip)]
    pub nmi_line: bool,
    pub line: u32,
}

impl Bus {
    fn peek(&self, a: u16) -> u8 {
        match a {
            0x0000..=0x3FFF => self.ram[a as usize],
            0x4000..=0xBFFF => self.cart.read(a),
            0xF800..=0xFFFF => self.bios.get((a - 0xF800) as usize).copied().unwrap_or(0xFF),
            _ => 0xFF,
        }
    }
}

impl Bus {
    /// Feed POKEY's pot inputs: the sticks when CONSOL bit 2 is set, else 228.
    pub fn update_pots(&mut self) {
        let on = self.gtia.consol & 4 != 0;
        for i in 0..8 {
            let off = if i < 2 && self.trackball { POT_CENTRE } else { POT_OFF };
            self.pokey.pot_in[i] = if on { self.stick[i] } else { off };
        }
    }

    /// Hand POKEY the keypad and top button of the controller CONSOL selects.
    pub fn update_keys(&mut self) {
        let sel = (self.gtia.consol & 3) as usize;
        let (m, t) = if sel < 2 { (self.keypads[sel], self.top[sel]) } else { (0, false) };
        self.pokey.set_key_matrix(m, t, t, t);
    }
}

impl VideoMem for Bus { fn peek(&self, a: u16) -> u8 { Bus::peek(self, a) } }

impl AddressBus for Bus {
    fn read_byte(&mut self, a: u16) -> u8 {
        match a {
            0xC000..=0xCFFF => self.gtia.read(a as u8),
            0xD400..=0xD5FF => match a & 0x0F {
                0x0B => (self.line >> 1) as u8,                 // VCOUNT
                0x0F => self.antic.nmist | 0x1F,                // NMIST
                _ => 0xFF,
            },
            0xE800..=0xEFFF => self.pokey.read(a as u8),
            _ => self.peek(a),
        }
    }

    fn write_byte(&mut self, a: u16, v: u8) {
        match a {
            0x0000..=0x3FFF => self.ram[a as usize] = v,
            0xC000..=0xCFFF => {
                self.gtia.write(a as u8, v);
                if a & 0x1F == 0x1F { self.update_pots(); self.update_keys(); }   // CONSOL
            }
            0xD400..=0xD5FF => {
                let n = &mut self.antic;
                match a & 0x0F {
                    0x0 => n.dmactl = v,
                    0x1 => n.chactl = v,
                    0x2 => n.dlist = (n.dlist & 0xFF00) | v as u16,
                    0x3 => n.dlist = (n.dlist & 0x00FF) | (v as u16) << 8,
                    0x4 => n.hscrol = v & 15,
                    0x5 => n.vscrol = v & 15,
                    0x7 => n.pmbase = v,
                    0x9 => n.chbase = v,
                    0xA => self.wsync = true,
                    0xE => n.nmien = v,
                    0xF => n.nmist = 0,                         // NMIRES
                    _ => {}
                }
            }
            0xE800..=0xEFFF => self.pokey.write(a as u8, v),
            _ => {}
        }
    }

    fn is_nmi_line_asserted(&mut self) -> bool { self.nmi_line }
    fn is_irq_line_asserted(&mut self) -> bool { self.pokey.irq() }
    fn is_nmi_enabled(&mut self) -> bool { true }
    fn step_cycles(&mut self, _cycles: u64) {}
    fn begin_cpu_cycle(&mut self) {}
}

pub struct A5200System {
    cpu: M6502Cpu,
    pub bus: Bus,
    palette: Vec<[u8; 3]>,
    pub framebuffer: Vec<u8>,
    current_frame: u64,
    // input
    /// mouse as Trak-Ball: last pointer position over the picture, buttons
    pointer: Option<(i32, i32)>,
    pointer_last: Option<(i32, i32)>,
    pointer_buttons: u32,
    /// per pot: the digital target, and frames of jitter left after a jump
    pot_target: [u8; 4],
    jitter: [u8; 4],
    // sound: POKEY averaged down to 44.1 kHz, DC removed
    audio: Vec<f32>,
    acc: f64,
    acc_n: u32,
    phase: f64,
    dc_in: f32,
    dc_out: f32,
    save_filename: String,
    save_battery_path: String,
    save_state_path: String,
}

#[derive(Serialize, Deserialize)]
struct State {
    version: u32,
    rom_len: usize,
    cpu: CpuState,
    bus: Bus,
    frame: u64,
}

impl A5200System {
    /// `bios`: a real 5200 BIOS (2 KB), or empty for AltirraOS.
    pub fn from_rom(rom: &[u8], filename: &str, bios: &[u8]) -> Result<Self, String> {
        let cart = Cart5200::new(rom)?;
        let bios = if bios.len() == 0x800 { bios.to_vec() } else { ALTIRRA_OS.to_vec() };
        println!("Atari 5200: {} KB cartridge ({:?}), {} BIOS", cart.rom.len() / 1024, cart.mapping,
                 if bios == ALTIRRA_OS { "AltirraOS" } else { "real" });
        let mut pokey = Pokey::new();
        pokey.pot_in = [POT_OFF; 8];
        let mut sys = Self {
            cpu: M6502Cpu::new(CpuVariant::NMOS6502),
            bus: Bus { ram: vec![0; 0x4000], bios, cart, antic: Antic::default(), gtia: Gtia::default(), pokey,
                       stick: [POT_CENTRE, POT_CENTRE, POT_CENTRE, POT_CENTRE, POT_OFF, POT_OFF, POT_OFF, POT_OFF],
                       trackball: false, keypads: [0; 2], top: [false; 2],
                       wsync: false, nmi_line: false, line: 0 },
            palette: palette(),
            framebuffer: vec![0; OUT_W * OUT_H * 4],
            current_frame: 0,
            pointer: None, pointer_last: None, pointer_buttons: 0, pot_target: [POT_CENTRE; 4], jitter: [0; 4],
            audio: Vec::new(), acc: 0.0, acc_n: 0, phase: 0.0, dc_in: 0.0, dc_out: 0.0,
            save_filename: filename.to_string(),
            save_battery_path: "user://GD_EMU/A5200/Save".to_string(),
            save_state_path: "user://GD_EMU/A5200/State".to_string(),
        };
        sys.cpu.power_on(&mut sys.bus);
        Ok(sys)
    }

    /// POKEY runs every cycle (the CPU may be stopped); its output is averaged.
    fn pokey_cycle(&mut self) {
        self.bus.pokey.step();
        self.acc += self.bus.pokey.output() as f64;
        self.acc_n += 1;
        self.phase += SAMPLE_RATE as f64;
        if self.phase >= CPU_CLOCK {
            self.phase -= CPU_CLOCK;
            let x = (self.acc / self.acc_n.max(1) as f64) as f32;
            self.acc = 0.0;
            self.acc_n = 0;
            // DC blocker
            let y = x - self.dc_in + 0.995 * self.dc_out;
            self.dc_in = x;
            self.dc_out = y;
            let s = (y * 1.6).clamp(-1.0, 1.0);
            self.audio.push(s);
            self.audio.push(s);
        }
    }

    fn run_line(&mut self, line: u32) {
        self.bus.line = line;
        if line == VBI_LINE {
            self.bus.antic.nmist = (self.bus.antic.nmist & !0x80) | 0x40;
        }
        let (stolen, dli) = {
            let bus = &mut self.bus;
            let mut antic = std::mem::take(&mut bus.antic);
            let r = antic.start_line(line, bus);
            bus.antic = antic;
            r
        };
        let visible = (FIRST_LINE..FIRST_LINE + OUT_H as u32).contains(&line);
        if visible {
            let bus = &mut self.bus;
            let mut gtia = std::mem::take(&mut bus.gtia);
            gtia.pm_dma(&bus.antic, line, bus);
            bus.gtia = gtia;
        }
        // DMA cycles come as one block early in the line
        let dma = 10..10 + stolen;
        for t in 0..CYCLES {
            if t == 8 && dli {
                self.bus.antic.nmist = (self.bus.antic.nmist & !0x40) | 0x80;
                if self.bus.antic.nmien & 0x80 != 0 { self.bus.nmi_line = true; }
            }
            if t == 7 && line == VBI_LINE && self.bus.antic.nmien & 0x40 != 0 { self.bus.nmi_line = true; }
            if t == DRAW_AT && visible { self.draw_line(line); }
            self.pokey_cycle();
            if self.bus.wsync {
                if t + 1 >= WSYNC_END { self.bus.wsync = false; }
                continue;
            }
            if dma.contains(&t) { continue; }
            self.cpu.step_one_cycle(&mut self.bus);
            self.cpu.sample_interrupt_lines(&mut self.bus);
            self.bus.nmi_line = false;
        }
        if self.bus.wsync && line + 1 < LINES { self.bus.wsync = false; }
    }

    fn draw_line(&mut self, line: u32) {
        let mut pf = [0u8; OUT_W];
        let mut out = [0u8; OUT_W];
        self.bus.antic.playfield(&self.bus, &mut pf);
        self.bus.gtia.compose(&pf, &mut out);
        let row = (line - FIRST_LINE) as usize * OUT_W * 4;
        for (x, &c) in out.iter().enumerate() {
            let [r, g, b] = self.palette[c as usize];
            self.framebuffer[row + x * 4..row + x * 4 + 4].copy_from_slice(&[r, g, b, 255]);
        }
    }
}

impl System for A5200System {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        for line in 0..LINES { self.run_line(line); }
        self.current_frame += 1;
    }

    fn set_input(&mut self, mask: u32) {
        let on = |b: u32| mask & b != 0;
        let p2 = pad::player2(mask);
        let on2 = |b: u32| p2 & b != 0;
        let axis = |lo: bool, hi: bool| match (lo, hi) { (true, false) => POT_MIN, (false, true) => POT_MAX, _ => POT_CENTRE };
        let targets = [axis(on(pad::LEFT), on(pad::RIGHT)), axis(on(pad::UP), on(pad::DOWN)),
                       axis(on2(pad::LEFT), on2(pad::RIGHT)), axis(on2(pad::UP), on2(pad::DOWN))];
        for i in 0..4 {
            if targets[i].abs_diff(self.pot_target[i]) > 64 { self.jitter[i] = 5; }
            self.pot_target[i] = targets[i];
            let j: i16 = if self.jitter[i] > 0 {
                self.jitter[i] -= 1;
                [-1, 0, 1, 0][(self.current_frame as usize + i) & 3]
            } else { 0 };
            self.bus.stick[i] = (targets[i] as i16 + j).clamp(POT_MIN as i16, POT_MAX as i16) as u8;
        }
        let mut fire1 = on(pad::A);
        let mut top1 = on(pad::B);
        if self.bus.trackball {
            // the mouse rolls the ball: pot = 114 + speed (Altirra keeps it within 38-189)
            let (dx, dy) = match (self.pointer, self.pointer_last) {
                (Some((x, y)), Some((lx, ly))) => (x - lx, y - ly),
                _ => (0, 0),
            };
            self.pointer_last = self.pointer;
            let roll = |d: i32| (POT_CENTRE as i32 + (d * 3).clamp(-75, 75)) as u8;
            self.bus.stick[0] = roll(dx);
            self.bus.stick[1] = roll(dy);
            fire1 |= self.pointer_buttons & 1 != 0;
            top1 |= self.pointer_buttons & 2 != 0;
        }
        self.bus.update_pots();
        self.bus.gtia.set_triggers([fire1, on2(pad::A), false, false]);
        // keypads: START / PAUSE / RESET and 0-9 * # on controller 1, START / PAUSE on 2
        let digit = (mask >> pad::KEYPAD_SHIFT) & 0x0F;
        let key1 = if on(pad::START) { Some(KEY_START) }
            else if on(pad::SELECT) { Some(KEY_PAUSE) }
            else if on(pad::START2) { Some(KEY_RESET) }
            else if (digit as usize) < KEYS.len() { Some(KEYS[digit as usize]) }
            else { None };
        let key2 = if on2(pad::START) { Some(KEY_START) } else if on2(pad::SELECT) { Some(KEY_PAUSE) } else { None };
        for (pad_keys, key) in self.bus.keypads.iter_mut().zip([key1, key2]) {
            *pad_keys = 0;
            // a 5200 key closes four of POKEY's codes (scan lines 0 and 5 aren't wired)
            if let Some(k) = key { for x in [0, 1, 0x20, 0x21] { *pad_keys |= 1u64 << ((k & 0x1E) | x); } }
        }
        self.bus.top = [top1, on2(pad::B)];
        self.bus.update_keys();
    }

    fn set_option(&mut self, key: &str, value: i64) {
        if key == "trackball" { self.bus.trackball = value != 0; }
    }

    /// The mouse, when port 1 is a Trak-Ball: position over the picture and buttons.
    fn set_pointer(&mut self, x: i32, y: i32, buttons: u32) {
        self.pointer = if x < 0 || y < 0 { None } else { Some((x, y)) };
        if self.pointer.is_none() { self.pointer_last = None; }
        self.pointer_buttons = buttons;
    }

    fn drain_audio(&mut self) -> Vec<f32> { std::mem::take(&mut self.audio) }
    fn sample_rate(&self) -> u32 { SAMPLE_RATE }
    fn power_on(&mut self) {}
    fn power_off(&mut self) {}
    fn has_battery(&self) -> bool { false }
    fn save_filename(&self) -> &str { &self.save_filename }
    fn save_battery_path(&self) -> &str { &self.save_battery_path }
    fn save_state_path(&self) -> &str { &self.save_state_path }
    fn is_sram_dirty(&self) -> bool { false }
    fn clear_sram_dirty(&mut self) {}
    fn get_current_frame(&self) -> i64 { self.current_frame as i64 }
    fn get_sram(&self) -> Option<&[u8]> { None }
    fn load_sram(&mut self, _data: &[u8]) {}
    fn framebuffer(&self) -> &[u8] { &self.framebuffer }

    fn reset(&mut self) {
        self.cpu = M6502Cpu::new(CpuVariant::NMOS6502);
        self.cpu.power_on(&mut self.bus);
    }

    fn display_info(&self) -> DisplayInfo {
        DisplayInfo {
            buffer_width: OUT_W as i32,
            buffer_height: OUT_H as i32,
            visible_x: 0,
            visible_y: 0,
            visible_width: OUT_W as i32,
            visible_height: OUT_H as i32,
            target_aspect_ratio: 4.0 / 3.0,
            rotation: Rotation::None,
        }
    }

    fn frame_time_usec(&self) -> i64 { (LINES as f64 * CYCLES as f64 * 1e6 / CPU_CLOCK) as i64 }

    fn save_state(&self) -> Vec<u8> {
        let config = bincode::config::standard().with_fixed_int_encoding();
        // the bus (with its ROM-less cartridge) goes in by reference
        #[derive(Serialize)]
        struct Ref<'a> { version: u32, rom_len: usize, cpu: CpuState, bus: &'a Bus, frame: u64 }
        bincode::serde::encode_to_vec(&Ref { version: STATE_VERSION, rom_len: self.bus.cart.rom.len(),
            cpu: self.cpu.get_state(), bus: &self.bus, frame: self.current_frame }, config).unwrap_or_default()
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let config = bincode::config::standard().with_fixed_int_encoding();
        let (s, _): (State, usize) = bincode::serde::decode_from_slice(data, config)
            .map_err(|e| format!("Failed to decode 5200 save state: {e:?}"))?;
        if s.version != STATE_VERSION { return Err(format!("save state version {} (expected {STATE_VERSION})", s.version)); }
        if s.rom_len != self.bus.cart.rom.len() { return Err("save state is from a different game".into()); }
        let rom = std::mem::take(&mut self.bus.cart.rom);
        let bios = std::mem::take(&mut self.bus.bios);
        self.bus = s.bus;
        self.bus.cart.rom = rom;
        self.bus.bios = bios;
        self.cpu.load_state(&s.cpu);
        self.current_frame = s.frame;
        Ok(())
    }
}
