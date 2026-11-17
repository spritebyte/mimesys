//! PlayStation memory cards (on SIO0, address 81h), from Nocash PSX-SPX's "Memory Card
//! Read/Write Commands" and "Memory Card Data Format".
//!
//! - 128 KB: 1024 sectors of 128 bytes. The card file is the raw image other emulators
//!   use (.mcd / .mcr / .bin, no header).
//! - Commands after 81h: "R" read a sector, "W" write one, "S" get ID. Each answers the
//!   FLAG byte first (bit 3: the card was just put in, cleared by the first write).
//! - Block 0 is the directory: frame 0 "MC", frames 1-15 one entry per data block (state
//!   51h first block of a file, 52h middle, 53h last, A0h free), with the file name in the
//!   first block's entry.

pub const SIZE: usize = 0x2_0000;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Cmd {
    #[default]
    None,
    Read,
    Write,
    Id,
}

#[derive(Clone)]
pub struct MemCard {
    pub data: Vec<u8>,
    flag: u8,
    cmd: Cmd,
    step: usize,
    sector: u16,
    chk: u8,
    prev: u8,
    buf: Vec<u8>,
    /// the game wrote to the card
    pub dirty: bool,
}

impl MemCard {
    /// A card from its file; empty or all-zero data: a freshly formatted card
    pub fn new(data: &[u8]) -> Self {
        let data = if data.len() == SIZE && data.iter().any(|&b| b != 0) { data.to_vec() } else { formatted() };
        Self { data, flag: 0x08, cmd: Cmd::None, step: 0, sector: 0, chk: 0, prev: 0, buf: Vec::new(), dirty: false }
    }

    /// The exchange starts (the host sent 81h)
    pub fn select(&mut self) {
        self.cmd = Cmd::None;
        self.step = 0;
    }

    /// One byte after 81h: the reply and whether the card ACKs (more to come)
    pub fn exchange(&mut self, b: u8) -> (u8, bool) {
        let s = self.step;
        self.step += 1;
        if s == 0 {
            self.cmd = match b { 0x52 => Cmd::Read, 0x57 => Cmd::Write, 0x53 => Cmd::Id, _ => Cmd::None };
            return (self.flag, self.cmd != Cmd::None);
        }
        let prev = std::mem::replace(&mut self.prev, b);
        match self.cmd {
            Cmd::None => (0xFF, false),
            Cmd::Id => match s {
                1 => (0x5A, true),
                2 => (0x5D, true),
                3 => (0x5C, true),
                4 => (0x5D, true),
                5 => (0x04, true),
                6 => (0x00, true),
                7 => (0x00, true),
                _ => (0x80, false),
            },
            Cmd::Read => match s {
                1 => (0x5A, true),
                2 => (0x5D, true),
                3 => { self.sector = (b as u16) << 8; (0x00, true) }
                4 => { self.sector |= b as u16; (prev, true) }
                5 => (0x5C, true),
                6 => (0x5D, true),
                7 => {
                    if self.sector > 0x3FF { return (0xFF, true); }
                    self.chk = (self.sector >> 8) as u8 ^ self.sector as u8;
                    ((self.sector >> 8) as u8, true)
                }
                8 => {
                    if self.sector > 0x3FF { return (0xFF, false); }
                    (self.sector as u8, true)
                }
                9..=136 => {
                    let v = self.data[self.sector as usize * 128 + (s - 9)];
                    self.chk ^= v;
                    (v, true)
                }
                137 => (self.chk, true),
                _ => (0x47, false),
            },
            Cmd::Write => match s {
                1 => (0x5A, true),
                2 => (0x5D, true),
                3 => { self.sector = (b as u16) << 8; self.buf.clear(); (0x00, true) }
                4 => {
                    self.sector |= b as u16;
                    self.chk = (self.sector >> 8) as u8 ^ self.sector as u8;
                    (prev, true)
                }
                5..=132 => { self.buf.push(b); self.chk ^= b; (prev, true) }
                133 => { self.chk ^= b; (prev, true) }        // the checksum: chk is now 0 if it matched
                134 => (0x5C, true),
                135 => (0x5D, true),
                _ => {
                    let end = if self.sector > 0x3FF {
                        0xFF
                    } else if self.chk != 0 {
                        0x4E
                    } else {
                        let a = self.sector as usize * 128;
                        self.data[a..a + 128].copy_from_slice(&self.buf);
                        self.dirty = true;
                        0x47
                    };
                    self.flag &= !0x08;
                    (end, false)
                }
            },
        }
    }

    /// As if the card had just been put in
    pub fn reinsert(&mut self) { self.flag = 0x08; }
}

/// A freshly formatted card: the "MC" header, 15 free directory entries, no broken sectors
pub fn formatted() -> Vec<u8> {
    let mut d = vec![0u8; SIZE];
    let frame = |d: &mut [u8], n: usize| {
        let f = &mut d[n * 128..n * 128 + 128];
        f[127] = f[..127].iter().fold(0, |a, b| a ^ b);
    };
    d[0] = b'M';
    d[1] = b'C';
    frame(&mut d, 0);
    for n in 1..16 {
        let f = n * 128;
        d[f] = 0xA0;
        d[f + 8] = 0xFF;
        d[f + 9] = 0xFF;
        frame(&mut d, n);
    }
    for n in 16..36 {
        let f = n * 128;
        d[f..f + 4].fill(0xFF);
        frame(&mut d, n);
    }
    for n in 36..63 { d[n * 128..n * 128 + 128].fill(0xFF); }
    d[63 * 128] = b'M';
    d[63 * 128 + 1] = b'C';
    frame(&mut d, 63);
    d
}

/// A card file: (free blocks, the saves' names); None if it isn't a 128 KB image
pub fn summary(data: &[u8]) -> Option<(usize, Vec<String>)> {
    if data.len() != SIZE { return None; }
    if data.iter().all(|&b| b == 0) { return Some((15, vec![])); }    // blank: formatted when used
    if &data[..2] != b"MC" { return None; }
    let mut free = 0;
    let mut names = vec![];
    for n in 1..16 {
        let f = &data[n * 128..n * 128 + 128];
        match f[0] {
            0x51 => {
                let raw = &f[0x0A..0x1F];
                let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
                names.push(String::from_utf8_lossy(&raw[..end]).into_owned());
            }
            0x52 | 0x53 => {}
            _ => free += 1,
        }
    }
    Some((free, names))
}
