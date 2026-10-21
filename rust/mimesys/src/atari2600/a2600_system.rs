// Atari 2600 (VCS, 1977): 6507 CPU at 1.19 MHz, TIA (video + sound), RIOT (128 bytes RAM,
// timer, I/O), cartridge. That's the whole machine.
//
// Frame loop: 3 TIA color clocks, then one CPU cycle. When the game writes WSYNC
// the CPU sleeps until the end of the line (the TIA keeps going). A frame ends when the game
// starts VSYNC, which is how real TVs found the top of the picture.
//
// Controls (unified input mask, see common/input.rs):
//   D-pad   joystick
//   A or B  fire
//   START   console RESET switch
//   SELECT  console SELECT switch
// The difficulty switches stay on B (novice) and TV type on color.
//
// Phosphor mode: some games draw half their objects each frame (Asteroids) and rely on the
// TV's afterglow. With phosphor on, each displayed frame is the average of the last two
// (Stella does the same for those games). It's switched on for file names containing
// "asteroids"; call set_phosphor() to control it yourself.

use serde::{Deserialize, Serialize};
use crate::common::cpu::m6502::{M6502Cpu, CpuVariant, CpuState};
use crate::common::system::{DisplayInfo, Rotation, System};
use crate::common::input as pad;
use crate::atari2600::a2600_bus::A2600Bus;
use crate::atari2600::cart::Cart;
use crate::atari2600::riot::Riot;
use crate::atari2600::tia::{Tia, FB_LINES, SCREEN_W};

const SAMPLE_RATE: u32 = 44_100;
const STATE_VERSION: u32 = 1;
/// Safety net: a frame never runs longer than this many CPU cycles (~1.5 normal frames).
const MAX_CYCLES_PER_FRAME: u32 = 30_000;
/// Lines in the picture we show. Games choose where their picture starts and how tall it is
/// (most: 192 lines after 3 VSYNC + 37 VBLANK, some go much further down), so the window
/// follows the lines where the game turns VBLANK off, centered, like Stella's auto mode.
pub const OUT_H: usize = 228;
/// Before a game has shown anything: lines 31..259 (the old fixed window, extended).
const DEFAULT_TOP: usize = 31;
/// The window moves only after the game's picture has been somewhere else for this many
/// frames in a row, so a single odd frame (loading, a flash) doesn't make it jump.
const SETTLE_FRAMES: u32 = 20;

pub struct A2600System {
    cpu: M6502Cpu,
    pub bus: A2600Bus,
    current_frame: u64,
    phosphor: bool,
    prev_frame_lines: u16,
    top: usize,             // first framebuffer line shown
    want_top: usize,        // where the game's picture currently suggests
    want_frames: u32,       // for how many frames in a row
    out: Vec<u8>,           // the SCREEN_W x OUT_H picture handed to the frontend
    display: Vec<u8>,       // what framebuffer() returns when phosphor is on
    prev_raw: Vec<u8>,
    save_filename: String,
    save_battery_path: String,
    save_state_path: String,
}

#[derive(Serialize, Deserialize)]
struct A2600State {
    version: u32,
    rom_len: usize,
    cpu: CpuState,
    tia: Tia,
    riot: Riot,
    cart: Cart,
    cycles: u64,
    frame: u64,
}

impl A2600System {
    pub fn from_rom(rom: &[u8], filename: &str, _bios: &[u8]) -> Result<Self, String> {
        let cart = Cart::new(rom.to_vec())?;
        println!("Atari 2600: {} KB, {:?}{}", rom.len() / 1024, cart.scheme,
                 if cart.superchip { " + Superchip RAM" } else { "" });
        let mut sys = Self {
            cpu: M6502Cpu::new(CpuVariant::NMOS6502),   // the 6507 has working decimal mode
            bus: A2600Bus::new(cart, SAMPLE_RATE),
            current_frame: 0,
            prev_frame_lines: 0,
            top: DEFAULT_TOP,
            want_top: DEFAULT_TOP,
            want_frames: 0,
            out: vec![0; SCREEN_W * OUT_H * 4],
            phosphor: filename.to_lowercase().contains("asteroids"),
            display: vec![0; SCREEN_W * FB_LINES * 4],
            prev_raw: vec![0; SCREEN_W * FB_LINES * 4],
            save_filename: filename.to_string(),
            save_battery_path: "user://GD_EMU/A2600/Save".to_string(),
            save_state_path: "user://GD_EMU/A2600/State".to_string(),
        };
        sys.cpu.power_on(&mut sys.bus);
        Ok(sys)
    }

