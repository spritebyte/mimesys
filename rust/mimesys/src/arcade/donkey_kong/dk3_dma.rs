//! Zilog Z80 DMA, as Donkey Kong 3 uses it: memory-to-memory block copies (sprite RAM),
//! started by the board's RDY line (7E85). After MAME z80dma.cpp (BSD-3-Clause).
//!
//! Registers are written one byte at a time to I/O port 0: a base byte picks WR0-WR6 and
//! says which parameter bytes follow. A transfer copies length + 1 bytes. It runs
//! instantly here (the CPU is held off the bus during it on the real board).

use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Z80Dma {
    /// WR0-WR6 base bytes
    wr: [u8; 7],
    port_a: u16,
    port_b: u16,
    block_len: u16,
    port_a_timing: u8,
    port_b_timing: u8,
    mask_byte: u8,
    match_byte: u8,
    int_ctrl: u8,
    pulse_ctrl: u8,
    int_vector: u8,
    read_mask: u8,
    /// parameter bytes still to come: (WRn, index)
    follow: Vec<(u8, u8)>,
    // working copies, loaded by the LOAD command
    addr_a: u16,
    addr_b: u16,
    enabled: bool,
    force_ready: bool,
    rdy: bool,
    /// an auto-restarted transfer waits for RDY to drop before running again
    wait_rdy_low: bool,
}

/// What a transfer reads from and writes to.
pub trait DmaMemory {
    fn dma_read(&mut self, addr: u16) -> u8;
    fn dma_write(&mut self, addr: u16, v: u8);
}

impl Z80Dma {
    pub fn new() -> Self { Self::default() }

    fn port_a_is_source(&self) -> bool { self.wr[0] & 0x04 != 0 }
    fn ready_active_high(&self) -> bool { self.wr[5] & 0x08 != 0 }
    fn auto_restart(&self) -> bool { self.wr[5] & 0x20 != 0 }

    fn step(addr: u16, wr: u8) -> u16 {
        // bit 5 set = fixed address; else bit 4 set = increment, clear = decrement
        if wr & 0x20 != 0 { addr } else if wr & 0x10 != 0 { addr.wrapping_add(1) } else { addr.wrapping_sub(1) }
    }

    pub fn write(&mut self, data: u8, mem: &mut dyn DmaMemory) {
        if let Some(&(reg, idx)) = self.follow.first() {
            self.follow.remove(0);
            match (reg, idx) {
                (0, 1) => self.port_a = (self.port_a & 0xFF00) | data as u16,
                (0, 2) => self.port_a = (self.port_a & 0x00FF) | (data as u16) << 8,
                (0, 3) => self.block_len = (self.block_len & 0xFF00) | data as u16,
                (0, 4) => self.block_len = (self.block_len & 0x00FF) | (data as u16) << 8,
                (1, 1) => self.port_a_timing = data,
                (2, 1) => self.port_b_timing = data,
                (3, 1) => self.mask_byte = data,
                (3, 2) => self.match_byte = data,
                (4, 1) => self.port_b = (self.port_b & 0xFF00) | data as u16,
                (4, 2) => self.port_b = (self.port_b & 0x00FF) | (data as u16) << 8,
                (4, 3) => {
                    self.int_ctrl = data;
                    // interrupt control byte: its own followers
                    self.follow.clear();
                    if data & 0x08 != 0 { self.follow.push((4, 5)); }
                    if data & 0x10 != 0 { self.follow.push((4, 4)); }
                }
                (4, 4) => self.int_vector = data,
                (4, 5) => self.pulse_ctrl = data,
                (6, 1) => self.read_mask = data,
                _ => {}
            }
            return;
        }
        let f = &mut self.follow;
        if data & 0x87 == 0x00 {
            self.wr[2] = data;
            if data & 0x40 != 0 { f.push((2, 1)); }
        } else if data & 0x87 == 0x04 {
            self.wr[1] = data;
            if data & 0x40 != 0 { f.push((1, 1)); }
        } else if data & 0x80 == 0 {
            self.wr[0] = data;
            if data & 0x08 != 0 { f.push((0, 1)); }
            if data & 0x10 != 0 { f.push((0, 2)); }
            if data & 0x20 != 0 { f.push((0, 3)); }
            if data & 0x40 != 0 { f.push((0, 4)); }
        } else if data & 0x83 == 0x80 {
            self.wr[3] = data;
            if data & 0x08 != 0 { f.push((3, 1)); }
            if data & 0x10 != 0 { f.push((3, 2)); }
            if data & 0x40 != 0 { self.enabled = true; self.run(mem); }
        } else if data & 0x83 == 0x81 {
            self.wr[4] = data;
            if data & 0x04 != 0 { f.push((4, 1)); }
            if data & 0x08 != 0 { f.push((4, 2)); }
            if data & 0x10 != 0 { f.push((4, 3)); }
        } else if data & 0xC7 == 0x82 {
            self.wr[5] = data;
        } else if data & 0x83 == 0x83 {
            self.wr[6] = data;
            match data {
                0xC3 => { self.enabled = false; self.force_ready = false; }      // reset
                0xCF => {                                                       // load
                    self.force_ready = false;
                    self.addr_a = self.port_a;
                    self.addr_b = self.port_b;
                }
                0xD3 => {}                                                      // continue
                0xB3 => { self.force_ready = true; self.run(mem); }             // force ready
                0x83 => self.enabled = false,                                   // disable
                0x87 => { self.enabled = true; self.run(mem); }                 // enable
                0xBB => f.push((6, 1)),                                         // read mask follows
                _ => {}
            }
        }
    }

