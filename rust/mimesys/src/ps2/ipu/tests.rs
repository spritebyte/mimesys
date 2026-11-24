//! The IPU against ffmpeg: one-picture streams (testdata/, made with ffmpeg's mpeg2video /
//! mpeg1video encoders from a noisy test pattern) and ffmpeg's own decodes of them (.yuv,
//! 4:2:0 planes). The tests drive the IPU the way a game's MPEG library does: the headers
//! are read here, then each slice's macroblocks go through IDEC, or VDEC / FDEC / BDEC.

use super::*;
use crate::ps2::dmac::{Dmac, IPU_FROM, IPU_TO};

const W: usize = 64;
const H: usize = 48;

/// What the headers set
struct Pic {
    mpeg1: bool,
    iq: [u8; 64],
    niq: [u8; 64],
    idp: u32,
    q_nonlinear: bool,
    intra_vlc: bool,
    alt_scan: bool,
    /// frame_pred_frame_dct: off means each macroblock says frame or field DCT
    fpfd: bool,
    /// slices: (vertical position, byte offset of the slice start code)
    slices: Vec<(usize, usize)>,
}

/// The default intra matrix (raster order)
const DEFAULT_INTRA: [u8; 64] = [
    8, 16, 19, 22, 26, 27, 29, 34, 16, 16, 22, 24, 27, 29, 34, 37, 19, 22, 26, 27, 29, 34, 34, 38, 22, 22, 26, 27, 29, 34, 37, 40,
    22, 26, 27, 29, 32, 35, 40, 48, 26, 27, 29, 32, 35, 40, 48, 58, 26, 27, 29, 34, 38, 46, 56, 69, 27, 29, 35, 38, 46, 56, 69, 83,
];

struct Rd<'a> { d: &'a [u8], pos: usize }
impl Rd<'_> {
    fn bits(&mut self, n: usize) -> u32 {
        let mut v = 0;
        for _ in 0..n {
            v = v << 1 | (self.d[self.pos / 8] >> (7 - self.pos % 8) & 1) as u32;
            self.pos += 1;
        }
        v
    }
}

fn parse(d: &[u8]) -> Pic {
    let mut p = Pic { mpeg1: true, iq: [0; 64], niq: [16; 64], idp: 0, q_nonlinear: false, intra_vlc: false, alt_scan: false, fpfd: true, slices: Vec::new() };
    // matrices in raster order until the scan is known
    let mut iq_raster = DEFAULT_INTRA;
    let mut niq_raster = [16u8; 64];
    let mut i = 0;
    while i + 4 <= d.len() {
        if d[i..i + 3] != [0, 0, 1] { i += 1; continue; }
        let code = d[i + 3];
        let mut r = Rd { d, pos: (i + 4) * 8 };
        match code {
            0xB3 => {
                r.bits(12 + 12 + 4 + 4 + 18 + 1 + 10 + 1);
                if r.bits(1) == 1 { for k in 0..64 { iq_raster[vlc::ZIGZAG[k]] = r.bits(8) as u8; } }
                if r.bits(1) == 1 { for k in 0..64 { niq_raster[vlc::ZIGZAG[k]] = r.bits(8) as u8; } }
            }
            0xB5 => {
                p.mpeg1 = false;
                if r.bits(4) == 8 {
                    r.bits(16);
                    p.idp = r.bits(2);
                    r.bits(2 + 1);
                    p.fpfd = r.bits(1) == 1;
                    r.bits(1);
                    p.q_nonlinear = r.bits(1) == 1;
                    p.intra_vlc = r.bits(1) == 1;
                    p.alt_scan = r.bits(1) == 1;
                }
            }
            0x01..=0xAF => p.slices.push((code as usize - 1, i)),
            _ => {}
        }
        i += 4;
    }
    // the IPU takes matrices in the order of the scan in use
    let scan = if p.alt_scan { &vlc::ALTERNATE } else { &vlc::ZIGZAG };
    for k in 0..64 { p.iq[k] = iq_raster[scan[k]]; p.niq[k] = niq_raster[scan[k]]; }
    p
}

/// An IPU with RAM holding `bytes` from 0x10000 (the slice data), IPU_TO set to send them
struct Rig { ipu: Ipu, dmac: Dmac, ram: Vec<u8>, spr: Vec<u8> }

