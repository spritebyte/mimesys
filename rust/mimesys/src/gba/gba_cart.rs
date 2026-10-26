//! GBA cartridge extras: the save chip, the GPIO port (real-time clock) and ROM mirroring.
//!
//! Save type: a small table of games that can't be detected (below), otherwise the
//! library string Nintendo's SDK leaves in the ROM: "EEPROM_V", "SRAM_V"/"SRAM_F_V",
//! "FLASH_V"/"FLASH512_V" (64K), "FLASH1M_V" (128K), "SRAM_V". Nothing found = decided
//! by the first write (Flash command = Flash 64K, else SRAM) or the save file's size;
//! until then reads give 0xFF like a cart without a save chip.
//!   SRAM   32K at 0E000000, plain bytes
//!   Flash  64K/128K at 0E000000, command sequences (AA@5555, 55@2AAA, cmd@5555),
//!          chip ID SST D4BF (64K) / Macronix 09C2 (128K), 4K sector erase, bank switch
//!   EEPROM 512 B / 8 KB, serial bit stream at 0D000000 (DMA 3). The size is guessed
//!          from the first DMA length (9/73 bits = 512 B, 17/81 = 8 KB) or the save file.
//! RTC: Seiko S-3511 on GPIO 080000C4 (data), C6 (direction), C8 (read enable).
//! Found by the "SIIRTC_V" string or the table.
//!
//! Classic NES Series / Famicom Mini carts mirror their ROM through the whole cart
//! space and check for it (plus EEPROM), or they show "game pak error".

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum SaveType { None, Auto, Sram, Flash64, Flash128, Eeprom512, Eeprom8k, EepromAuto }

/// Games whose save type or extras can't be found from the ROM, by game code
/// (header 0xAC). Facts gathered from NanoBoyAdvance's and mGBA's game lists.
/// (code, save type, has RTC, mirrors ROM)
const GAME_DB: &[(&str, SaveType, bool, bool)] = {
    use SaveType::*;
    &[
        // Classic NES Series (USA/Europe)
        ("FADE", Eeprom512, false, true), ("FBME", Eeprom512, false, true),
        ("FDKE", Eeprom512, false, true), ("FDME", Eeprom512, false, true),
        ("FEBE", Eeprom8k, false, true), ("FICE", Eeprom512, false, true),
        ("FLBE", Eeprom8k, false, true), ("FMRE", Eeprom512, false, true),
        ("FP7E", Eeprom512, false, true), ("FSME", Eeprom512, false, true),
        ("FXVE", Eeprom512, false, true), ("FZLE", Eeprom8k, false, true),
        // Famicom Mini (Japan) with known sizes; other "F" codes use the rule in detect()
        ("FSRJ", Eeprom8k, false, true), ("FGZJ", Eeprom512, false, true),
        ("FMBJ", Eeprom512, false, true), ("FCLJ", Eeprom512, false, true),
        ("FBFJ", Eeprom512, false, true), ("FWCJ", Eeprom512, false, true),
        ("FDMJ", Eeprom512, false, true), ("FDDJ", Eeprom512, false, true),
        ("FTBJ", Eeprom512, false, true), ("FMKJ", Eeprom512, false, true),
        ("FTWJ", Eeprom512, false, true), ("FGGJ", Eeprom512, false, true),
        ("FM2J", Eeprom512, false, true), ("FNMJ", Eeprom512, false, true),
        ("FMRJ", Eeprom8k, false, true), ("FPTJ", Eeprom8k, false, true),
        ("FLBJ", Eeprom8k, false, true), ("FFMJ", Eeprom512, false, true),
        // RTC games
        ("AXVE", Flash128, true, false), ("AXPE", Flash128, true, false),
        ("AXVJ", Flash128, true, false), ("AXPJ", Flash128, true, false),
        ("AXVP", Flash128, true, false), ("AXPP", Flash128, true, false),
        ("AXVD", Flash128, true, false), ("AXPD", Flash128, true, false),
        ("AXVF", Flash128, true, false), ("AXPF", Flash128, true, false),
        ("AXVI", Flash128, true, false), ("AXPI", Flash128, true, false),
        ("AXVS", Flash128, true, false), ("AXPS", Flash128, true, false),
        ("BPEE", Flash128, true, false), ("BPEJ", Flash128, true, false),
        ("BPEP", Flash128, true, false), ("BPED", Flash128, true, false),
        ("BPEF", Flash128, true, false), ("BPEI", Flash128, true, false),
        ("BPES", Flash128, true, false), ("BKAJ", Flash128, true, false),
        ("AGSE", Flash64, true, false), ("AGFE", Flash64, true, false),
        ("U3IE", EepromAuto, true, false), ("U32E", EepromAuto, true, false),
        ("U3IP", EepromAuto, true, false), ("U32P", EepromAuto, true, false),
        ("U3IJ", EepromAuto, true, false), ("U32J", EepromAuto, true, false),
        ("BR4J", Flash128, true, false),
        // EEPROM 8K that some detectors get wrong
        ("ALFE", Eeprom8k, false, false), ("ALFP", Eeprom8k, false, false),
        ("ALFJ", Eeprom8k, false, false), ("ALGE", Eeprom8k, false, false),
        ("ALGP", Eeprom8k, false, false), ("BDBE", Eeprom8k, false, false),
        ("BDBP", Eeprom8k, false, false), ("BG3E", Eeprom8k, false, false),
        ("BLFE", Eeprom8k, false, false), ("BT4E", Eeprom8k, false, false),
        ("BUFE", Eeprom8k, false, false), ("KYGE", Eeprom8k, false, false),
        ("KYGP", Eeprom8k, false, false), ("AROP", Eeprom8k, false, false),
        ("AR8e", Eeprom8k, false, false), ("BDKJ", Eeprom8k, false, false),
    ]
};

