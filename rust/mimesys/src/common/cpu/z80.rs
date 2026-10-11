/*
pub struct CpuConfig {
}
*/

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CpuVariant {
    ZilogZ80,
    ZilogZ180,
}

const FLAG_S:  u8 = 0x80;
const FLAG_Z:  u8 = 0x40;
const FLAG_Y:  u8 = 0x20;  // undocumented. often mirrors bit 5 of operand
const FLAG_H:  u8 = 0x10;
const FLAG_X:  u8 = 0x08;  // undocumented. often mirrors bit 3 of operand
const FLAG_PV: u8 = 0x04;
const FLAG_N:  u8 = 0x02;
const FLAG_C:  u8 = 0x01;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum IndexMode { HL, IX, IY }

pub struct Z80Cpu {
    pub a: u8, pub f: u8,
    pub b: u8, pub c: u8,
    pub d: u8, pub e: u8,
    pub h: u8, pub l: u8,

    // Shadow set, swapped in via EX AF,AF' / EXX
    pub a_: u8, pub f_: u8,
    pub b_: u8, pub c_: u8,
    pub d_: u8, pub e_: u8,
    pub h_: u8, pub l_: u8,

    pub ix: u16,
    pub iy: u16,
    pub sp: u16,
    pub pc: u16,
    pub i: u8,   // interrupt vector
    pub r: u8,   // memory refresh
    iff1: bool,
    iff2: bool,
    im: u8,
    ei_pending: bool,
    irq_pending: bool,
    nmi_pending: bool,
    prev_nmi_line: bool,
    halted: bool,
    last_cycles: u8,
    last_opcode: u8,
    pub is_running: bool,
//    pub config: CpuConfig,
    variant: CpuVariant,
}

impl Z80Cpu {
    // TODO: pass variant as parameter and assign values based on it.
    pub fn new() -> Self {
        Self {
            a: 0, b: 0, c: 0, d: 0, e: 0, f: 0, h: 0, l: 0,
            a_: 0, b_: 0, c_: 0, d_: 0, e_: 0, f_: 0, h_: 0, l_: 0,
            pc: 0,
            sp: 0,
            i: 0, r: 0, ix: 0, iy: 0,
            halted: false,
            iff1: false, iff2: false, ei_pending: false, im: 0,
            nmi_pending: false, prev_nmi_line: false, irq_pending: false,
            last_cycles: 0,
            last_opcode: 0,
            is_running: false,
            variant: CpuVariant::ZilogZ80,
        }
    }

    fn pop(&mut self, bus: &mut dyn Bus) -> u8 {
        let val = bus.read(self.sp);
        self.sp = self.sp.wrapping_add(1);
        val
    }

    fn push(&mut self, bus: &mut dyn Bus, p_val: u8) {
        self.sp = self.sp.wrapping_sub(1);
        bus.write(self.sp, p_val);
    }

    fn pop16(&mut self, bus: &mut dyn Bus) -> u16 {
        let lo:u8 = self.pop(bus);
        let hi:u8 = self.pop(bus);
        return ((hi as u16) << 8) | (lo as u16);
    }

    fn push16(&mut self, bus: &mut dyn Bus, p_val: u16) {
        self.push(bus, (p_val >> 8) as u8);
        self.push(bus, p_val as u8);
    }

    pub fn bc(&self) -> u16 { ((self.b as u16) << 8) | self.c as u16 }
    pub fn set_bc(&mut self, v: u16) { self.b = (v >> 8) as u8; self.c = v as u8; }

    pub fn de(&self) -> u16 { ((self.d as u16) << 8) | self.e as u16 }
    pub fn set_de(&mut self, v: u16) { self.d = (v >> 8) as u8; self.e = v as u8; }

    pub fn hl(&self) -> u16 { ((self.h as u16) << 8) | self.l as u16 }
    pub fn set_hl(&mut self, v: u16) { self.h = (v >> 8) as u8; self.l = v as u8; }

    pub fn af(&self) -> u16 { ((self.a as u16) << 8) | self.f as u16 }
    pub fn set_af(&mut self, v: u16) { self.a = (v >> 8) as u8; self.f = v as u8; }

    #[inline]
    fn mem_operand_addr(&mut self, index: IndexMode, bus: &mut dyn Bus) -> u16 {
        match index {
            IndexMode::HL => self.hl(),
            IndexMode::IX => { let d = self.fetch_u8(bus) as i8; self.ix.wrapping_add(d as i16 as u16) },
            IndexMode::IY => { let d = self.fetch_u8(bus) as i8; self.iy.wrapping_add(d as i16 as u16) },
        }
    }

    pub fn read_r(&self, r: u8, index: IndexMode, addr: u16, bus: &mut dyn Bus) -> u8 {
        match r & 0x07 {
            0 => self.b, 1 => self.c, 2 => self.d, 3 => self.e,
            4 => match index { 
                IndexMode::HL => self.h,
                IndexMode::IX => (self.ix >> 8) as u8,
                IndexMode::IY => (self.iy >> 8) as u8,
            },
            5 => match index {
                IndexMode::HL => self.l,
                IndexMode::IX => self.ix as u8,
                IndexMode::IY => self.iy as u8,
            },
            6 => bus.read(addr),
            7 => self.a,
            _ => unreachable!(),
        }
    }

