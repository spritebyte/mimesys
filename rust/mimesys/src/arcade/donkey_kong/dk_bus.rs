use crate::common::cpu::z80::Bus;
use crate::arcade::donkey_kong::dk_video::DKVideo;
use crate::arcade::donkey_kong::dk_dma::I8257;
use crate::arcade::donkey_kong::dk_sound::DkSoundIo;

pub struct DKBus {
    rom: Vec<u8>,
    ram: Vec<u8>,
    pub video: DKVideo,
    dma: I8257,

    pub in0: u8,
    pub in1: u8,
    pub in2: u8,
    pub dsw0: u8,

    pub nmi_mask: bool,

    pub sound: DkSoundIo,
}

impl DKBus {
    pub fn new(rom: Vec<u8>, video: DKVideo, sound_rom: Vec<u8>) -> Self {
        Self {
            rom,
            ram: vec![0; 0x0C00],
            video,
            dma: I8257::default(),
            in0: 0, in1: 0, in2: 0,
            dsw0: 0x80,
            nmi_mask: false,
            sound: DkSoundIo::new(sound_rom),
        }
    }

    fn run_dma(&mut self) {
        let len = self.dma.length(0);
        let (src, dst) = (self.dma.addr[0], self.dma.addr[1]);
        for i in 0..len {
            let v = self.read(src.wrapping_add(i));
            self.write(dst.wrapping_add(i), v);
        }
    }
}

impl Bus for DKBus {
    fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x4FFF => self.rom.get(addr as usize).copied().unwrap_or(0),
            0x6000..=0x6BFF => self.ram[(addr - 0x6000) as usize],
            0x7000..=0x73FF => self.video.sprite_ram[(addr - 0x7000) as usize],
            0x7400..=0x77FF => self.video.video_ram[(addr - 0x7400) as usize],
            0x7C00 => self.in0,
            0x7C80 => self.in1,
            0x7D00 => (self.in2 & !0x40) | if self.sound.status_bit() { 0x40 } else { 0 },
            0x7D80 => self.dsw0,
            _ => 0,
        }
    }

    fn write(&mut self, addr: u16, val: u8) {
        match addr {
            0x6000..=0x6BFF => self.ram[(addr - 0x6000) as usize] = val,
            0x7000..=0x73FF => self.video.sprite_ram[(addr - 0x7000) as usize] = val,
            0x7400..=0x77FF => self.video.video_ram[(addr - 0x7400) as usize] = val,
            0x7800..=0x780F => self.dma.write((addr & 0x0F) as u8, val),
            0x7C00 => self.sound.cmd_latch = val,
            0x7D00..=0x7D07 => {
                let bit = 1 << (addr & 7);
                if val & 1 != 0 { self.sound.sound_bits |= bit } else { self.sound.sound_bits &= !bit }
            }
            0x7D80 => self.sound.irq = val != 0 ,
            0x7D82 => self.video.flip = val & 1 != 0,
            0x7D83 => self.video.sprite_bank = val & 1,
            0x7D84 => self.nmi_mask = val & 1 != 0,
            0x7D85 => if val & 1 != 0 { self.run_dma() },
            0x7D86 | 0x7D87 => {
                let bit = 1 << (addr & 1);
                if val & 1 != 0 { self.video.palette_bank |= bit } else { self.video.palette_bank &= !bit }
            }
            _ => {}
        }
    }

    fn port_in(&mut self, port: u16) -> u8 { 0 }
    fn port_out(&mut self, port: u16, val: u8) { }
}