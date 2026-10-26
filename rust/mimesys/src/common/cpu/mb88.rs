// Fujitsu MB88xx 4-bit MCU (MB8843/MB8844 as used in Namco's 50xx-54xx custom chips).
// Port of MAME's mb88xx.cpp (Ernesto Corvi, BSD-3-Clause), same flag rules and timing.
//
// Registers: PC (6 bits) + PA (page, 5 bits) = 11-bit program address, A/X/Y (4 bits),
// flags ST (status, set by most instructions, tested by branches), ZF, CF, VF (timer
// overflow), SF (serial full), IF (IRQ pin). 4-level stack. RAM addressed by X:Y.
// One instruction cycle = 6 clocks; instructions take 1 or 2 cycles.
// Ports: K (4-bit in), O (8-bit out via the "PLA"), P (4-bit out), R0-R3 (4-bit I/O).
// Interrupts (enabled by bits of the PIO register): external IRQ pin (rising), timer
// overflow, serial. Vectors 0x02 / 0x04 / 0x06 on page 0.

pub trait Mb88Io {
    fn read_k(&mut self) -> u8;
    fn write_o(&mut self, data: u8, mask: u8);
    fn write_p(&mut self, data: u8);
    fn read_r(&mut self, port: usize) -> u8;
    fn write_r(&mut self, _port: usize, _data: u8) {}
    fn read_si(&mut self) -> bool { false }
}

const TIMER_PRESCALE: u8 = 32;
const SERIAL_DISABLE_THRESH: u16 = 1000;
const INT_SERIAL: u8 = 0x01;
const INT_TIMER: u8 = 0x02;
const INT_EXTERNAL: u8 = 0x04;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Mb88 {
    #[serde(skip)]
    rom: Vec<u8>,
    #[serde(with = "serde_bytes")]
    ram: Vec<u8>,
    pc: u8,
    pa: u8,
    sp: [u16; 4],
    si: u8,
    a: u8,
    x: u8,
    y: u8,
    st: u8,
    zf: u8,
    cf: u8,
    vf: u8,
    sf: u8,
    if_: u8,
    pio: u8,
    th: u8,
    tl: u8,
    tp: u8,
    ctr: u8,
    sb: u8,
    sb_count: u16,
    o_output: u8,
    pending_irq: u8,
    in_irq: bool,
    /// held in reset by the board
    pub held: bool,
    rom_mask: u16,
}

impl Mb88 {
    /// `rom`: program (1 KB for the MB8843/44). `ram_size`: 64 nibbles for those.
    pub fn new(rom: Vec<u8>, ram_size: usize) -> Self {
        let rom_mask = (rom.len().next_power_of_two().max(1) - 1) as u16;
        let mut m = Self {
            rom, ram: vec![0; ram_size], pc: 0, pa: 0, sp: [0; 4], si: 0, a: 0, x: 0, y: 0,
            st: 1, zf: 0, cf: 0, vf: 0, sf: 0, if_: 0, pio: 0, th: 0, tl: 0, tp: 0, ctr: 0,
            sb: 0, sb_count: 0, o_output: 0, pending_irq: 0, in_irq: false, held: false, rom_mask,
        };
        m.reset();
        m
    }

    /// After loading a state: take the program ROM back.
    pub fn keep_rom(&mut self, old: &mut Self) { self.rom = std::mem::take(&mut old.rom); }

    pub fn reset(&mut self) {
        self.pc = 0; self.pa = 0; self.sp = [0; 4]; self.si = 0;
        self.a = 0; self.x = 0; self.y = 0;
        self.st = 1; self.zf = 0; self.cf = 0; self.vf = 0; self.sf = 0;
        self.pio = 0; self.th = 0; self.tl = 0; self.tp = 0;
        self.sb = 0; self.sb_count = 0; self.pending_irq = 0; self.in_irq = false;
    }

    /// Reset input (true = held in reset). Released: starts from address 0.
    pub fn set_reset(&mut self, held: bool) {
        if held { self.reset(); }
        self.held = held;
    }

    /// IRQ pin (logical level): a rising edge requests the external interrupt.
    pub fn set_irq(&mut self, state: bool) {
        let s = state as u8;
        if self.if_ == 0 && s != 0 && self.pio & INT_EXTERNAL != 0 {
            self.pending_irq |= INT_EXTERNAL;
        }
        self.if_ = s;
    }

