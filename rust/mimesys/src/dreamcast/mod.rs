//! Sega Dreamcast (stage 2 of `claude/dc_plan.md`): the SH-4 runs the real BIOS
//! (dc_boot.bin + dc_flash.bin) on the memory map, on-chip devices, Holly, Maple pads,
//! the PowerVR2's sync / framebuffer output, its tile accelerator and software renderer
//! (stage 3), and the GD-ROM drive reading GDI / CHD / CUE images (stage 4). The rest of
//! the 3D features and sound come in later stages.

pub mod aica;
pub mod aica_sound;
pub mod bus;
pub mod flash;
pub mod gdrom;
pub mod holly;
pub mod maple;
pub mod onchip;
pub mod pvr;
pub mod render;
pub mod ta;
pub mod yuv;
pub mod texture;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod render_tests;
#[cfg(test)]
mod aica_tests;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::common::cpu::sh4::Sh4;
use crate::common::input as pad;
use crate::common::system::{DisplayInfo, System};
use bus::{Dc, BIOS_SIZE};
use maple::DcPad;
use pvr::{OUT_H, OUT_W};

const STATE_VERSION: u32 = 1;
/// the longest the CPU runs before the devices catch up
const SLICE: u64 = 2_000;

/// The BIOS blob from the frontend: "DCBB", then entries of a 4-byte tag, a u32
/// little-endian length and the data: BOOT = dc_boot.bin, FLSH = dc_flash.bin,
/// CABL = 1 byte video cable (0 VGA, 2 RGB, 3 composite).
/// BCST = 1 byte broadcast standard override (0 NTSC, 1 PAL, 2 PAL-M, 3 PAL-N; absent = as the
/// flash says), written over the flash's factory bytes 1A004 / 1A0A4 in memory.
pub fn parse_bios_blob(blob: &[u8]) -> HashMap<String, Vec<u8>> {
    let mut out = HashMap::new();
    if !blob.starts_with(b"DCBB") { return out; }
    let mut p = 4;
    while p + 8 <= blob.len() {
        let tag = String::from_utf8_lossy(&blob[p..p + 4]).to_string();
        let n = u32::from_le_bytes(blob[p + 4..p + 8].try_into().unwrap()) as usize;
        p += 8;
        if p + n > blob.len() { break; }
        out.insert(tag, blob[p..p + n].to_vec());
        p += n;
    }
    out
}

/// Host time spent, for the speed line.
struct Prof {
    since: std::time::Instant,
    cycles: u64,
    idle_cycles: u64,
}

impl Default for Prof {
    fn default() -> Self { Self { since: std::time::Instant::now(), cycles: 0, idle_cycles: 0 } }
}

#[derive(Serialize)]
struct StateRef<'a> { version: u32, cpu: &'a Sh4, dc: &'a Dc, frame: u64 }

#[derive(Deserialize)]
struct State { version: u32, cpu: Sh4, dc: Dc, frame: u64 }

