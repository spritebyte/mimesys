//! Sega Genesis / Mega Drive (1988).
//!
//! 68000 at MCLK / 7 (7.67 MHz), Z80 at MCLK / 15 (3.58 MHz) with 8 KB of its own RAM,
//! the VDP (md_vdp.rs), an SN76489 PSG inside the VDP and a YM2612 FM chip (ym2612.rs, one
//! sample every 1008 master clocks = 53.3 kHz, stepped along with the Z80 so its DAC
//! streams play at their real rate). NTSC: 262 lines of 3420 master clocks, 59.92 Hz.
//! PAL (European carts, or forced): 313 lines, master clock 53.203424 MHz, 49.70 Hz.
//!
//! Interrupts: VDP level 6 at vblank, level 4 from the line counter (both autovectored);
//! the Z80 gets /INT for about 64 us at vblank. The Z80 is kept in step with the 68000
//! instruction by instruction (Sonic 2's music driver needs its vblank interrupt while
//! the 68000's vblank handler is busy taking the Z80 bus).
//!
//! Controls (6-button pad; a 3-button pad as an option):
//!   D-pad      D-pad
//!   X [J]      A        B [K]  B        A [L]  C        Start  Start
//!   Y [U]      X        L [Q]  Y        R [E]  Z        Select Mode
//!   player 2:  Select = A, B = B, A = C, P2 Start = Start (3 buttons)
//!
//! Load options travel in the "BIOS" bytes: b"MDOP", region (0 auto, 1 USA, 2 Japan,
//! 3 Europe), pad (0 six buttons, 1 three buttons).

use serde::{Deserialize, Serialize};
use crate::common::audio::sn76489::{Sn76489, Sn76489Variant};
use crate::common::audio::ym2612::Ym2612;
use crate::common::cpu::m68000::M68000;
use crate::common::cpu::z80::{Z80Cpu, Z80State};
use crate::common::input as pad;
use crate::common::system::{DisplayInfo, Rotation, System};
use crate::genesis::md_bus::*;
use crate::genesis::md_cart::{self, Region};
use crate::genesis::md_vdp::{Vdp, BUFFER_H, SCREEN_W};

const MCLK_PER_LINE: i64 = 3420;
const M68K_DIV: i64 = 7;
const Z80_DIV: i64 = 15;
/// master clocks into the line before the VDP draws it (the hblank part)
const HBLANK_MCLK: i64 = 560;
const SAMPLE_RATE: u32 = 44_100;
const STATE_VERSION: u32 = 9;
/// how long the Z80's vblank /INT stays asserted (about 64 us, as on the console)
const Z80_INT_MCLK: i64 = 2565;
const MCLK_NTSC: u64 = 53_693_175;
const MCLK_PAL: u64 = 53_203_424;
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
    /// where the 68000 / Z80 are in the current line, in master clocks (may run a little
    /// past the end of the line; the excess carries into the next)
    t68: i64,
    tz80: i64,
    /// where the Sega CD's sub CPU is in the line (master clocks)
    tmcd: i64,
    /// where the 32X's SH-2s are in the line
    t32x: i64,
    /// the Z80's /INT is asserted until tz80 reaches this
    z80_int_end: i64,
    /// master clocks into the next YM2612 sample
    ym_mclk: i64,
    /// FM resampled to 44.1 kHz (interleaved stereo), waiting to be mixed with the PSG
    fm_out: Vec<f32>,
    fm_acc: (f32, f32, u32),
    fm_phase: u64,
    /// master clock rate (NTSC or PAL)
    mclk: u64,
    current_frame: u64,
    /// print a state report now and then (Sega CD bug reports)
    debug: bool,
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
    eeprom: Option<crate::genesis::md_eeprom::Eeprom>,
    sram_on: bool,
    banks: [u8; 8],
    t68: i64,
    tz80: i64,
    z80_int_end: i64,
    frame: u64,
    /// the Sega CD unit (sub CPU + gate array + its RAM), when there is one
    mcd: Option<(M68000, crate::segacd::mcd::State)>,
    tmcd: i64,
    m32x: Option<([crate::m32x::sh7604::Sh7604; 2], crate::m32x::State)>,
    t32x: i64,
}

