// Konami sound boards: a second Z80 (1.79 MHz) running the music/effects program,
// driving one or two AY-3-8910s. The main CPU talks to it through an 8255 PPI:
// port A = command latch, port B bit 3 = interrupt (on the falling edge), bit 4 = mute.
//
// Frogger, 1 x AY (MAME frogger_sound_map / frogger_sound_portmap):
//   0000-1FFF  ROM (first 2 KB has data lines D0/D1 swapped)
//   4000-43FF  RAM (mirrored to 5FFF)
//   6000-6FFF  W: filter switches, the ADDRESS bits A6-A11 pick RC filters per channel
//   I/O: A6 = AY data, A7 = AY address (writes); reads with A6 return AY data
//   Timer on AY port B has bits 3 and 5 swapped.
//
// Standard board, 2 x AY: Amidar, Turtles, Scramble... (konami_sound_map / _portmap):
//   0000-1FFF  ROM
//   8000-83FF  RAM (mirrored: any address with A15=1, A12=0)
//   9000-9FFF  W: filter switches (A15=1, A12=1): A0-A5 -> AY #2, A6-A11 -> AY #1
//   I/O writes: A4 = AY #2 address, else A5 = AY #2 data,
//               A6 = AY #1 address, else A7 = AY #1 data (both chips can be hit at once)
//   I/O reads:  A5 -> AY #2 data, A7 -> AY #1 data (ANDed together)
//
// AY #1 port A = command latch, port B = a free-running timer the program uses for tempo.
//
// Reference: MAME src/mame/galaxian/galaxian.cpp (konami_sound_*, frogger_*)

use crate::common::cpu::z80::{Bus, Z80Cpu};
use crate::common::audio::ay8910::Ay8910;

pub const KONAMI_SOUND_CLOCK: f64 = 14_318_181.0 / 8.0;   // CPU and AY: 1.7897 MHz

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KonamiBoard {
    Frogger,   // 1 x AY-3-8910
    TwoAy,     // 2 x AY-3-8910 (Amidar, Turtles)
}

struct KonamiBus {
    board: KonamiBoard,
    rom: Vec<u8>,
    ram: Vec<u8>,
    ay: [Ay8910; 2],        // [0] = AY #1 (latch + timer), [1] = AY #2 (TwoAy only)
    latch: u8,
    total_cycles: u64,      // for the timer on AY port B
}

impl KonamiBus {
    /// MAME konami_sound_timer_r (Frogger: frogger_sound_timer_r, bits 3 and 5 swapped).
    fn timer(&self) -> u8 {
        const PERIOD: u64 = 16 * 16 * 2 * 8 * 5 * 2;
        let mut cycles = (self.total_cycles * 8) % PERIOD;
        let mut hibit = 0;
        if cycles >= PERIOD / 2 {
            hibit = 1;
            cycles -= PERIOD / 2;
        }
        let bit = |n: u32| ((cycles >> n) & 1) as u8;
        let konami = (hibit << 7) | (bit(14) << 6) | (bit(13) << 5) | (bit(11) << 4) | 0x0E;
        match self.board {
            // bitswap<8>(v, 7,6,3,4,5,2,1,0): swap bits 5 and 3
            KonamiBoard::Frogger => (konami & 0xD7) | ((konami & 0x20) >> 2) | ((konami & 0x08) << 2),
            KonamiBoard::TwoAy => konami,
        }
    }

    /// Filter write: each pair of ADDRESS bits switches 0.22 uF / 0.047 uF caps onto
    /// one AY channel. Approximated as a one-pole low-pass per channel (about 1 kohm source).
    /// AY #1 uses A6-A11 on both boards; AY #2 uses A0-A5.
    fn set_filters(&mut self, offset: u16) {
        let chips: &[(usize, u16)] = match self.board {
            KonamiBoard::Frogger => &[(0, 6)],
            KonamiBoard::TwoAy => &[(0, 6), (1, 0)],
        };
        for &(chip, shift) in chips {
            for ch in 0..3 {
                let bits = (offset >> (shift + ch as u16 * 2)) & 3;
                let c = match bits {
                    0 => None,
                    1 => Some(0.22e-6),
                    2 => Some(0.047e-6),
                    _ => Some(0.267e-6),
                };
                let cutoff = c.map(|c| 1.0 / (2.0 * std::f64::consts::PI * 1000.0 * c));
                self.ay[chip].set_channel_lowpass(ch, cutoff);
            }
        }
    }
}

