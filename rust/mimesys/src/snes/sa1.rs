//! SA-1 (Super Accelerator): a second 65C816 at 10.74 MHz on the cartridge, with 2 KB of
//! I-RAM, the cartridge BW-RAM (also seen as 2 / 4 bit-per-pixel bitmaps), banked ROM
//! (Super MMC), DMA with character conversion, a multiply / divide / sum unit, a
//! variable-length bit reader and an H/V timer (Super Mario RPG, Kirby Super Star,
//! Kirby's Dream Land 3...). A port of ares's `sfc/coprocessor/sa1` (ISC, Near and the
//! ares team; see THIRD_PARTY_NOTICES.md), rewritten in Rust; the CPU core is our
//! W65816.
//!
//! It runs in step with the S-CPU (`run`, master clocks; a cycle is 2). BW-RAM is the
//! cartridge RAM (`Cart::sram`), ROM the cartridge ROM.
//!
//! Not done: the extra wait cycles when both CPUs hit the same memory (only ROM and
//! BW-RAM are counted, from the S-CPU's last address), the ROM penalty on jumps.

use serde::{Deserialize, Serialize};
use crate::common::cpu::w65816::{Bus816, W65816, I};

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Io {
    // 2200 CCNT
    sa1_irq: bool,
    sa1_rdyb: bool,
    sa1_resb: bool,
    sa1_nmi: bool,
    smeg: u8,
    // 2201 SIE / 2202 SIC
    cpu_irqen: bool,
    chdma_irqen: bool,
    cpu_irqcl: bool,
    chdma_irqcl: bool,
    crv: u16,
    cnv: u16,
    civ: u16,
    // 2209 SCNT
    cpu_irq: bool,
    cpu_ivsw: bool,
    cpu_nvsw: bool,
    cmeg: u8,
    // 220A CIE / 220B CIC
    sa1_irqen: bool,
    timer_irqen: bool,
    dma_irqen: bool,
    sa1_nmien: bool,
    sa1_irqcl: bool,
    timer_irqcl: bool,
    dma_irqcl: bool,
    sa1_nmicl: bool,
    snv: u16,
    siv: u16,
    // 2210 TMC
    hvselb: bool,
    ven: bool,
    hen: bool,
    hcnt: u16,
    vcnt: u16,
    // 2220-2223 Super MMC
    cbmode: bool,
    cb: u32,
    dbmode: bool,
    db: u32,
    ebmode: bool,
    eb: u32,
    fbmode: bool,
    fb: u32,
    sbm: u8,
    sw46: bool,
    cbm: u8,
    swen: bool,
    cwen: bool,
    bwp: u8,
    siwp: u8,
    ciwp: u8,
    // 2230 DCNT
    dmaen: bool,
    dprio: bool,
    cden: bool,
    cdsel: bool,
    dd: bool,
    sd: u8,
    // 2231 CDMA
    chdend: bool,
    dmasize: u8,
    dmacb: u8,
    dsa: u32,
    dda: u32,
    dtc: u16,
    bbf: bool,
    brf: [u8; 16],
    // 2250 MCNT
    acm: bool,
    md: bool,
    ma: u16,
    mb: u16,
    hl: bool,
    vb: u8,
    va: u32,
    vbit: u8,
    // 2300 SFR / 2301 CFR
    cpu_irqfl: bool,
    chdma_irqfl: bool,
    sa1_irqfl: bool,
    timer_irqfl: bool,
    dma_irqfl: bool,
    sa1_nmifl: bool,
    hcr: u16,
    vcr: u16,
    mr: u64,
    overflow: bool,
}

/// Everything but the SA-1's CPU registers.
#[derive(Clone, Serialize, Deserialize)]
pub struct State {
    pub io: Io,
    #[serde(with = "serde_bytes")]
    pub iram: Vec<u8>,
    hcounter: u32,
    vcounter: u32,
    scanlines: u32,
    bwram_dma: bool,
    dma_line: u32,
    interrupt_pending: bool,
    vector: u16,
    mdr: u8,
    /// the S-CPU's IRQ line, driven by the SA-1
    pub cpu_irq_line: bool,
    /// BW-RAM was written (the battery save is due)
    #[serde(skip)]
    pub bw_written: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Sa1 {
    pub cpu: W65816,
    pub st: State,
    debt: i64,
}

/// The SA-1's view of the cartridge while it runs an instruction.
struct Mem<'a> {
    st: &'a mut State,
    rom: &'a [u8],
    bwram: &'a mut [u8],
    /// the S-CPU's last bus address (for access conflicts)
    cpu_mar: u32,
    used: u32,
    /// reading vectors for an interrupt: 00:FFEx / FFFx give the SA-1's registers
    vector: Option<u16>,
}

