// Galaxian / Moon Cresta video.
//
// 256 x 224 visible (lines 16-239 of a 264-line frame), rotated 90 degrees on the cabinet.
//   - Tiles: 32 x 32 grid of 8x8, 2 bpp. Each COLUMN has its own vertical scroll and
//     color (object RAM 0x00-0x3F, pairs of scroll/color).
//   - Sprites: 8 of 16x16, 2 bpp (object RAM 0x40-0x5F, 4 bytes each).
//   - Bullets: 7 white "shells" + 1 yellow "missile", 4 pixels wide (object RAM 0x60-0x7F).
//   - Stars: a 17-bit LFSR running at the pixel clock, scrolling slowly sideways.
// Rendered one scanline at a time so mid-frame changes show up like on hardware.
//
// The board runs a 3x horizontal pixel clock (768 per line) for the star field; we draw
// at 256 and fold each star's sub-pixels into one pixel.
//
// Reference: MAME src/mame/galaxian/galaxian_v.cpp

pub const SCREEN_W: usize = 256;
pub const SCREEN_H: usize = 224;
pub const FIRST_LINE: usize = 16;       // VBEND
pub const VBLANK_START: usize = 240;    // VBSTART
const STAR_PERIOD: u32 = (1 << 17) - 1;
const RGB_MAXIMUM: f64 = 224.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Extension {
    None,
    MoonCresta,   // gfx banking through the three latches at A000-A002
    Frogger,      // nibble-swapped scroll/sprite Y, remapped colors, blue river, no stars/bullets
    Turtles,      // Amidar/Turtles: solid background color from 3 latches, no stars
}

pub type VideoState = (Vec<u8>, Vec<u8>, bool, bool, [u8; 3], [bool; 3], bool, u32);

pub struct GalaxianVideo {
    pub video_ram: Vec<u8>,         // 0x400 tile codes
    pub obj_ram: Vec<u8>,           // 0x100: column scroll/color, sprites, bullets
    pub flip_x: bool,
    pub flip_y: bool,
    pub gfx_bank: [u8; 3],
    pub extension: Extension,
    pub background: [bool; 3],      // Turtles: red, green, blue background latches

    stars_enabled: bool,
    star_origin: u32,
    stars: Vec<u8>,                 // precomputed LFSR: bit 7 = star on, bits 0-5 = color
    star_color: [[u8; 3]; 64],

    tiles: Vec<[u8; 64]>,
    sprites: Vec<[u8; 256]>,
    palette: [[u8; 3]; 32],

    pub framebuffer: Vec<u8>,       // RGBA, SCREEN_W x SCREEN_H
}

impl GalaxianVideo {
    pub fn new(gfx1: &[u8], proms: &[u8], extension: Extension) -> Result<Self, String> {
        if gfx1.len() < 0x1000 || gfx1.len() % 0x1000 != 0 {
            return Err(format!("gfx1 is 0x{:X} bytes, expected 0x1000 or 0x2000", gfx1.len()));
        }
        if proms.len() < 0x20 {
            return Err(format!("proms is 0x{:X} bytes, expected 0x20", proms.len()));
        }
        // Both layouts: plane 0 (high bit) in the first half of gfx1, plane 1 in the second half.
        let half = gfx1.len() / 2;
        let px = |byte: usize, bit: usize| -> u8 {
            (((gfx1[byte] >> (7 - bit)) & 1) << 1) | ((gfx1[half + byte] >> (7 - bit)) & 1)
        };
        let tiles = (0..half / 8).map(|code| {
            let mut t = [0u8; 64];
            for y in 0..8 { for x in 0..8 { t[y * 8 + x] = px(code * 8 + y, x); } }
            t
        }).collect();
        // 16x16: right half 8 bytes later, bottom half 16 bytes later
        let sprites = (0..half / 32).map(|code| {
            let mut s = [0u8; 256];
            for y in 0..16 {
                for x in 0..16 {
                    let byte = code * 32 + (y & 7) + if y >= 8 { 16 } else { 0 } + if x >= 8 { 8 } else { 0 };
                    s[y * 16 + x] = px(byte, x & 7);
                }
            }
            s
        }).collect();

        Ok(Self {
            video_ram: vec![0; 0x400],
            obj_ram: vec![0; 0x100],
            flip_x: false,
            flip_y: false,
            gfx_bank: [0; 3],
            extension,
            background: [false; 3],
            stars_enabled: false,
            star_origin: 0,
            stars: build_stars(),
            star_color: build_star_colors(),
            tiles,
            sprites,
            palette: build_palette(&proms[..0x20]),
            framebuffer: vec![0; SCREEN_W * SCREEN_H * 4],
        })
    }

