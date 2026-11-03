//! Super Nintendo: 65C816 CPU (W65816), the main bus (bus.rs: map, timing, DMA,
//! interrupts), the PPU (ppu.rs) and the sound side (smp.rs: SPC700 + timers, dsp.rs).
//!
//! A frame is 262 lines of 1364 master clocks (21.477 MHz, about 60.1 Hz). Interrupts
//! are taken between instructions: NMI at the start of vblank when enabled, IRQ from
//! the H / V timer while the I flag is clear (WAI wakes on either).
//!
//! Input mask (player 1; player 2 through `pad::player2`): D-pad, A, B, X, Y, L, R,
//! START, SELECT.

use serde::{Deserialize, Serialize};
use crate::common::cpu::w65816::{W65816, I};
use crate::common::input as pad;
use crate::common::system::{DisplayInfo, Rotation, System};
use crate::snes::bus::Bus;
use crate::snes::cart::Cart;
use crate::snes::ppu::{HEIGHT, WIDTH};

/// What the frontend plays (the DSP makes 32,040 Hz: 24.607 MHz / 768)
const SAMPLE_RATE: u32 = 44_100;
const DSP_RATE: f64 = 32_040.0;
const STATE_VERSION: u32 = 8;
const MASTER_HZ: f64 = 21_477_272.7;

pub struct SnesSystem {
    pub cpu: W65816,
    pub bus: Bus,
    audio: Vec<f32>,
    /// resampler: position between the last two DSP frames (0..1) and those frames
    rs_pos: f64,
    rs_prev: [f32; 2],
    rs_cur: [f32; 2],
    /// option "debug_report": print where both CPUs are (each second for 10 s, then
    /// every 10 s)
    debug: bool,
    /// (line, CPU PC, SMP PC) sampled through the current frame
    samples: Vec<(u32, u32, u16)>,
    save_filename: String,
    save_battery_path: String,
    save_state_path: String,
}

#[derive(Serialize, Deserialize)]
struct State { version: u32, cpu: W65816, bus: Bus }

/// Unified input mask -> SNES pad bits (15 B, 14 Y, 13 Select, 12 Start, 11 Up,
/// 10 Down, 9 Left, 8 Right, 7 A, 6 X, 5 L, 4 R).
fn pad_bits(m: u32) -> u16 {
    let mut b = 0u16;
    for (bit, on) in [(15, pad::B), (14, pad::Y), (13, pad::SELECT), (12, pad::START), (11, pad::UP), (10, pad::DOWN),
                      (9, pad::LEFT), (8, pad::RIGHT), (7, pad::A), (6, pad::X), (5, pad::L), (4, pad::R)] {
        if m & on != 0 { b |= 1 << bit; }
    }
    // opposite directions can't both be down on a real pad
    if b & 0x0C00 == 0x0C00 { b &= !0x0400; }
    if b & 0x0300 == 0x0300 { b &= !0x0100; }
    b
}

