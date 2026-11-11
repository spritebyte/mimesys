//! The SH-4 core against SingleStepTests/sh4 (MIT): every instruction file, including the
//! FPU's SZ / PR variants. Each test runs four instructions: NOP, the one under test,
//! ADD R1,R1 (its delay slot when it branches), then NOP or (at a branch target) ADD R2,R2.
//! Data reads return the recorded values; writes and the whole state are compared.
//! The sample (3 tests per file) is in tests/data; the full set (116,500 tests) is made by
//! tools/sh4_sst/convert_sh4.py, run with SH4_SST=<file> and --ignored.

use super::*;
use std::collections::HashMap;

struct TestBus {
    ops: [u16; 5],
    start: u32,
    reads: HashMap<u32, u64>,
    writes: Vec<(u32, u64, u32)>,
}

impl TestBus {
    fn data(&self, a: u32) -> u64 { self.reads.get(&a).copied().unwrap_or(0) }
}

impl Sh4Bus for TestBus {
    fn read8(&mut self, a: u32) -> u8 { self.data(a) as u8 }
    fn read16(&mut self, a: u32) -> u16 { self.data(a) as u16 }
    fn read32(&mut self, a: u32) -> u32 { self.data(a) as u32 }
    fn read64(&mut self, a: u32) -> u64 { self.data(a) }
    fn write8(&mut self, a: u32, v: u8) { self.writes.push((a, v as u64, 1)) }
    fn write16(&mut self, a: u32, v: u16) { self.writes.push((a, v as u64, 2)) }
    fn write32(&mut self, a: u32, v: u32) { self.writes.push((a, v as u64, 4)) }
    fn write64(&mut self, a: u32, v: u64) { self.writes.push((a, v, 8)) }
    fn fetch16(&mut self, a: u32) -> u16 {
        // the four addresses from the start hold the list, every other address ADD R2,R2
        let k = a.wrapping_sub(self.start);
        if k < 8 { self.ops[k as usize / 2] } else { self.ops[4] }
    }
}

const NAMES: [&str; 13] = ["PC", "GBR", "SR", "SSR", "SPC", "VBR", "SGR", "DBR", "MACL", "MACH", "PR", "FPSCR", "FPUL"];

fn name(i: usize) -> String {
    match i {
        0..=15 => format!("R{i}"),
        16..=23 => format!("RB{}", i - 16),
        24..=39 => format!("XF{}", i - 24),
        40..=55 => format!("FR{}", i - 40),
        _ => NAMES[i - 56].to_string(),
    }
}

fn load(c: &mut Sh4, v: &[u32]) {
    c.r.copy_from_slice(&v[..16]);
    c.r_bank.copy_from_slice(&v[16..24]);
    // the suite's "FP0" is the bank not in use, "FP1" the FR registers
    c.xf.copy_from_slice(&v[24..40]);
    c.fr.copy_from_slice(&v[40..56]);
    c.pc = v[56];
    c.gbr = v[57];
    let sr = v[58];
    c.t = sr & 1 != 0; c.s = sr & 2 != 0; c.q = sr & 0x100 != 0; c.m = sr & 0x200 != 0;
    c.sr_hi = sr & SR_MASK & !0x303;
    c.ssr = v[59]; c.spc = v[60]; c.vbr = v[61]; c.sgr = v[62]; c.dbr = v[63];
    c.macl = v[64]; c.mach = v[65]; c.pr = v[66]; c.fpscr = v[67]; c.fpul = v[68];
}

fn state(c: &Sh4) -> Vec<u32> {
    let mut v = c.r.to_vec();
    v.extend(c.r_bank);
    v.extend(c.xf);
    v.extend(c.fr);
    v.extend([c.pc, c.gbr, c.sr(), c.ssr, c.spc, c.vbr, c.sgr, c.dbr, c.macl, c.mach, c.pr, c.fpscr, c.fpul]);
    v
}

