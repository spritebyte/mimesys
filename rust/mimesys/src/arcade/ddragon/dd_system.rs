//! Technos Double Dragon and Double Dragon II boards (MAME ddragon.cpp).
//!
//!   main  HD6309E @ 3 MHz; screen 6 MHz pixel clock, 384 x 272 => 192 main cycles a line, 57.4 Hz
//!   DD1:  sub (sprite CPU) HD63701Y0 @ 1.5 MHz, sound MC6809 @ 1.5 MHz (YM2151 + 2x MSM5205)
//!   DD2:  sub Z80 @ 4 MHz, sound Z80 @ 3.58 MHz (YM2151 + OKI M6295)
//!
//! Main CPU (DD2 differences in brackets):
//!   0000-0FFF RAM [0000-17FF]   1000-13FF palette [3C00-3FFF]   1800-1FFF FG RAM
//!   2000-27FF shared RAM (200 x4)   2800-2FFF sprite RAM   3000-37FF BG RAM
//!   3800 P1  3801 P2  3802 EXTRA  3803/4 DSW
//!   3808 W bits: 0 X scroll 8, 1 Y scroll 8, 2 /flip, 3 /sub reset, 4 sub halt, 5-7 ROM bank
//!   3809/380A W scroll X/Y   380B NMI ack   380C FIRQ ack   380D IRQ ack
//!   380E sound latch   380F sub CPU NMI   4000-7FFF ROM bank   8000-FFFF ROM
//! Interrupts: NMI when the vertical counter hits F8 (start of vblank), FIRQ on every
//! rising edge of counter bit 3 (every 16 lines), IRQ from the sub CPU.
//! The shared RAM is only visible to the main CPU while the sub CPU is halted or reset.
//!
//! DD1 sub CPU: on-chip RAM/ROM, 8000-81FF shared RAM; port 6 bit 0 = 0 clears its NMI,
//! bit 1 rising = main IRQ.
//! DD2 sub CPU: 0000-BFFF ROM, C000-C3FF shared RAM, D000 W clears its NMI, E000 W main IRQ.

use std::collections::HashMap;
use serde::{Deserialize, Serialize};

use crate::arcade::ddragon::dd2_sound::{Dd2Sound, SOUND_CLOCK as DD2_SOUND_CLOCK};
use crate::arcade::ddragon::dd_sound::DdSound;
use crate::arcade::ddragon::dd_video::{DdVideo, SCREEN_H, SCREEN_W};
use crate::arcade::hiscore::HiScoreMem;
use crate::arcade::{state, take_region};
use crate::common::cpu::hd6309::HD6309;
use crate::common::cpu::m6801::{M6801, M6801Bus, Model};
use crate::common::cpu::m6809::{M6809, M6809Bus};
use crate::common::cpu::z80::{Bus as Z80Bus, Z80Cpu, Z80State};
use crate::common::input as pad;
use crate::common::system::{DisplayInfo, Rotation, System};

const STATE_VERSION: u32 = 2;   // 2: Double Dragon II layout (sub / sound CPU kinds)
const LINES: usize = 272;
const PIXEL_CLOCK: f64 = 6_000_000.0;
const LINE_TIME: f64 = 384.0 / PIXEL_CLOCK;
const MAIN_PER_LINE: u32 = 192;
/// Each line runs in slices so the three CPUs stay close (MAME: 60 kHz quantum).
const SLICES: u32 = 8;
const MAIN_SLICE: u32 = MAIN_PER_LINE / SLICES;    // 24

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Variant { DoubleDragon, DoubleDragon2 }

/// MAME scanline_to_vcount: the counter runs 08-FF, then E8-FF.
fn vcount(line: usize) -> usize {
    let v = line + 8;
    if v < 0x100 { v } else { (v - 0x18) | 0x100 }
}

#[derive(Serialize, Deserialize)]
pub enum SoundBoard { Dd(DdSound), Dd2(Dd2Sound) }

