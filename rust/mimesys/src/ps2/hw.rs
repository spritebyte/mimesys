//! The PS2's chips other than the two CPUs, and the two CPUs' views of them.
//!
//! EE physical map: 0000_0000 RAM (32 MB) · 1000_0000 I/O (timers, IPU, GIF, VIF, DMAC,
//! INTC, SIO, SIF, memory controller) · 1100_0000 VU memories · 1200_0000 GS privileged
//! registers · 1C00_0000 IOP RAM · 1FC0_0000 BIOS (4 MB).
//! IOP physical map: 0000_0000 RAM (2 MB, mirrored to 8 MB) · 1D00_0000 SIF · 1F40_2000
//! CDVD · 1F80_1000 I/O (INTC, DMA, timers, SIO2) · 1F90_0000 SPU2 · 1FC0_0000 BIOS.
//!
//! Registers nobody has modelled yet read back what was written (`io` / `iop_io`), and
//! with the debug log on, the first access to each such address is logged.

use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};

use super::cdvd::Cdvd;
use super::dmac::Dmac;
use super::iop_dma::IopDma;
use super::timers::{EeTimer, IopTimer};
use crate::common::cpu::r3000::R3000Bus;
use crate::common::cpu::r5900::EeBus;

pub const RAM_SIZE: usize = 32 << 20;
pub const IOP_RAM_SIZE: usize = 2 << 20;
pub const BIOS_SIZE: usize = 4 << 20;

// INTC (EE) sources
pub const INT_GS: u32 = 1 << 0;
pub const INT_SBUS: u32 = 1 << 1;
pub const INT_VBON: u32 = 1 << 2;
pub const INT_VBOF: u32 = 1 << 3;
pub const INT_TIMER0: u32 = 1 << 9;

// IOP I_STAT sources
pub const IOP_VBLANK: u32 = 1 << 0;
pub const IOP_CDVD: u32 = 1 << 2;
pub const IOP_DMA: u32 = 1 << 3;
pub const IOP_VBLANK_END: u32 = 1 << 11;
pub const IOP_SIO2: u32 = 1 << 17;

/// The SIF: EE <-> IOP mailboxes and the two DMA FIFOs (in 32-bit words)
#[derive(Serialize, Deserialize, Default)]
pub struct Sif {
    pub mscom: u32,
    pub smcom: u32,
    pub msflg: u32,
    pub smflg: u32,
    pub ee_ctrl: u32,
    pub iop_ctrl: u32,
    pub bd6: u32,
    /// SIF0: IOP -> EE; SIF1: EE -> IOP
    pub fifo0: VecDeque<u32>,
    pub fifo1: VecDeque<u32>,
}

/// The GS's privileged registers (the drawing side comes in stage 3)
#[derive(Serialize, Deserialize)]
pub struct GsPriv {
    /// PMODE .. BGCOLOR (1200_0000 + 10h steps)
    pub regs: [u64; 15],
    /// CSR flags: SIGNAL, FINISH, HSINT, VSINT, EDWINT
    pub csr: u64,
    pub imr: u64,
    pub busdir: u64,
    pub siglblid: u64,
    /// the field being displayed (CSR.FIELD)
    pub field: bool,
}

impl Default for GsPriv {
    fn default() -> Self { Self { regs: [0; 15], csr: 0, imr: 0x7F00, busdir: 0, siglblid: 0, field: false } }
}

impl GsPriv {
    /// CSR as read: the flags, FIFO empty, FIELD, revision 1B, ID 55
    pub fn csr_read(&self) -> u64 {
        (self.csr & 0x1F) | 0x4000 | (self.field as u64) << 13 | 0x1B << 16 | 0x55 << 24
    }

    pub fn csr_write(&mut self, v: u64) {
        self.csr &= !(v & 0x1F);
        if v & 0x200 != 0 { self.csr = 0; }
    }

    /// An unmasked flag (the GS interrupt line to INTC)
    pub fn irq(&self) -> bool { self.csr & 0x1F & !(self.imr >> 8) != 0 }
}

