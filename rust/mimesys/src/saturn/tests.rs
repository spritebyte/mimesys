use super::*;
use crate::common::system::System;

/// A tiny SH-2 program builder: code at `base`, 32-bit literals placed after it
struct Asm { base: u32, code: Vec<u16>, lits: Vec<(usize, usize, u32)> }

impl Asm {
    fn new(base: u32) -> Self { Self { base, code: vec![], lits: vec![] } }
    fn op(&mut self, o: u16) -> &mut Self { self.code.push(o); self }
    /// mov.l @(literal), Rn
    fn li(&mut self, n: u16, v: u32) -> &mut Self {
        self.lits.push((self.code.len(), n as usize, v));
        self.code.push(0xD000 | n << 8);
        self
    }
    /// bra to itself (+ nop)
    fn spin(&mut self) -> &mut Self { self.op(0xAFFE).op(0x0009) }
    fn place(&mut self, mem: &mut [u8]) {
        let mut code = self.code.clone();
        // literals after the code, 4-aligned
        let mut at = (self.base as usize + code.len() * 2 + 3) & !3;
        for &(i, _, v) in &self.lits {
            let pc = self.base as usize + i * 2;
            let disp = (at - ((pc & !3) + 4)) / 4;
            assert!(disp < 256);
            code[i] |= disp as u16;
            mem[at..at + 4].copy_from_slice(&v.to_be_bytes());
            at += 4;
        }
        for (k, w) in code.iter().enumerate() {
            let a = self.base as usize + k * 2;
            mem[a..a + 2].copy_from_slice(&w.to_be_bytes());
        }
    }
}

/// A fake BIOS: reset to 0x400 with the stack at 0x06001000, the program there, and
/// vector `vec` pointing at the handler at 0x800
fn bios(main: &mut Asm, handler: Option<(u8, &mut Asm)>) -> Vec<u8> {
    let mut b = vec![0u8; BIOS_SIZE];
    b[0..4].copy_from_slice(&0x400u32.to_be_bytes());
    b[4..8].copy_from_slice(&0x0600_1000u32.to_be_bytes());
    main.place(&mut b);
    if let Some((vec, h)) = handler {
        let v = vec as usize * 4;
        b[v..v + 4].copy_from_slice(&0x800u32.to_be_bytes());
        h.place(&mut b);
    }
    b
}

pub(super) fn system(b: Vec<u8>) -> SaturnSystem { SaturnSystem::from_rom(&[], "test", &b).unwrap() }

fn hw32(s: &SaturnSystem, a: usize) -> u32 { u32::from_be_bytes(s.bus.hwram[a..a + 4].try_into().unwrap()) }

#[test]
fn boots_from_the_bios_vectors_and_writes_work_ram() {
    let mut m = Asm::new(0x400);
    m.li(1, 0x0600_0100).li(0, 0x1234_5678).op(0x2102).spin();   // mov.l r0, @r1
    let mut s = system(bios(&mut m, None));
    assert!(is_bios(&s.bus.bios, "x.bin"), "the vectors look like a Saturn BIOS");
    s.run_frame(0);
    assert_eq!(hw32(&s, 0x100), 0x1234_5678);
    assert!(!s.bus.smpc.slave_on, "the slave starts held in reset");
}

#[test]
fn vblank_in_interrupts_the_master_through_the_scu() {
    let mut m = Asm::new(0x400);
    m.li(1, 0x25FE_00A0).op(0xE0FE).op(0x2102)       // IMS = FFFFFFFE: VBlank-IN only
        .op(0xE000).op(0x400E)                         // sr = 0
        .spin();
    let mut h = Asm::new(0x800);
    h.li(1, 0x0600_0800).op(0x6012).op(0x7001).op(0x2102)  // (*0x06000800)++
        .op(0x002B).op(0x0009);                        // rte
    let mut s = system(bios(&mut m, Some((0x40, &mut h))));
    for _ in 0..3 { s.run_frame(0); }
    assert_eq!(hw32(&s, 0x800), 3, "one VBlank-IN a frame");
    assert_eq!(s.bus.scu.ist & scu::IST_VBLANK_IN, 0, "taking it cleared IST");
    assert_ne!(s.bus.scu.ist & scu::IST_HBLANK_IN, 0, "masked ones stay pending");
}

