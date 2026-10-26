// Intel 8255 PPI (Programmable Peripheral Interface), mode 0 only:
// three 8-bit ports (A, B, C) that are each set as input or output by a control word.
// Konami boards (Frogger, Scramble, ...) use two of them for inputs and the sound latch.
//
// Registers (A1 A0): 0 = port A, 1 = port B, 2 = port C, 3 = control.
// Control word, bit 7 = 1: mode set. Bit 4: A is input, bit 3: C upper is input,
//   bit 1: B is input, bit 0: C lower is input. A mode set clears the output latches.
// Control word, bit 7 = 0: set/reset one bit of port C (bits 1-3 = bit number, bit 0 = value).

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Ppi8255 {
    pub out: [u8; 3],     // output latches
    control: u8,
}

/// What a write did, so the board can react (e.g. send a sound command).
pub enum PpiWrite { None, PortA(u8), PortB(u8), PortC(u8) }

impl Default for Ppi8255 {
    fn default() -> Self { Self { out: [0; 3], control: 0x9B } }   // power-up: all inputs
}

impl Ppi8255 {
    fn a_is_input(&self) -> bool { self.control & 0x10 != 0 }
    fn b_is_input(&self) -> bool { self.control & 0x02 != 0 }
    fn c_input_mask(&self) -> u8 {
        (if self.control & 0x08 != 0 { 0xF0 } else { 0 }) | (if self.control & 0x01 != 0 { 0x0F } else { 0 })
    }

    /// `inputs` = what the board drives onto ports A, B, C.
    pub fn read(&self, reg: u8, inputs: [u8; 3]) -> u8 {
        match reg & 3 {
            0 => if self.a_is_input() { inputs[0] } else { self.out[0] },
            1 => if self.b_is_input() { inputs[1] } else { self.out[1] },
            2 => { let m = self.c_input_mask(); (inputs[2] & m) | (self.out[2] & !m) }
            _ => 0xFF,
        }
    }

    pub fn write(&mut self, reg: u8, val: u8) -> PpiWrite {
        match reg & 3 {
            0 => { self.out[0] = val; if self.a_is_input() { PpiWrite::None } else { PpiWrite::PortA(val) } }
            1 => { self.out[1] = val; if self.b_is_input() { PpiWrite::None } else { PpiWrite::PortB(val) } }
            2 => { self.out[2] = val; PpiWrite::PortC(val) }
            _ => {
                if val & 0x80 != 0 {
                    self.control = val;
                    self.out = [0; 3];
                    PpiWrite::None
                } else {
                    let bit = (val >> 1) & 7;
                    if val & 1 != 0 { self.out[2] |= 1 << bit } else { self.out[2] &= !(1 << bit) }
                    PpiWrite::PortC(self.out[2])
                }
            }
        }
    }
}
