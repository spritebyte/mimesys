// Sega's early-80s Z80 encryption (315-50xx custom CPUs).
//
// The CPU decrypts bytes as it reads them. Only data bits 3, 5 and 7 are changed,
// and how depends on:
//   - whether the read is an opcode fetch (M1) or anything else (operand / data)
//   - a few address bits
// So every ROM byte has two meanings: an "opcode" one and a "data" one. We decode
// the ROM once at load time into two images, and the bus serves opcode fetches
// from one and normal reads from the other (Bus::read_opcode vs Bus::read).
//
// Only 0x0000-0x7FFF (A15 = 0) is encrypted.
//
// References: MAME src/devices/machine/segacrpt_device.cpp (decode, 315-5010 table)
//             MAME src/mame/pacman/pengo.cpp (decode_pengo6 = 315-5007)

pub struct Decrypted {
    pub opcodes: Vec<u8>,
    pub data: Vec<u8>,
}

/// One row per address combination: [opcode table, data table], 4 entries each.
type ConvTable = [[[u8; 4]; 2]; 16];

/// 315-5010, used by Pengo (pengoa/b/c, pengoja).
const SEGA_315_5010: ConvTable = [
    [[0xa0, 0x80, 0xa8, 0x88], [0x28, 0xa8, 0x08, 0x88]], // ...0...0...0...0
    [[0x28, 0xa8, 0x08, 0x88], [0xa0, 0x80, 0xa8, 0x88]], // ...0...0...0...1
    [[0xa0, 0x80, 0x20, 0x00], [0xa0, 0x80, 0x20, 0x00]], // ...0...0...1...0
    [[0x08, 0x28, 0x88, 0xa8], [0xa0, 0x80, 0xa8, 0x88]], // ...0...0...1...1
    [[0x08, 0x00, 0x88, 0x80], [0x28, 0xa8, 0x08, 0x88]], // ...0...1...0...0
    [[0xa0, 0x80, 0x20, 0x00], [0x08, 0x00, 0x88, 0x80]], // ...0...1...0...1
    [[0xa0, 0x80, 0x20, 0x00], [0xa0, 0x80, 0x20, 0x00]], // ...0...1...1...0
    [[0xa0, 0x80, 0x20, 0x00], [0x00, 0x08, 0x20, 0x28]], // ...0...1...1...1
    [[0x88, 0x80, 0x08, 0x00], [0xa0, 0x80, 0x20, 0x00]], // ...1...0...0...0
    [[0x88, 0x80, 0x08, 0x00], [0x00, 0x08, 0x20, 0x28]], // ...1...0...0...1
    [[0x08, 0x28, 0x88, 0xa8], [0x08, 0x28, 0x88, 0xa8]], // ...1...0...1...0
    [[0xa0, 0x80, 0xa8, 0x88], [0xa0, 0x80, 0x20, 0x00]], // ...1...0...1...1
    [[0x08, 0x00, 0x88, 0x80], [0x88, 0x80, 0x08, 0x00]], // ...1...1...0...0
    [[0x00, 0x08, 0x20, 0x28], [0x88, 0x80, 0x08, 0x00]], // ...1...1...0...1
    [[0x08, 0x28, 0x88, 0xa8], [0x08, 0x28, 0x88, 0xa8]], // ...1...1...1...0
    [[0x08, 0x00, 0x88, 0x80], [0xa0, 0x80, 0x20, 0x00]], // ...1...1...1...1
];

/// The generic Sega table decoder (MAME segacrpt decode()). Row comes from address
/// bits 12, 8, 4, 0; column from data bits 5 and 3. When bit 7 is set the table is
/// read mirrored and the result is XORed with 0xA8.
fn decode_table(rom: &[u8], table: &ConvTable) -> Decrypted {
    let mut opcodes = rom.to_vec();
    let mut data = rom.to_vec();
    for a in 0..rom.len().min(0x8000) {
        let src = rom[a];
        let row = (a & 1) | ((a >> 4) & 1) << 1 | ((a >> 8) & 1) << 2 | ((a >> 12) & 1) << 3;
        let mut col = ((src >> 3) & 1) as usize | (((src >> 5) & 1) as usize) << 1;
        let mut xor = 0u8;
        if src & 0x80 != 0 {
            col = 3 - col;
            xor = 0xA8;
        }
        opcodes[a] = (src & !0xA8) | (table[row][0][col] ^ xor);
        data[a] = (src & !0xA8) | (table[row][1][col] ^ xor);
    }
    Decrypted { opcodes, data }
}

pub fn decode_315_5010(rom: &[u8]) -> Decrypted {
    decode_table(rom, &SEGA_315_5010)
}

