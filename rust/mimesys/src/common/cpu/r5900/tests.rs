use super::*;

/// 4 MB of RAM, physical 0
struct Ram(Vec<u8>);

impl EeBus for Ram {
    fn read8(&mut self, pa: u32) -> u8 { self.0[pa as usize] }
    fn read16(&mut self, pa: u32) -> u16 { u16::from_le_bytes(self.0[pa as usize..pa as usize + 2].try_into().unwrap()) }
    fn read32(&mut self, pa: u32) -> u32 { u32::from_le_bytes(self.0[pa as usize..pa as usize + 4].try_into().unwrap()) }
    fn write8(&mut self, pa: u32, v: u8) { self.0[pa as usize] = v; }
    fn write16(&mut self, pa: u32, v: u16) { self.0[pa as usize..pa as usize + 2].copy_from_slice(&v.to_le_bytes()); }
    fn write32(&mut self, pa: u32, v: u32) { self.0[pa as usize..pa as usize + 4].copy_from_slice(&v.to_le_bytes()); }
}

// ---- a tiny assembler
fn r(op: u32, rs: u32, rt: u32, rd: u32, sa: u32, f: u32) -> u32 { op << 26 | rs << 21 | rt << 16 | rd << 11 | sa << 6 | f }
fn i(op: u32, rs: u32, rt: u32, imm: i32) -> u32 { op << 26 | rs << 21 | rt << 16 | (imm as u32 & 0xFFFF) }
fn special(rs: u32, rt: u32, rd: u32, sa: u32, f: u32) -> u32 { r(0, rs, rt, rd, sa, f) }
fn mmi(rs: u32, rt: u32, rd: u32, sa: u32, f: u32) -> u32 { r(0x1C, rs, rt, rd, sa, f) }
fn cop1(fmt: u32, ft: u32, fs: u32, fd: u32, f: u32) -> u32 { r(0x11, fmt, ft, fs, fd, f) }
fn addiu(rt: u32, rs: u32, v: i32) -> u32 { i(0x09, rs, rt, v) }
fn lui(rt: u32, v: u32) -> u32 { i(0x0F, 0, rt, v as i32) }
fn ori(rt: u32, rs: u32, v: u32) -> u32 { i(0x0D, rs, rt, v as i32) }
fn mtc0(rt: u32, rd: u32) -> u32 { r(0x10, 4, rt, rd, 0, 0) }
const NOP: u32 = 0;
const BASE: u32 = 0x8000_1000;

/// Load `code` at BASE (KSEG0), run `steps` instructions
fn run(code: &[u32], steps: usize) -> (R5900, Ram) {
    let mut ram = Ram(vec![0; 0x40_0000]);
    for (k, w) in code.iter().enumerate() { ram.write32(0x1000 + 4 * k as u32, *w); }
    let mut c = R5900::new();
    c.cop0[STATUS] = ST_CU1 | 1 << 30;   // kernel, COP1 / COP2 usable, BEV off
    c.jump(BASE);
    for _ in 0..steps { c.step(&mut ram); }
    (c, ram)
}

fn reg(c: &R5900, n: usize) -> u64 { c.gpr[n] as u64 }

#[test]
fn sixty_four_bit_arithmetic_and_sign_extension() {
    let (c, _) = run(&[
        addiu(2, 0, -1),                      // r2 = -1 (sign-extended to 64)
        lui(3, 0x8000),                       // r3 = FFFF_FFFF_8000_0000
        special(0, 2, 4, 0, 0x3C),            // DSLL32 r4 = r2 << 32
        special(2, 3, 5, 0, 0x2D),            // DADDU r5 = r2 + r3
        special(0, 3, 6, 4, 0x3E),            // DSRL32 r6 = r3 >> 36
        special(0, 0, 0, 0, 0),
    ], 6);
    assert_eq!(reg(&c, 2), u64::MAX);
    assert_eq!(reg(&c, 3), 0xFFFF_FFFF_8000_0000);
    assert_eq!(reg(&c, 4), 0xFFFF_FFFF_0000_0000);
    assert_eq!(reg(&c, 5), 0xFFFF_FFFF_7FFF_FFFF);
    assert_eq!(reg(&c, 6), 0x0FFF_FFFF);
}

