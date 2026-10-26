//! TI TMS5220 / TMS5220C LPC speech synthesizer. Port of MAME's tms5220.cpp
//! (Frank Palazzolo, Aaron Giles, Jonathan Gevaryahu, Raphael Nabet, Couriersud,
//! Michael Zapf), the 52xx paths: Speak External through the 16-byte FIFO, the
//! commands, the 5220C rate control, the lattice filter and the analog output clip.
//! No speech ROM (VSM) is attached: Speak from VSM reads stop frames, like MAME
//! with nothing connected.
//!
//! The host bus works like MAME's "true timing" mode: data_w latches a byte, /WS low
//! starts a 16-clock ready cycle that then performs the write. `clock()` once per chip
//! clock; every 80 clocks it returns a sample.

use serde::{Deserialize, Serialize};

const FIFO_SIZE: usize = 16;
const NOCOMMAND: u8 = 0xFF;
const RELOAD_TABLE: [u8; 4] = [0, 2, 4, 6];

const ENERGY: [i32; 16] = [0, 1, 2, 3, 4, 6, 8, 11, 16, 23, 33, 47, 63, 85, 114, 0];
const PITCH: [i32; 64] = [
    0, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29,
    30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 44, 46, 48,
    50, 52, 53, 56, 58, 60, 62, 65, 68, 70, 72, 76, 78, 80, 84, 86,
    91, 94, 98, 101, 105, 109, 114, 118, 122, 127, 132, 137, 142, 148, 153, 159,
];
const KBITS: [u32; 10] = [5, 5, 4, 4, 4, 4, 4, 3, 3, 3];
const K1: [i32; 32] = [
    -501, -498, -497, -495, -493, -491, -488, -482, -478, -474, -469, -464, -459, -452, -445, -437,
    -412, -380, -339, -288, -227, -158, -81, -1, 80, 157, 226, 287, 337, 379, 411, 436,
];
const K2: [i32; 32] = [
    -328, -303, -274, -244, -211, -175, -138, -99, -59, -18, 24, 64, 105, 143, 180, 215,
    248, 278, 306, 331, 354, 374, 392, 408, 422, 435, 445, 455, 463, 470, 476, 506,
];
const K3: [i32; 16] = [-441, -387, -333, -279, -225, -171, -117, -63, -9, 45, 98, 152, 206, 260, 314, 368];
const K4: [i32; 16] = [-328, -273, -217, -161, -106, -50, 5, 61, 116, 172, 228, 283, 339, 394, 450, 506];
const K5: [i32; 16] = [-328, -282, -235, -189, -142, -96, -50, -3, 43, 90, 136, 182, 229, 275, 322, 368];
const K6: [i32; 16] = [-256, -212, -168, -123, -79, -35, 10, 54, 98, 143, 187, 232, 276, 320, 365, 409];
const K7: [i32; 16] = [-308, -260, -212, -164, -117, -69, -21, 27, 75, 122, 170, 218, 266, 314, 361, 409];
const K8: [i32; 8] = [-256, -161, -66, 29, 124, 219, 314, 409];
const K9: [i32; 8] = [-256, -176, -96, -15, 65, 146, 226, 307];
const K10: [i32; 8] = [-205, -132, -59, 14, 87, 160, 234, 307];
const CHIRP: [i8; 52] = [
    0x00, 0x03, 0x0f, 0x28, 0x4c, 0x6c, 0x71, 0x50, 0x25, 0x26, 0x4c, 0x44, 0x1a, 0x32, 0x3b, 0x13,
    0x37, 0x1a, 0x25, 0x1f, 0x1d, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];
const INTERP: [u32; 8] = [0, 3, 3, 3, 2, 2, 1, 1];

fn ktable(k: usize, idx: u8) -> i32 {
    let i = idx as usize;
    match k { 0 => K1[i], 1 => K2[i], 2 => K3[i], 3 => K4[i], 4 => K5[i], 5 => K6[i],
              6 => K7[i], 7 => K8[i], 8 => K9[i], _ => K10[i] }
}

