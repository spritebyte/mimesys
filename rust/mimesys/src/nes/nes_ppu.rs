/// Dots between a $2001 write and the PPU's internal rendering-enable switching
/// (1 = takes effect on the dot after the write)
const RENDER_DELAY: u8 = 3;
/// Zapper: scanlines the photodiode keeps reporting light after the beam passed, and the
/// brightness (luma 0-255) it needs
const ZAPPER_LIGHT_LINES: i32 = 20;
const ZAPPER_BRIGHTNESS: u32 = 85;
/// sprite unit state bit in scanline_sprites.0 (low 8 bits = X counter)
const SPR_COUNTING: usize = 0x100;
/// dots from a $2007 read (CPU access point) to the PPU refilling the read buffer
/// dots from the second $2006 write until v takes t (4 = before the 4th dot after it)
const V_UPDATE_DELAY: u8 = 4;
/// dots between finding the 9th sprite and $2002's overflow bit being set
const OVERFLOW_FLAG_DELAY: u8 = 1;
const SM_DELAY: u64 = 5;

use crate::nes::mappers::Mapper;
use std::sync::atomic::{AtomicBool, Ordering};
use serde::{Serialize, Deserialize};
use std::sync::{Arc, Mutex};

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
    pub front_buffer: Arc<Mutex<Vec<u8>>>, // The last fully completed frame, safe for sharing across threads
    
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
    // PPU I/O latch ("decay register"): separate from the CPU data bus. Each bit
    // remembers the dot it was last refreshed; a 1 not refreshed for ~600 ms reads as 0.
    io_latch: u8,
    io_refresh: [u64; 8],
    // Background fetch latches (filled on dots 2/4/6/8 of each tile, loaded into the
    // shifters on the next tile boundary)
    nt_latch: u8,
    at_latch: u8,
    pt_lo_latch: u8,
    pt_hi_latch: u8,
    // Sprite evaluation result for the NEXT line: (oam index, y, tile, attr, x)
    /// Secondary OAM (32 bytes) and the PPU's own pointer into it
    sec_oam: [u8; 32],
    sec_addr: u8,
    /// sprite evaluation state (dots 65-256)
    eval_copying: bool,   // copying bytes 1-3 of an in-range sprite
    eval_first: bool,     // next Y checked is the first one this line ("sprite 0")
    eval_done: bool,      // OAM pointer wrapped: nothing left to check
    sec_full: bool,       // 8 sprites found: now hunting for overflow
    ovf_copy: u8,         // bytes still to "read" after an overflow hit
    sprite0_next: bool,   // slot 0 of the next line is "sprite 0"
    sprite_count: u8,     // sprites found by the last evaluation
    attr_feed: u8,        // attribute bits feeding the attribute shifters' serial input
    render_on: bool,      // rendering enabled, as seen RENDER_DELAY dots after a $2001 write
    oam2_ovf: bool,       // secondary OAM pointer wrapped: increments frozen
    // ---- PPU memory bus: each fetch = ALE dot (low address byte into the octal latch)
    //      + read dot (current high address bits | octal latch) ----
    octal: u8,
    ale_addr: u16,        // address put out on the last ALE dot
    ale_dot: u64,
    render_read: u8,      // value of the last rendering read (what's on the data bus)
    render_read_dot: u64,
    /// $2007 read: the buffer is refilled by a read SM_DELAY dots later (0 = none pending)
    sm_read_at: u64,
    sm_addr: u16,
    v_update_delay: u8,   // $2006 second write: dots until t -> v
    overflow_delay: u8,   // sprite overflow found: dots until $2002 bit 5 is set
    /// debug: NES_HIDE=bg / spr / bg,spr hides a layer on screen (bit 0 bg, bit 1 sprites);
    /// sprite 0 hits and everything else still happen
    debug_hide: u8,
    /// debug: NES_TRACE_V=1 logs, per visible line, v / fine X at dot 1 and the tile
    /// numbers fetched (drained into the event trace by the bus)
    debug_v: bool,
    debug_tiles: Vec<u8>,
    debug_v_start: (u16, u8, bool),
    pub debug_lines: Vec<String>,
    fetch_y: u8, fetch_tile: u8, fetch_attr: u8, fetch_x: u8,   // sprite fetch latches
    render_delay: u8,
    oam_buffer: u8,       // what a $2004 read returns during rendering
    /// rendering was turned off mid-line: OAM row 0 gets copied over this row at the
    /// next rendered dot (OAM corruption)
    oam_corrupt_row: Option<u8>,
    // Sprites fetched on dots 257-320, shown on the next line
    next_sprites: Vec<(usize, u8, u8, u8, u8, bool)>,
    overflow_at_dot: u32,
    a12_line: bool,
}

