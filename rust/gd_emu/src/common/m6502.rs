const STACK_BASE: u16 = 0x100;

use crate::common::bus::AddressBus;

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

#[cfg(test)]
mod tests {
    use super::*;

    struct MockBus {
        ram: [u8; 65536],
    }

    impl MockBus {
        fn new() -> Self {
            Self { ram: [0; 65536] }
        }
    }

    impl AddressBus for MockBus {
        fn read_byte(&self, addr: u16) -> u8 {
            self.ram[addr as usize]
        }
        fn write_byte(&mut self, addr: u16, val: u8) {
            self.ram[addr as usize] = val;
        }
        fn is_nmi_line_asserted(&mut self) -> bool { false }
        fn is_irq_line_asserted(&mut self) -> bool { false }
    }
    #[test]
    fn test_jsr_and_rts_execution_and_stack_handling() {
        let mut cpu = M6502Cpu::new(CpuVariant::Ricoh2A03);
        let mut bus = MockBus::new();

        // 1. Arrange: Write a JSR instruction at 0xC000 targeting 0xC500
        cpu.pc = 0xC000;
        cpu.sp = 0xFF; // Start at top of stack

        // JSR opcode is 0x20. Target address is 0xC500 (low-byte 00, high-byte C5)
        bus.ram[0xC000] = 0x20;
        bus.ram[0xC001] = 0x00;
        bus.ram[0xC002] = 0xC5;

        // Write an RTS instruction at the target subroutine location 0xC500
        bus.ram[0xC500] = 0x60; // RTS opcode

        // 2. Act: Step through the JSR instruction (6 cycles total)
        for _ in 0..6 {
            cpu.step_one_cycle(&mut bus);
        }

        // 3. Assert: JSR should have successfully jumped and prepared the stack
        assert_eq!(cpu.pc, 0xC500, "PC should be at the target address 0xC500");
        assert_eq!(cpu.sp, 0xFD, "SP should have decremented twice (0xFF -> 0xFD)");
        // Verify return address (PC of last byte of JSR instruction: 0xC002) was pushed
        assert_eq!(bus.ram[0x100 + 0xFF], 0xC0, "Stack should contain PC High byte (0xC0)");
        assert_eq!(bus.ram[0x100 + 0xFE], 0x02, "Stack should contain PC Low byte (0x02)");

        // 4. Act: Now execute the RTS instruction (6 cycles total)
        for _ in 0..6 {
            cpu.step_one_cycle(&mut bus);
        }

        // 5. Assert: RTS should pull the address, increment it by 1, and return to 0xC003
        assert_eq!(cpu.pc, 0xC003, "PC should have cleanly returned to 0xC003");
        assert_eq!(cpu.sp, 0xFF, "SP should have wound back up to 0xFF");
    }
#[test]
fn test_branch_zero_page_and_page_boundary() {
    let mut cpu = M6502Cpu::new(CpuVariant::Ricoh2A03);
    let mut bus = MockBus::new();

    // --- CASE 1: Branch NOT Taken (2 Cycles) ---
    // BNE (0xD0) with an offset of +4. If Zero flag is SET, it shouldn't branch.
    cpu.pc = 0xC000;
    cpu.status.zero = true; 
    bus.ram[0xC000] = 0xD0; 
    bus.ram[0xC001] = 0x04; // Offset +4

    for _ in 0..2 { cpu.step_one_cycle(&mut bus); }
    assert_eq!(cpu.pc, 0xC002, "Branch not taken should just advance past the instruction (2 cycles)");

    // --- CASE 2: Branch Taken, Same Page (3 Cycles) ---
    // BNE (0xD0) with an offset of +4. Zero flag is CLEAR, so it should branch.
    cpu.pc = 0xC000;
    cpu.status.zero = false; 
    
    // We expect 3 cycles total for a taken branch on the same page
    for _ in 0..3 { cpu.step_one_cycle(&mut bus); }
    assert_eq!(cpu.pc, 0xC006, "Branch taken should advance PC by offset (0xC002 + 4 = 0xC006)");

    // --- CASE 3: Branch Taken, Crosses Page Boundary (4 Cycles) ---
    // Place the BNE right at the end of page 0xC0 (e.g., 0xC0FE). 
    // An offset of +4 will push the execution target into page 0xC1 (0xC104).
    cpu.pc = 0xC0FE;
    cpu.status.zero = false;
    bus.ram[0xC0FE] = 0xD0;
    bus.ram[0xC0FF] = 0x04; // 0xC100 + 4 = 0xC104

    // We expect 4 cycles total due to the page cross penalty
    for _ in 0..4 { cpu.step_one_cycle(&mut bus); }
    assert_eq!(cpu.pc, 0xC104, "Branch crossing page boundary should land at 0xC104");
}
#[test]
fn test_adc_flags_and_overflow() {
    let mut cpu = M6502Cpu::new(CpuVariant::Ricoh2A03);
    let mut bus = MockBus::new();

    // --- CASE 1: Standard Addition (No Carry, No Overflow) ---
    // A = 0x01, Memory = 0x02. Result should be 0x03.
    cpu.pc = 0xC000;
    cpu.a = 0x01;
    cpu.status.carry = false;
    bus.ram[0xC000] = 0x69; // ADC Immediate opcode
    bus.ram[0xC001] = 0x02; // Immediate value

    // ADC Immediate takes 2 cycles
    for _ in 0..2 { cpu.step_one_cycle(&mut bus); }
    assert_eq!(cpu.a, 0x03, "0x01 + 0x02 should equal 0x03");
    assert!(!cpu.status.carry, "Carry should be clear");
    assert!(!cpu.status.overflow, "Overflow should be clear");
    assert!(!cpu.status.zero, "Zero should be clear");

    // --- CASE 2: Unsigned Carry Generation ---
    // A = 0xFF, Memory = 0x01. Result should roll over to 0x00 and set Carry + Zero.
    cpu.pc = 0xC000;
    cpu.a = 0xFF;
    cpu.status.carry = false;
    bus.ram[0xC000] = 0x69;
    bus.ram[0xC001] = 0x01;

    for _ in 0..2 { cpu.step_one_cycle(&mut bus); }
    assert_eq!(cpu.a, 0x00, "0xFF + 0x01 should roll over to 0x00");
    assert!(cpu.status.carry, "Carry flag must be set (unsigned overflow)");
    assert!(cpu.status.zero, "Zero flag must be set");
    assert!(!cpu.status.overflow, "Signed overflow should NOT be set here");

    // --- CASE 3: Signed Overflow Trigger (Positive + Positive = Negative) ---
    // A = 127 (0x7F), Memory = 1 (0x01). 
    // In signed 8-bit math, 127 + 1 = 128, which is -128 (0x80). This triggers signed overflow!
    cpu.pc = 0xC000;
    cpu.a = 0x7F;
    cpu.status.carry = false;
    cpu.status.overflow = false;
    bus.ram[0xC000] = 0x69;
    bus.ram[0xC001] = 0x01;

    for _ in 0..2 { cpu.step_one_cycle(&mut bus); }
    assert_eq!(cpu.a, 0x80, "0x7F + 0x01 = 0x80");
    assert!(cpu.status.overflow, "Overflow flag MUST be set (Positive + Positive yielded a negative result)");
    assert!(!cpu.status.carry, "Unsigned carry should be clear");
}
#[test]
fn test_indirect_indexed_page_cross() {
    let mut cpu = M6502Cpu::new(CpuVariant::Ricoh2A03);
    let mut bus = MockBus::new();

    // Setup: We want to execute LDA ($20), Y 
    // Opcode: 0xB1, Zero-Page Vector Address: 0x20
    cpu.pc = 0xC000;
    bus.ram[0xC000] = 0xB1; 
    bus.ram[0xC001] = 0x20; 

    // Put the base pointer inside Zero Page $20 and $21
    // The vector points to 0x70E0
    bus.ram[0x0020] = 0xE0; // Low Byte
    bus.ram[0x0021] = 0x70; // High Byte

    // --- CASE 1: No Page Cross (5 Cycles) ---
    // Y = 0x05. Target = 0x70E0 + 0x05 = 0x70E5 (Same page: 0x70)
    cpu.y = 0x05;
    bus.ram[0x70E5] = 0x42; // Put a dummy value to read into A

    for _ in 0..5 { cpu.step_one_cycle(&mut bus); }
    assert_eq!(cpu.a, 0x42, "Accumulator should load the value 0x42");
    assert_eq!(cpu.pc, 0xC002, "PC should advance 2 bytes");

    // --- CASE 2: Page Cross Penalty (6 Cycles) ---
    // Reset CPU position
    cpu.pc = 0xC000;
    // Y = 0x30. Target = 0x70E0 + 0x30 = 0x7110 (Crosses page 0x70 -> 0x71!)
    cpu.y = 0x30;
    bus.ram[0x7110] = 0x99; // Value across the boundary

    // We expect 6 cycles here because of the page boundary crossing penalty
    for _ in 0..6 { cpu.step_one_cycle(&mut bus); }
    assert_eq!(cpu.a, 0x99, "Accumulator should load the value 0x99 across the page boundary");
}
#[test]
fn test_stack_push_pull_and_status_flags() {
    let mut cpu = M6502Cpu::new(CpuVariant::Ricoh2A03);
    let mut bus = MockBus::new();

    // --- CASE 1: PHA & PLA Data Integrity (3 + 4 Cycles) ---
    // Load A with 0x55, push it to stack, clear A, then pull it back.
    cpu.pc = 0xC000;
    cpu.sp = 0xFF; // Start at the very top of page 1
    cpu.a = 0x55;
    
    bus.ram[0xC000] = 0x48; // PHA Opcode (3 cycles)
    bus.ram[0xC001] = 0x68; // PLA Opcode (4 cycles)

    // Execute PHA
    for _ in 0..3 { cpu.step_one_cycle(&mut bus); }
    assert_eq!(cpu.sp, 0xFE, "SP should decrement to 0xFE after push");
    assert_eq!(bus.ram[0x01FF], 0x55, "Memory at 0x01FF should hold the pushed value 0x55");

    // Clear Accumulator to prove the pull actually modifies it
    cpu.a = 0x00; 

    // Execute PLA
    for _ in 0..4 { cpu.step_one_cycle(&mut bus); }
    assert_eq!(cpu.a, 0x55, "Accumulator should have recovered 0x55 from the stack");
    assert_eq!(cpu.sp, 0xFF, "SP should wind back up to 0xFF after pull");
    assert!(!cpu.status.zero, "Zero flag should be clear because 0x55 is non-zero");

    // --- CASE 2: PLP Flag Masking Rules (4 Cycles) ---
    // When flags are pulled via PLP, Bits 4 and 5 are handled strictly by the hardware.
    // Bit 4 (Break) is entirely ignored on PLP, and Bit 5 is always forced to 1.
    cpu.pc = 0xC000;
    cpu.sp = 0xFF;
    
    // We will simulate a status byte on the stack: 0x00 (All flags clear)
    bus.ram[0x01FF] = 0x00;
    bus.ram[0xC000] = 0x28; // PLP Opcode (4 cycles)

    for _ in 0..4 { cpu.step_one_cycle(&mut bus); }
    
    // Convert status to raw byte to verify bits 4 and 5
    let raw_status = cpu.status.to_u8(false); 
    assert_eq!(raw_status & 0x10, 0x00, "Bit 4 (B flag) must be 0 after PLP execution");
    // Note: If your framework explicitly separates or manages bit 5 as an active flag,
    // ensure it defaults back to true when working with raw stack bytes!
}
#[test]
fn test_bit_and_asl_status_flags() {
    let mut cpu = M6502Cpu::new(CpuVariant::Ricoh2A03);
    let mut bus = MockBus::new();

    // =========================================================================
    // --- PART 1: BIT (Bit Test) ---
    // The BIT instruction performs an AND between A and Memory.
    // - Zero flag = Set if (A AND Memory) == 0
    // - Negative flag = Set to Bit 7 of the memory value
    // - Overflow flag = Set to Bit 6 of the memory value
    // =========================================================================
    cpu.pc = 0xC000;
    cpu.a = 0x01; // Testing bit 0
    
    // Memory value has bits 7 and 6 set, but bit 0 is clear (0xC0 = 1100 0000)
    bus.ram[0xC000] = 0x24; // BIT Zero Page Opcode (3 cycles)
    bus.ram[0xC001] = 0x10; // Zero page address $10
    bus.ram[0x0010] = 0xC0; // Value at $10

    for _ in 0..3 { cpu.step_one_cycle(&mut bus); }

    assert!(cpu.status.zero, "Zero flag MUST be set because (0x01 AND 0xC0) == 0");
    assert!(cpu.status.negative, "Negative flag MUST match bit 7 of memory (1)");
    assert!(cpu.status.overflow, "Overflow flag MUST match bit 6 of memory (1)");

    // =========================================================================
    // --- PART 2: ASL (Arithmetic Shift Left) Accumulator ---
    // Shifts all bits left by 1. 
    // - Bit 7 is shifted out directly into the Carry flag.
    // - Bit 0 is filled with 0.
    // =========================================================================
    cpu.pc = 0xC000;
    cpu.a = 0x80; // Only bit 7 is set (1000 0000)
    cpu.status.carry = false;

    bus.ram[0xC000] = 0x0A; // ASL Accumulator Opcode (2 cycles)

    for _ in 0..2 { cpu.step_one_cycle(&mut bus); }

    assert_eq!(cpu.a, 0x00, "0x80 shifted left by 1 should equal 0x00");
    assert!(cpu.status.carry, "Carry flag MUST be set because bit 7 was 1");
    assert!(cpu.status.zero, "Zero flag MUST be set because accumulator rolled over to 0x00");
}
#[test]
fn test_rol_and_ror_circular_shifts() {
    let mut cpu = M6502Cpu::new(CpuVariant::Ricoh2A03);
    let mut bus = MockBus::new();

    // =========================================================================
    // --- PART 1: ROL (Rotate Left) Accumulator ---
    // Shifts all bits left. 
    // - Old Bit 7 goes INTO the Carry Flag.
    // - Old Carry Flag goes INTO Bit 0.
    // =========================================================================
    cpu.pc = 0xC000;
    cpu.a = 0x81;          // Bit 7 and Bit 0 are set (1000 0001)
    cpu.status.carry = true; // Carry starts as 1

    bus.ram[0xC000] = 0x2A; // ROL Accumulator Opcode (2 cycles)

    for _ in 0..2 { cpu.step_one_cycle(&mut bus); }

    // What to expect:
    // - Bit 7 (1) shifts out into Carry -> New Carry = 1
    // - Remaining bits shift left: 0000 0010
    // - Old Carry (1) shifts into Bit 0 -> 0000 0011 (0x03)
    assert_eq!(cpu.a, 0x03, "0x81 rotated left with Carry=1 should equal 0x03");
    assert!(cpu.status.carry, "Carry flag should be set to the old Bit 7 (1)");
    assert!(!cpu.status.zero, "Zero flag should be clear");

    // =========================================================================
    // --- PART 2: ROR (Rotate Right) Accumulator ---
    // Shifts all bits right.
    // - Old Bit 0 goes INTO the Carry Flag.
    // - Old Carry Flag goes INTO Bit 7.
    // =========================================================================
    cpu.pc = 0xC000;
    cpu.a = 0x01;           // Only Bit 0 is set (0000 0001)
    cpu.status.carry = false; // Carry starts as 0

    bus.ram[0xC000] = 0x6A; // ROR Accumulator Opcode (2 cycles)

    for _ in 0..2 { cpu.step_one_cycle(&mut bus); }

    // What to expect:
    // - Bit 0 (1) shifts out into Carry -> New Carry = 1
    // - Remaining bits shift right: 0000 0000
    // - Old Carry (0) shifts into Bit 7 -> 0000 0000 (0x00)
    assert_eq!(cpu.a, 0x00, "0x01 rotated right with Carry=0 should equal 0x00");
    assert!(cpu.status.carry, "Carry flag should be set to the old Bit 0 (1)");
    assert!(cpu.status.zero, "Zero flag should be set because accumulator is 0x00");
}
#[test]
fn test_hardware_nmi_interrupt_stack_and_vector() {
    let mut cpu = M6502Cpu::new(CpuVariant::Ricoh2A03);
    let mut bus = MockBus::new();

    // Setup: We place a NOP instruction at 0xC000.
    // We will simulate an NMI being triggered right as this instruction processes.
    cpu.pc = 0xC000;
    cpu.sp = 0xFF;
    cpu.status.zero = true; 
    cpu.status.negative = false;
    cpu.status.interrupt_disable=false;

    bus.ram[0xC000] = 0xEA; // NOP Opcode (2 cycles)

    // Pre-program the NMI Vector to point to handling routine at 0xD000
    bus.ram[0xFFFA] = 0x00; // Low Byte
    bus.ram[0xFFFB] = 0xD0; // High Byte

    // Execute the 2 cycles for NOP
    for _ in 0..2 { cpu.step_one_cycle(&mut bus); }
    
    // --- Trigger the Interrupt ---
    // At this point, PC has advanced past NOP to 0xC001. 
    // Now we invoke your interrupt sequence mechanism. 
    // (Adjust this line to match your exact internal trigger function name)
    cpu.setup_hardware_interrupt(Operation::Nmi, &mut bus);

    // An interrupt sequence takes 7 micro-cycles to complete its execution pipeline
    for _ in 0..7 { cpu.step_one_cycle(&mut bus); }

    // --- ASSERTS ---
    // 1. The PC should now be pointing at the NMI vector destination
    assert_eq!(cpu.pc, 0xD000, "PC should have jumped to the NMI vector handler address (0xD000)");

    // 2. The Stack Pointer should have moved down 3 slots (0xFF -> 0xFC)
    assert_eq!(cpu.sp, 0xFC, "SP should be at 0xFC after pushing PC high, PC low, and Status");

    // 3. Verify the Return Address pushed to the stack is exactly 0xC001
    assert_eq!(bus.ram[0x01FF], 0xC0, "Stack top (0x01FF) should hold return PC High byte (0xC0)");
    assert_eq!(bus.ram[0x01FE], 0x01, "Stack mid (0x01FE) should hold return PC Low byte (0xC001 right after NOP)");

    // 4. Verify the Status Byte pushed to the stack
    // During hardware interrupts (NMI/IRQ), Bit 4 (B flag) is pushed as 0. Bit 5 is always 1.
//    let mut expected_status_pushed = cpu.status.to_u8(false); // false means not a BRK instruction
//    assert_eq!(bus.ram[0x01FD], expected_status_pushed, "Stack bottom (0x01FD) should match the pushed status register layout");
//    assert_eq!(bus.ram[0x01FD], 38, "Stack bottom should match the state during step 5");
    assert_eq!(bus.ram[0x01FD], 34, "Stack should hold the status from BEFORE the interrupt was handled");
    let final_live_status = cpu.status.to_u8(false);
    assert_eq!(final_live_status, 38, "Live CPU status register should now have the Interrupt Disable flag set to true");
}
}



