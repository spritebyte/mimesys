//! VIF0 / VIF1: the VUs' DMA front ends (from ps2tek's VIF pages). Each word of the
//! stream is a command or its data:
//! - UNPACK (all formats: S / V2 / V3 / V4 × 32 / 16 / 8, V4-5) with the mask, the
//!   offset / difference modes, write-skipping and write-filling (STCYCL), and VIF1's
//!   double buffer (BASE / OFFSET / TOPS / DBF, the FLG bit)
//! - MPG into micro memory; MSCAL / MSCALF / MSCNT run the VU's program (to its end)
//! - VIF1's DIRECT / DIRECTHL go to the GS (PATH2), as do the VU1's XGKICKs (PATH1)
//!
//! Undefined fields (V2's Z / W, V3's W, write-filled rows with the mask off) are left
//! as they were.

use serde::{Deserialize, Serialize};

use super::vu::Vu;

/// What a VIF writes to and starts
pub struct VifCtx<'a> {
    pub code: &'a mut [u8],
    pub data: &'a mut [u8],
    pub vu: &'a mut Vu,
    /// quadwords for the GS (DIRECT, and the VU program's XGKICKs)
    pub gs: &'a mut Vec<u128>,
}

#[derive(Default, Clone, Serialize, Deserialize)]
pub struct Vif {
    pub vif1: bool,
    /// STCYCL
    pub cl: u32,
    pub wl: u32,
    pub mode: u32,
    pub mask: u32,
    pub row: [u32; 4],
    pub col: [u32; 4],
    pub itop: u32,
    pub base: u32,
    pub ofst: u32,
    pub tops: u32,
    pub top: u32,
    pub dbf: bool,
    pub mark: u32,
    /// the command being fed data (whole command word), and words still to come
    code: u32,
    left: u32,
    /// MPG / UNPACK destination
    addr: u32,
    /// DIRECT: words gathered for the current quadword; UNPACK: all its data
    buf: Vec<u32>,
    /// debugging
    #[serde(skip)]
    pub direct_qw: u64,
    #[serde(skip)]
    pub mscal: u64,
    #[serde(skip)]
    pub unpacks: u64,
}

impl Vif {
    pub fn new(vif1: bool) -> Self { Self { vif1, ..Default::default() } }

    /// Feed one word of the stream
    pub fn word(&mut self, w: u32, cx: &mut VifCtx) {
        if self.left > 0 {
            self.data(w, cx);
            return;
        }
        let cmd = (w >> 24) & 0x7F;
        let imm = w & 0xFFFF;
        let num = (w >> 16) & 0xFF;
        self.code = w;
        self.buf.clear();
        match cmd {
            0x00 => {}                                              // NOP
            0x01 => { self.cl = imm & 0xFF; self.wl = imm >> 8 & 0xFF; }
            0x02 if self.vif1 => { self.ofst = imm & 0x3FF; self.dbf = false; self.tops = self.base; }
            0x03 if self.vif1 => self.base = imm & 0x3FF,
            0x04 => self.itop = imm & 0x3FF,
            0x05 => self.mode = imm & 3,
            0x06 => {}                                              // MSKPATH3
            0x07 => self.mark = imm,
            0x10 | 0x11 | 0x13 => {}                                // FLUSHE / FLUSH / FLUSHA
            0x14 | 0x15 => self.call(Some(imm * 8), cx),            // MSCAL / MSCALF
            0x17 => self.call(None, cx),                            // MSCNT
            0x20 => self.left = 1,                                  // STMASK
            0x30 | 0x31 => self.left = 4,                           // STROW / STCOL
            0x4A => {                                               // MPG
                let n = if num == 0 { 256 } else { num };
                self.addr = imm * 8;
                self.left = n * 2;
            }
            0x50 | 0x51 if self.vif1 => {                           // DIRECT / DIRECTHL
                let n = if imm == 0 { 65536 } else { imm };
                self.left = n * 4;
            }
            0x60..=0x7F => {                                        // UNPACK
                let (vn, vl) = (cmd >> 2 & 3, cmd & 3);
                let rows = self.rows_read(num);
                let bits = if vl == 3 { 16 } else { (32 >> vl) * (vn + 1) };
                self.left = (rows * bits).div_ceil(32);
                let mut a = imm & 0x3FF;
                if self.vif1 && w & (1 << 15) != 0 { a += self.tops; }
                self.addr = a;
                if self.left == 0 { self.unpack(cx); }
            }
            _ => {}
        }
    }

    /// Rows an UNPACK of `num` (0 = 256) writes that come from the stream
    fn rows_read(&self, num: u32) -> u32 {
        let n = if num == 0 { 256 } else { num };
        let (cl, wl) = (self.cl.max(1), self.wl.max(1));
        if wl <= cl { n } else { cl * (n / wl) + (n % wl).min(cl) }
    }

