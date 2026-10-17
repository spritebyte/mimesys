pub const SCREEN_W: usize = 288;
pub const SCREEN_H: usize = 224;
pub const COLS: usize = 36;
pub const ROWS: usize = 28;


pub struct PacmanVideo {
    pub video_ram: Vec<u8>,
    pub color_ram: Vec<u8>,
    pub sprite_ram: [u8; 16],
    pub sprite_pos: [u8; 16],
    pub flip: bool,

    // Pengo and Jr. Pacman banks. Always 0 for Pacman and Ms Pacman.
    pub char_bank: u8,
    pub sprite_bank: u8,
    pub palette_bank: u8,
    pub colortable_bank: u8,
    pub sprite_offset_hack: bool,

    tiles: Vec<[u8; 64]>,
    sprites: Vec<[u8; 256]>,
    palette: [[u8; 3]; 32],
    lookup: Vec<u8>,

    pub framebuffer: Vec<u8>,
}

impl PacmanVideo {
    pub fn new(gfx1: &[u8], proms: &[u8]) -> Result<Self, String> {
        if gfx1.len() < 0x2000 || gfx1.len() % 0x2000 != 0 { return Err(format!("gfx1 is 0x{:X} bytes. Expected a multiple of 0x2000", gfx1.len())); }
        if proms.len() < 0x120 { return Err(format!("proms is 0x{:X} bytes, expected at least 0x120", proms.len())); }

        let half = gfx1.len() / 2;
        let tiles = gfx1[..half].chunks_exact(16).map(decode_tile).collect();
        let sprites = gfx1[half..].chunks_exact(64).map(decode_sprite).collect();
        Ok(Self {
            video_ram: vec![0; 0x400],
            color_ram: vec![0; 0x400],
            sprite_ram: [0; 16],
            sprite_pos: [0; 16],
            flip: false,
            char_bank: 0,
            sprite_bank: 0,
            palette_bank: 0,
            colortable_bank: 0,
            sprite_offset_hack: true,
            tiles,
            sprites,
            palette: build_palette(&proms[..0x20]),
            lookup: proms[0x20..].iter().map(|b| b & 0x0F).collect(),
            framebuffer: vec![0; SCREEN_W * SCREEN_H * 4],
        })
    }

    fn lut(&self, color: usize, pixel: u8) -> u8 {
        self.lookup[((color * 4 + pixel as usize) & 0xFF) % self.lookup.len()]
    }

    fn pen(&self, color: usize, pixel: u8) -> u8 {
        self.lut(color, pixel) | ((self.palette_bank & 1) << 4)
    }

