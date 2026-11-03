//! The Sega CD's sample player (Ricoh RF5C164, "PCM"): 8 channels playing 8-bit
//! sign-magnitude samples from its own 64 KB of wave RAM, each with a volume
//! ("envelope"), a left / right pan, a pitch step and a loop point; a 0xFF byte means
//! "loop". It makes one stereo sample every 384 sub clocks (32.55 kHz).
//! Sub CPU: FF0000-FF7FFF odd bytes; registers 0-8, wave RAM window at 1000-1FFF
//! (one of 16 4 KB banks). Ported from ares md/mcd/pcm.cpp (ISC license).

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct Channel {
    pub enable: bool,
    pub envelope: u8,
    pub pan: u8,
    pub step: u16,
    pub loop_at: u16,
    pub start: u8,
    /// position in wave RAM, 16.11 fixed point (27 bits)
    pub address: u32,
}

impl Default for Channel {
    fn default() -> Self { Self { enable: false, envelope: 0, pan: 0xFF, step: 0, loop_at: 0, start: 0, address: 0 } }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Pcm {
    #[serde(with = "serde_bytes")]
    pub ram: Vec<u8>,
    pub enable: bool,
    pub bank: u8,
    pub channel: u8,
    pub channels: [Channel; 8],
}

impl Default for Pcm {
    fn default() -> Self { Self { ram: vec![0; 0x10000], enable: false, bank: 0, channel: 0, channels: [Channel::default(); 8] } }
}

impl Pcm {
    /// One output sample (left, right), as the 10-bit DAC gives it (-32768..32767 scale).
    pub fn clock(&mut self) -> (i32, i32) {
        let (mut left, mut right) = (0i32, 0i32);
        if self.enable {
            for c in self.channels.iter_mut() {
                if !c.enable { continue; }
                let mut data = self.ram[(c.address >> 11) as usize & 0xFFFF] as i32;
                if data == 0xFF {
                    // loop marker
                    c.address = (c.loop_at as u32) << 11;
                    data = self.ram[c.loop_at as usize] as i32;
                    if data == 0xFF { continue; } // a loop onto a loop marker: silence
                } else {
                    c.address = (c.address + c.step as u32) & 0x7FF_FFFF;
                }
                let s = if data & 0x80 != 0 { data & 0x7F } else { -(data & 0x7F) };
                left += s * c.envelope as i32 * (c.pan & 15) as i32 >> 5;
                right += s * c.envelope as i32 * (c.pan >> 4) as i32 >> 5;
            }
        }
        // 10-bit DAC
        let dac = |v: i32| (v.clamp(-32768, 32767) >> 2) & !15;
        (dac(left), dac(right))
    }

    /// Register / wave RAM read (`a` = the sub address / 2, 13 bits).
    pub fn read(&self, a: u32) -> u8 {
        let a = a & 0x1FFF;
        match a {
            0x1000..=0x1FFF => self.ram[((self.bank as u32) << 12 | (a & 0xFFF)) as usize],
            0x0010..=0x001F => {
                // where each channel is playing (high byte at odd addresses)
                let pos = (self.channels[(a >> 1 & 7) as usize].address >> 11) as u16;
                if a & 1 != 0 { (pos >> 8) as u8 } else { pos as u8 }
            }
            _ => 0xFF,
        }
    }

    pub fn write(&mut self, a: u32, v: u8) {
        let a = a & 0x1FFF;
        if (0x1000..=0x1FFF).contains(&a) {
            self.ram[((self.bank as u32) << 12 | (a & 0xFFF)) as usize] = v;
            return;
        }
        let c = &mut self.channels[self.channel as usize];
        match a {
            0x0 => c.envelope = v,
            0x1 => c.pan = v,
            0x2 => c.step = (c.step & 0xFF00) | v as u16,
            0x3 => c.step = (c.step & 0x00FF) | (v as u16) << 8,
            0x4 => c.loop_at = (c.loop_at & 0xFF00) | v as u16,
            0x5 => c.loop_at = (c.loop_at & 0x00FF) | (v as u16) << 8,
            0x6 => {
                c.start = v;
                if !c.enable { c.address = (v as u32) << 8 << 11; }
            }
            0x7 => {
                if v & 0x40 != 0 { self.channel = v & 7; } else { self.bank = v & 15; }
                self.enable = v & 0x80 != 0;
            }
            0x8 => {
                // a 0 bit turns a channel on; channels that are off sit at their start
                for (i, c) in self.channels.iter_mut().enumerate() {
                    c.enable = v >> i & 1 == 0;
                    if !c.enable { c.address = (c.start as u32) << 8 << 11; }
                }
            }
            _ => {}
        }
    }
}
