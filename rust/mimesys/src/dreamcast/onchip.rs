//! The SH7091's on-chip devices (the P4 area, FF000000-FFFFFFFF), from the Hitachi SH7750
//! manual and MAME devices/cpu/sh/sh4*.cpp (BSD-3-Clause):
//! - CCN: MMU / cache control, TRA / EXPEVT / INTEVT, store queue areas (QACR0/1)
//! - TMU: three 32-bit down-counters on the peripheral clock (50 MHz / 4, 16, 64, 256, 1024)
//! - INTC: IPRA-IPRD priorities for the on-chip sources; the IRL lines come from Holly
//! - DMAC: four channels (2 is Holly's PVR / TA channel; auto-request copies run at once)
//! - BSC: port A (PCTRA / PDTRA: the video cable type), refresh counters
//! - SCIF (serial port: what programs print is kept for the debug log), CPG, RTC, UBC:
//!   registers only
//! - store queues (E0000000-E3FFFFFF, flushed by PREF) and the operand-cache RAM
//!   (7C000000-7FFFFFFF with CCR.ORA)

use serde::{Deserialize, Serialize};

/// TMU prescaler (in peripheral clocks; one peripheral clock = 4 CPU cycles)
const TMU_DIV: [u64; 8] = [4, 16, 64, 256, 1024, 1024, 1024, 1024];

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Timer {
    pub tcor: u32,
    pub tcnt: u32,
    pub tcr: u16,
    /// CPU cycles not yet counted
    rem: u64,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct DmaChannel {
    pub sar: u32,
    pub dar: u32,
    pub dmatcr: u32,
    pub chcr: u32,
}

/// An auto-request DMA the bus has to carry out (the on-chip side can't reach memory).
pub struct DmaJob { pub channel: usize, pub src: u32, pub dst: u32, pub unit: u32, pub count: u32, pub sm: u32, pub dm: u32 }

#[derive(Clone, Serialize, Deserialize)]
pub struct OnChip {
    // CCN
    pub pteh: u32, pub ptel: u32, pub ttb: u32, pub tea: u32, pub mmucr: u32,
    pub basra: u8, pub basrb: u8, pub ccr: u32,
    pub tra: u32, pub expevt: u32, pub intevt: u32, pub ptea: u32,
    pub qacr: [u32; 2],
    // TMU
    pub tocr: u8,
    pub tstr: u8,
    pub tmu: [Timer; 3],
    pub tcpr2: u32,
    /// cycle the timers were last brought up to date
    tmu_at: u64,
    // INTC
    pub icr: u16,
    pub ipr: [u16; 4],
    // DMAC
    pub dma: [DmaChannel; 4],
    pub dmaor: u32,
    // BSC
    pub bcr1: u32, pub bcr2: u16, pub wcr1: u32, pub wcr2: u32, pub wcr3: u32, pub mcr: u32,
    pub pcr: u16, pub rtcsr: u16, pub rtcnt: u16, pub rtcor: u16, pub rfcr: u16,
    pub pctra: u32, pub pdtra: u16, pub pctrb: u32, pub pdtrb: u16, pub gpioic: u16,
    // CPG / watchdog
    pub frqcr: u16, pub stbcr: u8, pub wtcnt: u8, pub wtcsr: u8, pub stbcr2: u8,
    /// RTC, UBC, SCI, SCIF: plain registers, by address
    pub misc: std::collections::BTreeMap<u32, u32>,
    pub sq: [[u32; 8]; 2],
    /// 8 KB operand-cache RAM
    pub ocram: Vec<u8>,
    /// video cable on port A bits 8-9: 0 VGA, 2 RGB, 3 composite
    pub cable: u16,
    /// characters written to the serial port (debug log)
    #[serde(skip)]
    pub serial_out: String,
}

impl Default for OnChip {
    fn default() -> Self { Self::new() }
}

impl OnChip {
    pub fn new() -> Self {
        Self {
            pteh: 0, ptel: 0, ttb: 0, tea: 0, mmucr: 0, basra: 0, basrb: 0, ccr: 0,
            tra: 0, expevt: 0, intevt: 0, ptea: 0, qacr: [0; 2],
            tocr: 0, tstr: 0, tmu: Default::default(), tcpr2: 0, tmu_at: 0,
            icr: 0, ipr: [0; 4],
            dma: Default::default(), dmaor: 0,
            bcr1: 0, bcr2: 0x3FFC, wcr1: 0x7777_7777, wcr2: 0xFFFE_EFFF, wcr3: 0x0777_7777, mcr: 0,
            pcr: 0, rtcsr: 0, rtcnt: 0, rtcor: 0, rfcr: 0, pctra: 0, pdtra: 0, pctrb: 0, pdtrb: 0, gpioic: 0,
            frqcr: 0x0E0A, stbcr: 0, wtcnt: 0, wtcsr: 0, stbcr2: 0,
            misc: Default::default(), sq: [[0; 8]; 2], ocram: vec![0; 0x2000], cable: 3,
            serial_out: String::new(),
        }
    }

    pub fn reset(&mut self) {
        let cable = self.cable;
        *self = Self::new();
        self.cable = cable;
    }

    // ------------------------------------------------------------ timers

    /// Count the timers up to cycle `now`; returns true if one underflowed.
    pub fn tmu_update(&mut self, now: u64) -> bool {
        let elapsed = now.saturating_sub(self.tmu_at);
        self.tmu_at = now;
        let mut fired = false;
        for (i, t) in self.tmu.iter_mut().enumerate() {
            if self.tstr >> i & 1 == 0 { continue; }
            let per = TMU_DIV[(t.tcr & 7) as usize] * 4;
            t.rem += elapsed;
            let mut ticks = t.rem / per;
            t.rem %= per;
            while ticks > 0 {
                if ticks <= t.tcnt as u64 {
                    t.tcnt -= ticks as u32;
                    break;
                }
                // underflow: reload from TCOR, flag UNF
                ticks -= t.tcnt as u64 + 1;
                t.tcnt = t.tcor;
                t.tcr |= 0x100;
                fired = true;
                if t.tcor == 0 { break; }
                ticks %= t.tcor as u64 + 1;
            }
        }
        fired
    }

    /// CPU cycles until the next timer underflow (None: none running).
    pub fn tmu_next(&self) -> Option<u64> {
        (0..3).filter(|&i| self.tstr >> i & 1 != 0).map(|i| {
            let t = &self.tmu[i];
            let per = TMU_DIV[(t.tcr & 7) as usize] * 4;
            (t.tcnt as u64 + 1) * per - t.rem
        }).min()
    }

    // ------------------------------------------------------------ interrupts

    /// The on-chip interrupt to take: (priority level, INTEVT code).
    pub fn pending_irq(&self) -> Option<(u32, u32)> {
        let mut best: Option<(u32, u32)> = None;
        let mut offer = |level: u32, code: u32| {
            if level > 0 && best.map_or(true, |(l, _)| level > l) { best = Some((level, code)); }
        };
        for (i, t) in self.tmu.iter().enumerate() {
            if t.tcr & 0x120 == 0x120 { offer((self.ipr[0] >> (12 - 4 * i) & 15) as u32, 0x400 + 0x20 * i as u32); }
        }
        for (i, d) in self.dma.iter().enumerate() {
            if d.chcr & 6 == 6 { offer((self.ipr[2] >> 8 & 15) as u32, 0x640 + 0x20 * i as u32); }
        }
        best
    }

    // ------------------------------------------------------------ DMA

    /// CHCR / DMAOR written: an enabled auto-request channel (RS = 4) copies at once.
    fn dma_job(&mut self, ch: usize) -> Option<DmaJob> {
        let d = &self.dma[ch];
        let rs = d.chcr >> 8 & 15;
        if self.dmaor & 1 == 0 || d.chcr & 3 != 1 || rs != 4 { return None; }
        let unit = [8, 1, 2, 4, 32, 4, 4, 4][(d.chcr >> 4 & 7) as usize];
        let count = if d.dmatcr == 0 { 0x0100_0000 } else { d.dmatcr & 0x00FF_FFFF };
        Some(DmaJob { channel: ch, src: d.sar, dst: d.dar, unit, count, sm: d.chcr >> 12 & 3, dm: d.chcr >> 14 & 3 })
    }

    /// A DMA finished: addresses advanced, count 0, transfer end flag.
    pub fn dma_done(&mut self, ch: usize, src: u32, dst: u32) {
        let d = &mut self.dma[ch];
        d.sar = src;
        d.dar = dst;
        d.dmatcr = 0;
        d.chcr |= 2;
    }

    // ------------------------------------------------------------ store queues

    /// PREF on a store-queue address: the 32 bytes and where they go.
    pub fn sq_flush(&self, a: u32) -> (u32, [u32; 8]) {
        let q = (a >> 5 & 1) as usize;
        let dest = (self.qacr[q] >> 2 & 7) << 26 | (a & 0x03FF_FFE0);
        (dest, self.sq[q])
    }

    /// Operand-cache RAM index (CCR.ORA): bit 13 (or bit 25 with CCR.OIX) picks the half.
    pub fn ocram_index(&self, a: u32) -> usize {
        let half = if self.ccr & 0x80 != 0 { a >> 25 & 1 } else { a >> 13 & 1 };
        (a & 0xFFF | half << 12) as usize
    }

    pub fn ora(&self) -> bool { self.ccr & 0x20 != 0 }

    // ------------------------------------------------------------ registers

    /// Port A read: bits 8-9 the video cable, bits 0-1 what pins 0-1 read back. The BIOS
    /// checks them with pin 0 as an output: PCTRA low nibble 8 reads 3; B reads 3, but 0
    /// when it drives 2 out of PDTRA; C reads 3 only when it drives 2 (the behaviour
    /// Chankast found and Flycast keeps; MAME's pull-up rule reads 3 for B + 2, which makes
    /// the US BIOS halt).
    fn pdtra_read(&self) -> u16 {
        let low = match (self.pctra & 0xF, self.pdtra & 0xF) {
            (0x8, _) => 3,
            (0xB, 2) => 0,
            (0xB, _) => 3,
            (0xC, 2) => 3,
            _ => 0,
        };
        low | self.cable << 8
    }

    /// A read in FF000000-FFFFFFFF (the caller updates the timers first).
    pub fn read(&mut self, a: u32, size: u32) -> Option<u32> {
        let v = match a {
            0xFF00_0000 => self.pteh,
            0xFF00_0004 => self.ptel,
            0xFF00_0008 => self.ttb,
            0xFF00_000C => self.tea,
            0xFF00_0010 => self.mmucr,
            0xFF00_0014 => self.basra as u32,
            0xFF00_0018 => self.basrb as u32,
            0xFF00_001C => self.ccr,
            0xFF00_0020 => self.tra,
            0xFF00_0024 => self.expevt,
            0xFF00_0028 => self.intevt,
            0xFF00_0030 => 0x0402_05C1, // PVR: SH7091
            0xFF00_0034 => self.ptea,
            0xFF00_0038 => self.qacr[0],
            0xFF00_003C => self.qacr[1],
            0xFF80_0000 => self.bcr1,
            0xFF80_0004 => self.bcr2 as u32,
            0xFF80_0008 => self.wcr1,
            0xFF80_000C => self.wcr2,
            0xFF80_0010 => self.wcr3,
            0xFF80_0014 => self.mcr,
            0xFF80_0018 => self.pcr as u32,
            0xFF80_001C => self.rtcsr as u32,
            0xFF80_0020 => {
                // the refresh counter runs; programs wait on it
                self.rtcnt = self.rtcnt.wrapping_add(1) & 0xFF;
                self.rtcnt as u32
            }
            0xFF80_0024 => self.rtcor as u32,
            0xFF80_0028 => { self.rfcr = self.rfcr.wrapping_add(1) & 0x3FF; self.rfcr as u32 }
            0xFF80_002C => self.pctra,
            0xFF80_0030 => self.pdtra_read() as u32,
            0xFF80_0040 => self.pctrb,
            0xFF80_0044 => self.pdtrb as u32,
            0xFF80_0048 => self.gpioic as u32,
            0xFFA0_0000..=0xFFA0_003F => {
                let d = &self.dma[((a >> 4) & 3) as usize];
                [d.sar, d.dar, d.dmatcr, d.chcr][((a >> 2) & 3) as usize]
            }
            0xFFA0_0040 => self.dmaor,
            0xFFC0_0000 => self.frqcr as u32,
            0xFFC0_0004 => self.stbcr as u32,
            0xFFC0_0008 => self.wtcnt as u32,
            0xFFC0_000C => self.wtcsr as u32,
            0xFFC0_0010 => self.stbcr2 as u32,
            0xFFD0_0000 => self.icr as u32,
            0xFFD0_0004 | 0xFFD0_0008 | 0xFFD0_000C | 0xFFD0_0010 => self.ipr[((a - 0xFFD0_0004) >> 2) as usize] as u32,
            0xFFD8_0000 => self.tocr as u32,
            0xFFD8_0004 => self.tstr as u32,
            0xFFD8_0008..=0xFFD8_002B => {
                let t = &self.tmu[((a - 0xFFD8_0008) / 12) as usize];
                match (a - 0xFFD8_0008) % 12 { 0 => t.tcor, 4 => t.tcnt, _ => t.tcr as u32 }
            }
            0xFFD8_002C => self.tcpr2,
            // SCIF status: transmit FIFO empty and transmission ended, nothing received
            0xFFE8_0010 => 0x0060,
            0xFFE8_001C => 0,
            0xF000_0000..=0xF7FF_FFFF => 0, // cache / TLB arrays
            _ => return self.misc.get(&a).copied().or(if (0xFFC8_0000..0xFFC8_0060).contains(&a)
                || (0xFF20_0000..0xFF20_0024).contains(&a) || (0xFFE0_0000..0xFFE0_0020).contains(&a)
                || (0xFFE8_0000..0xFFE8_0028).contains(&a) { Some(0) } else { None }),
        };
        Some(if size < 4 { v & ((1u64 << (size * 8)) - 1) as u32 } else { v })
    }

    /// A write in FF000000-FFFFFFFF. Returns an auto-request DMA to run, or false in the
    /// bool when the address isn't a register (for the log).
    pub fn write(&mut self, a: u32, v: u32, size: u32) -> (bool, Option<DmaJob>) {
        let _ = size;
        match a {
            0xFF00_0000 => self.pteh = v,
            0xFF00_0004 => self.ptel = v,
            0xFF00_0008 => self.ttb = v,
            0xFF00_000C => self.tea = v,
            0xFF00_0010 => {
                // TI (bit 2) flushes the TLB: reads back 0
                self.mmucr = v & !4;
                if v & 1 != 0 { crate::dlog!("Dreamcast: the program turned the MMU on (MMUCR {v:08X}): not emulated yet"); }
            }
            0xFF00_0014 => self.basra = v as u8,
            0xFF00_0018 => self.basrb = v as u8,
            // ICI / OCI (bits 11 / 3) invalidate the caches: read back 0
            0xFF00_001C => self.ccr = v & !0x808,
            0xFF00_0020 => self.tra = v,
            0xFF00_0024 => self.expevt = v,
            0xFF00_0028 => self.intevt = v,
            0xFF00_0034 => self.ptea = v,
            0xFF00_0038 => self.qacr[0] = v & 0x1C,
            0xFF00_003C => self.qacr[1] = v & 0x1C,
            0xFF80_0000 => self.bcr1 = v,
            0xFF80_0004 => self.bcr2 = v as u16,
            0xFF80_0008 => self.wcr1 = v,
            0xFF80_000C => self.wcr2 = v,
            0xFF80_0010 => self.wcr3 = v,
            0xFF80_0014 => self.mcr = v,
            0xFF80_0018 => self.pcr = v as u16,
            0xFF80_001C => self.rtcsr = v as u16,
            0xFF80_0020 => self.rtcnt = v as u16 & 0xFF,
            0xFF80_0024 => self.rtcor = v as u16,
            0xFF80_0028 => self.rfcr = v as u16 & 0x3FF,
            0xFF80_002C => self.pctra = v,
            0xFF80_0030 => self.pdtra = v as u16,
            0xFF80_0040 => self.pctrb = v,
            0xFF80_0044 => self.pdtrb = v as u16,
            0xFF80_0048 => self.gpioic = v as u16,
            0xFF90_0000..=0xFF9F_FFFF => {} // SDRAM mode registers
            0xFFA0_0000..=0xFFA0_003F => {
                let ch = ((a >> 4) & 3) as usize;
                let d = &mut self.dma[ch];
                match (a >> 2) & 3 {
                    0 => d.sar = v,
                    1 => d.dar = v,
                    2 => d.dmatcr = v & 0x00FF_FFFF,
                    _ => d.chcr = v,
                }
                if (a >> 2) & 3 == 3 { return (true, self.dma_job(ch)); }
            }
            0xFFA0_0040 => {
                self.dmaor = v;
                let job = (0..4).find_map(|ch| self.dma_job(ch));
                return (true, job);
            }
            0xFFC0_0000 => self.frqcr = v as u16,
            0xFFC0_0004 => self.stbcr = v as u8,
            // the watchdog takes 16-bit writes with a key in the high byte
            0xFFC0_0008 => if v >> 8 & 0xFF == 0x5A { self.wtcnt = v as u8 },
            0xFFC0_000C => if v >> 8 & 0xFF == 0xA5 { self.wtcsr = v as u8 },
            0xFFC0_0010 => self.stbcr2 = v as u8,
            0xFFD0_0000 => self.icr = v as u16,
            0xFFD0_0004 | 0xFFD0_0008 | 0xFFD0_000C | 0xFFD0_0010 => self.ipr[((a - 0xFFD0_0004) >> 2) as usize] = v as u16,
            0xFFD8_0000 => self.tocr = v as u8,
            0xFFD8_0004 => self.tstr = v as u8 & 7,
            0xFFD8_0008..=0xFFD8_002B => {
                let t = &mut self.tmu[((a - 0xFFD8_0008) / 12) as usize];
                match (a - 0xFFD8_0008) % 12 {
                    0 => t.tcor = v,
                    4 => t.tcnt = v,
                    // UNF clears only by writing 0 to it
                    _ => t.tcr = (v as u16 & !0x100) | (t.tcr & v as u16 & 0x100),
                }
            }
            0xFFD8_002C => self.tcpr2 = v,
            0xFFE8_000C => {
                // SCIF transmit: keep what the program prints
                let c = v as u8;
                if c == b'\n' || self.serial_out.len() > 4000 {
                    if !self.serial_out.is_empty() { crate::dlog!("Dreamcast serial: {}", self.serial_out); }
                    self.serial_out.clear();
                } else if c != b'\r' {
                    self.serial_out.push(c as char);
                }
            }
            0xF000_0000..=0xF7FF_FFFF => {} // cache / TLB arrays
            _ => {
                let known = (0xFFC8_0000..0xFFC8_0060).contains(&a) || (0xFF20_0000..0xFF20_0024).contains(&a)
                    || (0xFFE0_0000..0xFFE0_0020).contains(&a) || (0xFFE8_0000..0xFFE8_0028).contains(&a);
                if !known { return (false, None); }
                self.misc.insert(a, v);
            }
        }
        (true, None)
    }
}
