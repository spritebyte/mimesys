//! The IOP's DMA: the PS1's seven channels plus six new ones (SPU2, DEV9, SIF0, SIF1,
//! SIO2in, SIO2out), DPCR / DPCR2, DICR / DICR2 interrupts (I_STAT bit 3). From ps2tek.
//!
//! SIF0 (channel 9) reads its tags from TADR: word 0 = address (bit 30 IRQ, bit 31 end),
//! word 1 = word count; with CHCR bit 8 the next two words (the EE's DMAtag) go into the
//! FIFO first, and TADR steps 16. SIF1 (channel 10) takes its tags from the FIFO. Counts
//! round up to whole quadwords (the EE side moves quadwords). Channels without a device
//! yet finish at once.

use serde::{Deserialize, Serialize};

use super::hw::Sif;

pub const SIF0: usize = 9;
pub const SIF1: usize = 10;

#[derive(Default, Clone, Serialize, Deserialize)]
pub struct IopChannel {
    pub madr: u32,
    pub bcr: u32,
    pub chcr: u32,
    pub tadr: u32,
    /// words left of the current tag, and whether that tag was the last
    words: u32,
    tag_end: bool,
}

#[derive(Serialize, Deserialize)]
pub struct IopDma {
    pub ch: [IopChannel; 13],
    pub dpcr: u32,
    pub dpcr2: u32,
    pub dicr: u32,
    pub dicr2: u32,
    pub dmacen: u32,
    pub dmacinten: u32,
    /// the master flag as last raised (I_STAT 3 is edge-triggered on it)
    irq_was: bool,
    #[serde(skip)]
    pub done: [u64; 13],
}

impl Default for IopDma {
    fn default() -> Self {
        Self { ch: Default::default(), dpcr: 0x0777_7777, dpcr2: 0, dicr: 0, dicr2: 0, dmacen: 0, dmacinten: 0,
            irq_was: false, done: [0; 13] }
    }
}

fn chan(a: u32) -> Option<usize> {
    match a {
        0x1F80_1080..=0x1F80_10EF => Some(((a - 0x1F80_1080) >> 4) as usize),
        0x1F80_1500..=0x1F80_155F => Some(7 + ((a - 0x1F80_1500) >> 4) as usize),
        _ => None,
    }
}

impl IopDma {
    pub fn read(&self, a: u32) -> u32 {
        match a {
            0x1F80_10F0 => self.dpcr,
            0x1F80_10F4 => self.dicr_value(),
            0x1F80_1570 => self.dpcr2,
            0x1F80_1574 => self.dicr2,
            0x1F80_1578 => self.dmacen,
            0x1F80_157C => self.dmacinten,
            _ => match chan(a) {
                Some(n) => {
                    let c = &self.ch[n];
                    match a & 0xC { 0x0 => c.madr, 0x4 => c.bcr, 0x8 => c.chcr, _ => c.tadr }
                }
                None => 0,
            },
        }
    }

    pub fn write(&mut self, a: u32, v: u32, mask: u32) {
        let merge = |old: u32| (old & !mask) | (v & mask);
        match a {
            0x1F80_10F0 => self.dpcr = merge(self.dpcr),
            0x1F80_10F4 => {
                // flags (24-30) clear on 1; the rest is set as written
                let v = merge(self.dicr);
                let flags = self.dicr & 0x7F00_0000 & !(v & mask & 0x7F00_0000);
                self.dicr = (v & 0x00FF_807F) | flags;
            }
            0x1F80_1570 => self.dpcr2 = merge(self.dpcr2),
            0x1F80_1574 => {
                let v = merge(self.dicr2);
                let flags = self.dicr2 & 0x3F00_0000 & !(v & mask & 0x3F00_0000);
                self.dicr2 = (v & 0x003F_1FFF) | flags;
            }
            0x1F80_1578 => self.dmacen = merge(self.dmacen),
            0x1F80_157C => self.dmacinten = merge(self.dmacinten),
            _ => if let Some(n) = chan(a) {
                let c = &mut self.ch[n];
                match a & 0xC {
                    0x0 => c.madr = merge(c.madr) & 0xFF_FFFF,
                    0x4 => c.bcr = merge(c.bcr),
                    0x8 => {
                        let was = c.chcr >> 24 & 1;
                        c.chcr = merge(c.chcr);
                        if was == 0 && c.chcr >> 24 & 1 != 0 { c.words = 0; c.tag_end = false; }
                    }
                    _ => c.tadr = merge(c.tadr) & 0xFF_FFFF,
                }
            },
        }
    }

