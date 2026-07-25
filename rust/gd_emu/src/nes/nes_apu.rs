use serde::{Serialize, Deserialize};
use godot::global::godot_print;
use std::io::Write;

const LENGTH_TABLE: [u8; 32] = [
    10, 254, 20,  2, 40,  4, 80,  6, 160,  8, 60, 10, 14, 12, 26, 14,
    12,  16, 24, 18, 48, 20, 96, 22, 192, 24, 72, 26, 16, 28, 32, 30
];

const DUTY_TABLE: [[u8; 8]; 4] = [
    [0, 1, 0, 0, 0, 0, 0, 0], // 12.5%
    [0, 1, 1, 0, 0, 0, 0, 0], // 25%
    [0, 1, 1, 1, 1, 0, 0, 0], // 50%
    [1, 0, 0, 1, 1, 1, 1, 1], // 25% inverted
];

const TRI_SEQUENCE: [u8; 32] = [
    15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0,
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15
];

const NOISE_PERIOD_TABLE: [u16; 16] = [
    4, 8, 16, 32, 64, 96, 128, 160, 202, 254, 380, 508, 762, 1016, 2032, 4064
];

// NTSC DMC rate table: CPU cycles between output level changes
const DMC_RATE_TABLE: [u16; 16] = [
    428, 380, 340, 320, 286, 254, 226, 214, 190, 160, 142, 128, 106, 84, 72, 54
];

pub struct NesAPU {
    frame_counter: u32,
    mode_5_step: bool,
    irq_inhibit: bool,
    frame_irq_flag: bool,
    frame_counter_reset_delay: u8,
    frame_counter_clock_immediately: bool,

    // Pulse 1 Registers & Components
    p1_timer_reload: u16,
    p1_timer: u16,
    p1_sequence_step: u8,
    p1_length_counter: u8,
    p1_duty_index: u8,
    p1_volume: u8,
    pulse1_enabled: bool,
    p1_halt: bool,
    p1_constant_volume: bool,
    // Pulse 1 Sweep Unit
    p1_sweep_enabled: bool,
    p1_sweep_period: u8,
    p1_sweep_negate: bool,
    p1_sweep_shift: u8,
    p1_sweep_reload: bool,
    p1_sweep_divider: u8,

    // Pulse 2 Registers & Components
    p2_timer_reload: u16,
    p2_timer: u16,
    p2_sequence_step: u8,
    p2_length_counter: u8,
    p2_duty_index: u8,
    p2_volume: u8,
    pulse2_enabled: bool,
    p2_halt: bool,
    p2_sweep_enabled: bool,
    p2_sweep_period: u8,
    p2_sweep_negate: bool,
    p2_sweep_shift: u8,
    p2_sweep_reload: bool,
    p2_sweep_divider: u8,

    // Triangle Channel State
    tri_enabled: bool,
    tri_reload_flag: bool,
    tri_timer_reload: u16,
    tri_timer: u16,
    tri_sequence_step: u8,
    tri_length_counter: u8,
    tri_linear_counter: u8,
    tri_linear_reload: u8,
    tri_control_flag: bool,
    
    // Noise Channel Registers
    n_halt: bool,
    n_constant_volume: bool,
    n_volume: u8,
    n_mode: bool,
    n_timer_reload: u16,
    n_timer: u16,
    n_shift_register: u16, // Fixed to u16 for 15-bit LFSR
    noise_enabled: bool,
    noise_length_counter: u8,

    // DMC Channel
    dmc_irq_enabled: bool,
    dmc_loop: bool,
    dmc_rate_index: u8,
    dmc_timer: u16,
    dmc_output_level: u8,        // 7-bit DAC level, sent to mixer always
    dmc_sample_addr_reg: u8,     // raw $4012 value
    dmc_sample_length_reg: u8,   // raw $4013 value
    dmc_current_addr: u16,       // memory reader address counter
    dmc_bytes_remaining: u16,    // memory reader bytes-remaining counter
    dmc_sample_buffer: Option<u8>, // None = empty
    dmc_shift_register: u8,
    dmc_bits_remaining: u8,
    dmc_silence_flag: bool,
    dmc_enabled: bool,           // from $4015 bit 4
    dmc_interrupt_flag: bool,
    pub dmc_dma_request: bool,       // set when a byte needs fetching; bus/CPU should service it

    // Envelopes
    p1_env_volume: u8,
    p1_env_divider: u8,
    p1_env_start: bool,
    p2_env_volume: u8,
    p2_env_divider: u8,
    p2_env_start: bool,
    n_env_volume: u8,
    n_env_divider: u8,
    n_env_start: bool,

