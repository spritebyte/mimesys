use crate::common::cpu::arm::arm7tdmi::{ArmBus, ACCESS_NSEQ, ACCESS_SEQ};
use crate::common::system::System;
use crate::gba::gba_bus::GbaBus;
use crate::gba::gba_ppu::CYCLES_PER_FRAME;
use crate::gba::gba_system::GbaSystem;

/// jsmolka's gba-tests (MIT, tests_data/LICENSE-gba-tests): each ROM runs its tests,
/// leaves the number of the first failure in r12 (0 = all passed) and idles.
fn run_jsmolka(rom: &[u8]) -> (u32, u32) {
    let mut sys = GbaSystem::from_rom(rom, "test.gba", &[]).unwrap();
    for _ in 0..60 { sys.run_frame(0); }
    // idle loop: "b idle" keeps the PC still
    let pc = sys.cpu.pc();
    sys.step();
    sys.step();
    assert_eq!(sys.cpu.pc(), pc, "not idling at the end (pc {pc:08x})");
    (sys.cpu.r[12], pc)
}

#[test]
fn jsmolka_arm() {
    let (fail, _) = run_jsmolka(include_bytes!("tests_data/arm.gba"));
    assert_eq!(fail, 0, "arm.gba: test {fail} failed");
}

#[test]
fn jsmolka_thumb() {
    let (fail, _) = run_jsmolka(include_bytes!("tests_data/thumb.gba"));
    assert_eq!(fail, 0, "thumb.gba: test {fail} failed");
}

#[test]
fn jsmolka_memory() {
    let (fail, _) = run_jsmolka(include_bytes!("tests_data/memory.gba"));
    assert_eq!(fail, 0, "memory.gba: test {fail} failed");
}

fn bus() -> GbaBus { GbaBus::new(vec![], vec![0; 0x1000]) }

#[test]
fn timer_overflow_irq_and_cascade() {
    let mut b = bus();
    // timer 0: reload FFF0, /1, IRQ; timer 1: count-up, reload FFFF, IRQ
    b.write16_raw(0x0400_0100, 0xFFF0);
    b.write16_raw(0x0400_0104, 0xFFFF);
    b.write16_raw(0x0400_0106, 0x00C4);
    b.write16_raw(0x0400_0102, 0x00C0);
    // counting starts 2 cycles after the enabling write
    b.cycles += 17;
    b.tick();
    assert_eq!(b.if_ & 0x18, 0, "not yet");
    assert_eq!(b.read16_raw(0x0400_0100), 0xFFFF);
    b.cycles += 1;
    b.tick();
    assert_eq!(b.if_ & 0x18, 0x18, "timer 0 overflowed, timer 1 counted it and overflowed too");
    assert_eq!(b.read16_raw(0x0400_0100), 0xFFF0, "reloaded");
}

#[test]
fn dma_immediate_copy_and_irq() {
    let mut b = bus();
    for i in 0..16u32 { b.write32_raw(0x0200_0000 + i * 4, 0x1111_1111u32.wrapping_mul(i + 1)); }
    b.write32_raw(0x0400_00D4, 0x0200_0000);       // DMA3 SAD
    b.write32_raw(0x0400_00D8, 0x0300_0100);       // DMA3 DAD
    b.write16_raw(0x0400_00DC, 16);                // count
    b.write16_raw(0x0400_00DE, 0x8000 | 0x4000 | 0x0400);   // enable, IRQ, 32-bit, now
    let before = b.cycles;
    b.tick();
    for i in 0..16u32 { assert_eq!(b.read32_raw(0x0300_0100 + i * 4), 0x1111_1111u32.wrapping_mul(i + 1)); }
    assert_eq!(b.read16_raw(0x0400_00DE) & 0x8000, 0, "enable bit cleared");
    assert_ne!(b.if_ & (1 << 11), 0, "DMA3 IRQ");
    assert!(b.cycles - before >= 2 + 16 * 7, "EWRAM 32-bit reads cost 6, IWRAM writes 1");
}

#[test]
fn hblank_dma_repeats_each_visible_line() {
    let mut b = bus();
    b.write16_raw(0x0200_0000, 0x1234);
    b.write32_raw(0x0400_00B0, 0x0200_0000);       // DMA0 from EWRAM (fixed)
    b.write32_raw(0x0400_00B4, 0x0300_0000);       // to IWRAM (increment)
    b.write16_raw(0x0400_00B8, 1);
    b.write16_raw(0x0400_00BA, 0x8000 | 0x2000 | 0x0200 | 0x0100);   // enable, HBlank, repeat, src fixed
    for _ in 0..CYCLES_PER_FRAME / 64 { b.cycles += 64; b.tick(); }
    for i in 0..160u32 { assert_eq!(b.read16_raw(0x0300_0000 + i * 2), 0x1234, "line {i}"); }
    assert_eq!(b.read16_raw(0x0300_0000 + 160 * 2), 0, "no HBlank DMA during VBlank");
    assert_ne!(b.read16_raw(0x0400_00BA) & 0x8000, 0, "repeat keeps it enabled");
}

#[test]
fn rom_wait_states() {
    let mut b = bus();
    let c0 = b.cycles;
    b.read16(0x0800_0000, ACCESS_NSEQ);
    assert_eq!(b.cycles - c0, 5, "WS0 N default 4 waits");
    let c0 = b.cycles;
    b.read16(0x0800_0002, ACCESS_SEQ);
    assert_eq!(b.cycles - c0, 3, "WS0 S default 2 waits");
    b.write16_raw(0x0400_0204, 0x4317);    // the usual game setting: WS0 3/1
    let c0 = b.cycles;
    b.read32(0x0800_0004, ACCESS_NSEQ);
    assert_eq!(b.cycles - c0, 4 + 2, "32-bit = N + S");
}

#[test]
fn vblank_irq_and_frame_length() {
    // a tiny ROM: enable VBlank IRQ in DISPSTAT and IE, then spin
    let mut rom = vec![0u8; 0x200];
    let prog: [u32; 6] = [
        0xE3A00301, // mov r0, #0x04000000
        0xE3A01008, // mov r1, #8          (DISPSTAT VBlank IRQ enable)
        0xE1C010B4, // strh r1, [r0, #4]
        0xE3A01001, // mov r1, #1
        0xE5801200, // str r1, [r0, #0x200] (IE = VBlank)
        0xEAFFFFFE, // b .
    ];
    for (i, w) in prog.iter().enumerate() { rom[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes()); }
    let mut sys = GbaSystem::from_rom(&rom, "t.gba", &[]).unwrap();
    sys.run_frame(0);
    let c = sys.bus.cycles;
    sys.run_frame(0);
    // a frame ends inside an instruction, so it can run a few cycles over
    let len = (sys.bus.cycles - c) as i64;
    assert!((len - CYCLES_PER_FRAME as i64).abs() < 16, "frame length {len}");
    assert_ne!(sys.bus.if_ & 1, 0, "VBlank flagged in IF");
}

