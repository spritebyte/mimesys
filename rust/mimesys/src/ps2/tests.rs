use super::dmac::{self, Dmac};
use super::hw::{Hw, Sif};
use super::iop_dma::{self, IopDma};
use super::*;

/// Boot a BIOS (PS2_BIOS=path, PS2_FRAMES=n, default 120) with the debug log on, and
/// print where both CPUs ended up. `cargo test --release --lib ps2::tests::boot_bios -- --ignored --nocapture`
#[test]
#[ignore]
fn boot_bios() {
    let path = std::env::var("PS2_BIOS").expect("PS2_BIOS=path to a 4 MB BIOS");
    let frames: u64 = std::env::var("PS2_FRAMES").ok().and_then(|v| v.parse().ok()).unwrap_or(120);
    let bios = std::fs::read(&path).unwrap();
    let mut s = Ps2System::from_rom(&bios, &path, &[]).unwrap();
    s.set_option("debug_report", 1);
    let t = std::time::Instant::now();
    for f in 0..frames {
        s.run_frame(0);
        if f % 60 == 59 { s.report(); }
    }
    let secs = t.elapsed().as_secs_f64();
    s.report();
    let (w, h) = s.size;
    println!("GS: {} primitives, {} pixels; display {w}x{h}, PMODE {:X} DISPFB1 {:X} DISPLAY1 {:X} DISPFB2 {:X} DISPLAY2 {:X}",
        s.hw.gs.prims, s.hw.gs.pixels, s.hw.gs.pregs[0], s.hw.gs.pregs[7], s.hw.gs.pregs[8], s.hw.gs.pregs[9], s.hw.gs.pregs[10]);
    // PS2_PPM=path: the last frame as a picture
    if let Ok(out) = std::env::var("PS2_PPM") {
        let mut ppm = format!("P6 {w} {h} 255\n").into_bytes();
        for p in s.frame.chunks(4).take(w * h) { ppm.extend_from_slice(&p[..3]); }
        std::fs::write(out, ppm).unwrap();
    }
    println!("{frames} frames in {secs:.1} s ({:.1} fps, {:.0}% of full speed)", frames as f64 / secs, 100.0 * frames as f64 / secs / 59.94);
}

fn qw(words: [u32; 4]) -> u128 { words.iter().rev().fold(0u128, |a, &w| a << 32 | w as u128) }

fn put(ram: &mut [u8], a: usize, q: u128) { ram[a..a + 16].copy_from_slice(&q.to_le_bytes()); }

/// A source chain with cnt / ref / end tags into SIF1, and the IOP's SIF1 channel
/// unpacking it by the IOP tag that rides in each DMAtag's upper half (TTE)
#[test]
fn sif1_carries_a_source_chain_to_iop_memory() {
    let mut ram = vec![0u8; 0x10000];
    let mut spr = vec![0u8; 0x4000];
    // tag 1 at 0x1000: cnt, 1 qword; IOP tag: dest 0x400, 4 words
    put(&mut ram, 0x1000, qw([0x1000_0001, 0, 0x400, 4]));
    put(&mut ram, 0x1010, qw([0x11, 0x22, 0x33, 0x44]));
    // tag 2 at 0x1020: ref (data at 0x2000), 1 qword; IOP tag: dest 0x800, end
    put(&mut ram, 0x1020, qw([0x3000_0001, 0x2000, 0x8000_0800, 4]));
    put(&mut ram, 0x2000, qw([0x55, 0x66, 0x77, 0x88]));
    // tag 3 at 0x1030: end, nothing
    put(&mut ram, 0x1030, qw([0x7000_0000, 0, 0, 0]));
    let mut d = Dmac::default();
    let mut sif = Sif::default();
    d.write(0xE000, 1, u32::MAX);
    d.write(0xC430, 0x1000, u32::MAX);
    d.write(0xC400, 0x1C5, u32::MAX);           // chain, TTE, STR
    d.step(&mut ram, &mut spr, &mut sif);
    assert_eq!(d.ch[dmac::SIF1].chcr & 0x100, 0, "finished at the end tag");
    assert_ne!(d.stat & (1 << dmac::SIF1), 0);
    assert_eq!(sif.fifo1.len(), 3 * 4 + 2 * 4, "three quadword tags and two quadwords of data");
    // the IOP side
    let mut iop_ram = vec![0u8; 0x1000];
    let mut io = IopDma::default();
    io.write(0x1F80_1578, 1, u32::MAX);
    io.write(0x1F80_1570, 0x8 << 12, u32::MAX);  // channel 10 on
    io.write(0x1F80_1574, 1 << 19, u32::MAX);    // its interrupt on
    io.write(0x1F80_10F4, 1 << 23, u32::MAX);    // master
    io.write(0x1F80_1538, 0x0100_0000 | 0x200, u32::MAX);
    let irq = io.step(&mut iop_ram, &mut sif, &mut Default::default());
    let w = |a: usize| u32::from_le_bytes(iop_ram[a..a + 4].try_into().unwrap());
    assert_eq!([w(0x400), w(0x40C), w(0x800), w(0x80C)], [0x11, 0x44, 0x55, 0x88]);
    assert!(irq, "DICR master flag -> I_STAT 3");
    assert_eq!(io.ch[iop_dma::SIF1].chcr >> 24 & 1, 0);
}

