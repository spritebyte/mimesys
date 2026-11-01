//! The DS wifi chip, only as far as games need it to start up: no radio, no packets.
//!
//! ARM7 0x04800000-0x0480FFFF (mirrored every 32 KB): registers 0x000-0xFFF, wifi RAM
//! 0x4000-0x5FFF. Registers read back what was written, except: the chip ID (0x000),
//! the random generator (0x044), the microsecond counter (0x0F8-0x0FE), baseband
//! registers (written / read through W_BB_CNT 0x158), RF registers (W_RF_DATA 0x17C/0x17E)
//! and the power state machine. Powering the transceiver on takes 2048 us, then IRQ 11
//! and W_POWERSTATE = 0, which startup code waits for.
//!
//! The behaviour follows melonDS's Wifi.cpp (register effects, reset values, power logic).

use serde::{Deserialize, Serialize};

const W_MODE_RESET: usize = 0x004;
const W_MODE_WEP: usize = 0x006;
const W_IF: usize = 0x010;
const W_IE: usize = 0x012;
const W_TRX_POWER: usize = 0x034;
const W_POWER_US: usize = 0x036;
const W_POWER_TX: usize = 0x038;
const W_POWER_STATE: usize = 0x03C;
const W_POWER_FORCE: usize = 0x040;
const W_RANDOM: usize = 0x044;
const W_POWER_DOWN_CTRL: usize = 0x048;
const W_US_COUNT_CNT: usize = 0x0E8;
const W_US_COMPARE_CNT: usize = 0x0EA;
const W_US_COUNT0: usize = 0x0F8;
const W_BB_CNT: usize = 0x158;
const W_BB_WRITE: usize = 0x15A;
const W_BB_READ: usize = 0x15C;
const W_BB_BUSY: usize = 0x15E;
const W_RF_DATA2: usize = 0x17C;
const W_RF_DATA1: usize = 0x17E;
const W_RF_BUSY: usize = 0x180;
const W_RF_CNT: usize = 0x184;
const W_RF_PINS: usize = 0x19C;
const W_RF_STATUS: usize = 0x214;
const W_IF_SET: usize = 0x21C;

#[derive(Clone, Serialize, Deserialize)]
pub struct Wifi {
    #[serde(with = "serde_bytes")]
    io: Vec<u8>,
    #[serde(with = "serde_bytes")]
    ram: Vec<u8>,
    #[serde(with = "serde_bytes")]
    bb: Vec<u8>,
    #[serde(with = "serde_bytes")]
    bb_ro: Vec<u8>,
    rf: Vec<u32>,
    rf_type: u8,
    random: u16,
    us_counter: u64,
    us_until_on: i64,
    /// POWCNT2 bit 1
    pub enabled: bool,
    /// bus cycle the microsecond clock was last brought up to date
    last: u64,
    /// wifi IRQ (ARM7 IRQ 24) waiting to be raised
    #[serde(skip)]
    pub irq: bool,
}

const BUS_CLOCK: u64 = 33_513_982;

impl Default for Wifi { fn default() -> Self { Self::new(3, 0x20) } }

impl Wifi {
    /// `rf_type` and `console_type` come from the firmware header (0x40, 0x1D).
    pub fn new(rf_type: u8, console_type: u8) -> Self {
        let mut w = Self {
            io: vec![0; 0x1000], ram: vec![0; 0x2000], bb: vec![0; 0x100], bb_ro: vec![0; 0x100], rf: vec![0; 0x40],
            rf_type, random: 1, us_counter: 0, us_until_on: 0, enabled: false, last: 0, irq: false,
        };
        let mut fixed = |id: usize, v: u8| { w.bb[id] = v; w.bb_ro[id] = 1; };
        fixed(0x00, 0x6D);
        for id in [0x0D, 0x0E, 0x0F, 0x10, 0x11, 0x12, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x27, 0x4D, 0x5E, 0x5F, 0x60, 0x61, 0x66] { fixed(id, 0); }
        fixed(0x5D, 0x01);
        fixed(0x64, 0xFF);
        for id in 0x69..0x100 { fixed(id, 0); }
        // DS: 1440, DS Lite and later: C340
        w.set(0x000, if console_type == 0xFF { 0x1440 } else { 0xC340 });
        for o in (0x018..0x01E).chain(0x020..0x026) { w.io[o] = 0xFF; }
        w.set(W_POWER_US, 1);
        w
    }

    fn get(&self, o: usize) -> u16 { u16::from_le_bytes([self.io[o], self.io[o + 1]]) }
    fn set(&mut self, o: usize, v: u16) { self.io[o..o + 2].copy_from_slice(&v.to_le_bytes()); }

    fn powered(&self) -> bool { self.enabled && self.get(W_POWER_US) & 1 == 0 }