/// What a ROM needs: (save type, RTC, ROM mirroring).
pub fn detect(rom: &[u8]) -> (SaveType, bool, bool) {
    let code = rom.get(0xAC..0xB0).map(|c| String::from_utf8_lossy(c).to_string()).unwrap_or_default();
    if let Some(&(_, t, rtc, mirror)) = GAME_DB.iter().find(|e| e.0 == code) {
        return (t, rtc, mirror);
    }
    let found = scan_strings(rom);
    let has = |i: usize| found & (1 << i) != 0;
    let save = if has(0) { SaveType::EepromAuto }
        else if has(1) { SaveType::Flash128 }
        else if has(2) || has(3) { SaveType::Flash64 }
        else if has(5) { SaveType::Sram }
        else { SaveType::Auto };
    // any other Classic NES Series / Famicom Mini cart (game codes "F...")
    if code.starts_with('F') && has(0) { return (SaveType::EepromAuto, false, true); }
    (save, has(4), false)
}

/// One pass over the ROM (strings are word aligned): bit 0 EEPROM_V, 1 FLASH1M_V,
/// 2 FLASH_V, 3 FLASH512_V, 4 SIIRTC_V, 5 SRAM_V / SRAM_F_V.
fn scan_strings(rom: &[u8]) -> u32 {
    const PATS: [&[u8]; 7] = [b"EEPROM_V", b"FLASH1M_V", b"FLASH_V", b"FLASH512_V", b"SIIRTC_V", b"SRAM_V", b"SRAM_F_V"];
    let mut found = 0;
    let mut i = 0;
    while i + 10 <= rom.len() {
        let c = rom[i];
        if c == b'E' || c == b'F' || c == b'S' {
            for (k, p) in PATS.iter().enumerate() {
                if rom[i..].starts_with(p) { found |= 1 << k.min(5); }
            }
        }
        i += 4;
    }
    found
}

// ---------------------------------------------------------------- Flash

