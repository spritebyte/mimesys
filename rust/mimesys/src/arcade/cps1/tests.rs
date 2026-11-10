use std::collections::HashMap;

use super::cps1_config::{config, SCROLL1, SCROLL2, SCROLL3, SPRITES};
use super::cps1_sets::set_info;
use super::cps_input::{cps1_layout, cps2_layout};
use super::cps1_system::Cps1System;
use super::cps1_video::{rgb, Video, SCREEN_W};
use crate::arcade::place_rom;
use crate::common::cpu::m68000::M68kBus;
use crate::common::input as pad;
use crate::common::system::System;

// ------------------------------------------------------------------ ROM loading

#[test]
fn place_rom_follows_the_mame_load_macros() {
    // ROM_LOAD64_WORD: 2 bytes every 8
    let mut r = vec![0u8; 16];
    place_rom(&mut r, 2, &[1, 2, 3, 4], 8, 2, false);
    assert_eq!(r, [0, 0, 1, 2, 0, 0, 0, 0, 0, 0, 3, 4, 0, 0, 0, 0]);
    // ROM_LOAD16_WORD_SWAP: words byte-swapped, contiguous
    let mut r = vec![0u8; 4];
    place_rom(&mut r, 0, &[0x12, 0x34, 0x56, 0x78], 2, 2, true);
    assert_eq!(r, [0x34, 0x12, 0x78, 0x56]);
    // ROM_LOAD16_BYTE and the plain copy
    let mut r = vec![0u8; 4];
    place_rom(&mut r, 1, &[9, 8], 2, 1, false);
    assert_eq!(r, [0, 9, 0, 8]);
    let mut r = vec![0u8; 3];
    place_rom(&mut r, 1, &[7, 6, 5], 1, 1, false);
    assert_eq!(r, [0, 7, 6], "bytes past the region end are dropped");
}

#[test]
fn every_exported_set_has_a_cps_b_config() {
    let db = include_str!("cps1_sets.rs");
    let names: Vec<&str> = db.lines()
        .filter_map(|l| l.trim().strip_prefix('"').and_then(|r| r.split('"').next()))
        .collect();
    assert!(names.len() > 100, "{} sets", names.len());
    for n in names {
        assert!(config(n).is_some(), "{n} has no CPS-B config");
        assert!(set_info(n).is_some(), "{n} has no set info");
    }
    assert!(cps1_layout("sf2").unwrap().six_button());
    assert!(!cps1_layout("ffight").unwrap().six_button());
    assert_eq!(set_info("ffight").unwrap().dsw, [0xFF, 0xF4, 0x9F], "MAME's DIP defaults");
    assert!(set_info("sf2ee").unwrap().cpsb_at_1c0);
    assert!(set_info("sf2ce").unwrap().clock_12mhz);
}

// ------------------------------------------------------------------ video

const GFX_SIZE: usize = 0x200000;

fn video(name: &str) -> Video { Video::new(vec![0; GFX_SIZE], config(name).unwrap(), -1) }

fn set_word(v: &mut Video, byte: usize, w: u16) {
    v.gfxram[byte] = (w >> 8) as u8;
    v.gfxram[byte + 1] = w as u8;
}

fn px(v: &Video, x: usize, y: usize) -> [u8; 4] {
    let o = (y * SCREEN_W + x) * 4;
    v.framebuffer[o..o + 4].try_into().unwrap()
}

/// ffight: CPS-B-04 (layer control 2E, priority 26 30 28 32, palette control 2A).
fn ffight_video() -> Video {
    let mut v = video("ffight");
    v.cps_b_write(0x2A / 2, 0x3F, 0xFFFF);     // all 6 palette pages
    v
}

/// Palette entry `e` = `w`, through gfx RAM and the CPS-B copy (base 0x9140 -> 0x14000).
fn set_pal(v: &mut Video, e: usize, w: u16) {
    set_word(v, 0x14000 + e * 2, w);
    v.cps_a_write(0x0A / 2, 0x9140, 0xFFFF);
}

