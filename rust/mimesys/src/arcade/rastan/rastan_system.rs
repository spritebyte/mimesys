//! Taito Rastan (1987). After MAME rastan.cpp (Jarek Burczynski) and the Taito chip
//! devices it uses.
//!
//! Main board: a 68000 at 8 MHz, IRQ 5 (autovectored) at vblank. Video is a PC080SN
//! tilemap chip and a PC090OJ sprite chip; the sound board (Z80, YM2151, MSM5205) talks
//! to the main CPU through a PC060HA.
//!
//! Main map: 000000 ROM; 10C000 RAM; 200000 palette; 380000 sprite colour bank / coin
//! lockout; 390000-39000B inputs and DIP switches; 3C0000 watchdog; 3E0001 / 3E0003
//! PC060HA; C00000 PC080SN RAM; C20000 y scroll; C40000 x scroll; C50000 PC080SN
//! control; D00000 PC090OJ RAM.

use std::collections::HashMap;
use serde::{Deserialize, Serialize};

use crate::arcade::rastan::rastan_sound::{Sound, SoundState};
use crate::arcade::rastan::rastan_video::{Video, SCREEN_H, SCREEN_W};
use crate::arcade::{state, take_region};
use crate::common::cpu::m68000::{M68000, M68kBus};
use crate::common::input as pad;
use crate::common::system::{DisplayInfo, Rotation, System};

const STATE_VERSION: u32 = 1;
const MAIN_CLOCK: u64 = 8_000_000;
const LINES: u32 = 256;
const LINE_RATE: u64 = 60 * LINES as u64;
/// first line of vblank (lines 8-247 are shown)
const VBLANK_LINE: u32 = 248;

pub struct RastanBus {
    rom: Vec<u8>,
    ram: Vec<u8>,
    pub video: Video,
    pub sound: Sound,
    pub irq5: bool,
    /// P1, P2, SPECIAL, SYSTEM, DSWA, DSWB
    pub inputs: [u8; 6],
}

/// Write one byte of a big-endian word array.
fn set_word_byte(mem: &mut [u16], a: u32, v: u8) {
    let i = (a as usize >> 1) % mem.len();
    mem[i] = if a & 1 == 0 { (mem[i] & 0x00FF) | (v as u16) << 8 } else { (mem[i] & 0xFF00) | v as u16 };
}

impl RastanBus {
    fn read16(&mut self, a: u32) -> u16 {
        match a {
            0x000000..=0x05FFFF => (self.rom[a as usize] as u16) << 8 | self.rom[a as usize + 1] as u16,
            0x10C000..=0x10FFFF => {
                let i = (a - 0x10C000) as usize;
                (self.ram[i] as u16) << 8 | self.ram[i + 1] as u16
            }
            0x200000..=0x200FFF => self.video.palette[((a - 0x200000) >> 1) as usize],
            0x390000..=0x39000F => self.inputs.get(((a - 0x390000) >> 1) as usize).copied().unwrap_or(0xFF) as u16,
            0x3E0000..=0x3E0003 => 0xFF00 | if a & 2 != 0 { self.sound.bus.ciu.master_comm_r() as u16 } else { 0xFF },
            0xC00000..=0xC0FFFF => self.video.tile_ram[((a - 0xC00000) >> 1) as usize],
            0xD00000..=0xD03FFF => self.video.sprite_ram[((a - 0xD00000) >> 1) as usize],
            _ => 0xFFFF,
        }
    }

    fn write16(&mut self, a: u32, v: u16) {
        match a {
            0x10C000..=0x10FFFF => {
                let i = (a - 0x10C000) as usize;
                self.ram[i] = (v >> 8) as u8;
                self.ram[i + 1] = v as u8;
            }
            0x200000..=0x200FFF => self.video.palette[((a - 0x200000) >> 1) as usize] = v,
            0x380000..=0x380001 => self.video.sprite_ctrl = v,
            0x3E0000..=0x3E0003 => self.port_w(a | 1, v as u8),
            0xC00000..=0xC0FFFF => self.video.tile_ram[((a - 0xC00000) >> 1) as usize] = v,
            0xC20000..=0xC20003 => self.video.yscroll[((a >> 1) & 1) as usize] = v,
            0xC40000..=0xC40003 => self.video.xscroll[((a >> 1) & 1) as usize] = v,
            0xC50000..=0xC50001 => self.video.ctrl = v,
            0xD00000..=0xD03FFF => self.video.sprite_ram[((a - 0xD00000) >> 1) as usize] = v,
            _ => {}   // 350008, watchdog
        }
    }

