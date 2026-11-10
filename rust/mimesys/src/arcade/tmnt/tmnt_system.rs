//! Konami Teenage Mutant Ninja Turtles (GX963). After MAME tmnt.cpp (BSD-3-Clause).
//!
//! Main board: a 68000 at 8 MHz, IRQ 5 at vblank (a level the game clears through
//! 0A0000 bit 5). Video is K052109 + K051962 tilemaps and a K051960 sprite chip (see
//! tmnt_video.rs); the sound board (Z80, YM2151, K007232, uPD7759) is tmnt_sound.rs.
//!
//! Main map: 000000-05FFFF ROM; 060000-063FFF RAM; 080000-080FFF palette (low bytes);
//! 0A0000 R coins, W control (bits 0-1 coin counters, bit 3 high -> low = Z80 IRQ, bit 5
//! = IRQ 5 enable, bit 7 = character ROM readback); 0A0002 / 0A0004 / 0A0006 / 0A0014 R
//! players 1-4; 0A0009 W sound latch; 0A0010 / 0A0012 / 0A0018 R DIP switches (0A0010 W
//! watchdog); 0C0000 W sprite / layer priority; 100000-107FFF K052109 (A12 not
//! connected); 140000-140007 K051937; 140400-1407FF K051960 RAM.

use std::collections::HashMap;
use serde::{Deserialize, Serialize};

use crate::arcade::tmnt::tmnt_sound::{Sound, SoundState};
use crate::arcade::tmnt::tmnt_video::{interleave_words, Video, SCREEN_H, SCREEN_W};
use crate::arcade::{state, take_region};
use crate::common::cpu::m68000::{M68000, M68kBus};
use crate::common::input as pad;
use crate::common::system::{DisplayInfo, Rotation, System};

const STATE_VERSION: u32 = 1;
const MAIN_CLOCK: u64 = 8_000_000;
/// 384 dots at 6 MHz per line
pub const LINES: u32 = 264;
pub const LINE_RATE: u64 = 15_625;
/// first line of vblank (lines 16-239 are shown) and the line the frame is drawn on
const VBLANK_START: u32 = 240;
const VBLANK_END: u32 = 16;

/// The sprite ROM address lines are wired through the 963A30 PROM (MAME init_tmnt, from the
/// schematics): for each 32-bit ROM word A, the PROM entry for A's top bits picks the sprite
/// size, and that size decides which line address bit feeds each of the low 10 ROM address
/// bits. Returns the ROM as the K051960 sees it.
pub fn unscramble_sprites(rom: &[u8], prom: &[u8]) -> Vec<u8> {
    // for each of the low 10 address bits, which bit to take it from, per size entry 0-7
    const PICK: [[u8; 8]; 10] = [
        [3, 3, 3, 3, 3, 3, 3, 3],
        [0, 0, 5, 5, 5, 5, 5, 5],
        [1, 1, 0, 0, 0, 7, 7, 7],
        [2, 2, 1, 1, 1, 0, 0, 9],
        [4, 4, 2, 2, 2, 1, 1, 0],
        [5, 6, 4, 4, 4, 2, 2, 1],
        [6, 5, 6, 6, 6, 4, 4, 2],
        [7, 7, 7, 7, 8, 6, 6, 4],
        [8, 8, 8, 8, 7, 8, 8, 6],
        [9, 9, 9, 9, 9, 9, 9, 8],
    ];
    let words = rom.len() / 4;
    let mut out = vec![0u8; rom.len()];
    for a in 0..words {
        let entry = (prom.get((a & 0x7F800) >> 11).copied().unwrap_or(0) & 7) as usize;
        let mut b = a & 0x7FC00;
        for (i, pick) in PICK.iter().enumerate() {
            b |= ((a >> pick[entry]) & 1) << i;
        }
        if b < words { out[a * 4..a * 4 + 4].copy_from_slice(&rom[b * 4..b * 4 + 4]); }
    }
    out
}

pub struct TmntBus {
    rom: Vec<u8>,
    ram: Vec<u8>,
    pub video: Video,
    pub sound: Sound,
    pub irq5: bool,
    irq5_mask: bool,
    /// bit 3 of the last 0A0000 write (the Z80 IRQ fires on its falling edge)
    last_a0000: u8,
    /// COINS, P1-P4 (active low), DSW1-3
    pub coins: u8,
    pub players: [u8; 4],
    pub dsw: [u8; 3],
    pub two_player: bool,
    /// the player slot (character) pad 1 controls; R cycles it
    pub slot: u8,
    r_prev: bool,
}

