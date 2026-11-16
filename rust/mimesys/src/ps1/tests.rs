use super::bus::*;
use super::gpu::{Gpu, VRAM_W};
use super::*;
use crate::common::cpu::r3000::R3000Bus;

// ---- encoders (see common/cpu/r3000/tests.rs)
fn i(op: u32, rs: u32, rt: u32, imm: u16) -> u32 { op << 26 | rs << 21 | rt << 16 | imm as u32 }
fn lui(rt: u32, v: u16) -> u32 { i(0x0F, 0, rt, v) }
fn ori(rt: u32, rs: u32, v: u16) -> u32 { i(0x0D, rs, rt, v) }
fn sw(rt: u32, base: u32, off: i16) -> u32 { i(0x2B, base, rt, off as u16) }
fn lw(rt: u32, base: u32, off: i16) -> u32 { i(0x23, base, rt, off as u16) }
fn addiu(rt: u32, rs: u32, v: i16) -> u32 { i(0x09, rs, rt, v as u16) }
fn mtc0(rt: u32, rd: u32) -> u32 { 0x4080_0000 | rt << 16 | rd << 11 }
fn mfc0(rt: u32, rd: u32) -> u32 { 0x4000_0000 | rt << 16 | rd << 11 }
fn jr(rs: u32) -> u32 { rs << 21 | 8 }
const RFE: u32 = 0x4200_0010;
const NOP: u32 = 0;
const T0: u32 = 8;
const T1: u32 = 9;
const K0: u32 = 26;
const K1: u32 = 27;

/// Store a 32-bit value at T0 + off (T0 = 1F800000)
fn store(out: &mut Vec<u32>, off: i16, v: u32) {
    out.extend([lui(T1, (v >> 16) as u16), ori(T1, T1, v as u16), sw(T1, T0, off)]);
}

fn system(bios_code: &[u32], ram: &[(u32, &[u32])]) -> Ps1System {
    let mut bios = vec![0u8; BIOS_SIZE];
    for (k, w) in bios_code.iter().enumerate() { bios[4 * k..4 * k + 4].copy_from_slice(&w.to_le_bytes()); }
    let mut s = Ps1System::from_rom(&bios, "scph1001.bin", &[]).unwrap();
    for (addr, words) in ram {
        for (k, w) in words.iter().enumerate() { s.bus.write32(addr + 4 * k as u32, *w); }
    }
    s
}

