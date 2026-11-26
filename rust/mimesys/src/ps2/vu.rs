//! The vector units, VU0 and VU1: an interpreter for micro mode (64-bit upper + lower
//! instruction pairs) that VU0's COP2 macro mode shares.
//!
//! From ps2tek's VU pages; opcode tables and latencies checked against Play! (BSD-2,
//! `Source/ee/MA_VU_*.cpp`, `VUShared.*`). Floats follow the EE FPU's rules (no NaN /
//! Inf, round toward zero), using the same helpers.
//!
//! Timing model: one instruction pair per cycle. What programs can see of the pipeline
//! is kept: an FMAC result's MAC / status / clip flags arrive 4 cycles later, Q (DIV /
//! SQRT / RSQRT) and P (EFU) arrive after their latencies, and reading a VF register an
//! FMAC wrote in the last 4 cycles stalls until it's ready (so the flags line up as on
//! hardware). The upper instruction's result wins when both halves write one register,
//! and the lower half reads registers as they were before the upper half's write.
//! Programs run to completion when started (MSCAL / VCALLMS / CMSAR1).

use serde::{Deserialize, Serialize};

use crate::common::cpu::r5900::fpu::{from_f64, ps2_div, to_f64};

const MAC_LATENCY: u64 = 4;
/// one program's cap (cycles) in case one never ends
const MAX_CYCLES: u64 = 4_000_000;

// status flag bits
pub const ST_Z: u32 = 1;
pub const ST_S: u32 = 2;
pub const ST_U: u32 = 4;
pub const ST_O: u32 = 8;
pub const ST_I: u32 = 16;
pub const ST_D: u32 = 32;

/// A PATH1 transfer under way: the next quadword, the current GIFtag's quadwords left
/// (None: a tag is next), whether that tag ends the packet, the cycle of the next read
#[derive(Clone, Copy, Serialize, Deserialize)]
struct Kick { addr: u32, left: Option<u32>, eop: bool, at: u64, tags: u32 }

/// A result's flags on their way: MAC + status, or (`is_clip`) a CLIP judgement
#[repr(C)]
#[derive(Clone, Copy, Serialize, Deserialize, Default)]
pub(crate) struct Flags { pub at: u64, pub mac: u32, pub stat: u32, pub clip: u32, pub is_clip: u32 }

/// The flags pipeline: a ring (translated code pushes and pops it too)
#[repr(C)]
#[derive(Clone, Copy, Serialize, Deserialize, Default)]
pub(crate) struct FlagQ { pub q: [Flags; 16], pub head: u32, pub len: u32 }

