// Atari TIA (Television Interface Adaptor): video and sound.
//
// There's no frame buffer and no tiles. The TIA draws whatever its registers say *right now*,
// one color clock at a time, and the game changes the registers in time with the beam
// ("racing the beam"). So we render one pixel per color clock.
//
// Line: 228 color clocks = 68 horizontal blank + 160 visible pixels. 3 color clocks per CPU cycle.
// Frame: as many lines as the game wants; a frame starts when the game turns VSYNC on.
//
// Objects (all 1 line tall; the game rewrites them every line):
//   playfield  20 bits (PF0 4-7, PF1 7-0, PF2 0-7), each 4 pixels wide; the right half
//              repeats or mirrors (CTRLPF bit 0)
//   player 0/1 8-pixel graphics (GRPx), 1-3 copies or 2x/4x wide (NUSIZx), mirrored (REFPx)
//   missile 0/1, ball: 1-8 pixel lines (sizes in NUSIZx bits 4-5 / CTRLPF bits 4-5)
// Positions are set by strobing RESxx at the moment the beam is where you want the object,
// then nudged with HMxx + HMOVE (-8..+7 pixels).
//
// Write registers (address & 0x3F) and read registers (address & 0x0F) are listed at
// `write()` and `read()` below.
//
// Reference: Stella Programmer's Guide (Steve Wright, 1979), Stella emulator, AtariAge docs

use serde::{Deserialize, Serialize};

pub const SCREEN_W: usize = 160;
pub const FB_LINES: usize = 262;
const HBLANK: u16 = 68;
const CLOCKS_PER_LINE: u16 = 228;
const MAX_LINES: u16 = 320;
const W: u16 = SCREEN_W as u16;
/// HMOVE timing, by the color clock where the write lands (3 x the CPU cycle count):
///   cycle 73-74 (clock 219-222): "late", no black bar, objects move 8 further left
///   cycle 75    (clock 225):     counts as a normal HMOVE for the *next* line (bar there)
const LATE_HMOVE: u16 = 219;
const NEXT_LINE_HMOVE: u16 = 225;     // a frame with no VSYNC gets cut here
pub const AUDIO_RATE: f64 = 3_579_545.0 / 114.0;   // 2 audio clocks per line = 31.4 kHz

/// NTSC palette (Stella's), indexed by color register >> 1.
#[rustfmt::skip]
const NTSC_PALETTE: [u32; 128] = [
    0x000000, 0x4a4a4a, 0x6f6f6f, 0x8e8e8e, 0xaaaaaa, 0xc0c0c0, 0xd6d6d6, 0xececec,
    0x484800, 0x69690f, 0x86861d, 0xa2a22a, 0xbbbb35, 0xd2d240, 0xe8e84a, 0xfcfc54,
    0x7c2c00, 0x904811, 0xa26221, 0xb47a30, 0xc3903d, 0xd2a44a, 0xdfb755, 0xecc860,
    0x901c00, 0xa33915, 0xb55328, 0xc66c3a, 0xd5824a, 0xe39759, 0xf0aa67, 0xfcbc74,
    0x940000, 0xa71a1a, 0xb83232, 0xc84848, 0xd65c5c, 0xe46f6f, 0xf08080, 0xfc9090,
    0x840064, 0x97197a, 0xa8308f, 0xb846a2, 0xc659b3, 0xd46cc3, 0xe07cd2, 0xec8ce0,
    0x500084, 0x68199a, 0x7d30ad, 0x9246c0, 0xa459d0, 0xb56ce0, 0xc57cee, 0xd48cfc,
    0x140090, 0x331aa3, 0x4e32b5, 0x6848c6, 0x7f5cd5, 0x956fe3, 0xa980f0, 0xbc90fc,
    0x000094, 0x181aa7, 0x2d32b8, 0x4248c8, 0x545cd6, 0x656fe4, 0x7580f0, 0x8490fc,
    0x001c88, 0x183b9d, 0x2d57b0, 0x4272c2, 0x548ad2, 0x65a0e1, 0x75b5ef, 0x84c8fc,
    0x003064, 0x185080, 0x2d6d98, 0x4288b0, 0x54a0c5, 0x65b7d9, 0x75cceb, 0x84e0fc,
    0x004030, 0x18624e, 0x2d8169, 0x429e82, 0x54b899, 0x65d1ae, 0x75e7c2, 0x84fcd4,
    0x004400, 0x1a661a, 0x328432, 0x48a048, 0x5cba5c, 0x6fd26f, 0x80e880, 0x90fc90,
    0x143c00, 0x355f18, 0x527e2d, 0x6e9c42, 0x87b754, 0x9ed065, 0xb4e775, 0xc8fc84,
    0x303800, 0x505916, 0x6d762b, 0x88923e, 0xa0ab4f, 0xb7c25f, 0xccd86e, 0xe0ec7c,
    0x482c00, 0x694d14, 0x866a26, 0xa28638, 0xbb9f47, 0xd2b656, 0xe8cc63, 0xfce070,
];

