use super::md_bus::*;
use super::md_cart;
use super::md_system::GenesisSystem;
use super::md_vdp::{Vdp, SCREEN_W};
use crate::common::cpu::m68000::M68kBus;
use crate::common::system::System;

fn reg(v: &mut Vdp, r: u8, d: u8) { v.control_write(0x8000 | (r as u16) << 8 | d as u16); }

/// Set up a write: code 1 VRAM, 3 CRAM, 5 VSRAM (bits 0-1 in the first word, 2-5 in the second).
fn set_addr(v: &mut Vdp, code: u16, addr: u16) {
    v.control_write((code & 3) << 14 | (addr & 0x3FFF));
    v.control_write((code & 0x3C) << 2 | addr >> 14);
}

fn vram_word(v: &Vdp, a: usize) -> u16 { (v.vram[a] as u16) << 8 | v.vram[a + 1] as u16 }

fn px(v: &Vdp, x: usize, y: usize) -> [u8; 4] {
    let i = (y * SCREEN_W + x) * 4;
    v.framebuffer[i..i + 4].try_into().unwrap()
}

#[test]
fn ports_registers_and_auto_increment() {
    let mut v = Vdp::new();
    reg(&mut v, 15, 2);
    set_addr(&mut v, 1, 0xC000);
    v.data_write(0x1234);
    v.data_write(0x5678);
    assert_eq!(vram_word(&v, 0xC000), 0x1234);
    assert_eq!(vram_word(&v, 0xC002), 0x5678);
    // an odd address swaps the bytes
    set_addr(&mut v, 1, 0x0101);
    v.data_write(0xAABB);
    assert_eq!(vram_word(&v, 0x0100), 0xBBAA);
    // CRAM keeps 9 bits of colour, VSRAM 11 bits
    set_addr(&mut v, 3, 0x0004);
    v.data_write(0xFFFF);
    assert_eq!(v.cram[2], 0x0EEE);
    set_addr(&mut v, 5, 0x0002);
    v.data_write(0xFFFF);
    assert_eq!(v.vsram[1], 0x07FF);
    // read back VRAM (code 0)
    set_addr(&mut v, 0, 0xC000);
    assert_eq!(v.data_read(), 0x1234);
    assert_eq!(v.data_read(), 0x5678);
    // a register write between the two halves isn't one: the second word completes the address
    reg(&mut v, 1, 0x44);
    assert_eq!(v.reg[1], 0x44);
}

fn bus_with_rom() -> MdBus {
    let psg = crate::common::audio::sn76489::Sn76489::new(crate::common::audio::sn76489::Sn76489Variant::Sega, 3_579_545, 44_100);
    let mut rom = vec![0u8; 0x10000];
    for i in 0..0x100 { rom[0x8000 + i] = i as u8; }
    MdBus::new(rom, None, 0xA0, psg)
}

#[test]
fn dma_from_68000_memory_fill_and_copy() {
    let mut b = bus_with_rom();
    b.write_word(0xC00004, 0x8114);          // M1: DMA allowed
    b.write_word(0xC00004, 0x8F02);          // increment 2
    // 16 words from ROM 0x8000
    b.write_word(0xC00004, 0x9300 | 16);
    b.write_word(0xC00004, 0x9400);
    b.write_word(0xC00004, 0x9500 | ((0x8000 >> 1) & 0xFF) as u16);
    b.write_word(0xC00004, 0x9600 | ((0x8000 >> 9) & 0xFF) as u16);
    b.write_word(0xC00004, 0x9700);
    b.write_word(0xC00004, 0x4000 | 0x2000);  // VRAM write 0x2000 ...
    b.write_word(0xC00004, 0x0080);           // ... with the DMA bit
    assert_eq!(vram_word(&b.vdp, 0x2000), 0x0001);
    assert_eq!(vram_word(&b.vdp, 0x201E), 0x1E1F);
    assert_eq!(vram_word(&b.vdp, 0x2020), 0x0000, "16 words only");
    assert!(b.stall > 0, "the 68000 waits for the transfer");
    assert_eq!((b.vdp.reg[19], b.vdp.reg[20]), (0, 0));
    assert_eq!(b.vdp.reg[21], ((0x8000 >> 1) + 16) as u8 & 0xFF);

    // fill 8 bytes at 0x3000 with 0x77 (increment 1)
    b.write_word(0xC00004, 0x8F01);
    b.write_word(0xC00004, 0x9308);
    b.write_word(0xC00004, 0x9400);
    b.write_word(0xC00004, 0x9780);
    b.write_word(0xC00004, 0x4000 | 0x3000);
    b.write_word(0xC00004, 0x0080);
    b.write_word(0xC00000, 0x7700);
    for a in 0x3001..0x3009 { assert_eq!(b.vdp.vram[a ^ 1] , 0x77, "byte {a:x}"); }

    // copy those 8 bytes to 0x4000
    b.write_word(0xC00004, 0x9308);
    b.write_word(0xC00004, 0x9400);
    b.write_word(0xC00004, 0x9500);
    b.write_word(0xC00004, 0x9630);
    b.write_word(0xC00004, 0x97C0);
    b.write_word(0xC00004, 0x4000);
    b.write_word(0xC00004, 0x00C1);
    assert_eq!(&b.vdp.vram[0x4000..0x4008], &b.vdp.vram[0x3000..0x3008]);
}

