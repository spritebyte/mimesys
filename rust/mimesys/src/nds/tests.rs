use super::nds_bus::*;
use super::nds_cart::{Save, SaveType};
use super::nds_io::*;
use super::nds_rtc::Rtc;
use super::nds_spi::{self, Spi};
use super::nds_system::NdsSystem;
use super::nds_vram::{Region, Vram};
use crate::common::system::System;

const HW_TEST: &[u8] = include_bytes!("tests_data/hw_test.nds");

fn word(n: &Nds, a: u32) -> u32 {
    let i = (a & 0x3F_FFFF) as usize;
    u32::from_le_bytes([n.main_ram[i], n.main_ram[i + 1], n.main_ram[i + 2], n.main_ram[i + 3]])
}

/// The assembled test program (tools/nds_tests/build.py) on both CPUs.
#[test]
fn hw_test_rom() {
    assert_eq!(crate::common::rom_detect::detect(HW_TEST, "x.bin")[0].system, "nds");
    let mut s = NdsSystem::from_rom(HW_TEST, "hw_test", &[]).unwrap();
    for _ in 0..5 { s.run_frame(0); }
    let n = &s.nds;
    let r = 0x0210_0000;
    assert_eq!(word(n, r), 0x1111_1111, "ARM9 ran");
    assert_eq!(word(n, r + 0x30), 0xD0D0_D0D0, "ARM7 ran to the end");
    assert_eq!(word(n, r + 4), 0x1234_5679, "IPC FIFO round trip");
    assert_eq!(word(n, r + 0x34), 5, "IPCSYNC");
    assert_eq!((word(n, r + 8), word(n, r + 12)), (14, 2), "divider");
    assert_eq!(word(n, r + 16), 1000, "square root");
    assert!((3..=5).contains(&word(n, r + 0x14)), "vblank IRQs through the BIOS: {}", word(n, r + 0x14));
    assert!(word(n, r + 0x20) > 100, "timer counted: {}", word(n, r + 0x20));
    assert_eq!(word(n, r + 0x24), b'm' as u32, "firmware user name over SPI");
    assert_eq!(word(n, r + 0x28), 0xCAFE_F00D, "card read B7");
    assert_eq!(word(n, r + 0x2C), 128, "0x200 bytes");
    // the header copy the firmware leaves
    assert_eq!(word(n, 0x027F_FE0C), u32::from_le_bytes(*b"MMTS"));
    // bank A in red, shown on the top screen (engine A on top)
    let fb = s.framebuffer();
    assert_eq!(&fb[0..4], &[255, 0, 0, 255]);
    assert_eq!(&fb[(191 * 256 + 255) * 4..(191 * 256 + 255) * 4 + 4], &[255, 0, 0, 255]);
    assert_eq!(&fb[(192 * 256) * 4..(192 * 256) * 4 + 4], &[255, 255, 255, 255], "engine B: display off = white");
    // save states come back
    let snap = s.save_state();
    s.run_frame(0);
    let after = s.save_state();
    s.load_state(&snap).unwrap();
    s.run_frame(0);
    assert_eq!(s.save_state(), after);
}

#[test]
fn vram_bank_mapping() {
    let mut v = Vram::new();
    v.set_cnt(0, 0x80);                       // A: LCDC
    v.write16(Region::Lcdc, 0x10, 0xBEEF);
    assert_eq!(v.read16(Region::Lcdc, 0x10), 0xBEEF);
    v.set_cnt(0, 0x81);                       // A: engine A BG at offset 0
    assert_eq!(v.read16(Region::ABg, 0x10), 0xBEEF);
    assert_eq!(v.read16(Region::Lcdc, 0x10), 0, "gone from LCDC");
    v.set_cnt(1, 0x81);                       // B: also engine A BG offset 0: the two OR together
    v.banks[1][0x10] = 0x01;
    assert_eq!(v.read8(Region::ABg, 0x10), 0xEF | 0x01);
    v.set_cnt(2, 0x82);                       // C: ARM7 memory
    v.set_cnt(3, 0x8A);                       // D: ARM7 memory, offset 1
    assert_eq!(v.arm7_stat(), 3);
    v.write32(Region::Arm7, 0x20004, 0x1234_5678);
    assert_eq!(&v.banks[3][4..8], &[0x78, 0x56, 0x34, 0x12]);
    v.set_cnt(8, 0x82);                       // I: engine B sprites
    v.write16(Region::BObj, 0x3FFE, 0x7FFF);
    assert_eq!(v.read16(Region::BObj, 0x3FFE), 0x7FFF);
}

#[test]
fn divider_and_square_root() {
    let mut m = Math::default();
    m.numer = (-100i64) as u64;
    m.denom = 7;
    m.divcnt = 2;
    m.divide();
    assert_eq!((m.quot as i64, m.rem as i64), (-14, -2));
    m.denom = 0;
    m.divcnt = 0;
    m.numer = 5;
    m.divide();
    assert_eq!(m.quot, 0x0000_0000_FFFF_FFFF, "32-bit divide by zero: -1 with the upper word inverted");
    assert_eq!(m.rem, 5);
    assert!(m.divcnt & 0x4000 != 0);
    m.sqrt_param = 0xFFFF_FFFF_FFFF_FFFF;
    m.sqrtcnt = 1;
    m.sqrt();
    assert_eq!(m.sqrt_result, 0xFFFF_FFFF);
}

#[test]
fn timers_overflow_and_cascade() {
    let mut t = Timers::default();
    t.write(0, 0xFFF0 | 0x00C0_0000, 0xFFFF_FFFF);    // reload FFF0, enable, IRQ, prescaler 1
    t.write(1, 0x0084_0000, 0xFFFF_0000);              // count-up, enable
    let irq = t.update(16);
    assert_eq!(irq, IRQ_TIMER0, "timer 0 overflowed after 16 cycles");
    assert_eq!(t.read(0) & 0xFFFF, 0xFFF0);
    assert_eq!(t.read(1) & 0xFFFF, 1, "timer 1 counted the overflow");
    t.update(16 + 16 * 3);
    assert_eq!(t.read(1) & 0xFFFF, 4);
    // prescaler 64
    let mut t = Timers::default();
    t.write(2, 0x0081_0000, 0xFFFF_FFFF);
    t.update(64 * 10 + 63);
    assert_eq!(t.read(2) & 0xFFFF, 10);
}

#[test]
fn ipc_fifo_irqs_and_errors() {
    let mut ipc = Ipc::default();
    assert_eq!(ipc.write_cnt(ARM7, 0x8400, 0xFFFF), 0, "receive IRQ on, nothing to receive yet");
    ipc.write_cnt(ARM9, 0x8004, 0xFFFF);
    let (_, theirs) = ipc.send(ARM9, 0xAA);
    assert_eq!(theirs, IRQ_IPC_RECV, "ARM7 told a word arrived");
    let (v, _, theirs) = ipc.recv(ARM7);
    assert_eq!(v, 0xAA);
    assert_eq!(theirs, IRQ_IPC_SEND_EMPTY, "ARM9's send FIFO emptied");
    let (v, _, _) = ipc.recv(ARM7);
    assert_eq!(v, 0xAA, "empty: the last word again");
    assert!(ipc.read_cnt(ARM7) & (1 << 14) != 0, "and the error flag");
    for i in 0..17 { ipc.send(ARM9, i); }
    assert!(ipc.read_cnt(ARM9) & 0x4002 == 0x4002, "full, and an error for the 17th");
}

