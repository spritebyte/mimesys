//! PowerVR2 (CLX2), stage 2 part: the register file at 5F8000, the sync pulse generator
//! (lines, vblank / hblank interrupts, SPG_STATUS), the framebuffer read-out, and the tile
//! accelerator (`ta.rs`) and the renderer (`render.rs`, run by STARTRENDER).
//! After the PowerVR2 / Holly documentation and MAME mame/sega/powervr2.cpp (BSD-3-Clause).

use serde::{Deserialize, Serialize};

pub use super::ta::Ta;

pub const OUT_W: usize = 640;
pub const OUT_H: usize = 480;

pub const ID: u32 = 0x000;
pub const REVISION: u32 = 0x004;
pub const STARTRENDER: u32 = 0x014;
pub const FB_R_CTRL: u32 = 0x044;
pub const FB_R_SOF1: u32 = 0x050;
pub const FB_R_SOF2: u32 = 0x054;
pub const FB_R_SIZE: u32 = 0x05C;
pub const SPG_HBLANK_INT: u32 = 0x0C8;
pub const SPG_VBLANK_INT: u32 = 0x0CC;
pub const SPG_CONTROL: u32 = 0x0D0;
pub const SPG_HBLANK: u32 = 0x0D4;
pub const SPG_LOAD: u32 = 0x0D8;
pub const SPG_VBLANK: u32 = 0x0DC;
pub const VO_CONTROL: u32 = 0x0E8;
pub const VO_BORDER_COL: u32 = 0x040;
pub const SPG_STATUS: u32 = 0x10C;
pub const TA_ISP_BASE: u32 = 0x128;
pub const TA_LIST_INIT: u32 = 0x144;
pub const TA_LIST_CONT: u32 = 0x160;

/// Holly normal interrupt bits raised by the PVR.
pub const IST_EOR_VIDEO: u32 = 1 << 0;
pub const IST_EOR_ISP: u32 = 1 << 1;
pub const IST_EOR_TSP: u32 = 1 << 2;
pub const IST_VBL_IN: u32 = 1 << 3;
pub const IST_VBL_OUT: u32 = 1 << 4;
pub const IST_HBL_IN: u32 = 1 << 5;
/// end of list for list types 0-4 (opaque, opaque modifier, translucent, translucent
/// modifier, punch-through)
pub const IST_LIST_END: [u32; 5] = [1 << 7, 1 << 8, 1 << 9, 1 << 10, 1 << 21];

#[derive(Clone, Serialize, Deserialize)]
pub struct Pvr {
    /// 5F8000-5F9FFF as words
    pub regs: Vec<u32>,
    /// current line of the sync generator
    pub vpos: u32,
    pub field: u32,
    /// fields started (frames shown)
    pub frames: u64,
    /// the next hblank-interrupt line (counting mode)
    next_hbl: u32,
    pub ta: Ta,
    /// renders started (debug)
    pub renders: u64,
    /// what the last render drew (debug)
    #[serde(skip)]
    pub last_render: super::render::Stats,
}

impl Default for Pvr {
    fn default() -> Self { Self::new() }
}

/// What a new line brings.
#[derive(Default, Debug, PartialEq)]
pub struct LineEvents {
    pub ist: u32,
    /// Maple's vblank-triggered DMA should run
    pub maple: bool,
    /// a field ended (vblank in): show the picture
    pub vblank: bool,
}

impl Pvr {
    pub fn new() -> Self {
        let mut p = Self { regs: vec![0; 0x800], vpos: 0, field: 0, frames: 0, next_hbl: 0, ta: Ta::default(), renders: 0, last_render: Default::default() };
        p.reset();
        p
    }

    /// Power-on values (MAME powervr2 device_reset).
    pub fn reset(&mut self) {
        self.regs.fill(0);
        let r = &mut self.regs;
        r[0x008 / 4] = 0x0000_0007;          // SOFTRESET
        r[VO_CONTROL as usize / 4] = 0x0000_0108;
        r[0x0EC / 4] = 0x0000_009D;          // VO_STARTX
        r[0x0F0 / 4] = 0x0015_0015;          // VO_STARTY
        r[SPG_HBLANK as usize / 4] = 0x007E_0345;
        r[SPG_LOAD as usize / 4] = 0x0106_0359;
        r[SPG_VBLANK as usize / 4] = 0x0150_0104;
        r[SPG_HBLANK_INT as usize / 4] = 0x031D_0000;
        r[SPG_VBLANK_INT as usize / 4] = 0x0150_0104;
        self.vpos = 0;
        self.field = 0;
        self.next_hbl = 0;
        self.ta = Ta::default();
    }

    pub fn reg(&self, off: u32) -> u32 { self.regs[(off as usize / 4) & 0x7FF] }

    pub fn interlaced(&self) -> bool { self.reg(SPG_CONTROL) & 0x10 != 0 }

    /// Lines per field (SPG_LOAD vertical count + 1).
    pub fn lines(&self) -> u32 { (self.reg(SPG_LOAD) >> 16 & 0x3FF) + 1 }

