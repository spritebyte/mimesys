//! The GS's 4 MB of local memory and its swizzled pixel layouts.
//!
//! Memory is pages (8 KB) of 32 blocks (256 bytes) of 4 columns (64 bytes). Each format
//! fills a page with a different-sized rectangle (32-bit 64x32, 16-bit 64x64, 8-bit
//! 128x64, 4-bit 128x128) and orders blocks and columns its own way. The bit rules below
//! are the GS manual's figures written as formulas (checked against the tables other
//! emulators print). Z formats are the colour ones with the in-page block number XOR 18h.
//!
//! Addresses: `bp` in blocks (256 bytes), `bw` in 64-pixel units; pixel positions wrap at
//! 2048. Results are byte offsets (nibble offsets for 4-bit).

pub const VRAM_SIZE: usize = 4 << 20;

pub const PSMCT32: u32 = 0x00;
pub const PSMCT24: u32 = 0x01;
pub const PSMCT16: u32 = 0x02;
pub const PSMCT16S: u32 = 0x0A;
pub const PSMT8: u32 = 0x13;
pub const PSMT4: u32 = 0x14;
pub const PSMT8H: u32 = 0x1B;
pub const PSMT4HL: u32 = 0x24;
pub const PSMT4HH: u32 = 0x2C;
pub const PSMZ32: u32 = 0x30;
pub const PSMZ24: u32 = 0x31;
pub const PSMZ16: u32 = 0x32;
pub const PSMZ16S: u32 = 0x3A;

#[inline] fn b(v: u32, i: u32) -> u32 { (v >> i) & 1 }

/// Block order inside a page
#[inline] fn blk32(bx: u32, by: u32) -> u32 { b(bx, 0) | b(by, 0) << 1 | b(bx, 1) << 2 | b(by, 1) << 3 | b(bx, 2) << 4 }
#[inline] fn blk16(bx: u32, by: u32) -> u32 { b(by, 0) | b(bx, 0) << 1 | b(by, 1) << 2 | b(bx, 1) << 3 | b(by, 2) << 4 }
#[inline] fn blk16s(bx: u32, by: u32) -> u32 { b(by, 0) | b(bx, 0) << 1 | b(by, 2) << 2 | b(by, 1) << 3 | b(bx, 1) << 4 }

/// The element inside a block (words, halfwords, bytes, nibbles)
#[inline] fn col32(x: u32, y: u32) -> u32 { (x & 1) | (y & 1) << 1 | (x >> 1 & 3) << 2 | (y >> 1 & 3) << 4 }
#[inline] fn col16(x: u32, y: u32) -> u32 {
    let xp = x & 7;
    b(x, 3) | (xp & 1) << 1 | (y & 1) << 2 | (xp >> 1 & 3) << 3 | (y >> 1 & 3) << 5
}
#[inline] fn col8(x: u32, y: u32) -> u32 {
    let (c, y4) = (y >> 2 & 3, y & 3);
    (y4 >> 1) | b(x, 3) << 1 | b(x, 0) << 2 | (y4 & 1) << 3 | b(x, 1) << 4 | (b(x, 2) ^ (y4 >> 1) ^ (c & 1)) << 5 | c << 6
}
#[inline] fn col4(x: u32, y: u32) -> u32 {
    let (c, y4) = (y >> 2 & 3, y & 3);
    (y4 >> 1) | b(x, 3) << 1 | b(x, 4) << 2 | b(x, 0) << 3 | (y4 & 1) << 4 | b(x, 1) << 5 | (b(x, 2) ^ (y4 >> 1) ^ (c & 1)) << 6 | c << 7
}

/// The formulas above, tabulated per page (offset of each pixel from the page's start:
/// bytes, nibbles for 4-bit), so an address is a page number and one lookup
struct Tables {
    t32: Vec<u32>, t32z: Vec<u32>,
    t16: Vec<u32>, t16s: Vec<u32>, t16z: Vec<u32>, t16sz: Vec<u32>,
    t8: Vec<u32>, t4: Vec<u32>,
}

