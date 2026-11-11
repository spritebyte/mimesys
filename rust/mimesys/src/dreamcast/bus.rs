//! The Dreamcast as the SH-4 sees it: the address map, the devices, the DMA engines and the
//! interrupt lines.
//!
//! Address map (physical = virtual & 1FFFFFFF outside P4; no MMU yet):
//! - area 0: BIOS 000000, flash 200000, Holly 5F6800-5F7FFF, PVR registers 5F8000, modem
//!   600000, AICA registers 700000, clock 710000, sound RAM 800000 (AICA mirrored at +2000000)
//! - area 1: video RAM, 04 = 64-bit view, 05 = 32-bit view (8 MB, mirrored at 06 / 07)
//! - area 3: 16 MB main RAM at 0C000000, mirrored to 0FFFFFFF
//! - area 4: tile accelerator: 10000000 parameters, 10800000 YUV, 11000000 direct texture
//!   path (64- or 32-bit view by SB_LMMODE0); 12000000-13FFFFFF the same with SB_LMMODE1
//! - 7C000000-7FFFFFFF operand-cache RAM when CCR.ORA is set
//! - E0000000-E3FFFFFF store queues, F0000000-F7FFFFFF cache / TLB arrays, FF000000 on-chip
//!   registers

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::aica::Aica;
use super::flash::Flash;
use super::gdrom::Gdrom;
use super::aica;
use super::yuv::{self, Yuv};
use crate::common::cpu::arm::arm7tdmi::Arm7tdmi;
use super::holly::{self, Holly, SbAction};
use super::maple::{self, DcPad};
use super::onchip::{DmaJob, OnChip};
use super::pvr::{self, Pvr};
use super::render;
use crate::common::cpu::sh4::{Event, Sh4Bus};

pub const RAM_SIZE: usize = 0x100_0000;
pub const VRAM_SIZE: usize = 0x80_0000;
pub const BIOS_SIZE: usize = 0x20_0000;
/// SH-4 cycles per second
pub const CLOCK: u64 = 200_000_000;
/// The AICA's ARM7: 22.5792 MHz, ~4 cycles an instruction with sound-RAM waits
/// (about 5.6 million instructions a second).
pub const ARM_CLOCK: u64 = 22_579_200;
pub const ARM_CPI: u64 = 4;

/// G1 registers (words from 5F7400): GD-ROM DMA start address, length, direction, enable,
/// start / busy, and the read-back address and count.
const G1_GDSTAR: usize = 0x04 / 4;
const G1_GDLEN: usize = 0x08 / 4;
const G1_GDEN: usize = 0x14 / 4;
const G1_GDST: usize = 0x18 / 4;
const G1_GDSTARD: usize = 0xF4 / 4;
const G1_GDLEND: usize = 0xF8 / 4;

