pub struct Joypad {
    input_mode: u8,
    coin_pulse_frames: u8,
    up: bool,
    down: bool,
    left: bool,
    right: bool,
    jump: bool,
}

const P_RIGHT: u8 = 0x01;
const P_LEFT: u8  = 0x02;
const P_UP: u8    = 0x04;
const P_DOWN: u8  = 0x08;
const P_JUMP: u8  = 0x10;

const GD_RIGHT: u8 = 0x08;
const GD_LEFT:  u8 = 0x04;
const GD_UP:    u8 = 0x01;
const GD_DOWN:  u8 = 0x02;
const GD_JUMP:  u8 = 0x10;
const GD_JUMP2: u8 = 0x20;
const GD_COIN:  u8 = 0x40;
const GD_START: u8 = 0x80;

const P_RESET: u8 = 0xE0;

impl Joypad {
    pub fn new() -> Self {
        Self {
            input_mode: 0,
            coin_pulse_frames: 0,
        }
    }

    pub fn set_button_state(&mut self, input_mask: u8) {
        self.up = if input_mask & GD_UP != 0 { true } else { false };
        self.down = if input_mask & GD_DOWN != 0 { true } else { false };
        self.left = if input_mask & GD_LEFT != 0 { true } else { false };
        self.right = if input_mask & GD_RIGHT != 0 { true } else { false };
        self.jump = if (input_mask & GD_JUMP != 0) ||
                    if (input_mask & GD_JUMP2 != 0) { true } else { false };
        
    }

    pub fn read_input() -> u8 {
        let value = 0x00;

        if self.up { value |= P_UP; }
        if self.down { value |= P_DOWN; }
        if self.left { value |= P_LEFT; }
        if self.right { value |= P_RIGHT; }
        if self.jump { value |= P_JUMP; }

        value
    }

    pub fn read_coin_input(&mut self) -> u8 {
        let value = 0x00;

        value
    }
}