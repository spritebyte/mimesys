#[cfg(test)]
mod control_flow_and_stack_tests {
    use super::*;
    use crate::gameboy::gb_cpu::{GameBoyCpu, GbVariant};
    use crate::gameboy::gb_bus::Bus;
    use crate::gameboy::gb_mock_bus::MockBus;

    const FLAG_Z: u8 = 0x80;
    const FLAG_N: u8 = 0x40;
    const FLAG_H: u8 = 0x20;
    const FLAG_C: u8 = 0x10;

    #[test]
    fn test_push_pop_and_pop_af_masking() {
        let mut cpu = GameBoyCpu::new(GbVariant::Dmg);
        let mut bus = MockBus::new();

        cpu.pc = 0xC000;
        cpu.sp = 0xFFFE;
        cpu.set_bc(0x1234);

        // --- CASE 1: PUSH BC (0xC5) -> 4 M-Cycles ---
        bus.ram[0xC000] = 0xC5;

        for _ in 0..4 {
            cpu.step_one_m_cycle(&mut bus);
        }

        assert_eq!(cpu.sp, 0xFFFC, "SP should decrement twice (0xFFFE -> 0xFFFC)");
        assert_eq!(bus.ram[0xFFFD], 0x12, "High byte B (0x12) pushed to SP-1");
        assert_eq!(bus.ram[0xFFFC], 0x34, "Low byte C (0x34) pushed to SP-2");

        // --- CASE 2: POP DE (0xD1) -> 3 M-Cycles ---
        bus.ram[0xC001] = 0xD1;

        for _ in 0..3 {
            cpu.step_one_m_cycle(&mut bus);
        }

        assert_eq!(cpu.de(), 0x1234, "DE should load popped value 0x1234");
        assert_eq!(cpu.sp, 0xFFFE, "SP restored to 0xFFFE");

        // --- CASE 3: POP AF Masking Rule (0xF1) -> 3 M-Cycles ---
        // Write 0x55FF to stack (A = 0x55, F = 0xFF)
        bus.ram[0xFFFC] = 0xFF; // F byte with lower nibble set
        bus.ram[0xFFFD] = 0x55; // A byte
        cpu.sp = 0xFFFC;
        bus.ram[0xC002] = 0xF1; // POP AF

        for _ in 0..3 {
            cpu.step_one_m_cycle(&mut bus);
        }

        assert_eq!(cpu.a, 0x55, "Register A loaded from stack");
        assert_eq!(
            cpu.f, 0xF0,
            "Register F lower 4 bits MUST be masked to 0 on POP AF (0xFF -> 0xF0)"
        );
        assert_eq!(cpu.sp, 0xFFFE, "SP restored to 0xFFFE");
    }

    #[test]
    fn test_jp_and_jr_conditional_timing() {
        let mut cpu = GameBoyCpu::new(GbVariant::Dmg);
        let mut bus = MockBus::new();

        // --- CASE 1: JP a16 Unconditional (0xC3) -> 4 M-Cycles ---
        cpu.pc = 0xC000;
        bus.ram[0xC000] = 0xC3;
        bus.ram[0xC001] = 0x00;
        bus.ram[0xC002] = 0xC5;

        for _ in 0..4 {
            cpu.step_one_m_cycle(&mut bus);
        }

        assert_eq!(cpu.pc, 0xC500, "Unconditional JP should land at 0xC500");

        // --- CASE 2: JP Z, a16 NOT Taken (0xCA) -> 3 M-Cycles ---
        cpu.pc = 0xC000;
        cpu.f = 0x00; // Zero flag clear -> condition false
        bus.ram[0xC000] = 0xCA;
        bus.ram[0xC001] = 0x00;
        bus.ram[0xC002] = 0xC6;

        for _ in 0..3 {
            cpu.step_one_m_cycle(&mut bus);
        }

        assert_eq!(
            cpu.pc, 0xC003,
            "Untaken conditional JP exits early after fetching address (3 M-Cycles)"
        );

        // --- CASE 3: JP Z, a16 TAKEN (0xCA) -> 4 M-Cycles ---
        cpu.pc = 0xC000;
        cpu.f = FLAG_Z; // Zero flag set -> condition true

        for _ in 0..4 {
            cpu.step_one_m_cycle(&mut bus);
        }

        assert_eq!(cpu.pc, 0xC600, "Taken conditional JP jumps to target (4 M-Cycles)");

        // --- CASE 4: JR e8 Relative Jump (0x18) with Positive Offset -> 3 M-Cycles ---
        cpu.pc = 0xC000;
        bus.ram[0xC000] = 0x18;
        bus.ram[0xC001] = 0x05; // Offset +5 (relative to 0xC002)

        for _ in 0..3 {
            cpu.step_one_m_cycle(&mut bus);
        }

        assert_eq!(cpu.pc, 0xC007, "JR +5 should jump to 0xC002 + 5 = 0xC007");

        // --- CASE 5: JR NZ, e8 TAKEN (0x20) with Negative Offset -> 3 M-Cycles ---
        cpu.pc = 0xC008;
        cpu.f = 0x00; // Zero flag clear -> NZ is true
        bus.ram[0xC008] = 0x20;
        bus.ram[0xC009] = 0xFB; // -5 in two's complement (0xC00A - 5 = 0xC005)

        for _ in 0..3 {
            cpu.step_one_m_cycle(&mut bus);
        }

        assert_eq!(cpu.pc, 0xC005, "JR NZ -5 should jump backwards to 0xC005");

        // --- CASE 6: JR NZ, e8 NOT TAKEN (0x20) -> 2 M-Cycles ---
        cpu.pc = 0xC000;
        cpu.f = FLAG_Z; // Zero flag set -> NZ is false
        bus.ram[0xC000] = 0x20;
        bus.ram[0xC001] = 0x10;

        for _ in 0..2 {
            cpu.step_one_m_cycle(&mut bus);
        }

        assert_eq!(
            cpu.pc, 0xC002,
            "Untaken JR exits early after 2 M-Cycles without applying offset"
        );
    }