#[test]
fn ordinary_instructions_keep_the_high_64_bits() {
    let mut ram = Ram(vec![0; 0x40_0000]);
    ram.write32(0x1000, addiu(2, 2, 1));
    let mut c = R5900::new();
    c.cop0[STATUS] = 0;
    c.jump(BASE);
    c.gpr[2] = 0x1234_5678_0000_0000_0000_0000_0000_0005;
    c.step(&mut ram);
    assert_eq!(c.gpr[2], 0x1234_5678_0000_0000_0000_0000_0000_0006);
}

#[test]
fn a_branch_likely_skips_its_delay_slot_when_not_taken() {
    let (c, _) = run(&[
        i(0x15, 0, 0, 2),                     // BNEL r0, r0 (never): skip the slot
        addiu(2, 0, 1),                       // (skipped)
        addiu(3, 0, 2),
        i(0x14, 0, 0, 2),                     // BEQL r0, r0 (taken): the slot runs
        addiu(4, 0, 3),
        addiu(5, 0, 4),                       // (jumped over)
        addiu(6, 0, 5),
    ], 5);
    assert_eq!((reg(&c, 2), reg(&c, 3), reg(&c, 4), reg(&c, 5), reg(&c, 6)), (0, 2, 3, 0, 5));
}

#[test]
fn quadword_and_unaligned_loads_and_stores() {
    let (c, mut ram) = run(&[
        lui(8, 0x8010),                       // r8 = 8010_0000
        i(0x1E, 8, 2, 0x13),                  // LQ r2, 0x13(r8): the address rounds down to 0x10
        i(0x1F, 8, 2, 0x40),                  // SQ r2, 0x40(r8)
        i(0x1B, 8, 3, 0x23),                  // LDR r3, 0x23(r8)
        i(0x1A, 8, 3, 0x2A),                  // LDL r3, 0x2A(r8): r3 = the dword at 0x23
        i(0x26, 8, 4, 0x31),                  // LWR r4, 0x31(r8)
        i(0x22, 8, 4, 0x34),                  // LWL r4, 0x34(r8): r4 = the word at 0x31
        i(0x2D, 8, 3, 0x51),                  // SDR r3, 0x51(r8)
        i(0x2C, 8, 3, 0x58),                  // SDL r3, 0x58(r8): the dword at 0x51
    ], 0);
    let _ = c;
    for k in 0..0x80u32 { ram.write8(0x10_0000 + k, k as u8 ^ 0xA5); }
    let mut c = R5900::new();
    c.cop0[STATUS] = 0;
    c.jump(BASE);
    for _ in 0..9 { c.step(&mut ram); }
    let bytes = |a: u32, n: u32| (0..n).map(|k| (a + k) as u8 ^ 0xA5).collect::<Vec<u8>>();
    let q = u128::from_le_bytes(bytes(0x10, 16).try_into().unwrap());
    assert_eq!(c.gpr[2], q, "LQ");
    assert_eq!(ram.read64(0x10_0040), q as u64, "SQ low");
    assert_eq!(ram.read64(0x10_0048), (q >> 64) as u64, "SQ high");
    assert_eq!(reg(&c, 3), u64::from_le_bytes(bytes(0x23, 8).try_into().unwrap()), "LDL / LDR");
    assert_eq!(reg(&c, 4) as u32, u32::from_le_bytes(bytes(0x31, 4).try_into().unwrap()), "LWL / LWR");
    assert_eq!(ram.read64(0x10_0051), reg(&c, 3), "SDL / SDR");
}

#[test]
fn mult_writes_rd_and_the_second_pipeline_uses_the_high_halves() {
    let (c, _) = run(&[
        addiu(2, 0, -3),
        addiu(3, 0, 7),
        special(2, 3, 4, 0, 0x18),            // MULT r4 = LO = -21
        mmi(3, 3, 5, 0, 0x18),                // MULT1 r5 = 49 (LO1)
        special(3, 0, 0, 0, 0x1A),            // DIV 7 / 0
    ], 5);
    assert_eq!(reg(&c, 4) as i64, -21);
    assert_eq!(c.lo as u64 as i64, -1, "7 / 0: LO = -1");
    assert_eq!(c.hi as u64, 7, "HI = the dividend");
    assert_eq!(reg(&c, 5), 49);
    assert_eq!((c.lo >> 64) as u64, 49, "LO1");
}

