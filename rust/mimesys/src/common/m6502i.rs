/* version of 6502 still using instruction-based timing */
const STACK_BASE: u16 = 0x100;

use bitflags::bitflags;
use crate::common::bus::AddressBus;
use godot::global::godot_print;

bitflags! {
    #[derive(Clone, Copy, Debug)]
    pub struct Status: u8 {
        const C = 0b0000_0001; // Carry
        const Z = 0b0000_0010; // Zero
        const I = 0b0000_0100; // Interrupt Disable
        const D = 0b0000_1000; // Decimal Mode
        const B = 0b0001_0000; // Break
        const U = 0b0010_0000; // Unused (always set when pushed)
        const V = 0b0100_0000; // Overflow
        const N = 0b1000_0000; // Negative
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
    pub p: Status,           // Status register (flags)
    nmi_armed: bool,
    nmi_pending: bool,
    prev_nmi_line: bool,
    last_cycles: u8,
    last_opcode: u8, // Save most recent instruction for debugging
    operand_address_crossed_page: bool,
    pub total_cycles: u64,
    pub is_running: bool,
    pub config: CpuConfig,
    cycles_remaining: u8,
    stall_cycles: u16,
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
            p: Status::empty(),
            nmi_armed: false,
            nmi_pending: false,
            prev_nmi_line: false,
            last_cycles: 0,
            last_opcode: 0,
            total_cycles: 0,
            operand_address_crossed_page: false,
            is_running: false,
            config,
            cycles_remaining: 0,
            stall_cycles: 0,
        }
    }

    pub fn is_interrupt_disabled(&self) -> bool {
        self.p.contains(Status::I)
    }

    pub fn power_on(&mut self, bus: &impl AddressBus) {
        self.is_running = true;
        self.a = 0; self.x = 0; self.y = 0;
        self.sp = 0xFD;
        self.nmi_armed = false;
        self.nmi_pending = false;
        self.prev_nmi_line = false;
        self.last_cycles = 0;
        self.last_opcode = 0;
        self.total_cycles = 0;
        self.operand_address_crossed_page = false;
        // Clean, readable bitflags construction
        self.p.insert(Status::I);
        self.p.insert(Status::U);
        self.p.insert(Status::B);

        let lo = bus.read_byte(0xFFFC) as u16;
        let hi = bus.read_byte(0xFFFD) as u16;
        self.pc = (hi << 8) | lo;
//        godot_print!("I flag after power_on: {}", self.p.contains(Status::I));
    }

    // return true only when an instruction completes
    pub fn step(&mut self, bus: &mut dyn AddressBus) -> u8 {
        if self.cycles_remaining > 0 {
            self.cycles_remaining -= 1;
            return 1;
        }

        // instruction is complete - check NMI/IRQ before fetching next opcode
        let current_nmi_line = bus.is_nmi_line_asserted();
        if !self.prev_nmi_line && current_nmi_line {
            self.nmi_pending = true;
        }
        self.prev_nmi_line = current_nmi_line;

        if self.nmi_pending {
            self.nmi_pending = false;
            let nmi_cycles = self.trigger_nmi(bus);
            self.cycles_remaining = nmi_cycles - 1;
            return 1;
        }

        if bus.is_irq_line_asserted() && !self.p.contains(Status::I) {
            let irq_cycles = self.trigger_irq(bus);
            self.cycles_remaining = irq_cycles - 1;
            return 1;
        }

        let opcode = bus.read_byte(self.pc);
        self.last_opcode = opcode;

        if bus.total_cycles() > 89000 && bus.total_cycles() < 93000 {
            godot_print!("PC={:04X} op={:02X} A={:02X} X={:02X} Y={:02X} SP={:02X} P={:02X}", 
            self.pc, opcode, self.a, self.x, self.y, self.sp, self.p.bits());
        }
//        if self.total_cycles > 265_000 && self.total_cycles < 271_000 {
//            godot_print!("Current opcode: {:02x} PC={:04x}|A={:02x}|SP={:04x}|$(PC+1)={:02X} {:02X}", opcode, self.pc, self.a, self.sp, memory, next_mem);
//        }
        self.pc = self.pc.wrapping_add(1);
        self.last_cycles = 0;
        self.execute(opcode, bus);

        self.cycles_remaining += self.last_cycles - 1;
        1
    }

    pub fn reset(&mut self, bus: &impl AddressBus) {
        self.sp = self.sp.wrapping_sub(3);
        self.a = 0;
        self.x = 0;
        self.y = 0;
        self.p.insert(Status::I);
        self.nmi_armed = false;
        self.nmi_pending = false;
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

    pub fn execute(&mut self, opcode: u8, bus: &mut dyn AddressBus) {
        match opcode {
            // ADC (ADD with Carry). Affects Flags: N V Z C
            0x69 => { let extra_cycles = self._op_adc(bus, AddressingMode::Immediate); self.last_cycles = 2 + extra_cycles; }
            0x65 => { let extra_cycles = self._op_adc(bus, AddressingMode::ZeroPage); self.last_cycles = 3 + extra_cycles; }
            0x75 => { let extra_cycles = self._op_adc(bus, AddressingMode::ZeroPageX); self.last_cycles = 4 + extra_cycles; }
            0x6D => { let extra_cycles = self._op_adc(bus, AddressingMode::Absolute); self.last_cycles = 4 + extra_cycles; }
            0x7D => { let extra_cycles = self._op_adc(bus, AddressingMode::AbsoluteX); self.last_cycles = 4 + extra_cycles; }
            0x79 => { let extra_cycles = self._op_adc(bus, AddressingMode::AbsoluteY); self.last_cycles = 4 + extra_cycles; }
            0x61 => { let extra_cycles = self._op_adc(bus, AddressingMode::IndirectX); self.last_cycles = 6 + extra_cycles; }
            0x71 => { let extra_cycles = self._op_adc(bus, AddressingMode::IndirectY); self.last_cycles = 5 + extra_cycles; }

            // AND (bitwise AND with accumulator)
            0x29 => { self._op_and_a(bus, AddressingMode::Immediate); self.last_cycles = 2; }
            0x25 => { self._op_and_a(bus, AddressingMode::ZeroPage); self.last_cycles = 3; }
            0x35 => { self._op_and_a(bus, AddressingMode::ZeroPageX); self.last_cycles = 4; }
            0x2D => { self._op_and_a(bus, AddressingMode::Absolute); self.last_cycles = 4; }
            0x3D => { let extra_cycles = self._op_and_a(bus, AddressingMode::AbsoluteX); self.last_cycles = 4 + extra_cycles; }
            0x39 => { let extra_cycles = self._op_and_a(bus, AddressingMode::AbsoluteY); self.last_cycles = 4 + extra_cycles; }
            0x21 => { self._op_and_a(bus, AddressingMode::IndirectX); self.last_cycles = 6; }
            0x31 => { let extra_cycles = self._op_and_a(bus, AddressingMode::IndirectY); self.last_cycles = 5 + extra_cycles; }

            // ASL (Arithmetic Shift Left)
            0x0A => { self.arithmetic_shift_left_a(); self.last_cycles = 2; }
            0x06 => { self._op_asl_memory(bus, AddressingMode::ZeroPage); self.last_cycles = 5; }
            0x16 => { self._op_asl_memory(bus, AddressingMode::ZeroPageX); self.last_cycles = 5; }
            0x0E => { self._op_asl_memory(bus, AddressingMode::Absolute); self.last_cycles = 5; }
            0x1E => { self._op_asl_memory(bus, AddressingMode::AbsoluteX); self.last_cycles = 5; }

            // BIT (test bits)
            0x24 => { self.bit_test_a(bus, AddressingMode::ZeroPage); self.last_cycles = 3; }
            0x2C => { self.bit_test_a(bus, AddressingMode::Absolute); self.last_cycles = 4; }

            // Branch Instructions
            0x10 => { let step_cycles = self.branch_if_flag_matches(bus, Status::N, false); self.last_cycles = step_cycles; }
            0x30 => { let step_cycles = self.branch_if_flag_matches(bus, Status::N, true); self.last_cycles = step_cycles; }
            0x50 => { let step_cycles = self.branch_if_flag_matches(bus, Status::V, false); self.last_cycles = step_cycles; }
            0x70 => { let step_cycles = self.branch_if_flag_matches(bus, Status::V, true); self.last_cycles = step_cycles; }
            0x90 => { let step_cycles = self.branch_if_flag_matches(bus, Status::C, false); self.last_cycles = step_cycles; }
            0xB0 => { let step_cycles = self.branch_if_flag_matches(bus, Status::C, true); self.last_cycles = step_cycles; }
            0xD0 => { let step_cycles = self.branch_if_flag_matches(bus, Status::Z, false); self.last_cycles = step_cycles; }
            0xF0 => { let step_cycles = self.branch_if_flag_matches(bus, Status::Z, true); self.last_cycles = step_cycles; }

            // BRK. Affects Flag B
            0x00 => { self.brk(bus); self.last_cycles = 7; }
            
            // CMP (Compare accumulator)
            0xC9 => { self.compare_a(bus, AddressingMode::Immediate); self.last_cycles = 2; }
            0xC5 => { self.compare_a(bus, AddressingMode::ZeroPage); self.last_cycles = 3; }
            0xD5 => { self.compare_a(bus, AddressingMode::ZeroPageX); self.last_cycles = 4; }
            0xCD => { self.compare_a(bus, AddressingMode::Absolute); self.last_cycles = 4; }
            0xDD => { let extra_cycles = self.compare_a(bus, AddressingMode::AbsoluteX); self.last_cycles = 4 + extra_cycles; }
            0xD9 => { let extra_cycles = self.compare_a(bus, AddressingMode::AbsoluteY); self.last_cycles = 4 + extra_cycles; }
            0xC1 => { self.compare_a(bus, AddressingMode::IndirectX); self.last_cycles = 6; }
            0xD1 => { let extra_cycles = self.compare_a(bus, AddressingMode::IndirectY); self.last_cycles = 5 + extra_cycles; }

            // CPX
            0xE0 => { self.compare_x(bus, AddressingMode::Immediate); self.last_cycles = 2; }
            0xE4 => { self.compare_x(bus, AddressingMode::ZeroPage); self.last_cycles = 3; }
            0xEC => { self.compare_x(bus, AddressingMode::Absolute); self.last_cycles = 4; }

            // CPY
            0xC0 => { self.compare_y(bus, AddressingMode::Immediate); self.last_cycles = 2; }
            0xC4 => { self.compare_y(bus, AddressingMode::ZeroPage); self.last_cycles = 3; }
            0xCC => { self.compare_y(bus, AddressingMode::Absolute); self.last_cycles = 4; }

            // DEC (Increment memory)
            0xC6 => { self.increment_memory(bus, AddressingMode::ZeroPage, -1); self.last_cycles = 5; }
            0xD6 => { self.increment_memory(bus, AddressingMode::ZeroPageX, -1); self.last_cycles = 6; }
            0xCE => { self.increment_memory(bus, AddressingMode::Absolute, -1); self.last_cycles = 6; }
            0xDE => { self.increment_memory(bus, AddressingMode::AbsoluteX, -1); self.last_cycles = 7; }

            // EOR (bitwise exclusive OR)
            0x49 => { self.exclusive_or_with_a(bus, AddressingMode::Immediate); self.last_cycles = 2; }
            0x45 => { self.exclusive_or_with_a(bus, AddressingMode::ZeroPage); self.last_cycles = 3; }
            0x55 => { self.exclusive_or_with_a(bus, AddressingMode::ZeroPageX); self.last_cycles = 4; }
            0x4D => { self.exclusive_or_with_a(bus, AddressingMode::Absolute); self.last_cycles = 4; }
            0x5D => { let extra_cycles = self.exclusive_or_with_a(bus, AddressingMode::AbsoluteX); self.last_cycles = 4 + extra_cycles; }
            0x59 => { let extra_cycles = self.exclusive_or_with_a(bus, AddressingMode::AbsoluteY); self.last_cycles = 4 + extra_cycles; }
            0x41 => { self.exclusive_or_with_a(bus, AddressingMode::IndirectX); self.last_cycles = 6; }
            0x51 => { let extra_cycles = self.exclusive_or_with_a(bus, AddressingMode::IndirectY); self.last_cycles = 5 + extra_cycles; }

            // Flag instructions
            0x18 => { self.p.remove(Status::C); self.last_cycles = 2; }
            0x38 => { self.p.insert(Status::C); self.last_cycles = 2; }
            0x58 => { self.p.remove(Status::I); self.last_cycles = 2; }
            0x78 => { self.p.insert(Status::I); self.last_cycles = 2; }
            0xB8 => { self.p.remove(Status::V); self.last_cycles = 2; }
            0xD8 => { self.p.remove(Status::D); self.last_cycles = 2; }
            0xF8 => { self.p.insert(Status::D); self.last_cycles = 2; }

            // INC (Increment memory)
            0xE6 => { self.increment_memory(bus, AddressingMode::ZeroPage, 1); self.last_cycles = 5; }
            0xF6 => { self.increment_memory(bus, AddressingMode::ZeroPageX, 1); self.last_cycles = 6; }
            0xEE => { self.increment_memory(bus, AddressingMode::Absolute, 1); self.last_cycles = 6; }
            0xFE => { self.increment_memory(bus, AddressingMode::AbsoluteX, 1); self.last_cycles = 7; }

            // JMP
            0x4C => { self.jump(bus, AddressingMode::Absolute); self.last_cycles = 3; }
            0x6C => { self.jump(bus, AddressingMode::Indirect); self.last_cycles = 5; }
 
            // JSR
            0x20 => { let addr = self._read_pc16(bus); self._stack_push16(bus, self.pc.wrapping_sub(1));
                 self.pc = addr; self.last_cycles = 6;
            }

            // LDA (Load Accumulator)
            0xA9 => { self.a = self._read_pc(bus); self.update_z_n_flags(self.a); self.last_cycles = 2; }
            0xA5 => { self.load_register_a(bus, AddressingMode::ZeroPage); self.last_cycles = 3; }
            0xB5 => { self.load_register_a(bus, AddressingMode::ZeroPageX); self.last_cycles = 4; }
            0xAD => { self.load_register_a(bus, AddressingMode::Absolute); self.last_cycles = 4; }
            0xBD => { let extra_cycles = self.load_register_a(bus, AddressingMode::AbsoluteX); self.last_cycles = 4 + extra_cycles; }
            0xB9 => { let extra_cycles = self.load_register_a(bus, AddressingMode::AbsoluteY); self.last_cycles = 4 + extra_cycles; }
            0xA1 => { self.load_register_a(bus, AddressingMode::IndirectX); self.last_cycles = 6; }
            0xB1 => { let extra_cycles = self.load_register_a(bus, AddressingMode::IndirectY); self.last_cycles = 5 + extra_cycles; }

            // LDX
            0xA2 => { self.x = self._read_pc(bus); self.update_z_n_flags(self.x); self.last_cycles = 2; }
            0xA6 => { self.load_register_x(bus, AddressingMode::ZeroPage); self.last_cycles = 3; }
            0xB6 => { self.load_register_x(bus, AddressingMode::ZeroPageY); self.last_cycles = 4; }
            0xAE => { self.load_register_x(bus, AddressingMode::Absolute); self.last_cycles = 4; }
            0xBE => { let extra_cycles = self.load_register_x(bus, AddressingMode::AbsoluteY); self.last_cycles = 4 + extra_cycles; }

            // LDY
            0xA0 => { self.y = self._read_pc(bus); self.update_z_n_flags(self.y); self.last_cycles = 2; }
            0xA4 => { self.load_register_y(bus, AddressingMode::ZeroPage); self.last_cycles = 3; }
            0xB4 => { self.load_register_y(bus, AddressingMode::ZeroPageX); self.last_cycles = 4; }
            0xAC => { self.load_register_y(bus, AddressingMode::Absolute); self.last_cycles = 4; }
            0xBC => { let extra_cycles = self.load_register_y(bus, AddressingMode::AbsoluteX); self.last_cycles = 4 + extra_cycles; }

            // LSR (Logical Shift Right)
            0x4A => { self.logical_shift_right_a(); self.last_cycles = 2; }
            0x46 => { self.logical_shift_right_memory(bus, AddressingMode::ZeroPage); self.last_cycles = 5; }
            0x56 => { self.logical_shift_right_memory(bus, AddressingMode::ZeroPageX); self.last_cycles = 6; }
            0x4E => { self.logical_shift_right_memory(bus, AddressingMode::Absolute); self.last_cycles = 6; }
            0x5E => { self.logical_shift_right_memory(bus, AddressingMode::AbsoluteX); self.last_cycles = 7; }

            // NOP
            0xEA => { self.last_cycles = 2; }

            // ORA (Bitwise OR with Accumulator)
            0x09 => { self.op_ora(bus, AddressingMode::Immediate); self.last_cycles = 2; }
            0x05 => { self.op_ora(bus, AddressingMode::ZeroPage); self.last_cycles = 3; }
            0x15 => { self.op_ora(bus, AddressingMode::ZeroPageX); self.last_cycles = 4; }
            0x0D => { self.op_ora(bus, AddressingMode::Absolute); self.last_cycles = 4; }
            0x1D => { let extra_cycles = self.op_ora(bus, AddressingMode::AbsoluteX); self.last_cycles = 4 + extra_cycles; }
            0x19 => { let extra_cycles = self.op_ora(bus, AddressingMode::AbsoluteY); self.last_cycles = 4 + extra_cycles; }
            0x01 => { self.op_ora(bus, AddressingMode::IndirectX); self.last_cycles = 6; }
            0x11 => { let extra_cycles = self.op_ora(bus, AddressingMode::IndirectY); self.last_cycles = 5 + extra_cycles; }

            // Register Instructions
            // TAX
            0xAA => { self.x = self.a; self.update_z_n_flags(self.a); self.last_cycles = 2; }
            // TXA
            0x8A => { self.a = self.x; self.update_z_n_flags(self.x); self.last_cycles = 2; }
            // DEX
            0xCA => { self.increment_x(-1); self.last_cycles = 2; }
            // INX
            0xE8 => { self.increment_x(1); self.last_cycles = 2; }
            // TAY
            0xA8 => { self.y = self.a; self.update_z_n_flags(self.a); self.last_cycles = 2; }
            // TYA
            0x98 => { self.a = self.y; self.update_z_n_flags(self.y); self.last_cycles = 2; }
            // DEY
            0x88 => { self.increment_y(-1); self.last_cycles = 2; }
            // INY
            0xC8 => { self.increment_y(1); self.last_cycles = 2; }

            // ROL (Rotate Left)
            0x2A => { self.rotate_left_a(); self.last_cycles = 2; }
            0x26 => { self.rotate_left_memory(bus, AddressingMode::ZeroPage); self.last_cycles = 5; }
            0x36 => { self.rotate_left_memory(bus, AddressingMode::ZeroPageX); self.last_cycles = 6; }
            0x2E => { self.rotate_left_memory(bus, AddressingMode::Absolute); self.last_cycles = 6; }
            0x3E => { self.rotate_left_memory(bus, AddressingMode::AbsoluteX); self.last_cycles = 7; }

            // ROR (Rotate Right)
            0x6A => { self.rotate_right_a(); self.last_cycles = 2; }
            0x66 => { self.rotate_right_memory(bus, AddressingMode::ZeroPage); self.last_cycles = 5; }
            0x76 => { self.rotate_right_memory(bus, AddressingMode::ZeroPageX); self.last_cycles = 6; }
            0x6E => { self.rotate_right_memory(bus, AddressingMode::Absolute); self.last_cycles = 6; }
            0x7E => { self.rotate_right_memory(bus, AddressingMode::AbsoluteX); self.last_cycles = 7; }

            // RTI
            0x40 => { let raw_flags:u8 = self._stack_pop8(bus);
                // 0x10 = Status::B, 0x20 = Status::U
                let sanitized_flags = (raw_flags & !0x10) | 0x20;
                self.p = Status::from_bits_truncate(sanitized_flags);
                self.pc = self._stack_pop16(bus); self.is_running = true; self.last_cycles = 6;
            }
            // RTS
            0x60 => { self.pc = self._stack_pop16(bus).wrapping_add(1); self.last_cycles = 6; }

            // SBC (Subtract with carry), 0xEB is unofficial opcode
            0xE9 | 0xEB => { self._op_sbc(bus, AddressingMode::Immediate); self.last_cycles = 2; }
            0xE5 => { self._op_sbc(bus, AddressingMode::ZeroPage); self.last_cycles = 3; }
            0xF5 => { self._op_sbc(bus, AddressingMode::ZeroPageX); self.last_cycles = 4; }
            0xED => { self._op_sbc(bus, AddressingMode::Absolute); self.last_cycles = 4; }
            0xFD => { let extra_cycles = self._op_sbc(bus, AddressingMode::AbsoluteX); self.last_cycles = 4 + extra_cycles; }
            0xF9 => { let extra_cycles = self._op_sbc(bus, AddressingMode::AbsoluteY); self.last_cycles = 4 + extra_cycles; }
            0xE1 => { self._op_sbc(bus, AddressingMode::IndirectX); self.last_cycles = 6; }
            0xF1 => { let extra_cycles = self._op_sbc(bus, AddressingMode::IndirectY); self.last_cycles = 5 + extra_cycles; }

            // STA (Store accumulator)
            0x85 => { let addr = self.get_operand_address(bus, AddressingMode::ZeroPage); bus.write_byte(addr, self.a); self.last_cycles = 3; }
            0x95 => { let addr = self.get_operand_address(bus, AddressingMode::ZeroPageX); bus.write_byte(addr, self.a); self.last_cycles = 4; }
            0x8D => { let addr = self.get_operand_address(bus, AddressingMode::Absolute); bus.write_byte(addr, self.a); self.last_cycles = 4; }
            0x9D => { self._op_indexed_store(bus, AddressingMode::AbsoluteX, self.a); }
            0x99 => { self._op_indexed_store(bus, AddressingMode::AbsoluteY, self.a); }
            0x81 => { let addr = self.get_operand_address(bus, AddressingMode::IndirectX); bus.write_byte(addr, self.a); self.last_cycles = 6; }
            0x91 => { self._op_indexed_store(bus, AddressingMode::IndirectY, self.a); }

            // Stack instructions
            // 0x9A=TXS, 0xBA=TSX, 0x48=PHA, 0x68=PLA, 0x08=PHP, 0x28=PLP
            0x9A => { self.sp = self.x; self.last_cycles = 2; }
            0xBA => { self.x = self.sp; self.update_z_n_flags(self.sp); self.last_cycles = 2; }
            0x48 => { self._stack_push8(bus, self.a); self.last_cycles = 3; }
            0x68 => { self.a = self._stack_pop8(bus); self.update_z_n_flags(self.a); self.last_cycles = 4; }
            0x08 => { 
                let flags:u8 = self.p.bits() | 0x30;
                self._stack_push8(bus, flags);
                self.last_cycles = 3; 
            }
            0x28 => {    
                let raw_flags:u8 = self._stack_pop8(bus);
                let sanitized_flags = (raw_flags & !0x10) | 0x20;
                self.p = Status::from_bits_truncate(sanitized_flags);
                self.last_cycles = 4; 
            }

            // STX (Store X)
            0x86 => { let addr = self.get_operand_address(bus, AddressingMode::ZeroPage); bus.write_byte(addr, self.x); self.last_cycles = 3; }
            0x96 => { let addr = self.get_operand_address(bus, AddressingMode::ZeroPageY); bus.write_byte(addr, self.x); self.last_cycles = 4; }
            0x8E => { let addr = self.get_operand_address(bus, AddressingMode::Absolute); bus.write_byte(addr, self.x); self.last_cycles = 4; }
            // STY (Store Y)
            0x84 => { let addr = self.get_operand_address(bus, AddressingMode::ZeroPage); bus.write_byte(addr, self.y); self.last_cycles = 3; }
            0x94 => { let addr = self.get_operand_address(bus, AddressingMode::ZeroPageX); bus.write_byte(addr, self.y); self.last_cycles = 4; }
            0x8C => { let addr = self.get_operand_address(bus, AddressingMode::Absolute); bus.write_byte(addr, self.y); self.last_cycles = 4; }

            // Unofficial opcodes
            0x0B | 0x2B => {
                // ANC Immediate
                let val = self.get_operand_address(bus, AddressingMode::Immediate) as u8;
                self.a &= val;                              // Perform bitwise AND with Accumulator
    
                // Update standard flags
                self.update_z_n_flags(self.a);
    
                // ANC Specific Quirk: Copy Bit 7 of the result directly into the Carry flag
                let carry = self.a & 0x80 != 0;
                if carry { self.p.insert(Status::C); } else { self.p.remove(Status::C); }
                self.last_cycles = 2;
            }
            0x07 => { // SLO
                self.op_slo(bus, AddressingMode::ZeroPage);
                self.last_cycles = 5;
            }
            0x17 => { // SLO
                self.op_slo(bus, AddressingMode::ZeroPageX);
                self.last_cycles = 6;
            }
            0x0F => { // SLO
                self.op_slo(bus, AddressingMode::Absolute);
                self.last_cycles = 6;
            }
            0x1F => { // SLO
                self.op_slo(bus, AddressingMode::AbsoluteX);
                self.last_cycles = 7;
            }
            0x1B => { // SLO
                self.op_slo(bus, AddressingMode::AbsoluteY);
                self.last_cycles = 7;
            }
            0x03 => { // SLO
                self.op_slo(bus, AddressingMode::IndirectX);
                self.last_cycles = 8;
            }
            0x13 => { // SLO
                self.op_slo(bus, AddressingMode::IndirectY);
                self.last_cycles = 8;
            }

            0x33 => { 
                let addr = self.get_operand_address(bus, AddressingMode::IndirectY);
                
                // ROL Memory Step
                let mut val = bus.read_byte(addr);
                let old_carry = self.p.contains(Status::C) as u8;
                if (val & 0x80) != 0 { self.p.insert(Status::C); } else { self.p.remove(Status::C); }
                val = (val << 1) | old_carry;
                bus.write_byte(addr, val);
                
                // AND step
                self.a &= val;
                self.update_z_n_flags(self.a);
                
                self.last_cycles = 8;
            }
            
            0x7B => {
                self.rotate_right_memory_then_add_to_a(bus, AddressingMode::AbsoluteY);
                self.last_cycles = 7;
            }

            0x7F => {
                self.rotate_right_memory_then_add_to_a(bus, AddressingMode::AbsoluteX);
                self.last_cycles = 7;
            }

            0x97 => { // AAX / SAX (Zero Page, Y)
                let addr = self.get_operand_address(bus, AddressingMode::ZeroPageY);
                let result = self.a & self.x;
                bus.write_byte(addr, result);
                self.last_cycles = 4;
            }

            0xB7 => { // LAX (Zero Page, Y)
                let addr = self.get_operand_address(bus, AddressingMode::ZeroPageY);
                let value = bus.read_byte(addr);
                self.a = value;
                self.x = value;
                self.update_z_n_flags(value);
                self.last_cycles = 4;
            }

            0x80 | 0x82 | 0x89 | 0xC2 | 0xE2 => { self._read_pc(bus); self.last_cycles = 2; }
            0x04 | 0x44 | 0x64 => { self._read_pc(bus); self.last_cycles = 3; }
            0x14 | 0x34 | 0x54 | 0x74 | 0xD4 | 0xF4 => { self._read_pc(bus); self.last_cycles = 4; }
            0x0C | 0xFC | 0x1C | 0x3C | 0x5C | 0x7C | 0xDC => {
                let mode = match opcode {
                    0x1C | 0x3C | 0x5C | 0x7C | 0xDC | 0xFC => AddressingMode::AbsoluteX,
                    _ => AddressingMode::Absolute
                };
                self.get_operand_address(bus, mode);
                self.last_cycles = 4 + (self.operand_address_crossed_page as u32) as u8;
            }
            0x02 | 0x12 | 0x1A | 0x22 | 0x32 | 0x3A | 0x42 | 0x52 | 0x5A => { self.last_cycles = 2; }
            0x62 | 0x72 | 0x7A | 0x92 | 0xB2 | 0xD2 | 0xDA | 0xF2 | 0xFA => { self.last_cycles = 2; }

            _=> { println!("Unimplemented opcode {:x} at {:x}", opcode, self.pc); }
        }
    }

    // Arithmetic and logic
    fn _op_adc(&mut self, bus: &mut dyn AddressBus, p_addressing_mode: AddressingMode) -> u8 {
        let addr:u16 = self.get_operand_address(bus, p_addressing_mode);
        let value:u8 = bus.read_byte(addr);
        self.add_with_carry_logic(value);
        if self.operand_address_crossed_page { 1 } else { 0 }
    }

    fn _op_sbc(&mut self, bus: &mut dyn AddressBus, p_addressing_mode: AddressingMode) -> u8 {
        let addr:u16 = self.get_operand_address(bus, p_addressing_mode);
        let value:u8 = bus.read_byte(addr);
        self.add_with_carry_logic(value ^ 0xFF);
        if self.operand_address_crossed_page { 1 } else { 0 }
    }

    // Standard indexed store execution with intermediate dummy read cycle emulation
    fn _op_indexed_store(&mut self, bus: &mut dyn AddressBus, p_addressing_mode: AddressingMode, val_to_write: u8) {
        match p_addressing_mode {
            AddressingMode::AbsoluteX => {
                let base: u16 = bus.read_word(self.pc);
                self.pc = (self.pc + 2) & 0xFFFF;
                let addr: u16 = base.wrapping_add(self.x as u16);
                
                // Emulate dummy read of uncrossed page cycle
                let dummy_addr = (base & 0xFF00) | (addr & 0x00FF);
                bus.read_byte(dummy_addr);
                
                bus.write_byte(addr, val_to_write);
                self.last_cycles = 5;
            }
            AddressingMode::AbsoluteY => {
                let base: u16 = bus.read_word(self.pc);
                self.pc = (self.pc + 2) & 0xFFFF;
                let addr: u16 = base.wrapping_add(self.y as u16);
                
                // Emulate dummy read of uncrossed page cycle
                let dummy_addr = (base & 0xFF00) | (addr & 0x00FF);
                bus.read_byte(dummy_addr);
                
                bus.write_byte(addr, val_to_write);
                self.last_cycles = 5;
            }
            AddressingMode::IndirectY => {
                let ptr = bus.read_byte(self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
    
                let lo = bus.read_byte(ptr) as u16;
                let hi = bus.read_byte(((ptr as u8).wrapping_add(1)) as u16) as u16;
                let base = (hi << 8) | lo;
                let addr = base.wrapping_add(self.y as u16);

                // Emulate dummy read of uncrossed page cycle
                let dummy_addr = (base & 0xFF00) | (addr & 0x00FF);
                bus.read_byte(dummy_addr);

                bus.write_byte(addr, val_to_write);
                self.last_cycles = 6;
            }
            _ => {}
        }
    }

    fn add_with_carry_logic(&mut self, value: u8) {
        let carry_in:u8 = if self.p.contains(Status::C) { 1 } else { 0 };

        if self.config.has_bcd && self.p.contains(Status::D) {
            let mut low_nibble = (self.a & 0x0F) + (value & 0x0F) + carry_in;
            let mut high_nibble = (self.a >> 4) + (value >> 4);

            if low_nibble > 9 {
                low_nibble += 6;
                high_nibble += 1; // Carry over into the tens digit
            }

            let binary_sum = (self.a as u16) + (value as u16) + (carry_in as u16);
            let uncorrected_a = (binary_sum & 0xFF) as u8;
            if (!(self.a ^ value) & (self.a ^ uncorrected_a) & 0x80) != 0 {
                self.p.insert(Status::V);
            } else {
                self.p.remove(Status::V);
            }

            // Correct the upper digit if it exceeds 9
            if high_nibble > 9 {
                high_nibble += 6;
                self.p.insert(Status::C);
            } else {
                self.p.remove(Status::C);
            }

            let result_a = ((high_nibble << 4) & 0xF0) | (low_nibble & 0x0F);
            
            self.a = result_a;
            if self.config.is_c02 {
                self.update_z_n_flags(self.a);
            } else {
                self.update_z_n_flags(uncorrected_a);
            }
        } else {
            let result:u16 = (self.a as u16) + (value as u16) + (carry_in as u16);
            if result > 0xFF { self.p.insert(Status::C); } else { self.p.remove(Status::C); }
            let new_a:u8 = (result & 0xFF) as u8;
            if (!(self.a ^ value) & (self.a ^ new_a) & 0x80) != 0 {
                self.p.insert(Status::V); } else { self.p.remove(Status::V); }
            self.a = new_a;
            self.update_z_n_flags(self.a);
        }
    }

    fn _op_and_a(&mut self, bus: &mut dyn AddressBus, p_addressing_mode: AddressingMode) -> u8 {
        let addr:u16 = self.get_operand_address(bus, p_addressing_mode);
        let value:u8 = bus.read_byte(addr);
        self.a &= value;
        self.update_z_n_flags(self.a);
        if self.operand_address_crossed_page { 1 } else { 0 }
    }

    fn arithmetic_shift_left_a(&mut self) {
        let value:u8 = self.a;
        if value & 0x80 != 0 {
            self.p.insert(Status::C);
        } else {
            self.p.remove(Status::C);
        }
        let result: u8 = value << 1;
        self.a = result;
        self.update_z_n_flags(result);
    }

    fn _op_asl_memory(&mut self, bus: &mut dyn AddressBus, p_addressing_mode: AddressingMode) {
        let addr = self.get_operand_address(bus, p_addressing_mode);
        self.asl_at_address(bus, addr);
    }

    fn op_slo(&mut self, bus: &mut dyn AddressBus, p_addressing_mode: AddressingMode) -> u8 {
        let addr = self.get_operand_address(bus, p_addressing_mode);
        let shifted_value = self.asl_at_address(bus, addr);
        self.a = self.a | shifted_value;
        self.update_z_n_flags(self.a);
        if self.operand_address_crossed_page { 1 } else { 0 }
    }

    fn bit_test_a(&mut self, bus: &mut dyn AddressBus, p_addressing_mode: AddressingMode) {
        let addr = self.get_operand_address(bus, p_addressing_mode);
        let value = bus.read_byte(addr);
        let result = value & self.a;
        if result == 0 { self.p.insert(Status::Z); } else { self.p.remove(Status::Z); }
        if !matches!(p_addressing_mode, AddressingMode::Immediate) {
            if (value & 0x80) != 0 { self.p.insert(Status::N); } else { self.p.remove(Status::N); }
            if (value & 0x40) != 0 { self.p.insert(Status::V); } else { self.p.remove(Status::V); }
        }
    }

    fn compare_a(&mut self, bus: &mut dyn AddressBus, p_addressing_mode: AddressingMode) -> u8 {
        let addr = self.get_operand_address(bus, p_addressing_mode);
        let value = bus.read_byte(addr);
        let result = self.a.wrapping_sub(value);
        if value <= self.a { self.p.insert(Status::C); } else { self.p.remove(Status::C); }
        self.update_z_n_flags(result);
        if self.operand_address_crossed_page { 1 } else { 0 }
    }

    fn compare_x(&mut self, bus: &mut dyn AddressBus, p_addressing_mode: AddressingMode) -> u8 {
        let addr = self.get_operand_address(bus, p_addressing_mode);
        let value = bus.read_byte(addr);
        let result = self.x.wrapping_sub(value);
        if value <= self.x { self.p.insert(Status::C); } else { self.p.remove(Status::C); }
        self.update_z_n_flags(result);
        if self.operand_address_crossed_page { 1 } else { 0 }
    }

    fn compare_y(&mut self, bus: &mut dyn AddressBus, p_addressing_mode: AddressingMode) -> u8 {
        let addr = self.get_operand_address(bus, p_addressing_mode);
        let value = bus.read_byte(addr);
        let result = self.y.wrapping_sub(value);
        if value <= self.y { self.p.insert(Status::C); } else { self.p.remove(Status::C); }
        self.update_z_n_flags(result);
        if self.operand_address_crossed_page { 1 } else { 0 }
    }

    fn exclusive_or_with_a(&mut self, bus: &mut dyn AddressBus, p_addressing_mode: AddressingMode) -> u8 {
        let addr:u16 = self.get_operand_address(bus, p_addressing_mode);
        let value:u8 = bus.read_byte(addr);
        self.a ^= value;
        self.update_z_n_flags(self.a);
        if self.operand_address_crossed_page { 1 } else { 0 }
    }

    fn logical_shift_right_a(&mut self) {
        let value = self.a;
        if (value & 0x01) != 0 { self.p.insert(Status::C); } else { self.p.remove(Status::C); }
        let shifted = value >> 1;
        self.a = shifted;
        self.update_z_n_flags(shifted);
    }

    fn logical_shift_right_memory(&mut self, bus: &mut dyn AddressBus, p_addressing_mode: AddressingMode) {
        let addr = self.get_operand_address(bus, p_addressing_mode);
        let value = bus.read_byte(addr);
        if (value & 0x01) != 0 { self.p.insert(Status::C); } else { self.p.remove(Status::C) }
        let shifted = value >> 1;
        bus.write_byte(addr, shifted);
        self.update_z_n_flags(shifted);
    }

    fn op_ora(&mut self,bus: &mut dyn AddressBus, p_addressing_mode: AddressingMode) -> u8 {
        let addr = self.get_operand_address(bus, p_addressing_mode);
        let value = bus.read_byte(addr);
        self.a = self.a | value;
        self.update_z_n_flags(self.a);
        if self.operand_address_crossed_page { 1 } else { 0 }
    }

    fn rotate_left_a(&mut self) {
        let old_value = self.a;
        let old_carry = self.p.contains(Status::C);
        if old_value & 0x80 != 0 { self.p.insert(Status::C); } else { self.p.remove(Status::C); }

        let mut result = old_value << 1;
        if old_carry { result |= 0x01; }

        self.a = result;
        self.update_z_n_flags(result);
    }

    fn rotate_right_a(&mut self) {
        let old_value = self.a;
        let old_carry = self.p.contains(Status::C);
        
        // Set new carry from old bit 0
        if old_value & 0x01 != 0 { self.p.insert(Status::C); } else { self.p.remove(Status::C); }
        
        let mut result = old_value >> 1;
        if old_carry { result |= 0x80; } // Shift old carry into bit 7
        
        self.a = result;
        self.update_z_n_flags(result);
    }

    fn rotate_left_memory(&mut self, bus: &mut dyn AddressBus, p_addressing_mode: AddressingMode) {
        let addr = self.get_operand_address(bus, p_addressing_mode);
        let old_value = bus.read_byte(addr);
        let old_carry = self.p.contains(Status::C);
        
        // Set new carry from old bit 7
        if old_value & 0x80 != 0 { self.p.insert(Status::C); } else { self.p.remove(Status::C); }
        
        let mut result = old_value << 1;
        if old_carry { result |= 0x01; } // Shift old carry into bit 0
        
        bus.write_byte(addr, result);
        self.update_z_n_flags(result);
    }

    fn rotate_right_memory(&mut self, bus: &mut dyn AddressBus, p_addressing_mode: AddressingMode) {
        let addr = self.get_operand_address(bus, p_addressing_mode);
        let old_value = bus.read_byte(addr);
        let old_carry = self.p.contains(Status::C);
        
        // Set new carry from old bit 0
        if old_value & 0x01 != 0 { self.p.insert(Status::C); } else { self.p.remove(Status::C); }
        
        let mut result = old_value >> 1;
        if old_carry { result |= 0x80; } // Shift old carry into bit 7
        
        bus.write_byte(addr, result);
        self.update_z_n_flags(result);
    }

    fn rotate_right_memory_then_add_to_a(&mut self, bus: &mut dyn AddressBus, p_addressing_mode: AddressingMode) {
        let addr = self.get_operand_address(bus, p_addressing_mode);
        let old_value = bus.read_byte(addr);
        let old_carry = self.p.contains(Status::C);
        
        // Set new carry from old bit 0
        if old_value & 0x01 != 0 { self.p.insert(Status::C); } else { self.p.remove(Status::C); }
        
        let mut result = old_value >> 1;
        if old_carry { result |= 0x80; } // Shift old carry into bit 7
        
        bus.write_byte(addr, result);
        self.update_z_n_flags(result);
        self.add_with_carry_logic(result);
    }

    fn asl_at_address(&mut self, bus: &mut dyn AddressBus, addr: u16) -> u8 {
        let value = bus.read_byte(addr);

        // Set carry using bit 7
        if value & 0x80 != 0 { 
            self.p.insert(Status::C); 
        } else {
            self.p.remove(Status::C); 
        }

        let result = value << 1;

        bus.write_byte(addr, result);
        self.update_z_n_flags(result);
    
        result
    }

    // Flag helpers
    #[inline(always)]
    fn update_z_n_flags(&mut self, value: u8) {
        if value == 0 {
            self.p.insert(Status::Z);
        } else {
            self.p.remove(Status::Z);
        }
        if value & 0x80 != 0 {
            self.p.insert(Status::N);
        } else {
            self.p.remove(Status::N);
        }
    }

    // Control Flow
    fn jump(&mut self, bus: &mut dyn AddressBus, p_addressing_mode: AddressingMode) {
        if p_addressing_mode == AddressingMode::Indirect {
            let ptr = bus.read_word(self.pc);
            if self.config.has_jmp_bug {
                let lo = bus.read_byte(ptr) as u16;
                let hi = if (ptr & 0xFF) == 0xFF {
                    bus.read_byte(ptr & 0xFF00)
                } else {
                    bus.read_byte(ptr + 1)
                } as u16;
                 self.pc = (hi << 8) | lo;
            } else {
                let lo = bus.read_byte(ptr) as u16;
                let hi = bus.read_byte(ptr.wrapping_add(1)) as u16;
                self.pc = (hi << 8) | lo;
            }
        } else {
            let addr = bus.read_word(self.pc);
            self.pc = addr;
        }
    }

    fn branch_if_flag_matches(&mut self, bus: & mut dyn AddressBus, p_flag: Status, p_is_set: bool) -> u8 {
        let offset = bus.read_byte(self.pc) as i8;
        self.pc = self.pc.wrapping_add(1);

        let mut cycles_spent = 2;

        let flag_actually_set: bool = self.p.contains(p_flag);
        if flag_actually_set == p_is_set {
            cycles_spent += 1;

            let base_pc = self.pc;
            let target_pc = (base_pc as i16).wrapping_add(offset as i16) as u16;

            if (target_pc & 0xFF00) != (base_pc & 0xFF00) {
                cycles_spent += 1;
            }
            self.pc = target_pc;
        }
        cycles_spent
    }

    fn _read_pc(&mut self, bus: &mut dyn AddressBus) -> u8 {
        let result:u8 = bus.read_byte(self.pc);
        self.pc = self.pc.wrapping_add(1);
        result
    }

    fn _read_pc16(&mut self, bus: &mut dyn AddressBus) -> u16 {
        let lo = self._read_pc(bus) as u16;
        let hi = self._read_pc(bus) as u16;
        (hi << 8) | lo
    }

    fn get_operand_address(&mut self, bus: &mut dyn AddressBus, p_addressing_mode: AddressingMode) -> u16 {
        self.operand_address_crossed_page = false;
        match p_addressing_mode {
            AddressingMode::Immediate => {
                let addr:u16 = self.pc;
                self.pc = self.pc.wrapping_add(1);
                addr
            }
            AddressingMode::ZeroPage => {
                let addr:u16 = bus.read_byte(self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                addr
            }
            AddressingMode::ZeroPageX => {
                let addr:u16 = bus.read_byte(self.pc).wrapping_add(self.x) as u16;
                self.pc = self.pc.wrapping_add(1);
                addr  
            }
            AddressingMode::ZeroPageY => {
                let addr:u16 = bus.read_byte(self.pc).wrapping_add(self.y) as u16;
                self.pc = self.pc.wrapping_add(1);
                addr
            }
            AddressingMode::Absolute => {
                let addr:u16 = bus.read_word(self.pc);
                self.pc = self.pc.wrapping_add(2);
                addr
            }
            AddressingMode::AbsoluteX => {
                let base:u16 = bus.read_word(self.pc);
                self.pc = self.pc.wrapping_add(2);
                let addr:u16 = base.wrapping_add(self.x as u16);
                self.operand_address_crossed_page = (base & 0xFF00) != (addr & 0xFF00);
                addr
            }
            AddressingMode::AbsoluteY => {
                let base:u16 = bus.read_word(self.pc);
                self.pc = self.pc.wrapping_add(2);
                let addr:u16 = base.wrapping_add(self.y as u16);
                self.operand_address_crossed_page = (base & 0xFF00) != (addr & 0xFF00);
                addr
            }
            AddressingMode::Indirect => {
                let ptr: u16 = bus.read_word(self.pc);
                self.pc = self.pc.wrapping_add(2);
                let lo = bus.read_byte(ptr) as u16;
                let hi = if (ptr & 0xFF) == 0xFF { bus.read_byte(ptr & 0xFF00) }
                else { bus.read_byte(ptr.wrapping_add(1) as u16) } as u16;
                (hi << 8) | lo
            }
            AddressingMode::IndirectX => {
                let base = bus.read_byte(self.pc);
                self.pc = self.pc.wrapping_add(1);
    
                let ptr = base.wrapping_add(self.x);
                let lo = bus.read_byte(ptr as u16) as u16;
                let hi = bus.read_byte(ptr.wrapping_add(1) as u16) as u16;
                (hi << 8) | lo
            }
            AddressingMode::IndirectY => {
                let ptr = bus.read_byte(self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
    
                let lo = bus.read_byte(ptr) as u16;
                let hi = bus.read_byte((ptr.wrapping_add(1)) as u16) as u16;
                let base = (hi << 8) | lo;
    
                let addr = base.wrapping_add(self.y as u16);
                self.operand_address_crossed_page = (base & 0xFF00) != (addr & 0xFF00);
                addr
            }
            _ => { 0 }
        }
    }

    // A,X,Y Registers
    fn increment_memory(&mut self, bus: &mut dyn AddressBus, p_addressing_mode: AddressingMode, p_by_amount: i8) {
        let addr:u16 = self.get_operand_address(bus, p_addressing_mode);
        let value:u8 = bus.read_byte(addr);
        let result:u8 = value.wrapping_add(p_by_amount as u8);
        bus.write_byte(addr, result);
        self.update_z_n_flags(result);
    }

    fn increment_x(&mut self, p_by_amount: i8) {
        self.x = self.x.wrapping_add(p_by_amount as u8);
        self.update_z_n_flags(self.x);
    }

    fn increment_y(&mut self, p_by_amount: i8) {
        self.y = self.y.wrapping_add(p_by_amount as u8);
        self.update_z_n_flags(self.y);
    }

    fn load_register_a(&mut self, bus: &mut dyn AddressBus, p_addressing_mode: AddressingMode) -> u8 {
        let addr = self.get_operand_address(bus, p_addressing_mode);
        let value = bus.read_byte(addr);
        self.a = value;
        self.update_z_n_flags(value);
        if self.operand_address_crossed_page { 1 } else { 0 }
    }

    fn load_register_x(&mut self, bus: &mut dyn AddressBus, p_addressing_mode: AddressingMode) -> u8 {
        let addr = self.get_operand_address(bus, p_addressing_mode);
        let value = bus.read_byte(addr);
        self.x = value;
        self.update_z_n_flags(value);
        if self.operand_address_crossed_page { 1 } else { 0 }
    }

    fn load_register_y(&mut self, bus: &mut dyn AddressBus, p_addressing_mode: AddressingMode) -> u8 {
        let addr = self.get_operand_address(bus, p_addressing_mode);
        let value = bus.read_byte(addr);
        self.y = value;
        self.update_z_n_flags(value);
        if self.operand_address_crossed_page { 1 } else { 0 }
    }

    // Stack Related
    fn _stack_pop8(&mut self, bus: &mut dyn AddressBus) -> u8 {
        self.sp = self.sp.wrapping_add(1);
        let value:u8 = bus.read_byte(STACK_BASE + (self.sp as u16));
        value
    }

    fn _stack_pop16(&mut self, bus: &mut dyn AddressBus) -> u16 {
        let lo = self._stack_pop8(bus) as u16;
        let hi = self._stack_pop8(bus) as u16;
        (hi << 8) | lo
    }

    fn _stack_push8(&mut self, bus: &mut dyn AddressBus, p_value: u8) {
        bus.write_byte(STACK_BASE + (self.sp as u16), p_value);
        self.sp = self.sp.wrapping_sub(1);
    }

   fn _stack_push16(&mut self, bus: &mut dyn AddressBus, p_value: u16) {
        let hi:u8 = ((p_value >> 8) & 0xFF) as u8;
        let lo:u8 = (p_value & 0xFF) as u8;
        self._stack_push8(bus, hi);
        self._stack_push8(bus, lo);
    }

    // Interrupts
    fn brk(&mut self, bus: &mut dyn AddressBus) {
        self.pc = self.pc.wrapping_add(1);
        self._stack_push16(bus, self.pc);
        // Push Status register with B flag and I flags set
        let flags:u8 = self.p.bits() | 0x30;
        self._stack_push8(bus, flags);
        self.p.insert(Status::I);
        let lo = bus.read_byte(0xFFFE) as u16;
        let hi = bus.read_byte(0xFFFF) as u16;
        self.pc = (hi << 8) | lo;
    }

    fn trigger_nmi(&mut self, bus: &mut dyn AddressBus) -> u8 {
        self._stack_push16(bus, self.pc);
        let flags:u8 = (self.p.bits() & !0x10) | 0x20;
        self._stack_push8(bus, flags);
        self.p.insert(Status::I);
        let lo = bus.read_byte(0xFFFA) as u16;
        let hi = bus.read_byte(0xFFFB) as u16;
        self.pc = (hi << 8) | lo;
        godot_print!("({}): NMI triggered. PC set to {:04X} SP={:04X}", bus.total_cycles(), self.pc, self.sp);
        7
    }

    pub fn trigger_irq(&mut self, bus: &mut dyn AddressBus) -> u8 {
        self._stack_push16(bus, self.pc);
        let flags:u8 = (self.p.bits() & !0x10) | 0x20;
        self._stack_push8(bus, flags);
        self.p.insert(Status::I);
        let lo = bus.read_byte(0xFFFE) as u16;
        let hi = bus.read_byte(0xFFFF) as u16;
        self.pc = (hi << 8) | lo;
//        let current_sp=self.sp;
//        println!("({}): IRQ triggered. SP={current_sp}", self.total_cycles);
        7
    }
}