//! Konami The Simpsons (GX072) board (MAME simpsons.cpp, BSD-3-Clause).
//!
//!   main  Konami 053248 CPU, 12 MHz in / 3 MHz bus (one clock per bus access)
//!   video K052109 + K051962 tilemaps, K053246 + K053247 sprites, K053251 mixer
//!   sound Z80 @ 3.58 MHz + YM2151 + K053260 (simpsons_sound.rs)
//!   ER5911 serial EEPROM: settings and high scores (saved as the battery file)
//!
//! Main CPU map:
//!   0000-0FFF K052109 / palette RAM (when bank bit 0 is set)   1000-1FFF K052109
//!   1F80 R coins   1F81 R test switch, EEPROM DO (bit 4) and READY (bit 5)
//!   1F90-1F93 R players 1-4   1FA0-1FA7 W K053246   1FB0-1FBF W K053251
//!   1FC0 W coin counters, bit 3 = tile ROM readback, bit 5 = /sprite ROM readback
//!   1FC2 W EEPROM (bit 3 CS, 4 CLK, 7 DI), bits 0-1 video banks, bit 2 FIRQ enable
//!   1FC4 R sound CPU IRQ   1FC6-1FC7 K053260 ports   1FC8-1FC9 R K053246 ROM readback
//!   1FCA R watchdog   2000-3FFF K052109, or sprite RAM 2000-2FFF + RAM 3000-3FFF
//!   4000-5FFF RAM   6000-7FFF ROM bank (SETLINES & 3F)   8000-FFFF ROM (last 32K)
//! Interrupts: IRQ from the K052109 at the start of vblank. At the same moment, if the
//! K053246 has object DMA on, sprite RAM is copied to the K053247 and FIRQ runs from
//! 256 to 2304 6 MHz ticks later (if enabled at 1FC2).

use std::collections::HashMap;
use serde::{Deserialize, Serialize};

use crate::arcade::simpsons::simpsons_video::{interleave_words, Video, FIRST_LINE, SCREEN_H, SCREEN_W};
use crate::arcade::{state, take_region};
use crate::arcade::simpsons::simpsons_sound::{SimpsonsSound, SOUND_CLOCK};
use crate::common::cpu::konami::{Konami, KonamiBus};
use crate::common::cpu::z80::{Z80Cpu, Z80State};
use crate::common::input as pad;
use crate::common::io::eeprom_serial::EepromSerial;
use crate::common::system::{DisplayInfo, Rotation, System};

const STATE_VERSION: u32 = 3;   // 2: sound board, 3: NMI edge latch, player slots
/// Main and sound CPUs take turns in slices of a line
const SLICES: u64 = 4;
pub const LINES: u64 = 264;
pub const CYCLES_PER_LINE: u64 = 192;     // 384 dots at 6 MHz, CPU at 3 MHz
const VBLANK_START: u64 = 240;
const VBLANK_END: u64 = FIRST_LINE as u64;
const FIRQ_ON: u64 = 128;                 // 256 ticks of 6 MHz
const FIRQ_OFF: u64 = 1152;               // 2304 ticks
const CPU_HZ: u64 = 3_000_000;

#[derive(Serialize, Deserialize)]
pub struct Board {
    #[serde(skip)] rom: Vec<u8>,
    #[serde(with = "serde_bytes")]
    ram: Vec<u8>,          // 4000-5FFF
    #[serde(with = "serde_bytes")]
    ram2: Vec<u8>,         // 3000-3FFF (video view 1)
    pub video: Video,
    pub eeprom: EepromSerial,
    bank: u8,
    palette_view: bool,
    sprite_view: bool,
    firq_enabled: bool,
    pub irq: bool,
    pub firq: bool,
    pub sound: SimpsonsSound,
    pub inputs: [u8; 4],
    pub coins: u8,
    pub service: bool,
    pub two_player: bool,
    /// the player slot (character) pad 1 controls; R cycles it
    pub slot: u8,
    r_prev: bool,
    /// main CPU clocks since power on
    pub cycle: u64,
}

impl Board {
    fn test_port(&self) -> u8 {
        0xCE | (!self.service as u8)
            | (self.eeprom.do_read(self.cycle) as u8) << 4
            | (self.eeprom.ready(self.cycle) as u8) << 5
    }

    fn eeprom_w(&mut self, d: u8) {
        if d == 0xFF { return; }
        let now = self.cycle;
        // MAME writes the port's fields in order: CS, CLK, then DI
        self.eeprom.cs_write(d & 0x08 != 0, now);
        self.eeprom.clk_write(d & 0x10 != 0, now);
        self.eeprom.di_write(d & 0x80 != 0);
        self.palette_view = d & 1 != 0;
        self.sprite_view = d & 2 != 0;
        self.firq_enabled = d & 4 != 0;
        if !self.firq_enabled { self.firq = false; }
    }