#[test]
fn smpc_intback_gives_the_status_then_the_pads() {
    let mut s = system(bios(Asm::new(0x400).spin(), None));
    s.set_input(input::START | input::B);   // Start and (Saturn) A
    let b = &mut s.bus;
    for (r, v) in [(0x01, 0x01), (0x03, 0x08), (0x05, 0xF0)] { b.write16(0x0010_0000 + r - 1, v, false, true); }
    b.write16(0x0010_0062, 1, false, true);   // SF
    b.write16(0x0010_001E, 0x10, false, true); // INTBACK
    assert_eq!(b.read16(0x0010_0062) & 1, 1, "busy");
    b.tick(30_000, HZ_320);
    assert_eq!(b.read16(0x0010_0062) & 1, 0, "the status is ready");
    assert_ne!(b.scu.ist & scu::IST_SMPC, 0, "with the SMPC interrupt");
    assert_eq!(b.smpc.oreg[9], 4, "area: North America");
    assert_eq!(b.smpc.sr & 0x60, 0x60, "pad data to follow");
    // continue: the pads
    b.scu.ist = 0;
    b.write16(0x0010_0000, 0x80, false, true);
    b.tick(30_000, HZ_320);
    assert_eq!(&b.smpc.oreg[..4], &[0xF1, 0x02, !0x0C, 0xFF]);
    assert_ne!(b.scu.ist & scu::IST_SMPC, 0);
}

#[test]
fn bcr1_tells_the_master_from_the_slave() {
    // both CPUs run this from reset: BCR1 -> 06000100 (master) or 06000104 (slave, bit 15 set)
    let mut m = Asm::new(0x400);
    m.li(1, 0xFFFF_FFE0).op(0x6012)              // mov.l @r1, r0
        .op(0x6203).op(0x4219).op(0x4209).op(0x4209).op(0x4201) // r2 = r0 >> 13
        .op(0xE304).op(0x2239)                   // r2 &= 4
        .li(4, 0x0600_0100).op(0x324C).op(0x2202) // mov.l r0, @(06000100 + r2)
        .li(1, 0x2010_001F).op(0xE002).op(0x2100) // SSHON
        .spin();
    let mut s = system(bios(&mut m, None));
    s.run_frame(0);
    s.run_frame(0);
    assert!(s.bus.smpc.slave_on);
    assert_eq!(hw32(&s, 0x100) & 0xFFFF, 0x03F0, "master: MASTER bit clear");
    assert_eq!(hw32(&s, 0x104) & 0xFFFF, 0x83F0, "slave: MASTER bit set");
}

#[test]
fn smpc_sshon_starts_the_slave_and_minit_captures_on_the_master() {
    // the master turns the slave on, the slave writes MINIT
    let mut m = Asm::new(0x400);
    m.li(1, 0x2010_001F).op(0xE002).op(0x2100)   // COMREG = 02h (SSHON), mov.b
        .spin();
    let mut s = system(bios(&mut m, None));
    s.run_frame(0);
    assert!(s.bus.smpc.slave_on);
    // the slave runs the same code (writes SSHON again: ignored) - check MINIT by hand
    s.bus.write16(0x0180_0000, 0, true, true);   // MINIT
    assert!(s.bus.capture[0]);
    s.bus.write16(0x0100_0000, 0, true, true);   // SINIT
    assert!(s.bus.capture[1]);
    s.bus.capture[1] = false;
    s.msh.p.frt.icie = true;
    s.bus.capture[0] = false;
    s.msh.input_capture();
    assert!(s.msh.p.frt.icf && s.msh.p.frt.pending_irq);
}

