//! Sega 32X / Mega 32X: two SH-2s (SH7604, 23 MHz) and a framebuffer VDP on top of the
//! Genesis. Ported from ares md/m32x (m32x.cpp, sh7604.cpp, bus-internal.cpp,
//! bus-external.cpp, io-internal.cpp, io-external.cpp, vdp.cpp), ISC license.
//!
//! SH-2 map (either CPU; addresses without the cache-area bits)
//!   00000000-00003FFF  its own boot ROM (master 2 KB, slave 1 KB)
//!   00004000-000043FF  32X registers (interrupt masks are per CPU)
//!   02000000-03FFFFFF  cartridge ROM (stalls while the 68000 has it: RV)
//!   04000000-0401FFFF  framebuffer (the bank the VDP isn't showing); 04020000 = overwrite
//!   06000000-0603FFFF  256 KB SDRAM
//! 68000 map with the adapter on (A15100 bit 0)
//!   000000-0000FF  the 32X's own vector table (256-byte 68000 BIOS; 70 = level-4 vector)
//!   000100-3FFFFF  cartridge only while RV (A15106 bit 0) is set, for DMA
//!   840000-87FFFF  framebuffer (when the 68000 has the VDP)   880000-8FFFFF  ROM 0-7FFFF
//!   900000-9FFFFF  ROM, 1 MB bank (A15104)                   A15100-A153FF  registers
//! Interrupts to the SH-2s: VRES 14, V 12, H 10, CMD 8 (from the 68000), PWM 6.
//!
//! Still to come: drawing the 32X picture over the Genesis one (step 3), PWM sound (4).

pub mod sh7604;

use serde::{Deserialize, Serialize};
use sh7604::{ExtBus, Sh7604};
use std::collections::VecDeque;

pub const BIOS_MAGIC: &[u8; 4] = b"MARB";

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
pub struct Source { pub enable: bool, pub active: bool }

/// One SH-2's interrupt inputs from the 32X.
#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct Irqs { pub pwm: Source, pub cmd: Source, pub hint: Source, pub vint: Source, pub vres: Source }

