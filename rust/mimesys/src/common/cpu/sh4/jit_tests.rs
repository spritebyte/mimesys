//! The recompiler against the interpreter: random blocks of translated instructions (with
//! and without a closing branch and its slot), from random CPU states, with registers
//! pointing both into RAM (the inline memory path) and elsewhere (the helper calls), must
//! leave the same registers, memory and clock.

use super::*;
use super::super::SR_MD;
use std::collections::BTreeMap;

const RAM: usize = 1 << 24;

#[derive(Clone)]
struct RamBus {
    ram: Vec<u8>,
    code: Vec<u8>,
    dirty: Vec<u32>,
    other: BTreeMap<u32, u8>,
    seed: u32,
    now: u64,
    prefs: Vec<u32>,
    attn: bool,
}

fn mix(mut x: u32) -> u32 {
    x ^= x >> 16; x = x.wrapping_mul(0x7FEB_352D);
    x ^= x >> 15; x = x.wrapping_mul(0x846C_A68B);
    x ^ x >> 16
}

/// Region 3 (0C000000-0FFFFFFF) is RAM, as on the Dreamcast's P0 mirror.
fn is_ram(a: u32) -> bool { a >> 26 == 3 }

impl Sh4Bus for RamBus {
    fn read8(&mut self, a: u32) -> u8 {
        if is_ram(a) { return self.ram[a as usize & (RAM - 1)]; }
        self.other.get(&a).copied().unwrap_or(mix(a ^ self.seed) as u8)
    }
    fn read16(&mut self, a: u32) -> u16 {
        if is_ram(a) { let i = a as usize & (RAM - 2); return u16::from_le_bytes([self.ram[i], self.ram[i + 1]]); }
        self.read8(a) as u16 | (self.read8(a.wrapping_add(1)) as u16) << 8
    }
    fn read32(&mut self, a: u32) -> u32 {
        if is_ram(a) { let i = a as usize & (RAM - 4); return u32::from_le_bytes(self.ram[i..i + 4].try_into().unwrap()); }
        self.read16(a) as u32 | (self.read16(a.wrapping_add(2)) as u32) << 16
    }
    fn write8(&mut self, a: u32, v: u8) {
        if is_ram(a) { let i = a as usize & (RAM - 1); self.hit(i); self.ram[i] = v; return; }
        self.other.insert(a, v);
    }
    fn write16(&mut self, a: u32, v: u16) {
        if is_ram(a) { let i = a as usize & (RAM - 2); self.hit(i); self.ram[i..i + 2].copy_from_slice(&v.to_le_bytes()); return; }
        self.write8(a, v as u8); self.write8(a.wrapping_add(1), (v >> 8) as u8);
    }
    fn write32(&mut self, a: u32, v: u32) {
        if is_ram(a) { let i = a as usize & (RAM - 4); self.hit(i); self.ram[i..i + 4].copy_from_slice(&v.to_le_bytes()); return; }
        self.write16(a, v as u16); self.write16(a.wrapping_add(2), (v >> 16) as u16);
    }
    fn pref(&mut self, a: u32) { self.prefs.push(a); }
    fn now(&self) -> u64 { self.now }
    fn tick(&mut self, c: u64) { self.now += c; }
    fn jit_mem(&mut self) -> Option<JitMem> { Some(JitMem { ram: self.ram.as_mut_ptr(), direct: 1 << 3, code: self.code.as_ptr() }) }
    fn jit_code(&mut self, p: u32) { self.code[p as usize] = 1; }
    fn jit_uncode(&mut self, p: u32) { self.code[p as usize] = 0; }
    fn jit_attn(&self) -> *const u8 { &self.attn as *const bool as *const u8 }
    fn attention(&self) -> bool { self.attn }
    fn jit_dirty(&mut self, out: &mut Vec<u32>) { out.append(&mut self.dirty); self.attn = false; }
}

impl RamBus {
    fn hit(&mut self, i: usize) {
        if self.code[i >> CODE_SHIFT] != 0 { self.dirty.push(i as u32 & !3); self.attn = true; }
    }
}

fn state(cpu: &Sh4) -> Vec<u8> { bincode::serde::encode_to_vec(cpu, bincode::config::standard()).unwrap() }

