//! Capcom "Kabuki" Z80 decryption. A port of MAME capcom/kabuki.cpp (BSD-3-Clause,
//! Nicola Salmoria).
//!
//! The Kabuki is a Z80 with a key in battery-backed RAM. Each byte is decoded with bit
//! swaps of neighbouring bits, rotations and an XOR; which swaps apply depends on the
//! address and on whether the byte is fetched as an opcode or as data. On CPS1 only the
//! first 32K (the fixed ROM) is encrypted.

/// The four CPS1 Q-Sound keys: (swap_key1, swap_key2, addr_key, xor_key).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Key { Wof, Dino, Punisher, Slammast }

impl Key {
    fn values(self) -> (u32, u32, u32, u8) {
        match self {
            Key::Wof => (0x01234567, 0x54163072, 0x5151, 0x51),
            Key::Dino => (0x76543210, 0x24601357, 0x4343, 0x43),
            Key::Punisher => (0x67452103, 0x75316024, 0x2222, 0x22),
            Key::Slammast => (0x54321076, 0x65432107, 0x3131, 0x19),
        }
    }
}

fn swap_pair(src: u8, pair: u32) -> u8 {
    let lo = 1u8 << (pair * 2);
    let hi = lo << 1;
    (src & !(lo | hi)) | ((src & lo) << 1) | ((src & hi) >> 1)
}

fn bitswap1(mut src: u8, key: u32, select: u32) -> u8 {
    for pair in 0..4 {
        if select & (1 << ((key >> (pair * 4)) & 7)) != 0 { src = swap_pair(src, pair); }
    }
    src
}

fn bitswap2(mut src: u8, key: u32, select: u32) -> u8 {
    for pair in 0..4 {
        if select & (1 << ((key >> ((3 - pair) * 4)) & 7)) != 0 { src = swap_pair(src, pair); }
    }
    src
}

fn bytedecode(src: u8, k1: u32, k2: u32, xor: u8, select: u32) -> u8 {
    let mut s = bitswap1(src, k1 & 0xFFFF, select & 0xFF);
    s = s.rotate_left(1);
    s = bitswap2(s, k1 >> 16, select & 0xFF);
    s ^= xor;
    s = s.rotate_left(1);
    s = bitswap2(s, k2 & 0xFFFF, (select >> 8) & 0xFF);
    s = s.rotate_left(1);
    bitswap1(s, k2 >> 16, (select >> 8) & 0xFF)
}

/// Encrypts one byte (tests): the byte that decodes to `plain` at `addr` as an opcode
/// (`opcode` true) or as data.
#[cfg(test)]
pub fn encrypt_byte(plain: u8, addr: u16, opcode: bool, key: Key) -> u8 {
    let (k1, k2, addr_key, xor) = key.values();
    let a = addr as u32;
    let select = if opcode { (a + addr_key) & 0xFFFF } else { ((a ^ 0x1FC0) + addr_key + 1) & 0xFFFF };
    (0..=255u8).find(|&b| bytedecode(b, k1, k2, xor, select) == plain).expect("bijection")
}

