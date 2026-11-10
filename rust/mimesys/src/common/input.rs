pub const UP: u32     = 1 << 0;
pub const DOWN: u32   = 1 << 1;
pub const LEFT: u32   = 1 << 2;
pub const RIGHT: u32  = 1 << 3;
pub const B: u32      = 1 << 4;
pub const A: u32      = 1 << 5;
pub const SELECT: u32 = 1 << 6;
pub const START: u32  = 1 << 7;
pub const KEYPAD_SHIFT: u32 = 8;   // 4-bit digit, 0x0F = none
pub const COIN: u32   = 1 << 16;
pub const START2: u32 = 1 << 17;
/// Third button (arcade games with 3 buttons), player 1 and player 2.
pub const X: u32      = 1 << 18;
pub const P2_X: u32   = 1 << 19;
/// Shoulder buttons (GBA L / R).
pub const L: u32      = 1 << 20;
pub const R: u32      = 1 << 21;
/// Buttons 4 and 5 (Defender's Hyperspace and Reverse).
pub const Y: u32      = 1 << 22;
pub const Z: u32      = 1 << 23;
/// Right stick, for twin-stick games (Robotron, Splat). Bits 12-15 sit above the keypad
/// nibble.
pub const RS_UP: u32    = 1 << 12;
pub const RS_DOWN: u32  = 1 << 13;
pub const RS_LEFT: u32  = 1 << 14;
pub const RS_RIGHT: u32 = 1 << 15;
/// Player 2 occupies bits 24-31 in the same order as player 1 (UP..START).
pub const P2_SHIFT: u32 = 24;

/// Player 2's buttons moved down into player 1's bit positions, so the constants above
/// can be used on it.
pub fn player2(mask: u32) -> u32 {
    (mask >> P2_SHIFT) & 0xFF
}
// ---------------------------------------------------------------- extra input word
// EmuSystem.set_extra_input(ext): buttons and players that don't fit the main mask.
/// Player 2 buttons 4-6 and the shoulder buttons (P2 Y, Z, L, R).
pub const EXT_P2_Y: u32 = 1 << 0;
pub const EXT_P2_Z: u32 = 1 << 1;
pub const EXT_P2_L: u32 = 1 << 2;
pub const EXT_P2_R: u32 = 1 << 3;
/// Players 3 and 4: one byte each, UP DOWN LEFT RIGHT B A X Y (bits 0-7): the low six bits
/// are player 1's UP..A, then X and Y.
pub const EXT_PAD_X: u32 = 1 << 6;
pub const EXT_PAD_Y: u32 = 1 << 7;
pub const EXT_P3_SHIFT: u32 = 8;
pub const EXT_P4_SHIFT: u32 = 16;
pub const EXT_P3_START: u32 = 1 << 24;
pub const EXT_P3_COIN: u32  = 1 << 25;
pub const EXT_P4_START: u32 = 1 << 26;
pub const EXT_P4_COIN: u32  = 1 << 27;

/// One player's controls, from the main mask and the extra word.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pad {
    pub up: bool, pub down: bool, pub left: bool, pub right: bool,
    pub b: bool, pub a: bool, pub x: bool, pub y: bool, pub z: bool, pub l: bool, pub r: bool,
    pub start: bool,
    /// the player's coin slot (Select on pads 1 / 2; pad 1's Coin too)
    pub coin: bool,
}

/// Pads 1-4. Player 2's Start is also P1's START2 bit; players 3 / 4 have no Z / L / R.
pub fn pads(mask: u32, ext: u32) -> [Pad; 4] {
    let on = |m: u32, b: u32| m & b != 0;
    let p2 = player2(mask);
    let ext_pad = |byte: u32, start: bool, coin: bool| Pad {
        up: on(byte, UP), down: on(byte, DOWN), left: on(byte, LEFT), right: on(byte, RIGHT),
        b: on(byte, B), a: on(byte, A), x: on(byte, EXT_PAD_X), y: on(byte, EXT_PAD_Y),
        z: false, l: false, r: false, start, coin,
    };
    [
        Pad {
            up: on(mask, UP), down: on(mask, DOWN), left: on(mask, LEFT), right: on(mask, RIGHT),
            b: on(mask, B), a: on(mask, A), x: on(mask, X), y: on(mask, Y), z: on(mask, Z),
            l: on(mask, L), r: on(mask, R), start: on(mask, START),
            coin: on(mask, SELECT) || on(mask, COIN),
        },
        Pad {
            up: on(p2, UP), down: on(p2, DOWN), left: on(p2, LEFT), right: on(p2, RIGHT),
            b: on(p2, B), a: on(p2, A), x: on(mask, P2_X), y: on(ext, EXT_P2_Y), z: on(ext, EXT_P2_Z),
            l: on(ext, EXT_P2_L), r: on(ext, EXT_P2_R), start: on(p2, START) || on(mask, START2),
            coin: on(p2, SELECT),
        },
        ext_pad((ext >> EXT_P3_SHIFT) & 0xFF, on(ext, EXT_P3_START), on(ext, EXT_P3_COIN)),
        ext_pad((ext >> EXT_P4_SHIFT) & 0xFF, on(ext, EXT_P4_START), on(ext, EXT_P4_COIN)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pads_split_the_main_mask_and_the_extra_word() {
        let p = pads(UP | SELECT | (A | START) << P2_SHIFT | P2_X,
                     EXT_P2_Y | EXT_P2_R | (DOWN | EXT_PAD_Y) << EXT_P3_SHIFT | EXT_P3_COIN | RIGHT << EXT_P4_SHIFT | EXT_P4_START);
        assert_eq!(p[0], Pad { up: true, coin: true, ..Pad::default() });
        assert_eq!(p[1], Pad { a: true, start: true, x: true, y: true, r: true, ..Pad::default() });
        assert_eq!(p[2], Pad { down: true, y: true, coin: true, ..Pad::default() });
        assert_eq!(p[3], Pad { right: true, start: true, ..Pad::default() });
        assert!(pads(START2, 0)[1].start, "pad 1's P2 Start bit");
        assert!(pads(COIN, 0)[0].coin);
    }
}