#[derive(Clone, Default, Serialize, Deserialize)]
struct Flash { phase: u8, id_mode: bool, erase: bool, write: bool, select: bool, bank: u8 }

// ---------------------------------------------------------------- EEPROM

#[derive(Clone, Default, Serialize, Deserialize)]
struct Eeprom {
    /// 0 idle, 1 address, 2 data in, 3 end bit, 4 reading
    state: u8,
    read_cmd: bool,
    buf: u64,
    bits: u32,
    addr: usize,
    busy_until: u64,
}

// ---------------------------------------------------------------- RTC

#[derive(Clone, Default, Serialize, Deserialize)]
struct Rtc {
    sck: bool, sio: bool, cs: bool,
    /// 0 command, 1 receiving, 2 sending, 3 done
    state: u8,
    data: u8,
    bit: u8,
    byte: u8,
    reg: u8,
    buf: [u8; 7],
    control: u8,
}

const RTC_ARGS: [u8; 8] = [0, 0, 7, 0, 1, 0, 3, 0];

#[derive(Serialize, Deserialize)]
pub struct Cart {
    pub save_type: SaveType,
    pub save: Vec<u8>,
    #[serde(skip)] pub dirty: bool,
    flash: Flash,
    eeprom: Eeprom,
    pub has_rtc: bool,
    rtc: Rtc,
    gpio_dir: u8,
    gpio_data: u8,
    gpio_readable: bool,
    /// address mask for ROM reads (mirroring)
    pub rom_mask: u32,
    /// minutes to add to UTC for the clock (the frontend sets the local offset)
    pub utc_offset_min: i32,
}

impl Cart {
    pub fn new(rom: &[u8]) -> Self {
        let (save_type, has_rtc, mirror) = detect(rom);
        let rom_mask = if mirror { (rom.len().max(1).next_power_of_two() - 1) as u32 } else { 0x01FF_FFFF };
        let mut c = Self {
            save_type, save: Vec::new(), dirty: false,
            flash: Flash::default(), eeprom: Eeprom::default(),
            has_rtc, rtc: Rtc { control: 0x40, ..Default::default() },
            gpio_dir: 0, gpio_data: 0, gpio_readable: false,
            rom_mask, utc_offset_min: 0,
        };
        c.save = vec![0xFF; c.save_size()];
        c
    }

    pub fn save_size(&self) -> usize {
        match self.save_type {
            SaveType::None | SaveType::Auto => 0,
            SaveType::Sram => 0x8000,
            SaveType::Flash64 => 0x10000,
            SaveType::Flash128 => 0x20000,
            SaveType::Eeprom512 => 512,
            SaveType::Eeprom8k | SaveType::EepromAuto => 0x2000,
        }
    }

    pub fn is_eeprom(&self) -> bool {
        matches!(self.save_type, SaveType::Eeprom512 | SaveType::Eeprom8k | SaveType::EepromAuto)
    }

    /// A battery file was loaded: its size settles an undecided EEPROM size.
    pub fn load_save(&mut self, data: &[u8]) {
        if self.save_type == SaveType::Auto {
            self.save_type = match data.len() {
                0x8000 => SaveType::Sram,
                0x10000 => SaveType::Flash64,
                0x20000 => SaveType::Flash128,
                512 => SaveType::Eeprom512,
                0x2000 => SaveType::Eeprom8k,
                _ => return,
            };
            self.save = vec![0xFF; self.save_size()];
        }
        if self.save_type == SaveType::EepromAuto && data.len() == 512 {
            self.save_type = SaveType::Eeprom512;
            self.save = vec![0xFF; 512];
        }
        let n = data.len().min(self.save.len());
        self.save[..n].copy_from_slice(&data[..n]);
    }