fn mirror(addr: usize, size: usize) -> usize {
    if size == 0 { return 0; }
    // as ares's Bus::mirror: the part past the largest power of two repeats
    let (mut addr, mut size, mut base, mut mask) = (addr, size, 0usize, 1usize << 23);
    while addr >= size {
        while addr & mask == 0 { mask >>= 1; }
        addr -= mask;
        if size > mask { size -= mask; base += mask; }
        mask >>= 1;
    }
    base + addr
}

fn rom_conflict(mar: u32) -> bool { mar & 0x408000 == 0x008000 || mar & 0xC00000 == 0xC00000 }
fn bwram_conflict(mar: u32) -> bool { mar & 0x40E000 == 0x006000 || mar & 0xF00000 == 0x400000 }

impl State {
    fn new() -> Self {
        let mut io = Io { sa1_resb: true, db: 1, eb: 2, fb: 3, bwp: 0x0F, vb: 16, ..Default::default() };
        io.cb = 0;
        Self {
            io, iram: vec![0; 0x800], hcounter: 0, vcounter: 0, scanlines: 262, bwram_dma: false, dma_line: 0,
            interrupt_pending: false, vector: 0, mdr: 0, cpu_irq_line: false, bw_written: false,
        }
    }

    // ---------------- ROM ----------------

    fn rom_read(rom: &[u8], a: usize) -> u8 { if rom.is_empty() { 0 } else { rom[mirror(a, rom.len())] } }

    /// A ROM read by either CPU. `addr` is translated already: 00-3F/80-BF:8000-FFFF
    /// as 00-3F:0000-FFFF (LoROM pieces packed), C0-FF untranslated.
    fn rom_read_cpu(&self, rom: &[u8], addr: u32) -> u8 {
        let io = &self.io;
        if addr & 0xFFFFE0 == 0x007FE0 {
            if addr == 0x7FEA && io.cpu_nvsw { return io.snv as u8; }
            if addr == 0x7FEB && io.cpu_nvsw { return (io.snv >> 8) as u8; }
            if addr == 0x7FEE && io.cpu_ivsw { return io.siv as u8; }
            if addr == 0x7FEF && io.cpu_ivsw { return (io.siv >> 8) as u8; }
        }
        let lo = addr < 0x400000;
        let a = addr & 0x3FFFFF;
        let (mode, bank) = match a >> 20 {
            0 => (io.cbmode, io.cb),
            1 => (io.dbmode, io.db),
            2 => (io.ebmode, io.eb),
            _ => (io.fbmode, io.fb),
        };
        if lo && !mode { return Self::rom_read(rom, a as usize); }
        Self::rom_read(rom, (bank << 20 | a & 0xFFFFF) as usize)
    }

    fn translate_lorom(addr: u32) -> u32 {
        if addr & 0x408000 == 0x008000 { (addr & 0x800000) >> 2 | (addr & 0x3F0000) >> 1 | addr & 0x7FFF } else { addr }
    }

    // ---------------- BW-RAM ----------------

    fn bw_read(bw: &[u8], a: u32) -> u8 { if bw.is_empty() { 0 } else { bw[mirror(a as usize, bw.len())] } }
    fn bw_write(bw: &mut [u8], a: u32, v: u8) { if !bw.is_empty() { let i = mirror(a as usize, bw.len()); bw[i] = v; } }

    fn bw_protected(&self, a: u32) -> bool {
        !self.io.swen && !self.io.cwen && (a & 0x3FFFF) < (0x100 << self.io.bwp)
    }

    fn bw_read_bitmap(&self, bw: &[u8], a: u32) -> u8 {
        let a = a & 0xFFFFF;
        if !self.io.bbf {
            let v = Self::bw_read(bw, a >> 1);
            if a & 1 == 0 { v & 15 } else { v >> 4 }
        } else {
            (Self::bw_read(bw, a >> 2) >> ((a & 3) * 2)) & 3
        }
    }

    fn bw_write_bitmap(&self, bw: &mut [u8], a: u32, v: u8) {
        let a = a & 0xFFFFF;
        if !self.io.bbf {
            let old = Self::bw_read(bw, a >> 1);
            let n = if a & 1 == 0 { old & 0xF0 | v & 15 } else { old & 0x0F | (v & 15) << 4 };
            Self::bw_write(bw, a >> 1, n);
        } else {
            let s = (a & 3) * 2;
            let old = Self::bw_read(bw, a >> 2);
            Self::bw_write(bw, a >> 2, old & !(3 << s) | (v & 3) << s);
        }
    }