impl TmntBus {
    /// 16-bit read of the board's input ports (the value sits in the low byte).
    fn port(&self, a: u32) -> u16 {
        (match a & !1 {
            0x0A0000 => self.coins,
            0x0A0002 => self.players[0],
            0x0A0004 => self.players[1],
            0x0A0006 => self.players[2],
            0x0A0014 => self.players[3],
            0x0A0010 => self.dsw[0],
            0x0A0012 => self.dsw[1],
            0x0A0018 => self.dsw[2],
            _ => 0,
        }) as u16
    }

    /// Write to 0A0000 as the 16-bit handler sees it.
    fn control_w(&mut self, data: u16) {
        // bit 3 high then low triggers the sound CPU's IRQ
        if self.last_a0000 == 0x08 && data & 0x08 == 0 { self.sound.irq = true; }
        self.last_a0000 = (data & 0x08) as u8;
        // bit 5: IRQ 5 enable
        self.irq5_mask = data & 0x20 != 0;
        if !self.irq5_mask { self.irq5 = false; }
        // bit 7: character ROM readback through the video RAM
        self.video.rmrd = data & 0x80 != 0;
    }

    fn read16(&mut self, a: u32) -> u16 {
        match a {
            0x000000..=0x05FFFF => (self.rom[a as usize] as u16) << 8 | self.rom[a as usize + 1] as u16,
            0x060000..=0x063FFF => {
                let i = (a - 0x060000) as usize;
                (self.ram[i] as u16) << 8 | self.ram[i + 1] as u16
            }
            0x080000..=0x080FFF => self.video.palette[((a - 0x080000) >> 1) as usize] as u16,
            0x0A0000..=0x0A001F => self.port(a),
            0x100000..=0x107FFF => self.video.k052109_word_read(a),
            0x140000..=0x140007 => {
                let o = (a & 7) as usize;
                (self.video.k051937_read(o) as u16) << 8 | self.video.k051937_read(o + 1) as u16
            }
            0x140400..=0x1407FF => {
                let o = (a - 0x140400) as usize;
                (self.video.k051960_read(o) as u16) << 8 | self.video.k051960_read(o + 1) as u16
            }
            _ => 0,
        }
    }

    fn write16(&mut self, a: u32, v: u16) {
        match a {
            0x060000..=0x063FFF => {
                let i = (a - 0x060000) as usize;
                self.ram[i] = (v >> 8) as u8;
                self.ram[i + 1] = v as u8;
            }
            0x080000..=0x080FFF => self.video.palette[((a - 0x080000) >> 1) as usize] = v as u8,
            0x0A0000..=0x0A0001 => self.control_w(v),
            0x0A0008..=0x0A0009 => self.sound.bus.latch = v as u8,
            0x0C0000..=0x0C0001 => self.video.priority = ((v & 0x0C) >> 2) as u8,
            0x100000..=0x107FFF => self.video.k052109_word_write(a, v),
            0x140000..=0x140007 => {
                let o = (a & 7) as usize;
                self.video.k051937_write(o, (v >> 8) as u8);
                self.video.k051937_write(o + 1, v as u8);
            }
            0x140400..=0x1407FF => {
                let o = (a - 0x140400) as usize;
                self.video.k051960_write(o, (v >> 8) as u8);
                self.video.k051960_write(o + 1, v as u8);
            }
            _ => {}   // 0A0010 watchdog
        }
    }

    #[cfg(test)]
    pub fn ram_word(&self, a: u32) -> u16 { let i = (a - 0x060000) as usize; (self.ram[i] as u16) << 8 | self.ram[i + 1] as u16 }
}

impl M68kBus for TmntBus {
    fn read_byte(&mut self, a: u32) -> u8 {
        match a {
            0x000000..=0x05FFFF => self.rom[a as usize],
            0x060000..=0x063FFF => self.ram[(a - 0x060000) as usize],
            // 8-bit devices on the byte lanes: one byte, one register
            0x100000..=0x107FFF => self.video.k052109_byte_read(a),
            0x140000..=0x140007 => self.video.k051937_read((a & 7) as usize),
            0x140400..=0x1407FF => self.video.k051960_read((a - 0x140400) as usize),
            // 16-bit ports and the palette's low-byte lane
            0x080000..=0x080FFF if a & 1 == 0 => 0,
            _ => { let w = self.read16(a & !1); if a & 1 == 0 { (w >> 8) as u8 } else { w as u8 } }
        }
    }

    fn read_word(&mut self, a: u32) -> u16 { self.read16(a) }

