//! The SPU (1F801C00-1F801FFF), from Nocash PSX-SPX. This first part is the register file
//! and the transfer path into its 512 KB of RAM, so the BIOS can set it up; the voices,
//! ADSR and reverb are stage 6 of the PS1 plan.
//!
//! - SPUSTAT (1DAE) bits 0-5 follow SPUCNT (1DAA) bits 0-5; bit 10 "busy" stays clear.
//! - Manual writes: the transfer address (1DA6, in 8-byte units), then halfwords to the
//!   FIFO (1DA8); DMA channel 4 writes / reads whole words the same way.

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct Spu {
    pub regs: Vec<u16>,
    pub ram: Vec<u8>,
    addr: u32,
    pub irq: bool,
}

impl Default for Spu {
    fn default() -> Self { Self::new() }
}

impl Spu {
    pub fn new() -> Self { Self { regs: vec![0; 0x200], ram: vec![0; 0x8_0000], addr: 0, irq: false } }

    pub fn read16(&mut self, a: u32) -> u16 {
        let r = ((a & 0x3FF) >> 1) as usize;
        match a & 0x3FF {
            0x1AE => (self.regs[0x1AA / 2] & 0x3F) | (self.regs[0x1AA / 2] & 0x20) << 2,
            0x1A6 => (self.addr / 8) as u16,
            _ => self.regs[r],
        }
    }

    pub fn write16(&mut self, a: u32, v: u16) {
        let r = ((a & 0x3FF) >> 1) as usize;
        self.regs[r] = v;
        match a & 0x3FF {
            0x1A6 => self.addr = (v as u32) * 8,
            0x1A8 => self.push(v),
            _ => {}
        }
    }

    fn push(&mut self, v: u16) {
        let a = (self.addr & 0x7FFFE) as usize;
        self.ram[a..a + 2].copy_from_slice(&v.to_le_bytes());
        self.addr = (self.addr + 2) & 0x7FFFF;
    }

    pub fn dma_write(&mut self, w: u32) {
        self.push(w as u16);
        self.push((w >> 16) as u16);
    }

    pub fn dma_read(&mut self) -> u32 {
        let a = (self.addr & 0x7FFFC) as usize;
        self.addr = (self.addr + 4) & 0x7FFFF;
        u32::from_le_bytes(self.ram[a..a + 4].try_into().unwrap())
    }

    /// Stage 6: the voices. Silence for now.
    pub fn sample(&mut self) -> (f32, f32) { (0.0, 0.0) }
}
