use super::sh7604::*;
use super::*;

/// Flat RAM outside the chip (16-bit bus), 1 MB mirrored.
struct Ram(Vec<u8>);
impl ExtBus for Ram {
    fn read(&mut self, a: u32) -> u16 { let a = a as usize & 0xFFFFE; (self.0[a] as u16) << 8 | self.0[a + 1] as u16 }
    fn write(&mut self, a: u32, v: u16, upper: bool, lower: bool) {
        let a = a as usize & 0xFFFFE;
        if upper { self.0[a] = (v >> 8) as u8; }
        if lower { self.0[a + 1] = v as u8; }
    }
}
fn ram() -> Ram { Ram(vec![0; 0x100000]) }

fn chip_bus<'a>(c: &'a mut Sh7604, r: &'a mut Ram) -> ChipBus<'a> { ChipBus { p: &mut c.p, ext: r, fetch: 0 } }

#[test]
fn divider_32_and_64() {
    let mut c = Sh7604::new();
    let mut r = ram();
    let mut b = chip_bus(&mut c, &mut r);
    b.write(0xFFFF_FF00, 4, 7, true);                  // DVSR = 7
    b.write(0xFFFF_FF04, 4, (-100i32) as u32, true);   // DVDNT: 32/32 starts
    assert_eq!(b.read(0xFFFF_FF04, 4, true) as i32, -14);
    assert_eq!(b.read(0xFFFF_FF10, 4, true) as i32, -2, "remainder in DVDNTH");
    // 64/32: 0x1_0000_0000 / 16
    b.write(0xFFFF_FF00, 4, 16, true);
    b.write(0xFFFF_FF10, 4, 1, true);
    b.write(0xFFFF_FF14, 4, 0, true);
    assert_eq!(b.read(0xFFFF_FF14, 4, true), 0x1000_0000);
    // divide by zero: overflow flag
    b.write(0xFFFF_FF00, 4, 0, true);
    b.write(0xFFFF_FF04, 4, 5, true);
    assert_eq!((b.read(0xFFFF_FF04, 4, true), b.read(0xFFFF_FF08, 4, true) & 1), (0x7FFF_FFFF, 1));
}

#[test]
fn cache_hits_are_stale_until_purged_and_data_array_is_ram() {
    let mut c = Sh7604::new();
    let mut r = ram();
    r.write(0x6000, 0x1234, true, true);
    let mut b = chip_bus(&mut c, &mut r);
    b.write(0xFFFF_FE92, 1, 0x01, true);              // cache on
    assert_eq!(b.read(0x0000_6000, 2, true), 0x1234);  // miss: line filled
    b.ext.write(0x6000, 0x5678, true, true);           // memory changes behind its back
    assert_eq!(b.read(0x0000_6000, 2, true), 0x1234, "cached: stale");
    assert_eq!(b.read(0x2000_6000, 2, true), 0x5678, "uncached area sees memory");
    b.write(0x4000_6000, 4, 0, true);                  // purge that line
    assert_eq!(b.read(0x0000_6000, 2, true), 0x5678);
    // a cached write updates the line and memory (write-through)
    b.write(0x0000_6002, 2, 0xABCD, true);
    assert_eq!((b.read(0x0000_6002, 2, true), b.ext.read(0x6002)), (0xABCD, 0xABCD));
    // data array: 4 KB of RAM at C0000000
    b.write(0xFFFF_FE92, 1, 0x10, true);              // cache off, purge all
    b.write(0xC000_0010, 4, 0xDEAD_BEEF, true);
    assert_eq!(b.read(0xC000_0010, 4, true), 0xDEAD_BEEF);
    assert_eq!(b.read(0xC000_0012, 2, true), 0xBEEF);
}

/// A CPU looping on BRA at 0 while DMA channel 0 copies 4 longs (auto request).
fn looping(r: &mut Ram) {
    r.write(0, 0, true, true); r.write(2, 0x0100, true, true);   // reset PC = 100
    r.write(4, 0, true, true); r.write(6, 0x8000, true, true);   // SP
    r.write(0x100, 0xAFFE, true, true);                            // BRA self
    r.write(0x102, 0x0009, true, true);                            // NOP
}