    /// DMA 3 to the EEPROM: the transfer length tells the address width.
    pub fn eeprom_dma_hint(&mut self, count: u32) {
        if self.save_type != SaveType::EepromAuto { return; }
        match count {
            9 | 73 => { self.save_type = SaveType::Eeprom512; self.save.truncate(512); }
            17 | 81 => self.save_type = SaveType::Eeprom8k,
            _ => {}
        }
    }

    /// Is a 0x0Dxxxxxx access the EEPROM? (whole 0D region for ROMs up to 16 MB)
    pub fn eeprom_at(&self, addr: u32, rom_len: usize) -> bool {
        if !self.is_eeprom() { return false; }
        let a = addr & 0x01FF_FFFF;
        if rom_len > 0x0100_0000 { a & 0x01FF_FF00 == 0x01FF_FF00 } else { a & 0x0100_0000 != 0 }
    }

    // ---- SRAM / Flash (0E000000) ----

    pub fn read_backup(&self, addr: u32) -> u8 {
        let a = (addr & 0xFFFF) as usize;
        match self.save_type {
            SaveType::Sram => self.save[a & 0x7FFF],
            SaveType::Flash64 | SaveType::Flash128 => {
                if self.flash.id_mode && a < 2 {
                    let id: [u8; 2] = if self.save_type == SaveType::Flash128 { [0xC2, 0x09] } else { [0xBF, 0xD4] };
                    return id[a];
                }
                self.save[self.flash.bank as usize * 0x10000 + a]
            }
            _ => 0xFF,
        }
    }

    pub fn write_backup(&mut self, addr: u32, v: u8) {
        let a = addr & 0xFFFF;
        if self.save_type == SaveType::Auto {
            self.save_type = if a == 0x5555 && v == 0xAA { SaveType::Flash64 } else { SaveType::Sram };
            self.save = vec![0xFF; self.save_size()];
        }
        match self.save_type {
            SaveType::Sram => { self.save[(a & 0x7FFF) as usize] = v; self.dirty = true; }
            SaveType::Flash64 | SaveType::Flash128 => self.flash_write(a, v),
            _ => {}
        }
    }

    fn flash_write(&mut self, a: u32, v: u8) {
        let big = self.save_type == SaveType::Flash128;
        let f = &mut self.flash;
        match f.phase {
            0 => if a == 0x5555 && v == 0xAA { f.phase = 1 },
            1 => f.phase = if a == 0x2AAA && v == 0x55 { 2 } else { 0 },
            2 => {
                f.phase = 0;
                if a == 0x5555 {
                    match v {
                        0x90 => f.id_mode = true,
                        0xF0 => f.id_mode = false,
                        0x80 => f.erase = true,
                        0x10 => if f.erase {
                            self.save.fill(0xFF);
                            self.flash.erase = false;
                            self.dirty = true;
                        },
                        0xA0 => { f.write = true; f.phase = 3; }
                        0xB0 => if big { f.select = true; f.phase = 3; },
                        _ => {}
                    }
                } else if f.erase && a & 0x0FFF == 0 && v == 0x30 {
                    let base = f.bank as usize * 0x10000 + a as usize;
                    f.erase = false;
                    self.save[base..base + 0x1000].fill(0xFF);
                    self.dirty = true;
                }
            }
            _ => {
                f.phase = 0;
                if f.write {
                    f.write = false;
                    let i = f.bank as usize * 0x10000 + a as usize;
                    self.save[i] = v;
                    self.dirty = true;
                } else if f.select && a == 0 {
                    f.select = false;
                    f.bank = v & 1;
                }
            }
        }
    }

    // ---- EEPROM (serial, one bit per 16-bit access) ----

    fn eeprom_addr_bits(&self) -> u32 { if self.save_type == SaveType::Eeprom512 { 6 } else { 14 } }

