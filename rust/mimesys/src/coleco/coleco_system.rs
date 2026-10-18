// ColecoVision (1982): Z80 at 3.58 MHz, TMS9918A video, SN76489 sound, 1 KB RAM.
//
// Timing (NTSC): 262 lines x 228 CPU cycles = 59,736 cycles per frame (59.92 Hz).
// Lines 0-191 are drawn; the VDP raises its frame flag at line 192, and when the game
// has interrupts enabled (VDP R1 bit 5) that becomes an NMI on the Z80.
//
// Needs the 8 KB BIOS: the cartridge's header points the BIOS at the game's start
// address, and games call BIOS routines for almost everything.
//
// Controls (unified input mask, see common/input.rs):
//   D-pad          joystick
//   A              left fire (main button)
//   B              right fire
//   bits 8-11      keypad: 0-9, 10 = *, 11 = #, 15 = none
//   START / SELECT keypad 1 / keypad * when no keypad key is held
//                  (most games: "press 1-8 to pick a skill level", "* to restart")

use serde::{Deserialize, Serialize};
use crate::common::cpu::z80::{Z80Cpu, Z80State};
use crate::common::system::{DisplayInfo, Rotation, System};
use crate::common::video::tms9918::{Tms9918, ACTIVE_H, ACTIVE_W, LINES_PER_FRAME};
use crate::common::audio::sn76489::Sn76489;
use crate::common::input as pad;
use crate::coleco::coleco_bus::{ColecoBus, Pad};

const CYCLES_PER_LINE: i32 = 228;
const SAMPLE_RATE: u32 = 44_100;
const STATE_VERSION: u32 = 1;

pub struct ColecoSystem {
    cpu: Z80Cpu,
    pub bus: ColecoBus,
    line_cycles: i32,       // cycles run past the end of the previous line
    nmi_prev: bool,         // NMI is edge triggered
    current_frame: u64,
    save_filename: String,
    save_battery_path: String,
    save_state_path: String,
}

#[derive(Serialize, Deserialize)]
struct ColecoState {
    version: u32,
    cart_len: usize,
    cpu: Z80State,
    vdp: Tms9918,
    psg: Sn76489,
    #[serde(with = "serde_bytes")]
    ram: Vec<u8>,
    bank: usize,
    keypad_mode: bool,
    nmi_prev: bool,
    line_cycles: i32,
    frame: u64,
}

impl ColecoSystem {
    pub fn from_rom(rom: &[u8], filename: &str, bios: &[u8]) -> Result<Self, String> {
        if bios.len() != 0x2000 {
            return Err(format!(
                "ColecoVision needs its 8 KB BIOS at user://BIOS/colecovision.rom (got {} bytes)",
                bios.len()));
        }
        if rom.is_empty() {
            return Err("cartridge ROM is empty".into());
        }
        // Header check: AA 55 (shows the title screen) or 55 AA (skips it). MegaCarts keep
        // the header in their last bank. Only a warning: some homebrew skips it.
        let header_at = if rom.len() > 0x8000 { rom.len() - 0x4000 } else { 0 };
        let h = &rom[header_at..(header_at + 2).min(rom.len())];
        if h != [0xAA, 0x55] && h != [0x55, 0xAA] {
            println!("ColecoVision: no AA55/55AA header in {filename}, trying anyway");
        }
        let mut sys = Self {
            cpu: Z80Cpu::new(),
            bus: ColecoBus::new(bios.to_vec(), rom.to_vec(), SAMPLE_RATE),
            line_cycles: 0,
            nmi_prev: false,
            current_frame: 0,
            save_filename: filename.to_string(),
            save_battery_path: "user://GD_EMU/Coleco/Save".to_string(),
            save_state_path: "user://GD_EMU/Coleco/State".to_string(),
        };
        sys.reset();
        Ok(sys)
    }

    fn check_nmi(&mut self) -> i32 {
        let level = self.bus.vdp.irq_line();
        let rising = level && !self.nmi_prev;
        self.nmi_prev = level;
        if rising { self.cpu.nmi(&mut self.bus) as i32 } else { 0 }
    }
}

