//! Cx4: Capcom's name for the Hitachi HG51BS169 DSP (Mega Man X2 / X3, "HITACHI-LOROM"
//! boards). Ported from ares (component/processor/hg51b, sfc/coprocessor/hitachidsp),
//! ISC license; see THIRD_PARTY_NOTICES.md.
//!
//! The chip runs at 20 MHz with 24-bit registers. Its program is not inside the chip: it
//! copies 512 bytes of cartridge ROM at a time into one of two 256-word instruction
//! caches. Inside the chip are 3 KB of data RAM (the S-CPU sees it at 6000-6BFF and
//! 7000-7BFF) and a 1024-word data ROM of maths tables. That data ROM is the only
//! firmware: `cx4.rom` (3 KB, three bytes per word, low byte first), supplied by the
//! user.
//!
//! The S-CPU drives it through 7F40-7FFF (mirrored at 6F40-6FFF):
//!   7F40-47 DMA source / length / target (writing 7F47 starts it)
//!   7F48    cache page (writing it preloads the page)    7F49-4B cache base address
//!   7F4C    cache page locks   7F4D-4E program bank      7F4F program counter (starts it)
//!   7F50    wait states        7F51 IRQ disable / ack    7F52 ROM count
//!   7F53    stop               7F55-5D suspend / resume  7F5E clear the IRQ flag
//!   7F5x    status (reads)     7F60-7F7F vectors         7F80-7FAF registers R0-R15
//! While the chip uses the bus (cache loads, DMA), the S-CPU sees the vectors at
//! 00:FFC0-FFFF come from 7F40-7F7F and the rest of the ROM as open bus.

use serde::{Deserialize, Serialize};

const MASTER_HZ: i64 = 21_477_272;
const CX4_HZ: i64 = 20_000_000;
const M24: u32 = 0xFF_FFFF;

/// The cartridge memory the chip reaches.
pub struct Mem<'a> {
    pub rom: &'a [u8],
    pub ram: &'a mut [u8],
}