#[test]
fn line_and_frame_interrupts() {
    let mut v = Vdp::new();
    reg(&mut v, 0, 0x14);                    // HINT on
    reg(&mut v, 1, 0x64);                    // display, VINT on
    reg(&mut v, 10, 3);
    for line in 225..262 { v.begin_line(line); }
    let mut hints = Vec::new();
    for line in 0..225 {
        v.begin_line(line);
        if v.ipl() == 4 { hints.push(line); v.acknowledge(4); }
    }
    assert_eq!(&hints[..3], &[3, 7, 11], "every 4th line");
    assert_eq!(v.ipl(), 6, "vblank");
    assert!(v.status() & 0x80 != 0);
    v.acknowledge(6);
    assert_eq!(v.ipl(), 0);
    assert!(v.status() & 0x08 != 0, "vblank flag");
}

/// A tile filled with colour `c` at tile index `t`.
fn solid_tile(v: &mut Vdp, t: usize, c: u8) {
    for i in 0..32 { v.vram[t * 32 + i] = c << 4 | c; }
}

fn name(v: &mut Vdp, base: usize, cols: usize, x: usize, y: usize, entry: u16) {
    let a = base + (y * cols + x) * 2;
    v.vram[a] = (entry >> 8) as u8;
    v.vram[a + 1] = entry as u8;
}

