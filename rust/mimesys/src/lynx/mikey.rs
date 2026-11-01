//! Mikey: timers, interrupts, the LCD's DMA and palette, I/O port, UART.
//!
//! Clock: 16 MHz "ticks". A timer's clock select (CTLA bits 0-2) picks 1, 2, 4 ... 64 us
//! (16 << n ticks) or, with 7, a borrow out of the timer before it in its chain:
//!   0 -> 2 -> 4            1 -> 3 -> 5 -> 7 -> audio 0-3            6 on its own
//! A timer counts from its value down; going below 0 is a borrow: it reloads from
//! BACKUP (CTLA bit 4) or stops with "done" set, and raises its interrupt (CTLA bit 7).
//! Timer 0 clocks the LCD lines, timer 2 counts lines and ends the frame; timer 4's
//! interrupt is the UART's (level: TX empty / RX full while enabled).
//!
//! Sound: four channels, each a timer (8-11 here, linked after timer 7) driving a 12-bit
//! LFSR whose taps (bits 0 1 2 3 4 5 7 10 11) are chosen by FEEDBACK and CTL bit 7. On
//! every borrow the LFSR steps and the output is +volume / -volume by its bit 0, or, in
//! integrate mode, moves by the volume (clamped to a signed byte). The Lynx II mixes
//! them to two speakers: MSTEREO bits turn a channel off per side, MPAN applies the
//! ATTEN_A-D nibbles (left high, right low) in sixteenths.
//!
//! Display (Handy's timing): at frame end the line count reloads from timer 2's BACKUP;
//! three lines before that runs out the address latches from DISPADR and 102 lines of
//! 80 bytes (160 pixels, 4 bits each, high nibble left) go out, one per timer-0 borrow.

use serde::{Deserialize, Serialize};

pub const WIDTH: usize = 160;
pub const HEIGHT: usize = 102;

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Timer {
    pub backup: u8,
    pub ctla: u8,
    pub count: u8,
    pub done: bool,
    pub last_clock: bool,
    pub borrow_in: bool,
    pub borrow_out: bool,
    /// ticks since the last count (own clock)
    acc: u32,
}

impl Timer {
    fn enabled(&self) -> bool { self.ctla & 0x08 != 0 && (self.ctla & 0x10 != 0 || !self.done) }
    fn linked(&self) -> bool { self.ctla & 7 == 7 }

    /// Count down once; true on a borrow.
    fn tick(&mut self) -> bool {
        self.borrow_in = true;
        if self.count == 0 {
            self.borrow_out = true;
            self.done = true;
            self.count = if self.ctla & 0x10 != 0 { self.backup } else { 0 };
            true
        } else {
            self.count -= 1;
            self.borrow_out = false;
            false
        }
    }

    fn ctlb(&self) -> u8 {
        (self.done as u8) << 3 | (self.last_clock as u8) << 2 | (self.borrow_in as u8) << 1 | self.borrow_out as u8
    }
}

/// What happened during `Mikey::advance`.
#[derive(Default)]
pub struct Events {
    pub frame_end: bool,
    /// LCD lines to draw: (line 0-101, RAM address of its 80 bytes, flipped)
    pub lines: Vec<(usize, u16, bool)>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Mikey {
    /// timers 0-7, then the four audio channels' timers
    pub timers: [Timer; 12],
    pub audio: [Channel; 4],
    pub atten: [u8; 4],
    pub pan: u8,
    pub stereo: u8,
    /// writes to the sound registers (FD20-FD50) since the last debug report
    #[serde(skip)]
    pub sound_writes: u32,
    /// INTRST / INTSET: pending interrupts, one bit per timer
    pub irq_pending: u8,
    pub dispctl: u8,
    pub dispadr: u16,
    /// the 16 palette entries: green (bits 0-3), blue (4-7) << 8 | red (0-3)
    pub green: [u8; 16],
    pub bluered: [u8; 16],
    pub iodir: u8,
    pub iodat: u8,
    pub sysctl1: u8,
    pub serctl: u8,
    pub serdat: u8,
    // LCD DMA
    lynx_line: u32,
    dma_lines: u32,
    dma_addr: u16,
    rest: bool,
}

impl Mikey {
    pub fn new() -> Self {
        Self { timers: Default::default(), audio: Default::default(), atten: [0xFF; 4], pan: 0, stereo: 0, sound_writes: 0, irq_pending: 0, dispctl: 0, dispadr: 0, green: [0; 16], bluered: [0; 16],
               iodir: 0, iodat: 0, sysctl1: 0, serctl: 0, serdat: 0, lynx_line: 0, dma_lines: 0, dma_addr: 0, rest: false }
    }

