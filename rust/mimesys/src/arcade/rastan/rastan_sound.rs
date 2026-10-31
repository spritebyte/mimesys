//! Rastan sound board: Z80 at 4 MHz, YM2151 at 4 MHz (its CT1/CT2 port picks the Z80's
//! ROM bank at 4000-7FFF), an MSM5205 at 384 kHz / 48 (8 kHz) playing 4-bit ADPCM from
//! its own ROM, and the PC060HA link to the 68000. After MAME rastan.cpp and
//! taitosnd.cpp (Philip Bennett).
//!
//! Z80 map: 0000-3FFF ROM, 4000-7FFF ROM bank, 8000-8FFF RAM, 9000/9001 YM2151,
//! A000 / A001 PC060HA port / data, B000 ADPCM start address (high byte), C000 ADPCM
//! start, D000 ADPCM stop.

use serde::{Deserialize, Serialize};
use crate::common::audio::msm5205::Msm5205;
use crate::common::audio::ym2151::Ym2151;
use crate::common::cpu::z80::{Bus, Z80Cpu, Z80State};

pub const CLOCK: u64 = 4_000_000;
const MSM_CLOCK: u64 = 384_000;
const OUT_RATE: u64 = 44_100;
/// YM2151 busy after a data write: 64 of its clocks = 64 Z80 cycles here
const YM_BUSY: u32 = 64;

/// PC060HA (a TC0140SYT relative): two 4-bit mailboxes of two nibbles each, an NMI to
/// the sound CPU while a mailbox is full (if enabled), and the sound CPU's reset line.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Pc060ha {
    main_mode: u8,
    sub_mode: u8,
    status: u8,
    nmi_enabled: bool,
    to_sound: [u8; 4],
    to_main: [u8; 4],
    /// sound CPU held in reset
    pub reset_line: bool,
}

const PORT01_FULL: u8 = 0x01;
const PORT23_FULL: u8 = 0x02;
const PORT01_FULL_MASTER: u8 = 0x04;
const PORT23_FULL_MASTER: u8 = 0x08;

impl Pc060ha {
    pub fn nmi(&self) -> bool { self.nmi_enabled && self.status & (PORT01_FULL | PORT23_FULL) != 0 }

    pub fn master_port_w(&mut self, d: u8) { self.main_mode = d & 0x0F; }

    pub fn master_comm_w(&mut self, d: u8) {
        let d = d & 0x0F;
        match self.main_mode {
            0..=3 => {
                let m = self.main_mode as usize;
                self.to_sound[m] = d;
                if m == 1 { self.status |= PORT01_FULL; }
                if m == 3 { self.status |= PORT23_FULL; }
                self.main_mode += 1;
            }
            4 => self.reset_line = d != 0,
            _ => {}
        }
    }

    pub fn master_comm_r(&mut self) -> u8 {
        match self.main_mode {
            0..=3 => {
                let m = self.main_mode as usize;
                let v = self.to_main[m];
                if m == 1 { self.status &= !PORT01_FULL_MASTER; }
                if m == 3 { self.status &= !PORT23_FULL_MASTER; }
                self.main_mode += 1;
                v
            }
            4 => self.status,
            _ => 0,
        }
    }

    fn slave_port_w(&mut self, d: u8) { self.sub_mode = d & 0x0F; }

    fn slave_comm_w(&mut self, d: u8) {
        let d = d & 0x0F;
        match self.sub_mode {
            0..=3 => {
                let m = self.sub_mode as usize;
                self.to_main[m] = d;
                if m == 1 { self.status |= PORT01_FULL_MASTER; }
                if m == 3 { self.status |= PORT23_FULL_MASTER; }
                self.sub_mode += 1;
            }
            5 => self.nmi_enabled = false,
            6 => self.nmi_enabled = true,
            _ => {}
        }
    }

