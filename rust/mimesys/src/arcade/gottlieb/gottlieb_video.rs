// Gottlieb (GG-III) video: one 32x32 tilemap of 8x8 4bpp tiles plus 64 16x16 4bpp sprites,
// 16 colors from palette RAM. MAME src/mame/gottlieb/gottlieb.cpp.
//
// Screen: 20 MHz / 4 pixel clock, 318 x 256 total, 256 x 240 visible, ~61.42 Hz.
// Tiles: videoram byte = tile code; Q*bert's tiles are in ROM ("bgtiles", 256 tiles, packed
//   4bpp, high nibble first). Other boards take codes < 0x80 from char RAM.
// Sprites: 4 bytes each: Y, X, code (inverted), unused. sx = X - 4, sy = Y - 13; 4 planes,
//   each a quarter of the sprite region (first quarter = most significant bit); not
//   drawn left of x = 8. Pen 0 is transparent for both layers.
// Control (general output bits 0-2): background priority (1 = tiles over sprites),
//   flip X, flip Y.
// Palette: even byte = green (high nibble) / blue (low), odd byte = red (low nibble), each
//   through a 2k/1k/470/240 ohm ladder.

pub const SCREEN_W: usize = 256;
pub const SCREEN_H: usize = 240;

pub struct GottliebVideo {
    pub videoram: [u8; 0x400],
    pub charram: [u8; 0x1000],
    pub spriteram: [u8; 0x100],
    pub paletteram: [u8; 0x20],
    palette: [[u8; 3]; 16],
    weights: [f64; 4],
    bgtiles: Vec<u8>,
    sprites: Vec<u8>,
    /// tile codes >= 0x80 / < 0x80 come from ROM (true) or char RAM
    rom_tiles_lo: bool,
    rom_tiles_hi: bool,
    pub bg_priority: bool,
    pub flip_x: bool,
    pub flip_y: bool,
    pub framebuffer: Vec<u8>,
    line: Vec<u8>,          // pen per pixel for the frame being drawn
}

/// MAME compute_resistor_weights for one 4-resistor channel with a 180 ohm pulldown,
/// scaled so all four bits on = 255.
fn resistor_weights() -> [f64; 4] {
    const R: [f64; 4] = [2000.0, 1000.0, 470.0, 240.0];
    const PULLDOWN: f64 = 180.0;
    let mut v = [0.0; 4];
    for i in 0..4 {
        let mut g = 1.0 / PULLDOWN;
        for (j, r) in R.iter().enumerate() {
            if j != i { g += 1.0 / r; }
        }
        let r0 = 1.0 / g;
        v[i] = r0 / (r0 + R[i]);
    }
    let max: f64 = v.iter().sum();
    let mut w = [0.0; 4];
    for i in 0..4 { w[i] = v[i] * 255.0 / max; }
    w
}

impl GottliebVideo {
    pub fn new(bgtiles: Vec<u8>, sprites: Vec<u8>, rom_tiles: bool) -> Self {
        Self {
            videoram: [0; 0x400], charram: [0; 0x1000], spriteram: [0; 0x100], paletteram: [0; 0x20],
            palette: [[0; 3]; 16], weights: resistor_weights(),
            bgtiles, sprites,
            rom_tiles_lo: rom_tiles, rom_tiles_hi: rom_tiles,
            bg_priority: false, flip_x: false, flip_y: false,
            framebuffer: vec![0; SCREEN_W * SCREEN_H * 4],
            line: vec![0; SCREEN_W * SCREEN_H],
        }
    }

    fn combine(&self, bits: u8) -> u8 {
        let mut s = 0.0;
        for i in 0..4 { if bits >> i & 1 != 0 { s += self.weights[i]; } }
        (s + 0.5).min(255.0) as u8
    }

    /// After loading a state: rebuild the 16 colors from palette RAM.
    pub fn rebuild_palette(&mut self) {
        for i in 0..0x20 { let v = self.paletteram[i]; self.palette_write(i, v); }
    }

