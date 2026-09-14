#[derive(Default)]
pub enum Rotation {
    #[default] None =  0,
    CW90 =  1,
    CCW90 = 2,
}

#[derive(Default)]
pub struct DisplayInfo {
       pub buffer_width: u32,
       pub buffer_height: u32,
       pub visible_x: u32, pub visible_y: u32,
       pub visible_width: u32, pub visible_height: u32,
       pub target_aspect_ratio: f32,
       pub rotation: Rotation,   // plain Rust enum, fine here
}

pub trait System: Send {
    fn run_frame(&mut self, input: u32);      // advance one frame
    fn framebuffer(&self) -> &[u8];           // RGBA out
    fn display_info(&self) -> DisplayInfo;     // system-specific screen size
    fn drain_audio(&mut self) -> Vec<f32>;
    fn sample_rate(&self) -> u32;
    fn set_input(&mut self, input: u32);
    fn save_state(&self) -> Vec<u8>;
    fn load_state(&mut self, data: &[u8]) -> Result<(), String>;
    fn load_sram(&mut self, data: &[u8]);
    fn get_sram(&self) -> Option<&[u8]>;
    fn reset(&mut self);
//    fn set_frame_published(&mut self, is_frame_published: bool);
    fn has_battery(&self) -> bool;
    fn save_battery_path(&self) -> &str;
    fn save_filename(&self) -> &str;
    fn power_on(&mut self);
    fn power_off(&mut self);
    fn is_sram_dirty(&self) -> bool;
    fn clear_sram_dirty(&mut self);
    fn get_current_frame(&self) -> i64;
//    fn is_frame_ready(&self) -> bool;
}