#[test]
fn rtc_reads_date_and_time() {
    let mut rtc = Rtc::new();
    rtc.fixed_time = Some(1_761_912_000 + 13 * 3600 + 5 * 60 + 9);   // 2025-10-31 12:00 UTC + 13:05:09
    // bit-bang command 0x26 | 0x80 (read date/time), LSB first
    let mut io = |rtc: &mut Rtc, v: u16| rtc.write(v, 0xFFFF);
    io(&mut rtc, 0x0000);
    io(&mut rtc, 0x0004);                          // select
    let cmd = 0xA6u8;
    for b in 0..8 {
        let d = (cmd >> b) as u16 & 1;
        io(&mut rtc, 0x0014 | d);                  // clock low, write
        io(&mut rtc, 0x0016 | d);                  // clock high
    }
    let mut bytes = [0u8; 7];
    for byte in bytes.iter_mut() {
        for b in 0..8 {
            io(&mut rtc, 0x0004);                  // clock low, read
            *byte |= ((rtc.io & 1) as u8) << b;
            io(&mut rtc, 0x0006);
        }
    }
    // 2025-11-01 01:05:09, a Saturday (6); hour has the PM bit clear
    assert_eq!(bytes, [0x25, 0x11, 0x01, 0x06, 0x01, 0x05, 0x09]);
}

#[test]
fn save_chip_commands() {
    // 64 KB EEPROM: 2 address bytes
    let mut e = Save::new(SaveType::Eeprom, 0x10000);
    e.transfer(0x06, true);                        // write enable
    for (i, b) in [0x02, 0x12, 0x34, 0xAB, 0xCD].iter().enumerate() { e.transfer(*b, i == 4); }
    assert_eq!(&e.data[0x1234..0x1236], &[0xAB, 0xCD]);
    let got: Vec<u8> = [0x03, 0x12, 0x34, 0, 0].iter().enumerate().map(|(i, b)| e.transfer(*b, i == 4)).collect();
    assert_eq!(&got[3..], &[0xAB, 0xCD]);
    // writes without write-enable are ignored
    for (i, b) in [0x02, 0x00, 0x00, 0x55].iter().enumerate() { e.transfer(*b, i == 3); }
    assert_eq!(e.data[0], 0xFF);
    // 512-byte EEPROM: command bit 3 is address bit 8
    let mut s = Save::new(SaveType::Eeprom512, 512);
    s.transfer(0x06, true);
    for (i, b) in [0x0A, 0x10, 0x77].iter().enumerate() { s.transfer(*b, i == 2); }
    assert_eq!(s.data[0x110], 0x77);
    // flash: 3 address bytes, fast read with a dummy byte, status
    let mut f = Save::new(SaveType::Flash, 0x40000);
    f.transfer(0x06, true);
    assert_eq!([0x05, 0].iter().enumerate().map(|(i, b)| f.transfer(*b, i == 1)).last(), Some(0x02), "WEL");
    for (i, b) in [0x0A, 0x01, 0x02, 0x03, 0x99].iter().enumerate() { f.transfer(*b, i == 4); }
    let got: Vec<u8> = [0x0B, 0x01, 0x02, 0x03, 0, 0].iter().enumerate().map(|(i, b)| f.transfer(*b, i == 5)).collect();
    assert_eq!(got[5], 0x99);
    assert!(f.dirty);
}

#[test]
fn generated_firmware_and_touchscreen() {
    let fw = nds_spi::make_firmware();
    let u = &fw[nds_spi::USER_SETTINGS..];
    assert_eq!(u16::from_le_bytes([u[0x72], u[0x73]]), nds_spi::crc16(&u[..0x70], 0xFFFF));
    assert_eq!(u[0x06], b'm');
    let mut spi = Spi::new(fw);
    spi.write_cnt(0x8A00, 0xFFFF);                 // touchscreen, hold
    spi.touch = (100, 50, true);
    spi.write_data(0xD0);                          // X position
    spi.write_data(0);
    let hi = spi.data as u16;
    spi.write_cnt(0x8200, 0xFFFF);
    spi.write_data(0);
    let lo = spi.data as u16;
    let adc = (hi << 5) | (lo >> 3);
    assert_eq!(adc >> 4, 100, "ADC {adc:#x} maps back to x = 100 with the generated calibration");
}

/// Speed: frames per second with the ARM7 in a busy loop and the ARM9 waking for IRQs
/// (cargo test --release nds_speed -- --ignored --nocapture).
#[test]
#[ignore]
fn nds_speed() {
    let mut s = NdsSystem::from_rom(HW_TEST, "hw_test", &[]).unwrap();
    let t = std::time::Instant::now();
    for _ in 0..120 { s.run_frame(0); }
    let ms = t.elapsed().as_secs_f64() * 1000.0 / 120.0;
    println!("DS frame: {ms:.2} ms ({:.1}x real time)", 16.715 / ms);
}

// ---------------------------------------------------------------- 2D engines

/// A booted system with both engines powered (A on top) and nothing drawn.
fn blank_2d() -> NdsSystem {
    let mut s = NdsSystem::from_rom(HW_TEST, "hw_test", &[]).unwrap();
    s.nds.powcnt1 = 0x820F;
    s
}

fn io(n: &mut Nds, a: u32, v: u32) { n.io_write(ARM9, a, v, 0xFFFF_FFFF); }
fn io16(n: &mut Nds, a: u32, v: u16) { let sh = (a & 2) * 8; n.io_write(ARM9, a & !3, (v as u32) << sh, 0xFFFF << sh); }
fn pal(n: &mut Nds, i: usize, c: u16) { n.palette[i * 2..i * 2 + 2].copy_from_slice(&c.to_le_bytes()); }
fn px(n: &Nds, x: usize, y: usize) -> [u8; 3] { let i = (y * 256 + x) * 4; [n.framebuffer[i], n.framebuffer[i + 1], n.framebuffer[i + 2]] }
fn draw(n: &mut Nds, lines: usize) { for y in 0..lines { n.start_line(y as u16); n.render_line(y); } }
const RED: u16 = 0x001F;
const GREEN: u16 = 0x03E0;
const BLUE: u16 = 0x7C00;

#[test]
fn text_bg_engine_a_with_scroll_and_bases() {
    let mut s = blank_2d();
    let n = &mut s.nds;
    n.vram.set_cnt(0, 0x81);                                   // bank A: engine A BG
    // DISPCNT: mode 0, BG0 on, graphics; char base +64K, screen base +64K
    io(n, 0x0400_0000, 0x0901_0100);
    io16(n, 0x0400_0008, 0x0204);                              // BG0CNT: char block 1, screen block 2
    let (cb, sb) = (0x10000 + 0x4000, 0x10000 + 0x1000);
    for i in 0..32 { n.vram.write8(Region::ABg, cb + 32 + i, 0x11); }   // tile 1: colour 1 everywhere
    n.vram.write16(Region::ABg, sb, 1 | (2 << 12));            // map (0,0): tile 1, palette 2
    pal(n, 0, BLUE);
    pal(n, 2 * 16 + 1, RED);
    draw(n, 1);
    assert_eq!(px(n, 0, 0), [255, 0, 0]);
    assert_eq!(px(n, 8, 0), [0, 0, 255], "tile 0 is blank: backdrop");
    io16(n, 0x0400_0010, 4);                                   // scroll 4 right
    draw(n, 1);
    assert_eq!(px(n, 3, 0), [255, 0, 0]);
    assert_eq!(px(n, 4, 0), [0, 0, 255]);
}

