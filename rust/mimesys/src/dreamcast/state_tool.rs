//! Looks inside a player's save state: writes main RAM (and video RAM) next to it and prints
//! the CPU registers, so the code a game is stuck in can be disassembled.
//!
//! DC_STATE=path.state cargo test --release --lib state_dump -- --ignored --nocapture

use super::State;

#[test]
#[ignore]
fn state_dump() {
    let path = std::env::var("DC_STATE").expect("DC_STATE=path to a .state file");
    let data = std::fs::read(&path).unwrap();
    let config = bincode::config::standard().with_fixed_int_encoding();
    let (s, _): (State, usize) = bincode::serde::decode_from_slice(&data, config).expect("a Dreamcast state");
    std::fs::write(format!("{path}.ram"), &s.dc.ram).unwrap();
    std::fs::write(format!("{path}.vram"), &s.dc.vram).unwrap();
    let regs: Vec<u8> = s.dc.pvr.regs.iter().flat_map(|r| r.to_le_bytes()).collect();
    std::fs::write(format!("{path}.pvr"), regs).unwrap();
    // the picture on screen, as a PPM
    let mut out = vec![0u8; 640 * 480 * 4];
    s.dc.pvr.output(&s.dc.vram, &mut out);
    let mut ppm = b"P6\n640 480\n255\n".to_vec();
    for p in out.chunks(4) { ppm.extend_from_slice(&p[..3]); }
    std::fs::write(format!("{path}.ppm"), ppm).unwrap();
    let c = &s.cpu;
    println!("PC {:08X} PR {:08X} T {} VBR {:08X} GBR {:08X} SPC {:08X} SSR {:08X}", c.pc, c.pr, c.t, c.vbr, c.gbr, c.spc, c.ssr);
    println!("R {:08X?}", c.r);
    println!("FR {:08X?} FPSCR {:08X} FPUL {:08X}", c.fr, c.fpscr, c.fpul);
    println!("frame {}", s.frame);
}