    pub fn palette_write(&mut self, offset: usize, v: u8) {
        let offset = offset & 0x1F;
        self.paletteram[offset] = v;
        let gb = self.paletteram[offset & !1];
        let r = self.paletteram[offset | 1];
        self.palette[offset / 2] = [self.combine(r & 0x0F), self.combine(gb >> 4), self.combine(gb & 0x0F)];
    }

    /// Bits 0-2 of the general output latch.
    pub fn control_write(&mut self, v: u8) {
        self.bg_priority = v & 1 != 0;
        self.flip_x = v & 2 != 0;
        self.flip_y = v & 4 != 0;
    }

    #[inline]
    fn tile_pixel(&self, code: u8, x: usize, y: usize) -> u8 {
        let from_rom = if code & 0x80 != 0 { self.rom_tiles_hi } else { self.rom_tiles_lo };
        let (data, idx): (&[u8], usize) = if from_rom {
            (&self.bgtiles, code as usize)
        } else {
            (&self.charram, (code & 0x7F) as usize)
        };
        let b = data.get(idx * 32 + y * 4 + x / 2).copied().unwrap_or(0);
        if x & 1 == 0 { b >> 4 } else { b & 0x0F }
    }

    #[inline]
    fn sprite_pixel(&self, code: u8, x: usize, y: usize) -> u8 {
        let quarter = self.sprites.len() / 4;
        let bit = code as usize * 256 + y * 16 + x;
        let mut v = 0;
        for plane in 0..4 {
            let b = self.sprites.get(plane * quarter + (bit >> 3)).copied().unwrap_or(0);
            v = (v << 1) | ((b >> (7 - (bit & 7))) & 1);
        }
        v
    }

    fn draw_bg(&mut self, transparent: bool) {
        for sy in 0..SCREEN_H {
            let ty = if self.flip_y { 255 - sy } else { sy };
            for sx in 0..SCREEN_W {
                let tx = if self.flip_x { 255 - sx } else { sx };
                let code = self.videoram[(ty / 8) * 32 + tx / 8];
                let p = self.tile_pixel(code, tx & 7, ty & 7);
                if !transparent || p != 0 {
                    self.line[sy * SCREEN_W + sx] = p;
                }
            }
        }
    }

    fn draw_sprites(&mut self) {
        for offs in (0..256).step_by(4) {
            let mut sx = self.spriteram[offs + 1] as i32 - 4;
            let mut sy = self.spriteram[offs] as i32 - 13;
            let code = 255 ^ self.spriteram[offs + 2];
            if self.flip_x { sx = 233 - sx; }
            if self.flip_y { sy = 228 - sy; }
            for py in 0..16 {
                let y = sy + py as i32;
                if !(0..SCREEN_H as i32).contains(&y) { continue; }
                let srcy = if self.flip_y { 15 - py } else { py };
                for px in 0..16 {
                    let x = sx + px as i32;
                    if !(8..SCREEN_W as i32).contains(&x) { continue; }
                    let srcx = if self.flip_x { 15 - px } else { px };
                    let p = self.sprite_pixel(code, srcx, srcy);
                    if p != 0 {
                        self.line[y as usize * SCREEN_W + x as usize] = p;
                    }
                }
            }
        }
    }

    pub fn render_frame(&mut self) {
        if !self.bg_priority {
            self.draw_bg(false);
        } else {
            self.line.fill(0);
        }
        self.draw_sprites();
        if self.bg_priority {
            self.draw_bg(true);
        }
        for (i, &p) in self.line.iter().enumerate() {
            let c = self.palette[p as usize];
            self.framebuffer[i * 4..i * 4 + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
        }
    }

    #[cfg(test)]
    pub(crate) fn palette_of(&self) -> [[u8; 3]; 16] { self.palette }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_scale_palette() {
        let mut v = GottliebVideo::new(vec![0; 0x2000], vec![0; 0x8000], true);
        v.palette_write(0, 0xFF);
        v.palette_write(1, 0x0F);
        assert_eq!(v.palette_of()[0], [255, 255, 255]);
        v.palette_write(2, 0x08);   // blue MSB only
        assert!(v.palette_of()[1][2] > 120 && v.palette_of()[1][2] < 160, "{:?}", v.palette_of()[1]);
    }
}
