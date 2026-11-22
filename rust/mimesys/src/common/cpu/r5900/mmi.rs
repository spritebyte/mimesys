//! The R5900's MMI instructions (opcode 1C): 128-bit SIMD integer ops on bytes,
//! halfwords and words, the second multiply / divide pipeline (MULT1, DIV1, ... on the
//! high halves of LO / HI), and the parallel multiply-accumulates.
//!
//! Lane conventions: lane 0 is the lowest. PHMADH / PHMSBH write the odd LO / HI lanes
//! with the odd products (PHMSBH with their complement), as PCSX2 documents; the
//! ps2autotests mmi tests are the check.

use super::{sext32, R5900, Res, EXC_RI};

#[inline] fn w(v: u128) -> [u32; 4] { let b = v.to_le_bytes(); core::array::from_fn(|i| u32::from_le_bytes(b[4 * i..4 * i + 4].try_into().unwrap())) }
#[inline] fn h(v: u128) -> [u16; 8] { let b = v.to_le_bytes(); core::array::from_fn(|i| u16::from_le_bytes([b[2 * i], b[2 * i + 1]])) }
#[inline] fn b(v: u128) -> [u8; 16] { v.to_le_bytes() }
#[inline] fn d(v: u128) -> [u64; 2] { [v as u64, (v >> 64) as u64] }
#[inline] fn fw(a: [u32; 4]) -> u128 { a.iter().rev().fold(0u128, |acc, &x| acc << 32 | x as u128) }
#[inline] fn fh(a: [u16; 8]) -> u128 { a.iter().rev().fold(0u128, |acc, &x| acc << 16 | x as u128) }
#[inline] fn fb(a: [u8; 16]) -> u128 { u128::from_le_bytes(a) }
#[inline] fn fd(a: [u64; 2]) -> u128 { a[0] as u128 | (a[1] as u128) << 64 }

fn mapw(x: u128, y: u128, f: impl Fn(u32, u32) -> u32) -> u128 { let (x, y) = (w(x), w(y)); fw(core::array::from_fn(|i| f(x[i], y[i]))) }
fn maph(x: u128, y: u128, f: impl Fn(u16, u16) -> u16) -> u128 { let (x, y) = (h(x), h(y)); fh(core::array::from_fn(|i| f(x[i], y[i]))) }
fn mapb(x: u128, y: u128, f: impl Fn(u8, u8) -> u8) -> u128 { let (x, y) = (b(x), b(y)); fb(core::array::from_fn(|i| f(x[i], y[i]))) }

fn mask32(c: bool) -> u32 { if c { u32::MAX } else { 0 } }
fn mask16(c: bool) -> u16 { if c { u16::MAX } else { 0 } }
fn mask8(c: bool) -> u8 { if c { u8::MAX } else { 0 } }

fn sat16(v: i32) -> u16 { v.clamp(i16::MIN as i32, i16::MAX as i32) as i16 as u16 }

/// Signed divide by the R5900's rules (zero divisor: quotient ±1 the other way, remainder
/// the dividend; MIN / -1 = MIN remainder 0)
fn sdiv(n: i32, dv: i32) -> (u32, u32) {
    if dv == 0 { (if n < 0 { 1 } else { -1i32 } as u32, n as u32) }
    else if n == i32::MIN && dv == -1 { (n as u32, 0) }
    else { ((n / dv) as u32, (n % dv) as u32) }
}

