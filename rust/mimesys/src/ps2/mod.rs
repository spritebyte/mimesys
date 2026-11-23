//! Sony PlayStation 2. Plan and status: the project docs `claude/ps2_plan.md` /
//! `claude/ps2_status.md`.
//!
//! Stage 2 (this): the EE (`common/cpu/r5900`) and the IOP (`common/cpu/r3000`, PRId 1F
//! so the BIOS knows which CPU it is) on their buses (`hw.rs`), the EE's DMAC
//! (`dmac.rs`), INTC, timers (`timers.rs`), the SIF, the IOP's DMA (`iop_dma.rs`), INTC
//! and timers, a disc-less CDVD (`cdvd.rs`) and the GS's privileged registers. The screen
//! stays black until the GS draws (stage 3); the BIOS's console output goes to the log.
//!
//! Timing (NTSC): EE 294.912 MHz, bus = EE / 2, IOP = EE / 8; 262 lines of 9370 bus
//! cycles (18 740 EE cycles); VBLANK from line 240. The EE runs in 64-cycle slices, the
//! IOP, DMA and timers catch up after each.

pub mod cdvd;
pub mod dmac;
pub mod gs;
pub mod hw;
pub mod iso;
pub mod memcard;
pub mod iop_dma;
pub mod sio2;
pub mod timers;
pub mod vif;
pub mod vu;
#[cfg(test)]
mod tests;

use crate::common::cpu::r3000::R3000;
use crate::common::cpu::r5900::R5900;
use crate::common::input;
use crate::common::system::{DisplayInfo, System};
use hw::{EeView, Hw, IopView, BIOS_SIZE};

static DEBUG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The debug log (option "ps2_debug"): the consoles' output is always logged; this adds
/// the chips' state every 5 s, first touches of unmodelled registers, and the EE's last
/// instructions before a bad exception.
pub fn debug() -> bool { DEBUG.load(std::sync::atomic::Ordering::Relaxed) }

const STATE_VERSION: u32 = 1;
/// Printed at the top of the debug log so a log says which build made it
const PATCH: &str = "0236";

pub const EE_HZ: u64 = 294_912_000;
const LINES: u32 = 262;
const VBLANK_LINE: u32 = 240;
const EE_PER_LINE: u32 = 18_740;
const SLICE: u32 = 64;
const SCREEN_W: usize = 640;
const SCREEN_H: usize = 448;

/// A PS2 BIOS dump: 4 MB, named like one (scph*, *ps2*) or holding the ROMDIR ("RESET")
/// table near the start
pub fn is_bios(data: &[u8], file: &str) -> bool {
    let f = file.to_ascii_lowercase();
    data.len() == BIOS_SIZE && (f.starts_with("scph") || f.contains("ps2")
        || data[..0x10000].windows(10).any(|w| w == b"RESET\0\0\0\0\0"))
}

pub struct Ps2System {
    pub ee: R5900,
    pub iop: R3000,
    pub hw: Hw,
    /// EE cycles the IOP is behind (8 a cycle)
    iop_owed: i64,
    /// hblanks since the timers last ran
    pub(crate) frame: Vec<u8>,
    /// the picture's size (from the GS display registers)
    pub(crate) size: (usize, usize),
    current_frame: u64,
    save_filename: String,
    save_path: String,
    /// the frontend's extra buttons (R2, L3, R3, Analog) and Analog's last state
    ext: u32,
    analog_held: [bool; 2],
    /// when INTC's line last went up (EE cycles), for the delivery delay
    int0_since: Option<u64>,
    /// idle-loop checks: the PC last checked and the answer (cleared every line)
    ee_idle: Option<(u32, bool)>,
    iop_idle: Option<(u32, bool)>,
    /// speed report: cycles skipped in idle loops, and where the last report was
    skipped_ee: u64,
    skipped_iop: u64,
    last_report: Option<(std::time::Instant, u64, u64, u64, u64, u64, u64, u64, u64)>,
}

/// EE cycles between INTC_STAT & INTC_MASK going non-zero and the EE seeing INT0
const INT0_DELAY: u64 = 128;

