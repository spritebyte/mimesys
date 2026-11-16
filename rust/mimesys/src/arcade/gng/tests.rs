use super::gng_system::*;
use crate::common::input as pad;
use crate::common::system::System;
use crate::common::cpu::m6809::M6809Bus;
use std::collections::HashMap;

/// A board with `main` at 6000 (reset vector there, IRQ vector at `irq`) and `snd` at 0.
fn regions(main: &[u8], irq: u16, snd: &[u8]) -> HashMap<String, Vec<u8>> {
    let mut rom = vec![0x12u8; 0x18000]; // NOP
    rom[0x6000..0x6000 + main.len()].copy_from_slice(main);
    rom[0xFFFE] = 0x60; rom[0xFFFF] = 0x00;
    rom[0xFFF8] = (irq >> 8) as u8; rom[0xFFF9] = irq as u8;
    // banked ROM: bank 2 starts with 0xB2, bank "4" (= 0x4000) with 0xB4
    rom[0x14000] = 0xB2;
    rom[0x4000] = 0xB4;
    let mut s = vec![0u8; 0x8000];
    s[..snd.len()].copy_from_slice(snd);
    let mut r = HashMap::new();
    r.insert("maincpu".into(), rom);
    r.insert("audiocpu".into(), s);
    r.insert("chars".into(), vec![0u8; 0x4000]);
    r.insert("tiles".into(), vec![0u8; 0x18000]);
    r.insert("sprites".into(), vec![0xFFu8; 0x20000]);
    r
}

/// The vblank IRQ comes once a frame; the inputs and DIP switches read where they
/// should; the bank switch picks the ROM banks; the sound CPU stays in reset until the
/// latch lets it go, then gets its four interrupts a frame and plays through the YM2203s.
#[test]
fn irq_inputs_banks_and_sound() {
    // 6000: LDS #1D00; LDA #2; STA 3E00; LDA 4000; STA 0010; LDA #1; STA 3D01 (sound on);
    //       ANDCC #EF; loop: BRA loop
    // 6040: IRQ: INC 0000; LDA 3001; STA 0001; LDA 3003; STA 0002; RTI
    let mut main = vec![0x12u8; 0x60];
    let prog = [0x10, 0xCE, 0x1D, 0x00, 0x86, 0x02, 0xB7, 0x3E, 0x00, 0xB6, 0x40, 0x00, 0xB7, 0x00, 0x10,
                0x86, 0x01, 0xB7, 0x3D, 0x01, 0x1C, 0xEF, 0x20, 0xFE];
    main[..prog.len()].copy_from_slice(&prog);
    let isr = [0x7C, 0x00, 0x00, 0xB6, 0x30, 0x01, 0xB7, 0x00, 0x01, 0xB6, 0x30, 0x03, 0xB7, 0x00, 0x02, 0x3B];
    main[0x40..0x40 + isr.len()].copy_from_slice(&isr);
    // sound: 0000: LD SP,C800; IM 1; EI; then set YM1 SSG channel A: tone period 0x80,
    //        volume 15, mixer tone A only; loop: JR loop
    //        0038: LD HL,C000; INC (HL); EI; RETI
    let mut snd = vec![0u8; 0x40];
    let mut p = vec![0x31, 0x00, 0xC8, 0xED, 0x56, 0xFB];
    for (r, v) in [(0x00u8, 0x80u8), (0x01, 0x00), (0x07, 0x3E), (0x08, 0x0F)] {
        p.extend([0x3E, r, 0x32, 0x00, 0xE0, 0x3E, v, 0x32, 0x01, 0xE0]);
    }
    p.extend([0x18, 0xFE]);
    snd[..p.len()].copy_from_slice(&p);
    snd[0x38..0x3E].copy_from_slice(&[0x21, 0x00, 0xC0, 0x34, 0xFB, 0xED]);
    let mut snd = snd;
    snd.push(0x4D);
    let mut s = GngSystem::new(Variant::Gng, "gng", regions(&main, 0x6040, &snd)).unwrap();
    for _ in 0..10 { s.run_frame(pad::LEFT | pad::B); }
    assert_eq!(s.board.read(0x0000), 10, "one IRQ a frame");
    assert_eq!(s.board.read(0x0001), !(0x02 | 0x10), "left and fire, active low");
    assert_eq!(s.board.read(0x0002), 0xDF, "DIP switches 1");
    assert_eq!(s.board.read(0x0010), 0xB2, "bank 2");
    let snd_irqs = crate::common::cpu::z80::Bus::read(&mut s.sound_bus(), 0xC000);
    assert!((39..=41).contains(&snd_irqs), "four sound IRQs a frame: {snd_irqs}");
    let audio = s.drain_audio();
    assert!(audio.len() > 1400, "about 10 frames of 44.1 kHz stereo: {}", audio.len());
    let peak = audio.iter().fold(0f32, |m, v| m.max(v.abs()));
    assert!(peak > 0.05, "the SSG tone plays: {peak}");
}

/// A background tile, a sprite in front of it, a text character in front of both, and a
/// background tile with the priority bit drawn over the sprite.
#[test]
fn layers_and_priority() {
    let mut r = regions(&[0x20, 0xFE], 0x6000, &[0x18, 0xFE]);
    // text: pen 3 (see-through) everywhere but char 1, pen 0
    { let c = r.get_mut("chars").unwrap(); c.fill(0xFF); for k in 0..16 { c[16 + k] = 0; } }
    // background tile 1: pen 1 (the low plane is the region's first third)
    { let t = r.get_mut("tiles").unwrap(); for k in 0..32 { t[32 + k] = 0xFF; } }
    // sprite 1: pen 0 (opaque); the rest pen 15 (see-through)
    { let sp = r.get_mut("sprites").unwrap(); for k in 0..64 { sp[64 + k] = 0; sp[0x10000 + 64 + k] = 0; } }
    let mut s = GngSystem::new(Variant::Gng, "gng", r).unwrap();
    let b = &mut s.board;
    b.write(0x3D00, 1); // flip off
    // red 1 for the background, 2 for the sprite, 3 for the text
    for (i, v) in [(0x01u16, 0x10u8), (0x40, 0x20), (0x80, 0x30)] { b.write(0x3800 + i, v); b.write(0x3900 + i, 0); }
    for i in 0..0x400 { b.write(0x2800 + i, 1); b.write(0x2C00 + i, 0); }
    // sprite 0: code 1 at (100, 100)
    for (k, v) in [1u8, 0, 100, 100].iter().enumerate() { b.write(0x1E00 + k as u16, *v); }
    b.write(0x3C00, 0);
    // text char 1 at column 2, row 5
    b.write(0x2000 + 5 * 32 + 2, 1);
    let red = |s: &GngSystem, x: usize, y: usize| s.framebuffer[((y - 16) * SCREEN_W + x) * 4];
    s.render();
    assert_eq!(red(&s, 50, 150), 17, "background");
    assert_eq!(red(&s, 105, 110), 34, "sprite over the background");
    assert_eq!(red(&s, 2 * 8 + 3, 5 * 8 + 3), 51, "text over everything");
    // the background tile under the sprite with the priority bit: its pen 1 goes in front
    s.board.write(0x2C00 + 6 * 32 + 6, 0x08);
    s.render();
    assert_eq!(red(&s, 105, 110), 17, "priority tile over the sprite");
    assert_eq!(red(&s, 99 + 16, 110), 34, "the sprite's other tile");
}
