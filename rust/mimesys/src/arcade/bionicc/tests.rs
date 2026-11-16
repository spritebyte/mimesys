use super::bionicc_system::*;
use crate::common::cpu::m68000::M68kBus;
use crate::common::cpu::z80::Bus;
use crate::common::input as pad;
use crate::common::system::System;
use std::collections::HashMap;

fn w16(rom: &mut [u8], a: usize, words: &[u16]) {
    for (k, w) in words.iter().enumerate() {
        rom[a + 2 * k] = (w >> 8) as u8;
        rom[a + 2 * k + 1] = *w as u8;
    }
}

fn regions(main: Vec<u8>, snd: &[u8], mcu: &[u8]) -> HashMap<String, Vec<u8>> {
    let mut s = vec![0u8; 0x8000];
    s[..snd.len()].copy_from_slice(snd);
    let mut m = vec![0u8; 0x1000];
    m[..mcu.len()].copy_from_slice(mcu);
    let mut r = HashMap::new();
    r.insert("maincpu".into(), main);
    r.insert("audiocpu".into(), s);
    r.insert("mcu".into(), m);
    // text: everything pen 3 (see-through) but tile 1, pen 1
    let mut g1 = vec![0xFFu8; 0x8000];
    g1[16..32].fill(0xF0);
    r.insert("gfx1".into(), g1);
    // background: tile 0 pen 2 (bit 1: the low nibbles of the first half), the rest pen 0
    let mut g2 = vec![0u8; 0x10000];
    g2[..16].fill(0x0F);
    r.insert("gfx2".into(), g2);
    // foreground: tile 0 see-through (pen 15), tile 1 pen 3
    let mut g3 = vec![0u8; 0x40000];
    g3[..64].fill(0xFF);
    g3[0x20000..0x20040].fill(0xFF);
    g3[64..128].fill(0xFF);
    r.insert("gfx3".into(), g3);
    r.insert("spritegen".into(), vec![0u8; 0x40000]);
    r
}

/// IRQ 2 once a frame, IRQ 4 reads the inputs; a write to E801A halts the 68000 until the
/// MCU has done its DMA; the sound NMI; commands both ways through the MCU's latches.
#[test]
fn irqs_inputs_mcu_dma_and_sound() {
    let mut rom = vec![0u8; 0x40000];
    w16(&mut rom, 0, &[0x000F, 0xFF00, 0x0000, 0x0400]);     // SSP FFF00, PC 400
    w16(&mut rom, 26 * 4, &[0x0000, 0x0500]);                // autovector 2
    w16(&mut rom, 28 * 4, &[0x0000, 0x0520]);                // autovector 4
    w16(&mut rom, 0x400, &[
        0x46FC, 0x2000,                         // MOVE #$2000,SR
        0x33FC, 0x0001, 0x000E, 0x801A,         // MOVE.W #1,$E801A    (start the DMA)
        0x5279, 0x000F, 0xC006,                 // ADDQ.W #1,$FC006
        0x13F9, 0x000E, 0x0E21, 0x000F, 0xC009, // MOVE.B $E0E21,$FC009 (what the MCU left)
        0x13FC, 0x0000, 0x000E, 0x4002,         // MOVE.B #0,$E4002    (sound NMI)
        0x60FE,                                 // BRA *
    ]);
    w16(&mut rom, 0x500, &[0x5279, 0x000F, 0xC000, 0x4E73]);  // ADDQ.W #1,$FC000; RTE
    w16(&mut rom, 0x520, &[
        0x33F9, 0x000E, 0x4000, 0x000F, 0xC002, // MOVE.W $E4000,$FC002
        0x5279, 0x000F, 0xC004, 0x4E73,         // ADDQ.W #1,$FC004; RTE
    ]);

    // MCU: INT0 increments xdata 0010 (= 68000 E3E21) and lets the 68000 go; the main
    // loop keeps the Z80's byte at 40h and latches 5A for it
    let mut mcu = vec![0u8; 0x50];
    mcu[0..3].copy_from_slice(&[0x02, 0x00, 0x30]);
    mcu[3..14].copy_from_slice(&[0x90, 0x00, 0x10, 0xE0, 0x04, 0xF0, 0xC2, 0xB0, 0xD2, 0xB0, 0x32]);
    mcu[0x30..0x45].copy_from_slice(&[
        0xC2, 0xB5, 0x75, 0xA8, 0x81,                   // CLR P3.5; MOV IE,#81
        0x75, 0x90, 0xFF, 0xE5, 0x90, 0xF5, 0x40,       // MOV P1,#FF; MOV A,P1; MOV 40h,A
        0x75, 0x90, 0x5A, 0xC2, 0xB6, 0xD2, 0xB6,       // MOV P1,#5A; CLR P3.6; SETB P3.6
        0x80, 0xF0,                                     // SJMP
    ]);

    // Z80: 77 to the MCU, then copy what comes back to C000; NMI counts at C001
    let mut snd = vec![0u8; 0x70];
    snd[..16].copy_from_slice(&[0x31, 0x00, 0xC8, 0x3E, 0x77, 0x32, 0x00, 0xA0, 0x3A, 0x00, 0xA0, 0x32, 0x00, 0xC0, 0x18, 0xF8]);
    snd[0x66..0x6C].copy_from_slice(&[0x21, 0x01, 0xC0, 0x34, 0xED, 0x45]);

    let mut s = BioniccSystem::new("bionicc", regions(rom, &snd, &mcu)).unwrap();
    for _ in 0..10 { s.run_frame(pad::UP | pad::B); }
    let b = &mut s.board;
    assert_eq!(b.read_word(0xFC000), 10, "one vblank IRQ a frame");
    assert_eq!(b.read_word(0xFC004), 10, "one input IRQ a frame");
    assert_eq!(b.read_word(0xFC002), !(0x0800 | 0x0080), "up and fire, active low");
    assert_eq!(b.read_word(0xFC006), 1, "the 68000 went on after the DMA");
    assert_eq!(b.read_byte(0xFC009), 1, "the MCU's write landed before it did");
    assert_eq!(s.mcu.iram[0x40], 0x77, "Z80 -> MCU");
    let mut sb = s.sound_bus();
    assert_eq!(sb.read(0xC001), 1, "one sound NMI");
    assert_eq!(sb.read(0xC000), 0x5A, "MCU -> Z80");
    sb.write(0x8000, 0x08);
    sb.write(0x8001, 0x00);
    assert_eq!(sb.read(0x8000) & 0x80, 0x80, "the YM2151 is busy after a data write");
}