#[test]
fn jit_matches_the_interpreter() {
    let mut rng = 0x2468_ACE1_1357_9BDFu64;
    let mut next = move || { rng ^= rng << 13; rng ^= rng >> 7; rng ^= rng << 17; (rng >> 32) as u32 };
    let base = RamBus { ram: vec![0; RAM], code: vec![0; RAM >> CODE_SHIFT], dirty: Vec::new(), other: BTreeMap::new(), seed: 0, now: 0, prefs: Vec::new(), attn: false };
    let mut jit = Jit::new();
    jit.hot = 1;
    let mut kinds = [0u32; 128];
    // inline FPU instructions run: by low nibble, and the xxxD group by its m field (16-31)
    let mut fgroups = [0u32; 32];
    for case in 0..6000u32 {
        let mut bus = base.clone();
        bus.seed = next();
        let mut cpu = Sh4::new();
        // sometimes with the FPU disabled, so FPU instructions trap
        cpu.set_sr(SR_MD | 0xF0 | next() & 0x303 | if next() % 8 == 0 { 1 << 15 } else { 0 });
        cpu.fpscr = next() & 0x0038_0000;
        // mostly the plain host arithmetic, where FPU instructions are done inline
        cpu.fast_fpu = next() % 4 != 0;
        for f in cpu.fr.iter_mut().chain(cpu.xf.iter_mut()) { *f = if next() % 2 == 0 { next() } else { (next() % 9 + 120) << 23 }; }
        for r in cpu.r.iter_mut() {
            *r = match next() % 4 {
                0 => next(),
                1 => (next() % 67).wrapping_sub(33),
                // into RAM, away from the code
                _ => 0x0C40_0000 + (next() & 0x3F_FFFF),
            };
        }
        cpu.pr = 0x0C10_0000 + (next() & 0xFFFE); cpu.mach = next(); cpu.macl = next(); cpu.gbr = 0x0C50_0000 + (next() & 0xFFFF);
        let pc = 0x0C01_0000 + (case & 0xFF) * 0x100;
        cpu.pc = pc;
        // a run of plain instructions, then maybe a branch and its slot
        let len = 1 + next() % 12;
        let mut a = pc;
        let pick = |next: &mut dyn FnMut() -> u32, slot: bool| loop {
            let x = next();
            // the one-opcode kinds, which random picks would almost never reach
            let op = if x >> 28 == 0 { [0x0008u16, 0x0018, 0x0009][(x >> 20) as usize % 3] } else { x as u16 };
            let k = jit_kind(op);
            if translatable(k) && !is_branch(k) && (!slot || slot_ok(op)) { return op; }
        };
        for _ in 0..len { let op = pick(&mut next, false); bus.write16(a, op); a += 2; }
        if next() % 3 != 0 {
            let op = if next() % 8 == 0 { 0x000B } else { loop { let op = next() as u16; if is_branch(KIND[op as usize] & 127) { break op; } } };
            bus.write16(a, op);
            let slot = pick(&mut next, true);
            bus.write16(a + 2, slot);
        } else {
            // something it stops before
            assert!(!translatable(jit_kind(0x001B)));
            bus.write16(a, 0x001B); // SLEEP
        }
        bus.code.fill(0);
        bus.dirty.clear();
        jit.flush();

        let (mut jc, mut jb) = (cpu.clone(), bus.clone());
        let mode = Jit::mode(&jc);
        let b = jit.block(&mut jb, pc, mode).expect("a block");
        let f = b.f.expect("translated");
        let attn = jb.jit_attn();
        let mut ctx = Ctx::new(&mut jc, &mut jb as *mut RamBus as *mut u8, attn);
        let to = jit.enter(&mut ctx, f);
        let n = ctx.insns as u64;
        jc.pc = to;
        jb.now += n;
        for i in (pc..a + 4).step_by(2) {
            let op = jb.read16(i);
            kinds[jit_kind(op) as usize] += 1;
            if op >> 12 == 0xF && mode & 4 != 0 && fpu_inline(op, mode & 1 != 0) {
                fgroups[if op & 15 == 0xD { 16 + (op >> 4 & 15) as usize } else { (op & 15) as usize }] += 1;
            }
        }

        let (mut rc, mut rb) = (cpu.clone(), bus.clone());
        rc.run(&mut rb, n, 1, None);
        // the interpreter's writes to the code pages don't matter here
        // only read for an exception inside a slot, which a block never stops in
        jc.branch_pc = rc.branch_pc;
        rb.code = jb.code.clone();
        rb.dirty = jb.dirty.clone();

        if rb.ram != jb.ram { let i = (0..RAM).find(|&i| rb.ram[i] != jb.ram[i]).unwrap(); eprintln!("ram differs at {i:06X}: {:02X?} / {:02X?}", &rb.ram[i & !3..(i & !3) + 8], &jb.ram[i & !3..(i & !3) + 8]); }
        if rb.other != jb.other { eprintln!("other: {:X?}\n / {:X?}", rb.other, jb.other); }
        if rb.prefs != jb.prefs { eprintln!("prefs {:X?} / {:X?}", rb.prefs, jb.prefs); }
        if state(&rc) != state(&jc) { let (x, y) = (state(&rc), state(&jc)); let i = (0..x.len()).find(|&i| x.get(i) != y.get(i)); eprintln!("cpu state differs at byte {i:?} of {}", x.len()); }
        assert!(state(&rc) == state(&jc) && rb.ram == jb.ram && rb.other == jb.other && rb.prefs == jb.prefs && rb.now == jb.now,
                "case {case}: block at {pc:08X} = {:04X?}\nPC {:08X} / {:08X}, R {:08X?}\n / {:08X?}\nT {} / {}, PR {:08X} / {:08X}, MAC {:08X}{:08X} / {:08X}{:08X}, clock {} / {}",
                (pc..a + 4).step_by(2).map(|i| rb.read16(i)).collect::<Vec<_>>(),
                rc.pc, jc.pc, rc.r, jc.r, rc.t, jc.t, rc.pr, jc.pr, rc.mach, rc.macl, jc.mach, jc.macl, rb.now, jb.now);
    }
    let fmiss: Vec<usize> = (0..32).filter(|&g| g != 13 && g != 15 && g != 16 + 0xC && g != 16 + 0xD && fgroups[g] == 0).collect();
    assert!(fmiss.is_empty(), "inline FPU groups never ran: {fmiss:X?}");
    let covered = (1..128u8).filter(|&k| translatable(k) && kinds[k as usize] > 0).count();
    let all = (1..128u8).filter(|&k| translatable(k)).count();
    let miss: Vec<u8> = (1..128u8).filter(|&k| translatable(k) && kinds[k as usize] == 0).collect();
    assert!(covered == all, "every translated kind ran: {covered} of {all}, missing {miss:?}");
}

