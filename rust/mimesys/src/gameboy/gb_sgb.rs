//! Super Game Boy: the SNES side of the SGB, as seen by a Game Boy game (Pan Docs "SGB
//! Functions").
//!
//! * Command packets come in through the joypad select lines (P14 / P15): both low =
//!   reset (start of a packet), P14 low = a 0 bit, P15 low = a 1 bit, both high between
//!   bits; 128 bits (16 bytes, LSB first) and a 0 stop bit. The first byte of a command
//!   is code * 8 + number of packets (1-7).
//! * The picture is 256 x 224: the Game Boy screen at (48, 40) inside a border. Each
//!   8x8 cell of the Game Boy screen uses one of 4 palettes (attributes); the Game Boy's
//!   shades 0-3 (after BGP / OBP) pick a colour from it. Colour 0 is shared by all.
//! * "_TRN" commands send 4 KB through the screen itself: the game shows tiles 00-FF in
//!   order with BGP = E4 and the SNES reads them off the next frame.
//! * MLT_REQ: 2 or 4 controllers; with both select lines high the low nibble of P1 reads
//!   F - player, and the player advances when P15 goes high.
//!
//! Until a game sends its own border, SameBoy's default border is shown (its own artwork,
//! Expat/MIT licence: bootroms/sgb_border.bin, made by tools/gb_bootroms/sgb_border_dump.c).
//!
//! Not emulated: SNES sound (SOUND / SOU_TRN), SNES code (DATA_SND / DATA_TRN / JUMP),
//! SNES sprites (OBJ_TRN), Nintendo's built-in borders and palettes menu.

use serde::{Deserialize, Serialize};

pub const SGB_W: usize = 256;
pub const SGB_H: usize = 224;
const GB_X: usize = 48;
const GB_Y: usize = 40;

/// The SGB's default palette ("1-A"): cream, orange, red, dark blue.
const DEFAULT_PAL: [u16; 4] = [0x67BF, 0x265B, 0x10B5, 0x2866];

/// Default border: 32 x 28 map (u16 LE), one 16-colour palette, 256 tiles (SNES 4bpp).
static DEFAULT_BORDER: &[u8] = include_bytes!("bootroms/sgb_border.bin");

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
enum Transfer { Pal, Attr, Chr(u8), Pct }

#[derive(Serialize, Deserialize)]
pub struct Sgb {
    /// header byte 146 = 03 and 14B = 33: the game may use SGB functions
    pub unlocked: bool,
    // ---- packet receiver
    lines: u8,          // last P1 bits 4-5
    receiving: bool,
    bit: usize,         // bits received in this packet (0-127, 128 = waiting for stop)
    packet: [u8; 16],
    command: Vec<u8>,   // packets of the current command, 16 bytes each
    // ---- display state
    pal: [[u16; 4]; 4],
    attr: Vec<u8>,      // 20 x 18 palette numbers
    #[serde(with = "serde_bytes")]
    sys_pal: Vec<u8>,   // 512 palettes x 4 colours (PAL_TRN)
    #[serde(with = "serde_bytes")]
    atf: Vec<u8>,       // 45 attribute files x 90 bytes (ATTR_TRN)
    #[serde(with = "serde_bytes")]
    border_chr: Vec<u8>, // 256 tiles x 32 bytes, SNES 4bpp
    #[serde(with = "serde_bytes")]
    border_map: Vec<u8>, // 32 x 28 entries, u16 LE
    border_pal: [[u16; 16]; 3], // SNES palettes 4-6
    mask: u8,
    pending: Option<(Transfer, u8)>,
    // ---- multiplayer
    pub players: u8,    // 1, 2 or 4
    pub player: u8,     // 0-based, the one P1 reads now
    // ---- output
    #[serde(skip, default = "blank_screen")]
    frozen: Vec<u8>,    // GB screen RGBA kept while the mask freezes it
    #[serde(skip, default = "blank_screen")]
    pub framebuffer: Vec<u8>,
    /// commands seen (for the tests and the log)
    #[serde(skip)]
    pub log: Vec<u8>,
}

fn blank_screen() -> Vec<u8> { vec![0; SGB_W * SGB_H * 4] }