#[test]
fn bios_reads_protected_outside_the_bios() {
    let mut bios = vec![0u8; 0x4000];
    for (i, b) in bios.iter_mut().enumerate() { *b = i as u8; }
    let mut b = GbaBus::new(bios, vec![0; 0x100]);
    // fetching from the BIOS: direct reads work
    b.read32(0x0000_0010, crate::common::cpu::arm::arm7tdmi::ACCESS_CODE);
    assert_eq!(b.read32(0x0000_0100, ACCESS_NSEQ), 0x0302_0100);
    // now running from the cart: BIOS reads give the last opcode fetched from it
    b.read32(0x0800_0000, crate::common::cpu::arm::arm7tdmi::ACCESS_CODE);
    assert_eq!(b.read32(0x0000_0100, ACCESS_NSEQ), 0x1312_1110);
}

/// With the real BIOS: boots through the BIOS intro, then jsmolka's bios.gba checks the
/// BIOS functions (SWIs). GBA_BIOS=<path to gba_bios.bin> cargo test --release --lib gba_real_bios -- --ignored
#[test]
#[ignore]
fn gba_real_bios() {
    let bios = std::fs::read(std::env::var("GBA_BIOS").expect("GBA_BIOS=<gba_bios.bin>")).unwrap();
    let mut sys = GbaSystem::from_rom(include_bytes!("tests_data/bios.gba"), "bios.gba", &bios).unwrap();
    // the intro takes about 4 seconds, then the cart runs
    let mut entered_cart = false;
    for _ in 0..600 {
        sys.run_frame(0);
        if sys.cpu.pc() >= 0x0800_0000 { entered_cart = true; }
    }
    assert!(entered_cart, "the BIOS never started the cartridge (pc {:08x})", sys.cpu.pc());
    let pc = sys.cpu.pc();
    sys.step();
    sys.step();
    assert_eq!(sys.cpu.pc(), pc, "not idling at the end");
    assert_eq!(sys.cpu.r[12], 0, "bios.gba: test {} failed", sys.cpu.r[12]);
}

#[test]
fn save_state_round_trip() {
    let mut sys = GbaSystem::from_rom(include_bytes!("tests_data/thumb.gba"), "t.gba", &[]).unwrap();
    for _ in 0..2 { sys.run_frame(0); }
    let saved = sys.save_state();
    for _ in 0..3 { sys.run_frame(0); }
    let (regs, cycles, iwram) = (sys.cpu.r, sys.bus.cycles, sys.bus.iwram.clone());
    sys.load_state(&saved).unwrap();
    for _ in 0..3 { sys.run_frame(0); }
    assert_eq!(sys.cpu.r, regs);
    assert_eq!(sys.bus.cycles, cycles);
    assert!(sys.bus.iwram == iwram);
}

// ---------------------------------------------------------------- PPU (stage 3)

use crate::gba::gba_ppu::{Ppu, SCREEN_W};

/// Runs a ROM for `frames` and returns the framebuffer (RGBA).
fn frame_of(rom: &[u8], frames: usize) -> Vec<u8> {
    let mut sys = GbaSystem::from_rom(rom, "t.gba", &[]).unwrap();
    for _ in 0..frames { sys.run_frame(0); }
    sys.framebuffer().to_vec()
}

/// RENDER_DIR=<dir> cargo test --lib gba_dump_frames -- --ignored  → PPM screenshots
#[test]
#[ignore]
fn gba_dump_frames() {
    let dir = std::env::var("RENDER_DIR").expect("RENDER_DIR");
    let roms: [(&str, &[u8]); 6] = [
        ("hello", include_bytes!("tests_data/hello.gba")),
        ("shades", include_bytes!("tests_data/shades.gba")),
        ("stripes", include_bytes!("tests_data/stripes.gba")),
        ("arm", include_bytes!("tests_data/arm.gba")),
        ("thumb", include_bytes!("tests_data/thumb.gba")),
        ("memory", include_bytes!("tests_data/memory.gba")),
    ];
    for (name, rom) in roms {
        let fb = frame_of(rom, 60);
        let mut ppm = format!("P6 240 160 255\n").into_bytes();
        for p in fb.chunks(4) { ppm.extend_from_slice(&p[..3]); }
        std::fs::write(format!("{dir}/{name}.ppm"), ppm).unwrap();
    }
}

fn px(p: &Ppu, x: usize, y: usize) -> [u8; 3] {
    let i = (y * SCREEN_W + x) * 4;
    [p.framebuffer[i], p.framebuffer[i + 1], p.framebuffer[i + 2]]
}
fn w16(v: &mut [u8], a: usize, x: u16) { v[a..a + 2].copy_from_slice(&x.to_le_bytes()); }
fn reg(p: &mut Ppu, off: u32, x: u16) { p.write8(off, x as u8); p.write8(off + 1, (x >> 8) as u8); }
const RED: u16 = 0x001F;
const GREEN: u16 = 0x03E0;
const BLUE: u16 = 0x7C00;

#[test]
fn mode3_direct_colour_and_forced_blank() {
    let mut p = Ppu::new();
    reg(&mut p, 0, 0x0403);                         // mode 3, BG2
    reg(&mut p, 0x20, 0x100); reg(&mut p, 0x26, 0x100);   // identity transform
    w16(&mut p.vram, (5 * 240 + 7) * 2, RED);
    p.latch_ref(0);
    for y in 0..=5 { p.render_line(y); }           // the reference point steps each line
    assert_eq!(px(&p, 7, 5), [255, 0, 0]);
    assert_eq!(px(&p, 8, 5), [0, 0, 0]);
    reg(&mut p, 0, 0x0483);                         // forced blank = white
    p.latch_ref(0);
    for y in 0..=5 { p.render_line(y); }
    assert_eq!(px(&p, 7, 5), [255, 255, 255]);
}

#[test]
fn mode4_page_flip() {
    let mut p = Ppu::new();
    reg(&mut p, 0x20, 0x100); reg(&mut p, 0x26, 0x100);
    w16(&mut p.palette, 2, GREEN);
    w16(&mut p.palette, 4, BLUE);
    p.vram[0] = 1;
    p.vram[0xA000] = 2;
    reg(&mut p, 0, 0x0404);
    p.latch_ref(0);
    p.render_line(0);
    assert_eq!(px(&p, 0, 0), [0, 255, 0]);
    reg(&mut p, 0, 0x0414);                         // page 1
    p.latch_ref(0);
    p.render_line(0);
    assert_eq!(px(&p, 0, 0), [0, 0, 255]);
}

