use crate::common::m6502::{M6502Cpu, CpuVariant};
//use crate::common::bus::AddressBus;
use crate::nes::nes_bus::NesBus;
use crate::nes::cartridge::Cartridge;
use crate::nes::mappers::{self, Mirroring};
use crate::nes::nes_state::NesSaveState;
use crate::nes::nes_rewind::RewindBuffer;

use godot::prelude::*;
//use serde::{Serialize, Deserialize};
use godot::classes::{AudioStreamGeneratorPlayback};
use godot::global::godot_print;
use godot::classes::{AudioStreamPlayer,Image,ImageTexture,Texture2D};
use godot::classes::image::Format;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc};
use std::path::PathBuf;

const PRG_ROM_COUNT_IDX: u8 = 0x04;
const CHR_ROM_COUNT_IDX: u8 = 0x05;
const CONTROL_BYTE_1_IDX: u8 = 0x06;
const CONTROL_BYTE_2_IDX: u8 = 0x07;
const INES2_MAPPER_BYTE:  u8 = 0x08;
const PRG_ROM_PAGE_SIZE: u16 = 1024 * 16;
const CHR_ROM_PAGE_SIZE: u16 = 1024 * 8; 


#[derive(GodotClass)]
#[class(base=RefCounted, no_init)]
pub struct NesSystem {
    cpu: M6502Cpu,
    bus: NesBus,
    frame_ready: Arc<AtomicBool>,
    is_running: Arc<AtomicBool>,
    save_battery_path: String,
    save_state_path: String,
    save_filename: String,
    sys_display: Gd<SystemDisplayInfo>,
    playback: Option<Gd<AudioStreamGeneratorPlayback>>,
    cached_image: Option<Gd<Image>>,
    cached_texture: Option<Gd<ImageTexture>>,
    pending_save_request: Option<PathBuf>,
    pending_load_request: Option<Vec<u8>>,
    rewind_buffer: RewindBuffer,
    current_frame: u64,
}

#[derive(godot::prelude::GodotClass)]
#[class(base=RefCounted, no_init)]
pub struct SystemDisplayInfo {
    // The literal dimensions of the raw texture array/Vec<u8>
    #[export] pub buffer_width: i32,
    #[export] pub buffer_height: i32,

    // The sub-rectangle that players should actually see (handles overscan)
    #[export] pub visible_x: i32,
    #[export] pub visible_y: i32,
    #[export] pub visible_width: i32,
    #[export] pub visible_height: i32,

    // The intended output aspect ratio (e.g., 4.0/3.0 for NES, 3.0/4.0 for DK)
    #[export] pub target_aspect_ratio: f32,
}

#[godot_api]
impl SystemDisplayInfo {
    fn new() -> Self {
        SystemDisplayInfo {
            buffer_width: 256,
            buffer_height: 240,
            visible_x: 0,
            visible_y: 8,
            visible_width: 256,
            visible_height: 224,
            target_aspect_ratio: 4.0/3.0,
        }
    }
    // Preset: Show the exact raw signal, glitches and all
    #[func]
    pub fn set_mode_overscan(&mut self) {
        self.visible_x = 0;
        self.visible_y = 0;
        self.visible_width = 256;
        self.visible_height = 240;
    }

    // Preset: Classic 80s TV crop (Removes SMB3 sidebars and top/bottom junk)
    #[func]
    pub fn set_mode_cropped_ntsc(&mut self) {
        self.visible_x = 8;       // Cut off left 8 pixels
        self.visible_y = 8;       // Cut off top 8 lines
        self.visible_width = 240;  // 256 - 8 (left) - 8 (right)
        self.visible_height = 224; // 240 - 8 (top) - 8 (bottom)
    }
}

