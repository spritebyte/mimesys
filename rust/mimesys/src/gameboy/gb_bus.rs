use crate::common::timed::Timed;
use crate::gameboy::gb_mbc::Mbc;
use crate::gameboy::gb_cartridge::GbCartridge;
use crate::gameboy::gb_palette::DmgPaletteSet;
use crate::gameboy::gb_common::GbVariant;
use crate::gameboy::gb_ppu::GbPPU;
use crate::gameboy::gb_apu::GbAPU;
use crate::gameboy::gb_joypad::Joypad;
use crate::gameboy::gb_dma::DmaState;
use crate::gameboy::gb_timer::GbTimer;
use std::cell::{UnsafeCell, Cell};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

pub trait Bus {
    fn read(&mut self, addr: u16) -> u8;
    fn write(&mut self, addr: u16, value: u8);
    fn peek(&mut self, addr: u16) -> u8;
    fn irq_pending(&self) -> u8;
    fn reset_div(&mut self) { }
    fn ack_irq(&mut self, which: u8);
    fn idle_cycle(&mut self);
    fn perform_speed_switch(&mut self);
    fn is_speed_switch_prepared(&self) -> bool {
        false
    }
    fn joypad_low_transition(&mut self) -> bool { false }
    fn get_dma_stall_cycles(&self) -> u64 { 0 }
    fn set_dma_stall_cycles(&mut self, value: u64) { }
}

pub struct GameBoyBus {
    pub variant: GbVariant,
    pub ram: Vec<u8>,
    pub hram: [u8;0x80],
    pub cartridge: GbCartridge,
    pub ppu: UnsafeCell<GbPPU>,
    pub apu: UnsafeCell<GbAPU>,
    pub timer: UnsafeCell<GbTimer>,
    pub ie: u8,
    pub iflags: u8,
    pub dma: DmaState,
    pub joypad: Joypad,
    serial_data_buffer: u8,
    serial_control: u8,
    last_master: u64,
    pub master: u64,                   // total t-cycles
    double_speed: bool,
    open_bus: u8,

    pub svbk: u8,
    pub key0: u8,
    pub key1: u8,
    pub hdma_src: u16,
    pub hdma_dst: u16,
    pub hdma_active: bool,
    pub hdma_len: u16,
    pub boot_rom: Vec<u8>,
    pub boot_rom_mapped: bool,
    pub dma_stall_m_cycles: u64,
}

unsafe impl Send for GameBoyBus {}
unsafe impl Sync for GameBoyBus {}

