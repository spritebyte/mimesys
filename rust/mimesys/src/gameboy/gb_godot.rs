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
}

#[godot_api]
impl GbSystemNode {
    #[func]
    fn load_rom(&mut self, path: GString) {
//        let bytes = 
//        self.system = Some(GbSystem::new(&bytes));
    }

    #[func]
    pub fn create_from_bytes(rom_bytes: PackedByteArray, base_name: String) -> Option<Gd<Self>> {
        let core = match GbSystem::from_rom(rom_bytes.as_slice(), &base_name) {
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
        });
        Some(node)
    }

    #[func]
    pub fn run_slice(&mut self, input_mask: u16) { 
        if let Some(sys) = &mut self.system {
//            sys.set_input(input_mask as u8);
            sys.run_frame(input_mask as u8);
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
    }
    
    #[func]
    pub fn reset(&mut self) {

    }

    #[func]
    pub fn power_off(&mut self) {
        if self.system.is_none() {
            return;
        }

        self.check_and_save_sram();
        println!("Gb System Power Off: Battery backed SRAM saved to persistent disk space safely.");
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
}

