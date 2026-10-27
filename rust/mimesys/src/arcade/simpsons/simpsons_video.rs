//! The Simpsons video (MAME simpsons.cpp, k052109.cpp, k053246_k053247_k055673.cpp,
//! k053251.cpp; BSD-3-Clause).
//!
//! * K052109 + K051962: three 512x256 tilemaps of 8x8 4bpp tiles: FIX (no scroll),
//!   A and B (XY scroll, per-line row scroll, per-column column scroll). 16K of RAM seen
//!   through the CPU window: 0000-17FF colour/attribute, 2000-37FF tile codes, control
//!   registers in 1800-1FFF and 3800-3FFF.
//! * K053246 + K053247: up to 256 zoomable sprites of 1x1 to 8x8 16x16 tiles, drawn
//!   front to back by Z code, with shadow pens (pen 15 darkens what's under it).
//! * K053251: picks the palette bank of each layer and their priorities; the layers are
//!   sorted by it and each sprite's priority decides which layers cover it.
//! * Palette: 2048 xBGR-555 colours, big-endian words.
//!
//! The screen is 384 x 264 at 6 MHz; visible x 0-319, y 16-239. The whole frame is drawn
//! once, at the end of vblank, like MAME (the chips don't do raster effects).

use serde::{Deserialize, Serialize};

pub const SCREEN_W: usize = 320;
pub const SCREEN_H: usize = 224;
pub const FIRST_LINE: usize = 16;
/// full raster, used when the tilemaps are flipped
const RASTER_W: i32 = 384;
const RASTER_H: i32 = 264;

/// K053247 configuration from the driver (set_config(NORMAL_PLANE_ORDER, -43, 23))
const SPRITE_DX: i32 = -43;
const SPRITE_DY: i32 = 23;

const SHADOW: u16 = 0x8000;   // pen flag: shadowed colour

#[derive(Serialize, Deserialize)]
pub struct Video {
    /// K052109 RAM (0x4000 seen by the CPU)
    #[serde(with = "serde_bytes")]
    pub vram: Vec<u8>,
    pub charrombank: [u8; 4],
    pub charrombank_2: [u8; 4],
    pub romsubbank: u8,
    pub irq_control: u8,
    pub addrmap: u8,
    pub tileflip: u8,
    pub rmrd: bool,
    /// K053246 registers / K053247 registers / K053247 sprite RAM (0x800 words, filled
    /// by object DMA) / the board's sprite RAM the CPU writes (0x800 words)
    pub k46: [u8; 8],
    pub k47: [u16; 8],
    pub objcha: bool,
    pub sprite_ram: Vec<u16>,
    pub sprite_ram_cpu: Vec<u16>,
    /// K053251 registers
    pub k51: [u8; 16],
    /// palette RAM (0x1000)
    #[serde(with = "serde_bytes")]
    pub palette: Vec<u8>,

    #[serde(skip)] tiles_rom: Vec<u8>,
    #[serde(skip)] sprite_rom: Vec<u8>,
    #[serde(skip)] pens: Vec<u16>,
    #[serde(skip)] pri: Vec<u8>,
    #[serde(skip)] pub framebuffer: Vec<u8>,
}

/// Builds a region from word-interleaved ROMs (MAME ROM_LOAD32_WORD / ROM_LOAD64_WORD):
/// ROM k's words go to bytes 2k, 2k+1 of every `2 * roms.len()`-byte group.
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

/// x offset (in bits) of sprite pixel x within its 64-bit row (MAME spritelayout)
const SPR_XBITS: [usize; 16] = [8, 12, 0, 4, 24, 28, 16, 20, 40, 44, 32, 36, 56, 60, 48, 52];

impl Video {
    pub fn new(tiles_rom: Vec<u8>, sprite_rom: Vec<u8>) -> Self {
        Self {
            vram: vec![0; 0x4000],
            charrombank: [0; 4], charrombank_2: [0; 4],
            romsubbank: 0, irq_control: 0, addrmap: 0, tileflip: 0, rmrd: false,
            k46: [0; 8], k47: [0; 8], objcha: false,
            sprite_ram: vec![0; 0x800],
            sprite_ram_cpu: vec![0; 0x800],
            k51: [0; 16],
            palette: vec![0; 0x1000],
            tiles_rom, sprite_rom,
            pens: vec![0; SCREEN_W * SCREEN_H],
            pri: vec![0; SCREEN_W * SCREEN_H],
            framebuffer: vec![0; SCREEN_W * SCREEN_H * 4],
        }
    }

