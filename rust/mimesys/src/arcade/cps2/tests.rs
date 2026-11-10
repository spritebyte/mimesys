use std::collections::HashMap;

use super::cps2_crypt::{decrypt, make_key_file, parse_key, Cipher, Key};
use super::cps2_system::Cps2System;
use super::cps2_video::{gfx_decode, unshuffle};
use crate::common::cpu::m68000::{M68000, M68kBus};
use crate::common::input as pad;
use crate::common::system::System;

// ------------------------------------------------------------------ decryption

/// The pseudo-random ROM the C reference harness used (MAME cps2crypt.cpp compiled
/// stand-alone, see testdata/crypt_vectors.txt).
fn lcg_rom() -> Vec<u8> {
    let mut s: u32 = 12345;
    let mut rom = Vec::with_capacity(0x80000);
    for _ in 0..0x40000 {
        s = s.wrapping_mul(1103515245).wrapping_add(12345);
        let w = (s >> 16) as u16;
        rom.push((w >> 8) as u8);
        rom.push(w as u8);
    }
    rom
}

#[test]
fn decryption_matches_mame() {
    let rom = lcg_rom();
    let keys = [[0x12345678u32, 0x9abcdef0], [0xdeadbeef, 0x01234567], [0x0f0f0f0f, 0xf0f0f0f0]];
    let uppers = [0x3FFFFu32, 0x1FFFF, 0x2FFFF];
    let outs: Vec<Vec<u8>> = (0..3).map(|k| decrypt(&rom, &Key { master: keys[k], lower: 0, upper: uppers[k] })).collect();
    let mut n = 0;
    for line in include_str!("testdata/crypt_vectors.txt").lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        let k: usize = f[0].parse().unwrap();
        let a = usize::from_str_radix(f[1], 16).unwrap();
        let raw = u16::from_str_radix(f[2], 16).unwrap();
        let dec = u16::from_str_radix(f[3], 16).unwrap();
        assert_eq!((rom[a * 2] as u16) << 8 | rom[a * 2 + 1] as u16, raw, "ROM word {a:05x}");
        let got = (outs[k][a * 2] as u16) << 8 | outs[k][a * 2 + 1] as u16;
        assert_eq!(got, dec, "key {k} word {a:05x}");
        n += 1;
    }
    assert_eq!(n, 195);
}

#[test]
fn key_file_unscrambles_like_mame() {
    let f0: Vec<u8> = (0..20).map(|i| (i * 37 + 11) as u8).collect();
    let f1: Vec<u8> = (0..20).map(|i| 0xA5 ^ (i * 91) as u8).collect();
    assert_eq!(parse_key(&f0), Some(Key { master: [0x4e940769, 0xb220df8d], lower: 0, upper: 0x17e000 }));
    assert_eq!(parse_key(&f1), Some(Key { master: [0x9b0dd6a0, 0x3fe9721c], lower: 0, upper: 0x2d8000 }));
    // and the test helper is its inverse
    let k = parse_key(&make_key_file([0x11223344, 0x55667788], 0)).unwrap();
    assert_eq!(k.master, [0x11223344, 0x55667788]);
    assert_eq!(k.upper, 0x800000, "range word 0: all 16 MB");
    // a dead board key: only FF0000-FFFFFF
    let k = parse_key(&make_key_file([1, 2], 0xFFFF)).unwrap();
    assert_eq!((k.lower, k.upper), (0x7F8000, 0x7FFFFF));
}

#[test]
fn encrypt_helper_is_the_inverse() {
    let c = Cipher::new([0xCAFEBABE, 0x12345678]);
    let mut rom = vec![0u8; 0x40];
    for a in 0..0x20u32 {
        let e = c.encrypt_word(a, 0x4E71 + a as u16);
        rom[a as usize * 2] = (e >> 8) as u8;
        rom[a as usize * 2 + 1] = e as u8;
    }
    let d = decrypt(&rom, &Key { master: [0xCAFEBABE, 0x12345678], lower: 0, upper: 0x7FFFFF });
    for a in 0..0x20usize { assert_eq!((d[a * 2] as u16) << 8 | d[a * 2 + 1] as u16, 0x4E71 + a as u16); }
}

// ------------------------------------------------------------------ 68000 program space