/// 315-5007 (pengojb, penta, pengojbl). An older XOR scheme: data depends on A0,
/// opcodes on A4/A8/A12, and both on data bits 1, 3, 5 (mirrored when bit 7 is set).
pub fn decode_315_5007(rom: &[u8]) -> Decrypted {
    const DATA_XOR: [[u8; 8]; 2] = [
        [0xa0, 0x82, 0x28, 0x0a, 0x82, 0xa0, 0x0a, 0x28], // ...............0
        [0x88, 0x0a, 0x82, 0x00, 0x88, 0x0a, 0x82, 0x00], // ...............1
    ];
    const OPCODE_XOR: [[u8; 8]; 8] = [
        [0x02, 0x08, 0x2a, 0x20, 0x20, 0x2a, 0x08, 0x02], // ...0...0...0....
        [0x88, 0x88, 0x00, 0x00, 0x88, 0x88, 0x00, 0x00], // ...0...0...1....
        [0x88, 0x0a, 0x82, 0x00, 0xa0, 0x22, 0xaa, 0x28], // ...0...1...0....
        [0x88, 0x0a, 0x82, 0x00, 0xa0, 0x22, 0xaa, 0x28], // ...0...1...1....
        [0x2a, 0x08, 0x2a, 0x08, 0x8a, 0xa8, 0x8a, 0xa8], // ...1...0...0....
        [0x2a, 0x08, 0x2a, 0x08, 0x8a, 0xa8, 0x8a, 0xa8], // ...1...0...1....
        [0x88, 0x0a, 0x82, 0x00, 0xa0, 0x22, 0xaa, 0x28], // ...1...1...0....
        [0x88, 0x0a, 0x82, 0x00, 0xa0, 0x22, 0xaa, 0x28], // ...1...1...1....
    ];
    let mut opcodes = rom.to_vec();
    let mut data = rom.to_vec();
    for a in 0..rom.len().min(0x8000) {
        let src = rom[a];
        let mut j = ((src >> 1) & 1) as usize | (((src >> 3) & 1) as usize) << 1 | (((src >> 5) & 1) as usize) << 2;
        if src & 0x80 != 0 { j = 7 - j; }
        data[a] = src ^ DATA_XOR[a & 1][j];
        let i = ((a >> 4) & 1) | ((a >> 8) & 1) << 1 | ((a >> 12) & 1) << 2;
        opcodes[a] = src ^ OPCODE_XOR[i][j];
    }
    Decrypted { opcodes, data }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_scrambled_bits_change_and_top_half_is_untouched() {
        let rom: Vec<u8> = (0..0x10000).map(|i| (i * 31 + 7) as u8).collect();
        // 315-5010 scrambles bits 3, 5, 7; 315-5007 scrambles bits 1, 3, 5, 7
        for (d, keep) in [(decode_315_5010(&rom), 0x57u8), (decode_315_5007(&rom), 0x55u8)] {
            for a in 0..0x10000 {
                if a < 0x8000 {
                    assert_eq!(d.opcodes[a] & keep, rom[a] & keep);
                    assert_eq!(d.data[a] & keep, rom[a] & keep);
                } else {
                    assert_eq!((d.opcodes[a], d.data[a]), (rom[a], rom[a]));
                }
            }
        }
    }

    #[test]
    fn decoding_is_a_permutation_for_each_address_class() {
        // For a fixed address, the 256 possible encrypted bytes must decode to 256
        // different values, otherwise some instruction could never be expressed.
        for a in [0x0000usize, 0x0001, 0x0010, 0x0111, 0x1001, 0x1111, 0x7FFF] {
            let rom_at = |b: u8| { let mut r = vec![0u8; 0x8000]; r[a] = b; r };
            for decode in [decode_315_5010 as fn(&[u8]) -> Decrypted, decode_315_5007] {
                let mut seen_op = [false; 256];
                let mut seen_data = [false; 256];
                for b in 0..=255u8 {
                    let d = decode(&rom_at(b));
                    seen_op[d.opcodes[a] as usize] = true;
                    seen_data[d.data[a] as usize] = true;
                }
                assert!(seen_op.iter().all(|&x| x) && seen_data.iter().all(|&x| x), "addr {a:#x}");
            }
        }
    }

    #[test]
    fn known_values_from_the_5010_table() {
        // address 0 (row 0), src 0x00: col 0 -> opcode 0xA0, data 0x28
        let d = decode_315_5010(&[0x00]);
        assert_eq!((d.opcodes[0], d.data[0]), (0xA0, 0x28));
        // src 0x80 at address 0: col 3 mirrored, xor A8 -> opcode 0x88^0xA8 = 0x20, data 0x88^0xA8 = 0x20
        let d = decode_315_5010(&[0x80]);
        assert_eq!((d.opcodes[0], d.data[0]), (0x20, 0x20));
    }
}
