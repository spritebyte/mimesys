//! Suzy: the sprite engine, the math unit, the joypad and the cartridge ports.
//!
//! The sprite engine follows Handy's model (K. Wilkins, zlib license; see
//! THIRD_PARTY_NOTICES): the CPU points SCBNEXT at a chain of sprite control blocks,
//! sets SPRGO and goes to sleep (Mikey's CPUSLEEP); Suzy draws every sprite in the chain
//! into the 160 x 102 x 4-bit buffer at VIDBAS (and a collision buffer at COLLBAS), then
//! wakes the CPU. Each sprite is drawn in up to four quadrants out from its reference
//! point, with its source lines (packed runs or literal pixels, 1-4 bits a pixel) scaled
//! by SPRHSIZ / SPRVSIZ (8.8 fixed point), widened line by line by STRETCH and slanted by
//! TILT, its pens mapped through a 16-entry table.
//!
//! The 16-bit registers at FC00-FC2F are kept as words; writing the low byte clears the
//! high one.
//!
//! Math: AB * CD -> EFGH (signed with SPRSYS bit 7, added into JKLM with bit 6), and
//! EFGH / NP -> ABCD remainder JKLM. Writing A starts a multiply, writing E a divide.
//!
//! JOYSTICK (FCB0), right-handed: bit 7 up, 6 down, 5 left, 4 right, 3 option 1,
//! 2 option 2, 1 inner button (B), 0 outer button (A). With SPRSYS bit 3 ("left hand",
//! the console turned round) up / down and left / right swap. SWITCHES (FCB1): bit 0 pause.

use serde::{Deserialize, Serialize};

pub const SCREEN_W: i32 = 160;
pub const SCREEN_H: i32 = 102;
/// Ticks for one sprite-engine RAM access (Handy's SPR_RDWR_CYC).
const RW: u32 = 3;
const LINE_END: u32 = 0x80;

// word registers (index = (address - FC00) / 2)
const TMPADR: usize = 0x00; const TILTACUM: usize = 0x01; const HOFF: usize = 0x02; const VOFF: usize = 0x03;
const VIDBAS: usize = 0x04; const COLLBAS: usize = 0x05; const SCBNEXT: usize = 0x08; const SPRDLINE: usize = 0x09;
const HPOSSTRT: usize = 0x0A; const VPOSSTRT: usize = 0x0B; const SPRHSIZ: usize = 0x0C; const SPRVSIZ: usize = 0x0D;
const STRETCH: usize = 0x0E; const TILT: usize = 0x0F; const SPRDOFF: usize = 0x10; const COLLOFF: usize = 0x12;
const VSIZACUM: usize = 0x13; const HSIZOFF: usize = 0x14; const VSIZOFF: usize = 0x15; const SCBADR: usize = 0x16;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum LineType { Error, AbsLiteral, Literal, Packed }

#[derive(Clone, Serialize, Deserialize)]
pub struct Suzy {
    /// FC00-FC2F as 24 words
    pub w: [u16; 24],
    pub hsizacum: u16,
    // SPRCTL0 / SPRCTL1 / SPRCOLL as fields
    sprite_type: u8,
    vflip: bool,
    hflip: bool,
    pixel_bits: u32,
    start_left: bool,
    start_up: bool,
    skip: bool,
    reload_palette: bool,
    reload_depth: u8,
    literal: bool,
    coll_number: u8,
    coll_off: bool,
    pub pen: [u8; 16],
    pub busen: bool,
    pub sprgo: bool,
    everon: bool,
    /// SPRSYS bits written (1 stop on current, 3 left hand, 4 v-stretch, 5 no collide,
    /// 6 accumulate, 7 signed math)
    pub sprsys: u8,
    unsafe_access: bool,
    last_carry: bool,
    math_bit: bool,
    // math: ABCD, EFGH, JKLM, NP and the signs
    abcd: u32,
    efgh: u32,
    jklm: u32,
    np: u32,
    ab_sign: i32,
    cd_sign: i32,
    /// buttons as the right-handed JOYSTICK byte
    pub joystick: u8,
    pub switches: u8,
    // line decoder
    collision: u32,
    line_base: u16,
    line_coll: u16,
    shift_reg: u32,
    shift_count: u32,
    repeat: u32,
    line_pixel: u32,
    line_type: LineType,
    bits_left: u32,
    cycles: u32,
}

