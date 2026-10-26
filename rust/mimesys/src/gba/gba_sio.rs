//! GBA serial / link port (0x04000120-0x0400015F).
//!
//! Modes: RCNT bit 15 = 0 → SIOCNT bits 12-13 pick Normal 8-bit, Normal 32-bit,
//! Multiplayer or UART; RCNT bits 14-15 = 10 → general purpose, 11 → JOY bus.
//!
//! Nothing plugged in (`linked` false):
//! * Normal mode with the internal clock: a transfer takes 8 or 32 bits at 256 kHz
//!   (64 clocks a bit) or 2 MHz (8 clocks), then receives all ones, clears the start
//!   bit and can raise the serial IRQ.
//! * Multiplayer, UART and external-clock transfers never finish.
//! Register read-back follows mGBA's test suite (sio-read). RCNT bits 0-3 read the link
//! wires (SC SD SI SO); Super Mario Advance polls SD there while "Checking".
//!
//! Linked to another GBA (`linked`; two side by side, or over the network):
//! * The side that drives a transfer (normal mode with the internal clock, or the
//!   multiplayer parent) runs it for its normal time, then sets `awaiting`; the CPU
//!   stops until the link hands the data across (`external` on the other GBA) and
//!   brings the answer back (`finish`). Same scheme as the Game Boy link.
//! * Normal mode: the other side takes part if its start bit is set with the external
//!   clock; otherwise the driver reads all ones.
//! * Multiplayer (2 players): the parent (`parent`: left GBA / the host) starts; both
//!   end with SIOMULTI0 = parent's SIOMLT_SEND, SIOMULTI1 = child's, 2-3 = FFFF, and
//!   their IRQ. SIOCNT shows SI (parent 0, child 1), the player ID, and SD "all ready",
//!   which is only set while both GBAs are in multiplayer mode (`partner_multi`, kept up
//!   to date by the pair / the network link).
//! `trace` (when on) records register writes, changed reads and transfers for debugging.

use serde::{Deserialize, Serialize};

/// Transfer kinds on the cable (`NetPort` mode byte).
pub const XFER_N8: u8 = 0;
pub const XFER_N32: u8 = 1;
pub const XFER_MULTI: u8 = 2;

#[derive(Clone, Copy, PartialEq)]
enum Mode { Normal8, Normal32, Multi, Uart, Gpio, Joy }

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Sio {
    siocnt: u16,
    rcnt: u16,
    data32: u32,
    data8: u16,
    joycnt: u16,
    /// a running transfer this side drives ends at this system clock
    pub end: Option<u64>,
    /// joined to another GBA
    pub linked: bool,
    /// multiplayer parent (left GBA / network host)
    pub parent: bool,
    /// our driven transfer is done and waits for the other side: (kind, our data)
    pub awaiting: Option<(u8, u32)>,
    multi: [u16; 4],
    id: u8,
    irq: bool,
    /// the other GBA is in multiplayer mode
    pub partner_multi: bool,
    /// system clock of the last access (for the trace)
    #[serde(skip)] pub clock: u64,
    #[serde(skip)] pub trace: Option<Vec<String>>,
    #[serde(skip)] last_read: [u16; 0x20],
    /// reads of each register since the last frame summary (trace)
    #[serde(skip)] reads: [u32; 0x20],
}

impl Sio {
    pub fn new() -> Self { Self::default() }

    /// In multiplayer mode (what the other GBA's SD bit depends on).
    pub fn in_multi(&self) -> bool { self.mode() == Mode::Multi }

    /// Trace line from outside (the system's frame summary), with this frame's poll counts.
    pub fn note_frame(&mut self, clock: u64, state: String) {
        if self.trace.is_none() { return; }
        let polls: Vec<String> = self.reads.iter().enumerate().filter(|(_, n)| **n > 0)
            .map(|(i, n)| format!("{:03X}x{n}", 0x120 + i * 2)).collect();
        self.reads = [0; 0x20];
        self.clock = clock;
        self.log(|| format!("frame: {state} polled [{}]", polls.join(" ")));
    }

    fn log(&mut self, msg: impl FnOnce() -> String) {
        let clock = self.clock;
        if let Some(t) = &mut self.trace {
            if t.len() < 500_000 { t.push(format!("{clock:>12} {}", msg())); }
        }
    }