/// BG0: 4bpp tile 1 filled with colour 1 (palette bank 2), map at block 8, scrolled.
fn text_bg_setup(p: &mut Ppu) {
    for i in 0..32 { p.vram[32 + i] = 0x11; }       // tile 1, all pixels colour 1
    w16(&mut p.vram, 0x4000 + (1 * 32 + 2) * 2, 0x2001);  // map (2,1) = tile 1, bank 2
    w16(&mut p.palette, (2 * 16 + 1) * 2, RED);
}

#[test]
fn text_bg_tiles_scroll_and_priority_against_sprites() {
    let mut p = Ppu::new();
    text_bg_setup(&mut p);
    reg(&mut p, 8, 8 << 8 | 1);                     // BG0: map block 8, priority 1
    reg(&mut p, 0x10, 4);                           // HOFS 4
    reg(&mut p, 0, 0x0100);                         // mode 0, BG0
    p.render_line(8);
    assert_eq!(px(&p, 12, 8), [255, 0, 0], "tile column 2 starts at 16 - 4");
    assert_eq!(px(&p, 11, 8), [0, 0, 0]);
    assert_eq!(px(&p, 19, 8), [255, 0, 0]);
    assert_eq!(px(&p, 20, 8), [0, 0, 0]);

    // an 8x8 sprite at (12, 8): tile 0 of OBJ VRAM, colour 1 = green
    for i in 0..32 { p.vram[0x10000 + i] = 0x11; }
    w16(&mut p.palette, 0x200 + 2, GREEN);
    w16(&mut p.oam, 0, 8);                          // y 8
    w16(&mut p.oam, 2, 14);                         // x 14
    w16(&mut p.oam, 4, 2 << 10);                    // priority 2: behind BG0
    for n in 1..128 { w16(&mut p.oam, n * 8, 0x200); }   // hide the rest
    reg(&mut p, 0, 0x1140);                         // + OBJ, 1D
    p.render_line(8);
    assert_eq!(px(&p, 14, 8), [255, 0, 0], "BG0 (prio 1) in front of the sprite (prio 2)");
    assert_eq!(px(&p, 21, 8), [0, 255, 0], "sprite shows where BG0 is clear");
    w16(&mut p.oam, 4, 1 << 10);                    // same priority: the sprite wins
    p.render_line(8);
    assert_eq!(px(&p, 14, 8), [0, 255, 0]);
}

#[test]
fn alpha_blend_fade_and_window() {
    let mut p = Ppu::new();
    text_bg_setup(&mut p);
    reg(&mut p, 8, 8 << 8);
    reg(&mut p, 0, 0x0100);
    w16(&mut p.palette, 0, BLUE);                   // backdrop
    reg(&mut p, 0x50, 0x0001 | (1 << 6) | (0x20 << 8));  // alpha: BG0 over backdrop
    reg(&mut p, 0x52, 0x0808);                      // 8/16 + 8/16
    p.render_line(8);
    let c = px(&p, 16, 8);
    assert!(c[0] > 100 && c[0] < 140 && c[2] > 100 && c[2] < 140 && c[1] == 0, "half red half blue: {c:?}");
    assert_eq!(px(&p, 0, 8), [0, 0, 255], "backdrop not a first target");

    reg(&mut p, 0x50, 0x0001 | (3 << 6));           // darken BG0 fully
    reg(&mut p, 0x54, 16);
    p.render_line(8);
    assert_eq!(px(&p, 16, 8), [0, 0, 0]);

    // window 0 covering x 0..18: BG0 off and no effects inside; outside: BG0 + effects
    reg(&mut p, 0x40, 18);                          // x1 0, x2 18
    reg(&mut p, 0x44, 160);                         // y 0..160
    reg(&mut p, 0x48, 0x0000);
    reg(&mut p, 0x4A, 0x0021);
    reg(&mut p, 0, 0x2100);
    p.render_line(8);
    assert_eq!(px(&p, 17, 8), [0, 0, 255], "inside win0: BG0 hidden");
    assert_eq!(px(&p, 18, 8), [0, 0, 0], "outside: BG0, darkened");
}

#[test]
fn affine_bg_scales_by_two() {
    let mut p = Ppu::new();
    // BG2 128x128 map at block 8, tile 1 = 8bpp colour 3 at map (1,0)
    for i in 0..64 { p.vram[64 + i] = 3; }
    p.vram[0x4000 + 1] = 1;
    w16(&mut p.palette, 6, RED);
    reg(&mut p, 0x0C, 8 << 8);
    reg(&mut p, 0x20, 0x80);                        // pa = 0.5: zoom x2
    reg(&mut p, 0x26, 0x80);
    reg(&mut p, 0, 0x0401);                         // mode 1, BG2
    p.latch_ref(0);
    p.render_line(0);
    assert_eq!(px(&p, 15, 0), [0, 0, 0]);
    assert_eq!(px(&p, 16, 0), [255, 0, 0], "texel 8 is at screen x 16");
    assert_eq!(px(&p, 31, 0), [255, 0, 0]);
    assert_eq!(px(&p, 32, 0), [0, 0, 0]);
}

#[test]
fn jsmolka_hello_text_on_screen() {
    let fb = frame_of(include_bytes!("tests_data/hello.gba"), 10);
    let white = |x: usize, y: usize| fb[(y * 240 + x) * 4..][..3] == [255, 255, 255];
    let in_text = (72..168).flat_map(|x| (76..84).map(move |y| (x, y))).filter(|&(x, y)| white(x, y)).count();
    let outside = (0..240).flat_map(|x| (0..70).map(move |y| (x, y))).filter(|&(x, y)| white(x, y)).count();
    assert!(in_text > 100, "text drawn ({in_text} white pixels)");
    assert_eq!(outside, 0);
}

/// One 8x8 4bpp sprite (OBJ tile 0, colour 1) in OAM slot 0; the rest hidden.
fn sprite_setup(p: &mut Ppu, a0: u16, a1: u16, a2: u16) {
    for i in 0..32 { p.vram[0x10000 + i] = 0x11; }
    w16(&mut p.palette, 0x200 + 2, GREEN);
    for n in 1..128 { w16(&mut p.oam, n * 8, 0x200); }
    w16(&mut p.oam, 0, a0);
    w16(&mut p.oam, 2, a1);
    w16(&mut p.oam, 4, a2);
}