impl GenesisSystem {
    pub fn from_rom(data: &[u8], filename: &str, options: &[u8]) -> Result<Self, String> {
        let mut rom = md_cart::normalize(data);
        if rom.len() < 0x200 { return Err(format!("ROM is only {} bytes", rom.len())); }
        if rom.len() % 2 == 1 { rom.push(0xFF); }
        let header = md_cart::parse_header(&rom);
        let (force_region, three_button) = match options {
            [b'M', b'D', b'O', b'P', r, p, ..] => (*r, *p == 1),
            _ => (0, false),
        };
        let region = match force_region { 1 => Region::Usa, 2 => Region::Japan, 3 => Region::Europe, _ => header.region };
        let pal = region == Region::Europe;
        let version = match region { Region::Usa => 0xA0, Region::Japan => 0x20, Region::Europe => 0xE0 };
        let mclk = if pal { MCLK_PAL } else { MCLK_NTSC };
        let mut psg = Sn76489::new(Sn76489Variant::Sega, (mclk / Z80_DIV as u64) as u32, SAMPLE_RATE);
        psg.gain = PSG_GAIN;
        let mut bus = MdBus::new(rom, header.sram, version, psg);
        bus.vdp.pal = pal;
        for p in bus.io.pads.iter_mut() { p.six_button = !three_button; }
        println!("Genesis: \"{}\", {} KB, {:?}{}{}{}", header.title, bus.rom.len() / 1024, region,
                 if pal { " (PAL, 50 Hz)" } else { "" },
                 header.sram.filter(|_| bus.eeprom.is_none()).map(|(s, e)| format!(", backup RAM {s:06X}-{e:06X}")).unwrap_or_default(),
                 bus.eeprom.as_ref().map(|e| format!(", serial EEPROM ({:?} board, {} bytes)", e.board, e.data.len())).unwrap_or_default());
        let mut sys = Self {
            cpu: M68000::new(),
            z80: Z80Cpu::new(),
            bus,
            t68: 0, tz80: 0, tmcd: 0, t32x: 0, z80_int_end: 0,
            ym_mclk: 0, fm_out: Vec::new(), fm_acc: (0.0, 0.0, 0), fm_phase: 0, mclk, current_frame: 0, debug: false,
            title: header.title,
            save_filename: filename.to_string(),
            save_battery_path: "user://GD_EMU/Genesis/Save".to_string(),
            save_state_path: "user://GD_EMU/Genesis/State".to_string(),
        };
        sys.cpu.reset(&mut sys.bus);
        Ok(sys)
    }

    /// A Sega CD: the BIOS (128 KB) boots, the disc is in the drive. `region`: 'U', 'E', 'J'.
    pub fn from_cd(bios: Vec<u8>, disc: crate::common::disc::Disc, region: char, title: &str, filename: &str) -> Result<Self, String> {
        let r = match region { 'J' => 2, 'E' => 3, _ => 1 };
        let mut sys = Self::from_rom(&bios, filename, &[b'M', b'D', b'O', b'P', r, 0])?;
        sys.bus.io.version &= !0x20; // bit 5 clear: an expansion unit is connected
        sys.bus.mcd = Some(Box::new(crate::segacd::mcd::Mcd::new(bios, Some(disc))));
        sys.title = title.to_string();
        sys.save_battery_path = "user://GD_EMU/SegaCD/Save".to_string();
        sys.save_state_path = "user://GD_EMU/SegaCD/State".to_string();
        sys.cpu = M68000::new();
        sys.cpu.reset(&mut sys.bus);
        Ok(sys)
    }

