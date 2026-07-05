use godot::prelude::*;
use godot::global::godot_print;
use godot::classes::ImageTexture;
use crate::nes::mappers::Mapper;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

// The standard 64-color original NES system hardware color palette
const NES_PALETTE: [(u8, u8, u8); 64] = [
    (84, 84, 84),    (0, 30, 116),    (8, 16, 144),    (48, 0, 136),    (68, 0, 100),    (92, 0, 48),     (84, 4, 0),      (60, 24, 0),
    (32, 42, 0),     (8, 58, 0),      (0, 64, 0),      (0, 60, 0),      (0, 50, 60),     (0, 0, 0),       (0, 0, 0),       (0, 0, 0),
    (152, 150, 152), (8, 76, 196),    (48, 50, 236),   (92, 30, 228),   (136, 20, 176),  (160, 20, 116),  (152, 34, 32),   (112, 64, 0),
    (72, 88, 0),     (24, 114, 0),    (0, 124, 0),     (0, 118, 40),    (0, 102, 120),   (0, 0, 0),       (0, 0, 0),       (0, 0, 0),
    (236, 238, 236), (76, 154, 236),  (120, 124, 236), (176, 98, 236),  (228, 84, 236),  (236, 88, 180),  (236, 106, 100), (212, 136, 32),
    (160, 170, 0),   (116, 196, 0),   (76, 208, 32),   (56, 204, 108),  (56, 180, 204),  (60, 60, 60),    (0, 0, 0),       (0, 0, 0),
    (236, 238, 236), (168, 204, 236), (188, 194, 236), (212, 178, 236), (236, 174, 236), (236, 174, 212), (236, 180, 176), (228, 196, 144),
    (204, 210, 120), (180, 222, 120), (168, 226, 144), (152, 226, 180), (160, 214, 228), (160, 162, 160), (0, 0, 0),       (0, 0, 0),
];

struct SpritePixelInfo {
    color_bit: u8,
    palette_idx: u8,
    priority: u8,
    is_sprite_0: bool,
}

pub struct NesPPU {
    // --- Hardware Registers ---
    ctrl: u8,       // $2000
    mask: u8,       // $2001
    status: u8,     // $2002
    oam_addr: u8,   // $2003
    scroll: u16,    // $2005 internal latches
    addr: u16,      // $2006 internal latches
    scanline_bg: [(u8, u8, u8); 33],   // (low_byte, high_byte, palette_idx) per tile, this scanline
    scanline_sprites: Vec<(usize, u8, u8, u8, u8, bool)>, // (oam_index, sprite_x, low_byte, high_byte, attr, is_sprite_zero)
    mid_scanline_write: bool,
    // Addresses and internal latches
    base_nametable_address: u16,
    vram_increment:u8,
    sprite_pattern_table:u16,
    background_pattern_table: u16,
    sprite_size:u8,
    fine_x:u8,
    w_latch:bool,
    pub v_addr: u16,  // Current VRAM read/write pointer address (15 bits)
    pub t_addr: u16,  // Temporary internal address latch

    // memory blocks
    pub vram: [u8; 4096],
    pub palette_ram: [u8; 32],
    pub oam: [u8; 256], // 64 sprites * 4 bytes each
    data_buffer: u8,    // Delayed reading cache buffer for PPUDATA ($2007)

    // --- Timing & Synchronization ---
    pub frame_ready: bool,
    scanline: i16,   // -1 to 261
    cycle: i16,      // 0 to 340
    
    // --- Video Buffers ---
    // Double buffering prevents the UI thread from reading half-rendered frames!
    back_buffer: Vec<u8>,       // The frame currently being drawn (Width * Height * 4 bytes RGBA)
    front_buffer: Arc<Vec<u8>>, // The last fully completed frame, safe for sharing across threads
    
    // Direct shared reference to the system's atomic sync flag
    system_frame_ready: Arc<AtomicBool>,
    total_ppu_cycles: u64,
}

impl NesPPU {
    pub fn new(system_frame_ready: Arc<AtomicBool>) -> Self {
        let buffer_size = 256 * 240 * 4; // NES Resolution: 256x240, 4 bytes per pixel (RGBA8)
        Self {
            ctrl: 0, mask: 0, status: 0, oam_addr: 0, scroll: 0, addr: 0,
            base_nametable_address: 0x2000, vram_increment: 1,
            sprite_pattern_table: 0x0, background_pattern_table: 0x0, sprite_size: 8,
            mid_scanline_write: false,
            frame_ready: false, scanline: 0, cycle: 0, w_latch: false, fine_x: 0,
            v_addr: 0, t_addr: 0, data_buffer: 0,
            vram: [0;4096], palette_ram: [0;32], oam: [0;256],
            scanline_bg: [(0, 0, 0); 33], scanline_sprites: Vec::with_capacity(8),
            back_buffer: vec![0; buffer_size],
            front_buffer: Arc::new(vec![0; buffer_size]),
            system_frame_ready,
            total_ppu_cycles:0,
        }
    }

