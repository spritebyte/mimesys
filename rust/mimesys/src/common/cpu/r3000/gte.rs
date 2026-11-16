//! The GTE (Geometry Transformation Engine), COP2, written from Nocash PSX-SPX's "GTE"
//! chapters: 32 data and 32 control registers and the 22 commands.
//!
//! - Command word: bit 19 sf (results >> 12), bits 17-18 matrix (rotation, light, colour,
//!   or the "garbage" one), bits 15-16 vector (V0-2 or IR), bits 13-14 translation (TR, BK,
//!   FC with its bug, none), bit 10 lm (IR clamps at 0), bits 0-5 the operation.
//! - MAC1-3 sums are 44-bit: each partial sum is checked for overflow (FLAG 30-25) and
//!   wrapped. IR1-3 saturate to -8000..7FFF (0..7FFF with lm), IR0 to 0..1000, colours to
//!   0..FF, SZ / OTZ to 0..FFFF, SX / SY to -400..3FF. FLAG bit 31 sums up 30-23 and 18-13.
//! - RTPS / RTPT divide H / SZ3 with the console's reciprocal table (the UNR method), so
//!   screen coordinates match the hardware's.
//! - Registers that aren't plain storage: VZ / IR read sign-extended, SZ / OTZ unsigned;
//!   SXYP pushes the screen fifo; IRGB / ORGB pack IR1-3; LZCR counts LZCS's leading bits.

use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Gte {
    pub data: [u32; 32],
    pub ctrl: [u32; 32],
    #[serde(skip)]
    flag: u32,
}

const UNR: [u8; 257] = unr_table();

const fn unr_table() -> [u8; 257] {
    let mut t = [0u8; 257];
    let mut i = 0;
    while i < 257 {
        let v = (0x4_0000 / (i as i32 + 0x100) + 1) / 2 - 0x101;
        t[i] = if v < 0 { 0 } else { v as u8 };
        i += 1;
    }
    t
}

// data registers
const IR0: usize = 8;
const SZ0: usize = 16;
const MAC0: usize = 24;
// control registers
const TRX: usize = 5;
const RBK: usize = 13;
const RFC: usize = 21;
const OFX: usize = 24;
const OFY: usize = 25;
const H: usize = 26;
const DQA: usize = 27;
const DQB: usize = 28;
const ZSF3: usize = 29;
const ZSF4: usize = 30;

impl Gte {
    // ------------------------------------------------------------------ registers

    pub fn read_data(&self, r: usize) -> u32 {
        let d = &self.data;
        match r {
            1 | 3 | 5 | 8 | 9 | 10 | 11 => d[r] as i16 as i32 as u32,
            7 | 16 | 17 | 18 | 19 => d[r] & 0xFFFF,
            15 => d[14],
            28 | 29 => {
                let c = |v: u32| ((v as i16 as i32) >> 7).clamp(0, 31) as u32;
                c(d[9]) | c(d[10]) << 5 | c(d[11]) << 10
            }
            31 => {
                let v = d[30];
                if (v as i32) < 0 { v.leading_ones() } else { v.leading_zeros() }
            }
            _ => d[r],
        }
    }

    pub fn write_data(&mut self, r: usize, v: u32) {
        match r {
            15 => {
                self.data[12] = self.data[13];
                self.data[13] = self.data[14];
                self.data[14] = v;
            }
            28 => {
                self.data[9] = (v & 0x1F) << 7;
                self.data[10] = ((v >> 5) & 0x1F) << 7;
                self.data[11] = ((v >> 10) & 0x1F) << 7;
            }
            29 | 31 => {}
            _ => self.data[r] = v,
        }
    }

    pub fn read_ctrl(&self, r: usize) -> u32 {
        match r {
            4 | 12 | 20 | 26 | 27 | 29 | 30 => self.ctrl[r] as i16 as i32 as u32,
            _ => self.ctrl[r],
        }
    }

    pub fn write_ctrl(&mut self, r: usize, v: u32) {
        self.ctrl[r] = match r {
            31 => {
                let f = v & 0x7FFF_F000;
                f | (((f & 0x7F87_E000) != 0) as u32) << 31
            }
            _ => v,
        };
    }

    // ------------------------------------------------------------------ helpers