    fn bw_read_sa1(&self, bw: &[u8], a: u32) -> u8 {
        if !self.io.sw46 {
            Self::bw_read(bw, (self.io.cbm as u32 & 0x1F) * 0x2000 + (a & 0x1FFF))
        } else {
            self.bw_read_bitmap(bw, self.io.cbm as u32 * 0x2000 + (a & 0x1FFF))
        }
    }

    fn bw_write_sa1(&self, bw: &mut [u8], a: u32, v: u8) {
        if !self.io.sw46 {
            let a = (self.io.cbm as u32 & 0x1F) * 0x2000 + (a & 0x1FFF);
            if !self.bw_protected(a) { Self::bw_write(bw, a, v); }
        } else {
            self.bw_write_bitmap(bw, self.io.cbm as u32 * 0x2000 + (a & 0x1FFF), v);
        }
    }

    // ---------------- I-RAM ----------------

    fn iram_write_sa1(&mut self, a: u32, v: u8) {
        if self.io.ciwp >> ((a >> 8) & 7) & 1 == 0 { return; }
        self.iram[(a & 0x7FF) as usize] = v;
    }

    // ---------------- interrupts / timer ----------------

    fn trigger_timer(&mut self) {
        self.io.timer_irqfl = true;
        if self.io.timer_irqen { self.io.timer_irqcl = false; }
    }

    /// One SA-1 cycle passes (2 master clocks): the H/V timer.
    fn tick_timer(&mut self) {
        if !self.io.hvselb {
            self.hcounter += 2;
            if self.hcounter >= 1364 {
                self.hcounter = 0;
                self.vcounter += 1;
                if self.vcounter >= self.scanlines { self.vcounter = 0; }
            }
        } else {
            self.hcounter += 2;
            self.vcounter += self.hcounter >> 11;
            self.hcounter &= 0x7FF;
            self.vcounter &= 0x1FF;
        }
        let (h, v) = ((self.io.hcnt as u32) << 2, self.io.vcnt as u32);
        match (self.io.hen as u8) | (self.io.ven as u8) << 1 {
            1 if self.hcounter == h => self.trigger_timer(),
            2 if self.vcounter == v && self.hcounter == 0 => self.trigger_timer(),
            3 if self.vcounter == v && self.hcounter == h => self.trigger_timer(),
            _ => {}
        }
    }

    /// ares SA1::lastCycle: which interrupt the SA-1 takes next.
    fn check_interrupts(&mut self, p: u8, wai: &mut bool) {
        let io = &mut self.io;
        if io.sa1_nmi && !io.sa1_nmicl {
            self.interrupt_pending = true;
            self.vector = io.cnv;
            io.sa1_nmifl = true;
            io.sa1_nmicl = true;
            *wai = false;
        } else if p & I == 0 {
            if io.timer_irqen && !io.timer_irqcl {
                io.timer_irqfl = true;
            } else if io.dma_irqen && !io.dma_irqcl {
                io.dma_irqfl = true;
            } else if io.sa1_irq && !io.sa1_irqcl {
                io.sa1_irqfl = true;
            } else {
                return;
            }
            self.interrupt_pending = true;
            self.vector = io.civ;
            *wai = false;
        }
    }

    // ---------------- DMA ----------------

    fn dma_normal(&mut self, rom: &[u8], bw: &mut [u8]) -> u32 {
        let mut clocks = 0;
        while self.io.dtc != 0 {
            self.io.dtc = self.io.dtc.wrapping_sub(1);
            let (src, dst) = (self.io.dsa & 0xFFFFFF, self.io.dda & 0xFFFFFF);
            self.io.dsa = self.io.dsa.wrapping_add(1);
            self.io.dda = self.io.dda.wrapping_add(1);
            let data = match self.io.sd {
                0 => { clocks += 2; self.rom_read_cpu(rom, Self::translate_lorom(src)) }
                1 => { clocks += 4; Self::bw_read(bw, src) }
                _ => { clocks += 2; self.iram[(src & 0x7FF) as usize] }
            };
            if self.io.dd { if self.io.sd != 1 { clocks += 2; Self::bw_write(bw, dst, data); } }
            else if self.io.sd != 2 { self.iram[(dst & 0x7FF) as usize] = data; }
        }
        self.io.dma_irqfl = true;
        if self.io.dma_irqen { self.io.dma_irqcl = false; }
        clocks
    }

    fn dma_cc1(&mut self) {
        self.bwram_dma = true;
        self.io.chdma_irqfl = true;
        if self.io.chdma_irqen { self.io.chdma_irqcl = false; self.cpu_irq_line = true; }
    }