/// SIF0 the other way: the IOP's tags carry the EE DMAtag; the EE side is a dest chain
#[test]
fn sif0_carries_iop_memory_to_the_ee() {
    let mut iop_ram = vec![0u8; 0x1000];
    let w32 = |m: &mut [u8], a: usize, v: u32| m[a..a + 4].copy_from_slice(&v.to_le_bytes());
    // IOP tag at 0x100: data at 0x200, 4 words, end; EE tag: end, 1 qword to 0x3000
    for (k, v) in [0x8000_0200u32, 4, 0x7000_0001, 0x3000].into_iter().enumerate() { w32(&mut iop_ram, 0x100 + 4 * k, v); }
    for k in 0..4 { w32(&mut iop_ram, 0x200 + 4 * k, 0xA0 + k as u32); }
    let mut sif = Sif::default();
    let mut io = IopDma::default();
    io.write(0x1F80_1578, 1, u32::MAX);
    io.write(0x1F80_1570, 0x8 << 8, u32::MAX);
    io.write(0x1F80_152C, 0x100, u32::MAX);
    io.write(0x1F80_1528, 0x0100_0000 | 0x100 | 0x200, u32::MAX);   // start, tags with the EE tag
    io.step(&mut iop_ram, &mut sif, &mut Default::default());
    assert_eq!(sif.fifo0.len(), 8, "a quadword tag and a quadword of data");
    let mut ram = vec![0u8; 0x10000];
    let mut spr = vec![0u8; 0x4000];
    let mut d = Dmac::default();
    d.write(0xE000, 1, u32::MAX);
    d.write(0xC000, 0x104, u32::MAX);           // chain, STR
    d.step(&mut ram, &mut spr, &mut sif);
    assert_eq!(u128::from_le_bytes(ram[0x3000..0x3010].try_into().unwrap()), qw([0xA0, 0xA1, 0xA2, 0xA3]));
    assert_eq!(d.ch[dmac::SIF0].chcr & 0x100, 0, "the EE tag said end");
}

#[test]
fn dmac_status_bits_clear_and_mask_bits_toggle() {
    let mut d = Dmac::default();
    d.stat = 0x3;
    d.write(0xE010, 0x0001_0001, u32::MAX);
    assert_eq!(d.stat, 0x0001_0002, "bit 0 cleared, mask 0 set");
    assert!(!d.int1());
    d.write(0xE010, 0x0002_0000, u32::MAX);
    assert!(d.int1(), "channel 1 status and mask");
    d.pcr = 0x2;
    assert!(d.cpcond0(), "the one channel picked has finished");
    d.pcr = 0x6;
    assert!(!d.cpcond0());
}

#[test]
fn intc_stat_clears_on_one_and_the_mask_toggles() {
    let mut h = Hw::new(vec![0; hw::BIOS_SIZE]);
    h.raise_intc(hw::INT_VBON | hw::INT_VBOF);
    h.ee_io_write(0x1000_F010, hw::INT_VBON, u32::MAX);
    assert!(h.int0());
    h.ee_io_write(0x1000_F000, hw::INT_VBON, u32::MAX);
    assert!(!h.int0(), "VBON acknowledged");
    assert_eq!(h.intc_stat, hw::INT_VBOF);
    h.ee_io_write(0x1000_F010, hw::INT_VBON, u32::MAX);
    assert_eq!(h.intc_mask, 0, "toggled back off");
}

#[test]
fn the_rdram_handshake_counts_two_devices() {
    let mut h = Hw::new(vec![0; hw::BIOS_SIZE]);
    h.ee_io_write(0x1000_F440, 0, u32::MAX);
    h.ee_io_write(0x1000_F430, 0x21 << 16 | 1 << 6, u32::MAX);     // INIT
    h.ee_io_write(0x1000_F430, 0x21 << 16, u32::MAX);               // read, SOP 0
    let r: Vec<u32> = (0..3).map(|_| h.ee_io_read(0x1000_F440)).collect();
    assert_eq!(r, [0x1F, 0x1F, 0], "two RDRAM devices answer");
}

#[test]
fn ee_timer_compare_interrupt_is_edge_triggered() {
    let mut t = timers::EeTimer::default();
    t.write(0x20, 100, );
    t.write(0x10, 0x80 | 0x100);                 // bus clock, enabled, compare IRQ
    assert!(!t.tick(50, 0));
    assert!(t.tick(60, 0), "COUNT passed 100");
    assert!(!t.tick(0x10000, 0), "flag still set: no new edge");
    t.write(0x10, 0x80 | 0x100 | 0x400);         // clear the flag
    assert_eq!(t.mode & 0x400, 0);
}

#[test]
fn iop_timer_target_interrupt_and_one_shot() {
    let mut t = timers::IopTimer::default();
    t.write(0x8, 1000);
    t.write(0x4, 0x10 | 0x08);                   // compare IRQ, zero return, one-shot
    assert!(!t.tick(3, 900, 0));
    assert!(t.tick(3, 200, 0), "reached 1000");
    assert_eq!(t.count, 100, "zero return");
    assert!(!t.tick(3, 1000, 0), "one-shot: bit 10 dropped");
    assert_ne!(t.read(0x4) & (1 << 11), 0);
    assert_eq!(t.read(0x4) & (1 << 11), 0, "reading mode clears the flags");
}

#[test]
fn cdvd_s_commands_answer_version_and_clock() {
    let mut c = cdvd::Cdvd::default();
    let mut i = 0u32;
    c.write(0x1F40_2017, 0x00, &mut i);
    c.write(0x1F40_2016, 0x03, &mut i);
    let v: Vec<u8> = (0..4).map(|_| c.read(0x1F40_2018)).collect();
    assert_eq!(v, [3, 6, 2, 0]);
    assert_eq!(c.read(0x1F40_2017) & 0x40, 0x40, "no more data");
    c.write(0x1F40_2016, 0x08, &mut i);
    let rtc: Vec<u8> = (0..8).map(|_| c.read(0x1F40_2018)).collect();
    assert!(rtc[1] < 0x60 && rtc[6] >= 1 && rtc[6] <= 0x12, "BCD seconds and month: {rtc:02X?}");
    c.write(0x1F40_2004, 0x00, &mut i);
    assert_ne!(i & hw::IOP_CDVD, 0, "an N command interrupts the IOP");
}

