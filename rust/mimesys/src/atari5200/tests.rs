use super::a5200_system::{A5200System, ALTIRRA_OS};
use super::cart::{Cart5200, Mapping};
use crate::common::system::System;

/// A 4 KB test cartridge (at 8000-8FFF, repeated up to BFFF): a display list of 3 x 8
/// blank lines, then 10 lines of mode D (2 bits per pixel) filled with PF0, PF0 = 0x46,
/// background 0x00; player 0 (all bits) at colour clock 100, colour 0x88. Then it spins.
fn test_cart() -> Vec<u8> {
    let mut rom = vec![0xFFu8; 0x1000];
    let code: Vec<u8> = vec![
        0x78,                         // SEI
        0xA9, 0x00, 0x8D, 0x0E, 0xD4, // NMIEN = 0
        0xA9, 0x00, 0x8D, 0x02, 0xD4, // DLISTL = 00
        0xA9, 0x81, 0x8D, 0x03, 0xD4, // DLISTH = 81
        0xA9, 0x46, 0x8D, 0x16, 0xC0, // COLPF0 = 46
        0xA9, 0x00, 0x8D, 0x1A, 0xC0, // COLBK = 00
        0xA9, 0x88, 0x8D, 0x12, 0xC0, // COLPM0 = 88
        0xA9, 0xFF, 0x8D, 0x0D, 0xC0, // GRAFP0 = FF
        0xA9, 0x64, 0x8D, 0x00, 0xC0, // HPOSP0 = 100
        0xA9, 0x01, 0x8D, 0x1B, 0xC0, // PRIOR = 1 (players in front)
        0xA9, 0x22, 0x8D, 0x00, 0xD4, // DMACTL = normal width + display list
        0x4C, 0x37, 0x80,             // spin: JMP spin
    ];
    rom[..code.len()].copy_from_slice(&code);
    // display list at 8100
    let mut dl = vec![0x70, 0x70, 0x70, 0x4D, 0x00, 0x82];
    dl.extend(std::iter::repeat(0x0D).take(9));
    dl.extend([0x41, 0x00, 0x81]);
    rom[0x100..0x100 + dl.len()].copy_from_slice(&dl);
    // screen data at 8200: 10 mode lines x 40 bytes of PF0 (01 01 01 01)
    for b in &mut rom[0x200..0x200 + 400] { *b = 0x55; }
    // cartridge header: BFFD = FF (no title screen), start address 8000
    rom[0xFFD] = 0xFF;
    rom[0xFFE] = 0x00;
    rom[0xFFF] = 0x80;
    rom
}

fn px(s: &A5200System, x: usize, y: usize) -> [u8; 3] {
    let i = (y * 384 + x) * 4;
    [s.framebuffer[i], s.framebuffer[i + 1], s.framebuffer[i + 2]]
}

#[test]
fn boots_through_altirra_os_and_draws_mode_d() {
    assert_eq!(ALTIRRA_OS.len(), 0x800);
    let mut s = A5200System::from_rom(&test_cart(), "test", &[]).unwrap();
    for _ in 0..30 { s.run_frame(0); }
    assert_eq!(s.bus.antic.dmactl, 0x22, "the cartridge ran");
    let pal = super::video::palette();
    // display list lines 24-33 are the mode D lines (picture rows 24-33, the first picture
    // line being line 8): PF0 inside the normal window (x 32-351), background outside
    assert_eq!(px(&s, 40, 24 + 2), pal[0x46], "mode D, PF0");
    assert_eq!(px(&s, 40, 24 + 19), pal[0x46], "the tenth mode line (2 scan lines each)");
    assert_eq!(px(&s, 40, 24 + 20), pal[0x00], "then a jump-and-wait: background");
    assert_eq!(px(&s, 40, 10), pal[0x00], "blank lines: background");
    assert_eq!(px(&s, 10, 26), pal[0x00], "outside the normal-width window");
    // player 0 at colour clock 100 = x (100 - 32) * 2 = 136, 8 colour clocks wide, every line
    assert_eq!(px(&s, 140, 26), pal[0x88], "player in front of the playfield");
    assert_eq!(px(&s, 140, 100), pal[0x88], "players aren't limited to the playfield lines");
    // collisions: player 0 touched PF0
    assert_eq!(s.bus.gtia.p_pf[0] & 1, 1);
}