    fn write_byte(&mut self, a: u32, v: u8) {
        match a {
            0x060000..=0x063FFF => self.ram[(a - 0x060000) as usize] = v,
            0x080000..=0x080FFF => if a & 1 == 1 { self.video.palette[((a - 0x080000) >> 1) as usize] = v },
            0x0A0000..=0x0A0001 => self.control_w(if a & 1 == 0 { (v as u16) << 8 } else { v as u16 }),
            0x0A0009 => self.sound.bus.latch = v,
            0x0C0000..=0x0C0001 => self.video.priority = (((if a & 1 == 0 { 0 } else { v }) & 0x0C) >> 2) as u8,
            0x100000..=0x107FFF => self.video.k052109_byte_write(a, v),
            0x140000..=0x140007 => self.video.k051937_write((a & 7) as usize, v),
            0x140400..=0x1407FF => self.video.k051960_write((a - 0x140400) as usize, v),
            _ => {}
        }
    }

    fn write_word(&mut self, a: u32, v: u16) { self.write16(a, v) }
}

#[derive(Serialize)]
struct MainStateRef<'a> {
    ram: &'a [u8], video: &'a Video, irq5: bool, irq5_mask: bool, last: u8, slot: u8,
}

#[derive(Deserialize)]
struct MainState { ram: Vec<u8>, video: Video, irq5: bool, irq5_mask: bool, last: u8, slot: u8 }

pub struct TmntSystem {
    cpu: M68000,
    pub bus: TmntBus,
    main_phase: u64,
    owed: i64,
    current_frame: u64,
    debug: bool,
    /// extra input word (players 3 / 4) for the next frame
    ext: u32,
    save_filename: String,
    save_path: String,
    state_path: String,
}

impl TmntSystem {
    pub fn new(set_name: &str, mut regions: HashMap<String, Vec<u8>>, two_player: bool) -> Result<Self, String> {
        let mut rom = take_region(&mut regions, "maincpu")?;
        rom.resize(0x60000, 0xFF);
        let mut audio = take_region(&mut regions, "audiocpu")?;
        audio.resize(0x10000, 0xFF);
        let tiles = interleave_words(&[take_region(&mut regions, "k052109_0")?,
                                       take_region(&mut regions, "k052109_1")?]);
        // the sprite ROMs are two word-interleaved pairs, one after the other
        let mut sprites = interleave_words(&[take_region(&mut regions, "k051960_0")?,
                                             take_region(&mut regions, "k051960_1")?]);
        sprites.extend(interleave_words(&[take_region(&mut regions, "k051960_2")?,
                                          take_region(&mut regions, "k051960_3")?]));
        let proms = take_region(&mut regions, "proms")?;
        let sprites = unscramble_sprites(&sprites, &proms);
        let k007232 = take_region(&mut regions, "k007232")?;
        let upd = take_region(&mut regions, "upd")?;
        let title = take_region(&mut regions, "title")?;
        let mut bus = TmntBus {
            rom, ram: vec![0; 0x4000],
            video: Video::new(tiles, sprites),
            sound: Sound::new(audio, k007232, upd, &title),
            irq5: false, irq5_mask: false, last_a0000: 0,
            coins: 0xFF, players: [0xFF; 4],
            dsw: [0xFF, 0x5E, 0xFF],
            two_player, slot: 0, r_prev: false,
        };
        let mut cpu = M68000::new();
        cpu.reset(&mut bus);
        Ok(Self {
            cpu, bus, main_phase: 0, owed: 0, current_frame: 0, debug: false,
            ext: 0,
            save_filename: set_name.to_string(),
            save_path: "user://GD_EMU/Arcade/Konami/Save".to_string(),
            state_path: "user://GD_EMU/Arcade/Konami/State".to_string(),
        })
    }

    fn run_main_line(&mut self) {
        self.main_phase += MAIN_CLOCK;
        self.owed += (self.main_phase / LINE_RATE) as i64;
        self.main_phase %= LINE_RATE;
        while self.owed > 0 {
            self.cpu.ipl = if self.bus.irq5 { 5 } else { 0 };
            if self.cpu.halted { self.owed = 0; break; }
            self.owed -= self.cpu.step(&mut self.bus) as i64;
        }
    }

    fn vblank_start(&mut self) {
        if self.bus.irq5_mask { self.bus.irq5 = true; }
        self.bus.video.sprite_dma();
    }
}

