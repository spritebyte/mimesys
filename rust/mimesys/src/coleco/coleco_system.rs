use crate::common::cpu::z80::Z80Cpu;
use crate::coleco::coleco_bus::ColecoBus;
use crate::coleco::coleco_cartridge::ColecoCartridge;
use crate::common::system::{System,DisplayInfo,Rotation};
use crate::common::snapshot::Snapshot;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
//use std::path::Path;
use std::io::Cursor;

pub struct ColecoSystem {
    pub cpu: Z80Cpu,
    pub bus: ColecoBus,
    pub slice_complete: bool,
    pub is_running: Arc<AtomicBool>,
    pub save_state_path: String,
    pub save_filename: String,
    pub current_frame: u64,
    pub frame_cache: Vec<u8>,
}

impl ColecoSystem {
    pub fn from_rom(rom_bytes: &[u8], base_name: &str, bios: &[u8]) -> Result<Self, String> {
        // Validate header and get sizes
        let bytes = rom_bytes;
        let mut run_boot_rom = true;
        let prg_rom = bytes.to_vec();
        let prg_rom_size = prg_rom.len();
        let buffer_size = 160 * 144 * 4;

//        let expected_size = if cgb_mode { 2304 } else { 256 };
//        if bios.len() != expected_size {
//            println!("Boot ROM is {} bytes, expected {}", bios.len(), expected_size);
//        }

        // initialize cartridge and bus
        let cartridge = ColecoCartridge::new(prg_rom, base_name.to_string());
        println!("Cartridge created: {0}", cartridge.base_filename);
        let bus = ColecoBus::new(cartridge, run_boot_rom, bios);
        println!("prg_rom size: {0} ", prg_rom_size);
  
        Ok( {
            Self {
                bus,
                cpu: Z80Cpu::new(),
                save_filename: base_name.to_string(),
                slice_complete: false,
                save_state_path: "user://GD_EMU/Coleco/State".to_string(),
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
        let cycles = self.cpu.step(&mut self.bus);
        self.bus.master += cycles as u64;
    }
}

impl System for ColecoSystem {
    fn run_frame(&mut self, input: u32) {
        unsafe { (*self.bus.vdp.get()).clear_slice_complete_flag() };
        self.current_frame = self.current_frame.wrapping_add(1);
        self.set_input(input);
        while !unsafe { (*self.bus.vdp.get()).is_slice_complete() } {
            self.tick();
        }
        self.frame_cache = {
            let vdp = self.bus.vdp.get_mut();
            let front_guard = vdp.front_buffer.lock().unwrap();
            front_guard.clone()
        };
    }

    fn set_input(&mut self, input_mask: u32) {
        self.bus.joypad.set_button_state(input_mask as u8);
    }

    fn drain_audio(&mut self) -> Vec<f32> {
        unsafe { (*self.bus.psg.get()).drain_samples() }
    }
 
    fn sample_rate(&self) -> u32 {
        44100
    }

    fn power_on(&mut self) {
        self.is_running.store(true, Ordering::SeqCst);
    }

    fn power_off(&mut self) {
        self.is_running.store(false, Ordering::SeqCst);
    }

    fn has_battery(&self) -> bool {
        false
    }

    fn save_filename(&self) -> &str {
        &self.save_filename
    }

    fn save_battery_path(&self) -> &str {
        &self.save_state_path
    }

    fn is_sram_dirty(&self) -> bool {
        false
    }

    fn clear_sram_dirty(&mut self) { }

    fn get_current_frame(&self) -> i64 {
        self.current_frame as i64
    }

    fn get_sram(&self) -> Option<&[u8]> {
        None
    }

    fn load_sram(&mut self, data: &[u8]) { }

    fn framebuffer(&self) -> &[u8] {
        &self.frame_cache
    }

    fn reset(&mut self) {
        self.power_off();
        self.power_on();
    }

    fn display_info(&self) -> DisplayInfo {
        DisplayInfo {
            buffer_width: 256,
            buffer_height: 192,
            visible_x: 0,
            visible_y: 0,
            visible_width: 256,
            visible_height: 192,
            target_aspect_ratio: 10.0/9.0,
            rotation: Rotation::None,
        }
    }

    fn save_state(&self) -> Vec<u8> {
        todo!("hook up Coleco save state")
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let _ = data;
        todo!("hook up Coleco load state")
    }
}


impl Snapshot for ColecoSystem {
    fn save(&self, out: &mut Vec<u8>) {
    }

    fn load(&mut self, cur: &mut Cursor<&[u8]>) {

    }
}