    // Audio Tracking
    audio_buffer: Vec<f32>,
    sample_clock: f32,
    sample_rate_ratio: f32,
    p1_output: f32,
    p2_output: f32,
    pub total_apu_cycles: u64,
    pub even_cycle: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ApuState {
    pub frame_counter: u32,
    pub mode_5_step: bool,
    pub irq_inhibit: bool,
    pub frame_irq_flag: bool,

    // Pulse 1
    pub p1_timer_reload: u16,
    pub p1_timer: u16,
    pub p1_sequence_step: u8,
    pub p1_length_counter: u8,
    pub p1_duty_index: u8,
    pub p1_volume: u8,
    pub pulse1_enabled: bool,
    pub p1_halt: bool,
    pub p1_constant_volume: bool,
    pub p1_sweep_enabled: bool,
    pub p1_sweep_period: u8,
    pub p1_sweep_negate: bool,
    pub p1_sweep_shift: u8,
    pub p1_sweep_reload: bool,
    pub p1_sweep_divider: u8,

    // Pulse 2
    pub p2_timer_reload: u16,
    pub p2_timer: u16,
    pub p2_sequence_step: u8,
    pub p2_length_counter: u8,
    pub p2_duty_index: u8,
    pub p2_volume: u8,
    pub pulse2_enabled: bool,
    pub p2_halt: bool,
    pub p2_sweep_enabled: bool,
    pub p2_sweep_period: u8,
    pub p2_sweep_negate: bool,
    pub p2_sweep_shift: u8,
    pub p2_sweep_reload: bool,
    pub p2_sweep_divider: u8,

    // Triangle
    pub tri_enabled: bool,
    pub tri_reload_flag: bool,
    pub tri_timer_reload: u16,
    pub tri_timer: u16,
    pub tri_sequence_step: u8,
    pub tri_length_counter: u8,
    pub tri_linear_counter: u8,
    pub tri_linear_reload: u8,
    pub tri_control_flag: bool,

    // Noise
    pub n_halt: bool,
    pub n_constant_volume: bool,
    pub n_volume: u8,
    pub n_mode: bool,
    pub n_timer_reload: u16,
    pub n_timer: u16,
    pub n_shift_register: u16,
    pub noise_enabled: bool,
    pub noise_length_counter: u8,

    // DMC
    pub dmc_irq_enabled: bool,
    pub dmc_loop: bool,
    pub dmc_rate_index: u8,
    pub dmc_timer: u16,
    pub dmc_output_level: u8,
    pub dmc_sample_addr_reg: u8,
    pub dmc_sample_length_reg: u8,
    pub dmc_current_addr: u16,
    pub dmc_bytes_remaining: u16,
    pub dmc_sample_buffer: Option<u8>,
    pub dmc_shift_register: u8,
    pub dmc_bits_remaining: u8,
    pub dmc_silence_flag: bool,
    pub dmc_enabled: bool,
    pub dmc_interrupt_flag: bool,
    pub dmc_dma_request: bool,

    // Envelopes
    pub p1_env_volume: u8,
    pub p1_env_divider: u8,
    pub p1_env_start: bool,
    pub p2_env_volume: u8,
    pub p2_env_divider: u8,
    pub p2_env_start: bool,
    pub n_env_volume: u8,
    pub n_env_divider: u8,
    pub n_env_start: bool,

    // Output/resampling continuity — kept so audio doesn't click/pop
    // right at the resume point
    pub sample_clock: f32,
    pub sample_rate_ratio: f32,
    pub p1_output: f32,
    pub p2_output: f32,

    pub total_apu_cycles: u64,

    pub frame_counter_reset_delay: u8,
    pub frame_counter_clock_immediately: bool,
    pub even_cycle: bool,
    // Deliberately NOT saved: `audio_buffer` — this is just a queue of
    // already-generated samples waiting to be drained by
    // take_audio_samples(); it's output, not state. On restore it should
    // start empty and simply refill on the next few step() calls.
}

impl NesAPU {
    pub fn new() -> Self {
        Self { 
            frame_counter: 0, 
            mode_5_step: false, 
            irq_inhibit: false, 
            frame_irq_flag: false,
            frame_counter_clock_immediately: false,
            frame_counter_reset_delay: 0,
            audio_buffer: Vec::with_capacity(1000),
            sample_clock: 0.0,
            sample_rate_ratio: 1789773.0 / 44100.0,
            p1_timer_reload: 0,
            p1_timer: 0,
            p1_sequence_step: 0,
            p1_length_counter: 0,
            p1_duty_index: 0,
            p1_volume: 0,
            pulse1_enabled: false,
            p1_constant_volume: false,
            p1_halt: false,
            p1_output: 0.0,
            p1_sweep_enabled: false,
            p1_sweep_period: 0,
            p1_sweep_negate: false,
            p1_sweep_shift: 0,
            p1_sweep_reload: false,
            p1_sweep_divider: 0,
            p2_timer_reload: 0,
            p2_timer: 0,
            p2_sequence_step: 0,
            p2_length_counter: 0,
            p2_duty_index: 0,
            p2_volume: 0,
            pulse2_enabled: false,
            p2_halt: false,
            p2_output: 0.0,
            p2_sweep_enabled: false,
            p2_sweep_period: 0,
            p2_sweep_negate: false,
            p2_sweep_shift: 0,
            p2_sweep_reload: false,
            p2_sweep_divider: 0,
            tri_enabled: false,
            tri_reload_flag: false,
            tri_timer_reload: 0,
            tri_timer: 0,
            tri_sequence_step: 0,
            tri_length_counter: 0,
            tri_linear_counter: 0,
            tri_linear_reload: 0,
            tri_control_flag: false,
            n_halt: false,
            n_constant_volume: false,
            n_volume: 0,
            n_mode: false,
            n_timer_reload: 0,
            n_timer: 0,
            n_shift_register: 1, // Must be initialized to 1!
            noise_enabled: false,
            noise_length_counter: 0,
            dmc_irq_enabled: false,
            dmc_loop: false,
            dmc_rate_index: 0,
            dmc_timer: DMC_RATE_TABLE[0] - 1,
            dmc_output_level: 0,
            dmc_sample_addr_reg: 0,
            dmc_sample_length_reg: 0,
            dmc_current_addr: 0xC000,
            dmc_bytes_remaining: 0,
            dmc_sample_buffer: None,
            dmc_shift_register: 0,
            dmc_bits_remaining: 0,
            dmc_silence_flag: true,
            dmc_enabled: false,
            dmc_interrupt_flag: false,
            dmc_dma_request: false,
            p1_env_volume: 0,
            p1_env_divider: 0,
            p1_env_start: false,
            p2_env_volume: 0,
            p2_env_divider: 0,
            p2_env_start: false,
            n_env_volume: 0,
            n_env_divider: 0,
            n_env_start: false,
            total_apu_cycles: 0,
            even_cycle: true,
        }
    }