#[test]
fn the_bios_test_needs_4_mb_and_a_name_or_romdir() {
    let mut d = vec![0u8; hw::BIOS_SIZE];
    assert!(is_bios(&d, "SCPH-39001.bin"));
    assert!(is_bios(&d, "OpenPS2BIOS.bin"));
    assert!(!is_bios(&d, "game.bin"));
    d[0x2700..0x270A].copy_from_slice(b"RESET\0\0\0\0\0");
    assert!(is_bios(&d, "game.bin"));
    assert!(!is_bios(&d[..0x80000], "scph1001.bin"), "a PS1 BIOS isn't one");
}

/// The Sony BIOS starts a memory-controller command at 1000_F410 and waits for bit 31
#[test]
fn the_memory_controller_command_finishes_at_once() {
    let mut h = Hw::new(vec![0; hw::BIOS_SIZE]);
    h.ee_io_write(0x1000_F410, 0x8000_0000, u32::MAX);
    assert_eq!(h.ee_io_read(0x1000_F410) >> 31, 0);
}

/// "# Initialize Start." waits for a non-zero halfword at 1A00_0006
#[test]
fn the_1a000006_status_comes_round_to_non_zero() {
    use crate::common::cpu::r5900::EeBus;
    let mut h = Hw::new(vec![0; hw::BIOS_SIZE]);
    let mut v = hw::EeView(&mut h);
    let r: Vec<u16> = (0..4).map(|_| v.read16(0x1A00_0006)).collect();
    assert_eq!(r, [0, 1, 2, 0]);
}

/// A tiny ROMDIR: RESET, ROMDIR, ROMVER (European), OSDVER
fn bios_with_romdir() -> Vec<u8> {
    let mut b = vec![0u8; hw::BIOS_SIZE];
    let reset_code = 0x2700usize;
    let ent = |b: &mut Vec<u8>, at: usize, name: &[u8], size: u32| {
        b[at..at + name.len()].copy_from_slice(name);
        b[at + 12..at + 16].copy_from_slice(&size.to_le_bytes());
    };
    ent(&mut b, reset_code, b"RESET", reset_code as u32);
    ent(&mut b, reset_code + 16, b"ROMDIR", 0x50);
    ent(&mut b, reset_code + 32, b"ROMVER", 0x10);
    ent(&mut b, reset_code + 48, b"OSDVER", 0x10);
    // files: RESET at 0 (0x2700 bytes), ROMDIR at 0x2700 (0x50), ROMVER at 0x2750, OSDVER at 0x2760
    b[0x2750..0x275E].copy_from_slice(b"0170EC20040614");
    b[0x2760..0x2768].copy_from_slice(b"0170Eeng");
    b
}

#[test]
fn region_params_come_from_romver_and_osdver() {
    let b = bios_with_romdir();
    assert_eq!(cdvd::romdir_file(&b, "ROMVER").map(|r| &r[..4]), Some(&b"0170"[..]));
    let mut c = cdvd::Cdvd::with_bios(&b);
    let mut i = 0u32;
    c.write(0x1F40_2016, 0x36, &mut i);
    let r: Vec<u8> = (0..15).map(|_| c.read(0x1F40_2018)).collect();
    assert_eq!(&r[..8], &[0, 1 << 2, 0, b'E', b'E', b'e', b'n', b'g'], "Europe = zone 2");
    assert_eq!(c.read(0x1F40_2017) & 0x40, 0x40, "15 bytes, then empty");
}

#[test]
fn s_commands_answer_their_full_length() {
    let mut c = cdvd::Cdvd::default();
    let mut i = 0u32;
    c.write(0x1F40_2016, 0x17, &mut i);              // ReadModelNumber: 9 bytes
    let mut n = 0;
    while c.read(0x1F40_2017) & 0x40 == 0 { c.read(0x1F40_2018); n += 1; }
    assert_eq!(n, 9);
}

/// One SIO2 command on `port`: its reply bytes and RECV1
fn sio2_cmd(h: &mut Hw, port: u32, bytes: &[u8]) -> (Vec<u8>, u32) {
    use crate::common::cpu::r3000::R3000Bus;
    let mut v = hw::IopView(h);
    v.write32(0x1F80_8268, 0x3BC | 0xC);                  // reset
    v.write32(0x1F80_8200, port | (bytes.len() as u32) << 8 | ((bytes.len() as u32) & 0x7F) << 18);
    v.write32(0x1F80_8204, 0);
    for &b in bytes { v.write32(0x1F80_8260, b as u32); }
    v.write32(0x1F80_8268, 0x3BC | 1);                    // go
    assert!(h.sio2.run(), "an interrupt");
    let mut v = hw::IopView(h);
    let recv1 = v.read32(0x1F80_826C);
    ((0..bytes.len()).map(|_| v.read32(0x1F80_8264) as u8).collect(), recv1)
}

/// No memory card: FFh bytes, "disconnected"
#[test]
fn sio2_memory_card_port_reports_no_device() {
    let mut h = Hw::new(vec![0; hw::BIOS_SIZE]);
    let (r, recv1) = sio2_cmd(&mut h, 2, &[0x81, 0x11, 0, 0, 0]);
    assert_eq!(recv1, 0x1D100);
    assert_eq!(r, [0xFF; 5]);
}

