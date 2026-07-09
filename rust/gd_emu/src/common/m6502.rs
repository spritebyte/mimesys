const STACK_BASE: u16 = 0x100;

use crate::common::bus::AddressBus;
use serde::{Serialize, Deserialize};

#[cfg(test)]
macro_rules! emu_print {
    ($($t:tt)*) => {
        println!($($t)*)
    };
}

#[cfg(not(test))]
macro_rules! emu_print {
    ($($t:tt)*) => {
        godot::global::godot_print!($($t)*)
    };
}

#[derive(Default, Clone, Copy, Serialize, Deserialize)]
pub struct StatusFlags {
    pub negative: bool,          // Bit 7 (N)
    pub overflow: bool,          // Bit 6 (V)
    // Bit 5 is unused on hardware (always pushed as 1 to stack)
    // Bit 4 is the Break flag (only exists on stack)
    pub decimal: bool,           // Bit 3 (D)
    pub interrupt_disable: bool, // Bit 2 (I)
    pub zero: bool,              // Bit 1 (Z)
    pub carry: bool,             // Bit 0 (C)
}

impl StatusFlags {
    pub fn to_u8(&self, is_instruction: bool) -> u8 {
        let mut byte = 0x00;
        if self.negative          { byte |= 0x80; }
        if self.overflow          { byte |= 0x40; }
        
        // Bit 5: Always 1 when pushed to the stack on a real 6502
        byte |= 0x20; 
        
        // Bit 4 (B flag): 1 if pushed by PHP or BRK; 0 if pushed by hardware IRQ/NMI
        if is_instruction         { byte |= 0x10; }
        
        if self.decimal           { byte |= 0x08; }
        if self.interrupt_disable { byte |= 0x04; }
        if self.zero              { byte |= 0x02; }
        if self.carry             { byte |= 0x01; }
        byte
    }

