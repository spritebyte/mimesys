use crate::common::cpu::z80::Z80Cpu;
use crate::common::input as pad;
use crate::common::system::{System, DisplayInfo, Rotation};
use crate::arcade::take_region;
use crate::arcade::galaxian::galaxian_bus::{GalaxianBus, Layout, SoundBoard};
use crate::arcade::galaxian::konami_sound::{self, KonamiBoard, KonamiSound, KONAMI_SOUND_CLOCK};
use crate::arcade::galaxian::galaxian_video::{GalaxianVideo, Extension, SCREEN_W, SCREEN_H, VBLANK_START};
use crate::arcade::galaxian::galaxian_sound::{GalaxianSound, Mixing};
use crate::arcade::galaxian::galaxian_bus::BusState;
use crate::arcade::hiscore::HiScoreMem;
use crate::arcade::state;
use crate::common::cpu::z80::{Bus, Z80State};
use std::collections::HashMap;

const STATE_VERSION: u32 = 1;

// 18.432 MHz crystal: Z80 at 3.072 MHz, 384 pixel clocks (6.144 MHz) per line
// -> 192 CPU cycles per line, 264 lines, 60.6 frames per second. Same as Pac-Man.
const CYCLES_PER_LINE: i32 = 192;
const LINES_PER_FRAME: usize = 264;
const SAMPLE_RATE: u32 = 44100;
const SAMPLES_PER_LINE_X100K: u64 = 44100 * 100_000 * 192 / 3_072_000;   // 2.75625 samples per line, exact

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Variant {
    Galaxian,           // galaxian and its many hacks/bootlegs
    MoonCresta,         // encrypted program ROMs (mooncrst, mooncrstu, ...)
    MoonCrestaPlain,    // unencrypted (mooncrstuu, mooncrstg, ...)
    Frogger,            // Konami: same video, 8255 I/O, Z80 + AY-3-8910 sound board
    Amidar,             // Konami "Turtles" board: 8255 I/O, Z80 + 2 x AY-3-8910, background color latches
    Turtles,            // same board as Amidar (Turtles, Turpin, 600); different DIP defaults
}

/// Sound CPU cycles per video line, x 1_000_000 to keep the fraction (111.86).
const SOUND_CYCLES_PER_LINE_X1M: i64 = (KONAMI_SOUND_CLOCK * 192.0 / 3_072_000.0 * 1e6) as i64;

pub struct GalaxianSystem {
    pub cpu: Z80Cpu,
    pub bus: GalaxianBus,
    line_cycles: i32,
    nmi_line: bool,
    sample_debt: u64,
    sound_cycle_debt: i64,
    current_frame: u64,
    save_filename: String,
    save_path: String,
}

