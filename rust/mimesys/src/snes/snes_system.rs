//! Super Nintendo: 65C816 CPU (W65816), the main bus (bus.rs: map, timing, DMA,
//! interrupts), the PPU (ppu.rs) and, until stage 3, a stand-in for the sound side.
//!
//! A frame is 262 lines of 1364 master clocks (21.477 MHz, about 60.1 Hz). Interrupts
//! are taken between instructions: NMI at the start of vblank when enabled, IRQ from
//! the H / V timer while the I flag is clear (WAI wakes on either).
//!
//! Input mask (player 1; player 2 through `pad::player2`): D-pad, A, B, X, Y, L, R,
//! START, SELECT.

use serde::{Deserialize, Serialize};
use crate::common::cpu::w65816::{W65816, I};
use crate::common::input as pad;
use crate::common::system::{DisplayInfo, Rotation, System};
use crate::snes::bus::Bus;
use crate::snes::cart::Cart;
use crate::snes::ppu::{HEIGHT, WIDTH};

const SAMPLE_RATE: u32 = 32_000;
const STATE_VERSION: u32 = 2;
const MASTER_HZ: f64 = 21_477_272.7;

pub struct SnesSystem {
    pub cpu: W65816,
    pub bus: Bus,
    audio: Vec<f32>,
    audio_phase: u64,
    save_filename: String,
    save_battery_path: String,
    save_state_path: String,
}

#[derive(Serialize, Deserialize)]
struct State { version: u32, cpu: W65816, bus: Bus }

/// Unified input mask -> SNES pad bits (15 B, 14 Y, 13 Select, 12 Start, 11 Up,
/// 10 Down, 9 Left, 8 Right, 7 A, 6 X, 5 L, 4 R).
fn pad_bits(m: u32) -> u16 {
    let mut b = 0u16;
    for (bit, on) in [(15, pad::B), (14, pad::Y), (13, pad::SELECT), (12, pad::START), (11, pad::UP), (10, pad::DOWN),
                      (9, pad::LEFT), (8, pad::RIGHT), (7, pad::A), (6, pad::X), (5, pad::L), (4, pad::R)] {
        if m & on != 0 { b |= 1 << bit; }
    }
    // opposite directions can't both be down on a real pad
    if b & 0x0C00 == 0x0C00 { b &= !0x0400; }
    if b & 0x0300 == 0x0300 { b &= !0x0100; }
    b
}

impl SnesSystem {
    pub fn from_rom(rom: &[u8], filename: &str) -> Result<Self, String> {
        let cart = Cart::new(rom)?;
        println!("SNES: \"{}\", {:?}{}, {} KB ROM, {} KB SRAM, chipset {:02X}, region {:02X}", cart.title, cart.map,
                 if cart.fast_rom { " (fast)" } else { "" }, cart.rom.len() / 1024, cart.sram.len() / 1024, cart.chipset, cart.region);
        if cart.chipset & 0x0F >= 3 {
            println!("SNES: this cartridge has an enhancement chip (not emulated yet); it may not run");
        }
        let mut s = Self {
            cpu: W65816::new(),
            bus: Bus::new(cart),
            audio: Vec::new(), audio_phase: 0,
            save_filename: filename.to_string(),
            save_battery_path: "user://GD_EMU/SNES/Save".to_string(),
            save_state_path: "user://GD_EMU/SNES/State".to_string(),
        };
        s.cpu.reset(&mut s.bus);
        Ok(s)
    }

