use std::collections::HashMap;
use crate::arcade::simpsons::simpsons_system::SimpsonsSystem;
use crate::common::cpu::konami::KonamiBus;
use crate::common::system::System;

fn put(rom: &mut [u8], at: usize, code: &[u8]) { rom[at..at + code.len()].copy_from_slice(code); }

/// CPU address 8000-FFFF is region offset 78000-7FFFF
const FIXED: usize = 0x78000 - 0x8000;

fn machine() -> SimpsonsSystem {
    let mut main = vec![0u8; 0x80000];
    // Konami CPU opcodes: 48 LDS#, 10 LDA#, 12 LDA idx, 3A STA idx (postbyte 07 = extended),
    // 38 SETLINES#, 3C ANDCC#, 60 BRA, 8B INC idx, 9F RTI
    put(&mut main, FIXED + 0x8000, &[
        0x48, 0x5F, 0x00,                 // LDS #$5F00
        0x38, 0x05,                       // SETLINES #5: bank 5 at 6000
        0x12, 0x07, 0x60, 0x00,           // LDA $6000
        0x3A, 0x07, 0x40, 0x10,           // STA $4010
        0x10, 0x04, 0x3A, 0x07, 0x1D, 0x00, // K052109 IRQ on
        0x10, 0x10, 0x3A, 0x07, 0x1F, 0xA5, // K053246 object DMA on
        0x10, 0x04, 0x3A, 0x07, 0x1F, 0xC2, // FIRQ enable
        0x3C, 0xAF,                       // ANDCC: IRQ + FIRQ on
        0x60, 0xFE,                       // BRA *
    ]);
    // IRQ: count at 4000, acknowledge (bit 2 1 -> 0 -> 1)
    put(&mut main, FIXED + 0x8040, &[
        0x8B, 0x07, 0x40, 0x00,
        0x10, 0x00, 0x3A, 0x07, 0x1D, 0x00,
        0x10, 0x04, 0x3A, 0x07, 0x1D, 0x00,
        0x9F,
    ]);
    // FIRQ: count at 4001, acknowledge by toggling the enable
    put(&mut main, FIXED + 0x8060, &[
        0x8B, 0x07, 0x40, 0x01,
        0x10, 0x00, 0x3A, 0x07, 0x1F, 0xC2,
        0x10, 0x04, 0x3A, 0x07, 0x1F, 0xC2,
        0x9F,
    ]);
    put(&mut main, FIXED + 0xFFF6, &[0x80, 0x60, 0x80, 0x40]);
    put(&mut main, FIXED + 0xFFFE, &[0x80, 0x00]);
    main[5 * 0x2000] = 0xB5;

    // tile 1: every pixel pen 3 (planes 2 and 3 = bytes 1 and 0 of each 4-byte row)
    let mut t0 = vec![0u8; 0x1000];
    for b in &mut t0[16..32] { *b = 0xFF; }
    // sprite tile 0: pen 5 everywhere; tile 64: pen 15 (shadow) everywhere
    let mut sp = vec![0u8; 0x1000];
    for b in &mut sp[0..32] { *b = 0x55; }
    for b in &mut sp[2048..2080] { *b = 0xFF; }

    let mut regions = HashMap::new();
    regions.insert("maincpu".to_string(), main);
    regions.insert("k052109_0".to_string(), t0);
    regions.insert("k052109_1".to_string(), vec![0u8; 0x1000]);
    for i in 0..4 { regions.insert(format!("k053246_{i}"), sp.clone()); }
    regions.insert("eeprom".to_string(), vec![0xA5, 0x3C]);
    regions.insert("audiocpu".to_string(), sound_rom());
    regions.insert("k053260".to_string(), sample_rom());
    SimpsonsSystem::new("simpsons_test", regions, false).unwrap()
}

#[test]
fn simpsons_banking_and_interrupts() {
    let mut m = machine();
    m.run_frame(0);
    assert_eq!(m.board.read(0x4010), 0xB5, "SETLINES picked bank 5");
    for _ in 0..3 { m.run_frame(0); }
    let irqs = m.board.read(0x4000);
    let firqs = m.board.read(0x4001);
    assert_eq!(irqs, 4, "one vblank IRQ a frame");
    assert_eq!(firqs, 4, "one object-DMA FIRQ a frame");
}

