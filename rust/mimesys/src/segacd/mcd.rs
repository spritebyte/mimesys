//! The Sega CD unit: its own 68000 (the "sub CPU", 12.5 MHz), the gate array that joins
//! it to the Genesis, and the memory they share. Ported from ares (md/mcd: mcd.cpp,
//! bus-internal.cpp, bus-external.cpp, io-internal.cpp, io-external.cpp, irq.cpp,
//! timer.cpp), ISC license; see THIRD_PARTY_NOTICES.md.
//!
//! Memory
//!   BIOS      128 KB  Genesis 000000-01FFFF (vector 70 = the level-4 vector register)
//!   PRG-RAM   512 KB  sub 000000-07FFFF; Genesis 020000-03FFFF shows one 128 KB bank,
//!                     only while it holds the sub CPU's bus (BUSREQ)
//!   word RAM  256 KB  2M mode: all of it to one CPU at a time (Genesis 200000, sub 080000)
//!                     1M mode: two 128 KB banks, one each (interleaved by word);
//!                     the sub sees its bank as pixels at 080000 too ("dot-mapped")
//!   backup RAM  8 KB  sub FE0000-FE3FFF, odd bytes
//! Registers: Genesis A12000-A1203F (mirrored to A120FF), sub FF8000-FF81FF.
//! Sub interrupts: 1 graphics, 2 from the Genesis (A12000 bit 8), 3 timer, 4 CD drive,
//! 5 CD decoder, 6 subcode.
//!
//! The parts: CD drive (cdd.rs), CD decoder (cdc.rs), graphics chip (gpu.rs), sample
//! player (pcm.rs). Sound: CD audio (44.1 kHz, with the FF8034 volume) and the PCM chip
//! (32.55 kHz, averaged down) are mixed into `audio` at 44.1 kHz for the Genesis to add in.

use crate::common::cpu::m68000::{M68000, M68kBus};
use crate::common::disc::Disc;
use crate::segacd::cdc::Cdc;
use crate::segacd::gpu::Gpu;
use crate::segacd::pcm::Pcm;
use crate::segacd::cdd::{Cdd, Toc};
use serde::{Deserialize, Serialize};

pub const SUB_HZ: i64 = 12_500_000;

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
pub struct Irq { pub enable: bool, pub pending: bool }