#[test]
fn scu_direct_dma_copies_and_signals_the_end() {
    let mut s = system(bios(Asm::new(0x400).spin(), None));
    let b = &mut s.bus;
    for k in 0..16u32 { b.write16(0x0600_2000 + k * 2, 0x100 + k as u16, true, true); }
    let w32 = |b: &mut Bus, a: u32, v: u32| { b.write16(a, (v >> 16) as u16, true, true); b.write16(a + 2, v as u16, true, true); };
    w32(b, 0x05FE_0020, 0x0600_2000);   // level 1 source
    w32(b, 0x05FE_0024, 0x05E0_0000);   // VDP2 VRAM
    w32(b, 0x05FE_0028, 32);
    w32(b, 0x05FE_002C, 0x0101);        // +4 source, +2 destination
    w32(b, 0x05FE_0034, 0x0000_0007);   // start by write
    w32(b, 0x05FE_0030, 0x0000_0101);   // enable + go
    assert_eq!(&b.vdp2.vram[..4], &[0x01, 0x00, 0x01, 0x01]);
    assert_eq!(b.vdp2.vram[30..32], [0x01, 0x0F]);
    assert_ne!(b.scu.ist & 1 << 10, 0, "level 1 end");
}

#[test]
fn scu_indirect_dma_follows_its_table() {
    let mut s = system(bios(Asm::new(0x400).spin(), None));
    let b = &mut s.bus;
    let w32 = |b: &mut Bus, a: u32, v: u32| { b.write16(a, (v >> 16) as u16, true, true); b.write16(a + 2, v as u16, true, true); };
    w32(b, 0x0600_3000, 0xAAAA_BBBB);
    w32(b, 0x0600_3010, 0xCCCC_DDDD);
    // table: (4 bytes 0x06003000 -> VRAM 0x100), (4 bytes 0x06003010 -> VRAM 0x200, end)
    for (k, v) in [4, 0x05E0_0100, 0x0600_3000, 4, 0x05E0_0200, 0x8600_3010].into_iter().enumerate() {
        w32(b, 0x0600_4000 + k as u32 * 4, v);
    }
    w32(b, 0x05FE_0004, 0x0600_4000);
    w32(b, 0x05FE_000C, 0x0101);
    w32(b, 0x05FE_0014, 0x0100_0007);   // indirect, start by write
    w32(b, 0x05FE_0010, 0x0000_0101);
    assert_eq!(&b.vdp2.vram[0x100..0x104], &[0xAA, 0xAA, 0xBB, 0xBB]);
    assert_eq!(&b.vdp2.vram[0x200..0x204], &[0xCC, 0xCC, 0xDD, 0xDD]);
    assert_ne!(b.scu.ist & 1 << 11, 0, "level 0 end");
}

#[test]
fn scu_timer0_hits_its_line() {
    let mut s = system(bios(Asm::new(0x400).spin(), None));
    let b = &mut s.bus;
    b.write16(0x05FE_0092, 100, true, true);   // T0C
    b.write16(0x05FE_009A, 1, true, true);     // TENB
    s.run_frame(0);
    assert_ne!(s.bus.scu.ist & scu::IST_TIMER0, 0);
    assert_ne!(s.bus.scu.ist & scu::IST_TIMER1, 0, "timer 1 every line");
}

#[test]
fn cd_block_says_cdblock_then_answers_commands() {
    let mut s = system(bios(Asm::new(0x400).spin(), None));
    let b = &mut s.bus;
    let regs: Vec<u16> = (0..4).map(|k| b.read16(0x0589_0018 + k * 4)).collect();
    assert_eq!(regs, [0x0043, 0x4442, 0x4C4F, 0x434B]);
    assert_eq!(b.read16(0x0589_0008) & cdblock::CMOK, 1);
    // Get Hardware Info
    b.write16(0x0589_0008, !cdblock::CMOK, true, true);
    for (k, v) in [0x0100u16, 0, 0, 0].into_iter().enumerate() { b.write16(0x0589_0018 + k as u32 * 4, v, true, true); }
    for _ in 0..400 { b.tick(64, HZ_320); }
    assert_eq!(b.read16(0x0589_0008) & cdblock::CMOK, 1);
    assert_eq!(b.read16(0x0589_0018) & 0x0F00, 0x0700, "no disc");
    assert_eq!(b.read16(0x0589_001C), 0x0201);
}

