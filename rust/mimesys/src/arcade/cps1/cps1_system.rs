//! Capcom CPS1 (CP System). After MAME capcom/cps1.cpp (BSD-3-Clause, Paul Leaman et al.).
//!
//! A-board: 68000 at 10 MHz (12 MHz on later boards), IRQ 2 at vblank (line 240), cleared
//! by the interrupt acknowledge. Sound board (Z80, YM2151, OKI6295): cps1_sound.rs, or on
//! the last games the Q-Sound board (Kabuki Z80 + Q-Sound DSP): cps1_qsound.rs. Video
//! (CPS-A, CPS-B): cps1_video.rs. Per-game CPS-B / bank mapper data: cps1_config.rs;
//! per-set DIP defaults and quirks: cps1_sets.rs.
//!
//! Main map: 000000-3FFFFF ROM; 800000-800007 R players (P1 low byte, P2 high byte);
//! 800018-80001F R system inputs + DIP A/B/C (in the high byte); 800030 W coin control;
//! 800100-80013F W CPS-A; 800140-80017F CPS-B (8001C0 on sf2ee / sf2ue); 800180 W sound
//! command; 800188 W second sound latch; 900000-92FFFF gfx RAM; FF0000-FFFFFF RAM.
//! Q-Sound boards instead: 000000-1FFFFF ROM; F00000 R raw sound ROM (Slam Masters
//! protection); F18000 / F1E000 the two RAMs shared with the Z80 (low byte of each word);
//! F1C000 / F1C002 R players 3 / 4; F1C006 the 93C46 EEPROM (game settings, no DIPs).

use std::collections::HashMap;
use serde::{Deserialize, Serialize};

use super::cps1_config::{config, Config};
use super::cps1_sets::{set_info, SetInfo};
use super::cps_input::{cps1_layout, InputLayout};
use super::cps1_qsound::{QSoundBoard, QState, QStateRef};
use super::cps1_sound::{Sound, SoundState, SoundStateRef};
use crate::common::io::eeprom_serial::{EepromSerial, Kind as EepromKind};
use super::cps1_video::{Video, GFXRAM_SIZE, SCREEN_H, SCREEN_W};
use crate::arcade::{state, take_region};
use crate::common::cpu::m68000::{M68000, M68kBus};
use crate::common::input as pad;
use crate::common::system::{DisplayInfo, Rotation, System};

const STATE_VERSION: u32 = 2;
/// 512 dots at 8 MHz per line, 262 lines
pub const LINES: u32 = 262;
pub const LINE_RATE: u64 = 15_625;
const VBLANK_START: u32 = 240;

/// The two sound boards CPS1 games use.
pub enum SoundBoard { Classic(Sound), QSound(QSoundBoard) }

impl SoundBoard {
    fn run(&mut self, num: u64, den: u64) {
        match self { SoundBoard::Classic(s) => s.run(num, den), SoundBoard::QSound(q) => q.run(num, den) }
    }
    fn take_out(&mut self) -> Vec<f32> {
        match self { SoundBoard::Classic(s) => std::mem::take(&mut s.out), SoundBoard::QSound(q) => std::mem::take(&mut q.out) }
    }
    fn reset(&mut self) {
        match self { SoundBoard::Classic(s) => s.reset(), SoundBoard::QSound(q) => q.reset() }
    }
}

