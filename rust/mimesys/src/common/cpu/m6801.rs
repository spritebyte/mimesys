// Motorola 6801/6803: the 6800 instruction set plus D-register ops (LDD/STD/ADDD/SUBD,
// LSRD/ASLD, MUL, ABX, PSHX/PULX, BRN, JSR direct), with on-chip I/O at 0x00-0x1F and
// 128 bytes of RAM at 0x80-0xFF. Port of MAME's m6800/m6801 (Aaron Giles, BSD-3-Clause):
// same flag macros, same cycle table (cycles_6803), same timer flag rules.
//
// Emulated on-chip parts: ports 1 and 2 (with DDRs), the 16-bit free-running counter with
// output compare (OCI) and overflow (TOI) interrupts. Not emulated: serial (SCI), input
// capture, port 3 handshake, RAM control.
//
// Interrupt vectors: FFFE reset, FFFC NMI, FFFA SWI, FFF8 IRQ1, FFF6 ICI, FFF4 OCI,
// FFF2 TOI, FFF0 SCI.
//
// `Model::Hd63701y0` (Hitachi HD63701Y0, MAME hd63701y0): the HD6301 instruction set
// (AIM/OIM/EIM/TIM, XGDX, SLP, illegal opcodes TRAP to FFEE) with MAME's cycles_63701,
// I/O at 0x00-0x27 (plus port 6 at 0x16/0x17, reported as port 5 to the bus) and
// 256 bytes of RAM at 0x40-0x13F. The on-chip ROM at C000-FFFF comes from the bus.

pub trait M6801Bus {
    fn read(&mut self, addr: u16) -> u8;
    fn write(&mut self, addr: u16, value: u8);
    /// Port 1/2 inputs (port = 0 or 1).
    fn port_read(&mut self, _port: usize) -> u8 { 0xFF }
    /// Port output: the pin levels (inputs pulled high) and the DDR.
    /// HD63701Y0 port 6 is port 5 here.
    fn port_write(&mut self, _port: usize, _data: u8, _ddr: u8) {}
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum Model { M6803, Hd63701y0 }

const C: u8 = 0x01;
const V: u8 = 0x02;
const Z: u8 = 0x04;
const N: u8 = 0x08;
const I: u8 = 0x10;
const H: u8 = 0x20;

const TCSR_OLVL: u8 = 0x01;
const TCSR_ETOI: u8 = 0x04;
const TCSR_EOCI: u8 = 0x08;
const TCSR_TOF: u8 = 0x20;
const TCSR_OCF: u8 = 0x40;

const XX: u8 = 4;
const CYCLES_6803: [u8; 256] = [
    /*       0  1  2  3  4  5  6  7  8  9  A  B  C  D  E  F */
    /*0*/ XX, 2,XX,XX, 3, 3, 2, 2, 3, 3, 2, 2, 2, 2, 2, 2,
    /*1*/  2, 2,XX,XX,XX,XX, 2, 2,XX, 2,XX, 2,XX,XX,XX,XX,
    /*2*/  3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3,
    /*3*/  3, 3, 4, 4, 3, 3, 3, 3, 5, 5, 3,10, 4,10, 9,12,
    /*4*/  2,XX,XX, 2, 2,XX, 2, 2, 2, 2, 2,XX, 2, 2,XX, 2,
    /*5*/  2,XX,XX, 2, 2,XX, 2, 2, 2, 2, 2,XX, 2, 2,XX, 2,
    /*6*/  6,XX,XX, 6, 6,XX, 6, 6, 6, 6, 6,XX, 6, 6, 3, 6,
    /*7*/  6,XX,XX, 6, 6,XX, 6, 6, 6, 6, 6,XX, 6, 6, 3, 6,
    /*8*/  2, 2, 2, 4, 2, 2, 2, 2, 2, 2, 2, 2, 4, 6, 3, 3,
    /*9*/  3, 3, 3, 5, 3, 3, 3, 3, 3, 3, 3, 3, 5, 5, 4, 4,
    /*A*/  4, 4, 4, 6, 4, 4, 4, 4, 4, 4, 4, 4, 6, 6, 5, 5,
    /*B*/  4, 4, 4, 6, 4, 4, 4, 4, 4, 4, 4, 4, 6, 6, 5, 5,
    /*C*/  2, 2, 2, 4, 2, 2, 2, 2, 2, 2, 2, 2, 3,XX, 3, 3,
    /*D*/  3, 3, 3, 5, 3, 3, 3, 3, 3, 3, 3, 3, 4, 4, 4, 4,
    /*E*/  4, 4, 4, 6, 4, 4, 4, 4, 4, 4, 4, 4, 5, 5, 5, 5,
    /*F*/  4, 4, 4, 6, 4, 4, 4, 4, 4, 4, 4, 4, 5, 5, 5, 5,
];

const CYCLES_63701: [u8; 256] = [
    /*       0  1  2  3  4  5  6  7  8  9  A  B  C  D  E  F */
    /*0*/ XX, 1,XX,XX, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    /*1*/  1, 1,XX,XX,XX,XX, 1, 1, 2, 2, 4, 1,XX,XX,XX,XX,
    /*2*/  3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3,
    /*3*/  1, 1, 3, 3, 1, 1, 4, 4, 4, 5, 1,10, 5, 7, 9,12,
    /*4*/  1,XX,XX, 1, 1,XX, 1, 1, 1, 1, 1,XX, 1, 1,XX, 1,
    /*5*/  1,XX,XX, 1, 1,XX, 1, 1, 1, 1, 1,XX, 1, 1,XX, 1,
    /*6*/  6, 7, 7, 6, 6, 7, 6, 6, 6, 6, 6, 5, 6, 4, 3, 5,
    /*7*/  6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 4, 6, 4, 3, 5,
    /*8*/  2, 2, 2, 3, 2, 2, 2,XX, 2, 2, 2, 2, 3, 5, 3,XX,
    /*9*/  3, 3, 3, 4, 3, 3, 3, 3, 3, 3, 3, 3, 4, 5, 4, 4,
    /*A*/  4, 4, 4, 5, 4, 4, 4, 4, 4, 4, 4, 4, 5, 5, 5, 5,
    /*B*/  4, 4, 4, 5, 4, 4, 4, 4, 4, 4, 4, 4, 5, 6, 5, 5,
    /*C*/  2, 2, 2, 3, 2, 2, 2,XX, 2, 2, 2, 2, 3,XX, 3,XX,
    /*D*/  3, 3, 3, 4, 3, 3, 3, 3, 3, 3, 3, 3, 4, 4, 4, 4,
    /*E*/  4, 4, 4, 5, 4, 4, 4, 4, 4, 4, 4, 4, 5, 5, 5, 5,
    /*F*/  4, 4, 4, 5, 4, 4, 4, 4, 4, 4, 4, 4, 5, 5, 5, 5,
];

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct M6801 {
    pub pc: u16,
    pub s: u16,
    pub x: u16,
    pub a: u8,
    pub b: u8,
    pub cc: u8,
    pub model: Model,
    #[serde(with = "serde_bytes")]
    pub ram: Vec<u8>,
    wai: bool,
    /// HD6301 SLP: asleep until NMI or IRQ1
    slp: bool,
    /// HD63701Y0 port 6 data / DDR
    port6: [u8; 2],
    #[serde(skip)] extra_cycles: u32,
    nmi_line: bool,
    nmi_pending: bool,
    /// IRQ1 input level
    pub irq_line: bool,
    port_ddr: [u8; 2],
    port_data: [u8; 2],
    port2_written: bool,
    tcsr: u8,
    pending_tcsr: u8,
    counter: u16,
    ocr: u16,
    latch09: u8,
    pub total_cycles: u64,
}

impl Default for M6801 {
    fn default() -> Self { Self::new() }
}

#[inline]
fn signed(b: u8) -> u16 { b as i8 as i16 as u16 }

impl M6801 {
    pub fn new() -> Self { Self::with_model(Model::M6803) }