/// A loop of nothing but NOPs and one branch back (BEQ $0, $0 or J) with a NOP delay
/// slot, containing `pc`: only an interrupt gets out of it. `read` gives code words.
fn idle_loop(read: impl Fn(u32) -> Option<u32>, pc: u32) -> bool {
    for k in 0..12 {
        let a = pc.wrapping_add(4 * k);
        let w = match read(a) { Some(w) => w, None => return false };
        if w == 0 { continue; }
        let target = if w >> 16 == 0x1000 {
            a.wrapping_add(4).wrapping_add(((w as i16 as i32) << 2) as u32)
        } else if w >> 26 == 2 {
            (a.wrapping_add(4) & 0xF000_0000) | (w & 0x03FF_FFFF) << 2
        } else {
            return false;
        };
        if read(a.wrapping_add(4)) != Some(0) || target > pc || a - target > 64 { return false; }
        return (target..a).step_by(4).all(|t| read(t) == Some(0));
    }
    false
}

impl Ps2System {
    pub fn from_rom(rom: &[u8], filename: &str, bios: &[u8]) -> Result<Self, String> {
        let name = filename.rsplit(['/', '\\']).next().unwrap_or(filename);
        // a BIOS opened directly wins over the BIOS folder's (so either can be picked)
        let bios = if is_bios(rom, name) {
            rom.to_vec()
        } else if bios.len() == BIOS_SIZE {
            bios.to_vec()
        } else {
            return Err("PlayStation 2 needs its BIOS: put a 4 MB BIOS dump (e.g. scph39001.bin) or OpenPS2BIOS in the BIOS folder".into());
        };
        let stem = name.rsplit_once('.').map_or(name, |(s, _)| s).to_string();
        let mut s = Self {
            ee: R5900::new(),
            iop: R3000::new(),
            hw: Hw::new(bios),
            iop_owed: 0,
            frame: vec![0; SCREEN_W * SCREEN_H * 4],
            size: (SCREEN_W, SCREEN_H),
            current_frame: 0,
            save_filename: stem,
            save_path: "user://GD_EMU/PS2/State".to_string(),
            ext: 0,
            analog_held: [false; 2],
            int0_since: None,
            ee_idle: None,
            iop_idle: None,
            skipped_ee: 0,
            skipped_iop: 0,
            last_report: None,
        };
        s.reset_cpus();
        // a disc image: the frontend sends its path
        if rom.starts_with(crate::segacd::PATH_MAGIC) {
            let path = crate::segacd::disc_path(rom).ok_or("not a disc image path")?;
            let d = crate::common::disc::Disc::open(&path)?;
            s.hw.cdvd.insert(d);
        }
        Ok(s)
    }

    fn reset_cpus(&mut self) {
        self.ee = R5900::new();
        self.iop = R3000::new();
        // the BIOS runs the IOP's boot code when PRId < 59h
        self.iop.cop0[crate::common::cpu::r3000::PRID] = 0x1F;
        self.iop.mips2_traps = true;
        self.iop_owed = 0;
    }

    /// The EE is in an idle loop (see `idle_loop`)
    fn ee_idle_now(&mut self) -> bool {
        let pc = self.ee.pc;
        if let Some((p, idle)) = self.ee_idle { if p == pc { return idle; } }
        let (ee, h) = (&self.ee, &self.hw);
        let read = |va: u32| -> Option<u32> {
            let pa = ee.virt_to_phys(va)?;
            if (pa as usize) + 4 <= h.ram.len() { return Some(u32::from_le_bytes(h.ram[pa as usize..pa as usize + 4].try_into().unwrap())); }
            if pa >= 0x1FC0_0000 { let o = pa as usize & (BIOS_SIZE - 1); return Some(u32::from_le_bytes(h.bios[o..o + 4].try_into().unwrap())); }
            None
        };
        let idle = idle_loop(read, pc);
        self.ee_idle = Some((pc, idle));
        idle
    }

    /// The IOP is in an idle loop
    fn iop_idle_now(&mut self) -> bool {
        let pc = self.iop.pc;
        if let Some((p, idle)) = self.iop_idle { if p == pc { return idle; } }
        let h = &self.hw;
        let read = |va: u32| -> Option<u32> {
            let pa = va & 0x1FFF_FFFF;
            if (pa as usize) + 4 <= h.iop_ram.len() { return Some(u32::from_le_bytes(h.iop_ram[pa as usize..pa as usize + 4].try_into().unwrap())); }
            if pa >= 0x1FC0_0000 { let o = pa as usize & (BIOS_SIZE - 1); return Some(u32::from_le_bytes(h.bios[o..o + 4].try_into().unwrap())); }
            None
        };
        let idle = idle_loop(read, pc);
        self.iop_idle = Some((pc, idle));
        idle
    }

