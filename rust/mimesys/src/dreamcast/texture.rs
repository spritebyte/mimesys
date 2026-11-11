//! PowerVR2 texture fetch: 1555 / 565 / 4444 / YUV422 / bump / 4bpp / 8bpp palette, twiddled,
//! stride (non-twiddled), VQ compressed and mipmapped. Texels come back as ARGB8888.
//! After MAME mame/sega/powervr2.cpp tex_get_info / tex_r_* (BSD-3-Clause).

/// Bits of `v` spread to the even positions (v < 1024).
const fn spread_table() -> [u32; 1024] {
    let mut t = [0u32; 1024];
    let mut i = 0;
    while i < 1024 {
        let mut r = 0;
        let mut b = 0;
        while b < 10 { r |= ((i as u32 >> b) & 1) << (2 * b); b += 1; }
        t[i] = r;
        i += 1;
    }
    t
}
static SPREAD: [u32; 1024] = spread_table();

/// Twiddled (Morton) texel index: the low `cd` bits of y in the even bits, of x in the odd
/// bits; the higher bits of the longer side follow above (MAME dilate0 / dilate1).
#[inline]
pub fn twiddle(x: u32, y: u32, cd: u32) -> u32 {
    let m = (1u32 << cd) - 1;
    SPREAD[(y & m & 1023) as usize] | SPREAD[(x & m & 1023) as usize] << 1 | ((x >> cd) | (y >> cd)) << (2 * cd)
}

pub fn cv_1555(c: u16) -> u32 {
    let c = c as u32;
    let r = (c >> 10 & 0x1F) << 3 | (c >> 12 & 7);
    let g = (c >> 5 & 0x1F) << 3 | (c >> 7 & 7);
    let b = (c & 0x1F) << 3 | (c >> 2 & 7);
    (if c & 0x8000 != 0 { 0xFF00_0000 } else { 0 }) | r << 16 | g << 8 | b
}

pub fn cv_565(c: u16) -> u32 {
    let c = c as u32;
    let r = (c >> 11) << 3 | (c >> 13);
    let g = (c >> 5 & 0x3F) << 2 | (c >> 9 & 3);
    let b = (c & 0x1F) << 3 | (c >> 2 & 7);
    0xFF00_0000 | r << 16 | g << 8 | b
}

pub fn cv_4444(c: u16) -> u32 {
    let c = c as u32;
    let n = |s: u32| (c >> s & 0xF) * 0x11;
    n(12) << 24 | n(8) << 16 | n(4) << 8 | n(0)
}

fn cv_yuv(c1: u16, c2: u16, x: u32) -> u32 {
    let u = 11 * ((c1 & 0xFF) as i32 - 128);
    let v = 11 * ((c2 & 0xFF) as i32 - 128);
    let y = (if x & 1 != 0 { c2 } else { c1 } >> 8) as i32;
    let r = (y + v / 8).clamp(0, 255) as u32;
    let g = (y - u / 32 - v / 16).clamp(0, 255) as u32;
    let b = (y + (3 * u) / 16).clamp(0, 255) as u32;
    0xFF00_0000 | r << 16 | g << 8 | b
}

/// A palette entry in the PAL_RAM_CTRL format (0 1555, 1 565, 2 4444, 3 8888).
pub fn cv_pal(e: u32, fmt: u32) -> u32 {
    match fmt & 3 {
        0 => cv_1555(e as u16),
        1 => cv_565(e as u16),
        2 => cv_4444(e as u16),
        _ => e,
    }
}

/// How u / v outside the texture fold back.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Wrap { Repeat, Flip, Clamp }

impl Wrap {
    #[inline]
    fn apply(self, i: i32, size: u32) -> u32 {
        let s = size as i32;
        match self {
            Wrap::Repeat => (i & (s - 1)) as u32,
            Wrap::Flip => (if i & s != 0 { !i } else { i } & (s - 1)) as u32,
            Wrap::Clamp => i.clamp(0, s - 1) as u32,
        }
    }
}

/// Everything needed to read one texture.
#[derive(Clone, Copy, Debug)]
pub struct Tex {
    pub fmt: u32,
    pub twiddled: bool,
    pub vq: bool,
    pub addr: u32,
    pub vqbase: u32,
    pub sx: u32,
    pub sy: u32,
    pub stride: u32,
    pub cd: u32,
    pub palbase: u32,
    pub pal_fmt: u32,
    pub wu: Wrap,
    pub wv: Wrap,
    pub bilinear: bool,
    /// mipmapped: levels below the top one (0 = not mipmapped), and the address the
    /// level offsets count from
    pub levels: u32,
    mip_base: u32,
}