/// A tiny "BIOS": sets up the GPU, draws, turns on the vblank IRQ and waits. The RAM
/// handler (80000080 jumps to 80001000) counts vblanks at 80000100.
#[test]
fn bios_draws_and_counts_vblanks() {
    let mut p = vec![lui(T0, 0x1F80)];
    store(&mut p, 0x1814, 0x0800_0001);             // GP1(08): 320 x 240
    store(&mut p, 0x1814, 0x0300_0000);             // display on
    store(&mut p, 0x1810, 0xE300_0000);             // drawing area 0,0 - 319,239
    store(&mut p, 0x1810, 0xE400_0000 | 239 << 10 | 319);
    store(&mut p, 0x1810, 0xE500_0000);
    store(&mut p, 0x1810, 0x02FF_0000);             // fill blue
    store(&mut p, 0x1810, 0);
    store(&mut p, 0x1810, 0x00F0_0140);
    store(&mut p, 0x1810, 0x2000_00FF);             // flat red triangle
    store(&mut p, 0x1810, 0x000A_000A);
    store(&mut p, 0x1810, 0x000A_0064);
    store(&mut p, 0x1810, 0x0064_000A);
    store(&mut p, 0x1074, 1);                       // I_MASK: vblank
    p.extend([lui(T1, 0), ori(T1, T1, 0x401), mtc0(T1, 12)]);   // SR: IM2, IEc, BEV off
    // putchar('H') through the A0 table: r9 = 3C, r4 = 'H', jump to A0
    p.extend([addiu(9, 0, 0x3C), addiu(4, 0, b'H' as i16), lui(T1, 0x8000), ori(T1, T1, 0xA0), 0x0120_F809, NOP]);  // JALR T1
    p.extend([addiu(4, 0, b'\n' as i16), lui(T1, 0x8000), ori(T1, T1, 0xA0), 0x0120_F809, NOP]);
    let here = p.len() as u32;
    p.extend([0x1000_FFFF, NOP]);                   // B $
    let _ = here;
    let handler = [
        lui(K0, 0x1F80), sw(0, K0, 0x1070),          // acknowledge
        lui(K1, 0x8000), lw(K0, K1, 0x100), NOP, addiu(K0, K0, 1), sw(K0, K1, 0x100),
        mfc0(K0, 14), NOP, jr(K0), RFE,
    ];
    let a0 = [jr(31), NOP];
    let mut s = system(&p, &[(0x80, &[0x0800_0400, NOP]), (0x1000, &handler), (0xA0, &a0)]);
    for _ in 0..5 { s.run_frame(0); }
    let v = &s.bus.gpu.vram;
    assert_eq!(v[20 * VRAM_W + 20], 0x001F, "red triangle");
    assert_eq!(v[200 * VRAM_W + 200], 0x7C00, "blue fill");
    assert_eq!(v[10 * VRAM_W + 100], 0x7C00, "the triangle's right corner isn't drawn");
    let n = u32::from_le_bytes(s.bus.ram[0x100..0x104].try_into().unwrap());
    assert!((4..=5).contains(&n), "one vblank IRQ a frame: {n}");
    let d = s.display_info();
    assert_eq!((d.buffer_width, d.buffer_height), (320, 240));
    let fb = s.framebuffer();
    assert_eq!(&fb[(20 * 320 + 20) * 4..(20 * 320 + 20) * 4 + 4], &[0xFF, 0, 0, 0xFF]);
    assert_eq!(s.tty_line, "", "a whole line was printed");
    assert!((16_600..16_800).contains(&s.frame_time_usec()));
}

#[test]
fn dma_ordering_table_and_gpu_list() {
    let mut b = Bus::new(vec![]);
    // channel 6: 16 entries ending at 2000 + 3C
    b.write32(0x1F80_10F0, 0x0800_0000 | 0x800);    // DPCR: channels 6 and 2
    b.write32(0x1F80_10F4, 0x0084_0000 | 1 << 22);  // DICR: master, channel 6 IRQ
    b.write32(0x1F80_10E0, 0x2000 + 0x3C);
    b.write32(0x1F80_10E4, 16);
    b.write32(0x1F80_10E8, 0x1100_0002);
    let r = |b: &mut Bus, a: u32| b.read32(a);
    assert_eq!(r(&mut b, 0x2000 + 0x3C), 0x2038);
    assert_eq!(r(&mut b, 0x2004), 0x2000);
    assert_eq!(r(&mut b, 0x2000), 0x00FF_FFFF);
    assert_eq!(r(&mut b, 0x1F80_10E8) & 0x0100_0000, 0, "done");
    assert_ne!(b.i_stat & IRQ_DMA, 0, "the DMA IRQ");
    assert_ne!(r(&mut b, 0x1F80_10F4) & (1 << 30), 0, "channel 6's flag");
    // acknowledging I_STAT, then writing DICR without clearing the flag: no new IRQ
    b.write32(0x1F80_1070, !IRQ_DMA);
    b.write32(0x1F80_10F4, 0x0084_0000 | 1 << 22);
    assert_eq!(b.i_stat & IRQ_DMA, 0, "the IRQ line stayed high: no new edge");
    // clearing the flag drops the line; the next transfer raises it again
    b.write32(0x1F80_10F4, 0x0084_0000 | 1 << 22 | 1 << 30);
    assert_eq!(r(&mut b, 0x1F80_10F4) >> 31, 0);
    b.write32(0x1F80_10E8, 0x1100_0002);
    assert_ne!(b.i_stat & IRQ_DMA, 0);

    // channel 2 linked list: one node with a fill, then the end marker
    for (k, w) in [0x0300_0000u32 | 0xFF_FFFF, 0x0200_00FF, 0x0000_0000, 0x0010_0010].iter().enumerate() {
        b.write32(0x3000 + 4 * k as u32, *w);
    }
    b.write32(0x1F80_10A0, 0x3000);
    b.write32(0x1F80_10A8, 0x0100_0401);
    assert_eq!(b.gpu.vram[5 * VRAM_W + 5], 0x001F);
    assert_eq!(b.gpu.vram[5 * VRAM_W + 16], 0);
}

