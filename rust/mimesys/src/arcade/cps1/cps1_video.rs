//! CPS1 video: CPS-A / CPS-B. After MAME capcom/cps1_v.cpp (BSD-3-Clause, Paul Leaman et al.).
//!
//! Everything lives in "gfx RAM" (900000-92FFFF). CPS-A registers point into it:
//! three tilemaps (scroll 1 = 8x8 tiles, scroll 2 = 16x16 with optional line scroll,
//! scroll 3 = 32x32; each 64x64 tiles of {code, attribute} words), the sprite table
//! (copied one frame late), line scroll values ("other") and the palette, which CPS-B
//! copies to the real palette RAM when the palette base register is written.
//!
//! The CPS-B chip differs per game: where its ID, multiply protection, layer control,
//! priority masks and palette control registers sit comes from cps1_config.rs.
//!
//! Graphics ROM: 4 bits per pixel as 4 bitplanes, one byte per plane (bytes 3, 2, 1, 0 =
//! planes from the top bit down), 8 pixels per 4 bytes, the left pixel in bit 7. Rows are
//! 8 bytes (8x8 tiles use one half, chosen by the tile column), 8 bytes (16x16) or 16
//! bytes (32x32). Codes go through the B-board bank mapper (a PAL) before the ROM.
//!
//! The screen is 384x224 of a 512x262 raster (x 64-447, y 16-239), 8 MHz dot clock.

use serde::{Deserialize, Serialize};

use super::cps1_config::{Config, SCROLL1, SCROLL2, SCROLL3, SPRITES};

pub const SCREEN_W: usize = 384;
pub const SCREEN_H: usize = 224;
/// raster position of the top-left visible pixel
pub const X0: i32 = 64;
pub const Y0: i32 = 16;

// CPS-A registers (word index)
const OBJ_BASE: usize = 0x00 / 2;
const SCROLL1_BASE: usize = 0x02 / 2;
const SCROLL2_BASE: usize = 0x04 / 2;
const SCROLL3_BASE: usize = 0x06 / 2;
const OTHER_BASE: usize = 0x08 / 2;
const PALETTE_BASE: usize = 0x0A / 2;
const SCROLL1_X: usize = 0x0C / 2;
const STARS1_X: usize = 0x18 / 2;
const ROWSCROLL_OFFS: usize = 0x20 / 2;
const VIDEOCONTROL: usize = 0x22 / 2;

pub const GFXRAM_SIZE: usize = 0x30000;

/// How a tilemap is drawn.
#[derive(Clone, Copy)]
pub enum LayerMode<'a> {
    /// pens 0-14 into the picture
    Normal,
    /// CPS1: mark the layer's high-priority pens (per tile group) in the priority buffer
    High(&'a [u16; 4]),
    /// CPS2: draw like Normal and OR this value into the priority buffer
    Pri(u8),
}
const PALETTE_ENTRIES: usize = 0xC00;
/// background pen (games set it black)
const BG_PEN: u16 = 0xBFF;

#[derive(Serialize, Deserialize)]
pub struct Video {
    #[serde(with = "serde_bytes")]
    pub gfxram: Vec<u8>,
    pub cps_a: [u16; 0x20],
    pub cps_b: [u16; 0x20],
    /// sprite table as of the last vblank (CPS1 shows sprites a frame late)
    obj_buf: Vec<u16>,
    /// RGBA of each palette entry
    #[serde(skip, default = "palette_init")]
    palette: Vec<[u8; 4]>,
    /// raw palette words as last copied (the RGBA is rebuilt from them after a load)
    pal_raw: Vec<u16>,
    frame: u32,
    #[serde(skip)] gfx: Vec<u8>,
    #[serde(skip)] cfg: Option<Config>,
    #[serde(skip)] stars: i32,
    /// CPS-B raster counters (CPS2): two line counters and the in-line position
    pub raster_on: bool,
    pub raster_counter: [u16; 3],
    pub raster_reload: [u16; 3],
    #[serde(skip)] pub(crate) pens: Vec<u16>,
    #[serde(skip)] pub(crate) prio: Vec<u8>,
    #[serde(skip)] pub framebuffer: Vec<u8>,
}

