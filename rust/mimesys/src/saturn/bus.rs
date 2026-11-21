//! The Saturn's memory map, as both SH-2s see it (27 address bits; the SH-2's
//! cache-through mirror at +0x20000000 is handled by the chip).
//!
//! | 0x00000000 | BIOS, 512 KB                  | 0x05A00000 | SCSP sound RAM, 512 KB |
//! | 0x00100000 | SMPC (odd bytes)              | 0x05B00000 | SCSP registers         |
//! | 0x00180000 | backup RAM, 32 KB (odd bytes) | 0x05C00000 | VDP1 VRAM, 512 KB      |
//! | 0x00200000 | work RAM low, 1 MB            | 0x05C80000 | VDP1 frame buffer      |
//! | 0x01000000 | SINIT (the slave's FRT input)  | 0x05D00000 | VDP1 registers        |
//! | 0x01800000 | MINIT (the master's FRT input) | 0x05E00000 | VDP2 VRAM, 512 KB      |
//! | 0x02000000 | A-bus: cartridge (none yet)   | 0x05F00000 | VDP2 colour RAM, 4 KB  |
//! | 0x05800000 | CD block                      | 0x05F80000 | VDP2 registers         |
//! | 0x06000000 | work RAM high, 1 MB           | 0x05FE0000 | SCU registers          |

use super::{cdblock::CdBlock, scsp::Scsp, scu::Scu, smpc::Smpc, vdp1::Vdp1, vdp2::Vdp2};
use crate::m32x::sh7604::ExtBus;
use serde::{Deserialize, Serialize};

pub const BIOS_SIZE: usize = 0x8_0000;
pub const RAM_SIZE: usize = 0x10_0000;
/// 32 KB of backup RAM, then 8 bytes: "SMPC", SMEM 0-3 (the SMPC's battery memory)
pub const BACKUP_SIZE: usize = 0x8000;
pub const SRAM_SIZE: usize = BACKUP_SIZE + 8;

#[derive(Clone, Serialize, Deserialize)]
pub struct Bus {
    #[serde(skip)]
    pub bios: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub lwram: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub hwram: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub backup: Vec<u8>,
    pub smpc: Smpc,
    pub scu: Scu,
    pub vdp1: Vdp1,
    pub vdp2: Vdp2,
    pub scsp: Scsp,
    pub cd: CdBlock,
    /// MINIT / SINIT were written: input capture on the master (0) / slave (1)
    #[serde(skip)]
    pub capture: [bool; 2],
    #[serde(skip)]
    pub backup_dirty: bool,
    /// the debug log: how often each address is read (one frame in 300)
    #[serde(skip)]
    pub prof: Option<std::collections::HashMap<u32, u32>>,
    /// the debug log: SCU DMA transfers started
    #[serde(skip)]
    pub dma_log: Vec<String>,
}

#[inline]
fn rd16(m: &[u8], a: usize) -> u16 { u16::from_be_bytes([m[a], m[a + 1]]) }

#[inline]
fn wr16(m: &mut [u8], a: usize, v: u16, upper: bool, lower: bool) {
    if upper { m[a] = (v >> 8) as u8; }
    if lower { m[a + 1] = v as u8; }
}

fn lanes(upper: bool, lower: bool) -> u16 { (if upper { 0xFF00 } else { 0 }) | (if lower { 0xFF } else { 0 }) }

impl Bus {
    pub fn new(bios: Vec<u8>, region: u8, pal: bool) -> Self {
        let backup = vec![0u8; SRAM_SIZE];
        Self {
            bios, lwram: vec![0; RAM_SIZE], hwram: vec![0; RAM_SIZE], backup,
            smpc: Smpc::new(region), scu: Scu::new(), vdp1: Vdp1::new(), vdp2: Vdp2::new(pal),
            scsp: Scsp::new(), cd: CdBlock::new(None), capture: [false; 2], backup_dirty: false, prof: None, dma_log: Vec::new(),
        }
    }

