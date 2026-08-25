//use godot::prelude::*;
//use godot::classes::ImageTexture;
use godot::global::godot_print;
use crate::nes::mappers::Mapper;
use std::sync::atomic::{AtomicBool, Ordering};
use serde::{Serialize, Deserialize};
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

const SPRITE_A12_PHASE: u32 = 5;   // cycles 260, 268, 276, ...
const BG_A12_PHASE: u32     = 5;   // cycles 5, 13, 21, ... (BG pattern fetch)

struct SpritePixelInfo {
    color_bit: u8,
    palette_idx: u8,
    priority: u8,
    is_sprite_0: bool,
}

struct FrameDebug {
    frame_num: u64,
    sprite_0_dot: u16,
    first_2002: u16,
    first_2005: u16,
    first_2006: u16,
}

pub struct NesPPU {
    // --- Hardware Registers ---
    pub ctrl: u8,       // $2000
    pub mask: u8,       // $2001
    pub status: u8,     // $2002
    pub oam_addr: u8,   // $2003
    pub scroll: u16,    // $2005 internal latches
    pub addr: u16,      // $2006 internal latches
    scanline_bg: [(u8, u8, u8); 33],   // (low_byte, high_byte, palette_idx) per tile, this scanline
    scanline_sprites: Vec<(usize, u8, u8, u8, u8, bool)>, // (oam_index, sprite_x, low_byte, high_byte, attr, is_sprite_zero)
    a12_sprite_tiles: [Option<u8>; 8],
    mid_scanline_write: bool,
    // Addresses and internal latches
    pub base_nametable_address: u16,
    pub vram_increment:u8,
    sprite_pattern_table:u16,
    background_pattern_table: u16,
    sprite_size:u8,
    pub fine_x:u8,
    pub w_latch:bool,
    pub v_addr: u16,  // Current VRAM read/write pointer address (15 bits)
    pub t_addr: u16,  // Temporary internal address latch
    is_odd_frame: bool,
    bg_shift_low: u16,
    bg_shift_high: u16,
    attr_shift_low: u16,
    attr_shift_high: u16,
    // memory blocks
    pub vram: [u8; 4096],
    pub palette_ram: [u8; 32],
    pub oam: [u8; 256], // 64 sprites * 4 bytes each
    data_buffer: u8,    // Delayed reading cache buffer for PPUDATA ($2007)
    // --- Timing & Synchronization ---
    // Should maybe rename this to something like slice_complete to avoid confusion with the
    //  system_frame_ready which indicates front buffer is available to blit. 
    pub frame_ready: bool,
    vbl_suppressed: bool,
    nmi_suppressed: bool,

    pub scanline: u16,   // 0 to 262
    pub cycle: u32,      // 0 to 340
    // --- Video Buffers ---
    // Double buffering prevents the UI thread from reading half-rendered frames!
    back_buffer: Vec<u8>,       // The frame currently being drawn (Width * Height * 4 bytes RGBA)
    front_buffer: Arc<Vec<u8>>, // The last fully completed frame, safe for sharing across threads
    
    // Direct shared reference to the system's atomic sync flag
    system_frame_ready: Arc<AtomicBool>,
    pub total_ppu_cycles: u64,
    pub sprite_0_hit: bool,
    pub debug_trace_bg: bool,
    pub frame_number: u64,
    pub last_2002: u64,
    pub last_2005: u64,
    pub last_2005_2: u64,
    pub last_2006: u64,
    pub last_2006_2: u64,
    pub sprite_0_delay: u8,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PpuState {
    // Registers exposed via $2000-$2007
    pub ctrl: u8,
    pub mask: u8,
    pub status: u8,
    pub oam_addr: u8,

    // Internal scroll/address registers (the real "hidden" state that
    // makes precise resume possible mid-scanline, mid-scroll)
    pub v_addr: u16,
    pub t_addr: u16,
    pub fine_x: u8,
    pub w_latch: bool,

    // Background pipeline — shift registers must be saved exactly as-is,
    // since they hold pre-fetched tile data mid-flight between loads
    pub bg_shift_low: u16,
    pub bg_shift_high: u16,
    pub attr_shift_low: u16,
    pub attr_shift_high: u16,

    // Timing position
    pub scanline: u16,
    pub cycle: u32,
    pub total_ppu_cycles: u64,
    pub is_odd_frame: bool,

    pub vbl_suppressed: bool,
    pub nmi_suppressed: bool,

    pub background_pattern_table: u16,
    pub data_buffer: u8,
    // Memory
    #[serde(with = "serde_bytes")]
    pub oam: Vec<u8>,           // 256 bytes
    #[serde(with = "serde_bytes")]
    pub vram: Vec<u8>,          // 4096 bytes (nametables)
    #[serde(with = "serde_bytes")]
    pub palette_ram: Vec<u8>,   // 32 bytes

    pub a12_sprite_tiles: [Option<u8>; 8],
    pub scanline_sprites: Vec<(usize, u8, u8, u8, u8, bool)>,
    // Deliberately NOT saved:
    // - front_buffer/back_buffer: the framebuffer will simply repopulate
    //   as rendering resumes; May want to save front_buffer separately if
    //   I want to have thumbnail preview
}

const TARGET_SCANLINE: u16 = 174;

impl NesPPU {
    pub fn new(system_frame_ready: Arc<AtomicBool>) -> Self {
        let buffer_size = 256 * 240 * 4; // NES Resolution: 256x240, 4 bytes per pixel (RGBA8)
        Self {
            ctrl: 0, mask: 0, status: 0, oam_addr: 0, scroll: 0, addr: 0,
            base_nametable_address: 0x2000, vram_increment: 1,
            sprite_pattern_table: 0x0, background_pattern_table: 0x0, sprite_size: 8,
            mid_scanline_write: false, vbl_suppressed: false, nmi_suppressed: false,
            is_odd_frame: false,
            frame_ready: false, scanline: 261, cycle: 0, w_latch: false, fine_x: 0,
            bg_shift_low: 0, bg_shift_high: 0, attr_shift_low: 0, attr_shift_high: 0,
            v_addr: 0, t_addr: 0, data_buffer: 0,
            vram: [0;4096], palette_ram: [0;32], oam: [0;256],
            scanline_bg: [(0, 0, 0); 33], scanline_sprites: Vec::with_capacity(8),
            back_buffer: vec![0; buffer_size],
            front_buffer: Arc::new(vec![0; buffer_size]),
            system_frame_ready,
            total_ppu_cycles:0, frame_number: 0, last_2002: 0, last_2005: 0, last_2006: 0,
            last_2005_2: 0, last_2006_2: 0,
            sprite_0_hit: false, debug_trace_bg: false,
            a12_sprite_tiles: [None; 8], sprite_0_delay: 0,
        }
    }