#[derive(Serialize)]
enum SoundSaveRef<'a> { Classic(SoundStateRef<'a>), QSound(QStateRef<'a>) }

#[derive(Deserialize)]
enum SoundSave { Classic(SoundState), QSound(QState) }

pub struct Cps1Bus {
    rom: Vec<u8>,
    ram: Vec<u8>,
    pub video: Video,
    pub sound: SoundBoard,
    /// Q-Sound boards: game settings EEPROM
    pub eeprom: EepromSerial,
    /// encrypted sound ROM as the 68000 sees it at F00000 (Slam Masters), else empty
    audiorom_raw: Vec<u8>,
    /// 68000 clocks since power on (EEPROM timing)
    pub cycle: u64,
    pub irq2: bool,
    /// active-low: IN0 (coins, starts, service), IN1 (P1 low, P2 high), IN2 / IN3 (players
    /// 3 / 4 or the kick buttons; on the CPS-B or at F1C000 / F1C002)
    pub in0: u8,
    pub in1: u16,
    pub in2: u16,
    pub in3: u16,
    pub dsw: [u8; 3],
    cpsb_base: u32,
}

impl Cps1Bus {
    fn qsound(&mut self) -> Option<&mut QSoundBoard> {
        match &mut self.sound { SoundBoard::QSound(q) => Some(q), _ => None }
    }

    fn read16(&mut self, a: u32) -> u16 {
        if a >= 0xF00000 && a < 0xFF0000 {
            return self.read_qsound_area(a);
        }
        match a {
            0x000000..=0x3FFFFF => {
                let i = a as usize;
                if i + 1 < self.rom.len() { (self.rom[i] as u16) << 8 | self.rom[i + 1] as u16 } else { 0xFFFF }
            }
            0x800000..=0x800007 => self.in1,
            0x800018..=0x80001F => {
                let v = match (a - 0x800018) >> 1 { 0 => self.in0, k => self.dsw[k as usize - 1] };
                (v as u16) << 8 | 0xFF
            }
            0x900000..=0x92FFFF => self.video.gfxram_word((a - 0x900000) as usize),
            0xFF0000..=0xFFFFFF => {
                let i = (a & 0xFFFF) as usize;
                (self.ram[i] as u16) << 8 | self.ram[i + 1] as u16
            }
            _ if a >= self.cpsb_base && a < self.cpsb_base + 0x40 => {
                self.video.cps_b_read(((a - self.cpsb_base) >> 1) as usize, self.in2, self.in3)
            }
            _ => 0xFFFF,
        }
    }

    fn read_qsound_area(&mut self, a: u32) -> u16 {
        let cycle = self.cycle;
        let raw = (a.wrapping_sub(0xF00000) >> 1) as usize;
        if a <= 0xF0FFFF { return self.audiorom_raw.get(raw).map(|&b| 0xFF00 | b as u16).unwrap_or(0); }
        if a == 0xF1C006 || a == 0xF1C007 {
            return if self.qsound().is_some() { self.eeprom.do_read(cycle) as u16 } else { 0xFFFF };
        }
        if self.qsound().is_none() { return 0xFFFF; }
        match a {
            0xF1C000..=0xF1C001 => return self.in2,
            0xF1C002..=0xF1C003 => return self.in3,
            _ => {}
        }
        let Some(q) = self.qsound() else { return 0xFFFF };
        match a {
            0xF18000..=0xF19FFF => 0xFF00 | q.bus.shared0[((a - 0xF18000) >> 1) as usize] as u16,
            0xF1E000..=0xF1FFFF => 0xFF00 | q.bus.shared1[((a - 0xF1E000) >> 1) as usize] as u16,
            _ => 0xFFFF,
        }
    }

    fn write_qsound_area(&mut self, a: u32, v: u16, mask: u16) {
        let cycle = self.cycle;
        if a == 0xF1C006 {
            if mask & 0x00FF != 0 && self.qsound().is_some() {
                // MAME port field order: DI, CLK, CS
                self.eeprom.di_write(v & 0x01 != 0);
                self.eeprom.clk_write(v & 0x40 != 0, cycle);
                self.eeprom.cs_write(v & 0x80 != 0, cycle);
            }
            return;
        }
        if mask & 0x00FF == 0 { return; }
        let Some(q) = self.qsound() else { return };
        match a {
            0xF18000..=0xF19FFF => q.bus.shared0[((a - 0xF18000) >> 1) as usize] = v as u8,
            0xF1E000..=0xF1FFFF => q.bus.shared1[((a - 0xF1E000) >> 1) as usize] = v as u8,
            _ => {}
        }
    }

    /// 16-bit write with a lane mask (FF00 = high byte only, 00FF = low byte only).
    fn write16(&mut self, a: u32, v: u16, mask: u16) {
        if a >= 0xF00000 && a < 0xFF0000 {
            return self.write_qsound_area(a, v, mask);
        }
        let classic = match &mut self.sound { SoundBoard::Classic(s) => Some(s), _ => None };
        match a {
            0x800100..=0x80013F => self.video.cps_a_write(((a - 0x800100) >> 1) as usize, v, mask),
            0x800180..=0x800187 => if let Some(s) = classic {
                // MAME cps1_soundlatch_w: the low byte if it's written, else the high byte
                s.bus.latch[0] = if mask & 0x00FF != 0 { v as u8 } else { (v >> 8) as u8 };
            },
            0x800188..=0x80018F => if let Some(s) = classic {
                if mask & 0x00FF != 0 { s.bus.latch[1] = v as u8 }
            },
            0x900000..=0x92FFFF => {
                let i = (a - 0x900000) as usize % GFXRAM_SIZE;
                if mask & 0xFF00 != 0 { self.video.gfxram[i] = (v >> 8) as u8; }
                if mask & 0x00FF != 0 { self.video.gfxram[i + 1] = v as u8; }
            }
            0xFF0000..=0xFFFFFF => {
                let i = (a & 0xFFFF) as usize;
                if mask & 0xFF00 != 0 { self.ram[i] = (v >> 8) as u8; }
                if mask & 0x00FF != 0 { self.ram[i + 1] = v as u8; }
            }
            _ if a >= self.cpsb_base && a < self.cpsb_base + 0x40 => {
                self.video.cps_b_write(((a - self.cpsb_base) >> 1) as usize, v, mask)
            }
            _ => {}   // 800030 coin counters / lockouts
        }
    }

    #[cfg(test)]
    pub fn ram_word(&self, a: u32) -> u16 { let i = (a & 0xFFFF) as usize; (self.ram[i] as u16) << 8 | self.ram[i + 1] as u16 }
}

impl M68kBus for Cps1Bus {
    fn read_byte(&mut self, a: u32) -> u8 {
        let w = self.read16(a & !1);
        if a & 1 == 0 { (w >> 8) as u8 } else { w as u8 }
    }
    fn read_word(&mut self, a: u32) -> u16 { self.read16(a & !1) }
    fn write_byte(&mut self, a: u32, v: u8) {
        if a & 1 == 0 { self.write16(a & !1, (v as u16) << 8, 0xFF00) } else { self.write16(a & !1, v as u16, 0x00FF) }
    }
    fn write_word(&mut self, a: u32, v: u16) { self.write16(a & !1, v, 0xFFFF) }
    /// The acknowledge clears the vblank interrupt (MAME irqack_r).
    fn interrupt_ack(&mut self, _level: u8) -> Option<u8> { self.irq2 = false; None }
}

#[derive(Serialize)]
struct MainStateRef<'a> { #[serde(with = "serde_bytes")] ram: &'a [u8], video: &'a Video, irq2: bool, eeprom: &'a EepromSerial, cycle: u64 }

