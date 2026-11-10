//! CPS2 program decryption. A port of MAME capcom/cps2crypt.cpp (BSD-3-Clause; Andreas
//! Naive, Nicola Salmoria, Charles MacDonald).
//!
//! The 68000's opcode fetches (function code "program") go through a decrypter; data reads
//! don't. Each 16-bit word is decrypted by a 4-round Feistel network keyed by a 64-bit
//! master key mixed with a 64-bit subkey, which itself comes from the word's address
//! (bits 1-16) passed through a first Feistel network. Only addresses up to a per-game
//! upper limit are encrypted.
//!
//! The key comes from the game's "key" file (20 bytes, battery RAM on the real board): 160
//! bits read in a scrambled order give ten 16-bit words: the master key, the watchdog
//! opcode and the encrypted range.

use super::cps2_crypt_tables::*;

const FN1_GROUP_A: [u8; 8] = [10, 4, 6, 7, 2, 13, 15, 14];
const FN1_GROUP_B: [u8; 8] = [0, 1, 3, 5, 8, 9, 11, 12];
const FN2_GROUP_A: [u8; 8] = [6, 0, 2, 13, 1, 4, 14, 7];
const FN2_GROUP_B: [u8; 8] = [3, 5, 9, 10, 8, 15, 12, 11];

#[derive(Clone, Copy)]
struct Opt { input: [u8; 256], output: [u8; 64] }

impl Opt {
    fn new(b: &SBox) -> Self {
        let mut o = Opt { input: [0; 256], output: [0; 64] };
        for v in 0..256usize {
            let mut res = 0u8;
            for (i, &bit) in b.inputs.iter().enumerate() {
                if bit >= 0 { res |= (((v >> bit) & 1) as u8) << i; }
            }
            o.input[v] = res;
        }
        for i in 0..64 {
            let t = b.table[i];
            let mut out = 0u8;
            if t & 1 != 0 { out |= 1 << b.outputs[0]; }
            if t & 2 != 0 { out |= 1 << b.outputs[1]; }
            o.output[i] = out;
        }
        o
    }
    #[inline]
    fn f(&self, v: u8, key: u32) -> u8 { self.output[(self.input[v as usize] ^ (key & 0x3F) as u8) as usize] }
}

fn round(v: u8, b: &[Opt], key: u32) -> u8 {
    b[0].f(v, key) | b[1].f(v, key >> 6) | b[2].f(v, key >> 12) | b[3].f(v, key >> 18)
}

fn opt4(b: &[SBox; 4]) -> [Opt; 4] { [Opt::new(&b[0]), Opt::new(&b[1]), Opt::new(&b[2]), Opt::new(&b[3])] }

fn feistel(val: u16, a: &[u8; 8], b: &[u8; 8], boxes: &[[Opt; 4]; 4], key: &[u32; 4]) -> u16 {
    let bit = |v: u16, n: u8| ((v >> n) & 1) as u8;
    let mut l = 0u8;
    let mut r = 0u8;
    for i in 0..8 {
        l |= bit(val, b[i]) << i;
        r |= bit(val, a[i]) << i;
    }
    l ^= round(r, &boxes[0], key[0]);
    r ^= round(l, &boxes[1], key[1]);
    l ^= round(r, &boxes[2], key[2]);
    r ^= round(l, &boxes[3], key[3]);
    let mut out = 0u16;
    for i in 0..8 {
        out |= (((l >> i) & 1) as u16) << a[i];
        out |= (((r >> i) & 1) as u16) << b[i];
    }
    out
}

fn bit64(k: &[u32; 2], n: u32) -> u32 { (k[(n / 32) as usize] >> (n % 32)) & 1 }

fn expand_1st_key(src: &[u32; 2]) -> [u32; 4] {
    const BITS: [u8; 96] = [
        33, 58, 49, 36, 0, 31, 22, 30, 3, 16, 5, 53, 10, 41, 23, 19, 27, 39, 43, 6, 34, 12, 61, 21,
        48, 13, 32, 35, 6, 42, 43, 14, 21, 41, 52, 25, 18, 47, 46, 37, 57, 53, 20, 8, 55, 54, 59, 60,
        27, 33, 35, 18, 8, 15, 63, 1, 50, 44, 16, 46, 5, 4, 45, 51, 38, 25, 13, 11, 62, 29, 48, 2,
        59, 61, 62, 56, 51, 57, 54, 9, 24, 63, 22, 7, 26, 42, 45, 40, 23, 14, 2, 31, 52, 28, 44, 17,
    ];
    let mut d = [0u32; 4];
    for (i, &b) in BITS.iter().enumerate() { d[i / 24] |= bit64(src, b as u32) << (i % 24); }
    d
}