    pub fn reset(&mut self) {
        self.ctrl = 0; self.mask = 0; self.status = 0;
        self.scanline = 0; self.cycle = 0; self.frame_ready = false;
        self.w_latch = false; self.v_addr = 0; self.t_addr = 0;
    }

    pub fn catch_up(&mut self, mapper: &mut dyn Mapper, cpu_cycles: u64) {
        let target_ppu_cycles: u64 = cpu_cycles * 3;
        while self.total_ppu_cycles < target_ppu_cycles {
            self.step_one_ppu_cycle(mapper);
            self.total_ppu_cycles += 1;
        }
    }
    pub fn step_one_ppu_cycle(&mut self, mapper: &mut dyn Mapper) {
        let rendering_enabled = self.rendering_enabled();

        match self.scanline {
            0..=239 => {
                    // ---- VISIBLE SCANLINES ----
                if self.cycle == 0 {
                        // Batch render background and sprites using current v_addr state
//                      godot_print!("mask=0x{:02X}", self.mask);
                    self.mid_scanline_write = false;
                    self.prefetch_scanline(mapper);
                }

                if self.cycle < 256 {
                    self.render_pixel(mapper, self.cycle as usize);
                }

                if rendering_enabled {
                        // Increment coarse X every 8 dots across the visible scanline
                    if self.cycle > 0 && self.cycle <= 256 && self.cycle % 8 == 0 {
                        self.increment_coarse_x();
                    }
                        // Increment fine Y at the end of the tile fetching phase
                    if self.cycle == 256 {
                        self.increment_fine_y();
                    }
                        // Reset horizontal scroll back to starting parameters for the next line
                    if self.cycle == 257 {
                        self.copy_horizontal();
                    }
                    }
                }
                240 => {
                // ---- POST-RENDER BLANK SCANLINE ----
                // Idle scanline; no rendering or scrolling operations happen here.
                if self.cycle == 0 {
                    let mut completed_buffer = std::mem::replace(&mut self.back_buffer, vec![0; 256 * 240 * 4]);
                    self.front_buffer = Arc::new(completed_buffer);
                    self.system_frame_ready.store(true, Ordering::Release);
                }
                }
                241 => {
                // ---- VBLANK START SCANLINE ----
                    if self.cycle == 1 {
//                        godot_print!("palette_ram: {:02X?}", self.palette_ram);
                        self.status |= 0x80;
//                      godot_print!("VBlank set at scanline 241, status={:02X}|ctrl={:02X} total_cycles={}", self.status, self.ctrl, self.total_ppu_cycles);
                    }
                }
                242..=260 => {
                // ---- REMAINING VBLANK SCANLINES ----
                // Idle; CPU normally updates scrolling parameters (t_addr) during this window.
                }
                261 => {
                // ---- PRE-RENDER SCANLINE ----
                    if self.cycle == 1 {
                        self.status &= 0x3F;// Clear VBlank flag at start of new frame
                    }

                    if rendering_enabled {
                        // Replicate horizontal & fine Y progressions to keep registers synchronized
                        if self.cycle > 0 && self.cycle <= 256 && self.cycle % 8 == 0 {
                            self.increment_coarse_x();
                        }
                        if self.cycle == 256 {
                            self.increment_fine_y();
                        }
                        if self.cycle == 257 {
                            self.copy_horizontal();
                        }
                        // Crucial: Copy total vertical scroll configurations throughout the lookahead window
                        if self.cycle >= 280 && self.cycle <= 304 {
                            self.copy_vertical();
                        }
                    }
                }
                _ => {}
            }

        //  ---- MMC3 IRQ HOOK (From previous architecture evaluation) ----
        //  If your MMC3 mapper uses a dedicated scanline clock counter instead of filtering A12:
  //        if self.cycle == 260 && (self.scanline < 240 || self.scanline == 261) && rendering_enabled {
            if rendering_enabled {
                if self.scanline < 240 || self.scanline == 261 {
                    let target_clock_cycle = if self.background_pattern_table == 0x1000 {
                        4
                    } else { 260 };
                    if self.cycle == target_clock_cycle {
//                      if self.total_ppu_cycles % 50000 < 341 {
//                          godot_print!(
//                              "IRQ_DIAG: scanline={} cycle={} bg_table={:#X} spr_table={:#X} spr_size={} ctrl={:#04X}",
//                               self.scanline, self.cycle, self.background_pattern_table,
//                                self.sprite_pattern_table, self.sprite_size, self.ctrl
//                            );
//                      }
//                      godot_print!("IRQ Clocked: Scanline {}, Cycle {}", self.scanline, self.cycle);
                        mapper.clock_scanline();
                    }
                }
            }

            self.cycle += 1;
            if self.cycle >= 341 {
                self.cycle = 0;
                self.scanline += 1;
                // end of scanline
                if self.scanline > 261 {
                    self.scanline = 0; // Wrap back around to the top of the frame
                    mapper.notify_frame_start();
                }
        }
    }