#[test]
fn planes_window_sprites_and_priority() {
    let mut v = Vdp::new();
    reg(&mut v, 1, 0x44);                     // display on, mode 5
    reg(&mut v, 2, 0x30);                     // plane A at 0xC000
    reg(&mut v, 4, 0x07);                     // plane B at 0xE000
    reg(&mut v, 3, 0x2C);                     // window at 0xB000
    reg(&mut v, 5, 0x54);                     // sprites at 0xA800
    reg(&mut v, 12, 0x81);                    // H40
    reg(&mut v, 13, 0x2B);                    // H scroll at 0xAC00
    reg(&mut v, 16, 0x01);                    // 64x32
    reg(&mut v, 7, 0x00);
    v.cram[0] = 0x0000;
    v.cram[1] = 0x000E;                       // red
    v.cram[16 + 2] = 0x00E0;                  // green, palette 1
    v.cram[32 + 3] = 0x0E00;                  // blue, palette 2
    v.cram[48 + 4] = 0x0EEE;                  // white, palette 3
    solid_tile(&mut v, 1, 1);
    solid_tile(&mut v, 2, 2);
    solid_tile(&mut v, 3, 3);
    solid_tile(&mut v, 4, 4);
    // plane B: red everywhere
    for y in 0..32 { for x in 0..64 { name(&mut v, 0xE000, 64, x, y, 1); } }
    // plane A: green tile at cell (2, 0), high priority at cell (3, 0)
    name(&mut v, 0xC000, 64, 2, 0, 0x2000 | 2);
    name(&mut v, 0xC000, 64, 3, 0, 0x8000 | 0x2000 | 2);
    // sprite 0: 1x1 blue at screen (16, 0) .. covers cells 2 and 3 when 2 wide: make it 2x1
    let sat = 0xA800usize;
    let spr = [128u16, 0x0400, 0x4000 | 3, 128 + 16];
    // through the data port, so the VDP's sprite cache sees it
    reg(&mut v, 15, 2);
    set_addr(&mut v, 1, sat as u16);
    for w in spr { v.data_write(w); }
    // sprite tiles are column-major: a 2x1 sprite uses tiles 3 and 4
    v.render_line(0);
    assert_eq!(px(&v, 0, 0), [255, 0, 0, 255], "plane B");
    assert_eq!(px(&v, 16, 0), [0, 0, 255, 255], "low sprite over low plane A");
    assert_eq!(px(&v, 24, 0), [0, 255, 0, 255], "high plane A over low sprite");

    // H scroll moves plane B and A: scroll A right by 8 so the green cell starts at x = 24
    v.vram[0xAC00] = 0; v.vram[0xAC01] = 8;
    v.render_line(0);
    assert_eq!(px(&v, 16, 0), [0, 0, 255, 255]);
    assert_eq!(px(&v, 33, 0), [0, 255, 0, 255], "high cell moved to 32-39");

    // window over the left 16 pixels showing tile 4 (white, palette 3)
    reg(&mut v, 17, 0x01);
    name(&mut v, 0xB000, 64, 0, 0, 0x6000 | 4);
    v.render_line(0);
    assert_eq!(px(&v, 0, 0), [255, 255, 255, 255], "window");
    assert_eq!(px(&v, 8, 0), [255, 0, 0, 255], "window cell 1 is empty: plane B shows");

    // shadow: everything low priority is darker
    reg(&mut v, 17, 0x00);
    reg(&mut v, 12, 0x89);
    v.render_line(0);
    assert_eq!(px(&v, 0, 0), [128, 0, 0, 255], "shadowed red");
    // H32 stretches 256 pixels to 320
    reg(&mut v, 12, 0x00);
    v.render_line(0);
    assert_eq!(px(&v, 319, 0), [255, 0, 0, 255]);
}

#[test]
fn three_button_pad() {
    let mut b = bus_with_rom();
    b.io.pads[0].buttons = PAD_A | PAD_C | PAD_UP;
    b.write_byte(0xA10009, 0x40);            // TH is an output
    b.write_byte(0xA10003, 0x40);            // TH high
    assert_eq!(b.read_byte(0xA10003), 0x40 | 0x10 | 0x0E, "C and Up pressed");
    b.write_byte(0xA10003, 0x00);            // TH low
    assert_eq!(b.read_byte(0xA10003), 0x20 | 0x02, "A and Up pressed, start not");
    assert_eq!(b.read_byte(0xA10001), 0xA0, "US NTSC console");
}

#[test]
fn z80_bus_arbitration() {
    let mut b = bus_with_rom();
    assert_eq!(b.read_byte(0xA11100) & 1, 1, "Z80 in reset: bus not granted");
    b.write_word(0xA11200, 0x0100);          // out of reset
    b.write_word(0xA11100, 0x0100);          // ask for the bus
    assert_eq!(b.read_word(0xA11100) & 0x0100, 0, "granted");
    b.write_byte(0xA00010, 0x5A);
    assert_eq!(b.zram[0x10], 0x5A);
    assert_eq!(b.read_byte(0xA00010), 0x5A);
    b.write_word(0xA11100, 0x0000);
    assert!(b.z80_running());
}

fn w16(rom: &mut [u8], a: usize, words: &[u16]) {
    for (k, w) in words.iter().enumerate() { rom[a + 2 * k] = (w >> 8) as u8; rom[a + 2 * k + 1] = *w as u8; }
}

