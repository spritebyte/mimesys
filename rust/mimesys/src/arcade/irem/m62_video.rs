// Irem M62 video, Kung-Fu Master layout (MAME src/mame/irem/m62_v.cpp).
//
// Screen: 6.144 MHz pixel clock, 384 x 284 total; visible x 128-383, y 0-255 (256 x 256).
// Background: 64 x 32 tilemap of 8x8 3bpp tiles (gfx1: three bitplanes, one per third,
//   the last third is the most significant). Tile RAM D000-D7FF = code low bits,
//   D800-DFFF = attributes: bits 0-4 color, bit 5 flip X, bits 6-7 code bits 8-9.
//   Tile rows 0-5 (the status bar) never scroll; rows 6-31 use the 16-bit scroll register.
//   "Category 1" tiles are drawn over the sprites: rows 0-5 and colors with
//   (color >> 1) > 0x0C.
// Sprites: 32 x 8 bytes. 0: color (bits 0-4), 2/3: Y (9 bits), 4/5: code (11 bits),
//   5: bit 6 flip X, bit 7 flip Y, 6/7: X (9 bits). A PROM gives each block of 32 codes a
//   height: 0 = 16, 1 = 32, 2 = 64 pixels (consecutive codes stacked). 3bpp, pen 0 clear.
// Palettes: two sets of three 256x4 PROMs (tiles, sprites), 2.2k/1k/470/220 ohm per gun
//   with a 470 ohm pulldown; MAME then raises the contrast so the brightest color is full.

pub const SCREEN_W: usize = 256;
pub const SCREEN_H: usize = 256;
const VIS_X0: i32 = 128;

pub struct M62Video {
    pub tileram: [u8; 0x1000],
    pub spriteram: [u8; 0x100],
    pub hscroll: u16,
    pub flip: bool,
    tiles: Vec<u8>,          // decoded: 1024 tiles x 64 pixels
    sprites: Vec<u8>,        // decoded: n sprites x 256 pixels
    sprite_count: usize,
    height_prom: Vec<u8>,
    chr_pal: [[u8; 3]; 256],
    spr_pal: [[u8; 3]; 256],
    pub framebuffer: Vec<u8>,
    pens: Vec<[u8; 3]>,
}

/// Voltage-divider weight of each gun bit (TTL high on set bits, low on clear ones).
fn gun_levels() -> [f64; 16] {
    const R: [f64; 4] = [2200.0, 1000.0, 470.0, 220.0];
    const PULLDOWN: f64 = 470.0;
    let total: f64 = R.iter().map(|r| 1.0 / r).sum::<f64>() + 1.0 / PULLDOWN;
    let mut out = [0.0; 16];
    for (v, o) in out.iter_mut().enumerate() {
        let on: f64 = (0..4).filter(|b| v >> b & 1 != 0).map(|b| 1.0 / R[b]).sum();
        *o = on / total;
    }
    out
}

fn decode_planar(src: &[u8], count: usize, w: usize, h: usize, bytes_per: usize,
                 xoff: &dyn Fn(usize) -> usize) -> Vec<u8> {
    let third = src.len() / 3;
    let mut out = vec![0u8; count * w * h];
    for n in 0..count {
        for y in 0..h {
            for x in 0..w {
                let bit = xoff(x);
                let byte = n * bytes_per + y + bit / 8;
                let mask = 0x80 >> (bit % 8);
                let mut p = 0;
                // planes: RGN_FRAC(2,3) first = most significant
                for plane in [2, 1, 0] {
                    let b = src.get(plane * third + byte).copied().unwrap_or(0);
                    p = (p << 1) | ((b & mask != 0) as u8);
                }
                out[(n * h + y) * w + x] = p;
            }
        }
    }
    out
}

impl M62Video {
    pub fn new(gfx1: &[u8], gfx2: &[u8], height_prom: Vec<u8>, chr_proms: &[u8], spr_proms: &[u8]) -> Self {
        // tiles: 8x8, 8 bytes each, one byte per row
        let tile_count = gfx1.len() / 3 / 8;
        let tiles = decode_planar(gfx1, tile_count, 8, 8, 8, &|x| x);
        // sprites: 16x16, 32 bytes; left half bytes 0-15 (one per row), right half 16-31
        let sprite_count = gfx2.len() / 3 / 32;
        let sprites = decode_planar(gfx2, sprite_count, 16, 16, 32, &|x| if x < 8 { x } else { 16 * 8 + (x - 8) });

        let lv = gun_levels();
        let mk = |proms: &[u8]| -> Vec<[f64; 3]> {
            (0..256).map(|i| {
                let g = |o: usize| lv[(proms.get(o + i).copied().unwrap_or(0) & 0x0F) as usize];
                [g(0), g(0x100), g(0x200)]
            }).collect()
        };
        let chr = mk(chr_proms);
        let spr = mk(spr_proms);
        // MAME m62_amplify_contrast: scale so the brightest entry has full luma
        let luma = |c: &[f64; 3]| 0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2];
        let ymax = chr.iter().chain(spr.iter()).map(luma).fold(1e-9, f64::max);
        let to8 = |c: &[f64; 3]| c.map(|v| (v / ymax * 255.0).round().clamp(0.0, 255.0) as u8);
        let mut chr_pal = [[0u8; 3]; 256];
        let mut spr_pal = [[0u8; 3]; 256];
        for i in 0..256 { chr_pal[i] = to8(&chr[i]); spr_pal[i] = to8(&spr[i]); }

