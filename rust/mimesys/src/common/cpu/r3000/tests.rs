use super::*;

/// 2 MB of RAM at 0, 512 KB of "BIOS" at 1FC00000
struct Mem {
    ram: Vec<u8>,
    bios: Vec<u8>,
    io_writes: Vec<(u32, u32)>,
}

impl Mem {
    fn new() -> Self { Self { ram: vec![0; 0x20_0000], bios: vec![0; 0x8_0000], io_writes: vec![] } }
    fn slot(&mut self, pa: u32) -> Option<&mut [u8]> {
        match pa {
            0..=0x1F_FFFF => Some(&mut self.ram[pa as usize..]),
            0x1FC0_0000..=0x1FC7_FFFF => Some(&mut self.bios[(pa - 0x1FC0_0000) as usize..]),
            _ => None,
        }
    }
    fn put(&mut self, pa: u32, words: &[u32]) {
        for (k, w) in words.iter().enumerate() { self.write32(pa + 4 * k as u32, *w); }
    }
}

impl R3000Bus for Mem {
    fn read8(&mut self, pa: u32) -> u8 { self.slot(pa).map_or(0xFF, |s| s[0]) }
    fn read16(&mut self, pa: u32) -> u16 { self.slot(pa).map_or(0xFFFF, |s| u16::from_le_bytes([s[0], s[1]])) }
    fn read32(&mut self, pa: u32) -> u32 { self.slot(pa).map_or(!0, |s| u32::from_le_bytes(s[..4].try_into().unwrap())) }
    fn write8(&mut self, pa: u32, v: u8) { match self.slot(pa) { Some(s) => s[0] = v, None => self.io_writes.push((pa, v as u32)) } }
    fn write16(&mut self, pa: u32, v: u16) { match self.slot(pa) { Some(s) => s[..2].copy_from_slice(&v.to_le_bytes()), None => self.io_writes.push((pa, v as u32)) } }
    fn write32(&mut self, pa: u32, v: u32) { match self.slot(pa) { Some(s) => s[..4].copy_from_slice(&v.to_le_bytes()), None => self.io_writes.push((pa, v)) } }
}

// ---- a few encoders
fn r(rs: u32, rt: u32, rd: u32, sa: u32, f: u32) -> u32 { rs << 21 | rt << 16 | rd << 11 | sa << 6 | f }
fn i(op: u32, rs: u32, rt: u32, imm: u16) -> u32 { op << 26 | rs << 21 | rt << 16 | imm as u32 }
fn addiu(rt: u32, rs: u32, v: i16) -> u32 { i(0x09, rs, rt, v as u16) }
fn lui(rt: u32, v: u16) -> u32 { i(0x0F, 0, rt, v) }
fn ori(rt: u32, rs: u32, v: u16) -> u32 { i(0x0D, rs, rt, v) }
fn addu(rd: u32, rs: u32, rt: u32) -> u32 { r(rs, rt, rd, 0, 0x21) }
fn lw(rt: u32, base: u32, off: i16) -> u32 { i(0x23, base, rt, off as u16) }
fn sw(rt: u32, base: u32, off: i16) -> u32 { i(0x2B, base, rt, off as u16) }
fn beq(rs: u32, rt: u32, off: i16) -> u32 { i(0x04, rs, rt, off as u16) }
fn mtc0(rt: u32, rd: u32) -> u32 { 0x4080_0000 | rt << 16 | rd << 11 }
fn mfc0(rt: u32, rd: u32) -> u32 { 0x4000_0000 | rt << 16 | rd << 11 }
const NOP: u32 = 0;

/// Load `prog` at 80001000 (RAM 1000) and run `n` instructions.
fn run(prog: &[u32], n: usize) -> (R3000, Mem) {
    let mut m = Mem::new();
    m.put(0x1000, prog);
    let mut c = R3000::new();
    c.cop0[SR] = 0;              // exceptions to 80000080
    c.jump(0x8000_1000);
    for _ in 0..n { c.step(&mut m); }
    (c, m)
}