    /// Character conversion type 1: the S-CPU's DMA reads BW-RAM through here.
    fn dma_cc1_read(&mut self, bw: &[u8], addr: u32) -> u8 {
        let io = &self.io;
        let charmask = (1u32 << (6 - io.dmacb)) - 1;
        if addr & charmask == 0 {
            let bpp = 2u32 << (2 - io.dmacb);
            let bpl = (8u32 << io.dmasize) >> io.dmacb;
            let bwmask = (bw.len().max(1) - 1) as u32;
            let tile = (addr.wrapping_sub(io.dsa) & bwmask) >> (6 - io.dmacb);
            let ty = tile >> io.dmasize;
            let tx = tile & ((1 << io.dmasize) - 1);
            let mut bwaddr = io.dsa.wrapping_add(ty * 8 * bpl + tx * bpp);
            let (dda, dmacb) = (io.dda, io.dmacb);
            for y in 0..8u32 {
                let mut data = 0u64;
                for byte in 0..bpp { data |= (Self::bw_read(bw, (bwaddr + byte) & bwmask) as u64) << (byte << 3); }
                bwaddr = bwaddr.wrapping_add(bpl);
                let mut out = [0u8; 8];
                for x in 0..8 {
                    for p in 0..(bpp as usize) {
                        out[p] |= ((data & 1) as u8) << (7 - x);
                        data >>= 1;
                    }
                    let _ = dmacb;
                }
                for byte in 0..bpp {
                    let p = dda + (y << 1) + ((byte & 6) << 3) + (byte & 1);
                    self.iram[(p & 0x7FF) as usize] = out[byte as usize];
                }
            }
        }
        self.iram[((self.io.dda + (addr & charmask)) & 0x7FF) as usize]
    }

    /// Character conversion type 2: one line from the bitmap register file.
    fn dma_cc2(&mut self) {
        let base = ((self.dma_line & 1) << 3) as usize;
        let bpp = 2u32 << (2 - self.io.dmacb);
        let mut addr = self.io.dda & 0x7FF;
        addr &= !((1u32 << (7 - self.io.dmacb)) - 1);
        addr += (self.dma_line & 8) * bpp;
        addr += (self.dma_line & 7) * 2;
        for byte in 0..bpp {
            let mut out = 0u8;
            for bit in 0..8 { out |= ((self.io.brf[base + bit] >> byte) & 1) << (7 - bit); }
            self.iram[((addr + ((byte & 6) << 3) + (byte & 1)) & 0x7FF) as usize] = out;
        }
        self.dma_line = (self.dma_line + 1) & 15;
    }

    // ---------------- registers ----------------

    fn read_vbr(&self, rom: &[u8], bw: &[u8], a: u32) -> u8 {
        let a = a & 0xFFFFFF;
        if a & 0x408000 == 0x008000 || a & 0xC00000 == 0xC00000 { return self.rom_read_cpu(rom, Self::translate_lorom(a)); }
        if a & 0x40E000 == 0x006000 || a & 0xF00000 == 0x400000 { return Self::bw_read(bw, a); }
        if a & 0x40F800 == 0x000000 || a & 0x40F800 == 0x003000 { return self.iram[(a & 0x7FF) as usize]; }
        0xFF
    }

    fn read_io_sa1(&mut self, rom: &[u8], bw: &[u8], a: u32, data: u8) -> u8 {
        let io = &mut self.io;
        match 0x2200 | (a & 0x1FF) {
            0x2301 => (data & 0x00) | io.smeg & 15 | (io.sa1_nmifl as u8) << 4 | (io.dma_irqfl as u8) << 5
                      | (io.timer_irqfl as u8) << 6 | (io.sa1_irqfl as u8) << 7,
            0x2302 => { io.hcr = (self.hcounter >> 2) as u16; io.vcr = self.vcounter as u16; io.hcr as u8 }
            0x2303 => (io.hcr >> 8) as u8,
            0x2304 => io.vcr as u8,
            0x2305 => (io.vcr >> 8) as u8,
            0x2306..=0x230A => (io.mr >> (((a & 0x1FF) - 0x106) * 8)) as u8,
            0x230B => (io.overflow as u8) << 7,
            0x230C | 0x230D => {
                let va = io.va;
                let d = (self.read_vbr(rom, bw, va) as u32 | (self.read_vbr(rom, bw, va + 1) as u32) << 8
                         | (self.read_vbr(rom, bw, va + 2) as u32) << 16) >> self.io.vbit;
                if a & 1 == 0 { return d as u8; }
                if self.io.hl {
                    self.io.vbit += self.io.vb;
                    self.io.va = self.io.va.wrapping_add((self.io.vbit >> 3) as u32) & 0xFFFFFF;
                    self.io.vbit &= 7;
                }
                (d >> 8) as u8
            }
            _ => 0xFF,
        }
    }