pub enum SuzyAccess { None, Cart0, Cart1 }

impl Suzy {
    pub fn new() -> Self {
        let mut w = [0u16; 24];
        w[HSIZOFF] = 0x7F;
        w[VSIZOFF] = 0x7F;
        Self {
            w, hsizacum: 0, sprite_type: 0, vflip: false, hflip: false, pixel_bits: 1, start_left: false, start_up: false,
            skip: false, reload_palette: false, reload_depth: 0, literal: false, coll_number: 0, coll_off: false,
            pen: core::array::from_fn(|i| i as u8), busen: false, sprgo: false, everon: false, sprsys: 0,
            unsafe_access: false, last_carry: false, math_bit: false,
            // Stun Runner relies on these power-up values (Handy)
            abcd: 0xFFFF_FFFF, efgh: 0xFFFF_FFFF, jklm: 0xFFFF_FFFF, np: 0xFFFF, ab_sign: 1, cd_sign: 1,
            joystick: 0, switches: 0,
            collision: 0, line_base: 0, line_coll: 0, shift_reg: 0, shift_count: 0, repeat: 0, line_pixel: 0,
            line_type: LineType::Error, bits_left: 0, cycles: 0,
        }
    }

    // ---------------- registers ----------------

    /// A read; cart ports are handled by the caller (`SuzyAccess`).
    pub fn read(&mut self, a: u8) -> (u8, SuzyAccess) {
        let byte = |x: u32, n: u32| (x >> (8 * n)) as u8;
        let v = match a {
            0x00..=0x2F => { let x = self.w[(a >> 1) as usize]; if a & 1 == 0 { x as u8 } else { (x >> 8) as u8 } }
            0x52 => byte(self.abcd, 0), 0x53 => byte(self.abcd, 1), 0x54 => byte(self.abcd, 2), 0x55 => byte(self.abcd, 3),
            0x56 => byte(self.np, 0), 0x57 => byte(self.np, 1),
            0x60 => byte(self.efgh, 0), 0x61 => byte(self.efgh, 1), 0x62 => byte(self.efgh, 2), 0x63 => byte(self.efgh, 3),
            0x6C => byte(self.jklm, 0), 0x6D => byte(self.jklm, 1), 0x6E => byte(self.jklm, 2), 0x6F => byte(self.jklm, 3),
            0x88 => 0x01,                                           // SUZYHREV
            0x92 => {                                               // SPRSYS status (the engine is done
                (self.sprsys & 0x02)                                //  by the time the CPU looks)
                    | (self.unsafe_access as u8) << 2 | (self.sprsys & 0x18)
                    | (self.last_carry as u8) << 5 | (self.math_bit as u8) << 6
            }
            0xB0 => {
                let j = self.joystick;
                if self.sprsys & 0x08 != 0 {
                    (j & 0x0F) | (j & 0x80) >> 1 | (j & 0x40) << 1 | (j & 0x20) >> 1 | (j & 0x10) << 1
                } else { j }
            }
            0xB1 => self.switches,
            0xB2 => return (0, SuzyAccess::Cart0),
            0xB3 => return (0, SuzyAccess::Cart1),
            _ => 0xFF,
        };
        (v, SuzyAccess::None)
    }