    /// Bring the microsecond clock and the power-on delay up to bus cycle `now`.
    pub fn tick(&mut self, now: u64) {
        let us = |c: u64| c * 1_000_000 / BUS_CLOCK;
        let dt = us(now).saturating_sub(us(self.last)) as i64;
        self.last = now;
        if dt == 0 || !self.powered() { return; }
        if self.us_until_on < 0 {
            self.us_until_on += dt;
            if self.us_until_on >= 0 {
                self.us_until_on = 0;
                self.set(W_POWER_STATE, 0);
                self.set_status(1);
                self.update_power(0);
            }
        }
        if self.get(W_US_COUNT_CNT) & 1 != 0 { self.us_counter = self.us_counter.wrapping_add(dt as u64); }
    }

    fn set_irq(&mut self, n: u32) {
        let old = self.get(W_IF) & self.get(W_IE);
        self.set(W_IF, self.get(W_IF) | (1 << n));
        self.check_irq(old);
    }

    fn check_irq(&mut self, old: u16) {
        if old == 0 && self.get(W_IF) & self.get(W_IE) != 0 { self.irq = true; }
    }

    fn set_status(&mut self, s: usize) {
        const PINS: [u16; 10] = [0x04, 0x84, 0, 0x46, 0, 0x84, 0x87, 0, 0x46, 0x04];
        self.set(W_RF_STATUS, s as u16);
        self.set(W_RF_PINS, PINS[s]);
    }

    /// The transceiver's power: 1 = on, -1 = off, 0 = as the registers say.
    fn update_power(&mut self, mut power: i32) {
        let mut cur = 0;
        if self.get(W_TRX_POWER) == 1 { cur |= 1; }
        if self.get(W_POWER_STATE) & (1 << 9) == 0 { cur |= 2; }
        let mut req = cur;
        if self.get(W_POWER_FORCE) & 0x8000 != 0 {
            req = if self.get(W_POWER_FORCE) & 1 != 0 { 0 } else { 3 };
        } else if self.get(W_MODE_RESET) & 1 == 0 {
            req = 0;
        } else {
            let ps = self.get(W_POWER_STATE);
            if power == 0 {
                if ps & 0x0202 == 0x0202 { power = 1; } else if ps & 0x0201 == 0x0001 { power = -1; }
            }
            if power == -1 && self.get(W_POWER_DOWN_CTRL) & 1 != 0 { power = 0; }
            if power == 1 { req = 3; }
            else if power == -1 { req = if self.get(W_POWER_DOWN_CTRL) != 0 { 3 } else { 0 }; }
            else if self.get(W_POWER_DOWN_CTRL) & 2 != 0 { req = 3; }
        }
        if req == cur { return; }
        if req & 1 != 0 {
            if cur & 1 == 0 { self.set(W_TRX_POWER, 1); self.set_status(1); }
        } else {
            self.set(W_TRX_POWER, 0);
            self.set_status(9);
        }
        if req & 2 != 0 {
            self.set(W_POWER_STATE, self.get(W_POWER_STATE) | (1 << 8));
            if cur & 2 == 0 && self.us_until_on == 0 {
                self.us_until_on = -2048;
                self.set_irq(11);
            }
        } else {
            self.set(W_POWER_STATE, (self.get(W_POWER_STATE) & !0x0101) | 0x0200);
            self.us_until_on = 0;
        }
    }

    pub fn read16(&mut self, addr: u32, now: u64) -> u16 {
        self.tick(now);
        let a = (addr & 0x7FFE) as usize;
        if (0x4000..0x6000).contains(&a) { let i = a & 0x1FFE; return u16::from_le_bytes([self.ram[i], self.ram[i + 1]]); }
        if (0x2000..0x4000).contains(&a) { return 0xFFFF; }
        let o = a & 0xFFF;
        match o {
            W_RANDOM => {
                self.random = (self.random & 1) ^ (((self.random & 0x3FF) << 1) | (self.random >> 10));
                self.random
            }
            0x0F8..=0x0FE => (self.us_counter >> ((o - W_US_COUNT0) * 8)) as u16,
            W_BB_READ => if self.get(W_BB_CNT) & 0xF000 == 0x6000 { self.bb[(self.get(W_BB_CNT) & 0xFF) as usize] as u16 } else { 0 },
            W_BB_BUSY | W_RF_BUSY => 0,
            _ => self.get(o),
        }
    }