impl R5900 {
    pub(super) fn mmi(&mut self, i: u32, rs: u32, rt: u32, rd: u32, sa: u32) -> Res<u32> {
        let (x, y) = (self.gpr[rs as usize], self.gpr[rt as usize]);
        match i & 0x3F {
            0x00 => { self.madd(0, rd, rs, rt, true); return Ok(2); }
            0x01 => { self.madd(0, rd, rs, rt, false); return Ok(2); }
            0x04 => {
                // PLZCW: leading bits equal to the sign, minus one, for the low two words
                let lz = |v: u32| (if v as i32 >= 0 { v.leading_zeros() } else { (!v).leading_zeros() }) - 1;
                let v = w(x);
                self.set64(rd, lz(v[0]) as u64 | (lz(v[1]) as u64) << 32);
            }
            0x08 => return self.mmi0(i, x, y, rd, sa),
            0x09 => return self.mmi2(i, rs, rt, x, y, rd, sa),
            0x10 => self.set64(rd, (self.hi >> 64) as u64),
            0x11 => self.set_hi64(1, self.r64(rs)),
            0x12 => self.set64(rd, (self.lo >> 64) as u64),
            0x13 => self.set_lo64(1, self.r64(rs)),
            0x18 => { self.mult(1, rd, rs, rt, true); return Ok(2); }
            0x19 => { self.mult(1, rd, rs, rt, false); return Ok(2); }
            0x1A => { self.div(1, rs, rt, true); return Ok(4); }
            0x1B => { self.div(1, rs, rt, false); return Ok(4); }
            0x20 => { self.madd(1, rd, rs, rt, true); return Ok(2); }
            0x21 => { self.madd(1, rd, rs, rt, false); return Ok(2); }
            0x28 => return self.mmi1(i, x, y, rd, sa),
            0x29 => return self.mmi3(i, rs, rt, x, y, rd, sa),
            0x30 => {
                // PMFHL
                let (l, hh) = (w(self.lo), w(self.hi));
                let v = match sa {
                    0 => fw([l[0], hh[0], l[2], hh[2]]),
                    1 => fw([l[1], hh[1], l[3], hh[3]]),
                    2 => {
                        let sat = |hi: u32, lo: u32| {
                            let v = ((hi as u64) << 32 | lo as u64) as i64;
                            v.clamp(i32::MIN as i64, i32::MAX as i64) as u64
                        };
                        fd([sat(hh[0], l[0]), sat(hh[2], l[2])])
                    }
                    3 => {
                        let (l, hh) = (h(self.lo), h(self.hi));
                        fh([l[0], l[2], hh[0], hh[2], l[4], l[6], hh[4], hh[6]])
                    }
                    4 => {
                        let s = |v: u32| sat16(v as i32);
                        fh([s(l[0]), s(l[1]), s(hh[0]), s(hh[1]), s(l[2]), s(l[3]), s(hh[2]), s(hh[3])])
                    }
                    _ => return Err(self.fault(EXC_RI)),
                };
                self.set128(rd, v);
            }
            0x31 => {
                // PMTHL.LW
                if sa != 0 { return Err(self.fault(EXC_RI)); }
                let v = w(x);
                let (mut l, mut hh) = (w(self.lo), w(self.hi));
                l[0] = v[0]; hh[0] = v[1]; l[2] = v[2]; hh[2] = v[3];
                self.lo = fw(l);
                self.hi = fw(hh);
            }
            0x34 => self.set128(rd, maph(y, 0, |a, _| a << (sa & 15))),
            0x36 => self.set128(rd, maph(y, 0, |a, _| a >> (sa & 15))),
            0x37 => self.set128(rd, maph(y, 0, |a, _| ((a as i16) >> (sa & 15)) as u16)),
            0x3C => self.set128(rd, mapw(y, 0, |a, _| a << sa)),
            0x3E => self.set128(rd, mapw(y, 0, |a, _| a >> sa)),
            0x3F => self.set128(rd, mapw(y, 0, |a, _| ((a as i32) >> sa) as u32)),
            _ => return Err(self.fault(EXC_RI)),
        }
        Ok(1)
    }

