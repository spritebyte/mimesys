use crate::common::timed::Timed;
use crate::gameboy::gb_mbc::Mbc;
use crate::gameboy::gb_cartridge::GbCartridge;
use crate::gameboy::gb_palette::DmgPaletteSet;
use crate::gameboy::gb_common::GbVariant;
use crate::gameboy::gb_ppu::GbPPU;
use crate::gameboy::gb_apu::GbAPU;
use crate::gameboy::gb_dma::DmaState;
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
    fn idle_cycle(&mut self);
    fn reset_div(&mut self);
    fn perform_speed_switch(&mut self);
}

pub struct GameBoyBus {
    pub ram: Vec<u8>,
    pub hram: [u8;0x80],
    pub cartridge: GbCartridge,
    pub ppu: UnsafeCell<GbPPU>,
    pub apu: UnsafeCell<GbAPU>,
    pub timer: UnsafeCell<GbTimer>,
    pub ie: u8,
    pub iflags: u8,
    pub dma: DmaState,
    // Input processing fields
    pub pad1_state: u8,
    pub pad1_shift_reg: Cell<u8>,
    pub pad_strobe: bool,
    joyp_reg: u8,
    serial_data_buffer: u8,
    serial_control: u8,
    last_master: u64,
    pub master: u64,                   // total t-cycles
    double_speed: bool,
    open_bus: u8,
}

unsafe impl Send for GameBoyBus {}
unsafe impl Sync for GameBoyBus {}

impl GameBoyBus {
    pub fn new(variant: GbVariant, cartridge: GbCartridge, system_frame_ready: Arc<AtomicBool>, selected_palette: DmgPaletteSet) -> Self {
        let ram_size: usize = match variant {
            GbVariant::Cgb => 32768,
            _=> 8192,
        };
        Self {
            ram: vec![0; ram_size],
            hram: [0; 0x80],
            ppu: UnsafeCell::new(GbPPU::new(variant, system_frame_ready, selected_palette)),   // Initialize a fresh PPU 
            apu: UnsafeCell::new(GbAPU::new()),
            timer: UnsafeCell::new(GbTimer::new()),
            dma: DmaState::new(),
            cartridge,
            ie: 0, iflags: 0,
            last_master: 0,
            pad1_state: 0,
            pad1_shift_reg: Cell::new(0),
            pad_strobe: false,
            joyp_reg: 0x30,
            serial_data_buffer: 0,
            serial_control: 0,
            master: 0,
            double_speed: false,
            open_bus: 0,
        }
    }
    pub fn get_sram(&self) -> Option<&[u8]> { self.cartridge.get_sram() }
    pub fn load_sram(&mut self, data: &[u8]) { self.cartridge.load_sram(data); }
    pub fn is_sram_dirty(&self) -> bool { self.cartridge.is_sram_dirty() }
    pub fn clear_sram_dirty(&mut self) { self.cartridge.clear_sram_dirty(); }
    pub fn cycle_len(&self) -> u64 {
        if self.double_speed {
            2
        } else {
            4
        }
    }

    fn access_offset(&self) -> u64 {
        self.cycle_len() / 2
    }

    pub fn is_double_speed_active(&self) -> bool {
        self.double_speed
    }

    pub fn read_joyp(&self) -> u8 {
        // Bits 4 & 5 store the selection lines set by CPU writes to $FF00
        // Bits 6 & 7 are unused and always read as 1 (0xC0)
        let select = self.joyp_reg & 0x30;
        let mut result = select | 0xC0;

        let mut button_bits = 0x0F; // Default: 0xF (no buttons pressed, active-low)

        // Bit 4 = 0: Select Direction Buttons
        if (select & 0x10) == 0 {
            let directions = (self.pad1_state >> 4) & 0x0F;
            button_bits &= !directions;
        }

        // Bit 5 = 0: Select Action Buttons
        if (select & 0x20) == 0 {
            let actions = self.pad1_state & 0x0F;
            button_bits &= !actions;
        }

        result | button_bits
    }

    fn _read_io(&mut self, addr: u16) -> u8 {
        match addr {
            0xFF00 => {
                return self.read_joyp();
            },
            0xFF01 => self.serial_data_buffer,
            0xFF02 => self.serial_control | 0x7E,
            0xFF04..=0xFF07 => unsafe { (*self.timer.get()).read(addr) }
            0xFF0F => self.iflags | 0xE0,
            0xFF40..=0xFF4B | 0xFF4F => unsafe { (*self.ppu.get()).read_register(addr) },
            _ => self.open_bus,
        }
    }

