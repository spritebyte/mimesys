use super::dec0_system::*;
use crate::common::system::System;
use std::collections::HashMap;

fn put_words(rom: &mut [u8], at: usize, words: &[u16]) {
    for (k, w) in words.iter().enumerate() { rom[at + 2 * k..at + 2 * k + 2].copy_from_slice(&w.to_be_bytes()); }
}

/// A tiny board: the 68000 mails the HuC6280 and sends a sound command; the HuC6280 answers
/// through the dual-port RAM; the 65C02 stores the command; VBlank counts frames.
fn board() -> Dec0System { board_with(|_| {}) }

fn board_with(patch_snd: impl Fn(&mut Vec<u8>)) -> Dec0System {
    let mut main = vec![0u8; 0x60000];
    put_words(&mut main, 0, &[0x00FF, 0xC000, 0x0000, 0x0400]);   // SSP, PC
    put_words(&mut main, 0x70, &[0x0000, 0x0500]);                // level 4 autovector
    put_words(&mut main, 0x78, &[0x0000, 0x0600]);                // level 6 autovector
    put_words(&mut main, 0x400, &[
        0x46FC, 0x2000,                         // move.w #$2000, sr
        0x13FC, 0x005A, 0x0018, 0x0FFF,         // move.b #$5A, $180FFF (mailbox to the HuC6280)
        0x13FC, 0x0012, 0x0030, 0xC015,         // move.b #$12, $30C015 (sound latch)
        0x60FE,                                 // bra *
    ]);
    put_words(&mut main, 0x500, &[
        0x13F9, 0x0018, 0x0FFD, 0x00FF, 0x8000, // move.b $180FFD, $FF8000 (the answer; acks IRQ 4)
        0x4E73,                                 // rte
    ]);
    put_words(&mut main, 0x600, &[
        0x5279, 0x00FF, 0x8002,                 // addq.w #1, $FF8002
        0x33FC, 0x0000, 0x0030, 0xC018,         // move.w #0, $30C018 (VBlank ack)
        0x4E73,
    ]);
    let mut sub = vec![0xFFu8; 0x10000];
    let code: &[u8] = &[
        0xA9, 0xF8, 0x53, 0x02,     // lda #$F8 ; tam #2 (zero page = 1F0000)
        0xA9, 0xF9, 0x53, 0x04,     // lda #$F9 ; tam #4 (4000 = the dual-port RAM)
        0x58,                       // cli
        0x80, 0xFE,                 // bra *
    ];
    sub[0x1E00..0x1E00 + code.len()].copy_from_slice(code);
    let irq: &[u8] = &[0xAD, 0xFF, 0x47, 0x1A, 0x8D, 0xFE, 0x47, 0x40];   // lda $47FF ; inc ; sta $47FE ; rti
    sub[0x1E20..0x1E20 + irq.len()].copy_from_slice(irq);
    sub[0x1FF8..0x1FFA].copy_from_slice(&[0x20, 0xFE]);   // IRQ1
    sub[0x1FFE..0x2000].copy_from_slice(&[0x00, 0xFE]);   // reset
    let mut snd = vec![0xFFu8; 0x10000];
    snd[0x8000..0x8003].copy_from_slice(&[0x4C, 0x00, 0x80]);                         // jmp $8000
    snd[0x8010..0x8016].copy_from_slice(&[0xAD, 0x00, 0x30, 0x85, 0x10, 0x40]);       // lda $3000 ; sta $10 ; rti
    snd[0xFFFA..0xFFFE].copy_from_slice(&[0x10, 0x80, 0x00, 0x80]);                   // NMI, reset
    patch_snd(&mut snd);
    let mut r = HashMap::new();
    r.insert("maincpu".to_string(), main);
    r.insert("audiocpu".to_string(), snd);
    r.insert("sub".to_string(), sub);
    for g in ["char", "tiles1", "tiles2", "sprites"] { r.insert(g.to_string(), vec![0; 0x10000]); }
    r.insert("oki".to_string(), vec![0; 0x10000]);
    Dec0System::new("robocop", r).unwrap()
}