    fn lo(v: u32) -> i64 { v as i16 as i64 }
    fn hi(v: u32) -> i64 { (v >> 16) as i16 as i64 }

    /// A 3x3 matrix from five control registers starting at `base`
    fn matrix(&self, base: usize) -> [[i64; 3]; 3] {
        let c = &self.ctrl;
        [
            [Self::lo(c[base]), Self::hi(c[base]), Self::lo(c[base + 1])],
            [Self::hi(c[base + 1]), Self::lo(c[base + 2]), Self::hi(c[base + 2])],
            [Self::lo(c[base + 3]), Self::hi(c[base + 3]), Self::lo(c[base + 4])],
        ]
    }

    fn vector(&self, n: usize) -> [i64; 3] {
        if n == 3 { return self.ir(); }
        let d = &self.data;
        [Self::lo(d[2 * n]), Self::hi(d[2 * n]), Self::lo(d[2 * n + 1])]
    }

    fn ir(&self) -> [i64; 3] { [Self::lo(self.data[9]), Self::lo(self.data[10]), Self::lo(self.data[11])] }

    fn ctrl_vec(&self, base: usize) -> [i64; 3] {
        [self.ctrl[base] as i32 as i64, self.ctrl[base + 1] as i32 as i64, self.ctrl[base + 2] as i32 as i64]
    }

    fn rgb(&self) -> [i64; 3] {
        let c = self.data[6];
        [(c & 0xFF) as i64, ((c >> 8) & 0xFF) as i64, ((c >> 16) & 0xFF) as i64]
    }

    /// Check a MAC1-3 partial sum (44-bit) and wrap it
    fn chk(&mut self, i: usize, v: i64) -> i64 {
        if v > (1 << 43) - 1 { self.flag |= 1 << (31 - i); }
        if v < -(1 << 43) { self.flag |= 1 << (28 - i); }
        (v << 20) >> 20
    }

    fn set_mac(&mut self, i: usize, v: i64, shift: u32) {
        let v = self.chk(i, v) >> shift;
        self.data[MAC0 + i] = v as u32;
    }

    fn mac(&self, i: usize) -> i64 { self.data[MAC0 + i] as i32 as i64 }

    fn sat_ir(&mut self, i: usize, v: i64, lm: bool) -> i64 {
        let min = if lm { 0 } else { -0x8000 };
        let c = if v < min || v > 0x7FFF { self.flag |= 1 << (25 - i); v.clamp(min, 0x7FFF) } else { v };
        self.data[IR0 + i] = c as u32;
        c
    }

    fn set_mac0(&mut self, v: i64) {
        if v > i32::MAX as i64 { self.flag |= 1 << 16; }
        if v < i32::MIN as i64 { self.flag |= 1 << 15; }
        self.data[MAC0] = v as u32;
    }

    fn set_ir0(&mut self, v: i64) {
        let c = if !(0..=0x1000).contains(&v) { self.flag |= 1 << 12; v.clamp(0, 0x1000) } else { v };
        self.data[IR0] = c as u32;
    }

    /// MAC1-3 to IR1-3
    fn mac_to_ir(&mut self, lm: bool) {
        for i in 1..=3 { let m = self.mac(i); self.sat_ir(i, m, lm); }
    }

    fn push_color(&mut self) {
        let mut c = [0u32; 3];
        for i in 0..3 {
            let v = self.mac(i + 1) >> 4;
            c[i] = if !(0..=255).contains(&v) { self.flag |= 1 << (21 - i); v.clamp(0, 255) as u32 } else { v as u32 };
        }
        self.data[20] = self.data[21];
        self.data[21] = self.data[22];
        self.data[22] = c[0] | c[1] << 8 | c[2] << 16 | (self.data[6] & 0xFF00_0000);
    }

    fn push_sz(&mut self, v: i64) {
        let c = if !(0..=0xFFFF).contains(&v) { self.flag |= 1 << 18; v.clamp(0, 0xFFFF) } else { v };
        self.data[SZ0] = self.data[SZ0 + 1];
        self.data[SZ0 + 1] = self.data[SZ0 + 2];
        self.data[SZ0 + 2] = self.data[SZ0 + 3];
        self.data[SZ0 + 3] = c as u32;
    }