#[derive(Clone, Serialize, Deserialize)]
pub struct Dc {
    #[serde(skip)]
    pub bios: Vec<u8>,
    pub flash: Flash,
    pub ram: Vec<u8>,
    pub vram: Vec<u8>,
    pub aica: Aica,
    pub holly: Holly,
    pub pvr: Pvr,
    pub gd: Gdrom,
    pub chip: OnChip,
    /// CPU cycles since power-on
    pub now: u64,
    /// when the sync generator starts its next line
    pub next_line: u64,
    /// delayed Holly interrupts (DMA ends, render ends): (cycle, bits)
    events: Vec<(u64, u32)>,
    /// the AICA's ARM7 (sound CPU), the SH-4 cycle it has run up to, its clock remainder
    #[serde(default)]
    pub arm: Arm7tdmi,
    #[serde(default)]
    arm_now: u64,
    #[serde(default)]
    arm_acc: u64,
    #[serde(default)]
    arm_running: bool,
    /// ARM instructions run, and a sample of where (debug)
    #[serde(default)]
    pub arm_steps: u64,
    #[serde(skip)]
    pub arm_pcs: std::collections::HashMap<u32, u64>,
    #[serde(default)]
    pub yuv: Yuv,
    /// host time spent rendering / making sound / running the ARM7 (the speed line)
    #[serde(skip)]
    pub t_render: std::time::Duration,
    #[serde(skip)]
    pub t_sound: std::time::Duration,
    #[serde(skip)]
    pub t_arm: std::time::Duration,
    #[serde(skip)]
    pub t_render_wait: std::time::Duration,
    /// render on another thread while the CPU runs (DcSystem turns this on)
    #[serde(skip)]
    pub render_async: bool,
    #[serde(skip)]
    pub render_job: render::RenderJob,
    /// textures and the render thread's copy of video RAM, kept between renders
    #[serde(skip)]
    pub rcache: render::RenderCache,
    /// sound traffic since the last speed line: SH-4 bytes written to sound RAM, AICA DMAs,
    /// their bytes, DMA starts without SB_ADEN; the last DMA's G2 address, system address,
    /// length, direction | trigger select << 8
    #[serde(skip)]
    pub snd_io: [u64; 4],
    #[serde(skip)]
    pub g2_last: [u32; 4],
    /// renders, polygons, pixels, translucent fragments since the last speed line
    #[serde(skip)]
    pub r_sum: (u64, u64, u64, u64),
    /// when the running GD-ROM DMA ends
    #[serde(default)]
    gd_dma_end: Option<u64>,
    ta_buf: [u32; 8],
    ta_fill: usize,
    /// a field ended since the frontend last looked
    #[serde(skip)]
    pub vblank: bool,
    /// the interrupt to take: (level, INTEVT)
    #[serde(skip)]
    pub irq: Option<(u32, u32)>,
    /// controllers on ports A-D
    #[serde(skip)]
    pub pads: [Option<DcPad>; 4],
    /// SB_SFRES: the program asked for a reset
    #[serde(skip)]
    pub soft_reset: bool,
    /// addresses no device answered: (write?, count), the first 64
    #[serde(skip)]
    pub unknown: BTreeMap<u32, (bool, u32)>,
    /// Maple DMAs run (debug)
    pub maple_dmas: u64,
    /// debug: log chip-register accesses (with the PC) into `reg_log`
    #[serde(skip)]
    pub trace_regs: bool,
    #[serde(skip)]
    pub reg_log: Vec<String>,
    /// debug: the instruction running
    #[serde(skip)]
    pub cur_pc: u32,
}

impl Dc {
    pub fn new(bios: Vec<u8>, flash: &[u8], unix_now: u64) -> Self {
        let mut d = Self {
            bios, flash: Flash::new(flash), ram: vec![0; RAM_SIZE], vram: vec![0; VRAM_SIZE],
            aica: Aica::new(unix_now), holly: Holly::new(), pvr: Pvr::new(), gd: Gdrom::new(), chip: OnChip::new(),
            now: 0, next_line: 0, events: Vec::new(), t_render: Default::default(), t_sound: Default::default(), t_arm: Default::default(), t_render_wait: Default::default(), render_async: false, render_job: Default::default(), rcache: Default::default(), snd_io: [0; 4], g2_last: [0; 4], r_sum: (0, 0, 0, 0), gd_dma_end: None, yuv: Yuv::default(), arm: Arm7tdmi::new(), arm_now: 0, arm_acc: 0, arm_running: false, arm_steps: 0, arm_pcs: Default::default(), ta_buf: [0; 8], ta_fill: 0, vblank: false, irq: None,
            pads: [Some(DcPad::default()), Some(DcPad::default()), None, None], soft_reset: false,
            unknown: BTreeMap::new(), maple_dmas: 0, trace_regs: false, reg_log: Vec::new(), cur_pc: 0,
        };
        d.next_line = d.pvr.line_cycles();
        d
    }

    /// Everything but memory contents back to power-on.
    pub fn reset(&mut self) {
        self.holly.reset();
        self.pvr.reset();
        self.gd.reset();
        self.chip.reset();
        self.aica.reset();
        self.events.clear();
        self.gd_dma_end = None;
        self.arm_running = false;
        self.ta_fill = 0;
        self.next_line = self.now + self.pvr.line_cycles();
        self.update_irq();
    }

    fn note(&mut self, a: u32, write: bool) {
        if self.unknown.len() < 64 || self.unknown.contains_key(&a) {
            let e = self.unknown.entry(a).or_insert((write, 0));
            e.1 += 1;
        }
    }

    // ------------------------------------------------------------ interrupts

    /// The interrupt to offer the CPU: Holly's IRL level or an on-chip source, the higher.
    pub fn update_irq(&mut self) {
        self.holly.set_ext(holly::EXT_GDROM, self.gd.irq);
        let level = self.holly.level();
        let irl = (level > 0).then(|| (level, 0x3E0 - level * 0x20));
        let chip = self.chip.pending_irq();
        self.irq = match (irl, chip) {
            (Some(a), Some(b)) => Some(if b.0 > a.0 { b } else { a }),
            (a, b) => a.or(b),
        };
    }