    fn mmi0(&mut self, _i: u32, x: u128, y: u128, rd: u32, sa: u32) -> Res<u32> {
        let v = match sa {
            0x00 => mapw(x, y, |a, b| a.wrapping_add(b)),
            0x01 => mapw(x, y, |a, b| a.wrapping_sub(b)),
            0x02 => mapw(x, y, |a, b| mask32(a as i32 > b as i32)),
            0x03 => mapw(x, y, |a, b| (a as i32).max(b as i32) as u32),
            0x04 => maph(x, y, |a, b| a.wrapping_add(b)),
            0x05 => maph(x, y, |a, b| a.wrapping_sub(b)),
            0x06 => maph(x, y, |a, b| mask16(a as i16 > b as i16)),
            0x07 => maph(x, y, |a, b| (a as i16).max(b as i16) as u16),
            0x08 => mapb(x, y, |a, b| a.wrapping_add(b)),
            0x09 => mapb(x, y, |a, b| a.wrapping_sub(b)),
            0x0A => mapb(x, y, |a, b| mask8(a as i8 > b as i8)),
            0x10 => mapw(x, y, |a, b| (a as i32).saturating_add(b as i32) as u32),
            0x11 => mapw(x, y, |a, b| (a as i32).saturating_sub(b as i32) as u32),
            0x12 => { let (a, b) = (w(x), w(y)); fw([b[0], a[0], b[1], a[1]]) }       // PEXTLW
            0x13 => { let (a, b) = (w(x), w(y)); fw([b[0], b[2], a[0], a[2]]) }       // PPACW
            0x14 => maph(x, y, |a, b| (a as i16).saturating_add(b as i16) as u16),
            0x15 => maph(x, y, |a, b| (a as i16).saturating_sub(b as i16) as u16),
            0x16 => { let (a, b) = (h(x), h(y)); fh(core::array::from_fn(|k| if k & 1 == 0 { b[k / 2] } else { a[k / 2] })) }
            0x17 => { let (a, b) = (h(x), h(y)); fh(core::array::from_fn(|k| if k < 4 { b[2 * k] } else { a[2 * (k - 4)] })) }
            0x18 => mapb(x, y, |a, b| (a as i8).saturating_add(b as i8) as u8),
            0x19 => mapb(x, y, |a, b| (a as i8).saturating_sub(b as i8) as u8),
            0x1A => { let (a, b) = (b(x), b(y)); fb(core::array::from_fn(|k| if k & 1 == 0 { b[k / 2] } else { a[k / 2] })) }
            0x1B => { let (a, b) = (b(x), b(y)); fb(core::array::from_fn(|k| if k < 8 { b[2 * k] } else { a[2 * (k - 8)] })) }
            0x1E => mapw(y, 0, |v, _| {
                // PEXT5: 1-5-5-5 -> 8-8-8-8
                (v & 0x1F) << 3 | (v >> 5 & 0x1F) << 11 | (v >> 10 & 0x1F) << 19 | (v >> 15 & 1) << 31
            }),
            0x1F => mapw(y, 0, |v, _| {
                // PPAC5
                (v >> 3 & 0x1F) | (v >> 11 & 0x1F) << 5 | (v >> 19 & 0x1F) << 10 | (v >> 31 & 1) << 15
            }),
            _ => return Err(self.fault(EXC_RI)),
        };
        self.set128(rd, v);
        Ok(1)
    }

    fn mmi1(&mut self, _i: u32, x: u128, y: u128, rd: u32, sa: u32) -> Res<u32> {
        let v = match sa {
            0x01 => mapw(y, 0, |a, _| if a == 0x8000_0000 { 0x7FFF_FFFF } else { (a as i32).unsigned_abs() }),
            0x02 => mapw(x, y, |a, b| mask32(a == b)),
            0x03 => mapw(x, y, |a, b| (a as i32).min(b as i32) as u32),
            0x04 => {
                // PADSBH: the low four halfwords subtract, the high four add
                let (a, b) = (h(x), h(y));
                fh(core::array::from_fn(|k| if k < 4 { a[k].wrapping_sub(b[k]) } else { a[k].wrapping_add(b[k]) }))
            }
            0x05 => maph(y, 0, |a, _| if a == 0x8000 { 0x7FFF } else { (a as i16).unsigned_abs() }),
            0x06 => maph(x, y, |a, b| mask16(a == b)),
            0x07 => maph(x, y, |a, b| (a as i16).min(b as i16) as u16),
            0x0A => mapb(x, y, |a, b| mask8(a == b)),
            0x10 => mapw(x, y, |a, b| a.saturating_add(b)),
            0x11 => mapw(x, y, |a, b| a.saturating_sub(b)),
            0x12 => { let (a, b) = (w(x), w(y)); fw([b[2], a[2], b[3], a[3]]) }       // PEXTUW
            0x14 => maph(x, y, |a, b| a.saturating_add(b)),
            0x15 => maph(x, y, |a, b| a.saturating_sub(b)),
            0x16 => { let (a, b) = (h(x), h(y)); fh(core::array::from_fn(|k| if k & 1 == 0 { b[4 + k / 2] } else { a[4 + k / 2] })) }
            0x18 => mapb(x, y, |a, b| a.saturating_add(b)),
            0x19 => mapb(x, y, |a, b| a.saturating_sub(b)),
            0x1A => { let (a, b) = (b(x), b(y)); fb(core::array::from_fn(|k| if k & 1 == 0 { b[8 + k / 2] } else { a[8 + k / 2] })) }
            0x1B => {
                // QFSRV: (rs:rt) >> SA bytes
                let s = self.sa * 8;
                if s == 0 { y } else { (y >> s) | (x << (128 - s)) }
            }
            _ => return Err(self.fault(EXC_RI)),
        };
        self.set128(rd, v);
        Ok(1)
    }

