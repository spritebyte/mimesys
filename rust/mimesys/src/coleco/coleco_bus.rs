use crate::common::cpu::z80::Bus;
use crate::coleco::coleco_vdp::ColecoVDP;
use crate::coleco::coleco_psg::ColecoPSG;
use crate::coleco::coleco_joypad::Joypad;
use crate::coleco::coleco_cartridge::ColecoCartridge;
use std::cell::UnsafeCell;

const RAM_START: u16 = 0x6000;

pub struct ColecoBus {
    pub ram: Vec<u8>,
    pub cartridge: ColecoCartridge,
    pub vdp: UnsafeCell<ColecoVDP>,
    pub psg: UnsafeCell<ColecoPSG>,
    pub joypad: Joypad,
    last_master: u64,
    pub master: u64,
    open_bus: u8,

    pub boot_rom: Vec<u8>,
    pub boot_rom_mapped: bool,
}

unsafe impl Send for ColecoBus {}
unsafe impl Sync for ColecoBus {}

impl ColecoBus {
    pub fn new(cartridge: ColecoCartridge, run_boot_rom: bool, bios: &[u8]) -> Self {
        let ram_size = 8192;
        Self {
            cartridge,
            ram: vec![0; ram_size],
            joypad: Joypad::new(),
            vdp: UnsafeCell::new(ColecoVDP::new()),
            psg: UnsafeCell::new(ColecoPSG::new(44100.0)),
            last_master: 0, master: 0,
            open_bus: 0,
            boot_rom_mapped: run_boot_rom,
            boot_rom: bios.to_vec(),
        }
    }
}

impl Bus for ColecoBus {
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