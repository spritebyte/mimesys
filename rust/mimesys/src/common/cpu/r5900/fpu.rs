//! The EE's COP1: single precision, but not IEEE 754.
//!
//! - There are no infinities, NaNs or denormals: exponent 255 is an ordinary (huge)
//!   number, exponent 0 is zero (a denormal input counts as a signed zero).
//! - Results round toward zero. A result too big becomes ±Fmax (7FFF_FFFF, with the
//!   O flag); too small becomes ±0 (U flag).
//! - Every value is exactly representable as an f64 (24-bit significand, exponent up to
//!   2^128), so the sums, products and quotients are worked out in f64 and then
//!   truncated. Products are exact; a sum or quotient that f64 had to round can, very
//!   rarely, land one unit off the console's.
//!
//! The same rules serve the VUs (`ps2_add` and friends return O / U as flag bits).

use super::{R5900, Res, EXC_RI};

pub const FCR31_C: u32 = 1 << 23;
pub const F_I: u32 = 1 << 17;
pub const F_D: u32 = 1 << 16;
pub const F_O: u32 = 1 << 15;
pub const F_U: u32 = 1 << 14;
const F_SI: u32 = 1 << 6;
const F_SD: u32 = 1 << 5;
const F_SO: u32 = 1 << 4;
const F_SU: u32 = 1 << 3;

const FMAX: u32 = 0x7FFF_FFFF;

/// A PS2 float's value (denormals = signed zero; exponent 255 is a number)
#[inline]
pub fn to_f64(x: u32) -> f64 {
    let e = x >> 23 & 0xFF;
    let sign = (x as u64 >> 31) << 63;
    if e == 0 { return f64::from_bits(sign); }
    let bits = sign | ((e as u64 + 1023 - 127) << 52) | ((x as u64 & 0x7F_FFFF) << 29);
    f64::from_bits(bits)
}

/// Back to a PS2 float, rounding toward zero; the O / U flags if it didn't fit
#[inline]
pub fn from_f64(v: f64) -> (u32, u32) {
    let bits = v.to_bits();
    let sign = ((bits >> 63) as u32) << 31;
    if v == 0.0 { return (sign, 0); }
    let e = (bits >> 52 & 0x7FF) as i32 - 1023 + 127;
    if e > 255 { return (sign | FMAX, F_O); }
    if e <= 0 { return (sign, F_U); }
    (sign | (e as u32) << 23 | (bits >> 29 & 0x7F_FFFF) as u32, 0)
}

#[inline]
pub fn ps2_add(a: u32, b: u32) -> (u32, u32) { from_f64(to_f64(a) + to_f64(b)) }
#[inline]
pub fn ps2_sub(a: u32, b: u32) -> (u32, u32) { from_f64(to_f64(a) - to_f64(b)) }
#[inline]
pub fn ps2_mul(a: u32, b: u32) -> (u32, u32) { from_f64(to_f64(a) * to_f64(b)) }

/// a / b; dividing by zero gives ±Fmax with D (or I for 0 / 0)
pub fn ps2_div(a: u32, b: u32) -> (u32, u32) {
    let (x, y) = (to_f64(a), to_f64(b));
    if y == 0.0 {
        let flag = if x == 0.0 { F_I } else { F_D };
        return (((a ^ b) & 0x8000_0000) | FMAX, flag);
    }
    from_f64(x / y)
}

impl R5900 {
    fn fset(&mut self, fd: u32, (v, f): (u32, u32)) {
        self.fpr[fd as usize] = v;
        self.fcr31 = (self.fcr31 & !(F_O | F_U)) | f;
        if f & F_O != 0 { self.fcr31 |= F_SO; }
        if f & F_U != 0 { self.fcr31 |= F_SU; }
    }

    fn fset_div(&mut self, fd: u32, (v, f): (u32, u32)) {
        self.fpr[fd as usize] = v;
        self.fcr31 = (self.fcr31 & !(F_I | F_D)) | f;
        if f & F_I != 0 { self.fcr31 |= F_SI; }
        if f & F_D != 0 { self.fcr31 |= F_SD; }
    }

    fn fset_acc(&mut self, (v, f): (u32, u32)) {
        self.acc = v;
        self.fcr31 = (self.fcr31 & !(F_O | F_U)) | f;
        if f & F_O != 0 { self.fcr31 |= F_SO; }
        if f & F_U != 0 { self.fcr31 |= F_SU; }
    }