    pub fn eeprom_read(&mut self, now: u64) -> u16 {
        let e = &mut self.eeprom;
        if e.state == 4 {
            e.bits += 1;
            if e.bits <= 4 { return 0; }          // 4 dummy bits
            let i = e.bits - 5;
            if e.bits == 68 { e.state = 0; e.bits = 0; }
            let byte = self.save.get(e.addr + (i / 8) as usize).copied().unwrap_or(0xFF);
            return ((byte >> (7 - i % 8)) & 1) as u16;
        }
        if now < e.busy_until { 0 } else { 1 }
    }

    pub fn eeprom_write(&mut self, v: u16, now: u64) {
        let addr_bits = self.eeprom_addr_bits();
        let size = self.save.len();
        let e = &mut self.eeprom;
        if e.state == 4 || now < e.busy_until { return; }
        let bit = (v & 1) as u64;
        e.buf = (e.buf << 1) | bit;
        e.bits += 1;
        match e.state {
            0 => if e.bits == 2 {
                match e.buf & 3 {
                    2 => { e.read_cmd = false; e.state = 1; }
                    3 => { e.read_cmd = true; e.state = 1; }
                    _ => {}
                }
                e.buf = 0; e.bits = 0;
            },
            1 => if e.bits == addr_bits {
                e.addr = ((e.buf as usize) * 8) & 0x1FFF & (size.max(8) - 1);
                e.buf = 0; e.bits = 0;
                e.state = if e.read_cmd { 3 } else { 2 };
            },
            2 => if e.bits == 64 {
                let bytes = e.buf.to_be_bytes();
                let a = e.addr;
                e.buf = 0; e.bits = 0;
                e.state = 3;
                if a + 8 <= self.save.len() {
                    self.save[a..a + 8].copy_from_slice(&bytes);
                    self.dirty = true;
                }
            },
            _ => {
                // end bit: a read starts sending; a write keeps the chip busy ~6 ms
                let e = &mut self.eeprom;
                e.buf = 0; e.bits = 0;
                if e.read_cmd { e.state = 4; } else { e.state = 0; e.busy_until = now + 101_400; }
            }
        }
    }

    // ---- GPIO / RTC (080000C4-080000C9) ----

    /// GPIO read, or None when the port isn't readable (the ROM shows through).
    pub fn gpio_read(&self, addr: u32) -> Option<u16> {
        if !self.has_rtc || !self.gpio_readable { return None; }
        match addr & 0x01FF_FFFE {
            0xC4 => {
                let pins = if self.rtc.cs && self.rtc.sio { 2 } else { 0 };
                Some(((self.gpio_data & self.gpio_dir) | (pins & !self.gpio_dir)) as u16 & 15)
            }
            0xC6 => Some(self.gpio_dir as u16),
            0xC8 => Some(self.gpio_readable as u16),
            _ => None,
        }
    }

    pub fn gpio_write(&mut self, addr: u32, v: u16) {
        if !self.has_rtc { return; }
        match addr & 0x01FF_FFFE {
            0xC4 => {
                self.gpio_data = (v & 15) as u8;
                self.rtc_pins(self.gpio_data);
            }
            0xC6 => self.gpio_dir = (v & 15) as u8,
            0xC8 => self.gpio_readable = v & 1 != 0,
            _ => {}
        }
    }

    pub fn is_gpio(&self, addr: u32) -> bool {
        self.has_rtc && (0xC4..=0xC9).contains(&(addr & 0x01FF_FFFF))
    }