    fn write_io_sa1(&mut self, rom: &[u8], bw: &mut [u8], a: u32, v: u8) -> u32 {
        let io = &mut self.io;
        match 0x2200 | (a & 0x1FF) {
            0x2209 => {
                io.cmeg = v & 15;
                io.cpu_nvsw = v & 0x10 != 0;
                io.cpu_ivsw = v & 0x40 != 0;
                io.cpu_irq = v & 0x80 != 0;
                if io.cpu_irq {
                    io.cpu_irqfl = true;
                    if io.cpu_irqen { io.cpu_irqcl = false; self.cpu_irq_line = true; }
                }
            }
            0x220A => {
                if !io.sa1_nmien && v & 0x10 != 0 && io.sa1_nmifl { io.sa1_nmicl = false; }
                if !io.dma_irqen && v & 0x20 != 0 && io.dma_irqfl { io.dma_irqcl = false; }
                if !io.timer_irqen && v & 0x40 != 0 && io.timer_irqfl { io.timer_irqcl = false; }
                if !io.sa1_irqen && v & 0x80 != 0 && io.sa1_irqfl { io.sa1_irqcl = false; }
                io.sa1_nmien = v & 0x10 != 0;
                io.dma_irqen = v & 0x20 != 0;
                io.timer_irqen = v & 0x40 != 0;
                io.sa1_irqen = v & 0x80 != 0;
            }
            0x220B => {
                io.sa1_nmicl = v & 0x10 != 0;
                io.dma_irqcl = v & 0x20 != 0;
                io.timer_irqcl = v & 0x40 != 0;
                io.sa1_irqcl = v & 0x80 != 0;
                if io.sa1_nmicl { io.sa1_nmifl = false; }
                if io.sa1_irqcl { io.sa1_irqfl = false; }
                if io.timer_irqcl { io.timer_irqfl = false; }
                if io.dma_irqcl { io.dma_irqfl = false; }
            }
            0x220C => io.snv = io.snv & 0xFF00 | v as u16,
            0x220D => io.snv = io.snv & 0xFF | (v as u16) << 8,
            0x220E => io.siv = io.siv & 0xFF00 | v as u16,
            0x220F => io.siv = io.siv & 0xFF | (v as u16) << 8,
            0x2210 => { io.hen = v & 1 != 0; io.ven = v & 2 != 0; io.hvselb = v & 0x80 != 0; }
            0x2211 => { self.vcounter = 0; self.hcounter = 0; }
            0x2212 => io.hcnt = io.hcnt & 0xFF00 | v as u16,
            0x2213 => io.hcnt = io.hcnt & 0xFF | (v as u16) << 8,
            0x2214 => io.vcnt = io.vcnt & 0xFF00 | v as u16,
            0x2215 => io.vcnt = io.vcnt & 0xFF | (v as u16) << 8,
            0x2225 => { io.cbm = v & 0x7F; io.sw46 = v & 0x80 != 0; }
            0x2227 => io.cwen = v & 0x80 != 0,
            0x222A => io.ciwp = v,
            0x2230 => {
                io.sd = v & 3;
                io.dd = v & 4 != 0;
                io.cdsel = v & 0x10 != 0;
                io.cden = v & 0x20 != 0;
                io.dprio = v & 0x40 != 0;
                io.dmaen = v & 0x80 != 0;
                if !io.dmaen { self.dma_line = 0; }
            }
            0x2231..=0x2237 => return self.write_io_shared(rom, bw, a, v),
            0x2238 => io.dtc = io.dtc & 0xFF00 | v as u16,
            0x2239 => io.dtc = io.dtc & 0xFF | (v as u16) << 8,
            0x223F => io.bbf = v & 0x80 != 0,
            0x2240..=0x224F => {
                let i = (a & 15) as usize;
                io.brf[i] = v;
                if (i == 7 || i == 15) && io.dmaen && io.cden && !io.cdsel { self.dma_cc2(); }
            }
            0x2250 => { io.md = v & 1 != 0; io.acm = v & 2 != 0; if io.acm { io.mr = 0; } }
            0x2251 => io.ma = io.ma & 0xFF00 | v as u16,
            0x2252 => io.ma = io.ma & 0xFF | (v as u16) << 8,
            0x2253 => io.mb = io.mb & 0xFF00 | v as u16,
            0x2254 => {
                io.mb = io.mb & 0xFF | (v as u16) << 8;
                if !io.acm {
                    if !io.md {
                        io.mr = ((io.ma as i16 as i32) * (io.mb as i16 as i32)) as u32 as u64;
                        io.mb = 0;
                    } else {
                        if io.mb == 0 {
                            io.mr = 0;
                        } else {
                            let dividend = io.ma as i16 as i32;
                            let divisor = io.mb as i32;
                            let rem = if dividend >= 0 { dividend % divisor } else { (dividend % divisor + divisor) % divisor };
                            let quot = (dividend - rem) / divisor;
                            io.mr = ((rem as u16 as u64) << 16) | quot as u16 as u64;
                        }
                        io.ma = 0;
                        io.mb = 0;
                    }
                } else {
                    let p = (io.ma as i16 as i64) * (io.mb as i16 as i64);
                    let sum = (io.mr as i64).wrapping_add(p) as u64;
                    io.overflow = sum >> 40 & 1 != 0;
                    io.mr = sum & 0xFF_FFFF_FFFF;
                    io.mb = 0;
                }
            }
            0x2258 => {
                io.vb = v & 15;
                io.hl = v & 0x80 != 0;
                if io.vb == 0 { io.vb = 16; }
                if !io.hl {
                    io.vbit += io.vb;
                    io.va = io.va.wrapping_add((io.vbit >> 3) as u32) & 0xFFFFFF;
                    io.vbit &= 7;
                }
            }
            0x2259 => io.va = io.va & 0xFFFF00 | v as u32,
            0x225A => io.va = io.va & 0xFF00FF | (v as u32) << 8,
            0x225B => { io.va = io.va & 0x00FFFF | (v as u32) << 16; io.vbit = 0; }
            _ => {}
        }
        0
    }

