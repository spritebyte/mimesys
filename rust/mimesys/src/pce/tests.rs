use super::huc6280::{Huc6280, PceBus, IRQ1, TIMER};
use super::pce_bus::{HuCard, PceBusImpl};
use super::pce_system::{PceSystem, OUT_W};
use super::psg::Psg;
use super::vce::Vce;
use super::vdc::{Vdc, MAX_WIDTH};
use crate::common::input as pad;
use crate::common::system::System;

const NO_KEY: u32 = 0x0F << pad::KEYPAD_SHIFT;

// ---------------- CPU on a flat 2 MB bus ----------------

struct Flat { mem: Vec<u8>, irq: u8 }
impl PceBus for Flat {
    fn read(&mut self, p: u32) -> u8 { self.mem[p as usize] }
    fn write(&mut self, p: u32, v: u8) { self.mem[p as usize] = v; }
    fn irq_pending(&mut self) -> u8 { self.irq }
}

/// Runs `prog` placed at logical 0xE000 (bank 0, MPR7 = 0), RAM bank F8 at 0x2000.
fn run(prog: &[u8], steps: usize) -> (Huc6280, Flat) {
    let mut bus = Flat { mem: vec![0; 0x200000], irq: 0 };
    bus.mem[..prog.len()].copy_from_slice(prog);
    bus.mem[0x1FFE] = 0x00; bus.mem[0x1FFF] = 0xE0;
    let mut cpu = Huc6280::new();
    cpu.reset(&mut bus);
    cpu.mpr[1] = 0xF8;
    for _ in 0..steps { cpu.step(&mut bus); }
    (cpu, bus)
}
const RAM: usize = 0xF8 << 13;

#[test]
fn zero_page_is_at_2000_through_mpr1() {
    let (_, b) = run(&[0xA9, 0x42, 0x85, 0x10, 0x8D, 0x34, 0x22], 3);   // LDA #42 ; STA $10 ; STA $2234
    assert_eq!(b.mem[RAM + 0x10], 0x42);
    assert_eq!(b.mem[RAM + 0x234], 0x42);
}

#[test]
fn tam_tma_and_register_swaps() {
    let prog = [0xA9, 0x55, 0x53, 0x0C,     // LDA #55 ; TAM #$0C -> MPR2, MPR3
                0xA9, 0x00, 0x43, 0x08,     // LDA #0 ; TMA #$08 -> A = MPR3
                0xA2, 0x11, 0xA0, 0x22,     // LDX #11 ; LDY #22
                0x02, 0x22];                // SXY ; SAX
    let (c, _) = run(&prog, 8);
    assert_eq!(c.mpr[2], 0x55);
    assert_eq!(c.mpr[3], 0x55);
    assert_eq!((c.a, c.x, c.y), (0x22, 0x55, 0x11));
}

#[test]
fn t_flag_makes_alu_ops_work_on_zero_page_x() {
    let prog = [0xA9, 0x30, 0x85, 0x05,     // zp[5] = $30
                0xA2, 0x05, 0xA9, 0x77,     // X = 5, A = $77
                0xF4, 0x09, 0x0F];          // SET ; ORA #$0F
    let (c, b) = run(&prog, 6);
    assert_eq!(b.mem[RAM + 5], 0x3F);
    assert_eq!(c.a, 0x77, "A untouched");
}

#[test]
fn block_transfers() {
    let mut prog = vec![0x73, 0x00, 0xE1, 0x00, 0x30, 0x04, 0x00];   // TII $E100 -> $3000, 4 bytes
    prog.extend([0xF3, 0x00, 0xE1, 0x10, 0x30, 0x04, 0x00]);        // TAI: source alternates
    let mut rom = vec![0u8; 0x200];
    rom[..prog.len()].copy_from_slice(&prog);
    rom[0x100..0x104].copy_from_slice(&[1, 2, 3, 4]);
    let mut bus = Flat { mem: vec![0; 0x200000], irq: 0 };
    bus.mem[..rom.len()].copy_from_slice(&rom);
    bus.mem[0x1FFE] = 0; bus.mem[0x1FFF] = 0xE0;
    let mut cpu = Huc6280::new();
    cpu.reset(&mut bus);
    cpu.mpr[1] = 0xF8;
    let c1 = cpu.step(&mut bus);
    assert_eq!(c1, (17 + 6 * 4) * 4, "slow mode counts 4x");
    cpu.step(&mut bus);
    assert_eq!(&bus.mem[RAM + 0x1000..RAM + 0x1004], &[1, 2, 3, 4]);
    assert_eq!(&bus.mem[RAM + 0x1010..RAM + 0x1014], &[1, 2, 1, 2]);
}

