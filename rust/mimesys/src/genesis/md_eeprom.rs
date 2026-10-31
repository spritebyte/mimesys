//! Serial (I2C) EEPROM saves: X24C01 / 24Cxx chips on a handful of carts (EA sports games,
//! Wonder Boy in Monster World, Mega Man: The Wily Wars, NBA Jam...). The game bit-bangs
//! SCL / SDA through writes to 0x200000-0x3FFFFF and reads SDA back from the same range;
//! which data bits carry which line depends on the board.
//!
//! Protocol (24Cxx data sheets): START = SDA falls while SCL is high, STOP = SDA rises while
//! SCL is high, bits are latched on SCL rising, every 9th clock is the ACK. X24C01 sends a
//! 7-bit word address + R/W straight after START; the others send a device byte
//! (1010 + block bits + R/W), then 1 or 2 address bytes. Board list and wiring after
//! Genesis Plus GX's cart database.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Board {
    /// EA: SCL = D6, SDA = D7
    Ea,
    /// Sega: SCL = D1, SDA = D0
    Sega,
    /// Acclaim 16M: SCL = D1, SDA in = D0, SDA out = D1, word writes only
    Acclaim16M,
    /// Acclaim 32M: byte writes to odd addresses set SDA, even addresses SCL (both D0);
    /// a word write's D0 picks ROM (1) or the EEPROM (0) for reads at 0x200000-0x2FFFFF
    Acclaim32M,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum State { Standby, WaitStop, WordAdr7, DeviceAdr, WordAdrHigh, WordAdrLow, Write, Read }

#[derive(Clone, Serialize, Deserialize)]
pub struct Eeprom {
    pub board: Board,
    /// 7 (X24C01), 8 or 16 address bits
    address_bits: u32,
    size_mask: u16,
    page_mask: u16,
    sda: bool,
    scl: bool,
    old_sda: bool,
    old_scl: bool,
    cycles: u8,
    read: bool,
    device_address: u16,
    word_address: u16,
    buffer: u8,
    state: State,
    /// Acclaim 32M: reads at 0x200000-0x2FFFFF come from ROM
    pub rom_mapped: bool,
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
    #[serde(skip)]
    pub dirty: bool,
}

/// Chip sizes: (address bits, size mask, page write mask)
#[derive(Clone, Copy, Debug)]
pub enum Chip { X24C01, X24C02, C24C02, C24C04, C24C16, C24C65 }

impl Chip {
    fn spec(self) -> (u32, u16, u16) {
        match self {
            Chip::X24C01 => (7, 0x7F, 0x03),
            Chip::X24C02 => (8, 0xFF, 0x03),
            Chip::C24C02 => (8, 0xFF, 0x07),
            Chip::C24C04 => (8, 0x1FF, 0x0F),
            Chip::C24C16 => (8, 0x7FF, 0x0F),
            Chip::C24C65 => (16, 0x1FFF, 0x3F),
        }
    }
}

/// Known carts by the product code in the header (0x180).
const GAMES: [(&str, Board, Chip); 23] = [
    ("T-50176", Board::Ea, Chip::X24C01),      // Rings of Power
    ("T-50396", Board::Ea, Chip::X24C01),      // NHLPA Hockey 93
    ("T-50446", Board::Ea, Chip::X24C01),      // John Madden Football 93
    ("T-50516", Board::Ea, Chip::X24C01),      // John Madden Football 93 Championship Edition
    ("T-50606", Board::Ea, Chip::X24C01),      // Bill Walsh College Football
    (" T-12046", Board::Sega, Chip::X24C01),   // Mega Man: The Wily Wars
    (" T-12053", Board::Sega, Chip::X24C01),   // Rockman Mega World
    ("MK-1215", Board::Sega, Chip::X24C01),    // Evander Holyfield's Real Deal Boxing
    ("MK-1228", Board::Sega, Chip::X24C01),    // Greatest Heavyweights (U/E)
    ("G-5538", Board::Sega, Chip::X24C01),     // Greatest Heavyweights (J)
    ("PR-1993", Board::Sega, Chip::X24C01),    // Greatest Heavyweights (prototype)
    (" G-4060", Board::Sega, Chip::X24C01),    // Wonder Boy in Monster World
    ("00001211", Board::Sega, Chip::X24C01),   // Sports Talk Baseball
    ("00004076", Board::Sega, Chip::X24C01),   // Honoo no Toukyuuji Dodge Danpei
    ("G-4524", Board::Sega, Chip::X24C01),     // Ninja Burai Densetsu
    ("00054503", Board::Sega, Chip::X24C01),   // Game Toshokan
    ("T-81033", Board::Acclaim16M, Chip::X24C02),   // NBA Jam (J)
    ("T-081326", Board::Acclaim16M, Chip::X24C02),  // NBA Jam (U/E)
    ("T-081276", Board::Acclaim32M, Chip::C24C02),  // NFL Quarterback Club
    ("T-81406", Board::Acclaim32M, Chip::C24C04),   // NBA Jam Tournament Edition
    ("T-081586", Board::Acclaim32M, Chip::C24C16),  // NFL Quarterback Club 96
    ("T-81476", Board::Acclaim32M, Chip::C24C65),   // Frank Thomas Big Hurt Baseball
    ("T-81576", Board::Acclaim32M, Chip::C24C65),   // College Slam
];

