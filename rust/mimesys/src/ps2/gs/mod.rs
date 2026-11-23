//! The Graphics Synthesizer: privileged registers (display, CSR / IMR), the general
//! registers written through the GIF, the vertex queue, host <-> local transfers, the CLUT
//! buffer, and the display read-out. Drawing is `draw.rs`, memory layouts `mem.rs`.
//! From the GS user's manual as summarised by ps2tek.
//!
//! The GIF (`feed`) takes 128-bit quadwords from PATH3 (the GIF DMA channel and the GIF
//! FIFO at 1000_6000): GIFtags, then PACKED / REGLIST / IMAGE data.

pub mod draw;
pub mod mem;
#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};

// general registers
pub const PRIM: usize = 0x00;
pub const RGBAQ: usize = 0x01;
pub const ST: usize = 0x02;
pub const UV: usize = 0x03;
pub const XYZF2: usize = 0x04;
pub const XYZ2: usize = 0x05;
pub const TEX0_1: usize = 0x06;
pub const TEX0_2: usize = 0x07;
pub const CLAMP_1: usize = 0x08;
pub const FOG: usize = 0x0A;
pub const XYZF3: usize = 0x0C;
pub const XYZ3: usize = 0x0D;
pub const TEX1_1: usize = 0x14;
pub const TEX2_1: usize = 0x16;
pub const XYOFFSET_1: usize = 0x18;
pub const PRMODECONT: usize = 0x1A;
pub const PRMODE: usize = 0x1B;
pub const TEXCLUT: usize = 0x1C;
pub const TEXA: usize = 0x3B;
pub const FOGCOL: usize = 0x3D;
pub const SCISSOR_1: usize = 0x40;
pub const ALPHA_1: usize = 0x42;
pub const DTHE: usize = 0x45;
pub const COLCLAMP: usize = 0x46;
pub const TEST_1: usize = 0x47;
pub const PABE: usize = 0x49;
pub const FBA_1: usize = 0x4A;
pub const FRAME_1: usize = 0x4C;
pub const ZBUF_1: usize = 0x4E;
pub const BITBLTBUF: usize = 0x50;
pub const TRXPOS: usize = 0x51;
pub const TRXREG: usize = 0x52;
pub const TRXDIR: usize = 0x53;
pub const HWREG: usize = 0x54;
pub const SIGNAL: usize = 0x60;
pub const FINISH: usize = 0x61;
pub const LABEL: usize = 0x62;

// privileged registers (1200_0000 + 10h each)
pub const PMODE: usize = 0;
pub const SMODE2: usize = 2;
pub const DISPFB1: usize = 7;
pub const DISPLAY1: usize = 8;
pub const DISPFB2: usize = 9;
pub const DISPLAY2: usize = 10;
pub const BGCOLOR: usize = 14;

#[inline]
pub fn bits(v: u64, lo: u32, n: u32) -> u32 { ((v >> lo) & ((1u64 << n) - 1)) as u32 }

#[derive(Clone, Copy, Default, Serialize, Deserialize, Debug, PartialEq)]
pub struct Vertex {
    /// screen position, 12.4 fixed point, after XYOFFSET
    pub x: i32,
    pub y: i32,
    pub z: u32,
    pub f: u8,
    pub rgba: [u8; 4],
    pub q: f32,
    pub s: f32,
    pub t: f32,
    /// UV, 10.4 fixed point (texels × 16)
    pub u: u32,
    pub v: u32,
}

/// A host -> local transfer in progress
#[derive(Default, Clone, Serialize, Deserialize)]
struct Transfer {
    active: bool,
    x: u32,
    y: u32,
    /// leftover bytes of a partly sent pixel group
    pending: Vec<u8>,
}

/// The GIF's PATH3 tag state
#[derive(Default, Clone, Serialize, Deserialize)]
struct GifTag {
    active: bool,
    nloop: u32,
    flg: u32,
    nreg: u32,
    regs: u64,
    /// which register of the loop is next
    reg: u32,
    /// PACKED RGBAQ takes Q from the last ST
    q: f32,
}

