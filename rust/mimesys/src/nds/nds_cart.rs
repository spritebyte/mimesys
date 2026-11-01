//! DS game cards: the header, direct boot, ROM reads over the card bus, and save memory
//! on the card's SPI bus (AUXSPI).
//!
//! After a direct boot the card is in the mode where data is read with command B7
//! (8 command bytes MSB first: B7, a 32-bit address, 3 zeros). The real card bus is
//! encrypted at this point; the console hardware and card take care of that, so games
//! never see it and it's skipped here. ROMCTRL bit 31 starts a transfer of the block size
//! in bits 24-26 (0, 0x200 << (n-1), or 4 bytes for 7); words come out of 0x04100010 with
//! bit 23 saying one is ready; the end clears bit 31 and can raise IRQ 19.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub struct Header {
    pub title: String,
    pub game_code: u32,
    pub arm9_rom: u32,
    pub arm9_entry: u32,
    pub arm9_ram: u32,
    pub arm9_size: u32,
    pub arm7_rom: u32,
    pub arm7_entry: u32,
    pub arm7_ram: u32,
    pub arm7_size: u32,
    pub header_crc: u16,
    pub secure_crc: u16,
}

fn le32(d: &[u8], a: usize) -> u32 { d.get(a..a + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).unwrap_or(0) }
fn le16(d: &[u8], a: usize) -> u16 { d.get(a..a + 2).map(|b| u16::from_le_bytes([b[0], b[1]])).unwrap_or(0) }

impl Header {
    pub fn parse(rom: &[u8]) -> Result<Self, String> {
        if rom.len() < 0x200 { return Err("too small for a DS ROM".into()); }
        let h = Header {
            title: String::from_utf8_lossy(&rom[0..12]).trim_end_matches('\0').trim().to_string(),
            game_code: le32(rom, 0x0C),
            arm9_rom: le32(rom, 0x20), arm9_entry: le32(rom, 0x24), arm9_ram: le32(rom, 0x28), arm9_size: le32(rom, 0x2C),
            arm7_rom: le32(rom, 0x30), arm7_entry: le32(rom, 0x34), arm7_ram: le32(rom, 0x38), arm7_size: le32(rom, 0x3C),
            header_crc: le16(rom, 0x15E),
            secure_crc: le16(rom, 0x6C),
        };
        if h.arm9_rom < 0x200 || (h.arm9_rom as u64 + h.arm9_size as u64) > rom.len() as u64
            || h.arm7_rom < 0x200 || (h.arm7_rom as u64 + h.arm7_size as u64) > rom.len() as u64
        {
            return Err("ARM9/ARM7 code doesn't fit in the file".into());
        }
        Ok(h)
    }

    pub fn code_str(&self) -> String {
        String::from_utf8_lossy(&self.game_code.to_le_bytes()).to_string()
    }
}

/// The card's chip ID: Macronix (C2) and the size in MB - 1.
pub fn chip_id(rom_len: usize) -> u32 {
    let mb = (rom_len.next_power_of_two() >> 20).max(1) as u32;
    if mb <= 128 { 0xC2 | ((mb - 1) << 8) } else { 0xC2 | ((0x100 - (mb >> 8)) << 8) }
}

// ---------------------------------------------------------------- secure area (KEY1)

/// Undo the encryption of the first 2 KB of the ARM9 code (the secure area), as the
/// firmware does on boot. Needs the key table from a real ARM7 BIOS (0x30, 0x1048
/// bytes). After GBATEK / melonDS: Blowfish-like, the game code as key.
pub fn decrypt_secure_area(area: &mut [u8], game_code: u32, bios7: &[u8]) -> bool {
    if bios7.len() < 0x30 + 0x1048 { return false; }
    let table: Vec<u32> = (0..0x412).map(|i| le32(bios7, 0x30 + i * 4)).collect();
    let mut k = Key1 { buf: table.clone() };
    k.init(game_code, 2, 2);
    let mut first = [le32(area, 0), le32(area, 4)];
    k.decrypt(&mut first);
    area[0..4].copy_from_slice(&first[0].to_le_bytes());
    area[4..8].copy_from_slice(&first[1].to_le_bytes());
    let mut k = Key1 { buf: table };
    k.init(game_code, 3, 2);
    for i in (0..0x800).step_by(8) {
        let mut w = [le32(area, i), le32(area, i + 4)];
        k.decrypt(&mut w);
        area[i..i + 4].copy_from_slice(&w[0].to_le_bytes());
        area[i + 4..i + 8].copy_from_slice(&w[1].to_le_bytes());
    }
    if &area[0..8] == b"encryObj" {
        area[0..4].copy_from_slice(&0xE7FF_DEFFu32.to_le_bytes());
        area[4..8].copy_from_slice(&0xE7FF_DEFFu32.to_le_bytes());
        true
    } else {
        false
    }
}

