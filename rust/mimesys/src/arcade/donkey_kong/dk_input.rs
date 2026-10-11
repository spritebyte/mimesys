pub struct Joypad {
    input_mode: u8,
}

impl Joypad {
    pub fn new() -> Self {
        Self {
            input_mode: 0,
        }
    }

    pub fn set_button_state(&mut self, input_mask: u8) {

    }    
}