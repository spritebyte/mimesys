use std::sync::{Arc, Mutex};

pub struct DKVDP {
    slice_complete: bool,
    back_buffer: Vec<u8>,
    pub front_buffer: Arc<Mutex<Vec<u8>>>,
}

impl DKVDP {
    pub fn new() -> Self {
        let buffer_size = 160 * 144 * 4;
        
        Self {
            slice_complete: false,
            back_buffer: vec![0; buffer_size],
            front_buffer: Arc::new(Mutex::new(vec![0; buffer_size])),
        }
    }

    pub fn clear_slice_complete_flag(&mut self) {
        self.slice_complete = false;
    }

    pub fn is_slice_complete(&self) -> bool {
        self.slice_complete
    }
}