#[derive(Serialize, Deserialize)]
pub struct Hw {
    pub ram: Vec<u8>,
    #[serde(skip)]
    pub bios: Vec<u8>,
    pub iop_ram: Vec<u8>,
    pub vu0_code: Vec<u8>,
    pub vu0_data: Vec<u8>,
    pub vu1_code: Vec<u8>,
    pub vu1_data: Vec<u8>,
    /// EE 1000_0000-1000_FFFF, words not modelled elsewhere
    pub io: Vec<u32>,
    pub intc_stat: u32,
    pub intc_mask: u32,
    pub dmac: Dmac,
    pub timers: [EeTimer; 4],
    pub sif: Sif,
    pub gs: GsPriv,
    pub mch_ricm: u32,
    pub mch_drd: u32,
    pub rdram_sdevid: u32,
    // ---- the IOP side
    /// IOP 1F80_0000-1F80_FFFF and 1F90_0000-1F90_FFFF, words not modelled elsewhere
    pub iop_io: Vec<u32>,
    pub spu2_regs: Vec<u16>,
    pub i_stat: u32,
    pub i_mask: u32,
    pub i_ctrl: u32,
    pub iop_dma: IopDma,
    pub iop_timers: [IopTimer; 6],
    pub cdvd: Cdvd,
    // ---- debugging
    #[serde(skip)]
    pub tty: String,
    #[serde(skip)]
    pub iop_tty: String,
    #[serde(skip)]
    pub logged: HashSet<u32>,
    #[serde(skip)]
    pub debug: bool,
    /// debugging: the last EE I/O accesses (address, value, write?)
    #[serde(skip)]
    pub io_ring: VecDeque<(u32, u32, bool)>,
}

impl Hw {
    pub fn new(bios: Vec<u8>) -> Self {
        Self {
            ram: vec![0; RAM_SIZE],
            bios,
            iop_ram: vec![0; IOP_RAM_SIZE],
            vu0_code: vec![0; 0x1000],
            vu0_data: vec![0; 0x1000],
            vu1_code: vec![0; 0x4000],
            vu1_data: vec![0; 0x4000],
            io: vec![0; 0x4000],
            intc_stat: 0,
            intc_mask: 0,
            dmac: Dmac::default(),
            timers: Default::default(),
            sif: Sif::default(),
            gs: GsPriv::default(),
            mch_ricm: 0,
            mch_drd: 0,
            rdram_sdevid: 0,
            iop_io: vec![0; 0x8000],
            spu2_regs: vec![0; 0x400],
            i_stat: 0,
            i_mask: 0,
            i_ctrl: 0,
            iop_dma: IopDma::default(),
            iop_timers: Default::default(),
            cdvd: Cdvd::default(),
            tty: String::new(),
            iop_tty: String::new(),
            logged: HashSet::new(),
            debug: false,
            io_ring: VecDeque::new(),
        }
    }

    /// INT0 (INTC) for the EE
    pub fn int0(&self) -> bool { self.intc_stat & self.intc_mask != 0 }

    pub fn raise_intc(&mut self, bits: u32) { self.intc_stat |= bits; }

    /// The IOP's interrupt line
    pub fn iop_irq(&self) -> bool { self.i_ctrl & 1 != 0 && self.i_stat & self.i_mask != 0 }

    pub fn raise_iop(&mut self, bits: u32) { self.i_stat |= bits; }

    /// The GS raised (or still has) an unmasked flag
    pub fn update_gs_irq(&mut self) { if self.gs.irq() { self.intc_stat |= INT_GS; } }

    fn note(&mut self, what: &str, a: u32, v: Option<u32>) {
        if self.debug && self.logged.insert(a) {
            match v {
                Some(v) => crate::dlog!("PS2: {what} write {a:08X} = {v:08X} (first; not modelled)"),
                None => crate::dlog!("PS2: {what} read {a:08X} (first; not modelled)"),
            }
        }
    }

    /// The EE console (KPUTCHAR, 1000_F180): whole lines to the log
    fn putc(&mut self, c: u8) { console(&mut self.tty, "EE", c); }

