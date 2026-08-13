#[cfg(test)]
mod tests {
    use super::*;
    use crate::gameboy::gb_cpu::{GameBoyCpu, GbVariant};
    use crate::gameboy::gb_dummy_bus::{Bus, GameBoyBus};

    const FLAG_Z:u8 = 0x80;
    const FLAG_N:u8 = 0x40;
    const FLAG_H:u8 = 0x20;
    const FLAG_C:u8 = 0x10;

    #[test]
    fn test_ld_reg_reg_and_immediate_execution() {
        let mut cpu = GameBoyCpu::new(GbVariant::Dmg);
        let mut bus = GameBoyBus::new();

        // --- CASE 1: Load Immediate (2 M-Cycles) ---
        // LD B, 0x42 (Opcode 0x06, Imm 0x42)
        cpu.pc = 0xC000;
        bus.ram[0xC000] = 0x06;
        bus.ram[0xC001] = 0x42;

        for _ in 0..2 {
            cpu.step_one_m_cycle(&mut bus);
        }

        assert_eq!(cpu.b, 0x42, "Register B should be loaded with immediate value 0x42");
        assert_eq!(cpu.pc, 0xC002, "PC should advance 2 bytes after immediate load");

        // --- CASE 2: Register to Register Transfer (1 M-Cycle) ---
        // LD A, B (Opcode 0x78)
        bus.ram[0xC002] = 0x78;

        for _ in 0..1 {
            cpu.step_one_m_cycle(&mut bus);
        }

        assert_eq!(cpu.a, 0x42, "Register A should receive value from Register B");
        assert_eq!(cpu.pc, 0xC003, "PC should advance 1 byte for register transfer");

        // --- CASE 3: Memory Write via (HL) (2 M-Cycles) ---
        // LD (HL), A (Opcode 0x77)
        cpu.set_hl(0xC500);
        bus.ram[0xC003] = 0x77;

        for _ in 0..2 {
            cpu.step_one_m_cycle(&mut bus);
        }

        assert_eq!(bus.read(0xC500), 0x42, "Memory at address HL (0xC500) should store 0x42");
        assert_eq!(cpu.pc, 0xC004, "PC should advance 1 byte after LD (HL), A");

        // --- CASE 4: Memory Read via (HL) (2 M-Cycles) ---
        // LD C, (HL) (Opcode 0x4E)
        bus.write(0xC500, 0x99);
        bus.ram[0xC004] = 0x4E;

        for _ in 0..2 {
            cpu.step_one_m_cycle(&mut bus);
        }

        assert_eq!(cpu.c, 0x99, "Register C should load value 0x99 from memory at (HL)");
        assert_eq!(cpu.pc, 0xC005, "PC should advance 1 byte after LD C, (HL)");
    }

    #[test]
    fn test_alu_add_and_adc_flags() {
        let mut cpu = GameBoyCpu::new(GbVariant::Dmg);
        let mut bus = GameBoyBus::new();

        // --- CASE 1: ADD Immediate causing Half-Carry and Carry (2 M-Cycles) ---
        // ADD A, 0x01 with A = 0xFF -> Result 0x00 (Z=1, N=0, H=1, C=1)
        cpu.pc = 0xC000;
        cpu.a = 0xFF;
        cpu.f = 0x00;
        bus.ram[0xC000] = 0xC6; // ADD A, n8
        bus.ram[0xC001] = 0x01;

        for _ in 0..2 {
            cpu.step_one_m_cycle(&mut bus);
        }

        assert_eq!(cpu.a, 0x00, "0xFF + 0x01 should wrap to 0x00");
        assert_ne!(cpu.f & FLAG_Z, 0, "Zero flag must be set");
        assert_eq!(cpu.f & FLAG_N, 0, "Subtraction flag must be clear");
        assert_ne!(cpu.f & FLAG_H, 0, "Half-carry flag must be set (0x0F + 0x01 > 0x0F)");
        assert_ne!(cpu.f & FLAG_C, 0, "Carry flag must be set (0xFF + 0x01 > 0xFF)");

        // --- CASE 2: ADC Immediate with Carry Input (2 M-Cycles) ---
        // ADC A, 0x05 with A = 0x10 and Carry=1 -> Result 0x16
        cpu.pc = 0xC002;
        cpu.a = 0x10;
        cpu.f = FLAG_C; // Pre-set Carry flag
        bus.ram[0xC002] = 0xCE; // ADC A, n8
        bus.ram[0xC003] = 0x05;

        for _ in 0..2 {
            cpu.step_one_m_cycle(&mut bus);
        }

        assert_eq!(cpu.a, 0x16, "0x10 + 0x05 + Carry(1) should equal 0x16");
        assert_eq!(cpu.f & FLAG_Z, 0, "Zero flag must be clear");
        assert_eq!(cpu.f & FLAG_C, 0, "Carry flag should be clear for non-overflowing add");
    }