#[derive(Deserialize)]
struct MainState { #[serde(with = "serde_bytes")] ram: Vec<u8>, video: Video, irq2: bool, eeprom: EepromSerial, cycle: u64 }

pub struct Cps1System {
    cpu: M68000,
    pub bus: Cps1Bus,
    info: SetInfo,
    inputs: &'static InputLayout,
    /// extra input word (player 2 buttons 4-6, players 3 / 4) for the next frame
    ext: u32,
    main_clock: u64,
    main_phase: u64,
    owed: i64,
    current_frame: u64,
    save_filename: String,
    save_path: String,
    state_path: String,
}

impl Cps1System {
    pub fn new(set_name: &str, mut regions: HashMap<String, Vec<u8>>) -> Result<Self, String> {
        let cfg: Config = config(set_name).ok_or_else(|| format!("no CPS1 configuration for '{set_name}'"))?;
        let info = set_info(set_name).ok_or_else(|| format!("'{set_name}' isn't a supported CPS1 set"))?;
        let rom = take_region(&mut regions, "maincpu")?;
        let gfx = take_region(&mut regions, "gfx")?;
        let mut audio = take_region(&mut regions, "audiocpu")?;
        let mut audiorom_raw = Vec::new();
        let sound = match info.qsound {
            None => {
                audio.resize(0x18000, 0xFF);
                SoundBoard::Classic(Sound::new(audio, take_region(&mut regions, "oki")?))
            }
            Some(key) => {
                if info.audiorom_raw { audiorom_raw = audio[..audio.len().min(0x8000)].to_vec(); }
                let samples = take_region(&mut regions, "qsound")?;
                let dsp = regions.remove("dsp").filter(|d| d.iter().any(|&b| b != 0)).ok_or_else(|| {
                    "Q-Sound games need the DSP ROM dl-1425.bin: put MAME's qsound.zip in the BIOS folder".to_string()
                })?;
                SoundBoard::QSound(QSoundBoard::new(audio, Some(key), samples, &dsp))
            }
        };
        let main_clock: u64 = if info.clock_12mhz { 12_000_000 } else { 10_000_000 };
        let mut bus = Cps1Bus {
            rom, ram: vec![0; 0x10000],
            video: Video::new(gfx, cfg, info.stars),
            sound,
            eeprom: EepromSerial::new(EepromKind::C93xx, 128, 8, 7, main_clock / 1000),
            audiorom_raw,
            cycle: 0,
            irq2: false,
            in0: 0xFF, in1: 0xFFFF, in2: 0xFFFF, in3: 0xFFFF,
            dsw: info.dsw,
            cpsb_base: if info.cpsb_at_1c0 { 0x8001C0 } else { 0x800140 },
        };
        let mut cpu = M68000::new();
        cpu.reset(&mut bus);
        Ok(Self {
            cpu, bus, info,
            inputs: cps1_layout(set_name).or_else(|| cps1_layout("sf2")).expect("sf2 input layout"),
            ext: 0,
            main_clock,
            main_phase: 0, owed: 0, current_frame: 0,
            save_filename: set_name.to_string(),
            save_path: "user://GD_EMU/Arcade/Capcom/Save".to_string(),
            state_path: "user://GD_EMU/Arcade/Capcom/State".to_string(),
        })
    }