#[test]
fn bit_ops_rmb_smb_bbr_bbs_tst() {
    let prog = [0xA9, 0xFF, 0x85, 0x20,     // zp[20] = FF
                0x37, 0x20,                 // RMB3 $20 -> F7
                0x3F, 0x20, 0x02,           // BBR3 $20, +2 (taken: skips the next LDA)
                0xA9, 0x99,
                0xB7, 0x20,                 // SMB3 $20 -> FF
                0x83, 0x08, 0x20];          // TST #$08, $20 -> Z clear
    let (c, b) = run(&prog, 6);
    assert_eq!(b.mem[RAM + 0x20], 0xFF);
    assert_eq!(c.a, 0xFF, "BBR3 jumped over LDA #$99");
    assert_eq!(c.p & 0x02, 0);
}

#[test]
fn decimal_mode() {
    let prog = [0xF8, 0x18, 0xA9, 0x19, 0x69, 0x03, 0x85, 0x00,   // 19 + 3 = 22
                0x38, 0xA9, 0x22, 0xE9, 0x05, 0x85, 0x01];        // 22 - 5 = 17
    let (_, b) = run(&prog, 9);
    assert_eq!(&b.mem[RAM..RAM + 2], &[0x22, 0x17]);
}

#[test]
fn st0_st1_st2_hit_the_vdc_ports() {
    let (_, b) = run(&[0x03, 0x05, 0x13, 0x88, 0x23, 0x0C], 3);
    assert_eq!(b.mem[0x1FE000], 0x05);
    assert_eq!(b.mem[0x1FE002], 0x88);
    assert_eq!(b.mem[0x1FE003], 0x0C);
}

#[test]
fn jsr_rts_bsr_and_stack_page() {
    let mut rom = vec![0xEAu8; 0x40];
    rom[..8].copy_from_slice(&[0x20, 0x20, 0xE0, 0x44, 0x1B, 0xA9, 0x77, 0xEA]);  // JSR $E020 ; BSR $E020 ; LDA #77
    rom[0x20..0x23].copy_from_slice(&[0xE8, 0x60, 0xEA]);                       // INX ; RTS
    let mut bus = Flat { mem: vec![0; 0x200000], irq: 0 };
    bus.mem[..rom.len()].copy_from_slice(&rom);
    bus.mem[0x1FFE] = 0; bus.mem[0x1FFF] = 0xE0;
    let mut cpu = Huc6280::new();
    cpu.reset(&mut bus);
    cpu.mpr[1] = 0xF8;
    for _ in 0..7 { cpu.step(&mut bus); }
    assert_eq!(cpu.x, 2, "subroutine ran twice");
    assert_eq!(cpu.a, 0x77);
    assert_eq!(bus.mem[RAM + 0x1FF], 0xE0, "return address pushed to the stack page at 0x2100");
}

#[test]
fn interrupts_use_their_vectors_and_priority() {
    let mut bus = Flat { mem: vec![0; 0x200000], irq: 0 };
    bus.mem[0..2].copy_from_slice(&[0x58, 0xEA]);                         // CLI ; NOP
    bus.mem[0x1FFE] = 0; bus.mem[0x1FFF] = 0xE0;
    bus.mem[0x1FFA] = 0x00; bus.mem[0x1FFB] = 0xF0;                      // TIMER -> F000
    bus.mem[0x1FF8] = 0x00; bus.mem[0x1FF9] = 0xF1;                      // IRQ1 -> F100
    let mut cpu = Huc6280::new();
    cpu.reset(&mut bus);
    cpu.mpr[1] = 0xF8;
    cpu.step(&mut bus);
    bus.irq = IRQ1 | TIMER;
    let c = cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0xF000, "timer has priority over the VDC");
    assert_eq!(c, 8 * 4);
    assert_ne!(cpu.p & 0x04, 0, "I set");
}

