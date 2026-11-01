//! Atari Lynx (1989): 65SC02 CPU, Mikey (timers, LCD, sound), Suzy (sprites, math,
//! joypad, cart port), 64 KB RAM, 160 x 102 LCD with 16 colours out of 4096.
//!
//! Map: RAM everywhere, with Suzy at FC00, Mikey at FD00, the boot ROM at FE00-FFF7 and
//! the vectors at FFFA-FFFF laid over it. MAPCTL (FFF9, always there) bit 0 hides Suzy,
//! bit 1 Mikey, bit 2 the ROM, bit 3 the vectors.
//!
//! Time is counted in 16 MHz ticks; a CPU instruction of n cycles takes 1 + 5n (Handy's
//! figure: RAM cycles are 5 ticks). A frame is whatever the game's timers make it (timer
//! 2's borrow), usually 60 or 75 Hz.
//!
//! Sprites: a write to CPUSLEEP with SPRGO set has Suzy draw the whole sprite chain at
//! once; the CPU then sleeps for the time that took (an interrupt wakes it sooner).
//!
//! Boot: with `user://BIOS/lynxboot.img` the real ROM runs; without it, `boot.rs` does
//! the ROM's job. `.o` homebrew files load straight into RAM.
//!
//! Input mask: D-pad, A = outer button, B = inner button, L = option 1, R = option 2,
//! START = pause.

use serde::{Deserialize, Serialize};
use crate::common::cpu::m65c02::{Bus65, Cpu65c02, Variant};
use crate::common::input as pad;
use crate::common::system::{DisplayInfo, Rotation, System};
use crate::lynx::boot;
use crate::lynx::cart::{Cart, Header, Rotate};
use crate::lynx::eeprom::Eeprom;
use crate::lynx::mikey::{Events, Mikey, MikeyWrite, HEIGHT, WIDTH};
use crate::lynx::suzy::{Suzy, SuzyAccess};

const TICKS_PER_SECOND: u64 = 16_000_000;
/// Longest frame we wait for before showing what we have (display off, odd timers).
const MAX_FRAME_TICKS: u64 = TICKS_PER_SECOND / 30;
const SAMPLE_RATE: u32 = 44_100;
const STATE_VERSION: u32 = 2;

#[derive(Serialize, Deserialize)]
pub struct Bus {
    #[serde(with = "serde_bytes")]
    pub ram: Vec<u8>,
    #[serde(skip)]
    pub rom: Vec<u8>,
    pub mapctl: u8,
    pub mikey: Mikey,
    pub suzy: Suzy,
    pub cart: Cart,
    pub eeprom: Eeprom,
    /// set by a write to the stand-in ROM's trap (FD97)
    #[serde(skip)]
    pub trap: bool,
    #[serde(skip)]
    pub sleep: bool,
}

impl Bus65 for Bus {
    fn read(&mut self, a: u16) -> u8 {
        let m = self.mapctl;
        match a {
            0xFC00..=0xFCFF if m & 1 == 0 => {
                let (v, acc) = self.suzy.read(a as u8);
                match acc {
                    SuzyAccess::Cart0 => { let v = self.cart.read0(); self.eeprom.counter(self.cart.counter as u16); v }
                    SuzyAccess::Cart1 => { let v = self.cart.read1(); self.eeprom.counter(self.cart.counter as u16); v }
                    SuzyAccess::None => v,
                }
            }
            0xFD8B if m & 2 == 0 && self.eeprom.present() => {
                // IODAT bit 4: the EEPROM's data out while the pin is an input
                self.eeprom.busy();
                let v = self.mikey.read(0x8B) & !0x10;
                v | if self.mikey.iodir & 0x10 != 0 { self.mikey.iodat & 0x10 } else { (self.eeprom.out_bit as u8) << 4 }
            }
            0xFD00..=0xFDFF if m & 2 == 0 => self.mikey.read(a as u8),
            0xFFF9 => self.mapctl,
            0xFE00..=0xFFF7 if m & 4 == 0 => self.rom[(a - 0xFE00) as usize],
            0xFFFA..=0xFFFF if m & 8 == 0 => self.rom[(a - 0xFE00) as usize],
            _ => self.ram[a as usize],
        }
    }