    pub fn get_state(&self) -> PpuState {
        PpuState {
            ctrl: self.ctrl,
            mask: self.mask,
            status: self.status,
            oam_addr: self.oam_addr,
            v_addr: self.v_addr,
            t_addr: self.t_addr,
            fine_x: self.fine_x,
            w_latch: self.w_latch,
            bg_shift_low: self.bg_shift_low,
            bg_shift_high: self.bg_shift_high,
            attr_shift_low: self.attr_shift_low,
            attr_shift_high: self.attr_shift_high,
            scanline: self.scanline,
            cycle: self.cycle,
            total_ppu_cycles: self.total_ppu_cycles,
            is_odd_frame: self.is_odd_frame,
            vbl_suppressed: self.vbl_suppressed,
            nmi_suppressed: self.nmi_suppressed,
            data_buffer: self.data_buffer,
            background_pattern_table: self.background_pattern_table,
            oam: self.oam.to_vec(),
            vram: self.vram.to_vec(),
            a12_sprite_tiles: self.a12_sprite_tiles.clone(),
            palette_ram: self.palette_ram.to_vec(),
            scanline_sprites: self.scanline_sprites.clone(),
        }
    }

    pub fn load_state(&mut self, state: &PpuState) {
        self.ctrl = state.ctrl;
        self.mask = state.mask;
        self.status = state.status;
        self.oam_addr = state.oam_addr;
        self.v_addr = state.v_addr;
        self.t_addr = state.t_addr;
        self.fine_x = state.fine_x;
        self.w_latch = state.w_latch;
        self.bg_shift_low = state.bg_shift_low;
        self.bg_shift_high = state.bg_shift_high;
        self.attr_shift_low = state.attr_shift_low;
        self.attr_shift_high = state.attr_shift_high;
        self.scanline = state.scanline;
        self.cycle = state.cycle;
        self.total_ppu_cycles = state.total_ppu_cycles;
        self.is_odd_frame = state.is_odd_frame;

        self.vbl_suppressed = state.vbl_suppressed;
        self.nmi_suppressed = state.nmi_suppressed;
        self.data_buffer = state.data_buffer;
        self.scanline_sprites = state.scanline_sprites.clone();
        self.a12_sprite_tiles = state.a12_sprite_tiles.clone();
        self.background_pattern_table = state.background_pattern_table;
        self.oam.copy_from_slice(&state.oam);
        self.vram.copy_from_slice(&state.vram);
        self.palette_ram.copy_from_slice(&state.palette_ram);
    }

    pub fn reset(&mut self) {
        self.ctrl = 0; self.mask = 0; self.status = 0;
        self.scanline = 261; self.cycle = 0; self.frame_ready = false;
        self.w_latch = false; self.v_addr = 0; self.t_addr = 0;
        self.frame_number = 0;
    }

    pub fn is_sprite_0_hit(&self) -> bool {
        let val = self.sprite_0_hit;
        return val
    }

    pub fn toggle_debug_trace(&mut self) {
        self.debug_trace_bg = !self.debug_trace_bg;
        godot_print!("DEBUG_TRACE={}", self.debug_trace_bg);
    }

    pub fn is_frame_complete(&self) -> bool {
        self.frame_ready
    }

    pub fn clear_frame_complete_flag(&mut self) {
        self.frame_ready = false;
    }