    /// Where the CPUs and the CD drive are (printed to the console).
    fn debug_report(&self) {
        let c = &self.cpu;
        crate::dlog!("=== Genesis debug, frame {} ===", self.current_frame);
        crate::dlog!("68K  PC {:06X} SR {:04X} D0 {:08X} A7 {:08X}  stopped {} halted {}", c.pc.wrapping_sub(4), c.sr, c.r[0], c.r[15], c.stopped as u8, c.halted as u8);
        if let Some(x) = &self.bus.m32x {
            for (n, s) in x.sh.iter().enumerate() {
                let c = &s.cpu;
                crate::dlog!("SH2{} PC {:08X} SR {:03X} R0 {:08X} R15 {:08X} PR {:08X}  cache {}", ["M", "S"][n], c.instruction_pc(), c.sr.get(), c.r[0], c.r[15], c.pr, s.p.cache.enable as u8);
            }
            let st = &x.st;
            crate::dlog!("32X  adapter {} run {} RV {} FM {}  comm {:04X?}  VDP mode {} bank {}  DREQ {} fifo {}",
                         st.io.adapter_enable as u8, st.io.adapter_reset as u8, st.dreq.vram as u8, st.vdp.framebuffer_access as u8,
                         st.comm, st.vdp.mode, st.vdp.framebuffer_active as u8, st.dreq.active as u8, st.dreq.fifo.len());
        }
        if let Some(m) = &self.bus.mcd {
            let (s, st) = (&m.cpu, &m.st);
            crate::dlog!("SUB  PC {:06X} SR {:04X} D0 {:08X} A7 {:08X}  stopped {} halt {} run {} busreq {}",
                     s.pc.wrapping_sub(4), s.sr, s.r[0], s.r[15], s.stopped as u8, st.io.halt as u8, st.io.run as u8, st.io.request as u8);
            let irq: String = (1..=6).map(|l| if st.irq[l].pending { '*' } else if st.irq[l].enable { '+' } else { '.' }).collect();
            crate::dlog!("     IRQ 1-6 {irq} (+ on, * pending)  timer {:02X}  word RAM {} switch {} select {}",
                     st.timer_frequency, if st.io.wram_mode { "1M" } else { "2M" }, st.io.wram_switch as u8, st.io.wram_select as u8);
            crate::dlog!("     comm CFM {:02X} CFS {:02X} cmd {:04X?} status {:04X?}", st.cfm, st.cfs, st.command, st.status);
            let d = &st.cdd;
            crate::dlog!("CDD  state {:X} sector {} track {} host clock {}  cmd {:X?} status {:X?}", d.state, d.sector, d.track, d.host_clock as u8, d.command, d.status);
            let t = &st.cdc.transfer;
            crate::dlog!("CDC  decoder {} store {} dest {} active {} len {:03X} src {:04X} ptr {:04X} wa {:04X} head {:02X?}",
                     st.cdc.decoder_enable as u8, (st.cdc.ctrl0 >> 2) & 1, t.destination, t.active as u8, t.length, t.source, t.pointer, t.target, st.cdc.header);
            let g = &st.gpu;
            crate::dlog!("GPU  busy {} size {}x{} dots, stamps {} map {} repeat {}", g.active as u8, g.hdots, g.vdots,
                         if g.tile_size { 32 } else { 16 }, if g.map_size { "16x16" } else { "1x1" }, g.repeat as u8);
        }
    }

    /// A 32X game: the cartridge plus the 32X. `bios`: an "MARB" bundle with the three
    /// BIOS files and "mdop" (the Genesis load options).
    pub fn from_32x(rom: &[u8], filename: &str, bios: &[u8]) -> Result<Self, String> {
        let x = crate::m32x::M32x::new(bios)?;
        let opts = crate::common::firmware::files(bios, crate::m32x::BIOS_MAGIC).into_iter()
            .find(|(n, _)| n == "mdop").map(|(_, d)| d).unwrap_or_default();
        let mut sys = Self::from_rom(rom, filename, &opts)?;
        sys.bus.m32x = Some(Box::new(x));
        sys.save_battery_path = "user://GD_EMU/32X/Save".to_string();
        sys.save_state_path = "user://GD_EMU/32X/State".to_string();
        println!("32X: BIOS found; SH-2s on");
        Ok(sys)
    }