    pub fn step(&mut self, mapper: &mut dyn Mapper, cycles: u64) {
        for _ in 0..cycles {
            let rendering_enabled = self.rendering_enabled();

            match self.scanline {
                0..=239 => {
                    // ---- VISIBLE SCANLINES ----
                    if self.cycle == 0 {
                        // Batch render background and sprites using current v_addr state
//                      godot_print!("mask=0x{:02X}", self.mask);
                        self.mid_scanline_write = false;
                        self.prefetch_scanline(mapper);
                    }

                    if self.cycle < 256 {
                        self.render_pixel(mapper, self.cycle as usize);
                    }

                    if rendering_enabled {
                        // Increment coarse X every 8 dots across the visible scanline
                        if self.cycle > 0 && self.cycle <= 256 && self.cycle % 8 == 0 {
                            self.increment_coarse_x();
                        }
                        // Increment fine Y at the end of the tile fetching phase
                        if self.cycle == 256 {
                            self.increment_fine_y();
                        }
                        // Reset horizontal scroll back to starting parameters for the next line
                        if self.cycle == 257 {
                            self.copy_horizontal();
                        }
                    }
                }
                240 => {
                // ---- POST-RENDER BLANK SCANLINE ----
                // Idle scanline; no rendering or scrolling operations happen here.
                if self.cycle == 0 {
                    let mut completed_buffer = std::mem::replace(&mut self.back_buffer, vec![0; 256 * 240 * 4]);
                    self.front_buffer = Arc::new(completed_buffer);
                    self.system_frame_ready.store(true, Ordering::Release);
                }
                }
                241 => {
                // ---- VBLANK START SCANLINE ----
                    if self.cycle == 1 {
//                        godot_print!("palette_ram: {:02X?}", self.palette_ram);
                        self.status |= 0x80;
                        godot_print!("VBlank set at scanline 241, status={:02X}|ctrl={:02X} total_cycles={}", self.status, self.ctrl, self.total_ppu_cycles);
                    }
                }
                242..=260 => {
                // ---- REMAINING VBLANK SCANLINES ----
                // Idle; CPU normally updates scrolling parameters (t_addr) during this window.
                }
                261 => {
                // ---- PRE-RENDER SCANLINE ----
                    if self.cycle == 1 {
                        godot_print!("PPU CLEARED VBLANK ON SCANLINE 261");
                        self.status &= 0x3F;// Clear VBlank flag at start of new frame
                    }

                    if rendering_enabled {
                        // Replicate horizontal & fine Y progressions to keep registers synchronized
                        if self.cycle > 0 && self.cycle <= 256 && self.cycle % 8 == 0 {
                            self.increment_coarse_x();
                        }
                        if self.cycle == 256 {
                            self.increment_fine_y();
                        }
                        if self.cycle == 257 {
                            self.copy_horizontal();
                        }
                        // Crucial: Copy total vertical scroll configurations throughout the lookahead window
                        if self.cycle >= 280 && self.cycle <= 304 {
                            self.copy_vertical();
                        }
                    }
                }
                _ => {}
            }

        //  ---- MMC3 IRQ HOOK (From previous architecture evaluation) ----
        //  If your MMC3 mapper uses a dedicated scanline clock counter instead of filtering A12:
  //        if self.cycle == 260 && (self.scanline < 240 || self.scanline == 261) && rendering_enabled {
            if rendering_enabled {
                if self.scanline < 240 || self.scanline == 261 {
                    let target_clock_cycle = if self.background_pattern_table == 0x1000 {
                        4
                    } else { 260 };
                    if self.cycle == target_clock_cycle {
//                      if self.total_ppu_cycles % 50000 < 341 {
//                          godot_print!(
//                              "IRQ_DIAG: scanline={} cycle={} bg_table={:#X} spr_table={:#X} spr_size={} ctrl={:#04X}",
//                               self.scanline, self.cycle, self.background_pattern_table,
//                                self.sprite_pattern_table, self.sprite_size, self.ctrl
//                            );
//                      }
//                      godot_print!("IRQ Clocked: Scanline {}, Cycle {}", self.scanline, self.cycle);
                        mapper.clock_scanline();
                    }
                }
            }

            // ---- ADVANCE PPU CLOCK DOTS ----
            self.cycle += 1;
            self.total_ppu_cycles += 1;
            if self.cycle >= 341 {
                self.cycle = 0;
                self.scanline += 1;

                if self.scanline > 261 {
                    self.scanline = 0; // Wrap back around to the top of the frame
                    mapper.notify_frame_start();
                }
            }
        }
    }