    /// The PC060HA sits on the low byte.
    fn port_w(&mut self, a: u32, v: u8) {
        if a & 1 == 0 { return; }
        let ciu = &mut self.sound.bus.ciu;
        if a & 2 == 0 { ciu.master_port_w(v) } else { ciu.master_comm_w(v) }
    }

    #[cfg(test)]
    pub fn ram_word(&self, a: u32) -> u16 { let i = (a - 0x10C000) as usize; (self.ram[i] as u16) << 8 | self.ram[i + 1] as u16 }
}

impl M68kBus for RastanBus {
    fn read_byte(&mut self, a: u32) -> u8 {
        match a {
            0x000000..=0x05FFFF => self.rom[a as usize],
            0x10C000..=0x10FFFF => self.ram[(a - 0x10C000) as usize],
            0x3E0000..=0x3E0003 => {
                // reads have side effects: only the low byte touches the chip
                if a & 1 == 0 { 0xFF } else { self.read16(a & !1) as u8 }
            }
            _ => { let w = self.read16(a & !1); if a & 1 == 0 { (w >> 8) as u8 } else { w as u8 } }
        }
    }

    fn read_word(&mut self, a: u32) -> u16 { self.read16(a) }

    fn write_byte(&mut self, a: u32, v: u8) {
        match a {
            0x10C000..=0x10FFFF => self.ram[(a - 0x10C000) as usize] = v,
            0x200000..=0x200FFF => set_word_byte(&mut self.video.palette, a - 0x200000, v),
            0x380000..=0x380001 => if a & 1 == 1 { self.video.sprite_ctrl = v as u16 },
            0x3E0000..=0x3E0003 => self.port_w(a, v),
            0xC00000..=0xC0FFFF => set_word_byte(&mut self.video.tile_ram, a - 0xC00000, v),
            0xD00000..=0xD03FFF => set_word_byte(&mut self.video.sprite_ram, a - 0xD00000, v),
            // scroll / control registers (MAME COMBINE_DATA)
            0xC20000..=0xC20003 => set_word_byte(&mut self.video.yscroll, a & 3, v),
            0xC40000..=0xC40003 => set_word_byte(&mut self.video.xscroll, a & 3, v),
            0xC50000..=0xC50001 => set_word_byte(std::slice::from_mut(&mut self.video.ctrl), a & 1, v),
            _ => {}
        }
    }

    fn write_word(&mut self, a: u32, v: u16) { self.write16(a, v) }

    fn interrupt_ack(&mut self, level: u8) -> Option<u8> {
        if level == 5 { self.irq5 = false; }   // HOLD_LINE
        None
    }
}

#[derive(Serialize)]
struct MainStateRef<'a> { ram: &'a [u8], video: &'a Video, irq5: bool }

#[derive(Deserialize)]
struct MainState { ram: Vec<u8>, video: Video, irq5: bool }

pub struct RastanSystem {
    cpu: M68000,
    pub bus: RastanBus,
    main_phase: u64,
    owed: i64,
    current_frame: u64,
    save_filename: String,
    save_path: String,
    state_path: String,
}

impl RastanSystem {
    pub fn new(set_name: &str, mut regions: HashMap<String, Vec<u8>>) -> Result<Self, String> {
        let mut rom = take_region(&mut regions, "maincpu")?;
        rom.resize(0x60000, 0xFF);
        let audio = take_region(&mut regions, "audiocpu")?;
        let tiles = take_region(&mut regions, "pc080sn")?;
        let sprites = take_region(&mut regions, "pc090oj")?;
        let adpcm = take_region(&mut regions, "adpcm")?;
        let mut bus = RastanBus {
            rom, ram: vec![0; 0x4000], video: Video::new(tiles, sprites)?, sound: Sound::new(audio, adpcm),
            irq5: false,
            inputs: [0xFF, 0xFF, 0x8F, 0x1F, 0xFE, 0xFF],
        };
        let mut cpu = M68000::new();
        cpu.reset(&mut bus);
        Ok(Self {
            cpu, bus, main_phase: 0, owed: 0, current_frame: 0,
            save_filename: set_name.to_string(),
            save_path: "user://Arcade/Taito/Save".to_string(),
            state_path: "user://Arcade/Taito/State".to_string(),
        })
    }

