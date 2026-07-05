use crate::common::m6502::{M6502Cpu, CpuVariant};
use crate::common::bus::AddressBus;
use crate::nes::nes_bus::NesBus;
use crate::nes::mappers::{Mapper, Mapper0, Mirroring};
use crate::nes::cartridge::Cartridge;
use crate::nes::mapper1::Mapper1;
use crate::nes::mapper2::Mapper2;
use crate::nes::mapper3::Mapper3;
use crate::nes::mapper4::Mapper4;
use crate::nes::mapper5::Mapper5;
use crate::nes::mapper7::Mapper7;
use crate::nes::mapper9::Mapper9;
use crate::nes::mapper34::Mapper34;
use crate::nes::mapper69::Mapper69;
use crate::nes::mapper206::Mapper206;

use godot::prelude::*;
use godot::classes::{Node, AudioStreamGeneratorPlayback};
use godot::global::godot_print;
use godot::classes::{AudioStreamPlayer,Image,ImageTexture,Texture2D};
use godot::classes::image::Format;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

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
    save_filename: String,
    sys_display: Gd<SystemDisplayInfo>,
    playback: Option<Gd<AudioStreamGeneratorPlayback>>,
    cached_image: Option<Gd<Image>>,
    cached_texture: Option<Gd<ImageTexture>>,
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
            visible_x: 8,
            visible_y: 8,
            visible_width: 240,
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
    pub fn get_display_info(&self) -> Gd<SystemDisplayInfo> {
        self.sys_display.clone()
    }

    #[func]
    pub fn create_from_bytes(rom_bytes: PackedByteArray, base_name: String) -> Option<Gd<Self>> {
        // Validate header and get sizes
        let bytes = rom_bytes.as_slice();
        if bytes.len() < 16 || &bytes[0..4] != b"NES\x1A" { return None; }

        let prg_rom_size = bytes[4] as usize * PRG_ROM_PAGE_SIZE as usize;
        let bytes_chr = bytes[5] as usize;
        let chr_rom_size = bytes_chr * CHR_ROM_PAGE_SIZE as usize;

        // extract mapper id
        let mapper_id_low = bytes[6] >> 4;
        let mapper_id_high = bytes[7] & 0xF0;
        let mapper_id = mapper_id_high | mapper_id_low;

        // slice ROM bytes
        let has_trainer:bool = (bytes[CONTROL_BYTE_1_IDX as usize] & 0b100) != 0;
        let ines2: bool = (bytes[CONTROL_BYTE_2_IDX as usize] & 0b1100) == 0x08;
        let mapper_byte = bytes[CONTROL_BYTE_2_IDX as usize];
        let submapper = (mapper_byte & 0xF0) >> 4;
        if ines2 { godot_print!("iNES 2 header found. Submapper={:02X} ", submapper); }
        else { godot_print!("iNES header byte found. {:02X}", mapper_byte)}
        let prg_start = 16;
        let prg_end = prg_start + prg_rom_size;
        let chr_end = prg_end + (bytes[5] as usize * 8192);

        let prg_rom = bytes[prg_start..prg_end].to_vec();
        let chr_rom = if bytes[5] > 0 { bytes[prg_end..chr_end].to_vec() } else { vec![] };
        let header = bytes[0..15].to_vec();

        // initialize the mapper
        let mapper = match Self::instantiate_mapper(mapper_id, prg_rom.clone(), chr_rom.clone(), header.clone()) {
            Some(m) => m,
            None => { godot_print!("Mapper not supported yet: {mapper_id}"); return None }
        };

        // --- instantiate the atomic sync flag early ---
        let frame_ready = Arc::new(AtomicBool::new(false));

        // initialize cartridge and bus
        let cartridge = Cartridge::new(prg_rom, chr_rom, mapper, base_name.clone());
        godot_print!("Cartridge created: {0}", cartridge.base_filename);
        let bus = NesBus::new(cartridge, Arc::clone(&frame_ready));
        godot_print!("prg_rom size: {prg_rom_size} ");
        godot_print!("chr_rom size: {chr_rom_size} ");


        Some(Gd::from_init_fn(|_base| {
            Self {
                cpu: M6502Cpu::new(CpuVariant::Ricoh2A03),
                bus,
                save_filename: base_name,
                frame_ready,
                is_running: Arc::new(AtomicBool::new(false)),
                save_battery_path: "user://GD_EMU/NES/Save".to_string(),
                playback: None,
                sys_display: Gd::from_object(SystemDisplayInfo::new()),
                cached_image: None,
                cached_texture: None,
            }
        }))
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
        let mut cycles_this_frame:u16 = 0;
        while cycles_this_frame < 29780 {
            let mapper = self.bus.cartridge.mapper_mut();
            self.bus.ppu.get_mut().step(mapper, 3);
            self.cpu.step_one_cycle(&mut self.bus);
            self.bus.total_cpu_cycles += 1;
            self.bus.step_cycles(1);
            cycles_this_frame += 1;
        }
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
    pub fn power_on(&mut self, mut audio_player: Gd<AudioStreamPlayer>) {
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
        self.cpu.reset(&self.bus);
            // self.bus.reset_ram();
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
}