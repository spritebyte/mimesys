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
