
#[cfg(test)]
mod tests {
    use super::*;

    /// A basic 64KB Bus implementation for testing instructions.
    struct TestBus {
        memory: [u8; 65536],
        ports_in: [u8; 256],
        ports_out: [u8; 256],
    }

    impl TestBus {
        fn new() -> Self {
            Self {
                memory: [0; 65536],
                ports_in: [0; 256],
                ports_out: [0; 256],
            }
        }

        /// Helper to write sequential byte sequences into memory.
        fn load_bytes(&mut self, addr: u16, bytes: &[u8]) {
            let start = addr as usize;
            self.memory[start..start + bytes.len()].copy_from_slice(bytes);
        }
    }

    impl Bus for TestBus {
        fn read(&mut self, addr: u16) -> u8 {
            self.memory[addr as usize]
        }

        fn write(&mut self, addr: u16, val: u8) {
            self.memory[addr as usize] = val;
        }

        fn port_in(&mut self, port: u16) -> u8 {
            self.ports_in[(port & 0xFF) as usize]
        }

        fn port_out(&mut self, port: u16, val: u8) {
            self.ports_out[(port & 0xFF) as usize] = val;
        }
    }

    #[test]
    fn test_nop() {
        let mut cpu = Z80Cpu::new();
        let mut bus = TestBus::new();
        bus.write(0x0000, 0x00); // NOP

        let cycles = cpu.step(&mut bus);

        assert_eq!(cycles, 4);
        assert_eq!(cpu.pc, 1);
    }

    #[test]
    fn test_ld_bc_nn_and_16bit_accessors() {
        let mut cpu = Z80Cpu::new();
        let mut bus = TestBus::new();
        bus.load_bytes(0x0000, &[0x01, 0x34, 0x12]); // LD BC, $1234

        let cycles = cpu.step(&mut bus);

        assert_eq!(cycles, 10);
        assert_eq!(cpu.bc(), 0x1234);
        assert_eq!(cpu.b, 0x12);
        assert_eq!(cpu.c, 0x34);
        assert_eq!(cpu.pc, 3);
    }

    #[test]
    fn test_ld_mem_bc_a() {
        let mut cpu = Z80Cpu::new();
        let mut bus = TestBus::new();
        cpu.a = 0xAB;
        cpu.set_bc(0x2000);
        bus.write(0x0000, 0x02); // LD (BC), A

        let cycles = cpu.step(&mut bus);

        assert_eq!(cycles, 7);
        assert_eq!(bus.read(0x2000), 0xAB);
    }

    #[test]
    fn test_ex_af_af_and_exx() {
        let mut cpu = Z80Cpu::new();
        let mut bus = TestBus::new();

        cpu.a = 0x11;
        cpu.f = 0x22;
        cpu.set_bc(0x1111);
        cpu.set_de(0x2222);
        cpu.set_hl(0x3333);

        // EX AF, AF' (0x08)
        bus.write(0x0000, 0x08);
        cpu.step(&mut bus);

        assert_eq!(cpu.a, 0x00);
        assert_eq!(cpu.f, 0x00);
        assert_eq!(cpu.a_, 0x11);
        assert_eq!(cpu.f_, 0x22);

        // EXX (0xD9)
        bus.write(0x0001, 0xD9);
        cpu.step(&mut bus);

        assert_eq!(cpu.bc(), 0x0000);
        assert_eq!(cpu.b_, 0x11);
        assert_eq!(cpu.c_, 0x11);
    }

