use crate::gameboy::gb_bus::Bus;

#[derive(Clone, Copy, PartialEq)]
pub enum GbVariant {
    Dmg,
    Mgb,
    Cgb,
    Sgb,
    Sgb2,
    Agb,
}

const FLAG_Z:u8 = 0x80;
const FLAG_N:u8 = 0x40;
const FLAG_H:u8 = 0x20;
const FLAG_C:u8 = 0x10;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Reg { B, C, D, E, H, L, MemHL, A }

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Reg16 { BC, DE, HL, SP }

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Reg16Stk { BC, DE, HL, AF }

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Reg16Mem { BC, DE, HLI, HLD }

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Condition { NZ, Z, NC, C }


impl Condition {
    fn from_index(i: u8) -> Condition {
        match i & 3 {
            0 => Condition::NZ,
            1 => Condition::Z,
            2 => Condition::NC,
            _ => Condition::C,
        }
    }
}

impl Reg {
    fn from_index(i: u8) -> Reg {
        match i & 7 {
            0 => Reg::B, 1 => Reg::C, 2 => Reg::D, 3 => Reg::E,
            4 => Reg::H, 5 => Reg::L, 6 => Reg::MemHL, _ => Reg::A,
        }
    }
}

impl Reg16 {
    fn from_index(i: u8) -> Reg16 {
        match i & 3 {
            0 => Reg16::BC, 1 => Reg16::DE, 2 => Reg16::HL, _ => Reg16::SP,
        }
    }
}

