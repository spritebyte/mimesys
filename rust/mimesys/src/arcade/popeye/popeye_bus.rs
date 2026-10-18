use crate::common::cpu::z80::Bus;
use crate::common::audio::ay8910::Ay8910;
use crate::arcade::popeye::popeye_video::{PopeyeVideo, DMA_SIZE};

// Popeye (TPP2 board) memory map, MAME tpp2_state::maincpu_program_map:
//   0000-7FFF  ROM (scrambled on the board, unscrambled at load)
//   8800-8BFF  RAM
//   8C00-8E7F  RAM, copied to the video chip by DMA every vblank (sprites, scroll, palette)
//   8E80-8FFF  RAM
//   A000-A3FF  W: text tile codes      A400-A7FF  W: text tile colors
//   C000-DFFF  W: background pixels (A12 picks the nibble)
//   E000-E001  protection chip (missing on the "noalu" sets)
// I/O ports (low byte only):
//   W 00: AY register select   W 01: AY data
//   R 00: P1   R 01: P2   R 02: system   R 03: AY data
// AY port A = DIP switches 0 (bit 7 = one bit of DIP switches 1, picked by port B)
// AY port B = bit 0 flip screen, bits 1-3 which DSW1 bit appears on DSW0 bit 7

pub struct PopeyeBus {
    rom: Vec<u8>,
    ram: Vec<u8>,                 // 0x8800-0x8FFF
    pub video: PopeyeVideo,
    pub ay: Ay8910,
    has_protection: bool,

    // protection chip: returns the last two values written, shifted
    prot0: u8,
    prot1: u8,
    prot_shift: u8,

    // Inputs are ACTIVE HIGH on this board (1 = pressed)
    pub p1: u8,
    pub p2: u8,
    pub system: u8,               // bit 4 is the video field, filled in on read
    pub field: u8,
    pub dsw0: u8,
    pub dsw1: u8,
    dsw_bit: u8,
}

impl PopeyeBus {
    pub fn new(rom: Vec<u8>, video: PopeyeVideo, ay: Ay8910, has_protection: bool) -> Self {
        Self {
            rom,
            ram: vec![0; 0x800],
            video,
            ay,
            has_protection,
            prot0: 0,
            prot1: 0,
            prot_shift: 0,
            p1: 0,
            p2: 0,
            system: 0,
            field: 0,
            // 1 coin 1 credit, bit 4 unused (high), "Nintendo" copyright
            dsw0: 0x5F,
            // 3 lives, easy, bonus at 40000, demo sounds on, upright
            dsw1: 0x3D,
            dsw_bit: 0,
        }
    }

    pub fn dma_source(&self) -> &[u8] {
        &self.ram[0x400..0x400 + DMA_SIZE]   // 0x8C00-0x8E7F
    }

    fn port_a(&self) -> u8 {
        (self.dsw0 & 0x7F) | (((self.dsw1 >> self.dsw_bit) & 1) << 7)
    }
}

impl Bus for PopeyeBus {
    fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x7FFF => self.rom[addr as usize],
            0x8800..=0x8FFF => self.ram[(addr - 0x8800) as usize],
            0xE000 if self.has_protection => {
                ((((self.prot1 as u16) << self.prot_shift) | ((self.prot0 as u16) >> (8 - self.prot_shift))) & 0xFF) as u8
            }
            0xE001 if self.has_protection => 0,   // the game only checks that bit 2 is clear
            _ => 0,                               // unpopulated / write-only areas
        }
    }

    fn write(&mut self, addr: u16, val: u8) {
        match addr {
            0x8800..=0x8FFF => self.ram[(addr - 0x8800) as usize] = val,
            0xA000..=0xA3FF => self.video.video_ram[(addr & 0x3FF) as usize] = val,
            0xA400..=0xA7FF => self.video.color_ram[(addr & 0x3FF) as usize] = val,
            0xC000..=0xDFFF => self.video.background_write(addr - 0xC000, val),
            0xE000 if self.has_protection => self.prot_shift = val & 0x07,   // the level number
            0xE001 if self.has_protection => {
                self.prot0 = self.prot1;
                self.prot1 = val;
            }
            _ => {}
        }
    }

    fn port_in(&mut self, port: u16) -> u8 {
        match port & 0xFF {
            0x00 => self.p1,
            0x01 => self.p2,
            0x02 => (self.system & !0x10) | ((self.field ^ 1) << 4),   // inverted even/odd field
            0x03 => self.ay.read_data(self.port_a()),
            _ => 0,
        }
    }

    fn port_out(&mut self, port: u16, val: u8) {
        match port & 0xFF {
            0x00 => self.ay.write_address(val),
            0x01 => {
                if let Some(b) = self.ay.write_data(val) {
                    self.video.flip = b & 1 != 0;
                    self.dsw_bit = (b & 0x0E) >> 1;
                }
            }
            _ => {}
        }
    }
}
