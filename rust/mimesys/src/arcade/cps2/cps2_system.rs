//! Capcom CPS2. After MAME capcom/cps2.cpp (BSD-3-Clause, Paul Leaman, Andreas Naive,
//! Nicola Salmoria, Charles MacDonald, David Haywood, Phil Stroffolino and others).
//!
//! 68000 at 16 MHz (MAME runs it at 73.75% to make up for unemulated wait states), its
//! program fetches decrypted (cps2_crypt.rs, key from the game's .key file). IRQ 2 at
//! vblank (line 240), IRQ 4 from the CPS-B raster counters (6 when both), both cleared by
//! the acknowledge. Sound: the Q-Sound section of cps1_qsound.rs (Z80 not encrypted).
//! Settings in a 93C46 EEPROM (16-bit), no DIP switches.
//!
//! Main map: 000000-3FFFFF ROM; 400000-40000B object output registers; 618000-619FFF
//! RAM shared with the Z80 (low bytes); 660000-664001 extra RAM; 700000 / 708000 object RAM
//! (708000 is the bank the game works on); 800100 / 804100 CPS-A; 800140 / 804140 CPS-B;
//! 804000 players 1-2; 804010 players 3-4 (or buttons 4-6 on 6-button games); 804020
//! system + EEPROM; 804030 volume / add-on status; 804040 EEPROM + Z80 reset + coin counters;
//! 8040E0 object RAM bank; 900000-92FFFF gfx RAM; FF0000-FFFFFF RAM. "Dead" boards move the
//! output registers to FFFFF0.

use std::collections::HashMap;
use serde::{Deserialize, Serialize};

use super::cps2_crypt;
use super::cps2_sets::{set_info, SetInfo};
use crate::arcade::cps1::cps_input::{cps2_layout, InputLayout};
use super::cps2_video::{gfx_decode, Cps2Video, OBJ_WORDS};
use crate::arcade::cps1::cps1_config::config;
use crate::arcade::cps1::cps1_qsound::{QSoundBoard, QState};
use crate::arcade::cps1::cps1_video::{Video, GFXRAM_SIZE, SCREEN_H, SCREEN_W};
use crate::arcade::{state, take_region};
use crate::common::cpu::m68000::{M68000, M68kBus};
use crate::common::input as pad;
use crate::common::io::eeprom_serial::{EepromSerial, Kind as EepromKind};
use crate::common::system::{DisplayInfo, Rotation, System};

const STATE_VERSION: u32 = 1;
pub const LINES: u32 = 262;
pub const LINE_RATE: u64 = 15_625;
const VBLANK_START: u32 = 240;
/// 16 MHz x 0.7375 (MAME's clock scale)
pub const MAIN_CLOCK: u64 = 11_800_000;

pub struct Cps2Bus {
    rom: Vec<u8>,
    /// the opcode view of the ROM (decrypted)
    ops: Vec<u8>,
    ram: Vec<u8>,
    extra_ram: Vec<u8>,
    pub video: Cps2Video,
    pub sound: QSoundBoard,
    pub eeprom: EepromSerial,
    pub irq2: bool,
    pub irq4: bool,
    /// active low: IN0 (P1 low, P2 high), IN1 (P3 / P4 or extra buttons), IN2 (system)
    pub in0: u16,
    pub in1: u16,
    pub in2: u16,
    volume: u16,
    dead: bool,
    pub cycle: u64,
}

impl Cps2Bus {
    fn rom_word(rom: &[u8], a: usize) -> u16 {
        if a + 1 < rom.len() { (rom[a] as u16) << 8 | rom[a + 1] as u16 } else { 0xFFFF }
    }

    fn output_index(&self, a: u32) -> Option<usize> {
        if self.dead {
            if (0xFFFFF0..=0xFFFFFB).contains(&a) { return Some(((a - 0xFFFFF0) >> 1) as usize); }
        } else if (0x400000..=0x40000B).contains(&a) {
            return Some(((a - 0x400000) >> 1) as usize);
        }
        None
    }

