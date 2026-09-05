use crate::common::timed::Timed;
use crate::gameboy::gb_common::GbVariant;
use crate::gameboy::gb_palette::{DmgPaletteSet,PaletteTheme};
use crate::gameboy::gb_bus::GameBoyBus;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub struct GbPPU {
    hardware: GbVariant,
    cgb_mode: bool,
    last_master: u64,
    ticks: u64,
    dot: u64,
    div: u64,
    pending_irqs: u8,
    pub current_mode: u8,
    window_line_counter: u8,
    // registers
    lcdc: u8,
    scy: u8,
    scx: u8,
    bgp: u8,
    wx: u8,
    wy: u8,
    obp0: u8,
    obp1: u8,
    stat: u8,
    lyc: u8,
    ly: u8,
    pub vram: Vec<u8>,
    pub oam: [u8; 160],
    scanline_bg_indices: [u8; 160],
    scanline_bg_priority: [bool;160],
    // --- Video Buffers ---
    // Double buffering prevents the UI thread from reading half-rendered frames!
    back_buffer: Vec<u8>,       // The frame currently being drawn (Width * Height * 4 bytes RGBA)
    pub front_buffer: Arc<Mutex<Vec<u8>>>, // The last fully completed frame, safe for sharing across threads
    
    // Frame published
    frame_published: Arc<AtomicBool>,
    slice_complete: bool,

    pub active_palette: DmgPaletteSet,
    // CGB-only
    hdma_pending: bool,
    pub vbk: u8,
    bgpi: u8,
    obpi: u8,
    bgpd: [u8;64],
    obpd: [u8;64],
}

impl GbPPU {
    pub fn new(variant: GbVariant, frame_published: Arc<AtomicBool>, active_palette: DmgPaletteSet) -> Self {
        Self::with_divider(variant, frame_published, active_palette, 1)
    }

    pub fn with_divider(variant: GbVariant, frame_published: Arc<AtomicBool>, active_palette: DmgPaletteSet, div: u64) -> Self {
        let buffer_size = 160 * 144 * 4;
        let in_cgb_mode = matches!(variant, GbVariant::Cgb);
        let vram_size: usize = if in_cgb_mode { 16384 } else { 8192 };

        Self {
            hardware: variant, cgb_mode: in_cgb_mode, hdma_pending: false,
            last_master: 0, ticks: 0,
            dot: 0, current_mode: 0,
            pending_irqs: 0,
            vram: vec![0;vram_size], oam: [0;160], scanline_bg_indices: [0; 160],
            scanline_bg_priority: [false;160],
            back_buffer: vec![0; buffer_size],
            front_buffer: Arc::new(Mutex::new(vec![0; buffer_size])),
            frame_published, slice_complete: false,
            window_line_counter: 0,
            div,
            lcdc: 0x91, scy: 0, scx: 0, bgp: 0xFC, wx: 0, wy: 0,
            obp0: 0, obp1: 0, stat: 0x85, lyc: 0, ly: 0,
            active_palette,
            vbk: 0,
            bgpi: 0, obpi: 0, bgpd: [0xFF;64], obpd: [0xFF;64],
        }
    }

