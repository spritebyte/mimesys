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
            visible_y: 0,
            visible_width: 256,
            visible_height: 240,
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