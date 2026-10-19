// Test helper: run a 6502 program on a flat 64K bus (used for the decimal-mode tests).
use crate::common::cpu::m6502::{M6502Cpu, CpuVariant, AddressBus};

pub struct FlatBus { pub mem: Vec<u8>, cycles: u64 }
impl FlatBus { pub fn new() -> Self { Self { mem: vec![0; 0x10000], cycles: 0 } } }
impl AddressBus for FlatBus {
    fn is_nmi_line_asserted(&mut self) -> bool { false }
    fn is_nmi_enabled(&mut self) -> bool { false }
    fn total_cycles(&self) -> u64 { self.cycles }
    fn is_irq_line_asserted(&mut self) -> bool { false }
    fn read_byte(&mut self, a: u16) -> u8 { self.mem[a as usize] }
    fn write_byte(&mut self, a: u16, v: u8) { self.mem[a as usize] = v; }
    fn step_cycles(&mut self, c: u64) { self.cycles += c; }
    fn begin_cpu_cycle(&mut self) { self.cycles += 1; }
}

/// Runs `prog` at 0x0200 for `cycles` cycles; returns memory.
pub fn run(prog: &[u8], cycles: usize) -> Vec<u8> {
    let mut bus = FlatBus::new();
    bus.mem[0x200..0x200 + prog.len()].copy_from_slice(prog);
    bus.mem[0xFFFC] = 0x00; bus.mem[0xFFFD] = 0x02;
    let mut cpu = M6502Cpu::new(CpuVariant::NMOS6502);
    cpu.power_on(&mut bus);
    for _ in 0..cycles { bus.begin_cpu_cycle(); cpu.step_one_cycle(&mut bus); }
    bus.mem
}

