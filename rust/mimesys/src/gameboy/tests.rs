//! Game Boy core tests with small hand-assembled ROMs (no copyrighted code needed).

use crate::common::system::System;
use crate::gameboy::gb_common::GbVariant;
use crate::gameboy::gb_system::{BootConfig, GbSystem, Model};

fn put(rom: &mut [u8], at: usize, code: &[u8]) { rom[at..at + code.len()].copy_from_slice(code); }

/// 32K MBC1 + RAM + battery ROM: LCD on with a striped tile, sound on; every vblank the
/// interrupt counts at C000, scrolls by the count, stores it in cartridge RAM and every
/// 16th frame starts a square wave note.
pub fn test_rom(cgb: bool) -> Vec<u8> {
    let mut r = vec![0u8; 0x8000];
    put(&mut r, 0x100, &[0x00, 0xC3, 0x50, 0x01]);
    put(&mut r, 0x134, b"TESTROM");
    r[0x143] = if cgb { 0x80 } else { 0x00 };
    r[0x147] = 0x03;    // MBC1 + RAM + battery
    r[0x148] = 0x00;    // 32K
    r[0x149] = 0x02;    // 8K RAM
    put(&mut r, 0x150, &[
        0x31, 0xFE, 0xFF,             // LD SP,$FFFE
        0x3E, 0x80, 0xE0, 0x26,       // sound on
        0x3E, 0x77, 0xE0, 0x24,
        0x3E, 0xFF, 0xE0, 0x25,
        0x3E, 0x0A, 0xEA, 0x00, 0x00, // cartridge RAM on
        0x21, 0x00, 0x80, 0x06, 0x10, // tile 0: 16 bytes of L
        0x7D, 0x22, 0x05, 0x20, 0xFB,
        0x3E, 0xE4, 0xE0, 0x47,       // BGP
        0x3E, 0x01, 0xE0, 0xFF,       // IE = vblank
        0xFB,                         // EI
        0x76, 0x18, 0xFD,             // HALT; JR -3
    ]);
    put(&mut r, 0x40, &[0xC3, 0x00, 0x02]);
    put(&mut r, 0x200, &[
        0x21, 0x00, 0xC0, 0x34, 0x7E, // INC (C000); LD A,(HL)
        0xE0, 0x43,                   // SCX
        0xEA, 0x00, 0xA0,             // LD ($A000),A
        0xE6, 0x0F, 0x20, 0x0C,       // every 16th frame:
        0x3E, 0x80, 0xE0, 0x16,
        0x3E, 0xF0, 0xE0, 0x17,
        0x3E, 0x87, 0xE0, 0x19,       // trigger channel 2
        0xD9,                         // RETI
    ]);
    r
}

fn boot(model: Model, skip: bool) -> BootConfig {
    BootConfig { model: Some(model), skip, ..Default::default() }
}

fn machine(cgb: bool) -> GbSystem {
    GbSystem::build(&test_rom(cgb), "testrom", boot(Model::Auto, true)).unwrap()
}

#[test]
fn gb_runs_test_rom() {
    let mut m = machine(false);
    for _ in 0..20 { m.run_frame(0); }
    let count = unsafe { (*m.bus.ppu.get()).ly };   // just touch the PPU
    let _ = count;
    assert!(m.bus.ram[0] >= 18, "vblank interrupt counted {}", m.bus.ram[0]);
    assert_eq!(m.get_sram().unwrap()[0], m.bus.ram[0], "cartridge RAM written");
    assert!(m.drain_audio().iter().any(|s| s.abs() > 0.01), "square wave audible");
}

#[test]
fn gb_save_state_round_trip() {
    let mut m = machine(false);
    crate::arcade::state::check_round_trip(&mut m, 10, 40);
}

#[test]
fn gbc_save_state_round_trip() {
    let mut m = machine(true);
    crate::arcade::state::check_round_trip(&mut m, 10, 40);
}

#[test]
fn gb_state_rejects_other_hardware() {
    let a = machine(false);
    let mut b = machine(true);
    assert!(b.load_state(&a.save_state()).is_err());
    assert!(b.load_state(b"junk").is_err());
}

