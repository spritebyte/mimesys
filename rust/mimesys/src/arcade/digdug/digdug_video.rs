// Dig Dug video (MAME src/mame/namco/digdug.cpp).
//
// Screen 288 x 224 (36 x 28 tiles), shown rotated (ROT90).
// Background: tile codes come from the playfield ROM (gfx4), 4 pictures of 1 KB picked by
//   the video latch; color = code >> 4 (or 0xF when "disabled") + a color bank (latch bits
//   4-5). 2bpp tiles (gfx3).
// Text layer: video RAM 8000-83FF, 1bpp tiles (gfx1), pen 0 transparent; color from the
//   code's top bits (or its low nibble in the unused mode 1).
// Sprites: 64 entries spread over three RAMs (8B80/9380/9B80): code + color, Y + X,
//   flip bits. 16x16 2bpp (gfx2), bit 7 of the code = 32x32 (4 tiles). Lookup color 0x1F
//   is transparent. Columns 0-1 and 34-35 are blanked for sprites.
// Palette: 32 colors from a PROM (3/3/2 bits through 1k/470/220 ohm), lookup PROMs for
//   sprites (colors 16-31) and background (colors 0-15).
// Tile memory is laid out 32 x 32 with the two side columns stored elsewhere (tilemap_scan).

pub const SCREEN_W: usize = 288;
pub const SCREEN_H: usize = 224;

pub struct DigDugVideo {
    fg_tiles: Vec<[u8; 64]>,
    bg_tiles: Vec<[u8; 64]>,
    sprites: Vec<[u8; 256]>,
    playfield: Vec<u8>,
    palette: [[u8; 3]; 32],
    spr_lookup: Vec<u8>,
    bg_lookup: Vec<u8>,
    pub framebuffer: Vec<u8>,
    pens: Vec<u8>,       // palette index per pixel
}

/// MAME compute_resistor_weights with no pull resistors: weights proportional to the
/// conductance of each resistor, all bits on = 255.
fn weights(r: &[f64]) -> Vec<f64> {
    let total: f64 = r.iter().map(|x| 1.0 / x).sum();
    r.iter().map(|x| (1.0 / x) / total * 255.0).collect()
}

fn decode(src: &[u8], count: usize, w: usize, h: usize, bytes: usize, bit: &dyn Fn(usize, usize) -> usize, planes: &[usize]) -> Vec<Vec<u8>> {
    (0..count).map(|n| {
        let mut t = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                let off = n * bytes * 8 + bit(x, y);
                let mut p = 0u8;
                for &pl in planes {
                    let b = off + pl;
                    let v = src.get(b / 8).map(|byte| (byte >> (7 - (b % 8))) & 1).unwrap_or(0);
                    p = (p << 1) | v;
                }
                t[y * w + x] = p;
            }
        }
        t
    }).collect()
}

/// 36x28 visible grid -> 32x32 memory index (MAME tilemap_scan)
#[inline]
pub fn tile_index(col: usize, row: usize) -> usize {
    let row = row + 2;
    let col = col.wrapping_sub(2) & 0x3F;
    if col & 0x20 != 0 { row + ((col & 0x1F) << 5) } else { col + (row << 5) }
}

pub struct VideoRegs<'a> {
    pub videoram: &'a [u8],
    pub objram: &'a [u8],
    pub posram: &'a [u8],
    pub flpram: &'a [u8],
    pub bg_select: u8,
    pub bg_color_bank: u8,
    pub tx_color_mode: bool,
    pub bg_disable: bool,
    pub flip: bool,
}

impl DigDugVideo {
    pub fn new(gfx1: &[u8], gfx2: &[u8], gfx3: &[u8], gfx4: &[u8], proms: &[u8]) -> Result<Self, String> {
        if proms.len() < 0x220 { return Err(format!("proms is 0x{:X} bytes, expected 0x220", proms.len())); }
        // gfx1: 1bpp, x = bit 7-x reversed (STEP8(7,-1)), 8 bytes a tile
        let fg = decode(gfx1, gfx1.len() / 8, 8, 8, 8, &|x, y| y * 8 + (7 - x), &[0]);
        // gfx3: 2bpp {0,4}, x0-3 at bit 64.., x4-7 at bit 0.., 16 bytes a tile
        let bg = decode(gfx3, gfx3.len() / 16, 8, 8, 16,
            &|x, y| y * 8 + if x < 4 { 64 + x } else { x - 4 }, &[0, 4]);
        // gfx2: 2bpp {0,4}, 16x16, x in four groups of 4 at bits 0/64/128/192, y rows of 8 bits,
        // rows 8-15 at +256 bits; 64 bytes a sprite
        let spr = decode(gfx2, gfx2.len() / 64, 16, 16, 64,
            &|x, y| (x / 4) * 64 + (x % 4) + (y % 8) * 8 + (y / 8) * 256, &[0, 4]);

        let rw = weights(&[1000.0, 470.0, 220.0]);
        let bw = weights(&[470.0, 220.0]);
        let comb = |w: &[f64], bits: &[u8]| -> u8 {
            let s: f64 = w.iter().zip(bits).map(|(w, &b)| if b != 0 { *w } else { 0.0 }).sum();
            (s + 0.5).min(255.0) as u8
        };
        let mut palette = [[0u8; 3]; 32];
        for (i, c) in palette.iter_mut().enumerate() {
            let p = proms[i];
            let bit = |n: u8| (p >> n) & 1;
            *c = [comb(&rw, &[bit(0), bit(1), bit(2)]), comb(&rw, &[bit(3), bit(4), bit(5)]), comb(&bw, &[bit(6), bit(7)])];
        }
        let to64 = |v: Vec<Vec<u8>>| -> Vec<[u8; 64]> { v.into_iter().map(|t| t.try_into().unwrap()).collect() };
        Ok(Self {
            fg_tiles: to64(fg),
            bg_tiles: to64(bg),
            sprites: spr.into_iter().map(|t| t.try_into().unwrap()).collect(),
            playfield: gfx4.to_vec(),
            palette,
            spr_lookup: proms[0x20..0x120].to_vec(),
            bg_lookup: proms[0x120..0x220].to_vec(),
            framebuffer: vec![0; SCREEN_W * SCREEN_H * 4],
            pens: vec![0; SCREEN_W * SCREEN_H],
        })
    }