impl System for TmntSystem {
    fn set_option(&mut self, key: &str, value: i64) {
        if key == "debug_report" { self.debug = value != 0; }
    }

    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        for line in 0..LINES {
            if line == VBLANK_START { self.vblank_start(); }
            if line == VBLANK_END { self.bus.video.render(); }
            self.run_main_line();
            self.bus.sound.run(1, LINE_RATE);
        }
        self.current_frame += 1;
        if self.debug && (matches!(self.current_frame, 200 | 600 | 1500) || (self.current_frame >= 3000 && self.current_frame % 1500 == 0 && self.current_frame <= 12000)) {
            self.bus.video.debug_dump(&format!("frame {}", self.current_frame));
        }
    }

    /// Pad 1 plays character slot `slot` (4-player sets: 0 Leonardo, 1 Michelangelo,
    /// 2 Donatello, 3 Raphael; each has its own coin slot), pad 2 the next one. R cycles pad
    /// 1's slot. 01 left, 02 right, 04 up, 08 down, 10 attack (B), 20 jump (A), 80 start
    /// (2-player sets only). Coin = the pad's coin slot (pad 2: P2 Select). L = service
    /// switch (test menu).
    fn set_input(&mut self, mask: u32) {
        let joy = |p: &pad::Pad| -> u8 {
            let mut v = 0xFF;
            if p.left { v &= !0x01; }
            if p.right { v &= !0x02; }
            if p.up { v &= !0x04; }
            if p.down { v &= !0x08; }
            if p.b { v &= !0x10; }
            if p.a { v &= !0x20; }
            if p.start { v &= !0x80; }
            v
        };
        let pads = pad::pads(mask, self.ext);
        let b = &mut self.bus;
        let slots = if b.two_player { 2 } else { 4 };
        if pads[0].r && !b.r_prev { b.slot = (b.slot + 1) % slots; }
        b.r_prev = pads[0].r;
        // pad n drives the slot n-1 places after pad 1's (pads 3 / 4 only on 4-player sets)
        b.players = [0xFF; 4];
        let mut coins = 0xFF;
        for (n, p) in pads.iter().take(slots as usize).enumerate() {
            let s = (b.slot as usize + n) % slots as usize;
            b.players[s] = joy(p);
            if p.coin { coins &= !(1 << s); }
        }
        b.coins = coins;
        b.dsw[2] = if pads[0].l { 0xFF & !0x04 } else { 0xFF };
    }

    fn set_extra_input(&mut self, ext: u32) { self.ext = ext; }

    fn drain_audio(&mut self) -> Vec<f32> { std::mem::take(&mut self.bus.sound.out) }
    fn sample_rate(&self) -> u32 { 44100 }
    fn power_on(&mut self) {}
    fn power_off(&mut self) {}
    fn has_battery(&self) -> bool { false }
    fn save_filename(&self) -> &str { &self.save_filename }
    fn save_battery_path(&self) -> &str { &self.save_path }
    fn save_state_path(&self) -> &str { &self.state_path }
    fn is_sram_dirty(&self) -> bool { false }
    fn clear_sram_dirty(&mut self) {}
    fn get_current_frame(&self) -> i64 { self.current_frame as i64 }
    fn get_sram(&self) -> Option<&[u8]> { None }
    fn load_sram(&mut self, _data: &[u8]) {}
    fn framebuffer(&self) -> &[u8] { &self.bus.video.framebuffer }

    fn reset(&mut self) {
        let b = &mut self.bus;
        b.irq5 = false;
        b.irq5_mask = false;
        b.last_a0000 = 0;
        b.video.reset();
        b.sound.reset();
        self.cpu = M68000::new();
        self.cpu.reset(&mut self.bus);
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
        let b = &self.bus;
        let main = MainStateRef {
            ram: &b.ram, video: &b.video, irq5: b.irq5, irq5_mask: b.irq5_mask,
            last: b.last_a0000, slot: b.slot,
        };
        let s = (&self.cpu, main, b.sound.save(), self.main_phase, self.owed, self.current_frame);
        state::encode(&self.save_filename, STATE_VERSION, &s)
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let (cpu, main, sound, mp, owed, frame): (M68000, MainState, SoundState, u64, i64, u64) =
            state::decode(&self.save_filename, STATE_VERSION, data)?;
        let b = &mut self.bus;
        if main.ram.len() != b.ram.len() { return Err("save state doesn't fit this board".into()); }
        b.ram = main.ram;
        let mut video = main.video;
        video.keep_static(&mut b.video);
        b.video = video;
        b.irq5 = main.irq5;
        b.irq5_mask = main.irq5_mask;
        b.last_a0000 = main.last;
        b.slot = main.slot;
        b.sound.load(sound);
        self.cpu = cpu;
        self.main_phase = mp;
        self.owed = owed;
        self.current_frame = frame;
        Ok(())
    }
}