/// Byte offset of the 2^j x 2^j level in a mipmap chain (smallest first): 16 bpp starts
/// with 6 bytes of padding, 8 bpp with 3, 4 bpp with 1.5; VQ index chains with 1x1 and
/// 2x2 sharing a byte each.
fn mip_offset(fmt: u32, vq: bool, j: u32) -> u32 {
    let sum = |n: u32| ((1u32 << (2 * n)) - 1) / 3; // 1 + 4 + ... + 4^(n-1)
    if vq { if j == 0 { 0 } else { 1 + sum(j - 1) } }
    else if fmt == 6 { 3 + sum(j) }
    else if fmt == 5 { (3 + sum(j)) / 2 }
    else { 6 + 2 * sum(j) }
}

const MIP_PAL8: [u32; 8] = [0x18, 0x58, 0x158, 0x558, 0x1558, 0x5558, 0x15558, 0x55558];
const MIP_16: [u32; 8] = [0x30, 0xB0, 0x2B0, 0xAB0, 0x2AB0, 0xAAB0, 0x2AAB0, 0xAAAB0];
const MIP_VQ: [u32; 8] = [0x6, 0x16, 0x56, 0x156, 0x556, 0x1556, 0x5556, 0x15556];

impl Tex {
    /// From a polygon's TSP and texture control words, TEXT_CONTROL and PAL_RAM_CTRL.
    pub fn new(tsp: u32, tcw: u32, text_control: u32, pal_ctrl: u32) -> Self {
        let fmt = tcw >> 27 & 7;
        let palette = fmt == 5 || fmt == 6;
        let vq = tcw & 0x4000_0000 != 0;
        let twiddled = palette || tcw & 0x0400_0000 == 0;
        let mip = twiddled && tcw & 0x8000_0000 != 0;
        let mut su = tsp >> 3 & 7;
        let mut sv = tsp & 7;
        if mip { sv = su; }
        let (sx, sy) = (8u32 << su, 8u32 << sv);
        let stride = if !twiddled && tcw & 0x0200_0000 != 0 { (text_control & 0x1F) * 32 } else { sx };
        let mut addr = (tcw & 0x1F_FFFF) << 3;
        let vqbase = addr;
        if vq { addr += 0x800; }
        let mip_base = addr;
        if mip {
            addr += if vq { MIP_VQ[su as usize] }
                else if fmt == 6 { MIP_PAL8[su as usize] }
                else if fmt == 5 { MIP_PAL8[su as usize] >> 1 }
                else { MIP_16[su as usize] };
            debug_assert_eq!(addr - mip_base, mip_offset(fmt, vq, 3 + su));
        }
        su = su.min(sv);
        let palbase = match fmt {
            5 => (tcw >> 21 & 0x3F) << 4,
            6 => (tcw >> 21 & 0x30) << 4,
            _ => 0,
        };
        let wrap = |clamp: u32, flip: u32| if tsp & clamp != 0 { Wrap::Clamp } else if tsp & flip != 0 { Wrap::Flip } else { Wrap::Repeat };
        Tex {
            fmt, twiddled, vq, addr, vqbase, sx, sy, stride, cd: 3 + su, palbase, pal_fmt: pal_ctrl & 3,
            wu: wrap(1 << 16, 1 << 18), wv: wrap(1 << 15, 1 << 17),
            bilinear: tsp >> 13 & 3 != 0,
            levels: if mip { 3 + su } else { 0 },
            mip_base,
        }
    }

    /// The texture `k` levels below the top (halved k times; mipmapped textures only).
    pub fn level(&self, k: u32) -> Tex {
        if self.levels == 0 || k == 0 { return *self; }
        let j = self.levels - k.min(self.levels);
        let size = 1u32 << j;
        Tex { sx: size, sy: size, stride: size, cd: j, addr: self.mip_base + mip_offset(self.fmt, self.vq, j), ..*self }
    }