impl Irq {
    /// Raise: becomes pending if enabled (and isn't already).
    pub fn raise(&mut self) { if !self.pending { self.pending = self.enable; } }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Io {
    /// RES0: the sub CPU runs (false = held in reset)
    pub run: bool,
    /// BUSREQ: the Genesis holds the sub CPU's bus (and sees PRG-RAM)
    pub request: bool,
    pub halt: bool,
    pub wram_latch: u16,
    /// MODE: false = 2M, true = 1M
    pub wram_mode: bool,
    pub wram_switch_request: bool,
    /// 2M: true = the sub CPU has word RAM
    pub wram_switch: bool,
    /// RET (2M) / which 1M bank the Genesis has
    pub wram_select: bool,
    pub wram_priority: u8,
    pub pram_bank: u8,
    pub pram_protect: u8,
    pub vector_level4: u32,
}

impl Default for Io {
    fn default() -> Self {
        Self { run: false, request: false, halt: true, wram_latch: 0, wram_mode: false, wram_switch_request: false,
               wram_switch: false, wram_select: false, wram_priority: 0, pram_bank: 0, pram_protect: 0, vector_level4: !0 }
    }
}

/// Registers of the parts that come later (CD drive / decoder, graphics, PCM): stored so
/// the BIOS reads back what it writes.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Later {
    pub fader: u16,
    pub font_colors: u8,
    pub font_data: u16,
    pub led: u8,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct State {
    #[serde(skip)]
    pub bios: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub pram: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub wram: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub bram: Vec<u8>,
    pub io: Io,
    pub cfm: u8,
    pub cfs: u8,
    pub command: [u16; 8],
    pub status: [u16; 8],
    /// interrupt sources, by level (index 1-6; 0 unused)
    pub irq: [Irq; 7],
    /// the sub CPU must load its reset vectors before it runs again
    pub reset_pending: bool,
    pub timer_frequency: u8,
    pub timer_counter: u8,
    /// sub clocks into the current 384-clock gate-array tick
    pub divider: u32,
    pub stopwatch: u16,
    pub later: Later,
    pub gpu: Gpu,
    pub pcm: Pcm,
    /// CD audio volume (FF8034 bits 6-15; 0x4000 = full) and where it is now (it slides)
    pub cdda_volume: u16,
    pub cdda_level: u16,
    /// sound out: interleaved stereo at 44.1 kHz, -1..1
    #[serde(skip)]
    pub audio: Vec<f32>,
    /// PCM samples since the last 44.1 kHz sample (sum left, sum right, count), and the last one
    #[serde(skip)]
    pcm_acc: (i32, i32, i32),
    #[serde(skip)]
    pcm_last: (i32, i32),
    /// the audio sector being played
    #[serde(skip)]
    cd_cache: Option<(i32, Vec<u8>)>,
    pub cdd: Cdd,
    pub cdc: Cdc,
    /// sub clocks towards the next CDC DMA word (every 6)
    pub dma_counter: u32,
    /// sub clocks * 44100 towards the next CD audio sample
    pub cdda_counter: i64,
    /// backup RAM changed since the last save
    #[serde(skip)]
    pub bram_dirty: bool,
}

pub struct Mcd {
    pub cpu: M68000,
    pub st: State,
    pub disc: Option<Disc>,
    /// master-clock time owed to the sub CPU, scaled (master * SUB_HZ - sub * mclk)
    debt: i64,
}

/// Backup RAM as the BIOS leaves it after "format": all zero, and in the last 64 bytes
/// the volume name, the free block count (total - 3; stored four times), the file count
/// (0, four times) and "SEGA_CD_ROM" / "RAM_CARTRIDGE___". Layout from superctr's buram
/// tool (MIT license).
pub fn format_bram(ram: &mut [u8]) {
    ram.fill(0);
    let top = ram.len() - 0x40;
    let free = (ram.len() / 64 - 3) as u16;
    let f = &mut ram[top..];
    f[..11].copy_from_slice(b"___________");
    f[0x0B..0x10].copy_from_slice(&[0, 0, 0, 0, 0x40]);
    for i in 0..4 { f[0x10 + i * 2..0x12 + i * 2].copy_from_slice(&free.to_be_bytes()); }
    f[0x20..0x40].copy_from_slice(b"SEGA_CD_ROM\x00\x01\x00\x00\x00RAM_CARTRIDGE___");
}

/// Has the BIOS (or `format_bram`) formatted this backup RAM?
pub fn bram_formatted(ram: &[u8]) -> bool {
    ram.len() >= 0x40 && &ram[ram.len() - 0x20..ram.len() - 0x15] == b"SEGA_CD_ROM"
}

fn word(mem: &[u8], i: usize) -> u16 { (mem[i * 2] as u16) << 8 | mem[i * 2 + 1] as u16 }

fn put(mem: &mut [u8], i: usize, upper: bool, lower: bool, v: u16) {
    if upper { mem[i * 2] = (v >> 8) as u8; }
    if lower { mem[i * 2 + 1] = v as u8; }
}

impl Mcd {
    pub fn new(bios: Vec<u8>, disc: Option<Disc>) -> Self {
        let toc = disc.as_ref().map(Toc::from_disc).unwrap_or_default();
        let st = State {
            cdd: Cdd::new(toc), cdc: Cdc::default(), gpu: Gpu::default(), pcm: Pcm::default(), cdda_volume: 0x4000, cdda_level: 0x4000,
            audio: Vec::new(), pcm_acc: (0, 0, 0), pcm_last: (0, 0), cd_cache: None, dma_counter: 0, cdda_counter: 0,
            bios, pram: vec![0; 0x80000], wram: vec![0; 0x40000], bram: vec![0; 0x2000],
            io: Io::default(), cfm: 0, cfs: 0, command: [0; 8], status: [0; 8], irq: [Irq::default(); 7],
            reset_pending: true, timer_frequency: 0, timer_counter: 0, divider: 0, stopwatch: 0, later: Later::default(), bram_dirty: false,
        };
        let mut m = Self { cpu: M68000::new(), st, disc, debt: 0 };
        format_bram(&mut m.st.bram); // new backup RAM comes formatted (games refuse a blank one)
        m
    }