/// The DualShock 2: digital at power-up; the BIOS's config sequence turns on analog and
/// pressure (ID 79h)
#[test]
fn sio2_dualshock2_digital_then_pressure() {
    let mut h = Hw::new(vec![0; hw::BIOS_SIZE]);
    h.sio2.pads[0].buttons = !(1 << 14);                  // Cross
    h.sio2.pads[0].pressures[6] = 0xFF;
    let (r, recv1) = sio2_cmd(&mut h, 0, &[0x01, 0x42, 0, 0, 0]);
    assert_eq!(recv1 & 0x2000, 0, "connected");
    assert_eq!(r, [0xFF, 0x41, 0x5A, 0xFF, 0xBF]);
    sio2_cmd(&mut h, 0, &[0x01, 0x43, 0, 1, 0]);                        // enter config
    let (r, _) = sio2_cmd(&mut h, 0, &[0x01, 0x45, 0, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A]);
    assert_eq!(r[1], 0xF3);
    assert_eq!(r[3], 0x03, "DualShock 2");
    sio2_cmd(&mut h, 0, &[0x01, 0x44, 0, 1, 3, 0, 0, 0, 0]);            // analog, locked
    sio2_cmd(&mut h, 0, &[0x01, 0x4F, 0, 0xFF, 0xFF, 0x03, 0, 0, 0]);  // all 18 bytes
    sio2_cmd(&mut h, 0, &[0x01, 0x43, 0, 0, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A]); // exit config
    let mut cmd = vec![0x01, 0x42, 0];
    cmd.extend([0; 18]);
    let (r, _) = sio2_cmd(&mut h, 0, &cmd);
    assert_eq!(&r[1..5], &[0x79, 0x5A, 0xFF, 0xBF]);
    assert_eq!(&r[5..9], &[0x80; 4]);
    assert_eq!(r[9 + 6], 0xFF, "Cross pressure");
    assert_eq!(r[9], 0);
}

/// libsd polls each core's STATX for bit 7 after an SPU2 DMA; ours finishes at once
#[test]
fn spu2_statx_reports_dma_ready() {
    let mut h = Hw::new(vec![0; hw::BIOS_SIZE]);
    let mut v = hw::IopView(&mut h);
    use crate::common::cpu::r3000::R3000Bus;
    assert_eq!(v.read16(0x1F90_0344) & 0x80, 0x80, "core 0");
    assert_eq!(v.read16(0x1F90_0744) & 0x80, 0x80, "core 1");
    v.write16(0x1F90_0744, 0);
    assert_eq!(v.read16(0x1F90_0744) & 0x80, 0x80, "read-only");
    v.write16(0x1F90_019A, 0x1234);
    assert_eq!(v.read16(0x1F90_019A), 0x1234, "other regs keep what was written");
    v.read32(0x1F90_07FE);                                 // last halfword: no overflow
}

// ====================================================================== VIF / VU (stage 4)

fn fl(x: f32) -> u32 { x.to_bits() }
fn up(f: u32, dest: u32, ft: u32, fs: u32, fd: u32) -> u32 { dest << 21 | ft << 16 | fs << 11 | fd << 6 | f }
const UNOP: u32 = 0x2FF;
const LNOP: u32 = 0x8000_033C;
const EBIT: u32 = 1 << 30;
fn lo(op: u32, rest: u32) -> u32 { op << 25 | rest }
fn special(n: u32, dest: u32, ft: u32, fs: u32) -> u32 { 0x40 << 25 | dest << 21 | ft << 16 | fs << 11 | (n >> 2) << 6 | 0x3C | (n & 3) }
fn load_prog(code: &mut [u8], pairs: &[(u32, u32)]) {
    for (k, &(u, l)) in pairs.iter().enumerate() {
        code[k * 8..k * 8 + 4].copy_from_slice(&l.to_le_bytes());
        code[k * 8 + 4..k * 8 + 8].copy_from_slice(&u.to_le_bytes());
    }
}

/// A loop: LQ, ADD in a loop with IBNE (delay slot), SQ, E bit
#[test]
fn vu_runs_a_loop_with_branch_delay_slots() {
    let mut vu = super::vu::Vu::new(true);
    let (mut code, mut data) = (vec![0u8; 0x4000], vec![0u8; 0x4000]);
    load_prog(&mut code, &[
        (UNOP, lo(0x08, 1 << 16 | 3)),                           // IADDIU vi1, vi0, 3
        (UNOP, lo(0x00, 0xF << 21 | 1 << 16)),                   // LQ.xyzw vf1, 0(vi0)
        (up(0x28, 0xF, 1, 2, 2), lo(0x09, 1 << 16 | 1 << 11 | 1)), // ADD vf2, vf2, vf1 | ISUBIU vi1, vi1, 1
        (UNOP, lo(0x29, 1 << 11 | 0x7FE)),                       // IBNE vi1, vi0, -2  (wait: it=0, is=1)
        (UNOP, LNOP),                                            // delay slot
        (UNOP, lo(0x01, 0xF << 21 | 2 << 11 | 1)),               // SQ.xyzw vf2, 1(vi0)
        (UNOP | EBIT, LNOP),
        (UNOP, LNOP),
        (UNOP, lo(0x08, 9 << 16 | 1)),                           // must not run
    ]);
    data[0..16].copy_from_slice(&qw([fl(1.0), fl(2.0), fl(3.0), fl(4.0)]).to_le_bytes());
    let mut p1 = Vec::new();
    vu.start(0);
    vu.run(&code, &mut data, &mut p1);
    let out = u128::from_le_bytes(data[16..32].try_into().unwrap());
    assert_eq!(out, qw([fl(3.0), fl(6.0), fl(9.0), fl(12.0)]));
    assert_eq!(vu.vi[1], 0);
    assert_eq!(vu.vi[9], 0, "stopped after the E bit's delay slot");
    assert_eq!(vu.tpc, 8 * 8);
    assert_eq!(vu.illegal, 0);
}

