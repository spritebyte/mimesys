//! The EE's DMA controller: ten channels (VIF0, VIF1, GIF, IPU_FROM, IPU_TO, SIF0, SIF1,
//! SIF2, SPR_FROM, SPR_TO), normal and chain modes, D_STAT interrupts (INT1), CPCOND0.
//! From ps2tek's DMAC pages.
//!
//! Transfers run as soon as the channel starts (no bus timing yet), except the SIF0 one,
//! which waits for the IOP to fill its FIFO. IPU data is dropped until it exists
//! (a later stage); VIF0's / VIF1's words go to `vif0_out` / `vif1_out`; `gif_out` collects GIF quadwords for the GS (stage 3).
//!
//! SIF tags: in both directions the tag crossing the FIFO is a whole quadword, 4 words
//! (SIF1: the EE DMAtag's bits 64-127 with TTE, then 2 padding words, read by the IOP as
//! its tag; SIF0: the 4 words the IOP sends from TADR + 8, read here as the EE DMAtag,
//! low 64 bits). Two-word tags left the Sony BIOS's first SIF1 packet two words out.

use serde::{Deserialize, Serialize};

use super::hw::Sif;

pub const VIF0: usize = 0;
pub const VIF1: usize = 1;
pub const GIF: usize = 2;
pub const IPU_FROM: usize = 3;
pub const IPU_TO: usize = 4;
pub const SIF0: usize = 5;
pub const SIF1: usize = 6;
pub const SIF2: usize = 7;
pub const SPR_FROM: usize = 8;
pub const SPR_TO: usize = 9;

/// Each channel's register block (offset from 1000_0000)
const BASES: [u32; 10] = [0x8000, 0x9000, 0xA000, 0xB000, 0xB400, 0xC000, 0xC400, 0xC800, 0xD000, 0xD400];

const STR: u32 = 1 << 8;

#[derive(Default, Clone, Serialize, Deserialize)]
pub struct Channel {
    pub chcr: u32,
    pub madr: u32,
    pub qwc: u32,
    pub tadr: u32,
    pub asr: [u32; 2],
    pub sadr: u32,
    /// chain mode: the last tag ends the transfer once QWC is done
    tag_end: bool,
}

#[derive(Serialize, Deserialize)]
pub struct Dmac {
    pub ch: [Channel; 10],
    pub ctrl: u32,
    pub stat: u32,
    pub pcr: u32,
    pub sqwc: u32,
    pub rbsr: u32,
    pub rbor: u32,
    pub stadr: u32,
    /// D_ENABLER / D_ENABLEW (bit 16: everything held)
    pub enable: u32,
    /// GIF PATH3 quadwords for the GS
    #[serde(skip)]
    pub gif_out: Vec<u128>,
    /// words for VIF0's / VIF1's command streams
    #[serde(skip)]
    pub vif0_out: Vec<u32>,
    #[serde(skip)]
    pub vif1_out: Vec<u32>,
    /// debugging: transfers finished per channel
    #[serde(skip)]
    pub done: [u64; 10],
}

impl Default for Dmac {
    fn default() -> Self {
        Self { ch: Default::default(), ctrl: 0, stat: 0, pcr: 0, sqwc: 0, rbsr: 0, rbor: 0, stadr: 0,
            enable: 0x1201, gif_out: Vec::new(), vif0_out: Vec::new(), vif1_out: Vec::new(), done: [0; 10] }
    }
}

/// RAM or (bit 31) the scratchpad
fn mem_read(ram: &[u8], spr: &[u8], a: u32) -> u128 {
    if a & 0x8000_0000 != 0 {
        let o = (a & 0x3FF0) as usize;
        u128::from_le_bytes(spr[o..o + 16].try_into().unwrap())
    } else {
        let o = (a & 0x01FF_FFF0) as usize;
        u128::from_le_bytes(ram[o..o + 16].try_into().unwrap())
    }
}

fn mem_write(ram: &mut [u8], spr: &mut [u8], a: u32, v: u128) {
    if a & 0x8000_0000 != 0 {
        let o = (a & 0x3FF0) as usize;
        spr[o..o + 16].copy_from_slice(&v.to_le_bytes());
    } else {
        let o = (a & 0x01FF_FFF0) as usize;
        ram[o..o + 16].copy_from_slice(&v.to_le_bytes());
    }
}

impl Dmac {
    pub fn read(&self, off: u32) -> u32 {
        match off {
            0xE000 => self.ctrl,
            0xE010 => self.stat,
            0xE020 => self.pcr,
            0xE030 => self.sqwc,
            0xE040 => self.rbsr,
            0xE050 => self.rbor,
            0xE060 => self.stadr,
            _ => match BASES.iter().position(|&b| off & !0xFF == b) {
                Some(n) => {
                    let c = &self.ch[n];
                    match off & 0xF0 {
                        0x00 => c.chcr,
                        0x10 => c.madr,
                        0x20 => c.qwc,
                        0x30 => c.tadr,
                        0x40 => c.asr[0],
                        0x50 => c.asr[1],
                        0x80 => c.sadr,
                        _ => 0,
                    }
                }
                None => 0,
            },
        }
    }

