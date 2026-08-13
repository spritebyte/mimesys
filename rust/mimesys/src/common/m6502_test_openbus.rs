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
    use crate::nes::nes_ppu::NesPPU;
    use crate::nes::mappers::{Mapper,Mirroring};
    use crate::nes::mapper0::Mapper0;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::cell::{UnsafeCell, Cell};
    use std::sync::{Arc};

    struct OpenBusMock {
        ram: [u8; 65536],
        open_bus: Cell<u8>,
    }

    impl OpenBusMock {
        fn new() -> Self {
            Self { ram: [0; 65536], open_bus: Cell::new(0) }
        }

        fn read_inner(&mut self, addr: u16) -> u8 {
            let value = match addr {
                0x0000..=0x1FFF => self.ram[(addr as usize) % 0x0800],
                // Write-only region: reads return pure open bus.
                0x4000..=0x401F => self.open_bus.get(),
                // Pretend $8000+ is ROM we've filled via a helper.
                _ => self.open_bus.get(), // unmapped -> open bus for this test
            };
            self.open_bus.set(value);
            value
        }  
    }

    impl AddressBus for OpenBusMock {
        fn read_byte(&mut self, addr: u16) -> u8 {
            self.read_inner(addr)
        }
        fn write_byte(&mut self, addr: u16, val: u8) {
            if addr < 0x2000 {
                self.ram[addr as usize] = val;
            }
            self.open_bus.set(val);
        }
        fn is_nmi_line_asserted(&mut self) -> bool { false }
        fn is_nmi_enabled(&mut self) -> bool { false }
        fn is_irq_line_asserted(&mut self) -> bool { false }
        fn begin_cpu_cycle(&mut self) { }
        fn step_cycles(&mut self, cycles: u64) { }
    }

    fn run(cpu: &mut M6502Cpu, bus: &mut OpenBusMock, cycles: u32) {
        for _ in 0..cycles {
            cpu.step_one_cycle(bus);
        }
    }

    #[test]
    fn write_only_read_returns_last_bus_value() {
        // LDA $4000 (absolute). The bus drives, in order:
        //   opcode AD, operand-low 00, operand-high 40, then the read of $4000.
        // The read of a write-only address should return open bus == 0x40,
        // the operand high byte that was last driven.
        let mut cpu = M6502Cpu::new(CpuVariant::Ricoh2A03);
        let mut bus = OpenBusMock::new();
 
        cpu.pc = 0x0000;
        bus.ram[0x0000] = 0xAD; // LDA absolute
        bus.ram[0x0001] = 0x00; // low
        bus.ram[0x0002] = 0x40; // high -> $4000
 
        run(&mut cpu, &mut bus, 4); // LDA abs = 4 cycles
        assert_eq!(cpu.a, 0x40,
            "LDA $4000 (write-only) should load open bus = operand high byte 0x40");
    }

    #[test]
    fn each_access_refreshes_latch() {
        // Two consecutive LDAs of write-only addresses. The second should see
        // the open bus left by the first instruction's last fetch, proving the
        // latch is continuously refreshed, not stale from boot.
        let mut cpu = M6502Cpu::new(CpuVariant::Ricoh2A03);
        let mut bus = OpenBusMock::new();
 
        cpu.pc = 0x0000;
        // LDA $4000
        bus.ram[0x0000] = 0xAD;
        bus.ram[0x0001] = 0x00;
        bus.ram[0x0002] = 0x40;
        // LDA $4012
        bus.ram[0x0003] = 0xAD;
        bus.ram[0x0004] = 0x12;
        bus.ram[0x0005] = 0x40;
 
        run(&mut cpu, &mut bus, 4);
        assert_eq!(cpu.a, 0x40, "first read returns operand high 0x40");
 
        run(&mut cpu, &mut bus, 4);
        // Before this read, last driven byte was operand high 0x40 again.
        assert_eq!(cpu.a, 0x40, "second read also returns its operand high 0x40");
    }

    #[test]
    fn write_drives_bus_then_read_sees_it() {
        // STA $0000 drives 0x?? ... actually STA writes A to RAM; then a read
        // of a write-only address should reflect the last value on the bus.
        // Sequence: LDA #$AB (imm), STA $10 (zp), LDA $4000 (abs, write-only).
        let mut cpu = M6502Cpu::new(CpuVariant::Ricoh2A03);
        let mut bus = OpenBusMock::new();
 
        cpu.pc = 0x0000;
        bus.ram[0x0000] = 0xA9; // LDA #imm
        bus.ram[0x0001] = 0xAB;
        bus.ram[0x0002] = 0x85; // STA zp
        bus.ram[0x0003] = 0x10;
        bus.ram[0x0004] = 0xAD; // LDA $4000
        bus.ram[0x0005] = 0x00;
        bus.ram[0x0006] = 0x40;
 
        run(&mut cpu, &mut bus, 2); // LDA #imm
        run(&mut cpu, &mut bus, 3); // STA zp
        run(&mut cpu, &mut bus, 4); // LDA $4000
 
        // Last byte driven before the $4000 read was the operand high 0x40.
        assert_eq!(cpu.a, 0x40,
            "read of write-only addr returns last-driven bus byte");
    }

    fn make_ppu() -> (NesPPU, Box<dyn Mapper>) {
        let flag = Arc::new(AtomicBool::new(false));
        let ppu = NesPPU::new(flag);
        let mapper = Box::new(Mapper0::new(1,1,vec![0; 8192],vec![0; 8192],Mirroring::Horizontal));  // minimal cart
            (ppu, mapper)
    }
 
    #[test]
    fn status_read_merges_open_bus_low_bits() {
        // $2002 drives bits 5-7 (vblank/sprite0/overflow); bits 0-4 float and
        // must come from open bus. With vblank set (0x80) and open bus 0xFF,
        // the result should be 0x80 in bit 7, real 0 in bits 5-6, and open bus
        // 0x1F in the low bits: 0x80 | 0x1F = 0x9F  (NOT 0xFF, NOT 0x80).
        let (mut ppu, mut mapper) = make_ppu();
        ppu.status = 0x80;              // vblank only
        ppu.scanline = 100;             // avoid the 241 suppression window
        let res = ppu.cpu_read_reg(mapper.as_mut(), 2, 0xFF);
        assert_eq!(res, 0x9F,
            "status bits 5-7 real, bits 0-4 from open bus 0x1F");
    }
 
    #[test]
    fn status_low_bits_do_not_leak_from_status_reg() {
        // The bug this guards: `self.status | (open_bus & 0x1F)`. If status has
        // a stale low bit set (e.g. 0x81), an OR would force that bit regardless
        // of open bus. With open bus 0x00, the low bits MUST be 0, not 0x01.
        let (mut ppu, mut mapper) = make_ppu();
        ppu.status = 0x81;              // vblank + a dirty low bit
        ppu.scanline = 100;
        let res = ppu.cpu_read_reg(mapper.as_mut(), 2, 0x00);
        assert_eq!(res, 0x80,
            "status must be masked to 0xE0 before merging open bus; low bit must not leak");
    }
 
    #[test]
    fn write_only_ppu_regs_return_open_bus() {
        // Registers 0,1,3,5,6 are write-only -> pure open bus.
        let (mut ppu, mut mapper) = make_ppu();
        ppu.scanline = 100;
        for reg in [0u16, 1, 3, 5, 6] {
            let res = ppu.cpu_read_reg(mapper.as_mut(), reg, 0xA5);
            assert_eq!(res, 0xA5, "write-only PPU reg {} returns open bus", reg);
        }
    }
}