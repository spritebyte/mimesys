//! The IOP's DMA: the PS1's seven channels plus six new ones (SPU2, DEV9, SIF0, SIF1,
//! SIO2in, SIO2out), DPCR / DPCR2, DICR / DICR2 interrupts (I_STAT bit 3). From ps2tek.
//!
//! SIF0 (channel 9) reads its tags from TADR: word 0 = address (bit 30 IRQ, bit 31 end),
//! word 1 = word count; with CHCR bit 8 a quadword from TADR + 8 (the EE's DMAtag) goes
//! into the FIFO first, and TADR steps 16. SIF1 (channel 10) takes its tags from the FIFO,
//! a quadword each (the first two words count). Counts
//! round up to whole quadwords (the EE side moves quadwords). Channels without a device
//! yet finish at once.

use serde::{Deserialize, Serialize};

use super::hw::Sif;
use super::sio2::Sio2;
use super::spu2::Spu2;

pub const CDVD: usize = 3;
pub const SIF0: usize = 9;
pub const SIF1: usize = 10;
pub const SIO2_IN: usize = 11;
pub const SIO2_OUT: usize = 12;

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
    /// SPU2 cores 0 / 1 (channels 4 / 7) in auto-DMA: a transfer lasts as long as its
    /// samples play (48 kHz stereo 16-bit: 768 IOP cycles a word); cycles still to go
    #[serde(default)]
    pub spu_timer: [i64; 2],
    /// auto-DMA words still to send (channels 4 / 7)
    #[serde(default)]
    pub adma_left: [u32; 2],
}

/// IOP cycles per word of auto-DMA audio (36 864 000 / 48 000)
const ADMA_CYCLES_PER_WORD: i64 = 768;

