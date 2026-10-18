// Sega Master System / Game Gear VDP (315-5124 / 315-5246 / 315-5378).
// A TMS9918A descendant with an extra "mode 4" that every SMS and Game Gear game uses.
//
// Ports (same shape as the TMS9918):
//   data port:    VRAM or CRAM at the address register, auto-increment
//   control port: two-byte write. 2nd byte bits 7-6 = code:
//                 0 VRAM read (pre-fetch), 1 VRAM write, 2 register write, 3 CRAM write
//                 Reads return the status: bit 7 frame interrupt, bit 6 sprite overflow,
//                 bit 5 sprite collision. Reading clears them and the line interrupt.
//
// Mode 4 registers:
//   R0  bit 7 lock vscroll for columns 24-31, bit 6 lock hscroll for rows 0-1,
//       bit 5 hide column 0 (backdrop), bit 4 line interrupt enable,
//       bit 3 shift sprites left 8, bit 2 M4 (mode 4 on)
//   R1  bit 6 display on, bit 5 frame interrupt enable, bit 1 8x16 sprites, bit 0 zoom
//   R2  name table = (R2 & 0x0E) << 10      (32 x 28 entries, 2 bytes each)
//   R5  sprite attribute table = (R5 & 0x7E) << 7
//   R6  bit 2: sprite tiles come from 256-511
//   R7  backdrop = sprite palette color (R7 & 15)
//   R8  horizontal scroll, R9 vertical scroll (latched at the start of each frame)
//   R10 line counter reload
//
// Name table entry: bits 0-8 tile, 9 h-flip, 10 v-flip, 11 sprite palette, 12 in front of sprites.
// Tiles: 8x8, 4 bitplanes, 32 bytes (4 bytes per row, one per plane).
// Sprites: 64, y at SAT+n, x/tile at SAT+0x80+2n. y = 0xD0 ends the list. 8 per line max.
//
// Colors: SMS CRAM = 32 bytes, --BBGGRR. Game Gear CRAM = 64 bytes, 2 per color,
// ----BBBB GGGGRRRR (little-endian; the even byte is latched until the odd one is written).
// Game Gear only shows the middle 160 x 144 of the 256 x 192 picture.
//
// Only the 192-line mode 4 is drawn. The TMS9918 legacy modes (used by SG-1000 games, not
// by SMS/GG games) and the 224/240-line modes show the backdrop for now.
//
// Reference: Charles MacDonald "Sega Master System VDP documentation", SMS Power

use serde::{Deserialize, Serialize};

pub const SCREEN_W: usize = 256;
pub const SCREEN_H: usize = 192;
pub const LINES_PER_FRAME: usize = 262;

#[derive(Serialize, Deserialize, Clone)]
pub struct SmsVdp {
    pub game_gear: bool,
    #[serde(with = "serde_bytes")]
    pub vram: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub cram: Vec<u8>,
    pub regs: [u8; 11],
    pub status: u8,
    addr: u16,
    code: u8,
    first_byte: u8,
    latch: bool,
    read_buffer: u8,
    cram_latch: u8,             // Game Gear: even CRAM byte waiting for its pair
    line_counter: u8,
    pub line_int_pending: bool,
    vscroll: u8,                // R9, latched once per frame

    #[serde(skip)]
    pub framebuffer: Vec<u8>,   // RGBA, SCREEN_W x SCREEN_H
}

impl SmsVdp {
    pub fn new(game_gear: bool) -> Self {
        Self {
            game_gear,
            vram: vec![0; 0x4000],
            cram: vec![0; if game_gear { 64 } else { 32 }],
            regs: [0; 11],
            status: 0,
            addr: 0,
            code: 0,
            first_byte: 0,
            latch: false,
            read_buffer: 0,
            cram_latch: 0,
            line_counter: 0xFF,
            line_int_pending: false,
            vscroll: 0,
            framebuffer: vec![0; SCREEN_W * SCREEN_H * 4],
        }
    }

