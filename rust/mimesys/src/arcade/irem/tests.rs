// Board tests with hand-assembled Z80 and 6803 programs (no game ROMs needed).
use super::m62_system::M62System;
use crate::common::system::System;
use std::collections::HashMap;

const Z80_PROG: &[(usize, &[u8])] = &[
    (0x0000, &[
        0x31, 0x00, 0xF0,             // ld sp,F000h
        0x21, 0x10, 0xD0, 0x36, 0x01, // ld hl,D010h ; ld (hl),1      tile 1, row 0, col 16
        0x21, 0x10, 0xD8, 0x36, 0x02, // ld hl,D810h ; ld (hl),2      color 2
        0x3E, 0x05, 0xD3, 0x00,       // ld a,5 ; out (0),a          sound command 5
        0xED, 0x56, 0xFB,             // im 1 ; ei
        0x18, 0xFE,                   // jr $
    ]),
    (0x0038, &[0x21, 0x00, 0xE0, 0x34, 0xFB, 0xED, 0x4D]), // inc (E000h) ; ei ; reti
];

// 6803: set AY 45M port B as output and write 0x10 (MSM #1: /96, 4-bit, running),
// then wait. NMI (MSM VCK) counts and feeds a nibble; IRQ reads the command through
// AY port A and acknowledges.
const SND_MAIN: &[u8] = &[
    0x8E, 0x00, 0xFF,
    0x86, 0xFF, 0x97, 0x00, 0x86, 0x1F, 0x97, 0x01,
    0x86, 0x07, 0x97, 0x02, 0x86, 0x0D, 0x97, 0x03, 0x86, 0x0C, 0x97, 0x03,
    0x86, 0x80, 0x97, 0x02, 0x86, 0x09, 0x97, 0x03, 0x86, 0x08, 0x97, 0x03,
    0x86, 0x0F, 0x97, 0x02, 0x86, 0x0D, 0x97, 0x03, 0x86, 0x0C, 0x97, 0x03,
    0x86, 0x10, 0x97, 0x02, 0x86, 0x09, 0x97, 0x03, 0x86, 0x08, 0x97, 0x03,
    0x0E, 0x20, 0xFE,
];
const SND_NMI: &[u8] = &[0x7C, 0x00, 0x80, 0x86, 0x07, 0xB7, 0x08, 0x01, 0x3B];
const SND_IRQ: &[u8] = &[
    0x86, 0x0E, 0x97, 0x02, 0x86, 0x0D, 0x97, 0x03, 0x86, 0x0C, 0x97, 0x03,
    0x86, 0x00, 0x97, 0x00, 0xD6, 0x02, 0xD7, 0x81,
    0x86, 0xFF, 0x97, 0x00, 0xB7, 0x08, 0x00, 0x3B,
];

fn regions() -> HashMap<String, Vec<u8>> {
    let mut main = vec![0u8; 0x8000];
    for (a, b) in Z80_PROG { main[*a..*a + b.len()].copy_from_slice(b); }
    let mut snd = vec![0u8; 0x10000];
    snd[0x4000..0x4000 + SND_MAIN.len()].copy_from_slice(SND_MAIN);
    snd[0x4100..0x4100 + SND_NMI.len()].copy_from_slice(SND_NMI);
    snd[0x4200..0x4200 + SND_IRQ.len()].copy_from_slice(SND_IRQ);
    snd[0xFFF8..0xFFFA].copy_from_slice(&[0x42, 0x00]);
    snd[0xFFFC..0xFFFE].copy_from_slice(&[0x41, 0x00]);
    snd[0xFFFE..].copy_from_slice(&[0x40, 0x00]);
    let mut gfx1 = vec![0u8; 0x6000];
    for plane in 0..3 { gfx1[plane * 0x2000 + 8..plane * 0x2000 + 16].fill(0xFF); } // tile 1 = pen 7
    let mut chr = vec![0u8; 0x300];
    chr[2 * 8 + 7] = 0x0F;          // color 2, pen 7: full red
    let mut r = HashMap::new();
    r.insert("maincpu".into(), main);
    r.insert("audiocpu".into(), snd);
    r.insert("gfx1".into(), gfx1);
    r.insert("gfx2".into(), vec![0u8; 0x18000]);
    r.insert("spr_height_prom".into(), vec![0u8; 0x20]);
    r.insert("spr_color_proms".into(), vec![0u8; 0x300]);
    r.insert("chr_color_proms".into(), chr);
    r
}

#[test]
fn kungfum_board_video_irq_and_sound() {
    let mut sys = M62System::new("kungfum", regions()).unwrap();
    for _ in 0..60 { sys.run_frame(0); }
    // one IRQ per frame
    let frames = sys.bus.read_ram(0);
    assert!((58..=60).contains(&frames), "frames {frames}");
    // tile 1 at the left edge of the status bar, red
    let fb = sys.framebuffer();
    assert_eq!(&fb[0..4], &[255, 0, 0, 255]);
    assert_eq!(&fb[8 * 4..8 * 4 + 4], &[0, 0, 0, 255]);
    // sound CPU: ~4000 NMIs a second from the MSM5205 (counter wraps at 256)
    let snd = sys.bus.sound.ram();
    assert_eq!(snd[1], 5, "command read through AY port A");
    assert!(!sys.drain_audio().is_empty());
}

#[test]
fn msm_nmi_rate() {
    let mut sys = M62System::new("kungfum", regions()).unwrap();
    sys.run_frame(0);
    let before = sys.bus.sound.ram()[0];
    // 10 frames at 56.34 Hz = 0.1775 s -> 710 NMIs (mod 256 = 198)
    for _ in 0..10 { sys.run_frame(0); }
    let n = sys.bus.sound.ram()[0].wrapping_sub(before) as i32;
    let expected = (710 % 256) as i32;
    assert!((n - expected).abs() <= 3, "nmi count {n} vs {expected}");
}


#[test]
fn save_state_round_trip() {
    let mut sys = M62System::new("kungfum", regions()).unwrap();
    crate::arcade::state::check_round_trip(&mut sys, 20, 30);
}