fn test_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x20000];
    w16(&mut rom, 0, &[0x00FF, 0xFE00, 0x0000, 0x0200]);      // SSP FFFE00, PC 200
    w16(&mut rom, 30 * 4, &[0x0000, 0x0300]);                 // level 6 autovector
    rom[0x100..0x110].copy_from_slice(b"SEGA GENESIS    ");
    rom[0x1F0] = b'U';
    w16(&mut rom, 0x200, &[
        0x33FC, 0x8164, 0x00C0, 0x0004,      // MOVE.W #$8164,$C00004  display + VINT on
        0x33FC, 0x0100, 0x00A1, 0x1200,      // MOVE.W #$100,$A11200   Z80 out of reset
        0x46FC, 0x2000,                      // MOVE #$2000,SR
        0x60FE,                              // BRA *
    ]);
    w16(&mut rom, 0x300, &[0x5279, 0x00FF, 0x0000, 0x4E73]);  // ADDQ.W #1,$FF0000; RTE
    rom
}

#[test]
fn boots_counts_vblanks_and_runs_the_z80() {
    let rom = test_rom();
    let g = crate::common::rom_detect::detect(&rom, "x.bin");
    assert_eq!(g[0].system, "md");
    let mut s = GenesisSystem::from_rom(&rom, "test", &[]).unwrap();
    assert_eq!(s.title, "");
    // the Z80 program: LD A,$42; LD ($1000),A; JR $
    s.bus.zram[..7].copy_from_slice(&[0x3E, 0x42, 0x32, 0x00, 0x10, 0x18, 0xFE]);
    for _ in 0..3 { s.run_frame(0); }
    assert_eq!((s.bus.ram[0] as u16) << 8 | s.bus.ram[1] as u16, 3, "one VINT a frame");
    assert_eq!(s.bus.zram[0x1000], 0x42, "the Z80 ran once released");
    let audio = s.drain_audio();
    assert!(audio.len() > 2 * 3 * 700, "{}", audio.len());
}

#[test]
fn save_state_round_trip() {
    let mut s = GenesisSystem::from_rom(&test_rom(), "test", &[]).unwrap();
    s.run_frame(0);
    let snap = s.save_state();
    s.run_frame(0);
    let after = s.save_state();
    s.load_state(&snap).unwrap();
    s.run_frame(0);
    assert_eq!(s.save_state(), after);
}

#[test]
fn smd_dumps_and_headers() {
    let rom = test_rom();
    // build an SMD file: 512-byte header, then per 16 KB block odd bytes then even bytes
    let mut smd = vec![0u8; 512];
    smd[8] = 0xAA; smd[9] = 0xBB;
    for block in rom.chunks(0x4000) {
        smd.extend(block.iter().skip(1).step_by(2));
        smd.extend(block.iter().step_by(2));
    }
    assert!(md_cart::is_smd(&smd));
    assert_eq!(md_cart::normalize(&smd), rom);
    let mut r = rom.clone();
    r[0x1B0..0x1B2].copy_from_slice(b"RA");
    r[0x1B4..0x1BC].copy_from_slice(&[0x00, 0x20, 0x00, 0x01, 0x00, 0x20, 0x3F, 0xFF]);
    r[0x150..0x156].copy_from_slice(b"SONIC ");
    r[0x1F0..0x1F3].copy_from_slice(b"J  ");
    let h = md_cart::parse_header(&r);
    assert_eq!(h.sram, Some((0x200001, 0x203FFF)));
    assert_eq!(h.region, md_cart::Region::Japan);
    assert_eq!(h.title, "SONIC");
    // backup RAM shows up at its address and is saved
    let mut s = GenesisSystem::from_rom(&r, "test", &[]).unwrap();
    assert!(s.has_battery());
    s.bus.write_byte(0x200001, 0x99);
    assert_eq!(s.bus.read_byte(0x200001), 0x99);
    assert!(s.is_sram_dirty());
}