    /// The IOP console (1F80_2080, the PS1's expansion debug port)
    fn iop_putc(&mut self, c: u8) { console(&mut self.iop_tty, "IOP", c); }

    // ================================================================== EE I/O (32-bit)

    pub fn ee_io_read(&mut self, a: u32) -> u32 {
        let v = self.ee_io_read_inner(a);
        if self.debug { self.ring(a, v, false); }
        v
    }

    fn ring(&mut self, a: u32, v: u32, w: bool) {
        // a loop polling one register would flood the ring: repeats only count once
        if self.io_ring.back() == Some(&(a, v, w)) { return; }
        if self.io_ring.len() >= 16 { self.io_ring.pop_front(); }
        self.io_ring.push_back((a, v, w));
    }

    fn ee_io_read_inner(&mut self, a: u32) -> u32 {
        let off = a & 0xFFFF;
        match off {
            0x0000..=0x1FFF => self.timers[(off >> 11) as usize].read(off & 0x7FF),
            0x8000..=0xEFFF => self.dmac.read(off),
            0xF000 => self.intc_stat,
            0xF010 => self.intc_mask,
            0xF200 => self.sif.mscom,
            0xF210 => self.sif.smcom,
            0xF220 => self.sif.msflg,
            0xF230 => self.sif.smflg,
            0xF240 => self.sif.ee_ctrl | 0xF000_0102,
            0xF260 => self.sif.bd6,
            0xF430 => 0,
            0xF440 => self.mch_drd_read(),
            0xF520 => self.dmac.enable,
            0x2010 => 0,   // IPU_CTRL: idle
            0x3020 => 0,   // GIF_STAT: idle
            0x3800 | 0x3C00 => 0, // VIF0 / VIF1 STAT
            _ => {
                self.note("EE I/O", a, None);
                self.io[(off >> 2) as usize]
            }
        }
    }

    /// `mask`: the bytes being written (narrow stores)
    pub fn ee_io_write(&mut self, a: u32, v: u32, mask: u32) {
        if self.debug { self.ring(a, v & mask, true); }
        let off = a & 0xFFFF;
        match off {
            0x0000..=0x1FFF => self.timers[(off >> 11) as usize].write(off & 0x7FF, v),
            0x8000..=0xEFFF => self.dmac.write(off, v, mask),
            0xF000 => self.intc_stat &= !(v & mask),
            0xF010 => self.intc_mask ^= v & mask & 0x7FFF,
            0xF180 => self.putc(v as u8),
            0xF200 => self.sif.mscom = v,
            0xF220 => self.sif.msflg |= v,
            0xF230 => self.sif.smflg &= !v,
            0xF240 => self.sif.ee_ctrl = v,
            0xF260 => self.sif.bd6 = v,
            0xF430 => {
                // MCH_RICM: the RDRAM init handshake (ps2tek)
                let sa = v >> 16 & 0xFFF;
                let sbc = v >> 6 & 0xF;
                if sa == 0x21 && sbc == 1 && self.mch_drd >> 7 & 1 == 0 { self.rdram_sdevid = 0; }
                self.mch_ricm = v & !0x8000_0000;
            }
            0xF440 => self.mch_drd = v,
            0xF590 => self.dmac.enable = v,
            _ => {
                self.note("EE I/O", a, Some(v));
                let w = &mut self.io[(off >> 2) as usize];
                *w = (*w & !mask) | (v & mask);
            }
        }
    }

    fn mch_drd_read(&mut self) -> u32 {
        let sop = self.mch_ricm >> 6 & 0xF;
        let sa = self.mch_ricm >> 16 & 0xFFF;
        if sop != 0 { return 0; }
        match sa {
            0x21 => if self.rdram_sdevid < 2 { self.rdram_sdevid += 1; 0x1F } else { 0 },
            0x23 => 0x0D0D,
            0x24 => 0x0090,
            0x40 => self.mch_ricm & 0x1F,
            _ => 0,
        }
    }

