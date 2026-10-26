// Irem M52/M62 sound board ("irem_audio", MAME src/mame/irem/irem.cpp), M62 version:
//
//   MC6803 @ 3.579545 MHz (E clock 894,886 Hz)
//     0000-001F on-chip I/O, 0080-00FF on-chip RAM
//     0800 (mirror F7FC)  W: interrupt acknowledge
//     0801 / 0802         W: MSM5205 #1 / #2 data
//     4000-FFFF           ROM
//   Port 1 = data bus to the two AY-3-8910s. Port 2 (on its bit 0 falling edge):
//     bit 2 = address (1) or data (0), bit 3 = AY "45M", bit 4 = AY "45L"
//   AY 45M: port A in = sound command latch; port B out: bits 0/1 reset MSM #1/#2,
//     bits 2-4 = MSM #1 rate and 3/4-bit mode (MSM #2 is a slave clocked by #1)
//   AY 45L: port A out = analog drums (bass/snare/hats) on Kid Niki-style boards: not emulated
//   MSM5205 #1 VCK drives the 6803 NMI: the CPU feeds a nibble per sample.
//   IRQ: the main CPU's command write asserts it when bit 7 = 0; the acknowledge write
//     clears it only when the latched command has bit 7 set.

use crate::common::audio::ay8910::Ay8910;
use crate::common::audio::msm5205::Msm5205;
use crate::common::cpu::m6801::{M6801, M6801Bus};

pub const SOUND_CPU_CLOCK: f64 = 3_579_545.0 / 4.0;
const MSM_CLOCK: f64 = 384_000.0;

#[derive(serde::Serialize, serde::Deserialize)]
struct SoundBus {
    #[serde(skip)]
    rom: Vec<u8>,           // 64 KB region, ROM from 0x4000
    ay: [Ay8910; 2],        // [0] = 45M, [1] = 45L
    msm: [Msm5205; 2],
    latch: u8,
    irq: bool,
    port1: u8,
    port2: u8,
}

impl SoundBus {
    fn ay_portb_45m(&mut self, data: u8) {
        self.msm[0].playmode((data >> 2) & 7);
        self.msm[1].playmode(((data >> 2) & 4) | 3);
        self.msm[0].set_reset(data & 1 != 0);
        self.msm[1].set_reset(data & 2 != 0);
    }
}

impl M6801Bus for SoundBus {
    fn read(&mut self, addr: u16) -> u8 {
        if addr >= 0x4000 { self.rom[addr as usize] } else { 0xFF }
    }

    fn write(&mut self, addr: u16, v: u8) {
        // A11 + A0/A1 are the only decoded lines (mirror 0xF7FC)
        if addr & 0x0800 != 0 {
            match addr & 0x0803 {
                0x0800 => if self.latch & 0x80 != 0 { self.irq = false; },
                0x0801 => self.msm[0].data_w(v),
                0x0802 => self.msm[1].data_w(v),
                _ => {}
            }
        }
    }

    fn port_read(&mut self, port: usize) -> u8 {
        if port == 0 {
            if self.port2 & 0x08 != 0 { return self.ay[0].read_data(self.latch); }
            if self.port2 & 0x10 != 0 { return self.ay[1].read_data(0xFF); }
            0xFF
        } else {
            0x00
        }
    }