/// Runs a save state for a while without the BIOS ROM or the disc (fine while the game
/// stays in RAM), with the speed log and debug report going to DC_LOG.
/// DC_STATE=... DC_FRAMES=300 DC_LOG=/tmp/x.log cargo test --release --lib state_run -- --ignored --nocapture
#[test]
#[ignore]
fn state_run() {
    use crate::common::system::System;
    let path = std::env::var("DC_STATE").expect("DC_STATE");
    let frames: u32 = std::env::var("DC_FRAMES").ok().and_then(|v| v.parse().ok()).unwrap_or(300);
    let log = std::env::var("DC_LOG").unwrap_or_else(|_| format!("{path}.log"));
    crate::common::debug_log::set_path(&log);
    let mut sys = super::DcSystem::from_rom(&vec![0u8; super::BIOS_SIZE], "state", &[]).unwrap();
    sys.load_state(&std::fs::read(&path).unwrap()).unwrap();
    if std::env::var("DC_TLB").is_ok() { for (k, e) in sys.dc.chip.mmu.utlb.iter().enumerate().filter(|(_, e)| e.v) { println!("UTLB {k}: vpn {:08X} asid {:02X} -> ppn {:08X} size {} pr {} sh {}", e.vpn, e.asid, e.ppn, e.sz, e.pr, e.sh); } }
    if std::env::var("DC_G2").is_ok() { let h = &sys.dc.holly; println!("G2 ch0 {:08X?}; SB 6880.. {:08X?}", &h.g2[0..8], (0x80..0xA0).step_by(4).map(|o| h.r(o)).collect::<Vec<_>>()); }
    if std::env::var("DC_GD").is_ok() { let g = &sys.dc.gd; println!("GD: status {:02X} error {:02X} ireason {:02X} state {:02X} byte_count {} irq {} phase {:?} data {} pos {} dma {} stream {:X?} fad {:X}; G1 GDEN {:X} GDST {:X} GDLEN {:X}; EXT {:08X}", g.status, g.error, g.ireason, g.state, g.byte_count, g.irq, g.phase, g.data.len(), g.pos, g.dma, g.stream, g.fad, sys.dc.holly.g1[5], sys.dc.holly.g1[6], sys.dc.holly.g1[1], 0); }
    sys.set_option("speed_log", 1);
    // DC_NO_DRAW=1: renders draw nothing (profile the CPU side)
    if std::env::var("DC_NO_DRAW").is_ok() { super::render::SKIP_DRAW.store(true, std::sync::atomic::Ordering::Relaxed); }
    // DC_SLOW_OUT=1: the per-pixel framebuffer paths (A/B timing)
    if std::env::var("DC_SLOW_OUT").is_ok() { super::pvr::SLOW_OUTPUT.store(true, std::sync::atomic::Ordering::Relaxed); }
    // DC_BLANK_DISC=1: a disc of empty sectors, so games that keep reading (streamed music)
    // don't stop for want of one
    // DC_DISC=path: a disc image (.iso/.gdi/.cue/.chd) in the drive
    if let Ok(p) = std::env::var("DC_DISC") {
        let mut d = crate::common::disc::Disc::open(std::path::Path::new(&p)).expect("DC_DISC");
        if std::env::var("DC_DISC_GD").is_ok() { d.gd = true; }
        sys.dc.gd.disc = super::gdrom::DiscSlot(Some(std::sync::Arc::new(std::sync::Mutex::new(d))));
    }
    if std::env::var("DC_BLANK_DISC").is_ok() {
        let d = crate::common::disc::Disc::blank(549_150);
        sys.dc.gd.disc = super::gdrom::DiscSlot(Some(std::sync::Arc::new(std::sync::Mutex::new(d))));
    }
    // DC_IDMAP=1: a state saved before the TLB was kept - guess 1 MB identity pages for
    // 0C000000-0CFFFFFF in the wired entries (50-63) and 16 MB...
    if std::env::var("DC_IDMAP").is_ok() {
        sys.dc.chip.mmu.utlb[63] = super::mmu::TlbEntry::load(0xE000_0000, 0x1000_0000 | 0x100 | 0x90 | 0x60 | 4 | 2);
        for k in 0..13u32 {
            let a = 0x0C00_0000 + k * 0x10_0000;
            sys.dc.chip.mmu.utlb[50 + k as usize] = super::mmu::TlbEntry::load(a, a | 0x100 | 0x90 | 0x60 | 8 | 4 | 2);
        }
    }
    if std::env::var("DC_REPORT").is_ok() { sys.set_option("debug_report", 1); }
    sys.set_option("dc_render_thread", if std::env::var("DC_THREAD").is_ok() { 1 } else { 0 });
    // DC_JIT: run the SH-4 through the recompiler
    if std::env::var("DC_JIT").is_ok() { sys.set_option("dc_jit", 1); }
    if let Ok(v) = std::env::var("DC_CPI_HALF") { sys.set_option("dc_cpu_half", v.parse().unwrap()); }
    let t = std::time::Instant::now();
    // DC_WATCH=0C848CA0,0C848D5C: report each change of these RAM words with the PC (traced)
    if let Ok(w) = std::env::var("DC_WATCH") {
        sys.set_option("debug_report", 1);
        for a in w.split(',') {
            let a = u32::from_str_radix(a.trim(), 16).unwrap();
            let i = (a as usize) & (super::bus::RAM_SIZE - 1) & !3;
            sys.watch.push((a, u32::from_le_bytes(sys.dc.ram[i..i + 4].try_into().unwrap())));
        }
    }
    for f in 0..frames {
        // DC_DUMP_AT=n: save that frame's render inputs (dc_frame_N.bin next to the log)
        if std::env::var("DC_DUMP_AT").ok().and_then(|v| v.parse::<u32>().ok()) == Some(f) { sys.dc.dump_frame = true; }
        sys.run_frame(0);
        // DC_SLOTWATCH=14,4840: one line a frame with that slot's registers, position and
        // 40 bytes of sound RAM at the hex offset (sound driver state)
        if let Ok(v) = std::env::var("DC_SLOTWATCH") {
            let v: Vec<usize> = v.split(',').map(|x| usize::from_str_radix(x, if x.len() > 2 { 16 } else { 10 }).unwrap()).collect();
            let a = &sys.dc.aica;
            let k = v[0];
            let r = |o: usize| u16::from_le_bytes([a.regs[k * 0x80 + o], a.regs[k * 0x80 + o + 1]]);
            let m: Vec<String> = a.wave[v[1]..v[1] + 40].chunks(4).map(|c| format!("{:08X}", u32::from_le_bytes(c.try_into().unwrap()))).collect();
            println!("f{f}: slot {k} {:04X} {:04X} {:04X} {:04X} cur {:X} act {} eg {}; {}", r(0), r(4), r(8), r(0xC), a.snd.slots[k].cur >> 12, a.snd.slots[k].active, a.snd.slots[k].eg_state, m.join(" "));
        }
    }
    sys.dc.finish_render();
    // DC_REPORT_END: the full debug report at the end
    if std::env::var("DC_REPORT_END").is_ok() { sys.report(); }
    if std::env::var("DC_SLOTS").is_ok() {
        let a = &sys.dc.aica;
        for (k, sl) in a.snd.slots.iter().enumerate().filter(|(_, sl)| sl.active) {
            let r = |o: usize| u16::from_le_bytes([a.regs[k * 0x80 + o], a.regs[k * 0x80 + o + 1]]);
            println!("slot {k}: regs {:04X?} cur {:08X} eg {} vol {} lpend {}", (0..0x2C).step_by(4).map(r).collect::<Vec<_>>(), sl.cur, sl.eg_state, sl.eg_vol, sl.lpend);
        }
    }
    if let Ok(p) = std::env::var("DC_RAM_OUT") { std::fs::write(&p, &sys.dc.ram).unwrap(); std::fs::write(format!("{p}.snd"), &sys.dc.aica.wave).unwrap(); }
    let el = t.elapsed().as_secs_f64();
    println!("{frames} frames in {el:.2} s ({:.0}% of real time)", 100.0 * frames as f64 / 60.0 / el);
    if let Some(j) = &sys.jit {
        println!("jit (since the last speed line): {} blocks ({} insns), {} runs, {} flushes, {} evicted on {} writes to code lines, {} interpreted, {} ms translating", j.compiled, j.compiled_insns, j.runs, j.flushes, j.evicted, j.dirty_writes, j.cold, j.compile_us / 1000);
    }
    let mut out = vec![0u8; 640 * 480 * 4];
    sys.dc.pvr.output(&sys.dc.vram, &mut out);
    let mut ppm = b"P6\n640 480\n255\n".to_vec();
    for p in out.chunks(4) { ppm.extend_from_slice(&p[..3]); }
    std::fs::write(format!("{log}.ppm"), ppm).unwrap();
    println!("ran {frames} frames; log {log}, screen {log}.ppm");
}
