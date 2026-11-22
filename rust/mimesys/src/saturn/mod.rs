//! Sega Saturn. The plan is in the project's `saturn_plan.md`; this is stage 1: two SH-2s
//! (`common/cpu/sh2` + the SH7604's on-chip parts from `m32x/sh7604`), the memory map
//! (`bus.rs`), the SMPC (`smpc.rs`), the SCU's interrupts, timers and DMA (`scu.rs`),
//! VDP2's timing and back screen (`vdp2.rs`), VDP1's command list walk (`vdp1.rs`), the
//! the SCSP sound chip with its 68000 (`scsp.rs`) and a CD block with no disc (`cdblock.rs`).
//!
//! - Clocks: the SH-2s run at 26.87 MHz (320-dot modes) or 28.64 MHz (352, after the
//!   SMPC's CKCHG352); a line is 1708 / 1820 of their clocks, a frame 263 lines (PAL 313).
//! - Interrupts: the SCU drives the master; the slave gets VBlank-IN (level 6, vector 43h)
//!   and HBlank-IN (level 2, vector 41h). A write to MINIT / SINIT is an FRT input capture
//!   on the other CPU (how they signal each other).
//! - The slave and the 68000 start held in reset; the BIOS turns them on through the SMPC.
//! - Backup RAM (32 KB) and the SMPC's battery memory (language, sound settings) are saved
//!   together as Saturn/Save/saturn_internal.sav.

pub mod bus;
pub mod cdblock;
pub mod scsp;
pub mod scu;
pub mod scudsp;
pub mod smpc;
pub mod vdp1;
pub mod vdp2;
pub mod vdp2_render;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod render_tests;
#[cfg(test)]
mod sound_tests;
#[cfg(test)]
mod cd_tests;
#[cfg(test)]
mod tests_support {
    /// A system whose fake BIOS just spins
    pub fn spin_system() -> super::SaturnSystem { super::tests::spin_system() }
}

use crate::common::disc::Disc;
use crate::common::input;
use crate::common::system::{DisplayInfo, System};
use crate::m32x::sh7604::Sh7604;
use crate::common::cpu::m68000::M68000;
use bus::{Bus, Ext, BACKUP_SIZE, BIOS_SIZE, SRAM_SIZE};
use serde::{Deserialize, Serialize};
use smpc::Action;

/// The debug log (option "saturn_debug"): SMPC / CD commands and the CPUs every 5 seconds
static DEBUG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn debug() -> bool { DEBUG.load(std::sync::atomic::Ordering::Relaxed) }

const STATE_VERSION: u32 = 4;
/// The 68000's clock (the SCSP's 22.5792 MHz / 2)
const SND_HZ: u64 = 11_289_600;
/// The CPUs take turns in slices of this many clocks
const SLICE: u32 = 64;
const HZ_320: u64 = 26_874_100;
const HZ_352: u64 = 28_636_360;
const SAMPLE_RATE: u32 = 44_100;

/// The slave's interrupt lines: (level, vector)
const SLAVE_VBLANK: (u8, u8) = (6, 0x43);
const SLAVE_HBLANK: (u8, u8) = (2, 0x41);

/// Whether a 512 KB file is a Saturn BIOS: by name, or by its vectors (big-endian SH-2:
/// the reset PC inside the BIOS, the stack in work RAM high)
pub fn is_bios(data: &[u8], file: &str) -> bool {
    if data.len() != BIOS_SIZE { return false; }
    let f = file.to_ascii_lowercase();
    if f.contains("saturn") || f.starts_with("sega_1") || f.starts_with("sega1") || f.starts_with("mpr-1793") || f.starts_with("mpr-1694") {
        return true;
    }
    let be = |o: usize| u32::from_be_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]]);
    let (pc, sp) = (be(0), be(4));
    (pc as usize) < BIOS_SIZE && pc >= 0x100 && (0x0600_0000..=0x0610_0000).contains(&sp)
}

/// A Saturn disc: its first data sector starts "SEGA SEGASATURN "
pub fn is_saturn_disc(d: &mut Disc) -> bool {
    !d.gd && d.tracks.first().is_some_and(|t| !t.is_audio())
        && d.read_data(0).is_some_and(|s| &s[..16] == b"SEGA SEGASATURN ")
}

/// Open the disc in the frontend's "MCDP" + path blob; an error if it isn't a Saturn disc
pub fn open_disc(blob: &[u8]) -> Result<Disc, String> {
    let path = crate::segacd::disc_path(blob).ok_or("not a disc image path")?;
    let mut d = Disc::open(&path)?;
    if !is_saturn_disc(&mut d) {
        let head = d.read_data(0).map(|s| String::from_utf8_lossy(&s[..16]).into_owned());
        return Err(format!("{}: sector 0 starts {head:?}, gd {}, {} track(s)", path.display(), d.gd, d.tracks.len()));
    }
    Ok(d)
}

/// The SMPC area code for a disc's area letters (J, T, U, B, K, A, E, L)
fn area_code(c: u8) -> u8 {
    match c {
        b'J' => 1, b'T' => 2, b'U' => 4, b'B' => 5, b'K' => 6, b'A' => 0xA, b'E' => 0xC, b'L' => 0xD,
        _ => 4,
    }
}

