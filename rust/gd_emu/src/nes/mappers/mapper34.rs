use crate::nes::mappers::{Mapper, Mirroring};

// Mapper 34: covers two unrelated boards distinguished by submapper/CHR size.
//   - BNROM  (submapper 2, or fallback when CHR is RAM): single 32KB-bank
//     register at ANY address $8000-$FFFF. No CHR banking (CHR-RAM only).
//     Mirroring is whatever the cartridge header says (fixed by solder pad).
//   - NINA-001 (submapper 1, or fallback when CHR-ROM > 8KB): PRG bank
//     register at fixed $7FFD, two 4KB CHR bank registers at $7FFE/$7FFF.
//     These overlap PRG-RAM, so writes also land in PRG-RAM underneath.
//     Mirroring is hardwired vertical, ignoring the header.
pub struct Mapper34 {
    prg_banks: usize,
    prg_bank_count: usize, // number of 32KB PRG banks (BNROM uses this)
    prg_bank: u8,
    chr_banks: usize,
    chr_rom_size: usize,

    is_nina001: bool,
    chr_bank_0: u8, // NINA-001 only: 4KB bank for PPU $0000-$0FFF
    chr_bank_1: u8, // NINA-001 only: 4KB bank for PPU $1000-$1FFF

    mirroring_mode: Mirroring,
    has_four_screen: bool,

    prg_rom: Vec<u8>,
    prg_ram: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_ram: Vec<u8>,

    current_cycle: i64,
    sram_dirty: bool,
}

impl Mapper34 {
    pub fn new(prg_banks: usize, chr_banks: usize, prg_rom: Vec<u8>, chr_rom: Vec<u8>, initial_mirroring: Mirroring, four_screen_bit: bool, submapper: u8) -> Self {
        let prg_ram = vec![0; 8192];
        let chr_rom_size = chr_rom.len();
        // NINA-001 always has real CHR-ROM (>8KB unambiguously means NINA-001
        // per NESdev); BNROM games use CHR-RAM. Submapper overrides the guess
        // when present.
        let is_nina001 = match submapper {
            1 => true,
            2 => false,
            _ => chr_rom_size > 8192,
        };
        let chr_ram = if is_nina001 { vec![] } else { vec![0; 8192] };
        let prg_bank_count = prg_rom.len() / 0x8000; // 32KB banks, BNROM-style

        let mirroring_mode = if is_nina001 {
            // NINA-001 is hardwired vertical regardless of header.
            Mirroring::Vertical
        } else {
            // BNROM has no banking control over mirroring; honor the header.
            initial_mirroring
        };

        Self {
            prg_banks,
            prg_bank_count,
            prg_bank: 0,
            chr_banks,
            chr_rom_size,
            is_nina001,
            chr_bank_0: 0,
            chr_bank_1: 0,
            mirroring_mode,
            has_four_screen: four_screen_bit,
            prg_rom,
            prg_ram,
            chr_rom,
            chr_ram,
            current_cycle: 0,
            sram_dirty: false,
        }
    }

    fn _chr_read(&self, addr: usize) -> u8 {
        if self.chr_rom_size > 0 {
            self.chr_rom[addr % self.chr_rom_size]
        } else {
            self.chr_ram[addr % self.chr_ram.len()]
        }
    }
}

impl Mapper for Mapper34 {
    fn step_cycles(&mut self, cycles: u64) {
        self.current_cycle += cycles as i64;
    }

    fn cpu_read(&self, addr: u16) -> u8 {
        if self.is_nina001 && addr >= 0x6000 && addr < 0x8000 {
            // $7FFD-$7FFF overlap PRG-RAM: reading the register address
            // returns the last value written there (to RAM), same as the
            // register's last-written value, per NINA-001 hardware behavior.
            return self.prg_ram[(addr - 0x6000) as usize];
        }

        if addr < 0x8000 { return 0; }

        if self.is_nina001 {
            // 32KB window into up to 128KB (NINA-002) of PRG-ROM.
            let bank = self.prg_bank as usize;
            let offset = (addr & 0x7FFF) as usize;
            let idx = (bank * 0x8000) + offset;
            return self.prg_rom[idx % self.prg_rom.len()];
        }

        // BNROM: single 32KB bank, registered on any $8000-$FFFF write.
        let bank = self.prg_bank as usize % self.prg_bank_count.max(1);
        let offset = (addr & 0x7FFF) as usize;
        self.prg_rom[(bank * 0x8000) + offset]
    }

    fn cpu_write(&mut self, addr: u16, value: u8) {
        if self.is_nina001 {
            if addr >= 0x6000 && addr < 0x8000 {
                // All writes in this range hit PRG-RAM underneath the
                // registers, regardless of address.
                self.prg_ram[(addr - 0x6000) as usize] = value;
                self.sram_dirty = true;
            }
            match addr {
                0x7FFD => self.prg_bank = value & 0x07, // up to 8 32KB banks (NINA-002)
                0x7FFE => self.chr_bank_0 = value & 0x0F,
                0x7FFF => self.chr_bank_1 = value & 0x0F,
                _ => {}
            }
            return;
        }

        // BNROM: bank-select register lives at every address $8000-$FFFF.
        if addr < 0x8000 { return; }
        if self.prg_bank_count > 0 {
            self.prg_bank = value % self.prg_bank_count as u8;
        }
    }

    fn ppu_read(&self, p_addr: u16) -> u8 {
        let addr = p_addr & 0x3FFF;

        if addr < 0x2000 {
            if self.is_nina001 {
                // Two independently-banked 4KB windows.
                let bank = if addr < 0x1000 { self.chr_bank_0 } else { self.chr_bank_1 } as usize;
                let local = (addr & 0x0FFF) as usize;
                let idx = (bank * 0x1000) + local;
                return self.chr_rom[idx % self.chr_rom.len()];
            }
            return self._chr_read(addr as usize);
        }
        0
    }

    fn ppu_write(&mut self, p_addr: u16, value: u8) {
        let addr = p_addr & 0x3FFF;

        // NINA-001 always has CHR-ROM (never CHR-RAM), so PPU writes are
        // only meaningful for BNROM's CHR-RAM case.
        if addr < 0x2000 && !self.is_nina001 && self.chr_banks == 0 {
            self.chr_ram[(addr & 0x1FFF) as usize] = value;
        }
    }

    fn mirror_vram_address(&self, addr: u16) -> usize {
        let relative_addr = (addr & 0x0FFF) as usize;
        if self.has_four_screen {
            return relative_addr;
        }

        match self.mirroring_mode {
            Mirroring::Horizontal => {
                if relative_addr < 0x800 {
                    relative_addr % 0x400
                } else {
                    0x400 + (relative_addr % 0x400)
                }
            }
            Mirroring::Vertical => relative_addr % 0x800,
            Mirroring::SingleLower => relative_addr & 0x03FF,
            Mirroring::SingleUpper => 0x400 | (relative_addr & 0x03FF),
            _ => relative_addr,
        }
    }
}