    fn mode(&self) -> Mode {
        if self.rcnt & 0x8000 != 0 {
            return if self.rcnt & 0x4000 != 0 { Mode::Joy } else { Mode::Gpio };
        }
        match (self.siocnt >> 12) & 3 { 0 => Mode::Normal8, 1 => Mode::Normal32, 2 => Mode::Multi, _ => Mode::Uart }
    }

    fn read16(&self, off: u32) -> u16 {
        let mode = self.mode();
        match off {
            0x120..=0x127 if mode == Mode::Multi => self.multi[((off - 0x120) / 2) as usize],
            0x120 => if mode == Mode::Normal32 { self.data32 as u16 } else { 0 },
            0x122 => if mode == Mode::Normal32 { (self.data32 >> 16) as u16 } else { 0 },
            0x128 => {
                let mut v = self.siocnt & 0x7F8F;
                if mode == Mode::Uart { v |= 0x20; }
                if mode == Mode::Multi && self.linked {
                    // SI: parent's is grounded; SD: everyone is in multiplayer; ID
                    let sd = if self.partner_multi { 8 } else { 0 };
                    v = (v & !0x3C) | if self.parent { 0 } else { 4 } | sd | ((self.id as u16) << 4);
                }
                v
            }
            0x12A => if mode == Mode::Uart { 0 } else { self.data8 },
            0x134 => {
                // bits 0-3 show the link wires (SC, SD, SI, SO), not what was written,
                // except in general-purpose mode where they are the data bits
                let wires: u16 = match mode {
                    Mode::Gpio => return self.rcnt & 0xC1FF,
                    Mode::Normal8 | Mode::Normal32 => 0b0101,
                    Mode::Joy => 0b1100,
                    Mode::Multi if self.linked => {
                        // SD high while everyone is in multiplayer; the parent's SI is grounded
                        1 | if self.partner_multi { 2 } else { 0 } | if self.parent { 0 } else { 4 } | 8
                    }
                    _ => 0b1111,
                };
                (self.rcnt & 0xC1F0) | wires
            }
            0x140 => self.joycnt & 0x40,
            _ => 0,
        }
    }

    pub fn read8(&self, off: u32) -> u8 { (self.read16(off & !1) >> ((off & 1) * 8)) as u8 }

    /// read8 for the CPU: with the trace on, notes values that changed since the last read.
    pub fn read8_traced(&mut self, off: u32) -> u8 {
        if self.trace.is_some() && off & 1 == 0 {
            let v = self.read16(off);
            let i = ((off - 0x120) / 2) as usize & 0x1F;
            self.reads[i] += 1;
            if self.last_read[i] != v {
                self.last_read[i] = v;
                self.log(|| format!("R {off:03X} = {v:04X}"));
            }
        }
        self.read8(off)
    }

    /// Clocks a multiplayer exchange of two GBAs takes: per unit a start bit, 16 data
    /// bits and a stop bit, plus a gap.
    fn multi_time(&self) -> u64 {
        let baud = [9600u64, 38400, 57600, 115200][(self.siocnt & 3) as usize];
        40 * 16_777_216 / baud
    }

    /// Byte write at time `now`.
    pub fn write8(&mut self, off: u32, v: u8, now: u64) {
        self.clock = now;
        self.write8_inner(off, v, now);
        if self.trace.is_some() && off & 1 == 1 {
            let r = self.read16(off & !1);
            self.log(|| format!("W {:03X} = {r:04X}", off & !1));
        }
    }

    fn write8_inner(&mut self, off: u32, v: u8, now: u64) {
        let sh = (off & 1) * 8;
        let set16 = |r: &mut u16| *r = (*r & !(0xFF << sh)) | ((v as u16) << sh);
        match off {
            0x120..=0x123 => {
                let s = (off - 0x120) * 8;
                self.data32 = (self.data32 & !(0xFF << s)) | ((v as u32) << s);
            }
            0x12A | 0x12B => set16(&mut self.data8),
            0x128 | 0x129 => {
                let was = self.siocnt & 0x80 != 0;
                set16(&mut self.siocnt);
                self.siocnt &= 0x7FFF;
                let start = self.siocnt & 0x80 != 0;
                if start && !was && off == 0x128 {
                    self.end = match self.mode() {
                        Mode::Normal8 | Mode::Normal32 if self.siocnt & 1 != 0 => {
                            let bits = if self.mode() == Mode::Normal32 { 32 } else { 8 };
                            let per_bit = if self.siocnt & 2 != 0 { 8 } else { 64 };
                            Some(now + bits * per_bit)
                        }
                        Mode::Multi if self.linked && self.parent => {
                            self.multi = [0xFFFF; 4];
                            Some(now + self.multi_time())
                        }
                        Mode::Multi if self.linked => {
                            // a child can't start; its busy bit follows the parent
                            self.siocnt &= !0x80;
                            None
                        }
                        _ => None,
                    };
                } else if !start {
                    self.end = None;
                    self.awaiting = None;
                }
            }
            0x134 | 0x135 => set16(&mut self.rcnt),
            0x140 => {
                self.joycnt &= !(v as u16 & 7);              // flags: write 1 to clear
                self.joycnt = (self.joycnt & !0x40) | (v as u16 & 0x40);
            }
            _ => {}
        }
    }