struct SplitBus { data: Vec<u8>, prog: Vec<u8> }
impl M68kBus for SplitBus {
    fn read_byte(&mut self, a: u32) -> u8 { self.data[(a & 0xFFFF) as usize] }
    fn read_word(&mut self, a: u32) -> u16 { let i = (a & 0xFFFE) as usize; (self.data[i] as u16) << 8 | self.data[i + 1] as u16 }
    fn write_byte(&mut self, a: u32, v: u8) { self.data[(a & 0xFFFF) as usize] = v; }
    fn write_word(&mut self, a: u32, v: u16) { let i = (a & 0xFFFE) as usize; self.data[i] = (v >> 8) as u8; self.data[i + 1] = v as u8; }
    fn read_program_word(&mut self, a: u32) -> u16 { let i = (a & 0xFFFE) as usize; (self.prog[i] as u16) << 8 | self.prog[i + 1] as u16 }
}

fn put(m: &mut [u8], a: usize, words: &[u16]) {
    for (k, w) in words.iter().enumerate() { m[a + 2 * k] = (w >> 8) as u8; m[a + 2 * k + 1] = *w as u8; }
}

#[test]
fn opcodes_and_pc_relative_reads_use_program_space_data_reads_dont() {
    let mut b = SplitBus { data: vec![0; 0x10000], prog: vec![0; 0x10000] };
    // reset vectors only in program space (MAME reads them through the opcode space)
    put(&mut b.prog, 0, &[0x0000, 0x8000, 0x0000, 0x0100]);
    put(&mut b.prog, 0x100, &[
        0x303A, 0x00FE,          // MOVE.W (d16,PC),D0   -> word at 0x200
        0x3238, 0x0200,          // MOVE.W ($0200).W,D1  -> data space
        0x4E71,
    ]);
    put(&mut b.prog, 0x200, &[0x1111]);
    put(&mut b.data, 0x200, &[0x2222]);
    put(&mut b.data, 0x100, &[0x4E72, 0x0000]);   // garbage if fetched from data space
    let mut cpu = M68000::new();
    cpu.reset(&mut b);
    assert_eq!(cpu.pc, 0x104, "PC from the program-space vectors");
    cpu.step(&mut b);
    cpu.step(&mut b);
    assert_eq!(cpu.r[0] & 0xFFFF, 0x1111, "PC-relative operand from program space");
    assert_eq!(cpu.r[1] & 0xFFFF, 0x2222, "absolute operand from data space");
}

// ------------------------------------------------------------------ graphics ROM order

#[test]
fn unshuffle_interleaves_halves_recursively() {
    let mut u: Vec<[u8; 8]> = (0..8u8).map(|i| [i; 8]).collect();
    unshuffle(&mut u);
    let order: Vec<u8> = u.iter().map(|x| x[0]).collect();
    assert_eq!(order, [0, 2, 4, 6, 1, 3, 5, 7]);
    // a whole 2 MB bank: still a permutation, and unit 1 comes from the middle
    let mut g: Vec<u8> = (0..0x200000u32).map(|i| (i / 8) as u8 ^ (i / 0x800) as u8).collect();
    let before = g.clone();
    gfx_decode(&mut g);
    let mut a = before.chunks(8).map(|c| c.to_vec()).collect::<Vec<_>>();
    let mut b2 = g.chunks(8).map(|c| c.to_vec()).collect::<Vec<_>>();
    a.sort(); b2.sort();
    assert_eq!(a, b2);
    assert_eq!(&g[..8], &before[..8]);
}

// ------------------------------------------------------------------ board

const MASTER: [u32; 2] = [0x0BADF00D, 0x600DCAFE];