    /// The texel at integer coordinates (wrapped by the texture's modes).
    pub fn texel(&self, vram: &[u8], pal: &[u32], xi: i32, yi: i32) -> u32 {
        let x = self.wu.apply(xi, self.sx);
        let y = self.wv.apply(yi, self.sy);
        let m = vram.len() - 1;
        let b = |a: u32| vram[a as usize & m];
        let h = |a: u32| u16::from_le_bytes([vram[a as usize & m], vram[(a as usize + 1) & m]]);
        // 16 bpp texel address (a texel offset in a VQ codebook entry for VQ)
        let off16 = |x: u32, y: u32| -> u32 {
            if self.vq {
                let idx = b(self.addr + twiddle(x >> 1, y >> 1, self.cd.saturating_sub(1))) as u32;
                self.vqbase + idx * 8 + twiddle(x & 1, y & 1, 1) * 2
            } else if self.twiddled {
                self.addr + twiddle(x, y, self.cd) * 2
            } else {
                self.addr + (self.stride * y + x) * 2
            }
        };
        match self.fmt {
            0 => cv_1555(h(off16(x, y))),
            1 => cv_565(h(off16(x, y))),
            2 => cv_4444(h(off16(x, y))),
            3 => {
                // YUV422: a pair of texels shares U (first) and V (second)
                let (a1, a2) = if self.twiddled && !self.vq {
                    let a = self.addr + twiddle(x & !1, y, self.cd) * 2;
                    (a, a + 4)
                } else if self.vq {
                    (off16(x & !1, y), off16(x | 1, y))
                } else {
                    let a = self.addr + (self.stride * y + (x & !1)) * 2;
                    (a, a + 2)
                };
                cv_yuv(h(a1), h(a2), x)
            }
            4 => 0xFFFF_FFFF, // bump map: lighting comes later
            5 => {
                let c = if self.vq {
                    let idx = b(self.addr + twiddle(x >> 1, y >> 1, self.cd.saturating_sub(1))) as u32;
                    b(self.vqbase + idx * 8 + twiddle(x & 1, y & 3, 1)) as u32 & 0xF
                } else {
                    let off = twiddle(x, y, self.cd);
                    (b(self.addr + (off >> 1)) as u32 >> ((off & 1) * 4)) & 0xF
                };
                cv_pal(pal[((self.palbase + c) & 0x3FF) as usize], self.pal_fmt)
            }
            6 => {
                let c = if self.vq {
                    let idx = b(self.addr + twiddle(x >> 1, y >> 1, self.cd.saturating_sub(1))) as u32;
                    b(self.vqbase + idx * 8 + twiddle(x & 1, y & 3, 1)) as u32
                } else {
                    b(self.addr + twiddle(x, y, self.cd)) as u32
                };
                cv_pal(pal[((self.palbase + c) & 0x3FF) as usize], self.pal_fmt)
            }
            _ => if (x ^ y) & 4 != 0 { 0xFFFF_FF00 } else { 0xFF00_00FF },
        }
    }

    /// The colour at texture coordinates u, v (0..1 across the texture), point or
    /// bilinear sampled, straight from video RAM.
    pub fn sample(&self, vram: &[u8], pal: &[u32], u: f32, v: f32) -> u32 {
        self.sample_with(u, v, |x, y| self.texel(vram, pal, x, y))
    }

    /// The whole texture as ARGB8888, row by row (for `sample_decoded`).
    pub fn decode(&self, vram: &[u8], pal: &[u32]) -> Vec<u32> {
        let mut out = Vec::with_capacity((self.sx * self.sy) as usize);
        for y in 0..self.sy as i32 { for x in 0..self.sx as i32 { out.push(self.texel(vram, pal, x, y)); } }
        out
    }

    /// `sample` from a decoded copy.
    #[inline]
    pub fn sample_decoded(&self, d: &[u32], u: f32, v: f32) -> u32 {
        let (wu, wv, sx, sy) = (self.wu, self.wv, self.sx, self.sy);
        if wu == Wrap::Repeat && wv == Wrap::Repeat {
            let (mx, my) = (sx as i32 - 1, sy as i32 - 1);
            return self.sample_with(u, v, |x, y| d[((y & my) as u32 * sx + (x & mx) as u32) as usize]);
        }
        self.sample_with(u, v, |x, y| d[(wv.apply(y, sy) * sx + wu.apply(x, sx)) as usize])
    }

    #[inline]
    fn sample_with(&self, u: f32, v: f32, texel: impl Fn(i32, i32) -> u32) -> u32 {
        let fu = u * self.sx as f32;
        let fv = v * self.sy as f32;
        if !self.bilinear {
            return texel(ffloor(fu), ffloor(fv));
        }
        let (fu, fv) = (fu - 0.5, fv - 0.5);
        let (x0, y0) = (ffloor(fu), ffloor(fv));
        let (ax, ay) = (((fu - x0 as f32) * 256.0) as u32, ((fv - y0 as f32) * 256.0) as u32);
        let c00 = texel(x0, y0);
        let c10 = texel(x0 + 1, y0);
        let c01 = texel(x0, y0 + 1);
        let c11 = texel(x0 + 1, y0 + 1);
        if c00 == c10 && c00 == c01 && c00 == c11 { return c00; }
        let mut out = 0;
        for s in [0u32, 8, 16, 24] {
            let ch = |c: u32| c >> s & 0xFF;
            let top = ch(c00) * (256 - ax) + ch(c10) * ax;
            let bot = ch(c01) * (256 - ax) + ch(c11) * ax;
            out |= ((top * (256 - ay) + bot * ay) >> 16) << s;
        }
        out
    }
}

/// floor() without the library call (values well inside i32).
#[inline]
fn ffloor(f: f32) -> i32 {
    let i = f as i32;
    if (i as f32) > f { i - 1 } else { i }
}