    pub fn reset(&mut self) {
        self.regs = [0; 11];
        self.status = 0;
        self.addr = 0;
        self.code = 0;
        self.latch = false;
        self.line_int_pending = false;
        self.line_counter = 0xFF;
    }

    pub fn load_state(&mut self, s: SmsVdp) {
        let fb = std::mem::take(&mut self.framebuffer);
        *self = s;
        self.framebuffer = fb;
    }

    // ---------------- CPU ports ----------------

    pub fn write_data(&mut self, v: u8) {
        self.latch = false;
        if self.code == 3 {
            self.write_cram(v);
        } else {
            self.vram[self.addr as usize] = v;
        }
        self.read_buffer = v;
        self.addr = (self.addr + 1) & 0x3FFF;
    }

    fn write_cram(&mut self, v: u8) {
        if self.game_gear {
            let a = (self.addr & 0x3F) as usize;
            if a & 1 == 0 {
                self.cram_latch = v;
            } else {
                self.cram[a - 1] = self.cram_latch;
                self.cram[a] = v & 0x0F;
            }
        } else {
            self.cram[(self.addr & 0x1F) as usize] = v & 0x3F;
        }
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
            self.addr = (self.addr & 0x3F00) | v as u16;
            return;
        }
        self.latch = false;
        self.code = v >> 6;
        self.addr = (((v & 0x3F) as u16) << 8) | self.first_byte as u16;
        match self.code {
            0 => {
                self.read_buffer = self.vram[self.addr as usize];
                self.addr = (self.addr + 1) & 0x3FFF;
            }
            2 => {
                let r = (v & 0x0F) as usize;
                if r < 11 { self.regs[r] = self.first_byte; }
            }
            _ => {}
        }
    }

    pub fn read_status(&mut self) -> u8 {
        self.latch = false;
        let v = self.status | 0x1F;
        self.status = 0;
        self.line_int_pending = false;
        v
    }

    /// The /INT pin (Z80 maskable interrupt, level triggered).
    pub fn irq_line(&self) -> bool {
        (self.status & 0x80 != 0 && self.regs[1] & 0x20 != 0)
            || (self.line_int_pending && self.regs[0] & 0x10 != 0)
    }

    /// V counter as the CPU sees it (NTSC 192-line mode: 00-DA, then D5-FF).
    pub fn v_counter(line: usize) -> u8 {
        if line <= 0xDA { line as u8 } else { (line - 6) as u8 }
    }

    // ---------------- per-line timing ----------------

    /// Call at the start of every line, before rendering it.
    pub fn start_line(&mut self, line: usize) {
        if line == 0 {
            self.vscroll = self.regs[9];
        }
        // the line counter runs on lines 0-192 and reloads everywhere else
        if line <= SCREEN_H {
            let (next, underflow) = self.line_counter.overflowing_sub(1);
            if underflow {
                self.line_counter = self.regs[10];
                self.line_int_pending = true;
            } else {
                self.line_counter = next;
            }
        } else {
            self.line_counter = self.regs[10];
        }
        if line < SCREEN_H {
            self.render_line(line);
        } else if line == SCREEN_H + 1 {
            self.status |= 0x80;   // frame interrupt
        }
    }

    // ---------------- rendering ----------------

    fn color(&self, index: usize) -> [u8; 3] {
        if self.game_gear {
            let lo = self.cram[index * 2];
            let hi = self.cram[index * 2 + 1];
            [(lo & 0x0F) * 17, (lo >> 4) * 17, (hi & 0x0F) * 17]
        } else {
            let c = self.cram[index];
            [(c & 3) * 85, ((c >> 2) & 3) * 85, ((c >> 4) & 3) * 85]
        }
    }

    /// Pixel (0-15) of 4bpp tile `tile`, row `row`, column `col`.
    fn tile_pixel(&self, tile: usize, row: usize, col: usize) -> u8 {
        let a = (tile * 32 + row * 4) & 0x3FFF;
        let bit = 7 - col;
        let mut p = 0;
        for plane in 0..4 {
            p |= ((self.vram[a + plane] >> bit) & 1) << plane;
        }
        p
    }

    pub fn render_line(&mut self, y: usize) {
        let backdrop = 16 + (self.regs[7] & 0x0F) as usize;
        let mut line = [backdrop as u8; SCREEN_W];
        let mode4 = self.regs[0] & 0x04 != 0;
        if self.regs[1] & 0x40 != 0 && mode4 {
            let mut bg_priority = [false; SCREEN_W];
            self.draw_background(y, &mut line, &mut bg_priority);
            self.draw_sprites(y, &mut line, &bg_priority);
            if self.regs[0] & 0x20 != 0 {
                for px in line.iter_mut().take(8) { *px = backdrop as u8; }
            }
        }
        let row = y * SCREEN_W * 4;
        for (x, &c) in line.iter().enumerate() {
            let rgb = self.color(c as usize);
            self.framebuffer[row + x * 4..row + x * 4 + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 0xFF]);
        }
    }

    fn draw_background(&self, y: usize, line: &mut [u8; SCREEN_W], priority: &mut [bool; SCREEN_W]) {
        let name_base = ((self.regs[2] & 0x0E) as usize) << 10;
        let hscroll = if self.regs[0] & 0x40 != 0 && y < 16 { 0 } else { self.regs[8] };
        for x in 0..SCREEN_W {
            let vs = if self.regs[0] & 0x80 != 0 && x >= 192 { 0 } else { self.vscroll as usize };
            let row = (y + vs) % 224;
            let bx = (x as u8).wrapping_sub(hscroll) as usize;
            let a = name_base + (row / 8) * 64 + (bx / 8) * 2;
            let entry = self.vram[a] as u16 | ((self.vram[a + 1] as u16) << 8);
            let tile = (entry & 0x1FF) as usize;
            let mut ty = row & 7;
            let mut tx = bx & 7;
            if entry & 0x0200 != 0 { tx = 7 - tx; }
            if entry & 0x0400 != 0 { ty = 7 - ty; }
            let p = self.tile_pixel(tile, ty, tx);
            let palette = if entry & 0x0800 != 0 { 16 } else { 0 };
            line[x] = (palette + p) as u8;
            priority[x] = entry & 0x1000 != 0 && p != 0;
        }
    }

    fn draw_sprites(&mut self, y: usize, line: &mut [u8; SCREEN_W], bg_priority: &[bool; SCREEN_W]) {
        let sat = ((self.regs[5] & 0x7E) as usize) << 7;
        let tall = self.regs[1] & 0x02 != 0;
        let zoom = if self.regs[1] & 0x01 != 0 { 2 } else { 1 };
        let height = if tall { 16 } else { 8 } * zoom;
        let tile_base = if self.regs[6] & 0x04 != 0 { 0x100 } else { 0 };
        let shift = if self.regs[0] & 0x08 != 0 { 8 } else { 0 };

        let mut taken = [false; SCREEN_W];
        let mut count = 0;
        for n in 0..64 {
            let sy = self.vram[sat + n];
            if sy == 0xD0 { break; }
            let mut top = sy as i32 + 1;
            if top > 240 { top -= 256; }    // wraps in from the top
            let r = y as i32 - top;
            if r < 0 || r >= height as i32 { continue; }
            count += 1;
            if count > 8 {
                self.status |= 0x40;          // overflow: the 9th sprite isn't drawn
                break;
            }
            let x0 = self.vram[sat + 0x80 + n * 2] as i32 - shift;
            let mut tile = self.vram[sat + 0x81 + n * 2] as usize | tile_base;
            if tall { tile &= !1; }
            let row = r as usize / zoom;
            let tile = tile + row / 8;
            for px in 0..8 * zoom {
                let x = x0 + px as i32;
                if !(0..SCREEN_W as i32).contains(&x) { continue; }
                let x = x as usize;
                let p = self.tile_pixel(tile, row & 7, px / zoom);
                if p == 0 { continue; }
                if taken[x] {
                    self.status |= 0x20;      // collision; the earlier sprite stays on top
                    continue;
                }
                taken[x] = true;
                if !bg_priority[x] { line[x] = 16 + p; }
            }
        }
    }
}