    fn tick_one_dot(&mut self) {
        let buffer_size = 160 * 144 * 4;

        if (self.lcdc & 0x80) == 0 {
            self.current_mode = 0;
            self.ly = 0;
            self.stat = self.stat & 0xFC;
            if self.dot >= 70224 {
                self.dot -= 70224;
                self.slice_complete = true;
            }
            return;
        }

        match self.current_mode {
            // HBlank
            0 => {
                if self.dot >= 204 {
                    self.dot -= 204;
                    self.ly = self.ly.wrapping_add(1);
                    self._update_stat_coincidence();
                    if (self.lcdc & 0x20) != 0 && self.ly >= self.wy && self.wx <= 166 {
                        // increments only when window was actually rendered on that line.
                        // I need to review this once renderer is in place.
                        self.window_line_counter = self.window_line_counter.wrapping_add(1);
                    }
                    if self.ly == 144 {
                        self._set_mode(1);
                        self._request_vblank_interrupt();
                        if let Ok(mut front_guard) = self.front_buffer.lock() {
                            std::mem::swap(&mut self.back_buffer, &mut *front_guard);
                        }
                        self.frame_published.store(true, Ordering::Release);
                    } else {
                        self._set_mode(2);
                    }
                }
            }

            // VBlank
            1 => {
                if self.dot >= 456 {
                    self.dot -= 456;
                    self.ly = self.ly.wrapping_add(1);
                    self._update_stat_coincidence();
                    self.window_line_counter = 0;
                    if self.ly > 153 {
                        self.ly = 0;
                        self.slice_complete = true;
                        self._set_mode(2);
                    }
                }
            }

            // OAM Search
            2 => {
                if self.dot >= 80 {
                    self.dot -= 80;
                    self._set_mode(3);
                }
            }

            // Drawing
            3 => {
                if self.dot >= 172 {
                    self.dot -= 172;
                    self._render_scanline();
                    self._render_sprites();
                    self._set_mode(0);

                }
            }

            _ => { }
        }
    }

    pub fn take_irqs(&mut self) -> u8 {
        std::mem::replace(&mut self.pending_irqs, 0)
    }

    pub fn get_front_buffer_ref(&self) -> Arc<Mutex<Vec<u8>>> {
        Arc::clone(&self.front_buffer)
    }

    pub fn copy_front_buffer(&self, dst: &mut [u8]) {
        if let Ok(front_guard) = self.front_buffer.lock() {
            dst.copy_from_slice(&front_guard);
        }
    }

    fn _set_mode(&mut self, new_mode: u8) {
        let old_mode = self.current_mode;
        self.current_mode = new_mode;
        self.stat = (self.stat & 0xFC) | (self.current_mode & 0x03);

        if new_mode == 0 && old_mode != 0 && self.ly < 144 {
            self.hdma_pending = true;
        }

        let mut trigger: bool = false;
        if self.current_mode == 0 && (self.stat & 0x08) != 0 { trigger = true; }
        else if self.current_mode == 1 && (self.stat & 0x10) != 0 { trigger = true; }
        else if self.current_mode == 2 && (self.stat & 0x20) != 0 { trigger = true; }

        if trigger {
            self._request_stat_interrupt();
        }
    }

    pub fn take_hdma_pending(&mut self) -> bool {
        std::mem::replace(&mut self.hdma_pending, false)
    }

    pub fn is_slice_complete(&self) -> bool {
        self.slice_complete
    }

    pub fn clear_slice_complete_flag(&mut self) {
        self.slice_complete = false;
    }

    fn _request_vblank_interrupt(&mut self) {
        self.pending_irqs |= 0x01;
    }

    fn _request_stat_interrupt(&mut self) {
        self.pending_irqs |= 0x02
    }

    fn _update_stat_coincidence(&mut self) {
        if self.ly == self.lyc {
            self.stat |= 0x04;
            if self.stat & 0x40 != 0 {
                self._request_stat_interrupt();
            }
        } else {
            self.stat &= !0x04;
        }
    }

