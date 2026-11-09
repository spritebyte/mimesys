//! NEC uPD7759 ADPCM speech chip, stand-alone ("master") mode only (MAME upd7759.cpp,
//! BSD-3-Clause; Aaron Giles and others).
//!
//! The host writes a sample number to the port and pulses /START; the chip reads the ROM
//! header (`nn 5a a5 69 55`, then a table of 16-bit sample offsets) and plays the sample:
//! a list of blocks (silence / n nibbles at a rate / repeat), each nibble a step of the
//! ADPCM converter. The chip runs on a 640 kHz clock, one state-machine step per
//! `clocks_left` clocks, exactly as in MAME, so /BUSY has the right timing.

use serde::{Deserialize, Serialize};

const STEP: [[i32; 16]; 16] = [
    [0, 0, 1, 2, 3, 5, 7, 10, 0, 0, -1, -2, -3, -5, -7, -10],
    [0, 1, 2, 3, 4, 6, 8, 13, 0, -1, -2, -3, -4, -6, -8, -13],
    [0, 1, 2, 4, 5, 7, 10, 15, 0, -1, -2, -4, -5, -7, -10, -15],
    [0, 1, 3, 4, 6, 9, 13, 19, 0, -1, -3, -4, -6, -9, -13, -19],
    [0, 2, 3, 5, 8, 11, 15, 23, 0, -2, -3, -5, -8, -11, -15, -23],
    [0, 2, 4, 7, 10, 14, 19, 29, 0, -2, -4, -7, -10, -14, -19, -29],
    [0, 3, 5, 8, 12, 16, 22, 33, 0, -3, -5, -8, -12, -16, -22, -33],
    [1, 4, 7, 10, 15, 20, 29, 43, -1, -4, -7, -10, -15, -20, -29, -43],
    [1, 4, 8, 13, 18, 25, 35, 53, -1, -4, -8, -13, -18, -25, -35, -53],
    [1, 6, 10, 16, 22, 31, 43, 64, -1, -6, -10, -16, -22, -31, -43, -64],
    [2, 7, 12, 19, 27, 37, 51, 76, -2, -7, -12, -19, -27, -37, -51, -76],
    [2, 9, 16, 24, 34, 46, 64, 96, -2, -9, -16, -24, -34, -46, -64, -96],
    [3, 11, 19, 29, 41, 57, 79, 117, -3, -11, -19, -29, -41, -57, -79, -117],
    [4, 13, 24, 36, 50, 69, 96, 143, -4, -13, -24, -36, -50, -69, -96, -143],
    [4, 16, 29, 44, 62, 85, 118, 175, -4, -16, -29, -44, -62, -85, -118, -175],
    [6, 20, 36, 54, 76, 104, 144, 214, -6, -20, -36, -54, -76, -104, -144, -214],
];
const STATE_TABLE: [i32; 16] = [-1, -1, 0, 0, 1, 2, 2, 3, -1, -1, 0, 0, 1, 2, 2, 3];

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
enum State {
    Idle, DropDrq, Start, FirstReq, LastSample, Dummy1, AddrMsb, AddrLsb, Dummy2,
    BlockHeader, NibbleCount, NibbleMsn, NibbleLsn,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Upd7759 {
    state: State,
    clocks_left: i32,
    nibbles_left: i32,
    repeat_count: u32,
    post_drq_state: State,
    post_drq_clocks: i32,
    req_sample: u8,
    last_sample: u8,
    block_header: u8,
    sample_rate: i32,
    first_valid_header: bool,
    offset: u32,
    repeat_offset: u32,
    adpcm_state: i32,
    adpcm_data: u8,
    sample: i32,
    fifo_in: u8,
    reset: bool,
    start: bool,
    drq: bool,
}

impl Upd7759 {
    pub fn new() -> Self {
        Self {
            state: State::Idle, clocks_left: 0, nibbles_left: 0, repeat_count: 0,
            post_drq_state: State::Idle, post_drq_clocks: 0, req_sample: 0, last_sample: 0,
            block_header: 0, sample_rate: 0, first_valid_header: false, offset: 0, repeat_offset: 0,
            adpcm_state: 0, adpcm_data: 0, sample: 0, fifo_in: 0, reset: true, start: true, drq: false,
        }
    }

