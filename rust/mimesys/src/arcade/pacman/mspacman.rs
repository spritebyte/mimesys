fn bitswap(val: usize, bits: &[u8]) -> usize {
    let n = bits.len();
    bits.iter().enumerate()
        .map(|(k, &src)| ((val >> src) & 1) << (n - 1 - k))
        .sum()
}

fn decrypt_data(b: u8) -> u8 {
    bitswap(b as usize, &[0, 4, 5, 7, 6, 3, 2, 1]) as u8
}

const PATCHES: [(usize, usize); 40] = [
    (0x0410, 0x8008), (0x08e0, 0x81d8), (0x0a30, 0x8118), (0x0bd0, 0x80d8), (0x0c20, 0x8120),
    (0x0e58, 0x8168), (0x0ea8, 0x8198), (0x1000, 0x8020), (0x1008, 0x8010), (0x1288, 0x8098),
    (0x1348, 0x8048), (0x1688, 0x8088), (0x16b0, 0x8188), (0x16d8, 0x80c8), (0x16f8, 0x81c8),
    (0x19a8, 0x80a8), (0x19b8, 0x81a8), (0x2060, 0x8148), (0x2108, 0x8018), (0x21a0, 0x81a0),
    (0x2298, 0x80a0), (0x23e0, 0x80e8), (0x2418, 0x8000), (0x2448, 0x8058), (0x2470, 0x8140),
    (0x2488, 0x8080), (0x24b0, 0x8180), (0x24d8, 0x80c0), (0x24f8, 0x81c0), (0x2748, 0x8050),
    (0x2780, 0x8090), (0x27b8, 0x8190), (0x2800, 0x8028), (0x2b20, 0x8100), (0x2b30, 0x8110),
    (0x2bf0, 0x81d0), (0x2cc0, 0x80d0), (0x2cd8, 0x80e0), (0x2cf0, 0x81e0), (0x2d60, 0x8160),   
];

pub fn build_banks(rom: &[u8]) -> Result<[Vec<u8>; 2], String> {
    if rom.len() < 0xC000 {
        return Err(format!("mspacman maincpu region is 0x{:X} bytes, expected 0x10000", rom.len()));
    }
    let mut pac = vec![0u8; 0x10000];
    pac[..0x4000].copy_from_slice(&rom[..0x4000]);
    pac.copy_within(0..0x4000, 0x8000);

    let mut ms = vec![0u8; 0x10000];
    ms[..0x3000].copy_from_slice(&rom[..0x3000]);
    for i in 0..0x1000 {
        ms[0x3000 + i] = decrypt_data(rom[0xB000 + bitswap(i, &[11, 3, 7, 9, 10, 8, 6, 5, 4, 2, 1, 0])]);
    }
    for i in 0..0x800 {
        ms[0x8000 + i] = decrypt_data(rom[0x8000 + bitswap(i, &[8, 7, 5, 9, 10, 6, 3, 4, 2, 1, 0])]); // u5
        ms[0x8800 + i] = decrypt_data(rom[0x9800 + bitswap(i, &[3, 7, 9, 10, 8, 6, 5, 4, 2, 1, 0])]); // u6 high half
        ms[0x9000 + i] = decrypt_data(rom[0x9000 + bitswap(i, &[3, 7, 9, 10, 8, 6, 5, 4, 2, 1, 0])]); // u6 low half
        ms[0x9800 + i] = rom[0x1800 + i];             // mirror of the top of 6f    
    }
    ms[0xA000..0xB000].copy_from_slice(&rom[0x2000..0x3000]);
    ms[0xB000..0xC000].copy_from_slice(&rom[0x3000..0x4000]);

    for &(dst, src) in PATCHES.iter() {
        ms.copy_within(src..src + 8, dst);
    }
    Ok([pac, ms])
}

pub fn trap(addr: u16) -> Option<usize> {
    match addr & 0xFFF8 {
        0x3FF8 => Some(1),
        0x0038 | 0x03B0 | 0x1600 | 0x2120 | 0x3FF0 | 0x8000 | 0x97F0 => Some(0),
        _ => None,
    }
}