    pub fn get_state(&self) -> ApuState {
        ApuState {
            frame_counter: self.frame_counter,
            mode_5_step: self.mode_5_step,
            irq_inhibit: self.irq_inhibit,
            frame_irq_flag: self.frame_irq_flag,

            // Pulse 1
            p1_timer_reload: self.p1_timer_reload,
            p1_timer: self.p1_timer,
            p1_sequence_step: self.p1_sequence_step,
            p1_length_counter: self.p1_length_counter,
            p1_duty_index: self.p1_duty_index,
            p1_volume: self.p1_volume,
            pulse1_enabled: self.pulse1_enabled,
            p1_halt: self.p1_halt,
            p1_constant_volume: self.p1_constant_volume,
            p1_sweep_enabled: self.p1_sweep_enabled,
            p1_sweep_period: self.p1_sweep_period,
            p1_sweep_negate: self.p1_sweep_negate,
            p1_sweep_shift: self.p1_sweep_shift,
            p1_sweep_reload: self.p1_sweep_reload,
            p1_sweep_divider: self.p1_sweep_divider,

            // Pulse 2
            p2_timer_reload: self.p2_timer_reload,
            p2_timer: self.p2_timer,
            p2_sequence_step: self.p2_sequence_step,
            p2_length_counter: self.p2_length_counter,
            p2_duty_index: self.p2_duty_index,
            p2_volume: self.p2_volume,
            pulse2_enabled: self.pulse2_enabled,
            p2_halt: self.p2_halt,
            p2_sweep_enabled: self.p2_sweep_enabled,
            p2_sweep_period: self.p2_sweep_period,
            p2_sweep_negate: self.p2_sweep_negate,
            p2_sweep_shift: self.p2_sweep_shift,
            p2_sweep_reload: self.p2_sweep_reload,
            p2_sweep_divider: self.p2_sweep_divider,

            // Triangle
            tri_enabled: self.tri_enabled,
            tri_reload_flag: self.tri_reload_flag,
            tri_timer_reload: self.tri_timer_reload,
            tri_timer: self.tri_timer,
            tri_sequence_step: self.tri_sequence_step,
            tri_length_counter: self.tri_length_counter,
            tri_linear_counter: self.tri_linear_counter,
            tri_linear_reload: self.tri_linear_reload,
            tri_control_flag: self.tri_control_flag,

            // Noise
            n_halt: self.n_halt,
            n_constant_volume: self.n_constant_volume,
            n_volume: self.n_volume,
            n_mode: self.n_mode,
            n_timer_reload: self.n_timer_reload,
            n_timer: self.n_timer,
            n_shift_register: self.n_shift_register,
            noise_enabled: self.noise_enabled,
            noise_length_counter: self.noise_length_counter,

            // DMC
            dmc_irq_enabled: self.dmc_irq_enabled,
            dmc_loop: self.dmc_loop,
            dmc_rate_index: self.dmc_rate_index,
            dmc_timer: self.dmc_timer,
            dmc_output_level: self.dmc_output_level,
            dmc_sample_addr_reg: self.dmc_sample_addr_reg,
            dmc_sample_length_reg: self.dmc_sample_length_reg,
            dmc_current_addr: self.dmc_current_addr,
            dmc_bytes_remaining: self.dmc_bytes_remaining,
            dmc_sample_buffer: self.dmc_sample_buffer,
            dmc_shift_register: self.dmc_shift_register,
            dmc_bits_remaining: self.dmc_bits_remaining,
            dmc_silence_flag: self.dmc_silence_flag,
            dmc_enabled: self.dmc_enabled,
            dmc_interrupt_flag: self.dmc_interrupt_flag,
            dmc_dma_request: self.dmc_dma_request,

            // Envelopes
            p1_env_volume: self.p1_env_volume,
            p1_env_divider: self.p1_env_divider,
            p1_env_start: self.p1_env_start,
            p2_env_volume: self.p2_env_volume,
            p2_env_divider: self.p2_env_divider,
            p2_env_start: self.p2_env_start,
            n_env_volume: self.n_env_volume,
            n_env_divider: self.n_env_divider,
            n_env_start: self.n_env_start,

            sample_clock: self.sample_clock,
            sample_rate_ratio: self.sample_rate_ratio,
            p1_output: self.p1_output,
            p2_output: self.p2_output,
            frame_counter_clock_immediately: self.frame_counter_clock_immediately,
            frame_counter_reset_delay: self.frame_counter_reset_delay,
            total_apu_cycles: self.total_apu_cycles,
            even_cycle: self.even_cycle,
        }
    }

