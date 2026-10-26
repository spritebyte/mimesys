// Board tests with hand-assembled programs (no game ROMs needed).
use super::digdug_system::DigDugSystem;
use crate::common::system::System;
use std::collections::HashMap;

fn regions() -> HashMap<String, Vec<u8>> {
    let mut main = vec![0u8; 0x4000];
    let prog: &[u8] = &[
        0x31, 0x00, 0x88,                   // ld sp,8800h
        0x3E, 0x01, 0x32, 0x20, 0x68,       // ld a,1 ; ld (6820h),a   main IRQ on
        0x32, 0x23, 0x68,                   // ld (6823h),a            release sub CPUs + customs
        0x3E, 0xA1, 0x32, 0x00, 0x71,       // ld a,A1h ; ld (7100h),a  06xx: chip 0, write, /32
        0x21, 0x40, 0x80, 0x36, 0x41,       // ld hl,8040h ; ld (hl),41h   (visible column 2, row 0)
        0xED, 0x56, 0xFB,                   // im 1 ; ei
        0x18, 0xFE,                         // jr $
    ];
    main[..prog.len()].copy_from_slice(prog);
    // IRQ: count at 8400, ack (write 0 then 1 to 6820)
    main[0x38..0x38 + 14].copy_from_slice(&[0x21, 0x00, 0x84, 0x34, 0xAF, 0x32, 0x20, 0x68, 0x3C, 0x32, 0x20, 0x68, 0xFB, 0xC9]);
    // NMI: count at 8401
    main[0x66..0x66 + 8].copy_from_slice(&[0xE5, 0x21, 0x01, 0x84, 0x34, 0xE1, 0xED, 0x45]);   // push hl .. pop hl
    // sound CPU: NMI counts at 8402
    let mut sub2 = vec![0u8; 0x4000];
    sub2[0..5].copy_from_slice(&[0x31, 0x00, 0x8C, 0x18, 0xFE]);     // ld sp,8C00h ; jr $
    sub2[0x66..0x66 + 6].copy_from_slice(&[0x21, 0x02, 0x84, 0x34, 0xED, 0x45]);
    let mut sub = vec![0u8; 0x4000];
    sub[0..2].copy_from_slice(&[0x18, 0xFE]);
    let mut gfx1 = vec![0u8; 0x1000];
    gfx1[0x41 * 8..0x41 * 8 + 8].fill(0xFF);     // text tile 0x41: solid
    let mut proms = vec![0u8; 0x220];
    proms[4] = 0x07;                              // color 4: full red
    let mut r = HashMap::new();
    for (k, v) in [("maincpu", main), ("sub", sub), ("sub2", sub2), ("gfx1", gfx1),
                   ("gfx2", vec![0u8; 0x4000]), ("gfx3", vec![0u8; 0x1000]), ("gfx4", vec![0u8; 0x1000]),
                   ("proms", proms), ("namco", vec![0u8; 0x200]),
                   ("n51xx", vec![0u8; 0x400]), ("n53xx", vec![0u8; 0x400])] {
        r.insert(k.to_string(), v);
    }
    r
}

#[test]
fn digdug_board_interrupts_06xx_and_video() {
    let mut sys = DigDugSystem::new("digdug", regions()).unwrap();
    for _ in 0..10 { sys.run_frame(0); }
    let irqs = sys.bus.ram[0];
    let nmis = sys.bus.ram[1];
    let snd_nmis = sys.bus.ram[2];
    assert!((9..=10).contains(&irqs), "vblank IRQs {irqs}");
    // 06xx at /32: one NMI per 2 x 1024 Z80 cycles -> ~24.75 a frame -> ~247 in 10 frames
    assert!((240..=250).contains(&nmis), "06xx NMIs {nmis}");
    assert!((19..=20).contains(&snd_nmis), "sound CPU NMIs {snd_nmis}");
    // text tile 0x41 at 8040 (visible column 2, row 0): color ((0x41 >> 4) & 0xE) | ((0x41 >> 3) & 2) = 4 = red
    let fb = sys.framebuffer();
    let px = |x: usize, y: usize| &fb[(y * 288 + x) * 4..(y * 288 + x) * 4 + 3];
    assert_eq!(px(16, 0), &[255, 0, 0]);
    assert_eq!(px(24, 0), &[0, 0, 0]);
}

#[test]
fn digdug_save_state_round_trip() {
    let mut sys = DigDugSystem::new("digdug", regions()).unwrap();
    crate::arcade::state::check_round_trip(&mut sys, 5, 20);
}