    pub fn write16(&mut self, addr: u32, v: u16, now: u64) {
        self.tick(now);
        let a = (addr & 0x7FFE) as usize;
        if (0x4000..0x6000).contains(&a) { let i = a & 0x1FFE; self.ram[i..i + 2].copy_from_slice(&v.to_le_bytes()); return; }
        if (0x2000..0x4000).contains(&a) { return; }
        let o = a & 0xFFF;
        match o {
            W_MODE_RESET => {
                let old = self.get(W_MODE_RESET);
                self.set(W_MODE_RESET, v & 1);
                if old & 1 == 0 && v & 1 != 0 { self.set(0x27C, 5); self.update_power(0); }
                else if old & 1 != 0 && v & 1 == 0 { self.set(0x27C, 0xA); self.update_power(0); }
                if v & 0x2000 != 0 {
                    for r in [0x056, 0x0C0, 0x0C4, 0x1A4] { self.set(r, 0); }
                    self.set(0x278, 0xF);
                }
                if v & 0x4000 != 0 {
                    for r in [W_MODE_WEP, 0x008, 0x00A, 0x018, 0x01A, 0x01C, 0x020, 0x022, 0x024, 0x028, 0x02A, 0x02E, 0x084, 0x194, 0x198] {
                        self.set(r, 0);
                    }
                    for (r, x) in [(0x02C, 0x0707), (0x050, 0x4000), (0x052, 0x4800), (0x0BC, 1), (0x0D0, 0x0401), (0x0D4, 1),
                                   (0x0E0, 8), (0x0EC, 0x3F03), (0x1A2, 1), (0x224, 3), (0x230, 0x47)] {
                        self.set(r, x);
                    }
                }
            }
            W_MODE_WEP => {
                let v = v & 0x7F;
                self.set(W_MODE_WEP, v);
                if self.get(W_POWER_TX) & 2 != 0 {
                    match v & 7 {
                        1 => self.set(W_POWER_DOWN_CTRL, self.get(W_POWER_DOWN_CTRL) | 2),
                        2 => self.set(W_POWER_DOWN_CTRL, 3),
                        _ => {}
                    }
                    if v & 7 != 3 { self.set(W_POWER_STATE, self.get(W_POWER_STATE) & 0x0300); }
                    self.update_power(0);
                }
            }
            W_IE => { let old = self.get(W_IF) & self.get(W_IE); self.set(W_IE, v); self.check_irq(old); }
            W_IF => self.set(W_IF, self.get(W_IF) & !v),
            W_IF_SET => { let old = self.get(W_IF) & self.get(W_IE); self.set(W_IF, self.get(W_IF) | (v & 0xFBFF)); self.check_irq(old); }
            W_POWER_US => self.set(W_POWER_US, v & 3),
            W_POWER_TX => {
                self.set(W_POWER_TX, v & 3);
                if v & 2 != 0 {
                    match self.get(W_MODE_WEP) & 7 {
                        1 => self.set(W_POWER_DOWN_CTRL, self.get(W_POWER_DOWN_CTRL) | 2),
                        2 => self.set(W_POWER_DOWN_CTRL, 3),
                        _ => {}
                    }
                    self.update_power(0);
                }
            }
            W_POWER_STATE => {
                if self.get(W_MODE_WEP) & 7 != 3 { return; }
                let mut v = (self.get(W_POWER_STATE) & 0x0300) | (v & 3);
                if v & 0x0300 == 0x0200 { v &= !1; } else { v &= !2; }
                if v & 0x0200 == 0 { v &= !0x0100; }
                self.set(W_POWER_STATE, v);
                self.update_power(0);
            }
            W_POWER_FORCE => { self.set(W_POWER_FORCE, v & 0x8001); self.update_power(0); }
            W_POWER_DOWN_CTRL => {
                self.set(W_POWER_DOWN_CTRL, v & 3);
                if self.get(W_POWER_TX) & 2 != 0 {
                    match self.get(W_MODE_WEP) & 7 {
                        1 => self.set(W_POWER_DOWN_CTRL, self.get(W_POWER_DOWN_CTRL) | 2),
                        2 => self.set(W_POWER_DOWN_CTRL, 3),
                        _ => {}
                    }
                }
                self.update_power(0);
            }
            W_US_COUNT_CNT => self.set(o, v & 1),
            W_US_COMPARE_CNT => self.set(o, v & 1),
            0x0F8..=0x0FE => {
                let sh = (o - W_US_COUNT0) * 8;
                self.us_counter = (self.us_counter & !(0xFFFFu64 << sh)) | ((v as u64) << sh);
            }
            W_BB_CNT => {
                self.set(W_BB_CNT, v);
                if v & 0xF000 == 0x5000 {
                    let id = (v & 0xFF) as usize;
                    if self.bb_ro[id] == 0 { self.bb[id] = self.get(W_BB_WRITE) as u8; }
                }
            }
            W_RF_DATA2 => {
                self.set(W_RF_DATA2, v);
                if self.rf_type == 3 {
                    let id = ((self.get(W_RF_DATA1) >> 8) & 0x3F) as usize;
                    match v & 0xF {
                        6 => { let d1 = (self.get(W_RF_DATA1) & 0xFF00) | (self.rf[id] & 0xFF) as u16; self.set(W_RF_DATA1, d1); }
                        5 => self.rf[id] = (self.get(W_RF_DATA1) & 0xFF) as u32,
                        _ => {}
                    }
                } else {
                    let id = ((v >> 2) & 0x1F) as usize;
                    if v & 0x80 != 0 {
                        let d = self.rf[id];
                        self.set(W_RF_DATA1, d as u16);
                        self.set(W_RF_DATA2, (v & 0xFFFC) | ((d >> 16) & 3) as u16);
                    } else {
                        self.rf[id] = self.get(W_RF_DATA1) as u32 | ((v as u32 & 3) << 16);
                    }
                }
            }
            W_RF_CNT => self.set(o, v & 0x413F),
            _ => self.set(o, v),
        }
    }
}
