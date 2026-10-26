//! SingleStepTests ARM7TDMI (github.com/SingleStepTests/ARM7TDMI, made from
//! NanoBoyAdvance): one instruction per test, compared on registers, CPSR/SPSRs,
//! the pipeline and every memory write.
//!
//! A sample of each file is committed (src/common/tests/data/arm7tdmi_sample.bin).
//! Full run: SST_ARM7TDMI=<clone>/v1 cargo test --release --lib arm7tdmi_sst_full -- --ignored --nocapture

use super::arm7tdmi::*;

#[derive(Clone, Debug)]
struct State { r: [u32; 16], fiq: [u32; 7], banks: [[u32; 2]; 4], cpsr: u32, spsr: [u32; 5], pipe: [u32; 2] }

#[derive(Clone, Copy, Debug)]
struct Tr { kind: u32, size: u32, addr: u32, data: u32 }

struct Test { init: State, fin: State, trs: Vec<Tr>, opcode: u32, base: u32 }

fn u32_at(b: &[u8], p: usize) -> u32 { u32::from_le_bytes([b[p], b[p + 1], b[p + 2], b[p + 3]]) }

fn state_at(b: &[u8], p: usize) -> (usize, State) {
    let sz = u32_at(b, p) as usize;
    let v: Vec<u32> = (0..40).map(|i| u32_at(b, p + 8 + i * 4)).collect();
    let mut s = State { r: [0; 16], fiq: [0; 7], banks: [[0; 2]; 4], cpsr: v[31], spsr: [0; 5], pipe: [v[37], v[38]] };
    s.r.copy_from_slice(&v[0..16]);
    s.fiq.copy_from_slice(&v[16..23]);
    for i in 0..4 { s.banks[i] = [v[23 + i * 2], v[24 + i * 2]]; }
    s.spsr.copy_from_slice(&v[32..37]);
    (sz, s)
}

/// Parses a .json.bin file (format from the suite's transcode_json.py).
fn parse(b: &[u8]) -> Vec<Test> {
    assert_eq!(u32_at(b, 0), 0xD33D_BAE0, "not a test file");
    let n = u32_at(b, 4) as usize;
    let mut p = 8;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let full = u32_at(b, p) as usize;
        let mut q = p + 4;
        let (sz, init) = state_at(b, q); q += sz;
        let (sz, fin) = state_at(b, q); q += sz;
        let tsz = u32_at(b, q) as usize;
        let cnt = u32_at(b, q + 8) as usize;
        let trs = (0..cnt).map(|i| {
            let o = q + 12 + i * 24;
            Tr { kind: u32_at(b, o), size: u32_at(b, o + 4), addr: u32_at(b, o + 8), data: u32_at(b, o + 12) }
        }).collect();
        q += tsz;
        let opcode = u32_at(b, q + 8);
        let base = u32_at(b, q + 12);
        out.push(Test { init, fin, trs, opcode, base });
        p += full;
    }
    out
}

struct TBus<'a> { t: &'a Test, writes: Vec<Tr> }

impl TBus<'_> {
    fn code(&self, addr: u32, size: u32) -> u32 {
        let a = if size == 2 { addr & !1 } else { addr & !3 };
        let v = if a == self.t.base { self.t.opcode } else { a };
        if size == 2 { v & 0xFFFF } else { v }
    }
    fn data(&self, addr: u32, size: u32) -> u32 {
        let v = self.t.trs.iter().find(|t| t.kind == 1 && t.addr == addr && t.size == size)
            .or_else(|| self.t.trs.iter().find(|t| t.kind == 1 && t.addr == addr))
            .map_or(0, |t| t.data);
        match size { 1 => v & 0xFF, 2 => v & 0xFFFF, _ => v }
    }
}

impl ArmBus for TBus<'_> {
    fn read8(&mut self, a: u32, _: u8) -> u8 { self.data(a, 1) as u8 }
    fn read16(&mut self, a: u32, acc: u8) -> u16 {
        (if acc & ACCESS_CODE != 0 { self.code(a, 2) } else { self.data(a, 2) }) as u16
    }
    fn read32(&mut self, a: u32, acc: u8) -> u32 {
        if acc & ACCESS_CODE != 0 { self.code(a, 4) } else { self.data(a, 4) }
    }
    fn write8(&mut self, a: u32, v: u8, _: u8) { self.writes.push(Tr { kind: 2, size: 1, addr: a, data: v as u32 }); }
    fn write16(&mut self, a: u32, v: u16, _: u8) { self.writes.push(Tr { kind: 2, size: 2, addr: a, data: v as u32 }); }
    fn write32(&mut self, a: u32, v: u32, _: u8) { self.writes.push(Tr { kind: 2, size: 4, addr: a, data: v }); }
}