#[derive(Serialize, Deserialize)]
pub struct Gs {
    /// PMODE .. BGCOLOR
    pub pregs: [u64; 15],
    /// CSR flags: SIGNAL, FINISH, HSINT, VSINT, EDWINT
    pub csr: u64,
    pub imr: u64,
    pub busdir: u64,
    pub siglblid: u64,
    /// the field being displayed (CSR.FIELD)
    pub field: bool,
    pub regs: Vec<u64>,
    pub vram: Vec<u8>,
    /// the vertex queue and how many are in it
    queue: [Vertex; 3],
    count: usize,
    /// for fans: the first vertex stays
    fan_first: Option<Vertex>,
    xfer: Transfer,
    gif: GifTag,
    /// the CLUT buffer (raw entries, 16-bit formats in the low half), and the CBP0 / CBP1
    /// copies TEX0's CLD compares against
    pub clut: Vec<u32>,
    cbp: [u32; 2],
    /// debugging: primitives drawn, pixels written
    #[serde(skip)]
    pub prims: u64,
    #[serde(skip)]
    pub pixels: u64,
    /// primitives waiting to be drawn (drawn before anything reads local memory, and at
    /// the end of every frame)
    #[serde(skip)]
    pub(crate) batch: draw::Batch,
    /// time spent drawing (speed report)
    #[serde(skip)]
    pub draw_ns: u64,
}

impl Default for Gs {
    fn default() -> Self { Self::new() }
}

impl Gs {
    pub fn new() -> Self {
        Self {
            pregs: [0; 15], csr: 0, imr: 0x7F00, busdir: 0, siglblid: 0, field: false,
            regs: vec![0; 0x63], vram: vec![0; mem::VRAM_SIZE],
            queue: [Vertex::default(); 3], count: 0, fan_first: None,
            xfer: Transfer::default(), gif: GifTag::default(),
            clut: vec![0; 512], cbp: [0; 2], prims: 0, pixels: 0, batch: Default::default(), draw_ns: 0,
        }.with_reset_regs()
    }

    /// Power-on values: PRMODECONT.AC = 1 (attributes from PRIM)
    fn with_reset_regs(mut self) -> Self {
        self.regs[PRMODECONT] = 1;
        self
    }

    // ------------------------------------------------------------------ CSR

    /// CSR as read: the flags, FIFO empty, FIELD, revision 1B, ID 55
    pub fn csr_read(&self) -> u64 {
        (self.csr & 0x1F) | 0x4000 | (self.field as u64) << 13 | 0x1B << 16 | 0x55 << 24
    }

    pub fn csr_write(&mut self, v: u64) {
        self.csr &= !(v & 0x1F);
        if v & 0x200 != 0 { self.csr = 0; self.gif = GifTag::default(); self.xfer = Transfer::default(); }
    }

    /// An unmasked flag (the GS interrupt line to INTC)
    pub fn irq(&self) -> bool { self.csr & 0x1F & !(self.imr >> 8) != 0 }

    // ------------------------------------------------------------------ the GIF

    /// One quadword from PATH3
    pub fn feed(&mut self, q: u128) {
        let g = &mut self.gif;
        if !g.active {
            let lo = q as u64;
            g.nloop = bits(lo, 0, 15);
            g.flg = bits(lo, 58, 2);
            g.nreg = match bits(lo, 60, 4) { 0 => 16, n => n };
            g.regs = (q >> 64) as u64;
            g.reg = 0;
            g.active = g.nloop > 0;
            // PRE: PRIM comes with the tag (PACKED only)
            if lo >> 46 & 1 != 0 && g.flg == 0 { self.write_reg(PRIM, bits(lo, 47, 11) as u64); }
            return;
        }
        match g.flg {
            0 => {
                let r = (g.regs >> (4 * g.reg)) & 0xF;
                self.packed(r as u32, q);
                self.next_reg();
            }
            1 => {
                // REGLIST: two registers a quadword
                for half in 0..2 {
                    if !self.gif.active { break; }
                    let r = (self.gif.regs >> (4 * self.gif.reg)) & 0xF;
                    let v = (q >> (64 * half)) as u64;
                    if r != 0xE && r != 0xF { self.write_reg(r as usize, v); }
                    self.next_reg();
                }
            }
            _ => {
                // IMAGE: data for HWREG
                self.write_reg(HWREG, q as u64);
                self.write_reg(HWREG, (q >> 64) as u64);
                let g = &mut self.gif;
                g.nloop -= 1;
                if g.nloop == 0 { g.active = false; }
            }
        }
    }

