use crate::common::timed::Timed;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub struct GbPPU {
    last_master: u64,
    dot: u64,
    ly: u8,
    lyc: u8,
    scx: u8,
    div: u64,
}

impl GbPPU {
    pub fn new(system_frame_ready: Arc<AtomicBool>) -> Self {
        let buffer_size = 256 * 240 * 4;
        Self {
            last_master: 0,
            dot: 0,
            ly: 0,
            lyc: 0,
            scx: 0,
            div: 0,
        }
    }
    fn tick_one_dot(&mut self) {

    }
}

impl Timed for GbPPU {
    fn run_until(&mut self, target_master: u64) {
        let target_dot = target_master / self.div;
    }

    fn sync_point(&self) -> u64 { self.last_master }
}