    fn raise_later(&mut self, delay: u64, bits: u32) { self.events.push((self.now + delay, bits)); }

    /// Catch up with time: lines, timers, delayed interrupts.
    pub fn service(&mut self) {
        while self.now >= self.next_line {
            let ev = self.pvr.next_line();
            self.holly.raise(ev.ist);
            if ev.maple && self.holly.maple[5] & 1 != 0 && self.holly.maple[4] & 1 != 0 { self.maple_dma(); }
            if ev.vblank { self.vblank = true; }
            self.next_line += self.pvr.line_cycles();
        }
        self.chip.tmu_update(self.now);
        if !self.events.is_empty() {
            let now = self.now;
            let mut bits = 0;
            self.events.retain(|&(t, b)| if t <= now { bits |= b; false } else { true });
            self.holly.raise(bits);
        }
        self.run_arm();
        if self.gd_dma_end.is_some_and(|t| t <= self.now) {
            self.gd_dma_end = None;
            self.holly.g1[G1_GDST] = 0;
            self.holly.raise(holly::IST_DMA_GDROM);
            self.gd.dma_finish();
            self.holly.set_ext(holly::EXT_GDROM, self.gd.irq);
        }
        self.update_irq();
    }

    /// Cycle of the next thing `service` has to do.
    pub fn next_event(&self) -> u64 {
        let mut t = self.next_line;
        for &(at, _) in &self.events { t = t.min(at); }
        if let Some(at) = self.gd_dma_end { t = t.min(at); }
        if let Some(s) = self.aica.next_timer() {
            let at = ((self.aica.samples + s) * CLOCK).div_ceil(aica::SAMPLE_RATE);
            t = t.min(at.max(self.now + 1));
        }
        if let Some(d) = self.chip.tmu_next() { t = t.min(self.now + d.max(1)); }
        t
    }

    // ------------------------------------------------------------ DMA

    /// Wait for a render running on its own thread and write its picture to video RAM
    /// (before the next render, the picture output, a save state).
    pub fn finish_render(&mut self) {
        if let Some(h) = self.render_job.0.take() {
            let t = std::time::Instant::now();
            let (d, snap) = h.join().expect("render thread");
            self.rcache.snap = Some(snap);
            if let Some((lo, hi)) = d.write(&mut self.vram) { self.rcache.mark(lo, hi); }
            self.pvr.last_render = d.stats;
            self.count_render();
            self.t_render += d.took;
            self.t_render_wait += t.elapsed();
        }
    }

    /// Add the last render to the speed line's totals.
    fn count_render(&mut self) {
        let st = self.pvr.last_render;
        self.r_sum = (self.r_sum.0 + 1, self.r_sum.1 + st.polys as u64, self.r_sum.2 + st.pixels, self.r_sum.3 + st.trans);
    }

    /// Bring the AICA up to `now`: its timers, and its ARM7 (when out of reset) at
    /// ARM_CLOCK with ARM_CPI cycles an instruction; then its interrupt to the SH-4.
    fn run_arm(&mut self) {
        let now = self.now;
        let gd = &mut self.gd;
        let target = now * aica::SAMPLE_RATE / CLOCK;
        if target > self.aica.samples {
            let t = std::time::Instant::now();
            self.aica.advance_cd(target, &mut |_| gd.cdda());
            self.t_sound += t.elapsed();
        }
        if self.aica.arm_reset {
            self.arm_running = false;
            self.arm_now = now;
        } else {
            if !self.arm_running {
                self.arm = Arm7tdmi::new();
                self.arm.reset(&mut self.aica);
                self.arm_running = true;
                self.arm_now = now;
                self.arm_acc = 0;
            }
            self.arm_acc += (now - self.arm_now) * ARM_CLOCK;
            self.arm_now = now;
            let per = CLOCK * ARM_CPI;
            let n = self.arm_acc / per;
            self.arm_acc %= per;
            let t = std::time::Instant::now();
            for k in 0..n {
                self.arm.fiq_line = self.aica.arm_fiq();
                self.arm.step(&mut self.aica);
                if (self.arm_steps + k) & 63 == 0 { *self.arm_pcs.entry(self.arm.pc()).or_insert(0) += 1; }
            }
            self.arm_steps += n;
            self.t_arm += t.elapsed();
        }
        self.holly.set_ext(holly::EXT_AICA, self.aica.sh4_irq());
    }

