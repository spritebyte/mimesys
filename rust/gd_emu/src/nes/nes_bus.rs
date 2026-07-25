use godot::global::godot_print;
use crate::common::bus::AddressBus;
//use crate::nes::mappers::Mapper;
use crate::nes::cartridge::Cartridge;
use crate::nes::nes_ppu::NesPPU;
use crate::nes::nes_apu::NesAPU;
use crate::common::m6502::M6502Cpu;
//use serde::{Serialize, Deserialize};
use std::cell::{UnsafeCell, Cell};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

pub struct NesBus {
    pub ram: [u8; 2048],
    pub cartridge: Cartridge,
    pub ppu: UnsafeCell<NesPPU>,
    pub apu: UnsafeCell<NesAPU>,
    // Input processing fields
    pub pad1_state: u8,
    pub pad1_shift_reg: Cell<u8>,
    pub pad_strobe: bool,
    pub dmc_stall_remaining: u16,
    pub dma_cycles_remaining: u16,
    pub dma_base_address: u16,
    pub dma_temp_buffer: u8,
    pub bus_available: bool,
    pub total_cpu_cycles: u64,
    pub total_ppu_dots: u64,
    pub accesses_this_cycle: u64,
    pub open_bus: Cell<u8>,
    pub dot_in_cycle: u32,
    pub nmi_line_snapshot: bool,
    pub irq_line_snapshot: bool,
    pub prev_nmi_line: bool,
    pub nmi_edge_latch: bool,
}

const INT_SAMPLE_DOT: u32 = 2;

unsafe impl Send for NesBus {}
unsafe impl Sync for NesBus {}

impl NesBus {
    pub fn new(cartridge: Cartridge, system_frame_ready: Arc<AtomicBool>) -> Self {
        Self {
            ram: [0; 2048],                         // Zero out the 2KB of CPU RAM on startup
            ppu: UnsafeCell::new(NesPPU::new(system_frame_ready)),   // Initialize a fresh PPU 
            apu: UnsafeCell::new(NesAPU::new()),
            cartridge,                              // Inject the cartridge we loaded
            pad1_state: 0,
            pad1_shift_reg: Cell::new(0),
            pad_strobe: false,
            bus_available: true,
            dmc_stall_remaining: 0,
            dma_cycles_remaining: 0, dma_base_address: 0, dma_temp_buffer: 0,
            total_cpu_cycles: 0,
            total_ppu_dots: 0,
            accesses_this_cycle: 0,
            open_bus: Cell::new(0),
            dot_in_cycle: 0,
            nmi_line_snapshot: false, irq_line_snapshot: false,
            prev_nmi_line: false, nmi_edge_latch: false,
        }
    }
    pub fn get_sram(&self) -> Option<&[u8]> { self.cartridge.get_sram() }
    pub fn load_sram(&mut self, data: &[u8]) { self.cartridge.load_sram(data); }
    pub fn is_sram_dirty(&self) -> bool { self.cartridge.is_sram_dirty() }
    pub fn clear_sram_dirty(&mut self) { self.cartridge.clear_sram_dirty(); }

    pub fn step_dma_one_cycle(&mut self, cpu: &mut M6502Cpu) {
        if self.dma_cycles_remaining == 0 {
            return;
        }
        self.begin_cpu_cycle();     // mapper, APU, total_cpu_cycles
        cpu.total_cycles += 1;

        // 1. Handle the initial initialization/halt cycles (cycles 514 or 513 down to 512)
        if self.dma_cycles_remaining > 512 {
            self.step_ppu_dots(3);
            self.dma_cycles_remaining -= 1;
            return;
        }

        // 2. Active copy window (512 cycles remaining down to 1)
        let dma_step = 512 - self.dma_cycles_remaining;
        let sprite_offset = dma_step / 2;

        if dma_step % 2 == 0 {
            // EVEN CYCLE: Read from CPU memory space
            let target_addr = self.dma_base_address + sprite_offset;
            self.dma_temp_buffer = self.read_byte(target_addr);
        } else {
            // ODD CYCLE: Write to PPU OAM data register directly ($2004)
            // This implicitly updates the PPU's OAM array in real system time
            self.write_byte(0x2004, self.dma_temp_buffer);
        }

        self.dma_cycles_remaining -= 1;

        // If that was the final cycle, release the CPU!
        if self.dma_cycles_remaining == 0 {
            self.bus_available = true;
        }
    }

    // deprecated
    pub fn step_one_cycle(&mut self) {
        self.cartridge.mapper_mut().step_cycles(1);
        let apu_ptr = self.apu.get();
        unsafe { (*apu_ptr).step_one_cycle(); }
        self.total_cpu_cycles += 1;
        let mapper = self.cartridge.mapper_mut();
        self.ppu.get_mut().step_one_cycle(mapper);
    }

    // deprecated
    pub fn step_remaining_ppu_cycles(&mut self) {
        let mapper = self.cartridge.mapper_mut();
        for _ in 0..2 {
            self.ppu.get_mut().step_one_cycle(mapper);
        }
    }