    fn increment_vertical_scroll(&mut self) {
        if self.v_addr & 0x7000 != 0x7000 {  // if fine Y < 7
            self.v_addr += 0x1000;            // increment fine Y
        }
        else {
            self.v_addr &= !0x7000;    // Fine Y = 0
            let mut y = (self.v_addr & 0x03E0) >> 5; // Coarse Y
            if y == 29 {                // if at bottom of name table
                y = 0;
                self.v_addr ^= 0x0800;       // switch vertical nametable
            }
            else if y == 31 {
                y = 0;
            }
            else {
                y += 1;
            }
            self.v_addr = (self.v_addr & !0x03E0) | (y << 5);
        }
    }

    pub fn increment_horizontal_scroll(&mut self) {
        // 1. Check if Coarse X has reached the end of the nametable row (Column 31)
        // 0x001F masks out bits 0-4
        if (self.v_addr & 0x001F) == 31 {
            // Coarse X wraps around to 0 (Clear the lowest 5 bits)
            self.v_addr &= !0x001F;
        
            // Switch to the neighboring horizontal nametable
            // Bit 10 controls the horizontal nametable; toggling it with XOR (^) swaps it
            self.v_addr ^= 0x0400;
        } else {
            // 2. Otherwise, simply move 1 tile to the right
            self.v_addr += 1;
        }
    }

fn rendering_enabled(&self) -> bool {
        // Checks PPUMASK ($2001) Bit 3 (Background visibility) or Bit 4 (Sprite visibility)
        (self.mask & 0x18) != 0
    }

    fn increment_coarse_x(&mut self) {
        if (self.v_addr & 0x001F) == 31 {
            self.v_addr &= !0x001F;       // Coarse X = 0
            self.v_addr ^= 0x0400;        // Switch horizontal nametable bit
        } else {
            self.v_addr += 1;             // Increment coarse X
        }
    }

    fn increment_fine_y(&mut self) {
        if (self.v_addr & 0x7000) != 0x7000 {
            self.v_addr += 0x1000;        // Increment fine Y
        } else {
            self.v_addr &= !0x7000;       // Fine Y = 0
            let mut y = (self.v_addr & 0x03E0) >> 5;
            if y == 29 {
                y = 0;
                self.v_addr ^= 0x0800;    // Switch vertical nametable bit
            } else if y == 31 {
                y = 0;                    // Coarse Y = 0, nametable does not switch
            } else {
                y += 1;
            }
            self.v_addr = (self.v_addr & !0x03E0) | (y << 5);
        }
    }

    fn copy_horizontal(&mut self) {
        // Copy coarse X (bits 0-4) and horizontal nametable (bit 10)
        // Mask: 0x041F
        self.v_addr = (self.v_addr & !0x041F) | (self.t_addr & 0x041F);
    }

    fn copy_vertical(&mut self) {
        // Copy fine Y (bits 12-14), coarse Y (bits 5-9), and vertical nametable (bit 11)
        // Mask: 0x7BE0
        self.v_addr = (self.v_addr & !0x7BE0) | (self.t_addr & 0x7BE0);
    }

