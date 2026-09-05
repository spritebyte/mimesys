use godot::prelude::*;
use godot::classes::{AudioStreamGeneratorPlayback};
use godot::classes::{AudioStreamPlayer,Image,ImageTexture,Texture2D};
use godot::classes::image::Format;
use crate::gameboy::gb_system::GbSystem;
use crate::gameboy::gb_bus::GameBoyBus;
use crate::common::gd_sys_display::SystemDisplayInfo;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc};
use std::path::PathBuf;
use std::path::Path;

// only checking a few bytes from logo
const _NINTENDO_LOGO: [u8; 10] = [0xCE, 0xED, 0x66, 0x66, 0xCC, 0x0D, 0x00, 0x0B, 0x03, 0x73];

#[derive(GodotClass)]
#[class(base=Node, no_init)]
pub struct GbSystemNode {
    system: Option<GbSystem>,   // the pure core, held inside
    base: Base<Node>,
    sys_display: Gd<SystemDisplayInfo>,
    playback: Option<Gd<AudioStreamGeneratorPlayback>>,
    cached_image: Option<Gd<Image>>,
    cached_texture: Option<Gd<ImageTexture>>,
    last_sample: Vector2,
}

#[godot_api]
impl GbSystemNode {
    #[func]
    fn load_rom(&mut self, path: GString) {
//        let bytes = 
//        self.system = Some(GbSystem::new(&bytes));
    }

    #[func]
    pub fn create_from_bytes(rom_bytes: PackedByteArray, base_name: String, bios: PackedByteArray) -> Option<Gd<Self>> {
//        let boot_rom_path = "user://BIOS/gbc_bios.bin".to_string();
//        let boot_path = Some(Path::new(&boot_rom_path));
    //    let boot_path = None;
        let core = match GbSystem::from_rom(rom_bytes.as_slice(), &base_name, bios.as_slice()) {
            Ok(c) => c,
            Err(e) => { godot_print!("GB load failed: {}", e); return None; }
        };

        let node = Gd::from_init_fn(|base| GbSystemNode {
            base,
            system: Some(core),
            sys_display: Gd::from_object(SystemDisplayInfo::new_gameboy()),
            playback: None,
            cached_image: None,
            cached_texture: None,
            last_sample: Vector2::ZERO,
        });
        Some(node)
    }

    #[func]
    pub fn run_slice(&mut self, input_mask: u16) { 
        if let Some(sys) = &mut self.system {
//            sys.set_input(input_mask as u8);
            sys.bus.joypad.set_button_state(input_mask as u8, &mut sys.bus.iflags);
            sys.run_frame();
//            self.blit(sys.framebuffer());
//            self.queue_audio(sys.audio_samples());
        }
    }

    #[func]
    pub fn get_display_info(&self) -> Gd<SystemDisplayInfo> {
        self.sys_display.clone()
    }

    fn blit(&mut self, buffer: Vec<u8>) {
        
    }

    #[func]
    pub fn power_on(&mut self, audio_player: Gd<AudioStreamPlayer>) {
        let Some(sys) = &mut self.system else { return; };
        sys.is_running.store(true, Ordering::SeqCst);
//        let _playback = audio_player.get_stream_playback();
        let save_path = format!("{}/{}.sav", sys.save_battery_path(), sys.save_filename());
        if sys.has_battery() && godot::classes::FileAccess::file_exists(&save_path) {
            if let Some(mut file) = godot::classes::FileAccess::open(&save_path, godot::classes::file_access::ModeFlags::READ) {
                let file_length = file.get_length() as i64;
                let buffer = file.get_buffer(file_length);
                sys.load_sram(buffer.as_slice());
                godot_print!("SRAM loaded successfully during power_on.");
            }
        } else { println!("No save file doesn't exist at {save_path}"); }
        println!("Gb System Power On");
        sys.power_on();
    }
    
    #[func]
    pub fn reset(&mut self) {
        let Some(sys) = &mut self.system else { return; };
    }

    #[func]
    pub fn power_off(&mut self) {
        let Some(sys) = &mut self.system else { return; };
        println!("Gb System Power Off: Battery backed SRAM saved to persistent disk space safely.");
        sys.power_off();
        self.check_and_save_sram();
    }

    #[func]
    pub fn set_audio_playback(&mut self, playback: Gd<AudioStreamGeneratorPlayback>) {
        self.playback = Some(playback);
    }

