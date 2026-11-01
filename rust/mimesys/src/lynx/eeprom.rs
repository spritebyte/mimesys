//! Cartridge EEPROM (93C46 / 56 / 66 / 76 / 86 serial EEPROMs, as on newer Lynx carts),
//! after Handy's eeprom.cpp (zlib).
//!
//! Wiring: the clock is bit 1 of the cart's address counter, chip select is bit 7 of
//! it; data in is IODAT bit 4 (AUDIN) while IODIR bit 4 makes it an output, and the
//! data out comes back on the same pin when it's an input.
//! Commands (start bit 1, then 2 opcode bits and the address): 10 read, 01 write,
//! 11 erase, 00 + 11.. write enable, 00 + 00.. write disable. Busy / ready shows on
//! data out after a write.
//!
//! Type byte (from the .lnx header): bits 0-2 the chip (1 = 93C46 ... 5 = 93C86), bit 7
//! organised as bytes (else 16-bit words).

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum St { None, Start, Data, Busy, Wait }

#[derive(Clone, Serialize, Deserialize)]
pub struct Eeprom {
    pub kind: u8,
    addr_mask: u16,
    cmd_bits: u32,
    addr_bits: u32,
    done_mask: u32,
    /// the contents (16-bit words are stored low byte first)
    #[serde(with = "serde_bytes")]
    pub mem: Vec<u8>,
    state: St,
    busy_count: u32,
    readdata: u32,
    data: u32,
    addr: u16,
    sendbits: u32,
    readonly: bool,
    counter: u16,
    iodir: u8,
    iodat: u8,
    pub out_bit: bool,
    #[serde(skip)]
    pub dirty: bool,
}

impl Eeprom {
    /// `kind` 0 = no EEPROM.
    pub fn new(kind: u8) -> Self {
        let (mut mask, mut cmd, mut abits) = match kind & 7 {
            1 => (0x7Fu16, 10, 7),
            2 => (0xFF, 12, 9),
            3 => (0x1FF, 12, 9),
            4 => (0x3FF, 14, 11),
            5 => (0x7FF, 14, 11),
            _ => (0, 1, 1),
        };
        let done = if kind & 0x80 != 0 { 0x100 } else { mask >>= 1; cmd -= 1; abits -= 1; 0x10000 };
        Self { kind, addr_mask: mask, cmd_bits: cmd, addr_bits: abits, done_mask: done, mem: vec![0xFF; 2048],
               state: St::None, busy_count: 0, readdata: 0, data: 0, addr: 0, sendbits: 0, readonly: true,
               counter: 0, iodir: 0, iodat: 0, out_bit: false, dirty: false }
    }

    pub fn present(&self) -> bool { self.kind & 7 != 0 }

    /// Size in bytes.
    pub fn size(&self) -> usize {
        let m = self.addr_mask as usize + 1;
        if self.kind & 0x80 != 0 { m } else { m * 2 }
    }

    /// The contents as saved (the first `size()` bytes).
    pub fn bytes(&self) -> &[u8] { &self.mem[..self.size()] }

    pub fn load(&mut self, data: &[u8]) {
        let n = data.len().min(self.size());
        self.mem[..n].copy_from_slice(&data[..n]);
    }

    fn word(&self, a: u16) -> u16 { u16::from_le_bytes([self.mem[a as usize * 2], self.mem[a as usize * 2 + 1]]) }

    pub fn io(&mut self, iodir: u8, iodat: u8) { self.iodir = iodir; self.iodat = iodat; }

    /// Reading IODAT polls the busy state.
    pub fn busy(&mut self) {
        if self.state == St::Busy || self.state == St::None {
            if self.busy_count < 2 {
                self.busy_count += 1;
                self.readdata = 0;
                self.out_bit = false;
            } else {
                self.readdata = 0xFFFF;
                self.out_bit = true;
                self.state = St::Wait;
            }
        }
    }

    /// The cart's address counter changed.
    pub fn counter(&mut self, cnt: u16) {
        if !self.present() { return; }
        let before = self.counter & 2 != 0;
        self.counter = cnt;
        if before || cnt & 2 == 0 { return; }                      // only a rising clock edge
        self.out_bit = self.readdata & (self.done_mask >> 1) != 0;
        self.readdata <<= 1;
        let cs = cnt & 0x80 != 0;
        let di = self.iodir & 0x10 != 0 && self.iodat & 0x10 != 0;
        if !cs { self.state = St::None; }
        match self.state {
            St::None => {
                self.data = 0;
                if cs {
                    if di {
                        self.out_bit = false;
                        self.state = St::Start;
                        self.sendbits = self.cmd_bits - 1;
                    } else if self.iodir & 0x10 == 0 {
                        self.state = St::Busy;
                        self.readdata = 0;
                        self.out_bit = false;
                        self.busy_count = 0;
                    }
                }
            }
            St::Start => {
                self.data = self.data << 1 | di as u32;
                self.sendbits -= 1;
                if self.sendbits > 0 { return; }
                self.state = St::None;
                self.addr = (self.data as u16) & self.addr_mask;
                match self.data >> self.addr_bits {
                    3 => if !self.readonly { self.write_cell(0xFFFF); },
                    2 => {
                        self.readdata = if self.kind & 0x80 != 0 { self.mem[self.addr as usize] as u32 } else { self.word(self.addr) as u32 };
                        self.out_bit = false;
                        self.state = St::Wait;
                    }
                    1 => { self.data = 1; self.state = St::Data; }
                    _ => match self.data >> (self.addr_bits - 2) {
                        0 => self.readonly = true,
                        3 => self.readonly = false,
                        _ => {}
                    },
                }
            }
            St::Data => {
                self.data = self.data << 1 | di as u32;
                if self.data & self.done_mask != 0 {
                    if !self.readonly { self.write_cell(self.data); }
                    self.busy_count = 0;
                    self.readdata = 0;
                    self.out_bit = false;
                    self.state = St::Wait;
                }
            }
            St::Busy | St::Wait => {}
        }
    }

    fn write_cell(&mut self, v: u32) {
        if self.kind & 0x80 != 0 {
            self.mem[self.addr as usize] = v as u8;
        } else {
            let a = self.addr as usize * 2;
            self.mem[a..a + 2].copy_from_slice(&(v as u16).to_le_bytes());
        }
        self.dirty = true;
    }
}
