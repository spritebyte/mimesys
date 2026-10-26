//! Atari System 2 board (MAME atarisy2.cpp). Paperboy for now.
//!
//! Main: DEC T-11 @ 10 MHz. Sound: 6502 @ 1.79 MHz with 2 POKEYs, YM2151, TMS5220,
//! a 512-byte EEPROM (high scores and settings: saved as the battery file).
//! Screen 512x384 of 640x416 at 16 MHz: 400 T-11 cycles per line, 60.1 Hz.
//!
//! Main CPU interrupts (T-11 CP lines, levels from the T-11 table):
//!   CP0 sound CPU read the command, CP1 sound CPU wrote a reply, CP2 32V (every
//!   64 lines), CP3 VBLANK. Each is latched only if enabled in 0x1600.

use std::collections::HashMap;
use serde::{Deserialize, Serialize};

use crate::arcade::atarisy2::slapstic::Slapstic;
use crate::arcade::atarisy2::sy2_sound::SoundChips;
use crate::arcade::atarisy2::sy2_video::{Sy2Video, SCREEN_H, SCREEN_W};
use crate::arcade::{state, take_region};
use crate::common::cpu::m6502::{AddressBus, CpuState, CpuVariant, M6502Cpu};
use crate::common::cpu::t11::{T11Bus, T11};
use crate::common::input as pad;
use crate::common::system::{DisplayInfo, Rotation, System};

const STATE_VERSION: u32 = 4;
const LINES_PER_FRAME: u16 = 416;
const VBLANK_LINE: u16 = 384;
const T11_CYCLES_PER_LINE: i64 = 400;
/// 6502 clock = 14.318181 MHz / 8; main clock = 10 MHz. Ratio as integers:
/// 6502 cycles per line = 400 * 14318181 / 80_000_000.
const SND_NUM: i64 = 14_318_181;
const SND_DEN: i64 = 80_000_000 / 400;   // per line: SND_NUM / SND_DEN = 71.59
/// Sound IRQ every 10 MHz / 16 / 16 / 16 / 10 = 40960 main cycles.
const SOUND_IRQ_PERIOD: i64 = 40960;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Variant { Paperboy }

/// Everything both CPUs can reach. The T-11 sees it through `T11Bus`, the 6502
/// through `SoundSide`.
#[derive(Serialize, Deserialize)]
pub struct Board {
    #[serde(skip)] main_rom: Vec<u8>,     // 0x90000: 0x8000-0xFFFF program, banks from 0x10000
    #[serde(skip)] snd_rom: Vec<u8>,      // 0x10000 (0x4000-0xFFFF used)
    pub(crate) ram: Vec<u8>,              // 0x0000-0x0FFF
    pub video: Sy2Video,
    pub(crate) slapstic: Slapstic,
    rombank: [usize; 2],                  // byte offsets into main_rom
    int_enable: u8,
    scanline_int: bool,
    video_int: bool,
    p2portwr: bool,                       // CP1: sound wrote a reply
    p2portrd: bool,                       // CP0: sound read the command
    soundlatch: u8, soundlatch_full: bool,  // main -> sound (6502 NMI while full)
    mainlatch: u8, mainlatch_full: bool,    // sound -> main
    sound_reset: bool,                    // 6502 held in reset by the main CPU
    sound_irq: bool,
    adc_channel: u8,
    pub adc: [u8; 4],
    pub in0: u16,
    pub in1: u8,
    pub dsw: [u8; 2],
    snd_ram: Vec<u8>,
    pub eeprom: Vec<u8>,
    pub eeprom_dirty: bool,
    pub chips: SoundChips,
    line: u16,
}

impl Board {
    fn update_cp(&self, cpu: &mut T11) {
        cpu.cp_state = (self.p2portrd as u8) | (self.p2portwr as u8) << 1
            | (self.scanline_int as u8) << 2 | (self.video_int as u8) << 3;
    }

    fn main_rom_byte(&self, a: u16) -> u8 {
        let off = match a {
            0x4000..=0x5FFF => self.rombank[0] + (a as usize - 0x4000),
            0x6000..=0x7FFF => self.rombank[1] + (a as usize - 0x6000),
            _ => a as usize,
        };
        self.main_rom.get(off).copied().unwrap_or(0xFF)
    }

    /// 0x2000-0x3FFF through the slapstic-selected view.
    fn view_word(&mut self, a: u16) -> Option<&mut u16> {
        let w = ((a - 0x2000) >> 1) as usize;
        let v = &mut self.video;
        match self.slapstic.current_bank {
            0 if a < 0x3800 => Some(&mut v.alpha_ram[w]),
            0 => Some(&mut v.mo_ram[w - 0xC00]),
            2 => Some(&mut v.pf_ram[w]),
            3 => Some(&mut v.pf_ram[0x1000 + w]),
            _ => None,
        }
    }