pub fn rgb(color_reg: u8) -> [u8; 3] {
    let c = NTSC_PALETTE[(color_reg >> 1) as usize];
    [(c >> 16) as u8, (c >> 8) as u8, c as u8]
}

/// Copy offsets for NUSIZ bits 0-2, and the player's pixel scale.
fn copies(nusiz: u8) -> (&'static [u16], u16) {
    match nusiz & 7 {
        0 => (&[0], 1),
        1 => (&[0, 16], 1),
        2 => (&[0, 32], 1),
        3 => (&[0, 16, 32], 1),
        4 => (&[0, 64], 1),
        5 => (&[0], 2),
        6 => (&[0, 32, 64], 1),
        _ => (&[0], 4),
    }
}

// ---------------------------------------------------------------- audio

#[derive(Serialize, Deserialize, Clone, Default)]
struct AudioChannel {
    audc: u8,
    audf: u8,
    audv: u8,
    divider: u8,
    aux: u8,
    out: bool,
    poly4: u8,
    poly5: u8,
    poly9: u16,
}

impl AudioChannel {
    fn new() -> Self { Self { poly4: 0x0F, poly5: 0x1F, poly9: 0x1FF, ..Default::default() } }

    fn step4(&mut self) { self.poly4 = ((self.poly4 << 1) | (((self.poly4 >> 3) ^ (self.poly4 >> 2)) & 1)) & 0x0F; }
    fn step5(&mut self) { self.poly5 = ((self.poly5 << 1) | (((self.poly5 >> 4) ^ (self.poly5 >> 2)) & 1)) & 0x1F; }
    fn step9(&mut self) { self.poly9 = ((self.poly9 << 1) | (((self.poly9 >> 8) ^ (self.poly9 >> 4)) & 1)) & 0x1FF; }

    /// One 31.4 kHz audio clock. AUDF divides it by (AUDF + 1); AUDC picks the waveform.
    fn clock(&mut self) {
        if self.divider > 0 {
            self.divider -= 1;
            return;
        }
        self.divider = self.audf & 0x1F;
        match self.audc & 0x0F {
            0x0 | 0xB => self.out = true,                         // volume only (sample playback)
            0x1 => { self.step4(); self.out = self.poly4 & 1 != 0; }
            0x2 => {                                               // div 15 -> poly4
                self.aux = (self.aux + 1) % 15;
                if self.aux == 0 { self.step4(); }
                self.out = self.poly4 & 1 != 0;
            }
            0x3 => {                                               // poly5 -> poly4
                self.step5();
                if self.poly5 & 1 != 0 { self.step4(); }
                self.out = self.poly4 & 1 != 0;
            }
            0x4 | 0x5 => self.out = !self.out,                     // pure tone, div 2
            0x6 | 0xA => { self.aux = (self.aux + 1) % 31; self.out = self.aux < 18; }   // div 31
            0x7 | 0x9 => { self.step5(); self.out = self.poly5 & 1 != 0; }
            0x8 => { self.step9(); self.out = self.poly9 & 1 != 0; }                     // white noise
            0xC | 0xD => {                                         // pure tone, div 6
                self.aux = (self.aux + 1) % 3;
                if self.aux == 0 { self.out = !self.out; }
            }
            0xE => { self.aux = (self.aux + 1) % 93; self.out = self.aux < 47; }         // div 93
            _ => {                                                 // 0xF: poly5 -> div 6
                self.step5();
                if self.poly5 & 1 != 0 {
                    self.aux = (self.aux + 1) % 3;
                    if self.aux == 0 { self.out = !self.out; }
                }
            }
        }
    }