#[godot_api]
impl NesSystem {
    #[func]
    pub fn create_from_bytes(rom_bytes: PackedByteArray, base_name: String) -> Option<Gd<Self>> {
        // Validate header and get sizes
        let bytes = rom_bytes.as_slice();
        if bytes.len() < 16 || &bytes[0..4] != b"NES\x1A" { return None; }

        let prg_rom_size = bytes[4] as usize * PRG_ROM_PAGE_SIZE as usize;
        let bytes_chr = bytes[5] as usize;
        let chr_rom_size = bytes_chr * CHR_ROM_PAGE_SIZE as usize;

        // extract mapper id
        let mapper_byte = bytes[INES2_MAPPER_BYTE as usize];

        let mapper_low = bytes[CONTROL_BYTE_1_IDX as usize] >> 4;
        let mapper_mid = bytes[CONTROL_BYTE_2_IDX as usize] & 0xF0;
        let mapper_high = ((mapper_byte & 0x0F) as u16) << 8;
        let mapper_id: u16;
        // slice ROM bytes
        let has_trainer:bool = (bytes[CONTROL_BYTE_1_IDX as usize] & 0b100) != 0;
        let ines2: bool = (bytes[CONTROL_BYTE_2_IDX as usize] & 0b1100) == 0x08;
        let submapper = (mapper_byte & 0xF0) >> 4;
        if ines2 {
            mapper_id = mapper_high | mapper_mid as u16 | mapper_low as u16;
            godot_print!("iNES 2 header found. Mapper={} Submapper={} ", mapper_id, submapper); 
        } else {
            mapper_id = (mapper_mid | mapper_low) as u16;
            godot_print!("iNES 1.0 header found. Mapper={}", mapper_id);
        }
        let prg_start = 16;
        let prg_end = prg_start + prg_rom_size;
        let chr_end = prg_end + (bytes[5] as usize * 8192);

        let prg_rom = bytes[prg_start..prg_end].to_vec();
        let chr_rom = if bytes[5] > 0 { bytes[prg_end..chr_end].to_vec() } else { vec![] };
        let header = bytes[0..15].to_vec();

        // initialize the mapper
        let mirroring_bit = (header[6] & 0x01) != 0;
        let initial_mirroring:Mirroring = if mirroring_bit { Mirroring::Vertical } else { Mirroring::Horizontal };
        let prg_bank_size = match mapper_id {
            4|206 => 8192,   // MMC3, DxROM: 8KB banks
            _ => 16384,      // MMC, UxROM: 16KB banks
        };
        let chr_bank_size = match mapper_id {
            4|206 => 1024,  // MMC3, DxROM: 1KB banks
            _ => 8192,      // MMC, UxROM: 8KB banks
        };
        let prg_banks = prg_rom.len() / prg_bank_size;
        let chr_banks = chr_rom.len() / chr_bank_size;

        let four_screen_bit = (header[6] & 0x08) != 0;
        let mapper = mappers::make_mapper(mapper_id, submapper, prg_banks, chr_banks, prg_rom.clone(), chr_rom.clone(), initial_mirroring, four_screen_bit).ok()?;
        /*
        let mapper = match Self::instantiate_mapper(mapper_id, prg_rom.clone(), chr_rom.clone(), header.clone()) {
            Some(m) => m,
            None => { godot_print!("Mapper not supported yet: {mapper_id}"); return None }
        };
*/
        // --- instantiate the atomic sync flag early ---
        let frame_ready = Arc::new(AtomicBool::new(false));

        // initialize cartridge and bus
        let cartridge = Cartridge::new(prg_rom, chr_rom, mapper, base_name.clone());
        godot_print!("Cartridge created: {0}", cartridge.base_filename);
        let bus = NesBus::new(cartridge, Arc::clone(&frame_ready));
        let rewind_buffer = RewindBuffer::new(60, 60);
        godot_print!("prg_rom size: {prg_rom_size} ");
        godot_print!("chr_rom size: {chr_rom_size} ");


        Some(Gd::from_init_fn(|_base| {
            Self {
                bus,
                cpu: M6502Cpu::new(CpuVariant::Ricoh2A03),
                save_filename: base_name,
                frame_ready,
                is_running: Arc::new(AtomicBool::new(false)),
                save_battery_path: "user://GD_EMU/NES/Save".to_string(),
                save_state_path: "user://GD_EMU/NES/State".to_string(),
                playback: None,
                sys_display: Gd::from_object(SystemDisplayInfo::new()),
                cached_image: None,
                cached_texture: None,
                pending_load_request: None,
                pending_save_request: None,
                rewind_buffer,
                current_frame: 0,
            }
        }))
    }

