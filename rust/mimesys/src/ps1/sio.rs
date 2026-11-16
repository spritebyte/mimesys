//! SIO0 (1F801040-1F80104F): the controller and memory-card port, from Nocash PSX-SPX.
//!
//! Digital pads only so far (ID 5A41): the host sends 01 42 00 00 00 and gets FF 41 5A,
//! then the two button bytes (active low). Every byte but the last is answered with an ACK
//! pulse, which raises IRQ 7 when JOY_CTRL bit 12 allows. Memory cards (address 81) aren't
//! there yet: nothing answers them.

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
    /// position in the current pad exchange (0 = waiting for 01)
    step: u8,
    /// which device the exchange is with: 0 none, 1 pad
    device: u8,
    /// buttons, active low (bit 0 Select ... bit 15 Square), for ports 1 and 2
    #[serde(skip)]
    pub buttons: [u16; 2],
    #[serde(skip)]
    pub connected: [bool; 2],
    pub irq: bool,
}

impl Sio {
    pub fn new() -> Self {
        Self { baud: 0x88, buttons: [0xFFFF; 2], connected: [true, false], ..Default::default() }
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
        match self.step {
            0 => {
                if b == 0x01 && self.connected[p] {
                    self.device = 1;
                    self.step = 1;
                    (0xFF, true)
                } else {
                    (0xFF, false)
                }
            }
            1 => { self.step = 2; (0x41, b == 0x42) }
            2 => { self.step = 3; (0x5A, true) }
            3 => { self.step = 4; (self.buttons[p] as u8, true) }
            4 => { self.step = 0; self.device = 0; ((self.buttons[p] >> 8) as u8, false) }
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