    fn read16(&mut self, a: u32) -> u16 {
        if let Some(i) = self.output_index(a) { return self.video.output[i]; }
        match a {
            0x000000..=0x3FFFFF => Self::rom_word(&self.rom, a as usize),
            0x618000..=0x619FFF => 0xFF00 | self.sound.bus.shared0[((a - 0x618000) >> 1) as usize] as u16,
            0x660000..=0x664001 => {
                let i = (a - 0x660000) as usize;
                (self.extra_ram[i] as u16) << 8 | self.extra_ram[i + 1] as u16
            }
            0x708000..=0x70FFFF => self.video.obj_read(1, ((a - 0x708000) >> 1) as usize & (OBJ_WORDS - 1)),
            0x800140..=0x80017F => self.video.base.cps_b_read(((a - 0x800140) >> 1) as usize, 0xFFFF, 0xFFFF),
            0x804140..=0x80417F => self.video.base.cps_b_read(((a - 0x804140) >> 1) as usize, 0xFFFF, 0xFFFF),
            0x804000..=0x804001 => self.in0,
            0x804010..=0x804011 => self.in1,
            0x804020..=0x804021 => (self.in2 & !1) | self.eeprom.do_read(self.cycle) as u16,
            0x804030..=0x804031 => self.volume,
            0x8040B0..=0x8040B3 => 0xFFFF,
            0x900000..=0x92FFFF => self.video.base.gfxram_word((a - 0x900000) as usize),
            0xFF0000..=0xFFFFFF => {
                let i = (a & 0xFFFF) as usize;
                (self.ram[i] as u16) << 8 | self.ram[i + 1] as u16
            }
            _ => 0xFFFF,
        }
    }

    fn write16(&mut self, a: u32, v: u16, mask: u16) {
        if let Some(i) = self.output_index(a) {
            let o = &mut self.video.output[i];
            *o = (*o & !mask) | (v & mask);
            return;
        }
        match a {
            0x618000..=0x619FFF => if mask & 0x00FF != 0 { self.sound.bus.shared0[((a - 0x618000) >> 1) as usize] = v as u8 },
            0x660000..=0x664001 => {
                let i = (a - 0x660000) as usize;
                if mask & 0xFF00 != 0 { self.extra_ram[i] = (v >> 8) as u8; }
                if mask & 0x00FF != 0 { self.extra_ram[i + 1] = v as u8; }
            }
            0x700000..=0x701FFF => self.video.obj_write(0, ((a - 0x700000) >> 1) as usize, v, mask),
            0x708000..=0x70FFFF => self.video.obj_write(1, ((a - 0x708000) >> 1) as usize & (OBJ_WORDS - 1), v, mask),
            0x800100..=0x80013F => self.video.base.cps_a_write(((a - 0x800100) >> 1) as usize, v, mask),
            0x804100..=0x80413F => self.video.base.cps_a_write(((a - 0x804100) >> 1) as usize, v, mask),
            0x800140..=0x80017F => self.video.base.cps_b_write(((a - 0x800140) >> 1) as usize, v, mask),
            0x804140..=0x80417F => self.video.base.cps_b_write(((a - 0x804140) >> 1) as usize, v, mask),
            0x804040..=0x804041 => {
                if mask & 0xFF00 != 0 {
                    // EEPROM: DI bit 12, CLK bit 13, CS bit 14 (MAME port field order)
                    let c = self.cycle;
                    self.eeprom.di_write(v & 0x1000 != 0);
                    self.eeprom.clk_write(v & 0x2000 != 0, c);
                    self.eeprom.cs_write(v & 0x4000 != 0, c);
                }
                // bit 3: Z80 reset (held while 0); bits 0-1 coin counters, 4-7 lockouts
                if mask & 0x00FF != 0 { self.sound.set_reset(v & 0x08 == 0); }
            }
            0x8040E0..=0x8040E1 => if mask & 0x00FF != 0 { self.video.objram_bank = (v & 1) as u8 },
            0x900000..=0x92FFFF => {
                let i = (a - 0x900000) as usize % GFXRAM_SIZE;
                if mask & 0xFF00 != 0 { self.video.base.gfxram[i] = (v >> 8) as u8; }
                if mask & 0x00FF != 0 { self.video.base.gfxram[i + 1] = v as u8; }
            }
            0xFF0000..=0xFFFFFF => {
                let i = (a & 0xFFFF) as usize;
                if mask & 0xFF00 != 0 { self.ram[i] = (v >> 8) as u8; }
                if mask & 0x00FF != 0 { self.ram[i + 1] = v as u8; }
            }
            _ => {}
        }
    }

    #[cfg(test)]
    pub fn ram_word(&self, a: u32) -> u16 { let i = (a & 0xFFFF) as usize; (self.ram[i] as u16) << 8 | self.ram[i + 1] as u16 }
}

