//! Maple bus: the DMA that walks the command table in system RAM, and Dreamcast
//! controllers on ports A-D. After MAME mame/sega/maple-dc.cpp, mapledev.cpp and
//! dc-ctrl.cpp (BSD-3-Clause).
//!
//! Table entry: word 0 = last flag (bit 31), port (16-17), pattern (8-10), send length in
//! words - 1 (0-7); word 1 = where the reply goes; then the frame: command (0-7),
//! recipient (8-15), sender (16-23), data length (24-31), data. A reply frame has the same
//! layout with the addresses swapped; a port without a device answers FFFFFFFF.

use serde::{Deserialize, Serialize};

/// A controller's state: buttons active low (C B A Start Up Down Left Right in bits 0-7,
/// Z Y X D in 8-11), triggers 0-255, stick 0-255 (0x80 = centre).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DcPad {
    pub buttons: u16,
    pub rt: u8,
    pub lt: u8,
    pub x: u8,
    pub y: u8,
}

impl Default for DcPad {
    fn default() -> Self { Self { buttons: 0xFFFF, rt: 0, lt: 0, x: 0x80, y: 0x80 } }
}

pub const BTN_B: u16 = 1 << 1;
pub const BTN_A: u16 = 1 << 2;
pub const BTN_START: u16 = 1 << 3;
pub const BTN_UP: u16 = 1 << 4;
pub const BTN_DOWN: u16 = 1 << 5;
pub const BTN_LEFT: u16 = 1 << 6;
pub const BTN_RIGHT: u16 = 1 << 7;
pub const BTN_Y: u16 = 1 << 9;
pub const BTN_X: u16 = 1 << 10;

const FUNC_CONTROLLER: u32 = 0x0100_0000;

fn put_str(out: &mut Vec<u8>, s: &str, len: usize) {
    let b = s.as_bytes();
    for k in 0..len { out.push(*b.get(k).unwrap_or(&b' ')); }
}

/// The 28-word device information block of a standard controller.
pub fn controller_info() -> Vec<u32> {
    let mut b = Vec::with_capacity(112);
    b.extend(FUNC_CONTROLLER.to_le_bytes());
    // function data: the buttons / axes a standard controller has
    b.extend(0xFE06_0F00u32.to_le_bytes());
    b.extend([0; 8]);
    b.extend([0xFF, 0]); // area code (every region), connector direction
    put_str(&mut b, "Dreamcast Controller", 30);
    put_str(&mut b, "Produced By or Under License From SEGA ENTERPRISES,LTD.", 60);
    b.extend(0x01F4_01AEu32.to_le_bytes()); // standby 43 mA, max 50 mA
    b.chunks(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect()
}

/// A controller's answer to one frame (None: no reply).
pub fn controller_reply(frame: &[u32], pad: &DcPad, port: u32) -> Option<Vec<u32>> {
    let f = *frame.first()?;
    let cmd = f & 0xFF;
    let (recipient, sender) = (f >> 8 & 0xFF, f >> 16 & 0xFF);
    // we are the main unit at 0x20 of this port; nothing plugged into the controller
    let me = 0x20 | port << 6;
    let _ = recipient;
    let mut data: Vec<u32> = match cmd {
        0x01 => controller_info(),
        0x02 => {
            let mut d = controller_info();
            let mut v = Vec::new();
            put_str(&mut v, "Version 1.000,1998/05/11,315-6215-AB   ,Analog Module: The 4th Edition. 05/08", 80);
            d.extend(v.chunks(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())));
            d
        }
        0x03 => Vec::new(),
        0x09 => vec![
            FUNC_CONTROLLER,
            pad.buttons as u32 | (pad.rt as u32) << 16 | (pad.lt as u32) << 24,
            pad.x as u32 | (pad.y as u32) << 8 | 0x80 << 16 | 0x80 << 24,
        ],
        _ => return Some(vec![0xFD | sender << 8 | me << 16]), // unknown command
    };
    let code = match cmd { 0x01 => 0x05, 0x02 => 0x06, 0x03 => 0x07, _ => 0x08 };
    let mut out = vec![code | sender << 8 | me << 16 | (data.len() as u32) << 24];
    out.append(&mut data);
    Some(out)
}

/// Run a Maple DMA over `ram` (16 MB, the table and the replies live there); `pads`:
/// the controllers on ports A-D (None = nothing plugged in). Returns the number of
/// table entries processed.
pub fn dma(ram: &mut [u8], table: u32, pads: &[Option<DcPad>; 4]) -> usize {
    let mask = ram.len() - 1;
    let rd = |ram: &[u8], a: u32| { let i = a as usize & mask & !3; u32::from_le_bytes(ram[i..i + 4].try_into().unwrap()) };
    let mut a = table;
    let mut n = 0;
    loop {
        let head = rd(ram, a);
        let dest = rd(ram, a + 4);
        let len = (head & 0xFF) + 1;
        let port = head >> 16 & 3;
        let pattern = head >> 8 & 7;
        let frame: Vec<u32> = (0..len).map(|k| rd(ram, a + 8 + 4 * k)).collect();
        a = a.wrapping_add(8 + 4 * len);
        n += 1;
        if pattern == 0 {
            let reply = match &pads[port as usize] {
                Some(p) => controller_reply(&frame, p, port).unwrap_or_else(|| vec![0xFFFF_FFFF]),
                None => vec![0xFFFF_FFFF],
            };
            for (k, w) in reply.iter().enumerate() {
                let i = (dest as usize).wrapping_add(4 * k) & mask & !3;
                ram[i..i + 4].copy_from_slice(&w.to_le_bytes());
            }
        }
        if head >> 31 != 0 || n >= 64 { break; }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_block_layout() {
        let d = controller_info();
        assert_eq!(d.len(), 28);
        assert_eq!(d[0], 0x0100_0000);
        let bytes: Vec<u8> = d.iter().flat_map(|w| w.to_le_bytes()).collect();
        assert_eq!(&bytes[18..38], b"Dreamcast Controller");
        assert_eq!(bytes[16], 0xFF);
        assert_eq!(d[27], 0x01F4_01AE);
    }
}