impl System for ColecoSystem {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        for line in 0..LINES_PER_FRAME {
            if line < ACTIVE_H {
                self.bus.vdp.render_line(line);
            } else if line == ACTIVE_H {
                self.bus.vdp.start_vblank();
            }
            while self.line_cycles < CYCLES_PER_LINE {
                let mut c = self.check_nmi();
                c += self.cpu.step(&mut self.bus) as i32;
                self.bus.psg.run(c as u32);
                self.line_cycles += c;
            }
            self.line_cycles -= CYCLES_PER_LINE;
        }
        self.current_frame += 1;
    }

    fn set_input(&mut self, mask: u32) {
        let on = |b: u32| mask & b != 0;
        let mut key = match (mask >> pad::KEYPAD_SHIFT) & 0x0F {
            k @ 0..=11 => Some(k as u8),
            _ => None,
        };
        if key.is_none() && on(pad::START) { key = Some(1); }
        if key.is_none() && on(pad::SELECT) { key = Some(10); }
        self.bus.pads[0] = Pad {
            up: on(pad::UP), down: on(pad::DOWN), left: on(pad::LEFT), right: on(pad::RIGHT),
            fire_left: on(pad::A),
            fire_right: on(pad::B),
            key,
        };
    }

    fn drain_audio(&mut self) -> Vec<f32> { std::mem::take(&mut self.bus.psg.out) }
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
    fn framebuffer(&self) -> &[u8] { &self.bus.vdp.framebuffer }

    /// The reset button restarts the CPU and VDP; RAM and VRAM keep their contents.
    fn reset(&mut self) {
        self.cpu = Z80Cpu::new();
        self.bus.vdp.reset();
        self.bus.bank = 0;
        self.bus.keypad_mode = false;
        self.line_cycles = 0;
        self.nmi_prev = false;
    }

    fn display_info(&self) -> DisplayInfo {
        DisplayInfo {
            buffer_width: ACTIVE_W as i32,
            buffer_height: ACTIVE_H as i32,
            visible_x: 0,
            visible_y: 0,
            visible_width: ACTIVE_W as i32,
            visible_height: ACTIVE_H as i32,
            target_aspect_ratio: 4.0 / 3.0,
            rotation: Rotation::None,
        }
    }

    fn save_state(&self) -> Vec<u8> {
        let state = ColecoState {
            version: STATE_VERSION,
            cart_len: self.bus.cart_len(),
            cpu: self.cpu.save_state(),
            vdp: self.bus.vdp.clone(),
            psg: self.bus.psg.clone(),
            ram: self.bus.ram.clone(),
            bank: self.bus.bank,
            keypad_mode: self.bus.keypad_mode,
            nmi_prev: self.nmi_prev,
            line_cycles: self.line_cycles,
            frame: self.current_frame,
        };
        let config = bincode::config::standard().with_fixed_int_encoding();
        bincode::serde::encode_to_vec(&state, config).unwrap_or_default()
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let config = bincode::config::standard().with_fixed_int_encoding();
        let (s, _): (ColecoState, usize) = bincode::serde::decode_from_slice(data, config)
            .map_err(|e| format!("Failed to decode ColecoVision save state: {e:?}"))?;
        if s.version != STATE_VERSION {
            return Err(format!("save state version {} (expected {})", s.version, STATE_VERSION));
        }
        if s.cart_len != self.bus.cart_len() {
            return Err("save state is from a different cartridge".into());
        }
        self.cpu.load_state(&s.cpu);
        self.bus.vdp.load_state(s.vdp);
        self.bus.psg.load_state(s.psg);
        self.bus.ram = s.ram;
        self.bus.bank = s.bank;
        self.bus.keypad_mode = s.keypad_mode;
        self.nmi_prev = s.nmi_prev;
        self.line_cycles = s.line_cycles;
        self.current_frame = s.frame;
        // redraw so the restored frame shows right away (without touching the sprite flags)
        let status = self.bus.vdp.status;
        for y in 0..ACTIVE_H { self.bus.vdp.render_line(y); }
        self.bus.vdp.status = status;
        Ok(())
    }
}