    fn device_reset(&mut self) {
        let (fifo, start, reset) = (self.fifo_in, self.start, self.reset);
        *self = Self::new();
        self.fifo_in = fifo;
        self.start = start;
        self.reset = reset;
    }

    /// The /RESET pin: the chip resets on a 1 -> 0 edge.
    pub fn reset_w(&mut self, level: bool) {
        let old = self.reset;
        self.reset = level;
        if old && !level { self.device_reset(); }
    }

    /// The /START pin: a 1 -> 0 edge starts the sample selected by the port (when idle).
    pub fn start_w(&mut self, level: bool) {
        let old = self.start;
        self.start = level;
        if self.state == State::Idle && old && !level && self.reset {
            self.state = State::Start;
            self.clocks_left = 0;
        }
    }

    pub fn port_w(&mut self, data: u8) { self.fifo_in = data; }

    /// The /BUSY pin: high when idle.
    pub fn busy_r(&self) -> bool { self.state == State::Idle }

    fn update_adpcm(&mut self, data: u8) {
        self.sample += STEP[self.adpcm_state as usize][data as usize];
        self.adpcm_state = (self.adpcm_state + STATE_TABLE[data as usize]).clamp(0, 15);
    }

    fn advance_state(&mut self, rom: &[u8]) {
        let rd = |a: u32| rom.get(a as usize).copied().unwrap_or(0);
        match self.state {
            State::Idle => self.clocks_left = 4,
            State::DropDrq => {
                self.drq = false;
                self.clocks_left = self.post_drq_clocks;
                self.state = self.post_drq_state;
            }
            State::Start => {
                self.req_sample = self.fifo_in;
                self.clocks_left = 70;
                self.state = State::FirstReq;
            }
            State::FirstReq => {
                self.drq = true;
                self.clocks_left = 44;
                self.state = State::LastSample;
            }
            State::LastSample => {
                self.last_sample = rd(0);
                self.drq = true;
                self.clocks_left = 28;
                self.state = if self.req_sample > self.last_sample { State::Idle } else { State::Dummy1 };
            }
            State::Dummy1 => {
                self.drq = true;
                self.clocks_left = 32;
                self.state = State::AddrMsb;
            }
            State::AddrMsb => {
                self.offset = (rd(self.req_sample as u32 * 2 + 5) as u32) << 9;
                self.drq = true;
                self.clocks_left = 44;
                self.state = State::AddrLsb;
            }
            State::AddrLsb => {
                self.offset |= (rd(self.req_sample as u32 * 2 + 6) as u32) << 1;
                self.drq = true;
                self.clocks_left = 36;
                self.state = State::Dummy2;
            }
            State::Dummy2 => {
                self.offset += 1;
                self.first_valid_header = false;
                self.drq = true;
                self.clocks_left = 36;
                self.state = State::BlockHeader;
            }
            State::BlockHeader => {
                if self.repeat_count > 0 {
                    self.repeat_count -= 1;
                    self.offset = self.repeat_offset;
                }
                self.block_header = rd(self.offset);
                self.offset += 1;
                self.drq = true;
                let h = self.block_header;
                match h & 0xC0 {
                    0x00 => {
                        self.clocks_left = 1024 * ((h as i32 & 0x3F) + 1);
                        self.state = if h == 0 && self.first_valid_header { State::Idle } else { State::BlockHeader };
                        self.sample = 0;
                        self.adpcm_state = 0;
                    }
                    0x40 => {
                        self.sample_rate = (h as i32 & 0x3F) + 1;
                        self.nibbles_left = 256;
                        self.clocks_left = 36;
                        self.state = State::NibbleMsn;
                    }
                    0x80 => {
                        self.sample_rate = (h as i32 & 0x3F) + 1;
                        self.clocks_left = 36;
                        self.state = State::NibbleCount;
                    }
                    _ => {
                        self.repeat_count = (h as u32 & 7) + 1;
                        self.repeat_offset = self.offset;
                        self.clocks_left = 36;
                        self.state = State::BlockHeader;
                    }
                }
                if h != 0 { self.first_valid_header = true; }
            }
            State::NibbleCount => {
                self.nibbles_left = rd(self.offset) as i32 + 1;
                self.offset += 1;
                self.drq = true;
                self.clocks_left = 36;
                self.state = State::NibbleMsn;
            }
            State::NibbleMsn => {
                self.adpcm_data = rd(self.offset);
                self.offset += 1;
                self.update_adpcm(self.adpcm_data >> 4);
                self.drq = true;
                self.clocks_left = self.sample_rate * 4;
                self.nibbles_left -= 1;
                self.state = if self.nibbles_left == 0 { State::BlockHeader } else { State::NibbleLsn };
            }
            State::NibbleLsn => {
                self.update_adpcm(self.adpcm_data & 15);
                self.clocks_left = self.sample_rate * 4;
                self.nibbles_left -= 1;
                self.state = if self.nibbles_left == 0 { State::BlockHeader } else { State::NibbleMsn };
            }
        }
        if self.drq {
            self.post_drq_state = self.state;
            self.post_drq_clocks = self.clocks_left - 21;
            self.state = State::DropDrq;
            self.clocks_left = 21;
        }
    }

