//! The SH-2 core against SingleStepTests/sh4 (MIT): the instructions the SH-2 shares with
//! the SH-4. Each test runs four instructions: NOP, the one under test, ADD R1,R1 (its
//! delay slot when it branches), then NOP or (at a branch target) ADD R2,R2. Data reads
//! return the recorded values; writes and all registers are compared.
//! The sample (5 tests per instruction) is in tests/data; the full set (67,500 tests) is
//! made by tools/sh4_sst/convert.py, run with SH2_SST=<file> and --ignored.

use super::*;
use std::collections::HashMap;

struct TestBus {
    ops: [u16; 5],
    start: u32,
    fetches: u32,
    /// the address the CPU will fetch its next instruction from
    fetch_at: u32,
    reads: HashMap<u32, u32>,
    writes: Vec<(u32, u32, u32)>,
}

impl TestBus {
    fn data(&self, a: u32, mask: u32) -> u32 { self.reads.get(&a).copied().unwrap_or(0) & mask }
}

impl Sh2Bus for TestBus {
    fn read8(&mut self, a: u32) -> u8 { self.data(a, 0xFF) as u8 }
    fn read16(&mut self, a: u32) -> u16 {
        if a == self.fetch_at && self.fetches != u32::MAX {
            // an instruction fetch: the four addresses from the start hold the list, every
            // other address ADD R2,R2 (a short branch back can land inside the four)
            self.fetches = u32::MAX; // (one fetch per step)
            let k = a.wrapping_sub(self.start) / 2;
            return if a.wrapping_sub(self.start) < 8 { self.ops[k as usize] } else { self.ops[4] };
        }
        self.data(a, 0xFFFF) as u16
    }
    fn read32(&mut self, a: u32) -> u32 { self.data(a, !0) }
    fn write8(&mut self, a: u32, v: u8) { self.writes.push((a, v as u32, 1)) }
    fn write16(&mut self, a: u32, v: u16) { self.writes.push((a, v as u32, 2)) }
    fn write32(&mut self, a: u32, v: u32) { self.writes.push((a, v, 4)) }
}

fn hexes(s: &str) -> Vec<u32> { s.split_whitespace().map(|x| u32::from_str_radix(x, 16).unwrap()).collect() }

fn accesses(s: &str) -> Vec<(u32, u32, u32)> {
    s.split(',').filter(|x| !x.is_empty()).map(|x| {
        let v: Vec<u32> = x.split(':').map(|y| u32::from_str_radix(y, 16).unwrap()).collect();
        (v[0], v[1], v[2])
    }).collect()
}

fn regs(c: &Sh2) -> Vec<u32> {
    let mut v = c.r.to_vec();
    v.extend([c.pc.wrapping_sub(4), c.gbr, c.sr.get(), c.vbr, c.mach, c.macl, c.pr]);
    v
}