/// A write into a page that translated code came from drops its blocks, so the next pass
/// runs the new code.
#[test]
fn writes_to_translated_code_drop_it() {
    let mut bus = RamBus { ram: vec![0; RAM], code: vec![0; RAM >> CODE_SHIFT], dirty: Vec::new(), other: BTreeMap::new(), seed: 1, now: 0, prefs: Vec::new(), attn: false };
    let mut cpu = Sh4::new();
    let pc = 0x0C01_0000;
    // MOV.L R1,@R2 ; ADD #1,R3 ; BRA pc ; NOP
    for (k, op) in [0x2212u16, 0x7301, 0xAFFC, 0x0009].iter().enumerate() { bus.write16(pc + 2 * k as u32, *op); }
    cpu.pc = pc;
    // the store rewrites itself as it was and turns ADD #1,R3 into ADD #5,R3
    cpu.r[2] = pc;
    cpu.r[1] = 0x7305_2212;
    let mut jit = Jit::new();
    jit.hot = 1;
    cpu.run_jit(&mut bus, &mut jit, 8, 1, None);
    // the first pass was already translated, so it added 1; the second added 5
    assert_eq!(cpu.r[3], 6);
    assert_eq!(cpu.pc, pc);
    assert!(jit.compiled == 2, "translated again after the write: {}", jit.compiled);
}

/// cargo test --release --lib jit_compile_speed -- --ignored --nocapture
#[test]
#[ignore]
fn jit_compile_speed() {
    for (name, code) in [("NOP; RTS; NOP", vec![0x0009u16, 0x000B, 0x0009]),
                         ("ADD; MOV.L @R1,R2; MOV.L R2,@R3; BT", vec![0x7101u16, 0x6212, 0x2322, 0x89FC]),
                         ("8 loads/stores, BF", vec![0x6212u16, 0x2322, 0x6312, 0x2422, 0x6512, 0x2622, 0x6712, 0x2822, 0x8BF0])] {
        let mut bus = RamBus { ram: vec![0; RAM], code: vec![0; RAM >> CODE_SHIFT], dirty: Vec::new(), other: BTreeMap::new(), seed: 1, now: 0, prefs: Vec::new(), attn: false };
        for (k, op) in code.iter().enumerate() { bus.write16(0x0C01_0000 + 2 * k as u32, *op); }
        let mut jit = Jit::new();
        let n = 300;
        for _ in 0..n { jit.blocks.clear(); jit.cache.fill((1, Block::NONE)); jit.block(&mut bus, 0x0C01_0000, 4).unwrap(); }
        println!("{name}: {} us per block", jit.compile_us / n);
    }
}

