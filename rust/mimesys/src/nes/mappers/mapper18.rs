use crate::nes::mappers::{Mapper, Mirroring, mirror_nt, encode_state, decode_state};
use serde::{Serialize, Deserialize};

// iNES Mapper 18: Jaleco SS 88006
// Features nibble-split PRG/CHR banking registers and a down-counting 16-bit cycle IRQ timer.

pub struct Mapper18 {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_ram: Vec<u8>,
    s: State,
}

#[derive(Serialize, Deserialize, Clone)]
struct State {
    prg: [u8; 3],          // Three 8K switchable PRG banks ($8000, $A000, $C000)
    chr: [u8; 8],          // Eight 1K CHR banks ($0000-$1FFF)
    irq_latch: [u16; 4],   // Four nibbles for the 16-bit reload value ($E000-$E003)
    irq_counter: u16,      // 16-bit down-counter
    irq_enabled: bool,
    irq_pending: bool,
    irq_control: u8,       // Counter size / config flags ($F001)
    mirroring: u8,
    prg_ram_protect: u8,   // $9002 control
    prg_ram: Vec<u8>,
    sram_dirty: bool,
}

impl Mapper18 {
    pub fn new(_mapper: u16, _submapper: u8, prg_rom: Vec<u8>, chr_rom: Vec<u8>, mirroring: Mirroring) -> Self {
        let chr_ram = if chr_rom.is_empty() { vec![0; 8192] } else { vec![] };
        Self {
            prg_rom, chr_rom, chr_ram,
            s: State {
                prg: [0, 1, 2],
                chr: [0, 1, 2, 3, 4, 5, 6, 7],
                irq_latch: [0; 4],
                irq_counter: 0,
                irq_enabled: false,
                irq_pending: false,
                irq_control: 0,
                mirroring: match mirroring {
                    Mirroring::Horizontal => 0,
                    Mirroring::Vertical => 1,
                    Mirroring::SingleLower => 2,
                    Mirroring::SingleUpper => 3,
                    Mirroring::FourScreen => 3,
                },
                prg_ram_protect: 0,
                prg_ram: vec![0; 8192],
                sram_dirty: false,
            },
        }
    }

    fn prg_banks_count(&self) -> usize { (self.prg_rom.len() / 0x2000).max(4) }
}

