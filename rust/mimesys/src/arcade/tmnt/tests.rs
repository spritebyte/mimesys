use std::collections::HashMap;
use super::tmnt_system::TmntSystem;
use super::tmnt_video::{interleave_words, Video, SCREEN_W};
use crate::common::cpu::m68000::M68kBus;
use crate::common::input as pad;
use crate::common::system::System;

fn px(v: &Video, x: usize, y: usize) -> [u8; 4] {
    let i = (y * SCREEN_W + x) * 4;
    v.framebuffer[i..i + 4].try_into().unwrap()
}

fn set_pal(v: &mut Video, idx: usize, c: u16) {
    v.palette[idx * 2] = (c >> 8) as u8;
    v.palette[idx * 2 + 1] = c as u8;
}

const RED: u16 = 0x001F;
const GREEN: u16 = 0x03E0;
const BLUE: u16 = 0x7C00;

/// 4 tiles of tile ROM: tile 1 is pen 3 everywhere.
fn tile_rom() -> Vec<u8> {
    let mut t = vec![0u8; 0x200];
    t[32..64].fill(0x33);
    t
}

/// 8 sprite tiles: tile 0 is pen 5 everywhere, tile 1 pen 15 everywhere.
fn sprite_rom() -> Vec<u8> {
    let mut s = vec![0u8; 0x400];
    s[0..128].fill(0x55);
    s[128..256].fill(0xFF);
    s
}

fn video() -> Video { Video::new(tile_rom(), sprite_rom()) }

/// A sprite entry: 1x1, no zoom, at screen (x, y) (visible area), colour byte `color`.
fn sprite(v: &mut Video, slot: usize, prio: u8, code: u8, color: u8, x: i32, y: i32) {
    let o = slot * 8;
    let ox = (x + 96) as u16;
    let oy = (256 - (y + 16)) as u16;
    v.sprite_buf[o..o + 8].copy_from_slice(&[0x80 | prio, 0, code, color,
        (oy >> 8) as u8, oy as u8, (ox >> 8) as u8, ox as u8]);
}

#[test]
fn background_layer_is_opaque_and_uses_its_palette() {
    let mut v = video();
    set_pal(&mut v, 40 * 16, GREEN);        // layer B pen 0
    set_pal(&mut v, 40 * 16 + 3, RED);      // layer B pen 3
    v.render();
    assert_eq!(px(&v, 5, 5), [0, 255, 0, 255], "empty tiles still show their pen 0");
    for i in 0..0x800 { v.vram[0x2000 + 2 * 0x800 + i] = 1; }
    v.render();
    assert_eq!(px(&v, 5, 5), [255, 0, 0, 255]);
}

#[test]
fn tile_callback_palette_comes_from_the_top_colour_bits() {
    let mut v = video();
    for i in 0..0x800 { v.vram[0x2000 + 2 * 0x800 + i] = 1; v.vram[2 * 0x800 + i] = 0x40; }   // colour bits 5-7 = 2
    set_pal(&mut v, 42 * 16 + 3, BLUE);
    v.render();
    assert_eq!(px(&v, 0, 0), [0, 0, 255, 255]);
}

#[test]
fn sprite_priority_against_layer_a_and_fix_on_top() {
    let mut v = video();
    set_pal(&mut v, 40 * 16 + 3, GREEN);    // B: opaque green
    for i in 0..0x800 { v.vram[0x2000 + 2 * 0x800 + i] = 1; }
    set_pal(&mut v, 32 * 16 + 3, RED);      // A: red tile under the sprite
    v.vram[0x2800 + 272] = 1;               // tilemap (16, 4) = screen (40, 20)
    set_pal(&mut v, 16 * 16 + 5, BLUE);     // sprite colour 0, pen 5
    sprite(&mut v, 0, 3, 0, 0, 40, 20);
    v.render();
    assert_eq!(px(&v, 41, 22), [0, 0, 255, 255], "sprite over B");
    assert_eq!(px(&v, 50, 19), [0, 255, 0, 255], "above the sprite");
    assert_eq!(px(&v, 56, 22), [0, 255, 0, 255], "right of the sprite");

    // A draws first only when its tile sits in the sprite's way: priority 0 = sprites over A
    assert_eq!(px(&v, 41, 22), [0, 0, 255, 255]);
    v.priority = 1;                          // sprites under A
    v.render();
    assert_eq!(px(&v, 41, 22), [255, 0, 0, 255], "A covers the sprite");
    assert_eq!(px(&v, 50, 30), [0, 0, 255, 255], "outside A's 8x8 tile the sprite shows");

    // FIX is on top of everything
    set_pal(&mut v, 3, GREEN | RED);
    v.vram[0x2000 + 273] = 1;       // FIX has no x offset: tilemap (17, 4) = screen (40, 16)
    v.render();
    assert_eq!(px(&v, 41, 22), [255, 255, 0, 255]);
}