    /// What the boot ROM leaves behind (Handy's homebrew preset): a 159 us line timer,
    /// 105 lines a frame (about 60 Hz), colour LCD DMA on.
    pub fn preset(&mut self) {
        self.timers[0].backup = 0x9E;
        self.timers[0].ctla = 0x18;
        self.timers[2].backup = 0x68;
        self.timers[2].ctla = 0x1F;
        self.dispctl = 0x09;
    }

    pub fn irq(&self) -> bool { self.irq_pending != 0 }

    /// Palette entry as RGB.
    pub fn rgb(&self, i: usize) -> [u8; 3] {
        let x = |n: u8| (n & 0x0F) * 17;
        [x(self.bluered[i]), x(self.green[i]), x(self.bluered[i] >> 4)]
    }

    fn timer_irq(&mut self, n: usize) {
        if self.timers[n].ctla & 0x80 != 0 { self.irq_pending |= 1 << n; }
    }

    /// Run `ticks` of 16 MHz time.
    pub fn advance(&mut self, ticks: u32, ev: &mut Events) {
        // timer 6 stands alone (games' sound drivers like it for their tick)
        for chain in [&[0usize, 2, 4][..], &[1, 3, 5, 7, 8, 9, 10, 11][..], &[6][..]] {
            let mut borrows = 0u32;                       // borrows coming down the chain
            for &n in chain.iter() {
                let t = &mut self.timers[n];
                if !t.enabled() { t.borrow_in = false; t.borrow_out = false; borrows = 0; continue; }
                let mut counts = 0u32;
                if t.linked() {
                    counts = borrows;
                } else {
                    let period = 16u32 << (t.ctla & 7);
                    t.acc += ticks;
                    if t.acc >= period { counts = t.acc / period; t.acc %= period; }
                }
                let mut out = 0;
                if counts == 0 { t.borrow_in = false; t.borrow_out = false; }
                for _ in 0..counts {
                    let t = &mut self.timers[n];
                    if !t.enabled() { break; }
                    if t.tick() {
                        out += 1;
                        match n {
                            0 => { self.timer_irq(0); self.line(ev); }
                            2 => { self.timer_irq(2); self.frame_end(ev); }
                            8..=11 => {
                                let linked = self.timers[n].linked();
                                let backup = self.timers[n].backup;
                                self.audio[n - 8].clock(backup != 0 || linked);
                            }
                            4 => {}                 // the UART's baud clock: its IRQ is the UART's
                            _ => self.timer_irq(n),
                        }
                    }
                }
                borrows = out;
            }
        }
        // the UART interrupt is a level while its condition holds (TX always empty here)
        if self.serctl & 0x80 != 0 { self.irq_pending |= 0x10; }
    }

    /// The two speakers' levels (each the sum of up to four signed channel outputs).
    pub fn sound(&self) -> (i32, i32) {
        let (mut l, mut r) = (0i32, 0i32);
        for c in 0..4 {
            let o = self.audio[c].output as i32;
            if self.stereo & (0x10 << c) == 0 {
                l += if self.pan & (0x10 << c) != 0 { o * (self.atten[c] & 0xF0) as i32 / 256 } else { o };
            }
            if self.stereo & (0x01 << c) == 0 {
                r += if self.pan & (0x01 << c) != 0 { o * (self.atten[c] & 0x0F) as i32 / 16 } else { o };
            }
        }
        (l, r)
    }

