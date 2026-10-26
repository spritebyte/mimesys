//! Konami Green Beret / Rush'n Attack board (MAME gberet.cpp), also Mr. Goemon.
//!
//!   Z80 @ 18.432 MHz / 6 = 3.072 MHz; screen 6.144 MHz, 384 x 264 => 192 cycles a line
//!   0000-BFFF ROM   C000 colour RAM   C800 video RAM   D000/D100 sprite banks
//!   D200-DFFF RAM   E000-E03F row scroll   E040-E047 005849 control
//!   F000 W coin counters (Mr. Goemon: bits 5-7 = ROM bank at F800-FFFF)
//!   F200 R DSW2 / W sound latch    F400 R DSW3 / W latch -> SN76489A
//!   F600 R DSW1 / W watchdog       F601 P2   F602 P1   F603 SYSTEM
//! Interrupts (Konami 005849): NMI at lines 16, 48, ... 240 when enabled (ctrl 4 bit 0);
//! IRQ at line 240 (bit 1), FIRQ every other frame (bit 2), both on the Z80 INT line.
//! Clearing an enable bit drops its line.

use std::collections::HashMap;
use serde::{Deserialize, Serialize};

use crate::arcade::gberet::gberet_video::{GberetVideo, SCREEN_H, SCREEN_W};
use crate::arcade::hiscore::HiScoreMem;
use crate::arcade::{state, take_region};
use crate::common::audio::sn76489::{Sn76489, Sn76489Variant};
use crate::common::cpu::z80::{Bus, Z80Cpu, Z80State};
use crate::common::input as pad;
use crate::common::system::{DisplayInfo, Rotation, System};

const STATE_VERSION: u32 = 1;
const CYCLES_PER_LINE: i32 = 192;
const LINES_PER_FRAME: u16 = 264;
const SN_CYCLES_PER_LINE: u32 = 96;    // 1.536 MHz vs 3.072 MHz

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Variant { GreenBeret, MrGoemon }

#[derive(Serialize, Deserialize)]
pub struct GberetBus {
    #[serde(skip)] rom: Vec<u8>,
    ram: Vec<u8>,
    pub video: GberetVideo,
    pub sn: Sn76489,
    sound_latch: u8,
    bank: u8,
    goemon: bool,
    pub p1: u8, pub p2: u8, pub system: u8,
    pub dsw: [u8; 3],
    // interrupt lines (005849)
    nmi_line: bool,
    irq_line: bool,
    firq_line: bool,
}

impl GberetBus {
    fn clear_lines(&mut self, cleared: u8) {
        if cleared & 1 != 0 { self.nmi_line = false; }
        if cleared & 2 != 0 { self.irq_line = false; }
        if cleared & 4 != 0 { self.firq_line = false; }
    }
}

impl Bus for GberetBus {
    fn read(&mut self, a: u16) -> u8 {
        let v = &self.video;
        match a {
            0x0000..=0xBFFF => self.rom[a as usize],
            0xC000..=0xC7FF => v.colorram[(a & 0x7FF) as usize],
            0xC800..=0xCFFF => v.videoram[(a & 0x7FF) as usize],
            0xD000..=0xD0FF => v.spriteram2[(a & 0xFF) as usize],
            0xD100..=0xD1FF => v.spriteram[(a & 0xFF) as usize],
            0xD200..=0xDFFF => self.ram[(a - 0xD200) as usize],
            0xE000..=0xE03F => v.scrollram[(a & 0x3F) as usize],
            0xF200 => self.dsw[1],
            0xF400 => self.dsw[2],
            0xF600 => self.dsw[0],
            0xF601 => self.p2,
            0xF602 => self.p1,
            0xF603 => self.system,
            0xF800..=0xFFFF if self.goemon => self.rom[0xC000 + self.bank as usize * 0x800 + (a & 0x7FF) as usize],
            _ => 0xFF,
        }
    }