    fn render_pixel(&mut self, mapper: &dyn Mapper, x: usize) {
        let mut bg_show = (self.mask & 0x08) != 0;
        let mut spr_show = (self.mask & 0x10) != 0;
        if x < 8 {
            if (self.mask & 0x02) == 0 {
                bg_show = false;  // Mask bit 1 is 0: Hide background in left 8 pixels
            }
            if (self.mask & 0x04) == 0 {
                spr_show = false; // Mask bit 2 is 0: Hide sprites in left 8 pixels
            }
        }

        if !bg_show && !spr_show {
            // If rendering is totally disabled via PPUMASK ($2001), output the universal background color
            let mut color_idx = self.palette_ram[0] & 0x3F;
            let (r, g, b) = NES_PALETTE[color_idx as usize];
            let pixel_index = (self.scanline as usize * 256 + x) * 4;
            self.back_buffer[pixel_index] = r;
            self.back_buffer[pixel_index + 1] = g;
            self.back_buffer[pixel_index + 2] = b;
            self.back_buffer[pixel_index + 3] = 255;
            return;
        }

        // ---- 1. EXTRACT BACKGROUND PIXEL ----
        let mut bg_pixel = 0u8;
        let mut bg_palette_idx = 0u8;

        // Only fetch background color if background rendering is enabled
        if bg_show {
            let total_offset = x + self.fine_x as usize;
            let tile_idx = total_offset / 8;
            let bit_shift = 7 - (total_offset % 8); 

            let (bg_low, bg_high, p_idx) = self.scanline_bg[tile_idx];
            bg_palette_idx = p_idx;
            let bg_color_bit0 = (bg_low >> bit_shift) & 1;
            let bg_color_bit1 = (bg_high >> bit_shift) & 1;
            bg_pixel = (bg_color_bit1 << 1) | bg_color_bit0; 
        }

        // ---- 2. EXTRACT SPRITE PIXEL ----
        let mut sprite_pixel = 0u8;
        let mut sprite_palette_idx = 0u8;
        let mut sprite_priority = 0u8; 
        let mut is_sprite_zero = false;

        // Only evaluate sprites if sprite rendering is enabled
        if spr_show {
            for sprite in &self.scanline_sprites {
                let (s_idx, s_x, s_low, s_high, s_attr, s_is_zero) = *sprite;
                let s_x = s_x as usize;

                if x >= s_x && x < s_x + 8 {
                    let mut s_bit_shift = 7 - (x - s_x);
                    if (s_attr & 0x40) != 0 {
                        s_bit_shift = x - s_x;
                    }

                    let s_color_bit0 = (s_low >> s_bit_shift) & 1;
                    let s_color_bit1 = (s_high >> s_bit_shift) & 1;
                    let p_pixel = (s_color_bit1 << 1) | s_color_bit0;

                    if p_pixel != 0 {
                        sprite_pixel = p_pixel;
                        sprite_palette_idx = s_attr & 0x03;
                        sprite_priority = (s_attr >> 5) & 1;
                        is_sprite_zero = s_is_zero;
                        break; 
                    }
                }
            }
        }

        // ---- 3. MULTIPLEXER / PRIORITY LOGIC ----
        let bg_opaque = bg_pixel != 0;
        let spr_opaque = sprite_pixel != 0;

        // Handle Sprite 0 Hit detection (Requires BOTH layers to be showing actively)
        if is_sprite_zero && bg_opaque && spr_opaque && bg_show && spr_show {
            let bg_clipped = (self.mask & 0x02) == 0 && x < 8;
            let spr_clipped = (self.mask & 0x04) == 0 && x < 8;
            if !bg_clipped && !spr_clipped && x < 255 {
                self.status |= 0x40; 
            }
        }


        // Determine whether background or sprite wins out
        let final_palette_offset = if spr_opaque && (!bg_opaque || sprite_priority == 0) {
            0x10 + (sprite_palette_idx as usize * 4) + sprite_pixel as usize
        } else if bg_opaque {
            (bg_palette_idx as usize * 4) + bg_pixel as usize
        } else {
            0x00
        };

        // ---- 4. PALETTE LOOKUP & BUFFER WRITE ----
        let color_idx = self.palette_ram[final_palette_offset] & 0x3F;
        let (r, g, b) = NES_PALETTE[color_idx as usize];
        let pixel_index = (self.scanline as usize * 256 + x) * 4;
        self.back_buffer[pixel_index] = r;
        self.back_buffer[pixel_index + 1] = g;
        self.back_buffer[pixel_index + 2] = b;
        self.back_buffer[pixel_index + 3] = 255;
    }

