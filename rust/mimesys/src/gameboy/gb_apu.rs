use serde::{Deserialize, Serialize};
use crate::common::timed::Timed;
//use std::collections::VecDeque;

const CPU_CLOCK: f64 = 4_194_304.0;

// Frame sequencer runs at 512 Hz -> one step every 8192 base cycles.
const FS_PERIOD: u32 = 8192;

// Duty patterns: bit read MSB->LSB as duty_step advances 0..7.
const DUTY_PATTERNS: [u8; 4] = [
    0b00000001, // 12.5%
    0b00000011, // 25%
    0b00001111, // 50%
    0b11111100, // 75%
];

// Noise divisor table (NR43 low 3 bits). Index 0 is treated as 8 (0.5).
const NOISE_DIVISORS: [u32; 8] = [8, 16, 32, 48, 64, 80, 96, 112];

// Read-back OR masks for $FF10..$FF3F. Bits that read as 1 (write-only / unused).
const READ_MASKS: [u8; 0x30] = [
    0x80, 0x3F, 0x00, 0xFF, 0xBF, // FF10 NR10..NR14
    0xFF, 0x3F, 0x00, 0xFF, 0xBF, // FF15(unused) NR21..NR24
    0x7F, 0xFF, 0x9F, 0xFF, 0xBF, // FF1A NR30..NR34
    0xFF, 0xFF, 0x00, 0x00, 0xBF, // FF1F(unused) NR41..NR44
    0x00, 0x00, 0x70,             // FF24 NR50, FF25 NR51, FF26 NR52
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, // FF27..FF2B unused
    0xFF, 0xFF, 0xFF, 0xFF,       // FF2C..FF2F unused
    // FF30..FF3F wave RAM -> handled separately, masks unused (0x00)
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
struct Square {
    enabled: bool,
    dac_on: bool,
    freq: u16,          // 11-bit
    timer: i32,         // frequency timer (base cycles)
    duty: u8,           // 0..3
    duty_step: u8,      // 0..7
    // length
    length: u16,        // remaining (0..64)
    length_enabled: bool,
    // envelope
    vol: u8,            // current 0..15
    env_initial: u8,
    env_add: bool,      // true = increase
    env_period: u8,
    env_timer: u8,
}

impl Square {
    fn trigger(&mut self) {
        self.enabled = self.dac_on;
        if self.length == 0 {
            self.length = 64;
        }
        self.timer = ((2048 - self.freq as i32).max(1)) * 4;
        self.vol = self.env_initial;
        self.env_timer = if self.env_period == 0 { 8 } else { self.env_period };
    }

    fn step_timer(&mut self, cycles: i32) {
        if !self.enabled { return; }
        let period = ((2048 - self.freq as i32).max(1)) * 4;
        self.timer -= cycles;
        while self.timer <= 0 {
            self.timer += period;
            self.duty_step = (self.duty_step + 1) & 7;
        }
    }

    fn clock_length(&mut self) {
        if self.length_enabled && self.length > 0 {
            self.length -= 1;
            if self.length == 0 {
                self.enabled = false;
            }
        }
    }

    fn clock_envelope(&mut self) {
        if self.env_period == 0 { return; }
        if self.env_timer > 0 { self.env_timer -= 1; }
        if self.env_timer == 0 {
            self.env_timer = self.env_period;
            if self.env_add && self.vol < 15 {
                self.vol += 1;
            } else if !self.env_add && self.vol > 0 {
                self.vol -= 1;
            }
        }
    }

    // Digital output 0..15.
    fn amplitude(&self) -> u8 {
        if !self.enabled || !self.dac_on { return 0; }
        let bit = (DUTY_PATTERNS[self.duty as usize] >> self.duty_step) & 1;
        if bit != 0 { self.vol } else { 0 }
    }
}

#[derive(Clone, Copy, Serialize, Deserialize)]
struct Wave {
    enabled: bool,
    dac_on: bool,
    freq: u16,
    timer: i32,
    position: usize,        // 0..31
    length: u16,            // 0..256
    length_enabled: bool,
    volume_shift: u8,       // 0=mute,1=100%,2=50%,3=25% (per NR32 code)
    ram: [u8; 16],
    sample_buffer: u8,      // current 4-bit sample
}

impl Default for Wave {
    fn default() -> Self {
        Wave {
            enabled: false, dac_on: false, freq: 0, timer: 0, position: 0,
            length: 0, length_enabled: false, volume_shift: 0,
            ram: [0; 16], sample_buffer: 0,
        }
    }
}

impl Wave {
    fn trigger(&mut self) {
        self.enabled = self.dac_on;
        if self.length == 0 {
            self.length = 256;
        }
        self.timer = ((2048 - self.freq as i32).max(1)) * 2;
        self.position = 0;
    }

    fn step_timer(&mut self, cycles: i32) {
        if !self.enabled { return; }
        let period = ((2048 - self.freq as i32).max(1)) * 2;
        self.timer -= cycles;
        while self.timer <= 0 {
            self.timer += period;
            self.position = (self.position + 1) & 31;
            let byte = self.ram[self.position / 2];
            self.sample_buffer = if self.position & 1 == 0 { byte >> 4 } else { byte & 0x0F };
        }
    }

    fn clock_length(&mut self) {
        if self.length_enabled && self.length > 0 {
            self.length -= 1;
            if self.length == 0 {
                self.enabled = false;
            }
        }
    }

    fn amplitude(&self) -> u8 {
        if !self.enabled || !self.dac_on { return 0; }
        match self.volume_shift {
            0 => 0,                       // mute
            1 => self.sample_buffer,      // 100%
            2 => self.sample_buffer >> 1, // 50%
            _ => self.sample_buffer >> 2, // 25%
        }
    }
}

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
struct Noise {
    enabled: bool,
    dac_on: bool,
    timer: i32,
    lfsr: u16,
    width_7bit: bool,
    clock_shift: u8,
    divisor_code: u8,
    length: u16,
    length_enabled: bool,
    vol: u8,
    env_initial: u8,
    env_add: bool,
    env_period: u8,
    env_timer: u8,
}

impl Noise {
    fn period(&self) -> i32 {
        (NOISE_DIVISORS[self.divisor_code as usize] << self.clock_shift) as i32
    }

    fn trigger(&mut self) {
        self.enabled = self.dac_on;
        if self.length == 0 {
            self.length = 64;
        }
        self.timer = self.period().max(1);
        self.lfsr = 0x7FFF;
        self.vol = self.env_initial;
        self.env_timer = if self.env_period == 0 { 8 } else { self.env_period };
    }

    fn step_timer(&mut self, cycles: i32) {
        if !self.enabled { return; }
        let period = self.period().max(1);
        self.timer -= cycles;
        while self.timer <= 0 {
            self.timer += period;
            let xor = (self.lfsr & 1) ^ ((self.lfsr >> 1) & 1);
            self.lfsr >>= 1;
            self.lfsr |= xor << 14;
            if self.width_7bit {
                self.lfsr &= !(1 << 6);
                self.lfsr |= xor << 6;
            }
        }
    }

    fn clock_length(&mut self) {
        if self.length_enabled && self.length > 0 {
            self.length -= 1;
            if self.length == 0 {
                self.enabled = false;
            }
        }
    }

    fn clock_envelope(&mut self) {
        if self.env_period == 0 { return; }
        if self.env_timer > 0 { self.env_timer -= 1; }
        if self.env_timer == 0 {
            self.env_timer = self.env_period;
            if self.env_add && self.vol < 15 {
                self.vol += 1;
            } else if !self.env_add && self.vol > 0 {
                self.vol -= 1;
            }
        }
    }

    fn amplitude(&self) -> u8 {
        if !self.enabled || !self.dac_on { return 0; }
        // Output when LFSR bit0 == 0.
        if self.lfsr & 1 == 0 { self.vol } else { 0 }
    }
}

#[derive(Serialize, Deserialize)]
pub struct GbAPU {
    sample_rate: f64,
    sample_timer: f64,
    #[serde(skip)]
    sample_buffer: Vec<f32>,

    ch1: Square,
    ch2: Square,
    ch3: Wave,
    ch4: Noise,

    // Channel 1 sweep
    sweep_period: u8,
    sweep_negate: bool,
    sweep_shift: u8,
    sweep_timer: u8,
    sweep_shadow: u16,
    sweep_enabled: bool,

    master_sound_enable: bool,
    master_vol_l: u8,   // 0..7
    master_vol_r: u8,   // 0..7
    panning: u8,        // NR51

    // Frame sequencer
    fs_counter: u32,
    fs_step: u8,

    // Raw register bytes for read-back ($FF10..$FF2F).
    #[serde(with = "serde_bytes")]
    raw: [u8; 0x30],

    last_master: u64,
    ticks: u64,
    div: u64,
}

impl GbAPU {
    pub fn new(sample_rate: f64) -> Self {
        Self::with_divider(1, sample_rate)
    }

    pub fn with_divider(div: u64, sample_rate: f64) -> Self {
        Self {
            sample_rate,
            sample_timer: 0.0,
            sample_buffer: Vec::with_capacity(4096),
            ch1: Square::default(),
            ch2: Square::default(),
            ch3: Wave::default(),
            ch4: Noise::default(),
            sweep_period: 0,
            sweep_negate: false,
            sweep_shift: 0,
            sweep_timer: 0,
            sweep_shadow: 0,
            sweep_enabled: false,
            master_sound_enable: true,
            master_vol_l: 7,
            master_vol_r: 7,
            panning: 0xF3,
            fs_counter: 0,
            fs_step: 0,
            raw: [0; 0x30],
            last_master: 0,
            ticks: 0,
            div,
        }
    }

    pub fn write_register(&mut self, addr: u16, value: u8) {
        // Wave RAM is always writable, even when sound is off.
        if (0xFF30..=0xFF3F).contains(&addr) {
            self.ch3.ram[(addr - 0xFF30) as usize] = value;
            return;
        }

        // When master sound is off, ignore everything except NR52.
        if !self.master_sound_enable && addr != 0xFF26 {
            return;
        }

        if (0xFF10..=0xFF2F).contains(&addr) {
            self.raw[(addr - 0xFF10) as usize] = value;
        }

        match addr {
            // ---- Channel 1: square + sweep ----
            0xFF10 => {
                self.sweep_period = (value >> 4) & 0x07;
                self.sweep_negate = (value & 0x08) != 0;
                self.sweep_shift = value & 0x07;
            }
            0xFF11 => {
                self.ch1.duty = (value >> 6) & 3;
                self.ch1.length = 64 - (value & 0x3F) as u16;
            }
            0xFF12 => {
                self.ch1.env_initial = (value >> 4) & 0x0F;
                self.ch1.env_add = (value & 0x08) != 0;
                self.ch1.env_period = value & 0x07;
                self.ch1.dac_on = (value & 0xF8) != 0;
                if !self.ch1.dac_on { self.ch1.enabled = false; }
            }
            0xFF13 => {
                self.ch1.freq = (self.ch1.freq & 0x700) | value as u16;
            }
            0xFF14 => {
                self.ch1.freq = (self.ch1.freq & 0xFF) | (((value & 7) as u16) << 8);
                self.ch1.length_enabled = (value & 0x40) != 0;
                if value & 0x80 != 0 {
                    self.ch1.trigger();
                    self.trigger_sweep();
                }
            }

            // ---- Channel 2: square ----
            0xFF16 => {
                self.ch2.duty = (value >> 6) & 3;
                self.ch2.length = 64 - (value & 0x3F) as u16;
            }
            0xFF17 => {
                self.ch2.env_initial = (value >> 4) & 0x0F;
                self.ch2.env_add = (value & 0x08) != 0;
                self.ch2.env_period = value & 0x07;
                self.ch2.dac_on = (value & 0xF8) != 0;
                if !self.ch2.dac_on { self.ch2.enabled = false; }
            }
            0xFF18 => {
                self.ch2.freq = (self.ch2.freq & 0x700) | value as u16;
            }
            0xFF19 => {
                self.ch2.freq = (self.ch2.freq & 0xFF) | (((value & 7) as u16) << 8);
                self.ch2.length_enabled = (value & 0x40) != 0;
                if value & 0x80 != 0 {
                    self.ch2.trigger();
                }
            }

            // ---- Channel 3: wave ----
            0xFF1A => {
                self.ch3.dac_on = (value & 0x80) != 0;
                if !self.ch3.dac_on { self.ch3.enabled = false; }
            }
            0xFF1B => {
                self.ch3.length = 256 - value as u16;
            }
            0xFF1C => {
                self.ch3.volume_shift = (value >> 5) & 0x03;
            }
            0xFF1D => {
                self.ch3.freq = (self.ch3.freq & 0x700) | value as u16;
            }
            0xFF1E => {
                self.ch3.freq = (self.ch3.freq & 0xFF) | (((value & 7) as u16) << 8);
                self.ch3.length_enabled = (value & 0x40) != 0;
                if value & 0x80 != 0 {
                    self.ch3.trigger();
                }
            }

            // ---- Channel 4: noise ----
            0xFF20 => {
                self.ch4.length = 64 - (value & 0x3F) as u16;
            }
            0xFF21 => {
                self.ch4.env_initial = (value >> 4) & 0x0F;
                self.ch4.env_add = (value & 0x08) != 0;
                self.ch4.env_period = value & 0x07;
                self.ch4.dac_on = (value & 0xF8) != 0;
                if !self.ch4.dac_on { self.ch4.enabled = false; }
            }
            0xFF22 => {
                self.ch4.clock_shift = (value >> 4) & 0x0F;
                self.ch4.width_7bit = (value & 0x08) != 0;
                self.ch4.divisor_code = value & 0x07;
            }
            0xFF23 => {
                self.ch4.length_enabled = (value & 0x40) != 0;
                if value & 0x80 != 0 {
                    self.ch4.trigger();
                }
            }

            // ---- Global ----
            0xFF24 => {
                self.master_vol_r = value & 0x07;
                self.master_vol_l = (value >> 4) & 0x07;
            }
            0xFF25 => {
                self.panning = value;
            }
            0xFF26 => {
                let enable = (value & 0x80) != 0;
                if !enable && self.master_sound_enable {
                    self.power_off();
                } else if enable && !self.master_sound_enable {
                    self.master_sound_enable = true;
                    self.fs_step = 0;
                }
                self.master_sound_enable = enable;
            }
            _ => {}
        }
    }

    pub fn read_register(&self, addr: u16) -> u8 {
        if (0xFF30..=0xFF3F).contains(&addr) {
            return self.ch3.ram[(addr - 0xFF30) as usize];
        }

        if addr == 0xFF26 {
            let mut status = 0x70;
            if self.master_sound_enable { status |= 0x80; }
            if self.ch1.enabled { status |= 0x01; }
            if self.ch2.enabled { status |= 0x02; }
            if self.ch3.enabled { status |= 0x04; }
            if self.ch4.enabled { status |= 0x08; }
            return status;
        }

        if (0xFF10..=0xFF2F).contains(&addr) {
            let i = (addr - 0xFF10) as usize;
            return self.raw[i] | READ_MASKS[i];
        }

        0xFF
    }

    fn power_off(&mut self) {
        // Clear all registers/state; NR52 handled by caller.
        let div = self.div;
        let sample_rate = self.sample_rate;
        let wave_ram = self.ch3.ram; // wave RAM is preserved across power-off
        let buf = std::mem::take(&mut self.sample_buffer);
        let ticks = self.ticks;
        let last_master = self.last_master;

        *self = GbAPU::with_divider(div, sample_rate);
        self.master_sound_enable = false;
        self.ch3.ram = wave_ram;
        self.sample_buffer = buf;
        self.ticks = ticks;
        self.last_master = last_master;
    }

    fn trigger_sweep(&mut self) {
        self.sweep_shadow = self.ch1.freq;
        self.sweep_timer = if self.sweep_period == 0 { 8 } else { self.sweep_period };
        self.sweep_enabled = self.sweep_period != 0 || self.sweep_shift != 0;
        if self.sweep_shift != 0 {
            // Immediate overflow check on trigger.
            let _ = self.sweep_calc();
        }
    }

    fn sweep_calc(&mut self) -> u16 {
        let delta = self.sweep_shadow >> self.sweep_shift;
        let new_freq = if self.sweep_negate {
            self.sweep_shadow.wrapping_sub(delta)
        } else {
            self.sweep_shadow + delta
        };
        if new_freq > 2047 {
            self.ch1.enabled = false; // overflow disables channel
        }
        new_freq
    }

    fn clock_sweep(&mut self) {
        if self.sweep_timer > 0 { self.sweep_timer -= 1; }
        if self.sweep_timer != 0 { return; }
        self.sweep_timer = if self.sweep_period == 0 { 8 } else { self.sweep_period };
        if self.sweep_enabled && self.sweep_period != 0 {
            let new_freq = self.sweep_calc();
            if new_freq <= 2047 && self.sweep_shift != 0 {
                self.sweep_shadow = new_freq;
                self.ch1.freq = new_freq;
                let _ = self.sweep_calc(); // second overflow check
            }
        }
    }

    fn step_frame_sequencer(&mut self) {
        match self.fs_step {
            0 | 4 => {
                self.ch1.clock_length();
                self.ch2.clock_length();
                self.ch3.clock_length();
                self.ch4.clock_length();
            }
            2 | 6 => {
                self.ch1.clock_length();
                self.ch2.clock_length();
                self.ch3.clock_length();
                self.ch4.clock_length();
                self.clock_sweep();
            }
            7 => {
                self.ch1.clock_envelope();
                self.ch2.clock_envelope();
                self.ch4.clock_envelope();
            }
            _ => {}
        }
        self.fs_step = (self.fs_step + 1) & 7;
    }

    pub fn drain_samples(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.sample_buffer)
    }

    fn generate_sample(&mut self) {
        if !self.master_sound_enable {
            self.sample_buffer.push(0.0);
            self.sample_buffer.push(0.0);
            return;
        }

        // Per-channel digital 0..15 -> normalized 0..1.
        let amps = [
            self.ch1.amplitude() as f32 / 15.0,
            self.ch2.amplitude() as f32 / 15.0,
            self.ch3.amplitude() as f32 / 15.0,
            self.ch4.amplitude() as f32 / 15.0,
        ];

        let mut left = 0.0f32;
        let mut right = 0.0f32;
        for ch in 0..4 {
            // NR51: bits 0-3 = right enable ch1-4, bits 4-7 = left enable ch1-4.
            if self.panning & (1 << ch) != 0 { right += amps[ch]; }
            if self.panning & (1 << (ch + 4)) != 0 { left += amps[ch]; }
        }

        // Master volume: (vol+1)/8, then normalize by 4 channels.
        left *= (self.master_vol_l as f32 + 1.0) / 8.0;
        right *= (self.master_vol_r as f32 + 1.0) / 8.0;
        left /= 4.0;
        right /= 4.0;

        // Center around 0 and apply a little headroom.
        self.sample_buffer.push((left * 2.0 - 1.0 * ((self.master_vol_l as f32 + 1.0) / 8.0)) * 0.5);
        self.sample_buffer.push((right * 2.0 - 1.0 * ((self.master_vol_r as f32 + 1.0) / 8.0)) * 0.5);
    }

    pub fn advance_cycles(&mut self, cycles: u64) {
        // Step frequency timers and the frame sequencer in one pass.
        // For robustness we process in FS-sized chunks so envelope/length/sweep
        // land at the right cycle boundaries even for large slices.
        let mut remaining = cycles;
        while remaining > 0 {
            let until_fs = (FS_PERIOD - self.fs_counter) as u64;
            let chunk = remaining.min(until_fs);
            let c = chunk as i32;

            if self.master_sound_enable {
                self.ch1.step_timer(c);
                self.ch2.step_timer(c);
                self.ch3.step_timer(c);
                self.ch4.step_timer(c);
            }

            // Sampling.
            let cycles_per_sample = CPU_CLOCK / self.sample_rate;
            self.sample_timer += chunk as f64;
            while self.sample_timer >= cycles_per_sample {
                self.sample_timer -= cycles_per_sample;
                self.generate_sample();
            }

            self.fs_counter += chunk as u32;
            if self.fs_counter >= FS_PERIOD {
                self.fs_counter -= FS_PERIOD;
                if self.master_sound_enable {
                    self.step_frame_sequencer();
                }
            }

            remaining -= chunk;
        }
    }
}

impl Timed for GbAPU {
    fn run_until(&mut self, target_master: u64) {
        let target_tick = target_master / self.div;

        if target_tick > self.ticks {
            let elapsed_cycles = target_tick - self.ticks;
            self.advance_cycles(elapsed_cycles);
            self.ticks = target_tick;
        }
        self.last_master = target_master;
    }

    fn sync_point(&self) -> u64 {
        self.last_master
    }
}