#[test]
fn vdp1_walks_its_command_list_and_signals_the_end() {
    let mut s = system(bios(Asm::new(0x400).spin(), None));
    let b = &mut s.bus;
    // command 0: jump to 0x40 (link 8); command at 0x40: end
    b.write16(0x05C0_0000, 0x1000, true, true);
    b.write16(0x05C0_0002, 0x0008, true, true);
    b.write16(0x05C0_0040, 0x8000, true, true);
    b.write16(0x05D0_0004, 1, true, true);   // PTMR: draw now
    assert_eq!(b.read16(0x05D0_0010) & 2, 2, "CEF");
    assert_eq!(b.read16(0x05D0_0014), 8, "COPR at the END");
    b.tick(64, HZ_320);
    assert_ne!(b.scu.ist & scu::IST_SPRITE_END, 0);
}

#[test]
fn backup_ram_and_smem_survive_the_save_file() {
    let mut s = system(bios(Asm::new(0x400).spin(), None));
    s.bus.write16(0x0018_0000, 0x0042, false, true);
    assert!(s.is_sram_dirty());
    for (r, v) in [(1, 0), (3, 0), (5, 0), (7, 5)] { s.bus.write16(0x0010_0000 + r - 1, v, false, true); }
    s.bus.write16(0x0010_001E, 0x17, false, true);   // SETSMEM: Japanese
    s.bus.tick(5_000, HZ_320);
    s.bus.write16(0x0010_0000, 0, false, true);      // any SMPC write refreshes the copy
    let file = s.get_sram().unwrap().to_vec();
    assert_eq!(file.len(), bus::SRAM_SIZE);
    assert_eq!(file[0], 0x42);
    let mut t = system(bios(Asm::new(0x400).spin(), None));
    t.load_sram(&file);
    assert_eq!(t.bus.read16(0x0018_0000), 0xFF42);
    assert_eq!(t.bus.smpc.smem, [0, 0, 0, 5, 0xFF]);
}

#[test]
fn save_state_round_trips() {
    let mut m = Asm::new(0x400);
    m.li(1, 0x0600_0100).op(0x6012).op(0x7001).op(0x2102).op(0xAFFA).op(0x0009);
    let mut s = system(bios(&mut m, None));
    s.run_frame(0);
    let st = s.save_state();
    let before = hw32(&s, 0x100);
    s.run_frame(0);
    let after = hw32(&s, 0x100);
    s.load_state(&st).unwrap();
    assert_eq!(hw32(&s, 0x100), before);
    s.run_frame(0);
    assert_eq!(hw32(&s, 0x100), after, "deterministic");
}

#[test]
#[ignore]
fn bench_two_cpus() {
    let mut m = Asm::new(0x400);
    // cache on (CCR = 1), then a load / add / store loop in work RAM, as games run
    m.li(2, 0xFFFF_FE92).op(0xE301).op(0x2230)
        .li(1, 0x0600_0100).op(0x6012).op(0x7001).op(0x2102).op(0xAFFA).op(0x0009);
    let mut s = system(bios(&mut m, None));
    s.bus.smpc.slave_on = true;
    s.bus.smpc.sound_on = true;
    let t = std::time::Instant::now();
    for _ in 0..120 { s.run_frame(0); }
    println!("120 frames: {:?}", t.elapsed());
}

/// A Saturn disc: a data track (sector 0 = the system ID) and an audio track
fn saturn_sector(lba: u32) -> [u8; 2352] {
    let mut s = [0u8; 2352];
    s[1..11].fill(0xFF);
    let (m, sec, f) = ((lba + 150) / 4500, (lba + 150) / 75 % 60, (lba + 150) % 75);
    let bcd = |v: u32| ((v / 10) << 4 | v % 10) as u8;
    s[12] = bcd(m); s[13] = bcd(sec); s[14] = bcd(f); s[15] = 1;
    if lba == 0 {
        s[16..32].copy_from_slice(b"SEGA SEGASATURN ");
        s[16 + 0x40] = b'U';
        s[16 + 0x60..16 + 0x6A].copy_from_slice(b"TEST TITLE");
    }
    s
}