#[test]
fn cartridge_mappings() {
    let mut rom = vec![0u8; 0x4000];
    rom[0] = 0x11;
    rom[0x2000] = 0x22;
    rom[0x3FFF] = 0x33;
    let one = Cart5200::new(&rom).unwrap();
    assert_eq!(one.mapping, Mapping::K16One, "no references to 4000-7FFF");
    assert_eq!((one.read(0x8000), one.read(0xBFFF), one.read(0x4000)), (0x11, 0x33, 0xFF));
    // lots of JSRs into 4000-7FFF: two chips
    for i in 0..40 { rom[0x2100 + i * 3..0x2100 + i * 3 + 3].copy_from_slice(&[0x20, 0x00, 0x50]); }
    let two = Cart5200::new(&rom).unwrap();
    assert_eq!(two.mapping, Mapping::K16Two);
    assert_eq!((two.read(0x4000), two.read(0x6000), two.read(0x8000), two.read(0xA000)), (0x11, 0x11, 0x22, 0x22));
    // the CRC lists (MAME): Centipede two chips, Choplifter one
    assert_eq!(super::cart::listed_16k(0x536A70FE), Some(Mapping::K16Two));
    assert_eq!(super::cart::listed_16k(0x9AD53BBC), Some(Mapping::K16One));
    assert_eq!(super::cart::listed_16k(0x12345678), None);
    let small = Cart5200::new(&vec![7u8; 0x1000]).unwrap();
    assert_eq!((small.read(0x8000), small.read(0xB123)), (7, 7));
    // a .car header picks the type
    let mut car = b"CART".to_vec();
    car.extend([0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0, 0]);
    car.extend(vec![0u8; 0x4000]);
    assert_eq!(Cart5200::new(&car).unwrap().mapping, Mapping::K16Two);
}

/// The keypad goes through POKEY's keyboard scan: with debounce off (the OS sets SKCTL
/// = 2) a held key keeps raising key IRQs, several a frame, as on the real console.
#[test]
fn keypad_and_top_button_through_pokey() {
    use crate::common::input::{B, KEYPAD_SHIFT};
    let mut s = A5200System::from_rom(&test_cart(), "test", &[]).unwrap();
    s.bus.pokey.write(0x0F, 0x02);                              // SKCTL: scan, no debounce
    s.bus.pokey.write(0x0E, 0xC0);                              // IRQEN: key + break
    let none = 0x0F << KEYPAD_SHIFT;
    s.set_input(3 << KEYPAD_SHIFT);                             // keypad "3"
    let mut irqs = 0;
    for _ in 0..(262 * 114) {
        s.bus.pokey.step();
        if s.bus.pokey.irq() { irqs += 1; s.bus.pokey.write(0x0E, 0x00); s.bus.pokey.write(0x0E, 0xC0); }
    }
    assert!(irqs >= 4, "a held key repeats with debounce off: {irqs} IRQs in a frame");
    assert_eq!(s.bus.pokey.read(0x09) & 0x1E, 0x3B & 0x1E, "KBCODE for 3 (bits 1-4)");
    s.set_input(none | B);
    for _ in 0..(262 * 114) { s.bus.pokey.step(); }
    assert_eq!(s.bus.pokey.read(0x0F) & 0x0C, 0x04, "no key, top button (shift) down");
    assert!(s.bus.pokey.read(0x0E) & 0x80 == 0, "top button: BREAK IRQ");
    // debounce on: a 5200 key closes several codes at once, which the debouncer reads
    // as different keys, so nothing gets through (why 5200 games leave it off)
    s.bus.pokey.write(0x0F, 0x03);
    s.set_input(none);
    for _ in 0..(262 * 114) { s.bus.pokey.step(); }
    s.bus.pokey.write(0x0E, 0x00); s.bus.pokey.write(0x0E, 0x40);
    s.set_input(5 << KEYPAD_SHIFT);
    let mut irqs = 0;
    for _ in 0..(262 * 114 * 2) {
        s.bus.pokey.step();
        if s.bus.pokey.irq() { irqs += 1; s.bus.pokey.write(0x0E, 0x00); s.bus.pokey.write(0x0E, 0x40); }
    }
    assert_eq!(irqs, 0, "debounced 5200 key: no IRQ");
}