#[test]
fn alu_and_shifts() {
    let prog = [
        lui(1, 0x1234), ori(1, 1, 0x5678),          // r1 = 12345678
        addiu(2, 0, -1),                             // r2 = FFFFFFFF
        addu(3, 1, 2),                               // 12345677
        r(1, 2, 4, 0, 0x23),                         // SUBU r4 = r1 - r2 = 12345679
        r(1, 2, 5, 0, 0x24),                         // AND
        r(1, 0, 6, 0, 0x27),                         // NOR r1, r0
        r(2, 1, 7, 0, 0x2A),                         // SLT -1 < x = 1
        r(2, 1, 8, 0, 0x2B),                         // SLTU FFFFFFFF < x = 0
        i(0x0A, 2, 9, 0),                            // SLTI -1 < 0 = 1
        i(0x0B, 2, 10, 0xFFFF),                      // SLTIU FFFFFFFF < FFFFFFFF = 0
        r(0, 1, 11, 4, 0x00),                        // SLL 4
        r(0, 2, 12, 28, 0x02),                       // SRL 28 = F
        r(0, 2, 13, 28, 0x03),                       // SRA 28 = FFFFFFFF
        addiu(14, 0, 8),
        r(14, 1, 15, 0, 0x06),                       // SRLV by 8 = 00123456
        r(0, 1, 0, 0, 0x21),                         // write r0: stays 0
        i(0x0E, 1, 16, 0xFFFF),                      // XORI
    ];
    let (c, _) = run(&prog, prog.len());
    let g = &c.regs;
    assert_eq!(g[1], 0x1234_5678);
    assert_eq!(g[3], 0x1234_5677);
    assert_eq!(g[4], 0x1234_5679);
    assert_eq!(g[5], 0x1234_5678);
    assert_eq!(g[6], !0x1234_5678);
    assert_eq!((g[7], g[8], g[9], g[10]), (1, 0, 1, 0));
    assert_eq!(g[11], 0x2345_6780);
    assert_eq!((g[12], g[13]), (0xF, 0xFFFF_FFFF));
    assert_eq!(g[15], 0x0012_3456);
    assert_eq!(g[0], 0);
    assert_eq!(g[16], 0x1234_A987);
}

#[test]
fn load_delay_slot() {
    let mut prog = vec![
        lui(1, 0x8000), ori(1, 1, 0x1000),
        addiu(2, 0, 7),
        lw(2, 1, 0x100),                             // r2 <- 55 (lands after the next one)
        addu(3, 2, 0),                               // sees 7
        addu(4, 2, 0),                               // sees 55
        lw(5, 1, 0x100),
        addiu(5, 0, 9),                              // in the delay: its write wins
        addu(6, 5, 0),                               // 9
        lw(7, 1, 0x100),
        lw(7, 1, 0x104),                             // the second load replaces the first
        addu(8, 7, 0),                               // r7 old (0): the first load was dropped
        addu(9, 7, 0),                               // 66
        lw(10, 1, 0x100),
        i(0x22, 1, 10, 0x105),                       // LWL r10 merges with the load in flight
        NOP,
    ];
    prog.resize(0x40, NOP);
    prog.extend([55, 66, 0xAABBCCDD]);              // at 80001100
    let (c, _) = run(&prog, 16);
    let g = &c.regs;
    assert_eq!((g[3], g[4]), (7, 55));
    assert_eq!(g[6], 9);
    assert_eq!((g[8], g[9]), (0, 66));
    // LWL at 1105: the top two bytes of the word at 1104 (66) over 55's low half
    assert_eq!(g[10], (66 << 16) | 55);
}

#[test]
fn unaligned_loads_and_stores() {
    let mut prog = vec![
        lui(1, 0x8000),
        i(0x22, 1, 2, 0x1103), i(0x26, 1, 2, 0x1100),   // LWL 1103 + LWR 1100 = the word at 1100
        i(0x22, 1, 3, 0x1105), i(0x26, 1, 3, 0x1102),   // the word at 1102
        NOP,
        lui(4, 0x1122), ori(4, 4, 0x3344),
        i(0x2A, 1, 4, 0x1209), i(0x2E, 1, 4, 0x1206),   // SWL 1209 + SWR 1206: 11223344 at 1206
        NOP,
    ];
    prog.resize(0x40, NOP);
    prog.extend([0x4433_2211, 0x8877_6655]);
    let (c, m) = run(&prog, 11);
    assert_eq!(c.regs[2], 0x4433_2211);
    assert_eq!(c.regs[3], 0x6655_4433);
    assert_eq!(&m.ram[0x1206..0x120A], &[0x44, 0x33, 0x22, 0x11]);
    assert_eq!(m.ram[0x1205], 0);
    assert_eq!(m.ram[0x120A], 0);
}

#[test]
fn branches_jumps_and_links() {
    let prog = [
        addiu(1, 0, 1),
        beq(0, 0, 3),                                // -> 80001014
        addiu(2, 0, 2),                              // delay slot runs
        addiu(3, 0, 3),                              // skipped
        addiu(4, 0, 4),                              // skipped
        0x0C00_0408,                                 // 1014: JAL 80001020
        addiu(5, 0, 5),
        NOP,
        r(31, 0, 0, 0, 0x08),                        // 1020: JR r31 -> 101C
        i(0x01, 1, 0x10, 5),                         // BLTZAL r1 (1 >= 0: not taken, but links)
        NOP,
    ];
    let (c, _) = run(&prog, 7);
    let g = &c.regs;
    assert_eq!((g[1], g[2], g[3], g[4], g[5]), (1, 2, 0, 0, 5));
    assert_eq!(g[31], 0x8000_102C, "BLTZAL links even when not taken");
    assert_eq!(c.pc, 0x8000_101C, "JR's delay slot ran, then back after the JAL's slot");
}