/// (passed, failing opcodes with counts, report lines)
fn run(text: &str) -> (usize, Vec<String>, Vec<String>) {
    let (mut ok, mut bad, mut report) = (0, std::collections::BTreeMap::new(), Vec::new());
    for line in text.lines().filter(|l| !l.starts_with('#') && !l.is_empty()) {
        let f: Vec<&str> = line.split('|').collect();
        let op = u16::from_str_radix(f[0], 16).unwrap();
        // DIV1 Rn,Rn: the tests come from Reicast, which shifts Rn before reading it as Rm
        // (giving 0); ares (and this core) reads both first. No program does this.
        if op & 0xF00F == 0x3004 && (op >> 8 & 15) == (op >> 4 & 15) { continue; }
        let (mut init, mut want) = (hexes(f[1]), hexes(f[2]));
        let _ = &mut init;
        // STC SR,Rn / STC.L SR,@-Rn: the SH-4's SR has bits the SH-2 doesn't (MD RB BL FD)
        let stc_sr = op & 0xF0FF == 0x0002 || op & 0xF0FF == 0x4003;
        // (STC SR,R1: the ADD R1,R1 after it doubles the SH-4 bits too; can't be masked)
        if op == 0x0102 { continue; }
        // a delayed branch to itself (disp -2) runs again as the 4th instruction; Reicast runs
        // a branch's delay slot inside the branch, so it counts one instruction more
        if matches!(op, 0x8DFE | 0x8FFE | 0xAFFE) { continue; }
        if op & 0xF0FF == 0x0002 { want[(op >> 8 & 15) as usize] &= 0x3F3; }
        let reads = accesses(f[3]);
        let mut bus = TestBus { ops: [0x0009, op, 0x311C, 0x0009, 0x322C], start: init[16], fetches: 0, fetch_at: 0,
                                reads: reads.iter().map(|&(a, v, _)| (a, v)).collect(), writes: Vec::new() };
        let mut c = Sh2::new();
        c.exceptions = 0;
        c.r.copy_from_slice(&init[..16]);
        c.pc = init[16].wrapping_add(4);
        c.gbr = init[17];
        c.sr.set(init[18]);
        c.vbr = init[19];
        c.mach = init[20];
        c.macl = init[21];
        c.pr = init[22];
        for k in 0..4 {
            bus.fetches = k;
            bus.fetch_at = c.instruction_pc();
            c.step(&mut bus);
        }
        let got = regs(&c);
        let mut want_writes = accesses(f[4]);
        if stc_sr { for w in want_writes.iter_mut().filter(|w| w.2 == 4) { if w.1 >> 28 != 0 || w.1 & 0x8000 != 0 { w.1 &= 0x3F3; } } }
        if got == want && bus.writes == want_writes {
            ok += 1;
        } else {
            *bad.entry(format!("{op:04X}")).or_insert(0) += 1;
            if report.len() < 20 {
                let diff: Vec<String> = (0..23).filter(|&i| got[i] != want[i]).map(|i| {
                    let name = ["R0","R1","R2","R3","R4","R5","R6","R7","R8","R9","R10","R11","R12","R13","R14","R15","PC","GBR","SR","VBR","MACH","MACL","PR"][i];
                    format!("{name} {:X} want {:X}", got[i], want[i])
                }).collect();
                report.push(format!("op {op:04X}: {diff:?} writes {:X?} want {:X?}", bus.writes, want_writes));
            }
        }
    }
    (ok, bad.into_iter().map(|(k, n)| format!("{k}x{n}")).collect(), report)
}

#[test]
fn sh2_sample() {
    let (ok, bad, report) = run(include_str!("../tests/data/sh2_sample.txt"));
    assert!(bad.is_empty(), "{ok} passed; failing {bad:?}\n{}", report.join("\n"));
    assert!(ok > 600);
}

#[test]
#[ignore]
fn sh2_full() {
    let text = std::fs::read_to_string(std::env::var("SH2_SST").expect("set SH2_SST")).unwrap();
    let (ok, bad, report) = run(&text);
    assert!(bad.is_empty(), "{ok} passed; failing {bad:?}\n{}", report.join("\n"));
}

/// A flat RAM bus for hand-written programs.
struct Ram(Vec<u8>);
impl Sh2Bus for Ram {
    fn read8(&mut self, a: u32) -> u8 { self.0[a as usize % self.0.len()] }
    fn read16(&mut self, a: u32) -> u16 { (self.read8(a) as u16) << 8 | self.read8(a + 1) as u16 }
    fn read32(&mut self, a: u32) -> u32 { (self.read16(a) as u32) << 16 | self.read16(a + 2) as u32 }
    fn write8(&mut self, a: u32, v: u8) { let n = self.0.len(); self.0[a as usize % n] = v; }
    fn write16(&mut self, a: u32, v: u16) { self.write8(a, (v >> 8) as u8); self.write8(a + 1, v as u8); }
    fn write32(&mut self, a: u32, v: u32) { self.write16(a, (v >> 16) as u16); self.write16(a + 2, v as u16); }
}

