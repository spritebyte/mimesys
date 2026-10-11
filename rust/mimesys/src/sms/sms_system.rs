use crate::common::cpu::z80::Z80Cpu;
use crate::sms::sms_bus::SmsBus;
use crate::sms::sms_cartridge::{SmsCartridge, CartType};
use crate::common::system::{System,DisplayInfo,Rotation};
use crate::common::snapshot::Snapshot;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::io::Cursor;

pub struct SmsSystem {
    pub cpu: Z80Cpu,
    pub bus: SmsBus,
    pub slice_complete: bool,
    pub is_running: Arc<AtomicBool>,
    pub save_battery_path: String,
    pub save_state_path: String,
    pub save_filename: String,
    pub current_frame: u64,
    pub frame_cache: Vec<u8>,
}

impl SmsSystem {
    pub fn from_rom(rom_bytes: &[u8], base_name: &str, bios: &[u8]) -> Result<Self, String> {
        let bytes = rom_bytes;
        let mut run_boot_rom = true;

        let prg_rom = rom_bytes.to_vec();
        let prg_rom_size = prg_rom.len();
        //let header_title = std::str::from_utf8(&bytes[0x0134..0x0143]).unwrap_or("");

        // initialize cartridge and bus
        let cartridge = SmsCartridge::new(prg_rom, base_name.to_string());
        println!("SMS Cartridge created: {0}", cartridge.base_filename);
        let bus = SmsBus::new(variant, cartridge, selected_palette, run_boot_rom, bios);
        println!("prg_rom size: {prg_rom_size} ");

        Ok( {
            Self {
                bus,
                cpu: Z80Cpu::new(run_boot_rom, variant),
                save_filename: base_name.to_string(),
                slice_complete: false, cgb_mode: cgb_mode,
                save_battery_path: "user://GD_EMU/SMS/Save".to_string(),
                save_state_path: "user://GD_EMU/SMS/State".to_string(),
                is_running: Arc::new(AtomicBool::new(false)),
                current_frame: 0,
                frame_cache: vec![0; buffer_size],
            }
        })
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

impl System for SmsSystem {
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
        todo!("hook up SMS save state")
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let _ = data;
        todo!("hook up SMS load state")
    }
}


impl Snapshot for SmsSystem {
    fn save(&self, out: &mut Vec<u8>) {
    }

    fn load(&mut self, cur: &mut Cursor<&[u8]>) {

    }
}