#[test]
fn bank_mapper_follows_the_pal_table() {
    let v = video("ffight");   // S224B: sprites 0000-43FF, scroll1 4400-4BFF, scroll3 4C00-5FFF, scroll2 6000-7FFF
    assert_eq!(v.bank_map(SPRITES, 0x0100), Some(0x0100));
    assert_eq!(v.bank_map(SCROLL1, 0x4400), Some(0x4400));
    assert_eq!(v.bank_map(SCROLL2, 0x3000), Some(0x3000), "16x16 codes are checked doubled");
    assert_eq!(v.bank_map(SCROLL2, 0x0100), None, "a sprite range code isn't a scroll 2 tile");
    assert_eq!(v.bank_map(SCROLL3, 0x0980), Some(0x0980), "32x32 codes are checked x8");
    assert_eq!(v.bank_map(SCROLL1, 0x0100), None);
}

#[test]
fn palette_is_12_bit_rgb_with_brightness_and_copied_by_page() {
    assert_eq!(rgb(0xFFFF), [255, 255, 255, 255]);
    assert_eq!(rgb(0x0F00)[0], 85, "brightness 0 is a third");
    let mut v = ffight_video();
    v.cps_b_write(0x2A / 2, 0x02, 0xFFFF);      // page 1 only: it comes from the start of the base
    set_word(&mut v, 0x14000, 0xF0F0);
    v.cps_a_write(0x0A / 2, 0x9140, 0xFFFF);
    // entry 0x200 (page 1) = the first word; drawn as the background pen? check through a tile
    assert_eq!(rgb(0xF0F0), [0, 255, 0, 255]);
    // a scroll 1 tile using palette 0x20 (entry 0x201 = second word)
    set_word(&mut v, 0x14002, 0xF00F);
    v.cps_a_write(0x0A / 2, 0x9140, 0xFFFF);
    put_scroll1_tile(&mut v);
    v.cps_b_write(0x2E / 2, 0x02 | 1 << 6, 0xFFFF);    // scroll 1 on, drawn first
    v.render();
    assert_eq!(px(&v, 0, 0), [0, 0, 255, 255]);
}

/// A scroll 1 tile (code 4400, pen 1 everywhere on its left half) at screen (0, 0):
/// raster (64, 16) = tilemap column 8, row 2.
fn put_scroll1_tile(v: &mut Video) {
    let idx = 2 + (8 << 5);
    set_word(v, idx * 4, 0x4400);
    set_word(v, idx * 4 + 2, 0x0000);
    for y in 0..8 { v.gfx_mut()[0x4400 * 64 + y * 8] = 0xFF; }   // plane 0 = pen 1
}

#[test]
fn scroll1_tile_lands_where_the_scroll_registers_say() {
    let mut v = ffight_video();
    set_pal(&mut v, 0x201, 0xFF00);
    put_scroll1_tile(&mut v);
    v.cps_b_write(0x2E / 2, 0x02 | 1 << 6, 0xFFFF);
    v.render();
    assert_eq!(px(&v, 0, 0), [255, 0, 0, 255]);
    assert_eq!(px(&v, 7, 7), [255, 0, 0, 255]);
    assert_ne!(px(&v, 8, 0), [255, 0, 0, 255]);
    // scroll x 8: the tile moves 8 pixels left (off screen), y 2: up 2
    v.cps_a_write(0x0C / 2, 0xFFF8, 0xFFFF);
    v.cps_a_write(0x0E / 2, 2, 0xFFFF);
    v.render();
    assert_eq!(px(&v, 8, 0), [255, 0, 0, 255], "scroll x -8 moves it right");
    assert_eq!(px(&v, 8, 5), [255, 0, 0, 255]);
    assert_ne!(px(&v, 8, 6), [255, 0, 0, 255], "scroll y 2 moves it up");
    // layer disabled in the layer control register: nothing
    v.cps_b_write(0x2E / 2, 1 << 6, 0xFFFF);
    v.render();
    assert_ne!(px(&v, 8, 0), [255, 0, 0, 255]);
}

