//! The PlayStation's memory map, interrupt controller and DMA, from Nocash PSX-SPX.
//!
//! Physical map: 00000000 RAM (2 MB, mirrored to 8 MB), 1F000000 expansion 1 (empty:
//! reads FF), 1F801000 I/O, 1F802000 expansion 2 (POST / debug UART), 1FC00000 BIOS
//! (512 KB). The CPU core masks the segments and owns the scratchpad.
//!
//! - I/O: 1000 memory control, 1040 SIO0 (pads), 1060 RAM size, 1070 I_STAT / 1074 I_MASK,
//!   1080-10FF DMA, 1100-112F timers, 1800 CD-ROM, 1810 / 1814 GPU, 1820 MDEC, 1C00 SPU.
//! - Interrupts: I_STAT bits 0 vblank, 1 GPU, 2 CD-ROM, 3 DMA, 4-6 timers, 7 SIO0, 9 SPU;
//!   writing I_STAT clears the bits written as 0. The CPU's line is I_STAT & I_MASK != 0.
//! - DMA: seven channels (MADR / BCR / CHCR), DPCR (priorities and enables), DICR (IRQs).
//!   Transfers run to completion at once. Sync mode 0 (all at once), 1 (blocks) and 2
//!   (the GPU's linked list); channel 6 clears an ordering table backwards.

use super::cdrom::Cdrom;
use super::gpu::Gpu;
use super::mdec::Mdec;
use super::sio::Sio;
use super::spu::Spu;
use super::timers::Timers;
use crate::common::cpu::r3000::R3000Bus;
use serde::{Deserialize, Serialize};

pub const RAM_SIZE: usize = 0x20_0000;
pub const BIOS_SIZE: usize = 0x8_0000;

pub const IRQ_VBLANK: u32 = 1 << 0;
pub const IRQ_GPU: u32 = 1 << 1;
pub const IRQ_CDROM: u32 = 1 << 2;
pub const IRQ_DMA: u32 = 1 << 3;
pub const IRQ_SIO0: u32 = 1 << 7;
pub const IRQ_SPU: u32 = 1 << 9;

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
pub struct DmaChannel {
    pub madr: u32,
    pub bcr: u32,
    pub chcr: u32,
}

#[derive(Serialize, Deserialize)]
pub struct Bus {
    pub ram: Vec<u8>,
    #[serde(skip)]
    pub bios: Vec<u8>,
    pub i_stat: u32,
    pub i_mask: u32,
    pub dma: [DmaChannel; 7],
    pub dpcr: u32,
    pub dicr: u32,
    pub mem_ctrl: [u32; 9],
    pub ram_size: u32,
    pub com_delay: u32,
    pub timers: Timers,
    pub gpu: Gpu,
    pub cdrom: Cdrom,
    pub spu: Spu,
    pub sio: Sio,
    pub mdec: Mdec,
    /// extra cycles the last accesses cost (wait states), collected by the system
    #[serde(skip)]
    pub wait: u32,
    /// characters from the debug UART (expansion 2)
    #[serde(skip)]
    pub tty: Vec<u8>,
}

/// The BIOS's region letter, from "System ROM Version 2.2 12/04/95 A" near its end
pub fn bios_region(bios: &[u8]) -> u8 {
    let key = b"System ROM Version";
    bios.windows(key.len()).position(|w| w == key)
        .and_then(|at| bios[at..(at + 40).min(bios.len())].iter().position(|&c| c == 0).map(|end| (at, end)))
        .and_then(|(at, end)| bios.get(at + end - 1).copied())
        .filter(|c| c.is_ascii_uppercase())
        .unwrap_or(b'A')
}

