// Texas Instruments TMS9918A / TMS9928A Video Display Processor.
// Used by the ColecoVision, MSX1, SG-1000, TI-99/4A. The Sega Master System VDP
// is a superset of it (adds "mode 4"), so this is the base for that later.
//
// 16 KB of VRAM that the CPU only reaches through two ports:
//   data port:    read/write VRAM at the address register, which auto-increments
//   control port: two-byte writes. 2nd byte bit 7 = 1: register write (reg = bits 0-2,
//                 value = 1st byte). Bit 7 = 0: set the address (bit 6 = 0 means "for
//                 reading", which pre-fetches the first byte). Reads return the status.
//
// Registers:
//   R0  bit 1 M3 (Graphics II), bit 0 external video
//   R1  bit 7 4K/16K, bit 6 display enable, bit 5 interrupt enable, bit 4 M1 (Text),
//       bit 3 M2 (Multicolor), bit 1 16x16 sprites, bit 0 magnified (2x) sprites
//   R2  name table      = R2 * 0x400
//   R3  color table     = R3 * 0x40   (Graphics II: bit 7 = base, bits 0-6 = mask)
//   R4  pattern table   = R4 * 0x800  (Graphics II: bit 2 = base, bits 0-1 = mask)
//   R5  sprite attributes = R5 * 0x80
//   R6  sprite patterns   = R6 * 0x800
//   R7  text color (high nibble) / backdrop color (low nibble)
//
// Status: bit 7 frame flag (vblank started), bit 6 fifth sprite on a line,
//         bit 5 sprite collision, bits 0-4 number of the fifth sprite.
//         Reading it clears bits 7-5. INT output = frame flag AND R1 bit 5.
//
// Screen: 256 x 192 active, 262 lines per frame (NTSC). We render one line at a time
// so mid-frame register changes (split screens) work.
//
// Reference: TI TMS9918A/TMS9928A/TMS9929A data manual (SPPU004), MAME tms9928a.cpp

use serde::{Deserialize, Serialize};

pub const ACTIVE_W: usize = 256;
pub const ACTIVE_H: usize = 192;
pub const LINES_PER_FRAME: usize = 262;