impl SoundBoard {
    fn write_latch(&mut self, v: u8) {
        match self { SoundBoard::Dd(s) => s.write_latch(v), SoundBoard::Dd2(s) => s.write_latch(v) }
    }
    fn out(&mut self) -> &mut Vec<f32> {
        match self { SoundBoard::Dd(s) => &mut s.out, SoundBoard::Dd2(s) => &mut s.out }
    }
    fn reset(&mut self) {
        match self { SoundBoard::Dd(s) => s.reset(), SoundBoard::Dd2(s) => s.reset() }
    }
    fn keep_static(&mut self, old: &mut Self) {
        match (self, old) {
            (SoundBoard::Dd(a), SoundBoard::Dd(b)) => a.keep_static(b),
            (SoundBoard::Dd2(a), SoundBoard::Dd2(b)) => a.keep_static(b),
            _ => {}
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct DdBoard {
    #[serde(skip)] main_rom: Vec<u8>,
    #[serde(skip)] sub_rom: Vec<u8>,
    #[serde(skip)] dd2: bool,
    ram: Vec<u8>,
    comram: Vec<u8>,
    pub video: DdVideo,
    pub sound: SoundBoard,
    bank: u8,
    pub p1: u8, pub p2: u8, pub extra: u8,
    pub dsw: [u8; 2],
    vblank: bool,
    // interrupt lines
    main_nmi: bool,
    main_firq: bool,
    main_irq: bool,
    sub_nmi: bool,
    sub_port6: u8,
    sub_reset: bool,
    sub_halt: bool,
}

impl DdBoard {
    fn sub_suspended(&self) -> bool { self.sub_reset || self.sub_halt }

    fn extra(&self) -> u8 {
        // bits 0-2 active-low buttons, 3 vblank, 4 high while the sub CPU runs (BA inverted)
        (self.extra & 0x07) | ((self.vblank as u8) << 3) | ((!self.sub_suspended() as u8) << 4)
    }

    fn interrupt_ack(&mut self, offset: u16, data: u8) {
        match offset {
            0 => self.main_nmi = false,
            1 => self.main_firq = false,
            2 => self.main_irq = false,
            3 => self.sound.write_latch(data),
            _ => self.sub_nmi = true,
        }
    }

    fn bankswitch(&mut self, data: u8) {
        self.video.scrollx_hi = data & 1;
        self.video.scrolly_hi = (data >> 1) & 1;
        self.video.flip = data & 0x04 == 0;
        self.sub_reset = data & 0x08 == 0;
        self.sub_halt = data & 0x10 != 0;
        self.bank = data >> 5;
    }

    fn sub_port6(&mut self, data: u8) {
        if data & 1 == 0 { self.sub_nmi = false; }
        if self.sub_port6 & 2 == 0 && data & 2 != 0 { self.main_irq = true; }
        self.sub_port6 = data;
    }

    /// Palette RAM address -> (ext, index)
    fn palette_slot(&self, a: u16) -> Option<(bool, usize)> {
        let (lo, hi) = if self.dd2 { (0x3C00, 0x3E00) } else { (0x1000, 0x1200) };
        if (lo..lo + 0x200).contains(&a) { Some((false, (a - lo) as usize)) }
        else if (hi..hi + 0x200).contains(&a) { Some((true, (a - hi) as usize)) }
        else { None }
    }
}

impl M6809Bus for DdBoard {
    fn read(&mut self, a: u16) -> u8 {
        if let Some((ext, i)) = self.palette_slot(a) {
            return if ext { self.video.palette_ext[i] } else { self.video.palette[i] };
        }
        let v = &self.video;
        match a {
            0x0000..=0x0FFF => self.ram[a as usize],
            0x1000..=0x17FF if self.dd2 => self.ram[a as usize],
            0x1800..=0x1FFF => v.fgram[(a & 0x7FF) as usize],
            0x2000..=0x27FF => if self.sub_suspended() { self.comram[(a & 0x1FF) as usize] } else { 0xFF },
            0x2800..=0x2FFF => v.spriteram[(a & 0x7FF) as usize],
            0x3000..=0x37FF => v.bgram[(a & 0x7FF) as usize],
            0x3800 => self.p1,
            0x3801 => self.p2,
            0x3802 => self.extra(),
            0x3803 => self.dsw[0],
            0x3804 => self.dsw[1],
            0x380B..=0x380F => { self.interrupt_ack(a - 0x380B, 0xFF); 0xFF }
            0x4000..=0x7FFF => {
                let o = 0x10000 + self.bank as usize * 0x4000 + (a & 0x3FFF) as usize;
                self.main_rom.get(o).copied().unwrap_or(0xFF)
            }
            0x8000..=0xFFFF => self.main_rom.get(a as usize).copied().unwrap_or(0xFF),
            _ => 0xFF,
        }
    }

    fn write(&mut self, a: u16, d: u8) {
        if let Some((ext, i)) = self.palette_slot(a) {
            if ext { self.video.palette_ext[i] = d } else { self.video.palette[i] = d }
            return;
        }
        let v = &mut self.video;
        match a {
            0x0000..=0x0FFF => self.ram[a as usize] = d,
            0x1000..=0x17FF if self.dd2 => self.ram[a as usize] = d,
            0x1800..=0x1FFF => v.fgram[(a & 0x7FF) as usize] = d,
            0x2000..=0x27FF => if self.sub_suspended() { self.comram[(a & 0x1FF) as usize] = d },
            0x2800..=0x2FFF => v.spriteram[(a & 0x7FF) as usize] = d,
            0x3000..=0x37FF => v.bgram[(a & 0x7FF) as usize] = d,
            0x3808 => self.bankswitch(d),
            0x3809 => v.scrollx_lo = d,
            0x380A => v.scrolly_lo = d,
            0x380B..=0x380F => self.interrupt_ack(a - 0x380B, d),
            _ => {}
        }
    }
}

/// DD1 sub CPU (HD63701Y0) view of the board.
struct SubBus<'a>(&'a mut DdBoard);

impl M6801Bus for SubBus<'_> {
    fn read(&mut self, a: u16) -> u8 {
        match a {
            0x8000..=0x81FF => self.0.comram[(a & 0x1FF) as usize],
            0xC000..=0xFFFF => self.0.sub_rom.get((a - 0xC000) as usize).copied().unwrap_or(0xFF),
            _ => 0xFF,
        }
    }
    fn write(&mut self, a: u16, v: u8) {
        if let 0x8000..=0x81FF = a { self.0.comram[(a & 0x1FF) as usize] = v; }
    }
    fn port_write(&mut self, port: usize, data: u8, _ddr: u8) {
        if port == 5 { self.0.sub_port6(data); }
    }
}

/// DD2 sub CPU (Z80) view of the board.
struct Sub2Bus<'a>(&'a mut DdBoard);