    fn sz(&self, n: usize) -> i64 { (self.data[SZ0 + n] & 0xFFFF) as i64 }

    /// M * V + T << 12, into MAC1-3 (shifted) with the partial-sum checks
    fn mat_vec(&mut self, m: &[[i64; 3]; 3], v: &[i64; 3], t: &[i64; 3], shift: u32) {
        for i in 0..3 {
            let mut s = self.chk(i + 1, t[i] << 12);
            s = self.chk(i + 1, s + m[i][0] * v[0]);
            s = self.chk(i + 1, s + m[i][1] * v[1]);
            s = self.chk(i + 1, s + m[i][2] * v[2]);
            self.data[MAC0 + 1 + i] = (s >> shift) as u32;
        }
    }

    /// H / SZ3 with the reciprocal table; 1FFFF and FLAG 17 on overflow
    fn divide(&mut self) -> i64 {
        let h = (self.ctrl[H] & 0xFFFF) as u32;
        let sz3 = (self.data[SZ0 + 3] & 0xFFFF) as u32;
        if h < sz3 * 2 {
            let z = (sz3 as u16).leading_zeros();
            let n = (h as u64) << z;
            let d = (sz3 << z) as u64;
            let u = UNR[((d - 0x7FC0) >> 7) as usize] as u64 + 0x101;
            let d = (0x200_0080 - d * u) >> 8;
            let d = (0x80 + d * u) >> 8;
            (((n * d) + 0x8000) >> 16).min(0x1FFFF) as i64
        } else {
            self.flag |= 1 << 17;
            0x1FFFF
        }
    }

    // ------------------------------------------------------------------ commands

    pub fn command(&mut self, cmd: u32) {
        self.flag = 0;
        let sf = if cmd & (1 << 19) != 0 { 12 } else { 0 };
        let lm = cmd & (1 << 10) != 0;
        match cmd & 0x3F {
            0x01 => self.rtp(0, sf, lm, true),
            0x06 => self.nclip(),
            0x0C => self.op(sf, lm),
            0x10 => { self.dpc(self.rgb(), sf, lm); }
            0x11 => self.intpl(sf, lm),
            0x12 => self.mvmva(cmd, sf, lm),
            0x13 => self.ncd(0, sf, lm),
            0x14 => self.cdp(sf, lm),
            0x16 => { for n in 0..3 { self.ncd(n, sf, lm); } }
            0x1B => self.ncc(0, sf, lm),
            0x1C => self.cc(sf, lm),
            0x1E => self.nc(0, sf, lm),
            0x20 => { for n in 0..3 { self.nc(n, sf, lm); } }
            0x28 => self.sqr(sf, lm),
            0x29 => self.dcpl(sf, lm),
            0x2A => {
                for _ in 0..3 {
                    let c = self.data[20];
                    self.dpc([(c & 0xFF) as i64, ((c >> 8) & 0xFF) as i64, ((c >> 16) & 0xFF) as i64], sf, lm);
                }
            }
            0x2D => {
                let s = self.sz(1) + self.sz(2) + self.sz(3);
                self.avsz(Self::lo(self.ctrl[ZSF3]) * s);
            }
            0x2E => {
                let s = self.sz(0) + self.sz(1) + self.sz(2) + self.sz(3);
                self.avsz(Self::lo(self.ctrl[ZSF4]) * s);
            }
            0x30 => { for n in 0..3 { self.rtp(n, sf, lm, n == 2); } }
            0x3D => self.gpf(sf, lm),
            0x3E => self.gpl(sf, lm),
            0x3F => { for n in 0..3 { self.ncc(n, sf, lm); } }
            _ => {}
        }
        let f = self.flag & 0x7FFF_F000;
        self.ctrl[31] = f | (((f & 0x7F87_E000) != 0) as u32) << 31;
    }