impl SnesSystem {
    /// `firmware`: coprocessor firmware files from the frontend ("SNFW" bundle, see
    /// `find_firmware`); empty when there are none.
    pub fn from_rom(rom: &[u8], filename: &str, firmware: &[u8]) -> Result<Self, String> {
        let mut cart = Cart::new(rom)?;
        let mut nec = None;
        if cart.has_nec_dsp() {
            let name = crate::snes::necdsp::firmware_name(&cart.title);
            let fw = cart.firmware.take().or_else(|| find_firmware(firmware, name));
            match fw {
                Some(fw) if fw.len() >= 0x2000 => {
                    println!("SNES: {} DSP ({name} firmware)", cart.title);
                    nec = Some(Box::new(crate::snes::necdsp::NecDsp::new(&fw)));
                }
                _ => println!("SNES: this game needs the {} firmware: put {name}.rom (8 KB) in user://BIOS/ ; it won't run without it", name.to_uppercase()),
            }
        }
        let mut cx4 = None;
        if cart.cx4 {
            let fw = cart.firmware.take().or_else(|| find_firmware(firmware, "cx4")).or_else(|| find_firmware(firmware, "cx4.data"));
            match fw {
                Some(fw) if fw.len() >= 0x0C00 => {
                    println!("SNES: {} Cx4", cart.title);
                    cx4 = Some(Box::new(crate::snes::cx4::Cx4::new(&fw)));
                }
                _ => println!("SNES: this game needs the Cx4 firmware: put cx4.rom (3 KB) in user://BIOS/ ; it won't run without it"),
            }
        }
        println!("SNES: \"{}\", {:?}{}, {} KB ROM, {} KB SRAM, chipset {:02X}, region {:02X}", cart.title, cart.map,
                 if cart.fast_rom { " (fast)" } else { "" }, cart.rom.len() / 1024, cart.sram.len() / 1024, cart.chipset, cart.region);
        if cart.has_nec_dsp() || cart.cx4 {
            // reported above once the firmware is found
        } else if cart.map == crate::snes::cart::Map::Sa1 {
            println!("SNES: SA-1 cartridge, {} KB of BW-RAM{}", cart.sram.len() / 1024, if cart.battery { " (battery)" } else { "" });
        } else if cart.map == crate::snes::cart::Map::SuperFx {
            println!("SNES: Super FX cartridge, {} KB of Super FX RAM{}", cart.sram.len() / 1024, if cart.battery { " (battery)" } else { "" });
        } else if cart.chipset & 0x0F >= 3 {
            println!("SNES: this cartridge has an enhancement chip (not emulated yet); it may not run");
        }
        let hirom = cart.map != crate::snes::cart::Map::LoRom;
        let (has_ram, rom_len) = (!cart.sram.is_empty(), cart.rom.len());
        let mut bus = Bus::new(cart);
        if nec.is_some() { bus.nec_map = Some(crate::snes::necdsp::board_map(hirom, has_ram, rom_len)); }
        bus.nec = nec;
        bus.cx4 = cx4;
        let mut s = Self {
            cpu: W65816::new(),
            bus,
            audio: Vec::new(), rs_pos: 0.0, rs_prev: [0.0; 2], rs_cur: [0.0; 2], debug: false, samples: Vec::new(),
            save_filename: filename.to_string(),
            save_battery_path: "user://GD_EMU/SNES/Save".to_string(),
            save_state_path: "user://GD_EMU/SNES/State".to_string(),
        };
        s.cpu.reset(&mut s.bus);
        Ok(s)
    }

    /// Run until the frame ends.
    fn run_until_frame(&mut self) {
        self.bus.frame_done = false;
        let start = self.bus.clocks;
        self.samples.clear();
        let mut next_sample = 0;
        while !self.bus.frame_done {
            if self.debug && self.bus.v >= next_sample {
                self.samples.push((self.bus.v, self.cpu.pc, self.bus.apu.cpu.pc));
                next_sample = self.bus.v + 33;
            }
            if self.bus.nmi_pending {
                self.bus.nmi_pending = false;
                self.cpu.interrupt(&mut self.bus, true);
            } else if self.bus.irq_line() && self.cpu.p & I == 0 {
                self.cpu.interrupt(&mut self.bus, false);
            } else {
                if self.cpu.wai && self.bus.irq_line() { self.cpu.wai = false; }
                self.cpu.step(&mut self.bus);
            }
            if self.bus.clocks - start > 4 * 1364 * 262 { break; }   // a stuck frame
        }
        // the DSP's 32 kHz stereo frames, resampled (linear) to the output rate
        let step = DSP_RATE / SAMPLE_RATE as f64;
        let frames = std::mem::take(&mut self.bus.apu.mem.dsp.out);
        for f in frames.chunks_exact(2) {
            self.rs_prev = self.rs_cur;
            self.rs_cur = [f[0] as f32 / 32768.0, f[1] as f32 / 32768.0];
            while self.rs_pos < 1.0 {
                let t = self.rs_pos as f32;
                for ch in 0..2 { self.audio.push(self.rs_prev[ch] + (self.rs_cur[ch] - self.rs_prev[ch]) * t); }
                self.rs_pos += step;
            }
            self.rs_pos -= 1.0;
        }
    }
}

/// Firmware from the frontend's bundle: "SNFW", then entries of a name length (u8), the
/// file name, a u32 little-endian length and the data. Takes `<name>.rom`, or
/// `<name>.program.rom` + `<name>.data.rom` (the split form) joined.
pub fn find_firmware(bundle: &[u8], name: &str) -> Option<Vec<u8>> {
    if !bundle.starts_with(b"SNFW") { return None; }
    let mut files = std::collections::HashMap::new();
    let mut i = 4;
    while i < bundle.len() {
        let n = bundle[i] as usize;
        let file = String::from_utf8_lossy(bundle.get(i + 1..i + 1 + n)?).to_lowercase();
        i += 1 + n;
        let len = u32::from_le_bytes(bundle.get(i..i + 4)?.try_into().ok()?) as usize;
        i += 4;
        files.insert(file, bundle.get(i..i + len)?.to_vec());
        i += len;
    }
    if let Some(f) = files.get(&format!("{name}.rom")) { return Some(f.clone()); }
    let (p, d) = (files.get(&format!("{name}.program.rom"))?, files.get(&format!("{name}.data.rom"))?);
    let mut out = p.clone();
    out.extend_from_slice(d);
    Some(out)
}