/// ~600 ms of PPU dots (5.369318 MHz)
const IO_DECAY_DOTS: u64 = 3_220_000;

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
            scanline_bg: [(0, 0, 0); 33], scanline_sprites: vec![(0, 0xFF, 0, 0, 0, false); 8],
            back_buffer: vec![0; buffer_size],
            front_buffer: Arc::new(Mutex::new(vec![0; buffer_size])),
            system_frame_ready,
            total_ppu_cycles:0, frame_number: 0, last_2002: 0, last_2005: 0, last_2006: 0,
            last_2005_2: 0, last_2006_2: 0,
            sprite_0_hit: false, debug_trace_bg: false,
            a12_sprite_tiles: [None; 8], sprite_0_delay: 0,
            io_latch: 0, io_refresh: [0; 8],
            nt_latch: 0, at_latch: 0, pt_lo_latch: 0, pt_hi_latch: 0,
            sec_oam: [0xFF; 32], sec_addr: 0, eval_copying: false, eval_first: true,
            eval_done: false, sec_full: false, ovf_copy: 0, sprite0_next: false,
            sprite_count: 0, attr_feed: 0, render_on: false, render_delay: 0, oam2_ovf: false,
            octal: 0, ale_addr: 0, ale_dot: u64::MAX, render_read: 0, render_read_dot: u64::MAX,
            sm_read_at: 0, sm_addr: 0, v_update_delay: 0, overflow_delay: 0,
            debug_v: std::env::var_os("NES_TRACE_V").is_some(),
            debug_tiles: Vec::new(), debug_v_start: (0, 0, false), debug_lines: Vec::new(),
            debug_hide: std::env::var("NES_HIDE").map(|v| {
                let v = v.to_ascii_lowercase();
                (v.contains("bg") as u8) | ((v.contains("spr") as u8) << 1)
            }).unwrap_or(0),
            fetch_y: 0xFF, fetch_tile: 0xFF, fetch_attr: 0xFF, fetch_x: 0xFF, oam_buffer: 0xFF, oam_corrupt_row: None,
            next_sprites: Vec::with_capacity(8),
            overflow_at_dot: 0, a12_line: false,
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
        self.render_on = (state.mask & 0x18) != 0;
        self.render_delay = 0;
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
        self.scanline_sprites.resize(8, (0, 0xFF, 0, 0, 0, false));
        self.a12_sprite_tiles = state.a12_sprite_tiles.clone();
        self.background_pattern_table = state.background_pattern_table;
        self.oam.copy_from_slice(&state.oam);
        self.vram.copy_from_slice(&state.vram);
        self.palette_ram.copy_from_slice(&state.palette_ram);
        self.base_nametable_address = 0x2000 + ((self.ctrl & 0x03) as u16) * 0x400;
        self.vram_increment = if self.ctrl & 0x04 != 0 { 32 } else { 1 };
        self.sprite_pattern_table = if self.ctrl & 0x08 != 0 { 0x1000 } else { 0x0000 };
        self.background_pattern_table = if self.ctrl & 0x10 != 0 { 0x1000 } else { 0x0000 };
        self.sprite_size = if self.ctrl & 0x20 != 0 { 16 } else { 8 };
        self.sprite_0_delay = 0;
        self.frame_ready = false;
    }

    pub fn reset(&mut self) {
        self.ctrl = 0; self.mask = 0; self.status = 0; self.render_on = false; self.render_delay = 0;
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
    }

    pub fn is_frame_complete(&self) -> bool {
        self.frame_ready
    }

    pub fn clear_frame_complete_flag(&mut self) {
        self.frame_ready = false;
    }

    pub fn total_cycles(&self) -> u64 { return self.total_ppu_cycles; }

    pub fn step_one_cycle(&mut self, mapper: &mut dyn Mapper) {
        self.tick_render_delay();
        if self.v_update_delay > 0 {
            self.v_update_delay -= 1;
            if self.v_update_delay == 0 {
                self.v_addr = self.t_addr;
                self.drive_a12_from_v(mapper, self.v_addr & 0x3FFF);
            }
        }
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
                if self.overflow_delay > 0 {
                    self.overflow_delay -= 1;
                    if self.overflow_delay == 0 {
                        self.status |= 0x20;
                    }
                }
                if self.debug_v && self.scanline < 240 {
                    if self.cycle == 1 {
                        self.debug_v_start = (self.v_addr, self.fine_x, self.rendering_enabled());
                        self.debug_tiles.clear();
                    } else if self.cycle == 257 {
                        let (v, fx, on) = self.debug_v_start;
                        let tiles: Vec<String> = self.debug_tiles.iter().map(|t| format!("{t:02X}")).collect();
                        self.debug_lines.push(format!(
                            "v@1={v:04X} (NT{} coarseY {} fineY {} coarseX {}) fx={fx} {} tiles: {}",
                            (v >> 10) & 3, (v >> 5) & 31, (v >> 12) & 7, v & 31,
                            if on { "on" } else { "off" }, tiles.join(" ")));
                    }
                }
                // 1. Visible Screen Rendering (Skip during prefetch line)
                if !is_prefetch && (1..=256).contains(&self.cycle) {
                    self.render_pixel(mapper, (self.cycle - 1) as usize);
                } else if is_prefetch && (1..=256).contains(&self.cycle) {
                    let _ = self.step_sprite_units();    // the units run, nothing is output
                }

                if is_prefetch && self.cycle == 1 {
                    self.status &= 0x1F;
                    self.nmi_suppressed = false;
                }

                if self.cycle == 339 {
                    // the sprite X counters are armed here; with rendering off at this dot
                    // they stay at 0, so every sprite starts as soon as rendering resumes
                    self.load_sprite_counters(rendering_enabled);
                }
                if rendering_enabled {
                    if let Some(row) = self.oam_corrupt_row.take() {
                        // OAM corruption: row 0 is copied over the row the secondary OAM
                        // pointer was at when rendering was switched off
                        let r = row as usize * 8;
                        if r != 0 {
                            let src: [u8; 8] = self.oam[0..8].try_into().unwrap();
                            self.oam[r..r + 8].copy_from_slice(&src);
                        }
                        self.sec_oam[row as usize & 0x1F] = self.sec_oam[0];
                    }
                    self.render_line_dot(mapper, is_prefetch);
                }
            }

            // ---- POST-RENDER / IDLE SCANLINE ----
            240 => {
                if self.cycle == 0 {
                    // End of frame: swap buffers safely
//                    let completed_buffer = std::mem::replace(&mut self.back_buffer, vec![0; 256 * 240 * 4]);
//                    self.front_buffer = Arc::new(completed_buffer);
                    if let Ok(mut front) = self.front_buffer.lock() {
                        std::mem::swap(&mut self.back_buffer, &mut *front);
                    }
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

        if self.sm_read_at != 0 && self.total_ppu_cycles >= self.sm_read_at {
            self.finish_2007_read(mapper);
        }

        // 5. Always advance the cycle counters at the very end
        self.advance_cycle(mapper);
    }

    // Helper method to keep stepping neat and eliminate the double cycle bug
    #[inline(always)]
    fn advance_cycle(&mut self, mapper: &mut dyn Mapper) {
        self.cycle += 1;
        self.total_ppu_cycles += 1;

        // the odd-frame skip follows $2001 without the rendering pipeline's lag
        // (blargg ppu_vbl_nmi 10-even_odd_timing)
        let rendering_enabled = (self.mask & 0x18) != 0;
        let should_skip = rendering_enabled && self.scanline == 261 && self.cycle == 339 && self.is_odd_frame;

        if should_skip {
            self.cycle = 340; // Artificially skip directly to cycle 340, which wraps next
            self.load_sprite_counters(true);
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

    /// One dot of a rendering line (0-239, and the pre-render line 261) with rendering on.
    /// Fetch timing (each fetch takes 2 dots; the data is used on the second):
    ///   dots 1-256, 321-336: per 8-dot tile: NT (2), AT (4), pattern low (6), high (8),
    ///                        coarse X++ on dot 8; shifters reload on dots 9, 17, ... 257
    ///                        (and 329/337 for the two tiles of the next line)
    ///   dot 256: fine/coarse Y++   dot 257: horizontal bits t -> v
    ///   dots 65-256: sprite evaluation for the next line (done at dot 65, overflow flag
    ///                set on the dot the hardware would find it)
    ///   dots 257-320: 8 sprite slots: garbage NT (2), garbage NT (4), pattern low (6), high (8)
    ///   dots 338, 340: dummy NT fetches
    /// Every fetch address goes to mapper.update_a12, so MMC3-style counters see the real
    /// A12 pattern.
    fn render_line_dot(&mut self, mapper: &mut dyn Mapper, is_prefetch: bool) {
        let dot = self.cycle;
        match dot {
            1..=256 | 321..=336 => {
                self.bg_fetch_step(mapper, dot);
                if dot == 256 {
                    self.increment_vertical_scroll();
                }
                if dot == 63 || dot == 255 {
                    self.oam2_ovf = false;
                }
                if dot >= 321 {
                    // $2004 sees secondary OAM at the pointer (index 0 normally)
                    self.oam_buffer = self.sec_oam[self.sec_addr as usize & 0x1F];
                }
                if dot <= 256 && !is_prefetch {
                    self.sprite_eval_dot(dot);
                }
                // no evaluation on the pre-render line: the stale secondary OAM (and its
                // sprite-0 flag) is fetched as-is, range-checked against line 261 & 255 = 5
            }
            257..=320 => {
                if dot == 257 && !self.oam2_ovf {
                    self.sec_addr = 0;
                }
                if is_prefetch && (280..=304).contains(&dot) {
                    self.copy_vertical();
                }
                if dot == 260 {
                    mapper.notify_scanline();
                }
                self.oam_addr = 0;
                self.sprite_fetch_step(mapper, dot);
                if dot == 257 {
                    // after this dot's NT address went out (from the old v)
                    self.copy_horizontal();
                }
            }
            337..=340 => {
                self.oam_buffer = self.sec_oam[self.sec_addr as usize & 0x1F];
                if dot == 339 {
                    self.oam2_ovf = false;
                }
                let nt = 0x2000 | (self.v_addr & 0x0FFF);
                if dot % 2 == 1 {
                    self.bus_ale(mapper, nt);
                } else {
                    let b = mapper.read_nametable_byte(self.bus_addr(nt), &self.vram, false);   // dummy fetches
                    self.bus_record(b);
                }
            }
            _ => {}
        }
        // Shift after this dot's pixel was drawn. The two tiles fetched on 321-336 end up
        // in the high byte / low byte, so the first is on screen at dot 1.
        if (1..=256).contains(&dot) || (321..=336).contains(&dot) {
            self.shift_bg();
            // the tile fetched over the last 8 dots goes into the shifters at the end of
            // the 8th dot (8, 16, ... 256 and 328, 336), after that dot's shift
            if dot % 8 == 0 {
                self.reload_shifters_low();
            }
        }
    }

    /// Put a PPU address on the bus. Mappers only see A12 *changes* (MMC3/RAMBO-1 act
    /// on edges), which saves a virtual call on most fetches.
    #[inline]
    fn drive_a12(&mut self, mapper: &mut dyn Mapper, addr: u16, cycle: u64) {
        let a12 = addr & 0x1000 != 0;
        if a12 != self.a12_line {
            self.a12_line = a12;
            mapper.update_a12(addr, cycle);
        }
    }

    /// $2006/$2007 put v on the PPU address bus, but only while the PPU is idle. During
    /// rendering the bus carries the rendering fetches, so the mapper never sees v there
    /// (a $2007 read mid-frame in Burai Fighter would otherwise clock MMC3 an extra time).
    fn drive_a12_from_v(&mut self, mapper: &mut dyn Mapper, addr: u16) {
        let rendering_line = self.scanline < 240 || self.scanline == 261;
        if !(self.rendering_enabled() && rendering_line) {
            self.drive_a12(mapper, addr, self.total_ppu_cycles);
        }
    }

    fn reload_shifters_low(&mut self) {
        self.bg_shift_low = (self.bg_shift_low & 0xFF00) | self.pt_lo_latch as u16;
        self.bg_shift_high = (self.bg_shift_high & 0xFF00) | self.pt_hi_latch as u16;
        let a0 = if self.at_latch & 1 != 0 { 0xFF } else { 0x00 };
        let a1 = if self.at_latch & 2 != 0 { 0xFF } else { 0x00 };
        self.attr_shift_low = (self.attr_shift_low & 0xFF00) | a0;
        self.attr_shift_high = (self.attr_shift_high & 0xFF00) | a1;
        self.attr_feed = self.at_latch;
    }

    /// The high pattern shifter shifts in 1s (the low one 0s); the attribute shifters shift in the 2-bit latch
    /// loaded at the last reload. Only visible when a reload is skipped (rendering
    /// toggled at the right moment) - AccuracyCoin "BG Serial In".
    fn shift_bg(&mut self) {
        self.bg_shift_low <<= 1;                            // low plane shifts in 0
        self.bg_shift_high = (self.bg_shift_high << 1) | 1; // high plane shifts in 1
        self.attr_shift_low = (self.attr_shift_low << 1) | (self.attr_feed & 1) as u16;
        self.attr_shift_high = (self.attr_shift_high << 1) | ((self.attr_feed >> 1) & 1) as u16;
    }

    /// ALE dot: the address goes out, its low byte into the octal latch.
    #[inline]
    fn bus_ale(&mut self, mapper: &mut dyn Mapper, addr: u16) {
        self.octal = addr as u8;
        self.ale_addr = addr;
        self.ale_dot = self.total_ppu_cycles;
        self.drive_a12(mapper, addr, self.total_ppu_cycles);
    }

    /// Read dot: the high address bits as they are now, the low byte from the octal
    /// latch (a $2006 write between the two dots gives a "hybrid" address).
    #[inline]
    fn bus_addr(&self, addr: u16) -> u16 {
        (addr & 0x3F00) | self.octal as u16
    }

    #[inline]
    fn bus_record(&mut self, value: u8) -> u8 {
        self.render_read = value;
        self.render_read_dot = self.total_ppu_cycles;
        value
    }

    fn bg_fetch_step(&mut self, mapper: &mut dyn Mapper, dot: u32) {
        // odd dot: address goes out (A12 changes here); even dot: data comes back
        let v = self.v_addr;
        let nt_addr = 0x2000 | (v & 0x0FFF);
        let at_addr = 0x23C0 | (v & 0x0C00) | ((v >> 4) & 0x38) | ((v >> 2) & 0x07);
        match dot % 8 {
            1 => self.bus_ale(mapper, nt_addr),
            2 => {
                let b = mapper.read_nametable_byte(self.bus_addr(nt_addr), &self.vram, false);
                self.nt_latch = self.bus_record(b);
                if self.debug_v { self.debug_tiles.push(b); }
            }
            3 => self.bus_ale(mapper, at_addr),
            4 => {
                let byte = mapper.read_nametable_byte(self.bus_addr(at_addr), &self.vram, true);
                self.bus_record(byte);
                let shift = (((v >> 5) & 0x02) << 1) | (v & 0x02);   // (coarse_y bit1)*4 + (coarse_x bit1)*2
                self.at_latch = (byte >> shift) & 0x03;
            }
            5 => { let a = self.bg_pattern_addr(); self.bus_ale(mapper, a) }
            6 => {
                let b = mapper.ppu_read_ctx(self.bus_addr(self.bg_pattern_addr()), true);
                self.pt_lo_latch = self.bus_record(b);
            }
            7 => { let a = self.bg_pattern_addr() + 8; self.bus_ale(mapper, a) }
            _ => {
                let b = mapper.ppu_read_ctx(self.bus_addr(self.bg_pattern_addr() + 8), true);
                self.pt_hi_latch = self.bus_record(b);
                self.increment_coarse_x();
            }
        }
    }

    #[inline]
    fn bg_pattern_addr(&self) -> u16 {
        self.background_pattern_table + ((self.nt_latch as u16) << 4) + ((self.v_addr >> 12) & 7)
    }

    /// One dot of sprite evaluation (lines 0-239; the pre-render line keeps the stale
    /// secondary OAM). OAMADDR itself is the pointer into OAM, so a misaligned OAMADDR
    /// stays misaligned the way the hardware does:
    ///   dots 1-64   clear secondary OAM to $FF (pointer = (dot-1)/2), $2004 reads $FF
    ///   dots 65-256 odd: read OAM[OAMADDR]; even: act on it
    ///     - Y byte: copy; in range -> copy the next 3 bytes too; out of range ->
    ///       OAMADDR = (OAMADDR + 4) & $FC
    ///     - after the 4th byte: +1, and realign (& $FC) if that byte isn't "in range"
    ///     - with 8 found: in range -> overflow flag; out of range -> +5 (the hardware bug)
    ///   the first sprite looked at becomes "sprite 0" for the hit test
    fn sprite_eval_dot(&mut self, dot: u32) {
        if dot <= 64 {
            self.sec_addr = ((dot - 1) / 2) as u8;
            if dot % 2 == 0 {
                self.sec_oam[self.sec_addr as usize] = 0xFF;
            }
            self.oam_buffer = 0xFF;
            return;
        }
        if dot == 65 {
            self.sec_addr = 0;
            self.eval_copying = false;
            self.eval_first = true;
            self.eval_done = false;
            self.sec_full = false;
            self.ovf_copy = 0;
            self.sprite0_next = false;
        }
        if dot % 2 == 1 {
            if self.sec_full && self.ovf_copy == 1 {
                // last step of the overflow sprite's copy: no OAM read this dot
                self.oam_buffer = self.sec_oam[self.sec_addr as usize & 0x1F];
                return;
            }
            let mut b = self.oam[self.oam_addr as usize];
            if self.oam_addr & 3 == 2 {
                b &= 0xE3;      // attribute bytes have no bits 2-4
            }
            self.oam_buffer = b;
            return;
        }
        let h: i32 = if (self.ctrl & 0x20) != 0 { 16 } else { 8 };
        let line = self.scanline as i32;
        let byte = self.oam_buffer;
        let in_range = { let r = line - byte as i32; r >= 0 && r < h };

        if self.eval_done {
            // OAM keeps being read on odd dots; even dots read secondary OAM
            self.oam_buffer = self.sec_oam[self.sec_addr as usize & 0x1F];
            self.oam_addr = self.oam_addr.wrapping_add(4);
        } else if !self.sec_full {
            self.sec_oam[self.sec_addr as usize] = byte;
            if !self.eval_copying {
                let first = std::mem::replace(&mut self.eval_first, false);
                if in_range {
                    self.eval_copying = true;
                    self.sprite0_next |= first;
                    self.sec_addr += 1;
                    self.oam_addr = self.oam_addr.wrapping_add(1);
                } else {
                    let (next, wrapped) = (self.oam_addr & 0xFC).overflowing_add(4);
                    self.oam_addr = next;
                    self.eval_done = wrapped;
                }
            } else {
                let fourth = self.sec_addr & 3 == 3;
                self.sec_addr += 1;
                let (mut next, wrapped) = self.oam_addr.overflowing_add(1);
                if fourth {
                    if !in_range { next &= 0xFC; }
                    self.eval_copying = false;
                    if self.sec_addr >= 32 {
                        self.sec_addr = 0;
                        self.sec_full = true;
                        self.oam2_ovf = true;
                    }
                }
                self.oam_addr = next;
                self.eval_done = wrapped;
            }
        } else {
            // 8 sprites found: writes turn into reads, look for a 9th (with the m++ bug)
            self.oam_buffer = self.sec_oam[self.sec_addr as usize & 0x1F];
            if self.ovf_copy > 0 {
                // after the overflow sprite: two more bytes, an idle read, then on to the
                // next whole sprite; evaluation then keeps stepping +4 (wrapping) to dot 256
                self.ovf_copy -= 1;
                if self.ovf_copy == 0 {
                    self.oam_addr = (self.oam_addr & 0xFC).wrapping_add(4);
                    self.eval_done = true;
                } else {
                    self.oam_addr = self.oam_addr.wrapping_add(1);
                }
            } else if in_range {
                self.overflow_delay = OVERFLOW_FLAG_DELAY;   // $2002 bit 5 shows up a dot later
                self.ovf_copy = 4;
                self.oam_addr = self.oam_addr.wrapping_add(1);
            } else {
                let hi = (self.oam_addr >> 2) as u16 + 1;
                let lo = (self.oam_addr + 1) & 3;
                self.oam_addr = (((hi << 2) as u8) & 0xFC) | lo;
                self.eval_done = hi > 63;
            }
        }
        if dot == 256 {
            self.sprite_count = if self.sec_full { 8 } else { self.sec_addr >> 2 };
        }
    }

    /// Dots 257-320: slot = (dot-257)/8, read from secondary OAM. Pattern low on the 6th
    /// dot, high on the 8th. Empty slots fetch tile $FF (what cleared secondary OAM holds),
    /// which matters for A12; only the first `sprite_count` slots are drawn.
    fn sprite_fetch_step(&mut self, mapper: &mut dyn Mapper, dot: u32) {
        let slot = ((dot - 257) / 8) as usize;
        let phase = (dot - 257) % 8;
        // everything comes through the secondary OAM pointer (so a pointer left
        // misaligned or frozen by rendering toggles shows up here)
        self.oam_buffer = self.sec_oam[self.sec_addr as usize & 0x1F];
        match phase {
            0 => self.fetch_y = self.oam_buffer,
            1 => self.fetch_tile = self.oam_buffer,
            2 => self.fetch_attr = self.oam_buffer,
            3 => self.fetch_x = self.oam_buffer,
            _ => {}
        }
        match phase {
            0 | 2 => {
                // two garbage nametable fetches (A12 low)
                let addr = 0x2000 | (self.v_addr & 0x0FFF);
                self.bus_ale(mapper, addr);
            }
            1 | 3 => {
                let addr = self.bus_addr(0x2000 | (self.v_addr & 0x0FFF));
                let b = mapper.read_nametable_byte(addr, &self.vram, false);
                self.bus_record(b);
            }
            4 | 6 => {
                let addr = self.sprite_pattern_addr();
                self.bus_ale(mapper, if phase == 6 { addr + 8 } else { addr });
            }
            5 | 7 => {
                let addr = self.sprite_pattern_addr();
                let addr = self.bus_addr(if phase == 7 { addr + 8 } else { addr });
                let byte = mapper.ppu_read_ctx(addr, false);
                self.bus_record(byte);
                // the fetch loads the sprite unit directly (if rendering is off during the
                // fetch, the unit keeps its stale contents). The Y byte is range-checked
                // against the line counter (261 & 255 = 5 on the pre-render line); out of
                // range (e.g. empty $FF slots) loads transparent
                let attr = self.fetch_attr;
                let visible = self.fetch_in_range();
                let mut byte = if visible { byte } else { 0 };
                if attr & 0x40 != 0 { byte = byte.reverse_bits(); }   // flip: shifter outputs bit 7
                let x = self.fetch_x;
                let zero = slot == 0 && self.sprite0_next && visible;
                let unit = &mut self.scanline_sprites[slot];
                if phase == 5 {
                    unit.1 = x;
                    unit.0 = (unit.0 & SPR_COUNTING) | x as usize;   // counter = X
                    unit.2 = byte;
                    unit.4 = attr;
                    unit.5 = zero;
                } else {
                    unit.3 = byte;
                }
            }
            _ => {}
        }
        // pointer +1 after the Y, tile and attribute bytes, then holds on X until the
        // slot's last dot
        if matches!(phase, 0..=2 | 7) {
            self.inc_sec_addr();
        }
    }

    /// Secondary OAM pointer increment. Wrapping past 31 raises the "OAM2 overflowed"
    /// flag, which freezes the pointer until rendering is on at dot 63, 255 or 339.
    fn inc_sec_addr(&mut self) {
        if self.oam2_ovf { return; }
        self.sec_addr = (self.sec_addr + 1) & 0x1F;
        if self.sec_addr == 0 {
            self.oam2_ovf = true;
        }
    }

    fn fetch_in_range(&self) -> bool {
        let h: i32 = if (self.ctrl & 0x20) != 0 { 16 } else { 8 };
        let r = (self.scanline as u8) as i32 - self.fetch_y as i32;
        r >= 0 && r < h
    }

    fn sprite_pattern_addr(&self) -> u16 {
        let tall = (self.ctrl & 0x20) != 0;
        let (y, tile, attr) = (self.fetch_y, self.fetch_tile, self.fetch_attr);
        let h: u8 = if tall { 16 } else { 8 };
        // the pre-render line's counter reads as 261 & 255 = 5
        let line = self.scanline as u8;
        let mut row = line.wrapping_sub(y) & (h - 1);
        if attr & 0x80 != 0 { row = h - 1 - row; }
        if tall {
            let bank = if tile & 1 != 0 { 0x1000 } else { 0x0000 };
            let t = (tile & 0xFE) as u16 + if row >= 8 { 1 } else { 0 };
            bank + t * 16 + (row as u16 & 7)
        } else {
            self.sprite_pattern_table + tile as u16 * 16 + row as u16
        }
    }

    /// The Zapper's photodiode: it sees light when the pixel it points at was drawn
    /// bright within the last ZAPPER_LIGHT_LINES scanlines (the sensor decays after that).
    pub fn zapper_sees_light(&self, x: i32, y: i32) -> bool {
        if !(0..256).contains(&x) || !(0..240).contains(&y) {
            return false;
        }
        let (line, dot) = (self.scanline as i32, self.cycle as i32);
        if line < y || line - y > ZAPPER_LIGHT_LINES || (line == y && dot <= x + 1) {
            return false;
        }
        let i = (y as usize * 256 + x as usize) * 4;
        let luma = |buf: &[u8]| -> u32 {
            let (r, g, b) = (buf[i] as u32, buf[i + 1] as u32, buf[i + 2] as u32);
            (r * 299 + g * 587 + b * 114) / 1000
        };
        // the frame being drawn; from line 240 on it has been handed to the front buffer
        let l = if line < 240 {
            luma(&self.back_buffer)
        } else {
            match self.front_buffer.lock() { Ok(f) => luma(&f), Err(_) => 0 }
        };
        l >= ZAPPER_BRIGHTNESS
    }

    /// Dot 339 (or 340 when the odd-frame skip jumps over 339): with rendering on, every
    /// sprite unit switches to "counting". With it off they stay as they were - normally
    /// "drawing", so they draw as soon as rendering resumes.
    fn load_sprite_counters(&mut self, rendering: bool) {
        if rendering {
            for unit in self.scanline_sprites.iter_mut() {
                unit.0 |= SPR_COUNTING;
            }
        }
    }

    /// One visible dot of the 8 sprite units. Counting units decrement every dot (even
    /// with rendering off) and switch to drawing at 0; drawing units shift out a pixel
    /// only while rendering is on. Returns the first opaque pixel:
    /// (pixel, palette, priority, is sprite 0).
    fn step_sprite_units(&mut self) -> Option<(u8, u8, u8, bool)> {
        let rendering = self.rendering_enabled();
        let mut out = None;
        for unit in self.scanline_sprites.iter_mut() {
            if unit.0 & SPR_COUNTING != 0 {
                if unit.0 & 0xFF != 0 {
                    unit.0 -= 1;
                    continue;
                }
                unit.0 &= !SPR_COUNTING;
            }
            if !rendering { continue; }
            let p = ((unit.3 >> 7) << 1) | (unit.2 >> 7);
            unit.2 <<= 1;
            unit.3 <<= 1;
            if p != 0 && out.is_none() {
                out = Some((p, unit.4 & 0x03, (unit.4 >> 5) & 1, unit.5));
            }
        }
        out
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

    /// Rendering on/off as the PPU's internals see it: a $2001 write reaches it
    /// RENDER_DELAY dots later.
    fn rendering_enabled(&self) -> bool {
        self.render_on
    }

    /// Advance the $2001 rendering-enable pipeline by one dot.
    fn tick_render_delay(&mut self) {
        if self.render_delay == 0 { return; }
        self.render_delay -= 1;
        if self.render_delay == 0 {
            self.apply_render_on();
        }
    }

    fn apply_render_on(&mut self) {
        let was_on = self.render_on;
        self.render_on = (self.mask & 0x18) != 0;
        if was_on && !self.render_on && (self.scanline < 240 || self.scanline == 261) {
            // seed for OAM corruption: the secondary OAM pointer (rounded up to a
            // whole sprite during evaluation; 0 once secondary OAM is full)
            let row = match self.cycle {
                65..=256 if self.sec_full => 0,
                65..=256 => (self.sec_addr + 3) & 0x1C,
                _ => self.sec_addr,
            };
            self.oam_corrupt_row = Some(row & 0x1F);
        }
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
    }

    fn copy_horizontal(&mut self) {
        // Copy coarse X (bits 0-4) and horizontal nametable (bit 10)
        // Mask: 0x041F
        let old_v_addr = self.v_addr;
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

        if let Some((p, pal, prio, zero)) = self.step_sprite_units() {
            if spr_show {
                sprite_pixel = p;
                sprite_palette_idx = pal;
                sprite_priority = prio;
                is_sprite_zero = zero;
            }
        }

        // ---- 3. MULTIPLEXER / PRIORITY & SPRITE 0 HIT ----
        let bg_opaque = bg_pixel != 0;
        let spr_opaque = sprite_pixel != 0;

        if is_sprite_zero && bg_opaque && spr_opaque && bg_show && spr_show {
            // Sprite 0 cannot hit at x=255 or if either side is clipped in the left 8 pixels
            if x < 255 {
                if self.status & 0x40 == 0 {
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

        // debug layer hiding (output only)
        let final_palette_offset = if self.debug_hide != 0 {
            let spr = spr_opaque && self.debug_hide & 2 == 0;
            let bg = bg_opaque && bg_show && self.debug_hide & 1 == 0;
            if spr && (!bg || sprite_priority == 0) && bg_show | spr_show {
                0x10 + (sprite_palette_idx as usize * 4) + sprite_pixel as usize
            } else if bg {
                (bg_palette_idx as usize * 4) + bg_pixel as usize
            } else { 0 }
        } else { final_palette_offset };

        // With rendering off the PPU outputs the backdrop - or, if v points into palette
        // RAM, the colour at v (games use this to show palette writes mid-screen)
        let mut final_palette_offset = final_palette_offset;
        if !self.rendering_enabled() {
            let v = self.v_addr & 0x3FFF;
            final_palette_offset = if v >= 0x3F00 {
                let mut i = (v & 0x1F) as usize;
                if i >= 0x10 && i % 4 == 0 { i -= 0x10; }
                i
            } else {
                0
            };
        }

        // ---- 4. WRITE PIXEL TO BACK BUFFER ----
        let color_idx = self.palette_ram[final_palette_offset] & 0x3F;
        let (r, g, b) = NES_PALETTE[color_idx as usize];
        let pixel_index = (self.scanline as usize * 256 + x) * 4;
        self.back_buffer[pixel_index] = r;
        self.back_buffer[pixel_index + 1] = g;
        self.back_buffer[pixel_index + 2] = b;
        self.back_buffer[pixel_index + 3] = 255;
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
    pub fn get_front_buffer_ref(&self) -> Arc<Mutex<Vec<u8>>> {
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

    /// The I/O latch as it reads now, with bits that weren't refreshed in time decayed to 0.
    fn io_latch_now(&mut self) -> u8 {
        for b in 0..8 {
            if self.io_latch & (1 << b) != 0
                && self.total_ppu_cycles.wrapping_sub(self.io_refresh[b]) > IO_DECAY_DOTS {
                self.io_latch &= !(1 << b);
            }
        }
        self.io_latch
    }

    /// Drive `mask` bits of the latch with `value` (a write, or bits a read defines).
    fn io_refresh(&mut self, value: u8, mask: u8) {
        self.io_latch = (self.io_latch & !mask) | (value & mask);
        for b in 0..8 {
            if mask & (1 << b) != 0 { self.io_refresh[b] = self.total_ppu_cycles; }
        }
    }

    /// CPU read of $2000-$3FFF. Undefined bits come from the PPU's own I/O latch, not the
    /// CPU data bus; the bits the register does define refresh the latch.
    pub fn cpu_read_reg(&mut self, mapper: &mut dyn Mapper, reg: u16, _cpu_open_bus: u8) -> u8 {
        let latch = self.io_latch_now();
        let (value, defined) = self.read_reg_raw(mapper, reg, latch);
        self.io_refresh(value, defined);
        (value & defined) | (latch & !defined)
    }

    /// (value, mask of the bits this register drives)
    fn read_reg_raw(&mut self, mapper: &mut dyn Mapper, reg: u16, open_bus: u8) -> (u8, u8) {
        let defined = match reg {
            2 => 0xE0,
            4 => 0xFF,
            7 if (self.v_addr & 0x3FFF) >= 0x3F00 => 0x3F,
            7 => 0xFF,
            _ => 0x00,
        };
        let v = match reg {
            2 => { // $2002 - PPUSTATUS
                let _is_currently_vblank = self.scanline >= 241 && self.scanline <= 260;
                let mut res = (self.status & 0xE0) | (open_bus & 0x1F);
                // (the sprite-0/overflow bits are re-sampled ~2 dots later by NesBus::read_byte)

                if self.scanline == 241 && self.cycle == 1 {
                    self.vbl_suppressed = true;
                }
                if self.scanline == 241 && (self.cycle == 2 || self.cycle == 3) {
                    self.nmi_suppressed = true;
                }
                self.status &= 0x7F; // Reading status clears V-Blank bit
                self.w_latch = false;    // And resets scroll/address double-write latch

                if (res & 0x40 != 0) && self.frame_number != self.last_2002 {
                    self.last_2002 = self.frame_number;
                }
                res
            }
            4 => {
                if self.rendering_enabled() && (self.scanline < 240 || self.scanline == 261) {
                    // during rendering $2004 shows the sprite pipeline's internal latch
                    return (self.oam_buffer, 0xFF);
                }
                let mut value = self.oam[self.oam_addr as usize];
                if (self.oam_addr & 0x03) == 2 {
                    value &= 0xE3; // Keeps bits 7, 6, 5, 1, 0
                }
                value
            }
            7 => { // $2007 - PPUDATA
                let current_vram_addr = self.v_addr & 0x3FFF;
                // the CPU gets the buffer; the PPU refills it SM_DELAY dots later (see
                // finish_2007_read). Palette reads come straight from palette RAM.
                if self.sm_read_at != 0 {
                    self.finish_2007_read(mapper);    // back-to-back reads: finish the last one
                }
                self.sm_read_at = self.total_ppu_cycles + SM_DELAY;
                self.sm_addr = current_vram_addr;
                let mut data = self.data_buffer;
                if current_vram_addr >= 0x3F00 {
                    data = self.ppu_read(mapper, current_vram_addr, false);
                    if self.mask & 0x01 != 0 {
                        data &= 0x30;       // greyscale applies to palette reads too
                    }
                    data = (data & 0x3F) | (open_bus & 0xC0);
                }
                data
            }
            _ => open_bus
        };
        (v, defined)
    }

    /// The read half of the $2007 state machine, then v's increment. While rendering, the PPU is already
    /// reading something on every other dot: if that's this dot, the buffer gets the
    /// rendering read's value. If this dot is a rendering ALE dot, ALE and Read collide:
    /// the octal latch keeps the data bus value and the read goes to (ALE high bits |
    /// latch), which the next rendering read then uses as well (AccuracyCoin "ALE + Read").
    /// Otherwise it reads v (palette addresses read the nametable underneath).
    fn finish_2007_read(&mut self, mapper: &mut dyn Mapper) {
        self.sm_read_at = 0;
        let rendering = self.rendering_enabled() && (self.scanline < 240 || self.scanline == 261);
        let now = self.total_ppu_cycles;
        if rendering && self.render_read_dot == now {
            self.data_buffer = self.render_read;
        } else if rendering && self.ale_dot == now {
            self.octal = self.render_read;
            let addr = self.bus_addr(self.ale_addr);
            let b = self.ppu_read(mapper, addr & 0x3FFF, self.is_bg_fetch());
            self.data_buffer = self.bus_record(b);
        } else {
            let mut addr = self.sm_addr;
            if addr >= 0x3F00 { addr -= 0x1000; }
            self.data_buffer = self.ppu_read(mapper, addr, self.is_bg_fetch());
        }
        // v increments at the end of the state machine (during rendering: the coarse X / Y
        // glitch increments)
        self.increment_vram_address();
        self.drive_a12_from_v(mapper, self.v_addr);
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
        self.io_refresh(value, 0xFF);   // any PPU register write sets the whole latch
        if self.scanline < 240 && self.cycle > 0 && self.cycle < 256 {
            if reg == 0 || reg == 1 || reg == 5 || reg == 6 {
                self.mid_scanline_write = true;
            }
        }
        match reg {
            0 => { // $2000 - PPUCTRL
                let old_nmi_enabled = (self.ctrl & 0x80) != 0;
                let vblank_flag_active = (self.status & 0x80) != 0;
                self.ctrl = value;
                let new_nmi_enabled = (self.ctrl & 0x80) != 0;
                // Extract bits to configure scrolling targets
                self.t_addr = (self.t_addr & 0xF3FF) | (((value & 0x03) as u16) << 10);
                self.vram_increment = if (value & 0x04) == 0x04 { 32 } else { 1 };
                self.background_pattern_table = if (value & 0x10) == 0x10 { 0x1000 } else { 0x0000 };
                self.sprite_pattern_table = if (value & 0x08) == 0x08 { 0x1000 } else { 0x0000 };
                self.sprite_size = if (value & 0x20) == 0x20 { 16 } else { 8 };
            }
            1 => { // $2001 - PPUMASK
                self.mask = value;
                if RENDER_DELAY == 0 {
                    self.apply_render_on();
                } else {
                    self.render_delay = RENDER_DELAY;
                }
            }
            3 => { // $2003 - OAMADDR
                self.oam_addr = value;
            }
            4 => { // $2004 - OAMDATA
                if self.rendering_enabled() && (self.scanline < 240 || self.scanline == 261) {
                    // no write during rendering, just a glitchy +4 of OAMADDR (low bits cleared)
                    self.oam_addr = self.oam_addr.wrapping_add(4) & 0xFC;
                } else {
                    self.oam[self.oam_addr as usize] = value;
                    self.oam_addr = self.oam_addr.wrapping_add(1);
                }
            }
            5 => { // $2005 - PPUSCROLL
                if self.w_latch == false {
                    if self.frame_number != self.last_2005 {
                        self.last_2005 = self.frame_number;
                    }
                    // First write: Coarse X and Fine X scrolling values
                    self.t_addr = (self.t_addr & 0x7FE0) | ((value >> 3) as u16);
                    self.fine_x = value & 0x07;
                    self.w_latch = true;

                } else {
                    if self.frame_number != self.last_2005_2 {
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
                        self.last_2006 = self.frame_number;
                    }
                    // First write: High byte of the 14-bit destination target address
                    self.t_addr = (self.t_addr & 0x00FF) | (((value & 0x3F) as u16) << 8);
                    self.w_latch = true;
                } else {
                    let old_v = self.v_addr;
                    if self.frame_number != self.last_2006_2 {
                        self.last_2006_2 = self.frame_number;
                    }
                    // Second write: Low byte of destination target address
                    self.t_addr = (self.t_addr & 0xFF00) | (value as u16);
                    // t is copied to v a few dots later (step_one_cycle); a nametable
                    // fetch whose ALE came before and read after that gets the new high
                    // bits with the old low byte (AccuracyCoin "Hybrid Addresses")
                    self.w_latch = false;
                    self.v_update_delay = V_UPDATE_DELAY;
                    let _ = old_v;
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
                self.drive_a12_from_v(mapper, self.v_addr);
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
#[cfg(test)]
mod zapper_tests {
    use super::*;

    #[test]
    fn zapper_sees_bright_pixel_for_a_while_after_the_beam() {
        let mut ppu = NesPPU::new(Arc::new(AtomicBool::new(false)));
        let i = (100 * 256 + 50) * 4;
        ppu.back_buffer[i..i + 3].copy_from_slice(&[255, 255, 255]);
        ppu.scanline = 99; ppu.cycle = 200;
        assert!(!ppu.zapper_sees_light(50, 100), "beam hasn't drawn it yet");
        ppu.scanline = 100; ppu.cycle = 40;
        assert!(!ppu.zapper_sees_light(50, 100), "same line, before the pixel");
        ppu.scanline = 100; ppu.cycle = 60;
        assert!(ppu.zapper_sees_light(50, 100));
        ppu.scanline = 118;
        assert!(ppu.zapper_sees_light(50, 100));
        ppu.scanline = 125;
        assert!(!ppu.zapper_sees_light(50, 100), "photodiode has decayed");
        ppu.scanline = 105;
        assert!(!ppu.zapper_sees_light(51, 100), "dark pixel next to it");
        assert!(!ppu.zapper_sees_light(-1, -1), "off screen");
    }
}
