//! Intel MCS-48 CPU core (8035 / 8048 / Fujitsu MB8884).
//! Timing: one machine cycle = 15 oscillator clocks. `step()` returns machine
//! cycles (1 or 2 per instruction, +2 when it takes an interrupt).

pub trait Mcs48Io {
    /// Program memory, 12-bit address.
    fn read_rom(&mut self, addr: u16) -> u8;
    fn read_bus(&mut self, offset: u8) -> u8;
    fn write_bus(&mut self, _offset: u8, _val: u8) {}
    fn write_p1(&mut self, val: u8);
    fn read_p1(&mut self) -> u8 { 0xFF }
    fn write_p2(&mut self, val: u8);
    fn read_p2(&mut self) -> u8 { 0xFF }
    fn t0(&mut self) -> bool;
    fn t1(&mut self) -> bool;
    /// External interrupt line asserted (the /INT pin pulled low).
    fn int_requested(&mut self) -> bool;
}

const C_FLAG: u8 = 0x80;
const A_FLAG: u8 = 0x40;
const F_FLAG: u8 = 0x20;
const B_FLAG: u8 = 0x10;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TimeCount { Off, Timer, Counter }

pub struct Mcs48 {
    pub a: u8,
    pub pc: u16,          // 12 bits
    pub psw: u8,          // C AC F0 BS 1 SP2 SP1 SP0 (bit 3 reads as 1)
    pub f1: bool,
    a11: u16,             // SEL MB0/MB1 -> 0x000 / 0x800 for the next JMP/CALL
    pub ram: [u8; 64],    // 8035/MB8884: 64 bytes (registers, stack, scratch)
    p1: u8,
    p2: u8,

    timer: u8,
    prescaler: u8,
    timecount: TimeCount,
    timer_flag: bool,
    timer_overflow: bool,
    t1_history: u8,

    xirq_enabled: bool,
    tirq_enabled: bool,
    irq_in_progress: bool,
    irq_polled: bool,
    prev_pc: u16,

    cycles: u32,
}

impl Default for Mcs48 {
    fn default() -> Self { Self::new() }
}

impl Mcs48 {
    pub fn new() -> Self {
        let mut cpu = Self {
            a: 0, pc: 0, psw: 0, f1: false, a11: 0, ram: [0; 64], p1: 0xFF, p2: 0xFF,
            timer: 0, prescaler: 0, timecount: TimeCount::Off, timer_flag: false,
            timer_overflow: false, t1_history: 0,
            xirq_enabled: false, tirq_enabled: false, irq_in_progress: false,
            irq_polled: false, prev_pc: 0, cycles: 0,
        };
        cpu.reset();
        cpu
    }

    pub fn reset(&mut self) {
        self.pc = 0;
        self.psw &= C_FLAG | A_FLAG;
        self.f1 = false;
        self.a11 = 0;
        self.tirq_enabled = false;
        self.xirq_enabled = false;
        self.timecount = TimeCount::Off;
        self.timer_flag = false;
        self.irq_in_progress = false;
        self.timer_overflow = false;
        self.irq_polled = false;
        self.p1 = 0xFF;
        self.p2 = 0xFF;
    }

    pub fn reset_ports(&mut self, io: &mut dyn Mcs48Io) {
        io.write_p1(self.p1);
        io.write_p2(self.p2);
    }

    // ---------- small helpers ----------

    #[inline]
    fn reg_base(&self) -> usize { if self.psw & B_FLAG != 0 { 24 } else { 0 } }
    #[inline]
    fn r(&self, n: u8) -> u8 { self.ram[self.reg_base() + n as usize] }
    #[inline]
    fn set_r(&mut self, n: u8, v: u8) { let b = self.reg_base(); self.ram[b + n as usize] = v; }
    // Internal RAM is 64 bytes; higher addresses mirror.
    #[inline]
    fn ram_r(&self, addr: u8) -> u8 { self.ram[(addr & 0x3F) as usize] }
    #[inline]
    fn ram_w(&mut self, addr: u8, v: u8) { self.ram[(addr & 0x3F) as usize] = v; }

    /// Fetch a byte at PC. PC wraps within the current 2 KB bank (bit 11 kept).
    fn fetch(&mut self, io: &mut dyn Mcs48Io) -> u8 {
        let addr = self.pc;
        self.pc = ((self.pc + 1) & 0x7FF) | (self.pc & 0x800);
        io.read_rom(addr)
    }