impl Default for IopDma {
    fn default() -> Self {
        Self { ch: Default::default(), dpcr: 0x0777_7777, dpcr2: 0, dicr: 0, dicr2: 0, dmacen: 0, dmacinten: 0,
            irq_was: false, done: [0; 13], spu_timer: [0; 2], adma_left: [0; 2] }
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
    pub fn step(&mut self, ram: &mut [u8], sif: &mut Sif, sio2: &mut Sio2, spu2: &mut Spu2) -> bool {
        for n in 0..13 {
            if self.ch[n].chcr >> 24 & 1 == 0 || !self.enabled(n) || self.dmacen & 1 == 0 { continue; }
            match n {
                SIF0 => self.sif0(ram, sif),
                SIF1 => self.sif1(ram, sif),
                SIO2_IN => self.sio2_in(ram, sio2),
                SIO2_OUT => self.sio2_out(ram, sio2),
                CDVD => {}                                       // the drive feeds it (cdvd.rs)
                4 | 7 => {
                    let k = (n == 7) as usize;
                    let adma = spu2.admas(k);
                    if !adma {
                        let words = self.block_words(n);
                        let c = &mut self.ch[n];
                        if c.chcr & 1 != 0 {
                            let data: Vec<u16> = (0..words * 2).map(|i| {
                                let a = ((c.madr as usize + 2 * i) & 0x1F_FFFE) as usize;
                                u16::from_le_bytes([ram[a], ram[a + 1]])
                            }).collect();
                            spu2.dma_write(k, &data);
                        } else {
                            let data = spu2.dma_read(k, words * 2);
                            for (i, w) in data.iter().enumerate() {
                                let a = ((c.madr as usize + 2 * i) & 0x1F_FFFE) as usize;
                                ram[a..a + 2].copy_from_slice(&w.to_le_bytes());
                            }
                        }
                        c.madr = (c.madr + 4 * words as u32) & 0xFF_FFFF;
                        self.finish(n);
                    } else {
                        // auto-DMA: one block (100h words: 100h left + 100h right samples)
                        // whenever the SPU2 has a free half, so the data plays in order;
                        // the channel ends with the last block (block size 0 moves 16
                        // words: Suikoden 3's 00C00000 is 12 KB)
                        if self.adma_left[k] == 0 {
                            let bcr = self.ch[n].bcr;
                            let bs = match bcr & 0xFFFF { 0 => 16, b => b } as u32;
                            self.adma_left[k] = (bs * (bcr >> 16).max(1)).max(1);
                        }
                        while self.adma_left[k] > 0 && spu2.adma_room(k) {
                            let words = self.adma_left[k].min(0x100) as usize;
                            let c = &mut self.ch[n];
                            let data: Vec<u16> = (0..words * 2).map(|i| {
                                let a = ((c.madr as usize + 2 * i) & 0x1F_FFFE) as usize;
                                u16::from_le_bytes([ram[a], ram[a + 1]])
                            }).collect();
                            c.madr = (c.madr + 4 * words as u32) & 0xFF_FFFF;
                            spu2.adma_write(k, &data);
                            self.adma_left[k] -= words as u32;
                        }
                        if self.adma_left[k] == 0 { self.finish(n); }
                    }
                }
                _ => self.finish(n),
            }
        }
        let m = self.dicr_value() >> 31 != 0;
        let rise = m && !self.irq_was;
        self.irq_was = m;
        rise
    }

    /// Auto-DMA audio transfers play out: a channel ends when its samples have
    pub fn tick_spu(&mut self, cycles: u32) {
        for (k, n) in [(0usize, 4usize), (1, 7)] {
            if self.spu_timer[k] > 0 {
                self.spu_timer[k] -= cycles as i64;
                if self.spu_timer[k] <= 0 {
                    self.spu_timer[k] = 0;
                    if self.ch[n].chcr >> 24 & 1 != 0 { self.finish(n); }
                }
            }
        }
    }

    /// The master flag rose since the last check (I_STAT 3), outside `step`
    pub fn irq_rise(&mut self) -> bool {
        let m = self.dicr_value() >> 31 != 0;
        let rise = m && !self.irq_was;
        self.irq_was = m;
        rise
    }

    /// Channel 3 is running (the CDVD drive may write)
    pub fn cdvd_active(&self) -> bool {
        self.ch[CDVD].chcr >> 24 & 1 != 0 && self.enabled(CDVD) && self.dmacen & 1 != 0
    }

    /// Channel 3 has room for an `n`-byte block
    pub fn cdvd_room(&self, n: usize) -> bool { self.cdvd_active() && self.block_words(CDVD) * 4 >= n }

    /// The drive puts a block at MADR: MADR moves on, BCR's count drops by the block's
    /// share; the channel ends when the count runs out (or at once for `last`: the TOC)
    pub fn cdvd_put(&mut self, ram: &mut [u8], data: &[u8], last: bool) {
        let c = &mut self.ch[CDVD];
        for &b in data {
            ram[(c.madr & 0x1F_FFFF) as usize] = b;
            c.madr = (c.madr + 1) & 0xFF_FFFF;
        }
        let bs = (c.bcr & 0xFFFF).max(1) * 4;
        let blocks = (c.bcr >> 16).saturating_sub(data.len() as u32 / bs);
        c.bcr = (c.bcr & 0xFFFF) | blocks << 16;
        if (last || blocks == 0) && self.ch[CDVD].chcr >> 24 & 1 != 0 { self.finish(CDVD); }
    }

    /// Words in a block transfer (BCR: block size × count, 0 counts as 1)
    fn block_words(&self, n: usize) -> usize {
        let bcr = self.ch[n].bcr;
        ((bcr & 0xFFFF).max(1) * (bcr >> 16).max(1)) as usize
    }

    /// SIO2in: IOP memory -> SIO2's FIFOIN (bytes)
    fn sio2_in(&mut self, ram: &[u8], sio2: &mut Sio2) {
        let words = self.block_words(SIO2_IN);
        // each command sits in its own block of BCR's block size
        sio2.dma_block = ((self.ch[SIO2_IN].bcr & 0xFFFF) * 4) as usize;
        let c = &mut self.ch[SIO2_IN];
        for _ in 0..words * 4 {
            sio2.fifo_in.push_back(ram[(c.madr & 0x1F_FFFF) as usize]);
            c.madr = (c.madr + 1) & 0xFF_FFFF;
        }
        self.finish(SIO2_IN);
    }

    /// SIO2out: the replies -> IOP memory, once the transfer has run
    fn sio2_out(&mut self, ram: &mut [u8], sio2: &mut Sio2) {
        if !sio2.done { return; }
        let words = self.block_words(SIO2_OUT);
        let c = &mut self.ch[SIO2_OUT];
        for _ in 0..words * 4 {
            ram[(c.madr & 0x1F_FFFF) as usize] = sio2.fifo_out.pop_front().unwrap_or(0xFF);
            c.madr = (c.madr + 1) & 0xFF_FFFF;
        }
        self.finish(SIO2_OUT);
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
            {
                let (m, n) = ((c.madr & 0x1F_FFFC) as usize, (c.words as usize).min(16));
                let pk: Vec<u32> = (0..n).map(|k| { let a = (m + 4 * k) & 0x1F_FFFC; u32::from_le_bytes(ram[a..a + 4].try_into().unwrap()) }).collect();
                sif.rpc.note(&pk, false);
            }
            if c.chcr >> 8 & 1 != 0 {
                // a quadword for the EE: the DMAtag and the two words after it
                let t = (c.tadr & 0x1F_FFFC) as usize;
                for k in 2..6 {
                    let a = (t + 4 * k) & 0x1F_FFFC;
                    sif.fifo0.push_back(u32::from_le_bytes(ram[a..a + 4].try_into().unwrap()));
                }
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
                if c.words == 0 {
                    if let Some((start, n)) = sif.rpc.blk.take() {
                        let m = (start & 0x1F_FFFC) as usize;
                        let pk: Vec<u32> = (0..(n as usize).min(16)).map(|k| { let a = (m + 4 * k) & 0x1F_FFFC; u32::from_le_bytes(ram[a..a + 4].try_into().unwrap()) }).collect();
                        sif.rpc.note(&pk, true);
                    }
                }
            }
            if c.tag_end { self.finish(SIF1); return; }
            // the tag is a quadword from the EE: the first two words count
            if sif.fifo1.len() < 4 { return; }
            let t0 = sif.fifo1.pop_front().unwrap();
            let t1 = sif.fifo1.pop_front().unwrap();
            sif.fifo1.pop_front();
            sif.fifo1.pop_front();
            c.madr = t0 & 0xFF_FFFF;
            c.words = ((t1 & 0xFF_FFFF) + 3) & !3;
            c.tag_end = t0 >> 30 & 3 != 0;
            if c.words > 0 { sif.rpc.blk = Some((c.madr, c.words)); }
        }
    }
}
