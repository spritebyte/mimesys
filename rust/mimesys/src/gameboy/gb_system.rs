use crate::gameboy::gb_cpu::GameBoyCpu;
use crate::gameboy::gb_bus::GameBoyBus;
use crate::gameboy::gb_common::GbVariant;
use crate::gameboy::gb_cartridge::{GbCartridge, CartType};
use crate::gameboy::gb_palette::*;
use crate::gameboy::gb_mbc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub struct GbSystem {
    pub cpu: GameBoyCpu,
    pub bus: GameBoyBus,
    pub slice_complete: bool,
    pub frame_published: Arc<AtomicBool>,
    pub is_running: Arc<AtomicBool>,
    pub save_battery_path: String,
    pub save_state_path: String,
    pub save_filename: String,
    pub current_frame: u64,
    pub cgb_mode: bool,
    pub current_ui_theme: PaletteTheme,
}

impl GbSystem {
    pub fn from_rom(rom_bytes: &[u8], base_name: &str) -> Result<Self, String> {
        // Validate header and get sizes
        let bytes = rom_bytes;

        let cart_type_code = bytes[0x147];
        println!("Cart type code={:02X}", cart_type_code);
        let prg_rom_size = (1 << bytes[0x148]) * 1024 * 32;
        let cart_ram_size = Self::calculate_ram_size(bytes[0x149]) as usize;
        let cart_type = Self::determine_mbc(cart_type_code);
        let prg_start = 0;
        let prg_end = prg_start + prg_rom_size;

        let prg_rom = bytes[prg_start..prg_end].to_vec();
        let cart_ram = if cart_ram_size > 0 { vec![0; cart_ram_size] } else { vec![] };
        let color_gb = bytes[0x143];
        let variant = if color_gb == 0x80 || color_gb == 0xC0 {
            GbVariant::Cgb
        } else {
            GbVariant::Dmg
        };
        let header_title = std::str::from_utf8(&bytes[0x0134..0x0143]).unwrap_or("");
        let selected_palette = PaletteTheme::to_palette_set(PaletteTheme::Auto, header_title);
        // initialize the mapper
        let mbc = gb_mbc::make_mbc(cart_type.mbc_id, prg_rom.clone(), cart_ram.clone());
        //{
        //    Some(m) => m,
        //    None => { println!("Cart type not supported yet: {}", cart_type.mbc_id); None }
        //};

        // --- instantiate the atomic sync flag early ---
        let frame_ready = Arc::new(AtomicBool::new(false));

        // initialize cartridge and bus
        let cartridge = GbCartridge::new(prg_rom, cart_ram, mbc?, base_name.to_string());
        println!("Cartridge created: {0} for {1}", cartridge.base_filename, header_title);
        let bus = GameBoyBus::new(variant, cartridge, Arc::clone(&frame_ready), selected_palette);
        println!("prg_rom size: {prg_rom_size} ");
        println!("cart ram size: {cart_ram_size} ");

        Ok( {
            Self {
                bus,
                cpu: GameBoyCpu::new(variant),
                save_filename: base_name.to_string(),
                slice_complete: false, cgb_mode: false,
                // todo: different path for gameboy color? allow user to select path?
                save_battery_path: "user://GD_EMU/Gb/Save".to_string(),
                save_state_path: "user://GD_EMU/Gb/State".to_string(),
                is_running: Arc::new(AtomicBool::new(false)),
                current_frame: 0,
                frame_published: frame_ready,
                current_ui_theme: PaletteTheme::Auto,
            }
        })
    }

    fn calculate_ram_size(header_byte: u8) -> u32 {
        match header_byte {
            0x00 => 0,
            0x01 => 2048,
            0x02 => 8192,
            0x03 => 32768,
            0x04 => 131072,
            0x05 => 65536,
            _=> 0,
        }
    }