#[test]
fn dma_auto_request_copies_and_interrupts() {
    let mut c = Sh7604::new();
    let mut r = ram();
    looping(&mut r);
    for i in 0..8u32 { r.write(0x1000 + i * 2, 0x1100 + i as u16, true, true); }
    {
        let mut b = chip_bus(&mut c, &mut r);
        b.write(0xFFFF_FF80, 4, 0x1000, true);   // SAR0
        b.write(0xFFFF_FF84, 4, 0x2000, true);   // DAR0
        b.write(0xFFFF_FF88, 4, 4, true);        // TCR0: 4 transfers
        b.write(0xFFFF_FFA0, 4, 0x55, true);     // vector
        b.write(0xFFFF_FEE2, 1, 0x05, true);     // DMAC priority 5 (IPRA low nibble... high = DIVU)
        b.write(0xFFFF_FFB0, 4, 1, true);        // DMAOR: DME
        // CHCR0: dest +, source +, long, auto request; enable, interrupt on end
        b.write(0xFFFF_FF8C, 4, 1 << 14 | 1 << 12 | 2 << 10 | 1 << 9 | 1 << 2 | 1, true);
    }
    c.cpu.sr.i = 0;
    for _ in 0..10 { c.step(&mut r); }
    let got: Vec<u16> = (0..8u32).map(|i| r.read(0x2000 + i * 2)).collect();
    assert_eq!(got, (0..8).map(|i| 0x1100 + i).collect::<Vec<u16>>());
    assert!(c.p.dmac.chcr[0].te, "transfer end");
    assert_eq!(c.cpu.sr.i, 5, "DMA end interrupt taken");
}

#[test]
fn frt_compare_interrupt() {
    let mut c = Sh7604::new();
    let mut r = ram();
    looping(&mut r);
    {
        let mut b = chip_bus(&mut c, &mut r);
        b.write(0xFFFF_FE16, 1, 3, true);       // FRT clock = CPU clock (/1)
        b.write(0xFFFF_FE14, 1, 0x00, true);    // OCRA = 0010
        b.write(0xFFFF_FE15, 1, 0x10, true);
        b.write(0xFFFF_FE10, 1, 0x08, true);    // OCIAE
        b.write(0xFFFF_FE60, 1, 0x07, true);    // FRT priority 7
        b.write(0xFFFF_FE67, 1, 0x60, true);    // vector
    }
    c.cpu.sr.i = 0;
    for _ in 0..40 { c.step(&mut r); if c.cpu.sr.i == 7 { break; } }
    assert_eq!(c.cpu.sr.i, 7, "output compare A interrupt");
    assert!(c.p.frt.ocfa);
}

// ---------------- the 32X ----------------

fn put16(m: &mut [u8], a: usize, v: u16) { m[a..a + 2].copy_from_slice(&v.to_be_bytes()); }
fn put32(m: &mut [u8], a: usize, v: u32) { m[a..a + 4].copy_from_slice(&v.to_be_bytes()); }