#[derive(Serialize, Deserialize)]
struct State {
    msh: Sh7604,
    ssh: Sh7604,
    bus: Bus,
    debt: [i64; 2],
    slave_irq: u8,
    nmi: bool,
    frame: u64,
    m68k: M68000,
    snd_frac: u64,
    smp_frac: u64,
    snd_debt: i64,
}

pub struct SaturnSystem {
    pub msh: Sh7604,
    pub ssh: Sh7604,
    pub bus: Bus,
    debt: [i64; 2],
    /// the slave's pending interrupts: bit 0 VBlank-IN, bit 1 HBlank-IN
    slave_irq: u8,
    /// an NMI for the master waits for it to leave a delay slot
    nmi: bool,
    /// the disc, shared with the CD block
    pub disc: Option<std::sync::Arc<std::sync::Mutex<Disc>>>,
    region: u8,
    frame: Vec<u8>,
    frame_w: usize,
    frame_h: usize,
    /// the sound CPU and the fractions of its clock / of a sample still owed
    pub m68k: M68000,
    snd_frac: u64,
    smp_frac: u64,
    snd_debt: i64,
    /// the debug log: the last master instruction that read the CD block's HIRQ
    hirq_pc: u32,
    /// the debug log: each CPU's last 64 pcs (a ring), and whether a crash was reported
    tracing: bool,
    /// (from, to) of each CPU's last 64 jumps, a ring
    pcs: [[(u32, u32); 64]; 2],
    pc_at: [usize; 2],
    last_pc: [u32; 2],
    crashed: [bool; 2],
    rebooted: bool,
    /// the debug log: the game set the clock (its boot), and each CPU's first visit to the BIOS ROM after that was logged
    game_on: bool,
    in_rom: [bool; 2],
    /// each CPU's DMAC channels seen running, so each start is logged once
    dmac_on: [[bool; 2]; 2],
    /// idle-loop detection, per CPU: the last short backward branch's target and the state there
    idle: [(u32, [u32; 24]); 2],
    /// clocks skipped as idle (debug report)
    idle_clocks: [u64; 2],
    last_step_pc: [u32; 2],
    /// debug: wall time in master, slave, sound, devices, VDP2 lines, VDP1 (seconds since the last report)
    wall: [f64; 6],
    current_frame: u64,
    save_filename: String,
    save_path: String,
    ext: u32,
}

impl SaturnSystem {
    pub fn from_rom(rom: &[u8], filename: &str, bios: &[u8]) -> Result<Self, String> {
        let name = filename.rsplit(['/', '\\']).next().unwrap_or(filename);
        let mut disc = if rom.starts_with(crate::segacd::PATH_MAGIC) { Some(open_disc(rom)?) } else { None };
        let bios = if bios.len() == BIOS_SIZE {
            bios.to_vec()
        } else if is_bios(rom, name) {
            rom.to_vec()
        } else {
            return Err("Saturn needs its BIOS: put a 512 KB Saturn BIOS (e.g. sega_101.bin or mpr-17933.bin) in the BIOS folder".into());
        };
        // the region: the disc's first area letter, else North America
        let region = disc.as_mut().and_then(|d| d.read_data(0)).map_or(4, |s| area_code(s[0x40]));
        if let Some(d) = disc.as_mut() {
            let h = d.read_data(0).unwrap_or([0; 2048]);
            let title = String::from_utf8_lossy(&h[0x60..0x90]).trim().to_string();
            println!("Saturn: disc \"{title}\", area {}", String::from_utf8_lossy(&h[0x40..0x4A]).trim());
        }
        let pal = matches!(region, 0xA | 0xC | 0xD);
        let mut s = Self {
            msh: Sh7604::new(), ssh: { let mut s = Sh7604::new(); s.p.slave = true; s }, bus: Bus::new(bios, region, pal),
            debt: [0; 2], slave_irq: 0, nmi: false, disc: disc.map(|d| std::sync::Arc::new(std::sync::Mutex::new(d))), region,
            frame: vec![0; 320 * 224 * 4], frame_w: 320, frame_h: 224, m68k: M68000::new(), snd_frac: 0, smp_frac: 0, snd_debt: 0, hirq_pc: 0, tracing: false, pcs: [[(0, 0); 64]; 2], pc_at: [0; 2], last_pc: [0; 2], crashed: [false; 2], rebooted: false, game_on: false, in_rom: [false; 2], dmac_on: [[false; 2]; 2], idle: [(u32::MAX, [0; 24]); 2], idle_clocks: [0; 2], last_step_pc: [0; 2], wall: [0.0; 6], current_frame: 0,
            save_filename: "saturn_internal".into(), save_path: "user://GD_EMU/Saturn/Save".into(), ext: 0,
        };
        s.frame.chunks_exact_mut(4).for_each(|p| p[3] = 0xFF);
        s.bus.cd = cdblock::CdBlock::new(s.disc.clone());
        s.power();
        Ok(s)
    }

