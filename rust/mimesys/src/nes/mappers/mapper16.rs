use crate::nes::mappers::{Mapper, Mirroring, mirror_nt, encode_state, decode_state};
use serde::{Serialize, Deserialize};

// iNES Mapper 16: Bandai FCG-1/2 and LZ93D50 ASICs
// Features 16K switchable PRG bank, eight 1K CHR banks, serial EEPROM stubs, and a 16-bit cycle IRQ timer.

pub struct Mapper16 {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_ram: Vec<u8>,
    s: State,
}

#[derive(Serialize, Deserialize, Clone)]
struct State {
    prg: u8,               // 16K PRG bank at $8000-$BFFF
    chr: [u8; 8],          // Eight 1K CHR banks ($0000-$1FFF)
    irq_latch: u16,        // IRQ latch (LZ93D50)
    irq_counter: u16,      // 16-bit down-counter
    irq_enabled: bool,
    irq_pending: bool,
    mirroring: u8,
    #[serde(with = "serde_bytes")]
    eeprom_ram: [u8; 256], // 256-byte serial EEPROM (24C02)
    sram_dirty: bool,
}

impl Mapper16 {
    pub fn new(_mapper: u16, _submapper: u8, prg_rom: Vec<u8>, chr_rom: Vec<u8>, mirroring: Mirroring) -> Self {
        let chr_ram = if chr_rom.is_empty() { vec![0; 8192] } else { vec![] };
        Self {
            prg_rom, chr_rom, chr_ram,
            s: State {
                prg: 0,
                chr: [0, 1, 2, 3, 4, 5, 6, 7],
                irq_latch: 0,
                irq_counter: 0,
                irq_enabled: false,
                irq_pending: false,
                mirroring: match mirroring {
                    Mirroring::Horizontal => 1,
                    Mirroring::Vertical => 0,
                    Mirroring::SingleLower => 2,
                    Mirroring::SingleUpper | Mirroring::FourScreen => 3,
                },
                eeprom_ram: [0; 256],
                sram_dirty: false,
            },
        }
    }

    fn prg_banks_count(&self) -> usize { (self.prg_rom.len() / 0x4000).max(2) }
}

impl Mapper for Mapper16 {
    fn cpu_read(&self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => {
                // Serial EEPROM data read (Submapper 5 / LZ93D50)
                // Bit 4 returns SDA line status; return low/zero as basic stub
                0
            }
            0x8000..=0xBFFF => {
                let n = self.prg_banks_count();
                let bank = (self.s.prg as usize) % n;
                self.prg_rom[bank * 0x4000 + (addr as usize & 0x3FFF)]
            }
            0xC000..=0xFFFF => {
                let n = self.prg_banks_count();
                let bank = n - 1; // Fixed to the last 16K bank
                self.prg_rom[bank * 0x4000 + (addr as usize & 0x3FFF)]
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, value: u8) {
        let reg = addr & 0x000F;
        let is_lz93 = addr >= 0x8000;
        let is_fcg = (0x6000..=0x7FFF).contains(&addr);

        if is_fcg || is_lz93 {
            match reg {
                0..=7 => {
                    self.s.chr[reg as usize] = value;
                }
                8 => {
                    self.s.prg = value & 0x0F;
                }
                9 => {
                    self.s.mirroring = value & 0x03;
                }
                10 => { // IRQ Control ($x00A)
                    self.s.irq_enabled = (value & 0x01) != 0;
                    self.s.irq_pending = false;
                    if is_lz93 {
                        self.s.irq_counter = self.s.irq_latch; // Copies latch to counter on LZ93D50
                    }
                    if self.s.irq_enabled && self.s.irq_counter == 0 {
                        self.s.irq_pending = true;
                    }
                }
                11 => { // IRQ Latch/Counter Low ($x00B)
                    if is_lz93 {
                        self.s.irq_latch = (self.s.irq_latch & 0xFF00) | (value as u16);
                    } else {
                        self.s.irq_counter = (self.s.irq_counter & 0xFF00) | (value as u16);
                    }
                }
                12 => { // IRQ Latch/Counter High ($x00C)
                    if is_lz93 {
                        self.s.irq_latch = (self.s.irq_latch & 0x00FF) | ((value as u16) << 8);
                    } else {
                        self.s.irq_counter = (self.s.irq_counter & 0x00FF) | ((value as u16) << 8);
                    }
                }
                13 => {
                    // EEPROM control ($800D, LZ93D50 only)
                }
                _ => {}
            }
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
            0 => Mirroring::Vertical,
            1 => Mirroring::Horizontal,
            2 => Mirroring::SingleLower,
            _ => Mirroring::SingleUpper,
        };
        mirror_nt(m, addr)
    }

    fn get_sram(&self) -> Option<&[u8]> { Some(&self.s.eeprom_ram) }
    fn load_sram(&mut self, data: &[u8]) {
        let n = data.len().min(self.s.eeprom_ram.len());
        self.s.eeprom_ram[..n].copy_from_slice(&data[..n]);
    }
    fn is_sram_dirty(&self) -> bool { self.s.sram_dirty }
    fn clear_sram_dirty(&mut self) { self.s.sram_dirty = false; }

    fn save_state(&self) -> Vec<u8> { encode_state(&self.s) }
    fn load_state(&mut self, data: &[u8]) { if let Some(s) = decode_state(data) { self.s = s; } }
}