impl Bus {
    pub fn new(bios: Vec<u8>) -> Self {
        let mut b = bios;
        b.resize(BIOS_SIZE, 0xFF);
        let region = bios_region(&b);
        Self {
            ram: vec![0; RAM_SIZE],
            bios: b,
            i_stat: 0,
            i_mask: 0,
            dma: [DmaChannel::default(); 7],
            dpcr: 0x0765_4321,
            dicr: 0,
            mem_ctrl: [0; 9],
            ram_size: 0x0000_0B88,
            com_delay: 0,
            timers: Timers::new(),
            gpu: Gpu::new(),
            cdrom: Cdrom::new(None, region),
            spu: Spu::new(),
            sio: Sio::new(),
            mdec: Mdec::new(),
            wait: 0,
            tty: Vec::new(),
        }
    }

    pub fn irq_line(&self) -> bool { self.i_stat & self.i_mask != 0 }

    pub fn raise(&mut self, bits: u32) { self.i_stat |= bits; }

    /// Advance the devices; collect their interrupt requests.
    pub fn tick(&mut self, cycles: u32) {
        self.timers.dot_div = self.gpu.dot_div();
        self.timers.tick(cycles);
        if self.timers.irq != 0 { self.i_stat |= self.timers.irq; self.timers.irq = 0; }
        self.cdrom.tick(cycles);
        if self.cdrom.irq { self.cdrom.irq = false; self.i_stat |= IRQ_CDROM; }
        self.spu.tick(cycles, &mut self.cdrom.audio);
        if self.spu.irq { self.spu.irq = false; self.i_stat |= IRQ_SPU; }
        self.sio.tick(cycles);
        // MDEC output that became readable may let a waiting channel 1 go
        if self.mdec.tick(cycles) { self.try_dma(1); }
        if self.sio.irq { self.sio.irq = false; self.i_stat |= IRQ_SIO0; }
        if self.gpu.irq { self.i_stat |= IRQ_GPU; }
    }

    // ------------------------------------------------------------------ I/O

    fn io_read(&mut self, a: u32, size: u32) -> u32 {
        let o = a & 0xFFF;
        self.wait += 2;
        match o {
            0x000..=0x023 => self.mem_ctrl[(o >> 2) as usize],
            0x040..=0x04F => {
                let v = self.sio.read(a);
                if size == 1 { v & 0xFF } else { v }
            }
            0x060..=0x063 => self.ram_size,
            0x070..=0x073 => self.i_stat >> ((o & 3) * 8),
            0x074..=0x077 => self.i_mask >> ((o & 3) * 8),
            0x080..=0x0EF => {
                let ch = &self.dma[((o - 0x80) >> 4) as usize];
                let v = match o & 0xC { 0 => ch.madr, 4 => ch.bcr, 8 => ch.chcr, _ => 0 };
                v >> ((o & 3) * 8)
            }
            0x0F0..=0x0F3 => self.dpcr >> ((o & 3) * 8),
            0x0F4..=0x0F7 => self.dicr >> ((o & 3) * 8),
            0x100..=0x12F => self.timers.read(a & !3) >> ((o & 3) * 8),
            // the data FIFO gives as many bytes as the read is wide
            0x802 if size > 1 => (0..size).fold(0, |v, k| v | (self.cdrom.read(a) as u32) << (8 * k)),
            0x800..=0x803 => self.cdrom.read(a) as u32,
            0x810 => self.gpu.read(),
            0x814 => self.gpu.status(),
            0x820 => self.mdec.read(),
            0x824 => self.mdec.status(),
            0xC00..=0xFFF => {
                let v = self.spu.read16(a & !1) as u32;
                if size == 4 { v | (self.spu.read16((a & !1) + 2) as u32) << 16 } else { v >> ((a & 1) * 8) }
            }
            _ => 0,
        }
    }

