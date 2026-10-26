// GI ER-2055 EAROM: 64 bytes of non-volatile memory (Dig Dug's high scores, many Atari
// games). After MAME's er2055.cpp (Aaron Giles).
//
// Address and data are latched by the board. Control lines C1/C2 pick the mode:
// C1 = read (data latched on the falling clock edge), C2 alone = erase (byte -> FF),
// neither = write (ANDed in, so a byte must be erased first). CS1 + CS2 select the chip.

const CK: u8 = 0x01;
const C1: u8 = 0x02;
const C2: u8 = 0x04;
const CS1: u8 = 0x08;
const CS2: u8 = 0x10;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Er2055 {
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
    control: u8,
    address: u8,
    latch: u8,
    #[serde(skip)]
    pub dirty: bool,
}

impl Default for Er2055 {
    fn default() -> Self { Self::new() }
}

impl Er2055 {
    pub fn new() -> Self {
        Self { data: vec![0xFF; 64], control: 0, address: 0, latch: 0, dirty: false }
    }

    pub fn set_address(&mut self, a: u8) { self.address = a & 0x3F; }
    pub fn set_data(&mut self, v: u8) { self.latch = v; }
    /// The output latch (valid after a read clock).
    pub fn read(&self) -> u8 { self.latch }

    pub fn set_control(&mut self, cs1: bool, cs2: bool, c1: bool, c2: bool) {
        let old = self.control;
        self.control = (old & CK)
            | if c1 { C1 } else { 0 } | if c2 { C2 } else { 0 }
            | if cs1 { CS1 } else { 0 } | if cs2 { CS2 } else { 0 };
        if self.control & (CS1 | CS2) != (CS1 | CS2) || self.control == old { return; }
        self.update();
    }

    pub fn set_clk(&mut self, state: bool) {
        let old = self.control;
        if state { self.control |= CK } else { self.control &= !CK }
        if self.control & (CS1 | CS2) == (CS1 | CS2) && self.control != old && !state {
            if self.control & C1 != 0 {
                self.latch = self.data[self.address as usize];
            }
            self.update();
        }
    }

    fn update(&mut self) {
        let a = self.address as usize;
        match self.control & (C1 | C2) {
            0 => {
                let v = self.data[a] & self.latch;
                if v != self.data[a] { self.data[a] = v; self.dirty = true; }
            }
            C2 => {
                if self.data[a] != 0xFF { self.data[a] = 0xFF; self.dirty = true; }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn erase_write_read() {
        let mut e = Er2055::new();
        e.set_address(5);
        e.set_data(0x3C);
        e.set_control(true, true, false, false);    // write mode, selected: writes now
        assert_eq!(e.data[5], 0x3C);
        e.set_control(true, true, false, true);     // erase
        assert_eq!(e.data[5], 0xFF);
        e.set_data(0x12);
        e.set_control(true, true, false, false);
        e.set_control(true, true, true, false);     // read mode
        e.set_clk(true);
        e.set_clk(false);
        assert_eq!(e.read(), 0x12);
        assert!(e.dirty);
    }
}
