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

use super::vif::{Vif, VifCtx};
use super::vu::Vu;
use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};

use super::cdvd::Cdvd;
use super::dmac::Dmac;
use super::sio2::Sio2;
use super::gs::Gs;
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
pub const INT_VIF0: u32 = 1 << 4;
pub const INT_VIF1: u32 = 1 << 5;
pub const INT_IPU: u32 = 1 << 8;
pub const INT_TIMER0: u32 = 1 << 9;

// IOP I_STAT sources
pub const IOP_VBLANK: u32 = 1 << 0;
pub const IOP_CDVD: u32 = 1 << 2;
pub const IOP_DMA: u32 = 1 << 3;
pub const IOP_SPU2: u32 = 1 << 9;
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
    /// debugging: SIF commands / RPCs seen (bind sid, call sid + function, replies)
    #[serde(skip)]
    pub rpc: RpcLog,
}

/// What went over the SIF as commands, from the packets' headers (ps2sdk's sifcmd.h /
/// sifrpc.h layouts): word 2 is the command id; binds name a server id (sid), calls a
/// server address (named by the sid its bind reply gave) and a function number
#[derive(Default)]
pub struct RpcLog {
    pub counts: std::collections::BTreeMap<String, u64>,
    pub recent: VecDeque<String>,
    servers: std::collections::HashMap<u32, u32>,
    /// calls not answered yet, by client address: (name, call number)
    pub waiting: std::collections::BTreeMap<u32, (String, u64)>,
    pub calls: u64,
    /// the first words of the last EE -> IOP block that wasn't a command (a call's
    /// arguments arrive just before the call)
    last_data: Vec<u32>,
    /// SIF1 block being received by the IOP: start, words
    pub blk: Option<(u32, u32)>,
}

impl RpcLog {
    /// One packet (`w` = its words); `to_iop` = EE -> IOP
    pub fn note(&mut self, w: &[u32], to_iop: bool) {
        if w.len() < 4 || w[2] & 0xFFFF_0000 != 0x8000_0000 {
            if to_iop { self.last_data = w.iter().take(4).copied().collect(); }
            return;
        }
        let g = |k: usize| w.get(k).copied().unwrap_or(0);
        let cid = w[2];
        let name = |s: &Self, srv: u32| s.servers.get(&srv).map_or(format!("@{srv:X}"), |sid| format!("{sid:X}"));
        let text = match (cid, to_iop) {
            (0x8000_0009, true) => format!("bind {:X}", g(8)),
            (0x8000_000A, true) => {
                let t = format!("call {} f{}", name(self, g(13)), g(8));
                self.calls += 1;
                let args = std::mem::take(&mut self.last_data);
                self.waiting.insert(g(7), (format!("{t} args {args:X?}"), self.calls));
                t
            }
            (0x8000_0008, false) => {
                if g(8) == 0x8000_0009 && g(9) != 0 {
                    // bind reply: which sid lives at that server address (the EE asked
                    // for the sid; the IOP answers with the server)
                    let sid = self.recent.iter().rev().find_map(|r| r.strip_prefix("bind ").and_then(|x| u32::from_str_radix(x, 16).ok()));
                    if let Some(sid) = sid { self.servers.insert(g(9), sid); }
                    format!("bound {}", name(self, g(9)))
                } else if g(8) == 0x8000_0009 {
                    "bind failed (no server)".to_string()
                } else if let Some((t, _)) = self.waiting.remove(&g(7)) {
                    format!("answered {}", t.trim_start_matches("call ").split(" args").next().unwrap_or(""))
                } else {
                    format!("end {:X}", g(8))
                }
            }
            _ => format!("{} cmd {:X}", if to_iop { "EE" } else { "IOP" }, cid & 0xFFFF),
        };
        // PS2_SIF_LOG=1: every RPC packet as it goes (with PS2_IOP_PCS, shows the order)
        static LOG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *LOG.get_or_init(|| std::env::var("PS2_SIF_LOG").is_ok()) {
            let n = self.counts.get(&text).copied().unwrap_or(0);
            if n < 4 || n.is_power_of_two() { crate::dlog!("PS2: SIF {} (#{})", text, n + 1); }
        }
        *self.counts.entry(text.clone()).or_insert(0) += 1;
        if self.recent.len() >= 24 { self.recent.pop_front(); }
        self.recent.push_back(text);
    }
}

