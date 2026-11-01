//! Small DS devices: interrupt flags, the IPC link between the CPUs, timers and the
//! ARM9's divide / square-root unit.

use std::collections::VecDeque;
use serde::{Deserialize, Serialize};

// ---- interrupt bits (IE / IF) ----
pub const IRQ_VBLANK: u32 = 1 << 0;
pub const IRQ_HBLANK: u32 = 1 << 1;
pub const IRQ_VCOUNT: u32 = 1 << 2;
pub const IRQ_TIMER0: u32 = 1 << 3;
pub const IRQ_RTC: u32 = 1 << 7;
pub const IRQ_DMA0: u32 = 1 << 8;
pub const IRQ_KEYPAD: u32 = 1 << 12;
pub const IRQ_IPC_SYNC: u32 = 1 << 16;
pub const IRQ_IPC_SEND_EMPTY: u32 = 1 << 17;
pub const IRQ_IPC_RECV: u32 = 1 << 18;
pub const IRQ_CART_DONE: u32 = 1 << 19;
pub const IRQ_GXFIFO: u32 = 1 << 21;
pub const IRQ_SPI: u32 = 1 << 23;
pub const IRQ_WIFI: u32 = 1 << 24;

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Irq {
    pub ime: u32,
    pub ie: u32,
    pub iflags: u32,
}

impl Irq {
    pub fn raise(&mut self, bits: u32) { self.iflags |= bits; }
    /// The CPU's IRQ line.
    pub fn line(&self) -> bool { self.ime & 1 != 0 && self.ie & self.iflags != 0 }
    /// Wakes a halted CPU (IME doesn't matter).
    pub fn pending(&self) -> bool { self.ie & self.iflags != 0 }
}

// ---- IPC ----

/// One CPU's side of the IPC registers.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct IpcSide {
    /// IPCSYNC bits 8-11 (what the other side reads in bits 0-3) and bit 14 (IRQ enable)
    pub sync_out: u8,
    pub sync_irq_enable: bool,
    /// IPCFIFOCNT bits 2 (send-empty IRQ), 10 (receive IRQ), 14 (error), 15 (enable)
    pub send_irq: bool,
    pub recv_irq: bool,
    pub error: bool,
    pub enabled: bool,
    /// this side's send FIFO (the other side receives from it)
    pub fifo: VecDeque<u32>,
    pub last_recv: u32,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Ipc {
    /// [ARM9, ARM7]
    pub side: [IpcSide; 2],
}

impl Ipc {
    pub fn read_sync(&self, me: usize) -> u32 {
        let s = &self.side[me];
        let o = &self.side[me ^ 1];
        (o.sync_out & 0xF) as u32 | ((s.sync_out & 0xF) as u32) << 8 | (s.sync_irq_enable as u32) << 14
    }

    /// IPCSYNC write. Returns the IRQ to raise on the other side.
    pub fn write_sync(&mut self, me: usize, v: u32, mask: u32) -> u32 {
        let mut remote = 0;
        if mask & 0xFF00 != 0 {
            let s = &mut self.side[me];
            s.sync_out = ((v >> 8) & 0xF) as u8;
            s.sync_irq_enable = v & (1 << 14) != 0;
            if v & (1 << 13) != 0 && self.side[me ^ 1].sync_irq_enable { remote = IRQ_IPC_SYNC; }
        }
        remote
    }

    pub fn read_cnt(&self, me: usize) -> u32 {
        let s = &self.side[me];
        let recv = &self.side[me ^ 1].fifo;
        (s.fifo.is_empty() as u32)
            | ((s.fifo.len() >= 16) as u32) << 1
            | (s.send_irq as u32) << 2
            | (recv.is_empty() as u32) << 8
            | ((recv.len() >= 16) as u32) << 9
            | (s.recv_irq as u32) << 10
            | (s.error as u32) << 14
            | (s.enabled as u32) << 15
    }

    /// IPCFIFOCNT write. Returns IRQs for this side (the IRQs fire on enabling while
    /// the condition already holds, as on hardware).
    pub fn write_cnt(&mut self, me: usize, v: u32, mask: u32) -> u32 {
        let mut irq = 0;
        let recv_nonempty = !self.side[me ^ 1].fifo.is_empty();
        let s = &mut self.side[me];
        if mask & 0xFF != 0 {
            if v & (1 << 3) != 0 { s.fifo.clear(); }
            let was = s.send_irq;
            s.send_irq = v & (1 << 2) != 0;
            if s.send_irq && s.fifo.is_empty() && (!was || v & (1 << 3) != 0) { irq |= IRQ_IPC_SEND_EMPTY; }
        }
        if mask & 0xFF00 != 0 {
            let was = s.recv_irq;
            s.recv_irq = v & (1 << 10) != 0;
            if s.recv_irq && !was && recv_nonempty { irq |= IRQ_IPC_RECV; }
            if v & (1 << 14) != 0 { s.error = false; }
            s.enabled = v & (1 << 15) != 0;
        }
        irq
    }

    /// Returns IRQs for (this side, the other side).
    pub fn send(&mut self, me: usize, v: u32) -> (u32, u32) {
        let s = &mut self.side[me];
        if !s.enabled { return (0, 0); }
        if s.fifo.len() >= 16 { s.error = true; return (0, 0); }
        let was_empty = s.fifo.is_empty();
        s.fifo.push_back(v);
        let theirs = if was_empty && self.side[me ^ 1].recv_irq { IRQ_IPC_RECV } else { 0 };
        (0, theirs)
    }