impl GameBoyBus {
    pub fn new(variant: GbVariant, cartridge: GbCartridge, system_frame_ready: Arc<AtomicBool>, selected_palette: DmgPaletteSet, run_boot_rom: bool, boot_rom_data: &[u8]) -> Self {
        let ram_size: usize = match variant {
            GbVariant::Cgb => 32768,
            _=> 8192,
        };

        let boot_mapped = run_boot_rom && !boot_rom_data.is_empty() && variant == GbVariant::Cgb;
        Self {
            variant,
            joypad: Joypad::new(),
            boot_rom: boot_rom_data.to_vec(),
            boot_rom_mapped: boot_mapped,
            ram: vec![0; ram_size],
            hram: [0; 0x80],
            ppu: UnsafeCell::new(GbPPU::new(variant, system_frame_ready, selected_palette)),   // Initialize a fresh PPU 
            apu: UnsafeCell::new(GbAPU::new(44100.0)),
            timer: UnsafeCell::new(GbTimer::new()),
            dma: DmaState::new(),
            cartridge,
            ie: 0, iflags: 0,
            last_master: 0,
            serial_data_buffer: 0xFF,
            serial_control: 0,
            master: 0,
            double_speed: false,
            open_bus: 0,
            dma_stall_m_cycles: 0,
            // CGB Fields
            svbk: 0x01, 
            key0: 0, key1: 0,
            hdma_src: 0,
            hdma_dst: 0,
            hdma_active: false,
            hdma_len: 0,
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

    fn _read_io(&mut self, addr: u16) -> u8 {
        match addr {
            0xFF00 => self.joypad.read_p1(),
            0xFF01 => self.serial_data_buffer,
            0xFF02 => self.serial_control | 0x7E,
            0xFF04..=0xFF07 => unsafe { (*self.timer.get()).read(addr) }
            0xFF0F => self.iflags | 0xE0,
            // APU Sound registers ($FF10-$FF3F)
            0xFF10..=0xFF3F => unsafe { (*self.apu.get()).read_register(addr) },
            0xFF40..=0xFF4B | 0xFF4F | 0xFF68..=0xFF6B => unsafe { (*self.ppu.get()).read_register(addr) },
            0xFF4C => {
                println!("Read from 0xFF4C returning {:02X}", self.key0);
                self.key0
            },
            0xFF4D => self.read_key1(),
            0xFF50 => if self.boot_rom_mapped { 0x00 } else { 0x01 },
            0xFF55 => {
                if !self.hdma_active {
                    0xFF
                } else {
                    (self.hdma_len.saturating_sub(1) as u8) & 0x7F
                }
            },
            0xFF70 => self.svbk | 0xF8,
            _ => self.open_bus,
        }
    }

    fn _write_io(&mut self, addr: u16, value: u8) {
        match addr {
            0xFF00 => self.joypad.write_p1(value, &mut self.iflags),
            0xFF01 => self.serial_data_buffer = value,
            0xFF02 => {
                self.serial_control = value;
                if (value & 0x80) != 0 {
                    self.serial_control &= !0x80;
                    self.serial_data_buffer = 0xFF;
                    self.iflags |= 0x08;
                }
            },
            0xFF04..=0xFF07 => unsafe { (*self.timer.get()).write(addr, value) },
            0xFF0F => self.iflags = value & 0x1F,
            // APU range $FF10-$FF3F
            0xFF10..=0xFF3F => unsafe { (*self.apu.get()).write_register(addr, value) },
            0xFF46 => self.dma.start(value, self.master),
            0xFF40..=0xFF4B | 0xFF4F | 0xFF68..=0xFF6B => unsafe { (*self.ppu.get()).write_register(addr, value) },
            0xFF4C => {
                 if self.boot_rom_mapped { 
                    self.key0 = value;
                    println!("Boot rom wrote {:02X} to $FF4C.", value);
                 }
            },
            0xFF4D => self.write_key1(value),
            0xFF50 => {
                if value != 0 {
                    self.boot_rom_mapped = false;
                }
                println!("Boot rom wrote to 0xFF50: {:02X}", value);
            }
            0xFF51 => self.hdma_src = (self.hdma_src & 0x00FF) | ((value as u16) << 8),
            0xFF52 => self.hdma_src = (self.hdma_src & 0xFF00) | ((value & 0xF0) as u16),
            0xFF53 => {
                let high = ((value & 0x1F) as u16) << 8;
                self.hdma_dst = 0x8000 | high | (self.hdma_dst & 0x00F0);
            },
            0xFF54 => { 
                let low = (value & 0xF0) as u16;
                self.hdma_dst = 0x8000 | (self.hdma_dst & 0x1F00) | low;
            },
            0xFF55 => {
                if self.hdma_active && (value & 0x80) == 0 {
                    self.hdma_active = false;
                } else {
                    let blocks = ((value & 0x7F) as u16) + 1;
                    let is_hdma = (value & 0x80) != 0;

                    if is_hdma {
                        self.hdma_len = blocks;
                        self.hdma_active = true;
                    } else {
                        self.perform_gdma(blocks);
                    }
                }
            },
            0xFF70 => self.svbk = value & 0x07,
            _ => { }
        }
    }

    fn read_raw(&mut self, addr: u16) -> u8 {
        if self.boot_rom_mapped {
            if let ref boot = self.boot_rom {
                match self.variant {
                    GbVariant::Cgb => {
                        if (addr <= 0x00FF || (0x0200..=0x08FF).contains(&addr)) && (addr as usize) < boot.len() {
                            return boot[addr as usize];
                        }
                    }
                    _ => {
                        if addr <= 0x00FF && (addr as usize) < boot.len() {
                            return boot[addr as usize];
                        }
                    }
                }
            }
        }

        match addr {
            0x0000..=0x7FFF | 0xA000..=0xBFFF => unsafe { (*self.cartridge.mbc.get()).read(addr) },
            0x8000..=0x9FFF => unsafe {
                let bank = if self.variant == GbVariant::Cgb { ((*self.ppu.get()).vbk & 0x01) as usize } else { 0 };
                let offset = (addr & 0x1FFF) as usize + (bank * 0x2000);
                (&(*self.ppu.get()).vram)[offset]
            },
            0xC000..=0xFDFF => self.ram[self.get_wram_offset(addr)],
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
            0x8000..=0x9FFF => unsafe {
                let bank = if self.variant == GbVariant::Cgb { ((*self.ppu.get()).vbk & 0x01) as usize } else { 0 };
                let offset = (addr & 0x1FFF) as usize + (bank * 0x2000);
                (&mut (*self.ppu.get()).vram)[offset] = value;
            },
            0xC000..=0xFDFF => {
                let offset = self.get_wram_offset(addr);
                self.ram[offset] = value;
            },
            0xFE00..=0xFE9F => unsafe { (*self.ppu.get()).oam[(addr & 0xFF) as usize] = value },
            0xFF00..=0xFF7F => self._write_io(addr, value),
            0xFF80..=0xFFFE => self.hram[(addr & 0x7F) as usize] = value,
            0xFFFF => self.ie = value,
            _ => { }
        }
    }

    fn read_inner(&mut self, addr: u16) -> u8 {
 /*       if self.dma.active {
            if addr >= 0xFF80 && addr <= 0xFFFE {
                return self.hram[(addr - 0xFF80) as usize];
            }
            if addr == 0xFF46 {
                return (self.dma.source_base >> 8) as u8;
            }
            println!("DMA BLOCK read {:04X} -> FF (dma active)", addr);
            return 0xFF;
        } */

        self.read_raw(addr)
    }

    fn write_inner(&mut self, addr: u16, value: u8) {
        /*if self.dma.active {
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
        } */
        self.write_raw(addr, value);
    }

    fn get_wram_offset(&self, addr: u16) -> usize {
        let offset = (addr & 0x1FFF) as usize;
        if offset < 0x1000 {
            // 0xC000..0xCFFF and $E000..$EFFF -> always Bank 0
            offset
        } else {
            // 0xD000..0xDFFF and $F000..$FDFF -> Switchable Bank 1..7 (SVBK)
            // Note: SVBK values of 0 or 1 both map to Bank 1
            let raw_bank = (self.svbk & 0x07) as usize;
            let bank = if raw_bank == 0 { 1 } else { raw_bank };
            (bank * 0x1000) + (offset & 0x0FFF)
        }
    }
    
    // Immediate GDMA Transfer
    fn perform_gdma(&mut self, blocks: u16) {
        let total_bytes = blocks * 16;

        for _ in 0..total_bytes {
            let byte = self.read_raw(self.hdma_src);
            self.write_raw(self.hdma_dst, byte);

            // Source advances continuously
            self.hdma_src = self.hdma_src.wrapping_add(1);

            // Destination advances but stays bound within VRAM ($8000..=$9FFF)
            let new_dst_offset = ((self.hdma_dst & 0x1FFF) + 1) & 0x1FFF;
            self.hdma_dst = 0x8000 | new_dst_offset;
        }

        self.hdma_active = false;
        self.hdma_len = 0;
        self.dma_stall_m_cycles = (blocks as u64) * 32;
    }

    pub fn tick_hdma_block(&mut self) {
        if !self.hdma_active {
            return;
        }

        for _ in 0..16 {
            let byte = self.read_raw(self.hdma_src);
            self.write_raw(self.hdma_dst, byte);

            self.hdma_src = self.hdma_src.wrapping_add(1);
        
            let new_dst = ((self.hdma_dst & 0x1FFF) + 1) & 0x1FFF;
            self.hdma_dst = 0x8000 | new_dst;
        }

        self.hdma_len = self.hdma_len.saturating_sub(1);
        if self.hdma_len == 0 {
            self.hdma_active = false;
        }

        self.dma_stall_m_cycles += 8;
    }

    pub fn read_key1(&self) -> u8 {
        self.key1 | 0x7E
    }

    pub fn write_key1(&mut self, value: u8) {
        self.key1 = (self.key1 & 0x80) | (value & 0x01);
    }
}

impl Timed for GameBoyBus {
    fn run_until(&mut self, target_master: u64) {
        unsafe {
            (*self.ppu.get()).run_until(target_master);
            if (*self.ppu.get()).take_hdma_pending() {
                self.tick_hdma_block();
            }
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

    fn reset_div(&mut self) {
        unsafe {
            (*self.timer.get()).reset_div();
        }
    }

    fn ack_irq(&mut self, which: u8) {
        self.iflags &= !(1 << which);
    }

    fn get_dma_stall_cycles(&self) -> u64 {
        self.dma_stall_m_cycles
    }

    fn set_dma_stall_cycles(&mut self, value:u64) {
        self.dma_stall_m_cycles = value;
    }

    fn perform_speed_switch(&mut self) {
        self.key1 ^= 0x80;
        self.key1 &= !0x01;
        self.double_speed = (self.key1 & 0x80) != 0;
//        if (self.key1_prepare & 0x01) != 0 {
//            self.double_speed = !self.double_speed;
//            self.key1_prepare = 0;
//        }
    }

    fn joypad_low_transition(&mut self) -> bool {
        if self.joypad.pending_low_transition {
            self.joypad.pending_low_transition = false;
            true
        } else {
            false
        }
    }

    fn is_speed_switch_prepared(&self) -> bool {
        (self.key1 & 0x01) != 0
    }

    fn read(&mut self, addr: u16) -> u8 {
        self.master += self.access_offset();   // partway into the cycle
        self.run_until(self.master);           // components catch up to here
        let value = self.read_inner(addr);     // the access sees state AT this point
//        if addr == 0xC671 || addr == 0xC672 {
//            println!("read from {:04X} returned val={:02X}", addr, value);
//            panic!("C671 or C672 write");
//        }
        self.master += self.cycle_len() - self.access_offset();
        self.run_until(self.master);           // finish out the M-cycle
//        println!("bus read. total cycles now {}", self.master);
        value
    }

    fn peek(&mut self, addr: u16) -> u8 {
        self.read_inner(addr)
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