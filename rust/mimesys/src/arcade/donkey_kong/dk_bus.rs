use crate::common::cpu::z80::Bus;
use crate::arcade::donkey_kong::dk_vdp::DKVDP;
use crate::arcade::donkey_kong::dk_psg::DKPSG;
use crate::arcade::donkey_kong::dk_input::Joypad;
use std::cell::UnsafeCell;

pub struct DKBus {
    pub ram: Vec<u8>,
    pub vdp: UnsafeCell<DKVDP>,
    pub psg: UnsafeCell<DKPSG>,
    pub joypad: Joypad,
    last_master: u64,
    pub master: u64,
    open_bus: u8,
}

unsafe impl Send for DKBus {}
unsafe impl Sync for DKBus {}

impl DKBus {
    pub fn new() -> Self {
        let ram_size = 8192;
        Self {
            ram: vec![0; ram_size],
            vdp: UnsafeCell::new(DKVDP::new()),
            psg: UnsafeCell::new(DKPSG::new(44100.0)),
            joypad: Joypad::new(),
            last_master: 0, master: 0,
            open_bus: 0,
        }
    }
}

impl Bus for DKBus {
    fn read(&mut self, addr: u16) -> u8 {
        0
    }
    fn write(&mut self, addr: u16, val: u8) {

    }
    fn port_in(&mut self, port: u16) -> u8 {
        0
    }
    fn port_out(&mut self, port: u16, val: u8) {

    }
}