    /// 2231-2237, written by either CPU. Returns master clocks a DMA took.
    fn write_io_shared(&mut self, rom: &[u8], bw: &mut [u8], a: u32, v: u8) -> u32 {
        let io = &mut self.io;
        match 0x2200 | (a & 0x1FF) {
            0x2231 => {
                io.dmacb = (v & 3).min(2);
                io.dmasize = (v >> 2 & 7).min(5);
                io.chdend = v & 0x80 != 0;
                if io.chdend { self.bwram_dma = false; }
            }
            0x2232 => io.dsa = io.dsa & 0xFFFF00 | v as u32,
            0x2233 => io.dsa = io.dsa & 0xFF00FF | (v as u32) << 8,
            0x2234 => io.dsa = io.dsa & 0x00FFFF | (v as u32) << 16,
            0x2235 => io.dda = io.dda & 0xFFFF00 | v as u32,
            0x2236 => {
                io.dda = io.dda & 0xFF00FF | (v as u32) << 8;
                if io.dmaen {
                    if !io.cden && !io.dd { return self.dma_normal(rom, bw); }
                    if io.cden && io.cdsel { self.dma_cc1(); }
                }
            }
            0x2237 => {
                io.dda = io.dda & 0x00FFFF | (v as u32) << 16;
                if io.dmaen && !io.cden && io.dd { return self.dma_normal(rom, bw); }
            }
            _ => {}
        }
        0
    }
}

impl Mem<'_> {
    fn step(&mut self) {
        self.used += 2;
        self.st.tick_timer();
    }

    fn read_inner(&mut self, a: u32) -> u8 {
        let a = a & 0xFFFFFF;
        if let Some(v) = self.vector {
            // vectors for this interrupt: the SA-1's own registers
            if a & 0xFFFFF0 == 0x00FFE0 || a & 0xFFFFF0 == 0x00FFF0 {
                if a & 0xF == 0xA || a & 0xF == 0xE { return v as u8; }
                if a & 0xF == 0xB || a & 0xF == 0xF { return (v >> 8) as u8; }
            }
        }
        let data = self.st.mdr;
        if a & 0x40FE00 == 0x002200 {
            self.step();
            let (rom, bw) = (self.rom, &*self.bwram);
            return self.st.read_io_sa1(rom, bw, a, data);
        }
        if a & 0x408000 == 0x008000 || a & 0xC00000 == 0xC00000 {
            self.step();
            if rom_conflict(self.cpu_mar) { self.step(); }
            return self.st.rom_read_cpu(self.rom, State::translate_lorom(a));
        }
        if a & 0x40E000 == 0x006000 || a & 0xE00000 == 0x400000 || a & 0xF00000 == 0x600000 {
            self.step();
            self.step();
            if bwram_conflict(self.cpu_mar) { self.step(); self.step(); }
            if a & 0x600000 == 0x600000 { return self.st.bw_read_bitmap(self.bwram, a); }
            if a & 0x400000 != 0 { return State::bw_read(self.bwram, a); }
            return self.st.bw_read_sa1(self.bwram, a);
        }
        if a & 0x40F800 == 0x000000 || a & 0x40F800 == 0x003000 {
            self.step();
            return self.st.iram[(a & 0x7FF) as usize];
        }
        self.step();
        data
    }
}

