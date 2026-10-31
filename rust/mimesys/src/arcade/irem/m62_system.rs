// Irem M62 board, Kung-Fu Master configuration (MAME src/mame/irem/m62.cpp: kungfum).
//
//   Z80 @ 18.432 MHz / 6 = 3.072 MHz, IRQ0 (held until taken) at vblank
//   0000-7FFF ROM        A000 W scroll low    B000 W scroll high
//   C000-C0FF sprite RAM D000-DFFF tile RAM   E000-EFFF RAM
//   I/O 00: R SYSTEM, W sound command    01: R P1, W flip screen / coin counters
//       02: R P2    03: R DSW1    04: R DSW2
// Video timing: 6.144 MHz pixel clock, 384 x 284 => 192 CPU cycles a line, 56.34 Hz.
// Sound: the Irem M62 board (m62_sound.rs).
//
// Regions: maincpu, audiocpu (64 KB, ROM in 0x4000-0xFFFF), gfx1 (tiles), gfx2 (sprites),
// spr_height_prom, spr_color_proms, chr_color_proms.

use crate::common::cpu::z80::{Bus, Z80Cpu};
use crate::common::input as pad;
use crate::common::system::{System, DisplayInfo, Rotation};
use crate::arcade::take_region;
use crate::arcade::irem::m62_video::{M62Video, SCREEN_W, SCREEN_H};
use crate::arcade::irem::m62_sound::{M62Sound, SOUND_CPU_CLOCK};
use crate::arcade::hiscore::HiScoreMem;
use crate::arcade::state;
use crate::common::cpu::z80::Z80State;
use serde::{Serialize, Deserialize};
use std::collections::HashMap;

const STATE_VERSION: u32 = 2;   // 2: 6801 core gained HD63701 fields

#[derive(Serialize, Deserialize)]
struct M62State {
    cpu: Z80State,
    ram: Vec<u8>,
    tileram: Vec<u8>,
    spriteram: Vec<u8>,
    hscroll: u16,
    flip: bool,
    sound: M62Sound,
    line_cycles: i32,
    irq_pending: bool,
    sound_cycle_debt: i64,
    frame: u64,
}

const CYCLES_PER_LINE: i32 = 192;
const LINES_PER_FRAME: usize = 284;
const VBLANK_START: usize = 256;
const CPU_CLOCK: f64 = 3_072_000.0;
const SAMPLE_RATE: u32 = 44100;
const SOUND_CYCLES_PER_LINE_X1M: i64 = (SOUND_CPU_CLOCK * CYCLES_PER_LINE as f64 / CPU_CLOCK * 1e6) as i64;

pub struct M62Bus {
    rom: Vec<u8>,
    ram: [u8; 0x1000],
    pub video: M62Video,
    pub sound: M62Sound,
    pub system: u8,
    pub p1: u8,
    pub p2: u8,
    pub dsw1: u8,
    pub dsw2: u8,
}

impl Bus for M62Bus {
    fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x7FFF => self.rom[addr as usize],
            0xD000..=0xDFFF => self.video.tileram[(addr & 0xFFF) as usize],
            0xE000..=0xEFFF => self.ram[(addr & 0xFFF) as usize],
            _ => 0xFF,
        }
    }

    fn write(&mut self, addr: u16, v: u8) {
        match addr {
            0xA000 => self.video.hscroll = (self.video.hscroll & 0xFF00) | v as u16,
            0xB000 => self.video.hscroll = (self.video.hscroll & 0x00FF) | ((v as u16) << 8),
            0xC000..=0xC0FF => self.video.spriteram[(addr & 0xFF) as usize] = v,
            0xD000..=0xDFFF => self.video.tileram[(addr & 0xFFF) as usize] = v,
            0xE000..=0xEFFF => self.ram[(addr & 0xFFF) as usize] = v,
            _ => {}
        }
    }

    fn port_in(&mut self, port: u16) -> u8 {
        match port & 0xFF {
            0 => self.system,
            1 => self.p1,
            2 => self.p2,
            3 => self.dsw1,
            4 => self.dsw2,
            _ => 0xFF,
        }
    }

    fn port_out(&mut self, port: u16, v: u8) {
        match port & 0xFF {
            0 => self.sound.command(v),
            1 => {
                // DSW2 bit 0 (flip screen switch, active low) inverts the software flip
                let d = v ^ (!self.dsw2 & 1);
                self.video.flip = d & 1 != 0;
            }
            _ => {}
        }
    }
}

pub struct M62System {
    pub cpu: Z80Cpu,
    pub bus: M62Bus,
    line_cycles: i32,
    irq_pending: bool,
    sound_cycle_debt: i64,
    current_frame: u64,
    save_filename: String,
    save_path: String,
}

impl M62System {
    pub fn new(set_name: &str, mut regions: HashMap<String, Vec<u8>>) -> Result<Self, String> {
        let mut rom = take_region(&mut regions, "maincpu")?;
        rom.resize(0x8000, 0xFF);
        let audio = take_region(&mut regions, "audiocpu")?;
        let gfx1 = take_region(&mut regions, "gfx1")?;
        let gfx2 = take_region(&mut regions, "gfx2")?;
        let height = take_region(&mut regions, "spr_height_prom")?;
        let spr = take_region(&mut regions, "spr_color_proms")?;
        let chr = take_region(&mut regions, "chr_color_proms")?;

        let video = M62Video::new(&gfx1, &gfx2, height, &chr, &spr);
        let sound = M62Sound::new(audio, SAMPLE_RATE);
        let bus = M62Bus {
            rom, ram: [0; 0x1000], video, sound,
            system: 0xFF, p1: 0xFF, p2: 0xFF,
            // DSW1: easy, slow energy loss, 3 lives, 1 coin 1 credit
            dsw1: 0xFF,
            // DSW2: flip off, upright, coin mode 1, no cheats, service off
            dsw2: 0xFD,
        };
        Ok(Self {
            cpu: Z80Cpu::new(),
            bus,
            line_cycles: 0,
            irq_pending: false,
            sound_cycle_debt: 0,
            current_frame: 0,
            save_filename: set_name.to_string(),
            save_path: "user://Arcade/Irem/State".to_string(),
        })
    }
}