impl SnesSystem {
    /// Where both CPUs are, for bug reports (printed to the console).
    fn debug_report(&self) {
        let c = &self.cpu;
        let b = &self.bus;
        let m = &b.apu.mem;
        crate::dlog!("=== SNES debug, frame {} ===", b.frame);
        crate::dlog!("CPU  PC {:02X}:{:04X}  A {:04X} X {:04X} Y {:04X} S {:04X} D {:04X} DB {:02X} P {:02X} E {}  wai {} stp {}",
                 c.pc >> 16, c.pc & 0xFFFF, c.a, c.x, c.y, c.s, c.d, c.b, c.p, c.e as u8, c.wai as u8, c.stp as u8);
        let code: Vec<String> = (0..12).map(|i| format!("{:02X}", b.peek((c.pc & 0xFF0000) | (c.pc.wrapping_add(i) & 0xFFFF)))).collect();
        crate::dlog!("CPU  code at PC: {}", code.join(" "));
        crate::dlog!("     NMITIMEN {:02X} INIDISP {:02X} HDMAEN {:02X} mode {:02X} TM {:02X}", b.nmitimen, b.ppu.inidisp, b.hdmaen, b.ppu.bgmode, b.ppu.tm);
        if let Some(x) = &b.cx4 { crate::dlog!("Cx4  {}", x.status()); }
        let s = &b.apu.cpu;
        crate::dlog!("SMP  PC {:04X}  A {:02X} X {:02X} Y {:02X} S {:02X} P {:02X}  sleep {} stop {}", s.pc, s.a, s.x, s.y, s.s, s.p, s.wait as u8, s.stop as u8);
        let scode: Vec<String> = (0..12).map(|i| format!("{:02X}", m.ram[s.pc.wrapping_add(i) as usize])).collect();
        crate::dlog!("SMP  code at PC (RAM): {}", scode.join(" "));
        crate::dlog!("Ports  CPU->SMP {:02X?}   SMP->CPU {:02X?}", m.from_cpu, m.to_cpu);
        let smp: Vec<String> = self.samples.iter().map(|&(v, cp, sp)| format!("line {v}: {:02X}:{:04X} / {sp:04X}", cp >> 16, cp & 0xFFFF)).collect();
        crate::dlog!("PC through the frame (CPU / SMP): {}", smp.join(", "));
        let log: Vec<String> = b.port_log.iter().map(|&(f, v, p, val)| format!("f{f} l{v} 214{p}={val:02X}")).collect();
        crate::dlog!("Last CPU port writes: {}", log.join(" "));
        // PPU register writes in the last whole frame (lines 1-224 are the picture)
        let ppu: Vec<String> = b.ppu_log_last.iter().map(|&(v, h, r, val)| {
            if r == 0 { format!("[l{v} h{h} IRQ]") } else { format!("l{v} h{h} 21{r:02X}={val:02X}") }
        }).collect();
        crate::dlog!("PPU writes last frame ({}): {}", ppu.len(), ppu.join(", "));
        let p = &b.ppu;
        crate::dlog!("PPU now: mode {:02X} TM {:02X} TS {:02X} TMW {:02X} TSW {:02X} CGWSEL {:02X} CGADSUB {:02X} win {:02X?} BG3 hofs {} vofs {} BG1 vofs {} BG2 vofs {} NMITIMEN {:02X} VTIME {} HTIME {}",
                 p.bgmode, p.tm, p.ts, p.tmw, p.tsw, p.cgwsel, p.cgadsub, &p.win[..9], p.bg[2].hofs, p.bg[2].vofs, p.bg[0].vofs, p.bg[1].vofs,
                 b.nmitimen, b.vtime, b.htime);
    }
}