    #[func]
    pub fn get_display_info(&self) -> Gd<SystemDisplayInfo> {
        self.sys_display.clone()
    }

    #[func]
    pub fn run_slice(&mut self, input_mask: u16) {   
        if !self.is_running.load(Ordering::Relaxed) {
            return;
        }
        let mut nes_pad_state = 0u8;

        // Frontend layout vs NES expected bit positions:
        if (input_mask & (1 << 5)) != 0 { nes_pad_state |= 1 << 0; } // GDScript A (Bit 5)      -> NES A (Bit 0)
        if (input_mask & (1 << 4)) != 0 { nes_pad_state |= 1 << 1; } // GDScript B (Bit 4)      -> NES B (Bit 1)
        if (input_mask & (1 << 6)) != 0 { nes_pad_state |= 1 << 2; } // GDScript Select (Bit 6) -> NES Select (Bit 2)
        if (input_mask & (1 << 7)) != 0 { nes_pad_state |= 1 << 3; } // GDScript Start (Bit 7)  -> NES Start (Bit 3)
        if (input_mask & (1 << 0)) != 0 { nes_pad_state |= 1 << 4; } // GDScript Up (Bit 0)     -> NES Up (Bit 4)
        if (input_mask & (1 << 1)) != 0 { nes_pad_state |= 1 << 5; } // GDScript Down (Bit 1)   -> NES Down (Bit 5)
        if (input_mask & (1 << 2)) != 0 { nes_pad_state |= 1 << 6; } // GDScript Left (Bit 2)   -> NES Left (Bit 6)
        if (input_mask & (1 << 3)) != 0 { nes_pad_state |= 1 << 7; } // GDScript Right (Bit 3)  -> NES Right (Bit 7)

//        godot_print!("nes_pad_state: 0x{:02X}", nes_pad_state);
        self.bus.pad1_state = nes_pad_state;
//        self.bus.pad1_state = 0x01;
        let mut cpu_cycles_run:u16 = 0;
        while !unsafe { (*self.bus.ppu.get()).is_frame_complete() } {
            if !self.bus.bus_available {
                self.bus.step_one_cycle();
                self.bus.step_dma_one_cycle(&mut self.cpu);
                self.bus.step_remaining_ppu_cycles();
                self.cpu.sample_interrupt_lines(&mut self.bus);
            } else {
                self.bus.step_one_cycle();                
                self.cpu.step_one_cycle(&mut self.bus);
                self.bus.step_remaining_ppu_cycles();
                self.cpu.sample_interrupt_lines(&mut self.bus);
            }
            cpu_cycles_run += 1;
//            for _ in 0..3 {
//                let mapper = self.bus.cartridge.mapper_mut();
//                self.bus.ppu.get_mut().step_one_cycle(mapper);
//            }
//            let mapper = self.bus.cartridge.mapper_mut();
//            godot_print!("CPU cycles: {} | Bus cycles: {} | PPU cycles: { } | APU cycles: {} | Mapper cycles: {}", self.cpu.total_cycles(), self.bus.total_cycles(), unsafe { (*self.bus.ppu.get()).total_cycles() }, self.bus.apu.get_mut().total_cycles(), mapper.total_cycles());
//            if self.bus.total_cpu_cycles >= 100000 && self.bus.total_cpu_cycles <= 120000 {
//                godot_print!("CYCLE COUNT: cpu: {}, ppu: {}", self.bus.total_cpu_cycles, self.bus.ppu.get_mut().total_ppu_cycles);
//            }

            // failsafe
            if cpu_cycles_run > 35000 {
                break;
            }
        }

        self.current_frame += 1;
        let mut buffer = std::mem::take(&mut self.rewind_buffer);
        buffer.record_frame(self.current_frame, input_mask, self);
        self.rewind_buffer = buffer;

        self.bus.ppu.get_mut().clear_frame_complete_flag();
        
        let samples = self.bus.apu.get_mut().take_audio_samples();
//        godot_print!("Frame sample size={}", samples.len());
        if !samples.is_empty() {
            if let Some(playback) = self.playback.as_mut() {
                let frames: PackedVector2Array = samples.iter()
                    .map(|&s| Vector2::new(s, s))
                    .collect();
                playback.push_buffer(&frames);
            }
        }
    }