#[test]
fn engine_b_extended_palette_on_the_bottom_screen() {
    let mut s = blank_2d();
    let n = &mut s.nds;
    n.vram.set_cnt(2, 0x84);                                   // C: engine B BG
    n.vram.set_cnt(7, 0x82);                                   // H: engine B BG extended palettes
    io(n, 0x0400_1000, 0x4001_0100);                           // mode 0, BG0, ext palettes
    io16(n, 0x0400_1008, 0x0180);                              // 256 colours, screen block 1
    for i in 0..64 { n.vram.write8(Region::BBg, 64 + i, 5); }
    n.vram.write16(Region::BBg, 0x800, 1 | (3 << 12));
    n.vram.banks[7][(3 * 256 + 5) * 2..(3 * 256 + 5) * 2 + 2].copy_from_slice(&GREEN.to_le_bytes());
    draw(n, 1);
    assert_eq!(px(n, 0, 192), [0, 255, 0], "slot 0, palette 3, colour 5");
    // engine A off (DISPCNT display mode 0) is white on top
    assert_eq!(px(n, 0, 0), [255, 255, 255]);
    // swap screens
    n.powcnt1 &= !0x8000;
    draw(n, 1);
    assert_eq!(px(n, 0, 0), [0, 255, 0]);
}

#[test]
fn direct_colour_bitmap_affine_and_brightness() {
    let mut s = blank_2d();
    let n = &mut s.nds;
    n.vram.set_cnt(0, 0x81);
    io(n, 0x0400_0000, 0x0001_0803);                           // mode 3, BG3 on
    io16(n, 0x0400_000E, 0x4084);                              // BG3: direct bitmap 256x256 at 0
    io16(n, 0x0400_0030, 0x100);                               // PA = 1
    io16(n, 0x0400_0036, 0x100);                               // PD = 1
    io(n, 0x0400_0038, 10 << 8);                               // X ref = 10
    for x in 0..256u32 { n.vram.write16(Region::ABg, (1 * 256 + x) * 2, if x == 12 { 0x8000 | RED } else { 0x8000 | GREEN }); }
    draw(n, 2);
    assert_eq!(px(n, 2, 1), [255, 0, 0], "texture x 12 at screen x 2, line 1 (the ref point stepped by PD)");
    assert_eq!(px(n, 3, 1), [0, 255, 0]);
    io16(n, 0x0400_006C, 0x8010);                              // master brightness: fully down
    draw(n, 2);
    assert_eq!(px(n, 2, 1), [0, 0, 0]);
}

#[test]
fn sprites_tile_bitmap_priority_and_blending() {
    let mut s = blank_2d();
    let n = &mut s.nds;
    n.vram.set_cnt(1, 0x82);                                   // B: engine A OBJ
    // OBJ on, tile 1D (32-byte boundary), bitmap 1D (128-byte boundary)
    io(n, 0x0400_0000, 0x0001_1050);
    pal(n, 0, BLUE);
    pal(n, 256 + 16 + 1, RED);                                 // OBJ palette 1, colour 1
    for i in 0..32 { n.vram.write8(Region::AObj, 2 * 32 + i, 0x11); }   // tile 2
    // sprite 0: 8x8 at (4, 0), tile 2, palette 1, priority 1
    let oam = |n: &mut Nds, k: usize, a: [u16; 3]| for (j, v) in a.iter().enumerate() { n.oam[k * 8 + j * 2..k * 8 + j * 2 + 2].copy_from_slice(&v.to_le_bytes()); };
    for k in 0..128 { oam(n, k, [0x0200, 0, 0]); }             // all hidden
    oam(n, 0, [0x0000, 4, 2 | (1 << 10) | (1 << 12)]);
    // sprite 1: 8x8 bitmap at (8, 0), alpha 15, tile 4 (1D: 4 * 128 bytes), priority 0
    oam(n, 1, [0x0C00, 8, 4 | (15 << 12)]);
    for i in 0..64u32 { n.vram.write16(Region::AObj, 4 * 128 + i * 2, 0x8000 | GREEN); }
    draw(n, 1);
    assert_eq!(px(n, 4, 0), [255, 0, 0]);
    assert_eq!(px(n, 8, 0), [0, 255, 0], "bitmap sprite in front (priority 0)");
    assert_eq!(px(n, 16, 0), [0, 0, 255]);
    // bitmap alpha 7 over the backdrop (2nd target): (green * 8 + blue * 8) / 16
    oam(n, 1, [0x0C00, 8, 4 | (7 << 12)]);
    io16(n, 0x0400_0050, 0x2000);
    draw(n, 1);
    let c = px(n, 8, 0);
    assert!(c[1] > 100 && c[2] > 100 && c[0] == 0, "blended: {c:?}");
}

#[test]
fn display_capture_writes_a_vram_bank() {
    let mut s = blank_2d();
    let n = &mut s.nds;
    n.vram.set_cnt(0, 0x81);
    n.vram.set_cnt(3, 0x80);                                   // D: LCDC, the capture target
    io(n, 0x0400_0000, 0x0001_0000);                           // graphics, no layers: backdrop only
    pal(n, 0, RED);
    // capture: source A, bank D, 256x192, enabled
    io(n, 0x0400_0064, 0x8033_0000);
    draw(n, 192);
    assert_eq!(n.vram.read16(Region::Lcdc, 0x60000), 0x8000 | RED);
    assert_eq!(n.vram.read16(Region::Lcdc, 0x60000 + (191 * 256 + 255) * 2), 0x8000 | RED);
    assert_eq!(u32::from_le_bytes(n.io9_raw[0x64..0x68].try_into().unwrap()) >> 31, 0, "busy bit clears");
}

/// Renderer speed with both engines busy: 4 text BGs and 128 sprites each
/// (cargo test --release nds_2d_speed -- --ignored --nocapture).
#[test]
#[ignore]
fn nds_2d_speed() {
    let mut s = blank_2d();
    let n = &mut s.nds;
    n.vram.set_cnt(0, 0x81); n.vram.set_cnt(1, 0x82); n.vram.set_cnt(2, 0x84); n.vram.set_cnt(3, 0x84);
    for e in 0..2u32 {
        let b = 0x0400_0000 + e * 0x1000;
        io(n, b, 0x0001_1F10);
        for i in 0..4 { io16(n, b + 8 + i * 2, (i as u16) | ((8 + i as u16) << 8)); }
    }
    for i in 0..0x800 { n.oam[i] = (i * 7) as u8 & 0xFC; }
    for i in 0..0x20000u32 { n.vram.write8(Region::ABg, i, (i * 13) as u8); n.vram.write8(Region::BBg, i, (i * 11) as u8); }
    let t = std::time::Instant::now();
    for _ in 0..60 { draw(n, 192); }
    let ms = t.elapsed().as_secs_f64() * 1000.0 / 60.0;
    println!("2D frame (both engines): {ms:.2} ms");
}