    fn port_write(&mut self, port: usize, data: u8, _ddr: u8) {
        if port == 0 {
            self.port1 = data;
            return;
        }
        // write strobe: port 2 bit 0 falling
        if self.port2 & 0x01 != 0 && data & 0x01 == 0 {
            let v = self.port1;
            if self.port2 & 0x04 != 0 {
                if self.port2 & 0x08 != 0 { self.ay[0].write_address(v); }
                if self.port2 & 0x10 != 0 { self.ay[1].write_address(v); }
            } else {
                if self.port2 & 0x08 != 0 {
                    if let Some(b) = self.ay[0].write_data(v) { self.ay_portb_45m(b); }
                }
                if self.port2 & 0x10 != 0 { self.ay[1].write_data(v); }
            }
        }
        self.port2 = data;
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct M62Sound {
    cpu: M6801,
    bus: SoundBus,
    ay_phase: u32,
    msm_phase: f64,
    out_rate: f64,
    out_phase: f64,
    msm_acc: f32,
    msm_n: u32,
    #[serde(skip)]
    msm_out: Vec<f32>,
    dc: (f32, f32),
    cycle_debt: i64,
}

impl M62Sound {
    pub fn new(rom: Vec<u8>, sample_rate: u32) -> Self {
        let mut full = vec![0xFFu8; 0x10000];
        let n = rom.len().min(0x10000);
        full[..n].copy_from_slice(&rom[..n]);
        let clock = SOUND_CPU_CLOCK as u32;
        let mut bus = SoundBus {
            rom: full,
            ay: [Ay8910::new(clock, sample_rate), Ay8910::new(clock, sample_rate)],
            msm: [Msm5205::new(4), Msm5205::new(7)],   // S96_4B master, SEX_4B slave
            latch: 0,
            irq: true,
            port1: 0,
            port2: 0,
        };
        for ay in bus.ay.iter_mut() { ay.gain = 0.5; }
        let mut cpu = M6801::new();
        cpu.reset(&mut bus);
        Self {
            cpu, bus, ay_phase: 0, msm_phase: 0.0, out_rate: sample_rate as f64, out_phase: 0.0,
            msm_acc: 0.0, msm_n: 0, msm_out: Vec::new(), dc: (0.0, 0.0), cycle_debt: 0,
        }
    }

    pub fn reset(&mut self) {
        self.bus.latch = 0;
        self.bus.irq = true;
        self.bus.port1 = 0;
        self.bus.port2 = 0;
        for ay in self.bus.ay.iter_mut() { ay.reset(); }
        self.cpu.reset(&mut self.bus);
    }

    /// After loading a state: take the ROM back from the board being replaced.
    pub fn keep_static(&mut self, old: &mut Self) {
        self.bus.rom = std::mem::take(&mut old.bus.rom);
    }

    /// Main CPU write to the sound command latch.
    pub fn command(&mut self, data: u8) {
        self.bus.latch = data;
        if data & 0x80 == 0 { self.bus.irq = true; }
    }

    /// Run for `cycles` E-clock cycles.
    pub fn run(&mut self, cycles: i64) {
        // carry the overshoot of the last instruction into the next call
        self.cycle_debt += cycles;
        while self.cycle_debt > 0 {
            self.cpu.irq_line = self.bus.irq;
            let c = self.cpu.step(&mut self.bus);
            self.cycle_debt -= c as i64;
            for _ in 0..c {
                self.ay_phase += 1;
                if self.ay_phase == 8 {
                    self.ay_phase = 0;
                    self.bus.ay[0].tick();
                    self.bus.ay[1].tick();
                }
                self.msm_phase += MSM_CLOCK / SOUND_CPU_CLOCK;
                while self.msm_phase >= 1.0 {
                    self.msm_phase -= 1.0;
                    if let Some(vck) = self.bus.msm[0].clock() {
                        self.cpu.set_nmi(vck);
                        self.bus.msm[1].vclk_w(vck);
                    }
                    self.bus.msm[1].clock();
                }
                self.msm_acc += self.bus.msm[0].output() + self.bus.msm[1].output();
                self.msm_n += 1;
                self.out_phase += self.out_rate;
                if self.out_phase >= SOUND_CPU_CLOCK {
                    self.out_phase -= SOUND_CPU_CLOCK;
                    let s = self.msm_acc / self.msm_n as f32;
                    self.msm_acc = 0.0;
                    self.msm_n = 0;
                    let (xp, yp) = self.dc;
                    let y = s - xp + 0.995 * yp;
                    self.dc = (s, y);
                    self.msm_out.push(y);
                }
            }
        }
    }

    /// Interleaved stereo at the output rate: both AYs plus the ADPCM voices.
    pub fn drain(&mut self) -> Vec<f32> {
        let n = (self.bus.ay[0].out.len() / 2)
            .min(self.bus.ay[1].out.len() / 2)
            .min(self.msm_out.len());
        let mut out = Vec::with_capacity(n * 2);
        for i in 0..n {
            let s = (self.bus.ay[0].out[i * 2] + self.bus.ay[1].out[i * 2]) * 0.8
                + self.msm_out[i] * 1.2;
            let s = s.clamp(-1.0, 1.0);
            out.push(s);
            out.push(s);
        }
        self.bus.ay[0].out.drain(..n * 2);
        self.bus.ay[1].out.drain(..n * 2);
        self.msm_out.drain(..n);
        out
    }

    #[cfg(test)]
    pub(crate) fn ram(&self) -> &[u8] { &self.cpu.ram }
}

