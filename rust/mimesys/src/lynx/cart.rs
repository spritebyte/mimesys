//! Lynx cartridges. The cart has no address bus of its own: Mikey shifts a block number
//! (8 bits) into a register, one bit per strobe, and Suzy's RCART0 / RCART1 ports read
//! bytes from the block, a counter stepping through it. A block is 256, 512, 1024 or
//! 2048 bytes ("page size"); bank 0 is the ROM, bank 1 an optional second chip.
//!
//! Files: `.lnx` (64-byte "LYNX" header: page sizes, name, rotation, EEPROM type),
//! headerless `.lyx` / `.lnx` dumps (page size guessed from the length), and `.o`
//! homebrew programs (BLL header 80 08, load address, length, "BS93") that load
//! straight into RAM.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Rotate { None, Left, Right }

#[derive(Clone, Debug)]
pub struct Header {
    pub name: String,
    pub maker: String,
    pub rotate: Rotate,
    pub eeprom: u8,
    pub audin: bool,
}

/// A homebrew program: bytes to put at `load`, then run from there.
#[derive(Clone)]
pub struct Homebrew { pub load: u16, pub data: Vec<u8> }

#[derive(Clone, Serialize, Deserialize)]
pub struct Cart {
    #[serde(skip)]
    pub bank0: Vec<u8>,
    #[serde(skip)]
    pub bank1: Vec<u8>,
    shift0: u32, mask0: u32,
    shift1: u32, mask1: u32,
    /// the block number shifted in through IODAT bit 1 + SYSCTL1 bit 0
    pub shifter: u32,
    pub counter: u32,
    strobe: bool,
    addr_data: bool,
    /// bank 1 is RAM (writable)
    pub bank1_writable: bool,
}

/// Games played with the Lynx held upright, by CRC-32 of the ROM data (no header), for
/// dumps whose header doesn't say (rotation list from Handy's cart database).
const ROTATED: &[(u32, Rotate)] = &[
    (0x97501709, Rotate::Right), // Centipede (proto)
    (0xDCD723E3, Rotate::Left),  // Gauntlet: The Third Encounter (beta)
    (0x7F0EC7AD, Rotate::Left),  // Gauntlet: The Third Encounter
    (0x4D5D94F4, Rotate::Left),  // Klax (beta)
    (0xA53649F1, Rotate::Left),  // Klax
    (0x0271B6E9, Rotate::Right), // Lexis
    (0x006FD398, Rotate::Right), // NFL Football
    (0xBCD10C3A, Rotate::Right), // Raiden (proto)
    (0x689F31A4, Rotate::Right), // Raiden (2015)
];

/// Carts with a save EEPROM, by CRC-32 of the ROM data, for headers that don't say
/// (Handy's cart database; all 93C46, 16-bit).
const EEPROMS: &[u32] = &[
    0x9D09BC4C, // 8-Bit Slicks
    0x95A1EA09, // Alpine Games
    0x427D0E97, // A Bug's Trip Redux
    0xCA7CF30B, // MegaPak 1
    0xB8879506, // Sky Raider Redux
    0xB2FA93D3, // Unnamed
    0x50B0575A, // Vikings Saga: Protect The Love
    0x18B006A9, // Vikings Saga: Save The Love
    0x7F8B5EFA, // Wyvern Tales
];

fn page_bits(page: usize) -> Option<(u32, u32)> {
    match page { 0x100 => Some((8, 0xFF)), 0x200 => Some((9, 0x1FF)), 0x400 => Some((10, 0x3FF)),
                 0x800 => Some((11, 0x7FF)), _ => None }
}

impl Cart {
    pub fn empty() -> Self {
        Self { bank0: Vec::new(), bank1: Vec::new(), shift0: 8, mask0: 0xFF, shift1: 8, mask1: 0xFF,
               shifter: 0, counter: 0, strobe: false, addr_data: false, bank1_writable: false }
    }