impl Bus816 for Mem<'_> {
    fn read(&mut self, a: u32) -> u8 {
        let v = self.read_inner(a);
        self.st.mdr = v;
        v
    }

    fn write(&mut self, a: u32, v: u8) {
        let a = a & 0xFFFFFF;
        self.st.mdr = v;
        if a & 0x40FE00 == 0x002200 {
            self.step();
            let rom = self.rom;
            self.used += self.st.write_io_sa1(rom, self.bwram, a, v);
            return;
        }
        if a & 0x408000 == 0x008000 || a & 0xC00000 == 0xC00000 { self.step(); return; }
        if a & 0x40E000 == 0x006000 || a & 0xE00000 == 0x400000 || a & 0xF00000 == 0x600000 {
            self.step();
            self.step();
            if bwram_conflict(self.cpu_mar) { self.step(); self.step(); }
            self.st.bw_written = true;
            if a & 0x600000 == 0x600000 { return self.st.bw_write_bitmap(self.bwram, a, v); }
            if a & 0x400000 != 0 {
                if !self.st.bw_protected(a) { State::bw_write(self.bwram, a, v); }
                return;
            }
            return self.st.bw_write_sa1(self.bwram, a, v);
        }
        if a & 0x40F800 == 0x000000 || a & 0x40F800 == 0x003000 {
            self.step();
            return self.st.iram_write_sa1(a, v);
        }
        self.step();
    }

    fn idle(&mut self) { self.step(); }

    fn interrupt_pending(&self) -> bool { self.st.interrupt_pending }
}

impl Sa1 {
    pub fn new() -> Self { Self { cpu: W65816::new(), st: State::new(), debt: 0 } }

    /// Master clocks pass: run the SA-1 up to the same point in time.
    pub fn run(&mut self, clocks: u32, rom: &[u8], bwram: &mut [u8], cpu_mar: u32) {
        self.debt += clocks as i64;
        while self.debt > 0 {
            let mut m = Mem { st: &mut self.st, rom, bwram: &mut *bwram, cpu_mar, used: 0, vector: None };
            if m.st.io.sa1_rdyb || m.st.io.sa1_resb {
                // asleep: only the timer runs
                let n = (self.debt as u32 + 1) / 2;
                for _ in 0..n { m.st.tick_timer(); }
                self.debt -= n as i64 * 2;
                continue;
            }
            let mut wai = self.cpu.wai;
            m.st.check_interrupts(self.cpu.p, &mut wai);
            self.cpu.wai = wai;
            if m.st.interrupt_pending {
                m.st.interrupt_pending = false;
                m.vector = Some(m.st.vector);
                // ares SA1::interrupt: the vector comes from CNV / CIV, bank 0
                self.cpu.interrupt(&mut m, false);
            } else {
                self.cpu.step(&mut m);
            }
            self.debt -= m.used.max(2) as i64;
        }
    }

    // ---------------- the S-CPU side ----------------

    /// An S-CPU read the SA-1 board answers (I/O, I-RAM, BW-RAM, ROM); None = not ours.
    pub fn cpu_read(&mut self, rom: &[u8], bw: &[u8], a: u32, open: u8) -> Option<u8> {
        let bank = (a >> 16) as u8;
        let off = a as u16;
        let st = &mut self.st;
        if bank & 0x40 == 0 {
            match off {
                0x2200..=0x23FF => {
                    let io = &st.io;
                    return Some(match 0x2200 | (off & 0x1FF) {
                        0x2300 => (open & 0) | io.cmeg & 15 | (io.cpu_nvsw as u8) << 4 | (io.chdma_irqfl as u8) << 5
                                  | (io.cpu_ivsw as u8) << 6 | (io.cpu_irqfl as u8) << 7,
                        _ => open,
                    });
                }
                0x3000..=0x37FF => return Some(st.iram[(off & 0x7FF) as usize]),
                0x6000..=0x7FFF => {
                    let addr = st.io.sbm as u32 * 0x2000 + (off as u32 & 0x1FFF);
                    if st.bwram_dma { return Some(st.dma_cc1_read(bw, addr)); }
                    return Some(State::bw_read(bw, addr));
                }
                0x8000..=0xFFFF => return Some(st.rom_read_cpu(rom, State::translate_lorom(a))),
                _ => return None,
            }
        }
        if (0x40..=0x4F).contains(&(bank & 0xFF)) {
            if st.bwram_dma { return Some(st.dma_cc1_read(bw, a & 0xFFFFF)); }
            return Some(State::bw_read(bw, a & 0xFFFFF));
        }
        if bank >= 0xC0 { return Some(st.rom_read_cpu(rom, a)); }
        None
    }