    #[func]
    pub fn request_rewind_to_frame(&mut self, target_frame: i64) -> bool {
        let mut buffer = std::mem::take(&mut self.rewind_buffer);
        let result = buffer.rewind_to(target_frame as u64, self);
        self.rewind_buffer = buffer;
        result.is_ok()
    }

    #[func]
    pub fn get_current_frame(&self) -> i64 {
        self.current_frame as i64
    }

    pub fn set_current_frame(&mut self, frame: u64) {
        self.current_frame = frame;
    }

    #[func]
    pub fn get_oldest_rewindable_frame(&self) -> i64 {
        self.rewind_buffer.oldest_available_frame().unwrap_or(0) as i64
    }

    #[func]
    pub fn power_on(&mut self, audio_player: Gd<AudioStreamPlayer>) {
        self.cpu.power_on(&mut self.bus);
        self.is_running.store(true, Ordering::SeqCst);
        let _playback = audio_player.get_stream_playback();
        /*
        let lo = self.bus.read_byte(0xFFFC);
        let hi = self.bus.read_byte(0xFFFD);
        let lo2 = self.bus.read_byte(0xFFFE);
        let hi2 = self.bus.read_byte(0xFFFF);
        godot_print!(
        "CPU Powering On:\n\
         - Reset Vector Bytes: [$FFFC] = 0x{:02X}, [$FFFD] = 0x{:02X}\n\
         - 0x{:02X}, 0x{:02X}\n\
         - Initial Program Counter (PC): 0x{:04X}", lo, hi, lo2, hi2, self.cpu.pc);
        */
        let save_path = format!("{}/{}.sav", self.save_battery_path, self.save_filename);
        if self.bus.cartridge.has_battery && godot::classes::FileAccess::file_exists(&save_path) {
            if let Some(mut file) = godot::classes::FileAccess::open(&save_path, godot::classes::file_access::ModeFlags::READ) {
                let file_length = file.get_length() as i64;
                let buffer = file.get_buffer(file_length);
                self.bus.load_sram(buffer.as_slice());
                godot_print!("SRAM loaded successfully during power_on.");
            }
        } else { println!("File doesn't exist at {save_path}"); }
        println!("NES System Power On: Audio streams mapped and checked for SRAM");
    }
    
    #[func]
    pub fn power_off(&mut self) {
        self.is_running.store(false, Ordering::SeqCst);
        self.cpu.is_running = false;
        self.check_and_save_sram();
        println!("NES System Power Off: Battery backed SRAM saved to persistent disk space safely.");
    }

    #[func]
    pub fn reset(&mut self) {
        self.bus.ppu.get_mut().reset();
        self.cpu.reset(&mut self.bus);
            // self.bus.reset_ram();
    }

    pub fn build_save_state(&self) -> NesSaveState {
        NesSaveState {
            version: 0.1,
            cpu: self.cpu.get_state(),
            ppu: unsafe { (*self.bus.ppu.get()).get_state() },
            apu: unsafe { (*self.bus.apu.get()).get_state() },
            ram: self.bus.ram,
            pad1_state: self.bus.pad1_state,
            pad1_shift_reg: self.bus.pad1_shift_reg.get(),
            pad_strobe: self.bus.pad_strobe,
            dma_cycles_remaining: self.bus.dma_cycles_remaining,
            dma_base_address: self.bus.dma_base_address,
            dma_temp_buffer: self.bus.dma_temp_buffer,
            bus_available: self.bus.bus_available,
            total_cpu_cycles: self.bus.total_cpu_cycles,
            mapper_number: self.bus.cartridge.mapper_number(),
            mapper_data: self.bus.cartridge.mapper().save_state(),
        }
    }

