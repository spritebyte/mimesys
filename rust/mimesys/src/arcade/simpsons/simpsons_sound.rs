//! The Simpsons sound board (MAME simpsons.cpp z80_map, k053260.cpp; BSD-3-Clause).
//!
//!   Z80 @ 3.579545 MHz, YM2151 and K053260 on the same clock.
//!   0000-7FFF ROM   8000-BFFF ROM bank (FE00 & 7)   F000-F7FF RAM   F800-F801 YM2151
//!   FA00 W arm NMI   FC00-FC2F K053260   FE00 W bank
//!
//! Interrupts: the main CPU reading 1FC4 raises the Z80 IRQ (IM 1, held until taken).
//! NMI: a write to FA00 drops the NMI line; the K053260's SH1 output (a 56 kHz clock)
//! raises it again at its next rising edge, but not within 4 clocks of the end of the
//! arming store (MAME's way of making sure the HALT after it has started). The NMI line
//! is high at reset. The edge is latched (`nmi_pending`): the line can drop and rise
//! again inside one instruction.
//!
//! K053260: four PCM / 4-bit KADPCM voices from 2 MB of sample ROM, plus the four
//! communication ports between the main and sound CPUs. Only the YM2151's left output
//! is wired (to both speakers).

use serde::{Deserialize, Serialize};

use crate::common::audio::ym2151::Ym2151;
use crate::common::cpu::z80::Bus;

pub const SOUND_CLOCK: f64 = 3_579_545.0;
const OUT_RATE: f64 = 44_100.0;

const PAN_MUL: [[i32; 2]; 8] = [
    [0, 0], [127, 0], [116, 52], [104, 73], [90, 90], [73, 104], [52, 116], [0, 127],
];
const KADPCM: [i8; 16] = [0, 1, 2, 4, 8, 16, 32, 64, -128, -64, -32, -16, -8, -4, -2, -1];

#[derive(Clone, Default, Serialize, Deserialize)]
struct Voice {
    position: u32,
    counter: u16,
    output: i8,
    playing: bool,
    start: u32,
    length: u16,
    pitch: u16,
    volume: u8,
    pan: u8,
    looped: bool,
    kadpcm: bool,
    reverse: bool,
}

impl Voice {
    fn pan_volume(&self) -> [i32; 2] {
        let p = PAN_MUL[self.pan as usize & 7];
        [self.volume as i32 * p[0], self.volume as i32 * p[1]]
    }

    fn set_register(&mut self, off: usize, d: u8) {
        match off & 7 {
            0 => self.pitch = (self.pitch & 0x0F00) | d as u16,
            1 => self.pitch = (self.pitch & 0x00FF) | ((d as u16) << 8 & 0x0F00),
            2 => self.length = (self.length & 0xFF00) | d as u16,
            3 => self.length = (self.length & 0x00FF) | (d as u16) << 8,
            4 => self.start = (self.start & 0x1FFF00) | d as u32,
            5 => self.start = (self.start & 0x1F00FF) | (d as u32) << 8,
            6 => self.start = (self.start & 0x00FFFF) | ((d as u32) << 16 & 0x1F0000),
            _ => self.volume = d & 0x7F,
        }
    }

    fn key_on(&mut self) {
        self.position = if self.kadpcm { 1 } else { 0 };
        self.counter = 0x1000 - 64;
        self.output = 0;
        self.playing = true;
    }

    fn key_off(&mut self) {
        self.position = 0;
        self.output = 0;
        self.playing = false;
    }

    fn play(&mut self, rom: &[u8], out: &mut [i32; 2]) {
        self.counter = self.counter.wrapping_add(64);
        while self.counter >= 0x1000 {
            self.counter = self.counter - 0x1000 + self.pitch;
            self.position += 1;
            let mut bytepos = self.position >> self.kadpcm as u32;
            if bytepos > self.length as u32 {
                if self.looped {
                    self.position = 0;
                    self.output = 0;
                    bytepos = 0;
                } else {
                    self.playing = false;
                    return;
                }
            }
            let addr = if self.reverse { self.start.wrapping_sub(bytepos) } else { self.start + bytepos };
            let mut d = rom_byte(rom, addr);
            if self.kadpcm {
                if self.position & 1 != 0 { d >>= 4; }
                self.output = self.output.wrapping_add(KADPCM[d as usize & 0x0F]);
            } else {
                self.output = d as i8;
            }
        }
        let pv = self.pan_volume();
        out[0] += (self.output as i32 * pv[0]) >> 6;
        out[1] += (self.output as i32 * pv[1]) >> 6;
    }
}

