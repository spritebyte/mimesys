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
/// Player 2 occupies bits 24-31 in the same order as player 1 (UP..START).
pub const P2_SHIFT: u32 = 24;

/// Player 2's buttons moved down into player 1's bit positions, so the constants above
/// can be used on it.
pub fn player2(mask: u32) -> u32 {
    (mask >> P2_SHIFT) & 0xFF
}