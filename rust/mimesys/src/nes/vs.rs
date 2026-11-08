//! Nintendo VS System (VS UniSystem): the arcade NES. One NES-like board per screen with
//! coin slots, a service button, eight DIP switches and RGB PPUs:
//!
//! - 2C03: the NES's colours, sent as RGB.
//! - 2C04-0001..0004: the same 64 colours in four different scrambled orders (copy
//!   protection: a game burned for one board shows wrong colours on another).
//! - 2C05-01..05: 2C03 colours, but PPUCTRL and PPUMASK swap addresses and PPUSTATUS
//!   returns an ID in its low 5 bits. A game on the wrong one never turns NMI on.
//!
//! Inputs (NESdev wiki, "Vs. System"):
//!   $4016 read: bit 0 right stick (A, B, button 1, button 3, Up, Down, Left, Right),
//!               bit 2 service, bits 3-4 DIP 1-2, bits 5-6 coins, bit 7 0 (primary CPU)
//!   $4017 read: bit 0 left stick (A, B, button 2, button 4, ...), bits 2-7 DIP 3-8
//! Buttons 1 and 2 are the cabinet's 1P and 2P start buttons. Most games put player 1 on
//! the right stick ($4016), some on the left.
//!
//! Colour tables from the NESdev wiki, "PPU palettes" (3 bits per channel).

use serde::{Deserialize, Serialize};

/// The PPU, numbered as in the NES 2.0 header (byte 13, low nibble).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum VsPpu {
    Rp2c03b, Rp2c03g, Rp2c04_0001, Rp2c04_0002, Rp2c04_0003, Rp2c04_0004,
    Rc2c03b, Rc2c03c, Rc2c05_01, Rc2c05_02, Rc2c05_03, Rc2c05_04, Rc2c05_05,
}

impl VsPpu {
    pub fn from_nes2(n: u8) -> Option<Self> {
        use VsPpu::*;
        Some(match n {
            0 => Rp2c03b, 1 => Rp2c03g, 2 => Rp2c04_0001, 3 => Rp2c04_0002, 4 => Rp2c04_0003,
            5 => Rp2c04_0004, 6 => Rc2c03b, 7 => Rc2c03c, 8 => Rc2c05_01, 9 => Rc2c05_02,
            10 => Rc2c05_03, 11 => Rc2c05_04, 12 => Rc2c05_05,
            _ => return None,
        })
    }
    pub fn name(self) -> &'static str {
        use VsPpu::*;
        match self {
            Rp2c03b => "RP2C03B", Rp2c03g => "RP2C03G", Rp2c04_0001 => "RP2C04-0001",
            Rp2c04_0002 => "RP2C04-0002", Rp2c04_0003 => "RP2C04-0003", Rp2c04_0004 => "RP2C04-0004",
            Rc2c03b => "RC2C03B", Rc2c03c => "RC2C03C", Rc2c05_01 => "RC2C05-01",
            Rc2c05_02 => "RC2C05-02", Rc2c05_03 => "RC2C05-03", Rc2c05_04 => "RC2C05-04",
            Rc2c05_05 => "RC2C05-05",
        }
    }
    /// Colour table: 0 = 2C03 (also 2C05), 1-4 = 2C04-0001..0004.
    pub fn palette(self) -> usize {
        use VsPpu::*;
        match self { Rp2c04_0001 => 1, Rp2c04_0002 => 2, Rp2c04_0003 => 3, Rp2c04_0004 => 4, _ => 0 }
    }
    /// 2C05: PPUCTRL and PPUMASK trade places.
    pub fn swaps_ctrl_mask(self) -> bool {
        matches!(self, VsPpu::Rc2c05_01 | VsPpu::Rc2c05_02 | VsPpu::Rc2c05_03 | VsPpu::Rc2c05_04 | VsPpu::Rc2c05_05)
    }
    /// 2C05: the constant in PPUSTATUS bits 0-4. (Values as other emulators use them; the
    /// NESdev wiki says only that they're constant and differ between versions.)
    pub fn status_id(self) -> Option<u8> {
        use VsPpu::*;
        match self { Rc2c05_01 => Some(0x1B), Rc2c05_02 => Some(0x3D), Rc2c05_03 => Some(0x1C), Rc2c05_04 => Some(0x1B),
                     Rc2c05_05 => Some(0x00), _ => None }
    }
}