    pub fn apply_save_state(&mut self, state: &NesSaveState) -> Result<(), String> {
        if state.mapper_number != self.bus.cartridge.mapper_number() {
            return Err(format!(
                "Save state mapper ({}) doesn't match loaded ROM's mapper ({})",
                state.mapper_number, self.bus.cartridge.mapper_number()
            ));
        }

        unsafe { (*self.bus.ppu.get()).load_state(&state.ppu); }
        unsafe { (*self.bus.apu.get()).load_state(&state.apu); }
        self.bus.ram = state.ram;
        self.bus.pad1_state = state.pad1_state;
        self.bus.pad1_shift_reg.set(state.pad1_shift_reg);
        self.bus.pad_strobe = state.pad_strobe;
        self.bus.dma_cycles_remaining = state.dma_cycles_remaining;
        self.bus.dma_base_address = state.dma_base_address;
        self.bus.dma_temp_buffer = state.dma_temp_buffer;
        self.bus.bus_available = state.bus_available;
        self.bus.total_cpu_cycles = state.total_cpu_cycles;
        self.bus.cartridge.mapper_mut().load_state(&state.mapper_data);
        self.cpu.load_state(&state.cpu);

        Ok(())
    }

    pub fn save_state_to_bytes(&self) -> Result<Vec<u8>, String> {
        let nes_state = self.build_save_state();
        let config = bincode::config::standard().with_fixed_int_encoding();
        bincode::serde::encode_to_vec(&nes_state, config).map_err(|e| format!("{:?}", e))
    }

    pub fn load_state_from_bytes(&mut self, data: &[u8]) -> Result<(), String> {
        let config = bincode::config::standard().with_fixed_int_encoding();
        match bincode::serde::decode_from_slice::<NesSaveState, _>(data, config) {
            Ok((state, _)) => { self.apply_save_state(&state); Ok(()) }
            Err(e) => Err(format!("Failed to decode save state: {:?}", e)),
        }
    }

    #[func]
    pub fn save_state_to_file(&mut self, slot: u32) {
        match self.save_state_to_bytes() {
            Ok(state_bytes) => {
                if !godot::classes::DirAccess::dir_exists_absolute(&self.save_state_path) {
                    godot::classes::DirAccess::make_dir_recursive_absolute(&self.save_state_path);
                }
            

                let save_path = format!("{}/{}_slot{}.state", self.save_state_path, self.save_filename, slot);
            
                if let Some(mut file) = godot::classes::FileAccess::open(&save_path, godot::classes::file_access::ModeFlags::WRITE) {
                    let packed_array = PackedByteArray::from(&state_bytes[..]);
                    file.store_buffer(&packed_array);
                    godot_print!("Save state slot {} saved successfully.", slot);
                } else {
                    godot_print!("Error: Couldn't open file for save state at {}", save_path);
                }
            }
            Err(e) => godot_print!("Error serializing save state: {}", e),
        }
    }

    #[func]
    pub fn load_state_from_file(&mut self, slot: u32) {
        let save_path = format!("{}/{}_slot{}.state", self.save_state_path, self.save_filename, slot);

        if let Some(mut file) = godot::classes::FileAccess::open(&save_path, godot::classes::file_access::ModeFlags::READ) {
            let file_length = file.get_length() as i64;
            let packed_array = file.get_buffer(file_length);
            let state_bytes = packed_array.to_vec();

            let _ = self.load_state_from_bytes(&state_bytes);
        } else {
            godot_print!("No save state found at {}", save_path);
        }
    }

    #[func]
    pub fn check_and_save_sram(&mut self) {
        // If the mapper says no writes have happened to $6000-$7FFF, exit instantly.
        // This makes the function call practically free 99.9% of the time.
        if !self.bus.is_sram_dirty() {
            return;
        }

        if let Some(sram_bytes) = self.bus.get_sram() {
            if !godot::classes::DirAccess::dir_exists_absolute(&self.save_battery_path) {
                godot::classes::DirAccess::make_dir_recursive_absolute(&self.save_battery_path);
                godot_print!("Created missing save directory: {}", self.save_battery_path);
            }
            let save_path = format!("{}/{}.sav", self.save_battery_path, self.save_filename);
            if let Some(mut file) = godot::classes::FileAccess::open(&save_path, godot::classes::file_access::ModeFlags::WRITE) {
//                let mut packed_array = PackedByteArray::new();
//                packed_array.extend_from_slice(sram_bytes);
                let packed_array = PackedByteArray::from(&sram_bytes[..]);
                file.store_buffer(&packed_array);
                
                // Reset the flag so we don't save again until the game modifies SRAM again
                self.bus.clear_sram_dirty(); 
                godot_print!("SRAM auto-flushed to disk safely.");
            }
            else { println!("Couldn't open file for saving at {save_path}") }
        }
    }

