//! Booting without the Lynx boot ROM.
//!
//! The ROM's job: read the cartridge's first "frame" from block 0 (a count byte, then
//! that many 51-byte blocks), decrypt it into RAM at 0200 and jump there. Each block is
//! RSA with exponent 3 and a published 51-byte modulus: the block (stored least
//! significant byte first) is cubed modulo the key, and the plaintext bytes are the
//! running sum (mod 256) of the result's bytes, least significant first, leaving out
//! the top byte. So 51 bytes in give 50 bytes out.
//!
//! Like Handy's high-level boot, a tiny stand-in ROM keeps the entry points games call:
//! FE00 (select block A: `STA $FD97; RTS`), FE19 / FE4A / FF80 (`STA $FD97`, then the
//! emulator loads the loader and sets PC). The write to FD97 is the trap.

/// The Lynx public key modulus, most significant byte first.
const MODULUS: [u8; 51] = [
    0x35, 0xB5, 0xA3, 0x94, 0x28, 0x06, 0xD8, 0xA2, 0x26, 0x95, 0xD7, 0x71, 0xB2, 0x3C, 0xFD, 0x56,
    0x1C, 0x4A, 0x19, 0xB6, 0xA3, 0xB0, 0x26, 0x00, 0x36, 0x5A, 0x30, 0x6E, 0x3C, 0x4D, 0x63, 0x38,
    0x1B, 0xD4, 0x1C, 0x13, 0x64, 0x89, 0x36, 0x4C, 0xF2, 0xBA, 0x2A, 0x58, 0xF4, 0xFE, 0xE1, 0xFD,
    0xAC, 0x7E, 0x79,
];

/// Big numbers as 51 bytes, most significant first.
type Big = [u8; 51];

fn ge(a: &Big, b: &Big) -> bool { a >= b }   // lexicographic = numeric for equal lengths

fn sub(a: &mut Big, b: &Big) {
    let mut borrow = 0i16;
    for i in (0..51).rev() {
        let d = a[i] as i16 - b[i] as i16 - borrow;
        a[i] = d as u8;
        borrow = (d < 0) as i16;
    }
}

/// a + b; returns the carry out.
fn add(a: &mut Big, b: &Big) -> bool {
    let mut carry = 0u16;
    for i in (0..51).rev() {
        let s = a[i] as u16 + b[i] as u16 + carry;
        a[i] = s as u8;
        carry = s >> 8;
    }
    carry != 0
}

/// x * y mod MODULUS (x, y < MODULUS), by shift-and-add over y's bits.
fn mul_mod(x: &Big, y: &Big) -> Big {
    let mut r = [0u8; 51];
    for &byte in y.iter() {
        for bit in (0..8).rev() {
            // r = 2r mod m
            let r2 = r;
            let over = add(&mut r, &r2);
            if over || ge(&r, &MODULUS) { sub(&mut r, &MODULUS); }
            if byte >> bit & 1 != 0 {
                let over = add(&mut r, x);
                if over || ge(&r, &MODULUS) { sub(&mut r, &MODULUS); }
            }
        }
    }
    r
}

/// Decrypt one frame: `data[0]` = 256 - block count, then the blocks. Returns the
/// plaintext (50 bytes a block) and how many bytes of `data` it used.
pub fn decrypt_frame(data: &[u8]) -> (Vec<u8>, usize) {
    let Some(&first) = data.first() else { return (Vec::new(), 0) };
    let blocks = 256 - first as usize;
    let mut out = Vec::with_capacity(blocks * 50);
    let mut acc = 0u8;
    for b in 0..blocks {
        let start = 1 + b * 51;
        let Some(chunk) = data.get(start..start + 51) else { break };
        let mut c: Big = [0; 51];
        for i in 0..51 { c[50 - i] = chunk[i]; }        // stored least significant first
        if ge(&c, &MODULUS) { sub(&mut c, &MODULUS); }
        let sq = mul_mod(&c, &c);
        let cube = mul_mod(&sq, &c);
        for i in (1..51).rev() {
            acc = acc.wrapping_add(cube[i]);
            out.push(acc);
        }
    }
    (out, 1 + blocks * 51)
}

/// The stand-in boot ROM (512 bytes at FE00).
pub fn fake_rom() -> Vec<u8> {
    let mut rom = vec![0x88u8; 0x200];
    let trap = [0x8D, 0x97, 0xFD];
    for at in [0x000usize, 0x019, 0x04A, 0x180] { rom[at..at + 3].copy_from_slice(&trap); }
    rom[0x003] = 0x60;                                   // FE00: STA $FD97; RTS
    // vectors at FFF8-FFFF: (FFF8/9 unused), NMI 3000, RESET FF80, IRQ FF80
    rom[0x1F8..0x200].copy_from_slice(&[0x00, 0x80, 0x00, 0x30, 0x80, 0xFF, 0x80, 0xFF]);
    rom
}

/// Which stand-in entry point a trap at `pc` (the PC after the STA) belongs to.
#[derive(Debug, PartialEq, Eq)]
pub enum Entry { SelectBlock, Boot, LoadLoader }

pub fn entry(pc: u16) -> Option<Entry> {
    match pc {
        0xFE03 => Some(Entry::SelectBlock),
        0xFE1C | 0xFF83 => Some(Entry::Boot),
        0xFE4D => Some(Entry::LoadLoader),
        _ => None,
    }
}