fn accesses(s: &str) -> Vec<(u32, u64, u32)> {
    s.split(',').filter(|x| !x.is_empty()).map(|x| {
        let v: Vec<&str> = x.split(':').collect();
        (u32::from_str_radix(v[0], 16).unwrap(), u64::from_str_radix(v[1], 16).unwrap(), v[2].parse().unwrap())
    }).collect()
}

/// Tests the reference can't be compared with.
fn skipped(op: u16) -> bool {
    // TRAPA: Reicast's test generator runs it as a NOP
    op >> 8 == 0xC3
    // DIV1 Rn,Rn: Reicast shifts Rn before reading it as Rm
    || op & 0xF00F == 0x3004 && (op >> 8 & 15) == (op >> 4 & 15)
    // a delayed branch to itself: Reicast runs the slot inside the branch, so it counts one
    // instruction more
    || matches!(op, 0x8DFE | 0x8FFE | 0xAFFE | 0xBFFE)
}

/// FIPR, FTRV, FSRRA and FSCA are approximations on the real chip (and on the reference,
/// computed by its host's float unit): compared within 1e-5 relative.
fn approximate(op: u16, fpscr: u32) -> bool {
    fpscr & FPSCR_PR == 0 && (op & 0xF0FF == 0xF0ED || op & 0xF3FF == 0xF1FD || op & 0xF0FF == 0xF07D
        || op & 0xF1FF == 0xF0FD && op != 0xF3FD && op != 0xFBFD)
}

/// (passed, failing opcodes with counts, report lines)
fn run(text: &str, filter: impl Fn(u16) -> bool) -> (usize, Vec<String>, Vec<String>) {
    let (mut ok, mut bad, mut report) = (0, std::collections::BTreeMap::new(), Vec::new());
    for line in text.lines().filter(|l| !l.starts_with('#') && !l.is_empty()) {
        let f: Vec<&str> = line.split('|').collect();
        let op = u16::from_str_radix(f[0], 16).unwrap();
        if skipped(op) || !filter(op) { continue; }
        let init: Vec<u32> = f[1].split_whitespace().map(|x| u32::from_str_radix(x, 16).unwrap()).collect();
        let mut want = init.clone();
        for d in f[2].split_whitespace() {
            let (i, v) = d.split_once('=').unwrap();
            want[i.parse::<usize>().unwrap()] = u32::from_str_radix(v, 16).unwrap();
        }
        let mut bus = TestBus {
            ops: [0x0009, op, 0x311C, 0x0009, 0x322C], start: init[56],
            reads: accesses(f[3]).iter().map(|&(a, v, _)| (a, v)).collect(), writes: Vec::new(),
        };
        let mut c = Sh4::new();
        c.check_fd = false;
        load(&mut c, &init);
        for _ in 0..4 { c.step(&mut bus); }
        let got = state(&c);
        // LDS / LDS.L to FPSCR: the reference keeps all 32 bits; the chip has 22
        if op & 0xF0EF == 0x4066 || op & 0xF0FF == 0x406A { want[67] &= FPSCR_MASK; }
        let want_writes = accesses(f[4]);
        let fsca = op & 0xF1FF == 0xF0FD && op != 0xF3FD && op != 0xFBFD;
        let close = |i: usize| {
            let (g, w) = (f32::from_bits(got[i]), f32::from_bits(want[i]));
            got[i] == want[i] || g.is_nan() && w.is_nan() || ((g - w).abs() <= w.abs().max(1e-30) * 1e-5)
                // overflowing sums: the reference's order of rounding decides the sign / limit
                || g.abs() >= f32::MAX / 2.0 && w.abs() >= f32::MAX / 2.0
                // FSCA uses a table on the chip (and on the reference): near 0 the results
                // agree to about 2^-24 absolute
                || fsca && (g - w).abs() < 1e-7
        };
        let fuzzy = approximate(op, init[67]);
        let same = (0..69).all(|i| got[i] == want[i] || fuzzy && (40..56).contains(&i) && close(i))
            // FTRC of a NaN: 80000000 (x86 / the manual); the reference's ARM host gave 0
            || op & 0xF0FF == 0xF03D && got[68] == 0x8000_0000 && want[68] == 0 && (0..68).all(|i| got[i] == want[i]);
        if same && bus.writes == want_writes {
            ok += 1;
        } else {
            let class = if op >> 12 == 0xF { if op & 15 == 0xD { op & 0xF0FF } else { op & 0xF00F } } else { op };
            let e = bad.entry(format!("{class:04X}")).or_insert(0);
            *e += 1;
            if *e <= 3 && report.len() < 120 {
                let diff: Vec<String> = (0..69).filter(|&i| got[i] != want[i])
                    .map(|i| format!("{} {:X} want {:X} (was {:X})", name(i), got[i], want[i], init[i])).collect();
                report.push(format!("op {op:04X} fpscr {:X}: {diff:?} writes {:X?} want {:X?}", init[67], bus.writes, want_writes));
            }
        }
    }
    (ok, bad.into_iter().map(|(k, n)| format!("{k}x{n}")).collect(), report)
}

