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

fn chip_bus<'a>(c: &'a mut Sh7604, r: &'a mut Ram) -> ChipBus<'a, Ram> { ChipBus { p: &mut c.p, ext: r, fetch: 0 } }

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

/// A 32X cartridge whose 68000 code sets RV (so the cartridge stays visible at 000000 once
/// the adapter is on), turns the adapter on and lets the SH-2s go (`a15100`), then loops.
fn test_rom(a15100: u16) -> Vec<u8> {
    let mut rom = vec![0u8; 0x10000];
    put32(&mut rom, 0, 0xFFFF_FE00);
    put32(&mut rom, 4, 0x200);
    rom[0x100..0x108].copy_from_slice(b"SEGA 32X");
    // move.w #1,$A15106; move.w #a15100,$A15100; bra *
    for (i, w) in [0x33FCu16, 0x0001, 0x00A1, 0x5106, 0x33FC, a15100, 0x00A1, 0x5100, 0x60FE].iter().enumerate() { put16(&mut rom, 0x200 + i * 2, *w); }
    rom
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
    let rom = test_rom(0x0003);
    let mut sys = crate::genesis::md_system::GenesisSystem::from_32x(&rom, "t", &bios()).unwrap();
    use crate::common::system::System;
    for _ in 0..3 { sys.run_frame(0); }
    assert!((0x200..=0x212).contains(&sys.debug_pc()), "the 68000 is in its loop: {:06X}", sys.debug_pc());
    use crate::common::cpu::m68000::M68kBus;
    let b = &mut sys.bus;
    b.write_word(0xA15106, 0);              // RV off: the cartridge leaves 000000
    assert_eq!((b.read_word(0xA130EC), b.read_word(0xA130EE)), (0x4D41, 0x5253), "MARS");
    assert_eq!(b.read_word(0xA15100) & 3, 3, "adapter on, SH-2s running");
    assert_eq!(b.read_word(0x000006), 0x0200, "the 32X vectors at 0");
    assert_eq!(b.read_word(0x000200), 0xFFFF, "the cartridge left 000000 (RV off)");
    assert_eq!(b.read_word(0x880208), 0x33FC, "it's at 880000");
    assert_eq!((b.read_word(0xA15120), b.read_word(0xA15122)), (0x4D53, 0x534C), "comm words from the SH-2s");
    b.write_word(0xA15106, 1);              // RV: back at 000000 (and gone from 880000)
    assert_eq!((b.read_word(0x000208), b.read_word(0x880208)), (0x33FC, 0xFFFF));
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
    let rom = test_rom(0x0003);
    let mut sys = crate::genesis::md_system::GenesisSystem::from_32x(&rom, "t", &bios()).unwrap();
    use crate::common::system::System;
    use crate::common::cpu::m68000::M68kBus;
    sys.run_frame(0);
    sys.bus.write_word(0xA15180, 1);   // packed-pixel picture on every line
    let t = std::time::Instant::now();
    let frames: usize = std::env::var("FRAMES").ok().and_then(|v| v.parse().ok()).unwrap_or(600);
    for _ in 0..frames { sys.run_frame(0); }
    println!("{frames} frames: {:?} ({:.2} ms a frame)", t.elapsed(), t.elapsed().as_secs_f64() * 1000.0 / frames as f64);
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

// ---------------- step 3: the picture ----------------

/// A VDP with mode `mode` latched and every line pointing at word 0x100 of the shown bank.
fn vdp(mode: u8, priority: bool) -> Vdp {
    let mut v = Vdp::default();
    v.latch = (mode, false, priority, false);
    let f = v.front();
    for y in 0..224 { v.dram[f + y] = 0x100; }
    v
}
fn px(row: &[u8], x: usize) -> [u8; 3] { [row[x * 4], row[x * 4 + 1], row[x * 4 + 2]] }
const GREY: [u8; 3] = [9, 9, 9];

#[test]
fn direct_colour_and_priority() {
    let mut v = vdp(2, false);
    let f = v.front();
    v.dram[f + 0x100] = 0x001F;            // x0: red
    v.dram[f + 0x101] = 0x8000 | 0x03E0;   // x1: green, through bit
    v.dram[f + 0x102] = 0x7C00;            // x2: blue
    let run = |v: &Vdp, bd: &[bool]| { let mut row = [9u8; 320 * 4]; v.composite(0, &mut row, bd); row };
    let mut bd = vec![true; 320];
    bd[2] = false;                          // the Genesis drew something at x2
    // Genesis has priority: 32X shows where the Genesis shows backdrop, or through the through bit
    let row = run(&v, &bd);
    assert_eq!((px(&row, 0), px(&row, 1), px(&row, 2)), ([255, 0, 0], [0, 255, 0], GREY));
    // 32X has priority: everything but through-bit pixels over Genesis graphics
    v.latch.2 = true;
    let mut bd2 = vec![false; 320];
    bd2[0] = true;
    let row = run(&v, &bd2);
    assert_eq!((px(&row, 0), px(&row, 1), px(&row, 2)), ([255, 0, 0], GREY, [0, 0, 255]));
    // mode 0 draws nothing; lines past 224 aren't drawn
    let mut row = [9u8; 320 * 4];
    v.composite(224, &mut row, &bd);
    v.latch.0 = 0;
    v.composite(0, &mut row, &bd);
    assert!(row.iter().all(|&b| b == 9));
}

#[test]
fn packed_pixels_with_dotshift() {
    let mut v = vdp(1, false);
    let f = v.front();
    v.cram[1] = 0x001F;
    v.cram[2] = 0x7C00;
    v.dram[f + 0x100] = 0x0102;             // x0 = colour 1 (high byte), x1 = colour 2
    let bd = vec![true; 320];
    let mut row = [9u8; 320 * 4];
    v.composite(0, &mut row, &bd);
    assert_eq!((px(&row, 0), px(&row, 1), px(&row, 2)), ([255, 0, 0], [0, 0, 255], [0, 0, 0]));
    v.latch.3 = true;                        // dotshift: one pixel left
    v.composite(0, &mut row, &bd);
    assert_eq!(px(&row, 0), [0, 0, 255]);
}

#[test]
fn run_length() {
    let mut v = vdp(3, false);
    let f = v.front();
    v.cram[5] = 0x03E0;
    v.cram[6] = 0x001F;
    v.dram[f + 0x100] = 0x0205;              // 3 pixels of colour 5
    v.dram[f + 0x101] = 0xFF06;              // then colour 6 (to the end of the line)
    let bd = vec![true; 320];
    let mut row = [9u8; 320 * 4];
    v.composite(0, &mut row, &bd);
    assert_eq!((px(&row, 2), px(&row, 3), px(&row, 258)), ([0, 255, 0], [255, 0, 0], [255, 0, 0]));
    assert_eq!(px(&row, 259), [0, 0, 0], "256 pixels, then the next word (colour 0)");
}

#[test]
fn fill_ends() {
    let mut x = M32x::new(&bios()).unwrap();
    x.run(50_000, &[]);
    x.st.vdp.autofill_length = 3;
    x.st.vdp.autofill_address = 0x10FE;
    vdp_write(&mut x.st.vdp, 0x8, true, true, 0xBEEF);
    let b = x.st.vdp.back();
    assert_eq!(&x.st.vdp.dram[b + 0x10FE..b + 0x1100], &[0xBEEF, 0xBEEF]);
    assert_eq!(&x.st.vdp.dram[b + 0x1000..b + 0x1002], &[0xBEEF, 0xBEEF], "the low byte wraps");
    assert!(x.st.vdp.engaged(), "FEN while it runs");
    x.run(1_000, &[]);
    assert!(!x.st.vdp.engaged(), "and then it's done");
}

/// The 68000 sets up the 32X picture by itself (FM = 0): line table and pixels into the
/// back bank, mode 2, swap. After the next vblank the frame shows it over the (blank) Genesis.
#[test]
fn picture_on_screen() {
    let rom = test_rom(0x0003);
    let mut sys = crate::genesis::md_system::GenesisSystem::from_32x(&rom, "t", &bios()).unwrap();
    use crate::common::system::System;
    use crate::common::cpu::m68000::M68kBus;
    sys.run_frame(0);
    let b = &mut sys.bus;
    for y in 0..224u32 { b.write_word(0x840000 + y * 2, 0x100); }
    for x in 0..320u32 { b.write_word(0x840200 + x * 2, if x == 7 { 0x001F } else { 0x7C00 }); }
    b.write_word(0xA15180, 2);                         // mode 2
    let fs = b.read_word(0xA1518A) & 1;
    b.write_word(0xA1518A, fs ^ 1);                    // show what was drawn
    sys.run_frame(0);
    sys.run_frame(0);
    let fb = &sys.bus.vdp.framebuffer;
    let at = |x: usize, y: usize| [fb[(y * 320 + x) * 4], fb[(y * 320 + x) * 4 + 1], fb[(y * 320 + x) * 4 + 2]];
    assert_eq!((at(7, 100), at(8, 100), at(0, 223)), ([255, 0, 0], [0, 0, 255], [0, 0, 255]));
}

// ---------------- step 4: PWM sound ----------------

#[test]
fn pwm_fifos() {
    let mut p = pwm::Pwm::default();
    assert_eq!(p.read(0x4) >> 14, 0b01, "empty");
    for s in [1u16, 2, 3, 4] { p.write(0x4, true, true, s); }
    assert_eq!(p.read(0x4) >> 14, 0b10, "full at 3");
    p.write(0x8, true, true, 0x123);          // mono: both FIFOs
    assert_eq!((p.read(0x6) >> 14, p.read(0x8) >> 14), (0b00, 0b10));
    p.write(0x6, false, true, 0x45);           // upper byte not written: low byte alone queues it
    p.write(0x6, true, false, 0x700);          // upper byte alone queues nothing
    assert_eq!(p.read(0x6) & 0xFFF, 0x745, "the latch reads back");
    // play: L now holds 3, 4, 0x123 (1 and 2 were pushed out)
    p.cycle = 100; p.lmode = 1; p.rmode = 1;
    let mut got = vec![];
    let mut played = vec![];
    for _ in 0..5 { p.tick(53_693_175.0); got.push(p.read(0x4) >> 14); played.push(p.playing().0); }
    assert_eq!(got, vec![0, 0, 1, 1, 1], "3 samples, then empty");
    assert_eq!(played, vec![3, 4, 0x123, 0x123, 0x123], "the newest 3; then the last one repeats");
}

#[test]
fn pwm_timer_interrupt_and_dreq() {
    let mut p = pwm::Pwm::default();
    p.cycle = 100; p.lmode = 1; p.timer = 3; p.dreq_irq = true;
    let ticks: Vec<bool> = (0..7).map(|_| p.tick(53_693_175.0).irq).collect();
    assert_eq!(ticks, vec![false, false, true, false, false, true, false], "every 3 periods");
    p.timer = 0;                               // 0 = 16
    p.periods = 0;
    assert_eq!((0..32).filter(|_| p.tick(53_693_175.0) == pwm::Tick { irq: true, dreq: true }).count(), 2);
    p.lmode = 0; p.rmode = 3;                  // nothing valid: no FIFO reads, no interrupts
    assert!((0..40).all(|_| !p.tick(53_693_175.0).irq));
}

/// The way games feed it: master DMA channel 1 on DREQ1, SDRAM → the left FIFO, one word
/// per PWM interrupt period.
#[test]
fn pwm_fed_by_dma() {
    let mut x = M32x::new(&bios()).unwrap();
    x.run(50_000, &[]);
    for (i, s) in [10u16, 20, 30, 40, 50, 60].iter().enumerate() { x.st.sdram[0x100 + i] = *s; }
    {
        let p = &mut x.sh[0].p;
        p.dmac.sar[1] = 0x2600_0200;
        p.dmac.dar[1] = 0x2000_4034;
        p.dmac.tcr[1] = 6;
        p.dmac.dme = true;
        p.dmac.drcr[1] = 0;
        p.dmac.chcr[1] = Chcr { de: true, ts: 1, sm: 1, dm: 0, ..Default::default() }; // word, source +
    }
    let mut seen = vec![];
    x.st.pwm.cycle = 1000;
    x.st.pwm.timer = 1;
    x.st.pwm.lmode = 1;
    x.st.pwm.dreq_irq = true;
    for _ in 0..12 { x.run(2400, &[]); seen.push(x.st.pwm.read(0x4) & 0xFFF); }
    assert!(x.sh[0].p.dmac.chcr[1].te, "all 6 words went");
    assert_eq!(*seen.last().unwrap(), 60);
    assert!(x.st.irq[0].pwm.active && x.st.irq[1].pwm.active);
    assert!(!x.sh[0].p.dmac.dreq[1], "DREQ1 is only a pulse");
}

#[test]
fn pwm_sound_out() {
    let mut x = M32x::new(&bios()).unwrap();
    // off: silence, but still 44.1 kHz of it (the mixer needs it)
    x.run(53_693_175 / 60, &[]);
    let n = x.st.pwm.out.len();
    // (735 stereo pairs a frame; the last period can run a few samples into the next frame)
    assert!((1468..=1490).contains(&n), "{n} samples");
    let mut n = n;
    for _ in 0..59 { x.st.pwm.out.clear(); x.run(53_693_175 / 60, &[]); n += x.st.pwm.out.len(); }
    assert!((88_190..=88_220).contains(&n), "a second: {n} (44,100 pairs)");
    assert!(x.st.pwm.out.iter().all(|&s| s == 0.0));
    x.st.pwm.out.clear();
    // a square wave on the left only: loud on the left, silent on the right
    x.st.pwm.cycle = 1000;
    x.st.pwm.lmode = 1;
    x.st.pwm.rmode = 0;
    for f in 0..6 {
        for i in 0..24 { x.st.pwm.write(0x4, true, true, if (f * 24 + i) / 12 % 2 == 0 { 900 } else { 100 }); x.run(2400, &[]); }
    }
    let out = &x.st.pwm.out;
    let peak = |o: usize| out.iter().skip(o).step_by(2).fold(0f32, |m, s| m.max(s.abs()));
    assert!(peak(0) > 0.2, "left {}", peak(0));
    assert_eq!(peak(1), 0.0);
}

/// The Genesis mixer gets a full frame of sound with the 32X in (it waits for the PWM stream).
#[test]
fn mixed_into_the_genesis_sound() {
    let rom = test_rom(0x0003);
    let mut sys = crate::genesis::md_system::GenesisSystem::from_32x(&rom, "t", &bios()).unwrap();
    use crate::common::system::System;
    let mut total = 0;
    for _ in 0..60 { sys.run_frame(0); total += sys.drain_audio().len(); }
    assert!((88_000..=88_400).contains(&total), "{total} samples in a second (the same as without the 32X)");
}

/// Tiny SH-2 assembler for the benchmark: code at `org`, then a literal pool of longs.
struct Asm { org: u32, code: Vec<u16>, lits: Vec<u32>, fix: Vec<(usize, usize)> }
impl Asm {
    fn new(org: u32) -> Self { Self { org, code: vec![], lits: vec![], fix: vec![] } }
    fn here(&self) -> u32 { self.org + self.code.len() as u32 * 2 }
    fn op(&mut self, w: u16) -> &mut Self { self.code.push(w); self }
    /// mov.l @(lit), rn
    fn li(&mut self, n: u16, v: u32) -> &mut Self { self.fix.push((self.code.len(), self.lits.len())); self.lits.push(v); self.op(0xD000 | n << 8) }
    fn bf(&mut self, to: u32) -> &mut Self { let d = (to as i32 - (self.here() as i32 + 4)) / 2; self.op(0x8B00 | (d as u8) as u16) }
    fn bra(&mut self, to: u32) -> &mut Self { let d = (to as i32 - (self.here() as i32 + 4)) / 2; self.op(0xA000 | (d as u16 & 0xFFF)).op(0x0009) }
    fn put(mut self, rom: &mut [u8]) {
        if self.code.len() % 2 == 1 { self.code.push(0x0009); }
        let pool = self.here();
        for (at, k) in std::mem::take(&mut self.fix) {
            let a = self.org + at as u32 * 2;
            let d = (pool + k as u32 * 4 - ((a & !3) + 4)) / 4;
            self.code[at] |= d as u16;
        }
        for (i, w) in self.code.iter().enumerate() { put16(rom, (self.org as usize) + i * 2, *w); }
        for (i, v) in self.lits.iter().enumerate() { put32(rom, pool as usize + i * 4, *v); }
    }
}

/// Local only: both SH-2s busy with cached memory work (master: SDRAM → framebuffer copy,
/// slave: multiply-accumulate over SDRAM, feeding the PWM FIFO), PWM at 22 kHz.
#[test]
#[ignore]
fn zz_m32x_busy_speed() {
    let mut fw = crate::common::firmware::files(&bios(), BIOS_MAGIC);
    for (name, data) in fw.iter_mut() {
        if data.len() == 256 { continue; }             // the 68000's vectors
        let slave = data.len() == 1024;
        let _ = name;
        let mut a = Asm::new(0x180);
        a.li(4, 0xFFFF_FE92).op(0xE001).op(0x2400);        // cache on: mov #1,r0; mov.b r0,@r4
        let top = a.here();
        if !slave {
            a.li(1, 0x0600_1000).li(2, 0x2400_0200).li(3, 0x400);
            let lp = a.here();
            a.op(0x6016).op(0x350C).op(0x4009).op(0x2201).op(0x7202).op(0x4310).bf(lp);
        } else {
            a.li(1, 0x0200_0000).li(2, 0x2000_4038).li(3, 0x4000);   // streams 64 KB of cartridge (cache misses)
            let lp = a.here();
            a.op(0x6016).op(0x0607).op(0x071A).op(0x4729).op(0x2271).op(0x4310).bf(lp); // mov.l @r1+,r0; mul.l; sts macl,r7; shlr16 r7; mov.w r7,@r2; dt; bf
        }
        a.bra(top);
        a.put(data);
    }
    if std::env::var("DUMP").is_ok() { for (n, d) in &fw { if d.len() == 2048 { let w: Vec<String> = (0x180..0x1C0).step_by(2).map(|i| format!("{:04X}", (d[i] as u16) << 8 | d[i+1] as u16)).collect(); println!("{n}: {}", w.join(" ")); } } }
    let bios: Vec<(&str, &[u8])> = fw.iter().map(|(n, d)| (n.as_str(), d.as_slice())).collect();
    let bundle = crate::common::firmware::bundle(BIOS_MAGIC, &bios);
    let rom = test_rom(0x8003);
    let mut sys = crate::genesis::md_system::GenesisSystem::from_32x(&rom, "t", &bundle).unwrap();
    use crate::common::system::System;
    use crate::common::cpu::m68000::M68kBus;
    sys.run_frame(0);
    sys.bus.write_word(0xA15180, 1);
    { let x = sys.bus.m32x.as_mut().unwrap(); x.st.pwm.cycle = 1047; x.st.pwm.lmode = 1; x.st.pwm.rmode = 1; x.st.pwm.timer = 1; }
    let frames: usize = std::env::var("FRAMES").ok().and_then(|v| v.parse().ok()).unwrap_or(600);
    let t = std::time::Instant::now();
    for _ in 0..frames { sys.run_frame(0); sys.drain_audio(); }
    let x = sys.bus.m32x.as_ref().unwrap();
    println!("busy {frames} frames: {:?} ({:.2} ms a frame)  PCs {:08X} {:08X}  cache {} {}", t.elapsed(), t.elapsed().as_secs_f64() * 1000.0 / frames as f64,
             x.sh[0].cpu.instruction_pc(), x.sh[1].cpu.instruction_pc(), x.sh[0].p.cache.enable, x.sh[1].p.cache.enable);
    assert!(x.sh.iter().all(|s| s.p.cache.enable && (0x180..0x1C0).contains(&s.cpu.instruction_pc())), "both still in their loops");
}

/// The Z80's 32 KB window into 68000 space goes through the 32X's map: music drivers read
/// their data from the cartridge at 880000/900000 and can write the 32X's registers.
#[test]
fn z80_window_sees_the_32x_map() {
    let mut rom = test_rom(0x0003);
    rom[0x1000] = 0xAB;
    let mut sys = crate::genesis::md_system::GenesisSystem::from_32x(&rom, "t", &bios()).unwrap();
    use crate::common::system::System;
    use crate::common::cpu::m68000::M68kBus;
    use crate::common::cpu::z80::Bus as Z80Bus;
    sys.run_frame(0);
    let b = &mut sys.bus;
    b.write_word(0xA15106, 0);                 // RV off: the cartridge is only at 880000
    let mut z = crate::genesis::md_bus::Z80View(b);
    z.0.zbank = 0x880000 >> 15;
    assert_eq!(z.read(0x8000 + 0x1000), 0xAB, "cartridge through 880000");
    z.0.zbank = 0x900000 >> 15;                // bank 0 of the 1 MB window
    assert_eq!(z.read(0x8000 + 0x1000), 0xAB, "and through 900000");
    z.0.zbank = 0;
    assert_eq!(z.read(0x8000 + 0x1000), 0xFF, "not at 000000 with RV off");
    z.0.zbank = 0xA10000 >> 15;                // 32X registers: the PWM cycle register
    z.write(0x8000 + 0x5132, 0x04);
    z.write(0x8000 + 0x5133, 0x17);
    assert_eq!(sys.bus.m32x.as_ref().unwrap().st.pwm.cycle, 0x417);
}