    fn line(&mut self, ev: &mut Events) {
        let bkup = self.timers[2].backup as u32;
        self.rest = self.lynx_line + 2 == bkup || self.lynx_line + 3 == bkup || self.lynx_line + 4 == bkup;
        if self.lynx_line + 3 == bkup {
            self.dma_addr = (self.dispadr & 0xFFFC) + if self.dispctl & 2 != 0 { 3 } else { 0 };
            self.dma_lines = HEIGHT as u32;
        }
        if self.lynx_line > 0 { self.lynx_line -= 1; }
        if self.dma_lines > 0 && self.dispctl & 1 != 0 {
            let line = HEIGHT - self.dma_lines as usize;
            let flip = self.dispctl & 2 != 0;
            ev.lines.push((line, self.dma_addr, flip));
            self.dma_addr = if flip { self.dma_addr.wrapping_sub(80) } else { self.dma_addr.wrapping_add(80) };
            self.dma_lines -= 1;
        } else if self.dma_lines > 0 {
            self.dma_lines -= 1;
        }
    }

    fn frame_end(&mut self, ev: &mut Events) {
        self.dma_lines = 0;
        self.lynx_line = self.timers[2].backup as u32;
        ev.frame_end = true;
    }

    pub fn read(&mut self, a: u8) -> u8 {
        match a {
            0x00..=0x1F => {
                let t = &self.timers[(a >> 2) as usize];
                match a & 3 { 0 => t.backup, 1 => t.ctla & 0xBF, 2 => t.count, _ => t.ctlb() }
            }
            0x20..=0x3F => {
                let c = ((a - 0x20) >> 3) as usize;
                let (ch, t) = (&self.audio[c], &self.timers[8 + c]);
                match a & 7 {
                    0 => ch.volume as u8,
                    1 => (ch.shaper >> 13) as u8,
                    2 => ch.output as u8,
                    3 => ch.shaper as u8,
                    4 => t.backup,
                    5 => (ch.integrate as u8) << 5 | (t.ctla & 0x1F) | if ch.shaper & 0x1000 != 0 { 0x80 } else { 0 },
                    6 => t.count,
                    _ => (t.borrow_out as u8) | (t.borrow_in as u8) << 1 | (t.last_clock as u8) << 3 | ((ch.shaper >> 4) & 0xF0) as u8,
                }
            }
            0x40..=0x43 => self.atten[(a & 3) as usize],
            0x44 => self.pan,
            0x50 => self.stereo,
            0x80 | 0x81 => self.irq_pending,
            0x84 | 0x85 => 0,                                   // MAGRDY
            0x86 => 0x80,                                       // AUDIN
            0x88 => 0x01,                                       // MIKEYHREV
            0x8B => {                                           // IODAT
                let d = self.iodir;
                let mut v = 0;
                v |= if d & 0x10 != 0 { self.iodat & 0x10 } else { 0x10 };       // AUDIN / EEPROM
                v |= if d & 0x08 != 0 { if self.iodat & 0x08 != 0 && self.rest { 0 } else { 0x08 } } else { 0 };
                v |= if d & 0x04 != 0 { self.iodat & 0x04 } else { 0 };           // NOEXP: no cable
                v |= if d & 0x02 != 0 { self.iodat & 0x02 } else { 0 };
                v |= if d & 0x01 != 0 { self.iodat & 0x01 } else { 0x01 };       // external power
                v
            }
            0x8C => 0x20 | 0x80,                                 // SERCTL: TX empty, TX ready
            0x8D => self.serdat,
            0x94 => self.dispadr as u8,
            0x95 => (self.dispadr >> 8) as u8,
            0xA0..=0xAF => self.green[(a & 15) as usize],
            0xB0..=0xBF => self.bluered[(a & 15) as usize],
            _ => 0xFF,
        }
    }