    pub fn write_r(&mut self, r: u8, index: IndexMode, addr: u16, value: u8, bus: &mut dyn Bus) {
        match r & 0x07 {
            0 => self.b = value, 1 => self.c = value, 2 => self.d = value, 3 => self.e = value,
            4 => match index {
                IndexMode::HL => self.h = value,
                IndexMode::IX => self.ix = (self.ix & 0x00FF) | ((value as u16) << 8),
                IndexMode::IY => self.iy = (self.iy & 0x00FF) | ((value as u16) << 8),
            },
            5 => match index {
                IndexMode::HL => self.l = value,
                IndexMode::IX => self.ix = (self.ix & 0xFF00) | value as u16,
                IndexMode::IY => self.iy = (self.iy & 0xFF00) | value as u16,
            },
            6 => bus.write(addr, value),
            7 => self.a = value,
            _ => unreachable!(),
        }
    }

    pub fn get_index_reg(&self, index: IndexMode) -> u16 {
        match index {
            IndexMode::IX => self.ix,
            IndexMode::IY => self.iy,
            _             => self.hl(),
        }
    }

    pub fn set_index_reg(&mut self, index: IndexMode, value: u16) {
        match index {
            IndexMode::IX => { self.ix = value; },
            IndexMode::IY => { self.iy = value; },
            _             => { self.set_hl(value); },
        }
    }

    fn get_rp(&self, idx: u8, index: IndexMode) -> u16 {
        match idx & 3 {
            0 => self.bc(),
            1 => self.de(),
            2 => self.get_index_reg(index),
            _ => self.sp,
        }
    }

    fn set_rp(&mut self, idx: u8, index: IndexMode, val: u16) {
        match idx & 3 {
            0 => self.set_bc(val),
            1 => self.set_de(val),
            2 => self.set_index_reg(index, val),
            _ => self.sp = val,
        }
    }

    fn get_rp2(&self, idx: u8, index: IndexMode) -> u16 {
        match idx & 3 {
            0 => self.bc(),
            1 => self.de(),
            2 => self.get_index_reg(index),
            _ => self.af(),
        }
    }

    fn set_rp2(&mut self, idx: u8, index: IndexMode, val: u16) {
        match idx & 3 {
            0 => self.set_bc(val),
            1 => self.set_de(val),
            2 => self.set_index_reg(index, val),
            _ => self.set_af(val),
        }
    }

    fn step_hl(&mut self, inc: bool) {
        let hl = self.hl();
        self.set_hl(if inc { hl.wrapping_add(1) } else {hl.wrapping_sub(1) });
    }

    fn block_ld(&mut self, inc: bool, bus: &mut dyn Bus) -> bool {
        let v = bus.read(self.hl());
        bus.write(self.de(), v);
        self.step_hl(inc);
        let de = self.de();
        self.set_de(if inc { de.wrapping_add(1) } else { de.wrapping_sub(1) } );
        let bc = self.bc().wrapping_sub(1);
        self.set_bc(bc);

        let n = v.wrapping_add(self.a);
        let mut f = self.f & (FLAG_S | FLAG_Z | FLAG_C);
        if bc != 0 { f |= FLAG_PV; }
        f |= n & FLAG_X;
        if n & 0x02 != 0 { f |= FLAG_Y; }
        self.f = f;
        bc != 0
    }

    fn block_cp(&mut self, inc: bool, bus: &mut dyn Bus) -> bool {
        let v = bus.read(self.hl());
        let r = self.a.wrapping_sub(v);
        self.step_hl(inc);
        let bc = self.bc().wrapping_sub(1);
        self.set_bc(bc);

        let h = (self.a ^ v ^ r) & 0x10 != 0;
        let mut f = (self.f & FLAG_C) | FLAG_N | (r & FLAG_S);
        if r == 0 { f |= FLAG_Z; }
        if h { f |= FLAG_H; }
        if bc != 0 { f |= FLAG_PV; }
        let n = r.wrapping_sub(h as u8);
        f |= n & FLAG_X;
        if n & 0x02 != 0 { f |= FLAG_Y; }
        self.f = f;
        bc != 0 && r != 0
    }

    fn block_in(&mut self, inc: bool, repeat: bool, bus: &mut dyn Bus) -> bool {
        let v = bus.port_in(self.bc());
        bus.write(self.hl(), v);
        self.step_hl(inc);
        self.b = self.b.wrapping_sub(1);
        let c2 = if inc { self.c.wrapping_add(1) } else { self.c.wrapping_sub(1) };
        self.f = self.io_block_flags(v, v as u16 + c2 as u16, repeat);
        self.b != 0
    }

    fn block_out(&mut self, inc: bool, repeat: bool, bus: &mut dyn Bus) -> bool {
        let v = bus.read(self.hl());
        self.b = self.b.wrapping_sub(1);
        bus.port_out(self.bc(), v);
        self.step_hl(inc);
        self.f = self.io_block_flags(v, v as u16 + self.l as u16, repeat);
        self.b != 0
    }