    /// G1 DMA from the GD-ROM drive (SB_GDSTAR / GDLEN; GDDIR 1 = to memory) into RAM.
    /// The copy happens now; GDST, the DMA-end interrupt and the drive's completion come
    /// after the time the transfer would take (~10 cycles a byte, faster than a real 12x
    /// drive).
    fn gd_dma(&mut self) {
        let dst = self.holly.g1[G1_GDSTAR] & 0x1FFF_FFE0;
        let len = self.holly.g1[G1_GDLEN] as usize;
        let data = self.gd.dma_take(len);
        if dst >> 24 & 0x1C == 0x0C {
            for (k, b) in data.iter().enumerate() { self.ram[(dst as usize + k) & (RAM_SIZE - 1)] = *b; }
        } else {
            for (k, b) in data.iter().enumerate() { self.write(dst + k as u32, *b as u32, 1); }
        }
        self.holly.g1[G1_GDSTARD] = dst + data.len() as u32;
        self.holly.g1[G1_GDLEND] = data.len() as u32;
        self.gd_dma_end = Some(self.now + 2_000 + data.len() as u64 * 10);
    }

    fn maple_dma(&mut self) {
        let table = self.holly.maple[1]; // SB_MDSTAR
        maple::dma(&mut self.ram, table, &self.pads);
        self.maple_dmas += 1;
        self.raise_later(20_000, holly::IST_DMA_MAPLE);
    }

    fn ch2_dma(&mut self) {
        let src = self.chip.dma[2].sar;
        let len = match self.holly.r(holly::SB_C2DLEN) { 0 => 0x100_0000, n => n };
        let dst = (self.holly.r(holly::SB_C2DSTAT) & 0x03FF_FFE0) | 0x1000_0000;
        let texture = dst & 0x0100_0000 != 0;
        for k in (0..len).step_by(4) {
            let v = self.read(src.wrapping_add(k), 4);
            self.write(if texture { dst.wrapping_add(k) } else { dst }, v, 4);
        }
        let dar = self.chip.dma[2].dar;
        self.chip.dma_done(2, src.wrapping_add(len), dar);
        self.holly.sb[0] = if texture { dst.wrapping_add(len) } else { dst };
        self.holly.sb[1] = 0;
        self.holly.sb[2] = 0;
        self.raise_later((len as u64 / 4).max(100), holly::IST_DMA_CH2);
    }

    fn run_job(&mut self, j: DmaJob) {
        let step = |mode: u32, unit: u32| -> u32 { match mode { 1 => unit, 2 => unit.wrapping_neg(), _ => 0 } };
        let (mut s, mut d) = (j.src, j.dst);
        for _ in 0..j.count {
            let mut k = 0;
            while k < j.unit {
                let n = j.unit.min(4);
                let v = self.read(s.wrapping_add(k), n);
                self.write(d.wrapping_add(k), v, n);
                k += n;
            }
            s = s.wrapping_add(step(j.sm, j.unit));
            d = d.wrapping_add(step(j.dm, j.unit));
        }
        self.chip.dma_done(j.channel, s, d);
        self.update_irq();
    }

    /// PVR DMA (5F7C00): system RAM <-> video RAM.
    fn pvr_dma(&mut self) {
        let r = &self.holly.pvrdma;
        let (pvr_a, sys_a, len, to_sys) = (r[0], r[1], r[2], r[3] & 1 != 0);
        for k in (0..len).step_by(4) {
            if to_sys {
                let v = self.read(pvr_a.wrapping_add(k), 4);
                self.write(sys_a.wrapping_add(k), v, 4);
            } else {
                let v = self.read(sys_a.wrapping_add(k), 4);
                self.write(pvr_a.wrapping_add(k), v, 4);
            }
        }
        self.holly.pvrdma[6] = 0;
        self.raise_later((len as u64 / 4).max(100), holly::IST_DMA_PVR);
    }

    /// AICA DMA (G2 channel 0, 5F7800): system RAM <-> sound RAM.
    fn aica_dma(&mut self) {
        let r = &self.holly.g2;
        let (g2_a, sys_a, len, to_sys) = (r[0], r[1], r[2] & 0x7FFF_FFFF, r[3] & 1 != 0);
        self.snd_io[1] += 1;
        self.snd_io[2] += len as u64;
        self.g2_last = [g2_a, sys_a, r[2], r[3] | r[4] << 8];
        for k in (0..len).step_by(4) {
            if to_sys {
                let v = self.read(g2_a.wrapping_add(k), 4);
                self.write(sys_a.wrapping_add(k), v, 4);
            } else {
                let v = self.read(sys_a.wrapping_add(k), 4);
                self.write(g2_a.wrapping_add(k), v, 4);
            }
        }
        self.holly.g2[6] = 0;
        self.raise_later((len as u64 / 4).max(100), holly::IST_DMA_AICA);
    }

