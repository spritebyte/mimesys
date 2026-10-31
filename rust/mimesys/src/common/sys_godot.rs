use godot::prelude::*;
use godot::classes::{AudioStreamGeneratorPlayback};
use godot::classes::{Image,ImageTexture,Texture2D};
use godot::classes::image::Format;
use crate::common::system::{System,DisplayInfo};
use crate::common::gd_sys_display::SystemDisplayInfo;
use crate::common::rotate::rotate_rgba;
use crate::common::system::Rotation;
use crate::gameboy::gb_system::GbSystem;
use crate::gameboy::gb_link_system::GbLinkPair;
use crate::gameboy::gb_netlink::NetLink;
//use std::sync::atomic::Ordering;
use std::collections::VecDeque;
use std::collections::HashMap;
use crate::common::system_factory::{ CartSystem, load_system, LoadRequest };

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
    fn load_cartridge(&mut self, system_id: GString, rom: PackedByteArray, filename: String, bios: PackedByteArray) -> bool {
        let system = match CartSystem::from_id(&system_id.to_string()) {
            Ok(s) => s,
            Err(e) => { godot_error!("{e}"); return false; }
        };
        let bios = if bios.is_empty() { None } else { Some(bios.to_vec()) };
        self.finish_load(LoadRequest::Cartridge { system, rom: rom.to_vec(), filename, bios })
    }

    #[func]
    fn load_arcade(&mut self, driver: GString, set_name: GString, regions: VarDictionary) -> bool {
        let mut map = HashMap::new();
        for (k, v) in regions.iter_shared() {
            let name: GString = k.to();
            let bytes: PackedByteArray = v.to();
            map.insert(name.to_string(), bytes.to_vec());
        }
        self.finish_load(LoadRequest::Arcade {
            driver: driver.to_string(),
            set_name: set_name.to_string(),
            regions: map,
        })
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

    /// Microseconds per frame for the loaded game (0 = the frontend's default).
    #[func]
    fn frame_time_usec(&self) -> i64 {
        self.system.as_ref().map(|s| s.frame_time_usec()).unwrap_or(0)
    }

    #[func]
    fn display_info(&self) -> Gd<SystemDisplayInfo> {
        let Some(sys)= &self.system else {
            return SystemDisplayInfo::from_core(DisplayInfo::default());
        };
        let mut info = sys.display_info();
        if !matches!(info.rotation, Rotation::None) {
            std::mem::swap(&mut info.buffer_width, &mut info.buffer_height);
            std::mem::swap(&mut info.visible_x, &mut info.visible_y);
            std::mem::swap(&mut info.visible_width, &mut info.visible_height);
        }
        SystemDisplayInfo::from_core(info)
    }

    fn state_path(sys: &dyn System, slot: u8) -> String {
        format!("{}/{}_slot{}.state", sys.save_state_path(), sys.save_filename(), slot)
    }

    /// Where `slot`'s state file lives ("" if nothing is loaded). May not exist yet.
    #[func]
    pub fn state_file_path(&self, slot: u8) -> GString {
        match &self.system {
            Some(sys) => GString::from(Self::state_path(sys.as_ref(), slot).as_str()),
            None => GString::new(),
        }
    }

    #[func]
    pub fn save_state_to_file(&mut self, slot: u8) -> bool {
        let Some(sys) = &self.system else { return false; };
        let state_bytes = sys.save_state();
        if state_bytes.is_empty() {
            godot_print!("Save states aren't supported for this system yet");
            return false;
        }
        if !godot::classes::DirAccess::dir_exists_absolute(sys.save_state_path()) {
            godot::classes::DirAccess::make_dir_recursive_absolute(sys.save_state_path());
        }
        let save_path = Self::state_path(sys.as_ref(), slot);
        match godot::classes::FileAccess::open(&save_path, godot::classes::file_access::ModeFlags::WRITE) {
            Some(mut file) => {
                file.store_buffer(&PackedByteArray::from(&state_bytes[..]));
                file.flush();
                godot_print!("Save state slot {} saved to {}", slot, save_path);
                true
            }
            None => {
                godot_print!("Error: Couldn't open file for save state at {}", save_path);
                false
            }
        }
    }

    #[func]
    pub fn load_state_from_file(&mut self, slot: u8) -> bool {
        let Some(sys) = &mut self.system else { return false; };
        let save_path = Self::state_path(sys.as_ref(), slot);
        let Some(mut file) = godot::classes::FileAccess::open(&save_path, godot::classes::file_access::ModeFlags::READ) else {
            godot_print!("No save state found at {}", save_path);
            return false;
        };
        let len = file.get_length() as i64;
        let state_bytes = file.get_buffer(len).to_vec();
        match sys.load_state(&state_bytes) {
            Ok(()) => true,
            Err(e) => {
                godot_print!("Couldn't load slot {}: {}", slot, e);
                false
            }
        }
    }

    #[func]
    pub fn reset(&mut self) {
        let Some(sys) = &mut self.system else { return; };
        sys.reset();
    }


    #[func]
    pub fn get_display_info(&self) -> Gd<SystemDisplayInfo> {
        self.sys_display.clone()
    }

    #[func]
    pub fn power_on(&mut self) {
        let Some(sys) = &mut self.system else { return; };
        load_battery(sys.as_mut());
        if let Some(partner) = sys.link_partner() {
            load_battery(partner);
        }
        sys.power_on();
        println!("System Power On");
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
        save_battery(sys.as_mut());
        if let Some(partner) = sys.link_partner() {
            save_battery(partner);
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
        let info = sys.display_info();
        let (pixels, w, h) = rotate_rgba(
            sys.framebuffer(),
            info.buffer_width as usize,
            info.buffer_height as usize,
            info.rotation,
        );
        let pixel_data = PackedByteArray::from(pixels.as_slice());
        //        let (w, h) = { let d = sys.display_info(); (d.visible_width, d.visible_height) };
//        let pixel_data = PackedByteArray::from(sys.framebuffer());
        let size_changed = self.cached_image.as_ref()
            .map_or(true, |img| img.get_width() != w as i32 || img.get_height() != h as i32);

        if self.cached_image.is_none() || size_changed {
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
            self.audio_fifo.extend(sys.drain_audio());
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
/*
        let target = 2048.0;
        let fill = (self.audio_fifo.len() / 2) as f32;
        let factor = (1.0 + 0.005 * (target - fill) / target).clamp(0.995, 1.005);
        if let Some(sys) = &mut self.system {
            sys.set_audio_rate_adjust(factor);
        } else { };
*/
        if to_write > 0 {
            playback.push_buffer(&frames);
        }

        let max_queued = (avail as usize) * 4 * 2;
        while self.audio_fifo.len() > max_queued {
            self.audio_fifo.pop_front();
        }
    }

    #[func]
    pub fn set_cheats(&mut self, codes: PackedStringArray) -> PackedStringArray {
        let codes: Vec<String> = codes.as_slice().iter().map(|s| s.to_string()).collect();
        let errors: Vec<String> = match self.system.as_mut().and_then(|s| s.cheats_mut()) {
            Some(cheats) => cheats.set(&codes),
            None => codes.iter().map(|_| "cheats aren't supported for this system yet".to_string()).collect(),
        };
        PackedStringArray::from_iter(errors.iter().map(|e| GString::from(e.as_str())))
    }

    #[func]
    pub fn supports_cheats(&mut self) -> bool {
        self.system.as_mut().map_or(false, |s| s.cheats_mut().is_some())
    }

    #[func]
    fn load_gb_link(&mut self, rom_a: PackedByteArray, name_a: String,
                    rom_b: PackedByteArray, name_b: String, bios: PackedByteArray) -> bool {
        let a = match GbSystem::from_rom(rom_a.as_slice(), &name_a, bios.as_slice()) {
            Ok(s) => s, Err(e) => { godot_error!("Game Boy 1: {e}"); return false; }
        };
        let b = match GbSystem::from_rom(rom_b.as_slice(), &name_b, bios.as_slice()) {
            Ok(s) => s, Err(e) => { godot_error!("Game Boy 2: {e}"); return false; }
        };
        self.system = Some(Box::new(GbLinkPair::new(a, b)));
        true
    }

    /// Two GBAs side by side, joined by a link cable (GBA 1 = multiplayer parent).
    #[func]
    fn load_gba_link(&mut self, rom_a: PackedByteArray, name_a: String,
                     rom_b: PackedByteArray, name_b: String, bios: PackedByteArray) -> bool {
        use crate::gba::{gba_link::GbaLinkPair, gba_system::GbaSystem};
        let a = match GbaSystem::from_rom(rom_a.as_slice(), &name_a, bios.as_slice()) {
            Ok(s) => s, Err(e) => { godot_error!("GBA 1: {e}"); return false; }
        };
        let b = match GbaSystem::from_rom(rom_b.as_slice(), &name_b, bios.as_slice()) {
            Ok(s) => s, Err(e) => { godot_error!("GBA 2: {e}"); return false; }
        };
        self.system = Some(Box::new(GbaLinkPair::new(a, b)));
        true
    }

    #[func]
    fn set_option(&mut self, key: GString, value: i64) {
        if let Some(sys) = &mut self.system {
            sys.set_option(&key.to_string(), value);
        }
    }

    /// Light gun pointer (framebuffer pixels, -1 = off screen; buttons bit 0 left, bit 1 right)
    #[func]
    fn set_pointer(&mut self, x: i64, y: i64, buttons: i64) {
        if let Some(sys) = &mut self.system {
            sys.set_pointer(x as i32, y as i32, buttons as u32);
        }
    }

    #[func]
    fn detect_rom(data: PackedByteArray, path: GString) -> PackedStringArray {
        use crate::common::rom_detect;
        let guesses = rom_detect::detect(data.as_slice(), &path.to_string());
        let mut lines = vec![if rom_detect::is_clear(&guesses) { "clear".to_string() } else { "ask".to_string() }];
        lines.extend(guesses.iter().map(|g| format!("{}|{}|{}", g.system, g.score, g.why)));
        PackedStringArray::from_iter(lines.iter().map(|s| GString::from(s.as_str())))
    }

    #[func]
    fn net_host(&mut self, port: i64) -> GString {
        let Some(sys) = &mut self.system else { return "Load a game first".into(); };
        match NetLink::host(port as u16) {
            Ok(link) => {
                if sys.attach_net(Some(link)) { GString::new() }
                else { "This system has no link port".into() }
            }
            Err(e) => GString::from(e.as_str()),
        }
    }

    #[func]
    fn net_join(&mut self, address: GString, port: i64) -> GString {
        let Some(sys) = &mut self.system else { return "Load a game first".into(); };
        match NetLink::join(&address.to_string(), port as u16) {
            Ok(link) => {
                if sys.attach_net(Some(link)) { GString::new() }
                else { "This system has no link port".into() }
            }
            Err(e) => GString::from(e.as_str()),
        }
    }

    #[func]
    fn net_disconnect(&mut self) {
        if let Some(sys) = &mut self.system {
            sys.attach_net(None);
        }
    }

    /// Debug log lines collected since the last call (GBA link log when "link_trace" is on).
    #[func]
    fn take_trace(&mut self) -> PackedStringArray {
        match &mut self.system {
            Some(sys) => PackedStringArray::from_iter(sys.take_trace().iter().map(|l| GString::from(l.as_str()))),
            None => PackedStringArray::new(),
        }
    }

    #[func]
    fn net_status(&self) -> GString {
        match &self.system {
            Some(sys) => GString::from(sys.cable_status().as_str()),
            None => GString::new(),
        }
    }
}

fn load_battery(sys: &mut dyn System) {
    let save_path = format!("{}/{}.sav", sys.save_battery_path(), sys.save_filename());
    if sys.has_battery() && godot::classes::FileAccess::file_exists(&save_path) {
        if let Some(mut file) = godot::classes::FileAccess::open(&save_path, godot::classes::file_access::ModeFlags::READ) {
            let len = file.get_length() as i64;
            let buffer = file.get_buffer(len);
            sys.load_sram(buffer.as_slice());
            godot_print!("SRAM loaded from {save_path}");
        }
    }
}

fn save_battery(sys: &mut dyn System) {
    if !sys.is_sram_dirty() {
        return;
    }
    let Some(sram) = sys.get_sram() else { return; };
    let sram = sram.to_vec();
    let dir = sys.save_battery_path().to_string();
    if !godot::classes::DirAccess::dir_exists_absolute(&dir) {
        godot::classes::DirAccess::make_dir_recursive_absolute(&dir);
    }
    let save_path = format!("{}/{}.sav", dir, sys.save_filename());
    if let Some(mut file) = godot::classes::FileAccess::open(&save_path, godot::classes::file_access::ModeFlags::WRITE) {
        file.store_buffer(&PackedByteArray::from(&sram[..]));
        file.flush();
        sys.clear_sram_dirty();
        godot_print!("SRAM saved to {save_path}");
    } else {
        godot_print!("Couldn't open {save_path} for saving");
    }
}