use std::collections::HashMap;
use crate::arcade::ddragon::dd_system::DdSystem;
use crate::arcade::ddragon::dd_video::decode_for_test;
use crate::common::cpu::m6809::M6809Bus;
use crate::common::system::System;

fn put(rom: &mut [u8], at: usize, code: &[u8]) { rom[at..at + code.len()].copy_from_slice(code); }

fn machine() -> DdSystem {
    // main HD6309 (6809 code)
    let mut main = vec![0u8; 0x30000];
    put(&mut main, 0x8000, &[
        0x10, 0xCE, 0x0F, 0xF0,     // LDS #$0FF0
        0x86, 0x1C, 0xB7, 0x38, 0x08, // sub halted (and out of reset)
        0x86, 0x5A, 0xB7, 0x20, 0x00, // shared RAM write while halted
        0x86, 0x0C, 0xB7, 0x38, 0x08, // sub runs
        0x86, 0x77, 0xB7, 0x38, 0x0E, // sound latch
        0xB7, 0x38, 0x0F,             // sub NMI
        0x1C, 0x00,                   // ANDCC #0
        0x20, 0xFE,                   // BRA *
    ]);
    put(&mut main, 0x8040, &[0x7C, 0x01, 0x00, 0xB7, 0x38, 0x0B, 0x3B]);  // NMI: INC $100, ack
    put(&mut main, 0x8048, &[0x7C, 0x01, 0x01, 0xB7, 0x38, 0x0C, 0x3B]);  // FIRQ: INC $101, ack
    put(&mut main, 0x8050, &[0x7C, 0x01, 0x02, 0xB7, 0x38, 0x0D, 0x3B]);  // IRQ: INC $102, ack
    put(&mut main, 0xFFF6, &[0x80, 0x48, 0x80, 0x50]);
    put(&mut main, 0xFFFC, &[0x80, 0x40, 0x80, 0x00]);
    main[0x10000 + 7 * 0x4000] = 0xB7;   // bank 7 marker
    // sub HD63701Y0, on-chip ROM at C000
    let mut sub = vec![0u8; 0x4000];
    put(&mut sub, 0x0000, &[
        0x8E, 0x01, 0x3F,             // LDS #$013F
        0x86, 0xFF, 0x97, 0x16,       // port 6 all outputs
        0x86, 0x01, 0x97, 0x17,       // port 6 = 01
        0x20, 0xFE,
    ]);
    put(&mut sub, 0x0010, &[
        0x7C, 0x80, 0x01,             // INC $8001 (shared RAM)
        0x86, 0x02, 0x97, 0x17,       // bit 0 low: NMI off; bit 1 rising: main IRQ
        0x86, 0x01, 0x97, 0x17,
        0x3B,
    ]);
    put(&mut sub, 0x3FFC, &[0xC0, 0x10, 0xC0, 0x00]);
    // sound 6809: on the latch IRQ, store it and play ADPCM chip 0 from 0 to 0x200
    let mut snd = vec![0u8; 0x10000];
    put(&mut snd, 0x8000, &[0x10, 0xCE, 0x0F, 0xF0, 0x1C, 0xEF, 0x20, 0xFE]);
    put(&mut snd, 0x8010, &[
        0xB6, 0x10, 0x00, 0xB7, 0x00, 0x20,   // LDA latch; STA $20
        0x4F, 0xB7, 0x38, 0x04,               // start address 0
        0x86, 0x01, 0xB7, 0x38, 0x02,         // end 0x200
        0xB7, 0x38, 0x00,                     // go
        0x3B,
    ]);
    put(&mut snd, 0xFFF8, &[0x80, 0x10]);
    put(&mut snd, 0xFFFE, &[0x80, 0x00]);
    let mut regions = HashMap::new();
    regions.insert("maincpu".to_string(), main);
    regions.insert("sub".to_string(), sub);
    regions.insert("soundcpu".to_string(), snd);
    regions.insert("chars".to_string(), vec![0; 0x8000]);
    regions.insert("sprites".to_string(), vec![0; 0x80000]);
    regions.insert("tiles".to_string(), vec![0; 0x40000]);
    regions.insert("adpcm1".to_string(), vec![0x77; 0x10000]);
    regions.insert("adpcm2".to_string(), vec![0; 0x10000]);
    DdSystem::new("test", regions).unwrap()
}

