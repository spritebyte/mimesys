//! Donkey Kong 3 sound board: two Ricoh RP2A03s (the NES CPU: a 6502 without decimal
//! mode, plus the NES APU) at 1.789773 MHz. After MAME dkong_a.cpp / dkong.cpp.
//!
//! Each has 2 KB of RAM (mirrored through 0000-7FFF) and an 8 KB ROM at 8000 (mirrored to
//! FFFF). The main Z80 talks to them through three latches: CPU A reads latch 1 at 4016
//! and latch 2 at 4017, CPU B reads latch 3 at 4016. Both get an NMI at vblank and are
//! held in reset while 7D80 bit 0 is low. Writes to the APU registers also land in RAM
//! (a bus conflict the game relies on). Each APU goes to the mono out at half volume.

use serde::{Deserialize, Serialize};
use crate::common::cpu::m6502::{AddressBus, CpuState, CpuVariant, M6502Cpu};
use crate::nes::nes_apu::{ApuState, NesAPU};

pub const CLOCK: u32 = 1_789_773;

struct Mem {
    ram: Vec<u8>,
    rom: Vec<u8>,
    apu: NesAPU,
    /// what 4016 / 4017 read
    latch: [u8; 2],
    nmi: bool,
}

impl Mem {
    fn rom_byte(&self, addr: u16) -> u8 {
        if self.rom.is_empty() { 0 } else { self.rom[(addr as usize & 0x1FFF) % self.rom.len()] }
    }
}

impl AddressBus for Mem {
    fn read_byte(&mut self, addr: u16) -> u8 {
        match addr {
            0x4015 => self.apu.read_4015(),
            0x4016 => self.latch[0],
            0x4017 => self.latch[1],
            0x0000..=0x7FFF => self.ram[addr as usize & 0x7FF],
            _ => self.rom_byte(addr),
        }
    }

    fn write_byte(&mut self, addr: u16, v: u8) {
        if (0x4000..=0x4017).contains(&addr) {
            if addr != 0x4014 && addr != 0x4016 { self.apu.write_reg(addr, v); }
            self.ram[addr as usize & 0x7FF] = v;      // bus conflict: RAM sees it too
        } else if addr < 0x8000 {
            self.ram[addr as usize & 0x7FF] = v;
        }
    }

    fn is_nmi_line_asserted(&mut self) -> bool { self.nmi }
    fn is_irq_line_asserted(&mut self) -> bool { self.apu.is_irq_asserted() }
    fn is_nmi_enabled(&mut self) -> bool { true }
    fn step_cycles(&mut self, _cycles: u64) {}
    fn begin_cpu_cycle(&mut self) {}
}

struct Rp2a03 {
    cpu: M6502Cpu,
    mem: Mem,
}

#[derive(Serialize, Deserialize)]
pub struct Rp2a03State {
    cpu: CpuState,
    ram: Vec<u8>,
    apu: ApuState,
    nmi: bool,
}

impl Rp2a03 {
    fn new(rom: Vec<u8>) -> Self {
        let mut s = Self {
            cpu: M6502Cpu::new(CpuVariant::Ricoh2A03),
            mem: Mem { ram: vec![0; 0x800], rom, apu: NesAPU::new(), latch: [0; 2], nmi: false },
        };
        s.cpu.power_on(&mut s.mem);
        s
    }

    fn cycle(&mut self, running: bool) {
        if running {
            self.cpu.step_one_cycle(&mut self.mem);
            self.cpu.sample_interrupt_lines(&mut self.mem);
        }
        self.mem.apu.step_one_cycle();
        let apu = &mut self.mem.apu;
        if apu.dmc_stop_event { apu.dmc_stop_event = false; }
        if apu.dmc_start_event {
            // sample fetch: done at once (the real chip steals a few CPU cycles for it)
            apu.dmc_start_event = false;
            let a = apu.dmc_dma_address();
            let b = self.mem.read_byte(a);
            self.mem.apu.dmc_dma_complete(b);
        }
    }

    fn save(&self) -> Rp2a03State {
        Rp2a03State { cpu: self.cpu.get_state(), ram: self.mem.ram.clone(), apu: self.mem.apu.get_state(), nmi: self.mem.nmi }
    }

    fn load(&mut self, s: Rp2a03State) -> Result<(), String> {
        if s.ram.len() != self.mem.ram.len() { return Err("save state doesn't fit this board".into()); }
        self.cpu.load_state(&s.cpu);
        self.mem.ram = s.ram;
        self.mem.apu.load_state(&s.apu);
        self.mem.nmi = s.nmi;
        Ok(())
    }
}

pub struct Dk3Sound {
    cpus: [Rp2a03; 2],
    /// 7D80 bit 0: low holds both CPUs in reset
    running: bool,
    /// fractional cycle count, in 1/LINE_RATE units
    phase: u64,
    line_rate: u64,
    pub out: Vec<f32>,
}

#[derive(Serialize, Deserialize)]
pub struct Dk3SoundState {
    a: Rp2a03State,
    b: Rp2a03State,
    running: bool,
    phase: u64,
}