    /// A 32-bit register's current value, without read side effects (for narrow writes)
    fn reg32(&self, o: u32) -> Option<u32> {
        let o = o & !3;
        Some(match o {
            0x000..=0x023 => self.mem_ctrl[(o >> 2) as usize],
            0x060 => self.ram_size,
            0x074 => self.i_mask,
            0x080..=0x0EF => {
                let ch = &self.dma[((o - 0x80) >> 4) as usize];
                match o & 0xC { 0 => ch.madr, 4 => ch.bcr, 8 => ch.chcr, _ => return None }
            }
            0x0F0 => self.dpcr,
            0x0F4 => self.dicr & 0x00FF_803F,                 // flags: see io_write
            0x100..=0x12F => {
                let n = ((o >> 4) & 3) as usize;
                if n > 2 { return None; }
                let t = &self.timers.t[n];
                match o & 0xF { 0 => t.count, 4 => t.mode & 0x3FF, 8 => t.target, _ => return None }
            }
            _ => return None,
        })
    }

    fn io_write(&mut self, a: u32, v: u32, size: u32) {
        let o = a & 0xFFF;
        // A byte / halfword write to a 32-bit register lands in its byte lane: merge it
        // with the rest (the BIOS and libraries write DICR's enable byte with SB, say).
        // DICR's flags (24-30) are write-1-to-clear, so only the written lane can clear.
        if size < 4 && !(0x070..=0x073).contains(&o) {
            if let Some(cur) = self.reg32(o) {
                let sh = (o & 3) * 8;
                let mask = (((1u64 << (size * 8)) - 1) as u32) << sh;
                let merged = (cur & !mask) | ((v << sh) & mask);
                if o & !3 == 0x0F4 {
                    // flags outside the lane stay as they are
                    let keep = self.dicr & 0x7F00_0000 & !mask;
                    let clear = (v << sh) & mask & 0x7F00_0000;
                    let flags = self.dicr & 0x7F00_0000 & !clear;
                    self.dicr = (merged & 0x00FF_803F) | flags | keep | (self.dicr & 0x8000_0000);
                    self.update_dicr();
                    return;
                }
                return self.io_write(a & !3, merged, 4);
            }
        }
        match o {
            0x000..=0x023 => self.mem_ctrl[(o >> 2) as usize] = v,
            0x040..=0x04F => self.sio.write(a, v),
            0x060..=0x063 => self.ram_size = v,
            0x070..=0x073 => {
                // bits written as 0 are acknowledged
                let sh = (o & 3) * 8;
                let m = if size == 4 { !0 } else { ((1u32 << (size * 8)) - 1) << sh };
                self.i_stat &= (v << sh) | !m;
            }
            0x074..=0x077 => self.i_mask = v & 0x7FF,
            0x080..=0x0EF => {
                let n = ((o - 0x80) >> 4) as usize;
                match o & 0xC {
                    0 => self.dma[n].madr = v & 0xFF_FFFF,
                    4 => self.dma[n].bcr = v,
                    8 => {
                        // channel 6 has only a few writable bits
                        self.dma[n].chcr = if n == 6 { (v & 0x5100_0000) | 2 } else { v & 0x7177_0703 };
                        self.try_dma(n);
                    }
                    _ => {}
                }
            }
            0x0F0..=0x0F3 => {
                self.dpcr = v;
                for n in 0..7 { self.try_dma(n); }
            }
            0x0F4..=0x0F7 => {
                // bits 0-5, 15-23 written; 24-30 cleared by writing 1
                // (bit 31 is kept: update_dicr raises the IRQ only when it goes 0 -> 1)
                let flags = self.dicr & 0x7F00_0000 & !(v & 0x7F00_0000);
                self.dicr = (v & 0x00FF_803F) | flags | (self.dicr & 0x8000_0000);
                self.update_dicr();
            }
            0x100..=0x12F => self.timers.write(a & !3, v),
            0x800..=0x803 => self.cdrom.write(a, v as u8),
            0x810 => self.gpu.gp0(v),
            0x814 => self.gpu.gp1(v),
            0x820 => {
                self.mdec.write(v);
                self.try_dma(1);
            }
            0x824 => self.mdec.control(v),
            0xC00..=0xFFF => {
                self.spu.write16(a & !1, v as u16);
                if size == 4 { self.spu.write16((a & !1) + 2, (v >> 16) as u16); }
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------------ DMA

    /// DICR bit 31: bus error, or master enable and any flag (the per-channel enables
    /// only gate the setting of a flag, PSX-SPX); its rising edge raises IRQ3
    fn update_dicr(&mut self) {
        let old = self.dicr & 0x8000_0000;
        let flags = (self.dicr >> 24) & 0x7F;
        let master = self.dicr & 0x0080_0000 != 0;
        let on = self.dicr & 0x8000 != 0 || (master && flags != 0);
        self.dicr = (self.dicr & 0x7FFF_FFFF) | (on as u32) << 31;
        if on && old == 0 { self.i_stat |= IRQ_DMA; }
    }

    fn try_dma(&mut self, n: usize) {
        let ch = self.dma[n];
        let enabled = self.dpcr & (8 << (n * 4)) != 0;
        let busy = ch.chcr & 0x0100_0000 != 0;
        let sync = (ch.chcr >> 9) & 3;
        let trigger = ch.chcr & 0x1000_0000 != 0;
        if !(enabled && busy && (sync != 0 || trigger)) { return; }
        // MDEC out waits until the decoder has that much output
        if n == 1 && self.mdec.out_words() < Self::dma_words(&ch) { return; }
        match sync {
            2 => self.dma_list(n),
            _ => self.dma_block(n),
        }
        let c = &mut self.dma[n];
        c.chcr &= !0x1100_0000;
        if sync == 1 {
            // the block count runs out
            c.bcr &= 0xFFFF;
        }
        // a flag is set only with both its channel's enable and the master enable on
        if self.dicr & (1 << (16 + n)) != 0 && self.dicr & 0x0080_0000 != 0 {
            self.dicr |= 1 << (24 + n);
        }
        self.update_dicr();
        // MDEC in may have produced what a waiting MDEC out needs
        if n == 0 { self.try_dma(1); }
    }

    fn dma_words(ch: &DmaChannel) -> usize {
        if (ch.chcr >> 9) & 3 == 0 {
            let w = ch.bcr & 0xFFFF;
            if w == 0 { 0x1_0000 } else { w as usize }
        } else {
            ((ch.bcr & 0xFFFF).max(1) * (ch.bcr >> 16).max(1)) as usize
        }
    }

    fn dma_block(&mut self, n: usize) {
        let ch = self.dma[n];
        let words = Self::dma_words(&ch) as u32;
        let from_ram = ch.chcr & 1 != 0;
        let step: i32 = if ch.chcr & 2 != 0 { -4 } else { 4 };
        let mut addr = ch.madr & 0x1F_FFFC;
        for k in 0..words {
            let a = (addr & 0x1F_FFFC) as usize;
            if from_ram {
                let w = u32::from_le_bytes(self.ram[a..a + 4].try_into().unwrap());
                match n {
                    0 => self.mdec.write(w),
                    2 => self.gpu.gp0(w),
                    4 => self.spu.dma_write(w),
                    _ => {}
                }
            } else {
                let w = match n {
                    6 => if k == words - 1 { 0x00FF_FFFF } else { addr.wrapping_sub(4) & 0x1F_FFFF },
                    1 => self.mdec.read(),
                    2 => self.gpu.read(),
                    3 => self.cdrom.dma_word(),
                    4 => self.spu.dma_read(),
                    _ => 0,
                };
                self.ram[a..a + 4].copy_from_slice(&w.to_le_bytes());
            }
            addr = (addr as i32).wrapping_add(step) as u32;
        }
        self.dma[n].madr = addr & 0xFF_FFFF;
    }

    /// The GPU's linked list: a header word (count << 24 | next) and its commands.
    fn dma_list(&mut self, n: usize) {
        if n != 2 { return; }
        let mut addr = self.dma[n].madr & 0x1F_FFFC;
        // a broken list could loop forever: stop after the RAM's worth of nodes
        for _ in 0..0x8_0000 {
            let a = addr as usize;
            let header = u32::from_le_bytes(self.ram[a..a + 4].try_into().unwrap());
            for k in 0..(header >> 24) {
                let p = ((addr + 4 + 4 * k) & 0x1F_FFFC) as usize;
                let w = u32::from_le_bytes(self.ram[p..p + 4].try_into().unwrap());
                self.gpu.gp0(w);
            }
            if header & 0x80_0000 != 0 { break; }
            addr = header & 0x1F_FFFC;
        }
        self.dma[n].madr = 0x00FF_FFFF;
    }

    // ------------------------------------------------------------------ raw access

    fn ram_at(&self, pa: u32) -> Option<usize> {
        if pa < 0x0080_0000 { Some((pa as usize) & (RAM_SIZE - 1)) } else { None }
    }
}

impl R3000Bus for Bus {
    /// Cached fetches cost nothing extra (the I-cache isn't modelled: every fetch hits);
    /// uncached ones pay the memory's wait states.
    fn fetch32(&mut self, pa: u32, cached: bool) -> u32 {
        let w = self.wait;
        let v = self.read32(pa);
        if cached { self.wait = w; }
        v
    }

    fn read32(&mut self, pa: u32) -> u32 {
        if let Some(a) = self.ram_at(pa) {
            self.wait += 3;
            return u32::from_le_bytes(self.ram[a..a + 4].try_into().unwrap());
        }
        match pa {
            0x1FC0_0000..=0x1FC7_FFFF => {
                self.wait += 6;
                let a = (pa - 0x1FC0_0000) as usize;
                u32::from_le_bytes(self.bios[a..a + 4].try_into().unwrap())
            }
            0x1F80_1000..=0x1F80_1FFF => self.io_read(pa, 4),
            0x1F00_0000..=0x1F7F_FFFF => 0xFFFF_FFFF,
            _ => 0,
        }
    }

    fn read16(&mut self, pa: u32) -> u16 {
        if let Some(a) = self.ram_at(pa) {
            self.wait += 3;
            return u16::from_le_bytes([self.ram[a], self.ram[a + 1]]);
        }
        match pa {
            0x1FC0_0000..=0x1FC7_FFFF => {
                let a = (pa - 0x1FC0_0000) as usize;
                u16::from_le_bytes([self.bios[a], self.bios[a + 1]])
            }
            0x1F80_1000..=0x1F80_1FFF => self.io_read(pa, 2) as u16,
            0x1F00_0000..=0x1F7F_FFFF => 0xFFFF,
            _ => 0,
        }
    }

    fn read8(&mut self, pa: u32) -> u8 {
        if let Some(a) = self.ram_at(pa) {
            self.wait += 3;
            return self.ram[a];
        }
        match pa {
            0x1FC0_0000..=0x1FC7_FFFF => self.bios[(pa - 0x1FC0_0000) as usize],
            0x1F80_1000..=0x1F80_1FFF => self.io_read(pa, 1) as u8,
            0x1F00_0000..=0x1F7F_FFFF => 0xFF,
            _ => 0,
        }
    }

    fn write32(&mut self, pa: u32, v: u32) {
        if let Some(a) = self.ram_at(pa) {
            self.ram[a..a + 4].copy_from_slice(&v.to_le_bytes());
            return;
        }
        if (0x1F80_1000..=0x1F80_1FFF).contains(&pa) { self.io_write(pa, v, 4); }
    }

    fn write16(&mut self, pa: u32, v: u16) {
        if let Some(a) = self.ram_at(pa) {
            self.ram[a..a + 2].copy_from_slice(&v.to_le_bytes());
            return;
        }
        if (0x1F80_1000..=0x1F80_1FFF).contains(&pa) { self.io_write(pa, v as u32, 2); }
    }

    fn write8(&mut self, pa: u32, v: u8) {
        if let Some(a) = self.ram_at(pa) {
            self.ram[a] = v;
            return;
        }
        match pa {
            0x1F80_1000..=0x1F80_1FFF => self.io_write(pa, v as u32, 1),
            0x1F80_2023 | 0x1F80_2080 => self.tty.push(v),     // DUART channel A / debug port
            _ => {}
        }
    }
}