fn expand_2nd_key(src: &[u32; 2]) -> [u32; 4] {
    const BITS: [u8; 96] = [
        34, 9, 32, 24, 44, 54, 38, 61, 47, 13, 28, 7, 29, 58, 18, 1, 20, 60, 15, 6, 11, 43, 39, 19,
        63, 23, 16, 62, 54, 40, 31, 3, 56, 61, 17, 25, 47, 38, 55, 57, 5, 4, 15, 42, 22, 7, 2, 19,
        46, 37, 29, 39, 12, 30, 49, 57, 31, 41, 26, 27, 24, 36, 11, 63, 33, 16, 56, 62, 48, 60, 59, 32,
        12, 30, 53, 48, 10, 0, 50, 35, 3, 59, 14, 49, 51, 45, 44, 2, 21, 33, 55, 52, 23, 28, 8, 26,
    ];
    let mut d = [0u32; 4];
    for (i, &b) in BITS.iter().enumerate() { d[i / 24] |= bit64(src, b as u32) << (i % 24); }
    d
}

fn expand_subkey(seed: u16) -> [u32; 2] {
    const BITS: [u8; 64] = [
        5, 10, 14, 9, 4, 0, 15, 6, 1, 8, 3, 2, 12, 7, 13, 11,
        5, 12, 7, 2, 13, 11, 9, 14, 4, 1, 6, 10, 8, 0, 15, 3,
        4, 10, 2, 0, 6, 9, 12, 1, 11, 7, 15, 8, 13, 5, 14, 3,
        14, 11, 12, 7, 4, 5, 2, 10, 1, 15, 0, 9, 8, 6, 13, 3,
    ];
    let mut k = [0u32; 2];
    for (i, &b) in BITS.iter().enumerate() { k[i / 32] |= (((seed >> b) & 1) as u32) << (i % 32); }
    k
}

/// What the 20-byte key file says.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Key {
    pub master: [u32; 2],
    /// encrypted word range (word addresses, inclusive)
    pub lower: u32,
    pub upper: u32,
}

/// MAME init_cps2crypt: unscramble the key file.
pub fn parse_key(file: &[u8]) -> Option<Key> {
    if file.len() < 20 { return None; }
    let mut d = [0u16; 10];
    for b in 0..160usize {
        let bit = (317 - b) % 160;
        if (file[bit / 8] >> ((bit ^ 7) % 8)) & 1 != 0 { d[b / 16] |= 0x8000 >> (b % 16); }
    }
    let master = [(d[0] as u32) << 16 | d[1] as u32, (d[2] as u32) << 16 | d[3] as u32];
    let (lower, upper) = if d[9] == 0xFFFF {
        // "dead" board: only FF0000-FFFFFF is encrypted
        (0xFF0000, 0xFFFFFF)
    } else {
        (0, (((!d[9] as u32 & 0x3FF) << 14) | 0x3FFF) + 1)
    };
    Some(Key { master, lower: lower / 2, upper: upper / 2 })
}

/// The two Feistel networks with their s-boxes and the expanded master key.
pub struct Cipher {
    s1: [[Opt; 4]; 4],
    s2: [[Opt; 4]; 4],
    key1: [u32; 4],
    master: [u32; 2],
}

