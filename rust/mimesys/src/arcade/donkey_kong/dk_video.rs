pub const SCREEN_W: usize = 256;
pub const SCREEN_H: usize = 224;
pub const FIRST_VISIBLE_LINE: u16 = 16;
pub const VBLANK_START: u16 = 240;


pub struct DKVideo {
    pub video_ram: Vec<u8>,
    pub sprite_ram: Vec<u8>,
    pub flip: bool,
    pub sprite_bank: u8,
    pub palette_bank: u8,

    tiles: Vec<[u8; 64]>,
    sprites: Vec<[u8; 256]>,
    palette: [[u8; 4]; 256],
    color_codes: Vec<u8>,

    pub framebuffer: Vec<u8>,
}

impl DKVideo {
    pub fn new(gfx1: &[u8], gfx2: &[u8], proms: &[u8]) -> Result<Self, String> {
        if gfx1.len() < 0x1000 { return Err("gfx1 region too small".into()); }
        if gfx2.len() < 0x2000 { return Err("gfx2 region too small".into()); }
        if proms.len() < 0x300 { return Err("proms region too small".into()); }

        Ok(Self {
            video_ram: vec![0; 0x400],
            sprite_ram: vec![0; 0x400],
            flip: false,
            sprite_bank: 0,
            palette_bank: 0,
            tiles: (0..256).map(|t| decode_tile(gfx1, t)).collect(),
            sprites: (0..128).map(|s| decode_sprite(gfx2, s)).collect(),
            palette: build_palette(proms),
            color_codes: proms[0x200..0x300].to_vec(),
            framebuffer: vec![0; SCREEN_W * SCREEN_H * 4],
        })
    }

    pub fn render_line(&mut self, line: u16) {
        let mut pens = [0u8; SCREEN_W];
        let bank = self.palette_bank as usize * 16;

        let ty = if self.flip { 255 - line as usize } else { line as usize };
        for x in 0..SCREEN_W {
            let tx = if self.flip { 255 - x } else { x };
            let (col, row) = (tx / 8, ty / 8);
            let code = self.video_ram[row * 32 + col] as usize;
            let group = (self.color_codes[col + 32 * (row / 4)] & 0x0F) as usize + bank;
            let p = self.tiles[code][(ty % 8) * 8 + (tx % 8)] as usize;
            pens[x] = (group * 4 + p) as u8;
        }

        let mut vf = (line as i32 - 1) & 0xFF;
        if self.flip { vf ^= 0xFF; }
        let add_y = if self.flip { 0xF7 } else { 0xF9 };
        let base = (self.sprite_bank as usize) << 9;
        let mut shown = 0;
        let mut offs = base;
        while shown < 16 && offs < base + 0x200 {
            let s = &self.sprite_ram[offs..offs + 4];
            let v = (s[0] as i32 + add_y + 1 + vf) & 0xFF;
            if v & 0xF0 == 0xF0 {
                let row = (v & 0x0F) as usize;
                let code = (s[1] & 0x7F) as usize;
                let flipy = s[1] & 0x80 != 0;
                let group = (s[2] & 0x0F) as usize + bank;
                let mut flipx = s[2] & 0x80 != 0;
                let mut x = (s[3] as i32 + 0xF7 + 1) & 0xFF;
                if self.flip { x = (x ^ 0xFF) - 15; flipx = !flipx; }

                let src_row = if flipy { 15 - row } else { row };
                for i in 0..16 {
                    let src_col = if flipx { 15 - i } else { i };
                    let p = self.sprites[code][src_row * 16 + src_col] as usize;
                    if p != 0 {
                        let px = ((x + i as i32) & 0xFF) as usize;
                        pens[px] = (group * 4 + p) as u8; 
                    } 
                }
                shown += 1;
            }
            offs += 4;
        }

        let out_row = (line - FIRST_VISIBLE_LINE) as usize * SCREEN_W * 4;
        for (x, &pen) in pens.iter().enumerate() {
            let o = out_row + x * 4;
            self.framebuffer[o..o + 4].copy_from_slice(&self.palette[pen as usize]);
        }
    }
}

fn decode_tile(gfx1: &[u8], tile: usize) -> [u8; 64] {
    let mut px = [0u8; 64];
    for y in 0..8 {
        let lo = gfx1[tile * 8 + y];
        let hi = gfx1[0x800 + tile * 8 + y];
        for x in 0..8 {
            let bit = 7 - x;
            px[y * 8 + x] = (((hi >> bit) & 1) << 1) | ((lo >> bit) & 1);
        }
    }
    px
}

    fn decode_sprite(gfx2: &[u8], code: usize) -> [u8; 256] {
        let mut px = [0u8; 256];
        for y in 0..16 {
            for x in 0..16 {
                let off = if x < 8 { 0 } else { 0x800 } + code * 16 + y;
                let bit = 7 - (x & 7);
                let lo = (gfx2[off] >> bit) & 1;
                let hi = (gfx2[0x1000 + off] >> bit) & 1;
                px[y * 16 + x] = (hi << 1) | lo;
            }
        }
        px
    }

    const RG_LEVELS: [u8; 8] = [225,197,167,139,102,71,36,0];
    const B_LEVELS: [u8; 4] = [255,218,138,0];

    fn build_palette(proms: &[u8]) -> [[u8; 4]; 256] {
        let mut pal = [[0,0,0,255]; 256];
        for i in 0..256 {
            if i & 3 == 0 { continue; }
            let k = proms[i];
            let j = proms[0x100 + i];
            let r = (j >> 1) & 0x07;
            let g = ((j << 2) & 0x04) | ((k >> 2) & 0x03);
            let b = k & 0x03;
            pal[i] = [
                RG_LEVELS[r as usize],
                RG_LEVELS[g as usize],
                B_LEVELS[b as usize],
                255,
            ];
        }
        normalize_range(&mut pal);
        pal
    }

    fn normalize_range(pal: &mut [[u8; 4]; 256]) {
        let lum = |c: & [u8; 4]| 299 * c[0] as i32 + 587 * c[1] as i32 + 114 * c[2] as i32;
        let ymin = pal.iter().map(lum).min().unwrap();
        let ymax = pal.iter().map(lum).max().unwrap();
        if ymax == ymin { return; }
        let (tmin, tmax) = (0, 255);
        let clamp = |v: i32| v.clamp(0, 255) as u8;
        for c in pal.iter_mut() {
            let y = lum(c);
            let u = (c[2] as i32 - y / 1000) * 492 / 1000;
            let v = (c[0] as i32 - y / 1000) * 877 / 1000;
            let target = tmin + ((y - ymin) * (tmax - tmin + 1)) / (ymax - ymin);
            *c = [
                clamp(target + 1140 * v / 1000),
                clamp(target - 395 * u / 1000 - 581 * v / 1000),
                clamp(target + 2032 * u / 1000),
                255,
            ];
        }
    }
