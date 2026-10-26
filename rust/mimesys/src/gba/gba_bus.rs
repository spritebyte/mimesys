//! GBA memory bus: the memory map, wait states, I/O registers, interrupts, timers and
//! DMA. The CPU calls it through `ArmBus`; every access adds its cycles to `cycles`,
//! and `tick()` (after each instruction) runs timers, screen timing and DMA up to now.
//!
//!   00000000 BIOS 16K (readable only while executing it)   02000000 EWRAM 256K
//!   03000000 IWRAM 32K   04000000 I/O   05000000 palette 1K   06000000 VRAM 96K
//!   07000000 OAM 1K   08000000/0A000000/0C000000 cart ROM (wait states 0/1/2)
//!   0E000000 cart SRAM (8-bit)
//!
//! Cycle costs (1 + wait): BIOS/IWRAM/IO/OAM 1; palette/VRAM 1 (32-bit 2); EWRAM 3
//! (32-bit 6); ROM and SRAM from WAITCNT, 32-bit ROM = two 16-bit accesses.
//! Prefetch buffer (WAITCNT bit 14): while the CPU runs from ROM, cycles spent
//! elsewhere fetch the next halfwords (up to 8); a sequential opcode fetch that finds
//! them costs 1 cycle.

use serde::{Deserialize, Serialize};

use crate::common::cpu::arm::arm7tdmi::{ArmBus, ACCESS_CODE, ACCESS_SEQ};
use crate::gba::gba_apu::Apu;
use crate::gba::gba_cart::Cart;
use crate::gba::gba_dma::{Dma, TIMING_HBLANK, TIMING_NOW, TIMING_SPECIAL, TIMING_VBLANK};
use crate::gba::gba_sio::Sio;
use crate::gba::gba_ppu::{Ppu, EV_FRAME, EV_HBLANK_DMA, EV_VBLANK_DMA};
use crate::gba::gba_timer::Timers;

/// IF / IE bits
pub const IRQ_VBLANK: u16 = 1 << 0;
pub const IRQ_HBLANK: u16 = 1 << 1;
pub const IRQ_VCOUNT: u16 = 1 << 2;
pub const IRQ_DMA0: u16 = 1 << 8;
pub const IRQ_KEYPAD: u16 = 1 << 12;

const ROM_N: [u32; 4] = [4, 3, 2, 8];
const ROM_S: [[u32; 2]; 3] = [[2, 1], [4, 1], [8, 1]];

#[derive(Serialize, Deserialize)]
pub struct GbaBus {
    #[serde(skip)] pub bios: Vec<u8>,
    #[serde(skip)] pub rom: Vec<u8>,
    pub ewram: Vec<u8>,
    pub iwram: Vec<u8>,
    /// save chip, clock, ROM mirroring
    pub cart: Cart,
    pub ppu: Ppu,
    pub apu: Apu,
    pub timers: Timers,
    pub dma: Dma,
    pub sio: Sio,
    /// other I/O (sound, serial...) kept as plain bytes until those parts exist
    io: Vec<u8>,
    pub ie: u16,
    pub if_: u16,
    pub ime: bool,
    pub waitcnt: u16,
    pub postflg: u8,
    /// HALTCNT was written: the CPU waits for an enabled interrupt
    pub halted: bool,
    /// KEYINPUT (active low: A B Select Start Right Left Up Down R L)
    pub keyinput: u16,
    pub keycnt: u16,
    /// system clocks since power-on
    pub cycles: u64,
    last_tick: u64,
    /// last opcode fetched from the BIOS, returned by reads of it from outside
    bios_latch: u32,
    /// the last code fetch was inside the BIOS
    in_bios: bool,
    /// value of an unmapped read (the last code fetch)
    open_bus: u32,
    /// set when a frame has been finished (cleared by the system)
    pub frame_done: bool,
    // prefetch buffer: halfwords ready from `pf_addr` on, and cycles into the next one
    pf_active: bool,
    pf_addr: u32,
    pf_count: u32,
    pf_progress: u32,
    /// when IME && (IE & IF) last became true
    irq_since: Option<u64>,
    /// mGBA debug port (04FFF600 string, 04FFF700 flags, 04FFF780 enable): homebrew
    /// and test ROMs print through it; lines collect in `debug_log`
    debug_on: bool,
    #[serde(skip)] debug_buf: Vec<u8>,
    #[serde(skip)] pub debug_log: Vec<String>,
}

