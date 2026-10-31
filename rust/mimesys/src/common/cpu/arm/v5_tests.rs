//! ARMv5TE (ARM946E-S) cases from Unicorn, made by tools/arm946_ref/gen.py:
//!     name|thumb|count|regs cpsr|code|pre|regs cpsr after|writes
//! `arm946_sample` runs the checked-in sample (common/tests/data/arm946_sample.txt);
//! `arm946_full` (ignored) runs a bigger file: ARM946_CASES=<file> cargo test arm946_full -- --ignored

use std::collections::HashMap;
use super::arm7tdmi::{Arm7tdmi, ArmBus};

const CODE: u32 = 0x1000;

fn pattern(a: u32) -> u8 {
    (((a as u64 * 0x9E37_79B1) >> 13) as u32 ^ (a >> 3)) as u8
}

struct Mem {
    over: HashMap<u32, u8>,
    writes: HashMap<u32, u8>,
}

impl Mem {
    fn byte(&self, a: u32) -> u8 {
        if let Some(v) = self.writes.get(&a) { return *v; }
        if let Some(v) = self.over.get(&a) { return *v; }
        pattern(a)
    }
    fn put(&mut self, a: u32, v: u8) { self.writes.insert(a, v); }
}

impl ArmBus for Mem {
    fn read8(&mut self, a: u32, _: u8) -> u8 { self.byte(a) }
    fn read16(&mut self, a: u32, _: u8) -> u16 { let a = a & !1; self.byte(a) as u16 | (self.byte(a + 1) as u16) << 8 }
    fn read32(&mut self, a: u32, _: u8) -> u32 {
        let a = a & !3;
        (0..4).map(|i| (self.byte(a + i) as u32) << (8 * i)).sum()
    }
    fn write8(&mut self, a: u32, v: u8, _: u8) { self.put(a, v) }
    fn write16(&mut self, a: u32, v: u16, _: u8) { let a = a & !1; self.put(a, v as u8); self.put(a + 1, (v >> 8) as u8) }
    fn write32(&mut self, a: u32, v: u32, _: u8) {
        let a = a & !3;
        for i in 0..4 { self.put(a + i, (v >> (8 * i)) as u8) }
    }
}

fn hex_list(s: &str) -> Vec<u32> {
    s.split_whitespace().map(|x| u32::from_str_radix(x, 16).unwrap()).collect()
}

fn pairs(s: &str) -> Vec<(u32, u32)> {
    if s.is_empty() { return Vec::new(); }
    s.split(',').map(|p| {
        let (a, v) = p.split_once(':').unwrap();
        (u32::from_str_radix(a, 16).unwrap(), u32::from_str_radix(v, 16).unwrap())
    }).collect()
}

pub fn run_line(line: &str) -> Result<(), String> {
    let f: Vec<&str> = line.split('|').collect();
    let thumb = f[1] == "1";
    let count: usize = f[2].parse().unwrap();
    let init = hex_list(f[3]);
    let code = hex_list(f[4]);
    let fin = hex_list(f[6]);
    let mut mem = Mem { over: HashMap::new(), writes: HashMap::new() };
    for (k, c) in code.iter().enumerate() {
        let width = if thumb { 2 } else { 4 };
        for b in 0..width { mem.over.insert(CODE + (k as u32) * width + b, (c >> (8 * b)) as u8); }
    }
    for (a, w) in pairs(f[5]) {
        for b in 0..4 { mem.over.insert(a + b, (w >> (8 * b)) as u8); }
    }
    let mut cpu = Arm7tdmi::new_v5();
    cpu.set_cpsr(init[16]);
    cpu.r[..15].copy_from_slice(&init[..15]);
    cpu.jump(&mut mem, init[15]);
    for _ in 0..count { cpu.step(&mut mem); }

    let mut errs = Vec::new();
    for i in 0..15 {
        if cpu.r[i] != fin[i] { errs.push(format!("r{i} {:x} want {:x}", cpu.r[i], fin[i])); }
    }
    if cpu.pc() != fin[15] { errs.push(format!("pc {:x} want {:x}", cpu.pc(), fin[15])); }
    if cpu.cpsr != fin[16] { errs.push(format!("cpsr {:x} want {:x}", cpu.cpsr, fin[16])); }
    let want: HashMap<u32, u8> = pairs(f[7]).into_iter().map(|(a, v)| (a, v as u8)).collect();
    // compare stored bytes, ignoring stores of a byte's own old value
    let mut got: HashMap<u32, u8> = mem.writes.clone();
    got.retain(|a, v| { let old = mem.over.get(a).copied().unwrap_or_else(|| pattern(*a)); *v != old || want.contains_key(a) });
    let want_changed: HashMap<u32, u8> = want.iter().filter(|(a, v)| {
        let old = mem.over.get(a).copied().unwrap_or_else(|| pattern(**a));
        **v != old || got.contains_key(a)
    }).map(|(a, v)| (*a, *v)).collect();
    if got != want_changed {
        let mut g: Vec<_> = got.iter().collect(); g.sort();
        let mut w: Vec<_> = want_changed.iter().collect(); w.sort();
        errs.push(format!("writes {g:x?} want {w:x?}"));
    }
    if errs.is_empty() { Ok(()) } else { Err(errs.join(", ")) }
}

