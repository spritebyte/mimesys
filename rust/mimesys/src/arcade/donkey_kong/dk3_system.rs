//! Donkey Kong 3 (Nintendo TKG04 board). After MAME dkong.cpp.
//!
//! Same video as Donkey Kong (256x224, 264 lines at 16 kHz), with 512 tiles, 256
//! sprites and a 4-bit-per-gun palette. A 4 MHz Z80 runs the game and copies sprites
//! with a Z80 DMA chip; two RP2A03s (NES CPU + APU) make the music and effects.
//!
//! Main map: 0000-5FFF ROM, 6000-6FFF RAM, 7000-73FF sprites, 7400-77FF tiles,
//! 7C00/7C80/7D00 IN0/IN1/DSW0 (writes: sound latches 1-3), 7D80 DSW1 (write: sound
//! reset), 7E80-7E87 control latch, 8000-9FFF ROM, I/O port 0 the DMA chip.

use std::collections::HashMap;
use serde::{Deserialize, Serialize};

use crate::arcade::donkey_kong::dk3_dma::{DmaMemory, Z80Dma};
use crate::arcade::donkey_kong::dk3_sound::{Dk3Sound, Dk3SoundState};
use crate::arcade::donkey_kong::dk_video::{DKVideo, FIRST_VISIBLE_LINE, SCREEN_H, SCREEN_W, VBLANK_START};
use crate::arcade::hiscore::HiScoreMem;
use crate::arcade::{state, take_region};
use crate::common::cpu::z80::{Bus, Z80Cpu, Z80State};
use crate::common::input as pad;
use crate::common::system::{DisplayInfo, Rotation, System};

const STATE_VERSION: u32 = 1;
/// 8 MHz / 2
const CPU_CLOCK: i32 = 4_000_000;
/// 6.144 MHz pixel clock / 384
const LINE_RATE: i32 = 16_000;
const LINES_PER_FRAME: u16 = 264;

pub struct Dk3Bus {
    rom: Vec<u8>,
    ram: Vec<u8>,
    pub video: DKVideo,
    dma: Z80Dma,
    pub sound: Dk3Sound,
    latches: [u8; 3],
    pub in0: u8,
    pub in1: u8,
    pub dsw0: u8,
    pub dsw1: u8,
    pub nmi_mask: bool,
}

#[derive(Serialize, Deserialize)]
pub struct Dk3BusState {
    ram: Vec<u8>,
    video_ram: Vec<u8>,
    sprite_ram: Vec<u8>,
    flip: bool,
    sprite_bank: u8,
    palette_bank: u8,
    gfx_bank: u8,
    dma: Z80Dma,
    sound: Dk3SoundState,
    latches: [u8; 3],
    nmi_mask: bool,
}

impl Dk3Bus {
    fn save(&self) -> Dk3BusState {
        let v = &self.video;
        Dk3BusState {
            ram: self.ram.clone(), video_ram: v.video_ram.clone(), sprite_ram: v.sprite_ram.clone(),
            flip: v.flip, sprite_bank: v.sprite_bank, palette_bank: v.palette_bank, gfx_bank: v.gfx_bank,
            dma: self.dma.clone(), sound: self.sound.save(), latches: self.latches, nmi_mask: self.nmi_mask,
        }
    }

    fn load(&mut self, s: Dk3BusState) -> Result<(), String> {
        let v = &mut self.video;
        if s.ram.len() != self.ram.len() || s.video_ram.len() != v.video_ram.len() || s.sprite_ram.len() != v.sprite_ram.len() {
            return Err("save state doesn't fit this board".into());
        }
        self.sound.load(s.sound)?;
        self.ram = s.ram;
        v.video_ram = s.video_ram;
        v.sprite_ram = s.sprite_ram;
        v.flip = s.flip;
        v.sprite_bank = s.sprite_bank;
        v.palette_bank = s.palette_bank;
        v.gfx_bank = s.gfx_bank;
        self.dma = s.dma;
        self.latches = s.latches;
        self.sound.set_latches(self.latches);
        self.nmi_mask = s.nmi_mask;
        Ok(())
    }