impl GbaBus {
    pub fn new(bios: Vec<u8>, rom: Vec<u8>) -> Self {
        Self {
            cart: Cart::new(&rom),
            bios, rom,
            ewram: vec![0; 0x40000],
            iwram: vec![0; 0x8000],
            ppu: Ppu::new(),
            apu: Apu::new(),
            timers: Timers::new(),
            dma: Dma::new(),
            sio: Sio::new(),
            io: vec![0; 0x400],
            ie: 0, if_: 0, ime: false, waitcnt: 0, postflg: 0, halted: false,
            keyinput: 0x3FF, keycnt: 0,
            cycles: 0, last_tick: 0,
            bios_latch: 0, in_bios: true, open_bus: 0,
            frame_done: false,
            pf_active: false, pf_addr: 0, pf_count: 0, pf_progress: 0,
            irq_since: None,
            debug_on: false, debug_buf: vec![0; 0x100], debug_log: Vec::new(),
        }
    }

    /// After loading a state: take ROM, BIOS and the framebuffer back.
    pub fn keep_static(&mut self, old: &mut Self) {
        self.bios = std::mem::take(&mut old.bios);
        self.rom = std::mem::take(&mut old.rom);
        self.ppu.framebuffer = std::mem::take(&mut old.ppu.framebuffer);
    }

    /// IRQ line into the CPU.
    /// IRQ line into the CPU: it follows IME && (IE & IF) 3 cycles late.
    pub fn irq_line(&self) -> bool {
        self.irq_since.is_some_and(|t| self.cycles >= t + 3)
    }

    /// When the pending interrupt becomes visible to the CPU (if IME allows one).
    pub fn irq_visible_at(&self) -> Option<u64> { self.irq_since.map(|t| t + 3) }

    /// Re-evaluates the interrupt condition after IE / IF / IME changed at `at`.
    fn update_irq(&mut self, at: u64) {
        let on = self.ime && self.ie & self.if_ != 0;
        if !on { self.irq_since = None; } else if self.irq_since.is_none() { self.irq_since = Some(at); }
    }

    pub fn request_irq(&mut self, bits: u16) {
        self.if_ |= bits;
        self.update_irq(self.cycles);
    }

    /// New key state (active low, 10 bits); checks the keypad interrupt.
    pub fn set_keys(&mut self, keys: u16) {
        self.keyinput = keys & 0x3FF;
        self.check_keypad_irq();
    }

    fn check_keypad_irq(&mut self) {
        if self.keycnt & 0x4000 == 0 { return; }
        let sel = self.keycnt & 0x3FF;
        let pressed = !self.keyinput & 0x3FF;
        let hit = if self.keycnt & 0x8000 != 0 { pressed & sel == sel } else { pressed & sel != 0 };
        if hit && sel != 0 { self.request_irq(IRQ_KEYPAD); }
    }

    // ---- timing ----

    /// Cycles for one access. `wide` = 32-bit.
    fn access_cycles(&self, addr: u32, wide: bool, seq: bool) -> u32 {
        match addr >> 24 {
            0x02 => if wide { 6 } else { 3 },
            0x05 | 0x06 => if wide { 2 } else { 1 },
            0x08..=0x0D => {
                let ws = ((addr >> 25) & 3).min(2) as usize;
                let shift = 2 + ws as u16 * 3;
                let n = 1 + ROM_N[((self.waitcnt >> shift) & 3) as usize];
                let s = 1 + ROM_S[ws][((self.waitcnt >> (shift + 2)) & 1) as usize];
                // a new 128K block always starts non-sequential
                let first = if seq && addr & 0x1_FFFF != 0 { s } else { n };
                if wide { first + s } else { first }
            }
            0x0E | 0x0F => 1 + ROM_N[(self.waitcnt & 3) as usize],
            _ => 1,
        }
    }