    /// Run the sub CPU for `mclk` master clocks of a `rate` Hz master clock.
    pub fn run(&mut self, mclk: i64, rate: i64) {
        self.debt += mclk * SUB_HZ;
        while self.debt > 0 {
            let st = &mut self.st;
            let clocks = if st.io.halt {
                16
            } else {
                if st.reset_pending {
                    st.reset_pending = false;
                    self.cpu = M68000::new();
                    self.cpu.reset(&mut SubBus(st))
                } else {
                    self.cpu.ipl = st.ipl();
                    self.cpu.step(&mut SubBus(st))
                }
            };
            st.clock(clocks, self.disc.as_mut());
            self.debt -= clocks as i64 * rate;
        }
    }

    /// The console's reset button: the CD unit starts over (backup RAM and the disc stay).
    pub fn reset(&mut self) {
        let bios = std::mem::take(&mut self.st.bios);
        let bram = std::mem::take(&mut self.st.bram);
        let disc = self.disc.take();
        *self = Mcd::new(bios, disc);
        self.st.bram = bram;
    }

    // ---------------- the Genesis side ----------------

    /// A word from 000000-3FFFFF (the CD unit's space when booting from CD). None = open bus.
    /// `dma`: a VDP DMA read, which gets word RAM one access late.
    pub fn ext_read(&mut self, a: u32, dma: bool) -> Option<u16> {
        let st = &mut self.st;
        let a = a & 0x3FFFFE & !(7 << 18);
        match a {
            0x000000..=0x01FFFF => Some(match a {
                0x70 => (st.io.vector_level4 >> 16) as u16,
                0x72 => st.io.vector_level4 as u16,
                _ => if st.bios.len() > a as usize { word(&st.bios, a as usize >> 1) } else { 0xFFFF },
            }),
            0x020000..=0x03FFFF => {
                if !st.io.request { return None; }
                Some(word(&st.pram, ((st.io.pram_bank as usize) << 16) | (a as usize & 0x1FFFF) >> 1))
            }
            0x200000..=0x23FFFF => {
                let i = st.main_wram_index(a)?;
                if !dma { return Some(word(&st.wram, i)); }
                let v = st.io.wram_latch;
                st.io.wram_latch = word(&st.wram, i);
                Some(v)
            }
            _ => None,
        }
    }

    pub fn ext_write(&mut self, a: u32, upper: bool, lower: bool, v: u16) {
        let st = &mut self.st;
        let a = a & 0x3FFFFE & !(7 << 18);
        match a {
            0x020000..=0x03FFFF if st.io.request => {
                let i = ((st.io.pram_bank as usize) << 16) | (a as usize & 0x1FFFF) >> 1;
                put(&mut st.pram, i, upper, lower, v);
            }
            0x200000..=0x23FFFF => if let Some(i) = st.main_wram_index(a) { put(&mut st.wram, i, upper, lower, v); },
            _ => {}
        }
    }

    /// Genesis registers A12000-A120FF.
    pub fn ext_io_read(&mut self, a: u32) -> u16 {
        let st = &mut self.st;
        let io = st.io.clone();
        match a & 0xFFFF3E {
            0xA12000 => io.run as u16 | (io.request as u16) << 1 | (st.irq[2].pending as u16) << 8 | (st.irq[2].enable as u16) << 15,
            0xA12002 => st.wram_bits() | (io.pram_bank as u16) << 6 | (io.pram_protect as u16) << 8,
            0xA12004 => st.cdc_dest_bits(),
            0xA12008 => if st.cdc.transfer.destination == 2 { let v = st.cdc.host_read(); st.sync_cdc_irq(); v } else { 0xFFFF },
            0xA12006 => io.vector_level4 as u16,
            0xA1200C => st.stopwatch & 0xFFF,
            0xA1200E => (st.cfm as u16) << 8 | st.cfs as u16,
            r @ 0xA12010..=0xA1201E => st.command[((r - 0xA12010) >> 1) as usize],
            r @ 0xA12020..=0xA1202E => st.status[((r - 0xA12020) >> 1) as usize],
            _ => 0,
        }
    }