#[test]
fn save_type_table() {
    let code = |s: &[u8; 4]| u32::from_le_bytes(*s);
    assert_eq!(SaveType::for_game(code(b"A2DE")), Some((SaveType::Eeprom, 0x2000)), "New Super Mario Bros. (USA)");
    assert_eq!(SaveType::for_game(code(b"ASCE")), Some((SaveType::Eeprom, 0x2000)), "Sonic Rush (USA)");
    assert_eq!(SaveType::for_game(code(b"MMTS")), None, "the test ROM isn't listed");
    // 128 KB EEPROM: 3 address bytes
    let mut e = Save::new(SaveType::Eeprom, 0x20000);
    e.transfer(0x06, true);
    for (i, b) in [0x02, 0x01, 0x23, 0x45, 0x77].iter().enumerate() { e.transfer(*b, i == 4); }
    assert_eq!(e.data[0x12345], 0x77);
}

// ---------------------------------------------------------------- 3D engine

fn gx(n: &mut Nds, cmd: u32, params: &[u32]) {
    let a = 0x0400_0400 + cmd * 4;
    if params.is_empty() { io(n, a, 0); }
    for &p in params { io(n, a, p); }
}
fn v16(n: &mut Nds, x: f32, y: f32, z: f32) {
    let f = |v: f32| ((v * 4096.0) as i32 as u32) & 0xFFFF;
    gx(n, 0x23, &[f(x) | f(y) << 16, f(z)]);
}
/// 3D on, BG0 = 3D, identity matrices, full viewport, clear to transparent.
fn setup_3d(n: &mut Nds) {
    n.powcnt1 = 0x820F;
    io(n, 0x0400_0000, 0x0001_0108);                           // engine A: BG0 on, BG0 = 3D
    io(n, 0x0400_0060, 0);                                      // DISP3DCNT
    io(n, 0x0400_0350, 0);                                      // clear colour: alpha 0
    io(n, 0x0400_0354, 0x7FFF);                                 // clear depth: far
    for mode in [0, 1, 3] { gx(n, 0x10, &[mode]); gx(n, 0x15, &[]); }
    gx(n, 0x10, &[2]);
    gx(n, 0x60, &[0xBFFF_0000]);                                // viewport 0,0 - 255,191
}
fn frame3d(n: &mut Nds, x: usize, y: usize) -> (u16, u32) { let p = n.gpu3d.frame[y * 256 + x]; ((p & 0x7FFF) as u16, p >> 16) }
fn swap(n: &mut Nds) { gx(n, 0x50, &[0]); n.start_line(192); }

#[test]
fn flat_triangle_culling_and_the_bg0_layer() {
    let mut s = blank_2d();
    let n = &mut s.nds;
    setup_3d(n);
    gx(n, 0x29, &[0x001F_00C0]);                               // alpha 31, front + back
    gx(n, 0x40, &[0]);                                          // triangles
    gx(n, 0x20, &[RED as u32]);
    v16(n, -1.0, -1.0, 0.0); v16(n, 1.0, -1.0, 0.0); v16(n, 0.0, 1.0, 0.0);
    gx(n, 0x41, &[]);
    assert_eq!(n.gpu3d.read(0x604), 1 | 3 << 16, "RAM_COUNT: 1 polygon, 3 vertices");
    swap(n);
    assert_eq!(frame3d(n, 128, 150), (RED, 31), "inside");
    assert_eq!(frame3d(n, 2, 2).1, 0, "outside: clear alpha 0");
    // the 3D layer shows as BG0 on the top screen, the backdrop around it
    pal(n, 0, BLUE);
    draw(n, 192);
    assert_eq!(px(n, 128, 150), [255, 0, 0]);
    assert_eq!(px(n, 2, 2), [0, 0, 255]);
    // counter-clockwise (with y up) is the front: front-only keeps it, back-only drops it
    for (attr, drawn) in [(0x001F_0080u32, true), (0x001F_0040, false)] {
        gx(n, 0x29, &[attr]);
        gx(n, 0x40, &[0]);
        v16(n, -1.0, -1.0, 0.0); v16(n, 1.0, -1.0, 0.0); v16(n, 0.0, 1.0, 0.0);
        swap(n);
        assert_eq!(frame3d(n, 128, 150).1 != 0, drawn, "attr {attr:#x}");
    }
}

#[test]
fn packed_fifo_matrices_and_depth() {
    let mut s = blank_2d();
    let n = &mut s.nds;
    setup_3d(n);
    // packed: POLYGON_ATTR, BEGIN_VTXS, COLOR, VTX_16 x3 (two packets)
    let f = |v: f32| ((v * 4096.0) as i32 as u32) & 0xFFFF;
    let words = [
        0x2320_4029u32, 0x001F_00C0, 1, GREEN as u32,         // attr, begin(quads), colour, VTX_16 (first vertex)
        f(-1.0) | f(-1.0) << 16, f(0.5),
        0x0023_2323, f(1.0) | f(-1.0) << 16, f(0.5), f(1.0) | f(1.0) << 16, f(0.5), f(-1.0) | f(1.0) << 16, f(0.5),
    ];
    for w in words { io(n, 0x0400_0400, w); }
    // a red triangle in front (z = 0 is nearer than 0.5)
    gx(n, 0x40, &[0]);
    gx(n, 0x20, &[RED as u32]);
    v16(n, -1.0, -1.0, 0.0); v16(n, 1.0, -1.0, 0.0); v16(n, 0.0, 1.0, 0.0);
    swap(n);
    assert_eq!(frame3d(n, 128, 150), (RED, 31), "nearer triangle wins");
    assert_eq!(frame3d(n, 5, 5), (GREEN, 31), "the quad elsewhere");
    // translate: the clip matrix reads back with the translation in row 3
    gx(n, 0x1C, &[0x1000, 0, 0]);
    assert_eq!(n.gpu3d.read(0x640 + 12 * 4), 0x1000);
    gx(n, 0x11, &[]); gx(n, 0x15, &[]); gx(n, 0x12, &[1]);
    assert_eq!(n.gpu3d.read(0x640 + 12 * 4), 0x1000, "push, identity, pop restores");
}

#[test]
fn textured_quad_direct_colour() {
    let mut s = blank_2d();
    let n = &mut s.nds;
    setup_3d(n);
    io(n, 0x0400_0060, 1);                                      // texture mapping on
    n.vram.set_cnt(0, 0x83);                                    // bank A: texture slot 0
    // 8x8 direct-colour texture: left half red, right half blue
    for t in 0..8u32 { for s in 0..8u32 {
        n.vram.write16(Region::Texture, (t * 8 + s) * 2, 0x8000 | if s < 4 { RED } else { BLUE });
    } }
    gx(n, 0x2A, &[7 << 26]);                                    // direct colour, 8x8, at 0, no repeat
    gx(n, 0x29, &[0x001F_00C0]);
    gx(n, 0x40, &[1]);
    gx(n, 0x20, &[0x7FFF]);
    let st = |s: i32, t: i32| (s * 16) as u32 & 0xFFFF | ((t * 16) as u32) << 16;
    gx(n, 0x22, &[st(0, 8)]); v16(n, -1.0, -1.0, 0.0);
    gx(n, 0x22, &[st(8, 8)]); v16(n, 1.0, -1.0, 0.0);
    gx(n, 0x22, &[st(8, 0)]); v16(n, 1.0, 1.0, 0.0);
    gx(n, 0x22, &[st(0, 0)]); v16(n, -1.0, 1.0, 0.0);
    swap(n);
    assert_eq!(frame3d(n, 30, 96), (RED, 31), "white x red texel = red");
    assert_eq!(frame3d(n, 220, 96), (BLUE, 31));
}