/// Sprite (code 0x10 -> pen 2 everywhere) at screen (0, 0) with palette 0.
fn put_sprite(v: &mut Video, colour: u16) {
    let o = 0x20000;   // object base 0x9200
    set_word(v, o, 64);
    set_word(v, o + 2, 16);
    set_word(v, o + 4, 0x0010);
    set_word(v, o + 6, colour);
    set_word(v, o + 8 + 6, 0xFF00);   // end of table
    for i in 0..128 { if i % 4 == 1 { v.gfx_mut()[0x10 * 128 + i] = 0xFF; } }
}

#[test]
fn sprites_show_a_frame_late_and_hide_behind_high_priority_pens() {
    let mut v = ffight_video();
    set_pal(&mut v, 0x201, 0xFF00);   // scroll 1 pen 1: red
    set_pal(&mut v, 0x002, 0xF0F0);   // sprite pen 2: green
    put_scroll1_tile(&mut v);
    put_sprite(&mut v, 0);
    // order: scroll 1, then sprites, then scroll 2, scroll 3
    v.cps_b_write(0x2E / 2, 0x02 | 1 << 6 | 0 << 8 | 2 << 10 | 3 << 12, 0xFFFF);
    v.render();
    assert_eq!(px(&v, 0, 0), [255, 0, 0, 255], "the table isn't latched yet");
    v.vblank();
    v.render();
    assert_eq!(px(&v, 0, 0), [0, 255, 0, 255], "sprites drawn after scroll 1");
    assert_eq!(px(&v, 15, 15), [0, 255, 0, 255], "16x16");
    // priority mask 0 (register 26) marks pen 1 of scroll 1 as in front of sprites
    v.cps_b_write(0x26 / 2, 1 << 1, 0xFFFF);
    v.render();
    assert_eq!(px(&v, 0, 0), [255, 0, 0, 255], "hidden where scroll 1 has a high pen");
    assert_eq!(px(&v, 8, 0), [0, 255, 0, 255], "shown where scroll 1 is empty");
    // sprites first, scroll 1 later: scroll 1 simply covers them
    v.cps_b_write(0x26 / 2, 0, 0xFFFF);
    v.cps_b_write(0x2E / 2, 0x02 | 0 << 6 | 1 << 8 | 2 << 10 | 3 << 12, 0xFFFF);
    v.render();
    assert_eq!(px(&v, 0, 0), [255, 0, 0, 255]);
}

#[test]
fn sprite_blocks_and_flips() {
    let mut v = ffight_video();
    set_pal(&mut v, 0x002, 0xF0F0);
    set_pal(&mut v, 0x003, 0xF00F);
    // a 2x1 block: tile 0x10 (pen 2) then 0x11 (pen 3)
    put_sprite(&mut v, 0x0100);
    for i in 0..128 { if i % 4 <= 1 { v.gfx_mut()[0x11 * 128 + i] = 0xFF; } }
    v.cps_b_write(0x2E / 2, 0 << 6 | 1 << 8 | 2 << 10 | 3 << 12, 0xFFFF);
    v.vblank();
    v.render();
    assert_eq!(px(&v, 0, 0), [0, 255, 0, 255]);
    assert_eq!(px(&v, 16, 0), [0, 0, 255, 255], "second tile to the right");
    // flip x: the tiles swap places
    set_word(&mut v, 0x20006, 0x0120);
    v.vblank();
    v.render();
    assert_eq!(px(&v, 0, 0), [0, 0, 255, 255]);
    assert_eq!(px(&v, 16, 0), [0, 255, 0, 255]);
}

