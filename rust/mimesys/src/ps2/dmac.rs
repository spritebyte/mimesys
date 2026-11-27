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
    /// MFIFO drain: the data being sent lies in the ring buffer (wraps there)
    #[serde(default)]
    ring_data: bool,
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
    /// debugging (PS2_GS_XFER): EE source address of each `gif_out` quadword
    #[serde(skip)]
    pub gif_src: Vec<u32>,
    /// words for VIF0's / VIF1's command streams
    #[serde(skip)]
    pub vif0_out: Vec<u32>,
    #[serde(skip)]
    pub vif1_out: Vec<u32>,
    /// debugging (PS2_VU1_SRC): the EE address each vif1_out word was read from (MAX: a tag)
    pub vif1_src: Vec<u32>,
    /// debugging: transfers finished per channel
    #[serde(skip)]
    pub done: [u64; 10],
    /// MFIFO: the drain channel caught up with SPR_FROM (D_STAT MEIS raised once)
    #[serde(default)]
    mfifo_empty: bool,
    /// PATH3 masked (GIF_MODE M3R, VIF1 MSKPATH3) or paused (GIF_CTRL PSE): the GIF
    /// channel waits
    #[serde(default)]
    pub gif_hold: bool,
    /// PATH3 just unmasked by the VIF: send the GIF channel's data up to the end of one
    /// GS packet (EOP) even if it's masked again, then stop
    #[serde(default)]
    pub gif_one_packet: bool,
    /// GIFtag parsing of what the channel sends: quadwords left in the current tag's data,
    /// that tag's EOP, and "a packet just ended" (stop for `gif_one_packet`)
    #[serde(default)]
    gif_left: u32,
    #[serde(default)]
    gif_eop: bool,
    #[serde(default)]
    gif_stop: bool,
}

impl Default for Dmac {
    fn default() -> Self {
        Self { ch: Default::default(), ctrl: 0, stat: 0, pcr: 0, sqwc: 0, rbsr: 0, rbor: 0, stadr: 0,
            enable: 0x1201, gif_out: Vec::new(), gif_src: Vec::new(), vif0_out: Vec::new(), vif1_out: Vec::new(), vif1_src: Vec::new(), done: [0; 10], mfifo_empty: false, gif_hold: false, gif_one_packet: false, gif_left: 0, gif_eop: false, gif_stop: false }
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
        if crate::common::cpu::r5900::watch_hit(o as u32, 16) && crate::common::cpu::r5900::watch_note() {
            crate::common::cpu::r5900::watch_log(format!("DMA write [{:08X}] = {:032X}", o, v));
            if crate::common::cpu::r5900::tl_on() { crate::common::cpu::r5900::tl(&format!("DMA write [{:08X}] = {:032X}", o, v)); }
        }
        crate::common::cpu::r5900::wr_note(o as u32, 0, 0, crate::common::cpu::r5900::WR_DMA);
        crate::common::cpu::r5900::jit::note_write(o as u32);
        ram[o..o + 16].copy_from_slice(&v.to_le_bytes());
    }
}