    /// One tick of the 640 kHz clock.
    pub fn clock(&mut self, rom: &[u8]) {
        if self.state == State::Idle { return; }
        self.clocks_left -= 1;
        if self.clocks_left <= 0 { self.advance_state(rom); }
    }

    /// Current DAC output, +-1.0 for 256 steps of the sum (MAME: sample * 128 / 32768).
    pub fn output(&self) -> f32 {
        if self.state == State::Idle { 0.0 } else { self.sample as f32 * (128.0 / 32768.0) }
    }
}

impl Default for Upd7759 { fn default() -> Self { Self::new() } }

#[cfg(test)]
mod tests {
    use super::*;

    /// A ROM with one sample: header, offset table, then one 256-nibble block of ramp data.
    fn rom() -> Vec<u8> {
        let mut r = vec![0u8; 0x400];
        r[..5].copy_from_slice(&[0, 0x5A, 0xA5, 0x69, 0x55]);
        // sample 0 starts at (0x0010 * 2) = 0x20
        r[5] = 0x00; r[6] = 0x10;
        r[0x20] = 0x00;                   // first byte: the dummy before the header
        r[0x21] = 0x40 | 3;               // 256 nibbles, rate 4
        for i in 0..128 { r[0x22 + i] = 0x22; }   // nibble 2: +1 each
        r[0x22 + 128] = 0x00;             // end of sample
        r
    }

    fn run(u: &mut Upd7759, rom: &[u8], clocks: u32) -> Vec<f32> {
        let mut out = Vec::new();
        for _ in 0..clocks { u.clock(rom); out.push(u.output()); }
        out
    }

    #[test]
    fn busy_goes_low_while_playing_and_the_sample_rises() {
        let rom = rom();
        let mut u = Upd7759::new();
        assert!(u.busy_r());
        u.port_w(0);
        u.start_w(true);
        u.start_w(false);
        assert!(!u.busy_r(), "busy once started");
        let out = run(&mut u, &rom, 20_000);
        assert!(u.busy_r(), "back to idle after the end mark");
        let peak = out.iter().cloned().fold(0.0f32, f32::max);
        assert!(peak > 0.1, "the ADPCM ramp is audible, peak {peak}");
    }

    #[test]
    fn a_sample_past_the_last_one_does_nothing() {
        let rom = rom();
        let mut u = Upd7759::new();
        u.port_w(5);                      // header says the last sample is 0
        u.start_w(true);
        u.start_w(false);
        run(&mut u, &rom, 2000);
        assert!(u.busy_r());
        assert_eq!(u.output(), 0.0);
    }

    #[test]
    fn reset_edge_stops_playback_and_start_needs_reset_high() {
        let rom = rom();
        let mut u = Upd7759::new();
        u.start_w(true);
        u.start_w(false);
        run(&mut u, &rom, 500);
        assert!(!u.busy_r());
        u.reset_w(false);
        assert!(u.busy_r(), "falling reset aborts");
        u.start_w(true);
        u.start_w(false);
        assert!(u.busy_r(), "no start while held in reset");
        u.reset_w(true);
        u.start_w(true);
        u.start_w(false);
        assert!(!u.busy_r());
    }
}
