use crate::common::cpu::z80::Bus;
use crate::arcade::galaxian::galaxian_video::GalaxianVideo;
use crate::arcade::galaxian::galaxian_sound::GalaxianSound;
use crate::arcade::galaxian::konami_sound::KonamiSound;
use crate::common::io::ppi8255::{Ppi8255, PpiWrite};

// Galaxian memory map (MAME galaxian_map). Unmapped reads return 0xFF.
//   0000-3FFF  ROM
//   4000-43FF  RAM (mirrored at 4400)
//   5000-53FF  tile RAM (mirrored at 5400)
//   5800-58FF  object RAM: column scroll/color, sprites, bullets (mirrored to 5FFF)
//   6000       R: IN0     W 6000-6001 lamps, 6002 coin lockout, 6003 coin counter,
//                         W 6004-6007 sound LFO bits
//   6800       R: IN1     W 6800-6807 sound latch (FS1-3, HIT, FIRE, VOL1-2)
//   7000       R: IN2     W 7001 NMI enable, 7004 stars, 7006 flip X, 7007 flip Y
//   7800       R: watchdog  W: sound pitch
//
// Moon Cresta (MAME mooncrst_map) is the same board with different decoding:
//   8000 RAM, 9000 tiles, 9800 objects, A000 IN0 / gfx bank latches A000-A002 /
//   coin counter A003 / LFO A004-A007, A800 IN1 / sound latch, B000 IN2 / NMI enable,
//   B004 stars, B006-B007 flip, B800 watchdog / pitch.
//
// Frogger (MAME frogger_map), Konami's version:
//   0000-3FFF ROM, 8000-87FF RAM (2 KB), 8800 watchdog, A800-ABFF tiles,
//   B000-B0FF objects, B808 NMI enable, B80C flip Y, B810 flip X, B818/B81C coin counters,
//   C000-FFFF two 8255s: A13 selects #1 (inputs), A12 selects #2 (sound latch/control),
//   register = A2-A1. Both can be selected at once (reads AND together).
//
// Turtles board (MAME turtles_map): Amidar, Turtles, Turpin, 600. A14 is ignored above 8000.
//   0000-7FFF ROM, 8000-87FF RAM, 9000-93FF tiles (mirror 9400), 9800-98FF objects (mirror to 9FFF),
//   A000-A7FF writes, register = A5-A3: 0 bg red, 1 NMI enable, 2 flip Y, 3 flip X,
//                                       4 bg green, 5 bg blue, 6-7 coin counters
//   A800 watchdog, B000-B7FF 8255 #1 (inputs), B800-BFFF 8255 #2 (sound), register = A5-A4.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layout { Galaxian, MoonCresta, Frogger, Turtles }

pub enum SoundBoard {
    Discrete(GalaxianSound),   // Galaxian, Moon Cresta
    Konami(KonamiSound),       // Frogger, Amidar, Turtles: Z80 + AY-3-8910(s)
}

pub type BusState = (Vec<u8>, [Ppi8255; 2], bool, usize, crate::arcade::galaxian::galaxian_video::VideoState,
                     (Option<GalaxianSound>, Option<crate::arcade::galaxian::konami_sound::KonamiState>));

pub struct GalaxianBus {
    layout: Layout,
    rom: Vec<u8>,
    ram: Vec<u8>,
    pub video: GalaxianVideo,
    pub sound: SoundBoard,
    ppi: [Ppi8255; 2],

    // Galaxian/Moon Cresta inputs are active HIGH, Frogger's active LOW.
    pub in0: u8,
    pub in1: u8,
    pub in2: u8,
    pub in3: u8,

    pub nmi_enable: bool,
    pub current_line: usize,   // for the star field reset
}

impl GalaxianBus {
    pub fn new(layout: Layout, rom: Vec<u8>, video: GalaxianVideo, sound: SoundBoard, dips: (u8, u8)) -> Self {
        Self {
            layout,
            rom,
            ram: vec![0; 0x800],
            video,
            sound,
            ppi: [Ppi8255::default(), Ppi8255::default()],
            in0: if matches!(layout, Layout::Frogger | Layout::Turtles) { 0xFF } else { 0 },
            in1: dips.0,
            in2: dips.1,
            in3: 0,
            nmi_enable: false,
            current_line: 0,
        }
    }

    pub fn layout(&self) -> Layout { self.layout }

