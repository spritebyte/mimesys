// Gottlieb GG-III arcade hardware (Q*bert and friends). MAME src/mame/gottlieb/gottlieb.cpp.
//
//   Main CPU: Intel 8088 @ 15 MHz / 3 = 5 MHz, NMI on vertical blank
//   Video:    20 MHz / 4 pixel clock = the CPU clock, so 318 CPU cycles per line,
//             256 lines (240 visible), 61.42 Hz
//   Sound:    "rev. 1" board, 6502 + RIOT + DAC + Votrax SC-01-A (gottlieb_sound.rs)
//
// Regions: maincpu (64 KB, ROM at 0x6000-0xFFFF), audiocpu (64 KB, ROM at 0x6000-0x7FFF),
// bgtiles (8 KB), sprites (32 KB), and optionally votrax (sc01a.bin, 512 bytes) for speech.

use crate::common::cpu::i8088::I8088;
use crate::common::input as pad;
use crate::common::system::{System, DisplayInfo, Rotation};
use crate::arcade::take_region;
use crate::arcade::gottlieb::gottlieb_bus::GottliebBus;
use crate::arcade::gottlieb::gottlieb_video::{GottliebVideo, SCREEN_W, SCREEN_H};
use crate::arcade::gottlieb::gottlieb_sound::{GottliebSound, SOUND_CPU_CLOCK};
use crate::arcade::state;
use crate::arcade::gottlieb::gottlieb_sound::SoundState;
use serde::{Serialize, Deserialize};
use std::collections::HashMap;

const STATE_VERSION: u32 = 1;

/// Everything but ROMs and decoded graphics.
#[derive(Serialize, Deserialize)]
struct GottliebState<C, S> {
    cpu: C,
    nvram: Vec<u8>,
    ram: Vec<u8>,
    videoram: Vec<u8>,
    charram: Vec<u8>,
    spriteram: Vec<u8>,
    paletteram: Vec<u8>,
    output: u8,
    knocker_prev: bool,
    watchdog: u32,
    sound: S,
    line_cycles: i64,
    sound_cycle_debt: i64,
    frame: u64,
}

const CYCLES_PER_LINE: i64 = 318;
const LINES_PER_FRAME: usize = 256;
const VBLANK_START: usize = 240;
const CPU_CLOCK: f64 = 5_000_000.0;
const SAMPLE_RATE: u32 = 44100;
/// Sound CPU cycles per video line x 1e6 (56.92)
const SOUND_CYCLES_PER_LINE_X1M: i64 = (SOUND_CPU_CLOCK * 318.0 / CPU_CLOCK * 1e6) as i64;
/// MAME: the watchdog fires after 16 vblanks without a write to 0x5800
const WATCHDOG_FRAMES: u32 = 16;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Variant {
    Qbert,
}

pub struct GottliebSystem {
    pub cpu: I8088,
    pub bus: GottliebBus,
    variant: Variant,
    line_cycles: i64,
    sound_cycle_debt: i64,
    current_frame: u64,
    save_filename: String,
    save_path: String,
    state_path: String,
}

impl GottliebSystem {
    pub fn new(variant: Variant, set_name: &str, mut regions: HashMap<String, Vec<u8>>) -> Result<Self, String> {
        let rom = take_region(&mut regions, "maincpu")?;
        let audio = take_region(&mut regions, "audiocpu")?;
        let bgtiles = take_region(&mut regions, "bgtiles")?;
        let sprites = take_region(&mut regions, "sprites")?;
        let votrax = regions.remove("votrax").filter(|v| v.len() >= 0x200 && v.iter().any(|&b| b != 0));

        let sound = GottliebSound::new(&audio, votrax.as_deref(), SAMPLE_RATE);
        if !sound.has_speech() {
            println!("Gottlieb: no Votrax ROM (sc01a.bin), speech is silent");
        }
        let video = GottliebVideo::new(bgtiles, sprites, true);
        let mut bus = GottliebBus::new(rom, video, sound);
        bus.has_knocker = variant == Variant::Qbert;

        let mut sys = Self {
            cpu: I8088::new(),
            bus,
            variant,
            line_cycles: 0,
            sound_cycle_debt: 0,
            current_frame: 0,
            save_filename: set_name.to_string(),
            save_path: "user://Arcade/Gottlieb/Save".to_string(),
            state_path: "user://Arcade/Gottlieb/State".to_string(),
        };
        sys.reset();
        Ok(sys)
    }
}

impl System for GottliebSystem {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        for line in 0..LINES_PER_FRAME {
            if line == VBLANK_START {
                self.bus.video.render_frame();
                self.cpu.nmi();
                self.bus.watchdog += 1;
                if self.bus.watchdog > WATCHDOG_FRAMES {
                    println!("Gottlieb: watchdog reset (frame {})", self.current_frame);
                    self.reset();
                }
            }
            while self.line_cycles < CYCLES_PER_LINE {
                self.line_cycles += self.cpu.step(&mut self.bus) as i64;
            }
            self.line_cycles -= CYCLES_PER_LINE;