    fn io_block_flags(&self, v: u8, k:u16, repeat: bool) -> u8 {
        let b = self.b;
        let even = |x: u8| x.count_ones() % 2 == 0;
        let mut f = b & (FLAG_S | FLAG_Y | FLAG_X);
        if b == 0 { f |= FLAG_Z; }
        if v & 0x80 != 0 { f |= FLAG_N; }
        if k > 0xFF { f |= FLAG_H | FLAG_C; }
        if even(((k as u8) & 7) ^ b) { f |= FLAG_PV; }

        if repeat && b != 0 {
            f &= !FLAG_H;
            let x = if k > 0xFF {
                if v & 0x80 != 0 {
                    if b & 0x0F == 0x00 { f |= FLAG_H; }
                    b.wrapping_sub(1) & 7
                } else {
                    if b & 0x0F == 0x0F { f |= FLAG_H; }
                    b.wrapping_add(1) & 7
                }
            } else {
                b & 7
            };
            if !even(x) { f ^= FLAG_PV; }
        }
        f
    }

    fn check_parity(&self, p_val: u8) -> bool {
        p_val.count_ones() % 2 == 0
    }

    fn cond(&self, y: u8) -> bool {
        match y & 7 {
            0 => self.f & FLAG_Z == 0, 1 => (self.f & FLAG_Z) != 0,
            2 => self.f & FLAG_C == 0, 3 => self.f & FLAG_C != 0,
            4 => self.f & FLAG_PV == 0, 5 => self.f & FLAG_PV != 0,
            6 => self.f & FLAG_S == 0, _ => self.f & FLAG_S != 0,
        }
    }

    pub fn trigger_nmi(&mut self) {
        self.nmi_pending = true;
    }

    pub fn trigger_interrupt(&mut self) {
        self.irq_pending = true;
    }

    pub fn step(&mut self, bus: &mut dyn Bus) -> u32 {
        let can_process_irq = self.iff1 && self.ei_pending;
        if self.ei_pending { self.ei_pending = false; }

        if self.halted {
            self.r = (self.r & 0x80) | self.r.wrapping_add(1) & 0x7F;
            return 4;
        }
        let mut cycles = 0u32;
        let mut index = IndexMode::HL;

        loop {
            let byte = self.fetch_u8(bus);
            self.inc_r();

            match byte {
                0xDD => {
                    index = IndexMode::IX;
                    cycles += 4;
                    continue;
                }
                0xFD => {
                    index = IndexMode::IY;
                    cycles += 4;
                    continue;
                }

                0xCB => {
                    if index == IndexMode::HL {
                        let op = self.fetch_u8(bus);
                        self.inc_r();
                        cycles += self.exec_cb(op, index, 0, bus);
                    } else {
                        let disp = self.fetch_i8(bus) as i8;
                        let op = self.fetch_u8(bus);
                        cycles += self.exec_cb(op, index, disp, bus);
                    }
                    return cycles;
                }

                0xED => {
                    let op = self.fetch_u8(bus);
                    self.r = (self.r & 0x80) | self.r.wrapping_add(1) & 0x7F;
                    cycles += self.exec_ed(op, bus);
                    return cycles;
                }

                _ => {
                    cycles += self.exec_main(byte, index, bus);
                    return cycles;
                }
            }
        }
    }

    #[inline]
    fn fetch_u8(&mut self, bus: &mut dyn Bus) -> u8 {
        let byte:u8 = bus.read(self.pc);
        self.pc = self.pc.wrapping_add(1);
        byte
    }

    #[inline]
    fn fetch_i8(&mut self, bus: &mut dyn Bus) -> i8 {
        let byte:u8 = bus.read(self.pc);
        self.pc = self.pc.wrapping_add(1);
        byte.cast_signed()
    }

    #[inline]
    fn fetch_u16(&mut self, bus: &mut dyn Bus) -> u16 {
        let lo = self.fetch_u8(bus) as u16;
        let hi = self.fetch_u8(bus) as u16;
        lo | (hi << 8)
    }