fn rgb(c: u16) -> [u8; 4] {
    let x = |v: u16| { let v = (v & 0x1F) as u8; (v << 3) | (v >> 2) };
    [x(c), x(c >> 5), x(c >> 10), 0xFF]
}

fn le16(d: &[u8], i: usize) -> u16 { d[i] as u16 | (d[i + 1] as u16) << 8 }

impl Sgb {
    pub fn new(unlocked: bool) -> Self {
        let mut s = Self::blank(unlocked);
        let d = DEFAULT_BORDER;
        s.border_map.copy_from_slice(&d[..32 * 28 * 2]);
        for c in 0..16 { s.border_pal[0][c] = le16(d, 32 * 28 * 2 + c * 2); }
        let tiles = &d[32 * 28 * 2 + 32..];
        s.border_chr.copy_from_slice(&tiles[..256 * 32]);
        s
    }

    /// No border at all (tests).
    pub fn blank(unlocked: bool) -> Self {
        Self {
            unlocked,
            lines: 0x30, receiving: false, bit: 0, packet: [0; 16], command: Vec::new(),
            pal: [DEFAULT_PAL; 4],
            attr: vec![0; 20 * 18],
            sys_pal: vec![0; 512 * 8],
            atf: vec![0; 45 * 90],
            border_chr: vec![0; 256 * 32],
            border_map: vec![0; 32 * 28 * 2],
            border_pal: [[0; 16]; 3],
            mask: 0,
            pending: None,
            players: 1, player: 0,
            frozen: blank_screen(),
            framebuffer: blank_screen(),
            log: Vec::new(),
        }
    }

    /// Whether the cartridge header unlocks SGB functions.
    pub fn header_unlocks(rom: &[u8]) -> bool {
        rom.get(0x146) == Some(&0x03) && rom.get(0x14B) == Some(&0x33)
    }

    /// After loading a state: the screen buffers aren't saved.
    pub fn keep_static(&mut self, old: &mut Self) {
        self.framebuffer = std::mem::take(&mut old.framebuffer);
        self.frozen = std::mem::take(&mut old.frozen);
    }

    // ------------------------------------------------------------------ joypad port

    /// A write to P1 (FF00): bits 4 and 5 are the select lines.
    pub fn p1_write(&mut self, value: u8) {
        let new = value & 0x30;
        let old = self.lines;
        self.lines = new;
        if new == old { return; }
        // multiplayer: P15 going high selects the next controller
        if self.players > 1 && old & 0x20 == 0 && new & 0x20 != 0 {
            self.player = (self.player + 1) & (self.players - 1);
        }
        match new {
            0x00 => {
                // reset pulse: a new packet starts
                self.receiving = true;
                self.bit = 0;
                self.packet = [0; 16];
            }
            0x10 | 0x20 if self.receiving && old == 0x30 => {
                let one = new == 0x10;      // P15 (bit 5) low = 1, P14 (bit 4) low = 0
                if self.bit < 128 {
                    if one { self.packet[self.bit / 8] |= 1 << (self.bit % 8); }
                    self.bit += 1;
                } else {
                    // stop bit (should be 0)
                    self.receiving = false;
                    if !one { self.packet_done(); }
                }
            }
            _ => {}
        }
    }

    /// With both select lines high in multiplayer mode, P1's low nibble is the player ID.
    pub fn p1_id(&self) -> Option<u8> {
        if self.players > 1 && self.lines == 0x30 { Some(0x0F - self.player) } else { None }
    }

    fn packet_done(&mut self) {
        self.command.extend_from_slice(&self.packet);
        // the boot ROM sends the cartridge header as single packets F1, F3 .. FB (codes
        // 1E / 1F): their low bits are a packet number, not a length
        let packets = if self.command[0] >= 0xF0 { 1 } else { (self.command[0] & 7).max(1) as usize };
        if self.command.len() >= packets * 16 {
            let cmd = std::mem::take(&mut self.command);
            self.execute(&cmd);
        }
    }

    // ------------------------------------------------------------------ commands

