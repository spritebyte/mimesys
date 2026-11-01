//! SingleStepTests 65816 (MIT, github.com/SingleStepTests/65816), converted to one line
//! per test by tools/w65816_sst/convert.py. `sample` runs the checked-in sample (4 tests
//! of every opcode in each mode); `full` (ignored) runs a whole converted file:
//! W65816_SST=/path/file.txt cargo test --release w65816 -- --ignored

use super::*;
use std::collections::HashMap;

struct Ram { m: HashMap<u32, u8>, cycles: u32 }
impl Bus816 for Ram {
    fn read(&mut self, a: u32) -> u8 { self.cycles += 1; *self.m.get(&a).unwrap_or(&0) }
    fn write(&mut self, a: u32, v: u8) { self.cycles += 1; self.m.insert(a, v); }
    fn idle(&mut self) { self.cycles += 1; }
}

fn nums(s: &str) -> Vec<u32> { s.split_whitespace().map(|x| x.parse().unwrap()).collect() }
fn ram(s: &str) -> Vec<(u32, u8)> {
    s.split_whitespace().map(|p| { let (a, v) = p.split_once(':').unwrap(); (a.parse().unwrap(), v.parse().unwrap()) }).collect()
}

fn run(text: &str) -> (usize, Vec<String>, Vec<String>) {
    let (mut ok, mut bad_ops, mut report) = (0, std::collections::BTreeMap::new(), Vec::new());
    for line in text.lines() {
        let f: Vec<&str> = line.split(';').collect();
        let (i, im, fi, fm, cyc) = (nums(f[0]), ram(f[1]), nums(f[2]), ram(f[3]), f[4].parse::<u32>().unwrap());
        let mut bus = Ram { m: im.iter().cloned().collect(), cycles: 0 };
        let mut cpu = W65816::new();
        cpu.pc = i[8] << 16 | i[0];
        cpu.s = i[1] as u16; cpu.p = i[2] as u8; cpu.a = i[3] as u16; cpu.x = i[4] as u16; cpu.y = i[5] as u16;
        cpu.b = i[6] as u8; cpu.d = i[7] as u16; cpu.e = i[9] != 0;
        // the tests' random S keeps its high byte in emulation mode; the chip holds it at 01
        if cpu.e { cpu.s = 0x0100 | (cpu.s & 0xFF); }
        let op = *bus.m.get(&cpu.pc).unwrap_or(&0);
        cpu.step(&mut bus);
        // block moves repeat until done: the tests run the whole move
        if matches!(op, 0x44 | 0x54) {
            let start = i[8] << 16 | i[0];
            let mut guard = 0;
            while cpu.pc == start && guard < 100_000 { cpu.step(&mut bus); guard += 1; }
        }
        let got = vec![cpu.pc & 0xFFFF, cpu.s as u32, cpu.p as u32, cpu.a as u32, cpu.x as u32, cpu.y as u32,
                       cpu.b as u32, cpu.d as u32, cpu.pc >> 16, cpu.e as u32];
        let mem_bad: Vec<String> = fm.iter().filter(|&&(a, v)| *bus.m.get(&a).unwrap_or(&0) != v)
            .map(|&(a, v)| format!("{a:06X}={:02X} want {v:02X}", bus.m.get(&a).unwrap_or(&0))).collect();
        // WAI / STP: the tests count the cycles up to the halt
        let cyc_ok = bus.cycles == cyc || matches!(op, 0xCB | 0xDB);
        if got == fi && mem_bad.is_empty() && cyc_ok {
            ok += 1;
        } else {
            let key = format!("{op:02X}{}", if i[9] != 0 { "e" } else { "n" });
            *bad_ops.entry(key).or_insert(0) += 1;
            if report.len() < 30 {
                report.push(format!("op {op:02X}: got {got:?} want {fi:?} cycles {} want {cyc} mem {mem_bad:?} | {line}", bus.cycles));
            }
        }
    }
    (ok, bad_ops.into_iter().map(|(k, n)| format!("{k}x{n}")).collect(), report)
}

#[test]
fn w65816_sample() {
    let (ok, bad, report) = run(include_str!("../tests/data/w65816_sample.txt"));
    assert!(bad.is_empty(), "{ok} passed; failing {bad:?}\n{}", report.join("\n"));
}

#[test]
#[ignore]
fn w65816_full() {
    let text = std::fs::read_to_string(std::env::var("W65816_SST").expect("set W65816_SST")).unwrap();
    let (ok, bad, report) = run(&text);
    assert!(bad.is_empty(), "{ok} passed; failing {bad:?}\n{}", report.join("\n"));
}