    /// DICR with its master flag (bit 31)
    fn dicr_value(&self) -> u32 {
        let flags = self.dicr & 0x7F00_0000 != 0 || self.dicr2 & 0x3F00_0000 != 0;
        let master = self.dicr & 0x8000 != 0 || (self.dicr & 0x80_0000 != 0 && flags);
        (self.dicr & 0x7FFF_FFFF) | (master as u32) << 31
    }

    /// Whether channel `n` is enabled in DPCR / DPCR2
    fn enabled(&self, n: usize) -> bool {
        if n < 7 { self.dpcr >> (4 * n + 3) & 1 != 0 } else { self.dpcr2 >> (4 * (n - 7) + 3) & 1 != 0 }
    }

    fn finish(&mut self, n: usize) {
        self.ch[n].chcr &= !(1 << 24);
        self.done[n] += 1;
        // the flag is raised only if that channel's interrupt is enabled
        if n < 7 {
            if self.dicr >> (16 + n) & 1 != 0 { self.dicr |= 1 << (24 + n); }
        } else if self.dicr2 >> (16 + n - 7) & 1 != 0 {
            self.dicr2 |= 1 << (24 + n - 7);
        }
    }

    /// Move data on every running channel; true when the master flag rises (I_STAT 3)
    pub fn step(&mut self, ram: &mut [u8], sif: &mut Sif) -> bool {
        for n in 0..13 {
            if self.ch[n].chcr >> 24 & 1 == 0 || !self.enabled(n) || self.dmacen & 1 == 0 { continue; }
            match n {
                SIF0 => self.sif0(ram, sif),
                SIF1 => self.sif1(ram, sif),
                _ => self.finish(n),
            }
        }
        let m = self.dicr_value() >> 31 != 0;
        let rise = m && !self.irq_was;
        self.irq_was = m;
        rise
    }

    fn sif0(&mut self, ram: &[u8], sif: &mut Sif) {
        loop {
            let c = &mut self.ch[SIF0];
            while c.words > 0 {
                let o = (c.madr & 0x1F_FFFC) as usize;
                sif.fifo0.push_back(u32::from_le_bytes(ram[o..o + 4].try_into().unwrap()));
                c.madr = (c.madr + 4) & 0xFF_FFFF;
                c.words -= 1;
            }
            if c.tag_end { self.finish(SIF0); return; }
            let t = (c.tadr & 0x1F_FFFC) as usize;
            let w = |k: usize| u32::from_le_bytes(ram[t + 4 * k..t + 4 * k + 4].try_into().unwrap());
            let (t0, t1) = (w(0), w(1));
            c.madr = t0 & 0xFF_FFFF;
            c.words = ((t1 & 0xFF_FFFF) + 3) & !3;
            c.tag_end = t0 >> 30 & 3 != 0;
            if c.chcr >> 8 & 1 != 0 {
                sif.fifo0.push_back(w(2));
                sif.fifo0.push_back(w(3));
                c.tadr = (c.tadr + 16) & 0xFF_FFFF;
            } else {
                c.tadr = (c.tadr + 8) & 0xFF_FFFF;
            }
        }
    }

    fn sif1(&mut self, ram: &mut [u8], sif: &mut Sif) {
        loop {
            let c = &mut self.ch[SIF1];
            while c.words > 0 {
                let Some(v) = sif.fifo1.pop_front() else { return };
                let o = (c.madr & 0x1F_FFFC) as usize;
                ram[o..o + 4].copy_from_slice(&v.to_le_bytes());
                c.madr = (c.madr + 4) & 0xFF_FFFF;
                c.words -= 1;
            }
            if c.tag_end { self.finish(SIF1); return; }
            if sif.fifo1.len() < 2 { return; }
            let t0 = sif.fifo1.pop_front().unwrap();
            let t1 = sif.fifo1.pop_front().unwrap();
            c.madr = t0 & 0xFF_FFFF;
            c.words = ((t1 & 0xFF_FFFF) + 3) & !3;
            c.tag_end = t0 >> 30 & 3 != 0;
        }
    }
}