/// A CPS2 set ("sfa": six-button layout) with a synthetic, properly encrypted program.
fn board_with(prog: &[(usize, Vec<u16>)]) -> Cps2System {
    let c = Cipher::new(MASTER);
    let mut plain = vec![0u8; 0x80000];
    put(&mut plain, 0, &[0x00FF, 0xF000, 0x0000, 0x0400]);
    put(&mut plain, 26 * 4, &[0x0000, 0x0600]);        // IRQ 2
    put(&mut plain, 28 * 4, &[0x0000, 0x0700]);        // IRQ 4
    put(&mut plain, 30 * 4, &[0x0000, 0x0700]);        // IRQ 6 (both)
    put(&mut plain, 0x600, &[0x5279, 0x00FF, 0x0000, 0x4E73]);   // ADDQ.W #1,$FF0000; RTE
    put(&mut plain, 0x700, &[0x5279, 0x00FF, 0x0002, 0x4E73]);   // ADDQ.W #1,$FF0002; RTE
    for (a, w) in prog { put(&mut plain, *a, w); }
    // encrypt what the CPU fetches in program space: the reset vectors and the code.
    // The other vectors are read as data, so they stay plain.
    let mut rom = plain.clone();
    for a in (0..4u32).chain(0x200..0x40000) {
        let w = (plain[a as usize * 2] as u16) << 8 | plain[a as usize * 2 + 1] as u16;
        let e = c.encrypt_word(a, w);
        rom[a as usize * 2] = (e >> 8) as u8;
        rom[a as usize * 2 + 1] = e as u8;
    }
    let mut r = HashMap::new();
    r.insert("maincpu".to_string(), rom);
    r.insert("key".to_string(), make_key_file(MASTER, 0));
    r.insert("gfx".to_string(), vec![0; 0x400000]);
    r.insert("audiocpu".to_string(), vec![0; 0x50000]);
    r.insert("qsound".to_string(), vec![0; 0x10000]);
    r.insert("dsp".to_string(), vec![1; 0x2000]);
    Cps2System::new("sfa", r).unwrap()
}

#[test]
fn encrypted_program_runs_and_takes_vblank_irq2() {
    let mut s = board_with(&[(0x400, vec![0x46FC, 0x2000, 0x60FE])]);   // MOVE #$2000,SR; BRA *
    for _ in 0..3 { s.run_frame(0); }
    assert_eq!(s.bus.ram_word(0xFF0000), 3, "one IRQ 2 a frame");
    assert_eq!(s.bus.ram_word(0xFF0002), 0, "no raster IRQ unless programmed");
}

#[test]
fn raster_counter_raises_irq4_on_the_programmed_line() {
    // CPS-B 804150 (counter 1 reload) = 100 with bit 15 = reload now
    let mut s = board_with(&[(0x400, vec![
        0x33FC, 0x8064, 0x0080, 0x4150,   // MOVE.W #$8064,$804150
        0x46FC, 0x2000, 0x60FE,
    ])]);
    for _ in 0..2 { s.run_frame(0); }
    let n = s.bus.ram_word(0xFF0002);
    assert!(n >= 1 && n <= 2, "counter 1 reaches 0 once a frame: {n}");
}

#[test]
fn inputs_eeprom_and_volume() {
    let mut s = board_with(&[(0x400, vec![0x60FE])]);
    s.set_input(pad::RIGHT | pad::Y | pad::B | pad::START | pad::SELECT);
    assert_eq!(s.bus.read_word(0x804000), 0xFFEE, "P1 right + light punch (button 1)");
    assert_eq!(s.bus.read_word(0x804010), 0xFFFE, "light kick = button 4, in IN1");
    let in2 = s.bus.read_word(0x804020);
    assert_eq!(in2 & 0xFF00, 0xFFFF & !0x0100 & !0x1000 & 0xFF00, "start 1 + coin 1");
    assert_eq!(in2 & 1, 1, "EEPROM DO idles high");
    // pad 2's kicks: B = short (IN1 bit 4), R = roundhouse (IN2 bit 14); Y = jab
    s.set_extra_input(pad::EXT_P2_Y | pad::EXT_P2_R);
    s.set_input(pad::B << pad::P2_SHIFT);
    assert_eq!(s.bus.read_word(0x804000), 0xEFFF, "P2 jab");
    assert_eq!(s.bus.read_word(0x804010), 0xFFEF, "P2 short kick");
    assert_eq!(s.bus.read_word(0x804020) & 0x4000, 0, "P2 roundhouse kick");
    s.set_extra_input(0);
    assert_eq!(s.bus.read_word(0x804030), 0xE021, "full volume, no add-on boards");
    assert!(s.has_battery());
    assert_eq!(s.get_sram().unwrap().len(), 128, "93C46 x16");
}

#[test]
fn object_ram_banks_and_z80_reset() {
    let mut s = board_with(&[(0x400, vec![0x60FE])]);
    s.bus.write_word(0x708000, 0x1234);         // the work bank
    s.bus.write_word(0x8040E0, 1);              // swap
    assert_eq!(s.bus.read_word(0x708000), 0, "708000 now shows the other bank");
    s.bus.write_word(0x8040E0, 0);
    assert_eq!(s.bus.read_word(0x70E000), 0x1234, "708000 is mirrored up to 70FFFF");
    s.bus.write_word(0x804040, 0x0000);        // Z80 held in reset
    assert!(s.bus.sound.in_reset);
    s.bus.write_word(0x804040, 0x0008);
    assert!(!s.bus.sound.in_reset);
    s.bus.write_word(0x618000, 0x00AB);
    assert_eq!(s.bus.read_word(0x618000), 0xFFAB, "shared RAM: low byte");
}