#[test]
fn sh4_sample() {
    let (ok, bad, report) = run(include_str!("../../tests/data/sh4_sample.txt"), |_| true);
    assert!(bad.is_empty(), "{ok} passed; failing {bad:?}\n{}", report.join("\n"));
    assert!(ok > 650, "{ok}");
}

#[test]
#[ignore]
fn sh4_full() {
    let text = std::fs::read_to_string(std::env::var("SH4_SST").expect("set SH4_SST")).unwrap();
    let only = std::env::var("SH4_OP").ok().map(|s| u16::from_str_radix(&s, 16).unwrap());
    let (ok, bad, report) = run(&text, |op| only.map_or(true, |o| op & 0xF00F == o & 0xF00F));
    assert!(bad.is_empty(), "{ok} passed; failing {} opcodes {bad:?}\n{}", bad.len(), report.join("\n"));
}

// ------------------------------------------------------------ hand-written programs

/// Little-endian RAM (the Dreamcast's byte order) at 0-0xFFFF, mirrored; records PREF and
/// CCN events.
struct Ram { mem: Vec<u8>, events: Vec<(Event, u32)>, prefs: Vec<u32> }

impl Ram {
    fn new() -> Self { Self { mem: vec![0; 0x10000], events: Vec::new(), prefs: Vec::new() } }
    fn at(&self, a: u32) -> usize { a as usize & 0xFFFF }
    fn prog(&mut self, a: u32, ops: &[u16]) { for (k, &op) in ops.iter().enumerate() { self.write16(a + 2 * k as u32, op); } }
}

impl Sh4Bus for Ram {
    fn read8(&mut self, a: u32) -> u8 { self.mem[self.at(a)] }
    fn read16(&mut self, a: u32) -> u16 { u16::from_le_bytes([self.read8(a), self.read8(a + 1)]) }
    fn read32(&mut self, a: u32) -> u32 { self.read16(a) as u32 | (self.read16(a + 2) as u32) << 16 }
    fn write8(&mut self, a: u32, v: u8) { let i = self.at(a); self.mem[i] = v; }
    fn write16(&mut self, a: u32, v: u16) { self.write8(a, v as u8); self.write8(a + 1, (v >> 8) as u8); }
    fn write32(&mut self, a: u32, v: u32) { self.write16(a, v as u16); self.write16(a + 2, (v >> 16) as u16); }
    fn pref(&mut self, a: u32) { self.prefs.push(a); }
    fn event(&mut self, reg: Event, v: u32) { self.events.push((reg, v)); }
}

/// A CPU in privileged mode, bank 0, interrupts open, at `pc`.
fn cpu_at(pc: u32) -> Sh4 {
    let mut c = Sh4::new();
    c.set_sr(SR_MD); // MD, RB = 0, BL = 0, IMASK 0
    c.pc = pc;
    c.vbr = 0x8000;
    c
}

#[test]
fn reset_state() {
    let c = Sh4::new();
    assert_eq!((c.pc, c.sr(), c.vbr, c.fpscr), (0xA000_0000, 0x7000_00F0, 0, 0x0004_0001));
}

