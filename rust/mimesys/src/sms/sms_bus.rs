// SMS / Game Gear memory and I/O.
//
// Memory:
//   0000-BFFF  cartridge through the mapper (see sms_mapper.rs)
//   C000-DFFF  8 KB RAM, mirrored at E000-FFFF
//   FFFC-FFFF  Sega mapper registers (writes also land in RAM)
//
// I/O: only A7, A6 and A0 are decoded:
//   00-3F  W even: memory control (BIOS/cart enables; no BIOS here), W odd: I/O control (TH pins)
//   40-7F  R even: V counter, R odd: H counter, W: SN76489
//   80-BF  even: VDP data, odd: VDP control / status
//   C0-FF  R even: port DC (controller 1 + half of 2), R odd: port DD (rest of 2, reset, TH)
// Game Gear adds ports 00-06 in front of that:
//   00 R: bit 7 = START (0 = pressed), bit 6 = 1 export console
//   01-05 link cable (idle values), 06 W: PSG stereo
//
// Port DC (active low): 0 up, 1 down, 2 left, 3 right, 4 button 1, 5 button 2, 6-7 pad 2 up/down
// Port DD (active low): 0-3 pad 2 left/right/1/2, 4 RESET, 6 TH A, 7 TH B

use crate::common::cpu::z80::Bus;
use crate::common::audio::sn76489::{Sn76489, Sn76489Variant};
use crate::sms::sms_mapper::SmsMapper;
use crate::sms::sms_vdp::SmsVdp;

pub const CPU_CLOCK: u32 = 3_579_545;

pub struct SmsBus {
    pub game_gear: bool,
    pub mapper: SmsMapper,
    pub ram: Vec<u8>,
    pub vdp: SmsVdp,
    pub psg: Sn76489,
    pub io_control: u8,
    pub port_dc: u8,
    pub port_dd: u8,
    pub gg_start: bool,
    pub current_line: usize,
}

impl SmsBus {
    pub fn new(rom: Vec<u8>, game_gear: bool, out_rate: u32) -> Self {
        Self {
            game_gear,
            mapper: SmsMapper::new(rom),
            ram: vec![0; 0x2000],
            vdp: SmsVdp::new(game_gear),
            psg: Sn76489::new(Sn76489Variant::Sega, CPU_CLOCK, out_rate),
            io_control: 0xFF,
            port_dc: 0xFF,
            port_dd: 0xFF,
            gg_start: false,
            current_line: 0,
        }
    }

    /// Port DD with the TH bits: when a TH pin is set as an output (I/O control bit 1 / 3
    /// = 0), it reads back the level written (bit 5 / 7). That's how games tell an export
    /// console from a Japanese one; we behave like an export console.
    fn read_dd(&self) -> u8 {
        let mut v = self.port_dd | 0xC0;
        if self.io_control & 0x02 == 0 && self.io_control & 0x20 == 0 { v &= !0x40; }
        if self.io_control & 0x08 == 0 && self.io_control & 0x80 == 0 { v &= !0x80; }
        v
    }
}

impl Bus for SmsBus {
    fn read(&mut self, addr: u16) -> u8 {
        if addr < 0xC000 { self.mapper.read(addr) } else { self.ram[(addr & 0x1FFF) as usize] }
    }

    fn write(&mut self, addr: u16, val: u8) {
        if addr < 0xC000 {
            self.mapper.write(addr, val);
            return;
        }
        self.ram[(addr & 0x1FFF) as usize] = val;
        if addr >= 0xFFFC { self.mapper.write_register(addr, val); }
    }

    fn port_in(&mut self, port: u16) -> u8 {
        let p = port as u8;
        if self.game_gear && p < 0x07 {
            return match p {
                0 => if self.gg_start { 0x40 } else { 0xC0 },
                1 => 0x7F,
                2 => 0xFF,
                3 => 0x00,
                4 => 0xFF,
                5 => 0x00,
                _ => 0xFF,
            };
        }
        match p & 0xC1 {
            0x40 => SmsVdp::v_counter(self.current_line),
            0x41 => 0x00,   // H counter: only used by light gun games
            0x80 => self.vdp.read_data(),
            0x81 => self.vdp.read_status(),
            0xC0 => self.port_dc,
            0xC1 => self.read_dd(),
            _ => 0xFF,
        }
    }

    fn port_out(&mut self, port: u16, val: u8) {
        let p = port as u8;
        if self.game_gear && p < 0x07 {
            if p == 6 { self.psg.stereo = val; }
            return;
        }
        match p & 0xC1 {
            0x00 => {}                          // memory control (BIOS / cartridge slot enables)
            0x01 => self.io_control = val,
            0x40 | 0x41 => self.psg.write(val),
            0x80 => self.vdp.write_data(val),
            0x81 => self.vdp.write_control(val),
            _ => {}
        }
    }
}