    /// Runs timers, screen timing and DMA up to `cycles`.
    pub fn tick(&mut self) {
        loop {
            self.catch_up();
            if !self.dma.any_pending() { break; }
            self.run_dma();
        }
    }

    /// Brings timers, sound and screen timing up to `cycles` (also done before every
    /// I/O access, so registers read mid-instruction are current). DMA waits for `tick`.
    fn catch_up(&mut self) {
        {
            let delta = (self.cycles - self.last_tick) as u32;
            self.last_tick = self.cycles;
            if delta > 0 {
                if let Some(end) = self.sio.end {
                    if self.cycles >= end {
                        self.sio.on_end();
                        if self.sio.take_irq() { self.if_ |= 1 << 7; self.update_irq(end); }
                    }
                }
                // transfers finished by the link (the other GBA, or the network)
                if self.sio.take_irq() { self.if_ |= 1 << 7; self.update_irq(self.cycles); }
                let (over, first_irq) = self.timers.advance_to(self.cycles);
                let bits = self.timers.irq_bits(&over);
                if bits != 0 {
                    self.if_ |= bits;
                    self.update_irq(first_irq.unwrap_or(self.cycles));
                }
                for t in 0..2 {
                    let req = self.apu.timer_overflow(t, over[t]);
                    for f in 0..2 { if req & (1 << f) != 0 { self.dma.trigger_fifo(f); } }
                }
                self.apu.advance(delta);
                let ev = self.ppu.advance(delta);
                if ev != 0 {
                    let bits = self.ppu.irq_bits(ev);
                    if bits != 0 { self.request_irq(bits); }
                    if ev & EV_VBLANK_DMA != 0 { self.dma.trigger(TIMING_VBLANK); }
                    if ev & EV_HBLANK_DMA != 0 { self.dma.trigger(TIMING_HBLANK); }
                    if ev & EV_FRAME != 0 { self.frame_done = true; }
                }
            }
        }
    }

    /// Clocks until the next screen event (for skipping ahead while halted).
    pub fn cycles_to_event(&self) -> u32 {
        let mut c = self.ppu.cycles_to_event() as u64;
        if let Some(t) = self.timers.cycles_to_overflow(self.cycles) { c = c.min(t); }
        if let Some(end) = self.sio.end { c = c.min(end.saturating_sub(self.cycles)); }
        c.min(u32::MAX as u64) as u32
    }