    fn put(&mut self, x: usize, y: usize, pen: u8) {
        let rgb = self.palette[(pen & 0x1F) as usize];
        let o = (y * SCREEN_W + x) * 4;
        self.framebuffer[o..o + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 0xFF]);
    }

    pub fn render_frame(&mut self) {
        let bank_bits = ((self.colortable_bank as usize) << 5) | ((self.palette_bank as usize) << 6);

        // background: 36x28 opaque tiles
        for row in 0..ROWS {
            for col in 0..COLS {
                let offs = tile_offset(col, row);
                let code = self.video_ram[offs] as usize | ((self.char_bank as usize) << 8);
                let color = (self.color_ram[offs] & 0x1F) as usize | bank_bits;
                let tile = self.tiles[code % self.tiles.len()];
                for ty in 0..8 {
                    for tx in 0..8 {
                        let pen = self.pen(color, tile[ty * 8 + tx]);
                        let (mut x, mut y) = (col * 8 + tx, row * 8 + ty);
                        if self.flip { x = SCREEN_W - 1 - x; y = SCREEN_H - 1 - y; }
                        self.put(x, y, pen);
                    }
                }
            }
        }

        // Sprites: drawn 7 down to 0 so sprite 0 ends up on top
        for s in (0..8).rev() {
            let attr = self.sprite_ram[s * 2];
            let color = (self.sprite_ram[s * 2 + 1] & 0x1F) as usize | bank_bits;
            let code = (attr >> 2) as usize | ((self.sprite_bank as usize) << 6);
            let (flipx, flipy) = (attr & 1 != 0, attr & 2 != 0);

            let sx = 272 - self.sprite_pos[s * 2 + 1] as i32;
            let mut sy = self.sprite_pos[s * 2] as i32 - 31;
            if self.sprite_offset_hack && s <= 2 { sy += 1; }

            self.draw_sprite(code, color, flipx, flipy, sx, sy);
            self.draw_sprite(code, color, flipx, flipy, sx - 256, sy);  // wraparound
        }
    }

    fn draw_sprite(&mut self, code: usize, color: usize, flipx: bool, flipy: bool, sx: i32, sy: i32) {
        let gfx = self.sprites[code % self.sprites.len()];
        for py in 0..16 {
            for px in 0..16 {
                let (x, y) = (sx + px, sy + py);
                if x < 16 || x >= 272 || y < 0 || y >= SCREEN_H as i32 { continue; }
                let gx = if flipx { 15 - px } else { px } as usize;
                let gy = if flipy { 15 - py } else { py } as usize;
                let pixel = gfx[gy * 16 + gx];

                if self.lut(color, pixel) == 0 { continue; }
//                let lut = self.lookup[(color * 4 + pixel as usize) % self.lookup.len()];
//                if lut == 0 { continue; }
                let pen = self.pen(color, pixel);
                self.put(x as usize, y as usize, pen);
            }
        }
    }
}

pub fn tile_offset(col: usize, row: usize) -> usize {
    let row = row as u32 + 2;
    let col = (col as u32).wrapping_sub(2);
    (if col & 0x20 != 0 { row + ((col & 0x1F) << 5) } else { col + (row << 5) }) as usize
}

fn bit(data: &[u8], bitpos: usize) -> u8 {
    (data[bitpos / 8] >> (7 - bitpos % 8)) & 1
}

fn pixel(data: &[u8], offset: usize) -> u8 {
    (bit(data, offset) << 1) | bit(data, offset + 4)
}

const TILE_X: [usize; 8] = [64, 65, 66, 67, 0, 1, 2, 3];
fn decode_tile(data: &[u8]) -> [u8; 64] {
    let mut out = [0u8; 64];
    for y in 0..8 {
        for x in 0..8 {
            out[y * 8 + x] = pixel(data, TILE_X[x] + y * 8);
        }
    }
    out
}

const SPR_X: [usize; 16] = [64, 65, 66, 67, 128, 129, 130, 131, 192, 193, 194, 195, 0, 1, 2, 3];
const SPR_Y: [usize; 16] = [0, 8, 16, 24, 32, 40, 48, 56, 256, 264, 272, 280, 288, 296, 304, 312];
fn decode_sprite(data: &[u8]) -> [u8; 256] {
    let mut out = [0u8; 256];
    for y in 0..16 {
        for x in 0..16 {
            out[y * 16 + x] = pixel(data, SPR_X[x] + SPR_Y[y]);
        }
    }
    out
}

fn build_palette(prom: &[u8]) -> [[u8; 3]; 32] {
    fn weights(ohms: &[f64]) -> Vec<f64> {
        let total: f64 = ohms.iter().map(|r| 1.0 / r).sum();
        ohms.iter().map(|r| 255.0 * (1.0 / r) / total).collect()
    }

    let rg = weights(&[1000.0, 470.0, 220.0]);
    let b = weights(&[470.0, 220.0]);
    let level = |w: & [f64], bits: u8| -> u8 {
        let v: f64 = w.iter().enumerate().filter(|(i, _)| bits >> i & 1 != 0).map(|(_, w)| w).sum();
        (v + 0.5) as u8
    };
    let mut pal = [[0u8; 3]; 32];
    for (i, p) in prom.iter().take(32).enumerate() {
        pal[i] = [level(&rg, p & 7), level(&rg, (p >> 3) & 7), level(&b, p >> 6)];
    }
    pal
}