    fn gs_read(&mut self, a: u32) -> u64 {
        match a & 0x1FFF {
            o @ 0x0000..=0x00EF => self.gs.regs[(o >> 4) as usize],
            0x1000 => self.gs.csr_read(),
            0x1010 => self.gs.imr,
            0x1040 => self.gs.busdir,
            0x1080 => self.gs.siglblid,
            _ => 0,
        }
    }

    fn gs_write(&mut self, a: u32, v: u64) {
        match a & 0x1FFF {
            o @ 0x0000..=0x00EF => self.gs.regs[(o >> 4) as usize] = v,
            0x1000 => self.gs.csr_write(v),
            0x1010 => { self.gs.imr = v & 0x7F00; self.update_gs_irq(); }
            0x1040 => self.gs.busdir = v,
            0x1080 => self.gs.siglblid = v,
            _ => {}
        }
    }

    // ================================================================== the IOP's I/O

    fn iop_io_read(&mut self, a: u32) -> u32 {
        match a {
            0x1F80_1070 => self.i_stat,
            0x1F80_1074 => self.i_mask,
            0x1F80_1078 => { let v = self.i_ctrl; self.i_ctrl = 0; v }
            0x1F80_1080..=0x1F80_10FF | 0x1F80_1500..=0x1F80_157F => self.iop_dma.read(a),
            0x1F80_1100..=0x1F80_112F => self.iop_timers[((a >> 4) & 3) as usize].read(a & 0xF),
            0x1F80_1480..=0x1F80_14AF => self.iop_timers[3 + ((a >> 4) & 3) as usize].read(a & 0xF),
            // SIO2: RECV1 = no device in either port (stage 6)
            0x1F80_826C => 0x1D100,
            0x1F80_8270 => 0xF,
            0x1F80_8274 => 0,
            0x1F80_8268 => 0,
            0x1F90_0000..=0x1F90_07FF => {
                let i = ((a & 0x7FF) >> 1) as usize;
                self.spu2_regs[i] as u32 | (self.spu2_regs[i + 1] as u32) << 16
            }
            _ => {
                self.note("IOP I/O", a, None);
                self.iop_io[((a & 0x1_FFFF) >> 2) as usize & 0x7FFF]
            }
        }
    }

    fn iop_io_write(&mut self, a: u32, v: u32, mask: u32) {
        match a {
            0x1F80_1070 => self.i_stat &= v | !mask,
            0x1F80_2080 => self.iop_putc(v as u8),
            0x1F80_1074 => self.i_mask = (self.i_mask & !mask) | (v & mask),
            0x1F80_1078 => self.i_ctrl = v & 1,
            0x1F80_1080..=0x1F80_10FF | 0x1F80_1500..=0x1F80_157F => self.iop_dma.write(a, v, mask),
            0x1F80_1100..=0x1F80_112F => self.iop_timers[((a >> 4) & 3) as usize].write(a & 0xF, v),
            0x1F80_1480..=0x1F80_14AF => self.iop_timers[3 + ((a >> 4) & 3) as usize].write(a & 0xF, v),
            0x1F90_0000..=0x1F90_07FF => {
                let i = ((a & 0x7FF) >> 1) as usize;
                if mask & 0xFFFF != 0 { self.spu2_regs[i] = v as u16; }
                if mask >> 16 != 0 { self.spu2_regs[i + 1] = (v >> 16) as u16; }
            }
            _ => {
                self.note("IOP I/O", a, Some(v));
                let w = &mut self.iop_io[((a & 0x1_FFFF) >> 2) as usize & 0x7FFF];
                *w = (*w & !mask) | (v & mask);
            }
        }
    }

    fn sif_iop_read(&mut self, a: u32) -> u32 {
        match a & 0xFF {
            0x00 => self.sif.mscom,
            0x10 => self.sif.smcom,
            0x20 => self.sif.msflg,
            0x30 => self.sif.smflg,
            0x40 => self.sif.iop_ctrl | 0xF000_0002,
            0x60 => self.sif.bd6,
            _ => 0,
        }
    }