    fn power(&mut self) {
        self.msh.reset(false);
        self.ssh.reset(false);
        self.debt = [0; 2];
        self.slave_irq = 0;
        self.nmi = false;
    }

    fn hz(&self) -> u64 { if self.bus.smpc.dot352 { HZ_352 } else { HZ_320 } }

    fn line_clocks(&self) -> u32 { if self.bus.smpc.dot352 { 1820 } else { 1708 } }

    fn run_cpu(&mut self, k: usize, n: u32) {
        self.debt[k] += n as i64;
        while self.debt[k] > 0 {
            if self.tracing {
                let pc = if k == 0 { self.msh.cpu.instruction_pc() } else { self.ssh.cpu.instruction_pc() };
                self.trace(k, pc);
            }
            let mut ext = Ext { bus: &mut self.bus, waits: 0 };
            let c = if k == 0 {
                if self.nmi && self.msh.nmi(&mut ext) { self.nmi = false; }
                if let Some((level, vector, bit)) = ext.bus.scu.pending() {
                    if self.msh.external_irq(&mut ext, level, vector) { ext.bus.scu.ist &= !bit; }
                }
                let (pc, n) = (self.msh.cpu.instruction_pc(), ext.bus.cd.hirq_reads);
                let c = self.msh.step(&mut ext);
                if ext.bus.cd.hirq_reads != n { self.hirq_pc = pc; }
                c
            } else {
                if self.slave_irq != 0 {
                    let (bit, (level, vector)) = if self.slave_irq & 1 != 0 { (1, SLAVE_VBLANK) } else { (2, SLAVE_HBLANK) };
                    if self.ssh.external_irq(&mut ext, level, vector) { self.slave_irq &= !bit; }
                }
                self.ssh.step(&mut ext)
            };
            let c = c + ext.waits;
            self.debt[k] -= c as i64;
            self.idle_check(k);
            if self.bus.capture != [false; 2] {
                if std::mem::take(&mut self.bus.capture[0]) { self.msh.input_capture(); }
                if std::mem::take(&mut self.bus.capture[1]) { self.ssh.input_capture(); }
            }
        }
    }

    /// A wait loop that made no progress: a short backward branch back to the same place with
    /// the same registers and nothing written since. The next pass would be the same until
    /// something outside changes (that only happens between slices), so the rest of this
    /// slice is skipped; the on-chip timers still count it.
    fn idle_check(&mut self, k: usize) {
        let c = if k == 0 { &self.msh } else { &self.ssh };
        let pc = c.cpu.instruction_pc();
        let (last_pc, _) = self.idle[k];
        // only at branch targets: the pc went backwards a little
        let prev = self.last_step_pc[k];
        self.last_step_pc[k] = pc;
        if !(pc < prev && prev - pc <= 256) { return; }
        let cpu = &c.cpu;
        let mut snap = [0u32; 24];
        snap[..16].copy_from_slice(&cpu.r);
        snap[16] = cpu.sr.get();
        snap[17] = cpu.gbr;
        snap[18] = cpu.mach;
        snap[19] = cpu.macl;
        snap[20] = cpu.pr;
        // the writes of this pass (only RAM / on-chip, and the same ones as last pass)
        snap[21] = c.p.wsig as u32;
        snap[22] = (c.p.wsig >> 32) as u32;
        snap[23] = c.p.wdev as u32;
        let dev = c.p.wdev;
        let p = if k == 0 { &mut self.msh.p } else { &mut self.ssh.p };
        p.wsig = 0;
        p.wdev = false;
        let c = if k == 0 { &self.msh } else { &self.ssh };
        if !dev && last_pc == pc && self.idle[k].1 == snap && self.debt[k] > 0 && !c.dma_busy() {
            let left = self.debt[k] as u32;
            if k == 0 { self.msh.burn(left) } else { self.ssh.burn(left) }
            self.idle_clocks[k] += left as u64;
            self.debt[k] = 0;
        }
        self.idle[k] = (pc, snap);
    }

    /// Run both CPUs and the devices for `cycles` clocks
    fn run(&mut self, cycles: u32) {
        let mut left = cycles;
        while left > 0 {
            let n = left.min(SLICE);
            let hz = self.hz();
            if self.tracing {
                let t = std::time::Instant::now();
                self.run_cpu(0, n);
                let t1 = std::time::Instant::now();
                if self.bus.smpc.slave_on { self.run_cpu(1, n); } else { self.debt[1] = 0; }
                let t2 = std::time::Instant::now();
                self.run_sound(n, hz);
                let t3 = std::time::Instant::now();
                self.bus.tick(n, hz);
                let t4 = std::time::Instant::now();
                self.wall[0] += (t1 - t).as_secs_f64();
                self.wall[1] += (t2 - t1).as_secs_f64();
                self.wall[2] += (t3 - t2).as_secs_f64();
                self.wall[3] += (t4 - t3).as_secs_f64();
            } else {
                self.run_cpu(0, n);
                if self.bus.smpc.slave_on { self.run_cpu(1, n); } else { self.debt[1] = 0; }
                self.run_sound(n, hz);
                self.bus.tick(n, hz);
            }
            self.bus.vdp2.dot += n / 4;
            if !self.bus.smpc.actions.is_empty() { self.smpc_actions(); }
            left -= n;
        }
    }