    fn next_reg(&mut self) {
        let g = &mut self.gif;
        g.reg += 1;
        if g.reg >= g.nreg {
            g.reg = 0;
            g.nloop -= 1;
            if g.nloop == 0 { g.active = false; }
        }
    }

    /// A PACKED register
    fn packed(&mut self, r: u32, q: u128) {
        let lo = q as u64;
        let hi = (q >> 64) as u64;
        match r {
            0x0 => self.write_reg(PRIM, lo & 0x7FF),
            0x1 => {
                let v = bits(lo, 0, 8) as u64 | (bits(lo, 32, 8) as u64) << 8 | (bits(hi, 0, 8) as u64) << 16
                    | (bits(hi, 32, 8) as u64) << 24 | (self.gif.q.to_bits() as u64) << 32;
                self.write_reg(RGBAQ, v);
            }
            0x2 => {
                self.gif.q = f32::from_bits(hi as u32);
                self.write_reg(ST, lo);
            }
            0x3 => self.write_reg(UV, bits(lo, 0, 14) as u64 | (bits(lo, 32, 14) as u64) << 16),
            0x4 | 0xC => {
                let v = bits(lo, 0, 16) as u64 | (bits(lo, 32, 16) as u64) << 16
                    | (bits(hi, 4, 24) as u64) << 32 | (bits(hi, 36, 8) as u64) << 56;
                let adc = hi >> 47 & 1 != 0;
                self.write_reg(if r == 0xC || adc { XYZF3 } else { XYZF2 }, v);
            }
            0x5 | 0xD => {
                let v = bits(lo, 0, 16) as u64 | (bits(lo, 32, 16) as u64) << 16 | (hi & 0xFFFF_FFFF) << 32;
                let adc = hi >> 47 & 1 != 0;
                self.write_reg(if r == 0xD || adc { XYZ3 } else { XYZ2 }, v);
            }
            0xA => self.write_reg(FOG, (bits(hi, 36, 8) as u64) << 56),
            0xE => self.write_reg(bits(hi, 0, 8) as usize, lo),
            0xF => {}
            _ => self.write_reg(r as usize, lo),
        }
    }

    // ------------------------------------------------------------------ registers

    pub fn write_reg(&mut self, r: usize, v: u64) {
        if r >= self.regs.len() { return; }
        match r {
            PRIM => {
                self.regs[PRIM] = v;
                self.count = 0;
                self.fan_first = None;
            }
            XYZF2 | XYZ2 | XYZF3 | XYZ3 => {
                self.regs[r] = v;
                let kick = r == XYZF2 || r == XYZ2;
                let with_f = r == XYZF2 || r == XYZF3;
                self.vertex(v, with_f, kick);
            }
            TEX0_1 | TEX0_2 => {
                self.regs[r] = v;
                self.load_clut(v);
            }
            TRXDIR => {
                self.regs[r] = v;
                self.start_transfer();
            }
            HWREG => self.hwreg(v),
            SIGNAL => {
                let (id, mask) = (v as u32, (v >> 32) as u32);
                let old = self.siglblid as u32;
                self.siglblid = (self.siglblid & !0xFFFF_FFFF) | ((old & !mask) | (id & mask)) as u64;
                self.csr |= 1;
            }
            FINISH => self.csr |= 2,
            LABEL => {
                let (id, mask) = (v as u32, (v >> 32) as u32);
                let old = (self.siglblid >> 32) as u32;
                self.siglblid = (self.siglblid & 0xFFFF_FFFF) | (((old & !mask) | (id & mask)) as u64) << 32;
            }
            TEXFLUSH_REG => {}
            _ => self.regs[r] = v,
        }
    }