#[test]
fn commands_after_swap_wait_for_vblank() {
    let mut s = blank_2d();
    let n = &mut s.nds;
    setup_3d(n);
    gx(n, 0x50, &[0]);
    gx(n, 0x20, &[GREEN as u32]);
    assert!(n.gpu3d.read(0x604) == 0 && n.gpu3d.fifo_len() == 1, "queued behind the swap");
    assert!(n.io_read(ARM9, 0x0400_0600) & (1 << 27) != 0, "GXSTAT busy");
    n.start_line(192);
    assert_eq!(n.gpu3d.fifo_len(), 0);
    assert!(n.io_read(ARM9, 0x0400_0600) & (1 << 27) == 0);
}

#[test]
fn lighting_diffuse_and_ambient() {
    let mut s = blank_2d();
    let n = &mut s.nds;
    setup_3d(n);
    gx(n, 0x29, &[0x001F_00C1]);                               // light 0 on
    gx(n, 0x40, &[0]);
    gx(n, 0x33, &[0x7FFF]);                                     // light 0 white
    let ten = |v: i32| (v as u32) & 0x3FF;
    gx(n, 0x32, &[ten(0) | ten(0) << 10 | ten(-511) << 20]);   // pointing into the screen (-z)
    gx(n, 0x30, &[0x001F]);                                     // diffuse red, no ambient
    gx(n, 0x31, &[0]);
    gx(n, 0x21, &[ten(0) | ten(0) << 10 | ten(511) << 20]);    // normal facing the viewer
    let lit = n.gpu3d.read_color();
    assert!(lit & 31 >= 29 && lit >> 5 == 0, "full diffuse red: {lit:#x}");
    gx(n, 0x21, &[ten(0) | ten(0) << 10 | ten(-511) << 20]);   // facing away
    assert_eq!(n.gpu3d.read_color(), 0, "unlit");
    gx(n, 0x30, &[0x0010 << 16]);                               // ambient red 16
    gx(n, 0x21, &[ten(0) | ten(0) << 10 | ten(-511) << 20]);
    assert_eq!(n.gpu3d.read_color(), 15, "ambient only (16 x 31/32)");
}

/// 3D speed: 2000 textured, overlapping triangles (about 12 screens of pixels)
/// (cargo test --release nds_3d_speed -- --ignored --nocapture).
#[test]
#[ignore]
fn nds_3d_speed() {
    let mut s = blank_2d();
    let n = &mut s.nds;
    setup_3d(n);
    io(n, 0x0400_0060, 9);                                      // textures + blending
    n.vram.set_cnt(0, 0x83);
    gx(n, 0x2A, &[7 << 26 | 3 << 16 | 3 << 20 | 3 << 23]);     // 64x64 direct, repeat
    let mut seed = 1u32;
    let mut rnd = || { seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345); ((seed >> 16) & 0x7FFF) as f32 / 32768.0 * 2.0 - 1.0 };
    let t = std::time::Instant::now();
    let mut render = 0.0;
    for _ in 0..10 {
        gx(n, 0x29, &[0x001F_00C0]);
        gx(n, 0x40, &[0]);
        for _ in 0..2000 {
            let (cx, cy, z) = (rnd(), rnd(), rnd() * 0.9);
            for k in 0..3 {
                gx(n, 0x22, &[((k * 512) as u32) | (((k * 256) as u32) << 16)]);
                v16(n, cx + [0.0, 0.2, 0.1][k], cy + [0.0, 0.0, 0.25][k], z);
            }
        }
        let r = std::time::Instant::now();
        swap(n);
        render += r.elapsed().as_secs_f64();
    }
    let ms = t.elapsed().as_secs_f64() * 1000.0 / 10.0;
    println!("3D frame (2000 triangles, about 12 screens of pixels): {ms:.2} ms, of which rendering {:.2} ms", render * 100.0);
}

#[test]
fn texture_palettes_above_32k() {
    // 16-colour texture whose palette sits at 0x9000 in texture-palette memory (bank E)
    let mut s = blank_2d();
    let n = &mut s.nds;
    setup_3d(n);
    io(n, 0x0400_0060, 1);
    n.vram.set_cnt(0, 0x83);                                    // A: texture slot 0
    n.vram.set_cnt(4, 0x83);                                    // E: texture palettes 0-64K
    for i in 0..32 { n.vram.write8(Region::Texture, i, 0x11); }  // 8x8, every texel colour 1
    n.vram.banks[4][0x9000 + 2..0x9000 + 4].copy_from_slice(&GREEN.to_le_bytes());
    gx(n, 0x2A, &[3 << 26]);                                    // 16 colours, 8x8 at 0
    gx(n, 0x2B, &[0x9000 / 16]);                                // PLTT_BASE in 16-byte units
    gx(n, 0x29, &[0x001F_00C0]);
    gx(n, 0x40, &[0]);
    gx(n, 0x20, &[0x7FFF]);
    v16(n, -1.0, -1.0, 0.0); v16(n, 1.0, -1.0, 0.0); v16(n, 0.0, 1.0, 0.0);
    swap(n);
    assert_eq!(frame3d(n, 128, 150), (GREEN, 31), "palette colour 1 from 0x9002");
}

#[test]
fn vtx_diff_adds_raw_units() {
    let mut s = blank_2d();
    let n = &mut s.nds;
    setup_3d(n);
    gx(n, 0x40, &[0]);
    v16(n, 0.5, 0.5, 0.5);                                      // 0x800 each
    let ten = |v: i32| (v as u32) & 0x3FF;
    gx(n, 0x28, &[ten(1) | ten(-2) << 10 | ten(511) << 20]);
    assert_eq!(n.gpu3d.last_vertex(), [0x801, 0x7FE, 0x800 + 511]);
}

#[test]
fn solid_3d_ignores_the_games_blend_setting() {
    let mut s = blank_2d();
    let n = &mut s.nds;
    setup_3d(n);
    pal(n, 0, BLUE);
    io16(n, 0x0400_0050, 0x2041);                               // alpha blend: BG0 over the backdrop
    io16(n, 0x0400_0052, 0x0808);                               // EVA = EVB = 8 (half and half)
    gx(n, 0x29, &[0x001F_00C0]);
    gx(n, 0x40, &[0]);
    gx(n, 0x20, &[RED as u32]);
    v16(n, -1.0, -1.0, 0.0); v16(n, 1.0, -1.0, 0.0); v16(n, 0.0, 1.0, 0.0);
    swap(n);
    draw(n, 192);
    assert_eq!(px(n, 128, 150), [255, 0, 0], "alpha 31 3D stays solid");
    // a 3D alpha of 15 does blend
    gx(n, 0x29, &[0x000F_00C0]);
    gx(n, 0x40, &[0]);
    v16(n, -1.0, -1.0, 0.0); v16(n, 1.0, -1.0, 0.0); v16(n, 0.0, 1.0, 0.0);
    swap(n);
    draw(n, 192);
    let c = px(n, 128, 150);
    assert!(c[0] > 100 && c[2] > 100, "half red, half blue: {c:?}");
}

