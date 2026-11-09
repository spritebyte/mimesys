//! Konami 007232 PCM controller (MAME k007232.cpp, BSD-3-Clause; Nicola Salmoria,
//! Hiromitsu Shioya).
//!
//! Two 7-bit PCM channels read from a 128 KB sample ROM. A sample ends at the first byte
//! with bit 7 set. The output rate is clock / 128; each channel has a volume for the
//! left and the right output. Register 12 is an external port (the boards use it as the
//! volume control), register 13 holds the two loop flags.

use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Serialize, Deserialize)]
struct Channel {
    vol: [i32; 2],
    addr: u32,
    counter: u32,
    start: u32,
    step: u32,
    play: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct K007232 {
    ch: [Channel; 2],
    wreg: [u8; 14],
    /// sample ROM size, capped at 128 KB: playback stops at the end of it
    pcmlimit: u32,
}

impl K007232 {
    pub fn new(rom_len: usize) -> Self {
        let mut k = Self { ch: Default::default(), wreg: [0; 14], pcmlimit: (rom_len as u32).min(1 << 17) };
        k.ch[0].counter = 0x1000;
        k.ch[1].counter = 0x1000;
        k.ch[0].vol = [255, 0];     // channel A to output A, channel B to output B
        k.ch[1].vol = [0, 255];
        k
    }

    pub fn set_volume(&mut self, channel: usize, a: i32, b: i32) {
        self.ch[channel].vol = [a, b];
    }

    /// Register write. Returns `Some(data)` for a write to the external port (12).
    pub fn write(&mut self, offset: usize, data: u8) -> Option<u8> {
        let offset = offset % 14;
        self.wreg[offset] = data;
        if offset == 12 { return Some(data); }
        if offset == 13 { return None; }
        let base = if offset >= 6 { 6 } else { 0 };
        let ch = &mut self.ch[(offset >= 6) as usize];
        match offset - base {
            0 | 1 => ch.step = ((self.wreg[base + 1] as u32 & 0x0F) << 8) | self.wreg[base] as u32,
            2..=4 => {
                ch.start = ((self.wreg[base + 4] as u32 & 1) << 16)
                    | ((self.wreg[base + 3] as u32) << 8) | self.wreg[base + 2] as u32;
            }
            _ => self.start_channel((offset >= 6) as usize),
        }
        None
    }

    fn start_channel(&mut self, i: usize) {
        let limit = self.pcmlimit;
        let ch = &mut self.ch[i];
        if ch.start < limit {
            ch.play = true;
            ch.addr = ch.start;
            ch.counter = 0x1000;
        }
    }

    /// Reading register 5 / 11 also starts the channel.
    pub fn read(&mut self, offset: usize) -> u8 {
        if offset == 5 { self.start_channel(0); }
        if offset == 11 { self.start_channel(1); }
        0
    }

    /// One output sample (every 128 clocks): left, right, scaled to +-32768.
    pub fn sample(&mut self, rom: &[u8]) -> [i32; 2] {
        let (mut l, mut r) = (0i32, 0i32);
        let limit = self.pcmlimit;
        let byte = |a: u32| rom.get(a as usize).copied().unwrap_or(0x80);
        for i in 0..2 {
            let looped = self.wreg[13] >> i & 1 != 0;
            let ch = &mut self.ch[i];
            if !ch.play { continue; }
            let (vol_a, vol_b) = (ch.vol[0] * 2, ch.vol[1] * 2);
            let mut addr = ch.addr & 0x1FFFF;
            while ch.counter <= ch.step {
                // result: clock / (4 * (4096 - frequency))
                let end = if addr >= limit { true } else { let b = byte(addr); addr += 1; b & 0x80 != 0 };
                if end {
                    if looped { addr = ch.start; } else { ch.play = false; break; }
                }
                ch.counter += 0x1000 - ch.step;
            }
            ch.addr = addr;
            // MAME leaves the channel loop here when the sample ended
            if !ch.play { break; }
            ch.counter -= 32;
            let out = (byte(addr) & 0x7F) as i32 - 0x40;
            l += out * vol_a;
            r += out * vol_b;
        }
        [l, r]
    }

    #[cfg(test)]
    pub fn playing(&self, i: usize) -> bool { self.ch[i].play }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rom() -> Vec<u8> {
        // a ramp of 0x50..0x5F ending in an end mark
        let mut r = vec![0x80u8; 0x100];
        for i in 0..16 { r[0x10 + i] = 0x50 + i as u8; }
        r
    }

    #[test]
    fn plays_until_the_end_mark() {
        let rom = rom();
        let mut k = K007232::new(0x20000);
        k.write(0, 0xE0); k.write(1, 0x0F);        // step 0xFE0: one ROM byte a sample (32 / (0x1000 - step))
        k.write(2, 0x10); k.write(3, 0x00); k.write(4, 0x00);
        k.write(5, 0);                              // start
        assert!(k.playing(0));
        let mut out = Vec::new();
        for _ in 0..80 { out.push(k.sample(&rom)); if !k.playing(0) { break; } }
        assert!(!k.playing(0), "stops at the end mark");
        assert!(out.len() > 16 && out.len() < 80);
        // channel A drives the left output only, with the ROM value - 0x40, volume 255 * 2
        assert!(out.iter().all(|s| s[1] == 0));
        let first = out.iter().find(|s| s[0] != 0).unwrap();
        assert_eq!(first[0], 0x10 * 510);
    }

    #[test]
    fn loop_flag_restarts_and_volume_port() {
        let rom = rom();
        let mut k = K007232::new(0x20000);
        k.write(13, 0x01);
        k.write(0, 0x00); k.write(1, 0x0F);
        k.write(2, 0x10); k.write(3, 0x00); k.write(4, 0x00);
        k.write(5, 0);
        for _ in 0..500 { k.sample(&rom); }
        assert!(k.playing(0), "looping channel keeps playing");
        assert_eq!(k.write(12, 0x5A), Some(0x5A));
        k.set_volume(0, 0, 0x11);
        let s = k.sample(&rom);
        assert_eq!(s[0], 0);
        assert!(s[1] != 0);
    }

    #[test]
    fn start_beyond_the_rom_is_ignored() {
        let mut k = K007232::new(0x100);
        k.write(2, 0x00); k.write(3, 0x02); k.write(4, 0);   // start 0x200 > 0x100
        k.write(5, 0);
        assert!(!k.playing(0));
        assert_eq!(k.read(11), 0);                           // channel B start from 0
        assert!(k.playing(1));
    }
}
