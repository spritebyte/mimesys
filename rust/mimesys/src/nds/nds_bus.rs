//! The DS's shared hardware and both CPUs' views of it.
//!
//! ARM9 map:                               ARM7 map:
//!   00000000 ITCM (32 KB, CP15)             00000000 BIOS (16 KB)
//!   DTCM base (16 KB, CP15, data only)      02000000 main RAM (4 MB)
//!   02000000 main RAM (4 MB)                03000000 shared WRAM (its share)
//!   03000000 shared WRAM (its share)        03800000 ARM7 WRAM (64 KB)
//!   04000000 I/O                            04000000 I/O
//!   05000000 palettes (2 KB)                06000000 VRAM banks C/D (as ARM7 memory)
//!   06000000 VRAM (bank mapping)
//!   07000000 OAM (2 KB)
//!   FFFF0000 BIOS (4 KB)
//!
//! Timing is approximate: every access costs a few cycles (the ARM9's caches are
//! assumed to hit). Clocks: ARM9 67 MHz, ARM7 and the bus 33.5 MHz.

use serde::{Deserialize, Serialize};
use crate::common::cpu::arm::arm7tdmi::{ArmBus, ACCESS_CODE};
use crate::nds::nds_cart::Cart;
use crate::nds::nds_dma::{Dma, Timing};
use crate::nds::nds_gpu2d::{self, Engine2d, View};
use crate::nds::nds_gpu3d::{Gpu3d, RenderRegs};
use crate::nds::nds_io::*;
use crate::nds::nds_rtc::Rtc;
use crate::nds::nds_sound::{Sound, SoundMem};
use crate::nds::nds_spi::Spi;
use crate::nds::nds_vram::{Region, Vram};
use crate::nds::nds_wifi::Wifi;

pub const ARM9: usize = 0;
pub const ARM7: usize = 1;
pub const SCREEN_W: usize = 256;
pub const SCREEN_H: usize = 192;
/// bus cycles per line, and where hblank starts
pub const LINE_CYCLES: u64 = 2130;
pub const HBLANK_CYCLE: u64 = 1606;
pub const LINES: u16 = 263;

#[derive(Clone, Serialize, Deserialize)]
pub struct Cp15 {
    pub control: u32,
    pub dtcm_setting: u32,
    pub itcm_setting: u32,
    pub dtcm_base: u32,
    pub dtcm_size: u32,
    pub itcm_size: u32,
    /// other registers, stored as written (cache/MPU setup)
    pub other: Vec<(u32, u32)>,
}

impl Default for Cp15 {
    fn default() -> Self {
        Self { control: 0x2078, dtcm_setting: 0, itcm_setting: 0, dtcm_base: 0xFFFF_FFFF, dtcm_size: 0, itcm_size: 0, other: Vec::new() }
    }
}

impl Cp15 {
    fn itcm_on(&self) -> bool { self.control & (1 << 18) != 0 }
    fn dtcm_on(&self) -> bool { self.control & (1 << 16) != 0 }

    fn set_tcm(&mut self) {
        let size = |v: u32| (512u32 << ((v >> 1) & 0x1F)).max(4096);
        self.dtcm_base = self.dtcm_setting & 0xFFFF_F000;
        self.dtcm_size = size(self.dtcm_setting);
        self.itcm_size = size(self.itcm_setting);
    }

    /// Returns true when the write asks the CPU to wait for an interrupt.
    pub fn write(&mut self, reg: u32, v: u32) -> bool {
        match reg {
            0x0100 => self.control = (self.control & !0x000F_F085) | (v & 0x000F_F085),
            0x0910 => { self.dtcm_setting = v; self.set_tcm(); }
            0x0911 => { self.itcm_setting = v; self.set_tcm(); }
            0x0704 | 0x0782 => return true,
            r if r >> 8 == 7 => {}                               // cache maintenance
            r => match self.other.iter_mut().find(|(k, _)| *k == r) {
                Some(e) => e.1 = v,
                None => self.other.push((r, v)),
            },
        }
        false
    }

