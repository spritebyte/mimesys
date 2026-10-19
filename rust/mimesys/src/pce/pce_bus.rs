// PC Engine physical memory map (2 MB, 256 banks of 8 KB, picked by the CPU's MPRs):
//
//   banks 00-7F  HuCard ROM (1 MB window)
//   banks F8-FB  8 KB work RAM (F8; F9-FB mirror it on the base console)
//   bank  FF     hardware:
//                  0000-03FF VDC   0400-07FF VCE   0800-0BFF PSG   0C00-0FFF timer
//                  1000-13FF joypad   1400-17FF interrupt controller
//
// Timer: counts down from its reload value (7 bits) every 1024 CPU clocks (7.16 MHz);
//   passing zero reloads it and raises the TIMER interrupt.
// Interrupt controller: 1402 = disable mask (bit 0 IRQ2, 1 IRQ1/VDC, 2 timer),
//   1403 read = pending, write = acknowledge the timer.
// Joypad (1000): write bit 0 = SEL, bit 1 = CLR. Read bits 0-3 (0 = pressed):
//   SEL=1: up, right, down, left. SEL=0: I, II, SELECT, RUN.
//   bit 6 = region (0 = Japanese PC Engine, 1 = TurboGrafx), bit 7 = 1 (no CD unit).
//
// HuCard layouts:
//   384 KB: banks 00-3F see the first 256 KB (mirrored), 40-7F the last 128 KB (mirrored)
//   2.5 MB (Street Fighter II'): 00-3F fixed to the first 512 KB; writing to 1FF0-1FF3
//          picks which 512 KB appears at 40-7F
//   otherwise: mirrored to fill the 1 MB window
// US HuCards have their data lines wired in reverse; such dumps are detected by their
// reset vector and flipped back.

use serde::{Deserialize, Serialize};
use crate::pce::huc6280::{PceBus, IRQ1, TIMER};
use crate::pce::psg::Psg;
use crate::pce::vce::Vce;
use crate::pce::vdc::Vdc;

#[derive(Serialize, Deserialize, Clone)]
pub struct HuCard {
    #[serde(skip)]
    rom: Vec<u8>,
    pub sf2_bank: usize,
}

impl HuCard {
    pub fn new(mut rom: Vec<u8>) -> Result<(Self, bool), String> {
        if rom.len() % 8192 == 512 { rom.drain(..512); }           // copier header
        if rom.len() < 8192 { return Err(format!("HuCard image too small ({} bytes)", rom.len())); }
        // Reset vector lives at the end of bank 0 (MPR7 = 0 at power-on)
        let vector_ok = |r: &[u8]| r[0x1FFF] >= 0xE0;
        let mut us_card = false;
        if !vector_ok(&rom) && rom[0x1FFF].reverse_bits() >= 0xE0 {
            for b in rom.iter_mut() { *b = b.reverse_bits(); }
            us_card = true;
        }
        Ok((Self { rom, sf2_bank: 0 }, us_card))
    }

    pub fn load_state(&mut self, s: HuCard) { self.sf2_bank = s.sf2_bank; }
    pub fn len(&self) -> usize { self.rom.len() }

    fn offset(&self, phys: u32) -> usize {
        let bank = (phys >> 13) as usize;
        let off = (phys & 0x1FFF) as usize;
        let len = self.rom.len();
        let b = if len == 0x60000 {
            if bank < 0x40 { bank & 0x1F } else { 0x20 + (bank & 0x0F) }
        } else if len > 0x100000 {
            if bank < 0x40 { bank } else { 0x40 + self.sf2_bank * 0x40 + (bank - 0x40) }
        } else {
            bank
        };
        (b * 0x2000 + off) % len
    }

    fn read(&self, phys: u32) -> u8 { self.rom[self.offset(phys)] }