impl Cipher {
    pub fn new(master: [u32; 2]) -> Self {
        let mut key1 = expand_1st_key(&master);
        // extra bits for s-boxes with fewer than 6 inputs
        key1[0] ^= ((key1[0] >> 1) & 1) << 4;
        key1[0] ^= ((key1[0] >> 2) & 1) << 5;
        key1[0] ^= ((key1[0] >> 8) & 1) << 11;
        key1[1] ^= (key1[1] & 1) << 5;
        key1[1] ^= ((key1[1] >> 8) & 1) << 11;
        key1[2] ^= ((key1[2] >> 1) & 1) << 5;
        key1[2] ^= ((key1[2] >> 8) & 1) << 11;
        Self {
            s1: [opt4(&FN1_R1_BOXES), opt4(&FN1_R2_BOXES), opt4(&FN1_R3_BOXES), opt4(&FN1_R4_BOXES)],
            s2: [opt4(&FN2_R1_BOXES), opt4(&FN2_R2_BOXES), opt4(&FN2_R3_BOXES), opt4(&FN2_R4_BOXES)],
            key1, master,
        }
    }

    /// The 2nd network's key for word address bits 0-15 `i` (address bits 1-16).
    fn key2(&self, i: u16) -> [u32; 4] {
        let seed = feistel(i, &FN1_GROUP_A, &FN1_GROUP_B, &self.s1, &self.key1);
        let sub = expand_subkey(seed);
        let mk = [sub[0] ^ self.master[0], sub[1] ^ self.master[1]];
        let mut k = expand_2nd_key(&mk);
        k[0] ^= (k[0] & 1) << 5;
        k[0] ^= ((k[0] >> 6) & 1) << 11;
        k[1] ^= (k[1] & 1) << 5;
        k[1] ^= ((k[1] >> 1) & 1) << 4;
        k[2] ^= ((k[2] >> 2) & 1) << 5;
        k[2] ^= ((k[2] >> 3) & 1) << 4;
        k[2] ^= ((k[2] >> 7) & 1) << 11;
        k[3] ^= ((k[3] >> 1) & 1) << 5;
        k
    }

    /// Encrypts one word (the inverse of the network; for tests).
    #[cfg(test)]
    pub fn encrypt_word(&self, word_addr: u32, plain: u16) -> u16 {
        let k = self.key2(word_addr as u16);
        let (a, b, bx) = (&FN2_GROUP_A, &FN2_GROUP_B, &self.s2);
        let bit = |v: u16, n: u8| ((v >> n) & 1) as u8;
        let (mut l, mut r) = (0u8, 0u8);
        for i in 0..8 { l |= bit(plain, a[i]) << i; r |= bit(plain, b[i]) << i; }
        r ^= round(l, &bx[3], k[3]);
        l ^= round(r, &bx[2], k[2]);
        r ^= round(l, &bx[1], k[1]);
        l ^= round(r, &bx[0], k[0]);
        let mut out = 0u16;
        for i in 0..8 {
            out |= (((l >> i) & 1) as u16) << b[i];
            out |= (((r >> i) & 1) as u16) << a[i];
        }
        out
    }
}

/// MAME cps2_decrypt: the opcode view of `rom` (big-endian 16-bit words in bytes).
pub fn decrypt(rom: &[u8], key: &Key) -> Vec<u8> {
    let c = Cipher::new(key.master);
    let words = rom.len() / 2;
    let mut out = rom.to_vec();
    for i in 0..0x10000u32 {
        let k2 = c.key2(i as u16);
        let mut a = i as usize;
        while a < words {
            if (a as u32) >= key.lower && (a as u32) <= key.upper {
                let w = (rom[a * 2] as u16) << 8 | rom[a * 2 + 1] as u16;
                let d = feistel(w, &FN2_GROUP_A, &FN2_GROUP_B, &c.s2, &k2);
                out[a * 2] = (d >> 8) as u8;
                out[a * 2 + 1] = d as u8;
            }
            a += 0x10000;
        }
    }
    out
}

/// A key file for `master` and the range word `d9` (tests): the inverse of parse_key.
#[cfg(test)]
pub fn make_key_file(master: [u32; 2], d9: u16) -> Vec<u8> {
    let d = [(master[0] >> 16) as u16, master[0] as u16, (master[1] >> 16) as u16, master[1] as u16, 0, 0, 0, 0, 0, d9];
    let mut f = vec![0u8; 20];
    for b in 0..160usize {
        if d[b / 16] & (0x8000 >> (b % 16)) != 0 {
            let bit = (317 - b) % 160;
            f[bit / 8] |= 1 << ((bit ^ 7) % 8);
        }
    }
    f
}