// ---------------- VDC / VCE ----------------

fn vdc_reg(v: &mut Vdc, r: u8, val: u16) { v.write(0, r); v.write(2, val as u8); v.write(3, (val >> 8) as u8); }

#[test]
fn vram_ports_increment_and_read_back() {
    let mut v = Vdc::new();
    vdc_reg(&mut v, 0x05, 1 << 11);        // increment 32
    vdc_reg(&mut v, 0x00, 0x100);
    vdc_reg(&mut v, 0x02, 0xBEEF);
    vdc_reg(&mut v, 0x02, 0x1234);
    assert_eq!(v.vram(0x100), 0xBEEF);
    assert_eq!(v.vram(0x120), 0x1234);
    vdc_reg(&mut v, 0x01, 0x100);          // read address
    v.write(0, 0x02);
    assert_eq!((v.read(2), v.read(3)), (0xEF, 0xBE));
    assert_eq!((v.read(2), v.read(3)), (0x34, 0x12));
}

#[test]
fn background_tile_and_scroll() {
    let mut v = Vdc::new();
    vdc_reg(&mut v, 0x05, 0x0080);         // BG on
    vdc_reg(&mut v, 0x09, 0x0000);         // 32x32 map
    // map entry (row 0, col 1) = tile 0x100, palette 3
    v.set_vram(1, 0x3100);
    // tile 0x100 row 0: leftmost pixel = color 5 (planes 0 and 2)
    v.set_vram(0x1000, 0x0080);
    v.set_vram(0x1008, 0x0080);
    let mut line = [0u16; MAX_WIDTH];
    v.render_line(0, &mut line);
    assert_eq!(line[8], 3 * 16 + 5);
    assert_eq!(line[9], 0, "transparent -> backdrop");
    vdc_reg(&mut v, 0x07, 3);              // scroll right by 3: pixel moves left
    v.render_line(0, &mut line);
    assert_eq!(line[5], 3 * 16 + 5);
}

#[test]
fn sprites_size_flip_priority_and_collision() {
    let mut v = Vdc::new();
    vdc_reg(&mut v, 0x05, 0x00C1);         // BG + sprites on, collision IRQ
    // sprite pattern cell 2 (word 0x80): plane 0 row 0 = leftmost pixel -> color 1
    v.set_vram(0x80, 0x8000);
    // SAT in VRAM at 0x7F00 -> DMA'd at vblank
    vdc_reg(&mut v, 0x0F, 0x0001);         // SATB-done interrupt on
    vdc_reg(&mut v, 0x13, 0x7F00);
    let sat = [64 + 10, 32 + 50, 2 << 1, 0x0085,       // sprite 0: line 10, x 50, palette 5, in front
               64 + 10, 32 + 50, 2 << 1, 0x0806];      // sprite 1: same spot, X flipped, palette 6
    for (i, w) in sat.iter().enumerate() { v.set_vram(0x7F00 + i as u16, *w); }
    v.set_vram(0x7F08, 0);                             // other sprites off-screen (y=0)
    v.start_vblank();
    assert_ne!(v.status & 0x08, 0, "SATB DMA done");
    let mut line = [0u16; MAX_WIDTH];
    v.render_line(0, &mut line);
    for n in 1..=10 { v.render_line(n, &mut line); }
    assert_eq!(line[50], 0x100 + 5 * 16 + 1, "sprite 0 on top");
    assert_eq!(line[65], 0x100 + 6 * 16 + 1, "sprite 1 flipped: pixel at the right edge");
    assert_eq!(v.status & 0x01, 0, "no overlap of opaque pixels yet");
}