    pub fn with_model(model: Model) -> Self {
        Self {
            pc: 0, s: 0, x: 0, a: 0, b: 0, cc: 0xC0 | I, model,
            ram: vec![0; if model == Model::Hd63701y0 { 256 } else { 128 }],
            wai: false, slp: false, port6: [0; 2], extra_cycles: 0, nmi_line: false, nmi_pending: false, irq_line: false,
            port_ddr: [0; 2], port_data: [0; 2], port2_written: false,
            tcsr: 0, pending_tcsr: 0, counter: 0, ocr: 0xFFFF, latch09: 0,
            total_cycles: 0,
        }
    }

    pub fn reset(&mut self, bus: &mut dyn M6801Bus) {
        self.cc = 0xC0 | I;
        self.wai = false;
        self.slp = false;
        self.port6 = [0; 2];
        self.nmi_pending = false;
        self.port_ddr = [0; 2];
        self.port2_written = false;
        self.tcsr = 0;
        self.pending_tcsr = 0;
        self.counter = 0;
        self.ocr = 0xFFFF;
        self.latch09 = 0;
        self.pc = self.rm16(bus, 0xFFFE);
    }

    /// NMI input level (edge triggered on the rising edge).
    pub fn set_nmi(&mut self, level: bool) {
        if level && !self.nmi_line { self.nmi_pending = true; }
        self.nmi_line = level;
    }

    #[inline] fn d(&self) -> u16 { ((self.a as u16) << 8) | self.b as u16 }
    #[inline] fn set_d(&mut self, v: u16) { self.a = (v >> 8) as u8; self.b = v as u8; }

    // ---- memory, with the on-chip registers and RAM ----
    fn rm(&mut self, bus: &mut dyn M6801Bus, a: u16) -> u8 {
        if self.model == Model::Hd63701y0 {
            return match a {
                0x0000..=0x0027 => self.io_read(bus, a as u8),
                0x0040..=0x013F => self.ram[(a - 0x40) as usize],
                _ => bus.read(a),
            };
        }
        match a {
            0x0000..=0x001F => self.io_read(bus, a as u8),
            0x0080..=0x00FF => self.ram[(a & 0x7F) as usize],
            _ => bus.read(a),
        }
    }
    fn wm(&mut self, bus: &mut dyn M6801Bus, a: u16, v: u8) {
        if self.model == Model::Hd63701y0 {
            match a {
                0x0000..=0x0027 => self.io_write(bus, a as u8, v),
                0x0040..=0x013F => self.ram[(a - 0x40) as usize] = v,
                _ => bus.write(a, v),
            }
            return;
        }
        match a {
            0x0000..=0x001F => self.io_write(bus, a as u8, v),
            0x0080..=0x00FF => self.ram[(a & 0x7F) as usize] = v,
            _ => bus.write(a, v),
        }
    }
    fn rm16(&mut self, bus: &mut dyn M6801Bus, a: u16) -> u16 {
        let hi = self.rm(bus, a) as u16;
        (hi << 8) | self.rm(bus, a.wrapping_add(1)) as u16
    }
    fn wm16(&mut self, bus: &mut dyn M6801Bus, a: u16, v: u16) {
        self.wm(bus, a, (v >> 8) as u8);
        self.wm(bus, a.wrapping_add(1), v as u8);
    }
    fn imm8(&mut self, bus: &mut dyn M6801Bus) -> u8 {
        let v = self.rm(bus, self.pc);
        self.pc = self.pc.wrapping_add(1);
        v
    }
    fn imm16(&mut self, bus: &mut dyn M6801Bus) -> u16 {
        let v = self.rm16(bus, self.pc);
        self.pc = self.pc.wrapping_add(2);
        v
    }
    fn push8(&mut self, bus: &mut dyn M6801Bus, v: u8) {
        let s = self.s;
        self.wm(bus, s, v);
        self.s = s.wrapping_sub(1);
    }
    fn push16(&mut self, bus: &mut dyn M6801Bus, v: u16) {
        self.push8(bus, v as u8);
        self.push8(bus, (v >> 8) as u8);
    }
    fn pull8(&mut self, bus: &mut dyn M6801Bus) -> u8 {
        self.s = self.s.wrapping_add(1);
        let s = self.s;
        self.rm(bus, s)
    }
    fn pull16(&mut self, bus: &mut dyn M6801Bus) -> u16 {
        let hi = self.pull8(bus) as u16;
        (hi << 8) | self.pull8(bus) as u16
    }