    fn run_main_line(&mut self) {
        self.main_phase += MAIN_CLOCK;
        self.owed += (self.main_phase / LINE_RATE) as i64;
        self.main_phase %= LINE_RATE;
        while self.owed > 0 {
            self.cpu.ipl = if self.bus.irq5 { 5 } else { 0 };
            if self.cpu.halted { self.owed = 0; break; }
            self.owed -= self.cpu.step(&mut self.bus) as i64;
        }
    }
}

impl System for RastanSystem {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        for line in 0..LINES {
            if line == VBLANK_LINE {
                self.bus.video.render();
                self.bus.irq5 = true;
            }
            self.run_main_line();
            self.bus.sound.run(1, LINE_RATE);
        }
        self.current_frame += 1;
    }

    fn set_input(&mut self, m: u32) {
        let on = |m: u32, b: u32| m & b != 0;
        // P1 / P2, active low: 01 up, 02 down, 04 left, 08 right, 10 attack, 20 jump
        let joy = |m: u32| -> u8 {
            let mut v = 0xFF;
            if on(m, pad::UP) { v &= !0x01; }
            if on(m, pad::DOWN) { v &= !0x02; }
            if on(m, pad::LEFT) { v &= !0x04; }
            if on(m, pad::RIGHT) { v &= !0x08; }
            if on(m, pad::B) { v &= !0x10; }
            if on(m, pad::A) { v &= !0x20; }
            v
        };
        self.bus.inputs[0] = joy(m);
        self.bus.inputs[1] = joy(pad::player2(m));
        // SYSTEM: 01 service, 04 tilt, 08 start 1, 10 start 2 (active low); 20 / 40 coins (active high)
        let mut sys = 0x1F;
        if on(m, pad::START) { sys &= !0x08; }
        if on(m, pad::START2) { sys &= !0x10; }
        if on(m, pad::COIN) || on(m, pad::SELECT) { sys |= 0x20; }
        if on(m, pad::X) { sys &= !0x01; }
        self.bus.inputs[3] = sys;
    }

    fn drain_audio(&mut self) -> Vec<f32> { std::mem::take(&mut self.bus.sound.out) }
    fn sample_rate(&self) -> u32 { 44100 }
    fn power_on(&mut self) {}
    fn power_off(&mut self) {}
    fn has_battery(&self) -> bool { false }
    fn save_filename(&self) -> &str { &self.save_filename }
    fn save_battery_path(&self) -> &str { &self.save_path }
    fn save_state_path(&self) -> &str { &self.state_path }
    fn is_sram_dirty(&self) -> bool { false }
    fn clear_sram_dirty(&mut self) {}
    fn get_current_frame(&self) -> i64 { self.current_frame as i64 }
    fn get_sram(&self) -> Option<&[u8]> { None }
    fn load_sram(&mut self, _data: &[u8]) {}
    fn framebuffer(&self) -> &[u8] { &self.bus.video.framebuffer }

    fn reset(&mut self) {
        self.bus.irq5 = false;
        let (audio, adpcm) = self.bus.sound.take_roms();
        self.bus.sound = Sound::new(audio, adpcm);
        self.cpu = M68000::new();
        self.cpu.reset(&mut self.bus);
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
        let b = &self.bus;
        let main = MainStateRef { ram: &b.ram, video: &b.video, irq5: b.irq5 };
        let s = (&self.cpu, main, b.sound.save(), self.main_phase, self.owed, self.current_frame);
        state::encode(&self.save_filename, STATE_VERSION, &s)
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let (cpu, main, sound, mp, owed, frame): (M68000, MainState, SoundState, u64, i64, u64) =
            state::decode(&self.save_filename, STATE_VERSION, data)?;
        let b = &mut self.bus;
        if main.ram.len() != b.ram.len() { return Err("save state doesn't fit this board".into()); }
        b.ram = main.ram;
        let mut video = main.video;
        video.keep_static(&mut b.video);
        b.video = video;
        b.irq5 = main.irq5;
        b.sound.load(sound);
        self.cpu = cpu;
        self.main_phase = mp;
        self.owed = owed;
        self.current_frame = frame;
        Ok(())
    }
}
