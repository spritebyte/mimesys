//! Teenage Mutant Ninja Turtles video (MAME tmnt.cpp, k052109.cpp, k051960.cpp;
//! BSD-3-Clause).
//!
//! * K052109 + K051962: three 512x256 tilemaps of 8x8 4bpp tiles: FIX (no scroll), A and
//!   B (XY scroll, row / column scroll). 16K of RAM seen through the CPU window; the same
//!   chip as The Simpsons (the tile and scroll code below is a copy of that driver's).
//! * K051960 + K051937: 128 zoomable sprites of 1x1 to 8x8 16x16 tiles. The sprite RAM is
//!   copied to an internal buffer at the start of vblank, and the sprites are drawn in
//!   priority-code order. Pen 15 of a sprite flagged as a shadow darkens what is under it.
//! * Palette: 1024 xBGR-555 colours, written one byte at a time (big-endian words).
//! * The board's priority register picks whether the sprites go under or over layer A;
//!   layer B is the opaque background and FIX is always on top.
//!
//! The screen is 384 x 264 at 6 MHz; visible x 0-319, y 16-239. The whole frame is drawn
//! once, at the end of vblank, like MAME.

use serde::{Deserialize, Serialize};

pub const SCREEN_W: usize = 320;
pub const SCREEN_H: usize = 224;
pub const FIRST_LINE: usize = 16;
const RASTER_W: i32 = 384;
const RASTER_H: i32 = 264;

/// pen flag: the pixel is shadowed
const SHADOW: u16 = 0x8000;
/// MAME `palette->set_shadow_factor(0.75)`
const SHADOW_FACTOR: f32 = 0.75;
/// layer colour bases (VIDEO_START tmnt) and the sprite colour base
const LAYER_COLORBASE: [u32; 3] = [0, 32, 40];
const SPRITE_COLORBASE: u32 = 16;

/// Builds a region from word-interleaved ROMs (MAME ROM_LOAD32_WORD): ROM k's words go to
/// bytes 2k, 2k+1 of every `2 * roms.len()`-byte group.
pub fn interleave_words(roms: &[Vec<u8>]) -> Vec<u8> {
    let n = roms.len();
    let len = roms.iter().map(|r| r.len()).max().unwrap_or(0);
    let mut out = vec![0u8; len * n];
    for (k, r) in roms.iter().enumerate() {
        for (i, w) in r.chunks(2).enumerate() {
            let d = i * 2 * n + 2 * k;
            out[d..d + w.len()].copy_from_slice(w);
        }
    }
    out
}

#[derive(Serialize, Deserialize)]
pub struct Video {
    /// K052109 RAM (0x4000 seen by the CPU)
    #[serde(with = "serde_bytes")]
    pub vram: Vec<u8>,
    pub charrombank: [u8; 4],
    pub charrombank_2: [u8; 4],
    pub romsubbank: u8,
    pub addrmap: u8,
    pub tileflip: u8,
    pub rmrd: bool,
    /// palette RAM (0x800 bytes)
    #[serde(with = "serde_bytes")]
    pub palette: Vec<u8>,
    /// K051960 sprite RAM the CPU writes / the buffer it is copied to at vblank
    #[serde(with = "serde_bytes")]
    pub sprite_ram: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub sprite_buf: Vec<u8>,
    /// K051937 registers: control, shadow config, ROM bank bytes, ROM readback address
    pub s_control: u8,
    pub s_shadow: u8,
    pub s_rombank: [u8; 3],
    pub s_romoffset: u32,
    /// the board's priority register (bits 2-3 of 0C0000, shifted down)
    pub priority: u8,

    #[serde(skip)] tiles_rom: Vec<u8>,
    #[serde(skip)] sprite_rom: Vec<u8>,
    #[serde(skip)] pens: Vec<u16>,
    #[serde(skip)] pub framebuffer: Vec<u8>,
}

/// x offset (in pixels) of sprite tile cell (x, y) within the 128-byte tile: the sprite
/// layout is four 8x8 quadrants of 8 rows x 4 bytes (one byte per bitplane)
fn sprite_pixel(rom: &[u8], code: u32, x: usize, y: usize) -> u8 {
    let n = (rom.len() / 128).max(1);
    let base = (code as usize % n) * 128 + (y >> 3) * 64 + (x >> 3) * 32 + (y & 7) * 4;
    let Some(b) = rom.get(base..base + 4) else { return 0 };
    nibble(b, x & 7)
}

