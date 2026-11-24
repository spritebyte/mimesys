//! SIO2: the IOP's controller / memory-card port (1F80_8200). The IOP queues up to 16
//! commands (SEND3: port in bit 0, length in bits 8-17), feeds the bytes in (FIFOIN or
//! DMA channel 11), sets CTRL bit 0, waits for the SIO2 interrupt (I_STAT bit 17), then
//! reads RECV1-3 and the reply bytes (FIFOOUT or DMA channel 12). From ps2tek.
//!
//! A DualShock 2 sits on each controller port (peripheral byte 01h, ports 0 / 1): the
//! PS1's `Pad` with its `ds2` extras answers byte by byte, and RECV1 says connected
//! (1100h). Memory cards (81h) answer through `memcard::MemCard` on port 0 (slot 1; slot
//! 2 is empty). Anything else (multitap 21h, infrared 61h) isn't there: FFh bytes, RECV1
//! 1D100h.
//!
//! Through DMA (channel 11) each command fills a whole DMA block (BCR's block size: MCMAN
//! uses 90h bytes) whatever its length, and each reply is padded to one too, so several
//! commands can go in one transfer (a memory card page read: set sector, 4 × read 128,
//! read ECC, end). Written straight to FIFOIN, commands follow each other.
//!
//! The transfer runs after the IOP's DMA has had its turn (`pending` -> `run`), since the
//! SIO2MAN starts the DMA before it sets CTRL bit 0.

use serde::{Deserialize, Serialize};

use crate::ps1::pad::Pad;
use super::memcard::MemCard;
use std::collections::VecDeque;

fn ds2_pads() -> [Pad; 2] { std::array::from_fn(|_| { let mut p = Pad::default(); p.ds2 = true; p }) }

#[derive(Serialize, Deserialize)]
pub struct Sio2 {
    #[serde(default = "ds2_pads")]
    pub pads: [Pad; 2],
    /// bytes per command block when the commands came by DMA (0: FIFOIN writes)
    #[serde(default)]
    pub dma_block: usize,
    /// memory card slots 1 and 2
    #[serde(default)]
    pub cards: [Option<MemCard>; 2],
    /// frames a swapped card stays out (so the BIOS / game sees it change)
    #[serde(default)]
    pub card_out: u32,
    pub send3: [u32; 16],
    pub send12: [u32; 8],
    pub ctrl: u32,
    pub recv1: u32,
    pub istat: u32,
    pub fifo_in: VecDeque<u8>,
    pub fifo_out: VecDeque<u8>,
    /// CTRL bit 0 was set: run once the DMA has moved the bytes in
    pub pending: bool,
    /// the last transfer finished (DMA channel 12 may read the replies)
    pub done: bool,
    /// debugging: transfers run, and the last commands that weren't pad reads
    /// (SEND3, first 3 bytes, length, RECV1, reply bytes 2-5)
    #[serde(skip)]
    pub transfers: u64,
    #[serde(skip)]
    pub log: VecDeque<(u32, [u8; 3], usize, u32, [u8; 4])>,
    /// debugging: memory card commands seen, by command byte
    #[serde(skip)]
    pub card_counts: std::collections::BTreeMap<u8, u64>,
}

impl Default for Sio2 {
    fn default() -> Self {
        Self {
            pads: ds2_pads(), dma_block: 0, cards: [None, None], card_out: 0, send3: [0; 16], send12: [0; 8], ctrl: 0, recv1: 0, istat: 0,
            fifo_in: VecDeque::new(), fifo_out: VecDeque::new(), pending: false, done: false, transfers: 0, log: VecDeque::new(), card_counts: Default::default(),
        }
    }
}

impl Sio2 {
    pub fn read(&mut self, a: u32) -> u32 {
        match a & 0xFF {
            o @ 0x00..=0x3F => self.send3[(o >> 2) as usize],
            o @ 0x40..=0x5F => self.send12[((o - 0x40) >> 2) as usize],
            0x64 => self.fifo_out.pop_front().unwrap_or(0xFF) as u32,
            0x68 => self.ctrl,
            0x6C => self.recv1,
            0x70 => 0xF,
            0x74 => 0,
            0x80 => self.istat,
            _ => 0,
        }
    }