fn run_text(text: &str, report: usize) -> (usize, usize, Vec<String>) {
    let (mut pass, mut fail) = (0, 0);
    let mut kinds: HashMap<String, usize> = HashMap::new();
    for line in text.lines().filter(|l| !l.is_empty()) {
        match run_line(line) {
            Ok(()) => pass += 1,
            Err(e) => {
                let name = line.split('|').next().unwrap();
                if fail < report { println!("{name}: {e}"); }
                *kinds.entry(name.split(' ').next().unwrap().to_string()).or_default() += 1;
                fail += 1;
            }
        }
    }
    let mut k: Vec<String> = kinds.into_iter().map(|(n, c)| format!("{n}: {c}")).collect();
    k.sort();
    (pass, fail, k)
}

#[test]
fn arm946_sample() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/common/tests/data/arm946_sample.txt");
    let text = std::fs::read_to_string(path).expect("arm946_sample.txt");
    let (pass, fail, kinds) = run_text(&text, 30);
    println!("ARM946 sample: {pass} passed, {fail} failed {kinds:?}");
    assert_eq!(fail, 0);
    assert!(pass > 500);
}

#[test]
#[ignore]
fn arm946_full() {
    let path = std::env::var("ARM946_CASES").expect("ARM946_CASES=<file>");
    let (pass, fail, kinds) = run_text(&std::fs::read_to_string(path).unwrap(), 20);
    println!("ARM946: {pass} passed, {fail} failed {kinds:?}");
    assert_eq!(fail, 0);
}

/// A bus with a CP15 that remembers writes and can move the vectors up.
struct Cp15Bus { mem: Mem, regs: HashMap<u32, u32>, high: bool }

impl ArmBus for Cp15Bus {
    fn read8(&mut self, a: u32, k: u8) -> u8 { self.mem.read8(a, k) }
    fn read16(&mut self, a: u32, k: u8) -> u16 { self.mem.read16(a, k) }
    fn read32(&mut self, a: u32, k: u8) -> u32 { self.mem.read32(a, k) }
    fn write8(&mut self, a: u32, v: u8, k: u8) { self.mem.write8(a, v, k) }
    fn write16(&mut self, a: u32, v: u16, k: u8) { self.mem.write16(a, v, k) }
    fn write32(&mut self, a: u32, v: u32, k: u8) { self.mem.write32(a, v, k) }
    fn cp15_read(&mut self, reg: u32) -> u32 { *self.regs.get(&reg).unwrap_or(&0) }
    fn cp15_write(&mut self, reg: u32, v: u32) {
        if reg == 0x0100 { self.high = v & (1 << 13) != 0; }
        self.regs.insert(reg, v);
    }
    fn exception_base(&self) -> u32 { if self.high { 0xFFFF_0000 } else { 0 } }
}

#[test]
fn cp15_bkpt_and_high_vectors() {
    let mut bus = Cp15Bus { mem: Mem { over: HashMap::new(), writes: HashMap::new() }, regs: HashMap::new(), high: false };
    let code: [u32; 4] = [
        0xEE091F11,      // MCR p15, 0, r1, c9, c1, 0   (DTCM region)
        0xEE193F11,      // MRC p15, 0, r3, c9, c1, 0
        0xEE012F10,      // MCR p15, 0, r2, c1, c0, 0   (control: high vectors)
        0xE1200070,      // BKPT #0
    ];
    for (k, c) in code.iter().enumerate() {
        for b in 0..4 { bus.mem.over.insert(CODE + k as u32 * 4 + b, (c >> (8 * b)) as u8); }
    }
    let mut cpu = Arm7tdmi::new_v5();
    cpu.set_cpsr(0xDF);
    cpu.r[1] = 0x0080_000A;
    cpu.r[2] = 1 << 13;
    cpu.jump(&mut bus, CODE);
    cpu.step(&mut bus);
    assert_eq!(bus.regs.get(&0x0910), Some(&0x0080_000A), "MCR reaches the bus as CRn 9, CRm 1, op2 0");
    cpu.step(&mut bus);
    assert_eq!(cpu.r[3], 0x0080_000A, "MRC reads it back");
    cpu.step(&mut bus);
    assert!(bus.high);
    cpu.step(&mut bus);
    assert_eq!(cpu.pc(), 0xFFFF_000C, "BKPT = prefetch abort, at the high vector");
    assert_eq!(cpu.mode(), 0x17, "abort mode");
    assert_eq!(cpu.r[14], CODE + 12 + 4, "LR = BKPT + 4");
    // on an ARM7TDMI the same MCR is undefined
    let mut cpu = Arm7tdmi::new();
    cpu.set_cpsr(0xDF);
    bus.high = false;
    cpu.jump(&mut bus, CODE);
    cpu.step(&mut bus);
    assert_eq!(cpu.pc(), 0x04, "undefined instruction vector");
}
