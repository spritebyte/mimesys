//! Serial (Microwire) EEPROMs: ER5911 and the 93Cxx family. After MAME's eepromser.cpp
//! (Aaron Giles). The board drives CS, CLK and DI and reads DO (and READY on the ER5911).
//!
//! Protocol: with CS high, a 1 start bit, then 2 opcode bits + address, clocked in on the
//! rising CLK edge. 10 = read (data shifts out MSB first on the next clocks), 01 = write
//! (data bits follow), 11 = erase (a write on the ER5911), 00 + top two address bits =
//! lock / write-all (not ER5911) / erase-all / unlock. Writes take time: READY reads low
//! until they finish. A CLK edge in the same board write that raised CS is ignored.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind { Er5911, C93xx }

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum State { InReset, WaitStart, WaitCommand, Reading, WaitData, WaitDone }

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Cmd { Read, Write, WriteAll, Erase, EraseAll, Lock, Unlock, Invalid }

#[derive(Clone, Serialize, Deserialize)]
pub struct EepromSerial {
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
    kind: Kind,
    data_bits: u32,      // 8 or 16
    addr_bits: u32,      // command address bits
    cells: usize,
    state: State,
    cs: bool,
    clk: bool,
    di: bool,
    locked: bool,
    bits: u32,
    accum: u32,
    command: Cmd,
    address: u32,
    shift: u32,
    cs_rise_time: u64,
    done_time: u64,
    /// time units a write / erase takes, and a write-all / erase-all
    write_ticks: u64,
    all_ticks: u64,
    #[serde(skip)]
    pub dirty: bool,
}

impl EepromSerial {
    /// ER5911 in 8-bit mode (128 bytes, 9 address bits). `ticks_per_ms`: the unit of the
    /// times passed to the line functions.
    pub fn er5911_8bit(ticks_per_ms: u64) -> Self {
        Self::new(Kind::Er5911, 128, 8, 9, ticks_per_ms)
    }

    pub fn new(kind: Kind, cells: usize, data_bits: u32, addr_bits: u32, ticks_per_ms: u64) -> Self {
        let bytes = cells * data_bits as usize / 8;
        Self {
            data: vec![0xFF; bytes], kind, data_bits, addr_bits, cells,
            state: State::InReset, cs: false, clk: false, di: false, locked: true,
            bits: 0, accum: 0, command: Cmd::Invalid, address: 0, shift: 0,
            cs_rise_time: 0, done_time: 0,
            write_ticks: ticks_per_ms * 1750 / 1000,
            all_ticks: ticks_per_ms * 8,
            dirty: false,
        }
    }

    /// Default contents (MAME's .nv file); short data leaves the rest erased.
    pub fn load(&mut self, d: &[u8]) {
        for (i, b) in self.data.iter_mut().enumerate() { *b = d.get(i).copied().unwrap_or(0xFF); }
    }

    fn cell(&self, a: u32) -> u32 {
        let a = a as usize % self.cells;
        if self.data_bits == 16 { (self.data[a * 2] as u32) << 8 | self.data[a * 2 + 1] as u32 }
        else { self.data[a] as u32 }
    }

    fn set_cell(&mut self, a: u32, v: u32) {
        let a = a as usize % self.cells;
        if self.data_bits == 16 {
            self.data[a * 2] = (v >> 8) as u8;
            self.data[a * 2 + 1] = v as u8;
        } else {
            self.data[a] = v as u8;
        }
        self.dirty = true;
    }

    pub fn ready(&self, now: u64) -> bool { now >= self.done_time }

    pub fn do_read(&self, now: u64) -> bool {
        match self.state {
            State::Reading => self.shift & 0x8000_0000 != 0,
            // 93Cxx: DO doubles as READY while waiting for the next start bit
            State::WaitStart if self.kind == Kind::C93xx => self.ready(now),
            _ => true,
        }
    }

    pub fn cs_write(&mut self, on: bool, now: u64) {
        if on == self.cs { return; }
        self.cs = on;
        if on {
            self.cs_rise_time = now;
            if self.state == State::InReset { self.state = State::WaitStart; }
        } else {
            self.state = State::InReset;
        }
    }

    pub fn di_write(&mut self, on: bool) { self.di = on; }