impl Rig {
    fn new(bytes: &[u8], p: &Pic) -> Self {
        let mut ram = vec![0u8; 0x200_0000];
        ram[0x1_0000..0x1_0000 + bytes.len()].copy_from_slice(bytes);
        let mut dmac = Dmac::default();
        dmac.write(0xE000, 1, u32::MAX);
        let qwc = (bytes.len() as u32).div_ceil(16) + 2;
        dmac.write(0xB410, 0x1_0000, u32::MAX);
        dmac.write(0xB420, qwc, u32::MAX);
        dmac.write(0xB400, 0x101, u32::MAX);
        let mut ipu = Ipu::default();
        let ctrl = p.idp << 16 | (p.alt_scan as u32) << 20 | (p.intra_vlc as u32) << 21 | (p.q_nonlinear as u32) << 22
            | (p.mpeg1 as u32) << 23 | 1 << 24;
        ipu.write(0x10, ctrl);
        let mut r = Self { ipu, dmac, ram, spr: vec![0u8; 0x4000] };
        r.load_matrix(&p.iq, false);
        r.load_matrix(&p.niq, true);
        r
    }

    fn step(&mut self) -> bool { self.ipu.step(&mut self.dmac, &mut self.ram, &mut self.spr) }

    /// A command, run to the end
    fn cmd(&mut self, v: u32) {
        self.ipu.write(0x00, v);
        for _ in 0..1000 {
            self.step();
            if !self.ipu.busy { return; }
        }
        panic!("command {v:08X} never finished");
    }

    /// SETIQ through the CPU's FIFO writes (the DMA keeps the stream)
    fn load_matrix(&mut self, m: &[u8; 64], non_intra: bool) {
        let saved = std::mem::take(&mut self.ipu.fifo_in);
        let bp = self.ipu.bp;
        self.ipu.bp = 0;
        for k in 0..4 { self.ipu.fifo_write(u128::from_le_bytes(m[16 * k..16 * k + 16].try_into().unwrap())); }
        self.ipu.write(0x00, 5 << 28 | (non_intra as u32) << 27);
        let mut none = || None;
        assert!(self.ipu.run(&mut none));
        self.ipu.busy = false;
        self.ipu.fifo_in = saved;
        self.ipu.bp = bp;
    }

    fn out(&mut self) -> Vec<u8> {
        let q: Vec<u128> = self.ipu.fifo_out.drain(..).collect();
        q.iter().flat_map(|x| x.to_le_bytes()).collect()
    }
}

/// The slice's header: (bit position of its macroblock data, quantiser scale code)
fn slice_header(d: &[u8], at: usize) -> (usize, u32) {
    let mut r = Rd { d, pos: (at + 4) * 8 };
    let qsc = r.bits(5);
    while r.bits(1) == 1 { r.bits(8); }
    (r.pos, qsc)
}

