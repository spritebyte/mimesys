//! Super FX (GSU-1 / GSU-2 / MARIO CHIP): a 16-bit RISC CPU on the cartridge that draws
//! into its own RAM as SNES bitplane tiles (Star Fox, Yoshi's Island, Stunt Race FX,
//! Doom...). A port of ares's `component/processor/gsu` and `sfc/coprocessor/superfx`
//! (ISC, Near and the ares team; see THIRD_PARTY_NOTICES.md), rewritten in Rust.
//!
//! The GSU runs on the 21.477 MHz master clock in step with the CPU (`run`). Its RAM is
//! the cartridge RAM (`Cart::sram`), its program the cartridge ROM; the CPU reaches its
//! registers and 512-byte code cache at 3000-34FF.

use serde::{Deserialize, Serialize};

// SFR bits
const Z: u16 = 1 << 1;
const CY: u16 = 1 << 2;
const S: u16 = 1 << 3;
const OV: u16 = 1 << 4;
const G: u16 = 1 << 5;
const R: u16 = 1 << 6;
const ALT1: u16 = 1 << 8;
const ALT2: u16 = 1 << 9;
const B: u16 = 1 << 12;
const IRQ: u16 = 1 << 15;

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
struct PixelCache {
    offset: u16,
    bitpend: u8,
    data: [u8; 8],
}

/// What the GSU reaches: cartridge ROM and its RAM.
pub struct Mem<'a> {
    pub rom: &'a [u8],
    pub ram: &'a mut [u8],
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Gsu {
    r: [u16; 16],
    r14_mod: bool,
    r15_mod: bool,
    sfr: u16,
    pbr: u8,
    rombr: u8,
    rambr: u8,
    cbr: u16,
    scbr: u8,
    // SCMR
    ht: u8,
    ron: bool,
    ran: bool,
    md: u8,
    colr: u8,
    // POR
    por_obj: bool,
    por_freezehigh: bool,
    por_highnibble: bool,
    por_dither: bool,
    por_transparent: bool,
    bramr: bool,
    vcr: u8,
    cfgr_irq: bool,
    cfgr_ms0: bool,
    clsr: bool,
    pipeline: u8,
    ramaddr: u16,
    romcl: u32,
    romdr: u8,
    ramcl: u32,
    ramar: u16,
    ramdr: u8,
    sreg: usize,
    dreg: usize,
    #[serde(with = "serde_bytes")]
    cache: Vec<u8>,
    cache_valid: [bool; 32],
    pixelcache: [PixelCache; 2],
    /// clocks used by the current instruction, and owed from the CPU side
    used: u32,
    debt: i64,
    /// the CPU's IRQ line (set by STOP, cleared by reading 3031)
    pub irq: bool,
}

impl Gsu {
    pub fn new() -> Self {
        Self {
            r: [0; 16], r14_mod: false, r15_mod: false, sfr: 0, pbr: 0, rombr: 0, rambr: 0, cbr: 0, scbr: 0,
            ht: 0, ron: false, ran: false, md: 0, colr: 0,
            por_obj: false, por_freezehigh: false, por_highnibble: false, por_dither: false, por_transparent: false,
            bramr: false, vcr: 0x04, cfgr_irq: false, cfgr_ms0: false, clsr: false, pipeline: 0x01, ramaddr: 0,
            romcl: 0, romdr: 0, ramcl: 0, ramar: 0, ramdr: 0, sreg: 0, dreg: 0,
            cache: vec![0; 512], cache_valid: [false; 32],
            pixelcache: [PixelCache { offset: 0xFFFF, bitpend: 0, data: [0; 8] }; 2],
            used: 0, debt: 0, irq: false,
        }
    }

    // ---------------- flags / registers ----------------

    fn f(&self, b: u16) -> bool { self.sfr & b != 0 }
    fn setf(&mut self, b: u16, on: bool) { if on { self.sfr |= b } else { self.sfr &= !b } }
    fn alt1(&self) -> bool { self.f(ALT1) }
    fn alt2(&self) -> bool { self.f(ALT2) }