    /// The drawing attributes: PRIM's, or PRMODE's when PRMODECONT.AC = 0
    pub fn prim_attr(&self) -> u64 {
        if self.regs[PRMODECONT] & 1 != 0 { self.regs[PRIM] } else { (self.regs[PRIM] & 7) | (self.regs[PRMODE] & !7) }
    }

    fn vertex(&mut self, xyz: u64, with_f: bool, kick: bool) {
        let attr = self.prim_attr();
        let ctx = bits(attr, 9, 1) as usize;
        let off = self.regs[XYOFFSET_1 + ctx];
        let rgbaq = self.regs[RGBAQ];
        let st = self.regs[ST];
        let uv = self.regs[UV];
        let vx = Vertex {
            x: bits(xyz, 0, 16) as i32 - bits(off, 0, 16) as i32,
            y: bits(xyz, 16, 16) as i32 - bits(off, 32, 16) as i32,
            z: if with_f { bits(xyz, 32, 24) } else { (xyz >> 32) as u32 },
            f: if with_f { bits(xyz, 56, 8) as u8 } else { bits(self.regs[FOG], 56, 8) as u8 },
            rgba: [rgbaq as u8, (rgbaq >> 8) as u8, (rgbaq >> 16) as u8, (rgbaq >> 24) as u8],
            q: f32::from_bits((rgbaq >> 32) as u32),
            s: f32::from_bits(st as u32),
            t: f32::from_bits((st >> 32) as u32),
            u: bits(uv, 0, 14),
            v: bits(uv, 16, 14),
        };
        let kind = attr & 7;
        let need = match kind { 0 => 1, 1 | 2 | 6 => 2, 7 => 0, _ => 3 };
        if need == 0 { return; }
        if self.count < 3 { self.queue[self.count] = vx; }
        self.count += 1;
        if kind == 5 && self.fan_first.is_none() { self.fan_first = Some(vx); }
        if self.count < need { return; }
        let q = self.queue;
        if kick {
            self.prims += 1;
            match kind {
                0 => self.draw_point(&q[0]),
                1 | 2 => self.draw_line(&q[0], &q[1]),
                6 => self.draw_sprite(&q[0], &q[1]),
                5 => { let f = self.fan_first.unwrap_or(q[0]); self.draw_triangle(&f, &q[1], &q[2]); }
                _ => self.draw_triangle(&q[0], &q[1], &q[2]),
            }
        }
        // what stays queued: strips keep the last vertices, fans the first and the last
        match kind {
            2 => { self.queue[0] = q[1]; self.count = 1; }
            4 => { self.queue[0] = q[1]; self.queue[1] = q[2]; self.count = 2; }
            5 => { self.queue[1] = q[2]; self.count = 2; }
            _ => self.count = 0,
        }
    }

    // ------------------------------------------------------------------ transfers

    fn start_transfer(&mut self) {
        self.flush();
        let dir = bits(self.regs[TRXDIR], 0, 2);
        let pos = self.regs[TRXPOS];
        self.xfer = Transfer { active: false, x: 0, y: 0, pending: Vec::new() };
        match dir {
            0 => self.xfer.active = true,
            2 => self.local_to_local(),
            _ => {}
        }
        let _ = pos;
    }