    pub fn write(&mut self, a: u8, v: u8) -> SuzyAccess {
        let set = |x: &mut u32, n: u32| *x = (*x & !(0xFF << (8 * n))) | (v as u32) << (8 * n);
        match a {
            0x00..=0x2F => {
                let r = &mut self.w[(a >> 1) as usize];
                *r = if a & 1 == 0 { v as u16 } else { (*r & 0xFF) | (v as u16) << 8 };
            }
            0x52 => { set(&mut self.abcd, 0); self.write(0x53, 0); }    // D (clears C, as Handy)
            0x53 => {
                set(&mut self.abcd, 1);
                if self.sprsys & 0x80 != 0 {
                    let cd = self.abcd as u16;
                    if cd.wrapping_sub(1) & 0x8000 != 0 {
                        self.abcd = (self.abcd & 0xFFFF_0000) | (cd ^ 0xFFFF).wrapping_add(1) as u32;
                        self.cd_sign = -1;
                    } else { self.cd_sign = 1; }
                }
            }
            0x54 => { set(&mut self.abcd, 2); self.abcd &= 0x00FF_FFFF; }   // B clears A
            0x55 => {
                set(&mut self.abcd, 3);
                if self.sprsys & 0x80 != 0 {
                    let ab = (self.abcd >> 16) as u16;
                    if ab.wrapping_sub(1) & 0x8000 != 0 {
                        self.abcd = (self.abcd & 0xFFFF) | ((ab ^ 0xFFFF).wrapping_add(1) as u32) << 16;
                        self.ab_sign = -1;
                    } else { self.ab_sign = 1; }
                }
                self.multiply();
            }
            0x56 => { set(&mut self.np, 0); self.np &= 0xFF; }          // P clears N
            0x57 => set(&mut self.np, 1),
            0x60 => { set(&mut self.efgh, 0); self.efgh &= 0xFFFF_00FF; } // H clears G
            0x61 => set(&mut self.efgh, 1),
            0x62 => { set(&mut self.efgh, 2); self.efgh &= 0x00FF_FFFF; } // F clears E
            0x63 => { set(&mut self.efgh, 3); self.divide(); }
            0x6C => { set(&mut self.jklm, 0); self.jklm &= 0xFFFF_00FF; self.math_bit = false; }
            0x6D => set(&mut self.jklm, 1),
            0x6E => { set(&mut self.jklm, 2); self.jklm &= 0x00FF_FFFF; }
            0x6F => set(&mut self.jklm, 3),
            0x80 => self.set_ctl0(v),
            0x81 => self.set_ctl1(v),
            0x82 => { self.coll_number = v & 0x0F; self.coll_off = v & 0x20 != 0; }
            0x90 => self.busen = v & 1 != 0,
            0x91 => { self.sprgo = v & 1 != 0; self.everon = v & 4 != 0; }
            0x92 => { self.sprsys = v; if v & 0x04 != 0 { self.unsafe_access = false; } }
            0xB2 => return SuzyAccess::Cart0,
            0xB3 => return SuzyAccess::Cart1,
            _ => {}
        }
        SuzyAccess::None
    }

    fn set_ctl0(&mut self, v: u8) {
        self.sprite_type = v & 7;
        self.vflip = v & 0x10 != 0;
        self.hflip = v & 0x20 != 0;
        self.pixel_bits = ((v >> 6) & 3) as u32 + 1;
    }

    fn set_ctl1(&mut self, v: u8) {
        self.start_left = v & 1 != 0;
        self.start_up = v & 2 != 0;
        self.skip = v & 4 != 0;
        self.reload_palette = v & 8 != 0;
        self.reload_depth = (v >> 4) & 3;
        self.literal = v & 0x80 != 0;
    }

    // ---------------- math ----------------

    fn multiply(&mut self) {
        self.math_bit = false;
        let mut r = (self.abcd >> 16) * (self.abcd & 0xFFFF);
        if self.sprsys & 0x80 != 0 && self.ab_sign + self.cd_sign == 0 { r = (!r).wrapping_add(1); }
        self.efgh = r;
        if self.sprsys & 0x40 != 0 { self.jklm = self.jklm.wrapping_add(r); }
    }

    fn divide(&mut self) {
        self.math_bit = false;
        if self.np != 0 {
            self.abcd = self.efgh / self.np;
            self.jklm = self.efgh % self.np;
        } else {
            self.abcd = 0xFFFF_FFFF;
            self.jklm = 0;
            self.math_bit = true;
        }
    }