    fn execute(&mut self, d: &[u8]) {
        let code = d[0] >> 3;
        self.log.push(code);
        // MLT_REQ works even when the game didn't unlock the rest (SGB detection)
        if code == 0x11 {
            self.players = match d[1] & 3 { 1 => 2, 3 => 4, _ => 1 };
            self.player = 0;
            return;
        }
        if !self.unlocked { return; }
        match code {
            0x00 => self.pal_pair(d, 0, 1),
            0x01 => self.pal_pair(d, 2, 3),
            0x02 => self.pal_pair(d, 0, 3),
            0x03 => self.pal_pair(d, 1, 2),
            0x04 => self.attr_blk(d),
            0x05 => self.attr_lin(d),
            0x06 => self.attr_div(d),
            0x07 => self.attr_chr(d),
            0x0A => self.pal_set(d),
            0x0B => self.pending = Some((Transfer::Pal, 1)),
            0x13 => self.pending = Some((Transfer::Chr(d[1] & 1), 1)),
            0x14 => self.pending = Some((Transfer::Pct, 1)),
            0x15 => self.pending = Some((Transfer::Attr, 1)),
            0x16 => self.attr_set(d[1]),
            0x17 => self.mask = d[1] & 3,
            _ => {}   // sound, SNES RAM / code, OBJ mode, menus: not emulated
        }
    }

    fn set_color0(&mut self, c: u16) {
        for p in &mut self.pal { p[0] = c; }
    }

    fn pal_pair(&mut self, d: &[u8], a: usize, b: usize) {
        self.set_color0(le16(d, 1));
        for i in 0..3 {
            self.pal[a][i + 1] = le16(d, 3 + i * 2);
            self.pal[b][i + 1] = le16(d, 9 + i * 2);
        }
    }

    fn pal_set(&mut self, d: &[u8]) {
        for i in 0..4 {
            let id = (le16(d, 1 + i * 2) & 0x1FF) as usize;
            for c in 0..4 {
                self.pal[i][c] = le16(&self.sys_pal, id * 8 + c * 2);
            }
        }
        let c0 = self.pal[0][0];
        self.set_color0(c0);
        let flags = d[9];
        if flags & 0x80 != 0 { self.attr_set(flags & 0x3F); }
        if flags & 0x40 != 0 { self.mask = 0; }
    }

    fn attr_set(&mut self, v: u8) {
        let file = (v & 0x3F) as usize;
        if file < 45 {
            for y in 0..18 {
                for x in 0..20 {
                    let b = self.atf[file * 90 + y * 5 + x / 4];
                    self.attr[y * 20 + x] = (b >> (6 - 2 * (x % 4))) & 3;
                }
            }
        }
        if v & 0x40 != 0 { self.mask = 0; }
    }

    fn attr_blk(&mut self, d: &[u8]) {
        let sets = (d[1] as usize).min(18);
        for s in 0..sets {
            let o = 2 + s * 6;
            if o + 6 > d.len() { break; }
            let mut ctrl = d[o] & 7;
            let pals = d[o + 1];
            let (inside, mut line, outside) = (pals & 3, (pals >> 2) & 3, (pals >> 4) & 3);
            let (x1, y1, x2, y2) = (d[o + 2] as usize, d[o + 3] as usize, d[o + 4] as usize, d[o + 5] as usize);
            // only inside or only outside: the surrounding line follows it
            if ctrl == 1 { ctrl |= 2; line = inside; }
            if ctrl == 4 { ctrl |= 2; line = outside; }
            for y in 0..18 {
                for x in 0..20 {
                    let in_box = x >= x1 && x <= x2 && y >= y1 && y <= y2;
                    let on_line = in_box && (x == x1 || x == x2 || y == y1 || y == y2);
                    let a = &mut self.attr[y * 20 + x];
                    if on_line {
                        if ctrl & 2 != 0 { *a = line; }
                    } else if in_box {
                        if ctrl & 1 != 0 { *a = inside; }
                    } else if ctrl & 4 != 0 {
                        *a = outside;
                    }
                }
            }
        }
    }