    fn latch(&mut self, n: usize, v: u8) {
        self.latches[n] = v;
        self.sound.set_latches(self.latches);
    }
}

impl DmaMemory for Dk3Bus {
    fn dma_read(&mut self, addr: u16) -> u8 { self.read(addr) }
    fn dma_write(&mut self, addr: u16, v: u8) { self.write(addr, v) }
}

impl Bus for Dk3Bus {
    fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x5FFF | 0x8000..=0x9FFF => self.rom.get(addr as usize).copied().unwrap_or(0),
            0x6000..=0x6FFF => self.ram[(addr - 0x6000) as usize],
            0x7000..=0x73FF => self.video.sprite_ram[(addr - 0x7000) as usize],
            0x7400..=0x77FF => self.video.video_ram[(addr - 0x7400) as usize],
            0x7C00 => self.in0,
            0x7C80 => self.in1,
            0x7D00 => self.dsw0,
            0x7D80 => self.dsw1,
            _ => 0,
        }
    }

    fn write(&mut self, addr: u16, val: u8) {
        match addr {
            0x6000..=0x6FFF => self.ram[(addr - 0x6000) as usize] = val,
            0x7000..=0x73FF => self.video.sprite_ram[(addr - 0x7000) as usize] = val,
            0x7400..=0x77FF => self.video.video_ram[(addr - 0x7400) as usize] = val,
            0x7C00 => self.latch(0, val),
            0x7C80 => self.latch(1, val),
            0x7D00 => self.latch(2, val),
            0x7D80 => self.sound.set_reset(val),
            0x7E81 => self.video.gfx_bank = !val & 1,
            0x7E82 => self.video.flip = val & 1 != 0,
            0x7E83 => self.video.sprite_bank = val & 1,
            0x7E84 => self.nmi_mask = val & 1 != 0,
            0x7E85 => {
                let mut dma = std::mem::take(&mut self.dma);
                dma.set_rdy(val & 1 != 0, self);
                self.dma = dma;
            }
            0x7E86 | 0x7E87 => {
                let bit = 1 << (addr & 1);
                if val & 1 != 0 { self.video.palette_bank |= bit } else { self.video.palette_bank &= !bit }
            }
            _ => {}      // 7E80 coin counter
        }
    }

    fn port_in(&mut self, _port: u16) -> u8 { 0 }

    fn port_out(&mut self, port: u16, val: u8) {
        if port & 0xFF == 0 {
            let mut dma = std::mem::take(&mut self.dma);
            dma.write(val, self);
            self.dma = dma;
        }
    }
}

pub struct Dk3System {
    pub cpu: Z80Cpu,
    pub bus: Dk3Bus,
    /// Z80 cycles run past the current line, in 1/LINE_RATE units
    phase: i32,
    current_frame: u64,
    save_filename: String,
    save_path: String,
    sound_roms: (Vec<u8>, Vec<u8>),
}

/// The 8 KB sound program from its 64 KB region (loaded at 0x8000), or as given.
fn sound_program(r: Vec<u8>) -> Vec<u8> {
    if r.len() >= 0xA000 { r[0x8000..0xA000].to_vec() } else { r }
}

impl Dk3System {
    pub fn new(set_name: String, mut regions: HashMap<String, Vec<u8>>) -> Result<Self, String> {
        let rom = take_region(&mut regions, "maincpu")?;
        let gfx1 = take_region(&mut regions, "gfx1")?;
        let gfx2 = take_region(&mut regions, "gfx2")?;
        let proms = take_region(&mut regions, "proms")?;
        let snd_a = sound_program(take_region(&mut regions, "rp2a03a")?);
        let snd_b = sound_program(take_region(&mut regions, "rp2a03b")?);
        let video = DKVideo::new_dk3(&gfx1, &gfx2, &proms)?;
        Ok(Self {
            cpu: Z80Cpu::new(),
            bus: Dk3Bus {
                rom,
                ram: vec![0; 0x1000],
                video,
                dma: Z80Dma::new(),
                sound: Dk3Sound::new(snd_a.clone(), snd_b.clone(), LINE_RATE as u64),
                latches: [0; 3],
                in0: 0, in1: 0, dsw0: 0, dsw1: 0,
                nmi_mask: false,
            },
            phase: 0,
            current_frame: 0,
            save_filename: set_name,
            save_path: "user://Arcade/DK/State".to_string(),
            sound_roms: (snd_a, snd_b),
        })
    }
}

