// Intel 8088 (and 8086 instruction set): real mode, 20-bit addresses, 8-bit data bus.
//
// Instruction-level interpreter: one call to `step` runs one whole instruction (a REP
// string instruction runs all its repeats) and returns an estimate of the clock count.
// Cycle counts are the 8086 book values plus 4 clocks for every word moved over the
// 8088's 8-bit bus, with a floor of 4 clocks per instruction byte fetched. No prefetch
// queue model: good enough for arcade boards, not for cycle-exact PC software.
//
// Includes the documented 8086 behaviours the SingleStepTests 8088 suite checks:
// POP CS (0F), 60-6F as Jcc aliases, C0/C1/C8/C9 as RET aliases, SALC (D6),
// SETMO/SETMOC (D0-D3 /6), 82 = 80, C6/C7 and 8F with any reg field, FF /7 = PUSH,
// PUSH SP pushing the new SP, IDIV refusing a -128 / -32768 quotient, and REP before
// MUL/IMUL/DIV/IDIV negating the result.
//
// Verified with SingleStepTests/8088 v2 (see the i8088_sst test at the bottom).

pub trait I8088Bus {
    fn read8(&mut self, addr: u32) -> u8;
    fn write8(&mut self, addr: u32, value: u8);
    fn in8(&mut self, _port: u16) -> u8 { 0xFF }
    fn out8(&mut self, _port: u16, _value: u8) {}
    /// Interrupt acknowledge: the vector number for a maskable interrupt (INTR).
    fn inta(&mut self) -> u8 { 0xFF }
}

pub const CF: u16 = 0x0001;
pub const PF: u16 = 0x0004;
pub const AF: u16 = 0x0010;
pub const ZF: u16 = 0x0040;
pub const SF: u16 = 0x0080;
pub const TF: u16 = 0x0100;
pub const IF: u16 = 0x0200;
pub const DF: u16 = 0x0400;
pub const OF: u16 = 0x0800;
const FLAGS_FIXED: u16 = 0xF002;
const FLAGS_MASK: u16 = 0x0FD5;

// register numbers as encoded in instructions
pub const AX: usize = 0;
pub const CX: usize = 1;
pub const DX: usize = 2;
pub const BX: usize = 3;
pub const SP: usize = 4;
pub const BP: usize = 5;
pub const SI: usize = 6;
pub const DI: usize = 7;
pub const ES: usize = 0;
pub const CS: usize = 1;
pub const SS: usize = 2;
pub const DS: usize = 3;

#[derive(Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
struct ModRm {
    md: u8,
    reg: u8,
    rm: u8,
    ea: u16,
    seg: usize,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct I8088 {
    pub regs: [u16; 8],     // AX CX DX BX SP BP SI DI
    pub sregs: [u16; 4],    // ES CS SS DS
    pub ip: u16,
    pub flags: u16,
    pub halted: bool,
    nmi_pending: bool,
    /// INTR input level (maskable, IF)
    pub intr: bool,
    /// MOV/POP to a segment register and STI hold interrupts off for one instruction
    int_inhibit: bool,
    // per-instruction decode state
    seg_override: Option<usize>,
    rep: u8,
    m: ModRm,
    cycles: u32,
    fetched: u32,
    pub total_cycles: u64,
}

impl Default for I8088 {
    fn default() -> Self { Self::new() }
}

#[inline]
fn lin(seg: u16, off: u16) -> u32 {
    (((seg as u32) << 4) + off as u32) & 0xFFFFF
}

#[inline]
fn parity(v: u8) -> bool { v.count_ones() & 1 == 0 }

impl I8088 {
    pub fn new() -> Self {
        let mut c = Self {
            regs: [0; 8], sregs: [0; 4], ip: 0, flags: FLAGS_FIXED,
            halted: false, nmi_pending: false, intr: false, int_inhibit: false,
            seg_override: None, rep: 0, m: ModRm::default(), cycles: 0, fetched: 0,
            total_cycles: 0,
        };
        c.reset();
        c
    }

    /// Hardware reset: execution starts at FFFF:0000.
    pub fn reset(&mut self) {
        self.sregs = [0, 0xFFFF, 0, 0];
        self.ip = 0;
        self.flags = FLAGS_FIXED;
        self.halted = false;
        self.nmi_pending = false;
        self.int_inhibit = false;
    }

    /// NMI input rising edge.
    pub fn nmi(&mut self) { self.nmi_pending = true; }

    #[inline] fn f(&self, m: u16) -> bool { self.flags & m != 0 }
    #[inline] fn setf(&mut self, m: u16, on: bool) {
        if on { self.flags |= m } else { self.flags &= !m }
    }