    pub fn load_state(&mut self, state: &ApuState) {
        self.frame_counter = state.frame_counter;
        self.mode_5_step = state.mode_5_step;
        self.irq_inhibit = state.irq_inhibit;
        self.frame_irq_flag = state.frame_irq_flag;

        // Pulse 1
        self.p1_timer_reload = state.p1_timer_reload;
        self.p1_timer = state.p1_timer;
        self.p1_sequence_step = state.p1_sequence_step;
        self.p1_length_counter = state.p1_length_counter;
        self.p1_duty_index = state.p1_duty_index;
        self.p1_volume = state.p1_volume;
        self.pulse1_enabled = state.pulse1_enabled;
        self.p1_halt = state.p1_halt;
        self.p1_constant_volume = state.p1_constant_volume;
        self.p1_sweep_enabled = state.p1_sweep_enabled;
        self.p1_sweep_period = state.p1_sweep_period;
        self.p1_sweep_negate = state.p1_sweep_negate;
        self.p1_sweep_shift = state.p1_sweep_shift;
        self.p1_sweep_reload = state.p1_sweep_reload;
        self.p1_sweep_divider = state.p1_sweep_divider;

        // Pulse 2
        self.p2_timer_reload = state.p2_timer_reload;
        self.p2_timer = state.p2_timer;
        self.p2_sequence_step = state.p2_sequence_step;
        self.p2_length_counter = state.p2_length_counter;
        self.p2_duty_index = state.p2_duty_index;
        self.p2_volume = state.p2_volume;
        self.pulse2_enabled = state.pulse2_enabled;
        self.p2_halt = state.p2_halt;
        self.p2_sweep_enabled = state.p2_sweep_enabled;
        self.p2_sweep_period = state.p2_sweep_period;
        self.p2_sweep_negate = state.p2_sweep_negate;
        self.p2_sweep_shift = state.p2_sweep_shift;
        self.p2_sweep_reload = state.p2_sweep_reload;
        self.p2_sweep_divider = state.p2_sweep_divider;

        // Triangle
        self.tri_enabled = state.tri_enabled;
        self.tri_reload_flag = state.tri_reload_flag;
        self.tri_timer_reload = state.tri_timer_reload;
        self.tri_timer = state.tri_timer;
        self.tri_sequence_step = state.tri_sequence_step;
        self.tri_length_counter = state.tri_length_counter;
        self.tri_linear_counter = state.tri_linear_counter;
        self.tri_linear_reload = state.tri_linear_reload;
        self.tri_control_flag = state.tri_control_flag;

        // Noise
        self.n_halt = state.n_halt;
        self.n_constant_volume = state.n_constant_volume;
        self.n_volume = state.n_volume;
        self.n_mode = state.n_mode;
        self.n_timer_reload = state.n_timer_reload;
        self.n_timer = state.n_timer;
        self.n_shift_register = state.n_shift_register;
        self.noise_enabled = state.noise_enabled;
        self.noise_length_counter = state.noise_length_counter;

        // DMC
        self.dmc_irq_enabled = state.dmc_irq_enabled;
        self.dmc_loop = state.dmc_loop;
        self.dmc_rate_index = state.dmc_rate_index;
        self.dmc_timer = state.dmc_timer;
        self.dmc_output_level = state.dmc_output_level;
        self.dmc_sample_addr_reg = state.dmc_sample_addr_reg;
        self.dmc_sample_length_reg = state.dmc_sample_length_reg;
        self.dmc_current_addr = state.dmc_current_addr;
        self.dmc_bytes_remaining = state.dmc_bytes_remaining;
        self.dmc_sample_buffer = state.dmc_sample_buffer;
        self.dmc_shift_register = state.dmc_shift_register;
        self.dmc_bits_remaining = state.dmc_bits_remaining;
        self.dmc_silence_flag = state.dmc_silence_flag;
        self.dmc_enabled = state.dmc_enabled;
        self.dmc_interrupt_flag = state.dmc_interrupt_flag;
        self.dmc_dma_request = state.dmc_dma_request;

        // Envelopes
        self.p1_env_volume = state.p1_env_volume;
        self.p1_env_divider =state.p1_env_divider;
        self.p1_env_start = state.p1_env_start;
        self.p2_env_volume = state.p2_env_volume;
        self.p2_env_divider = state.p2_env_divider;
        self.p2_env_start = state.p2_env_start;
        self.n_env_volume = state.n_env_volume;
        self.n_env_divider = state.n_env_divider;
        self.n_env_start = state.n_env_start;

        self.sample_clock = state.sample_clock;
        self.sample_rate_ratio = state.sample_rate_ratio;
        self.p1_output = state.p1_output;
        self.p2_output = state.p2_output;
        self.frame_counter_reset_delay = state.frame_counter_reset_delay;
        self.frame_counter_clock_immediately = state.frame_counter_clock_immediately;
        self.total_apu_cycles = state.total_apu_cycles;
        self.even_cycle = state.even_cycle;
    }

    pub fn total_cycles(&self) -> u64 { self.total_apu_cycles }

