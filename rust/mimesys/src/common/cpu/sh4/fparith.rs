//! FPU arithmetic with the SH-4's FPSCR modes, for f32 and f64:
//! - RM = 1 (round to zero, what Dreamcast games use): the host rounds to nearest, then
//!   the exact error (TwoSum for add, FMA for multiply / divide / square root) tells
//!   whether the result came out larger than the true value; if so it steps one unit
//!   toward zero. Overflow gives the largest finite number.
//! - DN = 1: denormal inputs and results are zero (sign kept).
//! - A NaN result is always the default quiet NaN 7FC00000 / 7FF8000000000000 (what the
//!   reference produced; the real chip's default NaN is 7FBFFFFF and it keeps payloads).
//! - Division by zero gives infinity in every mode.

pub trait Fp: Copy + PartialOrd + std::ops::Add<Output = Self> + std::ops::Sub<Output = Self>
    + std::ops::Mul<Output = Self> + std::ops::Div<Output = Self> + std::ops::Neg<Output = Self> {
    const ZERO: Self;
    const MAX: Self;
    const QNAN: Self;
    /// a power of two that brings the smaller factor of a subnormal product back to normal
    const SCALE: Self;
    fn abs(self) -> Self;
    fn fma(self, b: Self, c: Self) -> Self;
    fn sqrt(self) -> Self;
    fn is_nan(self) -> bool;
    fn is_finite(self) -> bool;
    fn is_subnormal(self) -> bool;
    fn is_sign_negative(self) -> bool;
    /// one unit toward zero (finite, non-zero)
    fn toward_zero(self) -> Self;
    fn signed_zero(self) -> Self;
}

macro_rules! fp {
    ($t:ty, $nan:expr, $scale:expr) => {
        impl Fp for $t {
            const ZERO: Self = 0.0;
            const MAX: Self = <$t>::MAX;
            const QNAN: Self = <$t>::from_bits($nan);
            const SCALE: Self = $scale;
            fn abs(self) -> Self { <$t>::abs(self) }
            fn fma(self, b: Self, c: Self) -> Self { self.mul_add(b, c) }
            fn sqrt(self) -> Self { <$t>::sqrt(self) }
            fn is_nan(self) -> bool { <$t>::is_nan(self) }
            fn is_finite(self) -> bool { <$t>::is_finite(self) }
            fn is_subnormal(self) -> bool { <$t>::is_subnormal(self) }
            fn is_sign_negative(self) -> bool { <$t>::is_sign_negative(self) }
            fn toward_zero(self) -> Self { <$t>::from_bits(self.to_bits() - 1) }
            fn signed_zero(self) -> Self { if self.is_sign_negative() { -0.0 } else { 0.0 } }
        }
    };
}
fp!(f32, 0x7FC0_0000, 18446744073709551616.0);   // 2^64
fp!(f64, 0x7FF8_0000_0000_0000, 4.149515568880993e180); // 2^600

#[derive(Clone, Copy)]
pub struct Mode {
    pub rz: bool,
    pub dn: bool,
    /// plain host arithmetic (`Sh4::fast_fpu`)
    pub fast: bool,
}

#[derive(Clone, Copy)]
pub enum Op { Add, Sub, Mul, Div }

impl Mode {
    /// RM 1 = round to zero; 0 and the reserved 2 / 3 round to nearest.
    pub fn from_fpscr(fpscr: u32) -> Self { Self { rz: fpscr & 3 == 1, dn: fpscr & super::FPSCR_DN != 0, fast: false } }

    #[inline]
    pub fn input<T: Fp>(self, x: T) -> T { if !self.fast && self.dn && x.is_subnormal() { x.signed_zero() } else { x } }

    /// Finish a result: `d` has the sign of (exact - r) (zero when exact).
    fn finish<T: Fp>(self, r: T, d: T, inputs_nan: bool, inputs_finite: bool) -> T {
        let _ = inputs_nan;
        let r = if r.is_nan() {
            T::QNAN
        } else if !r.is_finite() {
            if self.rz && inputs_finite { if r.is_sign_negative() { -T::MAX } else { T::MAX } } else { r }
        } else if self.rz && r != T::ZERO && d != T::ZERO && d.is_sign_negative() != r.is_sign_negative() {
            r.toward_zero()
        } else {
            r
        };
        self.output(r)
    }

    pub fn output<T: Fp>(self, r: T) -> T { if self.dn && r.is_subnormal() { r.signed_zero() } else { r } }

    #[inline]
    pub fn arith<T: Fp>(self, op: Op, a: T, b: T) -> T {
        if self.fast {
            return match op { Op::Add => a + b, Op::Sub => a - b, Op::Mul => a * b, Op::Div => a / b };
        }
        let (a, b) = (self.input(a), self.input(b));
        let nan = a.is_nan() || b.is_nan();
        let fin = a.is_finite() && b.is_finite();
        let (r, d) = match op {
            Op::Add | Op::Sub => {
                let b = if let Op::Sub = op { -b } else { b };
                let r = a + b;
                // TwoSum: a + b = r + e exactly
                let bb = r - a;
                let e = (a - (r - bb)) + (b - bb);
                (r, e)
            }
            Op::Mul => {
                let r = a * b;
                // a subnormal product: the FMA's error would underflow, so measure it scaled
                // (scaling the smaller factor and r up by a power of two is exact)
                let d = if r.is_subnormal() {
                    let (s, l) = if a.abs() < b.abs() { (a, b) } else { (b, a) };
                    (s * T::SCALE).fma(l, -(r * T::SCALE))
                } else {
                    a.fma(b, -r)
                };
                (r, d)
            }
            Op::Div if b == T::ZERO => return self.output(a / b),
            Op::Div => {
                let r = a / b;
                // a - r*b; the error of r has that sign times b's
                let rem = (-r).fma(b, a);
                (r, if b.is_sign_negative() { -rem } else { rem })
            }
        };
        let d = if r.is_finite() && fin { d } else { T::ZERO };
        self.finish(r, d, nan, fin)
    }

    pub fn sqrt<T: Fp>(self, x: T) -> T {
        if self.fast { return x.sqrt(); }
        let x = self.input(x);
        let r = x.sqrt();
        let d = if r.is_finite() { (-r).fma(r, x) } else { T::ZERO };
        self.finish(r, d, x.is_nan(), x.is_finite())
    }

    /// An f64 value rounded to f32 with this mode.
    #[inline]
    pub fn narrow(self, x: f64) -> f32 {
        let r = x as f32;
        if self.fast { return r; }
        let d = if r.is_finite() && x.is_finite() { x - r as f64 } else { 0.0 };
        if r.is_finite() || !x.is_finite() {
            let d = if d == 0.0 { 0.0f32 } else if d < 0.0 { -1.0 } else { 1.0 };
            self.finish(r, d, x.is_nan(), x.is_finite())
        } else {
            self.finish(r, 0.0, false, true)
        }
    }

    /// An integer to f32 / f64 (FLOAT).
    pub fn from_int_f32(self, v: i32) -> f32 {
        let r = v as f32;
        if self.fast { return r; }
        let d = (v as i64 - r as i64) as f32;
        self.finish(r, d, false, true)
    }
}