    fn write(&mut self, a: u16, d: u8) {
        let v = &mut self.video;
        match a {
            0xC000..=0xC7FF => v.colorram[(a & 0x7FF) as usize] = d,
            0xC800..=0xCFFF => v.videoram[(a & 0x7FF) as usize] = d,
            0xD000..=0xD0FF => v.spriteram2[(a & 0xFF) as usize] = d,
            0xD100..=0xD1FF => v.spriteram[(a & 0xFF) as usize] = d,
            0xD200..=0xDFFF => self.ram[(a - 0xD200) as usize] = d,
            0xE000..=0xE03F => v.scrollram[(a & 0x3F) as usize] = d,
            0xE040..=0xE047 => { let c = v.ctrl_w((a & 7) as usize, d); self.clear_lines(c); }
            0xF000 => if self.goemon { self.bank = (d >> 5) & 7; },
            0xF200 => self.sound_latch = d,
            0xF400 => { let l = self.sound_latch; self.sn.write(l); }
            _ => {}
        }
    }

    fn port_in(&mut self, _port: u16) -> u8 { 0xFF }
    fn port_out(&mut self, _port: u16, _val: u8) {}
}

pub struct GberetSystem {
    pub cpu: Z80Cpu,
    pub bus: GberetBus,
    line_cycles: i32,
    nmi_seen: bool,       // NMI is edge triggered: taken when the line goes high
    current_frame: u64,
    save_filename: String,
    save_path: String,
}

#[derive(Serialize, Deserialize)]
struct GberetState<B> {
    cpu: Z80State,
    bus: B,
    line_cycles: i32,
    nmi_seen: bool,
    frame: u64,
}

impl GberetSystem {
    pub fn new(variant: Variant, set_name: &str, mut regions: HashMap<String, Vec<u8>>) -> Result<Self, String> {
        let mut rom = take_region(&mut regions, "maincpu")?;
        rom.resize(0x10000, 0xFF);
        let tiles = take_region(&mut regions, "tiles")?;
        let sprites = take_region(&mut regions, "sprites")?;
        let proms = take_region(&mut regions, "proms")?;
        let goemon = variant == Variant::MrGoemon;
        let bus = GberetBus {
            rom,
            ram: vec![0; 0xE00],
            video: GberetVideo::new(&tiles, &sprites, &proms)?,
            sn: Sn76489::new(Sn76489Variant::Ti, 1_536_000, 44100),
            sound_latch: 0, bank: 0, goemon,
            p1: 0xFF, p2: 0xFF, system: 0xFF,
            // MAME defaults. DSW1: 1 coin 1 credit. DSW2: 3 lives, upright, bonus
            // 50K/100K (Mr. Goemon 20K/60K), normal, demo sounds on. DSW3: no flip, single controls.
            dsw: [0xFF, if goemon { 0x5A } else { 0x4A }, 0xFF],
            nmi_line: false, irq_line: false, firq_line: false,
        };
        Ok(Self {
            cpu: Z80Cpu::new(), bus, line_cycles: 0, nmi_seen: false, current_frame: 0,
            save_filename: set_name.to_string(),
            save_path: "user://GD_EMU/Arcade/Konami/State".to_string(),
        })
    }

    fn interrupts_at(&mut self, line: u16) {
        let ctrl = self.bus.video.ctrl[4];
        if ctrl & 1 != 0 && line & 0x1F == 0x10 { self.bus.nmi_line = true; }
        if line == 240 {
            if ctrl & 4 != 0 && self.current_frame & 1 != 0 { self.bus.firq_line = true; }
            if ctrl & 2 != 0 { self.bus.irq_line = true; }
        }
    }
}

