// ColecoVision memory and I/O.
//
// Memory:
//   0000-1FFF  BIOS (8 KB, "coleco.rom" / "colecovision.rom")
//   2000-5FFF  expansion port (nothing connected: reads 0xFF)
//   6000-7FFF  1 KB RAM, mirrored 8 times
//   8000-FFFF  cartridge (up to 32 KB)
//
// Bigger games use a "MegaCart": 8000-BFFF is fixed to the LAST 16 KB bank of the ROM,
// C000-FFFF is switchable. Touching FFC0-FFFF selects bank (address & 0x3F).
//
// I/O (only the low 8 address bits matter, decoded in 32-port blocks):
//   80-9F  W: controllers to keypad mode
//   A0-BF  R/W: VDP. Even = data, odd = control (write) / status (read)
//   C0-DF  W: controllers to joystick mode
//   E0-FF  W: SN76489 sound.  R: controller (A1 = 0: player 1, A1 = 1: player 2)
//
// Controller bytes are active low:
//   joystick mode: bit 0 up, 1 right, 2 down, 3 left, 6 left fire
//   keypad mode:   bits 0-3 key code (0xF = none), bit 6 right fire
//
// The VDP's interrupt pin drives the Z80's NMI.

use serde::{Deserialize, Serialize};
use crate::common::cpu::z80::Bus;
use crate::common::video::tms9918::Tms9918;
use crate::common::audio::sn76489::{Sn76489, Sn76489Variant};

pub const CPU_CLOCK: u32 = 3_579_545;

/// Key code the controller puts on bits 0-3, for keys 0-9, *, # (from the controller's wiring).
const KEY_CODES: [u8; 12] = [0x0A, 0x0D, 0x07, 0x0C, 0x02, 0x03, 0x0E, 0x05, 0x01, 0x0B, 0x06, 0x09];

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pad {
    pub up: bool, pub down: bool, pub left: bool, pub right: bool,
    pub fire_left: bool,     // the main button
    pub fire_right: bool,
    pub key: Option<u8>,     // 0-9 digits, 10 = *, 11 = #
}

impl Pad {
    fn joystick_byte(&self) -> u8 {
        let mut v = 0x7F;
        if self.up { v &= !0x01; }
        if self.right { v &= !0x02; }
        if self.down { v &= !0x04; }
        if self.left { v &= !0x08; }
        if self.fire_left { v &= !0x40; }
        v
    }

    fn keypad_byte(&self) -> u8 {
        let code = self.key.map(|k| KEY_CODES[k as usize % 12]).unwrap_or(0x0F);
        let mut v = 0x70 | code;
        if self.fire_right { v &= !0x40; }
        v
    }
}

pub struct ColecoBus {
    bios: Vec<u8>,
    pub ram: Vec<u8>,
    cart: Vec<u8>,
    megacart: bool,
    pub bank: usize,          // MegaCart bank at C000-FFFF
    pub vdp: Tms9918,
    pub psg: Sn76489,
    pub keypad_mode: bool,
    pub pads: [Pad; 2],
}

impl ColecoBus {
    pub fn new(bios: Vec<u8>, cart: Vec<u8>, out_rate: u32) -> Self {
        let megacart = cart.len() > 0x8000;
        Self {
            bios,
            ram: vec![0; 0x400],
            megacart,
            bank: 0,
            cart,
            vdp: Tms9918::new(),
            psg: Sn76489::new(Sn76489Variant::Ti, CPU_CLOCK, out_rate),
            keypad_mode: false,
            pads: [Pad::default(); 2],
        }
    }

    pub fn cart_len(&self) -> usize { self.cart.len() }

    fn banks(&self) -> usize { (self.cart.len() / 0x4000).max(1) }

    fn cart_read(&mut self, addr: u16) -> u8 {
        if !self.megacart {
            return self.cart.get((addr - 0x8000) as usize).copied().unwrap_or(0xFF);
        }
        if addr >= 0xFFC0 {
            self.bank = (addr & 0x3F) as usize % self.banks();
        }
        let bank = if addr < 0xC000 { self.banks() - 1 } else { self.bank };
        self.cart.get(bank * 0x4000 + (addr & 0x3FFF) as usize).copied().unwrap_or(0xFF)
    }
}

impl Bus for ColecoBus {
    fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x1FFF => self.bios.get(addr as usize).copied().unwrap_or(0xFF),
            0x2000..=0x5FFF => 0xFF,
            0x6000..=0x7FFF => self.ram[(addr & 0x3FF) as usize],
            _ => self.cart_read(addr),
        }
    }

    fn write(&mut self, addr: u16, val: u8) {
        match addr {
            0x6000..=0x7FFF => self.ram[(addr & 0x3FF) as usize] = val,
            0xFFC0..=0xFFFF if self.megacart => self.bank = (addr & 0x3F) as usize % self.banks(),
            _ => {}
        }
    }

    fn port_in(&mut self, port: u16) -> u8 {
        let p = port as u8;
        match p & 0xE0 {
            0xA0 => if p & 1 == 0 { self.vdp.read_data() } else { self.vdp.read_status() },
            0xE0 => {
                let pad = &self.pads[((p >> 1) & 1) as usize];
                if self.keypad_mode { pad.keypad_byte() } else { pad.joystick_byte() }
            }
            _ => 0xFF,
        }
    }

    fn port_out(&mut self, port: u16, val: u8) {
        let p = port as u8;
        match p & 0xE0 {
            0x80 => self.keypad_mode = true,
            0xA0 => if p & 1 == 0 { self.vdp.write_data(val) } else { self.vdp.write_control(val) },
            0xC0 => self.keypad_mode = false,
            0xE0 => self.psg.write(val),
            _ => {}
        }
    }
}