#[test]
fn exceptions() {
    // overflow: nothing written, EPC at the ADD, code 12
    let (c, _) = run(&[lui(1, 0x7FFF), ori(1, 1, 0xFFFF), addiu(2, 0, 9), r(1, 1, 2, 0, 0x20)], 4);
    assert_eq!(c.regs[2], 9);
    assert_eq!(c.cop0[EPC], 0x8000_100C);
    assert_eq!((c.cop0[CAUSE] >> 2) & 31, EXC_OV);
    assert_eq!(c.pc, 0x8000_0080);

    // SYSCALL in a delay slot: BD set, EPC at the branch; the KU/IE stack pushes
    let mut m = Mem::new();
    m.put(0x1000, &[beq(0, 0, 4), 0x0000_000C]);
    let mut c = R3000::new();
    c.cop0[SR] = 0x0040_0001;                       // BEV, IEc
    c.jump(0x8000_1000);
    c.step(&mut m);
    c.step(&mut m);
    assert_eq!(c.cop0[CAUSE] >> 31, 1);
    assert_eq!((c.cop0[CAUSE] >> 2) & 31, EXC_SYSCALL);
    assert_eq!(c.cop0[EPC], 0x8000_1000);
    assert_eq!(c.pc, 0xBFC0_0180, "BEV: the ROM vector");
    assert_eq!(c.cop0[SR] & 0x3F, 0b000100, "IEc pushed to IEp");
    // RFE pops it back
    m.put(0x1FC0_0180, &[0x4200_0010]);
    c.step(&mut m);
    assert_eq!(c.cop0[SR] & 0x3F, 0b000001);

    // misaligned LW: AdEL with BadVaddr; misaligned SH: AdES
    let (c, _) = run(&[lui(1, 0x8000), lw(2, 1, 0x1001)], 2);
    assert_eq!((c.cop0[CAUSE] >> 2) & 31, EXC_ADEL);
    assert_eq!(c.cop0[BADVADDR], 0x8000_1001);
    let (c, _) = run(&[lui(1, 0x8000), i(0x29, 1, 0, 0x1001)], 2);
    assert_eq!((c.cop0[CAUSE] >> 2) & 31, EXC_ADES);
    // COP2 while SR.CU2 is clear: coprocessor unusable, CE = 2
    let (c, _) = run(&[0x4A00_0001], 1);
    assert_eq!((c.cop0[CAUSE] >> 2) & 31, EXC_CPU);
    assert_eq!((c.cop0[CAUSE] >> 28) & 3, 2);
    // reserved instruction
    let (c, _) = run(&[0xFC00_0000], 1);
    assert_eq!((c.cop0[CAUSE] >> 2) & 31, EXC_RI);
}

#[test]
fn interrupts() {
    let mut m = Mem::new();
    m.put(0x1000, &[addiu(1, 1, 1), beq(0, 0, -2), NOP]);
    let mut c = R3000::new();
    c.cop0[SR] = 0x0000_0401;                       // IM2 (bit 10), IEc
    c.jump(0x8000_1000);
    for _ in 0..5 { c.step(&mut m); }
    assert_eq!(c.pc & 0xFFFF_F000, 0x8000_1000, "nothing pending");
    c.set_irq(true);
    c.step(&mut m);
    assert_eq!(c.pc, 0x8000_0080);
    assert_eq!((c.cop0[CAUSE] >> 2) & 31, EXC_INT);
    assert_ne!(c.cause() & 0x400, 0, "IP2 shows the line");
    assert_eq!(c.cop0[SR] & 1, 0, "interrupts off in the handler");
    // masked: nothing taken
    let mut c = R3000::new();
    c.cop0[SR] = 0x0000_0001;
    c.jump(0x8000_1000);
    c.set_irq(true);
    c.step(&mut m);
    assert_eq!(c.pc, 0x8000_1004);
    // software interrupt through Cause bits 8-9
    let mut m = Mem::new();
    m.put(0x1000, &[addiu(1, 0, 0x100), mtc0(1, CAUSE as u32), NOP, NOP]);
    let mut c = R3000::new();
    c.cop0[SR] = 0x0000_0101;
    c.jump(0x8000_1000);
    for _ in 0..3 { c.step(&mut m); }
    assert_eq!(c.pc, 0x8000_0080);
}

