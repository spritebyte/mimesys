//! TMNT sound board (MAME tmnt.cpp tmnt_audio_map; BSD-3-Clause).
//!
//!   Z80 @ 3.579545 MHz, YM2151, K007232 (two PCM channels, 128 KB ROM) and a uPD7759
//!   (ADPCM speech, 128 KB ROM) plus the title music, which is a 512 KB ROM read straight
//!   into a DAC at 20 kHz (MAME plays it as a sample).
//!
//!   0000-7FFF ROM   8000-87FF RAM   9000 R/W title music control / latch readback
//!   A000 R sound latch   B000-B00D K007232   C000-C001 YM2151
//!   D000 W uPD7759 data   E000 W uPD7759 /START (bit 0, inverted)   F000 R uPD7759 /BUSY
//!
//! The main CPU raises the Z80's IRQ (IM 1, held until taken) by a 1 -> 0 edge of bit 3
//! of its 0A0000 register. The K007232's external port register is the volume control:
//! channel A goes to the left output at the high nibble's volume, channel B to the right
//! at the low nibble's.

use serde::{Deserialize, Serialize};
use crate::common::audio::k007232::K007232;
use crate::common::audio::upd7759::Upd7759;
use crate::common::audio::ym2151::Ym2151;
use crate::common::cpu::z80::{Bus, Z80Cpu, Z80State};

pub const CLOCK: u64 = 3_579_545;
const OUT_RATE: u64 = 44_100;
const UPD_CLOCK: u64 = 640_000;
/// the title music ROM is read at 640 kHz / 32
const TITLE_RATE: u64 = 20_000;
/// YM2151 busy after a data write: 64 of its clocks = 64 Z80 cycles here
const YM_BUSY: u32 = 64;

/// MAME route gains into the mono output
const GAIN_YM: f32 = 0.40;
const GAIN_K007232: f32 = 0.15;
const GAIN_UPD: f32 = 0.30;
const GAIN_TITLE: f32 = 0.25;

/// ymfm::decode_fp: a 3.10 floating point sample to a signed 16-bit value
fn decode_fp(v: i16) -> i16 {
    let v = v ^ 0x1E00;
    ((v << 6) >> ((v >> 10) & 7)) as i16
}

/// The title music ROM as signed samples (MAME tmnt_decode_sample).
pub fn decode_title(rom: &[u8]) -> Vec<i16> {
    (0..rom.len() / 2)
        .map(|i| {
            let val = rom[2 * i] as i32 + rom[2 * i + 1] as i32 * 256;
            decode_fp((val >> 3) as i16)
        })
        .collect()
}

#[derive(Serialize, Deserialize)]
pub struct SoundBus {
    #[serde(skip)] rom: Vec<u8>,
    #[serde(skip)] k_rom: Vec<u8>,
    #[serde(skip)] upd_rom: Vec<u8>,
    #[serde(skip)] title: Vec<i16>,
    #[serde(with = "serde_bytes")]
    ram: Vec<u8>,
    /// sound latch written by the main CPU
    pub latch: u8,
    /// last value written to 9000
    sres: u8,
    ym: Ym2151,
    ym_busy: u32,
    k007232: K007232,
    upd: Upd7759,
    title_playing: bool,
    title_pos: u32,
    title_phase: u64,
}

impl SoundBus {
    #[cfg(test)]
    pub fn ram_byte(&self, a: u16) -> u8 { self.ram[(a - 0x8000) as usize] }
}

impl Bus for SoundBus {
    fn read(&mut self, a: u16) -> u8 {
        match a {
            0x0000..=0x7FFF => self.rom.get(a as usize).copied().unwrap_or(0),
            0x8000..=0x87FF => self.ram[(a - 0x8000) as usize],
            0x9000 => self.sres,
            0xA000 => self.latch,
            0xB000..=0xB00D => self.k007232.read((a - 0xB000) as usize),
            0xC000..=0xC001 => if a & 1 == 0 { 0xFF } else { self.ym.status() | if self.ym_busy > 0 { 0x80 } else { 0 } },
            0xF000 => self.upd.busy_r() as u8,
            _ => 0,
        }
    }