    /// The board's RDY line (7E85 bit 0).
    pub fn set_rdy(&mut self, on: bool, mem: &mut dyn DmaMemory) {
        self.rdy = on;
        if on != self.ready_active_high() { self.wait_rdy_low = false; }
        self.run(mem);
    }

    fn ready(&self) -> bool {
        self.force_ready || (self.rdy == self.ready_active_high() && !self.wait_rdy_low)
    }

    fn run(&mut self, mem: &mut dyn DmaMemory) {
        if !self.enabled || !self.ready() { return; }
        let count = self.block_len as u32 + 1;
        let a_src = self.port_a_is_source();
        for _ in 0..count {
            let (src, dst) = if a_src { (self.addr_a, self.addr_b) } else { (self.addr_b, self.addr_a) };
            let v = mem.dma_read(src);
            // only search modes don't write; DK3 uses plain transfers
            if self.wr[0] & 0x03 != 0x02 { mem.dma_write(dst, v); }
            self.addr_a = Self::step(self.addr_a, self.wr[1]);
            self.addr_b = Self::step(self.addr_b, self.wr[2]);
        }
        if self.auto_restart() {
            self.addr_a = self.port_a;
            self.addr_b = self.port_b;
            self.wait_rdy_low = !self.force_ready;
        } else {
            self.enabled = false;
        }
        self.force_ready = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Ram(Vec<u8>);
    impl DmaMemory for Ram {
        fn dma_read(&mut self, a: u16) -> u8 { self.0[a as usize] }
        fn dma_write(&mut self, a: u16, v: u8) { self.0[a as usize] = v; }
    }

    #[test]
    fn block_copy_on_rdy() {
        let mut m = Ram((0..=255u8).cycle().take(0x10000).collect());
        m.0[0x7000..0x7400].fill(0xEE);
        let mut d = Z80Dma::new();
        // WR0: A -> B transfer, port A address 6900, length 0x17F (0x180 bytes)
        for b in [0x7D, 0x00, 0x69, 0x7F, 0x01,
                  0x14,             // WR1: port A memory, increment
                  0x10,             // WR2: port B memory, increment
                  0xAD, 0x00, 0x70, // WR4: continuous, port B address 7000
                  0x8A,             // WR5: RDY active high
                  0xCF, 0x87] {     // load, enable
            d.write(b, &mut m);
        }
        assert_eq!(m.0[0x7000], 0xEE, "nothing until RDY");
        d.set_rdy(true, &mut m);
        assert_eq!(m.0[0x7000], m.0[0x6900]);
        assert_eq!(m.0[0x717F], m.0[0x6A7F]);
        assert_eq!(m.0[0x7180], 0xEE, "length + 1 bytes");
    }
}