/// Is the secure area already decrypted (as in most homebrew-style dumps)?
pub fn secure_area_plain(rom: &[u8], arm9_rom: u32) -> bool {
    let a = arm9_rom as usize;
    !(0x4000..0x8000).contains(&a) || (le32(rom, a) == 0xE7FF_DEFF && le32(rom, a + 4) == 0xE7FF_DEFF)
}

struct Key1 { buf: Vec<u32> }

impl Key1 {
    fn round(&self, z: u32) -> u32 {
        let b = &self.buf;
        let mut x = b[0x12 + (z >> 24) as usize];
        x = x.wrapping_add(b[0x112 + ((z >> 16) & 0xFF) as usize]);
        x ^= b[0x212 + ((z >> 8) & 0xFF) as usize];
        x.wrapping_add(b[0x312 + (z & 0xFF) as usize])
    }
    fn encrypt(&self, d: &mut [u32; 2]) {
        let (mut y, mut x) = (d[0], d[1]);
        for i in 0..=0xF {
            let z = self.buf[i] ^ x;
            x = self.round(z) ^ y;
            y = z;
        }
        d[0] = x ^ self.buf[0x10];
        d[1] = y ^ self.buf[0x11];
    }
    fn decrypt(&self, d: &mut [u32; 2]) {
        let (mut y, mut x) = (d[0], d[1]);
        for i in (0x2..=0x11).rev() {
            let z = self.buf[i] ^ x;
            x = self.round(z) ^ y;
            y = z;
        }
        d[0] = x ^ self.buf[0x1];
        d[1] = y ^ self.buf[0x0];
    }
    fn apply(&mut self, code: &mut [u32; 3], m: usize) {
        let mut a = [code[1], code[2]];
        self.encrypt(&mut a);
        code[1] = a[0]; code[2] = a[1];
        let mut a = [code[0], code[1]];
        self.encrypt(&mut a);
        code[0] = a[0]; code[1] = a[1];
        for i in 0..=0x11 { self.buf[i] ^= code[i % m].swap_bytes(); }
        let mut t = [0u32; 2];
        for i in (0..=0x410).step_by(2) {
            self.encrypt(&mut t);
            self.buf[i] = t[1];
            self.buf[i + 1] = t[0];
        }
    }
    fn init(&mut self, id: u32, level: u32, m: usize) {
        let mut code = [id, id >> 1, id << 1];
        if level >= 1 { self.apply(&mut code, m); }
        if level >= 2 { self.apply(&mut code, m); }
        if level >= 3 {
            code[1] <<= 1;
            code[2] >>= 1;
            self.apply(&mut code, m);
        }
    }
}

// ---------------------------------------------------------------- save memory

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SaveType { None, Eeprom512, Eeprom, Flash }

impl SaveType {
    /// From a save file's size.
    pub fn from_size(n: usize) -> SaveType {
        match n {
            0 => SaveType::None,
            1..=512 => SaveType::Eeprom512,
            513..=0x20000 => SaveType::Eeprom,
            _ => SaveType::Flash,
        }
    }
    /// Address bytes after a command: EEPROMs over 64 KB take 3, like FLASH.
    fn addr_bytes(self, size: usize) -> u8 {
        match self { SaveType::Eeprom512 => 1, SaveType::Eeprom if size <= 0x10000 => 2, _ => 3 }
    }