/// Decrypts the first 32K of `rom`: returns the opcode view, and replaces `rom`'s first
/// 32K with the data view (MAME cps1_decode).
pub fn decode(rom: &mut [u8], key: Key) -> Vec<u8> {
    let (k1, k2, addr_key, xor) = key.values();
    let n = rom.len().min(0x8000);
    let mut ops = vec![0u8; 0x8000];
    for a in 0..n {
        let src = rom[a];
        let a32 = a as u32;
        ops[a] = bytedecode(src, k1, k2, xor, (a32 + addr_key) & 0xFFFF);
        rom[a] = bytedecode(src, k1, k2, xor, ((a32 ^ 0x1FC0) + addr_key + 1) & 0xFFFF);
    }
    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The C original, transcribed literally, as a reference for the loop form above.
    fn bitswap1_ref(mut src: i32, key: i32, select: i32) -> i32 {
        if select & (1 << ((key >> 0) & 7)) != 0 { src = (src & 0xfc) | ((src & 0x01) << 1) | ((src & 0x02) >> 1); }
        if select & (1 << ((key >> 4) & 7)) != 0 { src = (src & 0xf3) | ((src & 0x04) << 1) | ((src & 0x08) >> 1); }
        if select & (1 << ((key >> 8) & 7)) != 0 { src = (src & 0xcf) | ((src & 0x10) << 1) | ((src & 0x20) >> 1); }
        if select & (1 << ((key >> 12) & 7)) != 0 { src = (src & 0x3f) | ((src & 0x40) << 1) | ((src & 0x80) >> 1); }
        src
    }
    fn bitswap2_ref(mut src: i32, key: i32, select: i32) -> i32 {
        if select & (1 << ((key >> 12) & 7)) != 0 { src = (src & 0xfc) | ((src & 0x01) << 1) | ((src & 0x02) >> 1); }
        if select & (1 << ((key >> 8) & 7)) != 0 { src = (src & 0xf3) | ((src & 0x04) << 1) | ((src & 0x08) >> 1); }
        if select & (1 << ((key >> 4) & 7)) != 0 { src = (src & 0xcf) | ((src & 0x10) << 1) | ((src & 0x20) >> 1); }
        if select & (1 << ((key >> 0) & 7)) != 0 { src = (src & 0x3f) | ((src & 0x40) << 1) | ((src & 0x80) >> 1); }
        src
    }
    fn bytedecode_ref(src: i32, k1: i32, k2: i32, xor: i32, select: i32) -> i32 {
        let mut s = bitswap1_ref(src, k1 & 0xffff, select & 0xff);
        s = ((s & 0x7f) << 1) | ((s & 0x80) >> 7);
        s = bitswap2_ref(s, k1 >> 16, select & 0xff);
        s ^= xor;
        s = ((s & 0x7f) << 1) | ((s & 0x80) >> 7);
        s = bitswap2_ref(s, k2 & 0xffff, select >> 8);
        s = ((s & 0x7f) << 1) | ((s & 0x80) >> 7);
        bitswap1_ref(s, k2 >> 16, select >> 8)
    }

    #[test]
    fn matches_the_c_original_for_every_key_byte_and_address() {
        for key in [Key::Wof, Key::Dino, Key::Punisher, Key::Slammast] {
            let (k1, k2, ak, x) = key.values();
            let mut rom: Vec<u8> = (0..0x8000u32).map(|a| (a * 37 + (a >> 8)) as u8).collect();
            let orig = rom.clone();
            let ops = decode(&mut rom, key);
            for a in 0..0x8000i32 {
                let s = orig[a as usize] as i32;
                let op = bytedecode_ref(s, k1 as i32, k2 as i32, x as i32, a + ak as i32);
                // MAME passes the full sum; only bits 0-15 are looked at
                let data = bytedecode_ref(s, k1 as i32, k2 as i32, x as i32, ((a ^ 0x1fc0) + ak as i32 + 1) & 0xFFFF);
                assert_eq!(ops[a as usize] as i32, op & 0xFF, "{key:?} op {a:04x}");
                assert_eq!(rom[a as usize] as i32, data & 0xFF, "{key:?} data {a:04x}");
            }
        }
    }

    #[test]
    fn decoding_is_a_permutation_of_byte_values() {
        // for a fixed address and fetch type the cipher is a bijection on bytes
        let (k1, k2, _, x) = Key::Dino.values();
        let mut seen = [false; 256];
        for b in 0..=255u8 { seen[bytedecode(b, k1, k2, x, 0x1234) as usize] = true; }
        assert!(seen.iter().all(|&s| s));
        // the encryption of 00 doesn't depend on the high select byte (MAME's "weakness" note)
        let enc0 = |sel: u32| (0..=255u8).find(|&b| bytedecode(b, k1, k2, x, sel) == 0).unwrap();
        assert_eq!(enc0(0x0034), enc0(0xFF34));
        assert_eq!(enc0(0x0034), enc0(0x5A34));
    }
}