    fn set_r(&mut self, n: usize, v: u16) {
        self.r[n] = v;
        if n == 14 { self.r14_mod = true; }
        if n == 15 { self.r15_mod = true; }
    }
    fn sr(&self) -> u16 { self.r[self.sreg] }
    fn set_dr(&mut self, v: u16) { let d = self.dreg; self.set_r(d, v); }
    fn dr(&self) -> u16 { self.r[self.dreg] }
    fn sz(&mut self, v: u16) { self.setf(S, v & 0x8000 != 0); self.setf(Z, v == 0); }

    fn reset_prefix(&mut self) {
        self.sfr &= !(B | ALT1 | ALT2);
        self.sreg = 0;
        self.dreg = 0;
    }

    pub fn running(&self) -> bool { self.f(G) }
    pub fn rom_owned(&self) -> bool { self.f(G) && self.ron }
    pub fn ram_owned(&self) -> bool { self.f(G) && self.ran }
    pub fn bram_writable(&self) -> bool { self.bramr }

    // ---------------- memory ----------------

    fn rom_at(m: &Mem, i: usize) -> u8 { if m.rom.is_empty() { 0 } else { m.rom[i % m.rom.len()] } }

    fn read(&self, m: &Mem, addr: u32) -> u8 {
        if addr & 0xC00000 == 0 {
            return Self::rom_at(m, (((addr & 0x3F0000) >> 1) | (addr & 0x7FFF)) as usize);
        }
        if addr & 0xE00000 == 0x400000 { return Self::rom_at(m, (addr & 0x3FFFFF) as usize); }
        if addr & 0xFE0000 == 0x700000 && !m.ram.is_empty() {
            return m.ram[(addr & 0x1FFFF) as usize % m.ram.len()];
        }
        0
    }

    fn write(&self, m: &mut Mem, addr: u32, v: u8) {
        if addr & 0xFE0000 == 0x700000 && !m.ram.is_empty() {
            let n = m.ram.len();
            m.ram[(addr & 0x1FFFF) as usize % n] = v;
        }
    }

    fn fast(&self) -> u32 { if self.clsr { 5 } else { 6 } }

    fn step(&mut self, m: &mut Mem, clocks: u32) {
        if self.romcl > 0 {
            self.romcl -= clocks.min(self.romcl);
            if self.romcl == 0 {
                self.setf(R, false);
                self.romdr = self.read(m, (self.rombr as u32) << 16 | self.r[14] as u32);
            }
        }
        if self.ramcl > 0 {
            self.ramcl -= clocks.min(self.ramcl);
            if self.ramcl == 0 {
                let a = 0x700000 + ((self.rambr as u32) << 16) + self.ramar as u32;
                self.write(m, a, self.ramdr);
            }
        }
        self.used += clocks;
    }

    fn sync_rom(&mut self, m: &mut Mem) { if self.romcl > 0 { let c = self.romcl; self.step(m, c); } }
    fn read_rom_buffer(&mut self, m: &mut Mem) -> u8 { self.sync_rom(m); self.romdr }
    fn update_rom_buffer(&mut self) { self.setf(R, true); self.romcl = self.fast(); }
    fn sync_ram(&mut self, m: &mut Mem) { if self.ramcl > 0 { let c = self.ramcl; self.step(m, c); } }
    fn read_ram_buffer(&mut self, m: &mut Mem, a: u16) -> u8 {
        self.sync_ram(m);
        self.read(m, 0x700000 + ((self.rambr as u32) << 16) + a as u32)
    }
    fn write_ram_buffer(&mut self, m: &mut Mem, a: u16, v: u8) {
        self.sync_ram(m);
        self.ramcl = self.fast();
        self.ramar = a;
        self.ramdr = v;
    }

    fn read_opcode(&mut self, m: &mut Mem, addr: u16) -> u8 {
        let offset = addr.wrapping_sub(self.cbr);
        if offset < 512 {
            let line = (offset >> 4) as usize;
            if !self.cache_valid[line] {
                let mut dp = (offset & 0xFFF0) as usize;
                let mut sp = ((self.pbr as u32) << 16) + (self.cbr.wrapping_add(offset & 0xFFF0) & 0xFFF0) as u32;
                for _ in 0..16 {
                    let f = self.fast();
                    self.step(m, f);
                    self.cache[dp] = self.read(m, sp);
                    dp += 1;
                    sp += 1;
                }
                self.cache_valid[line] = true;
            } else {
                let c = if self.clsr { 1 } else { 2 };
                self.step(m, c);
            }
            return self.cache[offset as usize];
        }
        if self.pbr <= 0x5F { self.sync_rom(m); } else { self.sync_ram(m); }
        let f = self.fast();
        self.step(m, f);
        self.read(m, (self.pbr as u32) << 16 | addr as u32)
    }

