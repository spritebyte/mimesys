//! SH-4 FPU: 16 single-precision registers in two banks (FPSCR.FR swaps FR / XF), pairs as
//! doubles (FPSCR.PR) or 64-bit moves (FPSCR.SZ), FPUL for transfers, and the vector ops
//! FIPR / FTRV the Dreamcast's geometry code is built on.
//!
//! A double DRn is FR(n) (sign / high half) and FR(n + 1). A 64-bit memory move puts the
//! word at the lower address in FR(n).

use super::*;
use super::fparith::{Mode, Op};

impl Sh4 {
    pub fn set_fpscr(&mut self, v: u32) {
        let v = v & FPSCR_MASK;
        if (v ^ self.fpscr) & FPSCR_FR != 0 { std::mem::swap(&mut self.fr, &mut self.xf); }
        self.fpscr = v;
    }

    fn f(&self, n: usize) -> f32 { f32::from_bits(self.fr[n]) }
    fn set_f(&mut self, n: usize, v: f32) { self.fr[n] = v.to_bits(); }
    fn d(&self, n: usize) -> f64 { f64::from_bits((self.fr[n & 14] as u64) << 32 | self.fr[n | 1] as u64) }
    fn set_d(&mut self, n: usize, v: f64) {
        let b = v.to_bits();
        self.fr[n & 14] = (b >> 32) as u32;
        self.fr[n | 1] = b as u32;
    }

    /// A 64-bit register for SZ moves: even n = DRn, odd n = XD(n - 1).
    fn pair(&self, n: usize) -> u64 {
        let b = if n & 1 == 0 { &self.fr } else { &self.xf };
        let k = n & 14;
        b[k] as u64 | (b[k + 1] as u64) << 32
    }
    fn set_pair(&mut self, n: usize, v: u64) {
        let b = if n & 1 == 0 { &mut self.fr } else { &mut self.xf };
        let k = n & 14;
        b[k] = v as u32;
        b[k + 1] = (v >> 32) as u32;
    }

    fn pr(&self) -> bool { self.fpscr & FPSCR_PR != 0 }
    fn mode(&self) -> Mode { Mode::from_fpscr(self.fpscr) }
    fn sz(&self) -> bool { self.fpscr & FPSCR_SZ != 0 }

    pub(super) fn fpu<B: Sh4Bus + ?Sized>(&mut self, bus: &mut B, op: u16) {
        if self.check_fd && self.sr_hi & SR_FD != 0 {
            let code = if self.in_slot { EXP_FPU_DISABLED_SLOT } else { EXP_FPU_DISABLED };
            return self.exception(bus, code);
        }
        let n = (op >> 8 & 15) as usize;
        let m = (op >> 4 & 15) as usize;
        let pr = self.pr();
        match op & 15 {
            0..=3 => {
                let o = [Op::Add, Op::Sub, Op::Mul, Op::Div][(op & 3) as usize];
                let md = self.mode();
                if pr {
                    let r = md.arith(o, self.d(n), self.d(m));
                    self.set_d(n, r);
                } else {
                    let r = md.arith(o, self.f(n), self.f(m));
                    self.set_f(n, r);
                }
            }
            4 | 5 => {
                let md = self.mode();
                let (lt, eq) = if pr {
                    let (a, b) = (md.input(self.d(n)), md.input(self.d(m)));
                    (a > b, a == b)
                } else {
                    let (a, b) = (md.input(self.f(n)), md.input(self.f(m)));
                    (a > b, a == b)
                };
                self.t = if op & 1 == 0 { eq } else { lt };
            }
            6 => {
                let a = self.r[m].wrapping_add(self.r[0]);
                if self.sz() { let v = bus.read64(a); self.set_pair(n, v); } else { self.fr[n] = bus.read32(a); }
            }
            7 => {
                let a = self.r[n].wrapping_add(self.r[0]);
                if self.sz() { bus.write64(a, self.pair(m)); } else { bus.write32(a, self.fr[m]); }
            }
            8 => {
                let a = self.r[m];
                if self.sz() { let v = bus.read64(a); self.set_pair(n, v); } else { self.fr[n] = bus.read32(a); }
            }
            9 => {
                let a = self.r[m];
                if self.sz() {
                    let v = bus.read64(a);
                    self.r[m] = a.wrapping_add(8);
                    self.set_pair(n, v);
                } else {
                    let v = bus.read32(a);
                    self.r[m] = a.wrapping_add(4);
                    self.fr[n] = v;
                }
            }
            0xA => {
                let a = self.r[n];
                if self.sz() { bus.write64(a, self.pair(m)); } else { bus.write32(a, self.fr[m]); }
            }
            0xB => {
                if self.sz() {
                    let v = self.pair(m);
                    self.r[n] = self.r[n].wrapping_sub(8);
                    bus.write64(self.r[n], v);
                } else {
                    let v = self.fr[m];
                    self.r[n] = self.r[n].wrapping_sub(4);
                    bus.write32(self.r[n], v);
                }
            }
            0xC => {
                if self.sz() { let v = self.pair(m); self.set_pair(n, v); } else { self.fr[n] = self.fr[m]; }
            }
            0xD => self.fpu_d(bus, op, n, m),
            0xE => {
                // FMAC FR0,FRm,FRn (single only)
                if pr { return; }
                // the product is exact in double precision, then the add and the narrowing
                // round in the current mode
                let md = self.mode();
                let (a, b, c) = (md.input(self.f(0)), md.input(self.f(m)), md.input(self.f(n)));
                let s = md.arith(Op::Add, a as f64 * b as f64, c as f64);
                let r = md.narrow(s);
                self.set_f(n, r);
            }
            _ => self.illegal(bus),
        }
    }

