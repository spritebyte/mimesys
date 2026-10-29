// Save state round trips for the Z80 boards, with a small program that keeps rewriting
// video RAM and random graphics ROMs so the picture changes every frame.
use crate::arcade::state::check_round_trip;
use std::collections::HashMap;

fn noise(len: usize, seed: u32) -> Vec<u8> {
    let mut x = seed;
    (0..len).map(|_| { x = x.wrapping_mul(1664525).wrapping_add(1013904223); (x >> 24) as u8 }).collect()
}

/// ld hl,vid ; L: inc (hl) ; inc hl ; ld a,h ; cp vid_hi+4 ; jr nz,L ; jr start
fn video_loop(vid: u16, size: usize) -> Vec<u8> {
    let mut rom = vec![0u8; size];
    let p = [0x21, vid as u8, (vid >> 8) as u8, 0x34, 0x23, 0x7C, 0xFE, ((vid >> 8) as u8) + 4, 0x20, 0xF9, 0x18, 0xF4];
    rom[0..3].copy_from_slice(&[0x00, 0x00, 0x00]);
    rom[3..3 + p.len()].copy_from_slice(&p);
    rom
}

fn regions(list: &[(&str, Vec<u8>)]) -> HashMap<String, Vec<u8>> {
    list.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

#[test]
fn pacman_round_trip() {
    use crate::arcade::pacman::pacman_system::{PacmanSystem, Variant};
    let r = regions(&[("maincpu", video_loop(0x4000, 0x4000)), ("gfx1", noise(0x2000, 1)),
                      ("proms", noise(0x120, 2)), ("namco", noise(0x200, 3))]);
    let mut sys = PacmanSystem::new(Variant::Pacman, "pacman", r).unwrap();
    check_round_trip(&mut sys, 5, 20);
}

#[test]
fn galaxian_round_trip() {
    use crate::arcade::galaxian::galaxian_system::{GalaxianSystem, Variant};
    let r = regions(&[("maincpu", video_loop(0x5000, 0x4000)), ("gfx1", noise(0x1000, 4)), ("proms", noise(0x20, 5))]);
    let mut sys = GalaxianSystem::new(Variant::Galaxian, "galaxian", r).unwrap();
    check_round_trip(&mut sys, 5, 20);
}

#[test]
fn frogger_round_trip() {
    use crate::arcade::galaxian::galaxian_system::{GalaxianSystem, Variant};
    let r = regions(&[("maincpu", video_loop(0xA800, 0x4000)), ("gfx1", noise(0x1000, 6)),
                      ("proms", noise(0x20, 7)), ("audiocpu", vec![0u8; 0x2000])]);
    let mut sys = GalaxianSystem::new(Variant::Frogger, "frogger", r).unwrap();
    check_round_trip(&mut sys, 5, 20);
}

#[test]
fn popeye_round_trip() {
    use crate::arcade::popeye::popeye_system::PopeyeSystem;
    let r = regions(&[("maincpu", noise(0x8000, 8)), ("gfx1", noise(0x800, 9)), ("gfx2", noise(0x8000, 10)),
                      ("proms", noise(0x40, 11)), ("sprpal", noise(0x100, 12))]);
    let mut sys = PopeyeSystem::new("popeye", r, true).unwrap();
    check_round_trip(&mut sys, 5, 20);
}

#[test]
fn dkong_round_trip() {
    use crate::arcade::donkey_kong::dk_system::DKSystem;
    let r = regions(&[("maincpu", video_loop(0x7400, 0x4000)), ("gfx1", noise(0x1000, 13)), ("gfx2", noise(0x2000, 14)),
                      ("proms", noise(0x300, 15)), ("soundcpu", noise(0x1800, 16))]);
    let mut sys = DKSystem::new(crate::arcade::donkey_kong::dk_system::Variant::DonkeyKong, "dkong".into(), r).unwrap();
    check_round_trip(&mut sys, 5, 20);
}

#[test]
fn hiscore_wrapper_saves_table() {
    use crate::arcade::load_arcade;
    let dat = b"pacman:\n@:maincpu,program,4c00,4,00,00\n".to_vec();
    let mut r = regions(&[("maincpu", video_loop(0x4000, 0x4000)), ("gfx1", noise(0x2000, 1)),
                          ("proms", noise(0x120, 2)), ("namco", noise(0x200, 3))]);
    r.insert("hiscore.dat".into(), dat);
    let mut sys = load_arcade("pacman", "pacman", r).unwrap();
    assert!(sys.has_battery());
    sys.load_sram(&[0x00, 0x12, 0x34, 0x00]);
    sys.run_frame(0);
    // RAM 4C00-4C03 starts at 0, so the defaults "match" and the saved table goes in;
    // the loop program never touches 4C00, so the table stays put and nothing is dirty.
    assert!(!sys.is_sram_dirty());
    assert_eq!(sys.get_sram().unwrap(), &[0x00, 0x12, 0x34, 0x00]);
}

#[test]
fn dkongjr_gfx_bank_selects_upper_tiles() {
    use crate::arcade::donkey_kong::dk_system::{DKSystem, Variant};
    use crate::common::system::System;
    // ld a,1 ; ld (7C80h),a ; jr $
    let mut rom = vec![0u8; 0x6000];
    rom[..7].copy_from_slice(&[0x3E, 0x01, 0x32, 0x80, 0x7C, 0x18, 0xFE]);
    let mut gfx1 = vec![0u8; 0x2000];
    gfx1[256 * 8..256 * 8 + 8].fill(0xFF);          // tile 256, plane 0: solid
    let make = |v| {
        let r = regions(&[("maincpu", rom.clone()), ("gfx1", gfx1.clone()), ("gfx2", vec![0u8; 0x2000]),
                          ("proms", noise(0x300, 21))]);
        let mut s = DKSystem::new(v, "dkongjr".into(), r).unwrap();
        for _ in 0..3 { s.run_frame(0); }
        s.framebuffer()[0..4].to_vec()
    };
    let jr = make(Variant::DonkeyKongJr);
    let dk = make(Variant::DonkeyKong);
    assert_ne!(jr, dk, "DK Jr shows tile 256 (bank 1), DK ignores the bank write");
}

fn dkong3_regions(rom: Vec<u8>, gfx1: Vec<u8>, gfx2: Vec<u8>, proms: Vec<u8>) -> std::collections::HashMap<String, Vec<u8>> {
    // sound: the RP2A03s just loop (reset vector 8000: JMP 8000)
    let mut snd = vec![0u8; 0x10000];
    snd[0x8000..0x8003].copy_from_slice(&[0x4C, 0x00, 0x80]);
    snd[0x9FFA..0xA000].copy_from_slice(&[0x00, 0x80, 0x00, 0x80, 0x00, 0x80]);
    regions(&[("maincpu", rom), ("gfx1", gfx1), ("gfx2", gfx2), ("proms", proms),
              ("rp2a03a", snd.clone()), ("rp2a03b", snd)])
}

#[test]
fn dkong3_round_trip() {
    use crate::arcade::donkey_kong::dk3_system::Dk3System;
    let r = dkong3_regions(video_loop(0x7400, 0x6000), noise(0x2000, 31), noise(0x4000, 32), noise(0x500, 33));
    let mut sys = Dk3System::new("dkong3".into(), r).unwrap();
    check_round_trip(&mut sys, 5, 20);
}

#[test]
fn dkong3_dma_copies_sprites_and_bit6_picks_upper_bank() {
    use crate::arcade::donkey_kong::dk3_system::Dk3System;
    use crate::common::system::System;
    let mut rom = vec![0u8; 0x10000];
    let code: &[u8] = &[
        0x3E, 0x80, 0x32, 0x00, 0x69,   // sprite y = 80
        0x3E, 0x00, 0x32, 0x01, 0x69,   // code 0
        0x3E, 0x40, 0x32, 0x02, 0x69,   // bit 6: +128, colour 0
        0x3E, 0x88, 0x32, 0x03, 0x69,   // x
        0x21, 0x00, 0x01,               // ld hl,0100
        0x06, 13, 0x0E, 0x00,           // ld b,13 ; ld c,0
        0xED, 0xB3,                     // otir: program the DMA (port 0)
        0x3E, 0x01, 0x32, 0x85, 0x7E,   // RDY: copy 6900-6A7F to 7000
        0x18, 0xFE,
    ];
    rom[..code.len()].copy_from_slice(code);
    rom[0x100..0x10D].copy_from_slice(&[0x7D, 0x00, 0x69, 0x7F, 0x01, 0x14, 0x10, 0xAD, 0x00, 0x70, 0x8A, 0xCF, 0x87]);
    let mut gfx2 = vec![0u8; 0x4000];
    gfx2[0x800..0x810].fill(0xFF);              // sprite 128, left half, plane 0
    gfx2[0x1800..0x1810].fill(0xFF);            // right half
    let mut proms = vec![0u8; 0x500];
    proms[1] = 0xFF;                            // pen 1: red + green on
    let mut sys = Dk3System::new("dkong3".into(), dkong3_regions(rom, vec![0u8; 0x2000], gfx2, proms)).unwrap();
    for _ in 0..2 { sys.run_frame(0); }
    assert_eq!(sys.bus.video.sprite_ram[0..4], [0x80, 0x00, 0x40, 0x88], "DMA copied the sprite");
    let fb = sys.framebuffer();
    let px = |x: usize, y: usize| fb[(y * 256 + x) * 4..(y * 256 + x) * 4 + 3].to_vec();
    assert_ne!(px(0x84, 125 - 16), px(0x10, 125 - 16), "sprite 128 drawn");
}