    // ------------------------------------------------------------ memory

    #[inline]
    fn le(mem: &[u8], i: usize, size: u32) -> u32 {
        match size {
            1 => mem[i] as u32,
            2 => u16::from_le_bytes([mem[i], mem[i + 1]]) as u32,
            _ => u32::from_le_bytes([mem[i], mem[i + 1], mem[i + 2], mem[i + 3]]),
        }
    }

    #[inline]
    fn put(mem: &mut [u8], i: usize, v: u32, size: u32) {
        mem[i..i + size as usize].copy_from_slice(&v.to_le_bytes()[..size as usize]);
    }

    /// The byte offset in video RAM of an area-1 address.
    #[inline]
    fn vram_index(p: u32) -> usize {
        let o = p & 0x7F_FFFF;
        if p & 0x0100_0000 != 0 { pvr::map32(o) } else { o as usize }
    }

    /// Part of a 32-bit register for a byte / word access.
    fn part(v: u32, a: u32, size: u32) -> u32 {
        if size >= 4 { return v; }
        (v >> ((a & 3) * 8)) & ((1u32 << (size * 8)) - 1)
    }

    /// Is this a chip register (on-chip, Holly, PVR, AICA, clock)? (debug trace)
    fn is_reg(a: u32) -> bool {
        if a >= 0xFF00_0000 { return true; }
        if a >= 0xE000_0000 { return false; }
        let p = a & 0x1FFF_FFFF;
        (0x005F_6800..0x0060_0000).contains(&p) || (0x0070_0000..0x0071_0010).contains(&p)
    }

    fn trace(&mut self, a: u32, v: u32, size: u32, write: bool) {
        let line = format!("{:08X} {} {a:08X}{} {v:0w$X}", self.cur_pc, if write { "W" } else { "R" }, [".b", ".w", "", ".l"][size.min(4) as usize - 1], w = size as usize * 2);
        // a polling loop repeats itself: count it instead
        if let Some(last) = self.reg_log.last_mut() {
            let (body, n) = match last.rsplit_once(" x") { Some((b, n)) if n.parse::<u32>().is_ok() => (b.to_string(), n.parse::<u32>().unwrap()), _ => (last.clone(), 1) };
            if body == line { *last = format!("{body} x{}", n + 1); return; }
        }
        if self.reg_log.len() < 400 { self.reg_log.push(line); }
    }

    pub fn read(&mut self, a: u32, size: u32) -> u32 {
        let v = self.read_inner(a, size);
        if self.trace_regs && Self::is_reg(a) { self.trace(a, v, size, false); }
        v
    }

    fn read_inner(&mut self, a: u32, size: u32) -> u32 {
        if a >= 0xE000_0000 {
            if a < 0xE400_0000 { return self.chip.sq[(a >> 5 & 1) as usize][(a >> 2 & 7) as usize]; }
            if (0xFFD8_0000..0xFFD8_0030).contains(&a) { self.chip.tmu_update(self.now); }
            return match self.chip.read(a, size) {
                Some(v) => v,
                None => { self.note(a, false); 0 }
            };
        }
        if a >> 26 == 0x1F && self.chip.ora() {
            let i = self.chip.ocram_index(a);
            return Self::le(&self.chip.ocram, i, size);
        }
        let p = a & 0x1FFF_FFFF;
        match p >> 26 {
            3 => Self::le(&self.ram, (p & 0xFF_FFFF) as usize, size),
            0 => self.area0_read(p, size),
            1 => Self::le(&self.vram, Self::vram_index(p), size),
            _ => { self.note(a, false); 0 }
        }
    }

