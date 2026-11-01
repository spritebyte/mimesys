//! ANTIC and GTIA: the 5200's (and Atari 8-bit computers') picture.
//!
//! ANTIC (D400) reads a display list: each instruction is a "mode line" of 1-16 scan lines.
//!   bits 0-3  0 = blank lines (bits 4-6 + 1 of them), 1 = jump (bit 6: and wait for vblank),
//!             2-F = a graphics mode
//!   bit 7 DLI (NMI on the mode line's last scan line), bit 6 LMS (2 address bytes follow),
//!   bit 5 vertical scroll, bit 4 horizontal scroll.
//! Modes 2-7 are characters (names from the screen data, shapes from CHBASE), 8-F maps.
//! DMACTL: 0-1 playfield width (narrow 128 / normal 160 / wide 192 colour clocks),
//! 2 missiles, 3 players, 4 one-line player resolution, 5 display list DMA.
//! NMIEN 7 DLI / 6 VBI; NMIST reads which happened, NMIRES clears; WSYNC halts the CPU to
//! the end of the line; VCOUNT = line / 2.
//!
//! GTIA (C000) mixes the playfield with 4 players and 4 missiles (8 / 2 bits wide, 1x 2x 4x),
//! by PRIOR's priority rules, collects collisions, and turns colour bytes (hue 4-7,
//! luminance 1-3) into RGB. Hi-res modes (2, 3, F) draw PF2's hue at PF1's luminance.
//!
//! A line is drawn in one go (at CPU cycle 24 of the line), from the registers then; the
//! picture covers colour clocks 32-223 (384 hi-res pixels) of lines 8-247.

use serde::{Deserialize, Serialize};

pub const OUT_W: usize = 384;
pub const OUT_H: usize = 240;
pub const FIRST_LINE: u32 = 8;
/// colour clock at the picture's left edge
const LEFT_CC: i32 = 32;

/// What the picture reads memory through.
pub trait VideoMem { fn peek(&self, a: u16) -> u8; }

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Antic {
    pub dmactl: u8,
    pub chactl: u8,
    pub dlist: u16,
    pub hscrol: u8,
    pub vscrol: u8,
    pub pmbase: u8,
    pub chbase: u8,
    pub nmien: u8,
    pub nmist: u8,
    // the current mode line
    mode: u8,
    instr: u8,
    /// scan line within the mode line, and the last one
    row: u8,
    last_row: u8,
    /// memory scan counter (screen data address)
    pub scan: u16,
    /// address of this mode line's data
    line_data: u16,
    /// waiting for vertical blank after a JVB
    jvb: bool,
    prev_vscroll: bool,
}

/// Mode table: (scan lines, bytes at normal width, colour clocks per byte).
fn mode_info(mode: u8) -> (u8, u16, u8) {
    match mode {
        2 => (8, 40, 4), 3 => (10, 40, 4), 4 => (8, 40, 4), 5 => (16, 40, 4),
        6 => (8, 20, 8), 7 => (16, 20, 8), 8 => (8, 10, 16), 9 => (4, 10, 16),
        0xA => (4, 20, 8), 0xB => (2, 20, 8), 0xC => (1, 20, 8), 0xD => (2, 40, 4),
        0xE => (1, 40, 4), _ => (1, 40, 4),
    }
}

impl Antic {
    fn width(&self) -> u8 { self.dmactl & 3 }

    /// Bytes ANTIC fetches for a mode line at width `w` (1 narrow, 2 normal, 3 wide).
    fn fetch_bytes(mode: u8, w: u8) -> u16 {
        let (_, normal, _) = mode_info(mode);
        match w { 1 => normal * 4 / 5, 2 => normal, 3 => normal * 6 / 5, _ => 0 }
    }

    fn dl_byte(&mut self, mem: &dyn VideoMem) -> u8 {
        let v = mem.peek(self.dlist);
        // the display list counter wraps inside a 1 KB block
        self.dlist = (self.dlist & 0xFC00) | (self.dlist.wrapping_add(1) & 0x03FF);
        v
    }