#[test]
fn line_scroll_shifts_scroll2_rows() {
    let mut v = ffight_video();
    set_pal(&mut v, 0x401, 0xFF00);
    // scroll 2 tile (code 0x3000) at tilemap column 4, row 1 = raster (64, 16)
    let base = 0x4000;   // scroll 2 base 0x9040
    let idx = (1 & 0x0F) + (4 << 4);
    set_word(&mut v, base + idx * 4, 0x3000);
    set_word(&mut v, base + idx * 4 + 2, 0);
    for y in 0..16 { v.gfx_mut()[0x3000 * 128 + y * 8] = 0xFF; v.gfx_mut()[0x3000 * 128 + y * 8 + 4] = 0xFF; }
    v.cps_a_write(0x22 / 2, 0x04, 0xFFFF);   // scroll 2 enabled in video control
    v.cps_b_write(0x2E / 2, 0x04 | 2 << 6 | 1 << 8 | 0 << 10 | 3 << 12, 0xFFFF);
    v.render();
    assert_eq!(px(&v, 0, 0), [255, 0, 0, 255]);
    // line scroll on; raster line 16's offset (other base 0x9100 -> 0x10000) = -4
    v.cps_a_write(0x22 / 2, 0x05, 0xFFFF);
    set_word(&mut v, 0x10000 + 16 * 2, 0xFFFC);
    v.render();
    assert_ne!(px(&v, 3, 0), [255, 0, 0, 255]);
    assert_eq!(px(&v, 19, 0), [255, 0, 0, 255], "line 0 moved right by 4");
    assert_ne!(px(&v, 17, 1), [255, 0, 0, 255], "line 1 didn't move");
    assert_eq!(px(&v, 15, 1), [255, 0, 0, 255]);
}

#[test]
fn cps_b_id_and_multiply_protection() {
    let v = video("ffight");   // CPS-B-04: ID 0004 at 20
    assert_eq!(v.cps_b_read(0x20 / 2, 0xFFFF, 0xFFFF), 0x0004);
    assert_eq!(v.cps_b_read(0x22 / 2, 0xFFFF, 0xFFFF), 0xFFFF);
    let mut v = video("sf2ce");   // CPS-B-21 default: factors 00 02, result 04 06, in2 at 36
    v.cps_b_write(0, 0x1234, 0xFFFF);
    v.cps_b_write(1, 0x5678, 0xFFFF);
    let p = 0x1234u32 * 0x5678;
    assert_eq!(v.cps_b_read(2, 0xFFFF, 0xFFFF), p as u16);
    assert_eq!(v.cps_b_read(3, 0xFFFF, 0xFFFF), (p >> 16) as u16);
    assert_eq!(v.cps_b_read(0x36 / 2, 0xFF7E, 0xFFFF), 0xFF7E, "extra buttons port");
}

// ------------------------------------------------------------------ board

fn w16(rom: &mut [u8], a: usize, words: &[u16]) {
    for (k, w) in words.iter().enumerate() {
        rom[a + 2 * k] = (w >> 8) as u8;
        rom[a + 2 * k + 1] = *w as u8;
    }
}

fn board(name: &str) -> Cps1System {
    let mut rom = vec![0u8; 0x80000];
    w16(&mut rom, 0, &[0x00FF, 0xF000, 0x0000, 0x0400]);    // SSP FFF000, PC 400
    w16(&mut rom, 26 * 4, &[0x0000, 0x0500]);               // autovector 2
    w16(&mut rom, 0x400, &[
        0x33FC, 0x005A, 0x0080, 0x0180,      // MOVE.W #$5A,$800180   (sound command)
        0x46FC, 0x2000,                      // MOVE #$2000,SR
        0x60FE,                              // BRA *
    ]);
    w16(&mut rom, 0x500, &[0x5279, 0x00FF, 0x0000, 0x4E73]);   // ADDQ.W #1,$FF0000; RTE
    let mut z80 = vec![0u8; 0x18000];
    let prog: &[u8] = &[0x3A, 0x08, 0xF0, 0x32, 0x00, 0xD0, 0x18, 0xF8];   // LD A,(F008); LD (D000),A; JR -8
    z80[..prog.len()].copy_from_slice(prog);
    let mut r = HashMap::new();
    r.insert("maincpu".to_string(), rom);
    r.insert("gfx".to_string(), vec![0; GFX_SIZE]);
    r.insert("audiocpu".to_string(), z80);
    r.insert("oki".to_string(), vec![0; 0x40000]);
    Cps1System::new(name, r).unwrap()
}

