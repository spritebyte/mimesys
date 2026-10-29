//! Return of the Jedi tests with small hand-written programs (no game ROMs needed).

use std::collections::HashMap;
use crate::arcade::jedi::jedi_system::JediSystem;
use crate::common::system::System;

/// Main program at 8000 (IRQ handler at 8800), sound program at 8000 of its own ROM.
fn board(main: &[u8], irq: &[u8], sound: &[u8], tweak: impl Fn(&mut HashMap<String, Vec<u8>>)) -> JediSystem {
    let mut rom = vec![0u8; 0x1C000];
    rom[0x8000..0x8000 + main.len()].copy_from_slice(main);
    rom[0x8800..0x8800 + irq.len()].copy_from_slice(irq);
    rom[0xFFFC..0xFFFE].copy_from_slice(&[0x00, 0x80]);
    rom[0xFFFE..0x10000].copy_from_slice(&[0x00, 0x88]);
    for page in 0..3 { rom[0x10000 + page * 0x4000] = 0xA0 + page as u8; }
    let mut snd = vec![0u8; 0x10000];
    snd[0x8000..0x8000 + sound.len()].copy_from_slice(sound);
    snd[0xFFFC..0xFFFE].copy_from_slice(&[0x00, 0x80]);
    snd[0xFFFE..0x10000].copy_from_slice(&[0x00, 0x88]);
    snd[0x8800] = 0x40;                                 // IRQ: RTI
    let mut r: HashMap<String, Vec<u8>> = HashMap::new();
    r.insert("maincpu".into(), rom);
    r.insert("audiocpu".into(), snd);
    r.insert("tx_gfx".into(), vec![0; 0x2000]);
    r.insert("bg_gfx".into(), vec![0; 0x10000]);
    r.insert("spr_gfx".into(), vec![0; 0x20000]);
    r.insert("proms".into(), vec![0; 0x1000]);
    tweak(&mut r);
    JediSystem::new("jedi", r).unwrap()
}

const SPIN: [u8; 3] = [0x4C, 0x00, 0x80];   // JMP 8000

#[test]
fn rom_pages_irqs_and_sound_latches() {
    let main = [
        0xA9, 0x02, 0x8D, 0x80, 0x1F,   // page 1
        0xAD, 0x00, 0x40, 0x85, 0x10,   // LDA 4000 -> $10
        0xA9, 0x80, 0x8D, 0x86, 0x1E,   // LS259 Q6 = 1: sound CPU runs
        0xA9, 0x5A, 0x8D, 0x00, 0x1F,   // command 5A
        0x58,                           // CLI
        0xAD, 0x00, 0x14, 0x85, 0x11,   // loop: LDA 1400 (reply) -> $11
        0x4C, 0x15, 0x80,
    ];
    let irq = [0xE6, 0x12, 0x8D, 0x00, 0x1E, 0x40];   // INC $12; STA 1E00 (ack); RTI
    // sound: wait for the command, echo it + 1
    let sound = [
        0xAD, 0x01, 0x1C, 0x10, 0xFB,   // LDA 1C01; BPL (wait for "command pending")
        0xAD, 0x00, 0x18, 0x18, 0x69, 0x01, 0x8D, 0x00, 0x14,  // LDA 1800; CLC; ADC #1; STA 1400
        0x4C, 0x0E, 0x80,
    ];
    let mut s = board(&main, &irq, &sound, |_| {});
    for _ in 0..3 { s.run_frame(0); }
    assert_eq!(s.bus.read_ram(0x10), 0xA1, "page 1 at 4000");
    assert_eq!(s.bus.read_ram(0x11), 0x5B, "sound CPU replied");
    let n = s.bus.read_ram(0x12);
    s.run_frame(0);
    assert_eq!(s.bus.read_ram(0x12) - n, 4, "4 IRQs a frame (32V)");
}

#[test]
fn background_and_sprite_pens() {
    // palette: pen 0x001 red-ish (intensity 7, red 7), pen 0x011 green; others black
    let main = [
        0xA9, 0xC0, 0x8D, 0x01, 0x28,   // 2801 = C0 (red bits 6-7)
        0xA9, 0x0F, 0x8D, 0x01, 0x2C,   // 2C01 = 0F (red MSB + intensity 7)
        0xA9, 0x38, 0x8D, 0x11, 0x28,   // 2811 = 38 (green 7)
        0xA9, 0x0E, 0x8D, 0x11, 0x2C,   // 2C11 = 0E (intensity 7)
        // sprite 0: picture 1, Y such that the top is line 100, X = 50
        0xA9, 0x01, 0x8D, 0xC0, 0x37,
        0xA9, 0x00, 0x8D, 0x00, 0x38,
        0xA9, 0x8D, 0x8D, 0x40, 0x38,   // 240 - 0x8D + 1 = 100
        0xA9, 0x34, 0x8D, 0xC0, 0x38,   // x = 0x34 - 2 = 50
        0x4C, 0x28, 0x80,
    ];
    let mut s = board(&main, &[0x40], &SPIN, |r| {
        // background tile 0, plane 2 (bit 0 of the pen) set everywhere
        r.get_mut("bg_gfx").unwrap()[0x8000..0x8010].fill(0x0F);
        // smoothing PROMs pass the current pixel through
        let p = r.get_mut("proms").unwrap();
        for i in 0..0x400 { p[i] = (i & 0x0F) as u8; p[0x800 + i] = (i & 0x0F) as u8; }
        // sprite 1, first row: all 8 pixels pen bit 4 (second plane, low nibble)
        r.get_mut("spr_gfx").unwrap()[0x10020..0x10022].fill(0x0F);
    });
    for _ in 0..2 { s.run_frame(0); }
    let px = |x: usize, y: usize| { let o = (y * 296 + x) * 4; s.framebuffer()[o..o + 3].to_vec() };
    assert_eq!(px(10, 10), vec![245, 0, 0], "background pen 1");
    assert_eq!(px(50, 100), vec![0, 245, 0], "sprite adds pen bits 4-7");
    assert_eq!(px(50, 101), vec![245, 0, 0], "sprite row 2 is empty");
}

#[test]
fn novram_store_and_battery() {
    let main = [
        0xA9, 0x42, 0x8D, 0x05, 0x08,   // NOVRAM[5] = 42
        0x8D, 0x00, 0x1D,               // STORE
        0x4C, 0x08, 0x80,
    ];
    let mut s = board(&main, &[0x40], &SPIN, |_| {});
    assert_eq!(s.get_sram().unwrap()[0x50], 0x00, "MAME's default fill");
    s.run_frame(0);
    assert!(s.is_sram_dirty());
    assert_eq!(s.get_sram().unwrap()[5], 0x42);
}

#[test]
fn save_state_round_trip() {
    let main = [0xE6, 0x20, 0x4C, 0x00, 0x80];
    let mut s = board(&main, &[0x40], &SPIN, |_| {});
    for _ in 0..3 { s.run_frame(0); }
    let st = s.save_state();
    for _ in 0..3 { s.run_frame(0); }
    let a = (s.bus.read_ram(0x20), s.framebuffer().to_vec());
    s.load_state(&st).unwrap();
    for _ in 0..3 { s.run_frame(0); }
    assert_eq!((s.bus.read_ram(0x20), s.framebuffer().to_vec()), a);
}