    fn sif_iop_write(&mut self, a: u32, v: u32) {
        match a & 0xFF {
            0x10 => self.sif.smcom = v,
            0x20 => self.sif.msflg &= !v,
            0x30 => self.sif.smflg |= v,
            0x40 => self.sif.iop_ctrl = v,
            0x60 => self.sif.bd6 = v,
            _ => {}
        }
    }
}

fn console(buf: &mut String, who: &str, c: u8) {
    if c == b'\n' {
        let line = std::mem::take(buf);
        crate::dlog!("{who} TTY: {line}");
    } else if c != b'\r' {
        buf.push(c as char);
        if buf.len() > 512 { let l = std::mem::take(buf); crate::dlog!("{who} TTY: {l}"); }
    }
}

// ====================================================================== the EE's view

pub struct EeView<'a>(pub &'a mut Hw);

impl EeView<'_> {
    fn rd32(&mut self, pa: u32) -> u32 {
        let h = &mut *self.0;
        match pa {
            0x0000_0000..=0x01FF_FFFF => rd32(&h.ram, pa as usize & (RAM_SIZE - 1)),
            0x1000_0000..=0x1000_FFFF => h.ee_io_read(pa),
            0x1100_0000..=0x1100_FFFF => rd32(vu_mem(h, pa), (pa & 0x3FFF) as usize & (vu_mem(h, pa).len() - 1)),
            0x1200_0000..=0x1200_1FFF => (h.gs_read(pa & !7) >> (8 * (pa & 4))) as u32,
            0x1C00_0000..=0x1C1F_FFFF => rd32(&h.iop_ram, (pa & 0x1F_FFFF) as usize),
            0x1FC0_0000..=0x1FFF_FFFF => rd32(&h.bios, (pa as usize) & (BIOS_SIZE - 1)),
            _ => { h.note("EE bus", pa, None); 0 }
        }
    }

    fn wr32(&mut self, pa: u32, v: u32, mask: u32) {
        let h = &mut *self.0;
        match pa {
            0x0000_0000..=0x01FF_FFFF => wr32m(&mut h.ram, pa as usize & (RAM_SIZE - 1), v, mask),
            0x1000_0000..=0x1000_FFFF => h.ee_io_write(pa, v, mask),
            0x1100_0000..=0x1100_FFFF => {
                let m = vu_mem_mut(h, pa);
                let o = (pa & 0x3FFF) as usize & (m.len() - 1);
                wr32m(m, o, v, mask);
            }
            0x1200_0000..=0x1200_1FFF => {
                // 32-bit halves of the 64-bit registers
                let base = pa & !7;
                let old = h.gs_read(base);
                let sh = 8 * (pa & 4);
                let m64 = (mask as u64) << sh;
                let new = (old & !m64) | ((v as u64) << sh & m64);
                // CSR's flag bits clear on 1: pass only what was written
                if base & 0x1FFF == 0x1000 { h.gs.csr_write((v as u64) << sh & m64); } else { h.gs_write(base, new); }
            }
            0x1C00_0000..=0x1C1F_FFFF => wr32m(&mut h.iop_ram, (pa & 0x1F_FFFF) as usize, v, mask),
            _ => h.note("EE bus", pa, Some(v)),
        }
    }
}

fn vu_mem(h: &Hw, pa: u32) -> &Vec<u8> {
    match pa & 0xC000 { 0x0000 => &h.vu0_code, 0x4000 => &h.vu0_data, 0x8000 => &h.vu1_code, _ => &h.vu1_data }
}

fn vu_mem_mut(h: &mut Hw, pa: u32) -> &mut Vec<u8> {
    match pa & 0xC000 { 0x0000 => &mut h.vu0_code, 0x4000 => &mut h.vu0_data, 0x8000 => &mut h.vu1_code, _ => &mut h.vu1_data }
}