#[derive(Serialize, Deserialize)]
pub struct Hw {
    pub ram: Vec<u8>,
    #[serde(skip)]
    pub bios: Vec<u8>,
    pub iop_ram: Vec<u8>,
    pub vu0: Vu,
    pub vu1: Vu,
    pub vif0: Vif,
    pub vif1: Vif,
    /// COP2 CMSAR0 (VCALLMSR's start address, in 8-byte units)
    pub cmsar0: u32,
    pub vu0_code: Vec<u8>,
    pub vu0_data: Vec<u8>,
    pub vu1_code: Vec<u8>,
    pub vu1_data: Vec<u8>,
    /// EE 1000_0000-1000_FFFF, words not modelled elsewhere
    pub io: Vec<u32>,
    pub intc_stat: u32,
    pub intc_mask: u32,
    /// INTC_STAT / INTC_MASK / D_STAT written since the EE last looked (`irq_lines`)
    #[serde(skip)]
    pub irq_written: bool,
    pub dmac: Dmac,
    pub timers: [EeTimer; 4],
    pub sif: Sif,
    pub gs: Gs,
    /// the MPEG decoder
    #[serde(default)]
    pub ipu: super::ipu::Ipu,
    pub mch_ricm: u32,
    pub mch_drd: u32,
    pub rdram_sdevid: u32,
    // ---- the IOP side
    /// IOP 1F80_0000-1F80_FFFF and 1F90_0000-1F90_FFFF, words not modelled elsewhere
    pub iop_io: Vec<u32>,
    #[serde(default)]
    pub spu2: super::spu2::Spu2,
    pub i_stat: u32,
    pub i_mask: u32,
    pub i_ctrl: u32,
    pub iop_dma: IopDma,
    pub iop_timers: [IopTimer; 6],
    pub cdvd: Cdvd,
    #[serde(default)]
    pub sio2: Sio2,
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
    /// debugging: the last IOP I/O accesses (address, value, write?)
    #[serde(skip)]
    pub iop_ring: VecDeque<(u32, u32, bool)>,
    /// 1A00_0006: an undocumented status the BIOS waits on ("# Initialize Start."); it
    /// counts 0, 1, 2 on each read, as PCSX2 found works
    #[serde(default)]
    pub ba6: u16,
    /// GIF_MODE (bit 0 M3R mask PATH3, bit 2 IMT) and GIF_CTRL's PSE (pause)
    #[serde(default)]
    pub gif_mode: u32,
    #[serde(default)]
    pub gif_pause: bool,
    /// debugging: recording VIF1 over a frame (vif1_frame.bin): the VIF1 / VU1 state at
    /// its start, then every feed (words, PATH3 running, PATH3 held) and VU1 start
    #[serde(skip)]
    pub vif_rec: Option<(Vec<u8>, Vec<VifEvent>)>,
}

/// One recorded VIF1 / VU1 event
#[derive(Clone, Serialize, Deserialize)]
pub enum VifEvent { Feed(Vec<u32>, bool, bool), Run(u32) }

/// "VIF1", then bincode of (state, events); state = (Vif, Vu, code, data)
pub fn vif_frame_file(state: &[u8], ev: &[VifEvent]) -> Vec<u8> {
    let cfg = bincode::config::standard().with_fixed_int_encoding();
    let mut out = b"VIF1".to_vec();
    out.extend_from_slice(&(state.len() as u64).to_le_bytes());
    out.extend_from_slice(state);
    if let Ok(e) = bincode::serde::encode_to_vec(ev, cfg) { out.extend_from_slice(&e); }
    out
}