    // Returns (target_period, is_muted)
    fn calculate_sweep_target(&self, is_pulse_1: bool) -> (u16, bool) {
        let current_period = if is_pulse_1 { self.p1_timer_reload } else { self.p2_timer_reload };
        let shift = if is_pulse_1 { self.p1_sweep_shift } else { self.p2_sweep_shift };
        let negate = if is_pulse_1 { self.p1_sweep_negate } else { self.p2_sweep_negate };

        let delta = current_period >> shift;
        
        let target_period = if negate {
            if is_pulse_1 {
                // Pulse 1 uses ones' complement
                current_period.saturating_sub(delta).saturating_sub(1)
            } else {
                // Pulse 2 uses two's complement
                current_period.saturating_sub(delta)
            }
        } else {
            current_period + delta
        };

        // Mute condition: 
        // 1. If raw period is less than 8, pulse channel is silenced.
        // 2. If the sweep engine pushes the period past 0x7FF (2047), it mutes.
        let is_muted = current_period < 8 || target_period > 0x7FF;

        (target_period, is_muted)
    }

    pub fn take_audio_samples(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.audio_buffer)
    }

    pub fn write_reg(&mut self, addr: u16, data: u8) {
        match addr {
            // Pulse 1
            0x4000 => { 
                self.p1_duty_index = (data >> 6) & 0x03; 
                self.p1_halt = (data & 0x20) > 0;
                self.p1_constant_volume = (data & 0x10) != 0;
                self.p1_volume = data & 0x0F;
                self.p1_env_start = true;
            }
            0x4001 => {
                self.p1_sweep_enabled = (data & 0x80) > 0;
                self.p1_sweep_period = (data >> 4) & 0x07;
                self.p1_sweep_negate = (data & 0x08) > 0;
                self.p1_sweep_shift = data & 0x07;
                self.p1_sweep_reload = true;            
            }
            0x4002 => { 
                self.p1_timer_reload = (self.p1_timer_reload & 0x0700) | (data as u16);
            }
            0x4003 => { 
                self.p1_timer_reload = (self.p1_timer_reload & 0x00FF) | (((data & 0x07) as u16) << 8); 
                self.p1_sequence_step = 0; 
                self.write_p1_length(data);
                self.p1_sweep_reload = true;
                self.p1_env_start = true;
            }
            
            // Pulse 2
            0x4004 => {
                self.p2_duty_index = (data >> 6) & 0x03;
                self.p2_halt = (data & 0x20) > 0;
                self.p2_volume = data & 0x0F;
                self.p2_env_start = true;
            }
            0x4005 => {
                self.p2_sweep_enabled = (data & 0x80) > 0;
                self.p2_sweep_period = (data >> 4) & 0x07;
                self.p2_sweep_negate = (data & 0x08) > 0;
                self.p2_sweep_shift = data & 0x07;
                self.p2_sweep_reload = true;
            }
            0x4006 => {
                self.p2_timer_reload = (self.p2_timer_reload & 0x0700) | (data as u16);
            }
            0x4007 => {
                self.p2_timer_reload = (self.p2_timer_reload & 0x00FF) | (((data & 0x07) as u16) << 8);
                self.p2_sequence_step = 0;
                self.write_p2_length(data);
                self.p2_sweep_reload = true;
                self.p2_env_start = true;
            }

            // Triangle
            0x4008..=0x400B => {
                self.write_triangle_reg(addr, data);
            }

            // Noise
            0x400C => {
                self.n_halt = (data & 0x20) > 0;
                self.n_constant_volume = (data & 0x10) > 0;
                self.n_volume = data & 0x0F;
                self.n_env_start = true;
            }
            0x400E => {
                self.n_mode = (data & 0x80) > 0;
                self.n_timer_reload = NOISE_PERIOD_TABLE[(data & 0x0F) as usize];
            }
            0x400F => {
                if self.noise_enabled {
                    self.noise_length_counter = LENGTH_TABLE[((data >> 3) & 0x1F) as usize];
                }
                self.n_env_start = true;
            }

            // DMC
            0x4010 => {
                self.dmc_irq_enabled = (data & 0x80) > 0;
                self.dmc_loop = (data & 0x40) > 0;
                self.dmc_rate_index = data & 0x0F;
                self.dmc_timer = DMC_RATE_TABLE[self.dmc_rate_index as usize] - 1;
                if !self.dmc_irq_enabled {
                    self.dmc_interrupt_flag = false;
                }
            }
            0x4011 => {
                // Direct load: sets output level directly, bypassing the shifter.
                self.dmc_output_level = data & 0x7F;
            }
            0x4012 => {
                self.dmc_sample_addr_reg = data;
            }
            0x4013 => {
                self.dmc_sample_length_reg = data;
            }

            // Channels Status Control
            0x4015 => {
                self.pulse1_enabled = (data & 0x01) != 0;
                self.pulse2_enabled = (data & 0x02) != 0;
                self.tri_enabled = (data & 0x04) != 0;
                self.noise_enabled = (data & 0x08) != 0;
                let dmc_enable = (data & 0x10) != 0;
                
                self.dmc_interrupt_flag = false;


                if !self.pulse1_enabled { self.p1_length_counter = 0; }
                if !self.pulse2_enabled { self.p2_length_counter = 0; }
                if !self.tri_enabled { self.tri_length_counter = 0; }
                if !self.noise_enabled { self.noise_length_counter = 0; }

                // Writing 0 to the DMC enable bit immediately disables DMA and
                // silences the channel's bytes-remaining counter (but NOT the
                // output level - that holds its last value, per hardware).
                // Writing 1 only (re)starts the sample if bytes_remaining is 0;
                // if a sample is already playing, it is NOT restarted.
                self.dmc_enabled = dmc_enable;
                if !dmc_enable {
                    self.dmc_bytes_remaining = 0;
                    self.dmc_dma_request = false;
                } else if self.dmc_bytes_remaining == 0 {
                    self.dmc_current_addr = 0xC000 + (self.dmc_sample_addr_reg as u16 * 64);
                    self.dmc_bytes_remaining = (self.dmc_sample_length_reg as u16 * 16) + 1;
                    self.dmc_silence_flag = false;
                }
            }

            // Frame Counter Mode
            0x4017 => {
                self.mode_5_step = (data & 0x80) != 0;
                self.irq_inhibit = (data & 0x40) != 0;
                if self.irq_inhibit {
                    self.frame_irq_flag = false;
                }
                let delay = if self.total_apu_cycles % 2 == 0 { 3 } else { 4 };
                self.frame_counter_reset_delay = delay;
                if self.mode_5_step {
                    self.frame_counter_clock_immediately = true;
                } else {
                    self.frame_counter_clock_immediately = false;
                }
            }
            _ => {}
        }
    }