    pub fn save(&self) -> impl serde::Serialize + '_ {
        let sound: (Option<&GalaxianSound>, Option<_>) = match &self.sound {
            SoundBoard::Discrete(s) => (Some(s), None),
            SoundBoard::Konami(k) => (None, Some(k.save())),
        };
        (&self.ram, &self.ppi, self.nmi_enable, self.current_line, self.video.save(), sound)
    }

    pub fn load(&mut self, s: BusState) -> Result<(), String> {
        let (ram, ppi, nmi_enable, current_line, video, sound) = s;
        if ram.len() != self.ram.len() { return Err("RAM size mismatch".into()); }
        match (&mut self.sound, sound) {
            (SoundBoard::Discrete(cur), (Some(mut new), None)) => {
                new.out = std::mem::take(&mut cur.out);
                *cur = new;
            }
            (SoundBoard::Konami(cur), (None, Some(k))) => cur.load(k)?,
            _ => return Err("save state is for a different sound board".into()),
        }
        self.video.load(video)?;
        self.ram = ram;
        self.ppi = ppi;
        self.nmi_enable = nmi_enable;
        self.current_line = current_line;
        Ok(())
    }

    /// Translate a Moon Cresta address to the Galaxian one, so both share one decoder.
    /// (The 0x4000 blocks line up as 8000->4000, 9000->5000, A000->6000, ... B800->7800.)
    fn normalize(&self, addr: u16) -> Option<u16> {
        match self.layout {
            Layout::Galaxian => Some(addr),
            Layout::MoonCresta => match addr {
                0x0000..=0x3FFF => Some(addr),
                0x8000..=0xBFFF => Some(addr - 0x4000),
                _ => None,
            },
            Layout::Frogger | Layout::Turtles => None,   // handled separately
        }
    }

    fn discrete(&mut self) -> Option<&mut GalaxianSound> {
        match &mut self.sound { SoundBoard::Discrete(s) => Some(s), _ => None }
    }

    /// 8255 #2 write: port A = sound command, port B = sound control.
    fn sound_ppi_write(&mut self, reg: u8, val: u8) {
        let w = self.ppi[1].write(reg, val);
        if let SoundBoard::Konami(k) = &mut self.sound {
            match w {
                PpiWrite::PortA(v) => k.write_latch(v),
                PpiWrite::PortB(v) => k.write_control(v),
                _ => {}
            }
        }
    }

    // ---------- Frogger ----------

    fn frogger_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x3FFF => self.rom.get(addr as usize).copied().unwrap_or(0xFF),
            0x8000..=0x87FF => self.ram[(addr & 0x7FF) as usize],
            0xA800..=0xAFFF => self.video.video_ram[(addr & 0x3FF) as usize],
            0xB000..=0xB7FF => self.video.obj_ram[(addr & 0xFF) as usize],
            0xC000..=0xFFFF => {
                let reg = ((addr >> 1) & 3) as u8;
                let mut v = 0xFF;
                if addr & 0x1000 != 0 { v &= self.ppi[1].read(reg, [0xFF, 0xFF, self.in3]); }
                if addr & 0x2000 != 0 { v &= self.ppi[0].read(reg, [self.in0, self.in1, self.in2]); }
                v
            }
            _ => 0xFF,
        }
    }

    fn frogger_write(&mut self, addr: u16, val: u8) {
        match addr {
            0x8000..=0x87FF => self.ram[(addr & 0x7FF) as usize] = val,
            0xA800..=0xAFFF => self.video.video_ram[(addr & 0x3FF) as usize] = val,
            0xB000..=0xB7FF => self.video.obj_ram[(addr & 0xFF) as usize] = val,
            0xB800..=0xBFFF => match (addr >> 2) & 7 {
                2 => self.nmi_enable = val & 1 != 0,          // B808
                3 => self.video.flip_y = val & 1 != 0,        // B80C
                4 => self.video.flip_x = val & 1 != 0,        // B810
                _ => {}                                       // coin counters
            },
            0xC000..=0xFFFF => {
                let reg = ((addr >> 1) & 3) as u8;
                if addr & 0x1000 != 0 { self.sound_ppi_write(reg, val); }
                if addr & 0x2000 != 0 { self.ppi[0].write(reg, val); }
            }
            _ => {}
        }
    }

    // ---------- Turtles (Amidar) ----------

    fn turtles_read(&mut self, addr: u16) -> u8 {
        if addr < 0x8000 { return self.rom.get(addr as usize).copied().unwrap_or(0xFF); }
        let a = addr & !0x4000;
        let reg = ((a >> 4) & 3) as u8;
        match a {
            0x8000..=0x87FF => self.ram[(a & 0x7FF) as usize],
            0x9000..=0x97FF => self.video.video_ram[(a & 0x3FF) as usize],
            0x9800..=0x9FFF => self.video.obj_ram[(a & 0xFF) as usize],
            0xB000..=0xB7FF => self.ppi[0].read(reg, [self.in0, self.in1, self.in2]),
            0xB800..=0xBFFF => self.ppi[1].read(reg, [0xFF, 0xFF, self.in3]),
            _ => 0xFF,   // A800 watchdog
        }
    }

    fn turtles_write(&mut self, addr: u16, val: u8) {
        if addr < 0x8000 { return; }
        let a = addr & !0x4000;
        let reg = ((a >> 4) & 3) as u8;
        let on = val & 1 != 0;
        match a {
            0x8000..=0x87FF => self.ram[(a & 0x7FF) as usize] = val,
            0x9000..=0x97FF => self.video.video_ram[(a & 0x3FF) as usize] = val,
            0x9800..=0x9FFF => self.video.obj_ram[(a & 0xFF) as usize] = val,
            0xA000..=0xA7FF => match (a >> 3) & 7 {
                0 => self.video.background[0] = on,
                1 => self.nmi_enable = on,
                2 => self.video.flip_y = on,
                3 => self.video.flip_x = on,
                4 => self.video.background[1] = on,
                5 => self.video.background[2] = on,
                _ => {}   // coin counters
            },
            0xB000..=0xB7FF => { self.ppi[0].write(reg, val); }
            0xB800..=0xBFFF => self.sound_ppi_write(reg, val),
            _ => {}
        }
    }
}