    fn run_line(&mut self, line: u32) {
        // code can change: the idle checks are redone each line
        self.ee_idle = None;
        self.iop_idle = None;
        let mut done = 0;
        while done < EE_PER_LINE {
            let target = SLICE.min(EE_PER_LINE - done);
            let mut c = 0;
            if !self.ee.irq_ready() && self.ee_idle_now() {
                // nothing to run until an interrupt: let the time pass
                self.ee.idle(target);
                self.skipped_ee += target as u64;
                c = target;
            } else {
                let mut bus = EeView(&mut self.hw);
                while c < target { c += self.ee.step(&mut bus); }
            }
            done += c;
            // the IOP at an eighth of the EE's clock
            self.iop_owed += c as i64;
            let mut iop_cycles = 0u32;
            if self.iop_owed >= 8 && !self.iop.irq_ready() && self.iop_idle_now() {
                let n = (self.iop_owed / 8) as u32;
                self.iop.idle(n);
                self.iop_owed -= 8 * n as i64;
                self.skipped_iop += n as u64;
                iop_cycles = n;
            } else {
                let mut bus = IopView(&mut self.hw);
                while self.iop_owed >= 8 {
                    let n = self.iop.step(&mut bus);
                    self.iop_owed -= 8 * n as i64;
                    iop_cycles += n;
                }
            }
            self.tick(c, iop_cycles, 0);
        }
        // HBLANK
        self.tick(0, 0, 1);
        self.hw.gs.csr |= 4;
        if line == VBLANK_LINE {
            self.hw.raise_intc(hw::INT_VBON);
            self.hw.raise_iop(hw::IOP_VBLANK);
            self.hw.gs.csr |= 8;
            self.hw.gs.field = !self.hw.gs.field;
        } else if line == 0 {
            self.hw.raise_intc(hw::INT_VBOF);
            self.hw.raise_iop(hw::IOP_VBLANK_END);
        }
        self.hw.update_gs_irq();
        self.update_irqs();
    }

    /// Timers and DMA after `ee` EE cycles, `iop` IOP cycles and `hblank` HBLANKs
    fn tick(&mut self, ee: u32, iop: u32, hblank: u32) {
        let h = &mut self.hw;
        for n in 0..4 {
            if h.timers[n].tick(ee / 2, hblank) { h.intc_stat |= hw::INT_TIMER0 << n; }
        }
        for n in 0..6 {
            if h.iop_timers[n].tick(n, iop, hblank) { h.i_stat |= timers::IOP_TIMER_IRQ[n]; }
        }
        h.dmac.step(&mut h.ram, &mut self.ee.scratch, &mut h.sif);
        // VIF1: its DIRECT quadwords join the GIF's (PATH2 before PATH3)
        if !h.dmac.vif0_out.is_empty() {
            let words = std::mem::take(&mut h.dmac.vif0_out);
            h.vif_feed(false, &words);
        }
        if !h.dmac.vif1_out.is_empty() {
            let words = std::mem::take(&mut h.dmac.vif1_out);
            h.vif_feed(true, &words);
        }
        // GIF PATH3: what the GIF channel moved goes to the GS
        if !h.dmac.gif_out.is_empty() {
            let q = std::mem::take(&mut h.dmac.gif_out);
            for v in q { h.gs.feed(v); }
            h.update_gs_irq();
        }
        if h.iop_dma.step(&mut h.iop_ram, &mut h.sif, &mut h.sio2) { h.i_stat |= hw::IOP_DMA; }
        // the CDVD drive: seeks, and sectors into DMA channel 3
        h.cdvd.tick(iop, &mut h.iop_dma, &mut h.iop_ram, &mut h.i_stat);
        if h.iop_dma.irq_rise() { h.i_stat |= hw::IOP_DMA; }
        // SIO2 runs after its input DMA; its reply DMA then goes on the next pass
        if h.sio2.run() {
            h.i_stat |= hw::IOP_SIO2;
            if h.iop_dma.step(&mut h.iop_ram, &mut h.sif, &mut h.sio2) { h.i_stat |= hw::IOP_DMA; }
        }
        // a SIF0 transfer may have new data for the EE
        if h.dmac.ch[dmac::SIF0].chcr & 0x100 != 0 { h.dmac.step(&mut h.ram, &mut self.ee.scratch, &mut h.sif); }
        self.update_irqs();
    }