fn palette_init() -> Vec<[u8; 4]> { vec![[0, 0, 0, 255]; PALETTE_ENTRIES] }

impl Video {
    pub fn new(gfx: Vec<u8>, cfg: Config, stars: i32) -> Self {
        let mut v = Self {
            gfxram: vec![0; GFXRAM_SIZE],
            cps_a: [0; 0x20],
            cps_b: [0; 0x20],
            obj_buf: vec![0; 0x400],
            palette: palette_init(),
            pal_raw: vec![0; PALETTE_ENTRIES],
            frame: 0,
            gfx, cfg: Some(cfg), stars,
            raster_on: false, raster_counter: [0; 3], raster_reload: [0x1FF, 0x1FF, 0],
            pens: vec![0; SCREEN_W * SCREEN_H],
            prio: vec![0; SCREEN_W * SCREEN_H],
            framebuffer: vec![0; SCREEN_W * SCREEN_H * 4],
        };
        v.reset();
        v
    }

    pub fn reset(&mut self) {
        self.gfxram.fill(0);
        self.cps_a = [0; 0x20];
        self.cps_b = [0; 0x20];
        // MAME's defaults
        self.cps_a[OBJ_BASE] = 0x9200;
        self.cps_a[SCROLL1_BASE] = 0x9000;
        self.cps_a[SCROLL2_BASE] = 0x9040;
        self.cps_a[SCROLL3_BASE] = 0x9080;
        self.cps_a[OTHER_BASE] = 0x9100;
        self.obj_buf.fill(0);
        self.raster_counter = [0; 3];
        self.raster_reload = [0x1FF, 0x1FF, 0];
    }

    /// Puts back what a save state doesn't carry.
    pub fn keep_static(&mut self, old: &mut Video) {
        self.gfx = std::mem::take(&mut old.gfx);
        self.cfg = old.cfg;
        self.stars = old.stars;
        self.pens = std::mem::take(&mut old.pens);
        self.prio = std::mem::take(&mut old.prio);
        self.framebuffer = std::mem::take(&mut old.framebuffer);
        for i in 0..PALETTE_ENTRIES { self.palette[i] = rgb(self.pal_raw[i]); }
    }

    #[cfg(test)]
    pub fn gfx_mut(&mut self) -> &mut Vec<u8> { &mut self.gfx }
    #[cfg(test)]
    pub fn palette_rgba(&self, e: usize) -> [u8; 4] { self.palette[e] }

    fn cfg(&self) -> &Config { self.cfg.as_ref().expect("CPS1 config") }

    // ---------------------------------------------------------------- gfx RAM

    pub fn gfxram_word(&self, byte: usize) -> u16 {
        let i = byte % GFXRAM_SIZE & !1;
        (self.gfxram[i] as u16) << 8 | self.gfxram[i + 1] as u16
    }

    /// Byte offset in gfx RAM a CPS-A base register points to (MAME cps1_base).
    pub(crate) fn base(&self, reg: usize, boundary: usize) -> usize {
        let b = (self.cps_a[reg] as usize * 256) & !(boundary - 1);
        b & 0x3FFFF
    }

    // ---------------------------------------------------------------- CPS-A / CPS-B

    pub fn cps_a_write(&mut self, offset: usize, v: u16, mask: u16) {
        let o = offset & 0x1F;
        self.cps_a[o] = (self.cps_a[o] & !mask) | (v & mask);
        // the palette is copied when its base register is written
        if o == PALETTE_BASE { self.build_palette(); }
    }

    pub fn cps_b_read(&self, offset: usize, in2: u16, in3: u16) -> u16 {
        let c = self.cfg();
        let o = (offset & 0x1F) as i16 * 2;
        let reg = |a: i16| self.cps_b[(a / 2) as usize & 0x1F] as u32;
        if o == c.cpsb.id_addr { return c.cpsb.id_value as u16; }
        if c.cpsb.mult_factor1 >= 0 {
            let prod = reg(c.cpsb.mult_factor1) * reg(c.cpsb.mult_factor2);
            if o == c.cpsb.mult_lo { return prod as u16; }
            if o == c.cpsb.mult_hi { return (prod >> 16) as u16; }
        }
        if c.in2 != 0 && o == c.in2 as i16 { return in2; }
        if c.in3 != 0 && o == c.in3 as i16 { return in3; }
        if self.raster_on {
            // (the in-line position counter is read as if at the start of the line)
            if o == 0x0E { return (self.raster_counter[2] << 1 & 0x1FE) | (self.cps_b[0x0E / 2] & 1); }
            if o == 0x10 || o == 0x12 { return self.raster_counter[(o as usize / 2) & 1] & 0x1FF; }
        }
        0xFFFF
    }