    /// Returns (value, IRQs for this side, IRQs for the other side).
    pub fn recv(&mut self, me: usize) -> (u32, u32, u32) {
        if !self.side[me].enabled { return (self.side[me].last_recv, 0, 0); }
        let other = &mut self.side[me ^ 1];
        match other.fifo.pop_front() {
            Some(v) => {
                let theirs = if other.fifo.is_empty() && other.send_irq { IRQ_IPC_SEND_EMPTY } else { 0 };
                self.side[me].last_recv = v;
                (v, 0, theirs)
            }
            None => {
                self.side[me].error = true;
                (self.side[me].last_recv, 0, 0)
            }
        }
    }
}

// ---- timers ----

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Timer {
    pub reload: u16,
    pub control: u8,
    /// counter with 10 fractional bits (prescaler steps)
    pub counter: u32,
}

/// Four timers counting at the 33.5 MHz bus clock (with prescalers 1/64/256/1024).
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Timers {
    pub t: [Timer; 4],
    /// bus clock the timers were last brought up to
    pub last: u64,
}

const PRESCALE_SHIFT: [u32; 4] = [0, 6, 8, 10];

impl Timers {
    /// Advance to bus clock `now`; returns timer IRQ bits.
    pub fn update(&mut self, now: u64) -> u32 {
        let elapsed = now.saturating_sub(self.last);
        self.last = now;
        if elapsed == 0 { return 0; }
        let mut irq = 0;
        let mut overflows_prev = 0u64;
        for i in 0..4 {
            let t = &mut self.t[i];
            let ovf;
            if t.control & 0x80 == 0 {
                ovf = 0;
            } else if i > 0 && t.control & 0x04 != 0 {
                ovf = Self::add_ticks(t, overflows_prev);
            } else {
                // counter keeps fractional prescaler phase in its low bits
                let shift = PRESCALE_SHIFT[(t.control & 3) as usize];
                let frac = (t.counter & 0x3FF) as u64 + (elapsed << (10 - shift));
                let ticks = frac >> 10;
                t.counter = (t.counter & !0x3FF) | (frac & 0x3FF) as u32;
                ovf = Self::add_ticks(t, ticks);
            }
            if ovf > 0 && t.control & 0x40 != 0 { irq |= super::nds_io::IRQ_TIMER0 << i; }
            overflows_prev = ovf;
        }
        irq
    }

    /// Add whole ticks to the 16-bit count (bits 10-25 of `counter`); returns overflows.
    fn add_ticks(t: &mut Timer, ticks: u64) -> u64 {
        if ticks == 0 { return 0; }
        let mut value = ((t.counter >> 10) & 0xFFFF) as u64 + ticks;
        let mut ovf = 0;
        if value > 0xFFFF {
            let period = 0x10000 - t.reload as u64;
            value -= 0x10000;
            ovf = 1 + value / period;
            value = t.reload as u64 + value % period;
        }
        t.counter = (t.counter & 0x3FF) | ((value as u32) << 10);
        ovf
    }

    pub fn read(&self, i: usize) -> u32 {
        let t = &self.t[i];
        ((t.counter >> 10) & 0xFFFF) | (t.control as u32) << 16
    }

    pub fn write(&mut self, i: usize, v: u32, mask: u32) {
        let t = &mut self.t[i];
        if mask & 0xFFFF != 0 { t.reload = v as u16; }
        if mask & 0xFF_0000 != 0 {
            let c = (v >> 16) as u8;
            if c & 0x80 != 0 && t.control & 0x80 == 0 {
                t.counter = (t.reload as u32) << 10;     // starting: load the reload value
            }
            t.control = c & 0xC7;
        }
    }
}

// ---- ARM9 maths ----

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Math {
    pub divcnt: u32,
    pub numer: u64,
    pub denom: u64,
    pub quot: u64,
    pub rem: u64,
    pub sqrtcnt: u32,
    pub sqrt_param: u64,
    pub sqrt_result: u32,
}

impl Math {
    /// DIVCNT mode: 0 = 32/32, 1 = 64/32, 2 = 64/64.
    pub fn divide(&mut self) {
        let mode = self.divcnt & 3;
        let (n, d): (i64, i64) = match mode {
            0 => (self.numer as u32 as i32 as i64, self.denom as u32 as i32 as i64),
            1 => (self.numer as i64, self.denom as u32 as i32 as i64),
            _ => (self.numer as i64, self.denom as i64),
        };
        let div0 = if mode == 0 { self.denom as u32 == 0 } else { self.denom == 0 };
        self.divcnt = (self.divcnt & !0x4000) | if self.denom == 0 { 0x4000 } else { 0 };
        if div0 {
            // quotient +-1 against the numerator's sign, remainder = numerator; in 32-bit
            // mode the quotient's upper word comes out inverted
            self.rem = n as u64;
            self.quot = if n < 0 { 1 } else { u64::MAX };
            if mode == 0 { self.quot ^= 0xFFFF_FFFF_0000_0000; }
            return;
        }
        if n == i64::MIN && d == -1 {
            self.quot = n as u64;          // 64-bit overflow
            self.rem = 0;
            return;
        }
        self.quot = (n / d) as u64;
        self.rem = (n % d) as u64;
    }

    /// SQRTCNT mode: 0 = 32-bit input, 1 = 64-bit.
    pub fn sqrt(&mut self) {
        let v = if self.sqrtcnt & 1 == 0 { self.sqrt_param & 0xFFFF_FFFF } else { self.sqrt_param };
        let v = v as u128;
        let mut r = (v as f64).sqrt() as u128;
        while r * r > v { r -= 1; }
        while (r + 1) * (r + 1) <= v { r += 1; }
        self.sqrt_result = r as u32;
    }
}