/// The graphics ROMs of this board hold 4-bit pixels packed two to a byte, 8 pixels per
/// 4-byte row. The row is a little-endian 32-bit word read from its top nibble: pixel 0 is
/// the high nibble of byte 3, pixel 7 the low nibble of byte 0. Worked out from the font in
/// real dumps: this is the only order that draws E, K, M, R the right way round.
fn nibble(row: &[u8], x: usize) -> u8 { (row[3 - (x >> 1)] >> ((1 - (x & 1)) * 4)) & 15 }

impl Video {
    pub fn new(tiles_rom: Vec<u8>, sprite_rom: Vec<u8>) -> Self {
        Self {
            vram: vec![0; 0x4000],
            charrombank: [0; 4], charrombank_2: [0; 4],
            romsubbank: 0, addrmap: 0, tileflip: 0, rmrd: false,
            palette: vec![0; 0x800],
            sprite_ram: vec![0; 0x400],
            sprite_buf: vec![0; 0x400],
            s_control: 0, s_shadow: 0, s_rombank: [0; 3], s_romoffset: 0,
            priority: 0,
            tiles_rom, sprite_rom,
            pens: vec![0; SCREEN_W * SCREEN_H],
            framebuffer: vec![0; SCREEN_W * SCREEN_H * 4],
        }
    }

    pub fn keep_static(&mut self, old: &mut Self) {
        self.tiles_rom = std::mem::take(&mut old.tiles_rom);
        self.sprite_rom = std::mem::take(&mut old.sprite_rom);
        self.pens = std::mem::take(&mut old.pens);
        self.framebuffer = std::mem::take(&mut old.framebuffer);
    }

    pub fn reset(&mut self) {
        self.rmrd = false;
        for off in (0x1C00..=0x1F00).step_by(0x80) { self.k052109_write(off, 0); }
        self.charrombank = [0; 4];
        self.charrombank_2 = [0; 4];
        for i in 0..5 { self.k051937_write(i, 0); }
        self.s_romoffset = 0;
    }

    // ---------------------------------------------------------------- K052109

    /// The board's tile callback (tmnt_tile_callback): colour bits 0,1,4 and 2,3 extend the
    /// code, bits 5-7 pick one of 8 palettes after the layer's base. Returns (code, palette).
    fn tile_callback(layer: usize, bank: u8, code: u32, color: u8) -> (u32, u32) {
        let c = color as u32;
        let code = code | ((c & 0x03) << 8) | ((c & 0x10) << 6) | ((c & 0x0C) << 9) | ((bank as u32) << 13);
        (code, LAYER_COLORBASE[layer] + ((c & 0xE0) >> 5))
    }

    pub fn k052109_read(&self, off: u16) -> u8 {
        let off = off as usize & 0x3FFF;
        if !self.rmrd { return self.vram[off]; }
        // RMRD: the CPU reads the character ROM (ROM test)
        let code = ((off & 0x1FFF) >> 5) as u32;
        let color = self.romsubbank;
        let sel = ((color & 0x0C) >> 2) as usize;
        let bank = (self.charrombank[sel] >> 2) | (self.charrombank_2[sel] >> 2);
        let (code, _) = Self::tile_callback(0, bank, code, color);
        let len = self.tiles_rom.len().max(1);
        let addr = (((code as usize) << 5) + (off & 0x1F)) & (len - 1);
        self.tiles_rom.get(addr).copied().unwrap_or(0xFF)
    }

    pub fn k052109_write(&mut self, off: u16, d: u8) {
        let off = off as usize & 0x3FFF;
        self.vram[off] = d;
        if off & 0x1FFF < 0x1800 { return; }
        match off {
            0x1C00 => self.addrmap = d,
            0x1D80 => { self.charrombank[0] = d & 0x0F; self.charrombank[1] = d >> 4; }
            0x1E00 | 0x3E00 => self.romsubbank = d,
            0x1E80 => self.tileflip = d & 0x07,
            0x1F00 => { self.charrombank[2] = d & 0x0F; self.charrombank[3] = d >> 4; }
            0x3D80 => { self.charrombank_2[0] = d & 0x0F; self.charrombank_2[1] = d >> 4; }
            0x3F00 => { self.charrombank_2[2] = d & 0x0F; self.charrombank_2[3] = d >> 4; }
            _ => {}
        }
    }