/// The suite's layout: `R` is always the user-mode set (r8-r14 included), and each
/// bank holds that mode's live registers. Builds a CPU in the bank's mode from that.
fn setup(s: &State) -> Arm7tdmi {
    let mut c = Arm7tdmi::new();
    c.cpsr = s.cpsr;
    c.spsr = s.spsr;
    c.usr_r8_12.copy_from_slice(&s.r[8..13]);
    c.usr_sp_lr = [s.r[13], s.r[14]];
    c.fiq_r8_12.copy_from_slice(&s.fiq[0..5]);
    c.bank_sp_lr[0] = [s.fiq[5], s.fiq[6]];
    for i in 0..4 { c.bank_sp_lr[i + 1] = s.banks[i]; }
    c.r = s.r;
    match bank_of(s.cpsr) {
        Some(0) => c.r[8..15].copy_from_slice(&s.fiq),
        Some(i) => { c.r[13] = s.banks[i - 1][0]; c.r[14] = s.banks[i - 1][1]; }
        None => {}
    }
    c.pipe = s.pipe;
    c
}

/// The CPU in the suite's layout (see `setup`).
fn export(c: &Arm7tdmi) -> State {
    let mut s = State { r: c.r, fiq: [0; 7], banks: [[0; 2]; 4], cpsr: c.cpsr, spsr: c.spsr, pipe: c.pipe };
    s.fiq[0..5].copy_from_slice(&c.fiq_r8_12);
    s.fiq[5] = c.bank_sp_lr[0][0];
    s.fiq[6] = c.bank_sp_lr[0][1];
    for i in 0..4 { s.banks[i] = c.bank_sp_lr[i + 1]; }
    match bank_of(c.cpsr) {
        Some(0) => {
            s.fiq.copy_from_slice(&c.r[8..15]);
            s.r[8..13].copy_from_slice(&c.usr_r8_12);
            s.r[13] = c.usr_sp_lr[0];
            s.r[14] = c.usr_sp_lr[1];
        }
        Some(i) => {
            s.banks[i - 1] = [c.r[13], c.r[14]];
            s.r[13] = c.usr_sp_lr[0];
            s.r[14] = c.usr_sp_lr[1];
        }
        None => {}
    }
    s
}

/// Returns a description of the first difference, if any.
fn check(t: &Test) -> Option<String> {
    let mut cpu = setup(&t.init);
    let mut bus = TBus { t, writes: vec![] };
    cpu.step(&mut bus);
    let f = &t.fin;
    let g = export(&cpu);
    let mut errs = vec![];
    if g.r != f.r { errs.push(format!("R {:08x?}\n   want {:08x?}", g.r, f.r)); }
    if g.cpsr != f.cpsr { errs.push(format!("CPSR {:08x} want {:08x}", g.cpsr, f.cpsr)); }
    if g.spsr != f.spsr { errs.push(format!("SPSR {:08x?} want {:08x?}", g.spsr, f.spsr)); }
    if g.pipe != f.pipe { errs.push(format!("pipe {:08x?} want {:08x?}", g.pipe, f.pipe)); }
    if g.fiq != f.fiq { errs.push(format!("R_fiq {:08x?} want {:08x?}", g.fiq, f.fiq)); }
    if g.banks != f.banks { errs.push(format!("banks svc/abt/irq/und {:08x?} want {:08x?}", g.banks, f.banks)); }
    let want_w: Vec<(u32, u32, u32)> = t.trs.iter().filter(|t| t.kind == 2).map(|t| (t.size, t.addr, t.data)).collect();
    let got_w: Vec<(u32, u32, u32)> = bus.writes.iter().map(|t| (t.size, t.addr, t.data)).collect();
    if got_w != want_w { errs.push(format!("writes {:08x?} want {:08x?}", got_w, want_w)); }
    if errs.is_empty() { None } else {
        Some(format!("op {:08x} at {:08x} cpsr {:08x}: {}", t.opcode, t.base, t.init.cpsr, errs.join("\n  ")))
    }
}

/// Runs a file's tests; returns (passed, total, first failures).
fn run(bytes: &[u8], show: usize) -> (usize, usize, Vec<String>) {
    let tests = parse(bytes);
    let mut fails = vec![];
    let mut ok = 0;
    for t in &tests {
        match check(t) {
            None => ok += 1,
            Some(e) => if fails.len() < show { fails.push(e) },
        }
    }
    (ok, tests.len(), fails)
}

#[test]
fn arm7tdmi_sst_sample() {
    let (ok, n, fails) = run(include_bytes!("../../tests/data/arm7tdmi_sample.bin"), 5);
    assert!(ok == n, "{ok}/{n} passed\n{}", fails.join("\n"));
}

#[test]
#[ignore]
fn arm7tdmi_sst_full() {
    let dir = std::env::var("SST_ARM7TDMI").expect("SST_ARM7TDMI=<ARM7TDMI clone>/v1");
    let only = std::env::var("SST_ONLY").unwrap_or_default();
    let mut names: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok())
        .map(|e| e.path()).filter(|p| p.to_string_lossy().ends_with(".json.bin")).collect();
    names.sort();
    let (mut all_ok, mut all) = (0, 0);
    for p in names {
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        if !only.is_empty() && !name.contains(&only) { continue; }
        let (ok, n, fails) = run(&std::fs::read(&p).unwrap(), 3);
        println!("{:45} {ok:6}/{n}", name);
        for f in fails { println!("  {f}"); }
        all_ok += ok;
        all += n;
    }
    println!("TOTAL {all_ok}/{all}");
}
