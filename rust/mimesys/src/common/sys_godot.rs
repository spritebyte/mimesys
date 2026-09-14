use godot::prelude::*;
use godot::classes::{AudioStreamGeneratorPlayback};
use godot::classes::{Image,ImageTexture,Texture2D};
use godot::classes::image::Format;
use crate::common::system::{System,DisplayInfo};
use crate::common::gd_sys_display::SystemDisplayInfo;
//use std::sync::atomic::Ordering;
use std::collections::VecDeque;
use std::collections::HashMap;
use crate::common::system_factory::{load_system, LoadRequest, RomSource, SystemKind};

#[derive(GodotClass)]
#[class(base=RefCounted)]
pub struct EmuSystem {
    system: Option<Box<dyn System>>,
    base: Base<RefCounted>,
    sys_display: Gd<SystemDisplayInfo>,
    playback: Option<Gd<AudioStreamGeneratorPlayback>>,
    cached_image: Option<Gd<Image>>,
    cached_texture: Option<Gd<ImageTexture>>,
    last_sample: Vector2,
    audio_fifo: VecDeque<f32>
}

#[godot_api]
impl IRefCounted for EmuSystem {
    fn init(base: Base<RefCounted>) -> Self {
        EmuSystem { system: None, base, playback: None,
             sys_display: Gd::from_object(SystemDisplayInfo::new_gameboy()),
             cached_image: None, cached_texture: None, last_sample: Vector2::ZERO,
             audio_fifo: VecDeque::new(),
        }
    }
}

#[godot_api]
impl EmuSystem {
    #[func]
    fn load_cartridge(&mut self, kind: i32, rom: PackedByteArray, filename: String, bios: PackedByteArray) -> bool {
        let bios_opt = if bios.is_empty() { None } else { Some(bios.to_vec()) };
        let req = LoadRequest {
            kind: kind_from_i32(kind),
            rom: RomSource::SingleFile(rom.to_vec()),
            filename,
            bios: bios_opt,
        };
        self.finish_load(req)
    }

    #[func]
    fn load_romset(&mut self, kind: i32, files: VarDictionary, filename: String) -> bool {
        let mut set: HashMap<String, Vec<u8>> = HashMap::new();
        for (k, v) in files.iter_shared() {
            let name: GString = k.to();
            let bytes: PackedByteArray = v.to();
            set.insert(name.to_string(), bytes.to_vec());
        }
        let req = LoadRequest {
            kind: kind_from_i32(kind),
            rom: RomSource::FileSet(set),
            filename,
            bios: None,
        };
        self.finish_load(req)
    }
 
    fn finish_load(&mut self, req: LoadRequest) -> bool {
        match load_system(req) {
            Ok(sys) => { self.system = Some(sys); true }
            Err(e) => { godot_error!("load failed: {e}"); false }
        }
    }

    // TODO: call this run_slice or run_frame?
    #[func]
    pub fn run_slice(&mut self, input: u32) {
        if let Some(sys) = &mut self.system {
//            sys.bus.joypad.set_button_state(input_mask as u8, &mut sys.bus.iflags);
            sys.run_frame(input);
        }
    }

    #[func]
    fn display_info(&self) -> Gd<SystemDisplayInfo> {
        let info = match &self.system {
            Some(sys) => sys.display_info(),
            None => return SystemDisplayInfo::from_core(DisplayInfo::default()),
        };
        SystemDisplayInfo::from_core(info)
    }

    #[func]
    pub fn save_state_to_file(slot: u8) {

    }

    #[func]
    pub fn load_state_from_file(slot: u8) {

    }

    #[func]
    pub fn get_display_info(&self) -> Gd<SystemDisplayInfo> {
        self.sys_display.clone()
    }

    #[func]
    pub fn power_on(&mut self) {
        let Some(sys) = &mut self.system else { return; };
//        sys.set_running(true);
//        sys.is_running.store(true, Ordering::SeqCst);
        let save_path = format!("{}/{}.sav", sys.save_battery_path(), sys.save_filename());
        if sys.has_battery() && godot::classes::FileAccess::file_exists(&save_path) {
            if let Some(mut file) = godot::classes::FileAccess::open(&save_path, godot::classes::file_access::ModeFlags::READ) {
                let file_length = file.get_length() as i64;
                let buffer = file.get_buffer(file_length);
                sys.load_sram(buffer.as_slice());
                godot_print!("SRAM loaded successfully during power_on.");
            }
        } else { println!("No save file doesn't exist at {save_path}"); }
        sys.power_on();
        println!("System Power On");
    }
    