#[test]
fn interrupts_shared_ram_and_sub_cpu() {
    let mut m = machine();
    for _ in 0..3 { m.run_frame(0); }
    let b = &mut m.board;
    assert_eq!(b.read(0x0100), 3, "NMI once a frame (vblank)");
    assert_eq!(b.read(0x0101), 48, "FIRQ every 16 lines: 16 a frame");
    assert_eq!(b.read(0x0102), 1, "IRQ from the sub CPU's port 6");
    assert_eq!(b.read(0x2000), 0xFF, "shared RAM hidden while the sub CPU runs");
    assert_eq!(b.read(0x3802) & 0x10, 0x10, "EXTRA bit 4: sub CPU running");
    b.write(0x3808, 0x1C);                  // halt it
    assert_eq!(b.read(0x3802) & 0x10, 0);
    assert_eq!(b.read(0x2000), 0x5A, "main's write while halted");
    assert_eq!(b.read(0x2001), 1, "sub CPU NMI handler ran once");
    b.write(0x3808, 0xE0 | 0x0C);
    assert_eq!(b.read(0x4000), 0xB7, "ROM bank 7");
}

#[test]
fn sound_latch_and_adpcm() {
    let mut m = machine();
    m.run_frame(0);
    assert_eq!(m.board.sound.read(0x0020), 0x77, "latch reached the sound CPU");
    assert_eq!(m.board.sound.read(0x1800) & 1, 0, "chip 0 playing");
    let audio = m.drain_audio();
    assert!(audio.iter().any(|&s| s > 0.05), "ADPCM output");
    for _ in 0..20 { m.run_frame(0); }
    assert_eq!(m.board.sound.read(0x1800) & 3, 3, "chip 0 stopped at its end address");
    let n = m.drain_audio().len();
    assert!((n as i64 - 20 * 2 * 44100 * 384 * 272 / 6_000_000).abs() < 40, "audio rate {n}");
}

#[test]
fn gfx_decode_and_layers() {
    // chars: pixel x=0 takes bit offsets 1,3,5,7 of the byte (planes 0-3, plane 0 = pen
    // bit 3); x=1 takes offsets 0,2,4,6. Offset n is mask 0x80 >> n.
    let mut rom = vec![0u8; 0x200];
    rom[0] = 0x40 | 0x08;   // offset 1 (x=0, plane 0) and offset 4 (x=1, plane 2)
    let (chars, _) = decode_for_test(&rom);
    assert_eq!(chars[0][0], 8);
    assert_eq!(chars[0][1], 2);

    // a frame: background tile 1 pen 3, a sprite pen 5 over it
    let mut m = machine();
    let v = &mut m.board.video;
    v.palette[256 + 3] = 0x0F;           // BG colour 0 pen 3: red
    v.palette[128 + 5] = 0xF0;           // sprite colour 0 pen 5: green
    // render with hand-made graphics
    let mut tiles = vec![0u8; 0x40000];
    // tile 1 all pen 3: planes 2 and 3 live in the low half of the region (bit 4 / bit 0 groups)
    for b in &mut tiles[64..128] { *b = 0xFF; }
    let mut sprites = vec![0u8; 0x80000];
    // sprite 0 all pen 5: plane 1 (high half, offset 4) and plane 3 (low half, offset 4)
    for b in &mut sprites[0..64] { *b = 0x0F; }
    for b in &mut sprites[0x40000..0x40040] { *b = 0x0F; }
    let chars = vec![0u8; 0x8000];
    let mut fresh = crate::arcade::ddragon::dd_video::DdVideo::new(&chars, &tiles, &sprites);
    fresh.palette = v.palette.clone();
    fresh.palette_ext = v.palette_ext.clone();
    *v = fresh;
    // BG tile at screen (0,0): tilemap row 0 col 0 covers y 0-15 -> screen line 0 shows row 8
    v.bgram[1] = 1;
    // sprite at screen x 40, y 40: sx = 240 - b4, sy = 232 - b0
    v.spriteram[0..5].copy_from_slice(&[192, 0x80, 0x00, 0x00, 200]);
    v.render_line(0);
    v.render_line(45);
    let px = |v: &crate::arcade::ddragon::dd_video::DdVideo, x: usize, y: usize|
        v.framebuffer[(y * 256 + x) * 4..(y * 256 + x) * 4 + 3].to_vec();
    assert_eq!(px(v, 3, 0), vec![0xFF, 0, 0], "background");
    assert_eq!(px(v, 45, 45), vec![0, 0xFF, 0], "sprite over background");
}

#[test]
fn ddragon_round_trip() {
    let mut m = machine();
    crate::arcade::state::check_round_trip(&mut m, 5, 10);
}