    /// The halfword multiplies: products in LO / HI order (LO0 LO1 HI0 HI1 LO2 LO3 HI2 HI3)
    fn halfword_products(x: u128, y: u128) -> [i32; 8] {
        let (a, b) = (h(x), h(y));
        core::array::from_fn(|k| a[k] as i16 as i32 * b[k] as i16 as i32)
    }

    /// The word lanes 0 / 2: {HI, LO} pairs (as 64-bit accumulators)
    fn acc_pair(&self, k: usize) -> i64 {
        let (l, hh) = (w(self.lo), w(self.hi));
        ((hh[2 * k] as u64) << 32 | l[2 * k] as u64) as i64
    }

    fn set_pair(&mut self, k: usize, v: u64) {
        self.set_lo64(k, sext32(v as u32));
        self.set_hi64(k, sext32((v >> 32) as u32));
    }

    fn mmi2(&mut self, _i: u32, _rs: u32, _rt: u32, x: u128, y: u128, rd: u32, sa: u32) -> Res<u32> {
        let (a, b) = (w(x), w(y));
        match sa {
            0x00 | 0x04 | 0x0C => {
                // PMADDW / PMSUBW / PMULTW (word lanes 0 and 2, signed)
                let mut out = [0u64; 2];
                for k in 0..2 {
                    let p = a[2 * k] as i32 as i64 * b[2 * k] as i32 as i64;
                    let r = match sa { 0x00 => self.acc_pair(k).wrapping_add(p), 0x04 => self.acc_pair(k).wrapping_sub(p), _ => p };
                    self.set_pair(k, r as u64);
                    out[k] = r as u64;
                }
                self.set128(rd, fd(out));
                return Ok(3);
            }
            0x02 | 0x03 => {
                // PSLLVW / PSRLVW
                let mut out = [0u64; 2];
                for k in 0..2 {
                    let s = a[2 * k] & 31;
                    out[k] = sext32(if sa == 2 { b[2 * k] << s } else { b[2 * k] >> s });
                }
                self.set128(rd, fd(out));
            }
            0x08 => self.set128(rd, self.hi),
            0x09 => self.set128(rd, self.lo),
            0x0A => {
                // PINTH
                let (p, q) = (h(x), h(y));
                self.set128(rd, fh(core::array::from_fn(|k| if k & 1 == 0 { q[k / 2] } else { p[4 + k / 2] })));
            }
            0x0D => {
                // PDIVW
                for k in 0..2 {
                    let (q, r) = sdiv(a[2 * k] as i32, b[2 * k] as i32);
                    self.set_lo64(k, sext32(q));
                    self.set_hi64(k, sext32(r));
                }
                return Ok(10);
            }
            0x0E => self.set128(rd, fd([d(y)[0], d(x)[0]])),                        // PCPYLD
            0x10 | 0x14 | 0x1C => {
                // PMADDH / PMSUBH / PMULTH
                let p = Self::halfword_products(x, y);
                let (mut l, mut hh) = (w(self.lo), w(self.hi));
                let slots: [(bool, usize); 8] = [(false, 0), (false, 1), (true, 0), (true, 1), (false, 2), (false, 3), (true, 2), (true, 3)];
                for (k, &(is_hi, lane)) in slots.iter().enumerate() {
                    let reg = if is_hi { &mut hh } else { &mut l };
                    reg[lane] = match sa {
                        0x10 => (reg[lane] as i32).wrapping_add(p[k]) as u32,
                        0x14 => (reg[lane] as i32).wrapping_sub(p[k]) as u32,
                        _ => p[k] as u32,
                    };
                }
                self.lo = fw(l);
                self.hi = fw(hh);
                self.set128(rd, fw([l[0], hh[0], l[2], hh[2]]));
                return Ok(3);
            }
            0x11 | 0x15 => {
                // PHMADH / PHMSBH: pairwise sums (differences) of the halfword products
                let p = Self::halfword_products(x, y);
                let sub = sa == 0x15;
                let pair = |k: usize| if sub { p[2 * k + 1].wrapping_sub(p[2 * k]) } else { p[2 * k + 1].wrapping_add(p[2 * k]) } as u32;
                let odd = |k: usize| if sub { !(p[2 * k + 1] as u32) } else { p[2 * k + 1] as u32 };
                self.lo = fw([pair(0), odd(0), pair(2), odd(2)]);
                self.hi = fw([pair(1), odd(1), pair(3), odd(3)]);
                self.set128(rd, fw([pair(0), pair(1), pair(2), pair(3)]));
                return Ok(3);
            }
            0x12 => self.set128(rd, x & y),
            0x13 => self.set128(rd, x ^ y),
            0x1A => { let q = h(y); self.set128(rd, fh([q[2], q[1], q[0], q[3], q[6], q[5], q[4], q[7]])); }   // PEXEH
            0x1B => { let q = h(y); self.set128(rd, fh([q[3], q[2], q[1], q[0], q[7], q[6], q[5], q[4]])); }   // PREVH
            0x1D => {
                // PDIVBW: each word / the low halfword of rt
                let dv = h(y)[0] as i16 as i32;
                let (mut l, mut hh) = ([0u32; 4], [0u32; 4]);
                for k in 0..4 {
                    let (q, r) = sdiv(a[k] as i32, dv);
                    l[k] = q;
                    hh[k] = r as i16 as i32 as u32;
                }
                self.lo = fw(l);
                self.hi = fw(hh);
                return Ok(10);
            }
            0x1E => self.set128(rd, fw([b[2], b[1], b[0], b[3]])),                      // PEXEW
            0x1F => self.set128(rd, fw([b[1], b[2], b[0], b[3]])),                      // PROT3W
            _ => return Err(self.fault(EXC_RI)),
        }
        Ok(1)
    }

