//! The VU's FMAC on all four fields at once (SSE2 on x86-64, else one field at a time).
//! Same arithmetic as `fpu::ps2_add` / `ps2_sub` / `ps2_mul` (exact via f64, round toward
//! zero, no denormals / NaN / Inf): results are bit-identical to the scalar path.

use crate::common::cpu::r5900::fpu::{ps2_add, ps2_mul, ps2_sub, F_O, F_U};

pub const ADD: u8 = 0;
pub const SUB: u8 = 1;
pub const MUL: u8 = 2;
pub const MADD: u8 = 3;
pub const MSUB: u8 = 4;

/// The MAC flags of four results (all fields; x is bit 3 of each group: Z, S, U, O)
#[inline(always)]
pub fn mac4(v: [u32; 4], fl: [u32; 4]) -> u32 {
    const REV: [u32; 16] = [0, 8, 4, 12, 2, 10, 6, 14, 1, 9, 5, 13, 3, 11, 7, 15];
    let (mut z, mut s, mut u, mut o) = (0usize, 0usize, 0usize, 0usize);
    for i in 0..4 {
        z |= ((v[i] & 0x7FFF_FFFF == 0) as usize) << i;
        s |= ((v[i] >> 31) as usize) << i;
        u |= ((fl[i] & 1) as usize) << i;
        o |= ((fl[i] >> 1 & 1) as usize) << i;
    }
    REV[z] | REV[s] << 4 | REV[u] << 8 | REV[o] << 12
}

/// Per field: the result and its U (bit 0) / O (bit 1) flags
#[inline(always)]
pub fn fmac4<const K: u8>(a: [u32; 4], b: [u32; 4], acc: [u32; 4]) -> ([u32; 4], [u32; 4]) {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: SSE2 is part of the x86-64 baseline
        unsafe { sse::fmac4::<K>(a, b, acc) }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        scalar::<K>(a, b, acc)
    }
}

#[allow(dead_code)]
#[inline(always)]
pub fn scalar<const K: u8>(a: [u32; 4], b: [u32; 4], acc: [u32; 4]) -> ([u32; 4], [u32; 4]) {
    let mut out = [0; 4];
    let mut fl = [0; 4];
    for i in 0..4 {
        let (v, f) = match K {
            ADD => ps2_add(a[i], b[i]),
            SUB => ps2_sub(a[i], b[i]),
            MUL => ps2_mul(a[i], b[i]),
            MADD => { let (m, f1) = ps2_mul(a[i], b[i]); let (v, f2) = ps2_add(acc[i], m); (v, f1 | f2) }
            _ => { let (m, f1) = ps2_mul(a[i], b[i]); let (v, f2) = ps2_sub(acc[i], m); (v, f1 | f2) }
        };
        out[i] = v;
        fl[i] = (f & F_U != 0) as u32 | ((f & F_O != 0) as u32) << 1;
    }
    (out, fl)
}

#[cfg(target_arch = "x86_64")]
mod sse {
    use std::arch::x86_64::*;

    /// 4 PS2 floats → two f64 pairs (fields 0,1 and 2,3)
    #[inline(always)]
    unsafe fn widen(x: __m128i) -> (__m128d, __m128d) {
        unsafe {
            // exponent 0 → ±0
            let ez = _mm_cmpeq_epi32(_mm_and_si128(x, _mm_set1_epi32(0x7F80_0000)), _mm_setzero_si128());
            let mag = _mm_and_si128(x, _mm_set1_epi32(0x7FFF_FFFF));
            let mag = _mm_andnot_si128(ez, mag);
            let sign = _mm_and_si128(x, _mm_set1_epi32(i32::MIN));
            let nz = _mm_andnot_si128(ez, _mm_set1_epi32(-1));
            let half = |m: __m128i, s: __m128i, n: __m128i| {
                // m, s, n hold two 32-bit values each in the low halves of 64-bit lanes
                let m = _mm_slli_epi64(m, 29);
                let bias = _mm_and_si128(n, _mm_set1_epi64x(896 << 52));
                let s = _mm_slli_epi64(s, 32);
                _mm_castsi128_pd(_mm_or_si128(_mm_add_epi64(m, bias), s))
            };
            let z = _mm_setzero_si128();
            let lo = half(_mm_unpacklo_epi32(mag, z), _mm_unpacklo_epi32(sign, z), _mm_unpacklo_epi32(nz, nz));
            let hi = half(_mm_unpackhi_epi32(mag, z), _mm_unpackhi_epi32(sign, z), _mm_unpackhi_epi32(nz, nz));
            (lo, hi)
        }
    }