#[test]
fn gb_models_and_boot_roms() {
    // Auto: colour cartridge on a CGB in CGB mode, DMG cartridge on a DMG
    let m = machine(true);
    assert_eq!(m.hardware(), GbVariant::Cgb);
    assert!(m.bus.cgb_mode && !m.bus.boot_rom_mapped, "boot ROM finished in CGB mode");
    let m = machine(false);
    assert_eq!(m.hardware(), GbVariant::Dmg);
    assert!(!m.bus.boot_rom_mapped);
    assert_eq!(m.cpu.pc, 0x100, "DMG boot ROM hands over at 0100");
    // DMG game on a CGB: compatibility mode, palettes set by the boot ROM
    let m = GbSystem::build(&test_rom(false), "t", boot(Model::Cgb, true)).unwrap();
    assert_eq!(m.hardware(), GbVariant::Cgb);
    assert!(!m.bus.cgb_mode, "DMG game runs in compatibility mode");
    assert_eq!(m.bus.key0 & 0x04, 0x04);
    // without skipping, the boot ROM is still running after the first frame
    let mut m = GbSystem::build(&test_rom(false), "t", boot(Model::Dmg, false)).unwrap();
    assert!(m.bus.boot_rom_mapped);
    m.run_frame(0);
    assert!(m.bus.boot_rom_mapped, "logo animation playing");
    for _ in 0..400 { m.run_frame(0); }
    assert!(!m.bus.boot_rom_mapped, "boot ROM finished on its own");
}

#[test]
fn gb_boot_blob_parsing() {
    let mut blob = b"GBBC".to_vec();
    for (tag, data) in [(b"MODL", vec![2u8]), (b"SKIP", vec![1]), (b"DMG0", vec![0x31; 256])] {
        blob.extend_from_slice(tag);
        blob.extend_from_slice(&(data.len() as u32).to_le_bytes());
        blob.extend_from_slice(&data);
    }
    let c = BootConfig::parse(&blob);
    assert_eq!(c.model, Some(Model::Cgb));
    assert!(c.skip);
    assert_eq!(c.dmg.as_ref().map(|d| d.len()), Some(256));
    assert!(c.cgb.is_none());
    // a bare file (old frontend) is told apart by its size
    assert!(BootConfig::parse(&vec![0; 0x900]).cgb.is_some());
    assert!(BootConfig::parse(&vec![0; 100]).dmg.is_none());
}

#[test]
fn gb_reset_keeps_battery_ram() {
    let mut m = machine(false);
    for _ in 0..5 { m.run_frame(0); }
    let saved = m.get_sram().unwrap()[0];
    assert!(saved > 0);
    m.reset();
    assert_eq!(m.bus.ram[0], 0, "work RAM cleared");
    assert_eq!(m.get_sram().unwrap()[0], saved, "cartridge RAM kept");
}

/// SGB-enhanced ROM: sends PAL01 (colour 0 black; palette 0 red, green, blue) and MLT_REQ
/// (2 players) through the joypad port, then idles with a striped tile on screen.
fn sgb_rom() -> Vec<u8> {
    let mut r = vec![0u8; 0x8000];
    put(&mut r, 0x100, &[0x00, 0xC3, 0x50, 0x01]);
    put(&mut r, 0x134, b"SGBTEST");
    r[0x146] = 0x03;    // SGB functions
    r[0x14B] = 0x33;
    put(&mut r, 0x150, &[
        0x31, 0xFE, 0xFF,
        0x21, 0x00, 0x80, 0x06, 0x10, 0x7D, 0x22, 0x05, 0x20, 0xFB,   // tile 0 = 00 01 02 ..
        0x3E, 0xE4, 0xE0, 0x47,                                     // BGP E4
        0x21, 0x00, 0x04, 0xCD, 0x00, 0x03,                         // PAL01
        0x21, 0x10, 0x04, 0xCD, 0x00, 0x03,                         // MLT_REQ
        0x18, 0xFE,
    ]);
    // send the 16-byte packet at HL: reset pulse, 128 bits LSB first, stop bit
    put(&mut r, 0x300, &[
        0x3E, 0x00, 0xE0, 0x00, 0x3E, 0x30, 0xE0, 0x00, 0x06, 0x10,
        0x2A, 0x57, 0x0E, 0x08,                                     // byte loop
        0x7A, 0xE6, 0x01, 0x3E, 0x10, 0x20, 0x02, 0x3E, 0x20,       // bit loop
        0xE0, 0x00, 0x3E, 0x30, 0xE0, 0x00, 0xCB, 0x3A, 0x0D, 0x20, 0xEC,
        0x05, 0x20, 0xE5,
        0x3E, 0x20, 0xE0, 0x00, 0x3E, 0x30, 0xE0, 0x00, 0xC9,
    ]);
    put(&mut r, 0x400, &[0x01, 0x00, 0x00, 0x1F, 0x00, 0xE0, 0x03, 0x00, 0x7C]);
    put(&mut r, 0x410, &[0x89, 0x01]);
    r
}