    fn _render_scanline(&mut self) {
        let bg_enabled:bool = (self.lcdc & 0x01) != 0;
	    let win_enabled:bool = (self.lcdc & 0x20) != 0;
        let is_win_line = win_enabled && self.ly >= self.wy;

        for x in 0..160 {
            let win_x = self.wx.saturating_sub(7);
            let is_win_pixel = is_win_line && x >= win_x;

            if !self.cgb_mode && !is_win_pixel && !bg_enabled {
                let base_idx:usize = (self.ly as usize * 160 + x as usize) * 4;
                self.back_buffer[base_idx] = 0xFF;
                self.back_buffer[base_idx+1] = 0xFF;
                self.back_buffer[base_idx+2] = 0xFF;
                self.back_buffer[base_idx+3] = 0xFF;
                continue;
            }

            let tile_map_bit = if is_win_pixel { 0x40 } else { 0x08 };
            let tile_map_base = if (self.lcdc & tile_map_bit) != 0 { 0x1C00 } else { 0x1800 };
            // attempt to add with overflow.
            let temp_x = x.wrapping_add(self.scx);
            let fetch_x = if is_win_pixel { x - win_x } else { (temp_x) & 0xFF };
            let fetch_y = if is_win_pixel { self.window_line_counter } else { (self.ly.wrapping_add(self.scy)) & 0xFF };

            let tile_row = fetch_y / 8;
            let tile_col = fetch_x / 8;
            let tile_line = (fetch_y % 8) * 2;

            let tile_id_addr: usize = tile_map_base as usize + (tile_row as usize * 32) + tile_col as usize;
            let tile_id = self.vram[tile_id_addr];

            let attr = if self.cgb_mode {
                self.vram[0x2000 + tile_id_addr]
            } else {
                0
            };
            let cgb_palette = attr & 0x07;
            let vram_bank = if (attr & 0x08) != 0 { 0x2000 } else { 0 };
            let x_flip = (attr & 0x20) != 0;
            let y_flip = (attr & 0x40) != 0;
            let bg_priority = (attr & 0x80) != 0;
            let effective_tile_line = if y_flip { 7 - (fetch_y % 8) } else { fetch_y % 8 };
            let tile_data_addr:usize = vram_bank + (self._get_tile_data_address(tile_id) as usize) + (effective_tile_line as usize * 2);

            let byte1 = self.vram[tile_data_addr];
            let byte2 = self.vram[tile_data_addr + 1];
            
//            let bit_idx = 7 - (fetch_x % 8);
            let bit_idx = if x_flip { fetch_x % 8 } else { 7 - (fetch_x % 8) };
            let bit0 = (byte1 >> bit_idx) & 0x01;
            let bit1 = (byte2 >> bit_idx) & 0x01;
            let color_idx = (bit1 << 1) | bit0;
            self.scanline_bg_indices[x as usize] = color_idx;
            self.scanline_bg_priority[x as usize] = bg_priority;
            let (r,g,b) = if self.cgb_mode {
                self._get_cgb_color(&self.bgpd, cgb_palette, color_idx)
            } else {
                let color: u32 = self._apply_palette(color_idx, self.bgp, &self.active_palette.bg);
                (
                    ((color >> 16) & 0xFF) as u8,
                    ((color >> 8) & 0xFF) as u8,
                    (color & 0xFF) as u8,
                )
            };
            let base_idx: usize = (self.ly as usize * 160 + x as usize) * 4;
            self.back_buffer[base_idx]     = r;
            self.back_buffer[base_idx + 1] = g;
            self.back_buffer[base_idx + 2] = b;
            self.back_buffer[base_idx + 3] = 0xFF;
        }
    }

    fn _get_tile_data_address(&mut self, p_tile_id: u8) -> u16 {
        if (self.lcdc & 0x10) != 0 {
            // Unsigned mode ($8000 - $8FFF): Tiles 0..255
            (p_tile_id as u16) * 16
        } else {
            // Signed mode ($8800 - $97FF): Tiles -128..127 centered at $9000 (offset 0x1000)
            let signed_id = p_tile_id as i8 as i32;
            (0x1000 + (signed_id * 16)) as u16
        }
    }

    fn _old_apply_palette(&self, color_idx: u8, palette_reg: u8) -> u32 {
        let shade = (palette_reg >> (color_idx * 2)) & 0x03;

        match shade {
            0 => 0xFFFFFF,
            1 => 0xAAAAAA,
            2 => 0x555555,
            3 => 0x000000,
            _ => 0xFFFFFF,
        }
    }

    pub fn set_palette(&mut self, palette: DmgPaletteSet) {
        self.active_palette = palette;
    }
    