    /// CPU cycles (200 MHz) per line: (horizontal count + 1) pixel clocks; the pixel clock
    /// is 27 MHz for VGA, 13.5 MHz for NTSC / PAL, doubled when interlaced (MAME).
    pub fn line_cycles(&self) -> u64 {
        let h = (self.reg(SPG_LOAD) & 0x3FF) as u64 + 1;
        let ctrl = self.reg(SPG_CONTROL);
        let clk: u64 = if ctrl >> 6 & 3 == 0 || ctrl >> 6 & 3 == 3 { 26_944_080 } else { 13_458_568 };
        let clk = clk * if ctrl & 0x10 != 0 { 2 } else { 1 };
        (200_000_000 * h / clk).max(100)
    }

    pub fn read(&self, off: u32) -> u32 {
        match off {
            ID => 0x17FD_11DB,
            REVISION => 0x0000_0011,
            SPG_STATUS => self.spg_status(),
            _ => self.reg(off),
        }
    }

    /// Returns true when the write starts a render.
    pub fn write(&mut self, off: u32, v: u32) -> bool {
        let i = (off as usize / 4) & 0x7FF;
        match off {
            ID | REVISION | SPG_STATUS => return false,
            STARTRENDER => { self.renders += 1; return true; }
            TA_LIST_INIT => { if v >> 31 != 0 { let b = self.reg(TA_ISP_BASE); self.ta.list_init(b); } return false; }
            TA_LIST_CONT => { if v >> 31 != 0 { self.ta.list_cont(); } return false; }
            _ => {}
        }
        self.regs[i] = v;
        false
    }

    /// SPG_STATUS (MAME spg_status_r; hsync from the line start only).
    fn spg_status(&self) -> u32 {
        let vb = self.reg(SPG_VBLANK);
        let (vbstart, vbend) = (vb & 0x3FF, vb >> 16 & 0x3FF);
        let in_vblank = self.vpos >= vbstart || self.vpos < vbend;
        let mut vsync = (!in_vblank) as u32;
        let mut blank = (!in_vblank) as u32;
        let vo = self.reg(VO_CONTROL);
        if vo & 4 != 0 { blank ^= 1; }
        if vo & 2 != 0 { vsync ^= 1; }
        vsync << 13 | 1 << 12 | blank << 11 | self.field << 10 | (self.vpos & 0x3FF)
    }

    /// The sync generator moves to the next line.
    pub fn next_line(&mut self) -> LineEvents {
        let mut ev = LineEvents::default();
        self.vpos += 1;
        if self.vpos >= self.lines() {
            self.vpos = 0;
            self.field ^= 1;
            self.next_hbl = self.reg(SPG_HBLANK_INT) & 0x3FF;
        }
        let il = self.interlaced() as u32;
        let vbi = self.reg(SPG_VBLANK_INT);
        let (vbin, vbout) = ((vbi & 0x3FF) << il, (vbi >> 16 & 0x3FF) << il);
        // the Maple bus's vblank trigger, a line or two before vblank-in (counted round the
        // field: Windows CE puts vblank-in at line 0)
        let n = self.lines();
        if n > 1 + il && self.vpos == (vbin + n - (1 + il)) % n { ev.maple = true; }
        if self.vpos == vbin {
            ev.ist |= IST_VBL_IN;
            ev.vblank = true;
            self.frames += 1;
        }
        if self.vpos == vbout { ev.ist |= IST_VBL_OUT; }
        // hblank interrupt: mode 0 at one line, 1 every N lines, 2 every line
        let hbi = self.reg(SPG_HBLANK_INT);
        let line = hbi & 0x3FF;
        match hbi >> 12 & 3 {
            0 => if self.vpos == line { ev.ist |= IST_HBL_IN },
            1 => if self.vpos == self.next_hbl { ev.ist |= IST_HBL_IN; self.next_hbl += line.max(1); },
            _ => ev.ist |= IST_HBL_IN,
        }
        ev
    }

    /// The frame buffer addresses (32-bit view, within 8 MB) `output` reads.
    pub fn display_range(&self) -> (u32, u32) {
        let size = self.reg(FB_R_SIZE);
        let words = (size & 0x3FF) + 1;
        let lines = (size >> 10 & 0x3FF) + 1;
        let modulus = size >> 20 & 0x3FF;
        let stride = (words + modulus).saturating_sub(1) * 4;
        let (a, b) = (self.reg(FB_R_SOF1) & 0x7F_FFFF, self.reg(FB_R_SOF2) & 0x7F_FFFF);
        (a.min(b), a.max(b) + stride.max(4) * lines)
    }

