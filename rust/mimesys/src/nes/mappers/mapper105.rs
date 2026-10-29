
use crate::nes::mappers::{Mapper, Mirroring};
use serde::{Deserialize, Serialize};

// iNES Mapper 105: NES-EVENT / Nintendo World Championships 1990.
//
// MMC1-derived board with:
// - 256 KiB PRG-ROM (two 128 KiB regions)
// - 8 KiB CHR-RAM
// - 8 KiB PRG-RAM
// - 30-bit CPU-cycle timer IRQ
// - PRG banking unlocked by clearing and then setting the I bit
//   in the MMC1 $A000 register.
//
// Default IRQ threshold uses the official tournament setting:
// 0x28000000 CPU cycles.

const PRG_BANK_SIZE: usize = 0x4000; // 16 KiB
const CHR_RAM_SIZE: usize = 0x2000;  // 8 KiB
const PRG_RAM_SIZE: usize = 0x2000;  // 8 KiB

const IRQ_COUNTER_MASK: u32 = 0x3FFF_FFFF;
const IRQ_THRESHOLD: u32 = 0x2800_0000;

pub struct Mapper105 {
    prg_rom: Vec<u8>,
    prg_ram: Vec<u8>,
    chr_ram: Vec<u8>,

    mirroring_mode: Mirroring,
    has_four_screen: bool,

    shift_reg: u8,
    write_count: u8,

    control: u8,
    chr_bank_0: u8,
    chr_bank_1: u8,
    prg_bank: u8,

    // NES-EVENT-specific $A000 register.
    event_reg: u8,

    // The board starts locked to the first 32 KiB.
    unlocked: bool,
    saw_timer_run: bool,

    // The I bit is bit 4 of event_reg:
    // 0 = timer runs
    // 1 = timer stops, resets, and acknowledges IRQ.
    timer_running: bool,
    irq_counter: u32,
    irq_pending: bool,

    sram_dirty: bool,

    last_write_cycle: i64,
    current_cycle: i64,
}

#[derive(Serialize, Deserialize)]
struct Mapper105State {
    mirroring_mode: Mirroring,

    shift_reg: u8,
    write_count: u8,

    control: u8,
    chr_bank_0: u8,
    chr_bank_1: u8,
    prg_bank: u8,

    event_reg: u8,
    unlocked: bool,
    saw_timer_run: bool,
    timer_running: bool,
    irq_counter: u32,
    irq_pending: bool,

    last_write_cycle: i64,
    current_cycle: i64,

    #[serde(with = "serde_bytes")]
    prg_ram: Vec<u8>,

    #[serde(with = "serde_bytes")]
    chr_ram: Vec<u8>,
}

impl Mapper105 {
    pub fn new(
        _mapper: u16,
        _submapper: u8,
        prg_rom: Vec<u8>,
        _chr_rom: Vec<u8>,
        initial_mirroring: Mirroring,
    ) -> Self {
        let has_four_screen = matches!(
            initial_mirroring,
            Mirroring::FourScreen
        );

        Self {
            prg_rom,
            prg_ram: vec![0; PRG_RAM_SIZE],
            chr_ram: vec![0; CHR_RAM_SIZE],

            mirroring_mode: initial_mirroring,
            has_four_screen,

            shift_reg: 0x10,
            write_count: 0,

            control: 0x0C,
            chr_bank_0: 0,
            chr_bank_1: 0,
            prg_bank: 0,

            event_reg: 0,
            unlocked: false,
            saw_timer_run: false,

            timer_running: true,
            irq_counter: 0,
            irq_pending: false,

            sram_dirty: false,

            last_write_cycle: -1,
            current_cycle: 0,
        }
    }

    fn update_mirroring(&mut self) {
        if self.has_four_screen {
            self.mirroring_mode = Mirroring::FourScreen;
            return;
        }

        self.mirroring_mode = match self.control & 0x03 {
            0 => Mirroring::SingleLower,
            1 => Mirroring::SingleUpper,
            2 => Mirroring::Vertical,
            3 => Mirroring::Horizontal,
            _ => unreachable!(),
        };
    }

    fn prg_bank_count(&self) -> usize {
        self.prg_rom.len() / PRG_BANK_SIZE
    }

    fn normalized_bank(&self, bank: usize) -> usize {
        let count = self.prg_bank_count();

        if count == 0 {
            0
        } else {
            bank % count
        }
    }

    fn ram_disabled(&self) -> bool {
        // MMC1-style PRG-RAM disable bit.
        self.prg_bank & 0x10 != 0
    }

    fn read_prg_bank(&self, bank: usize, addr: u16) -> u8 {
        let count = self.prg_bank_count();

        if count == 0 {
            return 0;
        }

        let bank = self.normalized_bank(bank);
        let offset = (addr as usize) & (PRG_BANK_SIZE - 1);
        let index = bank * PRG_BANK_SIZE + offset;

        self.prg_rom.get(index).copied().unwrap_or(0)
    }