#[test]
fn ym2612_plays_through_the_68000_window() {
    let mut s = GenesisSystem::from_rom(&test_rom(), "test", &[]).unwrap();
    s.run_frame(0);
    s.drain_audio();
    // the 68000 takes the Z80 bus and programs a sine-ish note on channel 1
    s.bus.write_word(0xA11100, 0x0100);
    let mut ym = |a: u8, d: u8| { s.bus.write_byte(0xA04000, a); s.bus.write_byte(0xA04001, d); };
    ym(0xB0, 0x07);                          // algorithm 7: all four operators audible
    ym(0xB4, 0xC0);
    for k in 0..4 {
        ym(0x30 + 4 * k, 0x01);              // MUL 1
        ym(0x40 + 4 * k, 0x10);              // loud
        ym(0x50 + 4 * k, 0x1F);              // instant attack
        ym(0x60 + 4 * k, 0x00);
        ym(0x70 + 4 * k, 0x00);
        ym(0x80 + 4 * k, 0x0F);
    }
    ym(0xA4, 0x22);                          // block 4
    ym(0xA0, 0x69);
    ym(0x28, 0xF0);                          // key on, channel 1
    for _ in 0..3 { s.run_frame(0); }
    let audio = s.drain_audio();
    let peak = audio.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    assert!(audio.len() > 2 * 3 * 700, "{}", audio.len());
    assert!(peak > 0.2, "FM is audible: peak {peak}");
    // the Z80 sees the same chip: its status port reads back (no timers running)
    assert_eq!(s.bus.read_byte(0xA04000), 0);
}

/// Sonic 2's case: the 68000's vblank handler takes the Z80 bus at once and holds it for
/// a few lines, but the Z80 must still have taken its own vblank interrupt (which drives
/// the music) in the moment before the bus request lands.
#[test]
fn z80_gets_its_vblank_interrupt_while_the_68000_takes_the_bus() {
    let mut rom = test_rom();
    w16(&mut rom, 0x300, &[
        0x33FC, 0x0100, 0x00A1, 0x1100,      // MOVE.W #$100,$A11100   take the Z80 bus
        0x303C, 0x012C,                      // MOVE.W #300,D0
        0x51C8, 0xFFFE,                      // DBRA D0,*              (several lines)
        0x33FC, 0x0000, 0x00A1, 0x1100,      // MOVE.W #0,$A11100      give it back
        0x4E73,                              // RTE
    ]);
    let mut s = GenesisSystem::from_rom(&rom, "test", &[]).unwrap();
    let z80: &[u8] = &[0xF3, 0xED, 0x56, 0x31, 0x00, 0x20, 0xFB, 0x18, 0xFE];   // DI; IM 1; LD SP,$2000; EI; JR $
    s.bus.zram[..z80.len()].copy_from_slice(z80);
    let isr: &[u8] = &[0x21, 0x00, 0x10, 0x34, 0xFB, 0xED, 0x4D];               // LD HL,$1000; INC (HL); EI; RETI
    s.bus.zram[0x38..0x38 + isr.len()].copy_from_slice(isr);
    for _ in 0..4 { s.run_frame(0); }
    assert_eq!(s.bus.zram[0x1000], 4, "one Z80 interrupt a frame");
}

#[test]
fn sprite_cache_keeps_y_size_and_link() {
    let mut v = Vdp::new();
    reg(&mut v, 1, 0x44);
    reg(&mut v, 5, 0x54);                     // sprites at 0xA800
    reg(&mut v, 12, 0x81);
    reg(&mut v, 15, 2);
    v.cram[17] = 0x000E;                      // palette 1 colour 1: red
    solid_tile(&mut v, 1, 1);
    set_addr(&mut v, 1, 0xA800);
    for w in [128u16 + 10, 0x0000, 0x2000 | 1, 128 + 50] { v.data_write(w); }   // 1x1 at (50, 10)
    v.render_line(10);
    assert_eq!(px(&v, 50, 10), [255, 0, 0, 255]);
    // a change that bypasses the ports (as if the cache had been loaded elsewhere) is
    // invisible: the cached Y stays
    v.vram[0xA801] = 128 + 20;
    v.render_line(10);
    assert_eq!(px(&v, 50, 10), [255, 0, 0, 255], "cached Y");
    // the X word isn't cached: it moves
    set_addr(&mut v, 1, 0xA806);
    v.data_write(128 + 100);
    v.render_line(10);
    assert_eq!(px(&v, 100, 10), [255, 0, 0, 255]);
}