    /// The debug log: remember the pc; the first time a CPU runs outside the BIOS / RAM,
    /// log its last 64 pcs and registers
    fn trace(&mut self, k: usize, pc: u32) {
        let name = if k == 0 { "master" } else { "slave" };
        // SH-2 DMAC starts (SCU DMA is logged by the bus)
        for c in 0..2 {
            let d = if k == 0 { &self.msh.p.dmac } else { &self.ssh.p.dmac };
            let h = d.chcr[c];
            let on = d.dme && h.de && !h.te;
            if on && !self.dmac_on[k][c] {
                crate::dlog!("Saturn: {name} DMAC{c} start at pc {pc:08X} frame {}: {:08X} -> {:08X} count {:06X} ts {} sm {} dm {} ar {} drcr {}",
                    self.current_frame, d.sar[c], d.dar[c], d.tcr[c], h.ts, h.sm, h.dm, h.ar, d.drcr[c]);
            }
            self.dmac_on[k][c] = on;
        }
        let last = self.last_pc[k];
        self.last_pc[k] = pc;
        if pc != last.wrapping_add(2) && pc != last {
            let i = self.pc_at[k];
            self.pcs[k][i & 63] = (last, pc);
            self.pc_at[k] = i + 1;
        }
        let a = pc & 0x1FFF_FFFF;
        // FFFFFFFC: just reset, the vectors aren't fetched yet
        let ok = pc == 0xFFFF_FFFC || a < 0x8_0000 || (0x20_0000..0x30_0000).contains(&a) || (0x600_0000..0x800_0000).contains(&a) || pc >> 29 == 6;
        let b = &self.bus.bios;
        let reset = if b.len() >= 4 { u32::from_be_bytes([b[0], b[1], b[2], b[3]]) } else { 0 };
        let reboot = k == 0 && !self.rebooted && self.current_frame > 10 && pc == reset;
        if reboot {
            self.rebooted = true;
            crate::dlog!("Saturn: master CPU is back at the BIOS reset entry {pc:08X} at frame {}", self.current_frame);
            self.dump_jumps(0);
            self.dump_jumps(1);
            return;
        }
        // CPU exceptions (illegal instruction / slot, TRAPA): a crash starts with one of these
        let cpu = if k == 0 { &mut self.msh.cpu } else { &mut self.ssh.cpu };
        if let Some((vec, ret)) = cpu.last_exception.take() {
            if !self.crashed[k] && vec != 32 && !(32..=63).contains(&vec) {
                self.crashed[k] = true;
                crate::dlog!("Saturn: {name} CPU exception {vec} at pc {ret:08X} (going to {pc:08X}) frame {}", self.current_frame);
                let code: Vec<String> = (0..24u32).map(|n| format!("{:04X}", self.bus.read16((ret & !1).wrapping_sub(16) + n * 2))).collect();
                crate::dlog!("Saturn:   code at {:08X}: {}", (ret & !1).wrapping_sub(16), code.join(" "));
                self.dump_jumps(k);
                return;
            }
        }
        // the BIOS ROM after the game booted: a BIOS call, or a jump back to the boot code
        if self.game_on && a < 0x8_0000 && !self.in_rom[k] {
            self.in_rom[k] = true;
            crate::dlog!("Saturn: {name} CPU entered the BIOS ROM at {pc:08X} frame {} (logged once)", self.current_frame);
            self.dump_jumps(k);
        }
        let ok = ok && pc & 1 == 0;
        if ok || self.crashed[k] { return; }
        self.crashed[k] = true;
        crate::dlog!("Saturn: {name} CPU ran off to {pc:08X} at frame {}", self.current_frame);
        self.dump_jumps(k);
    }

    /// the debug log: a CPU's last 64 jumps and its registers
    fn dump_jumps(&mut self, k: usize) {
        let i = self.pc_at[k];
        let ring: Vec<String> = (0..64).map(|n| { let (f, t) = self.pcs[k][(i + n) & 63]; format!("{f:08X}>{t:08X}") }).collect();
        let c = if k == 0 { &self.msh.cpu } else { &self.ssh.cpu };
        crate::dlog!("Saturn:   {} jumps (oldest first): {}", if k == 0 { "master" } else { "slave" }, ring.join(" "));
        crate::dlog!("Saturn:   r0-15 {:08X?} pr {:08X} sr {:03X} vbr {:08X} gbr {:08X}", c.r, c.pr, c.sr.get(), c.vbr, c.gbr);
        for l in std::mem::take(&mut self.bus.dma_log) { crate::dlog!("Saturn:   DMA {l}"); }
    }

