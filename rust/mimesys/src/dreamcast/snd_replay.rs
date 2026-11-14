//! Offline replay of a sound-driver snapshot (dc_sound_ram.bin): sound RAM, AICA registers
//! and ARM7 registers. SND_DUMP = the file, SND_CMDS = 32-bit words to write into the
//! driver's command ring at SND_RING (hex) from its read index at SND_IDX (hex), SND_MS =
//! milliseconds to run.
use super::aica::Aica;
use crate::common::cpu::arm::arm7tdmi::Arm7tdmi;

fn hex(s: &str) -> u32 { u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).unwrap() }

#[test]
#[ignore]
fn replay_sound_driver() {
    let Ok(path) = std::env::var("SND_DUMP") else { return };
    let f = std::fs::read(path).unwrap();
    let w = |a: usize| u32::from_le_bytes(f[a..a + 4].try_into().unwrap());
    let mut a = Aica::new(0);
    a.wave.copy_from_slice(&f[..0x20_0000]);
    a.regs.copy_from_slice(&f[0x20_0000..0x20_8000]);
    for o in [0x2890u32, 0x2894, 0x2898] {
        let v = u16::from_le_bytes([a.regs[o as usize], a.regs[o as usize + 1]]) as u32;
        a.reg_write(o, v, 2);
    }
    a.arm_reset = false;
    // patches: SND_POKE = "addr=byte,addr=byte"
    if let Ok(p) = std::env::var("SND_POKE") {
        for kv in p.split(',') { let (k, v) = kv.split_once('=').unwrap(); a.wave[hex(k) as usize] = hex(v) as u8; }
    }
    let mut arm = Arm7tdmi::new();
    let base = 0x20_8000;
    arm.cpsr = w(base + 64);
    for i in 0..15 { arm.r[i] = w(base + 4 * i); }
    arm.jump(&mut a, w(base + 60).wrapping_sub(8));
    if let (Ok(cmds), Ok(ring), Ok(idx)) = (std::env::var("SND_CMDS"), std::env::var("SND_RING"), std::env::var("SND_IDX")) {
        let (ring, idx) = (hex(&ring) as usize, hex(&idx) as usize);
        let mut k = u32::from_le_bytes(a.wave[idx..idx + 4].try_into().unwrap()) as usize;
        for c in cmds.split(',') {
            a.wave[ring + k..ring + k + 4].copy_from_slice(&hex(c).to_le_bytes());
            k = (k + 4) % 0x100;
        }
    }
    let ms: u64 = std::env::var("SND_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(500);
    let mut pcs = std::collections::BTreeMap::<u32, u64>::new();
    let watch: Vec<u32> = std::env::var("SND_HITS").map(|v| v.split(',').map(hex).collect()).unwrap_or_default();
    let mut hits = vec![0u64; watch.len()];
    let wmem: Option<usize> = std::env::var("SND_WATCH").ok().map(|v| hex(&v) as usize);
    let mut wlog = 0;
    let mut trace: Vec<u32> = Vec::new();
    let trace_from = std::env::var("SND_TRACE").ok().map(|v| hex(&v));
    for s in 1..=ms * 441 / 10 {
        a.advance(s);
        for k in 0..128 {
            arm.fiq_line = a.arm_fiq();
            let pc = arm.pc();
            if let Some(i) = watch.iter().position(|&w| w == pc) {
                hits[i] += 1;
                if hits[i] <= 3 { println!("hit {pc:X}: lr {:X} sp {:X} cpsr {:X} r0 {:X} r1 {:X}", arm.r[14], arm.r[13], arm.cpsr, arm.r[0], arm.r[1]); }
            }
            if trace_from.is_some_and(|t| t == pc) || (!trace.is_empty() && trace.len() < 400) {
                trace.push(pc);
                if trace.len() <= 400 { trace.push(arm.r[0]); }
            }
            let before = wmem.map(|m| a.wave[m]);
            arm.step(&mut a);
            if let (Some(m), Some(b)) = (wmem, before) {
                if a.wave[m] != b && wlog < 6 { wlog += 1; println!("write {m:X}: {b:X} -> {:X} at pc {pc:X} (lr {:X})", a.wave[m], arm.r[14]); }
            }
            if k & 15 == 0 { *pcs.entry(arm.pc() & !0x3F).or_insert(0) += 1; }
        }
    }
    if let Ok(p) = std::env::var("SND_PEEK") {
        for k in p.split(',') {
            let x = hex(k) as usize;
            println!("{x:05X}: {}", a.wave[x..x + 0x30].iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" "));
        }
    }
    println!("hits {:X?}", watch.iter().zip(&hits).collect::<Vec<_>>());
    if !trace.is_empty() { println!("trace (pc, r0): {}", trace.chunks(2).map(|c| format!("{:X}:{:X}", c[0], c.get(1).copied().unwrap_or(0))).collect::<Vec<_>>().join(" ")); }
    let st = &a.snd.stats;
    println!("key-ons {} samples {} playing {}", st.keyons, st.samples, st.playing);
    let mut v: Vec<_> = pcs.into_iter().collect();
    v.sort_by(|x, y| y.1.cmp(&x.1));
    println!("busiest 64-byte blocks: {:X?}", &v[..v.len().min(12)]);
    for (who, m) in a.reg_writes.iter().enumerate() {
        let mut v: Vec<_> = m.iter().collect();
        v.sort_by(|x, y| y.1.cmp(x.1));
        println!("{} writes: {:X?}", ["SH-4", "ARM"][who], &v[..v.len().min(16)]);
    }
}