    /// 7004 / B004. On the rising edge the star shift register restarts from "now".
    /// Save state: RAMs and latches (graphics and star tables are rebuilt from ROMs).
    pub fn save(&self) -> VideoState {
        (self.video_ram.clone(), self.obj_ram.clone(), self.flip_x, self.flip_y, self.gfx_bank,
         self.background, self.stars_enabled, self.star_origin)
    }

    pub fn load(&mut self, s: VideoState) -> Result<(), String> {
        if s.0.len() != self.video_ram.len() || s.1.len() != self.obj_ram.len() {
            return Err("video RAM size mismatch".into());
        }
        (self.video_ram, self.obj_ram, self.flip_x, self.flip_y, self.gfx_bank,
         self.background, self.stars_enabled, self.star_origin) = s;
        Ok(())
    }

    pub fn set_stars_enabled(&mut self, on: bool, line: usize) {
        if on && !self.stars_enabled {
            self.star_origin = (STAR_PERIOD - (line as u32 * 512) % STAR_PERIOD) % STAR_PERIOD;
        }
        self.stars_enabled = on;
    }

    /// Once per frame: the shift register runs 2^17 clocks per frame (+/-1), so the
    /// field creeps sideways one step every frame.
    pub fn end_of_frame(&mut self) {
        self.star_origin = if self.flip_x {
            (self.star_origin + 1) % STAR_PERIOD
        } else {
            (self.star_origin + STAR_PERIOD - 1) % STAR_PERIOD
        };
    }

    fn tile_code(&self, code: u16) -> usize {
        let mut code = code;
        if self.extension == Extension::MoonCresta && self.gfx_bank[2] != 0 && (code & 0xC0) == 0x80 {
            code = (code & 0x3F) | ((self.gfx_bank[0] as u16) << 6) | ((self.gfx_bank[1] as u16) << 7) | 0x100;
        }
        code as usize % self.tiles.len()
    }

    fn sprite_code(&self, code: u16) -> usize {
        let mut code = code;
        if self.extension == Extension::MoonCresta && self.gfx_bank[2] != 0 && (code & 0x30) == 0x20 {
            code = (code & 0x0F) | ((self.gfx_bank[0] as u16) << 4) | ((self.gfx_bank[1] as u16) << 5) | 0x40;
        }
        code as usize % self.sprites.len()
    }

    /// Frogger: the top and bottom 4 bits are swapped going into the position adder.
    fn frogger_swap(&self, v: u8) -> u8 {
        if self.extension == Extension::Frogger { v.rotate_left(4) } else { v }
    }

    /// Frogger: color bits are wired 0->2, 1->0, 2->1.
    fn frogger_color(&self, c: u8) -> u8 {
        if self.extension == Extension::Frogger { ((c >> 1) & 3) | ((c << 2) & 4) } else { c }
    }