    fn coin_counter_w(&mut self, d: u8) {
        self.video.rmrd = d & 0x08 != 0;
        self.video.objcha = d & 0x20 == 0;
    }

    fn k052109_w(&mut self, off: u16, d: u8) {
        let old = self.video.irq_control;
        self.video.k052109_write(off, d);
        if off & 0x3FFF == 0x1D00 && !d & old & 0x04 != 0 { self.irq = false; }
    }

    /// Sprite RAM to the K053247: active sprites (bit 15 and a Z code) packed to the
    /// front, the rest cleared.
    pub fn object_dma(&mut self) {
        let v = &mut self.video;
        let mut dst = 0;
        let mut inactive = 256;
        let mut out = vec![0u16; 0x800];
        for src in (0..0x800).step_by(8) {
            let w = v.sprite_ram_cpu[src];
            if w & 0x8000 != 0 && w & 0xFF != 0 {
                out[dst..dst + 8].copy_from_slice(&v.sprite_ram_cpu[src..src + 8]);
                dst += 8;
                inactive -= 1;
            }
        }
        let _ = inactive;   // the rest are already zero
        v.sprite_ram = out;
    }
}

impl KonamiBus for Board {
    fn read(&mut self, a: u16) -> u8 {
        match a {
            0x0000..=0x0FFF if self.palette_view => self.video.palette[a as usize],
            0x1F80 => self.coins,
            0x1F81 => self.test_port(),
            0x1F90..=0x1F93 => {
                let p = (a - 0x1F90) as usize;
                if self.two_player && p >= 2 { 0xFF } else { self.inputs[p] }
            }
            0x1FC4 => { self.sound.irq = true; 0 }
            0x1FC6 | 0x1FC7 => self.sound.k053260.ports[2 + (a as usize & 1)],
            0x1FC8 | 0x1FC9 => self.video.k053246_read(a),
            0x1FCA => 0,   // watchdog
            0x0000..=0x1FFF => self.video.k052109_read(a),
            0x2000..=0x2FFF if self.sprite_view => {
                let w = self.video.sprite_ram_cpu[(a as usize & 0xFFF) >> 1];
                if a & 1 != 0 { w as u8 } else { (w >> 8) as u8 }
            }
            0x3000..=0x3FFF if self.sprite_view => self.ram2[a as usize & 0xFFF],
            0x2000..=0x3FFF => self.video.k052109_read(a),
            0x4000..=0x5FFF => self.ram[a as usize - 0x4000],
            0x6000..=0x7FFF => {
                let o = self.bank as usize * 0x2000 + (a as usize - 0x6000);
                self.rom.get(o).copied().unwrap_or(0xFF)
            }
            _ => self.rom.get(0x78000 + (a as usize - 0x8000)).copied().unwrap_or(0xFF),
        }
    }

    fn write(&mut self, a: u16, d: u8) {
        match a {
            0x0000..=0x0FFF if self.palette_view => self.video.palette[a as usize] = d,
            0x1FA0..=0x1FA7 => self.video.k053246_write(a - 0x1FA0, d),
            0x1FB0..=0x1FBF => self.video.k053251_write(a - 0x1FB0, d),
            0x1FC0 => self.coin_counter_w(d),
            0x1FC2 => self.eeprom_w(d),
            0x1FC6 | 0x1FC7 => self.sound.k053260.ports[a as usize & 1] = d,
            0x0000..=0x1FFF => self.k052109_w(a, d),
            0x2000..=0x2FFF if self.sprite_view => {
                let w = &mut self.video.sprite_ram_cpu[(a as usize & 0xFFF) >> 1];
                *w = if a & 1 != 0 { (*w & 0xFF00) | d as u16 } else { (*w & 0x00FF) | (d as u16) << 8 };
            }
            0x3000..=0x3FFF if self.sprite_view => self.ram2[a as usize & 0xFFF] = d,
            0x2000..=0x3FFF => self.k052109_w(a, d),
            0x4000..=0x5FFF => self.ram[a as usize - 0x4000] = d,
            _ => {}
        }
    }

    fn set_lines(&mut self, v: u8) { self.bank = v & 0x3F; }
}

pub struct SimpsonsSystem {
    pub cpu: Konami,
    pub z80: Z80Cpu,
    /// Z80 clocks owed (fractional, can go negative when an instruction runs over)
    z80_owed: f64,
    pub board: Board,
    /// pending FIRQ on / off times (absolute clocks)
    firq_on_at: Option<u64>,
    firq_off_at: Option<u64>,
    current_frame: u64,
    save_filename: String,
    save_path: String,
    state_path: String,
}

