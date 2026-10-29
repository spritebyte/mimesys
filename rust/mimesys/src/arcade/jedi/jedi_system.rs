//! Atari Return of the Jedi (1984). After MAME jedi.cpp (Dan Boris, Aaron Giles).
//!
//! Main board: a 6502 at 2.5 MHz, three 16 KB ROM pages at 4000-7FFF, two X2212 NOVRAMs
//! (high scores and settings: RAM that's copied to EEPROM on a STORE), an ADC0809 for the
//! analog stick. Both CPUs get an IRQ from 32V (asserted at lines 64, 128, 192, 256,
//! released at 32, 96, 160, 224 or by an acknowledge write). 262 lines at 60 Hz.
//!
//! Main map: 0000 RAM; 0800 NOVRAM; 0C00 / 0C01 inputs; 1400 sound reply; 1800 ADC;
//! 1C00 NOVRAM recall; 1C80 ADC start (channel 0 = stick Y, 2 = stick X); 1D00 store;
//! 1E00 IRQ ack; 1E80-1E87 LS259 (coin counters, text bank, sound reset, video off);
//! 1F00 sound command; 1F80 ROM page; 2000-3FFF video; 4000 paged ROM; 8000 ROM.

use std::collections::HashMap;
use serde::{Deserialize, Serialize};

use crate::arcade::jedi::jedi_sound::{JediSound, JediSoundState, CLOCK as SOUND_CLOCK};
use crate::arcade::jedi::jedi_video::{JediVideo, SCREEN_H, SCREEN_W};
use crate::arcade::{state, take_region};
use crate::common::cpu::m6502::{AddressBus, CpuState, CpuVariant, M6502Cpu};
use crate::common::input as pad;
use crate::common::system::{DisplayInfo, Rotation, System};

const STATE_VERSION: u32 = 1;
const MAIN_CLOCK: u64 = 10_000_000 / 4;
const LINES: u16 = 262;
const LINE_RATE: u64 = 60 * LINES as u64;

/// Two X2212s side by side: 256 bytes of RAM, each with an EEPROM copy.
#[derive(Clone, Serialize, Deserialize)]
struct Novram {
    sram: Vec<u8>,
    eeprom: Vec<u8>,
    recall_line: bool,
    #[serde(skip)]
    dirty: bool,
}

impl Novram {
    fn new() -> Self {
        // MAME's default fill: guaranteed bad checksum, so the game sets itself up
        let mut eeprom = vec![0xFF; 0x100];
        eeprom[0x50..0x60].fill(0x00);
        Self { sram: eeprom.clone(), eeprom, recall_line: true, dirty: false }
    }
    fn store(&mut self) {
        if self.eeprom != self.sram { self.eeprom.copy_from_slice(&self.sram); self.dirty = true; }
    }
    fn recall(&mut self, state: bool) {
        if state && !self.recall_line { self.sram.copy_from_slice(&self.eeprom); }
        self.recall_line = state;
    }
}

pub struct MainBus {
    ram: Vec<u8>,
    rom: Vec<u8>,
    page: usize,
    pub video: JediVideo,
    novram: Novram,
    adc: u8,
    /// analog stick (Y, X), 0x80 centred
    stick: [u8; 2],
    in0: u8,
    outlatch: u8,
    pub irq: bool,
    vblank: bool,
    pub sound: JediSound,
}

#[derive(Serialize, Deserialize)]
struct MainState {
    ram: Vec<u8>,
    page: usize,
    video: JediVideo,
    novram: Novram,
    adc: u8,
    outlatch: u8,
    irq: bool,
}

impl MainBus {
    fn outlatch_w(&mut self, bit: u8, on: bool) {
        if on { self.outlatch |= 1 << bit } else { self.outlatch &= !(1 << bit) }
        match bit {
            4 => self.video.foreground_bank = on,
            6 => self.sound.set_running(on),
            7 => self.video.video_off = on,
            _ => {}   // coin counters, LEDs
        }
    }
}

impl MainBus {
    #[cfg(test)]
    pub fn read_ram(&self, a: usize) -> u8 { self.ram[a] }
}

impl AddressBus for MainBus {
    fn read_byte(&mut self, a: u16) -> u8 {
        match a {
            0x0000..=0x07FF => self.ram[a as usize],
            0x0800..=0x0BFF => self.novram.sram[(a & 0xFF) as usize],
            0x0C00..=0x0FFF if a & 1 == 0 => self.in0,
            0x0C00..=0x0FFF => {
                let snd = &self.sound.bus;
                0x1B | (snd.ack.pending as u8) << 5 | (snd.cmd.pending as u8) << 6 | (self.vblank as u8) << 7
            }
            0x1400..=0x17FF => self.sound.bus.ack.read(),
            0x1800..=0x1BFF => self.adc,
            0x2000..=0x27FF => self.video.background[(a - 0x2000) as usize],
            0x2800..=0x2BFF => self.video.palette_lo[(a - 0x2800) as usize],
            0x2C00..=0x2FFF => self.video.palette_hi[(a - 0x2C00) as usize],
            0x3000..=0x37BF => self.video.foreground[(a - 0x3000) as usize],
            0x37C0..=0x3BFF => self.video.sprites[(a - 0x37C0) as usize],
            0x4000..=0x7FFF => self.rom.get(0x10000 + self.page * 0x4000 + (a as usize - 0x4000)).copied().unwrap_or(0),
            0x8000..=0xFFFF => self.rom[a as usize],
            _ => 0,
        }
    }