    /// RTPS / RTPT for vertex `n`: rotate, translate, project
    fn rtp(&mut self, n: usize, sf: u32, lm: bool, last: bool) {
        let m = self.matrix(0);
        let v = self.vector(n);
        let t = self.ctrl_vec(TRX);
        self.mat_vec(&m, &v, &t, sf);
        let m1 = self.mac(1);
        let m2 = self.mac(2);
        let m3 = self.mac(3);
        self.sat_ir(1, m1, lm);
        self.sat_ir(2, m2, lm);
        // IR3's saturation flag looks at MAC3 >> 12 whatever sf says
        let min = if lm { 0 } else { -0x8000 };
        self.data[IR0 + 3] = m3.clamp(min, 0x7FFF) as u32;
        let raw = if sf == 0 { m3 >> 12 } else { m3 };
        if !(-0x8000..=0x7FFF).contains(&raw) { self.flag |= 1 << 22; }
        // SZ3 = MAC3 >> (12 - sf)
        let z = if sf == 0 { m3 >> 12 } else { m3 };
        self.push_sz(z);
        let d = self.divide();
        let ir = self.ir();
        let x = d * ir[0] + self.ctrl[OFX] as i32 as i64;
        self.set_mac0(x);
        let y = d * ir[1] + self.ctrl[OFY] as i32 as i64;
        self.set_mac0(y);
        let sx = x >> 16;
        let sy = y >> 16;
        let csx = if !(-0x400..=0x3FF).contains(&sx) { self.flag |= 1 << 14; sx.clamp(-0x400, 0x3FF) } else { sx };
        let csy = if !(-0x400..=0x3FF).contains(&sy) { self.flag |= 1 << 13; sy.clamp(-0x400, 0x3FF) } else { sy };
        self.data[12] = self.data[13];
        self.data[13] = self.data[14];
        self.data[14] = (csx as u32 & 0xFFFF) | (csy as u32) << 16;
        if last {
            let dq = d * Self::lo(self.ctrl[DQA]) + self.ctrl[DQB] as i32 as i64;
            self.set_mac0(dq);
            self.set_ir0(dq >> 12);
        }
    }

    fn nclip(&mut self) {
        let p = |r: u32| (Self::lo(r), Self::hi(r));
        let (x0, y0) = p(self.data[12]);
        let (x1, y1) = p(self.data[13]);
        let (x2, y2) = p(self.data[14]);
        self.set_mac0(x0 * y1 + x1 * y2 + x2 * y0 - x0 * y2 - x1 * y0 - x2 * y1);
    }

    fn op(&mut self, sf: u32, lm: bool) {
        let c = &self.ctrl;
        let (d1, d2, d3) = (Self::lo(c[0]), Self::lo(c[2]), Self::lo(c[4]));
        let ir = self.ir();
        self.set_mac(1, ir[2] * d2 - ir[1] * d3, sf);
        self.set_mac(2, ir[0] * d3 - ir[2] * d1, sf);
        self.set_mac(3, ir[1] * d1 - ir[0] * d2, sf);
        self.mac_to_ir(lm);
    }

    fn sqr(&mut self, sf: u32, lm: bool) {
        let ir = self.ir();
        for i in 0..3 { self.set_mac(i + 1, ir[i] * ir[i], sf); }
        self.mac_to_ir(lm);
    }

    fn avsz(&mut self, v: i64) {
        self.set_mac0(v);
        let o = v >> 12;
        let c = if !(0..=0xFFFF).contains(&o) { self.flag |= 1 << 18; o.clamp(0, 0xFFFF) } else { o };
        self.data[7] = c as u32;
    }

    fn mvmva(&mut self, cmd: u32, sf: u32, lm: bool) {
        let mx = (cmd >> 17) & 3;
        let vn = ((cmd >> 15) & 3) as usize;
        let cv = (cmd >> 13) & 3;
        let m = match mx {
            0 => self.matrix(0),
            1 => self.matrix(8),
            2 => self.matrix(16),
            _ => {
                let r = ((self.data[6] & 0xFF) as i64) << 4;
                let c = &self.ctrl;
                [[-r, r, Self::lo(self.data[IR0])], [Self::lo(c[1]); 3], [Self::lo(c[2]); 3]]
            }
        };
        let v = self.vector(vn);
        let t = match cv { 0 => self.ctrl_vec(TRX), 1 => self.ctrl_vec(RBK), 2 => self.ctrl_vec(RFC), _ => [0; 3] };
        if cv == 2 {
            // the FC bug: the first column's sum only sets flags, the result drops it
            for i in 0..3 {
                let s = self.chk(i + 1, (t[i] << 12) + m[i][0] * v[0]);
                let min = if lm { 0 } else { -0x8000 };
                if !(min..=0x7FFF).contains(&(s >> sf)) { self.flag |= 1 << (25 - (i + 1)); }
                let mut r = self.chk(i + 1, m[i][1] * v[1]);
                r = self.chk(i + 1, r + m[i][2] * v[2]);
                self.data[MAC0 + 1 + i] = (r >> sf) as u32;
            }
        } else {
            self.mat_vec(&m, &v, &t, sf);
        }
        self.mac_to_ir(lm);
    }