    /// Count cycles and run the timer/counter.
    fn burn(&mut self, count: u32, io: &mut dyn Mcs48Io) {
        self.cycles += count;
        let mut overflow = false;
        match self.timecount {
            TimeCount::Off => {}
            TimeCount::Timer => {
                let old = self.timer;
                self.prescaler += count as u8;
                self.timer = self.timer.wrapping_add(self.prescaler >> 5);
                self.prescaler &= 0x1F;
                overflow = self.timer < old;
            }
            TimeCount::Counter => {
                for _ in 0..count {
                    self.t1_history = (self.t1_history << 1) | io.t1() as u8;
                    if self.t1_history & 3 == 2 {          // falling edge on T1
                        self.timer = self.timer.wrapping_add(1);
                        if self.timer == 0 { overflow = true; }
                    }
                }
            }
        }
        if overflow {
            self.timer_flag = true;
            if self.tirq_enabled { self.timer_overflow = true; }
        }
    }

    fn add(&mut self, v: u8, carry_in: bool) {
        let c = carry_in as u16;
        let t = self.a as u16 + v as u16 + c;
        let t4 = (self.a & 0x0F) as u16 + (v & 0x0F) as u16 + c;
        self.psw &= !(C_FLAG | A_FLAG);
        if t4 > 0x0F { self.psw |= A_FLAG; }
        if t > 0xFF { self.psw |= C_FLAG; }
        self.a = t as u8;
    }

    fn push_pc_psw(&mut self) {
        let sp = self.psw & 0x07;
        self.ram_w(8 + 2 * sp, self.pc as u8);
        self.ram_w(9 + 2 * sp, ((self.pc >> 8) as u8 & 0x0F) | (self.psw & 0xF0));
        self.psw = (self.psw & 0xF0) | ((sp + 1) & 0x07);
    }

    /// RET: restore PC only.
    fn pull_pc(&mut self) {
        let sp = self.psw.wrapping_sub(1) & 0x07;
        let lo = self.ram_r(8 + 2 * sp) as u16;
        let hi = self.ram_r(9 + 2 * sp) as u16;
        self.pc = (lo | (hi << 8)) & if self.irq_in_progress { 0x7FF } else { 0xFFF };
        self.psw = (self.psw & 0xF0) | sp;
    }

    /// RETR: restore PC and the upper PSW nibble.
    fn pull_pc_psw(&mut self) {
        let sp = self.psw.wrapping_sub(1) & 0x07;
        let lo = self.ram_r(8 + 2 * sp) as u16;
        let hi = self.ram_r(9 + 2 * sp);
        self.pc = (lo | ((hi as u16 & 0x0F) << 8)) & if self.irq_in_progress { 0x7FF } else { 0xFFF };
        self.psw = (hi & 0xF0) | sp;
    }

    fn jmp(&mut self, addr: u16) {
        let a11 = if self.irq_in_progress { 0 } else { self.a11 };
        self.pc = addr | a11;
    }

    fn call(&mut self, addr: u16) {
        self.push_pc_psw();
        self.jmp(addr);
    }

    fn jcc(&mut self, cond: bool, io: &mut dyn Mcs48Io) {
        let page = self.pc & 0xF00;
        let target = self.fetch(io) as u16;
        if cond { self.pc = page | target; }
    }

    fn check_irqs(&mut self, io: &mut dyn Mcs48Io) {
        if self.irq_in_progress { return; }
        if self.xirq_enabled && io.int_requested() {
            self.burn(2, io);
            self.irq_in_progress = true;
            if self.irq_polled {
                self.pc = ((self.prev_pc + 1) & 0x7FF) | (self.prev_pc & 0x800);
                self.jcc(true, io);
            }
            self.call(0x003);
        } else if self.tirq_enabled && self.timer_overflow {
            self.burn(2, io);
            self.irq_in_progress = true;
            self.call(0x007);
            self.timer_overflow = false;
        }
    }

    /// Run one instruction (taking a pending interrupt first). Returns machine cycles.
    pub fn step(&mut self, io: &mut dyn Mcs48Io) -> u32 {
        self.cycles = 0;
        self.check_irqs(io);
        self.irq_polled = false;
        self.prev_pc = self.pc;
        let op = self.fetch(io);
        self.execute(op, io);
        self.cycles
    }