#[test]
fn parallel_adds_saturate_and_interleave() {
    let mut ram = Ram(vec![0; 0x40_0000]);
    let code = [
        mmi(2, 3, 4, 0x00, 0x08),             // PADDW
        mmi(2, 3, 5, 0x14, 0x08),             // PADDSH
        mmi(2, 3, 6, 0x12, 0x08),             // PEXTLW
        mmi(2, 3, 7, 0x0E, 0x09),             // PCPYLD
        mmi(2, 3, 8, 0x0E, 0x29),             // PCPYUD
        mmi(2, 3, 9, 0x1B, 0x08),             // PPACB
    ];
    for (k, w) in code.iter().enumerate() { ram.write32(0x1000 + 4 * k as u32, *w); }
    let mut c = R5900::new();
    c.cop0[STATUS] = 0;
    c.jump(BASE);
    c.gpr[2] = 0x7FFF_0001_0000_0003_0000_0002_FFFF_FFFF;
    c.gpr[3] = 0x0001_0001_0000_0010_0000_0020_0000_0001;
    for _ in 0..code.len() { c.step(&mut ram); }
    assert_eq!(c.gpr[4], 0x8000_0002_0000_0013_0000_0022_0000_0000, "PADDW wraps per word");
    assert_eq!(c.gpr[5] >> 112, 0x7FFF, "PADDSH: 7FFF + 1 saturates");
    assert_eq!(c.gpr[6], 0x0000_0002_0000_0020_FFFF_FFFF_0000_0001, "PEXTLW: rs.w1 rt.w1 rs.w0 rt.w0");
    assert_eq!(c.gpr[7], (c.gpr[2] as u64 as u128) << 64 | c.gpr[3] as u64 as u128, "PCPYLD");
    assert_eq!(c.gpr[8], (c.gpr[3] >> 64) << 64 | c.gpr[2] >> 64, "PCPYUD");
    let pb = c.gpr[9].to_le_bytes();
    let (x, y) = (c.gpr[2].to_le_bytes(), c.gpr[3].to_le_bytes());
    for k in 0..8 { assert_eq!(pb[k], y[2 * k]); assert_eq!(pb[8 + k], x[2 * k]); }
}

#[test]
fn pmaddw_accumulates_into_lo_hi_lanes() {
    let mut ram = Ram(vec![0; 0x40_0000]);
    let code = [
        mmi(2, 3, 4, 0x00, 0x09),             // PMADDW
        mmi(0, 0, 5, 0x00, 0x30),             // PMFHL.LW
        mmi(2, 0, 6, 0, 0x04),                // PLZCW
    ];
    for (k, w) in code.iter().enumerate() { ram.write32(0x1000 + 4 * k as u32, *w); }
    let mut c = R5900::new();
    c.cop0[STATUS] = 0;
    c.jump(BASE);
    c.gpr[2] = (1u128 << 64) | 0x0000_0000_FFFF_FFFE;          // words: -2, 0, 1, 0
    c.gpr[3] = (3u128 << 64) | 0x0000_0000_4000_0000;          // words: 2^30, 0, 3, 0
    c.lo = 5;
    for _ in 0..code.len() { c.step(&mut ram); }
    assert_eq!(c.gpr[4] as u64 as i64, 5 - (1 << 31), "lane 0: 5 + -2 * 2^30");
    assert_eq!((c.gpr[4] >> 64) as u64, 3, "lane 2: 1 * 3");
    let w = |v: u128, k: u32| (v >> (32 * k)) as u32;
    assert_eq!(w(c.gpr[5], 0), (5i64 - (1 << 31)) as u32, "PMFHL.LW: LO0");
    assert_eq!(w(c.gpr[5], 1), 0xFFFF_FFFF, "HI0 (the high half of a negative)");
    assert_eq!(w(c.gpr[5], 2), 3);
    assert_eq!(w(c.gpr[6], 0), 30, "PLZCW of -2 (FFFF_FFFE): 31 ones, minus 1");
    assert_eq!(w(c.gpr[6], 1), 31, "of 0");
}