    pub fn total_cycles(&self) -> u64 { return self.total_ppu_cycles; }

    pub fn step_one_cycle(&mut self, mapper: &mut dyn Mapper) {
        let rendering_enabled = self.rendering_enabled();
/*
        if self.decay_timer_upper > 0 {
            self.decay_timer_upper -= 1;
            if self.decay_timer_upper == 0 {
                self.ppu_open_bus &= 0x1F; // Clear upper 3 bits to 0
            }
        }
        if self.decay_timer_lower > 0 {
            self.decay_timer_lower -= 1;
            if self.decay_timer_lower == 0 {
                self.ppu_open_bus &= 0xE0; // Clear lower 5 bits to 0
            }
        }
*/
        match self.scanline {
            // ---- VISIBLE SCANLINES (0..=239) & PREFETCH SCANLINE (261) ----
            0..=239 | 261 => {
                let is_prefetch = self.scanline == 261;
                if self.sprite_0_delay > 0 {
                    self.sprite_0_delay -= 1;
                    if self.sprite_0_delay == 0 {
                        self.status |= 0x40;
                    }
                }
                // 1. Visible Screen Rendering (Skip during prefetch line)
                if !is_prefetch && (1..=256).contains(&self.cycle) {
                    self.render_pixel(mapper, (self.cycle - 1) as usize);
                }

                if is_prefetch && self.cycle == 1 {
                    self.status &= 0x1F;
                    self.nmi_suppressed = false;
                }

                if rendering_enabled {
                    match self.cycle {
                        0 => {
                            if !is_prefetch {
                                self.evaluate_sprites_for_scanline(mapper);
                            }
                        }
                        // Background coarse X increments every 8 dots
                        c if (9..=249).contains(&c) && c % 8 == 1 => {
                            self.load_background_shifters(mapper);
                            self.increment_coarse_x();
                        }
                        256 => {
                            self.increment_coarse_x();
                            self.increment_vertical_scroll();
                            if self.scanline == 175 {
//                                godot_print!("[{}] PPU dot 256 incremented coarse x and y. scanline={}, cycle={}. Total Cycles={ }. V={} T={}. frame parity={}", self.frame_number, self.scanline, self.cycle, self.total_ppu_cycles, self.v_addr, self.t_addr, self.frame_number % 2);
                            }
                        }
                        257 => {
                            self.collect_next_line_sprite_tiles();
                            self.copy_horizontal(); // Update active X from temporary X
                        }
                        260 => {
                            mapper.notify_scanline();
                        }
                        304 => {
                            if is_prefetch {
                                self.copy_vertical(); // Reset active Y for next frame
                            }
                        }
                        328 => {
                            self.load_background_shifters_high(mapper);
                            self.increment_coarse_x();
                        }
                        336 => {
                            self.load_background_shifters(mapper);
                            self.increment_coarse_x();
                        }
                        _ => {}
                    }

                    if (257..=320).contains(&self.cycle) {
                       self.oam_addr = 0;
                    }
                    let first_sprite_dot = 256 + SPRITE_A12_PHASE;
//                    godot_print!("First sprite dot={}",first_sprite_dot);
                    let in_sprite_region = (257..=320).contains(&self.cycle);
                    let phase = if in_sprite_region { SPRITE_A12_PHASE } else { BG_A12_PHASE };

                    if self.cycle % 8 == phase {
                        let a12_addr = if in_sprite_region {
                            let slot = ((self.cycle - first_sprite_dot) / 8) as usize;  // 260 -> slot 0
                            if self.sprite_size == 16 {
                                match self.a12_sprite_tiles[slot] {
                                    Some(tile) if (tile & 0x01) != 0 => 0x1000,
                                    Some(_) => 0x0000,
                                    None => 0x1000,
                                }
                            } else {
                                self.sprite_pattern_table
                            }
                        } else {
                            self.background_pattern_table
                        };
                        mapper.update_a12(a12_addr, self.total_ppu_cycles);
                    }
                }
            }

            // ---- POST-RENDER / IDLE SCANLINE ----
            240 => {
                if self.cycle == 0 {
                    // End of frame: swap buffers safely
                    if rendering_enabled {
                        self.evaluate_sprite_overflow(240);
                    }
                    let completed_buffer = std::mem::replace(&mut self.back_buffer, vec![0; 256 * 240 * 4]);
                    self.front_buffer = Arc::new(completed_buffer);
                    self.system_frame_ready.store(true, Ordering::Release);
                }
            }

            // ---- VBLANK PERIOD (241..=260) ----
            241..=260 => {
                if self.scanline == 241 && self.cycle == 1 {
                    if !self.vbl_suppressed {
                        self.status |= 0x80; // Set VBlank flag
                    }
                    self.vbl_suppressed = false;
                }
            }
            _ => unreachable!(),
        }

//        if rendering_enabled && ((1..=256).contains(&self.cycle) || (321..=335).contains(&self.cycle)) {
        if rendering_enabled && ((1..=256).contains(&self.cycle)) {
//            godot_print!("{} shifting registers for next cycle. Scanline={}, cycle={}", self.frame_number, self.scanline, self.cycle);
            self.bg_shift_low <<= 1;
            self.bg_shift_high <<= 1;
            self.attr_shift_low <<= 1;
            self.attr_shift_high <<= 1;
        }
        // 5. Always advance the cycle counters at the very end
        self.advance_cycle(mapper);
    }

