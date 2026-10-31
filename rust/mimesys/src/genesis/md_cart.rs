//! Genesis cartridges: plain dumps (.md / .gen / .bin), interleaved Super Magic Drive dumps
//! (.smd), the header at 0x100 (region, backup RAM), and the "SSF2" bank switching that
//! carts over 4 MB use (Super Street Fighter II).

/// Undo the SMD copier format: a 512-byte header, then 16 KB blocks holding the odd bytes
/// in their first half and the even bytes in their second.
pub fn decode_smd(data: &[u8]) -> Vec<u8> {
    let body = &data[512..];
    let mut out = vec![0u8; body.len()];
    for (b, block) in body.chunks(0x4000).enumerate() {
        let half = block.len() / 2;
        for i in 0..half {
            out[b * 0x4000 + i * 2 + 1] = block[i];
            out[b * 0x4000 + i * 2] = block[half + i];
        }
    }
    out
}

/// True when `data` looks like an SMD dump rather than a plain one.
pub fn is_smd(data: &[u8]) -> bool {
    if data.len() < 0x4200 || data.len() % 0x4000 != 512 { return false; }
    if data.len() >= 0x104 && &data[0x100..0x104] == b"SEGA" { return false; }
    (data[8] == 0xAA && data[9] == 0xBB) || decode_smd(&data[..0x4200]).get(0x100..0x104) == Some(b"SEGA".as_slice())
}

/// Plain ROM bytes from whatever was loaded, laid out as the 68000 sees them.
pub fn normalize(data: &[u8]) -> Vec<u8> {
    let rom = if is_smd(data) { decode_smd(data) } else { data.to_vec() };
    board_layout(rom)
}

/// Boards that don't wire the ROM straight. QuackShot Rev A (the original 512 KB dump):
/// its first 256 KB repeats through 000000-0FFFFF and the second 256 KB through
/// 100000-1FFFFF (address lines 18-19 not connected, line 20 goes to the chip's A18).
/// After Genesis Plus GX.
fn board_layout(rom: Vec<u8>) -> Vec<u8> {
    let product = rom.get(0x180..0x18E).map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default();
    let checksum = rom.get(0x18E..0x190).map(|b| u16::from_be_bytes([b[0], b[1]])).unwrap_or(0);
    if product.contains("00004054-01") && rom.len() == 0x80000 && checksum == 0xA4B3 {
        let mut out = vec![0u8; 0x200000];
        for (a, b) in out.iter_mut().enumerate() {
            let half = if a >= 0x100000 { 0x40000 } else { 0 };
            *b = rom[half + (a & 0x3FFFF)];
        }
        return out;
    }
    rom
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region { Japan, Usa, Europe }

#[derive(Clone, Debug)]
pub struct Header {
    pub title: String,
    pub region: Region,
    /// backup RAM: first and last 68000 address
    pub sram: Option<(u32, u32)>,
}

fn be32(d: &[u8], a: usize) -> u32 {
    if d.len() < a + 4 { return 0; }
    u32::from_be_bytes([d[a], d[a + 1], d[a + 2], d[a + 3]])
}

pub fn parse_header(rom: &[u8]) -> Header {
    let text = |a: usize, n: usize| -> String {
        rom.get(a..a + n).map(|b| String::from_utf8_lossy(b).replace(char::from(0), " ").split_whitespace().collect::<Vec<_>>().join(" ")).unwrap_or_default()
    };
    let mut title = text(0x150, 48);
    if title.is_empty() { title = text(0x120, 48); }

    // region: old style letters (J, U, E) or a hex digit (bit 0 Japan, 2 USA, 3 Europe)
    let codes = rom.get(0x1F0..0x1F3).unwrap_or(&[]);
    let (mut j, mut u, mut e) = (false, false, false);
    let single = codes.len() == 3 && codes[1..].iter().all(|&c| c == b' ' || c == 0);
    match codes.first() {
        // new style: one hex digit ('E' alone stays Europe, the old meaning)
        Some(&c) if single && matches!(c, b'0'..=b'9' | b'A'..=b'D' | b'F') => {
            let v = (c as char).to_digit(16).unwrap_or(0);
            j = v & 1 != 0; u = v & 4 != 0; e = v & 8 != 0;
        }
        _ => for &c in codes {
            match c { b'J' => j = true, b'U' => u = true, b'E' => e = true, _ => {} }
        },
    }
    let region = if u || (!j && !e) { Region::Usa } else if j { Region::Japan } else { Region::Europe };

    let sram = if rom.get(0x1B0..0x1B2) == Some(b"RA".as_slice()) {
        let (start, end) = (be32(rom, 0x1B4), be32(rom, 0x1B8));
        if start >= 0x200000 && end > start && end - start < 0x20000 { Some((start, end)) } else { None }
    } else {
        None
    };
    Header { title, region, sram }
}
