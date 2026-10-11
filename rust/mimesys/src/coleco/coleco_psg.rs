
pub struct ColecoPSG {
    noise_mode: u8,
    sample_buffer: Vec<f32>,
}

impl ColecoPSG {
    pub fn new(sample_rate: f64) -> Self {
        Self {
            noise_mode: 0,
            sample_buffer: Vec::with_capacity(4096),
        }
    }

    pub fn drain_samples(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.sample_buffer)
    }
}