    /// The board leaves A12 of the K052109 unconnected: the 68000 word at `a` reaches the
    /// chip at ((a >> 1) & 0x3000) >> 1 | (a >> 1) & 0x7FF (the high byte at +0, the low
    /// byte at +0x2000).
    fn noa12(a: u32) -> u16 {
        let o = (a >> 1) & 0x3FFF;
        (((o & 0x3000) >> 1) | (o & 0x07FF)) as u16
    }

    pub fn k052109_word_read(&self, a: u32) -> u16 {
        // MAME's handler answers the high byte only when it is selected
        (self.k052109_read(Self::noa12(a)) as u16) << 8
    }

    pub fn k052109_byte_read(&self, a: u32) -> u8 {
        let o = Self::noa12(a);
        if a & 1 == 0 { self.k052109_read(o) } else { self.k052109_read(o + 0x2000) }
    }

    pub fn k052109_word_write(&mut self, a: u32, v: u16) {
        self.k052109_write(Self::noa12(a), (v >> 8) as u8);
    }

    pub fn k052109_byte_write(&mut self, a: u32, v: u8) {
        let o = Self::noa12(a);
        if a & 1 == 0 { self.k052109_write(o, v) } else { self.k052109_write(o + 0x2000, v) }
    }

    // ---------------------------------------------------------------- K051960 / K051937

    pub fn k051937_write(&mut self, off: usize, d: u8) {
        match off & 7 {
            0 => self.s_control = d,
            1 => self.s_shadow = d & 0x07,
            n @ 2..=4 => self.s_rombank[n - 2] = d,
            _ => {}
        }
    }

    /// Sprite ROM byte for the ROM test (K051960 fetchromdata).
    fn sprite_fetch(&self, offset: usize) -> u8 {
        let b = &self.s_rombank;
        let addr = self.s_romoffset as usize + ((b[0] as usize) << 8) + (((b[1] as usize) & 3) << 16);
        let code = ((addr & 0x3FFE0) >> 5) as u32;
        let off1 = addr & 0x1F;
        let color = ((b[1] as u32 & 0xFC) >> 2) + ((b[2] as u32 & 3) << 6);
        let (code, _) = Self::sprite_callback(code, color);
        let a = ((code as usize) << 7) | (off1 << 2) | offset;
        let len = self.sprite_rom.len().max(1);
        self.sprite_rom.get(a & (len - 1)).copied().unwrap_or(0)
    }

    pub fn k051937_read(&self, off: usize) -> u8 {
        let off = off & 7;
        if self.s_control & 0x20 != 0 && off & 4 != 0 { self.sprite_fetch(off & 3) }
        else { 0 }   // the sprite engine's busy flag (offset 0) is never left set
    }

    pub fn k051960_read(&mut self, off: usize) -> u8 {
        let off = off & 0x3FF;
        if self.s_control & 0x20 != 0 {
            self.s_romoffset = ((off & 0x3FC) >> 2) as u32;
            self.sprite_fetch(off & 3)
        } else {
            self.sprite_ram[off]
        }
    }

    pub fn k051960_write(&mut self, off: usize, d: u8) { self.sprite_ram[off & 0x3FF] = d; }

    /// Vblank: the sprite RAM is copied to the buffer unless sprite processing is off.
    pub fn sprite_dma(&mut self) {
        if self.s_control & 0x10 == 0 {
            let (ram, buf) = (&self.sprite_ram, &mut self.sprite_buf);
            buf.copy_from_slice(ram);
        }
    }

    /// tmnt_sprite_callback: bit 4 of the colour byte is code bit 13, bits 0-3 the colour.
    fn sprite_callback(code: u32, color: u32) -> (u32, u32) {
        (code | ((color & 0x10) << 9), SPRITE_COLORBASE + (color & 0x0F))
    }

    // ---------------------------------------------------------------- palette