    #[func]
    pub fn check_and_save_sram(&mut self) {
        let Some(sys) = &mut self.system else {return;};
        if !sys.is_sram_dirty() {
            return;
        }
        if let Some(sram_bytes) = sys.bus.get_sram() {
            let dir_path_str = sys.save_battery_path();
            let dir_path = GString::from(dir_path_str);

            if !godot::classes::DirAccess::dir_exists_absolute(&dir_path) {
                let err = godot::classes::DirAccess::make_dir_recursive_absolute(&dir_path);
                if err != godot::global::Error::OK {
                    godot_print!("Failed to create save directory '{}'. Error: {:?}", dir_path_str, err);
                    return;
                }
                godot_print!("Created missing save directory: {}", sys.save_battery_path);
            }
            let save_path_str = format!("{}/{}.sav", dir_path_str, sys.save_filename());
            let save_path = GString::from(&save_path_str);

            if let Some(mut file) = godot::classes::FileAccess::open(&save_path, godot::classes::file_access::ModeFlags::WRITE) {
//                let mut packed_array = PackedByteArray::new();
//                packed_array.extend_from_slice(sram_bytes);
                let packed_array = PackedByteArray::from(&sram_bytes[..]);
                file.store_buffer(&packed_array);
                file.flush();
                // Reset the flag so we don't save again until the game modifies SRAM again
                sys.bus.clear_sram_dirty(); 
                godot_print!("SRAM successfully saved to {save_path_str}");
            }
            else { println!("Couldn't open file for saving at {save_path}") }
        }
    }

    #[func]
    pub fn is_frame_ready(&self) -> bool {
        let Some(sys) = &self.system else { return false;};
        sys.frame_published.load(Ordering::Acquire)
    }

    #[func]
    pub fn get_current_frame(&self) -> i64 {
        let Some(sys) = &self.system else { return 0;};
        sys.current_frame as i64
    }

    #[func]
    pub fn request_rewind_to_frame(&mut self, target_frame: i64) -> bool {
        false
    }

    #[func]
    pub fn get_oldest_rewindable_frame(&self) -> i64 {
        0
    }

    #[func]
    pub fn get_frame_texture(&mut self) -> Gd<Texture2D>  {
        let Some(sys) = &mut self.system else { return Default::default(); };
        sys.frame_published.store(false, Ordering::Release);
        let pixel_data = {
            let ppu = sys.bus.ppu.get_mut();
            let front_guard = ppu.front_buffer.lock().unwrap();
            PackedByteArray::from(front_guard.as_slice())
        };

        if self.cached_image.is_none() {
            let image = Image::create_from_data(160, 144, false, Format::RGBA8, &pixel_data).unwrap();
            let texture = ImageTexture::create_from_image(&image).unwrap();
            self.cached_image = Some(image);
            self.cached_texture = Some(texture);
        } else {
            if let Some(image) = self.cached_image.as_mut() {
                image.set_data(160, 144, false, Format::RGBA8, &pixel_data);
            }
            let image_clone = self.cached_image.as_ref().unwrap().clone();
            if let Some(texture) = self.cached_texture.as_mut() {
                texture.update(&image_clone);
            }
        }
        self.cached_texture.as_ref().unwrap().clone().upcast()
    }

    #[func]
    pub fn update_audio_buffer(&mut self) {
        let Some(playback) = &mut self.playback else { return };

        let avail = playback.get_frames_available();
        if avail <= 0 {
            return;
        }

        let Some(sys) = &mut self.system else { return; };
        let apu = sys.bus.apu.get_mut();
        let raw_samples = apu.drain_samples(); // [L, R, L, R, ...]
        let available_frames = raw_samples.len() / 2;

        let mut frames = PackedVector2Array::new();
        frames.resize(avail as usize);

        let count = available_frames.min(avail as usize);

        for i in 0..count {
            let left = raw_samples[i * 2];
            let right = raw_samples[i * 2 + 1];
            let sample_vec = Vector2::new(left, right);
            frames[i] = sample_vec;
            self.last_sample = sample_vec;
        }

        // FIXED: Fill remaining available buffer space with silence/last_sample 
        // to prevent audio stream underflow popping/silence stalls
        for i in count..(avail as usize) {
            frames[i] = self.last_sample;
        }

        playback.push_buffer(&frames);
    }
}