/// A write next to translated code (the same line, as with a variable after an exception
/// stub) keeps the block.
#[test]
fn writes_next_to_code_keep_it() {
    let mut bus = RamBus { ram: vec![0; RAM], code: vec![0; RAM >> CODE_SHIFT], dirty: Vec::new(), other: BTreeMap::new(), seed: 1, now: 0, prefs: Vec::new(), attn: false };
    let mut cpu = Sh4::new();
    let pc = 0x0C01_0000;
    // ADD #1,R3 ; MOV.L R1,@R2 ; BRA pc ; NOP ; (data)
    for (k, op) in [0x7301u16, 0x2212, 0xAFFC, 0x0009].iter().enumerate() { bus.write16(pc + 2 * k as u32, *op); }
    cpu.pc = pc;
    cpu.r[2] = pc + 8;
    cpu.r[1] = 0x1234_5678;
    let mut jit = Jit::new();
    jit.hot = 1;
    cpu.run_jit(&mut bus, &mut jit, 12, 1, None);
    assert_eq!(cpu.r[3], 3);
    assert_eq!(bus.read32(pc + 8), 0x1234_5678);
    assert_eq!((jit.compiled, jit.evicted), (1, 0));
}


/// Blocks going straight on to the next (linked) give what the interpreter gives: a program
/// of several blocks (a call, forward branches, a counted loop, FPU work) run to the same
/// point both ways, then with the loop known not to be a polling loop (linked too).
#[test]
fn linked_blocks_match_the_interpreter() {
    let pc = 0x0C01_0000u32;
    // 00 MOV #40,R1          02 MOV #0,R2           04 MOV.L @(lit),R3 (0C400000)
    // 06 BSR sub (+0x20)     08 NOP
    // 0A ADD R1,R2           0C MOV.L R2,@R3        0E BT/S skip (+0x02 -> 14)  10 ADD #4,R3
    // 12 ADD #1,R4
    // 14 FMOV.S @R3,FR0      16 FADD FR0,FR1        18 DT R1               1A BF 0A
    // 1C BRA 1C              1E NOP
    // 20 lit 0C400000
    // 26 sub: ADD #7,R5      28 RTS                 2A NOP
    let prog: [(u32, u16); 18] = [
        (0x00, 0xE128), (0x02, 0xE200), (0x04, 0xD306), (0x06, 0xB00E), (0x08, 0x0009),
        (0x0A, 0x321C), (0x0C, 0x2322), (0x0E, 0x8D01), (0x10, 0x7304), (0x12, 0x7401),
        (0x14, 0xF038), (0x16, 0xF100), (0x18, 0x4110), (0x1A, 0x8BF6), (0x1C, 0xAFFE), (0x1E, 0x0009),
        (0x26, 0x7507), (0x28, 0x000B),
    ];
    let mut bus = RamBus { ram: vec![0; RAM], code: vec![0; RAM >> CODE_SHIFT], dirty: Vec::new(), other: BTreeMap::new(), seed: 1, now: 0, prefs: Vec::new(), attn: false };
    for &(o, op) in &prog { bus.write16(pc + o, op); }
    bus.write16(pc + 0x2A, 0x0009);
    bus.write32(pc + 0x20, 0x0C40_0000);
    let mut cpu = Sh4::new();
    cpu.set_sr(super::super::SR_MD | 0xF0);
    cpu.fast_fpu = true;
    cpu.pc = pc;
    for idle_known in [false, true] {
        let (mut ic, mut ib) = (cpu.clone(), bus.clone());
        ic.run(&mut ib, 3000, 1, None);
        let (mut jc, mut jb) = (cpu.clone(), bus.clone());
        let mut jit = Jit::new();
        jit.hot = 1;
        if idle_known { jit.not_idle(pc + 0x0A); }
        let mut loops = (u32::MAX, 0u32);
        // in slices, as the system runs it, watching for polling loops
        let mut t = 0;
        while t < 3000 { t += 250; while jb.now < t { jc.run_jit(&mut jb, &mut jit, t, 1, Some(&mut loops)); } }
        assert_eq!((jc.pc, jc.r, jc.fr, jc.t), (ic.pc, ic.r, ic.fr, ic.t), "idle known {idle_known}");
        assert!(jb.ram == ib.ram);
        let linked = jit.links.iter().filter(|&&f| f != 0).count();
        assert!(linked >= 3, "links made: {linked}");
        // with the loop known, its back branch is linked too: far fewer returns to the loop
        println!("idle known {idle_known}: {} block runs, {linked} links", jit.runs);
    }
}

