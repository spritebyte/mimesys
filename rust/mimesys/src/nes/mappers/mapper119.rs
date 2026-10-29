
use crate::nes::mappers::{
    decode_state, encode_state, mirror_nt, Mapper, Mirroring,
};
use serde::{Deserialize, Serialize};

// iNES Mapper 119: TQROM (MMC3 + CHR-ROM / CHR-RAM)

const PRG_BANK_SIZE: usize = 0x2000; // 8 KiB
const CHR_BANK_SIZE: usize = 0x0400; // 1 KiB
const PRG_RAM_SIZE: usize = 0x2000;  // 8 KiB
const CHR_RAM_SIZE: usize = 0x2000;  // 8 KiB

pub struct Mapper119 {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_ram: Vec<u8>,
    s: State,

    // Preserve hardwired four-screen cartridge mirroring.
    four_screen: bool,
}

#[derive(Serialize, Deserialize, Clone)]
struct State {
    command: u8,
    chr_regs: [u8; 8],
    prg_regs: [u8; 2],
    prg_mode: bool,
    chr_mode: bool,
    mirroring: u8,

    // MMC3 IRQ state.
    irq_latch: u8,
    irq_counter: u8,
    irq_reload: bool,
    irq_enabled: bool,
    irq_pending: bool,

    // PPU A12 edge filter.
    a12_high: bool,
    last_a12_low_cycle: u64,
    a12_low_seen: bool,

    // PRG RAM control.
    prg_ram: Vec<u8>,
    prg_ram_enabled: bool,
    prg_ram_write_protected: bool,
    sram_dirty: bool,
}

impl Mapper119 {
    pub fn new(
        _mapper: u16,
        _submapper: u8,
        prg_rom: Vec<u8>,
        chr_rom: Vec<u8>,
        mirroring: Mirroring,
    ) -> Self {
        let four_screen = matches!(mirroring, Mirroring::FourScreen);

        Self {
            prg_rom,
            chr_rom,
            chr_ram: vec![0; CHR_RAM_SIZE],
            four_screen,
            s: State {
                command: 0,
                chr_regs: [0; 8],
                prg_regs: [0, 1],
                prg_mode: false,
                chr_mode: false,
                mirroring: match mirroring {
                    Mirroring::Vertical => 0,
                    Mirroring::Horizontal => 1,
                    Mirroring::SingleLower => 2,
                    Mirroring::SingleUpper => 3,
                    Mirroring::FourScreen => 4,
                },

                irq_latch: 0,
                irq_counter: 0,
                irq_reload: false,
                irq_enabled: false,
                irq_pending: false,

                a12_high: false,
                last_a12_low_cycle: 0,
                a12_low_seen: false,

                prg_ram: vec![0; PRG_RAM_SIZE],
                prg_ram_enabled: true,
                prg_ram_write_protected: false,
                sram_dirty: false,
            },
        }
    }

    fn prg_bank_count(&self) -> usize {
        self.prg_rom.len() / PRG_BANK_SIZE
    }

    fn chr_bank_for_slot(&self, slot: usize) -> usize {
        // MMC3 CHR mode 0:
        // $0000-$0FFF: R0 (2 KiB), R1 (2 KiB)
        // $1000-$1FFF: R2, R3, R4, R5 (1 KiB each)
        //
        // MMC3 CHR mode 1 swaps the two 4 KiB halves.

        match (self.s.chr_mode, slot) {
            (false, 0..=1) => {
                (self.s.chr_regs[0] as usize & 0xFE) + slot
            }
            (false, 2..=3) => {
                (self.s.chr_regs[1] as usize & 0xFE) + (slot - 2)
            }
            (false, 4..=7) => {
                self.s.chr_regs[slot - 2] as usize
            }

            (true, 0..=3) => {
                self.s.chr_regs[slot + 2] as usize
            }
            (true, 4..=5) => {
                (self.s.chr_regs[0] as usize & 0xFE) + (slot - 4)
            }
            (true, 6..=7) => {
                (self.s.chr_regs[1] as usize & 0xFE) + (slot - 6)
            }

            _ => 0,
        }
    }