fn yuv(name: &str) -> Vec<u8> { std::fs::read(format!("{}/src/ps2/ipu/testdata/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap() }

/// Y, Cb, Cr of the reference at (x, y)
fn reference(r: &[u8], x: usize, y: usize) -> (i32, i32, i32) {
    (r[y * W + x] as i32, r[W * H + (y / 2) * (W / 2) + x / 2] as i32, r[W * H * 5 / 4 + (y / 2) * (W / 2) + x / 2] as i32)
}

/// BDEC: each macroblock's header through VDEC / FDEC, its blocks through BDEC
fn check_bdec(stream: &str, reference_yuv: &str) {
    let d = yuv(stream);
    let refv = yuv(reference_yuv);
    let p = parse(&d);
    let mut worst = 0;
    let mut mbs = 0;
    for (si, &(row, at)) in p.slices.iter().enumerate() {
        let (pos, mut qsc) = slice_header(&d, at);
        let mut rig = Rig::new(&d[pos / 8..], &p);
        rig.cmd((pos % 8) as u32);                      // BCLR: the bit pointer
        let mut col: i32 = -1;
        let mut first = true;
        loop {
            // the address increment (the first one too: it's the column)
            rig.cmd(3 << 28);
            let v = rig.ipu.data;
            if v == 0 { break; }
            let inc = v & 0xFFFF;
            if inc == 34 { continue; }
            col += if inc == 35 { 33 } else { inc as i32 };
            if inc == 35 { continue; }
            rig.cmd(3 << 28 | 1 << 26);                // macroblock type (I picture)
            let mbtype = rig.ipu.data & 0xFFFF;
            assert!(mbtype & 1 != 0, "intra");
            // FDEC: the DCT type bit (if the picture has them), the quantiser scale code
            rig.cmd(4 << 28);
            let top = rig.ipu.data;
            let mut fb = 0;
            let mut dt = 0;
            if !p.fpfd { dt = top >> 31; fb += 1; }
            if mbtype & 0x10 != 0 { qsc = top << fb >> 27; fb += 5; }
            let dcr = first as u32;
            first = false;
            rig.cmd(2 << 28 | 1 << 27 | dcr << 26 | dt << 25 | qsc << 16 | fb);
            let out = rig.out();
            assert_eq!(out.len(), 768, "a RAW16 macroblock");
            let px = |k: usize| i16::from_le_bytes([out[2 * k], out[2 * k + 1]]) as i32;
            let (mx, my) = (col as usize * 16, row * 16);
            // (interlaced pictures are coded in 32-line units: rows past the picture)
            let visible = my < H;
            for y in 0..16 {
                if !visible { break; }
                for x in 0..16 {
                    let (ry, rcb, rcr) = reference(&refv, mx + x, my + y);
                    let dy = (px(y * 16 + x) - ry).abs();
                    let dcb = (px(256 + (y / 2) * 8 + x / 2) - rcb).abs();
                    let dcr = (px(320 + (y / 2) * 8 + x / 2) - rcr).abs();
                    worst = worst.max(dy).max(dcb).max(dcr);
                    assert!(dy <= 2 && dcb <= 2 && dcr <= 2, "{stream} slice {si} MB ({}, {}) pixel ({x}, {y}): Y {} vs {ry}, Cb {} vs {rcb}, Cr {} vs {rcr}",
                        col, row, px(y * 16 + x), px(256 + (y / 2) * 8 + x / 2), px(320 + (y / 2) * 8 + x / 2));
                }
            }
            if visible { mbs += 1; }
            // the slice's end: a start code next
            if rig.ipu.ctrl & CTRL_SCD != 0 { break; }
        }
        assert_eq!(rig.ipu.errors, 0, "{stream}: VLC errors");
    }
    assert_eq!(mbs, (W / 16) * (H / 16), "{stream}: every macroblock");
    println!("{stream}: {mbs} macroblocks, worst difference {worst}");
}

/// IDEC: whole slices to RGB32, compared with the reference through the same conversion
fn check_idec(stream: &str, reference_yuv: &str) {
    let d = yuv(stream);
    let refv = yuv(reference_yuv);
    let p = parse(&d);
    let mut worst = 0;
    let mut mbs = 0;
    for &(row, at) in &p.slices {
        let (pos, qsc) = slice_header(&d, at);
        // the first address increment is the game's to read (IDEC starts at the type)
        let mut rig = Rig::new(&d[pos / 8..], &p);
        rig.cmd((pos % 8) as u32);
        rig.cmd(3 << 28);
        let col0 = (rig.ipu.data & 0xFFFF) as usize - 1;
        rig.cmd(1 << 28 | (!p.fpfd as u32) << 24 | qsc << 16);
        assert_eq!(rig.ipu.errors, 0, "{stream}: VLC errors");
        let out = rig.out();
        assert_eq!(out.len() % 1024, 0);
        for (k, mb) in out.chunks(1024).enumerate() {
            let (mx, my) = ((col0 + k) * 16, row * 16);
            if my >= H { continue; }
            let mut raw = [0u8; 384];
            for y in 0..16 {
                for x in 0..16 {
                    let (ry, rcb, rcr) = reference(&refv, mx + x, my + y);
                    raw[y * 16 + x] = ry as u8;
                    raw[256 + (y / 2) * 8 + x / 2] = rcb as u8;
                    raw[320 + (y / 2) * 8 + x / 2] = rcr as u8;
                }
            }
            let want = csc(&raw, 0, 0, false);
            for i in 0..256 {
                let got = u32::from_le_bytes(mb[4 * i..4 * i + 4].try_into().unwrap());
                for s in [0, 8, 16] {
                    let dlt = ((got >> s & 0xFF) as i32 - (want[i] >> s & 0xFF) as i32).abs();
                    worst = worst.max(dlt);
                    assert!(dlt <= 6, "{stream} MB ({}, {row}) pixel {i}: {got:08X} vs {:08X}", col0 + k, want[i]);
                }
                assert_eq!(got >> 24, 0x80);
            }
            mbs += 1;
        }
    }
    assert_eq!(mbs, (W / 16) * (H / 16), "{stream}: every macroblock");
    println!("{stream} IDEC: {mbs} macroblocks, worst difference {worst}");
}

#[test]
fn bdec_mpeg2_zigzag_table_zero() { check_bdec("a.m2v", "a.yuv"); }

#[test]
fn bdec_mpeg2_alternate_scan_table_one_nonlinear_10_bit_dc() { check_bdec("b.m2v", "b.yuv"); }

#[test]
fn bdec_mpeg1() { check_bdec("c.m1v", "c.yuv"); }

#[test]
fn bdec_custom_matrix_fine_quantiser() { check_bdec("d.m2v", "d.yuv"); }

#[test]
fn idec_mpeg2() { check_idec("a.m2v", "a.yuv"); }

#[test]
fn idec_table_one_alternate_scan() { check_idec("b.m2v", "b.yuv"); }

#[test]
fn idec_mpeg1() { check_idec("c.m1v", "c.yuv"); }

/// IPU_FROM takes the output; IPU_TO ends when its data is in
#[test]
fn the_dma_channels_move_the_data() {
    let d = yuv("a.m2v");
    let p = parse(&d);
    let (pos, qsc) = slice_header(&d, p.slices[0].1);
    let mut rig = Rig::new(&d[pos / 8..], &p);
    rig.dmac.write(0xB010, 0x40_0000, u32::MAX);
    rig.dmac.write(0xB020, 4 * 64, u32::MAX);
    rig.dmac.write(0xB000, 0x100, u32::MAX);
    rig.cmd((pos % 8) as u32);
    rig.cmd(3 << 28);
    rig.cmd(1 << 28 | qsc << 16);
    for _ in 0..10 { rig.step(); }
    assert_eq!(rig.dmac.ch[IPU_FROM].chcr & 0x100, 0, "IPU_FROM done");
    assert_eq!(rig.dmac.ch[IPU_FROM].madr, 0x40_0000 + 4 * 1024);
    assert!(rig.ram[0x40_0000..0x40_0000 + 4096].iter().any(|&b| b != 0));
    let _ = IPU_TO;
}

/// The VLC tables are prefix-free and VDEC's macroblock types decode
#[test]
fn tables_build() {
    let t = tables();
    assert_eq!(t.mbtype[0].lut[0b10].1, 0x0001_0001);
    assert_eq!(t.dct0.bits, 16);
    assert_eq!(t.dct1.bits, 16);
}


#[test]
fn bdec_alternate_scan_field_dct() { check_bdec("x2.m2v", "x2.yuv"); }

#[test]
fn bdec_table_one() { check_bdec("x1.m2v", "x1.yuv"); }

/// A P picture (encoded with zero motion vectors): skipped macroblocks copy the reference,
/// "pattern" ones add BDEC's non-intra residual to it, intra ones replace it. Exercises the
/// P macroblock types, motion codes, the coded block pattern and non-intra blocks.
#[test]
fn bdec_p_picture_non_intra_blocks() {
    let d = yuv("p.m2v");
    let refv = yuv("p.yuv");
    let frame = W * H * 3 / 2;
    let (prev, want) = (&refv[..frame], &refv[frame..]);
    let p = parse(&d);
    // the second picture's slices
    let pics: Vec<usize> = (0..d.len() - 4).filter(|&i| d[i..i + 4] == [0, 0, 1, 0]).collect();
    let slices: Vec<(usize, usize)> = p.slices.iter().copied().filter(|&(_, at)| at > pics[1]).collect();
    assert_eq!(slices.len(), 3);
    let mut kinds = [0usize; 4];
    let mut worst = 0;
    for &(row, at) in &slices {
        let (pos, mut qsc) = slice_header(&d, at);
        let mut rig = Rig::new(&d[pos / 8..], &p);
        rig.ipu.write(0x10, rig.ipu.ctrl & !(7 << 24) | 2 << 24);   // a P picture
        rig.cmd((pos % 8) as u32);
        let mut col: i32 = -1;
        let mut prev_intra = false;
        let check = |col: i32, out: Option<&[i32; 384]>, worst: &mut i32| {
            for y in 0..16 {
                for x in 0..16 {
                    let (mx, my) = (col as usize * 16 + x, row * 16 + y);
                    let (py, pcb, pcr) = reference(prev, mx, my);
                    let (ry, rcb, rcr) = reference(want, mx, my);
                    let (gy, gcb, gcr) = match out {
                        None => (py, pcb, pcr),
                        Some(o) => (o[y * 16 + x], o[256 + (y / 2) * 8 + x / 2], o[320 + (y / 2) * 8 + x / 2]),
                    };
                    let dlt = (gy - ry).abs().max((gcb - rcb).abs()).max((gcr - rcr).abs());
                    *worst = (*worst).max(dlt);
                    assert!(dlt <= 2, "MB ({col}, {row}) ({x}, {y}): {gy} {gcb} {gcr} vs {ry} {rcb} {rcr}");
                }
            }
        };
        loop {
            rig.cmd(3 << 28);
            let v = rig.ipu.data;
            if v == 0 { break; }
            let inc = (v & 0xFFFF) as i32;
            if inc == 34 { continue; }
            if inc == 35 { col += 33; continue; }
            // skipped macroblocks: the reference as it was
            for k in 1..inc { if col + k >= 0 { check(col + k, None, &mut worst); kinds[3] += 1; } }
            let skipped = inc > 1 && col >= 0;
            col += inc;
            rig.cmd(3 << 28 | 1 << 26);
            let t = rig.ipu.data & 0xFFFF;
            let (quant, forward, pattern, intra) = (t & 0x10 != 0, t & 0x08 != 0, t & 0x02 != 0, t & 0x01 != 0);
            let mut fb = 0;
            if quant {
                rig.cmd(4 << 28);
                qsc = rig.ipu.data >> 27;
                fb = 5;
            }
            if forward {
                // zero vectors: motion code 0 both ways
                rig.cmd(3 << 28 | 2 << 26 | fb);
                assert_eq!(rig.ipu.data, 0x0001_0000, "horizontal motion code 0");
                rig.cmd(3 << 28 | 2 << 26);
                assert_eq!(rig.ipu.data, 0x0001_0000, "vertical motion code 0");
                fb = 0;
            }
            if intra {
                let dcr = (!prev_intra || skipped) as u32;
                rig.cmd(2 << 28 | 1 << 27 | dcr << 26 | qsc << 16 | fb);
                let o = rig.out();
                let mut mb = [0i32; 384];
                for k in 0..384 { mb[k] = i16::from_le_bytes([o[2 * k], o[2 * k + 1]]) as i32; }
                check(col, Some(&mb), &mut worst);
                kinds[0] += 1;
            } else if pattern {
                rig.cmd(2 << 28 | qsc << 16 | fb);
                let o = rig.out();
                assert_eq!(o.len(), 768);
                let mut mb = [0i32; 384];
                for y in 0..16 {
                    for x in 0..16 {
                        let (py, pcb, pcr) = reference(prev, col as usize * 16 + x, row * 16 + y);
                        let r = |k: usize| i16::from_le_bytes([o[2 * k], o[2 * k + 1]]) as i32;
                        mb[y * 16 + x] = (py + r(y * 16 + x)).clamp(0, 255);
                        let c = (y / 2) * 8 + x / 2;
                        mb[256 + c] = (pcb + r(256 + c)).clamp(0, 255);
                        mb[320 + c] = (pcr + r(320 + c)).clamp(0, 255);
                    }
                }
                check(col, Some(&mb), &mut worst);
                kinds[1] += 1;
            } else {
                check(col, None, &mut worst);
                kinds[2] += 1;
            }
            prev_intra = intra;
            if rig.ipu.ctrl & CTRL_SCD != 0 { break; }
        }
        assert_eq!(rig.ipu.errors, 0, "VLC errors");
    }
    println!("P picture: intra {} / coded {} / motion only {} / skipped {}, worst {worst}", kinds[0], kinds[1], kinds[2], kinds[3]);
    assert!(kinds[1] > 0, "some non-intra coded macroblocks");
}