    /// Register write. Returns what the system bus has to act on.
    pub fn write(&mut self, a: u8, v: u8) -> MikeyWrite {
        if (0x20..=0x50).contains(&a) { self.sound_writes += 1; }
        match a {
            0x00..=0x1F => {
                let t = &mut self.timers[(a >> 2) as usize];
                match a & 3 {
                    0 => t.backup = v,
                    1 => {
                        t.ctla = v & 0xBF;
                        if v & 0x40 != 0 { t.done = false; }
                        if v & 0x48 != 0 { t.acc = 0; }
                        if v & 0x80 == 0 { self.irq_pending &= !(1 << (a >> 2)); }
                    }
                    2 => t.count = v,
                    _ => {
                        t.done = v & 0x08 != 0; t.last_clock = v & 0x04 != 0;
                        t.borrow_in = v & 0x02 != 0; t.borrow_out = v & 0x01 != 0;
                    }
                }
            }
            0x20..=0x3F => {
                let c = ((a - 0x20) >> 3) as usize;
                let (ch, t) = (&mut self.audio[c], &mut self.timers[8 + c]);
                match a & 7 {
                    0 => ch.volume = v as i8,
                    1 => ch.shaper = (ch.shaper & 0x001FFF) | (v as u32) << 13,
                    2 => ch.output = v as i8,
                    3 => ch.shaper = (ch.shaper & 0x1FFF00) | v as u32,
                    4 => t.backup = v,
                    5 => {
                        t.ctla = v & 0x1F;
                        ch.integrate = v & 0x20 != 0;
                        if v & 0x40 != 0 { t.done = false; }
                        if v & 0x48 != 0 { t.acc = 0; }
                        ch.shaper = (ch.shaper & 0x1FEFFF) | if v & 0x80 != 0 { 0x1000 } else { 0 };
                    }
                    6 => t.count = v,
                    _ => {
                        ch.shaper = (ch.shaper & 0x1FF0FF) | ((v & 0xF0) as u32) << 4;
                        t.borrow_in = v & 2 != 0; t.borrow_out = v & 1 != 0; t.last_clock = v & 4 != 0;
                    }
                }
            }
            0x40..=0x43 => self.atten[(a & 3) as usize] = v,
            0x44 => self.pan = v,
            0x50 => self.stereo = v,
            0x80 => self.irq_pending &= !v,                     // INTRST
            0x81 => self.irq_pending |= v,                      // INTSET
            0x87 => { self.sysctl1 = v; return MikeyWrite::Strobe(v & 1 != 0); }
            0x8A => self.iodir = v,
            0x8B => { self.iodat = v; return MikeyWrite::AddrData(v & 2 != 0); }
            0x8C => { self.serctl = v; if v & 0x80 == 0 { self.irq_pending &= !0x10; } }
            0x8D => self.serdat = v,
            0x91 => return MikeyWrite::Sleep,                   // CPUSLEEP
            0x92 => self.dispctl = v,
            0x94 => self.dispadr = (self.dispadr & 0xFF00) | v as u16,
            0x95 => self.dispadr = (self.dispadr & 0x00FF) | (v as u16) << 8,
            0x97 => return MikeyWrite::BootTrap,
            0xA0..=0xAF => self.green[(a & 15) as usize] = v & 0x0F,
            0xB0..=0xBF => self.bluered[(a & 15) as usize] = v,
            _ => {}
        }
        MikeyWrite::None
    }
}

/// One sound channel's waveform state (its timer lives in `Mikey::timers[8 + n]`).
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Channel {
    pub volume: i8,
    /// bits 0-11 the LFSR, 12-20 the feedback taps (for LFSR bits 7 0 1 2 3 4 5 10 11)
    pub shaper: u32,
    pub integrate: bool,
    pub output: i8,
}

impl Channel {
    /// A borrow of the channel's timer.
    fn clock(&mut self, step: bool) {
        if step {
            const TAPS: [u32; 9] = [7, 0, 1, 2, 3, 4, 5, 10, 11];
            let (taps, lfsr) = (self.shaper >> 12, self.shaper & 0xFFF);
            let mut x = 0;
            for (i, &b) in TAPS.iter().enumerate() { if taps >> i & 1 != 0 { x ^= lfsr >> b & 1; } }
            self.shaper = taps << 12 | (lfsr << 1 & 0xFFE) | (x ^ 1);
        }
        let v = self.volume as i32;
        let up = self.shaper & 1 != 0;
        self.output = if self.integrate {
            (self.output as i32 + if up { v } else { -v }).clamp(-128, 127) as i8
        } else if up { self.volume } else { (-v).clamp(-128, 127) as i8 };
    }
}

pub enum MikeyWrite { None, Strobe(bool), AddrData(bool), Sleep, BootTrap }