impl M68kBus for Cps2Bus {
    fn read_byte(&mut self, a: u32) -> u8 {
        let w = self.read16(a & !1);
        if a & 1 == 0 { (w >> 8) as u8 } else { w as u8 }
    }
    fn read_word(&mut self, a: u32) -> u16 { self.read16(a & !1) }
    fn write_byte(&mut self, a: u32, v: u8) {
        if a & 1 == 0 { self.write16(a & !1, (v as u16) << 8, 0xFF00) } else { self.write16(a & !1, v as u16, 0x00FF) }
    }
    fn write_word(&mut self, a: u32, v: u16) { self.write16(a & !1, v, 0xFFFF) }
    /// Opcode fetches (and PC-relative reads) see the decrypted ROM.
    fn read_program_word(&mut self, a: u32) -> u16 {
        if (a as usize) < self.ops.len() { Self::rom_word(&self.ops, a as usize & !1) } else { self.read16(a & !1) }
    }
    /// MAME irqack_r: the acknowledge clears both interrupt lines.
    fn interrupt_ack(&mut self, _level: u8) -> Option<u8> { self.irq2 = false; self.irq4 = false; None }
}

#[derive(Serialize)]
struct MainStateRef<'a> {
    #[serde(with = "serde_bytes")] ram: &'a [u8],
    #[serde(with = "serde_bytes")] extra_ram: &'a [u8],
    video: &'a Cps2Video, eeprom: &'a EepromSerial, irq2: bool, irq4: bool, cycle: u64,
}

#[derive(Deserialize)]
struct MainState {
    #[serde(with = "serde_bytes")] ram: Vec<u8>,
    #[serde(with = "serde_bytes")] extra_ram: Vec<u8>,
    video: Cps2Video, eeprom: EepromSerial, irq2: bool, irq4: bool, cycle: u64,
}

pub struct Cps2System {
    cpu: M68000,
    pub bus: Cps2Bus,
    info: SetInfo,
    inputs: &'static InputLayout,
    /// extra input word (player 2 buttons 4-6, players 3 / 4) for the next frame
    ext: u32,
    main_phase: u64,
    owed: i64,
    current_frame: u64,
    save_filename: String,
    save_path: String,
    state_path: String,
}

impl Cps2System {
    pub fn new(set_name: &str, mut regions: HashMap<String, Vec<u8>>) -> Result<Self, String> {
        let info = set_info(set_name).ok_or_else(|| format!("'{set_name}' isn't a supported CPS2 set"))?;
        let cfg = config("cps2").ok_or("no CPS2 configuration")?;
        let rom = take_region(&mut regions, "maincpu")?;
        let ops = match regions.remove("key") {
            Some(k) if info.encrypted => {
                let key = cps2_crypt::parse_key(&k).ok_or("the .key file is too short")?;
                cps2_crypt::decrypt(&rom, &key)
            }
            _ if info.encrypted => return Err(format!("{set_name} needs its decryption key ({set_name}.key in the zip)")),
            _ => rom.clone(),
        };
        let mut gfx = take_region(&mut regions, "gfx")?;
        gfx_decode(&mut gfx);
        let audio = take_region(&mut regions, "audiocpu")?;
        let samples = take_region(&mut regions, "qsound")?;
        let dsp = regions.remove("dsp").filter(|d| d.iter().any(|&b| b != 0)).ok_or_else(|| {
            "CPS2 games need the Q-Sound DSP ROM dl-1425.bin: put MAME's qsound.zip in the BIOS folder".to_string()
        })?;
        let mut base = Video::new(gfx, cfg, -1);
        base.raster_on = true;
        let mut bus = Cps2Bus {
            rom, ops, ram: vec![0; 0x10000], extra_ram: vec![0; 0x4002],
            video: Cps2Video::new(base),
            sound: QSoundBoard::new(audio, None, samples, &dsp),
            eeprom: EepromSerial::new(EepromKind::C93xx, 64, 16, 6, MAIN_CLOCK / 1000),
            irq2: false, irq4: false,
            in0: 0xFFFF, in1: 0xFFFF, in2: 0xFFFF,
            // MAME cps2_qsound_volume_r: full volume, no add-on RAM / network board
            volume: if info.single_board { 0xD000 } else { 0xE021 },
            dead: info.dead,
            cycle: 0,
        };
        let mut cpu = M68000::new();
        cpu.reset(&mut bus);
        Ok(Self {
            cpu, bus, info,
            inputs: cps2_layout(set_name).or_else(|| cps2_layout("ssf2t")).expect("ssf2t input layout"),
            ext: 0,
            main_phase: 0, owed: 0, current_frame: 0,
            save_filename: set_name.to_string(),
            save_path: "user://GD_EMU/Arcade/Capcom/Save".to_string(),
            state_path: "user://GD_EMU/Arcade/Capcom/State".to_string(),
        })
    }