const CH_NAMES: [&str; 10] = ["VIF0", "VIF1", "GIF", "IPU_FROM", "IPU_TO", "SIF0", "SIF1", "SIF2", "SPR_FROM", "SPR_TO"];

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
                let was = self.stat;
                self.stat &= !(v & 0xE3FF);
                self.stat ^= v & 0x63FF_0000;
                if crate::common::cpu::r5900::tl_on() {
                    let pc = crate::common::cpu::r5900::NOW_PC.load(std::sync::atomic::Ordering::Relaxed);
                    crate::common::cpu::r5900::tl(&format!("EE writes D_STAT {v:08X} (pc {pc:08X}): {was:08X} -> {:08X}", self.stat));
                }
            }
            0xE020 => self.pcr = v,
            0xE030 => self.sqwc = v,
            0xE040 => self.rbsr = v,
            0xE050 => self.rbor = v,
            0xE060 => self.stadr = v,
            _ => if let Some(n) = BASES.iter().position(|&b| off & !0xFF == b) {
                if off & 0xF0 <= 0x30 && crate::common::cpu::r5900::tl_on() {
                    let reg = ["CHCR", "MADR", "QWC", "TADR"][(off >> 4 & 3) as usize];
                    let pc = crate::common::cpu::r5900::NOW_PC.load(std::sync::atomic::Ordering::Relaxed);
                    crate::common::cpu::r5900::tl(&format!("EE writes {} {reg} = {:08X} (pc {pc:08X}; CHCR was {:08X})",
                        CH_NAMES[n], v & mask, self.ch[n].chcr));
                }
                let c = &mut self.ch[n];
                let merge = |old: u32| (old & !mask) | (v & mask);
                match off & 0xF0 {
                    0x00 => {
                        let was = c.chcr & STR;
                        c.chcr = merge(c.chcr);
                        if was == 0 && c.chcr & STR != 0 {
                            crate::common::cpu::r5900::request_slice_break();
                            // a chain resuming with QWC left: the TAG field's ID decides
                            let id = c.chcr >> 28 & 7;
                            c.tag_end = c.chcr >> 2 & 1 == 1 && c.qwc > 0 && (id == 0 || id == 7);
                            if c.chcr >> 2 & 1 == 1 && c.qwc == 0 { c.tag_end = false; }
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
        if crate::common::cpu::r5900::tl_on() {
            crate::common::cpu::r5900::tl(&format!("DMA {} ends: D_STAT {:08X}", CH_NAMES[n], self.stat));
        }
        self.done[n] += 1;
    }

    /// VIF1 running towards memory (CHCR DIR 0): the GS's local -> host data
    pub fn vif1_from_gs(&mut self, ram: &mut [u8], spr: &mut [u8], src: &mut std::collections::VecDeque<u128>) {
        if self.ctrl & 1 == 0 || self.enable & 0x1_0000 != 0 { return; }
        let c = &mut self.ch[VIF1];
        if c.chcr & STR == 0 || c.chcr & 1 != 0 { return; }
        // (it waits for the GS's data, as the hardware does: the GS may get the
        // transfer's TRXDIR after the channel starts)
        while c.qwc > 0 {
            let Some(q) = src.pop_front() else { return };
            mem_write(ram, spr, c.madr, q);
            c.madr = c.madr.wrapping_add(16);
            c.qwc -= 1;
        }
        self.finish(VIF1);
    }

    /// Move data on every running channel
    pub fn step(&mut self, ram: &mut [u8], spr: &mut [u8], sif: &mut Sif) {
        if self.ctrl & 1 == 0 || self.enable & 0x1_0000 != 0 { return; }
        for n in 0..10 {
            if self.ch[n].chcr & STR == 0 { continue; }
            match n {
                SIF0 => self.sif0(ram, spr, sif),
                SPR_FROM => self.spr_from(ram, spr),
                IPU_FROM | IPU_TO | SIF2 => {}
                VIF1 if self.ch[n].chcr & 1 == 0 => {}
                GIF if self.gif_hold && !self.gif_one_packet => {}
                VIF1 | GIF if self.mfifo_drain() == Some(n) && self.ch[n].chcr >> 2 & 1 == 1 => self.mfifo(n, ram, spr, sif),
                _ => self.source(n, ram, spr, sif),
            }
        }
    }

    /// A channel reading memory (normal or source chain) into its device
    fn source(&mut self, n: usize, ram: &mut [u8], spr: &mut [u8], sif: &mut Sif) {
        // MOD 1 is chain; the undefined MOD 3 also runs as chain (FF12 starts its MFIFO
        // drain, VIF1, with CHCR 14D: as a normal transfer it ended at once with QWC 0)
        let chain = self.ch[n].chcr >> 2 & 1 == 1;
        // interleave (SPR_TO, MOD 2): TQWC quadwords, then skip SQWC in memory
        let inter = n == SPR_TO && self.ch[n].chcr >> 2 & 3 == 2;
        let (tqwc, sqwc) = (self.sqwc >> 16 & 0xFF, self.sqwc & 0xFF);
        let mut run = 0;
        let mut guard = 0;
        loop {
            // the data
            while self.ch[n].qwc > 0 {
                let a = self.ch[n].madr;
                let q = mem_read(ram, spr, a);
                if (n == VIF1 || n == GIF) && crate::common::cpu::r5900::tl_on()
                    && crate::common::cpu::r5900::watch_hit(a & 0x1FFF_FFFF, 16) {
                    crate::common::cpu::r5900::tl(&format!("{} DMA reads [{a:08X}] = {q:032X}", if n == VIF1 { "VIF1" } else { "GIF" }));
                }
                self.sink(n, q, ram, spr, sif);
                let c = &mut self.ch[n];
                c.madr = c.madr.wrapping_add(16);
                c.qwc -= 1;
                if inter {
                    run += 1;
                    if run == tqwc { run = 0; c.madr = c.madr.wrapping_add(sqwc * 16); }
                }
                guard += 1;
                if guard > 0x10_0000 { return; }
                // one PATH3 packet sent while masked: stop here (unless that was the end)
                if self.gif_stop && !(self.ch[n].qwc == 0 && (!chain || self.ch[n].tag_end)) {
                    self.gif_stop = false;
                    return;
                }
            }
            self.gif_stop = false;
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
            if (n == VIF1 || n == GIF) && crate::common::cpu::r5900::tl_on() {
                let ids = ["refe", "cnt", "next", "ref", "refs", "call", "ret", "end"];
                crate::common::cpu::r5900::tl(&format!("{} tag at {tadr:08X}: {} addr {addr:08X} qwc {} | {:016X}",
                    if n == VIF1 { "VIF1" } else { "GIF" }, ids[id as usize], lo & 0xFFFF, (tag >> 64) as u64));
            }
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
                if n == SPR_TO {
                    // the whole tag quadword lands in the scratchpad before its data (GTA3
                    // lays objects out after their tags and patches the tag IDs later)
                    self.sink(n, tag, ram, spr, sif);
                } else {
                    let hi = (tag >> 64) as u64;
                    self.sink_tag(n, hi, sif);
                }
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
            GIF => {
                self.gif_out.push(q);
                if crate::common::cpu::r5900::src_track() { self.gif_src.push(self.ch[GIF].madr); }
                // follow the GIFtags to see where a packet ends
                let ended = if self.gif_left == 0 {
                    let lo = q as u64;
                    let (nloop, flg) = ((lo & 0x7FFF) as u32, (lo >> 58 & 3) as u32);
                    let nreg = match (lo >> 60) as u32 { 0 => 16, n => n };
                    self.gif_left = match flg { 0 => nloop * nreg, 1 => (nloop * nreg).div_ceil(2), _ => nloop };
                    self.gif_eop = lo >> 15 & 1 != 0;
                    self.gif_left == 0 && self.gif_eop
                } else {
                    self.gif_left -= 1;
                    self.gif_left == 0 && self.gif_eop
                };
                if ended && self.gif_one_packet {
                    self.gif_one_packet = false;
                    self.gif_stop = true;
                }
            }
            VIF0 => for k in 0..4 { self.vif0_out.push((q >> (32 * k)) as u32); },
            VIF1 => {
                for k in 0..4 { self.vif1_out.push((q >> (32 * k)) as u32); }
                if crate::common::cpu::r5900::src_track() {
                    let a = self.ch[VIF1].madr;
                    self.vif1_src.extend([a, a + 4, a + 8, a + 12]);
                }
            }
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
        if n == VIF1 {
            self.vif1_out.extend([hi as u32, (hi >> 32) as u32]);
            if crate::common::cpu::r5900::src_track() { self.vif1_src.extend([u32::MAX, u32::MAX]); }
        }
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

    /// SPR_FROM (normal or interleave mode): scratchpad -> memory
    fn spr_from(&mut self, ram: &mut [u8], spr: &mut [u8]) {
        let mfifo = self.mfifo_drain().is_some();
        let (rbor, rbsr) = (self.rbor, self.rbsr);
        // interleave (MOD 2): TQWC quadwords, then skip SQWC in memory (D_SQWC); the
        // scratchpad side is contiguous. MGS2's movies turn 16x16 macroblocks into lines so
        let inter = self.ch[SPR_FROM].chcr >> 2 & 3 == 2;
        let (tqwc, sqwc) = (self.sqwc >> 16 & 0xFF, self.sqwc & 0xFF);
        let mut run = 0;
        let c = &mut self.ch[SPR_FROM];
        while c.qwc > 0 {
            let q = mem_read(ram, spr, 0x8000_0000 | c.sadr);
            // MFIFO: SPR_FROM fills the ring buffer (RBOR + offset & RBSR)
            if mfifo { c.madr = rbor | (c.madr & rbsr); }
            mem_write(ram, spr, c.madr & 0x7FFF_FFFF, q);
            // PS2_VU1_SRC: the copy keeps the scratchpad quadword's writer
            if crate::common::cpu::r5900::src_track() {
                let w = crate::common::cpu::r5900::wr_get(0x8000_0000 | c.sadr);
                crate::common::cpu::r5900::wr_set_raw(c.madr & 0x01FF_FFF0, w & !3 | crate::common::cpu::r5900::WR_SPR_FROM);
            }
            c.sadr = (c.sadr + 16) & 0x3FF0;
            c.madr = c.madr.wrapping_add(16);
            if mfifo { c.madr = rbor | (c.madr & rbsr); }
            c.qwc -= 1;
            if inter {
                run += 1;
                if run == tqwc { run = 0; c.madr = c.madr.wrapping_add(sqwc * 16); }
            }
        }
        if mfifo { self.mfifo_empty = false; }
        self.finish(SPR_FROM);
    }

    /// IPU_TO (channel 4): the next quadword for the IPU's input FIFO (normal or source
    /// chain); the channel ends as soon as its last quadword goes
    pub fn ipu_to_pull(&mut self, ram: &mut [u8], spr: &mut [u8]) -> Option<u128> {
        if self.ctrl & 1 == 0 || self.enable & 0x1_0000 != 0 { return None; }
        let n = IPU_TO;
        if self.ch[n].chcr & STR == 0 { return None; }
        let chain = self.ch[n].chcr >> 2 & 1 == 1;
        for _ in 0..64 {
            if self.ch[n].qwc > 0 {
                let a = self.ch[n].madr;
                let q = mem_read(ram, spr, a);
                let c = &mut self.ch[n];
                c.madr = c.madr.wrapping_add(16);
                c.qwc -= 1;
                if c.qwc == 0 && (!chain || c.tag_end) { self.finish(n); }
                return Some(q);
            }
            if !chain || self.ch[n].tag_end { self.finish(n); return None; }
            self.load_tag(n, ram, spr);
        }
        None
    }

    /// A source chain's next tag (no TTE: the IPU's channel)
    fn load_tag(&mut self, n: usize, ram: &mut [u8], spr: &mut [u8]) {
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
            0 => { c.madr = addr; c.tadr = tadr + 16; end = true; }
            1 => { c.madr = tadr + 16; c.tadr = c.madr + c.qwc * 16; }
            2 => { c.madr = tadr + 16; c.tadr = addr; }
            3 | 4 => { c.madr = addr; c.tadr = tadr + 16; }
            5 => {
                c.madr = tadr + 16;
                if asp < 2 { c.asr[asp as usize] = c.madr + c.qwc * 16; }
                c.tadr = addr;
                c.chcr = (c.chcr & !0x30) | ((asp + 1).min(2) << 4);
            }
            6 => {
                c.madr = tadr + 16;
                if asp > 0 {
                    c.tadr = c.asr[asp as usize - 1];
                    c.chcr = (c.chcr & !0x30) | ((asp - 1) << 4);
                } else {
                    end = true;
                }
            }
            _ => { c.madr = tadr + 16; end = true; }
        }
        if irq && c.chcr & (1 << 7) != 0 { end = true; }
        c.tag_end = end;
    }

    /// IPU_FROM (channel 3, normal mode): one quadword of the IPU's output to memory;
    /// false when the channel isn't taking any
    pub fn ipu_from_push(&mut self, ram: &mut [u8], spr: &mut [u8], q: u128) -> bool {
        if self.ctrl & 1 == 0 || self.enable & 0x1_0000 != 0 { return false; }
        let n = IPU_FROM;
        let c = &mut self.ch[n];
        if c.chcr & STR == 0 || c.qwc == 0 { return false; }
        mem_write(ram, spr, c.madr, q);
        c.madr = c.madr.wrapping_add(16);
        c.qwc -= 1;
        if c.qwc == 0 { self.finish(n); }
        true
    }

    /// D_CTRL MFD: 2 = VIF1 drains the MFIFO, 3 = the GIF
    fn mfifo_drain(&self) -> Option<usize> {
        match self.ctrl >> 2 & 3 { 2 => Some(VIF1), 3 => Some(GIF), _ => None }
    }

    /// An address in the ring buffer
    fn ring(&self, a: u32) -> u32 { self.rbor | (a & self.rbsr) }

    /// The MFIFO drain (VIF1 or GIF in source chain mode): tags come from the ring buffer
    /// at TADR (wrapping), up to where SPR_FROM has written (its MADR); CNT / NEXT / END
    /// data follows the tag in the ring, REF / REFS / REFE data is read where it lies.
    /// Caught up: the channel waits (STR stays set), D_STAT MEIS once.
    fn mfifo(&mut self, n: usize, ram: &mut [u8], spr: &mut [u8], sif: &mut Sif) {
        let mut guard = 0;
        loop {
            while self.ch[n].qwc > 0 {
                let (a, rd) = (self.ch[n].madr, self.ch[n].ring_data);
                let q = mem_read(ram, spr, a);
                self.sink(n, q, ram, spr, sif);
                let next = a.wrapping_add(16);
                let c_madr = if rd { self.ring(next) } else { next };
                let c = &mut self.ch[n];
                c.madr = c_madr;
                c.qwc -= 1;
                guard += 1;
                if guard > 0x10_0000 { return; }
            }
            if self.ch[n].tag_end { self.finish(n); return; }
            let tadr = self.ring(self.ch[n].tadr);
            if tadr == self.ring(self.ch[SPR_FROM].madr) {
                self.ch[n].tadr = tadr;
                if !self.mfifo_empty { self.mfifo_empty = true; self.stat |= 1 << 14; }
                return;
            }
            let tag = mem_read(ram, spr, tadr);
            let lo = tag as u64;
            let qwc = (lo & 0xFFFF) as u32;
            let id = (lo >> 28 & 7) as u32;
            let irq = lo >> 31 & 1 != 0;
            let addr = (lo >> 32) as u32 & 0xFFFF_FFF0;
            let after = self.ring(tadr + 16);
            let after_data = self.ring(tadr + 16 + qwc * 16);
            let c = &mut self.ch[n];
            c.chcr = (c.chcr & 0xFFFF) | ((lo as u32) & 0xFFFF_0000);
            c.qwc = qwc;
            let mut end = false;
            match id {
                0 => { c.madr = addr; c.ring_data = false; c.tadr = after; end = true; }   // refe
                1 => { c.madr = after; c.ring_data = true; c.tadr = after_data; }          // cnt
                2 => { c.madr = after; c.ring_data = true; c.tadr = addr; }                // next
                3 | 4 => { c.madr = addr; c.ring_data = false; c.tadr = after; }           // ref / refs
                7 => { c.madr = after; c.ring_data = true; c.tadr = after_data; end = true; } // end
                _ => { c.madr = after; c.ring_data = true; c.tadr = after_data; }          // call / ret: as cnt
            }
            if irq && c.chcr & (1 << 7) != 0 { end = true; }
            c.tag_end = end;
            if self.ch[n].chcr & (1 << 6) != 0 {
                let hi = (tag >> 64) as u64;
                self.sink_tag(n, hi, sif);
            }
            guard += 1;
            if guard > 0x10_0000 { return; }
        }
    }
}
