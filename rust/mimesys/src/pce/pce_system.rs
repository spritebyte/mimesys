// NEC PC Engine / TurboGrafx-16 (1987), HuCard games.
// HuC6280 CPU at 7.16 MHz, HuC6270 VDC (backgrounds, sprites), HuC6260 VCE (palette),
// 6-channel wavetable sound, 8 KB RAM.
//
// Timing (NTSC): 262 lines x 455 CPU cycles (7.16 MHz) = 60 frames/s. Each line: the VDC
// draws it (if it's in the active area) and checks the raster compare, then the CPU runs.
// The VDC's interrupts (raster, vblank) go to the CPU's IRQ1, the on-chip timer to TIMER.
//
// Output: the VDC's line width varies by game (256, 336, 352, 512...), so every line is
// stretched to a fixed 512 x 240 picture. Vertically the picture is a fixed window of the
// frame (like a TV): lines FIRST_VISIBLE..FIRST_VISIBLE+240 counted from vsync. Games that
// program a taller or offset display get the same placement and cropping as on hardware.
//
// Controls (unified input mask, see common/input.rs):
//   D-pad  D-pad     A  button I     B  button II     SELECT  SELECT     START  RUN

use serde::{Deserialize, Serialize};
use crate::common::system::{DisplayInfo, Rotation, System};
use crate::common::input as pad;
use crate::pce::huc6280::Huc6280;
use crate::pce::pce_bus::{HuCard, PceBusImpl, PceIo};
use crate::pce::psg::Psg;
use crate::pce::vce::Vce;
use crate::pce::vdc::{Vdc, MAX_WIDTH};

const CYCLES_PER_LINE: i32 = 455;
const LINES_PER_FRAME: usize = 262;
pub const OUT_W: usize = 512;
pub const OUT_H: usize = 240;
const SAMPLE_RATE: u32 = 44_100;
/// First frame line shown on screen. The standard setup (VSW 2, VDS 15) starts the
/// display here, so most games fill the 240 lines exactly.
const FIRST_VISIBLE: usize = 20;
/// Print the VDC's screen setup whenever a game changes it (for troubleshooting).
const LOG_VIDEO_MODE: bool = false;
const STATE_VERSION: u32 = 1;

pub struct PceSystem {
    cpu: Huc6280,
    pub bus: PceBusImpl,
    line_cycles: i32,
    current_frame: u64,
    framebuffer: Vec<u8>,
    line_buf: [u16; MAX_WIDTH],
    last_mode: (usize, usize, usize),
    save_filename: String,
    save_battery_path: String,
    save_state_path: String,
}

#[derive(Serialize, Deserialize)]
struct PceState {
    version: u32,
    rom_len: usize,
    cpu: Huc6280,
    vdc: Vdc,
    vce: Vce,
    psg: Psg,
    io: PceIo,
    card: HuCard,
    line_cycles: i32,
    frame: u64,
}

impl PceSystem {
    pub fn from_rom(rom: &[u8], filename: &str, _bios: &[u8]) -> Result<Self, String> {
        let (card, us_card) = HuCard::new(rom.to_vec())?;
        println!("PC Engine: {} KB HuCard{}", card.len() / 1024, if us_card { " (US, bit-reversed)" } else { "" });
        let mut sys = Self {
            cpu: Huc6280::new(),
            bus: PceBusImpl::new(card, us_card, SAMPLE_RATE),
            line_cycles: 0,
            current_frame: 0,
            framebuffer: vec![0; OUT_W * OUT_H * 4],
            line_buf: [0; MAX_WIDTH],
            last_mode: (0, 0, 0),
            save_filename: filename.to_string(),
            save_battery_path: "user://PCE/Save".to_string(),
            save_state_path: "user://PCE/State".to_string(),
        };
        sys.reset();
        Ok(sys)
    }

    /// Copy one VDC line (color indices) into the output, stretched to OUT_W.
    fn output_line(&mut self, row: usize) {
        let width = self.bus.vdc.display_width().clamp(1, MAX_WIDTH);
        let o = row * OUT_W * 4;
        let dst = &mut self.framebuffer[o..o + OUT_W * 4];
        let src = &self.line_buf[..width];
        let vce = &self.bus.vce;
        // step through the source line in 16.16 fixed point
        let step = ((width as u32) << 16) / OUT_W as u32;
        let mut pos = 0u32;
        for px in dst.chunks_exact_mut(4) {
            px.copy_from_slice(&vce.rgba(src[(pos >> 16) as usize]));
            pos += step;
        }
    }