    fn write(&mut self, phys: u32, _v: u8) {
        if self.rom.len() > 0x100000 && (0x1FF0..=0x1FF3).contains(&phys) {
            self.sf2_bank = (phys & 3) as usize;
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct PceIo {
    #[serde(with = "serde_bytes")]
    pub ram: Vec<u8>,
    timer_reload: u8,
    timer_counter: u8,
    timer_enabled: bool,
    timer_sub: u32,
    timer_irq: bool,
    irq_disable: u8,
    pad_sel: bool,
    pad_clr: bool,
    io_buffer: u8,
}

pub struct PceBusImpl {
    pub card: HuCard,
    pub io: PceIo,
    pub vdc: Vdc,
    pub vce: Vce,
    pub psg: Psg,
    pub pad: u8,          // buttons, 1 = pressed: 0 up 1 right 2 down 3 left 4 I 5 II 6 SELECT 7 RUN
    pub region_us: bool,
}

impl PceBusImpl {
    pub fn new(card: HuCard, region_us: bool, out_rate: u32) -> Self {
        Self {
            card,
            io: PceIo {
                ram: vec![0; 0x2000],
                timer_reload: 0, timer_counter: 0, timer_enabled: false, timer_sub: 0, timer_irq: false,
                irq_disable: 0, pad_sel: false, pad_clr: false, io_buffer: 0xFF,
            },
            vdc: Vdc::new(),
            vce: Vce::new(),
            psg: Psg::new(out_rate),
            pad: 0,
            region_us,
        }
    }

    /// Advance the timer and the PSG by `cycles` of the 7.16 MHz clock.
    pub fn tick(&mut self, cycles: u32) {
        if self.io.timer_enabled {
            let mut c = cycles;
            while c > 0 {
                let step = c.min(self.io.timer_sub);
                self.io.timer_sub -= step;
                c -= step;
                if self.io.timer_sub == 0 {
                    self.io.timer_sub = 1024;
                    if self.io.timer_counter == 0 {
                        self.io.timer_counter = self.io.timer_reload;
                        self.io.timer_irq = true;
                    } else {
                        self.io.timer_counter -= 1;
                    }
                }
            }
        }
        self.psg.run(cycles);
    }

    fn pad_read(&self) -> u8 {
        let nibble = if self.io.pad_clr {
            0
        } else if self.io.pad_sel {
            !(self.pad & 0x0F) & 0x0F
        } else {
            !(self.pad >> 4) & 0x0F
        };
        0x80 | if self.region_us { 0x40 } else { 0 } | 0x30 | nibble
    }

    fn hw_read(&mut self, off: u32) -> u8 {
        match off & 0x1C00 {
            0x0000 => self.vdc.read(off),
            0x0400 => self.vce.read(off),
            0x0800 => self.io.io_buffer,                                     // PSG is write-only
            0x0C00 => { self.io.io_buffer = (self.io.io_buffer & 0x80) | self.io.timer_counter; self.io.io_buffer }
            0x1000 => { self.io.io_buffer = self.pad_read(); self.io.io_buffer }
            0x1400 => {
                let v = match off & 3 {
                    2 => self.io.irq_disable,
                    3 => (if self.vdc.irq_line() { IRQ1 } else { 0 }) | (if self.io.timer_irq { TIMER } else { 0 }),
                    _ => 0,
                };
                self.io.io_buffer = (self.io.io_buffer & 0xF8) | (v & 0x07);
                self.io.io_buffer
            }
            _ => 0xFF,
        }
    }

    fn hw_write(&mut self, off: u32, v: u8) {
        match off & 0x1C00 {
            0x0000 => self.vdc.write(off, v),
            0x0400 => self.vce.write(off, v),
            0x0800 => { self.io.io_buffer = v; self.psg.write(off, v) }
            0x0C00 => {
                self.io.io_buffer = v;
                if off & 1 == 0 {
                    self.io.timer_reload = v & 0x7F;
                } else {
                    let on = v & 1 != 0;
                    if on && !self.io.timer_enabled {
                        self.io.timer_counter = self.io.timer_reload;
                        self.io.timer_sub = 1024;
                    }
                    self.io.timer_enabled = on;
                }
            }
            0x1000 => {
                self.io.io_buffer = v;
                self.io.pad_sel = v & 1 != 0;
                self.io.pad_clr = v & 2 != 0;
            }
            0x1400 => {
                self.io.io_buffer = v;
                match off & 3 {
                    2 => self.io.irq_disable = v & 0x07,
                    3 => self.io.timer_irq = false,
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

impl PceBus for PceBusImpl {
    fn read(&mut self, phys: u32) -> u8 {
        match phys >> 13 {
            0x00..=0x7F => self.card.read(phys),
            0xF8..=0xFB => self.io.ram[(phys & 0x1FFF) as usize],
            0xFF => self.hw_read(phys & 0x1FFF),
            _ => 0xFF,
        }
    }

    fn write(&mut self, phys: u32, v: u8) {
        match phys >> 13 {
            0x00..=0x7F => self.card.write(phys, v),
            0xF8..=0xFB => self.io.ram[(phys & 0x1FFF) as usize] = v,
            0xFF => self.hw_write(phys & 0x1FFF, v),
            _ => {}
        }
    }

    fn irq_pending(&mut self) -> u8 {
        let mut lines = 0;
        if self.vdc.irq_line() { lines |= IRQ1; }
        if self.io.timer_irq { lines |= TIMER; }
        lines & !self.io.irq_disable
    }
}