    pub fn render(&mut self, r: &VideoRegs) {
        // background (opaque) and text (pen 0 clear)
        for row in 0..28 {
            for col in 0..36 {
                let idx = tile_index(col, row);
                let code = self.playfield.get(idx | ((r.bg_select as usize) << 10)).copied().unwrap_or(0) as usize;
                let color = if r.bg_disable { 0x0F } else { code >> 4 } | r.bg_color_bank as usize;
                let bg = &self.bg_tiles[code % self.bg_tiles.len().max(1)];
                let tx = r.videoram[idx] as usize;
                let tx_color = if r.tx_color_mode { tx & 0x0F } else { ((tx >> 4) & 0x0E) | ((tx >> 3) & 2) };
                let fg = &self.fg_tiles[(tx & 0x7F) % self.fg_tiles.len().max(1)];
                for y in 0..8 {
                    for x in 0..8 {
                        let mut pen = self.bg_lookup[(color * 4 + bg[y * 8 + x] as usize) & 0xFF] & 0x0F;
                        if fg[y * 8 + x] != 0 { pen = tx_color as u8; }
                        self.pens[(row * 8 + y) * SCREEN_W + col * 8 + x] = pen;
                    }
                }
            }
        }
        // sprites
        const GFX_OFFS: [[usize; 2]; 2] = [[0, 1], [2, 3]];
        for offs in (0..0x80).step_by(2) {
            let mut sprite = r.objram[0x380 + offs] as usize;
            let color = (r.objram[0x380 + offs + 1] & 0x3F) as usize;
            let sx = r.posram[0x380 + offs + 1] as i32 - 40 + 1;
            let mut sy = 256 - r.posram[0x380 + offs] as i32 + 1;
            let flipx = r.flpram[0x380 + offs] & 1 != 0;
            let flipy = r.flpram[0x380 + offs] & 2 != 0;
            let size = (sprite & 0x80) >> 7;
            if size != 0 { sprite = (sprite & 0xC0) | ((sprite & 0x3F) << 2); }
            sy -= 16 * size as i32;
            sy = (sy & 0xFF) - 32;
            for y in 0..=size {
                for x in 0..=size {
                    let code = sprite + GFX_OFFS[y ^ (size * flipy as usize)][x ^ (size * flipx as usize)];
                    let px = (sx + 16 * x as i32) & 0xFF;
                    for wrap in [0, 0x100] {
                        self.draw_sprite(code, color, flipx, flipy, px + wrap, sy + 16 * y as i32);
                    }
                }
            }
        }
        let n = SCREEN_W * SCREEN_H;
        for i in 0..n {
            let src = if r.flip { n - 1 - i } else { i };
            let c = self.palette[self.pens[src] as usize & 31];
            self.framebuffer[i * 4..i * 4 + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
        }
    }

    fn draw_sprite(&mut self, code: usize, color: usize, flipx: bool, flipy: bool, sx: i32, sy: i32) {
        let gfx = &self.sprites[code % self.sprites.len().max(1)];
        for py in 0..16 {
            let y = sy + py;
            if !(0..SCREEN_H as i32).contains(&y) { continue; }
            let srcy = if flipy { 15 - py } else { py } as usize;
            for px in 0..16 {
                let x = sx + px;
                if !(16..(SCREEN_W as i32 - 16)).contains(&x) { continue; }
                let srcx = if flipx { 15 - px } else { px } as usize;
                let look = self.spr_lookup[(color * 4 + gfx[srcy * 16 + srcx] as usize) & 0xFF] & 0x0F;
                if look != 0x0F {
                    self.pens[y as usize * SCREEN_W + x as usize] = look | 0x10;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_matches_mame() {
        // visible column 2 row 0 -> memory row 2 col 0
        assert_eq!(tile_index(2, 0), 2 << 5);
        // visible column 0 (side column) -> col & 0x20 path
        assert_eq!(tile_index(0, 0), 2 + (30 << 5));
        assert_eq!(tile_index(35, 27), 29 + (1 << 5));
    }
}