    // ---------------- sprites ----------------

    fn peek(ram: &[u8], a: u16) -> u8 { ram[a as usize] }
    fn peekw(ram: &[u8], a: u16) -> u16 { ram[a as usize] as u16 | (ram[a.wrapping_add(1) as usize] as u16) << 8 }

    /// Draw the SCB chain (if SPRGO and the bus are enabled). Returns the time it took
    /// in 16 MHz ticks, for the CPU to sleep through.
    pub fn paint(&mut self, ram: &mut [u8]) -> u32 {
        if !self.busen || !self.sprgo { return 0; }
        self.cycles = 0;
        let mut count = 0;
        loop {
            let mut everonscreen = false;
            // only the high byte of SCBNEXT is tested (a documented hardware bug)
            if self.w[SCBNEXT] & 0xFF00 == 0 { self.sprgo = false; break; }
            self.w[TMPADR] = self.w[SCBNEXT];
            self.w[SCBADR] = self.w[SCBNEXT];
            let mut p = self.w[TMPADR];
            let c0 = Self::peek(ram, p); self.set_ctl0(c0); p = p.wrapping_add(1);
            let c1 = Self::peek(ram, p); self.set_ctl1(c1); p = p.wrapping_add(1);
            let cn = Self::peek(ram, p); self.coll_number = cn & 0x0F; self.coll_off = cn & 0x20 != 0; p = p.wrapping_add(1);
            self.w[SCBNEXT] = Self::peekw(ram, p); p = p.wrapping_add(2);
            self.cycles += 5 * RW;
            self.collision = 0;

            if !self.skip {
                self.w[SPRDLINE] = Self::peekw(ram, p); p = p.wrapping_add(2);
                self.w[HPOSSTRT] = Self::peekw(ram, p); p = p.wrapping_add(2);
                self.w[VPOSSTRT] = Self::peekw(ram, p); p = p.wrapping_add(2);
                self.cycles += 6 * RW;
                let (mut stretch_on, mut tilt_on) = (false, false);
                if self.reload_depth >= 1 {
                    self.w[SPRHSIZ] = Self::peekw(ram, p); p = p.wrapping_add(2);
                    self.w[SPRVSIZ] = Self::peekw(ram, p); p = p.wrapping_add(2);
                    self.cycles += 4 * RW;
                }
                if self.reload_depth >= 2 {
                    stretch_on = true;
                    self.w[STRETCH] = Self::peekw(ram, p); p = p.wrapping_add(2);
                    self.cycles += 2 * RW;
                }
                if self.reload_depth >= 3 {
                    tilt_on = true;
                    self.w[TILT] = Self::peekw(ram, p); p = p.wrapping_add(2);
                    self.cycles += 2 * RW;
                }
                if !self.reload_palette {
                    for i in 0..8 {
                        let d = Self::peek(ram, p); p = p.wrapping_add(1);
                        self.pen[i * 2] = d >> 4;
                        self.pen[i * 2 + 1] = d & 15;
                    }
                    self.cycles += 8 * RW;
                }
                self.w[TMPADR] = p;
                everonscreen = self.draw_sprite(ram, stretch_on, tilt_on);

                // collision depository
                if !self.coll_off && self.sprsys & 0x20 == 0 && matches!(self.sprite_type, 2 | 3 | 4 | 6 | 7) {
                    let a = self.w[SCBADR].wrapping_add(self.w[COLLOFF]);
                    ram[a as usize] = self.collision as u8;
                }
                if self.everon {
                    let a = self.w[SCBADR].wrapping_add(self.w[COLLOFF]) as usize;
                    ram[a] = if everonscreen { ram[a] & 0x7F } else { ram[a] | 0x80 };
                }
            }
            count += 1;
            if count > 4096 { self.sprgo = false; break; }      // a looped chain
        }
        self.cycles
    }