impl FlagQ {
    pub fn front(&self) -> Option<&Flags> { (self.len > 0).then(|| &self.q[self.head as usize & 15]) }
    pub fn back(&self) -> Option<&Flags> { (self.len > 0).then(|| &self.q[(self.head + self.len - 1) as usize & 15]) }
    pub fn pop_front(&mut self) { if self.len > 0 { self.head = (self.head + 1) & 15; self.len -= 1; } }
    pub fn push_back(&mut self, f: Flags) {
        debug_assert!(self.len < 16);
        if self.len == 16 { self.pop_front(); }
        self.q[(self.head + self.len) as usize & 15] = f;
        self.len += 1;
    }
    /// Drop the CLIP entries (FCSET)
    pub fn drop_clips(&mut self) {
        let keep: Vec<Flags> = (0..self.len).map(|k| self.q[(self.head + k) as usize & 15]).filter(|f| f.is_clip == 0).collect();
        self.head = 0;
        self.len = 0;
        for f in keep { self.push_back(f); }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Vu {
    pub vu1: bool,
    pub vf: [[u32; 4]; 32],
    pub vi: [u16; 16],
    pub acc: [u32; 4],
    pub q: u32,
    pub p: u32,
    pub i: u32,
    pub r: u32,
    pub status: u32,
    pub mac: u32,
    pub clip: u32,
    pub pc: u32,
    pub tpc: u32,
    pub running: bool,
    /// bumped by every write to micro memory (MPG, EE stores): translated blocks are
    /// checked against the code again only after one
    #[serde(skip)]
    pub code_gen: u64,
    /// micro-mode cycles run by translated blocks / by the interpreter with the JIT on
    #[serde(skip)]
    pub jit_cycles: u64,
    #[serde(skip)]
    pub interp_cycles: u64,
    /// cycle count of the current program
    pub cycle: u64,
    /// (unused: keeps the serialized layout of patch 0270 and earlier)
    vf_ready_old: [u64; 32],
    acc_ready_old: u64,
    /// per register and field (x y z w): the cycle an FMAC's result lands (always 0
    /// between programs)
    #[serde(skip)]
    vf_ready: [[u64; 4]; 32],
    #[serde(skip)]
    acc_ready: u64,
    #[serde(with = "flagq_serde")]
    pub(crate) flags: FlagQ,
    q_pend: Option<(u64, u32, u32)>,
    p_pend: Option<(u64, u32)>,
    /// branch target after the delay slot
    #[serde(with = "branch_serde")]
    pub(crate) branch: u32,
    /// E bit seen: stop after the next pair
    ending: u8,
    /// XGKICK in progress (PATH1 reads the packet a quadword a cycle while the program
    /// goes on, so stores just after the kick still make it into the packet)
    #[serde(default)]
    kick: Option<Kick>,
    /// debugging: where the last program started
    #[serde(skip)]
    pub start_pc: u32,
    /// host time spent running programs (ns)
    #[serde(skip)]
    pub host_ns: u64,
    /// debugging: copy the VU and its data memory as program number `snap_at` starts
    #[serde(skip)]
    pub snap_at: u64,
    #[serde(skip)]
    pub snap: Option<(Box<Vu>, Vec<u8>, Vec<u8>)>,
    /// VU1: VIF1's TOP / ITOP, read by XTOP / XITOP
    pub top: u16,
    pub itop: u16,
    /// debugging
    #[serde(skip)]
    pub programs: u64,
    #[serde(skip)]
    pub kicks: u64,
    /// per instruction pair: the VF registers it reads (stall check), decoded once
    #[serde(skip)]
    reads: Vec<Reads>,
    /// per instruction pair: stall inputs and VI target, decoded once (PCSX2 stall model)
    #[serde(skip)]
    dec: Vec<Dec>,
    /// the last four pairs' VI writes (register, value before, a flag read)
    #[serde(skip)]
    pub(crate) vi_hist: [u32; 4],
    #[serde(skip)]
    br_stalled: bool,
    #[serde(skip)]
    pub illegal: u64,
    /// cycles run, all programs (speed report)
    #[serde(skip)]
    pub total_cycles: u64,
    /// the JIT's view: the earliest cycle a flag / Q / P result lands (never later than
    /// the truth; 0 = check), whether an XGKICK is running, the upper result waiting for
    /// its commit
    #[serde(skip)]
    next_due: u64,
    /// the earliest Q / P result (u64::MAX: none)
    #[serde(skip)]
    qp_due: u64,
    #[serde(skip)]
    kick_on: bool,
    #[serde(skip)]
    up_pend: Option<(Dst, [u32; 4], bool)>,
    #[serde(skip)]
    pub jit: JitCell,
}

/// The layouts recordings and states were saved with (flags as a VecDeque of
/// {at, mac, stat, clip: Option}, the branch as an Option)
mod flagq_serde {
    use super::{FlagQ, Flags};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    #[derive(Serialize, Deserialize)]
    struct Old { at: u64, mac: u32, stat: u32, clip: Option<u32> }
    pub fn serialize<S: Serializer>(q: &FlagQ, s: S) -> Result<S::Ok, S::Error> {
        let v: std::collections::VecDeque<Old> = (0..q.len).map(|k| {
            let f = q.q[(q.head + k) as usize & 15];
            Old { at: f.at, mac: f.mac, stat: f.stat, clip: (f.is_clip != 0).then_some(f.clip) }
        }).collect();
        v.serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<FlagQ, D::Error> {
        let v = std::collections::VecDeque::<Old>::deserialize(d)?;
        let mut q = FlagQ::default();
        for o in v { q.push_back(Flags { at: o.at, mac: o.mac, stat: o.stat, clip: o.clip.unwrap_or(0), is_clip: o.clip.is_some() as u32 }); }
        Ok(q)
    }
}
mod branch_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    pub fn serialize<S: Serializer>(b: &u32, s: S) -> Result<S::Ok, S::Error> { (*b != super::NO_BRANCH).then_some(*b).serialize(s) }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> { Ok(Option::<u32>::deserialize(d)?.unwrap_or(super::NO_BRANCH)) }
}

/// No branch waiting for its delay slot
pub(crate) const NO_BRANCH: u32 = u32::MAX;

/// A `vi_hist` entry: VI `r` was written (it held `old`), by a flag read when `flag`
pub(crate) fn hist(r: usize, old: u16, flag: bool) -> u32 { 1 << 31 | (flag as u32) << 24 | (r as u32) << 16 | old as u32 }

/// The VU JIT's translations (not copied with the VU)
#[derive(Default)]
pub struct JitCell(pub Option<Box<super::vu_jit::VuJit>>);
impl Clone for JitCell { fn clone(&self) -> Self { JitCell(None) } }

/// Field offsets for translated code
#[derive(Clone, Copy)]
pub(crate) struct JitOffsets {
    pub pc: usize, pub cycle: usize, pub vf_ready: usize, pub br_stalled: usize,
    pub next_due: usize, pub kick_on: usize, pub ending: usize, pub running: usize,
    pub vf: usize, pub vi: usize, pub acc: usize, pub acc_ready: usize, pub q: usize, pub i: usize,
    pub flags: usize, pub fl_head: usize, pub fl_len: usize, pub branch: usize, pub vi_hist: usize,
    pub qp_due: usize, pub mac: usize, pub status: usize, pub clip: usize,
}

impl Vu {
    pub fn new(vu1: bool) -> Self {
        let mut vf = [[0u32; 4]; 32];
        vf[0][3] = 0x3F80_0000;
        Self {
            vu1, vf, vi: [0; 16], acc: [0; 4], q: 0, p: 0, i: 0, r: 0, status: 0, mac: 0, clip: 0,
            pc: 0, tpc: 0, running: false, cycle: 0, vf_ready: [[0; 4]; 32], acc_ready: 0, vf_ready_old: [0; 32], acc_ready_old: 0,
            flags: FlagQ::default(), q_pend: None, p_pend: None, branch: NO_BRANCH, ending: 0,
            snap_at: 0, snap: None, host_ns: 0, reads: Vec::new(), dec: Vec::new(), vi_hist: [0; 4], br_stalled: false, start_pc: 0, kick: None, top: 0, itop: 0, programs: 0, kicks: 0, illegal: 0, total_cycles: 0, next_due: 0, qp_due: 0, code_gen: 0, jit_cycles: 0, interp_cycles: 0, kick_on: false, up_pend: None, jit: JitCell(None),
        }
    }

    pub fn get_vf(&self, r: usize) -> u128 {
        let v = &self.vf[r & 31];
        v.iter().enumerate().fold(0, |a, (k, &x)| a | (x as u128) << (32 * k))
    }
    pub fn set_vf(&mut self, r: usize, q: u128) {
        if r & 31 == 0 { return; }
        for k in 0..4 { self.vf[r & 31][k] = (q >> (32 * k)) as u32; }
    }

    /// COP2 control registers 16-31 (CFC2 / CTC2): everything pending lands first
    pub fn ctrl_read(&mut self, r: usize) -> u32 {
        self.settle();
        match r {
            0..=15 => self.vi[r] as u32,
            16 => self.status,
            17 => self.mac,
            18 => self.clip,
            20 => self.r,
            21 => self.i,
            22 => self.q,
            23 => self.p,
            26 => self.tpc / 8,
            _ => 0,
        }
    }
    pub fn ctrl_write(&mut self, r: usize, v: u32) {
        self.settle();
        match r {
            1..=15 => self.vi[r] = v as u16,
            16 => self.status = (self.status & 0x3F) | (v & 0xFC0),
            17 => self.mac = v & 0xFFFF,
            18 => self.clip = v & 0xFF_FFFF,
            20 => self.r = v & 0x7F_FFFF,
            21 => self.i = v,
            22 => self.q = v,
            23 => self.p = v,
            _ => {}
        }
    }

    /// Land every result still in a pipeline (end of a program, or macro mode)
    pub fn settle(&mut self) {
        self.cycle = self.cycle.max(self.flags.back().map_or(0, |f| f.at));
        self.land_flags();
        if let Some((at, _, _)) = self.q_pend { self.cycle = self.cycle.max(at); }
        if let Some((at, _)) = self.p_pend { self.cycle = self.cycle.max(at); }
        self.land_q();
        self.land_p();
        self.vf_ready = [[0; 4]; 32];
        self.vi_hist = [0; 4];
        self.acc_ready = 0;
        self.cycle = 0;
    }

    fn land_flags(&mut self) {
        while let Some(f) = self.flags.front().copied() {
            if f.at > self.cycle { break; }
            self.flags.pop_front();
            if f.is_clip != 0 {
                self.clip = (self.clip << 6 | f.clip) & 0xFF_FFFF;
            } else {
                self.mac = f.mac;
                self.status = (self.status & !0xF) | f.stat | (f.stat << 6);
            }
        }
    }
    /// WAITQ's stall (before the pair runs)
    pub(crate) fn wait_q(&mut self) {
        if let Some((at, _, _)) = self.q_pend { self.cycle = self.cycle.max(at); }
    }

    fn land_q(&mut self) {
        if let Some((at, v, st)) = self.q_pend {
            if at <= self.cycle {
                self.q = v;
                self.status = (self.status & !(ST_I | ST_D)) | st | (st << 6);
                self.q_pend = None;
            }
        }
    }
    fn land_p(&mut self) {
        if let Some((at, v)) = self.p_pend {
            if at <= self.cycle { self.p = v; self.p_pend = None; }
        }
    }

    /// Start a micro program at `addr` (bytes)
    pub fn start(&mut self, addr: u32) {
        self.start_pc = addr;
        self.pc = addr;
        self.running = true;
        self.branch = NO_BRANCH;
        self.ending = 0;
        self.programs += 1;
    }

    /// Run until the program ends. `code` / `data` are this VU's memories; XGKICK
    /// packets go to `path1`.
    pub fn run(&mut self, code: &[u8], data: &mut [u8], path1: &mut Vec<u128>) {
        if self.snap_at != 0 && self.snap_at == self.programs && self.snap.is_none() {
            let mut me = self.clone();
            me.snap_at = 0;
            self.snap = Some((Box::new(me), data.to_vec(), code.to_vec()));
        }
        let start = self.cycle;
        let t0 = std::time::Instant::now();
        if self.dec.len() != code.len() / 8 { self.dec = vec![Dec::default(); code.len() / 8]; }
        // debugging: VU_JITCHECK=1 runs each program on the interpreter too and reports
        // the first one whose results differ
        static CHECK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let check = *CHECK.get_or_init(|| std::env::var("VU_JITCHECK").is_ok());
        let reference = if check && super::vu_jit::enabled() {
            let mut v = self.clone();
            let mut d = data.to_vec();
            let mut p = path1.clone();
            v.snap_at = 1;      // (keeps the JIT out of it)
            v.programs = 0;
            let mut ti = Vec::new();
            while v.running {
                ti.push((v.pc, v.cycle, v.vi));
                v.step(code, &mut d, &mut p);
                if v.cycle - start > MAX_CYCLES { v.running = false; }
            }
            v.pump_kick(&d, &mut p, u64::MAX);
            Some((v, d, p, ti))
        } else { None };
        if super::vu_jit::enabled() && pcsx2_stalls() && self.snap_at == 0 {
            self.kick_on = self.kick.is_some();
            self.recalc_due();
            let mut j = self.jit.0.take().unwrap_or_default();
            let mut ctx = super::vu_jit::Ctx::new(self as *mut Vu, data, path1 as *mut _);
            let mut tk = 0;
            let mut reported = false;
            let mut leader = true;
            while self.running {
                if let Some((_, _, _, ti)) = reference.as_ref() {
                    let st = (self.pc, self.cycle, self.vi);
                    while tk < ti.len() && ti[tk] != st { tk += 1; }
                    if tk == ti.len() && !reported {
                        reported = true;
                        println!("   JIT state unseen: pc {:04X} cycle {} (last block start {:04X?})", st.0, st.1, j.last);
                    }
                }
                match j.get(code, self.pc, leader, self.code_gen) {
                    // SAFETY: the block only touches the VU, data memory and path1 through ctx
                    Some(f) => {
                        let c0 = self.cycle;
                        unsafe { j.last = Some(self.pc); ctx.vu = self as *mut Vu; f(&mut ctx) };
                        self.jit_cycles += self.cycle - c0;
                        leader = true;
                    }
                    None => {
                        let next = self.pc.wrapping_add(8);
                        let c0 = self.cycle;
                        self.step(code, data, path1);
                        self.interp_cycles += self.cycle - c0;
                        self.kick_on = self.kick.is_some();
                        self.recalc_due();
                        leader = self.pc != next;
                    }
                }
                if self.cycle - start > MAX_CYCLES { self.running = false; }
            }
            self.jit.0 = Some(j);
        }
        while self.running {
            self.step(code, data, path1);
            if self.cycle - start > MAX_CYCLES { self.running = false; }
        }
        self.pump_kick(data, path1, u64::MAX);
        if let Some((v, d, p, _)) = reference {
            let same = v.pc == self.pc && v.cycle == self.cycle && v.vf == self.vf && v.vi == self.vi && v.acc == self.acc
                && v.q == self.q && v.p == self.p && v.mac == self.mac && v.status == self.status && v.clip == self.clip
                && d == *data && p == *path1;
            if !same {
                println!("VU JIT differs in program {} (start {:04X}): pc {:04X}/{:04X} cycle {}/{} vi {:X?}/{:X?} data same {} out same {}",
                    self.programs, self.start_pc, v.pc, self.pc, v.cycle, self.cycle, v.vi, self.vi, d == *data, p == *path1);
                for r in 0..32 { if v.vf[r] != self.vf[r] { println!("   vf{r} {:08X?} / {:08X?}", v.vf[r], self.vf[r]); } }
            }
        }
        self.tpc = self.pc;
        self.total_cycles += self.cycle - start;
        {
            static DBG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            if *DBG.get_or_init(|| std::env::var("VU_CYC").is_ok()) {
                println!("vu prog {} at {:04X}: {} cycles, pc {:04X}, out {}", self.programs, self.start_pc, self.cycle - start, self.pc, path1.len());
            }
        }
        self.host_ns += t0.elapsed().as_nanos() as u64;
        self.settle();
    }

    pub fn step(&mut self, code: &[u8], data: &mut [u8], path1: &mut Vec<u128>) {
        let a = self.pc as usize & (code.len() - 1) & !7;
        let lo = u32::from_le_bytes(code[a..a + 4].try_into().unwrap());
        let hi = u32::from_le_bytes(code[a + 4..a + 8].try_into().unwrap());
        // a branch in the pair before: this pair is its delay slot
        let delay = std::mem::replace(&mut self.branch, NO_BRANCH);
        self.pc = self.pc.wrapping_add(8);

        // stalls: VF sources an FMAC is still writing
        // (per field, as PCSX2 does; vf0 never stalls; ACC chains don't stall)
        let mut ready = 0;
        let fine = pcsx2_stalls();
        let k = a >> 3;
        if k >= self.dec.len() { self.dec = vec![Dec::default(); code.len() / 8]; }
        let raw = (hi as u64) << 32 | lo as u64;
        if !self.dec[k].ok || self.dec[k].raw != raw { self.dec[k] = Dec::decode(lo, hi); }
        let d = self.dec[k];
        if fine {
            for &(r, m) in &d.rd[..d.n as usize] {
                let t = &self.vf_ready[r as usize];
                for f in 0..4 { if m & (8 >> f) != 0 { ready = ready.max(t[f]); } }
            }
        } else {
            // patch 0270's model: whole registers (all fields share one time), vf0 included
            if self.reads.len() != code.len() / 8 { self.reads = vec![Reads::default(); code.len() / 8]; }
            let d = &mut self.reads[k];
            if d.lo != lo || d.hi != hi || !d.valid { *d = Reads::decode(lo, hi); }
            let (mut m, acc) = (d.vf, d.acc);
            while m != 0 {
                let r = m.trailing_zeros() as usize;
                m &= m - 1;
                ready = ready.max(self.vf_ready[r][0]);
            }
            if acc { ready = ready.max(self.acc_ready); }
        }
        self.br_stalled = ready > self.cycle;
        self.cycle = self.cycle.max(ready);
        // WAITQ stalls the whole pair: the upper half reads the new Q (DDS's city: MULq
        // beside WAITQ after DIV used the previous vertex's Q)
        if hi & (1 << 31) == 0 && is_waitq(lo) { self.wait_q(); }
        self.land_flags();
        self.land_q();
        self.land_p();
        if self.kick.is_some() { self.pump_kick(data, path1, self.cycle); }

        let up = (d.up)(self, hi);
        let h = &mut self.vi_hist;
        (h[3], h[2], h[1]) = (h[2], h[1], h[0]);
        if hi & (1 << 31) != 0 {
            self.i = lo;                                     // I bit: the lower word is I
            self.vi_hist[0] = 0;
        } else {
            // the VI this pair changed (if any), for the branch delay below
            let r = d.vit as usize;
            let before = self.vi[r];
            (d.lo)(self, lo, data, path1);
            self.vi_hist[0] = if r != 0 && self.vi[r] != before { hist(r, before, d.vflag) } else { 0 };
        }
        self.commit(up);
        self.cycle += 1;
        if delay != NO_BRANCH { self.pc = delay; }
        if self.ending > 0 {
            self.ending -= 1;
            if self.ending == 0 { self.running = false; }
        }
        if hi & (1 << 30) != 0 { self.ending = 1; }
    }

    // ------------------------------------------------------------ for the JIT

    pub(crate) fn jit_prepare(&mut self) { self.kick_on = self.kick.is_some(); self.recalc_due(); }
    fn recalc_due(&mut self) {
        let mut d = u64::MAX;
        if let Some((at, _, _)) = self.q_pend { d = d.min(at); }
        if let Some((at, _)) = self.p_pend { d = d.min(at); }
        self.qp_due = d;
        self.next_due = d.min(self.flags.front().map_or(u64::MAX, |f| f.at));
    }
    pub(crate) fn jit_offsets() -> JitOffsets {
        use std::mem::offset_of;
        JitOffsets { pc: offset_of!(Vu, pc), cycle: offset_of!(Vu, cycle), vf_ready: offset_of!(Vu, vf_ready),
            br_stalled: offset_of!(Vu, br_stalled), next_due: offset_of!(Vu, next_due), kick_on: offset_of!(Vu, kick_on),
            ending: offset_of!(Vu, ending), running: offset_of!(Vu, running),
            vf: offset_of!(Vu, vf), vi: offset_of!(Vu, vi), acc: offset_of!(Vu, acc), acc_ready: offset_of!(Vu, acc_ready),
            q: offset_of!(Vu, q), i: offset_of!(Vu, i), flags: offset_of!(Vu, flags) + offset_of!(FlagQ, q),
            fl_head: offset_of!(Vu, flags) + offset_of!(FlagQ, head), fl_len: offset_of!(Vu, flags) + offset_of!(FlagQ, len),
            branch: offset_of!(Vu, branch), vi_hist: offset_of!(Vu, vi_hist),
            qp_due: offset_of!(Vu, qp_due), mac: offset_of!(Vu, mac), status: offset_of!(Vu, status), clip: offset_of!(Vu, clip) }
    }
    pub(crate) fn jit_bvi(&self, r: usize) -> u32 { self.bvi(r) as u32 }
    pub(crate) fn jit_wait_q(&mut self) { self.wait_q(); self.recalc_due(); }
    /// The (register, fields) a pair waits for (as `step`)
    pub(crate) fn jit_reads(lo: u32, hi: u32) -> Vec<(usize, u32)> {
        let d = Dec::decode(lo, hi);
        d.rd[..d.n as usize].iter().map(|&(r, m)| (r as usize, m as u32)).collect()
    }
    pub(crate) fn jit_upper_fn(hi: u32) -> usize { up_handler(hi) as usize }
    pub(crate) fn jit_lower_fn(lo: u32) -> usize { lo_handler(lo) as usize }
    /// The VI the lower half may write, and bit 8: it's a flag read
    pub(crate) fn jit_vit(lo: u32) -> u32 {
        lower_vi_target(lo) as u32 | ((0x10..=0x1C).contains(&(lo >> 25)) as u32) << 8
    }
    pub(crate) fn jit_land(&mut self) {
        self.land_flags();
        self.land_q();
        self.land_p();
        self.recalc_due();
    }
    pub(crate) fn jit_kick(&mut self, data: &[u8], path1: &mut Vec<u128>) {
        self.pump_kick(data, path1, self.cycle);
        self.kick_on = self.kick.is_some();
    }
    pub(crate) fn jit_upper(&mut self, hi: u32, f: fn(&mut Vu, u32) -> Option<(Dst, [u32; 4], bool)>) {
        self.up_pend = f(self, hi);
        self.recalc_due();
    }
    pub(crate) fn jit_lower(&mut self, lo: u32, f: fn(&mut Vu, u32, &mut [u8], &mut Vec<u128>), vit: u32, data: &mut [u8], path1: &mut Vec<u128>) {
        let h = &mut self.vi_hist;
        (h[3], h[2], h[1]) = (h[2], h[1], h[0]);
        let r = (vit & 0xFF) as usize;
        let before = self.vi[r];
        f(self, lo, data, path1);
        self.vi_hist[0] = if r != 0 && self.vi[r] != before { hist(r, before, vit & 0x100 != 0) } else { 0 };
        self.kick_on = self.kick.is_some();
        self.recalc_due();
    }
    pub(crate) fn jit_commit(&mut self) {
        let up = self.up_pend.take();
        self.commit(up);
    }

    /// Write an upper instruction's result (after the lower half has run)
    fn commit(&mut self, up: Option<(Dst, [u32; 4], bool)>) {
        let Some((r, v, flags)) = up else { return };
        match r {
            Dst::Vf(r, dest) => {
                if r != 0 {
                    for k in 0..4 { if dest & (8 >> k) != 0 { self.vf[r][k] = v[k]; } }
                }
                // every upper result takes the FMAC's 4 cycles (MAX / MINI / FTOI / ITOF /
                // ABS too, though they set no flags)
                if pcsx2_stalls() {
                    if r != 0 {
                        for k in 0..4 { if dest & (8 >> k) != 0 { self.vf_ready[r][k] = self.cycle + MAC_LATENCY; } }
                    }
                } else if flags {
                    // (the model games ran with up to 0270: whole register, flag-setting ops)
                    self.vf_ready[r] = [self.cycle + MAC_LATENCY; 4];
                }
            }
            Dst::Acc(dest) => {
                for k in 0..4 { if dest & (8 >> k) != 0 { self.acc[k] = v[k]; } }
                self.acc_ready = self.cycle + MAC_LATENCY;
            }
        }
    }

    /// One COP2 macro instruction (CO bit set) other than VCALLMS / VCALLMSR: results
    /// and flags land at once
    pub fn macro_op(&mut self, op: u32, data: &mut [u8]) {
        let f = op & 0x3F;
        let upper = f < 0x30 || (f >= 0x3C && ((op >> 6 & 31) << 2 | (op & 3)) < 0x30);
        if upper {
            let up = self.upper(op & 0x01FF_FFFF);
            self.commit(up);
        } else {
            let mut none = Vec::new();
            self.lower(0x8000_0000 | (op & 0x01FF_FFFF), data, &mut none);
        }
        self.cycle += 1;
        self.settle();
    }

    // ================================================================ upper (FMAC)

    /// One upper instruction; returns where its result goes. Used by macro mode too
    /// (bits 0-24 are laid out alike).
    pub fn upper(&mut self, op: u32) -> Option<(Dst, [u32; 4], bool)> {
        up_handler(op)(self, op)
    }

    /// An FMAC operation on the fields in `dest`, queueing its flags
    #[inline(always)]
    fn fmac(&mut self, d: Dst, dest: u32, a: [u32; 4], b: [u32; 4], k: Arith) -> Option<(Dst, [u32; 4], bool)> {
        let mut out = match d { Dst::Vf(r, _) => self.vf[r], Dst::Acc(_) => self.acc };
        use super::vu_simd as v;
        let (res, fl) = match k {
            Arith::Add => v::fmac4::<{ v::ADD }>(a, b, self.acc),
            Arith::Sub => v::fmac4::<{ v::SUB }>(a, b, self.acc),
            Arith::Mul => v::fmac4::<{ v::MUL }>(a, b, self.acc),
            Arith::Madd => v::fmac4::<{ v::MADD }>(a, b, self.acc),
            Arith::Msub => v::fmac4::<{ v::MSUB }>(a, b, self.acc),
        };
        for i in 0..4 { if dest & (8 >> i) != 0 { out[i] = res[i]; } }
        let m4 = dest & 15;
        let mac = v::mac4(res, fl) & (m4 | m4 << 4 | m4 << 8 | m4 << 12);
        let stat = (mac & 0xF != 0) as u32 * ST_Z | (mac & 0xF0 != 0) as u32 * ST_S
            | (mac & 0xF00 != 0) as u32 * ST_U | (mac & 0xF000 != 0) as u32 * ST_O;
        self.flags.push_back(Flags { at: self.cycle + MAC_LATENCY, mac, stat, clip: 0, is_clip: 0 });
        Some((d, out, true))
    }

    // ================================================================ lower

    pub fn lower(&mut self, op: u32, data: &mut [u8], path1: &mut Vec<u128>) {
        lo_handler(op)(self, op, data, path1)
    }

    /// `lower` with the opcode (op >> 25) and, for 40h, the special function (op & 3F,
    /// and n for 3C-3F) passed separately: handlers pass constants so each compiles to its
    /// own arm
    #[inline(always)]
    fn lower_k(&mut self, key: u32, sf: u32, sn: u32, op: u32, data: &mut [u8], path1: &mut Vec<u128>) {
        let dest = op >> 21 & 15;
        let it = (op >> 16 & 15) as usize;
        let is = (op >> 11 & 15) as usize;
        let ft = (op >> 16 & 31) as usize;
        let fs = (op >> 11 & 31) as usize;
        let imm11 = ((op as i32) << 21 >> 21) as u32;
        let fsf = (op >> 21 & 3) as usize;
        let ftf = (op >> 23 & 3) as usize;
        let vi = |me: &Self, r: usize| me.vi[r] as u32;
        let qmask = (data.len() / 16 - 1) as u32;
        match key {
            0x00 => {                                                     // LQ
                let a = vi(self, is).wrapping_add(imm11);
                let v = self.load(data, a & qmask);
                self.write_vf(ft, dest, v);
            }
            0x01 => {                                                     // SQ
                let a = vi(self, it).wrapping_add(imm11);
                let v = self.vf[fs];
                self.store(data, a & qmask, dest, v);
            }
            0x04 => {                                                     // ILW
                let a = vi(self, is).wrapping_add(imm11);
                let v = self.load(data, a & qmask);
                self.set_vi(it, v[first_field(dest)]);
            }
            0x05 => {                                                     // ISW
                let a = vi(self, is).wrapping_add(imm11);
                let v = vi(self, it);
                self.store(data, a & qmask, dest, [v; 4]);
            }
            0x08 => { let v = vi(self, is).wrapping_add(imm15(op)); self.set_vi(it, v); }   // IADDIU
            0x09 => { let v = vi(self, is).wrapping_sub(imm15(op)); self.set_vi(it, v); }   // ISUBIU
            0x10 => { let c = self.clip_now(); self.vi[1] = (c == op & 0xFF_FFFF) as u16; }  // FCEQ
            0x11 => { self.land_all_clip(); self.clip = op & 0xFF_FFFF; }                 // FCSET
            0x12 => { let c = self.clip_now(); self.vi[1] = (c & op & 0xFF_FFFF != 0) as u16; } // FCAND
            0x13 => { let c = self.clip_now(); self.vi[1] = ((c | op) & 0xFF_FFFF == 0xFF_FFFF) as u16; } // FCOR
            0x14 => { let s = self.status & 0xFFF; self.set_vi(it, (s == imm12(op)) as u32); } // FSEQ
            0x15 => self.status = (self.status & 0x3F) | (imm12(op) & 0xFC0),              // FSSET
            0x16 => { let s = self.status & 0xFFF; self.set_vi(it, s & imm12(op)); }       // FSAND
            0x17 => { let s = self.status & 0xFFF; self.set_vi(it, s | imm12(op)); }       // FSOR
            0x18 => { let m = self.mac; let v = (m == vi(self, is)) as u32; self.set_vi(it, v); } // FMEQ
            0x1A => { let v = self.mac & vi(self, is); self.set_vi(it, v); }               // FMAND
            0x1B => { let v = self.mac | vi(self, is); self.set_vi(it, v); }               // FMOR
            0x1C => { let c = self.clip_now(); self.set_vi(it, c & 0xFFF); }               // FCGET
            0x20 => self.jump_rel(imm11),                                                  // B
            0x21 => { let l = self.pc.wrapping_add(8) / 8; self.set_vi(it, l); self.jump_rel(imm11); } // BAL
            0x24 => self.branch = vi(self, is) * 8,                                  // JR
            0x25 => {                                                                      // JALR
                let t = vi(self, is) * 8;
                let l = self.pc.wrapping_add(8) / 8;
                self.set_vi(it, l);
                self.branch = t;
            }
            0x28 => if self.bvi(it) == self.bvi(is) { self.jump_rel(imm11) },             // IBEQ
            0x29 => if self.bvi(it) != self.bvi(is) { self.jump_rel(imm11) },             // IBNE
            0x2C => if (self.bvi(is) as i16) < 0 { self.jump_rel(imm11) },                // IBLTZ
            0x2D => if (self.bvi(is) as i16) > 0 { self.jump_rel(imm11) },                // IBGTZ
            0x2E => if (self.bvi(is) as i16) <= 0 { self.jump_rel(imm11) },               // IBLEZ
            0x2F => if (self.bvi(is) as i16) >= 0 { self.jump_rel(imm11) },               // IBGEZ
            0x40 => self.lower_special(sf, sn, op, data, path1, dest, ft, fs, it, is, fsf, ftf),
            _ => self.illegal += 1,
        }
    }

    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    fn lower_special(&mut self, sf: u32, sn: u32, op: u32, data: &mut [u8], path1: &mut Vec<u128>, dest: u32,
                     ft: usize, fs: usize, it: usize, is: usize, fsf: usize, ftf: usize) {
        let id = (op >> 6 & 15) as usize;
        let vi = |me: &Self, r: usize| me.vi[r] as u32;
        let qmask = (data.len() / 16 - 1) as u32;
        match sf {
            0x30 => { let v = vi(self, is).wrapping_add(vi(self, it)); self.set_vi(id, v); }   // IADD
            0x31 => { let v = vi(self, is).wrapping_sub(vi(self, it)); self.set_vi(id, v); }   // ISUB
            0x32 => {                                                                       // IADDI
                let imm = ((op >> 6 & 31) as i32) << 27 >> 27;
                let v = vi(self, is).wrapping_add(imm as u32);
                self.set_vi(it, v);
            }
            0x34 => { let v = vi(self, is) & vi(self, it); self.set_vi(id, v); }               // IAND
            0x35 => { let v = vi(self, is) | vi(self, it); self.set_vi(id, v); }               // IOR
            0x3C..=0x3F => {
                let n = sn;
                let s = self.vf[fs];
                match n {
                    0x30 => self.write_vf(ft, dest, s),                                     // MOVE
                    0x31 => self.write_vf(ft, dest, [s[1], s[2], s[3], s[0]]),              // MR32
                    0x34 => {                                                               // LQI
                        let v = self.load(data, vi(self, is) & qmask);
                        self.write_vf(ft, dest, v);
                        if is != 0 { self.vi[is] = self.vi[is].wrapping_add(1); }
                    }
                    0x35 => {                                                               // SQI
                        self.store(data, vi(self, it) & qmask, dest, s);
                        if it != 0 { self.vi[it] = self.vi[it].wrapping_add(1); }
                    }
                    0x36 => {                                                               // LQD
                        if is != 0 { self.vi[is] = self.vi[is].wrapping_sub(1); }
                        let v = self.load(data, vi(self, is) & qmask);
                        self.write_vf(ft, dest, v);
                    }
                    0x37 => {                                                               // SQD
                        if it != 0 { self.vi[it] = self.vi[it].wrapping_sub(1); }
                        self.store(data, vi(self, it) & qmask, dest, s);
                    }
                    0x38 => {                                                               // DIV
                        let (v, st) = vu_div(s[fsf], self.vf[ft][ftf]);
                        self.start_q(v, st, 7);
                    }
                    0x39 => {                                                               // SQRT
                        let (v, st) = vu_sqrt(self.vf[ft][ftf]);
                        self.start_q(v, st, 7);
                    }
                    0x3A if self.vf[ft][ftf] & 0x7F80_0000 == 0 => {                       // RSQRT x / 0 (PCSX2)
                        let (x, y) = (s[fsf], self.vf[ft][ftf]);
                        let sign = (x ^ y) & 0x8000_0000;
                        let (v, st) = if x & 0x7F80_0000 == 0 { (sign, ST_D | ST_I) } else { (sign | 0x7F7F_FFFF, ST_D) };
                        self.start_q(v, st, 13);
                    }
                    0x3A => {                                                               // RSQRT
                        let (r, st) = vu_sqrt(self.vf[ft][ftf]);
                        let (v, st2) = vu_div(s[fsf], r);
                        let st = if st2 & ST_D != 0 { st | ST_D } else { st };
                        self.start_q(v, st, 13);
                    }
                    0x3B => {                                                               // WAITQ
                        if let Some((at, _, _)) = self.q_pend { self.cycle = self.cycle.max(at); }
                        self.land_q();
                    }
                    0x3C => self.set_vi(it, s[fsf] & 0xFFFF),                                // MTIR
                    0x3D => { let v = self.vi[is] as i16 as i32 as u32; self.write_vf(ft, dest, [v; 4]); } // MFIR
                    0x3E => {                                                               // ILWR
                        let v = self.load(data, vi(self, is) & qmask);
                        self.set_vi(it, v[first_field(dest)]);
                    }
                    0x3F => { let v = vi(self, it); self.store(data, vi(self, is) & qmask, dest, [v; 4]); } // ISWR
                    0x40 => {                                                               // RNEXT
                        let r = self.r;
                        let x = (r >> 4 & 1) ^ (r >> 22 & 1);
                        self.r = (r << 1 ^ x) & 0x7F_FFFF;
                        let v = self.r | 0x3F80_0000;
                        self.write_vf(ft, dest, [v; 4]);
                    }
                    0x41 => { let v = self.r | 0x3F80_0000; self.write_vf(ft, dest, [v; 4]); } // RGET
                    0x42 => self.r = s[fsf] & 0x7F_FFFF,                                    // RINIT
                    0x43 => self.r = (self.r ^ s[fsf]) & 0x7F_FFFF,                         // RXOR
                    0x64 => { let p = self.p; self.write_vf(ft, dest, [p; 4]); }            // MFP
                    0x68 => self.set_vi(it, self.top as u32),                                // XTOP
                    0x69 => self.set_vi(it, self.itop as u32),                               // XITOP
                    0x6C => self.xgkick(data, vi(self, is), path1),                         // XGKICK
                    0x70..=0x7E if n != 0x77 && n != 0x7F => self.efu(n, s, fsf),
                    _ => self.illegal += 1,
                }
            }
            _ => self.illegal += 1,
        }
    }

    /// The elementary function unit (VU1): results in P
    fn efu(&mut self, n: u32, s: [u32; 4], fsf: usize) {
        if n == 0x7B {                                                        // WAITP
            if let Some((at, _)) = self.p_pend { self.cycle = self.cycle.max(at); }
            self.land_p();
            return;
        }
        let f = |k: usize| to_f64(s[k]);
        let sq = f(0) * f(0) + f(1) * f(1) + f(2) * f(2);
        let x = f(fsf);
        let (v, lat) = match n {
            0x70 => (sq, 10),                                   // ESADD
            0x71 => (1.0 / sq, 17),                             // ERSADD
            0x72 => (sq.sqrt(), 17),                            // ELENG
            0x73 => (1.0 / sq.sqrt(), 23),                      // ERLENG
            0x74 => ((f(1) / f(0)).atan(), 53),                 // EATANxy
            0x75 => ((f(2) / f(0)).atan(), 53),                 // EATANxz
            0x76 => (f(0) + f(1) + f(2) + f(3), 11),            // ESUM
            0x78 => (x.abs().sqrt(), 11),                       // ESQRT
            0x79 => (1.0 / x.abs().sqrt(), 17),                 // ERSQRT
            0x7A => (1.0 / x, 11),                              // ERCPR
            0x7C => (x.sin(), 28),                              // ESIN
            0x7D => (x.atan(), 53),                             // EATAN
            0x7E => ((-x).exp(), 43),                           // EEXP
            _ => { self.illegal += 1; return; }
        };
        let v = if v.is_finite() { from_f64(v).0 } else { 0x7FFF_FFFF };
        if let Some((at, _)) = self.p_pend { self.cycle = self.cycle.max(at); self.land_p(); }
        self.p_pend = Some((self.cycle + lat, v));
    }

    fn start_q(&mut self, v: u32, st: u32, lat: u64) {
        // a second division waits for the first
        if let Some((at, _, _)) = self.q_pend { self.cycle = self.cycle.max(at); self.land_q(); }
        self.q_pend = Some((self.cycle + lat, v, st));
    }

    fn clip_now(&mut self) -> u32 { self.clip }
    fn land_all_clip(&mut self) { self.flags.drop_clips(); }

    fn jump_rel(&mut self, imm: u32) {
        // relative to the instruction after the branch
        self.branch = self.pc.wrapping_add(imm.wrapping_mul(8));
    }

    /// A VI register as a branch sees it. Branches read VI a stage early (PCSX2's
    /// analyzeBranchVI): when the pair just before wrote it (not a flag read), the branch
    /// gets the value from before the oldest of the last four pairs that wrote it in a row
    /// back from there; not when the branch itself stalled. Conditional branches only (as
    /// PCSX2). PS2_VU_BRANCH_VI=0: off
    fn bvi(&self, r: usize) -> u16 {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let on = *ON.get_or_init(|| std::env::var("PS2_VU_BRANCH_VI").map_or(true, |v| v != "0"));
        if r == 0 || !on || self.br_stalled { return self.vi[r]; }
        let h0 = self.vi_hist[0];
        if !(h0 >> 31 != 0 && (h0 >> 16 & 15) as usize == r && h0 >> 24 & 1 == 0) { return self.vi[r]; }
        let mut v = self.vi[r];
        for (k, &h) in self.vi_hist.iter().enumerate() {
            if h >> 31 != 0 && (h >> 16 & 15) as usize == r {
                if h >> 24 & 1 != 0 && k > 0 { break; }
                v = h as u16;
            } else {
                // (only a run of writes back from the branch: PCSX2 also counts a write
                // further back in the same block; the games here come out the same)
                break;
            }
        }
        v
    }

    fn set_vi(&mut self, r: usize, v: u32) { if r != 0 { self.vi[r & 15] = v as u16; } }

    /// A lower instruction's VF result (LQ, MOVE, MR32, MFIR, MFP, RGET...): readers
    /// stall 4 cycles for it too (PCSX2's analysis)
    fn write_vf(&mut self, r: usize, dest: u32, v: [u32; 4]) {
        if r == 0 { return; }
        for k in 0..4 {
            if dest & (8 >> k) != 0 {
                self.vf[r][k] = v[k];
                if pcsx2_stalls() { self.vf_ready[r][k] = self.cycle + MAC_LATENCY; }
            }
        }
    }

    fn load(&self, data: &[u8], qa: u32) -> [u32; 4] {
        let a = qa as usize * 16;
        std::array::from_fn(|k| u32::from_le_bytes(data[a + 4 * k..a + 4 * k + 4].try_into().unwrap()))
    }
    fn store(&self, data: &mut [u8], qa: u32, dest: u32, v: [u32; 4]) {
        let a = qa as usize * 16;
        for k in 0..4 {
            if dest & (8 >> k) != 0 { data[a + 4 * k..a + 4 * k + 4].copy_from_slice(&v[k].to_le_bytes()); }
        }
    }

    /// XGKICK: the GIF packet at `qa` (quadwords) goes to the GS as PATH1. The transfer
    /// runs alongside the program, a quadword a cycle; a second XGKICK waits for the first
    fn xgkick(&mut self, data: &[u8], qa: u32, path1: &mut Vec<u128>) {
        self.kicks += 1;
        if self.kick.is_some() {
            self.pump_kick(data, path1, u64::MAX);
        }
        let qmask = (data.len() / 16 - 1) as u32;
        self.kick = Some(Kick { addr: qa & qmask, left: None, eop: false, at: self.cycle + 1, tags: 0 });
    }

    /// Move the kicked packet's quadwords due by `until` (cycles); a wait (u64::MAX)
    /// finishes it and the VU stalls till then
    fn pump_kick(&mut self, data: &[u8], path1: &mut Vec<u128>, until: u64) {
        let qmask = (data.len() / 16 - 1) as u32;
        let rd = |a: u32| -> u128 { u128::from_le_bytes(data[a as usize * 16..a as usize * 16 + 16].try_into().unwrap()) };
        let Some(mut k) = self.kick else { return };
        let mut guard = 0u32;
        loop {
            if k.at > until { self.kick = Some(k); return; }
            guard += 1;
            if guard > (qmask + 1) * 4 { break; }
            match k.left {
                None => {
                    let tag = rd(k.addr);
                    path1.push(tag);
                    k.addr = (k.addr + 1) & qmask;
                    k.tags += 1;
                    let nloop = (tag & 0x7FFF) as u32;
                    k.eop = tag >> 15 & 1 != 0;
                    let flg = (tag >> 58 & 3) as u32;
                    let nreg = match (tag >> 60 & 15) as u32 { 0 => 16, n => n };
                    let n = match flg { 0 => nloop * nreg, 1 => (nloop * nreg).div_ceil(2), _ => nloop };
                    let n = n.min(qmask + 1);
                    k.left = if n > 0 { Some(n) } else { None };
                    if n == 0 && (k.eop || k.tags >= 64) { break; }
                }
                Some(n) => {
                    path1.push(rd(k.addr));
                    k.addr = (k.addr + 1) & qmask;
                    k.left = if n > 1 { Some(n - 1) } else { None };
                    if n == 1 && (k.eop || k.tags >= 64) { break; }
                }
            }
            k.at += 1;
        }
        // done: a waiting program stalls until the transfer ended
        if until == u64::MAX { self.cycle = self.cycle.max(k.at); }
        self.kick = None;
    }
}

#[derive(Clone, Copy)]
pub enum Dst { Vf(usize, u32), Acc(u32) }

#[derive(Clone, Copy)]
enum Arith { Add, Sub, Mul, Madd, Msub }

/// MAX / MINI compare the values (PS2 floats compare like sign-magnitude integers)
fn minmax(a: [u32; 4], b: [u32; 4], max: bool) -> [u32; 4] {
    std::array::from_fn(|k| {
        let (x, y) = (to_f64(a[k]), to_f64(b[k]));
        if (x > y) == max { a[k] } else if x == y { a[k] } else { b[k] }
    })
}

fn first_field(dest: u32) -> usize { (0..4).find(|k| dest & (8 >> k) != 0).unwrap_or(0) }

fn imm15(op: u32) -> u32 { (op >> 10 & 0x7800) | (op & 0x7FF) }
fn imm12(op: u32) -> u32 { (op >> 10 & 0x800) | (op & 0x7FF) }

/// WAITQ (lower 40h, special 3BFh)
pub(crate) fn is_waitq(lo: u32) -> bool { lo >> 25 == 0x40 && lo & 0x7FF == 0x3BF }

/// DIV: x / y with the I (0 / 0) and D (x / 0) status bits
fn vu_div(x: u32, y: u32) -> (u32, u32) {
    // by zero: ±0x7F7FFFFF (the largest IEEE single, as PCSX2), not the PS2's 0x7FFFFFFF.
    // God of War's title: hidden bones (zero matrices) give w = 0; with 0x7FFFFFFF their
    // vertices were drawn as huge triangles
    if y & 0x7F80_0000 == 0 {
        let st = if x & 0x7F80_0000 == 0 { ST_I } else { ST_D };
        return ((x ^ y) & 0x8000_0000 | 0x7F7F_FFFF, st);
    }
    let (v, f) = ps2_div(x, y);
    use crate::common::cpu::r5900::fpu::{F_D, F_I};
    let st = if f & F_I != 0 { ST_I } else if f & F_D != 0 { ST_D } else { 0 };
    (v, st)
}

/// SQRT of |x| (I set for a negative input)
fn vu_sqrt(x: u32) -> (u32, u32) {
    let st = if x & 0x8000_0000 != 0 && x & 0x7F80_0000 != 0 { ST_I } else { 0 };
    (from_f64(to_f64(x & 0x7FFF_FFFF).sqrt()).0, st)
}

/// VF registers and fields (x 8, y 4, z 2, w 1) an upper instruction reads (for stalls)
fn upper_reads(op: u32) -> [(usize, u32); 2] {
    let ft = (op >> 16 & 31) as usize;
    let fs = (op >> 11 & 31) as usize;
    let dest = op >> 21 & 15;
    let bc = 8 >> (op & 3);
    let f = op & 0x3F;
    if f >= 0x3C {
        let n = (op >> 6 & 31) << 2 | (op & 3);
        return match n {
            0x00..=0x0F | 0x18..=0x1B => [(fs, dest), (ft, bc)],
            0x10..=0x17 | 0x1C..=0x27 => [(fs, dest), (0, 0)],
            0x1F => [(fs, 0xE), (ft, 1)],                         // CLIP
            0x2E => [(fs, 0xE), (ft, 0xE)],                       // OPMULA
            0x2F => [(0, 0), (0, 0)],
            _ => [(fs, dest), (ft, dest)],
        };
    }
    match f {
        0x00..=0x1B => [(fs, dest), (ft, bc)],
        0x1C..=0x27 => [(fs, dest), (0, 0)],
        0x2E => [(fs, 0xE), (ft, 0xE)],                           // OPMSUB
        _ => [(fs, dest), (ft, dest)],
    }
}
fn upper_reads_acc(op: u32) -> bool {
    let f = op & 0x3F;
    if f >= 0x3C {
        let n = (op >> 6 & 31) << 2 | (op & 3);
        return matches!(n, 0x08..=0x0F | 0x21 | 0x23 | 0x25 | 0x27 | 0x29 | 0x2D);
    }
    matches!(f, 0x08..=0x0F | 0x21 | 0x23 | 0x25 | 0x27 | 0x29 | 0x2D | 0x2E)
}

/// VU stall rules. Default (patch 0281): PCSX2 microVU's (per field, no vf0 / ACC
/// stalls, lower VF writes land +4): Legaia 2's first scene builds right with them, wrecked
/// with the old ones. PS2_VU_STALLS=old: the model up to 0280 (whole registers, ACC stalls,
/// only flag-setting upper ops wait). (0271's broken Spy Hunter / KH frames were 0270's
/// INT0 timing, not these.)
pub(crate) fn pcsx2_stalls() -> bool {
    static P: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *P.get_or_init(|| std::env::var("PS2_VU_STALLS").map_or(true, |v| v != "old"))
}

/// A pair decoded for `step`: the VF fields it reads (stalls), the VI its lower half
/// writes and whether that's a flag read (FCEQ..FCGET)
#[derive(Clone, Copy)]
struct Dec { raw: u64, ok: bool, n: u8, rd: [(u8, u8); 4], vit: u8, vflag: bool, up: UpH, lo: LoH }

type UpH = fn(&mut Vu, u32) -> Option<(Dst, [u32; 4], bool)>;
type LoH = fn(&mut Vu, u32, &mut [u8], &mut Vec<u128>);

impl Default for Dec {
    fn default() -> Self {
        Dec { raw: 0, ok: false, n: 0, rd: [(0, 0); 4], vit: 0, vflag: false, up: up_bad, lo: lo_any }
    }
}

impl Dec {
    fn decode(lo: u32, hi: u32) -> Self {
        let mut d = Dec { raw: (hi as u64) << 32 | lo as u64, ok: true, up: up_handler(hi), lo: lo_handler(lo), ..Default::default() };
        let lower = hi & (1 << 31) == 0;
        let mut all = upper_reads(hi).to_vec();
        if lower { all.extend(lower_reads(lo)); }
        for (r, m) in all {
            if m != 0 && r != 0 { d.rd[d.n as usize] = (r as u8, m as u8); d.n += 1; }
        }
        if lower {
            d.vit = lower_vi_target(lo) as u8;
            d.vflag = (0x10..=0x1C).contains(&(lo >> 25));
        }
        d
    }
}

// ---------------------------------------------------------------- pair handlers
// `step` calls the handler picked when the pair was decoded: the common FMAC forms
// directly (same `fmac` as `upper`), everything else through `upper` / `lower`.

fn up_bad(vu: &mut Vu, _: u32) -> Option<(Dst, [u32; 4], bool)> { vu.illegal += 1; None }

/// MAX / MINI with a broadcast field, I, or ft
fn up_mm<const MAX: bool, const SRC: u8>(vu: &mut Vu, op: u32) -> Option<(Dst, [u32; 4], bool)> {
    let dest = op >> 21 & 15;
    let ft = (op >> 16 & 31) as usize;
    let s = vu.vf[(op >> 11 & 31) as usize];
    let b = match SRC { BC => [vu.vf[ft][(op & 3) as usize]; 4], I => [vu.i; 4], _ => vu.vf[ft] };
    Some((Dst::Vf((op >> 6 & 31) as usize, dest), minmax(s, b, MAX), false))
}
fn up_itof(vu: &mut Vu, op: u32) -> Option<(Dst, [u32; 4], bool)> {
    let sh = [0, 4, 12, 15][(op & 3) as usize];
    let s = vu.vf[(op >> 11 & 31) as usize];
    let v = s.map(|x| from_f64(x as i32 as f64 / (1u32 << sh) as f64).0);
    Some((Dst::Vf((op >> 16 & 31) as usize, op >> 21 & 15), v, false))
}
fn up_ftoi(vu: &mut Vu, op: u32) -> Option<(Dst, [u32; 4], bool)> {
    let sh = [0, 4, 12, 15][(op & 3) as usize];
    let s = vu.vf[(op >> 11 & 31) as usize];
    let v = s.map(|x| {
        let f = to_f64(x) * (1u32 << sh) as f64;
        f.trunc().clamp(i32::MIN as f64, i32::MAX as f64) as i32 as u32
    });
    Some((Dst::Vf((op >> 16 & 31) as usize, op >> 21 & 15), v, false))
}
fn up_abs(vu: &mut Vu, op: u32) -> Option<(Dst, [u32; 4], bool)> {
    let s = vu.vf[(op >> 11 & 31) as usize];
    Some((Dst::Vf((op >> 16 & 31) as usize, op >> 21 & 15), s.map(|x| x & 0x7FFF_FFFF), false))
}
fn up_clip(vu: &mut Vu, op: u32) -> Option<(Dst, [u32; 4], bool)> {
    let s = vu.vf[(op >> 11 & 31) as usize];
    let t = vu.vf[(op >> 16 & 31) as usize];
    let w = to_f64(t[3]).abs();
    let mut c = 0;
    for k in 0..3 {
        let v = to_f64(s[k]);
        if v > w { c |= 1 << (2 * k); }
        if v < -w { c |= 2 << (2 * k); }
    }
    vu.flags.push_back(Flags { at: vu.cycle + MAC_LATENCY, mac: 0, stat: 0, clip: c, is_clip: 1 });
    None
}
/// OPMULA (ACC) / OPMSUB (fd): the cross product's halves
fn up_op<const ACC: bool>(vu: &mut Vu, op: u32) -> Option<(Dst, [u32; 4], bool)> {
    let dest = op >> 21 & 15;
    let s = vu.vf[(op >> 11 & 31) as usize];
    let t = vu.vf[(op >> 16 & 31) as usize];
    let (a, b) = ([s[1], s[2], s[0], 0], [t[2], t[0], t[1], 0]);
    if ACC { vu.fmac(Dst::Acc(dest), dest, a, b, Arith::Mul) }
    else { vu.fmac(Dst::Vf((op >> 6 & 31) as usize, dest), dest, a, b, Arith::Msub) }
}
fn up_nop(_: &mut Vu, _: u32) -> Option<(Dst, [u32; 4], bool)> { None }
fn lo_k<const K: u32>(vu: &mut Vu, op: u32, data: &mut [u8], path1: &mut Vec<u128>) { vu.lower_k(K, 0, 0, op, data, path1) }
fn lo_sp<const F: u32, const N: u32>(vu: &mut Vu, op: u32, data: &mut [u8], path1: &mut Vec<u128>) { vu.lower_k(0x40, F, N, op, data, path1) }
/// anything else: the general path (same code, opcode not fixed)
fn lo_any(vu: &mut Vu, op: u32, data: &mut [u8], path1: &mut Vec<u128>) {
    vu.lower_k(op >> 25, op & 0x3F, (op >> 6 & 31) << 2 | (op & 3), op, data, path1)
}
fn lo_nop(_: &mut Vu, _: u32, _: &mut [u8], _: &mut Vec<u128>) {}

pub(crate) const ADD: u8 = 0;
pub(crate) const SUB: u8 = 1;
pub(crate) const MUL: u8 = 2;
pub(crate) const MADD: u8 = 3;
pub(crate) const MSUB: u8 = 4;
/// second operand: broadcast field of ft, Q, I, or all of ft
pub(crate) const BC: u8 = 0;
pub(crate) const Q: u8 = 1;
pub(crate) const I: u8 = 2;
pub(crate) const T: u8 = 3;

fn up_fm<const K: u8, const SRC: u8, const ACC: bool>(vu: &mut Vu, op: u32) -> Option<(Dst, [u32; 4], bool)> {
    let dest = op >> 21 & 15;
    let ft = (op >> 16 & 31) as usize;
    let fs = (op >> 11 & 31) as usize;
    let fd = (op >> 6 & 31) as usize;
    let s = vu.vf[fs];
    let b = match SRC { BC => [vu.vf[ft][(op & 3) as usize]; 4], Q => [vu.q; 4], I => [vu.i; 4], _ => vu.vf[ft] };
    let d = if ACC { Dst::Acc(dest) } else { Dst::Vf(fd, dest) };
    let k = match K { ADD => Arith::Add, SUB => Arith::Sub, MUL => Arith::Mul, MADD => Arith::Madd, _ => Arith::Msub };
    vu.fmac(d, dest, s, b, k)
}

/// What an upper instruction is, for the JIT: an FMAC form (`up_fm::<K, SRC, ACC>`), NOP,
/// or something else
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum UpClass { Fm(u8, u8, bool), Mm(bool, u8), Itof(u32), Ftoi(u32), Abs, Clip, Nop, Other }

pub(crate) fn up_class(op: u32) -> UpClass {
    use UpClass::*;
    let f = op & 0x3F;
    if f >= 0x3C {
        return match (op >> 6 & 31) << 2 | (op & 3) {
            0x00..=0x03 => Fm(ADD, BC, true),
            0x04..=0x07 => Fm(SUB, BC, true),
            0x08..=0x0B => Fm(MADD, BC, true),
            0x0C..=0x0F => Fm(MSUB, BC, true),
            0x18..=0x1B => Fm(MUL, BC, true),
            0x1C => Fm(MUL, Q, true),
            0x1E => Fm(MUL, I, true),
            0x20 => Fm(ADD, Q, true),
            0x21 => Fm(MADD, Q, true),
            0x22 => Fm(ADD, I, true),
            0x23 => Fm(MADD, I, true),
            0x24 => Fm(SUB, Q, true),
            0x25 => Fm(MSUB, Q, true),
            0x26 => Fm(SUB, I, true),
            0x27 => Fm(MSUB, I, true),
            0x28 => Fm(ADD, T, true),
            0x29 => Fm(MADD, T, true),
            0x2A => Fm(MUL, T, true),
            0x2C => Fm(SUB, T, true),
            0x2D => Fm(MSUB, T, true),
            0x10..=0x13 => Itof(op & 3),
            0x14..=0x17 => Ftoi(op & 3),
            0x1D => Abs,
            0x1F => Clip,
            0x2F => Nop,
            _ => Other,
        };
    }
    match f {
        0x00..=0x03 => Fm(ADD, BC, false),
        0x04..=0x07 => Fm(SUB, BC, false),
        0x08..=0x0B => Fm(MADD, BC, false),
        0x0C..=0x0F => Fm(MSUB, BC, false),
        0x18..=0x1B => Fm(MUL, BC, false),
        0x1C => Fm(MUL, Q, false),
        0x1E => Fm(MUL, I, false),
        0x20 => Fm(ADD, Q, false),
        0x21 => Fm(MADD, Q, false),
        0x22 => Fm(ADD, I, false),
        0x23 => Fm(MADD, I, false),
        0x24 => Fm(SUB, Q, false),
        0x25 => Fm(MSUB, Q, false),
        0x26 => Fm(SUB, I, false),
        0x27 => Fm(MSUB, I, false),
        0x28 => Fm(ADD, T, false),
        0x29 => Fm(MADD, T, false),
        0x2A => Fm(MUL, T, false),
        0x2C => Fm(SUB, T, false),
        0x2D => Fm(MSUB, T, false),
        0x10..=0x13 => Mm(true, BC),
        0x14..=0x17 => Mm(false, BC),
        0x1D => Mm(true, I),
        0x1F => Mm(false, I),
        0x2B => Mm(true, T),
        0x2F => Mm(false, T),
        _ => Other,
    }
}

fn up_handler(op: u32) -> UpH {
    // the FMAC forms come from `up_class` (the JIT's view too)
    if let UpClass::Fm(k, s, a) = up_class(op) {
        macro_rules! fm { ($($k:ident)*) => { $( if k == $k {
            return match (s, a) {
                (BC, true) => up_fm::<$k, BC, true>, (Q, true) => up_fm::<$k, Q, true>,
                (I, true) => up_fm::<$k, I, true>, (_, true) => up_fm::<$k, T, true>,
                (BC, false) => up_fm::<$k, BC, false>, (Q, false) => up_fm::<$k, Q, false>,
                (I, false) => up_fm::<$k, I, false>, (_, false) => up_fm::<$k, T, false>,
            };
        } )* } }
        fm!(ADD SUB MUL MADD MSUB);
    }
    let f = op & 0x3F;
    if f >= 0x3C {
        return match (op >> 6 & 31) << 2 | (op & 3) {
            0x00..=0x03 => up_fm::<ADD, BC, true>,
            0x04..=0x07 => up_fm::<SUB, BC, true>,
            0x08..=0x0B => up_fm::<MADD, BC, true>,
            0x0C..=0x0F => up_fm::<MSUB, BC, true>,
            0x10..=0x13 => up_itof,
            0x14..=0x17 => up_ftoi,
            0x18..=0x1B => up_fm::<MUL, BC, true>,
            0x1C => up_fm::<MUL, Q, true>,
            0x1D => up_abs,
            0x1F => up_clip,
            0x1E => up_fm::<MUL, I, true>,
            0x20 => up_fm::<ADD, Q, true>,
            0x21 => up_fm::<MADD, Q, true>,
            0x22 => up_fm::<ADD, I, true>,
            0x23 => up_fm::<MADD, I, true>,
            0x24 => up_fm::<SUB, Q, true>,
            0x25 => up_fm::<MSUB, Q, true>,
            0x26 => up_fm::<SUB, I, true>,
            0x27 => up_fm::<MSUB, I, true>,
            0x28 => up_fm::<ADD, T, true>,
            0x29 => up_fm::<MADD, T, true>,
            0x2A => up_fm::<MUL, T, true>,
            0x2C => up_fm::<SUB, T, true>,
            0x2D => up_fm::<MSUB, T, true>,
            0x2E => up_op::<true>,
            0x2F => up_nop,
            _ => up_bad,
        };
    }
    match f {
        0x00..=0x03 => up_fm::<ADD, BC, false>,
        0x04..=0x07 => up_fm::<SUB, BC, false>,
        0x08..=0x0B => up_fm::<MADD, BC, false>,
        0x0C..=0x0F => up_fm::<MSUB, BC, false>,
        0x10..=0x13 => up_mm::<true, BC>,
        0x14..=0x17 => up_mm::<false, BC>,
        0x18..=0x1B => up_fm::<MUL, BC, false>,
        0x1C => up_fm::<MUL, Q, false>,
        0x1D => up_mm::<true, I>,
        0x1E => up_fm::<MUL, I, false>,
        0x1F => up_mm::<false, I>,
        0x20 => up_fm::<ADD, Q, false>,
        0x21 => up_fm::<MADD, Q, false>,
        0x22 => up_fm::<ADD, I, false>,
        0x23 => up_fm::<MADD, I, false>,
        0x24 => up_fm::<SUB, Q, false>,
        0x25 => up_fm::<MSUB, Q, false>,
        0x26 => up_fm::<SUB, I, false>,
        0x27 => up_fm::<MSUB, I, false>,
        0x28 => up_fm::<ADD, T, false>,
        0x29 => up_fm::<MADD, T, false>,
        0x2A => up_fm::<MUL, T, false>,
        0x2B => up_mm::<true, T>,
        0x2C => up_fm::<SUB, T, false>,
        0x2D => up_fm::<MSUB, T, false>,
        0x2E => up_op::<false>,
        0x2F => up_mm::<false, T>,
        _ => up_bad,
    }
}

fn lo_handler(op: u32) -> LoH {
    // MOVE / MR32 to vf0: nothing (the usual lower NOP)
    if op >> 25 == 0x40 && op & 0x3C == 0x3C && matches!((op >> 6 & 31) << 2 | (op & 3), 0x30 | 0x31) && op >> 16 & 31 == 0 {
        return lo_nop;
    }
    macro_rules! k { ($($v:literal)*) => { match op >> 25 { $($v => return lo_k::<$v>,)* _ => {} } } }
    k!(0x00 0x01 0x04 0x05 0x08 0x09 0x10 0x11 0x12 0x13 0x14 0x15 0x16 0x17 0x18 0x1A 0x1B 0x1C
       0x20 0x21 0x24 0x25 0x28 0x29 0x2C 0x2D 0x2E 0x2F);
    if op >> 25 == 0x40 {
        macro_rules! f { ($($v:literal)*) => { match op & 0x3F { $($v => return lo_sp::<$v, 0>,)* _ => {} } } }
        f!(0x30 0x31 0x32 0x34 0x35);
        if op & 0x3C == 0x3C {
            macro_rules! n { ($($v:literal)*) => { match (op >> 6 & 31) << 2 | (op & 3) { $($v => return lo_sp::<0x3C, $v>,)* _ => {} } } }
            n!(0x30 0x31 0x34 0x35 0x36 0x37 0x38 0x39 0x3A 0x3B 0x3C 0x3D 0x3E 0x3F 0x40 0x41 0x42 0x43
               0x64 0x68 0x69 0x6C 0x70 0x71 0x72 0x73 0x74 0x75 0x76 0x78 0x79 0x7A 0x7B 0x7C 0x7D 0x7E);
        }
    }
    lo_any
}

/// The VI register a lower instruction may write (0: none)
pub(crate) fn lower_vi_target(op: u32) -> usize {
    let it = (op >> 16 & 15) as usize;
    let is = (op >> 11 & 15) as usize;
    match op >> 25 {
        0x04 | 0x08 | 0x09 | 0x14 | 0x16 | 0x17 | 0x18 | 0x1A | 0x1B | 0x1C | 0x21 | 0x25 => it,
        0x10 | 0x12 | 0x13 => 1,
        0x40 => match op & 0x3F {
            0x30 | 0x31 | 0x34 | 0x35 => (op >> 6 & 15) as usize,
            0x32 => it,
            0x3C..=0x3F => match (op >> 6 & 31) << 2 | (op & 3) {
                0x34 | 0x36 => is,
                0x35 | 0x37 | 0x3C | 0x3E | 0x68 | 0x69 => it,
                _ => 0,
            },
            _ => 0,
        },
        _ => 0,
    }
}

/// A pair's stall inputs (patch 0270's model), cached by address
#[derive(Clone, Copy, Default)]
struct Reads { lo: u32, hi: u32, valid: bool, vf: u32, acc: bool }

impl Reads {
    fn decode(lo: u32, hi: u32) -> Self {
        let mut vf = 0u32;
        for r in upper_reads_old(hi).into_iter().flatten() { vf |= 1 << r; }
        if hi & (1 << 31) == 0 { for r in lower_reads_old(lo).into_iter().flatten() { vf |= 1 << r; } }
        Reads { lo, hi, valid: true, vf, acc: upper_reads_acc(hi) }
    }
}

/// Patch 0270's read lists (registers, no fields)
fn upper_reads_old(op: u32) -> [Option<usize>; 2] {
    let ft = (op >> 16 & 31) as usize;
    let fs = (op >> 11 & 31) as usize;
    let f = op & 0x3F;
    if f >= 0x3C {
        let n = (op >> 6 & 31) << 2 | (op & 3);
        if n == 0x2F { return [None, None]; }
        if (0x10..=0x17).contains(&n) || n == 0x1D { return [Some(fs), None]; }
        return [Some(fs), Some(ft)];
    }
    match f {
        0x1C..=0x27 => [Some(fs), None],
        _ => [Some(fs), Some(ft)],
    }
}
fn lower_reads_old(op: u32) -> [Option<usize>; 2] {
    let ft = (op >> 16 & 31) as usize;
    let fs = (op >> 11 & 31) as usize;
    match op >> 25 {
        0x01 => [Some(fs), None],
        0x40 if op & 0x3C == 0x3C => {
            let n = (op >> 6 & 31) << 2 | (op & 3);
            match n {
                0x30 | 0x31 | 0x35 | 0x37 | 0x3C | 0x42 | 0x43 => [Some(fs), None],
                0x38 | 0x3A => [Some(fs), Some(ft)],
                0x39 => [Some(ft), None],
                0x70..=0x7E => [Some(fs), None],
                _ => [None, None],
            }
        }
        _ => [None, None],
    }
}

/// VF registers and fields a lower instruction reads
fn lower_reads(op: u32) -> [(usize, u32); 2] {
    let ft = (op >> 16 & 31) as usize;
    let fs = (op >> 11 & 31) as usize;
    let dest = op >> 21 & 15;
    let fsf = 8 >> (op >> 21 & 3);
    let ftf = 8 >> (op >> 23 & 3);
    let none = [(0, 0), (0, 0)];
    match op >> 25 {
        0x01 => [(fs, dest), (0, 0)],
        0x40 if op & 0x3C == 0x3C => {
            let n = (op >> 6 & 31) << 2 | (op & 3);
            match n {
                0x30 | 0x35 | 0x37 => [(fs, dest), (0, 0)],
                0x31 => [(fs, 0xF), (0, 0)],
                0x3C | 0x42 | 0x43 | 0x78..=0x7E => [(fs, fsf), (0, 0)],
                0x38 | 0x3A => [(fs, fsf), (ft, ftf)],
                0x39 => [(ft, ftf), (0, 0)],
                0x70..=0x73 => [(fs, 0xE), (0, 0)],
                0x74 => [(fs, 0xC), (0, 0)],
                0x75 => [(fs, 0xA), (0, 0)],
                0x76 => [(fs, 0xF), (0, 0)],
                _ => none,
            }
        }
        _ => none,
    }
}