    fn level(&self) -> f32 { if self.out { (self.audv & 0x0F) as f32 } else { 0.0 } }
}

// ---------------------------------------------------------------- TIA

#[derive(Serialize, Deserialize, Clone)]
pub struct Tia {
    pub hpos: u16,
    pub line: u16,
    pub last_frame_lines: u16,
    vsync: bool,
    vblank: u8,
    pub wsync: bool,              // CPU halted until the end of the line
    pub frame_done: bool,

    colup: [u8; 2],
    colupf: u8,
    colubk: u8,
    ctrlpf: u8,
    pf: [u8; 3],
    nusiz: [u8; 2],
    refp: [bool; 2],

    pos_p: [u16; 2],
    pos_m: [u16; 2],
    pos_bl: u16,
    hm_p: [u8; 2],
    hm_m: [u8; 2],
    hm_bl: u8,
    hmove_blank: bool,
    hmove_blank_next: bool,

    grp_new: [u8; 2],
    grp_old: [u8; 2],
    vdelp: [bool; 2],
    enam: [bool; 2],
    resmp: [bool; 2],
    /// Set by RESPx / RESMx until the line ends: the object's counter was just reset, so its
    /// main copy isn't drawn again until the next line (its NUSIZ copies still are).
    /// Not saved: it only lasts to the end of the current line.
    #[serde(skip)]
    reset_this_line_p: [bool; 2],
    #[serde(skip)]
    reset_this_line_m: [bool; 2],
    enabl_new: bool,
    enabl_old: bool,
    vdelbl: bool,

    collisions: [u8; 8],          // read registers 0-7, bits 7 and 6

    pub fire: [bool; 2],          // joystick buttons (INPT4/5)

    /// Writes the TIA applies a few color clocks late: (register, value, clocks left).
    delayed: Vec<(u8, u8, u8)>,
    /// The playfield is sampled once per 4-pixel cell, at the cell's first pixel.
    pf_latch: bool,

    audio: [AudioChannel; 2],
    audio_phase: f64,
    audio_acc: f32,
    audio_n: u32,
    dc: (f32, f32),
    out_rate: f64,
    #[serde(skip)]
    pub audio_out: Vec<f32>,

    #[serde(skip)]
    pub framebuffer: Vec<u8>,     // RGBA, SCREEN_W x FB_LINES

    /// Lines where VBLANK was off this frame (first, last), and the same for the last frame.
    #[serde(skip)]
    vis: Option<(u16, u16)>,
    #[serde(skip)]
    pub last_vis: Option<(u16, u16)>,
    /// 7800: color clocks not yet turned into audio clocks (see run_audio)
    #[serde(skip)]
    audio_clocks: u32,

    /// Debug: when Some, every register write is recorded as (line, color clock, register, value).
    #[serde(skip)]
    pub write_log: Option<Vec<(u16, u16, u8, u8)>>,
}