    fn peekpipe(&mut self, m: &mut Mem) -> u8 {
        let result = self.pipeline;
        self.pipeline = self.read_opcode(m, self.r[15]);
        self.r15_mod = false;
        result
    }

    fn pipe(&mut self, m: &mut Mem) -> u8 {
        let result = self.pipeline;
        self.r[15] = self.r[15].wrapping_add(1);
        self.pipeline = self.read_opcode(m, self.r[15]);
        self.r15_mod = false;
        result
    }

    fn flush_cache(&mut self) { self.cache_valid = [false; 32]; }

    // ---------------- plotting ----------------

    fn color(&self, source: u8) -> u8 {
        if self.por_highnibble { return (self.colr & 0xF0) | (source >> 4); }
        if self.por_freezehigh { return (self.colr & 0xF0) | (source & 0x0F); }
        source
    }

    fn char_addr(&self, x: u8, y: u8) -> (u32, u32) {
        let (x, y) = (x as u32, y as u32);
        let cn = match if self.por_obj { 3 } else { self.ht } {
            0 => ((x & 0xF8) << 1) + ((y & 0xF8) >> 3),
            1 => ((x & 0xF8) << 1) + ((x & 0xF8) >> 1) + ((y & 0xF8) >> 3),
            2 => ((x & 0xF8) << 1) + (x & 0xF8) + ((y & 0xF8) >> 3),
            _ => ((y & 0x80) << 2) + ((x & 0x80) << 1) + ((y & 0x78) << 1) + ((x & 0x78) >> 3),
        };
        let bpp = [2u32, 4, 4, 8][self.md as usize];
        (0x700000 + cn * (bpp << 3) + ((self.scbr as u32) << 10) + (y & 7) * 2, bpp)
    }

    fn plot(&mut self, m: &mut Mem, x: u8, y: u8) {
        if !self.por_transparent {
            if self.md == 3 {
                if self.por_freezehigh { if self.colr & 0x0F == 0 { return; } } else if self.colr == 0 { return; }
            } else if self.colr & 0x0F == 0 {
                return;
            }
        }
        let mut color = self.colr;
        if self.por_dither && self.md != 3 {
            if (x ^ y) & 1 != 0 { color >>= 4; }
            color &= 0x0F;
        }
        let offset = ((y as u16) << 5) + (x as u16 >> 3);
        if offset != self.pixelcache[0].offset {
            let c1 = self.pixelcache[1];
            self.flush_pixel_cache(m, c1);
            self.pixelcache[1] = self.pixelcache[0];
            self.pixelcache[0].bitpend = 0;
            self.pixelcache[0].offset = offset;
        }
        let xb = ((x & 7) ^ 7) as usize;
        self.pixelcache[0].data[xb] = color;
        self.pixelcache[0].bitpend |= 1 << xb;
        if self.pixelcache[0].bitpend == 0xFF {
            let c1 = self.pixelcache[1];
            self.flush_pixel_cache(m, c1);
            self.pixelcache[1] = self.pixelcache[0];
            self.pixelcache[0].bitpend = 0;
        }
    }

    fn rpix(&mut self, m: &mut Mem, x: u8, y: u8) -> u8 {
        let c1 = self.pixelcache[1];
        self.flush_pixel_cache(m, c1);
        self.pixelcache[1].bitpend = 0;
        let c0 = self.pixelcache[0];
        self.flush_pixel_cache(m, c0);
        self.pixelcache[0].bitpend = 0;
        let (addr, bpp) = self.char_addr(x, y);
        let xb = (x & 7) ^ 7;
        let mut data = 0u8;
        for n in 0..bpp {
            let byte = ((n >> 1) << 4) + (n & 1);
            let f = self.fast();
            self.step(m, f);
            data |= ((self.read(m, addr + byte) >> xb) & 1) << n;
        }
        data
    }

