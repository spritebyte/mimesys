//! VIF0 / VIF1: the VUs' DMA front ends (from ps2tek's VIF pages). Each word of the
//! stream is a command or its data:
//! - UNPACK (all formats: S / V2 / V3 / V4 × 32 / 16 / 8, V4-5) with the mask, the
//!   offset / difference modes, write-skipping and write-filling (STCYCL), and VIF1's
//!   double buffer (BASE / OFFSET / TOPS / DBF, the FLG bit)
//! - MPG into micro memory; MSCAL / MSCALF / MSCNT run the VU's program (to its end)
//! - VIF1's DIRECT / DIRECTHL go to the GS (PATH2), as do the VU1's XGKICKs (PATH1)
//!
//! - A command with the I bit (31) set raises the VIF's interrupt once it's done (unless
//!   ERR.MII) and stalls the VIF: later words wait in `pending` until FBRST.STC. The DMA
//!   channel has already finished by then; STAT's FQC shows the waiting words.
//!
//! Undefined fields (V2's Z / W, V3's W, write-filled rows with the mask off) are left
//! as they were.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

use super::vu::Vu;

/// What a VIF writes to and starts
pub struct VifCtx<'a> {
    pub code: &'a mut [u8],
    pub data: &'a mut [u8],
    pub vu: &'a mut Vu,
    /// quadwords for the GS (DIRECT, and the VU program's XGKICKs)
    pub gs: &'a mut Vec<u128>,
    /// the GIF channel (PATH3) is running, and held by something other than MSKPATH3
    /// (GIF_MODE M3R / GIF_CTRL PSE): FLUSHA waits for an unmasked PATH3 to finish
    pub gif_running: bool,
    pub gif_held: bool,
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
    /// ERR (bit 0 MII: no interrupts from the I bit)
    pub err: u32,
    /// VIF1's MSKPATH3: the GIF holds PATH3 (GIF_STAT M3P)
    #[serde(default)]
    pub mskpath3: bool,
    /// at a FLUSHA with PATH3 still sending: later words wait in `pending`
    #[serde(default)]
    pub flusha_wait: bool,
    /// just unmasked PATH3: waiting while it sends a packet
    #[serde(default)]
    pub p3_packet_wait: bool,
    /// stalled after an I-bit command (STAT VIS + INT) until FBRST.STC
    pub stalled: bool,
    /// words that arrived while stalled
    pub pending: VecDeque<u32>,
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
    #[serde(skip)]
    pub irqs: u64,
    /// debugging (PS2_VU1_SRC): EE source address of each word not yet read, and of the
    /// current UNPACK's first data word
    #[serde(skip)]
    pub dbg_srcq: VecDeque<u32>,
    #[serde(skip)]
    dbg_first: u32,
    /// debugging (PS2_GS_XFER): the source of the word being read, and (index in the
    /// output, source) of each DIRECT quadword sent to the GS
    #[serde(skip)]
    dbg_word_src: u32,
    #[serde(skip)]
    pub dbg_gs_src: Vec<(usize, u32)>,
}

impl Vif {
    pub fn new(vif1: bool) -> Self { Self { vif1, ..Default::default() } }

    /// Debugging: the command being fed and what's left of it
    pub fn feeding(&self) -> String {
        format!("code {:08X} left {} addr {:X} buf {} | cl {} wl {} mode {} | pending {}", self.code, self.left, self.addr, self.buf.len(), self.cl, self.wl, self.mode, self.pending.len())
    }

    /// STAT as the CPU reads it
    pub fn stat(&self) -> u32 {
        let mut s = if self.left > 0 { 1 } else { 0 };   // VPS: waiting for data
        if self.stalled { s |= 1 << 10 | 1 << 11; }     // VIS, INT
        s | (self.pending.len().div_ceil(4).min(16) as u32) << 24
    }

    /// FBRST: RST (bit 0) drops everything; STC (bit 3) ends a stall
    pub fn fbrst(&mut self, v: u32) {
        if v & 1 != 0 {
            self.pending.clear();
            self.stalled = false;
            self.flusha_wait = false;
            self.p3_packet_wait = false;
            self.left = 0;
            self.buf.clear();
        }
        if v & 8 != 0 { self.stalled = false; }
    }

