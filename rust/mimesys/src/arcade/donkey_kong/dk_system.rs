use crate::common::cpu::z80::Z80Cpu;
use crate::arcade::donkey_kong::dk_bus::DKBus;
use crate::arcade::donkey_kong::dk_sound::{SoundCpu, Mcs48, DacResampler, DkSfx, run_sound_line};
use crate::arcade::donkey_kong::dk_video::{DKVideo, SCREEN_W, SCREEN_H, FIRST_VISIBLE_LINE, VBLANK_START};
use crate::common::system::{System,DisplayInfo,Rotation};
use crate::common::snapshot::Snapshot;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::collections::HashMap;
use std::io::Cursor;
use crate::arcade::take_region;
use crate::common::input as pad;

const CYCLES_PER_LINE: i32 = 192;
const LINES_PER_FRAME: u16 = 264;

pub struct DKSystem {
    pub cpu: Z80Cpu,
    pub bus: DKBus,
    line_cycles: i32,
    current_frame: u64,
    save_filename: String,
    save_path: String,
    sound_cpu: Option<Box<dyn SoundCpu>>, // None until 8035 core exists
    resampler: DacResampler,
    sfx: DkSfx,
    sound_budget: i32,
}

impl DKSystem {
    pub fn new(set_name: String, mut regions: HashMap<String, Vec<u8>>) -> Result<Self, String> {
        let rom   = take_region(&mut regions, "maincpu")?;
        let gfx1  = take_region(&mut regions, "gfx1")?;
        let gfx2  = take_region(&mut regions, "gfx2")?;
        let proms = take_region(&mut regions, "proms")?;
        let sound_rom = regions.remove("soundcpu").unwrap_or_default();
        let has_sound = !sound_rom.is_empty();

        let video = DKVideo::new(&gfx1, &gfx2, &proms)?;
        let mut dk = Self {
            cpu: Z80Cpu::new(),
            bus: DKBus::new(rom, video, sound_rom),
            line_cycles: 0,
            current_frame: 0,
            save_filename: set_name.to_string(),
            save_path: "user://Arcade/DK/State".to_string(),
            sound_cpu: None,
            resampler: DacResampler::new(44100),
            sfx: DkSfx::new(44100),
            sound_budget: 0,
        };
        if has_sound {
            dk.attach_sound_cpu(Box::new(Mcs48::new()));
        }
        Ok(dk)
    }

    pub fn attach_sound_cpu(&mut self, cpu: Box<dyn SoundCpu>) {
        self.sound_cpu = Some(cpu);
        self.bus.sound.cpu_attached = true;
    }
}

impl System for DKSystem {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        for line in 0..LINES_PER_FRAME {
            if line == VBLANK_START && self.bus.nmi_mask {
                self.line_cycles += self.cpu.nmi(&mut self.bus) as i32;
            }
            while self.line_cycles < CYCLES_PER_LINE {
                self.line_cycles += self.cpu.step(&mut self.bus) as i32;
            }
            self.line_cycles -= CYCLES_PER_LINE;

            if let Some(cpu) = self.sound_cpu.as_mut() {
                run_sound_line(cpu.as_mut(), &mut self.bus.sound, &mut self.resampler, &mut self.sfx, &mut self.sound_budget);
            }

            if (FIRST_VISIBLE_LINE..VBLANK_START).contains(&line) {
                self.bus.video.render_line(line);
            }
        }
        self.current_frame += 1;
    }

    fn set_input(&mut self, mask: u32) {
        let on = |b: u32| mask & b != 0;
        let mut in0 = 0;
        if on(pad::RIGHT) { in0 |= 0x01; }
        if on(pad::LEFT)  { in0 |= 0x02; }
        if on(pad::UP)    { in0 |= 0x04; }
        if on(pad::DOWN)  { in0 |= 0x08; }
        if on(pad::A) || on(pad::B) { in0 |= 0x10; }        // jump on either button
        self.bus.in0 = in0;

        let mut in2 = 0;
        if on(pad::COIN) || on(pad::SELECT) { in2 |= 0x80; } // Select works as coin too
        if on(pad::START)  { in2 |= 0x04; }
        if on(pad::START2) { in2 |= 0x08; }
        self.bus.in2 = in2;
    }

    fn drain_audio(&mut self) -> Vec<f32> { std::mem::take(&mut self.resampler.out) } // silent for now
    fn sample_rate(&self) -> u32 { 44100 }
    fn power_on(&mut self) { }
    fn power_off(&mut self) { }
    fn has_battery(&self) -> bool { false }
    fn save_filename(&self) -> &str { &self.save_filename }
    fn save_battery_path(&self) -> &str { &self.save_path }
    fn is_sram_dirty(&self) -> bool { false }
    fn clear_sram_dirty(&mut self) { }
    fn get_current_frame(&self) -> i64 { self.current_frame as i64 }
    fn get_sram(&self) -> Option<&[u8]> { None }
    fn load_sram(&mut self, data: &[u8]) { }

    fn framebuffer(&self) -> &[u8] { &self.bus.video.framebuffer }

    fn reset(&mut self) {
        self.cpu = Z80Cpu::new();
        self.line_cycles = 0;
        if self.sound_cpu.is_some() {
            self.attach_sound_cpu(Box::new(Mcs48::new()))
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
            target_aspect_ratio: 3.0/4.0,
            rotation: Rotation::CCW90,
        }
    }

    fn save_state(&self) -> Vec<u8> {
        todo!("hook up DK Arcade save state")
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let _ = data;
        todo!("hook up DK Arcade load state")
    }
}