#[test]
fn affine_double_size_sprite() {
    let mut p = Ppu::new();
    // affine + double size: 16x16 box at (0,0); matrix 0 scales x2 → the 8x8 tile fills it
    sprite_setup(&mut p, 0x0300, 0, 0);
    w16(&mut p.oam, 6, 0x80);    // pa 0.5
    w16(&mut p.oam, 30, 0x80);   // pd 0.5
    reg(&mut p, 0, 0x1040);
    p.render_line(0);
    assert_eq!(px(&p, 0, 0), [0, 255, 0]);
    assert_eq!(px(&p, 15, 0), [0, 255, 0]);
    assert_eq!(px(&p, 16, 0), [0, 0, 0]);
    // identity matrix in a double-size box: the 8x8 sits in the middle
    w16(&mut p.oam, 6, 0x100);
    w16(&mut p.oam, 30, 0x100);
    p.render_line(0);
    assert_eq!(px(&p, 3, 0), [0, 0, 0], "row 0 of the box is above the sprite");
    for y in 0..4 { p.render_line(y); }
    assert_eq!(px(&p, 3, 3), [0, 0, 0]);
    p.render_line(4);
    assert_eq!(px(&p, 4, 4), [0, 255, 0]);
    assert_eq!(px(&p, 11, 4), [0, 255, 0]);
    assert_eq!(px(&p, 12, 4), [0, 0, 0]);
}

#[test]
fn semi_transparent_sprite_and_obj_window() {
    let mut p = Ppu::new();
    w16(&mut p.palette, 0, BLUE);                   // backdrop
    sprite_setup(&mut p, 0x0400, 0, 0);             // semi-transparent at (0,0)
    reg(&mut p, 0x50, 0x2000);                      // 2nd target: backdrop (no mode set)
    reg(&mut p, 0x52, 0x0808);
    reg(&mut p, 0, 0x1040);
    p.render_line(0);
    let c = px(&p, 0, 0);
    assert!(c[1] > 100 && c[1] < 140 && c[2] > 100 && c[2] < 140, "half green half blue: {c:?}");

    // OBJ window: the sprite's shape hides the backdrop effects; outside, fade to white
    w16(&mut p.oam, 0, 0x0800);                     // mode 2 = OBJ window
    reg(&mut p, 0x4A, 0x0020 | (0x10 << 8));        // outside: effects; inside obj window: OBJ only
    reg(&mut p, 0x50, 0x0020 | (2 << 6));           // brighten the backdrop
    reg(&mut p, 0x54, 16);
    reg(&mut p, 0, 0x9040);
    p.render_line(0);
    assert_eq!(px(&p, 0, 0), [0, 0, 255], "inside the OBJ window: plain backdrop");
    assert_eq!(px(&p, 8, 0), [255, 255, 255], "outside: brightened to white");
}

/// cargo test --release --lib gba_speed -- --ignored --nocapture
#[test]
#[ignore]
fn gba_speed() {
    let mut p = Ppu::new();
    // worst-ish case: mode 0, all 4 BGs + 128 64x64 sprites + windows + blending
    reg(&mut p, 0, 0xFF40);
    for n in 0..128 { w16(&mut p.oam, n * 8, 0xC000 | 0x0000); w16(&mut p.oam, n * 8 + 2, 0xC000 | (n as u16 % 200)); }
    for i in 0..0x18000 { p.vram[i] = i as u8 | 1; }
    reg(&mut p, 0x50, 0x3F7F);
    let t = std::time::Instant::now();
    for _ in 0..60 { for y in 0..160 { p.render_line(y); } }
    println!("60 heavy frames: {:?}", t.elapsed());
}

// ---------------------------------------------------------------- stage 4: saves, clock, sound, prefetch

use crate::gba::gba_cart::{civil_from_unix, now_civil, SaveType};

#[test]
fn jsmolka_save_chips() {
    for (name, rom, t) in [
        ("sram", &include_bytes!("tests_data/sram.gba")[..], SaveType::Sram),
        ("flash64", &include_bytes!("tests_data/flash64.gba")[..], SaveType::Flash64),
        ("flash128", &include_bytes!("tests_data/flash128.gba")[..], SaveType::Flash128),
        ("none", &include_bytes!("tests_data/none.gba")[..], SaveType::Auto),
    ] {
        let mut sys = GbaSystem::from_rom(rom, "t.gba", &[]).unwrap();
        assert_eq!(sys.bus.cart.save_type, t, "{name}: detected type");
        for _ in 0..60 { sys.run_frame(0); }
        assert_eq!(sys.cpu.r[12], 0, "{name}.gba: test {} failed", sys.cpu.r[12]);
    }
}

#[test]
fn jsmolka_nes_series_quirks() {
    let (fail, _) = run_jsmolka(include_bytes!("tests_data/nes.gba"));
    assert_eq!(fail, 0, "nes.gba: test {fail} failed");
}

/// A 1 MB ROM with a Classic NES Series game code.
fn nes_series_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x10_0000];
    rom[0xAC..0xB0].copy_from_slice(b"FDKE");
    rom[0x10] = 0xAB;
    rom
}

#[test]
fn classic_nes_series_mirrors_rom_and_uses_eeprom() {
    let mut b = GbaBus::new(vec![], nes_series_rom());
    assert_eq!(b.cart.save_type, SaveType::Eeprom512);
    assert_eq!(b.read8_raw(0x0810_0010), 0xAB, "1 MB mirror");
    assert_eq!(b.read8_raw(0x0870_0010), 0xAB);
    // a normal cart reads the address bus past the end of the ROM
    let mut plain = GbaBus::new(vec![], vec![0; 0x10_0000]);
    assert_eq!(plain.read16_raw(0x0810_0010), (0x0810_0010u32 >> 1) as u16);
}

fn eeprom_send(b: &mut GbaBus, bits: &[u8]) {
    for &x in bits { b.write16_raw(0x0D00_0000, x as u16); }
}
fn bits_of(v: u64, n: u32) -> Vec<u8> { (0..n).rev().map(|i| (v >> i & 1) as u8).collect() }

