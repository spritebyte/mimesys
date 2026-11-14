//! The fast path of `Sh4::run` against the general single-stepping path: every opcode,
//! from several random CPU states, must leave the same registers and memory.

use super::*;
use std::collections::BTreeMap;

/// Memory with a value at every address (a hash of it until written), and a clock.
#[derive(Clone, PartialEq, Debug)]
struct HashBus {
    mem: BTreeMap<u32, u8>,
    seed: u32,
    now: u64,
    prefs: Vec<u32>,
}

fn mix(mut x: u32) -> u32 {
    x ^= x >> 16; x = x.wrapping_mul(0x7FEB_352D);
    x ^= x >> 15; x = x.wrapping_mul(0x846C_A68B);
    x ^ x >> 16
}

impl Sh4Bus for HashBus {
    fn read8(&mut self, a: u32) -> u8 { self.mem.get(&a).copied().unwrap_or(mix(a ^ self.seed) as u8) }
    fn read16(&mut self, a: u32) -> u16 { self.read8(a) as u16 | (self.read8(a.wrapping_add(1)) as u16) << 8 }
    fn read32(&mut self, a: u32) -> u32 { self.read16(a) as u32 | (self.read16(a.wrapping_add(2)) as u32) << 16 }
    fn write8(&mut self, a: u32, v: u8) { self.mem.insert(a, v); }
    fn write16(&mut self, a: u32, v: u16) { self.write8(a, v as u8); self.write8(a.wrapping_add(1), (v >> 8) as u8); }
    fn write32(&mut self, a: u32, v: u32) { self.write16(a, v as u16); self.write16(a.wrapping_add(2), (v >> 16) as u16); }
    fn pref(&mut self, a: u32) { self.prefs.push(a); }
    fn now(&self) -> u64 { self.now }
    fn tick(&mut self, c: u64) { self.now += c; }
}

fn state(cpu: &Sh4) -> Vec<u8> {
    bincode::serde::encode_to_vec(cpu, bincode::config::standard()).unwrap()
}

#[test]
fn fast_path_matches_the_reference() {
    let mut rng = 0x1234_5678u32;
    let mut next = move || { rng = mix(rng.wrapping_add(0x9E37_79B9)); rng };
    let mut fast_kinds = 0;
    for op in 0..=0xFFFFu32 {
        let op = op as u16;
        if fast::KIND[op as usize] != 0 { fast_kinds += 1; }
        let rounds = if fast::KIND[op as usize] != 0 && op >> 12 != 0xF { 6 } else { 1 };
        for round in 0..rounds {
            let mut cpu = Sh4::new();
            cpu.set_sr(SR_MD | 0xF0 | next() & 0x303);
            for r in cpu.r.iter_mut() { *r = next(); }
            // some small values too: shift counts, equal registers, zero
            if round & 1 != 0 { for r in cpu.r.iter_mut() { if next() & 3 == 0 { *r = (next() % 67) as u32 - 33; } } }
            if round == 2 { let v = cpu.r[1]; for r in cpu.r.iter_mut() { if next() & 1 == 0 { *r = v; } } }
            cpu.pr = next(); cpu.mach = next(); cpu.macl = next(); cpu.gbr = next(); cpu.fpul = next();
            for f in cpu.fr.iter_mut() { *f = (next() as f32 / 65536.0 - 30000.0).to_bits(); }
            cpu.vbr = 0x8C00_0000;
            cpu.pc = 0x8C01_0000 + (next() & 0xFFE);
            let mut bus = HashBus { mem: BTreeMap::new(), seed: next(), now: 0, prefs: Vec::new() };
            bus.write16(cpu.pc, op);
            // the next instruction (a delay slot, for branches): often a plain one
            let slot = match next() % 4 { 0 => 0x0009, 1 => 0x7201, 2 => 0x6213, _ => next() as u16 };
            bus.write16(cpu.pc.wrapping_add(2), slot);

            let (mut rc, mut rb) = (cpu.clone(), bus.clone());
            rc.step(&mut rb);
            let mut ticks = 1;
            if rc.pending.is_some() { rc.step(&mut rb); ticks = 2; }
            rb.now += ticks;

            let (mut fc, mut fb) = (cpu.clone(), bus.clone());
            fc.run(&mut fb, 1, 1, None);

            assert!(state(&rc) == state(&fc) && rb == fb,
                    "opcode {op:04X} (slot {slot:04X}, round {round}): PC {:08X} / {:08X}, R {:08X?} / {:08X?}, T {} / {}, PR {:08X} / {:08X}, MAC {:08X}{:08X} / {:08X}{:08X}, clock {} / {}",
                    rc.pc, fc.pc, rc.r, fc.r, rc.t, fc.t, rc.pr, fc.pr, rc.mach, rc.macl, fc.mach, fc.macl, rb.now, fb.now);
        }
    }
    assert!(fast_kinds > 40_000, "most opcodes are on the fast path: {fast_kinds}");
}
