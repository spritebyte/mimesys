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
use std::collections::VecDeque;

use crate::common::cpu::r5900::fpu::{from_f64, ps2_add, ps2_div, ps2_mul, ps2_sub, to_f64, F_O, F_U};

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

#[derive(Clone, Copy, Serialize, Deserialize, Default)]
struct Flags { at: u64, mac: u32, stat: u32, clip: Option<u32> }

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
    /// cycle count of the current program
    cycle: u64,
    vf_ready: [u64; 32],
    acc_ready: u64,
    flags: VecDeque<Flags>,
    q_pend: Option<(u64, u32, u32)>,
    p_pend: Option<(u64, u32)>,
    /// branch target after the delay slot
    branch: Option<u32>,
    /// E bit seen: stop after the next pair
    ending: u8,
    /// VU1: VIF1's TOP / ITOP, read by XTOP / XITOP
    pub top: u16,
    pub itop: u16,
    /// debugging
    #[serde(skip)]
    pub programs: u64,
    #[serde(skip)]
    pub kicks: u64,
    #[serde(skip)]
    pub illegal: u64,
    /// cycles run, all programs (speed report)
    #[serde(skip)]
    pub total_cycles: u64,
}

impl Vu {
    pub fn new(vu1: bool) -> Self {
        let mut vf = [[0u32; 4]; 32];
        vf[0][3] = 0x3F80_0000;
        Self {
            vu1, vf, vi: [0; 16], acc: [0; 4], q: 0, p: 0, i: 0, r: 0, status: 0, mac: 0, clip: 0,
            pc: 0, tpc: 0, running: false, cycle: 0, vf_ready: [0; 32], acc_ready: 0,
            flags: VecDeque::new(), q_pend: None, p_pend: None, branch: None, ending: 0,
            top: 0, itop: 0, programs: 0, kicks: 0, illegal: 0, total_cycles: 0,
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
        self.vf_ready = [0; 32];
        self.acc_ready = 0;
        self.cycle = 0;
    }

    fn land_flags(&mut self) {
        while let Some(f) = self.flags.front().copied() {
            if f.at > self.cycle { break; }
            self.flags.pop_front();
            if let Some(c) = f.clip {
                self.clip = (self.clip << 6 | c) & 0xFF_FFFF;
            } else {
                self.mac = f.mac;
                self.status = (self.status & !0xF) | f.stat | (f.stat << 6);
            }
        }
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
        self.pc = addr;
        self.running = true;
        self.branch = None;
        self.ending = 0;
        self.programs += 1;
    }

    /// Run until the program ends. `code` / `data` are this VU's memories; XGKICK
    /// packets go to `path1`.
    pub fn run(&mut self, code: &[u8], data: &mut [u8], path1: &mut Vec<u128>) {
        let start = self.cycle;
        while self.running {
            self.step(code, data, path1);
            if self.cycle - start > MAX_CYCLES { self.running = false; }
        }
        self.tpc = self.pc;
        self.total_cycles += self.cycle - start;
        self.settle();
    }

    fn step(&mut self, code: &[u8], data: &mut [u8], path1: &mut Vec<u128>) {
        let a = self.pc as usize & (code.len() - 1) & !7;
        let lo = u32::from_le_bytes(code[a..a + 4].try_into().unwrap());
        let hi = u32::from_le_bytes(code[a + 4..a + 8].try_into().unwrap());
        // a branch in the pair before: this pair is its delay slot
        let delay = self.branch.take();
        self.pc = self.pc.wrapping_add(8);

        // stalls: VF sources an FMAC is still writing
        let mut ready = 0;
        for r in upper_reads(hi) { ready = ready.max(self.vf_ready[r]); }
        if hi & (1 << 31) == 0 { for r in lower_reads(lo) { ready = ready.max(self.vf_ready[r]); } }
        if upper_reads_acc(hi) { ready = ready.max(self.acc_ready); }
        self.cycle = self.cycle.max(ready);
        self.land_flags();
        self.land_q();
        self.land_p();

        let up = self.upper(hi);
        if hi & (1 << 31) != 0 {
            self.i = lo;                                     // I bit: the lower word is I
        } else {
            self.lower(lo, data, path1);
        }
        self.commit(up);
        self.cycle += 1;
        if let Some(t) = delay { self.pc = t; }
        if self.ending > 0 {
            self.ending -= 1;
            if self.ending == 0 { self.running = false; }
        }
        if hi & (1 << 30) != 0 { self.ending = 1; }
    }

    /// Write an upper instruction's result (after the lower half has run)
    fn commit(&mut self, up: Option<(Dst, [u32; 4], bool)>) {
        let Some((r, v, flags)) = up else { return };
        match r {
            Dst::Vf(r, dest) => {
                if r != 0 {
                    for k in 0..4 { if dest & (8 >> k) != 0 { self.vf[r][k] = v[k]; } }
                }
                if flags { self.vf_ready[r] = self.cycle + MAC_LATENCY; }
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
        let dest = op >> 21 & 15;
        let ft = (op >> 16 & 31) as usize;
        let fs = (op >> 11 & 31) as usize;
        let fd = (op >> 6 & 31) as usize;
        let bc = (op & 3) as usize;
        let f = op & 0x3F;
        let s = self.vf[fs];
        let t = self.vf[ft];
        let tb = [t[bc]; 4];
        let qv = [self.q; 4];
        let iv = [self.i; 4];
        use Arith::*;
        let fmac = |me: &mut Self, d: Dst, a: [u32; 4], b: [u32; 4], k: Arith| me.fmac(d, dest, a, b, k);
        let vd = Dst::Vf(fd, dest);
        let ac = Dst::Acc(dest);
        match f {
            0x00..=0x03 => fmac(self, vd, s, tb, Add),
            0x04..=0x07 => fmac(self, vd, s, tb, Sub),
            0x08..=0x0B => fmac(self, vd, s, tb, Madd),
            0x0C..=0x0F => fmac(self, vd, s, tb, Msub),
            0x10..=0x13 => Some((vd, minmax(s, tb, true), false)),
            0x14..=0x17 => Some((vd, minmax(s, tb, false), false)),
            0x18..=0x1B => fmac(self, vd, s, tb, Mul),
            0x1C => fmac(self, vd, s, qv, Mul),
            0x1D => Some((vd, minmax(s, iv, true), false)),
            0x1E => fmac(self, vd, s, iv, Mul),
            0x1F => Some((vd, minmax(s, iv, false), false)),
            0x20 => fmac(self, vd, s, qv, Add),
            0x21 => fmac(self, vd, s, qv, Madd),
            0x22 => fmac(self, vd, s, iv, Add),
            0x23 => fmac(self, vd, s, iv, Madd),
            0x24 => fmac(self, vd, s, qv, Sub),
            0x25 => fmac(self, vd, s, qv, Msub),
            0x26 => fmac(self, vd, s, iv, Sub),
            0x27 => fmac(self, vd, s, iv, Msub),
            0x28 => fmac(self, vd, s, t, Add),
            0x29 => fmac(self, vd, s, t, Madd),
            0x2A => fmac(self, vd, s, t, Mul),
            0x2B => Some((vd, minmax(s, t, true), false)),
            0x2C => fmac(self, vd, s, t, Sub),
            0x2D => fmac(self, vd, s, t, Msub),
            0x2E => {                                        // OPMSUB
                let (a, b) = ([s[1], s[2], s[0], 0], [t[2], t[0], t[1], 0]);
                self.fmac(vd, dest, a, b, Msub)
            }
            0x2F => Some((vd, minmax(s, t, false), false)),
            0x3C..=0x3F => {
                let n = (op >> 6 & 31) << 2 | (op & 3);
                let ftd = Dst::Vf(ft, dest);
                match n {
                    0x00..=0x03 => fmac(self, ac, s, tb, Add),
                    0x04..=0x07 => fmac(self, ac, s, tb, Sub),
                    0x08..=0x0B => fmac(self, ac, s, tb, Madd),
                    0x0C..=0x0F => fmac(self, ac, s, tb, Msub),
                    0x10..=0x13 => {                         // ITOF0/4/12/15
                        let sh = [0, 4, 12, 15][bc];
                        let v = s.map(|x| from_f64(x as i32 as f64 / (1u32 << sh) as f64).0);
                        Some((ftd, v, false))
                    }
                    0x14..=0x17 => {                         // FTOI0/4/12/15
                        let sh = [0, 4, 12, 15][bc];
                        let v = s.map(|x| {
                            let f = to_f64(x) * (1u32 << sh) as f64;
                            f.trunc().clamp(i32::MIN as f64, i32::MAX as f64) as i32 as u32
                        });
                        Some((ftd, v, false))
                    }
                    0x18..=0x1B => fmac(self, ac, s, tb, Mul),
                    0x1C => fmac(self, ac, s, qv, Mul),
                    0x1D => Some((ftd, s.map(|x| x & 0x7FFF_FFFF), false)),   // ABS
                    0x1E => fmac(self, ac, s, iv, Mul),
                    0x1F => {                                // CLIP
                        let w = to_f64(t[3]).abs();
                        let mut c = 0;
                        for k in 0..3 {
                            let v = to_f64(s[k]);
                            if v > w { c |= 1 << (2 * k); }
                            if v < -w { c |= 2 << (2 * k); }
                        }
                        self.flags.push_back(Flags { at: self.cycle + MAC_LATENCY, mac: 0, stat: 0, clip: Some(c) });
                        None
                    }
                    0x20 => fmac(self, ac, s, qv, Add),
                    0x21 => fmac(self, ac, s, qv, Madd),
                    0x22 => fmac(self, ac, s, iv, Add),
                    0x23 => fmac(self, ac, s, iv, Madd),
                    0x24 => fmac(self, ac, s, qv, Sub),
                    0x25 => fmac(self, ac, s, qv, Msub),
                    0x26 => fmac(self, ac, s, iv, Sub),
                    0x27 => fmac(self, ac, s, iv, Msub),
                    0x28 => fmac(self, ac, s, t, Add),
                    0x29 => fmac(self, ac, s, t, Madd),
                    0x2A => fmac(self, ac, s, t, Mul),
                    0x2C => fmac(self, ac, s, t, Sub),
                    0x2D => fmac(self, ac, s, t, Msub),
                    0x2E => {                                // OPMULA
                        let (a, b) = ([s[1], s[2], s[0], 0], [t[2], t[0], t[1], 0]);
                        self.fmac(ac, dest, a, b, Mul)
                    }
                    0x2F => None,                            // NOP
                    _ => { self.illegal += 1; None }
                }
            }
            _ => { self.illegal += 1; None }
        }
    }

    /// An FMAC operation on the fields in `dest`, queueing its flags
    fn fmac(&mut self, d: Dst, dest: u32, a: [u32; 4], b: [u32; 4], k: Arith) -> Option<(Dst, [u32; 4], bool)> {
        let mut out = match d { Dst::Vf(r, _) => self.vf[r], Dst::Acc(_) => self.acc };
        let (mut mac, mut stat) = (0u32, 0u32);
        for i in 0..4 {
            if dest & (8 >> i) == 0 { continue; }
            let (v, fl) = match k {
                Arith::Add => ps2_add(a[i], b[i]),
                Arith::Sub => ps2_sub(a[i], b[i]),
                Arith::Mul => ps2_mul(a[i], b[i]),
                Arith::Madd => { let (m, f1) = ps2_mul(a[i], b[i]); let (v, f2) = ps2_add(self.acc[i], m); (v, f1 | f2) }
                Arith::Msub => { let (m, f1) = ps2_mul(a[i], b[i]); let (v, f2) = ps2_sub(self.acc[i], m); (v, f1 | f2) }
            };
            out[i] = v;
            let bit = 3 - i;
            if v & 0x7FFF_FFFF == 0 { mac |= 1 << bit; stat |= ST_Z; }
            if v & 0x8000_0000 != 0 { mac |= 1 << (bit + 4); stat |= ST_S; }
            if fl & F_U != 0 { mac |= 1 << (bit + 8); stat |= ST_U; }
            if fl & F_O != 0 { mac |= 1 << (bit + 12); stat |= ST_O; }
        }
        self.flags.push_back(Flags { at: self.cycle + MAC_LATENCY, mac, stat, clip: None });
        Some((d, out, true))
    }

    // ================================================================ lower

    pub fn lower(&mut self, op: u32, data: &mut [u8], path1: &mut Vec<u128>) {
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
        match op >> 25 {
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
            0x24 => self.branch = Some(vi(self, is) * 8),                                  // JR
            0x25 => {                                                                      // JALR
                let t = vi(self, is) * 8;
                let l = self.pc.wrapping_add(8) / 8;
                self.set_vi(it, l);
                self.branch = Some(t);
            }
            0x28 => if self.vi[it] == self.vi[is] { self.jump_rel(imm11) },               // IBEQ
            0x29 => if self.vi[it] != self.vi[is] { self.jump_rel(imm11) },               // IBNE
            0x2C => if (self.vi[is] as i16) < 0 { self.jump_rel(imm11) },                 // IBLTZ
            0x2D => if (self.vi[is] as i16) > 0 { self.jump_rel(imm11) },                 // IBGTZ
            0x2E => if (self.vi[is] as i16) <= 0 { self.jump_rel(imm11) },                // IBLEZ
            0x2F => if (self.vi[is] as i16) >= 0 { self.jump_rel(imm11) },                // IBGEZ
            0x40 => self.lower_special(op, data, path1, dest, ft, fs, it, is, fsf, ftf),
            _ => self.illegal += 1,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_special(&mut self, op: u32, data: &mut [u8], path1: &mut Vec<u128>, dest: u32,
                     ft: usize, fs: usize, it: usize, is: usize, fsf: usize, ftf: usize) {
        let id = (op >> 6 & 15) as usize;
        let vi = |me: &Self, r: usize| me.vi[r] as u32;
        let qmask = (data.len() / 16 - 1) as u32;
        match op & 0x3F {
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
                let n = (op >> 6 & 31) << 2 | (op & 3);
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
    fn land_all_clip(&mut self) { self.flags.retain(|f| f.clip.is_none()); }

    fn jump_rel(&mut self, imm: u32) {
        // relative to the instruction after the branch
        self.branch = Some(self.pc.wrapping_add(imm.wrapping_mul(8)));
    }

    fn set_vi(&mut self, r: usize, v: u32) { if r != 0 { self.vi[r & 15] = v as u16; } }

    fn write_vf(&mut self, r: usize, dest: u32, v: [u32; 4]) {
        if r == 0 { return; }
        for k in 0..4 { if dest & (8 >> k) != 0 { self.vf[r][k] = v[k]; } }
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

    /// XGKICK: the GIF packet at `qa` (quadwords) goes to the GS as PATH1
    fn xgkick(&mut self, data: &[u8], qa: u32, path1: &mut Vec<u128>) {
        self.kicks += 1;
        let qmask = (data.len() / 16 - 1) as u32;
        let mut a = qa & qmask;
        let rd = |a: u32| -> u128 { u128::from_le_bytes(data[a as usize * 16..a as usize * 16 + 16].try_into().unwrap()) };
        for _ in 0..64 {
            let tag = rd(a);
            path1.push(tag);
            a = (a + 1) & qmask;
            let nloop = (tag & 0x7FFF) as u32;
            let eop = tag >> 15 & 1 != 0;
            let flg = (tag >> 58 & 3) as u32;
            let nreg = match (tag >> 60 & 15) as u32 { 0 => 16, n => n };
            let n = match flg { 0 => nloop * nreg, 1 => (nloop * nreg).div_ceil(2), _ => nloop };
            for _ in 0..n.min(qmask + 1) {
                path1.push(rd(a));
                a = (a + 1) & qmask;
            }
            if eop { break; }
        }
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

/// DIV: x / y with the I (0 / 0) and D (x / 0) status bits
fn vu_div(x: u32, y: u32) -> (u32, u32) {
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

/// VF registers an upper instruction reads (for stalls)
fn upper_reads(op: u32) -> Vec<usize> {
    let ft = (op >> 16 & 31) as usize;
    let fs = (op >> 11 & 31) as usize;
    let f = op & 0x3F;
    if f >= 0x3C {
        let n = (op >> 6 & 31) << 2 | (op & 3);
        if n == 0x2F { return vec![]; }
        if (0x10..=0x17).contains(&n) || n == 0x1D { return vec![fs]; }
        return vec![fs, ft];
    }
    match f {
        0x1C..=0x27 => vec![fs],
        _ => vec![fs, ft],
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
/// VF registers a lower instruction reads
fn lower_reads(op: u32) -> Vec<usize> {
    let ft = (op >> 16 & 31) as usize;
    let fs = (op >> 11 & 31) as usize;
    match op >> 25 {
        0x01 => vec![fs],
        0x40 if op & 0x3C == 0x3C => {
            let n = (op >> 6 & 31) << 2 | (op & 3);
            match n {
                0x30 | 0x31 | 0x35 | 0x37 | 0x3C | 0x42 | 0x43 => vec![fs],
                0x38 | 0x3A => vec![fs, ft],
                0x39 => vec![ft],
                0x70..=0x7E => vec![fs],
                _ => vec![],
            }
        }
        _ => vec![],
    }
}