    /// Main CPU word read (addr even). Side effects happen here.
    fn main_read(&mut self, a: u16) -> u16 {
        match a {
            0x0000..=0x0FFF => u16::from_le_bytes([self.ram[a as usize], self.ram[a as usize + 1]]),
            0x1000..=0x13FF => self.video.palette_ram[((a >> 1) & 0xFF) as usize],
            0x1400..=0x147F => 0xFF00 | self.adc[(self.adc_channel & 3) as usize] as u16,
            0x1800..=0x1BFF => {
                let mut v = self.in0 & !0x30;
                if self.mainlatch_full { v |= 0x10; }
                if self.soundlatch_full { v |= 0x20; }
                v
            }
            0x1C00..=0x1FFF => {
                self.p2portwr = false;
                self.mainlatch_full = false;
                0xFF00 | self.mainlatch as u16
            }
            0x2000..=0x3FFF => self.view_word(a).map(|w| *w).unwrap_or(0xFFFF),
            0x4000..=0xFFFF => u16::from_le_bytes([self.main_rom_byte(a), self.main_rom_byte(a + 1)]),
            _ => 0xFFFF,
        }
    }

    /// Main CPU write. `mask` = which bytes are written (0x00FF, 0xFF00 or 0xFFFF).
    fn main_write(&mut self, a: u16, data: u16, mask: u16) {
        let merge = |old: u16| (old & !mask) | (data & mask);
        let lo = mask & 0x00FF != 0;
        match a {
            0x0000..=0x0FFF => {
                if lo { self.ram[a as usize] = data as u8; }
                if mask & 0xFF00 != 0 { self.ram[a as usize + 1] = (data >> 8) as u8; }
            }
            0x1000..=0x13FF => {
                let i = ((a >> 1) & 0xFF) as usize;
                let v = merge(self.video.palette_ram[i]);
                self.video.write_palette(i, v);
            }
            0x1400..=0x147F => {
                // ROM bank select: 0x1400 -> 0x4000 window, 0x1402 -> 0x6000 window
                let n = (((data >> 10) & 0o77) ^ 3) as usize;
                let n = (n & 0x30) | ((n & 0x03) << 2) | ((n >> 2) & 0x03);
                self.rombank[((a >> 1) & 1) as usize] = 0x10000 + n * 0x2000;
            }
            0x1480..=0x14FF => if lo { self.adc_channel = ((a >> 1) & 7) as u8; },
            0x1580..=0x159F => if lo { self.p2portrd = false; },
            0x15A0..=0x15BF => if lo { self.set_sound_reset(data & 1 != 0); },
            0x15C0..=0x15DF => if lo { self.scanline_int = false; },
            0x15E0..=0x15FF => if lo { self.video_int = false; },
            0x1600..=0x167F => if lo { self.int_enable = data as u8; },
            0x1680..=0x16FF => if lo { self.soundlatch = data as u8; self.soundlatch_full = true; },
            0x1700..=0x177F => { let v = merge(self.video.xscroll); self.video.xscroll_w(v); }
            0x1780..=0x17FF => { let v = merge(self.video.yscroll); let l = self.line; self.video.yscroll_w(v, l); }
            0x1800..=0x1BFF => {}  // watchdog
            0x2000..=0x3FFF => if let Some(w) = self.view_word(a) { *w = (*w & !mask) | (data & mask); },
            _ => {}
        }
    }

    fn set_sound_reset(&mut self, held: bool) {
        self.sound_reset = held;
        // MAME also resets the YM2151 / TMS5220 and mixer here (sndrst_6502_w(0))
        self.chips.sound_reset();
    }

    // ---- sound CPU side ----
    fn snd_read(&mut self, a: u16) -> u8 {
        if a >= 0x4000 { return self.snd_rom[a as usize]; }
        if a & 0x1000 == 0 { return self.snd_ram[(a & 0x0FFF) as usize]; }
        if a & 0x0800 == 0 { return self.eeprom[(a & 0x1FF) as usize]; }
        match a & 0x7F {
            0x00..=0x0F => self.chips.pokey_read(0, (a & 0xF) as u8, self.dsw[0]),
            0x10..=0x1F => 0xFF,                    // LETA (720° only)
            0x30..=0x3F => self.chips.pokey_read(1, (a & 0xF) as u8, self.dsw[1]),
            0x40..=0x4F => {
                let mut v = self.in1 & !0x07;
                if self.soundlatch_full { v |= 0x01; }
                if self.mainlatch_full { v |= 0x02; }
                if !self.chips.tms_ready() { v |= 0x04; }
                v
            }
            0x50..=0x5F => self.chips.ym_read((a & 1) as u8),
            0x60..=0x6F => {
                // reading the command: CP0 to the main CPU (if enabled), latch empties
                self.p2portrd = self.int_enable & 1 != 0;
                self.soundlatch_full = false;
                self.soundlatch
            }
            _ => 0xFF,
        }
    }