pub struct DcSystem {
    pub cpu: Sh4,
    pub dc: Dc,
    frame: Vec<u8>,
    current_frame: u64,
    ext: u32,
    /// fractional CPU cycles carried between frames
    time_rem: u64,
    /// SPG_CONTROL last reported
    video_mode: u32,
    debug_report: bool,
    /// where the CPU was at each slice since the last report
    pc_hist: HashMap<u32, u32>,
    /// debug: the last jumps (from, to, kind, times)
    jumps: std::collections::VecDeque<(u32, u32, &'static str, u32)>,
    pub disc_path: Option<String>,
    /// SH-4 cycles an instruction counts (option dc_cpu_ratio; 2 = the default)
    pub cpi: u64,
    /// skip polling loops to the next event (option dc_idle_skip)
    pub idle_skip: bool,
    /// polling loops found: start address → (the loop's code words, idle?)
    idle_loops: HashMap<u32, (Vec<u16>, bool)>,
    /// print a speed line every 300 frames (option speed_log, or with the debug report)
    speed_log: bool,
    prof: Prof,
    save_filename: String,
    save_battery_path: String,
    save_state_path: String,
}

impl DcSystem {
    /// `blob`: the BIOS itself (dc_boot.bin opened as a game: no disc) or a disc path
    /// ("MCDP" + path); `bios_blob`: see `parse_bios_blob`.
    pub fn from_rom(blob: &[u8], filename: &str, bios_blob: &[u8]) -> Result<Self, String> {
        let files = parse_bios_blob(bios_blob);
        let disc_path = blob.strip_prefix(crate::segacd::PATH_MAGIC.as_slice()).map(|p| String::from_utf8_lossy(p).to_string());
        let boot = match files.get("BOOT") {
            Some(b) => b.clone(),
            None if blob.len() == BIOS_SIZE => blob.to_vec(),
            None => return Err("Dreamcast: put dc_boot.bin (the 2 MB BIOS) in the BIOS folder".into()),
        };
        if boot.len() != BIOS_SIZE {
            return Err(format!("Dreamcast: dc_boot.bin is {} bytes; the BIOS is 2097152", boot.len()));
        }
        let flash = files.get("FLSH").cloned().unwrap_or_default();
        if flash.len() != flash::SIZE {
            crate::dlog!("Dreamcast: no dc_flash.bin (128 KB) in the BIOS folder: starting with blank settings");
        }
        let disc = match &disc_path {
            Some(p) => {
                let d = crate::common::disc::Disc::open(std::path::Path::new(p)).map_err(|e| format!("Dreamcast: {e}"))?;
                crate::dlog!("Dreamcast: disc {p}: {} ({} tracks: {})", if d.gd { "GD-ROM" } else { "CD" }, d.tracks.len(),
                             d.tracks.iter().map(|t| format!("{} {:?} at {}", t.number, t.kind, t.start)).collect::<Vec<_>>().join(", "));
                Some(d)
            }
            None => None,
        };
        let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let mut dc = Dc::new(boot, &flash, unix);
        if let Some(c) = files.get("CABL").and_then(|c| c.first()) { dc.chip.cable = (*c as u16) & 3; }
        if let Some(&b) = files.get("BCST").and_then(|c| c.first()) { dc.flash.set_broadcast(b); }
        if let Some(d) = disc { dc.gd.disc = gdrom::DiscSlot::new(d); }
        let name = disc_path.as_deref().and_then(|p| std::path::Path::new(p).file_stem()).map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| filename.rsplit_once('.').map_or(filename, |(s, _)| s).to_string());
        crate::dlog!("Dreamcast: BIOS loaded, video cable {}", ["VGA", "?", "RGB", "composite"][dc.chip.cable as usize]);
        Ok(Self {
            cpu: Sh4::new(), dc, frame: vec![0; OUT_W * OUT_H * 4], current_frame: 0, ext: 0, time_rem: 0, video_mode: u32::MAX,
            debug_report: false, pc_hist: HashMap::new(), jumps: Default::default(), disc_path,
            cpi: 2, idle_skip: true, idle_loops: HashMap::new(), speed_log: false, prof: Prof::default(),
            // the flash is the console's, not the game's: one file for every game
            save_filename: "dc_flash".to_string(),
            save_battery_path: "user://GD_EMU/DC/Save".to_string(),
            save_state_path: format!("user://GD_EMU/DC/State/{name}"),
        })
    }

    /// Run the CPU up to cycle `target`, taking interrupts between instructions.
    fn run_cpu(&mut self, target: u64) {
        if self.debug_report { return self.run_cpu_traced(target); }
        // a backward jump to the same place several times in a row: maybe a polling loop
        let (mut loop_to, mut loop_n) = (u32::MAX, 0u32);
        while self.dc.now < target {
            if let Some((level, code)) = self.dc.irq {
                if self.cpu.accepts(level) { self.cpu.interrupt(&mut self.dc, code); }
            }
            if self.cpu.sleeping { self.dc.now = target; return; }
            let pc = self.cpu.pc;
            self.cpu.step(&mut self.dc);
            self.dc.now += self.cpi;
            let to = self.cpu.pc;
            if to < pc && pc - to <= 64 && self.idle_skip {
                if to == loop_to { loop_n += 1; } else { loop_to = to; loop_n = 1; }
                // checked once per run of the same loop (4th pass)
                if loop_n == 4 && self.polling_loop(to, pc) {
                    self.prof.idle_cycles += target - self.dc.now;
                    self.dc.now = target;
                    return;
                }
            }
        }
    }