    pub fn ext_io_write(&mut self, a: u32, upper: bool, lower: bool, v: u16) {
        let st = &mut self.st;
        match a & 0xFFFF3E {
            0xA12000 => {
                if lower {
                    let run = v & 1 != 0;
                    if st.io.run && !run { st.reset_cpu(); }
                    st.io.run = run;
                    st.io.request = if run { v & 2 != 0 } else { true }; // reset forces BUSREQ
                    st.io.halt = st.io.request;
                }
                if upper && v & 0x100 != 0 { st.irq[2].raise(); }
            }
            0xA12002 => {
                if lower {
                    if v & 2 == 0 { st.io.wram_switch_request = true; } else { st.io.wram_switch = true; }
                    st.io.pram_bank = (v >> 6) as u8 & 3;
                }
                if upper { st.io.pram_protect = (v >> 8) as u8; }
            }
            0xA12006 => {
                let mut x = st.io.vector_level4;
                if upper { x = (x & !0xFF00) | (v as u32 & 0xFF00); }
                if lower { x = (x & !0x00FF) | (v as u32 & 0x00FF); }
                st.io.vector_level4 = x;
            }
            0xA1200E => st.cfm = (v >> 8) as u8, // byte writes all land in the high byte
            r @ 0xA12010..=0xA1201E => put_word(&mut st.command[((r - 0xA12010) >> 1) as usize], upper, lower, v),
            _ => {}
        }
    }
}

fn put_word(w: &mut u16, upper: bool, lower: bool, v: u16) {
    if upper { *w = (*w & 0x00FF) | (v & 0xFF00); }
    if lower { *w = (*w & 0xFF00) | (v & 0x00FF); }
}

impl State {
    fn reset_cpu(&mut self) { self.reset_pending = true; }

    /// The interrupt level the sub CPU sees: the highest pending one.
    fn ipl(&self) -> u8 { (1..=6).rev().find(|&l| self.irq[l].pending).unwrap_or(0) as u8 }

    /// A12004 / FF8004 bits 8-15: DMA destination, data ready, transfer done.
    fn cdc_dest_bits(&self) -> u16 {
        let t = &self.cdc.transfer;
        (t.destination as u16) << 8 | (t.ready as u16) << 14 | (t.completed as u16) << 15
    }

    /// One CD audio sample (16-bit little-endian stereo) from an audio sector.
    fn cd_sample(&mut self, sector: i32, at: usize, disc: Option<&mut Disc>) -> (i32, i32) {
        if self.cd_cache.as_ref().map(|c| c.0) != Some(sector) {
            let mut raw = [0u8; 2352];
            let ok = sector >= 0 && disc.is_some_and(|d| d.read_sector(sector as u32, &mut raw));
            if !ok { raw.fill(0); }
            self.cd_cache = Some((sector, raw.to_vec()));
        }
        let b = &self.cd_cache.as_ref().unwrap().1;
        let at = at.min(2348);
        (i16::from_le_bytes([b[at], b[at + 1]]) as i32, i16::from_le_bytes([b[at + 2], b[at + 3]]) as i32)
    }

    /// The decoder's interrupt line drives level 5.
    fn sync_cdc_irq(&mut self) {
        if self.cdc.irq_line() { self.irq[5].raise(); } else { self.irq[5].pending = false; }
    }