#[test]
fn decal_shows_vertex_colour_through_transparent_texels() {
    let mut s = blank_2d();
    let n = &mut s.nds;
    setup_3d(n);
    io(n, 0x0400_0060, 1);
    n.vram.set_cnt(0, 0x83);
    // direct-colour texture, all texels transparent (bit 15 clear)
    gx(n, 0x2A, &[7 << 26]);
    gx(n, 0x29, &[0x001F_00D0]);                                // decal
    gx(n, 0x40, &[0]);
    gx(n, 0x20, &[GREEN as u32]);
    v16(n, -1.0, -1.0, 0.0); v16(n, 1.0, -1.0, 0.0); v16(n, 0.0, 1.0, 0.0);
    swap(n);
    assert_eq!(frame3d(n, 128, 150), (GREEN, 31));
    // modulate drops those pixels instead
    gx(n, 0x29, &[0x001F_00C0]);
    gx(n, 0x40, &[0]);
    v16(n, -1.0, -1.0, 0.0); v16(n, 1.0, -1.0, 0.0); v16(n, 0.0, 1.0, 0.0);
    swap(n);
    assert_eq!(frame3d(n, 128, 150).1, 0);
}

#[test]
fn firmware_wifi_blocks_and_ir_cart_save() {
    let fw = nds_spi::make_firmware();
    assert_eq!(u16::from_le_bytes([fw[0x2A], fw[0x2B]]), nds_spi::crc16(&fw[0x2C..0x2C + 0x138], 0), "wifi config CRC");
    for k in 0..3 {
        let ap = &fw[nds_spi::ACCESS_POINTS + k * 0x100..nds_spi::ACCESS_POINTS + (k + 1) * 0x100];
        assert_eq!(u16::from_le_bytes([ap[0xFE], ap[0xFF]]), nds_spi::crc16(&ap[..0xFE], 0), "access point {k} CRC");
        assert_eq!(ap[0xE7], 0xFF, "not configured");
    }
    // IR cart: the first byte of a command goes to the IR chip, 00 passes the rest through
    let mut rom = HW_TEST.to_vec();
    rom[0x0C] = b'I';
    let mut cart = super::nds_cart::Cart::new(rom, Save::new(SaveType::Flash, 0x80000));
    assert!(cart.ir);
    cart.write_auxspicnt(0xA040, 0xFFFF);                      // enabled, SPI mode, hold
    let mut cmd = |c: &mut super::nds_cart::Cart, bytes: &[u8]| -> Vec<u8> {
        bytes.iter().enumerate().map(|(i, &b)| {
            if i + 1 == bytes.len() { c.write_auxspicnt(0xA000, 0xFFFF); }   // drop hold for the last byte
            c.write_auxspidata(b);
            let r = c.auxspidata;
            if i + 1 == bytes.len() { c.write_auxspicnt(0xA040, 0xFFFF); }
            r
        }).collect()
    };
    cmd(&mut cart, &[0x00, 0x06]);                              // write enable
    cmd(&mut cart, &[0x00, 0x0A, 0x00, 0x01, 0x00, 0x5A]);       // page write 0x100 = 5A
    assert_eq!(cart.save.data[0x100], 0x5A);
    assert_eq!(cmd(&mut cart, &[0x08, 0x00])[1], 0xAA, "IR chip ID");
    assert_eq!(cmd(&mut cart, &[0x00, 0x03, 0x00, 0x01, 0x00, 0x00])[5], 0x5A, "read back through the IR chip");
}

#[test]
fn wifi_chip_startup() {
    let mut s = blank_2d();
    let n = &mut s.nds;
    let rd = |n: &mut Nds, a: u32| n.read16(ARM7, a, false, false);
    let wr = |n: &mut Nds, a: u32, v: u16| n.write16(ARM7, a, v, false);
    assert_eq!(rd(n, 0x0480_8000), 0xC340, "chip ID (DS Lite)");
    wr(n, 0x0400_0304, 0x0003);                                // POWCNT2: sound + wifi
    wr(n, 0x0480_8036, 0);                                     // W_POWER_US: clock on
    wr(n, 0x0480_8004, 1);                                     // W_MODE_RESET
    // baseband: write register 0x20, read it back; read-only register 0x00 reads 6D
    wr(n, 0x0480_815A, 0x42); wr(n, 0x0480_8158, 0x5020);
    wr(n, 0x0480_8158, 0x6020);
    assert_eq!(rd(n, 0x0480_815C), 0x42);
    wr(n, 0x0480_8158, 0x6000);
    assert_eq!(rd(n, 0x0480_815C), 0x6D);
    // wifi RAM
    wr(n, 0x0480_4010, 0xBEEF);
    assert_eq!(rd(n, 0x0480_C010), 0xBEEF, "mirrored");
    // microsecond counter runs while enabled
    wr(n, 0x0480_80E8, 1);
    n.cycles7 += 33_514 * 5;
    let us = rd(n, 0x0480_80F8);
    assert!((4999..=5001).contains(&us), "5 ms = {us} us");
    // forced off, then on: IRQ 11 at once, W_POWERSTATE settles to 0 after 2048 us
    wr(n, 0x0480_8012, 1 << 11);                               // W_IE
    n.irq[ARM7].ie |= 1 << 24;
    wr(n, 0x0480_8040, 0x8001);
    assert!(rd(n, 0x0480_803C) & 0x200 != 0, "off");
    wr(n, 0x0480_8040, 0x8000);
    assert!(n.irq[ARM7].iflags & (1 << 24) != 0, "wifi IRQ");
    assert!(rd(n, 0x0480_803C) & 0x100 != 0, "powering up");
    n.cycles7 += 33_514 * 3;
    assert_eq!(rd(n, 0x0480_803C), 0, "on");
}

/// The 10 s debug report runs (cargo test --release nds_report -- --ignored --nocapture).
#[test]
#[ignore]
fn nds_report() {
    let mut s = NdsSystem::from_rom(HW_TEST, "hw_test", &[]).unwrap();
    s.set_option("debug_report", 1);
    for _ in 0..600 { s.run_frame(0); }
}

#[test]
fn small_vram_banks_repeat() {
    let mut v = Vram::new();
    v.set_cnt(7, 0x81);                                        // H: engine B BG (32 KB)
    v.banks[7][0x10] = 0x11; v.banks[7][0x4010] = 0x22;
    assert_eq!(v.read8(Region::BBg, 0x10010), 0x11, "H again at 64 KB");
    assert_eq!(v.read8(Region::BBg, 0x14010), 0x22);
    assert_eq!(v.read8(Region::BBg, 0x08010), 0, "not at 32 KB");
    v.set_cnt(8, 0x82);                                        // I: engine B sprites (16 KB)
    v.banks[8][0x20] = 0x33;
    assert_eq!(v.read8(Region::BObj, 0x1C020), 0x33, "I at every 16 KB of sprite space");
    v.set_cnt(5, 0x81);                                        // F: engine A BG at 0 (16 KB)
    v.banks[5][0x30] = 0x44;
    assert_eq!(v.read8(Region::ABg, 0x8030), 0x44, "F again 32 KB up");
}