#[test]
fn qfsrv_funnel_shifts_by_sa_bytes() {
    let mut ram = Ram(vec![0; 0x40_0000]);
    let code = [addiu(2, 0, 3), i(0x01, 2, 0x18, 0), mmi(3, 4, 5, 0x1B, 0x28), special(0, 0, 6, 0, 0x28)];
    for (k, w) in code.iter().enumerate() { ram.write32(0x1000 + 4 * k as u32, *w); }   // MTSAB 3; QFSRV; MFSA
    let mut c = R5900::new();
    c.cop0[STATUS] = 0;
    c.jump(BASE);
    c.gpr[3] = 0xAABBCC;
    c.gpr[4] = 0x1122_3344_5566_7788_99AA_BBCC_DDEE_FF00;
    for _ in 0..4 { c.step(&mut ram); }
    assert_eq!(c.gpr[5], 0xAABB_CC11_2233_4455_6677_8899_AABB_CCDD, "(rs:rt) >> 3 bytes");
    assert_eq!(reg(&c, 6), 3);
}

#[test]
fn pext5_and_ppac5_round_trip_a_1555_pixel() {
    let mut ram = Ram(vec![0; 0x40_0000]);
    ram.write32(0x1000, mmi(0, 2, 3, 0x1E, 0x08));
    ram.write32(0x1004, mmi(0, 3, 4, 0x1F, 0x08));
    let mut c = R5900::new();
    c.cop0[STATUS] = 0;
    c.jump(BASE);
    c.gpr[2] = 0xFC1F;                                         // A=1, B=31, G=0, R=31
    c.step(&mut ram);
    c.step(&mut ram);
    assert_eq!(c.gpr[3] as u32, 0x80F8_00F8);
    assert_eq!(c.gpr[4] as u32, 0xFC1F);
}

// ---- the FPU

fn fpu(a: u32, b: u32, funct: u32) -> R5900 {
    let mut ram = Ram(vec![0; 0x40_0000]);
    ram.write32(0x1000, cop1(0x10, 2, 1, 3, funct));
    let mut c = R5900::new();
    c.cop0[STATUS] = ST_CU1;
    c.jump(BASE);
    c.fpr[1] = a;
    c.fpr[2] = b;
    c.step(&mut ram);
    c
}

#[test]
fn the_fpu_truncates_and_clamps_instead_of_ieee() {
    let f = |x: f32| x.to_bits();
    assert_eq!(fpu(f(1.5), f(2.25), 0).fpr[3], f(3.75), "ADD");
    // 1/3: IEEE rounds the last bit up (3EAAAAAB); the PS2 truncates
    assert_eq!(fpu(f(1.0), f(3.0), 3).fpr[3], 0x3EAA_AAAA, "DIV rounds toward zero");
    // exponent 255 is a number: Fmax * 2 overflows to Fmax with O set
    let c = fpu(0x7FFF_FFFF, f(2.0), 2);
    assert_eq!(c.fpr[3], 0x7FFF_FFFF);
    assert_ne!(c.fcr31 & fpu::F_O, 0, "O");
    assert_eq!(fpu(0x7F80_0000, f(0.5), 2).fpr[3], 0x7F00_0000, "7F80_0000 isn't infinity: 2^128 / 2");
    assert_eq!(fpu(0x0000_0001, f(1.0), 0).fpr[3], f(1.0), "denormals count as zero");
    let c = fpu(f(-1.0), 0, 3);
    assert_eq!(c.fpr[3], 0xFFFF_FFFF, "-1 / 0 = -Fmax");
    assert_ne!(c.fcr31 & fpu::F_D, 0, "D");
    let c = fpu(f(1e-30), f(1e-30), 2);
    assert_eq!(c.fpr[3], 0, "underflow -> 0");
    assert_ne!(c.fcr31 & fpu::F_U, 0, "U");
}

#[test]
fn cvt_compare_and_branch_on_fpu_condition() {
    let mut ram = Ram(vec![0; 0x40_0000]);
    let code = [
        cop1(0x10, 0, 1, 3, 0x24),            // CVT.W.S f3 = (int) f1
        cop1(0x10, 0, 2, 4, 0x24),            // CVT.W.S f4 = (int) f2 (huge: saturates)
        cop1(0x10, 2, 1, 0, 0x34),            // C.LT.S f1 < f2
        r(0x11, 8, 1, 0, 0, 2),               // BC1T +2
        NOP,
        addiu(5, 0, 1),                       // (skipped)
        addiu(6, 0, 1),
    ];
    for (k, w) in code.iter().enumerate() { ram.write32(0x1000 + 4 * k as u32, *w); }
    let mut c = R5900::new();
    c.cop0[STATUS] = ST_CU1;
    c.jump(BASE);
    c.fpr[1] = (-7.9f32).to_bits();
    c.fpr[2] = 3e10f32.to_bits();
    for _ in 0..6 { c.step(&mut ram); }
    assert_eq!(c.fpr[3] as i32, -7, "truncates");
    assert_eq!(c.fpr[4], 0x7FFF_FFFF, "saturates");
    assert_eq!((reg(&c, 5), reg(&c, 6)), (0, 1), "the branch on C was taken");
}

