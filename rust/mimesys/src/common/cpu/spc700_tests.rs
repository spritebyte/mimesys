//! SingleStepTests SPC700 (MIT, github.com/SingleStepTests/spc700), converted to one line
//! per test by tools/spc700_sst/convert.py. `sample` runs the checked-in sample (4 tests
//! per opcode); `full` (ignored) runs a whole converted file:
//! SPC700_SST=/path/file.txt cargo test --release spc700_full -- --ignored

use super::*;
use std::collections::HashMap;

struct Ram { m: HashMap<u16, u8>, cycles: u32 }
impl SpcBus for Ram {
    fn read(&mut self, a: u16) -> u8 { self.cycles += 1; *self.m.get(&a).unwrap_or(&0) }
    fn write(&mut self, a: u16, v: u8) { self.cycles += 1; self.m.insert(a, v); }
    fn idle(&mut self) { self.cycles += 1; }
}

fn nums(s: &str) -> Vec<u32> { s.split_whitespace().map(|x| x.parse().unwrap()).collect() }
fn ram(s: &str) -> Vec<(u16, u8)> {
    s.split_whitespace().map(|p| { let (a, v) = p.split_once(':').unwrap(); (a.parse().unwrap(), v.parse().unwrap()) }).collect()
}

fn run(text: &str) -> (usize, Vec<String>, Vec<String>) {
    let (mut ok, mut bad_ops, mut report) = (0, std::collections::BTreeMap::new(), Vec::new());
    for line in text.lines() {
        let f: Vec<&str> = line.split(';').collect();
        let (i, im, fi, fm, cyc) = (nums(f[0]), ram(f[1]), nums(f[2]), ram(f[3]), f[4].parse::<u32>().unwrap());
        let mut bus = Ram { m: im.iter().cloned().collect(), cycles: 0 };
        let mut cpu = Spc700::new();
        cpu.pc = i[0] as u16; cpu.a = i[1] as u8; cpu.x = i[2] as u8; cpu.y = i[3] as u8; cpu.s = i[4] as u8; cpu.p = i[5] as u8;
        let op = *bus.m.get(&cpu.pc).unwrap_or(&0);
        if matches!(op, 0xEF | 0xFF) { ok += 1; continue; }   // SLEEP / STOP never finish
        cpu.step(&mut bus);
        let got = vec![cpu.pc as u32, cpu.a as u32, cpu.x as u32, cpu.y as u32, cpu.s as u32, cpu.p as u32];
        let mem_bad: Vec<String> = fm.iter().filter(|&&(a, v)| *bus.m.get(&a).unwrap_or(&0) != v)
            .map(|&(a, v)| format!("{a:04X}={:02X} want {v:02X}", bus.m.get(&a).unwrap_or(&0))).collect();
        if got == fi && mem_bad.is_empty() && bus.cycles == cyc {
            ok += 1;
        } else {
            *bad_ops.entry(format!("{op:02X}")).or_insert(0) += 1;
            if report.len() < 30 {
                report.push(format!("op {op:02X}: got {got:?} want {fi:?} cycles {} want {cyc} mem {mem_bad:?} | {line}", bus.cycles));
            }
        }
    }
    (ok, bad_ops.into_iter().map(|(k, n)| format!("{k}x{n}")).collect(), report)
}

#[test]
fn spc700_sample() {
    let (ok, bad, report) = run(include_str!("../tests/data/spc700_sample.txt"));
    assert!(bad.is_empty(), "{ok} passed; failing {bad:?}\n{}", report.join("\n"));
}

#[test]
#[ignore]
fn spc700_full() {
    let text = std::fs::read_to_string(std::env::var("SPC700_SST").expect("set SPC700_SST")).unwrap();
    let (ok, bad, report) = run(&text);
    assert!(bad.is_empty(), "{ok} passed; failing {bad:?}\n{}", report.join("\n"));
}
