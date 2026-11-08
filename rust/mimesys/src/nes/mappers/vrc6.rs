use crate::nes::mappers::{Mapper, Mirroring};
use serde::{Serialize, Deserialize};
use std::cell::Cell;

// VRC6 Mapper (iNES Mappers 24 and 26)
// Used in games like Akumajou Densetsu (Castlevania III - JP) and Madara.

pub struct MapperVRC6 {
    prg_rom: Vec<u8>,
    prg_ram: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_ram: Vec<u8>,

    prg_bank_16k: usize,
    prg_bank_8k: usize,
    chr_regs: [u8; 8],

    b003: u8,
    initial_mirroring: Mirroring,
    has_four_screen: bool,
    is_mapper_26: bool, // true for mapper 26 (VRC6b), false for mapper 24 (VRC6a)

    // IRQ State
    irq_latch: u8,
    irq_counter: Cell<u8>,
    irq_enabled: bool,
    irq_mode: bool, // 0 = scanline, 1 = cycle
    irq_reload: bool, // the "A" bit: copied to irq_enabled when the IRQ is acknowledged ($F002)
    irq_pending: Cell<bool>,
    prescaler: Cell<u16>,

    sram_dirty: bool,
    trace: Trace,
    audio: Vrc6Audio,
}

// ---------------------------------------------------------------- VRC6 sound
// Two pulse channels and a sawtooth (NESdev wiki "VRC6 audio"):
// - pulse: 12-bit divider, a 16-step duty counter going 15 -> 0; outputs its volume while
//   the step is <= duty (or always in "mode" = bit 7). Disabling resets the duty step.
// - saw: 12-bit divider, 14 steps: on every even step 2..12 the rate is added to an
//   8-bit accumulator, reset at step 14; outputs the top 5 bits.
// - $9003: bit 0 halts everything, bit 1 = periods >> 4, bit 2 = periods >> 8.
// The 6-bit DAC is linear; a pulse at volume 15 is about as loud as a 2A03 square at 15.

#[derive(Clone, Default, Serialize, Deserialize)]
struct Vrc6Pulse { volume: u8, duty: u8, mode: bool, period: u16, enabled: bool, timer: u16, step: u8 }