    #[test]
    fn test_jr_relative_jumps() {
        let mut cpu = Z80Cpu::new();
        let mut bus = TestBus::new();

        // JR +5 (0x18 0x05) -> pc ends at 0x0002 + 5 = 0x0007
        bus.load_bytes(0x0000, &[0x18, 0x05]);
        let cycles = cpu.step(&mut bus);

        assert_eq!(cycles, 12);
        assert_eq!(cpu.pc, 0x0007);

        // JR Z, d when Z flag is NOT set
        cpu.f = 0;
        bus.load_bytes(0x0007, &[0x28, 0x10]);
        let cycles_untaken = cpu.step(&mut bus);

        assert_eq!(cycles_untaken, 7);
        assert_eq!(cpu.pc, 0x0009);

        // JR Z, d when Z flag IS set
        cpu.f = FLAG_Z;
        bus.load_bytes(0x0009, &[0x28, 0x05]);
        let cycles_taken = cpu.step(&mut bus);

        assert_eq!(cycles_taken, 12);
        assert_eq!(cpu.pc, 0x0010);
    }

    #[test]
    fn test_djnz_looping() {
        let mut cpu = Z80Cpu::new();
        let mut bus = TestBus::new();

        cpu.b = 2;
        // DJNZ -2 (offset -2 / 0xFE: jumps back to 0x0000)
        bus.load_bytes(0x0000, &[0x10, 0xFE]);

        // First decrement: B = 1, jump taken
        let cycles1 = cpu.step(&mut bus);
        assert_eq!(cycles1, 13);
        assert_eq!(cpu.b, 1);
        assert_eq!(cpu.pc, 0x0000);

        // Second decrement: B = 0, jump NOT taken
        let cycles2 = cpu.step(&mut bus);
        assert_eq!(cycles2, 8);
        assert_eq!(cpu.b, 0);
        assert_eq!(cpu.pc, 0x0002);
    }

    #[test]
    fn test_alu_add_and_half_carry_flag() {
        let mut cpu = Z80Cpu::new();
        let mut bus = TestBus::new();

        cpu.a = 0x0F;
        cpu.b = 0x01;
        // ADD A, B (0x80)
        bus.write(0x0000, 0x80);
        cpu.step(&mut bus);

        assert_eq!(cpu.a, 0x10);
        assert_ne!(cpu.f & FLAG_H, 0); // Half carry should trigger on 0x0F + 1
        assert_eq!(cpu.f & FLAG_Z, 0);
        assert_eq!(cpu.f & FLAG_C, 0);
    }
#[test]
    fn test_inc8_and_dec8_flags() {
        let mut cpu = Z80Cpu::new();
        let mut bus = TestBus::new();

        // INC B (0x04) with B = 0xFF -> overflow to 0x00, Z and H set
        cpu.b = 0xFF;
        bus.write(0x0000, 0x04);
        cpu.step(&mut bus);

        assert_eq!(cpu.b, 0x00);
        assert_ne!(cpu.f & FLAG_Z, 0);
        assert_ne!(cpu.f & FLAG_H, 0);

        // DEC B (0x05) with B = 0x00 -> underflow to 0xFF, N set, Z clear
        bus.write(0x0001, 0x05);
        cpu.step(&mut bus);

        assert_eq!(cpu.b, 0xFF);
        assert_eq!(cpu.f & FLAG_Z, 0);
        assert_ne!(cpu.f & FLAG_N, 0);
    }

    #[test]
    fn test_sub_and_cp8_flags() {
        let mut cpu = Z80Cpu::new();
        let mut bus = TestBus::new();

        cpu.a = 0x20;
        // SUB 0x20 via CP (opcode 0xBF: CP A) -> Z set, N set, A unchanged
        bus.write(0x0000, 0xBF);
        cpu.step(&mut bus);

        assert_eq!(cpu.a, 0x20); // CP does not modify A
        assert_ne!(cpu.f & FLAG_Z, 0);
        assert_ne!(cpu.f & FLAG_N, 0);

        // SUB B (opcode 0x90) where B = 0x25 -> borrow / Carry set
        cpu.b = 0x25;
        bus.write(0x0001, 0x90);
        cpu.step(&mut bus);

        assert_eq!(cpu.a, 0xFB); // 0x20 - 0x25 = -5 = 0xFB
        assert_ne!(cpu.f & FLAG_C, 0);
        assert_ne!(cpu.f & FLAG_S, 0); // Sign flag set for negative result
    }

