//! PlayStation GameShark / Action Replay codes (the two share one format): TTAAAAAA VVVV,
//! a type byte, a 24-bit RAM address and a 16-bit value. Applied once a frame (at
//! VBlank), like the cartridge's interrupt hook.
//!
//! - 80 / 30: write 16 / 8 bits.
//! - 10 / 11, 20 / 21: add / subtract to 16 / 8 bits (every frame).
//! - D0-D3 (16 bits) and E0-E3 (8 bits): if equal / not equal / less / greater, do the
//!   next code.
//! - D4 0000 VVVV: if pad 1's buttons are exactly VVVV, do the next code. GameShark's
//!   button values: 0001 L2, 0002 R2, 0004 L1, 0008 R1, 0010 Triangle, 0020 Circle,
//!   0040 Cross, 0080 Square, 0100 Select, 0800 Start, 1000 Up, 2000 Right, 4000 Down,
//!   8000 Left (the pad's two bytes swapped, pressed = 1).
//! - D5 / D6 0000 VVVV: on those buttons, turn all codes on / off.
//! - C0: if 16 bits equal, do the rest of this cheat; C1 0000 VVVV: wait VVVV frames
//!   before the rest; C2 AAAAAA NNNN + 80 BBBBBB 0000: copy NNNN bytes from A to B.
//! - 50 00XXYY ZZZZ + a write code: XX writes, each YY further on and ZZZZ more.
//!
//! One cheat can hold several codes (lines). Separators don't matter: the text is read as
//! 12-hex-digit codes, so lines pasted into one box (which drops the newlines) still work.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Code {
    kind: u8,
    addr: u32,
    value: u16,
}

#[derive(Clone, Default)]
struct Cheat {
    codes: Vec<Code>,
    /// frames left before a C1 delay ends (counted from when the cheat was switched on)
    frames: u32,
}

#[derive(Clone)]
pub struct Cheats {
    list: Vec<Cheat>,
    /// D5 / D6 switch everything on / off
    on: bool,
}

impl Default for Cheats {
    fn default() -> Self { Self { list: Vec::new(), on: true } }
}

const KNOWN: [u8; 22] = [
    0x80, 0x30, 0x10, 0x11, 0x20, 0x21, 0xD0, 0xD1, 0xD2, 0xD3, 0xE0, 0xE1, 0xE2, 0xE3,
    0xD4, 0xD5, 0xD6, 0xC0, 0xC1, 0xC2, 0x50, 0x00,
];

fn parse(text: &str) -> Result<Vec<Code>, String> {
    let hex: String = text.chars().filter(|c| !c.is_whitespace() && !matches!(c, '+' | ',' | '-' | ':')).collect();
    if hex.is_empty() { return Err("empty code".into()); }
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("expected GameShark codes like 8009C6E4 03E7 (hex digits only)".into());
    }
    if hex.len() % 12 != 0 {
        return Err(format!("{} hex digits: each code is 12 (AAAAAAAA VVVV)", hex.len()));
    }
    let mut out = Vec::new();
    for c in hex.as_bytes().chunks(12) {
        let s = std::str::from_utf8(c).unwrap();
        let kind = u8::from_str_radix(&s[0..2], 16).unwrap();
        let addr = u32::from_str_radix(&s[2..8], 16).unwrap();
        let value = u16::from_str_radix(&s[8..12], 16).unwrap();
        if !KNOWN.contains(&kind) || kind == 0x00 {
            return Err(format!("{}: code type {kind:02X} isn't supported", &s[..8]));
        }
        out.push(Code { kind, addr, value });
    }
    // the codes that need one after them
    for (i, c) in out.iter().enumerate() {
        let needs_next = matches!(c.kind, 0xD0..=0xD4 | 0xE0..=0xE3 | 0xC2 | 0x50);
        if needs_next && i + 1 == out.len() {
            return Err(format!("{:02X}{:06X}: needs a code after it", c.kind, c.addr));
        }
        if c.kind == 0x50 && !matches!(out[i + 1].kind, 0x80 | 0x30) {
            return Err("a 50 (slide) code must be followed by an 80 or 30 code".into());
        }
    }
    Ok(out)
}

impl Cheats {
    /// The cheats that are on; one error per entry ("" if it's fine)
    pub fn set(&mut self, codes: &[String]) -> Vec<String> {
        self.list.clear();
        self.on = true;
        let mut errors = Vec::new();
        for text in codes {
            match parse(text) {
                Ok(codes) => {
                    let frames = codes.iter().find(|c| c.kind == 0xC1).map_or(0, |c| c.value as u32);
                    self.list.push(Cheat { codes, frames });
                    errors.push(String::new());
                }
                Err(e) => errors.push(e),
            }
        }
        errors
    }

    pub fn is_empty(&self) -> bool { self.list.is_empty() }