fn sgb_machine() -> GbSystem {
    GbSystem::build(&sgb_rom(), "sgbtest", boot(Model::Sgb, true)).unwrap()
}

#[test]
fn sgb_model_colours_and_border() {
    let mut m = sgb_machine();
    assert_eq!(m.hardware(), GbVariant::Sgb);
    let di = m.display_info();
    assert_eq!((di.buffer_width, di.buffer_height), (256, 224));
    for _ in 0..5 { m.run_frame(0); }
    let sgb = m.bus.sgb.as_ref().unwrap();
    assert!(sgb.unlocked);
    assert_eq!(sgb.log.iter().filter(|&&c| c < 0x1E).copied().collect::<Vec<_>>(), vec![0x00, 0x11],
               "PAL01 then MLT_REQ received (after the boot ROM's header packets)");
    assert_eq!(sgb.players, 2);
    let fb = m.framebuffer();
    assert_eq!(fb.len(), 256 * 224 * 4);
    let px = |x: usize, y: usize| [fb[(y * 256 + x) * 4], fb[(y * 256 + x) * 4 + 1], fb[(y * 256 + x) * 4 + 2]];
    assert_ne!(px(0, 0), [0, 0, 0], "default border drawn");
    let mut seen = std::collections::HashSet::new();
    for y in 40..184 { for x in 48..208 { seen.insert(px(x, y)); } }
    for c in [[255, 0, 0], [0, 255, 0], [0, 0, 255]] {
        assert!(seen.contains(&c), "game screen uses {c:?}: {seen:?}");
    }
}

#[test]
fn sgb_without_header_flags_stays_default() {
    let mut rom = sgb_rom();
    rom[0x146] = 0;
    let mut m = GbSystem::build(&rom, "t", boot(Model::Sgb, true)).unwrap();
    for _ in 0..5 { m.run_frame(0); }
    let sgb = m.bus.sgb.as_ref().unwrap();
    assert!(!sgb.unlocked);
    assert_eq!(sgb.players, 2, "MLT_REQ still answers (SGB detection)");
    let fb = m.framebuffer();
    let o = (100 * 256 + 100) * 4;
    let gb_px = [fb[o], fb[o + 1], fb[o + 2]];
    assert!(![[255, 0, 0], [0, 255, 0], [0, 0, 255]].contains(&gb_px), "default palette, not the game's");
    assert_ne!(&fb[..3], &[0, 0, 0], "default border for a game without SGB functions");
}

#[test]
fn sgb_second_pad_is_player_two() {
    use crate::common::input as pad;
    let mut m = sgb_machine();
    for _ in 0..5 { m.run_frame(0); }
    m.set_input(pad::A << pad::P2_SHIFT);
    // select buttons with P15 low -> player 2's turn after P15 went high once
    let b = &mut m.bus;
    use crate::gameboy::gb_cpu::Bus;
    b.write(0xFF00, 0x30);
    assert_eq!(b.read(0xFF00) & 0x0F, 0x0F, "player 1 ID");
    b.write(0xFF00, 0x10);
    assert_eq!(b.read(0xFF00) & 0x0F, 0x0F, "player 1: A not pressed");
    b.write(0xFF00, 0x30);
    assert_eq!(b.read(0xFF00) & 0x0F, 0x0E, "player 2 ID");
    b.write(0xFF00, 0x10);
    assert_eq!(b.read(0xFF00) & 0x0F, 0x0E, "player 2: A pressed");
}

#[test]
fn sgb_save_state_round_trip() {
    let mut m = sgb_machine();
    crate::arcade::state::check_round_trip(&mut m, 5, 30);
}