    fn _apply_palette(&self, color_idx: u8, palette_reg: u8, colors: &[u32; 4]) -> u32 {
        let shade = (palette_reg >> (color_idx * 2)) & 0x03;
        colors[shade as usize]
    }

    fn _get_cgb_color(&self, palette_data: &[u8; 64], palette_num: u8, color_idx: u8) -> (u8, u8, u8) {
        let base_addr = (palette_num as usize * 8) + (color_idx as usize * 2);
        let low = palette_data[base_addr] as u16;
        let high = palette_data[base_addr + 1] as u16;
        let rgb555 = low | (high << 8);

        // Extract 5-bit channels
        let r5 = (rgb555 & 0x1F) as u8;
        let g5 = ((rgb555 >> 5) & 0x1F) as u8;
        let b5 = ((rgb555 >> 10) & 0x1F) as u8;

        // Expand 5-bit to 8-bit (x * 255 / 31 or bit reflection)
        let r8 = (r5 << 3) | (r5 >> 2);
        let g8 = (g5 << 3) | (g5 >> 2);
        let b8 = (b5 << 3) | (b5 >> 2);

        (r8, g8, b8)
    }

    fn _render_sprites(&mut self) {
        if (self.lcdc & 0x02) == 0 {
            return;
        }
        let mut sprites = self._get_active_sprites();
        let sprite_16 = (self.lcdc & 0x04) != 0;

        //sprites.reverse();
        let mut claimed_pixels = [false;160];

        //for sprite_idx in sprites.iter().rev() 
        for sprite_idx in sprites.iter() {
            let oam_base:usize = *sprite_idx as usize * 4;
            let oam_y = self.oam[oam_base] as i16;
            let sprite_height = if (self.lcdc & 0x04) != 0 { 16 } else { 8 };
            let x_pos = (self.oam[oam_base + 1] as i16) - 8;
            let tile_id = self.oam[oam_base + 2];
            let attributes = self.oam[oam_base + 3];
            let ly_offset = (self.ly as i16) + 16;

            if ly_offset >= oam_y && ly_offset < (oam_y + sprite_height) {
                let raw_line = (ly_offset - oam_y) as u8;

                let x_flip = (attributes & 0x20) != 0;
                let y_flip = (attributes & 0x40) != 0;
                let oam_priority = (attributes & 0x80) != 0;
                let mut tile_data_addr:usize = 0;

                let palette = if attributes & 0x10 != 0 { self.obp1 } else { self.obp0 };

                if sprite_height == 16 {
                    let actual_tile_id = tile_id & 0xFE;
                    let mut tile_number = actual_tile_id;
                    let mut tile_line = 0;
                    if !y_flip {
                        if raw_line < 8 {
                            tile_number = actual_tile_id;
                            tile_line = raw_line;
                        } else {
                            tile_number = actual_tile_id + 1;
                            tile_line = raw_line - 8;
                        }
                    } else { // y-flipped mode. The whole 16-pixel block is upside down.
                        if raw_line < 8 {
                            tile_number = actual_tile_id + 1;
                            tile_line = 7 - raw_line;
                        } else {
                            tile_number = actual_tile_id;
                            tile_line = 7 - (raw_line - 8);
                        }
                    }
                    tile_data_addr = (tile_number as usize * 16) + (tile_line as usize * 2);
                } else {   // standard 8x8 sprite mode
                    let tile_line = if y_flip { 7 - raw_line } else { raw_line };
                    tile_data_addr = (tile_id as usize * 16) + (tile_line as usize * 2);
                }

                let vram_bank = if self.cgb_mode && (attributes & 0x08) != 0 { 0x2000 } else { 0 };
                tile_data_addr += vram_bank;

                let byte1 = self.vram[tile_data_addr];
                let byte2 = self.vram[tile_data_addr + 1];

                for x in 0..8 {
                    let screen_x = x_pos + x;
                    if screen_x < 0 || screen_x >= 160 {
                        continue;
                    }

                    let bit_idx = if x_flip { x } else { 7 - x };
                    let bit0 = (byte1 >> bit_idx) & 0x01;
                    let bit1 = (byte2 >> bit_idx) & 0x01;
                    let color_idx = (bit1 << 1) | bit0;

                    // Color 0 is transparent for sprites
                    if color_idx == 0 {
                        continue;
                    }

                    let x_idx = screen_x as usize;
                    if claimed_pixels[x_idx] {
                        continue;
                    }
                    claimed_pixels[x_idx] = true;

                    // Check BG priority
                    let bg_color_idx = self.scanline_bg_indices[x_idx];
                    let bg_priority = self.scanline_bg_priority[x_idx];
//                    if priority && bg_color_idx != 0 {
//                        continue;
//                    }

                    let sprite_hidden = if self.cgb_mode {
                        let master_priority = (self.lcdc & 0x01) != 0;
                        master_priority && bg_color_idx != 0 && (bg_priority ||  oam_priority)
                    } else {
                        oam_priority && bg_color_idx != 0
                    };

                    if sprite_hidden {
                        continue;
                    }

                    let cgb_palette = attributes & 0x07;

                    let (r, g, b) = if self.cgb_mode {
                        self._get_cgb_color(&self.obpd, cgb_palette, color_idx)
                    } else {
                        let palette_reg = if attributes & 0x10 != 0 { self.obp1 } else { self.obp0 };
                        let palette_colors = if attributes & 0x10 != 0 { &self.active_palette.obp1 } else { &self.active_palette.obp0 };
                        let color = self._apply_palette(color_idx, palette_reg, palette_colors);
                        (
                            ((color >> 16) & 0xFF) as u8,
                            ((color >> 8) & 0xFF) as u8,
                            (color & 0xFF) as u8,
                        )
                    };
                    let base_idx: usize = (self.ly as usize * 160 + screen_x as usize) * 4;
                    self.back_buffer[base_idx]     = r;
                    self.back_buffer[base_idx + 1] = g;
                    self.back_buffer[base_idx + 2] = b;
                    self.back_buffer[base_idx + 3] = 0xFF;                       // A
                }
            }   
        }
    }