    /// Is the code from `start` to `last` (the branch, or its delay slot) a loop that only
    /// reads memory and tests it? Then it can't end before something else changes memory
    /// or time passes: the CPU can skip to the next event. Only loads (no post-increment),
    /// moves, immediates, tests / compares, AND, MOVT, NOP and branches qualify; no stores,
    /// no DT (a counted delay).
    fn polling_loop(&mut self, start: u32, last: u32) -> bool {
        let words: Vec<u16> = (start..=last).step_by(2).map(|a| crate::common::cpu::sh4::Sh4Bus::fetch16(&mut self.dc, a)).collect();
        if let Some((w, idle)) = self.idle_loops.get(&start) {
            if *w == words { return *idle; }
        }
        let safe = |op: u16| {
            let (hi, lo) = (op >> 12, op & 0xF);
            match hi {
                0x0 => op == 0x0009 || matches!(lo, 0xC..=0xE) || op & 0xF0FF == 0x0029,
                0x2 => matches!(lo, 8 | 9),
                0x3 => matches!(lo, 0 | 2 | 3 | 6 | 7),
                0x4 => matches!(op & 0xFF, 0x11 | 0x15),
                0x5 | 0x9 | 0xA | 0xD | 0xE => true,
                0x6 => !matches!(lo, 4..=6 | 0xA),
                0x8 => matches!(op >> 8 & 0xF, 4 | 5 | 8 | 9 | 0xB | 0xD | 0xF),
                0xC => matches!(op >> 8 & 0xF, 4..=6 | 8 | 9),
                _ => false,
            }
        };
        let idle = words.len() <= 16 && words.iter().all(|&w| safe(w));
        if self.idle_loops.len() > 4096 { self.idle_loops.clear(); }
        self.idle_loops.insert(start, (words, idle));
        idle
    }

    /// `run_cpu` for the debug log: records jumps (and exceptions) and register accesses.
    fn run_cpu_traced(&mut self, target: u64) {
        self.dc.trace_regs = true;
        while self.dc.now < target {
            if let Some((level, code)) = self.dc.irq {
                if self.cpu.accepts(level) {
                    let from = self.cpu.pc;
                    self.cpu.interrupt(&mut self.dc, code);
                    self.jump(from, self.cpu.pc, "irq");
                }
            }
            if self.cpu.sleeping { self.dc.now = target; return; }
            let pc = self.cpu.pc;
            self.dc.cur_pc = pc;
            let expevt = self.dc.chip.expevt;
            self.cpu.step(&mut self.dc);
            self.dc.now += 1;
            if self.cpu.pc != pc.wrapping_add(2) && self.cpu.pc != pc {
                let kind = if self.dc.chip.expevt != expevt || self.cpu.pc == self.cpu.vbr.wrapping_add(0x100) { "exc" } else { "" };
                self.jump(pc, self.cpu.pc, kind);
            }
        }
    }

    /// Note a jump (a branch's delay slot is the "from"); loops count instead of repeating.
    fn jump(&mut self, from: u32, to: u32, kind: &'static str) {
        if let Some(last) = self.jumps.back_mut() {
            if last.0 == from && last.1 == to { last.3 += 1; return; }
        }
        if self.jumps.len() >= 64 { self.jumps.pop_front(); }
        self.jumps.push_back((from, to, kind, 1));
    }