impl System for M62System {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        for line in 0..LINES_PER_FRAME {
            if line == VBLANK_START {
                self.bus.video.render_frame();
                self.irq_pending = true;
            }
            while self.line_cycles < CYCLES_PER_LINE {
                if self.irq_pending {
                    let c = self.cpu.irq(&mut self.bus, 0xFF);
                    if c > 0 {
                        self.irq_pending = false;
                        self.line_cycles += c as i32;
                        continue;
                    }
                }
                self.line_cycles += self.cpu.step(&mut self.bus) as i32;
            }
            self.line_cycles -= CYCLES_PER_LINE;

            self.sound_cycle_debt += SOUND_CYCLES_PER_LINE_X1M;
            let n = self.sound_cycle_debt / 1_000_000;
            self.sound_cycle_debt %= 1_000_000;
            self.bus.sound.run(n);
        }
        self.current_frame += 1;
    }

    /// Active low. SYSTEM: 0x01 start 1, 0x02 start 2, 0x08 coin 1.
    /// P1/P2: 0x01 right, 0x02 left, 0x04 down, 0x08 up, 0x20 button 2 (punch),
    /// 0x80 button 1 (kick); P2 0x10 = coin 2.
    fn set_input(&mut self, mask: u32) {
        let on = |m: u32, b: u32| m & b != 0;
        let mut sys = 0xFF;
        if on(mask, pad::START) { sys &= !0x01; }
        if on(mask, pad::START2) { sys &= !0x02; }
        if on(mask, pad::COIN) || on(mask, pad::SELECT) { sys &= !0x08; }
        self.bus.system = sys;
        let joy = |m: u32| -> u8 {
            let mut v = 0xFF;
            if on(m, pad::RIGHT) { v &= !0x01; }
            if on(m, pad::LEFT) { v &= !0x02; }
            if on(m, pad::DOWN) { v &= !0x04; }
            if on(m, pad::UP) { v &= !0x08; }
            if on(m, pad::B) { v &= !0x20; }
            if on(m, pad::A) { v &= !0x80; }
            v
        };
        self.bus.p1 = joy(mask);
        self.bus.p2 = joy(pad::player2(mask));
    }

    fn drain_audio(&mut self) -> Vec<f32> { self.bus.sound.drain() }
    fn sample_rate(&self) -> u32 { SAMPLE_RATE }
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
        self.irq_pending = false;
        self.bus.video.flip = false;
        self.bus.video.hscroll = 0;
        self.bus.sound.reset();
    }

    /// "dsw1" / "dsw2": the DIP switch bytes (see MAME's kungfum input ports).
    fn set_option(&mut self, key: &str, value: i64) {
        match key {
            "dsw1" => self.bus.dsw1 = value as u8,
            "dsw2" => self.bus.dsw2 = value as u8,
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
        #[derive(Serialize)]
        struct Ref<'a> {
            cpu: Z80State, ram: &'a [u8], tileram: &'a [u8], spriteram: &'a [u8],
            hscroll: u16, flip: bool, sound: &'a M62Sound,
            line_cycles: i32, irq_pending: bool, sound_cycle_debt: i64, frame: u64,
        }
        let v = &self.bus.video;
        state::encode(&self.save_filename, STATE_VERSION, &Ref {
            cpu: self.cpu.save_state(), ram: &self.bus.ram, tileram: &v.tileram, spriteram: &v.spriteram,
            hscroll: v.hscroll, flip: v.flip, sound: &self.bus.sound,
            line_cycles: self.line_cycles, irq_pending: self.irq_pending,
            sound_cycle_debt: self.sound_cycle_debt, frame: self.current_frame,
        })
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let mut s: M62State = state::decode(&self.save_filename, STATE_VERSION, data)?;
        if s.ram.len() != 0x1000 || s.tileram.len() != 0x1000 || s.spriteram.len() != 0x100 {
            return Err("save state has the wrong memory sizes".into());
        }
        self.cpu.load_state(&s.cpu);
        self.bus.ram.copy_from_slice(&s.ram);
        let v = &mut self.bus.video;
        v.tileram.copy_from_slice(&s.tileram);
        v.spriteram.copy_from_slice(&s.spriteram);
        v.hscroll = s.hscroll;
        v.flip = s.flip;
        s.sound.keep_static(&mut self.bus.sound);
        self.bus.sound = s.sound;
        self.line_cycles = s.line_cycles;
        self.irq_pending = s.irq_pending;
        self.sound_cycle_debt = s.sound_cycle_debt;
        self.current_frame = s.frame;
        Ok(())
    }
}

impl HiScoreMem for M62System {
    fn hs_read(&mut self, addr: u16) -> u8 { self.bus.read(addr) }
    fn hs_write(&mut self, addr: u16, v: u8) { self.bus.write(addr, v) }
}

#[cfg(test)]
impl M62Bus {
    pub(crate) fn read_ram(&self, a: usize) -> u8 { self.ram[a] }
}