impl System for GberetSystem {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        for line in 0..LINES_PER_FRAME {
            if line % 16 == 0 { self.interrupts_at(line); }
            if line == 240 { self.bus.video.render(); }
            while self.line_cycles < CYCLES_PER_LINE {
                if self.bus.nmi_line && !self.nmi_seen {
                    self.nmi_seen = true;
                    self.line_cycles += self.cpu.nmi(&mut self.bus) as i32;
                    continue;
                }
                if !self.bus.nmi_line { self.nmi_seen = false; }
                if self.bus.irq_line || self.bus.firq_line {
                    let c = self.cpu.irq(&mut self.bus, 0xFF);
                    if c > 0 { self.line_cycles += c as i32; continue; }
                }
                self.line_cycles += self.cpu.step(&mut self.bus) as i32;
            }
            self.line_cycles -= CYCLES_PER_LINE;
            self.bus.sn.run(SN_CYCLES_PER_LINE);
        }
        self.current_frame += 1;
    }

    /// Active low. P1/P2: 0x01 left, 0x02 right, 0x04 up, 0x08 down, 0x10 button 1
    /// (knife), 0x20 button 2 (special weapon). SYSTEM: 0x01 coin 1, 0x02 coin 2,
    /// 0x04 service, 0x08 start 1, 0x10 start 2.
    fn set_input(&mut self, mask: u32) {
        let on = |m: u32, b: u32| m & b != 0;
        let joy = |m: u32| -> u8 {
            let mut v = 0xFF;
            if on(m, pad::LEFT) { v &= !0x01; }
            if on(m, pad::RIGHT) { v &= !0x02; }
            if on(m, pad::UP) { v &= !0x04; }
            if on(m, pad::DOWN) { v &= !0x08; }
            if on(m, pad::A) { v &= !0x10; }
            if on(m, pad::B) { v &= !0x20; }
            v
        };
        self.bus.p1 = joy(mask);
        self.bus.p2 = joy(pad::player2(mask));
        let mut sys = 0xFF;
        if on(mask, pad::COIN) || on(mask, pad::SELECT) { sys &= !0x01; }
        if on(mask, pad::START) { sys &= !0x08; }
        if on(mask, pad::START2) { sys &= !0x10; }
        self.bus.system = sys;
    }

    fn drain_audio(&mut self) -> Vec<f32> { std::mem::take(&mut self.bus.sn.out) }
    fn sample_rate(&self) -> u32 { 44100 }
    fn power_on(&mut self) {}
    fn power_off(&mut self) {}
    fn has_battery(&self) -> bool { false }
    fn save_filename(&self) -> &str { &self.save_filename }
    fn save_battery_path(&self) -> &str { &self.save_path }
    fn save_state_path(&self) -> &str { &self.save_path }
    fn is_sram_dirty(&self) -> bool { false }
    fn clear_sram_dirty(&mut self) {}
    fn get_current_frame(&self) -> i64 { self.current_frame as i64 }
    fn get_sram(&self) -> Option<&[u8]> { None }
    fn load_sram(&mut self, _data: &[u8]) {}
    fn framebuffer(&self) -> &[u8] { &self.bus.video.framebuffer }

    fn reset(&mut self) {
        self.cpu = Z80Cpu::new();
        self.line_cycles = 0;
        for i in 0..8 { self.bus.video.ctrl_w(i, 0); }
        self.bus.nmi_line = false; self.bus.irq_line = false; self.bus.firq_line = false;
        self.nmi_seen = false;
        self.bus.bank = 0;
    }

    /// "dsw1" coinage, "dsw2" lives/bonus/difficulty/demo sounds, "dsw3" flip/controls
    /// (MAME's gberet input ports).
    fn set_option(&mut self, key: &str, value: i64) {
        match key {
            "dsw1" => self.bus.dsw[0] = value as u8,
            "dsw2" => self.bus.dsw[1] = value as u8,
            "dsw3" => self.bus.dsw[2] = value as u8,
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
            target_aspect_ratio: 4.0 / 3.0,
            rotation: Rotation::None,
        }
    }

    fn save_state(&self) -> Vec<u8> {
        state::encode(&self.save_filename, STATE_VERSION, &GberetState {
            cpu: self.cpu.save_state(), bus: &self.bus, line_cycles: self.line_cycles,
            nmi_seen: self.nmi_seen, frame: self.current_frame,
        })
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let s: GberetState<GberetBus> = state::decode(&self.save_filename, STATE_VERSION, data)?;
        let mut bus = s.bus;
        bus.rom = std::mem::take(&mut self.bus.rom);
        bus.video.keep_static(&mut self.bus.video);
        bus.sn.out = std::mem::take(&mut self.bus.sn.out);
        self.bus = bus;
        self.cpu.load_state(&s.cpu);
        self.line_cycles = s.line_cycles;
        self.nmi_seen = s.nmi_seen;
        self.current_frame = s.frame;
        Ok(())
    }
}

impl HiScoreMem for GberetSystem {
    fn hs_read(&mut self, addr: u16) -> u8 { self.bus.read(addr) }
    fn hs_write(&mut self, addr: u16, v: u8) { self.bus.write(addr, v) }
}