/// TRAPA: SPC = next instruction, SSR / SGR saved, MD RB BL set (bank 1 in use), TRA /
/// EXPEVT reported, PC = VBR + 0x100; RTE comes back with bank 0.
#[test]
fn trapa_exception_and_rte() {
    let mut ram = Ram::new();
    ram.prog(0x1000, &[0xC312, 0xE155]);        // TRAPA #12; MOV #55,R1
    ram.prog(0x8100, &[0xE077, 0x002B, 0x0009]); // handler: MOV #77,R0 (bank 1); RTE; NOP
    let mut c = cpu_at(0x1000);
    c.r[0] = 0x11;
    c.r[15] = 0x2000;
    let sr0 = c.sr();
    c.step(&mut ram);
    assert_eq!((c.pc, c.spc, c.ssr, c.sgr), (0x8100, 0x1002, sr0, 0x2000));
    assert_eq!(c.sr() & (SR_MD | SR_RB | SR_BL), SR_MD | SR_RB | SR_BL);
    assert_eq!(ram.events, vec![(Event::Tra, 0x48), (Event::Expevt, EXP_TRAPA)]);
    for _ in 0..3 { c.step(&mut ram); }      // MOV, RTE, NOP (slot)
    assert_eq!((c.pc, c.sr(), c.r[0], c.r_bank[0]), (0x1002, sr0, 0x11, 0x77), "bank 0 back, R0 kept");
    c.step(&mut ram);
    assert_eq!(c.r[1], 0x55);
}

/// Interrupts: refused while BL is set or the level isn't above IMASK, or between a branch
/// and its slot; taken: INTEVT, VBR + 0x600, SPC = the next instruction. SLEEP waits, and
/// the interrupt returns after it.
#[test]
fn interrupts_and_sleep() {
    let mut ram = Ram::new();
    ram.prog(0x1000, &[0x001B, 0xE155, 0xA000, 0x0009]); // SLEEP; MOV #55,R1; BRA +0; NOP
    let mut c = cpu_at(0x1000);
    c.set_sr(SR_MD | 0x80);                     // IMASK 8
    assert!(!c.accepts(8) && c.accepts(9));
    c.step(&mut ram);
    c.step(&mut ram);
    assert!(c.sleeping);
    assert_eq!(c.pc, 0x1000);
    c.interrupt(&mut ram, 0x320);
    assert_eq!((c.pc, c.spc, c.sleeping), (0x8600, 0x1002, false));
    assert_eq!(ram.events, vec![(Event::Intevt, 0x320)]);
    assert!(!c.accepts(15), "BL set inside the handler");
    // back at 1002, then the branch: no interrupt between it and its slot
    let mut c = cpu_at(0x1002);
    c.step(&mut ram);
    c.step(&mut ram);
    assert!(c.pending.is_some() && !c.accepts(15));
    c.step(&mut ram);
    assert!(c.accepts(15));
}

/// Illegal instructions: 0x180, or 0x1A0 in a delay slot with SPC = the branch.
#[test]
fn illegal_instructions() {
    let mut ram = Ram::new();
    ram.prog(0x1000, &[0xFFFF]);
    let mut c = cpu_at(0x1000);
    c.step(&mut ram);
    assert_eq!((c.pc, c.spc), (0x8100, 0x1000));
    assert_eq!(ram.events, vec![(Event::Expevt, EXP_ILLEGAL)]);
    // a branch in a delay slot
    let mut ram = Ram::new();
    ram.prog(0x1000, &[0xA010, 0xA020]);
    let mut c = cpu_at(0x1000);
    c.step(&mut ram);
    c.step(&mut ram);
    assert_eq!((c.pc, c.spc), (0x8100, 0x1000));
    assert_eq!(ram.events, vec![(Event::Expevt, EXP_ILLEGAL_SLOT)]);
}

#[test]
fn fpu_disabled_exception() {
    let mut ram = Ram::new();
    ram.prog(0x1000, &[0xF00C]);                // FMOV FR0,FR0
    let mut c = cpu_at(0x1000);
    c.set_sr(SR_MD | SR_FD);
    c.step(&mut ram);
    assert_eq!((c.pc, c.spc), (0x8100, 0x1000));
    assert_eq!(ram.events, vec![(Event::Expevt, EXP_FPU_DISABLED)]);
}