/// 3-bit channel to 8 bits.
const fn c8(x: u16) -> u8 { ((x as u32 * 255 + 3) / 7) as u8 }

const fn expand(t: [u16; 64]) -> [(u8, u8, u8); 64] {
    let mut out = [(0u8, 0u8, 0u8); 64];
    let mut i = 0;
    while i < 64 {
        let v = t[i];
        out[i] = (c8(v >> 8 & 7), c8(v >> 4 & 7), c8(v & 7));
        i += 1;
    }
    out
}

/// 0x0RGB with 3 bits per channel, as the wiki writes them ("637" = 0x637).
const RGB_2C03: [u16; 64] = [
    0x333, 0x014, 0x006, 0x326, 0x403, 0x503, 0x510, 0x420, 0x320, 0x120, 0x031, 0x040, 0x022, 0x000, 0x000, 0x000,
    0x555, 0x036, 0x027, 0x407, 0x507, 0x704, 0x700, 0x630, 0x430, 0x140, 0x040, 0x053, 0x044, 0x000, 0x000, 0x000,
    0x777, 0x357, 0x447, 0x637, 0x707, 0x737, 0x740, 0x750, 0x660, 0x360, 0x070, 0x276, 0x077, 0x000, 0x000, 0x000,
    0x777, 0x567, 0x657, 0x757, 0x747, 0x755, 0x764, 0x772, 0x773, 0x572, 0x473, 0x276, 0x467, 0x000, 0x000, 0x000,
];
const RGB_2C04_0001: [u16; 64] = [
    0x755, 0x637, 0x700, 0x447, 0x044, 0x120, 0x222, 0x704, 0x777, 0x333, 0x750, 0x503, 0x403, 0x660, 0x320, 0x777,
    0x357, 0x653, 0x310, 0x360, 0x467, 0x657, 0x764, 0x027, 0x760, 0x276, 0x000, 0x200, 0x666, 0x444, 0x707, 0x014,
    0x003, 0x567, 0x757, 0x070, 0x077, 0x022, 0x053, 0x507, 0x000, 0x420, 0x747, 0x510, 0x407, 0x006, 0x740, 0x000,
    0x000, 0x140, 0x555, 0x031, 0x572, 0x326, 0x770, 0x630, 0x020, 0x036, 0x040, 0x111, 0x773, 0x737, 0x430, 0x473,
];
const RGB_2C04_0002: [u16; 64] = [
    0x000, 0x750, 0x430, 0x572, 0x473, 0x737, 0x044, 0x567, 0x700, 0x407, 0x773, 0x747, 0x777, 0x637, 0x467, 0x040,
    0x020, 0x357, 0x510, 0x666, 0x053, 0x360, 0x200, 0x447, 0x222, 0x707, 0x003, 0x276, 0x657, 0x320, 0x000, 0x326,
    0x403, 0x764, 0x740, 0x757, 0x036, 0x310, 0x555, 0x006, 0x507, 0x760, 0x333, 0x120, 0x027, 0x000, 0x660, 0x777,
    0x653, 0x111, 0x070, 0x630, 0x022, 0x014, 0x704, 0x140, 0x000, 0x077, 0x420, 0x770, 0x755, 0x503, 0x031, 0x444,
];
const RGB_2C04_0003: [u16; 64] = [
    0x507, 0x737, 0x473, 0x555, 0x040, 0x777, 0x567, 0x120, 0x014, 0x000, 0x764, 0x320, 0x704, 0x666, 0x653, 0x467,
    0x447, 0x044, 0x503, 0x027, 0x140, 0x430, 0x630, 0x053, 0x333, 0x326, 0x000, 0x006, 0x700, 0x510, 0x747, 0x755,
    0x637, 0x020, 0x003, 0x770, 0x111, 0x750, 0x740, 0x777, 0x360, 0x403, 0x357, 0x707, 0x036, 0x444, 0x000, 0x310,
    0x077, 0x200, 0x572, 0x757, 0x420, 0x070, 0x660, 0x222, 0x031, 0x000, 0x657, 0x773, 0x407, 0x276, 0x760, 0x022,
];
const RGB_2C04_0004: [u16; 64] = [
    0x430, 0x326, 0x044, 0x660, 0x000, 0x755, 0x014, 0x630, 0x555, 0x310, 0x070, 0x003, 0x764, 0x770, 0x040, 0x572,
    0x737, 0x200, 0x027, 0x747, 0x000, 0x222, 0x510, 0x740, 0x653, 0x053, 0x447, 0x140, 0x403, 0x000, 0x473, 0x357,
    0x503, 0x031, 0x420, 0x006, 0x407, 0x507, 0x333, 0x704, 0x022, 0x666, 0x036, 0x020, 0x111, 0x773, 0x444, 0x707,
    0x757, 0x777, 0x320, 0x700, 0x760, 0x276, 0x777, 0x467, 0x000, 0x750, 0x637, 0x567, 0x360, 0x657, 0x077, 0x120,
];