#[test]
fn pad_exchange() {
    let mut b = Bus::new(vec![]);
    b.sio.buttons[0] = !(1 << 14);                  // Cross
    b.write16(0x1F80_104A, 0x1003);                 // TX on, select, ACK IRQ
    let mut got = vec![];
    for tx in [0x01u8, 0x42, 0x00, 0x00, 0x00] {
        b.write8(0x1F80_1040, tx);
        b.tick(2000);
        got.push(b.read8(0x1F80_1040));
        b.write16(0x1F80_104A, 0x1013);             // acknowledge
    }
    assert_eq!(got, vec![0xFF, 0x41, 0x5A, 0xFF, 0xBF]);
    assert_ne!(b.i_stat & IRQ_SIO0, 0);
}

#[test]
fn timers() {
    let mut b = Bus::new(vec![]);
    // timer 2: sysclk / 8, IRQ at target 100, reset at target, repeat
    b.write32(0x1F80_1128, 100);
    b.write32(0x1F80_1124, 0x0200 | 0x08 | 0x10 | 0x40);
    b.tick(808);
    assert_eq!(b.i_stat & (1 << 6), 1 << 6, "timer 2 reached its target");
    assert_eq!(b.read32(0x1F80_1120), 0, "and went back to 0");
    // timer 1 on hblanks
    b.write32(0x1F80_1114, 0x0100);
    for _ in 0..7 { b.timers.hblank(); }
    assert_eq!(b.read32(0x1F80_1110), 7);
    // mode reads clear "reached target"
    assert_ne!(b.read32(0x1F80_1124) & 0x800, 0);
    assert_eq!(b.read32(0x1F80_1124) & 0x800, 0);
}

#[test]
fn cdrom_with_no_disc() {
    let mut b = Bus::new(vec![]);
    b.write8(0x1F80_1800, 1);
    b.write8(0x1F80_1802, 0x1F);                    // all interrupts on
    b.write8(0x1F80_1800, 0);
    b.write8(0x1F80_1802, 0x20);                    // Test(20): the version
    b.write8(0x1F80_1801, 0x19);
    b.tick(30_000);
    assert_ne!(b.i_stat & IRQ_CDROM, 0);
    b.write8(0x1F80_1800, 1);
    assert_eq!(b.read8(0x1F80_1803) & 7, 3, "INT3");
    let r: Vec<u8> = (0..4).map(|_| b.read8(0x1F80_1801)).collect();
    assert_eq!(r, vec![0x94, 0x09, 0x19, 0xC0]);
    b.write8(0x1F80_1803, 0x1F);
    // GetID: INT3 then INT5 "no disc"
    b.write8(0x1F80_1800, 0);
    b.write8(0x1F80_1801, 0x1A);
    b.tick(30_000);
    b.write8(0x1F80_1800, 1);
    assert_eq!(b.read8(0x1F80_1803) & 7, 3);
    b.write8(0x1F80_1803, 0x1F);
    b.tick(110_000);
    assert_eq!(b.read8(0x1F80_1803) & 7, 5);
    assert_eq!(b.read8(0x1F80_1801), 0x08);
    assert_eq!(b.read8(0x1F80_1801), 0x40);
}

fn gpu() -> Gpu {
    let mut g = Gpu::new();
    g.gp0(0xE300_0000);
    g.gp0(0xE400_0000 | 511 << 10 | 1023);
    g
}