    /// Start of scan line `line`: fetch the next instruction if a mode line ended. Returns
    /// (DMA cycles stolen from the CPU, whether a DLI fires on this line).
    pub fn start_line(&mut self, line: u32, mem: &dyn VideoMem) -> (u32, bool) {
        let mut stolen = 9;                                    // memory refresh
        if line == FIRST_LINE {
            self.jvb = false;
            self.row = 0;
            self.last_row = 0;
            self.mode = 0;
            self.instr = 0;
            self.prev_vscroll = false;
            // a new display starts with a fresh instruction
            self.row = u8::MAX;
        }
        if !(FIRST_LINE..FIRST_LINE + OUT_H as u32).contains(&line) { return (stolen, false); }
        if self.dmactl & 0x20 == 0 || self.jvb {
            self.mode = 0;
            return (stolen, false);
        }
        let new_line = self.row == u8::MAX || self.row >= self.last_row;
        if new_line {
            let instr = self.dl_byte(mem);
            stolen += 1;
            self.instr = instr;
            let mode = instr & 0x0F;
            self.row = 0;
            match mode {
                0 => { self.mode = 0; self.last_row = (instr >> 4) & 7; }
                1 => {
                    let lo = self.dl_byte(mem) as u16;
                    let hi = self.dl_byte(mem) as u16;
                    stolen += 2;
                    self.dlist = lo | hi << 8;
                    self.mode = 0;
                    self.last_row = 0;
                    if instr & 0x40 != 0 { self.jvb = true; }
                }
                _ => {
                    if instr & 0x40 != 0 {
                        let lo = self.dl_byte(mem) as u16;
                        let hi = self.dl_byte(mem) as u16;
                        stolen += 2;
                        self.scan = lo | hi << 8;
                    }
                    self.mode = mode;
                    let (lines, _, _) = mode_info(mode);
                    let vs = instr & 0x20 != 0;
                    // vertical scrolling: the first scrolled line starts at VSCROL, the
                    // line after the scrolled region ends at it
                    let first = if vs && !self.prev_vscroll { self.vscrol & 15 } else { 0 };
                    let last = if !vs && self.prev_vscroll { self.vscrol & 15 } else { lines - 1 };
                    self.prev_vscroll = vs;
                    self.row = first.min(15);
                    self.last_row = last.max(self.row);
                    self.line_data = self.scan;
                    let w = self.fetch_width();
                    let n = Self::fetch_bytes(mode, w);
                    self.scan = (self.scan & 0xF000) | (self.scan.wrapping_add(n) & 0x0FFF);
                    stolen += n as u32;
                }
            }
        } else {
            self.row += 1;
        }
        // character modes read their shapes on every line
        if (2..=7).contains(&self.mode) && self.row != 0 {
            stolen += Self::fetch_bytes(self.mode, self.fetch_width()) as u32;
        }
        if self.dmactl & 0x0C != 0 { stolen += if self.dmactl & 0x08 != 0 { 5 } else { 1 }; }
        let dli = self.row == self.last_row && self.instr & 0x80 != 0;
        (stolen.min(110), dli)
    }

    /// Width actually fetched: horizontal scrolling reads one size wider.
    fn fetch_width(&self) -> u8 {
        let w = self.width();
        if self.instr & 0x10 != 0 && w != 0 && w < 3 { w + 1 } else { w }
    }