    fn attr_lin(&mut self, d: &[u8]) {
        let sets = d[1] as usize;
        for s in 0..sets {
            let Some(&v) = d.get(2 + s) else { break };
            let n = (v & 0x1F) as usize;
            let p = (v >> 5) & 3;
            if v & 0x80 != 0 {
                if n < 18 { for x in 0..20 { self.attr[n * 20 + x] = p; } }
            } else if n < 20 {
                for y in 0..18 { self.attr[y * 20 + n] = p; }
            }
        }
    }

    fn attr_div(&mut self, d: &[u8]) {
        let (right, left, line) = (d[1] & 3, (d[1] >> 2) & 3, (d[1] >> 4) & 3);
        let horizontal = d[1] & 0x40 != 0;
        let c = d[2] as usize;
        for y in 0..18 {
            for x in 0..20 {
                let v = if horizontal { y } else { x };
                self.attr[y * 20 + x] = if v < c { left } else if v == c { line } else { right };
            }
        }
    }

    fn attr_chr(&mut self, d: &[u8]) {
        let (mut x, mut y) = ((d[1] as usize).min(19), (d[2] as usize).min(17));
        let count = (le16(d, 3) as usize).min(360);
        let vertical = d[5] & 1 != 0;
        for i in 0..count {
            let Some(&b) = d.get(6 + i / 4) else { break };
            self.attr[y * 20 + x] = (b >> (6 - 2 * (i % 4))) & 3;
            if vertical {
                y += 1;
                if y == 18 { y = 0; x = (x + 1) % 20; }
            } else {
                x += 1;
                if x == 20 { x = 0; y = (y + 1) % 18; }
            }
        }
    }

    // ------------------------------------------------------------------ frames

    /// One finished Game Boy frame: `shades` holds the 160 x 144 LCD shades (0-3).
    /// Runs any pending screen transfer and builds the 256 x 224 picture.
    pub fn end_frame(&mut self, shades: &[u8]) {
        if let Some((t, wait)) = self.pending {
            if wait > 0 {
                self.pending = Some((t, wait - 1));
            } else {
                self.pending = None;
                let data = Self::screen_data(shades);
                match t {
                    Transfer::Pal => self.sys_pal.copy_from_slice(&data),
                    Transfer::Attr => self.atf.copy_from_slice(&data[..45 * 90]),
                    Transfer::Chr(half) => {
                        let o = half as usize * 0x1000;
                        self.border_chr[o..o + 0x1000].copy_from_slice(&data);
                    }
                    Transfer::Pct => {
                        self.border_map.copy_from_slice(&data[..32 * 28 * 2]);
                        for p in 0..3 {
                            for c in 0..16 {
                                self.border_pal[p][c] = le16(&data, 0x800 + p * 32 + c * 2);
                            }
                        }
                    }
                }
            }
        }
        self.compose(shades);
    }

    /// 4 KB read off the screen: tiles 00-FF shown left to right, 20 a row, as 2bpp.
    fn screen_data(shades: &[u8]) -> Vec<u8> {
        let mut out = vec![0u8; 0x1000];
        for t in 0..256 {
            let (tx, ty) = (t % 20, t / 20);
            for r in 0..8 {
                let (mut lo, mut hi) = (0u8, 0u8);
                for px in 0..8 {
                    let s = shades.get((ty * 8 + r) * 160 + tx * 8 + px).copied().unwrap_or(0) & 3;
                    lo |= (s & 1) << (7 - px);
                    hi |= (s >> 1) << (7 - px);
                }
                out[t * 16 + r * 2] = lo;
                out[t * 16 + r * 2 + 1] = hi;
            }
        }
        out
    }

    fn border_pixel(&self, x: usize, y: usize) -> Option<u16> {
        let e = le16(&self.border_map, ((y / 8) * 32 + x / 8) * 2);
        if e & 0x300 != 0 { return None; }   // tile numbers past FF: nothing drawn
        let tile = (e & 0xFF) as usize;
        let pal = ((e >> 10) & 7) as usize;
        let (mut px, mut py) = (x % 8, y % 8);
        if e & 0x4000 != 0 { px = 7 - px; }
        if e & 0x8000 != 0 { py = 7 - py; }
        let t = &self.border_chr[tile * 32..tile * 32 + 32];
        let bit = |b: u8| (b >> (7 - px)) & 1;
        let c = bit(t[py * 2]) | bit(t[py * 2 + 1]) << 1 | bit(t[16 + py * 2]) << 2 | bit(t[16 + py * 2 + 1]) << 3;
        if c == 0 { return None; }
        Some(self.border_pal[pal.clamp(4, 6) - 4][c as usize])
    }