#[test]
fn gpu_gouraud_textures_and_blending() {
    let mut g = gpu();
    // Gouraud triangle, red to green to blue
    for w in [0x3000_00FF, 0x0000_0000, 0x0000_FF00, 0x0000_0040, 0x00FF_0000, 0x0040_0000] { g.gp0(w); }
    let p = g.vram[VRAM_W + 1];
    assert!(p & 31 > 25, "near the red corner: {p:04X}");
    // 15-bit texture: upload a 2x1 texture at (512, 0), draw a textured rect (raw)
    for w in [0xA000_0000, 0x0000_0200, 0x0001_0002, 0x7C1F_03E0] { g.gp0(w); }
    g.gp0(0xE100_0000 | 8 | 2 << 7);                 // page x 512, 15-bit
    for w in [0x6500_0000, 0x0064_0064, 0x0000_0000, 0x0001_0002] { g.gp0(w); }
    assert_eq!(g.vram[100 * VRAM_W + 100], 0x03E0);
    assert_eq!(g.vram[100 * VRAM_W + 101], 0x7C1F);
    // 4-bit CLUT texture: CLUT at (0, 256) = [0, 001F, ...]; texel row: indices 1, 0
    for w in [0xA000_0000, 0x0100_0000, 0x0001_0002, 0x001F_0000] { g.gp0(w); }
    for w in [0xA000_0000, 0x0000_0300, 0x0001_0001, 0x0000_0001] { g.gp0(w); }
    g.gp0(0xE100_0000 | 12);                         // page x 768, 4-bit
    for w in [0x6500_0000, 0x00C8_00C8, 0x4000_0000, 0x0001_0002] { g.gp0(w); }   // CLUT y 256
    assert_eq!(g.vram[200 * VRAM_W + 200], 0x001F, "index 1");
    assert_eq!(g.vram[200 * VRAM_W + 201], 0x0000, "index 0 is see-through");
    // semi-transparent B+F over 0x0842 (8,2,2)
    g.vram[300 * VRAM_W + 300] = 0x0848;
    g.gp0(0xE100_0000 | 1 << 5);
    for w in [0x6A08_0808, 0x012C_012C] { g.gp0(w); } // 1x1 rect, (8,8,8)>>3 = 1 each
    assert_eq!(g.vram[300 * VRAM_W + 300], 0x0848 + 0x0421);
    // VRAM to CPU
    for w in [0xC000_0000, 0x0064_0064, 0x0001_0002] { g.gp0(w); }
    assert_eq!(g.read(), 0x7C1F_03E0);
    // mask: set on draw, then check blocks a fill-free overwrite
    g.gp0(0xE600_0003);
    for w in [0x6800_00FF, 0x0000_0190] { g.gp0(w); }
    assert_eq!(g.vram[400], 0x801F);
    for w in [0x6800_FF00, 0x0000_0190] { g.gp0(w); }
    assert_eq!(g.vram[400], 0x801F, "the mask bit protects it");
}

#[test]
fn gpu_lines_and_status() {
    let mut g = gpu();
    g.gp0(0xE600_0000);
    // flat polyline (0,0) -> (10,0) -> (10,10), terminated
    for w in [0x4800_00FF, 0x0000_0000, 0x0000_000A, 0x000A_000A, 0x5555_5555] { g.gp0(w); }
    assert_eq!(g.vram[5], 0x001F);
    assert_eq!(g.vram[5 * VRAM_W + 10], 0x001F);
    // the polyline ended: the next word is a command again
    for w in [0x6800_FF00, 0x0014_0014] { g.gp0(w); }
    assert_eq!(g.vram[20 * VRAM_W + 20], 0x03E0);
    let s = g.status();
    assert_ne!(s & (1 << 26), 0, "ready for commands");
    assert_ne!(s & (1 << 28), 0);
    g.gp1(0x0800_0007 | 0x20);                       // 640 x 480 interlaced
    assert_eq!((g.display_width(), g.display_height()), (640, 480));
    g.gp1(0x1000_0007);
    assert_eq!(g.read(), 2, "GPU version");
}