/// ares Bus::mirror: an offset into memory whose size need not be a power of two.
pub fn mirror(mut addr: usize, mut size: usize) -> usize {
    if size == 0 { return 0; }
    let mut base = 0;
    let mut mask = 1usize << 23;
    while addr >= size {
        while addr & mask == 0 { mask >>= 1; }
        addr -= mask;
        if size > mask { size -= mask; base += mask; }
        mask >>= 1;
    }
    base + addr
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Regs {
    pb: u16,  // program bank (15 bits)
    pc: u8,
    n: bool, z: bool, c: bool, v: bool,
    i: bool,  // IRQ flag
    a: u32,
    p: u16,   // page register (15 bits)
    mul: u64, // 48 bits
    mdr: u32,
    rom: u32,
    ram: u32,
    mar: u32,
    dpr: u32,
    gpr: [u32; 16],
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Cache { enable: bool, page: u8, preload: bool, lock: [bool; 2], address: [u32; 2], base: u32, pb: u16, pc: u8 }

#[derive(Clone, Default, Serialize, Deserialize)]
struct Dma { enable: bool, source: u32, target: u32, length: u16 }

#[derive(Clone, Default, Serialize, Deserialize)]
struct BusOp { enable: bool, reading: bool, writing: bool, pending: u32, address: u32 }

#[derive(Clone, Serialize, Deserialize)]
struct Io {
    lock: bool,
    halt: bool,
    irq: bool, // true = IRQ disabled
    rom: bool,
    vector: [u8; 32],
    wait_rom: u32,
    wait_ram: u32,
    suspend: bool,
    suspend_for: u32,
    cache: Cache,
    dma: Dma,
    bus: BusOp,
}

impl Default for Io {
    fn default() -> Self {
        Self { lock: false, halt: true, irq: false, rom: true, vector: [0; 32], wait_rom: 3, wait_ram: 3,
               suspend: false, suspend_for: 0, cache: Cache::default(), dma: Dma::default(), bus: BusOp::default() }
    }
}

/// What the S-CPU sees at a cartridge address.
pub enum CpuRead { Value(u8), OpenBus, Cart }

#[derive(Clone, Serialize, Deserialize)]
pub struct Cx4 {
    r: Regs,
    io: Io,
    /// two 256-word instruction caches
    program_ram: Vec<u16>,
    #[serde(skip)]
    data_rom: Vec<u32>,
    #[serde(with = "serde_bytes")]
    data_ram: Vec<u8>,
    stack: [u32; 8],
    /// the IRQ line to the S-CPU
    pub irq_line: bool,
    /// master clocks owed to the chip, scaled (master * CX4_HZ - cx4 * MASTER_HZ)
    debt: i64,
}

impl Cx4 {
    /// `firmware`: the data ROM, 1024 words of three bytes, low byte first.
    pub fn new(firmware: &[u8]) -> Self {
        let data_rom = (0..1024).map(|i| {
            let b = |k: usize| *firmware.get(i * 3 + k).unwrap_or(&0) as u32;
            b(0) | b(1) << 8 | b(2) << 16
        }).collect();
        Self { r: Regs::default(), io: Io::default(), program_ram: vec![0; 512], data_rom, data_ram: vec![0; 3072],
               stack: [0; 8], irq_line: false, debt: 0 }
    }

    pub fn keep_firmware(&mut self, old: &Cx4) { self.data_rom = old.data_rom.clone(); }

    /// One line for the debug report.
    pub fn status(&self) -> String {
        let (r, io) = (&self.r, &self.io);
        format!("PB {:04X} PC {:02X} P {:04X} A {:06X} halt {} lock {} suspend {} irq-off {} line {}  cache {:06X}/{:06X} page {} base {:06X}  R0-3 {:06X} {:06X} {:06X} {:06X}",
                r.pb, r.pc, r.p, r.a, io.halt as u8, io.lock as u8, io.suspend as u8, io.irq as u8, self.irq_line as u8,
                io.cache.address[0], io.cache.address[1], io.cache.page, io.cache.base, r.gpr[0], r.gpr[1], r.gpr[2], r.gpr[3])
    }

    pub fn running(&self) -> bool { self.io.cache.enable || self.io.dma.enable || self.io.bus.pending != 0 || !self.io.halt }
    pub fn busy(&self) -> bool { self.io.cache.enable || self.io.dma.enable || self.io.bus.pending != 0 }

    /// Master clocks pass: run the chip up to the same point in time.
    pub fn run(&mut self, clocks: u32, m: &mut Mem) {
        self.debt += clocks as i64 * CX4_HZ;
        while self.debt > 0 {
            // nothing to do until the S-CPU says so: skip ahead
            let idle = self.io.lock || (self.io.suspend && self.io.suspend_for == 0)
                || (!self.io.suspend && !self.io.cache.enable && !self.io.dma.enable && self.io.halt);
            if idle && !self.io.bus.enable { self.debt = 0; break; }
            self.main(m);
        }
    }

    fn main(&mut self, m: &mut Mem) {
        if self.io.lock { return self.step(1, m); }
        if self.io.suspend { return self.suspend(m); }
        if self.io.cache.enable { self.cache(m); return; }
        if self.io.dma.enable { return self.dma(m); }
        if self.io.halt { return self.step(1, m); }
        self.execute(m);
    }

    fn step(&mut self, clocks: u32, m: &mut Mem) {
        self.debt -= clocks as i64 * MASTER_HZ;
        if self.io.bus.enable {
            if self.io.bus.pending > clocks {
                self.io.bus.pending -= clocks;
            } else {
                self.io.bus.enable = false;
                self.io.bus.pending = 0;
                if self.io.bus.reading { self.io.bus.reading = false; self.r.mdr = self.read(self.io.bus.address, m) as u32; }
                if self.io.bus.writing { self.io.bus.writing = false; self.write(self.io.bus.address, self.r.mdr as u8, m); }
            }
        }
    }

    fn halt(&mut self) {
        self.io.halt = true;
        if !self.io.irq { self.r.i = true; self.irq_line = true; }
    }

    fn suspend(&mut self, m: &mut Mem) {
        if self.io.suspend_for == 0 { return self.step(1, m); }
        self.step(self.io.suspend_for, m);
        self.io.suspend_for = 0;
        self.io.suspend = false;
    }

    fn execute(&mut self, m: &mut Mem) {
        if !self.cache(m) { return self.halt(); }
        let op = self.program_ram[self.io.cache.page as usize * 256 + self.r.pc as usize];
        self.advance(m);
        self.step(1, m);
        self.instruction(op, m);
    }

    fn advance(&mut self, m: &mut Mem) {
        self.r.pc = self.r.pc.wrapping_add(1);
        if self.r.pc == 0 {
            if self.io.cache.page == 1 { return self.halt(); }
            self.io.cache.page = 1;
            if self.io.cache.lock[1] { return self.halt(); }
            self.r.pb = self.r.p;
            if !self.cache(m) { self.halt(); }
        }
    }

    /// Make sure the current program bank is in a cache page, loading it if needed.
    fn cache(&mut self, m: &mut Mem) -> bool {
        let c = &mut self.io.cache;
        let mut address = c.base.wrapping_add(self.r.pb as u32 * 512) & M24;
        if !c.preload {
            let mut page = c.page as usize;
            if c.address[page] == address { c.enable = false; return true; }
            page ^= 1;
            c.page = page as u8;
            if c.address[page] == address { c.enable = false; return true; }
            if c.lock[page] { page ^= 1; c.page = page as u8; }
            if c.lock[page] { c.enable = false; return false; }
        }
        c.preload = false;
        let page = c.page as usize;
        c.address[page] = address;
        for offset in 0..256 {
            let w = self.wait(address); self.step(w, m);
            let lo = self.read(address, m) as u16;
            address = (address + 1) & M24;
            let w = self.wait(address); self.step(w, m);
            let hi = self.read(address, m) as u16;
            address = (address + 1) & M24;
            self.program_ram[page * 256 + offset] = lo | hi << 8;
        }
        self.io.cache.enable = false;
        true
    }

    fn dma(&mut self, m: &mut Mem) {
        for offset in 0..self.io.dma.length as u32 {
            let source = self.io.dma.source.wrapping_add(offset) & M24;
            let target = self.io.dma.target.wrapping_add(offset) & M24;
            if (is_rom(source) && is_rom(target)) || (is_ram(source) && is_ram(target)) { self.io.lock = true; return; }
            let w = self.wait(source); self.step(w, m);
            let data = self.read(source, m);
            let w = self.wait(target); self.step(w, m);
            self.write(target, data, m);
        }
        self.io.dma.enable = false;
    }

    fn wait(&self, a: u32) -> u32 {
        if is_rom(a) { return 1 + self.io.wait_rom; }
        if is_ram(a) { return 1 + self.io.wait_ram; }
        0
    }

    // ---------------- the chip's own bus ----------------

    fn read(&mut self, a: u32, m: &mut Mem) -> u8 {
        if let Some(l) = rom_addr(a) { return if m.rom.is_empty() { 0 } else { m.rom[mirror(l, m.rom.len())] }; }
        if let Some(l) = ram_addr(a) { return if m.ram.is_empty() { 0 } else { m.ram[mirror(l, m.ram.len())] }; }
        if dram_hit(a) { return self.data_ram[(a & 0xFFF) as usize]; }
        if io_hit(a) { return self.read_io(a); }
        0
    }

    fn write(&mut self, a: u32, v: u8, m: &mut Mem) {
        if rom_addr(a).is_some() { return; }
        if let Some(l) = ram_addr(a) { if !m.ram.is_empty() { let n = m.ram.len(); m.ram[mirror(l, n)] = v; } return; }
        if dram_hit(a) { self.data_ram[(a & 0xFFF) as usize] = v; return; }
        if io_hit(a) { self.write_io(a, v); }
    }

    // ---------------- the S-CPU side ----------------

    pub fn cpu_read(&mut self, a: u32) -> CpuRead {
        if io_hit(a) { return CpuRead::Value(self.read_io(a)); }
        if dram_hit(a) {
            let o = (a & 0xFFF) as usize;
            return if o < 0xC00 { CpuRead::Value(self.data_ram[o]) } else { CpuRead::OpenBus };
        }
        if a & 0x408000 == 0x008000 && self.busy() {
            // the chip has the ROM: the vectors come from its registers
            if a & 0x7FFFC0 == 0x00FFC0 { return CpuRead::Value(self.read_io(0x7F40 | (a & 0x3F))); }
            return CpuRead::OpenBus;
        }
        CpuRead::Cart
    }

    /// True when the write was the chip's.
    pub fn cpu_write(&mut self, a: u32, v: u8) -> bool {
        if io_hit(a) { self.write_io(a, v); return true; }
        if dram_hit(a) { let o = (a & 0xFFF) as usize; if o < 0xC00 { self.data_ram[o] = v; } return true; }
        false
    }

    fn read_io(&mut self, a: u32) -> u8 {
        let a = 0x7C00 | (a & 0x3FF);
        let io = &self.io;
        let byte = |x: u32, n: u32| (x >> (n * 8)) as u8;
        match a {
            0x7F40..=0x7F42 => return byte(io.dma.source, a - 0x7F40),
            0x7F43..=0x7F44 => return byte(io.dma.length as u32, a - 0x7F43),
            0x7F45..=0x7F47 => return byte(io.dma.target, a - 0x7F45),
            0x7F48 => return io.cache.page,
            0x7F49..=0x7F4B => return byte(io.cache.base, a - 0x7F49),
            0x7F4C => return io.cache.lock[0] as u8 | (io.cache.lock[1] as u8) << 1,
            0x7F4D..=0x7F4E => return byte(io.cache.pb as u32, a - 0x7F4D),
            0x7F4F => return io.cache.pc,
            0x7F50 => return (io.wait_ram | io.wait_rom << 4) as u8,
            0x7F51 => return io.irq as u8,
            0x7F52 => return io.rom as u8,
            0x7F53..=0x7F57 | 0x7F59 | 0x7F5B..=0x7F5F =>
                return io.suspend as u8 | (self.r.i as u8) << 1 | (self.running() as u8) << 6 | (self.busy() as u8) << 7,
            0x7F60..=0x7F7F => return io.vector[(a & 0x1F) as usize],
            0x7F80..=0x7FAF | 0x7FC0..=0x7FEF => { let o = a & 0x3F; return byte(self.r.gpr[(o / 3) as usize], o % 3); }
            _ => 0,
        }
    }

    fn write_io(&mut self, a: u32, v: u8) {
        let a = 0x7C00 | (a & 0x3FF);
        let set = |x: &mut u32, n: u32| { *x = (*x & !(0xFF << (n * 8))) | (v as u32) << (n * 8); };
        let io = &mut self.io;
        match a {
            0x7F40..=0x7F42 => set(&mut io.dma.source, a - 0x7F40),
            0x7F43..=0x7F44 => { let mut l = io.dma.length as u32; set(&mut l, a - 0x7F43); io.dma.length = l as u16; }
            0x7F45..=0x7F47 => {
                set(&mut io.dma.target, a - 0x7F45);
                if a == 0x7F47 && io.halt { io.dma.enable = true; }
            }
            0x7F48 => {
                io.cache.page = v & 1;
                if io.halt { self.r.pb = io.cache.pb; io.cache.preload = true; io.cache.enable = true; }
            }
            0x7F49..=0x7F4B => set(&mut io.cache.base, a - 0x7F49),
            0x7F4C => { io.cache.lock = [v & 1 != 0, v & 2 != 0]; }
            0x7F4D..=0x7F4E => { let mut p = io.cache.pb as u32; set(&mut p, a - 0x7F4D); io.cache.pb = (p & 0x7FFF) as u16; }
            0x7F4F => {
                io.cache.pc = v;
                if io.halt { io.halt = false; self.r.pb = io.cache.pb; self.r.pc = io.cache.pc; }
            }
            0x7F50 => { io.wait_ram = v as u32 & 7; io.wait_rom = (v as u32 >> 4) & 7; }
            0x7F51 => {
                io.irq = v & 1 != 0;
                if io.irq { self.r.i = false; self.irq_line = false; }
            }
            0x7F52 => io.rom = v & 1 != 0,
            0x7F53 => { io.lock = false; io.halt = true; }
            0x7F55..=0x7F5C => { io.suspend = true; io.suspend_for = (a - 0x7F55) * 32; }
            0x7F5D => io.suspend = false,
            0x7F5E => self.r.i = false, // the line to the S-CPU stays as it is
            0x7F60..=0x7F7F => io.vector[(a & 0x1F) as usize] = v,
            0x7F80..=0x7FAF | 0x7FC0..=0x7FEF => { let o = a & 0x3F; set(&mut self.r.gpr[(o / 3) as usize], o % 3); }
            _ => {}
        }
    }

    // ---------------- registers ----------------

    fn read_reg(&mut self, n: u16) -> u32 {
        match n & 0x7F {
            0x01 => (self.r.mul >> 24) as u32 & M24,
            0x02 => self.r.mul as u32 & M24,
            0x03 => self.r.mdr,
            0x08 => self.r.rom,
            0x0C => self.r.ram,
            0x13 => self.r.mar,
            0x1C => self.r.dpr,
            0x20 => self.r.pc as u32,
            0x28 => self.r.p as u32,
            0x2E | 0x2F => {
                let wait = if n & 0x7F == 0x2E { self.io.wait_rom } else { self.io.wait_ram };
                self.io.bus = BusOp { enable: true, reading: true, writing: false, pending: wait, address: self.r.mar };
                0
            }
            0x50 => 0x000000, 0x51 => 0xFFFFFF, 0x52 => 0x00FF00, 0x53 => 0xFF0000,
            0x54 => 0x00FFFF, 0x55 => 0xFFFF00, 0x56 => 0x800000, 0x57 => 0x7FFFFF,
            0x58 => 0x008000, 0x59 => 0x007FFF, 0x5A => 0xFF7FFF, 0x5B => 0xFFFF7F,
            0x5C => 0x010000, 0x5D => 0xFEFFFF, 0x5E => 0x000100, 0x5F => 0x00FEFF,
            r @ 0x60..=0x7F => self.r.gpr[(r & 0xF) as usize],
            _ => 0,
        }
    }

    fn write_reg(&mut self, n: u16, v: u32) {
        let v = v & M24;
        match n & 0x7F {
            0x01 => self.r.mul = (self.r.mul & 0xFFFFFF) | (v as u64) << 24,
            0x02 => self.r.mul = (self.r.mul & 0xFFFFFF_000000) | v as u64,
            0x03 => self.r.mdr = v,
            0x08 => self.r.rom = v,
            0x0C => self.r.ram = v,
            0x13 => self.r.mar = v,
            0x1C => self.r.dpr = v,
            0x20 => self.r.pc = v as u8,
            0x28 => self.r.p = (v & 0x7FFF) as u16,
            0x2E | 0x2F => {
                let wait = if n & 0x7F == 0x2E { self.io.wait_rom } else { self.io.wait_ram };
                self.io.bus = BusOp { enable: true, reading: false, writing: true, pending: wait, address: self.r.mar };
            }
            r @ 0x60..=0x7F => self.r.gpr[(r & 0xF) as usize] = v,
            _ => {}
        }
    }

    // ---------------- instructions ----------------

    fn flags_nz(&mut self, x: u32) -> u32 {
        self.r.n = x & 0x800000 != 0;
        self.r.z = x == 0;
        x
    }

    fn add(&mut self, x: u32, y: u32) -> u32 {
        let z = x + y;
        self.r.n = z & 0x800000 != 0;
        self.r.z = z & M24 == 0;
        self.r.c = z > M24;
        self.r.v = !(x ^ y) & (x ^ z) & 0x800000 != 0;
        z & M24
    }

    fn sub(&mut self, x: u32, y: u32) -> u32 {
        let z = x as i32 - y as i32;
        self.r.n = z & 0x800000 != 0;
        self.r.z = z & M24 as i32 == 0;
        self.r.c = z >= 0;
        self.r.v = !(x as i32 ^ y as i32) & (x as i32 ^ z) & 0x800000 != 0;
        z as u32 & M24
    }

    fn shift_amount(s: u32) -> u32 { let s = s & 0x1F; if s > 24 { 0 } else { s } }

    fn push(&mut self) {
        self.stack.copy_within(0..7, 1);
        self.stack[0] = (self.r.pb as u32) << 8 | self.r.pc as u32;
    }

    fn pull(&mut self) {
        let pc = self.stack[0];
        self.stack.copy_within(1..8, 0);
        self.stack[7] = 0;
        self.r.pb = (pc >> 8) as u16 & 0x7FFF;
        self.r.pc = pc as u8;
    }

    fn rdram_addr(a: u32) -> usize {
        let mut a = a & 0xFFF;
        if a >= 0xC00 { a -= 0x400; }
        a as usize
    }

    fn instruction(&mut self, op: u16, m: &mut Mem) {
        const SHIFTS: [u32; 4] = [0, 1, 8, 16];
        let sh = SHIFTS[(op >> 8 & 3) as usize];
        let reg = op & 0x7F;
        let imm = (op & 0xFF) as u32;
        let far = op & 0x200 != 0;
        let a_sh = (self.r.a << sh) & M24;
        match op >> 10 {
            0x02..=0x06 | 0x0A..=0x0E => {
                let take = match (op >> 10) & 7 {
                    2 => true, 3 => self.r.z, 4 => self.r.c, 5 => self.r.n, _ => self.r.v,
                };
                if take {
                    if op >> 10 >= 0x0A { self.push(); }
                    if far { self.r.pb = self.r.p; }
                    self.r.pc = imm as u8;
                    self.step(2, m);
                }
            }
            0x07 => if self.io.bus.enable { let p = self.io.bus.pending; self.step(p, m); },
            0x09 => {
                let flag = match op >> 8 & 3 { 0 => self.r.v, 1 => self.r.c, 2 => self.r.z, _ => self.r.n };
                if flag == (op & 1 != 0) { self.advance(m); self.step(1, m); }
            }
            0x0F => { self.pull(); self.step(2, m); }
            0x10 => self.r.mar = (self.r.mar + 1) & M24,
            0x12 => { let y = self.read_reg(reg); self.sub(y, a_sh); }
            0x13 => { self.sub(imm, a_sh); }
            0x14 => { let y = self.read_reg(reg); self.sub(a_sh, y); }
            0x15 => { self.sub(a_sh, imm); }
            0x16 => match op >> 8 {
                0x59 => { let x = self.r.a as u8 as i8 as i32 as u32 & M24; self.r.a = self.flags_nz(x); }
                0x5A => { let x = self.r.a as u16 as i16 as i32 as u32 & M24; self.r.a = self.flags_nz(x); }
                _ => {}
            },
            0x18 => match op >> 8 & 3 {
                0 => self.r.a = self.read_reg(reg),
                1 => self.r.mdr = self.read_reg(reg),
                2 => self.r.mar = self.read_reg(reg),
                _ => self.r.p = (self.r.gpr[(op & 0xF) as usize] & 0x7FFF) as u16,
            },
            0x19 => match op >> 8 & 3 {
                0 => self.r.a = imm,
                1 => self.r.mdr = imm,
                2 => self.r.mar = imm,
                _ => self.r.p = imm as u16,
            },
            0x1A | 0x1B => {
                let byte = (op >> 8 & 3) as u32;
                if byte != 3 {
                    let at = if op >> 10 == 0x1A { self.r.a } else { self.r.dpr.wrapping_add(imm) };
                    let v = self.data_ram[Self::rdram_addr(at)] as u32;
                    self.r.ram = (self.r.ram & !(0xFF << (byte * 8))) | v << (byte * 8);
                }
            }
            0x1C => self.r.rom = self.data_rom[(self.r.a & 0x3FF) as usize],
            0x1D => self.r.rom = self.data_rom[(op & 0x3FF) as usize],
            0x1F => match op >> 8 {
                0x7C => self.r.p = (self.r.p & 0x7F00) | imm as u16,
                0x7D => self.r.p = (self.r.p & 0x00FF) | (op & 0x7F) << 8,
                _ => {}
            },
            0x20 => { let y = self.read_reg(reg); self.r.a = self.add(a_sh, y); }
            0x21 => self.r.a = self.add(a_sh, imm),
            0x22 => { let y = self.read_reg(reg); self.r.a = self.sub(y, a_sh); }
            0x23 => self.r.a = self.sub(imm, a_sh),
            0x24 => { let y = self.read_reg(reg); self.r.a = self.sub(a_sh, y); }
            0x25 => self.r.a = self.sub(a_sh, imm),
            0x26 | 0x27 => {
                let y = if op >> 10 == 0x26 { self.read_reg(reg) } else { imm };
                let sx = |v: u32| ((v << 8) as i32 >> 8) as i64;
                self.r.mul = (sx(self.r.a) * sx(y)) as u64 & 0xFFFF_FFFF_FFFF;
            }
            0x28..=0x2F => {
                let y = if op >> 10 & 1 == 0 { self.read_reg(reg) } else { imm };
                let x = match op >> 11 & 3 { 0 => !a_sh ^ y, 1 => a_sh ^ y, 2 => a_sh & y, _ => a_sh | y } & M24;
                self.r.a = self.flags_nz(x);
            }
            0x30..=0x37 => {
                let s = if op >> 10 & 1 == 0 { self.read_reg(reg) } else { (op & 0x1F) as u32 };
                let s = Self::shift_amount(s);
                let a = self.r.a;
                let x = match op >> 11 & 3 {
                    0 => a >> s,
                    1 => (((a << 8) as i32 >> 8) >> s) as u32,
                    2 => (a >> s) | (a << (24 - s)),
                    _ => a << s,
                } & M24;
                self.r.a = self.flags_nz(x);
            }
            0x38 => match op >> 8 & 3 {
                0 => { let v = self.r.a; self.write_reg(reg, v); }
                1 => { let v = self.r.mdr; self.write_reg(reg, v); }
                _ => {}
            },
            0x3A | 0x3B => {
                let byte = (op >> 8 & 3) as u32;
                if byte != 3 {
                    let at = if op >> 10 == 0x3A { self.r.a } else { self.r.dpr.wrapping_add(imm) };
                    self.data_ram[Self::rdram_addr(at)] = (self.r.ram >> (byte * 8)) as u8;
                }
            }
            0x3C => std::mem::swap(&mut self.r.a, &mut self.r.gpr[(op & 0xF) as usize]),
            0x3E => { self.r.a = 0; self.r.p = 0; self.r.ram = 0; self.r.dpr = 0; }
            0x3F => self.halt(),
            _ => {} // NOP and the unused slots
        }
    }
}

// The HITACHI-LOROM board, as the chip sees its address space (ares "Mapping 0").
fn rom_addr(a: u32) -> Option<usize> {
    if a & 0x408000 == 0x008000 || a & 0xC00000 == 0xC00000 {
        return Some((((a & 0x3F0000) >> 1 | (a & 0x7FFF)) & 0x1FFFFF) as usize);
    }
    None
}

fn ram_addr(a: u32) -> Option<usize> {
    if a & 0xF88000 == 0x700000 { return Some((((a & 0x070000) >> 1 | (a & 0x7FFF)) & 0x3FFFF) as usize); }
    None
}

fn is_rom(a: u32) -> bool { rom_addr(a).is_some() }
fn is_ram(a: u32) -> bool { ram_addr(a).is_some() }
/// data RAM: 00-3F,80-BF:6000-6BFF,7000-7BFF
fn dram_hit(a: u32) -> bool { a & 0x40E000 == 0x006000 && a & 0x0C00 != 0x0C00 }
/// registers: 00-3F,80-BF:6C00-6FFF,7C00-7FFF
fn io_hit(a: u32) -> bool { a & 0x40EC00 == 0x006C00 }
