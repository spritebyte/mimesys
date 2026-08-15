use crate::common::timed::Timed;
use crate::gameboy::gb_bus::GameBoyBus;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub struct GbPPU {
    last_master: u64,
    ticks: u64,
    dot: u64,
    div: u64,
    pending_irqs: u8,
    // registers
    lcdc: u8,
    scy: u8,
    scx: u8,
    bgp: u8,
    wx: u8,
    wy: u8,
    obp0: u8,
    obp1: u8,
    stat: u8,
    lyc: u8,
}

impl GbPPU {
    pub fn new(system_frame_ready: Arc<AtomicBool>) -> Self {
        let buffer_size = 160 * 144 * 3;
        Self {
            last_master: 0, ticks: 0,
            dot: 0,
            pending_irqs: 0,
            // Registers
            lcdc: 0x91, scy: 0, scx: 0, bgp: 0xFC, wx: 0, wy: 0, obp0: 0, obp1: 0, stat: 0x85, lyc: 0,
            div: 1,
        }
    }

    pub fn with_divider(div: u64) -> Self {
        Self {
            last_master: 0, ticks: 0,
            dot: 0,
            pending_irqs: 0,
            lcdc: 0x91, scy: 0, scx: 0, bgp: 0xFC, wx: 0, wy: 0, obp0: 0, obp1: 0, stat: 0x85, lyc: 0,
            div,
        }
    }

    fn tick_one_dot(&mut self) {

    }

    fn enter_vblank(&mut self) {
        self.pending_irqs |= 1 << 0;
    }

    pub fn take_irqs(&mut self) -> u8 {
        std::mem::replace(&mut self.pending_irqs, 0)
    }
}

impl Timed for GbPPU {
    fn run_until(&mut self, target_master: u64) {
        let target_dot = target_master / self.div;
        while self.dot < target_dot {
            self.tick_one_dot();
            self.dot = self.dot.wrapping_add(1);
        }
        self.last_master = target_master;
    }

    fn sync_point(&self) -> u64 { self.last_master }
}
