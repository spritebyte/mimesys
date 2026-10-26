use crate::common::cpu::z80::Bus;
use crate::arcade::pacman::pacman_video::PacmanVideo;
use crate::arcade::pacman::namco_wsg::NamcoWsg;
use crate::arcade::pacman::mspacman;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Board {
    Pacman,
    Pengo,
}


pub struct PacmanBus {
    board: Board,
    banks: Vec<Vec<u8>>,
    bank: usize,
    opcodes: Option<Vec<u8>>,
    ms_traps: bool,

    ram: Vec<u8>,
    pub video: PacmanVideo,
    pub wsg: NamcoWsg,

    pub in0: u8,
    pub in1: u8,
    pub dsw1: u8,
    pub dsw2: u8,

    pub irq_enable: bool,
    pub irq_line: bool,
    pub irq_vector: u8,
}

/// Save state: RAM, latches and chips (no ROMs or graphics).
#[derive(serde::Serialize, serde::Deserialize)]
pub struct PacmanBusState {
    bank: usize,
    ram: Vec<u8>,
    video: (Vec<u8>, Vec<u8>, [u8; 16], [u8; 16], bool, [u8; 4]),
    wsg: NamcoWsg,
    irq_enable: bool,
    irq_line: bool,
    irq_vector: u8,
}

impl PacmanBus {
    pub fn save(&self) -> PacmanBusState {
        let v = &self.video;
        PacmanBusState {
            bank: self.bank,
            ram: self.ram.clone(),
            video: (v.video_ram.clone(), v.color_ram.clone(), v.sprite_ram, v.sprite_pos, v.flip,
                    [v.char_bank, v.sprite_bank, v.palette_bank, v.colortable_bank]),
            wsg: self.wsg.save_copy(),
            irq_enable: self.irq_enable, irq_line: self.irq_line, irq_vector: self.irq_vector,
        }
    }

    pub fn load(&mut self, mut s: PacmanBusState) -> Result<(), String> {
        if s.bank >= self.banks.len() || s.ram.len() != self.ram.len()
            || s.video.0.len() != self.video.video_ram.len() || s.video.1.len() != self.video.color_ram.len() {
            return Err("save state doesn't fit this board".into());
        }
        self.bank = s.bank;
        self.ram = s.ram;
        let v = &mut self.video;
        (v.video_ram, v.color_ram, v.sprite_ram, v.sprite_pos, v.flip) = (s.video.0, s.video.1, s.video.2, s.video.3, s.video.4);
        [v.char_bank, v.sprite_bank, v.palette_bank, v.colortable_bank] = s.video.5;
        s.wsg.keep_static(&mut self.wsg);
        self.wsg = s.wsg;
        self.irq_enable = s.irq_enable;
        self.irq_line = s.irq_line;
        self.irq_vector = s.irq_vector;
        Ok(())
    }

    pub fn new(board: Board, banks: Vec<Vec<u8>>, opcodes: Option<Vec<u8>>, ms_traps: bool, video: PacmanVideo, wsg: NamcoWsg) -> Self {
        let bank = banks.len() - 1;
        let (dsw1, dsw2) = match board {
            Board::Pacman => (0xC9, 0xFF),
            Board::Pengo => (0xB0, 0xCC),
        };
        Self {
            board,
            banks,
            bank,
            opcodes,
            ms_traps,
            ram: vec![0; 0x800],
            video,
            wsg,
            in0: 0xFF, in1: 0xFF,
            dsw1,
            dsw2,
            irq_enable: false,
            irq_line: false,
            irq_vector: 0xFF,
        }
    }

    fn check_trap(&mut self, addr: u16) {
        if self.ms_traps {
            if let Some(b) = mspacman::trap(addr) { self.bank = b; }
        }
    }

    pub fn is_ms_decode_on(&self) -> bool { self.bank == 1 }

    fn latch_write(&mut self, bit: u16, val: u8) {
        let on = val & 1 != 0;
        match bit {
            0 => {
                self.irq_enable = on;
                if !on { self.irq_line = false; }
            }
            1 => self.wsg.enabled = on,
            2 if self.board == Board::Pengo => self.video.palette_bank = on as u8,
            3 => self.video.flip = on,
            6 if self.board == Board::Pengo => self.video.colortable_bank = on as u8,
            7 if self.board == Board::Pengo => {
                self.video.char_bank = on as u8;
                self.video.sprite_bank = on as u8;
            }
            _ => {}
        }
    }