    /// Runs the highest-priority pending DMA channel to completion.
    fn run_dma(&mut self) {
        let Some(n) = (0..4).find(|&i| self.dma.ch[i].pending) else { return };
        let mut c = self.dma.ch[n];
        // sound FIFO refill (DMA 1/2, special timing): 4 words, destination fixed
        let fifo = (n == 1 || n == 2) && c.timing() == TIMING_SPECIAL;
        if fifo { c.count = 4; c.ctrl |= 0x0400; }
        if n == 3 && c.dst >= 0x0D00_0000 { self.cart.eeprom_dma_hint(c.count); }
        let wide = c.ctrl & 0x0400 != 0;
        let unit = if wide { 4 } else { 2 };
        // the cart's address counter only counts up: a ROM source always increments
        let src_in_rom = (0x0800_0000..0x0E00_0000).contains(&c.src);
        let src_step: i32 = if src_in_rom { unit } else { match (c.ctrl >> 7) & 3 { 0 | 3 => unit, 1 => -unit, _ => 0 } };
        let dst_step: i32 = if fifo { 0 } else { match (c.ctrl >> 5) & 3 { 0 | 3 => unit, 1 => -unit, _ => 0 } };
        let (mut src, mut dst) = (c.src, c.dst);
        self.cycles += 2;   // startup
        for i in 0..c.count {
            let seq = i > 0;
            if wide {
                let a = src & !3;
                self.cycles += self.access_cycles(a, true, seq) as u64;
                let v = if dma_can_read(a) { self.read32_raw(a) } else { self.dma.latch };
                self.dma.latch = v;
                let d = dst & !3;
                self.cycles += self.access_cycles(d, true, seq) as u64;
                self.write32_raw(d, v);
            } else {
                let a = src & !1;
                self.cycles += self.access_cycles(a, false, seq) as u64;
                let v = if dma_can_read(a) {
                    let v = self.read16_raw(a);
                    self.dma.latch = v as u32 * 0x0001_0001;
                    v
                } else {
                    (self.dma.latch >> ((a & 2) * 8)) as u16
                };
                let d = dst & !1;
                self.cycles += self.access_cycles(d, false, seq) as u64;
                self.write16_raw(d, v);
            }
            src = src.wrapping_add(src_step as u32);
            dst = dst.wrapping_add(dst_step as u32);
        }
        let ch = &mut self.dma.ch[n];
        ch.pending = false;
        ch.src = src;
        ch.dst = dst;
        if fifo {
            ch.dst = c.dst;     // FIFO DMA keeps its count register and destination
        } else if ch.ctrl & 0x0200 != 0 && ch.timing() != TIMING_NOW {
            // repeat: reload the count (and the destination for "inc+reload")
            let count = self.dma.full_count(n);
            let ch = &mut self.dma.ch[n];
            ch.count = count;
            if (ch.ctrl >> 5) & 3 == 3 { ch.dst = ch.dad & if n == 3 { 0x0FFF_FFFF } else { 0x07FF_FFFF }; }
        } else {
            ch.ctrl &= !0x8000;
        }
        if self.dma.ch[n].ctrl & 0x4000 != 0 { self.request_irq(IRQ_DMA0 << n); }
    }

    // ---- raw memory (no timing) ----

    fn bios_read32(&self, addr: u32) -> u32 {
        if self.in_bios {
            let a = (addr & 0x3FFC) as usize;
            u32::from_le_bytes([self.bios[a], self.bios[a + 1], self.bios[a + 2], self.bios[a + 3]])
        } else {
            self.bios_latch
        }
    }

    pub fn read8_raw(&mut self, addr: u32) -> u8 {
        match addr >> 24 {
            0x00 if addr < 0x4000 && !self.bios.is_empty() => (self.bios_read32(addr) >> ((addr & 3) * 8)) as u8,
            0x02 => self.ewram[(addr & 0x3FFFF) as usize],
            0x03 => self.iwram[(addr & 0x7FFF) as usize],
            0x04 => if addr & 0x00FF_FFFF < 0x400 { self.io_read8(addr & 0x3FF) } else { self.open_bus8(addr) },
            0x05 => self.ppu.palette[(addr & 0x3FF) as usize],
            0x06 => self.ppu.vram[vram_off(addr)],
            0x07 => self.ppu.oam[(addr & 0x3FF) as usize],
            0x08..=0x0D => (self.read16_raw(addr) >> ((addr & 1) * 8)) as u8,
            0x0E | 0x0F => self.cart.read_backup(addr),
            _ => self.open_bus8(addr),
        }
    }