    /// Feed words; returns true if the VIF raised its interrupt (and stalled)
    pub fn feed(&mut self, words: &[u32], cx: &mut VifCtx) -> bool {
        if self.stalled || self.flusha_wait || self.p3_packet_wait || !self.pending.is_empty() {
            self.pending.extend(words);
            if self.stalled || self.flusha_wait || self.p3_packet_wait { return false; }
            while let Some(w) = self.pending.pop_front() {
                if self.word(w, cx) { return true; }
                if self.flusha_wait || self.p3_packet_wait { return false; }
            }
            return false;
        }
        for (k, &w) in words.iter().enumerate() {
            let irq = self.word(w, cx);
            if irq || self.flusha_wait || self.p3_packet_wait {
                self.pending.extend(&words[k + 1..]);
                return irq;
            }
        }
        false
    }

    /// A command finished: its I bit stalls the VIF and interrupts
    fn done(&mut self) -> bool {
        if self.code & 0x8000_0000 != 0 && self.err & 1 == 0 {
            self.stalled = true;
            self.irqs += 1;
            return true;
        }
        false
    }

    /// Feed one word of the stream; true if a finished command interrupts
    pub fn word(&mut self, w: u32, cx: &mut VifCtx) -> bool {
        if self.vif1 && crate::common::cpu::r5900::src_track() {
            let src = self.dbg_srcq.pop_front().unwrap_or(u32::MAX);
            if self.left > 0 && self.buf.is_empty() { self.dbg_first = src; }
            self.dbg_word_src = src;
        }
        if self.left > 0 && self.vif1 && vif_log() && std::env::var_os("VIF_LOG_DATA").is_some() {
            println!("VIF1   data {w:08X}");
        }
        if self.left > 0 {
            self.data(w, cx);
            return self.left == 0 && self.done();
        }
        let cmd = (w >> 24) & 0x7F;
        let imm = w & 0xFFFF;
        let num = (w >> 16) & 0xFF;
        self.code = w;
        self.buf.clear();
        if self.vif1 && vif_log() {
            println!("VIF1 {w:08X} cl {} wl {} mode {} tops {:X} top {:X} itop {:X} mask {:08X} vu programs {}",
                self.cl, self.wl, self.mode, self.tops, self.top, self.itop, self.mask, cx.vu.programs);
        }
        match cmd {
            0x00 => {}                                              // NOP
            0x01 => { self.cl = imm & 0xFF; self.wl = imm >> 8 & 0xFF; }
            0x02 if self.vif1 => { self.ofst = imm & 0x3FF; self.dbf = false; self.tops = self.base; }
            0x03 if self.vif1 => self.base = imm & 0x3FF,
            0x04 => self.itop = imm & 0x3FF,
            0x05 => self.mode = imm & 3,
            0x06 => {                                                // MSKPATH3
                let mask = imm >> 15 & 1 != 0;
                // unmasked with PATH3 waiting: it gets the GS for (at least) one packet
                // before the VIF goes on
                if self.vif1 && self.mskpath3 && !mask && cx.gif_running && !cx.gif_held { self.p3_packet_wait = true; }
                self.mskpath3 = mask;
            }
            0x07 => self.mark = imm,
            0x10 | 0x11 => {}                                       // FLUSHE / FLUSH
            // FLUSHA: wait for PATH3 too (the textures a game unmasked PATH3 to send
            // must reach the GS before the draws after this)
            0x13 => if self.vif1 && cx.gif_running && !self.mskpath3 && !cx.gif_held { self.flusha_wait = true; },
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
        self.left == 0 && self.done()
    }

    /// Rows an UNPACK of `num` (0 = 256) writes that come from the stream
    fn rows_read(&self, num: u32) -> u32 {
        let n = if num == 0 { 256 } else { num };
        // WL 0 means 256 (PCSX2): with CL 1 that's one row read, the rest filled (KH2)
        let (cl, wl) = (self.cl, if self.wl == 0 { 256 } else { self.wl });
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
                cx.vu.code_gen += 1;
                self.addr += 4;
            }
            0x50 | 0x51 => {
                if self.buf.is_empty() && self.dbg_word_src != 0 && crate::common::cpu::r5900::src_track() {
                    self.dbg_gs_src.push((cx.gs.len(), self.dbg_word_src));
                }
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
        // WL 0 means 256 (PCSX2): with CL 1 that's one row read, the rest filled (KH2)
        let (cl, wl) = (self.cl, if self.wl == 0 { 256 } else { self.wl });
        let qmask = (cx.data.len() / 16 - 1) as u32;
        let mut bitpos = 0usize;
        let buf = std::mem::take(&mut self.buf);
        // `bits` bits at bit `p` (elements never span more than two words)
        let peek = |p: usize, bits: usize| -> u32 {
            let lo = buf.get(p / 32).copied().unwrap_or(0) as u64;
            let hi = buf.get(p / 32 + 1).copied().unwrap_or(0) as u64;
            let v = (lo | hi << 32) >> (p % 32);
            (if bits >= 32 { v } else { v & ((1u64 << bits) - 1) }) as u32
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
            let row_bit = bitpos;
            if has_data {
                if vl == 3 {
                    let v = peek(bitpos, 16); bitpos += 16;
                    vals = [Some((v & 0x1F) << 3), Some((v >> 5 & 0x1F) << 3), Some((v >> 10 & 0x1F) << 3), Some((v >> 15 & 1) << 7)];
                } else {
                    let bits = 32 >> vl;
                    let mut e = [0u32; 4];
                    for x in e.iter_mut().take(vn as usize + 1) { *x = ext(peek(bitpos, bits as usize), bits); bitpos += bits as usize; }
                    match vn {
                        0 => vals = [Some(e[0]); 4],
                        // V2 writes x y x y (games rely on the "undefined" half)
                        1 => vals = [Some(e[0]), Some(e[1]), Some(e[0]), Some(e[1])],
                        // V3 runs as V4: w is the next element in the stream (not consumed)
                        2 => {
                            let w = (bitpos + bits as usize <= buf.len() * 32).then(|| ext(peek(bitpos, bits as usize), bits));
                            vals = [Some(e[0]), Some(e[1]), Some(e[2]), w];
                        }
                        _ => for (k, &x) in e.iter().enumerate().take(vn as usize + 1) { vals[k] = Some(x); },
                    }
                }
            }
            let cyc = in_cycle.min(3);
            let a = ((addr & qmask) * 16) as usize;
            for k in 0..4 {
                let m = if masked { self.mask >> (cyc * 8 + k as u32 * 2) & 3 } else { 0 };
                let v = match m {
                    0 => match vals[k] {
                        Some(d) => match if vl == 3 { 0 } else { self.mode } {
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
            if self.vif1 && has_data {
                if let Some((q, want)) = crate::common::cpu::r5900::vu1_src_cfg() {
                    let w3 = u32::from_le_bytes(cx.data[a + 12..a + 16].try_into().unwrap());
                    if q == addr & qmask && want.is_none_or(|x| x == w3) {
                        let words: Vec<String> = (0..4).map(|k| format!("{:08X}", u32::from_le_bytes(cx.data[a + 4 * k..a + 4 * k + 4].try_into().unwrap()))).collect();
                        let src = if self.dbg_first == u32::MAX { u32::MAX } else { self.dbg_first.wrapping_add((row_bit / 8) as u32) };
                        let who = if src == u32::MAX { "unknown".to_string() } else { crate::common::cpu::r5900::wr_describe(src) };
                        crate::common::cpu::r5900::src_log(format!("{} from EE {:08X} (UNPACK {:08X} row {}, VU1 programs {}): {}",
                            words.join(" "), src, c, n, cx.vu.programs, who));
                    }
                }
            }
        }
    }
}

/// Debugging (tests): VIF_LOG prints every VIF1 command
fn vif_log() -> bool {
    static L: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *L.get_or_init(|| std::env::var_os("VIF_LOG").is_some())
}