    fn _get_active_sprites(&self) -> Vec<usize> {
        let oam = &self.oam;
        let lcdc = self.lcdc;
        let ly:u16 = self.ly as u16;
        let sprite_height = if (lcdc & 0x04) != 0 { 16 } else { 8 };
        let mut active_sprites: Vec<usize> = Vec::with_capacity(10);

        // Iterate through all 40 potential sprites in OAM
        for i in 0..40 {
            let oam_addr = i * 4;
        
            // Bounds check in case oam slice is smaller than expected (though OAM is always 160 bytes)
            if oam_addr + 1 >= oam.len() {
                break;
            }

            let oam_y = oam[oam_addr] as u16;

            // Game Boy hardware visibility check:
            // Sprite is visible if: OAM_Y <= current_line + 16 < OAM_Y + sprite_height
            // Note: OAM Y=0 is hidden on real hardware, but logic below handles the range check.

            if (ly + 16) >= oam_y && (ly + 16) < (oam_y + sprite_height) {
                active_sprites.push(i);
            
                // Hardware limit: only 10 sprites per scanline
                if active_sprites.len() == 10 {
                    break;
                }
            }
        }

        // Sort by X coordinate (ascending), then by OAM index (ascending) for priority
        // Closures capture 'oam' by reference (&)
        if !self.cgb_mode {
            active_sprites.sort_by(|&a, &b| {
                let x_a = oam[a * 4 + 1];
                let x_b = oam[b * 4 + 1];
        
                // Compare X coordinates
                x_a.cmp(&x_b)
                    // If X is equal, compare indices (lower index = higher priority)
                    .then_with(|| a.cmp(&b))
            });
        }

        active_sprites
    }