    /// Parse a `.lnx` / `.lyx` image. Returns the cart and its header (defaults if none).
    pub fn new(data: &[u8]) -> Result<(Self, Header), String> {
        let mut c = Self::empty();
        let mut h = Header { name: String::new(), maker: String::new(), rotate: Rotate::None, eeprom: 0, audin: false };
        let rom: &[u8];
        let (page0, page1);
        if data.len() >= 64 && &data[0..4] == b"LYNX" {
            page0 = u16::from_le_bytes([data[4], data[5]]) as usize;
            page1 = u16::from_le_bytes([data[6], data[7]]) as usize;
            let text = |b: &[u8]| String::from_utf8_lossy(b.split(|&x| x == 0).next().unwrap_or(&[])).trim().to_string();
            h.name = text(&data[10..42]);
            h.maker = text(&data[42..58]);
            h.rotate = match data[58] { 1 => Rotate::Left, 2 => Rotate::Right, _ => Rotate::None };
            h.audin = data[59] & 1 != 0;
            h.eeprom = data[60];
            rom = &data[64..];
        } else {
            // no header: 256 blocks of whatever size makes up the file
            if data.is_empty() { return Err("empty file".into()); }
            page0 = (data.len() / 256).next_power_of_two().clamp(0x100, 0x800);
            page1 = 0;
            rom = data;
        }
        if h.rotate == Rotate::None || h.eeprom == 0 {
            let crc = crate::nes::rom_db::crc32(&[rom]);
            if h.rotate == Rotate::None {
                if let Some(&(_, r)) = ROTATED.iter().find(|(c, _)| *c == crc) { h.rotate = r; }
            }
            if h.eeprom == 0 && EEPROMS.contains(&crc) { h.eeprom = 1; }
        }
        let (s0, m0) = page_bits(page0).ok_or(format!("bank 0 page size {page0:#x} isn't 256-2048"))?;
        c.shift0 = s0; c.mask0 = m0;
        let len0 = (page0 * 256).min(rom.len());
        c.bank0 = rom[..len0].to_vec();
        c.bank0.resize(page0 * 256, 0xFF);
        if let Some((s1, m1)) = page_bits(page1) {
            c.shift1 = s1; c.mask1 = m1;
            c.bank1 = rom.get(len0..).map(|r| r.to_vec()).unwrap_or_default();
            if c.bank1.is_empty() { c.bank1_writable = true; }   // a RAM bank
            c.bank1.resize(page1 * 256, 0xFF);
        }
        Ok((c, h))
    }

    /// A BLL `.o` homebrew file.
    pub fn homebrew(data: &[u8]) -> Option<Homebrew> {
        if data.len() < 10 || data[0] != 0x80 || data[1] != 0x08 { return None; }
        let load = u16::from_be_bytes([data[2], data[3]]);
        let len = (u16::from_be_bytes([data[4], data[5]]) as usize).saturating_sub(10);
        let body = &data[10..];
        Some(Homebrew { load, data: body[..len.min(body.len())].to_vec() })
    }

    /// SYSCTL1 bit 0: the strobe. High resets the counter; its rising edge clocks
    /// IODAT bit 1 into the block number.
    pub fn set_strobe(&mut self, on: bool) {
        if on { self.counter = 0; }
        if on && !self.strobe { self.shifter = ((self.shifter << 1) | self.addr_data as u32) & 0xFF; }
        self.strobe = on;
    }

    pub fn set_addr_data(&mut self, bit: bool) { self.addr_data = bit; }

    fn step(&mut self) { if !self.strobe { self.counter = (self.counter + 1) & 0x7FF; } }

    pub fn read0(&mut self) -> u8 {
        let a = (self.shifter << self.shift0) + (self.counter & self.mask0);
        let v = if self.bank0.is_empty() { 0xFF } else { self.bank0[a as usize % self.bank0.len()] };
        self.step();
        v
    }

    pub fn read1(&mut self) -> u8 {
        let a = (self.shifter << self.shift1) + (self.counter & self.mask1);
        let v = if self.bank1.is_empty() { 0xFF } else { self.bank1[a as usize % self.bank1.len()] };
        self.step();
        v
    }

    pub fn write1(&mut self, v: u8) {
        let a = (self.shifter << self.shift1) + (self.counter & self.mask1);
        if self.bank1_writable && !self.bank1.is_empty() {
            let n = self.bank1.len();
            self.bank1[a as usize % n] = v;
        }
        self.step();
    }

    pub fn write0(&mut self) { self.step(); }
}