    fn write(&mut self, a: u16, v: u8) {
        match a {
            0x8000..=0x87FF => self.ram[(a - 0x8000) as usize] = v,
            0x9000 => {
                // bit 1 is the uPD7759's /RESET, bit 2 plays the title music
                self.upd.reset_w(v & 2 != 0);
                if v & 4 != 0 {
                    if !self.title_playing {
                        self.title_playing = true;
                        self.title_pos = 0;
                        self.title_phase = 0;
                    }
                } else {
                    self.title_playing = false;
                }
                self.sres = v;
            }
            0xB000..=0xB00D => {
                if let Some(port) = self.k007232.write((a - 0xB000) as usize, v) {
                    self.k007232.set_volume(0, (port >> 4) as i32 * 0x11, 0);
                    self.k007232.set_volume(1, 0, (port & 0x0F) as i32 * 0x11);
                }
            }
            0xC000..=0xC001 => {
                self.ym.write((a & 1) as u8, v);
                if a & 1 != 0 { self.ym_busy = YM_BUSY; }
            }
            0xD000 => self.upd.port_w(v),
            0xE000 => self.upd.start_w(v & 1 == 0),
            _ => {}
        }
    }

    fn port_in(&mut self, _p: u16) -> u8 { 0xFF }
    fn port_out(&mut self, _p: u16, _v: u8) {}
}

pub struct Sound {
    cpu: Z80Cpu,
    pub bus: SoundBus,
    /// Z80 IRQ, held until the Z80 takes it
    pub irq: bool,
    owed: i64,
    phase: u64,
    ym_div: u32,
    k_div: u32,
    ym_out: f32,
    k_out: f32,
    upd_phase: u64,
    resample: u64,
    acc: f32,
    acc_n: u32,
    pub out: Vec<f32>,
}

#[derive(Serialize)]
pub struct SoundStateRef<'a> {
    cpu: Z80State,
    bus: &'a SoundBus,
    irq: bool,
    owed: i64,
    phase: u64,
    ym_div: u32,
    k_div: u32,
    ym_out: f32,
    k_out: f32,
    upd_phase: u64,
    resample: u64,
}

#[derive(Deserialize)]
pub struct SoundState {
    cpu: Z80State,
    bus: SoundBus,
    irq: bool,
    owed: i64,
    phase: u64,
    ym_div: u32,
    k_div: u32,
    ym_out: f32,
    k_out: f32,
    upd_phase: u64,
    resample: u64,
}

impl Sound {
    pub fn new(rom: Vec<u8>, k_rom: Vec<u8>, upd_rom: Vec<u8>, title_rom: &[u8]) -> Self {
        let mut ym = Ym2151::new();
        ym.set_running(true);
        Self {
            cpu: Z80Cpu::new(),
            bus: SoundBus {
                k007232: K007232::new(k_rom.len()),
                rom, k_rom, upd_rom, title: decode_title(title_rom),
                ram: vec![0; 0x800], latch: 0, sres: 0, ym, ym_busy: 0,
                upd: Upd7759::new(),
                title_playing: false, title_pos: 0, title_phase: 0,
            },
            irq: false, owed: 0, phase: 0, ym_div: 0, k_div: 0, ym_out: 0.0, k_out: 0.0,
            upd_phase: 0, resample: 0, acc: 0.0, acc_n: 0, out: Vec::new(),
        }
    }