fn tables() -> &'static Tables {
    static T: std::sync::OnceLock<Tables> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let mk = |w: u32, h: u32, f: &dyn Fn(u32, u32) -> u32| -> Vec<u32> {
            (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).map(|(x, y)| f(x, y)).collect()
        };
        Tables {
            t32: mk(64, 32, &|x, y| blk32(x >> 3 & 7, y >> 3 & 3) * 256 + col32(x & 7, y & 7) * 4),
            t32z: mk(64, 32, &|x, y| (blk32(x >> 3 & 7, y >> 3 & 3) ^ 0x18) * 256 + col32(x & 7, y & 7) * 4),
            t16: mk(64, 64, &|x, y| blk16(x >> 4 & 3, y >> 3 & 7) * 256 + col16(x & 15, y & 7) * 2),
            t16s: mk(64, 64, &|x, y| blk16s(x >> 4 & 3, y >> 3 & 7) * 256 + col16(x & 15, y & 7) * 2),
            t16z: mk(64, 64, &|x, y| (blk16(x >> 4 & 3, y >> 3 & 7) ^ 0x18) * 256 + col16(x & 15, y & 7) * 2),
            t16sz: mk(64, 64, &|x, y| (blk16s(x >> 4 & 3, y >> 3 & 7) ^ 0x18) * 256 + col16(x & 15, y & 7) * 2),
            t8: mk(128, 64, &|x, y| blk32(x >> 4 & 7, y >> 4 & 3) * 256 + col8(x & 15, y & 15)),
            t4: mk(128, 128, &|x, y| blk16(x >> 5 & 3, y >> 4 & 7) * 512 + col4(x & 31, y & 15)),
        }
    })
}

const VMASK: usize = VRAM_SIZE - 1;

/// Byte offset of a 32-bit pixel (PSMCT32 / 24, and Z32 / Z24 with `z`)
#[inline]
pub fn addr32(x: u32, y: u32, bp: u32, bw: u32, z: bool) -> usize {
    let (x, y) = (x & 2047, y & 2047);
    let page = (x >> 6) + (y >> 5) * bw.max(1);
    let t = tables();
    let i = ((y & 31) * 64 + (x & 63)) as usize;
    let off = if z { t.t32z[i] } else { t.t32[i] };
    ((bp + page * 32) as usize * 256 + off as usize) & VMASK
}

/// Byte offset of a 16-bit pixel (`s`: PSMCT16S's block order; `z`: a Z format)
#[inline]
pub fn addr16(x: u32, y: u32, bp: u32, bw: u32, s: bool, z: bool) -> usize {
    let (x, y) = (x & 2047, y & 2047);
    let page = (x >> 6) + (y >> 6) * bw.max(1);
    let t = tables();
    let i = ((y & 63) * 64 + (x & 63)) as usize;
    let off = match (s, z) { (false, false) => t.t16[i], (true, false) => t.t16s[i], (false, true) => t.t16z[i], _ => t.t16sz[i] };
    ((bp + page * 32) as usize * 256 + off as usize) & VMASK
}

/// Byte offset of an 8-bit pixel
#[inline]
pub fn addr8(x: u32, y: u32, bp: u32, bw: u32) -> usize {
    let (x, y) = (x & 2047, y & 2047);
    let page = (x >> 7) + (y >> 6) * (bw >> 1).max(1);
    let off = tables().t8[((y & 63) * 128 + (x & 127)) as usize];
    ((bp + page * 32) as usize * 256 + off as usize) & VMASK
}

/// Nibble offset of a 4-bit pixel
#[inline]
pub fn addr4(x: u32, y: u32, bp: u32, bw: u32) -> usize {
    let (x, y) = (x & 2047, y & 2047);
    let page = (x >> 7) + (y >> 7) * (bw >> 1).max(1);
    let off = tables().t4[((y & 127) * 128 + (x & 127)) as usize];
    ((bp + page * 32) as usize * 512 + off as usize) & (2 * VRAM_SIZE - 1)
}

#[inline] fn rd32(m: &[u8], o: usize) -> u32 { u32::from_le_bytes(m[o..o + 4].try_into().unwrap()) }
#[inline] fn wr32(m: &mut [u8], o: usize, v: u32) { m[o..o + 4].copy_from_slice(&v.to_le_bytes()); }
#[inline] fn rd16(m: &[u8], o: usize) -> u32 { u16::from_le_bytes(m[o..o + 2].try_into().unwrap()) as u32 }
#[inline] fn wr16(m: &mut [u8], o: usize, v: u32) { m[o..o + 2].copy_from_slice(&(v as u16).to_le_bytes()); }