    fn write_triangle_reg(&mut self, addr: u16, data: u8) {
        match addr {
            0x4008 => {
                self.tri_control_flag = (data & 0x80) > 0;
                self.tri_linear_reload = data & 0x7F;
            }
            0x400A => {
                self.tri_timer_reload = (self.tri_timer_reload & 0x0700) | (data as u16);
            }
            0x400B => {
                self.tri_timer_reload = (self.tri_timer_reload & 0x00FF) | (((data & 0x07) as u16) << 8);
                if self.tri_enabled {
                    self.tri_length_counter = LENGTH_TABLE[((data >> 3) & 0x1F) as usize];
                }
                self.tri_reload_flag = true;
            }
            _ => {}
        }
    }

    pub fn read_4015(&mut self) -> u8 {
        let mut status = 0;

        if self.p1_length_counter > 0 { status |= 0x01; }
        if self.p2_length_counter > 0 { status |= 0x02; }
        if self.tri_length_counter > 0 { status |= 0x04; }
        if self.noise_length_counter > 0 { status |= 0x08; }
        if self.dmc_bytes_remaining > 0 { status |= 0x10; }

        if self.frame_irq_flag { status |= 0x40; }
        if self.dmc_interrupt_flag { status |= 0x80; }
        
        // Reading $4015 acknowledges and clears the Frame Counter IRQ flag!
        self.frame_irq_flag = false;
        // Note: reading $4015 does NOT clear the DMC IRQ flag (it's cleared by
        // disabling the channel via $4015 write, or by $4010 clearing IRQ enable).

        status
    }

    pub fn is_irq_asserted(&self) -> bool {
        (self.frame_irq_flag && !self.irq_inhibit) || self.dmc_interrupt_flag
    }

    pub fn write_p1_length(&mut self, data: u8) {
        if self.pulse1_enabled {
            self.p1_length_counter = LENGTH_TABLE[((data >> 3) & 0x1F) as usize];
        }
    }

    pub fn write_p2_length(&mut self, data: u8) {
        if self.pulse2_enabled {
            self.p2_length_counter = LENGTH_TABLE[((data >> 3) & 0x1F) as usize];
        }
    }

    pub fn step_one_cycle(&mut self) {
        self.total_apu_cycles += 1;
        self.even_cycle = !self.even_cycle;        
        // Handle a pending $4017 write's delayed reset first — this needs to
        // resolve before anything else this cycle touches frame_counter.
        let just_reset = self.tick_frame_counter_reset_delay();
        self.tick_pulse_timers();
        self.tick_triangle_timer();
        self.tick_noise_timer();
        self.tick_dmc_memory_reader();
        self.tick_dmc_output_unit();

        let (tri_output, n_output) = self.mix_digital_outputs();

        let (env_clock, len_clock) = if just_reset {
            (false, false)
        } else {
            self.tick_frame_sequencer()
        };
        if len_clock {
            self.clock_sweeps_and_lengths();
        }
        if env_clock {
            self.clock_envelopes_and_linear();
        }

        self.extract_sample(tri_output, n_output);
    }

    fn tick_frame_counter_reset_delay(&mut self) -> bool {
        if self.frame_counter_reset_delay == 0 {
            return false;
        }
        self.frame_counter_reset_delay -= 1;
        if self.frame_counter_reset_delay == 0 {
            self.frame_counter = 0;
            if self.frame_counter_clock_immediately {
                self.frame_counter_clock_immediately = false;
                self.clock_envelopes_and_linear();
                self.clock_sweeps_and_lengths();
            }
            return true;
        }
        false
    }

    /*
    fn clamp_pulse_timers(&mut self) {
        // Defensive: a register write mid-frame can lower *_timer_reload
        // below the timer's current in-flight value.
        if self.p1_timer > self.p1_timer_reload { self.p1_timer = self.p1_timer_reload; }
        if self.p2_timer > self.p2_timer_reload { self.p2_timer = self.p2_timer_reload; }
    }*/