impl Tia {
    pub fn new(out_rate: u32) -> Self {
        Self {
            hpos: 0, line: 0, last_frame_lines: 0,
            vsync: false, vblank: 0, wsync: false, frame_done: false,
            colup: [0; 2], colupf: 0, colubk: 0, ctrlpf: 0, pf: [0; 3], nusiz: [0; 2], refp: [false; 2],
            pos_p: [0; 2], pos_m: [0; 2], pos_bl: 0, hm_p: [0; 2], hm_m: [0; 2], hm_bl: 0, hmove_blank: false, hmove_blank_next: false,
            grp_new: [0; 2], grp_old: [0; 2], vdelp: [false; 2], enam: [false; 2], resmp: [false; 2],
            reset_this_line_p: [false; 2], reset_this_line_m: [false; 2],
            enabl_new: false, enabl_old: false, vdelbl: false,
            collisions: [0; 8],
            fire: [false; 2],
            delayed: Vec::with_capacity(4),
            pf_latch: false,
            audio: [AudioChannel::new(), AudioChannel::new()],
            audio_phase: 0.0, audio_acc: 0.0, audio_n: 0, dc: (0.0, 0.0),
            out_rate: out_rate as f64,
            audio_out: Vec::with_capacity(2048),
            framebuffer: vec![0; SCREEN_W * FB_LINES * 4],
            write_log: None,
            vis: None,
            last_vis: None,
            audio_clocks: 0,
        }
    }

    pub fn load_state(&mut self, s: Tia) {
        let fb = std::mem::take(&mut self.framebuffer);
        let out = std::mem::take(&mut self.audio_out);
        *self = s;
        self.framebuffer = fb;
        self.audio_out = out;
    }

    // ---------------- register access ----------------

    /// Where RESxx puts an object: a few clocks after the strobe, or the left edge during HBLANK.
    fn reset_position(&self, delay: u16) -> u16 {
        if self.hpos < HBLANK { delay - 2 } else { (self.hpos - HBLANK + delay) % SCREEN_W as u16 }
    }

    /// HMxx: signed high nibble, positive values move left.
    fn apply_motion(pos: &mut u16, hm: u8, extra: i32) {
        let delta = (hm as i8 >> 4) as i32 + extra;
        *pos = (*pos as i32 - delta).rem_euclid(SCREEN_W as i32) as u16;
    }

    fn missile_lock_offset(&self, n: usize) -> u16 {
        match self.nusiz[n] & 7 { 5 => 6, 7 => 10, _ => 3 }
    }

    pub fn write(&mut self, reg: u8, v: u8) {
        if let Some(log) = self.write_log.as_mut() { log.push((self.line, self.hpos, reg & 0x3F, v)); }
        // Delays measured from the end of the CPU write (Stella's TIA): playfield 2 clocks,
        // player graphics and enables 1 clock. Stored as clocks-to-go, counted down before
        // each pixel is drawn.
        let delay = match reg & 0x3F {
            0x0D..=0x0F => 3,
            0x1B..=0x1F => 2,
            _ => 0,
        };
        if delay > 0 {
            self.delayed.push((reg & 0x3F, v, delay));
        } else {
            self.write_now(reg, v);
        }
    }

