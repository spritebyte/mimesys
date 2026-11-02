//! NEC µPD7725: the DSP-1 / 1B / 2 / 3 / 4 maths chips (Mario Kart, Pilotwings, Dungeon
//! Master, SD Gundam GX, Top Gear 3000). A port of ares's `component/processor/upd96050`
//! (µPD7725 part) and `sfc/coprocessor/necdsp` (ISC, Near and the ares team; see
//! THIRD_PARTY_NOTICES.md), rewritten in Rust. The chip runs Nintendo's firmware, which
//! is not shipped: the player provides it (`dsp1b.rom` etc., 8 KB: 2048 24-bit program
//! words then 1024 16-bit data words, little-endian).
//!
//! The S-CPU talks to it through DR (data) and SR (status), chosen by one address bit.
//! It runs at 7.6 MHz, one instruction a clock, in step with the S-CPU.

use serde::{Deserialize, Serialize};

const DSP_HZ: i64 = 7_600_000;
const MASTER_HZ: i64 = 21_477_272;

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
struct Flag { ov0: bool, ov1: bool, z: bool, c: bool, s0: bool, s1: bool }

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
struct Status {
    p0: bool, p1: bool, ei: bool, sic: bool, soc: bool, drc: bool, dma: bool, drs: bool,
    usf0: bool, usf1: bool, rqm: bool, siack: bool, soack: bool,
}

