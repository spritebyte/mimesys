// ---- CPSR bit layout ----
//  31 30 29 28 ... 7 6 5     4-0
//   N  Z  C  V      I F T   mode
const FLAG_N: u32 = 1 << 31;
const FLAG_Z: u32 = 1 << 30;
const FLAG_C: u32 = 1 << 29;
const FLAG_V: u32 = 1 << 28;
const FLAG_I: u32 = 1 << 7; // IRQ disable
const FLAG_F: u32 = 1 << 6; // FIQ disable
const FLAG_T: u32 = 1 << 5; // Thumb state
 
// ---- Processor modes (low 5 bits of CPSR) ----
const MODE_USR: u32 = 0x10;
const MODE_FIQ: u32 = 0x11;
const MODE_IRQ: u32 = 0x12;
const MODE_SVC: u32 = 0x13;
const MODE_ABT: u32 = 0x17;
const MODE_UND: u32 = 0x1B;
const MODE_SYS: u32 = 0x1F;

fn bank_index(mode: u8) -> usize {
    match mode & 0x1F {
        MODE_FIQ => 1,
        MODE_IRQ => 2,
        MODE_SVC => 3,
        MODE_ABT => 4,
        MODE_UND => 5,
        _ => 0, // USR / SYS / anything else -> base bank
    }
}

#[inline]
fn spsr_index(mode: u32) -> Option<usize> {
    match mode & 0x1F {
        MODE_FIQ => Some(0),
        MODE_IRQ => Some(1),
        MODE_SVC => Some(2),
        MODE_ABT => Some(3),
        MODE_UND => Some(4),
        _ => None,
    }
}

pub struct Arm7T {
    r: [u32; 16],    // general-purpose registers r0-r15.
    fiq_r8_r12: [u32; 5],
    banked_sp_lr: [[u32;2]; 6],
    cpsr: u32,       // Current program status register.
    spsr: [u32; 5],  // Saved program status register, 1 for each exception handling mode
    halted: bool,
    thumb_mode: bool, // set to true when bit 5 of CPSR is not zero.
    cycles_taken: u64,
}

impl Arm7T {
    pub fn new() -> Self {
        Self {
            r: [0; 16],
            banked_r8_r12: [0;5],
            banked_sp_lr: [[0;2];6],
            cpsr: MODE_SVC,
            spsr: [0;5],
            halted: false
            thumb_mode: false,
            cycles_taken: 0,
        }
    }

    

    #[inline(always)]
    pub fn pc(&self) -> u32 {
        self.r[15]
    }

    #[inline(always)]
    pub fn set_pc(&self, value: u32) -> u32 {
        self.r[15] = value;
    }

    #[inline(always)]
    pub fn in_arm_mode(&self) -> bool {
        self.cpsr & 0x20 == 0
    }

    fn switch_mode(&mut self, new_mode: u8) {
        let old_mode = self.cpsr & 0x1F;
        if old_mode == new_mode { return; }

        self.cpsr = (self.cpsr & !0x1F) | new_mode as u32;
    }

    pub fn step(&mut self, bus: &mut dyn Bus) {
        if self.halted {
            return 1;
        }

        if self.thumb_mode {
            let instruction = bus.read16(get_reg(pc));
            self.r[15] += 2;
        } else {
            let instruction = bus.read32(get_reg(pc));
            set_reg();
        }
    }

    fn execute_arm(&mut self, opcode: u32, _bus: &mut dyn Bus) -> u32 {
        // Decode by the classic ARM bit-pattern groups:
        //   branch (B/BL/BX), data-processing, PSR transfer (MRS/MSR),
        //   multiply, single/halfword load-store, block transfer, SWI, coproc.
        let _ = opcode;
        todo!("decode ARM instruction")
    }
 
    fn execute_thumb(&mut self, opcode: u16, _bus: &mut dyn Bus) -> u32 {
        let _ = opcode;
        todo!("decode Thumb instruction")
    }
}

pub trait Bus {
    fn read8(&mut self, addr: u32) -> u8;
    fn read16(&mut self, addr: u32) -> u16;
    fn read32(&mut self, addr: u32) -> u32;
    fn write8(&mut self, addr: u32, val: u8);
    fn write16(&mut self, addr: u32, val: u16);
    fn write32(&mut self, addr: u32, val: u32);
}