    /// Two f64 pairs → 4 PS2 floats (round toward zero) and per-field flags U=1 / O=2
    #[inline(always)]
    unsafe fn narrow(lo: __m128d, hi: __m128d) -> (__m128i, __m128i) {
        unsafe {
            let (l, h) = (_mm_castpd_si128(lo), _mm_castpd_si128(hi));
            // the high and low 32 bits of each field's f64, as 4 × 32
            let hw = _mm_castps_si128(_mm_shuffle_ps::<0b11_01_11_01>(_mm_castsi128_ps(l), _mm_castsi128_ps(h)));
            // (bits >> 29) & 0x7F_FFFF: low 23 bits of mantissa top
            let m_lo = _mm_srli_epi64(l, 29);
            let m_hi = _mm_srli_epi64(h, 29);
            let mw = _mm_castps_si128(_mm_shuffle_ps::<0b10_00_10_00>(_mm_castsi128_ps(m_lo), _mm_castsi128_ps(m_hi)));
            let mant = _mm_and_si128(mw, _mm_set1_epi32(0x7F_FFFF));
            let sign = _mm_and_si128(hw, _mm_set1_epi32(i32::MIN));
            let ex = _mm_and_si128(_mm_srli_epi32(hw, 20), _mm_set1_epi32(0x7FF));
            let e = _mm_sub_epi32(ex, _mm_set1_epi32(896));
            let zero = _mm_cmpeq_epi32(ex, _mm_setzero_si128());                 // exact 0
            let under = _mm_andnot_si128(zero, _mm_cmplt_epi32(e, _mm_set1_epi32(1)));
            let over = _mm_cmpgt_epi32(e, _mm_set1_epi32(255));
            let normal = _mm_or_si128(_mm_slli_epi32(e, 23), mant);
            let small = _mm_or_si128(zero, under);
            let v = _mm_andnot_si128(small, normal);
            let v = _mm_or_si128(_mm_andnot_si128(over, v), _mm_and_si128(over, _mm_set1_epi32(0x7FFF_FFFF)));
            let v = _mm_or_si128(v, sign);
            let fl = _mm_or_si128(_mm_and_si128(under, _mm_set1_epi32(1)), _mm_and_si128(over, _mm_set1_epi32(2)));
            (v, fl)
        }
    }

    #[inline(always)]
    unsafe fn op2(k: u8, a: (__m128d, __m128d), b: (__m128d, __m128d)) -> (__m128d, __m128d) {
        unsafe {
            match k {
                super::ADD => (_mm_add_pd(a.0, b.0), _mm_add_pd(a.1, b.1)),
                super::SUB => (_mm_sub_pd(a.0, b.0), _mm_sub_pd(a.1, b.1)),
                _ => (_mm_mul_pd(a.0, b.0), _mm_mul_pd(a.1, b.1)),
            }
        }
    }

    #[inline(always)]
    pub unsafe fn fmac4<const K: u8>(a: [u32; 4], b: [u32; 4], acc: [u32; 4]) -> ([u32; 4], [u32; 4]) {
        unsafe {
            let ld = |x: [u32; 4]| _mm_loadu_si128(x.as_ptr() as *const __m128i);
            let (va, vb) = (widen(ld(a)), widen(ld(b)));
            let (v, fl) = match K {
                super::ADD | super::SUB | super::MUL => {
                    let r = op2(K, va, vb);
                    narrow(r.0, r.1)
                }
                _ => {
                    let m = op2(super::MUL, va, vb);
                    let (mv, f1) = narrow(m.0, m.1);
                    let r = op2(if K == super::MADD { super::ADD } else { super::SUB }, widen(ld(acc)), widen(mv));
                    let (v, f2) = narrow(r.0, r.1);
                    (v, _mm_or_si128(f1, f2))
                }
            };
            let mut out = [0u32; 4];
            let mut f = [0u32; 4];
            _mm_storeu_si128(out.as_mut_ptr() as *mut __m128i, v);
            _mm_storeu_si128(f.as_mut_ptr() as *mut __m128i, fl);
            (out, f)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check<const K: u8>(a: [u32; 4], b: [u32; 4], c: [u32; 4]) {
        assert_eq!(fmac4::<K>(a, b, c), scalar::<K>(a, b, c), "op {K} {a:08X?} {b:08X?} {c:08X?}");
    }

    #[test]
    fn simd_fmac_matches_scalar() {
        let mut x = 0x1234_5678u32;
        let mut rnd = || { x ^= x << 13; x ^= x >> 17; x ^= x << 5; x };
        let special = [0, 0x8000_0000, 0x7FFF_FFFF, 0xFFFF_FFFF, 0x0000_0001, 0x0080_0000, 0x3F80_0000, 0xBF80_0000,
                       0x7F80_0000, 0x0040_0000, 0x7F7F_FFFF, 0x0100_0000, 0x3F7F_FFFF];
        for i in 0..200_000 {
            let mut pick = |_: usize| {
                let r = rnd();
                match r % 4 { 0 => special[(r >> 8) as usize % special.len()], 1 => r & 0x8FFF_FFFF | 0x3000_0000, _ => r }
            };
            let a: [u32; 4] = std::array::from_fn(&mut pick);
            let b: [u32; 4] = std::array::from_fn(&mut pick);
            let c: [u32; 4] = std::array::from_fn(&mut pick);
            match i % 5 { 0 => check::<ADD>(a, b, c), 1 => check::<SUB>(a, b, c), 2 => check::<MUL>(a, b, c),
                          3 => check::<MADD>(a, b, c), _ => check::<MSUB>(a, b, c) }
        }
    }
}