    /// The 68000 (while SNDON) and the SCSP's samples for `n` SH-2 clocks
    fn run_sound(&mut self, n: u32, hz: u64) {
        if self.bus.smpc.sound_on {
            self.snd_frac += n as u64 * SND_HZ;
            self.snd_debt += (self.snd_frac / hz) as i64;
            self.snd_frac %= hz;
            while self.snd_debt > 0 {
                self.m68k.ipl = self.bus.scsp.ipl();
                let c = self.m68k.step(&mut scsp::SoundBus { scsp: &mut self.bus.scsp });
                self.snd_debt -= c.max(4) as i64;
            }
        }
        self.smp_frac += n as u64 * SAMPLE_RATE as u64;
        while self.smp_frac >= hz {
            self.smp_frac -= hz;
            // CD-DA into the SCSP's EXTS inputs
            let (l, r) = self.bus.cd.audio.pop_front().unwrap_or((0, 0));
            self.bus.scsp.set_exts(l, r);
            self.bus.scsp.sample();
        }
    }

    fn smpc_actions(&mut self) {
        for a in std::mem::take(&mut self.bus.smpc.actions) {
            if debug() { crate::dlog!("Saturn: SMPC {a:?}"); }
            match a {
                Action::Slave(on) => {
                    if on { self.ssh.reset(false); self.slave_irq = 0; self.debt[1] = 0; }
                }
                Action::Sound(on) => {
                    // SNDON: the 68000 starts from the vectors in sound RAM
                    if on {
                        self.m68k = M68000::new();
                        self.m68k.reset(&mut scsp::SoundBus { scsp: &mut self.bus.scsp });
                        self.snd_debt = 0;
                    }
                }
                Action::SysRes => {
                    let b = &mut self.bus;
                    b.scu = scu::Scu::new();
                    b.vdp1 = vdp1::Vdp1::new();
                    let pal = b.vdp2.pal;
                    b.vdp2 = vdp2::Vdp2::new(pal);
                    b.scsp = scsp::Scsp::new();
                    b.cd = cdblock::CdBlock::new(self.disc.clone());
                    b.smpc.slave_on = false;
                    b.smpc.sound_on = false;
                    self.msh.reset(false);
                    self.nmi = false;
                }
                Action::Clock(_) => {
                    self.game_on = true;
                    // the new clock takes effect from the next slice; VDP2 is reset
                    let pal = self.bus.vdp2.pal;
                    let (vram, cram) = (std::mem::take(&mut self.bus.vdp2.vram), std::mem::take(&mut self.bus.vdp2.cram));
                    self.bus.vdp2 = vdp2::Vdp2 { vram, cram, ..vdp2::Vdp2::new(pal) };
                }
                Action::MasterNmi => self.nmi = true,
                Action::Irq => self.bus.scu.ist |= scu::IST_SMPC,
            }
        }
    }

    fn run_line(&mut self, line: u32) {
        let total = self.bus.vdp2.total_lines();
        let vres = self.bus.vdp2.field_lines().min(total - 1);
        let v = &mut self.bus.vdp2;
        v.line = line;
        v.dot = 0;
        v.hblank = false;
        if line == 0 {
            v.vblank = false;
            if v.interlaced() { v.odd = !v.odd; } else { v.odd = true; }
            self.bus.scu.vblank_out();
        }
        if line == vres {
            self.bus.vdp2.vblank = true;
            self.bus.scu.raise(scu::IST_VBLANK_IN, Some(scu::Event::VBlankIn));
            self.slave_irq |= 1;
            let t = std::time::Instant::now();
            self.bus.vdp1.frame_change();
            if self.tracing { self.wall[5] += t.elapsed().as_secs_f64(); }
        }
        if line == 0 { self.start_frame(); }
        if line < vres {
            let t = std::time::Instant::now();
            self.draw_line(line);
            if self.tracing { self.wall[4] += t.elapsed().as_secs_f64(); }
        }
        let clocks = self.line_clocks();
        let (vis, dots) = if self.bus.smpc.dot352 { (352, 455) } else { (320, 427) };
        let active = clocks * vis / dots;
        self.run(active);
        self.bus.vdp2.hblank = true;
        self.bus.scu.hblank_in();
        self.slave_irq |= 2;
        self.run(clocks - active);
    }

    /// The debug log: both CPUs' recent data reads (what they're polling), the code at the
    /// master's pr, the slave's registers and code, and both FRTs' capture state
    fn report_more(&mut self) {
        for k in 0..2 {
            let name = if k == 0 { "master" } else { "slave" };
            let (pc, pr, r, frt, w) = {
                let c = if k == 0 { &mut self.msh } else { &mut self.ssh };
                let f = &c.p.frt;
                let frt = format!("FRT icf {} icie {} ficr {:04X} frc {:04X} ip {:X}", f.icf as u8, f.icie as u8, f.ficr, f.frc, c.p.frtip);
                let w: Vec<String> = c.p.watch.as_mut().map(|w| w.drain(..).map(|(a, v)| format!("{a:08X}={v:X}")).collect()).unwrap_or_default();
                (c.cpu.instruction_pc(), c.cpu.pr, c.cpu.r, frt, w)
            };
            crate::dlog!("Saturn:   {name} reads: {} | {frt}", w.join(" "));
            if k == 1 {
                crate::dlog!("Saturn:   slave r0-15 {:08X?} pr {:08X}", r, pr);
                let code: Vec<String> = (0..24u32).map(|n| format!("{:04X}", self.bus.read16((pc & !1).wrapping_sub(16) + n * 2))).collect();
                crate::dlog!("Saturn:   slave code at {:08X}: {}", (pc & !1).wrapping_sub(16), code.join(" "));
            }
            let code: Vec<String> = (0..24u32).map(|n| format!("{:04X}", self.bus.read16((pr & !1).wrapping_sub(24) + n * 2))).collect();
            crate::dlog!("Saturn:   {name} code before pr {:08X}: {}", (pr & !1).wrapping_sub(24), code.join(" "));
        }
    }