    /// Run until the next vblank (or the limit), then build the picture.
    /// One frontend frame: 1 / 59.94 s of emulated time (the frontend runs at that rate;
    /// a 50 Hz PAL picture still runs at the right speed), then the picture.
    pub fn run_field(&mut self) {
        self.dc.vblank = false;
        self.time_rem += bus::CLOCK * 1001;
        let end = self.dc.now + self.time_rem / 60_000;
        self.time_rem %= 60_000;
        while self.dc.now < end {
            let target = self.dc.next_event().min(self.dc.now + SLICE).max(self.dc.now + 1);
            self.run_cpu(target);
            self.dc.service();
            if self.debug_report { *self.pc_hist.entry(self.cpu.pc).or_insert(0) += 1; }
            if self.dc.soft_reset {
                self.dc.soft_reset = false;
                crate::dlog!("Dreamcast: soft reset (SB_SFRES)");
                self.reset();
            }
        }
        self.dc.pvr.output(&self.dc.vram, &mut self.frame);
        let mode = self.dc.pvr.reg(pvr::SPG_CONTROL);
        if mode != self.video_mode {
            if self.video_mode != u32::MAX {
                let std = match mode >> 6 & 3 { 1 => "NTSC", 2 => "PAL", _ => "VGA" };
                crate::dlog!("Dreamcast: video mode {std}{}, {} lines", if mode & 0x10 != 0 { " interlaced" } else { "" }, self.dc.pvr.lines());
            }
            self.video_mode = mode;
        }
    }

    /// How fast we run: host time per emulated second, split by part.
    fn speed_line(&mut self) {
        let wall = self.prof.since.elapsed().as_secs_f64();
        let emu = (self.dc.now - self.prof.cycles) as f64 / bus::CLOCK as f64;
        if emu <= 0.0 || wall <= 0.0 { return; }
        let span = (self.dc.now - self.prof.cycles) as f64;
        let d = &mut self.dc;
        let ms = |t: std::time::Duration| t.as_secs_f64() * 1000.0 / emu;
        let (r, s, a) = (ms(d.t_render), ms(d.t_sound), ms(d.t_arm));
        let total = wall * 1000.0 / emu;
        crate::dlog!("Dreamcast speed: {:.0}% of real time ({:.0} ms of host time per emulated second: render {r:.0}, sound {s:.0}, ARM7 {a:.0}, SH-4 + rest {:.0}); SH-4 idle-skipped {:.0}%, {} cycles an instruction",
                     100.0 / total * 1000.0, total, total - r - s - a,
                     100.0 * self.prof.idle_cycles as f64 / span, self.cpi);
        d.t_render = Default::default();
        d.t_sound = Default::default();
        d.t_arm = Default::default();
        let now = d.now;
        self.prof = Prof { cycles: now, ..Prof::default() };
    }