impl Hw {
    pub fn new(bios: Vec<u8>) -> Self {
        let cdvd = Cdvd::with_bios(&bios);
        Self {
            ram: vec![0; RAM_SIZE],
            bios,
            iop_ram: vec![0; IOP_RAM_SIZE],
            vu0: Vu::new(false),
            vu1: Vu::new(true),
            vif0: Vif::new(false),
            vif1: Vif::new(true),
            cmsar0: 0,
            vu0_code: vec![0; 0x1000],
            vu0_data: vec![0; 0x1000],
            vu1_code: vec![0; 0x4000],
            vu1_data: vec![0; 0x4000],
            io: vec![0; 0x4000],
            intc_stat: 0,
            intc_mask: 0,
            irq_written: false,
            gif_mode: 0,
            gif_pause: false,
            vif_rec: None,
            dmac: Dmac::default(),
            timers: Default::default(),
            sif: Sif::default(),
            gs: Gs::new(),
            ipu: Default::default(),
            mch_ricm: 0,
            mch_drd: 0,
            rdram_sdevid: 0,
            iop_io: vec![0; 0x8000],
            spu2: Default::default(),
            i_stat: 0,
            i_mask: 0,
            i_ctrl: 0,
            iop_dma: IopDma::default(),
            iop_timers: Default::default(),
            cdvd,
            sio2: Sio2::default(),
            tty: String::new(),
            iop_tty: String::new(),
            logged: HashSet::new(),
            debug: false,
            io_ring: VecDeque::new(),
            iop_ring: VecDeque::new(),
            ba6: 0,
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
            0x2000..=0x203F => self.ipu.read(off),
            0x3020 => self.gif_stat(),
            0x3800 => self.vif0.stat(),
            0x3C00 => self.vif1.stat(),
            0x3820 => self.vif0.err,
            0x3C20 => self.vif1.err,
            0x3830 => self.vif0.mark,
            0x3C30 => self.vif1.mark,
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
        if matches!(off, 0xE010 | 0xF000 | 0xF010) { self.irq_written = true; }
        match off {
            0x0000..=0x1FFF => self.timers[(off >> 11) as usize].write(off & 0x7FF, v),
            0x2000..=0x203F => if self.ipu.write(off, v & mask) { self.intc_stat |= INT_IPU; },
            0x8000..=0xEFFF => self.dmac.write(off, v, mask),
            0xF000 => self.intc_stat &= !(v & mask),
            0xF010 => self.intc_mask ^= v & mask & 0x7FFF,
            0x3000 => {
                // GIF_CTRL: RST clears the mask / pause state, PSE pauses PATH3
                if v & 1 != 0 { self.gif_mode = 0; self.gif_pause = false; }
                self.gif_pause = v & 8 != 0;
                self.update_gif_hold();
            }
            0x3010 => { self.gif_mode = v & 5; self.update_gif_hold(); }
            0x3810 => { self.vif0.fbrst(v); self.vif_feed(false, &[]); }
            0x3C10 => { self.vif1.fbrst(v); self.vif_feed(true, &[]); }
            0x3820 => self.vif0.err = v & 7,
            0x3C20 => self.vif1.err = v & 7,
            0x3830 => self.vif0.mark = v & 0xFFFF,
            0x3C30 => self.vif1.mark = v & 0xFFFF,
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
            // memory-controller command: bit 31 = busy, which the BIOS polls (9FC41918):
            // done at once
            0xF410 => self.io[(off >> 2) as usize] = v & mask & 0x7FFF_FFFF,
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
            o @ 0x0000..=0x00EF => self.gs.pregs[(o >> 4) as usize],
            0x1000 => self.gs.csr_read(),
            0x1010 => self.gs.imr,
            0x1040 => self.gs.busdir,
            0x1080 => self.gs.siglblid,
            _ => 0,
        }
    }

    fn gs_write(&mut self, a: u32, v: u64) {
        match a & 0x1FFF {
            o @ 0x0000..=0x00EF => self.gs.pregs[(o >> 4) as usize] = v,
            0x1000 => self.gs.csr_write(v),
            0x1010 => { self.gs.imr = v & 0x7F00; self.update_gs_irq(); }
            0x1040 => self.gs.busdir = v,
            0x1080 => self.gs.siglblid = v,
            _ => {}
        }
    }

    // ================================================================== the IOP's I/O

    fn iop_io_read(&mut self, a: u32) -> u32 {
        let v = self.iop_io_read_inner(a);
        if self.debug { iop_ring(&mut self.iop_ring, a, v, false); }
        v
    }

    fn iop_io_read_inner(&mut self, a: u32) -> u32 {
        match a {
            0x1F80_1070 => self.i_stat,
            0x1F80_1074 => self.i_mask,
            0x1F80_1078 => { let v = self.i_ctrl; self.i_ctrl = 0; v }
            0x1F80_1080..=0x1F80_10FF | 0x1F80_1500..=0x1F80_157F => self.iop_dma.read(a),
            0x1F80_1100..=0x1F80_112F => self.iop_timers[((a >> 4) & 3) as usize].read(a & 0xF),
            0x1F80_1480..=0x1F80_14AF => self.iop_timers[3 + ((a >> 4) & 3) as usize].read(a & 0xF),
            0x1F80_8200..=0x1F80_82FF => self.sio2.read(a),
            0x1F90_0000..=0x1F90_07FF => {
                let o = a & 0x7FC;
                self.spu2.read16(o) as u32 | (self.spu2.read16(o + 2) as u32) << 16
            }
            _ => {
                self.note("IOP I/O", a, None);
                self.iop_io[((a & 0x1_FFFF) >> 2) as usize & 0x7FFF]
            }
        }
    }

    fn iop_io_write(&mut self, a: u32, v: u32, mask: u32) {
        if self.debug { iop_ring(&mut self.iop_ring, a, v & mask, true); }
        match a {
            0x1F80_1070 => self.i_stat &= v | !mask,
            0x1F80_2080 => self.iop_putc(v as u8),
            0x1F80_8200..=0x1F80_82FF => self.sio2.write(a, v & mask),
            0x1F80_1074 => self.i_mask = (self.i_mask & !mask) | (v & mask),
            0x1F80_1078 => self.i_ctrl = v & 1,
            0x1F80_1080..=0x1F80_10FF | 0x1F80_1500..=0x1F80_157F => self.iop_dma.write(a, v, mask),
            0x1F80_1100..=0x1F80_112F => self.iop_timers[((a >> 4) & 3) as usize].write(a & 0xF, v),
            0x1F80_1480..=0x1F80_14AF => self.iop_timers[3 + ((a >> 4) & 3) as usize].write(a & 0xF, v),
            0x1F90_0000..=0x1F90_07FF => {
                let o = a & 0x7FC;
                if mask & 0xFFFF != 0 { self.spu2.write16(o, v as u16); }
                if mask >> 16 != 0 { self.spu2.write16(o + 2, (v >> 16) as u16); }
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

/// A loop polling one register would flood the ring: repeats only count once
fn iop_ring(r: &mut VecDeque<(u32, u32, bool)>, a: u32, v: u32, w: bool) {
    if r.back() == Some(&(a, v, w)) { return; }
    if r.len() >= 16 { r.pop_front(); }
    r.push_back((a, v, w));
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
            0x1A00_0004 => {
                let v = h.ba6;
                h.ba6 = (h.ba6 + 1) % 3;
                (v as u32) << 16
            }
            0x1A00_0000..=0x1A00_FFFF => 0,
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
            0x1A00_0000..=0x1A00_FFFF => {}
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
    /// Reading INTC_STAT with a pending INT0: the interrupt follows straight after (a
    /// poll may see the bit, but the kernel's handler still runs before the code moves on)
    fn int0_on_read(&mut self, pa: u32) -> bool { pa == 0x1000_F000 && super::int0_mode().1 && self.0.int0() }
    fn irq_lines(&mut self) -> Option<(bool, bool)> {
        let h = &mut *self.0;
        if !std::mem::take(&mut h.irq_written) { return None; }
        Some((h.int0(), h.dmac.int1()))
    }
    fn jit_mem(&mut self) -> Option<crate::common::cpu::r5900::jit::JitMem> {
        let h = &mut *self.0;
        Some(crate::common::cpu::r5900::jit::JitMem {
            ram: h.ram.as_mut_ptr(), ram_len: h.ram.len(),
            rom: h.bios.as_ptr(), rom_base: 0x1FC0_0000, rom_len: h.bios.len().min(BIOS_SIZE),
        })
    }
    fn read8(&mut self, pa: u32) -> u8 {
        if pa < RAM_SIZE as u32 { return self.0.ram[pa as usize]; }
        // the CDVD registers are on the EE's bus too: OSDSYS's disc thread reads the
        // disc type (1F40_200F) directly
        if (0x1F40_2000..0x1F40_2100).contains(&pa) { return self.0.cdvd.read(pa); }
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
        // the IPU's output FIFO
        if (0x1000_7000..0x1000_7010).contains(&pa) { return self.0.ipu.fifo_read(); }
        // VIF1's FIFO read by the CPU: the GS's local -> host data
        if (0x1000_5000..0x1000_6000).contains(&pa) { return self.0.gs.readback.pop_front().unwrap_or(0); }
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
        // the FIFOs (1000_4000-1000_7FFF) take whole quadwords: the GIF's (1000_6000) goes
        // to the GS (PATH3); VIF / IPU in later stages
        if (0x1000_6000..0x1000_7000).contains(&pa) { self.0.gs.feed(v); self.0.update_gs_irq(); return; }
        if (0x1000_4000..0x1000_6000).contains(&pa) {
            let words: [u32; 4] = std::array::from_fn(|k| (v >> (32 * k)) as u32);
            self.0.vif_feed(pa >= 0x1000_5000, &words);
            return;
        }
        if (0x1000_7010..0x1000_7020).contains(&pa) { self.0.ipu.fifo_write(v); return; }
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

    /// COP2: moves between EE and VU0 registers (QMFC2 / CFC2 / QMTC2 / CTC2), VU0
    /// macro instructions, VCALLMS / VCALLMSR, and CTC2 CMSAR1 starting VU1. Programs
    /// run to the end at once, so VPU_STAT always reads 0 (nothing busy).
    fn cop2_op(&mut self, op: u32, gpr: &mut [u128; 32]) -> u32 {
        let h = &mut *self.0;
        let rs = op >> 21 & 31;
        let rt = (op >> 16 & 31) as usize;
        let rd = (op >> 11 & 31) as usize;
        match rs {
            1 => if rt != 0 { gpr[rt] = h.vu0.get_vf(rd); },
            2 => if rt != 0 {
                let v = match rd { 27 => h.cmsar0, 28 | 29 | 31 => 0, _ => h.vu0.ctrl_read(rd) };
                gpr[rt] = v as i32 as i64 as u64 as u128;
            },
            5 => h.vu0.set_vf(rd, gpr[rt]),
            6 => {
                let v = gpr[rt] as u32;
                match rd {
                    27 => h.cmsar0 = v & 0xFFFF,
                    28 | 29 => {}                              // FBRST, VPU_STAT
                    31 => h.run_vu1((v & 0xFFFF) * 8),         // CMSAR1: start VU1
                    _ => h.vu0.ctrl_write(rd, v),
                }
            }
            0x10..=0x1F => match op & 0x3F {
                0x38 => h.run_vu0((op >> 6 & 0x7FFF) * 8),   // VCALLMS
                0x39 => { let a = h.cmsar0 * 8; h.run_vu0(a); } // VCALLMSR
                _ => h.vu0.macro_op(op, &mut h.vu0_data),
            },
            _ => {}
        }
        0
    }
    fn cop2_vf_read(&mut self, reg: u32) -> u128 { self.0.vu0.get_vf(reg as usize) }
    fn cop2_vf_write(&mut self, reg: u32, v: u128) { self.0.vu0.set_vf(reg as usize, v); }
}

impl Hw {
    /// Words for VIF0 (`vif1` false) or VIF1; what they send to the GS goes there
    /// GIF_STAT: M3R, M3P, IMT, PSE; while PATH3 is held with the GIF channel running, its
    /// data waits in the FIFO (FQC, P3Q). The GIF is otherwise always idle here.
    pub fn gif_stat(&self) -> u32 {
        let mut s = (self.gif_mode & 5) | (self.vif1.mskpath3 as u32) << 1 | (self.gif_pause as u32) << 3;
        let g = &self.dmac.ch[super::dmac::GIF];
        if self.dmac.gif_hold && g.chcr & 0x100 != 0 {
            s |= 1 << 6 | g.qwc.clamp(1, 16) << 24;
        }
        s
    }

    /// VIF1 waiting at FLUSHA: carry on once PATH3 has finished (or been masked)
    pub fn vif1_flusha_check(&mut self) {
        let running = self.dmac.ch[super::dmac::GIF].chcr & 0x100 != 0;
        if self.vif1.p3_packet_wait && (!self.dmac.gif_one_packet || !running) {
            self.dmac.gif_one_packet = false;
            self.vif1.p3_packet_wait = false;
            self.vif_feed(true, &[]);
        }
        if self.vif1.flusha_wait && !(running && !self.dmac.gif_hold) {
            self.vif1.flusha_wait = false;
            self.vif_feed(true, &[]);
        }
    }

    pub fn update_gif_hold(&mut self) {
        self.dmac.gif_hold = self.gif_mode & 1 != 0 || self.vif1.mskpath3 || self.gif_pause;
    }

    pub fn vif_feed(&mut self, vif1: bool, words: &[u32]) {
        let mut gs = Vec::new();
        let gif_running = self.dmac.ch[super::dmac::GIF].chcr & 0x100 != 0;
        let gif_held = self.gif_mode & 1 != 0 || self.gif_pause;
        if vif1 {
            if let Some((_, ev)) = &mut self.vif_rec { ev.push(VifEvent::Feed(words.to_vec(), gif_running, gif_held)); }
        }
        let irq = if vif1 {
            let mut cx = VifCtx { code: &mut self.vu1_code, data: &mut self.vu1_data, vu: &mut self.vu1, gs: &mut gs, gif_running, gif_held };
            self.vif1.feed(words, &mut cx)
        } else {
            let mut cx = VifCtx { code: &mut self.vu0_code, data: &mut self.vu0_data, vu: &mut self.vu0, gs: &mut gs, gif_running, gif_held };
            self.vif0.feed(words, &mut cx)
        };
        if irq { self.intc_stat |= if vif1 { INT_VIF1 } else { INT_VIF0 }; }
        if vif1 {
            self.update_gif_hold();
            if self.vif1.p3_packet_wait { self.dmac.gif_one_packet = true; }
        }
        self.to_gs(gs);
    }

    pub fn run_vu1(&mut self, addr: u32) {
        if let Some((_, ev)) = &mut self.vif_rec { ev.push(VifEvent::Run(addr)); }
        let mut gs = Vec::new();
        self.vu1.start(addr);
        self.vu1.run(&self.vu1_code, &mut self.vu1_data, &mut gs);
        self.to_gs(gs);
    }

    pub fn run_vu0(&mut self, addr: u32) {
        let mut none = Vec::new();
        self.vu0.start(addr);
        self.vu0.run(&self.vu0_code, &mut self.vu0_data, &mut none);
    }

    fn to_gs(&mut self, qs: Vec<u128>) {
        if qs.is_empty() { return; }
        for q in qs { self.gs.feed(q); }
        self.update_gs_irq();
    }
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
            // ROM1 (the DVD player's ROM on later models): none, the BIOS scans it anyway
            0x1E00_0000..=0x1E3F_FFFF => 0,
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
        // the SPU2's registers are 16 bits (reading one must not touch its neighbour)
        if (0x1F90_0000..0x1F90_0800).contains(&pa) {
            let v = self.0.spu2.read16(pa & 0x7FE);
            if self.0.debug { iop_ring(&mut self.0.iop_ring, pa, v as u32, false); }
            return v;
        }
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