impl Eeprom {
    pub fn new(board: Board, chip: Chip) -> Self {
        let (address_bits, size_mask, page_mask) = chip.spec();
        Self {
            board, address_bits, size_mask, page_mask,
            sda: true, scl: true, old_sda: true, old_scl: true, cycles: 0, read: false,
            device_address: 0, word_address: 0, buffer: 0, state: State::Standby,
            rom_mapped: true, data: vec![0xFF; size_mask as usize + 1], dirty: false,
        }
    }

    /// The cart's EEPROM, if it has one: by product code, or a header that says so
    /// (backup RAM type 0xE8, or a 1-byte range), which gets the Sega wiring.
    pub fn detect(rom: &[u8]) -> Option<Self> {
        let product = rom.get(0x180..0x18E).map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default();
        for (id, board, chip) in GAMES {
            if product.contains(id) {
                // the " T-12046"-style ids also match hacks patched to plain backup RAM
                if id.starts_with(' ') && sram_bytes(rom) > 2 { return None; }
                return Some(Self::new(board, chip));
            }
        }
        if rom.get(0x1B0..0x1B2) == Some(b"RA".as_slice()) && (rom.get(0x1B2) == Some(&0xE8) || sram_bytes(rom) < 2) {
            return Some(Self::new(Board::Sega, Chip::X24C01));
        }
        None
    }

    fn pins(&self) -> (u32, u32, u32) {
        // (SCL in, SDA in, SDA out)
        match self.board {
            Board::Ea => (6, 7, 7),
            Board::Sega => (1, 0, 0),
            Board::Acclaim16M => (1, 0, 1),
            Board::Acclaim32M => (0, 0, 0),
        }
    }

    /// Does this board answer reads at `a`?
    pub fn reads(&self, a: u32) -> bool {
        match self.board {
            Board::Acclaim32M => !self.rom_mapped && (0x200000..0x300000).contains(&a),
            _ => (0x200000..0x400000).contains(&a),
        }
    }

    pub fn writes(&self, a: u32) -> bool {
        match self.board {
            Board::Acclaim32M => (0x200000..0x300000).contains(&a),
            _ => (0x200000..0x400000).contains(&a),
        }
    }

    pub fn read_byte(&self, a: u32) -> u8 {
        if a & 1 == 1 || self.board == Board::Acclaim16M { (self.out() as u8) << self.pins().2 } else { 0 }
    }

    pub fn read_word(&self) -> u16 { (self.out() as u16) << self.pins().2 }

    pub fn write_byte(&mut self, a: u32, v: u8) {
        match self.board {
            Board::Acclaim32M => {
                if a & 1 == 1 { self.sda = v & 1 != 0 } else { self.scl = v & 1 != 0 }
                self.update();
            }
            Board::Acclaim16M => self.write_word(v as u16),
            _ => if a & 1 == 1 { self.set_lines(v as u16) },
        }
    }

    pub fn write_word(&mut self, v: u16) {
        match self.board {
            Board::Acclaim32M => self.rom_mapped = v & 1 != 0,
            _ => self.set_lines(v),
        }
    }

    fn set_lines(&mut self, v: u16) {
        let (scl, sda, _) = self.pins();
        self.sda = (v >> sda) & 1 != 0;
        self.scl = (v >> scl) & 1 != 0;
        self.update();
    }

    /// What the chip drives on SDA.
    fn out(&self) -> bool {
        if self.state == State::Read {
            if self.cycles < 9 {
                let a = (self.device_address | self.word_address) as usize & 0xFFFF;
                return (self.data.get(a).copied().unwrap_or(0xFF) >> (8 - self.cycles)) & 1 != 0;
            }
        } else if self.cycles == 9 {
            return false;               // ACK
        }
        self.sda
    }

