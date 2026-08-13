pub struct CpuConfig {
    has_bcd: bool,
    has_jmp_bug: bool,
    is_c02: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CpuVariant {
    ZilogZ80,
    ZilogZ180,
}

pub struct Z80Registers {
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
}

impl Z80Registers {
    pub fn bc(&self) -> u16 { ((self.b as u16) << 8) | self.c as u16 }
    pub fn set_bc(&mut self, v: u16) { self.b = (v >> 8) as u8; self.c = v as u8; }

    pub fn de(&self) -> u16 { ((self.d as u16) << 8) | self.e as u16 }
    pub fn set_de(&mut self, v: u16) { self.d = (v >> 8) as u8; self.e = v as u8; }

    pub fn hl(&self) -> u16 { ((self.h as u16) << 8) | self.l as u16 }
    pub fn set_hl(&mut self, v: u16) { self.h = (v >> 8) as u8; self.l = v as u8; }

    pub fn af(&self) -> u16 { ((self.a as u16) << 8) | self.f as u16 }
    pub fn set_af(&mut self, v: u16) { self.a = (v >> 8) as u8; self.f = v as u8; }

    pub fn read_r(&self, idx: u8, bus: &mut dyn Bus) -> u8 {
        match idx & 0x07 {
            0 => self.b, 1 => self.c, 2 => self.d, 3 => self.e,
            4 => self.h, 5 => self.l,
            6 => bus.read_byte(self.hl()),   // (HL) — the one index that hits memory, not a register
            7 => self.a,
            _ => unreachable!(),
        }
    }

    pub fn write_r(&mut self, idx: u8, value: u8, bus: &mut dyn Bus) {
        match idx & 0x07 {
            0 => self.b = value, 1 => self.c = value, 2 => self.d = value, 3 => self.e = value,
            4 => self.h = value, 5 => self.l = value,
            6 => bus.write_byte(self.hl(), value),
            7 => self.a = value,
            _ => unreachable!(),
        }
    }
}

pub struct Z80Cpu {
    pc: u16,
    sp: usize,
    regs: Z80Registers,
    nmi_pending: bool,
    prev_nmi_line: bool,
    last_cycles: u8,
    last_opcode: u8, // Save most recent instruction for debugging
    operand_address_crossed_page: bool,
    pub total_cycles: u64,
    pub is_running: bool,
    pub config: CpuConfig,
}

impl Z80Cpu {
    pub fn new() -> Self {
        Self {
            pc: 0,
            sp: 0,
        }
    }
    
    pub fn step_one_m_cycle(&mut self, bus: &mut GameBoyBus) {
        if self.cycles_remaining == 0 && self.handle_interrupts(bus) {
            return; 
        }

        if self.cycles_remaining == 0 {
            // ---- M-CYCLE 1: FETCH STAGE ----
            self.current_opcode = bus.read_byte(self.pc);
            self.pc = self.pc.wrapping_add(1);
            self.instruction_step = 0;
            self.cycles_remaining = self.get_instruction_m_cycles(self.current_opcode);
        } else {
            // ---- M-CYCLES 2+: EXECUTION PIPELINE MICRO-STEPS ----
            self.execute_micro_step(bus);
            self.instruction_step += 1;
        }

        self.cycles_remaining -= 1;
    }

    fn execute_micro_step(&mut self, bus: &mut GameBoyBus) {
        match self.current_opcode {
            // LD (HL), A
            0x77 => {
                if self.instruction_step == 0 {
                    let addr = ((self.h as u16) << 8) | (self.l as u16);
                    bus.write_byte(addr, self.a);
                }
            }
            // ... other multi-cycle opcodes
            _ => {}
        }
    }

    fn get_instruction_m_cycles(&self, opcode: u8) -> u8 {
        match opcode {
            // 1 M-cycle instructions (e.g., 8-bit register-to-register copies)
            0x7F | 0x40..=0x45 => 1, // LD A,A; LD B,B etc.

            // 2 M-cycle instructions (e.g., Immediate 8-bit loads, reading a byte from memory)
            0x06 | 0x0E | 0x16 | 0x1E | 0x26 | 0x2E => 2, // LD r, d8
            0x77 => 2, // LD (HL), A (You already have this one mapped!)

            // 3 M-cycle instructions (e.g., Loading an immediate 16-bit value like LD BC, d16)
            0x01 | 0x11 | 0x21 | 0x31 => 3, // LD rp, d16

            // 4 M-cycle instructions (e.g., Pushing to stack, writing an 8-bit register to an absolute 16-bit destination)
            0xEA => 4, // LD (a16), A
            0xC5 | 0xD5 | 0xE5 | 0xF5 => 4, // PUSH rp

            _ => todo!("Implement remaining opcode cycles mapping"),
        }
    }
}