    /// The picture the framebuffer registers point at, as RGBA (OUT_W x OUT_H).
    /// `vram32(a)` reads a byte of the 32-bit view of video RAM.
    pub fn output(&self, vram: &[u8], out: &mut [u8]) {
        let ctrl = self.reg(FB_R_CTRL);
        let border = self.reg(VO_BORDER_COL);
        let bcol = [(border >> 16) as u8, (border >> 8) as u8, border as u8, 0xFF];
        if ctrl & 1 == 0 {
            for px in out.chunks_exact_mut(4) { px.copy_from_slice(&bcol); }
            return;
        }
        let size = self.reg(FB_R_SIZE);
        let words = (size & 0x3FF) + 1;
        let lines = (size >> 10 & 0x3FF) + 1;
        let modulus = size >> 20 & 0x3FF;
        let stride = (words + modulus).saturating_sub(1) * 4;
        let depth = ctrl >> 2 & 3;
        let bpp = [2, 2, 3, 4][depth as usize];
        let width = (words * 4 / bpp) as usize;
        let hdouble = self.reg(VO_CONTROL) & 0x100 != 0;
        let interlaced = self.interlaced();
        let ldouble = ctrl & 2 != 0;
        let (sof1, sof2) = (self.reg(FB_R_SOF1), self.reg(FB_R_SOF2));
        let rd = |a: u32| vram[map32(a & 0x7F_FFFF)];
        for y in 0..OUT_H {
            let row = &mut out[y * OUT_W * 4..(y + 1) * OUT_W * 4];
            let (base, src_line) = if interlaced {
                (if y & 1 == 0 { sof1 } else { sof2 }, (y / 2) as u32)
            } else if ldouble {
                (sof1, (y / 2) as u32)
            } else {
                (sof1, y as u32)
            };
            if src_line >= lines {
                for px in row.chunks_exact_mut(4) { px.copy_from_slice(&bcol); }
                continue;
            }
            let line = base.wrapping_add(src_line * stride);
            // the usual case a word at a time: 16 or 32 bits a pixel, the line word-aligned
            // and wide enough (the slow loop below stays the reference)
            #[cfg(test)]
            let fast_ok = !SLOW_OUTPUT.load(std::sync::atomic::Ordering::Relaxed);
            #[cfg(not(test))]
            let fast_ok = true;
            let need = if hdouble { OUT_W / 2 } else { OUT_W };
            if fast_ok && depth != 2 && line & 3 == 0 && width >= need {
                let per = 4 / bpp as usize; // pixels a word
                let mut px = [[0u8; 4]; 2];
                for k in 0..need.div_ceil(per) {
                    let i = map32(line.wrapping_add(4 * k as u32) & 0x7F_FFFF);
                    let w = u32::from_le_bytes([vram[i], vram[i + 1], vram[i + 2], vram[i + 3]]);
                    let n = match depth {
                        0 => { for (j, c) in [w & 0xFFFF, w >> 16].into_iter().enumerate() { px[j] = [(c >> 7 & 0xF8) as u8, (c >> 2 & 0xF8) as u8, (c << 3 & 0xF8) as u8, 0xFF]; } 2 }
                        1 => { for (j, c) in [w & 0xFFFF, w >> 16].into_iter().enumerate() { px[j] = [(c >> 8 & 0xF8) as u8, (c >> 3 & 0xFC) as u8, (c << 3 & 0xF8) as u8, 0xFF]; } 2 }
                        _ => { px[0] = [(w >> 16) as u8, (w >> 8) as u8, w as u8, 0xFF]; 1 }
                    };
                    for j in 0..n {
                        let sx = k * per + j;
                        if sx >= need { break; }
                        if hdouble {
                            row[sx * 8..sx * 8 + 4].copy_from_slice(&px[j]);
                            row[sx * 8 + 4..sx * 8 + 8].copy_from_slice(&px[j]);
                        } else {
                            row[sx * 4..sx * 4 + 4].copy_from_slice(&px[j]);
                        }
                    }
                }
                continue;
            }
            for x in 0..OUT_W {
                let sx = if hdouble { x / 2 } else { x };
                let px = &mut row[x * 4..x * 4 + 4];
                if sx >= width { px.copy_from_slice(&bcol); continue; }
                let a = line.wrapping_add((sx * bpp as usize) as u32);
                let (r, g, b) = match depth {
                    0 => { let c = rd(a) as u32 | (rd(a + 1) as u32) << 8; (c >> 7 & 0xF8, c >> 2 & 0xF8, c << 3 & 0xF8) }
                    1 => { let c = rd(a) as u32 | (rd(a + 1) as u32) << 8; (c >> 8 & 0xF8, c >> 3 & 0xFC, c << 3 & 0xF8) }
                    _ => (rd(a + 2) as u32, rd(a + 1) as u32, rd(a) as u32),
                };
                px.copy_from_slice(&[r as u8, g as u8, b as u8, 0xFF]);
            }
        }
    }
}

/// Test tool: take the per-pixel paths of the picture output and the framebuffer write
/// (the references the word-at-a-time paths are checked against).
#[cfg(test)]
pub static SLOW_OUTPUT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Video RAM is two 4 MB banks side by side 32 bits at a time: the "64-bit" view (textures,
/// 04000000) is linear; the "32-bit" view (framebuffers, 05000000) has bank 0 in its low
/// 4 MB and bank 1 in the high 4 MB. Returns the linear offset of a 32-bit-view byte.
#[inline]
pub fn map32(a: u32) -> usize {
    let bank = a >> 22 & 1;
    (((a & 0x3F_FFFC) << 1) | bank << 2 | (a & 3)) as usize
}