    fn prefetch_scanline(&mut self, mapper: &dyn Mapper) {
        let mut coarse_x = (self.v_addr & 0x001F) as u16;
        let mut h_nt = (self.v_addr >> 10) & 0x01;   // toggles as we walk across tiles
        let v_nt = (self.v_addr >> 11) & 0x01;       // fixed for the whole scanline
        let coarse_y = ((self.v_addr >> 5) & 0x1F) as usize;
        let fine_y = ((self.v_addr >> 12) & 0x07) as usize;

        for i in 0..33 {
            let base_nt = 0x2000 + ((v_nt << 1 | h_nt) * 0x400);
            let nt_addr = base_nt + (coarse_y as u16 * 32 + coarse_x);
            let tile_id = self.ppu_read(mapper, nt_addr, true);

            let pattern_addr = self.background_pattern_table + (tile_id as u16 * 16) + fine_y as u16;
            let low_byte = self.ppu_read(mapper, pattern_addr, true);
            let high_byte = self.ppu_read(mapper, pattern_addr + 8, true);

            let attr_table_addr = base_nt + 0x03C0 + ((coarse_y / 4) as u16 * 8) + (coarse_x / 4);
            let attr_byte = self.ppu_read(mapper, attr_table_addr, true);
            let quadrant_x = (coarse_x as usize % 4) / 2;
            let quadrant_y = (coarse_y % 4) / 2;
            let attr_shift = (quadrant_y * 2 + quadrant_x) * 2;
            let palette_idx = (attr_byte >> attr_shift) & 0x03;

            self.scanline_bg[i] = (low_byte, high_byte, palette_idx);

            coarse_x += 1;
            if coarse_x == 32 {
                coarse_x = 0;
                h_nt ^= 1;
            }
        }

        self.scanline_sprites.clear();
        let height = self.sprite_size as usize;
        for i in 0..64 {
            let oam_idx = i * 4;
            let sprite_y = self.oam[oam_idx] as usize;
            if (self.scanline as usize) < sprite_y + 1 || (self.scanline as usize) >= sprite_y + 1 + height {
                continue;
            }
            let sprite_tile = self.oam[oam_idx + 1];
            let sprite_attr = self.oam[oam_idx + 2];
            let sprite_x = self.oam[oam_idx + 3];

            let mut fine_y = self.scanline as usize - (sprite_y + 1);
            if (sprite_attr & 0x80) != 0 { fine_y = height - 1 - fine_y; }

            let table = if height == 16 { ((sprite_tile & 0x01) as u16) * 0x1000 } else { self.sprite_pattern_table };
            let actual_tile = if height == 16 { sprite_tile & 0xFE } else { sprite_tile };
            let mut tile_offset = 0u16;
            let mut final_fine_y = fine_y;
            if height == 16 && fine_y >= 8 { tile_offset = 1; final_fine_y -= 8; }

            let pattern_addr = table + ((actual_tile as u16 + tile_offset) * 16) + final_fine_y as u16;
            let low_byte = self.ppu_read(mapper, pattern_addr, false);
            let high_byte = self.ppu_read(mapper, pattern_addr + 8, false);

            self.scanline_sprites.push((i, sprite_x, low_byte, high_byte, sprite_attr, i == 0));
            if self.scanline_sprites.len() >= 8 { break; }
        }
    }

    fn get_sprite_pixel(&self, mapper: &dyn Mapper, x: usize, y: usize) -> Option<SpritePixelInfo> {
        let sprite_height = self.sprite_size as usize; // 8 or 16

        // Scan through all 64 sprites in OAM
        // OAM index 0 has highest priority, so the first opaque sprite pixel we hit wins!
        for i in 0..64 {
            let oam_base = i * 4;
            let sprite_y = self.oam[oam_base] as usize;
            let tile_id = self.oam[oam_base + 1];
            let attributes = self.oam[oam_base + 2];
            let sprite_x = self.oam[oam_base + 3] as usize;

            // NES sprites are delayed by 1 scanline in hardware. 
            // A sprite with Y=0 in OAM actually starts rendering on scanline 1.
            let actual_y = sprite_y + 1;

            // Check if the current pixel coordinate falls inside this sprite bounding box
            if y >= actual_y && y < actual_y + sprite_height {
                if x >= sprite_x && x < sprite_x + 8 {
                    
                    // Determine internal offsets inside the 8x8 or 8x16 sprite tile
                    let mut fine_y = y - actual_y;
                    let mut fine_x = x - sprite_x;

                    // Parse Attribute Byte flags
                    let flip_horizontal = (attributes & 0x40) != 0;
                    let flip_vertical = (attributes & 0x80) != 0;
                    let priority = (attributes & 0x20) >> 5; // 0 = Front, 1 = Behind
                    let palette_idx = attributes & 0x03;

                    // Handle Flipping
                    if flip_horizontal { fine_x = 7 - fine_x; }
                    if flip_vertical { fine_y = (sprite_height - 1) - fine_y; }

                    // Fetch the correct Pattern Table address for the sprite pixel
                    let pattern_addr = if sprite_height == 8 {
                        // 8x8 Sprite Mode
                        self.sprite_pattern_table + (tile_id as u16 * 16) + fine_y as u16
                    } else {
                        // 8x16 Sprite Mode (Used by many advanced games, though Mario uses 8x8)
                        // Bit 0 of tile_id determines the pattern table bank ($0000 or $1000)
                        let bank = (tile_id & 0x01) as u16 * 0x1000;
                        let mut actual_tile = tile_id & 0xFE;
                        if fine_y >= 8 {
                            actual_tile += 1;
                            fine_y -= 8;
                        }
                        bank + (actual_tile as u16 * 16) + fine_y as u16
                    };

                    let low_byte = self.ppu_read(mapper, pattern_addr, false);
                    let high_byte = self.ppu_read(mapper, pattern_addr + 8, false);

                    let bit_shift = 7 - fine_x;
                    let color_bit = ((low_byte >> bit_shift) & 0x01) | (((high_byte >> bit_shift) & 0x01) << 1);

                    // If this pixel is not transparent, we found our sprite color!
                    if color_bit != 0 {
                        return Some(SpritePixelInfo {
                            color_bit,
                            palette_idx,
                            priority,
                            is_sprite_0: i == 0, // Sprite 0 is the very first entry in OAM
                        });
                    }
                }
            }
        }
        None
    }