    fn _write_io(&mut self, addr: u16, value: u8) {
        match addr {
            0xFF00 => {
                self.joyp_reg = value & 0x30;
                // write joypad
            },
            0xFF01 => { self.serial_data_buffer = value; },
            0xFF02 => {
                self.serial_control = value;
                // serial_transfer_pending. set cycles to 0
            },
            0xFF04..=0xFF07 => {
                unsafe { (*self.timer.get()).write(addr, value) }
            },
            0xFF0F => self.iflags = value & 0x1F,
            // APU range $FF10-$FF3F
            // Wave RAM  $FF30-$FF3F
            0xFF46 => self.dma.start(value, self.master),
            0xFF40..=0xFF4B | 0xFF4F => unsafe { (*self.ppu.get()).write_register(addr, value) },
            _ => { }
        }
    }

    fn read_raw(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x7FFF | 0xA000..=0xBFFF => unsafe { (*self.cartridge.mbc.get()).read(addr) },
            0x8000..=0x9FFF => unsafe { (&(*self.ppu.get()).vram)[(addr & 0x1FFF) as usize] },
            0xC000..=0xFDFF => self.ram[(addr & 0x1FFF) as usize], // WRAM & Echo RAM
            0xFE00..=0xFE9F => unsafe { (*self.ppu.get()).oam[(addr & 0xFF) as usize] },
            0xFF00..=0xFF7F => return self._read_io(addr),
            0xFF80..=0xFFFE => self.hram[(addr & 0x7F) as usize],
            0xFFFF => self.ie,
            _ => 0xFF
        }
    }

    fn write_raw(&mut self, addr: u16, value: u8) {
        match addr {
            0x0000..=0x7FFF|0xA000..=0xBFFF => unsafe { (*self.cartridge.mbc.get()).write(addr, value); }
            0x8000..=0x9FFF => unsafe { (&mut (*self.ppu.get()).vram)[(addr & 0x1FFF) as usize] = value },
            0xC000..=0xFDFF => self.ram[(addr & 0x1FFF) as usize] = value,
            0xFE00..=0xFE9F => unsafe { (*self.ppu.get()).oam[(addr & 0xFF) as usize] = value },
            0xFF00..=0xFF7F => self._write_io(addr, value),
            0xFF80..=0xFFFE => self.hram[(addr & 0x7F) as usize] = value,
            0xFFFF => self.ie = value,
            _ => { }
        }
    }

    fn read_inner(&mut self, addr: u16) -> u8 {
        if self.dma.active {
            if addr >= 0xFF80 && addr <= 0xFFFE {
                return self.hram[(addr - 0xFF80) as usize];
            }
            if addr == 0xFF46 {
                return (self.dma.source_base >> 8) as u8;
            }
            return 0xFF;
        }

        self.read_raw(addr)
    }

    fn write_inner(&mut self, addr: u16, value: u8) {
        if self.dma.active {
            // HRAM is always accessible
            if addr >= 0xFF80 && addr <= 0xFFFE {
                self.hram[(addr - 0xFF80) as usize] = value;
                return;
            }
            // Writing to $FF46 while DMA is active restarts the transfer
            if addr == 0xFF46 {
                self.dma.start(value, self.master);
                return;
            }
            // All other CPU writes are dropped while DMA is active
            return;     
        }
        self.write_raw(addr, value);
    }
}

impl Timed for GameBoyBus {
    fn run_until(&mut self, target_master: u64) {
        unsafe {
            (*self.ppu.get()).run_until(target_master);
            (*self.apu.get()).run_until(target_master);
            (*self.timer.get()).run_until(target_master);
        }

        while (self.dma.active || self.dma.delay_m_cycles > 0) && self.dma.next_tick_master <= target_master {
            if let Some((src_addr, oam_offset)) = self.dma.tick_m_cycle() {
                let byte = self.read_raw(src_addr);
                unsafe {
                    (*self.ppu.get()).oam[oam_offset] = byte;
                }
            }
            self.dma.next_tick_master += 4;
        }

        self.last_master = target_master;
        unsafe {
            self.iflags |= (*self.ppu.get()).take_irqs();
            self.iflags |= (*self.timer.get()).take_irqs();
        }
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
        self.iflags &= !(1 << which);
    }
    fn reset_div(&mut self) { }
    fn perform_speed_switch(&mut self) { self.double_speed = !self.double_speed; }
    fn read(&mut self, addr: u16) -> u8 {
        self.master += self.access_offset();   // partway into the cycle
        self.run_until(self.master);           // components catch up to here
        let value = self.read_inner(addr);     // the access sees state AT this point
        if addr == 0xC671 || addr == 0xC672 {
            println!("read from {:04X} returned val={:02X}", addr, value);
//            panic!("C671 or C672 write");
        }
        self.master += self.cycle_len() - self.access_offset();
        self.run_until(self.master);           // finish out the M-cycle
//        println!("bus read. total cycles now {}", self.master);
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

    fn idle_cycle(&mut self) {
        self.master += self.cycle_len();
        self.run_until(self.master);
    }
}