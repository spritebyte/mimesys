//! Double Dragon sound board (MAME ddragon.cpp): MC6809 @ 1.5 MHz, YM2151 @ 3.579545 MHz
//! (IRQ -> FIRQ), two MSM5205 @ 375 kHz (/48 = 7.8 kHz) playing 4-bit ADPCM straight
//! out of their own ROMs, sound latch from the main CPU (pending -> IRQ).
//!
//!   0000-0FFF RAM   1000 R latch   1800 R ADPCM idle flags   2800-2801 YM2151
//!   3800-3807 W ADPCM: offset bit 0 = chip; >>1: 0 start, 1 end, 2 start address, 3 stop
//!   8000-FFFF ROM

use serde::{Deserialize, Serialize};

use crate::common::audio::msm5205::Msm5205;
use crate::common::audio::ym2151::Ym2151;
use crate::common::cpu::m6809::M6809Bus;

pub const SOUND_CLOCK: f64 = 1_500_000.0;
const YM_RATE: f64 = 3_579_545.0 / 64.0;
const OUT_RATE: f64 = 44_100.0;
/// YM2151 busy after a data write: 64 YM clocks, in sound CPU cycles
const YM_BUSY_CYCLES: u32 = 27;

#[derive(Serialize, Deserialize, Clone, Copy)]
pub struct AdpcmChan { pos: u32, end: u32, idle: bool, data: i32 }

#[derive(Serialize, Deserialize)]
pub struct DdSound {
    #[serde(skip)] rom: Vec<u8>,
    #[serde(skip)] adpcm_rom: [Vec<u8>; 2],
    ram: Vec<u8>,
    pub latch: u8,
    pub latch_pending: bool,
    pub ym: Ym2151,
    ym_busy: u32,
    ym_phase: f64,
    ym_out: f32,
    msm: [Msm5205; 2],
    adpcm: [AdpcmChan; 2],
    msm_div: u8,
    out_phase: f64,
    acc: f32,
    acc_n: u32,
    #[serde(skip)] pub out: Vec<f32>,
}

impl DdSound {
    pub fn new(rom: Vec<u8>, adpcm1: Vec<u8>, adpcm2: Vec<u8>) -> Self {
        let mut ym = Ym2151::new();
        ym.set_running(true);
        let idle = AdpcmChan { pos: 0, end: 0, idle: true, data: -1 };
        Self {
            rom, adpcm_rom: [adpcm1, adpcm2], ram: vec![0; 0x1000],
            latch: 0, latch_pending: false,
            ym, ym_busy: 0, ym_phase: 0.0, ym_out: 0.0,
            // S48_4B: VCK = 375 kHz / 48
            msm: [Msm5205::new(5), Msm5205::new(5)],
            adpcm: [idle; 2], msm_div: 0,
            out_phase: 0.0, acc: 0.0, acc_n: 0, out: Vec::new(),
        }
    }

    pub fn keep_static(&mut self, old: &mut Self) {
        self.rom = std::mem::take(&mut old.rom);
        self.adpcm_rom = std::mem::take(&mut old.adpcm_rom);
        self.out = std::mem::take(&mut old.out);
    }

    pub fn reset(&mut self) {
        self.latch_pending = false;
        self.ym.reset();
        self.ym_busy = 0;
        self.adpcm = [AdpcmChan { pos: 0, end: 0, idle: true, data: -1 }; 2];
    }

    pub fn irq(&self) -> bool { self.latch_pending }
    pub fn firq(&self) -> bool { self.ym.status() & 0x03 != 0 }

    pub fn write_latch(&mut self, v: u8) {
        self.latch = v;
        self.latch_pending = true;
    }

    fn adpcm_w(&mut self, offset: u16, data: u8) {
        let chip = (offset & 1) as usize;
        let ch = &mut self.adpcm[chip];
        match offset >> 1 {
            3 => { ch.idle = true; self.msm[chip].set_reset(true); }
            2 => ch.pos = ((data & 0x7F) as u32) << 9,
            1 => ch.end = ((data & 0x7F) as u32) << 9,
            _ => { ch.idle = false; self.msm[chip].set_reset(false); }
        }
    }

    /// Runs the sound chips for `cycles` sound CPU cycles, making output samples.
    pub fn advance(&mut self, cycles: u32) {
        let ym_step = YM_RATE / SOUND_CLOCK;
        let out_step = OUT_RATE / SOUND_CLOCK;
        for _ in 0..cycles {
            self.ym_busy = self.ym_busy.saturating_sub(1);
            self.ym_phase += ym_step;
            if self.ym_phase >= 1.0 {
                self.ym_phase -= 1.0;
                let (l, r) = self.ym.generate();
                self.ym_out = (l + r) as f32 * (0.35 / 32768.0);
            }
            // MSM5205 master clock = 12 MHz / 32 = 375 kHz = sound CPU clock / 4
            self.msm_div += 1;
            if self.msm_div == 4 {
                self.msm_div = 0;
                for i in 0..2 {
                    let (msm, ch, rom) = (&mut self.msm[i], &mut self.adpcm[i], &self.adpcm_rom[i]);
                    msm.clock_with(|m| {
                        // MAME ddragon_adpcm_int: next nibble, or stop at the end address
                        if ch.pos >= ch.end || ch.pos as usize >= rom.len() {
                            ch.idle = true;
                            m.set_reset(true);
                        } else if ch.data != -1 {
                            m.data_w(ch.data as u8 & 0x0F);
                            ch.data = -1;
                        } else {
                            ch.data = rom[ch.pos as usize] as i32;
                            ch.pos += 1;
                            m.data_w((ch.data >> 4) as u8);
                        }
                    });
                }
            }
            // MSM full scale is +-1.0 in MAME (ours is +-0.5)
            self.acc += self.ym_out + 2.0 * (self.msm[0].output() + self.msm[1].output());
            self.acc_n += 1;
            self.out_phase += out_step;
            if self.out_phase >= 1.0 {
                self.out_phase -= 1.0;
                let s = (self.acc / self.acc_n as f32 * 0.6).clamp(-1.0, 1.0);
                self.out.push(s);
                self.out.push(s);
                self.acc = 0.0;
                self.acc_n = 0;
            }
        }
    }
}

impl M6809Bus for DdSound {
    fn read(&mut self, a: u16) -> u8 {
        match a {
            0x0000..=0x0FFF => self.ram[a as usize],
            0x1000 => { self.latch_pending = false; self.latch }
            0x1800 => (self.adpcm[0].idle as u8) | ((self.adpcm[1].idle as u8) << 1),
            0x2800..=0x2801 => {
                if a & 1 == 0 { 0xFF } else { self.ym.status() | if self.ym_busy > 0 { 0x80 } else { 0 } }
            }
            0x8000..=0xFFFF => self.rom.get(a as usize).copied().unwrap_or(0xFF),   // region is the full 64K
            _ => 0xFF,
        }
    }

    fn write(&mut self, a: u16, v: u8) {
        match a {
            0x0000..=0x0FFF => self.ram[a as usize] = v,
            0x2800..=0x2801 => {
                self.ym.write((a & 1) as u8, v);
                if a & 1 != 0 { self.ym_busy = YM_BUSY_CYCLES; }
            }
            0x3800..=0x3807 => self.adpcm_w(a & 7, v),
            _ => {}
        }
    }
}