    /// The debug log: the master's registers, the code around its pc, the most-read addresses
    fn report_cpu(&mut self) {
        let hp = self.hirq_pc & !1;
        if hp != 0 {
            let code: Vec<String> = (0..48u32).map(|k| format!("{:04X}", self.bus.read16(hp.wrapping_sub(48) + k * 2))).collect();
            crate::dlog!("Saturn:   HIRQ read at {:08X} (hirq {:04X} mask {:04X}); code from {:08X}: {}", self.hirq_pc, self.bus.cd.hirq, self.bus.cd.hirq_mask, hp.wrapping_sub(48), code.join(" "));
        }
        for a in std::mem::take(&mut self.bus.cd.answers) { crate::dlog!("Saturn:   CD {a}"); }
        for l in std::mem::take(&mut self.bus.dma_log) { crate::dlog!("Saturn:   DMA {l}"); }
        let c = &self.msh.cpu;
        let pc = c.instruction_pc();
        crate::dlog!("Saturn:   master r0-7 {:08X?}", &c.r[..8]);
        crate::dlog!("Saturn:   master r8-15 {:08X?} gbr {:08X} vbr {:08X} mach {:08X} macl {:08X}", &c.r[8..], c.gbr, c.vbr, c.mach, c.macl);
        let code: Vec<String> = (0..24u32).map(|k| format!("{:04X}", self.bus.read16((pc & !1).wrapping_sub(16) + k * 2))).collect();
        crate::dlog!("Saturn:   code at {:08X}: {}", (pc & !1).wrapping_sub(16), code.join(" "));
        let s = &self.ssh.cpu;
        crate::dlog!("Saturn:   slave pc {:08X} r0-3 {:08X?}", s.instruction_pc(), &s.r[..4]);
        self.report_more();
        if let Some(p) = self.bus.prof.take() {
            let mut v: Vec<(u32, u32)> = p.into_iter().collect();
            v.sort_by(|a, b| b.1.cmp(&a.1));
            let top: Vec<String> = v.iter().take(12).map(|(a, n)| format!("{a:08X}x{n}")).collect();
            crate::dlog!("Saturn:   most read (uncached, one frame): {}", top.join(" "));
        }
        let b = &mut self.bus;
        let mut ram = |a: u32| (0..8u32).map(|k| format!("{:04X}", b.read16(a + k * 2))).collect::<Vec<_>>().join(" ");
        let (r0, r1) = (ram(0x05A0_0000), ram(0x05B0_0400));
        crate::dlog!("Saturn:   SCSP RAM 0: {r0} | regs 400: {r1} | key-ons {} peak {:.3}", b.scsp.keyons, b.scsp.peak);
        b.scsp.keyons = 0;
        b.scsp.peak = 0.0;
        let m = &self.m68k;
        crate::dlog!("Saturn:   68000 pc {:06X} sr {:04X} ipl {} d0-3 {:08X?} a7 {:08X}", m.instruction_pc(), m.sr, m.ipl, &m.r[..4], m.r[15]);
    }

    /// The picture's size for this frame (TVMD may have changed)
    fn start_frame(&mut self) {
        let v = &self.bus.vdp2;
        let w = v.width() as usize;
        let h = (v.field_lines() * if v.interlaced() { 2 } else { 1 }) as usize;
        if (w, h) != (self.frame_w, self.frame_h) {
            self.frame = vec![0; w * h * 4];
            self.frame.chunks_exact_mut(4).for_each(|p| p[3] = 0xFF);
            self.frame_w = w;
            self.frame_h = h;
        }
    }

    /// Draw display line `line` of this field (interlace: every other row)
    fn draw_line(&mut self, line: u32) {
        let v = &self.bus.vdp2;
        let row = if v.interlaced() { line * 2 + !v.odd as u32 } else { line } as usize;
        if row >= self.frame_h { return; }
        let w = self.frame_w;
        v.render_line(row as u32, &self.bus.vdp1, &mut self.frame[row * w * 4..(row + 1) * w * 4]);
    }

    fn set_pads(&mut self, input: u32) {
        let pads = input::pads(input, self.ext);
        for k in 0..2 {
            let p = &pads[k];
            let r2 = k == 0 && self.ext & input::EXT_P1_R2 != 0;
            // R L D U Start A C B | R X Y Z L 1 1 1 (active low)
            let bits = [
                (p.right, 15), (p.left, 14), (p.down, 13), (p.up, 12), (p.start, 11), (p.b, 10), (p.r, 9), (p.a, 8),
                (r2 || (k == 1 && p.coin), 7), (p.x, 6), (p.y, 5), (p.l, 4), (p.z, 3),
            ];
            let mut v = 0u16;
            for (on, b) in bits { if on { v |= 1 << b; } }
            self.bus.smpc.pads[k] = !v;
        }
    }
}