    pub(super) fn cop1(&mut self, i: u32, rs: u32, rt: u32) -> Res<u32> {
        let fs = i >> 11 & 31;
        let fd = i >> 6 & 31;
        let (a, b) = (self.fpr[fs as usize], self.fpr[rt as usize]);
        match rs {
            0x00 => self.set32(rt, a),                                   // MFC1
            0x02 => self.set32(rt, if fs == 0 { 0x2E30 } else if fs == 31 { self.fcr31 } else { 0 }),
            0x04 => self.fpr[fs as usize] = self.r32(rt),                // MTC1
            0x06 => if fs == 31 { self.fcr31 = (self.r32(rt) & 0x0083_C078) | 0x0100_0000; },
            0x08 => {
                let c = self.fcr31 & FCR31_C != 0;
                match rt {
                    0 => self.branch(!c, i),
                    1 => self.branch(c, i),
                    2 => self.branch_likely(!c, i),
                    3 => self.branch_likely(c, i),
                    _ => return Err(self.fault(EXC_RI)),
                }
            }
            0x10 => match i & 0x3F {
                0x00 => self.fset(fd, ps2_add(a, b)),
                0x01 => self.fset(fd, ps2_sub(a, b)),
                0x02 => self.fset(fd, ps2_mul(a, b)),
                0x03 => { self.fset_div(fd, ps2_div(a, b)); return Ok(7); }
                0x04 => {
                    // SQRT ft: the root of |ft|, I for a negative one
                    let x = to_f64(b);
                    let flag = if x < 0.0 { F_I } else { 0 };
                    let (v, _) = from_f64(x.abs().sqrt());
                    self.fset_div(fd, (v, flag));
                    return Ok(7);
                }
                0x05 => { self.fpr[fd as usize] = a & 0x7FFF_FFFF; self.fcr31 &= !(F_O | F_U); }
                0x06 => self.fpr[fd as usize] = a,
                0x07 => { self.fpr[fd as usize] = a ^ 0x8000_0000; self.fcr31 &= !(F_O | F_U); }
                0x16 => {
                    // RSQRT fs / sqrt(ft)
                    let y = to_f64(b);
                    let r = if y == 0.0 {
                        ((a & 0x8000_0000) | FMAX, F_D)
                    } else {
                        let (v, _) = from_f64(to_f64(a) / y.abs().sqrt());
                        (v, if y < 0.0 { F_I } else { 0 })
                    };
                    self.fset_div(fd, r);
                    return Ok(13);
                }
                0x18 => self.fset_acc(ps2_add(a, b)),
                0x19 => self.fset_acc(ps2_sub(a, b)),
                0x1A => self.fset_acc(ps2_mul(a, b)),
                0x1C | 0x1D | 0x1E | 0x1F => {
                    // MADD / MSUB (to fd), MADDA / MSUBA (to ACC): ACC ± fs * ft
                    let (p, pf) = ps2_mul(a, b);
                    let (v, f) = if i & 1 == 0 { ps2_add(self.acc, p) } else { ps2_sub(self.acc, p) };
                    if i & 2 == 0 { self.fset(fd, (v, f | pf)); } else { self.fset_acc((v, f | pf)); }
                }
                0x24 => {
                    // CVT.W.S, truncating; out of range saturates
                    let x = to_f64(a);
                    self.fpr[fd as usize] = if x >= 2147483648.0 { 0x7FFF_FFFF }
                        else if x < -2147483648.0 { 0x8000_0000 } else { x as i32 as u32 };
                }
                0x28 => { self.fpr[fd as usize] = if to_f64(a) >= to_f64(b) { a } else { b }; self.fcr31 &= !(F_O | F_U); }
                0x29 => { self.fpr[fd as usize] = if to_f64(a) <= to_f64(b) { a } else { b }; self.fcr31 &= !(F_O | F_U); }
                0x30 | 0x32 | 0x34 | 0x36 => {
                    let (x, y) = (to_f64(a), to_f64(b));
                    let c = match i & 0x3F { 0x30 => false, 0x32 => x == y, 0x34 => x < y, _ => x <= y };
                    self.fcr31 = (self.fcr31 & !FCR31_C) | if c { FCR31_C } else { 0 };
                }
                _ => return Err(self.fault(EXC_RI)),
            },
            0x14 if i & 0x3F == 0x20 => {
                // CVT.S.W
                self.fpr[fd as usize] = from_f64(a as i32 as f64).0;
            }
            _ => return Err(self.fault(EXC_RI)),
        }
        Ok(1)
    }
}