    /// Host -> local data (HWREG or GIF IMAGE): 64 bits of packed pixels
    fn hwreg(&mut self, v: u64) {
        if !self.xfer.active { return; }
        self.flush();
        let bb = self.regs[BITBLTBUF];
        let (dbp, dbw, dpsm) = (bits(bb, 32, 14), bits(bb, 48, 6), bits(bb, 56, 6));
        let pos = self.regs[TRXPOS];
        let (dx, dy) = (bits(pos, 32, 11), bits(pos, 48, 11));
        let reg = self.regs[TRXREG];
        let (w, h) = (bits(reg, 0, 12).max(1), bits(reg, 32, 12));
        let bpp = mem::bpp(dpsm);
        let mut data = std::mem::take(&mut self.xfer.pending);
        data.extend_from_slice(&v.to_le_bytes());
        let mut i = 0usize;
        while self.xfer.y < h {
            let px = match bpp {
                32 => { if data.len() - i < 4 { break; } let p = u32::from_le_bytes(data[i..i + 4].try_into().unwrap()); i += 4; p }
                24 => { if data.len() - i < 3 { break; } let p = data[i] as u32 | (data[i + 1] as u32) << 8 | (data[i + 2] as u32) << 16; i += 3; p }
                16 => { if data.len() - i < 2 { break; } let p = u16::from_le_bytes([data[i], data[i + 1]]) as u32; i += 2; p }
                8 => { if data.len() - i < 1 { break; } let p = data[i] as u32; i += 1; p }
                _ => {
                    // 4-bit: two pixels a byte, low nibble first
                    if data.len() - i < 1 { break; }
                    let byte = data[i] as u32;
                    i += 1;
                    for k in 0..2 {
                        if self.xfer.y >= h { break; }
                        mem::write(&mut self.vram, dpsm, dx + self.xfer.x, dy + self.xfer.y, dbp, dbw, byte >> (4 * k) & 15);
                        self.advance(w);
                    }
                    continue;
                }
            };
            mem::write(&mut self.vram, dpsm, dx + self.xfer.x, dy + self.xfer.y, dbp, dbw, px);
            self.advance(w);
        }
        self.xfer.pending = data[i..].to_vec();
        if self.xfer.y >= h { self.xfer.active = false; }
    }

    fn advance(&mut self, w: u32) {
        self.xfer.x += 1;
        if self.xfer.x >= w { self.xfer.x = 0; self.xfer.y += 1; }
    }

    fn local_to_local(&mut self) {
        let bb = self.regs[BITBLTBUF];
        let (sbp, sbw, spsm) = (bits(bb, 0, 14), bits(bb, 16, 6), bits(bb, 24, 6));
        let (dbp, dbw, dpsm) = (bits(bb, 32, 14), bits(bb, 48, 6), bits(bb, 56, 6));
        let pos = self.regs[TRXPOS];
        let (sx, sy, dx, dy) = (bits(pos, 0, 11), bits(pos, 16, 11), bits(pos, 32, 11), bits(pos, 48, 11));
        let reg = self.regs[TRXREG];
        let (w, h) = (bits(reg, 0, 12), bits(reg, 32, 12));
        for y in 0..h {
            for x in 0..w {
                let p = mem::read(&self.vram, spsm, sx + x, sy + y, sbp, sbw);
                mem::write(&mut self.vram, dpsm, dx + x, dy + y, dbp, dbw, p);
            }
        }
    }

    // ------------------------------------------------------------------ the CLUT