#[test]
fn eeprom_write_then_read() {
    let mut b = GbaBus::new(vec![], nes_series_rom());
    let data = 0x0123_4567_89AB_CDEFu64;
    // write: 1 0, 6 address bits, 64 data bits, end bit
    let mut cmd = vec![1, 0];
    cmd.extend(bits_of(5, 6));
    cmd.extend(bits_of(data, 64));
    cmd.push(0);
    eeprom_send(&mut b, &cmd);
    assert_eq!(b.read16_raw(0x0D00_0000) & 1, 0, "busy while programming");
    b.cycles += 200_000;
    assert_eq!(b.read16_raw(0x0D00_0000) & 1, 1, "ready");
    assert_eq!(&b.cart.save[40..48], &data.to_be_bytes());
    assert!(b.cart.dirty);
    // read: 1 1, address, end bit; then 4 dummy bits and 64 data bits
    let mut cmd = vec![1, 1];
    cmd.extend(bits_of(5, 6));
    cmd.push(0);
    eeprom_send(&mut b, &cmd);
    let got: Vec<u16> = (0..68).map(|_| b.read16_raw(0x0D00_0000) & 1).collect();
    let v = got[4..].iter().fold(0u64, |a, &x| a << 1 | x as u64);
    assert_eq!(v, data);
}

#[test]
fn eeprom_size_from_dma_length() {
    let mut rom = vec![0u8; 0x1000];
    rom[0x200..0x208].copy_from_slice(b"EEPROM_V");
    let mut b = GbaBus::new(vec![], rom);
    assert_eq!(b.cart.save_type, SaveType::EepromAuto);
    // DMA 3: 17 halfwords to the EEPROM = a read request with a 14-bit address
    b.write32_raw(0x0400_00D4, 0x0200_0000);
    b.write32_raw(0x0400_00D8, 0x0D00_0000);
    b.write16_raw(0x0400_00DC, 17);
    b.write16_raw(0x0400_00DE, 0x8000);
    b.tick();
    assert_eq!(b.cart.save_type, SaveType::Eeprom8k);
    assert_eq!(b.cart.save.len(), 0x2000);
}

#[test]
fn rtc_reads_the_host_date() {
    let mut rom = vec![0u8; 0x1000];
    rom[0x300..0x308].copy_from_slice(b"SIIRTC_V");
    let mut b = GbaBus::new(vec![], rom);
    assert!(b.cart.has_rtc);
    let port = |b: &mut GbaBus, v: u16| b.write16_raw(0x0800_00C4, v);
    b.write16_raw(0x0800_00C8, 1);           // GPIO readable
    b.write16_raw(0x0800_00C6, 7);           // SCK SIO CS out
    port(&mut b, 1);                          // CS low, SCK high
    port(&mut b, 5);                          // CS high: start
    // command 0x65 = read date/time, sent LSB first
    for i in 0..8 {
        let bit = (0x65u16 >> i & 1) << 1;
        port(&mut b, 4 | bit);
        port(&mut b, 5 | bit);
    }
    b.write16_raw(0x0800_00C6, 5);           // SIO in
    let mut bytes = [0u8; 7];
    for byte in bytes.iter_mut() {
        for i in 0..8 {
            port(&mut b, 4);
            port(&mut b, 5);
            *byte |= ((b.read16_raw(0x0800_00C4) >> 1 & 1) as u8) << i;
        }
    }
    let t = now_civil(0);
    let bcd = |v: u32| ((v / 10) * 16 + v % 10) as u8;
    assert_eq!(bytes[0], bcd(t.year % 100));
    assert_eq!(bytes[1], bcd(t.month));
    assert_eq!(bytes[2], bcd(t.day));
    assert_eq!(bytes[3], bcd(t.weekday));
}

#[test]
fn civil_date_conversion() {
    let c = civil_from_unix(1_709_210_096);   // 2024-02-29 12:34:56 UTC, a Thursday
    assert_eq!((c.year, c.month, c.day, c.weekday, c.hour, c.min, c.sec), (2024, 2, 29, 4, 12, 34, 56));
}

#[test]
fn square_wave_reaches_the_output() {
    let mut b = bus();
    b.write16_raw(0x0400_0084, 0x80);        // master on
    b.write16_raw(0x0400_0080, 0xFF77);      // PSG full volume, all channels both sides
    b.write16_raw(0x0400_0082, 0x0002);      // PSG 100%
    b.write16_raw(0x0400_0062, 0xF080);      // duty 50%, envelope volume 15
    b.write16_raw(0x0400_0064, 0x8000 | 1750);  // trigger, ~440 Hz
    b.cycles += CYCLES_PER_FRAME as u64;
    b.tick();
    let s = b.apu.drain();
    assert!((1460..=1480).contains(&s.len()), "735 stereo samples a frame, got {}", s.len() / 2);
    let max = s.iter().cloned().fold(0.0f32, f32::max);
    let min = s.iter().cloned().fold(0.0f32, f32::min);
    assert!(max > 0.1 && min < -0.1, "square swings both ways: {min}..{max}");
    assert_eq!(b.read16_raw(0x0400_0084) & 0x8F, 0x81, "channel 1 reports on");
}

#[test]
fn fifo_dma_refills_from_timer_overflows() {
    let mut b = bus();
    for i in 0..256u32 { b.write8_raw(0x0200_0000 + i, (i as u8).wrapping_mul(7) | 1); }
    b.write16_raw(0x0400_0084, 0x80);
    b.write16_raw(0x0400_0082, 0x0304 | 0x0800);       // FIFO A 100%, both sides, timer 0, reset
    b.write32_raw(0x0400_00BC, 0x0200_0000);           // DMA1 source
    b.write32_raw(0x0400_00C0, 0x0400_00A0);           // to FIFO A
    b.write16_raw(0x0400_00C6, 0x8000 | 0x3000 | 0x0400 | 0x0200);   // enable, special, 32-bit, repeat
    b.write16_raw(0x0400_0100, 0x10000u32.wrapping_sub(512) as u16); // 32768 Hz
    b.write16_raw(0x0400_0102, 0x0080);
    for _ in 0..200 { b.cycles += 512; b.tick(); }
    let src = b.dma.ch[1].src;
    assert!(src > 0x0200_0000 + 100, "DMA kept refilling (source at {src:08x})");
    assert_eq!(b.dma.ch[1].dst, 0x0400_00A0, "destination stays on the FIFO");
    assert!(b.apu.drain().iter().any(|&x| x.abs() > 0.05), "samples reach the output");
}

#[test]
fn prefetch_buffer_speeds_up_rom_code() {
    let prog: [u32; 4] = [
        0xE3A03B01, // mov r3, #0x400
        0xE0020190, // loop: mul r2, r0, r1
        0xE2533001, // subs r3, r3, #1
        0x1AFFFFFC, // bne loop
    ];
    let run = |waitcnt: u16| {
        let mut rom = vec![0u8; 0x200];
        for (i, w) in prog.iter().enumerate() { rom[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes()); }
        rom[16..20].copy_from_slice(&0xEAFF_FFFEu32.to_le_bytes());   // b .
        let mut sys = GbaSystem::from_rom(&rom, "t.gba", &[]).unwrap();
        sys.bus.write16_raw(0x0400_0204, waitcnt);
        let c0 = sys.bus.cycles;
        sys.step();                                   // mov r3
        while sys.cpu.r[3] != 0 && sys.bus.cycles - c0 < 1_000_000 { sys.step(); }
        sys.bus.cycles - c0
    };
    let (plain, fast) = (run(0x0317), run(0x4317));
    println!("prefetch {fast} vs none {plain}");
    assert!(fast * 10 < plain * 9, "prefetch {fast} vs none {plain}");
}