impl Default for Irqs {
    fn default() -> Self {
        Self { pwm: Source::default(), cmd: Source::default(), hint: Source::default(), vint: Source::default(),
               vres: Source { enable: true, active: false } }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Io {
    pub vector_level4: u32,
    /// A15100 bit 0: the 32X is switched in
    pub adapter_enable: bool,
    /// A15100 bit 1: 0 holds both SH-2s in reset
    pub adapter_reset: bool,
    pub reset_enable: bool,
    pub rom_bank: u8,
    pub cartridge_mode: bool,
    pub hperiod: u8,
    pub hcounter: u8,
    pub hint_vblank: bool,
}

impl Default for Io {
    fn default() -> Self {
        Self { vector_level4: 0, adapter_enable: false, adapter_reset: true, reset_enable: true, rom_bank: 0,
               cartridge_mode: false, hperiod: 0, hcounter: 0, hint_vblank: false }
    }
}

/// 68000 -> SH-2 transfers (DREQ): the 68000 fills a FIFO, an SH-2's DMA empties it.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Dreq {
    /// RV: the cartridge goes back to the 68000's 000000 (and the SH-2s wait for it)
    pub vram: bool,
    pub dma: bool,
    pub active: bool,
    pub source: u32,
    pub target: u32,
    pub length: u16,
    pub fifo: VecDeque<u16>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Vdp {
    /// two 128 KB framebuffers (words)
    pub dram: Vec<u16>,
    pub cram: Vec<u16>,
    pub mode: u8,
    pub lines: bool,
    pub priority: bool,
    pub dotshift: bool,
    pub autofill_length: u8,
    pub autofill_address: u16,
    pub autofill_data: u16,
    /// FM: false = the 68000 has the VDP, true = the SH-2s
    pub framebuffer_access: bool,
    /// the bank being shown (switches at vblank)
    pub framebuffer_active: bool,
    pub framebuffer_select: bool,
    pub framebuffer_wait: u32,
    pub hblank: bool,
    pub vblank: bool,
    /// mode, lines, priority, dotshift as latched at the end of hblank (for drawing)
    pub latch: (u8, bool, bool, bool),
}

impl Default for Vdp {
    fn default() -> Self {
        Self { dram: vec![0; 0x20000], cram: vec![0; 256], mode: 0, lines: false, priority: false, dotshift: false,
               autofill_length: 0, autofill_address: 0, autofill_data: 0, framebuffer_access: false,
               framebuffer_active: false, framebuffer_select: false, framebuffer_wait: 0, hblank: false, vblank: true,
               latch: (0, false, false, false) }
    }
}

impl Vdp {
    /// Word offset of the bank the CPUs draw into (the one not on screen).
    fn back(&self) -> usize { if self.framebuffer_active { 0x10000 } else { 0 } }
    /// The bank on screen.
    pub fn front(&self) -> usize { if self.framebuffer_active { 0 } else { 0x10000 } }

    fn select(&mut self, sel: bool) {
        self.framebuffer_select = sel;
        if !self.vblank && self.latch.0 != 0 { return; } // waits for vblank
        self.framebuffer_active = sel;
    }

    /// FEN: the framebuffer is busy (a bank swap waiting for vblank, or a fill running).
    pub fn engaged(&self) -> bool {
        (self.latch.0 != 0 && self.framebuffer_select != self.framebuffer_active) || self.framebuffer_wait > 0
    }

    pub fn palette_engaged(&self) -> bool { !self.vblank && !self.hblank && self.latch.0 & 1 != 0 }

    fn fill(&mut self) {
        if self.framebuffer_wait > 0 { return; } // still busy with the last one
        self.framebuffer_wait = 7 + 3 * (self.autofill_length as u32 + 1);
        let b = self.back();
        for _ in 0..=self.autofill_length as u32 {
            self.dram[b + self.autofill_address as usize] = self.autofill_data;
            self.autofill_address = (self.autofill_address & 0xFF00) | (self.autofill_address as u8).wrapping_add(1) as u16;
        }
    }
}

/// PWM registers (the sound itself is step 4).
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct PwmRegs { pub lmode: u8, pub rmode: u8, pub mono: bool, pub dreq_irq: bool, pub timer: u8, pub cycle: u16,
                     pub lfifo: u16, pub rfifo: u16, pub mfifo: u16 }

#[derive(Clone, Serialize, Deserialize)]
pub struct State {
    #[serde(skip)]
    pub vectors: Vec<u8>,
    #[serde(skip)]
    pub mrom: Vec<u8>,
    #[serde(skip)]
    pub srom: Vec<u8>,
    pub sdram: Vec<u16>,
    pub io: Io,
    pub dreq: Dreq,
    pub comm: [u16; 8],
    pub vdp: Vdp,
    pub pwm: PwmRegs,
    /// [master, slave]
    pub irq: [Irqs; 2],
    /// a CPU asked for a reset (A15100 bit 1 written 0)
    pub restart: bool,
}

pub struct M32x {
    pub sh: [Sh7604; 2],
    pub st: State,
    /// master-clock time owed to each SH-2, scaled (master * 3 - sh2 * 7)
    debt: [i64; 2],
}

fn word(mem: &[u8], a: usize) -> u16 {
    if mem.is_empty() { return 0xFFFF; }
    let a = a % mem.len() & !1;
    (mem[a] as u16) << 8 | *mem.get(a + 1).unwrap_or(&0) as u16
}

/// The cartridge ROM as a 32X board sees it (mirrored).
fn rom_word(rom: &[u8], a: u32) -> u16 {
    if rom.is_empty() { return 0xFFFF; }
    let n = rom.len().next_power_of_two();
    let a = (a as usize) & (n - 1);
    if a + 1 < rom.len() { (rom[a & !1] as u16) << 8 | rom[(a & !1) + 1] as u16 } else { word(rom, a) }
}

fn set(w: &mut u16, upper: bool, lower: bool, v: u16) {
    if upper { *w = (*w & 0x00FF) | (v & 0xFF00); }
    if lower { *w = (*w & 0xFF00) | (v & 0x00FF); }
}

impl M32x {
    /// `bios`: an "MARB" bundle; the three BIOS files are told apart by size: 256 bytes
    /// (68000 vectors), 2 KB (master SH-2), 1 KB (slave SH-2).
    pub fn new(bios: &[u8]) -> Result<Self, String> {
        let (mut vectors, mut mrom, mut srom) = (Vec::new(), Vec::new(), Vec::new());
        for (_, data) in crate::common::firmware::files(bios, BIOS_MAGIC) {
            match data.len() {
                256 => vectors = data,
                2048 => mrom = data,
                1024 => srom = data,
                _ => {}
            }
        }
        let missing: Vec<&str> = [("68000 (256 bytes)", &vectors), ("master SH-2 (2 KB)", &mrom), ("slave SH-2 (1 KB)", &srom)]
            .iter().filter(|(_, d)| d.is_empty()).map(|(n, _)| *n).collect();
        if !missing.is_empty() {
            return Err(format!("32X: missing BIOS: {}; put 32X_G_BIOS.BIN, 32X_M_BIOS.BIN, 32X_S_BIOS.BIN in user://BIOS/", missing.join(", ")));
        }
        let st = State { vectors, mrom, srom, sdram: vec![0; 0x20000], io: Io::default(), dreq: Dreq::default(), comm: [0; 8],
                         vdp: Vdp::default(), pwm: PwmRegs::default(), irq: [Irqs::default(); 2], restart: false };
        Ok(Self { sh: [Sh7604::new(), Sh7604::new()], st, debt: [0; 2] })
    }

    /// The console's reset button.
    pub fn reset(&mut self) {
        let keep = (std::mem::take(&mut self.st.vectors), std::mem::take(&mut self.st.mrom), std::mem::take(&mut self.st.srom));
        let v4 = self.st.io.vector_level4;
        self.st = State { vectors: keep.0, mrom: keep.1, srom: keep.2, sdram: vec![0; 0x20000], io: Io { vector_level4: v4, ..Io::default() },
                          dreq: Dreq::default(), comm: [0; 8], vdp: Vdp::default(), pwm: PwmRegs::default(),
                          irq: [Irqs::default(); 2], restart: false };
        for s in &mut self.sh { s.reset(true); }
        self.debt = [0; 2];
    }

    /// Run both SH-2s for `mclk` master clocks (SH-2 clock = master * 3 / 7).
    pub fn run(&mut self, mclk: i64, rom: &[u8]) {
        if self.st.restart {
            self.st.restart = false;
            for s in &mut self.sh { s.reset(true); }
            self.st.irq = [Irqs::default(); 2];
        }
        self.debt[0] += mclk * 3;
        self.debt[1] += mclk * 3;
        while self.debt[0] > 0 || self.debt[1] > 0 {
            let c = if self.debt[0] >= self.debt[1] { 0 } else { 1 };
            if !self.st.io.adapter_reset {
                // held in reset
                self.debt[c] = self.debt[c].min(0);
                continue;
            }
            let clocks = self.step_cpu(c, rom);
            self.debt[c] -= clocks as i64 * 7;
            self.st.vdp.framebuffer_wait = self.st.vdp.framebuffer_wait.saturating_sub(clocks);
        }
    }

    /// One instruction (or an interrupt) on SH-2 `c`; returns the SH-2 clocks it took.
    fn step_cpu(&mut self, c: usize, rom: &[u8]) -> u32 {
        let mut ext = Ext { st: &mut self.st, rom, cpu: c, waits: 0 };
        let sh = &mut self.sh[c];
        if !sh.cpu.id && !sh.cpu.in_delay_slot() {
            let q = ext.st.irq[c];
            let i = sh.cpu.sr.i;
            let pick = if q.vres.active && q.vres.enable { Some((14, 71, true)) }
                else if q.vint.active && q.vint.enable && i < 12 { Some((12, 70, false)) }
                else if q.hint.active && q.hint.enable && i < 10 { Some((10, 69, false)) }
                else if q.cmd.active && q.cmd.enable && i < 8 { Some((8, 68, false)) }
                else if q.pwm.active && q.pwm.enable && i < 6 { Some((6, 67, false)) }
                else { None };
            if let Some((level, vector, vres)) = pick {
                if vres { ext.st.irq[c].vres.active = false; }
                sh.cpu.interrupt(&mut sh7604::ChipBus { p: &mut sh.p, ext: &mut ext, fetch: 0 }, level, vector);
                return 1 + ext.waits;
            }
        }
        let clocks = sh.step(&mut ext);
        // DREQ 0 follows the FIFO
        let fifo = !ext.st.dreq.fifo.is_empty();
        let waits = ext.waits;
        for s in &mut self.sh { s.p.dmac.dreq[0] = fifo; }
        clocks + waits
    }

    // ---------------- the Genesis side ----------------

    /// Genesis vblank (true at the start, false at the end).
    pub fn vblank(&mut self, line: bool) {
        self.st.vdp.vblank = line;
        for q in &mut self.st.irq { q.vint.active = line; }
        if line { let s = self.st.vdp.framebuffer_select; self.st.vdp.select(s); }
    }

    /// Genesis hblank: true at the start, false at the end.
    pub fn hblank(&mut self, line: bool) {
        let v = &mut self.st.vdp;
        if v.hblank && !line { v.latch = (v.mode, v.lines, v.priority, v.dotshift); }
        v.hblank = line;
        for q in &mut self.st.irq { q.hint.active = false; }
        let io = &mut self.st.io;
        if line && (!self.st.vdp.vblank || io.hint_vblank) {
            let n = io.hcounter;
            io.hcounter = io.hcounter.wrapping_add(1);
            if n >= io.hperiod {
                io.hcounter = 0;
                for q in &mut self.st.irq { q.hint.active = true; }
            }
        }
    }

    /// 68000 read in 000000-3FFFFF: Some = the 32X answers (None = the cartridge as usual).
    pub fn low_read(&self, a: u32, rom: &[u8]) -> Option<u16> {
        let io = &self.st.io;
        if !io.adapter_enable { return None; }
        if a < 0x100 {
            return Some(match a & !1 {
                0x70 => (io.vector_level4 >> 16) as u16,
                0x72 => io.vector_level4 as u16,
                x => word(&self.st.vectors, x as usize),
            });
        }
        Some(if self.st.dreq.vram { rom_word(rom, a & 0x3FFFFE) } else { 0xFFFF })
    }

    /// 68000 write in 000000-3FFFFF: true when the 32X took it.
    pub fn low_write(&mut self, a: u32, upper: bool, lower: bool, v: u16) -> bool {
        let io = &mut self.st.io;
        if !io.adapter_enable { return false; }
        if a & !3 == 0x70 {
            let shift = if a & 2 == 0 { 16 } else { 0 };
            let mut w = (io.vector_level4 >> shift) as u16;
            set(&mut w, upper, lower, v);
            io.vector_level4 = (io.vector_level4 & !(0xFFFF << shift)) | (w as u32) << shift;
        }
        true
    }

    /// 68000 800000-9FFFFF.
    pub fn high_read(&self, a: u32, rom: &[u8]) -> u16 {
        if !self.st.io.adapter_enable { return 0xFFFF; }
        let v = &self.st.vdp;
        match a {
            0x840000..=0x87FFFF => if v.framebuffer_access || v.engaged() { 0xFFFF } else { v.dram[v.back() + (a as usize >> 1 & 0xFFFF)] },
            0x880000..=0x8FFFFF if !self.st.dreq.vram => rom_word(rom, a & 0x7FFFE),
            0x900000..=0x9FFFFF if !self.st.dreq.vram => rom_word(rom, (self.st.io.rom_bank as u32) << 20 | (a & 0xFFFFE)),
            _ => 0xFFFF,
        }
    }

    pub fn high_write(&mut self, a: u32, upper: bool, lower: bool, v: u16) {
        if !self.st.io.adapter_enable { return; }
        let vdp = &mut self.st.vdp;
        if vdp.framebuffer_access || vdp.engaged() { return; }
        let i = vdp.back() + (a as usize >> 1 & 0xFFFF);
        match a {
            0x840000..=0x85FFFF => {
                if v == 0 && (!upper || !lower) { return; } // 8-bit 00 writes don't go through
                set(&mut vdp.dram[i], upper, lower, v);
            }
            0x860000..=0x87FFFF => {
                // overwrite: 00 bytes don't write
                if upper && v >> 8 != 0 { set(&mut vdp.dram[i], true, false, v); }
                if lower && v & 0xFF != 0 { set(&mut vdp.dram[i], false, true, v); }
            }
            _ => {}
        }
    }

    /// 68000 A130EC-A130EF ("MARS") and A15100-A153FF.
    pub fn io_read(&mut self, a: u32) -> Option<u16> {
        let st = &mut self.st;
        let a = a & 0xFFFFFE;
        Some(match a {
            0xA130EC => u16::from_be_bytes(*b"MA"),
            0xA130EE => u16::from_be_bytes(*b"RS"),
            0xA15100 => st.io.adapter_enable as u16 | (st.io.adapter_reset as u16) << 1 | (st.io.reset_enable as u16) << 7 | (st.vdp.framebuffer_access as u16) << 15,
            0xA15102 => st.irq[0].cmd.active as u16 | (st.irq[1].cmd.active as u16) << 1,
            0xA15104 => st.io.rom_bank as u16,
            0xA15106 => st.dreq.vram as u16 | (st.dreq.dma as u16) << 1 | (st.dreq.active as u16) << 2 | ((st.dreq.fifo.len() >= 8) as u16) << 7,
            0xA15108 => (st.dreq.source >> 16) as u16 & 0xFF,
            0xA1510A => st.dreq.source as u16,
            0xA1510C => (st.dreq.target >> 16) as u16 & 0xFF,
            0xA1510E => st.dreq.target as u16,
            0xA15110 => st.dreq.length,
            0xA1511A => st.io.cartridge_mode as u16,
            0xA15112..=0xA1511E => 0,
            0xA15120..=0xA1512E => st.comm[(a >> 1 & 7) as usize],
            0xA15130..=0xA15138 => pwm_read(&st.pwm, a & 0xF),
            0xA15180..=0xA1518A => vdp_read(&st.vdp, a & 0xF),
            0xA15200..=0xA153FE => if st.vdp.framebuffer_access { 0xFFFF } else { st.vdp.cram[(a as usize >> 1) & 0xFF] },
            _ => return None,
        })
    }

    pub fn io_write(&mut self, a: u32, upper: bool, lower: bool, v: u16) -> bool {
        let st = &mut self.st;
        let a = a & 0xFFFFFE;
        match a {
            0xA15100 => {
                if lower {
                    st.io.adapter_enable = v & 1 != 0;
                    if v & 2 == 0 { st.restart = true; }
                    st.io.adapter_reset = v & 2 != 0;
                }
                if upper { st.vdp.framebuffer_access = v & 0x8000 != 0; }
            }
            0xA15102 => if lower { st.irq[0].cmd.active = v & 1 != 0; st.irq[1].cmd.active = v & 2 != 0; },
            0xA15104 => if lower { st.io.rom_bank = v as u8 & 3; },
            0xA15106 => if lower {
                st.dreq.vram = v & 1 != 0;
                st.dreq.dma = v & 2 != 0;
                let active = v & 4 != 0;
                if st.dreq.active && !active { st.dreq.fifo.clear(); }
                st.dreq.active = active;
            },
            0xA15108 => if lower { st.dreq.source = (st.dreq.source & 0xFFFF) | (v as u32 & 0xFF) << 16; },
            0xA1510A => { let mut w = st.dreq.source as u16; set(&mut w, upper, lower, v); st.dreq.source = (st.dreq.source & 0xFF0000) | w as u32; }
            0xA1510C => if lower { st.dreq.target = (st.dreq.target & 0xFFFF) | (v as u32 & 0xFF) << 16; },
            0xA1510E => { let mut w = st.dreq.target as u16; set(&mut w, upper, lower, v); st.dreq.target = (st.dreq.target & 0xFF0000) | w as u32; }
            0xA15110 => { set(&mut st.dreq.length, upper, lower, v & if lower { !3 } else { !0 }); }
            0xA15112 => {
                if st.dreq.active && st.dreq.fifo.len() < 8 {
                    st.dreq.fifo.push_back(v);
                    st.dreq.length = st.dreq.length.wrapping_sub(1);
                    if st.dreq.length == 0 { st.dreq.active = false; }
                }
            }
            0xA1511A => if lower { st.io.cartridge_mode = v & 1 != 0; },
            0xA15120..=0xA1512E => set(&mut st.comm[(a >> 1 & 7) as usize], upper, lower, v),
            0xA15130 => if lower { st.pwm.lmode = v as u8 & 3; st.pwm.rmode = v as u8 >> 2 & 3; st.pwm.mono = v & 0x10 != 0; },
            0xA15132..=0xA15138 => pwm_write(&mut st.pwm, a & 0xF, upper, lower, v),
            0xA15180..=0xA1518A => if !st.vdp.framebuffer_access { vdp_write(&mut st.vdp, a & 0xF, upper, lower, v) },
            0xA15200..=0xA153FE => if !st.vdp.framebuffer_access { set(&mut st.vdp.cram[(a as usize >> 1) & 0xFF], upper, lower, v) },
            _ => return false,
        }
        true
    }
}

fn pwm_read(p: &PwmRegs, r: u32) -> u16 {
    match r {
        0x0 => p.lmode as u16 | (p.rmode as u16) << 2 | (p.mono as u16) << 4 | (p.dreq_irq as u16) << 7 | (p.timer as u16) << 8,
        0x2 => p.cycle,
        // (no FIFO yet: always empty)
        0x4 => p.lfifo & 0xFFF | 1 << 14,
        0x6 => p.rfifo & 0xFFF | 1 << 14,
        _ => p.mfifo & 0xFFF | 1 << 14,
    }
}

fn pwm_write(p: &mut PwmRegs, r: u32, upper: bool, lower: bool, v: u16) {
    let w = match r { 0x2 => &mut p.cycle, 0x4 => &mut p.lfifo, 0x6 => &mut p.rfifo, _ => &mut p.mfifo };
    set(w, upper, lower, v & 0x0FFF);
}

fn vdp_read(v: &Vdp, r: u32) -> u16 {
    match r {
        0x0 => v.mode as u16 | (v.lines as u16) << 6 | (v.priority as u16) << 7 | 1 << 15, // NTSC
        0x2 => v.dotshift as u16,
        0x4 => v.autofill_length as u16,
        0x6 => v.autofill_address,
        0x8 => v.autofill_data,
        _ => v.framebuffer_active as u16 | (v.engaged() as u16) << 1 | (!v.palette_engaged() as u16) << 13
            | (v.hblank as u16) << 14 | (v.vblank as u16) << 15,
    }
}

fn vdp_write(v: &mut Vdp, r: u32, upper: bool, lower: bool, d: u16) {
    match r {
        0x0 => if lower { v.mode = d as u8 & 3; v.lines = d & 0x40 != 0; v.priority = d & 0x80 != 0; },
        0x2 => if lower { v.dotshift = d & 1 != 0; },
        0x4 => if lower { v.autofill_length = d as u8; },
        0x6 => set(&mut v.autofill_address, upper, lower, d),
        0x8 => { set(&mut v.autofill_data, upper, lower, d); v.fill(); }
        _ => if lower { v.select(d & 1 != 0); },
    }
}

/// An SH-2's view of the 32X.
struct Ext<'a> {
    st: &'a mut State,
    rom: &'a [u8],
    cpu: usize,
    waits: u32,
}

impl ExtBus for Ext<'_> {
    fn read(&mut self, a: u32) -> u16 {
        let st = &mut *self.st;
        match a {
            0x0000_0000..=0x0000_3FFF => { self.waits += 1; word(if self.cpu == 0 { &st.mrom } else { &st.srom }, a as usize) }
            0x0000_4000..=0x0000_43FF => { self.waits += 1; self.io_read(a) }
            0x0200_0000..=0x03FF_FFFF => { self.waits += 6; rom_word(self.rom, a & 0x3FFFFF) }
            0x0400_0000..=0x05FF_FFFF => {
                let v = &st.vdp;
                if !v.framebuffer_access || v.engaged() { return 0xFFFF; }
                self.waits += 5;
                v.dram[v.back() + (a as usize >> 1 & 0xFFFF)]
            }
            0x0600_0000..=0x0603_FFFF => st.sdram[(a as usize >> 1) & 0x1FFFF],
            _ => 0xFFFF,
        }
    }

