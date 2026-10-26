// Gottlieb GG-III main board memory map (MAME gottlieb_base_map + gottlieb_ram_map).
// 8088 with A16-A19 ignored (global mask 0xFFFF):
//   0000-0FFF  NVRAM (battery backed, high scores and settings)
//   1000-2FFF  RAM
//   3000-30FF  sprite RAM, write only (mirror 0x0700)
//   3800-3BFF  video RAM (mirror 0x0400)
//   4000-4FFF  char RAM
//   5000-501F  palette RAM, write only (mirror 0x07E0)
//   5800-5807  (mirror 0x07F8)  R: DSW, IN1, IN2, IN3, IN4
//                               W: watchdog, analog reset, sound command, general output
//   6000-FFFF  ROM

use crate::common::cpu::i8088::I8088Bus;
use crate::arcade::gottlieb::gottlieb_video::GottliebVideo;
use crate::arcade::gottlieb::gottlieb_sound::GottliebSound;

pub struct GottliebBus {
    pub rom: Vec<u8>,           // 64 KB, ROM from 0x6000
    pub nvram: [u8; 0x1000],
    pub nvram_dirty: bool,
    pub ram: [u8; 0x2000],
    pub video: GottliebVideo,
    pub sound: GottliebSound,
    pub dsw: u8,
    pub in1: u8,
    pub in2: u8,
    pub in3: u8,
    pub in4: u8,
    pub watchdog: u32,          // vblanks since the last kick
    pub output: u8,
    pub(crate) knocker_prev: bool,
    /// Q*bert: bit 5 of the output latch drives the knocker instead of the coin counter
    pub has_knocker: bool,
}

impl GottliebBus {
    pub fn new(rom: Vec<u8>, video: GottliebVideo, sound: GottliebSound) -> Self {
        let mut full = vec![0u8; 0x10000];
        let n = rom.len().min(0x10000);
        full[..n].copy_from_slice(&rom[..n]);
        Self {
            rom: full, nvram: [0xFF; 0x1000], nvram_dirty: false, ram: [0; 0x2000],
            video, sound,
            dsw: 0x02, in1: 0x40, in2: 0xFF, in3: 0xFF, in4: 0x00,
            watchdog: 0, output: 0, knocker_prev: false, has_knocker: true,
        }
    }

    fn output_write(&mut self, v: u8) {
        self.output = v;
        self.video.control_write(v & 0x07);
        if self.has_knocker {
            let knock = v & 0x20 != 0;
            if knock && !self.knocker_prev { self.sound.knock(); }
            self.knocker_prev = knock;
        }
    }
}

impl I8088Bus for GottliebBus {
    fn read8(&mut self, addr: u32) -> u8 {
        let a = (addr & 0xFFFF) as usize;
        match a {
            0x0000..=0x0FFF => self.nvram[a],
            0x1000..=0x2FFF => self.ram[a - 0x1000],
            0x3000..=0x37FF => 0xFF,                       // sprite RAM is write only
            0x3800..=0x3FFF => self.video.videoram[a & 0x3FF],
            0x4000..=0x4FFF => self.video.charram[a & 0xFFF],
            0x5000..=0x57FF => 0xFF,
            0x5800..=0x5FFF => match a & 7 {
                0 => self.dsw,
                1 => self.in1,
                2 => self.in2,
                3 => self.in3,
                4 => self.in4,
                _ => 0xFF,
            },
            _ => self.rom[a],
        }
    }

    fn write8(&mut self, addr: u32, v: u8) {
        let a = (addr & 0xFFFF) as usize;
        match a {
            0x0000..=0x0FFF => {
                if self.nvram[a] != v { self.nvram[a] = v; self.nvram_dirty = true; }
            }
            0x1000..=0x2FFF => self.ram[a - 0x1000] = v,
            0x3000..=0x37FF => self.video.spriteram[a & 0xFF] = v,
            0x3800..=0x3FFF => self.video.videoram[a & 0x3FF] = v,
            0x4000..=0x4FFF => self.video.charram[a & 0xFFF] = v,
            0x5000..=0x57FF => self.video.palette_write(a & 0x1F, v),
            0x5800..=0x5FFF => match a & 7 {
                0 => self.watchdog = 0,
                1 => {}                                  // trackball counters reset
                2 => self.sound.command(v),
                3 => self.output_write(v),
                _ => {}
            },
            _ => {}
        }
    }
}