    fn rgb(&self, pen: u16) -> [u8; 4] {
        let i = (pen as usize & 0x3FF) * 2;
        let v = (self.palette[i] as u16) << 8 | self.palette[i + 1] as u16;
        let c = |n: u16| { let n = (n & 0x1F) as u8; (n << 3) | (n >> 2) };
        let (mut r, mut g, mut b) = (c(v), c(v >> 5), c(v >> 10));
        if pen & SHADOW != 0 {
            let s = |x: u8| (x as f32 * SHADOW_FACTOR + 0.5) as u8;
            r = s(r); g = s(g); b = s(b);
        }
        [r, g, b, 255]
    }

    // ---------------------------------------------------------------- drawing

    /// Debug aid: the chip registers, RAM and a ROM checksum, as hex (for bug reports).
    pub fn debug_dump(&self, tag: &str) {
        fn sum(b: &[u8]) -> u32 { b.iter().fold(0u32, |a, &x| a.rotate_left(5) ^ x as u32) }
        fn hex(name: &str, b: &[u8]) {
            crate::dlog!("  {name} ({} bytes)", b.len());
            for (i, c) in b.chunks(32).enumerate() { 
                if c.iter().all(|&x| x == 0) { continue; }
                let s: String = c.iter().map(|x| format!("{x:02x}")).collect();
                crate::dlog!("  {:04x}: {s}", i * 32);
            }
        }
        crate::dlog!("=== TMNT video dump: {tag} ===");
        crate::dlog!("addrmap {:02x} charrombank {:02x?} charrombank_2 {:02x?} romsubbank {:02x} tileflip {:02x} rmrd {}",
            self.addrmap, self.charrombank, self.charrombank_2, self.romsubbank, self.tileflip, self.rmrd);
        crate::dlog!("sprite ctrl {:02x} shadow {:02x} rombank {:02x?} priority {}", self.s_control, self.s_shadow, self.s_rombank, self.priority);
        crate::dlog!("tiles_rom {} bytes sum {:08x} head {:02x?}", self.tiles_rom.len(), sum(&self.tiles_rom), &self.tiles_rom[..16.min(self.tiles_rom.len())]);
        crate::dlog!("sprite_rom {} bytes sum {:08x} head {:02x?}", self.sprite_rom.len(), sum(&self.sprite_rom), &self.sprite_rom[..16.min(self.sprite_rom.len())]);
        // the first few distinct tiles each layer shows: final code, raw ROM bytes, pens
        let mut seen: Vec<u32> = Vec::new();
        for l in 0..3usize {
            for idx in 0..2048usize {
                if seen.len() >= 12 { break; }
                let code = self.vram[0x2000 + l * 0x800 + idx] as u32;
                let mut color = self.vram[l * 0x800 + idx];
                let mut bank = self.charrombank[((color & 0x0C) >> 2) as usize];
                if self.addrmap & 0x40 == 0 { color = (color & 0xF3) | ((bank & 3) << 2); }
                bank >>= 2;
                let (fc, pal) = Self::tile_callback(l, bank, code, color);
                if seen.contains(&fc) || fc == 0 { continue; }
                seen.push(fc);
                let base = (fc as usize % (self.tiles_rom.len() / 32).max(1)) * 32;
                crate::dlog!("tile layer {l} cell {idx} vram code {code:02x} attr {:02x} -> final code {fc:04x} pal {pal} raw {:02x?}",
                    self.vram[l * 0x800 + idx], &self.tiles_rom[base..base + 32]);
                for y in 0..8 {
                    let row: String = (0..8).map(|x| format!("{:x}", self.tile_pixel(fc, x, y))).collect();
                    crate::dlog!("    {row}");
                }
            }
        }
        // what the last frame drew on a few scanlines: palette pen & 15 per pixel (0-319)
        for sy in [20usize, 60, 100, 131, 135, 147, 190] {
            let row: String = (0..SCREEN_W).map(|x| format!("{:x}", self.pens[sy * SCREEN_W + x] & 15)).collect();
            crate::dlog!("pens y={sy}: {row}");
        }
        crate::dlog!("scroll regs 1C80 {:02x}, A x {:02x}{:02x} y {:02x}, B x {:02x}{:02x} y {:02x}, FIX cell row20 cols0-39: {:02x?}",
            self.vram[0x1C80], self.vram[0x1A01], self.vram[0x1A00], self.vram[0x180C],
            self.vram[0x3A01], self.vram[0x3A00], self.vram[0x380C], &self.vram[0x2500..0x2528]);
        // the first active sprites: their tile code and the 128 ROM bytes of that 16x16 tile
        let mut shown = 0;
        for offs in (0..0x400).step_by(8) {
            if shown >= 6 { break; }
            let b = &self.sprite_buf[offs..offs + 8];
            if b[0] & 0x80 == 0 { continue; }
            let (code, color) = Self::sprite_callback(b[2] as u32 + ((b[1] as u32 & 0x1F) << 8), b[3] as u32);
            let n = (self.sprite_rom.len() / 128).max(1);
            let base = (code as usize % n) * 128;
            crate::dlog!("sprite {offs:03x}: {:02x?} code {code:04x} size {} color {color}", b, (b[1] & 0xE0) >> 5);
            for part in self.sprite_rom[base..base + 128].chunks(32) {
                let h: String = part.iter().map(|x| format!("{x:02x}")).collect();
                crate::dlog!("    {h}");
            }
            shown += 1;
        }
        hex("K052109 RAM (0000 attr FIX/A/B, 1800 scroll, 1C00 regs, 2000 codes)", &self.vram);
        hex("palette", &self.palette);
        hex("sprite buffer", &self.sprite_buf);
    }