    fn cpu_read_prg(&self, addr: u16) -> u8 {
        let count = self.prg_bank_count();

        if count == 0 {
            return 0;
        }

        // Before initialization, the first 32 KiB of the first
        // PRG chip is selected regardless of MMC1 register contents.
        if !self.unlocked {
            let bank = if addr < 0xC000 { 0 } else { 1 };
            return self.read_prg_bank(bank, addr);
        }

        let chip_select = (self.event_reg >> 3) & 1;
        let bank_a = (self.event_reg >> 1) & 0x03;

        if chip_select == 0 {
            // First 128 KiB PRG chip.
            // A selects one of four 32 KiB banks.
            let bank = (bank_a as usize) * 2;

            if addr < 0xC000 {
                self.read_prg_bank(bank, addr)
            } else {
                self.read_prg_bank(bank + 1, addr)
            }
        } else {
            // Second 128 KiB PRG chip occupies 16 KiB banks 8-15.
            // The MMC1 PRG mode and B register control its mapping.
            let base = 8usize;
            let mode = (self.control >> 2) & 0x03;
            let bank_b = (self.prg_bank & 0x0F) as usize;

            match mode {
                // 32 KiB mode: switch a pair of 16 KiB banks.
                0 | 1 => {
                    let bank = base + (bank_b & 0x0E);

                    if addr < 0xC000 {
                        self.read_prg_bank(bank, addr)
                    } else {
                        self.read_prg_bank(bank + 1, addr)
                    }
                }

                // Fix the first bank, switch the upper bank.
                2 => {
                    if addr < 0xC000 {
                        self.read_prg_bank(base, addr)
                    } else {
                        self.read_prg_bank(base + bank_b, addr)
                    }
                }

                // Switch the lower bank, fix the last bank.
                3 => {
                    if addr < 0xC000 {
                        self.read_prg_bank(base + bank_b, addr)
                    } else {
                        self.read_prg_bank(base + 7, addr)
                    }
                }

                _ => unreachable!(),
            }
        }
    }

    fn write_event_register(&mut self, value: u8) {
        let old_i = self.event_reg & 0x10 != 0;
        let new_i = value & 0x10 != 0;

        self.event_reg = value & 0x1E;

        // Clearing I allows the timer to run.
        if !new_i {
            self.timer_running = true;
            self.saw_timer_run = true;
        } else {
            // Setting I resets the timer and acknowledges IRQ.
            self.timer_running = false;
            self.irq_counter = 0;
            self.irq_pending = false;

            // The board unlocks after I has first been cleared
            // and is subsequently set.
            if old_i == false && self.saw_timer_run {
                self.unlocked = true;
            }
        }
    }

    fn commit_register(&mut self, addr: u16, value: u8) {
        let target = (addr >> 13) & 0x03;

        match target {
            0 => {
                // $8000-$9FFF: MMC1 control.
                self.control = value & 0x1F;
                self.update_mirroring();
            }

            1 => {
                // $A000-$BFFF: NES-EVENT register.
                self.write_event_register(value);
            }

            2 => {
                // $C000-$DFFF: normally unused on this board.
                // Preserve the MMC1 register because the underlying
                // MMC1 serial interface still receives writes.
                self.chr_bank_1 = value & 0x1F;
            }

            3 => {
                // $E000-$FFFF: WRAM disable + PRG register B.
                self.prg_bank = value & 0x1F;
            }

            _ => unreachable!(),
        }
    }

    fn write_serial(&mut self, addr: u16, value: u8) {
        // MMC1 ignores a write occurring immediately after another
        // CPU-cycle write. CPU writes should be supplied with
        // correct current_cycle timing by the emulator.
        if self.last_write_cycle >= 0
            && self.current_cycle - self.last_write_cycle <= 1
        {
            self.last_write_cycle = self.current_cycle;
            return;
        }

        self.last_write_cycle = self.current_cycle;

        if value & 0x80 != 0 {
            self.shift_reg = 0x10;
            self.write_count = 0;
            self.control |= 0x0C;
            self.update_mirroring();
            return;
        }

        self.shift_reg = (self.shift_reg >> 1) | ((value & 1) << 4);
        self.write_count += 1;

        if self.write_count == 5 {
            let committed = self.shift_reg & 0x1F;
            self.commit_register(addr, committed);

            self.shift_reg = 0x10;
            self.write_count = 0;
        }
    }
}

impl Mapper for Mapper105 {
    fn step_cycles(&mut self, cycles: u64) {
        if cycles == 0 {
            return;
        }

        self.current_cycle = self
            .current_cycle
            .saturating_add(cycles.min(i64::MAX as u64) as i64);

        if self.timer_running {
            let next = (self.irq_counter as u64)
                .saturating_add(cycles)
                .min(IRQ_COUNTER_MASK as u64) as u32;

            self.irq_counter = next;

            if self.irq_counter >= IRQ_THRESHOLD {
                self.irq_pending = true;
                // Hardware keeps the IRQ asserted until the I bit is
                // set to acknowledge/reset the timer.
            }
        }
    }