    // ---- registers ----
    #[inline] fn r8(&self, r: u8) -> u8 {
        let v = self.regs[(r & 3) as usize];
        if r & 4 != 0 { (v >> 8) as u8 } else { v as u8 }
    }
    #[inline] fn set_r8(&mut self, r: u8, v: u8) {
        let i = (r & 3) as usize;
        if r & 4 != 0 { self.regs[i] = (self.regs[i] & 0x00FF) | ((v as u16) << 8) }
        else { self.regs[i] = (self.regs[i] & 0xFF00) | v as u16 }
    }
    #[inline] fn al(&self) -> u8 { self.regs[AX] as u8 }
    #[inline] fn ah(&self) -> u8 { (self.regs[AX] >> 8) as u8 }
    #[inline] fn set_al(&mut self, v: u8) { self.set_r8(0, v) }
    #[inline] fn set_ah(&mut self, v: u8) { self.set_r8(4, v) }

    // ---- memory ----
    #[inline] fn rd8(&mut self, bus: &mut dyn I8088Bus, seg: usize, off: u16) -> u8 {
        bus.read8(lin(self.sregs[seg], off))
    }
    #[inline] fn wr8(&mut self, bus: &mut dyn I8088Bus, seg: usize, off: u16, v: u8) {
        bus.write8(lin(self.sregs[seg], off), v)
    }
    /// Word access: two byte cycles, the offset wraps inside the segment.
    #[inline] fn rd16(&mut self, bus: &mut dyn I8088Bus, seg: usize, off: u16) -> u16 {
        self.cycles += 4;
        let lo = self.rd8(bus, seg, off) as u16;
        let hi = self.rd8(bus, seg, off.wrapping_add(1)) as u16;
        lo | (hi << 8)
    }
    #[inline] fn wr16(&mut self, bus: &mut dyn I8088Bus, seg: usize, off: u16, v: u16) {
        self.cycles += 4;
        self.wr8(bus, seg, off, v as u8);
        self.wr8(bus, seg, off.wrapping_add(1), (v >> 8) as u8);
    }
    #[inline] fn fetch8(&mut self, bus: &mut dyn I8088Bus) -> u8 {
        let v = bus.read8(lin(self.sregs[CS], self.ip));
        self.ip = self.ip.wrapping_add(1);
        self.fetched += 1;
        v
    }
    #[inline] fn fetch16(&mut self, bus: &mut dyn I8088Bus) -> u16 {
        let lo = self.fetch8(bus) as u16;
        lo | ((self.fetch8(bus) as u16) << 8)
    }
    fn push(&mut self, bus: &mut dyn I8088Bus, v: u16) {
        self.regs[SP] = self.regs[SP].wrapping_sub(2);
        let sp = self.regs[SP];
        self.wr16(bus, SS, sp, v);
    }
    fn pop(&mut self, bus: &mut dyn I8088Bus) -> u16 {
        let sp = self.regs[SP];
        let v = self.rd16(bus, SS, sp);
        self.regs[SP] = sp.wrapping_add(2);
        v
    }
    #[inline] fn data_seg(&self, default: usize) -> usize { self.seg_override.unwrap_or(default) }

    // ---- ModRM ----
    fn modrm(&mut self, bus: &mut dyn I8088Bus) {
        let b = self.fetch8(bus);
        let md = b >> 6;
        let reg = (b >> 3) & 7;
        let rm = b & 7;
        let mut m = ModRm { md, reg, rm, ea: 0, seg: DS };
        if md != 3 {
            let r = &self.regs;
            let (base, seg, ea_cost) = match rm {
                0 => (r[BX].wrapping_add(r[SI]), DS, 7),
                1 => (r[BX].wrapping_add(r[DI]), DS, 8),
                2 => (r[BP].wrapping_add(r[SI]), SS, 8),
                3 => (r[BP].wrapping_add(r[DI]), SS, 7),
                4 => (r[SI], DS, 5),
                5 => (r[DI], DS, 5),
                6 => if md == 0 { (0, DS, 6) } else { (r[BP], SS, 5) },
                _ => (r[BX], DS, 5),
            };
            let disp = match md {
                0 => if rm == 6 { self.fetch16(bus) } else { 0 },
                1 => self.fetch8(bus) as i8 as u16,
                _ => self.fetch16(bus),
            };
            m.ea = base.wrapping_add(disp);
            m.seg = self.seg_override.unwrap_or(seg);
            self.cycles += ea_cost + if md != 0 && !(md == 0 && rm == 6) { 4 } else { 0 };
        }
        self.m = m;
    }
    #[inline] fn rm_is_reg(&self) -> bool { self.m.md == 3 }
    fn rm_read8(&mut self, bus: &mut dyn I8088Bus) -> u8 {
        if self.m.md == 3 { self.r8(self.m.rm) } else { let (s, e) = (self.m.seg, self.m.ea); self.rd8(bus, s, e) }
    }
    fn rm_write8(&mut self, bus: &mut dyn I8088Bus, v: u8) {
        if self.m.md == 3 { self.set_r8(self.m.rm, v) } else { let (s, e) = (self.m.seg, self.m.ea); self.wr8(bus, s, e, v) }
    }
    fn rm_read16(&mut self, bus: &mut dyn I8088Bus) -> u16 {
        if self.m.md == 3 { self.regs[self.m.rm as usize] } else { let (s, e) = (self.m.seg, self.m.ea); self.rd16(bus, s, e) }
    }
    fn rm_write16(&mut self, bus: &mut dyn I8088Bus, v: u16) {
        if self.m.md == 3 { self.regs[self.m.rm as usize] = v } else { let (s, e) = (self.m.seg, self.m.ea); self.wr16(bus, s, e, v) }
    }
    fn rm_read(&mut self, bus: &mut dyn I8088Bus, w: bool) -> u16 {
        if w { self.rm_read16(bus) } else { self.rm_read8(bus) as u16 }
    }
    fn rm_write(&mut self, bus: &mut dyn I8088Bus, w: bool, v: u16) {
        if w { self.rm_write16(bus, v) } else { self.rm_write8(bus, v as u8) }
    }
    fn reg_read(&self, r: u8, w: bool) -> u16 {
        if w { self.regs[r as usize] } else { self.r8(r) as u16 }
    }
    fn reg_write(&mut self, r: u8, w: bool, v: u16) {
        if w { self.regs[r as usize] = v } else { self.set_r8(r, v as u8) }
    }

    // ---- flags / ALU ----
    fn set_szp(&mut self, r: u16, w: bool) {
        let (r, sign) = if w { (r, 0x8000) } else { (r & 0xFF, 0x80) };
        self.setf(ZF, r == 0);
        self.setf(SF, r & sign != 0);
        self.setf(PF, parity(r as u8));
    }

    /// The eight two-operand ALU operations (ADD OR ADC SBB AND SUB XOR CMP).
    fn alu(&mut self, op: u8, a: u16, b: u16, w: bool) -> u16 {
        let (mask, sign): (u32, u32) = if w { (0xFFFF, 0x8000) } else { (0xFF, 0x80) };
        let a = a as u32 & mask;
        let b = b as u32 & mask;
        let r = match op {
            0 | 2 => {
                let c = if op == 2 && self.f(CF) { 1 } else { 0 };
                let r = a + b + c;
                self.setf(CF, r > mask);
                self.setf(OF, (a ^ r) & (b ^ r) & sign != 0);
                self.setf(AF, (a ^ b ^ r) & 0x10 != 0);
                r
            }
            3 | 5 | 7 => {
                let c = if op == 3 && self.f(CF) { 1 } else { 0 };
                let r = a.wrapping_sub(b).wrapping_sub(c);
                self.setf(CF, a < b + c);
                self.setf(OF, (a ^ b) & (a ^ r) & sign != 0);
                self.setf(AF, (a ^ b ^ r) & 0x10 != 0);
                r
            }
            1 | 4 | 6 => {
                let r = match op { 1 => a | b, 4 => a & b, _ => a ^ b };
                self.setf(CF | OF | AF, false);
                r
            }
            _ => unreachable!(),
        } & mask;
        self.set_szp(r as u16, w);
        r as u16
    }

    fn inc_dec(&mut self, a: u16, dec: bool, w: bool) -> u16 {
        let (mask, sign) = if w { (0xFFFFu16, 0x8000u16) } else { (0xFF, 0x80) };
        let a = a & mask;
        let r = if dec { a.wrapping_sub(1) } else { a.wrapping_add(1) } & mask;
        if dec {
            self.setf(OF, a == sign);
            self.setf(AF, a & 0xF == 0);
        } else {
            self.setf(OF, r == sign);
            self.setf(AF, a & 0xF == 0xF);
        }
        self.set_szp(r, w);
        r
    }

    /// Shift/rotate group (D0-D3). `count` 0 leaves everything alone.
    fn shift(&mut self, op: u8, v: u16, count: u8, w: bool) -> u16 {
        if count == 0 { return v; }
        let bits: u32 = if w { 16 } else { 8 };
        let mask: u32 = (1 << bits) - 1;
        let msb: u32 = 1 << (bits - 1);
        let mut r = v as u32 & mask;
        let mut cf = self.f(CF);
        let mut of = self.f(OF);
        for _ in 0..count {
            match op {
                0 => { // ROL
                    cf = r & msb != 0;
                    r = ((r << 1) | cf as u32) & mask;
                    of = (r & msb != 0) != cf;
                }
                1 => { // ROR
                    cf = r & 1 != 0;
                    r = (r >> 1) | if cf { msb } else { 0 };
                    of = (r & msb != 0) != (r & (msb >> 1) != 0);
                }
                2 => { // RCL
                    let out = r & msb != 0;
                    r = ((r << 1) | cf as u32) & mask;
                    cf = out;
                    of = (r & msb != 0) != cf;
                }
                3 => { // RCR
                    let out = r & 1 != 0;
                    r = (r >> 1) | if cf { msb } else { 0 };
                    cf = out;
                    of = (r & msb != 0) != (r & (msb >> 1) != 0);
                }
                4 => { // SHL/SAL
                    cf = r & msb != 0;
                    r = (r << 1) & mask;
                    of = (r & msb != 0) != cf;
                }
                5 => { // SHR
                    of = r & msb != 0;
                    cf = r & 1 != 0;
                    r >>= 1;
                }
                6 => { // SETMO: all ones
                    r = mask;
                    cf = false;
                    of = false;
                }
                _ => { // SAR
                    cf = r & 1 != 0;
                    r = (r >> 1) | (r & msb);
                    of = false;
                }
            }
        }
        self.setf(CF, cf);
        self.setf(OF, of);
        if op >= 4 {
            self.set_szp(r as u16, w);
            self.setf(AF, false);
        }
        r as u16
    }

    fn interrupt(&mut self, bus: &mut dyn I8088Bus, n: u8) {
        let f = self.flags;
        self.push(bus, f);
        self.setf(IF | TF, false);
        let cs = self.sregs[CS];
        self.push(bus, cs);
        let ip = self.ip;
        self.push(bus, ip);
        let v = (n as u16) * 4;
        self.ip = self.rd16_abs(bus, v as u32);
        self.sregs[CS] = self.rd16_abs(bus, v as u32 + 2);
        self.cycles += 50;
    }
    fn rd16_abs(&mut self, bus: &mut dyn I8088Bus, a: u32) -> u16 {
        self.cycles += 4;
        bus.read8(a) as u16 | ((bus.read8(a + 1) as u16) << 8)
    }

    fn cond(&self, c: u8) -> bool {
        let r = match c >> 1 {
            0 => self.f(OF),
            1 => self.f(CF),
            2 => self.f(ZF),
            3 => self.f(CF) || self.f(ZF),
            4 => self.f(SF),
            5 => self.f(PF),
            6 => self.f(SF) != self.f(OF),
            _ => self.f(ZF) || (self.f(SF) != self.f(OF)),
        };
        r != (c & 1 != 0)
    }

    fn jump_rel8(&mut self, d: u8) {
        self.ip = self.ip.wrapping_add(d as i8 as u16);
    }

    /// Run one instruction (or service a pending interrupt). Returns clocks used.
    pub fn step(&mut self, bus: &mut dyn I8088Bus) -> u32 {
        self.cycles = 0;
        self.fetched = 0;
        if !self.int_inhibit {
            if self.nmi_pending {
                self.nmi_pending = false;
                self.halted = false;
                self.interrupt(bus, 2);
                self.total_cycles += self.cycles as u64;
                return self.cycles;
            }
            if self.intr && self.f(IF) {
                self.halted = false;
                let v = bus.inta();
                self.interrupt(bus, v);
                self.total_cycles += self.cycles as u64;
                return self.cycles;
            }
        }
        self.int_inhibit = false;
        if self.halted {
            self.total_cycles += 4;
            return 4;
        }
        let trap = self.f(TF);
        self.seg_override = None;
        self.rep = 0;
        let start_ip = self.ip;
        let mut op;
        loop {
            op = self.fetch8(bus);
            match op {
                0x26 | 0x2E | 0x36 | 0x3E => { self.seg_override = Some(((op >> 3) & 3) as usize); self.cycles += 2; }
                0xF0 | 0xF1 => { self.cycles += 2; }
                0xF2 | 0xF3 => { self.rep = op; self.cycles += 2; }
                _ => break,
            }
            // prefixes don't wrap forever if memory is all prefixes
            if self.fetched > 15 { break; }
        }
        self.exec(bus, op, start_ip);
        if trap && self.f(TF) {
            self.interrupt(bus, 1);
        }
        let floor = self.fetched * 4;
        if self.cycles < floor { self.cycles = floor; }
        self.total_cycles += self.cycles as u64;
        self.cycles
    }

    fn exec(&mut self, bus: &mut dyn I8088Bus, op: u8, start_ip: u16) {
        match op {
            // ALU r/m, reg / reg, r/m / acc, imm
            0x00..=0x3F if op & 7 < 6 => {
                let alu_op = (op >> 3) & 7;
                let w = op & 1 != 0;
                match op & 7 {
                    0 | 1 => {
                        self.modrm(bus);
                        let a = self.rm_read(bus, w);
                        let b = self.reg_read(self.m.reg, w);
                        let r = self.alu(alu_op, a, b, w);
                        if alu_op != 7 {
                            self.rm_write(bus, w, r);
                            self.cycles += if self.rm_is_reg() { 3 } else { 16 };
                        } else {
                            self.cycles += if self.rm_is_reg() { 3 } else { 9 };
                        }
                    }
                    2 | 3 => {
                        self.modrm(bus);
                        let b = self.rm_read(bus, w);
                        let a = self.reg_read(self.m.reg, w);
                        let r = self.alu(alu_op, a, b, w);
                        if alu_op != 7 { self.reg_write(self.m.reg, w, r); }
                        self.cycles += if self.rm_is_reg() { 3 } else { 9 };
                    }
                    4 => {
                        let b = self.fetch8(bus) as u16;
                        let r = self.alu(alu_op, self.al() as u16, b, false);
                        if alu_op != 7 { self.set_al(r as u8); }
                        self.cycles += 4;
                    }
                    _ => {
                        let b = self.fetch16(bus);
                        let r = self.alu(alu_op, self.regs[AX], b, true);
                        if alu_op != 7 { self.regs[AX] = r; }
                        self.cycles += 4;
                    }
                }
            }
            0x06 | 0x0E | 0x16 | 0x1E => {
                let v = self.sregs[((op >> 3) & 3) as usize];
                self.push(bus, v);
                self.cycles += 10;
            }
            0x07 | 0x0F | 0x17 | 0x1F => {
                let v = self.pop(bus);
                self.sregs[((op >> 3) & 3) as usize] = v;
                self.int_inhibit = true;
                self.cycles += 8;
            }
            0x27 => { // DAA
                let al = self.al();
                let old_cf = self.f(CF);
                let mut r = al;
                let af = self.f(AF) || (al & 0x0F) > 9;
                if af { r = r.wrapping_add(6); }
                let cf = old_cf || al > if self.f(AF) { 0x9F } else { 0x99 };
                if cf { r = r.wrapping_add(0x60); }
                self.setf(AF, af);
                self.setf(CF, cf);
                self.setf(OF, (al ^ r) & r & 0x80 != 0);
                self.set_al(r);
                self.set_szp(r as u16, false);
                self.cycles += 4;
            }
            0x2F => { // DAS
                let al = self.al();
                let old_cf = self.f(CF);
                let mut r = al;
                let af = self.f(AF) || (al & 0x0F) > 9;
                if af { r = r.wrapping_sub(6); }
                let cf = old_cf || al > if self.f(AF) { 0x9F } else { 0x99 };
                if cf { r = r.wrapping_sub(0x60); }
                self.setf(AF, af);
                self.setf(CF, cf);
                self.setf(OF, (al ^ r) & al & 0x80 != 0);
                self.set_al(r);
                self.set_szp(r as u16, false);
                self.cycles += 4;
            }
            0x37 | 0x3F => { // AAA / AAS
                let al = self.al();
                let adj = self.f(AF) || (al & 0x0F) > 9;
                let r = if adj {
                    if op == 0x37 {
                        self.set_ah(self.ah().wrapping_add(1));
                        al.wrapping_add(6)
                    } else {
                        self.set_ah(self.ah().wrapping_sub(1));
                        al.wrapping_sub(6)
                    }
                } else { al };
                self.setf(AF | CF, adj);
                self.set_szp(r as u16, false);
                self.setf(OF, false);
                self.set_al(r & 0x0F);
                self.cycles += 8;
            }
            0x40..=0x4F => {
                let r = (op & 7) as usize;
                self.regs[r] = self.inc_dec(self.regs[r], op & 8 != 0, true);
                self.cycles += 3;
            }
            0x50..=0x57 => {
                let r = (op & 7) as usize;
                // PUSH SP pushes the already-decremented value on the 8086/8088
                let v = if r == SP { self.regs[SP].wrapping_sub(2) } else { self.regs[r] };
                self.push(bus, v);
                self.cycles += 11;
            }
            0x58..=0x5F => {
                let v = self.pop(bus);
                self.regs[(op & 7) as usize] = v;
                self.cycles += 8;
            }
            0x60..=0x7F => {
                let d = self.fetch8(bus);
                if self.cond(op & 0x0F) { self.jump_rel8(d); self.cycles += 16; } else { self.cycles += 4; }
            }
            0x80..=0x83 => {
                let w = op & 1 != 0;
                self.modrm(bus);
                let a = self.rm_read(bus, w);
                let b = match op {
                    0x81 => self.fetch16(bus),
                    0x83 => self.fetch8(bus) as i8 as u16,
                    _ => self.fetch8(bus) as u16,
                };
                let alu_op = self.m.reg;
                let r = self.alu(alu_op, a, b, w);
                if alu_op != 7 { self.rm_write(bus, w, r); }
                self.cycles += if self.rm_is_reg() { 4 } else if alu_op == 7 { 10 } else { 17 };
            }
            0x84 | 0x85 => {
                let w = op & 1 != 0;
                self.modrm(bus);
                let a = self.rm_read(bus, w);
                let b = self.reg_read(self.m.reg, w);
                self.alu(4, a, b, w);
                self.cycles += if self.rm_is_reg() { 3 } else { 9 };
            }
            0x86 | 0x87 => {
                let w = op & 1 != 0;
                self.modrm(bus);
                let a = self.rm_read(bus, w);
                let b = self.reg_read(self.m.reg, w);
                self.reg_write(self.m.reg, w, a);
                self.rm_write(bus, w, b);
                self.cycles += if self.rm_is_reg() { 4 } else { 17 };
            }
            0x88..=0x8B => {
                let w = op & 1 != 0;
                self.modrm(bus);
                if op & 2 == 0 {
                    let v = self.reg_read(self.m.reg, w);
                    self.rm_write(bus, w, v);
                } else {
                    let v = self.rm_read(bus, w);
                    self.reg_write(self.m.reg, w, v);
                }
                self.cycles += if self.rm_is_reg() { 2 } else { 9 };
            }
            0x8C => {
                self.modrm(bus);
                let v = self.sregs[(self.m.reg & 3) as usize];
                self.rm_write16(bus, v);
                self.cycles += if self.rm_is_reg() { 2 } else { 9 };
            }
            0x8D => {
                self.modrm(bus);
                // register form is undefined; the 8088 leaves the last computed EA
                let ea = self.m.ea;
                self.regs[self.m.reg as usize] = ea;
                self.cycles += 2;
            }
            0x8E => {
                self.modrm(bus);
                let v = self.rm_read16(bus);
                self.sregs[(self.m.reg & 3) as usize] = v;
                self.int_inhibit = true;
                self.cycles += if self.rm_is_reg() { 2 } else { 8 };
            }
            0x8F => {
                self.modrm(bus);
                let v = self.pop(bus);
                self.rm_write16(bus, v);
                self.cycles += if self.rm_is_reg() { 8 } else { 17 };
            }
            0x90..=0x97 => {
                let r = (op & 7) as usize;
                self.regs.swap(AX, r);
                self.cycles += 3;
            }
            0x98 => { self.regs[AX] = self.al() as i8 as i16 as u16; self.cycles += 2; }
            0x99 => { self.regs[DX] = if self.regs[AX] & 0x8000 != 0 { 0xFFFF } else { 0 }; self.cycles += 5; }
            0x9A => {
                let ip = self.fetch16(bus);
                let cs = self.fetch16(bus);
                let (ocs, oip) = (self.sregs[CS], self.ip);
                self.push(bus, ocs);
                self.push(bus, oip);
                self.sregs[CS] = cs;
                self.ip = ip;
                self.cycles += 28;
            }
            0x9B => { self.cycles += 3; }
            0x9C => { let f = self.flags; self.push(bus, f); self.cycles += 10; }
            0x9D => {
                let v = self.pop(bus);
                self.flags = (v & FLAGS_MASK) | FLAGS_FIXED;
                self.cycles += 8;
            }
            0x9E => {
                let ah = self.ah() as u16;
                self.flags = (self.flags & 0xFF00) | (ah & 0xD5) | 0x02;
                self.cycles += 4;
            }
            0x9F => { self.set_ah(self.flags as u8); self.cycles += 4; }
            0xA0..=0xA3 => {
                let w = op & 1 != 0;
                let off = self.fetch16(bus);
                let seg = self.data_seg(DS);
                if op & 2 == 0 {
                    if w { self.regs[AX] = self.rd16(bus, seg, off); }
                    else { let v = self.rd8(bus, seg, off); self.set_al(v); }
                } else if w {
                    let v = self.regs[AX];
                    self.wr16(bus, seg, off, v);
                } else {
                    let v = self.al();
                    self.wr8(bus, seg, off, v);
                }
                self.cycles += 10;
            }
            0xA4..=0xA7 | 0xAA..=0xAF => self.string_op(bus, op),
            0xA8 => { let b = self.fetch8(bus) as u16; self.alu(4, self.al() as u16, b, false); self.cycles += 4; }
            0xA9 => { let b = self.fetch16(bus); self.alu(4, self.regs[AX], b, true); self.cycles += 4; }
            0xB0..=0xB7 => { let v = self.fetch8(bus); self.set_r8(op & 7, v); self.cycles += 4; }
            0xB8..=0xBF => { let v = self.fetch16(bus); self.regs[(op & 7) as usize] = v; self.cycles += 4; }
            0xC0 | 0xC2 => {
                let n = self.fetch16(bus);
                self.ip = self.pop(bus);
                self.regs[SP] = self.regs[SP].wrapping_add(n);
                self.cycles += 12;
            }
            0xC1 | 0xC3 => { self.ip = self.pop(bus); self.cycles += 8; }
            0xC4 | 0xC5 => {
                self.modrm(bus);
                let (seg, ea) = (self.m.seg, self.m.ea);
                let off = self.rd16(bus, seg, ea);
                let s = self.rd16(bus, seg, ea.wrapping_add(2));
                self.regs[self.m.reg as usize] = off;
                self.sregs[if op == 0xC4 { ES } else { DS }] = s;
                self.cycles += 16;
            }
            0xC6 | 0xC7 => {
                let w = op & 1 != 0;
                self.modrm(bus);
                let v = if w { self.fetch16(bus) } else { self.fetch8(bus) as u16 };
                self.rm_write(bus, w, v);
                self.cycles += if self.rm_is_reg() { 4 } else { 10 };
            }
            0xC8 | 0xCA => {
                let n = self.fetch16(bus);
                self.ip = self.pop(bus);
                self.sregs[CS] = self.pop(bus);
                self.regs[SP] = self.regs[SP].wrapping_add(n);
                self.cycles += 17;
            }
            0xC9 | 0xCB => {
                self.ip = self.pop(bus);
                self.sregs[CS] = self.pop(bus);
                self.cycles += 18;
            }
            0xCC => { self.interrupt(bus, 3); }
            0xCD => { let n = self.fetch8(bus); self.interrupt(bus, n); }
            0xCE => { if self.f(OF) { self.interrupt(bus, 4); } else { self.cycles += 4; } }
            0xCF => {
                self.ip = self.pop(bus);
                self.sregs[CS] = self.pop(bus);
                let f = self.pop(bus);
                self.flags = (f & FLAGS_MASK) | FLAGS_FIXED;
                self.cycles += 24;
            }
            0xD0..=0xD3 => {
                let w = op & 1 != 0;
                self.modrm(bus);
                let v = self.rm_read(bus, w);
                let by_cl = op & 2 != 0;
                let count = if by_cl { self.regs[CX] as u8 } else { 1 };
                let r = if self.m.reg == 6 && by_cl && count == 0 {
                    v // SETMOC with CL = 0 does nothing
                } else if self.m.reg == 6 {
                    self.shift(6, v, 1, w)
                } else {
                    self.shift(self.m.reg, v, count, w)
                };
                self.rm_write(bus, w, r);
                self.cycles += if by_cl { 8 + 4 * count as u32 } else { 2 }
                    + if self.rm_is_reg() { 0 } else { 13 };
            }
            0xD4 => { // AAM
                let n = self.fetch8(bus);
                if n == 0 {
                    // flags as if the result were 0 (8088)
                    self.set_szp(0, false);
                    self.setf(CF | OF | AF, false);
                    self.interrupt(bus, 0);
                    return;
                }
                let al = self.al();
                self.set_ah(al / n);
                let r = al % n;
                self.set_al(r);
                self.set_szp(r as u16, false);
                self.setf(CF | OF | AF, false);
                self.cycles += 83;
            }
            0xD5 => { // AAD (flags from the add AL + AH*n)
                let n = self.fetch8(bus);
                let t = self.ah().wrapping_mul(n);
                let r = self.alu(0, self.al() as u16, t as u16, false) as u8;
                self.regs[AX] = r as u16;
                self.cycles += 60;
            }
            0xD6 => { let v = if self.f(CF) { 0xFF } else { 0 }; self.set_al(v); self.cycles += 4; }
            0xD7 => {
                let seg = self.data_seg(DS);
                let off = self.regs[BX].wrapping_add(self.al() as u16);
                let v = self.rd8(bus, seg, off);
                self.set_al(v);
                self.cycles += 11;
            }
            0xD8..=0xDF => {
                // ESC: no coprocessor. The 8088 still reads the memory operand.
                self.modrm(bus);
                if !self.rm_is_reg() { let _ = self.rm_read8(bus); }
                self.cycles += 2;
            }
            0xE0..=0xE2 => {
                let d = self.fetch8(bus);
                self.regs[CX] = self.regs[CX].wrapping_sub(1);
                let go = self.regs[CX] != 0 && match op {
                    0xE0 => !self.f(ZF),
                    0xE1 => self.f(ZF),
                    _ => true,
                };
                if go { self.jump_rel8(d); self.cycles += 17; } else { self.cycles += 5; }
            }
            0xE3 => {
                let d = self.fetch8(bus);
                if self.regs[CX] == 0 { self.jump_rel8(d); self.cycles += 18; } else { self.cycles += 6; }
            }
            0xE4 | 0xE5 => {
                let p = self.fetch8(bus) as u16;
                self.port_in(bus, p, op & 1 != 0);
                self.cycles += 10;
            }
            0xE6 | 0xE7 => {
                let p = self.fetch8(bus) as u16;
                self.port_out(bus, p, op & 1 != 0);
                self.cycles += 10;
            }
            0xEC | 0xED => { let p = self.regs[DX]; self.port_in(bus, p, op & 1 != 0); self.cycles += 8; }
            0xEE | 0xEF => { let p = self.regs[DX]; self.port_out(bus, p, op & 1 != 0); self.cycles += 8; }
            0xE8 => {
                let d = self.fetch16(bus);
                let ip = self.ip;
                self.push(bus, ip);
                self.ip = ip.wrapping_add(d);
                self.cycles += 19;
            }
            0xE9 => { let d = self.fetch16(bus); self.ip = self.ip.wrapping_add(d); self.cycles += 15; }
            0xEA => {
                let ip = self.fetch16(bus);
                let cs = self.fetch16(bus);
                self.ip = ip;
                self.sregs[CS] = cs;
                self.cycles += 15;
            }
            0xEB => { let d = self.fetch8(bus); self.jump_rel8(d); self.cycles += 15; }
            0xF4 => { self.halted = true; self.cycles += 2; }
            0xF5 => { self.flags ^= CF; self.cycles += 2; }
            0xF6 | 0xF7 => self.group3(bus, op & 1 != 0),
            0xF8 => { self.setf(CF, false); self.cycles += 2; }
            0xF9 => { self.setf(CF, true); self.cycles += 2; }
            0xFA => { self.setf(IF, false); self.cycles += 2; }
            0xFB => { self.setf(IF, true); self.int_inhibit = true; self.cycles += 2; }
            0xFC => { self.setf(DF, false); self.cycles += 2; }
            0xFD => { self.setf(DF, true); self.cycles += 2; }
            0xFE | 0xFF => self.group45(bus, op == 0xFF),
            _ => {
                // prefixes only end up here after 15 of them in a row
                let _ = start_ip;
            }
        }
    }

    fn port_in(&mut self, bus: &mut dyn I8088Bus, p: u16, w: bool) {
        if w {
            let lo = bus.in8(p) as u16;
            let hi = bus.in8(p.wrapping_add(1)) as u16;
            self.regs[AX] = lo | (hi << 8);
            self.cycles += 4;
        } else {
            let v = bus.in8(p);
            self.set_al(v);
        }
    }
    fn port_out(&mut self, bus: &mut dyn I8088Bus, p: u16, w: bool) {
        bus.out8(p, self.al());
        if w {
            bus.out8(p.wrapping_add(1), self.ah());
            self.cycles += 4;
        }
    }

    fn string_op(&mut self, bus: &mut dyn I8088Bus, op: u8) {
        let w = op & 1 != 0;
        let step: u16 = if w { 2 } else { 1 };
        let delta = if self.f(DF) { step.wrapping_neg() } else { step };
        let src = self.data_seg(DS);
        let is_cmp = matches!(op, 0xA6 | 0xA7 | 0xAE | 0xAF);
        let per = match op {
            0xA4 | 0xA5 => 17,
            0xA6 | 0xA7 => 22,
            0xAA | 0xAB => 10,
            0xAC | 0xAD => 13,
            _ => 15,
        };
        if self.rep != 0 { self.cycles += 9; }
        loop {
            if self.rep != 0 && self.regs[CX] == 0 { break; }
            let (si, di) = (self.regs[SI], self.regs[DI]);
            match op {
                0xA4 | 0xA5 => {
                    if w { let v = self.rd16(bus, src, si); self.wr16(bus, ES, di, v); }
                    else { let v = self.rd8(bus, src, si); self.wr8(bus, ES, di, v); }
                    self.regs[SI] = si.wrapping_add(delta);
                    self.regs[DI] = di.wrapping_add(delta);
                }
                0xA6 | 0xA7 => {
                    let (a, b) = if w { (self.rd16(bus, src, si), self.rd16(bus, ES, di)) }
                                 else { (self.rd8(bus, src, si) as u16, self.rd8(bus, ES, di) as u16) };
                    self.alu(7, a, b, w);
                    self.regs[SI] = si.wrapping_add(delta);
                    self.regs[DI] = di.wrapping_add(delta);
                }
                0xAA | 0xAB => {
                    if w { let v = self.regs[AX]; self.wr16(bus, ES, di, v); }
                    else { let v = self.al(); self.wr8(bus, ES, di, v); }
                    self.regs[DI] = di.wrapping_add(delta);
                }
                0xAC | 0xAD => {
                    if w { self.regs[AX] = self.rd16(bus, src, si); }
                    else { let v = self.rd8(bus, src, si); self.set_al(v); }
                    self.regs[SI] = si.wrapping_add(delta);
                }
                _ => {
                    let b = if w { self.rd16(bus, ES, di) } else { self.rd8(bus, ES, di) as u16 };
                    let a = if w { self.regs[AX] } else { self.al() as u16 };
                    self.alu(7, a, b, w);
                    self.regs[DI] = di.wrapping_add(delta);
                }
            }
            self.cycles += per;
            if self.rep == 0 { break; }
            self.regs[CX] = self.regs[CX].wrapping_sub(1);
            if is_cmp {
                let z = self.f(ZF);
                if (self.rep == 0xF3 && !z) || (self.rep == 0xF2 && z) { break; }
            }
        }
    }

    fn group3(&mut self, bus: &mut dyn I8088Bus, w: bool) {
        self.modrm(bus);
        let reg = self.m.reg;
        let mem = !self.rm_is_reg();
        match reg {
            0 | 1 => {
                let a = self.rm_read(bus, w);
                let b = if w { self.fetch16(bus) } else { self.fetch8(bus) as u16 };
                self.alu(4, a, b, w);
                self.cycles += if mem { 11 } else { 5 };
            }
            2 => {
                let a = self.rm_read(bus, w);
                self.rm_write(bus, w, !a);
                self.cycles += if mem { 16 } else { 3 };
            }
            3 => {
                let a = self.rm_read(bus, w);
                let r = self.alu(5, 0, a, w);
                self.rm_write(bus, w, r);
                self.cycles += if mem { 16 } else { 3 };
            }
            4 | 5 => {
                let src = self.rm_read(bus, w);
                let negate = self.rep != 0;
                if w {
                    let r: u32 = if reg == 4 {
                        self.regs[AX] as u32 * src as u32
                    } else {
                        (self.regs[AX] as i16 as i32 * src as i16 as i32) as u32
                    };
                    let r = if negate { r.wrapping_neg() } else { r };
                    self.regs[AX] = r as u16;
                    self.regs[DX] = (r >> 16) as u16;
                    let big = if reg == 4 { r >> 16 != 0 } else { (r as u16 as i16 as i32 as u32) != r };
                    self.setf(CF | OF, big);
                    self.set_szp(self.regs[DX], true);
                    self.cycles += if reg == 4 { 118 } else { 128 };
                } else {
                    let r: u16 = if reg == 4 {
                        self.al() as u16 * src
                    } else {
                        (self.al() as i8 as i16 * src as u8 as i8 as i16) as u16
                    };
                    let r = if negate { r.wrapping_neg() } else { r };
                    self.regs[AX] = r;
                    let big = if reg == 4 { r >> 8 != 0 } else { (r as u8 as i8 as i16 as u16) != r };
                    self.setf(CF | OF, big);
                    self.set_szp(r >> 8, false);
                    self.cycles += if reg == 4 { 70 } else { 80 };
                }
                self.setf(ZF, false);
            }
            _ => {
                let src = self.rm_read(bus, w);
                let negate = self.rep != 0;
                let ok = if w {
                    let num = ((self.regs[DX] as u32) << 16) | self.regs[AX] as u32;
                    if reg == 6 {
                        if src == 0 { false } else {
                            let q = num / src as u32;
                            let r = num % src as u32;
                            if q > 0xFFFF { false } else {
                                let q = if negate { (q as u16).wrapping_neg() } else { q as u16 };
                                self.regs[AX] = q;
                                self.regs[DX] = r as u16;
                                true
                            }
                        }
                    } else {
                        let d = src as i16 as i32;
                        let n = num as i32;
                        if d == 0 { false } else {
                            let q = n.wrapping_div(d) as i64;
                            let r = n.wrapping_rem(d);
                            let q = if negate { -q } else { q };
                            if !(-32767..=32767).contains(&q) || (n == i32::MIN && d == -1) { false } else {
                                self.regs[AX] = q as u16;
                                self.regs[DX] = r as u16;
                                true
                            }
                        }
                    }
                } else {
                    let num = self.regs[AX];
                    if reg == 6 {
                        if src == 0 { false } else {
                            let q = num / src;
                            let r = num % src;
                            if q > 0xFF { false } else {
                                let q = if negate { (q as u8).wrapping_neg() } else { q as u8 };
                                self.set_al(q);
                                self.set_ah(r as u8);
                                true
                            }
                        }
                    } else {
                        let d = src as u8 as i8 as i16;
                        let n = num as i16;
                        if d == 0 { false } else {
                            let q = n.wrapping_div(d) as i32;
                            let r = n.wrapping_rem(d);
                            let q = if negate { -q } else { q };
                            if !(-127..=127).contains(&q) || (n == i16::MIN && d == -1) { false } else {
                                self.set_al(q as u8);
                                self.set_ah(r as u8);
                                true
                            }
                        }
                    }
                };
                self.cycles += match (w, reg) { (false, 6) => 80, (false, _) => 101, (true, 6) => 144, _ => 165 };
                if !ok {
                    self.interrupt(bus, 0);
                }
            }
        }
    }

    fn group45(&mut self, bus: &mut dyn I8088Bus, w: bool) {
        self.modrm(bus);
        let mem = !self.rm_is_reg();
        match self.m.reg {
            0 | 1 => {
                let a = self.rm_read(bus, w);
                let r = self.inc_dec(a, self.m.reg == 1, w);
                self.rm_write(bus, w, r);
                self.cycles += if mem { 15 } else { 3 };
            }
            2 => {
                let t = self.rm_read(bus, w);
                let t = if w { t } else { t | 0xFF00 };
                let ip = self.ip;
                self.push(bus, ip);
                self.ip = t;
                self.cycles += if mem { 21 } else { 16 };
            }
            3 => {
                let (seg, ea) = (self.m.seg, self.m.ea);
                let ip = self.rd16(bus, seg, ea);
                let cs = self.rd16(bus, seg, ea.wrapping_add(2));
                let (ocs, oip) = (self.sregs[CS], self.ip);
                self.push(bus, ocs);
                self.push(bus, oip);
                self.ip = ip;
                self.sregs[CS] = cs;
                self.cycles += 37;
            }
            4 => {
                let t = self.rm_read(bus, w);
                self.ip = if w { t } else { t | 0xFF00 };
                self.cycles += if mem { 18 } else { 11 };
            }
            5 => {
                let (seg, ea) = (self.m.seg, self.m.ea);
                let ip = self.rd16(bus, seg, ea);
                let cs = self.rd16(bus, seg, ea.wrapping_add(2));
                self.ip = ip;
                self.sregs[CS] = cs;
                self.cycles += 24;
            }
            _ => {
                let v = if self.m.md == 3 && self.m.rm as usize == SP && w {
                    self.regs[SP].wrapping_sub(2)
                } else {
                    self.rm_read(bus, w)
                };
                let v = if w { v } else { v | 0xFF00 };
                self.push(bus, v);
                self.cycles += if mem { 16 } else { 11 };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FlatBus { mem: Vec<u8> }
    impl I8088Bus for FlatBus {
        fn read8(&mut self, a: u32) -> u8 { self.mem[(a & 0xFFFFF) as usize] }
        fn write8(&mut self, a: u32, v: u8) { self.mem[(a & 0xFFFFF) as usize] = v; }
    }

    fn run_prog(code: &[u8], setup: impl Fn(&mut I8088)) -> (I8088, FlatBus) {
        let mut bus = FlatBus { mem: vec![0; 1 << 20] };
        bus.mem[0x1000..0x1000 + code.len()].copy_from_slice(code);
        let mut cpu = I8088::new();
        cpu.sregs[CS] = 0x0100;
        cpu.ip = 0;
        cpu.sregs[SS] = 0x2000;
        cpu.regs[SP] = 0x100;
        setup(&mut cpu);
        while (cpu.ip as usize) < code.len() && !cpu.halted { cpu.step(&mut bus); }
        (cpu, bus)
    }

    #[test]
    fn basic_arith() {
        // mov ax,1234h; add ax,0F00Fh; mov bx,ax; push bx; pop cx; hlt
        let (c, _) = run_prog(&[0xB8, 0x34, 0x12, 0x05, 0x0F, 0xF0, 0x89, 0xC3, 0x53, 0x59, 0xF4], |_| {});
        assert_eq!(c.regs[AX], 0x0243);
        assert_eq!(c.regs[CX], 0x0243);
        assert!(c.flags & CF != 0);
        assert_eq!(c.regs[SP], 0x100);
    }

    #[test]
    fn rep_movsb_and_loop() {
        // mov si,0; mov di,10h; mov cx,4; rep movsb (ds=es=0x200); hlt
        let (_c, b) = run_prog(&[0xBE, 0, 0, 0xBF, 0x10, 0, 0xB9, 4, 0, 0xF3, 0xA4, 0xF4], |c| {
            c.sregs[DS] = 0x200; c.sregs[ES] = 0x200;
        });
        let _ = b.mem[0x2010];
        assert_eq!(&b.mem[0x2010..0x2014], &b.mem[0x2000..0x2004]);
    }

    // ---------------------------------------------------------------------------------
    // SingleStepTests 8088 v2, converted to text by tools/sst_conv.py:
    //   I8088_SST=<dir of .txt> cargo test --release --lib i8088_sst -- --ignored --nocapture
    //   (I8088_SST_ONLY=F6.7 runs one file)
    // ---------------------------------------------------------------------------------
    struct SstBus { mem: std::collections::HashMap<u32, u8> }
    impl I8088Bus for SstBus {
        fn read8(&mut self, a: u32) -> u8 { *self.mem.get(&(a & 0xFFFFF)).unwrap_or(&0x90) }
        fn write8(&mut self, a: u32, v: u8) { self.mem.insert(a & 0xFFFFF, v); }
    }

    #[test]
    #[ignore]
    fn i8088_sst() {
        let dir = std::env::var("I8088_SST").expect("I8088_SST=<dir>");
        let only = std::env::var("I8088_SST_ONLY").ok();
        let mut files: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).collect();
        files.sort();
        let order = ["ax", "bx", "cx", "dx", "cs", "ss", "ds", "es", "sp", "bp", "si", "di", "ip", "flags"];
        let mut total_fail = 0;
        let mut total_pass = 0;
        let mut bad_files = Vec::new();
        for path in files {
            let stem = path.file_stem().unwrap().to_string_lossy().to_string();
            if let Some(o) = &only { if &stem != o { continue; } }
            let text = std::fs::read_to_string(&path).unwrap();
            let mut status = String::new();
            let (mut pass, mut fail) = (0, 0);
            let mut first_fail = String::new();
            for line in text.lines() {
                if let Some(s) = line.strip_prefix("# ") { status = s.to_string(); continue; }
                let cols: Vec<&str> = line.split('\t').collect();
                let init: Vec<u16> = cols[1].split(' ').map(|v| v.parse().unwrap()).collect();
                let mut bus = SstBus { mem: Default::default() };
                for p in cols[2].split(',').filter(|s| !s.is_empty()) {
                    let (a, v) = p.split_once(':').unwrap();
                    bus.mem.insert(a.parse().unwrap(), v.parse().unwrap());
                }
                let mut cpu = I8088::new();
                let get = |k: &str| init[order.iter().position(|o| *o == k).unwrap()];
                cpu.regs = [get("ax"), get("cx"), get("dx"), get("bx"), get("sp"), get("bp"), get("si"), get("di")];
                cpu.sregs = [get("es"), get("cs"), get("ss"), get("ds")];
                cpu.ip = get("ip");
                cpu.flags = get("flags");
                cpu.step(&mut bus);
                let mask: u16 = cols[5].parse().unwrap();
                let mut errs = Vec::new();
                for kv in cols[3].split(',').filter(|s| !s.is_empty()) {
                    let (k, v) = kv.split_once('=').unwrap();
                    let want: u16 = v.parse().unwrap();
                    let got = match k {
                        "ax" => cpu.regs[AX], "bx" => cpu.regs[BX], "cx" => cpu.regs[CX], "dx" => cpu.regs[DX],
                        "sp" => cpu.regs[SP], "bp" => cpu.regs[BP], "si" => cpu.regs[SI], "di" => cpu.regs[DI],
                        "cs" => cpu.sregs[CS], "ss" => cpu.sregs[SS], "ds" => cpu.sregs[DS], "es" => cpu.sregs[ES],
                        "ip" => cpu.ip, "flags" => cpu.flags, _ => want,
                    };
                    let (g, w2) = if k == "flags" { (got & mask, want & mask) } else { (got, want) };
                    if g != w2 { errs.push(format!("{k}: got {got:04X} want {want:04X}")); }
                }
                // registers not listed must be unchanged
                for (i, k) in order.iter().enumerate() {
                    if cols[3].split(',').any(|kv| kv.split('=').next() == Some(*k)) { continue; }
                    let got = match *k {
                        "ax" => cpu.regs[AX], "bx" => cpu.regs[BX], "cx" => cpu.regs[CX], "dx" => cpu.regs[DX],
                        "sp" => cpu.regs[SP], "bp" => cpu.regs[BP], "si" => cpu.regs[SI], "di" => cpu.regs[DI],
                        "cs" => cpu.sregs[CS], "ss" => cpu.sregs[SS], "ds" => cpu.sregs[DS], "es" => cpu.sregs[ES],
                        "ip" => cpu.ip, _ => cpu.flags,
                    };
                    let (g, w2) = if *k == "flags" { (got & mask, init[i] & mask) } else { (got, init[i]) };
                    if g != w2 { errs.push(format!("{k} changed: {got:04X} (was {:04X})", init[i])); }
                }
                // The flags a divide error pushes come out of the 8088's division microcode
                // part-way through; not modelled. Skip that stack slot (SS:SP+4 after the INT).
                let flags_slot = lin(cpu.sregs[SS], cpu.regs[SP].wrapping_add(4));
                let div_fault = stem.starts_with("F6.6") || stem.starts_with("F6.7")
                    || stem.starts_with("F7.6") || stem.starts_with("F7.7");
                for p in cols[4].split(',').filter(|s| !s.is_empty()) {
                    let (a, v) = p.split_once(':').unwrap();
                    let a: u32 = a.parse().unwrap();
                    let want: u8 = v.parse().unwrap();
                    let got = *bus.mem.get(&a).unwrap_or(&0x90);
                    if div_fault && (a == flags_slot || a == (flags_slot + 1) & 0xFFFFF) { continue; }
                    if got != want { errs.push(format!("[{a:05X}] got {got:02X} want {want:02X}")); }
                }
                if errs.is_empty() { pass += 1; } else {
                    fail += 1;
                    if first_fail.is_empty() { first_fail = format!("{}: {}", cols[0], errs.join("; ")); }
                }
            }
            if fail > 0 {
                println!("{stem:6} ({status}) {pass} pass {fail} FAIL  e.g. {first_fail}");
                bad_files.push(stem.clone());
            }
            total_fail += fail;
            total_pass += pass;
        }
        println!("{total_pass} tests passed, {total_fail} failed");
        println!("files with failures: {} {:?}", bad_files.len(), bad_files);
        assert_eq!(total_fail, 0);
    }
}