    /// Exposes a thread-safe read clone of the completed pixel array
    pub fn get_front_buffer(&self) -> Arc<Vec<u8>> {
        Arc::clone(&self.front_buffer)
    }

    pub fn is_in_vblank(&self) -> bool {
        return (self.status & 0x80) == 0x80;
    }

    pub fn is_nmi_enabled(&self) -> bool {
        return (self.ctrl & 0x80) == 0x80;
    }

    pub fn ppu_read(&self, mapper: &dyn Mapper, mut addr: u16,is_bg: bool) -> u8 {
        addr &= 0x3FFF;

        match addr {
            0x0000..=0x1FFF => mapper.ppu_read_ctx(addr, is_bg),
            0x2000..=0x3EFF => {
                let is_attribute_byte = (addr & 0x03FF) >= 0x03C0;
                mapper.read_nametable_byte(addr, &self.vram, is_attribute_byte)
            }
            0x3F00..=0x3FFF => {
                let mut palette_addr = (addr & 0x001F) as usize;
                if palette_addr >= 0x10 && (palette_addr % 4 == 0) { palette_addr -= 0x10; }
                self.palette_ram[palette_addr]
            }
            _ => 0,
        }
    }

    pub fn ppu_write(&mut self, mapper: &mut dyn crate::nes::mappers::Mapper, mut addr: u16, value: u8) {
        addr &= 0x3FFF;

//        mapper.check_a12(addr);

        match addr {
            0x0000..=0x1FFF => {
                mapper.ppu_write(addr, value);
            }
            0x2000..=0x3EFF => {
                let mirrored_addr = mapper.mirror_vram_address(addr);
                self.vram[(mirrored_addr as usize) & 0x0FFF] = value;
            }
            0x3F00..=0x3FFF => {
                let mut palette_addr = (addr & 0x001F) as usize;
                if palette_addr >= 0x10 && (palette_addr % 4 == 0) {
                    palette_addr -= 0x10;
                }
                self.palette_ram[palette_addr] = value & 0x3F;
            }
            _ => {}
        }
    }

    pub fn cpu_read_reg(&mut self, mapper: &dyn Mapper, reg: u16) -> u8 {
        match reg {
            2 => { // $2002 - PPUSTATUS
                let mut res = self.status;
//                if self.scanline >= 240 && self.scanline <= 252 {
                    if self.status & 0x80 != 0 {
                        godot_print!("$2002 read cleared v-blank: current scanline={}. Status={:02X}", self.scanline, self.status);
                    }
                self.status &= 0x7F; // Reading status clears V-Blank bit
                self.w_latch = false;    // And resets scroll/address double-write latch
                res
            }
            4 => {
                let mut value = self.oam[self.oam_addr as usize];
                if (self.oam_addr & 0x03) == 2 {
                    value &= 0xE3; // Keeps bits 7, 6, 5, 1, 0
                }
                value
            }
            7 => { // $2007 - PPUDATA
                let mut data = self.ppu_read(mapper, self.v_addr, false);
                if self.v_addr < 0x3F00 {
                    let buffered_data = self.data_buffer;
                    self.data_buffer = data;
                    data = buffered_data;
                } else {
                    self.data_buffer = self.ppu_read(mapper, self.v_addr - 0x1000, false);
                }
                self.v_addr = self.v_addr.wrapping_add(self.vram_increment as u16);
                data
            }
            _ => 0
        }
    }