/// The draw order: background, foreground behind the sprites, sprites, the foreground's
/// front pens, text.
#[test]
fn layers_and_priorities() {
    let mut rom = vec![0u8; 0x40000];
    w16(&mut rom, 0, &[0x000F, 0xFF00, 0x0000, 0x0400]);
    w16(&mut rom, 0x400, &[0x60FE]);
    let mut s = BioniccSystem::new("bionicc", regions(rom, &[0x18, 0xFE], &[0x80, 0xFE])).unwrap();
    let rgb = |s: &BioniccSystem, x: usize, y: usize| -> [u8; 3] {
        let o = (y * SCREEN_W + x) * 4;
        [s.framebuffer[o], s.framebuffer[o + 1], s.framebuffer[o + 2]]
    };
    let b = &mut s.board;
    b.write_word(0xF8000 + 2 * 2, 0xF008);          // bg pen 2: red
    b.write_word(0xF8000 + 2 * 259, 0x0F08);        // fg pen 3: green
    b.write_word(0xF8000 + 2 * 512, 0x00F8);        // sprite pen 0: blue
    b.write_word(0xF8000 + 2 * 769, 0xFFF8);        // text pen 1: white
    // all sprites off screen
    for i in 0..80 { b.write_word(0xE0800 + 8 * i + 4, 0x100); }
    b.write_word(0xE8018, 0);
    s.render();
    assert_eq!(rgb(&s, 100, 100), [255, 0, 0], "background");

    // foreground tile 1 at map (6, 7) = pixels 96-111 x 112-127 = screen rows 96-111
    let b = &mut s.board;
    b.write_word(0xF0000 + 4 * (7 * 64 + 6), 1);
    s.render();
    assert_eq!(rgb(&s, 100, 100), [0, 255, 0], "foreground over the background");

    // a sprite there: in front of group 0 ...
    let b = &mut s.board;
    for (k, v) in [0u16, 0, 112, 96].iter().enumerate() { b.write_word(0xE0800 + 2 * k as u32, *v); }
    b.write_word(0xE8018, 0);
    s.render();
    assert_eq!(rgb(&s, 100, 100), [0, 0, 255], "sprite over foreground group 0");
    // ... behind group 1's pens 1-5
    s.board.write_word(0xF0000 + 4 * (7 * 64 + 6) + 2, 0x20);
    s.render();
    assert_eq!(rgb(&s, 100, 100), [0, 255, 0], "group 1 pen 3 in front of the sprite");

    // text tile 1 at (12, 14) covers it all
    s.board.write_word(0xEC000 + 2 * (14 * 32 + 12), 1);
    s.render();
    assert_eq!(rgb(&s, 100, 100), [255, 255, 255], "text on top");
    assert_eq!(rgb(&s, 0, 100), [255, 0, 0], "text pen 3 is see-through");

    // background scroll: x 8 moves pen-2 tile 0... everything's tile 0, so scroll the
    // foreground instead: x 16 brings the tile to screen x 80-95
    s.board.write_word(0xEC000 + 2 * (14 * 32 + 12), 0);
    s.board.write_word(0xE8010, 16);
    s.render();
    assert_eq!(rgb(&s, 85, 100), [0, 255, 0], "foreground x scroll");
    assert_eq!(rgb(&s, 100, 100), [0, 0, 255], "the sprite doesn't scroll");

    // brightness: with its top bit clear, half the colour plus 18.15 a step
    s.board.write_word(0xF8000 + 2 * 2, 0x8804);
    s.render();
    assert_eq!(rgb(&s, 10, 10)[0], (0x88 / 2 + (4.0 * 18.15) as u8) as u8);
    // flip
    s.board.write_byte(0xE4000, 1);
    s.render();
    assert_eq!(rgb(&s, 255 - 85, 223 - 100), [0, 255, 0], "flipped");
}