    fn tick_pulse_timers(&mut self) {
        // Pulse channels run at half the CPU rate.
        if !self.even_cycle { return; }

        if self.p1_timer == 0 {
            self.p1_timer = self.p1_timer_reload;
            self.p1_sequence_step = (self.p1_sequence_step + 1) & 7;
        } else {
            self.p1_timer -= 1;
        }

        if self.p2_timer == 0 {
            self.p2_timer = self.p2_timer_reload;
            self.p2_sequence_step = (self.p2_sequence_step + 1) & 7;
        } else {
            self.p2_timer -= 1;
        }
    }

    fn tick_triangle_timer(&mut self) {
        if self.tri_timer == 0 {
            self.tri_timer = self.tri_timer_reload;
            if self.tri_length_counter > 0 && self.tri_linear_counter > 0 {
                self.tri_sequence_step = (self.tri_sequence_step + 1) & 31;
            }
        } else {
            self.tri_timer -= 1;
        }
    }

    fn tick_noise_timer(&mut self) {
        if self.n_timer == 0 {
            self.n_timer = self.n_timer_reload;
            let shift_bit = if self.n_mode { 6 } else { 1 };
            let feedback = (self.n_shift_register & 1) ^ ((self.n_shift_register >> shift_bit) & 1);
            self.n_shift_register = (self.n_shift_register >> 1) | (feedback << 14);
        } else {
            self.n_timer -= 1;
        }
    }

    fn tick_dmc_memory_reader(&mut self) {
        if self.dmc_sample_buffer.is_some() || self.dmc_bytes_remaining == 0 { return; }
        if self.dmc_dma_request { return; }
        self.dmc_dma_request = true;
    }

    pub fn dmc_dma_address(&self) -> u16 { self.dmc_current_addr }

    pub fn dmc_dma_complete(&mut self, byte: u8) {
        self.dmc_dma_request = false;
        if self.dmc_bytes_remaining == 0 { return; }
        self.dmc_sample_buffer = Some(byte);
        self.dmc_current_addr = if self.dmc_current_addr == 0xFFFF { 0x8000 }
                                else { self.dmc_current_addr + 1 };
        self.dmc_bytes_remaining -= 1;
        if self.dmc_bytes_remaining == 0 {
            if self.dmc_loop {
                self.dmc_current_addr = 0xC000 + (self.dmc_sample_addr_reg as u16 * 64);
                self.dmc_bytes_remaining = (self.dmc_sample_length_reg as u16 * 16) + 1;
            } else {
                self.dmc_silence_flag = true;
                if self.dmc_irq_enabled { self.dmc_interrupt_flag = true; }
            }
        }
    }

    fn tick_dmc_output_unit(&mut self) {
        if self.dmc_timer != 0 {
            self.dmc_timer -= 1;
            return;
        }
        self.dmc_timer = DMC_RATE_TABLE[self.dmc_rate_index as usize] - 1;

        if !self.dmc_silence_flag {
            if (self.dmc_shift_register & 1) == 1 {
                if self.dmc_output_level <= 125 { self.dmc_output_level += 2; }
            } else if self.dmc_output_level >= 2 {
                self.dmc_output_level -= 2;
            }
        }
        self.dmc_shift_register >>= 1;

        if self.dmc_bits_remaining > 0 {
            self.dmc_bits_remaining -= 1;
        }
        if self.dmc_bits_remaining == 0 {
            self.dmc_bits_remaining = 8;
            if let Some(byte) = self.dmc_sample_buffer.take() {
                self.dmc_silence_flag = false;
                self.dmc_shift_register = byte;
            } else {
                self.dmc_silence_flag = true;
            }
        }
    }

    fn clock_envelopes_and_linear(&mut self) {
        // Triangle Linear Counter Processing
        if self.tri_reload_flag {
            self.tri_linear_counter = self.tri_linear_reload;
        } else if self.tri_linear_counter > 0 {
            self.tri_linear_counter -= 1;
        }
        if !self.tri_control_flag {
            self.tri_reload_flag = false;
        }
        // --- PULSE 1 ENVELOPE ---
        if self.p1_env_start {
            self.p1_env_start = false;
            self.p1_env_volume = 15;
            self.p1_env_divider = self.p1_volume; // Volume register acts as reload value
        } else {
            if self.p1_env_divider == 0 {
                self.p1_env_divider = self.p1_volume;
                if self.p1_env_volume > 0 {
                    self.p1_env_volume -= 1;
                } else if self.p1_halt { // Loop flag (halt bit doubles as envelope loop)
                    self.p1_env_volume = 15;
                }
            } else {
                self.p1_env_divider -= 1;
            }
        }

        // --- PULSE 2 ENVELOPE ---
        if self.p2_env_start {
            self.p2_env_start = false;
            self.p2_env_volume = 15;
            self.p2_env_divider = self.p2_volume;
        } else {
            if self.p2_env_divider == 0 {
                self.p2_env_divider = self.p2_volume;
                if self.p2_env_volume > 0 {
                    self.p2_env_volume -= 1;
                } else if self.p2_halt {
                    self.p2_env_volume = 15;
                }
            } else {
                self.p2_env_divider -= 1;
            }
        }

        // --- NOISE ENVELOPE ---
        if self.n_env_start {
            self.n_env_start = false;
            self.n_env_volume = 15;
            self.n_env_divider = self.n_volume;
        } else {
            if self.n_env_divider == 0 {
                self.n_env_divider = self.n_volume;
                if self.n_env_volume > 0 {
                    self.n_env_volume -= 1;
                } else if self.n_halt {
                    self.n_env_volume = 15;
                }
            } else {
                self.n_env_divider -= 1;
            }
        }        
    }
    