#[test]
fn vblank_irq2_is_acknowledged_and_the_sound_command_reaches_the_z80() {
    let mut s = board("ffight");
    for _ in 0..3 { s.run_frame(0); }
    assert_eq!(s.bus.ram_word(0xFF0000), 3, "one IRQ 2 a frame, cleared by the acknowledge");
    let super::cps1_system::SoundBoard::Classic(snd) = &s.bus.sound else { panic!("classic board") };
    assert_eq!(snd.bus.ram_byte(0xD000), 0x5A, "the Z80 read the command latch");
    let audio = s.drain_audio();
    assert!((audio.len() as i32 - 2 * 3 * 739).abs() < 40, "{} samples", audio.len());
}

#[test]
fn inputs_and_dip_switches() {
    let mut s = board("ffight");
    s.set_input(pad::RIGHT | pad::B | pad::START | pad::SELECT | (pad::LEFT | pad::A) << pad::P2_SHIFT);
    assert_eq!(s.bus.read_word(0x800000), 0xDDEE, "P2 left + button 2 high, P1 right + button 1 low");
    assert_eq!(s.bus.read_byte(0x800018), 0xFF & !0x01 & !0x10, "coin 1 + start 1");
    assert_eq!(s.bus.read_word(0x80001C), 0xF4FF, "DIP B in the high byte");
    // six-button layout: kicks on the CPS-B extra port
    let mut s = board("sf2");
    s.set_input(pad::B | pad::R | pad::Y);
    assert_eq!(s.bus.read_word(0x800000) & 0xFF, 0xEF, "Y = light punch = button 1");
    assert_eq!(s.bus.read_word(0x800140 + 0x36), 0xFFFA, "light + heavy kick");
}

#[test]
fn every_exported_set_has_an_input_layout() {
    for (db, cps2) in [(include_str!("cps1_sets.rs"), false), (include_str!("../cps2/cps2_sets.rs"), true)] {
        for n in db.lines().filter_map(|l| l.trim().strip_prefix('"').and_then(|r| r.split('"').next())) {
            let l = if cps2 { cps2_layout(n) } else { cps1_layout(n) };
            assert!(l.is_some(), "{n} has no input layout");
        }
    }
}

#[test]
fn player_2_has_all_six_buttons_in_sf2() {
    let mut s = board("sf2");
    // P2: Y X L = punches (main mask X bit, extra word Y / L), B A R = kicks (R extra)
    s.set_extra_input(pad::EXT_P2_Y | pad::EXT_P2_L | pad::EXT_P2_R);
    s.set_input(pad::B << pad::P2_SHIFT | pad::P2_X);
    assert_eq!(s.bus.read_word(0x800000), 0x8FFF, "P2 jab + strong + fierce");
    assert_eq!(s.bus.read_word(0x800140 + 0x36), 0xFFAF, "P2 short + roundhouse kick");
    s.set_extra_input(0);
    s.set_input(0);
    assert_eq!(s.bus.read_word(0x800140 + 0x36), 0xFFFF);
}

#[test]
fn captain_commando_reads_players_3_and_4_on_the_cps_b() {
    let mut s = board("captcomm");   // CPS-B-21: IN2 at 36, IN3 at 38
    s.set_extra_input((pad::UP | pad::B) << pad::EXT_P3_SHIFT | pad::EXT_P3_START
        | (pad::RIGHT | pad::A) << pad::EXT_P4_SHIFT | pad::EXT_P4_COIN);
    s.set_input(0);
    assert_eq!(s.bus.read_word(0x800140 + 0x36), 0xFF67, "P3 up + button 1 + start 3");
    assert_eq!(s.bus.read_word(0x800140 + 0x38), 0xFF9E, "P4 right + button 2 + coin 4");
    assert_eq!(s.bus.read_word(0x800000), 0xFFFF, "players 1 / 2 untouched");
}

