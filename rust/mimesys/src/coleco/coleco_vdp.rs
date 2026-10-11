use std::sync::{Arc, Mutex};

pub enum VDPMode { GRAPHICS_1, TEXT, GRAPHICS_2, MULTICOLOR, UNKNOWN }

pub struct ColecoVDP {
    mode: u8,
    status_register: u16,
    address_register: u16,
    vdp_registers: [u8; 8],
    is_write_mode: bool,
    slice_complete: bool,
    back_buffer: Vec<u8>,
    pub front_buffer: Arc<Mutex<Vec<u8>>>,
}

impl ColecoVDP {
    pub fn new() -> Self {
        let buffer_size = 256 * 192 * 4;

        Self {
            mode: 0,
            status_register: 0,
            address_register: 0,
            vdp_registers: [0; 8],
            is_write_mode: false,
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