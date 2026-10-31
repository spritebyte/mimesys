//! SingleStepTests 680x0 (github.com/SingleStepTests/680x0, 68000/v1), converted to one
//! line per test by tools/m68000_sst/convert.py:
//!     name|state|ram|state|ram|length
//! with state = d0-d7 a0-a6 usp ssp sr pc prefetch0 prefetch1 in hex, ram = addr:value,...
//!
//! `sst_sample` runs the checked-in sample (common/tests/data/m68000_sample.txt, a few
//! tests of every instruction); `sst_full` (ignored by default) runs a whole converted
//! suite: M68K_SST_DIR=/path/to/converted cargo test --release sst_full -- --ignored

use std::collections::HashMap;
use super::{M68000, M68kBus};

struct Mem(HashMap<u32, u8>);

impl M68kBus for Mem {
    fn read_byte(&mut self, a: u32) -> u8 { *self.0.get(&a).unwrap_or(&0) }
    fn read_word(&mut self, a: u32) -> u16 { (self.read_byte(a) as u16) << 8 | self.read_byte(a + 1) as u16 }
    fn write_byte(&mut self, a: u32, v: u8) { self.0.insert(a, v); }
    fn write_word(&mut self, a: u32, v: u16) { self.write_byte(a, (v >> 8) as u8); self.write_byte(a + 1, v as u8); }
}

struct State { v: Vec<u32> }

fn parse_state(s: &str) -> State {
    State { v: s.split(' ').map(|x| u32::from_str_radix(x, 16).unwrap()).collect() }
}

fn parse_ram(s: &str) -> Vec<(u32, u8)> {
    if s.is_empty() { return Vec::new(); }
    s.split(',').map(|p| {
        let (a, v) = p.split_once(':').unwrap();
        (u32::from_str_radix(a, 16).unwrap(), u8::from_str_radix(v, 16).unwrap() )
    }).collect()
}

/// Tests whose expected results disagree with real hardware (and MAME / Musashi):
/// - ASR Dn by more than the operand size of a negative value: the suite clears C and X,
///   but the 68000 shifts a bit at a time, so the last bit out is the sign bit (C = X = 1).
/// - two ASL.b tests whose expected register value isn't a shift of the input at all.
fn known_bad(name: &str, init: &State) -> bool {
    if name.starts_with("e502 [ASL.b Q, D2] 1583") || name.starts_with("e502 [ASL.b Q, D2] 1761") { return true; }
    let op = u32::from_str_radix(name.split(' ').next().unwrap_or("0"), 16).unwrap_or(0);
    // ASR register form with a register count: 1110 ccc 0 ss 1 00 rrr
    if op & 0xF118 == 0xE000 && op & 0x20 != 0 && (op >> 6) & 3 != 3 {
        let bits = [8, 16, 32][((op >> 6) & 3) as usize];
        let count = init.v[((op >> 9) & 7) as usize] & 63;
        let v = init.v[(op & 7) as usize] as u64 & ((1u64 << bits) - 1);
        return count > bits && v >> (bits - 1) & 1 != 0;
    }
    false
}