    #[test]
    fn test_cb_prefix_bit_and_shift() {
        let mut cpu = Z80Cpu::new();
        let mut bus = TestBus::new();

        cpu.b = 0b0000_1000;
        // CB 40 -> BIT 0, B
        bus.load_bytes(0x0000, &[0xCB, 0x40]);
        cpu.step(&mut bus);

        assert_ne!(cpu.f & FLAG_Z, 0); // Bit 0 is 0, so Z set
        assert_ne!(cpu.f & FLAG_H, 0); // BIT instruction always sets Half-Carry

        // CB 58 -> BIT 3, B
        bus.load_bytes(0x0002, &[0xCB, 0x58]);
        cpu.step(&mut bus);

        assert_eq!(cpu.f & FLAG_Z, 0); // Bit 3 is 1, so Z clear

        // CB 20 -> SLA B (Shift Left Arithmetic B)
        bus.load_bytes(0x0004, &[0xCB, 0x20]);
        cpu.step(&mut bus);

        assert_eq!(cpu.b, 0b0001_0000);
    }

    #[test]
    fn test_dd_prefix_ix_indexed_access() {
        let mut cpu = Z80Cpu::new();
        let mut bus = TestBus::new();

        cpu.ix = 0x1000;
        bus.write(0x1005, 0x77);

        // DD 7E 05 -> LD A, (IX+5)
        bus.load_bytes(0x0000, &[0xDD, 0x7E, 0x05]);
        let cycles = cpu.step(&mut bus);

        assert_eq!(cycles, 19); // Indexed read takes extra cycles
        assert_eq!(cpu.a, 0x77);

        // DD 09 -> ADD IX, BC
        cpu.set_bc(0x0010);
        bus.load_bytes(0x0003, &[0xDD, 0x09]);
        cpu.step(&mut bus);

        assert_eq!(cpu.ix, 0x1010);
    }

    #[test]
    fn test_stack_push_pop() {
        let mut cpu = Z80Cpu::new();
        let mut bus = TestBus::new();

        cpu.sp = 0x2000;
        cpu.push16(&mut bus, 0xABCD);

        assert_eq!(cpu.sp, 0x1FFE);
        assert_eq!(bus.read(0x1FFE), 0xCD); // Low byte pushed last
        assert_eq!(bus.read(0x1FFF), 0xAB); // High byte pushed first

        let popped = cpu.pop16(&mut bus);
        assert_eq!(popped, 0xABCD);
        assert_eq!(cpu.sp, 0x2000);
    }

    #[test]
    fn test_ed_prefix_retn_reti() {
        let mut cpu = Z80Cpu::new();
        let mut bus = TestBus::new();

        cpu.sp = 0x1FFE;
        bus.write(0x1FFE, 0x34);
        bus.write(0x1FFF, 0x12);

        // ED 45 -> RETN
        bus.load_bytes(0x0000, &[0xED, 0x45]);
        let cycles = cpu.step(&mut bus);

        assert_eq!(cycles, 14);
        assert_eq!(cpu.pc, 0x1234);
        assert_eq!(cpu.sp, 0x2000);
    }

    #[test]
    fn test_logic_and_parity_flag() {
        let mut cpu = Z80Cpu::new();
        let mut bus = TestBus::new();

        cpu.a = 0b1010_1000; // 3 bits set (odd parity)
        cpu.b = 0b1111_1111;

        // AND B (opcode 0xA0) -> result 0b1010_1000, PV flag clear (odd parity)
        bus.write(0x0000, 0xA0);
        cpu.step(&mut bus);

        assert_eq!(cpu.a, 0b1010_1000);
        assert_eq!(cpu.f & FLAG_PV, 0);

        // XOR A (opcode 0xAF) -> result 0x00 (0 bits set, even parity), Z set
        bus.write(0x0001, 0xAF);
        cpu.step(&mut bus);

        assert_eq!(cpu.a, 0x00);
        assert_ne!(cpu.f & FLAG_Z, 0);
        assert_ne!(cpu.f & FLAG_PV, 0); // 0 set bits is even parity, PV should be set
    }