#[test]
fn exe_side_load() {
    let mut exe = vec![0u8; 0x800 + 8];
    exe[..8].copy_from_slice(b"PS-X EXE");
    exe[0x10..0x14].copy_from_slice(&0x8001_0000u32.to_le_bytes());
    exe[0x18..0x1C].copy_from_slice(&0x8001_0000u32.to_le_bytes());
    exe[0x1C..0x20].copy_from_slice(&8u32.to_le_bytes());
    exe[0x30..0x34].copy_from_slice(&0x801F_FF00u32.to_le_bytes());
    exe[0x800..0x808].copy_from_slice(&[0xFF, 0xFF, 0x00, 0x10, 0, 0, 0, 0]);   // B $
    // the BIOS jumps straight to the shell entry
    let mut bios = vec![0u8; BIOS_SIZE];
    for (k, w) in [lui(T0, 0x8003), jr(T0), NOP].iter().enumerate() { bios[4 * k..4 * k + 4].copy_from_slice(&w.to_le_bytes()); }
    let mut s = Ps1System::from_rom(&exe, "game.exe", &bios).unwrap();
    s.run_frame(0);
    assert_eq!(s.cpu.pc & 0xFFFF_FFF8, 0x8001_0000);
    assert_eq!(s.cpu.regs[29], 0x801F_FF00);
}


/// Speed: 60 frames of a register-only loop (cargo test --release -- --ignored speed)
#[test]
#[ignore]
fn speed() {
    let p = [addiu(T0, T0, 1), i(0x04, 0, 0, 0xFFFE), addiu(T1, T1, 1)];
    let mut s = system(&[lui(T0, 0x8000), ori(T0, T0, 0x1000), jr(T0), NOP], &[(0x1000, &p)]);
    s.cpu.regs[T0 as usize] = 0;
    let t = std::time::Instant::now();
    for _ in 0..60 { s.run_frame(0); }
    let ms = t.elapsed().as_millis();
    eprintln!("60 frames: {ms} ms ({} instructions)", s.cpu.regs[T0 as usize] * 3);
}

/// Bring-up: load a save state (PS1_STATE=path) and dump RAM + registers
#[test]
#[ignore]
fn dump_state() {
    let path = std::env::var("PS1_STATE").unwrap();
    let data = std::fs::read(&path).unwrap();
    let mut s = Ps1System::from_rom(&vec![0u8; BIOS_SIZE], "scph1001.bin", &[]).unwrap();
    s.load_state(&data).unwrap();
    std::fs::write(format!("{path}.ram"), &s.bus.ram).unwrap();
    let c = &s.cpu;
    eprintln!("pc {:08X} next {:08X} sr {:08X} cause {:08X} epc {:08X}", c.pc, c.next_pc, c.cop0[12], c.cause(), c.cop0[14]);
    for r in 0..32 { eprint!("r{r}={:08X} ", c.regs[r]); if r % 8 == 7 { eprintln!(); } }
    eprintln!("I_STAT {:X} I_MASK {:X} DPCR {:08X} DICR {:08X}", s.bus.i_stat, s.bus.i_mask, s.bus.dpcr, s.bus.dicr);
    for (n, d) in s.bus.dma.iter().enumerate() { eprintln!("dma{n} madr {:08X} bcr {:08X} chcr {:08X}", d.madr, d.bcr, d.chcr); }
    for (n, t) in s.bus.timers.t.iter().enumerate() { eprintln!("t{n} count {:X} mode {:X} target {:X}", t.count, t.mode, t.target); }
    // run on: how often is the CPU outside an exception handler?
    for f in 0..20 {
        let mut out = 0;
        for _ in 0..1000 {
            s.run_cycles(500);
            if s.cpu.cop0[12] & 1 != 0 { out += 1; }
        }
        eprintln!("chunk {f}: interrupts on {out}/1000, pc {:08X}, I_STAT {:X}", s.cpu.pc, s.bus.i_stat);
    }
}