    #[func]
    pub fn set_audio_playback(&mut self, playback: Gd<AudioStreamGeneratorPlayback>) {
        self.playback = Some(playback);
    }

    #[func]
    pub fn is_frame_ready(&self) -> bool {
        // Atomic read takes virtually zero execution cost and avoids Mutex lock stalls!
        self.frame_ready.load(Ordering::Acquire)
    }

    #[func]
    pub fn get_cpu_debug_dict(&self) -> godot::builtin::VarDictionary {
        let state = self.cpu.get_state();
        let mut dict = godot::builtin::VarDictionary::new();
        dict.set("pc", state.pc as i64);
        dict.set("a", state.a as i64);
        dict.set("x", state.x as i64);
        dict.set("y", state.y as i64);
        dict.set("sp", state.sp as i64);
        dict.set("cycles_remaining", state.cycles_remaining as i64);
        dict.set("instruction_step", state.instruction_step as i64);
        dict.set("opcode", format!("0x{:02X}", state.current_opcode));
        dict.set("flag_carry", state.status.carry);
        dict.set("flag_zero", state.status.zero);
        dict.set("flag_interrupt_disable", state.status.interrupt_disable);
        dict.set("flag_decimal", state.status.decimal);
        dict.set("flag_overflow", state.status.overflow);
        dict.set("flag_negative", state.status.negative);

        dict.set("i_flag_delayed", state.i_delay);
        dict
    }

    #[func]
    pub fn get_ppu_debug_dict(&self) -> godot::builtin::VarDictionary {
        let ppu = unsafe { &*self.bus.ppu.get() };
        let mut dict = Dictionary::new();
        dict.set("scanline", ppu.scanline as i64);
        dict.set("cycle", ppu.cycle as i64);
        dict.set("sprite0_hit", (ppu.status & 0x40) != 0);
        dict.set("vblank_active", (ppu.status & 0x80) != 0);
        dict.set("v_addr", ppu.v_addr as i64);
        dict.set("fine_x", ppu.fine_x as i64);
        dict
    }

    #[func]
    pub fn toggle_debug_trace(&mut self) {
        let ppu = self.bus.ppu.get_mut();
        ppu.toggle_debug_trace();
    }

