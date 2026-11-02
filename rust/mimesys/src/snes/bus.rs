//! The SNES main bus: memory map, access timing, the 5A22's own registers (interrupts,
//! timers, multiply / divide, joypads, DMA / HDMA), and the PPU / APU ports.
//!
//! Time is in master clocks (21.477 MHz). A CPU access takes 6, 8 or 12 of them by
//! address (fast ROM 6 when MEMSEL is set; I/O 6; 4000-41FF 12; the rest 8), an internal
//! cycle 6 (ares's CPU::wait). A line is 1364 clocks, a frame 262 lines (NTSC); the DRAM
//! refresh steals 40 clocks a line. Events by position in the line:
//!   H 0      new line; line 0 starts HDMA (reload), line 225 starts vblank (NMI)
//!   H 512    the line is drawn (rows are lines 1-224): register writes in the first
//!            half of a line show on it, later ones from the next line (as ares's
//!            scanline renderer; a few games want it earlier, see `render_point`)
//!   H 1104   HDMA writes for the next line (lines 0-224)
//!   H/V IRQ  at HTIME dots (x4 clocks) on matching lines
//!
//! Simplifications for stage 1: DMA runs as soon as MDMAEN is written; multiply / divide
//! answers are ready at once; the auto joypad read happens at the start of vblank.

use serde::{Deserialize, Serialize};
use crate::common::cpu::w65816::Bus816;
use crate::snes::cart::Cart;
use crate::snes::ppu::{Ppu, HEIGHT, WIDTH};
use crate::snes::smp::Smp;
use crate::snes::superfx::{self, Gsu};
use crate::snes::sa1::Sa1;
use crate::snes::necdsp::{NecDsp, NecMap};
use crate::snes::cx4::{self, Cx4};
use crate::common::cheats::{Cheats, CheatSystem};

pub const LINE_CLOCKS: u32 = 1364;
pub const LINES: u32 = 262;
const VDISP: u32 = 225;
const HDMA_AT: u32 = 1104;
const REFRESH_AT: u32 = 538;

#[derive(Clone, Copy, Default, Debug, Serialize, Deserialize)]
pub struct Channel {
    pub dmap: u8,
    pub bbad: u8,
    pub a1t: u16,
    pub a1b: u8,
    /// DAS: the DMA byte count, or HDMA's indirect address
    pub das: u16,
    pub dasb: u8,
    pub a2a: u16,
    pub ntrl: u8,
    pub unused: u8,
    hdma_done: bool,
    hdma_do: bool,
}

fn default_render_at() -> u32 { 512 }

/// Where in the line to draw it: mid-line, earlier for the games ares lists as needing it.
fn render_point(title: &str) -> u32 {
    match title {
        "ADVENTURES OF FRANKEN" | "AIR STRIKE PATROL" | "DESERT FIGHTER" | "FIREPOWER 2000" | "SUPER SWIV"
        | "NHL '94" | "NHL PROHOCKEY'94" => 32,
        "Sugoro Quest++" => 128,
        _ => 512,
    }
}