    /// Draws the whole frame.
    pub fn render(&mut self) {
        self.draw_layer(2, true);
        if self.priority & 1 == 1 { self.draw_sprites(); }
        self.draw_layer(1, false);
        if self.priority & 1 == 0 { self.draw_sprites(); }
        self.draw_layer(0, false);

        let mut fb = std::mem::take(&mut self.framebuffer);
        for (px, out) in self.pens.iter().zip(fb.chunks_exact_mut(4)) {
            out.copy_from_slice(&self.rgb(*px));
        }
        self.framebuffer = fb;
    }

    pub fn tile_pixel(&self, code: u32, x: usize, y: usize) -> u8 {
        let n = (self.tiles_rom.len() / 32).max(1);
        let base = (code as usize % n) * 32 + y * 4;
        let Some(b) = self.tiles_rom.get(base..base + 4) else { return 0 };
        nibble(b, x)
    }

    /// Tilemap pixel (x, y of the 512 x 256 map) of layer `l`: (palette pen, pen 0).
    fn layer_pixel(&self, l: usize, tx: usize, ty: usize) -> (u16, bool) {
        let idx = (ty >> 3) * 64 + (tx >> 3);
        let code = self.vram[0x2000 + l * 0x800 + idx] as u32;
        let mut color = self.vram[l * 0x800 + idx];
        let mut bank = self.charrombank[((color & 0x0C) >> 2) as usize];
        if self.addrmap & 0x40 == 0 {
            color = (color & 0xF3) | ((bank & 3) << 2);
        }
        bank >>= 2;
        let flipy = color & 0x02 != 0 && self.tileflip & 0x04 != 0;
        let (code, pal) = Self::tile_callback(l, bank, code, color);
        let py = if flipy { 7 - (ty & 7) } else { ty & 7 };
        let pen = self.tile_pixel(code, tx & 7, py);
        (((pal * 16 + pen as u32) & 0x3FF) as u16, pen == 0)
    }

    /// Tilemap coordinates shown at raster position (x, y), unflipped (MAME's tilemap
    /// scroll rules, including the k052109 row / column / mixed scroll modes).
    fn layer_coords(&self, l: usize, x: i32, y: i32) -> (usize, usize) {
        if l == 0 {
            return (((x + 96) & 511) as usize, (y & 255) as usize);
        }
        let ctrl = (self.vram[0x1C80] >> ((l - 1) * 3)) & 7;
        let rows = [1, 1, 32, 256][(ctrl & 3) as usize];
        let colscroll = ctrl & 4 != 0;
        let base = if l == 2 { 0x2000 } else { 0 };
        let ram_y = |i: usize| self.vram[base + 0x1800 + i] as i32;
        let ram_x = |i: usize| self.vram[base + 0x1A00 + i] as i32 + 256 * self.vram[base + 0x1A00 + i + 1] as i32;
        let yl = (y & 255) as usize;
        let (xs, ys);
        if rows == 1 && !colscroll {
            xs = ram_x(0);
            ys = ram_y(12);
        } else if !colscroll {
            let mask = if rows == 256 { 0xFF } else { 0xF8 };
            xs = ram_x(2 * (yl & mask));
            ys = ram_y(12);
        } else if rows == 1 {
            xs = ram_x(0);
            let tx = ((x + 90 + xs) & 511) as usize;
            let offs = ((tx >> 3) as i32 - xs / 8) & 63;
            ys = ram_y(offs as usize);
        } else {
            let (rh, step) = if rows == 256 { (1, 2) } else { (8, 16) };
            xs = ram_x(step * (yl / rh));
            let tx = ((x + 90 + xs) & 511) as i32;
            let col = (((tx & !7) - (xs & !7)) >> 3) & 63;
            ys = ram_y(col as usize);
        }
        (((x + 90 + xs) & 511) as usize, ((y + ys) & 255) as usize)
    }

