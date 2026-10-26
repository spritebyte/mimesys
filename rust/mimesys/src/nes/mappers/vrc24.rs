use crate::nes::mappers::{Mapper, Mirroring, mirror_nt, encode_state, decode_state};
use serde::{Serialize, Deserialize};

// Konami VRC2 / VRC4: mappers 21, 22, 23, 25.
//
// The chips are the same family; the boards wire different CPU address lines to the
// chip's register-select pins A0/A1, which is what the mapper numbers and submappers say:
//
//   mapper sub  chip    A0      A1      games
//   21     1    VRC4a   A1      A2      Wai Wai World 2
//   21     2    VRC4c   A6      A7      Ganbare Goemon Gaiden 2
//   22     -    VRC2a   A1      A0      TwinBee 3 (CHR banks are in 2K steps: value >> 1)
//   23     1    VRC4f   A0      A1
//   23     2    VRC4e   A2      A3      Parodius Da!
//   23     3    VRC2b   A0      A1      Contra (J), Wai Wai World, Kid Dracula
//   25     1    VRC4b   A1      A0      Gradius II
//   25     2    VRC4d   A3      A2
//   25     3    VRC2c   A1      A0
// With no submapper (plain iNES) both wirings are ORed together, which works for every
// game that only uses one of them.
//
// Registers (after decoding A0/A1 to sub-register 0-3):
//   $8000      PRG bank 0 (8K)          $A000  PRG bank 1 (8K at $A000)
//   $9000/1    mirroring (VRC2: bit 0 V/H; VRC4: V, H, one-screen A, one-screen B)
//   $9002/3    VRC4: bit 1 = PRG swap mode (bank 0 at $C000, second-last at $8000)
//   $B000-$E003  CHR banks 0-7, 1K each: sub 0/2 = low 4 bits, sub 1/3 = high bits
//   $F000/1    VRC4 IRQ latch low/high nibble
//   $F002      VRC4 IRQ control: bit 0 A (enable after ack), bit 1 E (enable), bit 2 M (cycle mode)
//   $F003      VRC4 IRQ acknowledge (E = A)
// The VRC4 IRQ counts scanlines with a CPU-cycle prescaler (341 PPU dots = 113.67 cycles),
// or every CPU cycle in cycle mode, and fires when it overflows from $FF (then reloads).

#[derive(Clone, Copy, PartialEq, Debug)]
enum Chip { Vrc2, Vrc4 }

pub struct Vrc24 {
    chip: Chip,
    a0: u16,            // CPU address bits that act as the chip's A0
    a1: u16,            // ... and A1
    chr_shift: u8,      // VRC2a drops the low CHR bit
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_ram: Vec<u8>,
    s: State,
}

#[derive(Serialize, Deserialize, Clone)]
struct State {
    prg: [u8; 2],
    swap: bool,
    chr: [u16; 8],
    mirroring: u8,
    irq_latch: u8,
    irq_ctrl: u8,
    irq_counter: u8,
    prescaler: i16,
    irq_pending: bool,
    prg_ram: Vec<u8>,
    sram_dirty: bool,
}

impl Vrc24 {
    pub fn new(mapper: u16, submapper: u8, prg_rom: Vec<u8>, chr_rom: Vec<u8>, mirroring: Mirroring) -> Self {
        use Chip::*;
        let (chip, a0, a1) = match (mapper, submapper) {
            (21, 1) => (Vrc4, 0x02, 0x04),
            (21, 2) => (Vrc4, 0x40, 0x80),
            (21, _) => (Vrc4, 0x42, 0x84),
            (22, _) => (Vrc2, 0x02, 0x01),
            (23, 1) => (Vrc4, 0x01, 0x02),
            (23, 2) => (Vrc4, 0x04, 0x08),
            (23, 3) => (Vrc2, 0x01, 0x02),
            (23, _) => (Vrc4, 0x05, 0x0A),
            (25, 1) => (Vrc4, 0x02, 0x01),
            (25, 2) => (Vrc4, 0x08, 0x04),
            (25, 3) => (Vrc2, 0x02, 0x01),
            (_, _)  => (Vrc4, 0x0A, 0x05),       // 25 without submapper
        };
        let chr_ram = if chr_rom.is_empty() { vec![0; 8192] } else { vec![] };
        Self {
            chip, a0, a1, chr_shift: if mapper == 22 { 1 } else { 0 },
            prg_rom, chr_rom, chr_ram,
            s: State {
                prg: [0, 1], swap: false, chr: [0; 8],
                mirroring: if mirroring == Mirroring::Horizontal { 1 } else { 0 },
                irq_latch: 0, irq_ctrl: 0, irq_counter: 0, prescaler: 341, irq_pending: false,
                prg_ram: vec![0; 8192], sram_dirty: false,
            },
        }
    }