/// cargo test --release --lib gba_frame_speed -- --ignored --nocapture
#[test]
#[ignore]
fn gba_frame_speed() {
    let mut sys = GbaSystem::from_rom(include_bytes!("tests_data/hello.gba"), "t.gba", &[]).unwrap();
    sys.bus.write16_raw(0x0400_0084, 0x80);
    sys.bus.write16_raw(0x0400_0062, 0xF080);
    sys.bus.write16_raw(0x0400_0064, 0x8000 | 1750);
    let t = std::time::Instant::now();
    for _ in 0..600 { sys.run_frame(0); sys.drain_audio(); }
    println!("600 frames (10 s of game time): {:?}", t.elapsed());
}

/// mGBA's test suite (github.com/mgba-emu/suite), built locally; prints each suite's
/// score through the mGBA debug port. Not shipped: it has to be compiled.
/// MGBA_SUITE=<suite.gba> [GBA_BIOS=<gba_bios.bin>] cargo test --release --lib gba_mgba_suite -- --ignored --nocapture
#[test]
#[ignore]
fn gba_mgba_suite() {
    use crate::common::input as pad;
    let rom = std::fs::read(std::env::var("MGBA_SUITE").expect("MGBA_SUITE")).unwrap();
    let only: Option<usize> = std::env::var("SUITE_ONLY").ok().and_then(|v| v.parse().ok());
    let verbose = std::env::var("SUITE_VERBOSE").is_ok();
    // optional GBA_BIOS=<gba_bios.bin>: the real BIOS (intro skipped)
    let bios = std::env::var("GBA_BIOS").ok().map(|p| std::fs::read(p).unwrap()).unwrap_or_default();
    let mut sys = GbaSystem::from_rom(&rom, "suite.gba", &bios).unwrap();
    if !bios.is_empty() { sys.skip_bios = true; sys.reset(); }
    for _ in 0..30 { sys.run_frame(0); }
    let tap = |sys: &mut GbaSystem, k: u32| { for _ in 0..3 { sys.run_frame(k); } for _ in 0..3 { sys.run_frame(0); } };
    // scores reached so far (built-in open BIOS); a lower score is a regression.
    // The rest mostly needs the real BIOS (its code timing and protection) or is 1-cycle detail.
    let floor: [u32; 13] = [1512, 130, 1722, 847, 80, 140, 93, 72, 603, 1232, 90, 0, 4];
    let mut low = Vec::new();
    for idx in 0..14 {
        if idx > 0 { tap(&mut sys, pad::DOWN); }
        if only.is_some_and(|o| o != idx) { continue; }
        sys.bus.debug_log.clear();
        tap(&mut sys, pad::A);
        let mut frames = 0;
        while !sys.bus.debug_log.iter().any(|l| l.starts_with("END:")) && frames < 3_000 {
            sys.run_frame(0);
            frames += 1;
        }
        let log = std::mem::take(&mut sys.bus.debug_log);
        let name = log.iter().find(|l| l.starts_with("BEGIN:")).cloned().unwrap_or_else(|| format!("suite {idx}: no BEGIN"));
        let end = log.iter().find(|l| l.starts_with("END:")).cloned().unwrap_or_else(|| format!("TIMEOUT after {frames} frames"));
        println!("{idx:2} {name:40} {end}");
        if end.starts_with("TIMEOUT") {
            let b = &sys.bus;
            println!("LOG| stuck: pc {:08x} cpsr {:08x} halted {} ie {:04x} if {:04x} ime {} tm0 {:04x}/{:04x} irq_line {}",
                sys.cpu.pc(), sys.cpu.cpsr, b.halted, b.ie, b.if_, b.ime,
                b.timers.ch[0].control, b.timers.ch[0].reload, b.irq_line());
        }
        if verbose { for l in &log { println!("LOG| {l}"); } }
        if idx < 13 && only.is_none() {
            let got = end.strip_prefix("END: ").and_then(|e| e.split('/').next()).and_then(|n| n.trim().parse::<u32>().ok());
            if got.is_none_or(|g| g < floor[idx]) { low.push(format!("{name}: {end} (floor {})", floor[idx])); }
        }
        tap(&mut sys, pad::B);
    }
    assert!(low.is_empty(), "scores dropped:\n{}", low.join("\n"));
}

// ---------------------------------------------------------------- polish: I/O, serial, renderer

#[test]
fn io_reads_follow_hardware() {
    let mut b = bus();
    b.write16_raw(0x0400_0008, 0xFFFF);
    assert_eq!(b.read16_raw(0x0400_0008), 0xDFFF, "BG0CNT has no wraparound bit");
    b.write16_raw(0x0400_0048, 0xFFFF);
    assert_eq!(b.read16_raw(0x0400_0048), 0x3F3F, "WININ");
    b.write16_raw(0x0400_0136, 0xFFFF);
    assert_eq!(b.read16_raw(0x0400_0136), 0, "unused high register reads 0");
    b.write16_raw(0x0400_00DC, 0x1234);
    assert_eq!(b.read16_raw(0x0400_00DC), 0, "DMA word count is write-only (reads 0)");
}

#[test]
fn serial_transfer_with_nothing_connected() {
    let mut b = bus();
    b.write16_raw(0x0400_0200, 1 << 7);                 // IE serial
    b.write16_raw(0x0400_0208, 1);
    b.write16_raw(0x0400_012A, 0x0012);
    b.write16_raw(0x0400_0128, 0x4083);                 // normal 8-bit, 2 MHz, IRQ, start
    b.cycles += 63;
    b.tick();
    assert_ne!(b.read16_raw(0x0400_0128) & 0x80, 0, "still sending (8 bits x 8 clocks)");
    b.cycles += 1;
    b.tick();
    assert_eq!(b.read16_raw(0x0400_0128) & 0x80, 0, "done");
    assert_eq!(b.read16_raw(0x0400_012A) & 0xFF, 0xFF, "received all ones");
    assert_ne!(b.if_ & (1 << 7), 0, "serial IRQ");
    // multiplayer with no partner never finishes
    b.write16_raw(0x0400_0128, 0x2080);
    b.cycles += 100_000;
    b.tick();
    assert_ne!(b.read16_raw(0x0400_0128) & 0x80, 0);
}