#[test]
fn layouts_follow_mame_for_odd_games() {
    use crate::common::input::{pads, EXT_PAD_X, EXT_PAD_Y, EXT_P3_SHIFT, EXT_P4_SHIFT};
    // Slam Masters: button 3 of players 3 / 4 sits in IN1 bits 7 / 15
    let l = cps1_layout("slammast").unwrap();
    let p = l.ports(&pads(0, EXT_PAD_X << EXT_P3_SHIFT | EXT_PAD_X << EXT_P4_SHIFT));
    assert_eq!(p[1], 0x7F7F);
    assert_eq!(p[2], 0xFFFF, "button 3 isn't in IN2");
    // quiz games: no joystick, P1 button 4 at IN1 bit 7
    let l = cps1_layout("qad").unwrap();
    assert_eq!(l.ports(&pads(pad::Y | pad::UP, 0))[1], 0xFF7F, "Y = button 4; the stick does nothing");
    // CPS2 Dungeons & Dragons: Tower of Doom: players 3 / 4 in IN1, starts / coins in IN2
    let l = cps2_layout("ddtod").unwrap();
    let ext = (pad::LEFT | EXT_PAD_Y) << EXT_P3_SHIFT | pad::DOWN << EXT_P4_SHIFT
        | pad::EXT_P3_COIN | pad::EXT_P4_START;
    let p = l.ports(&pads(0, ext));
    assert_eq!(p[1], 0xFB7D, "P3 left + button 4, P4 down");
    assert_eq!(p[2] & 0xFF00, 0xFFFF & !0x0800 & !0x4000 & 0xFF00, "start 4 + coin 3");
    // nothing pressed: the idle values (CPS2 IN2 bit 0 is the EEPROM's, added by the bus)
    assert_eq!(l.ports(&pads(0, 0)), [0xFFFF, 0xFFFF, 0xFFFE, 0xFFFF]);
}

#[test]
fn sf2ee_has_its_cps_b_at_8001c0() {
    let mut s = board("sf2ee");   // CPS-B-18: ID 0408 at 10
    assert_eq!(s.bus.read_word(0x8001C0 + 0x10), 0x0408);
    assert_eq!(s.bus.read_word(0x800140 + 0x10), 0xFFFF);
}

#[test]
fn save_state_round_trip() {
    let mut s = board("ffight");
    for _ in 0..2 { s.run_frame(0); }
    s.bus.video.gfxram[0x123] = 0x45;
    let st = s.save_state();
    s.run_frame(0);
    s.bus.video.gfxram[0x123] = 0;
    s.load_state(&st).unwrap();
    assert_eq!(s.bus.video.gfxram[0x123], 0x45);
    assert_eq!(s.bus.ram_word(0xFF0000), 2);
    s.run_frame(0);
    assert_eq!(s.bus.ram_word(0xFF0000), 3);
}

#[test]
fn tile_pixels_are_four_bitplanes_top_plane_in_byte_3() {
    let mut v = video("ffight");
    let g = v.gfx_mut();
    g[3] = 0x80;          // 8x8 tile 0, row 0, pixel 0: plane 3 (the top bit)
    g[0] = 0x40;          // pixel 1: plane 0
    g[2] = 0x20;          // pixel 2: plane 1
    g[4 + 1] = 0x80;      // right half (the odd-column 8x8 tile), pixel 0: plane 2
    g[8 + 3] = 0x01;      // row 1, pixel 7
    assert_eq!(v.tile_pen(0, 8, 0, 0, 0), 8);
    assert_eq!(v.tile_pen(0, 8, 0, 1, 0), 1);
    assert_eq!(v.tile_pen(0, 8, 0, 2, 0), 4);
    assert_eq!(v.tile_pen(0, 8, 1, 0, 0), 2);
    assert_eq!(v.tile_pen(0, 8, 0, 7, 1), 8);
    // 16x16: x 8-15 in the second 4 bytes of the row, 8-byte rows
    assert_eq!(v.tile_pen(0, 16, 0, 8, 0), 2);
    assert_eq!(v.tile_pen(0, 16, 0, 7, 1), 8);
    // 32x32: 16-byte rows, tile 1 starts 512 bytes in
    let g = v.gfx_mut();
    g[512 + 16 + 12 + 3] = 0x80;
    assert_eq!(v.tile_pen(1, 32, 0, 24, 1), 8);
}