    fn exec_main(&mut self, op: u8, index: IndexMode, bus: &mut dyn Bus) -> u32 {
        match op {
            0x00 => 4,   // NOP
            0x01 => {    // LD BC, nn
                self.c = self.fetch_u8(bus);
                self.b = self.fetch_u8(bus);
                10
            }
            0x02 => {    // LD (BC), a
                bus.write(self.bc(), self.a);
                7
            }
            // INC rr / DEC rr
            0x03 | 0x13 | 0x23 | 0x33 | 0x0B | 0x1B | 0x2B | 0x3B => {
                let rp = (op >> 4) & 3;
                let v = self.get_rp(rp, index);
                let result = if op & 0x08 == 0 { v.wrapping_add(1) } else { v.wrapping_sub(1) };
                self.set_rp(rp, index, result);
                6
            }
            // INC r / DEC r
            0x04 | 0x05 | 0x14 | 0x15 | 0x24 | 0x25 | 0x34 | 0x35 |
            0x0C | 0x0D | 0x1C | 0x1D | 0x2C | 0x2D | 0x3C | 0x3D => {
                let r = (op >> 3) & 7;
                let addr = if r == 6 { self.mem_operand_addr(index, bus) } else { 0 };
                let value = self.read_r(r, index, addr, bus);
                let result = if op & 0x01 == 0 { self.inc8(value) } else { self.dec8(value) };
                self.write_r(r, index, addr, result, bus);
                match (r, index) {
                    (6, IndexMode::HL) => 11,
                    (6, _) => 19,
                    _ => 4,
                }
            }
            0x06|0x0E|0x16|0x1E|0x26|0x2E|0x3E|0x36 => {    // LD r, n
                let dest = (op >> 3) & 0x07;
                let addr = if dest == 6 { self.mem_operand_addr(index, bus) } else { 0 };
                let n = self.fetch_u8(bus);
                if dest == 6 {
                    bus.write(addr, n);
                    return if index == IndexMode::HL { 10 } else { 15 };
                } else {
                    self.write_r(dest, index, addr, n, bus);
                }
                7
            }
            0x07 | 0x0F | 0x17 | 0x1F => {   // RLCA, RRCA, RLA, RRA
//                self.op_rl(false);
                let keep = self.f & (FLAG_S | FLAG_Z | FLAG_PV);
                self.a = self.rotate_shift(op >> 3, self.a);
                self.f = (self.f & (FLAG_C | FLAG_Y | FLAG_X)) | keep;
                4
            }
            0x08 => {   // EX AF, AF'
                let temp_a = self.a;
                let temp_f = self.f;
                self.a = self.a_;
                self.f = self.f_;
                self.a_ = temp_a;
                self.f_ = temp_f;
                4
            }
            0x09|0x19|0x29|0x39 => {    // ADD HL/IX/or IY, rr
                let lhs = self.get_index_reg(index);
                let rhs = match op {
                    0x09 => self.bc(),
                    0x19 => self.de(),
                    0x29 => lhs,
                    _    => self.sp,
                };
                let result = self.add16(lhs, rhs);
                self.set_index_reg(index, result);
                11
            }
            0x0A => {    // LD A, (BC)
                self.a = bus.read(self.bc());
                7
            }
            0x10 => {    // DJNZ d
                let offset = self.fetch_i8(bus);
                self.b = self.b.wrapping_sub(1);
                if self.b != 0 {
                    self.pc = self.pc.wrapping_add_signed(offset as i16);
                    return 13;
                }
                8
            }
            0x11 => {    // LD DE, nn
                self.e = self.fetch_u8(bus);
                self.d = self.fetch_u8(bus);
                10
            }
            0x12 => { bus.write(self.de(), self.a); 7 } // LD (DE), a
            0x18 => {   // JR d
                let disp:i16 = self.fetch_i8(bus) as i16;
                self.pc = self.pc.wrapping_add_signed(disp);
                12
            }
            0x1A => {    // LD A, (DE)
                self.a = bus.read(self.de());
                7
            }
            0x20|0x28|0x30|0x38 => {     // JR cc, d
                let disp:i16 = self.fetch_i8(bus) as i16;
                if self.cond((op >> 3) & 3) {
                    self.pc = self.pc.wrapping_add_signed(disp);
                    return 12;
                }
                7
            }
            0x21 => {    // LD HL, nn
                let v = self.fetch_u16(bus);
                self.set_index_reg(index, v);
                10
            }
            0x22 => {   // LD (nn), HL
                let v = self.get_index_reg(index);
                let addr = self.fetch_u16(bus);
                bus.write(addr, v as u8);
                bus.write(addr.wrapping_add(1), (v >> 8) as u8);
                16
            }
            0x27 => {   // DAA
                let a = self.a;
                let mut adj = 0u8;
                let mut carry = self.f & FLAG_C != 0;
                if self.f & FLAG_H != 0 || a & 0x0F > 9 { adj |= 0x06; }
                if carry || a > 0x99 { adj |= 0x60; carry = true; }
                let r = if self.f & FLAG_N != 0 { a.wrapping_sub(adj) } else { a.wrapping_add(adj) };
                let mut f = (r & (FLAG_S|FLAG_Y|FLAG_X)) | (self.f & FLAG_N);
                if r == 0 { f |= FLAG_Z; }
                if r.count_ones() % 2 == 0 { f |= FLAG_PV; }
                if (a ^ r) & 0x10 != 0 { f |= FLAG_H; }
                if carry { f |= FLAG_C; }
                self.a = r;
                self.f = f;
                4
            }
            0x2A => {   // LD HL, (nn)
                let addr = self.fetch_u16(bus);
                let lo:u16 = bus.read(addr) as u16;
                let hi:u16 = bus.read(addr.wrapping_add(1)) as u16 & 0xFFFF;
                let v = (hi << 8) | lo;
                self.set_index_reg(index, v);
                16
            }
            0x2F => {   // CPL
                self.a = !self.a;
                self.f |= FLAG_H | FLAG_N;
                self.f = (self.f & !(FLAG_Y | FLAG_X)) | (self.a & (FLAG_Y | FLAG_X));
                4
            }
            0x31 => {    // LD HL, nn 
                self.sp = self.fetch_u16(bus);
                10
            }
            0x32 => {   // LD (nn), A
                let addr = self.fetch_u16(bus);
                bus.write(addr, self.a);
                13
            }
            0x37 => {   // SCF
                self.f |= FLAG_C;
                self.f &= !(FLAG_H|FLAG_N);
                4
            }
            0x3A => {   // LD A, (nn)
                let addr = self.fetch_u16(bus);
                self.a = bus.read(addr);
                13
            }
            0x3F => {   // CCF
                let old_c = self.f & FLAG_C != 0;
                self.f &= !FLAG_N;
                if old_c {
                    self.f |= FLAG_H;
                    self.f &= !FLAG_C;
                } else {
                    self.f &= !FLAG_H;
                    self.f |= FLAG_C;
                }
                4
            }
            0x40..=0x75|0x77..=0x7F => {
                let src = op & 7;
                let dst = (op >> 3) & 0x07;
                let r = if src == 6 || dst == 6 { 6 } else { 0 };
                self.op_ld8(op, index, bus);
                match (r, index) {
                    (6, IndexMode::HL) => 7,
                    (6, _) => 15,
                    _ => 4,
                }                
            }
            0x76 => { self.halted = true; 4 }  // HALT
            0x80..=0xBF => {
                let r = op & 7;
                let addr = if r == 6 { self.mem_operand_addr(index, bus) } else { 0 };
                let v = self.read_r(r, index, addr, bus);
                self.alu((op >> 3) & 7, v);
                match (r, index) { (6, IndexMode::HL) => 7, (6, _) => 15, _ => 4 }
            }
            // RET cc
            0xC0|0xC8|0xD0|0xD8|0xE0|0xE8|0xF0|0xF8 => {
                if self.cond(op >> 3) {
                    self.pc = self.pop16(bus);
                    return 11;
                }
                5
            }
            // POP
            0xC1|0xD1|0xE1|0xF1 => {
                let idx = op >> 4;
                let val:u16 = self.pop16(bus);
                self.set_rp2(idx, index, val);
                10
            }
            // JP cc, nn
            0xC2|0xCA|0xD2|0xDA|0xE2|0xEA|0xF2|0xFA => {
                let addr = self.fetch_u16(bus);
                if self.cond(op >> 3) {
                    self.pc = addr;
                }
                return 10;
            }
            // JP nn
            0xC3 => { self.pc = self.fetch_u16(bus); return 10; }
            // CALL cc
            0xC4|0xCC|0xD4|0xDC|0xE4|0xEC|0xF4|0xFC => {
                let addr = self.fetch_u16(bus);
                if self.cond(op >> 3) {
                    self.push16(bus, self.pc);
                    self.pc = addr;
                    return 17;
                }
                10
            }
            // PUSH
            0xC5|0xD5|0xE5|0xF5 => {
                let idx = op >> 4;
                let val = self.get_rp2(idx, index);
                self.push16(bus, val);
                11
            }
            0xC6|0xCE|0xD6|0xDE|0xE6|0xEE|0xF6|0xFE => {
                let n = self.fetch_u8(bus);
                self.alu((op >> 3) & 7, n);
                7
            }
            // RST
            0xC7|0xCF|0xD7|0xDF|0xE7|0xEF|0xF7|0xFF => {
                let target:u8 = op & 0x38;
                self.push16(bus, self.pc);
                self.pc = target as u16;
                11
            }
            // RET
            0xC9 => { self.pc = self.pop16(bus); 10 }
            // CALL nn
            0xCD => {
                let addr = self.fetch_u16(bus);
                self.push16(bus, self.pc);
                self.pc = addr;
                17
            }
            0xD3 => {   // OUT (n), A
                let n = self.fetch_u8(bus);
                let port = ((self.a as u16) << 8) | n as u16;
                bus.port_out(port, self.a);
                11
            }
            0xD9 => {   // EXX (swap BC, DE, and HL with their shadow counterparts)
                let t_b = self.b; let t_c = self.c; let t_d = self.d; let t_e = self.e;
                let t_h = self.h; let t_l = self.l;
                self.b = self.b_; self.c = self.c_; self.d = self.d_; self.e = self.e_;
                self.h = self.h_; self.l = self.l_;
                self.b_ = t_b; self.c_ = t_c; self.d_ = t_d;
                self.e_ = t_e; self.h_ = t_h; self.l_ = t_l;
                4
            }
            0xDB => {   // IN A, (n)
                let n = self.fetch_u8(bus);
                let port = ((self.a as u16) << 8) | n as u16;
                self.a = bus.port_in(port);
                11
            }
            // EX (SP), HL. Exchange (SP) with L and (SP+1) with H
            0xE3 => {
                let val = self.get_index_reg(index);
                let lo = bus.read(self.sp);
                let hi = bus.read(self.sp.wrapping_add(1));
                bus.write(self.sp, val as u8);
                bus.write(self.sp.wrapping_add(1), (val >> 8) as u8);
                self.set_index_reg(index, ((hi as u16) << 8) | lo as u16);
                19
            }
            // JP (HL)
            0xE9 => {
                self.pc = self.get_index_reg(index);
                4
            }
            0xEB => {   // EX DE, HL
                let temp_d = self.d; let temp_e = self.e;
                self.d = self.h; self.e = self.l;
                self.h = temp_d; self.l = temp_e;
                4
            }
            0xF3 => { self.iff1 = false; self.iff2 = false; 4 } // DI
            0xF9 => {   // LD SP, HL
                let v = self.get_index_reg(index);
                self.sp = v;
                6
            }
            0xFB => { self.iff1 = true; self.iff2 = true; self.ei_pending = true; 4 } // EI
            _ => { println!("Unimplemented opcode {:02X}", op); 4 }
        }
    }