    /// TEX0's CLD: when the CLUT buffer loads from CBP (CSM1 layout only)
    fn load_clut(&mut self, tex0: u64) {
        let cbp = bits(tex0, 37, 14);
        let cld = bits(tex0, 61, 3);
        let load = match cld {
            1 => true,
            2 => { self.cbp[0] = cbp; true }
            3 => { self.cbp[1] = cbp; true }
            4 => { let l = self.cbp[0] != cbp; self.cbp[0] = cbp; l }
            5 => { let l = self.cbp[1] != cbp; self.cbp[1] = cbp; l }
            _ => false,
        };
        if !load { return; }
        self.flush();
        let psm = bits(tex0, 20, 6);
        let cpsm = bits(tex0, 51, 4);
        let csa = bits(tex0, 56, 5);
        let eight = psm == mem::PSMT8 || psm == mem::PSMT8H;
        let entries = if eight { 256 } else { 16 };
        let cformat = if cpsm == 0 { mem::PSMCT32 } else if cpsm == 0xA { mem::PSMCT16S } else { mem::PSMCT16 };
        for i in 0..entries {
            // CSM1: 8-bit indices swap bits 3 and 4 and fill 16 x 16; 4-bit fill 8 x 2
            let (x, y) = if eight {
                let j = (i & 0xE7) | (i & 8) << 1 | (i & 0x10) >> 1;
                (j & 15, j >> 4)
            } else {
                (i & 7, i >> 3)
            };
            let v = mem::read(&self.vram, cformat, x, y, cbp, 1);
            // CSA offsets in 16-entry units (32-bit CLUTs use the first 256 slots)
            let slot = (if cformat == mem::PSMCT32 { (csa & 15) * 16 } else { csa * 16 } + i) as usize % 512;
            self.clut[slot] = v;
        }
    }

    // ------------------------------------------------------------------ the display

    /// The picture both read circuits show, as RGBA8, and its size
    pub fn display(&mut self, out: &mut Vec<u8>) -> (usize, usize) {
        self.flush();
        let pmode = self.pregs[PMODE];
        let (en1, en2) = (pmode & 1 != 0, pmode & 2 != 0);
        let circuits: Vec<usize> = [(en1, 0usize), (en2, 1)].iter().filter(|c| c.0).map(|c| c.1).collect();
        // the size from the first enabled circuit (else circuit 2's settings)
        let main = *circuits.first().unwrap_or(&1);
        let disp = self.pregs[DISPLAY1 + 2 * main];
        let magh = bits(disp, 23, 4) + 1;
        let magv = bits(disp, 27, 2) + 1;
        let w = ((bits(disp, 32, 12) + 1) / magh).clamp(1, 1024) as usize;
        let mut h = ((bits(disp, 44, 11) + 1) / magv).clamp(1, 1024) as usize;
        let smode2 = self.pregs[SMODE2];
        // interlaced field mode: each field reads half the lines
        if smode2 & 1 != 0 && smode2 & 2 != 0 { h = (h / 2).max(1); }
        out.resize(w * h * 4, 0);
        let bg = self.pregs[BGCOLOR];
        let alp = bits(pmode, 8, 8);
        let mmod = pmode >> 5 & 1;
        for y in 0..h {
            for x in 0..w {
                let mut c = [bits(bg, 0, 8), bits(bg, 8, 8), bits(bg, 16, 8)];
                // circuit 2 first (the background), circuit 1 merged over it
                for &k in circuits.iter().rev() {
                    let fb = self.pregs[DISPFB1 + 2 * k];
                    let fbp = bits(fb, 0, 9) * 32;
                    let fbw = bits(fb, 9, 6);
                    let psm = bits(fb, 15, 5);
                    let (dbx, dby) = (bits(fb, 32, 11), bits(fb, 43, 11));
                    let p = mem::read(&self.vram, psm, dbx + x as u32, dby + y as u32, fbp, fbw);
                    let (rgb, a) = draw::to_rgba(p, psm);
                    if k == 1 || circuits.len() == 1 {
                        c = [rgb[0] as u32, rgb[1] as u32, rgb[2] as u32];
                    } else {
                        let a = if mmod == 1 { alp } else { (a as u32 * 2).min(255) };
                        for i in 0..3 { c[i] = (rgb[i] as u32 * a + c[i] * (255 - a)) / 255; }
                    }
                }
                let o = (y * w + x) * 4;
                out[o..o + 4].copy_from_slice(&[c[0] as u8, c[1] as u8, c[2] as u8, 255]);
            }
        }
        (w, h)
    }
}

const TEXFLUSH_REG: usize = 0x3F;