    /// Light matrix * V, then background colour + light colour matrix * IR
    fn light(&mut self, n: usize, sf: u32, lm: bool) {
        let l = self.matrix(8);
        let v = self.vector(n);
        self.mat_vec(&l, &v, &[0; 3], sf);
        self.mac_to_ir(lm);
        let c = self.matrix(16);
        let ir = self.ir();
        let bk = self.ctrl_vec(RBK);
        self.mat_vec(&c, &ir, &bk, sf);
        self.mac_to_ir(lm);
    }

    /// MAC = RGB * IR << 4 (the vertex colour times the light)
    fn color_mul(&mut self) {
        let rgb = self.rgb();
        let ir = self.ir();
        for i in 0..3 {
            let v = self.chk(i + 1, (rgb[i] * ir[i]) << 4);
            self.data[MAC0 + 1 + i] = v as u32;
        }
    }

    /// The depth cue: MAC moves toward the far colour by IR0, then >> sf
    fn interpolate(&mut self, sf: u32, lm: bool) {
        let fc = self.ctrl_vec(RFC);
        for i in 0..3 {
            let m = self.mac(i + 1);
            let d = self.chk(i + 1, (fc[i] << 12) - m) >> sf;
            self.sat_ir(i + 1, d, false);
        }
        let ir0 = Self::lo(self.data[IR0]);
        let ir = self.ir();
        for i in 0..3 {
            let m = self.mac(i + 1);
            self.set_mac(i + 1, ir[i] * ir0 + m, sf);
        }
        self.mac_to_ir(lm);
    }

    fn finish_color(&mut self, sf: u32, lm: bool) {
        for i in 1..=3 {
            let m = self.mac(i);
            self.set_mac(i, m, sf);
        }
        self.mac_to_ir(lm);
        self.push_color();
    }

    fn nc(&mut self, n: usize, sf: u32, lm: bool) {
        self.light(n, sf, lm);
        self.push_color();
    }

    fn ncc(&mut self, n: usize, sf: u32, lm: bool) {
        self.light(n, sf, lm);
        self.color_mul();
        self.finish_color(sf, lm);
    }

    fn ncd(&mut self, n: usize, sf: u32, lm: bool) {
        self.light(n, sf, lm);
        self.color_mul();
        self.interpolate(sf, lm);
        self.push_color();
    }

    fn cc(&mut self, sf: u32, lm: bool) {
        let c = self.matrix(16);
        let ir = self.ir();
        let bk = self.ctrl_vec(RBK);
        self.mat_vec(&c, &ir, &bk, sf);
        self.mac_to_ir(lm);
        self.color_mul();
        self.finish_color(sf, lm);
    }

    fn cdp(&mut self, sf: u32, lm: bool) {
        let c = self.matrix(16);
        let ir = self.ir();
        let bk = self.ctrl_vec(RBK);
        self.mat_vec(&c, &ir, &bk, sf);
        self.mac_to_ir(lm);
        self.color_mul();
        self.interpolate(sf, lm);
        self.push_color();
    }

    /// DPCS / DPCT: a colour toward the far colour
    fn dpc(&mut self, rgb: [i64; 3], sf: u32, lm: bool) {
        for i in 0..3 {
            let v = self.chk(i + 1, rgb[i] << 16);
            self.data[MAC0 + 1 + i] = v as u32;
        }
        self.interpolate(sf, lm);
        self.push_color();
    }

    fn dcpl(&mut self, sf: u32, lm: bool) {
        self.color_mul();
        self.interpolate(sf, lm);
        self.push_color();
    }