    fn write(&mut self, a: u16, v: u8) {
        let m = self.mapctl;
        match a {
            0xFC00..=0xFCFF if m & 1 == 0 => match self.suzy.write(a as u8, v) {
                SuzyAccess::Cart0 => { self.cart.write0(); self.eeprom.counter(self.cart.counter as u16); }
                SuzyAccess::Cart1 => { self.cart.write1(v); self.eeprom.counter(self.cart.counter as u16); }
                SuzyAccess::None => {}
            },
            0xFD00..=0xFDFF if m & 2 == 0 => match self.mikey.write(a as u8, v) {
                MikeyWrite::Strobe(on) => { self.cart.set_strobe(on); self.eeprom.counter(self.cart.counter as u16); }
                MikeyWrite::AddrData(bit) => {
                    self.cart.set_addr_data(bit);
                    self.eeprom.io(self.mikey.iodir, self.mikey.iodat);
                }
                MikeyWrite::None if a == 0xFD8A => self.eeprom.io(self.mikey.iodir, self.mikey.iodat),
                MikeyWrite::Sleep => self.sleep = true,
                MikeyWrite::BootTrap => self.trap = true,
                MikeyWrite::None => {}
            },
            0xFFF9 => self.mapctl = v,
            _ => self.ram[a as usize] = v,
        }
    }
}

pub struct LynxSystem {
    pub cpu: Cpu65c02,
    pub bus: Bus,
    pub header: Header,
    /// the player's choice over the cart's rotation
    rotate_override: Option<Rotate>,
    debug: bool,
    dbg_peak: f32,
    dbg_nonzero: u64,
    real_rom: bool,
    homebrew: Option<crate::lynx::cart::Homebrew>,
    pub framebuffer: Vec<u8>,
    frame: u64,
    last_frame_ticks: u64,
    /// ticks the CPU still sleeps (Suzy drawing)
    sleep_ticks: u32,
    // sound: the speakers averaged down to 44.1 kHz
    audio: Vec<f32>,
    audio_phase: u64,
    acc_l: i64,
    acc_r: i64,
    acc_t: u64,
    save_filename: String,
    save_battery_path: String,
    save_state_path: String,
}

#[derive(Serialize, Deserialize)]
struct State { version: u32, cpu: Cpu65c02, bus: Bus, frame: u64 }

impl LynxSystem {
    /// `bios`: the 512-byte boot ROM, or empty to boot without it.
    pub fn from_rom(rom: &[u8], filename: &str, bios: &[u8]) -> Result<Self, String> {
        let homebrew = Cart::homebrew(rom);
        let (cart, header) = if homebrew.is_some() {
            (Cart::empty(), Header { name: filename.to_string(), maker: String::new(), rotate: Rotate::None, eeprom: 0, audin: false })
        } else {
            Cart::new(rom)?
        };
        let real_rom = bios.len() == 0x200 && homebrew.is_none();
        println!("Atari Lynx: \"{}\" ({}), {} KB, rotation {:?}, EEPROM type {:#04x}, {}", header.name, header.maker,
                 cart.bank0.len() / 1024, header.rotate, header.eeprom, if homebrew.is_some() { "homebrew" } else if real_rom { "real boot ROM" } else { "boot without ROM" });
        let mut sys = Self {
            cpu: Cpu65c02::new(Variant::Sc02),
            bus: Bus { ram: vec![0; 0x10000], rom: if real_rom { bios.to_vec() } else { boot::fake_rom() }, mapctl: 0,
                       mikey: Mikey::new(), suzy: Suzy::new(), cart, eeprom: Eeprom::new(header.eeprom), trap: false, sleep: false },
            header, rotate_override: None, debug: false, dbg_peak: 0.0, dbg_nonzero: 0, real_rom, homebrew,
            framebuffer: vec![0; WIDTH * HEIGHT * 4],
            frame: 0, last_frame_ticks: TICKS_PER_SECOND / 60, sleep_ticks: 0,
            audio: Vec::new(), audio_phase: 0, acc_l: 0, acc_r: 0, acc_t: 0,
            save_filename: filename.to_string(),
            save_battery_path: "user://GD_EMU/Lynx/Save".to_string(),
            save_state_path: "user://GD_EMU/Lynx/State".to_string(),
        };
        sys.power_up();
        Ok(sys)
    }