    fn snd_write(&mut self, a: u16, v: u8) {
        if a >= 0x4000 { return; }
        if a & 0x1000 == 0 { self.snd_ram[(a & 0x0FFF) as usize] = v; return; }
        if a & 0x0800 == 0 {
            let i = (a & 0x1FF) as usize;
            if self.eeprom[i] != v { self.eeprom[i] = v; self.eeprom_dirty = true; }
            return;
        }
        match a & 0x7F {
            0x00..=0x0F => self.chips.pokey_write(0, (a & 0xF) as u8, v),
            0x30..=0x3F => self.chips.pokey_write(1, (a & 0xF) as u8, v),
            0x50..=0x5F => self.chips.ym_write((a & 1) as u8, v),
            0x70 | 0x71 => self.chips.tms_data(v),
            0x72 | 0x73 => self.chips.tms_wsq(a & 1 == 0),
            0x74 | 0x75 => {
                self.p2portwr = self.int_enable & 2 != 0;
                self.mainlatch = v;
                self.mainlatch_full = true;
            }
            0x76 | 0x77 => {}                       // coin counters
            0x78 | 0x79 => self.sound_irq = false,  // IRQ acknowledge
            0x7A | 0x7B => self.chips.mixer(v),
            0x7C | 0x7D => self.chips.misc(v),
            0x7E | 0x7F => self.chips.sound_enable(v & 1 != 0),
            _ => {}
        }
    }
}

impl T11Bus for Board {
    fn read_word(&mut self, a: u16) -> u16 {
        self.slapstic.access(a);
        self.main_read(a)
    }
    fn write_word(&mut self, a: u16, v: u16) {
        self.slapstic.access(a);
        self.main_write(a, v, 0xFFFF);
    }
    fn read_byte(&mut self, a: u16) -> u8 {
        self.slapstic.access(a);
        let w = self.main_read(a & !1);
        if a & 1 != 0 { (w >> 8) as u8 } else { w as u8 }
    }
    fn write_byte(&mut self, a: u16, v: u8) {
        self.slapstic.access(a);
        if a & 1 != 0 { self.main_write(a & !1, (v as u16) << 8, 0xFF00) }
        else { self.main_write(a, v as u16, 0x00FF) }
    }
}

struct SoundSide<'a>(&'a mut Board);

impl AddressBus for SoundSide<'_> {
    fn read_byte(&mut self, addr: u16) -> u8 { self.0.snd_read(addr) }
    fn write_byte(&mut self, addr: u16, value: u8) { self.0.snd_write(addr, value) }
    fn is_nmi_line_asserted(&mut self) -> bool { self.0.soundlatch_full }
    fn is_irq_line_asserted(&mut self) -> bool { self.0.sound_irq }
    fn is_nmi_enabled(&mut self) -> bool { true }
    fn step_cycles(&mut self, _cycles: u64) {}
    fn begin_cpu_cycle(&mut self) {}
}

pub struct Sy2System {
    pub(crate) cpu: T11,
    snd_cpu: M6502Cpu,
    pub(crate) board: Board,
    _variant: Variant,
    main_debt: i64,        // T-11 cycles run past the end of the last line
    snd_acc: i64,          // fractional 6502 cycles (units of 1/SND_DEN)
    irq_timer: i64,        // main cycles until the next sound IRQ
    snd_was_reset: bool,
    // handlebar positions (MAME: ramps 10 per frame toward the stick, back to centre)
    stick: [i32; 2],
    test_switch: bool,     // self-test switch, flipped by Start 2
    prev_start2: bool,
    current_frame: u64,
    save_filename: String,
    save_path: String,
    state_path: String,
}

#[derive(Serialize, Deserialize)]
struct Sy2State<B> {
    cpu: T11,
    snd_cpu: CpuState,
    board: B,
    main_debt: i64,
    snd_acc: i64,
    irq_timer: i64,
    snd_was_reset: bool,
    frame: u64,
}

