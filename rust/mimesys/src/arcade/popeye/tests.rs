use super::popeye_system::{PopeyeSystem, decrypt_tpp2, bitswap};
use super::popeye_video::{self, PopeyeVideo, SCREEN_W};
use crate::arcade::ay8910::Ay8910;
use crate::common::cpu::z80::Bus;
use crate::common::system::System;
use std::collections::HashMap;

/// Scramble a plain program the way the TPP2 board's ROMs are stored.
fn encrypt_tpp2(plain: &[u8]) -> Vec<u8> {
    let mut inv = [0u8; 256];
    for b in 0..256usize { inv[bitswap(b, &[3, 4, 2, 5, 1, 6, 0, 7])] = b as u8; }
    let mut rom = vec![0u8; 0x8000];
    for i in 0..0x8000 {
        let a = bitswap(i, &[15, 14, 13, 12, 11, 10, 8, 7, 6, 3, 9, 5, 4, 2, 1, 0]) ^ 0x3F;
        rom[a] = inv[plain[i] as usize];
    }
    rom
}

fn regions(plain_program: &[u8]) -> HashMap<String, Vec<u8>> {
    let mut prog = vec![0u8; 0x8000];
    prog[..plain_program.len()].copy_from_slice(plain_program);
    let mut r = HashMap::new();
    r.insert("maincpu".into(), encrypt_tpp2(&prog));
    r.insert("gfx1".into(), vec![0; 0x800]);
    r.insert("gfx2".into(), vec![0; 0x8000]);
    r.insert("proms".into(), vec![0; 0x40]);
    r.insert("sprpal".into(), vec![0; 0x100]);
    r
}

#[test]
fn rom_decryption_round_trips() {
    let plain: Vec<u8> = (0..0x8000).map(|i| (i * 7 + 3) as u8).collect();
    assert_eq!(decrypt_tpp2(&encrypt_tpp2(&plain)), plain);
}

/// NMI handler that re-arms itself by toggling bit 0 of I, like the real game.
fn nmi_program(rearm: bool) -> Vec<u8> {
    let mut p = vec![0u8; 0x100];
    let main = [0x31, 0x00, 0x8C,        // ld sp,8C00
                0x3E, 0x01, 0xED, 0x47,  // ld a,1 ; ld i,a   (NMI on)
                0x18, 0xFE];             // jr $
    p[..main.len()].copy_from_slice(&main);
    let mut nmi = vec![0x21, 0x00, 0x88, 0x34];   // ld hl,8800 ; inc (hl)
    if rearm {
        nmi.extend([0xAF, 0xED, 0x47,             // xor a ; ld i,a  (NMI off)
                    0x3E, 0x01, 0xED, 0x47]);     // ld a,1 ; ld i,a (NMI on)
    }
    nmi.extend([0xED, 0x45]);                     // retn
    p[0x66..0x66 + nmi.len()].copy_from_slice(&nmi);
    p
}

#[test]
fn nmi_follows_the_i_register_and_is_edge_triggered() {
    let mut sys = PopeyeSystem::new("popeye", regions(&nmi_program(true)), true).unwrap();
    for _ in 0..10 { sys.run_frame(0); }
    // the NMI fires at the end of each frame, so the 10th one is handled during frame 11
    assert_eq!(sys.bus.read(0x8800), 9, "one NMI per frame when re-armed");

    let mut sys = PopeyeSystem::new("popeye", regions(&nmi_program(false)), true).unwrap();
    for _ in 0..10 { sys.run_frame(0); }
    assert_eq!(sys.bus.read(0x8800), 1, "line stays high without re-arming -> only one edge");
}

#[test]
fn protection_chip_shifts_last_two_writes() {
    let mut sys = PopeyeSystem::new("popeye", regions(&[0x18, 0xFE]), true).unwrap();
    sys.bus.write(0xE001, 0x12);
    sys.bus.write(0xE001, 0x34);
    sys.bus.write(0xE000, 4);
    assert_eq!(sys.bus.read(0xE000), 0x41);   // (0x34 << 4 | 0x12 >> 4) & 0xFF
    assert_eq!(sys.bus.read(0xE001) & 0x04, 0);
    let mut noalu = PopeyeSystem::new("popeyeu", regions(&[0x18, 0xFE]), false).unwrap();
    noalu.bus.write(0xE001, 0x34);
    assert_eq!(noalu.bus.read(0xE000), 0);
}