    // Function shouldn't be used for a debugger memory viewer as it isn't side effect free.
    fn peek_byte(&self, addr: u16) -> u8 {
        let value = match addr {
            0x0000..=0x1FFF => self.ram[(addr % 0x0800) as usize],
            0x2000..=0x3FFF => {
                let register = addr % 8;
                let ppu_mut = unsafe { &mut *self.ppu.get() };
//                println!("BUS read_byte: {:04X} reg:{ :02X} ", addr, register);
                let mapper_ref = self.cartridge.mapper_mut();
                ppu_mut.cpu_read_reg(mapper_ref, register)
            }
            0x4015 => {
                let val = unsafe { (*self.apu.get()).read_4015() | (self.open_bus.get() & 0x20) };
                godot_print!("READ $4015: {:02X}", val);
                val
            }
            0x4016 => {
                let shift_reg = self.pad1_shift_reg.get();
                let val = (shift_reg & 0x01) | (self.open_bus.get() & 0xE0);

                if !self.pad_strobe {
                    let next_shift = (shift_reg >> 1) | 0x80;
                    self.pad1_shift_reg.set(next_shift);
                }
                val
            }
            0x4020..=0xFFFF => self.cartridge.mapper().cpu_read(addr),
            _ => self.open_bus.get(),
        };
        self.open_bus.set(value);
        value
    }

    pub fn step_ppu_dots(&mut self, dots: u32) {
        for _ in 0..dots {
            let mapper = self.cartridge.mapper_mut();
            self.ppu.get_mut().step_one_cycle(mapper);
            self.dot_in_cycle += 1;
            self.total_ppu_dots += 1;

            if self.dot_in_cycle == INT_SAMPLE_DOT {
                self.nmi_line_snapshot = self.ppu.get_mut().is_nmi_line_asserted();
                self.irq_line_snapshot = self.cartridge.mapper().is_irq_asserted() || self.apu.get_mut().is_irq_asserted();
            }
        }
    }
}

impl AddressBus for NesBus {
    fn is_nmi_line_asserted(&mut self) -> bool {
         self.nmi_line_snapshot
    }

    fn is_nmi_enabled(&mut self) -> bool {
         self.ppu.get_mut().is_nmi_enabled()
    }

    fn total_cycles(&self) -> u64 {
        self.total_cpu_cycles
    }

    fn is_irq_line_asserted(&mut self) -> bool {
//        self.apu.get_mut().is_irq_asserted() || self.cartridge.mapper_mut().is_irq_asserted()
        self.irq_line_snapshot
    }

    fn read_byte(&mut self, addr: u16) -> u8 {
        self.step_ppu_dots(1);
        let value = self.peek_byte(addr);   // one definition of what a read does
        self.step_ppu_dots(2);
        self.accesses_this_cycle += 1;
        value
    }

    fn write_byte(&mut self, addr: u16, value: u8) {
        self.step_ppu_dots(1);
        match addr {
            0x0000..=0x1FFF => self.ram[(addr % 0x0800) as usize] = value,
            0x2000..=0x3FFF => {
                let register = addr % 8;
                let mapper_ref = self.cartridge.mapper_mut();

                let ppu_mut = self.ppu.get_mut();
//                godot_print!("calling cpu_write_reg. Reg={} value={}", register, value);
                ppu_mut.cpu_write_reg(mapper_ref, register, value);
            }
            0x4014 => {
                self.dma_base_address = (value as u16) << 8;
                let cycles_to_burn = 513 + ((self.total_cpu_cycles) % 2);
                self.dma_cycles_remaining = cycles_to_burn as u16;
                self.bus_available = false;
            }
            0x4016 => {
                self.pad_strobe = (value & 0x01) == 0x01;
                if self.pad_strobe {
                    self.pad1_shift_reg.set(self.pad1_state);
                }
            }
            0x4000..=0x401F => { 
                self.apu.get_mut().write_reg(addr, value);
             }
            0x4020..=0xFFFF => {
                if addr == 0x5104 {
                    let ppu = self.ppu.get_mut();
                    godot_print!("Scanline={}, cycle={}", ppu.scanline, ppu.cycle);
                }
                if addr == 0x5104 || (addr >= 0x5126 && addr <= 0x512A) {
                    let ppu = self.ppu.get_mut();
                    godot_print!("Current scanline={} cycle={}", ppu.scanline, ppu.cycle);
                } 
                self.cartridge.mapper_mut().cpu_write(addr, value); }
        }
        self.accesses_this_cycle += 1;
        self.step_ppu_dots(2);
        self.open_bus.set(value);
    }

    fn step_cycles(&mut self, cycles: u64) {
        let mut step_cycles = cycles;
        while step_cycles > 0 {
            self.cartridge.mapper_mut().step_cycles(1);
            let apu_ptr = self.apu.get();
            unsafe { (*apu_ptr).step_one_cycle(); }
            step_cycles -= 1;
            self.total_cpu_cycles += 1;
            for _ in 0..3 {
                let mapper = self.cartridge.mapper_mut();
                self.ppu.get_mut().step_one_cycle(mapper);
                self.total_ppu_dots += 1;
            }
        }
    }

    fn begin_cpu_cycle(&mut self) {
        self.dot_in_cycle = 0;
        self.cartridge.mapper_mut().step_cycles(1);
        let apu_ptr = self.apu.get();
        unsafe { (*apu_ptr).step_one_cycle(); }
        self.total_cpu_cycles += 1;
//        godot_print!("begin_cpu_cycle: total cpu now {}", self.total_cpu_cycles);
    }
}