/// `RamBus` behind a pretend MMU: the 1 KB pages outside RAM fault when `bad` says so (as
/// the Dreamcast's TLB does: the access does nothing, and the instruction's later ones
/// fail too, until the CPU takes the fault); P2 reaches RAM untranslated.
#[derive(Clone)]
struct MmuBus { inner: RamBus, fault: Option<u32>, bad: u32 }

impl MmuBus {
    /// The address to use, or None when the access faults.
    fn tr(&mut self, a: u32, write: bool) -> Option<u32> {
        if self.fault.is_some() { return None; }
        if a >> 29 == 5 { return Some(a & 0x1FFF_FFFF); }
        if !is_ram(a) && mix(a >> 10 ^ self.bad) % 3 == 0 {
            self.fault = Some(if write { 0x060 } else { 0x040 });
            return None;
        }
        Some(a)
    }
}

impl Sh4Bus for MmuBus {
    fn read8(&mut self, a: u32) -> u8 { match self.tr(a, false) { Some(a) => self.inner.read8(a), None => 0 } }
    fn read16(&mut self, a: u32) -> u16 { match self.tr(a, false) { Some(a) => self.inner.read16(a), None => 0 } }
    fn read32(&mut self, a: u32) -> u32 { match self.tr(a, false) { Some(a) => self.inner.read32(a), None => 0 } }
    fn read64(&mut self, a: u32) -> u64 { match self.tr(a, false) { Some(a) => self.inner.read64(a), None => 0 } }
    fn write8(&mut self, a: u32, v: u8) { if let Some(a) = self.tr(a, true) { self.inner.write8(a, v) } }
    fn write16(&mut self, a: u32, v: u16) { if let Some(a) = self.tr(a, true) { self.inner.write16(a, v) } }
    fn write32(&mut self, a: u32, v: u32) { if let Some(a) = self.tr(a, true) { self.inner.write32(a, v) } }
    fn write64(&mut self, a: u32, v: u64) { if let Some(a) = self.tr(a, true) { self.inner.write64(a, v) } }
    fn pref(&mut self, a: u32) { if let Some(a) = self.tr(a, true) { self.inner.pref(a) } }
    fn now(&self) -> u64 { self.inner.now }
    fn tick(&mut self, c: u64) { self.inner.now += c; }
    fn mmu(&self) -> bool { true }
    fn mmu_fault(&mut self) -> Option<u32> { self.fault.take() }
    fn mmu_faulted(&self) -> bool { self.fault.is_some() }
    fn jit_fetch_pa(&mut self, va: u32, _md: bool) -> Option<u32> { if is_ram(va) { Some(va) } else { None } }
    fn jit_mem(&mut self) -> Option<JitMem> { self.inner.jit_mem() }
    fn jit_code(&mut self, p: u32) { self.inner.jit_code(p) }
    fn jit_uncode(&mut self, p: u32) { self.inner.jit_uncode(p) }
    fn jit_attn(&self) -> *const u8 { std::ptr::null() }
    fn attention(&self) -> bool { true }
    fn jit_dirty(&mut self, out: &mut Vec<u32>) { self.inner.jit_dirty(out) }
}

