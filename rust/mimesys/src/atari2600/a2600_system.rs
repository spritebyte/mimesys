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
/// First line shown, counted from the start of VSYNC, and how many lines.
/// Most games: 3 lines VSYNC + 37 VBLANK, then 192 picture lines.
const VISIBLE_Y: i32 = 31;
const VISIBLE_H: i32 = 210;

pub struct A2600System {
    cpu: M6502Cpu,
    pub bus: A2600Bus,
    current_frame: u64,
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
            save_filename: filename.to_string(),
            save_battery_path: "user://GD_EMU/A2600/Save".to_string(),
            save_state_path: "user://GD_EMU/A2600/State".to_string(),
        };
        sys.cpu.power_on(&mut sys.bus);
        Ok(sys)
    }
}

impl System for A2600System {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        self.bus.tia.frame_done = false;
        for _ in 0..MAX_CYCLES_PER_FRAME {
            // The TIA runs this cycle's 3 color clocks first: a CPU write lands at the end of
            // its cycle (same ordering as Stella), which is what RESPx positioning depends on.
            self.bus.tick();
            if !self.bus.tia.wsync {
                self.cpu.step_one_cycle(&mut self.bus);
            }
            if self.bus.tia.frame_done { break; }
        }
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
    fn framebuffer(&self) -> &[u8] { &self.bus.tia.framebuffer }

    fn reset(&mut self) {
        self.cpu = M6502Cpu::new(CpuVariant::NMOS6502);
        self.cpu.power_on(&mut self.bus);
    }

    fn display_info(&self) -> DisplayInfo {
        DisplayInfo {
            buffer_width: SCREEN_W as i32,
            buffer_height: FB_LINES as i32,
            visible_x: 0,
            visible_y: VISIBLE_Y,
            visible_width: SCREEN_W as i32,
            visible_height: VISIBLE_H,
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
