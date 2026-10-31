use std::collections::HashMap;
use super::rastan_system::RastanSystem;
use super::rastan_video::{Video, SCREEN_W};
use crate::common::system::System;

fn px(v: &Video, x: usize, y: usize) -> [u8; 4] {
    let i = (y * SCREEN_W + x) * 4;
    v.framebuffer[i..i + 4].try_into().unwrap()
}

#[test]
fn tilemaps_and_sprites() {
    let mut tiles = vec![0u8; 0x100];
    tiles[32..64].fill(0x33);                  // tile 1: pen 3 everywhere
    let mut sprites = vec![0u8; 0x200];
    sprites[128..256].fill(0x55);              // sprite 1: pen 5 everywhere
    let mut v = Video::new(tiles, sprites).unwrap();
    for i in 0..0x1000 { v.tile_ram[2 * i] = 2; v.tile_ram[2 * i + 1] = 1; }   // BG: colour 2, tile 1
    // FG: one tile, colour 4, at tilemap (16, 8) = screen (0, 0)
    v.tile_ram[0x4000 + 2 * 66] = 4;
    v.tile_ram[0x4000 + 2 * 66 + 1] = 1;
    v.palette[2 * 16 + 3] = 0x001F;            // red
    v.palette[4 * 16 + 3] = 0x03E0;            // green
    v.palette[16 + 5] = 0x7C00;                // blue
    v.palette[2 * 16 + 5] = 0x7FFF;            // white
    v.sprite_ram[0xDFF] = 1;                   // not flipped
    // sprite 0 at (40, 20) colour 1; sprite 1 at (48, 20) colour 2
    v.sprite_ram[0..4].copy_from_slice(&[1, 28, 1, 40]);
    v.sprite_ram[4..8].copy_from_slice(&[2, 28, 1, 48]);
    v.render();
    assert_eq!(px(&v, 0, 0), [0, 255, 0, 255], "foreground");
    assert_eq!(px(&v, 8, 0), [255, 0, 0, 255], "background through transparent foreground");
    assert_eq!(px(&v, 50, 25), [0, 0, 255, 255], "earlier sprite wins");
    assert_eq!(px(&v, 60, 25), [255, 255, 255, 255]);
    assert_eq!(px(&v, 40, 19), [255, 0, 0, 255], "above the sprite");

    // x scroll
    v.xscroll[0] = 8;                          // the register is subtracted: tilemap x 16 lands on screen x 8
    v.tile_ram[2 * 66] = 3;                    // colour 3 at tilemap (16, 8)
    v.palette[3 * 16 + 3] = 0x7FE0;            // cyan
    v.render();
    assert_eq!(px(&v, 8, 0), [0, 255, 255, 255]);
}

fn w16(rom: &mut [u8], a: usize, words: &[u16]) {
    for (k, w) in words.iter().enumerate() {
        rom[a + 2 * k] = (w >> 8) as u8;
        rom[a + 2 * k + 1] = *w as u8;
    }
}

fn board() -> RastanSystem {
    let mut rom = vec![0u8; 0x60000];
    w16(&mut rom, 0, &[0x0011, 0x0000, 0x0000, 0x0400]);    // SSP 110000, PC 400
    w16(&mut rom, 29 * 4, &[0x0000, 0x0500]);               // autovector 5
    w16(&mut rom, 0x400, &[
        0x13FC, 0x0000, 0x003E, 0x0001,      // MOVE.B #0,$3E0001   (PC060HA mode 0)
        0x13FC, 0x0005, 0x003E, 0x0003,      // MOVE.B #5,$3E0003
        0x13FC, 0x000A, 0x003E, 0x0003,      // MOVE.B #A,$3E0003   (mailbox 0/1 full)
        0x33FC, 0x7FFF, 0x0020, 0x0002,      // MOVE.W #$7FFF,$200002
        0x46FC, 0x2000,                      // MOVE #$2000,SR
        0x60FE,                              // BRA *
    ]);
    w16(&mut rom, 0x500, &[0x5279, 0x0010, 0xC000, 0x4E73]);  // ADDQ.W #1,$10C000; RTE

    let mut z80 = vec![0u8; 0x10000];
    let prog: &[u8] = &[
        0x3E, 0x06, 0x32, 0x00, 0xA0, 0x32, 0x01, 0xA0,          // LD A,6; LD (A000),A; LD (A001),A: NMI on
        0x18, 0xFE,                                             // JR $
    ];
    z80[..prog.len()].copy_from_slice(prog);
    let nmi: &[u8] = &[
        0xAF, 0x32, 0x00, 0xA0,                                 // XOR A; LD (A000),A
        0x3A, 0x01, 0xA0, 0x32, 0x00, 0x80,                     // LD A,(A001); LD (8000),A
        0x3A, 0x01, 0xA0, 0x32, 0x01, 0x80,                     // LD A,(A001); LD (8001),A
        0xED, 0x45,                                             // RETN
    ];
    z80[0x66..0x66 + nmi.len()].copy_from_slice(nmi);

    let mut r = HashMap::new();
    r.insert("maincpu".to_string(), rom);
    r.insert("audiocpu".to_string(), z80);
    r.insert("pc080sn".to_string(), vec![0; 0x80000]);
    r.insert("pc090oj".to_string(), vec![0; 0x80000]);
    r.insert("adpcm".to_string(), vec![0; 0x10000]);
    RastanSystem::new("rastan", r).unwrap()
}

#[test]
fn vblank_irq_palette_and_sound_link() {
    let mut s = board();
    for _ in 0..3 { s.run_frame(0); }
    assert_eq!(s.bus.ram_word(0x10C000), 3, "one IRQ 5 a frame, cleared by the acknowledge");
    assert_eq!(s.bus.video.palette[1], 0x7FFF);
    assert_eq!(s.bus.sound.bus.ram_byte(0x8000), 5, "Z80 NMI read the first nibble");
    assert_eq!(s.bus.sound.bus.ram_byte(0x8001), 0xA);
    let audio = s.drain_audio();
    assert!((audio.len() as i32 - 2 * 3 * 735).abs() < 8, "{} samples", audio.len());
}

#[test]
fn inputs() {
    use crate::common::input as pad;
    let mut s = board();
    s.set_input(pad::LEFT | pad::B | pad::START | pad::COIN);
    assert_eq!(s.bus.inputs[0], 0xFF & !0x04 & !0x10);
    assert_eq!(s.bus.inputs[3], (0x1F & !0x08) | 0x20);
}

#[test]
fn save_state_round_trip() {
    let mut s = board();
    s.run_frame(0);
    let snap = s.save_state();
    s.run_frame(0);
    let after = s.save_state();
    s.load_state(&snap).unwrap();
    s.run_frame(0);
    assert_eq!(s.save_state(), after, "replaying from the snapshot is deterministic");
}