    pub fn cps_b_write(&mut self, offset: usize, v: u16, mask: u16) {
        let o = offset & 0x1F;
        self.cps_b[o] = (self.cps_b[o] & !mask) | (v & mask);
        if self.raster_on {
            let d = self.cps_b[o];
            if o == 0x0E / 2 { self.raster_reload[2] = d >> 1 & 0xFF; }
            if o == 0x10 / 2 || o == 0x12 / 2 {
                let i = o & 1;
                self.raster_reload[i] = d & 0x1FF;
                if d & 0x8000 != 0 { self.raster_counter[i] = self.raster_reload[i]; }
            }
        }
    }

    /// MAME raster_scanline: clocks the line counters at the start of raster line `line`.
    /// Returns the pixel delay (in 2-pixel units) of a raster interrupt on this line, if any.
    pub fn raster_line(&mut self, line: u32) -> Option<u16> {
        let mut pending = false;
        for i in 0..2 {
            self.raster_counter[i] = if line == 0 { self.raster_reload[i] } else { self.raster_counter[i].wrapping_sub(1) & 0x1FF };
            if self.raster_counter[i] == 0 { pending = true; }
        }
        self.raster_counter[2] = self.raster_reload[2];
        if pending { Some(self.raster_counter[2]) } else { None }
    }

    pub(crate) fn cps_b_reg(&self, addr: i16) -> u16 {
        if addr < 0 { 0 } else { self.cps_b[(addr / 2) as usize & 0x1F] }
    }

    // ---------------------------------------------------------------- palette

    /// MAME cps1_build_palette: the pages enabled in the palette control register are
    /// copied from gfx RAM; skipped pages only take space after the first copied one.
    fn build_palette(&mut self) {
        let ctrl = self.cps_b_reg(self.cfg().cpsb.palette_control);
        let start = self.base(PALETTE_BASE, 0x400);
        let mut src = start;
        for page in 0..6 {
            if ctrl & (1 << page) != 0 {
                for i in 0..0x200 {
                    let w = self.gfxram_word(src);
                    src += 2;
                    let e = page * 0x200 + i;
                    self.pal_raw[e] = w;
                    self.palette[e] = rgb(w);
                }
            } else if src != start {
                src += 0x400;
            }
        }
    }

    // ---------------------------------------------------------------- frame timing

    /// Vblank: the sprite table is latched for the next frame.
    pub fn vblank(&mut self) {
        let base = self.base(OBJ_BASE, 0x800);
        for i in 0..0x400 { self.obj_buf[i] = self.gfxram_word(base + i * 2); }
        self.frame = self.frame.wrapping_add(1);
    }

    // ---------------------------------------------------------------- graphics ROM

    /// MAME gfxrom_bank_mapper: the B-board PAL turns a layer's tile code into a ROM tile
    /// number, or None when the code isn't mapped for that layer (drawn transparent).
    pub fn bank_map(&self, ty: u8, code: u32) -> Option<u32> {
        let m = &self.cfg().mapper;
        let shift = match ty { SPRITES => 1, SCROLL1 => 0, SCROLL2 => 1, _ => 3 };
        let code = code << shift;
        for r in m.ranges {
            if code >= r.start && code <= r.end && r.types & ty != 0 {
                let base: u32 = m.bank_sizes[..r.bank as usize].iter().sum();
                let size = m.bank_sizes[r.bank as usize];
                return Some((base + (code & size.wrapping_sub(1))) >> shift);
            }
        }
        None
    }