    /// Draw screen line `y` (16-239) with the current state.
    pub fn render_line(&mut self, y: usize) {
        if !(FIRST_LINE..VBLANK_START).contains(&y) { return; }
        let mut line = [[0u8; 3]; SCREEN_W];

        // 1. background: Frogger's river is a blue half (no stars), Turtles a solid
        //    color (390/470/390 ohm), others black + stars
        if self.extension == Extension::Turtles {
            let [r, g, b] = self.background;
            line = [[r as u8 * 0x55, g as u8 * 0x47, b as u8 * 0x55]; SCREEN_W];
        } else if self.extension == Extension::Frogger {
            let blue = [0, 0, 0x47];
            for (x, px) in line.iter_mut().enumerate() {
                let river = if self.flip_x { x >= 128 } else { x < 128 };
                if river { *px = blue; }
            }
        } else if self.stars_enabled {
            let mut offs = ((self.star_origin as u64 + y as u64 * 512) % STAR_PERIOD as u64) as usize;
            for x in 0..SCREEN_W {
                let enable = (y ^ (x >> 3)) & 1 != 0;
                let first = self.stars[offs];
                offs = (offs + 1) % STAR_PERIOD as usize;
                let second = self.stars[offs];
                offs = (offs + 1) % STAR_PERIOD as usize;
                if enable {
                    // the second clock covers 2 of the 3 sub-pixels, so it wins
                    if second & 0x80 != 0 { line[x] = self.star_color[(second & 0x3F) as usize]; }
                    else if first & 0x80 != 0 { line[x] = self.star_color[(first & 0x3F) as usize]; }
                }
            }
        }

        // 2. tiles, per-column scroll, pen 0 transparent
        for x in 0..SCREEN_W {
            let tx = if self.flip_x { 255 - x } else { x };
            let ty = if self.flip_y { 255 - y } else { y };
            let col = tx / 8;
            let scroll = self.frogger_swap(self.obj_ram[col * 2]) as usize;
            let row_px = (ty + scroll) & 0xFF;
            let code = self.tile_code(self.video_ram[(row_px / 8) * 32 + col] as u16);
            let color = self.frogger_color(self.obj_ram[col * 2 + 1] & 7) as usize;
            let p = self.tiles[code][(row_px & 7) * 8 + (tx & 7)];
            if p != 0 { line[x] = self.palette[color * 4 + p as usize]; }
        }

        // 3. sprites 7 down to 0, so sprite 0 ends up on top
        for n in (0..8).rev() {
            let b = &self.obj_ram[0x40 + n * 4..0x40 + n * 4 + 4];
            // the first three sprites match one line earlier
            let base0 = self.frogger_swap(b[0]);
            let mut sy = (240u8).wrapping_sub(base0.wrapping_sub((n < 3) as u8)) as i32;
            let mut sx = b[3].wrapping_add(1) as i32;
            let mut flipx = b[1] & 0x40 != 0;
            let mut flipy = b[1] & 0x80 != 0;
            if self.flip_x { sx = (240u8.wrapping_sub(sx as u8)) as i32; flipx = !flipx; }
            if self.flip_y { sy = (240u8.wrapping_sub(sy as u8)) as i32; flipy = !flipy; }
            let row = y as i32 - sy;
            if !(0..16).contains(&row) { continue; }
            let code = self.sprite_code((b[1] & 0x3F) as u16);
            let color = self.frogger_color(b[2] & 7) as usize;
            let gy = if flipy { 15 - row } else { row } as usize;
            for px in 0..16i32 {
                let x = sx + px;
                // the first 17 pixels are clipped by the line buffer
                let visible = if self.flip_x { (0..256 - 17).contains(&x) } else { (17..256).contains(&x) };
                if !visible { continue; }
                let gx = if flipx { 15 - px } else { px } as usize;
                let p = self.sprites[code][gy * 16 + gx];
                if p != 0 { line[x as usize] = self.palette[color * 4 + p as usize]; }
            }
        }

        // 4. bullets: one shell and one missile per line at most, last match wins
        //    (Frogger has no bullet hardware hooked up)
        let (mut shell, mut missile) = (None, None);
        let has_bullets = self.extension != Extension::Frogger;
        let eff = |yy: usize| -> u8 { let v = yy as u8; if self.flip_y { v ^ 0xFF } else { v } };
        for which in (0..8).filter(|_| has_bullets) {
            let effy = if which < 3 { eff(y - 1) } else { eff(y) };
            if self.obj_ram[0x60 + which * 4 + 1].wrapping_add(effy) == 0xFF {
                if which == 7 { missile = Some(which) } else { shell = Some(which) }
            }
        }
        for (b, color) in [(shell, [0xFF, 0xFF, 0xFF]), (missile, [0xFF, 0xFF, 0x00])] {
            if let Some(n) = b {
                let x = 255 - self.obj_ram[0x60 + n * 4 + 3] as i32 - 4;
                for i in 0..4 {
                    if (0..256).contains(&(x + i)) { line[(x + i) as usize] = color; }
                }
            }
        }

        let row = (y - FIRST_LINE) * SCREEN_W * 4;
        for (x, rgb) in line.iter().enumerate() {
            self.framebuffer[row + x * 4..row + x * 4 + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 0xFF]);
        }
    }
}