    /// TC pin: the timer counts on falling edges when PIO bit 6 is set.
    pub fn set_tc(&mut self, state: bool) {
        let s = state as u8;
        if self.ctr != 0 && s == 0 && self.pio & 0x40 != 0 { self.increment_timer(); }
        self.ctr = s;
    }

    #[inline] fn getpc(&self) -> u16 { ((self.pa as u16) << 6) + self.pc as u16 }
    #[inline] fn getea(&self) -> usize { ((self.x as usize) << 4) + self.y as usize }
    #[inline] fn readop(&self, a: u16) -> u8 { self.rom.get((a & self.rom_mask) as usize).copied().unwrap_or(0) }
    #[inline] fn incpc(&mut self) {
        self.pc += 1;
        if self.pc >= 0x40 { self.pc = 0; self.pa = self.pa.wrapping_add(1); }
    }
    #[inline] fn rdmem(&self, a: usize) -> u8 { self.ram.get(a).copied().unwrap_or(0) & 0x0F }
    #[inline] fn wrmem(&mut self, a: usize, v: u8) { if let Some(m) = self.ram.get_mut(a) { *m = v & 0x0F; } }
    #[inline] fn st_c(&mut self, v: u8) { self.st = if v & 0x10 != 0 { 0 } else { 1 }; }
    #[inline] fn st_z(&mut self, v: u8) { self.st = if v == 0 { 0 } else { 1 }; }
    #[inline] fn upd_cf(&mut self, v: u8) { self.cf = if v & 0x10 == 0 { 0 } else { 1 }; }
    #[inline] fn upd_zf(&mut self, v: u8) { self.zf = if v != 0 { 0 } else { 1 }; }

    fn write_pla(&mut self, io: &mut dyn Mb88Io, index: u8) {
        // 8-bit "PLA" mode (no PLA data): CF selects the nibble
        let shift = if index & 0x10 != 0 { 4 } else { 0 };
        let mask = 0x0Fu8 << shift;
        self.o_output = (self.o_output & !mask) | ((index << shift) & mask);
        io.write_o(self.o_output, mask);
    }

    fn increment_timer(&mut self) {
        self.tl = (self.tl + 1) & 0x0F;
        if self.tl == 0 {
            self.th = (self.th + 1) & 0x0F;
            if self.th == 0 {
                self.vf = 1;
                self.pending_irq |= INT_TIMER;
            }
        }
    }

    fn serial_tick(&mut self, io: &mut dyn Mb88Io) {
        if self.sb_count >= SERIAL_DISABLE_THRESH { return; }
        self.sb_count += 1;
        if self.sf == 0 {
            self.sb = (self.sb >> 1) | if io.read_si() { 8 } else { 0 };
            if self.sb_count >= 4 {
                self.sf = 1;
                self.pending_irq |= INT_SERIAL;
            }
        }
    }

    /// Account for cycles: timer, serial, then take a pending interrupt. Returns cycles used.
    fn burn_cycles(&mut self, io: &mut dyn Mb88Io, cycles: u32) -> u32 {
        let mut used = cycles;
        if self.pio & 0x80 != 0 {
            self.tp += cycles as u8;
            while self.tp >= TIMER_PRESCALE {
                self.tp -= TIMER_PRESCALE;
                self.increment_timer();
            }
        }
        if self.pio & 0x30 == 0x20 {
            for _ in 0..cycles { self.serial_tick(io); }
        }
        if !self.in_irq && self.pending_irq & self.pio != 0 {
            self.in_irq = true;
            let intpc = self.getpc();
            self.sp[self.si as usize] = intpc
                | ((self.cf as u16 & 1) << 15) | ((self.zf as u16 & 1) << 14) | ((self.st as u16 & 1) << 13);
            self.si = (self.si + 1) & 3;
            let p = self.pending_irq & self.pio;
            self.pc = if p & INT_EXTERNAL != 0 { 0x02 } else if p & INT_TIMER != 0 { 0x04 } else { 0x06 };
            self.pa = 0;
            self.st = 1;
            self.pending_irq = 0;
            used += self.burn_cycles(io, 3);
        }
        used
    }

    /// Run one instruction. Returns instruction cycles (1 cycle = 6 clocks).
    pub fn step(&mut self, io: &mut dyn Mb88Io) -> u32 {
        if self.held { return 1; }
        let opcode = self.readop(self.getpc());
        self.incpc();
        let mut oc = 1;
        match opcode {
            0x00 => self.st = 1,
            0x01 => { let i = (self.cf << 4) | self.a; self.write_pla(io, i); self.st = 1; }
            0x02 => { io.write_p(self.a); self.st = 1; }
            0x03 => { io.write_r((self.y & 3) as usize, self.a); self.st = 1; }
            0x04 => { self.y = self.a; self.st = 1; }
            0x05 => { self.th = self.a; self.st = 1; }
            0x06 => { self.tl = self.a; self.st = 1; }
            0x07 => { self.sb = self.a; self.st = 1; }
            0x08 => { self.y += 1; let y = self.y; self.st_c(y); self.y &= 0x0F; let y = self.y; self.upd_zf(y); }
            0x09 => {
                let ea = self.getea();
                let arg = self.rdmem(ea) + 1;
                self.st_c(arg);
                let arg = arg & 0x0F;
                self.upd_zf(arg);
                self.wrmem(ea, arg);
            }
            0x0A => {
                let ea = self.getea();
                self.wrmem(ea, self.a);
                self.y += 1; let y = self.y; self.st_c(y); self.y &= 0x0F; let y = self.y; self.upd_zf(y);
            }
            0x0B => {
                let ea = self.getea();
                let arg = self.rdmem(ea);
                self.wrmem(ea, self.a);
                self.a = arg;
                self.upd_zf(arg);
                self.st = 1;
            }
            0x0C => {
                self.a = (self.a << 1) | self.cf;
                let a = self.a; self.st_c(a);
                self.cf = self.st ^ 1;
                self.a &= 0x0F;
                let a = self.a; self.upd_zf(a);
            }
            0x0D => { let v = self.rdmem(self.getea()); self.a = v; self.upd_zf(v); self.st = 1; }
            0x0E => {
                let arg = self.rdmem(self.getea()) + self.a + self.cf;
                self.st_c(arg);
                self.cf = self.st ^ 1;
                self.a = arg & 0x0F;
                let a = self.a; self.upd_zf(a);
            }
            0x0F => {
                self.a &= self.rdmem(self.getea());
                let a = self.a; self.upd_zf(a);
                self.st = self.zf ^ 1;
            }
            0x10 => {
                if self.cf != 0 || self.a > 9 { self.a += 6; }
                let a = self.a; self.st_c(a);
                self.cf = self.st ^ 1;
                self.a &= 0x0F;
            }
            0x11 => {
                if self.cf != 0 || self.a > 9 { self.a += 10; }
                let a = self.a; self.st_c(a);
                self.cf = self.st ^ 1;
                self.a &= 0x0F;
            }
            0x12 => { let v = io.read_k() & 0x0F; self.a = v; self.upd_zf(v); self.st = 1; }
            0x13 => { let v = io.read_r((self.y & 3) as usize) & 0x0F; self.a = v; self.upd_zf(v); self.st = 1; }
            0x14 => { self.a = self.y; let a = self.a; self.upd_zf(a); self.st = 1; }
            0x15 => { self.a = self.th; let a = self.a; self.upd_zf(a); self.st = 1; }
            0x16 => { self.a = self.tl; let a = self.a; self.upd_zf(a); self.st = 1; }
            0x17 => { self.a = self.sb; let a = self.a; self.upd_zf(a); self.st = 1; }
            0x18 => { self.y = self.y.wrapping_sub(1); let y = self.y; self.st_c(y); self.y &= 0x0F; }
            0x19 => {
                let ea = self.getea();
                let arg = self.rdmem(ea).wrapping_sub(1);
                self.st_c(arg);
                let arg = arg & 0x0F;
                self.upd_zf(arg);
                self.wrmem(ea, arg);
            }
            0x1A => {
                let ea = self.getea();
                self.wrmem(ea, self.a);
                self.y = self.y.wrapping_sub(1); let y = self.y; self.st_c(y); self.y &= 0x0F; let y = self.y; self.upd_zf(y);
            }
            0x1B => { let t = self.x; self.x = self.a; self.a = t; self.upd_zf(t); self.st = 1; }
            0x1C => {
                self.a |= self.cf << 4;
                let v = self.a << 4; self.st_c(v);
                self.cf = self.st ^ 1;
                self.a = (self.a >> 1) & 0x0F;
                let a = self.a; self.upd_zf(a);
            }
            0x1D => { let ea = self.getea(); self.wrmem(ea, self.a); self.st = 1; }
            0x1E => {
                let arg = self.rdmem(self.getea()).wrapping_sub(self.a).wrapping_sub(self.cf);
                self.st_c(arg);
                self.cf = self.st ^ 1;
                self.a = arg & 0x0F;
                let a = self.a; self.upd_zf(a);
            }
            0x1F => {
                self.a |= self.rdmem(self.getea());
                let a = self.a; self.upd_zf(a);
                self.st = self.zf ^ 1;
            }
            0x20 => {
                let p = (self.y >> 2) as usize;
                let arg = io.read_r(p) & 0x0F;
                io.write_r(p, arg | (1 << (self.y & 3)));
                self.st = 1;
            }
            0x21 => { self.cf = 1; self.st = 1; }
            0x22 => {
                let p = (self.y >> 2) as usize;
                let arg = io.read_r(p) & 0x0F;
                io.write_r(p, arg & !(1 << (self.y & 3)));
                self.st = 1;
            }
            0x23 => { self.cf = 0; self.st = 1; }
            0x24 => {
                let arg = io.read_r((self.y >> 2) as usize) & 0x0F;
                self.st = if arg & (1 << (self.y & 3)) != 0 { 0 } else { 1 };
            }
            0x25 => self.st = self.if_ ^ 1,
            0x26 => { self.st = self.vf ^ 1; self.vf = 0; }
            0x27 => {
                self.st = self.sf ^ 1;
                if self.sf != 0 { self.sb_count = 0; }
                self.sf = 0;
            }
            0x28 => self.st = self.cf ^ 1,
            0x29 => self.st = self.zf ^ 1,
            0x2A => { let ea = self.getea(); self.wrmem(ea, self.sb); let s = self.sb; self.upd_zf(s); self.st = 1; }
            0x2B => { let v = self.rdmem(self.getea()); self.sb = v; self.upd_zf(v); self.st = 1; }
            0x2C => {
                self.si = self.si.wrapping_sub(1) & 3;
                let v = self.sp[self.si as usize];
                self.pc = (v & 0x3F) as u8;
                self.pa = ((v >> 6) & 0x1F) as u8;
                self.st = 1;
            }
            0x2D => { self.a = (!self.a).wrapping_add(1) & 0x0F; let a = self.a; self.st_z(a); }
            0x2E => {
                let arg = self.rdmem(self.getea()).wrapping_sub(self.a);
                self.upd_cf(arg);
                let arg = arg & 0x0F;
                self.st_z(arg);
                self.zf = self.st ^ 1;
            }
            0x2F => {
                self.a ^= self.rdmem(self.getea());
                let a = self.a; self.st_z(a);
                self.zf = self.st ^ 1;
            }
            0x30..=0x33 => { let ea = self.getea(); let v = self.rdmem(ea); self.wrmem(ea, v | (1 << (opcode & 3))); self.st = 1; }
            0x34..=0x37 => { let ea = self.getea(); let v = self.rdmem(ea); self.wrmem(ea, v & !(1 << (opcode & 3))); self.st = 1; }
            0x38..=0x3B => { let v = self.rdmem(self.getea()); self.st = if v & (1 << (opcode & 3)) != 0 { 0 } else { 1 }; }
            0x3C => {
                self.in_irq = false;
                self.si = self.si.wrapping_sub(1) & 3;
                let v = self.sp[self.si as usize];
                self.pc = (v & 0x3F) as u8;
                self.pa = ((v >> 6) & 0x1F) as u8;
                self.st = ((v >> 13) & 1) as u8;
                self.zf = ((v >> 14) & 1) as u8;
                self.cf = ((v >> 15) & 1) as u8;
            }
            0x3D => {
                self.pa = self.readop(self.getpc()) & 0x1F;
                self.pc = self.a * 4;
                oc += 1;
                self.st = 1;
            }
            0x3E => { let v = self.readop(self.getpc()); self.pio |= v; self.incpc(); oc += 1; self.st = 1; }
            0x3F => { let v = self.readop(self.getpc()); self.pio &= !v; self.incpc(); oc += 1; self.st = 1; }
            0x40..=0x43 => { let v = (io.read_r(0) & 0x0F) | (1 << (opcode & 3)); io.write_r(0, v); self.st = 1; }
            0x44..=0x47 => { let v = (io.read_r(0) & 0x0F) & !(1 << (opcode & 3)); io.write_r(0, v); self.st = 1; }
            0x48..=0x4B => { let v = io.read_r(2) & 0x0F; self.st = if v & (1 << (opcode & 3)) != 0 { 0 } else { 1 }; }
            0x4C..=0x4F => self.st = if self.a & (1 << (opcode & 3)) != 0 { 0 } else { 1 },
            0x50..=0x53 => {
                let a = (opcode & 3) as usize;
                let v = self.rdmem(a);
                self.wrmem(a, self.a);
                self.a = v;
                self.upd_zf(v);
                self.st = 1;
            }
            0x54..=0x57 => {
                let a = (opcode & 3) as usize + 4;
                let v = self.rdmem(a);
                self.wrmem(a, self.y);
                self.y = v;
                self.upd_zf(v);
                self.st = 1;
            }
            0x58..=0x5F => { self.x = opcode & 7; let x = self.x; self.upd_zf(x); self.st = 1; }
            0x60..=0x67 => {
                let arg = self.readop(self.getpc());
                self.incpc();
                oc += 1;
                if self.st & 1 != 0 {
                    self.sp[self.si as usize] = self.getpc();
                    self.si = (self.si + 1) & 3;
                    self.pc = arg & 0x3F;
                    self.pa = ((opcode & 7) << 2) | (arg >> 6);
                }
                self.st = 1;
            }
            0x68..=0x6F => {
                let arg = self.readop(self.getpc());
                self.incpc();
                oc += 1;
                if self.st & 1 != 0 {
                    self.pc = arg & 0x3F;
                    self.pa = ((opcode & 7) << 2) | (arg >> 6);
                }
                self.st = 1;
            }
            0x70..=0x7F => {
                let arg = (opcode & 0x0F) + self.a;
                self.st_c(arg);
                self.cf = self.st ^ 1;
                self.a = arg & 0x0F;
                let a = self.a; self.upd_zf(a);
            }
            0x80..=0x8F => { self.y = opcode & 0x0F; let y = self.y; self.upd_zf(y); self.st = 1; }
            0x90..=0x9F => { self.a = opcode & 0x0F; let a = self.a; self.upd_zf(a); self.st = 1; }
            0xA0..=0xAF => {
                let arg = (opcode & 0x0F).wrapping_sub(self.y);
                self.upd_cf(arg);
                let arg = arg & 0x0F;
                self.st_z(arg);
                self.zf = self.st ^ 1;
            }
            0xB0..=0xBF => {
                let arg = (opcode & 0x0F).wrapping_sub(self.a);
                self.upd_cf(arg);
                let arg = arg & 0x0F;
                self.st_z(arg);
                self.zf = self.st ^ 1;
            }
            _ => {
                if self.st & 1 != 0 { self.pc = opcode & 0x3F; }
                self.st = 1;
            }
        }
        self.burn_cycles(io, oc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Io { o: u8, p: u8, k: u8, r: [u8; 4] }
    impl Mb88Io for Io {
        fn read_k(&mut self) -> u8 { self.k }
        fn write_o(&mut self, d: u8, _m: u8) { self.o = d; }
        fn write_p(&mut self, d: u8) { self.p = d; }
        fn read_r(&mut self, p: usize) -> u8 { self.r[p] }
        fn write_r(&mut self, p: usize, d: u8) { self.r[p] = d; }
    }

    #[test]
    fn arithmetic_ports_and_irq() {
        // li 5; ai 4 (A=9); outP; inK; xx (X=K); li 0xC; rstc; outO (O low = C);
        // setc; li 3; outO (O high = 3); en 04; loop: jmp loop
        let mut rom = vec![0u8; 0x400];
        let prog = [0x95, 0x74, 0x02, 0x12, 0x1B, 0x9C, 0x23, 0x01, 0x21, 0x93, 0x01, 0x3E, 0x04, 0xDD];
        rom[0x10..0x10 + prog.len()].copy_from_slice(&prog);
        rom[0] = 0xD0;                  // jmp 0x10
        rom[2] = 0x90; rom[3] = 0x02; rom[4] = 0x3C;  // irq vector: li 0; outP; rti
        let mut m = Mb88::new(rom, 64);
        let mut io = Io { k: 7, ..Default::default() };
        for _ in 0..14 { m.step(&mut io); }
        assert_eq!(io.p, 9);
        assert_eq!(m.x, 7);
        assert_eq!(io.o, 0x3C);
        m.set_irq(true);
        for _ in 0..4 { m.step(&mut io); }
        assert_eq!(io.p, 0, "external interrupt handler ran");
        assert_eq!(m.getpc(), 0x1D, "returned to the loop");
    }
}