    /// Pen of pixel (x, y) of a `size`x`size` tile (8, 16 or 32). `half` picks the right
    /// half of an 8x8 tile's 8-byte rows.
    #[inline]
    pub(crate) fn tile_pen(&self, code: u32, size: usize, half: usize, x: usize, y: usize) -> u8 {
        let tile_bytes = size * size / 2;
        let row = (size / 2).max(8);
        let o = code as usize * tile_bytes.max(64) + y * row + ((x >> 3) + half) * 4;
        let Some(b) = self.gfx.get(o..o + 4) else { return 15 };
        let bit = 7 - (x & 7);
        ((b[3] >> bit) & 1) << 3 | ((b[2] >> bit) & 1) << 2 | ((b[1] >> bit) & 1) << 1 | ((b[0] >> bit) & 1)
    }

    // ---------------------------------------------------------------- rendering

    /// The layer control register (draw order in bits 6-13).
    pub(crate) fn layer_control(&self) -> u16 { self.cps_b_reg(self.cfg().cpsb.layer_control) }

    /// Whether scroll 1-3 are on (layer control enables + video control bits).
    pub(crate) fn layer_enables(&self) -> [bool; 3] {
        let c = self.cfg();
        let video = self.cps_a[VIDEOCONTROL];
        let layer_ctrl = self.layer_control();
        [
            layer_ctrl & c.cpsb.layer_enable[0] != 0,
            layer_ctrl & c.cpsb.layer_enable[1] != 0 && video & 0x04 != 0,
            layer_ctrl & c.cpsb.layer_enable[2] != 0 && video & 0x08 != 0,
        ]
    }

    /// Clears screen lines y0..y1 to the background pen and priority 0.
    pub(crate) fn clear_lines(&mut self, y0: usize, y1: usize) {
        self.pens[y0 * SCREEN_W..y1 * SCREEN_W].fill(BG_PEN);
        self.prio[y0 * SCREEN_W..y1 * SCREEN_W].fill(0);
    }

    /// Pens of lines y0..y1 -> RGBA (flip screen mirrors the whole visible area).
    pub(crate) fn output_lines(&mut self, y0: usize, y1: usize) {
        let flip = self.cps_a[VIDEOCONTROL] & 0x8000 != 0;
        for y in y0..y1 {
            for x in 0..SCREEN_W {
                let (sx, sy) = if flip { (SCREEN_W - 1 - x, SCREEN_H - 1 - y) } else { (x, y) };
                let p = self.palette[self.pens[sy * SCREEN_W + sx] as usize % PALETTE_ENTRIES];
                let o = (y * SCREEN_W + x) * 4;
                self.framebuffer[o..o + 4].copy_from_slice(&p);
            }
        }
    }

    pub fn render(&mut self) {
        let c = *self.cfg();
        let layer_ctrl = self.layer_control();
        let enabled = self.layer_enables();
        let stars_on = [layer_ctrl & c.cpsb.layer_enable[3] != 0, layer_ctrl & c.cpsb.layer_enable[4] != 0];
        // pens a layer's tiles count as "in front of sprites", per tile group
        let mut high = [0u16; 4];
        for (g, h) in high.iter_mut().enumerate() {
            *h = if c.cpsb.priority[g] >= 0 { self.cps_b_reg(c.cpsb.priority[g]) } else { 0 };
        }

        self.clear_lines(0, SCREEN_H);
        if self.stars >= 0 { self.draw_stars(stars_on); }

        let order = [(layer_ctrl >> 6) & 3, (layer_ctrl >> 8) & 3, (layer_ctrl >> 10) & 3, (layer_ctrl >> 12) & 3];
        for k in 0..4 {
            self.draw_layer(order[k] as usize, &enabled, &high, false);
            // sprites next: the high-priority pens of this layer will hide them
            if k < 3 && order[k + 1] == 0 {
                self.draw_layer(order[k] as usize, &enabled, &high, true);
            }
        }

        self.output_lines(0, SCREEN_H);
    }

