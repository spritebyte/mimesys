// Popeye video (Nintendo TPP2 board).
//
// The picture is built at 512 x 512 (448 lines visible) from three layers:
//   1. Background: a 64 x 64 grid of 4-bit pixels, each one shown as an 8 x 8 block
//      (it's a low-res bitmap the game paints into, with scrolling).
//   2. Sprites: 16 x 16, 2 bits per pixel, drawn at full resolution.
//   3. Foreground text: 32 x 32 tiles of 8 x 8, 1 bit per pixel, shown at 2x (16 x 16).
//
// Sprite attributes, the background scroll and the palette bank are copied from
// work RAM (0x8C00-0x8E7F) by DMA at the start of vblank.
//
// Reference: MAME src/mame/nintendo/popeye_v.cpp (tpp2_state)

pub const SCREEN_W: usize = 512;
pub const SCREEN_H: usize = 448;   // lines 32-479 of the 512-line frame
const FIRST_LINE: usize = 32;
pub const DMA_SIZE: usize = 0x280;

pub struct PopeyeVideo {
    pub video_ram: Vec<u8>,        // 0xA000 (write only)  tile codes
    pub color_ram: Vec<u8>,        // 0xA400 (write only)  tile colors
    pub background_ram: Vec<u8>,   // 0xC000-0xDFFF writes, two pixels per byte
    pub sprite_ram: Vec<u8>,       // copy of the DMA area made at vblank
    pub scroll: [u8; 3],           // DMA bytes 0-2
    pub palette_bank: u8,          // DMA byte 3
    pub flip: bool,                // AY port B bit 0

    chars: Vec<[u8; 64]>,          // 256 decoded 8x8 tiles, one byte per pixel (0/1)
    sprites: Vec<[u8; 256]>,       // 512 decoded 16x16 sprites (0-3)
    bg_prom: [u8; 32],             // tpp2-c.4a
    char_prom: [u8; 32],           // tpp2-c.3a, with the address-pin quirk applied
    sprite_prom: Vec<u8>,          // 0x100: tpp2-c.5a/5b nibbles combined by the loader

    palette: [[u8; 3]; 80],        // 0-15 bg, 16-47 chars (pairs), 48-79 sprites
    palette_bank_cache: Option<u8>,

    line: Vec<u16>,                // one 512-pixel line of pens, reused
    sprite_line: Vec<u16>,
    pub framebuffer: Vec<u8>,      // RGBA, SCREEN_W x SCREEN_H
}

impl PopeyeVideo {
    pub fn new(gfx1: &[u8], gfx2: &[u8], proms: &[u8], sprpal: &[u8]) -> Result<Self, String> {
        if gfx1.len() < 0x800 { return Err(format!("gfx1 is 0x{:X} bytes, expected 0x800", gfx1.len())); }
        if gfx2.len() < 0x8000 { return Err(format!("gfx2 is 0x{:X} bytes, expected 0x8000", gfx2.len())); }
        if proms.len() < 0x40 { return Err(format!("proms is 0x{:X} bytes, expected 0x40", proms.len())); }
        if sprpal.len() < 0x100 { return Err(format!("sprpal is 0x{:X} bytes, expected 0x100", sprpal.len())); }

        // charlayout: 1 bpp, 8 bytes per tile, leftmost pixel in bit 0
        let chars = gfx1[..0x800].chunks_exact(8).map(|t| {
            let mut out = [0u8; 64];
            for y in 0..8 { for x in 0..8 { out[y * 8 + x] = (t[y] >> x) & 1; } }
            out
        }).collect();

        // spritelayout: 512 sprites, 16 bytes each per quarter of gfx2.
        //   x 0-7 from the second quarter, x 8-15 from the first, bit 0 = leftmost
        //   rows stored bottom to top
        //   plane 0 (high bit) in the first half, plane 1 in the second half
        let sprites = (0..512).map(|code| {
            let mut out = [0u8; 256];
            for y in 0..16 {
                for x in 0..16 {
                    let byte = code * 16 + (15 - y) + if x < 8 { 0x2000 } else { 0 };
                    let bit = x & 7;
                    let p0 = (gfx2[byte] >> bit) & 1;
                    let p1 = (gfx2[byte + 0x4000] >> bit) & 1;
                    out[y * 16 + x] = (p0 << 1) | p1;
                }
            }
            out
        }).collect();

        let mut bg_prom = [0u8; 32];
        bg_prom.copy_from_slice(&proms[..0x20]);
        // Two of the char PROM's address pins are tied together (MAME tpp1_state::tnx1_palette)
        let mut char_prom = [0u8; 32];
        for i in 0..32 {
            char_prom[i] = proms[0x20 + ((i & 0xF) | ((i & 0x8) << 1))];
        }

        Ok(Self {
            video_ram: vec![0; 0x400],
            color_ram: vec![0; 0x400],
            background_ram: vec![0; 0x1000],
            sprite_ram: vec![0; DMA_SIZE],
            scroll: [0; 3],
            palette_bank: 0,
            flip: false,
            chars,
            sprites,
            bg_prom,
            char_prom,
            sprite_prom: sprpal[..0x100].to_vec(),
            palette: [[0; 3]; 80],
            palette_bank_cache: None,
            line: vec![0; 512],
            sprite_line: vec![0; 512],
            framebuffer: vec![0; SCREEN_W * SCREEN_H * 4],
        })
    }