    // Helper method to keep stepping neat and eliminate the double cycle bug
    #[inline(always)]
    fn advance_cycle(&mut self, mapper: &mut dyn Mapper) {
        self.cycle += 1;
        self.total_ppu_cycles += 1;

        let rendering_enabled = (self.mask & 0x18) != 0;
        let should_skip = rendering_enabled && self.scanline == 261 && self.cycle == 339 && self.is_odd_frame;

        if should_skip {
            self.cycle = 340; // Artificially skip directly to cycle 340, which wraps next
        }

        if self.cycle >= 341 {
            self.cycle = 0;
            self.scanline += 1;

            if self.scanline >= 262 {
                self.scanline = 0;
                self.is_odd_frame = !self.is_odd_frame;
                self.frame_ready = true;
                mapper.notify_frame_start();
                self.frame_number += 1;
            }
        }
    }

    fn collect_next_line_sprite_tiles(&mut self) {
        self.a12_sprite_tiles = [None; 8];
        let target = if self.scanline == 261 { 0 } else { self.scanline as i32 + 1 };
        let h = if (self.ctrl & 0x20) != 0 { 16 } else { 8 };
        let mut n = 0;
        for i in 0..64 {
            let y = self.oam[i * 4] as i32;
            if y >= 240 { continue; }
            let row = target - (y + 1);
            if row >= 0 && row < h {
                self.a12_sprite_tiles[n] = Some(self.oam[i * 4 + 1]);
                n += 1;
                if n == 8 { break; }
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

    // dead code
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
        let before_nt_x = (self.v_addr >> 10) & 1;
        if (self.v_addr & 0x001F) == 31 {
            self.v_addr &= !0x001F;       // Coarse X = 0
            self.v_addr ^= 0x0400;        // Switch horizontal nametable bit
        } else {
            self.v_addr += 1;             // Increment coarse X
        }
        let after_nt_x = (self.v_addr >> 10) & 1;
/*        if before_nt_x != after_nt_x {
            godot_print!(
                "[nt-x flip] scanline={} cycle={} v_addr={:04X} coarse_x={}",
                self.scanline, self.cycle, self.v_addr, self.v_addr & 0x1F
            );
        } */
    }

    // deprecated
    fn increment_fine_y(&mut self) {
        let before_nt_y = (self.v_addr >> 11) & 1;
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
        let after_nt_y = (self.v_addr >> 11) & 1;
/*        if before_nt_y != after_nt_y {
            godot_print!(
                "[nt-y flip] scanline={} cycle={} v_addr={:04X} coarse_y={} fine_y={}",
                self.scanline, self.cycle, self.v_addr, (self.v_addr >> 5) & 0x1F, (self.v_addr >> 12) & 7
            );
        } */
    }

    fn copy_horizontal(&mut self) {
        // Copy coarse X (bits 0-4) and horizontal nametable (bit 10)
        // Mask: 0x041F
        let old_v_addr = self.v_addr;
        self.v_addr = (self.v_addr & !0x041F) | (self.t_addr & 0x041F);
//        godot_print!("{} copy_horizontal: v_addr={}, was {}. t_addr={}", self.frame_number, self.v_addr, old_v_addr, self.t_addr);
    }

    fn copy_vertical(&mut self) {
        // Copy fine Y (bits 12-14), coarse Y (bits 5-9), and vertical nametable (bit 11)
        // Mask: 0x7BE0
        self.v_addr = (self.v_addr & !0x7BE0) | (self.t_addr & 0x7BE0);
    }

    fn render_pixel(&mut self, mapper: &dyn Mapper, x: usize) {
        let mut bg_show = (self.mask & 0x08) != 0;
        let mut spr_show = (self.mask & 0x10) != 0;
        let y = (self.v_addr & 0x03E0) >> 5;
        if x < 8 {
            if (self.mask & 0x02) == 0 { bg_show = false; } // Hide left 8px BG
            if (self.mask & 0x04) == 0 { spr_show = false; } // Hide left 8px Sprites
        }

//    if self.rendering_enabled() && (x % 8 == 7) {
//        self.load_background_shifters(mapper);
//    }

        let mut bg_pixel = 0u8;
        let mut bg_palette_idx = 0u8;

        if bg_show {
            let bit_shift = 15 - (self.fine_x as u32);
            let bg_color_bit0 = ((self.bg_shift_low >> bit_shift) & 1) as u8;
            let bg_color_bit1 = ((self.bg_shift_high >> bit_shift) & 1) as u8;
            bg_pixel = (bg_color_bit1 << 1) | bg_color_bit0;

            let attr_bit0 = ((self.attr_shift_low >> bit_shift) & 1) as u8;
            let attr_bit1 = ((self.attr_shift_high >> bit_shift) & 1) as u8;
            bg_palette_idx = (attr_bit1 << 1) | attr_bit0;

            if let Some((side, tile_count)) = mapper.split_config() {
                let split_width = tile_count as usize * 8;
                let in_split = if side { x >= 256 - split_width } else { x < split_width };
                if in_split {
                    let (lo, hi, pal) = mapper.read_split_tile(x % 8 + (x / 8) * 8, self.scanline as usize);
                    let bit = 7 - (x % 8);
                    bg_pixel = (((hi >> bit) & 1) << 1) | ((lo >> bit) & 1);
                    bg_palette_idx = pal;
                }
            }
        }

        // ---- 2. EXTRACT SPRITE PIXEL ----
        let mut sprite_pixel = 0u8;
        let mut sprite_palette_idx = 0u8;
        let mut sprite_priority = 0u8; 
        let mut is_sprite_zero = false;

        if spr_show {
            for sprite in &self.scanline_sprites {
                let (_, s_x, s_low, s_high, s_attr, s_is_zero) = *sprite;
                let s_x = s_x as usize;

                if x >= s_x && x < s_x + 8 {
                    let mut s_bit_shift = 7 - (x - s_x);
                    if (s_attr & 0x40) != 0 { s_bit_shift = x - s_x; } // Horizontal flip

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

        // ---- 3. MULTIPLEXER / PRIORITY & SPRITE 0 HIT ----
        let bg_opaque = bg_pixel != 0;
        let spr_opaque = sprite_pixel != 0;

        if is_sprite_zero && bg_opaque && spr_opaque && bg_show && spr_show {
            // Sprite 0 cannot hit at x=255 or if either side is clipped in the left 8 pixels
            if x < 255 {
                if self.status & 0x40 == 0 {
//                  godot_print!("[{}] Sprite 0 hit detected", self.total_ppu_cycles);
//                    godot_print!("[{}] ->Position: x={}. y={} Scanline={}, PPU cycle={}. V={}, T={}, Fine_X={}", self.frame_number, x, y, self.scanline, self.cycle, self.v_addr, self.t_addr, self.fine_x);
                }
//                self.status |= 0x40;
                self.sprite_0_delay = 1;
            }
        }

        let final_palette_offset = if spr_opaque && (!bg_show || !bg_opaque || sprite_priority == 0) {
            // If the background layer is hidden entirely (bg_show == false) but sprite has priority 1, 
            // real hardware drops back to the universal background color (0x00) instead of bleeding through.
            if !bg_show && sprite_priority == 1 {
                0x00
            } else {
                0x10 + (sprite_palette_idx as usize * 4) + sprite_pixel as usize
            }
        } else if bg_opaque {
            (bg_palette_idx as usize * 4) + bg_pixel as usize
        } else {
            0x00
        };

        // ---- 4. WRITE PIXEL TO BACK BUFFER ----
        let color_idx = self.palette_ram[final_palette_offset] & 0x3F;
        let (r, g, b) = NES_PALETTE[color_idx as usize];
        let pixel_index = (self.scanline as usize * 256 + x) * 4;
        self.back_buffer[pixel_index] = r;
        self.back_buffer[pixel_index + 1] = g;
        self.back_buffer[pixel_index + 2] = b;
        self.back_buffer[pixel_index + 3] = 255;
/*
        if self.debug_trace_bg && self.scanline == TARGET_SCANLINE && x >= 232 {
            godot_print!(
                "({}) [pixel] x={} bg_shift_low={:04X} bg_shift_high={:04X} bg_pixel={} bg_palette={}",
                self.frame_number, x, self.bg_shift_low, self.bg_shift_high, bg_pixel, bg_palette_idx); 
        } */
/*
    // ---- 5. SHIFT REGISTERS FOR THE NEXT CYCLE ----
    if (self.cycle >= 1 && self.cycle <= 256) || (self.cycle >= 321 && self.cycle <= 336) {
        if (self.mask & 0x18) != 0 {
            self.bg_shift_low <<= 1;
            self.bg_shift_high <<= 1;
            self.attr_shift_low <<= 1;
            self.attr_shift_high <<= 1;
        }
    }
*/
    }

    pub fn is_bg_fetch(&self) -> bool {
        if !self.rendering_enabled() {
            return false;
        }
        
        let is_visible_line = (0..=239).contains(&self.scanline) || self.scanline == 261;
        let is_bg_cycle = (1..=256).contains(&self.cycle) || (321..=336).contains(&self.cycle);
        
        is_visible_line && is_bg_cycle
    }

    fn load_background_shifters(&mut self, mapper: &dyn Mapper) {
        // 1. Fetch Tile ID from the PPU's internal VRAM (Not the mapper!)
        let nt_addr = 0x2000 | (self.v_addr & 0x0FFF);
        let tile_id = mapper.read_nametable_byte(nt_addr, &self.vram, false);
/*
    if self.debug_trace_bg && self.scanline == TARGET_SCANLINE && self.cycle == 240 {
        godot_print!(
            "({}) [last-tile fetch] scanline={} cycle={} v_addr={:04X} coarse_x={} nt_select_x={} tile_id={:02X}",
            self.frame_number, self.scanline, self.cycle, self.v_addr, self.v_addr & 0x1F, (self.v_addr >> 10) & 1, tile_id
        );
    }
*/
        // 2. Fetch Attribute Byte from the PPU's internal VRAM
        let attr_addr = 0x23C0 | (self.v_addr & 0x0C00) | ((self.v_addr >> 4) & 0x38) | ((self.v_addr >> 2) & 0x07);
//    let attr_vram_index = mapper.mirror_vram_address(attr_addr) as u16;
        let attr_byte = mapper.read_nametable_byte(attr_addr, &self.vram, true); 

        // Parse attribute byte to find the 2-bit palette index
        let coarse_x = self.v_addr & 0x001F;
        let coarse_y = (self.v_addr >> 5) & 0x001F;
        let top_bottom = (coarse_y >> 1) & 1; 
        let left_right = (coarse_x >> 1) & 1; 
        let shift = (top_bottom << 2) | (left_right << 1);
        let palette_idx = (attr_byte >> shift) & 0x03;

        // 3. Fetch Pattern Table Low and High bytes (THIS goes to the mapper!)
        let fine_y = (self.v_addr >> 12) & 0x07;
        let bg_table_base = self.background_pattern_table; 
        let pattern_addr_low = bg_table_base + ((tile_id as u16) << 4) + fine_y;

        // Pattern table addresses are < 0x2000, so the mapper safely processes them
        let bg_low = mapper.ppu_read_ctx(pattern_addr_low, self.is_bg_fetch());
        let bg_high = mapper.ppu_read_ctx(pattern_addr_low + 8, self.is_bg_fetch());

        // Load the lower 8 bits of our 16-bit shifters with the fresh data
        self.bg_shift_low = (self.bg_shift_low & 0xFF00) | (bg_low as u16);
        self.bg_shift_high = (self.bg_shift_high & 0xFF00) | (bg_high as u16);

        // Explode the 2-bit palette attributes into individual bitplanes for the 8 pixels
        let attr_bit0 = if (palette_idx & 0x01) != 0 { 0xFF } else { 0x00 };
        let attr_bit1 = if (palette_idx & 0x02) != 0 { 0xFF } else { 0x00 };
    
        self.attr_shift_low = (self.attr_shift_low & 0xFF00) | attr_bit0;
        self.attr_shift_high = (self.attr_shift_high & 0xFF00) | attr_bit1;
    }

    fn load_background_shifters_high(&mut self, mapper: &dyn Mapper) {
        // Same fetch as load_background_shifters, but seeds the HIGH byte directly —
        // this tile has no more per-dot shifts left before it's displayed at the
        // start of the next scanline (mirrors real hardware's dot ~321-328 fetch).
        let nt_addr = 0x2000 | (self.v_addr & 0x0FFF);
        let tile_id = mapper.read_nametable_byte(nt_addr, &self.vram, false);

        // 2. Fetch Attribute Byte from the PPU's internal VRAM
        let attr_addr = 0x23C0 | (self.v_addr & 0x0C00) | ((self.v_addr >> 4) & 0x38) | ((self.v_addr >> 2) & 0x07);
        let attr_byte = mapper.read_nametable_byte(attr_addr, &self.vram, true); 

        let coarse_x = self.v_addr & 0x001F;
        let coarse_y = (self.v_addr >> 5) & 0x001F;
        let shift = (((coarse_y >> 1) & 1) << 2) | (((coarse_x >> 1) & 1) << 1);
        let palette_idx = (attr_byte >> shift) & 0x03;

        let fine_y = (self.v_addr >> 12) & 0x07;
        let pattern_addr_low = self.background_pattern_table + ((tile_id as u16) << 4) + fine_y;
        let bg_low = mapper.ppu_read_ctx(pattern_addr_low, self.is_bg_fetch());
        let bg_high = mapper.ppu_read_ctx(pattern_addr_low + 8, self.is_bg_fetch());

        self.bg_shift_low  = (self.bg_shift_low  & 0x00FF) | ((bg_low  as u16) << 8);
        self.bg_shift_high = (self.bg_shift_high & 0x00FF) | ((bg_high as u16) << 8);

        let attr_bit0 = if (palette_idx & 0x01) != 0 { 0xFF00 } else { 0x0000 };
        let attr_bit1 = if (palette_idx & 0x02) != 0 { 0xFF00 } else { 0x0000 };
        self.attr_shift_low  = (self.attr_shift_low  & 0x00FF) | attr_bit0;
        self.attr_shift_high = (self.attr_shift_high & 0x00FF) | attr_bit1;
    }

    fn evaluate_sprites_for_scanline(&mut self, mapper: &dyn Mapper) {
        self.scanline_sprites.clear();

        let sprite_height = if (self.ctrl & 0x20) != 0 { 16 } else { 8 };
        let sprite_table_base = if (self.ctrl & 0x08) != 0 { 0x1000 } else { 0x0000 };

        let current_y = self.scanline as i32;

        // Scan through all 64 available sprites in OAM
        for i in 0..64 {
            let oam_idx = i * 4;
            let sprite_y = self.oam[oam_idx] as i32;
        
            // A sprite Y coordinate of 239+ means it's hidden or off-screen
            if sprite_y >= 240 { continue; }

            // Check if the sprite vertically intersects the current scanline
            // Note: Sprites are delayed by 1 scanline in hardware rendering
            let row = current_y - (sprite_y + 1);
            if row >= 0 && row < sprite_height {
                // Keep a hardware limit of 8 sprites per scanline
                if self.scanline_sprites.len() >= 8 {
                    self.status |= 0x20;
                    break;
                }

                let tile_id = self.oam[oam_idx + 1];
                let attr = self.oam[oam_idx + 2];
                let x = self.oam[oam_idx + 3];

                // Determine if this is Sprite 0 for hit detection
                let is_sprite_zero = i == 0;

                // Handle vertical flipping
                let flip_y = (attr & 0x80) != 0;
                let mut sprite_row = row;
                if flip_y {
                    sprite_row = (sprite_height - 1) - row;
                }

                // Calculate the pattern table address for this sprite tile row
                let pattern_addr_low = if sprite_height == 16 {
                    // 8x16 Sprite Mode: Bit 0 of tile_id selects the pattern table bank
                    let base_bank = if (tile_id & 1) != 0 { 0x1000 } else { 0x0000 };
                    let mut actual_tile = tile_id & 0xFE;
                    if sprite_row >= 8 {
                        actual_tile += 1;
                    }
                    base_bank + (actual_tile as u16 * 16) + (sprite_row % 8) as u16
                } else {
                    // 8x8 Sprite Mode: Uses the global sprite pattern table base selection
                    sprite_table_base + (tile_id as u16 * 16) + sprite_row as u16
                };

                // Fetch the 2 bitplanes for this sprite row using the mapper
                let s_low = mapper.ppu_read_ctx(pattern_addr_low, self.is_bg_fetch());
                let s_high = mapper.ppu_read_ctx(pattern_addr_low + 8, self.is_bg_fetch());

                // (index, x_coord, low_byte, high_byte, attributes, is_sprite_zero)
                self.scanline_sprites.push((i, x, s_low, s_high, attr, is_sprite_zero));
            }
        }
    }

    fn evaluate_sprite_overflow(&mut self, target_scanline: i32) {
        let sprite_height = if (self.ctrl & 0x20) != 0 { 16 } else { 8 };
        let mut count = 0;
        for i in 0..64 {
            let sprite_y = self.oam[i * 4] as i32;
            if sprite_y >= 240 { continue; }
            let row = target_scanline - (sprite_y + 1);
            if row >= 0 && row < sprite_height {
                count += 1;
                if count > 8 {
                    self.status |= 0x20;
                    return;
                }
            }
        }
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
                mapper.write_nametable_byte(addr, value, &mut self.vram);
//                let mirrored_addr = mapper.mirror_vram_address(addr);
//                self.vram[(mirrored_addr as usize) & 0x0FFF] = value;
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

    pub fn cpu_read_reg(&mut self, mapper: &mut dyn Mapper, reg: u16, open_bus: u8) -> u8 {
        match reg {
            2 => { // $2002 - PPUSTATUS
                let _is_currently_vblank = self.scanline >= 241 && self.scanline <= 260;
                let res = (self.status & 0xE0) | (open_bus & 0x1F);
//                if self.scanline == 241 && self.cycle < 6 {
//                    godot_print!("status={}. cycle={}", self.status,self.cycle);
//                }
                if self.scanline == 241 && self.cycle == 1 {
                    self.vbl_suppressed = true;
                }
                if self.scanline == 241 && (self.cycle == 2 || self.cycle == 3) {
                    self.nmi_suppressed = true;
                }
                self.status &= 0x7F; // Reading status clears V-Blank bit
                self.w_latch = false;    // And resets scroll/address double-write latch

                if (res & 0x40 != 0) && self.frame_number != self.last_2002 {
//                    godot_print!("[{}]. PPUSTATUS read returned bit 6 set. scanline={}|cycle={}", self.frame_number, self.scanline, self.cycle);
                    self.last_2002 = self.frame_number;
                }
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
                let current_vram_addr = self.v_addr & 0x3FFF;
                let mut data = self.ppu_read(mapper, current_vram_addr, self.is_bg_fetch());

                if current_vram_addr < 0x3F00 {
                    let buffered_data = self.data_buffer;
                    self.data_buffer = data;
                    data = buffered_data;
                } else {
                    self.data_buffer = self.ppu_read(mapper, current_vram_addr - 0x1000, self.is_bg_fetch());
                    data = (data & 0x3F) | (open_bus & 0xC0);
                }
                self.increment_vram_address();
//                self.v_addr = self.v_addr.wrapping_add(self.vram_increment as u16) & 0x3FFF;
                mapper.update_a12(self.v_addr, self.total_ppu_cycles);
                data
            }
            _ => open_bus
        }
    }

    fn increment_vram_address(&mut self) {
        if self.rendering_enabled() && (self.scanline < 240 || self.scanline == 261) {
            self.increment_coarse_x();
            self.increment_vertical_scroll();
        } else {
            // Normal behavior outside rendering
            self.v_addr = self.v_addr.wrapping_add(self.vram_increment as u16) & 0x3FFF;
        }
    }
    pub fn cpu_write_reg(&mut self, mapper: &mut dyn crate::nes::mappers::Mapper, reg: u16, value: u8) {
//        self.decay_timer_upper = 89342;
//        self.decay_timer_lower = 89342;
        if self.scanline >= 0 && self.scanline < 240 && self.cycle > 0 && self.cycle < 256 {
            if reg == 0 || reg == 1 || reg == 5 || reg == 6 {
                self.mid_scanline_write = true;
            }
        }
        match reg {
            0 => { // $2000 - PPUCTRL
                let old_nmi_enabled = (self.ctrl & 0x80) != 0;
                let vblank_flag_active = (self.status & 0x80) != 0;
                //                godot_print!("PPUCTRL write  {value}. scanline={}, cycle={}", self.scanline, self.cycle);
                self.ctrl = value;
                let new_nmi_enabled = (self.ctrl & 0x80) != 0;
/*                if !old_nmi_enabled && new_nmi_enabled && vblank_flag_active {
                    godot_print!("nmi_enabled. Trigger NMI");

                }
                if old_nmi_enabled && !new_nmi_enabled {
                    godot_print!("nmi_disabled");
                } */
                // Extract bits to configure scrolling targets
                self.t_addr = (self.t_addr & 0xF3FF) | (((value & 0x03) as u16) << 10);
                self.vram_increment = if (value & 0x04) == 0x04 { 32 } else { 1 };
                self.background_pattern_table = if (value & 0x10) == 0x10 { 0x1000 } else { 0x0000 };
                self.sprite_pattern_table = if (value & 0x08) == 0x08 { 0x1000 } else { 0x0000 };
                self.sprite_size = if (value & 0x20) == 0x20 { 16 } else { 8 };
            }
            1 => { // $2001 - PPUMASK
//                godot_print!("PPUMASK write  {value}. scanline={}, cycle={}", self.scanline, self.cycle);
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
                    if self.frame_number != self.last_2005 {
  //                      godot_print!("PPUSCROLL first write {value}. scanline={}, cycle={}", self.scanline, self.cycle);
                        self.last_2005 = self.frame_number;
                    }
                    // First write: Coarse X and Fine X scrolling values
                    self.t_addr = (self.t_addr & 0x7FE0) | ((value >> 3) as u16);
                    self.fine_x = value & 0x07;
                    self.w_latch = true;

                } else {
                    if self.frame_number != self.last_2005_2 {
//                        godot_print!("PPUSCROLL second write  {value}. scanline={}, cycle={}", self.scanline, self.cycle);
                        self.last_2005_2 = self.frame_number;
                    }
                    // Second write: Coarse Y and Fine Y scrolling values
                    self.t_addr = (self.t_addr & 0x0C1F) | (((value & 0x07) as u16) << 12) | (((value >> 3) as u16) << 5);
                    self.w_latch = false;
                }
            }
            6 => { // $2006 - PPUADDR
            
                if self.w_latch == false {
                    if self.frame_number != self.last_2006 {
//                        godot_print!("PPUADDR first write  {value}. scanline={}, cycle={}", self.scanline, self.cycle);
                        self.last_2006 = self.frame_number;
                    }
                    // First write: High byte of the 14-bit destination target address
                    self.t_addr = (self.t_addr & 0x00FF) | (((value & 0x3F) as u16) << 8);
                    self.w_latch = true;
                } else {
                    let old_v = self.v_addr;
                    if self.frame_number != self.last_2006_2 {
//                        godot_print!("PPUADDR second write {value}. scanline={}, cycle={}. Total Cycles={ }. V={} T={}", self.scanline, self.cycle, self.total_ppu_cycles, self.v_addr, self.t_addr);
                        self.last_2006_2 = self.frame_number;
                    }
                    // Second write: Low byte of destination target address
                    self.t_addr = (self.t_addr & 0xFF00) | (value as u16);
                    self.v_addr = self.t_addr; // Latch copies address into current VRAM target
                    self.w_latch = false;
//                    mapper.notify_vram_address(v & 0x3FFF, frame_cycle)
                    mapper.update_a12(self.t_addr & 0x3FFF, self.total_ppu_cycles);
                }
            }
            7 => { // $2007 - PPUDATA
                let old_v = self.v_addr & 0x3FFF;
                // Write the value into the destination VRAM address
                self.ppu_write(mapper, old_v, value);
                // Automatically step forward the target address based on $2000 setup configurations
//                self.v_addr = self.v_addr.wrapping_add(self.vram_increment as u16) & 0x3FFF;
//                mapper.notify_vram_address(v & 0x3FFF, frame_cycle)
                self.increment_vram_address();
                mapper.update_a12(self.v_addr, self.total_ppu_cycles);
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