    fn exec_cb(&mut self, op: u8, index: IndexMode, disp: i8, bus: &mut dyn Bus) -> u32 {
        let x = op >> 6;
        let y = (op >> 3) & 7;
        let z = op & 7;
        let indexed = index != IndexMode::HL;

        let (r, addr) = if indexed {
            (6, self.get_index_reg(index).wrapping_add_signed(disp as i16))
        } else {
            (z, self.hl())
        };
        let val = self.read_r(r, IndexMode::HL, addr, bus);
        
        if x == 1 {                         // BIT
            self.bit_flags(y, val);
            if indexed {
                self.f = (self.f & !(FLAG_X | FLAG_Y)) | ((addr >> 8) as u8 & (FLAG_X | FLAG_Y));
            }
            return match (indexed, r) { (true, _) => 16, (false, 6) => 12, _ => 8 };
        }

        let res = match x {
            0 => self.rotate_shift(y, val), // RLC RRC RL RR SLA SRA SLL SRL
            2 => val & !(1 << y),           // RES
            _ => val | (1 << y),            // SET
        };
        self.write_r(r, IndexMode::HL, addr, res, bus);
        if indexed && z != 6 {
            self.write_r(z, IndexMode::HL, 0, res, bus);
        }
        match (indexed, r) { (true, _) => 19, (false, 6) => 15, _ => 8 }
    }

