//! PS2 memory cards on SIO2 (peripheral byte 81h), from ps2tek's "SIO2 PS2 Memcards"
//! (behaviour cross-checked against PCSX2's notes on reply layouts).
//!
//! An official 8 MB card: 16 384 sectors of 512 bytes, each followed by 16 bytes of ECC
//! (the IOP's MCMAN writes those itself), erased in blocks of 16 sectors. The image is the
//! raw (512 + 16) × 16 384 = 8 650 752 bytes, the same `.ps2` layout other emulators use.
//! A new card is all FFh (erased flash): the BIOS browser offers to format it.
//!
//! Every reply is as long as the command: the peripheral and command bytes answer 00h,
//! then the command's bytes, usually ending in 2Bh and the terminator (55h until 27h
//! changes it). MagicGate (F0h / F3h / F7h) answers as an authentic card that agrees with
//! whatever the IOP sends (the mechacon side, CDVD S 80h-8Fh, agrees too).

use serde::{Deserialize, Serialize};

pub const SECTOR: usize = 512;
pub const SECTORS: usize = 16_384;
pub const RAW_SECTOR: usize = SECTOR + 16;
pub const CARD_SIZE: usize = RAW_SECTOR * SECTORS;
const ERASE_SECTORS: usize = 16;

#[derive(Clone, Serialize, Deserialize)]
pub struct MemCard {
    /// the image (not in save states: the card on disk is the card)
    #[serde(skip)]
    pub data: Vec<u8>,
    pub term: u8,
    /// byte offset of the sector 21h / 22h / 23h set, plus what 42h / 43h moved since
    addr: usize,
    /// written since the frontend last saved it
    #[serde(skip)]
    pub dirty: bool,
}

impl MemCard {
    /// A card from an image; anything not the right size becomes a blank card
    pub fn new(image: &[u8]) -> Self {
        let data = if image.len() == CARD_SIZE { image.to_vec() } else { vec![0xFF; CARD_SIZE] };
        Self { data, term: 0x55, addr: 0, dirty: image.len() != CARD_SIZE }
    }

    /// One command (`bytes` starts with the peripheral byte 81h): the reply, same length
    pub fn command(&mut self, bytes: &[u8]) -> Vec<u8> {
        let n = bytes.len();
        let arg = |i: usize| bytes.get(i).copied().unwrap_or(0);
        let mut out = vec![0u8, 0u8];
        // zeros up to the last two bytes, then 2Bh and the terminator
        let ok_end = |out: &mut Vec<u8>, term: u8| {
            while out.len() + 2 < n { out.push(0); }
            out.push(0x2B);
            out.push(term);
        };
        match arg(1) {
            0x11 | 0x12 | 0x81 | 0xBF | 0xF7 => ok_end(&mut out, self.term),
            0x21..=0x23 => {
                let sector = u32::from_le_bytes([arg(2), arg(3), arg(4), arg(5)]) as usize;
                self.addr = sector * RAW_SECTOR;
                ok_end(&mut out, self.term);
            }
            0x26 => {
                let spec = [(SECTOR & 0xFF) as u8, (SECTOR >> 8) as u8, ERASE_SECTORS as u8, 0,
                            (SECTORS & 0xFF) as u8, (SECTORS >> 8 & 0xFF) as u8, (SECTORS >> 16) as u8, 0];
                out.push(0x2B);
                out.extend_from_slice(&spec);
                out.push(spec.iter().fold(0, |a, b| a ^ b));
                out.push(self.term);
            }
            0x27 => {
                self.term = arg(2);
                out.extend_from_slice(&[0x00, 0x2B, self.term]);
            }
            0x28 => out.extend_from_slice(&[0x2B, self.term, self.term]),
            0x42 => {
                let len = arg(2) as usize;
                out.extend_from_slice(&[0x00, 0x2B]);
                let mut x = 0u8;
                for k in 0..len {
                    let b = arg(3 + k);
                    x ^= b;
                    if let Some(d) = self.data.get_mut(self.addr + k) { *d = b; }
                    out.push(0);
                }
                out.push(x);
                out.push(self.term);
                self.addr += len;
                self.dirty = true;
            }
            0x43 => {
                let len = arg(2) as usize;
                out.extend_from_slice(&[0x00, 0x2B]);
                let mut x = 0u8;
                for k in 0..len {
                    let b = self.data.get(self.addr + k).copied().unwrap_or(0xFF);
                    x ^= b;
                    out.push(b);
                }
                out.push(x);
                out.push(self.term);
                self.addr += len;
            }
            0x82 => {
                let end = (self.addr + ERASE_SECTORS * RAW_SECTOR).min(self.data.len());
                if self.addr < end { self.data[self.addr..end].fill(0xFF); }
                self.dirty = true;
                ok_end(&mut out, self.term);
            }
            0xF0 => match arg(2) {
                // the IOP sends 8 bytes and wants them XORed back
                0x01 | 0x02 | 0x04 | 0x0F | 0x11 | 0x13 => {
                    out.extend_from_slice(&[0x00, 0x2B]);
                    let x = (3..11).fold(0u8, |a, i| a ^ arg(i));
                    out.extend_from_slice(&[0; 8]);
                    out.push(x);
                    out.push(self.term);
                }
                _ => ok_end(&mut out, self.term),
            },
            0xF3 => {
                self.term = 0x55;
                ok_end(&mut out, self.term);
            }
            _ => {}
        }
        out.resize(n, 0);
        out
    }
}

/// For the frontend's card list: "formatted" / "unformatted"
pub fn summary(data: &[u8]) -> Option<Vec<String>> {
    if data.len() != CARD_SIZE { return None; }
    let formatted = data.starts_with(b"Sony PS2 Memory Card Format");
    Some(vec![if formatted { "formatted" } else { "unformatted" }.to_string()])
}
