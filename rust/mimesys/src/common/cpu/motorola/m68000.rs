use serde::{Serialize, Deserialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum Model { M68000, M68008, M68010 }

pub trait M68kBus {
    fn read(&mut self, addr: u16) -> u8;
    fn write(&mut self, addr: u16, value: u8);
    fn port_read(&mut self, _port: usize) -> u8 { 0xFF }
    fn port_write(&mut self, _port: usize, _data: u8, _ddr: u8) {}
}

struct StatusRegister {
    trace: u8,          // T0, T1 (2 bits)
    supervisor: bool,   // S bit
    master: bool,       // M bit (68020+)
    interrupt_mask: u8, // ipl (3 bits)
    x: bool,            // extend
    n: bool,            // negative
    z: bool,            // zero
    v: bool,            // overflow
    c: bool,            // carry
}

#[derive(Clone, Serialize, Deserialize)]
pub struct M68k {
    // --- Base 68000 registers
    // Data registers D0-D7
    pub d: [u32; 8],
    // Address registers A0-A7
    pub a: [u32; 8],
    pub pc: u32,
    pub sr: StatusRegister

    // -- 68020+ Extension fields (only active if cpu_type >= M68020)
    pub model: Model,
    pub vbr: u32,
    pub msp: u32,
    pub isp: u32,
    pub cacr: u32,
    pub caar: u32,
    pub nmi_line: bool,
    nmi_asserted: bool,
    pub firq_line: bool,
    pub irq_line: bool,
    free_run: bool,
//    wait: Wait,
    #[serde(skip)] cycles: u32,
    #[serde(skip, default = "default_mode")] mode: Mode,
    #[serde(skip)] ea: u16,
    #[serde(skip)] opcode: u8,
}

impl M68kCpu {

}