    /// Draws tilemap `l`; `opaque` also draws its pen 0 (the background layer).
    fn draw_layer(&mut self, l: usize, opaque: bool) {
        let flip = self.tileflip & 1 != 0;
        for sy in 0..SCREEN_H {
            for sx in 0..SCREEN_W {
                let (mut x, mut y) = (sx as i32, (sy + FIRST_LINE) as i32);
                if flip { x = RASTER_W - 1 - x; y = RASTER_H - 1 - y; }
                let (tx, ty) = self.layer_coords(l, x, y);
                let (pen, clear) = self.layer_pixel(l, tx, ty);
                if opaque || !clear {
                    self.pens[sy * SCREEN_W + sx] = pen;
                }
            }
        }
    }

    /// K051960 sprites (MAME k051960_sprites_draw with max_priority 0: no priority bitmap,
    /// drawn back to front by priority code).
    fn draw_sprites(&mut self) {
        const XOFF: [u32; 8] = [0, 1, 4, 5, 16, 17, 20, 21];
        const YOFF: [u32; 8] = [0, 2, 8, 10, 32, 34, 40, 42];
        const WIDTH: [i32; 8] = [1, 2, 1, 2, 4, 2, 4, 8];
        const HEIGHT: [i32; 8] = [1, 1, 2, 2, 2, 4, 4, 8];
        const ZOOMY: [u8; 64] = [
            0x00, 0x01, 0x03, 0x05, 0x07, 0x09, 0x0a, 0x0c, 0x0e, 0x0f, 0x11, 0x12, 0x14, 0x15, 0x16, 0x18,
            0x19, 0x1a, 0x1c, 0x1d, 0x1e, 0x1f, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29,
            0x2a, 0x2b, 0x2c, 0x2d, 0x2e, 0x2e, 0x2f, 0x30, 0x31, 0x31, 0x32, 0x33, 0x34, 0x34, 0x35, 0x36,
            0x36, 0x37, 0x38, 0x38, 0x39, 0x39, 0x3a, 0x3b, 0x3b, 0x3c, 0x3c, 0x3d, 0x3d, 0x3e, 0x3e, 0x3f,
        ];
        let mut sorted = [usize::MAX; 128];
        for offs in (0..0x400).step_by(8) {
            if self.sprite_buf[offs] & 0x80 != 0 {
                sorted[(self.sprite_buf[offs] & 0x7F) as usize] = offs;
            }
        }
        for &offs in sorted.iter().filter(|&&o| o != usize::MAX) {
            let b: [u8; 8] = self.sprite_buf[offs..offs + 8].try_into().unwrap();
            let raw_color = b[3] as u32;
            let shadow = self.s_shadow & 4 == 0 && (self.s_shadow & 2 != 0 || raw_color & 0x80 != 0);
            let (mut code, color) = Self::sprite_callback(b[2] as u32 + ((b[1] as u32 & 0x1F) << 8), raw_color);

            let size = ((b[1] & 0xE0) >> 5) as usize;
            let (w, h) = (WIDTH[size], HEIGHT[size]);
            if w >= 2 { code &= !0x01; }
            if h >= 2 { code &= !0x02; }
            if w >= 4 { code &= !0x04; }
            if h >= 4 { code &= !0x08; }
            if w >= 8 { code &= !0x10; }
            if h >= 8 { code &= !0x20; }

            let mut ox = ((256 * b[6] as i32 + b[7] as i32) & 0x1FF) as i32;
            let mut oy = 256 - ((256 * b[4] as i32 + b[5] as i32) & 0x1FF);
            let mut flipx = b[6] & 0x02 != 0;
            let mut flipy = b[4] & 0x02 != 0;
            // X zoom is linear, Y zoom follows a table measured on the board
            let zx = ((b[6] & 0xFC) >> 2) as i32;
            let zoomx = 0x10000 / 128 * (128 - zx);
            let mut zy = 128 - ZOOMY[((b[4] & 0xFC) >> 2) as usize] as i32;
            for i in 0..3 { if h <= (1 << i) { zy = (zy + 1) / 2; } }
            zy *= 8 / h;
            let zoomy = 0x10000 / 128 * zy;

            if self.s_control & 0x08 != 0 {
                ox = 512 - ((zoomx * w) >> 12) - ox;
                oy = 256 - ((zoomy * h) >> 12) - oy;
                flipx = !flipx;
                flipy = !flipy;
            }

            for y in 0..h {
                let sy = oy + ((zoomy * y + (1 << 11)) >> 12);
                let zh = (oy + ((zoomy * (y + 1) + (1 << 11)) >> 12)) - sy;
                for x in 0..w {
                    let sx = ox + ((zoomx * x + (1 << 11)) >> 12);
                    let zw = (ox + ((zoomx * (x + 1) + (1 << 11)) >> 12)) - sx;
                    let mut c = code;
                    c += if flipx { XOFF[(w - 1 - x) as usize] } else { XOFF[x as usize] };
                    c += if flipy { YOFF[(h - 1 - y) as usize] } else { YOFF[y as usize] };
                    self.zoom_tile(c, color, flipx, flipy, (sx & 0x1FF) - 96, sy, zw, zh, shadow);
                }
            }
        }
    }

