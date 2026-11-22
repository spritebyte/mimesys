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
pub mod hw;
pub mod iop_dma;
pub mod timers;
#[cfg(test)]
mod tests;

use crate::common::cpu::r3000::R3000;
use crate::common::cpu::r5900::R5900;
use crate::common::system::{DisplayInfo, System};
use hw::{EeView, Hw, IopView, BIOS_SIZE};

static DEBUG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The debug log (option "ps2_debug"): the consoles' output is always logged; this adds
/// the chips' state every 5 s, first touches of unmodelled registers, and the EE's last
/// instructions before a bad exception.
pub fn debug() -> bool { DEBUG.load(std::sync::atomic::Ordering::Relaxed) }

const STATE_VERSION: u32 = 1;
/// Printed at the top of the debug log so a log says which build made it
const PATCH: &str = "0217";

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
    frame: Vec<u8>,
    current_frame: u64,
    save_filename: String,
    save_path: String,
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
            current_frame: 0,
            save_filename: stem,
            save_path: "user://GD_EMU/PS2/State".to_string(),
        };
        s.reset_cpus();
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

    fn run_line(&mut self, line: u32) {
        let mut done = 0;
        while done < EE_PER_LINE {
            let target = SLICE.min(EE_PER_LINE - done);
            let mut c = 0;
            {
                let mut bus = EeView(&mut self.hw);
                while c < target { c += self.ee.step(&mut bus); }
            }
            done += c;
            // the IOP at an eighth of the EE's clock
            self.iop_owed += c as i64;
            let mut iop_cycles = 0u32;
            {
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
        if h.iop_dma.step(&mut h.iop_ram, &mut h.sif) { h.i_stat |= hw::IOP_DMA; }
        // a SIF0 transfer may have new data for the EE
        if h.dmac.ch[dmac::SIF0].chcr & 0x100 != 0 { h.dmac.step(&mut h.ram, &mut self.ee.scratch, &mut h.sif); }
        self.update_irqs();
    }

    fn update_irqs(&mut self) {
        self.ee.set_int0(self.hw.int0());
        self.ee.set_int1(self.hw.dmac.int1());
        self.iop.set_irq(self.hw.iop_irq());
    }

    fn report(&self) {
        let (e, i, h) = (&self.ee, &self.iop, &self.hw);
        crate::dlog!("PS2: frame {} EE pc {:08X} ra {:08X} status {:08X} cause {:08X} | IOP pc {:08X} ra {:08X} sr {:08X}",
            self.current_frame + 1, e.pc, e.gpr[31] as u32, e.cop0[12], e.cause(), i.pc, i.regs[31], i.cop0[12]);
        crate::dlog!("PS2:   INTC {:04X}/{:04X} D_STAT {:08X} D_CTRL {:X} SIF0 {:08X} SIF1 {:08X} | IOP I_STAT {:05X}/{:05X} ctrl {} DICR {:08X} DICR2 {:08X}",
            h.intc_stat, h.intc_mask, h.dmac.stat, h.dmac.ctrl, h.dmac.ch[dmac::SIF0].chcr, h.dmac.ch[dmac::SIF1].chcr,
            h.i_stat, h.i_mask, h.i_ctrl, h.iop_dma.dicr, h.iop_dma.dicr2);
        crate::dlog!("PS2:   SIF MSFLG {:08X} SMFLG {:08X} MSCOM {:08X} SMCOM {:08X} fifo0 {} fifo1 {} | EE DMA done {:?} | IOP DMA done {:?} | CDVD S {:02X?}",
            h.sif.msflg, h.sif.smflg, h.sif.mscom, h.sif.smcom, h.sif.fifo0.len(), h.sif.fifo1.len(), h.dmac.done, h.iop_dma.done, h.cdvd.log);
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
        let io: Vec<String> = h.io_ring.iter().map(|&(a, v, w)| format!("{}{a:08X}={v:08X}", if w { "W " } else { "R " })).collect();
        if !io.is_empty() { crate::dlog!("PS2:   recent EE I/O: {}", io.join(" ")); }
    }
}

impl System for Ps2System {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        for line in 0..LINES { self.run_line(line); }
        if debug() && self.current_frame % 300 == 299 { self.report(); }
        self.current_frame += 1;
    }

    fn framebuffer(&self) -> &[u8] { &self.frame }

    fn display_info(&self) -> DisplayInfo {
        DisplayInfo {
            buffer_width: SCREEN_W as i32,
            buffer_height: SCREEN_H as i32,
            visible_x: 0,
            visible_y: 0,
            visible_width: SCREEN_W as i32,
            visible_height: SCREEN_H as i32,
            target_aspect_ratio: 4.0 / 3.0,
            ..Default::default()
        }
    }

    fn drain_audio(&mut self) -> Vec<f32> { Vec::new() }
    fn sample_rate(&self) -> u32 { 48_000 }
    fn set_input(&mut self, _input: u32) {}

    fn save_state(&self) -> Vec<u8> {
        let s = (&self.ee, &self.iop, &self.hw, self.iop_owed, self.current_frame);
        crate::arcade::state::encode(&self.save_filename, STATE_VERSION, &s)
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let (mut ee, iop, mut hw, owed, frame): (R5900, R3000, Hw, i64, u64) =
            crate::arcade::state::decode(&self.save_filename, STATE_VERSION, data)?;
        hw.bios = std::mem::take(&mut self.hw.bios);
        hw.debug = self.hw.debug;
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
        self.hw = Hw::new(bios);
        self.hw.debug = debug;
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