    #[test]
    fn test_call_and_ret_conditional_execution() {
        let mut cpu = GameBoyCpu::new(GbVariant::Dmg);
        let mut bus = MockBus::new();

        // --- CASE 1: CALL a16 Unconditional (0xCD) -> 6 M-Cycles ---
        cpu.pc = 0xC000;
        cpu.sp = 0xFFFE;
        bus.ram[0xC000] = 0xCD;
        bus.ram[0xC001] = 0x00;
        bus.ram[0xC002] = 0xC5;

        for _ in 0..6 {
            cpu.step_one_m_cycle(&mut bus);
        }

        assert_eq!(cpu.pc, 0xC500, "CALL jumps to target address 0xC500");
        assert_eq!(cpu.sp, 0xFFFC, "SP decrements by 2");
        assert_eq!(bus.ram[0xFFFD], 0xC0, "Return address MSB (0xC0) pushed to stack");
        assert_eq!(bus.ram[0xFFFC], 0x03, "Return address LSB (0x03) pushed to stack");

        // --- CASE 2: RET Unconditional (0xC9) -> 4 M-Cycles ---
        bus.ram[0xC500] = 0xC9; // RET opcode at subroutine target

        for _ in 0..4 {
            cpu.step_one_m_cycle(&mut bus);
        }

        assert_eq!(cpu.pc, 0xC003, "RET restores PC to 0xC003");
        assert_eq!(cpu.sp, 0xFFFE, "SP restored to 0xFFFE");

        // --- CASE 3: CALL C, a16 NOT TAKEN (0xDC) -> 3 M-Cycles ---
        cpu.pc = 0xC003;
        cpu.f = 0x00; // Carry flag clear -> condition false
        bus.ram[0xC003] = 0xDC;
        bus.ram[0xC004] = 0x00;
        bus.ram[0xC005] = 0xD0;

        for _ in 0..3 {
            cpu.step_one_m_cycle(&mut bus);
        }

        assert_eq!(
            cpu.pc, 0xC006,
            "Untaken CALL advances past instruction without branching (3 M-Cycles)"
        );
        assert_eq!(cpu.sp, 0xFFFE, "SP must not change when CALL is untaken");

        // --- CASE 4: RET C TAKEN (0xD8) -> 5 M-Cycles ---
        // Setup stack manually with return address 0x8000
        cpu.pc = 0xD000;
        cpu.f = FLAG_C; // Carry flag set -> condition true
        cpu.sp = 0xFFFC;
        bus.ram[0xFFFC] = 0x00; // LSB
        bus.ram[0xFFFD] = 0x80; // MSB
        bus.ram[0xD000] = 0xD8; // RET C

        for _ in 0..5 {
            cpu.step_one_m_cycle(&mut bus);
        }

        assert_eq!(cpu.pc, 0x8000, "Taken RET C restores PC to 0x8000 (5 M-Cycles)");
        assert_eq!(cpu.sp, 0xFFFE, "SP unwound after return");

        // --- CASE 5: RET C NOT TAKEN (0xD8) -> 2 M-Cycles ---
        cpu.pc = 0xD000;
        cpu.f = 0x00; // Carry flag clear -> condition false
        cpu.sp = 0xFFFC;
        bus.ram[0xD000] = 0xD8; // RET C

        for _ in 0..2 {
            cpu.step_one_m_cycle(&mut bus);
        }

        assert_eq!(cpu.pc, 0xD001, "Untaken RET C exits early after 2 M-Cycles");
        assert_eq!(cpu.sp, 0xFFFC, "SP remains unchanged");
    }
}