    fn clock_sweeps_and_lengths(&mut self) {
        if self.p1_length_counter > 0 && !self.p1_halt { self.p1_length_counter -= 1; }
        if self.p2_length_counter > 0 && !self.p2_halt { self.p2_length_counter -= 1; }
        if self.tri_length_counter > 0 && !self.tri_control_flag { self.tri_length_counter -= 1; }
        if self.noise_length_counter > 0 && !self.n_halt { self.noise_length_counter -= 1; }
        // --- PULSE 1 SWEEP TICK ---
        let (p1_target, p1_muted) = self.calculate_sweep_target(true);
        if self.p1_sweep_divider == 0 && self.p1_sweep_enabled && !p1_muted && self.p1_sweep_shift > 0 {
            self.p1_timer_reload = p1_target;
        }
        if self.p1_sweep_divider == 0 || self.p1_sweep_reload {
            self.p1_sweep_divider = self.p1_sweep_period;
        self.p1_sweep_reload = false;
            } else {
            self.p1_sweep_divider -= 1;
        }

        // --- PULSE 2 SWEEP TICK ---
        let (p2_target, p2_muted) = self.calculate_sweep_target(false);
        if self.p2_sweep_divider == 0 && self.p2_sweep_enabled && !p2_muted && self.p2_sweep_shift > 0 {
            self.p2_timer_reload = p2_target;
        }
        if self.p2_sweep_divider == 0 || self.p2_sweep_reload {
            self.p2_sweep_divider = self.p2_sweep_period;
            self.p2_sweep_reload = false;
        } else {
            self.p2_sweep_divider -= 1;
        }
    }

    /// Returns (triangle_output, noise_output) — these two aren't persisted
    /// on self the way p1_output/p2_output are, so they're just passed
    /// straight through to extract_sample().
    fn mix_digital_outputs(&mut self) -> (f32, f32) {
        let (_, p1_muted) = self.calculate_sweep_target(true);
        self.p1_output = if self.p1_length_counter > 0 && !p1_muted {
            let bit = DUTY_TABLE[self.p1_duty_index as usize][self.p1_sequence_step as usize];
            let vol = if self.p1_halt { self.p1_volume } else { self.p1_env_volume };
            if bit > 0 { vol as f32 } else { 0.0 }
        } else { 0.0 };

        let (_, p2_muted) = self.calculate_sweep_target(false);
        self.p2_output = if self.p2_length_counter > 0 && !p2_muted {
            let bit = DUTY_TABLE[self.p2_duty_index as usize][self.p2_sequence_step as usize];
            let vol = if self.p2_halt { self.p2_volume } else { self.p2_env_volume };
            if bit > 0 { vol as f32 } else { 0.0 }
        } else { 0.0 };

        let tri_output = if self.tri_length_counter > 0 && self.tri_linear_counter > 0 {
            TRI_SEQUENCE[self.tri_sequence_step as usize] as f32
        } else { 0.0 };

        let n_output = if self.noise_length_counter > 0 && (self.n_shift_register & 1) == 0 {
            let vol = if self.n_constant_volume { self.n_volume } else { self.n_env_volume };
            vol as f32
        } else { 0.0 };

        (tri_output, n_output)
    }

    /// Advances the frame sequencer by one cycle and reports which units
    /// should be clocked this cycle. Also raises the frame IRQ flag where
    /// applicable — same 4-step/5-step logic as before, just extracted.
    fn tick_frame_sequencer(&mut self) -> (bool, bool) {
        self.frame_counter += 1;
        let mut env_clock = false;
        let mut len_clock = false;

        if !self.mode_5_step {
            match self.frame_counter {
                7457 | 22371 => { env_clock = true; }
                14913 => { env_clock = true; len_clock = true; }
                29828 | 29829 => {
                    env_clock = true; len_clock = true;
                    if !self.irq_inhibit { self.frame_irq_flag = true; }
                }
                _ => {
                    if self.frame_counter >= 29830 {
                        if !self.irq_inhibit { self.frame_irq_flag = true; }
                        self.frame_counter = 0;
                    }
                }
            }
        } else {
            if self.frame_counter == 7457 || self.frame_counter == 22371 {
                env_clock = true;
            } else if self.frame_counter == 14913 {
                env_clock = true; len_clock = true;
            } else if self.frame_counter >= 37281 {
                env_clock = true; len_clock = true;
                self.frame_counter = 0;
            }
        }
        (env_clock, len_clock)
    }

    fn extract_sample(&mut self, tri_output: f32, n_output: f32) {
        self.sample_clock += 1.0;
        if self.sample_clock < self.sample_rate_ratio { return; }
        self.sample_clock -= self.sample_rate_ratio;

        let pulse_sum = self.p1_output + self.p2_output;          // each 0..15
        let pulse_out = if pulse_sum == 0.0 { 0.0 }
                else { 95.88 / ((8128.0 / pulse_sum) + 100.0) };

        let tnd = tri_output / 8227.0            // 0..15
            + n_output / 12241.0             // 0..15
            + (self.dmc_output_level as f32) / 22638.0;   // 0..127, unscaled
        let tnd_out = if tnd == 0.0 { 0.0 }
              else { 159.79 / ((1.0 / tnd) + 100.0) };

        self.audio_buffer.push(pulse_out + tnd_out);   // ~0.0..1.0
    }
}