impl Reg16Stk {
    pub fn from_index(i: u8) -> Reg16Stk {
        match i & 3 {
            0 => Reg16Stk::BC,
            1 => Reg16Stk::DE,
            2 => Reg16Stk::HL,
            _ => Reg16Stk::AF,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum AluOp { Add, Adc, Sub, Sbc, And, Xor, Or, Cp }

#[derive(Clone, Copy, Debug)]
pub enum CbOp { Rlc, Rrc, Rl, Rr, Sla, Sra, Swap, Srl, Bit, Res, Set }

impl AluOp {
    fn from_index(i: u8) -> AluOp {
        match i & 7 {
            0 => AluOp::Add, 1 => AluOp::Adc, 2 => AluOp::Sub,
            3 => AluOp::Sbc, 4 => AluOp::And, 5 => AluOp::Xor,
            6 => AluOp::Or, _ => AluOp::Cp,
        }
    }
 }

#[derive(Clone, Copy, Debug)]
enum Instruction {
    Nop,
    DI,
    EI,
    AluReg     { op: AluOp, src: Reg },
    AluImm     { op: AluOp },
    BitAnd     { src: Reg },
    BitOr      { src: Reg },
    BitXor     { src: Reg },
    BitAndImm,
    BitOrImm,
    BitXorImm,
    Call       { cond: Option<Condition> },
    CbPrefix,
    Cb(CbInstruction),
    Daa,       // Decimal Adjust Accumulator
    DecReg16   { dst: Reg16 },
    DecReg     { dst: Reg },
    Halt,
    IncReg16   { dst: Reg16 },
    IncReg     { dst: Reg },
    Jp         { cond: Option<Condition> },
    JpHl,
    Jr         { cond: Option<Condition> },
    LdRegReg   { dst: Reg, src: Reg },
    LdRegImm   { dst: Reg },
    LdReg16Imm { dst: Reg16 },
    LdMemReg   { src: Reg },
    LdRegMem   { dst: Reg },
    LdR16MemA  { dst: Reg16Mem },
    LdAR16Mem  { src: Reg16Mem },
    Push       { dst: Reg16Stk },
    Pop        { dst: Reg16Stk },
    Ret        { cond: Option<Condition> },
    Rlca,
    Rla,
    Reti,
    Stop,
    Unknown(u8),
}

#[derive(Clone, Copy, Debug)]
pub struct CbInstruction { pub op: CbOp, pub bit: u8, pub target: Reg }

impl Default for Instruction {
    fn default() -> Self { Instruction::Nop }
}

enum Operand { Reg(Reg), MemHL }

pub struct GbCpuConfig {
    pub variant: GbVariant,
    pub initial_a: u8,
    pub initial_f: u8,
    pub initial_bc: u16,
    pub initial_de: u16,
    pub initial_hl: u16,
    pub initial_sp: u16,
    pub initial_pc: u16,             // set to 0x0100 to skip bios
    pub supports_double_speed: bool,
}

impl GbCpuConfig {
    pub fn for_variant(variant: GbVariant) -> Self {
        match variant {
            GbVariant::Dmg => Self {
                variant, initial_a: 0x01, initial_f: 0xB0,
                initial_bc: 0x0013, initial_de: 0x00D8, initial_hl: 0x014D,
                initial_sp: 0xFFFE, supports_double_speed: false,
                initial_pc: 0x0000,
            },
            GbVariant::Mgb => Self {
                variant, initial_a: 0xFF, initial_f: 0xB0,
                initial_bc: 0x0013, initial_de: 0x00D8, initial_hl: 0x014D,
                initial_sp: 0xFFFE, supports_double_speed: false,
                initial_pc: 0x0000,
            },
            GbVariant::Cgb => Self {
                variant, initial_a: 0x11, initial_f: 0x80,
                initial_bc: 0x0000, initial_de: 0xFF56, initial_hl: 0x000D,
                initial_sp: 0xFFFE, supports_double_speed: true,
                initial_pc: 0x0000,
            },
            GbVariant::Agb => Self {
                variant, initial_a: 0x11, initial_f: 0x80,
                initial_bc: 0x0100, initial_de: 0xFF56, initial_hl: 0x000D,
                initial_sp: 0xFFFE, supports_double_speed: true,
                initial_pc: 0x0000,
            },
            GbVariant::Sgb | GbVariant::Sgb2 => Self {
                variant,
                initial_a: 0x01,  // Identifies as a classic monochrome Game Boy
                initial_f: 0xB0,
                initial_bc: 0x0013,
                initial_de: 0x00D8,
                initial_hl: 0x014D,
                initial_sp: 0xFFFE,
                initial_pc: 0x0000,
                supports_double_speed: false, // Runs strictly at normal speed
            },
        }
    }
}

pub struct GameBoyCpu {
    pub a: u8, pub f: u8,
    pub b: u8, pub c: u8,
    pub d: u8, pub e: u8,
    pub h: u8, pub l: u8,
    pub sp: u16,
    pub pc: u16,

    pub current_opcode: u8,
    pub instruction_step: u8,
    pub current_lsb: u8,       
    pub current_msb: u8,
    pub current_value: u8,
    pub cb_prefixed: bool,
    pub halted: bool,
    pub halt_bug: bool,
    pub ime: bool,
    pub ime_pending: bool,
    pub config: GbCpuConfig,
    pub current: Instruction,
    pub double_speed: bool,
}

impl GameBoyCpu {
    pub fn new(variant: GbVariant) -> Self {
        let config = GbCpuConfig::for_variant(variant);
    
        Self {
            a: config.initial_a,
            b: (config.initial_bc >> 8) as u8,
            c: config.initial_bc as u8,
            d: (config.initial_de >> 8) as u8,
            e: config.initial_de as u8,
            f: config.initial_f,
            h: (config.initial_hl >> 8) as u8,
            l: config.initial_hl as u8,
            pc: config.initial_pc,
            sp: config.initial_sp,
            ime: false, ime_pending: false,
            instruction_step: 0,
            current: Instruction::Nop,
            current_msb: 0, current_lsb: 0,
            current_opcode: 0, current_value: 0,
            halted: false, halt_bug: false,
            double_speed: config.supports_double_speed,
            cb_prefixed: false,
            config,
        }
    }
    pub fn bc(&self) -> u16 { ((self.b as u16) << 8) | self.c as u16 }
    pub fn set_bc(&mut self, v: u16) { self.b = (v >> 8) as u8; self.c = v as u8; }
    pub fn de(&self) -> u16 { ((self.d as u16) << 8) | self.e as u16 }
    pub fn set_de(&mut self, v: u16) { self.d = (v >> 8) as u8; self.e = v as u8; }
    pub fn hl(&self) -> u16 { ((self.h as u16) << 8) | self.l as u16 }
    pub fn set_hl(&mut self, v: u16) { self.h = (v >> 8) as u8; self.l = v as u8; }
    pub fn af(&self) -> u16 { ((self.a as u16) << 8) | self.f as u16 }
    pub fn set_af(&mut self, v: u16) { self.a = (v >> 8) as u8; self.f = (v as u8) & 0xF0; }
    pub fn check_condition(&self, cond: Condition) -> bool {
        match cond {
            Condition::NZ => (self.f & FLAG_Z) == 0,
            Condition::Z  => (self.f & FLAG_Z) != 0,
            Condition::NC => (self.f & FLAG_C) == 0,
            Condition::C  => (self.f & FLAG_C) != 0,
        }
    }

    // read at current PC and advance PC - unless halt bug is armed
    // byte gets executed twice in that case.
    fn fetch_byte(&mut self, bus: &mut dyn Bus) -> u8 {
        let b = bus.read(self.pc);
        if self.halt_bug {
            self.halt_bug = false;
        } else {
            self.pc = self.pc.wrapping_add(1);
        }
        b
    }

    fn pop(&mut self, bus: &mut dyn Bus) -> u8 {
        let b = bus.read(self.sp);
        self.sp = self.sp.wrapping_add(1);
        b
    }

    fn push(&mut self, bus: &mut dyn Bus, val: u8) {
        self.sp = self.sp.wrapping_sub(1);
        bus.write(self.sp, val);
    }

    // exactly one bus access happens per call
    pub fn step_one_m_cycle(&mut self, bus: &mut dyn Bus) {
        if self.instruction_step == 0 {
            if self.service_interrupt(bus) { return; }
            if self.ime_pending {
                self.ime = true;
                self.ime_pending = false;
            }
            let opcode = self.fetch_byte(bus);
            self.current_opcode = opcode;
            if self.cb_prefixed {
                let cb = self.decode_cb(opcode);
                self.current = Instruction::Cb(cb);
                self.cb_prefixed = false;
                if cb.target == Reg::MemHL {
                    self.instruction_step = 1;
                } else {
                    let v = self.read_reg(cb.target);
                    if let Some(r) = self.cb_execute(cb, v) {
                        self.write_reg(cb.target, r);
                    }
                    self.instruction_step = 0;
                }
                
                return;
            }
            if opcode == 0xCB {
                self.cb_prefixed = true;
                return;
            }
            self.current = self.decode_main(opcode);
            if self.execute_fetch_cycle(bus) {
                self.instruction_step = 0;
            } else {
                self.instruction_step = 1;
            }
        } else {
            if self.execute_micro_step(bus) {
                self.instruction_step = 0;
            } else {
                self.instruction_step += 1;
            }
        }
    }

    fn execute_fetch_cycle(&mut self, bus: &mut dyn Bus) -> bool {
        match self.current {
            Instruction::CbPrefix => {
                self.cb_prefixed = true;
                true
            },
            Instruction::Cb(_) => false,
            Instruction::Nop => true,
            Instruction::EI => { self.ime_pending = true; true },
            Instruction::DI => { self.ime = false; self.ime_pending = false; true },
            Instruction::Halt => {
                let pending = bus.irq_pending() != 0;
                if !self.ime && pending {
                    self.halt_bug = true;
                } else {
                    self.halted = true;
                }
                true
            },

            Instruction::AluReg { op, src } => {
                if src == Reg::MemHL {
                    false // Needs micro-step to read from (HL)
                } else {
                    let v = self.read_reg(src);
                    self.alu(op, v);
                    true
                }
            },

            Instruction::Call { .. } => false,

            Instruction::Daa => {
                let mut a:u16 = self.a as u16;
                let h = self.f & FLAG_H == FLAG_H;
                let n = self.f & FLAG_N == FLAG_N;
                let c = self.f & FLAG_C == FLAG_C;

                if n {
                    if h {
                        a -= 0x06;
                        a &= 0xFF;
                    }
                    if c {
                        a -= 0x60;
                    }
                } else {
                    if (a & 0x0F) > 0x09 || h {
                        a += 0x06;
                    }
                    if (a > 0x9F) || c {
                        a += 0x60;
                    }
                }

                if a & 0xFF == 0 { self.f |= FLAG_Z; }
                self.f &= !FLAG_H;
                if (a & 0x100) != 0 { self.f |= FLAG_C; }
                self.a = (a & 0xFF) as u8;
                true
            },

            Instruction::IncReg { dst } => {
                if dst == Reg::MemHL {
                    false
                } else {
                    let reg = self.read_reg(dst);
                    let res = reg.wrapping_add(1);
                    let mut f:u8 = self.f & FLAG_C;
                    if res == 0 { f |= FLAG_Z; }
                    if reg & 0x0F == 0x0F { f |= FLAG_H; }
                    self.f = f;
                    self.write_reg(dst, res);
                    true
                }                
            },

            Instruction::IncReg16 { dst } => {
                false
            },

            Instruction::DecReg { dst } => {
                if dst == Reg::MemHL {
                    false
                } else {
                    let reg = self.read_reg(dst);
                    let res = reg.wrapping_sub(1);
                    let mut f = (self.f & FLAG_C) | FLAG_N;
                    if res == 0 { f |= FLAG_Z; }
                    if reg & 0x0F == 0 { f |= FLAG_H; }
                    self.f = f;
                    self.write_reg(dst, res);
                    true
                }
            },

            Instruction::DecReg16 { dst } => {
                false
            },

            Instruction::Jp { .. } => false,
            Instruction::Jr { .. } => false,

            // LD r, r' with both operands registers: 1 M-cycle
            // if either side is (HL), need a memory access
            Instruction::LdRegReg { dst, src } => {
                if dst == Reg::MemHL || src == Reg::MemHL {
                    false   // handled in execute_micro_step
                } else {
                    let v = self.read_reg(src);
                    self.write_reg(dst,v);
                    true
                }
            },

            Instruction::LdRegImm { .. } => false,
            Instruction::LdReg16Imm { .. } => false,
            Instruction::AluImm { .. } => false,

            Instruction::Pop { .. }=> false,
            Instruction::Push { .. } => false,
            Instruction::Ret { .. } => false,
            Instruction::Reti => false,
            Instruction::Unknown(op) => panic!("Unimplemented opcode {:02X}", op),
            _ => { todo!("need to handle all instructions"); },
        }
    }

    // returns true when instruction completes
    fn execute_micro_step(&mut self, bus: &mut dyn Bus) -> bool {
        match self.current {
            Instruction::Call { cond } => {
                match self.instruction_step {
                    1 => {
                        self.current_lsb = self.fetch_byte(bus);
                        false
                    }
                    2 => {
                        self.current_msb = self.fetch_byte(bus);
                        if let Some(c) = cond {
                            if !self.check_condition(c) {
                                return true;
                            }
                        }
                        false
                    }
                    3 => {
                        self.push(bus, (self.pc >> 8) as u8);
                        false
                    }
                    4 => {
                        self.push(bus, (self.pc & 0xFF) as u8);
                        false
                    }
                    _ => {
                        let target = ((self.current_msb as u16)<< 8) | (self.current_lsb as u16);
                        self.pc = target;
                        true
                    }
                }
            }

            Instruction::Cb(cb) => {
                match self.instruction_step {
                    1 => {
                        let v = bus.read(self.hl());
                        match self.cb_execute(cb, v) {
                            Some(r) => { self.current_value = r; false }
                            None    => true,                               // BIT: done at 3 M-cycles
                        }
                    }
                    _ => {
                        bus.write(self.hl(), self.current_value);
                        true
                    }
                }
            }

            Instruction::IncReg { dst } => {
                if dst == Reg::MemHL {
                    if self.instruction_step == 1 {
                        self.current_value = bus.read(self.hl());
                        false
                    } else {
                        let res = self.current_value.wrapping_add(1);
                        let mut f = (self.f & FLAG_C) | FLAG_N;
                        if res == 0 { f |= FLAG_Z; }
                        if self.current_value & 0x0F == 0x0F { f |= FLAG_H; }
                        self.f = f;
                        bus.write(self.hl(), res);
                        true
                    }                
                } else {
                    true
                }
            }

            Instruction::IncReg16 { dst } => {
                let val = self.read_reg16(dst);
                let res = val.wrapping_add(1);
                self.write_reg16(dst, res);
                true
            }

            Instruction::DecReg { dst } => {
                if dst == Reg::MemHL {
                    if self.instruction_step == 1 {
                        self.current_value = bus.read(self.hl());
                        false
                    } else {
                        let res = self.current_value.wrapping_sub(1);
                        let mut f = (self.f & FLAG_C) | FLAG_N;
                        if res == 0 { f |= FLAG_Z; }
                        if self.current_value & 0xF0 == 0 { f |= FLAG_H; }
                        self.f = f;
                        bus.write(self.hl(), res);
                        true
                    }                
                } else {
                    true
                }
            }

            Instruction::DecReg16 { dst } => {
                let val = self.read_reg16(dst);
                let res = val.wrapping_sub(1);
                self.write_reg16(dst, res);
                true
            }

            Instruction::Jp { cond } => {
                match self.instruction_step {
                    1 => {
                        self.current_lsb = self.fetch_byte(bus);
                        false
                    }
                    2 => {
                        self.current_msb = self.fetch_byte(bus);
                        if let Some(c) = cond {
                            if !self.check_condition(c) {
                                return true;
                            }
                        }
                        false
                    }
                    _ => {
                        let target = ((self.current_msb as u16)<< 8) | (self.current_lsb as u16);
                        self.pc = target;
                        true
                    }
                }
            }

            Instruction::Jr { cond } => {
                match self.instruction_step {
                    1 => {
                        self.current_lsb = self.fetch_byte(bus);
                        if let Some(c) = cond {
                            if !self.check_condition(c) {
                                return true;
                            }
                        }
                        false
                    }
                    _ => {
                        let offset = self.current_lsb as i8;
                        self.pc = self.pc.wrapping_add_signed(offset as i16);
                        true
                    }
                }
            }

            Instruction::LdRegReg { dst, src } => {
                if src == Reg::MemHL {
                    let v = bus.read(self.hl());
                    self.write_reg(dst, v);
                } else {
                    let v = self.read_reg(src);
                    bus.write(self.hl(), v);                    
                }
                true
            }

            Instruction::LdRegImm { dst } => {
                if dst == Reg::MemHL {
                    if self.instruction_step == 1 {
                        self.current_lsb = self.fetch_byte(bus);
                        false
                    } else {
                        bus.write(self.hl(), self.current_lsb);
                        true
                    }
                } else {
                    let v = self.fetch_byte(bus);
                    self.write_reg(dst, v);
                    true
                }
            }

            Instruction::LdReg16Imm { dst } => {
                if self.instruction_step == 1 {
                    self.current_lsb = self.fetch_byte(bus);
                    false
                } else {
                    self.current_msb = self.fetch_byte(bus);
                    let v = ((self.current_msb as u16) << 8) | (self.current_lsb as u16);
                    self.write_reg16(dst,v);
                    true
                }
            }

            Instruction::AluReg { op, .. } => {
                let v = bus.read(self.hl());
                self.alu(op, v);
                true
            }

            Instruction::AluImm { op } => {
                let v = self.fetch_byte(bus);
                self.alu(op, v);
                true
            }

            Instruction::Pop { dst }=> {
                match self.instruction_step {
                    1 => {
                        self.current_lsb = self.pop(bus);
                        false
                    }
                    _ => {
                        self.current_msb = self.pop(bus);
                        self.write_reg16_stk(dst, ((self.current_msb as u16) << 8) | self.current_lsb as u16);
                        true
                    }
                }
            }


            Instruction::Push { dst } => {
                match self.instruction_step {
                    1 => false,
                    2 => {
                        self.push(bus, (self.read_reg16_stk(dst) >> 8) as u8);
                        false
                    }
                    _ => {
                        self.push(bus, (self.read_reg16_stk(dst) & 0xFF) as u8);
                        true
                    }
                }
            }

            Instruction::Ret { cond } => {
                match cond {
                    None => match self.instruction_step {
                        1 => {
                            self.current_lsb = self.pop(bus);
                            false
                        }
                        2 => {
                            self.current_msb = self.pop(bus);
                            false
                        }
                        _ => {
                            self.pc = ((self.current_msb as u16) << 8) | (self.current_lsb as u16);
                            true
                        }
                    },
                    Some(c) => match self.instruction_step {
                        1 => {
                            if !self.check_condition(c) {
                                return true;
                            }
                            false
                        }
                        2 => {
                            self.current_lsb = self.pop(bus);
                            false
                        }
                        3 => {
                            self.current_msb = self.pop(bus);
                            false
                        }

                        _ => {
                            let target = ((self.current_msb as u16)<< 8) | (self.current_lsb as u16);
                            self.pc = target;
                            true
                        }
                    },
                }
            }

            Instruction::Reti => {
                match self.instruction_step {
                    1 => {
                        self.current_lsb = self.pop(bus);
                        false
                    }
                    2 => {
                        self.current_msb = self.pop(bus);
                        false
                    }
                    _ => {
                        self.pc = (self.current_msb as u16) >> 8 | (self.current_lsb as u16);
                        self.ime = true;
                        true
                    }
                }
            }

            _ => true,
        }
    }

    fn decode_main(&self, op: u8) -> Instruction {
        match op {
            0x00 => Instruction::Nop,
            0x01|0x11|0x21|0x31 => {
                let dst = Reg16::from_index((op >> 4) & 3);
                Instruction::LdReg16Imm { dst }
            },
            0x03|0x13|0x23|0x33 => {
                let dst = Reg16::from_index((op >> 4) & 3);
                Instruction::IncReg16 { dst }
            },
            0x04|0x0C|0x14|0x1C|0x24|0x2C|0x34|0x3C => {
                let dst = Reg::from_index((op >> 3) & 7);
                Instruction::IncReg { dst }
            },
            0x05|0x0D|0x15|0x1D|0x25|0x2D|0x35|0x3D => {
                let dst = Reg::from_index((op >> 3) & 7);
                Instruction::DecReg { dst }
            },
            0x06|0x0E|0x16|0x1E|0x26|0x2E|0x36|0x3E => {
                let dst = Reg::from_index((op >> 3) & 7);
                Instruction::LdRegImm { dst }
            },
            0x0B|0x1B|0x2B|0x3B => {
                let dst = Reg16::from_index((op >> 4) & 3);
                Instruction::DecReg16 { dst }
            },
            0x18 => Instruction::Jr { cond: None },
            0x20|0x28|0x30|0x38 => Instruction::Jr {
                cond: Some(Condition::from_index((op >> 3) & 3)),
            },
            0x27 => Instruction::Daa,
            0x76 => Instruction::Halt,   
            0x40..=0x7F => {
                let dst = Reg::from_index((op >> 3) & 7);
                let src = Reg::from_index(op & 7);
                Instruction::LdRegReg { dst, src }
            },
            0x80..=0xBF => {
                let alu = AluOp::from_index((op >> 3) & 7);
                let src = Reg::from_index(op & 7);
                Instruction::AluReg { op: alu, src }
            },
            0xC0|0xC8|0xD0|0xD8 => {
                let cond = Some(Condition::from_index((op >> 3) & 3));
                Instruction::Ret { cond }
            },
            0xC1|0xD1|0xE1|0xF1 => {
                let dst = Reg16Stk::from_index((op >> 4) & 3);
                Instruction::Pop { dst }
            },
            0xC2|0xCA|0xD2|0xDA => {
                let cond = Some(Condition::from_index((op >> 3) & 3));
                Instruction::Jp { cond }
            },
            0xC3 => Instruction::Jp { cond: None },
            0xC4|0xD4|0xCC|0xDC => {
                let cond = Some(Condition::from_index((op >> 3) & 3));
                Instruction::Call { cond }
            },
            0xC5|0xD5|0xE5|0xF5 => {
                let dst = Reg16Stk::from_index((op >> 4) & 3);
                Instruction::Push { dst }
            },
            0xC6 => Instruction::AluImm { op: AluOp::Add },
            0xC9 => Instruction::Ret { cond: None },
            0xCB => Instruction::CbPrefix,
            0xCD => Instruction::Call { cond: None },
            0xCE => Instruction::AluImm { op: AluOp::Adc },
            0xD6 => Instruction::AluImm { op: AluOp::Sub },
            0xD9 => Instruction::Reti,
            0xDE => Instruction::AluImm { op: AluOp::Sbc },
            0xE6 => Instruction::AluImm { op: AluOp::And },
            0xE9 => Instruction::JpHl,
            0xEE => Instruction::AluImm { op: AluOp::Xor },
            0xF3 => Instruction::DI,
            0xF6 => Instruction::AluImm { op: AluOp::Or  },
            0xFB => Instruction::EI,
            0xFE => Instruction::AluImm { op: AluOp::Cp  },
            _ => todo!("opcode {:02X}", op),
        }
    }

    fn decode_cb(&mut self, opcode: u8) -> CbInstruction {
        let target = Reg::from_index(opcode & 7);
        let y = (opcode >> 3) & 7;
        let op = match opcode >> 6 {
            0 => match y {
                0 => CbOp::Rlc, 1 => CbOp::Rrc, 2 => CbOp::Rl,  3 => CbOp::Rr,
                4 => CbOp::Sla, 5 => CbOp::Sra, 6 => CbOp::Swap, _ => CbOp::Srl,
            },
            1 => CbOp::Bit,
            2 => CbOp::Res,
            _ => CbOp::Set,
        };
        CbInstruction { op, bit: y, target }
    }

    /// Returns Some(result) for ops that write back, None for BIT.
    fn cb_execute(&mut self, cb: CbInstruction, v: u8) -> Option<u8> {
        let old_c = (self.f & FLAG_C) != 0;
        let (result, carry) = match cb.op {
            CbOp::Rlc  => ((v << 1) | (v >> 7),              v & 0x80 != 0),
            CbOp::Rrc  => ((v >> 1) | (v << 7),              v & 0x01 != 0),
            CbOp::Rl   => ((v << 1) | old_c as u8,           v & 0x80 != 0),
            CbOp::Rr   => ((v >> 1) | ((old_c as u8) << 7),  v & 0x01 != 0),
            CbOp::Sla  => (v << 1,                            v & 0x80 != 0),
            CbOp::Sra  => ((v >> 1) | (v & 0x80),            v & 0x01 != 0),
            CbOp::Swap => ((v << 4) | (v >> 4),              false),
            CbOp::Srl  => (v >> 1,                            v & 0x01 != 0),

            CbOp::Bit => {
                // Z = bit is ZERO, N=0, H=1, C preserved. No write-back.
                let is_zero = (v & (1 << cb.bit)) == 0;
                self.f = (self.f & FLAG_C)
                       | if is_zero { FLAG_Z } else { 0 }
                       | FLAG_H;
                return None;
            }
            CbOp::Res => return Some(v & !(1 << cb.bit)),  // no flags
            CbOp::Set => return Some(v |  (1 << cb.bit)),  // no flags
        };

        // Rotates/shifts/swap: Z from result, N=0, H=0, C from shifted-out bit.
        self.f = if result == 0 { FLAG_Z } else { 0 }
               | if carry { FLAG_C } else { 0 };
        Some(result)
    }

    fn service_interrupt(&mut self, bus: &mut dyn Bus) -> bool {
        if !self.ime { return false; }
        let pending = bus.irq_pending();
        if pending == 0 { self.halted = false; return false; }

        let index = pending.trailing_zeros() as u8;
        let vector = 0x0040 + (index as u16) * 8;
        self.ime = false;
        bus.ack_irq(index);
        self.pc = vector;
        true
    }

    fn read_reg(&self, r: Reg) -> u8 {
        match r {
            Reg::B => self.b, Reg::C => self.c,
            Reg::D => self.d, Reg::E => self.e,
            Reg::H => self.h, Reg::L => self.l,
            Reg::A => self.a,
            Reg::MemHL => unreachable!("MemHL requires a bus access, not read_reg"),
        }
    }

    fn write_reg(&mut self, r: Reg, v: u8) {
        match r {
            Reg::B => { self.b = v; }
            Reg::C => { self.c = v; }
            Reg::D => self.d = v, Reg::E => self.e = v,
            Reg::H => self.h = v, Reg::L => self.l = v,
            Reg::A => self.a = v,
            Reg::MemHL => unreachable!("MemHL requires a bus access, not write_reg"),
        }
    }

    fn read_reg16(&self, rr: Reg16) -> u16 {
        match rr {
            Reg16::BC => self.bc(),
            Reg16::DE => self.de(),
            Reg16::HL => self.hl(),
            Reg16::SP => self.sp,
        }
    }

    fn write_reg16(&mut self, r: Reg16, v: u16) {
        match r {
            Reg16::BC => { self.set_bc(v); }
            Reg16::DE => { self.set_de(v); }
            Reg16::HL => { self.set_hl(v); }
            Reg16::SP => self.sp = v,
        }
    }

    fn read_reg16_stk(&self, rr: Reg16Stk) -> u16 {
        match rr {
            Reg16Stk::BC => self.bc(),
            Reg16Stk::DE => self.de(),
            Reg16Stk::HL => self.hl(),
            Reg16Stk::AF => self.af(),
        }
    }

    fn write_reg16_stk(&mut self, rr: Reg16Stk, v: u16) {
        match rr {
            Reg16Stk::BC => self.set_bc(v),
            Reg16Stk::DE => self.set_de(v),
            Reg16Stk::HL => self.set_hl(v),
            Reg16Stk::AF => self.set_af(v), // set_af automatically masks lower 4 bits of F to 0
        }
    }
    
    fn alu(&mut self, op: AluOp, v: u8) {
        let a = self.a;
        let (res, f) = match op {
            AluOp::Add => {
                let res = a.wrapping_add(v);
                let h = (a & 0x0F) + (v & 0x0F) > 0x0F;
                let c = (a as u16) + (v as u16) > 0xFF;
                (res, (if res == 0 { FLAG_Z } else { 0 }) | (if h { FLAG_H } else { 0 }) | (if c { FLAG_C } else { 0 }))
            }
            AluOp::Adc => {
                let carry_in: u8 = if (self.f & FLAG_C) != 0 { 1 } else { 0 };
                let full = (a as u16) + (v as u16) + carry_in as u16;
                let res = full as u8;
                let h = ((a & 0x0F) + (v & 0x0F) + carry_in) > 0x0F;
                (res, (if res == 0 { FLAG_Z } else { 0 }) | (if h { FLAG_H } else { 0 }) | (if full > 0xFF { FLAG_C } else { 0 }))
            }
            AluOp::Sub | AluOp::Cp => {
                let res = a.wrapping_sub(v);
                let h = (a & 0x0F) < (v & 0x0F);
                let c = a < v;
                (res, FLAG_N | (if res == 0 { FLAG_Z } else { 0 }) | (if h { FLAG_H } else { 0 }) | (if c { FLAG_C } else { 0 }))
            }
            AluOp::Sbc => {
                let carry_in = if (self.f & FLAG_C) != 0 { 1 } else { 0 };
                let res = a.wrapping_sub(v).wrapping_sub(carry_in);
                let h = (a & 0x0F) < (v & 0x0F) + carry_in;
                let c = (a as u16) < (v as u16) + (carry_in as u16);
                (res, FLAG_N | (if res == 0 { FLAG_Z } else { 0 }) | (if h { FLAG_H } else { 0 }) | (if c { FLAG_C } else { 0 }))
            }
            AluOp::And => (a & v, FLAG_H | (if (a & v) == 0 { FLAG_Z } else { 0 })),
            AluOp::Xor => (a ^ v, if (a ^ v) == 0 { FLAG_Z } else { 0 }),
            AluOp::Or  => (a | v, if (a | v) == 0 { FLAG_Z } else { 0 }),
        };

        self.f = f & 0xF0;
        if op != AluOp::Cp {
            self.a = res;
        }
    }
}