#[test]
fn inputs_dips_and_field_bit() {
    let mut sys = PopeyeSystem::new("popeye", regions(&[0x18, 0xFE]), true).unwrap();
    use crate::common::input as pad;
    sys.set_input(pad::LEFT | pad::A | pad::COIN | pad::START);
    assert_eq!(sys.bus.port_in(0x00), 0x12);
    let s = sys.bus.port_in(0x02);
    assert_eq!(s & !0x10, 0x84);
    let f0 = s & 0x10;
    sys.run_frame(0);
    assert_ne!(sys.bus.port_in(0x02) & 0x10, f0, "field bit toggles every frame");

    // AY port B picks which DSW1 bit shows up on DSW0 bit 7 (read through AY port A)
    sys.bus.port_out(0x00, 7);  sys.bus.port_out(0x01, 0x80);   // port B = output
    for bit in 0..8u8 {
        sys.bus.port_out(0x00, 15); sys.bus.port_out(0x01, bit << 1);
        sys.bus.port_out(0x00, 14);
        let a = sys.bus.port_in(0x03);
        assert_eq!(a & 0x7F, 0x5F);
        assert_eq!(a >> 7, (0x3D >> bit) & 1, "dsw bit {bit}");
    }
    sys.bus.port_out(0x00, 15); sys.bus.port_out(0x01, 0x01);
    assert!(sys.bus.video.flip);
}

#[test]
fn ay_tone_pitch_and_mixer() {
    let mut ay = Ay8910::new(2_000_000, 44100);
    // channel A: period 250 -> 2 MHz / (16 * 250) = 500 Hz, volume 15, tone only
    for (r, v) in [(0, 250u8), (1, 0), (7, 0x3E), (8, 15)] { ay.write_address(r); ay.write_data(v); }
    for _ in 0..250_000 { ay.tick(); }   // one second
    let left: Vec<f32> = ay.out.iter().step_by(2).copied().collect();
    assert!((44099..=44101).contains(&left.len()));
    let rising = left.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
    assert!((495..=505).contains(&rising), "{rising} Hz");

    // all channels muted by the mixer -> silence (after the DC filter settles)
    ay.out.clear();
    ay.write_address(7); ay.write_data(0x3F);
    for _ in 0..250_000 { ay.tick(); }
    assert!(ay.out[ay.out.len() - 100..].iter().all(|s| s.abs() < 1e-3));
}

#[test]
fn ay_envelope_decay_then_hold() {
    let mut ay = Ay8910::new(2_000_000, 44100);
    // tone off, noise off but channel A "on" -> output = volume level directly
    for (r, v) in [(7, 0x3F), (8, 0x10), (11, 1), (12, 0), (13, 0x00)] { ay.write_address(r); ay.write_data(v); }
    // envelope shape 0: decay 15 -> 0 then stay at 0; one step = 2 ticks at period 1
    let mut seen = vec![];
    for _ in 0..40 { ay.tick(); seen.push(ay_env(&ay)); }
    assert!(seen[0] >= 14);
    assert_eq!(*seen.last().unwrap(), 0);
    assert!(seen.windows(2).all(|w| w[1] <= w[0]), "monotonic decay");
}
fn ay_env(ay: &Ay8910) -> u8 { ay.envelope_volume_for_test() }