impl Status {
    fn get(&self) -> u16 {
        let drs = self.drs && !self.drc;
        self.p0 as u16 | (self.p1 as u16) << 1 | (self.ei as u16) << 7 | (self.sic as u16) << 8 | (self.soc as u16) << 9
            | (self.drc as u16) << 10 | (self.dma as u16) << 11 | (drs as u16) << 12 | (self.usf0 as u16) << 13
            | (self.usf1 as u16) << 14 | (self.rqm as u16) << 15
    }
    fn set(&mut self, v: u16) {
        let b = |n: u32| v >> n & 1 != 0;
        self.p0 = b(0); self.p1 = b(1); self.ei = b(7); self.sic = b(8); self.soc = b(9); self.drc = b(10);
        self.dma = b(11); self.drs = b(12); self.usf0 = b(13); self.usf1 = b(14); self.rqm = b(15);
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct NecDsp {
    #[serde(skip)]
    program: Vec<u32>,
    #[serde(skip)]
    data_rom: Vec<u16>,
    data_ram: Vec<u16>,
    stack: [u16; 16],
    pc: u16,
    rp: u16,
    dp: u16,
    sp: u8,
    si: u16,
    so: u16,
    k: i16,
    l: i16,
    m: i16,
    n: i16,
    a: i16,
    b: i16,
    tr: u16,
    trb: u16,
    dr: u16,
    sr: Status,
    fa: Flag,
    fb: Flag,
    debt: i64,
}

impl NecDsp {
    /// From an 8 KB firmware image (program ROM then data ROM).
    pub fn new(firmware: &[u8]) -> Self {
        let mut program = vec![0u32; 2048];
        let mut data_rom = vec![0u16; 1024];
        for (i, w) in program.iter_mut().enumerate() {
            let o = i * 3;
            *w = firmware[o] as u32 | (firmware[o + 1] as u32) << 8 | (firmware[o + 2] as u32) << 16;
        }
        for (i, w) in data_rom.iter_mut().enumerate() {
            let o = 0x1800 + i * 2;
            *w = firmware[o] as u16 | (firmware[o + 1] as u16) << 8;
        }
        Self {
            program, data_rom, data_ram: vec![0; 256], stack: [0; 16], pc: 0, rp: 0, dp: 0, sp: 0, si: 0, so: 0,
            k: 0, l: 0, m: 0, n: 0, a: 0, b: 0, tr: 0, trb: 0, dr: 0, sr: Status::default(),
            fa: Flag::default(), fb: Flag::default(), debt: 0,
        }
    }

    /// After loading a save state: the firmware isn't in the state.
    pub fn keep_firmware(&mut self, old: &NecDsp) {
        self.program = old.program.clone();
        self.data_rom = old.data_rom.clone();
    }

    /// Master clocks pass: run the DSP up to the same point in time.
    pub fn run(&mut self, clocks: u32) {
        self.debt += clocks as i64 * DSP_HZ;
        while self.debt > 0 {
            self.exec();
            self.debt -= MASTER_HZ;
        }
    }

    // ---------------- the S-CPU side ----------------

    pub fn read(&mut self, sr: bool) -> u8 {
        if sr { return (self.sr.get() >> 8) as u8; }
        if !self.sr.drc {
            if !self.sr.drs {
                self.sr.drs = true;
                self.dr as u8
            } else {
                self.sr.rqm = false;
                self.sr.drs = false;
                (self.dr >> 8) as u8
            }
        } else {
            self.sr.rqm = false;
            self.dr as u8
        }
    }

    pub fn write(&mut self, sr: bool, v: u8) {
        if sr { return; }
        if !self.sr.drc {
            if !self.sr.drs {
                self.sr.drs = true;
                self.dr = (self.dr & 0xFF00) | v as u16;
            } else {
                self.sr.rqm = false;
                self.sr.drs = false;
                self.dr = (v as u16) << 8 | (self.dr & 0xFF);
            }
        } else {
            self.sr.rqm = false;
            self.dr = (self.dr & 0xFF00) | v as u16;
        }
    }

    // ---------------- the chip ----------------

    fn exec(&mut self) {
        let op = self.program[(self.pc & 0x7FF) as usize];
        self.pc = (self.pc + 1) & 0x7FF;
        match op >> 22 {
            0 => self.exec_op(op),
            1 => { self.exec_op(op); self.sp = self.sp.wrapping_sub(1) & 15; self.pc = self.stack[self.sp as usize] & 0x7FF; }
            2 => self.exec_jp(op),
            _ => self.exec_ld(op),
        }
        let result = self.k as i32 * self.l as i32;
        self.m = (result >> 15) as i16;
        self.n = (result << 1) as i16;
    }

    fn exec_op(&mut self, op: u32) {
        let pselect = op >> 20 & 3;
        let alu = op >> 16 & 15;
        let asl = op >> 15 & 1;
        let dpl = op >> 13 & 3;
        let dphm = (op >> 9 & 15) as u16;
        let rpdcr = op >> 8 & 1;
        let src = op >> 4 & 15;
        let dst = op & 15;

        let idb: u16 = match src {
            0 => self.trb,
            1 => self.a as u16,
            2 => self.b as u16,
            3 => self.tr,
            4 => self.dp,
            5 => self.rp,
            6 => self.data_rom[(self.rp & 0x3FF) as usize],
            7 => 0x8000 - self.fa.s1 as u16,
            8 => { self.sr.rqm = true; self.dr }
            9 => self.dr,
            10 => self.sr.get(),
            11 | 12 => self.si,
            13 => self.k as u16,
            14 => self.l as u16,
            _ => self.data_ram[(self.dp & 0xFF) as usize],
        };

        if alu != 0 {
            let mut p: u16 = match pselect {
                0 => self.data_ram[(self.dp & 0xFF) as usize],
                1 => idb,
                2 => self.m as u16,
                _ => self.n as u16,
            };
            let (q, mut flag, c) = if asl == 0 { (self.a as u16, self.fa, self.fb.c) } else { (self.b as u16, self.fb, self.fa.c) };
            let c16 = c as u16;
            let r: u16 = match alu {
                1 => q | p,
                2 => q & p,
                3 => q ^ p,
                4 => q.wrapping_sub(p),
                5 => q.wrapping_add(p),
                6 => q.wrapping_sub(p).wrapping_sub(c16),
                7 => q.wrapping_add(p).wrapping_add(c16),
                8 => { p = 1; q.wrapping_sub(1) }
                9 => { p = 1; q.wrapping_add(1) }
                10 => !q,
                11 => q >> 1 | q & 0x8000,
                12 => q << 1 | c16,
                13 => q << 2 | 3,
                14 => q << 4 | 15,
                _ => q.rotate_left(8),
            };
            flag.z = r == 0;
            flag.s0 = r & 0x8000 != 0;
            if !flag.ov1 { flag.s1 = flag.s0; }
            match alu {
                1 | 2 | 3 | 10 | 13 | 14 | 15 => { flag.ov0 = false; flag.ov1 = false; flag.c = false; }
                4..=9 => {
                    let carries = q ^ p ^ r;
                    let overflow = (q ^ r) & (p ^ if alu & 1 != 0 { r } else { q });
                    flag.ov0 = overflow & 0x8000 != 0;
                    flag.ov1 = if flag.ov0 && flag.ov1 { flag.s0 == flag.s1 } else { flag.ov0 || flag.ov1 };
                    flag.c = (carries ^ overflow) & 0x8000 != 0;
                }
                11 => { flag.ov0 = false; flag.ov1 = false; flag.c = q & 1 != 0; }
                _ => { flag.ov0 = false; flag.ov1 = false; flag.c = q >> 15 != 0; }
            }
            if asl == 0 { self.a = r as i16; self.fa = flag; } else { self.b = r as i16; self.fb = flag; }
        }

        self.exec_ld((idb as u32) << 6 | dst);

        if dst != 4 {
            match dpl {
                1 => self.dp = (self.dp & 0xF0) + ((self.dp + 1) & 0x0F),
                2 => self.dp = (self.dp & 0xF0) + (self.dp.wrapping_sub(1) & 0x0F),
                3 => self.dp &= 0xF0,
                _ => {}
            }
            self.dp = (self.dp ^ dphm << 4) & 0xFF;
        }
        if dst != 5 && rpdcr != 0 { self.rp = self.rp.wrapping_sub(1) & 0x3FF; }
    }

    fn exec_jp(&mut self, op: u32) {
        let brch = op >> 13 & 0x1FF;
        let na = (op >> 2 & 0x7FF) as u16;
        let jp = na & 0x7FF;             // the µPD7725 has 2048 words: bank bits drop out
        let (a, b) = (self.fa, self.fb);
        let take = match brch {
            0x000 => { self.pc = self.so & 0x7FF; return; }
            0x080 => !a.c, 0x082 => a.c, 0x084 => !b.c, 0x086 => b.c,
            0x088 => !a.z, 0x08A => a.z, 0x08C => !b.z, 0x08E => b.z,
            0x090 => !a.ov0, 0x092 => a.ov0, 0x094 => !b.ov0, 0x096 => b.ov0,
            0x098 => !a.ov1, 0x09A => a.ov1, 0x09C => !b.ov1, 0x09E => b.ov1,
            0x0A0 => !a.s0, 0x0A2 => a.s0, 0x0A4 => !b.s0, 0x0A6 => b.s0,
            0x0A8 => !a.s1, 0x0AA => a.s1, 0x0AC => !b.s1, 0x0AE => b.s1,
            0x0B0 => self.dp & 0x0F == 0x00, 0x0B1 => self.dp & 0x0F != 0x00,
            0x0B2 => self.dp & 0x0F == 0x0F, 0x0B3 => self.dp & 0x0F != 0x0F,
            0x0B4 => !self.sr.siack, 0x0B6 => self.sr.siack, 0x0B8 => !self.sr.soack, 0x0BA => self.sr.soack,
            0x0BC => !self.sr.rqm, 0x0BE => self.sr.rqm,
            0x100 | 0x101 => true,
            0x140 | 0x141 => {
                self.stack[self.sp as usize] = self.pc;
                self.sp = (self.sp + 1) & 15;
                true
            }
            _ => false,
        };
        if take { self.pc = jp; }
    }

    fn exec_ld(&mut self, op: u32) {
        let id = (op >> 6) as u16;
        match op & 15 {
            0 => {}
            1 => self.a = id as i16,
            2 => self.b = id as i16,
            3 => self.tr = id,
            4 => self.dp = id & 0xFF,
            5 => self.rp = id & 0x3FF,
            6 => { self.dr = id; self.sr.rqm = true; }
            7 => { let v = self.sr.get() & 0x907C | id & !0x907C; self.sr.set(v); }
            8 | 9 => self.so = id,
            10 => self.k = id as i16,
            11 => { self.k = id as i16; self.l = self.data_rom[(self.rp & 0x3FF) as usize] as i16; }
            12 => { self.l = id as i16; self.k = self.data_ram[((self.dp | 0x40) & 0xFF) as usize] as i16; }
            13 => self.l = id as i16,
            14 => self.trb = id,
            _ => self.data_ram[(self.dp & 0xFF) as usize] = id,
        }
    }
}

/// Where the DSP sits for the S-CPU: (bank ranges, address range, DR/SR select bit).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct NecMap {
    pub banks: (u8, u8),
    pub lo: u16,
    pub hi: u16,
    pub select: u16,
}

impl NecMap {
    /// Some(is SR) when the S-CPU address is the DSP's.
    pub fn hit(&self, addr: u32) -> Option<bool> {
        let bank = (addr >> 16) as u8 & 0x7F;
        let a = addr as u16;
        if bank >= self.banks.0 && bank <= self.banks.1 && a >= self.lo && a <= self.hi {
            return Some(a & self.select != 0);
        }
        None
    }
}

/// The board's DSP window (ares's NEC-* boards): HiROM 00-1F:6000-7FFF (bit 12);
/// LoROM without RAM 30-3F:8000-FFFF, with RAM and up to 1 MB 20-3F:8000-FFFF, bigger
/// 60-6F:0000-7FFF (bit 14). Banks 80-FF mirror.
pub fn board_map(hirom: bool, has_ram: bool, rom_len: usize) -> NecMap {
    if hirom { return NecMap { banks: (0x00, 0x1F), lo: 0x6000, hi: 0x7FFF, select: 0x1000 }; }
    if !has_ram { return NecMap { banks: (0x30, 0x3F), lo: 0x8000, hi: 0xFFFF, select: 0x4000 }; }
    if rom_len <= 0x100000 { return NecMap { banks: (0x20, 0x3F), lo: 0x8000, hi: 0xFFFF, select: 0x4000 }; }
    NecMap { banks: (0x60, 0x6F), lo: 0x0000, hi: 0x7FFF, select: 0x4000 }
}

/// Which firmware a title wants (ares's list).
pub fn firmware_name(title: &str) -> &'static str {
    match title {
        "PILOTWINGS" => "dsp1",
        "DUNGEON MASTER" => "dsp2",
        "PLANETS CHAMP TG3000" | "TOP GEAR 3000" => "dsp4",
        // "SDｶﾞﾝﾀﾞﾑGX" in half-width katakana (not UTF-8 in the header)
        t if t.starts_with("SD") && t.ends_with("GX") => "dsp3",
        _ => "dsp1b",
    }
}
