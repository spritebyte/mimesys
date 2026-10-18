use crate::gameboy::gb_cpu::GameBoyCpu;
use crate::gameboy::gb_bus::GameBoyBus;
use crate::gameboy::gb_common::GbVariant;
use crate::gameboy::gb_cartridge::{GbCartridge, CartType};
use crate::gameboy::gb_palette::*;
use crate::gameboy::gb_mbc;
use crate::common::system::{System,DisplayInfo,Rotation};
use crate::common::snapshot::Snapshot;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
//use std::path::Path;
use std::io::Cursor;

pub struct GbSystem {
    pub cpu: GameBoyCpu,
    pub bus: GameBoyBus,
    pub slice_complete: bool,
//    pub frame_published: Arc<AtomicBool>,
    pub is_running: Arc<AtomicBool>,
    pub save_battery_path: String,
    pub save_state_path: String,
    pub save_filename: String,
    pub current_frame: u64,
    pub cgb_mode: bool,
    pub current_ui_theme: PaletteTheme,
    pub frame_cache: Vec<u8>,
}

impl GbSystem {
    pub fn from_rom(rom_bytes: &[u8], base_name: &str, bios: &[u8]) -> Result<Self, String> {
        // Validate header and get sizes
        let bytes = rom_bytes;
        let mut run_boot_rom = true;
        let buffer_size = 160 * 144 * 4;
        let cart_type_code = bytes[0x147];
        println!("Cart type code={:02X}", cart_type_code);
        let prg_rom_size = (1 << bytes[0x148]) * 1024 * 32;
        let cart_ram_size = Self::calculate_ram_size(bytes[0x149]) as usize;
        let cart_type = Self::determine_mbc(cart_type_code);
        let prg_start = 0;
        let prg_end = prg_start + prg_rom_size;

        let prg_rom = bytes.to_vec();
        let cart_ram = if cart_ram_size > 0 { vec![0; cart_ram_size] } else { vec![] };
        let cgb_mode = (bytes[0x143] & 0x80) != 0;
        let expected_size = if cgb_mode { 2304 } else { 256 };
        if bios.len() != expected_size {
            println!("Boot ROM is {} bytes, expected {}", bios.len(), expected_size);
        }

        let variant = if cgb_mode { //|| (run_boot_rom && bios.len() == 2304) {
            println!("Loading CGB variant");
            GbVariant::Cgb
        } else {
            println!("Loading DMG variant");
            run_boot_rom = false;
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
        // let frame_ready = Arc::new(AtomicBool::new(false));

        // initialize cartridge and bus
        let cartridge = GbCartridge::new(prg_rom, cart_ram, mbc?, base_name.to_string(), cart_type.has_battery);
        println!("Cartridge created: {0} for {1}", cartridge.base_filename, header_title);
        let bus = GameBoyBus::new(variant, cartridge, selected_palette, run_boot_rom, bios);
        println!("prg_rom size: {prg_rom_size} ");
        println!("cart ram size: {cart_ram_size} ");

        Ok( {
            Self {
                bus,
                cpu: GameBoyCpu::new(run_boot_rom, variant),
                save_filename: base_name.to_string(),
                slice_complete: false, cgb_mode: cgb_mode,
                // todo: different path for gameboy color? allow user to select path?
                save_battery_path: "user://GD_EMU/GB/Save".to_string(),
                save_state_path: "user://GD_EMU/GB/State".to_string(),
                is_running: Arc::new(AtomicBool::new(false)),
                current_frame: 0,
//                frame_published: frame_ready,
                current_ui_theme: PaletteTheme::Auto,
                frame_cache: vec![0; buffer_size],
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
            0x19|0x1A|0x1C|0x1D => CartType { mbc_id: 5, has_battery: false },
            0x1B|0x1E|0x1F => CartType { mbc_id: 5, has_battery: true },
            _ => {
                println!("unknown carttype {:02X}, using default of no MBC", cart_type_code);
                CartType { mbc_id: 0, has_battery: false }
            },
        }
    }

    fn is_running(&self) -> bool {
        self.is_running.load(Ordering::Relaxed)
    }

    pub fn tick(&mut self) {
        let before = self.bus.master;
        self.cpu.step_one_m_cycle(&mut self.bus);
        debug_assert_eq!(self.bus.master - before, self.bus.cycle_len(), 
                         "M-cycle must advance the clock exactly once");
    }
}

impl System for GbSystem {
    fn run_frame(&mut self, input: u32) {
        unsafe { (*self.bus.ppu.get()).clear_slice_complete_flag() };
        self.current_frame = self.current_frame.wrapping_add(1);
        self.set_input(input);
        while !unsafe { (*self.bus.ppu.get()).is_slice_complete() } {
            self.tick();
        }
        self.frame_cache = {
            let ppu = self.bus.ppu.get_mut();
            let front_guard = ppu.front_buffer.lock().unwrap();
            front_guard.clone()
        };
    }

    // Function is setting bits high for pressed, gameboy expects bits to be low for pressed
    // So reading 0xFF00 needs to invert the result
    fn set_input(&mut self, input_mask: u32) {
        /*
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
        */
        self.bus.joypad.set_button_state(input_mask as u8, &mut self.bus.iflags);
    }

    fn drain_audio(&mut self) -> Vec<f32> {
        // Forward to the APU's drain_samples() through the bus.
        unsafe { (*self.bus.apu.get()).drain_samples() }
    }
 
    fn sample_rate(&self) -> u32 {
        44100   // whatever you constructed the APU with
    }

/*    fn is_frame_ready(&self) -> bool {
        self.frame_published.load(Ordering::Acquire)
    }
*/
    fn power_on(&mut self) {
        self.is_running.store(true, Ordering::SeqCst);
        println!("CPU A at boot={:02X}", self.cpu.a);
    }

    fn power_off(&mut self) {
        self.is_running.store(false, Ordering::SeqCst);
    }

    fn has_battery(&self) -> bool {
        self.bus.cartridge.has_battery
    }

    fn save_filename(&self) -> &str {
        &self.save_filename
    }

    fn save_battery_path(&self) -> &str {
        &self.save_battery_path
    }

    fn save_state_path(&self) -> &str {
        &self.save_state_path
    }

    fn is_sram_dirty(&self) -> bool {
        self.bus.is_sram_dirty()
    }

    fn clear_sram_dirty(&mut self) {
        self.bus.clear_sram_dirty();
    }

    fn get_current_frame(&self) -> i64 {
        self.current_frame as i64
    }

    fn get_sram(&self) -> Option<&[u8]> {
        self.bus.get_sram()
    }

    fn load_sram(&mut self, data: &[u8]) {
        self.bus.load_sram(data);
    }

    fn framebuffer(&self) -> &[u8] {
        &self.frame_cache
    }

    fn reset(&mut self) {
        self.power_off();
        self.power_on();
    }

    fn display_info(&self) -> DisplayInfo {
        DisplayInfo {
            buffer_width: 160,
            buffer_height: 144,
            visible_x: 0,
            visible_y: 0,
            visible_width: 160,
            visible_height: 144,
            target_aspect_ratio: 10.0/9.0,
            rotation: Rotation::None,
        }
    }

    fn save_state(&self) -> Vec<u8> {
        todo!("hook up GB save state")
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let _ = data;
        todo!("hook up GB load state")
    }
}


impl Snapshot for GbSystem {
    fn save(&self, out: &mut Vec<u8>) {
    }

    fn load(&mut self, cur: &mut Cursor<&[u8]>) {

    }
}