/// Made-up BIOS: each SH-2 writes a word to a communication register; the master also
/// turns interrupts on and its CMD handler (vector 68) writes 77 to comm 2.
fn bios() -> Vec<u8> {
    let mut m = vec![0u8; 2048];
    put32(&mut m, 0, 0x180);                // reset PC
    put32(&mut m, 4, 0x0604_0000);          // SP (top of SDRAM)
    put32(&mut m, 8, 0x180);                // warm reset (after A15100 bit 1 = 0)
    put32(&mut m, 12, 0x0604_0000);
    put32(&mut m, 68 * 4, 0x200);           // CMD interrupt
    for (i, w) in [0xD103u16, 0xD204, 0x2121, 0xE000, 0x400E, 0xAFFE, 0x0009].iter().enumerate() { put16(&mut m, 0x180 + i * 2, *w); }
    put32(&mut m, 0x190, 0x2000_4020);
    put32(&mut m, 0x194, 0x4D53);           // "MS"
    for (i, w) in [0xD102u16, 0xE277, 0x2121, 0xAFFE, 0x0009].iter().enumerate() { put16(&mut m, 0x200 + i * 2, *w); }
    put32(&mut m, 0x20C, 0x2000_4024);
    let mut s = vec![0u8; 1024];
    put32(&mut s, 0, 0x180);
    put32(&mut s, 4, 0x0603_0000);
    put32(&mut s, 8, 0x180);
    put32(&mut s, 12, 0x0603_0000);
    for (i, w) in [0xD103u16, 0xD204, 0x2121, 0xAFFE, 0x0009].iter().enumerate() { put16(&mut s, 0x180 + i * 2, *w); }
    put32(&mut s, 0x190, 0x2000_4022);
    put32(&mut s, 0x194, 0x534C);           // "SL"
    let mut v = vec![0u8; 256];
    put32(&mut v, 4, 0x0088_0200);          // the 32X's 68000 reset vector (into the cartridge window)
    crate::common::firmware::bundle(BIOS_MAGIC, &[("32X_G_BIOS.BIN", &v), ("32X_M_BIOS.BIN", &m), ("32X_S_BIOS.BIN", &s)])
}

#[test]
fn both_sh2s_boot_and_take_the_68000s_interrupt() {
    let mut x = M32x::new(&bios()).unwrap();
    x.run(50_000, &[]);
    assert_eq!(&x.st.comm[..2], &[0x4D53, 0x534C], "both ran their boot ROM");
    // the 68000 raises CMD for the master (A15102 bit 0); its mask allows it
    x.st.irq[0].cmd.enable = true;
    assert!(x.io_write(0xA15102, false, true, 1));
    x.run(5_000, &[]);
    assert_eq!(x.st.comm[2], 0x77, "CMD handler ran");
    assert_eq!(x.sh[0].cpu.sr.i, 8);
}

#[test]
fn missing_bios_is_named() {
    let e = M32x::new(&crate::common::firmware::bundle(BIOS_MAGIC, &[("32X_M_BIOS.BIN", &[0u8; 2048])])).err().unwrap();
    assert!(e.contains("68000") && e.contains("slave") && !e.contains("master"), "{e}");
}

/// A cartridge whose 68000 code switches the 32X in and keeps running from the 880000
/// window; the Genesis side sees the ID, the vectors and the SH-2s' words.
#[test]
fn genesis_side_through_the_adapter() {
    let mut rom = vec![0u8; 0x10000];
    put32(&mut rom, 0, 0xFFFF_FE00);
    put32(&mut rom, 4, 0x200);
    rom[0x100..0x108].copy_from_slice(b"SEGA 32X");
    // move.w #3,$A15100 (adapter on, SH-2s out of reset), jmp $88020E, bra *
    for (i, w) in [0x33FCu16, 0x0003, 0x00A1, 0x5100, 0x4EF9, 0x0088, 0x020E, 0x60FE].iter().enumerate() { put16(&mut rom, 0x200 + i * 2, *w); }
    let mut sys = crate::genesis::md_system::GenesisSystem::from_32x(&rom, "t", &bios()).unwrap();
    use crate::common::system::System;
    for _ in 0..3 { sys.run_frame(0); }
    use crate::common::cpu::m68000::M68kBus;
    let b = &mut sys.bus;
    assert_eq!((b.read_word(0xA130EC), b.read_word(0xA130EE)), (0x4D41, 0x5253), "MARS");
    assert_eq!(b.read_word(0xA15100) & 3, 3, "adapter on, SH-2s running");
    assert_eq!(b.read_word(0x000006), 0x0200, "the 32X vectors at 0");
    assert_eq!(b.read_word(0x000200), 0xFFFF, "the cartridge left 000000 (RV off)");
    assert_eq!(b.read_word(0x880200), 0x33FC, "it's at 880000");
    assert_eq!((b.read_word(0xA15120), b.read_word(0xA15122)), (0x4D53, 0x534C), "comm words from the SH-2s");
    b.write_word(0xA15106, 1);              // RV: back at 000000
    assert_eq!(b.read_word(0x000200), 0x33FC);
}