fn rom_byte(rom: &[u8], addr: u32) -> u8 {
    rom.get(addr as usize & 0x1F_FFFF).copied().unwrap_or(0)
}

#[derive(Serialize, Deserialize)]
pub struct K053260 {
    pub ports: [u8; 4],
    keyon: u8,
    mode: u8,
    voices: [Voice; 4],
}

impl K053260 {
    fn new() -> Self { Self { ports: [0; 4], keyon: 0, mode: 0, voices: Default::default() } }

    fn reset(&mut self) {
        for v in &mut self.voices { *v = Voice::default(); }
    }

    fn read(&mut self, rom: &[u8], off: u16) -> u8 {
        match off & 0x3F {
            0x00 | 0x01 => self.ports[(off & 1) as usize],
            0x29 => self.voices.iter().enumerate().fold(0, |r, (i, v)| r | (v.playing as u8) << i),
            0x2E if self.mode & 1 != 0 => {
                let v = &mut self.voices[0];
                let d = rom_byte(rom, v.start + v.position);
                v.position = (v.position + 1) & 0xFFFF;
                d
            }
            _ => 0,
        }
    }

    fn write(&mut self, off: u16, d: u8) {
        let off = (off & 0x3F) as usize;
        if (0x08..=0x27).contains(&off) {
            self.voices[(off - 8) / 8].set_register(off, d);
            return;
        }
        match off {
            0x02 | 0x03 => self.ports[off] = d,
            0x28 => {
                let rising = d & !self.keyon;
                for (i, v) in self.voices.iter_mut().enumerate() {
                    v.reverse = d >> (i + 4) & 1 != 0;
                    if rising >> i & 1 != 0 { v.key_on(); }
                    else if d >> i & 1 == 0 { v.key_off(); }
                }
                self.keyon = d;
            }
            0x2A => for (i, v) in self.voices.iter_mut().enumerate() {
                v.looped = d >> i & 1 != 0;
                v.kadpcm = d >> (i + 4) & 1 != 0;
            },
            0x2C => { self.voices[0].pan = d & 7; self.voices[1].pan = (d >> 3) & 7; }
            0x2D => { self.voices[2].pan = d & 7; self.voices[3].pan = (d >> 3) & 7; }
            0x2F => self.mode = d,
            _ => {}
        }
    }

    /// One output sample (every 64 clocks): left, right in +-32768.
    fn sample(&mut self, rom: &[u8]) -> [i32; 2] {
        let mut out = [0i32; 2];
        if self.mode & 2 != 0 {
            for v in &mut self.voices {
                if v.playing { v.play(rom, &mut out); }
            }
        }
        [out[0].clamp(-32768, 32767), out[1].clamp(-32768, 32767)]
    }
}

#[derive(Serialize, Deserialize)]
pub struct SimpsonsSound {
    #[serde(skip)] rom: Vec<u8>,
    #[serde(skip)] samples: Vec<u8>,
    #[serde(with = "serde_bytes")]
    ram: Vec<u8>,
    bank: u8,
    pub k053260: K053260,
    pub ym: Ym2151,
    ym_busy: u32,
    /// Z80 IRQ (held until the Z80 takes it) and NMI line
    pub irq: bool,
    pub nmi_line: bool,
    /// a rising edge of the NMI line not yet taken by the Z80
    pub nmi_pending: bool,
    /// FA00 written during the instruction now running
    arm_pending: bool,
    nmi_block_until: u64,
    /// Z80 clocks since power on; the K053260's 64-clock frame position
    pub clock: u64,
    ym_out: i32,
    k_out: [i32; 2],
    out_phase: f64,
    acc: [f32; 2],
    acc_n: u32,
    #[serde(skip)] pub out: Vec<f32>,
}

impl SimpsonsSound {
    pub fn new(rom: Vec<u8>, samples: Vec<u8>) -> Self {
        let mut ym = Ym2151::new();
        ym.set_running(true);
        Self {
            rom, samples, ram: vec![0; 0x800], bank: 0,
            k053260: K053260::new(), ym, ym_busy: 0,
            irq: false, nmi_line: true, nmi_pending: false, arm_pending: false, nmi_block_until: 0, clock: 0,
            ym_out: 0, k_out: [0; 2], out_phase: 0.0, acc: [0.0; 2], acc_n: 0, out: Vec::new(),
        }
    }