impl Bus for KonamiBus {
    fn read(&mut self, addr: u16) -> u8 {
        match self.board {
            KonamiBoard::Frogger => {
                let a = addr & 0x7FFF;
                match a {
                    0x0000..=0x1FFF => self.rom.get(a as usize).copied().unwrap_or(0xFF),
                    0x4000..=0x5FFF => self.ram[(a & 0x3FF) as usize],
                    _ => 0xFF,
                }
            }
            KonamiBoard::TwoAy => match addr {
                0x0000..=0x1FFF => self.rom.get(addr as usize).copied().unwrap_or(0xFF),
                _ if addr & 0x9000 == 0x8000 => self.ram[(addr & 0x3FF) as usize],
                _ => 0xFF,
            },
        }
    }

    fn write(&mut self, addr: u16, val: u8) {
        match self.board {
            KonamiBoard::Frogger => {
                let a = addr & 0x7FFF;
                match a {
                    0x4000..=0x5FFF => self.ram[(a & 0x3FF) as usize] = val,
                    0x6000..=0x7FFF => self.set_filters(a & 0x0FFF),
                    _ => {}
                }
            }
            KonamiBoard::TwoAy => match addr & 0x9000 {
                0x8000 => self.ram[(addr & 0x3FF) as usize] = val,
                0x9000 => self.set_filters(addr & 0x0FFF),
                _ => {}
            },
        }
    }

    fn port_in(&mut self, port: u16) -> u8 {
        let p = port & 0xFF;
        let timer = self.timer();
        match self.board {
            KonamiBoard::Frogger => {
                if p & 0x40 != 0 { self.ay[0].read_data_ab(self.latch, timer) } else { 0xFF }
            }
            KonamiBoard::TwoAy => {
                let mut v = 0xFF;
                if p & 0x20 != 0 { v &= self.ay[1].read_data_ab(0xFF, 0xFF); }
                if p & 0x80 != 0 { v &= self.ay[0].read_data_ab(self.latch, timer); }
                v
            }
        }
    }

    fn port_out(&mut self, port: u16, val: u8) {
        let p = port & 0xFF;
        match self.board {
            KonamiBoard::Frogger => {
                if p & 0x40 != 0 {
                    self.ay[0].write_data(val);
                } else if p & 0x80 != 0 {
                    self.ay[0].write_address(val);
                }
            }
            KonamiBoard::TwoAy => {
                if p & 0x10 != 0 { self.ay[1].write_address(val); }
                else if p & 0x20 != 0 { self.ay[1].write_data(val); }
                if p & 0x40 != 0 { self.ay[0].write_address(val); }
                else if p & 0x80 != 0 { self.ay[0].write_data(val); }
            }
        }
    }
}

pub type KonamiState = (crate::common::cpu::z80::Z80State, Vec<u8>, [Ay8910; 2], u8, u64, bool, u8, i64);

pub struct KonamiSound {
    board: KonamiBoard,
    cpu: Z80Cpu,
    bus: KonamiBus,
    irq_pending: bool,
    control: u8,
    cycle_debt: i64,
}

impl KonamiSound {
    /// `rom` = the "audiocpu" region, already unscrambled.
    pub fn new(board: KonamiBoard, rom: Vec<u8>, out_rate: u32) -> Self {
        let ay = || Ay8910::new(KONAMI_SOUND_CLOCK as u32, out_rate);
        let mut s = Self {
            board,
            cpu: Z80Cpu::new(),
            bus: KonamiBus {
                board,
                rom,
                ram: vec![0; 0x400],
                ay: [ay(), ay()],
                latch: 0,
                total_cycles: 0,
            },
            irq_pending: false,
            control: 0,
            cycle_debt: 0,
        };
        s.set_gain(false);
        s
    }