#[test]
fn simpsons_video_views_and_dma() {
    let mut m = machine();
    let b = &mut m.board;
    // view 0: 2000-3FFF is the K052109 (offset +2000)
    b.write(0x2800, 0x12);
    assert_eq!(b.video.vram[0x2800], 0x12);
    // view 1: sprite RAM (big-endian words) + RAM
    b.write(0x1FC2, 0x02);
    b.write(0x2000, 0x80);
    b.write(0x2001, 0x07);
    b.write(0x2002, 0x12);
    b.write(0x2003, 0x34);
    b.write(0x3000, 0x99);
    assert_eq!(b.video.sprite_ram_cpu[0], 0x8007);
    assert_eq!(b.read(0x2002), 0x12);
    assert_eq!(b.read(0x3000), 0x99);
    assert_eq!(b.video.vram[0x2800], 0x12, "K052109 untouched");
    // an inactive sprite (no Z code) is skipped by object DMA
    b.video.sprite_ram_cpu[8] = 0x8000;
    b.video.sprite_ram_cpu[16] = 0x8009;
    b.video.sprite_ram[24] = 0x8001;   // stale entry gets cleared
    b.object_dma();
    assert_eq!(b.video.sprite_ram[0], 0x8007);
    assert_eq!(b.video.sprite_ram[1], 0x1234);
    assert_eq!(b.video.sprite_ram[8], 0x8009);
    assert_eq!(b.video.sprite_ram[16], 0);
    assert_eq!(b.video.sprite_ram[24], 0);
    // palette view at 0000-0FFF
    b.write(0x1FC2, 0x01);
    b.write(0x0005, 0x77);
    assert_eq!(b.video.palette[5], 0x77);
    b.write(0x1FC2, 0x00);
    b.write(0x0005, 0x66);
    assert_eq!(b.video.vram[5], 0x66);
}

#[test]
fn simpsons_render_layers_sprites_shadow() {
    let mut m = machine();
    let v = &mut m.board.video;
    let pal = |v: &mut crate::arcade::simpsons::simpsons_video::Video, i: usize, c: u16| {
        v.palette[i * 2] = (c >> 8) as u8;
        v.palette[i * 2 + 1] = c as u8;
    };
    pal(v, 0, 0x7C00);   // background: blue
    pal(v, 3, 0x001F);   // tile pen 3: red
    pal(v, 5, 0x03E0);   // sprite pen 5: green
    // FIX layer tile 1 at the top-left: raster (0, 16) = map (96, 16) = column 12, row 2
    v.vram[0x2000 + 2 * 64 + 12] = 1;
    // sprite at raster (100, 100): x = 100 + 51, y = -(100 + 31)
    let s = &mut v.sprite_ram;
    s[0..7].copy_from_slice(&[0x8001, 0, (1024 - 131) as u16, 151, 0x40, 0x40, 0]);
    // shadow sprite (tile 64, pen 15, shadow code 1) at raster (200, 100)
    s[8..15].copy_from_slice(&[0x8002, 64, (1024 - 131) as u16, 251, 0x40, 0x40, 0x0400]);
    // a double-size sprite (zoom 0x20) at raster centre (40, 200): covers 24..55
    s[16..23].copy_from_slice(&[0x8003, 0, (1024 - 200 - 23) as u16, 40 + 43, 0x20, 0x20, 0]);
    v.render();
    let px = |v: &crate::arcade::simpsons::simpsons_video::Video, x: usize, y: usize| {
        let i = ((y - 16) * 320 + x) * 4;
        v.framebuffer[i..i + 3].to_vec()
    };
    assert_eq!(px(v, 0, 16), vec![255, 0, 0], "FIX tile");
    assert_eq!(px(v, 7, 23), vec![255, 0, 0], "FIX tile corner");
    assert_eq!(px(v, 8, 16), vec![0, 0, 255], "background");
    assert_eq!(px(v, 100, 100), vec![0, 255, 0], "sprite");
    assert_eq!(px(v, 115, 115), vec![0, 255, 0], "sprite corner");
    assert_eq!(px(v, 116, 100), vec![0, 0, 255], "right of the sprite");
    assert_eq!(px(v, 205, 105), vec![0, 0, 153], "shadow darkens the background");
    assert_eq!(px(v, 24, 184), vec![0, 255, 0], "zoomed sprite top-left");
    assert_eq!(px(v, 55, 215), vec![0, 255, 0], "zoomed sprite bottom-right");
    assert_eq!(px(v, 56, 215), vec![0, 0, 255], "zoomed sprite ends");

    // scroll layer A by 8 pixels with the same tile: map column 12 + ... check it moves
    v.vram[0x2000 + 2 * 64 + 12] = 0;
    v.vram[0x2800 + 2 * 64 + 12] = 1;      // layer A tile at map (96, 16)
    v.vram[0x1A00] = 2;                     // x scroll 2: raster x = map x - 92
    v.render();
    assert_eq!(px(v, 4, 16), vec![255, 0, 0], "layer A, scrolled");
    assert_eq!(px(v, 3, 16), vec![0, 0, 255]);
    assert_eq!(px(v, 11, 16), vec![255, 0, 0]);
    assert_eq!(px(v, 12, 16), vec![0, 0, 255]);
}

#[test]
fn simpsons_eeprom_on_the_bus() {
    let mut m = machine();
    let b = &mut m.board;
    let w = |b: &mut crate::arcade::simpsons::simpsons_system::Board, v: u8| {
        b.cycle += 10;
        b.write(0x1FC2, v);
    };
    // CS low, then high; send start bit + READ (10) + address 1 (9 bits)
    w(b, 0x00);
    w(b, 0x08);
    let bits: Vec<u8> = std::iter::once(1).chain([1, 0]).chain((0..9).rev().map(|i| (1u16 >> i & 1) as u8)).collect();
    for bit in bits {
        let di = bit << 7;
        w(b, 0x08 | di);
        w(b, 0x18 | di);
    }
    let mut v = 0u8;
    for _ in 0..8 {
        w(b, 0x08);
        w(b, 0x18);
        v = v << 1 | (b.read(0x1F81) >> 4 & 1);
    }
    assert_eq!(v, 0x3C);
    assert_eq!(b.read(0x1F81) & 0x20, 0x20, "ready");
}