    pub fn cpu_write_reg(&mut self, mapper: &mut dyn crate::nes::mappers::Mapper, reg: u16, value: u8) {
        if self.scanline >= 0 && self.scanline < 240 && self.cycle > 0 && self.cycle < 256 {
            if reg == 0 || reg == 1 || reg == 5 || reg == 6 {
                self.mid_scanline_write = true;
            }
        }
        match reg {
            0 => { // $2000 - PPUCTRL
                let old_ctrl = self.ctrl;
                self.ctrl = value;
                if (old_ctrl & 0x80) != (self.ctrl & 0x80) {
                    if (self.ctrl & 0x80) != 0 {
                        godot_print!("nmi_enabled");
                    } else {
                        godot_print!("nmi_disabled");
                    }
                }
                // Extract bits to configure scrolling targets
                self.t_addr = (self.t_addr & 0xF3FF) | (((value & 0x03) as u16) << 10);
                self.vram_increment = if (value & 0x04) == 0x04 { 32 } else { 1 };
                self.background_pattern_table = if (value & 0x10) == 0x10 { 0x1000 } else { 0x0000 };
                self.sprite_pattern_table = if (value & 0x08) == 0x08 { 0x1000 } else { 0x0000 };
                self.sprite_size = if (value & 0x20) == 0x20 { 16 } else { 8 };
            }
            1 => { // $2001 - PPUMASK
                self.mask = value;
            }
            3 => { // $2003 - OAMADDR
                self.oam_addr = value;
            }
            4 => { // $2004 - OAMDATA
                self.oam[self.oam_addr as usize] = value;
                self.oam_addr = self.oam_addr.wrapping_add(1);
            }
            5 => { // $2005 - PPUSCROLL
                if self.w_latch == false {
//                    godot_print!("PPUSCROLL first write {value}. scanline={}, cycle={}", self.scanline, self.cycle);
                    // First write: Coarse X and Fine X scrolling values
                    self.t_addr = (self.t_addr & 0x7FE0) | ((value >> 3) as u16);
                    self.fine_x = value & 0x07;
                    self.w_latch = true;

                } else {
//                    godot_print!("PPUSCROLL second write  {value}. scanline={}, cycle={}", self.scanline, self.cycle);
                    // Second write: Coarse Y and Fine Y scrolling values
                    self.t_addr = (self.t_addr & 0x0C1F) | (((value & 0x07) as u16) << 12) | (((value >> 3) as u16) << 5);
                    self.w_latch = false;
                }
            }
            6 => { // $2006 - PPUADDR
            
                if self.w_latch == false {
//                    godot_print!("PPUADDR first write  {value}. scanline={}, cycle={}", self.scanline, self.cycle);
                    // First write: High byte of the 14-bit destination target address
                    self.t_addr = (self.t_addr & 0x00FF) | (((value & 0x3F) as u16) << 8);
                    self.w_latch = true;
                } else {
                    let old_v = self.v_addr;
//                    godot_print!("PPUADDR second write {value}. scanline={}, cycle={}", self.scanline, self.cycle);
                    // Second write: Low byte of destination target address
                    self.t_addr = (self.t_addr & 0xFF00) | (value as u16);
                    self.v_addr = self.t_addr; // Latch copies address into current VRAM target
                    self.w_latch = false;

                    // If Bit 12 transitioned from 0 to 1, and the PPU isn't actively rendering:
                    let old_a12 = (old_v & 0x1000) != 0;
                    let new_a12 = (self.v_addr & 0x1000) != 0;

                    if !old_a12 && new_a12 && !self.rendering_enabled() {
                        mapper.clock_scanline();
                    }
                }
            }
            7 => { // $2007 - PPUDATA
                let old_v = self.v_addr;
                // Write the value into the destination VRAM address
                self.ppu_write(mapper, self.v_addr, value);
                // Automatically step forward the target address based on $2000 setup configurations
                self.v_addr = self.v_addr.wrapping_add(self.vram_increment as u16);

                let old_a12 = (old_v & 0x1000) != 0;
                let new_a12 = (self.v_addr & 0x1000) != 0;

                if !old_a12 && new_a12 && !self.rendering_enabled() {
                    mapper.clock_scanline();
                }
            }
            _ => {}
        }
    }

    pub fn write_oam_dma(&mut self, data: &[u8; 256]) {
        self.oam.copy_from_slice(data);
    }

    pub fn is_nmi_line_asserted(&self) -> bool {
        let nmi_occurred = (self.status & 0x80) != 0;
        let nmi_output = (self.ctrl & 0x80) != 0;
        nmi_occurred && nmi_output
    }
}