impl Z80Bus for Sub2Bus<'_> {
    fn read(&mut self, a: u16) -> u8 {
        match a {
            0x0000..=0xBFFF => self.0.sub_rom.get(a as usize).copied().unwrap_or(0xFF),
            0xC000..=0xC3FF => self.0.comram[(a & 0x3FF) as usize],
            _ => 0xFF,
        }
    }
    fn write(&mut self, a: u16, v: u8) {
        match a {
            0xC000..=0xC3FF => self.0.comram[(a & 0x3FF) as usize] = v,
            0xD000 => self.0.sub_nmi = false,
            0xE000 => self.0.main_irq = true,
            _ => {}
        }
    }
    fn port_in(&mut self, _port: u16) -> u8 { 0xFF }
    fn port_out(&mut self, _port: u16, _val: u8) {}
}

pub enum SubCpu { Hd63701(M6801), Z80 { cpu: Z80Cpu, nmi_seen: bool } }
pub enum SoundCpu { M6809(M6809), Z80 { cpu: Z80Cpu, nmi_seen: bool } }

#[derive(Serialize, Deserialize)]
enum SubState { Hd63701(M6801), Z80(Z80State, bool) }
#[derive(Serialize, Deserialize)]
enum SoundState { M6809(M6809), Z80(Z80State, bool) }

fn z80_from(s: &Z80State) -> Z80Cpu { let mut c = Z80Cpu::new(); c.load_state(s); c }

pub struct DdSystem {
    variant: Variant,
    pub main: HD6309,
    pub sub: SubCpu,
    pub snd: SoundCpu,
    pub board: DdBoard,
    /// cycles each CPU ran past its last slice end
    debt: [i32; 3],
    /// fractional sound CPU cycles carried over (DD2: 3.58 MHz)
    snd_frac: f64,
    sub_was_reset: bool,
    current_frame: u64,
    save_filename: String,
    save_path: String,
}