    /// Option "debug_report": every 600 frames, the sound side of Mikey in the console.
    fn debug_report(&mut self) {
        let m = &mut self.bus.mikey;
        println!("--- Lynx debug, frame {} ({} us a frame), MAPCTL {:02X}, IRQ pending {:02X}, sound reg writes {}",
                 self.frame, self.last_frame_ticks * 1_000_000 / TICKS_PER_SECOND, self.bus.mapctl, m.irq_pending, m.sound_writes);
        println!("  MSTEREO {:02X} MPAN {:02X} ATTEN {:02X?}; output peak {:.3}, non-zero samples {}",
                 m.stereo, m.pan, m.atten, self.dbg_peak, self.dbg_nonzero);
        for c in 0..4 {
            let r: Vec<u8> = (0..8).map(|i| m.read(0x20 + c * 8 + i)).collect();
            println!("  audio {c}: VOL {:02X} FB {:02X} OUT {:02X} SHIFT {:02X} BACKUP {:02X} CTL {:02X} COUNT {:02X} MISC {:02X}",
                     r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7]);
        }
        let t: Vec<String> = (0..8).map(|i| format!("{i}:{:02X}/{:02X}/{:02X}", m.timers[i].backup, m.timers[i].ctla, m.timers[i].count)).collect();
        println!("  timers (backup/ctla/count) {}", t.join(" "));
        m.sound_writes = 0;
        self.dbg_peak = 0.0;
        self.dbg_nonzero = 0;
    }

    pub fn rotate(&self) -> Rotate { self.rotate_override.unwrap_or(self.header.rotate) }

    fn power_up(&mut self) {
        self.bus.ram.iter_mut().for_each(|b| *b = 0);
        self.bus.mapctl = 0;
        self.bus.mikey = Mikey::new();
        self.bus.suzy = Suzy::new();
        self.bus.cart.shifter = 0;
        self.bus.cart.counter = 0;
        self.cpu = Cpu65c02::new(Variant::Sc02);
        if let Some(h) = self.homebrew.clone() {
            self.bus.mikey.preset();
            let end = (h.load as usize + h.data.len()).min(0x10000);
            self.bus.ram[h.load as usize..end].copy_from_slice(&h.data[..end - h.load as usize]);
            self.cpu.pc = h.load;
            return;
        }
        if !self.real_rom { self.bus.mikey.preset(); }
        self.cpu.reset(&mut self.bus);
    }

    /// The stand-in ROM was called (see boot.rs).
    pub(crate) fn boot_trap(&mut self) {
        match boot::entry(self.cpu.pc) {
            Some(boot::Entry::SelectBlock) => {
                self.bus.cart.shifter = self.cpu.a as u32;
                self.bus.cart.counter = 0;
            }
            Some(boot::Entry::Boot) => {
                self.bus.ram.iter_mut().for_each(|b| *b = 0);
                self.bus.ram[5] = 0x00;
                self.bus.ram[6] = 0x02;
                self.bus.cart.shifter = 0;
                self.bus.cart.counter = 0;
                self.load_loader();
            }
            Some(boot::Entry::LoadLoader) => self.load_loader(),
            None => {}
        }
    }

    /// Read the encrypted loader from the cart, decrypt it to ($05) and jump to 0200.
    fn load_loader(&mut self) {
        let first = self.bus.cart.read0();
        let blocks = 256 - first as usize;
        let mut enc = vec![first];
        for _ in 0..blocks * 51 { enc.push(self.bus.cart.read0()); }
        let (plain, _) = boot::decrypt_frame(&enc);
        let mut addr = self.bus.ram[5] as u16 | (self.bus.ram[6] as u16) << 8;
        for b in plain { self.bus.write(addr, b); addr = addr.wrapping_add(1); }
        self.cpu.pc = 0x0200;
    }

    fn draw_line(&mut self, line: usize, addr: u16, flip: bool) {
        let pal: Vec<[u8; 3]> = (0..16).map(|i| self.bus.mikey.rgb(i)).collect();
        let row = line * WIDTH * 4;
        let mut a = addr;
        for x in 0..WIDTH / 2 {
            let b = self.bus.ram[a as usize];
            let (p0, p1) = if flip { (b & 15, b >> 4) } else { (b >> 4, b & 15) };
            a = if flip { a.wrapping_sub(1) } else { a.wrapping_add(1) };
            for (k, p) in [p0, p1].into_iter().enumerate() {
                let [r, g, bl] = pal[p as usize];
                let i = row + (x * 2 + k) * 4;
                self.framebuffer[i..i + 4].copy_from_slice(&[r, g, bl, 255]);
            }
        }
    }

    /// Run until the frame ends (timer 2) or a time limit.
    fn run_until_frame(&mut self) {
        let mut ticks = 0u64;
        let mut ev = Events::default();
        while ticks < MAX_FRAME_TICKS {
            let t = if self.sleep_ticks > 0 {
                // the CPU sleeps while Suzy draws; an interrupt wakes it early
                let t = self.sleep_ticks.min(64);
                self.sleep_ticks -= t;
                if self.bus.mikey.irq() { self.sleep_ticks = 0; }
                t
            } else {
                let irq = self.bus.mikey.irq();
                let n = self.cpu.interrupt(&mut self.bus, irq, false);
                let cycles = if n > 0 { n } else { self.cpu.step(&mut self.bus) };
                if self.bus.trap { self.bus.trap = false; self.boot_trap(); }
                if self.bus.sleep {
                    // CPUSLEEP: Suzy draws the sprite chain now; the CPU sleeps for as long
                    // as that takes (no time at all if SPRGO isn't set)
                    self.bus.sleep = false;
                    let Bus { suzy, ram, .. } = &mut self.bus;
                    self.sleep_ticks = suzy.paint(ram);
                }
                1 + 5 * cycles
            };
            ticks += t as u64;
            self.bus.mikey.advance(t, &mut ev);
            self.mix(t);
            for (line, addr, flip) in ev.lines.drain(..) { self.draw_line(line, addr, flip); }
            if ev.frame_end { break; }
        }
        self.last_frame_ticks = ticks.max(1);
    }

    /// Average the speakers over `t` ticks and put out 44.1 kHz stereo samples.
    fn mix(&mut self, t: u32) {
        let (l, r) = self.bus.mikey.sound();
        self.acc_l += l as i64 * t as i64;
        self.acc_r += r as i64 * t as i64;
        self.acc_t += t as u64;
        self.audio_phase += t as u64 * SAMPLE_RATE as u64;
        if self.audio_phase >= TICKS_PER_SECOND {
            self.audio_phase -= TICKS_PER_SECOND;
            let n = self.acc_t.max(1) as f32;
            // four channels of +-128 at most; leave some headroom
            let scale = 1.0 / (384.0 * n);
            self.audio.push((self.acc_l as f32 * scale).clamp(-1.0, 1.0));
            self.audio.push((self.acc_r as f32 * scale).clamp(-1.0, 1.0));
            self.acc_l = 0; self.acc_r = 0; self.acc_t = 0;
        }
    }
}