        Self {
            tileram: [0; 0x1000], spriteram: [0; 0x100], hscroll: 0, flip: false,
            tiles, sprites, sprite_count: sprite_count.max(1), height_prom,
            chr_pal, spr_pal,
            framebuffer: vec![0; SCREEN_W * SCREEN_H * 4],
            pens: vec![[0; 3]; SCREEN_W * SCREEN_H],
        }
    }

    fn draw_bg(&mut self, category: u8) {
        for y in 0..SCREEN_H {
            let row = y / 8;
            let scroll = if row < 6 { 0 } else { self.hscroll as i32 };
            for sx in 0..SCREEN_W {
                let tx = ((sx as i32 + VIS_X0 + scroll) & 511) as usize;
                let idx = row * 64 + tx / 8;
                let attr = self.tileram[idx + 0x800];
                let color = (attr & 0x1F) as usize;
                let cat = if row < 6 || (color >> 1) > 0x0C { 1 } else { 0 };
                if cat != category { continue; }
                let code = self.tileram[idx] as usize | (((attr & 0xC0) as usize) << 2);
                let mut px = tx & 7;
                if attr & 0x20 != 0 { px = 7 - px; }
                let code = code % (self.tiles.len() / 64).max(1);
                let p = self.tiles[code * 64 + (y & 7) * 8 + px] as usize;
                self.pens[y * SCREEN_W + sx] = self.chr_pal[color * 8 + p];
            }
        }
    }

    fn draw_sprites(&mut self) {
        for offs in (0..0x100).step_by(8) {
            let r = &self.spriteram[offs..offs + 8];
            let mut code = r[4] as usize + (((r[5] & 7) as usize) << 8);
            let col = (r[0] & 0x1F) as usize;
            let sx = 256 * (r[7] & 1) as i32 + r[6] as i32;
            let mut sy = 256 + 128 - 15 - (256 * (r[3] & 1) as i32 + r[2] as i32);
            let flipx = r[5] & 0x40 != 0;
            let flipy = r[5] & 0x80 != 0;
            let mut i = self.height_prom.get((code >> 5) & 0x1F).copied().unwrap_or(0) as i32;
            if i == 1 {
                code &= !1;
                sy -= 16;
            } else if i == 2 {
                i = 3;
                code &= !3;
                sy -= 3 * 16;
            } else {
                i = 0;
            }
            let incr: i32 = if flipy { code += i as usize; -1 } else { 1 };
            while i >= 0 {
                let c = ((code as i32 + i * incr) as usize) % self.sprite_count;
                self.draw_one(c, col, flipx, flipy, sx - VIS_X0, sy + 16 * i);
                i -= 1;
            }
        }
    }

    fn draw_one(&mut self, code: usize, col: usize, flipx: bool, flipy: bool, sx: i32, sy: i32) {
        for py in 0..16 {
            let y = sy + py;
            if !(0..SCREEN_H as i32).contains(&y) { continue; }
            let srcy = if flipy { 15 - py } else { py } as usize;
            for px in 0..16 {
                let x = sx + px;
                if !(0..SCREEN_W as i32).contains(&x) { continue; }
                let srcx = if flipx { 15 - px } else { px } as usize;
                let p = self.sprites[(code * 16 + srcy) * 16 + srcx] as usize;
                if p != 0 {
                    self.pens[y as usize * SCREEN_W + x as usize] = self.spr_pal[col * 8 + p];
                }
            }
        }
    }

    pub fn render_frame(&mut self) {
        self.draw_bg(0);
        self.draw_sprites();
        self.draw_bg(1);
        let n = SCREEN_W * SCREEN_H;
        for i in 0..n {
            // flip screen (cocktail): the whole picture turned 180 degrees
            let src = if self.flip { n - 1 - i } else { i };
            let c = self.pens[src];
            self.framebuffer[i * 4..i * 4 + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
        }
    }
}