    fn compose(&mut self, shades: &[u8]) {
        let backdrop = rgb(self.pal[0][0]);
        let mut fb = std::mem::take(&mut self.framebuffer);
        // the Game Boy screen
        let frozen = self.mask == 1;
        for y in 0..144 {
            for x in 0..160 {
                let o = ((GB_Y + y) * SGB_W + GB_X + x) * 4;
                let px = match self.mask {
                    1 => { fb[o..o + 4].copy_from_slice(&self.frozen[o..o + 4]); continue; }
                    2 => [0, 0, 0, 0xFF],
                    3 => backdrop,
                    _ => {
                        let s = shades.get(y * 160 + x).copied().unwrap_or(0) & 3;
                        let p = self.attr[(y / 8) * 20 + x / 8] as usize;
                        rgb(self.pal[p][s as usize])
                    }
                };
                fb[o..o + 4].copy_from_slice(&px);
            }
        }
        // the border on top (colour 0 is see-through)
        for y in 0..SGB_H {
            for x in 0..SGB_W {
                let inside = (GB_X..GB_X + 160).contains(&x) && (GB_Y..GB_Y + 144).contains(&y);
                let o = (y * SGB_W + x) * 4;
                match self.border_pixel(x, y) {
                    Some(c) => fb[o..o + 4].copy_from_slice(&rgb(c)),
                    None if !inside => fb[o..o + 4].copy_from_slice(&backdrop),
                    None => {}
                }
            }
        }
        if !frozen {
            self.frozen.copy_from_slice(&fb);
        }
        self.framebuffer = fb;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sends one command through the joypad lines, like a game does.
    fn send(sgb: &mut Sgb, bytes: &[u8]) {
        for chunk in bytes.chunks(16) {
            let mut p = [0u8; 16];
            p[..chunk.len()].copy_from_slice(chunk);
            sgb.p1_write(0x00);
            sgb.p1_write(0x30);
            for i in 0..128 {
                let one = p[i / 8] >> (i % 8) & 1 != 0;
                sgb.p1_write(if one { 0x10 } else { 0x20 });
                sgb.p1_write(0x30);
            }
            sgb.p1_write(0x20);   // stop bit
            sgb.p1_write(0x30);
        }
    }

    fn px(s: &Sgb, x: usize, y: usize) -> [u8; 3] {
        let o = (y * SGB_W + x) * 4;
        [s.framebuffer[o], s.framebuffer[o + 1], s.framebuffer[o + 2]]
    }

    #[test]
    fn packets_palettes_and_attributes() {
        let mut s = Sgb::blank(true);
        // PAL01: colour 0 black, palette 0 = red/green/blue, palette 1 = white x3
        send(&mut s, &[0x01, 0x00, 0x00, 0x1F, 0x00, 0xE0, 0x03, 0x00, 0x7C,
                       0xFF, 0x7F, 0xFF, 0x7F, 0xFF, 0x7F, 0x00]);
        assert_eq!(s.log, vec![0]);
        // ATTR_BLK: one block, inside only, palette 1, tiles (2,2)-(5,4)
        send(&mut s, &[0x21, 0x01, 0x01, 0x01, 2, 2, 5, 4, 0, 0, 0, 0, 0, 0, 0, 0]);
        let mut shades = vec![1u8; 160 * 144];
        shades[0] = 0;
        s.end_frame(&shades);
        assert_eq!(px(&s, 48, 40), [0, 0, 0], "shade 0 = shared colour 0");
        assert_eq!(px(&s, 49, 40), [255, 0, 0], "palette 0 colour 1");
        assert_eq!(px(&s, 48 + 3 * 8, 40 + 3 * 8), [255, 255, 255], "inside the block: palette 1");
        assert_eq!(px(&s, 48 + 2 * 8, 40 + 2 * 8), [255, 255, 255], "the line follows the inside");
        assert_eq!(px(&s, 0, 0), [0, 0, 0], "plain border = backdrop");
    }

    #[test]
    fn locked_header_only_allows_mlt_req() {
        let mut s = Sgb::blank(false);
        send(&mut s, &[0x01, 0x00, 0x00, 0x1F, 0x00]);
        assert_eq!(s.pal[0][1], DEFAULT_PAL[1]);
        send(&mut s, &[0x89, 0x01]);   // MLT_REQ two players
        assert_eq!(s.players, 2);
        assert_eq!(s.p1_id(), Some(0x0F));
        s.p1_write(0x10);   // P15 low...
        s.p1_write(0x30);   // ...and high: next player
        assert_eq!(s.p1_id(), Some(0x0E));
        s.p1_write(0x10);
        s.p1_write(0x30);
        assert_eq!(s.p1_id(), Some(0x0F), "wraps around");
    }

    #[test]
    fn border_through_vram_transfer() {
        let mut s = Sgb::blank(true);
        // CHR_TRN tiles 00-7F: tile 1 = colour 1 everywhere (plane 0 set in every row)
        send(&mut s, &[0x99, 0x00]);
        let mut shades = vec![0u8; 160 * 144];
        // tile t at screen tile (t % 20, t / 20); the SNES tile 1 is GB tiles 2 (planes
        // 0-1) and 3 (planes 2-3): GB tile 2 rows = lo FF hi 00 -> shade 1
        for r in 0..8 { for p in 0..8 { shades[r * 160 + 2 * 8 + p] = 1; } }
        s.end_frame(&shades);           // the frame the command was sent in
        assert!(s.pending.is_some());
        s.end_frame(&shades);           // the next frame: read off the screen
        assert!(s.pending.is_none());
        assert_eq!(&s.border_chr[32..34], &[0xFF, 0x00]);
        // PCT_TRN: map entry 0 = tile 1, palette 4; palette 4 colour 1 = blue
        send(&mut s, &[0xA1, 0x00]);
        let mut data = vec![0u8; 0x1000];
        data[0] = 1;
        data[1] = 4 << 2;
        data[0x802] = 0x00;
        data[0x803] = 0x7C;
        let shades2 = data_to_screen(&data);
        s.end_frame(&shades2);
        s.end_frame(&shades2);
        assert_eq!(px(&s, 3, 3), [0, 0, 255], "border tile drawn");
        let b = rgb(DEFAULT_PAL[0]);
        assert_eq!(px(&s, 9, 3), [b[0], b[1], b[2]], "tile 0 is see-through: backdrop");
    }

    /// The inverse of screen_data: what the game shows to send `data`.
    fn data_to_screen(data: &[u8]) -> Vec<u8> {
        let mut shades = vec![0u8; 160 * 144];
        for t in 0..256 {
            for r in 0..8 {
                let (lo, hi) = (data[t * 16 + r * 2], data[t * 16 + r * 2 + 1]);
                for p in 0..8 {
                    let s = (lo >> (7 - p) & 1) | (hi >> (7 - p) & 1) << 1;
                    shades[((t / 20) * 8 + r) * 160 + (t % 20) * 8 + p] = s;
                }
            }
        }
        shades
    }

    #[test]
    fn screen_data_round_trip() {
        let data: Vec<u8> = (0..0x1000).map(|i| (i * 7 + 3) as u8).collect();
        assert_eq!(Sgb::screen_data(&data_to_screen(&data)), data);
    }

    #[test]
    fn mask_freezes_the_picture() {
        let mut s = Sgb::blank(true);
        s.end_frame(&vec![1u8; 160 * 144]);
        let before = px(&s, 100, 100);
        send(&mut s, &[0xB9, 0x01]);   // MASK_EN freeze
        s.end_frame(&vec![3u8; 160 * 144]);
        assert_eq!(px(&s, 100, 100), before);
        send(&mut s, &[0xB9, 0x00]);
        s.end_frame(&vec![3u8; 160 * 144]);
        assert_ne!(px(&s, 100, 100), before);
    }
}
