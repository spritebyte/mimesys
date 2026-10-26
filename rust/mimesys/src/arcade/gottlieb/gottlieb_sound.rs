// Gottlieb "rev. 1" sound board with SC-01-A speech (MA-216), as on Q*bert.
// MAME src/mame/shared/gottlieb_a.cpp (gottlieb_sound_r1 / speech_r1a).
//
//   6502 @ 3.579545 MHz / 4, address bus masked to 0x7FFF
//   0000-007F  RIOT RAM (mirror 0x0D80)       0200-021F  RIOT I/O (mirror 0x0DE0)
//   1000       W: 8-bit DAC (MC1408)          2000       W: Votrax phoneme / inflection
//   3000       W: speech clock DAC            6000-7FFF  ROM
//
// The main board's command byte arrives on RIOT port A: bits 0-5 = ~command, bit 7 = the
// low 4 bits NANDed (so a command other than xF raises PA7 and the RIOT edge detector
// interrupts the 6502). Port B: bit 6 = sound test switch, bit 7 = Votrax A/R.
// NMI = Votrax A/R (ready for the next phoneme) OR port B bit 7 output low.

use crate::common::audio::votrax_sc01::{VotraxSc01, VotraxStub};
use crate::common::cpu::m6502::{AddressBus, CpuVariant, M6502Cpu};
use crate::common::io::riot6532::Riot6532;

pub const SOUND_CPU_CLOCK: f64 = 3_579_545.0 / 4.0;

#[derive(serde::Serialize, serde::Deserialize)]
pub enum Speech {
    Chip(Box<VotraxSc01>),
    /// no phone ROM: silent, but keeps the A/R timing
    Stub(VotraxStub),
}

impl Speech {
    fn ar(&self) -> bool {
        match self { Speech::Chip(v) => v.ar(), Speech::Stub(s) => s.ar() }
    }
}