    fn write_now(&mut self, reg: u8, v: u8) {
        match reg & 0x3F {
            0x00 => {                                            // VSYNC
                let on = v & 0x02 != 0;
                if on && !self.vsync { self.end_frame(); }
                self.vsync = on;
            }
            0x01 => self.vblank = v,                             // VBLANK (bit 1 = blank)
            // WSYNC: halt the CPU until the line ends. Writes reach us after their cycle's 3
            // clocks; if those clocks just finished the line (hpos 0-2), the latch on real
            // hardware was set *during* that cycle and released at the line end already,
            // so the CPU doesn't stop. Halting here would cost a whole extra line.
            0x02 => if self.hpos >= 3 { self.wsync = true },
            0x03 => self.hpos = 0,                               // RSYNC (test register)
            0x04 => self.nusiz[0] = v,
            0x05 => self.nusiz[1] = v,
            0x06 => self.colup[0] = v,
            0x07 => self.colup[1] = v,
            0x08 => self.colupf = v,
            0x09 => self.colubk = v,
            0x0A => self.ctrlpf = v,
            0x0B => self.refp[0] = v & 0x08 != 0,
            0x0C => self.refp[1] = v & 0x08 != 0,
            0x0D => self.pf[0] = v,
            0x0E => self.pf[1] = v,
            0x0F => self.pf[2] = v,
            0x10 | 0x11 => {                                     // RESP0/1
                let n = (reg & 1) as usize;
                self.pos_p[n] = self.reset_position(5);
                self.reset_this_line_p[n] = true;
            }
            0x12 | 0x13 => {                                     // RESM0/1
                let n = (reg & 1) as usize;
                self.pos_m[n] = self.reset_position(4);
                self.reset_this_line_m[n] = true;
            }
            0x14 => self.pos_bl = self.reset_position(4),        // RESBL
            0x15 => self.audio[0].audc = v,
            0x16 => self.audio[1].audc = v,
            0x17 => self.audio[0].audf = v,
            0x18 => self.audio[1].audf = v,
            0x19 => self.audio[0].audv = v,
            0x1A => self.audio[1].audv = v,
            0x1B => {                                            // GRP0 (also latches the old GRP1)
                self.grp_new[0] = v;
                self.grp_old[1] = self.grp_new[1];
            }
            0x1C => {                                            // GRP1 (latches old GRP0 and ENABL)
                self.grp_new[1] = v;
                self.grp_old[0] = self.grp_new[0];
                self.enabl_old = self.enabl_new;
            }
            0x1D => self.enam[0] = v & 0x02 != 0,
            0x1E => self.enam[1] = v & 0x02 != 0,
            0x1F => self.enabl_new = v & 0x02 != 0,
            0x20 => self.hm_p[0] = v,
            0x21 => self.hm_p[1] = v,
            0x22 => self.hm_m[0] = v,
            0x23 => self.hm_m[1] = v,
            0x24 => self.hm_bl = v,
            0x25 => self.vdelp[0] = v & 0x01 != 0,
            0x26 => self.vdelp[1] = v & 0x01 != 0,
            0x27 => self.vdelbl = v & 0x01 != 0,
            0x28 | 0x29 => {                                     // RESMP0/1: lock missile to player
                let n = (reg & 1) as usize;
                self.resmp[n] = v & 0x02 != 0;
                if self.resmp[n] {
                    self.pos_m[n] = (self.pos_p[n] + self.missile_lock_offset(n)) % SCREEN_W as u16;
                }
            }
            0x2A => {                                            // HMOVE
                // HMOVE sends (HM xor 8) = 0..15 extra clocks to each object during HBLANK,
                // and stretches HBLANK by 8 clocks, which costs every object 8 clocks:
                // net motion = HM (-8..+7). Hit at the very end of the previous line
                // (cycle 73-75, the Activision trick) HBLANK isn't stretched: no black bar,
                // and objects move 8 further left.
                let late = (LATE_HMOVE..NEXT_LINE_HMOVE).contains(&self.hpos);
                let extra = if late { 8 } else { 0 };
                for n in 0..2 {
                    Self::apply_motion(&mut self.pos_p[n], self.hm_p[n], extra);
                    Self::apply_motion(&mut self.pos_m[n], self.hm_m[n], extra);
                }
                Self::apply_motion(&mut self.pos_bl, self.hm_bl, extra);
                // HMOVE at the start of a line blanks its first 8 pixels (the "HMOVE bar")
                if self.hpos < HBLANK { self.hmove_blank = true; }
                if self.hpos >= NEXT_LINE_HMOVE { self.hmove_blank_next = true; }
            }
            0x2B => { self.hm_p = [0; 2]; self.hm_m = [0; 2]; self.hm_bl = 0; }   // HMCLR
            0x2C => self.collisions = [0; 8],                    // CXCLR
            _ => {}
        }
    }

    /// Reads: 0-7 collision latches (bits 7/6), 8-B paddles, C-D fire buttons (0 = pressed).
    /// Only bits 7-6 are real; the bus fills in the rest from the data bus.
    pub fn read(&self, reg: u8) -> u8 {
        match reg & 0x0F {
            r @ 0..=7 => self.collisions[r as usize],
            0x8..=0xB => if self.vblank & 0x80 != 0 { 0x00 } else { 0x80 },   // paddles: no paddle connected
            0xC => if self.fire[0] { 0x00 } else { 0x80 },
            0xD => if self.fire[1] { 0x00 } else { 0x80 },
            _ => 0x00,                   // 0x0E/0x0F: nothing drives the bus (see a2600_bus.rs)
        }
    }