#[derive(Default, Clone, Copy)]
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
}

impl Operation {
    pub fn is_rmw(&self) -> bool {
        matches!(self, Operation::Lsr | Operation::Asl | Operation::Rol | Operation::Ror | Operation::Inc | Operation::Dec)
    }

    pub fn is_write(&self) -> bool {
        matches!(self, Operation::Sta | Operation::Stx | Operation::Sty)
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
    cycles_remaining: u8,
    instruction_step: u8,
    test_print: bool,
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
            test_print: false,
            last_cycles: 0,
            last_opcode: 0,
            total_cycles: 0,
            operand_address_crossed_page: false,
            is_running: false,
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

    pub fn is_interrupt_disabled(&self) -> bool {
        self.status.interrupt_disable
    }

    pub fn power_on(&mut self, bus: &impl AddressBus) {
        self.is_running = true;
        self.a = 0; self.x = 0; self.y = 0;
        self.sp = 0xFD;
        self.status.interrupt_disable = true;
        self.nmi_pending = false;
        self.prev_nmi_line = false;
        self.last_cycles = 0;
        self.last_opcode = 0;
        self.total_cycles = 0;
        self.operand_address_crossed_page = false;

        let lo = bus.read_byte(0xFFFC) as u16;
        let hi = bus.read_byte(0xFFFD) as u16;
        self.pc = (hi << 8) | lo;
    }

    pub fn reset(&mut self, bus: &impl AddressBus) {
        self.sp = self.sp.wrapping_sub(3);
        self.a = 0;
        self.x = 0;
        self.y = 0;
        self.status.interrupt_disable = true;
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

    pub fn step_one_cycle(&mut self, bus: &mut dyn AddressBus) {
        let current_nmi_line = bus.is_nmi_line_asserted();
        if !self.prev_nmi_line && current_nmi_line {
            self.nmi_pending = true;
        }
        self.prev_nmi_line = current_nmi_line;

        if self.cycles_remaining == 0 {
            if !self.check_interrupts(bus) {
                // CYCLE 1: FETCH STAGE
                self.current_opcode = bus.read_byte(self.pc);
//                if self.total_cycles >= 62100 && self.total_cycles <= 63246 {
//                if self.test_print {
//                    emu_print!("Current opcode: {:02x} PC={:04x}|A={:02x}|SP={:04x}|", self.current_opcode, self.pc, self.a, self.sp);
//                }
                self.pc = self.pc.wrapping_add(1);

                let (op, mode, cycles) = self.decode_opcode(self.current_opcode);
                self.current_mode = mode;
                self.current_op = op;
                self.cycles_remaining = cycles;
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
            0x71 => (Operation::Adc, AddressingMode::AbsoluteY, 5),
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
            _=> { emu_print!("Opcode unimplemented: {:02X}", opcode);
                todo!() },
        }
    }

    fn check_interrupts(&mut self, bus: &mut dyn AddressBus) -> bool {
        if self.nmi_pending {
            self.nmi_pending = false; // Clear edge trigger flag
            emu_print!("Setup NMI Interrupt");
            self.setup_hardware_interrupt(Operation::Nmi, bus);
            return true;
        }
    
        if !self.status.interrupt_disable && bus.is_irq_line_asserted() {
            emu_print!("Setup IRQ Interrupt");
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
        // Hardware interrupts do NOT increment PC here!
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
                        let offset_addr = self.pc;
                        let offset = bus.read_byte(self.pc) as i8;
                        self.pc = self.pc.wrapping_add(1);

                        if condition_met {
                            let target_pc = (self.pc as i16).wrapping_add(offset as i16) as u16;
                            self.effective_addr = target_pc;
                            
                            self.cycles_remaining += 1;
                        }
                    }
                    3 => {  // Branch occurs on same page
                        let base_page = (self.pc.wrapping_sub(1)) & 0xFF00;
                        let target_page = self.effective_addr & 0xFF00;

                        let page_crossed = base_page != target_page;
                        if !page_crossed {
                            let _dummy = bus.read_byte(self.effective_addr);
                            // instruction finishes here if branch occurs on same page.
                            self.pc = self.effective_addr;
                        } else {
                            let uncorrected_addr = (self.pc & 0xFF00) | (self.effective_addr & 0x00FF);
                            let _dummy = bus.read_byte(uncorrected_addr);
                            // inject page boundary penalty
                            self.cycles_remaining += 1;
                        }
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
                    4 => {
                        if self.current_op.is_write() {
                            self.execute_operation(self.current_op, 0, bus);
                        } else {
                            let value = bus.read_byte(self.effective_addr);
                            self.execute_operation(self.current_op, value, bus);
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
                        if self.current_op.is_write() {
                            // If it's a write operation (e.g., STA), write the register contents to memory
                            self.execute_operation(self.current_op, 0, bus);
                        } else {
                            // If it's a read operation (e.g., LDA, AND, ADC), fetch the byte and execute
                            let value = bus.read_byte(self.effective_addr);
                            self.execute_operation(self.current_op, value, bus);
                        }
                    }
                    _ => { }
                }
            }
            AddressingMode::IndirectY => {
                match self.instruction_step {
                    2 => {
                        // Cycle 2: Fetch the zero-page vector pointer address from the instruction stream
                        self.temp_addr_low = bus.read_byte(self.pc);
                        self.pc = self.pc.wrapping_add(1);
                    }
                    3 => {
                        // Cycle 3: Read target address low byte from the zero-page location
                        self.temp_value = bus.read_byte(self.temp_addr_low as u16);
                    }
                    4 => {
                        // Cycle 4: Read target address high byte from the next zero-page location.
                        // Hardware constraint: The vector address increment wraps strictly inside Page 0!
                        let ptr_high = self.temp_addr_low.wrapping_add(1) as u16;
                        self.temp_addr_high = bus.read_byte(ptr_high);

                        // Construct the base target address and add the Y offset to form effective address
                        let base_target = ((self.temp_addr_high as u16) << 8) | (self.temp_value as u16);
                        self.effective_addr = base_target.wrapping_add(self.y as u16);
                    }
                    5 => {
                        // Cycle 5: Check if a page boundary was crossed
                        let expected_high = self.temp_addr_high as u16;
                        let actual_high = self.effective_addr >> 8;
                        let uncorrected_addr = (expected_high << 8) | (self.effective_addr & 0x00FF);

                        if self.current_op.is_write() {
                            // Write operations (like STA) are decoded at a base of 6 cycles.
                            // We perform the uncorrected read and naturally let it advance into step 6.
                            let _garbage = bus.read_byte(uncorrected_addr);
                        } else {
                            // Read operations (LDA, AND, etc.) are decoded at a base of 5 cycles.
                            if expected_high == actual_high {
                                // No page cross: Execute the operation early and terminate the instruction cleanly
                                let value = bus.read_byte(self.effective_addr);
                                self.execute_operation(self.current_op, value, bus);
                            } else {
                                // Page crossed! Perform dummy uncorrected read and inject the penalty cycle to hit step 6
                                let _garbage = bus.read_byte(uncorrected_addr);
                                self.cycles_remaining += 1;
                            }
                        }
                    }
                    6 => {
                        // Cycle 6: Terminal execution phase for page-crossed reads or write operations
                        if self.current_op.is_write() {
                            self.execute_operation(self.current_op, 0, bus);
                        } else {
                            let value = bus.read_byte(self.effective_addr);
                            self.execute_operation(self.current_op, value, bus);
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
                    (Operation::Plp, 3) => { let _dummy = bus.read_byte(STACK_BASE + self.sp as u16); self.sp = self.sp.wrapping_add(1); }
                    (Operation::Plp, 4) => { let val = bus.read_byte(STACK_BASE + self.sp as u16); self.status.from_u8(val); }
                    // RTI
                    (Operation::Rti, 2) => { let _dummy = bus.read_byte(self.pc); }
                    (Operation::Rti, 3) => { let _dummy = bus.read_byte(0x0100 + self.sp as u16); self.sp = self.sp.wrapping_add(1); }
                    (Operation::Rti, 4) => { self.status.from_u8(bus.read_byte(0x0100 + self.sp as u16)); self.sp = self.sp.wrapping_add(1); }
                    (Operation::Rti, 5) => { self.temp_addr_low = bus.read_byte(0x0100 + self.sp as u16); self.sp = self.sp.wrapping_add(1); }
                    (Operation::Rti, 6) => { self.temp_addr_high = bus.read_byte(STACK_BASE + self.sp as u16);
                                             self.pc = ((self.temp_addr_high as u16) << 8) | (self.temp_addr_low as u16); }
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
                            emu_print!("Interrupt cycle 2 at PC={:04X}. Cycle={}, bus cycles={}", self.pc, self.total_cycles, bus.total_cycles());                        
                        if self.current_op == Operation::Brk {
                            // Software BRK is a 2-byte instruction frame, so it advances PC here.
                            // Hardware interrupts do NOT advance PC.
                            emu_print!("Operation is BRK at PC={:04X}. Cycle={}, bus cycles={}", self.pc, self.total_cycles, bus.total_cycles());
                            self.pc = self.pc.wrapping_add(1);
                        }
                    }
                    3 => {
                        // Cycle 3: Push PC High Byte to Stack
                        let pc_high = (self.pc >> 8) as u8;
                        bus.write_byte(STACK_BASE + (self.sp as u16), pc_high);
                        self.sp = self.sp.wrapping_sub(1);
                            emu_print!("Interrupt cycle 3 at PC={:04X}. Cycle={}, bus cycles={}", self.pc, self.total_cycles, bus.total_cycles()); 
                    }
                    4 => {
                        // Cycle 4: Push PC Low Byte to Stack
                        let pc_low = (self.pc & 0x00FF) as u8;
                        bus.write_byte(STACK_BASE + (self.sp as u16), pc_low);
                        self.sp = self.sp.wrapping_sub(1);
                        emu_print!("Interrupt cycle 4 at PC={:04X}. Cycle={}, bus cycles={}", self.pc, self.total_cycles, bus.total_cycles()); 
                    }
                    5 => {
                        // Cycle 5: Push Status Flags to Stack
                        let is_instruction = self.current_op == Operation::Brk;
                        let status_byte = self.status.to_u8(is_instruction);
            
                        bus.write_byte(STACK_BASE + (self.sp as u16), status_byte);
                        self.sp = self.sp.wrapping_sub(1);
                        emu_print!("Interrupt cycle 5 at PC={:04X}. Cycle={}, bus cycles={}", self.pc, self.total_cycles, bus.total_cycles()); 
                    }
                    6 => {
                        // Cycle 6: Fetch Vector Low Byte
                        self.temp_addr_low = bus.read_byte(vector_base_addr);
                        emu_print!("Interrupt cycle 6 at PC={:04X}. Cycle={}, bus cycles={}", self.pc, self.total_cycles, bus.total_cycles());
                    }
                    7 => {
                        // Cycle 7: Fetch Vector High Byte and perform the actual vector jump!
                        self.status.interrupt_disable = true;
                        let high_byte = bus.read_byte(vector_base_addr + 1);
                        self.pc = ((high_byte as u16) << 8) | (self.temp_addr_low as u16);
                        emu_print!("INTERRUPT cycle 7: Vector Addr: {:04X}|PC: {:04X}|Vector high byte: {:02X} | low byte: {:02X}", vector_base_addr, self.pc, high_byte, self.temp_addr_low);
//                        self.test_print = true;
                    }
                    _ => {}
                }
            }
        }
    }

    fn execute_operation(&mut self, op: Operation, value: u8, bus: &mut dyn AddressBus) {
        match op {
            Operation::Adc => self.add_with_carry_logic(value),
            Operation::And => { self.a &= value; self.update_nz_flags(self.a); }
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
                self.status.overflow = value & 0x40 != 0;
                self.status.negative = value & 0x80 != 0;
                self.status.zero = result == 0;
            }
            Operation::Clc => { self.status.carry = false; }
            Operation::Cld => { self.status.decimal = false; }
            Operation::Cli => { self.status.interrupt_disable = false; }
            Operation::Clv => { self.status.overflow = false; }
            Operation::Cmp => { 
                let result = self.a.wrapping_sub(value);
                self.update_nz_flags(result);
                self.status.carry = value <= self.a;
            }
            Operation::Cpx => { let result = self.x.wrapping_sub(value); self.update_nz_flags(result); self.status.carry = value <= self.x}
            Operation::Cpy => { let result = self.y.wrapping_sub(value); self.update_nz_flags(result); self.status.carry = value <= self.y}
            Operation::Dec => { let result = value.wrapping_sub(1); self.update_nz_flags(result); bus.write_byte(self.effective_addr, result); }
            Operation::Dex => { self.x = self.x.wrapping_sub(1); self.update_nz_flags(self.x); }
            Operation::Dey => { self.y = self.y.wrapping_sub(1); self.update_nz_flags(self.y); }
            Operation::Eor => { self.a ^= value; self.update_nz_flags(self.a); }
            Operation::Inc => { let result = value.wrapping_add(1); self.update_nz_flags(result); bus.write_byte(self.effective_addr, result); }
            Operation::Inx => { self.x = self.x.wrapping_add(1); self.update_nz_flags(self.x); }
            Operation::Iny => { self.y = self.y.wrapping_add(1); self.update_nz_flags(self.y); }
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
            Operation::Jmp => { self.pc = self.effective_addr; }
            Operation::Nop => { }
            Operation::Ora => { self.a |= value; self.update_nz_flags(self.a); }
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
            Operation::Sbc => { self.add_with_carry_logic(value ^ 0xFF); }
            Operation::Sec => { self.status.carry = true; }
            Operation::Sed => { self.status.decimal = true; }
            Operation::Sei => { self.status.interrupt_disable = true; }
            Operation::Sta => { bus.write_byte(self.effective_addr, self.a); }
            Operation::Stx => { bus.write_byte(self.effective_addr, self.x); }
            Operation::Sty => { bus.write_byte(self.effective_addr, self.y); }
            Operation::Tax => { self.x = self.a; self.update_nz_flags(self.x); }
            Operation::Tay => { self.y = self.a; self.update_nz_flags(self.y); }
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