    pub fn read16(&mut self, a: u32) -> u16 {
        let a = a & 0x07FF_FFFE;
        match a >> 20 {
            0x00 => rd16(&self.bios, a as usize & (BIOS_SIZE - 1)),
            0x01 => {
                if a < 0x0018_0000 {
                    0xFF00 | self.smpc.read((a & 0x7F) | 1) as u16
                } else {
                    0xFF00 | self.backup[(a as usize & 0xFFFF) >> 1] as u16
                }
            }
            0x02 => rd16(&self.lwram, a as usize & (RAM_SIZE - 1)),
            0x58 => self.cd.read(a & 0xF_FFFF),
            0x5A => rd16(&self.scsp.ram, a as usize & 0x7_FFFF),
            0x5B => self.scsp.read_reg(a & 0xFFF),
            0x5C => {
                if a < 0x05C8_0000 { rd16(&self.vdp1.vram, a as usize & 0x7_FFFF) } else { rd16(self.vdp1.fb(), a as usize & 0x3_FFFF) }
            }
            0x5D => self.vdp1.read_reg(a),
            0x5E => rd16(&self.vdp2.vram, a as usize & 0x7_FFFF),
            0x5F => {
                if a < 0x05F8_0000 {
                    rd16(&self.vdp2.cram, a as usize & 0xFFF)
                } else if a < 0x05FE_0000 {
                    self.vdp2.read_reg(a & 0x1FF)
                } else if a < 0x05FF_0000 && (0x80..0x90).contains(&(a & 0xFC)) {
                    self.scu.dsp.read(a & 0xFC, a & 2 != 0)
                } else if a < 0x05FF_0000 {
                    let v = self.scu.read(a & 0xFC);
                    if a & 2 == 0 { (v >> 16) as u16 } else { v as u16 }
                } else {
                    0xFFFF
                }
            }
            0x60..=0x7F => rd16(&self.hwram, a as usize & (RAM_SIZE - 1)),
            _ => 0xFFFF,
        }
    }

    pub fn write16(&mut self, a: u32, v: u16, upper: bool, lower: bool) {
        let a = a & 0x07FF_FFFE;
        match a >> 20 {
            0x01 => {
                if !lower { return; }
                if a < 0x0018_0000 {
                    self.smpc.write((a & 0x7F) | 1, v as u8);
                    if self.smpc.smem[4] != 0 {
                        // SETSMEM was used: keep the battery memory with the backup RAM
                        let mut t = *b"SMPC\0\0\0\0";
                        t[4..].copy_from_slice(&self.smpc.smem[..4]);
                        if self.backup[BACKUP_SIZE..] != t {
                            self.backup[BACKUP_SIZE..].copy_from_slice(&t);
                            self.backup_dirty = true;
                        }
                    }
                } else {
                    let i = (a as usize & 0xFFFF) >> 1;
                    if self.backup[i] != v as u8 {
                        self.backup[i] = v as u8;
                        self.backup_dirty = true;
                    }
                }
            }
            0x02 => wr16(&mut self.lwram, a as usize & (RAM_SIZE - 1), v, upper, lower),
            // SINIT (0x01000000) signals the slave, MINIT (0x01800000) the master
            0x10..=0x17 => self.capture[1] = true,
            0x18..=0x1F => self.capture[0] = true,
            0x58 => self.cd.write(a & 0xF_FFFF, v),
            0x5A => wr16(&mut self.scsp.ram, a as usize & 0x7_FFFF, v, upper, lower),
            0x5B => self.scsp.write_reg(a & 0xFFF, v, lanes(upper, lower)),
            0x5C => {
                if a < 0x05C8_0000 {
                    wr16(&mut self.vdp1.vram, a as usize & 0x7_FFFF, v, upper, lower)
                } else {
                    wr16(self.vdp1.fb(), a as usize & 0x3_FFFF, v, upper, lower)
                }
            }
            0x5D => self.vdp1.write_reg(a, v),
            0x5E => wr16(&mut self.vdp2.vram, a as usize & 0x7_FFFF, v, upper, lower),
            0x5F => {
                if a < 0x05F8_0000 {
                    wr16(&mut self.vdp2.cram, a as usize & 0xFFF, v, upper, lower)
                } else if a < 0x05FE_0000 {
                    self.vdp2.write_reg(a & 0x1FF, v, lanes(upper, lower))
                } else if a < 0x05FF_0000 && (0x80..0x90).contains(&(a & 0xFC)) {
                    self.scu.dsp.write(a & 0xFC, v, a & 2 != 0);
                } else if a < 0x05FF_0000 {
                    let m = lanes(upper, lower) as u32;
                    let (v, m) = if a & 2 == 0 { ((v as u32) << 16, m << 16) } else { (v as u32, m) };
                    self.scu.write(a & 0xFC, v, m);
                    if self.scu.start != 0 { self.run_dma(); }
                }
            }
            0x60..=0x7F => wr16(&mut self.hwram, a as usize & (RAM_SIZE - 1), v, upper, lower),
            _ => {}
        }
    }

