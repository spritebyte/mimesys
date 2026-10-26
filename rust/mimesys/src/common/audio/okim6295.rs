//! OKI MSM6295 4-voice ADPCM player, a port of MAME's okim6295.cpp / okiadpcm.cpp
//! (Mirko Buffoni, R. Belmont, Aaron Giles).
//!
//! The ROM starts with a table of 8-byte entries (18-bit start and stop addresses).
//! Commands: `1sss ssss` picks sample s; the next byte `vvvv aaaa` starts it on the
//! voices whose bit is set in vvvv, at attenuation aaaa. `0vvv v000` stops voices.
//! Status: bits 0-3 = voice playing, 4-7 read 1. One output sample every 132
//! (pin 7 high) or 165 (pin 7 low) clocks.

use serde::{Deserialize, Serialize};

const INDEX_SHIFT: [i32; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];
/// MAME s_volume_table, in 32nds
const VOLUME: [i32; 16] = [0x20, 0x16, 0x10, 0x0B, 0x08, 0x06, 0x04, 0x03, 0x02, 0, 0, 0, 0, 0, 0, 0];

#[derive(Serialize, Deserialize, Clone, Copy, Default)]
struct Voice {
    playing: bool,
    base: u32,
    sample: u32,
    count: u32,
    signal: i32,
    step: i32,
    volume: i32,
}

#[derive(Serialize, Deserialize)]
pub struct Okim6295 {
    #[serde(skip)] rom: Vec<u8>,
    #[serde(skip, default = "diff_table")] diff: Vec<i32>,
    command: i32,
    voices: [Voice; 4],
    pub pin7_high: bool,
}

/// okiadpcm compute_tables: step x nibble.
fn diff_table() -> Vec<i32> {
    const NBL2BIT: [[i32; 4]; 16] = [
        [1, 0, 0, 0], [1, 0, 0, 1], [1, 0, 1, 0], [1, 0, 1, 1],
        [1, 1, 0, 0], [1, 1, 0, 1], [1, 1, 1, 0], [1, 1, 1, 1],
        [-1, 0, 0, 0], [-1, 0, 0, 1], [-1, 0, 1, 0], [-1, 0, 1, 1],
        [-1, 1, 0, 0], [-1, 1, 0, 1], [-1, 1, 1, 0], [-1, 1, 1, 1],
    ];
    let mut d = vec![0; 49 * 16];
    for step in 0..=48 {
        let sv = (16.0 * 1.1f64.powi(step)).floor() as i32;
        for (nib, n) in NBL2BIT.iter().enumerate() {
            d[step as usize * 16 + nib] = n[0] * (sv * n[1] + sv / 2 * n[2] + sv / 4 * n[3] + sv / 8);
        }
    }
    d
}

impl Okim6295 {
    pub fn new(rom: Vec<u8>, pin7_high: bool) -> Self {
        Self { rom, diff: diff_table(), command: -1, voices: [Voice::default(); 4], pin7_high }
    }

    /// Output samples per second for a given chip clock.
    pub fn sample_rate(&self, clock: f64) -> f64 { clock / if self.pin7_high { 132.0 } else { 165.0 } }

    pub fn keep_static(&mut self, old: &mut Self) { self.rom = std::mem::take(&mut old.rom); }

    pub fn reset(&mut self) {
        for v in &mut self.voices { v.playing = false; }
    }

    fn byte(&self, a: u32) -> u8 { self.rom.get((a & 0x3FFFF) as usize).copied().unwrap_or(0) }

    pub fn read(&self) -> u8 {
        let mut r = 0xF0;
        for (i, v) in self.voices.iter().enumerate() { if v.playing { r |= 1 << i; } }
        r
    }

    pub fn write(&mut self, command: u8) {
        if self.command != -1 {
            let base = self.command as u32 * 8;
            let start = (((self.byte(base) as u32) << 16) | ((self.byte(base + 1) as u32) << 8) | self.byte(base + 2) as u32) & 0x3FFFF;
            let stop = (((self.byte(base + 3) as u32) << 16) | ((self.byte(base + 4) as u32) << 8) | self.byte(base + 5) as u32) & 0x3FFFF;
            let mut mask = command >> 4;
            for v in &mut self.voices {
                if mask & 1 != 0 && !v.playing && start < stop {
                    *v = Voice {
                        playing: true, base: start, sample: 0, count: 2 * (stop - start + 1),
                        signal: 0, step: 0, volume: VOLUME[(command & 0x0F) as usize],
                    };
                }
                mask >>= 1;
            }
            self.command = -1;
        } else if command & 0x80 != 0 {
            self.command = (command & 0x7F) as i32;
        } else {
            let mut mask = command >> 3;
            for v in &mut self.voices {
                if mask & 1 != 0 { v.playing = false; }
                mask >>= 1;
            }
        }
    }

    /// Next output sample, -1.0..1.0 per voice (MAME add_int(signal * volume, 2048)).
    pub fn sample(&mut self) -> f32 {
        let mut out = 0.0f32;
        for i in 0..4 {
            let v = self.voices[i];
            if !v.playing { continue; }
            let shift = ((v.sample & 1) << 2) ^ 4;
            let nib = (self.byte(v.base + v.sample / 2) >> shift) as usize & 15;
            let v = &mut self.voices[i];
            v.signal = (v.signal + self.diff[v.step as usize * 16 + nib]).clamp(-2048, 2047);
            v.step = (v.step + INDEX_SHIFT[nib & 7]).clamp(0, 48);
            out += v.signal as f32 * v.volume as f32 / 32.0;
            v.sample += 1;
            if v.sample >= v.count { v.playing = false; }
        }
        out / 2048.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plays_a_sample_and_stops() {
        let mut rom = vec![0u8; 0x1000];
        // sample 1: 0x400..0x40F
        rom[8..14].copy_from_slice(&[0x00, 0x04, 0x00, 0x00, 0x04, 0x0F]);
        for b in &mut rom[0x400..0x410] { *b = 0x77; }
        let mut oki = Okim6295::new(rom, true);
        assert_eq!(oki.sample_rate(1_056_000.0), 8000.0);
        oki.write(0x81);
        oki.write(0x20);            // voice 1, full volume
        assert_eq!(oki.read(), 0xF2);
        let s: Vec<f32> = (0..40).map(|_| oki.sample()).collect();
        assert!(s[0] > 0.0 && s[31] > s[0], "rising: {:?}", &s[..4]);
        assert_eq!(oki.read(), 0xF0, "32 nibbles then stop");
        // a stop command
        oki.write(0x81); oki.write(0x10);
        oki.write(0x08);
        assert_eq!(oki.read(), 0xF0);
    }
}
