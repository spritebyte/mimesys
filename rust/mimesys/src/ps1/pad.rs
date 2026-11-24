//! PlayStation controllers on SIO0 address 01h: the digital pad and the DualShock
//! (SCPH-1200), from Nocash PSX-SPX's "Controllers" chapters.
//!
//! - Digital mode (LED off, ID 5A41): command 42h answers the two button bytes.
//! - Analog mode (LED red, ID 5A73): the buttons plus L3 / R3, then the sticks: right X / Y,
//!   left X / Y (00h left / up, 80h centre, FFh right / down). The pad powers up digital;
//!   the Analog button or the game (config command 44h) switches.
//! - Config mode (ID 5AF3, 9-byte transfers), entered and left with 43h: 44h sets the LED
//!   (and locks the Analog button), 45h reads it, 46h / 47h / 4Ch constants, 4Dh maps the
//!   rumble motors onto command 42h's parameter bytes.
//! - Rumble: once mapped, a byte mapped 00h drives the small motor (bit 0: on / off) and one
//!   mapped 01h the large motor (00h-FFh). Before any config command, the old one-motor
//!   method works: 42h's bytes 40h-7Fh then odd turn the small motor on.
//! - DualShock 2 (`ds2`, the PS2's pad; ps2tek "SIO2 Controller Commands"): 45h says
//!   model 03h, 40h takes a pressure button's parameters, 41h reads and 4Fh sets the poll mask; a mask that includes the pressure
//!   bytes switches 42h to ID 79h with 12 pressure bytes after the sticks.

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct Pad {
    /// a DualShock (else a digital pad: no analog mode, no config commands, no rumble)
    pub dualshock: bool,
    pub analog: bool,
    /// the game locked the Analog button (44h key 3)
    locked: bool,
    config: bool,
    /// config commands have been used: the old rumble method is off
    configured: bool,
    /// 42h's parameter bytes 3-8 -> motor: 00 small, 01 large, FF none
    rumble_map: [u8; 6],
    pub small: bool,
    pub large: u8,
    /// a DualShock 2: 4Fh's poll mask, and whether it asks for the pressure bytes
    #[serde(default)]
    pub ds2: bool,
    #[serde(default = "full_mask")]
    mask: [u8; 3],
    #[serde(default)]
    pressure: bool,
    // the transfer in progress
    cmd: u8,
    step: usize,
    param: [u8; 8],
    /// buttons (active low, bit 0 Select ... bit 15 Square, with L3 / R3) and the sticks
    /// (right X / Y, left X / Y); the frontend's, not saved
    #[serde(skip)]
    pub buttons: u16,
    #[serde(skip, default = "centred")]
    pub axes: [u8; 4],
    /// DualShock 2 pressures (right, left, up, down, triangle, circle, cross, square, L1,
    /// R1, L2, R2; 00h released .. FFh fully pressed); the frontend's
    #[serde(skip)]
    pub pressures: [u8; 12],
}

fn centred() -> [u8; 4] { [0x80; 4] }
fn full_mask() -> [u8; 3] { [0xFF, 0xFF, 0x03] }

impl Default for Pad {
    fn default() -> Self {
        Self {
            dualshock: true, analog: false, locked: false, config: false, configured: false,
            rumble_map: [0xFF; 6], small: false, large: 0, ds2: false, mask: full_mask(), pressure: false,
            cmd: 0, step: 0, param: [0; 8], buttons: 0xFFFF, axes: centred(), pressures: [0; 12],
        }
    }
}

impl Pad {
    /// The Analog button: digital <-> analog, unless the game locked it. Stops the motors.
    pub fn analog_button(&mut self) {
        if !self.dualshock || self.locked { return; }
        self.analog = !self.analog;
        self.small = false;
        self.large = 0;
    }

    /// Address 01h arrived: a new transfer
    pub fn select(&mut self) {
        self.step = 0;
        self.cmd = 0;
    }

    /// The buttons as sent: L3 / R3 only in analog (or config) mode
    fn button_bytes(&self, with_sticks: bool) -> [u8; 2] {
        let b = if with_sticks { self.buttons } else { self.buttons | 0x0006 };
        [b as u8, (b >> 8) as u8]
    }