impl Sy2System {
    pub fn new(variant: Variant, set_name: &str, mut regions: HashMap<String, Vec<u8>>) -> Result<Self, String> {
        let mut main_rom = take_region(&mut regions, "maincpu")?;
        let mut snd_rom = take_region(&mut regions, "audiocpu")?;
        let tiles = take_region(&mut regions, "tiles")?;
        let sprites = take_region(&mut regions, "sprites")?;
        let chars = take_region(&mut regions, "chars")?;
        let mut eeprom = regions.remove("eeprom").unwrap_or_else(|| vec![0xFF; 0x200]);
        eeprom.resize(0x200, 0xFF);
        main_rom.resize(0x90000, 0xFF);
        snd_rom.resize(0x10000, 0xFF);
        if variant == Variant::Paperboy {
            // MAME init_paperboy: each 16K bank pair fills a whole 64K chunk
            for i in (0x10000..0x90000).step_by(0x20000) {
                let src = main_rom[i..i + 0x8000].to_vec();
                for k in 1..4 { main_rom[i + k * 0x8000..i + (k + 1) * 0x8000].copy_from_slice(&src); }
            }
        }
        let board = Board {
            main_rom, snd_rom,
            ram: vec![0; 0x1000],
            video: Sy2Video::new(&tiles, &sprites, &chars)?,
            slapstic: Slapstic::new_105(),
            rombank: [0x10000, 0x10000],
            int_enable: 0, scanline_int: false, video_int: false, p2portwr: false, p2portrd: false,
            soundlatch: 0, soundlatch_full: false, mainlatch: 0, mainlatch_full: false,
            sound_reset: true, sound_irq: false,
            adc_channel: 0, adc: [0x80, 0x80, 0xFF, 0xFF],
            in0: 0xFFFF, in1: 0xFF, dsw: [0x00, 0x00],
            snd_ram: vec![0; 0x1000],
            eeprom, eeprom_dirty: false,
            chips: SoundChips::new(44100),
            line: 0,
        };
        let mut sys = Self {
            cpu: T11::new(0x36FF),
            snd_cpu: M6502Cpu::new(CpuVariant::NMOS6502),
            board, _variant: variant,
            main_debt: 0, snd_acc: 0, irq_timer: SOUND_IRQ_PERIOD, snd_was_reset: true,
            stick: [0x80, 0x80],
            test_switch: false, prev_start2: false,
            current_frame: 0,
            save_filename: set_name.to_string(),
            save_path: "user://GD_EMU/Arcade/AtariSys2/Save".to_string(),
            state_path: "user://GD_EMU/Arcade/AtariSys2/State".to_string(),
        };
        sys.board.set_sound_reset(true);
        Ok(sys)
    }

    fn run_line(&mut self) {
        let b = &mut self.board;
        // main CPU
        let mut left = T11_CYCLES_PER_LINE - self.main_debt;
        while left > 0 {
            b.update_cp(&mut self.cpu);
            let c = self.cpu.step(b) as i64;
            let c = c.max(1);
            left -= c;
            self.irq_timer -= c;
            if self.irq_timer <= 0 {
                self.irq_timer += SOUND_IRQ_PERIOD;
                b.sound_irq = true;
            }
        }
        self.main_debt = -left;
        b.update_cp(&mut self.cpu);

        // sound CPU
        self.snd_acc += SND_NUM;
        let n = self.snd_acc / SND_DEN;
        self.snd_acc %= SND_DEN;
        if b.sound_reset {
            self.snd_was_reset = true;
            let dsw = b.dsw; b.chips.run(n as u32, dsw);
            return;
        }
        if self.snd_was_reset {
            self.snd_was_reset = false;
            self.snd_cpu.reset(&mut SoundSide(b));
        }
        for _ in 0..n {
            self.snd_cpu.sample_interrupt_lines(&mut SoundSide(b));
            self.snd_cpu.step_one_cycle(&mut SoundSide(b));
        }
        let dsw = b.dsw; b.chips.run(n as u32, dsw);
    }

    fn update_sticks(&mut self, mask: u32) {
        let target = |neg: bool, pos: bool| if neg && !pos { 0x10 } else if pos && !neg { 0xF0 } else { 0x80 };
        let t = [
            target(mask & pad::LEFT != 0, mask & pad::RIGHT != 0),
            target(mask & pad::UP != 0, mask & pad::DOWN != 0),
        ];
        for i in 0..2 {
            let d = t[i] - self.stick[i];
            self.stick[i] += d.clamp(-24, 24);
            self.board.adc[i] = self.stick[i] as u8;
        }
    }
}