#[test]
fn sprites_draw_in_priority_code_order_and_shadow_darkens() {
    let mut v = video();
    set_pal(&mut v, 40 * 16, 0x7FFF);
    set_pal(&mut v, 16 * 16 + 5, RED);       // sprite colour 0 pen 5
    set_pal(&mut v, 17 * 16 + 5, BLUE);      // sprite colour 1 pen 5
    // slot 0 has the higher priority code: it is drawn last, on top
    sprite(&mut v, 0, 9, 0, 1, 40, 20);
    sprite(&mut v, 1, 2, 0, 0, 40, 20);
    v.render();
    assert_eq!(px(&v, 45, 25), [0, 0, 255, 255]);

    // a shadow sprite (colour bit 7): its pen 15 darkens the pixel below by 25 %
    let mut v = video();
    set_pal(&mut v, 40 * 16, 0x7FFF);
    sprite(&mut v, 0, 1, 1, 0x80, 40, 20);
    v.render();
    let p = px(&v, 45, 25);
    assert_eq!(p, [(255.0f32 * 0.75 + 0.5) as u8; 3].iter().cloned().chain([255]).collect::<Vec<_>>()[..4]);
    // without the shadow flag pen 15 is an ordinary colour
    let mut v = video();
    set_pal(&mut v, 40 * 16, 0x7FFF);
    set_pal(&mut v, 16 * 16 + 15, GREEN);
    sprite(&mut v, 0, 1, 1, 0x00, 40, 20);
    v.render();
    assert_eq!(px(&v, 45, 25), [0, 255, 0, 255]);
}

#[test]
fn sprite_flip_zoom_and_dma() {
    let mut v = video();
    set_pal(&mut v, 40 * 16, 0x7FFF);
    set_pal(&mut v, 16 * 16 + 5, RED);
    // the buffer is only loaded by the vblank DMA
    sprite(&mut v, 0, 1, 0, 0, 40, 20);
    let o = 0;
    v.sprite_ram.copy_from_slice(&v.sprite_buf.clone());
    v.sprite_buf[o..o + 8].fill(0);
    v.render();
    assert_eq!(px(&v, 45, 25), [255, 255, 255, 255], "RAM alone isn't drawn");
    v.sprite_dma();
    v.render();
    assert_eq!(px(&v, 45, 25), [255, 0, 0, 255]);
    // control bit 4 stops the DMA
    v.s_control = 0x10;
    v.sprite_ram[0] = 0;
    v.sprite_dma();
    v.render();
    assert_eq!(px(&v, 45, 25), [255, 0, 0, 255], "buffer kept");
    // x zoom 32 of 128: a quarter width... 0x10000/128*(128-32) = 3/4 of 16 pixels = 12
    v.s_control = 0;
    sprite(&mut v, 0, 1, 0, 0, 40, 20);
    v.sprite_buf[6] |= 32 << 2;
    v.render();
    assert_eq!(px(&v, 51, 25), [255, 0, 0, 255]);
    assert_eq!(px(&v, 52, 25), [255, 255, 255, 255], "narrowed to 12 pixels");
}

#[test]
fn k052109_window_ignores_a12() {
    let mut v = video();
    let mut s = TmntBoardHelper::new();
    s.bus.write_byte(0x100000, 0x11);          // high byte lane: chip 0000
    s.bus.write_byte(0x100001, 0x22);          // low byte lane: chip 2000
    s.bus.write_byte(0x102000, 0x33);          // o = 1000: chip 0800
    s.bus.write_byte(0x106001, 0x44);          // o = 3000, low lane: chip 1800 + 2000 = 3800
    assert_eq!(s.bus.video.vram[0x0000], 0x11);
    assert_eq!(s.bus.video.vram[0x2000], 0x22);
    assert_eq!(s.bus.video.vram[0x0800], 0x33);
    assert_eq!(s.bus.video.vram[0x3800], 0x44);
    // word accesses reach the high byte only, as in MAME
    assert_eq!(s.bus.read_word(0x100000), 0x1100);
    s.bus.write_word(0x100000, 0x5566);
    assert_eq!(s.bus.video.vram[0], 0x55);
    assert_eq!(s.bus.video.vram[0x2000], 0x22);
    v.rmrd = true;
    let _ = v.k052109_read(0);
}