    fn exec_ed(&mut self, op: u8, bus: &mut dyn Bus) -> u32 {
        let (x, y, z) = (op >> 6, (op >> 3) & 7, op & 7);
        let (p, q) = (y >> 1, y & 1);
        match (x, z) {
            (1, 0) => {     // IN r, (C)
                let port = self.bc();
                let val = bus.port_in(port);
                if y != 6 { self.write_r(y, IndexMode::HL, 0, val, bus); }
                let mut f = (self.f & FLAG_C) | (val & (FLAG_S | FLAG_X | FLAG_Y));
                if val == 0 { f |= FLAG_Z; }
                if self.check_parity(val) { f |= FLAG_PV; }
                self.f = f;
                12
            },
            (1, 1) => {     // OUT (C), r
                let port = self.bc();
                let val = if y == 6 { 0 } else { self.read_r(y, IndexMode::HL, 0, bus)};
                bus.port_out(port, val);
                12
            },
            (1, 2) => {
                match q {
                    0 => {  // SBC HL, rp[p]
                        let rp = (op >> 4) & 3;
                        let val = self.get_rp(rp, IndexMode::HL);
                        let result = self.sbc16(val);
                        self.set_hl(result);
                        15
                    }
                    _ => {  // ADC HL, rp[p]
                        let rp = (op >> 4) & 3;
                        let val = self.get_rp(rp, IndexMode::HL);
                        let result = self.adc16(val);
                        self.set_hl(result);
                        15
                    }
                }
            }
            (1, 3) => {
                match q {
                    0 => {  // LD (nn), rr
                        let rp = (op >> 4) & 3;
                        let val = self.get_rp(rp, IndexMode::HL);
                        let addr = self.fetch_u16(bus);
                        bus.write(addr, val as u8);
                        bus.write(addr.wrapping_add(1), (val >> 8) as u8);
                        20
                    }
                    _ => {  // LD rr, (nn)
                        let rp = (op >> 4) & 3;
                        let addr = self.fetch_u16(bus);
                        let lo = bus.read(addr);
                        let hi = bus.read(addr.wrapping_add(1));
                        let result = (hi as u16) << 8 | lo as u16;
                        self.set_rp(rp, IndexMode::HL, result);
                        20
                    }
                }
            }
            (1, 4) => {     // NEG (0x44)
                let v = self.a; self.a = 0; self.a = self.sub8(v, 0); 8
            }
            (1, 5) => {     // RETN / RETI (0x45, 0x4D)
                self.pc = self.pop16(bus);
                self.iff1 = self.iff2;
                return 14;
            }
            (1, 6) => { self.im = [0, 0, 1, 2, 0, 0, 1, 2][y as usize]; return 8; }
            (1, 7) => {
                match y {
                    0 => { self.i = self.a; return 9; }
                    1 => { self.r = self.a; return 9; }
                    2|3 => {   // LD A, I | LD A, R
                        self.a = if y == 2 { self.i } else { self.r };
                        let mut f = (self.f & FLAG_C) | (self.a & (FLAG_S | FLAG_Y | FLAG_X));
                        if self.a == 0 { f |= FLAG_Z; }
                        if self.iff2 { f |= FLAG_PV; }
                        self.f = f;
                        9
                    }
                    0b100 => {  // RRD
                        let hl = self.hl();
                        let v = bus.read(hl);
                        let a = self.a;
                        let a_low = a & 0x0F;
                        let m_high = (v & 0xF0) >> 4;
                        let m_low = v & 0x0F;
                        self.a = a & 0xF0 | m_low;
                        let c = self.f & FLAG_C;
                        self.logic_flags(false);
                        self.f |= c;
                        bus.write(hl, (a_low << 4) | m_high);
                        return 18;
                    }
                    0b101 => {  // RLD
                        let hl = self.hl();
                        let v = bus.read(hl);
                        let a = self.a;
                        let a_low = a & 0x0F;
                        let m_high = (v & 0xF0) >> 4;
                        let m_low = v & 0x0F;
                        self.a = a & 0xF0 | m_high;
                        let c = self.f & FLAG_C;
                        self.logic_flags(false);
                        self.f |= c;
                        bus.write(hl, (m_low << 4) | (a & 0x0F));                        
                        return 18;
                    }
                    _ => {
                        return 8;
                    }
                }
            }
            // Block ops
            (2, 0..=3) if y >= 4 => {
                let inc = y & 1 == 0;
                let repeat = y >= 6;
                let again = match z {
                    0 => self.block_ld(inc, bus),
                    1 => self.block_cp(inc, bus),
                    2 => self.block_in(inc, repeat, bus),
                    _ => self.block_out(inc, repeat, bus),
                };
                if repeat && again {
                    self.pc = self.pc.wrapping_sub(2);
                    self.f = (self.f & !(FLAG_Y | FLAG_X)) | ((self.pc >> 8) as u8 & (FLAG_Y | FLAG_X));
                    21
                } else {
                    16
                }
            }
            _ => 8
        }
    }