    /// layer 0 = sprites, 1-3 = scroll 1-3. `high_only`: mark the pixels of the layer's
    /// high-priority pens in the priority buffer instead of drawing.
    fn draw_layer(&mut self, layer: usize, enabled: &[bool; 3], high: &[u16; 4], high_only: bool) {
        match layer {
            0 => if !high_only { self.draw_sprites() },
            1..=3 if enabled[layer - 1] => {
                let mode = if high_only { LayerMode::High(high) } else { LayerMode::Normal };
                self.draw_scroll(layer, mode, 0, SCREEN_H)
            }
            _ => {}
        }
    }

    /// Draws tilemap `layer` (1-3) on screen lines y0..y1.
    pub(crate) fn draw_scroll(&mut self, layer: usize, mode: LayerMode, y0: usize, y1: usize) {
        let (size, gtype, reg, pal_base) = match layer {
            1 => (8usize, SCROLL1, SCROLL1_BASE, 0x20u16),
            2 => (16, SCROLL2, SCROLL2_BASE, 0x40),
            _ => (32, SCROLL3, SCROLL3_BASE, 0x60),
        };
        let base = self.base(reg, 0x4000);
        let sx = self.cps_a[SCROLL1_X + (layer - 1) * 2] as i32;
        let sy = self.cps_a[SCROLL1_X + (layer - 1) * 2 + 1] as i32;
        let wrap = (size * 64) as i32 - 1;
        let line_scroll = layer == 2 && self.cps_a[VIDEOCONTROL] & 1 != 0;
        let other = self.base(OTHER_BASE, 0x800);
        let row_offs = self.cps_a[ROWSCROLL_OFFS] as usize;
        for y in y0..y1 {
            let ry = y as i32 + Y0;
            let ty = ((ry + sy) & wrap) as usize;
            let mut lsx = sx;
            if line_scroll {
                let i = ry as usize & 0xFF;
                lsx = sx.wrapping_add(self.gfxram_word(other + ((i + row_offs) & 0x3FF) * 2) as i32);
            }
            let mut cached_tile = usize::MAX;
            let (mut code, mut attr) = (None, 0u16);
            for x in 0..SCREEN_W {
                let tx = ((x as i32 + X0 + lsx) & wrap) as usize;
                let (col, row) = (tx / size, ty / size);
                let idx = match layer {
                    1 => (row & 0x1F) + ((col & 0x3F) << 5) + ((row & 0x20) << 6),
                    2 => (row & 0x0F) + ((col & 0x3F) << 4) + ((row & 0x30) << 6),
                    _ => (row & 0x07) + ((col & 0x3F) << 3) + ((row & 0x38) << 6),
                };
                if idx != cached_tile {
                    cached_tile = idx;
                    let mut raw = self.gfxram_word(base + idx * 4) as u32;
                    if layer == 3 { raw &= 0x3FFF; }
                    attr = self.gfxram_word(base + idx * 4 + 2);
                    code = self.bank_map(gtype, raw);
                }
                let (mut px, mut py) = (tx % size, ty % size);
                if attr & 0x20 != 0 { px = size - 1 - px; }
                if attr & 0x40 != 0 { py = size - 1 - py; }
                let half = if layer == 1 { (idx >> 5) & 1 } else { 0 };
                let pen = match code { Some(c) => self.tile_pen(c, size, half, px, py), None => 15 };
                let o = y * SCREEN_W + x;
                match mode {
                    LayerMode::High(high) => {
                        let group = ((attr >> 7) & 3) as usize;
                        if high[group] & (1 << pen) != 0 { self.prio[o] = 1; }
                    }
                    _ if pen == 15 => {}
                    LayerMode::Normal => self.pens[o] = (pal_base + (attr & 0x1F)) * 16 + pen as u16,
                    LayerMode::Pri(p) => {
                        self.pens[o] = (pal_base + (attr & 0x1F)) * 16 + pen as u16;
                        self.prio[o] |= p;
                    }
                }
            }
        }
    }