#[derive(Serialize, Deserialize)]
struct SimpsonsState<B> {
    cpu: Konami,
    z80: Z80State,
    z80_owed: f64,
    board: B,
    firq_on_at: Option<u64>,
    firq_off_at: Option<u64>,
    frame: u64,
}

impl SimpsonsSystem {
    /// Regions: "maincpu" (0x80000), "k052109_0/1" (the two tile ROMs, word-interleaved
    /// here), "k053246_0..3" (the four sprite ROMs), optional "eeprom" (MAME's default
    /// .nv contents). `two_player`: the 2-player sets (P3 / P4 unused).
    pub fn new(set_name: &str, mut regions: HashMap<String, Vec<u8>>, two_player: bool) -> Result<Self, String> {
        let mut rom = take_region(&mut regions, "maincpu")?;
        rom.resize(0x80000, 0xFF);
        let tiles = interleave_words(&[take_region(&mut regions, "k052109_0")?,
                                       take_region(&mut regions, "k052109_1")?]);
        let mut sprites = Vec::new();
        for i in 0..4 { sprites.push(take_region(&mut regions, &format!("k053246_{i}"))?); }
        let sprites = interleave_words(&sprites);
        let mut snd_rom = take_region(&mut regions, "audiocpu")?;
        snd_rom.resize(0x28000, 0xFF);
        let sound = SimpsonsSound::new(snd_rom, regions.remove("k053260").unwrap_or_default());
        let mut eeprom = EepromSerial::er5911_8bit(CPU_HZ / 1000);
        if let Some(d) = regions.remove("eeprom") { eeprom.load(&d); }
        eeprom.dirty = false;
        let board = Board {
            rom,
            ram: vec![0; 0x2000],
            ram2: vec![0; 0x1000],
            video: Video::new(tiles, sprites),
            eeprom,
            bank: 0, palette_view: false, sprite_view: false, firq_enabled: false,
            irq: false, firq: false,
            sound,
            inputs: [0xFF; 4], coins: 0xFF, service: false, two_player, slot: 0, r_prev: false,
            cycle: 0,
        };
        let mut sys = Self {
            cpu: Konami::new(),
            z80: Z80Cpu::new(),
            z80_owed: 0.0,
            board,
            firq_on_at: None,
            firq_off_at: None,
            current_frame: 0,
            save_filename: set_name.to_string(),
            save_path: "user://GD_EMU/Arcade/Konami/Save".to_string(),
            state_path: "user://GD_EMU/Arcade/Konami/State".to_string(),
        };
        sys.reset();
        Ok(sys)
    }

    /// Runs the main CPU to absolute clock `until`, firing the FIRQ timers on the way.
    fn run_main(&mut self, until: u64) {
        while self.board.cycle < until {
            if let Some(t) = self.firq_on_at {
                if self.board.cycle >= t {
                    self.firq_on_at = None;
                    if self.board.firq_enabled { self.board.firq = true; }
                }
            }
            if let Some(t) = self.firq_off_at {
                if self.board.cycle >= t {
                    self.firq_off_at = None;
                    self.board.firq = false;
                }
            }
            self.cpu.set_irq(self.board.irq);
            self.cpu.set_firq(self.board.firq);
            let c = self.cpu.step(&mut self.board) as u64;
            self.board.cycle += c.max(1);
        }
    }

    /// Runs the sound Z80 for `clocks` more clocks.
    fn run_z80(&mut self, clocks: f64) {
        self.z80_owed += clocks;
        let s = &mut self.board.sound;
        while self.z80_owed >= 1.0 {
            let mut c = 0;
            if s.nmi_pending {
                s.nmi_pending = false;
                c = self.z80.nmi(s);
            } else if s.irq {
                c = self.z80.irq(s, 0xFF);
                if c != 0 { s.irq = false; }   // HOLD_LINE: dropped once taken
            }
            if c == 0 { c = self.z80.step(s); }
            s.advance(c);
            self.z80_owed -= c as f64;
        }
    }

    fn vblank_start(&mut self) {
        if self.board.video.irq_enabled() { self.board.irq = true; }
        if self.board.video.obj_dma_enabled() {
            self.board.object_dma();
            self.firq_on_at = Some(self.board.cycle + FIRQ_ON);
            self.firq_off_at = Some(self.board.cycle + FIRQ_OFF);
        }
    }
}