    fn run_main(&mut self, cycles: i64) {
        self.owed += cycles;
        while self.owed > 0 {
            self.cpu.ipl = (if self.bus.irq2 { 2 } else { 0 }) | (if self.bus.irq4 { 4 } else { 0 });
            if self.cpu.halted { self.owed = 0; break; }
            let c = self.cpu.step(&mut self.bus) as u64;
            self.bus.cycle += c;
            self.owed -= c as i64;
        }
    }

    fn line_cycles(&mut self) -> i64 {
        self.main_phase += MAIN_CLOCK;
        let c = self.main_phase / LINE_RATE;
        self.main_phase %= LINE_RATE;
        c as i64
    }
}

impl System for Cps2System {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        self.bus.video.frame_start();
        for line in 0..LINES {
            if line == VBLANK_START {
                self.bus.video.vblank();
                self.bus.irq2 = true;
            }
            let total = self.line_cycles();
            match self.bus.video.base.raster_line(line) {
                Some(delay) => {
                    // the raster interrupt comes `delay` x 2 dots into the line (512 dots)
                    let first = total * (delay as i64 * 2).min(512) / 512;
                    self.run_main(first);
                    // the lines so far are drawn with the old registers (MAME update_partial)
                    self.bus.video.render_to(line as i32 + 1);
                    self.bus.irq4 = true;
                    self.run_main(total - first);
                }
                None => self.run_main(total),
            }
            self.bus.sound.run(1, LINE_RATE);
        }
        self.current_frame += 1;
    }

    /// Ports from the set's MAME input layout (cps1::cps_input): pads 1-4, 6-button games
    /// Y X L = punches and B A R = kicks, other games B A X Y = buttons 1-4; Select = coin,
    /// pad 1 Z = service switch (test menu).
    fn set_input(&mut self, mask: u32) {
        let p = self.inputs.ports(&pad::pads(mask, self.ext));
        let b = &mut self.bus;
        b.in0 = p[0];
        b.in1 = p[1];
        b.in2 = p[2];
    }

    fn set_extra_input(&mut self, ext: u32) { self.ext = ext; }

    fn drain_audio(&mut self) -> Vec<f32> { std::mem::take(&mut self.bus.sound.out) }
    fn sample_rate(&self) -> u32 { 44100 }
    fn power_on(&mut self) {}
    fn power_off(&mut self) {}
    /// The settings EEPROM is saved like battery RAM.
    fn has_battery(&self) -> bool { true }
    fn save_filename(&self) -> &str { &self.save_filename }
    fn save_battery_path(&self) -> &str { &self.save_path }
    fn save_state_path(&self) -> &str { &self.state_path }
    fn is_sram_dirty(&self) -> bool { self.bus.eeprom.dirty }
    fn clear_sram_dirty(&mut self) { self.bus.eeprom.dirty = false; }
    fn get_current_frame(&self) -> i64 { self.current_frame as i64 }
    fn get_sram(&self) -> Option<&[u8]> { Some(&self.bus.eeprom.data) }
    fn load_sram(&mut self, data: &[u8]) {
        if data.len() == self.bus.eeprom.data.len() { self.bus.eeprom.data.copy_from_slice(data); }
    }
    fn framebuffer(&self) -> &[u8] { &self.bus.video.base.framebuffer }

    fn reset(&mut self) {
        let b = &mut self.bus;
        b.irq2 = false;
        b.irq4 = false;
        b.ram.fill(0);
        b.extra_ram.fill(0);
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
        let main = MainStateRef {
            ram: &b.ram, extra_ram: &b.extra_ram, video: &b.video, eeprom: &b.eeprom,
            irq2: b.irq2, irq4: b.irq4, cycle: b.cycle,
        };
        let s = (&self.cpu, main, b.sound.save(), self.main_phase, self.owed, self.current_frame);
        state::encode(&self.save_filename, STATE_VERSION, &s)
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let (cpu, main, sound, mp, owed, frame): (M68000, MainState, QState, u64, i64, u64) =
            state::decode(&self.save_filename, STATE_VERSION, data)?;
        let b = &mut self.bus;
        if main.ram.len() != b.ram.len() || main.video.base.gfxram.len() != GFXRAM_SIZE {
            return Err("save state doesn't fit this board".into());
        }
        b.ram = main.ram;
        b.extra_ram = main.extra_ram;
        let mut video = main.video;
        video.keep_static(&mut b.video);
        b.video = video;
        b.eeprom = main.eeprom;
        b.irq2 = main.irq2;
        b.irq4 = main.irq4;
        b.cycle = main.cycle;
        b.sound.load(sound);
        self.cpu = cpu;
        self.main_phase = mp;
        self.owed = owed;
        self.current_frame = frame;
        Ok(())
    }
}