/// Sprite RAM, the K051937 registers and the palette through the bus.
#[test]
fn sprite_ram_registers_and_palette_through_the_bus() {
    let mut s = TmntBoardHelper::new();
    s.bus.write_byte(0x140400, 0xAB);
    s.bus.write_word(0x140402, 0xCDEF);
    assert_eq!(s.bus.video.sprite_ram[0], 0xAB);
    assert_eq!(&s.bus.video.sprite_ram[2..4], &[0xCD, 0xEF]);
    assert_eq!(s.bus.read_word(0x140402), 0xCDEF);
    assert_eq!(s.bus.read_byte(0x140400), 0xAB);
    s.bus.write_byte(0x140000, 0x18);           // control: DMA off, flip
    assert_eq!(s.bus.video.s_control, 0x18);
    s.bus.write_byte(0x140001, 0x07);
    assert_eq!(s.bus.video.s_shadow, 7);
    s.bus.write_word(0x140002, 0x1234);         // ROM bank bytes 0, 1
    assert_eq!(s.bus.video.s_rombank[..2], [0x12, 0x34]);
    // palette: low byte lane only
    s.bus.write_byte(0x080003, 0x7F);
    s.bus.write_byte(0x080002, 0x55);
    s.bus.write_word(0x080004, 0xAB12);
    assert_eq!(s.bus.video.palette[1], 0x7F);
    assert_eq!(s.bus.video.palette[2], 0x12);
    assert_eq!(s.bus.read_word(0x080002), 0x007F);
    // priority register: word writes only
    s.bus.write_word(0x0C0000, 0x0004);
    assert_eq!(s.bus.video.priority, 1);
    s.bus.write_word(0x0C0000, 0);
    assert_eq!(s.bus.video.priority, 0);
}

#[test]
fn sprite_rom_readback() {
    let mut s = TmntBoardHelper::new();
    // control bit 5: reads of sprite RAM return ROM bytes of tile (rombank-selected)
    s.bus.write_byte(0x140000, 0x20);
    s.bus.write_byte(0x140002, 0x00);
    s.bus.write_byte(0x140003, 0x00);
    // offset 4*0x10 selects romoffset 0x10: tile 0, line 0x10, byte 2
    assert_eq!(s.bus.read_byte(0x140400 + 0x10 * 4 + 2), s.rom_byte(0x10 * 4 + 2));
}

fn w16(rom: &mut [u8], a: usize, words: &[u16]) {
    for (k, w) in words.iter().enumerate() {
        rom[a + 2 * k] = (w >> 8) as u8;
        rom[a + 2 * k + 1] = *w as u8;
    }
}

struct TmntBoardHelper { bus_sys: TmntSystem, sprite: Vec<u8> }
impl std::ops::Deref for TmntBoardHelper { type Target = TmntSystem; fn deref(&self) -> &TmntSystem { &self.bus_sys } }
impl std::ops::DerefMut for TmntBoardHelper { fn deref_mut(&mut self) -> &mut TmntSystem { &mut self.bus_sys } }
impl TmntBoardHelper {
    fn new() -> Self { Self { bus_sys: board(), sprite: sprite_rom() } }
    fn rom_byte(&self, i: usize) -> u8 {
        // pair 0 = ROM 0 words / ROM 1 words interleaved; the helper's sprite_rom() is split
        // the same way in `board()`
        // (as the K051960 sees it, after the PROM address unscramble)
        let rom = interleave_words(&split_sprites(&self.sprite));
        super::tmnt_system::unscramble_sprites(&rom, &[0; 0x200])[i]
    }
}

/// Splits a sprite image into the four ROM files `new()` expects.
fn split_sprites(rom: &[u8]) -> Vec<Vec<u8>> {
    // inverse of the two interleaved pairs: words alternate between ROM 0 and ROM 1
    let mut r = vec![Vec::new(); 4];
    for (pair, chunk) in rom.chunks(rom.len() / 2).enumerate() {
        for (i, w) in chunk.chunks(2).enumerate() {
            r[pair * 2 + (i & 1)].extend_from_slice(w);
        }
    }
    r
}

