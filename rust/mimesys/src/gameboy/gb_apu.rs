use crate::common::timed::Timed;

const CPU_CLOCK: f64 = 4_194_304.0;

const DUTY_PATTERNS: [u8; 4] = [
    0b00000001, // 12.5%
    0b10000001, // 25%
    0b10000111, // 50%
    0b01111110, // 75%
];

pub struct GbAPU {
    sample_rate: f64,
    sample_timer: f64,
    sample_buffer: Vec<f32>,

    ch_enabled: [bool; 4],
    ch_frequency: [u16; 4],
    ch_timer: [f64; 4],
    ch_duty: [u8; 2],
    ch_duty_step: [u8; 2],
    ch_volume: [u8; 4],
    ch_output: [f64; 4],
    ch_length_timer: [u8; 4],
    ch_length_enabled: [bool; 4],

    env_initial_vol: [u8; 4],
    env_direction: [i8; 4],
    env_period: [u8; 4],
    env_counter: [u8; 4],

    sweep_period: u8,
    sweep_direction: u8,
    sweep_shift: u8,
    sweep_timer: u8,
    sweep_shadow_freq: u8,
    master_sound_enable: bool,
    master_vol_l: u8,
    master_vol_r: u8,
    panning: u8,
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
            master_sound_enable: true, // FIXED: Default to true on power-on
            ch_enabled: [false; 4],
            ch_frequency: [0; 4],
            ch_timer: [0.0; 4],
            ch_duty: [0, 0],
            ch_duty_step: [0, 0],
            ch_volume: [0; 4],
            ch_output: [0.0; 4],
            ch_length_timer: [0; 4],
            ch_length_enabled: [false; 4],
            env_initial_vol: [0; 4],
            env_direction: [0; 4],
            env_period: [0; 4],
            env_counter: [0; 4],
            master_vol_l: 7,
            master_vol_r: 7,
            panning: 0xFF,
            sweep_period: 0,
            sweep_direction: 1,
            sweep_shift: 0,
            sweep_timer: 0,
            sweep_shadow_freq: 0,
            last_master: 0,
            ticks: 0,
            div,
        }
    }

    pub fn write_register(&mut self, addr: u16, value: u8) {
        if !self.master_sound_enable && addr != 0xFF26 { return; }

        match addr {
            0xFF26 => {
                self.master_sound_enable = (value & 0x80) != 0;
                if !self.master_sound_enable {
                    self.ch_enabled = [false, false, false, false];
                }
            }
            0xFF11 | 0xFF16 | 0xFF20 => {
                let mut ch = 0;
                if addr == 0xFF16 { ch = 1; }
                if addr == 0xFF20 { ch = 3; }

                // FIXED: Only update duty for channels 1 and 2
                if ch < 2 {
                    self.ch_duty[ch] = (value >> 6) & 3;
                }
                self.ch_length_timer[ch] = 64 - (value & 0x3F);
            }
            0xFF12 | 0xFF17 | 0xFF21 => {
                let mut ch = 0;
                if addr == 0xFF17 { ch = 1; }
                if addr == 0xFF21 { ch = 3; }
                self.env_initial_vol[ch] = (value >> 4) & 0x0F;
                self.env_direction[ch] = if (value & 0x08) != 0 { 1 } else { -1 };
                self.env_period[ch] = value & 0x07;
                if (value >> 3) == 0 {
                    self.ch_enabled[ch] = false;
                }
            }
            0xFF13 | 0xFF18 => {
                let ch = if addr == 0xFF13 { 0 } else { 1 };
                self.ch_frequency[ch] = (self.ch_frequency[ch] & 0x700) | value as u16;
            }
            0xFF14 | 0xFF19 | 0xFF23 => {
                let mut ch: usize = 0;
                if addr == 0xFF19 { ch = 1; }
                if addr == 0xFF23 { ch = 3; }

                // FIXED: Bit 6 is length enable, NOT channel enable!
                self.ch_length_enabled[ch] = (value & 0x40) != 0;
                self.ch_frequency[ch] = (self.ch_frequency[ch] & 0xFF) | (((value & 7) as u16) << 8);

                if value & 0x80 != 0 {
                    self._trigger_channel(ch);
                }
            }
            _ => {}
        }
    }

    pub fn read_register(&self, addr: u16) -> u8 {
        match addr {
            0xFF26 => {
                let mut status = 0x70;
                if self.master_sound_enable { status |= 0x80; }
                if self.ch_enabled[0] { status |= 0x01; }
                if self.ch_enabled[1] { status |= 0x02; }
                if self.ch_enabled[2] { status |= 0x04; }
                if self.ch_enabled[3] { status |= 0x08; }
                status
            }
            _ => 0xFF,
        }
    }

    fn _trigger_channel(&mut self, ch: usize) {
        self.ch_enabled[ch] = true;
        self.ch_volume[ch] = self.env_initial_vol[ch];
        self.env_counter[ch] = self.env_period[ch];
    }

    pub fn drain_samples(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.sample_buffer)
    }

    fn generate_sample(&mut self) {
        let mut left = 0.0f32;
        let mut right = 0.0f32;

        if self.master_sound_enable {
            for ch in 0..2 {
                if self.ch_enabled[ch] {
                    let vol = self.ch_volume[ch] as f32 / 15.0;
                    let sample = (self.ch_output[ch] as f32) * vol;

                    left += sample;
                    right += sample;
                }
            }
        }

        self.sample_buffer.push(left * 0.05);
        self.sample_buffer.push(right * 0.05);
    }

    pub fn advance_cycles(&mut self, cycles: u64) {
        let cycles_f64 = cycles as f64;

        if self.master_sound_enable {
            for ch in 0..2 {
                if !self.ch_enabled[ch] { continue; }

                let period = ((2048 - self.ch_frequency[ch]) as f64) * 4.0;
                if period > 0.0 {
                    self.ch_timer[ch] += cycles_f64;
                    while self.ch_timer[ch] >= period {
                        self.ch_timer[ch] -= period;
                        self.ch_duty_step[ch] = (self.ch_duty_step[ch] + 1) & 7;

                        let duty_bit = (DUTY_PATTERNS[self.ch_duty[ch] as usize] >> self.ch_duty_step[ch]) & 1;
                        self.ch_output[ch] = if duty_bit != 0 { 1.0 } else { -1.0 };
                    }
                }
            }
        }

        let cycles_per_sample = CPU_CLOCK / self.sample_rate;
        self.sample_timer += cycles_f64;

        while self.sample_timer >= cycles_per_sample {
            self.sample_timer -= cycles_per_sample;
            self.generate_sample();
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