    pub fn read16_raw(&mut self, addr: u32) -> u16 {
        let a = addr & !1;
        match a >> 24 {
            0x00 if a < 0x4000 && !self.bios.is_empty() => (self.bios_read32(a) >> ((a & 2) * 8)) as u16,
            0x02 => le16(&self.ewram, (a & 0x3FFFF) as usize),
            0x03 => le16(&self.iwram, (a & 0x7FFF) as usize),
            0x05 => le16(&self.ppu.palette, (a & 0x3FF) as usize),
            0x06 => le16(&self.ppu.vram, vram_off(a)),
            0x07 => le16(&self.ppu.oam, (a & 0x3FF) as usize),
            0x08..=0x0D => self.rom_read16(a),
            0x0E | 0x0F => self.cart.read_backup(addr) as u16 * 0x0101,
            0x04 if a == 0x04FF_F780 => if self.debug_on { 0x1DEA } else { 0 },
            0x04 if a & 0x00FF_FFFF < 0x400 => {
                let lo = self.io_read8(a & 0x3FF) as u16;
                lo | ((self.io_read8((a & 0x3FF) + 1) as u16) << 8)
            }
            _ => (self.open_bus >> ((a & 2) * 8)) as u16,
        }
    }

    pub fn read32_raw(&mut self, addr: u32) -> u32 {
        let a = addr & !3;
        match a >> 24 {
            0x00 if a < 0x4000 && !self.bios.is_empty() => self.bios_read32(a),
            0x02 => le32(&self.ewram, (a & 0x3FFFF) as usize),
            0x03 => le32(&self.iwram, (a & 0x7FFF) as usize),
            0x05 => le32(&self.ppu.palette, (a & 0x3FF) as usize),
            0x06 => le32(&self.ppu.vram, vram_off(a)),
            0x07 => le32(&self.ppu.oam, (a & 0x3FF) as usize),
            0x0E | 0x0F => self.cart.read_backup(addr) as u32 * 0x0101_0101,
            0x08..=0x0D | 0x04 => self.read16_raw(a) as u32 | ((self.read16_raw(a + 2) as u32) << 16),
            _ => self.open_bus,
        }
    }

    fn open_bus8(&self, addr: u32) -> u8 { (self.open_bus >> ((addr & 3) * 8)) as u8 }

    pub fn write8_raw(&mut self, addr: u32, v: u8) {
        match addr >> 24 {
            0x02 => self.ewram[(addr & 0x3FFFF) as usize] = v,
            0x03 => self.iwram[(addr & 0x7FFF) as usize] = v,
            0x04 => if addr & 0x00FF_FFFF < 0x400 { self.io_write8(addr & 0x3FF, v) } else { self.debug_write(addr, v as u16, 1) },
            // byte writes to palette RAM write the byte to both halves
            0x05 => { let a = (addr & 0x3FE) as usize; self.ppu.palette[a] = v; self.ppu.palette[a + 1] = v; }
            0x06 => {
                // VRAM: like palette for the background area; ignored for sprite tiles
                let limit = if self.ppu.dispcnt() & 7 >= 3 { 0x14000 } else { 0x10000 };
                let a = vram_off(addr) & !1;
                if a < limit { self.ppu.vram[a] = v; self.ppu.vram[a + 1] = v; }
            }
            0x07 => {}   // OAM ignores byte writes
            0x08..=0x0D => self.rom_write16(addr & !1, v as u16 * 0x0101),
            0x0E | 0x0F => self.cart.write_backup(addr, v),
            _ => {}
        }
    }

    pub fn write16_raw(&mut self, addr: u32, v: u16) {
        let a = addr & !1;
        match a >> 24 {
            0x02 => put16(&mut self.ewram, (a & 0x3FFFF) as usize, v),
            0x03 => put16(&mut self.iwram, (a & 0x7FFF) as usize, v),
            0x04 => if a & 0x00FF_FFFF < 0x400 {
                self.io_write8(a & 0x3FF, v as u8);
                self.io_write8((a & 0x3FF) + 1, (v >> 8) as u8);
            } else {
                self.debug_write(a, v, 2);
            },
            0x05 => put16(&mut self.ppu.palette, (a & 0x3FF) as usize, v),
            0x06 => put16(&mut self.ppu.vram, vram_off(a), v),
            0x07 => put16(&mut self.ppu.oam, (a & 0x3FF) as usize, v),
            0x08..=0x0D => self.rom_write16(a, v),
            0x0E | 0x0F => self.cart.write_backup(addr, (v >> ((addr & 1) * 8)) as u8),
            _ => {}
        }
    }