/// MAME stars_init: on when the top 8 bits are 1 and bit 0 is 0; color from the 6 bits below.
fn build_stars() -> Vec<u8> {
    let mut out = vec![0u8; STAR_PERIOD as usize];
    let mut sr: u32 = 0;
    for v in out.iter_mut() {
        let enabled = (sr & 0x1FE01) == 0x1FE00;
        let color = ((!sr & 0x1F8) >> 3) as u8;
        *v = color | ((enabled as u8) << 7);
        sr = (sr >> 1) | ((((sr >> 12) ^ !sr) & 1) << 16);
    }
    out
}

fn build_star_colors() -> [[u8; 3]; 64] {
    let minval = (RGB_MAXIMUM * 130.0 / 150.0) as i32;
    let midval = (RGB_MAXIMUM * 130.0 / 100.0) as i32;
    let maxval = (RGB_MAXIMUM * 130.0 / 60.0) as i32;
    let map = [0, minval, minval + (255 - minval) * (midval - minval) / (maxval - minval), 255];
    let mut out = [[0u8; 3]; 64];
    for (i, c) in out.iter_mut().enumerate() {
        let lvl = |lsb: usize, msb: usize| map[(((i >> msb) & 1) << 1) | ((i >> lsb) & 1)] as u8;
        *c = [lvl(5, 4), lvl(3, 2), lvl(1, 0)];
    }
    out
}

/// MAME galaxian_palette: compute_resistor_weights(0, 224, auto, ..., pulldown 470).
fn build_palette(prom: &[u8]) -> [[u8; 3]; 32] {
    let rg = [1000.0, 470.0, 220.0];
    let b = [470.0, 220.0];
    let wrg = weights(&rg, 470.0);
    let wb = weights(&b, 470.0);
    // one scale for all nets, from the net with the largest total
    let scale = RGB_MAXIMUM / wrg.iter().sum::<f64>().max(wb.iter().sum::<f64>());
    let combine = |w: &[f64], bits: u8| -> u8 {
        let s: f64 = w.iter().enumerate().filter(|(i, _)| bits >> i & 1 != 0).map(|(_, v)| v * scale).sum();
        (s + 0.5) as u8
    };
    let mut pal = [[0u8; 3]; 32];
    for (i, p) in prom.iter().take(32).enumerate() {
        pal[i] = [combine(&wrg, p & 7), combine(&wrg, (p >> 3) & 7), combine(&wb, p >> 6)];
    }
    pal
}

/// Output of each resistor alone driven high, the others and the pulldown to ground.
fn weights(r: &[f64], pulldown: f64) -> Vec<f64> {
    (0..r.len()).map(|n| {
        let mut g0 = 1.0 / pulldown;
        let mut g1 = 1e-12;
        for (j, &rj) in r.iter().enumerate() {
            if j == n { g1 += 1.0 / rj } else { g0 += 1.0 / rj }
        }
        let (r0, r1) = (1.0 / g0, 1.0 / g1);
        (RGB_MAXIMUM * r0 / (r1 + r0)).clamp(0.0, RGB_MAXIMUM)
    }).collect()
}

#[cfg(test)]
pub(crate) fn palette_of(v: &GalaxianVideo) -> [[u8; 3]; 32] { v.palette }
#[cfg(test)]
pub(crate) fn star_count() -> usize { build_stars().iter().filter(|&&s| s & 0x80 != 0).count() }