    /// Debug: record every TIA write during the next frame, then print the ones on lines
    /// `first..=last` (line numbers count from the start of VSYNC).
    pub fn dump_tia_writes(&mut self, first: u16, last: u16) {
        const NAMES: [&str; 0x2D] = ["VSYNC","VBLANK","WSYNC","RSYNC","NUSIZ0","NUSIZ1","COLUP0","COLUP1",
            "COLUPF","COLUBK","CTRLPF","REFP0","REFP1","PF0","PF1","PF2","RESP0","RESP1","RESM0","RESM1",
            "RESBL","AUDC0","AUDC1","AUDF0","AUDF1","AUDV0","AUDV1","GRP0","GRP1","ENAM0","ENAM1","ENABL",
            "HMP0","HMP1","HMM0","HMM1","HMBL","VDELP0","VDELP1","VDELBL","RESMP0","RESMP1","HMOVE","HMCLR","CXCLR"];
        self.bus.tia.write_log = Some(Vec::new());
        self.run_frame(0x0F << pad::KEYPAD_SHIFT);
        for (line, clock, reg, v) in self.bus.tia.write_log.take().unwrap_or_default() {
            if (first..=last).contains(&line) {
                let name = NAMES.get(reg as usize).copied().unwrap_or("?");
                println!("line {line:3} cycle {:2} (clock {clock:3}) {name:7} = {v:02X}", clock / 3);
            }
        }
    }

    /// Blend each frame with the previous one (for games that flicker on purpose).
    pub fn set_phosphor(&mut self, on: bool) { self.phosphor = on; }

    /// Where the window should start for a picture spanning lines first..=last.
    pub(crate) fn top_for(first: u16, last: u16) -> usize {
        let (first, h) = (first as i64, last as i64 - first as i64 + 1);
        let top = first - (OUT_H as i64 - h) / 2;             // center (h > OUT_H: center the overflow)
        top.clamp(0, (FB_LINES - OUT_H) as i64) as usize
    }

    /// Follow the game's picture (see OUT_H), then copy the window into `out`.
    fn update_window(&mut self) {
        if let Some((first, last)) = self.bus.tia.last_vis {
            let want = Self::top_for(first, last);
            if want == self.want_top {
                self.want_frames += 1;
            } else {
                self.want_top = want;
                self.want_frames = 1;
            }
            // first picture after power-on: adopt it quickly
            let needed = if self.current_frame < 120 { 3 } else { SETTLE_FRAMES };
            if self.want_frames >= needed {
                self.top = self.want_top;
            }
        }
        let src = if self.phosphor { &self.display } else { &self.bus.tia.framebuffer };
        let row = SCREEN_W * 4;
        self.out.copy_from_slice(&src[self.top * row..(self.top + OUT_H) * row]);
    }

    fn blend_frames(&mut self) {
        let raw = &self.bus.tia.framebuffer;
        for ((d, &r), p) in self.display.iter_mut().zip(raw.iter()).zip(self.prev_raw.iter_mut()) {
            *d = ((r as u16 + *p as u16) / 2) as u8;
            *p = r;
        }
    }
}

