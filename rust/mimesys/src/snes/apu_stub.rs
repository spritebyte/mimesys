//! Stand-in for the sound side until stage 3 (SPC700 + DSP).
//!
//! The CPU talks to the APU through four ports (2140-2143). After reset the APU's boot
//! ROM puts AA BB on ports 0 and 1 and then echoes what the CPU writes while it takes an
//! upload. Here every port simply reads back the last value the CPU wrote to it, which
//! gets games through the upload handshake; a sound driver that answers with other
//! values may still keep a game waiting.

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct ApuPorts {
    /// what the CPU reads
    pub out: [u8; 4],
}

impl ApuPorts {
    pub fn new() -> Self { Self { out: [0xAA, 0xBB, 0, 0] } }
    pub fn cpu_read(&mut self, port: usize) -> u8 { self.out[port] }
    pub fn cpu_write(&mut self, port: usize, v: u8) { self.out[port] = v; }
    /// Master clocks passing (the real APU will run here).
    pub fn run(&mut self, _clocks: u32) {}
}