    // ---------------- clocking ----------------

    fn end_frame(&mut self) {
        // black out lines this frame didn't reach
        let from = (self.line as usize).min(FB_LINES) * SCREEN_W * 4;
        for px in self.framebuffer[from..].chunks_mut(4) { px.copy_from_slice(&[0, 0, 0, 0xFF]); }
        self.last_frame_lines = self.line;
        self.last_vis = self.vis.take();
        self.line = 0;
        self.frame_done = true;
    }

    /// One color clock.
    pub fn clock(&mut self) {
        if !self.delayed.is_empty() {
            let mut i = 0;
            while i < self.delayed.len() {
                self.delayed[i].2 -= 1;
                if self.delayed[i].2 == 0 {
                    let (reg, v, _) = self.delayed.remove(i);
                    self.write_now(reg, v);
                } else {
                    i += 1;
                }
            }
        }
        if self.hpos >= HBLANK {
            self.render_pixel((self.hpos - HBLANK) as usize);
        }
        if self.hpos == 0 || self.hpos == CLOCKS_PER_LINE / 2 {
            self.clock_audio();
        }
        self.hpos += 1;
        if self.hpos == CLOCKS_PER_LINE {
            self.hpos = 0;
            self.line += 1;
            self.wsync = false;
            self.hmove_blank = self.hmove_blank_next;
            self.hmove_blank_next = false;
            self.reset_this_line_p = [false; 2];
            self.reset_this_line_m = [false; 2];
            if self.line >= MAX_LINES { self.end_frame(); }
        }
    }

    fn playfield_bit(&self, x: usize) -> bool {
        let mut i = x / 4;                       // 0-39
        if i >= 20 {
            i = if self.ctrlpf & 0x01 != 0 { 39 - i } else { i - 20 };
        }
        match i {
            0..=3 => self.pf[0] & (0x10 << i) != 0,
            4..=11 => self.pf[1] & (0x80 >> (i - 4)) != 0,
            _ => self.pf[2] & (0x01 << (i - 12)) != 0,
        }
    }

    fn player_bit(&self, n: usize, x: u16) -> bool {
        let grp = if self.vdelp[n] { self.grp_old[n] } else { self.grp_new[n] };
        if grp == 0 { return false; }
        let (offsets, scale) = copies(self.nusiz[n]);
        for &o in offsets {
            if o == 0 && self.reset_this_line_p[n] { continue; }    // main copy: from next line
            let d = (x + 2 * SCREEN_W as u16 - self.pos_p[n] - o) % SCREEN_W as u16;
            if d < 8 * scale {
                let bit = d / scale;
                let mask = if self.refp[n] { 1 << bit } else { 0x80 >> bit };
                if grp & mask != 0 { return true; }
            }
        }
        false
    }

    fn missile_bit(&self, n: usize, x: u16) -> bool {
        if !self.enam[n] || self.resmp[n] { return false; }
        let size = 1 << ((self.nusiz[n] >> 4) & 3);
        let (offsets, _) = copies(self.nusiz[n]);
        offsets.iter()
            .filter(|&&o| !(o == 0 && self.reset_this_line_m[n]))   // main copy: from next line
            .any(|&o| (x + 2 * W - self.pos_m[n] - o) % W < size)
    }

    fn ball_bit(&self, x: u16) -> bool {
        let on = if self.vdelbl { self.enabl_old } else { self.enabl_new };
        if !on { return false; }
        let size = 1 << ((self.ctrlpf >> 4) & 3);
        (x + W - self.pos_bl) % W < size
    }