/// Run one test line; `Err` describes the first mismatch. Known-bad tests pass.
pub fn run_line(line: &str) -> Result<(), String> {
    let f: Vec<&str> = line.split('|').collect();
    let (init, iram, fin, fram) = (parse_state(f[1]), parse_ram(f[2]), parse_state(f[3]), parse_ram(f[4]));
    if known_bad(f[0], &init) { return Ok(()); }
    let length: u32 = f[5].parse().unwrap();
    let mut mem = Mem(iram.into_iter().collect());
    let mut cpu = M68000::new();
    let i = &init.v;
    cpu.set_state(
        [i[0], i[1], i[2], i[3], i[4], i[5], i[6], i[7]],
        [i[8], i[9], i[10], i[11], i[12], i[13], i[14]],
        i[15], i[16], i[17] as u16, i[18], [i[19] as u16, i[20] as u16],
    );
    let cycles = cpu.step(&mut mem);

    let mut errs = Vec::new();
    let names = ["d0","d1","d2","d3","d4","d5","d6","d7","a0","a1","a2","a3","a4","a5","a6","usp","ssp","sr","pc","pf0","pf1"];
    let got = [
        cpu.r[0], cpu.r[1], cpu.r[2], cpu.r[3], cpu.r[4], cpu.r[5], cpu.r[6], cpu.r[7],
        cpu.r[8], cpu.r[9], cpu.r[10], cpu.r[11], cpu.r[12], cpu.r[13], cpu.r[14],
        cpu.usp(), cpu.ssp(), cpu.sr as u32, cpu.pc.wrapping_sub(4), cpu.prefetch[0] as u32, cpu.prefetch[1] as u32,
    ];
    for k in 0..21 {
        if got[k] != fin.v[k] { errs.push(format!("{} {:x} want {:x}", names[k], got[k], fin.v[k])); }
    }
    for (a, v) in fram {
        let g = *mem.0.get(&a).unwrap_or(&0);
        if g != v { errs.push(format!("ram[{a:x}] {g:x} want {v:x}")); }
    }
    if cycles != length { errs.push(format!("cycles {cycles} want {length}")); }
    if errs.is_empty() { Ok(()) } else { Err(errs.join(", ")) }
}

fn run_text(text: &str, label: &str, report: usize) -> (usize, usize) {
    let (mut pass, mut fail) = (0, 0);
    for line in text.lines() {
        if line.is_empty() { continue; }
        match run_line(line) {
            Ok(()) => pass += 1,
            Err(e) => {
                if fail < report { println!("{label}: {} -> {e}", line.split('|').next().unwrap()); }
                fail += 1;
            }
        }
    }
    (pass, fail)
}

#[test]
fn sst_sample() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/common/tests/data/m68000_sample.txt");
    let Ok(text) = std::fs::read_to_string(path) else { return };
    let (pass, fail) = run_text(&text, "sample", 40);
    println!("68000 sample: {pass} passed, {fail} failed");
    assert_eq!(fail, 0);
}

#[test]
#[ignore]
fn sst_full() {
    let dir = std::env::var("M68K_SST_DIR").expect("set M68K_SST_DIR to the converted tests");
    let only = std::env::var("M68K_SST_ONLY").ok();
    let report: usize = std::env::var("M68K_SST_REPORT").ok().and_then(|s| s.parse().ok()).unwrap_or(3);
    let mut files: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
    files.sort();
    let (mut tp, mut tf) = (0, 0);
    let mut summary = Vec::new();
    for p in files {
        let name = p.file_stem().unwrap().to_string_lossy().to_string();
        if let Some(o) = &only { if !o.split(',').any(|x| x == name) { continue; } }
        let text = std::fs::read_to_string(&p).unwrap();
        let (pass, fail) = run_text(&text, &name, report);
        if fail > 0 { summary.push(format!("{name}: {fail} failed / {}", pass + fail)); }
        tp += pass;
        tf += fail;
    }
    for s in &summary { println!("{s}"); }
    println!("68000 full suite: {tp} passed, {tf} failed");
    assert_eq!(tf, 0);
}

// ---------------------------------------------------------------- beyond the suite

/// Flat 64 KB test memory with a vector table.
fn machine(code: &[u8]) -> (M68000, Mem) {
    let mut m = Mem(HashMap::new());
    let put_l = |m: &mut Mem, a: u32, v: u32| for k in 0..4 { m.0.insert(a + k, (v >> (24 - 8 * k)) as u8); };
    put_l(&mut m, 0, 0x8000);            // SSP
    put_l(&mut m, 4, 0x1000);            // PC
    for v in 2..64 { put_l(&mut m, v * 4, 0x2000 + v * 0x10); }   // each vector -> 2000 + 16 * n
    for (k, b) in code.iter().enumerate() { m.0.insert(0x1000 + k as u32, *b); }
    // every handler: RTE
    for v in 2..64 { m.0.insert(0x2000 + v * 0x10, 0x4E); m.0.insert(0x2001 + v * 0x10, 0x73); }
    let mut cpu = M68000::new();
    cpu.reset(&mut m);
    (cpu, m)
}