#[test]
fn the_three_cpus_talk() {
    let mut s = board();
    for _ in 0..5 { s.run_frame(0); }
    assert_eq!(s.board.ram[0], 0x5B, "68000 -> HuC6280 mailbox -> answer in the dual-port RAM -> IRQ 4");
    let vbl = u16::from_be_bytes([s.board.ram[2], s.board.ram[3]]);
    assert!((4..=5).contains(&vbl), "one VBlank IRQ 6 a frame, acked ({vbl})");
    assert_eq!(s.board.mail, [1, 1]);
    assert_eq!(s.snd.pc & 0xFFF0, 0x8000, "the 65C02 is back in its loop");
    assert_eq!(s.board.latch_ram(0x10), 0x12, "the sound command arrived through the latch NMI");
    assert!(!s.out.is_empty(), "audio comes out");
}

#[test]
fn inputs_and_dips_read_back() {
    let mut s = board();
    s.set_input(crate::common::input::UP | crate::common::input::B | crate::common::input::COIN);
    use crate::common::cpu::m68000::M68kBus;
    assert_eq!(s.board.read_word(0x30C000) & 0x00FF, 0x00EE, "P1 up + attack, active low");
    assert_eq!(s.board.read_word(0x30C002) & 0x7F, 0x6F, "coin 1");
    assert_eq!(s.board.read_word(0x30C004), 0xFF7F, "factory DIPs");
}

/// A graphics region where element `code` is all `pen`: `planes` = the quarter holding
/// each pen bit (high bit first), `bytes` per element per plane
fn gfx_with(len: usize, planes: [usize; 4], bytes: usize, code: usize, pen: u8) -> Vec<u8> {
    let mut r = vec![0u8; len];
    let q = len / 4;
    for (k, &quarter) in planes.iter().enumerate() {
        if pen >> (3 - k) & 1 != 0 {
            let a = quarter * q + code * bytes;
            r[a..a + bytes].fill(0xFF);
        }
    }
    r
}

fn video_board() -> Dec0System {
    let mut s = board();
    let tiles = [1, 3, 0, 2];
    let chars = [0, 2, 1, 3];
    s.gfx = super::dec0_video::Gfx::new(
        &gfx_with(0x10000, chars, 8, 1, 9),
        &gfx_with(0x10000, tiles, 32, 1, 6),
        &gfx_with(0x10000, tiles, 32, 1, 3),
        &gfx_with(0x10000, tiles, 32, 1, 4),
    );
    let b = &mut s.board;
    for (n, c0) in [(0, 0x0003u16), (1, 0x0002), (2, 0x0002)] {
        b.bac[n].ctrl = [c0, 0, 0, 1];
    }
    // chip 2 = background (priority bit 0 clear): every tile is code 1
    for i in 0..0x400 { b.bac[2].vram[2 * i + 1] = 1; }
    // chip 1 = foreground: column 1, row 0 is code 1, colour 1
    b.bac[1].vram[2..4].copy_from_slice(&0x1001u16.to_be_bytes());
    // chip 0 = text: column 4, row 1
    b.bac[0].vram[2 * 36..2 * 36 + 2].copy_from_slice(&0x0001u16.to_be_bytes());
    s
}

fn px(s: &Dec0System, r: usize, x: usize) -> u16 {
    let mut out = vec![0u16; SCREEN_W * SCREEN_H];
    super::dec0_video::render(&s.board, &s.gfx, 1, &mut out);
    out[r * SCREEN_W + x]
}

#[test]
fn the_layers_stack_background_foreground_text() {
    let s = video_board();
    assert_eq!(px(&s, 0, 0), 768 + 3, "background (tiles2), drawn opaque");
    assert_eq!(px(&s, 0, 20), 512 + 16 + 6, "foreground (tiles1) over it");
    assert_eq!(px(&s, 0, 34), 9, "text (chars) on top");
    assert_eq!(px(&s, 9, 34), 768 + 3, "the text cell is only 8 lines high");
}

#[test]
fn sprites_sit_in_front_unless_the_priority_puts_them_behind() {
    let mut s = video_board();
    // a 16x16 sprite at screen (100, 50): code 1, colour 2
    let words = [0x8000u16 | (240 - 58), 1, 0x2000 | (240 - 100), 0];
    for (k, w) in words.iter().enumerate() { s.board.sprite_buf[2 * k..2 * k + 2].copy_from_slice(&w.to_be_bytes()); }
    assert_eq!(px(&s, 50, 100), 256 + 32 + 4, "sprite over the background");
    // a foreground tile under it (column 6, row 3 of the 16x16 map)
    let i = 6 + (3 << 4);
    s.board.bac[1].vram[2 * i..2 * i + 2].copy_from_slice(&0x0001u16.to_be_bytes());
    assert_eq!(px(&s, 50, 100), 256 + 32 + 4, "and over the foreground");
    s.board.pri = 2;                     // colour bit 3 clear (= priority bit 2): behind
    assert_eq!(px(&s, 50, 100), 512 + 6, "now behind the foreground");
    assert_eq!(px(&s, 60, 100), 256 + 32 + 4, "but still over the background where there's no foreground");
}

