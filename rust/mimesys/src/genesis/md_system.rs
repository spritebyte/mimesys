//! Sega Genesis / Mega Drive (1988).
//!
//! 68000 at MCLK / 7 (7.67 MHz), Z80 at MCLK / 15 (3.58 MHz) with 8 KB of its own RAM,
//! the VDP (md_vdp.rs), an SN76489 PSG inside the VDP and a YM2612 FM chip (ym2612.rs, one
//! sample every 1008 master clocks = 53.3 kHz, stepped along with the Z80 so its DAC
//! streams play at their real rate). NTSC: 262 lines of 3420 master clocks, 59.92 Hz.
//!
//! Interrupts: VDP level 6 at vblank, level 4 from the line counter (both autovectored);
//! the Z80 gets /INT for one line at vblank.
//!
//! Controls (3-button pad):
//!   D-pad      D-pad
//!   X [J]      A        B [K]  B        A [L]  C        Start  Start
//!   player 2:  Select = A, B = B, A = C, P2 Start = Start

use serde::{Deserialize, Serialize};
use crate::common::audio::sn76489::{Sn76489, Sn76489Variant};
use crate::common::audio::ym2612::Ym2612;
use crate::common::cpu::m68000::M68000;
use crate::common::cpu::z80::{Z80Cpu, Z80State};
use crate::common::input as pad;
use crate::common::system::{DisplayInfo, Rotation, System};
use crate::genesis::md_bus::*;
use crate::genesis::md_cart::{self, Region};
use crate::genesis::md_vdp::{Vdp, LINES, SCREEN_H, SCREEN_W, VBLANK_LINE};

const MCLK_PER_LINE: i64 = 3420;
const M68K_DIV: i64 = 7;
const Z80_DIV: i64 = 15;
/// master clocks into the line before the VDP draws it (the hblank part)
const HBLANK_MCLK: i64 = 560;
const SAMPLE_RATE: u32 = 44_100;
const STATE_VERSION: u32 = 2;
const MCLK: u64 = 53_693_175;
/// master clocks per YM2612 sample (144 of its clocks at MCLK / 7)
const YM_MCLK: i64 = 1008;
/// mix levels: FM is about +-32K with all 6 channels at full scale (one channel about
/// +-5.4K), the PSG about +-1 at gain 1 with all 4 channels at full volume
const FM_LEVEL: f32 = 1.4 / 32768.0;
const PSG_GAIN: f32 = 0.5;

pub struct GenesisSystem {
    cpu: M68000,
    z80: Z80Cpu,
    pub bus: MdBus,
    /// master clocks the 68000 / Z80 are behind
    m68k_owed: i64,
    z80_owed: i64,
    z80_int: bool,
    /// master clocks into the next YM2612 sample
    ym_mclk: i64,
    /// FM resampled to 44.1 kHz (interleaved stereo), waiting to be mixed with the PSG
    fm_out: Vec<f32>,
    fm_acc: (f32, f32, u32),
    fm_phase: u64,
    current_frame: u64,
    pub title: String,
    save_filename: String,
    save_battery_path: String,
    save_state_path: String,
}

#[derive(Serialize, Deserialize)]
struct MdState {
    version: u32,
    rom_len: usize,
    cpu: M68000,
    z80: Z80State,
    vdp: Vdp,
    #[serde(with = "serde_bytes")]
    ram: Vec<u8>,
    #[serde(with = "serde_bytes")]
    zram: Vec<u8>,
    zbank: u32,
    zbusreq: bool,
    zreset: bool,
    io: Io,
    psg: Sn76489,
    ym: Ym2612,
    ym_mclk: i64,
    #[serde(with = "serde_bytes")]
    sram: Vec<u8>,
    sram_on: bool,
    banks: [u8; 8],
    m68k_owed: i64,
    z80_owed: i64,
    z80_int: bool,
    frame: u64,
}