fn board() -> TmntSystem {
    let mut rom = vec![0u8; 0x60000];
    w16(&mut rom, 0, &[0x0006, 0x4000, 0x0000, 0x0400]);    // SSP 064000, PC 400
    w16(&mut rom, 29 * 4, &[0x0000, 0x0500]);               // autovector 5
    w16(&mut rom, 0x400, &[
        0x13FC, 0x005A, 0x000A, 0x0009,      // MOVE.B #$5A,$0A0009   (sound latch)
        0x33FC, 0x0008, 0x000A, 0x0000,      // MOVE.W #8,$0A0000
        0x33FC, 0x0000, 0x000A, 0x0000,      // MOVE.W #0,$0A0000     (bit 3 falls: Z80 IRQ)
        0x33FC, 0x0020, 0x000A, 0x0000,      // MOVE.W #$20,$0A0000   (IRQ 5 on)
        0x46FC, 0x2000,                      // MOVE #$2000,SR
        0x60FE,                              // BRA *
    ]);
    w16(&mut rom, 0x500, &[
        0x5279, 0x0006, 0x0000,              // ADDQ.W #1,$060000
        0x33FC, 0x0000, 0x000A, 0x0000,      // MOVE.W #0,$0A0000     (drop the IRQ 5 level)
        0x33FC, 0x0020, 0x000A, 0x0000,      // MOVE.W #$20,$0A0000
        0x4E73,                              // RTE
    ]);

    let mut z80 = vec![0u8; 0x10000];
    let prog: &[u8] = &[0xED, 0x56, 0xFB, 0x18, 0xFE];      // IM 1; EI; JR $
    z80[..prog.len()].copy_from_slice(prog);
    let isr: &[u8] = &[
        0x3A, 0x00, 0xA0, 0x32, 0x00, 0x80,                 // LD A,(A000); LD (8000),A
        0x3E, 0x06, 0x32, 0x00, 0x90,                       // LD A,6; LD (9000),A  (title on, uPD running)
        0x21, 0x01, 0x80, 0x34,                             // LD HL,8001; INC (HL)
        0xFB, 0xED, 0x4D,                                   // EI; RETI
    ];
    z80[0x38..0x38 + isr.len()].copy_from_slice(isr);

    let mut r = HashMap::new();
    r.insert("maincpu".to_string(), rom);
    r.insert("audiocpu".to_string(), z80);
    r.insert("k052109_0".to_string(), vec![0; 0x100]);
    r.insert("k052109_1".to_string(), vec![0; 0x100]);
    for (i, s) in split_sprites(&sprite_rom()).into_iter().enumerate() {
        r.insert(format!("k051960_{i}"), s);
    }
    r.insert("k007232".to_string(), vec![0x80; 0x100]);
    r.insert("upd".to_string(), vec![0; 0x100]);
    r.insert("title".to_string(), vec![0; 0x100]);
    r.insert("proms".to_string(), vec![0; 0x200]);
    TmntSystem::new("tmnt", r, false).unwrap()
}

#[test]
fn vblank_irq_is_a_level_cleared_by_the_game_and_the_sound_link_works() {
    let mut s = board();
    for _ in 0..3 { s.run_frame(0); }
    assert_eq!(s.bus.ram_word(0x060000), 3, "one IRQ 5 a frame, dropped by the 0A0000 write");
    assert_eq!(s.bus.sound.bus.ram_byte(0x8000), 0x5A, "the Z80 read the latch in its IRQ handler");
    assert_eq!(s.bus.sound.bus.ram_byte(0x8001), 1, "the Z80 IRQ fired once (falling edge only)");
    let audio = s.drain_audio();
    assert!((audio.len() as i32 - 2 * 3 * 745).abs() < 40, "{} samples", audio.len());
}

#[test]
fn irq5_stays_asserted_until_the_game_clears_it() {
    let mut rom = vec![0u8; 0x60000];
    w16(&mut rom, 0, &[0x0006, 0x4000, 0x0000, 0x0400]);
    w16(&mut rom, 29 * 4, &[0x0000, 0x0500]);
    w16(&mut rom, 0x400, &[0x33FC, 0x0020, 0x000A, 0x0000, 0x46FC, 0x2000, 0x60FE]);
    w16(&mut rom, 0x500, &[0x5279, 0x0006, 0x0000, 0x4E73]);   // count, RTE, never clear
    let mut r = HashMap::new();
    r.insert("maincpu".to_string(), rom);
    r.insert("audiocpu".to_string(), vec![0; 0x10000]);
    for n in ["k052109_0", "k052109_1", "k051960_0", "k051960_1", "k051960_2", "k051960_3"] {
        r.insert(n.to_string(), vec![0; 0x100]);
    }
    for n in ["k007232", "upd", "title"] { r.insert(n.to_string(), vec![0; 0x100]); }
    r.insert("proms".to_string(), vec![0; 0x200]);
    let mut s = TmntSystem::new("tmnt", r, false).unwrap();
    s.run_frame(0);
    s.run_frame(0);
    assert!(s.bus.ram_word(0x060000) > 100, "the level re-interrupts at once ({})", s.bus.ram_word(0x060000));
}