    /// Write a pixel cache's row out to RAM (the caller clears its pending bits).
    fn flush_pixel_cache(&mut self, m: &mut Mem, cache: PixelCache) {
        if cache.bitpend == 0 { return; }
        let x = (cache.offset << 3) as u8;
        let y = (cache.offset >> 5) as u8;
        let (addr, bpp) = self.char_addr(x, y);
        for n in 0..bpp {
            let byte = ((n >> 1) << 4) + (n & 1);
            let mut data = 0u8;
            for xx in 0..8 { data |= ((cache.data[xx] >> n) & 1) << xx; }
            if cache.bitpend != 0xFF {
                let f = self.fast();
                self.step(m, f);
                data &= cache.bitpend;
                data |= self.read(m, addr + byte) & !cache.bitpend;
            }
            let f = self.fast();
            self.step(m, f);
            self.write(m, addr + byte, data);
        }
    }

    // ---------------- running ----------------

    /// Master clocks pass: run the GSU up to the same point in time.
    pub fn run(&mut self, clocks: u32, m: &mut Mem) {
        self.debt += clocks as i64;
        while self.debt > 0 {
            self.used = 0;
            if !self.f(G) {
                if self.romcl == 0 && self.ramcl == 0 { self.debt = 0; return; }
                self.step(m, 6);
            } else {
                let op = self.peekpipe(m);
                self.instruction(op, m);
                if self.r14_mod { self.r14_mod = false; self.update_rom_buffer(); }
                if self.r15_mod { self.r15_mod = false; } else { self.r[15] = self.r[15].wrapping_add(1); }
            }
            self.debt -= self.used as i64;
        }
    }