    /// 0xC000-0xDFFF: A12 picks which nibble of the byte is written.
    pub fn background_write(&mut self, offset: u16, data: u8) {
        let shift = if offset & 0x1000 != 0 { 4 } else { 0 };
        let o = (offset & 0x0FFF) as usize;
        self.background_ram[o] = (self.background_ram[o] & !(0x0F << shift)) | ((data & 0x0F) << shift);
    }

    /// Vblank DMA: sprites, scroll and palette bank come from the game's work RAM.
    pub fn dma(&mut self, src: &[u8]) {
        self.sprite_ram.copy_from_slice(&src[..DMA_SIZE]);
        self.scroll.copy_from_slice(&src[..3]);
        self.palette_bank = src[3];
    }

    fn update_palette(&mut self) {
        if self.palette_bank_cache == Some(self.palette_bank) { return; }
        let bank3 = ((self.palette_bank & 0x08) >> 3) as usize;
        // background: pens 0-15
        for i in 0..16 {
            let v = self.bg_prom[16 * bank3 + i];
            self.palette[i] = [
                res_net(v & 7, &BAK_RG, 470.0),
                res_net((v >> 3) & 7, &BAK_RG, 470.0),
                res_net(v >> 6, &BAK_B, 680.0),
            ];
        }
        // characters: pens 16-47, pairs of (black, color)
        for i in 0..16 {
            let v = self.char_prom[16 * bank3 + i];
            self.palette[16 + 2 * i] = [0, 0, 0];
            self.palette[16 + 2 * i + 1] = [
                res_net(v & 7, &TXT_RG, 470.0),
                res_net((v >> 3) & 7, &TXT_RG, 470.0),
                res_net(v >> 6, &TXT_B, 680.0),
            ];
        }
        // sprites: pens 48-79, 8 colors x 4, bank = palette_bank bits 0-2
        let base = 32 * (self.palette_bank & 7) as usize;
        for i in 0..32 {
            let v = self.sprite_prom[base + i];
            self.palette[48 + i] = [
                res_net(v & 7, &TXT_RG, 470.0),
                res_net((v >> 3) & 7, &TXT_RG, 470.0),
                res_net(v >> 6, &TXT_B, 680.0),
            ];
        }
        self.palette_bank_cache = Some(self.palette_bank);
    }