    fn run(cpu: &mut Z80Cpu, bus: &mut TestBus, at: u16, bytes: &[u8]) -> u32 {
    cpu.pc = at;
    bus.load_bytes(at, bytes);
    cpu.step(bus)
}

#[test]
fn jr_backward() {
    let (mut cpu, mut bus) = (Z80Cpu::new(), TestBus::new());
    let cycles = run(&mut cpu, &mut bus, 0x0010, &[0x18, 0xFE]); // JR -2
    assert_eq!(cpu.pc, 0x0010);
    assert_eq!(cycles, 12);
}

#[test]
fn add_overflow_full_flags() {
    let (mut cpu, mut bus) = (Z80Cpu::new(), TestBus::new());
    cpu.a = 0x7F; cpu.b = 0x01;
    run(&mut cpu, &mut bus, 0, &[0x80]);              // ADD A,B
    assert_eq!(cpu.a, 0x80);
    assert_eq!(cpu.f, FLAG_S | FLAG_H | FLAG_PV);     // 0x94, N clear
}

#[test]
fn logic_ops_clear_stale_flags() {
    let (mut cpu, mut bus) = (Z80Cpu::new(), TestBus::new());
    cpu.a = 0xFF; cpu.f = 0xFF;                       // all flags stale
    run(&mut cpu, &mut bus, 0, &[0xAF]);              // XOR A
    assert_eq!(cpu.f, FLAG_Z | FLAG_PV);              // 0x44: no H, no X/Y

    cpu.a = 0xFF; cpu.b = 0x0F; cpu.f = 0xFF;
    run(&mut cpu, &mut bus, 0, &[0xA0]);              // AND B -> 0x0F
    assert_eq!(cpu.f, FLAG_H | FLAG_X | FLAG_PV);     // 0x1C
}

#[test]
fn ld_r_hl_timing() {
    let (mut cpu, mut bus) = (Z80Cpu::new(), TestBus::new());
    cpu.set_hl(0x2000); bus.write(0x2000, 0x42);
    let cycles = run(&mut cpu, &mut bus, 0, &[0x46]); // LD B,(HL)
    assert_eq!(cpu.b, 0x42);
    assert_eq!(cycles, 7);
}

#[test]
fn r_register_counts_opcode_fetches() {
    let (mut cpu, mut bus) = (Z80Cpu::new(), TestBus::new());
    run(&mut cpu, &mut bus, 0, &[0x00]);                    // NOP
    assert_eq!(cpu.r, 1);
    run(&mut cpu, &mut bus, 0, &[0xDD, 0x21, 0x34, 0x12]);  // LD IX,$1234
    assert_eq!(cpu.r, 3);                                   // +2, not +3
    cpu.r = 0xFF;
    run(&mut cpu, &mut bus, 0, &[0x00]);
    assert_eq!(cpu.r, 0x80);                                // bit 7 never changes
}

#[test]
fn ddcb_rlc_copies_to_register() {
    let (mut cpu, mut bus) = (Z80Cpu::new(), TestBus::new());
    cpu.ix = 0x1000; bus.write(0x1005, 0x81);
    let cycles = run(&mut cpu, &mut bus, 0, &[0xDD, 0xCB, 0x05, 0x00]); // RLC (IX+5),B
    assert_eq!(bus.read(0x1005), 0x03);
    assert_eq!(cpu.b, 0x03);                         // undocumented copy
    assert_eq!(cpu.f, FLAG_PV | FLAG_C);             // 0x05
    assert_eq!(cycles, 23);
    assert_eq!(cpu.pc, 4);
}

    #[test]
    fn retn_restores_iff1() {
        let (mut cpu, mut bus) = (Z80Cpu::new(), TestBus::new());
        cpu.sp = 0x1FFE; cpu.iff1 = false; cpu.iff2 = true;
        run(&mut cpu, &mut bus, 0, &[0xED, 0x45]);
        assert!(cpu.iff1);
    }
}