#[test]
fn reset_bit_holds_the_sh2s() {
    let mut x = M32x::new(&bios()).unwrap();
    x.run(50_000, &[]);                      // they boot (and loop)
    x.st.comm[0] = 0;
    x.io_write(0xA15100, false, true, 1);   // adapter on, SH-2s in reset
    x.run(50_000, &[]);
    assert_eq!(x.st.comm[0], 0, "held");
    x.io_write(0xA15100, false, true, 3);   // let go: they start over from the reset vector
    x.run(50_000, &[]);
    assert_eq!(x.st.comm[0], 0x4D53, "the boot code ran again");
}

/// DREQ: the 68000 writes 4 words into the FIFO; the master's DMA channel 0 (DREQ, from
/// the FIFO register 20004012) copies them to SDRAM.
#[test]
fn dreq_fifo_to_sdram() {
    let mut x = M32x::new(&bios()).unwrap();
    x.run(50_000, &[]);                       // boot ROMs done; master loops
    {
        let p = &mut x.sh[0].p;
        p.dmac.sar[0] = 0x2000_4012;
        p.dmac.dar[0] = 0x2600_0100;
        p.dmac.tcr[0] = 4;
        p.dmac.dme = true;
        p.dmac.drcr[0] = 0;
        p.dmac.chcr[0] = Chcr { de: true, ts: 1, sm: 0, dm: 1, ..Default::default() }; // word, dest +
    }
    x.io_write(0xA15110, true, true, 4);      // length 4
    x.io_write(0xA15106, false, true, 4);     // 68S: FIFO on
    for w in [0x1111u16, 0x2222, 0x3333, 0x4444] {
        assert!(x.io_write(0xA15112, true, true, w));
        x.run(500, &[]);
    }
    assert_eq!(&x.st.sdram[0x80..0x84], &[0x1111, 0x2222, 0x3333, 0x4444]);
    assert!(!x.st.dreq.active, "the length ran out");
    assert!(x.sh[0].p.dmac.chcr[0].te);
}

/// Local only: 600 frames, both SH-2s looping.
#[test]
#[ignore]
fn zz_m32x_speed() {
    let mut rom = vec![0u8; 0x10000];
    put32(&mut rom, 0, 0xFFFF_FE00);
    put32(&mut rom, 4, 0x200);
    rom[0x100..0x108].copy_from_slice(b"SEGA 32X");
    for (i, w) in [0x33FCu16, 0x0003, 0x00A1, 0x5100, 0x4EF9, 0x0088, 0x020E, 0x60FE].iter().enumerate() { put16(&mut rom, 0x200 + i * 2, *w); }
    let mut sys = crate::genesis::md_system::GenesisSystem::from_32x(&rom, "t", &bios()).unwrap();
    use crate::common::system::System;
    let t = std::time::Instant::now();
    for _ in 0..600 { sys.run_frame(0); }
    println!("600 frames: {:?} ({:.2} ms a frame)", t.elapsed(), t.elapsed().as_secs_f64() * 1000.0 / 600.0);
}

#[test]
fn detected_as_32x() {
    let mut rom = vec![0u8; 0x1000];
    rom[0x100..0x108].copy_from_slice(b"SEGA 32X");
    assert_eq!(crate::common::rom_detect::detect(&rom, "game.bin")[0].system, "32x");
    let mut rom = vec![0u8; 0x1000];
    rom[0x100..0x10C].copy_from_slice(b"SEGA GENESIS");
    rom[0x3C0..0x3CF].copy_from_slice(b"MARS CHECK MODE");
    assert_eq!(crate::common::rom_detect::detect(&rom, "game.bin")[0].system, "32x", "by its start-up code");
    rom[0x3C0..0x3CF].fill(0);
    assert_eq!(crate::common::rom_detect::detect(&rom, "game.bin")[0].system, "md");
}