    fn instruction(&mut self, op: u8, m: &mut Mem) {
        let n = (op & 15) as usize;
        match op {
            0x00 => {
                // STOP
                if !self.cfgr_irq { self.setf(IRQ, true); self.irq = true; }
                self.setf(G, false);
                self.pipeline = 0x01;
                self.reset_prefix();
            }
            0x01 => self.reset_prefix(),
            0x02 => {
                if self.cbr != self.r[15] & 0xFFF0 { self.cbr = self.r[15] & 0xFFF0; self.flush_cache(); }
                self.reset_prefix();
            }
            0x03 => {
                let s = self.sr();
                self.setf(CY, s & 1 != 0);
                let v = s >> 1;
                self.set_dr(v);
                self.sz(v);
                self.reset_prefix();
            }
            0x04 => {
                let s = self.sr();
                let v = s << 1 | self.f(CY) as u16;
                self.set_dr(v);
                self.setf(CY, s & 0x8000 != 0);
                self.sz(v);
                self.reset_prefix();
            }
            0x05..=0x0F => {
                let (s, ov, z, cy) = (self.f(S), self.f(OV), self.f(Z), self.f(CY));
                let take = match op {
                    0x05 => true, 0x06 => s == ov, 0x07 => s != ov, 0x08 => !z, 0x09 => z,
                    0x0A => !s, 0x0B => s, 0x0C => !cy, 0x0D => cy, 0x0E => !ov, _ => ov,
                };
                let d = self.pipe(m) as i8;
                if take { let v = self.r[15].wrapping_add(d as u16); self.set_r(15, v); }
            }
            0x10..=0x1F => {
                if !self.f(B) { self.dreg = n; } else { let v = self.sr(); self.set_r(n, v); self.reset_prefix(); }
            }
            0x20..=0x2F => { self.sreg = n; self.dreg = n; self.setf(B, true); }
            0x30..=0x3B => {
                self.ramaddr = self.r[n];
                let (a, s) = (self.ramaddr, self.sr());
                self.write_ram_buffer(m, a, s as u8);
                if !self.alt1() { self.write_ram_buffer(m, a ^ 1, (s >> 8) as u8); }
                self.reset_prefix();
            }
            0x3C => {
                let v = self.r[12].wrapping_sub(1);
                self.set_r(12, v);
                self.sz(v);
                if v != 0 { let t = self.r[13]; self.set_r(15, t); }
                self.reset_prefix();
            }
            0x3D => { self.setf(B, false); self.setf(ALT1, true); }
            0x3E => { self.setf(B, false); self.setf(ALT2, true); }
            0x3F => { self.setf(B, false); self.setf(ALT1, true); self.setf(ALT2, true); }
            0x40..=0x4B => {
                self.ramaddr = self.r[n];
                let a = self.ramaddr;
                let mut v = self.read_ram_buffer(m, a) as u16;
                if !self.alt1() { v |= (self.read_ram_buffer(m, a ^ 1) as u16) << 8; }
                self.set_dr(v);
                self.reset_prefix();
            }
            0x4C => {
                if !self.alt1() {
                    let (x, y) = (self.r[1] as u8, self.r[2] as u8);
                    self.plot(m, x, y);
                    let v = self.r[1].wrapping_add(1);
                    self.set_r(1, v);
                } else {
                    let (x, y) = (self.r[1] as u8, self.r[2] as u8);
                    let v = self.rpix(m, x, y) as u16;
                    self.set_dr(v);
                    self.sz(v);
                }
                self.reset_prefix();
            }
            0x4D => { let s = self.sr(); let v = s.rotate_left(8); self.set_dr(v); self.sz(v); self.reset_prefix(); }
            0x4E => {
                let s = self.sr();
                if !self.alt1() {
                    self.colr = self.color(s as u8);
                } else {
                    let d = s as u8;
                    self.por_obj = d & 0x10 != 0;
                    self.por_freezehigh = d & 0x08 != 0;
                    self.por_highnibble = d & 0x04 != 0;
                    self.por_dither = d & 0x02 != 0;
                    self.por_transparent = d & 0x01 != 0;
                }
                self.reset_prefix();
            }
            0x4F => { let v = !self.sr(); self.set_dr(v); self.sz(v); self.reset_prefix(); }
            0x50..=0x5F => {
                let k = if self.alt2() { n as u32 } else { self.r[n] as u32 };
                let s = self.sr() as u32;
                let r = s + k + if self.alt1() { self.f(CY) as u32 } else { 0 };
                self.setf(OV, !(s ^ k) & (k ^ r) & 0x8000 != 0);
                self.setf(S, r & 0x8000 != 0);
                self.setf(CY, r >= 0x10000);
                self.setf(Z, r as u16 == 0);
                self.set_dr(r as u16);
                self.reset_prefix();
            }
            0x60..=0x6F => {
                let k = if !self.alt2() || self.alt1() { self.r[n] as i32 } else { n as i32 };
                let s = self.sr() as i32;
                let r = s - k - if !self.alt2() && self.alt1() { !self.f(CY) as i32 } else { 0 };
                self.setf(OV, (s ^ k) & (s ^ r) & 0x8000 != 0);
                self.setf(S, r & 0x8000 != 0);
                self.setf(CY, r >= 0);
                self.setf(Z, r as u16 == 0);
                if !self.alt2() || !self.alt1() { self.set_dr(r as u16); }
                self.reset_prefix();
            }
            0x70 => {
                let v = (self.r[7] & 0xFF00) | (self.r[8] >> 8);
                self.set_dr(v);
                self.setf(OV, v & 0xC0C0 != 0);
                self.setf(S, v & 0x8080 != 0);
                self.setf(CY, v & 0xE0E0 != 0);
                self.setf(Z, v & 0xF0F0 != 0);
                self.reset_prefix();
            }
            0x71..=0x7F => {
                let k = if self.alt2() { n as u16 } else { self.r[n] };
                let v = self.sr() & if self.alt1() { !k } else { k };
                self.set_dr(v);
                self.sz(v);
                self.reset_prefix();
            }
            0x80..=0x8F => {
                let k = if self.alt2() { n as u16 } else { self.r[n] };
                let s = self.sr();
                let v = if !self.alt1() {
                    ((s as u8 as i8 as i32) * (k as u8 as i8 as i32)) as u16
                } else {
                    (s as u8 as u16) * (k as u8 as u16)
                };
                self.set_dr(v);
                self.sz(v);
                self.reset_prefix();
                if !self.cfgr_ms0 { let c = if self.clsr { 1 } else { 2 }; self.step(m, c); }
            }
            0x90 => {
                let (a, s) = (self.ramaddr, self.sr());
                self.write_ram_buffer(m, a, s as u8);
                self.write_ram_buffer(m, a ^ 1, (s >> 8) as u8);
                self.reset_prefix();
            }
            0x91..=0x94 => { let v = self.r[15].wrapping_add(n as u16); self.set_r(11, v); self.reset_prefix(); }
            0x95 => { let v = self.sr() as u8 as i8 as i16 as u16; self.set_dr(v); self.sz(v); self.reset_prefix(); }
            0x96 => {
                let s = self.sr();
                self.setf(CY, s & 1 != 0);
                let mut v = ((s as i16) >> 1) as u16;
                if self.alt1() { v = v.wrapping_add(((s as u32 + 1) >> 16) as u16); }
                self.set_dr(v);
                self.sz(v);
                self.reset_prefix();
            }
            0x97 => {
                let s = self.sr();
                let v = (self.f(CY) as u16) << 15 | s >> 1;
                self.set_dr(v);
                self.setf(CY, s & 1 != 0);
                self.sz(v);
                self.reset_prefix();
            }
            0x98..=0x9D => {
                let k = (op - 0x90) as usize;
                if !self.alt1() {
                    let v = self.r[k];
                    self.set_r(15, v);
                } else {
                    self.pbr = (self.r[k] & 0x7F) as u8;
                    let v = self.sr();
                    self.set_r(15, v);
                    self.cbr = self.r[15] & 0xFFF0;
                    self.flush_cache();
                }
                self.reset_prefix();
            }
            0x9E => {
                let v = self.sr() & 0xFF;
                self.set_dr(v);
                self.setf(S, v & 0x80 != 0);
                self.setf(Z, v == 0);
                self.reset_prefix();
            }
            0x9F => {
                let result = (self.sr() as i16 as i32 * self.r[6] as i16 as i32) as u32;
                if self.alt1() { self.set_r(4, result as u16); }
                let v = (result >> 16) as u16;
                self.set_dr(v);
                self.setf(S, v & 0x8000 != 0);
                self.setf(CY, result & 0x8000 != 0);
                self.setf(Z, v == 0);
                self.reset_prefix();
                let c = (if self.cfgr_ms0 { 3 } else { 7 }) * if self.clsr { 1 } else { 2 };
                self.step(m, c);
            }
            0xA0..=0xAF => {
                if self.alt1() {
                    self.ramaddr = (self.pipe(m) as u16) << 1;
                    let a = self.ramaddr;
                    let lo = self.read_ram_buffer(m, a) as u16;
                    let v = (self.read_ram_buffer(m, a ^ 1) as u16) << 8 | lo;
                    self.set_r(n, v);
                } else if self.alt2() {
                    self.ramaddr = (self.pipe(m) as u16) << 1;
                    let (a, v) = (self.ramaddr, self.r[n]);
                    self.write_ram_buffer(m, a, v as u8);
                    self.write_ram_buffer(m, a ^ 1, (v >> 8) as u8);
                } else {
                    let v = self.pipe(m) as i8 as i16 as u16;
                    self.set_r(n, v);
                }
                self.reset_prefix();
            }
            0xB0..=0xBF => {
                if !self.f(B) {
                    self.sreg = n;
                } else {
                    let v = self.r[n];
                    self.set_dr(v);
                    self.setf(OV, v & 0x80 != 0);
                    self.sz(v);
                    self.reset_prefix();
                }
            }
            0xC0 => {
                let v = self.sr() >> 8;
                self.set_dr(v);
                self.setf(S, v & 0x80 != 0);
                self.setf(Z, v == 0);
                self.reset_prefix();
            }
            0xC1..=0xCF => {
                let k = if self.alt2() { n as u16 } else { self.r[n] };
                let v = if !self.alt1() { self.sr() | k } else { self.sr() ^ k };
                self.set_dr(v);
                self.sz(v);
                self.reset_prefix();
            }
            0xD0..=0xDE => { let v = self.r[n].wrapping_add(1); self.set_r(n, v); self.sz(v); self.reset_prefix(); }
            0xDF => {
                if !self.alt2() {
                    let b = self.read_rom_buffer(m);
                    self.colr = self.color(b);
                } else if !self.alt1() {
                    self.sync_ram(m);
                    self.rambr = (self.sr() & 1) as u8;
                } else {
                    self.sync_rom(m);
                    self.rombr = (self.sr() & 0x7F) as u8;
                }
                self.reset_prefix();
            }
            0xE0..=0xEE => { let v = self.r[n].wrapping_sub(1); self.set_r(n, v); self.sz(v); self.reset_prefix(); }
            0xEF => {
                let b = self.read_rom_buffer(m) as u16;
                let s = self.sr();
                let v = match (self.alt2() as u8) << 1 | self.alt1() as u8 {
                    0 => b,
                    1 => b << 8 | (s & 0xFF),
                    2 => (s & 0xFF00) | b,
                    _ => b as u8 as i8 as i16 as u16,
                };
                self.set_dr(v);
                self.reset_prefix();
            }
            0xF0..=0xFF => {
                if self.alt1() {
                    let lo = self.pipe(m) as u16;
                    self.ramaddr = lo | (self.pipe(m) as u16) << 8;
                    let a = self.ramaddr;
                    let l = self.read_ram_buffer(m, a) as u16;
                    let v = (self.read_ram_buffer(m, a ^ 1) as u16) << 8 | l;
                    self.set_r(n, v);
                } else if self.alt2() {
                    let lo = self.pipe(m) as u16;
                    self.ramaddr = lo | (self.pipe(m) as u16) << 8;
                    let (a, v) = (self.ramaddr, self.r[n]);
                    self.write_ram_buffer(m, a, v as u8);
                    self.write_ram_buffer(m, a ^ 1, (v >> 8) as u8);
                } else {
                    let lo = self.pipe(m) as u16;
                    let v = (self.pipe(m) as u16) << 8 | lo;
                    self.set_r(n, v);
                }
                self.reset_prefix();
            }
        }
    }