#[test]
fn sprite_blocks_wrap_within_a_row_of_16_tiles() {
    let mut v = ffight_video();
    set_pal(&mut v, 0x002, 0xF0F0);
    set_pal(&mut v, 0x003, 0xF00F);
    put_sprite(&mut v, 0x0100);          // 2x1 block
    set_word(&mut v, 0x20004, 0x001F);   // starting at tile 1F: the next one is 10, not 20
    for i in 0..128 { if i % 4 <= 1 { v.gfx_mut()[0x1F * 128 + i] = 0xFF; } }
    for i in 0..128 { v.gfx_mut()[0x20 * 128 + i] = 0x00; }
    v.cps_b_write(0x2E / 2, 0 << 6 | 1 << 8 | 2 << 10 | 3 << 12, 0xFFFF);
    v.vblank();
    v.render();
    assert_eq!(px(&v, 0, 0), [0, 0, 255, 255], "tile 1F");
    assert_eq!(px(&v, 16, 0), [0, 255, 0, 255], "tile 10 (pen 2)");
}

#[test]
fn skipped_palette_pages_still_take_space_after_the_first_copied_one() {
    let mut v = ffight_video();
    v.cps_b_write(0x2A / 2, 0x05, 0xFFFF);      // pages 0 and 2
    set_word(&mut v, 0x14000 + 0x800, 0xFF00);  // page 2's data is after page 1's space
    set_word(&mut v, 0x14000 + 0x400, 0xF0F0);
    v.cps_a_write(0x0A / 2, 0x9140, 0xFFFF);
    assert_eq!(v.palette_rgba(0x400), [255, 0, 0, 255]);
    assert_eq!(v.palette_rgba(0x200), [0, 0, 0, 255], "page 1 not copied");
}


#[test]
fn earlier_sprites_are_in_front_of_later_ones() {
    let mut v = ffight_video();
    set_pal(&mut v, 0x002, 0xF0F0);
    set_pal(&mut v, 0x013, 0xF00F);
    put_sprite(&mut v, 0);                   // sprite 0: tile 10, pen 2, palette 0 (green)
    let o = 0x20000 + 8;                     // sprite 1: same place, tile 11 (pen 3), palette 1 (blue)
    set_word(&mut v, o, 64);
    set_word(&mut v, o + 2, 16);
    set_word(&mut v, o + 4, 0x0011);
    set_word(&mut v, o + 6, 0x0001);
    set_word(&mut v, o + 8 + 6, 0xFF00);
    for i in 0..128 { if i % 4 <= 1 { v.gfx_mut()[0x11 * 128 + i] = 0xFF; } }
    v.cps_b_write(0x2E / 2, 0 << 6 | 1 << 8 | 2 << 10 | 3 << 12, 0xFFFF);
    v.vblank();
    v.render();
    assert_eq!(px(&v, 0, 0), [0, 255, 0, 255], "sprite 0 covers sprite 1");
}

// ------------------------------------------------------------------ Q-Sound board

/// A Kabuki-encrypted Z80 program: (byte, fetched as an opcode?) from address `at`.
fn encrypt_into(rom: &mut [u8], at: u16, prog: &[(u8, bool)], key: super::kabuki::Key) {
    for (i, &(b, op)) in prog.iter().enumerate() {
        let a = at + i as u16;
        rom[a as usize] = super::kabuki::encrypt_byte(b, a, op, key);
    }
}

