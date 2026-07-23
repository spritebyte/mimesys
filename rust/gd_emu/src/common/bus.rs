pub trait AddressBus {
    fn read_byte(&mut self, addr: u16) -> u8;
    fn write_byte(&mut self, addr: u16, value: u8);
    fn total_cycles(&self) -> u64 { 0 }
    fn read_word(&mut self, addr: u16) -> u16 {
        let lo = self.read_byte(addr) as u16;
        let hi = self.read_byte(addr + 1) as u16;
        (hi << 8) | lo
    }

    fn is_nmi_line_asserted(&mut self) -> bool;
    fn is_irq_line_asserted(&mut self) -> bool;
    fn is_nmi_enabled(&mut self) -> bool;
    fn step_cycles(&mut self, _cycles: u64);
    fn begin_cpu_cycle(&mut self);
}