    fn prg_banks(&self) -> usize { (self.prg_rom.len() / 0x2000).max(2) }

    fn mirroring(&self) -> Mirroring {
        let m = if self.chip == Chip::Vrc2 { self.s.mirroring & 1 } else { self.s.mirroring & 3 };
        match m {
            0 => Mirroring::Vertical,
            1 => Mirroring::Horizontal,
            2 => Mirroring::SingleLower,
            _ => Mirroring::SingleUpper,
        }
    }

    fn clock_irq(&mut self) {
        if self.s.irq_counter == 0xFF {
            self.s.irq_counter = self.s.irq_latch;
            self.s.irq_pending = true;
        } else {
            self.s.irq_counter += 1;
        }
    }
}

impl Mapper for Vrc24 {
    fn cpu_read(&self, addr: u16) -> u8 {
        match addr {
            0x6000..=0x7FFF => self.s.prg_ram[addr as usize - 0x6000],
            0x8000..=0xFFFF => {
                let n = self.prg_banks();
                let slot = (addr as usize - 0x8000) / 0x2000;
                let bank = match (slot, self.s.swap) {
                    (0, false) | (2, true) => self.s.prg[0] as usize,
                    (0, true) | (2, false) => n - 2,
                    (1, _) => self.s.prg[1] as usize,
                    _ => n - 1,
                } % n;
                self.prg_rom[bank * 0x2000 + (addr as usize & 0x1FFF)]
            }
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, value: u8) {
        if (0x6000..=0x7FFF).contains(&addr) {
            self.s.prg_ram[addr as usize - 0x6000] = value;
            self.s.sram_dirty = true;
            return;
        }
        if addr < 0x8000 { return; }
        let sub = (if addr & self.a0 != 0 { 1 } else { 0 }) | (if addr & self.a1 != 0 { 2 } else { 0 });
        let vrc4 = self.chip == Chip::Vrc4;
        match addr & 0xF000 {
            0x8000 => self.s.prg[0] = value & 0x1F,
            0xA000 => self.s.prg[1] = value & 0x1F,
            0x9000 => {
                if !vrc4 || sub < 2 {
                    self.s.mirroring = value & 3;
                } else if sub == 2 {
                    self.s.swap = value & 0x02 != 0;
                }
            }
            0xB000..=0xE000 => {
                let idx = (((addr & 0xF000) - 0xB000) >> 12) as usize * 2 + (sub >> 1);
                let c = &mut self.s.chr[idx];
                if sub & 1 == 0 {
                    *c = (*c & 0x1F0) | (value & 0x0F) as u16;
                } else {
                    let high = if vrc4 { value & 0x1F } else { value & 0x0F };
                    *c = (*c & 0x00F) | ((high as u16) << 4);
                }
            }
            0xF000 if vrc4 => match sub {
                0 => self.s.irq_latch = (self.s.irq_latch & 0xF0) | (value & 0x0F),
                1 => self.s.irq_latch = (self.s.irq_latch & 0x0F) | (value << 4),
                2 => {
                    self.s.irq_ctrl = value & 0x07;
                    self.s.irq_pending = false;
                    if value & 0x02 != 0 {
                        self.s.irq_counter = self.s.irq_latch;
                        self.s.prescaler = 341;
                    }
                }
                _ => {
                    self.s.irq_pending = false;
                    let a = self.s.irq_ctrl & 1;
                    self.s.irq_ctrl = (self.s.irq_ctrl & !0x02) | (a << 1);
                }
            },
            _ => {}
        }
    }

    fn step_cycles(&mut self, cycles: u64) {
        if self.chip != Chip::Vrc4 { return; }
        for _ in 0..cycles {
            if self.s.irq_ctrl & 0x02 == 0 { return; }
            if self.s.irq_ctrl & 0x04 != 0 {
                self.clock_irq();
            } else {
                self.s.prescaler -= 3;
                if self.s.prescaler <= 0 {
                    self.s.prescaler += 341;
                    self.clock_irq();
                }
            }
        }
    }

    fn is_irq_asserted(&self) -> bool { self.s.irq_pending }

    fn ppu_read(&self, addr: u16) -> u8 {
        let addr = addr as usize & 0x1FFF;
        if self.chr_rom.is_empty() { return self.chr_ram[addr]; }
        let bank = (self.s.chr[addr / 0x400] >> self.chr_shift) as usize;
        self.chr_rom[(bank * 0x400 + (addr & 0x3FF)) % self.chr_rom.len()]
    }
    fn ppu_write(&mut self, addr: u16, value: u8) {
        if self.chr_rom.is_empty() && addr < 0x2000 { self.chr_ram[addr as usize] = value; }
    }
    fn mirror_vram_address(&self, addr: u16) -> usize { mirror_nt(self.mirroring(), addr) }

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

#[cfg(test)]
mod tests {
    use super::*;

    fn rom(banks_8k: usize) -> Vec<u8> {
        let mut r = vec![0u8; banks_8k * 0x2000];
        for b in 0..banks_8k { r[b * 0x2000] = b as u8; }   // first byte of each bank = its number
        r
    }

    #[test]
    fn prg_banking_and_swap() {
        // Gradius II wiring: mapper 25 sub 1 (A0 = CPU A1, A1 = CPU A0)
        let mut m = Vrc24::new(25, 1, rom(16), vec![0; 0x2000], Mirroring::Vertical);
        m.cpu_write(0x8000, 5);
        m.cpu_write(0xA000, 7);
        assert_eq!((m.cpu_read(0x8000), m.cpu_read(0xA000), m.cpu_read(0xC000), m.cpu_read(0xE000)), (5, 7, 14, 15));
        m.cpu_write(0x9001, 0x02);   // CPU A0 = chip A1 -> sub 2 = swap mode
        assert_eq!((m.cpu_read(0x8000), m.cpu_read(0xC000)), (14, 5));
    }

    #[test]
    fn chr_nibbles_and_vrc2a_shift() {
        let mut chr = vec![0u8; 256 * 0x400];
        for b in 0..256 { chr[b * 0x400] = b as u8; }
        let mut m = Vrc24::new(23, 3, rom(8), chr.clone(), Mirroring::Vertical);   // VRC2b
        m.cpu_write(0xB000, 0x0A);   // bank 0 low
        m.cpu_write(0xB001, 0x01);   // bank 0 high -> $1A
        m.cpu_write(0xB002, 0x03);   // bank 1 low
        assert_eq!((m.ppu_read(0x0000), m.ppu_read(0x0400)), (0x1A, 0x03));
        let mut m = Vrc24::new(22, 0, rom(8), chr, Mirroring::Vertical);           // VRC2a
        m.cpu_write(0xB000, 0x08);
        assert_eq!(m.ppu_read(0x0000), 0x04);
    }

    #[test]
    fn vrc4_scanline_irq() {
        // VRC4a: chip A0 = CPU A1, chip A1 = CPU A2, so $F000/$F002/$F004/$F006 = sub 0-3
        let mut m = Vrc24::new(21, 1, rom(8), vec![0; 0x2000], Mirroring::Vertical);
        m.cpu_write(0xF000, 0x0E);   // latch low nibble
        m.cpu_write(0xF002, 0x0F);   // latch high nibble -> $FE
        m.cpu_write(0xF004, 0x02);   // control: enable, scanline mode -> counter = $FE
        m.step_cycles(114);          // ~1 scanline: $FE -> $FF
        assert!(!m.is_irq_asserted());
        m.step_cycles(114);          // overflow -> IRQ, reload
        assert!(m.is_irq_asserted());
        m.cpu_write(0xF006, 0);      // ack
        assert!(!m.is_irq_asserted());
    }
}