#[test]
fn firmware_flash_writes() {
    let mut spi = Spi::new(nds_spi::make_firmware());
    let mut cmd = |spi: &mut Spi, bytes: &[u8]| -> Vec<u8> {
        spi.write_cnt(0x8900, 0xFFFF);                         // firmware, hold
        bytes.iter().enumerate().map(|(i, &b)| {
            if i + 1 == bytes.len() { spi.write_cnt(0x8100, 0xFFFF); }
            spi.write_data(b);
            spi.data
        }).collect()
    };
    assert_eq!(cmd(&mut spi, &[0x05, 0])[1] & 2, 0);
    cmd(&mut spi, &[0x06]);
    assert_eq!(cmd(&mut spi, &[0x05, 0])[1] & 2, 2, "WEL after write enable");
    cmd(&mut spi, &[0x0A, 0x03, 0xFA, 0x00, 0x12, 0x34]);
    assert_eq!(cmd(&mut spi, &[0x05, 0])[1] & 2, 0, "WEL cleared by the write");
    let back = cmd(&mut spi, &[0x03, 0x03, 0xFA, 0x00, 0, 0]);
    assert_eq!(&back[4..], &[0x12, 0x34]);
    cmd(&mut spi, &[0x06]);
    cmd(&mut spi, &[0xDB, 0x03, 0xFA, 0x00]);
    assert_eq!(cmd(&mut spi, &[0x03, 0x03, 0xFA, 0x00, 0])[4], 0xFF, "page erased");
}

// ---------------------------------------------------------------- sound

fn snd(n: &mut Nds, a: u32, v: u32) { n.io_write(ARM7, a, v, 0xFFFF_FFFF); }
/// Run the sound unit for `ms` milliseconds; the 44.1 kHz stereo output.
fn play(n: &mut Nds, ms: u64) -> Vec<f32> {
    n.cycles7 += 33_514 * ms;
    n.sound_sync();
    n.sound.drain()
}

#[test]
fn sound_pcm16_one_shot_and_output_rate() {
    let mut s = blank_2d();
    let n = &mut s.nds;
    // 1000 PCM16 samples of +0x4000 in main RAM
    for i in 0..1000 { n.main_ram[0x1000 + i * 2..0x1002 + i * 2].copy_from_slice(&0x4000i16.to_le_bytes()); }
    snd(n, 0x0400_0500, 0x807F);                                // master on, volume 127
    snd(n, 0x0400_0404, 0x0200_1000);                           // SAD
    snd(n, 0x0400_0408, 0x10000 - 512);                         // TMR: one sample per mixer tick (32.7 kHz)
    snd(n, 0x0400_040C, 500);                                   // LEN: 500 words = 1000 samples
    snd(n, 0x0400_0400, 0x8000_0000 | 2 << 27 | 1 << 29 | 64 << 16 | 127);   // start, one-shot, PCM16, centre, vol 127
    let out = play(n, 10);
    assert!((440..=442).contains(&(out.len() / 2)), "10 ms at 44.1 kHz: {} frames", out.len() / 2);
    let (l, r) = (out[100], out[101]);
    assert!(l > 0.2 && (l - r).abs() < 1e-6, "centred: {l} {r}");
    assert!(n.io_read(ARM7, 0x0400_0400) & 0x8000_0000 != 0, "still playing at 10 ms");
    let out = play(n, 30);
    assert!(n.io_read(ARM7, 0x0400_0400) & 0x8000_0000 == 0, "one-shot finished (1000 samples = 30.6 ms)");
    assert_eq!(out[out.len() - 2], 0.0);
}

#[test]
fn sound_adpcm_psg_and_noise() {
    let mut s = blank_2d();
    let n = &mut s.nds;
    snd(n, 0x0400_0500, 0x807F);
    // ADPCM: header (value 0, index 0), then nibbles 7, 7, ...: +11 first (7/8 + 7/4 + 7/2 + 7)
    n.main_ram[0x2000..0x2004].copy_from_slice(&[0, 0, 0, 0]);
    for i in 0..16 { n.main_ram[0x2004 + i] = 0x77; }
    snd(n, 0x0400_0414, 0x0200_2000);
    snd(n, 0x0400_0418, 0x10000 - 512);
    snd(n, 0x0400_041C, 4);
    snd(n, 0x0400_0410, 0x8000_0000 | 1 << 27 | 2 << 29 | 127 | 3 << 8);    // loop, ADPCM, divider /16 off (shift 0)
    let ch = &n.sound.ch[1];
    assert_eq!(ch.cnt >> 29 & 3, 2);
    play(n, 1);
    // rising: it's loud and positive after a few samples
    let out = play(n, 1);
    assert!(out[0] > 0.0);
    // PSG on channel 8, 50% duty: a square wave
    snd(n, 0x0400_0410, 0);
    snd(n, 0x0400_0488, 0x10000 - 512);
    snd(n, 0x0400_0480, 0x8000_0000 | 3 << 29 | 3 << 24 | 64 << 16 | 127);
    let out = play(n, 2);
    let pos = out.iter().step_by(2).filter(|&&x| x > 0.0).count();
    let neg = out.iter().step_by(2).filter(|&&x| x < 0.0).count();
    assert!(pos > 20 && neg > 20, "square wave: {pos} up, {neg} down");
    // PSG duty on channel 0 makes no sound; noise on channel 14 does
    snd(n, 0x0400_0480, 0);
    snd(n, 0x0400_0400, 0x8000_0000 | 3 << 29 | 64 << 16 | 127);
    play(n, 1);                                                 // the resampler's last blend of the square wave
    assert!(play(n, 1).iter().all(|&x| x == 0.0));
    snd(n, 0x0400_04E8, 0x10000 - 512);
    snd(n, 0x0400_04E0, 0x8000_0000 | 3 << 29 | 64 << 16 | 127);
    let out = play(n, 2);
    assert!(out.iter().any(|&x| x > 0.0) && out.iter().any(|&x| x < 0.0), "noise");
}

#[test]
fn sound_capture_records_the_mix() {
    let mut s = blank_2d();
    let n = &mut s.nds;
    snd(n, 0x0400_0500, 0x807F);
    // channel 0: constant PCM8 0x40, panned hard left
    for i in 0..64 { n.main_ram[0x3000 + i] = 0x40; }
    snd(n, 0x0400_0404, 0x0200_3000);
    snd(n, 0x0400_0408, 0x10000 - 512);
    snd(n, 0x0400_040C, 16);
    snd(n, 0x0400_0400, 0x8000_0000 | 1 << 27 | 127);          // loop, PCM8, pan 0 (left), vol 127
    // capture 0 at channel 1's rate: PCM16, one-shot, 4 words, into 0x02004000
    snd(n, 0x0400_0418, 0x10000 - 512);
    snd(n, 0x0400_0510, 0x0200_4000);
    snd(n, 0x0400_0514, 4);
    snd(n, 0x0400_0508, 0x84);
    play(n, 2);
    let v = i16::from_le_bytes([n.main_ram[0x4002], n.main_ram[0x4003]]);
    assert!(v > 0x1000, "captured the left mix: {v:#x}");
    assert_eq!(n.io_read(ARM7, 0x0400_0508) & 0x80, 0, "one-shot capture done");
}

fn tri(n: &mut Nds, attr: u32, color: u16, z: f32, scale: f32) {
    gx(n, 0x29, &[attr]);
    gx(n, 0x40, &[0]);
    gx(n, 0x20, &[color as u32]);
    v16(n, -scale, -scale, z); v16(n, scale, -scale, z); v16(n, 0.0, scale, z);
}