#[derive(Serialize, Deserialize)]
struct DdState<B> {
    main: HD6309,
    sub: SubState,
    snd: SoundState,
    board: B,
    debt: [i32; 3],
    snd_frac: f64,
    sub_was_reset: bool,
    frame: u64,
}

impl DdSystem {
    pub fn new(variant: Variant, set_name: &str, mut regions: HashMap<String, Vec<u8>>) -> Result<Self, String> {
        let dd2 = variant == Variant::DoubleDragon2;
        let mut main_rom = take_region(&mut regions, "maincpu")?;
        main_rom.resize(0x30000, 0xFF);
        let sub_rom = take_region(&mut regions, "sub")?;
        let mut snd_rom = take_region(&mut regions, "soundcpu")?;
        snd_rom.resize(0x10000, 0xFF);
        let chars = take_region(&mut regions, "chars")?;
        let sprites = take_region(&mut regions, "sprites")?;
        let tiles = take_region(&mut regions, "tiles")?;
        let sound = if dd2 {
            SoundBoard::Dd2(Dd2Sound::new(snd_rom, take_region(&mut regions, "oki")?))
        } else {
            let adpcm1 = take_region(&mut regions, "adpcm1")?;
            let adpcm2 = take_region(&mut regions, "adpcm2")?;
            SoundBoard::Dd(DdSound::new(snd_rom, adpcm1, adpcm2))
        };
        let mut video = DdVideo::new(&chars, &tiles, &sprites);
        video.dd2 = dd2;
        let board = DdBoard {
            main_rom, sub_rom, dd2,
            ram: vec![0; 0x1800],
            comram: vec![0; 0x400],
            video, sound,
            bank: 0,
            p1: 0xFF, p2: 0xFF, extra: 0x07,
            // MAME defaults. DSW0: 1 coin 1 credit, upright, no flip.
            // DSW1 DD1: medium, demo sounds on, 30k and every 60k, 2 lives.
            // DSW1 DD2: hard, demo sounds on, easy hurricane kick, fast timer, 2 lives.
            dsw: [0xFF, if dd2 { 0x96 } else { 0xFF }],
            vblank: false,
            main_nmi: false, main_firq: false, main_irq: false,
            sub_nmi: false, sub_port6: 0, sub_reset: false, sub_halt: false,
        };
        let mut sys = Self {
            variant,
            main: HD6309::new(),
            sub: SubCpu::Hd63701(M6801::with_model(Model::Hd63701y0)),
            snd: SoundCpu::M6809(M6809::new()),
            board,
            debt: [0; 3],
            snd_frac: 0.0,
            sub_was_reset: false,
            current_frame: 0,
            save_filename: set_name.to_string(),
            save_path: "user://GD_EMU/Arcade/Technos/State".to_string(),
        };
        sys.reset();
        Ok(sys)
    }

    fn run_main(&mut self, target: u32) {
        let mut done = self.debt[0];
        while done < target as i32 {
            let b = &self.board;
            self.main.set_nmi(b.main_nmi);
            self.main.set_firq(b.main_firq);
            self.main.set_irq(b.main_irq);
            done += self.main.step(&mut self.board) as i32;
        }
        self.debt[0] = done - target as i32;
    }

    fn reset_sub(&mut self) {
        match &mut self.sub {
            SubCpu::Hd63701(c) => {
                *c = M6801::with_model(Model::Hd63701y0);
                c.reset(&mut SubBus(&mut self.board));
            }
            SubCpu::Z80 { cpu, nmi_seen } => { *cpu = Z80Cpu::new(); *nmi_seen = false; }
        }
    }

    fn run_sub(&mut self, target: u32) {
        // held in reset: reset when released (MAME resets on the line, then holds)
        if self.board.sub_reset {
            self.sub_was_reset = true;
            self.debt[1] = 0;
            return;
        }
        if self.sub_was_reset {
            self.sub_was_reset = false;
            self.reset_sub();
        }
        if self.board.sub_halt { self.debt[1] = 0; return; }
        let mut done = self.debt[1];
        while done < target as i32 && !self.board.sub_halt && !self.board.sub_reset {
            let line = self.board.sub_nmi;
            done += match &mut self.sub {
                SubCpu::Hd63701(c) => {
                    c.set_nmi(line);
                    c.step(&mut SubBus(&mut self.board))
                }
                SubCpu::Z80 { cpu, nmi_seen } => {
                    if !line { *nmi_seen = false; }
                    if line && !*nmi_seen {
                        *nmi_seen = true;
                        cpu.nmi(&mut Sub2Bus(&mut self.board))
                    } else {
                        cpu.step(&mut Sub2Bus(&mut self.board))
                    }
                }
            } as i32;
        }
        self.debt[1] = (done - target as i32).max(0);
    }