    /// The gate array's clock: every 384 sub clocks the timer, the stopwatch and the drive
    /// tick; the decoder's DMA moves a word every 6.
    fn clock(&mut self, clocks: u32, mut disc: Option<&mut Disc>) {
        if self.gpu.active && self.gpu.step(clocks, &mut self.wram, self.io.wram_priority) { self.irq[1].raise(); }
        if self.cdc.transfer.active {
            self.dma_counter += clocks;
            while self.dma_counter >= 6 {
                self.dma_counter -= 6;
                let Some((dest, a, w)) = self.cdc.dma_word() else { break };
                match dest {
                    5 => self.sub_write(true, true, a & 0x7FFFE, w),
                    7 => {
                        let a = if !self.io.wram_mode { 0x080000 | (a & 0x3FFFE) } else { 0x0C0000 | (a & 0x1FFFE) };
                        self.sub_write(true, true, a, w);
                    }
                    4 => {
                        // PCM wave RAM takes bytes
                        let at = 0x1000 | ((a >> 1) & 0xFFF);
                        self.pcm.write(at | 1, (w >> 8) as u8);
                        self.pcm.write(at, w as u8);
                    }
                    _ => {}
                }
                self.sync_cdc_irq();
            }
        }
        self.cdda_counter += clocks as i64 * 44_100;
        while self.cdda_counter >= SUB_HZ {
            self.cdda_counter -= SUB_HZ;
            let (l, r) = match self.cdd.sample() {
                Some((sector, at)) => self.cd_sample(sector, at as usize, disc.as_deref_mut()),
                None => (0, 0),
            };
            let v = self.cdda_level as i32;
            let (l, r) = ((l * v / 0x4000) >> 1, (r * v / 0x4000) >> 1);
            if self.cdda_level < self.cdda_volume { self.cdda_level += 1; } else if self.cdda_level > self.cdda_volume { self.cdda_level -= 1; }
            let (pl, pr) = if self.pcm_acc.2 > 0 { (self.pcm_acc.0 / self.pcm_acc.2, self.pcm_acc.1 / self.pcm_acc.2) } else { self.pcm_last };
            self.pcm_acc = (0, 0, 0);
            self.audio.push(((l + pl) as f32 / 32768.0).clamp(-1.0, 1.0));
            self.audio.push(((r + pr) as f32 / 32768.0).clamp(-1.0, 1.0));
            if self.audio.len() > 16384 { self.audio.drain(..8192); } // nobody is listening
        }
        self.divider += clocks;
        while self.divider >= 384 {
            self.divider -= 384;
            let (l, r) = self.pcm.clock();
            self.pcm_acc = (self.pcm_acc.0 + l, self.pcm_acc.1 + r, self.pcm_acc.2 + 1);
            self.pcm_last = (l, r);
            let tick = self.cdd.clock();
            if tick.irq { self.irq[4].raise(); }
            if let Some(sector) = tick.decode {
                let mut raw = [0u8; 2352];
                let ok = sector >= 0 && disc.as_deref_mut().is_some_and(|d| d.read_sector(sector as u32, &mut raw));
                self.cdc.decode(sector, ok.then_some(&raw));
                self.sync_cdc_irq();
            }
            self.stopwatch = (self.stopwatch + 1) & 0xFFF;
            if self.timer_frequency != 0 {
                if self.timer_counter == 0 {
                    self.timer_counter = self.timer_frequency;
                    self.irq[3].raise();
                } else {
                    self.timer_counter -= 1;
                }
            }
        }
    }

    /// A12002 / FF8002 bits 0-2.
    fn wram_bits(&self) -> u16 {
        let io = &self.io;
        let (b0, b1) = if !io.wram_mode { (!io.wram_switch, io.wram_switch) } else { (io.wram_select, io.wram_switch_request) };
        b0 as u16 | (b1 as u16) << 1 | (io.wram_mode as u16) << 2
    }