    /// Run until the frame ends.
    fn run_until_frame(&mut self) {
        self.bus.frame_done = false;
        let start = self.bus.clocks;
        while !self.bus.frame_done {
            if self.bus.nmi_pending {
                self.bus.nmi_pending = false;
                self.cpu.interrupt(&mut self.bus, true);
            } else if self.bus.irq_line() && self.cpu.p & I == 0 {
                self.cpu.interrupt(&mut self.bus, false);
            } else {
                if self.cpu.wai && self.bus.irq_line() { self.cpu.wai = false; }
                self.cpu.step(&mut self.bus);
            }
            if self.bus.clocks - start > 4 * 1364 * 262 { break; }   // a stuck frame
        }
        // silence until stage 3, at the right rate
        let clocks = self.bus.clocks - start;
        self.audio_phase += clocks * SAMPLE_RATE as u64;
        let n = (self.audio_phase as f64 / MASTER_HZ) as u64;
        self.audio_phase -= (n as f64 * MASTER_HZ) as u64;
        self.audio.extend(std::iter::repeat(0.0).take(n as usize * 2));
    }
}

impl System for SnesSystem {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        self.run_until_frame();
    }

    fn set_input(&mut self, m: u32) {
        self.bus.pads[0] = pad_bits(m);
        self.bus.pads[1] = pad_bits(pad::player2(m) | if m & pad::P2_X != 0 { pad::X } else { 0 });
    }

    fn framebuffer(&self) -> &[u8] { &self.bus.framebuffer }

    fn display_info(&self) -> DisplayInfo {
        DisplayInfo {
            buffer_width: WIDTH as i32, buffer_height: HEIGHT as i32,
            visible_x: 0, visible_y: 0, visible_width: WIDTH as i32, visible_height: HEIGHT as i32,
            target_aspect_ratio: 4.0 / 3.0,
            rotation: Rotation::None,
        }
    }

    fn frame_time_usec(&self) -> i64 { (1364.0 * 262.0 * 1e6 / MASTER_HZ) as i64 }
    fn drain_audio(&mut self) -> Vec<f32> { std::mem::take(&mut self.audio) }
    fn sample_rate(&self) -> u32 { SAMPLE_RATE }
    fn power_on(&mut self) {}
    fn power_off(&mut self) {}
    fn reset(&mut self) {
        self.bus.ppu = crate::snes::ppu::Ppu::new();
        self.bus.apu = crate::snes::smp::Smp::new();
        self.cpu.reset(&mut self.bus);
    }
    fn has_battery(&self) -> bool { !self.bus.cart.sram.is_empty() }
    fn save_filename(&self) -> &str { &self.save_filename }
    fn save_battery_path(&self) -> &str { &self.save_battery_path }
    fn save_state_path(&self) -> &str { &self.save_state_path }
    fn is_sram_dirty(&self) -> bool { self.bus.cart.sram_dirty }
    fn clear_sram_dirty(&mut self) { self.bus.cart.sram_dirty = false; }
    fn get_current_frame(&self) -> i64 { self.bus.frame as i64 }
    fn get_sram(&self) -> Option<&[u8]> { if self.bus.cart.sram.is_empty() { None } else { Some(&self.bus.cart.sram) } }
    fn load_sram(&mut self, data: &[u8]) {
        let n = data.len().min(self.bus.cart.sram.len());
        self.bus.cart.sram[..n].copy_from_slice(&data[..n]);
    }

    fn save_state(&self) -> Vec<u8> {
        let config = bincode::config::standard().with_fixed_int_encoding();
        #[derive(Serialize)]
        struct Ref<'a> { version: u32, cpu: &'a W65816, bus: &'a Bus }
        bincode::serde::encode_to_vec(&Ref { version: STATE_VERSION, cpu: &self.cpu, bus: &self.bus }, config).unwrap_or_default()
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let config = bincode::config::standard().with_fixed_int_encoding();
        let (s, _): (State, usize) = bincode::serde::decode_from_slice(data, config)
            .map_err(|e| format!("Failed to decode SNES save state: {e:?}"))?;
        if s.version != STATE_VERSION { return Err(format!("save state version {} (expected {STATE_VERSION})", s.version)); }
        let rom = std::mem::take(&mut self.bus.cart.rom);
        let fb = std::mem::take(&mut self.bus.framebuffer);
        self.bus = s.bus;
        self.bus.cart.rom = rom;
        self.bus.framebuffer = fb;
        self.cpu = s.cpu;
        Ok(())
    }
}