    fn write_byte(&mut self, a: u16, v: u8) {
        match a {
            0x0000..=0x07FF => self.ram[a as usize] = v,
            0x0800..=0x0BFF => self.novram.sram[(a & 0xFF) as usize] = v,
            0x1C00..=0x1C7F => self.novram.recall(a & 1 != 0),
            0x1C80..=0x1CFF => {
                // ADC0809: converts at once here (about 100 us on the real chip)
                self.adc = match a & 7 { 0 => self.stick[0], 2 => self.stick[1], _ => 0 };
            }
            0x1D00..=0x1D7F => self.novram.store(),
            0x1E00..=0x1E7F => self.irq = false,
            0x1E80..=0x1EFF => self.outlatch_w((a & 7) as u8, v & 0x80 != 0),
            0x1F00..=0x1F7F => self.sound.bus.cmd.write(v),
            0x1F80..=0x1FFF => {
                if v & 1 != 0 { self.page = 0; }
                if v & 2 != 0 { self.page = 1; }
                if v & 4 != 0 { self.page = 2; }
            }
            0x2000..=0x27FF => self.video.background[(a - 0x2000) as usize] = v,
            0x2800..=0x2BFF => self.video.palette_lo[(a - 0x2800) as usize] = v,
            0x2C00..=0x2FFF => self.video.palette_hi[(a - 0x2C00) as usize] = v,
            0x3000..=0x37BF => self.video.foreground[(a - 0x3000) as usize] = v,
            0x37C0..=0x3BFF => self.video.sprites[(a - 0x37C0) as usize] = v,
            0x3C00..=0x3CFF => self.video.vscroll = v as u16 | ((a & 1) << 8),
            0x3D00..=0x3DFF => self.video.hscroll = v as u16 | ((a & 1) << 8),
            0x3E00..=0x3FFF => self.video.smoothing = v,
            _ => {}   // watchdog
        }
    }

    fn is_nmi_line_asserted(&mut self) -> bool { false }
    fn is_irq_line_asserted(&mut self) -> bool { self.irq }
    fn is_nmi_enabled(&mut self) -> bool { true }
    fn step_cycles(&mut self, _c: u64) {}
    fn begin_cpu_cycle(&mut self) {}
}

pub struct JediSystem {
    cpu: M6502Cpu,
    pub bus: MainBus,
    main_phase: u64,
    sound_phase: u64,
    current_frame: u64,
    save_filename: String,
    save_path: String,
    state_path: String,
}

impl JediSystem {
    pub fn new(set_name: &str, mut regions: HashMap<String, Vec<u8>>) -> Result<Self, String> {
        let mut rom = take_region(&mut regions, "maincpu")?;
        rom.resize(0x1C000, 0);
        let audio = take_region(&mut regions, "audiocpu")?;
        let video = JediVideo::new(
            take_region(&mut regions, "tx_gfx")?,
            take_region(&mut regions, "bg_gfx")?,
            take_region(&mut regions, "spr_gfx")?,
            take_region(&mut regions, "proms")?,
        )?;
        let mut bus = MainBus {
            ram: vec![0; 0x800], rom, page: 0, video, novram: Novram::new(),
            adc: 0, stick: [0x80; 2], in0: 0xFF, outlatch: 0, irq: false, vblank: false,
            sound: JediSound::new(audio),
        };
        let mut cpu = M6502Cpu::new(CpuVariant::NMOS6502);
        cpu.power_on(&mut bus);
        Ok(Self {
            cpu, bus, main_phase: 0, sound_phase: 0, current_frame: 0,
            save_filename: set_name.to_string(),
            save_path: "user://GD_EMU/Arcade/Atari/Save".to_string(),
            state_path: "user://GD_EMU/Arcade/Atari/State".to_string(),
        })
    }
}

/// Move an analog axis toward its target (a digital pad standing in for the stick).
fn ramp(cur: u8, target: u8) -> u8 {
    const STEP: i32 = 24;
    let (c, t) = (cur as i32, target as i32);
    (if c < t { (c + STEP).min(t) } else { (c - STEP).max(t) }) as u8
}

