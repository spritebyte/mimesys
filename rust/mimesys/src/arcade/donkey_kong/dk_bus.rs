use crate::common::cpu::z80::Bus;
use crate::arcade::donkey_kong::dk_system::Variant;
use crate::arcade::donkey_kong::dk_video::DKVideo;
use crate::arcade::donkey_kong::dk_dma::I8257;
use crate::arcade::donkey_kong::dk_sound::DkSoundIo;

pub struct DKBus {
    rom: Vec<u8>,
    ram: Vec<u8>,
    pub video: DKVideo,
    dma: I8257,
    variant: Variant,

    pub in0: u8,
    pub in1: u8,
    pub in2: u8,
    pub dsw0: u8,

    pub nmi_mask: bool,

    pub sound: DkSoundIo,
}

/// Save state: RAMs, DMA controller, sound latches (no ROMs or graphics).
pub type DkBusState = (Vec<u8>, Vec<u8>, Vec<u8>, bool, u8, u8, u8, I8257, bool, DkSoundIo);

impl DKBus {
    pub fn save(&self) -> impl serde::Serialize + '_ {
        let v = &self.video;
        (&self.ram, &v.video_ram, &v.sprite_ram, v.flip, v.sprite_bank, v.palette_bank, v.gfx_bank,
         &self.dma, self.nmi_mask, &self.sound)
    }

    pub fn load(&mut self, s: DkBusState) -> Result<(), String> {
        let (ram, video_ram, sprite_ram, flip, sprite_bank, palette_bank, gfx_bank, dma, nmi_mask, mut sound) = s;
        let v = &mut self.video;
        if ram.len() != self.ram.len() || video_ram.len() != v.video_ram.len() || sprite_ram.len() != v.sprite_ram.len() {
            return Err("save state doesn't fit this board".into());
        }
        self.ram = ram;
        v.video_ram = video_ram;
        v.sprite_ram = sprite_ram;
        v.flip = flip;
        v.sprite_bank = sprite_bank;
        v.palette_bank = palette_bank;
        v.gfx_bank = gfx_bank;
        self.dma = dma;
        self.nmi_mask = nmi_mask;
        sound.keep_rom(&mut self.sound);
        self.sound = sound;
        Ok(())
    }

    pub fn new(rom: Vec<u8>, video: DKVideo, sound_rom: Vec<u8>, variant: Variant) -> Self {
        Self {
            rom,
            ram: vec![0; 0x0C00],
            video,
            dma: I8257::default(),
            in0: 0, in1: 0, in2: 0,
            dsw0: 0x80,
            nmi_mask: false,
            sound: DkSoundIo::new(sound_rom, variant == Variant::DonkeyKongJr),
            variant,
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
            // DK: 0000-3FFF. DK Jr: 0000-5FFF (and 8000-9FFF on a bootleg)
            0x0000..=0x5FFF | 0x8000..=0x9FFF => self.rom.get(addr as usize).copied().unwrap_or(0),
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
            0x7C80..=0x7C87 => {
                // LS259 at 4H (DK Jr: bit 1 feeds the sound CPU)
                let bit = 1 << (addr & 7);
                if val & 1 != 0 { self.sound.latch_4h |= bit } else { self.sound.latch_4h &= !bit }
                // DK Jr: 0x7C80 bit 0 picks the second half of the tile ROMs (tiles 256-511)
                if addr == 0x7C80 && self.variant == Variant::DonkeyKongJr { self.video.gfx_bank = val & 1; }
            }
            0x7D00..=0x7D07 => {
                let bit = 1 << (addr & 7);
                if val & 1 != 0 { self.sound.sound_bits |= bit } else { self.sound.sound_bits &= !bit }
            }
            0x7D80..=0x7D87 => {
                // LS259 at 5H (DK Jr: 0x7D81 = falling sound)
                let bit = 1 << (addr & 7);
                if val & 1 != 0 { self.sound.latch_5h |= bit } else { self.sound.latch_5h &= !bit }
                match addr {
                    0x7D80 => self.sound.irq = val != 0,
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
            _ => {}
        }
    }

    fn port_in(&mut self, _port: u16) -> u8 { 0 }
    fn port_out(&mut self, _port: u16, _val: u8) { }
}
