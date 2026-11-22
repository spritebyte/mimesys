//! Dreamcast stage 2 tests: programs in a made-up BIOS run on the real bus, plus device
//! checks through the bus.

use super::*;
use crate::common::cpu::sh4::Sh4Bus;

/// A tiny SH-4 assembler with a literal pool for 32-bit constants.
struct Asm { code: Vec<u16>, lits: Vec<(usize, u32)>, labels: HashMap<&'static str, usize>, fix: Vec<(usize, &'static str, u8)> }

impl Asm {
    fn new() -> Self { Self { code: Vec::new(), lits: Vec::new(), labels: HashMap::new(), fix: Vec::new() } }
    fn op(&mut self, op: u16) -> &mut Self { self.code.push(op); self }
    /// Rn = v (MOV.L @(disp,PC),Rn from the pool)
    fn li(&mut self, n: u16, v: u32) -> &mut Self { self.lits.push((self.code.len(), v)); self.op(0xD000 | n << 8) }
    fn label(&mut self, l: &'static str) -> &mut Self { self.labels.insert(l, self.code.len()); self }
    /// BRA / BF / BT to a label (kind 0xA0, 0x8B, 0x89); BRA has a delay slot
    fn jump(&mut self, kind: u8, l: &'static str) -> &mut Self { self.fix.push((self.code.len(), l, kind)); self.op(0) }
    fn mov_l_store(&mut self, m: u16, n: u16) -> &mut Self { self.op(0x2002 | n << 8 | m << 4) } // MOV.L Rm,@Rn
    fn mov_l_load(&mut self, m: u16, n: u16) -> &mut Self { self.op(0x6002 | n << 8 | m << 4) }  // MOV.L @Rm,Rn
    fn add_i(&mut self, n: u16, i: i8) -> &mut Self { self.op(0x7000 | n << 8 | i as u8 as u16) }
    fn nop(&mut self) -> &mut Self { self.op(0x0009) }

    /// Code + pool, as bytes placed at `base` (the pool's displacements depend on it).
    fn build(&mut self, base: u32) -> Vec<u8> {
        for &(at, l, kind) in &self.fix {
            let to = self.labels[l] as i32;
            let d = to - (at as i32 + 2);
            self.code[at] = if kind == 0xA0 { 0xA000 | (d as u16 & 0xFFF) } else { (kind as u16) << 8 | (d as u8 as u16) };
        }
        let mut words = self.code.clone();
        if words.len() % 2 == 1 { words.push(0x0009); }
        let pool = words.len() * 2;
        let mut bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        for (k, &(at, v)) in self.lits.iter().enumerate() {
            let lit = pool + 4 * k;
            let pc = (base as usize + at * 2 + 4) & !3;
            let d = (base as usize + lit - pc) / 4;
            assert!(d < 256, "literal out of reach");
            let op = u16::from_le_bytes([bytes[at * 2], bytes[at * 2 + 1]]) | d as u16;
            bytes[at * 2..at * 2 + 2].copy_from_slice(&op.to_le_bytes());
            bytes.extend(v.to_le_bytes());
        }
        bytes
    }
}

/// A system whose "BIOS" is `prog` at 0 (where the SH-4 starts: A0000000), and `extra`
/// pieces of code elsewhere in the BIOS (offset, bytes).
fn system_with(prog: &[u8], extra: &[(usize, Vec<u8>)]) -> DcSystem {
    let mut bios = vec![0u8; BIOS_SIZE];
    bios[..prog.len()].copy_from_slice(prog);
    for (at, b) in extra { bios[*at..*at + b.len()].copy_from_slice(b); }
    let mut blob = b"DCBB".to_vec();
    blob.extend(b"BOOT");
    blob.extend((bios.len() as u32).to_le_bytes());
    blob.extend(&bios);
    DcSystem::from_rom(&[0; 4], "dc_boot.bin", &blob).unwrap()
}

fn ram32(s: &DcSystem, a: u32) -> u32 { let i = (a & 0xFF_FFFF) as usize; u32::from_le_bytes(s.dc.ram[i..i + 4].try_into().unwrap()) }

#[test]
fn the_bios_runs_and_writes_ram() {
    let mut a = Asm::new();
    a.li(1, 0x8C00_1000).li(2, 0xCAFE_F00D).mov_l_store(2, 1).label("end").jump(0xA0, "end").nop();
    let mut s = system_with(&a.build(0xA000_0000), &[]);
    s.run_frame(0);
    assert_eq!(ram32(&s, 0x0C00_1000), 0xCAFE_F00D);
    // the same RAM through P2 and the 0D / 0E / 0F mirrors
    assert_eq!(s.dc.read(0xAC00_1000, 4), 0xCAFE_F00D);
    assert_eq!(s.dc.read(0x0F00_1000, 4), 0xCAFE_F00D);
}

/// Vblank in (Holly IML6) reaches the CPU: the handler at VBR + 0x600 counts, acknowledges
/// in SB_ISTNRM and returns with RTE.
#[test]
fn vblank_interrupt_reaches_the_cpu() {
    let mut a = Asm::new();
    a.li(0, 0x8000_1000).op(0x402E)               // VBR = 80001000 (BIOS offset 1000)
        .li(1, 0xA05F_6930).li(2, pvr::IST_VBL_IN).mov_l_store(2, 1) // IML6NRM = vblank in
        .li(0, 0x4000_0000).op(0x400E)            // SR = MD only: BL off, IMASK 0
        .label("spin").jump(0xA0, "spin").nop();
    let mut h = Asm::new();
    h.li(1, 0x8C00_0100).mov_l_load(1, 2).add_i(2, 1).mov_l_store(2, 1) // count
        .li(1, 0xA05F_6900).li(2, pvr::IST_VBL_IN).mov_l_store(2, 1)     // acknowledge
        .op(0x002B).nop();                                              // RTE
    let mut s = system_with(&a.build(0xA000_0000), &[(0x1600, h.build(0x8000_1600))]);
    for _ in 0..4 { s.run_frame(0); }
    let n = ram32(&s, 0x0C00_0100) as u64;
    let fields = s.dc.pvr.frames;
    assert!(fields >= 4 && (fields - 1..=fields).contains(&n), "one interrupt per field: {n} for {fields} fields");
    assert_eq!(s.dc.chip.intevt, 0x320, "IRL level 6");
}

/// TMU0 underflows raise TUNI0 (INTEVT 400) at the IPRA priority.
#[test]
fn timer_interrupt() {
    let mut a = Asm::new();
    a.li(0, 0x8000_1000).op(0x402E)
        .li(1, 0xFFD0_0004).li(2, 0xF000).op(0x2121) // IPRA: TMU0 priority 15 (MOV.W R2,@R1)
        .li(1, 0xFFD8_0008).li(2, 999).mov_l_store(2, 1)   // TCOR0
        .li(1, 0xFFD8_000C).mov_l_store(2, 1)              // TCNT0
        .li(1, 0xFFD8_0010).li(2, 0x20).op(0x2121)         // TCR0: /4, UNIE
        .li(1, 0xFFD8_0004).li(2, 1).op(0x2120)            // TSTR (MOV.B): start
        .li(0, 0x4000_0000).op(0x400E)
        .label("spin").jump(0xA0, "spin").nop();
    let mut h = Asm::new();
    h.li(1, 0x8C00_0100).mov_l_load(1, 2).add_i(2, 1).mov_l_store(2, 1)
        .li(1, 0xFFD8_0010).li(2, 0x20).op(0x2121)          // clear UNF
        .op(0x002B).nop();
    let mut s = system_with(&a.build(0xA000_0000), &[(0x1600, h.build(0x8000_1600))]);
    s.run_frame(0);
    // the timer underflows every 1000 x 16 cycles (TCOR 999, Pφ / 4, Pφ = CPU / 4)
    let n = ram32(&s, 0x0C00_0100) as u64;
    let want = s.dc.now / 16_000;
    assert!(n + 1 >= want && n <= want, "{n} underflows in {} cycles", s.dc.now);
    assert_eq!(s.dc.chip.intevt, 0x400);
}

fn bus() -> Dc { Dc::new(vec![0; BIOS_SIZE], &[], 0) }

#[test]
fn maple_get_condition_reads_the_pad() {
    let mut d = bus();
    d.pads[0] = Some(dc_pad(&pad::pads(pad::B | pad::RIGHT, 0)[0], None));
    // table at 0C010000: one entry, last, port A, 2 words; reply to 0C010100
    let t = 0x8C01_0000;
    for (k, w) in [0x8000_0001u32, 0x0C01_0100, 0x09 | 0x20 << 8 | 1 << 24, 0x0100_0000].iter().enumerate() {
        d.write(t + 4 * k as u32, *w, 4);
    }
    d.write(0xA05F_6C04, 0x0C01_0000, 4); // MDSTAR
    d.write(0xA05F_6C14, 1, 4);           // MDEN
    d.write(0xA05F_6C18, 1, 4);           // MDST
    let r: Vec<u32> = (0..4).map(|k| d.read(0x8C01_0100 + 4 * k, 4)).collect();
    assert_eq!(r[0] & 0xFF, 8, "data transfer");
    assert_eq!(r[0] >> 24, 3, "3 words");
    assert_eq!(r[0] >> 16 & 0xFF, 0x21, "from the controller on port A, a memory card in its first slot");
    assert_eq!(r[1], 0x0100_0000);
    assert_eq!(r[2] & 0xFFFF, (0xFFFF & !maple::BTN_A & !maple::BTN_RIGHT) as u32);
    assert_eq!(r[3] & 0xFF, 0xFF, "stick right");
    // the DMA-done interrupt comes a little later
    assert_eq!(d.holly.istnrm() & holly::IST_DMA_MAPLE, 0);
    d.now += 30_000;
    d.service();
    assert_ne!(d.holly.istnrm() & holly::IST_DMA_MAPLE, 0);
    // port C has nothing plugged in
    d.write(t, 0x8002_0001, 4);
    d.write(0xA05F_6C18, 1, 4);
    assert_eq!(d.read(0x8C01_0100, 4), 0xFFFF_FFFF);
}

/// The memory card answers at sub-unit 1 of port A: its device info, then a block read.
#[test]
fn maple_reaches_the_memory_card() {
    let mut d = bus();
    let t = 0x8C01_0000;
    let dma = |d: &mut Dc, frame: &[u32]| -> Vec<u32> {
        d.write(t, 0x8000_0000 | (frame.len() as u32 - 1), 4);
        d.write(t + 4, 0x0C01_0100, 4);
        for (k, w) in frame.iter().enumerate() { d.write(t + 8 + 4 * k as u32, *w, 4); }
        d.write(0xA05F_6C04, 0x0C01_0000, 4);
        d.write(0xA05F_6C14, 1, 4);
        d.write(0xA05F_6C18, 1, 4);
        let n = d.read(0x8C01_0100, 4) >> 24;
        (0..=n).map(|k| d.read(0x8C01_0100 + 4 * k, 4)).collect()
    };
    let r = dma(&mut d, &[0x01 | 0x01 << 8]);
    assert_eq!(r[0] & 0xFF, 0x05, "device info");
    assert_eq!(r[0] >> 16 & 0xFF, 0x01, "from A1");
    assert_eq!(r[1], 0x0E00_0000, "storage, LCD, clock");
    let r = dma(&mut d, &[0x0B | 0x01 << 8 | 2 << 24, vmu::FUNC_STORAGE, u32::from_le_bytes([0, 0, 0, 255])]);
    assert_eq!(r[0] & 0xFF, 0x08);
    assert_eq!(r.len(), 1 + 2 + 128);
    assert_eq!(r[3], 0x5555_5555, "the root block's format mark");
    // A2 is empty; nothing behind a card when it's unplugged
    assert_eq!(dma(&mut d, &[0x01 | 0x02 << 8])[0], 0xFFFF_FFFF);
    d.vmu_on = false;
    assert_eq!(dma(&mut d, &[0x01 | 0x01 << 8])[0], 0xFFFF_FFFF);
}

/// Sort DMA: two chains from a 32-bit start table (the second block reached by a link),
/// fed to the TA in order; then the sort-DMA end, after the list end it carried.
#[test]
fn sort_dma_feeds_linked_blocks_to_the_ta() {
    let mut d = bus();
    let base = 0x8C10_0000u32;
    let put = |d: &mut super::bus::Dc, a: u32, w: &[u32]| for (k, v) in w.iter().enumerate() { d.write(a + 4 * k as u32, *v, 4); };
    // block A at +0x100: header (size 2 = 64 bytes, next link 0x200) and a vertex
    put(&mut d, base + 0x100, &[4 << 29, 0, 0, 0, 0, 0, 2, 0x200]);
    put(&mut d, base + 0x120, &[7 << 29 | 1 << 28, 0, 0, 0, 0, 0, 0, 0]);
    // block B at +0x200: header, vertex, end of chain (link 1)
    put(&mut d, base + 0x200, &[4 << 29, 0, 0, 0, 0, 0, 2, 1]);
    put(&mut d, base + 0x220, &[7 << 29 | 1 << 28, 0, 0, 0, 0, 0, 0, 0]);
    // block C at +0x300: the end of the opaque list, alone (size 1), end of chain
    put(&mut d, base + 0x300, &[0, 0, 0, 0, 0, 0, 1, 1]);
    // start table: chain at 0x100, chain at 0x300, end of DMA
    put(&mut d, base + 0x1000, &[0x100, 0x300, 2]);
    d.write(0xA05F_6810, 0x0C10_1000, 4);  // SDSTAW
    d.write(0xA05F_6814, 0x0C10_0000, 4);  // SDBAAW
    d.write(0xA05F_6818, 1, 4);            // SDWLT: 32-bit links
    d.write(0xA05F_681C, 0, 4);            // SDLAS: links in bytes
    d.write(0xA05F_6820, 1, 4);            // SDST: go
    assert_eq!((d.pvr.ta.params, d.pvr.ta.vertices), (2, 2), "both polygons reached the TA");
    let (mut sort_end, mut list_end) = (None, None);
    for t in 0..100 {
        d.now += 10;
        d.service();
        let ist = d.holly.istnrm();
        if ist & holly::IST_DMA_SORT != 0 { sort_end.get_or_insert(t); }
        if ist & pvr::IST_LIST_END[0] != 0 { list_end.get_or_insert(t); }
    }
    assert!(list_end.is_some() && sort_end.is_some(), "{list_end:?} {sort_end:?}");
}

/// A long channel-2 DMA ending a list: the DMA-end interrupt comes before the list end,
/// whose data arrives last (NBA 2K2 hangs at "Loading" the other way round).
#[test]
fn ch2_dma_end_comes_before_the_list_end_it_carries() {
    let mut d = bus();
    let n = 0x1000; // words: 16 KB of vertices, then the end of list
    let mut words = vec![0u32; n + 8];
    words[0] = 4 << 29;                    // polygon header, opaque list
    for k in (8..n).step_by(8) { words[k] = 7 << 29; }
    words[n - 8] = 7 << 29 | 1 << 28;      // last vertex: end of strip
    words[n] = 0;                          // end of list
    for (k, w) in words.iter().enumerate() { d.write(0x8C02_0000 + 4 * k as u32, *w, 4); }
    d.write(0xFFA0_0020, 0x0C02_0000, 4);  // SAR2
    d.write(0xA05F_6800, 0x1000_0000, 4);  // C2DSTAT: TA FIFO
    d.write(0xA05F_6804, 4 * words.len() as u32, 4);
    d.write(0xA05F_6808, 1, 4);
    let (mut dma_end, mut list_end) = (None, None);
    for t in 0..400 {
        d.now += 50;
        d.service();
        let ist = d.holly.istnrm();
        if ist & holly::IST_DMA_CH2 != 0 { dma_end.get_or_insert(t); }
        if ist & pvr::IST_LIST_END[0] != 0 { list_end.get_or_insert(t); }
    }
    let (dma_end, list_end) = (dma_end.expect("DMA end"), list_end.expect("list end"));
    assert!(dma_end < list_end, "DMA end at {dma_end}, list end at {list_end} (x50 cycles)");
}

/// Channel-2 DMA feeds the tile accelerator: a polygon list's end raises its interrupt.
#[test]
fn ch2_dma_to_the_ta_ends_a_list() {
    let mut d = bus();
    let mut words = vec![0u32; 24];
    words[0] = 4 << 29 | 0 << 24;         // polygon header, opaque list, packed colour
    words[8] = 7 << 29;                    // vertex (32 bytes)
    words[16] = 0;                         // end of list
    for (k, w) in words.iter().enumerate() { d.write(0x8C02_0000 + 4 * k as u32, *w, 4); }
    d.write(0xFFA0_0020, 0x0C02_0000, 4);  // SAR2
    d.write(0xA05F_6800, 0x1000_0000, 4);  // C2DSTAT: TA FIFO
    d.write(0xA05F_6804, 96, 4);           // C2DLEN
    d.write(0xA05F_6808, 1, 4);            // C2DST
    assert_eq!(d.chip.dma[2].sar, 0x0C02_0060);
    assert_eq!(d.holly.r(holly::SB_C2DST), 0);
    d.now += 1_000;
    d.service();
    let ist = d.holly.istnrm();
    assert_ne!(ist & pvr::IST_LIST_END[0], 0, "opaque list end");
    assert_ne!(ist & holly::IST_DMA_CH2, 0);
    assert_eq!((d.pvr.ta.params, d.pvr.ta.vertices, d.pvr.ta.list), (1, 1, None));
    // acknowledging clears it
    d.write(0xA05F_6900, pvr::IST_LIST_END[0], 4);
    assert_eq!(d.holly.istnrm() & pvr::IST_LIST_END[0], 0);
}

#[test]
fn ta_parameter_sizes() {
    let mut t = pvr::Ta::default();
    let blk = |w0: u32| { let mut b = [0u32; 8]; b[0] = w0; b };
    // translucent list, textured, floating colour: 32-byte header, 64-byte vertices
    t.block(&blk(4 << 29 | 2 << 24 | 1 << 4 | 8));
    t.block(&blk(7 << 29));
    t.block(&blk(0)); // second half of the vertex, not an end of list
    assert_eq!(t.list, Some(2));
    assert_eq!(t.block(&blk(0)), pvr::IST_LIST_END[2]);
    // intensity with offset colour: 64-byte header
    t.block(&blk(4 << 29 | 4 << 24 | 2 << 4 | 4 | 8));
    t.block(&blk(0));
    assert_eq!(t.list, Some(4), "the header's second half isn't an end of list");
    // sprites: 64-byte vertices
    assert_eq!(t.block(&blk(0)), pvr::IST_LIST_END[4]);
    t.block(&blk(5 << 29 | 2 << 24));
    t.block(&blk(7 << 29));
    t.block(&blk(0));
    assert_eq!(t.block(&blk(0)), pvr::IST_LIST_END[2]);
}

#[test]
fn store_queue_flush() {
    let mut d = bus();
    d.write(0xFF00_0038, 3 << 2, 4); // QACR0: area 3 (RAM)
    for k in 0..8 { d.write(0xE000_0000 + 4 * k, 0x1111_0000 + k, 4); }
    d.write(0xE000_0020, 0xBBBB, 4); // queue 1
    d.pref(0xE012_3440);              // queue 0 -> 0C123440
    for k in 0..8 { assert_eq!(d.read(0x8C12_3440 + 4 * k, 4), 0x1111_0000 + k); }
    assert_eq!(d.read(0x8C12_3460, 4), 0, "queue 1 not flushed");
}

#[test]
fn framebuffer_output_565_and_32_bit_view() {
    let mut d = bus();
    // 640 x 480 RGB565 at 0 in the 32-bit view: a red pixel at (1, 2)
    let stride = 640 * 2;
    d.write(0xA500_0000 + 2 * stride + 2, 0xF800, 2);
    d.write(0xA500_0000 + 2 * stride + 4, 0x07E0, 2); // green at (2, 2)
    d.write(0xA500_0000 + 2 * stride + 6, 0x001F, 2); // blue at (3, 2)
    d.write(0xA05F_8044, 1 | 1 << 2, 4);                       // FB_R_CTRL: on, 565
    d.write(0xA05F_805C, (320 - 1) | (480 - 1) << 10 | 1 << 20, 4); // FB_R_SIZE
    d.write(0xA05F_8050, 0, 4);                                // FB_R_SOF1
    d.write(0xA05F_80E8, 0, 4);                                // VO_CONTROL: no pixel double
    let mut out = vec![0; OUT_W * OUT_H * 4];
    d.pvr.output(&d.vram, &mut out);
    let px = |x: usize, y: usize| &out[(y * OUT_W + x) * 4..(y * OUT_W + x) * 4 + 4];
    assert_eq!(px(1, 2), &[0xF8, 0, 0, 0xFF]);
    assert_eq!(px(0, 2), &[0, 0, 0, 0xFF]);
    assert_eq!(px(2, 2), &[0, 0xFC, 0, 0xFF]);
    assert_eq!(px(3, 2), &[0, 0, 0xF8, 0xFF]);
    // the 32-bit view interleaves the two banks: word 1 of bank 0 is linear byte 8
    d.write(0xA500_0004, 0xAABB_CCDD, 4);
    assert_eq!(d.read(0xA400_0008, 4), 0xAABB_CCDD);
    d.write(0xA540_0000, 0x1234_5678, 4);
    assert_eq!(d.read(0xA400_0004, 4), 0x1234_5678, "bank 1");
}

#[test]
fn sync_generator_lines_and_interrupts() {
    let mut p = pvr::Pvr::new();
    // NTSC: 263 lines, vblank in at line 260, out at 21
    p.write(pvr::SPG_CONTROL, 0x40);
    p.write(pvr::SPG_VBLANK_INT, 0x0015_0104);
    let mut seen = Vec::new();
    for _ in 0..263 {
        let ev = p.next_line();
        if ev.ist & pvr::IST_VBL_IN != 0 { seen.push(("in", p.vpos)); }
        if ev.ist & pvr::IST_VBL_OUT != 0 { seen.push(("out", p.vpos)); }
    }
    assert_eq!(seen, vec![("out", 0x15), ("in", 0x104)]);
    assert_eq!(p.read(pvr::SPG_STATUS) & 0x3FF, p.vpos);
    // about 60 fields a second
    let fps = 200_000_000.0 / (p.line_cycles() * p.lines() as u64) as f64;
    assert!((59.0..61.0).contains(&fps), "{fps}");
}

#[test]
fn holly_routes_levels_to_irl() {
    let mut d = bus();
    d.holly.raise(pvr::IST_VBL_OUT);
    d.update_irq();
    assert_eq!(d.irq, None, "masked");
    d.write(0xA05F_6920, pvr::IST_VBL_OUT, 4); // IML4NRM
    assert_eq!(d.irq, Some((4, 0x360)));
    d.write(0xA05F_6930, pvr::IST_VBL_OUT, 4); // IML6NRM wins
    assert_eq!(d.irq, Some((6, 0x320)));
    // the GD-ROM's interrupt is external bit 0
    d.write(0xA05F_6910, 0, 4);
    d.write(0xA05F_6920, 0, 4);
    d.write(0xA05F_6930, 0, 4);
    d.write(0xA05F_6914, holly::EXT_GDROM, 4); // IML2EXT
    d.write(0xA05F_709C, 0xEF, 1);             // GD-ROM SET FEATURES: completes with an interrupt
    assert_eq!(d.irq, Some((2, 0x3A0)));
    assert_ne!(d.holly.istnrm() & 1 << 30, 0, "summary bit");
    d.read(0xA05F_709C, 1);                    // status read acknowledges
    assert_eq!(d.irq, None);
}

#[test]
fn bios_flash_and_clock_through_the_bus() {
    let mut d = bus();
    d.write(0xA020_5555, 0xAA, 1);
    d.write(0xA020_2AAA, 0x55, 1);
    d.write(0xA020_5555, 0xA0, 1);
    d.write(0xA021_A002, 0x31, 1);
    assert_eq!(d.read(0xA021_A002, 1), 0x31);
    assert!(d.flash.dirty);
    let t = d.read(0xA071_0000, 4) << 16 | d.read(0xA071_0004, 4);
    assert_eq!(t as u64, aica::EPOCH_1950);
    // AICA registers and the ARM7 reset
    assert_eq!(d.read(0xA070_2C00, 4), 1);
    d.write(0xA070_2C00, 0, 4);
    assert!(!d.aica.arm_reset);
    d.write(0xA080_0010, 0x1234_5678, 4);
    assert_eq!(d.read(0xA280_0010, 4), 0x1234_5678, "sound RAM mirror");
}

#[test]
fn port_a_reports_the_cable() {
    let mut d = bus();
    // the US BIOS's check (Greg's trace): PCTRA ...F8, PDTRA 0303 -> 0303; then PCTRA
    // ...FB, PDTRA 0302 -> 0300 (else it halts)
    d.write(0xFF80_002C, 0x000A_03F8, 4);
    d.write(0xFF80_0030, 0x0303, 2);
    assert_eq!(d.read(0xFF80_0030, 2), 0x0303, "composite, pins high");
    d.write(0xFF80_002C, 0x000A_03FB, 4);
    assert_eq!(d.read(0xFF80_0030, 2), 0x0303);
    d.write(0xFF80_0030, 0x0302, 2);
    assert_eq!(d.read(0xFF80_0030, 2), 0x0300);
    d.chip.cable = 0;
    assert_eq!(d.read(0xFF80_0030, 2) >> 8 & 3, 0, "VGA");
}

#[test]
fn unknown_addresses_are_logged() {
    let mut d = bus();
    d.read(0xA060_0800 + 0x100_0000, 4);
    d.write(0xB800_0000, 1, 4);
    assert!(d.unknown.contains_key(&0x0160_0800));
    assert!(d.unknown.get(&0xB800_0000).is_some());
}

#[test]
fn save_state_round_trip() {
    let mut a = Asm::new();
    a.li(1, 0x8C00_1000).label("l").mov_l_load(1, 2).add_i(2, 1).mov_l_store(2, 1).jump(0xA0, "l").nop();
    let mut s = system_with(&a.build(0xA000_0000), &[]);
    s.run_frame(0);
    let st = s.save_state();
    let n = ram32(&s, 0x0C00_1000);
    s.run_frame(0);
    assert!(ram32(&s, 0x0C00_1000) > n);
    s.load_state(&st).unwrap();
    assert_eq!(ram32(&s, 0x0C00_1000), n);
    assert_eq!(s.dc.bios.len(), BIOS_SIZE, "BIOS kept");
}

#[test]
fn missing_bios_says_what_to_do() {
    let e = DcSystem::from_rom(b"MCDP/x/game.gdi", "game.gdi", b"DCBB").err().unwrap();
    assert!(e.contains("dc_boot.bin"));
}

/// cargo test --release --lib dc_speed -- --ignored --nocapture
#[test]
#[ignore]
fn dc_speed() {
    let mut a = Asm::new();
    a.li(1, 0x8C00_1000).label("l").mov_l_load(1, 2).add_i(2, 1).mov_l_store(2, 1).jump(0xA0, "l").nop();
    let mut s = system_with(&a.build(0xA000_0000), &[]);
    let t = std::time::Instant::now();
    let n = std::env::var("DC_N").ok().and_then(|v| v.parse().ok()).unwrap_or(60);
    for _ in 0..n { s.run_frame(0); }
    let el = t.elapsed().as_secs_f64();
    println!("{n} fields in {el:.2} s ({:.0}% of real time)", 100.0 * n as f64 / 60.0 / el);
}

/// Like `dc_speed`, a geometry-style loop: FMOV loads, FTRV, FMUL / FADD, stores, DT / BF.
#[test]
#[ignore]
fn dc_speed_fpu() {
    let mut a = Asm::new();
    a.label("s").li(1, 0x8C00_1000).li(2, 0x8C00_4000).li(3, 256).label("l");
    for op in [0xF019, 0xF119, 0xF219, 0xF319, 0xF1FD, 0xF102, 0xF320, 0xF20A, 0x7204, 0xF21A, 0x7204, 0xF23A, 0x7204, 0x4310] { a.op(op); }
    a.jump(0x8B, "l").nop().jump(0xA0, "s").nop();
    let mut s = system_with(&a.build(0xA000_0000), &[]);
    let t = std::time::Instant::now();
    let n = std::env::var("DC_N").ok().and_then(|v| v.parse().ok()).unwrap_or(60);
    for _ in 0..n { s.run_frame(0); }
    let el = t.elapsed().as_secs_f64();
    println!("{n} fields in {el:.2} s ({:.0}% of real time)", 100.0 * n as f64 / 60.0 / el);
}

#[test]
fn polling_loops_skip_to_the_next_event_and_counted_loops_dont() {
    // poll a RAM word until non-zero: MOV.L @R1,R0 / TST R0,R0 / BT loop / NOP
    let mut a = Asm::new();
    a.li(1, 0x8C00_1000).label("l").mov_l_load(1, 0).op(0x2008).jump(0x89, "l").nop().label("x").jump(0xA0, "x").nop();
    let mut s = system_with(&a.build(0xA000_0000), &[]);
    s.run_frame(0);
    let frac = s.prof.idle_cycles as f64 / s.dc.now as f64;
    assert!(frac > 0.9, "mostly skipped: {frac}");
    // memory changes (as an interrupt handler or the ARM7 would): the loop ends
    s.dc.ram[0x1000] = 1;
    s.run_frame(0);
    assert!(s.cpu.pc >= 0xA000_000A, "left the loop: {:08X}", s.cpu.pc);
    // a DT delay loop runs normally
    let mut a = Asm::new();
    a.li(1, 0x7FFF_FFFF).label("d").op(0x4110).jump(0x8B, "d").nop();
    let mut s = system_with(&a.build(0xA000_0000), &[]);
    s.run_frame(0);
    assert_eq!(s.prof.idle_cycles, 0);
    // with the option off nothing is skipped
    let mut a = Asm::new();
    a.li(1, 0x8C00_1000).label("l").mov_l_load(1, 0).op(0x2008).jump(0x89, "l").nop();
    let mut s = system_with(&a.build(0xA000_0000), &[]);
    s.set_option("dc_idle_skip", 0);
    s.run_frame(0);
    assert_eq!(s.prof.idle_cycles, 0);
}

#[test]
fn cpu_ratio_counts_cycles_per_instruction() {
    for cpi in [1u64, 2, 3] {
        let mut a = Asm::new();
        a.li(1, 0x8C00_1000).label("l").mov_l_load(1, 2).add_i(2, 1).mov_l_store(2, 1).jump(0xA0, "l").nop();
        let mut s = system_with(&a.build(0xA000_0000), &[]);
        s.set_option("dc_cpu_ratio", cpi as i64);
        s.run_frame(0);
        // the loop is 5 instructions (4 + the slot) and adds 1 per pass
        let passes = ram32(&s, 0x8C00_1000) as u64;
        let expect = s.dc.now / (5 * cpi);
        assert!(passes.abs_diff(expect) < expect / 50, "cpi {cpi}: {passes} vs {expect}");
    }
}

/// With the MMU on, a load from an unmapped page puts the registers back (even a
/// post-increment of the address register) and takes the TLB-miss exception at VBR+0x400
/// with TEA / PTEH set; after LDTLB the same instruction runs through the mapping.
#[test]
fn mmu_tlb_miss_undoes_the_instruction_then_ldtlb_maps_it() {
    use crate::common::cpu::sh4::{Sh4, Sh4Bus};
    let mut d = bus();
    let mut cpu = Sh4::new();
    d.write(0x8C01_0000, 0x6116, 2); // mov.l @r1+,r1
    d.write(0x8C02_0010, 0xDEAD_BEEF, 4);
    cpu.vbr = 0x8C00_0000;
    cpu.pc = 0x8C01_0000;
    cpu.r[1] = 0x0000_4010;
    d.write(0xFF00_0010, 1, 4); // MMUCR.AT
    cpu.step(&mut d);
    assert_eq!(cpu.pc, 0x8C00_0400, "TLB miss vector");
    assert_eq!(cpu.r[1], 0x0000_4010, "the post-increment was undone");
    assert_eq!(cpu.spc, 0x8C01_0000);
    assert_eq!(d.chip.expevt, 0x040);
    assert_eq!(d.chip.tea, 0x4010);
    assert_eq!(d.chip.pteh & 0xFFFF_FC00, 0x4000);
    // the handler: PTEL = PPN 0C020000, 4 KB, RW, dirty, valid; LDTLB; RTE
    d.chip.ptel = 0x0C02_0000 | 0x100 | 0x10 | 0x60 | 4;
    d.ldtlb();
    cpu.set_sr(cpu.ssr);
    cpu.pc = cpu.spc;
    cpu.step(&mut d);
    assert_eq!(cpu.r[1], 0xDEAD_BEEF);
    assert_eq!(cpu.pc, 0x8C01_0002);
}

/// Like `dc_speed`, with the code in main RAM as games have it: loads, stores, adds, a
/// shift, compares, a taken and an untaken branch, a delayed branch.
/// cargo test --release --lib dc_speed_ram -- --ignored --nocapture
#[test]
#[ignore]
fn dc_speed_ram() {
    let mut s = system_with(&[0u8; 16], &[]);
    let code: [u16; 13] = [0x6212, 0x7201, 0x2122, 0x5311, 0x332C, 0x1131, 0x4208, 0x3230, 0x8901, 0x0009, 0x4410, 0xAFF3, 0x0009];
    for (k, w) in code.iter().enumerate() { s.dc.ram[0x1_0000 + 2 * k..0x1_0002 + 2 * k].copy_from_slice(&w.to_le_bytes()); }
    s.cpu.pc = 0x8C01_0000;
    s.cpu.r[1] = 0x8C00_1000;
    if std::env::var("DC_JIT").is_ok() { s.set_option("dc_jit", 1); }
    let t = std::time::Instant::now();
    let n = std::env::var("DC_N").ok().and_then(|v| v.parse().ok()).unwrap_or(60);
    for _ in 0..n { s.run_frame(0); }
    let el = t.elapsed().as_secs_f64();
    assert!(s.cpu.pc >= 0x8C01_0000 && s.cpu.pc < 0x8C01_001A, "still in the loop: {:08X}", s.cpu.pc);
    println!("{n} fields in {el:.2} s ({:.0}% of real time)", 100.0 * n as f64 / 60.0 / el);
}

/// A memory card swapped in from the frontend: the slot is empty for a moment (the game
/// sees the card go), then holds the new card; the old one comes back out as card-file
/// bytes, and a game's write marks the card for saving.
#[test]
fn memory_cards_swap() {
    // a program that spins (BRA to itself)
    let mut s = system_with(&[0xFE, 0xAF, 0x09, 0x00], &[]);
    let old = s.memory_card().unwrap();
    let mut other = vmu::Vmu::new();
    other.data[0] = 0x5A; // something to tell it by
    assert!(s.insert_memory_card(&other.data, true));
    assert!(!s.dc.vmu_on, "out of the slot first");
    for _ in 0..40 { s.run_frame(0); }
    assert!(s.dc.vmu_on, "back in");
    assert_eq!(s.memory_card().unwrap()[0], 0x5A);
    assert_ne!(old[0], 0x5A);
    // the save file follows the card in the slot
    assert_eq!(s.get_sram().unwrap()[flash::SIZE], 0x5A);
    // an empty card file: a new formatted card
    s.insert_memory_card(&[], false);
    assert_eq!(vmu::Vmu::summary(&s.memory_card().unwrap()), Some((200, vec![])));
    assert!(!s.memory_card_dirty());
    s.dc.vmu.dirty = true;
    s.run_frame(0);
    assert!(s.memory_card_dirty());
    assert!(!s.memory_card_dirty(), "asking clears it");
}

/// Skies of Arcadia: the stick walks, the d-pad turns the camera, so they're separate
#[test]
fn the_stick_and_the_d_pad_are_separate_with_a_controller() {
    let p = pad::pads(pad::LEFT, 0)[0];
    let with = dc_pad(&p, Some([0xC0, 0x20]));
    assert_eq!(with.buttons & maple::BTN_LEFT, 0, "d-pad left pressed (active low)");
    assert_eq!((with.x, with.y), (0xC0, 0x20), "the stick is the controller's own");
    let keyboard = dc_pad(&p, None);
    assert_eq!((keyboard.x, keyboard.y), (0, 0x80), "no controller: the d-pad pushes the stick");
    let centred = dc_pad(&pad::pads(0, 0)[0], Some([0x80, 0x80]));
    assert_eq!(centred.buttons, 0xFFFF, "and the stick presses no d-pad button");
}
