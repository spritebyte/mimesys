use super::gunsmoke_system::*;
use crate::common::cpu::z80::Bus;
use crate::common::input as pad;
use crate::common::system::System;
use std::collections::HashMap;

fn regions(main: &[u8], snd: &[u8]) -> HashMap<String, Vec<u8>> {
    let mut rom = vec![0u8; 0x18000];
    rom[..main.len()].copy_from_slice(main);
    rom[0x8000 + 2 * 0x4000] = 0xB2; // bank 2's first byte
    let mut s = vec![0u8; 0x8000];
    s[..snd.len()].copy_from_slice(snd);
    // red from the colour's top nibble; lookups pass the low nibble
    let mut proms = vec![0u8; 0xA00];
    for i in 0..0x100 { proms[i] = (i >> 4) as u8; }
    for i in 0x300..0x800 { proms[i] = (i & 15) as u8; }
    let mut r = HashMap::new();
    r.insert("maincpu".into(), rom);
    r.insert("audiocpu".into(), s);
    r.insert("chars".into(), vec![0u8; 0x4000]);
    r.insert("tiles".into(), vec![0u8; 0x40000]);
    r.insert("sprites".into(), vec![0u8; 0x40000]);
    r.insert("bgtiles".into(), vec![0u8; 0x8000]);
    r.insert("proms".into(), proms);
    r
}

/// The vblank IRQ once a frame, four for the sound CPU; inputs; the ROM bank; the
/// protection bytes; the YM2203s.
#[test]
fn irqs_inputs_bank_protection_and_sound() {
    // 0000: LD SP,F000; IM 1; LD A,08; LD (C804),A (bank 2); LD A,(8000); LD (E002),A;
    //       LD A,(C4C9); LD (E003),A; EI; JR $      0038: INC (E000); input -> E001; EI; RETI
    let prog = [0x31, 0x00, 0xF0, 0xED, 0x56, 0x3E, 0x08, 0x32, 0x04, 0xC8, 0x3A, 0x00, 0x80, 0x32, 0x02, 0xE0,
                0x3A, 0xC9, 0xC4, 0x32, 0x03, 0xE0, 0xFB, 0x18, 0xFE];
    let mut main = vec![0u8; 0x50];
    main[..prog.len()].copy_from_slice(&prog);
    main[0x38..0x46].copy_from_slice(&[0x21, 0x00, 0xE0, 0x34, 0x3A, 0x01, 0xC0, 0x32, 0x01, 0xE0, 0xFB, 0xED, 0x4D, 0x00]);
    let mut p = vec![0x31, 0x00, 0xC8, 0xED, 0x56, 0xFB];
    for (r, v) in [(0x00u8, 0x80u8), (0x01, 0x00), (0x07, 0x3E), (0x08, 0x0F)] {
        p.extend([0x3E, r, 0x32, 0x00, 0xE0, 0x3E, v, 0x32, 0x01, 0xE0]);
    }
    p.extend([0x18, 0xFE]);
    let mut snd = vec![0u8; 0x50 + p.len()];
    snd[..3].copy_from_slice(&[0xC3, 0x50, 0x00]);
    snd[0x50..].copy_from_slice(&p);
    snd[0x38..0x3F].copy_from_slice(&[0x21, 0x00, 0xC0, 0x34, 0xFB, 0xED, 0x4D]);
    let mut s = GunsmokeSystem::new("gunsmoke", regions(&main, &snd)).unwrap();
    for _ in 0..10 { s.run_frame(pad::UP | pad::Y | pad::A); }
    assert_eq!(s.board.read(0xE000), 10, "one vblank IRQ a frame");
    assert_eq!(s.board.read(0xE001), !(0x08 | 0x10 | 0x40), "up, shoot left and right, active low");
    assert_eq!(s.board.read(0xE002), 0xB2, "bank 2");
    assert_eq!(s.board.read(0xE003), 0xFF, "protection byte");
    let n = s.sound_bus().read(0xC000);
    assert!((39..=41).contains(&n), "four sound IRQs a frame: {n}");
    let audio = s.drain_audio();
    assert!(audio.len() > 1400);
    assert!(audio.iter().fold(0f32, |m, v| m.max(v.abs())) > 0.03, "the SSG tone plays");
}

/// Layers switch on and off; the text is see-through where its colour looks up to 0x4F.
#[test]
fn layers_and_text_transparency() {
    let mut r = regions(&[0x18, 0xFE], &[0x18, 0xFE]);
    // text colour 3: pen 0 looks up to 0x4F (see-through), the rest to 0x40-0x4E
    { let p = r.get_mut("proms").unwrap(); p[0x300 + 3 * 4] = 0x0F; }
    // sprite tile 1: pen 1 everywhere (the lowest plane: the high nibbles of the first half)
    { let g = r.get_mut("sprites").unwrap(); for k in 0..64 { g[64 + k] = 0xF0; } }
    let mut s = GunsmokeSystem::new("gunsmoke", r).unwrap();
    let red = |s: &GunsmokeSystem, x: usize, y: usize| s.framebuffer[((y - 16) * SCREEN_W + x) * 4];
    s.render();
    assert_eq!(red(&s, 100, 100), 0, "everything off: black");
    // background on: tile 0 pen 0 -> colour 0 (red 0); text on with colour 3 = see-through
    for i in 0..0x400 { s.board.write(0xD400 + i, 3); }
    s.board.write(0xD806, 0x30);
    s.board.write(0xC804, 0x80);
    // sprite 0: tile 1, colour 2, at (50, 60)
    for (k, v) in [1u8, 0x02, 60, 50].iter().enumerate() { s.board.write(0xF000 + k as u16, *v); }
    s.board.write(0xC806, 0);
    s.render();
    // sprite colour 2 pen 1 looks up to 0x80 | 1 | (1 << 4) = 0x91: red 9
    assert_eq!(red(&s, 55, 65), 9 * 17, "sprite through the see-through text");
    assert_eq!(red(&s, 150, 150), 0, "background");
    // text colour 4: pen 0 looks up to 0x40 (opaque)
    for i in 0..0x400 { s.board.write(0xD400 + i, 4); }
    s.render();
    assert_eq!(red(&s, 55, 65), 4 * 17, "opaque text (0x40) on top");
}