impl System for SimpsonsSystem {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        let start = self.board.cycle;
        for line in 0..LINES {
            if line == VBLANK_START { self.vblank_start(); }
            if line == VBLANK_END { self.board.video.render(); }
            for slice in 1..=SLICES {
                self.run_main(start + line * CYCLES_PER_LINE + slice * CYCLES_PER_LINE / SLICES);
                self.run_z80((CYCLES_PER_LINE / SLICES) as f64 * SOUND_CLOCK / CPU_HZ as f64);
            }
        }
        self.current_frame += 1;
    }

    /// Pad 1 plays character slot `slot` (4-player sets: 0 Marge, 1 Homer, 2 Bart,
    /// 3 Lisa; each has its own coin slot and start), pad 2 the next one. R cycles pad 1's
    /// slot. 01 left, 02 right, 04 up, 08 down, 10 attack (B), 20 jump (A), 80 start.
    /// Coin = the pad's coin slot (pad 2: P2 Select). L = test switch (service menu).
    fn set_input(&mut self, mask: u32) {
        let on = |m: u32, b: u32| m & b != 0;
        let joy = |m: u32| -> u8 {
            let mut v = 0xFF;
            if on(m, pad::LEFT) { v &= !0x01; }
            if on(m, pad::RIGHT) { v &= !0x02; }
            if on(m, pad::UP) { v &= !0x04; }
            if on(m, pad::DOWN) { v &= !0x08; }
            if on(m, pad::B) { v &= !0x10; }
            if on(m, pad::A) { v &= !0x20; }
            if on(m, pad::START) { v &= !0x80; }
            v
        };
        let p2 = pad::player2(mask);
        let b = &mut self.board;
        let slots = if b.two_player { 2 } else { 4 };
        let r = on(mask, pad::R);
        if r && !b.r_prev { b.slot = (b.slot + 1) % slots; }
        b.r_prev = r;
        let s1 = b.slot as usize % slots as usize;
        let s2 = (s1 + 1) % slots as usize;
        b.inputs = [0xFF; 4];
        b.inputs[s1] = joy(mask);
        b.inputs[s2] = joy(p2);
        if on(mask, pad::START2) { b.inputs[s2] &= !0x80; }
        let mut coins = 0xFF;
        if on(mask, pad::COIN) || on(mask, pad::SELECT) { coins &= !(1 << s1); }
        if on(p2, pad::SELECT) { coins &= !(1 << s2); }
        b.coins = coins;
        b.service = on(mask, pad::L);
    }

    fn drain_audio(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.board.sound.out)
    }
    fn sample_rate(&self) -> u32 { 44100 }
    fn power_on(&mut self) {}
    fn power_off(&mut self) {}
    fn has_battery(&self) -> bool { true }
    fn save_filename(&self) -> &str { &self.save_filename }
    fn save_battery_path(&self) -> &str { &self.save_path }
    fn save_state_path(&self) -> &str { &self.state_path }
    fn is_sram_dirty(&self) -> bool { self.board.eeprom.dirty }
    fn clear_sram_dirty(&mut self) { self.board.eeprom.dirty = false; }
    fn get_current_frame(&self) -> i64 { self.current_frame as i64 }
    fn get_sram(&self) -> Option<&[u8]> { Some(&self.board.eeprom.data) }
    fn load_sram(&mut self, data: &[u8]) {
        if data.len() == self.board.eeprom.data.len() {
            self.board.eeprom.data.copy_from_slice(data);
        }
    }
    fn framebuffer(&self) -> &[u8] { &self.board.video.framebuffer }

    fn reset(&mut self) {
        let b = &mut self.board;
        b.bank = 0;
        b.palette_view = false;
        b.sprite_view = false;
        b.firq_enabled = false;
        b.irq = false;
        b.firq = false;
        b.sound.reset();
        b.video.reset();
        self.firq_on_at = None;
        self.firq_off_at = None;
        self.cpu = Konami::new();
        self.cpu.reset(&mut self.board);
        self.z80 = Z80Cpu::new();
        self.z80_owed = 0.0;
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
            cpu: &'a Konami, z80: Z80State, z80_owed: f64, board: &'a Board,
            firq_on_at: Option<u64>, firq_off_at: Option<u64>, frame: u64,
        }
        state::encode(&self.save_filename, STATE_VERSION, &Ref {
            cpu: &self.cpu, z80: self.z80.save_state(),
            z80_owed: self.z80_owed, board: &self.board,
            firq_on_at: self.firq_on_at, firq_off_at: self.firq_off_at,
            frame: self.current_frame,
        })
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let s: SimpsonsState<Board> = state::decode(&self.save_filename, STATE_VERSION, data)?;
        let mut board = s.board;
        board.rom = std::mem::take(&mut self.board.rom);
        board.video.keep_static(&mut self.board.video);
        board.sound.keep_static(&mut self.board.sound);
        board.two_player = self.board.two_player;
        self.board = board;
        self.cpu = s.cpu;
        self.z80 = Z80Cpu::new();
        self.z80.load_state(&s.z80);
        self.z80_owed = s.z80_owed;
        self.firq_on_at = s.firq_on_at;
        self.firq_off_at = s.firq_off_at;
        self.current_frame = s.frame;
        Ok(())
    }
}
