// Board-level tests with tiny hand-assembled programs (no game ROMs needed).
use super::gottlieb_system::{GottliebSystem, Variant};
use crate::common::system::System;
use std::collections::HashMap;

fn regions(main: &[(usize, &[u8])], audio: &[(usize, &[u8])]) -> HashMap<String, Vec<u8>> {
    let mut m = vec![0u8; 0x10000];
    for (a, b) in main { m[*a..*a + b.len()].copy_from_slice(b); }
    let mut s = vec![0u8; 0x10000];
    for (a, b) in audio { s[*a..*a + b.len()].copy_from_slice(b); }
    let mut bg = vec![0u8; 0x2000];
    bg[32..64].fill(0x11);                 // tile 1: every pixel pen 1
    let mut r = HashMap::new();
    r.insert("maincpu".to_string(), m);
    r.insert("audiocpu".to_string(), s);
    r.insert("bgtiles".to_string(), bg);
    r.insert("sprites".to_string(), vec![0u8; 0x8000]);
    r
}

const MAIN_PROG: &[u8] = &[
    0xB8, 0x00, 0x00,                   // mov ax,0
    0x8E, 0xD0,                         // mov ss,ax
    0x8E, 0xD8,                         // mov ds,ax
    0xBC, 0x00, 0x2F,                   // mov sp,2F00h
    0xC7, 0x06, 0x08, 0x00, 0x00, 0x61, // mov word [0008h],6100h   NMI vector
    0xC7, 0x06, 0x0A, 0x00, 0x00, 0x00, // mov word [000Ah],0
    0xC6, 0x06, 0x02, 0x50, 0x00,       // mov byte [5002h],00h     pen 1: no green/blue
    0xC6, 0x06, 0x03, 0x50, 0x0F,       // mov byte [5003h],0Fh     pen 1: full red
    0xBF, 0x00, 0x38,                   // mov di,3800h
    0x8E, 0xC0,                         // mov es,ax
    0xB0, 0x01,                         // mov al,1
    0xB9, 0x00, 0x04,                   // mov cx,400h
    0xF3, 0xAA,                         // rep stosb                 fill the tilemap with tile 1
    0xC6, 0x06, 0x02, 0x58, 0x1F,       // mov byte [5802h],1Fh      "no command" (PA7 low)
    0xC6, 0x06, 0x02, 0x58, 0x12,       // mov byte [5802h],12h      sound command (PA7 rises)
    0xA2, 0x00, 0x58,                   // loop: mov [5800h],al      kick the watchdog
    0xEB, 0xFB,                         // jmp loop
];
const NMI: &[u8] = &[0xFF, 0x06, 0x00, 0x10, 0xCF];       // inc word [1000h]; iret
const RESET: &[u8] = &[0xEA, 0x00, 0x60, 0x00, 0x00];     // jmp far 0000:6000

// 6502: enable the PA7 positive-edge IRQ, then wait. The IRQ copies port A to the DAC.
const SOUND_PROG: &[u8] = &[
    0xA2, 0xFF, 0x9A,                   // ldx #$FF; txs
    0x8D, 0x07, 0x02,                   // sta $0207   edge control: positive, IRQ on
    0x58,                               // cli
    0x4C, 0x07, 0x70,                   // jmp *
];
const SOUND_IRQ: &[u8] = &[
    0xAD, 0x00, 0x02,                   // lda $0200
    0x8D, 0x00, 0x10,                   // sta $1000   DAC
    0xAD, 0x05, 0x02,                   // lda $0205   clear the edge flag
    0x40,                               // rti
];

#[test]
fn qbert_board_runs_nmi_video_and_sound_latch() {
    let r = regions(
        &[(0x6000, MAIN_PROG), (0x6100, NMI), (0xFFF0, RESET)],
        &[(0x7000, SOUND_PROG), (0x7100, SOUND_IRQ), (0x7FFA, &[0x00, 0x70, 0x00, 0x70, 0x00, 0x71])],
    );
    let mut sys = GottliebSystem::new(Variant::Qbert, "qbert", r).unwrap();
    for _ in 0..30 { sys.run_frame(0); }
    // one NMI per frame
    let n = sys.bus.ram[0] as u16 | (sys.bus.ram[1] as u16) << 8;
    assert!((28..=30).contains(&n), "NMI count {n}");
    // the whole screen is pen 1 = red
    let fb = sys.framebuffer();
    assert_eq!(&fb[0..4], &[255, 0, 0, 255]);
    assert_eq!(&fb[fb.len() - 4..], &[255, 0, 0, 255]);
    // command 0x12: PA0-5 = ~0x12, PA6 = 1, PA7 = 1
    assert_eq!(sys.bus.sound.dac_for_test(), 0x2D | 0x40 | 0x80);
    assert!(!sys.drain_audio().is_empty());
}

#[test]
fn watchdog_resets_a_stuck_board() {
    // jmp $ at the reset address: never kicks the watchdog
    let r = regions(&[(0xFFF0, &[0xEB, 0xFE])], &[]);
    let mut sys = GottliebSystem::new(Variant::Qbert, "qbert", r).unwrap();
    for _ in 0..20 { sys.run_frame(0); }
    assert!(sys.bus.watchdog <= 16);
}

#[test]
fn save_state_round_trip() {
    let r = regions(
        &[(0x6000, MAIN_PROG), (0x6100, NMI), (0xFFF0, RESET)],
        &[(0x7000, SOUND_PROG), (0x7100, SOUND_IRQ), (0x7FFA, &[0x00, 0x70, 0x00, 0x70, 0x00, 0x71])],
    );
    let mut sys = GottliebSystem::new(Variant::Qbert, "qbert", r).unwrap();
    crate::arcade::state::check_round_trip(&mut sys, 10, 20);
}