    pub fn save(&self) -> SoundStateRef<'_> {
        SoundStateRef {
            cpu: self.cpu.save_state(), bus: &self.bus, irq: self.irq, owed: self.owed,
            phase: self.phase, ym_div: self.ym_div, k_div: self.k_div, ym_out: self.ym_out,
            k_out: self.k_out, upd_phase: self.upd_phase, resample: self.resample,
        }
    }

    pub fn load(&mut self, s: SoundState) {
        let mut bus = s.bus;
        bus.rom = std::mem::take(&mut self.bus.rom);
        bus.k_rom = std::mem::take(&mut self.bus.k_rom);
        bus.upd_rom = std::mem::take(&mut self.bus.upd_rom);
        bus.title = std::mem::take(&mut self.bus.title);
        self.bus = bus;
        self.cpu.load_state(&s.cpu);
        self.irq = s.irq;
        self.owed = s.owed;
        self.phase = s.phase;
        self.ym_div = s.ym_div;
        self.k_div = s.k_div;
        self.ym_out = s.ym_out;
        self.k_out = s.k_out;
        self.upd_phase = s.upd_phase;
        self.resample = s.resample;
    }

    /// The ROMs back out, for rebuilding the board on a reset.
    pub fn take_roms(&mut self) -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<i16>) {
        let b = &mut self.bus;
        (std::mem::take(&mut b.rom), std::mem::take(&mut b.k_rom),
         std::mem::take(&mut b.upd_rom), std::mem::take(&mut b.title))
    }

    pub fn reset(&mut self) {
        let (rom, k_rom, upd_rom, title) = self.take_roms();
        *self = Self::new(rom, k_rom, upd_rom, &[]);
        self.bus.title = title;
    }

    /// Run `num / den` seconds of the sound board.
    pub fn run(&mut self, num: u64, den: u64) {
        self.phase += num * CLOCK;
        self.owed += (self.phase / den) as i64;
        self.phase %= den;
        while self.owed > 0 {
            let mut c = 0;
            if self.irq {
                c = self.cpu.irq(&mut self.bus, 0xFF);
                if c != 0 { self.irq = false; }   // HOLD_LINE: dropped once taken
            }
            if c == 0 { c = self.cpu.step(&mut self.bus); }
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
            self.ym_out = (l + r) as f32 * (GAIN_YM / 32768.0);
        }
        self.k_div += 1;
        if self.k_div == 128 {
            self.k_div = 0;
            let [l, r] = b.k007232.sample(&b.k_rom);
            self.k_out = (l + r) as f32 * (GAIN_K007232 / 32768.0);
        }
        self.upd_phase += UPD_CLOCK;
        while self.upd_phase >= CLOCK {
            self.upd_phase -= CLOCK;
            b.upd.clock(&b.upd_rom);
        }
        let mut title = 0.0;
        if b.title_playing {
            b.title_phase += TITLE_RATE;
            while b.title_phase >= CLOCK {
                b.title_phase -= CLOCK;
                b.title_pos += 1;
            }
            match b.title.get(b.title_pos as usize) {
                Some(&s) => title = s as f32 * (GAIN_TITLE / 32768.0),
                None => b.title_playing = false,   // the sample is over
            }
        }
        self.acc += self.ym_out + self.k_out + b.upd.output() * GAIN_UPD + title;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_fp_matches_ymfm() {
        // the exponent bits come inverted: 0x1E00 is "zero", exponent 0
        assert_eq!(decode_fp(0x1E00), 0);
        assert_eq!(decode_fp(0x1E01), 64, "mantissa shifted up to 16 bits");
        // the smallest code is a big negative value, scaled down by the full exponent
        assert_eq!(decode_fp(0x0000), -256);
        assert_eq!(decode_fp(0x1FFF), 0x7FC0, "largest positive");
    }

    #[test]
    fn title_rom_is_decoded_two_bytes_a_sample() {
        let s = decode_title(&[0x00, 0x00, 0x08, 0xF0]);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0], decode_fp(0));
        assert_eq!(s[1], decode_fp((0xF008u32 >> 3) as i16));
    }
}