#[test]
fn madd_s_uses_the_accumulator() {
    let mut ram = Ram(vec![0; 0x40_0000]);
    ram.write32(0x1000, cop1(0x10, 2, 1, 0, 0x18));            // ADDA.S acc = f1 + f2
    ram.write32(0x1004, cop1(0x10, 2, 1, 3, 0x1C));            // MADD.S f3 = acc + f1 * f2
    let mut c = R5900::new();
    c.cop0[STATUS] = ST_CU1;
    c.jump(BASE);
    c.fpr[1] = 2.0f32.to_bits();
    c.fpr[2] = 5.0f32.to_bits();
    c.step(&mut ram);
    c.step(&mut ram);
    assert_eq!(f32::from_bits(c.fpr[3]), 17.0);
}

// ---- COP0: exceptions, interrupts, the TLB

#[test]
fn syscall_goes_to_the_common_vector_and_eret_returns() {
    let (c, _) = run(&[special(0, 0, 0, 0, 0x0C)], 1);
    assert_eq!(c.pc, 0x8000_0180);
    assert_eq!(c.cop0[EPC], BASE);
    assert_eq!(c.cop0[CAUSE] >> 2 & 31, EXC_SYSCALL);
    assert_ne!(c.cop0[STATUS] & ST_EXL, 0);
    let mut ram = Ram(vec![0; 0x40_0000]);
    ram.write32(0x180, r(0x10, 0x10, 0, 0, 0, 0x18));          // ERET
    let mut c = c;
    c.cop0[EPC] = BASE + 4;
    c.step(&mut ram);
    assert_eq!(c.pc, BASE + 4);
    assert_eq!(c.cop0[STATUS] & ST_EXL, 0);
}

#[test]
fn intc_interrupts_need_ie_eie_and_the_mask() {
    let mut ram = Ram(vec![0; 0x40_0000]);
    ram.write32(0x1000, NOP);
    ram.write32(0x1004, NOP);
    let mut c = R5900::new();
    c.cop0[STATUS] = ST_IE | IP_INT0;     // EIE off
    c.jump(BASE);
    c.set_int0(true);
    c.step(&mut ram);
    assert_eq!(c.pc, BASE + 4, "EIE off: no interrupt");
    c.cop0[STATUS] |= ST_EIE;
    c.step(&mut ram);
    assert_eq!(c.pc, 0x8000_0200, "the interrupt vector");
    assert_eq!(c.cop0[EPC], BASE + 4);
    assert_ne!(c.cause() & IP_INT0, 0);
}

#[test]
fn an_interrupt_in_a_delay_slot_returns_to_the_branch() {
    let mut ram = Ram(vec![0; 0x40_0000]);
    ram.write32(0x1000, i(0x04, 0, 0, 4));                     // BEQ (taken)
    ram.write32(0x1004, NOP);
    let mut c = R5900::new();
    c.cop0[STATUS] = ST_IE | ST_EIE | IP_INT1;
    c.jump(BASE);
    c.step(&mut ram);
    c.set_int1(true);
    c.step(&mut ram);
    assert_eq!(c.pc, 0x8000_0200);
    assert_eq!(c.cop0[EPC], BASE, "EPC = the branch");
    assert_ne!(c.cop0[CAUSE] & 0x8000_0000, 0, "BD");
}

#[test]
fn compare_raises_the_timer_interrupt() {
    let mut ram = Ram(vec![0; 0x40_0000]);
    for k in 0..16 { ram.write32(0x1000 + 4 * k, NOP); }
    let mut c = R5900::new();
    c.cop0[STATUS] = 0;
    c.jump(BASE);
    c.cop0[COMPARE] = 5;
    for _ in 0..4 { c.step(&mut ram); }
    assert_eq!(c.cop0[CAUSE] & IP_TIMER, 0);
    c.step(&mut ram);
    assert_ne!(c.cop0[CAUSE] & IP_TIMER, 0, "Count reached Compare");
}