impl System for A2600System {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        self.bus.tia.frame_done = false;
        for _ in 0..MAX_CYCLES_PER_FRAME {
            // The TIA runs this cycle's 3 color clocks first: a CPU write lands at the end of
            // its cycle (same ordering as Stella), which is what RESPx positioning depends on.
            // WSYNC is checked *before* the clocks: when the line ends during these 3 clocks
            // they still belong to the old line, so the CPU resumes on the next cycle,
            // which is cycle 0 of the new line.
            let halted = self.bus.tia.wsync;
            self.bus.tick();
            if !halted {
                self.cpu.step_one_cycle(&mut self.bus);
            }
            if self.bus.tia.frame_done { break; }
        }
        if self.phosphor { self.blend_frames(); }
        self.update_window();
        // A steady game always has the same number of lines per frame (usually 262).
        // If this changes while playing, the picture jumps: print it to help find why.
        let lines = self.bus.tia.last_frame_lines;
        if lines != self.prev_frame_lines && self.current_frame > 2 {
            println!("2600 frame {}: {} scan lines (was {})", self.current_frame, lines, self.prev_frame_lines);
        }
        self.prev_frame_lines = lines;
        self.current_frame += 1;
    }

    fn set_input(&mut self, mask: u32) {
        let on = |b: u32| mask & b != 0;
        let mut swcha = 0xFFu8;
        if on(pad::RIGHT) { swcha &= !0x80; }
        if on(pad::LEFT) { swcha &= !0x40; }
        if on(pad::DOWN) { swcha &= !0x20; }
        if on(pad::UP) { swcha &= !0x10; }
        self.bus.riot.swcha_in = swcha;
        self.bus.tia.fire[0] = on(pad::A) || on(pad::B);
        let mut swchb = 0x0Bu8;
        if on(pad::START) { swchb &= !0x01; }
        if on(pad::SELECT) { swchb &= !0x02; }
        self.bus.riot.swchb_in = swchb;
    }

    fn drain_audio(&mut self) -> Vec<f32> { std::mem::take(&mut self.bus.tia.audio_out) }
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
    fn framebuffer(&self) -> &[u8] { &self.out }

    fn reset(&mut self) {
        self.cpu = M6502Cpu::new(CpuVariant::NMOS6502);
        self.cpu.power_on(&mut self.bus);
    }

    fn display_info(&self) -> DisplayInfo {
        DisplayInfo {
            buffer_width: SCREEN_W as i32,
            buffer_height: OUT_H as i32,
            visible_x: 0,
            visible_y: 0,
            visible_width: SCREEN_W as i32,
            visible_height: OUT_H as i32,
            target_aspect_ratio: 4.0 / 3.0,   // 2600 pixels are wide: 160 of them fill a 4:3 screen
            rotation: Rotation::None,
        }
    }

    fn save_state(&self) -> Vec<u8> {
        let state = A2600State {
            version: STATE_VERSION,
            rom_len: self.bus.cart.rom_len(),
            cpu: self.cpu.get_state(),
            tia: self.bus.tia.clone(),
            riot: self.bus.riot.clone(),
            cart: self.bus.cart.clone(),
            cycles: self.bus.cycles,
            frame: self.current_frame,
        };
        let config = bincode::config::standard().with_fixed_int_encoding();
        bincode::serde::encode_to_vec(&state, config).unwrap_or_default()
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let config = bincode::config::standard().with_fixed_int_encoding();
        let (s, _): (A2600State, usize) = bincode::serde::decode_from_slice(data, config)
            .map_err(|e| format!("Failed to decode 2600 save state: {e:?}"))?;
        if s.version != STATE_VERSION {
            return Err(format!("save state version {} (expected {})", s.version, STATE_VERSION));
        }
        if s.rom_len != self.bus.cart.rom_len() {
            return Err("save state is from a different game".into());
        }
        self.cpu.load_state(&s.cpu);
        self.bus.tia.load_state(s.tia);
        self.bus.riot = s.riot;
        self.bus.cart.load_state(s.cart);
        self.bus.cycles = s.cycles;
        self.current_frame = s.frame;
        Ok(())
    }
}