    fn intpl(&mut self, sf: u32, lm: bool) {
        let ir = self.ir();
        for i in 0..3 {
            let v = self.chk(i + 1, ir[i] << 12);
            self.data[MAC0 + 1 + i] = v as u32;
        }
        self.interpolate(sf, lm);
        self.push_color();
    }

    fn gpf(&mut self, sf: u32, lm: bool) {
        let ir0 = Self::lo(self.data[IR0]);
        let ir = self.ir();
        for i in 0..3 { self.set_mac(i + 1, ir0 * ir[i], sf); }
        self.mac_to_ir(lm);
        self.push_color();
    }

    fn gpl(&mut self, sf: u32, lm: bool) {
        let ir0 = Self::lo(self.data[IR0]);
        let ir = self.ir();
        for i in 0..3 {
            let m = self.mac(i + 1) << sf;
            self.set_mac(i + 1, m + ir0 * ir[i], sf);
        }
        self.mac_to_ir(lm);
        self.push_color();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gte() -> Gte {
        let mut g = Gte::default();
        // identity rotation (1.0 = 0x1000), no translation
        g.write_ctrl(0, 0x0000_1000);
        g.write_ctrl(2, 0x0000_1000);
        g.write_ctrl(4, 0x1000);
        g.write_ctrl(OFX, 160 << 16);
        g.write_ctrl(OFY, 120 << 16);
        g.write_ctrl(H, 200);
        g
    }

    #[test]
    fn rtps_projects_a_point() {
        let mut g = gte();
        g.write_data(0, (20u32 << 16) | 100);       // x 100, y 20
        g.write_data(1, 400);                       // z 400
        g.command(0x0008_0001 | 1 << 19);           // RTPS, sf
        // x' = 100 * 200 / 400 + 160 = 210, y' = 20 * 200 / 400 + 120 = 130
        let sxy = g.read_data(14);
        assert_eq!((sxy & 0xFFFF) as i16, 210);
        assert_eq!((sxy >> 16) as i16, 130);
        assert_eq!(g.read_data(19), 400, "SZ3");
        assert_eq!(g.read_ctrl(31), 0, "no flags");
    }

    #[test]
    fn divide_overflow_and_nclip() {
        let mut g = gte();
        g.write_data(0, 0);
        g.write_data(1, 50);                         // H 200 >= 2 * 50: overflow
        g.command(0x0008_0001);
        assert_ne!(g.read_ctrl(31) & (1 << 17), 0);
        assert_ne!(g.read_ctrl(31) & (1 << 31), 0);
        // NCLIP of (0,0) (10,0) (0,10): +100
        g.write_data(12, 0);
        g.write_data(13, 10);
        g.write_data(14, 10 << 16);
        g.command(0x06);
        assert_eq!(g.read_data(24) as i32, 100);
    }

    #[test]
    fn avsz_sqr_and_color() {
        let mut g = gte();
        for (k, z) in [100u32, 200, 300, 400].iter().enumerate() { g.write_data(16 + k, *z); }
        g.write_ctrl(ZSF3, 0x1000 / 3);
        g.command(0x2D);
        assert!((299..=300).contains(&g.read_data(7)), "OTZ = average of SZ1-3: {}", g.read_data(7));
        g.write_data(9, 3);
        g.write_data(10, (-4i32) as u32);
        g.write_data(11, 0x200);
        g.command(0x28);                             // SQR, sf 0
        assert_eq!(g.read_data(25), 9);
        assert_eq!(g.read_data(26), 16);
        assert_eq!(g.read_data(11), 0x7FFF, "IR3 saturated");
        assert_ne!(g.read_ctrl(31) & (1 << 22), 0);
        // GPF: IR0 * IR >> 12, pushed as a colour
        g.write_data(IR0, 0x1000);
        g.write_data(9, 0x800);
        g.write_data(10, 0x100);
        g.write_data(11, 0x2000);
        g.write_data(6, 0x2C00_0000);
        g.command(0x3D | 1 << 19);
        assert_eq!(g.read_data(22), 0x2CFF_1080, "R 0x80, G 0x10, B clamped, the code byte kept");
    }

    #[test]
    fn unr_table_ends() {
        assert_eq!(UNR[0], 0xFF);
        assert_eq!(UNR[256], 0x00);
    }
}