/// PC-relative loads in a delay slot use the branch target + 2.
#[test]
fn pc_relative_in_a_delay_slot() {
    let mut ram = Ram::new();
    ram.prog(0x1000, &[0xA00E, 0xD101]);        // BRA 0x1020; MOV.L @(1,PC),R1
    ram.write32(0x1024, 0xDEAD_BEEF);           // (0x1020 + 2) & ~3 + 4
    ram.write32(0x1008, 0x1234_5678);           // where the non-slot rule would read
    let mut c = cpu_at(0x1000);
    c.step(&mut ram);
    c.step(&mut ram);
    assert_eq!((c.pc, c.r[1]), (0x1020, 0xDEAD_BEEF));
}

#[test]
fn mac_w_and_mac_l() {
    let mut ram = Ram::new();
    ram.prog(0x1000, &[0x412F, 0x412F, 0x412F, 0x010F]); // MAC.W @R2+,@R1+ x3; MAC.L @R0+,@R1+
    for (k, v) in [3i16, -4, 0x7FFF].iter().enumerate() { ram.write16(0x200 + 2 * k as u32, *v as u16); }
    for (k, v) in [5i16, 6, 0x7FFF].iter().enumerate() { ram.write16(0x300 + 2 * k as u32, *v as u16); }
    let mut c = cpu_at(0x1000);
    c.r[1] = 0x200;
    c.r[2] = 0x300;
    c.step(&mut ram);
    c.step(&mut ram);
    assert_eq!((c.mach, c.macl), (0xFFFF_FFFF, (15i32 - 24) as u32), "64-bit sum, sign-extended");
    assert_eq!((c.r[1], c.r[2]), (0x204, 0x304));
    // saturating (S): MACL clamps to 32 bits
    c.s = true;
    c.macl = 0x7FFF_0000;
    c.step(&mut ram);
    assert_eq!(c.macl, 0x7FFF_FFFF);
    // MAC.L: 64-bit, S saturates to 48 bits
    ram.write32(0x400, 0x4000_0000);
    ram.write32(0x500, 0x4000_0000);
    c.r[0] = 0x400;
    c.r[1] = 0x500;
    c.mach = 0;
    c.macl = 0;
    c.step(&mut ram);
    assert_eq!((c.mach, c.macl), (0x7FFF, 0xFFFF_FFFF), "2^60 saturates at 2^47 - 1");
}

#[test]
fn pref_and_ccn_ops_reach_the_bus() {
    let mut ram = Ram::new();
    ram.prog(0x1000, &[0x0383, 0x0393, 0x03C3]); // PREF @R3; OCBI @R3; MOVCA.L R0,@R3
    let mut c = cpu_at(0x1000);
    c.r[3] = 0x400;
    c.r[0] = 0xCAFE_F00D;
    for _ in 0..3 { c.step(&mut ram); }
    assert_eq!(ram.prefs, vec![0x400]);
    assert_eq!(ram.read32(0x400), 0xCAFE_F00D);
}

/// Round to zero (FPSCR.RM = 1, the Dreamcast default) where the error term matters.
#[test]
fn round_to_zero_corners() {
    use super::fparith::{Mode, Op};
    let rz = Mode { rz: true, dn: true, fast: false };
    let rn = Mode { rz: false, dn: true, fast: false };
    let tiny = f32::from_bits(0x3300_0000); // 2^-25: below half an ulp of 1.0
    assert_eq!(rn.arith(Op::Sub, 1.0f32, tiny), 1.0);
    assert_eq!(rz.arith(Op::Sub, 1.0f32, tiny).to_bits(), 0x3F7F_FFFF, "0.99999994");
    assert_eq!(rz.arith(Op::Div, 1.0f32, 3.0).to_bits(), 0x3EAA_AAAA);
    assert_eq!(rn.arith(Op::Div, 1.0f32, 3.0).to_bits(), 0x3EAA_AAAB);
    assert_eq!(rz.arith(Op::Mul, f32::MAX, 2.0), f32::MAX, "overflow stops at the largest");
    assert_eq!(rz.arith(Op::Div, 1.0f32, 0.0), f32::INFINITY, "division by zero");
    assert_eq!(rz.sqrt(2.0f32).to_bits(), 0x3FB5_04F3);
    assert_eq!(rz.arith(Op::Mul, f32::from_bits(1), 1.0), 0.0, "DN: denormals are zero");
    assert_eq!(rz.from_int_f32(0x7FFF_FFFF).to_bits(), 0x4EFF_FFFF);
    assert_eq!(rz.narrow(1.0 + 1e-12), 1.0);
    assert_eq!(rz.narrow(1.0 - 1e-12).to_bits(), 0x3F7F_FFFF);
}

