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
    let sat = 0xA800;
    let spr = [128u16, 0x0400, 0x4000 | 3, 128 + 16];
    for (k, w) in spr.iter().enumerate() { v.vram[sat + 2 * k] = (w >> 8) as u8; v.vram[sat + 2 * k + 1] = *w as u8; }
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
