// Sega Master System (1986) and Game Gear (1990).
// Z80 at 3.58 MHz, 8 KB RAM, SMS VDP (mode 4), SN76489 sound (Sega version).
// The Game Gear is the same machine with a 160 x 144 window, 4096 colors,
// stereo sound, and a START button instead of PAUSE.
//
// Timing (NTSC): 262 lines x 228 CPU cycles, 59.92 Hz. The VDP raises a frame interrupt
// after line 192 and (optionally) a line interrupt every R10+1 lines; both go to the Z80's
// maskable /INT (IM 1, level triggered). PAUSE on the SMS is the Z80's NMI.
//
// Boots the cartridge directly (no BIOS needed).
//
// Controls (unified input mask, see common/input.rs):
//   D-pad   D-pad
//   B       button 1 (left button: usually "start" / main action)
//   A       button 2
//   START   SMS: PAUSE (console button).  Game Gear: START

use serde::{Deserialize, Serialize};
use crate::common::cpu::z80::{Z80Cpu, Z80State};
use crate::common::system::{DisplayInfo, Rotation, System};
use crate::common::audio::sn76489::Sn76489;
use crate::common::input as pad;
use crate::sms::sms_bus::SmsBus;
use crate::sms::sms_mapper::SmsMapper;
use crate::sms::sms_vdp::{SmsVdp, LINES_PER_FRAME, SCREEN_H, SCREEN_W};

const CYCLES_PER_LINE: i32 = 228;
const SAMPLE_RATE: u32 = 44_100;
const STATE_VERSION: u32 = 1;

pub struct SmsSystem {
    cpu: Z80Cpu,
    pub bus: SmsBus,
    line_cycles: i32,
    pause_prev: bool,
    pause_pending: bool,
    current_frame: u64,
    save_filename: String,
    save_battery_path: String,
    save_state_path: String,
}

#[derive(Serialize, Deserialize)]
struct SmsState {
    version: u32,
    rom_len: usize,
    game_gear: bool,
    cpu: Z80State,
    vdp: SmsVdp,
    psg: Sn76489,
    mapper: SmsMapper,
    #[serde(with = "serde_bytes")]
    ram: Vec<u8>,
    io_control: u8,
    line_cycles: i32,
    pause_prev: bool,
    frame: u64,
}

impl SmsSystem {
    pub fn from_rom(rom: &[u8], filename: &str, _bios: &[u8], game_gear: bool) -> Result<Self, String> {
        // Some dumps carry a 512-byte copier header in front
        let rom = if rom.len() % 0x4000 == 512 { &rom[512..] } else { rom };
        if rom.len() < 0x2000 {
            return Err(format!("ROM is only {} bytes", rom.len()));
        }
        let dir = if game_gear { "GG" } else { "SMS" };
        let mut sys = Self {
            cpu: Z80Cpu::new(),
            bus: SmsBus::new(rom.to_vec(), game_gear, SAMPLE_RATE),
            line_cycles: 0,
            pause_prev: false,
            pause_pending: false,
            current_frame: 0,
            save_filename: filename.to_string(),
            save_battery_path: format!("user://GD_EMU/{dir}/Save"),
            save_state_path: format!("user://GD_EMU/{dir}/State"),
        };
        sys.reset();
        println!("{}: {} KB, {:?} mapper", if game_gear { "Game Gear" } else { "Master System" },
                 rom.len() / 1024, sys.bus.mapper.kind);
        Ok(sys)
    }
}