/// TMS9918A colors (MAME's resistor-derived values). Index 0 is "transparent".
pub const PALETTE: [[u8; 3]; 16] = [
    [0, 0, 0],       [0, 0, 0],       [33, 200, 66],   [94, 220, 120],
    [84, 85, 237],   [125, 118, 252], [212, 82, 77],   [66, 235, 245],
    [252, 85, 84],   [255, 121, 120], [212, 193, 84],  [230, 206, 128],
    [33, 176, 59],   [201, 91, 186],  [204, 204, 204], [255, 255, 255],
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode { Graphics1, Graphics2, Multicolor, Text }

#[derive(Serialize, Deserialize, Clone)]
pub struct Tms9918 {
    #[serde(with = "serde_bytes")]
    pub vram: Vec<u8>,
    pub regs: [u8; 8],
    pub status: u8,
    addr: u16,
    first_byte: u8,
    latch: bool,          // true = first control byte received, waiting for the second
    read_buffer: u8,

    #[serde(skip)]
    pub framebuffer: Vec<u8>,   // RGBA, ACTIVE_W x ACTIVE_H
}

impl Default for Tms9918 {
    fn default() -> Self { Self::new() }
}

impl Tms9918 {
    pub fn new() -> Self {
        Self {
            vram: vec![0; 0x4000],
            regs: [0; 8],
            status: 0,
            addr: 0,
            first_byte: 0,
            latch: false,
            read_buffer: 0,
            framebuffer: vec![0; ACTIVE_W * ACTIVE_H * 4],
        }
    }

    pub fn reset(&mut self) {
        self.regs = [0; 8];
        self.status = 0;
        self.addr = 0;
        self.latch = false;
        self.read_buffer = 0;
    }

    /// Restore from a save state, keeping our framebuffer allocation.
    pub fn load_state(&mut self, s: Tms9918) {
        let fb = std::mem::take(&mut self.framebuffer);
        *self = s;
        self.framebuffer = fb;
    }

    // ---------------- CPU ports ----------------

    pub fn write_data(&mut self, v: u8) {
        self.latch = false;
        self.vram[self.addr as usize] = v;
        self.read_buffer = v;   // the TMS9918 updates its read-ahead buffer on writes too
        self.addr = (self.addr + 1) & 0x3FFF;
    }

    pub fn read_data(&mut self) -> u8 {
        self.latch = false;
        let v = self.read_buffer;
        self.read_buffer = self.vram[self.addr as usize];
        self.addr = (self.addr + 1) & 0x3FFF;
        v
    }

    pub fn write_control(&mut self, v: u8) {
        if !self.latch {
            self.first_byte = v;
            self.latch = true;
            // the low address byte is updated right away
            self.addr = (self.addr & 0x3F00) | v as u16;
            return;
        }
        self.latch = false;
        if v & 0x80 != 0 {
            self.regs[(v & 7) as usize] = self.first_byte;
        } else {
            self.addr = (((v & 0x3F) as u16) << 8) | self.first_byte as u16;
            if v & 0x40 == 0 {
                // set up for reading: pre-fetch
                self.read_buffer = self.vram[self.addr as usize];
                self.addr = (self.addr + 1) & 0x3FFF;
            }
        }
    }

    pub fn read_status(&mut self) -> u8 {
        self.latch = false;
        let v = self.status;
        self.status &= 0x1F;    // clear frame, 5th sprite and collision flags
        v
    }

    /// The INT pin (wired to the Z80's NMI on the ColecoVision).
    pub fn irq_line(&self) -> bool {
        self.status & 0x80 != 0 && self.regs[1] & 0x20 != 0
    }

    /// Call at the start of vblank (after line 191 is drawn).
    pub fn start_vblank(&mut self) {
        self.status |= 0x80;
    }

    // ---------------- rendering ----------------

    pub fn mode(&self) -> Mode {
        let m1 = self.regs[1] & 0x10 != 0;
        let m2 = self.regs[1] & 0x08 != 0;
        let m3 = self.regs[0] & 0x02 != 0;
        if m1 { Mode::Text } else if m2 { Mode::Multicolor } else if m3 { Mode::Graphics2 } else { Mode::Graphics1 }
    }

    fn backdrop(&self) -> u8 { self.regs[7] & 0x0F }
    fn name_base(&self) -> usize { (self.regs[2] as usize & 0x0F) << 10 }
    fn color_base(&self) -> usize { (self.regs[3] as usize) << 6 }
    fn pattern_base(&self) -> usize { (self.regs[4] as usize & 0x07) << 11 }
    fn sprite_attr_base(&self) -> usize { (self.regs[5] as usize & 0x7F) << 7 }
    fn sprite_pattern_base(&self) -> usize { (self.regs[6] as usize & 0x07) << 11 }

    /// Draw active line `y` (0-191).
    pub fn render_line(&mut self, y: usize) {
        if y >= ACTIVE_H { return; }
        let mut line = [0u8; ACTIVE_W];   // color indices, 0 = transparent
        let enabled = self.regs[1] & 0x40 != 0;
        if enabled {
            match self.mode() {
                Mode::Graphics1 => self.draw_graphics1(y, &mut line),
                Mode::Graphics2 => self.draw_graphics2(y, &mut line),
                Mode::Multicolor => self.draw_multicolor(y, &mut line),
                Mode::Text => self.draw_text(y, &mut line),
            }
            if self.mode() != Mode::Text {
                self.draw_sprites(y, &mut line);
            }
        }
        let backdrop = self.backdrop();
        let row = y * ACTIVE_W * 4;
        for (x, &c) in line.iter().enumerate() {
            let c = if c == 0 { backdrop } else { c };
            let rgb = PALETTE[c as usize];
            self.framebuffer[row + x * 4..row + x * 4 + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 0xFF]);
        }
    }

    /// One 8-pixel pattern byte, two colors.
    fn put_pattern(line: &mut [u8], x0: usize, bits: u8, fg: u8, bg: u8, width: usize) {
        for i in 0..width {
            line[x0 + i] = if bits & (0x80 >> i) != 0 { fg } else { bg };
        }
    }

    fn draw_graphics1(&self, y: usize, line: &mut [u8; ACTIVE_W]) {
        let (row, ty) = (y & 7, y / 8);
        for col in 0..32 {
            let name = self.vram[self.name_base() + ty * 32 + col] as usize;
            let bits = self.vram[self.pattern_base() + name * 8 + row];
            let color = self.vram[self.color_base() + name / 8];
            Self::put_pattern(line, col * 8, bits, color >> 4, color & 0x0F, 8);
        }
    }

    fn draw_graphics2(&self, y: usize, line: &mut [u8; ACTIVE_W]) {
        let (row, ty) = (y & 7, y / 8);
        let third = (y / 64) << 8;
        // R4 bits 0-1 and R3 bits 0-6 act as address masks; their base bit picks the half of VRAM
        let pattern_mask = (((self.regs[4] as usize) & 3) << 8) | 0xFF;
        let color_mask = (((self.regs[3] as usize) & 0x7F) << 3) | 7;
        let pattern_base = ((self.regs[4] as usize) & 4) << 11;
        let color_base = ((self.regs[3] as usize) & 0x80) << 6;
        for col in 0..32 {
            let name = self.vram[self.name_base() + ty * 32 + col] as usize | third;
            let bits = self.vram[pattern_base + (name & pattern_mask) * 8 + row];
            let color = self.vram[color_base + (name & color_mask) * 8 + row];
            Self::put_pattern(line, col * 8, bits, color >> 4, color & 0x0F, 8);
        }
    }

    fn draw_multicolor(&self, y: usize, line: &mut [u8; ACTIVE_W]) {
        let ty = y / 8;
        for col in 0..32 {
            let name = self.vram[self.name_base() + ty * 32 + col] as usize;
            // each name covers 8x8 pixels = 2x2 blocks of 4x4; which byte depends on the row
            let byte = self.vram[self.pattern_base() + name * 8 + (ty & 3) * 2 + ((y / 4) & 1)];
            for i in 0..4 { line[col * 8 + i] = byte >> 4; }
            for i in 4..8 { line[col * 8 + i] = byte & 0x0F; }
        }
    }

    fn draw_text(&self, y: usize, line: &mut [u8; ACTIVE_W]) {
        // 40 columns of 6 pixels, with an 8-pixel border on each side
        let (row, ty) = (y & 7, y / 8);
        let (fg, bg) = (self.regs[7] >> 4, self.regs[7] & 0x0F);
        for col in 0..40 {
            let name = self.vram[self.name_base() + ty * 40 + col] as usize;
            let bits = self.vram[self.pattern_base() + name * 8 + row];
            Self::put_pattern(line, 8 + col * 6, bits, fg, bg, 6);
        }
    }

    fn draw_sprites(&mut self, y: usize, line: &mut [u8; ACTIVE_W]) {
        let size16 = self.regs[1] & 0x02 != 0;
        let mag = self.regs[1] & 0x01 != 0;
        let size = if size16 { 16 } else { 8 } * if mag { 2 } else { 1 };
        let attr = self.sprite_attr_base();
        let pat_base = self.sprite_pattern_base();

        let mut on_line = [0usize; 4];
        let mut count = 0;
        let mut last = 31;
        for n in 0..32 {
            let sy = self.vram[attr + n * 4];
            if sy == 0xD0 { last = n; break; }    // end of sprite list
            // the sprite's first line is Y+1; Y values above 0xE0 are slightly off the top
            let mut top = sy as i32 + 1;
            if sy > 0xE0 { top -= 256; }
            let r = y as i32 - top;
            if r < 0 || r >= size { continue; }
            if count == 4 {
                // fifth sprite on this line: flag it (only the first one per frame is kept)
                if self.status & 0x40 == 0 {
                    self.status = (self.status & 0xE0) | 0x40 | n as u8;
                }
                last = n;
                count += 1;
                break;
            }
            on_line[count] = n;
            count += 1;
        }
        if self.status & 0x40 == 0 {
            self.status = (self.status & 0xE0) | last as u8;
        }

        // lower-numbered sprites have priority: draw in order, later sprites skip pixels
        // that are already taken (and that overlap is a collision)
        let mut covered = [false; ACTIVE_W];
        let shown = count.min(4);
        for &n in on_line[..shown].iter() {
            let base = attr + n * 4;
            let (sy, sx, name, color) = (self.vram[base], self.vram[base + 1], self.vram[base + 2], self.vram[base + 3]);
            let mut top = sy as i32 + 1;
            if sy > 0xE0 { top -= 256; }
            let mut r = (y as i32 - top) as usize;
            if mag { r /= 2; }
            let mut x0 = sx as i32;
            if color & 0x80 != 0 { x0 -= 32; }     // early clock
            let c = color & 0x0F;
            let (name, cols) = if size16 { ((name & 0xFC) as usize, 2) } else { (name as usize, 1) };
            for half in 0..cols {
                let bits = self.vram[pat_base + name * 8 + half * 16 + r];
                for b in 0..8 {
                    if bits & (0x80 >> b) == 0 { continue; }
                    let px = (half * 8 + b) as i32 * if mag { 2 } else { 1 };
                    for m in 0..if mag { 2 } else { 1 } {
                        let x = x0 + px + m;
                        if !(0..ACTIVE_W as i32).contains(&x) { continue; }
                        let x = x as usize;
                        if covered[x] {
                            self.status |= 0x20;   // collision (even transparent sprites count)
                            continue;               // the earlier (higher priority) sprite stays
                        }
                        covered[x] = true;
                        if c != 0 { line[x] = c; }
                    }
                }
            }
        }
    }
}