    fn update_irqs(&mut self) {
        // INT0 (INTC) reaches the EE a little after INTC_STAT changes, as on the console:
        // code that polls INTC_STAT with interrupts on (OSDSYS's browser waits for VBON
        // that way) must get to see the bit before the kernel's handler clears it
        let want = self.hw.int0();
        if !want {
            self.int0_since = None;
            self.ee.set_int0(false);
        } else {
            let since = *self.int0_since.get_or_insert(self.ee.cycles);
            if self.ee.cycles - since >= INT0_DELAY { self.ee.set_int0(true); }
        }
        self.ee.set_int1(self.hw.dmac.int1());
        self.iop.set_irq(self.hw.iop_irq());
    }

    fn report(&mut self) {
        // speed since the last report
        let now = std::time::Instant::now();
        let cur = (self.current_frame, self.ee.cycles, self.skipped_ee, self.iop.cycles, self.skipped_iop, self.hw.vu1.total_cycles,
            self.hw.gs.draw_ns, self.hw.gs.pixels);
        if let Some((t, f0, ee0, se0, iop0, si0, vu0, gs0, px0)) = self.last_report {
            let frames = (cur.0 - f0).max(1) as f64;
            let secs = now.duration_since(t).as_secs_f64().max(1e-6);
            let fps = frames / secs;
            let ee = (cur.1 - ee0) as f64;
            let pct = |a: u64, b: f64| if b > 0.0 { 100.0 * a as f64 / b } else { 0.0 };
            crate::dlog!("PS2:   speed {:.1} fps ({:.0}% of full) | per frame: EE {:.2} M cycles ({:.0}% idle-skipped), IOP {:.0}% idle-skipped, VU1 {:.0} k cycles, GS {} prims",
                fps, 100.0 * fps / 59.94, ee / frames / 1e6, pct(cur.2 - se0, ee), pct(cur.4 - si0, (cur.3 - iop0) as f64),
                (cur.5 - vu0) as f64 / frames / 1e3, self.hw.gs.prims);
            crate::dlog!("PS2:   GS drawing {:.1} ms a frame ({:.2} M pixels a frame, {} threads)",
                (cur.6 - gs0) as f64 / frames / 1e6, (cur.7 - px0) as f64 / frames / 1e6, gs::draw::draw_threads());
        }
        self.last_report = Some((now, cur.0, cur.1, cur.2, cur.3, cur.4, cur.5, cur.6, cur.7));
        let (e, i, h) = (&self.ee, &self.iop, &self.hw);
        crate::dlog!("PS2: frame {} EE pc {:08X} ra {:08X} status {:08X} cause {:08X} | IOP pc {:08X} ra {:08X} sr {:08X}",
            self.current_frame + 1, e.pc, e.gpr[31] as u32, e.cop0[12], e.cause(), i.pc, i.regs[31], i.cop0[12]);
        crate::dlog!("PS2:   INTC {:04X}/{:04X} D_STAT {:08X} D_CTRL {:X} SIF0 {:08X} SIF1 {:08X} | IOP I_STAT {:05X}/{:05X} ctrl {} DICR {:08X} DICR2 {:08X}",
            h.intc_stat, h.intc_mask, h.dmac.stat, h.dmac.ctrl, h.dmac.ch[dmac::SIF0].chcr, h.dmac.ch[dmac::SIF1].chcr,
            h.i_stat, h.i_mask, h.i_ctrl, h.iop_dma.dicr, h.iop_dma.dicr2);
        {
            let c = &h.cdvd;
            crate::dlog!("PS2:   CDVD disc {:02X} {} serial {} | ready {:02X} status {:02X} pos {} op {:?} | N {:02X?} | sectors {}",
                c.disc_type, if c.dvd { "DVD" } else { "CD" }, c.serial, c.ready, c.status, c.pos, c.op,
                &c.n_log[c.n_log.len().saturating_sub(24)..], c.sectors);
        }
        crate::dlog!("PS2:   SIF MSFLG {:08X} SMFLG {:08X} MSCOM {:08X} SMCOM {:08X} fifo0 {} fifo1 {} | EE DMA done {:?} | IOP DMA done {:?} | CDVD S {:02X?}",
            h.sif.msflg, h.sif.smflg, h.sif.mscom, h.sif.smcom, h.sif.fifo0.len(), h.sif.fifo1.len(), h.dmac.done, h.iop_dma.done, &h.cdvd.log[..h.cdvd.log.len().min(24)]);
        crate::dlog!("PS2:   SIO2 transfers {} | GS prims {} pixels {} PMODE {:X} DISPFB1 {:X} DISPLAY1 {:X} DISPFB2 {:X} DISPLAY2 {:X} SMODE2 {:X} | GIF DMA done {}",
            h.sio2.transfers, h.gs.prims, h.gs.pixels, h.gs.pregs[0], h.gs.pregs[7], h.gs.pregs[8], h.gs.pregs[9], h.gs.pregs[10],
            h.gs.pregs[gs::SMODE2], h.dmac.done[dmac::GIF]);
        if !h.sio2.card_counts.is_empty() {
            let c: Vec<String> = h.sio2.card_counts.iter().map(|(k, n)| format!("{k:02X}x{n}")).collect();
            crate::dlog!("PS2:   memory card commands so far: {}", c.join(" "));
        }
        {
            let l: Vec<String> = h.sio2.log.iter().map(|(s3, c, n, r1, rep)|
                format!("[{s3:X} {:02X} {:02X} {:02X} len {n} -> {r1:X} {:02X?}]", c[0], c[1], c[2], rep)).collect();
            crate::dlog!("PS2:   last memory card commands: {}", l.join(" "));
            crate::dlog!("PS2:   memory card 1 {}", match &h.sio2.cards[0] { Some(c) => format!("in, term {:02X}", c.term), None => "not in".into() });
        }
        crate::dlog!("PS2:   VIF0 MSCAL {} UNPACK {} | VIF1 DIRECT qw {} MSCAL {} UNPACK {} | VU0 programs {} | VU1 programs {} XGKICK {} illegal {}/{}",
            h.vif0.mscal, h.vif0.unpacks, h.vif1.direct_qw, h.vif1.mscal, h.vif1.unpacks, h.vu0.programs, h.vu1.programs, h.vu1.kicks,
            h.vu0.illegal, h.vu1.illegal);
        crate::dlog!("PS2:   GS FRAME_1 {:X} ZBUF_1 {:X} SCISSOR_1 {:X} XYOFFSET_1 {:X} TEST_1 {:X}",
            h.gs.regs[0x4C], h.gs.regs[0x4E], h.gs.regs[0x40], h.gs.regs[0x18], h.gs.regs[0x47]);
        if let Some((code, epc, bad)) = e.last_exception {
            crate::dlog!("PS2:   EE last exception {code} at {epc:08X} (BadVAddr {bad:08X})");
        }
        // where the EE is spinning: its last instructions, registers, recent I/O
        let recent = e.recent(48);
        if !recent.is_empty() {
            let mut line = String::from("PS2:   EE last instructions:");
            for (k, (pc, op)) in recent.iter().enumerate() {
                if k % 6 == 0 { crate::dlog!("{line}"); line = String::from("     "); }
                line += &format!(" {pc:08X}:{op:08X}");
            }
            crate::dlog!("{line}");
            for row in 0..4 {
                let r: Vec<String> = (0..8).map(|k| { let n = row * 8 + k; format!("r{n:02} {:016X}", e.gpr[n] as u64) }).collect();
                crate::dlog!("PS2:   {}", r.join(" "));
            }
            crate::dlog!("PS2:   COP0 Count {:08X} Compare {:08X} EPC {:08X}", e.cop0[9], e.cop0[11], e.cop0[14]);
        }
        // and the IOP's
        let recent = i.recent(36);
        if !recent.is_empty() {
            let mut line = String::from("PS2:   IOP last instructions:");
            for (k, (pc, op)) in recent.iter().enumerate() {
                if k % 6 == 0 { crate::dlog!("{line}"); line = String::from("     "); }
                line += &format!(" {pc:08X}:{op:08X}");
            }
            crate::dlog!("{line}");
            for row in 0..4 {
                let r: Vec<String> = (0..8).map(|k| { let n = row * 8 + k; format!("r{n:02} {:08X}", i.regs[n]) }).collect();
                crate::dlog!("PS2:   IOP {}", r.join(" "));
            }
            let io: Vec<String> = h.iop_ring.iter().map(|&(a, v, w)| format!("{}{a:08X}={v:08X}", if w { "W " } else { "R " })).collect();
            crate::dlog!("PS2:   recent IOP I/O: {}", io.join(" "));
        }
        let io: Vec<String> = h.io_ring.iter().map(|&(a, v, w)| format!("{}{a:08X}={v:08X}", if w { "W " } else { "R " })).collect();
        if !io.is_empty() { crate::dlog!("PS2:   recent EE I/O: {}", io.join(" ")); }
    }
}