    pub fn write32_raw(&mut self, addr: u32, v: u32) {
        let a = addr & !3;
        match a >> 24 {
            0x0E | 0x0F => self.cart.write_backup(addr, (v >> ((addr & 3) * 8)) as u8),
            _ => {
                self.write16_raw(a, v as u16);
                self.write16_raw(a + 2, (v >> 16) as u16);
            }
        }
    }

    // ---- I/O registers ----

    fn io_read8(&mut self, off: u32) -> u8 {
        self.catch_up();
        match io_read_kind(off) {
            IoRead::OpenBus => return self.open_bus8(off),
            IoRead::Zero => return 0,
            IoRead::Normal => {}
        }
        match off {
            0x000..=0x05F => self.ppu.read8(off),
            0x060..=0x0AF => self.apu.read8(off),
            0x0B0..=0x0DF => self.dma.read8(off - 0xB0),
            0x100..=0x10F => self.timers.read8(off - 0x100, self.cycles),
            0x120..=0x12F | 0x134..=0x135 | 0x140..=0x15F => { self.sio.clock = self.cycles; self.sio.read8_traced(off) }
            0x130 => self.keyinput as u8,
            0x131 => (self.keyinput >> 8) as u8,
            0x132 => self.keycnt as u8,
            0x133 => (self.keycnt >> 8) as u8,
            0x200 => self.ie as u8,
            0x201 => (self.ie >> 8) as u8,
            0x202 => self.if_ as u8,
            0x203 => (self.if_ >> 8) as u8,
            0x204 => self.waitcnt as u8,
            0x205 => (self.waitcnt >> 8) as u8,
            0x208 => self.ime as u8,
            0x209..=0x20B => 0,
            0x300 => self.postflg,
            _ => self.io[off as usize],
        }
    }

    fn io_write8(&mut self, off: u32, v: u8) {
        self.catch_up();
        match off {
            0x000..=0x05F => self.ppu.write8(off, v),
            0x060..=0x0AF => self.apu.write8(off, v),
            0x0B0..=0x0DF => self.dma.write8(off - 0xB0, v),
            0x100..=0x10F => self.timers.write8(off - 0x100, v, self.cycles),
            0x120..=0x12F | 0x134..=0x135 | 0x140..=0x15F => self.sio.write8(off, v, self.cycles),
            0x130 | 0x131 => {}
            0x132 => { self.keycnt = (self.keycnt & 0xFF00) | v as u16; self.check_keypad_irq(); }
            0x133 => { self.keycnt = (self.keycnt & 0x00FF) | ((v as u16 & 0xC3) << 8); self.check_keypad_irq(); }
            0x200 => { self.ie = (self.ie & 0xFF00) | v as u16; self.update_irq(self.cycles + 1); }
            0x201 => { self.ie = (self.ie & 0x00FF) | ((v as u16 & 0x3F) << 8); self.update_irq(self.cycles + 1); }
            0x202 => { self.if_ &= !(v as u16); self.update_irq(self.cycles + 1); }
            0x203 => { self.if_ &= !((v as u16) << 8); self.update_irq(self.cycles + 1); }
            0x204 => self.waitcnt = (self.waitcnt & 0xFF00) | v as u16,
            0x205 => self.waitcnt = (self.waitcnt & 0x80FF) | ((v as u16 & 0x5F) << 8),
            0x208 => { self.ime = v & 1 != 0; self.update_irq(self.cycles + 1); }
            0x209..=0x20B => {}
            0x300 => self.postflg = v & 1,
            0x301 => if v & 0x80 == 0 { self.halted = true },   // STOP (bit 7) not emulated
            _ => self.io[off as usize] = v,
        }
    }
}