    fn inc_r(&mut self) {
        self.r = (self.r & 0x80) | self.r.wrapping_add(1) & 0x7F;
    }

    fn rotate_shift(&mut self, op_type: u8, val: u8) -> u8 {
        let carry_in = self.f & FLAG_C;
        
        let (res, carry_out) = match op_type & 7 {
            0 => (val.rotate_left(1), val & 0x80 != 0),    // RLC
            1 => (val.rotate_right(1), val & 0x01 != 0),   // RRC
            2 => ((val << 1) | carry_in, val & 0x80 != 0), // RL
            3 => ((val >> 1) | (carry_in << 7), val & 0x01 != 0), // RR
            4 => (val << 1, val & 0x80 != 0),              // SLA
            5 => ((val >> 1) | (val & 0x80), val & 0x01 != 0), // SRA (keeps sign bit)
            6 => ((val << 1) | 0x01, val & 0x80 != 0),         // SLL (undocumented)
            _ => (val >> 1, val & 0x01 != 0),                  // SRL
        };

        let mut f = res & (FLAG_S | FLAG_Y | FLAG_X);
        if res == 0 { f |= FLAG_Z; }
        if res.count_ones() % 2 == 0 { f |= FLAG_PV; }
        if carry_out { f |= FLAG_C; }
        self.f = f;
        res
    }

    fn bit_flags(&mut self, bit: u8, val: u8) {
        let tested = val & (1 << bit);
        let mut f = (self.f & FLAG_C) | FLAG_H | (val & (FLAG_Y | FLAG_X));
        if tested == 0 { f |= FLAG_Z | FLAG_PV; }
        f |= tested & FLAG_S;
        self.f = f;
    }

    fn logic_flags(&mut self, set_h: bool) {
        let a = self.a;
        let mut f = a & (FLAG_S | FLAG_Y | FLAG_X);
        if a == 0 { f |= FLAG_Z; }
        if a.count_ones() % 2 == 0 { f |= FLAG_PV; }
        if set_h { f |= FLAG_H; }
        self.f = f;
    }

    fn op_ld8(&mut self, op: u8, index: IndexMode, bus: &mut dyn Bus) {
        let src = op & 0x07;
        let dst = (op >> 3) & 0x07;
        let (addr, halves) = if dst == 6 || src == 6 {
            (self.mem_operand_addr(index, bus), IndexMode::HL)
        } else {
            (0, index)
        };
        let val = self.read_r(src, halves, addr, bus);
        self.write_r(dst, halves, addr, val, bus);
    }

    fn op_rl(&mut self, through_carry: bool) {
        let a:u8 = self.a;
        let old_carry = self.f & FLAG_C;
        let new_carry = a & 0x80 != 0;

        let mut result = 0;
        if through_carry {
            result = (a << 1) | old_carry;
        } else {
            result = a << 1 | a >> 7;
        }
        if new_carry {
            self.f |= FLAG_C;
        } else {
            self.f &= !FLAG_C;
        }
        self.f &= !FLAG_H;
        self.f &= !FLAG_N;
        self.a = result;
    }

    fn inc8(&mut self, p_val: u8) -> u8 {
        let ret_val:u8 = p_val.wrapping_add(1);
        let mut new_f:u8 = self.f & FLAG_C;
        if ret_val == 0 { new_f |= FLAG_Z; }
        if ret_val & 0x80 != 0 { new_f |= FLAG_S; }
        if p_val & 0x0F == 0x0F { new_f |= FLAG_H; }
        if p_val == 0x7F { new_f |= FLAG_PV; }
        new_f |= ret_val & (FLAG_X | FLAG_Y);
        self.f = new_f;
        ret_val
    }