#[test]
fn inputs_dips_and_slots() {
    let mut s = board();
    s.set_input(pad::LEFT | pad::B | pad::COIN);
    assert_eq!(s.bus.read_word(0x0A0002), (0xFF & !0x01 & !0x10) as u16);
    assert_eq!(s.bus.read_word(0x0A0000), 0xFE, "coin 1");
    assert_eq!(s.bus.read_word(0x0A0004), 0xFF);
    // R moves pad 1 to the next character: its own port and coin
    s.set_input(pad::R);
    s.set_input(pad::DOWN | pad::COIN);
    assert_eq!(s.bus.read_word(0x0A0004), (0xFF & !0x08) as u16);
    assert_eq!(s.bus.read_word(0x0A0000), 0xFD);
    // DIP switches: coinage 1C/1C, 2 lives, normal, demo sounds on
    assert_eq!(s.bus.read_word(0x0A0010), 0xFF);
    assert_eq!(s.bus.read_word(0x0A0012), 0x5E);
    assert_eq!(s.bus.read_word(0x0A0018), 0xFF);
    s.set_input(pad::L);
    assert_eq!(s.bus.read_word(0x0A0018), 0xFB, "service switch");
    // pads 3 / 4 take the two slots after pad 2's (pad 1 is on slot 2 now)
    s.set_extra_input(pad::UP << pad::EXT_P3_SHIFT | pad::EXT_P3_COIN | pad::A << pad::EXT_P4_SHIFT);
    s.set_input(0);
    assert_eq!(s.bus.read_word(0x0A0014), (0xFF & !0x04) as u16, "pad 3 = slot 4");
    assert_eq!(s.bus.read_word(0x0A0002), (0xFF & !0x20) as u16, "pad 4 = slot 1");
    assert_eq!(s.bus.read_word(0x0A0000), 0xF7, "coin 4");
    s.set_extra_input(0);
    // byte reads: the value is on the odd address
    assert_eq!(s.bus.read_byte(0x0A0012), 0);
    assert_eq!(s.bus.read_byte(0x0A0013), 0x5E);
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

/// A real glyph from a TMNT dump (a letter "A"): packed nibbles, a little-endian 32-bit row read from its top nibble.
#[test]
fn tile_rom_rows_are_little_endian_words_of_nibbles() {
    let mut rom = vec![0u8; 64];
    let a: [u8; 32] = [
        0x00, 0x12, 0x11, 0x02, 0x20, 0xb1, 0xbb, 0x21, 0x1b, 0x01, 0xb0, 0x11, 0x1b, 0x01, 0xb0, 0x11,
        0x1b, 0x11, 0x11, 0x11, 0x1b, 0xb1, 0xbb, 0x11, 0x1b, 0x01, 0xb0, 0x11, 0xbb, 0x00, 0xb0, 0x0b,
    ];
    rom[32..].copy_from_slice(&a);
    let v = Video::new(rom, sprite_rom());
    let row = |y: usize| -> String { (0..8).map(|x| format!("{:x}", v.tile_pixel(1, x, y))).collect() };
    assert_eq!(row(0), "02111200");
    assert_eq!(row(2), "11b0011b");
}

#[test]
fn sprite_address_unscramble_follows_the_prom() {
    // 2048 words of distinct values, PROM entry 3 (2x2 sprites) for the first 2048 words
    let mut rom = Vec::new();
    for w in 0..2048u32 { rom.extend_from_slice(&w.to_le_bytes()); }
    let word = |r: &[u8], a: usize| u32::from_le_bytes(r[a * 4..a * 4 + 4].try_into().unwrap());
    let out = super::tmnt_system::unscramble_sprites(&rom, &[3; 0x100]);
    // entry 3: ROM bit 0 <- line bit 3, bit 2 <- bit 0, bit 5 <- bit 4 (MAME's table)
    assert_eq!(word(&out, 1), 4);
    assert_eq!(word(&out, 8), 1);
    assert_eq!(word(&out, 0x10), 0x20);
    // it is a permutation of each 1024-word block, for every sprite size
    for e in 0..8u8 {
        let out = super::tmnt_system::unscramble_sprites(&rom, &[e; 0x100]);
        let mut seen = vec![false; 2048];
        for a in 0..2048 { seen[word(&out, a) as usize] = true; }
        assert!(seen.iter().all(|&b| b), "entry {e}");
    }
}
