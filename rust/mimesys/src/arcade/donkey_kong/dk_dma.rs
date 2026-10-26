#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct I8257 {
    pub addr: [u16; 4],
    pub count: [u16; 4],
    pub mode: u8,
    high_byte_next: bool,
}

impl I8257 {
    pub fn write(&mut self, reg: u8, val: u8) {
        match reg {
            0..=7 => {
                let ch = (reg >> 1) as usize;
                let target = if reg & 1 == 0 { &mut self.addr[ch] } else { &mut self.count[ch] };
                *target = if self.high_byte_next {
                    (*target & 0x00FF) | ((val as u16) << 8)
                } else {
                    (*target & 0xFF00) | val as u16
                };
                self.high_byte_next = !self.high_byte_next;
            }
            8 => {
                self.mode = val;
                self.high_byte_next = false;
            }
            _ => { }
        }
    }

    pub fn length(&self, ch: usize) -> u16 {
        (self.count[ch] & 0x3FFF) + 1
    }
}