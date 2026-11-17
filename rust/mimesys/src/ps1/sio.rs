//! SIO0 (1F801040-1F80104F): the controller and memory-card port, from Nocash PSX-SPX.
//!
//! - Controllers: address 01, then the pad's own commands (`pad.rs`: digital pad or
//!   DualShock).
//! - Memory cards: address 81, then the card's own commands (`memcard.rs`).
//! - Every byte but the last is answered with an ACK pulse, which raises IRQ 7 when
//!   JOY_CTRL bit 12 allows.

use super::memcard::MemCard;
use super::pad::Pad;
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Sio {
    ctrl: u16,
    mode: u16,
    baud: u16,
    rx: Option<u8>,
    irq_flag: bool,
    /// cycles until the byte in flight arrives / until its ACK
    xfer_wait: i32,
    ack_wait: i32,
    pending_rx: u8,
    pending_ack: bool,
    /// position in the current exchange (0 = waiting for the address)
    step: u8,
    /// which device the exchange is with: 0 none, 1 pad, 2 memory card
    device: u8,
    /// the controllers in ports 1 and 2 (their modes are saved, their inputs aren't)
    pub pads: [Pad; 2],
    #[serde(skip)]
    pub connected: [bool; 2],
    /// the memory cards in slots 1 and 2 (kept out of save states: they live in their files)
    #[serde(skip)]
    pub cards: [Option<MemCard>; 2],
    pub irq: bool,
}

impl Sio {
    pub fn new() -> Self {
        Self { baud: 0x88, connected: [true, false], ..Default::default() }
    }

    pub fn read(&mut self, a: u32) -> u32 {
        match a & 0xF {
            0 => self.rx.take().map_or(0xFF, |b| b as u32),
            4 => {
                let mut s = 1 | 4;                       // TX ready, TX finished
                if self.rx.is_some() { s |= 2; }
                if self.irq_flag { s |= 1 << 9; }
                s
            }
            8 => self.mode as u32,
            0xA => self.ctrl as u32,
            0xE => self.baud as u32,
            _ => 0,
        }
    }

    pub fn write(&mut self, a: u32, v: u32) {
        match a & 0xF {
            0 => self.send(v as u8),
            8 => self.mode = v as u16,
            0xA => {
                self.ctrl = v as u16;
                if v & 0x10 != 0 { self.irq_flag = false; }        // acknowledge
                if v & 0x40 != 0 { self.ctrl = 0; self.irq_flag = false; self.rx = None; }
                if v & 2 == 0 { self.step = 0; self.device = 0; }   // deselected
            }
            0xE => self.baud = v as u16,
            _ => {}
        }
    }

    fn port(&self) -> usize { ((self.ctrl >> 13) & 1) as usize }

    fn send(&mut self, b: u8) {
        let selected = self.ctrl & 2 != 0;
        let (reply, ack) = if !selected {
            (0xFF, false)
        } else {
            self.exchange(b)
        };
        self.pending_rx = reply;
        self.pending_ack = ack;
        self.xfer_wait = (self.baud as i32).max(1) * 8;
    }

    /// The device's answer to one byte, and whether it ACKs
    fn exchange(&mut self, b: u8) -> (u8, bool) {
        let p = self.port();
        if self.device == 2 {
            let Some(card) = self.cards[p].as_mut() else { return (0xFF, false) };
            let (r, more) = card.exchange(b);
            if !more { self.device = 0; self.step = 0; }
            return (r, more);
        }
        if self.device == 1 {
            let (r, more) = self.pads[p].exchange(b);
            if !more { self.device = 0; self.step = 0; }
            return (r, more);
        }
        match self.step {
            0 => {
                if b == 0x01 && self.connected[p] {
                    self.device = 1;
                    self.pads[p].select();
                    (0xFF, true)
                } else if b == 0x81 && self.cards[p].is_some() {
                    self.device = 2;
                    self.cards[p].as_mut().unwrap().select();
                    (0xFF, true)
                } else {
                    (0xFF, false)
                }
            }
            _ => { self.step = 0; (0xFF, false) }
        }
    }

    pub fn tick(&mut self, cycles: u32) {
        if self.xfer_wait > 0 {
            self.xfer_wait -= cycles as i32;
            if self.xfer_wait <= 0 {
                self.rx = Some(self.pending_rx);
                if self.pending_ack { self.ack_wait = 100; }
            }
        }
        if self.ack_wait > 0 {
            self.ack_wait -= cycles as i32;
            if self.ack_wait <= 0 && self.ctrl & 0x1000 != 0 && !self.irq_flag {
                self.irq_flag = true;
                self.irq = true;
            }
        }
    }
}