/// MAC flags arrive 4 cycles after the FMAC; Q waits for WAITQ; I-bit immediates; XGKICK
#[test]
fn vu_flags_q_and_xgkick() {
    let mut vu = super::vu::Vu::new(true);
    let (mut code, mut data) = (vec![0u8; 0x4000], vec![0u8; 0x4000]);
    vu.vf[1] = [fl(1.0), fl(2.0), fl(3.0), fl(4.0)];
    vu.vi[3] = 0x7FFF;
    vu.vi[5] = 10;
    load_prog(&mut code, &[
        (up(0x2C, 0xF, 1, 1, 3), special(0x38, 0, 1, 1) | 1 << 23),   // SUB vf3, vf1, vf1 | DIV Q, vf1x, vf1y
        (UNOP, lo(0x1A, 2 << 16 | 3 << 11)),                     // FMAND vi2, vi3 (too soon)
        (UNOP, LNOP), (UNOP, LNOP), (UNOP, LNOP),
        (UNOP, lo(0x1A, 4 << 16 | 3 << 11)),                     // FMAND vi4, vi3
        (UNOP, special(0x3B, 0, 0, 0)),                          // WAITQ
        (up(0x1C, 0xF, 0, 1, 4), special(0x6C, 0, 0, 5)),        // MULq vf4, vf1, Q | XGKICK vi5
        (up(0x1E, 0x8, 0, 1, 6) | 1 << 31, fl(10.0)),            // MULi.x vf6, vf1, I (old I) / I = 10
        (up(0x1E, 0x8, 0, 1, 7) | EBIT, LNOP),                   // MULi.x vf7, vf1, I = 10
        (UNOP, LNOP),
    ]);
    // a GIF packet at qword 10: PACKED, 1 loop, 1 register, EOP
    let tag: u128 = 1 | 1 << 15 | 1u128 << 60;
    data[160..176].copy_from_slice(&tag.to_le_bytes());
    data[176..192].copy_from_slice(&0xABCDu128.to_le_bytes());
    let mut p1 = Vec::new();
    vu.start(0);
    vu.run(&code, &mut data, &mut p1);
    assert_eq!(vu.vi[2], 0, "flags not there yet");
    assert_eq!(vu.vi[4], 0xF, "Z on x, y, z, w");
    assert_eq!(vu.vf[4], [fl(0.5), fl(1.0), fl(1.5), fl(2.0)]);
    assert_eq!(vu.vf[6][0], 0);
    assert_eq!(vu.vf[7][0], fl(10.0));
    assert_eq!(p1, [tag, 0xABCD]);
}

/// COP2 macro mode: QMTC2 / VADD / QMFC2, CFC2 VPU_STAT, VF00, VCALLMS
#[test]
fn cop2_macro_mode_and_vcallms() {
    use crate::common::cpu::r5900::EeBus;
    let mut h = Hw::new(vec![0; hw::BIOS_SIZE]);
    load_prog(&mut h.vu0_code[0x40..], &[
        (up(0x28, 0xF, 1, 1, 5), LNOP),                          // ADD vf5, vf1, vf1
        (UNOP | EBIT, LNOP), (UNOP, LNOP),
    ]);
    let mut v = hw::EeView(&mut h);
    let mut g = [0u128; 32];
    let mv = |rs: u32, rt: u32, rd: u32| 0x4800_0000 | rs << 21 | rt << 16 | rd << 11;
    g[4] = qw([fl(1.0), fl(2.0), fl(3.0), fl(4.0)]);
    g[6] = qw([fl(0.5); 4]);
    v.cop2_op(mv(5, 4, 1), &mut g);                              // QMTC2 $4, vf1
    v.cop2_op(mv(5, 6, 2), &mut g);                              // QMTC2 $6, vf2
    v.cop2_op(0x4A00_0000 | up(0x28, 0xF, 2, 1, 3), &mut g);     // VADD vf3, vf1, vf2
    v.cop2_op(mv(1, 7, 3), &mut g);                              // QMFC2 $7, vf3
    assert_eq!(g[7], qw([fl(1.5), fl(2.5), fl(3.5), fl(4.5)]));
    g[8] = 0x780;
    v.cop2_op(mv(2, 8, 29), &mut g);                             // CFC2 $8, VPU_STAT
    assert_eq!(g[8], 0);
    v.cop2_op(mv(1, 9, 0), &mut g);
    assert_eq!(g[9], qw([0, 0, 0, fl(1.0)]), "VF00");
    v.cop2_op(0x4A00_0000 | (0x40 / 8) << 6 | 0x38, &mut g);     // VCALLMS 0x40
    v.cop2_op(mv(1, 10, 5), &mut g);
    assert_eq!(g[10], qw([fl(2.0), fl(4.0), fl(6.0), fl(8.0)]));
}

/// UNPACK formats, the mask with ROW, write-skipping, MPG and DIRECT
#[test]
fn vif_unpack_formats_mask_and_skip() {
    let mut vu = super::vu::Vu::new(true);
    let (mut code, mut data, mut gs) = (vec![0u8; 0x4000], vec![0u8; 0x4000], Vec::new());
    let mut vif = super::vif::Vif::new(true);
    let ws: &[u32] = &[
        0x0100_0101,                                    // STCYCL cl 1 wl 1
        0x6E01_0002, 0x7F80_01FF,                       // UNPACK V4-8 x1 at 2
        0x6002_0004, 5, 6,                              // UNPACK S-32 x2 at 4
        0x3000_0000, 10, 20, 30, 40,                    // STROW
        0x2000_0000, 0xC4,                              // STMASK: y = row, w = keep
        0x7C01_0006, 1, 2, 3, 4,                        // UNPACK V4-32 masked at 6
        0x0100_0102,                                    // STCYCL cl 2 wl 1
        0x6002_0008, 7, 8,                              // S-32 x2 at 8, skipping a row
        0x4A01_0002, 0x1111_1111, 0x2222_2222,          // MPG 1 dword at 16
        0x5000_0001, 0xA, 0xB, 0xC, 0xD,                // DIRECT 1 qw
    ];
    {
        let mut cx = super::vif::VifCtx { code: &mut code, data: &mut data, vu: &mut vu, gs: &mut gs };
        for &w in ws { vif.word(w, &mut cx); }
    }
    let row = |r: usize| u128::from_le_bytes(data[r * 16..r * 16 + 16].try_into().unwrap());
    assert_eq!(row(2), qw([0xFFFF_FFFF, 1, 0xFFFF_FF80, 0x7F]));
    assert_eq!(row(4), qw([5; 4]));
    assert_eq!(row(5), qw([6; 4]));
    assert_eq!(row(6), qw([1, 20, 3, 0]));
    assert_eq!(row(8), qw([7; 4]));
    assert_eq!(row(9), 0);
    assert_eq!(row(10), qw([8; 4]));
    assert_eq!(&code[16..24], &[0x11, 0x11, 0x11, 0x11, 0x22, 0x22, 0x22, 0x22]);
    assert_eq!(gs, [qw([0xA, 0xB, 0xC, 0xD])]);
}