#[test]
fn save_state_round_trip() {
    let mut s = board_with(&[(0x400, vec![0x46FC, 0x2000, 0x60FE])]);
    s.run_frame(0);
    let st = s.save_state();
    s.run_frame(0);
    s.load_state(&st).unwrap();
    assert_eq!(s.bus.ram_word(0xFF0000), 1);
    s.run_frame(0);
    assert_eq!(s.bus.ram_word(0xFF0000), 2);
}

#[test]
fn missing_key_says_so() {
    let mut r = HashMap::new();
    r.insert("maincpu".to_string(), vec![0; 0x80000]);
    r.insert("gfx".to_string(), vec![0; 0x400000]);
    r.insert("audiocpu".to_string(), vec![0; 0x50000]);
    r.insert("qsound".to_string(), vec![0; 0x10000]);
    r.insert("dsp".to_string(), vec![1; 0x2000]);
    let e = Cps2System::new("sfa", r).err().unwrap();
    assert!(e.contains("sfa.key"), "{e}");
}

// ------------------------------------------------------------------ video

use super::cps2_video::Cps2Video;
use crate::arcade::cps1::cps1_config::config;
use crate::arcade::cps1::cps1_video::{Video, SCREEN_W, Y0};

fn video() -> Cps2Video {
    // tiles live 8 MB in (the "cps2" mapper); sprites at the start
    let mut v = Cps2Video::new(Video::new(vec![0; 0x900000], config("cps2").unwrap(), -1));
    v.base.cps_b_write(0x30 / 2, 0x3F, 0xFFFF);                // all palette pages
    let set = |v: &mut Cps2Video, byte: usize, w: u16| { v.base.gfxram[byte] = (w >> 8) as u8; v.base.gfxram[byte + 1] = w as u8; };
    set(&mut v, 0x14000 + 0x201 * 2, 0xFF00);                  // scroll 1 pen 1: red
    set(&mut v, 0x14000 + 0x002 * 2, 0xF0F0);                  // sprite pen 2: green
    v.base.cps_a_write(0x0A / 2, 0x9140, 0xFFFF);
    // tile 0 (every other cell) transparent; scroll 1 tile 1 at screen (0, 0) (tilemap
    // column 8, row 2): pen 1 on its left half, transparent on the right
    for i in 0..128 { v.base.gfx_mut()[0x20000 * 64 + i] = 0xFF; }
    let idx = 2 + (8 << 5);
    set(&mut v, idx * 4, 0x0001);
    set(&mut v, idx * 4 + 2, 0x0000);
    for y in 0..8 { for k in 0..8 { v.base.gfx_mut()[0x20001 * 64 + y * 8 + k] = if k == 0 { 0xFF } else if k < 4 { 0 } else { 0xFF }; } }
    // sprite tile 0x10: pen 2
    for i in 0..128 { if i % 4 == 1 { v.base.gfx_mut()[0x10 * 128 + i] = 0xFF; } }
    v.output[4] = 0x40;   // usual offsets
    v.output[5] = 0x10;
    v
}

fn sprite(v: &mut Cps2Video, pri: u16) {
    v.obj_write(0, 0, 64 | pri << 13, 0xFFFF);   // bank 0 = the one latched with the bank bit clear
    v.obj_write(0, 1, 16, 0xFFFF);
    v.obj_write(0, 2, 0x10, 0xFFFF);
    v.obj_write(0, 3, 0, 0xFFFF);
    v.obj_write(0, 5, 0x8000, 0xFFFF);           // end of table
}

fn px(v: &Cps2Video, x: usize, y: usize) -> [u8; 4] {
    let o = (y * SCREEN_W + x) * 4;
    v.base.framebuffer[o..o + 4].try_into().unwrap()
}

