//! The SH7604: an SH-2 core plus what Hitachi put on the chip with it. Ported from ares
//! component/processor/sh2/sh7604 (bus.cpp, cache.cpp, dma.cpp, interrupts.cpp, io.cpp,
//! timer.cpp), ISC license.
//!
//! Address areas (address >> 29): 0 cached, 1 uncached (the same memory), 2 cache purge,
//! 3 cache address array, 6 cache data array (4 KB of fast RAM when the cache is off or
//! two-way), 7 on-chip registers (FFFFFE00-FFFFFFFF).
//! On-chip: 4 KB 4-way cache, 2-channel DMA, 32/32 and 64/32 divider, 16-bit free-running
//! timer, watchdog timer (interval mode), interrupt controller. The serial port (used by
//! a few games to link the two CPUs) only stores its registers; the bus state controller
//! and break controller store theirs too.

use crate::common::cpu::sh2::{Sh2, Sh2Bus};
use serde::{Deserialize, Serialize};

/// The world outside the chip: 16-bit accesses, addresses 0-1FFFFFFF. It counts its own
/// wait states (`take_waits`).
pub trait ExtBus {
    fn read(&mut self, a: u32) -> u16;
    fn write(&mut self, a: u32, v: u16, upper: bool, lower: bool);
}

const INVALID: u32 = 1 << 19;

#[derive(Clone, Serialize, Deserialize)]
pub struct Cache {
    pub lines: Vec<[u8; 16]>,
    pub tags: Vec<u32>,
    lrus: Vec<u8>,
    lru_select: Vec<u8>,
    lru_update: Vec<Vec<u8>>,
    pub enable: bool,
    pub disable_code: bool,
    pub disable_data: bool,
    /// 0 = 4-way, 2 = 2-way (ways 2 and 3 only)
    pub two_way: u8,
    pub way_select: u8,
}

impl Cache {
    fn new() -> Self {
        let mut lru_select = vec![3u8; 64];
        let mut lru_update = vec![vec![0u8; 64]; 4];
        for n in 0..64u8 {
            let b = |k: u8| n >> k & 1;
            lru_select[n as usize] = if b(5) == 1 && b(4) == 1 && b(3) == 1 { 0 }
                else if b(5) == 0 && b(2) == 1 && b(1) == 1 { 1 }
                else if b(4) == 0 && b(2) == 0 && b(0) == 1 { 2 }
                else { 3 }; // includes the half of the values no rule matches
            lru_update[0][n as usize] = n & !0b111000;
            lru_update[1][n as usize] = (n | 0b100000) & !0b000110;
            lru_update[2][n as usize] = (n | 0b010100) & !0b000001;
            lru_update[3][n as usize] = n | 0b001011;
        }
        Self { lines: vec![[0; 16]; 256], tags: vec![INVALID; 256], lrus: vec![0; 64], lru_select, lru_update,
               enable: false, disable_code: false, disable_data: false, two_way: 0, way_select: 0 }
    }

    /// The way holding `address`, if any (and the LRU bookkeeping for the hit).
    fn hit(&mut self, address: u32) -> Option<usize> {
        let entry = (address >> 4 & 63) as usize;
        let tag = address >> 10 & 0x7FFFF;
        let lru = self.lrus[entry] as usize;
        for way in (0..4).rev() {
            if self.tags[way << 6 | entry] == tag {
                self.lrus[entry] = self.lru_update[way][lru];
                return Some(way << 6 | entry);
            }
        }
        None
    }

    fn purge_line(&mut self, address: u32) {
        let entry = (address >> 4 & 63) as usize;
        let tag = address >> 10 & 0x7FFFF;
        for way in 0..4 { if self.tags[way << 6 | entry] == tag { self.tags[way << 6 | entry] |= INVALID; } }
    }

    fn purge_all(&mut self, ways: usize) {
        self.lrus.fill(0);
        for t in &mut self.tags[..64 * ways] { *t |= INVALID; }
    }
}

