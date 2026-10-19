// Hudson HuC6260 VCE (Video Color Encoder): the palette and the pixel clock.
// 512 colors of 9 bits (GGGRRRBBB): entries 0-255 = 16 background palettes,
// 256-511 = 16 sprite palettes. Entry 0 is the backdrop, entry 0x100 the overscan border.
//
// CPU side (physical 0x1FE400-0x1FE7FF, A0-A2):
//   0  control: bits 0-1 pixel clock (0 = 5.37 MHz ~256 wide, 1 = 7.16 MHz ~341, 2 = 10.7 MHz ~512)
//   2/3  color table address low / high (9 bits)
//   4/5  color data low / high; writing (or reading) the high byte steps the address

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub struct Vce {
    pub ctrl: u8,
    addr: u16,
    pub palette: Vec<u16>,
    /// palette as ready-to-copy RGBA bytes (rebuilt on load)
    #[serde(skip)]
    rgba: Vec<[u8; 4]>,
}

impl Default for Vce {
    fn default() -> Self { Self::new() }
}

impl Vce {
    pub fn new() -> Self {
        let mut v = Self { ctrl: 0, addr: 0, palette: vec![0; 512], rgba: vec![[0, 0, 0, 0xFF]; 512] };
        v.rebuild();
        v
    }

    /// Recompute the RGBA cache (call after loading a save state).
    pub fn rebuild(&mut self) {
        self.rgba.resize(512, [0, 0, 0, 0xFF]);
        for i in 0..512 { self.update(i); }
    }

    fn update(&mut self, i: usize) {
        let [r, g, b] = self.rgb(i as u16);
        self.rgba[i] = [r, g, b, 0xFF];
    }

    #[inline]
    pub fn rgba(&self, index: u16) -> [u8; 4] { self.rgba[index as usize & 0x1FF] }

    pub fn write(&mut self, port: u32, v: u8) {
        match port & 7 {
            0 => self.ctrl = v,
            2 => self.addr = (self.addr & 0x100) | v as u16,
            3 => self.addr = (self.addr & 0x0FF) | ((v as u16 & 1) << 8),
            4 => {
                let a = self.addr as usize;
                self.palette[a] = (self.palette[a] & 0x100) | v as u16;
                self.update(a);
            }
            5 => {
                let a = self.addr as usize;
                self.palette[a] = (self.palette[a] & 0x0FF) | ((v as u16 & 1) << 8);
                self.update(a);
                self.addr = (self.addr + 1) & 0x1FF;
            }
            _ => {}
        }
    }

    pub fn read(&mut self, port: u32) -> u8 {
        match port & 7 {
            4 => self.palette[self.addr as usize] as u8,
            5 => {
                let v = 0xFE | (self.palette[self.addr as usize] >> 8) as u8;
                self.addr = (self.addr + 1) & 0x1FF;
                v
            }
            _ => 0xFF,
        }
    }

    pub fn rgb(&self, index: u16) -> [u8; 3] {
        let c = self.palette[index as usize & 0x1FF];
        let scale = |v: u16| ((v & 7) * 255 / 7) as u8;
        [scale(c >> 3), scale(c >> 6), scale(c)]
    }
}