    /// Unpack a byte pulled from the stack via PLP or RTI
    pub fn from_u8(&mut self, byte: u8) {
        self.negative          = (byte & 0x80) != 0;
        self.overflow          = (byte & 0x40) != 0;
        // Bits 4 and 5 are ignored when pulling from the stack
        self.decimal           = (byte & 0x08) != 0;
        self.interrupt_disable = (byte & 0x04) != 0;
        self.zero              = (byte & 0x02) != 0;
        self.carry             = (byte & 0x01) != 0;
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum InterruptType {
    Brk,
    Irq,
    Nmi,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Operation {
    Adc, And, Asl, Bcc, Bcs, Beq, Bit, Bmi, Bne, Bpl, Brk, Bvc, Bvs, Clc, Cld, Cli, Clv, Cmp,
    Cpx, Cpy, Dec, Dex, Dey, Eor, Inc, Inx, Iny, Irq, Jmp, Jsr, Lda, Ldx, Ldy, Lsr, Nmi, Nop,
    Ora, Pha, Php, Pla, Plp, Rol, Ror, Rti, Rts, Sbc, Sec, Sed, Sei, Sta, Stx, Sty, Tax, Tay,
    Tsx, Txa, Txs, Tya,
    // 65C02 only
    Pea, Pei, Phy, Stz, Trb, Tsb, JmpIndexedIndirect, JmpIndirect, JsrIndexedIndirect,
    // 6502 illegal/Undocumented instructions
    Alr, Anc, Ane, Arr, Dcp, Isc, Las, Lax, Lxa, Rla, Rra, Sax, Sbx, Sha, Shx, Shy, Slo, Sre, Tas,
}

impl Operation {
    pub fn is_rmw(&self) -> bool {
        matches!(self, Operation::Lsr | Operation::Asl | Operation::Rol | Operation::Ror | Operation::Inc | Operation::Dec | Operation::Dcp | Operation::Isc | Operation::Slo | Operation::Rla | Operation::Rra | Operation::Sre)
    }

    pub fn is_write(&self) -> bool {
        matches!(self, Operation::Sta | Operation::Stx | Operation::Sty | Operation::Sax | Operation::Sbx | Operation::Sha | Operation::Shx | Operation::Shy | Operation::Tas)
    }
}

pub struct CpuConfig {
    has_bcd: bool,
    has_jmp_bug: bool,
    is_c02: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CpuVariant {
    NMOS6502,
    Ricoh2A03,
    WDC65C02,
}

pub struct M6502Cpu {
    pub pc: u16,
    pub sp: u8,
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub status: StatusFlags,
    nmi_pending: bool,
    prev_nmi_line: bool,
    i_delay: bool,
    bus_available: bool,  // false during dma transfer
    last_cycles: u8,
    last_opcode: u8, // Save most recent instruction for debugging
    operand_address_crossed_page: bool,
    pub total_cycles: u64,
    pub is_running: bool,
    pub config: CpuConfig,
    stall_cycles: u16,
    temp_addr_low: u8,
    temp_addr_high: u8,
    temp_value: u8,
    effective_addr: u16,
    current_opcode: u8,
    current_op: Operation,
    current_mode: AddressingMode, 
    cycles_remaining: u32,
    instruction_step: u32,
    test_prints: u8,
}

#[derive(Clone, Serialize, Deserialize)] 
pub struct CpuState {
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub pc: u16,
    pub sp: u8,
    pub status: StatusFlags,
    
    // Crucial for your cycle accuracy and debugging:
    pub cycles_remaining: u32,
    pub instruction_step: u32,
    pub current_opcode: u8,
    pub nmi_pending: bool,
    pub prev_nmi_line: bool,
    pub total_cycles: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum AddressingMode {
    Immediate,
    Indirect,
    Relative,
    Accumulator,
    Implied,
    ZeroPage,
    ZeroPageX,
    ZeroPageY,
    Absolute,
    AbsoluteX,
    AbsoluteY,
    IndirectX,
    IndirectY,
    Interrupt,
    Unique,
}


impl M6502Cpu {
    pub fn new(variant: CpuVariant) -> Self {
        let config = match variant {
            CpuVariant::NMOS6502 => CpuConfig {
                has_bcd: true,
                has_jmp_bug: true,
                is_c02: false,
            },
            CpuVariant::Ricoh2A03 => CpuConfig {
                has_bcd: false,
                has_jmp_bug: true,
                is_c02: false,
            },
              CpuVariant::WDC65C02 => CpuConfig {
                has_bcd: true,
                has_jmp_bug: false,
                is_c02: true,
            },
        };
        Self {
            pc: 0,
            sp: 0xfd,
            a: 0,
            x: 0,
            y: 0,
            status: StatusFlags { negative: false, overflow: false, decimal: false, interrupt_disable: false, zero: false, carry: false},
            nmi_pending: false,
            prev_nmi_line: false,
            i_delay: false,
            test_prints: 0,
            last_cycles: 0,
            last_opcode: 0,
            total_cycles: 0,
            operand_address_crossed_page: false,
            is_running: false,
            bus_available: true,
            config,
            stall_cycles: 0,
            temp_addr_low: 0,
            temp_addr_high: 0,
            temp_value: 0,
            effective_addr: 0,
            current_opcode: 0,
            current_op: Operation::Nop,
            current_mode: AddressingMode::Implied,
            instruction_step: 0,
            cycles_remaining: 0,
        }
    }

    pub fn get_state(&self) -> CpuState {
        CpuState {
            a: self.a,
            x: self.x,
            y: self.y,
            pc: self.pc,
            sp: self.sp,
            status: self.status,
            cycles_remaining: self.cycles_remaining,
            instruction_step: self.instruction_step,
            current_opcode: self.current_opcode,
            nmi_pending: self.nmi_pending,
            prev_nmi_line: self.prev_nmi_line,
            total_cycles: self.total_cycles,
        }
    }

    pub fn total_cycles(&self) -> u64 { self.total_cycles }

    // 2. Load the CPU state back in (Used for loading a save state or rewinding the debugger)
    pub fn set_state(&mut self, state: CpuState) {
        self.a = state.a;
        self.x = state.x;
        self.y = state.y;
        self.pc = state.pc;
        self.sp = state.sp;
        self.status = state.status;
        self.cycles_remaining = state.cycles_remaining;
        self.instruction_step = state.instruction_step;
        self.current_opcode = state.current_opcode;
        self.nmi_pending = state.nmi_pending;
        self.prev_nmi_line = state.prev_nmi_line;
        self.total_cycles = state.total_cycles;
    }

    pub fn is_interrupt_disabled(&self) -> bool {
        self.status.interrupt_disable
    }

    pub fn power_on(&mut self, bus: &mut dyn AddressBus) {
        self.is_running = true;
        self.a = 0; self.x = 0; self.y = 0;
        self.sp = 0xFD;
        self.status.interrupt_disable = true;
        self.nmi_pending = false;
        self.i_delay = false;
        self.prev_nmi_line = false;
        self.last_cycles = 0;
        self.last_opcode = 0;
        for _ in 0..5 {
            bus.step_cycles(1);
        }
        self.operand_address_crossed_page = false;

        let lo = bus.read_byte(0xFFFC) as u16;
        bus.step_cycles(1);
        let hi = bus.read_byte(0xFFFD) as u16;
        bus.step_cycles(1);
        self.pc = (hi << 8) | lo;
        self.total_cycles = 7;
        emu_print!("total cycles {} bus cycles {}", self.total_cycles, bus.total_cycles());
    }

    pub fn reset(&mut self, bus: &mut dyn AddressBus) {
        self.sp = self.sp.wrapping_sub(3);
        self.status.interrupt_disable = true;
        self.nmi_pending = false;
        self.i_delay = false;
        self.prev_nmi_line = false;
        self.last_cycles = 0;
        self.last_opcode = 0;
        self.total_cycles = 0;
        self.operand_address_crossed_page = false;
        self.is_running = false;
        let lo = bus.read_byte(0xFFFC) as u16;
        let hi = bus.read_byte(0xFFFD) as u16;
        self.pc = (hi << 8) | lo;
    }

    pub fn step_one_cycle(&mut self, bus: &mut dyn AddressBus) {
        let current_nmi_line = bus.is_nmi_line_asserted();
        if !self.prev_nmi_line && current_nmi_line {
            self.nmi_pending = true;
        }
        self.prev_nmi_line = current_nmi_line;

        if self.cycles_remaining == 0 {
            self.instruction_step = 1;
            if !self.check_interrupts(bus) {
                self.i_delay = false;
                // CYCLE 1: FETCH STAGE
                self.current_opcode = bus.read_byte(self.pc);

/*                if self.total_cycles >= 85000 && self.total_cycles <= 8585050 {
                    let result = bus.read_byte(0x6000);
                    let result1 = bus.read_byte(0x6001);
                    let result2 = bus.read_byte(0x6004);
                    let result3 = bus.read_byte(0x6005);
                    let result4 = bus.read_byte(0x6006);
                    let result5 = bus.read_byte(0x6007);
                    let result6 = bus.read_byte(0x6008);
                    emu_print!("******result={:02X}. {:02X} {:02X} {:02X} {:02X} {:02X} {:02X}", result, result1, result2, result3, result4, result5, result6);
                } // && self.total_cycles <= 63246 { */
//                if self.test_print {
//                if bus.total_cycles() <= 100 || (self.pc >= 0xA350 && self.pc <= 0xA370) {
/*                if !bus.is_nmi_enabled() { */
                if self.test_prints > 0 {
                    emu_print!("{ } Current opcode: {:02x} PC={:04x}|A={:02x}|X={:02X}|Y={:02X}|SP={:04x}|I={}|I_DELAY={}", bus.total_cycles(), self.current_opcode, self.pc, self.a, self.x, self.y, self.sp, self.status.interrupt_disable, self.i_delay);
                    self.test_prints -= 1;
                }
/*
                    if self.pc == 0xDDC6 {
                       let result1 = bus.read_byte(0xDDC7);
                       let result2 = bus.read_byte(0xDDC8);
                       emu_print!("operand={:02X} {:02X}", result1, result2);  
                    }
                    if self.pc == 0x82C6 || self.pc == 0x82C8 {
                       let result1 = bus.read_byte(self.pc + 1);
                       emu_print!("operand={:02X}", result1);  

                    }
                } */
                let (op, mode, cycles) = self.decode_opcode(self.current_opcode);
                self.pc = self.pc.wrapping_add(1);
                self.current_mode = mode;
                self.current_op = op;
                self.cycles_remaining = cycles as u32;
                self.instruction_step = 2;
            }
        } else {
            // CYCLES 2+: EXECUTION PIPELINE
            self.execute_micro_cycle(bus);
            self.instruction_step += 1;
        }
        self.cycles_remaining -= 1;
        self.total_cycles += 1;
    }

    /* Third byte returned should be the minimum number of cycles */
    fn decode_opcode(&self, opcode: u8) -> (Operation, AddressingMode, u8) {
        match opcode {
            0x00 => (Operation::Brk, AddressingMode::Interrupt, 7),
            0x01 => (Operation::Ora, AddressingMode::IndirectX, 6),
            0x05 => (Operation::Ora, AddressingMode::ZeroPage, 3),
            0x06 => (Operation::Asl, AddressingMode::ZeroPage, 5),
            0x08 => (Operation::Php, AddressingMode::Unique, 3),
            0x09 => (Operation::Ora, AddressingMode::Immediate, 2),
            0x0A => (Operation::Asl, AddressingMode::Accumulator, 2),
            0x0D => (Operation::Ora, AddressingMode::Absolute, 4),
            0x0E => (Operation::Asl, AddressingMode::Absolute, 6),
            0x10 => (Operation::Bpl, AddressingMode::Relative, 2),
            0x11 => (Operation::Ora, AddressingMode::IndirectY, 5),
            0x15 => (Operation::Ora, AddressingMode::ZeroPageX, 4),
            0x16 => (Operation::Asl, AddressingMode::ZeroPageX, 6),
            0x18 => (Operation::Clc, AddressingMode::Implied, 2),
            0x19 => (Operation::Ora, AddressingMode::AbsoluteY, 4),
            0x1D => (Operation::Ora, AddressingMode::AbsoluteX, 4),
            0x1E => (Operation::Asl, AddressingMode::AbsoluteX, 7),
            0x20 => (Operation::Jsr, AddressingMode::Unique, 6),
            0x21 => (Operation::And, AddressingMode::IndirectX, 6),
            0x24 => (Operation::Bit, AddressingMode::ZeroPage, 3),
            0x25 => (Operation::And, AddressingMode::ZeroPage, 3),
            0x26 => (Operation::Rol, AddressingMode::ZeroPage, 5),
            0x28 => (Operation::Plp, AddressingMode::Unique, 4),
            0x29 => (Operation::And, AddressingMode::Immediate, 2),
            0x2A => (Operation::Rol, AddressingMode::Accumulator, 2),
            0x2C => (Operation::Bit, AddressingMode::Absolute, 4),
            0x2D => (Operation::And, AddressingMode::Absolute, 4),
            0x2E => (Operation::Rol, AddressingMode::Absolute, 6),
            0x30 => (Operation::Bmi, AddressingMode::Relative, 2),
            0x31 => (Operation::And, AddressingMode::IndirectY, 5),
            0x35 => (Operation::And, AddressingMode::ZeroPageX, 4),
            0x36 => (Operation::Rol, AddressingMode::ZeroPageX, 6),
            0x38 => (Operation::Sec, AddressingMode::Implied, 2),
            0x39 => (Operation::And, AddressingMode::AbsoluteY, 4),
            0x3D => (Operation::And, AddressingMode::AbsoluteX, 4),
            0x3E => (Operation::Rol, AddressingMode::AbsoluteX, 7),
            0x40 => (Operation::Rti, AddressingMode::Unique, 6),
            0x41 => (Operation::Eor, AddressingMode::IndirectX, 6),
            0x45 => (Operation::Eor, AddressingMode::ZeroPage, 3),
            0x46 => (Operation::Lsr, AddressingMode::ZeroPage, 5),
            0x48 => (Operation::Pha, AddressingMode::Unique, 3),
            0x49 => (Operation::Eor, AddressingMode::Immediate, 2),
            0x4A => (Operation::Lsr, AddressingMode::Accumulator, 2),
            0x4C => (Operation::Jmp, AddressingMode::Absolute, 3),
            0x4D => (Operation::Eor, AddressingMode::Absolute, 4),
            0x4E => (Operation::Lsr, AddressingMode::Absolute, 6),
            0x50 => (Operation::Bvc, AddressingMode::Relative, 2),
            0x51 => (Operation::Eor, AddressingMode::IndirectY, 5),
            0x55 => (Operation::Eor, AddressingMode::ZeroPageX, 4),
            0x56 => (Operation::Lsr, AddressingMode::ZeroPageX, 6),
            0x58 => (Operation::Cli, AddressingMode::Implied, 2),
            0x59 => (Operation::Eor, AddressingMode::AbsoluteY, 4),
            0x5D => (Operation::Eor, AddressingMode::AbsoluteX, 4),
            0x5E => (Operation::Lsr, AddressingMode::AbsoluteX, 7),
            0x60 => (Operation::Rts, AddressingMode::Unique, 6),
            0x61 => (Operation::Adc, AddressingMode::IndirectX, 6),
            0x65 => (Operation::Adc, AddressingMode::ZeroPage, 3),
            0x66 => (Operation::Ror, AddressingMode::ZeroPage, 5),
            0x68 => (Operation::Pla, AddressingMode::Unique, 4),
            0x69 => (Operation::Adc, AddressingMode::Immediate, 2),
            0x6A => (Operation::Ror, AddressingMode::Accumulator, 2),
            0x6C => {
                let cycles_remaining = match self.config.has_jmp_bug {
                    true => 5,
                    false => 6,
                };
                (Operation::Jmp, AddressingMode::Indirect, cycles_remaining)
            },
            0x6D => (Operation::Adc, AddressingMode::Absolute, 4),
            0x6E => (Operation::Ror, AddressingMode::Absolute, 6),
            0x70 => (Operation::Bvs, AddressingMode::Relative, 2),
            0x71 => (Operation::Adc, AddressingMode::IndirectY, 5),
            0x75 => (Operation::Adc, AddressingMode::ZeroPageX, 4),
            0x76 => (Operation::Ror, AddressingMode::ZeroPageX, 6),
            0x78 => (Operation::Sei, AddressingMode::Implied, 2),
            0x79 => (Operation::Adc, AddressingMode::AbsoluteY, 4),
            0x7D => (Operation::Adc, AddressingMode::AbsoluteX, 4),
            0x7E => (Operation::Ror, AddressingMode::AbsoluteX, 7),
            0x81 => (Operation::Sta, AddressingMode::IndirectX, 6),
            0x84 => (Operation::Sty, AddressingMode::ZeroPage, 3),
            0x85 => (Operation::Sta, AddressingMode::ZeroPage, 3),
            0x86 => (Operation::Stx, AddressingMode::ZeroPage, 3),
            0x88 => (Operation::Dey, AddressingMode::Implied, 2),
            0x8A => (Operation::Txa, AddressingMode::Implied, 2),
            0x8C => (Operation::Sty, AddressingMode::Absolute, 4),
            0x8D => (Operation::Sta, AddressingMode::Absolute, 4),
            0x8E => (Operation::Stx, AddressingMode::Absolute, 4),
            0x90 => (Operation::Bcc, AddressingMode::Relative, 2),
            0x91 => (Operation::Sta, AddressingMode::IndirectY, 6),
            0x94 => (Operation::Sty, AddressingMode::ZeroPageX, 4),
            0x95 => (Operation::Sta, AddressingMode::ZeroPageX, 4),
            0x96 => (Operation::Stx, AddressingMode::ZeroPageY, 4),
            0x98 => (Operation::Tya, AddressingMode::Implied, 2),
            0x99 => (Operation::Sta, AddressingMode::AbsoluteY, 5),
            0x9A => (Operation::Txs, AddressingMode::Implied, 2),
            0x9D => (Operation::Sta, AddressingMode::AbsoluteX, 5),
            0xA0 => (Operation::Ldy, AddressingMode::Immediate, 2),
            0xA1 => (Operation::Lda, AddressingMode::IndirectX, 6),
            0xA2 => (Operation::Ldx, AddressingMode::Immediate, 2),
            0xA4 => (Operation::Ldy, AddressingMode::ZeroPage, 3),
            0xA5 => (Operation::Lda, AddressingMode::ZeroPage, 3),
            0xA6 => (Operation::Ldx, AddressingMode::ZeroPage, 3),
            0xA8 => (Operation::Tay, AddressingMode::Implied, 2),
            0xA9 => (Operation::Lda, AddressingMode::Immediate, 2),
            0xAA => (Operation::Tax, AddressingMode::Implied, 2),
            0xAC => (Operation::Ldy, AddressingMode::Absolute, 4),
            0xAD => (Operation::Lda, AddressingMode::Absolute, 4),
            0xAE => (Operation::Ldx, AddressingMode::Absolute, 4),
            0xB0 => (Operation::Bcs, AddressingMode::Relative, 2),
            0xB1 => (Operation::Lda, AddressingMode::IndirectY, 5),
            0xB4 => (Operation::Ldy, AddressingMode::ZeroPageX, 4),
            0xB5 => (Operation::Lda, AddressingMode::ZeroPageX, 4),
            0xB6 => (Operation::Ldx, AddressingMode::ZeroPageY, 4),
            0xB8 => (Operation::Clv, AddressingMode::Implied, 2),
            0xB9 => (Operation::Lda, AddressingMode::AbsoluteY, 4),
            0xBA => (Operation::Tsx, AddressingMode::Implied, 2),
            0xBC => (Operation::Ldy, AddressingMode::AbsoluteX, 4),
            0xBD => (Operation::Lda, AddressingMode::AbsoluteX, 4),
            0xBE => (Operation::Ldx, AddressingMode::AbsoluteY, 4),
            0xC0 => (Operation::Cpy, AddressingMode::Immediate, 2),
            0xC1 => (Operation::Cmp, AddressingMode::IndirectX, 6),
            0xC4 => (Operation::Cpy, AddressingMode::ZeroPage, 3),
            0xC5 => (Operation::Cmp, AddressingMode::ZeroPage, 3),
            0xC6 => (Operation::Dec, AddressingMode::ZeroPage, 5),
            0xC8 => (Operation::Iny, AddressingMode::Implied, 2),
            0xC9 => (Operation::Cmp, AddressingMode::Immediate, 2),
            0xCA => (Operation::Dex, AddressingMode::Implied, 2),
            0xCC => (Operation::Cpy, AddressingMode::Absolute, 4),
            0xCD => (Operation::Cmp, AddressingMode::Absolute, 4),
            0xCE => (Operation::Dec, AddressingMode::Absolute, 6),
            0xD0 => (Operation::Bne, AddressingMode::Relative, 2),
            0xD1 => (Operation::Cmp, AddressingMode::IndirectY, 5),
            0xD5 => (Operation::Cmp, AddressingMode::ZeroPageX, 4),
            0xD6 => (Operation::Dec, AddressingMode::ZeroPageX, 6),
            0xD8 => (Operation::Cld, AddressingMode::Implied, 2),
            0xD9 => (Operation::Cmp, AddressingMode::AbsoluteY, 4),
            0xDD => (Operation::Cmp, AddressingMode::AbsoluteX, 4),
            0xDE => (Operation::Dec, AddressingMode::AbsoluteX, 7),
            0xE0 => (Operation::Cpx, AddressingMode::Immediate, 2),
            0xE1 => (Operation::Sbc, AddressingMode::IndirectX, 6),
            0xE4 => (Operation::Cpx, AddressingMode::ZeroPage, 3),
            0xE5 => (Operation::Sbc, AddressingMode::ZeroPage, 3),
            0xE6 => (Operation::Inc, AddressingMode::ZeroPage, 5),
            0xE8 => (Operation::Inx, AddressingMode::Implied, 2),
            0xE9 | 0xEB => (Operation::Sbc, AddressingMode::Immediate, 2),
            0xEA => (Operation::Nop, AddressingMode::Implied, 2),
            0xEC => (Operation::Cpx, AddressingMode::Absolute, 4),
            0xED => (Operation::Sbc, AddressingMode::Absolute, 4),
            0xEE => (Operation::Inc, AddressingMode::Absolute, 6),
            0xF0 => (Operation::Beq, AddressingMode::Relative, 2),
            0xF1 => (Operation::Sbc, AddressingMode::IndirectY, 5),
            0xF5 => (Operation::Sbc, AddressingMode::ZeroPageX, 4),
            0xF6 => (Operation::Inc, AddressingMode::ZeroPageX, 6),
            0xF9 => (Operation::Sbc, AddressingMode::AbsoluteY, 4),
            0xF8 => (Operation::Sed, AddressingMode::Implied, 2),
            0xFD => (Operation::Sbc, AddressingMode::AbsoluteX, 4),
            0xFE => (Operation::Inc, AddressingMode::AbsoluteX, 7),
            // 65C02 instructions
            0x04 => {
                if self.config.is_c02 {
                    (Operation::Tsb, AddressingMode::ZeroPage, 5)
                } else {
                    (Operation::Nop, AddressingMode::ZeroPage, 3)
                }
            }
            0x0C => {
                if self.config.is_c02 {
                    (Operation::Tsb, AddressingMode::Absolute, 5)
                } else {
                    (Operation::Nop, AddressingMode::Absolute, 4)
                }
            }
            0x14 => {
                if self.config.is_c02 {
                    (Operation::Trb, AddressingMode::ZeroPage, 5)
                } else {
                    (Operation::Nop, AddressingMode::ZeroPageX, 4)
                }
            }
            0x1C => {
                if self.config.is_c02 {
                    (Operation::Trb, AddressingMode::Absolute, 6)
                } else {
                    (Operation::Nop, AddressingMode::AbsoluteX, 4)
                }
            }
            0x34 => {
                if self.config.is_c02 {
                    (Operation::Bit, AddressingMode::ZeroPageX, 4)
                } else {
                    (Operation::Nop, AddressingMode::ZeroPageX, 4)
                }
            }
            0x3A => {
                if self.config.is_c02 {
                    (Operation::Dec, AddressingMode::Implied, 2)
                } else {
                    (Operation::Nop, AddressingMode::Implied, 2)
                }
            }
            0x3C => {
                if self.config.is_c02 {
                    (Operation::Bit, AddressingMode::AbsoluteX, 4)
                } else {
                    (Operation::Nop, AddressingMode::AbsoluteX, 4)
                }
            }
            0x43 => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Sre, AddressingMode::IndirectX, 8)
                }
            }
            0x47 => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Sre, AddressingMode::ZeroPage, 5)
                }
            }
            0x4F => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Sre, AddressingMode::Absolute, 6)
                }
            }
            0x53 => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Sre, AddressingMode::IndirectY, 8)
                }
            }
            0x57 => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Sre, AddressingMode::ZeroPageX, 6)
                }
            }
            0x5A => {
                if self.config.is_c02 {
                    (Operation::Phy, AddressingMode::Unique, 2)
                } else {
                    (Operation::Nop, AddressingMode::Implied, 2)
                }
            }
            0x5B => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Sre, AddressingMode::AbsoluteY, 7)
                }
            }
            0x5F => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Sre, AddressingMode::AbsoluteX, 7)
                }
            }
            0x64 => {
                if self.config.is_c02 {
                    (Operation::Stz, AddressingMode::ZeroPage, 3)
                } else {
                    (Operation::Nop, AddressingMode::ZeroPage, 3)
                }
            }
            0x6B => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Arr, AddressingMode::Immediate, 2)
                }
            }
            0x74 => {
                if self.config.is_c02 {
                    (Operation::Stz, AddressingMode::ZeroPageX, 4)
                } else {
                    (Operation::Nop, AddressingMode::ZeroPageX, 4)
                }
            }
            0x7C => {
                if self.config.is_c02 {
                    (Operation::JmpIndexedIndirect, AddressingMode::AbsoluteX, 6) 
                } else {
                    (Operation::Nop, AddressingMode::AbsoluteX, 4)
                }
            }
            0xBB => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Las, AddressingMode::AbsoluteY, 4) // Standard NES path
                }
            }
            0xCB => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Sbx, AddressingMode::Immediate, 2)
                }
            }
            0xD4 => {
                if self.config.is_c02 { // TODO: NEED TO IMPLEMENT OPERATION
                    (Operation::Pei, AddressingMode::ZeroPage, 6)
                } else {
                    (Operation::Nop, AddressingMode::ZeroPageX, 4)
                }
            }
            0xDC => {
                if self.config.is_c02 {
                    (Operation::JmpIndirect, AddressingMode::Absolute, 6)
                } else {
                    (Operation::Nop, AddressingMode::AbsoluteX, 4) // Standard NES path
                }
            }
            0xE3 => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Isc, AddressingMode::IndirectX, 8)
                }
            }
            0xE7 => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Isc, AddressingMode::ZeroPage, 5)
                }
            }
            0xEF => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Isc, AddressingMode::Absolute, 6)
                }
            }
            0xF3 => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Isc, AddressingMode::IndirectY, 8)
                }
            }
            0xF4 => {
                if self.config.is_c02 {
                    // 65C02 treats this as PEA, which has a 2-byte operand (3 bytes total)
                    (Operation::Pea, AddressingMode::Immediate, 5)
                } else {
                    // Standard NES treats this as a 2-byte, 4-cycle ZeroPageX NOP
                    (Operation::Nop, AddressingMode::ZeroPageX, 4) 
                }
            }
            0xF7 => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Isc, AddressingMode::ZeroPageX, 6)
                }
            }
            0xFB => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Isc, AddressingMode::AbsoluteY, 7)
                }
            }
            0xFC => {
                if self.config.is_c02 {
                    (Operation::JsrIndexedIndirect, AddressingMode::AbsoluteX, 6)
                } else {
                    (Operation::Nop, AddressingMode::AbsoluteX, 4)
                }
            }
            0xFF => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Isc, AddressingMode::AbsoluteX, 7)
                }
            }
            // Unofficial/Illegal opcodes
            0x03 => {
                if self.config.is_c02 {
                    // 65C02 treats this as a 1-byte, 1-cycle implied NOP block
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                // Standard NES treats this as a heavy 8-cycle SLO instruction
                    (Operation::Slo, AddressingMode::IndirectX, 8)
                }
            }
            0x0B | 0x2B => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Anc, AddressingMode::Immediate, 2)
                }
            }
            0x23 => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Rla, AddressingMode::IndirectX, 8)
                }
            }
            0x27 => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Rla, AddressingMode::ZeroPage, 5)
                }
            }
            0x2F => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Rla, AddressingMode::Absolute, 6)
                }
            }
            0x33 => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Rla, AddressingMode::IndirectY, 8)
                }
            }
            0x37 => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Rla, AddressingMode::ZeroPageX, 6)
                }
            }
            0x3B => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Rla, AddressingMode::AbsoluteY, 7)
                }
            }
            0x3F => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Rla, AddressingMode::AbsoluteX, 7)
                }
            }
            0x4B => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Alr, AddressingMode::Immediate, 2)
                }
            }
            0x63 => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Rra, AddressingMode::IndirectX, 8)
                }
            }
            0x67 => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Rra, AddressingMode::ZeroPage, 5)
                }
            }
            0x6F => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Rra, AddressingMode::Absolute, 6)
                }
            }
            0x73 => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Rra, AddressingMode::IndirectY, 8)
                }
            }
            0x77 => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Rra, AddressingMode::ZeroPageX, 6)
                }
            }
            0x7B => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Rra, AddressingMode::AbsoluteY, 7)
                }
            }
            0x7F => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Rra, AddressingMode::AbsoluteX, 7)
                }
            }
            0x83 => {
                if self.config.is_c02 {
                    // 65C02 treats this as a 1-byte, 1-cycle implied NOP block
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Sax, AddressingMode::IndirectX, 6)
                }
            }
            0x87 => {
                if self.config.is_c02 {
                    // 65C02 treats this as a 1-byte, 1-cycle implied NOP block
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Sax, AddressingMode::ZeroPage, 3)
                }
            }
            0x8B => {
                if self.config.is_c02 {
                    // 65C02 treats this as a 1-byte, 1-cycle implied NOP block
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Ane, AddressingMode::Immediate, 2)
                }
            }
            0x8F => {
                if self.config.is_c02 {
                    // 65C02 treats this as a 1-byte, 1-cycle implied NOP block
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Sax, AddressingMode::Absolute, 4)
                }
            }
            0x93 => {
                if self.config.is_c02 {
                    // 65C02 treats this as a 1-byte, 1-cycle implied NOP block
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Sha, AddressingMode::IndirectY, 6)
                }
            }
            0x97 => {
                if self.config.is_c02 {
                    // 65C02 treats this as a 1-byte, 1-cycle implied NOP block
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Sax, AddressingMode::ZeroPageY, 4)
                }
            }
            0x9B => {
                if self.config.is_c02 {
                    // 65C02 treats this as a 1-byte, 1-cycle implied NOP block
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Tas, AddressingMode::AbsoluteY, 5)
                }
            }
            0x9C => {
                if self.config.is_c02 {
                    // 65C02 treats this as a 1-byte, 1-cycle implied NOP block
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Shy, AddressingMode::AbsoluteX, 5)
                }
            }
            0x9E => {
                if self.config.is_c02 {
                    // 65C02 treats this as a 1-byte, 1-cycle implied NOP block
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Shx, AddressingMode::AbsoluteY, 5)
                }
            }
            0x9F => {
                if self.config.is_c02 {
                    // 65C02 treats this as a 1-byte, 1-cycle implied NOP block
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Sha, AddressingMode::AbsoluteY, 5)
                }
            }
            0xA3 => {
                if self.config.is_c02 {
                    // 65C02 treats this as a 1-byte, 1-cycle implied NOP block
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Lax, AddressingMode::IndirectX, 6)
                }
            }
            0xA7 => {
                if self.config.is_c02 {
                    // 65C02 treats this as a 1-byte, 1-cycle implied NOP block
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Lax, AddressingMode::ZeroPage, 3)
                }
            }
            0xAB => {
                if self.config.is_c02 {
                    // 65C02 treats this as a 1-byte, 1-cycle implied NOP block
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Lxa, AddressingMode::Immediate, 2)
                }
            }
            0xAF => {
                if self.config.is_c02 {
                    // 65C02 treats this as a 1-byte, 1-cycle implied NOP block
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Lax, AddressingMode::Absolute, 4)
                }
            }
            0xB3 => {
                if self.config.is_c02 {
                    // 65C02 treats this as a 1-byte, 1-cycle implied NOP block
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Lax, AddressingMode::IndirectY, 5)
                }
            }
            0xB7 => {
                if self.config.is_c02 {
                    // 65C02 treats this as a 1-byte, 1-cycle implied NOP block
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Lax, AddressingMode::ZeroPageY, 4)
                }
            }
            0xBF => {
                if self.config.is_c02 {
                    // 65C02 treats this as a 1-byte, 1-cycle implied NOP block
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Lax, AddressingMode::AbsoluteY, 4)
                }
            }
            0xC3 => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Dcp, AddressingMode::IndirectX, 8)
                }
            }
            0xC7 => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Dcp, AddressingMode::ZeroPage, 5)
                }
            }
            0xCF => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Dcp, AddressingMode::Absolute, 6)
                }
            }
            0xD3 => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Dcp, AddressingMode::IndirectY, 8)
                }
            }
            0xD7 => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Dcp, AddressingMode::ZeroPageX, 6)
                }
            }
            0xDB => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Dcp, AddressingMode::AbsoluteY, 7)
                }
            }
            0xDF => {
                if self.config.is_c02 {
                    (Operation::Nop, AddressingMode::Implied, 1)
                } else {
                    (Operation::Dcp, AddressingMode::AbsoluteX, 7)
                }
            }
            0x1A | 0x7A | 0xDA | 0xFA => (Operation::Nop, AddressingMode::Implied, 2),
            0x80 | 0x82 | 0x89 | 0xC2 | 0xE2 => (Operation::Nop, AddressingMode::Immediate, 2),
            0x44 => (Operation::Nop, AddressingMode::ZeroPage, 3),
            0x54 => (Operation::Nop, AddressingMode::ZeroPageX, 4),
            0x5C => (Operation::Nop, AddressingMode::AbsoluteX, 4),
            0x07 => (Operation::Slo, AddressingMode::ZeroPage, 5),
            0x0F => (Operation::Slo, AddressingMode::Absolute, 6),
            0x13 => (Operation::Slo, AddressingMode::IndirectY, 8),
            0x17 => (Operation::Slo, AddressingMode::ZeroPageX, 6),
            0x1B => (Operation::Slo, AddressingMode::AbsoluteY, 7),
            0x1F => (Operation::Slo, AddressingMode::AbsoluteX, 7),
            _=> { emu_print!("Opcode unimplemented: {:02X}. {:04X}", opcode, self.pc);
                todo!() },
        }
    }

    fn check_interrupts(&mut self, bus: &mut dyn AddressBus) -> bool {
        if bus.is_irq_line_asserted() && self.test_prints > 0 {
                emu_print!("IRQ Check - line=low, I_flag={}, i_delay={}", 
              self.status.interrupt_disable, self.i_delay);
        }

        if self.nmi_pending {
            self.nmi_pending = false; // Clear edge trigger flag
            emu_print!("Setup NMI Interrupt");
            self.setup_hardware_interrupt(Operation::Nmi, bus);
            return true;
        }

        let interrupts_disabled = if self.i_delay {
            if self.status.interrupt_disable {
                false
            } else {
                true
            }
        } else {
            self.status.interrupt_disable
        };
        
        if bus.is_irq_line_asserted() && !interrupts_disabled {
            emu_print!("******Setup IRQ Interrupt. I_DELAY={}", self.i_delay);
            self.setup_hardware_interrupt(Operation::Irq, bus);
            return true;
        }

        false
    }

    fn setup_hardware_interrupt(&mut self, op: Operation, bus: &mut dyn AddressBus) {
        self.current_op = op;
        self.current_mode = AddressingMode::Interrupt;
        self.cycles_remaining = 7;
        self.instruction_step = 2;
    
        // Cycle 1 Hardware Reality: Read from current PC and discard the byte
        let _dummy = bus.read_byte(self.pc);
        self.i_delay = false;
    }

    fn execute_micro_cycle(&mut self, bus: &mut dyn AddressBus) {
        match self.current_mode {
            AddressingMode::Implied => {
                self.execute_operation(self.current_op, 0, bus);
            }

            AddressingMode::Accumulator => {
                self.execute_operation(self.current_op, self.a, bus);
            }

            AddressingMode::Immediate => {
                let value = bus.read_byte(self.pc);
                self.pc = self.pc.wrapping_add(1);
                self.execute_operation(self.current_op, value, bus);
            }

            AddressingMode::ZeroPage => {
                match self.instruction_step {
                    2 => {
                        self.temp_addr_low = bus.read_byte(self.pc);
                        self.pc = self.pc.wrapping_add(1);
                        self.effective_addr = self.temp_addr_low as u16;
                    }
                    3 => {
                        if self.current_op.is_write() {
                            self.execute_operation(self.current_op, 0, bus);
                        } else {
                            let value = bus.read_byte(self.effective_addr);
                            if self.current_op.is_rmw() {
                                self.temp_value = value;
                            } else {
                                self.execute_operation(self.current_op, value, bus);
                            }
                        }
                    }
                    4 => {
                        bus.write_byte(self.effective_addr, self.temp_value);
                    }
                    5 => { // (RMW only)
                        self.execute_operation(self.current_op, self.temp_value, bus);
                    }
                    _=> {}
                }
            }

            AddressingMode::ZeroPageX | AddressingMode::ZeroPageY => {
                match self.instruction_step {
                    2 => {
                        self.temp_addr_low = bus.read_byte(self.pc);
                        self.pc = self.pc.wrapping_add(1);
                    }
                    3 => {
                        if self.current_mode == AddressingMode::ZeroPageX {
                            self.effective_addr = self.temp_addr_low.wrapping_add(self.x) as u16;
                        } else {
                            self.effective_addr = self.temp_addr_low.wrapping_add(self.y) as u16;
                        }
                    }
                    4 => {
                        if self.current_op.is_write() {
                            self.execute_operation(self.current_op, 0, bus);
                        } else {
                            let value = bus.read_byte(self.effective_addr);
                            if self.current_op.is_rmw() {
                                self.temp_value = value;
                            } else {
                                self.execute_operation(self.current_op, value, bus);
                            }
                        }
                    }
                    5 => { // (RMW only)
                        if self.current_op.is_rmw() {
                        // Dummy write back of unmodified value
                            bus.write_byte(self.effective_addr, self.temp_value);
                        }
                    }
                    6 => {
                        if self.current_op.is_rmw() {
                            self.execute_operation(self.current_op, self.temp_value, bus);
                        } 
                    }
                    _=> { }
                }
            }

            AddressingMode::Relative => {
                let condition_met = match self.current_op {
                    Operation::Bcc => !self.status.carry,
                    Operation::Bcs => self.status.carry,
                    Operation::Bne => !self.status.zero,
                    Operation::Beq => self.status.zero,
                    Operation::Bpl => !self.status.negative,
                    Operation::Bmi => self.status.negative,
                    Operation::Bvc => !self.status.overflow,
                    Operation::Bvs => self.status.overflow,
                    _ => false,
                };
                match self.instruction_step {
                    2 => {
                        let offset = bus.read_byte(self.pc) as i8;
                        self.pc = self.pc.wrapping_add(1);

                        // Check if the condition for the branch is met (e.g., if BNE, check zero flag)
                        if condition_met {
                            // Calculate the base PC address *after* fetching the offset
                            let base_pc = self.pc;
                            self.effective_addr = base_pc.wrapping_add(offset as i16 as u16);

                            // Calculate page cross using the updated base_pc!
                            let page_crossed = (base_pc >> 8) != (self.effective_addr >> 8);
            
                            if page_crossed {
                                // Branch taken + Page Cross = 4 cycles total (Needs 2 more micro-cycles)
                                self.cycles_remaining += 2; 
                            } else {
                                // Branch taken + Same Page = 3 cycles total (Needs 1 more micro-cycle)
                                self.cycles_remaining += 1;
                            }
                        }
                    }
                    3 => {
                        let _dummy = bus.read_byte(self.pc);
                        self.pc = self.effective_addr;
                    }
                    4 => {  // Branch occurs to different page
                        let _dummy = bus.read_byte(self.effective_addr);
                        self.pc = self.effective_addr;
                    }
                    _=> { }
                }
            }

            AddressingMode::Absolute => {
                match self.instruction_step {
                    2 => {
                        self.temp_addr_low = bus.read_byte(self.pc);
                        self.pc = self.pc.wrapping_add(1);
                    }
                    3 => {
                        self.temp_addr_high = bus.read_byte(self.pc);
                        self.pc = self.pc.wrapping_add(1);
                        self.effective_addr = ((self.temp_addr_high as u16) << 8) | (self.temp_addr_low as u16);
                        if self.current_op == Operation::Jmp {
                            self.execute_operation(self.current_op, 0, bus);
                        }
                    }
                    4 =>  {
                        if self.current_op.is_write() {
                            self.execute_operation(self.current_op, 0, bus);
                        } else {
                            let value = bus.read_byte(self.effective_addr);
                            if self.current_op.is_rmw() {
                                self.temp_value = value;
                                bus.write_byte(self.effective_addr, self.temp_value);
                            } else {
                                self.execute_operation(self.current_op, value, bus);
                            }
                        }
                    }
                    5 => {
                        if self.current_op.is_rmw() {
                            self.execute_operation(self.current_op, self.temp_value, bus);
                        } 
                    }
                    _=> {}
                }
            }

            AddressingMode::AbsoluteX | AddressingMode::AbsoluteY => {
                let index = if self.current_mode == AddressingMode::AbsoluteX { self.x } else { self.y };

                match self.instruction_step {
                    2 => {
                        // Cycle 2: Fetch low byte of absolute base address
                        self.temp_addr_low = bus.read_byte(self.pc);
                        self.pc = self.pc.wrapping_add(1);
                    }
                    3 => {
                        // Cycle 3: Fetch high byte of absolute base address
                        self.temp_addr_high = bus.read_byte(self.pc);
                        self.pc = self.pc.wrapping_add(1);

                        // Calculate the uncorrected effective address (high byte hasn't accounted for carry yet)
                        let sum_low = self.temp_addr_low.wrapping_add(index);
                        self.effective_addr = ((self.temp_addr_high as u16) << 8) | (sum_low as u16);
                    }
                    4 => {
                        // Cycle 4: Hardware ALWAYS performs a read from the uncorrected address
                        let page_crossed = (self.temp_addr_low as u16 + index as u16) > 0xFF;
                        let value_read = bus.read_byte(self.effective_addr);

                        // Correct the page address if a cross actually occurred
                        if page_crossed {
                            self.effective_addr = self.effective_addr.wrapping_add(0x0100);
                        }

                        if self.current_op.is_write() || self.current_op.is_rmw() {
                            // Writes and RMWs never early-out. They naturally advance to Step 5.
                            // Their base cycle counts (5 and 7 respectively) are configured by the decoder.
                        } else {
                            // It's a regular Read instruction (LDA, AND, CMP, etc.)
                            if !page_crossed {
                                // No page cross: The uncorrected read we just did was from the final target address!
                                // Execute the operation with this data and drop cycles to finish early.
                                self.execute_operation(self.current_op, value_read, bus);
                                self.cycles_remaining = 1; 
                            } else {
                                // Page crossed: The read data was garbage. Inject a penalty cycle to reach Step 5.
                                self.cycles_remaining += 1;
                            }
                        }
                    }
                    5 => {
                        if self.current_op.is_rmw() {
                            // RMW Cycle 5: Read the valid old data from the now-corrected address
                            self.temp_value = bus.read_byte(self.effective_addr);
                        } else if self.current_op.is_write() {
                            // Write Cycle 5: Write the register contents to the corrected address (Instruction ends)
                            self.execute_operation(self.current_op, 0, bus);
                        } else {
                            // Read Cycle 5 (Page Crossed): Read true data from corrected address and execute
                            let value = bus.read_byte(self.effective_addr);
                            self.execute_operation(self.current_op, value, bus);
                        }
                    }
                    6 => {
                        // RMW Cycle 6: Real 6502 quirk—writes the unmodified old value back to the bus
                        // while the ALU holds the data and calculates the modification.
                        bus.write_byte(self.effective_addr, self.temp_value);
                    }
                    7 => {
                        // RMW Cycle 7: Modify value in the ALU and write the final result back to memory
                        self.execute_operation(self.current_op, self.temp_value, bus);
                    }
                    _ => { }
                }
            }

            AddressingMode::Indirect => {
                match self.instruction_step {
                    2 => {
                        // Cycle 2: Fetch low byte of the pointer address
                        self.temp_addr_low = bus.read_byte(self.pc);
                        self.pc = self.pc.wrapping_add(1);
                    }
                    3 => {
                        // Cycle 3: Fetch high byte of the pointer address
                        self.temp_addr_high = bus.read_byte(self.pc);
                        self.pc = self.pc.wrapping_add(1);                        
            
                        // Construct the 16-bit pointer address
                        self.effective_addr = ((self.temp_addr_high as u16) << 8) | (self.temp_addr_low as u16);
                    }
                    4 => {
                        // Cycle 4: Read the target address low byte from the pointer location
                        self.temp_value = bus.read_byte(self.effective_addr);
                    }
                    5 => {
                        // Cycle 5: Calculate where the high byte lives, factoring in the hardware JMP bug
                        let high_byte_ptr = if self.config.has_jmp_bug && (self.effective_addr & 0x00FF) == 0x00FF {
                            // NMOS 6502 Bug: Page wrap-around instead of crossing a page boundary!
                            // e.g., Pointer $10FF reads low byte from $10FF and high byte from $1000
                            self.effective_addr & 0xFF00
                        } else {
                            // Normal behavior (65C02 / Bug-free variants)
                            self.effective_addr.wrapping_add(1)
                        };

                        // Read the target address high byte
                        let target_high = bus.read_byte(high_byte_ptr);
                        let target_low = self.temp_value;

                        // Formulate the final destination address
                        self.effective_addr = ((target_high as u16) << 8) | (target_low as u16);

                        // If this is an NMOS CPU, the instruction finishes right now on cycle 5
                        if self.config.has_jmp_bug {
                            self.execute_operation(self.current_op, 0, bus);
                        }
                    }
                    6 => {
                        // Cycle 6 (CMOS / WDC65C02 only): 
                        // The 65C02 takes one extra cycle here to safely settle the internal buses.
                        // We do a dummy read from our destination and commit the jump.
                        let _dummy = bus.read_byte(self.effective_addr);
                        self.execute_operation(self.current_op, 0, bus);
                    }
                    _ => { }
                }
            }
            AddressingMode::IndirectX => {
                match self.instruction_step {
                    2 => {
                        // Cycle 2: Fetch the zero-page base address pointer from the instruction stream
                        self.temp_addr_low = bus.read_byte(self.pc);
                        self.pc = self.pc.wrapping_add(1);
                    }
                    3 => {
                    // Cycle 3: Hardware performs a dummy read from the unindexed base address
                    // while adding the X register to it internally.
                    let _dummy = bus.read_byte(self.temp_addr_low as u16);
            
                    // The indexed zero-page pointer address is kept strictly within Page 0
                    self.effective_addr = self.temp_addr_low.wrapping_add(self.x) as u16;
                    }
                    4 => {
                        // Cycle 4: Read the low byte of the target address from the indexed zero-page location
                        self.temp_value = bus.read_byte(self.effective_addr);
                    }
                    5 => {
                        // Cycle 5: Read the high byte of the target address.
                        // Critical 6502 rule: The pointer increment wraps around Page 0! 
                        // If the low byte was at $00FF, the high byte is read from $0000, not $0100.
                        let ptr_high = (self.effective_addr as u8).wrapping_add(1) as u16;
                        self.temp_addr_high = bus.read_byte(ptr_high);

                        // Construct the final 16-bit target address
                        self.effective_addr = ((self.temp_addr_high as u16) << 8) | (self.temp_value as u16);
                    }
                    6 => {
                        // Cycle 6: Perform the actual bus access and execute the instruction operation.
                        if self.current_op.is_rmw() {
                            // RMW Cycle 6: Read the actual data byte from the correct target address
                            self.temp_value = bus.read_byte(self.effective_addr);
                        }
                        else if self.current_op.is_write() {
                            // If it's a write operation (e.g., STA), write the register contents to memory
                            self.execute_operation(self.current_op, 0, bus);
                        } else {
                            // If it's a read operation (e.g., LDA, AND, ADC), fetch the byte and execute
                            let value = bus.read_byte(self.effective_addr);
                            self.execute_operation(self.current_op, value, bus);
                        }
                    }
                    7 => {
                        if self.current_op.is_rmw() {
                            // RMW Cycle 7: Dummy write step. 
                            // Hardware writes the unmodified old value back to memory while calculating the shift.
                            bus.write_byte(self.effective_addr, self.temp_value);
                        }
                    }
                    8 => {
                        if self.current_op.is_rmw() {
                            // RMW Cycle 8: Terminal step.
                            // Execute the actual combined shifting and ORA logic, writing back the shifted byte.
                            self.execute_operation(self.current_op, self.temp_value, bus);
                        }
                    }
                    _ => { }
                }
            }

            AddressingMode::IndirectY => {
                match self.instruction_step {
                    2 => {
                        self.temp_addr_low = bus.read_byte(self.pc);
                        self.pc = self.pc.wrapping_add(1);
                    }
                    3 => {
                        self.temp_value = bus.read_byte(self.temp_addr_low as u16);
                    }
                    4 => {
                        let ptr_high = self.temp_addr_low.wrapping_add(1) as u16;
                        self.temp_addr_high = bus.read_byte(ptr_high);

                        let base_target = ((self.temp_addr_high as u16) << 8) | (self.temp_value as u16);
                        self.effective_addr = base_target.wrapping_add(self.y as u16);
                    }
                    5 => {
                        let expected_high = self.temp_addr_high as u16;
                        let uncorrected_addr = (expected_high << 8) | (self.effective_addr & 0x00FF);
            
                        if self.current_op.is_rmw() {
                            // RMW Illegal Instructions: Read from the uncorrected address and 
                            // proceed directly to cycle 6. Never skip cycles, never add penalty cycles.
                            let _garbage = bus.read_byte(uncorrected_addr);
                        } else if self.current_op.is_write() {
                            let _garbage = bus.read_byte(uncorrected_addr);
                        } else {
                            // Normal Read instruction behavior
                            let actual_high = self.effective_addr >> 8;
                            if expected_high == actual_high {
                                let value = bus.read_byte(self.effective_addr);
                                self.execute_operation(self.current_op, value, bus);
                            } else {
                                let _garbage = bus.read_byte(uncorrected_addr);
                                self.cycles_remaining += 1;
                            }
                        }
                    }

                    6 => {
                        if self.current_op.is_rmw() {
                            // RMW Cycle 6: Read the actual data byte from the correct target address
                            self.temp_value = bus.read_byte(self.effective_addr);
                        } else if self.current_op.is_write() {
                            self.execute_operation(self.current_op, 0, bus);
                        } else {
                            let value = bus.read_byte(self.effective_addr);
                            self.execute_operation(self.current_op, value, bus);
                        }
                    }
                    7 => {
                        if self.current_op.is_rmw() {
                            // RMW Cycle 7: Dummy write step. 
                            // Hardware writes the unmodified old value back to memory while calculating the shift.
                            bus.write_byte(self.effective_addr, self.temp_value);
                        }
                    }
                    8 => {
                        if self.current_op.is_rmw() {
                            // RMW Cycle 8: Terminal step.
                            // Execute the actual combined shifting and ORA logic, writing back the shifted byte.
                            self.execute_operation(self.current_op, self.temp_value, bus);
                        }
                    }
                    _ => { }
                }
            }
            AddressingMode::Unique => {
                match (self.current_op, self.instruction_step) {
                    // JSR
                    (Operation::Jsr, 2) => {
                        self.temp_addr_low = bus.read_byte(self.pc);
                        self.pc = self.pc.wrapping_add(1);
                        self.effective_addr = self.pc;
//                        emu_print!("**JSR** PC={:04X}|SP={:04X}|cycles_remaining={ }, instruction_step={ } ", self.pc, self.sp, self.cycles_remaining, self.instruction_step);
                    }
                    (Operation::Jsr, 3) => { 
                        let _dummy = bus.read_byte(STACK_BASE + self.sp as u16); 
//                        emu_print!("**JSR** PC={:04X}|SP={:04X}|cycles_remaining={ }, instruction_step={ } ", self.pc, self.sp, self.cycles_remaining, self.instruction_step);
                    }
                    (Operation::Jsr, 4) => {
                        bus.write_byte(STACK_BASE + self.sp as u16, (self.effective_addr >> 8) as u8);
                        self.sp = self.sp.wrapping_sub(1);
//                        emu_print!("**JSR** PC={:04X}|SP={:04X}|cycles_remaining={ }, instruction_step={ } | pushed {:02X} ", self.pc, self.sp, self.cycles_remaining, self.instruction_step, (self.effective_addr >> 8) as u8);
                    }
                    (Operation::Jsr, 5) => {
                        bus.write_byte(STACK_BASE + self.sp as u16, (self.effective_addr & 0xFF) as u8);
                        self.sp = self.sp.wrapping_sub(1);
//                        emu_print!("**JSR** PC={:04X}|SP={:04X}|cycles_remaining={ }, instruction_step={ } | pushed {:02X}", self.pc, self.sp, self.cycles_remaining, self.instruction_step, (self.effective_addr & 0xFF) as u8);
                    }
                    (Operation::Jsr, 6) => {
                        self.temp_addr_high = bus.read_byte(self.pc);
                        self.pc = ((self.temp_addr_high as u16) << 8) | self.temp_addr_low as u16;
 //                       emu_print!("**JSR** PC={:04X}|SP={:04X}|cycles_remaining={ }, instruction_step={ } ", self.pc, self.sp, self.cycles_remaining, self.instruction_step);
                    }
                    // PHA
                    (Operation::Pha, 2) => { let _dummy = bus.read_byte(self.pc);
//                        emu_print!("**PHA** PC={:04X}|SP={:04X}|cycles_remaining={ }, instruction_step={ }", self.pc, self.sp, self.cycles_remaining, self.instruction_step);
                    } // Idle cycle
                    (Operation::Pha, 3) => {
                        bus.write_byte(STACK_BASE + self.sp as u16, self.a);
                        self.sp = self.sp.wrapping_sub(1);
//                        emu_print!("**PHA** PC={:04X}|SP={:04X}|cycles_remaining={ }, instruction_step={ } | pushed {:02X}", self.pc, self.sp, self.cycles_remaining, self.instruction_step, self.a);
                    }

                    // PHP
                    (Operation::Php, 2) => { let _dummy = bus.read_byte(self.pc); }
                    (Operation::Php, 3) => {
                        bus.write_byte(STACK_BASE + self.sp as u16, self.status.to_u8(true));
                        self.sp = self.sp.wrapping_sub(1);
 //                       emu_print!("**PHP** PC={:04X}|SP={:04X}|cycles_remaining={ }, instruction_step={ } | pushed {:02X}", self.pc, self.sp, self.cycles_remaining, self.instruction_step, self.status.to_u8(true));
                    }
                    // PHY
                    (Operation::Phy, 2) => { let _dummy = bus.read_byte(self.pc);
//                        emu_print!("**PHA** PC={:04X}|SP={:04X}|cycles_remaining={ }, instruction_step={ }", self.pc, self.sp, self.cycles_remaining, self.instruction_step);
                    } // Idle cycle
                    (Operation::Phy, 3) => {
                        bus.write_byte(STACK_BASE + self.sp as u16, self.y);
                        self.sp = self.sp.wrapping_sub(1);
//                        emu_print!("**PHA** PC={:04X}|SP={:04X}|cycles_remaining={ }, instruction_step={ } | pushed {:02X}", self.pc, self.sp, self.cycles_remaining, self.instruction_step, self.a);
                    }
                    // PLA
                    (Operation::Pla, 2) => {
                        let _dummy = bus.read_byte(self.pc);
   //                     emu_print!("**PLA** PC={:04X}|SP={:04X}|cycles_remaining={ }, instruction_step={ }", self.pc, self.sp, self.cycles_remaining, self.instruction_step);
                    }
                    (Operation::Pla, 3) => {
                        let _dummy = bus.read_byte(STACK_BASE + self.sp as u16);
                        self.sp = self.sp.wrapping_add(1);
  //                      emu_print!("**PLA** PC={:04X}|SP={:04X}|cycles_remaining={ }, instruction_step={ }", self.pc, self.sp, self.cycles_remaining, self.instruction_step);
                    }
                    (Operation::Pla, 4) => {
                        self.a = bus.read_byte(STACK_BASE + self.sp as u16);
                        self.update_nz_flags(self.a);
 //                       emu_print!("**PLA** PC={:04X}|SP={:04X}|cycles_remaining={ }, instruction_step={ } | pulled {:02X}", self.pc, self.sp, self.cycles_remaining, self.instruction_step, self.a);
                    }

                    // PLP
                    (Operation::Plp, 2) => { let _dummy = bus.read_byte(self.pc); }
                    (Operation::Plp, 3) => { self.sp = self.sp.wrapping_add(1); let _dummy = bus.read_byte(STACK_BASE + self.sp as u16); }
                    (Operation::Plp, 4) => {
                        let val = bus.read_byte(STACK_BASE + self.sp as u16);
                        let old_interrupt_disable = self.status.interrupt_disable;


                        self.status.from_u8(val); 

                        if old_interrupt_disable != self.status.interrupt_disable {
                            self.i_delay = true;
                        }
                    }
                    // RTI
                    (Operation::Rti, 2) => { let _dummy = bus.read_byte(self.pc); }
                    (Operation::Rti, 3) => { let _dummy = bus.read_byte(STACK_BASE + self.sp as u16); self.sp = self.sp.wrapping_add(1); }
                    (Operation::Rti, 4) => {
                         self.status.from_u8(bus.read_byte(STACK_BASE + self.sp as u16));
                         self.sp = self.sp.wrapping_add(1);
                         self.i_delay = false;
                    }
                    (Operation::Rti, 5) => { self.temp_addr_low = bus.read_byte(STACK_BASE + self.sp as u16); self.sp = self.sp.wrapping_add(1); }
                    (Operation::Rti, 6) => { self.temp_addr_high = bus.read_byte(STACK_BASE + self.sp as u16);
                                             self.pc = ((self.temp_addr_high as u16) << 8) | (self.temp_addr_low as u16);
                                                emu_print!("{} ***RTI*** returning to PC={:04X}. SP now={:04X}. I={}|I_DELAY={}", bus.total_cycles(), self.pc, self.sp, self.status.interrupt_disable, self.i_delay);
                                            }
                    // RTS
                    (Operation::Rts, 2) => { let _dummy = bus.read_byte(self.pc);
                        // emu_print!("**RTS** PC={:04X}|SP={:04X}|cycles_remaining={ }, instruction_step={ } ", self.pc, self.sp, self.cycles_remaining, self.instruction_step);
                    }
                    (Operation::Rts, 3) => { let _dummy = bus.read_byte(0x0100 + self.sp as u16);
                        // emu_print!("**RTS** PC={:04X}|SP={:04X}|cycles_remaining={ }, instruction_step={ } ", self.pc, self.sp, self.cycles_remaining, self.instruction_step); 
                    }
                    (Operation::Rts, 4) => { self.sp = self.sp.wrapping_add(1); self.temp_addr_low = bus.read_byte(0x0100 + self.sp as u16);
                        // emu_print!("**RTS** PC={:04X}|SP={:04X}|cycles_remaining={ }, instruction_step={ } ", self.pc, self.sp, self.cycles_remaining, self.instruction_step); 
                    }
                    (Operation::Rts, 5) => { self.sp = self.sp.wrapping_add(1); self.temp_addr_high = bus.read_byte(STACK_BASE + self.sp as u16);
                        // emu_print!("**RTS** PC={:04X}|SP={:04X}|cycles_remaining={ }, instruction_step={ } ", self.pc, self.sp, self.cycles_remaining, self.instruction_step); 
                    }
                    (Operation::Rts, 6) => { let _dummy = bus.read_byte(self.pc); self.pc = ((self.temp_addr_high as u16) << 8) | (self.temp_addr_low as u16); self.pc = self.pc.wrapping_add(1);
                        // emu_print!("**RTS** PC={:04X}|SP={:04X}|cycles_remaining={ }, instruction_step={ } ", self.pc, self.sp, self.cycles_remaining, self.instruction_step);
                    }
                    _=> { }
                }
            }

            AddressingMode::Interrupt => {
                let vector_base_addr = match self.current_op {
                    Operation::Nmi => 0xFFFA,
                    Operation::Brk | Operation::Irq => 0xFFFE,
                    _ => 0xFFFE,
                };

                match self.instruction_step {
                    2 => {
                    // Cycle 2: Dummy Read
                        let _dummy = bus.read_byte(self.pc);
 //                       emu_print!("Interrupt cycle 2 at PC={:04X}. Cycle={}, bus cycles={}", self.pc, self.total_cycles, bus.total_cycles());
                        if self.current_op == Operation::Brk {
                            // Software BRK is a 2-byte instruction frame, so it advances PC here.
                            // Hardware interrupts do NOT advance PC.
                            self.pc = self.pc.wrapping_add(1);
                            emu_print!("Operation is BRK. Cycle 2 done and PC advanced to {:04X}. Cycle={}, bus cycles={}", self.pc, self.total_cycles, bus.total_cycles());
                        }
                        if self.current_op == Operation::Nmi {
                            emu_print!("{} NMI cycle 2: PC: {:04X}| SP: {:04X}", bus.total_cycles(), self.pc, self.sp);
                        } else if self.current_op == Operation::Irq {
                            emu_print!("{} IRQ cycle 2: PC: {:04X}| SP: {:04X}", bus.total_cycles(), self.pc, self.sp);
//                        self.test_print = true;
                        }
                    }
                    3 => {
                        // Cycle 3: Push PC High Byte to Stack
                        let pc_high = (self.pc >> 8) as u8;
                        bus.write_byte(STACK_BASE + (self.sp as u16), pc_high);
                        self.sp = self.sp.wrapping_sub(1);
//                            emu_print!("Interrupt cycle 3 at PC={:04X}. Cycle={}, bus cycles={}", self.pc, self.total_cycles, bus.total_cycles()); 
                        if self.current_op == Operation::Brk {
                            emu_print!("Operation is BRK. Cycle 3 done and PC still {:04X}. Cycle={}, bus cycles={}", self.pc, self.total_cycles, bus.total_cycles());

                        }
                    }
                    4 => {
                        // Cycle 4: Push PC Low Byte to Stack
                        let pc_low = (self.pc & 0x00FF) as u8;
                        bus.write_byte(STACK_BASE + (self.sp as u16), pc_low);
                        self.sp = self.sp.wrapping_sub(1);
                        if self.current_op == Operation::Brk {
                            emu_print!("Operation is BRK. Cycle 4 done and PC still {:04X}. Cycle={}, bus cycles={}", self.pc, self.total_cycles, bus.total_cycles());

                        }

                        //                        emu_print!("Interrupt cycle 4 at PC={:04X}. Cycle={}, bus cycles={}", self.pc, self.total_cycles, bus.total_cycles()); 
                    }
                    5 => {
                        // Cycle 5: Push Status Flags to Stack
                        let is_instruction = self.current_op == Operation::Brk;
                        let status_byte = self.status.to_u8(is_instruction);
            
                        bus.write_byte(STACK_BASE + (self.sp as u16), status_byte);
                        self.sp = self.sp.wrapping_sub(1);
                        if self.current_op == Operation::Brk {
                            emu_print!("Operation is BRK. Cycle 5 done and PC still {:04X}. Cycle={}, bus cycles={}", self.pc, self.total_cycles, bus.total_cycles());

                        }

                        //                        emu_print!("Interrupt cycle 5 at PC={:04X}. Cycle={}, bus cycles={}", self.pc, self.total_cycles, bus.total_cycles()); 
                    }
                    6 => {
                        // Cycle 6: Fetch Vector Low Byte
                        self.temp_addr_low = bus.read_byte(vector_base_addr);
                        if self.current_op == Operation::Brk {
                            emu_print!("Operation is BRK. Cycle 6 done and PC still {:04X}. Cycle={}, bus cycles={}", self.pc, self.total_cycles, bus.total_cycles());

                        }

                        //                        emu_print!("Interrupt cycle 6 at PC={:04X}. Cycle={}, bus cycles={}", self.pc, self.total_cycles, bus.total_cycles());
                    }
                    7 => {
                        // Cycle 7: Fetch Vector High Byte and perform the actual vector jump!
                        self.status.interrupt_disable = true;
                        let high_byte = bus.read_byte(vector_base_addr + 1);
                        self.pc = ((high_byte as u16) << 8) | (self.temp_addr_low as u16);
                        if self.current_op == Operation::Nmi {
                            emu_print!("{} NMI cycle 7: Vector Addr: {:04X}|PC: {:04X}|Vector high byte: {:02X} | low byte: {:02X}", bus.total_cycles(), vector_base_addr, self.pc, high_byte, self.temp_addr_low);
                        } else if self.current_op == Operation::Irq {
                            emu_print!("{} IRQ cycle 7: Vector Addr: {:04X}|PC: {:04X}|Vector high byte: {:02X} | low byte: {:02X}", bus.total_cycles(), vector_base_addr, self.pc, high_byte, self.temp_addr_low);
//                        self.test_print = true;
                        }
                        if self.current_op == Operation::Brk {
                            emu_print!("Operation is BRK. Cycle 7 done and PC now {:04X}. Cycle={}, bus cycles={}", self.pc, self.total_cycles, bus.total_cycles());

                        }
                    }
                    _ => { emu_print!("INTERRUPT step {}: Vector Addr: {:04X}|PC: {:04X}|", self.instruction_step, vector_base_addr, self.pc); }
                }
            }
        }
    }

    fn execute_operation(&mut self, op: Operation, value: u8, bus: &mut dyn AddressBus) {
        match op {
            Operation::Adc => self.add_with_carry_logic(value),
            Operation::Alr => {  // Undocumented 6502 instruction
                // 1. Perform intermediate AND operation
                let intermediate = self.a & value;
    
                // 2. Perform LSR on the intermediate result
                self.status.carry = (intermediate & 0x01) != 0; // Bit 0 goes to Carry
                self.a = intermediate >> 1;                     // Shift right and save to Accumulator
    
                // 3. Update status flags
                self.status.zero = self.a == 0;
                self.status.negative = false; // Always 0 because bit 7 was shifted right
            }
            Operation::Anc => { // Unofficial 6502 instruction
                // 1. Perform standard bitwise AND on the Accumulator
                self.a &= value; // 'value' is the immediate operand fetched by the addressing mode
    
                // 2. Set standard ALU flags based on the Accumulator
                self.status.zero = self.a == 0;
                self.status.negative = (self.a & 0x80) != 0;
    
                // 3. Unofficial behavior: Copy Bit 7 of the resulting Accumulator into the Carry flag
                // (This is functionally equivalent to making Carry equal to the Negative flag status)
                self.status.carry = self.status.negative;
            }
            Operation::And => { self.a &= value; self.update_nz_flags(self.a); }
            Operation::Ane => { // undocumented 6502 instruction
                // use 0xEE as the 'stable' constant
                let magic_constant = 0xEE; 
    
                // Perform the unstable combined logic loop
                self.a = (self.a | magic_constant) & self.x & value;
    
                // Update basic flags
                self.status.zero = self.a == 0;
                self.status.negative = (self.a & 0x80) != 0;
            }
            Operation::Arr => { // undocumented 6502 instruction
                let intermediate = self.a & value;
    
                let old_carry = if self.status.carry { 0x80 } else { 0 };
                let result = (intermediate >> 1) | old_carry;
    
                self.a = result;
    
                self.status.zero = self.a == 0;
                self.status.negative = (self.a & 0x80) != 0;
    
                // Carry is determined entirely by Bit 6 of the final result
                self.status.carry = (result & 0x40) != 0;
    
                // Overflow is determined by Bit 6 XOR Bit 5 of the final result
                let bit6 = (result >> 6) & 1;
                let bit5 = (result >> 5) & 1;
                self.status.overflow = (bit6 ^ bit5) == 1;
            }
            Operation::Asl => {
                self.status.carry = (value & 0x80) != 0;
                let result = value << 1;
                self.update_nz_flags(result);
                if self.current_mode == AddressingMode::Accumulator {
                    self.a = result;
                } else {
                    bus.write_byte(self.effective_addr, result);
                }
                self.temp_value = result;
            }
            Operation::Bit => { 
                let result = self.a & value;
                self.status.zero = result == 0;
                if self.current_mode != AddressingMode::Immediate {
                    self.status.overflow = value & 0x40 != 0;
                    self.status.negative = value & 0x80 != 0;
                }
            }
            Operation::Clc => { self.status.carry = false; }
            Operation::Cld => { self.status.decimal = false; }
            Operation::Cli => {
                if self.status.interrupt_disable {
                    self.i_delay = true;
                }
                self.status.interrupt_disable = false;

                emu_print!("CLI operation executed");
                self.test_prints = 3;
            }
            Operation::Clv => { self.status.overflow = false; }
            Operation::Cmp => { 
                let result = self.a.wrapping_sub(value);
                self.update_nz_flags(result);
                self.status.carry = value <= self.a;
            }
            Operation::Cpx => { let result = self.x.wrapping_sub(value); self.update_nz_flags(result); self.status.carry = value <= self.x}
            Operation::Cpy => { let result = self.y.wrapping_sub(value); self.update_nz_flags(result); self.status.carry = value <= self.y}
            Operation::Dcp => {
                // 1. Decrement the fetched value by 1
                let decremented_value = value.wrapping_sub(1);
    
                // 2. Write the decremented value back to memory
                bus.write_byte(self.effective_addr, decremented_value);
    
                // 3. Perform CMP logic (A - decremented_value) to set flags
                // Carry is set if Accumulator is greater than or equal to the value
                self.status.carry = self.a >= decremented_value;
    
                // Calculate the temporary result to evaluate Zero and Negative flags
                let comparison_result = self.a.wrapping_sub(decremented_value);
                self.status.zero = comparison_result == 0;
                self.status.negative = (comparison_result & 0x80) != 0;
            }
            Operation::Dec => { let result = value.wrapping_sub(1); self.update_nz_flags(result); bus.write_byte(self.effective_addr, result); }
            Operation::Dex => { self.x = self.x.wrapping_sub(1); self.update_nz_flags(self.x); }
            Operation::Dey => { self.y = self.y.wrapping_sub(1); self.update_nz_flags(self.y); }
            Operation::Eor => { self.a ^= value; self.update_nz_flags(self.a); }
            Operation::Inc => { let result = value.wrapping_add(1); self.update_nz_flags(result); bus.write_byte(self.effective_addr, result); }
            Operation::Inx => { self.x = self.x.wrapping_add(1); self.update_nz_flags(self.x); }
            Operation::Iny => { self.y = self.y.wrapping_add(1); self.update_nz_flags(self.y); }
            Operation::Isc => {
                let incremented_value = value.wrapping_add(1);
                bus.write_byte(self.effective_addr, incremented_value);    
                self.add_with_carry_logic(incremented_value ^ 0xFF);
            }

            Operation::Las => { // undocumented 6502 instruction
                let result = value & self.sp;

                self.a = result;
                self.x = result;
                self.sp = result;
    
                self.status.zero = result == 0;
                self.status.negative = (result & 0x80) != 0;
            }
            Operation::Lax => { // Undocumented 6502 instruction
                self.a = bus.read_byte(self.effective_addr);
                self.x = self.a;
                self.update_nz_flags(self.a);
            }
            Operation::Lda => { self.a = value; self.update_nz_flags(self.a); }
            Operation::Ldx => { self.x = value; self.update_nz_flags(self.x); }
            Operation::Ldy => { self.y = value; self.update_nz_flags(self.y); }
            Operation::Lsr => {
                self.status.carry = (value & 0x01) != 0;
                let result = value >> 1;
                self.update_nz_flags(result);
                if self.current_mode == AddressingMode::Accumulator {
                    self.a = result;
                } else {
                    bus.write_byte(self.effective_addr, result);
                }
                self.temp_value = result;
            }
            Operation::Lxa => { // Undocumented 6502 instruction
                // use 0xEE as the 'stable' constant
                self.a = value;
                self.x = value;
    
                // Update basic flags
                self.status.zero = self.a == 0;
                self.status.negative = (self.a & 0x80) != 0;
            }
            Operation::Jmp => { self.pc = self.effective_addr; }
            Operation::JmpIndexedIndirect => { // 65C02 only
                let low_byte = bus.read_byte(self.effective_addr);
                let high_byte = bus.read_byte(self.effective_addr.wrapping_add(1));
    
                self.pc = ((high_byte as u16) << 8) | (low_byte as u16);
            }
            Operation::Nop => { }
            Operation::Ora => { self.a |= value; self.update_nz_flags(self.a); }
            Operation::Rla => {
                // 1. Grab the current carry bit to insert into Bit 0 of the shifted result
                let old_carry = if self.status.carry { 1 } else { 0 };
    
                // 2. Set the new carry bit to whatever Bit 7 currently is
                self.status.carry = (value & 0x80) != 0;
    
                // 3. Shift left and inject the old carry into bit 0
                let rotated_value = (value << 1) | old_carry;
    
                // 4. Write the rotated value back to memory
                bus.write_byte(self.effective_addr, rotated_value);
    
                // 5. Bitwise AND the result into the Accumulator
                self.a &= rotated_value;
    
                // 6. Update ALU flags based on the final Accumulator status
                self.status.zero = self.a == 0;
                self.status.negative = (self.a & 0x80) != 0;
            }
            Operation::Rol => {
                let old_value = if self.current_mode == AddressingMode::Accumulator { self.a } else { value };
                let next_carry = (old_value & 0x80) != 0;
                let mut result = old_value << 1;
                if self.status.carry { result |= 0x01; }
                self.status.carry = next_carry;

                if self.current_mode == AddressingMode::Accumulator {
                    self.a = result;
                } else {
                    bus.write_byte(self.effective_addr, result);
                }
                self.update_nz_flags(result);
            }
            Operation::Ror => {
                let old_value = if self.current_mode == AddressingMode::Accumulator { self.a } else { value };
                let next_carry = (old_value & 0x01) != 0;
                let mut result = old_value >> 1;
                if self.status.carry { result |= 0x80; }
                self.status.carry = next_carry;

                if self.current_mode == AddressingMode::Accumulator {
                    self.a = result;
                } else {
                    bus.write_byte(self.effective_addr, result);
                }
                self.update_nz_flags(result);
            }
            Operation::Rra => { // Unofficial 6502
                // 1. Grab the current carry bit to insert into Bit 0 of the shifted result
                let old_carry = if self.status.carry { 0x80 } else { 0 };
    
                // 2. Set the new carry bit to whatever Bit 0 currently is
                self.status.carry = (value & 0x01) != 0;
    
                // 3. Shift right and inject the old carry into bit 7
                let rotated_value = (value >> 1) | old_carry;
    
                // 4. Write the rotated value back to memory
                bus.write_byte(self.effective_addr, rotated_value);
    
                // 5. Adc 
                self.add_with_carry_logic(rotated_value);
            }
            Operation::Sax => {
                let result = self.a & self.x;
                bus.write_byte(self.effective_addr, result);
            }
            Operation::Sbc => { self.add_with_carry_logic(value ^ 0xFF); }
            Operation::Sbx => {
                let base = self.a & self.x;
    
                self.status.carry = base >= value;   
                self.x = base.wrapping_sub(value);
    
                self.status.zero = self.x == 0;
                self.status.negative = (self.x & 0x80) != 0;
            }
            Operation::Sec => { self.status.carry = true; }
            Operation::Sed => { self.status.decimal = true; }
            Operation::Sei => { 
                emu_print!("Sei operation executed");
                if !self.status.interrupt_disable {
                    self.i_delay = true;
                }
                self.status.interrupt_disable = true;
                self.test_prints = 3;
            }
            Operation::Sha => {
                // High byte of the target address + 1
                let high_plus_one = ((self.effective_addr >> 8) + 1) as u8;
                let val_to_write = self.a & self.x & high_plus_one;
                bus.write_byte(self.effective_addr, val_to_write);
            }

            Operation::Shx => {
                let high_plus_one = self.temp_addr_high.wrapping_add(1);
                let val_to_write = self.x & high_plus_one;
                
                // Crucial hardware quirk: If a page cross ACTUALLY occurred, 
                // the written value replaces the high byte of the address on the bus.
                let page_crossed = (self.temp_addr_low as u16 + self.y as u16) > 0xFF; // Note: SHX uses Y index
                if page_crossed {
                    let final_addr = ((val_to_write as u16) << 8) | (self.effective_addr & 0x00FF);
                    bus.write_byte(final_addr, val_to_write);
                } else {
                    bus.write_byte(self.effective_addr, val_to_write);
                }
            }

            Operation::Shy => {
                let high_plus_one = self.temp_addr_high.wrapping_add(1);
                let val_to_write = self.y & high_plus_one;
                
                // Crucial hardware quirk: If a page cross ACTUALLY occurred,
                // the written value replaces the high byte of the address on the bus.
                let page_crossed = (self.temp_addr_low as u16 + self.x as u16) > 0xFF; // Note: SHY uses X index
                if page_crossed {
                    let final_addr = ((val_to_write as u16) << 8) | (self.effective_addr & 0x00FF);
                    bus.write_byte(final_addr, val_to_write);
                } else {
                    bus.write_byte(self.effective_addr, val_to_write);
                }
            }

            Operation::Slo => {
                // 1. Unofficial opcode SLO. Shift memory value left (ASL logic)
                self.status.carry = (value & 0x80) != 0; // Bit 7 goes to Carry
                let shifted_value = value << 1;
    
                // 2. Write the shifted value back to the calculated target memory location
                bus.write_byte(self.effective_addr, shifted_value);
    
                // 3. Bitwise OR the result into the Accumulator (ORA logic)
                self.a |= shifted_value;
    
                // 4. Set standard ALU flags based on the final Accumulator register status
                self.status.zero = self.a == 0;
                self.status.negative = (self.a & 0x80) != 0;
            }
            Operation::Sre => {
                // 1. Shift memory value right (LSR logic)
                self.status.carry = (value & 0x01) != 0; // Bit 0 goes to Carry
                let shifted_value = value >> 1;
    
                // 2. Write the shifted value back to memory
                bus.write_byte(self.effective_addr, shifted_value);
    
                // 3. Bitwise XOR the result into the Accumulator (EOR logic)
                self.a ^= shifted_value;
    
                // 4. Update standard ALU flags based on the final Accumulator status
                self.status.zero = self.a == 0;
                self.status.negative = (self.a & 0x80) != 0;
            }
            Operation::Sta => { bus.write_byte(self.effective_addr, self.a); }
            Operation::Stx => { bus.write_byte(self.effective_addr, self.x); }
            Operation::Sty => { bus.write_byte(self.effective_addr, self.y); }
            Operation::Stz => {  // 65C02 only
                // Write 0 directly to the calculated effective address
                bus.write_byte(self.effective_addr, 0x00);
            }
            Operation::Tas => {
                // 1. Bitwise AND A and X 
                let intermediate = self.a & self.x;
                self.sp = intermediate; // Overwrites the CPU stack pointer!
    
                // 2. Compute the value to drop onto the memory bus
                let high_plus_one = ((self.effective_addr >> 8) + 1) as u8;
                let val_to_write = intermediate & high_plus_one;
    
                // 3. Perform the memory store
                bus.write_byte(self.effective_addr, val_to_write);
            }
            Operation::Tax => { self.x = self.a; self.update_nz_flags(self.x); }
            Operation::Tay => { self.y = self.a; self.update_nz_flags(self.y); }
            Operation::Trb => {  // 65C02/65C816 only
                let test_result = self.a & value;
                self.status.zero = test_result == 0;
                let reset_result = self.a & !value;
                bus.write_byte(self.effective_addr, reset_result);
            }
            Operation::Tsb => {  // 65C02/65C816 only
                // 1. Test: Sets Z flag if (A & memory_value) == 0
                let test_result = self.a & value;
                self.status.zero = test_result == 0;
    
                // 2. Set: Force bits to 1 in memory where the Accumulator has a 1
                let set_result = value | self.a;
    
                // 3. Write back the modified value
                bus.write_byte(self.effective_addr, set_result);
            }
            Operation::Tsx => { self.x = self.sp; self.update_nz_flags(self.x); }
            Operation::Txa => { self.a = self.x; self.update_nz_flags(self.a); }
            Operation::Txs => { self.sp = self.x; }
            Operation::Tya => { self.a = self.y; self.update_nz_flags(self.a); }
            _=> todo!()
        }
    }

    /************************/
    /* Arithmetic and logic */
    /************************/

    fn add_with_carry_logic(&mut self, value: u8) {
        let carry_in:u8 = if self.status.carry { 1 } else { 0 };

        if self.config.has_bcd && self.status.decimal {
            let mut low_nibble = (self.a & 0x0F) + (value & 0x0F) + carry_in;
            let mut high_nibble = (self.a >> 4) + (value >> 4);

            if low_nibble > 9 {
                low_nibble += 6;
                high_nibble += 1; // Carry over into the tens digit
            }

            let binary_sum = (self.a as u16) + (value as u16) + (carry_in as u16);
            let uncorrected_a = (binary_sum & 0xFF) as u8;
            if (!(self.a ^ value) & (self.a ^ uncorrected_a) & 0x80) != 0 {
                self.status.overflow = true;
            } else {
                self.status.overflow = false;
            }

            // Correct the upper digit if it exceeds 9
            if high_nibble > 9 {
                high_nibble += 6;
                self.status.carry = true;
            } else {
                self.status.carry = false;
            }

            let result_a = ((high_nibble << 4) & 0xF0) | (low_nibble & 0x0F);
            
            self.a = result_a;
            if self.config.is_c02 {
                self.update_nz_flags(self.a);
            } else {
                self.update_nz_flags(uncorrected_a);
            }
        } else {
            let result:u16 = (self.a as u16) + (value as u16) + (carry_in as u16);
            self.status.carry = result > 0xFF as u16;
            let new_a:u8 = result as u8;
            self.status.overflow = (!(self.a ^ value) & (self.a ^ new_a) & 0x80) != 0;
            self.a = new_a;
            self.update_nz_flags(self.a);
        }
    }

    // Flag helpers
    #[inline(always)]
    fn update_nz_flags(&mut self, value: u8) {
        self.status.zero = value == 0;
        self.status.negative = (value & 0x80) != 0;
    }
}