impl Bus for GalaxianBus {
    fn read(&mut self, addr: u16) -> u8 {
        if self.layout == Layout::Frogger { return self.frogger_read(addr); }
        if self.layout == Layout::Turtles { return self.turtles_read(addr); }
        let Some(a) = self.normalize(addr) else { return 0xFF };
        match a {
            0x0000..=0x3FFF => self.rom.get(a as usize).copied().unwrap_or(0xFF),
            0x4000..=0x47FF => self.ram[(a & 0x3FF) as usize],
            0x5000..=0x57FF => self.video.video_ram[(a & 0x3FF) as usize],
            0x5800..=0x5FFF => self.video.obj_ram[(a & 0xFF) as usize],
            0x6000..=0x67FF => self.in0,
            0x6800..=0x6FFF => self.in1,
            0x7000..=0x77FF => self.in2,
            _ => 0xFF,   // 7800: watchdog reset, reads as open bus
        }
    }

    fn write(&mut self, addr: u16, val: u8) {
        if self.layout == Layout::Frogger { return self.frogger_write(addr, val); }
        if self.layout == Layout::Turtles { return self.turtles_write(addr, val); }
        let Some(a) = self.normalize(addr) else { return };
        match a {
            0x4000..=0x47FF => self.ram[(a & 0x3FF) as usize] = val,
            0x5000..=0x57FF => self.video.video_ram[(a & 0x3FF) as usize] = val,
            0x5800..=0x5FFF => self.video.obj_ram[(a & 0xFF) as usize] = val,
            0x6000..=0x67FF => match a & 7 {
                0..=2 if self.layout == Layout::MoonCresta => self.video.gfx_bank[(a & 7) as usize] = val & 1,
                4..=7 => if let Some(s) = self.discrete() { s.lfo_w((a & 3) as u8, val) },
                _ => {}   // lamps, coin lockout, coin counter
            },
            0x6800..=0x6FFF => if let Some(s) = self.discrete() { s.sound_w((a & 7) as u8, val) },
            0x7000..=0x77FF => {
                // Moon Cresta's NMI enable is at B000 (= 7000 here), Galaxian's at 7001
                let nmi_bit = if self.layout == Layout::MoonCresta { 0 } else { 1 };
                match a & 7 {
                    n if n == nmi_bit => self.nmi_enable = val & 1 != 0,
                    4 => self.video.set_stars_enabled(val & 1 != 0, self.current_line),
                    6 => self.video.flip_x = val & 1 != 0,
                    7 => self.video.flip_y = val & 1 != 0,
                    _ => {}
                }
            }
            0x7800..=0x7FFF => if let Some(s) = self.discrete() { s.pitch = val },
            _ => {}
        }
    }

    // No I/O ports on the main CPU
    fn port_in(&mut self, _port: u16) -> u8 { 0xFF }
    fn port_out(&mut self, _port: u16, _val: u8) {}
}