#[test]
fn simpsons_round_trip() {
    let mut m = machine();
    crate::arcade::state::check_round_trip(&mut m, 5, 10);
}

#[test]
#[ignore]
fn simpsons_speed() {
    let mut m = machine();
    let t = std::time::Instant::now();
    for _ in 0..600 { m.run_frame(0); }
    println!("600 frames: {:?}", t.elapsed());
}

/// Sound Z80: arms the NMI and halts in a loop (NMI counts at F010); the IRQ echoes the
/// main CPU's port 0 + 1 back on port 2 and plays a PCM sample on voice 0.
fn sound_rom() -> Vec<u8> {
    let mut r = vec![0u8; 0x28000];
    // a varying delay (DJNZ, 1-16 turns) before each arm-and-HALT moves it around the
    // K053260's 64-clock frame
    put(&mut r, 0x0000, &[0x31, 0x00, 0xF8, 0xED, 0x56, 0xFB,
                          0x0C, 0x79, 0xE6, 0x3F, 0x3C, 0x47, 0x10, 0xFE,
                          0x32, 0x00, 0xFA, 0x76, 0x18, 0xF2]);
    put(&mut r, 0x0038, &[0xC3, 0x00, 0x01]);
    put(&mut r, 0x0066, &[0xE5, 0x2A, 0x10, 0xF0, 0x23, 0x22, 0x10, 0xF0, 0xE1, 0xED, 0x45]);
    let mut irq = vec![0xF5, 0x3A, 0x00, 0xFC, 0x3C, 0x32, 0x02, 0xFC];
    for (reg, v) in [(0x2F, 0x02), (0x08, 0x00), (0x09, 0x0F), (0x0A, 0x40), (0x0B, 0x00),
                     (0x0C, 0x00), (0x0D, 0x00), (0x0E, 0x00), (0x0F, 0x7F), (0x2C, 0x04),
                     (0x28, 0x00), (0x28, 0x01)] {
        irq.extend_from_slice(&[0x3E, v, 0x32, reg, 0xFC]);
    }
    irq.extend_from_slice(&[0xF1, 0xFB, 0xED, 0x4D]);
    put(&mut r, 0x0100, &irq);
    r
}

fn sample_rom() -> Vec<u8> {
    let mut r = vec![0u8; 0x1000];
    for b in &mut r[1..0x41] { *b = 0x40; }
    r
}

#[test]
fn simpsons_sound_board() {
    let mut m = machine();
    m.run_frame(0);
    assert!(m.board.sound.read_ram(0xF010) > 0, "K053260 SH1 drives the Z80 NMI");
    let _ = m.drain_audio();
    m.board.write(0x1FC6, 0x41);
    m.board.read(0x1FC4);   // sound IRQ
    m.run_frame(0);
    assert_eq!(m.board.read(0x1FC6), 0x42, "Z80 answered through the K053260 ports");
    let a = m.drain_audio();
    let n = a.len() as i64;
    assert!((n - 2 * 44100 * 50688 / 3_000_000).abs() < 8, "audio rate {n}");
    assert!(a.iter().any(|s| s.abs() > 0.05), "PCM voice audible");
}

#[test]
fn simpsons_sound_nmi_keeps_running() {
    // the arm-then-HALT loop must never take the NMI before the HALT (it would sleep forever)
    let mut m = machine();
    for f in 0..240 {
        let cnt = |m: &SimpsonsSystem| m.board.sound.read_ram(0xF010) as u16 | (m.board.sound.read_ram(0xF011) as u16) << 8;
        let before = cnt(&m);
        m.run_frame(0);
        assert!(cnt(&m).wrapping_sub(before) > 20, "sound NMIs stopped in frame {f}");
    }
}


#[test]
fn simpsons_player_slots() {
    use crate::common::input as pad;
    let mut m = machine();
    m.set_input(pad::COIN | pad::B);
    assert_eq!(m.board.read(0x1F80), 0xFE, "coin 1 = Marge");
    assert_eq!(m.board.read(0x1F90), 0xEF);
    m.set_input(pad::R);
    m.set_input(pad::R);            // held: one step only
    m.set_input(0);
    m.set_input(pad::R);            // Homer -> Bart
    m.set_input(pad::COIN | pad::START);
    assert_eq!(m.board.read(0x1F80), 0xFB, "coin 3 = Bart");
    assert_eq!(m.board.read(0x1F92), 0x7F, "start 3");
    assert_eq!(m.board.read(0x1F90), 0xFF);
    // pad 2 drives the next slot (Lisa)
    m.set_input(pad::A << pad::P2_SHIFT);
    assert_eq!(m.board.read(0x1F93), 0xDF);
}