    fn area0_read(&mut self, p: u32, size: u32) -> u32 {
        let q = if (0x0270_0000..0x02A0_0000).contains(&p) { p - 0x0200_0000 } else { p };
        let r = q & !3;
        let v = match q {
            0..=0x1F_FFFF => return Self::le(&self.bios, q as usize, size),
            0x20_0000..=0x21_FFFF => {
                let mut v = 0;
                for k in 0..size { v |= (self.flash.read(q - 0x20_0000 + k) as u32) << (8 * k); }
                return v;
            }
            0x5F_6800..=0x5F_69FF => self.holly.read_sb(r - 0x5F_6800),
            0x5F_6C00..=0x5F_6CFF => {
                let i = ((r - 0x5F_6C00) / 4) as usize;
                if i == 6 { 0 } else { self.holly.maple[i] } // SB_MDST: DMAs finish at once
            }
            0x5F_7000..=0x5F_70FF => {
                let v = self.gd.read(r - 0x5F_7000);
                self.update_irq();
                v
            }
            0x5F_7400..=0x5F_74FF => self.holly.g1[((r - 0x5F_7400) / 4) as usize],
            0x5F_7800..=0x5F_78FF => {
                let i = ((r - 0x5F_7800) / 4) as usize;
                if r == 0x5F_7880 { 0x12 } else { self.holly.g2[i] } // SB_G2ID
            }
            0x5F_7C00..=0x5F_7CFF => self.holly.pvrdma[((r - 0x5F_7C00) / 4) as usize],
            0x5F_8150 => self.yuv.count,
            0x5F_8000..=0x5F_9FFF => self.pvr.read(r - 0x5F_8000),
            0x60_0000..=0x60_07FF => 0, // no modem
            0x70_0000..=0x70_7FFF => return self.aica.reg_read(q - 0x70_0000, size),
            0x71_0000..=0x71_000F => self.aica.rtc_read(r, self.now),
            0x80_0000..=0x9F_FFFF => return Self::le(&self.aica.wave, (q - 0x80_0000) as usize, size),
            _ => { self.note(p, false); 0 }
        };
        Self::part(v, q, size)
    }

    pub fn write(&mut self, a: u32, v: u32, size: u32) {
        if self.trace_regs && Self::is_reg(a) { self.trace(a, v, size, true); }
        self.write_inner(a, v, size)
    }

    fn write_inner(&mut self, a: u32, v: u32, size: u32) {
        if a >= 0xE000_0000 {
            if a < 0xE400_0000 {
                let q = (a >> 5 & 1) as usize;
                let w = &mut self.chip.sq[q][(a >> 2 & 7) as usize];
                *w = if size >= 4 { v } else {
                    let sh = (a & 3) * 8;
                    let m = ((1u32 << (size * 8)) - 1) << sh;
                    *w & !m | (v << sh) & m
                };
                return;
            }
            if (0xFFD8_0000..0xFFD8_0030).contains(&a) { self.chip.tmu_update(self.now); }
            let (known, job) = self.chip.write(a, v, size);
            if !known { self.note(a, true); }
            if let Some(j) = job { self.run_job(j); }
            self.update_irq();
            return;
        }
        if a >> 26 == 0x1F && self.chip.ora() {
            let i = self.chip.ocram_index(a);
            return Self::put(&mut self.chip.ocram, i, v, size);
        }
        let p = a & 0x1FFF_FFFF;
        match p >> 26 {
            3 => Self::put(&mut self.ram, (p & 0xFF_FFFF) as usize, v, size),
            0 => self.area0_write(p, v, size),
            1 => { let i = Self::vram_index(p); self.rcache.mark(i, i + size as usize - 1); Self::put(&mut self.vram, i, v, size) }
            4 => self.ta_write(p, v, size),
            _ => self.note(a, true),
        }
    }