#[test]
fn save_state_round_trip() {
    let mut rom = vec![0u8; 0x40000];
    w16(&mut rom, 0, &[0x000F, 0xFF00, 0x0000, 0x0400]);
    w16(&mut rom, 0x400, &[0x5279, 0x000F, 0xC000, 0x60F8]);   // ADDQ.W #1,$FC000; BRA
    let mut s = BioniccSystem::new("bionicc", regions(rom, &[0x18, 0xFE], &[0x80, 0xFE])).unwrap();
    for _ in 0..3 { s.run_frame(0); }
    let st = s.save_state();
    let before = s.board.read_word(0xFC000);
    s.run_frame(0);
    s.load_state(&st).unwrap();
    assert_eq!(s.board.read_word(0xFC000), before);
    s.run_frame(0);
    assert_ne!(s.board.read_word(0xFC000), before);
}

/// Sets without the MCU dump: MAME 0.139's stand-in puts the controls at FFFFA-FFFFF
/// (active high) on a write to E801A, and passes FFFF8 straight to the Z80.
#[test]
fn without_the_mcu_dump() {
    let mut rom = vec![0u8; 0x40000];
    w16(&mut rom, 0, &[0x000F, 0xFF00, 0x0000, 0x0400]);
    w16(&mut rom, 0x400, &[
        0x33FC, 0x0001, 0x000E, 0x801A,         // MOVE.W #1,$E801A
        0x33FC, 0x0042, 0x000F, 0xFFF8,         // MOVE.W #$42,$FFFF8
        0x60FE,
    ]);
    // Z80: copy the latch to C000
    let snd = [0x3A, 0x00, 0xA0, 0x32, 0x00, 0xC0, 0x18, 0xF8];
    let mut r = regions(rom, &snd, &[]);
    r.remove("mcu");
    let mut s = BioniccSystem::new("bionicc", r).unwrap();
    s.run_frame(pad::COIN | pad::LEFT | pad::A | (pad::UP << pad::P2_SHIFT));
    let b = &mut s.board;
    assert_eq!(b.read_word(0xFFFFA), 0x8, "coin 1");
    assert_eq!(b.read_word(0xFFFFC), 0x20, "player 2 up");
    assert_eq!(b.read_word(0xFFFFE), 0x08 | 0x01, "player 1 left and button 2");
    assert_eq!(b.read_word(0xFFFF8), 0x42);
    assert_eq!(s.sound_bus().read(0xC000), 0x42, "the sound command");
}