#[test]
fn background_nibbles_and_foreground_chars() {
    let mut gfx1 = vec![0u8; 0x800];
    gfx1[8] = 0x01;                       // char 1, row 0: leftmost pixel set
    let mut proms = vec![0u8; 0x40];
    proms[0x20 + 3] = 0x07;               // char color 3
    proms[0x02] = 0x38;                   // background color 2
    let mut v = PopeyeVideo::new(&gfx1, &vec![0; 0x8000], &proms, &vec![0; 0x100]).unwrap();

    v.background_write(0x0005, 0x0A);       // low nibble of byte 5
    v.background_write(0x1005, 0x8B);       // high nibble of byte 5 (A12 set), only low 4 bits of data used
    assert_eq!(v.background_ram[5], 0xBA);

    // text tile at column 3, row 4 -> screen x 48, y 64 (32 lines are cropped off the top)
    v.video_ram[4 * 32 + 3] = 1;
    v.color_ram[4 * 32 + 3] = 3;
    v.render_frame();
    let pal = popeye_video::palette_of(&mut v);
    let px = |v: &PopeyeVideo, x: usize, y: usize| { let o = ((y - 32) * SCREEN_W + x) * 4; [v.framebuffer[o], v.framebuffer[o + 1], v.framebuffer[o + 2]] };
    assert_eq!(px(&v, 48, 64), pal[16 + 2 * 3 + 1]);
    assert_eq!(px(&v, 49, 65), pal[16 + 2 * 3 + 1], "2x scaled");
    assert_ne!(pal[16 + 2 * 3 + 1], pal[0]);
}

#[test]
fn sprite_decode_and_position() {
    let mut gfx2 = vec![0u8; 0x8000];
    // sprite code 0x1FF (attr code 0 ^ 0x1FF): plane 0 bytes, left half lives in the 2nd quarter
    let code = 0x1FF;
    for y in 0..16 { gfx2[0x2000 + code * 16 + y] = 0xFF; gfx2[code * 16 + y] = 0xFF; }
    let mut sprpal = vec![0u8; 0x100];
    sprpal[4 * 1 + 2] = 0x07;               // sprite color 1, pixel value 2 (plane 0 only)
    let mut v = PopeyeVideo::new(&vec![0; 0x800], &gfx2, &vec![0; 0x40], &sprpal).unwrap();
    let mut dma = vec![0u8; 0x280];
    dma[4] = 100;          // x: sx = 200, drawn from 194
    dma[5] = 100;          // y: 0x200 - 200 = 312
    dma[6] = 0;            // code 0 -> 0x1FF
    dma[7] = 1;            // color 1
    v.dma(&dma);
    v.render_frame();
    let pal = popeye_video::palette_of(&mut v);
    let px = |v: &PopeyeVideo, x: usize, y: usize| { let o = ((y - 32) * SCREEN_W + x) * 4; [v.framebuffer[o], v.framebuffer[o + 1], v.framebuffer[o + 2]] };
    assert_eq!(px(&v, 194, 312), pal[48 + 4 + 2]);
    assert_eq!(px(&v, 209, 327), pal[48 + 4 + 2]);
    assert_eq!(px(&v, 193, 312), pal[0]);
    assert_eq!(px(&v, 194, 328), pal[0]);
}

#[test]
fn resistor_network_extremes() {
    // inverted monitor: all inputs high -> darkest, all low -> brightest
    let (txt_hi, bak_hi) = popeye_video::res_net_for_test(7);
    let (txt_lo, bak_lo) = popeye_video::res_net_for_test(0);
    assert!(txt_lo > 200 && bak_lo > 150, "{txt_lo} {bak_lo}");
    assert!(txt_hi < 40 && bak_hi < 40, "{txt_hi} {bak_hi}");
    // more inputs high -> darker, step by step
    let levels: Vec<u8> = [0u8, 1, 2, 4, 7].iter().map(|&i| popeye_video::res_net_for_test(i).0).collect();
    assert!(levels[0] > levels[1] && levels[1] > levels[2] && levels[2] > levels[3] && levels[3] > levels[4], "{levels:?}");
}

#[test]
fn frame_makes_audio_and_full_picture() {
    let mut sys = PopeyeSystem::new("popeye", regions(&[0x18, 0xFE]), true).unwrap();
    sys.run_frame(0);
    let a = sys.drain_audio();
    assert!((1460..=1475).contains(&a.len()), "{}", a.len());   // 44100 / 59.94 * 2
    assert_eq!(sys.framebuffer().len(), 512 * 448 * 4);
}