    fn total_cycles(&self) -> u64 {
        self.current_cycle.max(0) as u64
    }

    fn get_sram(&self) -> Option<&[u8]> {
        Some(&self.prg_ram)
    }

    fn load_sram(&mut self, data: &[u8]) {
        let n = data.len().min(self.prg_ram.len());
        self.prg_ram[..n].copy_from_slice(&data[..n]);
    }

    fn is_sram_dirty(&self) -> bool {
        self.sram_dirty
    }

    fn clear_sram_dirty(&mut self) {
        self.sram_dirty = false;
    }

    fn cpu_read(&self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => {
                if self.ram_disabled() {
                    return 0;
                }

                self.prg_ram
                    .get((addr - 0x6000) as usize)
                    .copied()
                    .unwrap_or(0)
            }

            0x8000..=0xFFFF => self.cpu_read_prg(addr),

            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, value: u8) {
        match addr {
            0x6000..=0x7FFF => {
                if !self.ram_disabled() {
                    let index = (addr - 0x6000) as usize;

                    if let Some(byte) = self.prg_ram.get_mut(index) {
                        *byte = value;
                        self.sram_dirty = true;
                    }
                }
            }

            0x8000..=0xFFFF => {
                self.write_serial(addr, value);
            }

            _ => {}
        }
    }

    fn ppu_read(&self, addr: u16) -> u8 {
        if addr < 0x2000 {
            self.chr_ram
                .get(addr as usize)
                .copied()
                .unwrap_or(0)
        } else {
            0
        }
    }

    fn ppu_write(&mut self, addr: u16, value: u8) {
        if addr < 0x2000 {
            if let Some(byte) = self.chr_ram.get_mut(addr as usize) {
                *byte = value;
            }
        }
    }

    fn mirror_vram_address(&self, addr: u16) -> usize {
        let normalized = (addr & 0x0FFF) as usize;

        if self.has_four_screen {
            return normalized;
        }

        match self.mirroring_mode {
            Mirroring::Horizontal => {
                if normalized < 0x800 {
                    normalized % 0x400
                } else {
                    0x400 + normalized % 0x400
                }
            }

            Mirroring::Vertical => normalized % 0x800,

            Mirroring::SingleLower => normalized % 0x400,

            Mirroring::SingleUpper => {
                0x400 + normalized % 0x400
            }

            Mirroring::FourScreen => normalized,
        }
    }

    fn is_irq_asserted(&self) -> bool {
        self.irq_pending
    }

    fn save_state(&self) -> Vec<u8> {
        let state = Mapper105State {
            mirroring_mode: self.mirroring_mode,

            shift_reg: self.shift_reg,
            write_count: self.write_count,

            control: self.control,
            chr_bank_0: self.chr_bank_0,
            chr_bank_1: self.chr_bank_1,
            prg_bank: self.prg_bank,

            event_reg: self.event_reg,
            unlocked: self.unlocked,
            saw_timer_run: self.saw_timer_run,
            timer_running: self.timer_running,
            irq_counter: self.irq_counter,
            irq_pending: self.irq_pending,

            last_write_cycle: self.last_write_cycle,
            current_cycle: self.current_cycle,

            prg_ram: self.prg_ram.clone(),
            chr_ram: self.chr_ram.clone(),
        };

        let config = bincode::config::standard()
            .with_fixed_int_encoding();

        bincode::serde::encode_to_vec(&state, config)
            .unwrap_or_default()
    }

    fn load_state(&mut self, state_bytes: &[u8]) {
        let config = bincode::config::standard()
            .with_fixed_int_encoding();

        if let Ok((state, _)) =
            bincode::serde::decode_from_slice::<Mapper105State, _>(
                state_bytes,
                config,
            )
        {
            self.mirroring_mode = if self.has_four_screen {
                Mirroring::FourScreen
            } else {
                state.mirroring_mode
            };

            self.shift_reg = state.shift_reg & 0x1F;
            self.write_count = state.write_count.min(4);

            self.control = state.control & 0x1F;
            self.chr_bank_0 = state.chr_bank_0 & 0x1F;
            self.chr_bank_1 = state.chr_bank_1 & 0x1F;
            self.prg_bank = state.prg_bank & 0x1F;

            self.event_reg = state.event_reg & 0x1E;
            self.unlocked = state.unlocked;
            self.saw_timer_run = state.saw_timer_run;
            self.timer_running = state.timer_running;
            self.irq_counter = state.irq_counter & IRQ_COUNTER_MASK;
            self.irq_pending = state.irq_pending;

            self.last_write_cycle = state.last_write_cycle;
            self.current_cycle = state.current_cycle.max(0);

            self.prg_ram = state.prg_ram;
            self.prg_ram.resize(PRG_RAM_SIZE, 0);
            self.prg_ram.truncate(PRG_RAM_SIZE);

            self.chr_ram = state.chr_ram;
            self.chr_ram.resize(CHR_RAM_SIZE, 0);
            self.chr_ram.truncate(CHR_RAM_SIZE);
        }
    }
}