    pub fn write(&mut self, off: u32, v: u32, mask: u32) {
        match off {
            0xE000 => self.ctrl = v,
            0xE010 => {
                // status bits clear on 1, mask bits toggle on 1
                let v = v & mask;
                self.stat &= !(v & 0xE3FF);
                self.stat ^= v & 0x63FF_0000;
            }
            0xE020 => self.pcr = v,
            0xE030 => self.sqwc = v,
            0xE040 => self.rbsr = v,
            0xE050 => self.rbor = v,
            0xE060 => self.stadr = v,
            _ => if let Some(n) = BASES.iter().position(|&b| off & !0xFF == b) {
                let c = &mut self.ch[n];
                let merge = |old: u32| (old & !mask) | (v & mask);
                match off & 0xF0 {
                    0x00 => {
                        let was = c.chcr & STR;
                        c.chcr = merge(c.chcr);
                        if was == 0 && c.chcr & STR != 0 {
                            // a chain resuming with QWC left: the TAG field's ID decides
                            let id = c.chcr >> 28 & 7;
                            c.tag_end = c.chcr >> 2 & 3 == 1 && c.qwc > 0 && (id == 0 || id == 7);
                            if c.chcr >> 2 & 3 == 1 && c.qwc == 0 { c.tag_end = false; }
                        }
                    }
                    0x10 => c.madr = merge(c.madr) & !0xF,
                    0x20 => c.qwc = merge(c.qwc) & 0xFFFF,
                    0x30 => c.tadr = merge(c.tadr) & !0xF,
                    0x40 => c.asr[0] = merge(c.asr[0]) & !0xF,
                    0x50 => c.asr[1] = merge(c.asr[1]) & !0xF,
                    0x80 => c.sadr = merge(c.sadr) & 0x3FF0,
                    _ => {}
                }
            },
        }
    }

    /// INT1 for the EE
    pub fn int1(&self) -> bool {
        let s = self.stat;
        (s & 0x3FF & (s >> 16)) != 0 || (s >> 13 & 1 != 0 && s >> 29 & 1 != 0) || (s >> 14 & 1 != 0 && s >> 30 & 1 != 0) || s >> 15 & 1 != 0
    }

    /// BC0F / BC0T: every channel picked in D_PCR has finished
    pub fn cpcond0(&self) -> bool { (!self.pcr | self.stat) & 0x3FF == 0x3FF }

    fn finish(&mut self, n: usize) {
        self.ch[n].chcr &= !STR;
        self.stat |= 1 << n;
        self.done[n] += 1;
    }

    /// Move data on every running channel
    pub fn step(&mut self, ram: &mut [u8], spr: &mut [u8], sif: &mut Sif) {
        if self.ctrl & 1 == 0 || self.enable & 0x1_0000 != 0 { return; }
        for n in 0..10 {
            if self.ch[n].chcr & STR == 0 { continue; }
            match n {
                SIF0 => self.sif0(ram, spr, sif),
                SPR_FROM => self.spr_from(ram, spr),
                IPU_FROM | SIF2 => {}
                VIF1 if self.ch[n].chcr & 1 == 0 => {}
                _ => self.source(n, ram, spr, sif),
            }
        }
    }

    /// A channel reading memory (normal or source chain) into its device
    fn source(&mut self, n: usize, ram: &mut [u8], spr: &mut [u8], sif: &mut Sif) {
        let chain = self.ch[n].chcr >> 2 & 3 == 1;
        let mut guard = 0;
        loop {
            // the data
            while self.ch[n].qwc > 0 {
                let a = self.ch[n].madr;
                let q = mem_read(ram, spr, a);
                self.sink(n, q, ram, spr, sif);
                let c = &mut self.ch[n];
                c.madr = c.madr.wrapping_add(16);
                c.qwc -= 1;
                guard += 1;
                if guard > 0x10_0000 { return; }
            }
            if !chain || self.ch[n].tag_end { self.finish(n); return; }
            // the next tag
            let c = &mut self.ch[n];
            let tag = mem_read(ram, spr, c.tadr);
            let lo = tag as u64;
            c.chcr = (c.chcr & 0xFFFF) | ((lo as u32) & 0xFFFF_0000);
            c.qwc = (lo & 0xFFFF) as u32;
            let id = (lo >> 28 & 7) as u32;
            let irq = lo >> 31 & 1 != 0;
            let addr = (lo >> 32) as u32 & 0xFFFF_FFF0;
            let tadr = c.tadr;
            let asp = c.chcr >> 4 & 3;
            let mut end = false;
            match id {
                0 => { c.madr = addr; c.tadr = tadr + 16; end = true; }                  // refe
                1 => { c.madr = tadr + 16; c.tadr = c.madr + c.qwc * 16; }               // cnt
                2 => { c.madr = tadr + 16; c.tadr = addr; }                              // next
                3 | 4 => { c.madr = addr; c.tadr = tadr + 16; }                          // ref / refs
                5 => {                                                                    // call
                    c.madr = tadr + 16;
                    if asp < 2 { c.asr[asp as usize] = c.madr + c.qwc * 16; }
                    c.tadr = addr;
                    c.chcr = (c.chcr & !0x30) | ((asp + 1).min(2) << 4);
                }
                6 => {                                                                    // ret
                    c.madr = tadr + 16;
                    if asp > 0 {
                        c.tadr = c.asr[asp as usize - 1];
                        c.chcr = (c.chcr & !0x30) | ((asp - 1) << 4);
                    } else {
                        end = true;
                    }
                }
                _ => { c.madr = tadr + 16; end = true; }                                 // end
            }
            if irq && c.chcr & (1 << 7) != 0 { end = true; }
            c.tag_end = end;
            // TTE: the tag's upper 64 bits go first
            if self.ch[n].chcr & (1 << 6) != 0 {
                let hi = (tag >> 64) as u64;
                self.sink_tag(n, hi, sif);
            }
            guard += 1;
            if guard > 0x10_0000 { return; }
        }
    }