    fn slave_comm_r(&mut self) -> u8 {
        match self.sub_mode {
            0..=3 => {
                let m = self.sub_mode as usize;
                let v = self.to_sound[m];
                if m == 1 { self.status &= !PORT01_FULL; }
                if m == 3 { self.status &= !PORT23_FULL; }
                self.sub_mode += 1;
                v
            }
            4 => self.status,
            _ => 0,
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct SoundBus {
    #[serde(skip)] rom: Vec<u8>,
    #[serde(skip)] adpcm_rom: Vec<u8>,
    ram: Vec<u8>,
    bank: usize,
    pub ciu: Pc060ha,
    ym: Ym2151,
    ym_addr: u8,
    ym_busy: u32,
    msm: Msm5205,
    adpcm_pos: u16,
    adpcm_ff: bool,
}

impl SoundBus {
    #[cfg(test)]
    pub fn ram_byte(&self, a: u16) -> u8 { self.ram[(a - 0x8000) as usize] }
}

impl Bus for SoundBus {
    fn read(&mut self, a: u16) -> u8 {
        match a {
            0x0000..=0x3FFF => self.rom.get(a as usize).copied().unwrap_or(0),
            0x4000..=0x7FFF => self.rom.get(self.bank * 0x4000 + (a as usize - 0x4000)).copied().unwrap_or(0),
            0x8000..=0x8FFF => self.ram[(a - 0x8000) as usize],
            0x9000..=0x9FFF => if a & 1 == 0 { 0xFF } else { self.ym.status() | if self.ym_busy > 0 { 0x80 } else { 0 } },
            0xA000..=0xAFFF if a & 1 == 1 => self.ciu.slave_comm_r(),
            _ => 0,
        }
    }

    fn write(&mut self, a: u16, v: u8) {
        match a {
            0x8000..=0x8FFF => self.ram[(a - 0x8000) as usize] = v,
            0x9000..=0x9FFF => {
                if a & 1 == 0 {
                    self.ym_addr = v;
                } else {
                    if self.ym_addr == 0x1B { self.bank = (v >> 6) as usize & 3; }   // CT1/CT2 port
                    self.ym_busy = YM_BUSY;
                }
                self.ym.write((a & 1) as u8, v);
            }
            0xA000..=0xAFFF => if a & 1 == 0 { self.ciu.slave_port_w(v) } else { self.ciu.slave_comm_w(v) },
            0xB000..=0xBFFF => self.adpcm_pos = (self.adpcm_pos & 0x00FF) | (v as u16) << 8,
            0xC000..=0xCFFF => { self.msm.set_reset(false); self.adpcm_ff = false; }
            0xD000..=0xDFFF => { self.msm.set_reset(true); self.adpcm_pos &= 0xFF00; }
            _ => {}
        }
    }

    fn port_in(&mut self, _p: u16) -> u8 { 0xFF }
    fn port_out(&mut self, _p: u16, _v: u8) {}
}

pub struct Sound {
    cpu: Z80Cpu,
    pub bus: SoundBus,
    nmi_line: bool,
    was_reset: bool,
    owed: i64,
    phase: u64,
    ym_div: u32,
    ym_out: f32,
    msm_phase: u64,
    resample: u64,
    acc: f32,
    acc_n: u32,
    pub out: Vec<f32>,
}

#[derive(Serialize, Deserialize)]
pub struct SoundState {
    cpu: Z80State,
    bus: SoundBus,
    nmi_line: bool,
    was_reset: bool,
    owed: i64,
    phase: u64,
    ym_div: u32,
    msm_phase: u64,
}

impl Sound {
    pub fn new(rom: Vec<u8>, adpcm_rom: Vec<u8>) -> Self {
        let mut msm = Msm5205::new(5);     // S48_4B: 8 kHz, 4-bit
        msm.set_reset(true);
        Self {
            cpu: Z80Cpu::new(),
            bus: SoundBus {
                rom, adpcm_rom, ram: vec![0; 0x1000], bank: 0, ciu: Pc060ha::default(),
                ym: Ym2151::new(), ym_addr: 0, ym_busy: 0, msm, adpcm_pos: 0, adpcm_ff: false,
            },
            nmi_line: false, was_reset: false, owed: 0, phase: 0, ym_div: 0, ym_out: 0.0,
            msm_phase: 0, resample: 0, acc: 0.0, acc_n: 0, out: Vec::new(),
        }
    }

    pub fn save(&self) -> SoundState {
        let b = &self.bus;
        SoundState {
            cpu: self.cpu.save_state(),
            bus: SoundBus {
                rom: Vec::new(), adpcm_rom: Vec::new(), ram: b.ram.clone(), bank: b.bank, ciu: b.ciu.clone(),
                ym: bincode_clone(&b.ym), ym_addr: b.ym_addr, ym_busy: b.ym_busy, msm: bincode_clone(&b.msm),
                adpcm_pos: b.adpcm_pos, adpcm_ff: b.adpcm_ff,
            },
            nmi_line: self.nmi_line, was_reset: self.was_reset, owed: self.owed, phase: self.phase,
            ym_div: self.ym_div, msm_phase: self.msm_phase,
        }
    }

    pub fn load(&mut self, s: SoundState) {
        let mut bus = s.bus;
        bus.rom = std::mem::take(&mut self.bus.rom);
        bus.adpcm_rom = std::mem::take(&mut self.bus.adpcm_rom);
        self.bus = bus;
        self.cpu.load_state(&s.cpu);
        self.nmi_line = s.nmi_line;
        self.was_reset = s.was_reset;
        self.owed = s.owed;
        self.phase = s.phase;
        self.ym_div = s.ym_div;
        self.msm_phase = s.msm_phase;
    }

    /// The ROMs back out, for rebuilding the board on a reset.
    pub fn take_roms(&mut self) -> (Vec<u8>, Vec<u8>) {
        (std::mem::take(&mut self.bus.rom), std::mem::take(&mut self.bus.adpcm_rom))
    }

    /// Run `num / den` seconds of the sound board.
    pub fn run(&mut self, num: u64, den: u64) {
        self.phase += num * CLOCK;
        self.owed += (self.phase / den) as i64;
        self.phase %= den;
        while self.owed > 0 {
            let c = if self.bus.ciu.reset_line {
                self.was_reset = true;
                4
            } else {
                if self.was_reset {
                    self.was_reset = false;
                    self.cpu = Z80Cpu::new();
                }
                let nmi = self.bus.ciu.nmi();
                let mut c = 0;
                if nmi && !self.nmi_line { c = self.cpu.nmi(&mut self.bus); }
                self.nmi_line = nmi;
                if c == 0 && self.bus.ym.status() & 0x03 != 0 { c = self.cpu.irq(&mut self.bus, 0xFF); }
                if c == 0 { c = self.cpu.step(&mut self.bus); }
                c
            };
            self.owed -= c as i64;
            for _ in 0..c { self.tick(); }
        }
    }

    fn tick(&mut self) {
        let b = &mut self.bus;
        b.ym_busy = b.ym_busy.saturating_sub(1);
        self.ym_div += 1;
        if self.ym_div == 64 {
            self.ym_div = 0;
            let (l, r) = b.ym.generate();
            self.ym_out = (l + r) as f32 * (0.5 / 32768.0);
        }
        self.msm_phase += MSM_CLOCK;
        while self.msm_phase >= CLOCK {
            self.msm_phase -= CLOCK;
            let (msm, rom, pos, ff) = (&mut b.msm, &b.adpcm_rom, &mut b.adpcm_pos, &mut b.adpcm_ff);
            msm.clock_with(|m| {
                // MAME msm5205_vck: high nibble of the next byte, then its low nibble
                *ff = !*ff;
                if *ff {
                    let byte = rom.get(*pos as usize).copied().unwrap_or(0);
                    *pos = pos.wrapping_add(1);
                    m.data_w(byte >> 4);
                } else {
                    let byte = rom.get(pos.wrapping_sub(1) as usize).copied().unwrap_or(0);
                    m.data_w(byte & 0x0F);
                }
            });
        }
        // MSM full scale +-0.5 here (+-1.0 in MAME), route 0.60
        self.acc += self.ym_out + b.msm.output() * 2.0 * 0.6;
        self.acc_n += 1;
        self.resample += OUT_RATE;
        if self.resample >= CLOCK {
            self.resample -= CLOCK;
            let s = (self.acc / self.acc_n as f32).clamp(-1.0, 1.0);
            self.out.push(s);
            self.out.push(s);
            self.acc = 0.0;
            self.acc_n = 0;
        }
    }
}

/// Clone a serde value by round-tripping it (for chips without Clone).
fn bincode_clone<T: Serialize + for<'de> Deserialize<'de>>(v: &T) -> T {
    let cfg = bincode::config::standard();
    let bytes = bincode::serde::encode_to_vec(v, cfg).expect("encode");
    bincode::serde::decode_from_slice(&bytes, cfg).expect("decode").0
}