#[test]
fn ftrc_saturates() {
    let mut ram = Ram::new();
    ram.prog(0x1000, &[0xF03D, 0xF13D, 0xF23D]); // FTRC FR0 / FR1 / FR2,FPUL
    let mut c = cpu_at(0x1000);
    c.fr[0] = 1e20f32.to_bits();
    c.fr[1] = (-1e20f32).to_bits();
    c.fr[2] = f32::NAN.to_bits();
    let mut out = Vec::new();
    for _ in 0..3 { c.step(&mut ram); out.push(c.fpul); }
    assert_eq!(out, vec![0x7FFF_FF80, 0x8000_0000, 0x8000_0000]);
}

/// FRCHG swaps the banks; LDS FPSCR with a new FR bit does too; FSCHG flips SZ.
#[test]
fn fpscr_bank_switch() {
    let mut ram = Ram::new();
    ram.prog(0x1000, &[0xFBFD, 0xF3FD, 0x406A]); // FRCHG; FSCHG; LDS R0,FPSCR
    let mut c = cpu_at(0x1000);
    c.fpscr = 0;
    c.fr[3] = 1;
    c.xf[3] = 2;
    c.step(&mut ram);
    assert_eq!((c.fr[3], c.xf[3], c.fpscr), (2, 1, FPSCR_FR));
    c.step(&mut ram);
    assert_eq!(c.fpscr, FPSCR_FR | FPSCR_SZ);
    c.r[0] = 0xFFFF_FFFF ^ FPSCR_FR;
    c.step(&mut ram);
    assert_eq!((c.fr[3], c.fpscr), (1, FPSCR_MASK & !FPSCR_FR));
}

#[test]
fn save_state_round_trip() {
    let mut c = cpu_at(0x1234);
    c.fr[5] = 42;
    c.pending = Some(0x2000);
    let config = bincode::config::standard();
    let bytes = bincode::serde::encode_to_vec(&c, config).unwrap();
    let (back, _): (Sh4, usize) = bincode::serde::decode_from_slice(&bytes, config).unwrap();
    assert_eq!((back.pc, back.fr[5], back.pending, back.check_fd), (0x1234, 42, Some(0x2000), true));
}

/// Interpreter speed on a mixed loop (integer, load/store, FPU, delayed branch).
/// cargo test --release --lib sh4_speed -- --ignored --nocapture
#[test]
#[ignore]
fn sh4_speed() {
    let mut ram = Ram::new();
    // loop: ADD #1,R2; MOV.L R2,@R4; MOV.L @R4,R3; FADD FR1,FR0; FMUL FR2,FR1; DT R1; BF/S loop; NOP
    ram.prog(0x1000, &[0x7201, 0x2422, 0x6342, 0xF010, 0xF122, 0x4110, 0x8FF9, 0x0009]);
    let mut c = cpu_at(0x1000);
    c.r[1] = 10_000_000;
    c.r[4] = 0x800;
    c.fpscr = 0x40001;
    let t = std::time::Instant::now();
    let mut n = 0u64;
    while c.r[1] != 0 || c.pending.is_some() { c.step(&mut ram); n += 1; }
    let s = t.elapsed().as_secs_f64();
    println!("{n} instructions in {s:.2} s = {:.0} MIPS (the SH-4 runs ~200 MIPS peak)", n as f64 / s / 1e6);
}
