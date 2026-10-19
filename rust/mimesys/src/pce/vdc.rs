// Hudson HuC6270 VDC (Video Display Controller): backgrounds and sprites.
// 64 KB of VRAM (32K 16-bit words), reached through an address register + 2 data ports.
//
// CPU side (physical 0x1FE000-0x1FE3FF, A0-A1):
//   0  write: register select (AR, 0-0x13)       read: status
//   2  data low byte   3  data high byte (the high byte write commits 16-bit registers)
//
// Registers:
//   00 MAWR  VRAM write address        01 MARR  VRAM read address
//   02 VWR/VRR  VRAM data (write/read; the address steps by 1/32/64/128, CR bits 11-12)
//   05 CR    bit 0 collision IRQ, 1 overflow IRQ, 2 raster IRQ, 3 vblank IRQ,
//            bit 6 sprites on, bit 7 background on
//   06 RCR   raster compare: IRQ on the line whose counter equals this (first line = 64)
//   07 BXR / 08 BYR   background scroll
//   09 MWR   background map size: bits 4-5 width 32/64/128/128, bit 6 height 32/64 tiles
//   0A-0E    display timing (HSR HDR VPR VDW VCR): width = (HDR+1)*8, height = VDW+1
//   0F DCR   DMA control: bit 0 SATB-done IRQ, 1 VRAM-DMA-done IRQ, 2/3 dest/source decrement,
//            bit 4 repeat the sprite-table copy every vblank
//   10 SOUR 11 DESR 12 LENR   VRAM-to-VRAM DMA (starts when LENR is written)
//   13 DVSSR address of the sprite table in VRAM (copied into the chip at the next vblank)
//
// Status: bit 0 sprite 0 collision, 1 sprite overflow, 2 raster, 3 sprite-table DMA done,
//         4 VRAM DMA done, 5 vblank. Reading it clears them (and the interrupt).
//
// Background map entry: bits 0-11 tile number, bits 12-15 palette. Tile = 16 words:
//   words 0-7 = rows, planes 0 (low byte) and 1 (high byte); words 8-15 = planes 2 and 3.
// Sprite (4 words in the sprite table): Y (+64), X (+32), pattern, attributes:
//   bit 0-3 palette, 7 in front of background, 8 32 wide, 11 X flip, 12-13 16/32/64 tall, 15 Y flip
//   Pattern = 64 words: 4 planes of 16 rows x 16 bits.
//
// Reference: "pcetech.txt" (Charles MacDonald), Mednafen pce/vdc.cpp

use serde::{Deserialize, Serialize};

pub const MAX_WIDTH: usize = 512;

#[derive(Serialize, Deserialize, Clone)]
pub struct Vdc {
    #[serde(with = "serde_bytes")]
    vram_bytes: Vec<u8>,          // 64 KB, stored as bytes for serde; accessed as words
    pub regs: [u16; 0x20],
    ar: u8,
    pub status: u8,
    read_buffer: u16,
    pub satb: Vec<u16>,           // 256 words: 64 sprites x 4
    satb_pending: bool,
    bg_y: u16,                    // background line counter
    byr_written: bool,
    bxr_latch: u16,
}

impl Default for Vdc {
    fn default() -> Self { Self::new() }
}

impl Vdc {
    pub fn new() -> Self {
        let mut regs = [0u16; 0x20];
        regs[0x0A] = 0x0202;          // sane defaults until the game programs the timing
        regs[0x0B] = 0x031F;          // 256 wide
        regs[0x0C] = 0x0F02;
        regs[0x0D] = 0x00EF;          // 240 lines
        regs[0x0E] = 0x0003;
        Self {
            vram_bytes: vec![0; 0x10000],
            regs, ar: 0, status: 0, read_buffer: 0,
            satb: vec![0; 256], satb_pending: false,
            bg_y: 0, byr_written: false, bxr_latch: 0,
        }
    }

    pub fn vram(&self, addr: u16) -> u16 {
        let i = (addr as usize & 0x7FFF) * 2;
        self.vram_bytes[i] as u16 | (self.vram_bytes[i + 1] as u16) << 8
    }
    pub fn set_vram(&mut self, addr: u16, v: u16) {
        if addr & 0x8000 != 0 { return; }      // only 32K words fitted
        let i = addr as usize * 2;
        self.vram_bytes[i] = v as u8;
        self.vram_bytes[i + 1] = (v >> 8) as u8;
    }

    fn increment(&self) -> u16 {
        match (self.regs[5] >> 11) & 3 { 0 => 1, 1 => 32, 2 => 64, _ => 128 }
    }

    // ---------------- CPU ports ----------------