impl ArmBus for GbaBus {
    fn read8(&mut self, addr: u32, access: u8) -> u8 {
        self.charge(addr, false, access);
        self.read8_raw(addr)
    }

    fn read16(&mut self, addr: u32, access: u8) -> u16 {
        self.charge(addr, false, access);
        if access & ACCESS_CODE != 0 { self.code_region(addr); }
        let v = self.read16_raw(addr);
        if access & ACCESS_CODE != 0 {
            self.open_bus = v as u32 * 0x0001_0001;
            if addr < 0x4000 && !self.bios.is_empty() { self.bios_latch = self.bios_read32(addr); }
        }
        v
    }

    fn read32(&mut self, addr: u32, access: u8) -> u32 {
        self.charge(addr, true, access);
        if access & ACCESS_CODE != 0 { self.code_region(addr); }
        let v = self.read32_raw(addr);
        if access & ACCESS_CODE != 0 {
            self.open_bus = v;
            if addr < 0x4000 { self.bios_latch = v; }
        }
        v
    }

    fn write8(&mut self, addr: u32, val: u8, access: u8) {
        self.charge(addr, false, access);
        self.write8_raw(addr, val);
    }

    fn write16(&mut self, addr: u32, val: u16, access: u8) {
        self.charge(addr, false, access);
        self.write16_raw(addr, val);
    }

    fn write32(&mut self, addr: u32, val: u32, access: u8) {
        self.charge(addr, true, access);
        self.write32_raw(addr, val);
    }

    fn idle(&mut self) { self.spend(1); }
}

impl GbaBus {
    /// A code fetch: notes whether the CPU is now running inside the BIOS.
    fn code_region(&mut self, addr: u32) {
        self.in_bios = addr < 0x4000;
    }

    fn debug_write(&mut self, addr: u32, v: u16, size: u32) {
        if self.debug_buf.len() < 0x100 { self.debug_buf.resize(0x100, 0); }   // after a state load
        match addr {
            0x04FF_F780 => self.debug_on = v == 0xC0DE,
            0x04FF_F700 if self.debug_on && v & 0x100 != 0 => {
                let end = self.debug_buf.iter().position(|&c| c == 0).unwrap_or(0x100);
                let line = String::from_utf8_lossy(&self.debug_buf[..end]).to_string();
                if self.debug_log.len() < 100_000 { self.debug_log.push(line); }
                self.debug_buf.fill(0);
            }
            0x04FF_F600..=0x04FF_F6FF if self.debug_on => {
                let o = (addr - 0x04FF_F600) as usize;
                for i in 0..size as usize {
                    if o + i < 0x100 { self.debug_buf[o + i] = (v >> (8 * i)) as u8; }
                }
            }
            _ => {}
        }
    }

    /// Cart ROM halfword: clock port, EEPROM, mirroring, then the ROM or (past its
    /// end) the address bus value.
    fn rom_read16(&mut self, a: u32) -> u16 {
        if self.cart.is_gpio(a) {
            if let Some(v) = self.cart.gpio_read(a) { return v; }
        }
        if a >> 24 >= 0x0D && self.cart.eeprom_at(a, self.rom.len()) {
            return self.cart.eeprom_read(self.cycles);
        }
        let o = (a & 0x01FF_FFFF & self.cart.rom_mask) as usize;
        if o + 1 < self.rom.len() { le16(&self.rom, o) } else { (a >> 1) as u16 }
    }

    fn rom_write16(&mut self, a: u32, v: u16) {
        if self.cart.is_gpio(a) { self.cart.gpio_write(a, v); }
        if a >> 24 >= 0x0D && self.cart.eeprom_at(a, self.rom.len()) {
            self.cart.eeprom_write(v, self.cycles);
        }
    }