    pub fn keep_static(&mut self, old: &mut Self) {
        self.tiles_rom = std::mem::take(&mut old.tiles_rom);
        self.sprite_rom = std::mem::take(&mut old.sprite_rom);
        self.pens = std::mem::take(&mut old.pens);
        self.pri = std::mem::take(&mut old.pri);
        self.framebuffer = std::mem::take(&mut old.framebuffer);
    }

    pub fn reset(&mut self) {
        self.rmrd = false;
        // K052109 reset writes 0 to 1C00-1F00
        for off in (0x1C00..=0x1F00).step_by(0x80) { self.k052109_write(off, 0); }
        self.charrombank = [0; 4];
        self.charrombank_2 = [0; 4];
        self.k46 = [0; 8];
        self.k47 = [0; 8];
        self.objcha = false;
        self.k51 = [0; 16];
    }

    // ---------------------------------------------------------------- K052109

    /// The game's own tile code callback: colour bits 0-5 extend the code, bits 6-7 pick
    /// one of 4 palettes after the layer's base. Returns (code, palette).
    fn tile_callback(&self, layer: usize, bank: u8, code: u32, color: u8) -> (u32, u32) {
        let code = code | ((color as u32 & 0x3F) << 8) | ((bank as u32) << 14);
        let pal = self.layer_colorbase(layer) + ((color as u32 & 0xC0) >> 6);
        (code, pal)
    }