#[test]
fn raster_and_vblank_interrupts() {
    let mut v = Vdc::new();
    vdc_reg(&mut v, 0x06, 64 + 20);
    vdc_reg(&mut v, 0x05, 0x000C);         // raster + vblank IRQs
    v.check_raster(19);
    assert!(!v.irq_line());
    v.check_raster(20);
    assert!(v.irq_line());
    assert_eq!(v.read(0) & 0x04, 0x04);
    assert!(!v.irq_line(), "status read acknowledges");
    v.start_vblank();
    assert!(v.irq_line());
}

#[test]
fn vram_to_vram_dma() {
    let mut v = Vdc::new();
    for i in 0..4 { v.set_vram(0x200 + i, 0x1110 + i); }
    vdc_reg(&mut v, 0x0F, 0x0002);         // DMA-done IRQ
    vdc_reg(&mut v, 0x10, 0x200);
    vdc_reg(&mut v, 0x11, 0x300);
    vdc_reg(&mut v, 0x12, 3);              // 4 words, starts now
    assert_eq!(v.vram(0x303), 0x1113);
    assert!(v.irq_line());
}

#[test]
fn vce_palette_and_colors() {
    let mut c = Vce::new();
    c.write(2, 0x05); c.write(3, 0x01);    // entry 0x105
    c.write(4, 0x38); c.write(5, 0x00);    // red = 7
    c.write(4, 0xC7); c.write(5, 0x01);    // entry 0x106: green 7, blue 7
    assert_eq!(c.rgb(0x105), [255, 0, 0]);
    assert_eq!(c.rgb(0x106), [0, 255, 255]);
}

#[test]
fn psg_tone_frequency() {
    let mut p = Psg::new(44_100);
    p.write(0, 0);
    p.write(1, 0xFF);
    p.write(4, 0x40); p.write(4, 0x00);    // reset the wave write index
    for i in 0..32 { p.write(6, if i < 16 { 31 } else { 0 }); }
    p.write(2, 0x00); p.write(3, 0x01);    // freq 0x100 -> 3579545 / (32 * 256) = 437 Hz
    p.write(5, 0xFF);
    p.write(4, 0x9F);                      // on, full volume
    p.run(7_159_090);                      // one second
    let left: Vec<f32> = p.out.chunks(2).map(|c| c[0]).collect();
    let crossings = left.windows(2).filter(|w| (w[0] >= 0.0) != (w[1] >= 0.0)).count();
    let hz = crossings as f32 / 2.0;
    assert!((425.0..450.0).contains(&hz), "{hz} Hz");
    assert!((44_000..44_200).contains(&left.len()));
}

// ---------------- HuCard / bus ----------------

#[test]
fn hucard_layouts() {
    // 384 KB: 40-7F see the last 128 KB
    let mut rom = vec![0u8; 0x60000];
    for b in 0..0x30 { rom[b * 0x2000] = b as u8; }
    rom[0x1FFF] = 0xE0;
    let (card, us) = HuCard::new(rom).unwrap();
    assert!(!us);
    let mut bus = PceBusImpl::new(card, false, 44_100);
    assert_eq!(bus.read(0x21 << 13), 0x01, "banks 00-3F mirror the first 256 KB");
    assert_eq!(bus.read(0x40 << 13), 0x20);
    assert_eq!(bus.read(0x4F << 13), 0x2F);

    // Street Fighter II mapper
    let mut rom = vec![0u8; 0x280000];
    for b in 0..0x140 { rom[b * 0x2000] = (b >> 4) as u8; }
    rom[0x1FFF] = 0xE0;
    let (card, _) = HuCard::new(rom).unwrap();
    let mut bus = PceBusImpl::new(card, false, 44_100);
    assert_eq!(bus.read(0x40 << 13), 0x04);
    bus.write(0x1FF2, 0);
    assert_eq!(bus.read(0x40 << 13), 0x0C, "bank 0x40 now shows ROM bank 0xC0");
}

#[test]
fn us_hucards_are_bit_reversed_back() {
    let mut rom = vec![0u8; 0x2000];
    rom[0x1FFE] = 0x00; rom[0x1FFF] = 0xE0;
    rom[0] = 0x01;
    for b in rom.iter_mut() { *b = b.reverse_bits(); }
    let (card, us) = HuCard::new(rom).unwrap();
    assert!(us);
    let mut bus = PceBusImpl::new(card, us, 44_100);
    assert_eq!(bus.read(0), 0x01);
    assert_eq!(bus.read(0x1FF000) & 0x40, 0x40, "TurboGrafx region bit");
}