/// MSCAL runs VU1, flips the double buffer, and the program's XGKICK reaches the GS list
#[test]
fn vif1_mscal_runs_vu1_with_tops() {
    let mut vu = super::vu::Vu::new(true);
    let (mut code, mut data, mut gs) = (vec![0u8; 0x4000], vec![0u8; 0x4000], Vec::new());
    load_prog(&mut code, &[
        (UNOP, special(0x68, 0, 1, 0)),                  // XTOP vi1
        (UNOP | EBIT, LNOP), (UNOP, LNOP),
    ]);
    let mut vif = super::vif::Vif::new(true);
    {
        let mut cx = super::vif::VifCtx { code: &mut code, data: &mut data, vu: &mut vu, gs: &mut gs };
        for &w in &[0x0300_0010u32, 0x0200_0020, 0x1400_0000] { vif.word(w, &mut cx); }  // BASE 16, OFFSET 32, MSCAL 0
        assert_eq!(cx.vu.vi[1], 16);
        vif.word(0x1400_0000, &mut cx);
        assert_eq!(cx.vu.vi[1], 48, "second buffer");
    }
    assert_eq!(vif.mscal, 2);
}

/// The idle loops from the Sony BIOS logs are recognised; a polling loop isn't
#[test]
fn idle_loops_are_recognised() {
    use std::collections::HashMap;
    let mem = |words: &[(u32, u32)]| -> HashMap<u32, u32> { words.iter().copied().collect() };
    // EE kernel idle thread: 81FC0-81FD4 NOPs, 81FD8 BEQ $0,$0,-7, 81FDC NOP
    let mut ee: Vec<(u32, u32)> = (0..8).map(|k| (0x81FC0 + 4 * k, 0)).collect();
    ee[6].1 = 0x1000_FFF9;
    ee.push((0x81FE0, 0x1234_5678));
    let m = mem(&ee);
    let rd = |a: u32| m.get(&a).copied();
    for pc in [0x81FC0, 0x81FCC, 0x81FD8] { assert!(super::idle_loop(rd, pc), "{pc:X}"); }
    // IOP: AE94 J AE94, AE98 NOP
    let m = mem(&[(0xAE94, 0x0800_2BA5), (0xAE98, 0)]);
    assert!(super::idle_loop(|a| m.get(&a).copied(), 0xAE94));
    // OpenPS2BIOS's vsync wait: a load in the loop
    let m = mem(&[(0x1008AC, 0x2463_FFFF), (0x1008B0, 0x1060_0005), (0x1008B4, 0), (0x1008B8, 0x8C41_0000),
                  (0x1008BC, 0x3021_0008), (0x1008C0, 0x1020_FFFA), (0x1008C4, 0)]);
    assert!(!super::idle_loop(|a| m.get(&a).copied(), 0x1008AC));
}

// ====================================================================== memory cards