#[test]
fn six_button_pad_protocol() {
    let mut b = bus_with_rom();
    b.io.pads[0].six_button = true;
    b.io.pads[0].buttons = PAD_X | PAD_MODE | PAD_UP;
    b.write_byte(0xA10009, 0x40);
    let mut seq = Vec::new();
    for _ in 0..4 {
        b.write_byte(0xA10003, 0x40);
        seq.push(b.read_byte(0xA10003) & 0x7F);
        b.write_byte(0xA10003, 0x00);
        seq.push(b.read_byte(0xA10003) & 0x7F);
    }
    assert_eq!(seq[0], 0x7E, "TH high: C B R L D U, up pressed");
    assert_eq!(seq[1], 0x32, "TH low: S A 0 0 D U");
    assert_eq!(seq[5], 0x30, "3rd TH low: up and down low = 6-button ID");
    assert_eq!(seq[6], 0x40 | 0x30 | 0x02 | 0x01, "4th TH high: C B M X Y Z with mode and X pressed");
    assert_eq!(seq[7], 0x3F, "4th TH low: all high");
    // after 1.5 ms without TH changes it starts over
    b.io.now += 30;
    b.write_byte(0xA10003, 0x40);
    assert_eq!(b.read_byte(0xA10003) & 0x7F, 0x7E);
    b.write_byte(0xA10003, 0x00);
    assert_eq!(b.read_byte(0xA10003) & 0x7F, 0x32);
    // as a 3-button pad the 3rd TH low is ordinary
    b.io.now += 30;
    b.io.pads[0].six_button = false;
    let mut last = 0;
    for _ in 0..3 {
        b.write_byte(0xA10003, 0x40);
        b.write_byte(0xA10003, 0x00);
        last = b.read_byte(0xA10003) & 0x7F;
    }
    assert_eq!(last, 0x32);
}

#[test]
fn pal_timing_and_v30() {
    let mut rom = test_rom();
    rom[0x1F0..0x1F3].copy_from_slice(b"E  ");
    let mut s = GenesisSystem::from_rom(&rom, "test", &[]).unwrap();
    assert!(s.bus.vdp.pal);
    assert_eq!(s.bus.read_byte(0xA10001), 0xE0, "overseas PAL console");
    assert_eq!(s.frame_time_usec(), 313 * 3420 * 1_000_000 / 53_203_424);
    assert_eq!(s.display_info().visible_height, 240);
    assert_eq!(s.bus.vdp.status() & 1, 1, "PAL status bit");
    // options can force a region and the 3-button pad
    let s2 = GenesisSystem::from_rom(&rom, "test", b"MDOP\x01\x01").unwrap();
    assert!(!s2.bus.vdp.pal);
    assert!(!s2.bus.io.pads[0].six_button);
    // V30: 240 lines, vblank at 240, V counter 0x10A then 0x1D2
    let v = &mut s.bus.vdp;
    reg(v, 1, 0x4C);
    assert_eq!(v.vblank_line(), 240);
    v.begin_line(0x10A);
    assert_eq!(v.hv_counter() >> 8, 0x0A);
    v.begin_line(0x10B);
    assert_eq!(v.hv_counter() >> 8, 0xD2);
    // V28 on PAL: the picture moves down 8 rows, with the backdrop above it
    reg(v, 1, 0x44);
    reg(v, 7, 0x01);
    v.cram[1] = 0x00E0;
    v.cram[0] = 0;
    v.render_line(0);
    assert_eq!(px(v, 0, 0), [0, 255, 0, 255], "border");
    assert_eq!(px(v, 0, 239), [0, 255, 0, 255], "border");
    assert_eq!(px(v, 0, 8), [0, 255, 0, 255], "line 0 (empty planes show the backdrop)");
}

#[test]
fn interlace_mode_2_uses_8x16_tiles() {
    let mut v = Vdp::new();
    reg(&mut v, 1, 0x44);
    reg(&mut v, 4, 0x07);                     // plane B at 0xE000
    reg(&mut v, 12, 0x87);                    // H40, interlace mode 2
    v.cram[1] = 0x000E;
    v.cram[2] = 0x0E00;
    // tile 1 = 64 bytes: rows 0-7 colour 1, rows 8-15 colour 2
    for i in 0..32 { v.vram[64 + i] = 0x11; v.vram[96 + i] = 0x22; }
    for x in 0..64 { name(&mut v, 0xE000, 64, x, 0, 1); }
    // each display line is two of the 448: line 3 = rows 6/7, line 4 = rows 8/9
    v.render_line(3);
    v.render_line(4);
    assert_eq!(px(&v, 0, 3), [255, 0, 0, 255]);
    assert_eq!(px(&v, 0, 4), [0, 0, 255, 255]);
}