/// MAME convert_speech_clock: "totally random guesswork" mapping of the clock DAC value.
fn speech_clock(data: u8) -> f64 {
    let d = data.max(0x40) as f64;
    950_000.0 + (d - 160.0) * 5500.0
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SoundBus {
    #[serde(skip)]
    rom: Vec<u8>,          // 0x6000-0x7FFF
    riot: Riot6532,
    dac: u8,
    speech: Speech,
    speech_clock: f64,
    /// GOTTLIEB_SPEECH_LOG=1: print every phoneme and clock write
    #[serde(skip)]
    log: bool,
    /// A/R is already high at power-on, so the first NMI edge comes at the end of the
    /// first phoneme. The 6502 core sees a low line before its first sample, so hold A/R
    /// off the NMI line until the game has spoken once (no spurious NMI at reset).
    spoken: bool,
}

impl AddressBus for SoundBus {
    fn read_byte(&mut self, addr: u16) -> u8 {
        let a = addr & 0x7FFF;
        if a < 0x1000 {
            if a & 0x0200 == 0 { self.riot.ram_read(a) } else { self.riot.io_read(a) }
        } else if a >= 0x6000 {
            self.rom[(a - 0x6000) as usize]
        } else {
            0xFF
        }
    }

    fn write_byte(&mut self, addr: u16, v: u8) {
        let a = addr & 0x7FFF;
        match a {
            0x0000..=0x0FFF => {
                if a & 0x0200 == 0 { self.riot.ram_write(a, v) } else { self.riot.io_write(a, v) }
            }
            0x1000..=0x1FFF => self.dac = v,
            0x2000..=0x2FFF => {
                let phone = !v & 0x3F;
                self.spoken = true;
                if self.log {
                    println!("speech: {:02X} {:4} infl {} ar {} clock {:.0} Hz",
                        phone, crate::common::audio::votrax_sc01::PHONE_NAMES[phone as usize],
                        v >> 6, self.speech.ar() as u8, self.speech_clock);
                }
                match &mut self.speech {
                    Speech::Chip(c) => { c.set_inflection(v >> 6); c.write(phone); }
                    Speech::Stub(s) => s.write(phone, self.speech_clock),
                }
            }
            0x3000..=0x3FFF => {
                self.speech_clock = speech_clock(v);
                if self.log { println!("speech: clock DAC {v:02X} -> {:.0} Hz", self.speech_clock); }
                if let Speech::Chip(c) = &mut self.speech { c.set_clock(self.speech_clock); }
            }
            _ => {}
        }
    }

    fn is_nmi_line_asserted(&mut self) -> bool {
        (self.spoken && self.speech.ar()) || self.riot.pb_output() & 0x80 == 0
    }
    fn is_irq_line_asserted(&mut self) -> bool { self.riot.irq() }
    fn is_nmi_enabled(&mut self) -> bool { true }
    fn step_cycles(&mut self, _cycles: u64) {}
    fn begin_cpu_cycle(&mut self) {}
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct GottliebSound {
    #[serde(skip, default = "new_6502")]
    cpu: M6502Cpu,
    bus: SoundBus,
    sample_rate: f64,
    out_phase: f64,
    dac_sum: f64,
    dac_n: u32,
    speech_last: f32,
    hp_prev_in: f32,
    hp_prev_out: f32,
    knock_t: f32,           // seconds into the knocker thump, < 0 = idle
    peak_dac: f32,
    peak_speech: f32,
    peak_n: u32,
    #[serde(skip)]
    pub out: Vec<f32>,
}

fn new_6502() -> M6502Cpu { M6502Cpu::new(CpuVariant::NMOS6502) }

/// Save state form: the 6502 (which has its own state type) plus everything else.
pub type SoundState = (crate::common::cpu::m6502::CpuState, GottliebSound);

impl GottliebSound {
    /// `audiocpu`: the 64 KB region (ROMs at 0x6000-0x7FFF). `votrax_rom`: sc01a.bin if present.
    pub fn new(audiocpu: &[u8], votrax_rom: Option<&[u8]>, sample_rate: u32) -> Self {
        let mut rom = vec![0u8; 0x2000];
        if audiocpu.len() >= 0x8000 {
            rom.copy_from_slice(&audiocpu[0x6000..0x8000]);
        } else {
            // a bare 4 KB / 8 KB image: put it at the top
            let n = audiocpu.len().min(0x2000);
            rom[0x2000 - n..].copy_from_slice(&audiocpu[audiocpu.len() - n..]);
        }
        let clock = speech_clock(0);
        let speech = match votrax_rom {
            Some(r) if r.len() >= 0x200 => Speech::Chip(Box::new(VotraxSc01::new(r, clock))),
            _ => Speech::Stub(VotraxStub::new()),
        };
        let mut riot = Riot6532::new();
        riot.pb_in = 0x7F;
        let mut bus = SoundBus { rom, riot, dac: 0x80, speech, speech_clock: clock,
            log: std::env::var("GOTTLIEB_SPEECH_LOG").is_ok(), spoken: false };
        let mut cpu = M6502Cpu::new(CpuVariant::NMOS6502);
        cpu.power_on(&mut bus);
        Self {
            cpu, bus, sample_rate: sample_rate as f64, out_phase: 0.0,
            dac_sum: 0.0, dac_n: 0, speech_last: 0.0, hp_prev_in: 0.0, hp_prev_out: 0.0,
            knock_t: -1.0, peak_dac: 0.0, peak_speech: 0.0, peak_n: 0, out: Vec::new(),
        }
    }

    pub fn save(&self) -> (crate::common::cpu::m6502::CpuState, &Self) { (self.cpu.get_state(), self) }

    /// Replace this board with a loaded one, keeping the ROMs (sound ROM, Votrax phone ROM).
    pub fn restore(&mut self, s: SoundState) {
        let (cpu, mut new) = s;
        new.bus.rom = std::mem::take(&mut self.bus.rom);
        new.bus.log = self.bus.log;
        if let (Speech::Chip(n), Speech::Chip(o)) = (&mut new.bus.speech, &self.bus.speech) {
            n.keep_static(o);
        }
        new.cpu = std::mem::replace(&mut self.cpu, new_6502());
        new.cpu.load_state(&cpu);
        *self = new;
    }

    pub fn has_speech(&self) -> bool { matches!(self.bus.speech, Speech::Chip(_)) }

    pub fn reset(&mut self) {
        self.bus.riot.reset();
        self.bus.spoken = false;
        self.cpu.reset(&mut self.bus);
    }

    /// Main board write to the sound latch.
    pub fn command(&mut self, data: u8) {
        let pa0_5 = !data & 0x3F;
        let pa7 = if data & 0x0F != 0x0F { 0x80 } else { 0 };
        self.bus.riot.set_pa_in(pa0_5 | pa7, 0xBF);
    }

    pub fn knock(&mut self) { self.knock_t = 0.0; }

    /// Run `cycles` sound CPU cycles, producing audio at the output rate.
    pub fn run(&mut self, cycles: i64) {
        let speech_per_cycle = self.bus.speech_clock / SOUND_CPU_CLOCK;
        for _ in 0..cycles {
            // Votrax A/R feeds the speech chip's request line on port B bit 7
            let ar = self.bus.speech.ar();
            self.bus.riot.pb_in = 0x7F | if ar { 0x80 } else { 0 };
            self.cpu.step_one_cycle(&mut self.bus);
            self.bus.riot.tick();
            match &mut self.bus.speech {
                Speech::Chip(c) => c.advance(speech_per_cycle),
                Speech::Stub(s) => s.advance(speech_per_cycle),
            }
            self.cpu.sample_interrupt_lines(&mut self.bus);

            self.dac_sum += self.bus.dac as f64;
            self.dac_n += 1;
            self.out_phase += self.sample_rate;
            if self.out_phase >= SOUND_CPU_CLOCK {
                self.out_phase -= SOUND_CPU_CLOCK;
                self.emit();
            }
        }
    }

    fn emit(&mut self) {
        let dac = (self.dac_sum / self.dac_n.max(1) as f64 - 128.0) / 128.0;
        self.dac_sum = 0.0;
        self.dac_n = 0;
        let speech = if let Speech::Chip(c) = &mut self.bus.speech {
            if !c.out.is_empty() {
                self.speech_last = c.out.iter().sum::<f32>() / c.out.len() as f32;
                c.out.clear();
            }
            self.speech_last
        } else { 0.0 };
        if self.bus.log {
            self.peak_dac = self.peak_dac.max(dac.abs() as f32);
            self.peak_speech = self.peak_speech.max(speech.abs());
            self.peak_n += 1;
            if self.peak_n >= self.sample_rate as u32 {
                if self.peak_speech > 0.001 || self.peak_dac > 0.01 {
                    println!("sound peaks (last second): DAC {:.3}  speech {:.3}", self.peak_dac, self.peak_speech);
                }
                self.peak_n = 0;
                self.peak_dac = 0.0;
                self.peak_speech = 0.0;
            }
        }
        // MAME mix: DAC 0.20, Votrax 0.80
        let mut s = dac as f32 * 0.20 * 2.5 + speech * 0.80 * 2.5;
        if self.knock_t >= 0.0 {
            // the cabinet knocker (a solenoid hitting the side panel): a short low thump
            let t = self.knock_t;
            s += (2.0 * std::f32::consts::PI * 70.0 * t).sin() * (-t * 30.0).exp() * 0.8;
            self.knock_t += 1.0 / self.sample_rate as f32;
            if self.knock_t > 0.25 { self.knock_t = -1.0; }
        }
        // DC blocker (the DAC idles off-center)
        let y = s - self.hp_prev_in + 0.995 * self.hp_prev_out;
        self.hp_prev_in = s;
        self.hp_prev_out = y;
        self.out.push(y.clamp(-1.0, 1.0));
    }
}

#[cfg(test)]
impl GottliebSound {
    pub(crate) fn dac_for_test(&self) -> u8 { self.bus.dac }
}