/// The BIOS's kind of mapping: a 4 KB page pair at 0001_0000 -> 0020_0000, and the
/// scratchpad at 7000_0000
#[test]
fn the_tlb_maps_pages_and_the_scratchpad() {
    let (c, mut ram) = run(&[
        lui(2, 0x0001), mtc0(2, ENTRYHI as u32),                // VPN2 = 0001_0000
        lui(2, 0), ori(2, 2, (0x200 << 6) | 0x7), mtc0(2, ENTRYLO0 as u32),   // PFN 200, D V G
        ori(2, 0, (0x201 << 6) | 0x7), mtc0(2, ENTRYLO1 as u32),
        mtc0(0, PAGEMASK as u32), mtc0(0, INDEX as u32),
        r(0x10, 0x10, 0, 0, 0, 0x02),                            // TLBWI
        lui(2, 0x7000), mtc0(2, ENTRYHI as u32),
        lui(2, 0x8000), ori(2, 2, 0x7), mtc0(2, ENTRYLO0 as u32), // S, D V G
        ori(2, 0, 0x1), mtc0(2, ENTRYLO1 as u32),
        lui(2, 0x0000), ori(2, 2, 0x6000), mtc0(2, PAGEMASK as u32), // 16 KB pages
        ori(2, 0, 1), mtc0(2, INDEX as u32),
        r(0x10, 0x10, 0, 0, 0, 0x02),                            // TLBWI
        lui(3, 0x0001), addiu(4, 0, 0x55), i(0x2B, 3, 4, 0x1004),   // SW 0x55 -> 0001_1004
        lui(5, 0x7000), i(0x2B, 5, 4, 0x3FF0),                   // SW 0x55 -> scratchpad 3FF0
        i(0x23, 5, 6, 0x3FF0),                                    // LW it back
    ], 31);
    assert_eq!(ram.read32(0x20_1004), 0x55, "odd page -> PFN 201");
    assert_eq!(c.scratch[0x3FF0], 0x55);
    assert_eq!(reg(&c, 6), 0x55);
    assert_eq!(c.virt_to_phys(0x0001_0010), Some(0x20_0010));
}

#[test]
fn an_unmapped_kuseg_access_takes_the_refill_vector() {
    let (c, _) = run(&[lui(2, 0x0400), i(0x23, 2, 3, 0)], 2);
    assert_eq!(c.pc, 0x8000_0000, "TLB refill");
    assert_eq!(c.cop0[CAUSE] >> 2 & 31, EXC_TLBL);
    assert_eq!(c.cop0[BADVADDR], 0x0400_0000);
}

#[test]
fn overflow_traps_on_add_but_not_addu() {
    let (c, _) = run(&[lui(2, 0x7FFF), ori(2, 2, 0xFFFF), special(2, 2, 3, 0, 0x21), special(2, 2, 4, 0, 0x20)], 4);
    assert_eq!(reg(&c, 3), 0xFFFF_FFFF_FFFF_FFFE, "ADDU wraps (sign-extended)");
    assert_eq!(reg(&c, 4), 0, "ADD didn't write");
    assert_eq!(c.cop0[CAUSE] >> 2 & 31, EXC_OV);
}

/// The Sony BIOS's first TLB entry: the scratchpad at 7000_0000 with PageMask 0, then the
/// stack at 7000_3FF0
#[test]
fn a_scratchpad_entry_maps_16_kb_whatever_the_page_mask() {
    let (c, _) = run(&[
        mtc0(0, INDEX as u32), mtc0(0, PAGEMASK as u32),
        lui(5, 0x7000), mtc0(5, ENTRYHI as u32),
        lui(6, 0x8000), ori(6, 6, 7), mtc0(6, ENTRYLO0 as u32),
        ori(7, 0, 7), mtc0(7, ENTRYLO1 as u32),
        r(0x10, 0x10, 0, 0, 0, 0x02),                            // TLBWI
        addiu(2, 0, 0x77), i(0x3F, 5, 2, 0x3FE0),                // SD -> 7000_3FE0
    ], 12);
    assert_eq!(c.cop0[CAUSE] >> 2 & 31, 0, "no TLB fault");
    assert_eq!(c.scratch[0x3FE0], 0x77);
}