    fn write(&mut self, a: u32, v: u16, upper: bool, lower: bool) {
        match a {
            0x0000_4000..=0x0000_43FF => { self.waits += 1; self.io_write(a, v, upper, lower) }
            0x0400_0000..=0x05FF_FFFF => {
                let a = a & 0x0403_FFFF;
                let vdp = &mut self.st.vdp;
                if !vdp.framebuffer_access || vdp.engaged() { return; }
                self.waits += 4;
                let i = vdp.back() + (a as usize >> 1 & 0xFFFF);
                if a < 0x0402_0000 {
                    if v == 0 && (!upper || !lower) { return; }
                    set(&mut vdp.dram[i], upper, lower, v);
                } else {
                    if upper && v >> 8 != 0 { set(&mut vdp.dram[i], true, false, v); }
                    if lower && v & 0xFF != 0 { set(&mut vdp.dram[i], false, true, v); }
                }
            }
            0x0600_0000..=0x0603_FFFF => set(&mut self.st.sdram[(a as usize >> 1) & 0x1FFFF], upper, lower, v),
            _ => {}
        }
    }
}

impl Ext<'_> {
    /// SH-2 registers 4000-43FF.
    fn io_read(&mut self, a: u32) -> u16 {
        let st = &mut *self.st;
        let a = a & 0x43FE;
        match a {
            0x4000 => {
                let q = &st.irq[self.cpu];
                q.pwm.enable as u16 | (q.cmd.enable as u16) << 1 | (q.hint.enable as u16) << 2 | (q.vint.enable as u16) << 3
                    | (st.io.hint_vblank as u16) << 7 | (st.io.adapter_enable as u16) << 9 | (st.vdp.framebuffer_access as u16) << 15
                // bit 8 = 0: a cartridge is in
            }
            0x4004 => st.io.hperiod as u16,
            0x4006 => st.dreq.vram as u16 | (st.dreq.dma as u16) << 1 | (st.dreq.active as u16) << 2
                | (st.dreq.fifo.is_empty() as u16) << 14 | ((st.dreq.fifo.len() >= 8) as u16) << 15,
            0x4008 => (st.dreq.source >> 16) as u16 & 0xFF,
            0x400A => st.dreq.source as u16,
            0x400C => (st.dreq.target >> 16) as u16 & 0xFF,
            0x400E => st.dreq.target as u16,
            0x4010 => st.dreq.length,
            0x4012 => st.dreq.fifo.pop_front().unwrap_or(0),
            0x4020..=0x402E => st.comm[(a >> 1 & 7) as usize],
            0x4030..=0x4038 => pwm_read(&st.pwm, a & 0xF),
            0x4100..=0x410A => vdp_read(&st.vdp, a & 0xF),
            0x4200..=0x43FE => if st.vdp.framebuffer_access { self.waits += 4; st.vdp.cram[(a as usize >> 1) & 0xFF] } else { 0xFFFF },
            _ => 0,
        }
    }

    fn io_write(&mut self, a: u32, v: u16, upper: bool, lower: bool) {
        let st = &mut *self.st;
        let a = a & 0x43FE;
        let me = self.cpu;
        match a {
            0x4000 => {
                if lower {
                    let q = &mut st.irq[me];
                    q.pwm.enable = v & 1 != 0; q.cmd.enable = v & 2 != 0; q.hint.enable = v & 4 != 0; q.vint.enable = v & 8 != 0;
                    st.io.hint_vblank = v & 0x80 != 0;
                }
                if upper { st.vdp.framebuffer_access = v & 0x8000 != 0; }
            }
            0x4004 => if lower { st.io.hperiod = v as u8; st.io.hcounter = 0; },
            0x4014 => st.irq[me].vres.active = false,
            0x4016 => st.irq[me].vint.active = false,
            0x4018 => st.irq[me].hint.active = false,
            0x401A => st.irq[me].cmd.active = false,
            0x401C => st.irq[me].pwm.active = false,
            0x4020..=0x402E => set(&mut st.comm[(a >> 1 & 7) as usize], upper, lower, v),
            0x4030 => {
                if lower { st.pwm.lmode = v as u8 & 3; st.pwm.rmode = v as u8 >> 2 & 3; st.pwm.mono = v & 0x10 != 0; st.pwm.dreq_irq = v & 0x80 != 0; }
                if upper { st.pwm.timer = (v >> 8) as u8 & 15; }
            }
            0x4032..=0x4038 => pwm_write(&mut st.pwm, a & 0xF, upper, lower, v),
            0x4100..=0x410A => if st.vdp.framebuffer_access { vdp_write(&mut st.vdp, a & 0xF, upper, lower, v) },
            0x4200..=0x43FE => if st.vdp.framebuffer_access { self.waits += 4; set(&mut st.vdp.cram[(a as usize >> 1) & 0xFF], upper, lower, v) },
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests;
