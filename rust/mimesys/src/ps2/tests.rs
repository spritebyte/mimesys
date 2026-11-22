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
    assert_eq!(sif.fifo1.len(), 3 * 2 + 2 * 4, "three 2-word tags and two quadwords");
    // the IOP side
    let mut iop_ram = vec![0u8; 0x1000];
    let mut io = IopDma::default();
    io.write(0x1F80_1578, 1, u32::MAX);
    io.write(0x1F80_1570, 0x8 << 12, u32::MAX);  // channel 10 on
    io.write(0x1F80_1574, 1 << 19, u32::MAX);    // its interrupt on
    io.write(0x1F80_10F4, 1 << 23, u32::MAX);    // master
    io.write(0x1F80_1538, 0x0100_0000 | 0x200, u32::MAX);
    let irq = io.step(&mut iop_ram, &mut sif);
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
    io.step(&mut iop_ram, &mut sif);
    assert_eq!(sif.fifo0.len(), 6);
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