    #[cfg(test)]
    pub(crate) fn output_all_lines_for_bench(&mut self) {
        self.fill_overscan();
        for row in 0..OUT_H { self.output_line(row); }
    }
    #[cfg(test)]
    pub(crate) fn cpu_only_for_bench(&mut self) {
        for _ in 0..LINES_PER_FRAME {
            while self.line_cycles < CYCLES_PER_LINE {
                let c = self.cpu.step(&mut self.bus);
                self.bus.tick(c);
                self.line_cycles += c as i32;
            }
            self.line_cycles -= CYCLES_PER_LINE;
        }
    }

    fn fill_overscan(&mut self) {
        let c = self.bus.vce.rgba(0x100);
        for px in self.framebuffer.chunks_exact_mut(4) { px.copy_from_slice(&c); }
    }
}

impl System for PceSystem {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        self.fill_overscan();
        let start = self.bus.vdc.active_start().min(LINES_PER_FRAME - 1);
        let height = self.bus.vdc.display_height();
        let end = (start + height).min(LINES_PER_FRAME - 1);
        let mode = (self.bus.vdc.display_width(), start, height);
        if LOG_VIDEO_MODE && mode != self.last_mode {
            println!("PCE video: {} wide, display lines {}..{} ({} tall), screen shows {}..{}",
                mode.0, start, start + height, height, FIRST_VISIBLE, FIRST_VISIBLE + OUT_H);
            self.last_mode = mode;
        }

        for line in 0..LINES_PER_FRAME {
            // raster counter: 64 at the first display line, counting until the next frame's
            if line >= start { self.bus.vdc.check_raster(line - start); }
            if (start..end).contains(&line) {
                let n = line - start;
                self.bus.vdc.render_line(n, &mut self.line_buf);
                if let Some(row) = line.checked_sub(FIRST_VISIBLE).filter(|&r| r < OUT_H) {
                    self.output_line(row);
                }
            } else if line == end {
                self.bus.vdc.start_vblank();
            }
            while self.line_cycles < CYCLES_PER_LINE {
                let c = self.cpu.step(&mut self.bus);
                self.bus.tick(c);
                self.line_cycles += c as i32;
            }
            self.line_cycles -= CYCLES_PER_LINE;
        }
        self.current_frame += 1;
    }

    fn set_input(&mut self, mask: u32) {
        let on = |b: u32| mask & b != 0;
        let mut p = 0u8;
        if on(pad::UP) { p |= 0x01; }
        if on(pad::RIGHT) { p |= 0x02; }
        if on(pad::DOWN) { p |= 0x04; }
        if on(pad::LEFT) { p |= 0x08; }
        if on(pad::A) { p |= 0x10; }       // I
        if on(pad::B) { p |= 0x20; }       // II
        if on(pad::SELECT) { p |= 0x40; }
        if on(pad::START) { p |= 0x80; }   // RUN
        self.bus.pad = p;
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
    fn framebuffer(&self) -> &[u8] { &self.framebuffer }

    fn reset(&mut self) {
        self.cpu = Huc6280::new();
        self.cpu.reset(&mut self.bus);
        self.line_cycles = 0;
    }

    fn display_info(&self) -> DisplayInfo {
        DisplayInfo {
            buffer_width: OUT_W as i32,
            buffer_height: OUT_H as i32,
            visible_x: 0,
            visible_y: 0,
            visible_width: OUT_W as i32,
            visible_height: OUT_H as i32,
            target_aspect_ratio: 4.0 / 3.0,
            rotation: Rotation::None,
        }
    }

    fn save_state(&self) -> Vec<u8> {
        let state = PceState {
            version: STATE_VERSION,
            rom_len: self.bus.card.len(),
            cpu: self.cpu.clone(),
            vdc: self.bus.vdc.clone(),
            vce: self.bus.vce.clone(),
            psg: self.bus.psg.clone(),
            io: self.bus.io.clone(),
            card: self.bus.card.clone(),
            line_cycles: self.line_cycles,
            frame: self.current_frame,
        };
        let config = bincode::config::standard().with_fixed_int_encoding();
        bincode::serde::encode_to_vec(&state, config).unwrap_or_default()
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let config = bincode::config::standard().with_fixed_int_encoding();
        let (s, _): (PceState, usize) = bincode::serde::decode_from_slice(data, config)
            .map_err(|e| format!("Failed to decode PC Engine save state: {e:?}"))?;
        if s.version != STATE_VERSION {
            return Err(format!("save state version {} (expected {})", s.version, STATE_VERSION));
        }
        if s.rom_len != self.bus.card.len() {
            return Err("save state is from a different game".into());
        }
        self.cpu = s.cpu;
        self.bus.vdc = s.vdc;
        self.bus.vce = s.vce;
        self.bus.vce.rebuild();
        self.bus.psg.load_state(s.psg);
        self.bus.io = s.io;
        self.bus.card.load_state(s.card);
        self.line_cycles = s.line_cycles;
        self.current_frame = s.frame;
        Ok(())
    }
}