#[test]
fn multiply_divide() {
    let prog = [
        addiu(1, 0, -3), addiu(2, 0, 7),
        r(1, 2, 0, 0, 0x18), r(0, 0, 3, 0, 0x12), r(0, 0, 4, 0, 0x10),     // MULT: lo -21, hi -1
        r(1, 2, 0, 0, 0x19), r(0, 0, 5, 0, 0x10),                          // MULTU hi = 6
        r(2, 1, 0, 0, 0x1A), r(0, 0, 6, 0, 0x12), r(0, 0, 7, 0, 0x10),     // DIV 7 / -3 = -2 r 1
        r(1, 0, 0, 0, 0x1A), r(0, 0, 8, 0, 0x12), r(0, 0, 9, 0, 0x10),     // DIV -3 / 0: lo 1, hi -3
        r(2, 0, 0, 0, 0x1B), r(0, 0, 10, 0, 0x12),                         // DIVU / 0: lo FFFFFFFF
        lui(11, 0x8000), addiu(12, 0, -1),
        r(11, 12, 0, 0, 0x1A), r(0, 0, 13, 0, 0x12), r(0, 0, 14, 0, 0x10), // 80000000 / -1
        addiu(15, 0, 42), r(15, 0, 0, 0, 0x13), r(0, 0, 16, 0, 0x12),      // MTLO / MFLO
    ];
    let (c, _) = run(&prog, prog.len());
    let g = &c.regs;
    assert_eq!((g[3] as i32, g[4] as i32), (-21, -1));
    assert_eq!(g[5], 6);
    assert_eq!((g[6] as i32, g[7] as i32), (-2, 1));
    assert_eq!((g[8], g[9] as i32), (1, -3));
    assert_eq!(g[10], 0xFFFF_FFFF);
    assert_eq!((g[13], g[14]), (0x8000_0000, 0));
    assert_eq!(g[16], 42);
    // MFLO straight after a DIV waits for it
    let (c, _) = run(&[addiu(1, 0, 5), r(1, 1, 0, 0, 0x1A), r(0, 0, 2, 0, 0x12)], 3);
    assert!(c.cycles >= 36);
}

#[test]
fn segments_scratchpad_and_isolation() {
    let prog = [
        lui(1, 0xA000), addiu(2, 0, 0x77),
        sw(2, 1, 0x200),                             // KSEG1 A0000200 = RAM 200
        lui(3, 0x0000), lw(4, 3, 0x200),            // read back through KUSEG
        lui(5, 0x1F80), sw(2, 5, 0x10),             // the scratchpad (KUSEG view)
        lui(6, 0x9F80), lw(7, 6, 0x10),             // and KSEG0
        lui(8, 1), mtc0(8, SR as u32),               // SR.IsC on
        addiu(9, 0, 0x55), sw(9, 1, 0x200),          // dropped
        mtc0(0, SR as u32), lw(10, 1, 0x200),
        mfc0(11, PRID as u32), NOP,
        lui(12, 0xFFFE), addiu(13, 0, 0xE988u16 as i16), sw(13, 12, 0x130), lw(14, 12, 0x130), NOP,
    ];
    let (c, m) = run(&prog, prog.len());
    let g = &c.regs;
    assert_eq!(m.ram[0x200], 0x77);
    assert_eq!(g[4], 0x77);
    assert_eq!(g[7], 0x77);
    assert_eq!(c.scratch[0x10], 0x77);
    assert_eq!(g[10], 0x77, "the store under IsC went nowhere");
    assert_eq!(g[11], 2, "PRId");
    assert_eq!(c.cache_ctrl, 0xFFFF_E988);
    assert_eq!(g[14], 0xFFFF_E988);
    assert!(m.io_writes.is_empty());
}

#[test]
fn gte_registers() {
    let prog = [
        lui(1, 0x4000), mtc0(1, SR as u32),         // CU2
        addiu(2, 0, -2),
        0x4882_0800,                                 // MTC2 r2, VZ0 (data 1)
        0x4803_0800,                                 // MFC2 r3, data 1: sign-extended
        NOP,
        addiu(4, 0, 0x7FFF),
        0x4884_E000,                                 // MTC2 r4, IRGB (28)
        0x4805_4800,                                 // MFC2 r5, IR1 (9) = 1F << 7
        0x4806_E800,                                 // MFC2 r6, ORGB (29)
        addiu(7, 0, 0x00FF),
        0x4887_F000,                                 // MTC2 r7, LZCS (30)
        0x4808_F800,                                 // MFC2 r8, LZCR = 24
        NOP,
    ];
    let (c, _) = run(&prog, prog.len());
    let g = &c.regs;
    assert_eq!(g[3] as i32, -2);
    assert_eq!(g[5], 0x1F << 7);
    assert_eq!(g[6], 0x7FFF);
    assert_eq!(g[8], 24);
}