    #[test]
    fn test_alu_sub_and_cp_flags() {
        let mut cpu = GameBoyCpu::new(GbVariant::Dmg);
        let mut bus = GameBoyBus::new();

        // --- CASE 1: SUB Immediate with Borrow (2 M-Cycles) ---
        // SUB A, 0x05 with A = 0x00 -> Result 0xFB (Z=0, N=1, H=1, C=1)
        cpu.pc = 0xC000;
        cpu.a = 0x00;
        cpu.f = 0x00;
        bus.ram[0xC000] = 0xD6; // SUB A, n8
        bus.ram[0xC001] = 0x05;

        for _ in 0..2 {
            cpu.step_one_m_cycle(&mut bus);
        }

        assert_eq!(cpu.instruction_step, 0, "case 1 should end mid-nothing");
        assert_eq!(cpu.a, 0xFB, "0x00 - 0x05 wrapping sub should equal 0xFB");
        assert_ne!(cpu.f & FLAG_N, 0, "Subtraction flag N must be set for SUB");
        assert_ne!(cpu.f & FLAG_C, 0, "Carry flag C must be set on borrow (0x00 < 0x05)");
        assert_ne!(cpu.f & FLAG_H, 0, "Half-Carry H must be set on nibble borrow");

        // --- CASE 2: CP Immediate (Compare - Registers preserve state) (2 M-Cycles) ---
        // CP 0x42 with A = 0x42 -> Z=1, N=1, H=0, C=0. Register A remains 0x42.
        cpu.pc = 0xC002;
        cpu.a = 0x42;
        cpu.f = 0x00;
        bus.ram[0xC002] = 0xFE; // CP n8
        bus.ram[0xC003] = 0x42;

        for _ in 0..2 {
            cpu.step_one_m_cycle(&mut bus);
        }
        assert_eq!(cpu.instruction_step, 0, "case 2 should end mid-nothing");
        assert_eq!(cpu.a, 0x42, "Compare operation must not alter Accumulator contents");
        assert_ne!(cpu.f & FLAG_Z, 0, "Zero flag Z must be set when comparing equal values");
        assert_ne!(cpu.f & FLAG_N, 0, "Subtraction flag N must be set for CP");
        assert_eq!(cpu.f & FLAG_C, 0, "Carry flag C must be clear");
    }