    /// One sprite, all its quadrants. Returns whether any pixel landed on screen.
    fn draw_sprite(&mut self, ram: &mut [u8], stretch_on: bool, tilt_on: bool) -> bool {
        let mut everon = false;
        let sh_start = self.w[HOFF] as i16 as i32;
        let sh_end = sh_start + SCREEN_W;
        let sv_start = self.w[VOFF] as i16 as i32;
        let sv_end = sv_start + SCREEN_H;
        let h_mid = sh_start + SCREEN_W / 2;
        let v_mid = sv_start + SCREEN_H / 2;
        // quadrants: 0 SE, 1 NE, 2 NW, 3 SW
        let mut quadrant = match (self.start_left, self.start_up) { (true, true) => 2, (true, false) => 3, (false, true) => 1, _ => 0 };
        let hpos = self.w[HPOSSTRT] as i16 as i32;
        let vpos = self.w[VPOSSTRT] as i16 as i32;
        let superclip = hpos < sh_start || hpos >= sh_end || vpos < sv_start || vpos >= sv_end;
        let (mut vquadoff, mut hquadoff) = (0i32, 0i32);
        let mut quad = 0;
        while quad < 4 {
            let sprite_v = self.w[VPOSSTRT] as i32;
            let sprite_h = self.w[HPOSSTRT] as i32;
            let mut hsign = if quadrant == 0 || quadrant == 1 { 1 } else { -1 };
            let mut vsign = if quadrant == 0 || quadrant == 3 { 1 } else { -1 };
            if self.vflip { vsign = -vsign; }
            if self.hflip { hsign = -hsign; }
            let render = if superclip {
                let mut modq = quadrant;
                if self.vflip { modq = [1, 0, 3, 2][modq]; }
                if self.hflip { modq = [3, 2, 1, 0][modq]; }
                match modq {
                    3 => (sprite_h >= sh_start || sprite_h <= h_mid) && (sprite_v < sv_end || sprite_v >= v_mid),
                    2 => (sprite_h >= sh_start || sprite_h <= h_mid) && (sprite_v >= sv_start || sprite_v <= v_mid),
                    1 => (sprite_h < sh_end || sprite_h >= h_mid) && (sprite_v >= sv_start || sprite_v <= v_mid),
                    _ => (sprite_h < sh_end || sprite_h >= h_mid) && (sprite_v < sv_end || sprite_v >= v_mid),
                }
            } else { true };

            if render {
                let mut voff = self.w[VPOSSTRT] as i16 as i32 - sv_start;
                self.w[TILTACUM] = 0;
                self.w[VSIZACUM] = if vsign == 1 { self.w[VSIZOFF] } else { 0 };
                if quad == 0 { vquadoff = vsign; }
                if vsign != vquadoff { voff += vsign; }
                loop {
                    // vertical scaling
                    self.w[VSIZACUM] = self.w[VSIZACUM].wrapping_add(self.w[SPRVSIZ]);
                    let pixel_height = (self.w[VSIZACUM] >> 8) as i32;
                    self.w[VSIZACUM] &= 0x00FF;
                    let off = self.line_init(ram, 0) as u16;
                    self.w[SPRDOFF] = off;
                    if off == 1 { self.w[SPRDLINE] = self.w[SPRDLINE].wrapping_add(off); break; }   // next quadrant
                    if off == 0 { quad = 4; break; }                                                 // end of sprite
                    for _ in 0..pixel_height {
                        if vsign == 1 && voff >= SCREEN_H { break; }
                        if vsign == -1 && voff < 0 { break; }
                        if (0..SCREEN_H).contains(&voff) {
                            // start + tilt
                            let t = (self.w[TILTACUM] as i16) >> 8;
                            self.w[HPOSSTRT] = self.w[HPOSSTRT].wrapping_add(t as u16);
                            self.w[TILTACUM] &= 0x00FF;
                            let mut hoff = self.w[HPOSSTRT] as i16 as i32 - sh_start;
                            self.hsizacum = self.w[HSIZOFF];
                            if quad == 0 { hquadoff = hsign; }
                            if hsign != hquadoff { hoff += hsign; }
                            self.line_init(ram, voff as u32);
                            let mut onscreen = false;
                            'line: loop {
                                let pixel = self.line_pixel(ram);
                                if pixel == LINE_END { break; }
                                self.hsizacum = self.hsizacum.wrapping_add(self.w[SPRHSIZ]);
                                let width = self.hsizacum >> 8;
                                self.hsizacum &= 0x00FF;
                                for _ in 0..width {
                                    if (0..SCREEN_W).contains(&hoff) {
                                        self.process_pixel(ram, hoff as u32, pixel);
                                        onscreen = true;
                                        everon = true;
                                    } else if onscreen {
                                        break 'line;
                                    }
                                    hoff += hsign;
                                }
                            }
                        }
                        voff += vsign;
                        if stretch_on { self.w[SPRHSIZ] = self.w[SPRHSIZ].wrapping_add(self.w[STRETCH]); }
                        if tilt_on { self.w[TILTACUM] = self.w[TILTACUM].wrapping_add(self.w[TILT]); }
                    }
                    if self.sprsys & 0x10 != 0 {
                        self.w[SPRVSIZ] = self.w[SPRVSIZ].wrapping_add(self.w[STRETCH].wrapping_mul(pixel_height as u16));
                    }
                    self.w[SPRDLINE] = self.w[SPRDLINE].wrapping_add(self.w[SPRDOFF]);
                }
            } else {
                // skip this quadrant's data
                loop {
                    let off = self.line_init(ram, 0) as u16;
                    self.w[SPRDOFF] = off;
                    self.w[SPRDLINE] = self.w[SPRDLINE].wrapping_add(off);
                    if off == 1 { break; }
                    if off == 0 { quad = 4; break; }
                }
            }
            quad += 1;
            quadrant = (quadrant + 1) & 3;
        }
        everon
    }

    fn line_init(&mut self, ram: &[u8], voff: u32) -> u32 {
        self.shift_reg = 0;
        self.shift_count = 0;
        self.repeat = 0;
        self.line_pixel = 0;
        self.line_type = LineType::Error;
        self.bits_left = 0xFFFF;
        self.w[TMPADR] = self.w[SPRDLINE];
        let offset = self.get_bits(ram, 8);
        self.bits_left = offset.wrapping_sub(1).wrapping_mul(8);
        if self.literal {
            self.line_type = LineType::AbsLiteral;
            self.repeat = offset.wrapping_sub(1).wrapping_mul(8) / self.pixel_bits;
        }
        let voff = if voff > 101 { 0 } else { voff };
        self.line_base = self.w[VIDBAS].wrapping_add((voff * 80) as u16);
        self.line_coll = self.w[COLLBAS].wrapping_add((voff * 80) as u16);
        offset
    }

    fn line_pixel(&mut self, ram: &[u8]) -> u32 {
        if self.repeat == 0 {
            if self.line_type != LineType::AbsLiteral {
                self.line_type = if self.get_bits(ram, 1) != 0 { LineType::Literal } else { LineType::Packed };
            }
            match self.line_type {
                LineType::AbsLiteral => { self.line_pixel = LINE_END; return LINE_END; }
                LineType::Literal => { self.repeat = self.get_bits(ram, 4) + 1; }
                LineType::Packed => {
                    self.repeat = self.get_bits(ram, 4);
                    if self.repeat == 0 {
                        self.line_pixel = LINE_END;
                    } else {
                        let b = self.pixel_bits;
                        self.line_pixel = self.pen[self.get_bits(ram, b) as usize] as u32;
                    }
                    self.repeat += 1;
                }
                LineType::Error => return 0,
            }
        }
        if self.line_pixel != LINE_END {
            self.repeat -= 1;
            let b = self.pixel_bits;
            match self.line_type {
                LineType::AbsLiteral => {
                    let p = self.get_bits(ram, b);
                    self.line_pixel = if self.repeat == 0 && p == 0 { LINE_END } else { self.pen[p as usize] as u32 };
                }
                LineType::Literal => { self.line_pixel = self.pen[self.get_bits(ram, b) as usize] as u32; }
                LineType::Packed => {}
                LineType::Error => return 0,
            }
        }
        self.line_pixel
    }

    fn get_bits(&mut self, ram: &[u8], bits: u32) -> u32 {
        if self.bits_left <= bits { return 0; }
        if self.shift_count < bits {
            let a = self.w[TMPADR];
            self.shift_reg = self.shift_reg << 24
                | (ram[a as usize] as u32) << 16
                | (ram[a.wrapping_add(1) as usize] as u32) << 8
                | ram[a.wrapping_add(2) as usize] as u32;
            self.w[TMPADR] = a.wrapping_add(3);
            self.shift_count += 24;
            self.cycles += 3 * RW;
        }
        let v = (self.shift_reg >> (self.shift_count - bits)) & ((1 << bits) - 1);
        self.shift_count -= bits;
        self.bits_left -= bits;
        v
    }

    fn write_nibble(&mut self, ram: &mut [u8], base: u16, hoff: u32, v: u32) {
        let a = base.wrapping_add((hoff / 2) as u16) as usize;
        ram[a] = if hoff & 1 == 0 { (ram[a] & 0x0F) | (v as u8) << 4 } else { (ram[a] & 0xF0) | v as u8 };
        self.cycles += 2 * RW;
    }

    fn read_nibble(&mut self, ram: &[u8], base: u16, hoff: u32) -> u32 {
        let d = ram[base.wrapping_add((hoff / 2) as u16) as usize] as u32;
        self.cycles += RW;
        if hoff & 1 == 0 { d >> 4 } else { d & 15 }
    }

    /// Collide: note the highest number already there and leave ours.
    fn collide(&mut self, ram: &mut [u8], hoff: u32) {
        if self.coll_off || self.sprsys & 0x20 != 0 { return; }
        let c = self.read_nibble(ram, self.line_coll, hoff);
        if c > self.collision { self.collision = c; }
        let n = self.coll_number as u32;
        self.write_nibble(ram, self.line_coll, hoff, n);
    }

    /// One pixel, by sprite type (0 background shadow, 1 background no-collide,
    /// 2 boundary shadow, 3 boundary, 4 normal, 5 no-collide, 6 xor shadow, 7 shadow).
    fn process_pixel(&mut self, ram: &mut [u8], hoff: u32, pixel: u32) {
        let base = self.line_base;
        match self.sprite_type {
            0 => {
                self.write_nibble(ram, base, hoff, pixel);
                if !self.coll_off && self.sprsys & 0x20 == 0 && pixel != 0x0E {
                    let n = self.coll_number as u32;
                    self.write_nibble(ram, self.line_coll, hoff, n);
                }
            }
            1 => self.write_nibble(ram, base, hoff, pixel),
            5 => if pixel != 0 { self.write_nibble(ram, base, hoff, pixel) },
            3 => {
                if pixel != 0 && pixel != 0x0F { self.write_nibble(ram, base, hoff, pixel); }
                if pixel != 0 { self.collide(ram, hoff); }
            }
            4 => if pixel != 0 { self.write_nibble(ram, base, hoff, pixel); self.collide(ram, hoff); },
            2 => {
                if pixel != 0 && pixel != 0x0E && pixel != 0x0F { self.write_nibble(ram, base, hoff, pixel); }
                if pixel != 0 && pixel != 0x0E { self.collide(ram, hoff); }
            }
            7 => {
                if pixel != 0 { self.write_nibble(ram, base, hoff, pixel); }
                if pixel != 0 && pixel != 0x0E { self.collide(ram, hoff); }
            }
            _ => {                                                  // 6: xor shadow
                if pixel != 0 {
                    let old = self.read_nibble(ram, base, hoff);
                    self.write_nibble(ram, base, hoff, old ^ pixel);
                }
                if pixel != 0 && pixel != 0x0E { self.collide(ram, hoff); }
            }
        }
    }
}