fn matrix_multiply(mut a: i32, mut b: i32) -> i32 {
    while a > 511 { a -= 1024; }
    while a < -512 { a += 1024; }
    while b > 16383 { b -= 32768; }
    while b < -16384 { b += 32768; }
    (a * b) >> 9
}

fn clip_analog(mut c: i32) -> i16 {
    c = c.clamp(-2048, 2047);
    c &= !0xF;
    ((c << 4) | ((c & 0x7F0) >> 3) | ((c & 0x400) >> 10)) as i16
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Tms5220 {
    fifo: [u8; FIFO_SIZE],
    fifo_head: usize,
    fifo_tail: usize,
    fifo_count: usize,
    fifo_bits_taken: u32,
    previous_talk_status: bool,
    spen: bool,
    ddis: bool,
    talk: bool,
    talkd: bool,
    buffer_low: bool,
    buffer_empty: bool,
    irq_pin: bool,
    command_register: u8,
    data_latched: bool,
    olde: bool,
    oldp: bool,
    new_energy_idx: u8,
    new_pitch_idx: u8,
    new_k_idx: [u8; 10],
    current_energy: i16,
    current_pitch: i16,
    current_k: [i16; 10],
    previous_energy: u16,
    subcycle: u8,
    subc_reload: u8,
    pc: u8,
    ip: u8,
    inhibit: bool,
    uv_zpar: bool,
    zpar: bool,
    pitch_zero: bool,
    c_variant_rate: u8,
    pitch_count: u16,
    u: [i32; 11],
    x: [i32; 10],
    rng: u16,
    excitation: i16,
    rdb_flag: bool,
    read_byte_register: u8,
    io_ready: bool,
    rs_ws: u8,
    write_latch: u8,
    /// Chip clocks until the pending ready cycle completes (MAME m_timer_io_ready).
    io_timer: Option<u32>,
    sample_clock: u32,
}

impl Default for Tms5220 { fn default() -> Self { Self::new() } }

impl Tms5220 {
    pub fn new() -> Self {
        let mut t = Self {
            fifo: [0; FIFO_SIZE], fifo_head: 0, fifo_tail: 0, fifo_count: 0, fifo_bits_taken: 0,
            previous_talk_status: false, spen: false, ddis: false, talk: false, talkd: false,
            buffer_low: true, buffer_empty: true, irq_pin: false, command_register: NOCOMMAND,
            data_latched: false, olde: true, oldp: true, new_energy_idx: 0, new_pitch_idx: 0,
            new_k_idx: [0; 10], current_energy: 0, current_pitch: 0, current_k: [0; 10],
            previous_energy: 0, subcycle: 0, subc_reload: 1, pc: 0, ip: 0, inhibit: true,
            uv_zpar: false, zpar: false, pitch_zero: false, c_variant_rate: 0, pitch_count: 0,
            u: [0; 11], x: [0; 10], rng: 0x1FFF, excitation: 0, rdb_flag: false, read_byte_register: 0,
            io_ready: true, rs_ws: 0x03, write_latch: 0, io_timer: None, sample_clock: 0,
        };
        t.reset();
        t
    }

    /// MAME device_reset (the pins' state survives).
    pub fn reset(&mut self) {
        self.fifo = [0; FIFO_SIZE];
        self.fifo_head = 0; self.fifo_tail = 0; self.fifo_count = 0; self.fifo_bits_taken = 0;
        self.spen = false; self.ddis = false; self.talk = false; self.talkd = false;
        self.previous_talk_status = false;
        self.irq_pin = false;
        self.buffer_empty = true; self.buffer_low = true;
        self.command_register = NOCOMMAND;
        self.data_latched = false;
        self.rdb_flag = false;
        self.new_energy_idx = 0; self.current_energy = 0; self.previous_energy = 0;
        self.new_pitch_idx = 0; self.current_pitch = 0;
        self.zpar = false; self.uv_zpar = false;
        self.new_k_idx = [0; 10];
        self.current_k = [0; 10];
        self.inhibit = true;
        self.subcycle = 0; self.c_variant_rate = 0; self.pitch_count = 0; self.pc = 0;
        self.subc_reload = 1;
        self.olde = true; self.oldp = true;
        self.ip = RELOAD_TABLE[0];
        self.rng = 0x1FFF;
        self.u = [0; 11];
        self.x = [0; 10];
    }

    fn talk_status(&self) -> bool { self.spen || self.talkd }

    /// /READY pin, active low: true = not ready.
    pub fn readyq(&self) -> bool { !self.io_ready }

    pub fn data_w(&mut self, data: u8) {
        self.write_latch = data;
        self.data_latched = true;
    }

    /// /WS pin (active low). /RS is tied high on Atari System 2.
    pub fn wsq_w(&mut self, state: bool) {
        let new_val = (self.rs_ws & 0x02) | state as u8;
        if new_val == self.rs_ws { return; }
        self.rs_ws = new_val;
        if new_val == 0 { self.reset(); return; }      // 5220C: /RS and /WS both low
        if new_val == 3 { return; }                      // high impedance
        if !state {
            self.io_ready = false;
            self.io_timer = Some(16);
        }
    }

    fn set_io_ready(&mut self) {
        match self.rs_ws {
            0x02 => {
                if self.fifo_count >= FIFO_SIZE && self.ddis {
                    self.io_timer = Some(16);
                } else if self.command_register == NOCOMMAND {
                    self.data_latched = false;
                    let d = self.write_latch;
                    self.data_write(d);
                }
            }
            _ => self.io_ready = true,
        }
    }

    fn new_frame_k_defaults(&mut self) {
        self.new_energy_idx = 0;
        self.new_pitch_idx = 0;
        for i in 0..4 { self.new_k_idx[i] = 0; }
        for i in 4..7 { self.new_k_idx[i] = 0xF; }
        for i in 7..10 { self.new_k_idx[i] = 0x7; }
    }

    fn data_write(&mut self, data: u8) {
        let old_buffer_low = self.buffer_low;
        if self.ddis {
            if self.fifo_count < FIFO_SIZE {
                self.fifo[self.fifo_tail] = data;
                self.fifo_tail = (self.fifo_tail + 1) % FIFO_SIZE;
                self.fifo_count += 1;
                self.update_fifo_status_and_ints();
                if !self.spen && old_buffer_low && !self.buffer_low {
                    self.zpar = true;
                    self.uv_zpar = true;
                    self.olde = true;
                    self.oldp = true;
                    self.spen = true;
                    self.talk = true;      // MAME FAST_START_HACK
                    self.new_frame_k_defaults();
                }
            }
            self.data_latched = false;
        } else {
            self.process_command(data);
        }
        if !self.data_latched { self.io_ready = true; }
    }

    fn update_fifo_status_and_ints(&mut self) {
        if self.fifo_count <= 8 {
            if !self.buffer_low { self.buffer_low = true; self.irq_pin = true; }
        } else {
            self.buffer_low = false;
        }
        if self.fifo_count == 0 {
            if !self.buffer_empty { self.buffer_empty = true; self.irq_pin = true; }
            if self.ddis { self.talk = false; self.spen = false; }
        } else {
            self.buffer_empty = false;
        }
        if self.previous_talk_status && !self.talk_status() {
            self.irq_pin = true;
            self.ddis = false;
            self.previous_talk_status = false;
            if self.command_register != NOCOMMAND {
                let c = self.command_register;
                self.process_command(c);
                if self.data_latched { self.io_timer = Some(16); } else { self.io_ready = true; }
            }
        }
        self.previous_talk_status = self.talk_status();
    }

    fn read_bits(&mut self, mut count: u32) -> u32 {
        if !self.ddis { return (1 << count) - 1; }   // nothing on the VSM bus: reads float high
        let mut val = 0u32;
        while count > 0 {
            count -= 1;
            val = (val << 1) | ((self.fifo[self.fifo_head] >> self.fifo_bits_taken) & 1) as u32;
            self.fifo_bits_taken += 1;
            if self.fifo_bits_taken >= 8 {
                self.fifo_count -= 1;
                self.fifo[self.fifo_head] = 0;
                self.fifo_head = (self.fifo_head + 1) % FIFO_SIZE;
                self.fifo_bits_taken = 0;
                self.update_fifo_status_and_ints();
            }
        }
        val
    }

    fn process_command(&mut self, cmd: u8) {
        self.command_register = cmd;
        match cmd & 0x70 {
            0x10 => { // read byte
                if !self.talk_status() {
                    self.read_byte_register = self.read_bits(8) as u8;
                    self.rdb_flag = true;
                    self.command_register = NOCOMMAND;
                }
            }
            0x00 | 0x20 => { // set rate (5220C)
                self.c_variant_rate = cmd & 0x0F;
                self.command_register = NOCOMMAND;
            }
            0x30 => { // read and branch
                if !self.talk_status() {
                    self.rdb_flag = false;
                    self.command_register = NOCOMMAND;
                }
            }
            0x40 => { // load address
                if !self.talk_status() { self.command_register = NOCOMMAND; }
            }
            0x50 => { // speak (from the VSM)
                self.spen = true;
                self.talk = true;
                self.ddis = false;
                self.zpar = true; self.uv_zpar = true;
                self.olde = true; self.oldp = true;
                self.new_frame_k_defaults();
                self.command_register = NOCOMMAND;
            }
            0x60 => { // speak external
                self.fifo = [0; FIFO_SIZE];
                self.fifo_head = 0; self.fifo_tail = 0; self.fifo_count = 0; self.fifo_bits_taken = 0;
                self.ddis = true;
                self.zpar = true; self.uv_zpar = true;
                self.olde = true; self.oldp = true;
                self.new_frame_k_defaults();
                self.rdb_flag = false;
                self.command_register = NOCOMMAND;
            }
            _ => { // 0x70 reset
                self.reset();
                self.command_register = NOCOMMAND;
            }
        }
        self.update_fifo_status_and_ints();
    }

    fn parse_frame(&mut self) {
        self.uv_zpar = false;
        self.zpar = false;
        if self.c_variant_rate & 0x04 != 0 {
            let i = self.read_bits(2);
            self.ip = RELOAD_TABLE[i as usize];
        } else {
            self.ip = RELOAD_TABLE[(self.c_variant_rate & 3) as usize];
        }
        self.update_fifo_status_and_ints();
        if self.ddis && self.buffer_empty { return; }
        self.new_energy_idx = self.read_bits(4) as u8;
        self.update_fifo_status_and_ints();
        if self.ddis && self.buffer_empty { return; }
        if self.new_energy_idx == 0 || self.new_energy_idx == 15 { return; }
        let rep = self.read_bits(1);
        self.new_pitch_idx = self.read_bits(6) as u8;
        self.uv_zpar = self.new_pitch_idx == 0;
        self.update_fifo_status_and_ints();
        if self.ddis && self.buffer_empty { return; }
        if rep != 0 { return; }
        for i in 0..4 {
            self.new_k_idx[i] = self.read_bits(KBITS[i]) as u8;
            self.update_fifo_status_and_ints();
            if self.ddis && self.buffer_empty { return; }
        }
        if self.new_pitch_idx == 0 { return; }
        for i in 4..10 {
            self.new_k_idx[i] = self.read_bits(KBITS[i]) as u8;
            self.update_fifo_status_and_ints();
            if self.ddis && self.buffer_empty { return; }
        }
    }

    fn lattice_filter(&mut self) -> i32 {
        let k = |c: &Self, i: usize| c.current_k[i] as i32;
        self.u[10] = matrix_multiply(self.previous_energy as i32, (self.excitation as i32) << 6);
        for i in (0..10).rev() {
            self.u[i] = self.u[i + 1] - matrix_multiply(k(self, i), self.x[i]);
        }
        for i in (1..10).rev() {
            self.x[i] = self.x[i - 1] + matrix_multiply(k(self, i - 1), self.u[i - 1]);
        }
        self.x[0] = self.u[0];
        self.previous_energy = self.current_energy as u16;
        self.u[0]
    }

    /// One output sample (MAME process(), one iteration).
    fn sample(&mut self) -> i16 {
        let out;
        if self.talkd {
            if self.ip == 0 && self.pc == 12 && self.subcycle == 1 {
                self.ip = RELOAD_TABLE[(self.c_variant_rate & 3) as usize];
                self.parse_frame();
                if self.new_energy_idx == 0x0F {
                    self.talk = false;
                    self.spen = false;
                    self.update_fifo_status_and_ints();
                }
                let new_unvoiced = self.new_pitch_idx == 0;
                let new_silence = self.new_energy_idx == 0;
                self.inhibit = (!self.oldp && new_unvoiced) || (self.oldp && !new_unvoiced)
                    || (self.olde && !new_silence) || (self.oldp && new_silence);
            } else {
                let inhibit_state = self.inhibit && self.ip != 0;
                let keep = if inhibit_state { 0 } else { 1 };
                let sh = INTERP[self.ip as usize];
                let zp = if self.zpar { 0 } else { 1 };
                let uzp = if self.uv_zpar { 0 } else { 1 };
                if self.subcycle == 2 {
                    match self.pc {
                        0 => {
                            if self.ip == 0 { self.pitch_zero = false; }
                            let c = self.current_energy as i32;
                            self.current_energy = ((c + (((ENERGY[self.new_energy_idx as usize] - c) * keep) >> sh)) * zp) as i16;
                        }
                        1 => {
                            let c = self.current_pitch as i32;
                            self.current_pitch = ((c + (((PITCH[self.new_pitch_idx as usize] - c) * keep) >> sh)) * zp) as i16;
                        }
                        2..=11 => {
                            let i = (self.pc - 2) as usize;
                            let c = self.current_k[i] as i32;
                            let z = if i < 4 { zp } else { uzp };
                            self.current_k[i] = ((c + (((ktable(i, self.new_k_idx[i]) - c) * keep) >> sh)) * z) as i16;
                        }
                        _ => {}
                    }
                }
            }

            self.excitation = if self.oldp {
                if self.rng & 1 != 0 { !0x3F } else { 0x40 }
            } else if self.pitch_count >= 51 {
                CHIRP[51] as i16
            } else {
                CHIRP[self.pitch_count as usize] as i16
            };

            for _ in 0..20 {
                let r = self.rng;
                let bit = ((r >> 12) ^ (r >> 3) ^ (r >> 2) ^ r) & 1;
                self.rng = (r << 1) | bit;
            }

            let mut s = self.lattice_filter();
            while s > 16383 { s -= 32768; }
            while s < -16384 { s += 32768; }
            out = clip_analog(s as i16 as i32);

            self.subcycle += 1;
            if self.subcycle == 2 && self.pc == 12 {
                if self.ip == 7 && self.inhibit { self.pitch_zero = true; }
                if self.ip == 7 {
                    self.olde = self.new_energy_idx == 0;
                    self.oldp = self.new_pitch_idx == 0;
                    self.talkd = self.talk;
                    self.update_fifo_status_and_ints();
                    if !self.talk && self.spen { self.talk = true; }
                }
                self.subcycle = self.subc_reload;
                self.pc = 0;
                self.ip = (self.ip + 1) & 7;
            } else if self.subcycle == 3 {
                self.subcycle = self.subc_reload;
                self.pc += 1;
            }
            self.pitch_count = self.pitch_count.wrapping_add(1);
            if self.pitch_count as i32 >= self.current_pitch as i32 || self.pitch_zero { self.pitch_count = 0; }
            self.pitch_count &= 0x1FF;
        } else {
            self.subcycle += 1;
            if self.subcycle == 2 && self.pc == 12 {
                if self.ip == 7 {
                    self.talkd = self.talk;
                    self.update_fifo_status_and_ints();
                    if !self.talk && self.spen { self.talk = true; }
                }
                self.subcycle = self.subc_reload;
                self.pc = 0;
                self.ip = (self.ip + 1) & 7;
            } else if self.subcycle == 3 {
                self.subcycle = self.subc_reload;
                self.pc += 1;
            }
            out = -1;
        }
        out
    }

    /// One chip clock. Returns a sample (-32768..32767 scale) every 80 clocks.
    pub fn clock(&mut self) -> Option<i16> {
        if let Some(t) = self.io_timer {
            if t <= 1 { self.io_timer = None; self.set_io_ready(); } else { self.io_timer = Some(t - 1); }
        }
        self.sample_clock += 1;
        if self.sample_clock >= 80 {
            self.sample_clock = 0;
            return Some(self.sample());
        }
        None
    }

    pub fn is_talking(&self) -> bool { self.talk_status() }
    /// Speak External mode (FIFO accepting speech data).
    pub fn in_speak_external(&self) -> bool { self.ddis }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Script + hashes from MAME's tms5220.cpp (tools/tms5220_ref):
    /// `w <byte>` write through /WS, `c <n>` run n clocks, `h <hash> <samples>` check.
    fn run_vectors(text: &str) -> usize {
        let mut t = Tms5220::new();
        let mut h: u64 = 0xcbf29ce484222325;
        let mut nsamp = 0usize;
        let mut checks = 0;
        let mut run = |t: &mut Tms5220, h: &mut u64, nsamp: &mut usize| {
            if let Some(s) = t.clock() { *h = (*h ^ (s as u16 as u64)).wrapping_mul(0x100000001b3); *nsamp += 1; }
        };
        for (lineno, line) in text.lines().enumerate() {
            let p: Vec<&str> = line.split_whitespace().collect();
            match p.first() {
                Some(&"w") => {
                    t.data_w(u8::from_str_radix(p[1], 16).unwrap());
                    t.wsq_w(false);
                    while t.readyq() { run(&mut t, &mut h, &mut nsamp); }
                    t.wsq_w(true);
                }
                Some(&"c") => { for _ in 0..p[1].parse::<usize>().unwrap() { run(&mut t, &mut h, &mut nsamp); } }
                Some(&"h") => {
                    assert_eq!((h, nsamp), (u64::from_str_radix(p[1], 16).unwrap(), p[2].parse().unwrap()),
                        "line {}", lineno + 1);
                    checks += 1;
                }
                _ => {}
            }
        }
        checks
    }

    #[test]
    fn tms5220_matches_mame_sample() {
        assert!(run_vectors(include_str!("../tests/data/tms5220_vectors.txt")) > 100);
    }

    /// TMS5220_VECTORS=<file> cargo test --release --lib tms5220_matches_mame_full -- --ignored
    #[test]
    #[ignore]
    fn tms5220_matches_mame_full() {
        let path = std::env::var("TMS5220_VECTORS").expect("TMS5220_VECTORS=<file>");
        println!("{} checks ok", run_vectors(&std::fs::read_to_string(path).unwrap()));
    }

    fn write(t: &mut Tms5220, v: u8) {
        t.data_w(v);
        t.wsq_w(false);
        let mut n = 0;
        while t.readyq() { t.clock(); n += 1; assert!(n < 100_000, "never ready"); }
        t.wsq_w(true);
    }

    #[test]
    fn speak_external_plays_and_stops() {
        let mut t = Tms5220::new();
        write(&mut t, 0x60);                      // Speak External
        assert!(!t.is_talking());
        // frames: energy 10, not repeat, pitch 20, k1..k10 mid values; packed LSB first per byte
        let mut bits: Vec<u8> = vec![];
        let mut push = |v: u32, n: u32| for i in (0..n).rev() { bits.push(((v >> i) & 1) as u8); };
        for _ in 0..20 {
            push(10, 4); push(0, 1); push(20, 6);
            for &kb in &KBITS { push(1 << (kb - 1), kb); }
        }
        push(15, 4);                               // stop frame
        let bytes: Vec<u8> = bits.chunks(8).map(|c| c.iter().enumerate().fold(0u8, |a, (i, &b)| a | (b << i))).collect();
        let mut out = vec![];
        let mut fed = 0;
        for _ in 0..2_000_000 {
            if fed < bytes.len() && t.fifo_count < 15 && !t.readyq() { write(&mut t, bytes[fed]); fed += 1; }
            if let Some(s) = t.clock() { out.push(s); }
            if fed == bytes.len() && !t.is_talking() { break; }
        }
        assert_eq!(fed, bytes.len());
        assert!(!t.is_talking(), "stop frame ends speech");
        let loud = out.iter().filter(|&&s| s.abs() > 256).count();
        assert!(loud > 200, "voiced frames make sound ({loud})");
        for _ in 0..800 { if let Some(s) = t.clock() { out.push(s); } }
        assert_eq!(*out.last().unwrap(), -1, "idle output");
    }
}