    fn render_pixel(&mut self, x: usize) {
        let xx = x as u16;
        if x % 4 == 0 { self.pf_latch = self.playfield_bit(x); }
        let pf = self.pf_latch;
        let bl = self.ball_bit(xx);
        let p0 = self.player_bit(0, xx);
        let p1 = self.player_bit(1, xx);
        let m0 = self.missile_bit(0, xx);
        let m1 = self.missile_bit(1, xx);

        // collision latches
        let cx = &mut self.collisions;
        let mut hit = |reg: usize, bit: u8, a: bool, b: bool| if a && b { cx[reg] |= bit };
        hit(0, 0x80, m0, p1); hit(0, 0x40, m0, p0);
        hit(1, 0x80, m1, p0); hit(1, 0x40, m1, p1);
        hit(2, 0x80, p0, pf); hit(2, 0x40, p0, bl);
        hit(3, 0x80, p1, pf); hit(3, 0x40, p1, bl);
        hit(4, 0x80, m0, pf); hit(4, 0x40, m0, bl);
        hit(5, 0x80, m1, pf); hit(5, 0x40, m1, bl);
        hit(6, 0x80, bl, pf);
        hit(7, 0x80, p0, p1); hit(7, 0x40, m0, m1);

        if (self.line as usize) >= FB_LINES { return; }
        if self.vblank & 0x02 == 0 {
            let l = self.line;
            self.vis = Some(self.vis.map_or((l, l), |(a, b)| (a.min(l), b.max(l))));
        }
        let color = if self.vblank & 0x02 != 0 || (self.hmove_blank && x < 8) {
            None
        } else {
            // score mode: the playfield takes the player colors (left half P0, right half P1)
            let pf_color = if self.ctrlpf & 0x02 != 0 { self.colup[if x < 80 { 0 } else { 1 }] } else { self.colupf };
            let field = if pf { Some(pf_color) } else if bl { Some(self.colupf) } else { None };
            let p0c = if p0 || m0 { Some(self.colup[0]) } else { None };
            let p1c = if p1 || m1 { Some(self.colup[1]) } else { None };
            let c = if self.ctrlpf & 0x04 != 0 {
                field.or(p0c).or(p1c)            // playfield/ball in front
            } else {
                p0c.or(p1c).or(field)
            };
            Some(c.unwrap_or(self.colubk))
        };
        let [r, g, b] = color.map(rgb).unwrap_or([0, 0, 0]);
        let o = (self.line as usize * SCREEN_W + x) * 4;
        self.framebuffer[o..o + 4].copy_from_slice(&[r, g, b, 0xFF]);
    }

    /// Atari 7800: there the TIA only makes sound (MARIA does the picture). Advance its audio
    /// by `tia_clocks` color clocks (3.58 MHz) without drawing anything.
    pub fn run_audio(&mut self, tia_clocks: u32) {
        self.audio_clocks += tia_clocks;
        while self.audio_clocks >= CLOCKS_PER_LINE as u32 / 2 {
            self.audio_clocks -= CLOCKS_PER_LINE as u32 / 2;
            self.clock_audio();
        }
    }

    fn clock_audio(&mut self) {
        self.audio[0].clock();
        self.audio[1].clock();
        self.audio_acc += (self.audio[0].level() + self.audio[1].level()) / 30.0;
        self.audio_n += 1;
        self.audio_phase += self.out_rate;
        while self.audio_phase >= AUDIO_RATE {
            self.audio_phase -= AUDIO_RATE;
            let x = if self.audio_n > 0 { self.audio_acc / self.audio_n as f32 } else { self.dc.0 };
            self.audio_acc = 0.0;
            self.audio_n = 0;
            // DC blocker: TIA output is 0..1, games also play samples by changing the volume
            let y = x - self.dc.0 + 0.995 * self.dc.1;
            self.dc = (x, y);
            let s = y * 0.8;
            self.audio_out.push(s);
            self.audio_out.push(s);
        }
    }

    /// Tests that move the beam by hand (hpos = 0) skip the real line end; this does its part.
    #[cfg(test)]
    pub(crate) fn new_line_for_test(&mut self) {
        self.hpos = 0;
        self.reset_this_line_p = [false; 2];
        self.reset_this_line_m = [false; 2];
    }

    #[cfg(test)]
    pub(crate) fn object_positions(&self) -> ([u16; 2], [u16; 2], u16) { (self.pos_p, self.pos_m, self.pos_bl) }
}
