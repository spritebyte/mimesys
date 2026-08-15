use crate::common::timed::Timed;
use crate::gameboy::gb_mbc::Mbc;
use crate::gameboy::gb_cartridge::GbCartridge;
use crate::gameboy::gb_ppu::GbPPU;
use crate::gameboy::gb_apu::GbAPU;
use crate::gameboy::gb_timer::GbTimer;
use std::cell::{UnsafeCell, Cell};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

pub trait Bus {
    fn read(&mut self, addr: u16) -> u8;
    fn write(&mut self, addr: u16, value: u8);
    fn peek(&self, addr: u16) -> u8;
    fn irq_pending(&self) -> u8;
    fn ack_irq(&mut self, which: u8);
}

pub struct GameBoyBus {
    pub ram: [u8; 2048],
    pub cartridge: GbCartridge,
    pub ppu: UnsafeCell<GbPPU>,
    pub apu: UnsafeCell<GbAPU>,
    pub timer: UnsafeCell<GbTimer>,
    pub ie: u8,
    pub iflags: u8,
    // Input processing fields
    pub pad1_state: u8,
    pub pad1_shift_reg: Cell<u8>,
    pub pad_strobe: bool,
    serial_data_buffer: u8,
    serial_control: u8,
    last_master: u64,
    pub master: u64,                   // total t-cycles
    is_double_speed_active: bool,
    open_bus: u8,
}

unsafe impl Send for GameBoyBus {}
unsafe impl Sync for GameBoyBus {}

impl GameBoyBus {
    pub fn new(cartridge: GbCartridge, system_frame_ready: Arc<AtomicBool>) -> Self {
        Self {
            ram: [0; 2048],                         // Zero out the 2KB of CPU RAM on startup
            ppu: UnsafeCell::new(GbPPU::new(system_frame_ready)),   // Initialize a fresh PPU 
            apu: UnsafeCell::new(GbAPU::new()),
            timer: UnsafeCell::new(GbTimer::new()),
            cartridge,
            ie: 0, iflags: 0,
            last_master: 0,
            pad1_state: 0,
            pad1_shift_reg: Cell::new(0),
            pad_strobe: false,
            serial_data_buffer: 0,
            serial_control: 0,
            master: 0,
            is_double_speed_active: false,
            open_bus: 0,
        }
    }
    pub fn get_sram(&self) -> Option<&[u8]> { self.cartridge.get_sram() }
    pub fn load_sram(&mut self, data: &[u8]) { self.cartridge.load_sram(data); }
    pub fn is_sram_dirty(&self) -> bool { self.cartridge.is_sram_dirty() }
    pub fn clear_sram_dirty(&mut self) { self.cartridge.clear_sram_dirty(); }

    pub fn cycle_len(&self) -> u64 {
        if self.is_double_speed_active() {
            2
        } else {
            4
        }
    }

    pub fn is_double_speed_active(&self) -> bool {
        self.is_double_speed_active
    }

    pub fn idle_cycle(&mut self) {
        self.master += self.cycle_len();
        self.run_until(self.master);
    }

    fn read_inner(&mut self, addr: u16) -> u8 {
        match addr {
            0..=0x7FFF|0xA000..=0xBFFF => {
                let value = unsafe { (*self.cartridge.mbc.get()).read(addr) };
                return value;
            }
            _ => { return 0; }
        }
    }

    fn write_inner(&mut self, addr: u16, value: u8) {
        match addr {
            0..=0x7FFF|0xA000..=0xBFFF => {
                unsafe { (*self.cartridge.mbc.get()).write(addr, value); }
            }
            0xFF01 => {
                self.serial_data_buffer = value;
            }
            0xFF02 => {
                self.serial_control = value;
                if (value & 0x80) != 0 {
                    // Bit 7 being set means a transfer was requested!
                    // This is the hook where future Network/Link Cable component 
                    // will intercept execution and talk to the other emulator instance.
//                    self.link_cable.initiate_transfer(self.serial_data_buffer, value);
                }
            }
            0xFFFF => {
                self.ie = value;
            }
            _ => { }
        }
    }

    fn access_offset(&self) -> u64 {
        0
    }
}

impl Timed for GameBoyBus {
    fn run_until(&mut self, target_master: u64) {
        let gb_ppu = self.ppu.get_mut();
        gb_ppu.run_until(target_master);

        let gb_apu = self.apu.get_mut();
        gb_apu.run_until(target_master);

        let gb_timer = self.timer.get_mut();
        gb_timer.run_until(target_master);
        self.last_master = target_master;
        self.iflags |= gb_ppu.take_irqs();
        self.iflags |= gb_timer.take_irqs();
    }

    fn sync_point(&self) -> u64 {
        self.last_master
    }
}

impl Bus for GameBoyBus {
    fn irq_pending(&self) -> u8 {
        self.ie & self.iflags & 0x1F
    }

    fn ack_irq(&mut self, which: u8) {

    }

    fn read(&mut self, addr: u16) -> u8 {
        self.master += self.access_offset();   // partway into the cycle
        self.run_until(self.master);           // components catch up to here
        let value = self.read_inner(addr);     // the access sees state AT this point
        self.master += self.cycle_len() - self.access_offset();
        self.run_until(self.master);           // finish out the M-cycle
        value
    }

    fn peek(&self, addr: u16) -> u8 {
        0
    }

    fn write(&mut self, addr: u16, value: u8) {
        self.master += self.access_offset();
        self.run_until(self.master);
        self.write_inner(addr, value);
        self.master += self.cycle_len() - self.access_offset();
        self.run_until(self.master);
    }
}