    /// The 32X's SH-2s catch up to `t` master clocks into the line.
    fn m32x_until(&mut self, t: i64) {
        if let Some(x) = &mut self.bus.m32x {
            if t > self.t32x { x.run(t - self.t32x, &self.bus.rom); }
        }
        self.t32x = self.t32x.max(t);
    }

    /// The Sega CD's sub CPU catches up to `t` master clocks into the line.
    fn mcd_until(&mut self, t: i64) {
        if let Some(m) = &mut self.bus.mcd {
            if t > self.tmcd { m.run(t - self.tmcd, self.mclk as i64); }
        }
        self.tmcd = self.tmcd.max(t);
    }

    /// Run both CPUs to `target` master clocks into the line. The Z80 catches up after
    /// every 68000 instruction, so bus requests, resets and the shared RAM see each
    /// other's writes at (nearly) the right time.
    fn run_until(&mut self, target: i64) {
        while self.t68 < target {
            if self.bus.stall > 0 {
                // DMA holds the 68000; the Z80 keeps going
                self.t68 += self.bus.stall as i64 * M68K_DIV;
                self.bus.stall = 0;
            } else if self.cpu.halted {
                self.t68 = target;
            } else {
                self.bus.vdp.line_pos = (self.t68 as f32 / MCLK_PER_LINE as f32).clamp(0.0, 0.999);
                self.cpu.ipl = self.bus.vdp.ipl();
                self.t68 += self.cpu.step(&mut self.bus) as i64 * M68K_DIV;
            }
            self.z80_until(self.t68);
            self.mcd_until(self.t68);
            self.m32x_until(self.t68);
        }
    }

    fn z80_until(&mut self, t: i64) {
        while self.tz80 < t {
            if self.bus.zreset_released {
                self.bus.zreset_released = false;
                self.z80 = Z80Cpu::new();
            }
            let c = if self.bus.z80_running() {
                let mut c = 0;
                if self.tz80 < self.z80_int_end { c = self.z80.irq(&mut Z80View(&mut self.bus), 0xFF); }
                if c == 0 { c = self.z80.step(&mut Z80View(&mut self.bus)); }
                c
            } else {
                4
            };
            self.tz80 += c as i64 * Z80_DIV;
            self.bus.psg.run(c);
            self.ym_mclk += c as i64 * Z80_DIV;
            while self.ym_mclk >= YM_MCLK {
                self.ym_mclk -= YM_MCLK;
                self.ym_sample();
            }
        }
    }