    pub fn read(&mut self, port: u32) -> u8 {
        match port & 3 {
            0 => {
                let v = self.status;
                self.status &= 0x40;          // clear everything but BUSY
                v
            }
            2 => self.read_buffer as u8,
            3 => {
                let v = (self.read_buffer >> 8) as u8;
                if self.ar == 0x02 {
                    self.regs[1] = self.regs[1].wrapping_add(self.increment());
                    self.read_buffer = self.vram(self.regs[1]);
                }
                v
            }
            _ => 0,
        }
    }

    pub fn write(&mut self, port: u32, v: u8) {
        match port & 3 {
            0 => self.ar = v & 0x1F,
            2 => {
                let r = self.ar as usize;
                self.regs[r] = (self.regs[r] & 0xFF00) | v as u16;
                self.after_write(false);
            }
            3 => {
                let r = self.ar as usize;
                self.regs[r] = (self.regs[r] & 0x00FF) | (v as u16) << 8;
                self.after_write(true);
            }
            _ => {}
        }
    }

    fn after_write(&mut self, high: bool) {
        match self.ar {
            0x01 if high => self.read_buffer = self.vram(self.regs[1]),
            0x02 if high => {
                let (addr, data) = (self.regs[0], self.regs[2]);
                self.set_vram(addr, data);
                self.regs[0] = addr.wrapping_add(self.increment());
            }
            0x08 => self.byr_written = true,
            0x12 if high => self.vram_dma(),
            0x13 if high => self.satb_pending = true,
            _ => {}
        }
    }

    fn vram_dma(&mut self) {
        let dcr = self.regs[0x0F];
        let (mut src, mut dst) = (self.regs[0x10], self.regs[0x11]);
        let len = self.regs[0x12] as u32 + 1;
        for _ in 0..len {
            let v = self.vram(src);
            self.set_vram(dst, v);
            src = if dcr & 0x08 != 0 { src.wrapping_sub(1) } else { src.wrapping_add(1) };
            dst = if dcr & 0x04 != 0 { dst.wrapping_sub(1) } else { dst.wrapping_add(1) };
        }
        self.regs[0x10] = src;
        self.regs[0x11] = dst;
        self.regs[0x12] = 0xFFFF;
        if self.regs[0x0F] & 0x02 != 0 { self.status |= 0x10; }
    }

    /// The VDC's interrupt output (IRQ1).
    pub fn irq_line(&self) -> bool {
        let cr = self.regs[5];
        let dcr = self.regs[0x0F];
        (self.status & 0x01 != 0 && cr & 0x01 != 0)
            || (self.status & 0x02 != 0 && cr & 0x02 != 0)
            || (self.status & 0x04 != 0 && cr & 0x04 != 0)
            || (self.status & 0x20 != 0 && cr & 0x08 != 0)
            || (self.status & 0x08 != 0 && dcr & 0x01 != 0)
            || (self.status & 0x10 != 0 && dcr & 0x02 != 0)
    }

    // ---------------- display timing ----------------

    pub fn display_width(&self) -> usize { (((self.regs[0x0B] & 0x7F) as usize + 1) * 8).min(MAX_WIDTH) }
    pub fn display_height(&self) -> usize { (self.regs[0x0D] & 0x1FF) as usize + 1 }
    /// First active line, counted from the start of vertical sync.
    pub fn active_start(&self) -> usize {
        let vpr = self.regs[0x0C];
        (vpr & 0x1F) as usize + (vpr >> 8) as usize + 3
    }

    /// Raster compare: `n` = lines since the display started (the counter reads 64 + n and
    /// keeps counting through the bottom border). Status flags are only raised when the game
    /// has enabled that interrupt, so a stale flag can't fire later at the wrong time.
    pub fn check_raster(&mut self, n: usize) {
        if self.regs[5] & 0x04 != 0 && (self.regs[6] & 0x3FF) as usize == n + 64 {
            self.status |= 0x04;
        }
    }

    /// Start of vertical blank: flag it and copy the sprite table if asked.
    pub fn start_vblank(&mut self) {
        if self.regs[5] & 0x08 != 0 { self.status |= 0x20; }
        if self.satb_pending || self.regs[0x0F] & 0x10 != 0 {
            let base = self.regs[0x13];
            for i in 0..256u16 { self.satb[i as usize] = self.vram(base.wrapping_add(i)); }
            self.satb_pending = false;
            if self.regs[0x0F] & 0x01 != 0 { self.status |= 0x08; }
        }
    }

    // ---------------- rendering ----------------