    fn area0_write(&mut self, p: u32, v: u32, size: u32) {
        let q = if (0x0270_0000..0x02A0_0000).contains(&p) { p - 0x0200_0000 } else { p };
        let r = q & !3;
        match q {
            0..=0x1F_FFFF => {} // BIOS ROM
            0x20_0000..=0x21_FFFF => {
                for k in 0..size { self.flash.write(q - 0x20_0000 + k, (v >> (8 * k)) as u8); }
            }
            0x5F_6800..=0x5F_69FF => {
                match self.holly.write_sb(r - 0x5F_6800, v) {
                    SbAction::Ch2Dma => self.ch2_dma(),
                    SbAction::SoftReset => self.soft_reset = true,
                    SbAction::None => {}
                }
                self.update_irq();
            }
            0x5F_6C00..=0x5F_6CFF => {
                let i = ((r - 0x5F_6C00) / 4) as usize;
                self.holly.maple[i] = v;
                // SB_MDST: start (software trigger, enabled)
                if i == 6 && v & 1 != 0 && self.holly.maple[5] & 1 != 0 && self.holly.maple[4] & 1 == 0 { self.maple_dma(); }
            }
            0x5F_7000..=0x5F_70FF => { self.gd.write(r - 0x5F_7000, v); self.update_irq(); }
            0x5F_7400..=0x5F_74FF => {
                let i = ((r - 0x5F_7400) / 4) as usize;
                self.holly.g1[i] = v;
                if i == G1_GDST && v & 1 != 0 && self.holly.g1[G1_GDEN] & 1 != 0 { self.gd_dma(); }
            }
            0x5F_7800..=0x5F_78FF => {
                let i = ((r - 0x5F_7800) / 4) as usize;
                self.holly.g2[i] = v;
                // SB_ADST with SB_ADEN
                if i == 6 && v & 1 != 0 {
                    if self.holly.g2[5] & 1 != 0 { self.aica_dma(); } else { self.snd_io[3] += 1; }
                }
            }
            0x5F_7C00..=0x5F_7CFF => {
                let i = ((r - 0x5F_7C00) / 4) as usize;
                self.holly.pvrdma[i] = v;
                // SB_PDST with SB_PDEN
                if i == 6 && v & 1 != 0 && self.holly.pvrdma[5] & 1 != 0 { self.pvr_dma(); }
            }
            0x5F_8000..=0x5F_9FFF => {
                if r - 0x5F_8000 == yuv::TA_YUV_TEX_BASE { self.yuv.reset(); }
                if self.pvr.write(r - 0x5F_8000, v) {
                    self.finish_render();
                    let base = self.pvr.reg(render::PARAM_BASE);
                    let t = std::time::Instant::now();
                    if self.render_async {
                        // copies, so the CPU can change video RAM / start the next scene
                        let scene = self.pvr.ta.scene(base).cloned();
                        let snap = self.rcache.prepare(&self.vram);
                        self.render_job = render::RenderJob::start(self.pvr.regs.clone(), scene, snap, self.rcache.tex.clone());
                        self.t_render_wait += t.elapsed();
                    } else {
                        let scene = self.pvr.ta.scene(base);
                        self.rcache.tex.invalidate(&self.rcache.tex_dirty);
                        self.rcache.tex_dirty = Default::default();
                        let d = render::draw(&self.pvr.regs, scene, &self.vram, &self.rcache.tex);
                        if let Some((lo, hi)) = d.write(&mut self.vram) { self.rcache.mark(lo, hi); }
                        self.pvr.last_render = d.stats;
                        self.count_render();
                        self.t_render += t.elapsed();
                    }
                    // the render "finishes" after about 1 ms
                    self.raise_later(200_000, pvr::IST_EOR_ISP | pvr::IST_EOR_TSP | pvr::IST_EOR_VIDEO);
                }
            }
            0x60_0000..=0x60_07FF => {}
            0x70_0000..=0x70_7FFF => self.aica.reg_write(q - 0x70_0000, v, size),
            0x71_0000..=0x71_000F => self.aica.rtc_write(r, v, self.now),
            0x80_0000..=0x9F_FFFF => { self.snd_io[0] += size as u64; Self::put(&mut self.aica.wave, (q - 0x80_0000) as usize, v, size) }
            _ => self.note(p, true),
        }
    }

    /// Area 4: the tile accelerator's FIFOs and the direct texture paths.
    fn ta_write(&mut self, p: u32, v: u32, size: u32) {
        let o = p & 0x01FF_FFFF;
        if o >= 0x0100_0000 {
            // direct path to video RAM: 64-bit view, or 32-bit with SB_LMMODE
            let mode = self.holly.r(if p & 0x0200_0000 != 0 { holly::SB_LMMODE1 } else { holly::SB_LMMODE0 }) & 1;
            let i = if mode == 0 { (o & 0x7F_FFFF) as usize } else { pvr::map32(o & 0x7F_FFFF) };
            self.rcache.mark(i, i + size as usize - 1);
            return Self::put(&mut self.vram, i, v, size);
        }
        if o >= 0x80_0000 {
            let bytes = v.to_le_bytes();
            let (base, ctrl) = (self.pvr.reg(yuv::TA_YUV_TEX_BASE) & 0x7F_FFFF, self.pvr.reg(yuv::TA_YUV_TEX_CTRL));
            // the whole picture's bytes
            let (wb, hb) = ((ctrl & 0x3F) + 1, (ctrl >> 8 & 0x3F) + 1);
            self.rcache.mark(base as usize, base as usize + (wb * hb * 512) as usize);
            if self.yuv.push(&bytes[..size as usize], base, ctrl, &mut self.vram) { self.raise_later(2000, yuv::IST_YUV); }
            return;
        }
        if size < 4 { return; }
        self.ta_buf[self.ta_fill] = v;
        self.ta_fill += 1;
        if self.ta_fill == 8 {
            self.ta_fill = 0;
            let bits = self.pvr.ta.block(&self.ta_buf);
            if bits != 0 { self.raise_later(400, bits); }
        }
    }
}

