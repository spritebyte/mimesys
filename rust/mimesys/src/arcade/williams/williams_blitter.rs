//! Williams "Special Chip" blitter (SC1 / SC2). After MAME williamsblitter.cpp (BSD-3-Clause).
//!
//! CA00 control (start), CA01 solid colour, CA02-03 source, CA04-05 destination,
//! CA06-07 width / height (XOR 4 on the SC1, a chip bug). Control bits:
//! 7 skip even pixels (D7-D4), 6 skip odd pixels (D3-D0), 5 shift one pixel right,
//! 4 solid colour, 3 foreground only (colour 0 is transparent), 2 slow,
//! 1 destination is in screen layout (256-byte stride), 0 the same for the source.
//! The CPU is halted while it runs; `start` returns how many CPU (1 MHz) cycles that is.

use serde::{Deserialize, Serialize};

pub trait BlitterBus {
    /// A read through the CPU's map (ROM when it's banked in).
    fn blit_read(&mut self, addr: u16) -> u8;
    /// Destination read: video RAM below C000, whatever the banking.
    fn blit_read_dest(&mut self, addr: u16) -> u8;
    fn blit_write(&mut self, addr: u16, v: u8);
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Blitter {
    control: u8,
    solid_color: u8,
    sstart: u16,
    dstart: u16,
    width: u8,
    height: u8,
    /// 4 on the SC1, 0 on the SC2
    size_xor: u8,
    /// Sinistar: with the window on, video RAM at or above this isn't written
    clip: u16,
    pub window: bool,
}

impl Blitter {
    pub fn sc1(clip: u16) -> Self { Self { size_xor: 4, clip, ..Default::default() } }
    pub fn sc2(clip: u16) -> Self { Self { clip, ..Default::default() } }

    /// Register write (offset 0-7). Offset 0 runs the blit: returns CPU cycles to halt.
    pub fn write(&mut self, offset: u16, data: u8, bus: &mut dyn BlitterBus) -> u32 {
        match offset & 7 {
            0 => { self.control = data; return self.start(bus); }
            1 => self.solid_color = data,
            2 => self.sstart = (self.sstart & 0x00FF) | (data as u16) << 8,
            3 => self.sstart = (self.sstart & 0xFF00) | data as u16,
            4 => self.dstart = (self.dstart & 0x00FF) | (data as u16) << 8,
            5 => self.dstart = (self.dstart & 0xFF00) | data as u16,
            6 => self.width = data,
            _ => self.height = data,
        }
        0
    }

    fn start(&mut self, bus: &mut dyn BlitterBus) -> u32 {
        let w = match (self.width ^ self.size_xor) as u32 { 0 => 1, v => v };
        let h = match (self.height ^ self.size_xor) as u32 { 0 => 1, v => v };
        let c = self.control;
        let (no_even, no_odd) = (c & 0x80 != 0, c & 0x40 != 0);
        let (shift, solid, fg_only) = (c & 0x20 != 0, c & 0x10 != 0, c & 0x08 != 0);
        let (dst_256, src_256) = (c & 0x02 != 0, c & 0x01 != 0);
        let (sxadv, syadv) = if src_256 { (0x100u16, 1u16) } else { (1, w as u16) };
        let (dxadv, dyadv) = if dst_256 { (0x100u16, 1u16) } else { (1, w as u16) };

        let mut accesses = 0u32;
        let mut pixdata: u32 = 0;
        let (mut sstart, mut dstart) = (self.sstart, self.dstart);
        for _ in 0..h {
            let (mut src, mut dst) = (sstart, dstart);
            for _ in 0..w {
                let raw = bus.blit_read(src);
                let data = if shift {
                    pixdata = (pixdata << 8) | raw as u32;
                    ((pixdata >> 4) & 0xFF) as u8
                } else {
                    raw
                };
                // which nibbles of the destination survive
                let mut keep = 0xFFu8;
                if fg_only && data & 0xF0 == 0 { if no_even { keep &= 0x0F; } } else if !no_even { keep &= 0x0F; }
                if fg_only && data & 0x0F == 0 { if no_odd { keep &= 0xF0; } } else if !no_odd { keep &= 0xF0; }
                let cur = bus.blit_read_dest(dst) & keep;
                let v = cur | (if solid { self.solid_color } else { data } & !keep);
                if !self.window || dst < self.clip || dst >= 0xC000 { bus.blit_write(dst, v); }
                accesses += 2;
                src = src.wrapping_add(sxadv);
                dst = dst.wrapping_add(dxadv);
            }
            if dst_256 { dstart = (dstart & 0xFF00) | (dstart.wrapping_add(dyadv) & 0xFF); } else { dstart = dstart.wrapping_add(dyadv); }
            if src_256 { sstart = (sstart & 0xFF00) | (sstart.wrapping_add(syadv) & 0xFF); } else { sstart = sstart.wrapping_add(syadv); }
        }

        let clocks_4mhz = 4 + if c & 0x04 != 0 { 4 * (accesses + 2) } else { 2 * (accesses + 3) };
        (clocks_4mhz + 3) / 4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Mem(Vec<u8>);
    impl BlitterBus for Mem {
        fn blit_read(&mut self, a: u16) -> u8 { self.0[a as usize] }
        fn blit_read_dest(&mut self, a: u16) -> u8 { self.0[a as usize] }
        fn blit_write(&mut self, a: u16, v: u8) { self.0[a as usize] = v; }
    }

    #[test]
    fn linear_source_to_screen_with_transparency() {
        let mut m = Mem(vec![0; 0x10000]);
        // 2x2 bytes of source at D000: 0x10 0x02 / 0x34 0x00
        m.0[0xD000..0xD004].copy_from_slice(&[0x10, 0x02, 0x34, 0x00]);
        m.0[0x0100] = 0xAB;
        m.0[0x0001] = 0xCD;
        let mut b = Blitter::sc1(0xC000);
        for (o, v) in [(2, 0xD0), (3, 0x00), (4, 0x00), (5, 0x00), (6, 2 ^ 4), (7, 2 ^ 4)] { b.write(o, v, &mut m); }
        let cycles = b.write(0, 0x0A, &mut m);              // dest in screen layout, foreground only
        assert_eq!(m.0[0x0000], 0x10, "row 0, column 0");
        assert_eq!(m.0[0x0100], 0xA2, "column 1: colour 0 nibble keeps the old pixel");
        assert_eq!(m.0[0x0001], 0x34, "row 1");
        assert_eq!(m.0[0x0101], 0x00);
        assert_eq!(cycles, (4 + 2 * (8 + 3) + 3) / 4);
    }

    #[test]
    fn solid_fill() {
        let mut m = Mem(vec![0x11; 0x10000]);
        let mut b = Blitter::sc2(0xC000);
        for (o, v) in [(1, 0x77), (2, 0x00), (3, 0x00), (4, 0x20), (5, 0x00), (6, 3), (7, 1)] { b.write(o, v, &mut m); }
        b.write(0, 0x10, &mut m);
        assert_eq!(&m.0[0x2000..0x2004], &[0x77, 0x77, 0x77, 0x11]);
    }
}