impl Channel {
    /// The DMA registers read FF after power on (a stray transfer then goes to 21FF,
    /// where nothing answers).
    fn power_on() -> Self {
        Self { dmap: 0xFF, bbad: 0xFF, a1t: 0xFFFF, a1b: 0xFF, das: 0xFFFF, dasb: 0xFF, a2a: 0xFFFF, ntrl: 0xFF, unused: 0xFF,
               hdma_done: false, hdma_do: false }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Bus {
    #[serde(with = "serde_bytes")]
    pub wram: Vec<u8>,
    pub cart: Cart,
    pub ppu: Ppu,
    pub apu: Smp,
    /// Super FX on the cartridge
    pub gsu: Option<Gsu>,
    /// SA-1 on the cartridge
    pub sa1: Option<Box<Sa1>>,
    /// NEC µPD7725 DSP on the cartridge, and where it answers
    pub nec: Option<Box<NecDsp>>,
    #[serde(default)]
    pub nec_map: Option<NecMap>,
    /// Cx4 (Mega Man X2 / X3), when the cartridge has one and its firmware was found
    pub cx4: Option<Box<Cx4>>,
    /// the CPU's last bus address (the SA-1 waits when both want the same memory)
    #[serde(default)]
    mar: u32,
    /// where in a line it is drawn (master clocks)
    #[serde(default = "default_render_at")]
    render_at: u32,
    /// channels whose general DMA an HDMA has cut short
    #[serde(default)]
    hdma_kill: u8,
    pub dma: [Channel; 8],
    // 5A22 registers
    pub nmitimen: u8,
    pub wrio: u8,
    wrmpya: u8,
    wrdiv: u16,
    pub htime: u16,
    pub vtime: u16,
    pub hdmaen: u8,
    pub memsel: u8,
    rddiv: u16,
    rdmpy: u16,
    pub joy: [u16; 4],
    wram_addr: u32,
    // interrupt state
    pub nmi_flag: bool,
    pub nmi_pending: bool,
    pub irq_flag: bool,
    // joypads: what's held (bit 15 B ... bit 4 R), and the serial port shifters
    pub pads: [u16; 2],
    pad_latch: bool,
    pad_shift: [u32; 2],
    autojoy_busy_until: u32,
    // time
    pub h: u32,
    pub v: u32,
    pub frame: u64,
    #[serde(skip)]
    pub frame_done: bool,
    refreshed: bool,
    pub mdr: u8,
    #[serde(skip)]
    pub framebuffer: Vec<u8>,
    /// debug report: the last CPU writes to the APU ports (frame, line, port, value)
    #[serde(skip)]
    pub port_log: Vec<(u32, u16, u8, u8)>,
    #[serde(skip)]
    pub port_log_on: bool,
    /// debug report: PPU register writes this frame (line, H clock, register, value);
    /// register 0 = the H/V IRQ fired
    #[serde(skip)]
    pub ppu_log: Vec<(u16, u16, u8, u8)>,
    #[serde(skip)]
    pub ppu_log_last: Vec<(u16, u16, u8, u8)>,
    /// master clocks since power on
    pub clocks: u64,
    /// cheat codes (Game Genie patches ROM reads, Pro Action Replay writes RAM each frame)
    #[serde(skip, default = "no_cheats")]
    pub cheats: Cheats,
}

fn no_cheats() -> Cheats { Cheats::new(CheatSystem::Snes) }

impl Bus {
    pub fn new(cart: Cart) -> Self {
        let gsu = (cart.map == crate::snes::cart::Map::SuperFx).then(Gsu::new);
        let sa1 = (cart.map == crate::snes::cart::Map::Sa1).then(|| Box::new(Sa1::new()));
        let render_at = render_point(&cart.title);
        Self {
            wram: vec![0x55; 0x20000], cart, ppu: Ppu::new(), apu: Smp::new(), gsu, sa1, nec: None, nec_map: None, cx4: None, mar: 0, render_at, hdma_kill: 0, dma: [Channel::power_on(); 8],
            nmitimen: 0, wrio: 0xFF, wrmpya: 0xFF, wrdiv: 0xFFFF, htime: 0x1FF, vtime: 0x1FF, hdmaen: 0, memsel: 0,
            rddiv: 0, rdmpy: 0, joy: [0; 4], wram_addr: 0,
            nmi_flag: false, nmi_pending: false, irq_flag: false,
            pads: [0; 2], pad_latch: false, pad_shift: [0; 2], autojoy_busy_until: 0,
            h: 0, v: 0, frame: 0, frame_done: false, refreshed: false, mdr: 0,
            framebuffer: vec![0; WIDTH * HEIGHT * 4], clocks: 0, port_log: Vec::new(), port_log_on: false, ppu_log: Vec::new(), ppu_log_last: Vec::new(),
            cheats: no_cheats(),
        }
    }

    fn in_display(&self) -> bool { self.v >= 1 && self.v < VDISP }

    /// Access time in master clocks (ares CPU::wait).
    fn wait(&self, a: u32) -> u32 {
        if a & 0x408000 != 0 { return if a & 0x800000 != 0 && self.memsel & 1 != 0 { 6 } else { 8 }; }
        if (a.wrapping_add(0x6000)) & 0x4000 != 0 { return 8; }
        if (a.wrapping_sub(0x4000)) & 0x7E00 != 0 { return 6; }
        12
    }

    // ---------------- time ----------------

    /// Advance `n` master clocks, running the line events that fall in them.
    pub fn tick(&mut self, mut n: u32) {
        while n > 0 {
            // the next event in this line
            let mut next = LINE_CLOCKS;
            for at in [REFRESH_AT, self.render_at, HDMA_AT, self.hirq_pos()] {
                if at > self.h && at < next { next = at; }
            }
            let step = (next - self.h).min(n);
            let from = self.h;
            self.h += step;
            n -= step;
            self.clocks += step as u64;
            self.apu.run(step);
            if let Some(g) = &mut self.gsu {
                g.run(step, &mut superfx::Mem { rom: &self.cart.rom, ram: &mut self.cart.sram });
            }
            if let Some(d) = &mut self.nec { d.run(step); }
            if let Some(c) = &mut self.cx4 { c.run(step, &mut cx4::Mem { rom: &self.cart.rom, ram: &mut self.cart.sram }); }
            if let Some(s) = &mut self.sa1 {
                s.run(step, &self.cart.rom, &mut self.cart.sram, self.mar);
                if s.st.bw_written { s.st.bw_written = false; if self.cart.battery { self.cart.sram_dirty = true; } }
            }
            self.events(from, self.h);
            if self.h >= LINE_CLOCKS { self.h -= LINE_CLOCKS; self.new_line(); }
        }
    }

    fn hirq_pos(&self) -> u32 {
        if self.nmitimen & 0x10 != 0 { (self.htime as u32 & 0x1FF) * 4 + 14 } else { 10 }
    }

    /// Events at positions in (from, to].
    fn events(&mut self, from: u32, to: u32) {
        let hit = |at: u32| from < at && at <= to;
        if hit(REFRESH_AT) && !self.refreshed { self.refreshed = true; self.tick(40); }
        if hit(self.render_at) && self.v >= 1 && self.v as usize <= HEIGHT {
            let row = self.v as usize - 1;
            self.ppu.render_line(row, &mut self.framebuffer);
        }
        if hit(HDMA_AT) && self.v < VDISP { self.hdma_run(); }
        // H / V timer IRQ
        let hen = self.nmitimen & 0x10 != 0;
        let ven = self.nmitimen & 0x20 != 0;
        if (hen || ven) && hit(self.hirq_pos()) && (!ven || self.v == self.vtime as u32) {
            if self.port_log_on && !self.irq_flag && self.ppu_log.len() < 400 { self.ppu_log.push((self.v as u16, self.h as u16, 0, 0)); }
            self.irq_flag = true;
        }
    }

    fn new_line(&mut self) {
        self.v += 1;
        self.refreshed = false;
        if self.v >= LINES {
            self.v = 0;
            self.frame += 1;
            self.frame_done = true;
        }
        if self.v == 0 {
            self.ppu_log_last = std::mem::take(&mut self.ppu_log);
            self.nmi_flag = false;
            self.ppu.frame_start();
            self.hdma_init();
        }
        if self.v == VDISP {
            self.ppu.vblank();
            self.nmi_flag = true;
            if self.nmitimen & 0x80 != 0 { self.nmi_pending = true; }
            if self.nmitimen & 1 != 0 { self.auto_joypad(); }
        }
    }

    pub fn irq_line(&self) -> bool {
        self.irq_flag || self.cx4.as_ref().map_or(false, |c| c.irq_line) || self.gsu.as_ref().map_or(false, |g| g.irq) || self.sa1.as_ref().map_or(false, |s| s.st.cpu_irq_line)
    }

    // ---------------- joypads ----------------

    fn auto_joypad(&mut self) {
        self.joy[0] = self.pads[0];
        self.joy[1] = self.pads[1];
        self.joy[2] = 0;
        self.joy[3] = 0;
        self.pad_shift = [0xFFFF_FFFF; 2];                 // the serial ports read out
        self.autojoy_busy_until = (self.v * LINE_CLOCKS + self.h) + 4224;
    }

    fn autojoy_busy(&self) -> bool {
        self.nmitimen & 1 != 0 && self.v >= VDISP && (self.v * LINE_CLOCKS + self.h) < self.autojoy_busy_until
    }

    // ---------------- registers ----------------

    fn read_io(&mut self, a: u16) -> Option<u8> {
        Some(match a {
            0x2134..=0x213F => {
                if a == 0x2137 { if self.wrio & 0x80 != 0 { self.ppu.latch_counters((self.h / 4) as u16, self.v as u16); } return None; }
                let disp = self.in_display();
                return self.ppu.read(a as u8, disp);
            }
            0x2140..=0x217F => self.apu.cpu_read((a & 3) as usize),
            0x2180 => { let v = self.wram[self.wram_addr as usize]; self.wram_addr = (self.wram_addr + 1) & 0x1FFFF; v }
            0x4016 => {
                let bit = (self.pad_shift[0] >> 31) as u8;
                if !self.pad_latch { self.pad_shift[0] = self.pad_shift[0] << 1 | 1; }
                (self.mdr & 0xFC) | bit
            }
            0x4017 => {
                let bit = (self.pad_shift[1] >> 31) as u8;
                if !self.pad_latch { self.pad_shift[1] = self.pad_shift[1] << 1 | 1; }
                (self.mdr & 0xE0) | 0x1C | bit
            }
            0x4210 => { let f = self.nmi_flag; self.nmi_flag = false; (self.mdr & 0x70) | (f as u8) << 7 | 0x02 }
            0x4211 => { let f = self.irq_flag; self.irq_flag = false; (self.mdr & 0x7F) | (f as u8) << 7 }
            0x4212 => {
                let vbl = self.v >= VDISP;
                let hbl = self.h <= 2 || self.h >= 1096;
                (self.mdr & 0x3E) | (vbl as u8) << 7 | (hbl as u8) << 6 | self.autojoy_busy() as u8
            }
            0x4213 => self.wrio,
            0x4214 => self.rddiv as u8,
            0x4215 => (self.rddiv >> 8) as u8,
            0x4216 => self.rdmpy as u8,
            0x4217 => (self.rdmpy >> 8) as u8,
            0x4218..=0x421F => { let j = self.joy[((a - 0x4218) / 2) as usize]; if a & 1 == 0 { j as u8 } else { (j >> 8) as u8 } }
            0x4300..=0x437F => {
                let c = &self.dma[((a >> 4) & 7) as usize];
                match a & 0x0F {
                    0 => c.dmap, 1 => c.bbad, 2 => c.a1t as u8, 3 => (c.a1t >> 8) as u8, 4 => c.a1b,
                    5 => c.das as u8, 6 => (c.das >> 8) as u8, 7 => c.dasb, 8 => c.a2a as u8, 9 => (c.a2a >> 8) as u8,
                    0xA => c.ntrl, 0xB | 0xF => c.unused, _ => return None,
                }
            }
            _ => return None,
        })
    }

    fn write_io(&mut self, a: u16, v: u8) {
        match a {
            0x2100..=0x2133 => {
                if self.port_log_on {
                    // the debug report's PPU log: registers, not the data ports
                    let data_port = matches!(a, 0x2104 | 0x2118 | 0x2119 | 0x2122);
                    if !data_port && self.ppu_log.len() < 400 { self.ppu_log.push((self.v as u16, self.h as u16, a as u8, v)); }
                }
                let disp = self.in_display();
                if a == 0x2100 && self.ppu.forced_blank() && v & 0x80 == 0 && self.v == VDISP { self.ppu.vblank(); }
                self.ppu.write(a as u8, v, disp);
            }
            0x2140..=0x217F => {
                if self.port_log_on {
                    if self.port_log.len() == 24 { self.port_log.remove(0); }
                    self.port_log.push((self.frame as u32, self.v as u16, (a & 3) as u8, v));
                }
                self.apu.cpu_write((a & 3) as usize, v)
            }
            0x2180 => { self.wram[self.wram_addr as usize] = v; self.wram_addr = (self.wram_addr + 1) & 0x1FFFF; }
            0x2181 => self.wram_addr = (self.wram_addr & 0x1FF00) | v as u32,
            0x2182 => self.wram_addr = (self.wram_addr & 0x100FF) | (v as u32) << 8,
            0x2183 => self.wram_addr = (self.wram_addr & 0x0FFFF) | ((v & 1) as u32) << 16,
            0x4016 => {
                let latch = v & 1 != 0;
                if latch { self.pad_shift = [(self.pads[0] as u32) << 16 | 0xFFFF, (self.pads[1] as u32) << 16 | 0xFFFF]; }
                self.pad_latch = latch;
            }
            0x4200 => {
                let was = self.nmitimen;
                self.nmitimen = v;
                if v & 0x80 != 0 && was & 0x80 == 0 && self.nmi_flag { self.nmi_pending = true; }
                if v & 0x30 == 0 { self.irq_flag = false; }
            }
            0x4201 => {
                if self.wrio & 0x80 != 0 && v & 0x80 == 0 { self.ppu.latch_counters((self.h / 4) as u16, self.v as u16); }
                self.wrio = v;
            }
            0x4202 => self.wrmpya = v,
            0x4203 => { self.rdmpy = self.wrmpya as u16 * v as u16; self.rddiv = v as u16; }
            0x4204 => self.wrdiv = (self.wrdiv & 0xFF00) | v as u16,
            0x4205 => self.wrdiv = (self.wrdiv & 0x00FF) | (v as u16) << 8,
            0x4206 => {
                if v == 0 { self.rddiv = 0xFFFF; self.rdmpy = self.wrdiv; }
                else { self.rddiv = self.wrdiv / v as u16; self.rdmpy = self.wrdiv % v as u16; }
            }
            0x4207 => self.htime = (self.htime & 0x100) | v as u16,
            0x4208 => self.htime = (self.htime & 0x0FF) | ((v & 1) as u16) << 8,
            0x4209 => self.vtime = (self.vtime & 0x100) | v as u16,
            0x420A => self.vtime = (self.vtime & 0x0FF) | ((v & 1) as u16) << 8,
            0x420B => self.dma_run(v),
            0x420C => self.hdmaen = v,
            0x420D => self.memsel = v,
            0x4300..=0x437F => {
                let c = &mut self.dma[((a >> 4) & 7) as usize];
                match a & 0x0F {
                    0 => c.dmap = v, 1 => c.bbad = v,
                    2 => c.a1t = (c.a1t & 0xFF00) | v as u16, 3 => c.a1t = (c.a1t & 0x00FF) | (v as u16) << 8,
                    4 => c.a1b = v,
                    5 => c.das = (c.das & 0xFF00) | v as u16, 6 => c.das = (c.das & 0x00FF) | (v as u16) << 8,
                    7 => c.dasb = v,
                    8 => c.a2a = (c.a2a & 0xFF00) | v as u16, 9 => c.a2a = (c.a2a & 0x00FF) | (v as u16) << 8,
                    0xA => c.ntrl = v, 0xB | 0xF => c.unused = v, _ => {}
                }
            }
            _ => {}
        }
    }

    // ---------------- memory ----------------

    /// A read without side effects on timing (DMA and the CPU both use it).
    /// Read ROM / WRAM without side effects (debug report); 0 elsewhere.
    pub fn peek(&self, a: u32) -> u8 {
        let (bank, off) = (a >> 16 & 0xFF, a & 0xFFFF);
        if bank == 0x7E || bank == 0x7F { return self.wram[(a & 0x1FFFF) as usize]; }
        if bank & 0x40 == 0 && off < 0x2000 { return self.wram[off as usize]; }
        self.cart.read(a).unwrap_or(0)
    }

    /// A cartridge read; while the Super FX runs, it may own the ROM (the CPU sees
    /// fixed vectors) and its RAM (open bus).
    fn cart_read(&mut self, a: u32) -> Option<u8> {
        if let Some(c) = &mut self.cx4 {
            match c.cpu_read(a) {
                cx4::CpuRead::Value(v) => return Some(v),
                cx4::CpuRead::OpenBus => return None,
                cx4::CpuRead::Cart => {}
            }
        }
        if let Some(m) = self.nec_map {
            if let Some(sr) = m.hit(a) { return self.nec.as_mut().map(|d| d.read(sr)); }
        }
        if let Some(s) = &mut self.sa1 { return s.cpu_read(&self.cart.rom, &self.cart.sram, a, self.mdr); }
        if let Some(g) = &self.gsu {
            let b = (a >> 16) as u8 & 0x7F;
            let off = a as u16;
            let rom = (b < 0x40 && off >= 0x8000) || (0x40..0x60).contains(&b);
            let ram = (b < 0x40 && (0x6000..0x8000).contains(&off)) || b == 0x70 || b == 0x71;
            if rom && g.rom_owned() { return Some(superfx::rom_vector(a)); }
            if ram && g.ram_owned() { return None; }
        }
        self.cart.read(a)
    }

    fn cart_write(&mut self, a: u32, v: u8) {
        if let Some(c) = &mut self.cx4 { if c.cpu_write(a, v) { return; } }
        if let Some(m) = self.nec_map {
            if let Some(sr) = m.hit(a) { if let Some(d) = &mut self.nec { d.write(sr, v); } return; }
        }
        if let Some(s) = &mut self.sa1 {
            s.cpu_write(&self.cart.rom, &mut self.cart.sram, a, v);
            if s.st.bw_written { s.st.bw_written = false; if self.cart.battery { self.cart.sram_dirty = true; } }
            return;
        }
        self.cart.write(a, v);
    }

    fn bus_read(&mut self, a: u32) -> u8 {
        let bank = (a >> 16) as u8;
        let off = a as u16;
        let v = if bank == 0x7E || bank == 0x7F {
            Some(self.wram[(a & 0x1FFFF) as usize])
        } else if bank & 0x40 == 0 && off < 0x8000 {
            match off {
                0x0000..=0x1FFF => Some(self.wram[off as usize]),
                0x2100..=0x21FF | 0x4000..=0x43FF => self.read_io(off),
                0x3000..=0x34FF if self.gsu.is_some() => Some(self.gsu.as_mut().unwrap().read_io(off)),
                _ => self.cart_read(a),
            }
        } else {
            self.cart_read(a)
        };
        let v = v.unwrap_or(self.mdr);
        if self.cheats.is_empty() { return v; }
        // a code for 00:8000 also hits its FastROM mirror 80:8000 (and the other way)
        self.cheats.patched(a, v).or_else(|| self.cheats.patched(a ^ 0x800000, v)).unwrap_or(v)
    }

    /// Pro Action Replay codes: called once per frame.
    pub fn apply_cheat_writes(&mut self) {
        for i in 0..self.cheats.ram_writes().len() {
            let w = self.cheats.ram_writes()[i];
            self.bus_write(w.addr, w.value);
        }
    }

    fn bus_write(&mut self, a: u32, v: u8) {
        let bank = (a >> 16) as u8;
        let off = a as u16;
        if bank == 0x7E || bank == 0x7F { self.wram[(a & 0x1FFFF) as usize] = v; return; }
        if bank & 0x40 == 0 && off < 0x8000 {
            match off {
                0x0000..=0x1FFF => self.wram[off as usize] = v,
                0x2100..=0x21FF | 0x4000..=0x43FF => self.write_io(off, v),
                0x3000..=0x34FF if self.gsu.is_some() => self.gsu.as_mut().unwrap().write_io(off, v),
                _ => self.cart_write(a, v),
            }
            return;
        }
        self.cart_write(a, v);
    }

    // ---------------- DMA ----------------

    /// A-bus addresses DMA can't reach (the B bus and the CPU's registers).
    fn valid_a(a: u32) -> bool {
        !((a & 0x40FF00) == 0x2100 || (a & 0x40FE00) == 0x4000 || (a & 0x40FFE0) == 0x4200 || (a & 0x40FF80) == 0x4300)
    }

    fn b_offset(mode: u8, i: u32) -> u8 {
        (match mode & 7 { 1 | 5 => i & 1, 3 | 7 => (i >> 1) & 1, 4 => i & 3, _ => 0 }) as u8
    }

    fn transfer(&mut self, ch: usize, a: u32, i: u32) {
        let c = self.dma[ch];
        self.mar = a;
        let b = 0x2100 | c.bbad.wrapping_add(Self::b_offset(c.dmap, i)) as u32;
        // WRAM to WRAM through 2180 doesn't work
        let valid = b != 0x2180 || ((a & 0xFE0000) != 0x7E0000 && (a & 0x40E000) != 0);
        if c.dmap & 0x80 == 0 {
            let v = if Self::valid_a(a) { self.bus_read(a) } else { 0 };
            self.mdr = v;
            if valid { self.bus_write(b, v); }
        } else {
            let v = if valid { self.bus_read(b) } else { 0 };
            self.mdr = v;
            if Self::valid_a(a) { self.bus_write(a, v); }
        }
        self.tick(8);
    }

    fn dma_run(&mut self, mask: u8) {
        if mask == 0 { return; }
        // HDMA set up or run on a channel cancels its general DMA, even one still
        // waiting its turn behind a long transfer on a lower channel
        self.hdma_kill = 0;
        self.tick(8);
        for ch in 0..8 {
            if mask & (1 << ch) == 0 || self.hdma_kill & (1 << ch) != 0 { continue; }
            self.tick(8);
            let mut i = 0u32;
            loop {
                // HDMA starting or running on this channel stops its general DMA
                if self.hdma_kill & (1 << ch) != 0 { break; }
                let c = self.dma[ch];
                let a = (c.a1b as u32) << 16 | c.a1t as u32;
                self.transfer(ch, a, i);
                i += 1;
                let c = &mut self.dma[ch];
                if c.dmap & 0x08 == 0 { c.a1t = if c.dmap & 0x10 != 0 { c.a1t.wrapping_sub(1) } else { c.a1t.wrapping_add(1) }; }
                c.das = c.das.wrapping_sub(1);
                if c.das == 0 { break; }
            }
        }
    }

    fn hdma_init(&mut self) {
        for ch in 0..8 {
            self.dma[ch].hdma_done = false;
            self.dma[ch].hdma_do = false;
            if self.hdmaen & (1 << ch) == 0 { continue; }
            self.hdma_kill |= 1 << ch;
            self.dma[ch].a2a = self.dma[ch].a1t;
            self.dma[ch].ntrl = 0;
            self.hdma_reload(ch);
        }
        if self.hdmaen != 0 { self.tick(18); }
    }

    /// A channel at the end of its line count reads the next table entry: 8 master
    /// clocks for the count, 16 more for an indirect address.
    fn hdma_reload(&mut self, ch: usize) {
        let c = self.dma[ch];
        if c.ntrl & 0x7F == 0 {
            self.tick(8);
            let v = self.bus_read((c.a1b as u32) << 16 | c.a2a as u32);
            let c = &mut self.dma[ch];
            c.ntrl = v;
            c.a2a = c.a2a.wrapping_add(1);
            c.hdma_done = v == 0;
            c.hdma_do = !c.hdma_done;
            if c.dmap & 0x40 != 0 {
                let bank = (c.a1b as u32) << 16;
                let a = c.a2a;
                self.tick(16);
                let lo = self.bus_read(bank | a as u32);
                let hi = self.bus_read(bank | a.wrapping_add(1) as u32);
                let c = &mut self.dma[ch];
                c.das = (hi as u16) << 8 | lo as u16;
                c.a2a = c.a2a.wrapping_add(2);
            }
        }
    }

    fn hdma_run(&mut self) {
        let active = (0..8).filter(|&ch| self.hdmaen & (1 << ch) != 0 && !self.dma[ch].hdma_done).count();
        if active == 0 { return; }
        self.tick(18);
        const LEN: [u32; 8] = [1, 2, 2, 4, 4, 4, 2, 4];
        for ch in 0..8 {
            if self.hdmaen & (1 << ch) == 0 || self.dma[ch].hdma_done { continue; }
            self.hdma_kill |= 1 << ch;
            if self.dma[ch].hdma_do {
                for i in 0..LEN[(self.dma[ch].dmap & 7) as usize] {
                    let c = self.dma[ch];
                    let a = if c.dmap & 0x40 == 0 {
                        self.dma[ch].a2a = c.a2a.wrapping_add(1);
                        (c.a1b as u32) << 16 | c.a2a as u32
                    } else {
                        self.dma[ch].das = c.das.wrapping_add(1);
                        (c.dasb as u32) << 16 | c.das as u32
                    };
                    self.transfer(ch, a, i);
                }
            }
        }
        for ch in 0..8 {
            if self.hdmaen & (1 << ch) == 0 || self.dma[ch].hdma_done { continue; }
            let c = &mut self.dma[ch];
            c.ntrl = c.ntrl.wrapping_sub(1);
            c.hdma_do = c.ntrl & 0x80 != 0;
            self.hdma_reload(ch);
        }
    }
}

impl Bus816 for Bus {
    fn read(&mut self, a: u32) -> u8 {
        let w = self.wait(a);
        self.mar = a;
        self.tick(w - 4);
        let v = self.bus_read(a);
        self.tick(4);
        // the CPU's own registers don't drive the data bus
        if (a & 0x40FC00) != 0x4000 { self.mdr = v; }
        v
    }
    fn write(&mut self, a: u32, v: u8) {
        let w = self.wait(a);
        self.mar = a;
        self.tick(w);
        self.mdr = v;
        self.bus_write(a, v);
    }
    fn idle(&mut self) { self.tick(6); }
    fn interrupt_pending(&self) -> bool { self.nmi_pending || self.irq_line() }
}
