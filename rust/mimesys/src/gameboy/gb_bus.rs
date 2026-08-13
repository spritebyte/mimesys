use crate::common::timed::Timed;
use crate::gameboy::gb_mbc::Mbc;
use crate::gameboy::gb_cartridge::GbCartridge;
use crate::gameboy::gb_ppu::GbPPU;
use crate::gameboy::gb_apu::GbAPU;
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
    pub ie: u8,
    pub iflags: u8,
    // Input processing fields
    pub pad1_state: u8,
    pub pad1_shift_reg: Cell<u8>,
    pub pad_strobe: bool,
    serial_data_buffer: u8,
    serial_control: u8,
    last_master: u64,
    div: u64,
    total_t_cycles: u64,
    is_double_speed_active: bool,
}

unsafe impl Send for GameBoyBus {}
unsafe impl Sync for GameBoyBus {}

impl GameBoyBus {
    pub fn new(cartridge: GbCartridge, system_frame_ready: Arc<AtomicBool>) -> Self {
        Self {
            ram: [0; 2048],                         // Zero out the 2KB of CPU RAM on startup
            ppu: UnsafeCell::new(GbPPU::new(system_frame_ready)),   // Initialize a fresh PPU 
            apu: UnsafeCell::new(GbAPU::new()),
            cartridge,
            ie: 0, iflags: 0,
            div: 2,
            last_master: 0,
            pad1_state: 0,
            pad1_shift_reg: Cell::new(0),
            pad_strobe: false,
            serial_data_buffer: 0,
            serial_control: 0,
            total_t_cycles: 0,
            is_double_speed_active: false,
        }
    }
    pub fn get_sram(&self) -> Option<&[u8]> { self.cartridge.get_sram() }
    pub fn load_sram(&mut self, data: &[u8]) { self.cartridge.load_sram(data); }
    pub fn is_sram_dirty(&self) -> bool { self.cartridge.is_sram_dirty() }
    pub fn clear_sram_dirty(&mut self) { self.cartridge.clear_sram_dirty(); }
    fn tick(&mut self) {
        let cycles_passed = if self.is_double_speed_active() {
            2 // In CGB Double Speed, a CPU M-cycle takes only 2 T-cycles
        } else {
            4 // In normal speed, a CPU M-cycle takes 4 T-cycles
        };

        // 3. Accumulate global time and tick the rest of the physical hardware
        self.total_t_cycles += cycles_passed;
    
//        self.timer.tick(cycles_passed);
//        self.ppu.tick(cycles_passed);
//        self.apu.tick(cycles_passed);
    }

    pub fn is_double_speed_active(&self) -> bool {
        self.is_double_speed_active
    }
}

impl Timed for GameBoyBus {
    fn run_until(&mut self, target_master: u64) {
        let target_tick = target_master / self.div;
        while self.total_t_cycles < target_tick {
            self.total_t_cycles = self.total_t_cycles.wrapping_add(1);
        }
        self.last_master = target_master;
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
        0
    }

    fn peek(&self, addr: u16) -> u8 {
        0
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
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
            _ => {}
        }
    }
}