impl System for SmsSystem {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        if self.pause_pending {
            self.pause_pending = false;
            self.line_cycles += self.cpu.nmi(&mut self.bus) as i32;
        }
        for line in 0..LINES_PER_FRAME {
            self.bus.current_line = line;
            self.bus.vdp.start_line(line);
            while self.line_cycles < CYCLES_PER_LINE {
                let mut c = 0;
                if self.bus.vdp.irq_line() {
                    c = self.cpu.irq(&mut self.bus, 0xFF) as i32;
                }
                if c == 0 {
                    c = self.cpu.step(&mut self.bus) as i32;
                }
                self.bus.psg.run(c as u32);
                self.line_cycles += c;
            }
            self.line_cycles -= CYCLES_PER_LINE;
        }
        self.current_frame += 1;
    }

    fn set_input(&mut self, mask: u32) {
        let on = |b: u32| mask & b != 0;
        let mut dc = 0xFFu8;
        if on(pad::UP) { dc &= !0x01; }
        if on(pad::DOWN) { dc &= !0x02; }
        if on(pad::LEFT) { dc &= !0x04; }
        if on(pad::RIGHT) { dc &= !0x08; }
        if on(pad::B) { dc &= !0x10; }
        if on(pad::A) { dc &= !0x20; }
        self.bus.port_dc = dc;
        self.bus.port_dd = 0xFF;

        let start = on(pad::START);
        if self.bus.game_gear {
            self.bus.gg_start = start;
        } else if start && !self.pause_prev {
            self.pause_pending = true;   // PAUSE is an edge-triggered NMI
        }
        self.pause_prev = start;
    }

    fn drain_audio(&mut self) -> Vec<f32> { std::mem::take(&mut self.bus.psg.out) }
    fn sample_rate(&self) -> u32 { SAMPLE_RATE }
    fn power_on(&mut self) {}
    fn power_off(&mut self) {}

    // Many games have battery RAM, and there's no header flag for it. Report a battery so
    // the frontend loads an existing .sav; get_sram() only returns data once the game has
    // actually switched cartridge RAM on.
    fn has_battery(&self) -> bool { true }
    fn save_filename(&self) -> &str { &self.save_filename }
    fn save_battery_path(&self) -> &str { &self.save_battery_path }
    fn save_state_path(&self) -> &str { &self.save_state_path }
    fn is_sram_dirty(&self) -> bool { self.bus.mapper.ram_dirty }
    fn clear_sram_dirty(&mut self) { self.bus.mapper.ram_dirty = false; }
    fn get_current_frame(&self) -> i64 { self.current_frame as i64 }
    fn get_sram(&self) -> Option<&[u8]> {
        if self.bus.mapper.ram_used { Some(&self.bus.mapper.cart_ram) } else { None }
    }
    fn load_sram(&mut self, data: &[u8]) {
        let n = data.len().min(self.bus.mapper.cart_ram.len());
        self.bus.mapper.cart_ram[..n].copy_from_slice(&data[..n]);
        self.bus.mapper.ram_used = true;
    }
    fn framebuffer(&self) -> &[u8] { &self.bus.vdp.framebuffer }

    fn reset(&mut self) {
        self.cpu = Z80Cpu::new();
        self.cpu.sp = 0xDFF0;     // where the BIOS would leave it
        self.bus.vdp.reset();
        self.bus.io_control = 0xFF;
        self.line_cycles = 0;
        self.pause_pending = false;
    }

    fn display_info(&self) -> DisplayInfo {
        if self.bus.game_gear {
            DisplayInfo {
                buffer_width: SCREEN_W as i32, buffer_height: SCREEN_H as i32,
                visible_x: 48, visible_y: 24, visible_width: 160, visible_height: 144,
                target_aspect_ratio: 160.0 / 144.0,
                rotation: Rotation::None,
            }
        } else {
            DisplayInfo {
                buffer_width: SCREEN_W as i32, buffer_height: SCREEN_H as i32,
                visible_x: 0, visible_y: 0, visible_width: SCREEN_W as i32, visible_height: SCREEN_H as i32,
                target_aspect_ratio: 4.0 / 3.0,
                rotation: Rotation::None,
            }
        }
    }

    fn save_state(&self) -> Vec<u8> {
        let state = SmsState {
            version: STATE_VERSION,
            rom_len: self.bus.mapper.rom_len(),
            game_gear: self.bus.game_gear,
            cpu: self.cpu.save_state(),
            vdp: self.bus.vdp.clone(),
            psg: self.bus.psg.clone(),
            mapper: self.bus.mapper.clone(),
            ram: self.bus.ram.clone(),
            io_control: self.bus.io_control,
            line_cycles: self.line_cycles,
            pause_prev: self.pause_prev,
            frame: self.current_frame,
        };
        let config = bincode::config::standard().with_fixed_int_encoding();
        bincode::serde::encode_to_vec(&state, config).unwrap_or_default()
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let config = bincode::config::standard().with_fixed_int_encoding();
        let (s, _): (SmsState, usize) = bincode::serde::decode_from_slice(data, config)
            .map_err(|e| format!("Failed to decode SMS save state: {e:?}"))?;
        if s.version != STATE_VERSION {
            return Err(format!("save state version {} (expected {})", s.version, STATE_VERSION));
        }
        if s.rom_len != self.bus.mapper.rom_len() || s.game_gear != self.bus.game_gear {
            return Err("save state is from a different game".into());
        }
        self.cpu.load_state(&s.cpu);
        self.bus.vdp.load_state(s.vdp);
        self.bus.psg.load_state(s.psg);
        self.bus.mapper.load_state(s.mapper);
        self.bus.ram = s.ram;
        self.bus.io_control = s.io_control;
        self.line_cycles = s.line_cycles;
        self.pause_prev = s.pause_prev;
        self.current_frame = s.frame;
        // redraw so the restored picture shows right away
        let status = self.bus.vdp.status;
        for y in 0..SCREEN_H { self.bus.vdp.render_line(y); }
        self.bus.vdp.status = status;
        Ok(())
    }
}