    fn alu(&mut self, op: u8, v: u8) {
        let c = self.f & FLAG_C;
        match op {
            0 => self.a = self.add8(v, 0),
            1 => self.a = self.add8(v, c),
            2 => self.a = self.sub8(v, 0),
            3 => self.a = self.sub8(v, c),
            4 => { self.a &= v; self.logic_flags(true); },
            5 => { self.a ^= v; self.logic_flags(false); },
            6 => { self.a |= v; self.logic_flags(false); },
            _ => self.cp8(v),
        }
    }

    fn dec8(&mut self, p_val: u8) -> u8 {
        let ret_val:u8 = p_val.wrapping_sub(1);
        let mut new_f:u8 = (self.f & FLAG_C) | FLAG_N;
        if ret_val == 0 { new_f |= FLAG_Z; }
        if ret_val & 0x80 != 0 { new_f |= FLAG_S; }
        if p_val & 0x0F == 0x00 { new_f |= FLAG_H; }
        if p_val == 0x80 { new_f |= FLAG_PV; }
        new_f |= ret_val & (FLAG_X | FLAG_Y);
        self.f = new_f;
        return ret_val;
    }

    // using this function for both SUB and CP
    // so return result that SUB can assign to A register and CP can ignore.
    fn sub8(&mut self, v: u8, carry: u8) -> u8 {
        let a: u8 = self.a;
        let wide = (a as u16).wrapping_sub(v as u16).wrapping_sub(carry as u16);
        let r = wide as u8;
        let mut f = FLAG_N | (r & (FLAG_S | FLAG_X | FLAG_Y));
        if r == 0 { f |= FLAG_Z; }
        if (a ^ v ^ r) & 0x10 != 0 { f |= FLAG_H; }
        if (a ^ v) & (a ^ r) & 0x80 != 0 { f |= FLAG_PV; }
        if wide & 0x100 != 0 { f |= FLAG_C; }
        self.f = f;
        r
    }

    fn add8(&mut self, v: u8, carry: u8) -> u8 {
        let a: u8 = self.a;
        let wide = (a as u16).wrapping_add(v as u16).wrapping_add(carry as u16);
        let r = wide as u8;
        let mut f = r & (FLAG_S | FLAG_X | FLAG_Y);
        if r == 0 { f |= FLAG_Z; }
        if (a ^ v ^ r) & 0x10 != 0 { f |= FLAG_H; }
        if !(a ^ v) & (a ^ r) & 0x80 != 0 { f |= FLAG_PV; }
        if wide & 0x100 != 0 { f |= FLAG_C; }
        self.f = f;
        r
    }

    fn cp8(&mut self, p_val: u8) {
        self.sub8(p_val, 0);
        self.f = (self.f & !(FLAG_X | FLAG_Y)) | (p_val & (FLAG_X | FLAG_Y));
    }

    fn add16(&mut self, a: u16, b: u16) -> u16 {
        let (result, carry) = a.overflowing_add(b);
        let mut f = self.f & (FLAG_S | FLAG_Z | FLAG_PV);
        if ((a ^ b ^ result) & 0x1000) != 0 { f |= FLAG_H; }
        if carry { f |= FLAG_C; }
        f |= ((result >> 8) as u8) & (FLAG_X | FLAG_Y);
        self.f = f;
        return result
    }

    fn adc16(&mut self, v: u16) -> u16 {
        let hl = self.hl();
        let wide = hl as u32 + v as u32 + (self.f & FLAG_C) as u32;
        let r = wide as u16;
        let mut f = ((r >> 8) as u8) & (FLAG_S | FLAG_Y | FLAG_X);
        if r == 0 { f |= FLAG_Z; }
        if (hl ^ v ^ r) & 0x1000 != 0 { f |= FLAG_H; }
        if !(hl ^ v) & (hl ^ r) & 0x8000 != 0 { f |= FLAG_PV; }
        if wide > 0xFFFF { f |= FLAG_C; } 
        self.f = f;
        r
    }

    fn sbc16(&mut self, v: u16) -> u16 {
        let hl: u16 = self.hl();
        let carry = if self.f & FLAG_C != 0 { 1 } else { 0 };
        let wide = (hl as u32).wrapping_sub(v as u32).wrapping_sub(carry as u32);
        let r = wide as u16;
        let mut f = FLAG_N | ((r >> 8) as u8 & (FLAG_X | FLAG_Y)) as u8;
        if r == 0 { f |= FLAG_Z; }
        if r & 0x8000 != 0 { f |= FLAG_S; }
        if (hl ^ v ^ r) & 0x1000 != 0 { f |= FLAG_H; }
        if (hl ^ v) & (hl ^ r) & 0x8000 != 0 { f |= FLAG_PV; }
        if wide & 0x10000 != 0 { f |= FLAG_C; }
        self.f = f;
        r
    }

    fn set_flag(&mut self, flag: u8, cond: bool) {
        if cond {
            self.f |= flag;
        } else {
            self.f &= !flag;
        }
    }
}

pub trait Bus {
    fn read(&mut self, addr: u16) -> u8;
    fn write(&mut self, addr: u16, val: u8);
    fn port_in(&mut self, port: u16) -> u8;
    fn port_out(&mut self, port: u16, val: u8); 
}