use crate::gameboy::gb_cpu::{GameBoyCpu,GbVariant};
use crate::gameboy::gb_bus::GameBoyBus;
use crate::gameboy::gb_cartridge::{GbCartridge, CartType};
use crate::gameboy::gb_mbc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub struct GbSystem {
    pub cpu: GameBoyCpu,
    pub bus: GameBoyBus,
    pub frame_ready: Arc<AtomicBool>,
    pub is_running: Arc<AtomicBool>,
    pub save_battery_path: String,
    pub save_state_path: String,
    pub save_filename: String,
}

impl GbSystem {
    pub fn from_rom(rom_bytes: &[u8], base_name: &str) -> Result<Self, String> {
        // Validate header and get sizes
        let bytes = rom_bytes;

        let cart_type_code = bytes[0x147];
        let prg_rom_size = (1 << bytes[0x148]) * 1024 * 32;
        let cart_ram_size = Self::calculate_ram_size(bytes[0x149]) as usize;
        let cart_type = Self::determine_mbc(cart_type_code);
        let prg_start = 0;
        let prg_end = prg_start + prg_rom_size;

        let prg_rom = bytes[prg_start..prg_end].to_vec();
        let cart_ram = if cart_ram_size > 0 { vec![0; cart_ram_size] } else { vec![] };

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
        println!("Cartridge created: {0}", cartridge.base_filename);
        let bus = GameBoyBus::new(cartridge, Arc::clone(&frame_ready));
        println!("prg_rom size: {prg_rom_size} ");
        println!("cart ram size: {cart_ram_size} ");

        Ok( {
            Self {
                bus,
                cpu: GameBoyCpu::new(GbVariant::Dmg),
                save_filename: base_name.to_string(),
                frame_ready,
                // todo: different path for gameboy color?
                save_battery_path: "user://GD_EMU/Gb/Save".to_string(),
                save_state_path: "user://GD_EMU/Gb/State".to_string(),
                is_running: Arc::new(AtomicBool::new(false)),
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
            0 => CartType {mbc_id: 0, has_battery: false},
            _ => CartType {mbc_id: 0, has_battery: false},
        }
    }

    pub fn run_frame(&mut self) {

    }

    pub fn framebuffer(&self) -> Vec<u8> {
        Vec::new()
    }

    pub fn set_input(&mut self, input_mask: u8) {
        let mut gb_pad_state = 0u8;

        // Frontend layout vs Gameboy expected bit positions:
        if (input_mask & (1 << 5)) != 0 { gb_pad_state |= 1 << 0; } // GDScript A (Bit 5)      -> GB A (Bit 0)
        if (input_mask & (1 << 4)) != 0 { gb_pad_state |= 1 << 1; } // GDScript B (Bit 4)      -> GB B (Bit 1)
        if (input_mask & (1 << 6)) != 0 { gb_pad_state |= 1 << 2; } // GDScript Select (Bit 6) -> GB Select (Bit 2)
        if (input_mask & (1 << 7)) != 0 { gb_pad_state |= 1 << 3; } // GDScript Start (Bit 7)  -> GB Start (Bit 3)
        if (input_mask & (1 << 0)) != 0 { gb_pad_state |= 1 << 4; } // GDScript Up (Bit 0)     -> GB Up (Bit 4)
        if (input_mask & (1 << 1)) != 0 { gb_pad_state |= 1 << 5; } // GDScript Down (Bit 1)   -> GB Down (Bit 5)
        if (input_mask & (1 << 2)) != 0 { gb_pad_state |= 1 << 6; } // GDScript Left (Bit 2)   -> GB Left (Bit 6)
        if (input_mask & (1 << 3)) != 0 { gb_pad_state |= 1 << 7; } // GDScript Right (Bit 3)  -> GB Right (Bit 7)

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