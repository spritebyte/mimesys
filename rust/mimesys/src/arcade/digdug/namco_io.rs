// Namco custom I/O chips on the Galaga-family CPU board (MAME namco06/51/53.cpp).
//
// 06xx: the main CPU's window onto up to four custom chips. Control register: bits 0-3
//   chip selects, bit 4 = read (1) / write (0), bits 5-7 = clock divider (0 = stopped).
//   While running it toggles at 2 x 48 kHz / 2^div: on each rising half it drives R/W to
//   the chips, pulses the chip select lines (the MCUs' IRQ) and pulses the main CPU's
//   NMI, whose handler moves the next byte. The first NMI of a read is skipped so the
//   chip has a cycle to answer.
// 51xx: MB8843 running Namco's I/O program: coins, credits, joysticks (R0-R3 = inputs,
//   K = R/W line + the low data bits, O = data to the CPU).
// 53xx: MB8843 reading the DIP switches for Dig Dug (K = mode bits from the CPU latch).

use crate::common::cpu::mb88::{Mb88, Mb88Io};

/// Z80 cycles per 06xx half period with divider 0 (48 kHz clock, toggling twice a period).
const BASE_HALF_PERIOD: u64 = 32;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Namco06 {
    pub control: u8,
    timer_state: bool,
    read_stretch: bool,
    running: bool,
    next_toggle: u64,
    half_period: u64,
    pub nmi: bool,
}

/// What a 06xx toggle drives.
pub struct Toggle {
    pub rw: Option<bool>,
    pub chipsel: [bool; 4],
}

impl Namco06 {
    pub fn new() -> Self {
        Self { control: 0, timer_state: false, read_stretch: false, running: false,
               next_toggle: 0, half_period: BASE_HALF_PERIOD, nmi: false }
    }

    /// Control write. Returns true when the clock stopped (chip selects drop).
    pub fn ctrl_w(&mut self, data: u8, now: u64) -> bool {
        self.control = data;
        if data & 0xE0 == 0 {
            self.running = false;
            self.timer_state = false;
            self.nmi = false;
            return true;
        }
        if data & 0x10 != 0 {
            self.nmi = false;
            self.read_stretch = true;
        } else {
            self.read_stretch = false;
        }
        let div = 1u64 << ((data & 0xE0) >> 5);
        self.half_period = BASE_HALF_PERIOD * div;
        // first toggle on the next 48 kHz clock edge (64 Z80 cycles a clock)
        self.next_toggle = (now / 64 + 1) * 64;
        self.running = true;
        false
    }

    /// One toggle if it is due by `now` (Z80 cycles); call until None.
    pub fn advance(&mut self, now: u64) -> Option<Toggle> {
        if !self.running || now < self.next_toggle { return None; }
        self.next_toggle += self.half_period;
        self.timer_state = !self.timer_state;
        let rw = if self.timer_state { Some(self.control & 0x10 != 0) } else { None };
        self.nmi = self.timer_state && !self.read_stretch;
        self.read_stretch = false;
        let cs = |b: u8| self.control & (1 << b) != 0 && self.timer_state;
        Some(Toggle { rw, chipsel: [cs(0), cs(1), cs(2), cs(3)] })
    }
}

struct N51Io<'a> { port_o: &'a mut u8, rw: u8, inputs: [u8; 4], out: &'a mut u8 }
impl Mb88Io for N51Io<'_> {
    fn read_k(&mut self) -> u8 { (self.rw << 3) | (*self.port_o & 7) }
    fn write_o(&mut self, data: u8, _mask: u8) { *self.port_o = data; }
    fn write_p(&mut self, data: u8) { *self.out = data; }
    fn read_r(&mut self, port: usize) -> u8 { self.inputs[port & 3] }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Namco51 {
    pub mcu: Mb88,
    pub port_o: u8,
    pub rw: u8,
    /// R0-R3: IN0 low, IN0 high, IN1 low, IN1 high
    #[serde(skip)]
    pub inputs: [u8; 4],
    pub out: u8,
}

impl Namco51 {
    pub fn new(rom: Vec<u8>) -> Self {
        Self { mcu: Mb88::new(rom, 64), port_o: 0, rw: 0, inputs: [0x0F; 4], out: 0 }
    }
    pub fn step(&mut self) -> u32 {
        let mut io = N51Io { port_o: &mut self.port_o, rw: self.rw, inputs: self.inputs, out: &mut self.out };
        self.mcu.step(&mut io)
    }
}

struct N53Io<'a> { port_o: &'a mut u8, k: u8, inputs: [u8; 4] }
impl Mb88Io for N53Io<'_> {
    fn read_k(&mut self) -> u8 { self.k }
    fn write_o(&mut self, data: u8, _mask: u8) { *self.port_o = data; }
    fn write_p(&mut self, _data: u8) {}
    fn read_r(&mut self, port: usize) -> u8 { self.inputs[port & 3] }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Namco53 {
    pub mcu: Mb88,
    pub port_o: u8,
    pub k: u8,
    /// R0-R3: DSWA low, DSWA high, DSWB low, DSWB high
    #[serde(skip)]
    pub inputs: [u8; 4],
}

impl Namco53 {
    pub fn new(rom: Vec<u8>) -> Self {
        Self { mcu: Mb88::new(rom, 64), port_o: 0, k: 0, inputs: [0; 4] }
    }
    pub fn step(&mut self) -> u32 {
        let mut io = N53Io { port_o: &mut self.port_o, k: self.k, inputs: self.inputs };
        self.mcu.step(&mut io)
    }
}