            self.sound_cycle_debt += SOUND_CYCLES_PER_LINE_X1M;
            let n = self.sound_cycle_debt / 1_000_000;
            self.sound_cycle_debt %= 1_000_000;
            self.bus.sound.run(n);
        }
        self.current_frame += 1;
    }

    /// IN1 (active high): 0x01 start 1, 0x02 start 2, 0x04 coin 1, 0x08 coin 2,
    ///   0x40 service switch (active low), 0x80 service select.
    /// IN4: the joystick is mounted 45 degrees turned: 0x01 right (down-right on screen),
    ///   0x02 left (up-left), 0x04 up (up-right), 0x08 down (down-left); bits 4-7 player 2.
    ///   A d-pad diagonal picks the matching screen diagonal.
    fn set_input(&mut self, mask: u32) {
        let on = |m: u32, b: u32| m & b != 0;
        let mut in1 = 0x40;
        if on(mask, pad::START) { in1 |= 0x01; }
        if on(mask, pad::START2) { in1 |= 0x02; }
        if on(mask, pad::COIN) || on(mask, pad::SELECT) { in1 |= 0x04; }
        self.bus.in1 = in1;

        let joy = |m: u32| -> u8 {
            let (u, d, l, r) = (on(m, pad::UP), on(m, pad::DOWN), on(m, pad::LEFT), on(m, pad::RIGHT));
            match (u, d, l, r) {
                (true, false, false, true) => 0x04,   // up-right
                (true, false, true, false) => 0x02,   // up-left
                (false, true, false, true) => 0x01,   // down-right
                (false, true, true, false) => 0x08,   // down-left
                _ => (if r { 0x01 } else { 0 }) | (if l { 0x02 } else { 0 })
                    | (if u { 0x04 } else { 0 }) | (if d { 0x08 } else { 0 }),
            }
        };
        self.bus.in4 = joy(mask) | (joy(pad::player2(mask)) << 4);
    }

    /// The frontend takes interleaved stereo: duplicate the mono board output.
    fn drain_audio(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.bus.sound.out).into_iter().flat_map(|s| [s, s]).collect()
    }
    fn sample_rate(&self) -> u32 { SAMPLE_RATE }
    fn power_on(&mut self) {}
    fn power_off(&mut self) {}
    fn has_battery(&self) -> bool { true }
    fn save_filename(&self) -> &str { &self.save_filename }
    fn save_battery_path(&self) -> &str { &self.save_path }
    fn save_state_path(&self) -> &str { &self.state_path }
    fn is_sram_dirty(&self) -> bool { self.bus.nvram_dirty }
    fn clear_sram_dirty(&mut self) { self.bus.nvram_dirty = false; }
    fn get_current_frame(&self) -> i64 { self.current_frame as i64 }
    fn get_sram(&self) -> Option<&[u8]> { Some(&self.bus.nvram) }
    fn load_sram(&mut self, data: &[u8]) {
        let n = data.len().min(0x1000);
        self.bus.nvram[..n].copy_from_slice(&data[..n]);
    }
    fn framebuffer(&self) -> &[u8] { &self.bus.video.framebuffer }

    fn reset(&mut self) {
        self.cpu.reset();
        self.line_cycles = 0;
        self.bus.watchdog = 0;
        self.bus.sound.reset();
        let _ = self.variant;
    }

    /// "dsw": the whole DIP switch byte. Q*bert: 0x01 demo sounds off, 0x02 kicker,
    /// 0x04 cocktail, 0x08 demo mode (unlimited lives), 0x10 free play.
    fn set_option(&mut self, key: &str, value: i64) {
        match key {
            "dsw" => self.bus.dsw = value as u8,
            "free_play" => {
                if value != 0 { self.bus.dsw |= 0x10 } else { self.bus.dsw &= !0x10 }
            }
            _ => {}
        }
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
            rotation: Rotation::CCW90,        // MAME ROT270
        }
    }

    fn save_state(&self) -> Vec<u8> {
        let b = &self.bus;
        let v = &b.video;
        state::encode(&self.save_filename, STATE_VERSION, &GottliebState {
            cpu: &self.cpu,
            nvram: b.nvram.to_vec(), ram: b.ram.to_vec(),
            videoram: v.videoram.to_vec(), charram: v.charram.to_vec(),
            spriteram: v.spriteram.to_vec(), paletteram: v.paletteram.to_vec(),
            output: b.output, knocker_prev: b.knocker_prev, watchdog: b.watchdog,
            sound: b.sound.save(),
            line_cycles: self.line_cycles, sound_cycle_debt: self.sound_cycle_debt,
            frame: self.current_frame,
        })
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let s: GottliebState<I8088, SoundState> = state::decode(&self.save_filename, STATE_VERSION, data)?;
        let b = &mut self.bus;
        if s.nvram.len() != b.nvram.len() || s.ram.len() != b.ram.len()
            || s.videoram.len() != 0x400 || s.charram.len() != 0x1000
            || s.spriteram.len() != 0x100 || s.paletteram.len() != 0x20 {
            return Err("save state has the wrong memory sizes".into());
        }
        self.cpu = s.cpu;
        b.nvram.copy_from_slice(&s.nvram);
        b.nvram_dirty = true;
        b.ram.copy_from_slice(&s.ram);
        b.video.videoram.copy_from_slice(&s.videoram);
        b.video.charram.copy_from_slice(&s.charram);
        b.video.spriteram.copy_from_slice(&s.spriteram);
        b.video.paletteram.copy_from_slice(&s.paletteram);
        b.video.rebuild_palette();
        b.video.control_write(s.output & 7);
        b.output = s.output;
        b.knocker_prev = s.knocker_prev;
        b.watchdog = s.watchdog;
        b.sound.restore(s.sound);
        self.line_cycles = s.line_cycles;
        self.sound_cycle_debt = s.sound_cycle_debt;
        self.current_frame = s.frame;
        Ok(())
    }
}