    pub fn read_register(&mut self, p_addr: u16) -> u8 {
        match p_addr {
            0xFF40 => { self.lcdc },
            0xFF41 => { self.stat | 0x80 },
            0xFF42 => { self.scy },
            0xFF43 => { self.scx },
            0xFF44 => { println!("LY read = {}", self.ly); self.ly },
            0xFF45 => { self.lyc },
            0xFF47 => { self.bgp },
            0xFF48 => { self.obp0 },
            0xFF49 => { self.obp1 },
            0xFF4A => { self.wy },
            0xFF4B => { self.wx },
            0xFF4F => {
                if self.hardware == GbVariant::Cgb { self.vbk | 0xFE }
                else { 0xFF }
            },
            0xFF68 => if self.cgb_mode { self.bgpi | 0x40 } else { 0xFF },
            0xFF69 => if self.cgb_mode { self.bgpd[(self.bgpi & 0x3F) as usize]} else { 0xFF },
            0xFF6A => if self.cgb_mode { self.obpi | 0x40 } else { 0xFF },
            0xFF6B => if self.cgb_mode { self.obpd[(self.obpi & 0x3F) as usize]} else { 0xFF },
            _=> { 0xFF },
        }
    }

    pub fn write_register(&mut self, p_addr: u16, p_value: u8) {
        match p_addr {
            0xFF40 => {
                let lcd_was_enabled:bool = (self.lcdc & 0x80) != 0;
                let lcd_enabled: bool = (p_value & 0x80) != 0;
                self.lcdc = p_value;
                if !lcd_was_enabled && lcd_enabled {
                    println!("lcd enabled");
                    self.ly = 0;                    // Force scanline 0
                    self.window_line_counter = 0;   // Reset internal window line counter
                    // immediately set mode 2 so STAT ($FF41) reports 0x02
                    self._set_mode(2);
                    self._update_stat_coincidence();

                    // offset dot counter by 2 to align with mid-m-cycle write timing
                    self.dot = 2;
                }
                if  !lcd_enabled && lcd_was_enabled {
                    println!("lcd disabled");
                    self.dot = 0;
                    self.ly = 0;
                    self.current_mode = 0;
                    self.stat = self.stat & 0xFC;
                }
            },
            0xFF41 => { self.stat = (self.stat & 0x07) | (p_value & 0x78); },
            0xFF42 => { self.scy = p_value; },
            0xFF43 => { self.scx = p_value; },
            0xFF44 => { },
            0xFF45 => { self.lyc = p_value; self._update_stat_coincidence(); },
            0xFF47 => { self.bgp = p_value; },
            0xFF48 => { self.obp0 = p_value; },
            0xFF49 => { self.obp1 = p_value; },
            0xFF4A => { self.wy = p_value; },
            0xFF4B => { self.wx = p_value; },
            0xFF4D => { },
            0xFF68 => { self.bgpi = p_value; },
            0xFF69 => { 
                let index = (self.bgpi & 0x3F) as usize;
                self.bgpd[index] = p_value;
                if (self.bgpi & 0x80) != 0 {
                    let new_idx = (index + 1) & 0x3F;
                    self.bgpi = (0x80) | (new_idx as u8);
                }
            },
            0xFF6A => { self.obpi = p_value; },
            0xFF6B => { 
                let index = (self.obpi & 0x3F) as usize;
                self.obpd[index] = p_value;
                if (self.obpi & 0x80) != 0 {
                    let new_idx = (index + 1) & 0x3F;
                    self.obpi = (0x80) | (new_idx as u8);
                }
            },
            0xFF4F => {
                self.vbk = p_value & 0x01;
            },
            _=> { },
        }
    }
}

impl Timed for GbPPU {
    fn run_until(&mut self, target_master: u64) {
        let target_dot = target_master / self.div;
        while self.ticks < target_dot {
            self.ticks = self.ticks.wrapping_add(1);
            self.dot = self.dot.wrapping_add(1);
            self.tick_one_dot();
        }
        self.last_master = target_master;
    }

    fn sync_point(&self) -> u64 { self.last_master }
}