    /// Word RAM as the Genesis sees it at 200000-23FFFF: a word index, or None when the
    /// sub CPU has it.
    fn main_wram_index(&self, a: u32) -> Option<usize> {
        let io = &self.io;
        if !io.wram_mode {
            if io.wram_switch { return None; }
            return Some((a as usize & 0x3FFFF) >> 1);
        }
        let mut a = a;
        if a >= 0x220000 {
            // cell-mapped windows: tiles laid out for the VDP
            a = if a < 0x230000 { (a & 0x0FC00) >> 8 | (a & 0x003FC) << 6 | a & 0x10002 }
                else if a < 0x238000 { (a & 0x07E00) >> 7 | (a & 0x001FC) << 6 | a & 0x18002 }
                else if a < 0x23C000 { (a & 0x03F00) >> 6 | (a & 0x000FC) << 6 | a & 0x1C002 }
                else { (a & 0x01F80) >> 5 | (a & 0x0007C) << 6 | a & 0x1E002 };
        }
        Some((a as usize & 0x1FFFF & !1) | io.wram_select as usize)
    }

    /// The sub CPU's 1M bank (the one the Genesis doesn't have).
    fn sub_bank(&self) -> usize { !self.io.wram_select as usize }

    // ---------------- the sub CPU's bus ----------------

    fn sub_read(&mut self, upper: bool, lower: bool, a: u32) -> u16 {
        let a = a & 0xFFFFFE;
        match a {
            0x000000..=0x07FFFF => word(&self.pram, a as usize >> 1),
            0x080000..=0x0BFFFF => {
                if !self.io.wram_mode {
                    if !self.io.wram_switch { return 0; }
                    word(&self.wram, (a as usize & 0x3FFFF) >> 1)
                } else {
                    // dot-mapped: each byte of word RAM is two 4-bit pixels, one per byte here
                    let ofs = if a & 2 != 0 { 0 } else { 8 };
                    let i = ((a as usize >> 1) & 0x1FFFF & !1) | self.sub_bank();
                    let w = word(&self.wram, i);
                    ((w >> (4 + ofs)) & 0xF) << 8 | ((w >> ofs) & 0xF)
                }
            }
            0x0C0000..=0x0DFFFF => {
                if !self.io.wram_mode { return 0; }
                word(&self.wram, (a as usize & 0x1FFFF & !1) | self.sub_bank())
            }
            0xFE0000..=0xFEFFFF => if lower { self.bram[(a as usize >> 1) & 0x1FFF] as u16 } else { 0 },
            0xFF0000..=0xFF7FFF => if lower { self.pcm.read(a >> 1) as u16 } else { 0 },
            0xFF8000..=0xFFFFFF => self.io_read(upper, lower, a),
            _ => 0,
        }
    }

    fn sub_write(&mut self, upper: bool, lower: bool, a: u32, v: u16) {
        let a = a & 0xFFFFFE;
        match a {
            0x000000..=0x07FFFF => {
                if a >= (self.io.pram_protect as u32) << 9 { put(&mut self.pram, a as usize >> 1, upper, lower, v); }
            }
            0x080000..=0x0BFFFF => {
                if !self.io.wram_mode {
                    // (the hardware holds the sub CPU until it gets word RAM; dropped here)
                    if self.io.wram_switch { put(&mut self.wram, (a as usize & 0x3FFFF) >> 1, upper, lower, v); }
                } else {
                    let ofs = if a & 2 != 0 { 0 } else { 8 };
                    let i = ((a as usize >> 1) & 0x1FFFF & !1) | self.sub_bank();
                    let mut w = word(&self.wram, i);
                    let mut nib = |w: &mut u16, at: u32, n: u16| {
                        let old = (*w >> at) & 0xF;
                        let write = match self.io.wram_priority {
                            0 => true,
                            1 => old == 0, // under write: only into empty pixels
                            2 => n != 0,   // over write: transparent pixels don't write
                            _ => false,
                        };
                        if write { *w = (*w & !(0xF << at)) | n << at; }
                    };
                    if upper { nib(&mut w, 4 + ofs, (v >> 8) & 0xF); }
                    if lower { nib(&mut w, ofs, v & 0xF); }
                    put(&mut self.wram, i, true, true, w);
                }
            }
            0x0C0000..=0x0DFFFF => {
                if self.io.wram_mode { let i = (a as usize & 0x1FFFF & !1) | self.sub_bank(); put(&mut self.wram, i, upper, lower, v); }
            }
            0xFE0000..=0xFEFFFF => if lower {
                let i = (a as usize >> 1) & 0x1FFF;
                if self.bram[i] != v as u8 { self.bram[i] = v as u8; self.bram_dirty = true; }
            },
            0xFF0000..=0xFF7FFF => if lower { self.pcm.write(a >> 1, v as u8) },
            0xFF8000..=0xFFFFFF => self.io_write(upper, lower, a, v),
            _ => {}
        }
    }