impl System for SaturnSystem {
    fn run_frame(&mut self, input: u32) {
        self.set_pads(input);
        let total = self.bus.vdp2.total_lines();
        // the debug log profiles the bus for the frame before each report
        if debug() && self.current_frame % 300 == 298 { self.bus.prof = Some(Default::default()); }
        for line in 0..total {
            self.run_line(line);
        }
        if debug() && self.current_frame % 300 == 299 {
            self.report_cpu();
            crate::dlog!("Saturn:   idle (skipped wait loops) in the last 300 frames: master {}M clocks, slave {}M clocks",
                self.idle_clocks[0] / 1_000_000, self.idle_clocks[1] / 1_000_000);
            self.idle_clocks = [0; 2];
            let w = self.wall;
            crate::dlog!("Saturn:   time for the last 300 frames (5 s at full speed): master {:.2}s slave {:.2}s sound {:.2}s devices {:.2}s VDP2 {:.2}s VDP1 {:.2}s = {:.2}s",
                w[0], w[1], w[2], w[3], w[4], w[5], w.iter().sum::<f64>());
            self.wall = [0.0; 6];
            let (m, s, b) = (&self.msh.cpu, &self.ssh.cpu, &self.bus);
            crate::dlog!("Saturn: frame {} master pc {:08X} pr {:08X} sr {:03X} | slave {} pc {:08X} | IMS {:08X} IST {:08X} | TVMD {:04X} VDP1 draws {} | SMPC {:02X?} | CD hirq {:04X} cmds {}",
                self.current_frame + 1, m.instruction_pc(), m.pr, m.sr.get(), if b.smpc.slave_on { "on" } else { "off" }, s.instruction_pc(),
                b.scu.ims, b.scu.ist, b.vdp2.tvmd(), b.vdp1.draws, b.smpc.log, b.cd.hirq, b.cd.log.len());
            let r = |o: usize| b.vdp2.regs[o / 2];
            crate::dlog!("Saturn:   VDP2 BGON {:04X} CHCTL {:04X} {:04X} PNCN {:04X} {:04X} {:04X} {:04X} RAMCTL {:04X} SPCTL {:04X} PRIN {:04X} {:04X} {:04X} PRIS {:04X} CCCTL {:04X} CLOFEN {:04X} | VDP1 TVMR {:X} FBCR {:X} PTMR {:X}",
                r(0x20), r(0x28), r(0x2A), r(0x30), r(0x32), r(0x34), r(0x36), r(0x0E), r(0xE0), r(0xF8), r(0xFA), r(0xFC), r(0xF0), r(0xEC), r(0x110),
                b.vdp1.tvmr, b.vdp1.fbcr, b.vdp1.ptmr);
            let rr = |a: usize, n: usize| (0..n).map(|k| format!("{:04X}", r(a + k * 2))).collect::<Vec<_>>().join(" ");
            crate::dlog!("Saturn:   VDP2 NBG0 scroll/zoom {} | NBG1 {} | ZMCTL {:04X} SCRCTL {:04X} VCSTA {} LSTA0 {} LSTA1 {} | MPOFN {:04X} BMPNA {:04X} MPAB0 {:04X} | DSP {}",
                rr(0x70, 8), rr(0x80, 8), r(0x98), r(0x9A), rr(0x9C, 2), rr(0xA0, 2), rr(0xA4, 2), r(0x3C), r(0x2C), r(0x40),
                if b.scu.dsp.running() { "running" } else { "stopped" });
            crate::dlog!("Saturn:   VDP2 SFSEL {:04X} SFCODE {:04X} SFCCMD {:04X} SFPRMD {:04X} CCRSA-D {} CCRNA/B {} LNCLEN {:04X}", r(0x24), r(0x26), r(0xEE), r(0xEA), rr(0xF0, 4), rr(0x108, 2), r(0xE8));
            if r(0x9A) & 0x0F0F != 0 {
                let tbl = |a: u32, n: u32| (0..n).map(|k| format!("{:04X}{:04X}", b.vdp2.vram_word(a + k * 4), b.vdp2.vram_word(a + k * 4 + 2))).collect::<Vec<_>>().join(" ");
                let ta = |o: usize| ((r(o) as u32 & 7) << 16 | (r(o + 2) as u32 & 0xFFFE)) << 1;
                crate::dlog!("Saturn:   line scroll table 0 @{:05X}: {}", ta(0xA0), tbl(ta(0xA0), 12));
                crate::dlog!("Saturn:   vertical cell table @{:05X}: {}", ta(0x9C), tbl(ta(0x9C), 44));
            }
            if r(0x20) & 0x30 != 0 { crate::dlog!("Saturn:   (rotation screens are on: not drawn yet)"); }
            for c in &b.cd.log { crate::dlog!("Saturn:   CD {:04X} {:04X} {:04X} {:04X}", c[0], c[1], c[2], c[3]); }
            self.bus.cd.log.clear();
            self.bus.smpc.log.clear();
        }
        self.current_frame += 1;
    }