impl Dk3Sound {
    /// `rom_a` / `rom_b`: the 8 KB programs. `line_rate`: scanlines per second.
    pub fn new(rom_a: Vec<u8>, rom_b: Vec<u8>, line_rate: u64) -> Self {
        Self {
            cpus: [Rp2a03::new(rom_a), Rp2a03::new(rom_b)],
            running: true,
            phase: 0,
            line_rate,
            out: Vec::new(),
        }
    }

    /// 7D80 write.
    pub fn set_reset(&mut self, data: u8) {
        let run = data & 1 != 0;
        if run && !self.running {
            for c in self.cpus.iter_mut() { c.cpu.reset(&mut c.mem); }
        }
        self.running = run;
    }

    /// The Z80's latches 1-3 (7C00, 7C80, 7D00).
    pub fn set_latches(&mut self, l: [u8; 3]) {
        self.cpus[0].mem.latch = [l[0], l[1]];
        self.cpus[1].mem.latch = [l[2], 0];
    }

    /// Run one scanline. `vblank`: the NMI line for both CPUs.
    pub fn run_line(&mut self, vblank: bool) {
        for c in self.cpus.iter_mut() { c.mem.nmi = vblank; }
        self.phase += CLOCK as u64;
        while self.phase >= self.line_rate {
            self.phase -= self.line_rate;
            for c in self.cpus.iter_mut() { c.cycle(self.running); }
        }
    }

    /// Mix both APUs (half volume each) into interleaved stereo.
    pub fn end_frame(&mut self) {
        let a = self.cpus[0].mem.apu.take_audio_samples();
        let b = self.cpus[1].mem.apu.take_audio_samples();
        for (x, y) in a.iter().zip(b.iter()) {
            let s = 0.5 * (x + y);
            self.out.push(s);
            self.out.push(s);
        }
    }

    pub fn save(&self) -> Dk3SoundState {
        Dk3SoundState { a: self.cpus[0].save(), b: self.cpus[1].save(), running: self.running, phase: self.phase }
    }

    pub fn load(&mut self, s: Dk3SoundState) -> Result<(), String> {
        self.cpus[0].load(s.a)?;
        self.cpus[1].load(s.b)?;
        self.running = s.running;
        self.phase = s.phase;
        Ok(())
    }

    #[cfg(test)]
    pub fn ram(&self, cpu: usize) -> &[u8] { &self.cpus[cpu].mem.ram }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny program: reads 4016 and copies it to RAM 0010, turns on pulse 1 at full
    /// volume, and counts NMIs at 0011.
    fn prog() -> Vec<u8> {
        let mut rom = vec![0xEA; 0x2000];
        let code = [
            0xA9, 0x01, 0x8D, 0x15, 0x40,   // LDA #1; STA 4015 (pulse 1 on)
            0xA9, 0xBF, 0x8D, 0x00, 0x40,   // LDA #BF; STA 4000 (duty 2, constant vol 15)
            0xA9, 0x80, 0x8D, 0x02, 0x40,   // LDA #80; STA 4002
            0xA9, 0x08, 0x8D, 0x03, 0x40,   // LDA #08; STA 4003
            0xAD, 0x16, 0x40,               // loop: LDA 4016
            0x85, 0x10,                     // STA 10
            0x4C, 0x14, 0x80,               // JMP loop
        ];
        rom[..code.len()].copy_from_slice(&code);
        rom[0x100..0x103].copy_from_slice(&[0xE6, 0x11, 0x40]); // nmi: INC 11; RTI
        rom[0x1FFA..0x2000].copy_from_slice(&[0x00, 0x81, 0x00, 0x80, 0x00, 0x80]);
        rom
    }

    #[test]
    fn latches_nmi_and_sound() {
        let mut s = Dk3Sound::new(prog(), prog(), 16000);
        s.set_latches([0x5A, 0, 0xA5]);
        for f in 0..3 {
            for line in 0..264 { s.run_line(line >= 240); }
            s.end_frame();
            let _ = f;
        }
        assert_eq!(s.ram(0)[0x10], 0x5A);
        assert_eq!(s.ram(1)[0x10], 0xA5);
        assert_eq!(s.ram(0)[0x11], 3, "one NMI per frame");
        // the 4000-4017 writes also landed in RAM
        assert_eq!(s.ram(0)[0x0000], 0xBF);
        let peak = s.out.iter().fold(0f32, |m, &v| m.max(v.abs()));
        assert!(peak > 0.05, "a tone comes out ({peak})");
        assert!((700..760).contains(&(s.out.len() / 2 / 3)), "~727 samples a frame");

        // held in reset: nothing runs
        s.set_reset(0);
        s.set_latches([0x11, 0, 0]);
        for line in 0..264 { s.run_line(line >= 240); }
        assert_eq!(s.ram(0)[0x10], 0x5A);
        s.set_reset(1);
        for line in 0..264 { s.run_line(line >= 240); }
        assert_eq!(s.ram(0)[0x10], 0x11);
    }
}
