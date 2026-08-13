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
const _NINTENDO_LOGO: [u8; 6] = [0xCE, 0xED, 0x66, 0x66, 0xCC, 0x0D];

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
            sys.set_input(input_mask as u8);
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
}