    pub fn render_frame(&mut self) {
        self.update_palette();
        for y in FIRST_LINE..FIRST_LINE + SCREEN_H {
            self.draw_background_line(y);
            self.draw_sprite_line(y);
            self.draw_foreground_line(y);
            let row = (y - FIRST_LINE) * SCREEN_W * 4;
            for x in 0..SCREEN_W {
                let rgb = self.palette[self.line[x] as usize];
                let o = row + x * 4;
                self.framebuffer[o..o + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 0xFF]);
            }
        }
    }

    /// tpp2_state::draw_background
    fn draw_background_line(&mut self, y: usize) {
        let half_y = if self.flip { ((y / 2) ^ 0xFF) as u16 } else { (y / 2) as u16 };
        let rovi = half_y + self.scroll[1] as u16;
        let shift = if rovi & 0x80 != 0 { 4 } else { 0 };
        for x in 0..512 {
            let roh = (0x38u16 + (x / 2) as u16 + self.scroll[0] as u16) as u8;
            let addr = if rovi & 0x100 != 0 {
                ((((rovi >> 1) & 0x3F) as usize) << 6) | ((roh >> 2) & 0x3F) as usize
            } else {
                0
            };
            self.line[x] = ((self.background_ram[addr] >> shift) & 0x0F) as u16;
        }
    }

    /// tnx1_state::draw_sprites, one line. Each sprite lands in one of 64 slots chosen
    /// by its x position, later sprites replacing earlier ones in the same slot.
    fn draw_sprite_line(&mut self, y: usize) {
        #[derive(Clone, Copy, Default)]
        struct Slot { row: usize, sx: i32, color: u8, code: usize, flipx: usize, flipy: usize }
        let mut slots = [Slot::default(); 64];
        let y = y as i32;

        for offs in (4..DMA_SIZE).step_by(4) {
            let s = &self.sprite_ram[offs..offs + 4];
            let mut sy = 0x200 - (s[1] as i32) * 2;
            let mut row = y - sy;
            if self.flip {
                sy ^= 0x1FF;
                row = sy - y;
            }
            if (0..16).contains(&row) {
                slots[(s[0] >> 2) as usize] = Slot {
                    row: row as usize,
                    sx: s[0] as i32 * 2,
                    code: (((s[2] & 0x7F) as usize) + (((s[3] & 0x10) as usize) << 3) + (((s[3] & 0x04) as usize) << 6)) ^ 0x1FF,
                    color: s[3] & 0x07,
                    flipx: if s[2] & 0x80 != 0 { 0xF } else { 0 },
                    flipy: if s[3] & 0x08 != 0 { 0xF } else { 0 },
                };
            }
        }

        self.sprite_line.iter_mut().for_each(|p| *p = 0);
        for a in slots.iter().filter(|a| a.color != 0) {
            let gfx = &self.sprites[a.code % 512];
            let src_row = (a.row ^ a.flipy) * 16;
            for x in 0..16 {
                let mut px = a.sx + x as i32 - 6;
                if !(0..512).contains(&px) { continue; }
                if self.flip { px ^= 0x1FF; }
                let p = gfx[src_row + (x ^ a.flipx)];
                // a sprite's transparent pixels still erase earlier sprites on this line
                self.sprite_line[px as usize] = if p != 0 { 48 + 4 * a.color as u16 + p as u16 } else { 0 };
            }
        }
        for x in 0..512 {
            if self.sprite_line[x] != 0 { self.line[x] = self.sprite_line[x]; }
        }
    }

    /// 32 x 32 tilemap of 8x8 chars scaled 2x, pen 0 transparent.
    fn draw_foreground_line(&mut self, y: usize) {
        let ty = if self.flip { 511 - y } else { y };
        for x in 0..512 {
            let tx = if self.flip { 511 - x } else { x };
            let idx = (ty / 16) * 32 + tx / 16;
            let code = self.video_ram[idx] as usize;
            let color = (self.color_ram[idx] & 0x0F) as u16;
            let pixel = self.chars[code][((ty % 16) / 2) * 8 + (tx % 16) / 2];
            if pixel != 0 {
                self.line[x] = 16 + 2 * color + pixel as u16;
            }
        }
    }
}

// ---- Resistor networks (MAME compute_res_net, Darlington amp, MB7051/7052 TTL
// ---- outputs, inverting Sanyo EZV20 monitor input) ----

const TXT_RG: [f64; 3] = [1000.0, 470.0, 220.0];
const TXT_B: [f64; 3] = [470.0, 220.0, 0.0];
const BAK_RG: [f64; 3] = [1200.0, 680.0, 470.0];
const BAK_B: [f64; 3] = [680.0, 470.0, 0.0];

fn res_net(inputs: u8, r: &[f64; 3], r_bias: f64) -> u8 {
    const VCC: f64 = 5.0;
    const V_BIAS: f64 = 5.0;
    const VOL: f64 = 0.05;      // TTL output low
    const VOH: f64 = 4.0;       // TTL output high
    const TTL_H_RES: f64 = 50.0;
    const MINOUT: f64 = 0.7;    // Darlington

    // low inputs pull toward VOL
    let (mut g, mut v) = (0.0, 0.0);
    for (i, &ri) in r.iter().enumerate() {
        if ri != 0.0 && (inputs >> i) & 1 == 0 {
            g += 1.0 / ri;
            v += VOL / ri;
        }
    }
    g += 1.0 / r_bias;
    v += V_BIAS / r_bias;
    // if the low inputs already pull above VOH, high outputs stop driving
    let open_collector = v / g > VOH;
    if !open_collector {
        for (i, &ri) in r.iter().enumerate() {
            if ri != 0.0 && (inputs >> i) & 1 != 0 {
                g += 1.0 / (ri + TTL_H_RES);
                v += VOH / (ri + TTL_H_RES);
            }
        }
    }
    let mut out = (v / g).max(MINOUT);

    // Sanyo EZV20 input: inverted, and clipped by two diode drops
    out = VCC - out;
    out = (out - 0.7).max(0.0).min(VCC - 1.4);
    out = out / (VCC - 1.4) * VCC;
    (out * 255.0 / VCC + 0.4) as u8
}

#[cfg(test)]
pub(crate) fn res_net_for_test(inputs: u8) -> (u8, u8) {
    (res_net(inputs, &TXT_RG, 470.0), res_net(inputs, &BAK_RG, 470.0))
}
#[cfg(test)]
pub(crate) fn palette_of(v: &mut PopeyeVideo) -> [[u8; 3]; 80] { v.update_palette(); v.palette }