#[inline]
pub fn rd32(m: &[u8], o: usize) -> u32 { u32::from_le_bytes(m[o..o + 4].try_into().unwrap()) }
#[inline]
pub fn wr32(m: &mut [u8], o: usize, v: u32) { m[o..o + 4].copy_from_slice(&v.to_le_bytes()); }
#[inline]
fn wr32m(m: &mut [u8], o: usize, v: u32, mask: u32) {
    if mask == u32::MAX { wr32(m, o, v); } else { let old = rd32(m, o); wr32(m, o, (old & !mask) | (v & mask)); }
}

impl EeBus for EeView<'_> {
    fn read8(&mut self, pa: u32) -> u8 {
        if pa < RAM_SIZE as u32 { return self.0.ram[pa as usize]; }
        (self.rd32(pa & !3) >> (8 * (pa & 3))) as u8
    }
    fn read16(&mut self, pa: u32) -> u16 {
        if pa < RAM_SIZE as u32 { let o = pa as usize; return u16::from_le_bytes([self.0.ram[o], self.0.ram[o + 1]]); }
        (self.rd32(pa & !3) >> (8 * (pa & 2))) as u16
    }
    fn read32(&mut self, pa: u32) -> u32 { self.rd32(pa) }
    fn read64(&mut self, pa: u32) -> u64 {
        if pa < RAM_SIZE as u32 { let o = pa as usize; return u64::from_le_bytes(self.0.ram[o..o + 8].try_into().unwrap()); }
        if (0x1200_0000..0x1200_2000).contains(&pa) { return self.0.gs_read(pa); }
        self.rd32(pa) as u64 | (self.rd32(pa + 4) as u64) << 32
    }
    fn read128(&mut self, pa: u32) -> u128 {
        if pa < RAM_SIZE as u32 { let o = pa as usize; return u128::from_le_bytes(self.0.ram[o..o + 16].try_into().unwrap()); }
        self.read64(pa) as u128 | (self.read64(pa + 8) as u128) << 64
    }
    fn write8(&mut self, pa: u32, v: u8) {
        if pa < RAM_SIZE as u32 { self.0.ram[pa as usize] = v; return; }
        let sh = 8 * (pa & 3);
        self.wr32(pa & !3, (v as u32) << sh, 0xFF << sh);
    }
    fn write16(&mut self, pa: u32, v: u16) {
        if pa < RAM_SIZE as u32 { let o = pa as usize; self.0.ram[o..o + 2].copy_from_slice(&v.to_le_bytes()); return; }
        let sh = 8 * (pa & 2);
        self.wr32(pa & !3, (v as u32) << sh, 0xFFFF << sh);
    }
    fn write32(&mut self, pa: u32, v: u32) { self.wr32(pa, v, u32::MAX); }
    fn write64(&mut self, pa: u32, v: u64) {
        if pa < RAM_SIZE as u32 { let o = pa as usize; self.0.ram[o..o + 8].copy_from_slice(&v.to_le_bytes()); return; }
        if (0x1200_0000..0x1200_2000).contains(&pa) { self.0.gs_write(pa, v); return; }
        self.wr32(pa, v as u32, u32::MAX);
        self.wr32(pa + 4, (v >> 32) as u32, u32::MAX);
    }
    fn write128(&mut self, pa: u32, v: u128) {
        if pa < RAM_SIZE as u32 { let o = pa as usize; self.0.ram[o..o + 16].copy_from_slice(&v.to_le_bytes()); return; }
        // the FIFOs (1000_4000-1000_7FFF) take whole quadwords: VIF / GIF / IPU in later stages
        if (0x1000_4000..0x1000_8000).contains(&pa) { return; }
        self.write64(pa, v as u64);
        self.write64(pa + 8, (v >> 64) as u64);
    }
    fn fetch32(&mut self, pa: u32) -> u32 {
        if pa < RAM_SIZE as u32 { return rd32(&self.0.ram, pa as usize); }
        if pa >= 0x1FC0_0000 { return rd32(&self.0.bios, pa as usize & (BIOS_SIZE - 1)); }
        self.rd32(pa)
    }
    fn cpcond0(&mut self) -> bool { self.0.dmac.cpcond0() }
}

// ====================================================================== the IOP's view