    /// Render active line `n` into `out` as VCE color indices (0-511; 0x100+ = sprite palettes).
    /// Pixels that are transparent everywhere get 0 (the backdrop).
    pub fn render_line(&mut self, n: usize, out: &mut [u16; MAX_WIDTH]) {
        // scroll registers take effect at the start of each line
        if n == 0 {
            self.bg_y = self.regs[8];
        } else if self.byr_written {
            self.bg_y = self.regs[8].wrapping_add(1);
        } else {
            self.bg_y = self.bg_y.wrapping_add(1);
        }
        self.byr_written = false;
        self.bxr_latch = self.regs[7];

        let width = self.display_width();
        out[..width].fill(0);
        let mut bg_opaque = [false; MAX_WIDTH];
        let cr = self.regs[5];
        if cr & 0x80 != 0 { self.draw_background(width, out, &mut bg_opaque); }
        if cr & 0x40 != 0 { self.draw_sprites(n, width, out, &bg_opaque); }
    }

    fn draw_background(&self, width: usize, out: &mut [u16; MAX_WIDTH], opaque: &mut [bool; MAX_WIDTH]) {
        let mwr = self.regs[9];
        let map_w = [32usize, 64, 128, 128][((mwr >> 4) & 3) as usize];
        let map_h = if mwr & 0x40 != 0 { 64usize } else { 32 };
        let y = self.bg_y as usize % (map_h * 8);
        let (row, fine_y) = (y / 8, y & 7);
        // decode one tile row (8 pixels) at a time
        let wrap = map_w * 8;
        let mut x = 0usize;
        let mut bx = self.bxr_latch as usize % wrap;
        while x < width {
            let entry = self.vram((row * map_w + bx / 8) as u16);
            let tile = entry & 0x0FFF;
            let w0 = self.vram(tile.wrapping_mul(16).wrapping_add(fine_y as u16));
            let w1 = self.vram(tile.wrapping_mul(16).wrapping_add(8 + fine_y as u16));
            let pal = (entry >> 12) * 16;
            let first = bx & 7;
            let count = (8 - first).min(width - x);
            for i in 0..count {
                let bit = 7 - (first + i);
                let p = ((w0 >> bit) & 1) | ((w0 >> (bit + 8)) & 1) << 1
                      | ((w1 >> bit) & 1) << 2 | ((w1 >> (bit + 8)) & 1) << 3;
                if p != 0 {
                    out[x + i] = pal + p;
                    opaque[x + i] = true;
                }
            }
            x += count;
            bx = (bx + count) % wrap;
        }
    }

    fn draw_sprites(&mut self, n: usize, width: usize, out: &mut [u16; MAX_WIDTH], bg_opaque: &[bool; MAX_WIDTH]) {
        let line = (n + 64) as i32;
        // which sprite drew each pixel (lower number wins); 0xFF = none
        let mut owner = [0xFFu8; MAX_WIDTH];
        let mut count = 0;
        for i in 0..64 {
            let e = &self.satb[i * 4..i * 4 + 4];
            let y = (e[0] & 0x3FF) as i32;
            let attr = e[3];
            let h = [16, 32, 64, 64][((attr >> 12) & 3) as usize];
            let row = line - y;
            if row < 0 || row >= h { continue; }
            count += 1;
            if count > 16 {
                if self.regs[5] & 0x02 != 0 { self.status |= 0x02; }   // overflow: only 16 sprites per line
                break;
            }
            let w = if attr & 0x0100 != 0 { 32 } else { 16 };
            let x0 = (e[1] & 0x3FF) as i32 - 32;
            let mut cell = (e[2] >> 1) & 0x3FF;
            if w == 32 { cell &= !1; }
            if h == 32 { cell &= !2; }
            if h == 64 { cell &= !6; }
            let r = if attr & 0x8000 != 0 { h - 1 - row } else { row } as u16;
            let palette = 0x100 + (attr & 0x0F) * 16;
            let front = attr & 0x80 != 0;
            for c in 0..w {
                let x = x0 + c;
                if x < 0 || x >= width as i32 { continue; }
                let cc = if attr & 0x0800 != 0 { w - 1 - c } else { c } as u16;
                let addr = (cell + cc / 16 + (r / 16) * 2) * 64 + (r % 16);
                let bit = 15 - (cc % 16);
                let p = (self.vram(addr) >> bit) & 1
                      | ((self.vram(addr + 16) >> bit) & 1) << 1
                      | ((self.vram(addr + 32) >> bit) & 1) << 2
                      | ((self.vram(addr + 48) >> bit) & 1) << 3;
                if p == 0 { continue; }
                let x = x as usize;
                if owner[x] != 0xFF {
                    if owner[x] == 0 && self.regs[5] & 0x01 != 0 { self.status |= 0x01; }   // sprite 0 collision
                    continue;
                }
                owner[x] = i as u8;
                if front || !bg_opaque[x] { out[x] = palette + p; }
            }
        }
    }
}
