pub struct DKPSG {
    noise_mode: u8,
    noise_counter: u8,
    latched_register: u8,   // 0-3 for tones, 4-6 for volume, 7 for noise
    volume: [u8; 4],
    tone_period: [u8; 3],
    tone_counter: [u8; 3],
    tone_output: [u8; 3],
    sample_buffer: Vec<f32>,
}

impl DKPSG {
    pub fn new(sample_rate: f64) -> Self {
        Self {
            noise_mode: 0,
            noise_counter: 0,
            latched_register: 0,
            volume: [15; 4],
            tone_period: [0; 3],
            tone_counter: [0; 3],
            tone_output: [0; 3],
            sample_buffer: Vec::with_capacity(4096),
        }
    }

    pub fn drain_samples(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.sample_buffer)
    }
}