impl System for LynxSystem {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        let before = self.audio.len();
        self.run_until_frame();
        if self.debug {
            for &x in &self.audio[before..] {
                self.dbg_peak = self.dbg_peak.max(x.abs());
                if x != 0.0 { self.dbg_nonzero += 1; }
            }
        }
        self.frame += 1;
        if self.debug && self.frame % 600 == 0 { self.debug_report(); }
    }

    fn set_input(&mut self, m: u32) {
        let on = |b: u32| m & b != 0;
        // with the Lynx held upright the D-pad turns with the screen (as libretro-handy)
        let (up, down, left, right) = match self.rotate() {
            Rotate::None => (pad::UP, pad::DOWN, pad::LEFT, pad::RIGHT),
            Rotate::Left => (pad::RIGHT, pad::LEFT, pad::UP, pad::DOWN),
            Rotate::Right => (pad::LEFT, pad::RIGHT, pad::DOWN, pad::UP),
        };
        let mut j = 0u8;
        if on(up) { j |= 0x80; }
        if on(down) { j |= 0x40; }
        if on(left) { j |= 0x20; }
        if on(right) { j |= 0x10; }
        if on(pad::L) { j |= 0x08; }
        if on(pad::R) { j |= 0x04; }
        if on(pad::B) { j |= 0x02; }
        if on(pad::A) { j |= 0x01; }
        self.bus.suzy.joystick = j;
        self.bus.suzy.switches = on(pad::START) as u8;
    }

    fn framebuffer(&self) -> &[u8] { &self.framebuffer }

    fn display_info(&self) -> DisplayInfo {
        DisplayInfo {
            buffer_width: WIDTH as i32,
            buffer_height: HEIGHT as i32,
            visible_x: 0,
            visible_y: 0,
            visible_width: WIDTH as i32,
            visible_height: HEIGHT as i32,
            // square pixels; turned on its side for games played with the Lynx held
            // upright (Handy: "left" puts the first pixel top right, i.e. clockwise)
            target_aspect_ratio: if self.rotate() == Rotate::None { WIDTH as f32 / HEIGHT as f32 } else { HEIGHT as f32 / WIDTH as f32 },
            rotation: match self.rotate() { Rotate::Left => Rotation::CW90, Rotate::Right => Rotation::CCW90, Rotate::None => Rotation::None },
        }
    }

    /// Option "lynx_rotate": 0 = from the cart, 1 = none, 2 = left, 3 = right.
    fn set_option(&mut self, key: &str, value: i64) {
        if key == "debug_report" { self.debug = value != 0; }
        if key == "lynx_rotate" {
            self.rotate_override = match value { 1 => Some(Rotate::None), 2 => Some(Rotate::Left), 3 => Some(Rotate::Right), _ => None };
        }
    }

    fn frame_time_usec(&self) -> i64 { (self.last_frame_ticks * 1_000_000 / TICKS_PER_SECOND) as i64 }
    fn drain_audio(&mut self) -> Vec<f32> { std::mem::take(&mut self.audio) }
    fn sample_rate(&self) -> u32 { SAMPLE_RATE }
    fn power_on(&mut self) {}
    fn power_off(&mut self) {}
    fn reset(&mut self) { self.power_up(); }
    fn has_battery(&self) -> bool { self.bus.eeprom.present() }
    fn save_filename(&self) -> &str { &self.save_filename }
    fn save_battery_path(&self) -> &str { &self.save_battery_path }
    fn save_state_path(&self) -> &str { &self.save_state_path }
    fn is_sram_dirty(&self) -> bool { self.bus.eeprom.dirty }
    fn clear_sram_dirty(&mut self) { self.bus.eeprom.dirty = false; }
    fn get_current_frame(&self) -> i64 { self.frame as i64 }
    /// The cartridge EEPROM, if the cart has one.
    fn get_sram(&self) -> Option<&[u8]> { if self.bus.eeprom.present() { Some(self.bus.eeprom.bytes()) } else { None } }
    fn load_sram(&mut self, data: &[u8]) { self.bus.eeprom.load(data); }

    fn save_state(&self) -> Vec<u8> {
        let config = bincode::config::standard().with_fixed_int_encoding();
        #[derive(Serialize)]
        struct Ref<'a> { version: u32, cpu: &'a Cpu65c02, bus: &'a Bus, frame: u64 }
        bincode::serde::encode_to_vec(&Ref { version: STATE_VERSION, cpu: &self.cpu, bus: &self.bus, frame: self.frame }, config)
            .unwrap_or_default()
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let config = bincode::config::standard().with_fixed_int_encoding();
        let (s, _): (State, usize) = bincode::serde::decode_from_slice(data, config)
            .map_err(|e| format!("Failed to decode Lynx save state: {e:?}"))?;
        if s.version != STATE_VERSION { return Err(format!("save state version {} (expected {STATE_VERSION})", s.version)); }
        let rom = std::mem::take(&mut self.bus.rom);
        let (b0, b1) = (std::mem::take(&mut self.bus.cart.bank0), std::mem::take(&mut self.bus.cart.bank1));
        self.bus = s.bus;
        self.bus.rom = rom;
        self.bus.cart.bank0 = b0;
        self.bus.cart.bank1 = b1;
        self.cpu = s.cpu;
        self.frame = s.frame;
        Ok(())
    }
}