impl GenesisSystem {
    pub fn from_rom(data: &[u8], filename: &str, _bios: &[u8]) -> Result<Self, String> {
        let mut rom = md_cart::normalize(data);
        if rom.len() < 0x200 { return Err(format!("ROM is only {} bytes", rom.len())); }
        if rom.len() % 2 == 1 { rom.push(0xFF); }
        let header = md_cart::parse_header(&rom);
        let version = match header.region { Region::Usa => 0xA0, Region::Japan => 0x20, Region::Europe => 0xE0 };
        let mut psg = Sn76489::new(Sn76489Variant::Sega, 3_579_545, SAMPLE_RATE);
        psg.gain = PSG_GAIN;
        println!("Genesis: \"{}\", {} KB, {:?}{}", header.title, rom.len() / 1024, header.region,
                 header.sram.map(|(s, e)| format!(", backup RAM {s:06X}-{e:06X}")).unwrap_or_default());
        let mut sys = Self {
            cpu: M68000::new(),
            z80: Z80Cpu::new(),
            bus: MdBus::new(rom, header.sram, version, psg),
            m68k_owed: 0, z80_owed: 0, z80_int: false,
            ym_mclk: 0, fm_out: Vec::new(), fm_acc: (0.0, 0.0, 0), fm_phase: 0, current_frame: 0,
            title: header.title,
            save_filename: filename.to_string(),
            save_battery_path: "user://GD_EMU/Genesis/Save".to_string(),
            save_state_path: "user://GD_EMU/Genesis/State".to_string(),
        };
        sys.cpu.reset(&mut sys.bus);
        Ok(sys)
    }

    /// Run the 68000 until it has caught up to `until` master clocks into the line.
    fn run_68k(&mut self, budget: i64, line_start: i64) {
        self.m68k_owed += budget;
        while self.m68k_owed > 0 {
            if self.bus.stall > 0 {
                self.m68k_owed -= self.bus.stall as i64 * M68K_DIV;
                self.bus.stall = 0;
                continue;
            }
            let pos = line_start + budget - self.m68k_owed;
            self.bus.vdp.line_pos = (pos as f32 / MCLK_PER_LINE as f32).clamp(0.0, 0.999);
            self.cpu.ipl = self.bus.vdp.ipl();
            if self.cpu.halted { self.m68k_owed = 0; break; }
            let c = self.cpu.step(&mut self.bus) as i64;
            self.m68k_owed -= c * M68K_DIV;
        }
    }

    fn run_z80(&mut self) {
        self.z80_owed += MCLK_PER_LINE;
        let mut clocks = 0u32;
        while self.z80_owed > 0 {
            if self.bus.zreset_released {
                self.bus.zreset_released = false;
                self.z80 = Z80Cpu::new();
            }
            let c = if self.bus.z80_running() {
                let mut c = 0;
                if self.z80_int { c = self.z80.irq(&mut Z80View(&mut self.bus), 0xFF); }
                if c == 0 { c = self.z80.step(&mut Z80View(&mut self.bus)); }
                c
            } else {
                4
            };
            clocks += c;
            self.z80_owed -= c as i64 * Z80_DIV;
            self.ym_mclk += c as i64 * Z80_DIV;
            while self.ym_mclk >= YM_MCLK {
                self.ym_mclk -= YM_MCLK;
                self.ym_sample();
            }
        }
        self.bus.psg.run(clocks);
    }

    /// One YM2612 sample, averaged down to 44.1 kHz.
    fn ym_sample(&mut self) {
        let (l, r) = self.bus.ym.generate();
        let a = &mut self.fm_acc;
        a.0 += l as f32;
        a.1 += r as f32;
        a.2 += 1;
        self.fm_phase += SAMPLE_RATE as u64 * YM_MCLK as u64;
        if self.fm_phase >= MCLK {
            self.fm_phase -= MCLK;
            let n = a.2 as f32;
            self.fm_out.push(a.0 / n * FM_LEVEL);
            self.fm_out.push(a.1 / n * FM_LEVEL);
            *a = (0.0, 0.0, 0);
        }
    }
}