    // ---------------- the CPU side (3000-34FF) ----------------

    pub fn read_io(&mut self, addr: u16) -> u8 {
        let a = 0x3000 | (addr & 0x3FF);
        if (0x3100..=0x32FF).contains(&a) { return self.cache[((a - 0x3100).wrapping_add(self.cbr) & 511) as usize]; }
        if (0x3000..=0x301F).contains(&a) { return (self.r[(a >> 1 & 15) as usize] >> ((a & 1) << 3)) as u8; }
        match a {
            0x3030 => (self.sfr & 0x9F7E) as u8,
            0x3031 => {
                let r = ((self.sfr & 0x9F7E) >> 8) as u8;
                self.setf(IRQ, false);
                self.irq = false;
                r
            }
            0x3034 => self.pbr,
            0x3036 => self.rombr,
            0x303B => self.vcr,
            0x303C => self.rambr,
            0x303E => self.cbr as u8,
            0x303F => (self.cbr >> 8) as u8,
            _ => 0,
        }
    }

    pub fn write_io(&mut self, addr: u16, v: u8) {
        let a = 0x3000 | (addr & 0x3FF);
        if (0x3100..=0x32FF).contains(&a) {
            let i = ((a - 0x3100).wrapping_add(self.cbr) & 511) as usize;
            self.cache[i] = v;
            if i & 15 == 15 { self.cache_valid[i >> 4] = true; }
            return;
        }
        if (0x3000..=0x301F).contains(&a) {
            let n = (a >> 1 & 15) as usize;
            self.r[n] = if a & 1 == 0 { (self.r[n] & 0xFF00) | v as u16 } else { (v as u16) << 8 | (self.r[n] & 0xFF) };
            if n == 14 { self.update_rom_buffer(); }
            if a == 0x301F { self.setf(G, true); }
            return;
        }
        match a {
            0x3030 => {
                let g = self.f(G);
                self.sfr = (self.sfr & 0xFF00) | v as u16;
                if g && !self.f(G) { self.cbr = 0; self.flush_cache(); }
            }
            0x3031 => self.sfr = (v as u16) << 8 | (self.sfr & 0xFF),
            0x3033 => self.bramr = v & 1 != 0,
            0x3034 => { self.pbr = v & 0x7F; self.flush_cache(); }
            0x3037 => { self.cfgr_irq = v & 0x80 != 0; self.cfgr_ms0 = v & 0x20 != 0; }
            0x3038 => self.scbr = v,
            0x3039 => self.clsr = v & 1 != 0,
            0x303A => {
                self.ht = (v & 0x20 != 0) as u8 * 2 + (v & 0x04 != 0) as u8;
                self.ron = v & 0x10 != 0;
                self.ran = v & 0x08 != 0;
                self.md = v & 3;
            }
            _ => {}
        }
    }
}

/// What the CPU reads from ROM while the GSU owns it: fixed interrupt vectors.
pub fn rom_vector(addr: u32) -> u8 {
    const V: [u8; 16] = [0x00, 0x01, 0x00, 0x01, 0x04, 0x01, 0x00, 0x01, 0x00, 0x01, 0x08, 0x01, 0x00, 0x01, 0x0C, 0x01];
    V[(addr & 15) as usize]
}