    // ---- on-chip I/O ----
    fn io_read(&mut self, bus: &mut dyn M6801Bus, r: u8) -> u8 {
        match r {
            0x02 | 0x03 => {
                let p = (r - 2) as usize;
                let ddr = self.port_ddr[p];
                if ddr == 0xFF { self.port_data[p] } else {
                    (bus.port_read(p) & !ddr) | (self.port_data[p] & ddr)
                }
            }
            0x08 => { self.pending_tcsr = 0; self.tcsr }
            0x09 => {
                if self.pending_tcsr & TCSR_TOF == 0 { self.tcsr &= !TCSR_TOF; }
                (self.counter >> 8) as u8
            }
            0x0A => self.counter as u8,
            0x0B => (self.ocr >> 8) as u8,
            0x0C => self.ocr as u8,
            0x11 => 0x20,       // SCI: transmit data register empty
            0x17 if self.model == Model::Hd63701y0 => {
                let ddr = self.port6[1];
                (bus.port_read(5) & !ddr) | (self.port6[0] & ddr)
            }
            _ => 0xFF,
        }
    }

    fn io_write(&mut self, bus: &mut dyn M6801Bus, r: u8, v: u8) {
        match r {
            0x00 => {
                if self.port_ddr[0] != v {
                    self.port_ddr[0] = v;
                    self.write_port1(bus);
                }
            }
            0x01 => {
                if self.port_ddr[1] != v {
                    self.port_ddr[1] = v;
                    self.write_port2(bus);
                }
            }
            0x02 => { self.port_data[0] = v; self.write_port1(bus); }
            0x03 => { self.port_data[1] = v; self.port2_written = true; self.write_port2(bus); }
            0x08 => {
                self.tcsr = (v & 0x1F) | (self.tcsr & 0xE0);
                self.pending_tcsr &= self.tcsr;
            }
            0x09 => { self.latch09 = v; self.counter = 0xFFF8; }
            0x0A => { self.counter = ((self.latch09 as u16) << 8) | v as u16; }
            0x0B | 0x0C => {
                if self.pending_tcsr & TCSR_OCF == 0 { self.tcsr &= !TCSR_OCF; }
                if r == 0x0B { self.ocr = (self.ocr & 0x00FF) | ((v as u16) << 8); }
                else { self.ocr = (self.ocr & 0xFF00) | v as u16; }
            }
            0x16 | 0x17 if self.model == Model::Hd63701y0 => {
                let changed = r == 0x17 || self.port6[1] != v;
                self.port6[(r == 0x16) as usize] = v;
                if changed {
                    let ddr = self.port6[1];
                    bus.port_write(5, (self.port6[0] & ddr) | !ddr, ddr);
                }
            }
            _ => {}
        }
    }

    fn write_port1(&mut self, bus: &mut dyn M6801Bus) {
        let ddr = self.port_ddr[0];
        bus.port_write(0, (self.port_data[0] & ddr) | !ddr, ddr);
    }

    /// MAME write_port2: only 5 pins; all-output passes the data straight through.
    fn write_port2(&mut self, bus: &mut dyn M6801Bus) {
        if !self.port2_written { return; }
        let ddr = self.port_ddr[1] & 0x1F;
        let mut data = self.port_data[1];
        if ddr != 0x1F && ddr != 0 {
            data = (data & ddr) | !ddr;
        }
        bus.port_write(1, data & 0x1F, ddr);
    }

    fn tick_timer(&mut self, bus: &mut dyn M6801Bus, n: u32) {
        for _ in 0..n {
            self.counter = self.counter.wrapping_add(1);
            if self.counter == self.ocr {
                self.tcsr |= TCSR_OCF;
                self.pending_tcsr |= TCSR_OCF;
                if self.port_ddr[1] & 2 != 0 {
                    self.port_data[1] = (self.port_data[1] & !2) | ((self.tcsr & TCSR_OLVL) << 1);
                    self.port2_written = true;
                    self.write_port2(bus);
                }
            }
            if self.counter == 0 {
                self.tcsr |= TCSR_TOF;
                self.pending_tcsr |= TCSR_TOF;
            }
        }
    }

    fn enter_interrupt(&mut self, bus: &mut dyn M6801Bus, vector: u16) -> u32 {
        let cycles = if self.wai {
            self.wai = false;
            4
        } else {
            let (pc, x, a, b, cc) = (self.pc, self.x, self.a, self.b, self.cc);
            self.push16(bus, pc);
            self.push16(bus, x);
            self.push8(bus, a);
            self.push8(bus, b);
            self.push8(bus, cc);
            12
        };
        self.cc |= I;
        self.pc = self.rm16(bus, vector);
        cycles
    }