    fn resolve_chr(&self, addr: usize) -> (bool, usize) {
        let addr = addr & 0x1FFF;
        let slot = addr / CHR_BANK_SIZE;
        let offset = addr & (CHR_BANK_SIZE - 1);
        let bank = self.chr_bank_for_slot(slot);

        // TQROM CHR bank registers:
        // bit 6 = 1: CHR RAM, bank selected by bits 0-2
        // bit 6 = 0: CHR ROM, bank selected by bits 0-5
        if bank & 0x40 != 0 {
            let ram_bank = bank & 0x07;
            (true, ram_bank * CHR_BANK_SIZE + offset)
        } else {
            let rom_banks = (self.chr_rom.len() / CHR_BANK_SIZE).min(64);

            if rom_banks == 0 {
                return (false, 0);
            }

            let rom_bank = (bank & 0x3F) % rom_banks;
            (false, rom_bank * CHR_BANK_SIZE + offset)
        }
    }

    fn mapped_prg_bank(&self, slot: usize) -> usize {
        let count = self.prg_bank_count();

        if count == 0 {
            return 0;
        }

        let last = count - 1;
        let second_last = count.saturating_sub(2);

        // MMC3 R6 is a 6-bit PRG bank register.
        let r6 = (self.s.prg_regs[0] as usize & 0x3F) % count;

        // MMC3 R7 is a 6-bit PRG bank register.
        let r7 = (self.s.prg_regs[1] as usize & 0x3F) % count;

        match (self.s.prg_mode, slot) {
            // PRG mode 0:
            // $8000=R6, $A000=R7, $C000=second-last, $E000=last
            (false, 0) => r6,
            (false, 1) => r7,
            (false, 2) => second_last,
            (false, 3) => last,

            // PRG mode 1:
            // $8000=second-last, $A000=R7, $C000=R6, $E000=last
            (true, 0) => second_last,
            (true, 1) => r7,
            (true, 2) => r6,
            (true, 3) => last,

            _ => last,
        }
    }

    fn clock_irq(&mut self) {
        // The counter clocks independently of IRQ enable.
        if self.s.irq_counter == 0 || self.s.irq_reload {
            self.s.irq_counter = self.s.irq_latch;
            self.s.irq_reload = false;
        } else {
            self.s.irq_counter = self.s.irq_counter.wrapping_sub(1);
        }

        if self.s.irq_counter == 0 && self.s.irq_enabled {
            self.s.irq_pending = true;
        }
    }

    fn set_mirroring(&mut self, value: u8) {
        // Four-screen wiring cannot be overridden by MMC3 $A000.
        if !self.four_screen {
            self.s.mirroring = value & 1;
        }
    }
}

impl Mapper for Mapper119 {
    fn cpu_read(&self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => {
                if self.s.prg_ram_enabled {
                    self.s
                        .prg_ram
                        .get((addr - 0x6000) as usize)
                        .copied()
                        .unwrap_or(0)
                } else {
                    0
                }
            }

            0x8000..=0xFFFF => {
                let count = self.prg_bank_count();

                if count == 0 {
                    return 0;
                }

                let slot = (addr as usize - 0x8000) / PRG_BANK_SIZE;
                let bank = self.mapped_prg_bank(slot);
                let offset = addr as usize & (PRG_BANK_SIZE - 1);
                let index = bank * PRG_BANK_SIZE + offset;

                self.prg_rom.get(index).copied().unwrap_or(0)
            }

            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, value: u8) {
        match addr {
            0x6000..=0x7FFF => {
                if self.s.prg_ram_enabled
                    && !self.s.prg_ram_write_protected
                {
                    let index = (addr - 0x6000) as usize;

                    if let Some(byte) = self.s.prg_ram.get_mut(index) {
                        *byte = value;
                        self.s.sram_dirty = true;
                    }
                }
            }

            0x8000..=0x9FFF => {
                if addr & 1 == 0 {
                    // Bank select.
                    self.s.command = value;
                    self.s.prg_mode = value & 0x40 != 0;
                    self.s.chr_mode = value & 0x80 != 0;
                } else {
                    // Bank data.
                    let reg = (self.s.command & 0x07) as usize;

                    match reg {
                        0..=5 => {
                            // R0 and R1 are 2 KiB registers.
                            self.s.chr_regs[reg] = if reg <= 1 {
                                value & 0xFE
                            } else {
                                value
                            };
                        }
                        6 => self.s.prg_regs[0] = value & 0x3F,
                        7 => self.s.prg_regs[1] = value & 0x3F,
                        _ => {}
                    }
                }
            }

            0xA000..=0xBFFF => {
                if addr & 1 == 0 {
                    // $A000: mirroring control.
                    self.set_mirroring(value);
                } else {
                    // $A001: PRG RAM protect control.
                    self.s.prg_ram_enabled = value & 0x80 != 0;
                    self.s.prg_ram_write_protected = value & 0x40 != 0;
                }
            }

            0xC000..=0xDFFF => {
                if addr & 1 == 0 {
                    // $C000: IRQ latch.
                    self.s.irq_latch = value;
                } else {
                    // $C001: request counter reload.
                    self.s.irq_counter = 0;
                    self.s.irq_reload = true;
                }
            }

            0xE000..=0xFFFF => {
                if addr & 1 == 0 {
                    // $E000: disable IRQ and acknowledge pending IRQ.
                    self.s.irq_enabled = false;
                    self.s.irq_pending = false;
                } else {
                    // $E001: enable IRQ.
                    self.s.irq_enabled = true;
                }
            }

            _ => {}
        }
    }