fn get(line: &[u8; 16], a: u32, size: u32) -> u32 {
    let i = (a & 15 & !(size - 1)) as usize;
    (0..size as usize).fold(0, |v, k| v << 8 | line[i + k] as u32)
}

fn put(line: &mut [u8; 16], a: u32, size: u32, v: u32) {
    let i = (a & 15 & !(size - 1)) as usize;
    for k in 0..size as usize { line[i + k] = (v >> (8 * (size as usize - 1 - k))) as u8; }
}

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
pub struct Chcr { pub de: bool, pub te: bool, pub ie: bool, pub ar: bool, pub ts: u8, pub sm: u8, pub dm: u8, pub raw: u8, pub raw_lo: u8 }

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Dmac {
    pub sar: [u32; 2],
    pub dar: [u32; 2],
    /// 24-bit transfer counts
    pub tcr: [u32; 2],
    pub chcr: [Chcr; 2],
    pub vcrdma: [u8; 2],
    pub drcr: [u8; 2],
    pub dme: bool,
    pub nmif: bool,
    pub ae: bool,
    pub pr: bool,
    /// DREQ lines from outside (32X: 0 = the 68000's FIFO, 1 = PWM)
    pub dreq: [bool; 2],
    pub pending_irq: u8,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Frt {
    pub ovie: bool, pub ociae: bool, pub ocibe: bool, pub icie: bool,
    pub cclra: bool, pub ovf: bool, pub ocfa: bool, pub ocfb: bool, pub icf: bool,
    pub frc: u16, pub ocra: u16, pub ocrb: u16, pub cks: u8, pub iedg: bool,
    pub olvla: bool, pub olvlb: bool, pub ocrs: bool, pub ficr: u16,
    counter: u32,
    pub pending_irq: bool,
}

impl Frt {
    fn run(&mut self, cycles: u32) {
        const DIV: [u32; 4] = [8, 32, 128, 1];
        let f = DIV[self.cks as usize & 3];
        self.counter += cycles;
        let inc = self.counter / f;
        self.counter -= inc * f;
        if inc == 0 { return; }
        let prev = self.frc;
        self.frc = self.frc.wrapping_add(inc as u16);
        self.ovf |= (prev as u32 + inc) > 0xFFFF;
        self.ocfa |= (self.ocra.wrapping_sub(1).wrapping_sub(prev) as u32) < inc;
        self.ocfb |= (self.ocrb.wrapping_sub(1).wrapping_sub(prev) as u32) < inc;
        self.pending_irq |= (self.ovie && self.ovf) || (self.ociae && self.ocfa) || (self.ocibe && self.ocfb);
        if self.cclra && self.ocfa { self.frc = 0; }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Wdt {
    pub cks: u8, pub tme: bool, pub wtit: bool, pub ovf: bool,
    pub rsts: bool, pub rste: bool, pub wovf: bool,
    pub wtcnt: u8,
    counter: u32,
    pub pending_irq: bool,
    select: u8,
}

impl Wdt {
    fn run(&mut self, cycles: u32) {
        const DIV: [u32; 8] = [2, 64, 128, 256, 512, 1024, 4096, 8192];
        if !self.tme || self.wtit { return; } // (watchdog reset mode isn't emulated)
        let f = DIV[self.cks as usize & 7];
        self.counter += cycles;
        let inc = self.counter / f;
        self.counter %= f;
        if inc == 0 { return; }
        let before = self.wtcnt as u32;
        self.wtcnt = (before + inc) as u8;
        if before + inc > 0xFF { self.ovf = true; self.pending_irq = true; }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Divu { pub dvsr: u32, pub ovf: bool, pub ovfie: bool, pub vcrdiv: u8, pub dvdnth: u32, pub dvdntl: u32 }

/// Everything on the chip but the CPU core.
#[derive(Clone, Serialize, Deserialize)]
pub struct Periph {
    pub cache: Cache,
    pub dmac: Dmac,
    pub frt: Frt,
    pub wdt: Wdt,
    pub divu: Divu,
    /// interrupt priorities: IPRA (DIVU, DMAC, WDT), IPRB (SCI, FRT)
    pub divuip: u8, pub dmacip: u8, pub wdtip: u8, pub sciip: u8, pub frtip: u8,
    /// vectors: FRT output compare, WDT interval
    pub focv: u8, pub witv: u8,
    /// on-chip registers that only hold their value (serial, BSC, UBC, other vectors ...)
    #[serde(with = "serde_bytes")]
    pub regs: Vec<u8>,
    /// sub-cycles to charge for the current instruction (cache refills)
    #[serde(skip)]
    pub waits: u32,
}

impl Periph {
    fn new() -> Self {
        let mut regs = vec![0u8; 0x200];
        regs[0x001] = 0xFF;          // BRR
        regs[0x003] = 0xFF;          // TDR
        regs[0x004] = 0x84;          // SSR: TDRE, TEND
        Self { cache: Cache::new(), dmac: Dmac::default(), frt: Frt::default(), wdt: Wdt::default(), divu: Divu::default(),
               divuip: 0, dmacip: 0, wdtip: 0, sciip: 0, frtip: 0, focv: 0, witv: 0, regs, waits: 0 }
    }

    // ---------------- on-chip registers ----------------

    fn io_read(&mut self, a: u32) -> u8 {
        let d = &self.divu;
        let b = |v: u32, k: u32| (v >> (8 * k)) as u8;
        match a {
            0xFFFF_FE10 => 1 | (self.frt.ovie as u8) << 1 | (self.frt.ocibe as u8) << 2 | (self.frt.ociae as u8) << 3 | (self.frt.icie as u8) << 7,
            0xFFFF_FE11 => self.frt.cclra as u8 | (self.frt.ovf as u8) << 1 | (self.frt.ocfb as u8) << 2 | (self.frt.ocfa as u8) << 3 | (self.frt.icf as u8) << 7,
            0xFFFF_FE12 => (self.frt.frc >> 8) as u8,
            0xFFFF_FE13 => self.frt.frc as u8,
            0xFFFF_FE14 => (if self.frt.ocrs { self.frt.ocrb } else { self.frt.ocra } >> 8) as u8,
            0xFFFF_FE15 => (if self.frt.ocrs { self.frt.ocrb } else { self.frt.ocra }) as u8,
            0xFFFF_FE16 => self.frt.cks | (self.frt.iedg as u8) << 7,
            0xFFFF_FE17 => self.frt.olvlb as u8 | (self.frt.olvla as u8) << 1 | (self.frt.ocrs as u8) << 4 | 0xE0,
            0xFFFF_FE18 => (self.frt.ficr >> 8) as u8,
            0xFFFF_FE19 => self.frt.ficr as u8,
            0xFFFF_FE60 => self.frtip | self.sciip << 4,
            0xFFFF_FE67 => self.focv,
            0xFFFF_FE71 => self.dmac.drcr[0],
            0xFFFF_FE72 => self.dmac.drcr[1],
            0xFFFF_FE80 => { let w = &self.wdt; w.cks | (w.tme as u8) << 5 | (w.wtit as u8) << 6 | (w.ovf as u8) << 7 }
            0xFFFF_FE81 => self.wdt.wtcnt,
            0xFFFF_FE83 => { let w = &self.wdt; (w.rsts as u8) << 5 | (w.rste as u8) << 6 | (w.wovf as u8) << 7 }
            0xFFFF_FE92 | 0xFFFF_FE93 => {
                let c = &self.cache;
                c.enable as u8 | (c.disable_code as u8) << 1 | (c.disable_data as u8) << 2 | ((c.two_way == 2) as u8) << 3 | c.way_select << 6
            }
            0xFFFF_FEE2 => self.dmacip | self.divuip << 4,
            0xFFFF_FEE3 => self.wdtip << 4,
            0xFFFF_FEE4 => self.witv,
            0xFFFF_FF00..=0xFFFF_FF03 => b(d.dvsr, 3 - (a & 3)),
            0xFFFF_FF04..=0xFFFF_FF07 => b(d.dvdntl, 3 - (a & 3)),
            0xFFFF_FF0B => d.ovf as u8 | (d.ovfie as u8) << 1,
            0xFFFF_FF0F => d.vcrdiv,
            0xFFFF_FF10..=0xFFFF_FF13 | 0xFFFF_FF18..=0xFFFF_FF1B => b(d.dvdnth, 3 - (a & 3)),
            0xFFFF_FF14..=0xFFFF_FF17 | 0xFFFF_FF1C..=0xFFFF_FF1F => b(d.dvdntl, 3 - (a & 3)),
            0xFFFF_FF80..=0xFFFF_FF9F => {
                let c = ((a >> 4) & 1) as usize;
                let dm = &self.dmac;
                match a & 15 {
                    0..=3 => b(dm.sar[c], 3 - (a & 3)),
                    4..=7 => b(dm.dar[c], 3 - (a & 3)),
                    9..=11 => b(dm.tcr[c], 11 - (a & 15)),
                    14 => dm.chcr[c].raw_lo,
                    15 => { let h = &dm.chcr[c]; h.de as u8 | (h.te as u8) << 1 | (h.ie as u8) << 2 | (h.raw & 0xF8) }
                    _ => 0,
                }
            }
            0xFFFF_FFA3 => self.dmac.vcrdma[0],
            0xFFFF_FFAB => self.dmac.vcrdma[1],
            0xFFFF_FFB3 => { let dm = &self.dmac; dm.dme as u8 | (dm.nmif as u8) << 1 | (dm.ae as u8) << 2 | (dm.pr as u8) << 3 }
            0xFFFF_FE00..=0xFFFF_FFFF => self.regs[(a & 0x1FF) as usize],
            _ => 0,
        }
    }

    fn io_write(&mut self, a: u32, v: u8) {
        let set = |x: &mut u32, k: u32| { *x = (*x & !(0xFF << (8 * k))) | (v as u32) << (8 * k); };
        match a {
            0xFFFF_FE04 => {
                // SSR: flags are cleared by writing 0; nothing is ever sent (no link yet), so
                // a cleared "transmit empty" comes straight back
                let r = &mut self.regs[4];
                *r = (*r & (v | 0x07)) | 0x84;
            }
            0xFFFF_FE10 => { let f = &mut self.frt; f.ovie = v & 2 != 0; f.ocibe = v & 4 != 0; f.ociae = v & 8 != 0; f.icie = v & 0x80 != 0; }
            0xFFFF_FE11 => {
                let f = &mut self.frt;
                f.cclra = v & 1 != 0;
                f.ovf &= v & 2 != 0; f.ocfb &= v & 4 != 0; f.ocfa &= v & 8 != 0; f.icf &= v & 0x80 != 0;
            }
            0xFFFF_FE12 => self.frt.frc = (self.frt.frc & 0xFF) | (v as u16) << 8,
            0xFFFF_FE13 => self.frt.frc = (self.frt.frc & 0xFF00) | v as u16,
            0xFFFF_FE14 | 0xFFFF_FE15 => {
                let r = if self.frt.ocrs { &mut self.frt.ocrb } else { &mut self.frt.ocra };
                *r = if a & 1 == 0 { (*r & 0xFF) | (v as u16) << 8 } else { (*r & 0xFF00) | v as u16 };
            }
            0xFFFF_FE16 => { self.frt.cks = v & 3; self.frt.iedg = v & 4 != 0; }
            0xFFFF_FE17 => { self.frt.olvlb = v & 1 != 0; self.frt.olvla = v & 2 != 0; self.frt.ocrs = v & 0x10 != 0; }
            0xFFFF_FE18 | 0xFFFF_FE19 => {}
            0xFFFF_FE60 => { self.frtip = v & 15; self.sciip = v >> 4; }
            0xFFFF_FE67 => self.focv = v & 0x7F,
            0xFFFF_FE71 => self.dmac.drcr[0] = v & 3,
            0xFFFF_FE72 => self.dmac.drcr[1] = v & 3,
            0xFFFF_FE80 => self.wdt.select = v,
            0xFFFF_FE81 => {
                let w = &mut self.wdt;
                if w.select == 0xA5 {
                    w.cks = v & 7; w.tme = v & 0x20 != 0; w.wtit = v & 0x40 != 0; w.ovf &= v & 0x80 != 0;
                    if !w.tme { w.wtcnt = 0; }
                }
                if w.select == 0x5A { w.wtcnt = v; }
            }
            0xFFFF_FE83 => { let w = &mut self.wdt; w.rsts = v & 0x20 != 0; w.rste = v & 0x40 != 0; w.wovf = v & 0x80 != 0; }
            0xFFFF_FE92 | 0xFFFF_FE93 => {
                let c = &mut self.cache;
                c.enable = v & 1 != 0;
                c.disable_code = v & 2 != 0;
                c.disable_data = v & 4 != 0;
                c.two_way = if v & 8 != 0 { 2 } else { 0 };
                c.way_select = v >> 6;
                if v & 8 != 0 { c.purge_all(2); }
                if v & 0x10 != 0 { c.purge_all(4); }
            }
            0xFFFF_FEE2 => { self.dmacip = v & 15; self.divuip = v >> 4; }
            0xFFFF_FEE3 => self.wdtip = v >> 4,
            0xFFFF_FEE4 => self.witv = v & 0x7F,
            0xFFFF_FF00..=0xFFFF_FF03 => set(&mut self.divu.dvsr, 3 - (a & 3)),
            0xFFFF_FF04..=0xFFFF_FF07 => {
                set(&mut self.divu.dvdntl, 3 - (a & 3));
                if a & 3 == 3 { self.divide32(); }
            }
            0xFFFF_FF0B => { self.divu.ovf = v & 1 != 0; self.divu.ovfie = v & 2 != 0; }
            0xFFFF_FF0F => self.divu.vcrdiv = v & 0x7F,
            0xFFFF_FF10..=0xFFFF_FF13 | 0xFFFF_FF18..=0xFFFF_FF1B => set(&mut self.divu.dvdnth, 3 - (a & 3)),
            0xFFFF_FF14..=0xFFFF_FF17 | 0xFFFF_FF1C..=0xFFFF_FF1F => {
                set(&mut self.divu.dvdntl, 3 - (a & 3));
                if a & 3 == 3 { self.divide64(); }
            }
            0xFFFF_FF80..=0xFFFF_FF9F => {
                let c = ((a >> 4) & 1) as usize;
                let dm = &mut self.dmac;
                match a & 15 {
                    0..=3 => set(&mut dm.sar[c], 3 - (a & 3)),
                    4..=7 => set(&mut dm.dar[c], 3 - (a & 3)),
                    9..=11 => set(&mut dm.tcr[c], 11 - (a & 15)),
                    14 => {
                        let h = &mut dm.chcr[c];
                        h.raw_lo = v;
                        h.ar = v & 2 != 0; h.ts = v >> 2 & 3; h.sm = v >> 4 & 3; h.dm = v >> 6;
                    }
                    15 => {
                        let h = &mut dm.chcr[c];
                        h.de = v & 1 != 0; h.te &= v & 2 != 0; h.ie = v & 4 != 0;
                        h.raw = v;
                    }
                    _ => {}
                }
            }
            0xFFFF_FFA3 => self.dmac.vcrdma[0] = v,
            0xFFFF_FFAB => self.dmac.vcrdma[1] = v,
            0xFFFF_FFB3 => {
                let dm = &mut self.dmac;
                dm.dme = v & 1 != 0; dm.nmif &= v & 2 != 0; dm.ae &= v & 4 != 0; dm.pr = v & 8 != 0;
            }
            0xFFFF_FE00..=0xFFFF_FFFF => self.regs[(a & 0x1FF) as usize] = v,
            _ => {}
        }
    }

    fn divide32(&mut self) {
        let d = &mut self.divu;
        if d.dvsr != 0 {
            let (n, m) = (d.dvdntl as i32, d.dvsr as i32);
            d.dvdntl = n.wrapping_div(m) as u32;
            d.dvdnth = n.wrapping_rem(m) as u32;
        } else {
            d.dvdntl = 0x7FFF_FFFF;
            d.ovf = true;
        }
    }

    fn divide64(&mut self) {
        let d = &mut self.divu;
        if d.dvsr != 0 {
            let n = ((d.dvdnth as u64) << 32 | d.dvdntl as u64) as i64;
            let m = d.dvsr as i32 as i64;
            let q = n.wrapping_div(m);
            d.dvdntl = q as u32;
            d.dvdnth = n.wrapping_rem(m) as u32;
            // (ares's bounds)
            if q > 0x7FFF_7FFF { d.dvdntl = 0x7FFF_FFFF; d.ovf = true; }
            if q < -0x8000_0000 { d.dvdntl = 0x8000_0000; d.ovf = true; }
        } else {
            d.dvdntl = 0x7FFF_FFFF;
            d.ovf = true;
        }
    }
}

/// The chip's view of memory for the CPU core (and its DMA).
pub struct ChipBus<'a> {
    pub p: &'a mut Periph,
    pub ext: &'a mut dyn ExtBus,
    /// the address of the instruction being executed (cache: code or data?)
    pub fetch: u32,
}

impl ChipBus<'_> {
    fn ext_read(&mut self, a: u32, size: u32) -> u32 {
        match size {
            1 => { let w = self.ext.read(a & !1); if a & 1 != 0 { w as u32 & 0xFF } else { (w >> 8) as u32 } }
            2 => self.ext.read(a & !1) as u32,
            _ => (self.ext.read(a & !3) as u32) << 16 | self.ext.read((a & !3) | 2) as u32,
        }
    }

    fn ext_write(&mut self, a: u32, size: u32, v: u32) {
        match size {
            1 => { let w = (v as u16 & 0xFF) * 0x101; self.ext.write(a & !1, w, a & 1 == 0, a & 1 != 0) }
            2 => self.ext.write(a & !1, v as u16, true, true),
            _ => { self.ext.write(a & !3, (v >> 16) as u16, true, true); self.ext.write((a & !3) | 2, v as u16, true, true) }
        }
    }

    /// A read through the cache (cached area, cache on).
    fn cached_read(&mut self, a: u32, size: u32) -> u32 {
        if let Some(i) = self.p.cache.hit(a) { return get(&self.p.cache.lines[i], a, size); }
        let c = &self.p.cache;
        let code = a == self.fetch;
        if (c.disable_code && code) || (c.disable_data && !code) { return self.ext_read(a & 0x1FFF_FFFF, size); }
        let entry = (a >> 4 & 63) as usize;
        let lru = c.lrus[entry] as usize;
        let way = (c.lru_select[lru] | c.two_way) as usize & 3;
        let index = way << 6 | entry;
        let c = &mut self.p.cache;
        c.lrus[entry] = c.lru_update[way][lru];
        c.tags[index] = a >> 10 & 0x7FFFF;
        let base = a & 0x1FFF_FFF0;
        let mut line = [0u8; 16];
        for k in 0..4 { put(&mut line, k * 4, 4, self.ext_read(base + k * 4, 4)); }
        self.p.cache.lines[index] = line;
        self.p.waits += 12;
        get(&line, a, size)
    }

    pub fn read(&mut self, a: u32, size: u32, through_cache: bool) -> u32 {
        match a >> 29 {
            0 if through_cache && self.p.cache.enable => self.cached_read(a, size),
            0 | 1 => self.ext_read(a & 0x1FFF_FFFF, size),
            3 if size == 4 => {
                let c = &self.p.cache;
                let entry = (a >> 4 & 63) as usize;
                let tag = c.tags[(c.way_select as usize) << 6 | entry];
                (tag & (0x7FFFF << 10)) | (c.lrus[entry] as u32) << 4 | ((tag & INVALID == 0) as u32) << 1
            }
            6 => get(&self.p.cache.lines[(a >> 4 & 255) as usize], a, size),
            7 => (0..size).fold(0, |v, k| v << 8 | self.p.io_read((a & !(size - 1)) + k) as u32),
            _ => 0,
        }
    }

    pub fn write(&mut self, a: u32, size: u32, v: u32, through_cache: bool) {
        match a >> 29 {
            0 => {
                if through_cache && self.p.cache.enable {
                    if let Some(i) = self.p.cache.hit(a) { put(&mut self.p.cache.lines[i], a, size, v); } // write-through
                }
                self.ext_write(a & 0x1FFF_FFFF, size, v)
            }
            1 => self.ext_write(a & 0x1FFF_FFFF, size, v),
            2 => self.p.cache.purge_line(a),
            3 if size == 4 => {
                let c = &mut self.p.cache;
                let entry = (a >> 4 & 63) as usize;
                c.lrus[entry] = (v >> 6 & 63) as u8;
                c.tags[(c.way_select as usize) << 6 | entry] = (a >> 10 & 0x7FFFF) | ((a >> 2 & 1) ^ 1) << 19;
            }
            6 => put(&mut self.p.cache.lines[(a >> 4 & 255) as usize], a, size, v),
            7 => for k in 0..size { self.p.io_write((a & !(size - 1)) + k, (v >> (8 * (size - 1 - k))) as u8) },
            _ => {}
        }
    }
}

impl Sh2Bus for ChipBus<'_> {
    fn read8(&mut self, a: u32) -> u8 { self.read(a, 1, true) as u8 }
    fn read16(&mut self, a: u32) -> u16 { self.read(a & !1, 2, true) as u16 }
    fn read32(&mut self, a: u32) -> u32 { self.read(a & !3, 4, true) }
    fn write8(&mut self, a: u32, v: u8) { self.write(a, 1, v as u32, true) }
    fn write16(&mut self, a: u32, v: u16) { self.write(a & !1, 2, v as u32, true) }
    fn write32(&mut self, a: u32, v: u32) { self.write(a & !3, 4, v, true) }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Sh7604 {
    pub cpu: Sh2,
    pub p: Periph,
}

impl Sh7604 {
    pub fn new() -> Self { Self { cpu: Sh2::new(), p: Periph::new() } }

    /// Power on / the reset line.
    pub fn reset(&mut self, warm: bool) {
        self.cpu.power(warm);
        self.p = Periph::new();
    }

    /// One instruction, then the on-chip interrupts and one DMA transfer. Returns the
    /// clocks it took (1 + cache refills; the outside world adds its own waits).
    pub fn step(&mut self, ext: &mut dyn ExtBus) -> u32 {
        self.p.waits = 0;
        let fetch = self.cpu.instruction_pc();
        {
            let mut bus = ChipBus { p: &mut self.p, ext: &mut *ext, fetch };
            self.cpu.step(&mut bus);
        }
        self.intc(&mut *ext);
        self.dma(&mut *ext);
        let clocks = 1 + self.p.waits;
        self.p.frt.run(clocks);
        self.p.wdt.run(clocks);
        clocks
    }

    /// Take an interrupt from outside the chip if the CPU accepts it now.
    pub fn external_irq(&mut self, ext: &mut dyn ExtBus, level: u8, vector: u8) -> bool {
        if !self.cpu.accepts(level) { return false; }
        let fetch = self.cpu.instruction_pc();
        let mut bus = ChipBus { p: &mut self.p, ext, fetch };
        self.cpu.interrupt(&mut bus, level, vector);
        true
    }

    /// The on-chip interrupt sources (ares INTC::run order: FRT, DMAC, WDT).
    fn intc(&mut self, ext: &mut dyn ExtBus) {
        if self.cpu.in_delay_slot() { return; }
        let p = &self.p;
        let take = if p.frt.pending_irq && self.cpu.sr.i < p.frtip {
            Some((p.frtip, p.focv, 0))
        } else if p.dmac.pending_irq != 0 && self.cpu.sr.i < p.dmacip {
            let c = if p.dmac.pending_irq & 1 != 0 { 0 } else { 1 };
            Some((p.dmacip, p.dmac.vcrdma[c] & 0x7F, 1 + c as u8))
        } else if p.wdt.pending_irq && self.cpu.sr.i < p.wdtip {
            Some((p.wdtip, p.witv, 3))
        } else {
            None
        };
        if let Some((level, vector, src)) = take {
            match src {
                0 => self.p.frt.pending_irq = false,
                1 => self.p.dmac.pending_irq &= !1,
                2 => self.p.dmac.pending_irq &= !2,
                _ => self.p.wdt.pending_irq = false,
            }
            let fetch = self.cpu.instruction_pc();
            let mut bus = ChipBus { p: &mut self.p, ext, fetch };
            self.cpu.interrupt(&mut bus, level, vector);
        }
    }

    /// One DMA transfer on the first channel that can go (ares: no cycle steal).
    fn dma(&mut self, ext: &mut dyn ExtBus) {
        for c in 0..2 {
            let d = &self.p.dmac;
            let h = d.chcr[c];
            if !(h.de && !h.te && d.dme && !d.nmif && !d.ae) { continue; }
            let go = if h.ar { true } else if d.drcr[c] == 0 { d.dreq[c] } else if d.drcr[c] == 3 { continue } else { false };
            if go { self.transfer(c, ext); }
            break;
        }
    }

    fn transfer(&mut self, c: usize, ext: &mut dyn ExtBus) {
        let fetch = self.cpu.instruction_pc();
        let mut bus = ChipBus { p: &mut self.p, ext, fetch };
        let (sar, dar) = (bus.p.dmac.sar[c], bus.p.dmac.dar[c]);
        let ts = bus.p.dmac.chcr[c].ts;
        match ts {
            0 => { let v = bus.read(sar, 1, false); bus.write(dar, 1, v, false); }
            1 => { let v = bus.read(sar & !1, 2, false); bus.write(dar & !1, 2, v, false); }
            2 => { let v = bus.read(sar & !3, 4, false); bus.write(dar & !3, 4, v, false); }
            _ => {
                for k in 0..4 { let v = bus.read((sar & !3) + k * 4, 4, false); bus.write((dar & !3) + k * 4, 4, v, false); }
            }
        }
        let d = &mut self.p.dmac;
        if ts == 3 {
            d.sar[c] = d.sar[c].wrapping_add(16); // always, whatever the mode
            d.tcr[c] = d.tcr[c].wrapping_sub(3) & 0xFF_FFFF;
        }
        const SSIZE: [u32; 4] = [1, 2, 4, 0];
        const DSIZE: [u32; 4] = [1, 2, 4, 16];
        let h = d.chcr[c];
        if h.sm == 1 { d.sar[c] = d.sar[c].wrapping_add(SSIZE[ts as usize]); }
        if h.sm == 2 { d.sar[c] = d.sar[c].wrapping_sub(SSIZE[ts as usize]); }
        if h.dm == 1 { d.dar[c] = d.dar[c].wrapping_add(DSIZE[ts as usize]); }
        if h.dm == 2 { d.dar[c] = d.dar[c].wrapping_sub(DSIZE[ts as usize]); }
        d.tcr[c] = d.tcr[c].wrapping_sub(1) & 0xFF_FFFF;
        if d.tcr[c] == 0 {
            d.chcr[c].te = true;
            if d.chcr[c].ie { d.pending_irq |= 1 << c; }
        }
    }
}

impl Default for Sh7604 {
    fn default() -> Self { Self::new() }
}