#[test]
fn sprite_priority_against_the_layer_priority_nibbles() {
    let mut v = video();
    // order: scroll 1, sprites, scroll 2, scroll 3; scroll 1 enabled
    v.base.cps_b_write(0x26 / 2, 0x02 | 1 << 6 | 0 << 8 | 2 << 10 | 3 << 12, 0xFFFF);
    v.output[2] = 0x0020;          // scroll 1's priority nibble = 2
    sprite(&mut v, 1);
    v.frame_start();
    v.vblank();                    // latch (draws a frame with the old table)
    v.frame_start();
    v.vblank();
    assert_eq!(px(&v, 0, 0), [255, 0, 0, 255], "priority 1 sprite behind a priority 2 layer");
    assert_eq!(px(&v, 8, 0), [0, 255, 0, 255], "shown where the layer is empty");
    sprite(&mut v, 3);
    v.frame_start(); v.vblank(); v.frame_start(); v.vblank();
    assert_eq!(px(&v, 0, 0), [0, 255, 0, 255], "priority 3 sprite in front");
    sprite(&mut v, 0);
    v.frame_start(); v.vblank(); v.frame_start(); v.vblank();
    assert_eq!(px(&v, 8, 0), [0, 0, 0, 255], "priority 0 sprites are never shown (MAME primasks[0])");
}

#[test]
fn later_table_entries_are_in_front() {
    let mut v = video();
    v.base.cps_b_write(0x26 / 2, 1 << 6 | 0 << 8 | 2 << 10 | 3 << 12, 0xFFFF);
    let set = |v: &mut Cps2Video, byte: usize, w: u16| { v.base.gfxram[byte] = (w >> 8) as u8; v.base.gfxram[byte + 1] = w as u8; };
    set(&mut v, 0x14000 + 0x013 * 2, 0xF00F);   // palette 1 pen 3: blue
    v.base.cps_a_write(0x0A / 2, 0x9140, 0xFFFF);
    for i in 0..128 { if i % 4 <= 1 { v.base.gfx_mut()[0x11 * 128 + i] = 0xFF; } }
    sprite(&mut v, 1);
    for (k, w) in [64 | 1 << 13, 16, 0x11, 1].iter().enumerate() { v.obj_write(0, 4 + k, *w, 0xFFFF); }
    v.obj_write(0, 9, 0x8000, 0xFFFF);
    v.frame_start(); v.vblank(); v.frame_start(); v.vblank();
    assert_eq!(px(&v, 0, 0), [0, 0, 255, 255], "sprite 1 drawn over sprite 0");
}

#[test]
fn lines_drawn_before_a_raster_split_keep_the_old_scroll() {
    let mut v = video();
    v.base.cps_b_write(0x26 / 2, 0x02 | 1 << 6 | 0 << 8 | 2 << 10 | 3 << 12, 0xFFFF);
    v.frame_start();
    v.render_to(Y0 + 4);                       // lines 0-3 with scroll 0
    v.base.cps_a_write(0x0C / 2, 0xFFF8, 0xFFFF); // then scroll 1 x = -8
    v.vblank();                                // the rest of the frame
    assert_eq!(px(&v, 0, 3), [255, 0, 0, 255], "line 3: old scroll");
    assert_eq!(px(&v, 0, 4), [0, 0, 0, 255], "line 4: moved right");
    assert_eq!(px(&v, 8, 4), [255, 0, 0, 255]);
}

#[test]
fn primasks_follow_mame() {
    use super::cps2_video::primasks;
    // layer priorities 3, 1, 2 (in draw order)
    assert_eq!(primasks([3, 1, 2]), [0xFF, 0xFE, 0xF2, 0x02, 0, 0, 0, 0]);
    // 1, 2, 3: nothing pruned
    assert_eq!(primasks([1, 2, 3]), [0xFF, 0xFE, 0xFC, 0xF0, 0, 0, 0, 0]);
    // 2, 2, 1: layer 2 above layer 3 prunes mask1
    assert_eq!(primasks([2, 2, 1]), [0xFF, 0xFE, 0xAA & !0xA0 | 0xCC & !0xC0, 0, 0, 0, 0, 0]);
}

#[test]
fn raster_counter_reloads_now_only_with_bit_15() {
    let mut v = video();
    v.base.raster_on = true;
    v.base.raster_counter[0] = 7;
    v.base.cps_b_write(0x10 / 2, 0x0064, 0xFFFF);
    assert_eq!(v.base.raster_counter[0], 7, "just the reload value");
    assert_eq!(v.base.raster_reload[0], 0x64);
    v.base.cps_b_write(0x10 / 2, 0x8050, 0xFFFF);
    assert_eq!(v.base.raster_counter[0], 0x50, "bit 15: reload now");
    assert_eq!(v.base.cps_b_read(0x10 / 2, 0, 0), 0x50, "counters read back");
}