impl System for Sy2System {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        self.update_sticks(input);
        self.board.video.start_frame();
        for line in 0..LINES_PER_FRAME {
            self.board.line = line;
            if line % 64 == 0 && line <= VBLANK_LINE {
                self.board.scanline_int = self.board.int_enable & 4 != 0;
            }
            if line == VBLANK_LINE {
                self.board.video_int = self.board.int_enable & 8 != 0;
            }
            if line < SCREEN_H as u16 {
                self.board.video.render_line(line as usize);
            }
            self.run_line();
        }
        self.current_frame += 1;
    }

    /// IN0 (main, active low): 0x80 button 1 (left throw), 0x40 button 2 (right throw),
    /// 0x8000 self test (Start 2 toggles it). IN1 (sound): 0x40 coin 1, 0x80 coin 2, 0x20 service coin.
    /// Handlebars: left/right and up (faster) / down (slower) on the ADC.
    fn set_input(&mut self, mask: u32) {
        let on = |b: u32| mask & b != 0;
        let mut in0: u16 = 0xFFFF;
        if on(pad::A) || on(pad::START) { in0 &= !0x80; }
        if on(pad::B) { in0 &= !0x40; }
        // Start 2 flips the self-test switch (it stays on, like the real one)
        if on(pad::START2) && !self.prev_start2 { self.test_switch = !self.test_switch; }
        self.prev_start2 = on(pad::START2);
        if self.test_switch { in0 &= !0x8000; }
        self.board.in0 = in0;
        let mut in1: u8 = 0xFF;
        if on(pad::COIN) || on(pad::SELECT) { in1 &= !0x40; }
        self.board.in1 = in1;
    }

    fn drain_audio(&mut self) -> Vec<f32> { self.board.chips.drain() }
    fn sample_rate(&self) -> u32 { 44100 }
    fn power_on(&mut self) {}
    fn power_off(&mut self) {}
    fn has_battery(&self) -> bool { true }
    fn save_filename(&self) -> &str { &self.save_filename }
    fn save_battery_path(&self) -> &str { &self.save_path }
    fn save_state_path(&self) -> &str { &self.state_path }
    fn is_sram_dirty(&self) -> bool { self.board.eeprom_dirty }
    fn clear_sram_dirty(&mut self) { self.board.eeprom_dirty = false; }
    fn get_current_frame(&self) -> i64 { self.current_frame as i64 }
    fn get_sram(&self) -> Option<&[u8]> { Some(&self.board.eeprom) }
    fn load_sram(&mut self, data: &[u8]) {
        if data.len() == 0x200 { self.board.eeprom.copy_from_slice(data); }
    }
    fn framebuffer(&self) -> &[u8] { &self.board.video.framebuffer }

    fn reset(&mut self) {
        self.cpu = T11::new(0x36FF);
        let b = &mut self.board;
        b.int_enable = 0;
        b.scanline_int = false; b.video_int = false; b.p2portwr = false; b.p2portrd = false;
        b.slapstic.reset();
        b.set_sound_reset(true);
        self.main_debt = 0;
    }

    /// "dsw0" / "dsw1": the two DIP banks (MAME's paperboy DSW0 / DSW1, read through the POKEYs).
    fn set_option(&mut self, key: &str, value: i64) {
        match key {
            "dsw0" => self.board.dsw[0] = value as u8,
            "dsw1" => self.board.dsw[1] = value as u8,
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
        state::encode(&self.save_filename, STATE_VERSION, &Sy2State {
            cpu: self.cpu.clone(), snd_cpu: self.snd_cpu.get_state(), board: &self.board,
            main_debt: self.main_debt, snd_acc: self.snd_acc, irq_timer: self.irq_timer,
            snd_was_reset: self.snd_was_reset, frame: self.current_frame,
        })
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let s: Sy2State<Board> = state::decode(&self.save_filename, STATE_VERSION, data)?;
        let mut board = s.board;
        board.main_rom = std::mem::take(&mut self.board.main_rom);
        board.snd_rom = std::mem::take(&mut self.board.snd_rom);
        board.video.keep_static(&mut self.board.video);
        board.chips.keep_output(&mut self.board.chips);
        board.eeprom_dirty = true;
        self.board = board;
        self.cpu = s.cpu;
        self.snd_cpu.load_state(&s.snd_cpu);
        self.main_debt = s.main_debt;
        self.snd_acc = s.snd_acc;
        self.irq_timer = s.irq_timer;
        self.snd_was_reset = s.snd_was_reset;
        self.current_frame = s.frame;
        Ok(())
    }
}
