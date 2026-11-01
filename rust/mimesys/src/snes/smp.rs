//! The S-SMP: the SNES sound CPU (SPC700, `common/cpu/spc700.rs`) with its 64 KB of RAM,
//! I/O registers (F0-FF), three timers, the four ports to the main CPU and a 64-byte
//! boot ROM. Behaviour follows ares's `sfc/smp` (ISC). The DSP is only a register file
//! here (`dsp_regs`); stage 3 adds the voices.
//!
//! Clocking: the APU crystal is 24.607 MHz. An SMP cycle is 24 of those clocks at the
//! default wait states (ares: steps of 2 on a /12 clock). The SMP runs in step with the
//! main CPU: `run` is called with every slice of master clocks, and the ports catch the
//! SMP up before the CPU touches them, so the CPU never sees the SMP behind it.
//!
//! The boot ROM is our own (Nintendo's is not shipped). It does the same job with the
//! same protocol, entry point (FFC0) and stack: clear zero page, show AA BB, wait for CC,
//! then take blocks: port 2/3 = address, port 1 = 0 to jump there or non-zero for data,
//! port 0 = byte index (echoed), a jump of 2+ in port 0 starts the next command.

use serde::{Deserialize, Serialize};
use crate::common::cpu::spc700::{Spc700, SpcBus, P};

pub const APU_HZ: i64 = 32_040 * 768;       // 24.607 MHz
const MASTER_HZ: i64 = 21_477_272;

/// mimesys's IPL (hand-assembled SPC700, 64 bytes at FFC0).
pub const IPL: [u8; 64] = [
    0xCD, 0xEF,             // FFC0 mov x,#$EF
    0xBD,                   // FFC2 mov sp,x
    0xE8, 0x00,             // FFC3 mov a,#0
    0xC6,                   // FFC5 clear: mov (x),a
    0x1D,                   // FFC6 dec x
    0xD0, 0xFC,             // FFC7 bne clear
    0x8F, 0xAA, 0xF4,       // FFC9 mov $F4,#$AA
    0x8F, 0xBB, 0xF5,       // FFCC mov $F5,#$BB
    0x78, 0xCC, 0xF4,       // FFCF ready: cmp $F4,#$CC
    0xD0, 0xFB,             // FFD2 bne ready
    0xBA, 0xF6,             // FFD4 start: movw ya,$F6      (address)
    0xDA, 0x00,             // FFD6 movw $00,ya
    0xBA, 0xF4,             // FFD8 movw ya,$F4             (A = port 0, Y = command)
    0xC4, 0xF4,             // FFDA mov $F4,a               (echo)
    0xDD,                   // FFDC mov a,y
    0x5D,                   // FFDD mov x,a
    0xF0, 0x1B,             // FFDE beq exec
    0x8D, 0x00,             // FFE0 mov y,#0
    0x7E, 0xF4,             // FFE2 loop: cmp y,$F4
    0xD0, 0x0D,             // FFE4 bne other
    0xE4, 0xF5,             // FFE6 mov a,$F5
    0xCB, 0xF4,             // FFE8 mov $F4,y
    0xD7, 0x00,             // FFEA mov [$00]+y,a
    0xFC,                   // FFEC inc y
    0xD0, 0xF3,             // FFED bne loop
    0xAB, 0x01,             // FFEF inc $01
    0x2F, 0xEF,             // FFF1 bra loop
    0x10, 0xED,             // FFF3 other: bpl loop         (an old index: keep waiting)
    0x7E, 0xF4,             // FFF5 cmp y,$F4               (read twice, as the real one)
    0x10, 0xE9,             // FFF7 bpl loop
    0x2F, 0xD9,             // FFF9 bra start
    0x1F, 0x00, 0x00,       // FFFB exec: jmp [$0000+x]     (x = 0)
    0xC0, 0xFF,             // FFFE reset vector
];

#[derive(Clone, Default, Serialize, Deserialize)]
struct Timer {
    stage0: u32,
    stage1: bool,
    stage2: u8,
    stage3: u8,
    line: bool,
    enable: bool,
    target: u8,
    period: u32,
}