    fn pacman_read(&mut self, addr: u16) -> u8 {
        if addr & 0x4000 == 0 {
            self.check_trap(addr);
            return self.banks[self.bank][addr as usize];
        }
        let a = addr & 0x5FFF;
        match a {
            0x4000..=0x43FF => self.video.video_ram[(a & 0x3FF) as usize],
            0x4400..=0x47FF => self.video.color_ram[(a & 0x3FF) as usize],
            0x4800..=0x4BFF => 0xBF, // open bus, Ms. Pacman reads this
            0x4C00..=0x4FEF => self.ram[(a - 0x4C00) as usize],
            0x4FF0..=0x4FFF => self.video.sprite_ram[(a & 0x0F) as usize],
            0x5000..=0x5FFF => match a & 0xC0 {
                0x00 => self.in0,
                0x40 => self.in1,
                0x80 => self.dsw1,
                _ => self.dsw2,
            },
            _ => 0xFF,
        }
    }

    fn pacman_write(&mut self, addr: u16, val: u8) {
        if addr & 0x4000 == 0 {
            self.check_trap(addr);
            return;
        }
        let a = addr & 0x5FFF;
        match a {
            0x4000..=0x43FF => self.video.video_ram[(a & 0x3FF) as usize] = val,
            0x4400..=0x47FF => self.video.color_ram[(a & 0x3FF) as usize] = val,
            0x4C00..=0x4FEF => self.ram[(a - 0x4C00) as usize] = val,
            0x4FF0..=0x4FFF => self.video.sprite_ram[(a & 0x0F) as usize] = val,
            0x5000..=0x5FFF => match a & 0xFF {
                0x00..=0x3F => {
                    let on = val & 1 != 0;
                    match a & 7 {
                        0 => {
                            self.irq_enable = on;
                            if !on { self.irq_line = false; }
                        }
                        1 => self.wsg.enabled = on,
                        3 => self.video.flip = on,
                        _ => {}
                    }
                }
                0x40..=0x5F => self.wsg.write((a & 0x1F) as u8, val),
                0x60..=0x6F => self.video.sprite_pos[(a & 0x0F) as usize] = val,
                _ => {}
            },
            _ => {}
        }
    }

    fn pengo_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x7FFF => self.banks[0][addr as usize],
            0x8000..=0x83FF => self.video.video_ram[(addr & 0x3FF) as usize],
            0x8400..=0x87FF => self.video.color_ram[(addr & 0x3FF) as usize],
            0x8800..=0x8FEF => self.ram[(addr - 0x8800) as usize],
            0x8FF0..=0x8FFF => self.video.sprite_ram[(addr & 0x0F) as usize],
            0x9000..=0x903F => self.dsw2,
            0x9040..=0x907F => self.dsw1,
            0x9080..=0x90BF => self.in1,
            0x90C0..=0x90FF => self.in0,
            _ => 0xFF,
        }
    }

    fn pengo_write(&mut self, addr: u16, val: u8) {
        match addr {
            0x8000..=0x83FF => self.video.video_ram[(addr & 0x3FF) as usize] = val,
            0x8400..=0x87FF => self.video.color_ram[(addr & 0x3FF) as usize] = val,
            0x8800..=0x8FEF => self.ram[(addr - 0x8800) as usize] = val,
            0x8FF0..=0x8FFF => self.video.sprite_ram[(addr & 0x0F) as usize] = val,
            0x9000..=0x901F => self.wsg.write((addr & 0x1F) as u8, val),
            0x9020..=0x902F => self.video.sprite_pos[(addr & 0x0F) as usize] = val,
            0x9040..=0x9047 => self.latch_write(addr & 7, val),
            _ => {}
        }
    }
}

impl Bus for PacmanBus {
    fn read(&mut self, addr: u16) -> u8 {
        match self.board {
            Board::Pacman => self.pacman_read(addr),
            Board::Pengo => self.pengo_read(addr),
        }
    }

    fn write(&mut self, addr: u16, val: u8) {
        match self.board {
            Board::Pacman => self.pacman_write(addr, val),
            Board::Pengo => self.pengo_write(addr, val),
        }
    }

    fn read_opcode(&mut self, addr: u16) -> u8 {
        match &self.opcodes {
            Some(ops) if addr < 0x8000 => ops[addr as usize],
            _ => self.read(addr),
        }
    }

    fn port_in(&mut self, port: u16) -> u8 { 0xFF }
    fn port_out(&mut self, port: u16, val: u8) {
        if self.board == Board::Pacman && port & 0xFF == 0 { self.irq_vector = val; }
    }
}