    fn start(&mut self) {
        if self.old_scl && self.scl && self.old_sda && !self.sda {
            self.cycles = 0;
            if self.address_bits == 7 {
                self.word_address = 0;
                self.state = State::WordAdr7;
            } else {
                self.device_address = 0;
                self.state = State::DeviceAdr;
            }
        }
    }

    fn stop(&mut self) {
        if self.old_scl && self.scl && !self.old_sda && self.sda { self.state = State::Standby; }
    }

    fn update(&mut self) {
        let falling = self.old_scl && !self.scl;
        let rising = !self.old_scl && self.scl;
        let sda = self.sda as u16;
        match self.state {
            State::Standby => self.start(),
            State::WaitStop => self.stop(),
            State::WordAdr7 => {
                self.start();
                self.stop();
                if falling {
                    if self.cycles < 9 { self.cycles += 1; } else {
                        self.cycles = 1;
                        self.state = if self.read { State::Read } else { State::Write };
                        self.buffer = 0;
                    }
                } else if rising {
                    if self.cycles < 8 { self.word_address |= sda << (7 - self.cycles); }
                    else if self.cycles == 8 { self.read = self.sda; }
                }
            }
            State::DeviceAdr => {
                self.start();
                self.stop();
                if falling {
                    if self.cycles < 9 { self.cycles += 1; } else {
                        self.device_address <<= self.address_bits;
                        self.cycles = 1;
                        if self.read {
                            self.state = State::Read;
                        } else {
                            self.word_address = 0;
                            self.state = if self.address_bits == 16 { State::WordAdrHigh } else { State::WordAdrLow };
                        }
                    }
                } else if rising {
                    if self.cycles > 4 && self.cycles < 8 { self.device_address |= sda << (7 - self.cycles); }
                    else if self.cycles == 8 { self.read = self.sda; }
                }
            }
            State::WordAdrHigh => {
                self.start();
                self.stop();
                if falling {
                    if self.cycles < 9 { self.cycles += 1; } else { self.cycles = 1; self.state = State::WordAdrLow; }
                } else if rising && self.cycles < 9 {
                    if (self.size_mask as u32) < (1 << (16 - self.cycles as u32)) {
                        self.device_address >>= 1;     // bit not used by this size: it was a block bit
                    } else {
                        self.word_address |= sda << (16 - self.cycles as u32);
                    }
                }
            }
            State::WordAdrLow => {
                self.start();
                self.stop();
                if falling {
                    if self.cycles < 9 { self.cycles += 1; } else {
                        self.cycles = 1;
                        self.state = State::Write;
                        self.buffer = 0;
                    }
                } else if rising && self.cycles < 9 {
                    if (self.size_mask as u32) < (1 << (8 - self.cycles as u32)) {
                        self.device_address >>= 1;
                    } else {
                        self.word_address |= sda << (8 - self.cycles);
                    }
                }
            }
            State::Read => {
                self.start();
                self.stop();
                if falling {
                    if self.cycles < 9 { self.cycles += 1; } else { self.cycles = 1; }
                } else if rising && self.cycles == 9 {
                    if self.sda { self.state = State::WaitStop; }      // no ACK: done
                    else { self.word_address = (self.word_address + 1) & self.size_mask; }
                }
            }
            State::Write => {
                self.start();
                self.stop();
                if falling {
                    if self.cycles < 9 { self.cycles += 1; } else { self.cycles = 1; }
                } else if rising {
                    if self.cycles < 9 {
                        self.buffer |= (self.sda as u8) << (8 - self.cycles);
                    } else {
                        let a = (self.device_address | self.word_address) as usize & 0xFFFF;
                        if let Some(d) = self.data.get_mut(a) {
                            if *d != self.buffer { *d = self.buffer; self.dirty = true; }
                        }
                        self.buffer = 0;
                        self.word_address = (self.word_address & !self.page_mask) | ((self.word_address + 1) & self.page_mask);
                    }
                }
            }
        }
        self.old_scl = self.scl;
        self.old_sda = self.sda;
    }
}

fn sram_bytes(rom: &[u8]) -> u32 {
    let be32 = |a: usize| rom.get(a..a + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]])).unwrap_or(0);
    be32(0x1B8).wrapping_sub(be32(0x1B4))
}