    fn run_sound(&mut self, target: u32) {
        let mut done = self.debt[2];
        while done < target as i32 {
            let c = match (&mut self.snd, &mut self.board.sound) {
                (SoundCpu::M6809(cpu), SoundBoard::Dd(s)) => {
                    cpu.set_irq(s.irq());
                    cpu.set_firq(s.firq());
                    let c = cpu.step(s);
                    s.advance(c);
                    c
                }
                (SoundCpu::Z80 { cpu, nmi_seen }, SoundBoard::Dd2(s)) => {
                    let line = s.nmi();
                    if !line { *nmi_seen = false; }
                    let mut c = 0;
                    if line && !*nmi_seen {
                        *nmi_seen = true;
                        c = cpu.nmi(s);
                    } else if s.irq() {
                        c = cpu.irq(s, 0xFF);
                    }
                    if c == 0 { c = cpu.step(s); }
                    s.advance(c);
                    c
                }
                _ => unreachable!("sound CPU and board mismatch"),
            };
            done += c as i32;
        }
        self.debt[2] = done - target as i32;
    }

    fn line_events(&mut self, line: usize) {
        let old = vcount(if line == 0 { LINES - 1 } else { line - 1 });
        let new = vcount(line);
        if new == 0xF8 { self.board.main_nmi = true; }
        if old & 8 == 0 && new & 8 != 0 { self.board.main_firq = true; }
        self.board.vblank = line >= SCREEN_H;
    }

    /// Sub and sound CPU cycles for one slice.
    fn slice_targets(&mut self) -> (u32, u32) {
        match self.variant {
            Variant::DoubleDragon => (MAIN_SLICE / 2, MAIN_SLICE / 2),   // both 1.5 MHz
            Variant::DoubleDragon2 => {
                self.snd_frac += DD2_SOUND_CLOCK * LINE_TIME / SLICES as f64;
                let snd = self.snd_frac.floor();
                self.snd_frac -= snd;
                (MAIN_SLICE * 4 / 3, snd as u32)                          // 4 MHz, 3.58 MHz
            }
        }
    }
}