#[test]
fn joypad_select_and_timer_irq() {
    let mut rom = vec![0u8; 0x2000];
    rom[0x1FFF] = 0xE0;
    let (card, _) = HuCard::new(rom).unwrap();
    let mut bus = PceBusImpl::new(card, false, 44_100);
    bus.pad = 0x01 | 0x80;                 // up + RUN
    bus.write(0x1FF000, 0x01);             // SEL = 1: directions
    assert_eq!(bus.read(0x1FF000) & 0x0F, 0x0E);
    bus.write(0x1FF000, 0x00);             // SEL = 0: buttons
    assert_eq!(bus.read(0x1FF000) & 0x0F, 0x07);
    // timer: reload 1, start -> IRQ after 2 x 1024 cycles
    bus.write(0x1FEC00, 1);
    bus.write(0x1FEC01, 1);
    bus.tick(1024);
    assert_eq!(bus.irq_pending() & TIMER, 0);
    bus.tick(1024);
    assert_eq!(bus.irq_pending() & TIMER, TIMER);
    bus.write(0x1FF402, TIMER);            // masked
    assert_eq!(bus.irq_pending(), 0);
    bus.write(0x1FF403, 0);                // acknowledge
    bus.write(0x1FF402, 0);
    assert_eq!(bus.irq_pending(), 0);
}

// ---------------- whole system ----------------

/// Sets the backdrop green, turns on the VDC's vblank interrupt and counts frames at $2000.
fn test_card() -> Vec<u8> {
    let mut rom = vec![0xEAu8; 0x2000];
    let main = [
        0x78, 0xD4,                         // SEI ; CSH
        0xA9, 0xFF, 0x53, 0x01,             // MPR0 = FF (hardware)
        0xA9, 0xF8, 0x53, 0x02,             // MPR1 = F8 (RAM)
        0xA2, 0xFF, 0x9A,                   // stack
        0x9C, 0x02, 0x04, 0x9C, 0x03, 0x04, // VCE address 0
        0xA9, 0xC0, 0x8D, 0x04, 0x04,       // color 0 = green
        0xA9, 0x01, 0x8D, 0x05, 0x04,
        0x03, 0x05, 0x13, 0x08, 0x23, 0x00, // VDC CR = vblank IRQ
        0xA9, 0x05, 0x8D, 0x02, 0x14,       // only IRQ1 enabled
        0x58,                               // CLI
        0x80, 0xFE,                         // BRA *
    ];
    rom[..main.len()].copy_from_slice(&main);
    let irq = [0x48, 0xAD, 0x00, 0x00, 0xEE, 0x00, 0x20, 0x68, 0x40];   // PHA; LDA VDC status; INC $2000; PLA; RTI
    rom[0x40..0x40 + irq.len()].copy_from_slice(&irq);
    rom[0x50] = 0x40;                       // RTI for the other vectors
    for (v, target) in [(0x1FF6, 0xE050u16), (0x1FF8, 0xE040), (0x1FFA, 0xE050), (0x1FFC, 0xE050), (0x1FFE, 0xE000)] {
        rom[v] = target as u8;
        rom[v + 1] = (target >> 8) as u8;
    }
    rom
}

#[test]
fn vblank_irq_once_per_frame_and_backdrop() {
    let mut sys = PceSystem::from_rom(&test_card(), "t", &[]).unwrap();
    for _ in 0..10 { sys.run_frame(NO_KEY); }
    let frames = sys.bus.read(0xF8 << 13);
    assert!((9..=10).contains(&frames), "{frames}");
    let fb = sys.framebuffer();
    let o = (120 * OUT_W + 256) * 4;
    assert_eq!(&fb[o..o + 3], &[0, 255, 0]);
    let samples = sys.drain_audio().len() / 2 / 10;
    assert!((730..740).contains(&samples), "{samples} samples per frame");
}