    #[test]
    fn test_halt_bug_trigger_and_byte_duplication() {
        let mut cpu = GameBoyCpu::new(GbVariant::Dmg);
        let mut bus = GameBoyBus::new();

        // Setup HALT Bug Trigger Condition: IME = 0, Pending IRQ > 0
        cpu.pc = 0xC000;
        cpu.ime = false;
        bus.ie = 0x01;
        bus.iflags = 0x01;

        // Sequence: HALT (0x76), followed by LD B, 0x10 (0x06, 0x10)
        bus.ram[0xC000] = 0x76; // HALT
        bus.ram[0xC001] = 0x06; // LD B, n8 opcode
        bus.ram[0xC002] = 0x10; // Immediate operand

        // 1. Execute HALT instruction (1 M-Cycle)
        cpu.step_one_m_cycle(&mut bus);

        assert!(cpu.halt_bug, "HALT bug must trigger when IME=0 and IRQ is pending");
        assert!(!cpu.halted, "CPU must not enter halt mode when HALT bug triggers");
        assert_eq!(cpu.pc, 0xC001, "PC should advance to next byte after fetching HALT");

        // 2. Fetch cycle for instruction after HALT (1 M-Cycle)
        // Due to halt_bug=true, PC is read at 0xC001, but NOT incremented.
        cpu.step_one_m_cycle(&mut bus);

        assert_eq!(cpu.current_opcode, 0x06, "CPU fetched opcode 0x06");
        assert_eq!(cpu.pc, 0xC001, "PC must NOT advance during first byte fetch when halt_bug is active");
        assert!(!cpu.halt_bug, "halt_bug state must clear after being consumed");

        // 3. Second step of LD B, n8 (1 M-Cycle)
        // Reads operand at 0xC001 (which is opcode 0x06, NOT 0x10, due to duplicate read!)
        cpu.step_one_m_cycle(&mut bus);

        assert_eq!(
            cpu.b, 0x06,
            "Register B received duplicated opcode byte 0x06 instead of 0x10 due to HALT bug"
        );
        assert_eq!(cpu.pc, 0xC002, "PC advances past byte 0xC001 after reading operand");
    }
    #[test]
    fn test_inc_dec_8bit_registers_and_flags() {
        let mut cpu = GameBoyCpu::new(GbVariant::Dmg);
        let mut bus = GameBoyBus::new();

        // --- CASE 1: INC B causing Half-Carry (1 M-Cycle) ---
        // INC B (0x04) with B = 0x0F, pre-set Carry flag
        // Result: B = 0x10, Z = 0, N = 0, H = 1, C = PRESERVED (1)
        cpu.pc = 0xC000;
        cpu.b = 0x0F;
        cpu.f = FLAG_C; // Pre-set Carry to prove INC does NOT affect C
        bus.ram[0xC000] = 0x04; // INC B

        for _ in 0..1 {
            cpu.step_one_m_cycle(&mut bus);
        }

        assert_eq!(cpu.b, 0x10, "0x0F + 1 should equal 0x10");
        assert_eq!(cpu.f & FLAG_Z, 0, "Zero flag must be clear");
        assert_eq!(cpu.f & FLAG_N, 0, "Subtraction flag N must be clear for INC");
        assert_ne!(cpu.f & FLAG_H, 0, "Half-Carry H must be set (0x0F + 1 > 0x0F)");
        assert_ne!(cpu.f & FLAG_C, 0, "Carry flag C must remain UNCHANGED by 8-bit INC");

        // --- CASE 2: INC B wrapping to Zero (1 M-Cycle) ---
        // INC B (0x04) with B = 0xFF
        // Result: B = 0x00, Z = 1, N = 0, H = 1, C = PRESERVED
        cpu.pc = 0xC001;
        cpu.b = 0xFF;
        cpu.f = 0x00;
        bus.ram[0xC001] = 0x04; // INC B

        for _ in 0..1 {
            cpu.step_one_m_cycle(&mut bus);
        }

        assert_eq!(cpu.b, 0x00, "0xFF + 1 should wrap to 0x00");
        assert_ne!(cpu.f & FLAG_Z, 0, "Zero flag Z must be set");
        assert_eq!(cpu.f & FLAG_N, 0, "Subtraction flag N must be clear for INC");
        assert_ne!(cpu.f & FLAG_H, 0, "Half-Carry H must be set on 0x0F overflow");

        // --- CASE 3: DEC B causing Half-Borrow (1 M-Cycle) ---
        // DEC B (0x05) with B = 0x10
        // Result: B = 0x0F, Z = 0, N = 1, H = 1, C = PRESERVED
        cpu.pc = 0xC002;
        cpu.b = 0x10;
        cpu.f = 0x00;
        bus.ram[0xC002] = 0x05; // DEC B

        for _ in 0..1 {
            cpu.step_one_m_cycle(&mut bus);
        }

        assert_eq!(cpu.b, 0x0F, "0x10 - 1 should equal 0x0F");
        assert_eq!(cpu.f & FLAG_Z, 0, "Zero flag Z must be clear");
        assert_ne!(cpu.f & FLAG_N, 0, "Subtraction flag N must be set for DEC");
        assert_ne!(cpu.f & FLAG_H, 0, "Half-Carry H must be set on nibble borrow (0x00 -> 0x0F)");

        // --- CASE 4: DEC B to Zero (1 M-Cycle) ---
        // DEC B (0x05) with B = 0x01
        // Result: B = 0x00, Z = 1, N = 1, H = 0, C = PRESERVED
        cpu.pc = 0xC003;
        cpu.b = 0x01;
        cpu.f = FLAG_C; // Pre-set Carry
        bus.ram[0xC003] = 0x05; // DEC B

        for _ in 0..1 {
            cpu.step_one_m_cycle(&mut bus);
        }

        assert_eq!(cpu.b, 0x00, "0x01 - 1 should equal 0x00");
        assert_ne!(cpu.f & FLAG_Z, 0, "Zero flag Z must be set");
        assert_ne!(cpu.f & FLAG_N, 0, "Subtraction flag N must be set for DEC");
        assert_eq!(cpu.f & FLAG_H, 0, "Half-Carry H must be clear when no nibble borrow occurs");
        assert_ne!(cpu.f & FLAG_C, 0, "Carry flag C must remain UNCHANGED by 8-bit DEC");
    }