    /// Playfield of the current scan line: per hi-res pixel, 0 = background, 1-4 = PF0-PF3,
    /// 5 = hi-res "on" (PF2's hue, PF1's luminance; a PF2 pixel for collisions).
    pub fn playfield(&self, mem: &dyn VideoMem, out: &mut [u8; OUT_W]) {
        out.fill(0);
        let w = self.width();
        if self.mode < 2 || w == 0 || self.dmactl & 0x20 == 0 { return; }
        let mode = self.mode;
        let (lines, _, cc_per_byte) = mode_info(mode);
        let fw = self.fetch_width();
        let n = Self::fetch_bytes(mode, fw) as i32;
        // where the fetched data starts, in colour clocks; scrolled lines move right
        let start_cc = match fw { 1 => 64, 2 => 48, _ => 32 } + if self.instr & 0x10 != 0 { (self.hscrol & 15) as i32 } else { 0 };
        let (win_l, win_r) = match w { 1 => (64, 192), 2 => (48, 208), _ => (32, 224) };
        let row = self.row as u16;
        let chbase = if mode >= 6 && mode <= 7 { (self.chbase as u16 & 0xFE) << 8 } else { (self.chbase as u16 & 0xFC) << 8 };
        let mut put = |cc: i32, half: i32, v: u8, out: &mut [u8; OUT_W]| {
            // `half`: 0 = whole colour clock, 1 / 2 = its left / right hi-res half
            if cc < win_l || cc >= win_r { return; }
            let x = (cc - LEFT_CC) * 2;
            if x < 0 || x as usize + 1 >= OUT_W + 1 { return; }
            let x = x as usize;
            match half {
                0 => { out[x] = v; if x + 1 < OUT_W { out[x + 1] = v; } }
                1 => out[x] = v,
                _ => if x + 1 < OUT_W { out[x + 1] = v },
            }
        };
        for i in 0..n {
            let a = (self.line_data & 0xF000) | (self.line_data.wrapping_add(i as u16) & 0x0FFF);
            let b = mem.peek(a);
            let cc0 = start_cc + i * cc_per_byte as i32;
            match mode {
                2 | 3 | 4 | 5 => {
                    // character shape row
                    let r = match mode {
                        5 => row / 2,
                        3 => {
                            // 10 lines: characters 60-7F drop their top two rows to the bottom
                            if b & 0x60 == 0x60 { if row < 2 { 0xFF } else if row >= 8 { row - 8 } else { row } }
                            else if row >= 8 { 0xFF } else { row }
                        }
                        _ => row,
                    };
                    let r = if self.chactl & 4 != 0 && r != 0xFF { 7 - (r & 7) } else { r };
                    let mut d = if r == 0xFF { 0 } else { mem.peek(chbase + (b as u16 & 0x7F) * 8 + (r & 7)) };
                    if mode <= 3 {
                        if b & 0x80 != 0 {
                            if self.chactl & 1 != 0 { d = 0; }
                            if self.chactl & 2 != 0 { d ^= 0xFF; }
                        }
                        for k in 0..8 {
                            let on = d & (0x80 >> k) != 0;
                            put(cc0 + k / 2, 1 + (k & 1), if on { 5 } else { 3 }, out);
                        }
                    } else {
                        for k in 0..4 {
                            let v = (d >> (6 - k * 2)) & 3;
                            let c = match v { 0 => 0, 1 => 1, 2 => 2, _ => if b & 0x80 != 0 { 4 } else { 3 } };
                            put(cc0 + k, 0, c, out);
                        }
                    }
                }
                6 | 7 => {
                    let r = if mode == 7 { row / 2 } else { row };
                    let r = if self.chactl & 4 != 0 { 7 - (r & 7) } else { r };
                    let d = mem.peek(chbase + (b as u16 & 0x3F) * 8 + (r & 7));
                    let c = (b >> 6) + 1;
                    for k in 0..8 { put(cc0 + k, 0, if d & (0x80 >> k) != 0 { c } else { 0 }, out); }
                }
                0x9 | 0xB | 0xC => {
                    // 1 bit per pixel, PF0 on background
                    let px = cc_per_byte as i32 / 8;
                    for k in 0..8 {
                        let c = if b & (0x80 >> k) != 0 { 1 } else { 0 };
                        for j in 0..px { put(cc0 + k * px + j, 0, c, out); }
                    }
                }
                0xF => {
                    for k in 0..8 {
                        put(cc0 + k / 2, 1 + (k & 1), if b & (0x80 >> k) != 0 { 5 } else { 3 }, out);
                    }
                }
                _ => {
                    // 8, A, D, E: 2 bits per pixel
                    let px = cc_per_byte as i32 / 4;
                    for k in 0..4 {
                        let c = (b >> (6 - k * 2)) & 3;
                        for j in 0..px { put(cc0 + k * px + j, 0, c, out); }
                    }
                }
            }
        }
        // hi-res modes fill their whole window with PF2, even where no data was fetched
        if matches!(mode, 2 | 3 | 0xF) {
            for cc in win_l..win_r {
                let x = ((cc - LEFT_CC) * 2) as usize;
                for j in 0..2 { if x + j < OUT_W && out[x + j] == 0 { out[x + j] = 3; } }
            }
        }
        let _ = lines;
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Gtia {
    pub hpos: [u8; 8],                 // players 0-3, missiles 0-3
    pub sizep: [u8; 4],
    pub sizem: u8,
    pub grafp: [u8; 4],
    pub grafm: u8,
    pub colpm: [u8; 4],
    pub colpf: [u8; 4],
    pub colbk: u8,
    pub prior: u8,
    pub vdelay: u8,
    pub gractl: u8,
    pub consol: u8,
    // collisions
    pub m_pf: [u8; 4],
    pub p_pf: [u8; 4],
    pub m_pl: [u8; 4],
    pub p_pl: [u8; 4],
    /// fire buttons (true = pressed) and the latched state (GRACTL bit 2)
    pub trig: [bool; 4],
    trig_latch: [bool; 4],
}

impl Gtia {
    pub fn read(&mut self, r: u8) -> u8 {
        match r & 0x1F {
            o @ 0x00..=0x03 => self.m_pf[o as usize],
            o @ 0x04..=0x07 => self.p_pf[o as usize - 4],
            o @ 0x08..=0x0B => self.m_pl[o as usize - 8],
            o @ 0x0C..=0x0F => self.p_pl[o as usize - 12],
            o @ 0x10..=0x13 => {
                let i = o as usize - 0x10;
                let down = self.trig[i] || (self.gractl & 4 != 0 && self.trig_latch[i]);
                if down { 0 } else { 1 }
            }
            0x14 => 0x0F,                                  // NTSC
            0x1F => 0x0F,
            _ => 0x0F,
        }
    }

    pub fn write(&mut self, r: u8, v: u8) {
        match r & 0x1F {
            o @ 0x00..=0x07 => self.hpos[o as usize] = v,
            o @ 0x08..=0x0B => self.sizep[o as usize - 8] = v & 3,
            0x0C => self.sizem = v,
            o @ 0x0D..=0x10 => self.grafp[o as usize - 0x0D] = v,
            0x11 => self.grafm = v,
            o @ 0x12..=0x15 => self.colpm[o as usize - 0x12] = v & 0xFE,
            o @ 0x16..=0x19 => self.colpf[o as usize - 0x16] = v & 0xFE,
            0x1A => self.colbk = v & 0xFE,
            0x1B => self.prior = v,
            0x1C => self.vdelay = v,
            0x1D => { self.gractl = v; if v & 4 == 0 { self.trig_latch = [false; 4]; } }
            0x1E => { self.m_pf = [0; 4]; self.p_pf = [0; 4]; self.m_pl = [0; 4]; self.p_pl = [0; 4]; }
            _ => self.consol = v,
        }
    }

    /// Fire buttons for this frame (the latch keeps a press until GRACTL bit 2 clears).
    pub fn set_triggers(&mut self, t: [bool; 4]) {
        self.trig = t;
        for i in 0..4 { if t[i] { self.trig_latch[i] = true; } }
    }

    /// Load this line's player / missile graphics by DMA (when ANTIC and GRACTL allow).
    pub fn pm_dma(&mut self, antic: &Antic, line: u32, mem: &dyn VideoMem) {
        let one = antic.dmactl & 0x10 != 0;
        let (base, l) = if one { ((antic.pmbase as u16 & 0xF8) << 8, line as u16) } else { ((antic.pmbase as u16 & 0xFC) << 8, line as u16 / 2) };
        if antic.dmactl & 0x04 != 0 && self.gractl & 1 != 0 {
            let off = if one { 0x300 } else { 0x180 };
            self.grafm = mem.peek(base + off + l);
        }
        if antic.dmactl & 0x08 != 0 && self.gractl & 2 != 0 {
            for p in 0..4u16 {
                let off = if one { 0x400 + p * 0x100 } else { 0x200 + p * 0x80 };
                self.grafp[p as usize] = mem.peek(base + off + l);
            }
        }
    }

    /// Mix one line: playfield codes from ANTIC plus players and missiles, into colour bytes.
    pub fn compose(&mut self, pf: &[u8; OUT_W], out: &mut [u8; OUT_W]) {
        // which players / missiles cover each colour clock
        let mut pl = [0u8; OUT_W / 2];
        let mut ms = [0u8; OUT_W / 2];
        let width = |s: u8| match s & 3 { 1 => 2, 3 => 4, _ => 1 };
        for p in 0..4 {
            let g = self.grafp[p];
            if g == 0 { continue; }
            let w = width(self.sizep[p]);
            for bit in 0..8 {
                if g & (0x80 >> bit) == 0 { continue; }
                for j in 0..w {
                    let cc = self.hpos[p] as i32 + bit * w + j - LEFT_CC;
                    if (0..(OUT_W / 2) as i32).contains(&cc) { pl[cc as usize] |= 1 << p; }
                }
            }
        }
        for m in 0..4 {
            let g = (self.grafm >> (m * 2)) & 3;
            if g == 0 { continue; }
            let w = width(self.sizem >> (m * 2));
            for bit in 0..2 {
                if g & (2 >> bit) == 0 { continue; }
                for j in 0..w {
                    let cc = self.hpos[4 + m] as i32 + bit * w + j - LEFT_CC;
                    if (0..(OUT_W / 2) as i32).contains(&cc) { ms[cc as usize] |= 1 << m; }
                }
            }
        }
        let prio = self.prior & 0x0F;
        let fifth = self.prior & 0x10 != 0;
        let multi = self.prior & 0x20 != 0;
        let hires_on = (self.colpf[2] & 0xF0) | (self.colpf[1] & 0x0E);
        for x in 0..OUT_W {
            let cc = x / 2;
            let (p, m) = (pl[cc], ms[cc]);
            let mut code = pf[x];
            // collisions (hi-res pixels count as PF2)
            let pf_bit = match code { 1..=4 => 1u8 << (code - 1), 5 => 4, _ => 0 };
            if x & 1 == 0 {
                for i in 0..4 {
                    if p & (1 << i) != 0 { self.p_pf[i] |= pf_bit; self.p_pl[i] |= p & !(1 << i); }
                    if m & (1 << i) != 0 { self.m_pf[i] |= pf_bit; self.m_pl[i] |= p; }
                }
            }
            // the fifth player: missiles drawn as PF3
            let mut players = p;
            if m != 0 {
                if fifth { code = 4; } else { players |= m; }
            }
            let pf_color = match code { 0 => self.colbk, 5 => hires_on, c => self.colpf[(c - 1) as usize] };
            if players == 0 { out[x] = pf_color; continue; }
            let pc = if multi {
                let a = if players & 3 != 0 { (if players & 1 != 0 { self.colpm[0] } else { 0 }) | (if players & 2 != 0 { self.colpm[1] } else { 0 }) } else { 0 };
                if a != 0 { a } else { (if players & 4 != 0 { self.colpm[2] } else { 0 }) | (if players & 8 != 0 { self.colpm[3] } else { 0 }) }
            } else {
                self.colpm[players.trailing_zeros() as usize]
            };
            let group_a = players & 3 != 0;
            let pf_is_a = matches!(code, 1 | 2);
            let player_wins = match code {
                0 => true,
                _ if prio & 4 != 0 => false,
                _ if prio & 8 != 0 => !pf_is_a,
                _ if prio & 2 != 0 => group_a,
                _ => true,
            };
            out[x] = if player_wins { pc } else { pf_color };
        }
    }
}

/// NTSC colours: hue 0 grey, 1-15 around the colour wheel from gold; 8 luminances.
pub fn palette() -> Vec<[u8; 3]> {
    (0..256u32).map(|c| {
        let hue = c >> 4;
        let lum = (c >> 1) & 7;
        let y = 0.04 + lum as f32 / 7.0 * 0.92;
        let (i, q) = if hue == 0 { (0.0, 0.0) } else {
            let a = (-30.0 + (hue - 1) as f32 * 24.0).to_radians();
            (0.22 * a.cos(), 0.22 * a.sin())
        };
        let r = y + 0.956 * i + 0.621 * q;
        let g = y - 0.272 * i - 0.647 * q;
        let b = y - 1.106 * i + 1.703 * q;
        let f = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        [f(r), f(g), f(b)]
    }).collect()
}