impl System for Ps2System {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        for line in 0..LINES { self.run_line(line); }
        self.size = self.hw.gs.display(&mut self.frame);
        self.hw.sio2.card_out = self.hw.sio2.card_out.saturating_sub(1);
        if debug() && self.current_frame % 300 == 299 { self.report(); }
        self.current_frame += 1;
    }

    fn framebuffer(&self) -> &[u8] { &self.frame }

    fn display_info(&self) -> DisplayInfo {
        let (w, h) = self.size;
        DisplayInfo {
            buffer_width: w as i32,
            buffer_height: h as i32,
            visible_x: 0,
            visible_y: 0,
            visible_width: w as i32,
            visible_height: h as i32,
            target_aspect_ratio: 4.0 / 3.0,
            ..Default::default()
        }
    }

    fn drain_audio(&mut self) -> Vec<f32> { Vec::new() }
    fn sample_rate(&self) -> u32 { 48_000 }
    /// The DualShock 2s, mapped like the PS1's pads (B = Cross, A = Circle, X = Square,
    /// Y = Triangle, L / R = L1 / R1, Z = L2, extras R2 / L3 / R3 / Analog); a pressed
    /// button reads full pressure.
    fn set_input(&mut self, input: u32) {
        let ext = self.ext;
        let pads = input::pads(input, ext);
        for k in 0..2 {
            let p = &pads[k];
            let [r2b, l3b, r3b, anb] = if k == 0 {
                [input::EXT_P1_R2, input::EXT_P1_L3, input::EXT_P1_R3, input::EXT_P1_ANALOG]
            } else {
                [input::EXT_P2_R2, input::EXT_P2_L3, input::EXT_P2_R3, input::EXT_P2_ANALOG]
            };
            let (r2, l3, r3) = (ext & r2b != 0, ext & l3b != 0, ext & r3b != 0);
            let analog_btn = ext & anb != 0;
            let pad = &mut self.hw.sio2.pads[k];
            if analog_btn && !self.analog_held[k] { pad.analog_button(); }
            self.analog_held[k] = analog_btn;
            let dir = crate::ps1::stick_or_dpad(pad.analog, pad.axes, [p.up, p.right, p.down, p.left]);
            let bits = [
                (p.coin, 0), (l3, 1), (r3, 2), (p.start, 3), (dir[0], 4), (dir[1], 5), (dir[2], 6), (dir[3], 7),
                (p.z, 8), (r2, 9), (p.l, 10), (p.r, 11), (p.y, 12), (p.a, 13), (p.b, 14), (p.x, 15),
            ];
            let mut v = 0u16;
            for (on, b) in bits { if on { v |= 1 << b; } }
            pad.buttons = !v;
            // right, left, up, down, triangle, circle, cross, square, L1, R1, L2, R2
            let order = [5, 7, 4, 6, 12, 13, 14, 15, 10, 11, 8, 9];
            pad.pressures = order.map(|b| if v >> b & 1 != 0 { 0xFF } else { 0 });
        }
    }

    fn set_extra_input(&mut self, ext: u32) { self.ext = ext; }

    fn memory_card(&self) -> Option<Vec<u8>> {
        self.hw.sio2.cards[0].as_ref().map(|c| c.data.clone())
    }

    /// Slot 1's card (an empty / wrong-sized image: a blank card). A swap takes the old
    /// card out for a second first.
    fn insert_memory_card(&mut self, data: &[u8], swap: bool) -> bool {
        let c = memcard::MemCard::new(data);
        if let Some(info) = memcard::summary(&c.data) { println!("PS2: memory card 1 put in ({})", info.join(", ")); }
        self.hw.sio2.cards[0] = Some(c);
        if swap { self.hw.sio2.card_out = 60; }
        true
    }

    fn memory_card_dirty(&mut self) -> bool {
        self.hw.sio2.cards[0].as_mut().is_some_and(|c| std::mem::take(&mut c.dirty))
    }

    fn set_analog(&mut self, port: usize, sticks: [u8; 4]) {
        if let Some(p) = self.hw.sio2.pads.get_mut(port) {
            let [lx, ly, rx, ry] = sticks;
            p.axes = [rx, ry, lx, ly];
        }
    }

    fn rumble(&self, port: usize) -> (u8, u8) {
        self.hw.sio2.pads.get(port).map_or((0, 0), |p| (if p.small { 0xFF } else { 0 }, p.large))
    }

    fn save_state(&self) -> Vec<u8> {
        let s = (&self.ee, &self.iop, &self.hw, self.iop_owed, self.current_frame);
        crate::arcade::state::encode(&self.save_filename, STATE_VERSION, &s)
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let (mut ee, iop, mut hw, owed, frame): (R5900, R3000, Hw, i64, u64) =
            crate::arcade::state::decode(&self.save_filename, STATE_VERSION, data)?;
        hw.bios = std::mem::take(&mut self.hw.bios);
        hw.debug = self.hw.debug;
        hw.cdvd.disc = self.hw.cdvd.disc.take();
        // the cards in the slots stay (a state doesn't carry card images)
        for k in 0..2 {
            let old = self.hw.sio2.cards[k].take();
            hw.sio2.cards[k] = match (old, hw.sio2.cards[k].take()) {
                (Some(mut c), Some(saved)) => { c.term = saved.term; Some(c) }
                (old, _) => old,
            };
        }
        ee.rebuild_tlb();
        self.ee = ee;
        self.iop = iop;
        self.hw = hw;
        self.iop_owed = owed;
        self.current_frame = frame;
        Ok(())
    }

    fn load_sram(&mut self, _data: &[u8]) {}
    fn get_sram(&self) -> Option<&[u8]> { None }

    fn reset(&mut self) {
        let bios = std::mem::take(&mut self.hw.bios);
        let debug = self.hw.debug;
        let cards = std::mem::take(&mut self.hw.sio2.cards);
        let disc = self.hw.cdvd.disc.take();
        let (dt, dvd, serial) = (self.hw.cdvd.disc_type, self.hw.cdvd.dvd, self.hw.cdvd.serial.clone());
        self.hw = Hw::new(bios);
        self.hw.debug = debug;
        self.hw.sio2.cards = cards;
        self.hw.cdvd.disc = disc;
        (self.hw.cdvd.disc_type, self.hw.cdvd.dvd, self.hw.cdvd.serial) = (dt, dvd, serial);
        self.reset_cpus();
        if debug { self.ee.set_trace(true); }
    }

    fn has_battery(&self) -> bool { false }

    fn set_option(&mut self, key: &str, value: i64) {
        if key == "debug_report" {
            if value != 0 { crate::dlog!("PS2 core: patch {PATCH}"); }
            DEBUG.store(value != 0, std::sync::atomic::Ordering::Relaxed);
            self.hw.debug = value != 0;
            self.ee.set_trace(value != 0);
            self.iop.set_trace(value != 0);
        }
    }

    fn save_battery_path(&self) -> &str { &self.save_path }
    fn save_filename(&self) -> &str { &self.save_filename }
    fn save_state_path(&self) -> &str { &self.save_path }
    fn power_on(&mut self) {}
    fn power_off(&mut self) {}
    fn is_sram_dirty(&self) -> bool { false }
    fn clear_sram_dirty(&mut self) {}
    fn get_current_frame(&self) -> i64 { self.current_frame as i64 }

    fn frame_time_usec(&self) -> i64 { (LINES as u64 * EE_PER_LINE as u64 * 1_000_000 / EE_HZ) as i64 }
}
