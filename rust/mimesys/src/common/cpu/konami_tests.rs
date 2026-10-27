//! Konami CPU against MAME's generated core (tools/konami_ref): each vector is a random
//! state + one instruction; registers, clocks, memory writes and SETLINES must match.

use super::*;

struct Mem { m: Vec<u8>, writes: Vec<(u16, u8)>, lines: Vec<u8> }

impl KonamiBus for Mem {
    fn read(&mut self, a: u16) -> u8 { self.m[a as usize] }
    fn write(&mut self, a: u16, v: u8) { self.m[a as usize] = v; self.writes.push((a, v)); }
    fn set_lines(&mut self, v: u8) { self.lines.push(v); }
}

fn xs(s: &mut u32) -> u32 { *s ^= *s << 13; *s ^= *s >> 17; *s ^= *s << 5; *s }

/// Runs every vector in `text`; returns (passed, failures).
fn run_vectors(text: &str) -> (usize, Vec<String>) {
    let mut ok = 0;
    let mut fails = Vec::new();
    for line in text.lines() {
        if line.contains("SKIP") || line.trim().is_empty() { continue; }
        let parts: Vec<&str> = line.split('|').collect();
        let h = |s: &str| u32::from_str_radix(s, 16).unwrap();
        let i: Vec<u32> = parts[0].split_whitespace().map(h).collect();
        let o: Vec<&str> = parts[1].split_whitespace().collect();
        let w: Vec<(u16, u8)> = parts[2].split_whitespace().map(|p| {
            let (a, v) = p.split_once('=').unwrap();
            (h(a) as u16, h(v) as u8)
        }).collect();
        let lines: Vec<u8> = parts.get(3).map_or(vec![], |p| p.split_whitespace().map(|v| h(v) as u8).collect());

        let mut s = i[0];
        let mut mem = Mem { m: (0..0x10000).map(|_| xs(&mut s) as u8).collect(), writes: vec![], lines: vec![] };
        let pc = i[1] as u16;
        let ops = [i[14] as u8, (i[14] >> 8) as u8, i[15] as u8, (i[15] >> 8) as u8];
        for (k, b) in ops.iter().enumerate() { mem.m[pc.wrapping_add(k as u16) as usize] = *b; }
        let mut c = Konami::new();
        c.pc = pc; c.x = i[2] as u16; c.y = i[3] as u16; c.u = i[4] as u16; c.s = i[5] as u16;
        c.a = i[6] as u8; c.b = i[7] as u8; c.dp = i[8] as u8; c.cc = i[9] as u8; c.dp_low = i[10] as u8;
        c.irq_line = i[11] != 0; c.firq_line = i[12] != 0; c.nmi_pending = i[13] != 0;
        let cycles = c.step(&mut mem);

        let mut got_w: Vec<(u16, u8)> = Vec::new();
        for (k, &(a, v)) in mem.writes.iter().enumerate() {
            if !mem.writes[k + 1..].iter().any(|&(b, _)| b == a) { got_w.push((a, v)); }
        }
        let got = format!("{:x} {:x} {:x} {:x} {:x} {:x} {:x} {:x} {:x} {:x} {:x} {} {}",
            c.pc, c.x, c.y, c.u, c.s, c.a, c.b, c.dp, c.cc, c.dp_low,
            c.pc_transfer.map_or(0, |o| 0x100 | o as u32), c.nmi_pending as u32, cycles);
        let want = o.join(" ");
        if got == want && got_w == w && mem.lines == lines {
            ok += 1;
        } else if fails.len() < 40 {
            fails.push(format!("op {:02x} {:02x} {:02x} {:02x} cc {:02x}\n  want {want} {w:x?} {lines:x?}\n  got  {got} {got_w:x?} {:x?}",
                ops[0], ops[1], ops[2], ops[3], i[9], mem.lines));
        }
    }
    (ok, fails)
}

#[test]
fn konami_matches_mame_sample() {
    let (ok, fails) = run_vectors(include_str!("../tests/data/konami_vectors.txt"));
    assert!(fails.is_empty(), "{ok} passed; failures:\n{}", fails.join("\n"));
    assert!(ok > 1000);
}

/// KONAMI_VECTORS=<file> cargo test --release --lib konami_matches_mame_full -- --ignored
#[test]
#[ignore]
fn konami_matches_mame_full() {
    let text = std::fs::read_to_string(std::env::var("KONAMI_VECTORS").expect("KONAMI_VECTORS")).unwrap();
    let (ok, fails) = run_vectors(&text);
    println!("{ok} passed");
    assert!(fails.is_empty(), "{ok} passed; failures:\n{}", fails.join("\n"));
}