fn qboard() -> Cps1System {
    use super::kabuki::Key;
    let mut rom = vec![0u8; 0x80000];
    w16(&mut rom, 0, &[0x00FF, 0xF000, 0x0000, 0x0400]);
    w16(&mut rom, 0x400, &[
        0x33FC, 0x005A, 0x00F1, 0x8000,      // MOVE.W #$5A,$F18000   (shared RAM 0, byte 0)
        0x60FE,                              // BRA *
    ]);
    let mut z80 = vec![0u8; 0x28000];
    let (o, d) = (true, false);
    encrypt_into(&mut z80, 0, &[
        (0x31, o), (0xF0, d), (0xFF, d),     // LD SP,FFF0
        (0xED, o), (0x56, o),                // IM 1
        (0xFB, o),                           // EI
        (0x3A, o), (0x00, d), (0xC0, d),     // loop: LD A,(C000)
        (0x32, o), (0x01, d), (0xC0, d),     // LD (C001),A
        (0x18, o), (0xF8, d),                // JR loop
    ], Key::Dino);
    encrypt_into(&mut z80, 0x38, &[
        (0x3A, o), (0x00, d), (0xF0, d),     // LD A,(F000)
        (0x3C, o),                           // INC A
        (0x32, o), (0x00, d), (0xF0, d),     // LD (F000),A
        (0xFB, o),                           // EI
        (0xC9, o),                           // RET
    ], Key::Dino);
    let mut r = HashMap::new();
    r.insert("maincpu".to_string(), rom);
    r.insert("gfx".to_string(), vec![0; GFX_SIZE]);
    r.insert("audiocpu".to_string(), z80);
    r.insert("qsound".to_string(), vec![0; 0x200000]);
    r.insert("dsp".to_string(), vec![1; 0x2000]);
    Cps1System::new("dino", r).unwrap()
}

#[test]
fn qsound_board_runs_the_decrypted_z80_and_shares_ram_with_the_68000() {
    let mut s = qboard();
    for _ in 0..4 { s.run_frame(0); }
    assert_eq!(s.bus.read_word(0xF18002), 0xFF5A, "the Z80 copied shared byte 0 to byte 1");
    let irqs = s.bus.read_word(0xF1E000) & 0xFF;
    assert!((15..=18).contains(&irqs), "250 Hz IRQ over 4 frames: {irqs}");
    let audio = s.drain_audio();
    assert!((audio.len() as i32 - 2 * 4 * 739).abs() < 60, "{} stereo samples", audio.len());
    assert!(s.has_battery(), "the settings EEPROM is saved");
    assert_eq!(s.get_sram().unwrap().len(), 128);
    assert_eq!(s.bus.read_word(0xF1C006) & 1, 1, "EEPROM DO idles high");
    // player 3 at F1C000 (Cadillacs and Dinosaurs is a 3-player game)
    s.set_extra_input((pad::RIGHT | pad::A) << pad::EXT_P3_SHIFT);
    s.set_input(0);
    assert_eq!(s.bus.read_word(0xF1C000), 0xFFDE, "P3 right + button 2");
    assert_eq!(s.bus.read_word(0xF1C002), 0xFFFF);
}

#[test]
fn qsound_sets_need_the_dsp_rom() {
    let mut s = qboard();
    let st = s.save_state();
    s.run_frame(0);
    s.load_state(&st).unwrap();
    // without dl-1425.bin the loader leaves a zero-filled region: say what's missing
    let mut r = HashMap::new();
    r.insert("maincpu".to_string(), vec![0; 0x80000]);
    r.insert("gfx".to_string(), vec![0; GFX_SIZE]);
    r.insert("audiocpu".to_string(), vec![0; 0x28000]);
    r.insert("qsound".to_string(), vec![0; 0x1000]);
    r.insert("dsp".to_string(), vec![0; 0x2000]);
    let err = Cps1System::new("dino", r).err().unwrap();
    assert!(err.contains("qsound.zip"), "{err}");
}