    /// Pending interrupt, in MAME's priority order. Returns cycles used, 0 if none.
    fn check_interrupts(&mut self, bus: &mut dyn M6801Bus) -> u32 {
        if self.nmi_pending {
            self.nmi_pending = false;
            return self.enter_interrupt(bus, 0xFFFC);
        }
        if self.cc & I != 0 { return 0; }
        if self.irq_line {
            return self.enter_interrupt(bus, 0xFFF8);
        }
        if self.tcsr & (TCSR_EOCI | TCSR_OCF) == (TCSR_EOCI | TCSR_OCF) {
            return self.enter_interrupt(bus, 0xFFF4);
        }
        if self.tcsr & (TCSR_ETOI | TCSR_TOF) == (TCSR_ETOI | TCSR_TOF) {
            return self.enter_interrupt(bus, 0xFFF2);
        }
        0
    }

    // ---- flags (MAME macros) ----
    #[inline] fn set_nz8(&mut self, r: u8) {
        if r & 0x80 != 0 { self.cc |= N; }
        if r == 0 { self.cc |= Z; }
    }
    #[inline] fn set_nz16(&mut self, r: u16) {
        if r & 0x8000 != 0 { self.cc |= N; }
        if r == 0 { self.cc |= Z; }
    }
    /// SET_FLAGS8: N Z V C from a op b = r (r keeps bit 8 as carry/borrow)
    #[inline] fn flags8(&mut self, a: u16, b: u16, r: u16) {
        self.set_nz8(r as u8);
        self.cc |= ((((a ^ b ^ r ^ (r >> 1)) & 0x80) >> 6) as u8) & V;
        self.cc |= ((r & 0x100) >> 8) as u8;
    }
    #[inline] fn flags16(&mut self, a: u32, b: u32, r: u32) {
        self.set_nz16(r as u16);
        self.cc |= ((((a ^ b ^ r ^ (r >> 1)) & 0x8000) >> 14) as u8) & V;
        self.cc |= ((r & 0x10000) >> 16) as u8;
    }
    #[inline] fn set_h(&mut self, a: u16, b: u16, r: u16) {
        self.cc |= (((a ^ b ^ r) & 0x10) << 1) as u8;
    }
    #[inline] fn nxorc(&self) -> bool { (self.cc & N != 0) != (self.cc & C != 0) }
    #[inline] fn nxorv(&self) -> bool { (self.cc & N != 0) != (self.cc & V != 0) }

    // ---- read-modify-write helpers (shared by A, B and memory forms) ----
    fn op_neg(&mut self, t: u8) -> u8 {
        let r = 0u16.wrapping_sub(t as u16);
        self.cc &= 0xF0;
        self.flags8(0, t as u16, r);
        r as u8
    }
    fn op_com(&mut self, t: u8) -> u8 {
        let r = !t;
        self.cc &= 0xF1;
        self.set_nz8(r);
        self.cc |= C;
        r
    }
    fn op_lsr(&mut self, t: u8) -> u8 {
        self.cc &= 0xF0;
        self.cc |= t & 1;
        let r = t >> 1;
        if r == 0 { self.cc |= Z; }
        if self.nxorc() { self.cc |= V; }
        r
    }
    fn op_ror(&mut self, t: u8) -> u8 {
        let mut r = (self.cc & C) << 7;
        self.cc &= 0xF0;
        self.cc |= t & 1;
        r |= t >> 1;
        self.set_nz8(r);
        if self.nxorc() { self.cc |= V; }
        r
    }
    fn op_asr(&mut self, t: u8) -> u8 {
        self.cc &= 0xF0;
        self.cc |= t & 1;
        let r = (t >> 1) | (t & 0x80);
        self.set_nz8(r);
        if self.nxorc() { self.cc |= V; }
        r
    }
    fn op_asl(&mut self, t: u8) -> u8 {
        let r = (t as u16) << 1;
        self.cc &= 0xF0;
        self.flags8(t as u16, t as u16, r);
        r as u8
    }
    fn op_rol(&mut self, t: u8) -> u8 {
        let r = ((self.cc & C) as u16) | ((t as u16) << 1);
        self.cc &= 0xF0;
        self.flags8(t as u16, t as u16, r);
        r as u8
    }
    fn op_dec(&mut self, t: u8) -> u8 {
        let r = t.wrapping_sub(1);
        self.cc &= 0xF1;
        self.set_nz8(r);
        if r == 0x7F { self.cc |= V; }
        r
    }
    fn op_inc(&mut self, t: u8) -> u8 {
        let r = t.wrapping_add(1);
        self.cc &= 0xF1;
        self.set_nz8(r);
        if r == 0x80 { self.cc |= V; }
        r
    }
    fn op_tst(&mut self, t: u8) {
        self.cc &= 0xF0;
        self.set_nz8(t);
    }

    /// 8-bit accumulator ops 0x80-0xFF low nibble: SUB CMP SBC - AND BIT LDA STA EOR ADC ORA ADD
    fn acc_op(&mut self, op: u8, acc: u8, t: u8) -> Option<u8> {
        let (a, t16) = (acc as u16, t as u16);
        match op {
            0x0 => { let r = a.wrapping_sub(t16); self.cc &= 0xF0; self.flags8(a, t16, r); Some(r as u8) }
            0x1 => { let r = a.wrapping_sub(t16); self.cc &= 0xF0; self.flags8(a, t16, r); None }
            0x2 => {
                let r = a.wrapping_sub(t16).wrapping_sub((self.cc & C) as u16);
                self.cc &= 0xF0; self.flags8(a, t16, r); Some(r as u8)
            }
            0x4 => { let r = acc & t; self.cc &= 0xF1; self.set_nz8(r); Some(r) }
            0x5 => { let r = acc & t; self.cc &= 0xF1; self.set_nz8(r); None }
            0x6 => { self.cc &= 0xF1; self.set_nz8(t); Some(t) }
            0x8 => { let r = acc ^ t; self.cc &= 0xF1; self.set_nz8(r); Some(r) }
            0x9 => {
                let r = a + t16 + (self.cc & C) as u16;
                self.cc &= 0xD0; self.flags8(a, t16, r); self.set_h(a, t16, r); Some(r as u8)
            }
            0xA => { let r = acc | t; self.cc &= 0xF1; self.set_nz8(r); Some(r) }
            0xB => {
                let r = a + t16;
                self.cc &= 0xD0; self.flags8(a, t16, r); self.set_h(a, t16, r); Some(r as u8)
            }
            _ => None,
        }
    }

    /// Effective address for the 0x80-0xFF block by column (1 = direct, 2 = indexed, 3 = extended).
    fn ea(&mut self, bus: &mut dyn M6801Bus, mode: u8) -> u16 {
        match mode {
            1 => self.imm8(bus) as u16,
            2 => { let o = self.imm8(bus) as u16; self.x.wrapping_add(o) }
            _ => self.imm16(bus),
        }
    }

    /// Run one instruction (or take an interrupt). Returns E-clock cycles.
    pub fn step(&mut self, bus: &mut dyn M6801Bus) -> u32 {
        if self.slp && (self.nmi_pending || self.irq_line) { self.slp = false; }
        let mut cycles = self.check_interrupts(bus);
        if cycles == 0 {
            if self.wai || self.slp {
                cycles = 1;
            } else {
                let op = self.imm8(bus);
                self.extra_cycles = 0;
                if self.model == Model::Hd63701y0 {
                    if !self.exec_6301(bus, op) { self.exec(bus, op); }
                    cycles = CYCLES_63701[op as usize] as u32 + self.extra_cycles;
                } else {
                    self.exec(bus, op);
                    cycles = CYCLES_6803[op as usize] as u32;
                }
            }
        }
        self.tick_timer(bus, cycles);
        self.total_cycles += cycles as u64;
        cycles
    }

    /// HD6301 opcodes that differ from the 6803 (MAME hd63701_insn). Returns false
    /// for everything else.
    fn exec_6301(&mut self, bus: &mut dyn M6801Bus, op: u8) -> bool {
        match op {
            0x00 | 0x02 | 0x03 | 0x14 | 0x15 | 0x1C..=0x1F
            | 0x41 | 0x42 | 0x45 | 0x4B | 0x4E | 0x51 | 0x52 | 0x55 | 0x5B | 0x5E
            | 0x87 | 0x8F | 0xC7 | 0xCD | 0xCF => {
                // TRAP: back up to the opcode, interrupt through FFEE
                self.pc = self.pc.wrapping_sub(1);
                self.extra_cycles = self.enter_interrupt(bus, 0xFFEE);
            }
            0x12 | 0x13 => {
                let s = self.s.wrapping_add(1);
                let v = self.rm(bus, s) as u16;
                self.x = self.x.wrapping_add(v);
            }
            0x18 => { let t = self.x; self.x = self.d(); self.set_d(t); }   // XGDX
            0x1A => self.slp = !(self.nmi_pending || self.irq_line),      // SLP
            0x61 | 0x62 | 0x65 | 0x6B | 0x71 | 0x72 | 0x75 | 0x7B => {
                // AIM / OIM / EIM / TIM: #imm, then indexed (6x) or direct (7x)
                let t = self.imm8(bus);
                let o = self.imm8(bus) as u16;
                let ea = if op < 0x70 { self.x.wrapping_add(o) } else { o };
                let m = self.rm(bus, ea);
                let r = match op & 0x0F { 0x1 => m & t, 0x2 => m | t, 0x5 => m ^ t, _ => m & t };
                self.cc &= 0xF1;
                self.set_nz8(r);
                if op & 0x0F != 0xB { self.wm(bus, ea, r); }
            }
            _ => return false,
        }
        true
    }

    fn branch(&mut self, bus: &mut dyn M6801Bus, cond: bool) {
        let t = self.imm8(bus);
        if cond { self.pc = self.pc.wrapping_add(signed(t)); }
    }

    fn exec(&mut self, bus: &mut dyn M6801Bus, op: u8) {
        match op {
            0x01 => {}
            0x04 => { // LSRD
                let t = self.d();
                self.cc &= 0xF0;
                self.cc |= (t & 1) as u8;
                let r = t >> 1;
                if r == 0 { self.cc |= Z; }
                if self.nxorc() { self.cc |= V; }
                self.set_d(r);
            }
            0x05 => { // ASLD
                let t = self.d() as u32;
                let r = t << 1;
                self.cc &= 0xF0;
                self.flags16(t, t, r);
                self.set_d(r as u16);
            }
            0x06 => self.cc = self.a,
            0x07 => self.a = self.cc,
            0x08 => { self.x = self.x.wrapping_add(1); self.cc &= !Z; if self.x == 0 { self.cc |= Z; } }
            0x09 => { self.x = self.x.wrapping_sub(1); self.cc &= !Z; if self.x == 0 { self.cc |= Z; } }
            0x0A => self.cc &= !V,
            0x0B => self.cc |= V,
            0x0C => self.cc &= !C,
            0x0D => self.cc |= C,
            0x0E => self.cc &= !I,
            0x0F => self.cc |= I,
            0x10 | 0x11 => { // SBA, CBA
                let (a, b) = (self.a as u16, self.b as u16);
                let r = a.wrapping_sub(b);
                self.cc &= 0xF0;
                self.flags8(a, b, r);
                if op == 0x10 { self.a = r as u8; }
            }
            0x16 => { self.b = self.a; self.cc &= 0xF1; self.set_nz8(self.b); }
            0x17 => { self.a = self.b; self.cc &= 0xF1; self.set_nz8(self.a); }
            0x19 => { // DAA
                let a = self.a;
                let (msn, lsn) = (a & 0xF0, a & 0x0F);
                let mut cf: u16 = 0;
                if lsn > 9 || self.cc & H != 0 { cf |= 0x06; }
                if msn > 0x80 && lsn > 9 { cf |= 0x60; }
                if msn > 0x90 || self.cc & C != 0 { cf |= 0x60; }
                let t = cf + a as u16;
                self.cc &= 0xF1;
                self.set_nz8(t as u8);
                self.cc |= ((t & 0x100) >> 8) as u8;
                self.a = t as u8;
            }
            0x1B => { // ABA
                let (a, b) = (self.a as u16, self.b as u16);
                let r = a + b;
                self.cc &= 0xD0;
                self.flags8(a, b, r);
                self.set_h(a, b, r);
                self.a = r as u8;
            }
            0x20..=0x2F => {
                let cc = self.cc;
                let (n, z, v, c) = (cc & N != 0, cc & Z != 0, cc & V != 0, cc & C != 0);
                let cond = match op & 0x0F {
                    0x0 => true, 0x1 => false,
                    0x2 => !(c || z), 0x3 => c || z,
                    0x4 => !c, 0x5 => c,
                    0x6 => !z, 0x7 => z,
                    0x8 => !v, 0x9 => v,
                    0xA => !n, 0xB => n,
                    0xC => !self.nxorv(), 0xD => self.nxorv(),
                    0xE => !(self.nxorv() || z), _ => self.nxorv() || z,
                };
                self.branch(bus, cond);
            }
            0x30 => self.x = self.s.wrapping_add(1),
            0x31 => self.s = self.s.wrapping_add(1),
            0x32 => self.a = self.pull8(bus),
            0x33 => self.b = self.pull8(bus),
            0x34 => self.s = self.s.wrapping_sub(1),
            0x35 => self.s = self.x.wrapping_sub(1),
            0x36 => { let a = self.a; self.push8(bus, a); }
            0x37 => { let b = self.b; self.push8(bus, b); }
            0x38 => self.x = self.pull16(bus),
            0x39 => self.pc = self.pull16(bus),
            0x3A => self.x = self.x.wrapping_add(self.b as u16),
            0x3B => {
                self.cc = self.pull8(bus);
                self.b = self.pull8(bus);
                self.a = self.pull8(bus);
                self.x = self.pull16(bus);
                self.pc = self.pull16(bus);
            }
            0x3C => { let x = self.x; self.push16(bus, x); }
            0x3D => { // MUL
                let t = self.a as u16 * self.b as u16;
                self.cc &= !C;
                if t & 0x80 != 0 { self.cc |= C; }
                self.set_d(t);
            }
            0x3E => { // WAI: stack everything now, then wait
                let (pc, x, a, b, cc) = (self.pc, self.x, self.a, self.b, self.cc);
                self.push16(bus, pc);
                self.push16(bus, x);
                self.push8(bus, a);
                self.push8(bus, b);
                self.push8(bus, cc);
                self.wai = true;
            }
            0x3F => { // SWI
                let (pc, x, a, b, cc) = (self.pc, self.x, self.a, self.b, self.cc);
                self.push16(bus, pc);
                self.push16(bus, x);
                self.push8(bus, a);
                self.push8(bus, b);
                self.push8(bus, cc);
                self.cc |= I;
                self.pc = self.rm16(bus, 0xFFFA);
            }
            0x40..=0x5F => {
                let t = if op < 0x50 { self.a } else { self.b };
                let r = match op & 0x0F {
                    0x0 => Some(self.op_neg(t)),
                    0x3 => Some(self.op_com(t)),
                    0x4 => Some(self.op_lsr(t)),
                    0x6 => Some(self.op_ror(t)),
                    0x7 => Some(self.op_asr(t)),
                    0x8 => Some(self.op_asl(t)),
                    0x9 => Some(self.op_rol(t)),
                    0xA => Some(self.op_dec(t)),
                    0xC => Some(self.op_inc(t)),
                    0xD => { self.op_tst(t); None }
                    0xF => { self.cc &= 0xF0; self.cc |= Z; Some(0) }
                    _ => None,
                };
                if let Some(r) = r { if op < 0x50 { self.a = r } else { self.b = r } }
            }
            0x60..=0x7F => {
                let ea = if op < 0x70 { let o = self.imm8(bus) as u16; self.x.wrapping_add(o) } else { self.imm16(bus) };
                match op & 0x0F {
                    0xE => { self.pc = ea; return; }
                    0xF => { self.wm(bus, ea, 0); self.cc &= 0xF0; self.cc |= Z; return; }
                    _ => {}
                }
                let t = self.rm(bus, ea);
                let r = match op & 0x0F {
                    0x0 => Some(self.op_neg(t)),
                    0x3 => Some(self.op_com(t)),
                    0x4 => Some(self.op_lsr(t)),
                    0x6 => Some(self.op_ror(t)),
                    0x7 => Some(self.op_asr(t)),
                    0x8 => Some(self.op_asl(t)),
                    0x9 => Some(self.op_rol(t)),
                    0xA => Some(self.op_dec(t)),
                    0xC => Some(self.op_inc(t)),
                    0xD => { self.op_tst(t); None }
                    _ => None,
                };
                if let Some(r) = r { self.wm(bus, ea, r); }
            }
            0x8D => { // BSR
                let t = self.imm8(bus);
                let pc = self.pc;
                self.push16(bus, pc);
                self.pc = self.pc.wrapping_add(signed(t));
            }
            0x80..=0xFF => self.exec_block(bus, op),
            _ => {}   // illegal: 1-byte no-op, like MAME's illegl1
        }
    }

    /// The regular 0x80-0xFF block: high nibble picks A/B and the addressing mode.
    fn exec_block(&mut self, bus: &mut dyn M6801Bus, op: u8) {
        let use_b = op >= 0xC0;
        let mode = (op >> 4) & 3;    // 0 imm, 1 direct, 2 indexed, 3 extended
        let low = op & 0x0F;
        match low {
            0x3 => { // SUBD (A side) / ADDD (B side)
                let t = if mode == 0 { self.imm16(bus) } else { let ea = self.ea(bus, mode); self.rm16(bus, ea) } as u32;
                let d = self.d() as u32;
                let r = if use_b { d + t } else { d.wrapping_sub(t) };
                self.cc &= 0xF0;
                self.flags16(d, t, r);
                self.set_d(r as u16);
            }
            0xC => {
                if !use_b { // CPX
                    let t = if mode == 0 { self.imm16(bus) } else { let ea = self.ea(bus, mode); self.rm16(bus, ea) } as u32;
                    let d = self.x as u32;
                    let r = d.wrapping_sub(t);
                    self.cc &= 0xF0;
                    self.flags16(d, t, r);
                } else { // LDD
                    let t = if mode == 0 { self.imm16(bus) } else { let ea = self.ea(bus, mode); self.rm16(bus, ea) };
                    self.set_d(t);
                    self.cc &= 0xF1;
                    self.set_nz16(t);
                }
            }
            0xD => {
                if !use_b { // JSR (0x9D direct, 0xAD indexed, 0xBD extended); 0x8D is BSR
                    let ea = self.ea(bus, mode);
                    let pc = self.pc;
                    self.push16(bus, pc);
                    self.pc = ea;
                } else { // STD
                    let ea = if mode == 0 { let e = self.pc; self.pc = self.pc.wrapping_add(2); e } else { self.ea(bus, mode) };
                    let d = self.d();
                    self.cc &= 0xF1;
                    self.set_nz16(d);
                    self.wm16(bus, ea, d);
                }
            }
            0xE => { // LDS / LDX
                let t = if mode == 0 { self.imm16(bus) } else { let ea = self.ea(bus, mode); self.rm16(bus, ea) };
                if use_b { self.x = t } else { self.s = t }
                self.cc &= 0xF1;
                self.set_nz16(t);
            }
            0xF => { // STS / STX
                let ea = if mode == 0 { let e = self.pc; self.pc = self.pc.wrapping_add(2); e } else { self.ea(bus, mode) };
                let v = if use_b { self.x } else { self.s };
                self.cc &= 0xF1;
                self.set_nz16(v);
                self.wm16(bus, ea, v);
            }
            0x7 => { // STA / STB (immediate form writes to the byte after the opcode, like MAME)
                let ea = if mode == 0 { let e = self.pc; self.pc = self.pc.wrapping_add(1); e } else { self.ea(bus, mode) };
                let v = if use_b { self.b } else { self.a };
                self.cc &= 0xF1;
                self.set_nz8(v);
                self.wm(bus, ea, v);
            }
            _ => {
                let t = if mode == 0 { self.imm8(bus) } else { let ea = self.ea(bus, mode); self.rm(bus, ea) };
                let acc = if use_b { self.b } else { self.a };
                if let Some(r) = self.acc_op(low, acc, t) {
                    if use_b { self.b = r } else { self.a = r }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Mem { m: Vec<u8>, p1: u8, p2: u8 }
    impl M6801Bus for Mem {
        fn read(&mut self, a: u16) -> u8 { self.m[a as usize] }
        fn write(&mut self, a: u16, v: u8) { self.m[a as usize] = v; }
        fn port_write(&mut self, p: usize, d: u8, _ddr: u8) { if p == 0 { self.p1 = d } else { self.p2 = d } }
    }

    fn run(code: &[u8], steps: usize) -> (M6801, Mem) {
        let mut bus = Mem { m: vec![0; 0x10000], p1: 0, p2: 0 };
        bus.m[0x4000..0x4000 + code.len()].copy_from_slice(code);
        bus.m[0xFFFE] = 0x40;
        bus.m[0xFFFF] = 0x00;
        let mut cpu = M6801::new();
        cpu.reset(&mut bus);
        for _ in 0..steps { cpu.step(&mut bus); }
        (cpu, bus)
    }

    #[test]
    fn arithmetic_and_flags() {
        // LDAA #$7F; ADDA #$01 -> $80, V and N set, H set
        let (c, _) = run(&[0x86, 0x7F, 0x8B, 0x01], 2);
        assert_eq!(c.a, 0x80);
        assert_eq!(c.cc & (N | V | H | C | Z), N | V | H);
        // LDAA #$10; SUBA #$20 -> $F0, carry (borrow), N
        let (c, _) = run(&[0x86, 0x10, 0x80, 0x20], 2);
        assert_eq!(c.a, 0xF0);
        assert_eq!(c.cc & (N | C | Z | V), N | C);
        // LDD #$1234; ADDD #$EDCC -> $0000 Z C
        let (c, _) = run(&[0xCC, 0x12, 0x34, 0xC3, 0xED, 0xCC], 2);
        assert_eq!(c.d(), 0);
        assert_eq!(c.cc & (Z | C), Z | C);
        // LDAA #$0C; LDAB #$0A; MUL -> $78
        let (c, _) = run(&[0x86, 0x0C, 0xC6, 0x0A, 0x3D], 3);
        assert_eq!(c.d(), 0x78);
    }

    #[test]
    fn stack_subroutine_and_ram() {
        // LDS #$00FF; JSR $4010; (at 4010) LDAA #$55; STAA $80; RTS ; then STAB? -> check RAM
        let mut code = vec![0x8E, 0x00, 0xFF, 0xBD, 0x40, 0x10, 0x01];
        code.resize(0x10, 0x01);
        code.extend_from_slice(&[0x86, 0x55, 0x97, 0x80, 0x39]);
        let (c, _) = run(&code, 5);
        assert_eq!(c.ram[0], 0x55);
        assert_eq!(c.pc, 0x4006);
        assert_eq!(c.s, 0x00FF);
    }

    #[test]
    fn ports_and_output_compare_irq() {
        // DDR1 = FF, port1 = $A5; DDR2 = 1F, port2 = $15
        // set OCR = 0x0040, enable OCI, CLI, then spin
        let code = [
            0x86, 0xFF, 0x97, 0x00, 0x86, 0xA5, 0x97, 0x02,
            0x86, 0x1F, 0x97, 0x01, 0x86, 0x15, 0x97, 0x03,
            0xCC, 0x00, 0x40, 0xDD, 0x0B,       // LDD #$0040; STD $0B
            0x86, 0x08, 0x97, 0x08,             // TCSR = EOCI
            0x8E, 0x00, 0xFF, 0x0E,             // LDS #$FF; CLI
            0x20, 0xFE,                         // BRA *
        ];
        let mut bus = Mem { m: vec![0; 0x10000], p1: 0, p2: 0 };
        bus.m[0x4000..0x4000 + code.len()].copy_from_slice(&code);
        bus.m[0xFFFE] = 0x40; bus.m[0xFFFF] = 0x00;
        bus.m[0xFFF4] = 0x50; bus.m[0xFFF5] = 0x00;
        bus.m[0x5000] = 0x20; bus.m[0x5001] = 0xFE;   // OCI handler: BRA *
        let mut cpu = M6801::new();
        cpu.reset(&mut bus);
        for _ in 0..100 { cpu.step(&mut bus); }
        assert_eq!(bus.p1, 0xA5);
        assert_eq!(bus.p2, 0x15);
        assert_eq!(cpu.pc & 0xFF00, 0x5000, "OCI taken");
    }

    struct Y0Bus { m: Vec<u8>, p6: Vec<u8> }
    impl M6801Bus for Y0Bus {
        fn read(&mut self, a: u16) -> u8 { self.m[a as usize] }
        fn write(&mut self, a: u16, v: u8) { self.m[a as usize] = v; }
        fn port_write(&mut self, p: usize, d: u8, _ddr: u8) { if p == 5 { self.p6.push(d) } }
    }

    #[test]
    fn hd63701_extras() {
        let code = [
            0x8E, 0x01, 0x3F,       // LDS #$013F (top of on-chip RAM)
            0x86, 0xF0, 0x97, 0x40, // LDAA #$F0; STAA $40 (on-chip RAM)
            0x71, 0x3C, 0x40,       // AIM #$3C,$40 -> $30
            0x72, 0x01, 0x40,       // OIM #$01,$40 -> $31
            0x75, 0xFF, 0x40,       // EIM #$FF,$40 -> $CE
            0x7B, 0x00, 0x40,       // TIM #$00,$40 -> Z
            0xCC, 0x12, 0x34, 0xCE, 0x56, 0x78, 0x18,   // LDD; LDX; XGDX
            0x86, 0xFF, 0x97, 0x16, 0x86, 0x02, 0x97, 0x17, // port 6 DDR = FF, data = 02
            0x1A,                   // SLP
            0x00,                   // illegal -> TRAP
        ];
        let mut bus = Y0Bus { m: vec![0; 0x10000], p6: vec![] };
        bus.m[0xC000..0xC000 + code.len()].copy_from_slice(&code);
        bus.m[0xFFFE] = 0xC0; bus.m[0xFFFF] = 0x00;
        bus.m[0xFFFC] = 0xD0; bus.m[0xFFFD] = 0x00;   // NMI -> D000: RTI
        bus.m[0xD000] = 0x3B;
        bus.m[0xFFEE] = 0xE0; bus.m[0xFFEF] = 0x00;   // TRAP -> E000
        let mut cpu = M6801::with_model(Model::Hd63701y0);
        cpu.reset(&mut bus);
        for _ in 0..3 { cpu.step(&mut bus); }
        assert_eq!(cpu.ram[0], 0xF0);
        assert_eq!(bus.m[0x40], 0, "0x40 is on-chip RAM, not the bus");
        cpu.step(&mut bus); assert_eq!(cpu.ram[0], 0x30);
        cpu.step(&mut bus); assert_eq!(cpu.ram[0], 0x31);
        cpu.step(&mut bus); assert_eq!(cpu.ram[0], 0xCE);
        cpu.step(&mut bus); assert_eq!(cpu.ram[0], 0xCE); assert!(cpu.cc & Z != 0);
        for _ in 0..3 { cpu.step(&mut bus); }
        assert_eq!((cpu.d(), cpu.x), (0x5678, 0x1234));
        for _ in 0..4 { cpu.step(&mut bus); }
        assert_eq!(bus.p6.last(), Some(&0x02));
        cpu.step(&mut bus);                        // SLP
        let pc = cpu.pc;
        for _ in 0..10 { assert_eq!(cpu.step(&mut bus), 1); }
        assert_eq!(cpu.pc, pc, "asleep");
        cpu.set_nmi(true);
        cpu.step(&mut bus);                        // wakes, takes NMI
        assert_eq!(cpu.pc, 0xD000);
        cpu.step(&mut bus);                        // RTI
        assert_eq!(cpu.pc, pc);
        let c = cpu.step(&mut bus);                // illegal 0x00
        assert_eq!(cpu.pc, 0xE000);
        assert_eq!(c, 4 + 12);
        // the stacked PC points back at the illegal opcode
        let s = cpu.s as usize;
        assert_eq!(((cpu.ram[s + 6 - 0x40] as u16) << 8) | cpu.ram[s + 7 - 0x40] as u16, pc);
    }
}
