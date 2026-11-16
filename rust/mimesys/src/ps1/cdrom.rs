//! The CD-ROM controller (1F801800-1F801803), from Nocash PSX-SPX. This first part has the
//! register interface and the commands the BIOS sends with no disc in the drive; reading
//! discs is stage 5 of the PS1 plan.
//!
//! - 1F801800: index (bits 0-1) and status (parameter / response / data FIFO states, busy).
//! - Commands answer with INT3 (first response), some with a second INT2 / INT5 once the
//!   first is acknowledged. Responses arrive after a delay, as on the drive.
//! - No disc: the status byte says the shell (lid) is open; GetID answers INT5 08 40.

use serde::{Deserialize, Serialize};

const STAT_SHELL_OPEN: u8 = 0x10;
const FIRST_DELAY: i32 = 25_000;
const SECOND_DELAY: i32 = 100_000;

#[derive(Clone, Serialize, Deserialize)]
struct Response {
    delay: i32,
    int: u8,
    bytes: Vec<u8>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Cdrom {
    index: u8,
    params: Vec<u8>,
    response: Vec<u8>,
    resp_pos: usize,
    int_enable: u8,
    int_flag: u8,
    queue: Vec<Response>,
    stat: u8,
    pub irq: bool,
    #[serde(skip)]
    logged: u32,
}

impl Cdrom {
    pub fn new() -> Self { Self { int_enable: 0x1F, stat: STAT_SHELL_OPEN, ..Default::default() } }

    pub fn read(&mut self, a: u32) -> u8 {
        match a & 3 {
            0 => {
                let mut s = self.index;
                if self.params.is_empty() { s |= 0x08; }
                if self.params.len() < 16 { s |= 0x10; }
                if self.resp_pos < self.response.len() { s |= 0x20; }
                if !self.queue.is_empty() && self.int_flag == 0 { s |= 0x80; }
                s
            }
            1 => {
                let v = self.response.get(self.resp_pos).copied().unwrap_or(0);
                if self.resp_pos < self.response.len() { self.resp_pos += 1; }
                v
            }
            2 => 0,                                           // data FIFO: no disc
            _ => match self.index & 1 {
                0 => self.int_enable | 0xE0,
                _ => self.int_flag | 0xE0,
            },
        }
    }

    pub fn write(&mut self, a: u32, v: u8) {
        match (a & 3, self.index) {
            (0, _) => self.index = v & 3,
            (1, 0) => self.command(v),
            (2, 0) => if self.params.len() < 16 { self.params.push(v) },
            (2, 1) => self.int_enable = v & 0x1F,
            (3, 1) => {
                self.int_flag &= !(v & 0x1F);
                if v & 0x40 != 0 { self.params.clear(); }
                if self.int_flag == 0 { self.irq = false; }
            }
            _ => {}                                           // volumes, request register
        }
    }

    fn command(&mut self, c: u8) {
        let s = self.stat;
        // bring-up log: the first 200 commands
        if self.logged < 200 {
            self.logged += 1;
            println!("PS1 CD: command {c:02X} params {:02X?}", self.params);
        }
        let first = |b: Vec<u8>| Response { delay: FIRST_DELAY, int: 3, bytes: b };
        match c {
            0x01 | 0x02 | 0x0B | 0x0C | 0x0D | 0x0E | 0x06 | 0x07 | 0x1B => self.queue.push(first(vec![s])),
            0x0A | 0x09 | 0x08 => {                           // Init, Pause, Stop
                self.queue.push(first(vec![s]));
                self.queue.push(Response { delay: SECOND_DELAY, int: 2, bytes: vec![s] });
            }
            0x19 => {
                let sub = self.params.first().copied().unwrap_or(0);
                let r = match sub {
                    0x20 => vec![0x94, 0x09, 0x19, 0xC0],     // controller date / version
                    _ => vec![s],
                };
                self.queue.push(first(r));
            }
            0x1A => {                                         // GetID: no disc
                self.queue.push(first(vec![s]));
                self.queue.push(Response { delay: SECOND_DELAY, int: 5, bytes: vec![0x08, 0x40, 0, 0, 0, 0, 0, 0] });
            }
            _ => self.queue.push(Response { delay: FIRST_DELAY, int: 5, bytes: vec![s | 1, 0x40] }),
        }
        self.params.clear();
    }

    pub fn tick(&mut self, cycles: u32) {
        if self.int_flag != 0 { return; }                      // waits for the acknowledge
        let Some(r) = self.queue.first_mut() else { return };
        r.delay -= cycles as i32;
        if r.delay > 0 { return; }
        let r = self.queue.remove(0);
        self.int_flag = r.int;
        self.response = r.bytes;
        self.resp_pos = 0;
        if self.int_flag & self.int_enable != 0 { self.irq = true; }
    }
}