impl Vrc6Pulse {
    fn write(&mut self, reg: u16, v: u8) {
        match reg {
            0 => { self.volume = v & 0x0F; self.duty = (v >> 4) & 7; self.mode = v & 0x80 != 0; }
            1 => self.period = self.period & 0xF00 | v as u16,
            _ => {
                self.period = self.period & 0x0FF | ((v as u16 & 0x0F) << 8);
                self.enabled = v & 0x80 != 0;
                if !self.enabled { self.step = 15; }
            }
        }
    }
    fn clock(&mut self, shift: u8) {
        if !self.enabled { return; }
        if self.timer == 0 {
            self.timer = self.period >> shift;
            self.step = if self.step == 0 { 15 } else { self.step - 1 };
        } else {
            self.timer -= 1;
        }
    }
    fn output(&self) -> u8 {
        if self.enabled && (self.mode || self.step <= self.duty) { self.volume } else { 0 }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Vrc6Saw { rate: u8, period: u16, enabled: bool, timer: u16, step: u8, acc: u8 }

impl Vrc6Saw {
    fn write(&mut self, reg: u16, v: u8) {
        match reg {
            0 => self.rate = v & 0x3F,
            1 => self.period = self.period & 0xF00 | v as u16,
            _ => {
                self.period = self.period & 0x0FF | ((v as u16 & 0x0F) << 8);
                self.enabled = v & 0x80 != 0;
                if !self.enabled { self.step = 0; self.acc = 0; }
            }
        }
    }
    fn clock(&mut self, shift: u8) {
        if !self.enabled { return; }
        if self.timer == 0 {
            self.timer = self.period >> shift;
            self.step += 1;
            if self.step == 14 {
                self.step = 0;
                self.acc = 0;
            } else if self.step % 2 == 0 {
                self.acc = self.acc.wrapping_add(self.rate);
            }
        } else {
            self.timer -= 1;
        }
    }
    fn output(&self) -> u8 { if self.enabled { self.acc >> 3 } else { 0 } }
}

#[derive(Clone, Serialize, Deserialize)]
struct Vrc6Audio { pulse: [Vrc6Pulse; 2], saw: Vrc6Saw, halt: bool, shift: u8 }

impl Vrc6Audio {
    fn new() -> Self {
        let mut a = Self { pulse: Default::default(), saw: Vrc6Saw::default(), halt: false, shift: 0 };
        a.pulse[0].step = 15;
        a.pulse[1].step = 15;
        a
    }
    /// A write to $9000-$9003, $A000-$A002, $B000-$B002 (after the mapper 26 address swap).
    fn write(&mut self, addr: u16, v: u8) {
        let reg = addr & 3;
        match addr & 0xF000 {
            0x9000 if reg == 3 => {
                self.halt = v & 1 != 0;
                self.shift = if v & 4 != 0 { 8 } else if v & 2 != 0 { 4 } else { 0 };
            }
            0x9000 => self.pulse[0].write(reg, v),
            0xA000 if reg < 3 => self.pulse[1].write(reg, v),
            0xB000 if reg < 3 => self.saw.write(reg, v),
            _ => {}
        }
    }
    fn clock(&mut self) {
        if self.halt { return; }
        self.pulse[0].clock(self.shift);
        self.pulse[1].clock(self.shift);
        self.saw.clock(self.shift);
    }
    /// In the APU's mix units: one step of volume = 1/15 of a 2A03 square at volume 15.
    fn level(&self) -> f32 {
        const UNIT: f32 = 0.1494 / 15.0;
        (self.pulse[0].output() + self.pulse[1].output() + self.saw.output()) as f32 * UNIT
    }
}

/// Debug aid: banking/IRQ register writes of each frame, logged (to the debug log) whenever a
/// frame's list differs from the one before, so a steady game is silent.
#[derive(Default)]
struct Trace {
    cycle: u64,
    frame: u64,
    writes: Vec<(u16, u16, u8)>, // CPU cycle / 114 within the frame, register, value
    last: Vec<(u16, u16, u8)>,
    lines: u32,
}

impl Trace {
    fn tick(&mut self) {
        self.cycle += 1;
        if self.cycle >= 29781 {
            self.cycle = 0;
            self.frame += 1;
            if self.writes != self.last && self.lines < 4000 {
                self.lines += 1;
                let list: Vec<String> = self.writes.iter()
                    .map(|(l, r, v)| format!("{l}:{r:04X}={v:02X}")).collect();
                crate::dlog!("VRC6 frame {}: {}", self.frame, list.join(" "));
            }
            self.last = std::mem::take(&mut self.writes);
        }
    }
    fn write(&mut self, reg: u16, v: u8) {
        if matches!(reg, 0x8000..=0x8003 | 0xB003 | 0xC000..=0xC003 | 0xD000..=0xE003 | 0xF000..=0xF002)
            && self.writes.len() < 64 {
            self.writes.push(((self.cycle / 114) as u16, reg, v));
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct MapperVRC6StateVariables {
    prg_bank_16k: usize,
    prg_bank_8k: usize,
    chr_regs: [u8; 8],
    b003: u8,
    irq_latch: u8,
    irq_counter: u8,
    irq_enabled: bool,
    irq_mode: bool,
    irq_reload: bool,
    irq_pending: bool,
    prescaler: u16,
    prg_ram: Vec<u8>,
    audio: Vrc6Audio,
}

impl MapperVRC6 {
    pub fn new(
        _prg_banks: usize,
        _chr_banks: usize,
        prg_rom: Vec<u8>,
        chr_rom: Vec<u8>,
        initial_mirroring: Mirroring,
        four_screen_bit: bool,
        is_mapper_26: bool,
    ) -> Self {
        let chr_ram = if chr_rom.is_empty() { vec![0; 8192] } else { vec![] };
        
        Self {
            prg_rom,
            prg_ram: vec![0; 8192], // Standard 8K PRG RAM for VRC6 games that support it
            chr_rom,
            chr_ram,
            prg_bank_16k: 0,
            prg_bank_8k: 0,
            chr_regs: [0; 8],
            b003: 0,
            initial_mirroring,
            has_four_screen: four_screen_bit,
            is_mapper_26,
            irq_latch: 0,
            irq_counter: Cell::new(0),
            irq_enabled: false,
            irq_mode: false,
            irq_reload: false,
            irq_pending: Cell::new(false),
            prescaler: Cell::new(341),
            sram_dirty: false,
            trace: Trace::default(),
            audio: Vrc6Audio::new(),
        }
    }

    /// VRC6b (Mapper 26) swaps address lines A0 and A1 for register access compared to VRC6a (Mapper 24).
    fn map_reg_addr(&self, addr: u16) -> u16 {
        if self.is_mapper_26 {
            (addr & 0xFFFC) | ((addr & 1) << 1) | ((addr >> 1) & 1)
        } else {
            addr
        }
    }

    fn resolve_prg(&self, addr: u16) -> (bool, usize, usize) {
        let offset = (addr & 0x1FFF) as usize;
        match addr {
            0x8000..=0x9FFF => {
                // 16KB bank split into two 8KB windows
                let sub_bank = (addr & 0x2000) != 0;
                let bank = (self.prg_bank_16k * 2) + if sub_bank { 1 } else { 0 };
                (false, bank, offset)
            }
            0xA000..=0xBFFF => {
                let sub_bank = (addr & 0x2000) != 0;
                let bank = (self.prg_bank_16k * 2) + if sub_bank { 1 } else { 0 };
                (false, bank, offset)
            }
            0xC000..=0xDFFF => {
                (false, self.prg_bank_8k, offset)
            }
            0xE000..=0xFFFF => {
                // Fixed to last 8KB of PRG ROM
                let total_8k_banks = self.prg_rom.len() / 0x2000;
                let bank = if total_8k_banks > 0 { total_8k_banks - 1 } else { 0 };
                (false, bank, offset)
            }
            0x6000..=0x7FFF => {
                (true, 0, (addr & 0x1FFF) as usize)
            }
            _ => (false, 0, 0),
        }
    }

    fn read_chr_bank(&self, addr: u16) -> u8 {
        let bank_idx = ((addr & 0x1FFF) / 0x0400) as usize;
        let reg_val = self.chr_regs[bank_idx] as usize;
        let chr_offset = (addr & 0x03FF) as usize;

        if !self.chr_rom.is_empty() {
            let final_addr = (reg_val * 0x400) + chr_offset;
            self.chr_rom[final_addr % self.chr_rom.len()]
        } else if !self.chr_ram.is_empty() {
            let final_addr = (reg_val * 0x400) + chr_offset;
            self.chr_ram[final_addr % self.chr_ram.len()]
        } else {
            0
        }
    }

    fn clock_irq(&self) {
        if !self.irq_enabled {
            return;
        }
        if self.irq_counter.get() == 0xFF {
            self.irq_counter.set(self.irq_latch);
            self.irq_pending.set(true);
        } else {
            self.irq_counter.set(self.irq_counter.get().wrapping_add(1));
        }
    }
}

impl Mapper for MapperVRC6 {
    fn step_cycles(&mut self, cycles: u64) {
        for _ in 0..cycles {
            self.trace.tick();
            self.audio.clock();
            if !self.irq_enabled {
                continue;
            }
            if self.irq_mode {
                // Cycle mode: clock every CPU cycle
                self.clock_irq();
            } else {
                // Scanline mode: the prescaler counts 341 PPU dots = 113.67 CPU cycles
                // (it really divides by 114, 114, 113): start at 341, take 3 per CPU
                // cycle, add 341 back and clock the counter when it reaches 0 or below.
                let mut prescaler = self.prescaler.get() as i32 - 3;
                if prescaler <= 0 {
                    prescaler += 341;
                    self.clock_irq();
                }
                self.prescaler.set(prescaler as u16);
            }
        }
    }

    fn audio_out(&mut self) -> f32 { self.audio.level() }

    fn get_sram(&self) -> Option<&[u8]> {
        Some(&self.prg_ram)
    }

    fn load_sram(&mut self, data: &[u8]) {
        if data.len() == self.prg_ram.len() {
            self.prg_ram.copy_from_slice(data);
        }
    }

    fn is_sram_dirty(&self) -> bool {
        self.sram_dirty
    }

    fn clear_sram_dirty(&mut self) {
        self.sram_dirty = false;
    }

    fn is_irq_asserted(&self) -> bool {
        self.irq_pending.get()
    }

    fn cpu_read(&self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => {
                let offset = (addr & 0x1FFF) as usize;
                self.prg_ram[offset % self.prg_ram.len()]
            }
            0x8000..=0xFFFF => {
                let (_, bank, offset) = self.resolve_prg(addr);
                let len = self.prg_rom.len();
                if len == 0 { return 0; }
                self.prg_rom[(bank * 0x2000 + offset) % len]
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, value: u8) {
        let reg_addr = self.map_reg_addr(addr);
        self.trace.write(reg_addr, value);
        match reg_addr {
            0x6000..=0x7FFF => {
                let offset = (addr & 0x1FFF) as usize;
                let len = self.prg_ram.len();
                self.prg_ram[offset % len] = value;
                self.sram_dirty = true;
            }
            // 16k PRG Select ($8000-$8003)
            0x8000..=0x8003 => {
                self.prg_bank_16k = (value & 0x7F) as usize;
            }
            // 8k PRG Select ($C000-$C003)
            0xC000..=0xC003 => {
                self.prg_bank_8k = (value & 0x7F) as usize;
            }
            // PPU Banking Style & Mirroring ($B003)
            0xB003 => {
                self.b003 = value;
            }
            // CHR Select 0..3 ($D000-$D003)
            0xD000..=0xD003 => {
                let idx = (reg_addr & 3) as usize;
                self.chr_regs[idx] = value;
            }
            // CHR Select 4..7 ($E000-$E003)
            0xE000..=0xE003 => {
                let idx = 4 + (reg_addr & 3) as usize;
                self.chr_regs[idx] = value;
            }
            // IRQ Latch ($F000)
            0xF000 => {
                self.irq_latch = value;
            }
            // IRQ Control ($F001): A (bit 0), E (bit 1), M (bit 2). Any write acknowledges
            // and resets the prescaler; with E set the counter reloads from the latch.
            0xF001 => {
                self.irq_reload = (value & 0x01) != 0;
                self.irq_enabled = (value & 0x02) != 0;
                self.irq_mode = (value & 0x04) != 0;
                self.irq_pending.set(false);
                self.prescaler.set(341);
                if self.irq_enabled {
                    self.irq_counter.set(self.irq_latch);
                }
            }
            // IRQ Acknowledge ($F002): A is copied to E (a game wanting one IRQ a frame
            // leaves A clear, so the IRQ switches off here)
            0xF002 => {
                self.irq_pending.set(false);
                self.irq_enabled = self.irq_reload;
            }
            // Sound
            0x9000..=0x9003 | 0xA000..=0xA002 | 0xB000..=0xB002 => {
                self.audio.write(reg_addr, value);
            }
            _ => {}
        }
    }

    fn ppu_read(&self, addr: u16) -> u8 {
        if addr < 0x2000 {
            self.read_chr_bank(addr)
        } else {
            0
        }
    }

    fn ppu_write(&mut self, addr: u16, value: u8) {
        if addr < 0x2000 && self.chr_rom.is_empty() && !self.chr_ram.is_empty() {
            let len = self.chr_ram.len();
            self.chr_ram[(addr as usize) % len] = value;
        }
    }

    fn mirror_vram_address(&self, addr: u16) -> usize {
        let normalized = (addr & 0x0FFF) as usize;
        if self.has_four_screen {
            return normalized;
        }
        let mirroring_bits = (self.b003 >> 2) & 0x03;
        match mirroring_bits {
            0 => { // Vertical / 1-screen A
                if normalized >= 0x0800 { normalized - 0x0800 } else { normalized }
            }
            1 => { // Horizontal: $2000/$2400 are page A (0), $2800/$2C00 page B ($400), the
                   // same pages the one-screen modes use (the game flips between them mid-frame)
                (normalized & 0x03FF) | if normalized >= 0x0800 { 0x0400 } else { 0 }
            }
            2 => normalized % 0x0400, // 1-screen lower bank
            3 => 0x0400 + (normalized % 0x0400), // 1-screen upper bank
            _ => normalized,
        }
    }

    fn save_state(&self) -> Vec<u8> {
        let variables = MapperVRC6StateVariables {
            prg_bank_16k: self.prg_bank_16k,
            prg_bank_8k: self.prg_bank_8k,
            chr_regs: self.chr_regs,
            b003: self.b003,
            irq_latch: self.irq_latch,
            irq_counter: self.irq_counter.get(),
            irq_enabled: self.irq_enabled,
            irq_mode: self.irq_mode,
            irq_reload: self.irq_reload,
            irq_pending: self.irq_pending.get(),
            prescaler: self.prescaler.get(),
            prg_ram: self.prg_ram.clone(),
            audio: self.audio.clone(),
        };
        let config = bincode::config::standard().with_fixed_int_encoding();
        bincode::serde::encode_to_vec(&variables, config).unwrap_or_default()
    }

    fn load_state(&mut self, state_bytes: &[u8]) {
        let config = bincode::config::standard().with_fixed_int_encoding();
        if let Ok((state, _)) = bincode::serde::decode_from_slice::<MapperVRC6StateVariables, _>(state_bytes, config) {
            self.prg_bank_16k = state.prg_bank_16k;
            self.prg_bank_8k = state.prg_bank_8k;
            self.chr_regs = state.chr_regs;
            self.b003 = state.b003;
            self.irq_latch = state.irq_latch;
            self.irq_counter.set(state.irq_counter);
            self.irq_enabled = state.irq_enabled;
            self.irq_mode = state.irq_mode;
            self.irq_reload = state.irq_reload;
            self.irq_pending.set(state.irq_pending);
            self.prescaler.set(state.prescaler);
            if state.prg_ram.len() == self.prg_ram.len() {
                self.prg_ram = state.prg_ram;
            }
            self.audio = state.audio;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn vrc6(m26: bool) -> MapperVRC6 {
        MapperVRC6::new(16, 32, vec![0; 0x20000], vec![0; 0x20000], Mirroring::Vertical, false, m26)
    }
    /// CPU cycles until the IRQ line goes up (None within `max`).
    fn until_irq(m: &mut MapperVRC6, max: u32) -> Option<u32> {
        (1..=max).find(|_| { m.step_cycles(1); m.is_irq_asserted() })
    }

    #[test]
    fn scanline_irq_timing() {
        let mut m = vrc6(false);
        m.cpu_write(0xF000, 0xF0);                 // 16 scanlines
        m.cpu_write(0xF001, 0x02);                 // E on, A off, scanline mode
        let t = until_irq(&mut m, 5000).unwrap();
        assert!((1817..=1821).contains(&t), "16 lines of 113.67 cycles: {t}");
        // acknowledge with A clear: the IRQ switches off (one IRQ a frame)
        m.cpu_write(0xF002, 0);
        assert!(!m.is_irq_asserted());
        assert_eq!(until_irq(&mut m, 20_000), None, "no second IRQ further down the screen");
        // with A set it keeps going, every 16 lines
        m.cpu_write(0xF001, 0x03);
        until_irq(&mut m, 5000).unwrap();
        m.cpu_write(0xF002, 0);
        let t2 = until_irq(&mut m, 5000).unwrap();
        assert!((1817..=1821).contains(&t2), "{t2}");
    }

    #[test]
    fn cycle_irq_and_no_double_clock() {
        let mut m = vrc6(false);
        m.cpu_write(0xF000, 0x00);
        m.cpu_write(0xF001, 0x06);                 // E + cycle mode
        assert_eq!(until_irq(&mut m, 1000), Some(256));
        // the PPU's scanline signal must not clock the VRC's counter
        let mut m = vrc6(false);
        m.cpu_write(0xF000, 0xFE);
        m.cpu_write(0xF001, 0x02);
        for _ in 0..10 { m.notify_scanline(); }
        assert!(!m.is_irq_asserted());
        assert!(until_irq(&mut m, 300).unwrap() > 200, "2 lines from the prescaler only");
    }

    /// Output over `n` CPU cycles.
    fn run(m: &mut MapperVRC6, n: usize) -> Vec<f32> { (0..n).map(|_| { m.step_cycles(1); m.audio_out() }).collect() }
    fn rising(o: &[f32]) -> usize { o.windows(2).filter(|w| w[1] > w[0] + 1e-6).count() }

    #[test]
    fn mirroring_modes_share_the_same_two_pages() {
        let mut m = vrc6(false);
        let page = |m: &MapperVRC6, nt: u16| m.mirror_vram_address(0x2000 + nt * 0x400 + 5);
        m.cpu_write(0xB003, 0x24); // horizontal
        assert_eq!([0, 1, 2, 3].map(|n| page(&m, n)), [5, 5, 0x405, 0x405]);
        m.cpu_write(0xB003, 0x20); // vertical
        assert_eq!([0, 1, 2, 3].map(|n| page(&m, n)), [5, 0x405, 5, 0x405]);
        m.cpu_write(0xB003, 0x28); // one screen A
        assert_eq!([0, 1, 2, 3].map(|n| page(&m, n)), [5; 4]);
        m.cpu_write(0xB003, 0x2C); // one screen B
        assert_eq!([0, 1, 2, 3].map(|n| page(&m, n)), [0x405; 4]);
    }

    #[test]
    fn pulses() {
        let mut m = vrc6(false);
        m.cpu_write(0x9000, 0x7F);                 // duty 7 (8/16), volume 15
        m.cpu_write(0x9001, 99);
        m.cpu_write(0x9002, 0x80);                 // period 99: CPU / (16 * 100)
        let o = run(&mut m, 160_000);
        assert_eq!(rising(&o), 100, "100 cycles of 1600");
        let on = o.iter().filter(|&&x| x > 0.0).count();
        assert!((79_000..=81_000).contains(&on), "half the time: {on}");
        let peak = o.iter().cloned().fold(0.0, f32::max);
        assert!((peak - 0.1494).abs() < 1e-4, "volume 15 = a full 2A03 square");
        m.cpu_write(0x9000, 0x0F);                 // duty 0: 1/16
        let on = run(&mut m, 160_000).iter().filter(|&&x| x > 0.0).count();
        assert!((9_000..=11_000).contains(&on), "{on}");
        m.cpu_write(0x9000, 0x8F);                 // mode: always on
        assert!(run(&mut m, 5000).iter().all(|&x| x > 0.0));
        m.cpu_write(0x9002, 0x00);                 // disabled: silent
        assert!(run(&mut m, 5000).iter().all(|&x| x == 0.0));
        // $9003 bit 1: periods >> 4 (16x the pitch)
        m.cpu_write(0x9000, 0x7F);
        m.cpu_write(0x9001, 0xFF);
        m.cpu_write(0x9002, 0x81);                 // period 0x1FF
        m.cpu_write(0x9003, 0x02);                 // -> 0x1F
        assert_eq!(rising(&run(&mut m, 51_200)), 100, "16 * 32 cycles a wave");
        m.cpu_write(0x9003, 0x01);                 // halt
        let o = run(&mut m, 3000);
        assert!(o.windows(2).all(|w| w[0] == w[1]), "halted");
    }

    #[test]
    fn sawtooth() {
        let mut m = vrc6(false);
        m.cpu_write(0xB000, 42);                   // the loudest clean rate
        m.cpu_write(0xB001, 9);
        m.cpu_write(0xB002, 0x80);                 // period 9: a step every 10 cycles
        let o = run(&mut m, 140);
        // 6 additions of 42 = 252 at the top: output 252 >> 3 = 31
        let levels: Vec<u32> = o.iter().map(|x| (x / (0.1494 / 15.0)).round() as u32).collect();
        let mut seen: Vec<u32> = levels.clone();
        seen.dedup();
        assert_eq!(seen, vec![0, 5, 10, 15, 21, 26, 31, 0], "a 14-step ramp");
    }

    #[test]
    fn mapper_26_swaps_address_lines() {
        let mut m = vrc6(true);
        m.cpu_write(0x9000, 0x8F);                 // mode on, volume 15
        m.cpu_write(0x9001, 0x80);                 // on mapper 26 this is $9002: enable
        assert!(run(&mut m, 10).iter().all(|&x| x > 0.0));
    }
}
