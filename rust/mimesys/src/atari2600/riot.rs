// MOS 6532 RIOT: 128 bytes of RAM, two I/O ports, and an interval timer.
// It's all the RAM the 2600 has (zero page 0x80-0xFF, also the stack).
//
// Ports (active low):
//   SWCHA (0x280): joysticks. P0: bit 7 right, 6 left, 5 down, 4 up. P1: bits 3-0 same order.
//   SWCHB (0x282): console switches. bit 0 RESET, bit 1 SELECT, bit 3 color (1) / B&W (0),
//                  bit 6 P0 difficulty (0 = B/novice), bit 7 P1 difficulty.
//   SWACNT/SWBCNT (0x281/0x283): data direction (1 = output).
// Timer: write 0x294-0x297 = start counting down from the value every 1/8/64/1024 CPU cycles.
//   INTIM (0x284) reads the count. After it passes zero it sets the flag (TIMINT 0x285 bit 7)
//   and counts down every cycle; reading INTIM clears the flag and restores the interval.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub struct Riot {
    #[serde(with = "serde_bytes")]
    pub ram: Vec<u8>,
    timer: u8,
    interval: u16,          // current divider (1 after underflow)
    set_interval: u16,      // divider chosen by the last timer write
    prescale: u16,
    underflow: bool,
    pub swcha_in: u8,       // joystick lines (set from input)
    pub swchb_in: u8,       // console switches
    swacnt: u8,
    swbcnt: u8,
    swcha_out: u8,
    swchb_out: u8,
}

impl Riot {
    pub fn new() -> Self {
        Self {
            ram: vec![0; 128],
            timer: 0, interval: 1024, set_interval: 1024, prescale: 0, underflow: false,
            swcha_in: 0xFF,
            swchb_in: 0x0B,     // color, both difficulties B, RESET/SELECT up
            swacnt: 0, swbcnt: 0, swcha_out: 0, swchb_out: 0,
        }
    }

    /// One CPU cycle.
    pub fn tick(&mut self) {
        if self.prescale > 0 {
            self.prescale -= 1;
            return;
        }
        self.prescale = self.interval - 1;
        let (v, under) = self.timer.overflowing_sub(1);
        self.timer = v;
        if under {
            self.underflow = true;
            self.interval = 1;
            self.prescale = 0;
        }
    }

    /// Reads from 0x280-0x29F (address bits 0-2 pick the register).
    pub fn read(&mut self, addr: u16) -> u8 {
        match addr & 0x07 {
            0 => (self.swcha_in & !self.swacnt) | (self.swcha_out & self.swacnt),
            1 => self.swacnt,
            2 => (self.swchb_in & !self.swbcnt) | (self.swchb_out & self.swbcnt),
            3 => self.swbcnt,
            4 | 6 => {
                if self.underflow {
                    self.underflow = false;
                    self.interval = self.set_interval;
                    self.prescale = self.interval - 1;
                }
                self.timer
            }
            _ => if self.underflow { 0x80 } else { 0x00 },   // TIMINT
        }
    }

    pub fn write(&mut self, addr: u16, v: u8) {
        if addr & 0x04 == 0 {
            match addr & 0x03 {
                0 => self.swcha_out = v,
                1 => self.swacnt = v,
                2 => self.swchb_out = v,
                _ => self.swbcnt = v,
            }
        } else if addr & 0x10 != 0 {
            // TIM1T / TIM8T / TIM64T / T1024T: decrements on the next cycle, then every interval
            self.set_interval = [1, 8, 64, 1024][(addr & 0x03) as usize];
            self.interval = self.set_interval;
            self.timer = v;
            self.prescale = 0;
            self.underflow = false;
        }
        // else: PA7 edge-detect control, unused by games
    }

    #[cfg(test)]
    pub(crate) fn timer_value(&self) -> u8 { self.timer }
}
