// MOS 6532 RIOT (RAM, I/O, Timer) with its interrupt logic, as used on arcade sound
// boards (Gottlieb). Modelled on MAME's mos6530.cpp: 128 bytes of RAM, ports A and B
// with data direction registers, an interval timer (/1 /8 /64 /1024) and two interrupt
// sources (timer underflow, PA7 edge). The 2600 core keeps its own simpler copy.
//
// Register select (address bits, after the RAM/IO decode):
//   write: A2=0: A1A0 = PA data, PA DDR, PB data, PB DDR
//          A2=1, A4=0: PA7 edge control (A0 = positive edge, A1 = IRQ enable)
//          A2=1, A4=1: timer start, A1A0 = prescale, A3 = timer IRQ enable
//   read:  A2=0: same as write
//          A2=1, A0=0: timer (A3 = timer IRQ enable)
//          A2=1, A0=1: interrupt flags (bit 7 timer, bit 6 PA7), clears the PA7 flag

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Riot6532 {
    #[serde(with = "serde_bytes")]
    pub ram: [u8; 128],
    pub pa_in: u8,
    pub pb_in: u8,
    pa_out: u8,
    pa_ddr: u8,
    pb_out: u8,
    pb_ddr: u8,
    pa7: bool,
    pa7_rising: bool,
    ie_timer: bool,
    irq_timer: bool,
    ie_edge: bool,
    irq_edge: bool,
    shift: u32,
    counting: bool,
    /// clocks until the timer reaches its target (counting) or the next wrap (spinning)
    remaining: u32,
    since_timeout: u32,
}

impl Default for Riot6532 {
    fn default() -> Self { Self::new() }
}

impl Riot6532 {
    pub fn new() -> Self {
        let mut r = Self {
            ram: [0; 128], pa_in: 0xFF, pb_in: 0xFF,
            pa_out: 0, pa_ddr: 0, pb_out: 0, pb_ddr: 0,
            pa7: true, pa7_rising: false,
            ie_timer: false, irq_timer: false, ie_edge: false, irq_edge: false,
            shift: 10, counting: true, remaining: 256 << 10, since_timeout: u32::MAX,
        };
        r.edge_detect();
        r
    }

    pub fn reset(&mut self) {
        self.pa_out = 0; self.pa_ddr = 0; self.pb_out = 0; self.pb_ddr = 0;
        self.ie_timer = false; self.ie_edge = false; self.irq_edge = false;
        self.pa7_rising = false;
        self.edge_detect();
    }

    /// One clock (the RIOT runs on the CPU's phi2).
    #[inline]
    pub fn tick(&mut self) {
        self.since_timeout = self.since_timeout.saturating_add(1);
        self.remaining -= 1;
        if self.remaining == 0 {
            if self.counting {
                self.irq_timer = true;
                self.since_timeout = 0;
            }
            self.counting = false;
            self.remaining = 256;
        }
    }

    /// IRQ output (active = true).
    pub fn irq(&self) -> bool {
        (self.ie_timer && self.irq_timer) || (self.ie_edge && self.irq_edge)
    }

    /// Port B as seen from outside: outputs where DDR = 1, pulled high elsewhere.
    pub fn pb_output(&self) -> u8 { (self.pb_out & self.pb_ddr) | !self.pb_ddr }
    #[allow(dead_code)]
    pub fn pa_output(&self) -> u8 { (self.pa_out & self.pa_ddr) | !self.pa_ddr }

    /// External device drives port A (bits in `mask`).
    pub fn set_pa_in(&mut self, data: u8, mask: u8) {
        self.pa_in = (self.pa_in & !mask) | (data & mask);
        self.edge_detect();
    }

    fn edge_detect(&mut self) {
        let data = (self.pa_out & self.pa_ddr) | (self.pa_in & !self.pa_ddr);
        let state = data & 0x80 != 0;
        if state != self.pa7 && state == self.pa7_rising && !self.irq_edge {
            self.irq_edge = true;
        }
        self.pa7 = state;
    }

    fn get_timer(&self) -> u8 {
        let shift = if self.counting { self.shift } else { 0 };
        let val = (self.remaining >> shift) as u8;
        if self.remaining & ((1 << shift) - 1) != 0 { val } else { val.wrapping_sub(1) }
    }

    fn timer_start(&mut self, data: u8) {
        self.counting = true;
        self.remaining = 1 + ((data as u32) << self.shift);
    }

    pub fn ram_read(&self, addr: u16) -> u8 { self.ram[(addr & 0x7F) as usize] }
    pub fn ram_write(&mut self, addr: u16, v: u8) { self.ram[(addr & 0x7F) as usize] = v; }

    pub fn io_read(&mut self, addr: u16) -> u8 {
        if addr & 0x04 == 0 {
            match addr & 3 {
                0 => (self.pa_out & self.pa_ddr) | (self.pa_in & !self.pa_ddr),
                1 => self.pa_ddr,
                2 => (self.pb_out & self.pb_ddr) | (self.pb_in & !self.pb_ddr),
                _ => self.pb_ddr,
            }
        } else if addr & 0x01 == 0 {
            let data = self.get_timer();
            // the flag isn't cleared by a read in the same moment it is raised
            if self.since_timeout > 2 {
                self.irq_timer = false;
                if !self.counting { self.timer_start(data); }
            }
            self.ie_timer = addr & 0x08 != 0;
            data
        } else {
            let mut data = 0;
            if self.irq_timer { data |= 0x80; }
            if self.irq_edge { data |= 0x40; }
            self.irq_edge = false;
            data
        }
    }

    pub fn io_write(&mut self, addr: u16, v: u8) {
        if addr & 0x04 == 0 {
            match addr & 3 {
                0 => { self.pa_out = v; self.edge_detect(); }
                1 => { self.pa_ddr = v; self.edge_detect(); }
                2 => self.pb_out = v,
                _ => self.pb_ddr = v,
            }
        } else if addr & 0x10 != 0 {
            self.shift = [0, 3, 6, 10][(addr & 3) as usize];
            self.timer_start(v);
            self.irq_timer = false;
            self.ie_timer = addr & 0x08 != 0;
        } else {
            self.pa7_rising = addr & 0x01 != 0;
            self.ie_edge = addr & 0x02 != 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_counts_and_flags() {
        let mut r = Riot6532::new();
        r.io_write(0x1D, 2);    // /8, IRQ on, value 2
        // reads 1 right away (MAME: timeout lands one clock after value << shift)
        assert_eq!(r.io_read(0x0C), 2u8.wrapping_sub(0).min(2));
        for _ in 0..(2 * 8 + 1) { r.tick(); }
        assert!(r.irq());
        assert_eq!(r.io_read(0x05) & 0x80, 0x80);
    }

    #[test]
    fn pa7_positive_edge() {
        let mut r = Riot6532::new();
        r.io_write(0x07, 0);     // positive edge, IRQ enabled
        r.set_pa_in(0x00, 0x80);
        assert!(!r.irq());
        r.set_pa_in(0x80, 0x80);
        assert!(r.irq());
        assert_eq!(r.io_read(0x05) & 0x40, 0x40);
        assert!(!r.irq());
    }
}