#[test]
fn reset_loads_ssp_and_pc() {
    let (cpu, _) = machine(&[0x4E, 0x71]);
    assert_eq!(cpu.r[15], 0x8000);
    assert_eq!(cpu.instruction_pc(), 0x1000);
    assert_eq!(cpu.sr, 0x2700);
    assert_eq!(cpu.prefetch[0], 0x4E71);
}

#[test]
fn autovectored_interrupt_respects_the_mask() {
    // MOVE #$2000,SR (mask 0); NOP; NOP; BRA *
    let (mut cpu, mut m) = machine(&[0x46, 0xFC, 0x20, 0x00, 0x4E, 0x71, 0x4E, 0x71, 0x60, 0xFE]);
    cpu.ipl = 3;
    cpu.step(&mut m);                         // mask still 7 when this starts
    assert_eq!(cpu.sr & 0x0700, 0);
    let c = cpu.step(&mut m);                 // takes level 3, autovector 27
    assert_eq!(cpu.instruction_pc(), 0x2000 + 27 * 0x10);
    assert_eq!(cpu.sr & 0x0700, 0x0300);
    assert_eq!(c, 46, "CLK timing: n nn ns ni(4) n- n nS ns nV nv np n np");
    cpu.ipl = 0;
    cpu.step(&mut m);                         // RTE
    assert_eq!(cpu.instruction_pc(), 0x1004, "returns to the interrupted instruction");
}

#[test]
fn stop_waits_for_an_interrupt() {
    // STOP #$2000; NOP
    let (mut cpu, mut m) = machine(&[0x4E, 0x72, 0x20, 0x00, 0x4E, 0x71]);
    cpu.step(&mut m);
    assert!(cpu.stopped);
    for _ in 0..5 { assert_eq!(cpu.step(&mut m), 2); }
    cpu.ipl = 1;
    cpu.step(&mut m);
    assert!(!cpu.stopped);
    assert_eq!(cpu.instruction_pc(), 0x2000 + 25 * 0x10);
    cpu.ipl = 0;
    cpu.step(&mut m);                         // RTE
    assert_eq!(cpu.instruction_pc(), 0x1004, "resumes after the STOP");
}

#[test]
fn illegal_line_a_and_privilege() {
    let (mut cpu, mut m) = machine(&[0x4A, 0xFC]);   // ILLEGAL
    assert_eq!(cpu.step(&mut m), 34);
    assert_eq!(cpu.instruction_pc(), 0x2000 + 4 * 0x10);
    let (mut cpu, mut m) = machine(&[0xA1, 0x23]);   // line A
    cpu.step(&mut m);
    assert_eq!(cpu.instruction_pc(), 0x2000 + 10 * 0x10);
    // ANDI #0,SR to drop to user mode, then RESET (privileged)
    let (mut cpu, mut m) = machine(&[0x02, 0x7C, 0x00, 0x00, 0x4E, 0x70]);
    cpu.step(&mut m);
    assert!(!cpu.supervisor());
    cpu.step(&mut m);
    assert_eq!(cpu.instruction_pc(), 0x2000 + 8 * 0x10);
    assert!(cpu.supervisor());
    assert_eq!(cpu.r[15], 0x8000 - 6);
}

#[test]
fn level_7_is_edge_triggered() {
    // BRA * with the mask at 7
    let (mut cpu, mut m) = machine(&[0x60, 0xFE]);
    cpu.ipl = 7;
    cpu.step(&mut m);
    assert_eq!(cpu.instruction_pc(), 0x2000 + 31 * 0x10, "NMI taken despite mask 7");
    cpu.step(&mut m);                         // RTE back
    cpu.step(&mut m);
    assert_eq!(cpu.instruction_pc(), 0x1000, "held level 7 doesn't retrigger");
}