    /// Once a frame: `buttons` is pad 1, pressed = 1 (bit 0 Select ... bit 15 Square)
    pub fn apply(&mut self, ram: &mut [u8], buttons: u16) {
        // in GameShark's order: the shoulder / shape byte first
        let buttons = buttons.swap_bytes();
        let m = ram.len() - 1;
        let r8 = |ram: &[u8], a: u32| ram[a as usize & m];
        let r16 = |ram: &[u8], a: u32| {
            let a = a as usize & m & !1;
            u16::from_le_bytes([ram[a], ram[a + 1]])
        };
        let w8 = |ram: &mut [u8], a: u32, v: u8| ram[a as usize & m] = v;
        let w16 = |ram: &mut [u8], a: u32, v: u16| {
            let a = a as usize & m & !1;
            ram[a..a + 2].copy_from_slice(&v.to_le_bytes());
        };
        // D5 / D6 first: they work even while everything is off
        for cheat in &self.list {
            for c in &cheat.codes {
                if c.kind == 0xD5 && buttons == c.value { self.on = true; }
                if c.kind == 0xD6 && buttons == c.value { self.on = false; }
            }
        }
        if !self.on { return; }
        for cheat in &mut self.list {
            let codes = &cheat.codes;
            let mut i = 0;
            while i < codes.len() {
                let c = codes[i];
                i += 1;
                let (a, v) = (c.addr, c.value);
                match c.kind {
                    0x80 => w16(ram, a, v),
                    0x30 => w8(ram, a, v as u8),
                    0x10 => { let x = r16(ram, a).wrapping_add(v); w16(ram, a, x) }
                    0x11 => { let x = r16(ram, a).wrapping_sub(v); w16(ram, a, x) }
                    0x20 => { let x = r8(ram, a).wrapping_add(v as u8); w8(ram, a, x) }
                    0x21 => { let x = r8(ram, a).wrapping_sub(v as u8); w8(ram, a, x) }
                    0xD0..=0xD3 | 0xE0..=0xE3 | 0xD4 => {
                        let (x, want) = match c.kind {
                            0xD4 => (buttons, v),
                            0xE0..=0xE3 => (r8(ram, a) as u16, v & 0xFF),
                            _ => (r16(ram, a), v),
                        };
                        let ok = match c.kind & 0x0F {
                            0 | 4 => x == want,
                            1 => x != want,
                            2 => x < want,
                            _ => x > want,
                        };
                        if !ok { i += 1; }               // skip the next code
                    }
                    0xD5 | 0xD6 => {}
                    0xC0 => if r16(ram, a) != v { break; },
                    0xC1 => {
                        if cheat.frames > 0 { cheat.frames -= 1; break; }
                    }
                    0xC2 => {
                        let dst = codes[i].addr;
                        i += 1;
                        for k in 0..v as u32 {
                            let b = r8(ram, a + k);
                            w8(ram, dst + k, b);
                        }
                    }
                    0x50 => {
                        let next = codes[i];
                        i += 1;
                        let count = (a >> 8) & 0xFF;
                        let step = a & 0xFF;
                        let mut addr = next.addr;
                        let mut val = next.value;
                        for _ in 0..count {
                            if next.kind == 0x80 { w16(ram, addr, val) } else { w8(ram, addr, val as u8) }
                            addr = addr.wrapping_add(step);
                            val = val.wrapping_add(v);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(codes: &[&str], ram: &mut [u8], buttons: u16) -> Vec<String> {
        let mut c = Cheats::default();
        let e = c.set(&codes.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        c.apply(ram, buttons);
        e
    }

    #[test]
    fn writes_and_conditions() {
        let mut ram = vec![0u8; 0x20_0000];
        let e = run(&["8009C6E4 03E7", "300A0000 0042", "D0001000 0000 80001002 BEEF", "D0001000 0001 80001004 BEEF"], &mut ram, 0);
        assert!(e.iter().all(|s| s.is_empty()), "{e:?}");
        assert_eq!(&ram[0x9C6E4..0x9C6E6], &[0xE7, 0x03]);
        assert_eq!(ram[0xA0000], 0x42);
        assert_eq!(&ram[0x1002..0x1004], &[0xEF, 0xBE], "condition true");
        assert_eq!(&ram[0x1004..0x1006], &[0, 0], "condition false: skipped");
    }

    #[test]
    fn pasted_lines_slide_copy_joker() {
        let mut ram = vec![0u8; 0x20_0000];
        // two lines pasted into one box: the newline is gone
        let e = run(&["50000302 00018000200000010"], &mut ram, 0);
        assert!(!e[0].is_empty(), "25 digits");
        let e = run(&["5000030200018000200000108000300000AB"], &mut ram, 0);
        assert!(e[0].is_empty(), "{e:?}");
        assert_eq!(&ram[0x2000..0x2006], &[0x10, 0, 0x11, 0, 0x12, 0], "slide: 3 words, +2, +1");
        assert_eq!(&ram[0x3000..0x3002], &[0xAB, 0]);
        ram[0x4000..0x4004].copy_from_slice(&[1, 2, 3, 4]);
        run(&["C2004000 0004 80005000 0000"], &mut ram, 0);
        assert_eq!(&ram[0x5000..0x5004], &[1, 2, 3, 4]);
        // joker: only with L1 + R1 held (GameShark 000C; pad bits 10 / 11)
        run(&["D4000000 000C 30006000 0001"], &mut ram, 0x0800);
        assert_eq!(ram[0x6000], 0);
        run(&["D4000000 000C 30006000 0001"], &mut ram, 0x0C00);
        assert_eq!(ram[0x6000], 1);
        // Start (GameShark 0800; pad bit 3)
        run(&["D4000000 0800 30006001 0001"], &mut ram, 0x0008);
        assert_eq!(ram[0x6001], 1);
        assert!(!run(&["99001000 0000"], &mut ram, 0)[0].is_empty(), "unknown type");
        assert!(!run(&["D0001000 0000"], &mut ram, 0)[0].is_empty(), "condition with nothing after");
    }
}