    fn report(&mut self) {
        let c = &self.cpu;
        let d = &self.dc;
        crate::dlog!("DC frame {}: PC {:08X}{} SR {:08X} PR {:08X} VBR {:08X} SPC {:08X} FPSCR {:08X}, {} fields, cycle {}",
                 self.current_frame, c.pc, if c.sleeping { " (SLEEP)" } else { "" }, c.sr(), c.pr, c.vbr, c.spc, c.fpscr,
                 d.pvr.frames, d.now);
        if let (Some(p), true) = (&self.disc_path, d.gd.has_disc()) {
            crate::dlog!("  disc {p}: drive state {:02X}, FAD {}, {} sectors read, DMA waiting {}", d.gd.sector_number(), d.gd.fad, d.gd.sectors, d.gd.dma);
        }
        crate::dlog!("  R0-R15 {}", c.r.iter().map(|v| format!("{v:08X}")).collect::<Vec<_>>().join(" "));
        let mut top: Vec<(u32, u32)> = self.pc_hist.iter().map(|(&a, &n)| (a, n)).collect();
        top.sort_by(|a, b| b.1.cmp(&a.1));
        crate::dlog!("  busiest PCs {}", top.iter().take(8).map(|(a, n)| format!("{a:08X}x{n}")).collect::<Vec<_>>().join(" "));
        self.pc_hist.clear();
        let h = &d.holly;
        crate::dlog!("  Holly ISTNRM {:08X} EXT {:08X} ERR {:08X}, IML6 {:08X}/{:08X} IML4 {:08X}/{:08X} IML2 {:08X}/{:08X}, IRL level {}, pending {:X?}",
                 h.istnrm(), h.r(holly::SB_ISTEXT), h.r(holly::SB_ISTERR), h.r(0x130), h.r(0x134), h.r(0x120), h.r(0x124), h.r(0x110), h.r(0x114),
                 h.level(), d.irq);
        let p = &d.pvr;
        crate::dlog!("  PVR SPG_LOAD {:08X} CONTROL {:08X} VBLANK_INT {:08X} HBLANK_INT {:08X}, FB_R_CTRL {:08X} SOF1 {:08X} SOF2 {:08X} SIZE {:08X}, VO_CONTROL {:08X}, line {}",
                 p.reg(pvr::SPG_LOAD), p.reg(pvr::SPG_CONTROL), p.reg(pvr::SPG_VBLANK_INT), p.reg(pvr::SPG_HBLANK_INT),
                 p.reg(pvr::FB_R_CTRL), p.reg(pvr::FB_R_SOF1), p.reg(pvr::FB_R_SOF2), p.reg(pvr::FB_R_SIZE), p.reg(pvr::VO_CONTROL), p.vpos);
        crate::dlog!("  TA {} params {} vertices, list {:?}, {} renders; Maple {} DMAs (MDEN {} MDTSEL {}), AICA ARM reset {}",
                 p.ta.params, p.ta.vertices, p.ta.list, p.renders, d.maple_dmas, h.maple[5], h.maple[4], d.aica.arm_reset);
        let a = &d.aica;
        crate::dlog!("  ARM7 {}: PC {:08X} CPSR {:08X}, {} instructions; SCIEB {:04X} SCIPD {:04X} MCIEB {:04X} MCIPD {:04X} timers {:04X} {:04X} {:04X}, sample {}",
                 if a.arm_reset { "held in reset" } else { "running" }, d.arm.pc(), d.arm.cpsr, d.arm_steps,
                 a.peek16(0x289C), a.scipd, a.peek16(0x28B4), a.mcipd, a.peek16(0x2890), a.peek16(0x2894), a.peek16(0x2898), a.samples);
        let mut pcs: Vec<_> = d.arm_pcs.iter().collect();
        pcs.sort_by(|x, y| y.1.cmp(x.1));
        crate::dlog!("  ARM7 busiest PCs (1 in 64 sampled): {}", pcs.iter().take(10).map(|(p, n)| format!("{p:08X}x{n}")).collect::<Vec<_>>().join(" "));
        let mut rd: Vec<_> = a.arm_reads.iter().collect();
        rd.sort_by(|x, y| y.1.cmp(x.1));
        crate::dlog!("  ARM7 register reads: {}", rd.iter().take(16).map(|(r, n)| format!("{r:04X}x{n}")).collect::<Vec<_>>().join(" "));
        crate::dlog!("  sound RAM 100-13F: {}", (0..16).map(|k| format!("{:08X}", u32::from_le_bytes(a.wave[0x100 + 4 * k..0x104 + 4 * k].try_into().unwrap()))).collect::<Vec<_>>().join(" "));
        let lr = p.last_render;
        crate::dlog!("  last render: {} strips ({} triangles culled, {} modifier volumes), {} pixels, {} tiles; PARAM_BASE {:08X} REGION_BASE {:08X} FB_W_CTRL {:X} FB_W_SOF1 {:08X} BACKGND_T {:08X}; scenes (ISP base, strips, volume triangles) {:?}",
                 lr.polys, lr.culled, lr.volumes, lr.pixels, lr.tiles, p.reg(render::PARAM_BASE), p.reg(render::REGION_BASE), p.reg(render::FB_W_CTRL),
                 p.reg(render::FB_W_SOF1), p.reg(render::ISP_BACKGND_T),
                 p.ta.scenes.iter().chain([&p.ta.cur]).map(|s| (s.isp_base, s.polys.len(), s.mods.len())).collect::<Vec<_>>());
        let ch = &d.chip;
        crate::dlog!("  TMU TSTR {:02X} TCNT {:08X} {:08X} {:08X} TCR {:04X} {:04X} {:04X}, IPRA-C {:04X} {:04X} {:04X}, CHCR2 {:08X} DMAOR {:08X}, CCR {:08X} MMUCR {:08X} QACR {:02X}/{:02X}",
                 ch.tstr, ch.tmu[0].tcnt, ch.tmu[1].tcnt, ch.tmu[2].tcnt, ch.tmu[0].tcr, ch.tmu[1].tcr, ch.tmu[2].tcr,
                 ch.ipr[0], ch.ipr[1], ch.ipr[2], ch.dma[2].chcr, ch.dmaor, ch.ccr, ch.mmucr, ch.qacr[0], ch.qacr[1]);
        if !d.gd.log.is_empty() { crate::dlog!("  GD-ROM: {}", d.gd.log.join(", ")); }
        if !d.unknown.is_empty() {
            crate::dlog!("  unhandled: {}", d.unknown.iter().map(|(a, (w, n))| format!("{}{a:08X}x{n}", if *w { "W" } else { "R" })).collect::<Vec<_>>().join(" "));
        }
        crate::dlog!("  FRQCR {:04X} STBCR {:02X} WTCNT {:02X} WTCSR {:02X} BCR1 {:08X} BCR2 {:04X} MCR {:08X} PCTRA {:08X} PDTRA {:04X} EXPEVT {:03X} INTEVT {:03X} SSR {:08X} SGR {:08X}",
                 ch.frqcr, ch.stbcr, ch.wtcnt, ch.wtcsr, ch.bcr1, ch.bcr2, ch.mcr, ch.pctra, ch.pdtra, ch.expevt, ch.intevt, c.ssr, c.sgr);
        let f = &self.dc.flash.data;
        crate::dlog!("  flash 1A000: {} (region 1A002 '0' JP '1' US '2' EU, broadcast 1A004 '0' NTSC '1' PAL)",
                 f[0x1A000..0x1A010].iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" "));
        crate::dlog!("  last jumps (from > to):");
        for chunk in self.jumps.iter().collect::<Vec<_>>().chunks(6) {
            crate::dlog!("    {}", chunk.iter().map(|(f, t, k, n)| format!("{f:08X}>{t:08X}{}{}", if k.is_empty() { String::new() } else { format!(" {k}") }, if *n > 1 { format!(" x{n}") } else { String::new() })).collect::<Vec<_>>().join("  "));
        }
        if !self.dc.reg_log.is_empty() {
            crate::dlog!("  register accesses since the last report (PC, R/W, address, value; first 400):");
            for l in std::mem::take(&mut self.dc.reg_log) { crate::dlog!("    {l}"); }
        }
        let pc = self.cpu.pc & !1;
        let code: Vec<String> = (0..16u32).map(|k| format!("{:04X}", self.dc.read(pc.wrapping_add(2 * k), 2))).collect();
        crate::dlog!("  code at PC {}", code.join(" "));
    }
}