impl GalaxianSystem {
    pub fn new(variant: Variant, set_name: &str, mut regions: HashMap<String, Vec<u8>>) -> Result<Self, String> {
        let mut rom = take_region(&mut regions, "maincpu")?;
        let gfx1 = take_region(&mut regions, "gfx1")?;
        let proms = take_region(&mut regions, "proms")?;

        if variant == Variant::MoonCresta {
            decode_mooncrst(&mut rom);
        }
        let (layout, extension, dips, sound) = match variant {
            // DIP defaults (IN1, IN2): 1 coin 1 credit, bonus 7000, 3 lives
            Variant::Galaxian => (Layout::Galaxian, Extension::None, (0x00, 0x04),
                SoundBoard::Discrete(GalaxianSound::new(SAMPLE_RATE, Mixing::Galaxian))),
            // bonus 30000, English, 1 coin 1 credit
            Variant::MoonCresta | Variant::MoonCrestaPlain => (Layout::MoonCresta, Extension::MoonCresta, (0x80, 0x00),
                SoundBoard::Discrete(GalaxianSound::new(SAMPLE_RATE, Mixing::MoonCresta))),
            // active low: 3 lives (bits 0-1 = 0), 1 coin 1 credit, upright
            Variant::Frogger => {
                let mut audio = take_region(&mut regions, "audiocpu")?;
                let n = audio.len().min(0x800);
                konami_sound::swap_d0_d1(&mut audio[..n]);
                (Layout::Frogger, Extension::Frogger, (0xFC, 0xF1),
                 SoundBoard::Konami(KonamiSound::new(KonamiBoard::Frogger, audio, SAMPLE_RATE)))
            }
            // Amidar: lives bits 0x03 = 3; demo sounds on, upright
            // Turtles/Turpin: lives bits 0x00 = 3; coinage 1/1, upright
            Variant::Amidar | Variant::Turtles => {
                let audio = take_region(&mut regions, "audiocpu")?;
                let lives = if variant == Variant::Amidar { 0x03 } else { 0x00 };
                (Layout::Turtles, Extension::Turtles, (0xFC | lives, 0xF1),
                 SoundBoard::Konami(KonamiSound::new(KonamiBoard::TwoAy, audio, SAMPLE_RATE)))
            }
        };
        let mut gfx1 = gfx1;
        if variant == Variant::Frogger && gfx1.len() >= 0x1000 {
            konami_sound::swap_d0_d1(&mut gfx1[0x800..0x1000]);   // 2nd gfx ROM has D0/D1 swapped too
        }
        let video = GalaxianVideo::new(&gfx1, &proms, extension)?;

        let mut bus = GalaxianBus::new(layout, rom, video, sound, dips);
        // IN3 (8255 #2 port C): Amidar's coinage DIPs, 0xFF = 1 coin 1 credit on both slots
        // (0x00 would mean free play / slots disabled). Unused on Turtles.
        if variant == Variant::Amidar { bus.in3 = 0xFF; }

        Ok(Self {
            cpu: Z80Cpu::new(),
            bus,
            line_cycles: 0,
            nmi_line: false,
            sample_debt: 0,
            sound_cycle_debt: 0,
            current_frame: 0,
            save_filename: set_name.to_string(),
            save_path: "user://GD_EMU/Arcade/Galaxian/State".to_string(),
        })
    }
}

impl System for GalaxianSystem {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        for line in 0..LINES_PER_FRAME {
            self.bus.current_line = line;

            // VBLANK clocks the interrupt flip-flop; it stays set until the game writes 0
            // to the NMI enable latch. NMI is edge triggered, so one NMI per vblank.
            if line == VBLANK_START && self.bus.nmi_enable && !self.nmi_line {
                self.nmi_line = true;
                self.line_cycles += self.cpu.nmi(&mut self.bus) as i32;
            }

            while self.line_cycles < CYCLES_PER_LINE {
                self.line_cycles += self.cpu.step(&mut self.bus) as i32;
                if !self.bus.nmi_enable { self.nmi_line = false; }
            }
            self.line_cycles -= CYCLES_PER_LINE;

            self.bus.video.render_line(line);

