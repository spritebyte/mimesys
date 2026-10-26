#[derive(Default)]
pub enum Rotation {
    #[default] None =  0,
    CW90 =  1,
    CCW90 = 2,
}

#[derive(Default)]
pub struct DisplayInfo {
       pub buffer_width: i32,
       pub buffer_height: i32,
       pub visible_x: i32, pub visible_y: i32,
       pub visible_width: i32, pub visible_height: i32,
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
    fn has_battery(&self) -> bool;
    fn save_battery_path(&self) -> &str;
    fn save_filename(&self) -> &str;
    fn save_state_path(&self) -> &str;
    fn power_on(&mut self);
    fn power_off(&mut self);
    fn is_sram_dirty(&self) -> bool;
    fn clear_sram_dirty(&mut self);
    fn get_current_frame(&self) -> i64;
    fn set_audio_rate_adjust(&mut self, _factor: f32) {}
    fn cheats_mut(&mut self) -> Option<&mut crate::common::cheats::Cheats> { None }
    fn set_option(&mut self, _key: &str, _value: i64) {}
    /// Pointer for light guns: position in framebuffer pixels (-1 = off screen) and mouse
    /// buttons (bit 0 left, bit 1 right). Systems without one ignore it.
    fn set_pointer(&mut self, _x: i32, _y: i32, _buttons: u32) {}
    fn link_partner(&mut self) -> Option<&mut dyn System> { None }
    fn attach_cable(&mut self, _cable: Option<Box<dyn crate::gameboy::gb_link::LinkCable>>) -> bool { false }
    /// Plug a network link cable in (None = unplug). Systems with a link port that isn't
    /// the Game Boy's (GBA) override this; the default hands it to `attach_cable`.
    fn attach_net(&mut self, link: Option<crate::gameboy::gb_netlink::NetLink>) -> bool {
        self.attach_cable(link.map(|l| Box::new(l) as Box<dyn crate::gameboy::gb_link::LinkCable>))
    }
    /// The cable's status line for the UI ("" = no cable).
    fn cable_status(&self) -> String { String::new() }
    /// Debug trace lines collected since the last call (GBA link log; "link_trace" option).
    fn take_trace(&mut self) -> Vec<String> { Vec::new() }    
}