    fn call(&mut self, addr: Option<u32>, cx: &mut VifCtx) {
        self.mscal += 1;
        if self.vif1 {
            self.top = self.tops;
            self.dbf = !self.dbf;
            self.tops = self.base + if self.dbf { self.ofst } else { 0 };
            cx.vu.top = self.top as u16;
        }
        cx.vu.itop = self.itop as u16;
        let pc = addr.unwrap_or(cx.vu.tpc);
        cx.vu.start(pc);
        cx.vu.run(cx.code, cx.data, cx.gs);
    }

    fn data(&mut self, w: u32, cx: &mut VifCtx) {
        self.left -= 1;
        match self.code >> 24 & 0x7F {
            0x20 => self.mask = w,
            0x30 => self.row[3 - self.left as usize] = w,
            0x31 => self.col[3 - self.left as usize] = w,
            0x4A => {
                let a = self.addr as usize & (cx.code.len() - 1);
                cx.code[a..a + 4].copy_from_slice(&w.to_le_bytes());
                self.addr += 4;
            }
            0x50 | 0x51 => {
                self.buf.push(w);
                if self.buf.len() == 4 {
                    let q = self.buf.iter().enumerate().fold(0u128, |a, (k, &x)| a | (x as u128) << (32 * k));
                    self.buf.clear();
                    cx.gs.push(q);
                    self.direct_qw += 1;
                }
            }
            0x60..=0x7F => {
                self.buf.push(w);
                if self.left == 0 { self.unpack(cx); }
            }
            _ => {}
        }
    }

    /// Write a whole UNPACK's rows into VU data memory
    fn unpack(&mut self, cx: &mut VifCtx) {
        self.unpacks += 1;
        let c = self.code;
        let cmd = c >> 24 & 0x7F;
        let (vn, vl) = (cmd >> 2 & 3, cmd & 3);
        let num = match c >> 16 & 0xFF { 0 => 256, n => n };
        let masked = c & (1 << 28) != 0;
        let unsigned = c & (1 << 14) != 0;
        let (cl, wl) = (self.cl.max(1), self.wl.max(1));
        let qmask = (cx.data.len() / 16 - 1) as u32;
        let mut bitpos = 0usize;
        let buf = std::mem::take(&mut self.buf);
        let mut take = |bits: usize| -> u32 {
            let mut v = 0u64;
            for k in 0..bits {
                let p = bitpos + k;
                let word = buf.get(p / 32).copied().unwrap_or(0);
                v |= ((word >> (p % 32) & 1) as u64) << k;
            }
            bitpos += bits;
            v as u32
        };
        let ext = |v: u32, bits: u32| -> u32 {
            if bits == 32 || unsigned { v } else { ((v << (32 - bits)) as i32 >> (32 - bits)) as u32 }
        };
        for n in 0..num {
            let in_cycle = n % wl;
            let addr = if wl <= cl { self.addr + (n / wl) * cl + in_cycle } else { self.addr + n };
            let has_data = wl <= cl || in_cycle < cl;
            // this row's values (None = undefined)
            let mut vals: [Option<u32>; 4] = [None; 4];
            if has_data {
                if vl == 3 {
                    let v = take(16);
                    vals = [Some((v & 0x1F) << 3), Some((v >> 5 & 0x1F) << 3), Some((v >> 10 & 0x1F) << 3), Some((v >> 15 & 1) << 7)];
                } else {
                    let bits = 32 >> vl;
                    let e: Vec<u32> = (0..=vn).map(|_| ext(take(bits as usize), bits)).collect();
                    if vn == 0 { vals = [Some(e[0]); 4]; } else { for (k, &x) in e.iter().enumerate() { vals[k] = Some(x); } }
                }
            }
            let cyc = in_cycle.min(3);
            let a = ((addr & qmask) * 16) as usize;
            for k in 0..4 {
                let m = if masked { self.mask >> (cyc * 8 + k as u32 * 2) & 3 } else { 0 };
                let v = match m {
                    0 => match vals[k] {
                        Some(d) => match self.mode {
                            1 => Some(d.wrapping_add(self.row[k])),
                            2 => { self.row[k] = self.row[k].wrapping_add(d); Some(self.row[k]) }
                            _ => Some(d),
                        },
                        None => None,
                    },
                    1 => Some(self.row[k]),
                    2 => Some(self.col[cyc as usize]),
                    _ => None,
                };
                if let Some(v) = v { cx.data[a + 4 * k..a + 4 * k + 4].copy_from_slice(&v.to_le_bytes()); }
            }
        }
    }
}