    /// The driven transfer's time is up: hand it to the link, or finish it as unplugged.
    pub fn on_end(&mut self) {
        self.end = None;
        let linked = self.linked;
        self.log(|| format!("transfer time up ({})", if linked { "to the other GBA" } else { "unplugged" }));
        if self.linked {
            self.awaiting = Some(match self.mode() {
                Mode::Normal32 => (XFER_N32, self.data32),
                Mode::Multi => (XFER_MULTI, self.data8 as u32),
                _ => (XFER_N8, (self.data8 & 0xFF) as u32),
            });
        } else {
            self.unplugged();
        }
    }

    fn done(&mut self) {
        self.siocnt &= !0x80;
        if self.siocnt & 0x4000 != 0 { self.irq = true; }
    }

    /// Finish our transfer with nobody on the other end: the input line stays high.
    pub fn unplugged(&mut self) {
        self.awaiting = None;
        match self.mode() {
            Mode::Normal32 => self.data32 = 0xFFFF_FFFF,
            Mode::Multi => self.multi = [self.data8, 0xFFFF, 0xFFFF, 0xFFFF],
            _ => self.data8 |= 0xFF,
        }
        self.done();
    }

    /// The other GBA drove a transfer of `kind` with `value`: take part if ready.
    /// Returns our data for it (all ones when not ready).
    pub fn external(&mut self, kind: u8, value: u32) -> u32 {
        let reply = self.external_inner(kind, value);
        let cnt = self.read16(0x128);
        self.log(|| format!("in: kind {kind} value {value:08X} -> answered {reply:08X} (SIOCNT {cnt:04X})"));
        reply
    }

    fn external_inner(&mut self, kind: u8, value: u32) -> u32 {
        match (kind, self.mode()) {
            (XFER_MULTI, Mode::Multi) => {
                let mine = self.data8;
                self.multi = [value as u16, mine, 0xFFFF, 0xFFFF];
                self.id = 1;
                self.done();
                mine as u32
            }
            (XFER_N8, Mode::Normal8) if self.ready_external() => {
                let mine = (self.data8 & 0xFF) as u32;
                self.data8 = (self.data8 & 0xFF00) | (value as u16 & 0xFF);
                self.done();
                mine
            }
            (XFER_N32, Mode::Normal32) if self.ready_external() => {
                let mine = self.data32;
                self.data32 = value;
                self.done();
                mine
            }
            _ => 0xFFFF_FFFF,
        }
    }

    fn ready_external(&self) -> bool { self.siocnt & 0x81 == 0x80 }

    /// The other side's answer to our transfer.
    pub fn finish(&mut self, value: u32) {
        let Some((kind, sent)) = self.awaiting.take() else { return };
        self.log(|| format!("out: kind {kind} value {sent:08X} -> got {value:08X}"));
        match kind {
            XFER_MULTI => { self.multi = [self.data8, value as u16, 0xFFFF, 0xFFFF]; self.id = 0; }
            XFER_N32 => self.data32 = value,
            _ => self.data8 = (self.data8 & 0xFF00) | (value as u16 & 0xFF),
        }
        self.done();
    }

    /// A serial IRQ is due (taken by the bus).
    pub fn take_irq(&mut self) -> bool { std::mem::take(&mut self.irq) }
}

impl crate::gameboy::gb_netlink::NetPort for Sio {
    fn outgoing(&self) -> Option<(u8, u32)> { self.awaiting }
    fn local_state(&self) -> u8 { self.in_multi() as u8 }
    fn set_remote_state(&mut self, state: u8) { self.partner_multi = state & 1 != 0; }
    fn external(&mut self, mode: u8, value: u32) -> u32 { Sio::external(self, mode, value) }
    fn finish(&mut self, value: u32) { Sio::finish(self, value) }
    fn unplugged(&mut self) { Sio::unplugged(self) }
}