    pub fn write(&mut self, a: u32, v: u32) {
        match a & 0xFF {
            o @ 0x00..=0x3F => self.send3[(o >> 2) as usize] = v,
            o @ 0x40..=0x5F => self.send12[((o - 0x40) >> 2) as usize] = v,
            0x60 => { self.fifo_in.push_back(v as u8); self.dma_block = 0; }
            0x68 => {
                // bits 2-3 reset, but not on the write that starts a transfer (SIO2MAN
                // writes 3BDh: the bytes it queued must stay)
                if v & 0xC != 0 && v & 1 == 0 {
                    self.fifo_in.clear();
                    self.fifo_out.clear();
                    self.done = false;
                }
                if v & 1 != 0 { self.pending = true; }
                self.ctrl = v & !1;
            }
            0x80 => self.istat &= !v,
            _ => {}
        }
    }

    /// Run the queued commands; true when it raises the SIO2 interrupt
    pub fn run(&mut self) -> bool {
        if !self.pending { return false; }
        self.pending = false;
        self.transfers += 1;
        for k in 0..16 {
            let s3 = self.send3[k];
            if s3 == 0 { break; }
            // bytes sent in bits 8-16, bytes received in bits 18-26 (usually the same; the
            // memory card's auth reset sends 2 and reads 5)
            let tx = (s3 >> 8 & 0x1FF) as usize;
            let rx = match (s3 >> 18 & 0x1FF) as usize { 0 => tx, n => n };
            let tx = if tx == 0 { rx } else { tx };
            let block = self.dma_block;
            let take = if block > 0 { block.max(tx) } else { tx };
            let mut bytes: Vec<u8> = (0..take).map(|_| self.fifo_in.pop_front().unwrap_or(0)).collect();
            bytes.truncate(tx);
            // the exchange is as long as the longer side; the device sees zeros after the
            // command, and only `rx` reply bytes come back
            let len = tx.max(rx);
            bytes.resize(len, 0);
            let port = (s3 & 1) as usize;
            let out0 = self.fifo_out.len();
            if bytes.first() == Some(&0x01) && port < 2 {
                let pad = &mut self.pads[port];
                pad.select();
                self.fifo_out.push_back(0xFF);
                for &b in &bytes[1..] { self.fifo_out.push_back(pad.exchange(b).0); }
                self.recv1 = 0x1100;
            } else if bytes.first() == Some(&0x81) && self.card_out == 0 && self.cards[port & 1].is_some() {
                let reply = self.cards[port & 1].as_mut().unwrap().command(&bytes);
                self.fifo_out.extend(reply);
                self.recv1 = 0x1100;
            } else {
                self.fifo_out.extend(std::iter::repeat(0xFF).take(len));
                self.recv1 = 0x1D100;
            }
            self.fifo_out.truncate(out0 + rx);
            // a DMA'd reply fills its block
            if block > 0 {
                while (self.fifo_out.len() - out0) % block != 0 { self.fifo_out.push_back(0); }
            }
            // memory card commands only (pads and the multitap probes would flood it)
            if bytes.first() == Some(&0x81) {
                *self.card_counts.entry(bytes.get(1).copied().unwrap_or(0)).or_insert(0) += 1;
                let rep: Vec<u8> = self.fifo_out.iter().skip(out0).copied().collect();
                let b = |v: &[u8], i: usize| v.get(i).copied().unwrap_or(0);
                if self.log.len() >= 40 { self.log.pop_front(); }
                self.log.push_back((s3, [b(&bytes, 0), b(&bytes, 1), b(&bytes, 2)], len, self.recv1, [b(&rep, 2), b(&rep, 3), b(&rep, 4), b(&rep, 5)]));
            }
        }
        self.dma_block = 0;
        self.done = true;
        self.istat |= 1;
        true
    }
}
