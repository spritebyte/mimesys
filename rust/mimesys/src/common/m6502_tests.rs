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
    use crate::common::m6502::{M6502Cpu,Operation,CpuVariant};
    use crate::common::bus::AddressBus;

    struct MockBus {
        ram: [u8; 65536],
    }

    impl MockBus {
        fn new() -> Self {
            Self { ram: [0; 65536] }
        }
    }

    impl AddressBus for MockBus {
        fn read_byte(&mut self, addr: u16) -> u8 {
            self.ram[addr as usize]
        }
        fn write_byte(&mut self, addr: u16, val: u8) {
            self.ram[addr as usize] = val;
        }
        fn is_nmi_line_asserted(&mut self) -> bool { false }
        fn is_nmi_enabled(&mut self) -> bool { false }
        fn is_irq_line_asserted(&mut self) -> bool { false }
        fn begin_cpu_cycle(&mut self) { }
        fn step_cycles(&mut self, cycles: u64) { }
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
    assert_eq!(cpu.cycles_remaining, 0, "case 1 should end on an instruction boundary");
    // --- CASE 2: Branch Taken, Same Page (3 Cycles) ---
    // BNE (0xD0) with an offset of +4. Zero flag is CLEAR, so it should branch.
    cpu.branch_taken = false;
    cpu.branch_page_crossed = false;
    cpu.pc = 0xC000;
    cpu.status.zero = false; 
    
    // We expect 3 cycles total for a taken branch on the same page
    for _ in 0..3 { cpu.step_one_cycle(&mut bus); }
    assert_eq!(cpu.pc, 0xC006, "Branch taken should advance PC by offset (0xC002 + 4 = 0xC006)");
    assert_eq!(cpu.cycles_remaining, 0, "case 2 should end on an instruction boundary");
    // --- CASE 3: Branch Taken, Crosses Page Boundary (4 Cycles) ---
    // Place the BNE right at the end of page 0xC0 (e.g., 0xC0FE). 
    // An offset of +4 will push the execution target into page 0xC1 (0xC104).
    cpu.reset(&mut bus);
    cpu.branch_taken = false;
    cpu.branch_page_crossed = false;
    cpu.pc = 0xC0FD;
    cpu.status.zero = false;
    bus.ram[0xC0FD] = 0xD0; // BNE
    bus.ram[0xC0FE] = 0x04; // 0xC0FF + 4 = 0xC103 (crosses page)

    // We expect 4 cycles total due to the page cross penalty
    for _ in 0..4 { cpu.step_one_cycle(&mut bus); }
    assert_eq!(cpu.pc, 0xC103, "Branch crossing page boundary should land at 0xC104");
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