    fn execute(&mut self, op: u8, io: &mut dyn Mcs48Io) {
        let rn = op & 0x07;      // register number for the Rn groups
        let ri = op & 0x01;      // R0 / R1 for the @Ri groups

        match op {
            // ----- register groups (low 3 bits = register) -----
            0x18..=0x1F => { self.burn(1, io); let v = self.r(rn).wrapping_add(1); self.set_r(rn, v); }   // INC Rn
            0xC8..=0xCF => { self.burn(1, io); let v = self.r(rn).wrapping_sub(1); self.set_r(rn, v); }   // DEC Rn
            0x28..=0x2F => { self.burn(1, io); let t = self.a; self.a = self.r(rn); self.set_r(rn, t); }  // XCH A,Rn
            0x48..=0x4F => { self.burn(1, io); self.a |= self.r(rn); }                                    // ORL A,Rn
            0x58..=0x5F => { self.burn(1, io); self.a &= self.r(rn); }                                    // ANL A,Rn
            0xD8..=0xDF => { self.burn(1, io); self.a ^= self.r(rn); }                                    // XRL A,Rn
            0x68..=0x6F => { self.burn(1, io); let v = self.r(rn); self.add(v, false); }                  // ADD A,Rn
            0x78..=0x7F => { self.burn(1, io); let v = self.r(rn); let c = self.psw & C_FLAG != 0; self.add(v, c); } // ADDC A,Rn
            0xA8..=0xAF => { self.burn(1, io); self.set_r(rn, self.a); }                                  // MOV Rn,A
            0xF8..=0xFF => { self.burn(1, io); self.a = self.r(rn); }                                     // MOV A,Rn
            0xB8..=0xBF => { self.burn(2, io); let n = self.fetch(io); self.set_r(rn, n); }               // MOV Rn,#n
            0xE8..=0xEF => {                                                                               // DJNZ Rn,addr
                self.burn(2, io);
                let v = self.r(rn).wrapping_sub(1);
                self.set_r(rn, v);
                self.jcc(v != 0, io);
            }

            // ----- @R0 / @R1 groups -----
            0x10 | 0x11 => { self.burn(1, io); let p = self.r(ri); let v = self.ram_r(p).wrapping_add(1); self.ram_w(p, v); } // INC @Ri
            0x20 | 0x21 => { self.burn(1, io); let p = self.r(ri); let t = self.a; self.a = self.ram_r(p); self.ram_w(p, t); } // XCH A,@Ri
            0x30 | 0x31 => {                                                                               // XCHD A,@Ri
                self.burn(1, io);
                let p = self.r(ri);
                let old = self.ram_r(p);
                self.ram_w(p, (old & 0xF0) | (self.a & 0x0F));
                self.a = (self.a & 0xF0) | (old & 0x0F);
            }
            0x40 | 0x41 => { self.burn(1, io); let p = self.r(ri); self.a |= self.ram_r(p); }             // ORL A,@Ri
            0x50 | 0x51 => { self.burn(1, io); let p = self.r(ri); self.a &= self.ram_r(p); }             // ANL A,@Ri
            0xD0 | 0xD1 => { self.burn(1, io); let p = self.r(ri); self.a ^= self.ram_r(p); }             // XRL A,@Ri
            0x60 | 0x61 => { self.burn(1, io); let p = self.r(ri); let v = self.ram_r(p); self.add(v, false); } // ADD A,@Ri
            0x70 | 0x71 => { self.burn(1, io); let p = self.r(ri); let v = self.ram_r(p); let c = self.psw & C_FLAG != 0; self.add(v, c); } // ADDC A,@Ri
            0xA0 | 0xA1 => { self.burn(1, io); let p = self.r(ri); self.ram_w(p, self.a); }               // MOV @Ri,A
            0xB0 | 0xB1 => { self.burn(2, io); let p = self.r(ri); let n = self.fetch(io); self.ram_w(p, n); } // MOV @Ri,#n
            0xF0 | 0xF1 => { self.burn(1, io); let p = self.r(ri); self.a = self.ram_r(p); }              // MOV A,@Ri
            0x80 | 0x81 => { self.burn(2, io); let p = self.r(ri); self.a = io.read_bus(p); }             // MOVX A,@Ri
            0x90 | 0x91 => { self.burn(2, io); let p = self.r(ri); io.write_bus(p, self.a); }             // MOVX @Ri,A

            // ----- JMP / CALL / JBb (top 3 bits = page or bit number) -----
            _ if op & 0x1F == 0x04 => { self.burn(2, io); let lo = self.fetch(io) as u16; self.jmp(((op as u16 >> 5) << 8) | lo); }  // JMP
            _ if op & 0x1F == 0x14 => { self.burn(2, io); let lo = self.fetch(io) as u16; self.call(((op as u16 >> 5) << 8) | lo); } // CALL
            _ if op & 0x1F == 0x12 => { self.burn(2, io); let bit = op >> 5; self.jcc(self.a & (1 << bit) != 0, io); }              // JBb

            // ----- immediate ALU -----
            0x03 => { self.burn(2, io); let n = self.fetch(io); self.add(n, false); }                         // ADD A,#n
            0x13 => { self.burn(2, io); let n = self.fetch(io); let c = self.psw & C_FLAG != 0; self.add(n, c); } // ADDC A,#n
            0x23 => { self.burn(2, io); self.a = self.fetch(io); }                                            // MOV A,#n
            0x43 => { self.burn(2, io); self.a |= self.fetch(io); }                                           // ORL A,#n
            0x53 => { self.burn(2, io); self.a &= self.fetch(io); }                                           // ANL A,#n
            0xD3 => { self.burn(2, io); self.a ^= self.fetch(io); }                                           // XRL A,#n

            // ----- accumulator -----
            0x07 => { self.burn(1, io); self.a = self.a.wrapping_sub(1); }                                    // DEC A
            0x17 => { self.burn(1, io); self.a = self.a.wrapping_add(1); }                                    // INC A
            0x27 => { self.burn(1, io); self.a = 0; }                                                         // CLR A
            0x37 => { self.burn(1, io); self.a ^= 0xFF; }                                                     // CPL A
            0x47 => { self.burn(1, io); self.a = self.a.rotate_left(4); }                                     // SWAP A
            0x57 => {                                                                                          // DA A
                self.burn(1, io);
                if (self.a & 0x0F) > 0x09 || self.psw & A_FLAG != 0 {
                    if self.a > 0xF9 { self.psw |= C_FLAG; }
                    self.a = self.a.wrapping_add(0x06);
                }
                if (self.a & 0xF0) > 0x90 || self.psw & C_FLAG != 0 {
                    self.a = self.a.wrapping_add(0x60);
                    self.psw |= C_FLAG;
                }
            }
            0x77 => { self.burn(1, io); self.a = self.a.rotate_right(1); }                                    // RR A
            0xE7 => { self.burn(1, io); self.a = self.a.rotate_left(1); }                                     // RL A
            0x67 => {                                                                                          // RRC A
                self.burn(1, io);
                let new_c = (self.a << 7) & C_FLAG;
                self.a = (self.a >> 1) | (self.psw & C_FLAG);
                self.psw = (self.psw & !C_FLAG) | new_c;
            }
            0xF7 => {                                                                                          // RLC A
                self.burn(1, io);
                let new_c = self.a & C_FLAG;
                self.a = (self.a << 1) | (self.psw >> 7);
                self.psw = (self.psw & !C_FLAG) | new_c;
            }

            // ----- flags -----
            0x97 => { self.burn(1, io); self.psw &= !C_FLAG; }    // CLR C
            0xA7 => { self.burn(1, io); self.psw ^= C_FLAG; }     // CPL C
            0x85 => { self.burn(1, io); self.psw &= !F_FLAG; }    // CLR F0
            0x95 => { self.burn(1, io); self.psw ^= F_FLAG; }     // CPL F0
            0xA5 => { self.burn(1, io); self.f1 = false; }        // CLR F1
            0xB5 => { self.burn(1, io); self.f1 = !self.f1; }     // CPL F1
            0xC7 => { self.burn(1, io); self.a = self.psw | 0x08; }                    // MOV A,PSW
            0xD7 => { self.burn(1, io); self.psw = self.a & !0x08; }                   // MOV PSW,A
            0xC5 => { self.burn(1, io); self.psw &= !B_FLAG; }    // SEL RB0
            0xD5 => { self.burn(1, io); self.psw |= B_FLAG; }     // SEL RB1
            0xE5 => { self.burn(1, io); self.a11 = 0x000; }       // SEL MB0
            0xF5 => { self.burn(1, io); self.a11 = 0x800; }       // SEL MB1

            // ----- conditional jumps -----
            0x16 => { self.burn(2, io); let f = self.timer_flag; self.jcc(f, io); self.timer_flag = false; } // JTF
            0x26 => { self.burn(2, io); let t = io.t0(); self.jcc(!t, io); }          // JNT0
            0x36 => { self.burn(2, io); let t = io.t0(); self.jcc(t, io); }           // JT0
            0x46 => { self.burn(2, io); let t = io.t1(); self.jcc(!t, io); }          // JNT1
            0x56 => { self.burn(2, io); let t = io.t1(); self.jcc(t, io); }           // JT1
            0x76 => { self.burn(2, io); let f = self.f1; self.jcc(f, io); }           // JF1
            0x86 => {                                                                 // JNI (jump if /INT low)
                self.burn(2, io);
                let irq = io.int_requested();
                self.irq_polled = !irq;
                self.jcc(irq, io);
            }
            0x96 => { self.burn(2, io); let z = self.a != 0; self.jcc(z, io); }       // JNZ
            0xB6 => { self.burn(2, io); let f = self.psw & F_FLAG != 0; self.jcc(f, io); } // JF0
            0xC6 => { self.burn(2, io); let z = self.a == 0; self.jcc(z, io); }       // JZ
            0xE6 => { self.burn(2, io); let c = self.psw & C_FLAG == 0; self.jcc(c, io); } // JNC
            0xF6 => { self.burn(2, io); let c = self.psw & C_FLAG != 0; self.jcc(c, io); } // JC

            // ----- returns, table jumps, program-memory reads -----
            0x83 => { self.burn(2, io); self.pull_pc(); }                              // RET
            0x93 => { self.burn(2, io); self.irq_in_progress = false; self.pull_pc_psw(); } // RETR
            0xB3 => {                                                                  // JMPP @A
                self.burn(2, io);
                let target = io.read_rom((self.pc & 0xF00) | self.a as u16) as u16;
                self.pc = (self.pc & 0xF00) | target;
            }
            0xA3 => { self.burn(2, io); self.a = io.read_rom((self.pc & 0xF00) | self.a as u16); } // MOVP A,@A
            0xE3 => { self.burn(2, io); self.a = io.read_rom(0x300 | self.a as u16); }             // MOVP3 A,@A

            // ----- ports and bus -----
            0x08 => { self.burn(2, io); self.a = io.read_bus(0); }                                      // INS A,BUS
            0x02 => { self.burn(2, io); io.write_bus(0, self.a); }                                      // OUTL BUS,A
            0x88 => { self.burn(2, io); let n = self.fetch(io); let v = io.read_bus(0) | n; io.write_bus(0, v); } // ORL BUS,#n
            0x98 => { self.burn(2, io); let n = self.fetch(io); let v = io.read_bus(0) & n; io.write_bus(0, v); } // ANL BUS,#n
            0x09 => { self.burn(2, io); self.a = io.read_p1() & self.p1; }                              // IN A,P1
            0x0A => { self.burn(2, io); self.a = io.read_p2() & self.p2; }                              // IN A,P2
            0x39 => { self.burn(2, io); self.p1 = self.a; io.write_p1(self.p1); }                       // OUTL P1,A
            0x3A => { self.burn(2, io); self.p2 = self.a; io.write_p2(self.p2); }                       // OUTL P2,A
            0x89 => { self.burn(2, io); let n = self.fetch(io); self.p1 |= n; io.write_p1(self.p1); }   // ORL P1,#n
            0x8A => { self.burn(2, io); let n = self.fetch(io); self.p2 |= n; io.write_p2(self.p2); }   // ORL P2,#n
            0x99 => { self.burn(2, io); let n = self.fetch(io); self.p1 &= n; io.write_p1(self.p1); }   // ANL P1,#n
            0x9A => { self.burn(2, io); let n = self.fetch(io); self.p2 &= n; io.write_p2(self.p2); }   // ANL P2,#n

            // 8243 port expander (MOVD / ANLD / ORLD): not wired on Donkey Kong
            0x0C..=0x0F | 0x3C..=0x3F | 0x8C..=0x8F | 0x9C..=0x9F => { self.burn(2, io); }

            // ----- timer / counter / interrupts -----
            0x42 => { self.burn(1, io); self.a = self.timer; }                                          // MOV A,T
            0x62 => { self.burn(1, io); self.timer = self.a; }                                          // MOV T,A
            0x55 => { self.burn(1, io); self.timecount = TimeCount::Timer; self.prescaler = 0; }        // STRT T
            0x45 => {                                                                                   // STRT CNT
                self.burn(1, io);
                if self.timecount != TimeCount::Counter { self.t1_history = io.t1() as u8; }
                self.timecount = TimeCount::Counter;
            }
            0x65 => { self.burn(1, io); self.timecount = TimeCount::Off; }                              // STOP TCNT
            0x25 => { self.burn(1, io); self.tirq_enabled = true; }                                     // EN TCNTI
            0x35 => { self.burn(1, io); self.tirq_enabled = false; self.timer_overflow = false; }       // DIS TCNTI
            0x05 => { self.burn(1, io); self.xirq_enabled = true; }                                     // EN I
            0x15 => { self.burn(1, io); self.xirq_enabled = false; }                                    // DIS I
            0x75 => { self.burn(1, io); }                                                               // ENT0 CLK (no clock output here)

            0x00 => { self.burn(1, io); }                                                               // NOP

            // Everything left is undefined on the 8035; MAME treats it as a 1-cycle no-op.
            _ => { self.burn(1, io); }
        }
    }
}