#[test]
fn sprite_line_budget() {
    let mut p = Ppu::new();
    for i in 0..32 { p.vram[0x10000 + i] = 0x11; }
    w16(&mut p.palette, 0x200 + 2, GREEN);
    for n in 0..128 { w16(&mut p.oam, n * 8, 0x200); }
    // 18 off-screen 64x64 sprites use 18 x 64 = 1152 of the 1210 clocks
    for n in 0..18 {
        w16(&mut p.oam, n * 8, 0x0000);
        w16(&mut p.oam, n * 8 + 2, 0xC000 | 300);
    }
    w16(&mut p.oam, 18 * 8, 0x0000);                    // 8x8 at x 0: costs 8, fits
    w16(&mut p.oam, 18 * 8 + 2, 0);
    w16(&mut p.oam, 19 * 8, 0x0000);                    // 64x64 at x 100: no clocks left
    w16(&mut p.oam, 19 * 8 + 2, 0xC000 | 100);
    reg(&mut p, 0, 0x1040);
    p.render_line(0);
    assert_eq!(px(&p, 0, 0), [0, 255, 0], "sprite 18 drawn");
    assert_eq!(px(&p, 100, 0), [0, 0, 0], "sprite 19 dropped");
    reg(&mut p, 0, 0x1060);                             // H-blank interval free: 954 clocks
    p.render_line(0);
    assert_eq!(px(&p, 0, 0), [0, 0, 0], "now even sprite 18 is out");
}

#[test]
fn affine_bg_vertical_mosaic() {
    let mut p = Ppu::new();
    // 8bpp tile 1: row 0 colour 3 (red), other rows colour 5 (blue)
    for i in 0..64 { p.vram[64 + i] = if i < 8 { 3 } else { 5 }; }
    p.vram[0x4000] = 1;
    w16(&mut p.palette, 6, RED);
    w16(&mut p.palette, 10, BLUE);
    reg(&mut p, 0x0C, 8 << 8 | 0x40);                   // BG2: map block 8, mosaic
    reg(&mut p, 0x4C, 3 << 4);                          // vertical mosaic 4
    reg(&mut p, 0x20, 0x100); reg(&mut p, 0x26, 0x100);
    reg(&mut p, 0, 0x0401);
    p.latch_ref(0);
    for y in 0..3 { p.render_line(y); }
    assert_eq!(px(&p, 0, 2), [255, 0, 0], "line 2 repeats line 0");
    reg(&mut p, 0x4C, 0);
    p.latch_ref(0);
    for y in 0..3 { p.render_line(y); }
    assert_eq!(px(&p, 0, 2), [0, 0, 255], "no mosaic: row 2 of the tile");
}

// ---------------------------------------------------------------- link cable

use crate::gba::gba_link::run_linked;

/// A GBA idling in "b ." with its link port set up by the test.
fn idle_gba(name: &str) -> GbaSystem {
    let mut rom = vec![0u8; 0x200];
    rom[0..4].copy_from_slice(&0xEAFF_FFFEu32.to_le_bytes());
    GbaSystem::from_rom(&rom, name, &[]).unwrap()
}

fn sio_pair() -> (GbaSystem, GbaSystem) {
    let mut a = idle_gba("a.gba");
    let mut b = idle_gba("b.gba");
    for (s, parent) in [(&mut a, true), (&mut b, false)] {
        s.bus.sio.linked = true;
        s.bus.sio.parent = parent;
        s.bus.write16_raw(0x0400_0134, 0);              // RCNT: SIO modes
    }
    (a, b)
}

#[test]
fn link_multiplayer_exchange() {
    let (mut a, mut b) = sio_pair();
    for s in [&mut a, &mut b] { s.bus.write16_raw(0x0400_0128, 0x6003); }   // multi, IRQ, 115200
    a.bus.write16_raw(0x0400_012A, 0x1111);
    b.bus.write16_raw(0x0400_012A, 0x2222);
    b.bus.write16_raw(0x0400_0128, 0x6083);             // a child can't start
    assert_eq!(b.bus.read16_raw(0x0400_0128) & 0x80, 0);
    a.bus.write16_raw(0x0400_0128, 0x6083);             // parent starts
    run_linked(&mut a, &mut b, 20_000);
    for (s, who) in [(&mut a, "parent"), (&mut b, "child")] {
        assert_eq!(s.bus.read16_raw(0x0400_0120), 0x1111, "{who}: SIOMULTI0");
        assert_eq!(s.bus.read16_raw(0x0400_0122), 0x2222, "{who}: SIOMULTI1");
        assert_eq!(s.bus.read16_raw(0x0400_0124), 0xFFFF, "{who}: SIOMULTI2");
        assert_eq!(s.bus.read16_raw(0x0400_0128) & 0x80, 0, "{who}: done");
        assert_ne!(s.bus.if_ & 0x80, 0, "{who}: serial IRQ");
    }
    let (ca, cb) = (a.bus.read16_raw(0x0400_0128), b.bus.read16_raw(0x0400_0128));
    assert_eq!((ca >> 2) & 1, 0, "parent's SI is low");
    assert_eq!((cb >> 2) & 1, 1, "child's SI is high");
    assert_eq!(((ca >> 4) & 3, (cb >> 4) & 3), (0, 1), "player IDs");
    assert_ne!(ca & 8, 0, "all ready");
}

#[test]
fn link_normal32_exchange_and_partner_not_ready() {
    let (mut a, mut b) = sio_pair();
    for (s, v) in [(&mut a, 0xAABB_CCDDu32), (&mut b, 0x1122_3344)] {
        s.bus.write32_raw(0x0400_0120, v);
    }
    b.bus.write16_raw(0x0400_0128, 0x5080);             // 32-bit, external clock, ready, IRQ
    a.bus.write16_raw(0x0400_0128, 0x5083);             // 32-bit, internal 2 MHz, start, IRQ
    run_linked(&mut a, &mut b, 2_000);
    assert_eq!(a.bus.read32_raw(0x0400_0120), 0x1122_3344);
    assert_eq!(b.bus.read32_raw(0x0400_0120), 0xAABB_CCDD);
    assert!(a.bus.if_ & 0x80 != 0 && b.bus.if_ & 0x80 != 0);
    // B not armed this time: A reads the idle line, B is untouched
    b.bus.write16_raw(0x0400_0128, 0x5000);
    b.bus.write32_raw(0x0400_0120, 0x5555_5555);
    a.bus.write16_raw(0x0400_0128, 0x5083);
    run_linked(&mut a, &mut b, 2_000);
    assert_eq!(a.bus.read32_raw(0x0400_0120), 0xFFFF_FFFF);
    assert_eq!(b.bus.read32_raw(0x0400_0120), 0x5555_5555);
}