    /// Charges the cycles of one CPU access, using the prefetch buffer for opcodes.
    fn charge(&mut self, addr: u32, wide: bool, access: u8) {
        let seq = access & ACCESS_SEQ != 0;
        if !(0x08..=0x0D).contains(&(addr >> 24)) {
            let c = self.access_cycles(addr, wide, seq);
            self.spend(c);
            return;
        }
        let code = access & ACCESS_CODE != 0;
        let need = if wide { 2 } else { 1 };
        // an opcode the buffer already holds (or is fetching) is served from it,
        // even right after an internal cycle (which the CPU marks non-sequential)
        if code && self.waitcnt & 0x4000 != 0 && self.pf_active && addr & !1 == self.pf_addr {
            let cost = if self.pf_count >= need {
                1
            } else {
                // wait for the halfwords still being fetched
                let s = self.access_cycles(self.pf_addr | 2, false, true);
                ((need - self.pf_count) * s).saturating_sub(self.pf_progress).max(1)
            };
            self.cycles += cost as u64;
            self.pf_count = self.pf_count.saturating_sub(need);
            if self.pf_count == 0 { self.pf_progress = 0; }
            self.pf_addr = self.pf_addr.wrapping_add(2 * need);
            return;
        }
        self.cycles += self.access_cycles(addr, wide, seq) as u64;
        self.pf_active = code;
        self.pf_addr = (addr & !1).wrapping_add(2 * need);
        self.pf_count = 0;
        self.pf_progress = 0;
    }

    /// Cycles outside the cart: the prefetcher keeps reading ahead meanwhile.
    fn spend(&mut self, c: u32) {
        self.cycles += c as u64;
        if self.pf_active && self.waitcnt & 0x4000 != 0 && self.pf_count < 8 {
            self.pf_progress += c;
            let s = self.access_cycles(self.pf_addr | 2, false, true);
            while self.pf_progress >= s && self.pf_count < 8 {
                self.pf_progress -= s;
                self.pf_count += 1;
            }
            if self.pf_count == 8 { self.pf_progress = 0; }
        }
    }
}

/// DMA sees nothing in the BIOS or unmapped space: it gets its own last value instead.
fn dma_can_read(a: u32) -> bool { (0x0200_0000..0x1000_0000).contains(&a) }

enum IoRead { Normal, OpenBus, Zero }

/// What a read of I/O byte `off` gives: write-only and unused registers in the video,
/// DMA and FIFO areas show open bus; other unused addresses read 0.
fn io_read_kind(off: u32) -> IoRead {
    match off {
        0x010..=0x047 | 0x04C..=0x04F | 0x054..=0x05F | 0x08C..=0x08F | 0x0A0..=0x0FF => {
            if (0x0B0..0x0E0).contains(&off) {
                match (off - 0xB0) % 12 {
                    0..=7 => IoRead::OpenBus,       // SAD / DAD
                    8 | 9 => IoRead::Zero,          // word count
                    _ => IoRead::Normal,            // control
                }
            } else {
                IoRead::OpenBus
            }
        }
        0x000..=0x00F | 0x048..=0x04B | 0x050..=0x053 | 0x060..=0x08B | 0x090..=0x09F => IoRead::Normal,
        0x100..=0x10F | 0x120..=0x12F | 0x130..=0x135 | 0x140..=0x141 | 0x150..=0x159 => IoRead::Normal,
        0x200..=0x205 | 0x208 | 0x300 | 0x301 => IoRead::Normal,
        _ => IoRead::Zero,
    }
}

fn vram_off(addr: u32) -> usize {
    let o = (addr & 0x1FFFF) as usize;
    if o >= 0x18000 { o - 0x8000 } else { o }
}

fn le16(m: &[u8], o: usize) -> u16 { u16::from_le_bytes([m[o], m[o + 1]]) }
fn le32(m: &[u8], o: usize) -> u32 { u32::from_le_bytes([m[o], m[o + 1], m[o + 2], m[o + 3]]) }
fn put16(m: &mut [u8], o: usize, v: u16) { m[o..o + 2].copy_from_slice(&v.to_le_bytes()); }
