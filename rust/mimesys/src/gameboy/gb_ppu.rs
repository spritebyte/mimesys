use crate::common::timed::Timed;
use crate::gameboy::gb_common::GbVariant;
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
    // --- Video Buffers ---
    // Double buffering prevents the UI thread from reading half-rendered frames!
    back_buffer: Vec<u8>,       // The frame currently being drawn (Width * Height * 4 bytes RGBA)
    pub front_buffer: Arc<Mutex<Vec<u8>>>, // The last fully completed frame, safe for sharing across threads
    
    // Frame published
    frame_published: Arc<AtomicBool>,
    slice_complete: bool,
    // CGB-only
    vbk: u8,
}

impl GbPPU {
    pub fn new(variant: GbVariant, frame_published: Arc<AtomicBool>) -> Self {
        let buffer_size = 160 * 144 * 4;
        let vram_size: usize = match variant {
            GbVariant::Cgb => 16384,
            _=> 8192,
        };
        Self {
            hardware: variant, cgb_mode: false,
            last_master: 0, ticks: 0,
            dot: 0, current_mode: 0,
            pending_irqs: 0,
            vram: vec![0;vram_size], oam: [0;160],
            back_buffer: vec![0; buffer_size],
            front_buffer: Arc::new(Mutex::new(vec![0; buffer_size])),
            frame_published, slice_complete: false,
            window_line_counter: 0,
            div: 1,
            // Registers
            lcdc: 0x91, scy: 0, scx: 0, bgp: 0xFC, wx: 0, wy: 0,
            obp0: 0, obp1: 0, stat: 0x85, lyc: 0, ly: 0,
            vbk: 0,
        }
    }

    pub fn with_divider(variant: GbVariant, frame_published: Arc<AtomicBool>, div: u64) -> Self {
        let buffer_size = 160 * 144 * 4;
        let vram_size: usize = match variant {
            GbVariant::Cgb => 16384,
            _=> 8192,
        };

        Self {
            hardware: variant, cgb_mode: false,
            last_master: 0, ticks: 0,
            dot: 0, current_mode: 0,
            pending_irqs: 0,
            vram: vec![0;vram_size], oam: [0;160],
            back_buffer: vec![0; buffer_size],
            front_buffer: Arc::new(Mutex::new(vec![0; buffer_size])),
            frame_published, slice_complete: false,
            window_line_counter: 0,
            div,
            lcdc: 0x91, scy: 0, scx: 0, bgp: 0xFC, wx: 0, wy: 0,
            obp0: 0, obp1: 0, stat: 0x85, lyc: 0, ly: 0,
            vbk: 0,
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
        let mut trigger: bool = false;
        if self.current_mode == 0 && (self.stat & 0x08) != 0 { trigger = true; }
        else if self.current_mode == 1 && (self.stat & 0x10) != 0 { trigger = true; }
        else if self.current_mode == 2 && (self.stat & 0x20) != 0 { trigger = true; }

        if trigger {
            self._request_stat_interrupt();
        }
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

            if !is_win_pixel && !bg_enabled {
                let base_idx:usize = (self.ly as usize * 160 + x as usize) * 4;
                self.back_buffer[base_idx] = 0xFF;
                self.back_buffer[base_idx+1] = 0xFF;
                self.back_buffer[base_idx+2] = 0xFF;
                self.back_buffer[base_idx+3] = 0xFF;
                continue;
            }

            let tile_map_bit = if is_win_pixel { 0x40 } else { 0x08 };
            let tile_map_base = if (self.lcdc & tile_map_bit) != 0 { 0x1C00 } else { 0x1800 };
            let fetch_x = if is_win_pixel { x - win_x } else { (x + self.scx) & 0xFF };
            let fetch_y = if is_win_pixel { self.window_line_counter } else { (self.ly.wrapping_add(self.scy)) & 0xFF };

            let tile_row = fetch_y / 8;
            let tile_col = fetch_x / 8;
            let tile_line = (fetch_y % 8) * 2;

            let tile_id_addr: usize = tile_map_base as usize + (tile_row as usize * 32) + tile_col as usize;
            let tile_id = self.vram[tile_id_addr];

            let tile_data_addr:usize = (self._get_tile_data_address(tile_id) + tile_line as u16) as usize;
            let byte1 = self.vram[tile_data_addr];
            let byte2 = self.vram[tile_data_addr + 1];
            
            let bit_idx = 7 - (fetch_x % 8);
            let bit0 = (byte1 >> bit_idx) & 0x01;
            let bit1 = (byte2 >> bit_idx) & 0x01;
            let color_idx = (bit1 << 1) | bit0;

            // TODO: store background indices for checking sprite priority 
            let color: u32 = self._apply_palette(color_idx, self.bgp);
            let base_idx: usize = (self.ly as usize * 160 + x as usize) * 4;

            self.back_buffer[base_idx]     = (color >> 16) as u8 & 0xFF;    // R
            self.back_buffer[base_idx + 1] = (color >> 8) as u8 & 0xFF; // G
            self.back_buffer[base_idx + 2] = color as u8 & 0xFF;        // B
            self.back_buffer[base_idx + 3] = 0xFF;                      // A
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

    fn _apply_palette(&self, color_idx: u8, palette_reg: u8) -> u32 {
        let shade = (palette_reg >> (color_idx * 2)) & 0x03;

        match shade {
            0 => 0xFFFFFF,
            1 => 0xAAAAAA,
            2 => 0x555555,
            3 => 0x000000,
            _ => 0xFFFFFF,
        }
    }

    fn _render_sprites(&mut self) {
        if (self.lcdc & 0x02) == 0 {
            return;
        }
    }

    pub fn read_register(&mut self, p_addr: u16) -> u8 {
        match p_addr {
            0xFF40 => { self.lcdc },
            0xFF41 => { self.stat | 0x80 },
            0xFF42 => { self.scy },
            0xFF43 => { self.scx },
            0xFF44 => { self.ly },
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
            // unmapped reads return 0xFF, but I need to remember to review how open bus
            // actually works on Gameboy and Gameboy Color in case of any exceptions
            // According to Claude it floats high rather than holding a latch like the NES.
            _=> { 0xFF },
        }
    }

    pub fn write_register(&mut self, p_addr: u16, p_value: u8) {
        match p_addr {
            0xFF40 => {
                let lcd_was_enabled:bool = (self.lcdc & 0x80) != 0;
                let lcd_enabled: bool = (p_value & 0x80) != 0;
                self.lcdc = p_value;
                if  !lcd_enabled && lcd_was_enabled {
                    println!("lcd disabled");
                    self.dot = 0;
                    self.ly = 0;
                    self._set_mode(0);
                }
                if !lcd_was_enabled && lcd_enabled {
                    println!("lcd enabled");
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
            // VBK register is CGB only, need to treat as unmapped/open bus on original DMG.
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