    pub fn read(&self, reg: u32) -> u32 {
        match reg {
            0x0000 => 0x4105_9461,
            0x0001 => 0x0F0D_2112,
            0x0002 => 0x0014_0180,
            0x0100 => self.control,
            0x0910 => self.dtcm_setting,
            0x0911 => self.itcm_setting,
            r => self.other.iter().find(|(k, _)| *k == r).map(|e| e.1).unwrap_or(0),
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Nds {
    #[serde(with = "serde_bytes")]
    pub main_ram: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub swram: Vec<u8>,
    pub wramcnt: u8,
    #[serde(with = "serde_bytes")]
    pub wram7: Vec<u8>,
    pub vram: Vram,
    #[serde(with = "serde_bytes")]
    pub palette: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub oam: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub itcm: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub dtcm: Vec<u8>,
    pub cp15: Cp15,
    #[serde(skip)]
    pub bios9: Vec<u8>,
    #[serde(skip)]
    pub bios7: Vec<u8>,
    /// I/O registers kept as written (display engines, 3D, sound until they exist)
    #[serde(with = "serde_bytes")]
    pub io9_raw: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub io7_raw: Vec<u8>,
    pub irq: [Irq; 2],
    pub ipc: Ipc,
    pub timers: [Timers; 2],
    pub dma: [Dma; 2],
    pub math: Math,
    pub cart: Cart,
    pub spi: Spi,
    pub rtc: Rtc,
    pub dispstat: [u16; 2],
    pub vcount: u16,
    pub keyinput: u16,
    pub extkeyin: u16,
    pub powcnt1: u16,
    pub powcnt2: u16,
    pub postflg: [u8; 2],
    pub exmemcnt: u16,
    pub halted: [bool; 2],
    /// absolute time: ARM9 cycles and ARM7 (= bus) cycles
    pub cycles9: u64,
    pub cycles7: u64,
    /// 2D engines A and B
    #[serde(default)]
    pub gpu: [Engine2d; 2],
    /// display capture running this frame (DISPCAPCNT bit 31 seen at line 0)
    #[serde(default)]
    pub capturing: bool,
    pub gpu3d: Gpu3d,
    #[serde(default)]
    pub wifi: Wifi,
    #[serde(default)]
    pub sound: Sound,
    #[serde(skip, default = "blank_frame")]
    pub framebuffer: Vec<u8>,
}

fn blank_frame() -> Vec<u8> { vec![0; SCREEN_W * SCREEN_H * 2 * 4] }

#[inline(always)]
fn rd16(m: &[u8], a: usize) -> u16 { u16::from_le_bytes(m[a..a + 2].try_into().unwrap()) }
#[inline(always)]
fn rd32(m: &[u8], a: usize) -> u32 { u32::from_le_bytes(m[a..a + 4].try_into().unwrap()) }
fn wr16(m: &mut [u8], a: usize, v: u16) { m[a..a + 2].copy_from_slice(&v.to_le_bytes()); }
fn wr32(m: &mut [u8], a: usize, v: u32) { m[a..a + 4].copy_from_slice(&v.to_le_bytes()); }

/// Where an access lands.
enum Loc { Main(usize), Swram(usize), Wram7(usize), Itcm(usize), Dtcm(usize), Palette(usize), Oam(usize),
           Vram(Region, u32), Bios9(usize), Bios7(usize), Io, None }

impl Nds {
    pub fn new(cart: Cart, spi: Spi, bios9: Vec<u8>, bios7: Vec<u8>) -> Self {
        // the wifi chip's model and RF type, from the firmware header
        let wifi = Wifi::new(spi.firmware.get(0x40).copied().unwrap_or(3), spi.firmware.get(0x1D).copied().unwrap_or(0x20));
        Self {
            main_ram: vec![0; 0x40_0000], swram: vec![0; 0x8000], wramcnt: 0, wram7: vec![0; 0x1_0000],
            vram: Vram::new(), palette: vec![0; 0x800], oam: vec![0; 0x800],
            itcm: vec![0; 0x8000], dtcm: vec![0; 0x4000], cp15: Cp15::default(), bios9, bios7,
            io9_raw: vec![0; 0x2000], io7_raw: vec![0; 0x2000],
            irq: Default::default(), ipc: Ipc::default(), timers: Default::default(),
            dma: [Dma::new(true), Dma::new(false)], math: Math::default(), cart, spi, rtc: Rtc::new(),
            dispstat: [0; 2], vcount: 0, keyinput: 0x3FF, extkeyin: 0x7F, powcnt1: 0, powcnt2: 0,
            postflg: [0; 2], exmemcnt: 0, halted: [false; 2], cycles9: 0, cycles7: 0,
            gpu: Default::default(), capturing: false, gpu3d: Gpu3d::new(),
            wifi, sound: Sound::new(),
            framebuffer: blank_frame(),
        }
    }

    #[inline(always)]
    fn locate(&self, cpu: usize, a: u32, code: bool, dma: bool) -> Loc {
        if cpu == ARM9 && !dma {
            if self.cp15.itcm_on() && a < self.cp15.itcm_size { return Loc::Itcm((a & 0x7FFF) as usize); }
            if !code && self.cp15.dtcm_on() && a.wrapping_sub(self.cp15.dtcm_base) < self.cp15.dtcm_size {
                return Loc::Dtcm((a.wrapping_sub(self.cp15.dtcm_base) & 0x3FFF) as usize);
            }
        }
        match a >> 24 {
            0x00 if cpu == ARM7 && a < 0x4000 => Loc::Bios7(a as usize),
            0x02 => Loc::Main((a & 0x3F_FFFF) as usize),
            0x03 => {
                if cpu == ARM7 && a >= 0x0380_0000 { return Loc::Wram7((a & 0xFFFF) as usize); }
                let (lo, hi) = (a & 0x3FFF, a & 0x7FFF);
                match (cpu, self.wramcnt & 3) {
                    (ARM9, 0) => Loc::Swram(hi as usize),
                    (ARM9, 1) => Loc::Swram(0x4000 + lo as usize),
                    (ARM9, 2) => Loc::Swram(lo as usize),
                    (ARM9, _) => Loc::None,
                    (_, 0) => Loc::Wram7((a & 0xFFFF) as usize),
                    (_, 1) => Loc::Swram(lo as usize),
                    (_, 2) => Loc::Swram(0x4000 + lo as usize),
                    _ => Loc::Swram(hi as usize),
                }
            }
            0x04 => Loc::Io,
            0x05 if cpu == ARM9 => Loc::Palette((a & 0x7FF) as usize),
            0x06 if cpu == ARM9 => { let (r, o) = Vram::arm9_region(a); Loc::Vram(r, o) }
            0x06 => Loc::Vram(Region::Arm7, a & 0x3FFFF),
            0x07 if cpu == ARM9 => Loc::Oam((a & 0x7FF) as usize),
            0xFF if cpu == ARM9 && a >= 0xFFFF_0000 => Loc::Bios9((a & 0xFFF) as usize),
            _ => Loc::None,
        }
    }

    fn cost(cpu: usize, loc: &Loc, wide: bool) -> u64 {
        match (cpu, loc) {
            (ARM9, Loc::Itcm(_) | Loc::Dtcm(_) | Loc::Bios9(_)) => 1,
            (ARM9, Loc::Main(_)) => 2,
            (ARM9, _) => if wide { 4 } else { 2 },
            (_, Loc::Main(_)) => if wide { 3 } else { 2 },
            _ => 1,
        }
    }

    fn charge(&mut self, cpu: usize, c: u64) {
        if cpu == ARM9 { self.cycles9 += c } else { self.cycles7 += c }
    }

    #[inline]
    pub fn read32(&mut self, cpu: usize, a: u32, code: bool, dma: bool) -> u32 {
        let a = a & !3;
        // fast paths: the memories code runs from
        if cpu == ARM7 {
            if a >> 23 == 0x07 { self.cycles7 += 1; return rd32(&self.wram7, (a & 0xFFFF) as usize); }
            if a >> 23 == 0x06 && self.wramcnt == 3 { self.cycles7 += 1; return rd32(&self.swram, (a & 0x7FFF) as usize); }
            if a >> 24 == 0x02 { self.cycles7 += 3; return rd32(&self.main_ram, (a & 0x3F_FFFF) as usize); }
        } else if !dma {
            if a < self.cp15.itcm_size && self.cp15.itcm_on() { self.cycles9 += 1; return rd32(&self.itcm, (a & 0x7FFF) as usize); }
            if a >> 24 == 0x02 && (code || a.wrapping_sub(self.cp15.dtcm_base) >= self.cp15.dtcm_size || !self.cp15.dtcm_on()) {
                self.cycles9 += 2;
                return rd32(&self.main_ram, (a & 0x3F_FFFF) as usize);
            }
        }
        self.read32_slow(cpu, a, code, dma)
    }

    #[inline(never)]
    fn read32_slow(&mut self, cpu: usize, a: u32, code: bool, dma: bool) -> u32 {
        let loc = self.locate(cpu, a, code, dma);
        self.charge(cpu, Self::cost(cpu, &loc, true));
        match loc {
            Loc::Main(i) => rd32(&self.main_ram, i),
            Loc::Swram(i) => rd32(&self.swram, i),
            Loc::Wram7(i) => rd32(&self.wram7, i),
            Loc::Itcm(i) => rd32(&self.itcm, i),
            Loc::Dtcm(i) => rd32(&self.dtcm, i),
            Loc::Palette(i) => rd32(&self.palette, i),
            Loc::Oam(i) => rd32(&self.oam, i),
            Loc::Vram(r, o) => self.vram.read32(r, o),
            Loc::Bios9(i) => self.bios9.get(i..i + 4).map(|b| rd32(b, 0)).unwrap_or(0),
            Loc::Bios7(i) => self.bios7.get(i..i + 4).map(|b| rd32(b, 0)).unwrap_or(0),
            Loc::Io => self.io_read(cpu, a),
            Loc::None => 0,
        }
    }

    /// A word of memory as `cpu` sees it, with no side effects or timing (debug output).
    pub fn peek32(&self, cpu: usize, a: u32) -> u32 {
        let a = a & !3;
        match self.locate(cpu, a, true, false) {
            Loc::Main(i) => rd32(&self.main_ram, i),
            Loc::Swram(i) => rd32(&self.swram, i),
            Loc::Wram7(i) => rd32(&self.wram7, i),
            Loc::Itcm(i) => rd32(&self.itcm, i),
            Loc::Dtcm(i) => rd32(&self.dtcm, i),
            Loc::Vram(r, o) => self.vram.read32(r, o),
            Loc::Bios9(i) => self.bios9.get(i..i + 4).map(|b| rd32(b, 0)).unwrap_or(0),
            Loc::Bios7(i) => self.bios7.get(i..i + 4).map(|b| rd32(b, 0)).unwrap_or(0),
            _ => 0,
        }
    }

    pub fn read16(&mut self, cpu: usize, a: u32, code: bool, dma: bool) -> u16 {
        let a = a & !1;
        let loc = self.locate(cpu, a, code, dma);
        self.charge(cpu, Self::cost(cpu, &loc, false));
        match loc {
            Loc::Main(i) => rd16(&self.main_ram, i),
            Loc::Swram(i) => rd16(&self.swram, i),
            Loc::Wram7(i) => rd16(&self.wram7, i),
            Loc::Itcm(i) => rd16(&self.itcm, i),
            Loc::Dtcm(i) => rd16(&self.dtcm, i),
            Loc::Palette(i) => rd16(&self.palette, i),
            Loc::Oam(i) => rd16(&self.oam, i),
            Loc::Vram(r, o) => self.vram.read16(r, o),
            Loc::Bios9(i) => self.bios9.get(i..i + 2).map(|b| rd16(b, 0)).unwrap_or(0),
            Loc::Bios7(i) => self.bios7.get(i..i + 2).map(|b| rd16(b, 0)).unwrap_or(0),
            Loc::Io => (self.io_read(cpu, a & !3) >> ((a & 2) * 8)) as u16,
            Loc::None => 0,
        }
    }

    pub fn read8(&mut self, cpu: usize, a: u32) -> u8 {
        let loc = self.locate(cpu, a, false, false);
        self.charge(cpu, Self::cost(cpu, &loc, false));
        match loc {
            Loc::Main(i) => self.main_ram[i],
            Loc::Swram(i) => self.swram[i],
            Loc::Wram7(i) => self.wram7[i],
            Loc::Itcm(i) => self.itcm[i],
            Loc::Dtcm(i) => self.dtcm[i],
            Loc::Palette(i) => self.palette[i],
            Loc::Oam(i) => self.oam[i],
            Loc::Vram(r, o) => self.vram.read8(r, o),
            Loc::Bios9(i) => self.bios9.get(i).copied().unwrap_or(0),
            Loc::Bios7(i) => self.bios7.get(i).copied().unwrap_or(0),
            Loc::Io => (self.io_read(cpu, a & !3) >> ((a & 3) * 8)) as u8,
            Loc::None => 0,
        }
    }

    pub fn write32(&mut self, cpu: usize, a: u32, v: u32, dma: bool) {
        let a = a & !3;
        let loc = self.locate(cpu, a, false, dma);
        self.charge(cpu, Self::cost(cpu, &loc, true));
        match loc {
            Loc::Main(i) => wr32(&mut self.main_ram, i, v),
            Loc::Swram(i) => wr32(&mut self.swram, i, v),
            Loc::Wram7(i) => wr32(&mut self.wram7, i, v),
            Loc::Itcm(i) => wr32(&mut self.itcm, i, v),
            Loc::Dtcm(i) => wr32(&mut self.dtcm, i, v),
            Loc::Palette(i) => wr32(&mut self.palette, i, v),
            Loc::Oam(i) => wr32(&mut self.oam, i, v),
            Loc::Vram(r, o) => self.vram.write32(r, o, v),
            Loc::Io => self.io_write(cpu, a, v, 0xFFFF_FFFF),
            _ => {}
        }
    }

    pub fn write16(&mut self, cpu: usize, a: u32, v: u16, dma: bool) {
        let a = a & !1;
        let loc = self.locate(cpu, a, false, dma);
        self.charge(cpu, Self::cost(cpu, &loc, false));
        match loc {
            Loc::Main(i) => wr16(&mut self.main_ram, i, v),
            Loc::Swram(i) => wr16(&mut self.swram, i, v),
            Loc::Wram7(i) => wr16(&mut self.wram7, i, v),
            Loc::Itcm(i) => wr16(&mut self.itcm, i, v),
            Loc::Dtcm(i) => wr16(&mut self.dtcm, i, v),
            Loc::Palette(i) => wr16(&mut self.palette, i, v),
            Loc::Oam(i) => wr16(&mut self.oam, i, v),
            Loc::Vram(r, o) => self.vram.write16(r, o, v),
            Loc::Io => { let sh = (a & 2) * 8; self.io_write(cpu, a & !3, (v as u32) << sh, 0xFFFF << sh) }
            _ => {}
        }
    }

    pub fn write8(&mut self, cpu: usize, a: u32, v: u8) {
        let loc = self.locate(cpu, a, false, false);
        self.charge(cpu, Self::cost(cpu, &loc, false));
        match loc {
            Loc::Main(i) => self.main_ram[i] = v,
            Loc::Swram(i) => self.swram[i] = v,
            Loc::Wram7(i) => self.wram7[i] = v,
            Loc::Itcm(i) => self.itcm[i] = v,
            Loc::Dtcm(i) => self.dtcm[i] = v,
            Loc::Vram(Region::Arm7, o) => self.vram.write8(Region::Arm7, o, v),
            // the ARM9's palette, VRAM and OAM ignore byte writes
            Loc::Io => { let sh = (a & 3) * 8; self.io_write(cpu, a & !3, (v as u32) << sh, 0xFF << sh) }
            _ => {}
        }
    }

    // ------------------------------------------------------------ I/O

    fn raw(&mut self, cpu: usize) -> &mut Vec<u8> { if cpu == ARM9 { &mut self.io9_raw } else { &mut self.io7_raw } }

    /// Timers are brought up to date before they're touched.
    pub fn update_timers(&mut self, cpu: usize) {
        let now = if cpu == ARM9 { self.cycles9 / 2 } else { self.cycles7 };
        let irq = self.timers[cpu].update(now);
        self.irq[cpu].raise(irq);
    }

    #[inline(never)]
    pub fn io_read(&mut self, cpu: usize, a: u32) -> u32 {
        let off = a & 0x00FF_FFFF;
        match off {
            0x004 => self.dispstat[cpu] as u32 | (self.vcount as u32) << 16,
            0x0B0..=0x0DF => self.dma[cpu].read(off - 0xB0),
            0x0E0..=0x0EF if cpu == ARM9 => self.dma[ARM9].fill[((off - 0xE0) / 4) as usize],
            0x100..=0x10F => { self.update_timers(cpu); self.timers[cpu].read(((off - 0x100) / 4) as usize) }
            0x130 => { let keycnt = rd16(if cpu == ARM9 { &self.io9_raw } else { &self.io7_raw }, 0x132); self.keyinput as u32 | (keycnt as u32) << 16 }
            0x134 if cpu == ARM7 => 0x8000 | (self.extkeyin as u32) << 16,
            0x138 if cpu == ARM7 => self.rtc.io as u32,
            0x180 => self.ipc.read_sync(cpu),
            0x184 => self.ipc.read_cnt(cpu),
            0x1A0 => self.cart.auxspicnt as u32 | (self.cart.auxspidata as u32) << 16,
            0x1A4 => self.cart.romctrl,
            0x1A8 => u32::from_le_bytes([self.cart.cmd[0], self.cart.cmd[1], self.cart.cmd[2], self.cart.cmd[3]]),
            0x1AC => u32::from_le_bytes([self.cart.cmd[4], self.cart.cmd[5], self.cart.cmd[6], self.cart.cmd[7]]),
            0x1C0 if cpu == ARM7 => self.spi.cnt as u32 | (self.spi.data as u32) << 16,
            0x204 => self.exmemcnt as u32,
            0x208 => self.irq[cpu].ime,
            0x210 => self.irq[cpu].ie,
            0x214 => self.irq[cpu].iflags,
            0x240 if cpu == ARM9 => u32::from_le_bytes([self.vram.cnt[0], self.vram.cnt[1], self.vram.cnt[2], self.vram.cnt[3]]),
            0x244 if cpu == ARM9 => u32::from_le_bytes([self.vram.cnt[4], self.vram.cnt[5], self.vram.cnt[6], self.wramcnt]),
            0x248 if cpu == ARM9 => u32::from_le_bytes([self.vram.cnt[7], self.vram.cnt[8], 0, 0]),
            0x240 => self.vram.arm7_stat() as u32 | (self.wramcnt as u32) << 8,
            0x280 if cpu == ARM9 => self.math.divcnt,
            0x290 if cpu == ARM9 => self.math.numer as u32,
            0x294 if cpu == ARM9 => (self.math.numer >> 32) as u32,
            0x298 if cpu == ARM9 => self.math.denom as u32,
            0x29C if cpu == ARM9 => (self.math.denom >> 32) as u32,
            0x2A0 if cpu == ARM9 => self.math.quot as u32,
            0x2A4 if cpu == ARM9 => (self.math.quot >> 32) as u32,
            0x2A8 if cpu == ARM9 => self.math.rem as u32,
            0x2AC if cpu == ARM9 => (self.math.rem >> 32) as u32,
            0x2B0 if cpu == ARM9 => self.math.sqrtcnt,
            0x2B4 if cpu == ARM9 => self.math.sqrt_result,
            0x2B8 if cpu == ARM9 => self.math.sqrt_param as u32,
            0x2BC if cpu == ARM9 => (self.math.sqrt_param >> 32) as u32,
            0x600 if cpu == ARM9 => self.gpu3d.gxstat(rd32(&self.io9_raw, 0x600) >> 30),
            0x604..=0x6A3 if cpu == ARM9 => self.gpu3d.read(off),
            0x400..=0x51F if cpu == ARM7 => { self.sound_sync(); self.sound.read(off) }
            0x80_0000..=0x80_FFFF if cpu == ARM7 => {
                let now = self.cycles7;
                let v = self.wifi.read16(a, now) as u32 | (self.wifi.read16(a + 2, now) as u32) << 16;
                self.wifi_irq();
                v
            }
            0x300 => self.postflg[cpu] as u32,
            0x304 => if cpu == ARM9 { self.powcnt1 as u32 } else { self.powcnt2 as u32 },
            0x10_0000 => {
                let (v, mine, theirs) = self.ipc.recv(cpu);
                self.irq[cpu].raise(mine);
                self.irq[cpu ^ 1].raise(theirs);
                v
            }
            0x10_0010 => {
                let (v, done) = self.cart.read_data();
                if done && self.cart.auxspicnt & 0x4000 != 0 { self.irq[cpu].raise(IRQ_CART_DONE); }
                v
            }
            o if o < 0x2000 => { let r = self.raw(cpu); rd32(r, o as usize) }
            _ => 0,
        }
    }

    #[inline(never)]
    pub fn io_write(&mut self, cpu: usize, a: u32, v: u32, mask: u32) {
        let off = a & 0x00FF_FFFF;
        let merge = |old: u32| (old & !mask) | (v & mask);
        match off {
            0x004 => {
                if mask & 0xFFFF != 0 {
                    let old = self.dispstat[cpu] as u32;
                    self.dispstat[cpu] = ((old & 0x0007) | (merge(old) & 0xFFB8)) as u16;
                }
            }
            0x0B0..=0x0DF => {
                if let Some(ch) = self.dma[cpu].write(off - 0xB0, v, mask) {
                    // geometry FIFO DMA runs at once: commands past a SWAP_BUFFERS queue up until vblank
                    if matches!(self.dma[cpu].timing(ch), Timing::Now | Timing::GeometryFifo) { self.run_dma(cpu, ch); }
                    else if self.dma[cpu].timing(ch) == Timing::Cart && self.cart.data_ready() { self.cart_dma(cpu); }
                }
            }
            0x0E0..=0x0EF if cpu == ARM9 => { let i = ((off - 0xE0) / 4) as usize; self.dma[ARM9].fill[i] = merge(self.dma[ARM9].fill[i]); }
            0x100..=0x10F => { self.update_timers(cpu); self.timers[cpu].write(((off - 0x100) / 4) as usize, v, mask); }
            0x138 if cpu == ARM7 => self.rtc.write(v as u16, mask as u16),
            0x180 => { let irq = self.ipc.write_sync(cpu, v, mask); self.irq[cpu ^ 1].raise(irq); }
            0x184 => { let irq = self.ipc.write_cnt(cpu, v, mask); self.irq[cpu].raise(irq); }
            0x188 => { let (mine, theirs) = self.ipc.send(cpu, v); self.irq[cpu].raise(mine); self.irq[cpu ^ 1].raise(theirs); }
            0x1A0 => {
                if mask & 0xFFFF != 0 { self.cart.write_auxspicnt(v as u16, mask as u16); }
                if mask & 0x00FF_0000 != 0 { self.cart.write_auxspidata((v >> 16) as u8); }
            }
            0x1A4 => {
                if self.cart.write_romctrl(v, mask) {
                    self.cart_dma(cpu);
                    if !self.cart.data_ready() && self.cart.auxspicnt & 0x4000 != 0 && self.cart.romctrl & 0x8000_0000 == 0 {
                        self.irq[cpu].raise(IRQ_CART_DONE);
                    }
                }
            }
            0x1A8 | 0x1AC => {
                let base = ((off - 0x1A8) * 1) as usize;
                for i in 0..4 { if mask >> (i * 8) & 0xFF != 0 { self.cart.cmd[base + i] = (v >> (i * 8)) as u8; } }
            }
            0x1C0 if cpu == ARM7 => {
                if mask & 0xFFFF != 0 { self.spi.write_cnt(v as u16, mask as u16); }
                if mask & 0x00FF_0000 != 0 && self.spi.write_data((v >> 16) as u8) { self.irq[ARM7].raise(IRQ_SPI); }
            }
            0x204 => if cpu == ARM9 { self.exmemcnt = merge(self.exmemcnt as u32) as u16 },
            0x208 => self.irq[cpu].ime = merge(self.irq[cpu].ime) & 1,
            0x210 => self.irq[cpu].ie = merge(self.irq[cpu].ie),
            0x214 => self.irq[cpu].iflags &= !(v & mask),
            0x240 | 0x244 | 0x248 if cpu == ARM9 => {
                for i in 0..4 {
                    if mask >> (i * 8) & 0xFF == 0 { continue; }
                    let b = (v >> (i * 8)) as u8;
                    match off - 0x240 + i {
                        n @ 0..=6 => self.vram.set_cnt(n as usize, b),
                        7 => self.wramcnt = b & 3,
                        8 => self.vram.set_cnt(7, b),
                        9 => self.vram.set_cnt(8, b),
                        _ => {}
                    }
                }
            }
            0x280 if cpu == ARM9 => { self.math.divcnt = merge(self.math.divcnt) & 3; self.math.divide(); }
            0x290 | 0x294 if cpu == ARM9 => { let sh = (off - 0x290) * 8; self.math.numer = merge64(self.math.numer, v, mask, sh); self.math.divide(); }
            0x298 | 0x29C if cpu == ARM9 => { let sh = (off - 0x298) * 8; self.math.denom = merge64(self.math.denom, v, mask, sh); self.math.divide(); }
            0x2B0 if cpu == ARM9 => { self.math.sqrtcnt = merge(self.math.sqrtcnt) & 1; self.math.sqrt(); }
            0x2B8 | 0x2BC if cpu == ARM9 => { let sh = (off - 0x2B8) * 8; self.math.sqrt_param = merge64(self.math.sqrt_param, v, mask, sh); self.math.sqrt(); }
            0x400..=0x5FF if cpu == ARM9 => self.gpu3d.write(off, v),
            0x400..=0x51F if cpu == ARM7 => {
                self.sound_sync();
                let mut snd = std::mem::take(&mut self.sound);
                snd.write(off, v, mask, &Mem7(self));
                self.sound = snd;
            }
            0x80_0000..=0x80_FFFF if cpu == ARM7 => {
                let now = self.cycles7;
                for half in 0..2u32 {
                    let m = (mask >> (half * 16)) as u16;
                    if m == 0 { continue; }
                    let addr = a + half * 2;
                    let nv = (v >> (half * 16)) as u16;
                    let nv = if m == 0xFFFF { nv } else { (self.wifi.read16(addr, now) & !m) | (nv & m) };
                    self.wifi.write16(addr, nv, now);
                }
                self.wifi_irq();
            }
            0x600 if cpu == ARM9 => {
                if v & mask & 0x8000 != 0 { self.gpu3d.ack_error(); }
                let old = rd32(&self.io9_raw, 0x600);
                wr32(&mut self.io9_raw, 0x600, (old & !mask) | (v & mask & 0xC000_0000));
            }
            0x300 => {
                if mask & 0xFF != 0 { self.postflg[cpu] |= (v as u8) & 1; }
                if cpu == ARM7 && mask & 0xFF00 != 0 && (v >> 14) & 3 == 2 { self.halted[ARM7] = true; }
            }
            0x304 => if cpu == ARM9 { self.powcnt1 = merge(self.powcnt1 as u32) as u16 } else {
                self.powcnt2 = merge(self.powcnt2 as u32) as u16;
                self.wifi.tick(self.cycles7);
                self.wifi.enabled = self.powcnt2 & 2 != 0;
            },
            o if o < 0x2000 => {
                let r = self.raw(cpu);
                let old = rd32(r, o as usize);
                wr32(r, o as usize, (old & !mask) | (v & mask));
                // writing BG2/BG3 X/Y reloads that engine's internal reference point
                if cpu == ARM9 && (o == 0x60 || (0x320..0x3C0).contains(&o)) { self.gpu3d.dirty = true; }
                if cpu == ARM9 && o & 0xE000 == 0 && matches!(o & 0xFFF, 0x28..=0x2F | 0x38..=0x3F) {
                    let e = (o >> 12) as usize;
                    let base = e * 0x1000;
                    self.gpu[e].reload(&self.io9_raw[base..base + 0x70]);
                }
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------ DMA

    pub fn run_dma(&mut self, cpu: usize, i: usize) {
        let ch = self.dma[cpu].ch[i].clone();
        let wide = ch.cnt & (1 << 26) != 0;
        let unit: u32 = if wide { 4 } else { 2 };
        let step = |ctrl: u32| -> i64 { match ctrl { 0 | 3 => unit as i64, 1 => -(unit as i64), _ => 0 } };
        let (ds, ss) = (step((ch.cnt >> 21) & 3), step((ch.cnt >> 23) & 3));
        let (mut src, mut dst) = (ch.src, ch.dst);
        for _ in 0..ch.count {
            if wide {
                let v = self.read32(cpu, src, false, true);
                self.write32(cpu, dst, v, true);
            } else {
                let v = self.read16(cpu, src, false, true);
                self.write16(cpu, dst, v, true);
            }
            src = (src as i64 + ss) as u32;
            dst = (dst as i64 + ds) as u32;
        }
        let timing = self.dma[cpu].timing(i);
        let reload = self.dma[cpu].word_count(i);
        let c = &mut self.dma[cpu].ch[i];
        c.src = src;
        c.dst = if (c.cnt >> 21) & 3 == 3 { c.dad } else { dst };
        if c.cnt & (1 << 25) != 0 && !matches!(timing, Timing::Now | Timing::GeometryFifo) {
            c.count = reload;
        } else {
            self.dma[cpu].ch[i].cnt &= !0x8000_0000;
        }
        if self.dma[cpu].ch[i].cnt & (1 << 30) != 0 { self.irq[cpu].raise(IRQ_DMA0 << i); }
    }

    /// Start every enabled channel waiting for `timing`.
    pub fn trigger_dma(&mut self, cpu: usize, timing: Timing) {
        for i in 0..4 {
            if self.dma[cpu].ch[i].cnt & 0x8000_0000 != 0 && self.dma[cpu].timing(i) == timing { self.run_dma(cpu, i); }
        }
    }

    /// Card data is ready: let a DS-card DMA channel move it.
    fn cart_dma(&mut self, cpu: usize) {
        let mut guard = 0;
        while self.cart.data_ready() && guard < 0x10000 {
            guard += 1;
            let ch = (0..4).find(|&i| self.dma[cpu].ch[i].cnt & 0x8000_0000 != 0 && self.dma[cpu].timing(i) == Timing::Cart);
            match ch { Some(i) => self.run_dma(cpu, i), None => break }
        }
    }

    // ------------------------------------------------------------ display timing

    /// Run the sound unit up to the ARM7's time.
    pub fn sound_sync(&mut self) {
        let now = self.cycles7;
        if now < self.sound.time + crate::nds::nds_sound::MIX_CYCLES { return; }
        let mut snd = std::mem::take(&mut self.sound);
        snd.run(now, &mut Mem7(self));
        self.sound = snd;
    }

    /// A byte of ARM7 memory written with no timing (sound capture).
    fn poke8(&mut self, a: u32, v: u8) {
        match self.locate(ARM7, a, false, true) {
            Loc::Main(i) => self.main_ram[i] = v,
            Loc::Swram(i) => self.swram[i] = v,
            Loc::Wram7(i) => self.wram7[i] = v,
            Loc::Vram(r, o) => self.vram.write8(r, o, v),
            _ => {}
        }
    }

    /// Pass on a wifi interrupt to the ARM7.
    fn wifi_irq(&mut self) {
        if self.wifi.irq { self.wifi.irq = false; self.irq[ARM7].raise(IRQ_WIFI); }
    }

    pub fn start_line(&mut self, line: u16) {
        self.vcount = line;
        // the wifi chip's clock (power-on delay, microsecond counter) runs on its own
        self.wifi.tick(self.cycles7);
        self.wifi_irq();
        if line == 0 {
            for e in 0..2 { let b = e * 0x1000; self.gpu[e].reload(&self.io9_raw[b..b + 0x70]); }
            self.capturing = rd32(&self.io9_raw, 0x64) & 0x8000_0000 != 0;
        }
        for cpu in 0..2 {
            let ds = &mut self.dispstat[cpu];
            *ds &= !0x0002;                                       // hblank over
            if line == 192 { *ds |= 0x0001; }
            if line == LINES - 1 { *ds &= !0x0001; }
            let target = (*ds >> 8) | ((*ds & 0x80) << 1);
            if line == target {
                *ds |= 0x0004;
                if *ds & 0x0020 != 0 { self.irq[cpu].raise(IRQ_VCOUNT); }
            } else {
                *ds &= !0x0004;
            }
            if line == 192 && self.dispstat[cpu] & 0x0008 != 0 { self.irq[cpu].raise(IRQ_VBLANK); }
        }
        // the geometry FIFO's IRQ is level-triggered: 1 = less than half full, 2 = empty
        let n = self.gpu3d.fifo_len();
        match rd32(&self.io9_raw, 0x600) >> 30 {
            1 if n < 128 => self.irq[ARM9].raise(IRQ_GXFIFO),
            2 if n == 0 => self.irq[ARM9].raise(IRQ_GXFIFO),
            _ => {}
        }
        if line == 192 {
            // new polygon lists, then the frame the next screen shows
            let swapped = self.gpu3d.vblank();
            if self.powcnt1 & 0x0008 == 0 {
                self.gpu3d.clear_frame();
            } else if swapped || self.gpu3d.dirty {
                self.gpu3d.render(RenderRegs(&self.io9_raw), &self.vram);
            }
            self.trigger_dma(ARM9, Timing::VBlank);
            self.trigger_dma(ARM7, Timing::VBlank);
        }
    }

    pub fn hblank(&mut self, line: u16) {
        for cpu in 0..2 {
            self.dispstat[cpu] |= 0x0002;
            if self.dispstat[cpu] & 0x0010 != 0 { self.irq[cpu].raise(IRQ_HBLANK); }
        }
        if line < 192 { self.trigger_dma(ARM9, Timing::HBlank); }
    }

    /// Draw line `line` of both screens: each engine's graphics (or VRAM for engine A's
    /// display mode 2), display capture, then master brightness. POWCNT1 bit 15 puts
    /// engine A on the top screen. A display that's off is white; a powered-off engine black.
    pub fn render_line(&mut self, line: usize) {
        let a_top = self.powcnt1 & 0x8000 != 0;
        for engine in 0..2 {
            let base = engine * 0x1000;
            let dispcnt = rd32(&self.io9_raw, base);
            let powered = self.powcnt1 & if engine == 0 { 0x0002 } else { 0x0200 } != 0;
            let mode = (dispcnt >> 16) & if engine == 0 { 3 } else { 1 };
            let need_graphics = mode == 1 || (engine == 0 && self.capturing);
            let graphics = if powered && need_graphics {
                let mut view = View::new(engine, &self.io9_raw[base..base + 0x70], &self.vram,
                                         &self.palette[engine * 0x400..engine * 0x400 + 0x400],
                                         &self.oam[engine * 0x400..engine * 0x400 + 0x400],
                                         self.gpu[engine].bg_ref);
                if engine == 0 { view.threed = Some(&self.gpu3d.frame[line * SCREEN_W..(line + 1) * SCREEN_W]); }
                view.compose(line)
            } else {
                [0x7FFF; nds_gpu2d::W]
            };
            let mut out = match mode {
                _ if !powered => [0; nds_gpu2d::W],
                0 => [0x7FFF; nds_gpu2d::W],
                1 => graphics,
                2 => {
                    let bank = (dispcnt >> 18) & 3;
                    let mut l = [0u16; nds_gpu2d::W];
                    for (x, c) in l.iter_mut().enumerate() {
                        *c = self.vram.read16(Region::Lcdc, bank * 0x20000 + ((line * SCREEN_W + x) * 2) as u32) & 0x7FFF;
                    }
                    l
                }
                _ => [0; nds_gpu2d::W],              // main memory display FIFO: not yet
            };
            if engine == 0 && self.capturing { self.capture_line(line, dispcnt, &graphics); }
            if powered && mode != 0 { nds_gpu2d::master_bright(&mut out, rd16(&self.io9_raw, base + 0x6C)); }
            let screen = if (engine == 0) == a_top { 0 } else { 1 };
            let row = (screen * SCREEN_H + line) * SCREEN_W * 4;
            for (x, c) in out.iter().enumerate() {
                self.framebuffer[row + x * 4..row + x * 4 + 4].copy_from_slice(&nds_gpu2d::to_rgba(*c));
            }
            self.gpu[engine].step(&self.io9_raw[base..base + 0x70]);
        }
    }

    /// Display capture (DISPCAPCNT 0x04000064): blends source A (engine A's graphics, or
    /// the 3D layer from stage 4) and source B (a VRAM bank, or the main memory FIFO) into
    /// an LCDC bank. Bits: 0-4 EVA, 8-12 EVB, 16-17 bank, 18-19 write offset (32 KB),
    /// 20-21 size (128x128, 256x64, 256x128, 256x192), 24 A = 3D, 25 B = FIFO,
    /// 26-27 read offset, 29-30 source (A, B, blend), 31 busy.
    fn capture_line(&mut self, line: usize, dispcnt: u32, graphics: &[u16; nds_gpu2d::W]) {
        let cap = rd32(&self.io9_raw, 0x64);
        let (w, h) = [(128, 128), (256, 64), (256, 128), (256, 192)][((cap >> 20) & 3) as usize];
        if line < h {
            let eva = (cap & 31).min(16);
            let evb = ((cap >> 8) & 31).min(16);
            let wbank = (cap >> 16) & 3;
            let wofs = ((cap >> 18) & 3) * 0x8000;
            let rbank = (dispcnt >> 18) & 3;
            let rofs = ((cap >> 26) & 3) * 0x8000;
            for x in 0..w {
                // source A: engine A's graphics (opaque), or the 3D layer alone
                let a = if cap & (1 << 24) != 0 {
                    let p = self.gpu3d.frame[line * SCREEN_W + x];
                    (p & 0x7FFF) as u16 | if p >> 16 != 0 { 0x8000 } else { 0 }
                } else {
                    graphics[x] | 0x8000
                };
                let b = if cap & (1 << 25) != 0 {
                    0
                } else {
                    self.vram.read16(Region::Lcdc, rbank * 0x20000 + ((rofs + ((line * 256 + x) * 2) as u32) & 0x1FFFF))
                };
                let c = match (cap >> 29) & 3 {
                    0 => a,
                    1 => b,
                    _ => {
                        let (aa, ab) = ((a >> 15) as u32, (b >> 15) as u32);
                        let mut c = 0u16;
                        for s in [0, 5, 10] {
                            let ca = (a >> s) as u32 & 31;
                            let cb = (b >> s) as u32 & 31;
                            c |= (((ca * aa * eva + cb * ab * evb) >> 4).min(31) as u16) << s;
                        }
                        let alpha = (eva > 0 && aa != 0) || (evb > 0 && ab != 0);
                        c | if alpha { 0x8000 } else { 0 }
                    }
                };
                self.vram.write16(Region::Lcdc, wbank * 0x20000 + ((wofs + ((line * w + x) * 2) as u32) & 0x1FFFF), c);
            }
        }
        if line + 1 == h.min(SCREEN_H) {
            let v = cap & !0x8000_0000;
            wr32(&mut self.io9_raw, 0x64, v);
            self.capturing = false;
        }
    }
}

/// The ARM7's memory, as the sound unit reads and writes it.
struct Mem7<'a>(&'a mut Nds);
impl SoundMem for Mem7<'_> {
    fn read8(&self, a: u32) -> u8 { (self.0.peek32(ARM7, a) >> ((a & 3) * 8)) as u8 }
    fn write8(&mut self, a: u32, v: u8) { self.0.poke8(a, v) }
}

fn merge64(old: u64, v: u32, mask: u32, sh: u32) -> u64 {
    let m = (mask as u64) << sh;
    (old & !m) | (((v as u64) << sh) & m)
}

// ---------------------------------------------------------------- the CPUs' buses

pub struct Arm9Bus<'a>(pub &'a mut Nds);
pub struct Arm7Bus<'a>(pub &'a mut Nds);

impl ArmBus for Arm9Bus<'_> {
    fn read8(&mut self, a: u32, _k: u8) -> u8 { self.0.read8(ARM9, a) }
    fn read16(&mut self, a: u32, k: u8) -> u16 { self.0.read16(ARM9, a, k & ACCESS_CODE != 0, false) }
    fn read32(&mut self, a: u32, k: u8) -> u32 { self.0.read32(ARM9, a, k & ACCESS_CODE != 0, false) }
    fn write8(&mut self, a: u32, v: u8, _k: u8) { self.0.write8(ARM9, a, v) }
    fn write16(&mut self, a: u32, v: u16, _k: u8) { self.0.write16(ARM9, a, v, false) }
    fn write32(&mut self, a: u32, v: u32, _k: u8) { self.0.write32(ARM9, a, v, false) }
    fn idle(&mut self) { self.0.cycles9 += 1; }
    fn cp15_read(&mut self, reg: u32) -> u32 { self.0.cp15.read(reg) }
    fn cp15_write(&mut self, reg: u32, v: u32) {
        if self.0.cp15.write(reg, v) { self.0.halted[ARM9] = true; }
    }
    fn exception_base(&self) -> u32 { if self.0.cp15.control & (1 << 13) != 0 { 0xFFFF_0000 } else { 0 } }
}

impl ArmBus for Arm7Bus<'_> {
    fn read8(&mut self, a: u32, _k: u8) -> u8 { self.0.read8(ARM7, a) }
    fn read16(&mut self, a: u32, k: u8) -> u16 { self.0.read16(ARM7, a, k & ACCESS_CODE != 0, false) }
    fn read32(&mut self, a: u32, k: u8) -> u32 { self.0.read32(ARM7, a, k & ACCESS_CODE != 0, false) }
    fn write8(&mut self, a: u32, v: u8, _k: u8) { self.0.write8(ARM7, a, v) }
    fn write16(&mut self, a: u32, v: u16, _k: u8) { self.0.write16(ARM7, a, v, false) }
    fn write32(&mut self, a: u32, v: u32, _k: u8) { self.0.write32(ARM7, a, v, false) }
    fn idle(&mut self) { self.0.cycles7 += 1; }
}