    /// The chip and size a retail game uses, from `save_types.bin` (built from melonDS's
    /// ROM list by tools/nds_savedb). None = not listed.
    pub fn for_game(code: u32) -> Option<(SaveType, usize)> {
        const DB: &[u8] = include_bytes!("save_types.bin");
        let n = DB.len() / 5;
        let key = |i: usize| u32::from_le_bytes([DB[i * 5], DB[i * 5 + 1], DB[i * 5 + 2], DB[i * 5 + 3]]);
        let (mut lo, mut hi) = (0, n);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if key(mid) < code { lo = mid + 1 } else { hi = mid }
        }
        if lo >= n || key(lo) != code { return None; }
        Some(match DB[lo * 5 + 4] {
            1 => (SaveType::Eeprom512, 512),
            2 => (SaveType::Eeprom, 0x2000),
            3 => (SaveType::Eeprom, 0x10000),
            4 => (SaveType::Eeprom, 0x20000),
            5 => (SaveType::Flash, 0x40000),
            6 => (SaveType::Flash, 0x80000),
            7 => (SaveType::Flash, 0x100000),
            _ => (SaveType::None, 0),         // none, or NAND (not emulated)
        })
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Save {
    pub kind: SaveType,
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
    cmd: u8,
    pos: u32,
    addr: u32,
    wel: bool,
    pub dirty: bool,
}

impl Save {
    pub fn new(kind: SaveType, size: usize) -> Self {
        Self { kind, data: vec![0xFF; size], cmd: 0, pos: 0, addr: 0, wel: false, dirty: false }
    }

    /// One byte over SPI. `last` = chip select goes high after it.
    pub fn transfer(&mut self, v: u8, last: bool) -> u8 {
        if self.kind == SaveType::None { return 0xFF; }
        let mut out = 0xFF;
        if self.pos == 0 {
            self.cmd = v;
            self.addr = 0;
            match v {
                0x06 => self.wel = true,
                0x04 => self.wel = false,
                _ => {}
            }
        } else {
            let ab = self.kind.addr_bytes(self.data.len()) as u32;
            let size = self.data.len().max(1) as u32;
            match self.cmd {
                0x05 => out = (self.wel as u8) << 1,
                0x9F => out = 0xFF,                    // JEDEC ID: retail carts answer FF (as melonDS)
                0x03 | 0x0B => {
                    let fast = self.cmd == 0x0B && self.kind == SaveType::Flash;
                    if self.pos <= ab {
                        self.addr = (self.addr << 8) | v as u32;
                        if self.kind == SaveType::Eeprom512 && self.cmd == 0x0B { self.addr |= 0x100; }
                    } else if !(fast && self.pos == ab + 1) {
                        out = self.data[(self.addr % size) as usize];
                        self.addr = self.addr.wrapping_add(1);
                    }
                }
                0x02 | 0x0A => {
                    if self.pos <= ab {
                        self.addr = (self.addr << 8) | v as u32;
                        if self.kind == SaveType::Eeprom512 && self.cmd == 0x0A { self.addr |= 0x100; }
                    } else if self.wel {
                        let i = (self.addr % size) as usize;
                        if self.data[i] != v { self.data[i] = v; self.dirty = true; }
                        self.addr = self.addr.wrapping_add(1);
                    }
                }
                0xD8 | 0xDB if self.kind == SaveType::Flash => {
                    if self.pos <= 3 { self.addr = (self.addr << 8) | v as u32; }
                    if self.pos == 3 && self.wel {
                        let len = if self.cmd == 0xD8 { 0x10000 } else { 0x100 };
                        let start = (self.addr & !(len - 1)) % size;
                        for b in &mut self.data[start as usize..(start + len).min(size) as usize] { *b = 0xFF; }
                        self.dirty = true;
                    }
                }
                _ => {}
            }
        }
        self.pos += 1;
        if last { self.end(); }
        out
    }

    /// Chip select released: the command is over.
    pub fn end(&mut self) {
        if self.pos > 0 && matches!(self.cmd, 0x02 | 0x0A | 0xD8 | 0xDB) { self.wel = false; }
        self.pos = 0;
    }
}

// ---------------------------------------------------------------- card bus

#[derive(Clone, Serialize, Deserialize)]
pub struct Cart {
    #[serde(skip)]
    pub rom: Vec<u8>,
    pub chip_id: u32,
    pub auxspicnt: u16,
    pub auxspidata: u8,
    pub romctrl: u32,
    pub cmd: [u8; 8],
    /// words of the current transfer still to read
    buffer: Vec<u32>,
    pos: usize,
    pub save: Save,
    /// an infrared cart (game code I...: Pokémon HG/SS, B/W, B2/W2, Walk with Me): every
    /// SPI command starts with a byte for the IR chip; 00 passes the rest to the save chip
    #[serde(default)]
    pub ir: bool,
    #[serde(default)]
    ir_pos: u8,
    #[serde(default)]
    ir_cmd: u8,
}

impl Cart {
    pub fn new(rom: Vec<u8>, save: Save) -> Self {
        let chip_id = chip_id(rom.len());
        let ir = rom.get(0x0C) == Some(&b'I');
        Self { rom, chip_id, auxspicnt: 0, auxspidata: 0, romctrl: 0, cmd: [0; 8], buffer: Vec::new(), pos: 0, save,
               ir, ir_pos: 0, ir_cmd: 0 }
    }