impl Timer {
    fn new(period: u32) -> Self { Self { period, ..Default::default() } }
    fn step(&mut self, clocks: u32, running: bool) {
        self.stage0 += clocks;
        if self.stage0 < self.period { return; }
        self.stage0 -= self.period;
        self.stage1 = !self.stage1;
        self.sync(running);
    }
    /// Count on a 1 -> 0 edge of stage 1 (gated by the TEST register).
    fn sync(&mut self, running: bool) {
        let level = self.stage1 && running;
        let fell = self.line && !level;
        self.line = level;
        if !fell || !self.enable { return; }
        self.stage2 = self.stage2.wrapping_add(1);
        if self.stage2 != self.target { return; }
        self.stage2 = 0;
        self.stage3 = (self.stage3 + 1) & 15;
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Mem {
    #[serde(with = "serde_bytes")]
    pub ram: Vec<u8>,
    pub dsp_regs: Vec<u8>,
    pub dsp_addr: u8,
    /// CPU -> SMP (F4-F7 reads) and SMP -> CPU (2140-2143 reads)
    pub from_cpu: [u8; 4],
    pub to_cpu: [u8; 4],
    aux: [u8; 2],
    timers_disable: bool,
    ram_writable: bool,
    ram_disable: bool,
    timers_enable: bool,
    ext_wait: u8,
    int_wait: u8,
    ipl_enable: bool,
    timers: [Timer; 3],
    /// P flag of the CPU (TEST writes are ignored while it is set)
    p_flag: bool,
    /// APU clocks used by the current instruction
    pub used: u32,
}

impl Mem {
    fn new() -> Self {
        Self {
            ram: vec![0; 0x10000], dsp_regs: vec![0; 128], dsp_addr: 0,
            from_cpu: [0; 4], to_cpu: [0; 4], aux: [0; 2],
            timers_disable: false, ram_writable: true, ram_disable: false, timers_enable: true,
            ext_wait: 0, int_wait: 0, ipl_enable: true,
            timers: [Timer::new(128), Timer::new(128), Timer::new(16)],
            p_flag: false, used: 0,
        }
    }

    fn running(&self) -> bool { self.timers_enable && !self.timers_disable }

    /// ares SMP::wait: one access, in APU clocks (24 per cycle at the default wait states).
    fn wait(&mut self, halve: bool, addr: Option<u16>) {
        const CYCLE: [u32; 4] = [2, 4, 10, 20];
        const TIMER: [u32; 4] = [2, 4, 8, 16];
        let ws = match addr {
            None => self.int_wait,
            Some(a) if a & 0xFFF0 == 0x00F0 => self.int_wait,
            Some(a) if a >= 0xFFC0 && self.ipl_enable => self.int_wait,
            Some(_) => self.ext_wait,
        } as usize;
        self.used += (CYCLE[ws] >> halve as u32) * 12;
        let run = self.running();
        for t in &mut self.timers { t.step(TIMER[ws] >> halve as u32, run); }
    }

    fn read_ram(&self, a: u16) -> u8 {
        if a >= 0xFFC0 && self.ipl_enable { return IPL[(a & 0x3F) as usize]; }
        if self.ram_disable { return 0x5A; }
        self.ram[a as usize]
    }

    fn read_io(&mut self, a: u16) -> u8 {
        match a {
            0xF2 => self.dsp_addr,
            0xF3 => self.dsp_regs[(self.dsp_addr & 0x7F) as usize],
            0xF4..=0xF7 => self.from_cpu[(a - 0xF4) as usize],
            0xF8 | 0xF9 => self.aux[(a - 0xF8) as usize],
            0xFD..=0xFF => { let t = &mut self.timers[(a - 0xFD) as usize]; let v = t.stage3; t.stage3 = 0; v }
            _ => 0,
        }
    }

    fn write_io(&mut self, a: u16, v: u8) {
        match a {
            0xF0 => {
                if self.p_flag { return; }
                self.timers_disable = v & 1 != 0;
                self.ram_writable = v & 2 != 0;
                self.ram_disable = v & 4 != 0;
                self.timers_enable = v & 8 != 0;
                self.ext_wait = v >> 4 & 3;
                self.int_wait = v >> 6;
                let run = self.running();
                for t in &mut self.timers { t.sync(run); }
            }
            0xF1 => {
                for (i, t) in self.timers.iter_mut().enumerate() {
                    let on = v >> i & 1 != 0;
                    if on && !t.enable { t.stage2 = 0; t.stage3 = 0; }
                    t.enable = on;
                }
                if v & 0x10 != 0 { self.from_cpu[0] = 0; self.from_cpu[1] = 0; }
                if v & 0x20 != 0 { self.from_cpu[2] = 0; self.from_cpu[3] = 0; }
                self.ipl_enable = v & 0x80 != 0;
            }
            0xF2 => self.dsp_addr = v,
            0xF3 => {
                if self.dsp_addr & 0x80 != 0 { return; }
                let r = self.dsp_addr as usize;
                // ENDX: any write clears it
                self.dsp_regs[r] = if r == 0x7C { 0 } else { v };
            }
            0xF4..=0xF7 => self.to_cpu[(a - 0xF4) as usize] = v,
            0xF8 | 0xF9 => self.aux[(a - 0xF8) as usize] = v,
            0xFA..=0xFC => self.timers[(a - 0xFA) as usize].target = v,
            _ => {}
        }
    }
}

impl SpcBus for Mem {
    fn read(&mut self, a: u16) -> u8 {
        let port = a & 0xFFFC == 0x00F4;
        self.wait(port, Some(a));
        let mut v = self.read_ram(a);
        if a & 0xFFF0 == 0x00F0 { v = self.read_io(a); }
        if port { self.wait(true, Some(a)); }
        v
    }
    fn write(&mut self, a: u16, v: u8) {
        self.wait(false, Some(a));
        if self.ram_writable && !self.ram_disable { self.ram[a as usize] = v; }
        if a & 0xFFF0 == 0x00F0 { self.write_io(a, v); }
    }
    fn idle(&mut self) { self.wait(false, None); }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Smp {
    pub cpu: Spc700,
    pub mem: Mem,
    /// master-clock time owed to the SMP, scaled (master * APU_HZ - apu * MASTER_HZ)
    debt: i64,
}

impl Smp {
    pub fn new() -> Self {
        let mut cpu = Spc700::new();
        cpu.pc = 0xFFC0;
        Self { cpu, mem: Mem::new(), debt: 0 }
    }

    /// Master clocks pass: run the SMP up to the same point in time.
    pub fn run(&mut self, master: u32) {
        self.debt += master as i64 * APU_HZ;
        while self.debt > 0 {
            self.mem.used = 0;
            self.mem.p_flag = self.cpu.p & P != 0;
            self.cpu.step(&mut self.mem);
            self.debt -= self.mem.used as i64 * MASTER_HZ;
        }
    }

    pub fn cpu_read(&mut self, port: usize) -> u8 { self.mem.to_cpu[port] }
    pub fn cpu_write(&mut self, port: usize, v: u8) { self.mem.from_cpu[port] = v; }
}