    /// One YM2612 sample, averaged down to 44.1 kHz.
    fn ym_sample(&mut self) {
        let (l, r) = self.bus.ym.generate();
        let a = &mut self.fm_acc;
        a.0 += l as f32;
        a.1 += r as f32;
        a.2 += 1;
        self.fm_phase += SAMPLE_RATE as u64 * YM_MCLK as u64;
        if self.fm_phase >= self.mclk {
            self.fm_phase -= self.mclk;
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
        for line in 0..self.bus.vdp.lines() {
            self.bus.vdp.begin_line(line);
            self.bus.io.now = self.bus.io.now.wrapping_add(1);
            if line == self.bus.vdp.vblank_line() { self.z80_int_end = Z80_INT_MCLK; }
            if let Some(x) = &mut self.bus.m32x {
                // the 32X follows the Genesis's blanking: vblank from the vblank line to line 0,
                // hblank for the first part of each line here
                if line == self.bus.vdp.vblank_line() { x.vblank(true); }
                if line == 0 { x.vblank(false); }
                x.hblank(true);
            }
            self.run_until(HBLANK_MCLK);
            if let Some(x) = &mut self.bus.m32x { x.hblank(false); }
            self.bus.vdp.render_line(line as usize);
            self.run_until(MCLK_PER_LINE);
            self.t68 -= MCLK_PER_LINE;
            self.tz80 -= MCLK_PER_LINE;
            self.tmcd -= MCLK_PER_LINE;
            self.t32x -= MCLK_PER_LINE;
            self.z80_int_end = (self.z80_int_end - MCLK_PER_LINE).max(0);
        }
        self.bus.apply_cheat_writes();
        self.current_frame += 1;
        let f = self.current_frame;
        if self.debug && ((f <= 600 && f % 60 == 0) || f % 600 == 0) { self.debug_report(); }
    }

    fn set_option(&mut self, key: &str, value: i64) {
        if key == "debug_report" {
            self.debug = value != 0;
            if let Some(m) = &mut self.bus.mcd { m.st.cdd.log = self.debug; }
        }
    }

    fn set_input(&mut self, m: u32) {
        let pad_bits = |m: u32, a: u32, b: u32, c: u32, start: bool| -> u16 {
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
        let mut p1 = pad_bits(m, pad::X, pad::B, pad::A, m & pad::START != 0);
        for (bit, b) in [(pad::Y, PAD_X), (pad::L, PAD_Y), (pad::R, PAD_Z), (pad::SELECT, PAD_MODE)] {
            if m & bit != 0 { p1 |= b; }
        }
        self.bus.io.pads[0].buttons = p1;
        let p2 = pad::player2(m);
        self.bus.io.pads[1].buttons = pad_bits(p2, pad::SELECT, pad::B, pad::A, m & pad::START2 != 0);
    }

    /// FM + PSG. Both run at 44.1 kHz of emulated time; whatever one has beyond the other
    /// waits for the next frame.
    fn drain_audio(&mut self) -> Vec<f32> {
        let psg = &mut self.bus.psg.out;
        let mut n = self.fm_out.len().min(psg.len());
        // the Sega CD's sound (CD audio + PCM), also 44.1 kHz stereo
        let mut cd = self.bus.mcd.as_mut().map(|m| &mut m.st.audio);
        if let Some(c) = &cd { n = n.min(c.len()); }
        let mut mix: Vec<f32> = self.fm_out[..n].iter().zip(&psg[..n]).map(|(f, p)| f + p).collect();
        if let Some(c) = &mut cd {
            for (m, x) in mix.iter_mut().zip(c.iter()) { *m += x; }
            c.drain(..n);
        }
        for m in mix.iter_mut() { *m = m.clamp(-1.0, 1.0); }
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
    fn cheats_mut(&mut self) -> Option<&mut crate::common::cheats::Cheats> { Some(&mut self.bus.cheats) }
    fn has_battery(&self) -> bool { self.bus.mcd.is_some() || !self.bus.sram.is_empty() || self.bus.eeprom.is_some() }
    fn save_filename(&self) -> &str { &self.save_filename }
    fn save_battery_path(&self) -> &str { &self.save_battery_path }
    fn save_state_path(&self) -> &str { &self.save_state_path }
    fn is_sram_dirty(&self) -> bool { self.bus.mcd.as_ref().is_some_and(|m| m.st.bram_dirty) || self.bus.sram_dirty || self.bus.eeprom.as_ref().is_some_and(|e| e.dirty) }
    fn clear_sram_dirty(&mut self) {
        self.bus.sram_dirty = false;
        if let Some(m) = &mut self.bus.mcd { m.st.bram_dirty = false; }
        if let Some(e) = &mut self.bus.eeprom { e.dirty = false; }
    }
    fn get_current_frame(&self) -> i64 { self.current_frame as i64 }
    fn get_sram(&self) -> Option<&[u8]> {
        if let Some(m) = &self.bus.mcd { return Some(&m.st.bram); } // the CD unit's backup RAM
        if let Some(e) = &self.bus.eeprom { return Some(&e.data); }
        if self.bus.sram.is_empty() { None } else { Some(&self.bus.sram) }
    }
    fn load_sram(&mut self, data: &[u8]) {
        if let Some(m) = &mut self.bus.mcd {
            let n = data.len().min(m.st.bram.len());
            m.st.bram[..n].copy_from_slice(&data[..n]);
            // a blank save (from before backup RAM came formatted): format it
            if !crate::segacd::mcd::bram_formatted(&m.st.bram) { crate::segacd::mcd::format_bram(&mut m.st.bram); }
            return;
        }
        let dest = match &mut self.bus.eeprom { Some(e) => &mut e.data, None => &mut self.bus.sram };
        let n = data.len().min(dest.len());
        dest[..n].copy_from_slice(&data[..n]);
    }

    fn frame_time_usec(&self) -> i64 {
        (self.bus.vdp.lines() as u64 * MCLK_PER_LINE as u64 * 1_000_000 / self.mclk) as i64
    }

    fn framebuffer(&self) -> &[u8] { &self.bus.vdp.framebuffer }

    /// The console's reset button: 68000 and Z80, not the VDP.
    fn reset(&mut self) {
        self.bus.zreset = true;
        self.bus.zbusreq = false;
        self.bus.ym.reset();
        if let Some(e) = &mut self.bus.eeprom { e.rom_mapped = true; }
        if let Some(m) = &mut self.bus.mcd { m.reset(); }
        if let Some(x) = &mut self.bus.m32x { x.reset(); }
        self.z80 = Z80Cpu::new();
        self.cpu = M68000::new();
        self.cpu.reset(&mut self.bus);
    }

    fn display_info(&self) -> DisplayInfo {
        DisplayInfo {
            buffer_width: SCREEN_W as i32,
            buffer_height: BUFFER_H as i32,
            visible_x: 0,
            visible_y: 0,
            visible_width: SCREEN_W as i32,
            visible_height: self.bus.vdp.frame_height() as i32,
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
            io: b.io.clone(), psg: b.psg.clone(), ym: b.ym.clone(), ym_mclk: self.ym_mclk, sram: b.sram.clone(), eeprom: b.eeprom.clone(), sram_on: b.sram_on, banks: b.banks,
            t68: self.t68, tz80: self.tz80, z80_int_end: self.z80_int_end, frame: self.current_frame,
            mcd: b.mcd.as_ref().map(|m| (m.cpu.clone(), m.st.clone())), tmcd: self.tmcd,
            m32x: b.m32x.as_ref().map(|x| (x.sh.clone(), x.st.clone())), t32x: self.t32x,
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
        if b.eeprom.is_some() && s.eeprom.is_some() { b.eeprom = s.eeprom; }
        b.sram_on = s.sram_on;
        b.banks = s.banks;
        b.stall = 0;
        self.t68 = s.t68;
        self.tz80 = s.tz80;
        self.tmcd = s.tmcd;
        self.t32x = s.t32x;
        if let (Some(x), Some((sh, st))) = (&mut b.m32x, s.m32x) {
            let roms = (std::mem::take(&mut x.st.vectors), std::mem::take(&mut x.st.mrom), std::mem::take(&mut x.st.srom));
            x.sh = sh;
            x.st = st;
            (x.st.vectors, x.st.mrom, x.st.srom) = roms;
        }
        if let (Some(m), Some((cpu, st))) = (&mut b.mcd, s.mcd) {
            let bios = std::mem::take(&mut m.st.bios);
            m.cpu = cpu;
            m.st = st;
            m.st.bios = bios;
        }
        self.z80_int_end = s.z80_int_end;
        self.current_frame = s.frame;
        Ok(())
    }
}