/// Pad n's state on the Dreamcast controller: B (bottom) = A, A (right) = B, X, Y, Start,
/// d-pad (also full stick deflection), L / R = the triggers.
fn dc_pad(p: &pad::Pad) -> DcPad {
    let mut b: u16 = 0xFFFF;
    for (on, bit) in [(p.b, maple::BTN_A), (p.a, maple::BTN_B), (p.x, maple::BTN_X), (p.y, maple::BTN_Y), (p.start, maple::BTN_START),
                      (p.up, maple::BTN_UP), (p.down, maple::BTN_DOWN), (p.left, maple::BTN_LEFT), (p.right, maple::BTN_RIGHT)] {
        if on { b &= !bit; }
    }
    let axis = |neg: bool, pos: bool| if neg && !pos { 0 } else if pos && !neg { 0xFF } else { 0x80 };
    DcPad { buttons: b, lt: if p.l { 0xFF } else { 0 }, rt: if p.r { 0xFF } else { 0 }, x: axis(p.left, p.right), y: axis(p.up, p.down) }
}

impl System for DcSystem {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        self.run_field();
        self.current_frame += 1;
        if (self.speed_log || self.debug_report) && self.current_frame % 300 == 0 { self.speed_line(); }
        if self.debug_report && matches!(self.current_frame, 10 | 60 | 300) || self.debug_report && self.current_frame % 600 == 0 {
            self.report();
        }
    }

    fn framebuffer(&self) -> &[u8] { &self.frame }

    fn display_info(&self) -> DisplayInfo {
        DisplayInfo {
            buffer_width: OUT_W as i32, buffer_height: OUT_H as i32,
            visible_x: 0, visible_y: 0, visible_width: OUT_W as i32, visible_height: OUT_H as i32,
            target_aspect_ratio: 4.0 / 3.0, ..Default::default()
        }
    }

    fn drain_audio(&mut self) -> Vec<f32> { std::mem::take(&mut self.dc.aica.out) }

    fn sample_rate(&self) -> u32 { 44_100 }

    fn set_input(&mut self, input: u32) {
        let pads = pad::pads(input, self.ext);
        for (k, p) in pads.iter().enumerate() {
            if self.dc.pads[k].is_some() { self.dc.pads[k] = Some(dc_pad(p)); }
        }
    }

    fn set_extra_input(&mut self, ext: u32) {
        self.ext = ext;
        // players 3 / 4 get a controller once they press something
        for k in 2..4 {
            if self.dc.pads[k].is_none() && (ext >> (8 + 8 * (k - 2)) & 0xFF != 0) { self.dc.pads[k] = Some(DcPad::default()); }
        }
    }

    fn save_state(&self) -> Vec<u8> {
        let s = StateRef { version: STATE_VERSION, cpu: &self.cpu, dc: &self.dc, frame: self.current_frame };
        let config = bincode::config::standard().with_fixed_int_encoding();
        bincode::serde::encode_to_vec(&s, config).unwrap_or_default()
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let config = bincode::config::standard().with_fixed_int_encoding();
        let (s, _): (State, usize) = bincode::serde::decode_from_slice(data, config)
            .map_err(|e| format!("Failed to decode Dreamcast save state: {e:?}"))?;
        if s.version != STATE_VERSION { return Err(format!("save state version {} (expected {STATE_VERSION})", s.version)); }
        let mut dc = s.dc;
        dc.bios = std::mem::take(&mut self.dc.bios);
        dc.pads = self.dc.pads;
        dc.gd.disc = self.dc.gd.disc.clone();
        dc.flash.broadcast = self.dc.flash.broadcast;
        dc.flash.apply_broadcast();
        dc.update_irq();
        self.dc = dc;
        self.cpu = s.cpu;
        self.current_frame = s.frame;
        Ok(())
    }

    fn load_sram(&mut self, data: &[u8]) {
        if data.len() == flash::SIZE {
            self.dc.flash.data.copy_from_slice(data);
            // the save holds the factory bytes too: keep the TV standard option
            self.dc.flash.apply_broadcast();
            crate::dlog!("Dreamcast: flash settings loaded from the save file");
        }
    }

    fn get_sram(&self) -> Option<&[u8]> { Some(&self.dc.flash.data) }

    fn reset(&mut self) {
        self.cpu.reset();
        self.dc.reset();
    }

    fn has_battery(&self) -> bool { true }
    fn save_battery_path(&self) -> &str { &self.save_battery_path }
    fn save_filename(&self) -> &str { &self.save_filename }
    fn save_state_path(&self) -> &str { &self.save_state_path }
    fn power_on(&mut self) {}
    fn power_off(&mut self) {}
    fn is_sram_dirty(&self) -> bool { self.dc.flash.dirty }
    fn clear_sram_dirty(&mut self) { self.dc.flash.dirty = false; }
    fn get_current_frame(&self) -> i64 { self.current_frame as i64 }

    fn set_option(&mut self, key: &str, value: i64) {
        match key {
            "debug_report" => self.debug_report = value != 0,
            "dc_cable" => self.dc.chip.cable = (value as u16) & 3,
            "dc_cpu_ratio" => self.cpi = value.clamp(1, 4) as u64,
            "dc_idle_skip" => self.idle_skip = value != 0,
            "speed_log" => self.speed_log = value != 0,
            _ => {}
        }
    }

    /// One field. The frontend asks once, at load, before the BIOS has set up the sync
    /// generator, and every Dreamcast video mode (NTSC, VGA) runs at 59.94 Hz; PAL (50 Hz)
    /// when the PAL bit is already set.
    fn frame_time_usec(&self) -> i64 {
        if self.dc.pvr.reg(pvr::SPG_CONTROL) & 0x80 != 0 { 20_000 } else { 16_683 }
    }
}
