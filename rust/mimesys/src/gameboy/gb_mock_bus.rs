use crate::gameboy::gb_bus::Bus;
/*
pub trait MockBus {
    fn read(&mut self, addr: u16) -> u8;
    fn write(&mut self, addr: u16, value: u8);
    fn peek(&self, addr: u16) -> u8;
    fn irq_pending(&self) -> u8;
    fn ack_irq(&mut self, index: u8);   // clear bit 'index' in IF
}
*/
#[derive(Clone, Copy)]
pub enum Irq { VBlank = 0, LcdStat = 1, Timer = 2, Serial = 3, Joypad = 4 }

pub struct MockBus {
    pub ram: [u8; 65536],
    pub ie: u8,
    pub iflags: u8,
    serial_data_buffer: u8,
    serial_control: u8,
}

impl MockBus {
    pub fn new() -> Self {
        Self {
            ram: [0; 65536],
            ie: 0,
            iflags: 0,
            serial_data_buffer: 0,
            serial_control: 0,
        }
    }

    pub fn request_irq(&mut self, irq: Irq) {
        self.iflags |= 1 << (irq as u8);
    }
}

impl Bus for MockBus {
    fn irq_pending(&self) -> u8 {
        self.ie & self.iflags & 0x1F
    }

    fn ack_irq(&mut self, which: u8) {

    }

    fn read(&mut self, addr: u16) -> u8 {
        self.ram[addr as usize]
    }

    fn peek(&self, addr: u16) -> u8 {
        self.ram[addr as usize]
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            0xFF01 => {
                self.serial_data_buffer = value;
            }
            0xFF02 => {
                self.serial_control = value;
                if (value & 0x80) != 0 {
                    // Bit 7 being set means a transfer was requested!
                    // This is the hook where future Network/Link Cable component 
                    // will intercept execution and talk to the other emulator instance.
//                    self.link_cable.initiate_transfer(self.serial_data_buffer, value);
                }
            }
            _ => { self.ram[addr as usize] = value; }
        }
    }
}