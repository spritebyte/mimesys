// 7800 cartridges: 0x4000-0xFFFF (48K) of address space.
//
// .a78 files start with a 128-byte header ("ATARI7800" at byte 1, ROM size at 49-52,
// cart type bits at 53-54). Headerless .bin files are guessed from their size.
//
//   Plain       up to 48K, mapped so it ends at 0xFFFF
//   SuperGame   16K banks: 0xC000-0xFFFF = last bank, 0x8000-0xBFFF = the bank picked by
//               writing its number anywhere in 0x8000-0xBFFF, 0x4000-0x7FFF = one of:
//                 type bit 2: 16K of RAM          type bit 4: bank 6 (the 2nd-to-last)
//                 type bit 3: the first bank, with the switched banks numbered from the
//                             second one (the 144K "large" boards)
//   POKEY sound chip (type bit 0, at 0x4000): not emulated yet, reads 0xFF.

use serde::{Deserialize, Serialize};

const HEADER: usize = 128;
const BANK: usize = 0x4000;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Low {           // what's at 0x4000-0x7FFF on a SuperGame board
    Nothing,
    Ram,
    Bank6,
    FirstBank,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Cart7800 {
    pub supergame: bool,
    pub low: Low,
    pub pokey: bool,
    pub bank: usize,
    #[serde(with = "serde_bytes")]
    pub ram: Vec<u8>,
    #[serde(skip)]
    rom: Vec<u8>,
    pub title: String,
}

impl Cart7800 {
    pub fn new(file: &[u8]) -> Result<Self, String> {
        let has_header = file.len() > HEADER && &file[1..10] == b"ATARI7800";
        let (rom, cart_type, title) = if has_header {
            let h = &file[..HEADER];
            let size = u32::from_be_bytes([h[49], h[50], h[51], h[52]]) as usize;
            let rom = &file[HEADER..];
            let rom = if size > 0 && size <= rom.len() { &rom[..size] } else { rom };
            let title = String::from_utf8_lossy(&h[17..49]).trim_end_matches(['\0', ' ']).to_string();
            (rom.to_vec(), (h[53] as u16) << 8 | h[54] as u16, title)
        } else {
            // no header: big images are SuperGame boards
            let t = match file.len() {
                n if n > 0x24000 => 0x02,
                n if n > 0x20000 => 0x0A,          // 144K: first bank at 0x4000
                n if n > 0xC000 => 0x02,
                _ => 0,
            };
            (file.to_vec(), t, String::new())
        };
        if rom.is_empty() || rom.len() % 1024 != 0 {
            return Err(format!("7800 ROM size {} isn't a multiple of 1K", rom.len()));
        }
        let supergame = cart_type & 0x02 != 0;
        if !supergame && rom.len() > 0xC000 {
            return Err(format!("7800 ROM is {}K but its header doesn't say it's bank switched", rom.len() / 1024));
        }
        let low = if !supergame { Low::Nothing }
            else if cart_type & 0x04 != 0 { Low::Ram }
            else if cart_type & 0x08 != 0 { Low::FirstBank }
            else if cart_type & 0x10 != 0 { Low::Bank6 }
            else { Low::Nothing };
        Ok(Self {
            supergame, low, pokey: cart_type & 0x01 != 0, bank: 0,
            ram: if low == Low::Ram { vec![0; BANK] } else { Vec::new() },
            rom, title,
        })
    }

    pub fn load_state(&mut self, s: Cart7800) {
        let rom = std::mem::take(&mut self.rom);
        *self = s;
        self.rom = rom;
    }

    pub fn rom_len(&self) -> usize { self.rom.len() }

    fn banks(&self) -> usize { (self.rom.len() / BANK).max(1) }

    fn bank_byte(&self, bank: usize, a: u16) -> u8 {
        self.rom[(bank % self.banks()) * BANK + (a as usize & (BANK - 1))]
    }

    /// Read 0x4000-0xFFFF. No side effects, so MARIA's DMA uses it too.
    pub fn read(&self, a: u16) -> u8 {
        if a < 0x4000 { return 0xFF; }
        if !self.supergame {
            let start = 0x10000 - self.rom.len();
            return if (a as usize) >= start { self.rom[a as usize - start] } else { 0xFF };
        }
        let last = self.banks() - 1;
        let switched = if self.low == Low::FirstBank { self.bank + 1 } else { self.bank };
        match a {
            0xC000..=0xFFFF => self.bank_byte(last, a),
            0x8000..=0xBFFF => self.bank_byte(switched, a),
            _ => match self.low {
                Low::Ram => self.ram[(a & 0x3FFF) as usize],
                Low::Bank6 => self.bank_byte(last.saturating_sub(1), a),
                Low::FirstBank => self.bank_byte(0, a),
                Low::Nothing => 0xFF,
            },
        }
    }

    pub fn write(&mut self, a: u16, v: u8) {
        if !self.supergame { return; }
        match a {
            0x8000..=0xBFFF => self.bank = v as usize % self.banks(),
            0x4000..=0x7FFF if self.low == Low::Ram => self.ram[(a & 0x3FFF) as usize] = v,
            _ => {}
        }
    }
}