#[test]
fn the_palette_is_xbgr_over_two_rams() {
    let mut s = video_board();
    // colour 768 + 3: R 0x12, G 0x34 in the first RAM, B 0x56 in the second
    let i = 768 + 3;
    s.board.palette[2 * i..2 * i + 2].copy_from_slice(&0x3412u16.to_be_bytes());
    s.board.palette_ext[2 * i..2 * i + 2].copy_from_slice(&0x0056u16.to_be_bytes());
    s.render();
    assert_eq!(&s.framebuffer[0..4], &[0x12, 0x34, 0x56, 0xFF]);
}

/// The sound program's clock: YM3812 timer A interrupts the 65C02 until it's reset
#[test]
fn the_ym3812_timer_interrupts_the_sound_cpu() {
    let mut s = board_with(|snd| {
        let code: &[u8] = &[
            0xA9, 0x02, 0x8D, 0x00, 0x10, 0xA9, 0x00, 0x8D, 0x01, 0x10,   // timer A = 0: 1024 samples
            0xA9, 0x04, 0x8D, 0x00, 0x10, 0xA9, 0x01, 0x8D, 0x01, 0x10,   // start it
            0x58,                                                           // cli
            0x4C, 0x15, 0x90,                                               // jmp *
        ];
        snd[0x9000..0x9000 + code.len()].copy_from_slice(code);
        let irq: &[u8] = &[
            0xE6, 0x20,                                                     // inc $20
            0xA9, 0x80, 0x8D, 0x01, 0x10,                                   // IRQ reset (address is still 4)
            0x40,
        ];
        snd[0x9100..0x9100 + irq.len()].copy_from_slice(irq);
        snd[0xFFFC..0x10000].copy_from_slice(&[0x00, 0x90, 0x00, 0x91]);  // reset, IRQ
    });
    for _ in 0..30 { s.run_frame(0); }
    // 30 frames of 1/57.4 s against 41667 / 1024 = 40.7 Hz
    let n = s.board.latch_ram(0x20);
    assert!((19..=23).contains(&n), "about 21 timer IRQs ({n})");
}

/// A note on the YM3812 reaches the mixed output
#[test]
fn the_ym3812_is_heard() {
    let mut s = board();
    let regs = [(0x20u8, 0x21u8), (0x23, 0x21), (0x40, 0x3F), (0x43, 0), (0x60, 0xF0), (0x63, 0xF0),
                (0x80, 0x0F), (0x83, 0x0F), (0xA0, 0x41), (0xB0, 0x32)];
    for (r, v) in regs { s.board.ym2.write(0, r); s.board.ym2.write(1, v); }
    s.out.clear();
    for _ in 0..3 { s.run_frame(0); }
    let peak = s.out.iter().fold(0f32, |m, v| m.max(v.abs()));
    assert!(peak > 0.015, "one sine is 1/8 of full scale x 0.16: the OPL in the mix ({peak})");
}

#[test]
fn flip_screen_turns_the_picture_round() {
    let mut s = video_board();
    let (a, b) = (px(&s, 0, 34), px(&s, 9, 34));
    s.board.bac[0].ctrl[0] |= 0x80;
    assert_eq!(px(&s, SCREEN_H - 1, SCREEN_W - 1 - 34), a, "the text cell, upside down");
    assert_eq!(px(&s, SCREEN_H - 10, SCREEN_W - 1 - 34), b);
}

/// hiscore.dat rows use 68000 byte addresses (above 64K)
#[test]
fn the_score_table_is_reachable_for_hiscore_dat() {
    use crate::arcade::hiscore::{HiScore, HiScoreMem};
    let mut s = board();
    s.hs_write(0xFF9000, 0x12);
    s.hs_write(0xFF9001, 0x34);
    assert_eq!(s.board.ram[0x1000..0x1002], [0x12, 0x34], "main RAM, big-endian bytes");
    assert_eq!(s.hs_read(0xFF9001), 0x34);
    let h = HiScore::parse("robocop:\n@:maincpu,program,ff9000,40,12,00\n", "robocop").expect("parsed");
    assert_eq!(h.total_len(), 0x40);
}