    #[func]
    pub fn get_frame_texture(&mut self) -> Gd<Texture2D> {
        self.frame_ready.store(false, Ordering::Release);
        let raw_pixels = self.bus.ppu.get_mut().get_front_buffer();
        let pixel_data = PackedByteArray::from_iter(raw_pixels.iter().copied());

        if self.cached_image.is_none() {
            let image = Image::create_from_data(256, 240, false, Format::RGBA8, &pixel_data).unwrap();
            let texture = ImageTexture::create_from_image(&image).unwrap();
            self.cached_image = Some(image);
            self.cached_texture = Some(texture);
        } else {
            if let Some(image) = self.cached_image.as_mut() {
                image.set_data(256, 240, false, Format::RGBA8, &pixel_data);
            }
            let image_clone = self.cached_image.as_ref().unwrap().clone();
            if let Some(texture) = self.cached_texture.as_mut() {
                texture.update(&image_clone);
            }
        }
        self.cached_texture.as_ref().unwrap().clone().upcast()
    }
/*
    pub fn load_rom(&mut self, bus) {
        let rom = bus.cartridge;
        bus.Cartridge.mapper = mappers::make_mapper(
            rom.mapper_id,
            rom.prg_banks,
            rom.chr_banks,
            rom.prg_rom,
            rom.chr_rom,
            rom.mirroring,
            rom.four_screen,
        ).unwrap();
    }
*/
/*
    // A factory function that maps an integer ID to a concrete Rust struct
    fn instantiate_mapper(mapper_id: u8, prg_rom: Vec<u8>, chr_rom: Vec<u8>, header:Vec<u8>) -> Option<Box<dyn Mapper>> {
        let prg_bank_size = match mapper_id {
            4|206 => 8192,   // MMC3, DxROM: 8KB banks
            _ => 16384,      // MMC, UxROM: 16KB banks
        };
        let chr_bank_size = match mapper_id {
            4|206 => 1024,  // MMC3, DxROM: 1KB banks
            _ => 8192,      // MMC, UxROM: 8KB banks
        };
        let prg_banks = prg_rom.len() / prg_bank_size;
        let chr_banks = chr_rom.len() / chr_bank_size;
        let submapper = 0;

        let mirroring_bit = (header[6] & 0x01) != 0;
        let four_screen_bit = (header[6] & 0x08) != 0;

        match mapper_id {
            0 => { let initial_mirroring:Mirroring = if mirroring_bit { Mirroring::Vertical } else { Mirroring::Horizontal };
                godot_print!("Mapper0 (Nrom) created with initial mirroring bit={mirroring_bit}");
                Some(Box::new(Mapper0::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring))) // NROM
            }
            1 => { // MMC1 
                godot_print!("Mapper1 (MMC1) created");
                let initial_mirroring:Mirroring = if mirroring_bit { Mirroring::Vertical } else { Mirroring::Horizontal };
                Some(Box::new(Mapper1::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen_bit)))
            } 
            2 => { // UxROM
                godot_print!("Mapper2 (UxROM) created");
                let initial_mirroring:Mirroring = if mirroring_bit { Mirroring::Vertical } else { Mirroring::Horizontal };
                Some(Box::new(Mapper2::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen_bit, submapper)))
            }
            3 => { // CNROM
                godot_print!("Mapper3 (CNROM) created");
                let initial_mirroring:Mirroring = if mirroring_bit { Mirroring::Vertical } else { Mirroring::Horizontal };
                Some(Box::new(Mapper3::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen_bit)))
            }
            4 => { // MMC3
                godot_print!("Mapper4 (MMC3) created");
                let initial_mirroring:Mirroring = if mirroring_bit { Mirroring::Vertical } else { Mirroring::Horizontal };
                Some(Box::new(Mapper4::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen_bit, submapper)))
            }
            5 => { // MMC3
                godot_print!("Mapper5 (MMC5) created");
                let initial_mirroring:Mirroring = if mirroring_bit { Mirroring::Vertical } else { Mirroring::Horizontal };
                Some(Box::new(Mapper5::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen_bit)))
            }
            7 => { // AxROM
                godot_print!("Mapper7 (AxROM) created");
                let initial_mirroring:Mirroring = if mirroring_bit { Mirroring::Vertical } else { Mirroring::Horizontal };
                Some(Box::new(Mapper7::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen_bit)))
            } 
            9 => { // CNROM
                godot_print!("Mapper9 (MMC2) created");
                let initial_mirroring:Mirroring = if mirroring_bit { Mirroring::Vertical } else { Mirroring::Horizontal };
                Some(Box::new(Mapper9::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen_bit)))
            }
            34 => { // BNROM/NINA-001
                godot_print!("Mapper34 (NINA-001/NINA-002/BNROM) created");
                let initial_mirroring:Mirroring = if mirroring_bit { Mirroring::Vertical } else { Mirroring::Horizontal };
                Some(Box::new(Mapper34::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen_bit, submapper)))
            }
            69 => { // Sunsoft mappers
                godot_print!("Mapper69 (Sunsoft FME-7/5A/5B) created");
                let initial_mirroring:Mirroring = if mirroring_bit { Mirroring::Vertical } else { Mirroring::Horizontal };
                Some(Box::new(Mapper69::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen_bit, submapper)))
            } 
            206 => { // DxROM
                godot_print!("Mapper206 (DxROM) created");
                let initial_mirroring:Mirroring = if mirroring_bit { Mirroring::Vertical } else { Mirroring::Horizontal };
                Some(Box::new(Mapper206::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen_bit)))
            } 
            _ => {
                godot_error!("Unsupported Mapper ID: {}", mapper_id);
                None
            }
        }
    }
    */
}