/// RGB for each of the five colour tables (VsPpu::palette).
pub static PALETTES: [[(u8, u8, u8); 64]; 5] = [
    expand(RGB_2C03), expand(RGB_2C04_0001), expand(RGB_2C04_0002), expand(RGB_2C04_0003), expand(RGB_2C04_0004),
];

/// What the header says about a VS cartridge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VsHeader { pub ppu: Option<VsPpu>, pub hardware: u8 }

/// VS cartridge? iNES: byte 7 bit 0. NES 2.0: byte 7 bits 0-1 = 1, byte 13 = PPU (low
/// nibble) and board type (high nibble: 0 normal, 1-3 protection, 4 Ice Climber JP,
/// 5-6 DualSystem).
pub fn header(h: &[u8]) -> Option<VsHeader> {
    let nes2 = h[7] & 0x0C == 0x08;
    if nes2 {
        if h[7] & 3 != 1 { return None; }
        return Some(VsHeader { ppu: VsPpu::from_nes2(h[13] & 0x0F), hardware: h[13] >> 4 });
    }
    if h[7] & 1 != 0 { Some(VsHeader { ppu: None, hardware: 0 }) } else { None }
}

/// The PPU for old iNES dumps that don't say, from words in the file name. Only games
/// whose board is well known; anything else gets the 2C03 (and the System Options
/// "VS PPU" choice to fix it).
pub fn ppu_from_name(name: &str) -> Option<VsPpu> {
    let n = name.to_ascii_lowercase().replace(['.', '_', '-', '\''], " ");
    let has = |w: &str| n.contains(w);
    use VsPpu::*;
    Some(if has("super mario") { Rp2c04_0004 }
        else if has("castlevania") { Rp2c04_0002 }
        else if has("excitebike") { Rp2c04_0003 }
        else if has("duck hunt") || has("tennis") { Rp2c03b }
        else if has("top gun") { Rc2c05_04 }
        else if has("mighty bomb jack") { Rc2c05_02 }
        else if has("jajamaru") { Rc2c05_01 }
        else if has("gumshoe") { Rc2c05_03 }
        else { return None })
}

/// Coins, service button, DIP switches and how the sticks are wired.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct VsInput {
    /// DIP switches 1-8 (bit 0 = switch 1)
    pub dips: u8,
    /// player 1 on the left stick ($4017) instead of the right ($4016)
    pub swap: bool,
    /// frames left on the coin signal (a coin reads true for ~40-70 ms)
    coin_frames: u8,
    coin_held: bool,
    service: bool,
    /// the PPU found at load (System Options can override it, and go back)
    pub detected: Option<VsPpu>,
}

pub const COIN_FRAMES: u8 = 3;