    // (FPU encodings that don't exist for the current FPSCR.PR do nothing, as on the reference)
    fn fpu_d<B: Sh4Bus + ?Sized>(&mut self, bus: &mut B, op: u16, n: usize, m: usize) {
        let pr = self.pr();
        match m {
            0x0 => self.fr[n] = self.fpul,                                   // FSTS FPUL,FRn
            0x1 => self.fpul = self.fr[n],                                   // FLDS FRm,FPUL
            0x2 => {
                // FLOAT FPUL,FRn / DRn
                let v = self.fpul as i32;
                if pr { self.set_d(n, v as f64) } else { let r = self.mode().from_int_f32(v); self.set_f(n, r) }
            }
            0x3 => {
                // FTRC FRm / DRm,FPUL: truncate, saturating; single precision saturates at
                // 7FFFFF80 (the largest float below 2^31), NaN reads as 80000000
                let v = if pr { self.d(n) } else if self.f(n).is_nan() { f64::NAN } else { self.f(n).min(2147483520.0) as f64 };
                self.fpul = if v.is_nan() { 0x8000_0000 } else { v.clamp(i32::MIN as f64, i32::MAX as f64) as i32 as u32 };
            }
            0x4 => {
                // FNEG
                if pr { self.fr[n & 14] ^= 0x8000_0000 } else { self.fr[n] ^= 0x8000_0000 }
            }
            0x5 => {
                // FABS
                if pr { self.fr[n & 14] &= 0x7FFF_FFFF } else { self.fr[n] &= 0x7FFF_FFFF }
            }
            0x6 => {
                // FSQRT
                let md = self.mode();
                if pr { let v = md.sqrt(self.d(n)); self.set_d(n, v) } else { let v = md.sqrt(self.f(n)); self.set_f(n, v) }
            }
            0x7 => {
                // FSRRA FRn: 1 / sqrt
                if pr { return; }
                let md = self.mode();
                let x = md.input(self.f(n)) as f64;
                let v = md.narrow(1.0 / x.sqrt());
                self.set_f(n, v);
            }
            0x8 => { if pr { return; } self.fr[n] = 0; }           // FLDI0
            0x9 => { if pr { return; } self.fr[n] = 0x3F80_0000; } // FLDI1
            0xA => {
                // FCNVSD FPUL,DRn
                if !pr { return; }
                let v = f32::from_bits(self.fpul) as f64;
                self.set_d(n, v);
            }
            0xB => {
                // FCNVDS DRm,FPUL
                if !pr { return; }
                self.fpul = self.mode().narrow(self.d(n)).to_bits();
            }
            0xE => {
                // FIPR FVm,FVn: FR(n + 3) = FVm . FVn
                if pr { return; }
                let (vn, vm) = (n & 12, (n & 3) << 2);
                // summed in double precision, rounded once (the reference's way)
                let md = self.mode();
                let mut s = 0.0f64;
                for k in 0..4 { s += md.input(self.f(vm + k)) as f64 * md.input(self.f(vn + k)) as f64; }
                let r = md.narrow(s);
                self.set_f(vn + 3, r);
            }
            0xF => match op {
                0xF3FD => self.fpscr ^= FPSCR_SZ,                                     // FSCHG
                0xFBFD => { let v = self.fpscr ^ FPSCR_FR; self.set_fpscr(v); }   // FRCHG
                _ if n & 3 == 1 => {
                    // FTRV XMTRX,FVn
                    if pr { return; }
                    let vn = n & 12;
                    let md = self.mode();
                    let v = [self.f(vn), self.f(vn + 1), self.f(vn + 2), self.f(vn + 3)].map(|x| md.input(x) as f64);
                    for i in 0..4 {
                        let mut s = 0.0f64;
                        for (j, &vj) in v.iter().enumerate() { s += md.input(f32::from_bits(self.xf[i + 4 * j])) as f64 * vj; }
                        let r = md.narrow(s);
                        self.set_f(vn + i, r);
                    }
                }
                _ if n & 1 == 0 => {
                    // FSCA FPUL,DRn: sine and cosine of FPUL / 65536 turns
                    if pr { return; }
                    let a = (self.fpul & 0xFFFF) as f64 * (std::f64::consts::TAU / 65536.0);
                    self.set_f(n & 14, a.sin() as f32);
                    self.set_f(n | 1, a.cos() as f32);
                }
                _ => self.illegal(bus),
            },
            _ => self.illegal(bus),
        }
    }
}