    pub fn read32(&mut self, a: u32) -> u32 { (self.read16(a) as u32) << 16 | self.read16(a + 2) as u32 }

    /// Which bus an address is on, for SCU DMA's rules: 0 A-bus, 1 B-bus, 2 C-bus (work RAM high)
    fn bus_of(a: u32) -> u8 {
        match a & 0x07FF_FFFF {
            0x0600_0000..=0x07FF_FFFF => 2,
            0x05A0_0000..=0x05FE_FFFF => 1,
            _ => 0,
        }
    }

    /// Run the SCU DMA levels that were started (stage 1: all at once)
    pub fn run_dma(&mut self) {
        while self.scu.start != 0 {
            let level = self.scu.start.trailing_zeros() as usize;
            self.scu.start &= !(1 << level);
            let d = self.scu.dma[level].clone();
            if super::debug() && self.dma_log.len() < 48 {
                self.dma_log.push(format!("L{level} {} src {:08X} dst {:08X} count {:X} add {}/{} factor {}{}{}",
                    if d.indirect { "indirect" } else { "direct" }, d.src, d.dst, d.count, d.src_add, d.dst_add, d.factor,
                    if d.rup { " rup" } else { "" }, if d.wup { " wup" } else { "" }));
            }
            if d.indirect {
                let mut table = d.dst;
                for _ in 0..4096 {
                    let count = self.read32(table) & if level == 0 { 0xF_FFFF } else { 0x3_FFFF };
                    let dst = self.read32(table + 4);
                    let src = self.read32(table + 8);
                    if super::debug() && self.dma_log.len() < 48 {
                        self.dma_log.push(format!("   entry at {table:08X}: count {count:X} dst {dst:08X} src {src:08X}"));
                    }
                    table = table.wrapping_add(12);
                    self.transfer(src & 0x07FF_FFFF, dst & 0x07FF_FFFF, count, d.src_add, d.dst_add);
                    if src & 0x8000_0000 != 0 { break; }
                }
                if d.wup { self.scu.dma[level].dst = table; }
            } else {
                let count = if d.count == 0 { if level == 0 { 0x10_0000 } else { 0x1000 } } else { d.count };
                let (s, t) = self.transfer(d.src, d.dst, count, d.src_add, d.dst_add);
                if d.rup { self.scu.dma[level].src = s; }
                if d.wup { self.scu.dma[level].dst = t; }
            }
            // DMA end: level 0 bit 11, level 1 bit 10, level 2 bit 9
            self.scu.ist |= 1 << (11 - level);
        }
    }

    /// Move `count` bytes as 16-bit words; returns the final addresses
    fn transfer(&mut self, mut src: u32, mut dst: u32, count: u32, src_add: u32, dst_add: u32) -> (u32, u32) {
        // writes to work RAM always step by 2; the source steps 0 or 2 per word
        let dstep = if Self::bus_of(dst) == 2 { 2 } else { dst_add };
        let sstep = src_add / 2;
        let words = count.div_ceil(2).max(1);
        for _ in 0..words {
            let v = self.read16(src);
            self.write16(dst, v, true, true);
            src = src.wrapping_add(sstep);
            dst = dst.wrapping_add(dstep);
        }
        (src, dst)
    }