    fn framebuffer(&self) -> &[u8] { &self.frame }

    fn display_info(&self) -> DisplayInfo {
        DisplayInfo {
            buffer_width: self.frame_w as i32,
            buffer_height: self.frame_h as i32,
            visible_x: 0,
            visible_y: 0,
            visible_width: self.frame_w as i32,
            visible_height: self.frame_h as i32,
            target_aspect_ratio: 4.0 / 3.0,
            ..Default::default()
        }
    }

    fn drain_audio(&mut self) -> Vec<f32> { std::mem::take(&mut self.bus.scsp.out) }

    fn sample_rate(&self) -> u32 { SAMPLE_RATE }

    fn set_input(&mut self, input: u32) { self.set_pads(input); }

    fn set_extra_input(&mut self, ext: u32) { self.ext = ext; }

    fn save_state(&self) -> Vec<u8> {
        let s = State {
            msh: self.msh.clone(), ssh: self.ssh.clone(), bus: self.bus.clone(), debt: self.debt,
            slave_irq: self.slave_irq, nmi: self.nmi, frame: self.current_frame,
            m68k: self.m68k.clone(), snd_frac: self.snd_frac, smp_frac: self.smp_frac, snd_debt: self.snd_debt,
        };
        crate::arcade::state::encode(&self.save_filename, STATE_VERSION, &s)
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let mut s: State = crate::arcade::state::decode(&self.save_filename, STATE_VERSION, data)?;
        if s.bus.hwram.len() != bus::RAM_SIZE || s.bus.backup.len() != SRAM_SIZE {
            return Err("save state doesn't fit".into());
        }
        s.bus.bios = std::mem::take(&mut self.bus.bios);
        s.bus.cd.disc = self.disc.clone();
        s.bus.smpc.pads = self.bus.smpc.pads;
        s.bus.smpc.connected = self.bus.smpc.connected;
        self.msh = s.msh;
        self.ssh = s.ssh;
        self.ssh.p.slave = true;
        self.bus = s.bus;
        self.debt = s.debt;
        self.slave_irq = s.slave_irq;
        self.nmi = s.nmi;
        self.current_frame = s.frame;
        self.m68k = s.m68k;
        self.snd_frac = s.snd_frac;
        self.smp_frac = s.smp_frac;
        self.snd_debt = s.snd_debt;
        Ok(())
    }

    /// The backup RAM file: 32 KB, optionally followed by "SMPC" and the 4 SMEM bytes
    fn load_sram(&mut self, data: &[u8]) {
        if data.len() < BACKUP_SIZE { return; }
        self.bus.backup[..BACKUP_SIZE].copy_from_slice(&data[..BACKUP_SIZE]);
        if data.len() >= SRAM_SIZE && &data[BACKUP_SIZE..BACKUP_SIZE + 4] == b"SMPC" {
            self.bus.backup[BACKUP_SIZE..].copy_from_slice(&data[BACKUP_SIZE..SRAM_SIZE]);
            self.bus.smpc.smem[..4].copy_from_slice(&data[BACKUP_SIZE + 4..SRAM_SIZE]);
            self.bus.smpc.smem[4] = 0xFF;
        }
    }

    fn get_sram(&self) -> Option<&[u8]> { Some(&self.bus.backup) }

    fn reset(&mut self) {
        let bios = std::mem::take(&mut self.bus.bios);
        let backup = std::mem::take(&mut self.bus.backup);
        let smem = self.bus.smpc.smem;
        let pal = self.bus.vdp2.pal;
        self.bus = Bus::new(bios, self.region, pal);
        self.bus.backup = backup;
        self.bus.smpc.smem = smem;
        self.bus.cd = cdblock::CdBlock::new(self.disc.clone());
        self.power();
    }

    fn has_battery(&self) -> bool { true }

    fn set_option(&mut self, key: &str, value: i64) {
        if key == "debug_report" {
            DEBUG.store(value != 0, std::sync::atomic::Ordering::Relaxed);
            self.tracing = value != 0;
            let w = if value != 0 { Some(Default::default()) } else { None };
            self.msh.p.watch = w.clone();
            self.ssh.p.watch = w;
        }
    }

    fn save_battery_path(&self) -> &str { &self.save_path }
    fn save_filename(&self) -> &str { &self.save_filename }
    fn save_state_path(&self) -> &str { "user://GD_EMU/Saturn/State" }
    fn power_on(&mut self) {}
    fn power_off(&mut self) {}
    fn is_sram_dirty(&self) -> bool { self.bus.backup_dirty }
    fn clear_sram_dirty(&mut self) { self.bus.backup_dirty = false; }
    fn get_current_frame(&self) -> i64 { self.current_frame as i64 }

    fn frame_time_usec(&self) -> i64 {
        self.bus.vdp2.total_lines() as i64 * self.line_clocks() as i64 * 1_000_000 / self.hz() as i64
    }
}