fn saturn_chd(dir: &str, pregap: u32) -> std::path::PathBuf {
    use chd::write::{ChdWriteOptions, ChdWriter};
    use std::io::Write;
    let d = std::env::temp_dir().join(format!("mimesys_ss_{dir}_{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join("game.chd");
    let mut frames = Vec::new();
    for lba in 0..20 { frames.extend_from_slice(&saturn_sector(lba)); frames.extend_from_slice(&[0u8; 96]); }
    for _ in 0..(8 + pregap) { frames.extend_from_slice(&[0u8; 2448]); }
    let mut w = ChdWriter::new(std::fs::File::create(&p).unwrap(), ChdWriteOptions::cd()).unwrap();
    for m in [
        "TRACK:1 TYPE:MODE1_RAW SUBTYPE:NONE FRAMES:20 PREGAP:0 PGTYPE:MODE1 PGSUB:NONE POSTGAP:0".to_string(),
        format!("TRACK:2 TYPE:AUDIO SUBTYPE:NONE FRAMES:{} PREGAP:{pregap} PGTYPE:VAUDIO PGSUB:NONE POSTGAP:0", 8 + pregap),
    ] {
        let mut b = m.into_bytes();
        b.push(0);
        w.add_metadata(chd::metadata::KnownMetadata::CdRomTrack2, b.as_slice(), true).unwrap();
    }
    w.write_all(&frames).unwrap();
    w.finish().unwrap();
    p
}

#[test]
fn a_saturn_chd_is_detected_as_saturn() {
    for pregap in [0, 150] {
        let p = saturn_chd(&format!("det{pregap}"), pregap);
        let mut blob = crate::segacd::PATH_MAGIC.to_vec();
        blob.extend_from_slice(p.to_string_lossy().as_bytes());
        let g = crate::common::rom_detect::detect(&blob, &p.to_string_lossy());
        assert_eq!(g.first().map(|g| g.system), Some("saturn"), "pregap {pregap}: {:?}", g.iter().map(|g| (g.system, &g.why)).collect::<Vec<_>>());
    }
}

pub(super) fn spin_system() -> SaturnSystem { system(bios(Asm::new(0x400).spin(), None)) }

#[test]
fn scu_dsp_multiplies_adds_and_ends_with_its_interrupt() {
    let mut m = Asm::new(0x400);
    m.spin();
    let mut s = system(bios(&mut m, None));
    let w32 = |s: &mut SaturnSystem, a: u32, v: u32| { s.bus.write16(a, (v >> 16) as u16, true, true); s.bus.write16(a + 2, v as u16, true, true); };
    let prog = [
        0x9000_0003u32, // MVI #3, RX
        0x8400_0006,    // MVI #6, MC1      (MD1[0] = 6, CT1 -> 1)
        0x0000_1D00,    // MOV #0, CT1
        0x0008_4000,    // MOV M1, Y        (RY = 6)
        0x0000_0000,    // NOP              (MUL = 18)
        0x0102_0000,    // MOV MUL, P ; CLR A
        0x1000_3209,    // ADD ; MOV ALL, MC2
        0xD000_000A,    // JMP 0A (the next one still runs)
        0x8800_0063,    // MVI #99, MC2     (delay slot: MD2[1] = 99)
        0x8800_0001,    // (skipped)
        0xF800_0000,    // 0A: ENDI
    ];
    w32(&mut s, 0x05FE_0080, 0x0000_8000);              // LE, PC = 0
    for op in prog { w32(&mut s, 0x05FE_0084, op); }
    w32(&mut s, 0x05FE_0080, 0x0001_8000);              // LE + EX: run from 0
    s.run_frame(0);
    assert!(!s.bus.scu.dsp.running(), "ENDI stopped it");
    assert!(s.bus.scu.ist & scu::IST_DSP_END != 0, "the DSP end interrupt is raised");
    let ppaf = (s.bus.read16(0x05FE_0080) as u32) << 16 | s.bus.read16(0x05FE_0082) as u32;
    assert!(ppaf & (1 << 18) != 0, "E flag");
    w32(&mut s, 0x05FE_0088, 0x80);                     // MD2, address 0
    let rd = |s: &mut SaturnSystem| (s.bus.read16(0x05FE_008C) as u32) << 16 | s.bus.read16(0x05FE_008E) as u32;
    assert_eq!(rd(&mut s), 18, "3 * 6 through MUL -> P -> ALU");
    assert_eq!(rd(&mut s), 99, "the jump's delay slot ran");
    assert_eq!(rd(&mut s), 0, "the instruction after the delay slot was skipped");
}

#[test]
fn idle_wait_loops_are_skipped_but_interrupts_still_arrive() {
    // the master spins in "bra self" with VBlank-IN unmasked; the handler counts in 06000200
    let mut m = Asm::new(0x400);
    m.li(1, 0x25FE_00A0).li(0, 0xFFFF_FFFE).op(0x2102)      // IMS: VBlank-IN on
        .li(1, 0x0600_0000).op(0x412E)                       // VBR = 06000000? (ldc r1, vbr)
        .op(0xE000).op(0x400E)                               // sr = 0 (ldc r0, sr)
        .spin();
    let mut h = Asm::new(0x800);
    h.li(1, 0x0600_0200).op(0x6012).op(0x7001).op(0x2102).op(0x002B).op(0x0009);  // count; rte
    let mut b = bios(&mut m, Some((0x40, &mut h)));
    // the vector table is at VBR = 06000000: put the handler address there too
    let mut s = system(std::mem::take(&mut b));
    s.bus.hwram[0x100..0x104].copy_from_slice(&0x800u32.to_be_bytes());   // vector 40h
    for _ in 0..5 { s.run_frame(0); }
    assert!(s.idle_clocks[0] > 1_000_000, "the spin was skipped ({} clocks)", s.idle_clocks[0]);
    let n = hw32(&s, 0x200);
    assert!((4..=6).contains(&n), "one VBlank interrupt a frame still arrives ({n})");
}

#[test]
#[ignore]
fn bench_two_cpus_waiting() {
    // both CPUs in a cached poll loop (read a flag, branch back), as games wait for VBlank
    let mut m = Asm::new(0x400);
    m.li(2, 0xFFFF_FE92).op(0xE301).op(0x2230)
        .li(1, 0x0600_0100).op(0x6012).op(0x2008).op(0x89FD).op(0x0009).spin();   // mov.l @r1,r0; tst r0,r0; bt loop
    let mut s = system(bios(&mut m, None));
    s.bus.smpc.slave_on = true;
    s.bus.smpc.sound_on = true;
    let t = std::time::Instant::now();
    for _ in 0..120 { s.run_frame(0); }
    println!("120 frames waiting: {:?} (idle {:?})", t.elapsed(), s.idle_clocks);
}

#[test]
fn idle_loops_may_rewrite_the_same_ram_but_not_count() {
    // loop A: store the same value each pass (idle); loop B (other test bench): a counter (busy)
    let mut m = Asm::new(0x400);
    m.li(1, 0x0600_0100).op(0xE205)            // r2 = 5
        .op(0x2122)                             // loop: mov.l r2, @r1
        .op(0x6012)                             // mov.l @r1, r0
        .op(0x8800)                             // cmp/eq #0, r0
        .op(0x8BFC)                             // bf loop
        .op(0x0009).spin();
    let mut s = system(bios(&mut m, None));
    for _ in 0..3 { s.run_frame(0); }
    assert!(s.idle_clocks[0] > 500_000, "same-value stores still idle ({})", s.idle_clocks[0]);
    let mut m = Asm::new(0x400);
    m.li(1, 0x0600_0100).op(0x6012).op(0x7001).op(0x2102).op(0xAFFA).op(0x0009);
    let mut s = system(bios(&mut m, None));
    for _ in 0..3 { s.run_frame(0); }
    assert_eq!(s.idle_clocks[0], 0, "a counter loop is real work");
    assert!(hw32(&s, 0x100) > 10_000);
}