impl System for Dk3System {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        for line in 0..LINES_PER_FRAME {
            if line == VBLANK_START && self.bus.nmi_mask {
                self.phase += self.cpu.nmi(&mut self.bus) as i32 * LINE_RATE;
            }
            while self.phase < CPU_CLOCK {
                self.phase += self.cpu.step(&mut self.bus) as i32 * LINE_RATE;
            }
            self.phase -= CPU_CLOCK;

            self.bus.sound.run_line(line >= VBLANK_START);

            if (FIRST_VISIBLE_LINE..VBLANK_START).contains(&line) {
                self.bus.video.render_line(line);
            }
        }
        self.bus.sound.end_frame();
        self.current_frame += 1;
    }

    fn set_input(&mut self, mask: u32) {
        let stick = |m: u32| {
            let on = |b: u32| m & b != 0;
            let mut v = 0;
            if on(pad::RIGHT) { v |= 0x01; }
            if on(pad::LEFT)  { v |= 0x02; }
            if on(pad::UP)    { v |= 0x04; }
            if on(pad::DOWN)  { v |= 0x08; }
            if on(pad::A) || on(pad::B) { v |= 0x10; }     // spray on either button
            v
        };
        let on = |b: u32| mask & b != 0;
        let mut in0 = stick(mask);
        if on(pad::START)  { in0 |= 0x20; }
        if on(pad::START2) { in0 |= 0x40; }
        let mut in1 = stick(pad::player2(mask));
        if on(pad::COIN) || on(pad::SELECT) { in1 |= 0x20; }  // Select works as coin too
        self.bus.in0 = in0;
        self.bus.in1 = in1;
    }

    fn set_option(&mut self, key: &str, value: i64) {
        match key {
            "dsw0" => self.bus.dsw0 = value as u8,
            "dsw1" => self.bus.dsw1 = value as u8,
            _ => {}
        }
    }

    fn drain_audio(&mut self) -> Vec<f32> { std::mem::take(&mut self.bus.sound.out) }
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
        self.phase = 0;
        self.bus.nmi_mask = false;
        self.bus.dma = Z80Dma::new();
        let (a, b) = self.sound_roms.clone();
        self.bus.sound = Dk3Sound::new(a, b, LINE_RATE as u64);
        self.bus.sound.set_latches(self.bus.latches);
    }

    fn display_info(&self) -> DisplayInfo {
        DisplayInfo {
            buffer_width: SCREEN_W as i32,
            buffer_height: SCREEN_H as i32,
            visible_x: 0,
            visible_y: 0,
            visible_width: SCREEN_W as i32,
            visible_height: SCREEN_H as i32,
            target_aspect_ratio: 3.0 / 4.0,
            rotation: Rotation::CCW90,
        }
    }

    fn save_state(&self) -> Vec<u8> {
        let s = (self.cpu.save_state(), self.bus.save(), self.phase, self.current_frame);
        state::encode(&self.save_filename, STATE_VERSION, &s)
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let (cpu, bus, phase, frame): (Z80State, Dk3BusState, i32, u64) =
            state::decode(&self.save_filename, STATE_VERSION, data)?;
        self.bus.load(bus)?;
        self.cpu.load_state(&cpu);
        self.phase = phase;
        self.current_frame = frame;
        Ok(())
    }
}

impl HiScoreMem for Dk3System {
    fn hs_read(&mut self, addr: u16) -> u8 { self.bus.read(addr) }
    fn hs_write(&mut self, addr: u16, v: u8) { self.bus.write(addr, v) }
}