impl System for DdSystem {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        for line in 0..LINES {
            self.line_events(line);
            for _ in 0..SLICES {
                let (sub, snd) = self.slice_targets();
                self.run_main(MAIN_SLICE);
                self.run_sub(sub);
                self.run_sound(snd);
            }
            if line < SCREEN_H { self.board.video.render_line(line); }
        }
        self.current_frame += 1;
    }

    /// P1: 0x01 right, 0x02 left, 0x04 up, 0x08 down, 0x10 button 1 (B), 0x20 button 2 (A),
    /// 0x40 start 1, 0x80 start 2. P2: same, 0x40 coin 1, 0x80 coin 2.
    /// EXTRA: 0x02 P1 button 3 (X), 0x04 P2 button 3. All active low.
    fn set_input(&mut self, mask: u32) {
        let on = |m: u32, b: u32| m & b != 0;
        let joy = |m: u32| -> u8 {
            let mut v = 0xFF;
            if on(m, pad::RIGHT) { v &= !0x01; }
            if on(m, pad::LEFT) { v &= !0x02; }
            if on(m, pad::UP) { v &= !0x04; }
            if on(m, pad::DOWN) { v &= !0x08; }
            if on(m, pad::B) { v &= !0x10; }
            if on(m, pad::A) { v &= !0x20; }
            v
        };
        let mut p1 = joy(mask);
        if on(mask, pad::START) { p1 &= !0x40; }
        if on(mask, pad::START2) { p1 &= !0x80; }
        let mut p2 = joy(pad::player2(mask));
        if on(mask, pad::COIN) || on(mask, pad::SELECT) { p2 &= !0x40; }
        let mut extra = 0x07;
        if on(mask, pad::X) { extra &= !0x02; }
        if on(mask, pad::P2_X) { extra &= !0x04; }
        self.board.p1 = p1;
        self.board.p2 = p2;
        self.board.extra = extra;
    }

    fn drain_audio(&mut self) -> Vec<f32> { std::mem::take(self.board.sound.out()) }
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
    fn framebuffer(&self) -> &[u8] { &self.board.video.framebuffer }

    fn reset(&mut self) {
        let b = &mut self.board;
        b.video.scrollx_hi = 0;
        b.video.scrolly_hi = 0;
        b.bank = 0;
        b.main_nmi = false; b.main_firq = false; b.main_irq = false;
        b.sub_nmi = false; b.sub_port6 = 0; b.sub_reset = false; b.sub_halt = false;
        b.sound.reset();
        self.main = HD6309::new();
        self.main.reset(&mut self.board);
        match self.variant {
            Variant::DoubleDragon => {
                self.sub = SubCpu::Hd63701(M6801::with_model(Model::Hd63701y0));
                let mut snd = M6809::new();
                if let SoundBoard::Dd(s) = &mut self.board.sound { snd.reset(s); }
                self.snd = SoundCpu::M6809(snd);
            }
            Variant::DoubleDragon2 => {
                self.sub = SubCpu::Z80 { cpu: Z80Cpu::new(), nmi_seen: false };
                self.snd = SoundCpu::Z80 { cpu: Z80Cpu::new(), nmi_seen: false };
            }
        }
        self.reset_sub();
        self.debt = [0; 3];
        self.snd_frac = 0.0;
        self.sub_was_reset = false;
    }

    /// "dsw0": coinage, cabinet, flip. "dsw1": difficulty, demo sounds, bonus / timer, lives.
    fn set_option(&mut self, key: &str, value: i64) {
        match key {
            "dsw0" | "dsw1" => self.board.dsw[(key == "dsw1") as usize] = value as u8,
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
            main: &'a HD6309, sub: SubState, snd: SoundState, board: &'a DdBoard,
            debt: [i32; 3], snd_frac: f64, sub_was_reset: bool, frame: u64,
        }
        let sub = match &self.sub {
            SubCpu::Hd63701(c) => SubState::Hd63701(c.clone()),
            SubCpu::Z80 { cpu, nmi_seen } => SubState::Z80(cpu.save_state(), *nmi_seen),
        };
        let snd = match &self.snd {
            SoundCpu::M6809(c) => SoundState::M6809(c.clone()),
            SoundCpu::Z80 { cpu, nmi_seen } => SoundState::Z80(cpu.save_state(), *nmi_seen),
        };
        state::encode(&self.save_filename, STATE_VERSION, &Ref {
            main: &self.main, sub, snd, board: &self.board,
            debt: self.debt, snd_frac: self.snd_frac, sub_was_reset: self.sub_was_reset,
            frame: self.current_frame,
        })
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let s: DdState<DdBoard> = state::decode(&self.save_filename, STATE_VERSION, data)?;
        let mut board = s.board;
        board.main_rom = std::mem::take(&mut self.board.main_rom);
        board.sub_rom = std::mem::take(&mut self.board.sub_rom);
        board.dd2 = self.board.dd2;
        board.video.keep_static(&mut self.board.video);
        board.sound.keep_static(&mut self.board.sound);
        self.board = board;
        self.main = s.main;
        self.sub = match s.sub {
            SubState::Hd63701(c) => SubCpu::Hd63701(c),
            SubState::Z80(z, seen) => SubCpu::Z80 { cpu: z80_from(&z), nmi_seen: seen },
        };
        self.snd = match s.snd {
            SoundState::M6809(c) => SoundCpu::M6809(c),
            SoundState::Z80(z, seen) => SoundCpu::Z80 { cpu: z80_from(&z), nmi_seen: seen },
        };
        self.debt = s.debt;
        self.snd_frac = s.snd_frac;
        self.sub_was_reset = s.sub_was_reset;
        self.current_frame = s.frame;
        Ok(())
    }
}

impl HiScoreMem for DdSystem {
    fn hs_read(&mut self, addr: u16) -> u8 { self.board.read(addr) }
    fn hs_write(&mut self, addr: u16, v: u8) { self.board.write(addr, v) }
}