impl System for SnesSystem {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        self.run_until_frame();
        self.bus.apply_cheat_writes();
        let f = self.bus.frame;
        if self.debug && ((f <= 600 && f % 60 == 0) || f % 600 == 0) { self.debug_report(); }
    }

    fn set_option(&mut self, key: &str, value: i64) {
        if key == "debug_report" {
            self.debug = value != 0;
            self.bus.port_log_on = self.debug;
        }
    }

    fn set_input(&mut self, m: u32) {
        self.bus.pads[0] = pad_bits(m);
        self.bus.pads[1] = pad_bits(pad::player2(m) | if m & pad::P2_X != 0 { pad::X } else { 0 });
    }

    fn framebuffer(&self) -> &[u8] { &self.bus.framebuffer }

    fn display_info(&self) -> DisplayInfo {
        DisplayInfo {
            buffer_width: WIDTH as i32, buffer_height: HEIGHT as i32,
            visible_x: 0, visible_y: 0, visible_width: WIDTH as i32, visible_height: HEIGHT as i32,
            target_aspect_ratio: 4.0 / 3.0,
            rotation: Rotation::None,
        }
    }

    fn frame_time_usec(&self) -> i64 { (1364.0 * 262.0 * 1e6 / MASTER_HZ) as i64 }
    fn drain_audio(&mut self) -> Vec<f32> { std::mem::take(&mut self.audio) }
    fn sample_rate(&self) -> u32 { SAMPLE_RATE }
    fn power_on(&mut self) {}
    fn power_off(&mut self) {}
    fn reset(&mut self) {
        self.bus.ppu = crate::snes::ppu::Ppu::new();
        self.bus.apu = crate::snes::smp::Smp::new();
        if self.bus.gsu.is_some() { self.bus.gsu = Some(crate::snes::superfx::Gsu::new()); }
        if self.bus.sa1.is_some() { self.bus.sa1 = Some(Box::new(crate::snes::sa1::Sa1::new())); }
        if let Some(d) = &self.bus.nec {
            let mut fresh = crate::snes::necdsp::NecDsp::new(&[0; 0x2000]);
            fresh.keep_firmware(d);
            self.bus.nec = Some(Box::new(fresh));
        }
        if let Some(c) = &self.bus.cx4 {
            let mut fresh = crate::snes::cx4::Cx4::new(&[]);
            fresh.keep_firmware(c);
            self.bus.cx4 = Some(Box::new(fresh));
        }
        self.cpu.reset(&mut self.bus);
    }
    fn cheats_mut(&mut self) -> Option<&mut crate::common::cheats::Cheats> { Some(&mut self.bus.cheats) }
    fn has_battery(&self) -> bool { self.bus.cart.battery && !self.bus.cart.sram.is_empty() }
    fn save_filename(&self) -> &str { &self.save_filename }
    fn save_battery_path(&self) -> &str { &self.save_battery_path }
    fn save_state_path(&self) -> &str { &self.save_state_path }
    fn is_sram_dirty(&self) -> bool { self.bus.cart.sram_dirty }
    fn clear_sram_dirty(&mut self) { self.bus.cart.sram_dirty = false; }
    fn get_current_frame(&self) -> i64 { self.bus.frame as i64 }
    fn get_sram(&self) -> Option<&[u8]> { if self.bus.cart.sram.is_empty() { None } else { Some(&self.bus.cart.sram) } }
    fn load_sram(&mut self, data: &[u8]) {
        let n = data.len().min(self.bus.cart.sram.len());
        self.bus.cart.sram[..n].copy_from_slice(&data[..n]);
    }

    fn save_state(&self) -> Vec<u8> {
        let config = bincode::config::standard().with_fixed_int_encoding();
        #[derive(Serialize)]
        struct Ref<'a> { version: u32, cpu: &'a W65816, bus: &'a Bus }
        bincode::serde::encode_to_vec(&Ref { version: STATE_VERSION, cpu: &self.cpu, bus: &self.bus }, config).unwrap_or_default()
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let config = bincode::config::standard().with_fixed_int_encoding();
        let (s, _): (State, usize) = bincode::serde::decode_from_slice(data, config)
            .map_err(|e| format!("Failed to decode SNES save state: {e:?}"))?;
        if s.version != STATE_VERSION { return Err(format!("save state version {} (expected {STATE_VERSION})", s.version)); }
        let rom = std::mem::take(&mut self.bus.cart.rom);
        let fb = std::mem::take(&mut self.bus.framebuffer);
        let old_nec = self.bus.nec.take();
        let old_cx4 = self.bus.cx4.take();
        let cheats = std::mem::replace(&mut self.bus.cheats, crate::common::cheats::Cheats::new(crate::common::cheats::CheatSystem::Snes));
        self.bus = s.bus;
        self.bus.cheats = cheats;
        if let (Some(new), Some(old)) = (&mut self.bus.nec, &old_nec) { new.keep_firmware(old); }
        if let (Some(new), Some(old)) = (&mut self.bus.cx4, &old_cx4) { new.keep_firmware(old); }
        self.bus.cart.rom = rom;
        self.bus.framebuffer = fb;
        self.cpu = s.cpu;
        Ok(())
    }
}
