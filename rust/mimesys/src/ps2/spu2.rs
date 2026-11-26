//! The SPU2 (IOP 1F90_0000-1F90_07FF): two PS1-style cores of 24 ADPCM voices over
//! 2 MB of sound RAM (addresses in 16-bit words, 20 bits), at 48 kHz (768 IOP cycles a
//! sample). From ps2tek and PCSX2's behaviour (no code taken).
//!
//! - Core c's registers at c × 400h (byte offsets): voice parameters 000-17F (16 bytes a
//!   voice: VOLL, VOLR, PITCH, ADSR1, ADSR2, ENVX, VOLXL, VOLXR), PMON 180, NON 184,
//!   VMIXL / EL / R / ER 188-197, MMIX 198, ATTR 19A, IRQA 19C (hi) / 19E (lo), KON 1A0,
//!   KOFF 1A4, TSA 1A8 (hi) / 1AA (lo), the data port 1AC, ADMAS 1B0, voice addresses
//!   1C0-2DF (12 bytes a voice: SSA, LSAX, NAX, each hi / lo), ENDX 340, STATX 344.
//!   Then per core at 760 + c × 28h: MVOL, EVOL, AVOL, BVOL, MVOLX. 7C2: IRQINFO.
//! - Voices play like the PS1's (see `ps1::spu`): blocks of 8 words, header flags end /
//!   repeat / loop start. They keep reading memory when off, as on the console, so the
//!   addresses they pass still hit IRQA.
//! - IRQ (IOP interrupt 9): when ATTR bit 6 is on, a voice reading IRQA's block, a
//!   transfer writing it, or the ADMA input reading it sets IRQINFO bit 2 + core (once
//!   until IRQINFO is written or ATTR bit 6 goes off). Both cores watch every access.
//! - DMA (IOP channels 4 / 7): block transfers to / from TSA; with ADMAS the data is
//!   auto-DMA input, 200h words a block (100h left, 100h right) into the half of the
//!   core's input buffer (2000h / 2200h + c × 400h) not being played.
//! - Mixing: each core's voices (VMIXL / R) and its ADMA input (BVOL), core 0's output
//!   into core 1 (AVOL), times MVOL. No reverb or noise yet.

use serde::{Deserialize, Serialize};

use crate::ps1::spu::{clamp16, Phase, Voice, Volume, FILTERS};

/// IOP cycles per 48 kHz sample
const CYCLES_PER_SAMPLE: u32 = 768;
const RAM_WORDS: usize = 0x10_0000;
const MASK: u32 = 0xF_FFFF;

#[derive(Clone, Default, Serialize, Deserialize)]
struct Core {
    /// STATX (344): 80h = DMA ready (cleared when ATTR leaves DMA mode, PCSX2); reads
    /// 80h until written otherwise
    #[serde(default)]
    statx_clear: bool,
    voices: Vec<Voice>,
    irqa: u32,
    tsa: u32,
    endx: u32,
    mvol_l: Volume,
    mvol_r: Volume,
    /// auto-DMA input: the sample being played (0-1FF) and the half to fill next
    in_pos: u32,
    in_half: u32,
    /// halves of the input buffer free for auto-DMA data (0-2)
    #[serde(default)]
    in_free: u32,
    /// the last output, for core 1's external input
    out: (i32, i32),
}