impl System for JediSystem {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        for line in 0..LINES {
            // 32V: both CPUs' IRQ
            if line >= 32 && line % 32 == 0 {
                let on = line & 32 == 0;
                self.bus.irq = on;
                self.bus.sound.bus.irq = on;
            }
            self.bus.vblank = line >= SCREEN_H as u16;
            self.main_phase += MAIN_CLOCK;
            while self.main_phase >= LINE_RATE {
                self.main_phase -= LINE_RATE;
                self.cpu.step_one_cycle(&mut self.bus);
                self.cpu.sample_interrupt_lines(&mut self.bus);
            }
            self.sound_phase += SOUND_CLOCK;
            while self.sound_phase >= LINE_RATE {
                self.sound_phase -= LINE_RATE;
                self.bus.sound.cycle();
            }
            if (line as usize) < SCREEN_H {
                self.bus.video.render_line(line as usize);
            }
        }
        self.current_frame += 1;
    }

    fn set_input(&mut self, m: u32) {
        let on = |b: u32| m & b != 0;
        // 0C00, active low: 01 right thumb, 02 fire, 04 left thumb, 10 test, 80 coin
        let mut in0 = 0xFF;
        if on(pad::A) { in0 &= !0x02; }
        if on(pad::B) { in0 &= !0x04; }
        if on(pad::X) { in0 &= !0x01; }
        if on(pad::L) { in0 &= !0x10; }
        if on(pad::COIN) || on(pad::SELECT) { in0 &= !0x80; }
        self.bus.in0 = in0;
        let ty = if on(pad::UP) { 0x00 } else if on(pad::DOWN) { 0xFF } else { 0x80 };
        let tx = if on(pad::LEFT) { 0x00 } else if on(pad::RIGHT) { 0xFF } else { 0x80 };
        let s = &mut self.bus.stick;
        *s = [ramp(s[0], ty), ramp(s[1], tx)];
    }

    fn drain_audio(&mut self) -> Vec<f32> { std::mem::take(&mut self.bus.sound.out) }
    fn sample_rate(&self) -> u32 { 44100 }
    fn power_on(&mut self) {}
    fn power_off(&mut self) {}
    fn has_battery(&self) -> bool { true }
    fn save_filename(&self) -> &str { &self.save_filename }
    fn save_battery_path(&self) -> &str { &self.save_path }
    fn save_state_path(&self) -> &str { &self.state_path }
    fn is_sram_dirty(&self) -> bool { self.bus.novram.dirty }
    fn clear_sram_dirty(&mut self) { self.bus.novram.dirty = false; }
    fn get_current_frame(&self) -> i64 { self.current_frame as i64 }
    fn get_sram(&self) -> Option<&[u8]> { Some(&self.bus.novram.eeprom) }
    fn load_sram(&mut self, data: &[u8]) {
        let n = data.len().min(0x100);
        let nv = &mut self.bus.novram;
        nv.eeprom[..n].copy_from_slice(&data[..n]);
        nv.sram.copy_from_slice(&nv.eeprom);         // the power-on recall
    }

    fn framebuffer(&self) -> &[u8] { &self.bus.video.framebuffer }

    fn reset(&mut self) {
        let b = &mut self.bus;
        b.page = 0;
        b.irq = false;
        for bit in 0..8 { b.outlatch_w(bit, false); }
        b.novram.recall(false);
        b.novram.recall(true);
        self.cpu = M6502Cpu::new(CpuVariant::NMOS6502);
        self.cpu.power_on(&mut self.bus);
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
        let main = MainState {
            ram: b.ram.clone(), page: b.page, video: b.video.clone(), novram: b.novram.clone(),
            adc: b.adc, outlatch: b.outlatch, irq: b.irq,
        };
        let s: (CpuState, MainState, JediSoundState, u64, u64, u64) =
            (self.cpu.get_state(), main, b.sound.save(), self.main_phase, self.sound_phase, self.current_frame);
        state::encode(&self.save_filename, STATE_VERSION, &s)
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let (cpu, main, sound, mp, sp, frame): (CpuState, MainState, JediSoundState, u64, u64, u64) =
            state::decode(&self.save_filename, STATE_VERSION, data)?;
        let b = &mut self.bus;
        if main.ram.len() != b.ram.len() { return Err("save state doesn't fit this board".into()); }
        b.ram = main.ram;
        b.page = main.page;
        let mut video = main.video;
        video.keep_roms(&mut b.video);
        b.video = video;
        b.novram = main.novram;
        b.novram.dirty = true;
        b.adc = main.adc;
        b.outlatch = main.outlatch;
        b.irq = main.irq;
        b.sound.load(sound);
        self.cpu.load_state(&cpu);
        self.main_phase = mp;
        self.sound_phase = sp;
        self.current_frame = frame;
        Ok(())
    }
}