    fn rom_word(&self, a: u32) -> u32 {
        let mask = self.rom.len().next_power_of_two() as u32 - 1;
        let a = (a & mask) as usize;
        if a + 4 <= self.rom.len() { le32(&self.rom, a) } else { 0xFFFF_FFFF }
    }

    /// ROMCTRL write. Returns true when a transfer started (data ready at once).
    pub fn write_romctrl(&mut self, v: u32, mask: u32) -> bool {
        let start = v & mask & 0x8000_0000 != 0 && self.romctrl & 0x8000_0000 == 0;
        self.romctrl = (self.romctrl & !mask) | (v & mask & 0xFF7F_FFFF);
        if !start || self.auxspicnt & 0x8000 == 0 || self.auxspicnt & 0x2000 != 0 {
            if start { self.romctrl &= !0x8000_0000; }
            return false;
        }
        let size = match (self.romctrl >> 24) & 7 { 0 => 0, 7 => 4, n => 0x100 << n };
        let words = size / 4;
        let c = &self.cmd;
        self.buffer.clear();
        self.pos = 0;
        match c[0] {
            0xB7 => {
                let mut a = u32::from_be_bytes([c[1], c[2], c[3], c[4]]);
                if a < 0x8000 { a = 0x8000 + (a & 0x1FF); }
                let page = a & !0xFFF;
                for i in 0..words {
                    let off = (a + i * 4) & 0xFFF;
                    self.buffer.push(self.rom_word(page + off));
                }
            }
            0xB8 | 0x90 => self.buffer.resize(words as usize, self.chip_id),
            0x00 => for i in 0..words { self.buffer.push(self.rom_word((i * 4) & 0xFFF)) },
            _ => self.buffer.resize(words as usize, 0xFFFF_FFFF),
        }
        if self.buffer.is_empty() {
            self.romctrl &= !0x8000_0000;
        } else {
            self.romctrl |= 0x0080_0000;
        }
        true
    }

    pub fn data_ready(&self) -> bool { self.pos < self.buffer.len() }

    /// Read 0x04100010. Returns (word, transfer finished).
    pub fn read_data(&mut self) -> (u32, bool) {
        if self.pos >= self.buffer.len() { return (0xFFFF_FFFF, false); }
        let v = self.buffer[self.pos];
        self.pos += 1;
        if self.pos >= self.buffer.len() {
            self.romctrl &= !0x8080_0000;
            return (v, true);
        }
        (v, false)
    }

    pub fn write_auxspicnt(&mut self, v: u16, mask: u16) {
        // the save chip is released after a byte sent without the hold bit, or when the
        // slot's SPI mode is switched off (libnds ends commands that way)
        self.auxspicnt = (self.auxspicnt & !mask) | (v & mask & 0xE043);
        if self.auxspicnt & 0xA000 != 0xA000 { self.save.end(); self.ir_pos = 0; }
    }

    pub fn write_auxspidata(&mut self, v: u8) {
        if self.auxspicnt & 0xA000 != 0xA000 { return; }
        let last = self.auxspicnt & 0x40 == 0;
        self.auxspidata = if !self.ir {
            self.save.transfer(v, last)
        } else if self.ir_pos == 0 {
            self.ir_cmd = v;
            self.ir_pos = 1;
            0
        } else {
            match self.ir_cmd {
                0x00 => self.save.transfer(v, last),
                0x08 => 0xAA,                       // IR chip ID
                _ => 0,                             // infrared itself isn't emulated
            }
        };
        if last { self.ir_pos = 0; self.save.end(); }
    }
}