    #[func]
    pub fn reset(&mut self) {
        let Some(sys) = &mut self.system else { return; };
    }

    #[func]
    pub fn power_off(&mut self) {
        let Some(sys) = &mut self.system else { return; };
        sys.power_off();
        self.check_and_save_sram();
        println!("System Power Off: Battery backed SRAM saved to persistent disk space safely.");
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
        if let Some(sram_bytes) = sys.get_sram() {
            let dir_path_str = sys.save_battery_path();
            let dir_path = GString::from(dir_path_str);

            if !godot::classes::DirAccess::dir_exists_absolute(&dir_path) {
                let err = godot::classes::DirAccess::make_dir_recursive_absolute(&dir_path);
                if err != godot::global::Error::OK {
                    godot_print!("Failed to create save directory '{}'. Error: {:?}", dir_path_str, err);
                    return;
                }
                godot_print!("Created missing save directory: {}", sys.save_battery_path());
            }
            let save_path_str = format!("{}/{}.sav", dir_path_str, sys.save_filename());
            let save_path = GString::from(&save_path_str);

            if let Some(mut file) = godot::classes::FileAccess::open(&save_path, godot::classes::file_access::ModeFlags::WRITE) {

                let packed_array = PackedByteArray::from(&sram_bytes[..]);
                file.store_buffer(&packed_array);
                file.flush();
                sys.clear_sram_dirty(); 
                godot_print!("SRAM successfully saved to {save_path_str}");
            }
            else { println!("Couldn't open file for saving at {save_path}") }
        }
    }

    #[func]
    pub fn is_frame_ready(&self) -> bool {
        let Some(sys) = &self.system else { return false;};
//        sys.frame_published.load(Ordering::Acquire)
//        sys.is_frame_ready()
        true
    }

    #[func]
    pub fn get_current_frame(&self) -> i64 {
        let Some(sys) = &self.system else { return 0;};
        sys.get_current_frame()
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
//        sys.set_frame_published(false);
        //        sys.frame_published.store(false, Ordering::Release);
        let (w, h) = { let d = sys.display_info(); (d.visible_width, d.visible_height) };
        let pixel_data = PackedByteArray::from(sys.framebuffer());

        if self.cached_image.is_none() {
            let image = Image::create_from_data(w as i32, h as i32, false, Format::RGBA8, &pixel_data).unwrap();
            let texture = ImageTexture::create_from_image(&image).unwrap();
            self.cached_image = Some(image);
            self.cached_texture = Some(texture);
        } else {
            if let Some(image) = self.cached_image.as_mut() {
                image.set_data(w as i32, h as i32, false, Format::RGBA8, &pixel_data);
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
        if avail <= 0 { return; }

        if let Some(sys) = &mut self.system {
//            let apu = sys.bus.apu.get_mut();
//            self.audio_fifo.extend(apu.drain_samples());
            sys.drain_audio();
        } else { return; };
//        let raw_samples = apu.drain_samples(); // [L, R, L, R, ...]
//        let available_frames = raw_samples.len() / 2;

        let queued_frames = self.audio_fifo.len() / 2;
        let to_write = queued_frames.min(avail as usize);

        let mut frames = PackedVector2Array::new();
        frames.resize(to_write as usize);

//        let count = available_frames.min(avail as usize);

        for i in 0..to_write {
//            let left = raw_samples[i * 2];
//            let right = raw_samples[i * 2 + 1];
            let left = self.audio_fifo.pop_front().unwrap();
            let right = self.audio_fifo.pop_front().unwrap();
            let sample_vec = Vector2::new(left, right);
            frames[i] = sample_vec;
            self.last_sample = sample_vec;
        }

        // FIXED: Fill remaining available buffer space with silence/last_sample 
        // to prevent audio stream underflow popping/silence stalls
 //       for i in count..(avail as usize) {
//            frames[i] = self.last_sample;
//        }
        if to_write > 0 {
            playback.push_buffer(&frames);
        }

        let max_queued = (avail as usize) * 4 * 2;
        while self.audio_fifo.len() > max_queued {
            self.audio_fifo.pop_front();
        }
    }
}

fn kind_from_i32(v: i32) -> SystemKind {
    match v {
        0 => SystemKind::GameBoy,
        1 => SystemKind::GameBoyColor,
        2 => SystemKind::Gba,
        3 => SystemKind::Pacman,
        _ => SystemKind::GameBoy,
    }
}