    /// Sub registers FF8000-FF81FF.
    fn io_read(&mut self, _upper: bool, _lower: bool, a: u32) -> u16 {
        let l = &self.later;
        match 0xFF8000 | (a & 0x1FE) {
            0xFF8000 => 1 | (l.led as u16) << 8, // peripherals ready, version 0
            0xFF8002 => self.wram_bits() | (self.io.wram_priority as u16) << 3 | (self.io.pram_protect as u16) << 8,
            0xFF8004 => self.cdc.address as u16 | self.cdc_dest_bits(),
            0xFF8006 => { let v = self.cdc.read() as u16; self.sync_cdc_irq(); v }
            0xFF8008 => if self.cdc.transfer.destination == 3 { let v = self.cdc.host_read(); self.sync_cdc_irq(); v } else { 0xFFFF },
            0xFF800A => (self.cdc.transfer.address >> 3) as u16,
            0xFF800C => self.stopwatch & 0xFFF,
            0xFF800E => (self.cfm as u16) << 8 | self.cfs as u16,
            r @ 0xFF8010..=0xFF801E => self.command[((r - 0xFF8010) >> 1) as usize],
            r @ 0xFF8020..=0xFF802E => self.status[((r - 0xFF8020) >> 1) as usize],
            0xFF8030 => self.timer_frequency as u16,
            0xFF8032 => (1..=6).fold(0, |m, l| m | (self.irq[l].enable as u16) << l),
            0xFF8036 => (self.cdd.host_clock as u16) << 2 | (self.cdd.current_is_data() as u16) << 8,
            r @ 0xFF8038..=0xFF8040 => { let i = (r - 0xFF8038) as usize; (self.cdd.status[i] as u16) << 8 | self.cdd.status[i + 1] as u16 }
            r @ 0xFF8042..=0xFF804A => { let i = (r - 0xFF8042) as usize; (self.cdd.command[i] as u16) << 8 | self.cdd.command[i + 1] as u16 }
            0xFF804C => l.font_colors as u16,
            0xFF804E => l.font_data,
            r @ 0xFF8050..=0xFF8056 => {
                // the font data as 4-bit pixels, four per word
                let index = 12 - ((r - 0xFF8050) << 1);
                let (fg, bg) = ((l.font_colors >> 4) as u16, (l.font_colors & 15) as u16);
                (0..4).fold(0, |w, k| w | (if l.font_data >> (index + k) & 1 != 0 { fg } else { bg }) << (k * 4))
            }
            0xFF8058 => { let g = &self.gpu; g.repeat as u16 | (g.tile_size as u16) << 1 | (g.map_size as u16) << 2 | (g.active as u16) << 15 }
            0xFF805A => ((self.gpu.map_base >> 9) as u16 & 0x1FF) << 7,
            0xFF805C => self.gpu.vcells as u16,
            0xFF805E => ((self.gpu.image_base >> 5) as u16 & 0x1FFF) << 3,
            0xFF8060 => self.gpu.offset as u16,
            0xFF8062 => self.gpu.hdots,
            0xFF8064 => self.gpu.vdots as u16,
            0xFF8066 => ((self.gpu.vector_base >> 3) as u16 & 0x7FFF) << 1,
            _ => 0,
        }
    }