    /// MAME find_last_sprite + cps1_render_sprites: drawn in table order, but a pixel can't be
    /// drawn twice, so earlier sprites are in front. Hidden
    /// where a layer drawn below marked high-priority pixels.
    fn draw_sprites(&mut self) {
        let mut count = 0x100;
        for i in 0..0x100 {
            if self.obj_buf[i * 4 + 3] & 0xFF00 == 0xFF00 { count = i; break; }
        }
        for i in 0..count {
            let e = &self.obj_buf[i * 4..i * 4 + 4];
            let (x, y, raw, colour) = (e[0] as i32, e[1] as i32, e[2] as u32, e[3]);
            let Some(code) = self.bank_map(SPRITES, raw) else { continue };
            let col = colour & 0x1F;
            let flipx = colour & 0x20 != 0;
            let flipy = colour & 0x40 != 0;
            let (nx, ny) = if colour & 0xFF00 != 0 {
                (((colour >> 8) & 0xF) as u32 + 1, ((colour >> 12) & 0xF) as u32 + 1)
            } else { (1, 1) };
            for nys in 0..ny {
                for nxs in 0..nx {
                    let cx = if flipx { nx - 1 - nxs } else { nxs };
                    let cy = if flipy { ny - 1 - nys } else { nys };
                    // blocks wrap within a row of 16 tiles
                    let c = if nx * ny > 1 { (code & !0xF) + ((code + cx) & 0xF) + 0x10 * cy } else { code };
                    let sx = (x + nxs as i32 * 16) & 0x1FF;
                    let sy = (y + nys as i32 * 16) & 0x1FF;
                    self.draw_sprite_tile(c, col, flipx, flipy, sx, sy);
                }
            }
        }
    }

    fn draw_sprite_tile(&mut self, code: u32, col: u16, flipx: bool, flipy: bool, sx: i32, sy: i32) {
        for py in 0..16 {
            let y = sy + py as i32 - Y0;
            if y < 0 || y >= SCREEN_H as i32 { continue; }
            for px in 0..16 {
                let x = sx + px as i32 - X0;
                if x < 0 || x >= SCREEN_W as i32 { continue; }
                let tx = if flipx { 15 - px } else { px };
                let ty = if flipy { 15 - py } else { py };
                let pen = self.tile_pen(code, 16, 0, tx, ty);
                if pen == 15 { continue; }
                let o = y as usize * SCREEN_W + x as usize;
                if self.prio[o] != 0 { continue; }
                self.pens[o] = col * 16 + pen as u16;
                // MAME prio_transpen marks drawn pixels (priority 31) and never draws over
                // them, so the first sprite in the table ends up on top
                self.prio[o] = 31;
            }
        }
    }

    /// MAME cps1_render_stars: two star fields from ROM, blinking by frame count.
    fn draw_stars(&mut self, on: [bool; 2]) {
        for (field, &enabled) in on.iter().enumerate() {
            if !enabled { continue; }
            // field 0 (enable mask 3) uses stars 2's scroll and bytes 4 of each 8
            let (byte, scroll, pal) = if field == 0 { (4, STARS1_X + 2, 0xA00) } else { (0, STARS1_X, 0x800) };
            let (sxr, syr) = (self.cps_a[scroll] as i32, self.cps_a[scroll + 1] as i32);
            for offs in 0..0x1000usize {
                let Some(&colb) = self.gfx.get(self.stars as usize + 8 * offs + byte) else { continue };
                if colb & 0x1F == 0x0F { continue; }
                let sx = (((offs / 256) * 32) as i32 - sxr + (colb & 0x1F) as i32) & 0x1FF;
                let sy = ((offs % 256) as i32 - syr) & 0xFF;
                let cnt = (self.frame / 16) % if colb & 0x80 != 0 { 15 } else { 16 };
                let col = ((colb & 0xE0) >> 1) as u32 + cnt;
                let (x, y) = (sx - X0, sy - Y0);
                if x >= 0 && x < SCREEN_W as i32 && y >= 0 && y < SCREEN_H as i32 {
                    self.pens[y as usize * SCREEN_W + x as usize] = pal + col as u16;
                }
            }
        }
    }
}

/// 12-bit RGB plus a 4-bit brightness (0 = about a third). MAME cps1_build_palette.
pub fn rgb(w: u16) -> [u8; 4] {
    let bright = 0x0F + ((w >> 12) << 1) as u32;
    let c = |v: u16| ((v & 0x0F) as u32 * 0x11 * bright / 0x2D) as u8;
    [c(w >> 8), c(w >> 4), c(w), 255]
}