#[test]
fn input_mapping() {
    let mut sys = PceSystem::from_rom(&test_card(), "t", &[]).unwrap();
    sys.set_input(NO_KEY | pad::LEFT | pad::A | pad::START);
    assert_eq!(sys.bus.pad, 0x08 | 0x10 | 0x80);
}

#[test]
fn save_state_round_trip() {
    let mut sys = PceSystem::from_rom(&test_card(), "t", &[]).unwrap();
    for _ in 0..5 { sys.run_frame(NO_KEY); }
    let state = sys.save_state();
    let before = sys.bus.read(0xF8 << 13);
    for _ in 0..6 { sys.run_frame(NO_KEY); }
    sys.load_state(&state).unwrap();
    assert_eq!(sys.bus.read(0xF8 << 13), before);
    for _ in 0..6 { sys.run_frame(NO_KEY); }
    assert_eq!(sys.bus.read(0xF8 << 13), before + 6);
}

#[test]
#[ignore]
fn speed_60_frames() {
    let mut sys = PceSystem::from_rom(&test_card(), "t", &[]).unwrap();
    sys.bus.vdc.regs[5] = 0x00C8;   // BG + sprites on so rendering does real work
    let t = std::time::Instant::now();
    for _ in 0..60 { sys.run_frame(NO_KEY); }
    println!("60 frames in {:?}", t.elapsed());
}

#[test]
#[ignore]
fn speed_heavy_scene() {
    let mut sys = PceSystem::from_rom(&test_card(), "t", &[]).unwrap();
    sys.run_frame(NO_KEY);
    // all 6 PSG channels on
    for ch in 0..6u32 {
        sys.bus.psg.write(0, ch as u8);
        sys.bus.psg.write(1, 0xFF);
        sys.bus.psg.write(2, 0x80 + ch as u8);
        sys.bus.psg.write(5, 0xFF);
        sys.bus.psg.write(4, 0x9F);
    }
    // 64 sprites, 32x64, spread over the screen; BG on with a 64x64 map
    for i in 0..64u16 {
        let e = [64 + (i * 3) % 200, 32 + (i * 37) % 256, 0x10, 0x3180];
        for (k, w) in e.iter().enumerate() { sys.bus.vdc.satb[(i * 4) as usize + k] = *w; }
    }
    sys.bus.vdc.regs[5] = 0x00C8;
    sys.bus.vdc.regs[9] = 0x0050;
    let t = std::time::Instant::now();
    for _ in 0..60 { sys.run_frame(NO_KEY); }
    println!("heavy: 60 frames in {:?}", t.elapsed());

    // where does the time go?
    let t = std::time::Instant::now();
    let mut buf = [0u16; MAX_WIDTH];
    for _ in 0..60 { for n in 0..240 { sys.bus.vdc.render_line(n, &mut buf); } }
    println!("  VDC render only: {:?}", t.elapsed());
    let t = std::time::Instant::now();
    for _ in 0..60 { sys.bus.psg.run(455 * 262); }
    println!("  PSG only:        {:?}", t.elapsed());
}

#[test]
#[ignore]
fn speed_breakdown() {
    let mut sys = PceSystem::from_rom(&test_card(), "t", &[]).unwrap();
    sys.run_frame(NO_KEY);
    let t = std::time::Instant::now();
    for _ in 0..60 { sys.output_all_lines_for_bench(); }
    println!("  output 60 frames: {:?}", t.elapsed());
    let t = std::time::Instant::now();
    for _ in 0..60 { sys.cpu_only_for_bench(); }
    println!("  cpu+bus 60 frames: {:?}", t.elapsed());
}

#[test]
fn disabled_interrupts_leave_no_stale_flags() {
    let mut v = Vdc::new();
    vdc_reg(&mut v, 0x06, 64 + 5);         // RCR = display line 5, raster IRQ still off
    v.check_raster(5);
    v.start_vblank();
    assert_eq!(v.status & 0x24, 0, "no raster/vblank flag while disabled");
    vdc_reg(&mut v, 0x05, 0x000C);         // now enable raster + vblank IRQs
    assert!(!v.irq_line(), "enabling must not fire an old event");
    v.check_raster(5);
    assert!(v.irq_line());
}
