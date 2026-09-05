pub struct Joypad {
    p1_select: u8, // Bits 4 & 5 written by CPU
    
    // Pressed = true, Released = false
    pub a: bool, pub b: bool, pub select: bool, pub start: bool,
    pub right: bool, pub left: bool, pub up: bool, pub down: bool,
    
    // Latch lower 4 bits output to detect transitions
    last_p1_nibble: u8,
    pub pending_low_transition: bool,
}

impl Joypad {
    pub fn new() -> Self {
        Self {
            p1_select: 0x30, // Default both deselected (bits 4, 5 = 1)
            a: false, b: false, select: false, start: false,
            right: false, left: false, up: false, down: false,
            last_p1_nibble: 0x0F, // All released (high)
            pending_low_transition: false,
        }
    }

    /// Evaluates the lower 4 bits of $FF00 based on current button states and active selection.
    pub fn read_p1_nibble(&self) -> u8 {
        let mut nibble = 0x0F; // Active Low: 1 = unpressed

        // Bit 4 = 0: Direction keys selected
        if (self.p1_select & 0x10) == 0 {
            if self.right { nibble &= !0x01; }
            if self.left  { nibble &= !0x02; }
            if self.up    { nibble &= !0x04; }
            if self.down  { nibble &= !0x08; }
        }

        // Bit 5 = 0: Button keys selected
        if (self.p1_select & 0x20) == 0 {
            if self.a      { nibble &= !0x01; }
            if self.b      { nibble &= !0x02; }
            if self.select { nibble &= !0x04; }
            if self.start  { nibble &= !0x08; }
        }

        nibble
    }

    /// Re-evaluates state and updates interrupt / transition flags.
    pub fn update_state(&mut self, if_reg: &mut u8) {
        let current_nibble = self.read_p1_nibble();

        // Check if any bit transitioned from 1 (released) -> 0 (pressed)
        let transition = (self.last_p1_nibble & !current_nibble) & 0x0F;

        if transition != 0 {
            // 1. Request Joypad Interrupt (Bit 4 of IF register)
            *if_reg |= 0x10;
            // 2. Latch wake-up flag for CPU STOP mode
            self.pending_low_transition = true;
        }

        self.last_p1_nibble = current_nibble;
    }

    /// CPU writes to $FF00 (P1)
    pub fn write_p1(&mut self, val: u8, if_reg: &mut u8) {
        // Only bits 4 and 5 are writable
        self.p1_select = val & 0x30;
        self.update_state(if_reg);
    }

    /// Reads full $FF00 register
    pub fn read_p1(&self) -> u8 {
        0xC0 | self.p1_select | self.read_p1_nibble()
    }

    /// Called from Godot when input events arrive
    pub fn set_button_state(&mut self, input_mask: u8, if_reg: &mut u8) {
    /*    match button_id {
            0 => self.a = pressed,
            1 => self.b = pressed,
            2 => self.select = pressed,
            3 => self.start = pressed,
            4 => self.right = pressed,
            5 => self.left = pressed,
            6 => self.up = pressed,
            7 => self.down = pressed,
            _ => {}
        }
    */
        self.a = if (input_mask & (1 << 5)) != 0 { true } else { false };
        self.b = if (input_mask & (1 << 4)) != 0 { true } else { false };
        self.select = if (input_mask & (1 << 6)) != 0 { true } else { false };
        self.start = if (input_mask & (1 << 7)) != 0 { true } else { false };

        // Directional Buttons (Bits 4..7 -> P10..P13 when P14 is low)
        self.right = if (input_mask & (1 << 3)) != 0 { true } else { false };
        self.left = if (input_mask & (1 << 2)) != 0 { true } else { false };
        self.up = if (input_mask & (1 << 0)) != 0 { true } else { false };
        self.down = if (input_mask & (1 << 1)) != 0 { true } else { false };

        self.update_state(if_reg);
    }
}