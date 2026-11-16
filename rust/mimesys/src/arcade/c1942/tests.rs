use super::c1942_system::*;
use crate::common::cpu::z80::Bus;
use crate::common::input as pad;
use crate::common::system::System;
use std::collections::HashMap;

fn regions(main: &[u8], snd: &[u8], irq: &[(usize, u8)]) -> HashMap<String, Vec<u8>> {
    let mut rom = vec![0u8; 0x20000];
    rom[..main.len()].copy_from_slice(main);
    rom[0x10000 + 2 * 0x4000] = 0xB2; // bank 2's first byte
    let mut s = vec![0u8; 0x4000];
    s[..snd.len()].copy_from_slice(snd);
    let mut p = vec![0u8; 0x100];
    for &(l, v) in irq { p[l] = v; }
    let mut r = HashMap::new();
    r.insert("maincpu".into(), rom);
    r.insert("audiocpu".into(), s);
    r.insert("gfx1".into(), vec![0u8; 0x2000]);
    r.insert("gfx2".into(), vec![0u8; 0xC000]);
    r.insert("gfx3".into(), vec![0xFFu8; 0x10000]);
    // palette: red from the colour index's top nibble (so the sprite, background and
    // text ranges differ); LUTs pass the low nibble
    let mut pal = vec![0u8; 0x300];
    for i in 0..0x100 { pal[i] = (i >> 4) as u8; }
    r.insert("palproms".into(), pal);
    r.insert("charprom".into(), (0..0x100).map(|i| (i & 15) as u8).collect());
    r.insert("tileprom".into(), (0..0x100).map(|i| (i & 15) as u8).collect());
    r.insert("sprprom".into(), (0..0x100).map(|i| (i & 15) as u8).collect());
    r.insert("irqprom".into(), p);
    r
}

/// The PROM's vblank RST 10 once a frame, the sound CPU's four; inputs; the ROM bank;
/// both AY-3-8910s reached by the sound CPU.
#[test]
fn irqs_inputs_bank_and_sound() {
    let mut main = vec![0u8; 0x20];
    // 0000: LD SP,F000; IM 0; LD A,2; LD (C806),A; LD A,(8000); LD (E002),A; EI; JR $
    let prog = [0x31, 0x00, 0xF0, 0xED, 0x46, 0x3E, 0x02, 0x32, 0x06, 0xC8, 0x3A, 0x00, 0x80, 0x32, 0x02, 0xE0, 0xFB, 0x18, 0xFE];
    main[..prog.len()].copy_from_slice(&prog);
    // the vblank entry uses RST 18 here (PROM bits 0-1 = 3), its handler at 0018
    main.resize(0x30, 0);
    main[0x18..0x26].copy_from_slice(&[0x21, 0x00, 0xE0, 0x34, 0x3A, 0x01, 0xC0, 0x32, 0x01, 0xE0, 0xFB, 0xED, 0x4D, 0x00]);
    // 0000: JP 0050 (the program sits after the IM 1 handler at 0038)
    let mut p = vec![0x31, 0x00, 0x48, 0xED, 0x56, 0xFB];
    for (port, r, v) in [(0x80u8, 0x00u8, 0x80u8), (0x80, 0x07, 0x3E), (0x80, 0x08, 0x0F), (0xC0, 0x00, 0x40), (0xC0, 0x07, 0x3E), (0xC0, 0x08, 0x0F)] {
        p.extend([0x3E, r, 0x32, 0x00, port, 0x3E, v, 0x32, 0x01, port]);
    }
    p.extend([0x18, 0xFE]);
    let mut snd = vec![0u8; 0x50 + p.len()];
    snd[..3].copy_from_slice(&[0xC3, 0x50, 0x00]);
    snd[0x50..].copy_from_slice(&p);
    snd[0x38..0x3F].copy_from_slice(&[0x21, 0x00, 0x40, 0x34, 0xFB, 0xED, 0x4D]);
    let irq = [(240, 0x0B), (8, 0x04), (66, 0x04), (132, 0x04), (198, 0x04)];
    let mut s = C1942System::new("1942", regions(&main, &snd, &irq)).unwrap();
    for _ in 0..10 { s.run_frame(pad::RIGHT | pad::A); }
    assert_eq!(s.board.read(0xE000), 10, "one vblank interrupt a frame");
    assert_eq!(s.board.read(0xE001), !(0x01 | 0x20), "right and loop, active low");
    assert_eq!(s.board.read(0xE002), 0xB2, "bank 2");
    assert_eq!(s.sound_bus().read(0x4000), 40, "four sound IRQs a frame");
    let audio = s.drain_audio();
    assert!(audio.len() > 1400);
    assert!(audio.iter().fold(0f32, |m, v| m.max(v.abs())) > 0.1, "both tones play");
}

/// A sprite shows on the 16 lines from its Y, and a tall one (4 tiles) on 64; sprites
/// 16-23 only in the top half.
#[test]
fn sprites_per_line() {
    let mut r = regions(&[0x18, 0xFE], &[0x18, 0xFE], &[]);
    // sprite tiles 4-7: pen 0 (opaque)
    { let g = r.get_mut("gfx3").unwrap(); for t in 4..8 { for k in 0..64 { g[t * 64 + k] = 0; g[0x8000 + t * 64 + k] = 0; } } }
    let mut s = C1942System::new("1942", r).unwrap();
    let red = |s: &C1942System, x: usize, y: usize| s.framebuffer[((y - 16) * SCREEN_W + x) * 4];
    let rows = |s: &C1942System, x: usize| (16..240).filter(|&y| red(s, x, y) != red(s, 250, 20)).collect::<Vec<_>>();
    // sprite 0: code 4, colour 3, 1 tile tall, at (40, 100)
    for (k, v) in [4u8, 0x03, 100, 40].iter().enumerate() { s.board.write(0xCC00 + k as u16, *v); }
    s.render();
    let r1 = rows(&s, 45);
    assert_eq!((r1.len(), r1[0]), (16, 100), "16 lines from Y: {r1:?}");
    // 4 tiles tall (height bits 2)
    s.board.write(0xCC01, 0x83);
    s.render();
    assert_eq!(rows(&s, 45).len(), 64, "64 lines");
    // sprite 16 (byte 0x40) low on the screen: hidden; high: shown
    for k in 0..4 { s.board.write(0xCC00 + k, 0); }
    for (k, v) in [4u8, 0x03, 200, 120].iter().enumerate() { s.board.write(0xCC40 + k as u16, *v); }
    s.render();
    assert!(rows(&s, 125).is_empty(), "sprite 16 isn't drawn below line 128");
    s.board.write(0xCC42, 60);
    s.render();
    assert_eq!(rows(&s, 125).len(), 16, "sprite 16 drawn above line 128");
}