impl Core {
    fn new() -> Self {
        Self { voices: vec![Voice::default(); 24], irqa: 0x800, endx: 0xFF_FFFF, ..Default::default() }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Spu2 {
    /// the raw registers (index = byte offset / 2)
    pub regs: Vec<u16>,
    pub ram: Vec<u16>,
    cores: Vec<Core>,
    irqinfo: u16,
    /// an interrupt to pass to the IOP (I_STAT bit 9)
    pub irq: bool,
    cycles: u32,
    /// where the output areas (voice 1 / 3, dry / wet mixes, core 0's output) are written
    /// next, 0..1FFh
    #[serde(default)]
    out_pos: u32,
    /// debugging: interrupts raised, voices keyed on
    #[serde(skip)]
    pub irqs: u64,
    #[serde(skip)]
    pub key_ons: u64,
    /// 48 kHz stereo, -1..1
    #[serde(skip)]
    pub out: Vec<f32>,
}

impl Default for Spu2 {
    fn default() -> Self { Self::new() }
}

impl Spu2 {
    pub fn new() -> Self {
        Self {
            regs: vec![0; 0x400], ram: vec![0; RAM_WORDS], cores: vec![Core::new(), Core::new()],
            irqinfo: 0, irq: false, cycles: 0, out_pos: 0, irqs: 0, key_ons: 0, out: Vec::new(),
        }
    }

    fn reg(&self, byte: u32) -> u16 { self.regs[(byte as usize >> 1) & 0x3FF] }
    fn attr(&self, c: usize) -> u16 { self.reg(0x19A + 0x400 * c as u32) }
    fn bits24(&self, byte: u32) -> u32 { self.reg(byte) as u32 | (self.reg(byte + 2) as u32 & 0xFF) << 16 }
    pub fn admas(&self, c: usize) -> bool { self.reg(0x1B0 + 0x400 * c as u32) & 3 != 0 }

    // ------------------------------------------------------------------ interrupts

    fn raise(&mut self, c: usize) {
        if self.attr(c) & 0x40 != 0 && self.irqinfo & (4 << c) == 0 {
            self.irqinfo |= 4 << c;
            self.irq = true;
            self.irqs += 1;
        }
    }

    /// A read or write at word address `a`
    fn touch(&mut self, a: u32) {
        for c in 0..2 {
            if self.cores[c].irqa == a & MASK { self.raise(c); }
        }
    }

    /// A sample the SPU2 itself writes to an output area of sound RAM (an IRQ if IRQA is
    /// there, as PCSX2's spu2M_WriteFast); games read these (Vice City times its streaming
    /// against core 1's dry output at 1800h)
    fn out_write(&mut self, a: u32, v: i32) {
        self.ram[a as usize] = clamp16(v) as i16 as u16;
        self.touch(a);
    }

    /// Words `a .. a + n` were accessed
    fn touch_range(&mut self, a: u32, n: u32) {
        for c in 0..2 {
            let off = self.cores[c].irqa.wrapping_sub(a) & MASK;
            if off < n { self.raise(c); }
        }
    }

    // ------------------------------------------------------------------ registers

    pub fn read16(&mut self, byte: u32) -> u16 {
        let byte = byte & 0x7FE;
        if byte >= 0x760 {
            return match byte {
                0x770 | 0x772 | 0x798 | 0x79A => {
                    let c = if byte >= 0x788 { 1 } else { 0 };
                    let k = &self.cores[c];
                    (if byte & 2 == 0 { k.mvol_l.level } else { k.mvol_r.level }) as u16
                }
                0x7C2 => self.irqinfo,
                _ => self.reg(byte),
            };
        }
        let c = (byte >> 10) as usize;
        let o = byte & 0x3FF;
        let core = &self.cores[c];
        match o {
            0x000..=0x17F => {
                let v = &core.voices[(o >> 4) as usize];
                match o & 0xF {
                    0xA => v.level as u16,
                    0xC => v.vol_l.level as u16,
                    0xE => v.vol_r.level as u16,
                    _ => self.reg(byte),
                }
            }
            0x19C => (core.irqa >> 16) as u16,
            0x19E => core.irqa as u16,
            0x1A8 => (core.tsa >> 16) as u16,
            0x1AA => core.tsa as u16,
            0x1AC => {
                let a = core.tsa;
                self.touch(a);
                self.cores[c].tsa = (a + 1) & MASK;
                self.ram[a as usize]
            }
            0x1C0..=0x2DF => {
                let k = o - 0x1C0;
                let n = (k / 12) as usize;
                let v = &core.voices[n];
                let a = match k % 12 {
                    0 | 2 => self.ssa(c, n),
                    4 | 6 => v.repeat,
                    _ => nax(v),
                };
                if k % 4 == 0 { (a >> 16) as u16 } else { a as u16 }
            }
            0x340 => core.endx as u16,
            0x342 => (core.endx >> 16) as u16,
            // DMA ready unless ATTR left DMA mode; never busy (our transfers finish at
            // once). libsd's core reset waits for STATX & 7FF = 0 after writing ATTR (DMC:
            // with 80h always set, each wait timed out: sceSdInit took 2 seconds)
            0x344 => if core.statx_clear { 0 } else { 0x0080 },
            _ => self.reg(byte),
        }
    }

    fn ssa(&self, c: usize, n: usize) -> u32 {
        let b = 0x1C0 + 0x400 * c as u32 + 12 * n as u32;
        (self.reg(b) as u32 & 0xF) << 16 | self.reg(b + 2) as u32
    }

    pub fn write16(&mut self, byte: u32, v: u16) {
        let byte = byte & 0x7FE;
        let old = self.reg(byte);
        self.regs[byte as usize >> 1] = v;
        if byte >= 0x760 {
            match byte {
                0x760 => self.cores[0].mvol_l.set(v),
                0x762 => self.cores[0].mvol_r.set(v),
                0x788 => self.cores[1].mvol_l.set(v),
                0x78A => self.cores[1].mvol_r.set(v),
                0x7C2 => self.irqinfo = v,
                _ => {}
            }
            return;
        }
        let c = (byte >> 10) as usize;
        let o = byte & 0x3FF;
        let hi = |x: u32, v: u16| (x & 0xFFFF) | (v as u32 & 0xF) << 16;
        let lo = |x: u32, v: u16| (x & 0xF_0000) | v as u32;
        match o {
            0x000..=0x17F => {
                let voice = &mut self.cores[c].voices[(o >> 4) as usize];
                match o & 0xF {
                    0x0 => voice.vol_l.set(v),
                    0x2 => voice.vol_r.set(v),
                    0x4 => voice.pitch = v,
                    0x6 => voice.adsr = (voice.adsr & 0xFFFF_0000) | v as u32,
                    0x8 => voice.adsr = (voice.adsr & 0xFFFF) | (v as u32) << 16,
                    0xA => voice.level = v as i16 as i32,
                    _ => {}
                }
            }
            0x19A => {
                if v & 0x40 == 0 { self.irqinfo &= !(4 << c); }
                // DMA mode bits 4-5: off clears STATX's ready bit, on sets it (PCSX2)
                if v & 0x30 == 0 { self.cores[c].statx_clear = true; }
                else if old & 0x30 == 0 { self.cores[c].statx_clear = false; }
            }
            0x19C => self.cores[c].irqa = hi(self.cores[c].irqa, v),
            0x19E => self.cores[c].irqa = lo(self.cores[c].irqa, v),
            0x1A0 => self.key_on(c, v as u32),
            0x1A2 => self.key_on(c, (v as u32 & 0xFF) << 16),
            0x1A4 => self.key_off(c, v as u32),
            0x1A6 => self.key_off(c, (v as u32 & 0xFF) << 16),
            0x1A8 => self.cores[c].tsa = hi(self.cores[c].tsa, v),
            0x1AA => self.cores[c].tsa = lo(self.cores[c].tsa, v),
            0x1AC => {
                let a = self.cores[c].tsa;
                self.touch(a);
                self.ram[a as usize] = v;
                self.cores[c].tsa = (a + 1) & MASK;
            }
            0x1B0 => {
                let was = old & 3 != 0;
                if v & 3 == 0 || !was {
                    // (re)started: play from the start, both halves to fill
                    let k = &mut self.cores[c];
                    k.in_pos = 0;
                    k.in_half = 0;
                    k.in_free = 2;
                }
            }
            0x1C0..=0x2DF => {
                let k = o - 0x1C0;
                let n = (k / 12) as usize;
                let voice = &mut self.cores[c].voices[n];
                match k % 12 {
                    0 | 2 => {}                          // SSA: read from the registers at key on
                    4 => { voice.repeat = hi(voice.repeat, v) & !7; voice.ignore_loop = true; }
                    6 => { voice.repeat = lo(voice.repeat, v) & !7; voice.ignore_loop = true; }
                    8 | 10 => {
                        let a = if k % 12 == 8 { hi(nax(voice), v) } else { lo(nax(voice), v) };
                        voice.cur = a & !7;
                        voice.counter = 0;
                        self.decode_block(c, n);
                    }
                    _ => {}
                }
            }
            0x340 => self.cores[c].endx &= 0xFF_0000,
            0x342 => self.cores[c].endx &= 0xFFFF,
            _ => {}
        }
    }

    fn key_on(&mut self, c: usize, bits: u32) {
        for n in 0..24 {
            if bits & (1 << n) == 0 { continue; }
            self.key_ons += 1;
            let start = self.ssa(c, n) & !7;
            self.cores[c].endx &= !(1 << n);
            let v = &mut self.cores[c].voices[n];
            v.cur = start;
            v.repeat = start;
            v.ignore_loop = false;
            v.counter = 0;
            v.buf.fill(0);
            v.s1 = 0;
            v.s2 = 0;
            v.level = 0;
            v.env_counter = 0;
            v.phase = Phase::Attack;
            self.decode_block(c, n);
        }
    }

    fn key_off(&mut self, c: usize, bits: u32) {
        for n in 0..24 {
            if bits & (1 << n) != 0 {
                let v = &mut self.cores[c].voices[n];
                if v.phase != Phase::Off { v.phase = Phase::Release; v.env_counter = 0; }
            }
        }
    }

    // ------------------------------------------------------------------ DMA

    /// A block transfer from IOP memory to TSA
    pub fn dma_write(&mut self, c: usize, data: &[u16]) {
        self.cores[c].statx_clear = false;                    // a DMA ended: ready
        let a = self.cores[c].tsa;
        self.touch_range(a, data.len() as u32);
        for (k, &w) in data.iter().enumerate() {
            self.ram[((a + k as u32) & MASK) as usize] = w;
        }
        self.cores[c].tsa = (a + data.len() as u32) & MASK;
    }

    /// A block transfer from TSA to IOP memory
    pub fn dma_read(&mut self, c: usize, n: usize) -> Vec<u16> {
        self.cores[c].statx_clear = false;
        let a = self.cores[c].tsa;
        self.touch_range(a, n as u32);
        let v = (0..n).map(|k| self.ram[((a + k as u32) & MASK) as usize]).collect();
        self.cores[c].tsa = (a + n as u32) & MASK;
        v
    }

    /// Room for another auto-DMA block (one half of the input buffer isn't waiting to play)
    pub fn adma_room(&self, c: usize) -> bool { self.cores[c].in_free > 0 }

    /// Auto-DMA input: blocks of 100h left then 100h right samples, each into the next half
    pub fn adma_write(&mut self, c: usize, data: &[u16]) {
        for blk in data.chunks(0x200) {
            self.cores[c].in_free = self.cores[c].in_free.saturating_sub(1);
            let half = self.cores[c].in_half;
            let base = 0x2000 + 0x400 * c as u32 + half * 0x100;
            for (k, &w) in blk.iter().enumerate() {
                let a = base + if k < 0x100 { k as u32 } else { 0x200 + (k as u32 - 0x100) };
                self.ram[a as usize] = w;
            }
            self.cores[c].in_half ^= 1;
        }
    }

    // ------------------------------------------------------------------ voices

    fn decode_block(&mut self, c: usize, n: usize) {
        let a = self.cores[c].voices[n].cur & MASK & !7;
        self.touch_range(a, 8);
        let w: [u16; 8] = std::array::from_fn(|k| self.ram[((a + k as u32) & MASK) as usize]);
        let v = &mut self.cores[c].voices[n];
        let mut shift = (w[0] & 0xF) as i32;
        if shift > 12 { shift = 9; }
        let (f0, f1) = FILTERS[((w[0] >> 4) & 7).min(4) as usize];
        v.flags = (w[0] >> 8) as u8;
        if v.flags & 4 != 0 && !v.ignore_loop { v.repeat = a; }
        let tail = [v.buf[28], v.buf[29], v.buf[30]];
        v.buf[..3].copy_from_slice(&tail);
        for i in 0..28 {
            let nib = (w[1 + i / 4] >> ((i & 3) * 4)) & 0xF;
            let t = (((nib as i32) << 28) >> 28) << 12;
            let s = clamp16((t >> shift) + ((v.s1 * f0 + v.s2 * f1 + 32) >> 6));
            v.s2 = v.s1;
            v.s1 = s;
            v.buf[3 + i] = s as i16;
        }
    }

    fn next_block(&mut self, c: usize, n: usize) {
        let v = &mut self.cores[c].voices[n];
        if v.flags & 1 != 0 {
            self.cores[c].endx |= 1 << n;
            let v = &mut self.cores[c].voices[n];
            v.cur = v.repeat;
            if v.flags & 2 == 0 {
                v.phase = Phase::Off;
                v.level = 0;
            }
        } else {
            v.cur = (v.cur + 8) & MASK;
        }
        self.decode_block(c, n);
    }

    fn voice_sample(&mut self, c: usize, n: usize, prev_out: i32) -> i32 {
        let pmon = n > 0 && self.bits24(0x180 + 0x400 * c as u32) & (1 << n) != 0;
        let v = &self.cores[c].voices[n];
        let out = (v.interpolate() * v.level) >> 15;
        let mut step = v.pitch as u32;
        if pmon {
            step = (((step as i16 as i32) * (prev_out + 0x8000)) >> 15) as u32 & 0xFFFF;
        }
        step = step.min(0x3FFF);
        self.cores[c].voices[n].counter += step;
        while self.cores[c].voices[n].counter >= 28 << 12 {
            self.cores[c].voices[n].counter -= 28 << 12;
            self.next_block(c, n);
        }
        let v = &mut self.cores[c].voices[n];
        v.tick_envelope();
        v.vol_l.tick();
        v.vol_r.tick();
        v.out = out;
        out
    }

    /// Time passes (IOP cycles)
    pub fn tick(&mut self, cycles: u32) {
        self.cycles += cycles;
        while self.cycles >= CYCLES_PER_SAMPLE {
            self.cycles -= CYCLES_PER_SAMPLE;
            self.sample();
        }
    }

    fn sample(&mut self) {
        let mut final_out = (0, 0);
        for c in 0..2 {
            let base = 0x400 * c as u32;
            let (vl_mask, vr_mask) = (self.bits24(base + 0x188), self.bits24(base + 0x190));
            let (wl_mask, wr_mask) = (self.bits24(base + 0x18C), self.bits24(base + 0x194));
            let (mut l, mut r) = (0i32, 0i32);
            let (mut wl, mut wr) = (0i32, 0i32);
            let mut prev = 0;
            let op = self.out_pos;
            for n in 0..24 {
                let out = self.voice_sample(c, n, prev);
                prev = out;
                let v = &self.cores[c].voices[n];
                let (vl, vr) = ((out * v.vol_l.level) >> 15, (out * v.vol_r.level) >> 15);
                if vl_mask & (1 << n) != 0 { l += vl; }
                if vr_mask & (1 << n) != 0 { r += vr; }
                if wl_mask & (1 << n) != 0 { wl += vl; }
                if wr_mask & (1 << n) != 0 { wr += vr; }
                // voices 1 and 3 (after the envelope) into their output areas
                if n == 1 || n == 3 {
                    let a = [[0x400, 0x600], [0xC00, 0xE00]][c][(n == 3) as usize] + op;
                    self.out_write(a, out);
                }
            }
            // the voice mixes into the output areas (dry L / R, wet L / R), as PCSX2
            let ob = if c == 0 { 0x1000 } else { 0x1800 };
            self.out_write(ob + op, l);
            self.out_write(ob + 0x200 + op, r);
            self.out_write(ob + 0x400 + op, wl);
            self.out_write(ob + 0x600 + op, wr);
            let mmix = self.reg(base + 0x198) as u32;
            if mmix & 0x800 == 0 { l = 0; }
            if mmix & 0x400 == 0 { r = 0; }
            // the auto-DMA input
            let pos = self.cores[c].in_pos;
            let (la, ra) = (0x2000 + 0x400 * c as u32 + pos, 0x2200 + 0x400 * c as u32 + pos);
            self.touch(la);
            self.touch(ra);
            if self.admas(c) {
                let bl = self.reg(0x76C + 0x28 * c as u32) as i16 as i32;
                let br = self.reg(0x76E + 0x28 * c as u32) as i16 as i32;
                if mmix & 0x80 != 0 { l += (self.ram[la as usize] as i16 as i32 * bl) >> 15; }
                if mmix & 0x40 != 0 { r += (self.ram[ra as usize] as i16 as i32 * br) >> 15; }
            }
            self.cores[c].in_pos = (pos + 1) & 0x1FF;
            // a half has played: free for the next block
            if (pos + 1) & 0xFF == 0 { self.cores[c].in_free = (self.cores[c].in_free + 1).min(2); }
            // core 0's output into core 1
            if c == 1 {
                let al = self.reg(0x768 + 0x28) as i16 as i32;
                let ar = self.reg(0x76A + 0x28) as i16 as i32;
                let (el, er) = self.cores[0].out;
                if mmix & 0x8 != 0 { l += (el * al) >> 15; }
                if mmix & 0x4 != 0 { r += (er * ar) >> 15; }
            }
            let k = &mut self.cores[c];
            k.mvol_l.tick();
            k.mvol_r.tick();
            let ol = (clamp16(l) * k.mvol_l.level) >> 15;
            let or = (clamp16(r) * k.mvol_r.level) >> 15;
            k.out = (ol, or);
            final_out = (ol, or);
            // core 0's output (after its master volume) into its area before core 1 mixes
            if c == 0 {
                self.out_write(0x800 + self.out_pos, ol);
                self.out_write(0xA00 + self.out_pos, or);
            }
        }
        self.out_pos = (self.out_pos + 1) & 0x1FF;
        if self.out.len() < 2 * 48_000 {
            self.out.push(clamp16(final_out.0) as f32 / 32768.0);
            self.out.push(clamp16(final_out.1) as f32 / 32768.0);
        }
    }

    /// Debugging: per core ATTR, IRQA, voices playing
    pub fn summary(&self) -> String {
        (0..2).map(|c| {
            let on = self.cores[c].voices.iter().filter(|v| v.phase != Phase::Off).count();
            format!("core {c} ATTR {:04X} IRQA {:05X} TSA {:05X} ADMAS {:X} voices on {on}",
                self.attr(c), self.cores[c].irqa, self.cores[c].tsa, self.reg(0x1B0 + 0x400 * c as u32))
        }).collect::<Vec<_>>().join(" | ")
            + &format!(" | IRQINFO {:X} | irqs {} | key ons {}", self.irqinfo, self.irqs, self.key_ons)
    }
}

/// The word a voice reads next: its block's header, then the data word holding the
/// current sample
fn nax(v: &Voice) -> u32 { (v.cur & !7) + 1 + ((v.counter >> 12) / 4).min(6) }