    /// One byte after the address: the reply, and whether the pad ACKs (more to come)
    pub fn exchange(&mut self, b: u8) -> (u8, bool) {
        let i = self.step;
        self.step += 1;
        if i == 0 { self.cmd = b; }
        if i < 8 { self.param[i] = b; }
        if !self.dualshock {
            // a digital pad: 41 5A and the buttons, for any command
            let [lo, hi] = self.button_bytes(false);
            return match i { 0 => (0x41, true), 1 => (0x5A, true), 2 => (lo, true), _ => (hi, false) };
        }
        if self.config { return self.config_byte(i, b); }
        // normal mode: 42h (and 43h, which reads the same) answer the buttons and sticks
        // (and a DualShock 2's pressures, for 42h only)
        let press = self.ds2 && self.analog && self.pressure;
        let len = if !self.analog { 4 } else if press && self.cmd == 0x42 { 20 } else { 8 };
        let [lo, hi] = self.button_bytes(self.analog);
        let reply = match i {
            0 => if press { 0x79 } else if self.analog { 0x73 } else { 0x41 },
            1 => 0x5A,
            2 => lo,
            3 => hi,
            4..=7 => self.axes[i - 4],
            8..=19 if press => self.pressures[i - 8],
            _ => 0xFF,
        };
        if self.cmd == 0x42 && (2..8).contains(&i) { self.motor_byte(i - 2, b); }
        if self.cmd == 0x42 && i == 3 && !self.configured {
            // the old method: one motor, from the TAP / MOT bytes
            self.small = self.param[2] & 0xC0 == 0x40 && b & 1 != 0;
        }
        if self.cmd == 0x43 && i == 2 && b == 0x01 {
            self.config = true;
            self.configured = true;
        }
        (reply, i + 1 < len)
    }

    fn motor_byte(&mut self, k: usize, b: u8) {
        match self.rumble_map[k] {
            0x00 => self.small = b & 1 != 0,
            0x01 => self.large = b,
            _ => {}
        }
    }

    fn config_byte(&mut self, i: usize, b: u8) -> (u8, bool) {
        let more = i < 7;
        let p2 = self.param[2];
        let reply = match i {
            0 => 0xF3,
            1 => 0x5A,
            _ => match self.cmd {
                0x42 => {
                    let [lo, hi] = self.button_bytes(true);
                    if (2..8).contains(&i) { self.motor_byte(i - 2, b); }
                    match i { 2 => lo, 3 => hi, _ => self.axes[i - 4] }
                }
                0x43 => {
                    if i == 2 && b == 0x00 { self.config = false; }
                    0
                }
                // 40h: a pressure button's parameters (libpad2's pdman sends one per button
                // and wants the 5Ah)
                0x40 if self.ds2 => match i { 4 => 0x02, 7 => 0x5A, _ => 0 },
                0x41 if self.ds2 => match i {
                    2..=4 if self.analog => self.mask[i - 2],
                    7 => 0x5A,
                    _ => 0,
                },
                0x4F if self.ds2 => {
                    if (2..=4).contains(&i) { self.mask[i - 2] = b; }
                    if i == 4 {
                        // response bytes 6-17 are the pressures
                        self.pressure = self.mask[0] & 0xC0 != 0 || self.mask[1] != 0 || self.mask[2] & 3 != 0;
                    }
                    if i == 7 { 0x5A } else { 0 }
                }
                0x44 => {
                    if i == 2 && b <= 1 {
                        self.analog = b == 1;
                        if !self.analog { self.pressure = false; }
                    }
                    if i == 3 { self.locked = b & 3 == 3; }
                    0
                }
                0x45 => [if self.ds2 { 0x03 } else { 0x01 }, 0x02, self.analog as u8, 0x02, 0x01, 0x00][i - 2],
                0x46 => match (p2, i) {
                    (0, 4) => 0x01, (0, 5) => 0x02, (0, 6) => 0x00, (0, 7) => 0x0A,
                    (1, 4) => 0x01, (1, 5) => 0x01, (1, 6) => 0x01, (1, 7) => 0x14,
                    _ => 0,
                },
                0x47 => [0x00, 0x00, 0x02, 0x00, 0x01, 0x00][i - 2],
                0x48 => if i == 6 && p2 <= 1 { 0x01 } else { 0 },
                0x4C => if i == 5 { match p2 { 0 => 0x04, 1 => 0x07, _ => 0 } } else { 0 },
                0x4D => {
                    let old = self.rumble_map[i - 2];
                    self.rumble_map[i - 2] = b;
                    if i == 7 && self.rumble_map.iter().all(|&m| m == 0xFF) { self.small = false; self.large = 0; }
                    old
                }
                _ => 0,
            },
        };
        (reply, more)
    }
}