/// Mouse as Trak-Ball: unpowered pots read 114 (not 228), rolling sets the speed.
#[test]
fn trackball_from_the_mouse() {
    use crate::common::cpu::m6502::AddressBus;
    let mut s = A5200System::from_rom(&test_cart(), "test", &[]).unwrap();
    s.set_option("trackball", 1);
    let none = 0x0F << crate::common::input::KEYPAD_SHIFT;
    s.set_input(none);
    assert_eq!(s.bus.pokey.pot_in[0], 114, "pots off: a Trak-Ball reads 114");
    s.bus.write_byte(0xC01F, 0x04);
    s.set_pointer(100, 100, 0); s.set_input(none);
    s.set_pointer(110, 95, 1); s.set_input(none);
    assert_eq!((s.bus.pokey.pot_in[0], s.bus.pokey.pot_in[1]), (144, 99));
    assert!(s.bus.gtia.trig[0], "left button = bottom fire");
    s.set_pointer(400, 0, 0); s.set_input(none);
    assert_eq!(s.bus.pokey.pot_in[0], 189, "capped at 114 + 75");
}

/// CONSOL bit 2 powers the pots: off, a joystick reads 228 (a Trak-Ball would read 114).
/// On, the OS's VBI copies the stick into PADDL0/1 ($11/$12).
#[test]
fn stick_pots_need_consol_bit_2() {
    use crate::common::cpu::m6502::AddressBus;
    use crate::common::input::{DOWN, KEYPAD_SHIFT, LEFT, RIGHT, UP};
    let mut rom = test_cart();
    rom[0] = 0xEA;                                              // keep interrupts on
    rom[2] = 0x40;                                              // NMIEN = VBI
    rom[0xFFD] = 0x00;                                          // the OS logo, then the cart
    let mut s = A5200System::from_rom(&rom, "test", &[]).unwrap();
    let none = 0x0F << KEYPAD_SHIFT;
    for _ in 0..200 { s.run_frame(none); }
    assert_eq!(s.bus.ram[2] > 0, true, "the VBI runs");
    assert_eq!((s.bus.ram[0x11], s.bus.ram[0x12]), (228, 228), "pots unpowered: joystick reads 228");
    s.bus.write_byte(0xC01F, 0x04);
    let mut read = |mask: u32| {
        for _ in 0..8 { s.run_frame(mask | none); }             // past the jitter
        (s.bus.ram[0x11], s.bus.ram[0x12])
    };
    assert_eq!(read(0), (114, 114), "centred");
    assert_eq!(read(LEFT), (1, 114));
    assert_eq!(read(RIGHT | DOWN), (227, 227));
    assert_eq!(read(UP), (114, 1));
    // a jump wiggles by one for a few frames, so both odd and even values show up
    s.run_frame(RIGHT | none);
    s.run_frame(RIGHT | none);
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..6 { seen.insert(s.bus.stick[0]); s.run_frame(RIGHT | none); }
    assert!(seen.len() > 1 && seen.iter().all(|&v| v >= 226), "{seen:?}");
    // pots 4-7: no controller
    assert_eq!(s.bus.pokey.pot_in[4], 228);
}

#[test]
fn detection() {
    let g = crate::common::rom_detect::detect(&test_cart(), "game.a52");
    assert_eq!(g[0].system, "a5200", "{g:?}");
}

#[test]
fn save_state_round_trip() {
    let mut s = A5200System::from_rom(&test_cart(), "test", &[]).unwrap();
    for _ in 0..5 { s.run_frame(0); }
    let snap = s.save_state();
    s.run_frame(0);
    let after = s.save_state();
    s.load_state(&snap).unwrap();
    s.run_frame(0);
    assert_eq!(s.save_state(), after);
}

