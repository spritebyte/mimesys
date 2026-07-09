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

/// Implemented by the bus/memory system so the APU can fetch DMC sample bytes
/// via DMA without needing to know about the cartridge/mapper directly.
pub trait DmcMemoryReader {
    /// Read a single byte for DMC playback. Real hardware stalls the CPU for
    /// 1-4 cycles per fetch; the caller (bus) is responsible for accounting for
    /// that stall if cycle-accurate CPU timing matters to you.
    fn dmc_read(&self, addr: u16) -> u8;
}

pub struct NesAPU {
    frame_counter: u32,
    mode_5_step: bool,
    irq_inhibit: bool,
    frame_irq_flag: bool,
    
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
    dmc_dma_request: bool,       // set when a byte needs fetching; bus/CPU should service it

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
    total_apu_cycles: u64,
}

impl NesAPU {
    pub fn new() -> Self {
        Self { 
            frame_counter: 0, 
            mode_5_step: false, 
            irq_inhibit: false, 
            frame_irq_flag: false,
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
            dmc_timer: DMC_RATE_TABLE[0],
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
        }
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
                self.dmc_timer = DMC_RATE_TABLE[self.dmc_rate_index as usize];
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
                self.pulse1_enabled = (data & 0x01) > 0;
                self.pulse2_enabled = (data & 0x02) > 0;
                self.tri_enabled = (data & 0x04) > 0;
                self.noise_enabled = (data & 0x08) > 0;
                let dmc_enable = (data & 0x10) > 0;

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
                } else if self.dmc_bytes_remaining == 0 {
                    self.dmc_current_addr = 0xC000 + (self.dmc_sample_addr_reg as u16 * 64);
                    self.dmc_bytes_remaining = (self.dmc_sample_length_reg as u16 * 16) + 1;
                }
                // Reading $4015 acknowledges the DMC IRQ; writing $4015 does not
                // clear dmc_interrupt_flag directly, but disabling the channel
                // does clear any pending IRQ per hardware behavior.
                if !dmc_enable {
                    self.dmc_interrupt_flag = false;
                }
            }

            // Frame Counter Mode
            0x4017 => {
                self.mode_5_step = (data & 0x80) != 0;
                self.irq_inhibit = (data & 0x40) != 0;
                if self.irq_inhibit {
                    self.frame_irq_flag = false;
                }
                self.frame_counter = 0;
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

    /// 'mem' provides DMC sample bytes via DMA. Pass your Bus (or any wrapper
    /// around it) - it just needs to implement 'DmcMemoryReader::dmc_read'.
    pub fn step<M: DmcMemoryReader>(&mut self, cycles: u64, mem: &M) {
        for _ in 0..cycles {
            self.total_apu_cycles += 1;
            if self.p1_timer > self.p1_timer_reload {
                self.p1_timer = self.p1_timer_reload;
            }
            if self.p2_timer > self.p2_timer_reload {
                self.p2_timer = self.p2_timer_reload;
            }
            // 1. Tick Core Timers
            if self.frame_counter % 2 == 0 {
                // Pulse 1
                if self.p1_timer == 0 {
                    self.p1_timer = self.p1_timer_reload;
                    self.p1_sequence_step = (self.p1_sequence_step + 1) & 7;
                } else {
                    self.p1_timer -= 1;
                }

                // Pulse 2
                if self.p2_timer == 0 {
                    self.p2_timer = self.p2_timer_reload;
                    self.p2_sequence_step = (self.p2_sequence_step + 1) & 7;
                } else {
                    self.p2_timer -= 1;
                }
            }

            // Triangle runs at CPU frequency
            if self.tri_timer == 0 {
                self.tri_timer = self.tri_timer_reload;
                if self.tri_length_counter > 0 && self.tri_linear_counter > 0 {
                    self.tri_sequence_step = (self.tri_sequence_step + 1) & 31;
                }
            } else {
                self.tri_timer -= 1;
            }

            // Noise runs at CPU frequency
            if self.n_timer == 0 {
                self.n_timer = self.n_timer_reload;
                let shift_bit = if self.n_mode { 6 } else { 1 };
                let feedback = (self.n_shift_register & 1) ^ ((self.n_shift_register >> shift_bit) & 1);
                self.n_shift_register = (self.n_shift_register >> 1) | (feedback << 14);
            } else {
                self.n_timer -= 1;
            }

            // --- DMC Memory Reader ---
            // Whenever the sample buffer is empty and there are bytes left,
            // fetch the next byte via DMA. On real hardware this stalls the
            // CPU for 1-4 cycles; that's the bus's responsibility, not ours.
            if self.dmc_sample_buffer.is_none() && self.dmc_bytes_remaining > 0 {
                let byte = mem.dmc_read(self.dmc_current_addr);
                self.dmc_sample_buffer = Some(byte);
                self.dmc_dma_request = true; // bus can inspect/clear this if it wants stall accounting

                self.dmc_current_addr = if self.dmc_current_addr == 0xFFFF {
                    0x8000
                } else {
                    self.dmc_current_addr + 1
                };

                self.dmc_bytes_remaining -= 1;
                if self.dmc_bytes_remaining == 0 {
                    if self.dmc_loop {
                        // Restart the sample immediately.
                        self.dmc_current_addr = 0xC000 + (self.dmc_sample_addr_reg as u16 * 64);
                        self.dmc_bytes_remaining = (self.dmc_sample_length_reg as u16 * 16) + 1;
                    } else if self.dmc_irq_enabled {
                        self.dmc_interrupt_flag = true;
                    }
                }
            }

            // --- DMC Timer & Output Unit ---
            // The rate table value is in CPU cycles, so this ticks directly
            // against the per-cycle loop (unlike pulse, which halves first).
            if self.dmc_timer == 0 {
                self.dmc_timer = DMC_RATE_TABLE[self.dmc_rate_index as usize];

                if !self.dmc_silence_flag {
                    if (self.dmc_shift_register & 1) == 1 {
                        if self.dmc_output_level <= 125 { self.dmc_output_level += 2; }
                    } else {
                        if self.dmc_output_level >= 2 { self.dmc_output_level -= 2; }
                    }
                }
                self.dmc_shift_register >>= 1;

                if self.dmc_bits_remaining > 0 {
                    self.dmc_bits_remaining -= 1;
                }
                if self.dmc_bits_remaining == 0 {
                    // Output cycle ended; start a new one.
                    self.dmc_bits_remaining = 8;
                    if let Some(byte) = self.dmc_sample_buffer.take() {
                        self.dmc_silence_flag = false;
                        self.dmc_shift_register = byte;
                    } else {
                        self.dmc_silence_flag = true;
                    }
                }
            } else {
                self.dmc_timer -= 1;
            }

            // 2. Mix Digital Outputs
            // Pulse 1
            let (_, p1_muted) = self.calculate_sweep_target(true);
            if self.p1_length_counter > 0 && !p1_muted {
                let current_bit = DUTY_TABLE[self.p1_duty_index as usize][self.p1_sequence_step as usize];
                // Check bit 4 of $4000 (halt flag doubles as constant volume flag)
                let p1_active_volume = if self.p1_halt { self.p1_volume } else { self.p1_env_volume };
                self.p1_output = if current_bit > 0 { p1_active_volume as f32 } else { 0.0 };
            } else {
                self.p1_output = 0.0;
            }

            // Pulse 2
            let (_, p2_muted) = self.calculate_sweep_target(false);
            if self.p2_length_counter > 0 && !p2_muted {
                let current_bit = DUTY_TABLE[self.p2_duty_index as usize][self.p2_sequence_step as usize];
                let p2_active_volume = if self.p2_halt { self.p2_volume } else { self.p2_env_volume };
                self.p2_output = if current_bit > 0 { p2_active_volume as f32 } else { 0.0 };
            } else {
                self.p2_output = 0.0;
            }

            // Triangle
            let tri_output = if self.tri_length_counter > 0 && self.tri_linear_counter > 0 {
                TRI_SEQUENCE[self.tri_sequence_step as usize] as f32
            } else {
                0.0
            };

            // Noise
            let n_output = if self.noise_length_counter > 0 && (self.n_shift_register & 1) == 0 {
                let n_active_volume = if self.n_constant_volume { self.n_volume } else { self.n_env_volume };
                n_active_volume as f32
            } else {
                0.0
            };

            // 3. Frame Sequencer Steps (~240 Hz updates)
            self.frame_counter += 1;
            let mut env_clock = false;
            let mut len_clock = false;

            if !self.mode_5_step {
                match self.frame_counter {
                    7457 | 22371 => {
                        env_clock = true;
                    }
                    14913 => {
                        env_clock = true;
                        len_clock = true;
                    }
                    29828 => {
                        // CRITICAL: The Frame IRQ flag is first raised here!
                        if !self.irq_inhibit { 
                            self.frame_irq_flag = true; 
                        }
                    }
                    29829 => {
                        env_clock = true;
                        len_clock = true;
                        if !self.irq_inhibit { 
                            self.frame_irq_flag = true; 
                        }
                    }
                    _ => {
                        if self.frame_counter >= 29830 {
                            if !self.irq_inhibit { 
                                self.frame_irq_flag = true; 
                            }
                            self.frame_counter = 0; // Wrap around to 0
                        }
                    }
                }
            } else {
                if self.frame_counter == 7457 || self.frame_counter == 22371 {
                    env_clock = true;
                } else if self.frame_counter == 14913 {
                    env_clock = true;
                    len_clock = true;
                } else if self.frame_counter >= 37282 {
                    env_clock = true;
                    len_clock = true;
                    self.frame_counter = 0;
                }
            }

            if len_clock {
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

            if env_clock {
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

            // 4. Sample Extraction & Mixing Approximation
            self.sample_clock += 1.0;
            if self.sample_clock >= self.sample_rate_ratio {
                self.sample_clock -= self.sample_rate_ratio;
                
                // DMC output level is 7-bit (0-127); scale down to roughly the
                // same 0-15 range the other channels use before applying the
                // same weighting scheme, so it doesn't drown everything else out.
                let dmc_scaled = (self.dmc_output_level as f32 / 127.0) * 15.0;

                // Approximate linear mixing weights
                let mixed = (self.p1_output + self.p2_output + (tri_output * 0.6) + (n_output * 0.4) + (dmc_scaled * 0.6)) / 30.0;
                self.audio_buffer.push(mixed);
            }
        }
    }
}