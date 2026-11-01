use super::boot;
use super::cart::{Cart, Rotate};
use super::lynx_system::LynxSystem;
use crate::common::cpu::m65c02::Bus65;
use crate::common::system::System;

fn hex(s: &str) -> Vec<u8> { (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect() }

/// Decryption checked against the published reference routine (lynxdec.cpp) on random
/// blocks: 3 blocks of 51 bytes in, 150 bytes out.
#[test]
fn decrypt_matches_the_reference() {
    let enc = hex("fd0e830b73ecf54b4a7435e7f10641c86baadf0c3de64c48a1dbd46878dc584eeadb595ec74ea911c2def9b4e43a7c4fe45b5c0242a86ae3833e4bfb1ba34a057ea36346f20c57b4eb5068cf8be41f6f407b918223fc65a63ab0a15553eb5bd18fbe1781cb6f15b6bf9e854a82a4bac21f4b444247a9e882598ad7ac75327e04f19585bc04bb72c459f70edb9cc89ebb14e2fe5b8ce6dde57015");
    let want = hex("569d27456e0218a1bd665eaa77450a9abce45b4168f8cde0b6fe10d9a7c78c83cfde3971435f3d2041f24712f50326cf6b697cea9d4b9bf4a1b86684b2bf6a03b7bfadd98d24c752f301fdd422eb449e05a899d175a9f033636d1d41112e17ad5268a9d656e2c58965f7228217ae4d08c8dd8d973d7ac3cc389c01d3b771389aff1c99959a576880e898b8fbf29bb5b9c8a6831202b0");
    let (out, used) = boot::decrypt_frame(&enc);
    assert_eq!(used, 1 + 3 * 51);
    assert_eq!(out, want);
}

/// A homebrew (.o) program: palette entry 1 yellow, screen at 2000, line 0 all colour 1,
/// pixel (0, 50) colour 1; vectors from RAM, timer 2 (frame) interrupt counting into $80.
fn homebrew() -> Vec<u8> {
    let code: Vec<u8> = vec![
        0xA9, 0x0F, 0x8D, 0xA1, 0xFD, 0x8D, 0xB1, 0xFD,       // GREEN1 = REDBLUE1 = 0F
        0xA9, 0x00, 0x8D, 0x94, 0xFD, 0xA9, 0x20, 0x8D, 0x95, 0xFD, // DISPADR = 2000
        0xA2, 0x00, 0xA9, 0x11, 0x9D, 0x00, 0x20, 0xE8, 0xE0, 0x50, 0xD0, 0xF6, // fill line 0
        0xA9, 0x10, 0x8D, 0xA0, 0x2F,                         // (0, 50) = colour 1
        0xA9, 0x40, 0x8D, 0xFE, 0xFF, 0xA9, 0x04, 0x8D, 0xFF, 0xFF, // IRQ vector 0440
        0xA9, 0x08, 0x8D, 0xF9, 0xFF,                         // MAPCTL: vectors from RAM
        0xA9, 0x9F, 0x8D, 0x09, 0xFD,                         // timer 2: IRQ, reload, count, linked
        0x58, 0x80, 0xFE,                                     // CLI; BRA *
    ];
    let mut code = code;
    code.resize(0x40, 0xEA);                                  // the handler at 0440:
    code.extend([0xE6, 0x80, 0xA9, 0xFF, 0x8D, 0x80, 0xFD, 0x40]); // INC $80; INTRST = FF; RTI
    assert_eq!(code[0x40], 0xE6, "IRQ handler lands at 0440");
    let len = (code.len() + 10) as u16;
    let mut f = vec![0x80, 0x08, 0x04, 0x00, (len >> 8) as u8, len as u8, b'B', b'S', b'9', b'3'];
    f.extend(code);
    f
}

fn px(s: &LynxSystem, x: usize, y: usize) -> [u8; 3] {
    let i = (y * 160 + x) * 4;
    [s.framebuffer[i], s.framebuffer[i + 1], s.framebuffer[i + 2]]
}

#[test]
fn homebrew_draws_through_the_lcd_dma_and_counts_frames() {
    let mut s = LynxSystem::from_rom(&homebrew(), "test.o", &[]).unwrap();
    for _ in 0..10 { s.run_frame(0); }
    assert_eq!(px(&s, 0, 0), [255, 255, 0], "line 0, colour 1 = yellow");
    assert_eq!(px(&s, 159, 0), [255, 255, 0]);
    assert_eq!(px(&s, 0, 50), [255, 255, 0]);
    assert_eq!(px(&s, 1, 50), [0, 0, 0], "colour 0");
    assert_eq!(px(&s, 0, 1), [0, 0, 0]);
    let irqs = s.bus.ram[0x80];
    assert!((8..=10).contains(&irqs), "one frame interrupt a frame: {irqs}");
    // the boot preset gives about 60 frames a second
    let us = s.frame_time_usec();
    assert!((16_000..=17_500).contains(&us), "{us} us a frame");
}

/// The cart's block number goes in one bit per strobe (IODAT bit 1, SYSCTL1 bit 0);
/// RCART0 then steps through the block.
#[test]
fn cart_block_select_and_read() {
    let mut img = b"LYNX".to_vec();
    img.extend([0x00, 0x01, 0, 0, 1, 0]);                  // 256-byte pages, version 1
    img.extend(b"Test cart".iter().chain(std::iter::repeat(&0u8)).take(32));
    img.extend(b"mimesys".iter().chain(std::iter::repeat(&0u8)).take(16));
    img.extend([2, 0, 0, 0, 0, 0]);                         // rotate right
    assert_eq!(img.len(), 64);
    for b in 0..256usize { for i in 0..256usize { img.push((b ^ i) as u8); } }
    let (cart, h) = Cart::new(&img).unwrap();
    assert_eq!((h.name.as_str(), h.rotate), ("Test cart", Rotate::Right));
    let mut s = LynxSystem::from_rom(&img, "t.lnx", &[]).unwrap();
    s.bus.cart = cart;
    let bus = &mut s.bus;
    bus.mapctl = 0;
    for bit in (0..8).rev() {
        let d = (5u8 >> bit & 1) << 1;
        bus.write(0xFD8B, d);                                // IODAT: address bit
        bus.write(0xFD87, 0x03);                             // strobe up: clock it in
        bus.write(0xFD87, 0x02);
    }
    assert_eq!(bus.cart.shifter, 5);
    let got: Vec<u8> = (0..3).map(|_| bus.read(0xFCB2)).collect();
    assert_eq!(got, vec![5, 4, 7], "block 5, bytes 0-2");
}

#[test]
fn joypad_and_left_hand_swap() {
    use crate::common::input::{A, LEFT, START, UP};
    let mut s = LynxSystem::from_rom(&homebrew(), "test.o", &[]).unwrap();
    s.set_input(UP | LEFT | A | START);
    assert_eq!(s.bus.read(0xFCB0), 0x80 | 0x20 | 0x01);
    assert_eq!(s.bus.read(0xFCB1), 0x01, "pause");
    s.bus.write(0xFC92, 0x08);                               // SPRSYS: left hand
    assert_eq!(s.bus.read(0xFCB0), 0x40 | 0x10 | 0x01, "up/down and left/right swap");
}

#[test]
fn save_state_round_trip() {
    let mut s = LynxSystem::from_rom(&homebrew(), "test.o", &[]).unwrap();
    for _ in 0..3 { s.run_frame(0); }
    let st = s.save_state();
    for _ in 0..3 { s.run_frame(0); }
    let after = s.bus.ram[0x80];
    s.load_state(&st).unwrap();
    for _ in 0..3 { s.run_frame(0); }
    assert_eq!(s.bus.ram[0x80], after);
}

#[test]
fn detection() {
    let g = crate::common::rom_detect::detect(&homebrew(), "game.o");
    assert_eq!(g[0].system, "lynx", "{g:?}");
    let mut img = b"LYNX".to_vec();
    img.resize(64 + 0x10000, 0);
    let g = crate::common::rom_detect::detect(&img, "whatever.bin");
    assert_eq!(g[0].system, "lynx", "{g:?}");
}

/// Without a boot ROM: reset goes to the stand-in ROM's FF80, which traps, loads and
/// decrypts the cart's first frame to 0200 and jumps there.
#[test]
fn boots_without_the_rom() {
    let mut img = b"LYNX".to_vec();
    img.extend([0x00, 0x02, 0, 0, 1, 0]);
    img.resize(64, 0);
    let mut rom = vec![0u8; 0x200 * 256];
    rom[0] = 256u16.wrapping_sub(2) as u8;                   // two blocks
    for i in 1..1 + 2 * 51 { rom[i] = (i * 37 % 251) as u8; }
    rom[51] &= 0x1F; rom[102] &= 0x1F;                      // each block below the key
    let (plain, _) = boot::decrypt_frame(&rom);
    img.extend(&rom);
    let mut s = LynxSystem::from_rom(&img, "t.lnx", &[]).unwrap();
    assert_eq!(s.cpu.pc, 0xFF80, "reset vector of the stand-in ROM");
    let n = s.cpu.step(&mut s.bus);                          // STA $FD97: the trap
    assert!(n > 0 && s.bus.trap);
    s.bus.trap = false;
    s.boot_trap();                                           // what run_frame does next
    assert_eq!(s.cpu.pc, 0x0200);
    assert_eq!(plain.len(), 100);
    assert_eq!(&s.bus.ram[0x200..0x200 + 100], &plain[..], "loader decrypted to 0200");
}

/// A real cartridge image: tests/data/lynx_cc65_hello.c built with cc65 (zlib license)
/// for the Lynx. cc65's encrypted micro-loader is decrypted by the no-ROM boot, loads
/// the program from the cart, and the C code fills the top line with colour 1 (yellow),
/// the bottom line with colour 2 (blue) and echoes the joypad into line 50.
#[test]
fn cc65_cartridge_boots_and_draws() {
    let img = include_bytes!("../common/tests/data/lynx_cc65_hello.lnx");
    let mut s = LynxSystem::from_rom(img, "hello.lnx", &[]).unwrap();
    for _ in 0..30 { s.run_frame(0); }
    assert_eq!(px(&s, 0, 0), [255, 255, 0], "top line yellow");
    assert_eq!(px(&s, 159, 101), [0, 0, 255], "bottom line blue");
    s.run_frame(crate::common::input::A);                   // outer button = bit 0
    s.run_frame(crate::common::input::A);
    assert_eq!(px(&s, 1, 50), [255, 255, 0], "joypad bit 0 shows as colour 1");
}


/// cc65's TGI graphics driver draws with Suzy sprites: a filled red box (a scaled
/// one-pixel sprite), a green line along the bottom and "LYNX" in white text, then
/// swaps buffers on the frame interrupt. tests/data/lynx_cc65_tgi.c.
#[test]
fn cc65_tgi_draws_with_sprites() {
    let img = include_bytes!("../common/tests/data/lynx_cc65_tgi.lnx");
    let mut s = LynxSystem::from_rom(img, "tgi.lnx", &[]).unwrap();
    for _ in 0..60 { s.run_frame(0); }
    let bg = px(&s, 5, 5);
    let red = px(&s, 30, 20);
    assert_ne!(red, bg, "the box is drawn");
    assert!(red[0] > 200 && red[1] < 100, "red box: {red:?}");
    assert_eq!(px(&s, 10, 10), red, "box corner (10, 10)");
    assert_eq!(px(&s, 49, 29), red, "box corner (49, 29)");
    assert_eq!(px(&s, 50, 29), bg, "box ends at x 49");
    assert_eq!(px(&s, 49, 30), bg, "box ends at y 29");
    let green = px(&s, 80, 101);
    assert!(green[1] > 100 && green[0] < 100, "green line: {green:?}");
    let white = (48..60).flat_map(|y| (60..100).map(move |x| (x, y))).filter(|&(x, y)| px(&s, x, y) == [255, 255, 255]).count();
    assert!(white > 30, "text pixels: {white}");
    // the program's long multiply (software) landed too
    assert_eq!(u32::from_le_bytes([s.bus.ram[0x80], s.bus.ram[0x81], s.bus.ram[0x82], s.bus.ram[0x83]]), 1234 * 567);
}

mod suzy_unit {
    use crate::lynx::suzy::Suzy;

    /// RAM with: screen 2000, collision buffer 4000, SCBs at 1000 / 1100, sprite data
    /// at 1800 (one literal 4-bit line: pens 1 2 3 and a closing 0).
    fn setup() -> (Suzy, Vec<u8>) {
        let mut s = Suzy::new();
        let mut ram = vec![0u8; 0x10000];
        for (a, v) in [(0x08u8, 0x00u8), (0x09, 0x20), (0x0A, 0x00), (0x0B, 0x40),   // VIDBAS, COLLBAS
                       (0x18, 0x00), (0x19, 0x01), (0x1A, 0x00), (0x1B, 0x01),      // SPRHSIZ = SPRVSIZ = 1.0
                       (0x24, 0x10), (0x25, 0x00)] {                                // COLLOFF = 0x10
            s.write(a, v);
        }
        s.write(0x90, 1);                                                           // SUZYBUSEN
        ram[0x1800..0x1804].copy_from_slice(&[3, 0x12, 0x30, 0]);
        (s, ram)
    }

    /// SCB at `at`: 4-bit normal sprite (type 4), literal, keep pens, collision number,
    /// next SCB, data 1800, position.
    fn scb(ram: &mut [u8], at: usize, ctl0_extra: u8, coll: u8, next: u16, x: u16, y: u16) {
        let b = [0xC4 | ctl0_extra, 0x88, coll, next as u8, (next >> 8) as u8, 0x00, 0x18,
                 x as u8, (x >> 8) as u8, y as u8, (y >> 8) as u8];
        ram[at..at + b.len()].copy_from_slice(&b);
    }

    fn pixel(ram: &[u8], base: usize, x: usize, y: usize) -> u8 {
        let b = ram[base + y * 80 + x / 2];
        if x & 1 == 0 { b >> 4 } else { b & 15 }
    }

    fn go(s: &mut Suzy, ram: &mut [u8], first: u16) -> u32 {
        s.write(0x10, first as u8);
        s.write(0x11, (first >> 8) as u8);
        s.write(0x91, 1);                                                           // SPRGO
        s.paint(ram)
    }

    #[test]
    fn literal_sprite_and_flip() {
        let (mut s, mut ram) = setup();
        scb(&mut ram, 0x1000, 0, 1, 0, 10, 5);
        assert!(go(&mut s, &mut ram, 0x1000) > 0, "takes time");
        assert!(!s.sprgo, "SPRGO clears when the chain ends");
        assert_eq!((10..14).map(|x| pixel(&ram, 0x2000, x, 5)).collect::<Vec<_>>(), vec![1, 2, 3, 0]);
        assert_eq!(pixel(&ram, 0x2000, 10, 4), 0, "one line high");
        // horizontal flip draws leftwards from the reference point
        let (mut s, mut ram) = setup();
        scb(&mut ram, 0x1000, 0x20, 1, 0, 10, 5);
        go(&mut s, &mut ram, 0x1000);
        assert_eq!((8..11).map(|x| pixel(&ram, 0x2000, x, 5)).collect::<Vec<_>>(), vec![3, 2, 1]);
    }

    #[test]
    fn scaling_doubles_the_size() {
        let (mut s, mut ram) = setup();
        s.write(0x18, 0x00); s.write(0x19, 0x02);                                   // 2.0 x 2.0
        s.write(0x1A, 0x00); s.write(0x1B, 0x02);
        scb(&mut ram, 0x1000, 0, 1, 0, 20, 30);
        go(&mut s, &mut ram, 0x1000);
        assert_eq!((20..26).map(|x| pixel(&ram, 0x2000, x, 30)).collect::<Vec<_>>(), vec![1, 1, 2, 2, 3, 3]);
        assert_eq!(pixel(&ram, 0x2000, 21, 31), 1, "two lines high");
        assert_eq!(pixel(&ram, 0x2000, 21, 32), 0);
    }

    #[test]
    fn collisions_reach_the_depository() {
        let (mut s, mut ram) = setup();
        scb(&mut ram, 0x1000, 0, 2, 0x1100, 10, 5);                                 // number 2, then...
        scb(&mut ram, 0x1100, 0, 5, 0, 11, 5);                                      // number 5 over it
        go(&mut s, &mut ram, 0x1000);
        assert_eq!(ram[0x1010], 0, "the first sprite hit nothing");
        assert_eq!(ram[0x1110], 2, "the second sprite hit number 2");
        assert_eq!(pixel(&ram, 0x4000, 11, 5), 5, "collision buffer holds the latest number");
        // a non-colliding sprite (SPRCOLL bit 5) leaves its depository alone
        ram[0x1110] = 0xAA;
        ram[0x1102] = 0x25;
        go(&mut s, &mut ram, 0x1000);
        assert_eq!(ram[0x1110], 0xAA);
    }

    #[test]
    fn math_unit() {
        let mut s = Suzy::new();
        let w = |s: &mut Suzy, a: u8, v: u8| { s.write(a, v); };
        let r32 = |s: &mut Suzy, a: u8| u32::from_le_bytes([0, 1, 2, 3].map(|i| s.read(a + i).0));
        // 1234 * 567, unsigned: D C B A (writing A starts it)
        w(&mut s, 0x52, (1234u16 & 0xFF) as u8); w(&mut s, 0x53, (1234u16 >> 8) as u8);
        w(&mut s, 0x54, (567u16 & 0xFF) as u8); w(&mut s, 0x55, (567u16 >> 8) as u8);
        assert_eq!(r32(&mut s, 0x60), 1234 * 567);
        // signed: -3 * 4 = -12
        w(&mut s, 0x92, 0x80);
        w(&mut s, 0x52, 0xFD); w(&mut s, 0x53, 0xFF);
        w(&mut s, 0x54, 0x04); w(&mut s, 0x55, 0x00);
        assert_eq!(r32(&mut s, 0x60) as i32, -12);
        // accumulate into JKLM
        w(&mut s, 0x92, 0x40);
        w(&mut s, 0x6C, 0); w(&mut s, 0x6D, 0); w(&mut s, 0x6E, 0); w(&mut s, 0x6F, 0);
        for _ in 0..3 { w(&mut s, 0x52, 10); w(&mut s, 0x53, 0); w(&mut s, 0x54, 7); w(&mut s, 0x55, 0); }
        assert_eq!(r32(&mut s, 0x6C), 210);
        // 1000 / 7 = 142 r 6: H G F E (writing E starts it), P N
        w(&mut s, 0x92, 0);
        w(&mut s, 0x56, 7); w(&mut s, 0x57, 0);
        w(&mut s, 0x60, (1000u16 & 0xFF) as u8); w(&mut s, 0x61, (1000u16 >> 8) as u8); w(&mut s, 0x62, 0); w(&mut s, 0x63, 0);
        assert_eq!((r32(&mut s, 0x52), r32(&mut s, 0x6C)), (142, 6));
        // divide by zero sets the math bit (SPRSYS bit 6)
        w(&mut s, 0x56, 0); w(&mut s, 0x57, 0); w(&mut s, 0x63, 0);
        assert_ne!(s.read(0x92).0 & 0x40, 0);
    }
}

/// Rotation: the player's option over the cart, and the D-pad turning with the screen.
#[test]
fn rotation_option_and_dpad() {
    use crate::common::input::{RIGHT, UP};
    use crate::common::system::Rotation;
    let mut s = LynxSystem::from_rom(&homebrew(), "test.o", &[]).unwrap();
    assert!(matches!(s.display_info().rotation, Rotation::None));
    s.set_option("lynx_rotate", 2);                          // left (Gauntlet, Klax)
    assert!(matches!(s.display_info().rotation, Rotation::CW90));
    s.set_input(RIGHT);
    assert_eq!(s.bus.read(0xFCB0), 0x80, "right on the pad is up on the Lynx");
    s.set_option("lynx_rotate", 3);                          // right (Raiden, NFL Football)
    assert!(matches!(s.display_info().rotation, Rotation::CCW90));
    s.set_input(UP);
    assert_eq!(s.bus.read(0xFCB0), 0x10, "up on the pad is right on the Lynx");
}

/// Sound channel 0: the LFSR steps on each timer borrow exactly as Handy's
/// GetLfsrNext (sequence computed with it), output = +-volume by bit 0; the stereo
/// mixer's attenuation and per-side mute.
#[test]
fn sound_channel_lfsr_and_mixer() {
    use super::mikey::{Events, Mikey};
    let mut m = Mikey::new();
    // shaper 0x1B3A5: taps 0x0D << 13 | tap-7 bit (CTL bit 7) | LFSR 3A5
    for (a, v) in [(0x20u8, 0x40u8), (0x21, 0x0D), (0x23, 0xA5), (0x27, 0x30), (0x24, 9), (0x26, 0), (0x25, 0x80 | 0x18)] {
        m.write(a, v);
    }
    let mut ev = Events::default();
    // first borrow after 1 us (count 0), then one every 10 us: 16 borrows
    m.advance(16 + 15 * 160, &mut ev);
    let shaper = (m.read(0x21) as u32) << 13 | ((m.read(0x25) >> 7) as u32) << 12 | ((m.read(0x27) >> 4) as u32) << 8 | m.read(0x23) as u32;
    assert_eq!(shaper, 0x1B5CA, "16th value of the reference sequence");
    assert_eq!(m.read(0x22) as i8, -0x40, "bit 0 clear: -volume");
    assert_eq!(m.sound(), (-0x40, -0x40), "both speakers, no attenuation");
    // left at 8/16 through MPAN + ATTEN_A, right muted through MSTEREO
    m.write(0x40, 0x8F);
    m.write(0x44, 0x10);
    m.write(0x50, 0x01);
    assert_eq!(m.sound(), (-0x20, 0));
    // integrate mode moves the output by the volume each borrow
    m.write(0x50, 0); m.write(0x44, 0);
    m.write(0x22, 0);
    m.write(0x25, 0x80 | 0x20 | 0x18);
    m.advance(160 * 4, &mut ev);
    let o = m.read(0x22) as i8;
    assert!(o != 0 && o % 0x40 == 0, "integrated: {o}");
}

/// The cc65 TGI cart now produces a steady stream of 44.1 kHz stereo samples.
#[test]
fn audio_stream_rate() {
    let img = include_bytes!("../common/tests/data/lynx_cc65_tgi.lnx");
    let mut s = LynxSystem::from_rom(img, "tgi.lnx", &[]).unwrap();
    for _ in 0..60 { s.run_frame(0); }
    s.drain_audio();
    let mut n = 0usize;
    let mut us = 0i64;
    for _ in 0..60 { s.run_frame(0); n += s.drain_audio().len() / 2; us += s.frame_time_usec(); }
    let want = us as f64 * 44_100.0 / 1e6;
    assert!((n as f64 - want).abs() < 50.0, "{n} samples for {us} us");
}

/// Cartridge EEPROM through cc65's 93C46 routines (tests/data/lynx_cc65_eeprom.c): the
/// header's EEPROM byte set to 1, word 5 preloaded as if from a save file; the program
/// reads it, writes 0x1234 to word 3 and reads that back.
#[test]
fn eeprom_93c46_through_cc65() {
    let mut img = include_bytes!("../common/tests/data/lynx_cc65_eeprom.lnx").to_vec();
    img[60] = 1;                                              // 93C46, 16-bit
    let mut s = LynxSystem::from_rom(&img, "eeprom.lnx", &[]).unwrap();
    assert!(s.has_battery());
    let mut save = vec![0xFFu8; 128];
    save[10] = 0xCD; save[11] = 0xAB;                          // word 5 = ABCD
    s.load_sram(&save);
    for _ in 0..30 { s.run_frame(0); }
    let w = |s: &LynxSystem, a: usize| u16::from_le_bytes([s.bus.ram[a], s.bus.ram[a + 1]]);
    assert_eq!(w(&s, 0x84), 0xBEEF, "program finished");
    assert_eq!(w(&s, 0x82), 0xABCD, "read the saved word");
    assert_eq!(w(&s, 0x80), 0x1234, "read back what it wrote");
    assert!(s.is_sram_dirty());
    let sram = s.get_sram().unwrap();
    assert_eq!(sram.len(), 128);
    assert_eq!(&sram[6..8], &[0x34, 0x12], "word 3 in the save file");
}

/// Every timer counts, timer 6 included (it isn't in a chain; Rygar's sound driver runs
/// on its interrupt). Timer 4 is the UART's baud clock: it raises no interrupt itself.
#[test]
fn all_eight_timers_count_and_interrupt() {
    use super::mikey::{Events, Mikey};
    let mut m = Mikey::new();
    for n in 0..8u8 {
        m.write(n * 4, 9);                                    // backup 9
        m.write(n * 4 + 1, 0x80 | 0x18);                      // IRQ, reload, count, 1 us
    }
    let mut ev = Events::default();
    m.advance(16 * 25, &mut ev);
    assert_eq!(m.irq_pending, 0xEF, "{:08b}", m.irq_pending);
}