    /// An S-CPU write to the SA-1 board; returns whether it was ours.
    pub fn cpu_write(&mut self, rom: &[u8], bw: &mut [u8], a: u32, v: u8) -> bool {
        let bank = (a >> 16) as u8;
        let off = a as u16;
        if bank & 0x40 == 0 {
            match off {
                0x2200..=0x23FF => { self.write_io_cpu(rom, bw, off as u32, v); return true; }
                0x3000..=0x37FF => {
                    if self.st.io.siwp >> ((off >> 8) & 7) & 1 != 0 { self.st.iram[(off & 0x7FF) as usize] = v; }
                    return true;
                }
                0x6000..=0x7FFF => {
                    let addr = self.st.io.sbm as u32 * 0x2000 + (off as u32 & 0x1FFF);
                    if !self.st.bw_protected(addr) { State::bw_write(bw, addr, v); self.st.bw_written = true; }
                    return true;
                }
                0x8000..=0xFFFF => return true,
                _ => return false,
            }
        }
        if (0x40..=0x4F).contains(&bank) {
            let addr = a & 0xFFFFF;
            if !self.st.bw_protected(addr) { State::bw_write(bw, addr, v); self.st.bw_written = true; }
            return true;
        }
        bank >= 0xC0
    }

    fn write_io_cpu(&mut self, rom: &[u8], bw: &mut [u8], a: u32, v: u8) {
        let st = &mut self.st;
        let io = &mut st.io;
        match 0x2200 | (a & 0x1FF) {
            0x2200 => {
                if io.sa1_resb && v & 0x20 == 0 {
                    // the SA-1 leaves reset: PC from CRV, bank 0
                    self.cpu.pc = io.crv as u32;
                    self.cpu.b = 0;
                    self.cpu.stp = false;
                    self.cpu.wai = false;
                    io.ciwp = 0;
                }
                io.smeg = v & 15;
                io.sa1_nmi = v & 0x10 != 0;
                io.sa1_resb = v & 0x20 != 0;
                io.sa1_rdyb = v & 0x40 != 0;
                io.sa1_irq = v & 0x80 != 0;
                if io.sa1_irq { io.sa1_irqfl = true; if io.sa1_irqen { io.sa1_irqcl = false; } }
                if io.sa1_nmi { io.sa1_nmifl = true; if io.sa1_nmien { io.sa1_nmicl = false; } }
            }
            0x2201 => {
                if !io.chdma_irqen && v & 0x20 != 0 && io.chdma_irqfl { io.chdma_irqcl = false; st.cpu_irq_line = true; }
                if !io.cpu_irqen && v & 0x80 != 0 && io.cpu_irqfl { io.cpu_irqcl = false; st.cpu_irq_line = true; }
                io.chdma_irqen = v & 0x20 != 0;
                io.cpu_irqen = v & 0x80 != 0;
            }
            0x2202 => {
                io.chdma_irqcl = v & 0x20 != 0;
                io.cpu_irqcl = v & 0x80 != 0;
                if io.chdma_irqcl { io.chdma_irqfl = false; }
                if io.cpu_irqcl { io.cpu_irqfl = false; }
                if !io.chdma_irqfl && !io.cpu_irqfl { st.cpu_irq_line = false; }
            }
            0x2203 => io.crv = io.crv & 0xFF00 | v as u16,
            0x2204 => io.crv = io.crv & 0xFF | (v as u16) << 8,
            0x2205 => io.cnv = io.cnv & 0xFF00 | v as u16,
            0x2206 => io.cnv = io.cnv & 0xFF | (v as u16) << 8,
            0x2207 => io.civ = io.civ & 0xFF00 | v as u16,
            0x2208 => io.civ = io.civ & 0xFF | (v as u16) << 8,
            0x2220 => { io.cb = (v & 7) as u32; io.cbmode = v & 0x80 != 0; }
            0x2221 => { io.db = (v & 7) as u32; io.dbmode = v & 0x80 != 0; }
            0x2222 => { io.eb = (v & 7) as u32; io.ebmode = v & 0x80 != 0; }
            0x2223 => { io.fb = (v & 7) as u32; io.fbmode = v & 0x80 != 0; }
            0x2224 => io.sbm = v & 0x1F,
            0x2226 => io.swen = v & 0x80 != 0,
            0x2228 => io.bwp = v & 15,
            0x2229 => io.siwp = v,
            0x2231..=0x2237 => { st.write_io_shared(rom, bw, a, v); }
            _ => {}
        }
    }
}