    /// Save state: CPU, RAM, both AYs, latches.
    pub fn save(&self) -> impl serde::Serialize + '_ {
        (self.cpu.save_state(), &self.bus.ram, &self.bus.ay, self.bus.latch, self.bus.total_cycles,
         self.irq_pending, self.control, self.cycle_debt)
    }

    pub fn load(&mut self, s: KonamiState) -> Result<(), String> {
        let (cpu, ram, mut ay, latch, total_cycles, irq_pending, control, cycle_debt) = s;
        if ram.len() != self.bus.ram.len() { return Err("sound RAM size mismatch".into()); }
        self.cpu.load_state(&cpu);
        self.bus.ram = ram;
        for (new, old) in ay.iter_mut().zip(self.bus.ay.iter_mut()) {
            new.out = std::mem::take(&mut old.out);
        }
        self.bus.ay = ay;
        self.bus.latch = latch;
        self.bus.total_cycles = total_cycles;
        self.irq_pending = irq_pending;
        self.control = control;
        self.cycle_debt = cycle_debt;
        Ok(())
    }

    /// Two chips share the output, so each gets less headroom.
    fn set_gain(&mut self, muted: bool) {
        let g = match (muted, self.board) {
            (true, _) => 0.0,
            (false, KonamiBoard::Frogger) => 0.6,
            (false, KonamiBoard::TwoAy) => 0.45,
        };
        for ay in self.bus.ay.iter_mut() { ay.gain = g; }
    }

    /// 8255 #2 port A
    pub fn write_latch(&mut self, v: u8) { self.bus.latch = v; }

    /// 8255 #2 port B: bit 3 falling edge interrupts the sound CPU (held until taken),
    /// bit 4 mutes the output.
    pub fn write_control(&mut self, v: u8) {
        if self.control & 0x08 != 0 && v & 0x08 == 0 {
            self.irq_pending = true;
        }
        self.control = v;
        self.set_gain(v & 0x10 != 0);
    }

    /// Run the sound CPU for `cycles` of its own clock (fractions carry over).
    pub fn run(&mut self, cycles: i64) {
        self.cycle_debt += cycles;
        while self.cycle_debt > 0 {
            let mut c = 0;
            if self.irq_pending {
                c = self.cpu.irq(&mut self.bus, 0xFF) as i64;   // HOLD_LINE: cleared once taken
                if c > 0 { self.irq_pending = false; }
            }
            if c == 0 {
                c = self.cpu.step(&mut self.bus) as i64;
            }
            self.cycle_debt -= c;
            // the AY runs from the same clock: one tick per 8 cycles
            let before = self.bus.total_cycles / 8;
            self.bus.total_cycles += c as u64;
            let chips = if self.board == KonamiBoard::TwoAy { 2 } else { 1 };
            for _ in before..self.bus.total_cycles / 8 {
                for ay in self.bus.ay[..chips].iter_mut() { ay.tick(); }
            }
        }
    }

    pub fn drain(&mut self) -> Vec<f32> {
        let mut out = std::mem::take(&mut self.bus.ay[0].out);
        if self.board == KonamiBoard::TwoAy {
            // both chips tick in lockstep, so their buffers line up sample for sample
            let second = std::mem::take(&mut self.bus.ay[1].out);
            for (a, b) in out.iter_mut().zip(second) { *a += b; }
        }
        out
    }

    #[cfg(test)]
    pub(crate) fn timer_for_test(&mut self, cycles: u64) -> u8 { self.bus.total_cycles = cycles; self.bus.timer() }
    #[cfg(test)]
    pub(crate) fn ram(&self, a: usize) -> u8 { self.bus.ram[a] }
}

/// The first sound ROM has data lines D0 and D1 swapped (MAME decode_frogger_sound).
pub fn swap_d0_d1(data: &mut [u8]) {
    for b in data.iter_mut() {
        *b = (*b & 0xFC) | ((*b & 1) << 1) | ((*b >> 1) & 1);
    }
}