impl VsInput {
    /// Per frame: coin button (a press drops one coin), service button.
    pub fn frame(&mut self, coin: bool, service: bool) {
        if self.coin_frames > 0 { self.coin_frames -= 1; }
        if coin && !self.coin_held { self.coin_frames = COIN_FRAMES; }
        self.coin_held = coin;
        self.service = service;
    }
    /// $4016 bits other than the stick: service, DIP 1-2, coin (one acceptor, bit 5).
    pub fn bits_4016(&self) -> u8 {
        (self.service as u8) << 2 | (self.dips & 3) << 3 | ((self.coin_frames > 0) as u8) << 5
    }
    /// $4017 bits other than the stick: DIP 3-8.
    pub fn bits_4017(&self) -> u8 { self.dips & 0xFC }

    /// The two serial reports ($4016, $4017) from player 1 and 2's NES-order pads
    /// (A, B, Select, Start, Up, Down, Left, Right). On the VS a stick has no Start or
    /// Select: those slots are the cabinet's buttons 1-4. Player 1's Start is button 1
    /// (the 1P start, on $4016) and player 2's is button 2 (on $4017), whichever stick
    /// each player uses.
    pub fn reports(&self, p1: u8, p2: u8) -> (u8, u8) {
        let stick = |p: u8| p & 0xF3;
        let (right, left) = if self.swap { (stick(p2), stick(p1)) } else { (stick(p1), stick(p2)) };
        let start1 = if p1 & 0x08 != 0 { 0x04 } else { 0 };
        let start2 = if p2 & 0x08 != 0 { 0x04 } else { 0 };
        (right | start1, left | start2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_agree_with_each_other() {
        // every 2C04 colour that's also a 2C03 colour is the same colour (the 2C04s are
        // re-orderings of one palette); spot-check the wiki's LUT: 2C04-0001 entry 0 is
        // 2C03 entry $35, entry 1 is $23
        assert_eq!(PALETTES[1][0], PALETTES[0][0x35]);
        assert_eq!(PALETTES[1][1], PALETTES[0][0x23]);
        assert_eq!(PALETTES[0][0x20], (255, 255, 255));
        assert_eq!(PALETTES[0][0x0D], (0, 0, 0));
        for t in 1..5 { assert!(PALETTES[t].contains(&(0, 0, 0)) && PALETTES[t].contains(&(255, 255, 255))); }
    }

    #[test]
    fn header_detection() {
        let mut h = [0u8; 16];
        h[7] = 0x01;
        assert_eq!(header(&h), Some(VsHeader { ppu: None, hardware: 0 }), "iNES VS bit");
        h[7] = 0x09; h[13] = 0x15;
        assert_eq!(header(&h), Some(VsHeader { ppu: Some(VsPpu::Rp2c04_0004), hardware: 1 }), "NES 2.0");
        h[7] = 0x08;
        assert_eq!(header(&h), None, "NES 2.0 home console");
        assert_eq!(ppu_from_name("VS. Super Mario Bros. (VS)"), Some(VsPpu::Rp2c04_0004));
        assert_eq!(ppu_from_name("vs_topgun"), None);
        assert_eq!(ppu_from_name("Vs. Top Gun"), Some(VsPpu::Rc2c05_04));
    }

    #[test]
    fn coins_and_sticks() {
        let mut v = VsInput::default();
        v.frame(true, false);
        assert_eq!(v.bits_4016() & 0x20, 0x20, "coin in");
        for _ in 0..5 { v.frame(true, false); }
        assert_eq!(v.bits_4016() & 0x20, 0, "held: still one coin");
        v.frame(false, true);
        v.frame(true, true);
        assert_eq!(v.bits_4016() & 0x24, 0x24, "second coin, service");
        v.dips = 0b1010_0110;
        assert_eq!((v.bits_4016() >> 3 & 3, v.bits_4017()), (0b10, 0b1010_0100));
        // p1: A + Start, p2: Right
        assert_eq!(v.reports(0x09, 0x80), (0x05, 0x80));
        v.swap = true;
        assert_eq!(v.reports(0x09, 0x80), (0x84, 0x01), "sticks swap, start buttons stay");
    }
}