    fn io_write(&mut self, upper: bool, lower: bool, a: u32, v: u16) {
        match 0xFF8000 | (a & 0x1FE) {
            0xFF8000 => {
                if upper { self.later.led = (v >> 8) as u8 & 3; }
                if lower && v & 1 == 0 { self.cdd.power(); } // peripheral reset: the drive starts over
            }
            0xFF8002 => {
                // all writes land in the low byte
                let select = v & 1 != 0;
                if self.io.wram_select != select { self.io.wram_switch_request = false; }
                self.io.wram_select = select;
                self.io.wram_mode = v & 4 != 0;
                self.io.wram_priority = (v >> 3) as u8 & 3;
                if select { self.io.wram_switch = false; } // RET: word RAM back to the Genesis (2M)
            }
            0xFF8004 => {
                if lower { self.cdc.address = v as u8 & 0xF; }
                if upper { self.cdc.transfer.destination = (v >> 8) as u8 & 7; }
            }
            0xFF8006 => if lower { self.cdc.write(v as u8); self.sync_cdc_irq(); },
            0xFF800A => self.cdc.transfer.address = (v as u32) << 3,
            0xFF800C => self.stopwatch = 0,
            0xFF800E => self.cfs = v as u8, // all writes land in the low byte
            r @ 0xFF8020..=0xFF802E => put_word(&mut self.status[((r - 0xFF8020) >> 1) as usize], upper, lower, v),
            0xFF8030 => { self.timer_frequency = v as u8; self.timer_counter = v as u8; }
            0xFF8032 => if lower { for l in 1..=6 { self.irq[l].enable = v >> l & 1 != 0; } },
            0xFF8034 => self.cdda_volume = v & 0xFFC0,
            0xFF8036 => if lower {
                let on = v & 4 != 0;
                if !self.cdd.host_clock && on { self.irq[4].raise(); }
                self.cdd.host_clock = on;
                self.cdd.counter = 0;
            },
            r @ 0xFF8042..=0xFF804A => {
                let i = (r - 0xFF8042) as usize;
                if upper { self.cdd.command[i] = (v >> 8) as u8 & 15; }
                if lower {
                    self.cdd.command[i + 1] = v as u8 & 15;
                    if i + 1 == 9 { self.cdd.process(); } // the last nibble sends it
                }
            }
            0xFF804C => self.later.font_colors = v as u8,
            0xFF804E => put_word(&mut self.later.font_data, upper, lower, v),
            0xFF8058 => if lower { self.gpu.repeat = v & 1 != 0; self.gpu.tile_size = v & 2 != 0; self.gpu.map_size = v & 4 != 0; },
            0xFF805A => self.gpu.map_base = (v as u32 & 0xFFE0) << 2,
            0xFF805C => self.gpu.vcells = v as u8 & 31,
            0xFF805E => self.gpu.image_base = (v as u32 & 0xFFF8) << 2,
            0xFF8060 => self.gpu.offset = v as u8 & 63,
            0xFF8062 => self.gpu.hdots = v & 0x1FF,
            0xFF8064 => self.gpu.vdots = v as u8,
            0xFF8066 => { self.gpu.vector_base = (v as u32 & 0xFFFE) << 2; self.gpu.start(self.io.wram_mode); }
            _ => {}
        }
    }
}

/// The sub CPU's view of the CD unit.
pub struct SubBus<'a>(pub &'a mut State);

impl M68kBus for SubBus<'_> {
    fn read_byte(&mut self, a: u32) -> u8 {
        let odd = a & 1 != 0;
        let w = self.0.sub_read(!odd, odd, a);
        if odd { w as u8 } else { (w >> 8) as u8 }
    }
    fn read_word(&mut self, a: u32) -> u16 { self.0.sub_read(true, true, a) }
    fn write_byte(&mut self, a: u32, v: u8) {
        let odd = a & 1 != 0;
        self.0.sub_write(!odd, odd, a, (v as u16) << 8 | v as u16);
    }
    fn write_word(&mut self, a: u32, v: u16) { self.0.sub_write(true, true, a, v) }
    /// Taking an interrupt clears that source (autovectored).
    fn interrupt_ack(&mut self, level: u8) -> Option<u8> {
        if (1..=6).contains(&level) { self.0.irq[level as usize].pending = false; }
        None
    }
}