impl Mapper for Mapper18 {
    fn cpu_read(&self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => {
                if (self.s.prg_ram_protect & 0x02) != 0 {
                    self.s.prg_ram[(addr - 0x6000) as usize]
                } else {
                    0
                }
            }
            0x8000..=0xFFFF => {
                let n = self.prg_banks_count();
                let slot = (addr as usize - 0x8000) / 0x2000;
                let bank = match slot {
                    0 => self.s.prg[0] as usize,
                    1 => self.s.prg[1] as usize,
                    2 => self.s.prg[2] as usize,
                    _ => n - 1, // Fixed last 8K bank at $E000
                } % n;
                self.prg_rom[bank * 0x2000 + (addr as usize & 0x1FFF)]
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, value: u8) {
        let val4 = value & 0x0F;
        match addr {
            0x6000..=0x7FFF => {
                if (self.s.prg_ram_protect & 0x01) != 0 { // Write protection check (1 = allow writes)
                    self.s.prg_ram[(addr - 0x6000) as usize] = value;
                    self.s.sram_dirty = true;
                }
            }
            // PRG Bank Selectors ($8000-$9FFD) - Split nibbles across sequential addresses
            0x8000..=0x8001 => {
                let idx = (addr - 0x8000) as usize; // 0 for low, 1 for high
                if idx == 0 {
                    self.s.prg[0] = (self.s.prg[0] & 0xF0) | val4;
                } else {
                    self.s.prg[0] = (self.s.prg[0] & 0x0F) | (val4 << 4);
                }
            }
            0x8002..=0x8003 => {
                if (addr - 0x8002) == 0 {
                    self.s.prg[1] = (self.s.prg[1] & 0xF0) | val4;
                } else {
                    self.s.prg[1] = (self.s.prg[1] & 0x0F) | (val4 << 4);
                }
            }
            0x9000..=0x9001 => {
                if (addr - 0x9000) == 0 {
                    self.s.prg[2] = (self.s.prg[2] & 0xF0) | val4;
                } else {
                    self.s.prg[2] = (self.s.prg[2] & 0x0F) | (val4 << 4);
                }
            }
            0x9002 => {
                self.s.prg_ram_protect = value & 0x03;
            }
            // CHR Bank Selectors ($A000-$DFFF)
            0xA000..=0xA003 => {
                let bank_reg = 0;
                let nibble = (addr - 0xA000) as usize;
                let shift = ((nibble & 1) * 4) as u16;
                let slot_idx = (nibble / 2) as u16;
                self.s.chr[slot_idx as usize] = (self.s.chr[slot_idx as usize] & !(0x0F << shift)) | ((val4 as u16) << shift) as u8;
            }
            0xB000..=0xB003 => {
                let nibble = (addr - 0xB000) as usize;
                let shift = ((nibble & 1) * 4) as u16;
                let slot_idx = 2 + (nibble / 2) as  usize;
                self.s.chr[slot_idx] = (self.s.chr[slot_idx] & !(0x0F << shift)) | ((val4 as u16) << shift) as u8;
            }
            0xC000..=0xC003 => {
                let nibble = (addr - 0xC000) as usize;
                let shift = ((nibble & 1) * 4) as u16;
                let slot_idx = (4 + (nibble / 2)) as usize;
                self.s.chr[slot_idx] = (self.s.chr[slot_idx] & !(0x0F << shift)) | ((val4 as u16) << shift) as u8;
            }
            0xD000..=0xD003 => {
                let nibble = (addr - 0xD000) as usize;
                let shift = ((nibble & 1) * 4) as u16;
                let slot_idx = (6 + (nibble / 2)) as usize;
                self.s.chr[slot_idx] = (self.s.chr[slot_idx] & !(0x0F << shift)) | ((val4 as u16) << shift) as u8;
            }
            // IRQ Reload Value ($E000-$E003)
            0xE000..=0xE003 => {
                let idx = (addr - 0xE000) as usize;
                self.s.irq_latch[idx] = val4 as u16;
            }
            // IRQ Reload / Acknowledge ($F000)
            0xF000 => {
                let reload = self.s.irq_latch[0] 
                    | (self.s.irq_latch[1] << 4) 
                    | (self.s.irq_latch[2] << 8) 
                    | (self.s.irq_latch[3] << 12);
                self.s.irq_counter = reload;
                self.s.irq_pending = false;
            }
            // IRQ Control / Counter Size ($F001)
            0xF001 => {
                self.s.irq_control = value & 0x07;
                self.s.irq_enabled = (value & 0x01) != 0 || (value & 0x02) != 0 || (value & 0x04) != 0;
                self.s.irq_pending = false;
            }
            // Mirroring Control ($F002)
            0xF002 => {
                self.s.mirroring = value & 0x03;
            }
            _ => {}
        }
    }

    fn step_cycles(&mut self, cycles: u64) {
        if !self.s.irq_enabled { return; }
        for _ in 0..cycles {
            if self.s.irq_counter == 0 {
                self.s.irq_pending = true;
            } else {
                self.s.irq_counter -= 1;
            }
        }
    }

    fn is_irq_asserted(&self) -> bool { self.s.irq_pending }

    fn ppu_read(&self, addr: u16) -> u8 {
        let addr = addr as usize & 0x1FFF;
        let bank_idx = addr / 0x400;
        let bank_val = self.s.chr[bank_idx] as usize;
        let offset = addr & 0x3FF;

        if self.chr_rom.is_empty() {
            self.chr_ram[(bank_val * 0x400 + offset) % self.chr_ram.len()]
        } else {
            self.chr_rom[(bank_val * 0x400 + offset) % self.chr_rom.len()]
        }
    }

    fn ppu_write(&mut self, addr: u16, value: u8) {
        let addr = addr as usize & 0x1FFF;
        let bank_idx = addr / 0x400;
        let bank_val = self.s.chr[bank_idx] as usize;
        let offset = addr & 0x3FF;

        if self.chr_rom.is_empty() {
            let target = (bank_val * 0x400 + offset) % self.chr_ram.len();
            self.chr_ram[target] = value;
        }
    }

    fn mirror_vram_address(&self, addr: u16) -> usize {
        let m = match self.s.mirroring & 3 {
            0 => Mirroring::Horizontal,
            1 => Mirroring::Vertical,
            2 => Mirroring::SingleLower,
            _ => Mirroring::SingleUpper,
        };
        mirror_nt(m, addr)
    }

    fn get_sram(&self) -> Option<&[u8]> { Some(&self.s.prg_ram) }
    fn load_sram(&mut self, data: &[u8]) {
        let n = data.len().min(self.s.prg_ram.len());
        self.s.prg_ram[..n].copy_from_slice(&data[..n]);
    }
    fn is_sram_dirty(&self) -> bool { self.s.sram_dirty }
    fn clear_sram_dirty(&mut self) { self.s.sram_dirty = false; }

    fn save_state(&self) -> Vec<u8> { encode_state(&self.s) }
    fn load_state(&mut self, data: &[u8]) { if let Some(s) = decode_state(data) { self.s = s; } }
}