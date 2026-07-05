/* TODO: Make sure to implement HALT bug 
    If HALT executed with IME=0 and a pending interrupt, the next byte after HALT gets executed twice.
*/

#[derive(Clone, Copy, PartialEq)]
pub enum GbVariant {
    Dmg,
    Mgb,
    Cgb,
    Sgb,
    Sgb2,
    Agb,
}

pub struct GbCpuConfig {
    pub variant: GbVariant,
    pub initial_a: u8,
    pub initial_f: u8,
    pub initial_bc: u16,
    pub initial_de: u16,
    pub initial_hl: u16,
    pub initial_sp: u16,
    pub supports_double_speed: bool,
}

impl GbCpuConfig {
    pub fn for_variant(variant: GbVariant) -> Self {
        match variant {
            GbVariant::Dmg => Self {
                variant, initial_a: 0x01, initial_f: 0xB0,
                initial_bc: 0x0013, initial_de: 0x00D8, initial_hl: 0x014D,
                initial_sp: 0xFFFE, supports_double_speed: false,
            },
            GbVariant::Mgb => Self {
                variant, initial_a: 0xFF, initial_f: 0xB0,
                initial_bc: 0x0013, initial_de: 0x00D8, initial_hl: 0x014D,
                initial_sp: 0xFFFE, supports_double_speed: false,
            },
            GbVariant::Cgb => Self {
                variant, initial_a: 0x11, initial_f: 0x80,
                initial_bc: 0x0000, initial_de: 0xFF56, initial_hl: 0x000D,
                initial_sp: 0xFFFE, supports_double_speed: true,
            },
            GbVariant::Agb => Self {
                variant, initial_a: 0x11, initial_f: 0x80,
                initial_bc: 0x0100, initial_de: 0xFF56, initial_hl: 0x000D,
                initial_sp: 0xFFFE, supports_double_speed: true,
            },
            GbVariant::Sgb | GbVariant::Sgb2 => Self {
                variant,
                initial_a: 0x01,  // Identifies as a classic monochrome Game Boy
                initial_f: 0xB0,
                initial_bc: 0x0013,
                initial_de: 0x00D8,
                initial_hl: 0x014D,
                initial_sp: 0xFFFE,
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
    pub cycles_remaining: u8,

    pub temp_16: u16,
    pub temp_8: u8,
    GbCpuConfig config,
}

impl GameBoyCpu {
    pub fn new(variant: CpuVariant) -> Self {
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
        }
    }
    pub fn bc(&self) -> u16 { ((self.b as u16) << 8) | self.c as u16 }
    pub fn set_bc(&mut self, v: u16) { self.b = (v >> 8) as u8; self.c = v as u8; }
    pub fn de(&self) -> u16 { ((self.d as u16) << 8) | self.e as u16 }
    pub fn set_de(&mut self, v: u16) { self.d = (v >> 8) as u8; self.e = v as u8; }
    pub fn hl(&self) -> u16 { ((self.h as u16) << 8) | self.l as u16 }
    pub fn set_hl(&mut self, v: u16) { self.h = (v >> 8) as u8; self.l = v as u8; }
    pub fn af(&self) -> u16 { ((self.a as u16) << 8) | self.f as u16 }
    pub fn set_af(&mut self, v: u16) { self.a = (v >> 8) as u8; self.f = v as u8; }
    fn get_r8_value(&self, reg_idx: u8) -> u8 {
        match reg_idx {
            0 => self.b, 1 => self.c, 2 => self.d, 3 => self.e,
            4 => self.h, 5 => self.l, 6 => 0, // (HL) - handled separately
            7 => self.a, _ => 0,
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
            // ---- THE PREFIX CB PIPELINE ----
            0xCB => {
                if self.instruction_step == 0 {
                    // ---- M-CYCLE 2: FETCH SUB-OPCODE ----
                    // This is where 0xDD is fetched from memory!
                    let sub_opcode = bus.read_byte(self.pc);
                    self.pc = self.pc.wrapping_add(1);

                    let reg_idx = sub_opcode & 0x07;

                    if reg_idx != 6 {
                        // This is a Register Instruction (like SET 3, L for 0xDD).
                        // It completes immediately on this M-cycle!
                        self.execute_cb_register_operation(sub_opcode);
                    } else {
                        // This is an (HL) memory instruction. Store the sub-opcode
                        // in our temp register buffer to process on the next M-cycles.
                        self.temp_8 = sub_opcode;
                    }
                } 
                else if self.instruction_step == 1 {
                    // ---- M-CYCLE 3: READ MEMORY (HL) ----
                    let addr = ((self.h as u16) << 8) | (self.l as u16);
                    self.temp_hl_val = bus.read_byte(addr); // cache current memory value
                } 
                else if self.instruction_step == 2 {
                    // ---- M-CYCLE 4: WRITE BACK TO MEMORY (HL) ----
                    let updated_val = self.calculate_cb_value(self.temp_8, self.temp_hl_val);
                    let addr = ((self.h as u16) << 8) | (self.l as u16);
                    bus.write_byte(addr, updated_val);
                }
            }
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
            0xCB => {
                // Peek at the upcoming sub-opcode byte right now to compute length
                let sub_byte = bus.peek_byte(self.pc); 
                if (sub_byte & 0x07) == 6 {
                    4 // Memory (HL) operations take 4 cycles total
                } else {
                    2 // Register operations take 2 cycles total
                }
            }
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

    fn execute_cb_register_operation(&mut self, sub_opcode: u8) {
        // Bits 6-7 define the operation category
        let category = (sub_opcode >> 6) & 0x03; 
        // Bits 3-5 define the bit index (0 to 7) being targeted
        let bit_idx = (sub_opcode >> 3) & 0x07;  
        // Bits 0-2 define the target register index
        let reg_idx = sub_opcode & 0x07;         

        // 1. Read the source value (Registers or Memory)
        let mut value = match reg_idx {
            0 => self.b, 1 => self.c, 2 => self.d, 3 => self.e,
            4 => self.h, 5 => self.l,
            6 => {
                // Memory operations (HL) require extra micro-steps!
                // We will map these out next.
                todo!("Handle memory (HL) cycles")
            }
            7 => self.a,
            _ => unreachable!(),
        };

        // 2. Perform the operation based on the category grid
        match category {
            0 => { // Category 0: Shifts and Rotates (RLC, RRC, RL, RR, SLA, SRA, SWAP, SRL)
                value = self.execute_shift_rotate(bit_idx, value);
            }
            1 => { // Category 1: BIT (Test if a bit is 0 or 1)
                let bit_set = (value & (1 << bit_idx)) != 0;
                // Update flags: Zero flag set if bit is 0, Subtract clear, Half-Carry set
                let old_carry = self.f & 0x10;
                let z_flag = if !bit_set { 0x80 } else { 0x00 };
                let h_flag = 0x20; // Half-carry is always set for BIT instructions

                self.f = z_flag | h_flag | old_carry;
                // BIT operations do not write back, they only update flags!
                return;
            }
            2 => { // Category 2: RES (Clear a specific bit)
                value &= !(1 << bit_idx);
            }
            3 => { // Category 3: SET (Set a specific bit)
                value |= 1 << bit_idx;
            }
            _ => unreachable!(),
        }

        // 3. Write back the updated value to the register
        match reg_idx {
            0 => self.b = value, 1 => self.c = value, 2 => self.d = value, 3 => self.e = value,
            4 => self.h = value, 5 => self.l = value,
            7 => self.a = value,
            _ => unreachable!(),
        }
    }
}