pub struct IopView<'a>(pub &'a mut Hw);

impl IopView<'_> {
    fn rd32(&mut self, pa: u32) -> u32 {
        let h = &mut *self.0;
        match pa {
            0x0000_0000..=0x007F_FFFF => rd32(&h.iop_ram, pa as usize & (IOP_RAM_SIZE - 1)),
            0x1D00_0000..=0x1D00_00FF => h.sif_iop_read(pa),
            0x1F40_2000..=0x1F40_20FF => {
                let b = pa & !3;
                (0..4).map(|k| (h.cdvd.read(b + k) as u32) << (8 * k)).fold(0, |a, x| a | x)
            }
            0x1F80_0000..=0x1F80_FFFF | 0x1F90_0000..=0x1F90_FFFF => h.iop_io_read(pa),
            0x1FC0_0000..=0x1FFF_FFFF => rd32(&h.bios, pa as usize & (BIOS_SIZE - 1)),
            _ => { h.note("IOP bus", pa, None); 0 }
        }
    }

    fn wr32(&mut self, pa: u32, v: u32, mask: u32) {
        let h = &mut *self.0;
        match pa {
            0x0000_0000..=0x007F_FFFF => wr32m(&mut h.iop_ram, pa as usize & (IOP_RAM_SIZE - 1), v, mask),
            0x1D00_0000..=0x1D00_00FF => h.sif_iop_write(pa, v),
            0x1F40_2000..=0x1F40_20FF => {
                for k in 0..4 {
                    if mask >> (8 * k) & 0xFF != 0 { h.cdvd.write(pa + k, (v >> (8 * k)) as u8, &mut h.i_stat); }
                }
            }
            0x1F80_0000..=0x1F80_FFFF | 0x1F90_0000..=0x1F90_FFFF => h.iop_io_write(pa, v, mask),
            _ => h.note("IOP bus", pa, Some(v)),
        }
    }
}

impl R3000Bus for IopView<'_> {
    fn read8(&mut self, pa: u32) -> u8 {
        if pa < 0x80_0000 { return self.0.iop_ram[pa as usize & (IOP_RAM_SIZE - 1)]; }
        if (0x1F40_2000..0x1F40_2100).contains(&pa) { return self.0.cdvd.read(pa); }
        (self.rd32(pa & !3) >> (8 * (pa & 3))) as u8
    }
    fn read16(&mut self, pa: u32) -> u16 {
        if pa < 0x80_0000 { let o = pa as usize & (IOP_RAM_SIZE - 1); return u16::from_le_bytes([self.0.iop_ram[o], self.0.iop_ram[o + 1]]); }
        (self.rd32(pa & !3) >> (8 * (pa & 2))) as u16
    }
    fn read32(&mut self, pa: u32) -> u32 { self.rd32(pa) }
    fn write8(&mut self, pa: u32, v: u8) {
        if pa < 0x80_0000 { self.0.iop_ram[pa as usize & (IOP_RAM_SIZE - 1)] = v; return; }
        let sh = 8 * (pa & 3);
        self.wr32(pa & !3, (v as u32) << sh, 0xFF << sh);
    }
    fn write16(&mut self, pa: u32, v: u16) {
        if pa < 0x80_0000 { let o = pa as usize & (IOP_RAM_SIZE - 1); self.0.iop_ram[o..o + 2].copy_from_slice(&v.to_le_bytes()); return; }
        let sh = 8 * (pa & 2);
        self.wr32(pa & !3, (v as u32) << sh, 0xFFFF << sh);
    }
    fn write32(&mut self, pa: u32, v: u32) { self.wr32(pa, v, u32::MAX); }
    fn fetch32(&mut self, pa: u32, _cached: bool) -> u32 {
        if pa < 0x80_0000 { return rd32(&self.0.iop_ram, pa as usize & (IOP_RAM_SIZE - 1)); }
        if pa >= 0x1FC0_0000 { return rd32(&self.0.bios, pa as usize & (BIOS_SIZE - 1)); }
        self.rd32(pa)
    }
}