/// With the MMU on: random blocks whose accesses may fault leave what the interpreter
/// (`step_mmu`) leaves: the registers as before the faulting instruction (a slot's branch
/// for a slot), memory untouched by it, and the TLB exception taken at the same PC.
#[test]
fn jit_with_the_mmu_matches_the_interpreter() {
    let mut rng = 0x1357_9BDF_2468_ACE1u64;
    let mut next = move || { rng ^= rng << 13; rng ^= rng >> 7; rng ^= rng << 17; (rng >> 32) as u32 };
    let ram = RamBus { ram: vec![0; RAM], code: vec![0; RAM >> CODE_SHIFT], dirty: Vec::new(), other: BTreeMap::new(), seed: 0, now: 0, prefs: Vec::new(), attn: false };
    let base = MmuBus { inner: ram, fault: None, bad: 0 };
    let mut jit = Jit::new();
    jit.hot = 1;
    let (mut faults, mut slot_faults) = (0, 0);
    for case in 0..6000u32 {
        let mut bus = base.clone();
        bus.inner.seed = next();
        bus.bad = next();
        let mut cpu = Sh4::new();
        cpu.set_sr(SR_MD | 0xF0 | next() & 0x303);
        cpu.vbr = 0x8C00_0000;
        cpu.fpscr = next() & 0x0038_0000;
        cpu.fast_fpu = next() % 4 != 0;
        for f in cpu.fr.iter_mut().chain(cpu.xf.iter_mut()) { *f = next(); }
        for r in cpu.r.iter_mut() {
            *r = match next() % 4 {
                0 => next() & 0x7FFF_FFFF,
                1 => 0x0100_0000 + (next() & 0xFFFF),
                _ => 0x0C40_0000 + (next() & 0x3F_FFFF),
            };
        }
        cpu.pr = 0x0C10_0000 + (next() & 0xFFFE); cpu.mach = next(); cpu.macl = next(); cpu.gbr = 0x0100_0000 + (next() & 0xFFFF);
        let pc = 0x0C01_0000 + (case & 0xFF) * 0x100;
        cpu.pc = pc;
        let len = 1 + next() % 12;
        let mut a = pc;
        let pick = |next: &mut dyn FnMut() -> u32, slot: bool| loop {
            let op = next() as u16;
            let k = jit_kind(op);
            if translatable(k) && !is_branch(k) && (!slot || slot_ok(op)) { return op; }
        };
        for _ in 0..len { let op = pick(&mut next, false); bus.inner.write16(a, op); a += 2; }
        let op = loop { let op = next() as u16; if is_branch(KIND[op as usize] & 127) { break op; } };
        bus.inner.write16(a, op);
        let slot = pick(&mut next, true);
        bus.inner.write16(a + 2, slot);
        bus.inner.code.fill(0);
        bus.inner.dirty.clear();
        jit.flush();

        let (mut jc, mut jb) = (cpu.clone(), bus.clone());
        let mut ctx = Ctx::new(&mut jc, &mut jb as *mut MmuBus as *mut u8, &ATTN_SET);
        let before = jit.compiled;
        jc.run_mmu_block(&mut jb, &mut jit, &mut ctx, u64::MAX, 1);
        assert!(jit.compiled == before + 1, "case {case}: translated");
        let faulted = jc.pc == 0x8C00_0400;
        // the interpreter, one instruction at a time, until it's where the block left off
        let (mut rc, mut rb) = (cpu.clone(), bus.clone());
        let mut same = false;
        for step in 0..(a - pc) / 2 + 2 {
            rc.step(&mut rb);
            jc.branch_pc = rc.branch_pc;
            jc.cur = rc.cur;
            jc.in_slot = rc.in_slot;
            jc.trapped = rc.trapped;
            if state(&rc) == state(&jc) && rb.inner.ram == jb.inner.ram && rb.inner.other == jb.inner.other && rb.inner.prefs == jb.inner.prefs {
                same = true;
                if faulted { faults += 1; if rc.spc == a { slot_faults += 1; } }
                let _ = step;
                break;
            }
        }
        assert!(same, "case {case}: block at {pc:08X} = {:04X?}\nPC {:08X} / {:08X} SPC {:08X} / {:08X}, R {:08X?}\n / {:08X?}\nT {} / {}, PR {:08X} / {:08X}",
                (pc..a + 4).step_by(2).map(|i| rb.inner.read16(i)).collect::<Vec<_>>(),
                rc.pc, jc.pc, rc.spc, jc.spc, rc.r, jc.r, rc.t, jc.t, rc.pr, jc.pr);
    }
    println!("{faults} faulted, {slot_faults} in a slot");
    assert!(faults > 500 && slot_faults > 20, "faults {faults}, in slots {slot_faults}");
}