    /// One 16x16 sprite tile drawn `dw` x `dh` pixels at raster (dx, dy) (MAME
    /// zoom_transtable). Pen 0 is clear; pen 15 darkens what is below when `shadow`.
    #[allow(clippy::too_many_arguments)]
    fn zoom_tile(&mut self, code: u32, color: u32, flipx: bool, flipy: bool,
                 dx: i32, dy: i32, dw: i32, dh: i32, shadow: bool) {
        if dw < 1 || dh < 1 { return; }
        let stepx = (16 << 16) / dw;
        let stepy = (16 << 16) / dh;
        let (clip_x0, clip_x1) = (0, SCREEN_W as i32 - 1);
        let (clip_y0, clip_y1) = (FIRST_LINE as i32, (FIRST_LINE + SCREEN_H) as i32 - 1);
        let (mut x0, mut x1) = (dx, dx + dw - 1);
        let (mut y0, mut y1) = (dy, dy + dh - 1);
        if x0 > clip_x1 || x1 < clip_x0 || y0 > clip_y1 || y1 < clip_y0 { return; }
        let mut srcx = 0;
        if x0 < clip_x0 { srcx = (clip_x0 - x0) * stepx; x0 = clip_x0; }
        if x1 > clip_x1 { x1 = clip_x1; }
        let mut srcy = 0;
        if y0 < clip_y0 { srcy = (clip_y0 - y0) * stepy; y0 = clip_y0; }
        if y1 > clip_y1 { y1 = clip_y1; }
        let (mut sdx, mut sdy) = (stepx, stepy);
        if flipx { srcx = (dw - 1) * stepx - srcx; sdx = -stepx; }
        if flipy { srcy = (dh - 1) * stepy - srcy; sdy = -stepy; }
        let base = (color & 0x3F) as u16 * 16;
        for y in y0..=y1 {
            let ty = (srcy >> 16) as usize;
            srcy += sdy;
            let row = (y as usize - FIRST_LINE) * SCREEN_W;
            let mut cx = srcx;
            for x in x0..=x1 {
                let pen = sprite_pixel(&self.sprite_rom, code, (cx >> 16) as usize & 15, ty & 15);
                cx += sdx;
                if pen == 0 { continue; }
                let i = row + x as usize;
                if pen == 15 && shadow {
                    self.pens[i] |= SHADOW;
                } else {
                    self.pens[i] = base + pen as u16;
                }
            }
        }
    }
}