impl System for GenesisSystem {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        for line in 0..LINES {
            self.bus.vdp.begin_line(line);
            if line == VBLANK_LINE { self.z80_int = true; }
            if line == VBLANK_LINE + 1 { self.z80_int = false; }
            self.run_68k(HBLANK_MCLK, 0);
            self.bus.vdp.render_line(line as usize);
            self.run_68k(MCLK_PER_LINE - HBLANK_MCLK, HBLANK_MCLK);
            self.run_z80();
        }
        self.current_frame += 1;
    }

    fn set_input(&mut self, m: u32) {
        let pad_bits = |m: u32, a: u32, b: u32, c: u32, start: bool| -> u8 {
            let on = |bit: u32| m & bit != 0;
            let mut v = 0;
            if on(pad::UP) { v |= PAD_UP; }
            if on(pad::DOWN) { v |= PAD_DOWN; }
            if on(pad::LEFT) { v |= PAD_LEFT; }
            if on(pad::RIGHT) { v |= PAD_RIGHT; }
            if on(a) { v |= PAD_A; }
            if on(b) { v |= PAD_B; }
            if on(c) { v |= PAD_C; }
            if start { v |= PAD_START; }
            v
        };
        self.bus.io.pads[0].buttons = pad_bits(m, pad::X, pad::B, pad::A, m & pad::START != 0);
        let p2 = pad::player2(m);
        self.bus.io.pads[1].buttons = pad_bits(p2, pad::SELECT, pad::B, pad::A, m & pad::START2 != 0);
    }

    /// FM + PSG. Both run at 44.1 kHz of emulated time; whatever one has beyond the other
    /// waits for the next frame.
    fn drain_audio(&mut self) -> Vec<f32> {
        let psg = &mut self.bus.psg.out;
        let n = self.fm_out.len().min(psg.len());
        let mix: Vec<f32> = self.fm_out[..n].iter().zip(&psg[..n]).map(|(f, p)| (f + p).clamp(-1.0, 1.0)).collect();
        self.fm_out.drain(..n);
        psg.drain(..n);
        // never let a backlog build up
        if self.fm_out.len() > 4096 { self.fm_out.clear(); }
        if psg.len() > 4096 { psg.clear(); }
        mix
    }
    fn sample_rate(&self) -> u32 { SAMPLE_RATE }
    fn power_on(&mut self) {}
    fn power_off(&mut self) {}
    fn has_battery(&self) -> bool { !self.bus.sram.is_empty() }
    fn save_filename(&self) -> &str { &self.save_filename }
    fn save_battery_path(&self) -> &str { &self.save_battery_path }
    fn save_state_path(&self) -> &str { &self.save_state_path }
    fn is_sram_dirty(&self) -> bool { self.bus.sram_dirty }
    fn clear_sram_dirty(&mut self) { self.bus.sram_dirty = false; }
    fn get_current_frame(&self) -> i64 { self.current_frame as i64 }
    fn get_sram(&self) -> Option<&[u8]> { if self.bus.sram.is_empty() { None } else { Some(&self.bus.sram) } }
    fn load_sram(&mut self, data: &[u8]) {
        let n = data.len().min(self.bus.sram.len());
        self.bus.sram[..n].copy_from_slice(&data[..n]);
    }

    fn framebuffer(&self) -> &[u8] { &self.bus.vdp.framebuffer }

    /// The console's reset button: 68000 and Z80, not the VDP.
    fn reset(&mut self) {
        self.bus.zreset = true;
        self.bus.zbusreq = false;
        self.bus.ym.reset();
        self.z80 = Z80Cpu::new();
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
        let s = MdState {
            version: STATE_VERSION, rom_len: b.rom.len(),
            cpu: self.cpu.clone(), z80: self.z80.save_state(), vdp: b.vdp.clone(),
            ram: b.ram.clone(), zram: b.zram.clone(), zbank: b.zbank, zbusreq: b.zbusreq, zreset: b.zreset,
            io: b.io.clone(), psg: b.psg.clone(), ym: b.ym.clone(), ym_mclk: self.ym_mclk, sram: b.sram.clone(), sram_on: b.sram_on, banks: b.banks,
            m68k_owed: self.m68k_owed, z80_owed: self.z80_owed, z80_int: self.z80_int, frame: self.current_frame,
        };
        let config = bincode::config::standard().with_fixed_int_encoding();
        bincode::serde::encode_to_vec(&s, config).unwrap_or_default()
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let config = bincode::config::standard().with_fixed_int_encoding();
        let (s, _): (MdState, usize) = bincode::serde::decode_from_slice(data, config)
            .map_err(|e| format!("Failed to decode Genesis save state: {e:?}"))?;
        if s.version != STATE_VERSION {
            return Err(format!("save state version {} (expected {})", s.version, STATE_VERSION));
        }
        if s.rom_len != self.bus.rom.len() { return Err("save state is from a different game".into()); }
        let b = &mut self.bus;
        self.cpu = s.cpu;
        self.z80.load_state(&s.z80);
        b.vdp.load_state(s.vdp);
        b.ram = s.ram;
        b.zram = s.zram;
        b.zbank = s.zbank;
        b.zbusreq = s.zbusreq;
        b.zreset = s.zreset;
        b.zreset_released = false;
        let pads = b.io.pads;
        b.io = s.io;
        b.io.pads = pads;
        b.psg.load_state(s.psg);
        b.ym = s.ym;
        self.ym_mclk = s.ym_mclk;
        if s.sram.len() == b.sram.len() { b.sram = s.sram; }
        b.sram_on = s.sram_on;
        b.banks = s.banks;
        b.stall = 0;
        self.m68k_owed = s.m68k_owed;
        self.z80_owed = s.z80_owed;
        self.z80_int = s.z80_int;
        self.current_frame = s.frame;
        Ok(())
    }
}