/// Main RAM (area 3, any mirror, not the P4 region): the CPU's fast path.
#[inline]
fn is_ram(a: u32) -> bool { a & 0xE000_0000 != 0xE000_0000 && (a >> 26) & 7 == 3 }

impl Sh4Bus for Dc {
    #[inline]
    fn read8(&mut self, a: u32) -> u8 {
        if is_ram(a) { return self.ram[(a & 0xFF_FFFF) as usize]; }
        self.read(a, 1) as u8
    }
    #[inline]
    fn read16(&mut self, a: u32) -> u16 {
        if is_ram(a) { let i = (a & 0xFF_FFFE) as usize; return u16::from_le_bytes([self.ram[i], self.ram[i + 1]]); }
        self.read(a, 2) as u16
    }
    #[inline]
    fn read64(&mut self, a: u32) -> u64 {
        if is_ram(a) && a & 0xFF_FFF8 != 0xFF_FFF8 {
            let i = (a & 0xFF_FFFC) as usize;
            return u64::from_le_bytes(self.ram[i..i + 8].try_into().unwrap());
        }
        self.read32(a) as u64 | (self.read32(a.wrapping_add(4)) as u64) << 32
    }
    #[inline]
    fn write64(&mut self, a: u32, v: u64) {
        if is_ram(a) && a & 0xFF_FFF8 != 0xFF_FFF8 {
            let i = (a & 0xFF_FFFC) as usize;
            return self.ram[i..i + 8].copy_from_slice(&v.to_le_bytes());
        }
        self.write32(a, v as u32);
        self.write32(a.wrapping_add(4), (v >> 32) as u32);
    }
    #[inline]
    fn read32(&mut self, a: u32) -> u32 {
        if a & 0xE000_0000 != 0xE000_0000 && (a >> 26) & 7 == 3 {
            let i = (a & 0xFF_FFFC) as usize;
            return u32::from_le_bytes(self.ram[i..i + 4].try_into().unwrap());
        }
        self.read(a, 4)
    }
    #[inline]
    fn write8(&mut self, a: u32, v: u8) {
        if is_ram(a) { self.ram[(a & 0xFF_FFFF) as usize] = v; return; }
        self.write(a, v as u32, 1)
    }
    #[inline]
    fn write16(&mut self, a: u32, v: u16) {
        if is_ram(a) { let i = (a & 0xFF_FFFE) as usize; return self.ram[i..i + 2].copy_from_slice(&v.to_le_bytes()); }
        self.write(a, v as u32, 2)
    }
    #[inline]
    fn write32(&mut self, a: u32, v: u32) {
        if a & 0xE000_0000 != 0xE000_0000 && (a >> 26) & 7 == 3 {
            let i = (a & 0xFF_FFFC) as usize;
            return self.ram[i..i + 4].copy_from_slice(&v.to_le_bytes());
        }
        self.write(a, v, 4)
    }
    #[inline(always)]
    fn fetch16(&mut self, a: u32) -> u16 {
        if a & 0xE000_0000 != 0xE000_0000 {
            let p = a & 0x1FFF_FFFF;
            if p >> 26 == 3 { let i = (p & 0xFF_FFFE) as usize; return u16::from_le_bytes([self.ram[i], self.ram[i + 1]]); }
            if p < 0x20_0000 { let i = p as usize & !1; return u16::from_le_bytes([self.bios[i], self.bios[i + 1]]); }
        }
        self.read(a, 2) as u16
    }
    fn pref(&mut self, a: u32) {
        if a >> 26 != 0x38 { return; }
        let (dest, data) = self.chip.sq_flush(a);
        for (k, w) in data.iter().enumerate() { self.write(dest + 4 * k as u32, *w, 4); }
    }
    #[inline]
    fn irq(&self) -> Option<(u32, u32)> { self.irq }
    #[inline]
    fn now(&self) -> u64 { self.now }
    #[inline]
    fn tick(&mut self, cycles: u64) { self.now += cycles; }
    fn event(&mut self, reg: Event, v: u32) {
        match reg {
            Event::Tra => self.chip.tra = v,
            Event::Expevt => self.chip.expevt = v,
            Event::Intevt => self.chip.intevt = v,
        }
    }
}