    fn ppu_read(&self, addr: u16) -> u8 {
        let (is_ram, index) = self.resolve_chr(addr as usize);

        if is_ram {
            self.chr_ram.get(index).copied().unwrap_or(0)
        } else {
            self.chr_rom.get(index).copied().unwrap_or(0)
        }
    }

    fn ppu_write(&mut self, addr: u16, value: u8) {
        let (is_ram, index) = self.resolve_chr(addr as usize);

        if is_ram {
            if let Some(byte) = self.chr_ram.get_mut(index) {
                *byte = value;
            }
        }
    }

    fn update_a12(&mut self, addr: u16, ppu_cycle: u64) {
        let high = addr & 0x1000 != 0;

        if !high {
            if self.s.a12_high {
                self.s.a12_high = false;
                self.s.last_a12_low_cycle = ppu_cycle;
                self.s.a12_low_seen = true;
            }
            return;
        }

        // Only process a rising edge.
        if !self.s.a12_high {
            self.s.a12_high = true;

            // MMC3 filters out A12 rising edges that were preceded
            // by fewer than 8 low PPU cycles.
            if self.s.a12_low_seen
                && ppu_cycle.wrapping_sub(self.s.last_a12_low_cycle) >= 8
            {
                self.clock_irq();
            }
        }
    }

    fn is_irq_asserted(&self) -> bool {
        self.s.irq_pending
    }

    fn mirror_vram_address(&self, addr: u16) -> usize {
        let mirroring = match self.s.mirroring {
            0 => Mirroring::Vertical,
            1 => Mirroring::Horizontal,
            2 => Mirroring::SingleLower,
            3 => Mirroring::SingleUpper,
            4 => Mirroring::FourScreen,
            _ => Mirroring::Horizontal,
        };

        mirror_nt(mirroring, addr)
    }

    fn get_sram(&self) -> Option<&[u8]> {
        Some(&self.s.prg_ram)
    }

    fn load_sram(&mut self, data: &[u8]) {
        let n = data.len().min(self.s.prg_ram.len());
        self.s.prg_ram[..n].copy_from_slice(&data[..n]);
    }

    fn is_sram_dirty(&self) -> bool {
        self.s.sram_dirty
    }

    fn clear_sram_dirty(&mut self) {
        self.s.sram_dirty = false;
    }

    fn save_state(&self) -> Vec<u8> {
        encode_state(&self.s)
    }

    fn load_state(&mut self, data: &[u8]) {
        if let Some(mut state) = decode_state::<State>(data) {
            // Keep the mapper state safe if loading a malformed state.
            state.prg_ram.resize(PRG_RAM_SIZE, 0);
            state.prg_ram.truncate(PRG_RAM_SIZE);

            state.command &= 0xC7;
            state.prg_regs[0] &= 0x3F;
            state.prg_regs[1] &= 0x3F;

            // Cartridge wiring is fixed by the loaded ROM.
            if self.four_screen {
                state.mirroring = 4;
            } else if state.mirroring > 3 {
                state.mirroring = 0;
            }

            self.s = state;
        }
    }
}
