use crate::common::cpu::z80::Z80Cpu;
use crate::common::input as pad;
use crate::common::system::{System,DisplayInfo,Rotation};
use crate::arcade::take_region;
use crate::arcade::pacman::pacman_bus::{PacmanBus, Board};
use crate::arcade::pacman::pacman_video::{PacmanVideo, SCREEN_W, SCREEN_H};
use crate::arcade::pacman::namco_wsg::NamcoWsg;
use crate::arcade::pacman::mspacman;
use crate::common::encryption::sega_crypt;
use crate::arcade::pacman::pacman_bus::PacmanBusState;
use crate::arcade::hiscore::HiScoreMem;
use crate::arcade::state;
use crate::common::cpu::z80::{Bus, Z80State};
use std::collections::HashMap;

const STATE_VERSION: u32 = 1;

type PacmanState = (Z80State, PacmanBusState, i32, bool, u8, u64);

const CYCLES_PER_LINE: i32 = 192;
const LINES_PER_FRAME: u16 = 264;
const VBLANK_START: u16 = 224;
const WSG_TICKS_PER_LINE: u32 = 6;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Variant {
    Pacman,
    MsPacman,
    Pengo(Cipher),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cipher {
    None,
    Sega5010,
    Sega5007,
}

impl Variant {
    fn is_pengo(self) -> bool { matches!(self, Variant::Pengo(_)) }
}

const COIN_PULSE_FRAMES: u8 = 2;

pub struct PacmanSystem {
    pub cpu: Z80Cpu,
    pub bus: PacmanBus,
    variant: Variant,
    line_cycles: i32,
    current_frame: u64,
    coin_was_down: bool,
    coin_pulse: u8,
    save_filename: String,
    save_path: String,
}

impl PacmanSystem {
    pub fn new(variant: Variant, set_name: &str, mut regions: HashMap<String, Vec<u8>>) -> Result<Self, String> {
        let rom   = take_region(&mut regions, "maincpu")?;
        let gfx1  = take_region(&mut regions, "gfx1")?;
        let proms = take_region(&mut regions, "proms")?;
        let namco = take_region(&mut regions, "namco")?;

        let (rom, opcodes) = match variant {
            Variant::Pengo(Cipher::Sega5010) => { let d = sega_crypt::decode_315_5010(&rom); (d.data, Some(d.opcodes)) }
            Variant::Pengo(Cipher::Sega5007) => { let d = sega_crypt::decode_315_5007(&rom); (d.data, Some(d.opcodes)) }
            _ => (rom, None),
        };
        let banks = match variant {
            Variant::MsPacman => mspacman::build_banks(&rom)?.to_vec(),
            Variant::Pacman | Variant::Pengo(_) => {
                let mut img = vec![0u8; 0x10000];
                let n = rom.len().min(0x10000);
                img[..n].copy_from_slice(&rom[..n]);
                if rom.len() <= 0x4000 {
                    img.copy_within(0..0x4000, 0x8000);
                }
                vec![img]
            }
        };

        let mut video = PacmanVideo::new(&gfx1, &proms)?;
        video.sprite_offset_hack = !variant.is_pengo();
        let board = if variant.is_pengo() { Board::Pengo } else { Board::Pacman };
        let wsg = NamcoWsg::new(&namco[..namco.len().min(0x100)], 44100);
        Ok(Self {
            cpu: Z80Cpu::new(),
            bus: PacmanBus::new(board, banks, opcodes, variant == Variant::MsPacman, video, wsg),
            variant,
            line_cycles: 0,
            current_frame: 0,
            coin_was_down: false,
            coin_pulse: 0,
            save_filename: set_name.to_string(),
            save_path: "user://Arcade/Pacman/State".to_string(),
        })
    }

}

impl System for PacmanSystem {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        for line in 0..LINES_PER_FRAME {
            if line == VBLANK_START {
                self.bus.video.render_frame();
                if self.bus.irq_enable { self.bus.irq_line = true; }
            }

            while self.line_cycles < CYCLES_PER_LINE {
                if self.bus.irq_line {
                    let vector = self.bus.irq_vector;
                    let c = self.cpu.irq(&mut self.bus, vector);
                    if c > 0 { self.line_cycles += c as i32; continue; }
                }
                self.line_cycles += self.cpu.step(&mut self.bus) as i32;
            }
            self.line_cycles -= CYCLES_PER_LINE;
            for _ in 0..WSG_TICKS_PER_LINE { self.bus.wsg.tick(); }
        }
        self.current_frame += 1;

        if self.coin_pulse > 0 {
            self.coin_pulse -= 1;
            if self.coin_pulse == 0 { self.bus.in0 |= 0x10; } // release Pengo's coin line
        }
    }

    fn set_input(&mut self, mask: u32) {
        let on = |b: u32| mask & b != 0;
        let coin = on(pad::COIN) || on(pad::SELECT);
        let mut in0 = 0xFFu8;
        let mut in1 = 0xFFu8;
        
        if self.variant.is_pengo() {
            if on(pad::UP)    { in0 &= !0x01; }
            if on(pad::DOWN)  { in0 &= !0x02; }
            if on(pad::LEFT) { in0 &= !0x04; }
            if on(pad::RIGHT)  { in0 &= !0x08; }
            if on(pad::A) || on(pad::B) { in0 &= !0x80; } // push the ice block

            if coin && !self.coin_was_down { self.coin_pulse = COIN_PULSE_FRAMES; }
            self.coin_was_down = coin;
            if self.coin_pulse > 0 { in0 &= !0x10; }
        } else {
            if on(pad::UP)    { in0 &= !0x01; }
            if on(pad::LEFT)  { in0 &= !0x02; }
            if on(pad::RIGHT) { in0 &= !0x04; }
            if on(pad::DOWN)  { in0 &= !0x08; }
            if coin { in0 &= !0x20; }
        }
        if on(pad::START)  { in1 &= !0x20; }
        if on(pad::START2) { in1 &= !0x40; }
        self.bus.in0 = in0;
        self.bus.in1 = in1;
    }

    fn drain_audio(&mut self) -> Vec<f32> { std::mem::take(&mut self.bus.wsg.out) }
    fn sample_rate(&self) -> u32 { 44100 }
    fn power_on(&mut self) { }
    fn power_off(&mut self) { }
    fn has_battery(&self) -> bool { false }
    fn save_filename(&self) -> &str { &self.save_filename }
    fn save_battery_path(&self) -> &str { &self.save_path }
    fn save_state_path(&self) -> &str { &self.save_path }
    fn is_sram_dirty(&self) -> bool { false }
    fn clear_sram_dirty(&mut self) { }
    fn get_current_frame(&self) -> i64 { self.current_frame as i64 }
    fn get_sram(&self) -> Option<&[u8]> { None }
    fn load_sram(&mut self, data: &[u8]) { }

    fn framebuffer(&self) -> &[u8] { &self.bus.video.framebuffer }

    fn reset(&mut self) {
        self.cpu = Z80Cpu::new();
        self.line_cycles = 0;
        self.bus.irq_enable = false;
        self.bus.irq_line = false;
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
            rotation: Rotation::CW90,
        }
    }

    fn save_state(&self) -> Vec<u8> {
        let s: PacmanState = (self.cpu.save_state(), self.bus.save(), self.line_cycles,
                              self.coin_was_down, self.coin_pulse, self.current_frame);
        state::encode(&self.save_filename, STATE_VERSION, &s)
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let (cpu, bus, line_cycles, coin_was_down, coin_pulse, frame): PacmanState =
            state::decode(&self.save_filename, STATE_VERSION, data)?;
        self.bus.load(bus)?;
        self.cpu.load_state(&cpu);
        self.line_cycles = line_cycles;
        self.coin_was_down = coin_was_down;
        self.coin_pulse = coin_pulse;
        self.current_frame = frame;
        Ok(())
    }
}

impl HiScoreMem for PacmanSystem {
    fn hs_read(&mut self, addr: u16) -> u8 { self.bus.read(addr) }
    fn hs_write(&mut self, addr: u16, v: u8) { self.bus.write(addr, v) }
}