/// Bit-bang a 24C01-style transfer through the Sega board wiring (SDA = D0, SCL = D1).
struct I2c<'a>(&'a mut MdBus);
impl I2c<'_> {
    fn lines(&mut self, sda: u8, scl: u8) { self.0.write_byte(0x200001, scl << 1 | sda); }
    fn sda(&mut self) -> u8 { self.0.read_byte(0x200001) & 1 }
    fn start(&mut self) { self.lines(1, 1); self.lines(0, 1); self.lines(0, 0); }
    fn stop(&mut self) { self.lines(0, 0); self.lines(0, 1); self.lines(1, 1); }
    fn bit(&mut self, b: u8) -> u8 { self.lines(b, 0); self.lines(b, 1); let r = self.sda(); self.lines(b, 0); r }
    fn byte_out(&mut self, v: u8) -> u8 {
        for i in (0..8).rev() { self.bit((v >> i) & 1); }
        self.bit(1)                                  // the chip's ACK
    }
    fn byte_in(&mut self, ack: bool) -> u8 {
        let mut v = 0;
        for _ in 0..8 { v = v << 1 | self.bit(1); }
        self.bit(if ack { 0 } else { 1 });
        v
    }
}

#[test]
fn serial_eeprom_saves() {
    let psg = crate::common::audio::sn76489::Sn76489::new(crate::common::audio::sn76489::Sn76489Variant::Sega, 3_579_545, 44_100);
    let mut rom = vec![0u8; 0x80000];
    rom[0x180..0x18E].copy_from_slice(b"GM G-4060 -00 ");          // Wonder Boy in Monster World
    let mut b = MdBus::new(rom, None, 0xA0, psg);
    assert!(b.eeprom.is_some());
    let mut i = I2c(&mut b);
    // X24C01: START, 7-bit word address 0x05 + write, two data bytes, STOP
    i.start();
    assert_eq!(i.byte_out(0x05 << 1), 0, "ACK");
    i.byte_out(0x12);
    i.byte_out(0x34);
    i.stop();
    // read them back: address + read, two bytes
    i.start();
    i.byte_out(0x05 << 1 | 1);
    let a = i.byte_in(true);
    let c = i.byte_in(false);
    i.stop();
    assert_eq!((a, c), (0x12, 0x34));
    let e = b.eeprom.as_ref().unwrap();
    assert_eq!(&e.data[5..7], &[0x12, 0x34]);
    assert!(e.dirty);
}

#[test]
fn rom_mirroring_and_quackshot_layout() {
    // a 256 KB ROM repeats through the cartridge space
    let psg = || crate::common::audio::sn76489::Sn76489::new(crate::common::audio::sn76489::Sn76489Variant::Sega, 3_579_545, 44_100);
    let mut rom = vec![0u8; 0x40000];
    rom[0x10] = 0x5A;
    let mut b = MdBus::new(rom, None, 0xA0, psg());
    assert_eq!(b.read_byte(0x40010), 0x5A);
    assert_eq!(b.read_byte(0xC0010), 0x5A);
    // QuackShot Rev A: lower 256 KB at 000000-0FFFFF, upper 256 KB at 100000-1FFFFF
    let mut q = vec![0u8; 0x80000];
    q[0x180..0x18E].copy_from_slice(b"GM 00004054-01");
    q[0x18E] = 0xA4;
    q[0x18F] = 0xB3;
    q[0x40000] = 0x77;
    let q = md_cart::normalize(&q);
    assert_eq!(q.len(), 0x200000);
    let mut b = MdBus::new(q, None, 0xA0, psg());
    assert_eq!(b.read_byte(0x100000), 0x77, "upper half");
    assert_eq!(b.read_byte(0x1C0000), 0x77, "repeats");
    assert_eq!(b.read_byte(0x040180), b'G', "lower half repeats");
    assert_eq!(b.read_byte(0x040000), 0x00, "not the upper half");
}
