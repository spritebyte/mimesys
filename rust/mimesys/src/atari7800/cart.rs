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
    /// Games whose handlers only work if WSYNC doesn't halt the CPU (see `NO_WSYNC`)
    #[serde(skip)]
    pub ignore_wsync: bool,
    /// MD5 of the ROM without its header (shown in the load message)
    #[serde(skip)]
    pub md5: String,
}

/// MD5 (of the ROM without its header) of the games that need WSYNC ignored: Kung-Fu Master
/// (NTSC) and two fan versions of it. Their display-list interrupt handlers set a two-byte
/// vector after a WSYNC and expect to be done before the next interrupt; with WSYNC honoured
/// and MARIA's DMA stealing cycles, an interrupt lands between the two stores and the
/// nested handlers overflow the stack. The ProSystem emulator's game list has the same entry
/// (and, like every game it doesn't flag otherwise, runs it without MARIA stealing CPU cycles
/// for DMA; with stealing, the handlers start at a different point in the line each frame and
/// the colors flicker).
const NO_WSYNC: [[u8; 16]; 3] = [
    [0xf5, 0x7d, 0x0a, 0xf3, 0x23, 0xd4, 0xe1, 0x73, 0xfb, 0x49, 0xed, 0x44, 0x7f, 0x05, 0x63, 0xd7],
    [0x41, 0xb1, 0xdf, 0xe4, 0x17, 0x4a, 0xd5, 0x43, 0x4f, 0x1e, 0xf1, 0xae, 0xea, 0xc9, 0x05, 0x92],
    [0x4f, 0xc0, 0xb1, 0x6e, 0x3a, 0x5c, 0x46, 0x37, 0x90, 0xa0, 0x4f, 0xb4, 0x2a, 0x1e, 0x4c, 0x40],
];

/// MD5, for the table above.
pub fn md5(data: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22,
        5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20,
        4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23,
        6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21];
    let k: Vec<u32> = (0..64).map(|i| ((i as f64 + 1.0).sin().abs() * 4294967296.0) as u32).collect();
    let mut h: [u32; 4] = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 { msg.push(0); }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_le_bytes());
    for chunk in msg.chunks_exact(64) {
        let m: Vec<u32> = chunk.chunks_exact(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
        let [mut a, mut b, mut c, mut d] = h;
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let t = d;
            d = c;
            c = b;
            b = b.wrapping_add(a.wrapping_add(f).wrapping_add(k[i]).wrapping_add(m[g]).rotate_left(S[i]));
            a = t;
        }
        h = [h[0].wrapping_add(a), h[1].wrapping_add(b), h[2].wrapping_add(c), h[3].wrapping_add(d)];
    }
    let mut out = [0u8; 16];
    for (i, v) in h.iter().enumerate() { out[4 * i..4 * i + 4].copy_from_slice(&v.to_le_bytes()); }
    out
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
        let digest = md5(&rom);
        let ignore_wsync = NO_WSYNC.contains(&digest);
        let md5_hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        Ok(Self {
            supergame, low, pokey: cart_type & 0x01 != 0, bank: 0, ignore_wsync, md5: md5_hex,
            ram: if low == Low::Ram { vec![0; BANK] } else { Vec::new() },
            rom, title,
        })
    }

    pub fn load_state(&mut self, s: Cart7800) {
        let rom = std::mem::take(&mut self.rom);
        let (ignore_wsync, md5) = (self.ignore_wsync, std::mem::take(&mut self.md5));
        *self = s;
        self.rom = rom;
        self.ignore_wsync = ignore_wsync;
        self.md5 = md5;
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