    /// Port bits: 0 SCK, 1 SIO, 2 CS. Data moves on the rising edge of SCK.
    fn rtc_pins(&mut self, p: u8) {
        let old_sck = self.rtc.sck;
        let old_cs = self.rtc.cs;
        let dir = self.gpio_dir;
        if dir & 4 != 0 { self.rtc.cs = p & 4 != 0; }
        if dir & 1 != 0 { self.rtc.sck = p & 1 != 0; }
        if dir & 2 != 0 { self.rtc.sio = p & 2 != 0; }
        if !self.rtc.cs { return; }
        if !old_cs {
            self.rtc.state = 0;
            self.rtc.bit = 0;
            self.rtc.byte = 0;
            return;
        }
        if old_sck || !self.rtc.sck { return; }
        match self.rtc.state {
            0 => if self.rtc_shift_in() {
                let mut d = self.rtc.data;
                if d >> 4 == 6 { d = d.reverse_bits(); }   // sent LSB-first: "0110 rrr w"
                else if d & 15 != 6 { return; }
                self.rtc.reg = (d >> 4) & 7;
                self.rtc.byte = 0;
                let args = RTC_ARGS[self.rtc.reg as usize];
                if d & 0x80 != 0 {
                    self.rtc_read_reg();
                    self.rtc.state = if args > 0 { 2 } else { 3 };
                } else if args > 0 {
                    self.rtc.state = 1;
                } else {
                    self.rtc_write_reg();
                    self.rtc.state = 3;
                }
            },
            1 => {
                let args = RTC_ARGS[self.rtc.reg as usize];
                if self.rtc.byte < args && self.rtc_shift_in() {
                    self.rtc.buf[self.rtc.byte as usize] = self.rtc.data;
                    self.rtc.byte += 1;
                    if self.rtc.byte == args { self.rtc_write_reg(); self.rtc.state = 3; }
                }
            }
            2 => {
                let r = &mut self.rtc;
                r.sio = r.buf[r.byte as usize] & 1 != 0;
                r.buf[r.byte as usize] >>= 1;
                r.bit += 1;
                if r.bit == 8 {
                    r.bit = 0;
                    r.byte += 1;
                    if r.byte == RTC_ARGS[r.reg as usize] { r.state = 3; }
                }
            }
            _ => {}
        }
    }

    fn rtc_shift_in(&mut self) -> bool {
        let r = &mut self.rtc;
        r.data = (r.data & !(1 << r.bit)) | ((r.sio as u8) << r.bit);
        r.bit += 1;
        if r.bit == 8 { r.bit = 0; true } else { false }
    }

    fn rtc_read_reg(&mut self) {
        let t = now_civil(self.utc_offset_min);
        let mode24 = self.rtc.control & 0x40 != 0;
        let hour = if !mode24 && t.hour >= 12 { bcd(t.hour - 12) | 0x40 } else { bcd(t.hour) };
        match self.rtc.reg {
            4 => self.rtc.buf[0] = self.rtc.control,
            2 => self.rtc.buf = [bcd(t.year % 100), bcd(t.month), bcd(t.day), bcd(t.weekday), hour, bcd(t.min), bcd(t.sec)],
            6 => { self.rtc.buf[0] = hour; self.rtc.buf[1] = bcd(t.min); self.rtc.buf[2] = bcd(t.sec); }
            _ => {}
        }
    }

    fn rtc_write_reg(&mut self) {
        match self.rtc.reg {
            4 => self.rtc.control = self.rtc.buf[0] & 0x6A,
            0 => self.rtc.control = 0,
            _ => {}   // setting the date/time is ignored: the clock follows the host
        }
    }
}

fn bcd(v: u32) -> u8 { ((v / 10) * 16 + v % 10) as u8 }

pub struct Civil { pub year: u32, pub month: u32, pub day: u32, pub weekday: u32, pub hour: u32, pub min: u32, pub sec: u32 }

/// Host time (UTC + offset) as a calendar date.
pub fn now_civil(offset_min: i32) -> Civil {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    civil_from_unix(secs + offset_min as i64 * 60)
}

pub fn civil_from_unix(t: i64) -> Civil {
    let days = t.div_euclid(86400);
    let s = t.rem_euclid(86400) as u32;
    // Howard Hinnant's days-to-civil
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = (yoe + era * 400 + if month <= 2 { 1 } else { 0 }) as u32;
    Civil { year, month, day, weekday: (days + 4).rem_euclid(7) as u32, hour: s / 3600, min: s / 60 % 60, sec: s % 60 }
}