    fn determine_mbc(cart_type_code: u8) -> CartType {
        match cart_type_code {
            0 => CartType { mbc_id: 0, has_battery: false },
            1|2|7|8 => CartType { mbc_id: 1, has_battery: false },
            3 => CartType { mbc_id: 1, has_battery: true },
            5 => CartType { mbc_id: 2, has_battery: false },
            6 => CartType { mbc_id: 2, has_battery: true },
            9 => CartType { mbc_id: 0, has_battery: true },
            0x0F|0x11|0x12 => CartType { mbc_id: 3, has_battery: false },
            0x10|0x13 => CartType { mbc_id: 3, has_battery: true },
            0x1B => CartType { mbc_id: 5, has_battery: true },
            _ => {
                println!("unknown carttype {:02X}, using default of no MBC", cart_type_code);
                CartType { mbc_id: 0, has_battery: false }
            },
        }
    }

    pub fn run_frame(&mut self, input: u8) {
        unsafe { (*self.bus.ppu.get()).clear_slice_complete_flag() };
        self.current_frame = self.current_frame.wrapping_add(1);
        self.set_input(input);
        while !unsafe { (*self.bus.ppu.get()).is_slice_complete() } {
            self.tick();
        }
    }

    pub fn framebuffer(&self) -> Vec<u8> {
        Vec::new()
    }

    // Function is setting bits high for pressed, gameboy expects bits to be low for pressed
    // So reading 0xFF00 needs to invert the result
    pub fn set_input(&mut self, input_mask: u8) {
        let mut gb_pad_state = 0u8;

        // Action Buttons (Bits 0..3 -> P10..P13 when P15 is low)
        if (input_mask & (1 << 5)) != 0 { gb_pad_state |= 1 << 0; } // A      -> Bit 0
        if (input_mask & (1 << 4)) != 0 { gb_pad_state |= 1 << 1; } // B      -> Bit 1
        if (input_mask & (1 << 6)) != 0 { gb_pad_state |= 1 << 2; } // Select -> Bit 2
        if (input_mask & (1 << 7)) != 0 { gb_pad_state |= 1 << 3; } // Start  -> Bit 3

        // Directional Buttons (Bits 4..7 -> P10..P13 when P14 is low)
        if (input_mask & (1 << 3)) != 0 { gb_pad_state |= 1 << 4; } // Right  -> Bit 4
        if (input_mask & (1 << 2)) != 0 { gb_pad_state |= 1 << 5; } // Left   -> Bit 5
        if (input_mask & (1 << 0)) != 0 { gb_pad_state |= 1 << 6; } // Up     -> Bit 6
        if (input_mask & (1 << 1)) != 0 { gb_pad_state |= 1 << 7; } // Down   -> Bit 7

        self.bus.pad1_state = gb_pad_state;
    }

    pub fn tick(&mut self) {
        let before = self.bus.master;
        self.cpu.step_one_m_cycle(&mut self.bus);
        debug_assert_eq!(self.bus.master - before, self.bus.cycle_len(), 
                         "M-cycle must advance the clock exactly once");
    }

    pub fn power_on(&mut self) {
        self.is_running.store(true, Ordering::SeqCst);
    }

    pub fn power_off(&mut self) {
        self.is_running.store(false, Ordering::SeqCst);
    }

    pub fn is_running(&self) -> bool {
        self.is_running.load(Ordering::Relaxed)
    }

    pub fn has_battery(&self) -> bool {
        self.bus.cartridge.has_battery
    }

    pub fn save_filename(&self) -> &str {
        &self.save_filename
    }

    pub fn save_battery_path(&self) -> &str {
        &self.save_battery_path
    }

    pub fn is_sram_dirty(&self) -> bool {
        self.bus.is_sram_dirty()
    }

    pub fn clear_sram_dirty(&mut self) {
        self.bus.clear_sram_dirty();
    }

    pub fn get_sram(&self) -> Option<&[u8]> {
        self.bus.get_sram()
    }

    pub fn load_sram(&mut self, data: &[u8]) {
        self.bus.load_sram(data);
    }
}