/// Run one instruction with R1 = rm, R2 = rn and T; returns (R2, T).
fn one(op: u16, rm: u32, rn: u32, t: bool) -> (u32, bool) {
    let mut ram = Ram(vec![0; 0x100]);
    ram.write16(0x10, op);
    let mut c = Sh2::new();
    c.exceptions = 0;
    c.pc = 0x14;
    c.r[1] = rm;
    c.r[2] = rn;
    c.sr.t = t;
    c.step(&mut ram);
    (c.r[2], c.sr.t)
}

/// Carry and overflow corners random tests rarely reach (Rm = R1, Rn = R2).
#[test]
fn sh2_flag_corners() {
    // ADDC: 0 + FFFFFFFF + T: the carry comes from adding T
    assert_eq!(one(0x321E, 0xFFFF_FFFF, 0, true), (0, true));
    assert_eq!(one(0x321E, 0xFFFF_FFFF, 0, false), (0xFFFF_FFFF, false));
    // SUBC: 0 - 0 - T borrows
    assert_eq!(one(0x321A, 0, 0, true), (0xFFFF_FFFF, true));
    // NEGC: -0 - T borrows
    assert_eq!(one(0x621A, 0, 0, true), (0xFFFF_FFFF, true));
    assert_eq!(one(0x621A, 0, 0, false), (0, false));
    // ADDV: 7FFFFFFF + 1 overflows; -1 + 1 doesn't
    assert_eq!(one(0x321F, 1, 0x7FFF_FFFF, false), (0x8000_0000, true));
    assert_eq!(one(0x321F, 1, 0xFFFF_FFFF, false), (0, false));
    // SUBV: 80000000 - 1 overflows; 0 - 1 doesn't; 7FFFFFFF - (-1) overflows
    assert_eq!(one(0x321B, 1, 0x8000_0000, false), (0x7FFF_FFFF, true));
    assert_eq!(one(0x321B, 1, 0, false), (0xFFFF_FFFF, false));
    assert_eq!(one(0x321B, 0xFFFF_FFFF, 0x7FFF_FFFF, false), (0x8000_0000, true));
}

/// A level-4 interrupt with vector 0x40: SR and PC pushed, I = 4, handler runs, RTE
/// returns to the instruction it interrupted; SLEEP waits for it.
#[test]
fn sh2_interrupt_and_rte() {
    let mut ram = Ram(vec![0; 0x1000]);
    ram.write32(0, 0x300);              // reset PC
    ram.write32(4, 0x800);              // reset SP
    ram.write32(0x40 * 4, 0x200);       // vector 0x40 -> handler 200
    ram.write16(0x300, 0x001B);         // SLEEP
    ram.write16(0x302, 0xE155);         // MOV #55,R1
    ram.write16(0x200, 0xE277);         // handler: MOV #77,R2
    ram.write16(0x202, 0x002B);         // RTE
    ram.write16(0x204, 0x0009);         // (delay slot) NOP
    let mut c = Sh2::new();
    c.step(&mut ram);                   // reset, SLEEP
    c.step(&mut ram);                   // still asleep
    assert_eq!(c.instruction_pc(), 0x300);
    c.sr.i = 0;
    assert!(c.accepts(4));
    c.interrupt(&mut ram, 4, 0x40);
    assert_eq!((c.instruction_pc(), c.sr.i, c.r[15]), (0x200, 4, 0x7F8));
    assert_eq!((ram.read32(0x7F8), ram.read32(0x7FC)), (0x300, 0x000), "PC, SR pushed");
    for _ in 0..3 { c.step(&mut ram); }  // MOV, RTE, NOP
    assert_eq!((c.instruction_pc(), c.sr.i, c.r[15], c.r[2]), (0x300, 0, 0x800, 0x77));
    c.step(&mut ram);                   // SLEEP again: the interrupt woke it
    c.step(&mut ram);                   // MOV #55,R1
    assert_eq!(c.r[1], 0x55);
}