    fn run_main_line(&mut self) {
        self.main_phase += self.main_clock;
        self.owed += (self.main_phase / LINE_RATE) as i64;
        self.main_phase %= LINE_RATE;
        while self.owed > 0 {
            self.cpu.ipl = if self.bus.irq2 { 2 } else { 0 };
            if self.cpu.halted { self.owed = 0; break; }
            let c = self.cpu.step(&mut self.bus) as u64;
            self.bus.cycle += c;
            self.owed -= c as i64;
        }
    }
}

impl System for Cps1System {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        for line in 0..LINES {
            if line == VBLANK_START {
                // the frame is drawn, then the sprite table is latched for the next one
                self.bus.video.render();
                self.bus.video.vblank();
                self.bus.irq2 = true;
            }
            self.run_main_line();
            self.bus.sound.run(1, LINE_RATE);
        }
        self.current_frame += 1;
    }

    /// Ports from the set's MAME input layout (cps_input.rs): pads 1-4, 6-button games
    /// Y X L = punches and B A R = kicks, other games B A X Y = buttons 1-4; Select = coin,
    /// pad 1 Z = service switch (test menu).
    fn set_input(&mut self, mask: u32) {
        let p = self.inputs.ports(&pad::pads(mask, self.ext));
        let b = &mut self.bus;
        b.in0 = p[0] as u8;
        b.in1 = p[1];
        b.in2 = p[2];
        b.in3 = p[3];
    }

    fn set_extra_input(&mut self, ext: u32) { self.ext = ext; }

    fn drain_audio(&mut self) -> Vec<f32> { self.bus.sound.take_out() }
    fn sample_rate(&self) -> u32 { 44100 }
    fn power_on(&mut self) {}
    fn power_off(&mut self) {}
    /// Q-Sound games keep their settings in the EEPROM.
    fn has_battery(&self) -> bool { self.info.qsound.is_some() }
    fn save_filename(&self) -> &str { &self.save_filename }
    fn save_battery_path(&self) -> &str { &self.save_path }
    fn save_state_path(&self) -> &str { &self.state_path }
    fn is_sram_dirty(&self) -> bool { self.info.qsound.is_some() && self.bus.eeprom.dirty }
    fn clear_sram_dirty(&mut self) { self.bus.eeprom.dirty = false; }
    fn get_current_frame(&self) -> i64 { self.current_frame as i64 }
    fn get_sram(&self) -> Option<&[u8]> {
        if self.info.qsound.is_some() { Some(&self.bus.eeprom.data) } else { None }
    }
    fn load_sram(&mut self, data: &[u8]) {
        if self.info.qsound.is_some() && data.len() == self.bus.eeprom.data.len() {
            self.bus.eeprom.data.copy_from_slice(data);
        }
    }
    fn framebuffer(&self) -> &[u8] { &self.bus.video.framebuffer }

    fn reset(&mut self) {
        let b = &mut self.bus;
        b.irq2 = false;
        b.ram.fill(0);
        b.video.reset();
        b.sound.reset();
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
            rotation: match self.info.rotation { 90 => Rotation::CW90, 270 => Rotation::CCW90, _ => Rotation::None },
        }
    }

    fn save_state(&self) -> Vec<u8> {
        let b = &self.bus;
        let main = MainStateRef { ram: &b.ram, video: &b.video, irq2: b.irq2, eeprom: &b.eeprom, cycle: b.cycle };
        let sound = match &b.sound {
            SoundBoard::Classic(s) => SoundSaveRef::Classic(s.save()),
            SoundBoard::QSound(q) => SoundSaveRef::QSound(q.save()),
        };
        let s = (&self.cpu, main, sound, self.main_phase, self.owed, self.current_frame);
        state::encode(&self.save_filename, STATE_VERSION, &s)
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let (cpu, main, sound, mp, owed, frame): (M68000, MainState, SoundSave, u64, i64, u64) =
            state::decode(&self.save_filename, STATE_VERSION, data)?;
        let b = &mut self.bus;
        if main.ram.len() != b.ram.len() || main.video.gfxram.len() != GFXRAM_SIZE {
            return Err("save state doesn't fit this board".into());
        }
        b.ram = main.ram;
        let mut video = main.video;
        video.keep_static(&mut b.video);
        b.video = video;
        match (&mut b.sound, sound) {
            (SoundBoard::Classic(s), SoundSave::Classic(st)) => s.load(st),
            (SoundBoard::QSound(q), SoundSave::QSound(st)) => q.load(st),
            _ => return Err("save state is for the other sound board".into()),
        }
        b.irq2 = main.irq2;
        b.eeprom = main.eeprom;
        b.cycle = main.cycle;
        self.cpu = cpu;
        self.main_phase = mp;
        self.owed = owed;
        self.current_frame = frame;
        Ok(())
    }
}