    fn mmi3(&mut self, _i: u32, _rs: u32, _rt: u32, x: u128, y: u128, rd: u32, sa: u32) -> Res<u32> {
        let (a, b) = (w(x), w(y));
        match sa {
            0x00 | 0x0C => {
                // PMADDUW / PMULTUW
                let mut out = [0u64; 2];
                for k in 0..2 {
                    let p = a[2 * k] as u64 * b[2 * k] as u64;
                    let r = if sa == 0 { (self.acc_pair(k) as u64).wrapping_add(p) } else { p };
                    self.set_pair(k, r);
                    out[k] = r;
                }
                self.set128(rd, fd(out));
                return Ok(3);
            }
            0x03 => {
                // PSRAVW
                let mut out = [0u64; 2];
                for k in 0..2 { out[k] = sext32(((b[2 * k] as i32) >> (a[2 * k] & 31)) as u32); }
                self.set128(rd, fd(out));
            }
            0x08 => self.hi = x,
            0x09 => self.lo = x,
            0x0A => {
                // PINTEH
                let (p, q) = (h(x), h(y));
                self.set128(rd, fh(core::array::from_fn(|k| if k & 1 == 0 { q[k] } else { p[k - 1] })));
            }
            0x0D => {
                // PDIVUW
                for k in 0..2 {
                    let (n, dv) = (a[2 * k], b[2 * k]);
                    let (q, r) = if dv == 0 { (u32::MAX, n) } else { (n / dv, n % dv) };
                    self.set_lo64(k, sext32(q));
                    self.set_hi64(k, sext32(r));
                }
                return Ok(10);
            }
            0x0E => self.set128(rd, fd([d(x)[1], d(y)[1]])),                        // PCPYUD
            0x12 => self.set128(rd, x | y),
            0x13 => self.set128(rd, !(x | y)),
            0x1A => { let q = h(y); self.set128(rd, fh([q[0], q[2], q[1], q[3], q[4], q[6], q[5], q[7]])); }   // PEXCH
            0x1B => { let q = h(y); self.set128(rd, fh([q[0], q[0], q[0], q[0], q[4], q[4], q[4], q[4]])); }   // PCPYH
            0x1E => self.set128(rd, fw([b[0], b[2], b[1], b[3]])),                      // PEXCW
            _ => return Err(self.fault(EXC_RI)),
        }
        Ok(1)
    }
}