/// Through SIO2: probe, specs, a sector written and read back, erase
#[test]
fn memory_card_probe_specs_write_read_erase() {
    let mut h = Hw::new(vec![0; hw::BIOS_SIZE]);
    h.sio2.cards[0] = Some(super::memcard::MemCard::new(&[]));
    let (r, recv1) = sio2_cmd(&mut h, 0, &[0x81, 0x11, 0, 0]);
    assert_eq!(recv1, 0x1100, "card connected");
    assert_eq!(r, [0, 0, 0x2B, 0x55]);
    let (r, _) = sio2_cmd(&mut h, 0, &[0x81, 0x26, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(&r[2..12], &[0x2B, 0x00, 0x02, 0x10, 0x00, 0x00, 0x40, 0x00, 0x00, 0x52]);
    // write 8 bytes at sector 3
    sio2_cmd(&mut h, 0, &[0x81, 0x22, 3, 0, 0, 0, 3, 0, 0]);
    let mut w = vec![0x81, 0x42, 8];
    w.extend(1..=8u8);
    w.extend(std::iter::repeat(0).take(128 - 8 + 3));
    let (r, _) = sio2_cmd(&mut h, 0, &w);
    assert_eq!(&r[2..4], &[0x00, 0x2B]);
    assert_eq!(r[12], (1..=8u8).fold(0, |a, b| a ^ b), "XOR of what was written");
    assert!(h.sio2.cards[0].as_ref().unwrap().dirty);
    // read it back
    sio2_cmd(&mut h, 0, &[0x81, 0x23, 3, 0, 0, 0, 3, 0, 0]);
    let mut rd = vec![0x81, 0x43, 8];
    rd.extend(std::iter::repeat(0).take(131));
    let (r, _) = sio2_cmd(&mut h, 0, &rd);
    assert_eq!(&r[4..12], &[1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(r[13], 0x55);
    let at = 3 * super::memcard::RAW_SECTOR;
    assert_eq!(h.sio2.cards[0].as_ref().unwrap().data[at], 1);
    // erase the block holding sector 0..15
    sio2_cmd(&mut h, 0, &[0x81, 0x21, 0, 0, 0, 0, 0, 0, 0]);
    sio2_cmd(&mut h, 0, &[0x81, 0x82, 0, 0]);
    assert_eq!(h.sio2.cards[0].as_ref().unwrap().data[at], 0xFF);
    // slot 2 is empty
    let (_, recv1) = sio2_cmd(&mut h, 1, &[0x81, 0x11, 0, 0]);
    assert_eq!(recv1, 0x1D100);
}

/// MagicGate: the 8 bytes the IOP sends come back XORed; the terminator can change
#[test]
fn memory_card_auth_and_terminator() {
    let mut c = super::memcard::MemCard::new(&[]);
    let mut cmd = vec![0x81, 0xF0, 0x01];
    cmd.extend([1, 2, 4, 8, 16, 32, 64, 128]);
    cmd.extend([0, 0, 0]);
    let r = c.command(&cmd);
    assert_eq!(r.len(), cmd.len());
    assert_eq!(&r[2..4], &[0x00, 0x2B]);
    assert_eq!((r[12], r[13]), (0xFF, 0x55), "XOR, terminator");
    let r = c.command(&[0x81, 0x27, 0x5A, 0, 0]);
    assert_eq!(&r[2..5], &[0x00, 0x2B, 0x5A]);
    assert_eq!(c.command(&[0x81, 0x11, 0, 0]), [0, 0, 0x2B, 0x5A]);
    assert_eq!(super::memcard::summary(&c.data).unwrap(), ["unformatted"]);
}

/// MCMAN's page read as it sends it: one SIO2 transfer by DMA, every command in a 90h-byte
/// block, replies padded to 90h (offsets as ps2sdk's mcman_readpage checks them)
#[test]
fn memory_card_page_read_in_dma_blocks() {
    let mut s = super::sio2::Sio2::default();
    let mut card = super::memcard::MemCard::new(&[]);
    card.term = 0x5A;
    for (k, b) in card.data[..512].iter_mut().enumerate() { *b = k as u8; }
    s.cards[0] = Some(card);
    let cmds: Vec<Vec<u8>> = vec![
        vec![0x81, 0x23, 0, 0, 0, 0, 0, 0, 0],                       // start read, sector 0
        [vec![0x81, 0x43, 0x80], vec![0; 131]].concat(),             // 4 x read 128
        [vec![0x81, 0x43, 0x80], vec![0; 131]].concat(),
        [vec![0x81, 0x43, 0x80], vec![0; 131]].concat(),
        [vec![0x81, 0x43, 0x80], vec![0; 131]].concat(),
        [vec![0x81, 0x43, 0x10], vec![0; 19]].concat(),              // read the 16 ECC bytes
        vec![0x81, 0x81, 0, 0],                                      // end
    ];
    for (k, c) in cmds.iter().enumerate() {
        s.send3[k] = 2 | (c.len() as u32) << 8 | ((c.len() as u32) & 0x7F) << 18;   // port 2 = slot 1
        let mut block = c.clone();
        block.resize(0x90, 0);
        s.fifo_in.extend(block);
    }
    s.dma_block = 0x90;
    s.write(0x1F80_8268, 1);
    assert!(s.run());
    let p: Vec<u8> = s.fifo_out.iter().copied().collect();
    assert_eq!(p.len(), 7 * 0x90);
    assert_eq!(s.recv1 & 0xF000, 0x1000);
    assert_eq!(p[8], 0x5A, "start read's terminator");
    for k in 0..4 {
        let d = &p[0x94 + k * 0x90..0x94 + k * 0x90 + 128];
        assert_eq!(d[1], (k * 128 + 1) as u8);
        assert_eq!(d.iter().fold(0, |a, b| a ^ b), p[0x94 + 128 + k * 0x90], "EDC of chunk {k}");
    }
    assert_eq!(p[0x94 + 0x2CF], 0x5A, "end's terminator");
}

// ====================================================================== discs

/// A small ISO: PVD at 16, root directory at 20 with SYSTEM.CNF (sector 21); sector k
/// beyond holds the byte k & FF. `dvd`: make the PVD's two block-size copies differ.
fn make_iso(cnf: &str, sectors: u32, dvd: bool) -> std::path::PathBuf {
    let mut img = vec![0u8; sectors as usize * 2048];
    for k in 22..sectors as usize { img[k * 2048..(k + 1) * 2048].fill(k as u8); }
    let pvd = &mut img[16 * 2048..17 * 2048];
    pvd[0] = 1;
    pvd[1..6].copy_from_slice(b"CD001");
    pvd[8..19].copy_from_slice(b"PLAYSTATION");
    // root directory record at 156: extent 20, size 2048 (LE and BE)
    pvd[156] = 34;
    pvd[158..162].copy_from_slice(&20u32.to_le_bytes());
    pvd[166..170].copy_from_slice(&2048u32.to_le_bytes());
    pvd[170..174].copy_from_slice(&2048u32.to_be_bytes());
    if dvd { pvd[171] = 0x55; }
    let dir = &mut img[20 * 2048..21 * 2048];
    let name = b"SYSTEM.CNF;1";
    let len = 33 + name.len() + 1;
    dir[0] = len as u8;
    dir[2..6].copy_from_slice(&21u32.to_le_bytes());
    dir[10..14].copy_from_slice(&(cnf.len() as u32).to_le_bytes());
    dir[32] = name.len() as u8;
    dir[33..33 + name.len()].copy_from_slice(name);
    img[21 * 2048..21 * 2048 + cnf.len()].copy_from_slice(cnf.as_bytes());
    let dir = std::env::temp_dir().join(format!("mimesys_ps2_{}_{}.iso", std::process::id(), if dvd { "dvd" } else { "cd" }));
    std::fs::write(&dir, &img).unwrap();
    dir
}

#[test]
fn ps2_disc_detection_and_serial() {
    use crate::common::disc::Disc;
    let p = make_iso("BOOT2 = cdrom0:\\SLUS_200.62;1\r\nVER = 1.00\r\n", 64, true);
    let mut d = Disc::open(&p).unwrap();
    let i = super::iso::disc_info(&mut d).unwrap();
    assert!(i.ps2 && i.dvd);
    assert_eq!(i.serial, "SLUS-20062");
    assert_eq!(i.disc_type, super::iso::TYPE_PS2_DVD);
    let mut blob = crate::segacd::PATH_MAGIC.to_vec();
    blob.extend_from_slice(p.to_str().unwrap().as_bytes());
    assert!(super::iso::is_ps2_disc(&blob));
    let g = crate::common::rom_detect::detect(&blob, p.to_str().unwrap());
    assert_eq!(g[0].system, "ps2", "before PS1 (both say PLAYSTATION)");
    let p1 = make_iso("BOOT = cdrom:\\SLUS_005.94;1\r\n", 64, false);
    let mut d1 = Disc::open(&p1).unwrap();
    let i1 = super::iso::disc_info(&mut d1).unwrap();
    assert!(!i1.ps2 && !i1.dvd);
    let _ = (std::fs::remove_file(p), std::fs::remove_file(p1));
}

/// N 06 (CD read, 2048): two sectors into IOP DMA channel 3, then the interrupt; N 08
/// (DVD read) adds the 12-byte ID
#[test]
fn cdvd_reads_sectors_into_dma_channel_3() {
    use crate::common::disc::Disc;
    let p = make_iso("BOOT2 = cdrom0:\\SLUS_200.62;1\r\n", 64, false);
    let mut c = super::cdvd::Cdvd::default();
    c.insert(Disc::open(&p).unwrap());
    let mut dma = super::iop_dma::IopDma::default();
    let mut ram = vec![0u8; 0x20_0000];
    let mut i_stat = 0u32;
    let setup = |dma: &mut super::iop_dma::IopDma, words: u32, blocks: u32| {
        dma.write(0x1F80_10F0, 1 << 15, u32::MAX);
        dma.write(0x1F80_1578, 1, u32::MAX);
        dma.write(0x1F80_10B0, 0x1000, u32::MAX);
        dma.write(0x1F80_10B4, blocks << 16 | words, u32::MAX);
        dma.write(0x1F80_10B8, 0x0100_0000, u32::MAX);
    };
    let n = |c: &mut super::cdvd::Cdvd, i: &mut u32, cmd: u8, params: &[u8]| {
        for &b in params { c.write(0x1F40_2005, b, i); }
        c.write(0x1F40_2004, cmd, i);
    };
    setup(&mut dma, 512, 2);
    n(&mut c, &mut i_stat, 0x06, &[30, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0]);
    assert_eq!(c.read(0x1F40_2005) & 0xC0, 0x80, "busy");
    let mut t = 0;
    while c.istat & 2 == 0 && t < 4000 { c.tick(10_000, &mut dma, &mut ram, &mut i_stat); t += 1; }
    assert!(c.istat & 2 != 0 && i_stat & super::hw::IOP_CDVD != 0, "finished with the interrupt");
    assert_eq!(c.read(0x1F40_2005), 0x40);
    assert_eq!(&ram[0x1000..0x1003], &[30, 30, 30]);
    assert_eq!(&ram[0x1800..0x1803], &[31, 31, 31]);
    assert_eq!(dma.ch[3].chcr >> 24 & 1, 0, "channel 3 ended");
    assert_eq!(c.sectors, 2);
    // a DVD read on a CD: error 14h
    c.istat = 0;
    n(&mut c, &mut i_stat, 0x08, &[30, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
    assert_eq!(c.read(0x1F40_2006), 0x14);
    let _ = std::fs::remove_file(p);
    // the same on a DVD
    let p = make_iso("BOOT2 = cdrom0:\\SLUS_200.62;1\r\n", 64, true);
    let mut c = super::cdvd::Cdvd::default();
    c.insert(Disc::open(&p).unwrap());
    setup(&mut dma, 516, 1);
    n(&mut c, &mut i_stat, 0x08, &[40, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
    let mut t = 0;
    while c.istat & 2 == 0 && t < 4000 { c.tick(10_000, &mut dma, &mut ram, &mut i_stat); t += 1; }
    assert_eq!(&ram[0x1000..0x1004], &[0x20, 0x03, 0x00, 40], "layer 0, sector 40 + 30000h");
    assert_eq!(ram[0x1000 + 12], 40);
    let _ = std::fs::remove_file(p);
}

#[test]
fn disc_key_from_the_serial() {
    let k = super::cdvd::disc_key("SLUS-20062", 75);
    assert_eq!(k[15], 0x05);
    assert_eq!(k[14], (((20062 & 0x3E0) >> 2) | 4) as u8);
    assert_eq!(k[4], (((20062 & 0x1F) << 3) | ((b'S' as u32 & 0x7F) << 21 & 0x0E00_0000) >> 25) as u8);
    assert_eq!(super::cdvd::disc_key("SLUS-20062", 0)[15], 0x01);
}

/// PS2_ISO=path: what the detector and disc_info say about a real image
#[test]
#[ignore]
fn ps2_iso_probe() {
    let p = std::env::var("PS2_ISO").expect("PS2_ISO=path");
    let mut d = crate::common::disc::Disc::open(std::path::Path::new(&p)).unwrap();
    println!("tracks {:?} leadout {}", d.tracks.len(), d.leadout);
    println!("SYSTEM.CNF: {:?}", super::iso::read_root_file(&mut d, "SYSTEM.CNF").map(|v| String::from_utf8_lossy(&v).to_string()));
    println!("info: {:?}", super::iso::disc_info(&mut d));
    let mut blob = crate::segacd::PATH_MAGIC.to_vec();
    blob.extend_from_slice(p.as_bytes());
    let g = crate::common::rom_detect::detect(&blob, &p);
    println!("detect: {:?}", g.iter().map(|g| (g.system, g.why.clone())).collect::<Vec<_>>());
}
