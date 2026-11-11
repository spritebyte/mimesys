//! The tile accelerator's YUV converter (TA area 10800000): YUV420 or YUV422 macroblocks
//! (16 x 16 pixels) become a YUV422 texture in video RAM, used by FMV players.
//! TA_YUV_TEX_BASE (5F8148, a 64-bit-path address; writing it restarts the converter),
//! TA_YUV_TEX_CTRL (5F814C: width in macroblocks - 1 in bits 0-5, height - 1 in 8-13,
//! bit 16 = input YUV422), TA_YUV_TEX_CNT (5F8150: macroblocks done). After the whole
//! picture: Holly IST bit 6.
//! Input macroblock: U then V (8 x 8 for YUV420, 8 x 16 for YUV422), then Y as four 8 x 8
//! blocks (top-left, top-right, bottom-left, bottom-right). Output: a stride (non-twiddled)
//! YUV422 picture, pixel pairs as U Y0 V Y1 bytes, (width x 16) pixels a row.
//! Layout from the PowerVR2 documentation; the output arrangement is unverified.

use serde::{Deserialize, Serialize};

pub const TA_YUV_TEX_BASE: u32 = 0x148;
pub const TA_YUV_TEX_CTRL: u32 = 0x14C;
pub const IST_YUV: u32 = 1 << 6;

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Yuv {
    #[serde(with = "serde_bytes")]
    buf: Vec<u8>,
    /// macroblocks converted since TA_YUV_TEX_BASE was written
    pub count: u32,
}

impl Yuv {
    pub fn reset(&mut self) { self.buf.clear(); self.count = 0; }

    /// Bytes from the FIFO; returns true when the picture is complete.
    pub fn push(&mut self, data: &[u8], base: u32, ctrl: u32, vram: &mut [u8]) -> bool {
        let yuv422 = ctrl & 0x1_0000 != 0;
        let size = if yuv422 { 512 } else { 384 };
        let (wb, hb) = ((ctrl & 0x3F) + 1, (ctrl >> 8 & 0x3F) + 1);
        let mut done = false;
        for &b in data {
            self.buf.push(b);
            if self.buf.len() < size { continue; }
            let mb = std::mem::take(&mut self.buf);
            let (bx, by) = (self.count % wb, self.count / wb % hb);
            let (c, ysrc) = mb.split_at(if yuv422 { 256 } else { 128 });
            let (us, vs) = c.split_at(c.len() / 2);
            let width = wb * 16;
            for y in 0..16u32 {
                for x in (0..16u32).step_by(2) {
                    let yat = |xx: u32| ysrc[((y / 8) * 2 + xx / 8) as usize * 64 + ((y % 8) * 8 + xx % 8) as usize];
                    let ci = if yuv422 { (y * 8 + x / 2) as usize } else { ((y / 2) * 8 + x / 2) as usize };
                    let px = bx * 16 + x;
                    let py = by * 16 + y;
                    let a = base.wrapping_add((py * width + px) * 2) as usize;
                    let m = vram.len() - 1;
                    for (k, v) in [us[ci], yat(x), vs[ci], yat(x + 1)].into_iter().enumerate() { vram[(a + k) & m] = v; }
                }
            }
            self.count += 1;
            if self.count % (wb * hb) == 0 { done = true; }
        }
        done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yuv420_macroblock_becomes_yuv422_pairs() {
        let mut vram = vec![0u8; 1 << 16];
        let mut y = Yuv::default();
        // 2 x 1 macroblocks
        let ctrl = 1;
        let mut mb = vec![0u8; 384];
        for k in 0..64 { mb[k] = 100 + k as u8; mb[64 + k] = 200 - k as u8; }
        for k in 0..256 { mb[128 + k] = k as u8; }
        assert!(!y.push(&mb, 0x1000, ctrl, &mut vram));
        // pixel (2, 9): Y from block 2 (bottom-left) row 1 col 2; U / V at (1, 4)
        let a = 0x1000 + (9 * 32 + 2) * 2;
        assert_eq!(&vram[a..a + 4], &[100 + 33, 128 + 8 + 2, 200 - 33, 128 + 8 + 3]);
        assert!(y.push(&mb, 0x1000, ctrl, &mut vram), "two macroblocks: done");
        let a = 0x1000 + (16 + 10) * 2;
        assert_eq!(vram[a + 1], 64 + 2, "second block, top-right Y block");
        assert_eq!(y.count, 2);
    }
}
