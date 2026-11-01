//! SingleStepTests 65x02 (MIT, github.com/SingleStepTests/65x02), converted to one line
//! per test by tools/m65c02_sst/convert.py. `sc02_sample` runs the checked-in sample
//! (8 tests of every opcode, synertek65c02 set); `sc02_full` (ignored) runs a whole
//! converted file: M65C02_SST=/path/file.txt cargo test --release sc02_full -- --ignored

use super::*;

struct Ram { m: Vec<u8> }
impl Bus65 for Ram {
    fn read(&mut self, a: u16) -> u8 { self.m[a as usize] }
    fn write(&mut self, a: u16, v: u8) { self.m[a as usize] = v; }
}

fn regs(s: &str) -> [u32; 6] {
    let v: Vec<u32> = s.split_whitespace().map(|x| x.parse().unwrap()).collect();
    [v[0], v[1], v[2], v[3], v[4], v[5]]
}

fn ram(s: &str) -> Vec<(u16, u8)> {
    s.split_whitespace().map(|p| {
        let (a, v) = p.split_once(':').unwrap();
        (a.parse().unwrap(), v.parse().unwrap())
    }).collect()
}

/// Runs the tests; returns (passed, failures per opcode, first few failure lines).
fn run(text: &str, variant: Variant) -> (usize, [u32; 256], Vec<String>) {
    let mut bus = Ram { m: vec![0; 0x10000] };
    let (mut ok, mut fails, mut report) = (0, [0u32; 256], Vec::new());
    for line in text.lines() {
        let f: Vec<&str> = line.split(';').collect();
        let (ir, im, fr, fm, cyc) = (regs(f[0]), ram(f[1]), regs(f[2]), ram(f[3]), f[4].parse::<u32>().unwrap());
        for &(a, v) in &im { bus.m[a as usize] = v; }
        let mut cpu = Cpu65c02::new(variant);
        cpu.pc = ir[0] as u16; cpu.s = ir[1] as u8; cpu.a = ir[2] as u8;
        cpu.x = ir[3] as u8; cpu.y = ir[4] as u8; cpu.p = ir[5] as u8;
        let op = bus.m[cpu.pc as usize];
        let n = cpu.step(&mut bus);
        let got = [cpu.pc as u32, cpu.s as u32, cpu.a as u32, cpu.x as u32, cpu.y as u32, cpu.p as u32];
        let mem_bad: Vec<_> = fm.iter().filter(|&&(a, v)| bus.m[a as usize] != v)
            .map(|&(a, v)| format!("{a:04X}={:02X} (want {v:02X})", bus.m[a as usize])).collect();
        if got == fr && mem_bad.is_empty() && n == cyc {
            ok += 1;
        } else {
            fails[op as usize] += 1;
            if report.len() < 40 {
                report.push(format!("op {op:02X}: regs {got:?} want {fr:?}, cycles {n} want {cyc}, mem {mem_bad:?} | {line}"));
            }
        }
        for &(a, _) in im.iter().chain(fm.iter()) { bus.m[a as usize] = 0; }
    }
    (ok, fails, report)
}

#[test]
fn sc02_sample() {
    let text = include_str!("../tests/data/m65sc02_sample.txt");
    let (ok, fails, report) = run(text, Variant::Sc02);
    let bad: Vec<String> = (0..256).filter(|&i| fails[i] > 0).map(|i| format!("{i:02X}x{}", fails[i])).collect();
    assert!(bad.is_empty(), "{ok} passed; failing opcodes {bad:?}\n{}", report.join("\n"));
}

#[test]
#[ignore]
fn sc02_full() {
    let path = std::env::var("M65C02_SST").expect("set M65C02_SST to a converted file");
    let text = std::fs::read_to_string(path).unwrap();
    let (ok, fails, report) = run(&text, Variant::Sc02);
    let bad: Vec<String> = (0..256).filter(|&i| fails[i] > 0).map(|i| format!("{i:02X}x{}", fails[i])).collect();
    assert!(bad.is_empty(), "{ok} passed; failing opcodes {bad:?}\n{}", report.join("\n"));
}