#[test]
fn link_multiplayer_over_the_network() {
    use crate::gameboy::gb_netlink::NetLink;
    use std::time::{Duration, Instant};
    let host = NetLink::host(0).unwrap();
    let guest = NetLink::join("127.0.0.1", host.port()).unwrap();
    let mut a = idle_gba("a.gba");
    let mut b = idle_gba("b.gba");
    assert!(a.attach_net(Some(host)) && b.attach_net(Some(guest)));
    assert!(a.bus.sio.parent && !b.bus.sio.parent, "the host is the parent");
    let start = Instant::now();
    while !(a.cable_status().starts_with("Connected") && b.cable_status().starts_with("Connected")) {
        assert!(start.elapsed() < Duration::from_secs(3), "{} / {}", a.cable_status(), b.cable_status());
        a.run_frame(0);
        b.run_frame(0);
    }
    for (s, v) in [(&mut a, 0xA1A1u16), (&mut b, 0xB2B2)] {
        s.bus.write16_raw(0x0400_0134, 0);
        s.bus.write16_raw(0x0400_0128, 0x6003);
        s.bus.write16_raw(0x0400_012A, v);
    }
    a.bus.write16_raw(0x0400_0128, 0x6083);
    // each side in its own thread, like two computers
    let tb = std::thread::spawn(move || {
        for _ in 0..30 { b.run_frame(0); }
        b
    });
    for _ in 0..30 { a.run_frame(0); }
    let b = tb.join().unwrap();
    for (s, who) in [(&a, "host"), (&b, "guest")] {
        assert_eq!(s.bus.sio.read8(0x120) as u16 | (s.bus.sio.read8(0x121) as u16) << 8, 0xA1A1, "{who}");
        assert_eq!(s.bus.sio.read8(0x122) as u16 | (s.bus.sio.read8(0x123) as u16) << 8, 0xB2B2, "{who}");
    }
}

#[test]
fn link_all_ready_follows_the_partner_and_trace_records() {
    let (mut a, mut b) = sio_pair();
    a.set_option("link_trace", 1);
    a.bus.write16_raw(0x0400_0128, 0x6003);             // A in multiplayer, B still normal
    run_linked(&mut a, &mut b, 1_000);
    assert_eq!(a.bus.read16_raw(0x0400_0128) & 8, 0, "B isn't in multiplayer: not all ready");
    b.bus.write16_raw(0x0400_0128, 0x6003);
    run_linked(&mut a, &mut b, 1_000);
    assert_ne!(a.bus.read16_raw(0x0400_0128) & 8, 0, "now all ready");
    assert_ne!(b.bus.read16_raw(0x0400_0128) & 8, 0);
    a.bus.write16_raw(0x0400_0128, 0x6083);
    run_linked(&mut a, &mut b, 20_000);
    let log = a.take_trace().join("\n");
    assert!(log.contains("W 128 = 608B"), "{log}");     // start + all ready
    assert!(log.contains("out: kind 2"), "{log}");
}

/// Two copies of one game enter multiplayer at the same clock and check "all ready" a
/// few instructions later (Super Mario Advance does this): both must see it set.
#[test]
fn link_same_clock_multiplayer_entry_sees_all_ready() {
    let prog: [u32; 13] = [
        0xE3A00301, // mov r0, #0x04000000
        0xE2800C01, // add r0, r0, #0x100
        0xE3A04000, // mov r4, #0
        0xE1C043B4, // strh r4, [r0, #0x34]   RCNT = 0
        0xE3A01A02, // mov r1, #0x2000
        0xE1C012B8, // strh r1, [r0, #0x28]   SIOCNT = multiplayer
        0xE1A00000, 0xE1A00000, 0xE1A00000,   // nop x3
        0xE1D022B8, // ldrh r2, [r0, #0x28]
        0xE3A03403, // mov r3, #0x03000000
        0xE5832000, // str r2, [r3]
        0xEAFFFFFE, // b .
    ];
    let mut rom = vec![0u8; 0x200];
    for (i, w) in prog.iter().enumerate() { rom[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes()); }
    let mut a = GbaSystem::from_rom(&rom, "a.gba", &[]).unwrap();
    let mut b = GbaSystem::from_rom(&rom, "b.gba", &[]).unwrap();
    for (s, parent) in [(&mut a, true), (&mut b, false)] { s.bus.sio.linked = true; s.bus.sio.parent = parent; }
    run_linked(&mut a, &mut b, 5_000);
    let (ca, cb) = (a.bus.read16_raw(0x0300_0000), b.bus.read16_raw(0x0300_0000));
    assert_ne!(ca & 8, 0, "left GBA saw all ready ({ca:04X})");
    assert_ne!(cb & 8, 0, "right GBA saw all ready ({cb:04X})");
    assert_eq!((ca & 4, cb & 4), (0, 4), "parent / child");
}

/// cargo test --release --lib gba_link_pair_speed -- --ignored --nocapture
#[test]
#[ignore]
fn gba_link_pair_speed() {
    use crate::gba::gba_link::GbaLinkPair;
    let rom = include_bytes!("tests_data/hello.gba");
    let mut pair = GbaLinkPair::new(GbaSystem::from_rom(rom, "a", &[]).unwrap(), GbaSystem::from_rom(rom, "b", &[]).unwrap());
    let t = std::time::Instant::now();
    for _ in 0..600 { pair.run_frame(0); pair.drain_audio(); }
    println!("pair: 600 frames (10 s) in {:?}", t.elapsed());
}

#[test]
fn link_rcnt_shows_the_wires() {
    let (mut a, mut b) = sio_pair();
    for s in [&mut a, &mut b] {
        s.bus.write16_raw(0x0400_0134, 0);              // RCNT written 0 (as SMA does)
        s.bus.write16_raw(0x0400_0128, 0x6003);
    }
    run_linked(&mut a, &mut b, 1_000);
    let (ra, rb) = (a.bus.read16_raw(0x0400_0134), b.bus.read16_raw(0x0400_0134));
    assert_eq!(ra & 0xF, 0b1011, "parent: SC, SD (all ready), SI low, SO ({ra:04X})");
    assert_eq!(rb & 0xF, 0b1111, "child: SI high ({rb:04X})");
    b.bus.write16_raw(0x0400_0128, 0x0000);             // B leaves multiplayer
    run_linked(&mut a, &mut b, 1_000);
    assert_eq!(a.bus.read16_raw(0x0400_0134) & 2, 0, "SD drops");
}