    /// CPU clocks pass at `hz`: the timers and the slow devices
    pub fn tick(&mut self, cycles: u32, hz: u64) {
        let ps = cycles as i64 * 1_000_000_000_000 / hz as i64;
        self.scu.tick(cycles);
        self.smpc.tick(ps);
        self.cd.tick(ps);
        for a in self.smpc.actions.iter() {
            if *a == super::smpc::Action::Irq { self.scu.ist |= super::scu::IST_SMPC; }
        }
        self.smpc.actions.retain(|a| *a != super::smpc::Action::Irq);
        if std::mem::take(&mut self.scsp.main_irq) {
            self.scu.raise(super::scu::IST_SOUND, Some(super::scu::Event::Sound));
        }
        if std::mem::take(&mut self.vdp1.ended) {
            self.scu.raise(super::scu::IST_SPRITE_END, Some(super::scu::Event::SpriteEnd));
        }
        if self.scu.start != 0 { self.run_dma(); }
        if self.scu.dsp.running() {
            let mut d = std::mem::replace(&mut self.scu.dsp, super::scudsp::Dsp::empty());
            d.run(cycles, self);
            let irq = std::mem::take(&mut d.irq);
            self.scu.dsp = d;
            if irq { self.scu.raise(super::scu::IST_DSP_END, None); }
            if self.scu.start != 0 { self.run_dma(); }
        }
    }
}

impl super::scudsp::DspBus for Bus {
    fn read16(&mut self, a: u32) -> u16 { Bus::read16(self, a) }
    fn write16(&mut self, a: u32, v: u16) { Bus::write16(self, a, v, true, true) }
}

/// One SH-2's view of the bus. It counts rough wait states per 16-bit access (BIOS and
/// work RAM low 3, work RAM high 1, the chips 6); cache hits cost none.
pub struct Ext<'a> {
    pub bus: &'a mut Bus,
    pub waits: u32,
}

impl ExtBus for Ext<'_> {
    #[inline]
    fn read(&mut self, a: u32) -> u16 {
        if let Some(p) = &mut self.bus.prof { *p.entry(a).or_default() += 1; }
        // the memories (the hot paths) straight away
        match (a >> 20) & 0x7F {
            0x00 => { self.waits += 3; rd16(&self.bus.bios, a as usize & (BIOS_SIZE - 2)) }
            0x02 => { self.waits += 3; rd16(&self.bus.lwram, a as usize & (RAM_SIZE - 2)) }
            0x60..=0x7F => { self.waits += 1; rd16(&self.bus.hwram, a as usize & (RAM_SIZE - 2)) }
            _ => { self.waits += 6; self.bus.read16(a) }
        }
    }

    fn read_line(&mut self, a: u32, line: &mut [u8; 16]) {
        let m = match a & 0x07F0_0000 {
            0x0000_0000 => Some((&self.bus.bios, BIOS_SIZE)),
            0x0020_0000 => Some((&self.bus.lwram, RAM_SIZE)),
            x if x >= 0x0600_0000 => Some((&self.bus.hwram, RAM_SIZE)),
            _ => None,
        };
        match m {
            Some((mem, size)) => {
                let i = a as usize & (size - 16);
                line.copy_from_slice(&mem[i..i + 16]);
            }
            None => for k in 0..8 { line[k * 2..k * 2 + 2].copy_from_slice(&self.bus.read16(a + k as u32 * 2).to_be_bytes()); },
        }
    }

    #[inline]
    fn write(&mut self, a: u32, v: u16, upper: bool, lower: bool) {
        if (a >> 20) & 0x7F >= 0x60 {
            self.waits += 1;
            wr16(&mut self.bus.hwram, a as usize & (RAM_SIZE - 2), v, upper, lower)
        } else {
            self.waits += if (a >> 20) & 0x7F == 0x02 { 3 } else { 6 };
            self.bus.write16(a, v, upper, lower)
        }
    }
}