    pub fn keep_static(&mut self, old: &mut Self) {
        self.rom = std::mem::take(&mut old.rom);
        self.samples = std::mem::take(&mut old.samples);
        self.out = std::mem::take(&mut old.out);
    }

    pub fn reset(&mut self) {
        self.bank = 0;
        self.irq = false;
        self.nmi_line = true;   // Z80 /NMI goes low with reset
        self.nmi_pending = false;
        self.arm_pending = false;
        self.nmi_block_until = 0;
        self.ym.reset();
        self.ym_busy = 0;
        self.k053260.reset();
    }

    /// Runs the chips for `cycles` Z80 clocks (the instruction that just ran).
    pub fn advance(&mut self, cycles: u32) {
        if self.arm_pending {
            // MAME: no NMI until 4 clocks after the arming store, so it can't arrive
            // before the HALT that follows it has started
            self.arm_pending = false;
            self.nmi_block_until = self.clock + cycles as u64 + 4;
        }
        let out_step = OUT_RATE / SOUND_CLOCK;
        for _ in 0..cycles {
            self.ym_busy = self.ym_busy.saturating_sub(1);
            let phase = self.clock & 63;
            // SH1 rises at the start of each 64-clock frame (high for 16 clocks)
            if phase == 0 && !self.nmi_line && self.clock >= self.nmi_block_until {
                self.nmi_line = true;
                self.nmi_pending = true;
            }
            if phase == 0 {
                let (l, _r) = self.ym.generate();
                self.ym_out = l;
                self.k_out = self.k053260.sample(&self.samples);
            }
            self.clock += 1;
            // YM2151 left only, at half level on both sides; K053260 at half level
            let l = (self.ym_out + self.k_out[0]) as f32 * (0.5 / 32768.0);
            let r = (self.ym_out + self.k_out[1]) as f32 * (0.5 / 32768.0);
            self.acc[0] += l;
            self.acc[1] += r;
            self.acc_n += 1;
            self.out_phase += out_step;
            if self.out_phase >= 1.0 {
                self.out_phase -= 1.0;
                let n = self.acc_n as f32;
                self.out.push((self.acc[0] / n).clamp(-1.0, 1.0));
                self.out.push((self.acc[1] / n).clamp(-1.0, 1.0));
                self.acc = [0.0; 2];
                self.acc_n = 0;
            }
        }
    }

    fn bank_offset(&self) -> usize {
        // entries 0 and 1 both map region 10000; 2-7 map 10000 + (n - 2) * 4000
        0x10000 + (self.bank.max(2) as usize - 2) * 0x4000
    }
}

impl Bus for SimpsonsSound {
    fn read(&mut self, a: u16) -> u8 {
        match a {
            0x0000..=0x7FFF => self.rom.get(a as usize).copied().unwrap_or(0xFF),
            0x8000..=0xBFFF => self.rom.get(self.bank_offset() + (a as usize - 0x8000)).copied().unwrap_or(0xFF),
            0xF000..=0xF7FF => self.ram[(a & 0x7FF) as usize],
            0xF800..=0xF801 => {
                if a & 1 == 0 { 0xFF } else { self.ym.status() | if self.ym_busy > 0 { 0x80 } else { 0 } }
            }
            0xFC00..=0xFC2F => self.k053260.read(&self.samples, a - 0xFC00),
            _ => 0xFF,
        }
    }

    fn write(&mut self, a: u16, v: u8) {
        match a {
            0xF000..=0xF7FF => self.ram[(a & 0x7FF) as usize] = v,
            0xF800..=0xF801 => {
                self.ym.write((a & 1) as u8, v);
                if a & 1 != 0 { self.ym_busy = 64; }
            }
            0xFA00 => {
                self.nmi_line = false;
                self.arm_pending = true;
            }
            0xFC00..=0xFC2F => self.k053260.write(a - 0xFC00, v),
            0xFE00 => self.bank = v & 7,
            _ => {}
        }
    }

    fn port_in(&mut self, _port: u16) -> u8 { 0xFF }
    fn port_out(&mut self, _port: u16, _val: u8) {}
}

#[cfg(test)]
impl SimpsonsSound {
    pub fn read_ram(&self, a: u16) -> u8 { self.ram[(a & 0x7FF) as usize] }
}
