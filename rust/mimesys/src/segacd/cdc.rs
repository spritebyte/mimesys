//! The CD data decoder (Sanyo LC8951, "CDC"): takes the data sectors the drive reads,
//! keeps them in its 16 KB buffer, and hands them to the CPUs (host data register) or by
//! DMA (PRG-RAM, word RAM, PCM). The sub CPU drives it through FF8004 (register select)
//! and FF8006 (register data). A perfect decoder: no read errors.
//! Ported from ares md/mcd/cdc.cpp + cdc-transfer.cpp (ISC license).

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
pub struct Flag { pub enable: bool, pub pending: bool }

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Transfer {
    /// 2 Genesis host read, 3 sub host read, 4 PCM, 5 PRG-RAM, 7 word RAM
    pub destination: u8,
    /// DMA destination address (19 bits)
    pub address: u32,
    pub source: u16,
    pub target: u16,
    pub pointer: u16,
    /// bytes left, minus one (12 bits)
    pub length: u16,
    pub enable: bool,
    pub active: bool,
    pub busy: bool,
    pub ready: bool,
    pub completed: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Cdc {
    #[serde(with = "serde_bytes")]
    pub ram: Vec<u8>,
    /// the register FF8006 reads / writes (auto-increments)
    pub address: u8,
    pub decoder_irq: Flag,
    pub transfer_irq: Flag,
    pub command_irq: Flag,
    pub transfer: Transfer,
    pub decoder_enable: bool,
    pub decoder_mode: bool,
    pub decoder_form: bool,
    pub decoder_valid: bool,
    pub header: [u8; 4],
    pub subheader: [u8; 4],
    pub status_active: bool,
    pub status_busy: bool,
    pub ctrl0: u8,
    pub ctrl1: u8,
    pub ctrl2: u8,
    pub ifctrl: u8,
}

impl Default for Cdc {
    fn default() -> Self {
        Self { ram: vec![0; 0x4000], address: 0, decoder_irq: Flag::default(), transfer_irq: Flag::default(), command_irq: Flag::default(),
               transfer: Transfer::default(), decoder_enable: false, decoder_mode: false, decoder_form: false, decoder_valid: false,
               header: [0; 4], subheader: [0; 4], status_active: false, status_busy: false, ctrl0: 0, ctrl1: 0, ctrl2: 0, ifctrl: 0 }
    }
}

fn bcd(v: i32) -> u8 { ((v / 10) << 4 | (v % 10)) as u8 }

impl Cdc {
    /// The decoder's interrupt line (level 5).
    pub fn irq_line(&self) -> bool {
        (self.decoder_irq.enable && self.decoder_irq.pending)
            || (self.transfer_irq.enable && self.transfer_irq.pending)
            || (self.command_irq.enable && self.command_irq.pending)
    }

    fn word(&self, i: u16) -> u16 {
        let i = (i as usize >> 1 & 0x1FFF) * 2;
        (self.ram[i] as u16) << 8 | self.ram[i + 1] as u16
    }

    /// The drive read data sector `sector` (raw 2352 bytes in `raw`, None if it couldn't).
    pub fn decode(&mut self, sector: i32, raw: Option<&[u8; 2352]>) {
        if !self.decoder_enable { return; }
        let f = (sector + 150).max(0);
        self.header = [bcd(f / 4500), bcd(f / 75 % 60), bcd(f % 75), 0x01];
        self.decoder_valid = true;
        self.decoder_irq.pending = true;
        if self.ctrl0 & 4 != 0 {
            // WRRQ: store it. The 12-byte sync goes at the tail, the rest from the pointer on.
            self.transfer.pointer = self.transfer.pointer.wrapping_add(2352);
            self.transfer.target = self.transfer.target.wrapping_add(2352);
            if let Some(raw) = raw {
                let p = self.transfer.pointer as usize;
                for i in 0..12 { self.ram[(p + 2340 + i) & 0x3FFF] = raw[i]; }
                for i in 0..2340 { self.ram[(p + i) & 0x3FFF] = raw[12 + i]; }
            }
        }
    }

    /// FF8006 read.
    pub fn read(&mut self) -> u8 {
        let t = &self.transfer;
        let head = self.ctrl1 & 1 != 0;
        let v = match self.address {
            0x0 => 0xFF, // COMIN: no commands from a host
            0x1 => (!self.status_active as u8) | (!t.active as u8) << 1 | (!self.status_busy as u8) << 2 | (!t.busy as u8) << 3
                | 1 << 4 | (!self.decoder_irq.pending as u8) << 5 | (!self.transfer_irq.pending as u8) << 6 | (!self.command_irq.pending as u8) << 7,
            0x2 => t.length as u8,
            0x3 => (t.length >> 8) as u8 & 15,
            r @ 0x4..=0x7 => if head { self.subheader[(r - 4) as usize] } else { self.header[(r - 4) as usize] },
            0x8 => t.pointer as u8,
            0x9 => (t.pointer >> 8) as u8,
            0xA => t.target as u8,
            0xB => (t.target >> 8) as u8,
            0xC => (self.decoder_enable as u8) << 7, // CRC OK
            0xD => 0,
            0xE => (self.decoder_form as u8) << 2 | (self.decoder_mode as u8) << 3,
            _ => {
                // STAT3: !VALST; reading it acknowledges the decoder interrupt
                let v = (!self.decoder_valid as u8) << 7;
                self.decoder_valid = false;
                self.decoder_irq.pending = false;
                v
            }
        };
        if self.address != 0 { self.address = (self.address + 1) & 15; }
        v
    }

    /// FF8006 write.
    pub fn write(&mut self, v: u8) {
        match self.address {
            0x0 => { self.status_active = true; self.status_busy = true; } // SBOUT (nothing listens)
            0x1 => {
                self.ifctrl = v;
                self.transfer.enable = v & 2 != 0;
                self.decoder_irq.enable = v & 0x20 != 0;
                self.transfer_irq.enable = v & 0x40 != 0;
                self.command_irq.enable = v & 0x80 != 0;
                if !self.transfer.enable { self.stop(); }
            }
            0x2 => self.transfer.length = (self.transfer.length & 0xF00) | v as u16,
            0x3 => self.transfer.length = (self.transfer.length & 0x0FF) | (v as u16 & 15) << 8,
            0x4 => self.transfer.source = (self.transfer.source & 0xFF00) | v as u16,
            0x5 => self.transfer.source = (self.transfer.source & 0x00FF) | (v as u16) << 8,
            0x6 => self.start(),
            0x7 => self.transfer_irq.pending = false, // DTACK
            0x8 => self.transfer.target = (self.transfer.target & 0xFF00) | v as u16,
            0x9 => self.transfer.target = (self.transfer.target & 0x00FF) | (v as u16) << 8,
            0xA => { self.ctrl0 = v; self.decoder_enable = v & 0x80 != 0; self.update_decoder(); }
            0xB => { self.ctrl1 = v; self.update_decoder(); }
            0xC => self.transfer.pointer = (self.transfer.pointer & 0xFF00) | v as u16,
            0xD => self.transfer.pointer = (self.transfer.pointer & 0x00FF) | (v as u16) << 8,
            0xE => self.ctrl2 = v,
            _ => {
                // RESET
                let ram = std::mem::take(&mut self.ram);
                let (destination, address) = (self.transfer.destination, self.transfer.address);
                let t = Transfer { destination, address, source: self.transfer.source, target: self.transfer.target,
                                   pointer: self.transfer.pointer, length: self.transfer.length, ..Transfer::default() };
                *self = Cdc { ram, transfer: t, ..Cdc::default() };
            }
        }
        if self.address != 0 { self.address = (self.address + 1) & 15; }
    }

    fn update_decoder(&mut self) {
        self.decoder_mode = self.ctrl1 & 8 != 0;
        self.decoder_form = self.ctrl1 & 4 != 0 && self.ctrl0 & 0x10 != 0;
    }

    fn start(&mut self) {
        if !self.transfer.enable { return; }
        let t = &mut self.transfer;
        t.active = true;
        t.busy = true;
        t.ready = t.destination == 2 || t.destination == 3;
        t.completed = false;
        self.transfer_irq.pending = false;
    }

    fn complete(&mut self) {
        let t = &mut self.transfer;
        t.active = false;
        t.busy = false;
        t.ready = false;
        t.completed = true;
        self.transfer_irq.pending = true;
    }

    pub fn stop(&mut self) {
        let t = &mut self.transfer;
        t.active = false;
        t.busy = false;
        t.ready = false;
    }

    /// Two bytes counted off the transfer (it ends when the count runs out).
    fn count_word(&mut self) {
        for _ in 0..2 {
            let was = self.transfer.length;
            self.transfer.length = was.wrapping_sub(1) & 0xFFF;
            if was == 0 { self.complete(); }
        }
    }

    /// The host data register (FF8008 for the sub CPU, A12008 for the Genesis).
    pub fn host_read(&mut self) -> u16 {
        if !self.transfer.ready { return 0xFFFF; }
        let v = self.word(self.transfer.source);
        self.transfer.source = self.transfer.source.wrapping_add(2);
        self.count_word();
        v
    }

    /// One DMA word, if a DMA is running: (destination, address, data).
    pub fn dma_word(&mut self) -> Option<(u8, u32, u16)> {
        let t = &self.transfer;
        if !t.active || !matches!(t.destination, 4 | 5 | 7) { return None; }
        let out = (t.destination, t.address, self.word(t.source));
        // PCM takes bytes: its address moves twice as fast
        let step = if t.destination == 4 { 4 } else { 2 };
        self.transfer.address = (self.transfer.address + step) & 0x7FFFF;
        self.transfer.source = self.transfer.source.wrapping_add(2);
        self.count_word();
        Some(out)
    }
}