    pub fn clk_write(&mut self, on: bool, now: u64) {
        if on == self.clk { return; }
        self.clk = on;
        if !on { return; }   // everything happens on the rising edge
        let di = self.di as u32;
        match self.state {
            State::WaitStart => {
                if self.di && self.ready(now) && now > self.cs_rise_time {
                    self.accum = 0;
                    self.bits = 0;
                    self.state = State::WaitCommand;
                }
            }
            State::WaitCommand => {
                self.accum = (self.accum << 1) | di;
                self.bits += 1;
                if self.bits == 2 + self.addr_bits { self.execute(now); }
            }
            State::Reading => {
                let i = self.bits;
                self.bits += 1;
                self.shift = if i % self.data_bits == 0 && i == 0 {
                    self.cell(self.address) << (32 - self.data_bits)
                } else {
                    (self.shift << 1) | 1
                };
            }
            State::WaitData => {
                self.shift = (self.shift << 1) | di;
                self.bits += 1;
                if self.bits == self.data_bits { self.execute_write(now); }
            }
            State::InReset | State::WaitDone => {}
        }
    }

    fn execute(&mut self, now: u64) {
        let ab = self.addr_bits;
        self.address = self.accum & ((1 << ab) - 1);
        self.command = match self.accum >> ab {
            0 => {
                let c = match self.address >> (ab - 2) {
                    0 => Cmd::Lock,
                    1 => if self.kind == Kind::Er5911 { Cmd::Invalid } else { Cmd::WriteAll },
                    2 => Cmd::EraseAll,
                    _ => Cmd::Unlock,
                };
                self.address = 0;
                c
            }
            1 => Cmd::Write,
            2 => Cmd::Read,
            _ => if self.kind == Kind::Er5911 { Cmd::Write } else { Cmd::Erase },
        };
        self.bits = 0;
        self.state = match self.command {
            Cmd::Read => { self.shift = 0; State::Reading }
            Cmd::Write | Cmd::WriteAll => { self.shift = 0; State::WaitData }
            Cmd::Erase if !self.locked => {
                self.set_cell(self.address, !0);
                self.done_time = now + self.write_ticks;
                State::WaitDone
            }
            Cmd::EraseAll if !self.locked => {
                for a in 0..self.cells as u32 { self.set_cell(a, !0); }
                self.done_time = now + self.all_ticks;
                State::WaitDone
            }
            Cmd::Lock => { self.locked = true; State::InReset }
            Cmd::Unlock => { self.locked = false; State::InReset }
            _ => State::InReset,
        };
    }

    fn execute_write(&mut self, now: u64) {
        if self.locked { self.state = State::InReset; return; }
        let v = self.shift;
        if self.command == Cmd::WriteAll {
            for a in 0..self.cells as u32 { let old = self.cell(a); self.set_cell(a, old & v); }
            self.done_time = now + self.all_ticks;
        } else {
            self.set_cell(self.address, v);
            self.done_time = now + self.write_ticks;
        }
        self.state = State::WaitDone;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Host { e: EepromSerial, t: u64 }
    impl Host {
        fn tick(&mut self) -> u64 { self.t += 10; self.t }
        fn bit(&mut self, b: bool) {
            let t = self.tick();
            self.e.di_write(b);
            self.e.clk_write(false, t);
            let t = self.tick();
            self.e.clk_write(true, t);
        }
        fn send(&mut self, v: u32, n: u32) { for i in (0..n).rev() { self.bit(v >> i & 1 != 0); } }
        fn select(&mut self) { let t = self.tick(); self.e.cs_write(false, t); let t = self.tick(); self.e.cs_write(true, t); }
        fn read(&mut self, a: u32) -> u8 {
            self.select();
            self.send(1, 1);
            self.send(0b10 << 9 | a, 11);
            let mut v = 0;
            for _ in 0..8 {
                self.bit(false);
                v = v << 1 | self.e.do_read(self.t) as u8;
            }
            v
        }
    }

    #[test]
    fn er5911_read_write_lock() {
        let mut h = Host { e: EepromSerial::er5911_8bit(3000), t: 0 };
        h.e.load(&[0x12, 0x34]);
        assert_eq!(h.read(1), 0x34);
        // locked: write ignored
        h.select(); h.send(1, 1); h.send(0b01 << 9 | 5, 11); h.send(0xA5, 8);
        assert_eq!(h.read(5), 0xFF);
        // unlock (00 11xxxxxxx), write, wait for ready
        h.select(); h.send(1, 1); h.send(0b0011 << 7, 11);
        h.select(); h.send(1, 1); h.send(0b01 << 9 | 5, 11); h.send(0xA5, 8);
        assert!(!h.e.ready(h.t), "busy after write");
        h.t += 6000;
        assert!(h.e.ready(h.t));
        assert_eq!(h.read(5), 0xA5);
        assert!(h.e.dirty);
    }
}