    #[test]
    fn test_inc_dec_memory_hl() {
        let mut cpu = GameBoyCpu::new(GbVariant::Dmg);
        let mut bus = GameBoyBus::new();

        // --- CASE 1: INC (HL) (3 M-Cycles: Fetch, Read, Write) ---
        // Opcode 0x34: INC (HL)
        cpu.pc = 0xC000;
        cpu.set_hl(0xC500);
        bus.write(0xC500, 0x0F);
        bus.ram[0xC000] = 0x34;

        for _ in 0..3 {
            cpu.step_one_m_cycle(&mut bus);
        }

        assert_eq!(bus.read(0xC500), 0x10, "Value at (HL) should increment from 0x0F to 0x10");
        assert_ne!(cpu.f & FLAG_H, 0, "Half-Carry H must be set");
        assert_eq!(cpu.pc, 0xC001, "PC should advance 1 byte after INC (HL)");

        // --- CASE 2: DEC (HL) (3 M-Cycles: Fetch, Read, Write) ---
        // Opcode 0x35: DEC (HL)
        cpu.pc = 0xC001;
        cpu.set_hl(0xC500);
        bus.write(0xC500, 0x00);
        bus.ram[0xC001] = 0x35;

        for _ in 0..3 {
            cpu.step_one_m_cycle(&mut bus);
        }

        assert_eq!(bus.read(0xC500), 0xFF, "Value at (HL) should decrement from 0x00 to 0xFF");
        assert_ne!(cpu.f & FLAG_N, 0, "Subtraction flag N must be set");
        assert_ne!(cpu.f & FLAG_H, 0, "Half-Carry H must be set on borrow");
        assert_eq!(cpu.pc, 0xC002, "PC should advance 1 byte after DEC (HL)");
    }

    #[test]
    fn test_inc_dec_16bit_registers_preserve_all_flags() {
        let mut cpu = GameBoyCpu::new(GbVariant::Dmg);
        let mut bus = GameBoyBus::new();

        // Preset flags to a distinct pattern (Z=1, N=0, H=1, C=0)
        let initial_flags = FLAG_Z | FLAG_H;

        // --- CASE 1: INC BC Across Byte Boundary (2 M-Cycles) ---
        // Opcode 0x03: INC BC
        cpu.pc = 0xC000;
        cpu.set_bc(0x00FF);
        cpu.f = initial_flags;
        bus.ram[0xC000] = 0x03;

        for _ in 0..2 {
            cpu.step_one_m_cycle(&mut bus);
        }

        assert_eq!(cpu.bc(), 0x0100, "BC should increment from 0x00FF to 0x0100");
        assert_eq!(
            cpu.f, initial_flags,
            "16-bit INC must NOT alter any flags! (Flags remained {:02X})",
            cpu.f
        );

        // --- CASE 2: DEC HL Underflow (2 M-Cycles) ---
        // Opcode 0x2B: DEC HL
        cpu.pc = 0xC001;
        cpu.set_hl(0x0000);
        cpu.f = initial_flags;
        bus.ram[0xC001] = 0x2B;

        for _ in 0..2 {
            cpu.step_one_m_cycle(&mut bus);
        }

        assert_eq!(cpu.hl(), 0xFFFF, "HL should decrement from 0x0000 to 0xFFFF");
        assert_eq!(
            cpu.f, initial_flags,
            "16-bit DEC must NOT alter any flags! (Flags remained {:02X})",
            cpu.f
        );
    }
}