            match &mut self.bus.sound {
                SoundBoard::Discrete(s) => {
                    // whole samples per line, carrying the fraction
                    self.sample_debt += SAMPLES_PER_LINE_X100K;
                    let n = (self.sample_debt / 100_000) as usize;
                    self.sample_debt %= 100_000;
                    s.render(n);
                }
                SoundBoard::Konami(k) => {
                    self.sound_cycle_debt += SOUND_CYCLES_PER_LINE_X1M;
                    let n = self.sound_cycle_debt / 1_000_000;
                    self.sound_cycle_debt %= 1_000_000;
                    k.run(n);
                }
            }
        }
        self.bus.video.end_of_frame();
        self.current_frame += 1;
    }

    /// Active high, same layout on both boards.
    ///   IN0: 0x01 coin 1, 0x02 coin 2, 0x04 left, 0x08 right, 0x10 fire
    ///   IN1: 0x01 start 1, 0x02 start 2 (upper bits are DIP switches)
    fn set_input(&mut self, mask: u32) {
        let on = |b: u32| mask & b != 0;
        if matches!(self.bus.layout(), Layout::Frogger | Layout::Turtles) {
            // Active low. Frogger and the Turtles board (Amidar) share the layout.
            //   IN0: 0x80 coin 1, 0x40 coin 2, 0x20 left, 0x10 right,
            //        0x08 button (Amidar's "jump"; Turtles board only)
            //   IN1: 0x80 start 1, 0x40 start 2 (bits 0-1: lives DIP)
            //   IN2: 0x10 up, 0x40 down (bits 1-3: coinage/cabinet DIPs)
            let mut in0 = 0xFFu8;
            if on(pad::COIN) || on(pad::SELECT) { in0 &= !0x80; }
            if on(pad::LEFT)  { in0 &= !0x20; }
            if on(pad::RIGHT) { in0 &= !0x10; }
            if on(pad::A) && self.bus.layout() == Layout::Turtles { in0 &= !0x08; }
            self.bus.in0 = in0;
            let mut in1 = (self.bus.in1 & 0x03) | 0xFC;
            if on(pad::START)  { in1 &= !0x80; }
            if on(pad::START2) { in1 &= !0x40; }
            self.bus.in1 = in1;
            let mut in2 = (self.bus.in2 & 0x0E) | 0xF1;
            if on(pad::UP)   { in2 &= !0x10; }
            if on(pad::DOWN) { in2 &= !0x40; }
            self.bus.in2 = in2;
            return;
        }
        let mut in0 = 0;
        if on(pad::COIN) || on(pad::SELECT) { in0 |= 0x01; }
        if on(pad::LEFT)  { in0 |= 0x04; }
        if on(pad::RIGHT) { in0 |= 0x08; }
        if on(pad::A) || on(pad::B) { in0 |= 0x10; }
        self.bus.in0 = in0;

        let mut in1 = self.bus.in1 & 0xE0;   // keep the DIP switch bits
        if on(pad::START)  { in1 |= 0x01; }
        if on(pad::START2) { in1 |= 0x02; }
        self.bus.in1 = in1;
    }

    fn drain_audio(&mut self) -> Vec<f32> {
        match &mut self.bus.sound {
            SoundBoard::Discrete(s) => std::mem::take(&mut s.out),
            SoundBoard::Konami(k) => k.drain(),
        }
    }
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
        self.nmi_line = false;
        self.bus.nmi_enable = false;
    }

    fn display_info(&self) -> DisplayInfo {
        DisplayInfo {
            buffer_width: SCREEN_W as i32,
            buffer_height: SCREEN_H as i32,
            visible_x: 0,
            visible_y: 0,
            visible_width: SCREEN_W as i32,
            visible_height: SCREEN_H as i32,
            target_aspect_ratio: 3.0 / 4.0,   // after rotation
            rotation: Rotation::CW90,         // MAME ROT90
        }
    }

    fn save_state(&self) -> Vec<u8> {
        let s = (self.cpu.save_state(), self.bus.save(), self.line_cycles, self.nmi_line,
                 self.sample_debt, self.sound_cycle_debt, self.current_frame);
        state::encode(&self.save_filename, STATE_VERSION, &s)
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let (cpu, bus, line_cycles, nmi_line, sample_debt, sound_cycle_debt, frame):
            (Z80State, BusState, i32, bool, u64, i64, u64) =
            state::decode(&self.save_filename, STATE_VERSION, data)?;
        self.bus.load(bus)?;
        self.cpu.load_state(&cpu);
        self.line_cycles = line_cycles;
        self.nmi_line = nmi_line;
        self.sample_debt = sample_debt;
        self.sound_cycle_debt = sound_cycle_debt;
        self.current_frame = frame;
        Ok(())
    }
}

impl HiScoreMem for GalaxianSystem {
    fn hs_read(&mut self, addr: u16) -> u8 { self.bus.read(addr) }
    fn hs_write(&mut self, addr: u16, v: u8) { self.bus.write(addr, v) }
}

/// Moon Cresta program ROM scramble (MAME decode_mooncrst): two data bits flip each
/// other, and on even addresses bits 6 and 2 swap.
pub fn decode_mooncrst(rom: &mut [u8]) {
    for (offs, b) in rom.iter_mut().take(0x4000).enumerate() {
        let data = *b;
        let mut res = data;
        if data & 0x02 != 0 { res ^= 0x40; }
        if data & 0x20 != 0 { res ^= 0x04; }
        if offs & 1 == 0 {
            // bitswap<8>(res, 7,2,5,4,3,6,1,0): swap bits 6 and 2
            res = (res & 0xBB) | ((res & 0x40) >> 4) | ((res & 0x04) << 4);
        }
        *b = res;
    }
}