    fn sink(&mut self, n: usize, q: u128, ram: &mut [u8], spr: &mut [u8], sif: &mut Sif) {
        match n {
            SIF1 => for k in 0..4 { sif.fifo1.push_back((q >> (32 * k)) as u32); },
            SPR_TO => {
                let c = &mut self.ch[n];
                mem_write(ram, spr, 0x8000_0000 | c.sadr, q);
                c.sadr = (c.sadr + 16) & 0x3FF0;
            }
            GIF => self.gif_out.push(q),
            VIF0 => for k in 0..4 { self.vif0_out.push((q >> (32 * k)) as u32); },
            VIF1 => for k in 0..4 { self.vif1_out.push((q >> (32 * k)) as u32); },
            _ => {}
        }
    }

    fn sink_tag(&mut self, n: usize, hi: u64, sif: &mut Sif) {
        // the SIF FIFO is a quadword wide on the EE's side: the tag's upper half travels as
        // a whole quadword (the IOP reads 4 words, using the first two)
        if n == SIF1 {
            sif.fifo1.extend([hi as u32, (hi >> 32) as u32, 0, 0]);
        }
        // VIF: the tag's upper two words are VIF codes
        if n == VIF0 { self.vif0_out.extend([hi as u32, (hi >> 32) as u32]); }
        if n == VIF1 { self.vif1_out.extend([hi as u32, (hi >> 32) as u32]); }
    }

    /// SIF0: destination chain, tags and data from the IOP through the FIFO
    fn sif0(&mut self, ram: &mut [u8], spr: &mut [u8], sif: &mut Sif) {
        loop {
            while self.ch[SIF0].qwc > 0 {
                if sif.fifo0.len() < 4 { return; }
                let mut q = 0u128;
                for k in 0..4 { q |= (sif.fifo0.pop_front().unwrap() as u128) << (32 * k); }
                let c = &mut self.ch[SIF0];
                mem_write(ram, spr, c.madr, q);
                c.madr = c.madr.wrapping_add(16);
                c.qwc -= 1;
            }
            if self.ch[SIF0].tag_end { self.finish(SIF0); return; }
            // the tag is a whole quadword; its low 64 bits are the DMAtag
            if sif.fifo0.len() < 4 { return; }
            let lo = sif.fifo0.pop_front().unwrap() as u64 | (sif.fifo0.pop_front().unwrap() as u64) << 32;
            sif.fifo0.pop_front();
            sif.fifo0.pop_front();
            let c = &mut self.ch[SIF0];
            c.chcr = (c.chcr & 0xFFFF) | ((lo as u32) & 0xFFFF_0000);
            c.qwc = (lo & 0xFFFF) as u32;
            c.madr = (lo >> 32) as u32 & 0xFFFF_FFF0;
            let id = lo >> 28 & 7;
            let irq = lo >> 31 & 1 != 0;
            c.tag_end = id == 7 || (irq && c.chcr & (1 << 7) != 0);
        }
    }

    /// SPR_FROM (normal mode): scratchpad -> memory
    fn spr_from(&mut self, ram: &mut [u8], spr: &mut [u8]) {
        let c = &mut self.ch[SPR_FROM];
        while c.qwc > 0 {
            let q = mem_read(ram, spr, 0x8000_0000 | c.sadr);
            mem_write(ram, spr, c.madr & 0x7FFF_FFFF, q);
            c.sadr = (c.sadr + 16) & 0x3FF0;
            c.madr = c.madr.wrapping_add(16);
            c.qwc -= 1;
        }
        self.finish(SPR_FROM);
    }
}