/// Bits per pixel of a format
pub fn bpp(psm: u32) -> u32 {
    match psm {
        PSMCT32 | PSMZ32 | PSMT8H | PSMT4HL | PSMT4HH => 32,
        PSMCT24 | PSMZ24 => 24,
        PSMCT16 | PSMCT16S | PSMZ16 | PSMZ16S => 16,
        PSMT8 => 8,
        PSMT4 => 4,
        _ => 32,
    }
}

/// Bits per pixel in a host <-> local transfer: as `bpp`, except that 8H / 4HL / 4HH
/// move 8 / 4 bits a pixel (into the top of a 32-bit pixel)
pub fn xfer_bpp(psm: u32) -> u32 {
    match psm {
        PSMT8H => 8,
        PSMT4HL | PSMT4HH => 4,
        _ => bpp(psm),
    }
}

/// A pixel's raw value (32-bit formats: the whole word; 24: low 24 bits; 16: the halfword;
/// 8 / 4: the index, also from the high bits of a 32-bit pixel for 8H / 4HL / 4HH)
#[inline]
pub fn read(m: &[u8], psm: u32, x: u32, y: u32, bp: u32, bw: u32) -> u32 {
    match psm {
        PSMCT32 => rd32(m, addr32(x, y, bp, bw, false)),
        PSMZ32 => rd32(m, addr32(x, y, bp, bw, true)),
        PSMCT24 => rd32(m, addr32(x, y, bp, bw, false)) & 0xFF_FFFF,
        PSMZ24 => rd32(m, addr32(x, y, bp, bw, true)) & 0xFF_FFFF,
        PSMCT16 => rd16(m, addr16(x, y, bp, bw, false, false)),
        PSMCT16S => rd16(m, addr16(x, y, bp, bw, true, false)),
        PSMZ16 => rd16(m, addr16(x, y, bp, bw, false, true)),
        PSMZ16S => rd16(m, addr16(x, y, bp, bw, true, true)),
        PSMT8 => m[addr8(x, y, bp, bw)] as u32,
        PSMT4 => { let n = addr4(x, y, bp, bw); (m[n >> 1] >> ((n & 1) * 4) & 15) as u32 }
        PSMT8H => rd32(m, addr32(x, y, bp, bw, false)) >> 24,
        PSMT4HL => rd32(m, addr32(x, y, bp, bw, false)) >> 24 & 15,
        PSMT4HH => rd32(m, addr32(x, y, bp, bw, false)) >> 28,
        _ => rd32(m, addr32(x, y, bp, bw, false)),
    }
}

/// Store a pixel's raw value (the other bits of a shared word are kept)
#[inline]
pub fn write(m: &mut [u8], psm: u32, x: u32, y: u32, bp: u32, bw: u32, v: u32) {
    match psm {
        PSMCT32 => wr32(m, addr32(x, y, bp, bw, false), v),
        PSMZ32 => wr32(m, addr32(x, y, bp, bw, true), v),
        PSMCT24 | PSMZ24 => {
            let o = addr32(x, y, bp, bw, psm == PSMZ24);
            let old = rd32(m, o);
            wr32(m, o, (old & 0xFF00_0000) | (v & 0xFF_FFFF));
        }
        PSMCT16 => wr16(m, addr16(x, y, bp, bw, false, false), v),
        PSMCT16S => wr16(m, addr16(x, y, bp, bw, true, false), v),
        PSMZ16 => wr16(m, addr16(x, y, bp, bw, false, true), v),
        PSMZ16S => wr16(m, addr16(x, y, bp, bw, true, true), v),
        PSMT8 => m[addr8(x, y, bp, bw)] = v as u8,
        PSMT4 => {
            let n = addr4(x, y, bp, bw);
            let sh = (n & 1) * 4;
            m[n >> 1] = (m[n >> 1] & !(15 << sh)) | ((v as u8 & 15) << sh);
        }
        PSMT8H | PSMT4HL | PSMT4HH => {
            let o = addr32(x, y, bp, bw, false);
            let old = rd32(m, o);
            let new = match psm {
                PSMT8H => (old & 0x00FF_FFFF) | (v & 0xFF) << 24,
                PSMT4HL => (old & 0xF0FF_FFFF) | (v & 15) << 24,
                _ => (old & 0x0FFF_FFFF) | (v & 15) << 28,
            };
            wr32(m, o, new);
        }
        _ => wr32(m, addr32(x, y, bp, bw, false), v),
    }
}