#[test]
fn shadow_polygons_draw_through_the_stencil() {
    let mut s = blank_2d();
    let n = &mut s.nds;
    setup_3d(n);
    io(n, 0x0400_0060, 8);                                      // blending on
    gx(n, 0x29, &[0x011F_00C0]);                                // floor: opaque, id 1, a full-screen quad
    gx(n, 0x40, &[1]);
    gx(n, 0x20, &[GREEN as u32]);
    v16(n, -1.0, -1.0, 0.5); v16(n, 1.0, -1.0, 0.5); v16(n, 1.0, 1.0, 0.5); v16(n, -1.0, 1.0, 0.5);
    // shadow mask (mode 3, id 0) behind the floor in the middle: its depth test fails there
    tri(n, 0x000F_00F0, 0, 0.8, 0.5);
    // shadow (mode 3, id 2), black, half transparent, in front of everything, full size
    tri(n, 0x020F_00F0, 0, -0.5, 1.0);
    swap(n);
    let mid = frame3d(n, 128, 110).0;
    let edge = frame3d(n, 5, 186).0;
    assert!((mid >> 5) & 31 < 25, "shadowed: {mid:#x}");
    assert_eq!(edge, GREEN, "outside the mask: untouched");
}

#[test]
fn edge_marking_outlines_opaque_polygons() {
    let mut s = blank_2d();
    let n = &mut s.nds;
    setup_3d(n);
    io(n, 0x0400_0060, 0x20);                                   // edge marking
    io16(n, 0x0400_0332, GREEN);                                // EDGE_COLOR[1]: ids 8-15
    tri(n, 0x081F_00C0, RED, 0.0, 1.0);                         // id 8
    swap(n);
    assert_eq!(frame3d(n, 128, 150).0, RED, "inside");
    // the bottom row of the triangle is an edge against the clear plane
    let bottom = (0..192).rev().find(|&y| frame3d(n, 128, y).1 != 0).unwrap();
    assert_eq!(frame3d(n, 128, bottom).0, GREEN, "edge at row {bottom}");
}

#[test]
fn anti_aliasing_softens_outlines() {
    let mut s = blank_2d();
    let n = &mut s.nds;
    setup_3d(n);
    io(n, 0x0400_0060, 0x10);                                   // anti-aliasing
    tri(n, 0x011F_00C0, RED, 0.0, 1.0);                         // id 1, over the clear plane
    swap(n);
    // along the slanted left edge, some pixel in each row is partly transparent
    let row = 100;
    let first = (0..256).find(|&x| frame3d(n, x, row).1 != 0).unwrap();
    assert!(frame3d(n, first, row).1 < 31, "edge alpha {}", frame3d(n, first, row).1);
    assert_eq!(frame3d(n, 128, row).1, 31, "inside stays solid");
    // in front of another object (green wall, id 2): the edge mixes the two colours
    gx(n, 0x29, &[0x021F_00C0]);
    gx(n, 0x40, &[1]);
    gx(n, 0x20, &[GREEN as u32]);
    v16(n, -1.0, -1.0, 0.5); v16(n, 1.0, -1.0, 0.5); v16(n, 1.0, 1.0, 0.5); v16(n, -1.0, 1.0, 0.5);
    tri(n, 0x011F_00C0, RED, 0.0, 1.0);
    swap(n);
    let mixed = (0..256).map(|x| frame3d(n, x, row).0).find(|&c| c & 31 > 0 && (c >> 5) & 31 > 0);
    assert!(mixed.is_some(), "an edge pixel with both red and green");
    // without anti-aliasing: no mixed pixels
    io(n, 0x0400_0060, 0);
    gx(n, 0x29, &[0x021F_00C0]);
    gx(n, 0x40, &[1]);
    gx(n, 0x20, &[GREEN as u32]);
    v16(n, -1.0, -1.0, 0.5); v16(n, 1.0, -1.0, 0.5); v16(n, 1.0, 1.0, 0.5); v16(n, -1.0, 1.0, 0.5);
    tri(n, 0x011F_00C0, RED, 0.0, 1.0);
    swap(n);
    assert!((0..256).all(|x| { let c = frame3d(n, x, row).0; !(c & 31 > 0 && (c >> 5) & 31 > 0) }));
}

#[test]
fn microphone_silence_and_blowing() {
    let mut spi = Spi::new(nds_spi::make_firmware());
    let mut sample = |spi: &mut Spi, eight: bool| -> u16 {
        spi.write_cnt(0x8A00, 0xFFFF);                         // touchscreen, hold
        spi.write_data(0xE4 | if eight { 0x08 } else { 0 });   // channel 6 (AUX = mic)
        spi.write_data(0);
        let hi = spi.data as u16;
        spi.write_cnt(0x8200, 0xFFFF);
        spi.write_data(0);
        let lo = spi.data as u16;
        if eight { (hi << 1) | (lo >> 7) } else { (hi << 5) | (lo >> 3) }
    };
    assert_eq!(sample(&mut spi, false), 0x800, "silence: the midpoint");
    assert_eq!(sample(&mut spi, true), 0x80, "8-bit silence");
    spi.mic = true;
    let vals: Vec<u16> = (0..32).map(|_| sample(&mut spi, false)).collect();
    let spread = vals.iter().max().unwrap() - vals.iter().min().unwrap();
    assert!(spread > 0x800, "blowing is loud: spread {spread:#x}");
}

#[test]
fn screen_layouts_and_touch() {
    let mut s = blank_2d();
    // red top screen, blue bottom screen
    for y in 0..384 { for x in 0..256 {
        let i = (y * 256 + x) * 4;
        s.nds.framebuffer[i..i + 4].copy_from_slice(if y < 192 { &[255, 0, 0, 255] } else { &[0, 0, 255, 255] });
    } }
    let px = |s: &NdsSystem, x: usize, y: usize| { let w = s.display_info().buffer_width as usize; let i = (y * w + x) * 4; s.framebuffer()[i..i + 3].to_vec() };
    s.set_option("screen_layout", 1);
    assert_eq!((s.display_info().buffer_width, s.display_info().buffer_height), (512, 192));
    assert_eq!(px(&s, 10, 10), [255, 0, 0], "top screen on the left");
    assert_eq!(px(&s, 300, 10), [0, 0, 255], "touch screen on the right");
    s.set_pointer(256 + 40, 30, 1);
    assert_eq!(s.nds.spi.touch, (40, 30, true));
    s.set_option("screen_swap", 1);
    assert_eq!(px(&s, 10, 10), [0, 0, 255], "swapped");
    s.set_pointer(40, 30, 1);
    assert_eq!(s.nds.spi.touch, (40, 30, true));
    s.set_option("screen_layout", 3);
    assert_eq!((s.display_info().buffer_width, s.display_info().buffer_height), (256, 192));
    assert_eq!(px(&s, 10, 10), [0, 0, 255], "touch screen only");
    s.set_option("screen_layout", 2);
    s.set_pointer(40, 30, 1);
    assert!(!s.nds.spi.touch.2, "no touch screen shown: no touch");
    s.set_option("screen_layout", 0);
    s.set_option("screen_swap", 0);
    s.set_pointer(40, 192 + 30, 1);
    assert_eq!(s.nds.spi.touch, (40, 30, true), "stacked as before");
}