    pub fn k052109_read(&self, off: u16) -> u8 {
        let off = off as usize & 0x3FFF;
        if !self.rmrd { return self.vram[off]; }
        // RMRD: the CPU reads the character ROM (ROM test)
        let code = ((off & 0x1FFF) >> 5) as u32;
        let color = self.romsubbank;
        let sel = ((color & 0x0C) >> 2) as usize;
        let bank = (self.charrombank[sel] >> 2) | (self.charrombank_2[sel] >> 2);
        let (code, _) = self.tile_callback(0, bank, code, color);
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
            0x1D00 => self.irq_control = d,   // the board clears the IRQ on a 1 -> 0 of bit 2
            0x1D80 => { self.charrombank[0] = d & 0x0F; self.charrombank[1] = d >> 4; }
            0x1E00 | 0x3E00 => self.romsubbank = d,
            0x1E80 => self.tileflip = d & 0x07,
            0x1F00 => { self.charrombank[2] = d & 0x0F; self.charrombank[3] = d >> 4; }
            0x3D80 => { self.charrombank_2[0] = d & 0x0F; self.charrombank_2[1] = d >> 4; }
            0x3F00 => { self.charrombank_2[2] = d & 0x0F; self.charrombank_2[3] = d >> 4; }
            _ => {}
        }
    }

    pub fn irq_enabled(&self) -> bool { self.irq_control & 0x04 != 0 }

    // ---------------------------------------------------------------- K053246 / K053247

    pub fn k053246_write(&mut self, off: u16, d: u8) { self.k46[off as usize & 7] = d; }

    /// Sprite ROM readback (OBJCHA), for the ROM test.
    pub fn k053246_read(&self, off: u16) -> u8 {
        if !self.objcha { return 0; }
        let k = &self.k46;
        let addr = ((k[6] as usize) << 17) | ((k[7] as usize) << 9) | ((k[4] as usize) << 1)
            | ((off as usize & 1) ^ 1);
        let len = self.sprite_rom.len().max(1);
        self.sprite_rom.get(addr & (len - 1)).copied().unwrap_or(0)
    }

    pub fn obj_dma_enabled(&self) -> bool { self.k46[5] & 0x10 != 0 }

    // ---------------------------------------------------------------- K053251

    pub fn k053251_write(&mut self, off: u16, d: u8) { self.k51[off as usize & 0x0F] = d & 0x3F; }

    /// Palette bank base (in 16-colour units) of CI0..CI4.
    fn palette_index(&self, ci: usize) -> u32 {
        let (r9, r10) = (self.k51[9] as u32, self.k51[10] as u32);
        match ci {
            0 => 32 * (r9 & 3),
            1 => 32 * ((r9 >> 2) & 3),
            2 => 32 * ((r9 >> 4) & 3),
            3 => 16 * (r10 & 7),
            _ => 16 * ((r10 >> 3) & 7),
        }
    }

    /// Tilemap layers 0-2 use CI2-CI4.
    fn layer_colorbase(&self, layer: usize) -> u32 { self.palette_index(2 + layer) }

    // ---------------------------------------------------------------- palette

    fn rgb(&self, pen: u16) -> [u8; 4] {
        let i = (pen as usize & 0x7FF) * 2;
        let v = (self.palette[i] as u16) << 8 | self.palette[i + 1] as u16;
        let c = |n: u16| { let n = (n & 0x1F) as u8; (n << 3) | (n >> 2) };
        let (mut r, mut g, mut b) = (c(v), c(v >> 5), c(v >> 10));
        if pen & SHADOW != 0 {
            // MAME's default shadow: 60% brightness
            let s = |x: u8| (x as f32 * 0.6 + 0.5) as u8;
            r = s(r); g = s(g); b = s(b);
        }
        [r, g, b, 255]
    }

    // ---------------------------------------------------------------- drawing

    /// Draws the whole frame.
    pub fn render(&mut self) {
        let bg = (16 * self.palette_index(0)) as u16;
        self.pens.fill(bg);
        self.pri.fill(0);

        // sort the layers back to front by their K053251 priority (bigger = further back)
        let mut layer = [0usize, 1, 2];
        let mut lpri = [self.k51[2], self.k51[3], self.k51[4]];
        for (a, b) in [(0, 1), (0, 2), (1, 2)] {
            if lpri[a] < lpri[b] {
                lpri.swap(a, b);
                layer.swap(a, b);
            }
        }
        for (i, &l) in layer.iter().enumerate() {
            self.draw_layer(l, 1 << i);
        }
        self.draw_sprites(lpri);

        let mut fb = std::mem::take(&mut self.framebuffer);
        for (px, out) in self.pens.iter().zip(fb.chunks_exact_mut(4)) {
            out.copy_from_slice(&self.rgb(*px));
        }
        self.framebuffer = fb;
    }

    fn tile_pixel(&self, code: u32, x: usize, y: usize) -> u8 {
        let n = (self.tiles_rom.len() / 32).max(1);
        let base = (code as usize % n) * 32 + y * 4;
        let Some(b) = self.tiles_rom.get(base..base + 4) else { return 0 };
        let bit = |v: u8| (v >> (7 - x)) & 1;
        bit(b[3]) << 3 | bit(b[2]) << 2 | bit(b[1]) << 1 | bit(b[0])
    }

    /// Tilemap pixel (x, y of the 512 x 256 map) of layer `l`: palette pen, None = clear.
    fn layer_pixel(&self, l: usize, tx: usize, ty: usize) -> Option<u16> {
        let idx = (ty >> 3) * 64 + (tx >> 3);
        let code = self.vram[0x2000 + l * 0x800 + idx] as u32;
        let mut color = self.vram[l * 0x800 + idx];
        let mut bank = self.charrombank[((color & 0x0C) >> 2) as usize];
        if self.addrmap & 0x40 == 0 {
            color = (color & 0xF3) | ((bank & 3) << 2);
        }
        bank >>= 2;
        let flipy = color & 0x02 != 0 && self.tileflip & 0x04 != 0;
        let (code, pal) = self.tile_callback(l, bank, code, color);
        let py = if flipy { 7 - (ty & 7) } else { ty & 7 };
        let pen = self.tile_pixel(code, tx & 7, py);
        if pen == 0 { None } else { Some((pal as u16 * 16 + pen as u16) & 0x7FF) }
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
            // mixed: row scroll by screen line bands, column scroll by map column
            let (rh, step) = if rows == 256 { (1, 2) } else { (8, 16) };
            xs = ram_x(step * (yl / rh));
            let tx = ((x + 90 + xs) & 511) as i32;
            let col = (((tx & !7) - (xs & !7)) >> 3) & 63;
            ys = ram_y(col as usize);
        }
        (((x + 90 + xs) & 511) as usize, ((y + ys) & 255) as usize)
    }

    fn draw_layer(&mut self, l: usize, pri: u8) {
        let flip = self.tileflip & 1 != 0;
        for sy in 0..SCREEN_H {
            for sx in 0..SCREEN_W {
                let (mut x, mut y) = (sx as i32, (sy + FIRST_LINE) as i32);
                if flip { x = RASTER_W - 1 - x; y = RASTER_H - 1 - y; }
                let (tx, ty) = self.layer_coords(l, x, y);
                if let Some(pen) = self.layer_pixel(l, tx, ty) {
                    let i = sy * SCREEN_W + sx;
                    self.pens[i] = pen;
                    self.pri[i] |= pri;
                }
            }
        }
    }

    fn sprite_pixel(&self, code: u32, x: usize, y: usize) -> u8 {
        let n = (self.sprite_rom.len() / 128).max(1);
        let xb = SPR_XBITS[x];
        let i = (code as usize % n) * 128 + y * 8 + xb / 8;
        let b = self.sprite_rom.get(i).copied().unwrap_or(0);
        if xb % 8 == 0 { b >> 4 } else { b & 0x0F }
    }

    /// K053247 sprites, front to back (MAME k053247_sprites_draw_common and
    /// k053247_draw_single_sprite_gxcore, non-GX path).
    fn draw_sprites(&mut self, lpri: [u8; 3]) {
        let ram = &self.sprite_ram;
        let mut list: Vec<usize> = (0..0x800).step_by(8).filter(|&o| ram[o] & 0x8000 != 0).collect();
        // selection sort by Z code, exactly as MAME (ties keep MAME's order)
        let w = list.len();
        let ascending = self.k47[6] & 0x10 != 0;
        for y in 0..w.saturating_sub(1) {
            let mut offs = list[y];
            let mut z = ram[offs] & 0xFF;
            for x in y + 1..w {
                let t = list[x];
                let c = ram[t] & 0xFF;
                let swap = if ascending { z >= c } else { z <= c };
                if swap {
                    z = c;
                    list[x] = offs;
                    list[y] = t;
                    offs = t;
                }
            }
        }
        let colorbase = self.palette_index(1);
        for &offs in list.iter().rev() {
            let attr = self.sprite_ram[offs + 6];
            // simpsons sprite callback
            let p = ((attr & 0x0F80) >> 6) as u8;
            let primask: u32 = if p <= lpri[2] { 0 }
                else if p <= lpri[1] { 0xF0 }
                else if p <= lpri[0] { 0xF0 | 0xCC }
                else { 0xF0 | 0xCC | 0xAA };
            let color = colorbase + (attr as u32 & 0x1F);
            let shadow = (attr >> 10) & 3 != 0;
            self.draw_sprite(offs, color, primask, shadow);
        }
    }

    fn draw_sprite(&mut self, offs: usize, color: u32, primask: u32, shadow: bool) {
        const XOFF: [u32; 8] = [0, 1, 4, 5, 16, 17, 20, 21];
        const YOFF: [u32; 8] = [0, 2, 8, 10, 32, 34, 40, 42];
        let r = &self.sprite_ram;
        let mut code = r[offs + 1] as u32;
        let (mut xa, mut ya) = (0, 0);
        if code & 0x01 != 0 { xa += 1; }
        if code & 0x02 != 0 { ya += 1; }
        if code & 0x04 != 0 { xa += 2; }
        if code & 0x08 != 0 { ya += 2; }
        if code & 0x10 != 0 { xa += 4; }
        if code & 0x20 != 0 { ya += 4; }
        code &= !0x3F;

        let temp4 = r[offs];
        let mut oy = (r[offs + 2] & 0x3FF) as i32;
        let mut ox = (r[offs + 3] & 0x3FF) as i32;
        let zoom = |z: i32| if z != 0 { (0x400000 + (z >> 1)) / z } else { 0x800000 };
        let scaley = (r[offs + 4] & 0x3FF) as i32;
        let zoomy = zoom(scaley);
        let mut zoomx = if temp4 & 0x4000 == 0 { zoom((r[offs + 5] & 0x3FF) as i32) } else { zoomy };
        let mut flipx = temp4 & 0x1000 != 0;
        let mut flipy = temp4 & 0x2000 != 0;
        let t6 = r[offs + 6];
        let mirrorx = t6 & 0x4000 != 0;
        if mirrorx { flipx = false; }
        let mirrory = t6 & 0x8000 != 0;

        let flipscreenx = self.k46[5] & 0x01 != 0;
        let flipscreeny = self.k46[5] & 0x02 != 0;
        if self.k46[5] & 0x08 != 0 {
            // half-width sprites (Escape Kids)
            zoomx >>= 1;
            ox = (ox >> 1) + 1;
            if flipscreenx { ox += RASTER_W; }
        }
        if flipscreenx { ox = -ox; if !mirrorx { flipx = !flipx; } }
        if flipscreeny { oy = -oy; if !mirrory { flipy = !flipy; } }

        let (wrap, xlim, ylim) = if self.k47[6] & 0x40 != 0 { (512, 512 - 64, 512 - 128) }
            else { (1024, 1024 - 384, 1024 - 512) };
        let offx = ((self.k46[0] as u16) << 8 | self.k46[1] as u16) as i16 as i32;
        let offy = ((self.k46[2] as u16) << 8 | self.k46[3] as u16) as i16 as i32;
        ox = (ox - offx) & (wrap - 1);
        oy = (-oy - offy) & (wrap - 1);
        if ox >= xlim { ox -= wrap; }
        if oy >= ylim { oy -= wrap; }

        let size = (temp4 >> 8) & 0x0F;
        let width = 1i32 << (size & 3);
        let height = 1i32 << ((size >> 2) & 3);
        ox += SPRITE_DX;
        oy -= SPRITE_DY;
        // the coordinates are the sprite's centre
        ox -= (zoomx * width) >> 13;
        oy -= (zoomy * height) >> 13;

        for y in 0..height {
            let sy = oy + ((zoomy * y + (1 << 11)) >> 12);
            let zh = (oy + ((zoomy * (y + 1) + (1 << 11)) >> 12)) - sy;
            for x in 0..width {
                let sx = ox + ((zoomx * x + (1 << 11)) >> 12);
                let zw = (ox + ((zoomx * (x + 1) + (1 << 11)) >> 12)) - sx;
                let mut tc = code;
                let fx;
                if mirrorx {
                    if !flipx ^ ((x << 1) < width) {
                        tc += XOFF[((width - 1 - x + xa) & 7) as usize];
                        fx = true;
                    } else {
                        tc += XOFF[((x + xa) & 7) as usize];
                        fx = false;
                    }
                } else {
                    tc += if flipx { XOFF[((width - 1 - x + xa) & 7) as usize] } else { XOFF[((x + xa) & 7) as usize] };
                    fx = flipx;
                }
                let fy;
                if mirrory {
                    if !flipy ^ ((y << 1) >= height) {
                        tc += YOFF[((height - 1 - y + ya) & 7) as usize];
                        fy = true;
                    } else {
                        tc += YOFF[((y + ya) & 7) as usize];
                        fy = false;
                    }
                } else {
                    tc += if flipy { YOFF[((height - 1 - y + ya) & 7) as usize] } else { YOFF[((y + ya) & 7) as usize] };
                    fy = flipy;
                }
                self.zoom_tile(tc, color, fx, fy, sx, sy, zw, zh, primask, shadow);
                if mirrory && height == 1 {
                    // Simpsons shadows: drawn again upside down
                    self.zoom_tile(tc, color, fx, !fy, sx, sy, zw, zh, primask, shadow);
                }
            }
        }
    }

    /// MAME prio_zoom_transtable for one 16x16 tile drawn `dw` x `dh` pixels at raster
    /// (dx, dy). Pen 0 is clear, pen 15 is a shadow when `shadow` is set.
    #[allow(clippy::too_many_arguments)]
    fn zoom_tile(&mut self, code: u32, color: u32, flipx: bool, flipy: bool,
                 dx: i32, dy: i32, dw: i32, dh: i32, primask: u32, shadow: bool) {
        if dw < 1 || dh < 1 { return; }
        let pmask = primask | (1 << 31);
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
        let base = (color % 128) as u16 * 16;
        for y in y0..=y1 {
            let ty = (srcy >> 16) as usize;
            srcy += sdy;
            let row = (y as usize - FIRST_LINE) * SCREEN_W;
            let mut cx = srcx;
            for x in x0..=x1 {
                let pen = self.sprite_pixel(code, (cx >> 16) as usize, ty);
                cx += sdx;
                if pen == 0 { continue; }
                let i = row + x as usize;
                let p = self.pri[i];
                let hidden = (1u32 << (p & 0x1F)) & pmask != 0;
                if pen == 15 && shadow {
                    if p & 0x80 == 0 && !hidden {
                        self.pens[i] |= SHADOW;
                        self.pri[i] = p | 0x80;
                    }
                } else {
                    if !hidden { self.pens[i] = base + pen as u16; }
                    self.pri[i] = 31;
                }
            }
        }
    }
}
