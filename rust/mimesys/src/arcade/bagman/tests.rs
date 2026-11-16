use super::bagman_system::*;
use crate::common::system::System;
use crate::common::input as pad;
use std::collections::HashMap;

fn regions(prog: &[u8]) -> HashMap<String, Vec<u8>> {
    let mut rom = vec![0u8; 0x10000];
    rom[..prog.len()].copy_from_slice(prog);
    let mut r = HashMap::new();
    r.insert("maincpu".to_string(), rom);
    r.insert("gfx1".to_string(), vec![0u8; 0x2000]);
    r.insert("gfx2".to_string(), vec![0u8; 0x2000]);
    let prom: Vec<u8> = (0..0x40).map(|i| i as u8 * 4).collect();
    r.insert("proms".to_string(), prom);
    r
}

/// The PAL16R6 counter: every output starts high, it moves on with each read and
/// comes back round, never sticking.
#[test]
fn pal_counts_on_reads() {
    let mut p = Pal16r6::new();
    let seq: Vec<u8> = (0..200).map(|_| p.read()).collect();
    assert!(seq.iter().all(|&v| v < 0x40), "six outputs");
    let distinct: std::collections::BTreeSet<u8> = seq.iter().copied().collect();
    assert!(distinct.len() > 4, "it moves: {seq:02X?}");
    // the same from reset again
    let mut q = Pal16r6::new();
    assert!((0..200).all(|i| q.read() == seq[i]));
}

/// The vblank interrupt (IM 1, RST 38) once the game sets the enable latch at A000; the
/// joystick through the AY's port A; the DIP switches at B000.
#[test]
fn vblank_irq_inputs_and_dips() {
    // 0000: IM 1; LD A,1; LD (A000),A; EI; loop: JR loop
    // 0038: LD HL,6000; INC (HL); XOR A; LD (A000),A; INC A; LD (A000),A;
    //       LD A,0E; OUT (08),A; IN A,(0C); LD (6001),A; LD A,(B000); LD (6002),A; EI; RETI
    let mut prog = vec![0u8; 0x60];
    let main = [0xED, 0x56, 0x3E, 0x01, 0x32, 0x00, 0xA0, 0xFB, 0x18, 0xFE];
    prog[..main.len()].copy_from_slice(&main);
    let isr = [0x21, 0x00, 0x60, 0x34, 0xAF, 0x32, 0x00, 0xA0, 0x3C, 0x32, 0x00, 0xA0,
               0x3E, 0x0E, 0xD3, 0x08, 0xDB, 0x0C, 0x32, 0x01, 0x60, 0x3A, 0x00, 0xB0, 0x32, 0x02, 0x60,
               0xFB, 0xED, 0x4D];
    prog[0x38..0x38 + isr.len()].copy_from_slice(&isr);
    let mut s = BagmanSystem::new(Variant::Bagman, "bagman", regions(&prog)).unwrap();
    for _ in 0..10 { s.run_frame(pad::LEFT | pad::A); }
    let ram = |s: &mut BagmanSystem, a: u16| crate::common::cpu::z80::Bus::read(&mut s.bus, a);
    assert_eq!(ram(&mut s, 0x6000), 10, "one interrupt a frame");
    assert_eq!(ram(&mut s, 0x6001), !(0x08 | 0x80), "left and the button, active low");
    assert_eq!(ram(&mut s, 0x6002), 0xFE, "default DIP switches");
    assert_eq!(s.framebuffer().len(), SCREEN_W * SCREEN_H * 4);
}

/// A tile and a sprite land where the hardware puts them, in their PROM colours.
#[test]
fn tile_and_sprite() {
    let mut r = regions(&[0x18, 0xFE]);
    // character 1: high plane all set (pen 2); sprite 1 (bytes 32-63): low plane set (pen 1)
    let g = r.get_mut("gfx1").unwrap();
    for y in 0..8 { g[8 + y] = 0xFF; }
    for k in 0..32 { g[0x1000 + 32 + k] = 0xFF; }
    let mut s = BagmanSystem::new(Variant::Bagman, "bagman", r).unwrap();
    // tile (3, 4): code 1, colour 2 -> pen 2*4+2 = 10
    s.bus.videoram[4 * 32 + 3] = 1;
    s.bus.colorram[4 * 32 + 3] = 2;
    // sprite 0: code 1, colour 3, x 100, y = 256 - 16 - 120 -> top at line 120
    s.bus.colorram[0] = 1;
    s.bus.colorram[1] = 3;
    s.bus.colorram[2] = 120;
    s.bus.colorram[3] = 100;
    s.render();
    let px = |s: &BagmanSystem, x: usize, y: usize| { let o = ((y - 16) * SCREEN_W + x) * 4; [s.framebuffer[o], s.framebuffer[o + 1], s.framebuffer[o + 2]] };
    let tile = px(&s, 3 * 8 + 1, 4 * 8 + 1);
    let spr = px(&s, 105, 125);
    let bg = px(&s, 200, 200);
    assert_ne!(tile, bg);
    assert_ne!(spr, bg);
    assert_ne!(spr, tile);
}

/// The counter's first steps, as the fuse map works out (checked against a gate-level
/// evaluation of the fuse map): q6 counts on bit 0 ... q1 on bit 5, bit-reversed.
#[test]
fn pal_sequence() {
    let mut p = Pal16r6::new();
    let want = [0x00, 0x20, 0x10, 0x30, 0x08, 0x28, 0x18, 0x38, 0x04, 0x24, 0x14, 0x34, 0x0C, 0x2C, 0x1C, 0x3C, 0x02];
    for (i, &w) in want.iter().enumerate() { assert_eq!(p.read(), w, "read {i}"); }
}
