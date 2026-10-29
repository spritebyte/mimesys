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
    /// DK Jr only: tile code + 256 * gfx_bank (MAME dkongjr_gfxbank_w)
    pub gfx_bank: u8,

    tiles: Vec<[u8; 64]>,
    sprites: Vec<[u8; 256]>,
    palette: [[u8; 4]; 256],
    color_codes: Vec<u8>,

    pub framebuffer: Vec<u8>,
}

impl DKVideo {
    pub fn new(gfx1: &[u8], gfx2: &[u8], proms: &[u8]) -> Result<Self, String> {
        if proms.len() < 0x300 { return Err("proms region too small".into()); }
        Self::build(gfx1, gfx2, build_palette(proms), proms[0x200..0x300].to_vec())
    }

    /// Donkey Kong 3 (TKG04): 4-bit resistor-network palette, colour codes at 0x400.
    pub fn new_dk3(gfx1: &[u8], gfx2: &[u8], proms: &[u8]) -> Result<Self, String> {
        if proms.len() < 0x500 { return Err("proms region too small".into()); }
        Self::build(gfx1, gfx2, build_palette_dk3(proms), proms[0x400..0x500].to_vec())
    }

    fn build(gfx1: &[u8], gfx2: &[u8], palette: [[u8; 4]; 256], color_codes: Vec<u8>) -> Result<Self, String> {
        if gfx1.len() < 0x1000 { return Err("gfx1 region too small".into()); }
        if gfx2.len() < 0x2000 { return Err("gfx2 region too small".into()); }

        // gfx1 holds two bitplanes, one per half (MAME charlayout: RGN_FRAC(1,2)):
        // DK 0x1000 = 256 tiles, DK Jr / DK3 0x2000 = 512 tiles.
        // gfx2 is in quarters (RGN_FRAC(1,4)): DK 0x2000 = 128 sprites, DK3 0x4000 = 256.
        let tile_count = gfx1.len() / 16;
        let sprite_count = gfx2.len() / 4 / 16;
        Ok(Self {
            video_ram: vec![0; 0x400],
            sprite_ram: vec![0; 0x400],
            flip: false,
            sprite_bank: 0,
            palette_bank: 0,
            gfx_bank: 0,
            tiles: (0..tile_count).map(|t| decode_tile(gfx1, t)).collect(),
            sprites: (0..sprite_count).map(|s| decode_sprite(gfx2, s)).collect(),
            palette,
            color_codes,
            framebuffer: vec![0; SCREEN_W * SCREEN_H * 4],
        })
    }

    pub fn render_line(&mut self, line: u16) {
        let mut pens = [0u8; SCREEN_W];
        let bank = self.palette_bank as usize * 16;
        let tile_base = self.gfx_bank as usize * 256;

        let ty = if self.flip { 255 - line as usize } else { line as usize };
        for x in 0..SCREEN_W {
            let tx = if self.flip { 255 - x } else { x };
            let (col, row) = (tx / 8, ty / 8);
            let code = (self.video_ram[row * 32 + col] as usize + tile_base) % self.tiles.len();
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
                // s[2] bit 6 is the sprite code's top bit (only DK3 has sprites up there)
                let code = ((s[1] & 0x7F) as usize + ((s[2] & 0x40) as usize) * 2) % self.sprites.len();
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

/// 8x8, 2 bitplanes: plane 0 in the first half of gfx1, plane 1 in the second half.
fn decode_tile(gfx1: &[u8], tile: usize) -> [u8; 64] {
    let half = gfx1.len() / 2;
    let mut px = [0u8; 64];
    for y in 0..8 {
        let lo = gfx1[tile * 8 + y];
        let hi = gfx1[half + tile * 8 + y];
        for x in 0..8 {
            let bit = 7 - x;
            px[y * 8 + x] = (((hi >> bit) & 1) << 1) | ((lo >> bit) & 1);
        }
    }
    px
}

/// 16x16, 2 bitplanes (second half / first half of gfx2); the left and right 8-pixel
/// halves of each sprite are a quarter of the region apart.
fn decode_sprite(gfx2: &[u8], code: usize) -> [u8; 256] {
    let (quarter, half) = (gfx2.len() / 4, gfx2.len() / 2);
    let mut px = [0u8; 256];
    for y in 0..16 {
        for x in 0..16 {
            let off = if x < 8 { 0 } else { quarter } + code * 16 + y;
            let bit = 7 - (x & 7);
            let lo = (gfx2[off] >> bit) & 1;
            let hi = (gfx2[half + off] >> bit) & 1;
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

/// DK3: R = 1D high nibble, G = 1D low nibble, B = 1C low nibble, each through a
/// 2200/1000/470/220 ohm network with a 470 ohm pull-up into a darlington, then the
/// inverting Sanyo EZV20 input (MAME dkong3_net_info / compute_res_net).
fn build_palette_dk3(proms: &[u8]) -> [[u8; 4]; 256] {
    let mut pal = [[0, 0, 0, 255]; 256];
    for (i, c) in pal.iter_mut().enumerate() {
        let r = proms[i] >> 4;
        let g = proms[i] & 0x0F;
        let b = proms[0x200 + i] & 0x0F;
        *c = [res_net_dk3(r), res_net_dk3(g), res_net_dk3(b), 255];
    }
    normalize_range(&mut pal);
    pal
}

fn res_net_dk3(inputs: u8) -> u8 {
    const R: [f64; 4] = [2200.0, 1000.0, 470.0, 220.0];
    const VOL: f64 = 0.05;          // TTL low
    const VOH: f64 = 4.0;           // TTL high
    const TTL_H_RES: f64 = 50.0;
    const R_BIAS: f64 = 470.0;
    const VBIAS: f64 = 5.0;
    const VCC: f64 = 5.0;
    const MINOUT: f64 = 0.7;        // darlington
    let (mut r_total, mut v) = (0.0, 0.0);
    for (i, r) in R.iter().enumerate() {
        if inputs >> i & 1 == 0 { r_total += 1.0 / r; v += VOL / r; }
    }
    r_total += 1.0 / R_BIAS;
    v += VBIAS / R_BIAS;
    // high outputs stop driving when the low ones already pull above VOH
    let open_col = v / r_total > VOH;
    for (i, r) in R.iter().enumerate() {
        if inputs >> i & 1 != 0 && !open_col {
            r_total += 1.0 / (r + TTL_H_RES);
            v += VOH / (r + TTL_H_RES);
        }
    }
    let mut v = (v / r_total).max(MINOUT);
    // Sanyo EZV20: inverted, 0.7 V dropped on each side
    v = VCC - v;
    v = (v - 0.7).max(0.0).min(VCC - 1.4);
    v = v / (VCC - 1.4) * VCC;
    (v * 255.0 / VCC + 0.4) as u8
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
