//! Capcom Q-Sound (DL-1425), high-level emulation. A port of MAME's
//! sound/qsoundhle.cpp (BSD-3-Clause, superctr and Valley Bell), which is based on the
//! disassembled DSP program.
//!
//! The chip is a DSP16A running a fixed program from its internal ROM; this emulates
//! what that program does, sample by sample. It still reads its tables (pan laws, FIR
//! filter taps, ADPCM steps) from that ROM, so the DSP ROM dump (dl-1425.bin, first 0x2000
//! bytes = 0x1000 little-endian words) is needed.
//!
//! 16 PCM voices (8-bit samples from the sample ROM, any rate) + 3 ADPCM voices, echo,
//! and a FIR filter + delay line per output. The sound CPU writes 16-bit values into 256
//! registers: data high byte, data low byte, then the register number.
//! Output: 60 MHz / 2 / 1248 = 24038 Hz stereo.

use serde::{Deserialize, Serialize};

pub const CLOCK: u64 = 60_000_000;
/// DSP cycles per output sample (the DSP runs at clock / 2)
pub const CYCLES_PER_SAMPLE: u64 = 2 * 1248;

const DATA_PAN_TAB: u16 = 0x110;
const DATA_ADPCM_TAB: u16 = 0x9DC;
const DATA_FILTER_TAB: u16 = 0xD53;
const STATE_BOOT: u16 = 0x000;
const STATE_INIT2: u16 = 0x61A;
const STATE_REFRESH1: u16 = 0x039;
const STATE_REFRESH2: u16 = 0x04F;
const STATE_NORMAL1: u16 = 0x314;
const STATE_NORMAL2: u16 = 0x6B2;
const PAN_TABLE_DRY: u16 = 0;
const PAN_TABLE_WET: u16 = 98;
const PAN_TABLE_CH_OFFSET: u16 = 196;
const FILTER_ENTRY_SIZE: u16 = 95;
const DELAY_BASE_OFFSET: u16 = 0x554;
const DELAY_BASE_OFFSET2: u16 = 0x53C;

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
struct Voice { bank: u16, addr: i16, phase: u16, rate: u16, loop_len: i16, end_addr: i16, volume: i16, echo: i16 }

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
struct Adpcm { start_addr: u16, end_addr: u16, bank: u16, volume: i16, flag: u16, cur_vol: i16, step_size: i16, cur_addr: u16 }

#[derive(Clone, Serialize, Deserialize)]
struct Fir { tap_count: i32, delay_pos: i32, table_pos: u16, taps: Vec<i16>, delay_line: Vec<i16> }

impl Default for Fir {
    fn default() -> Self { Self { tap_count: 0, delay_pos: 0, table_pos: 0, taps: vec![0; 95], delay_line: vec![0; 95] } }
}

impl Fir {
    fn apply(&mut self, input: i16) -> i32 {
        let mut out: i32 = 0;
        let mut tap = 0usize;
        while (tap as i32) < self.tap_count - 1 {
            out = out.wrapping_sub((self.taps[tap] as i32 * self.delay_line[self.delay_pos as usize] as i32) << 2);
            self.delay_pos += 1;
            if self.delay_pos >= self.tap_count - 1 { self.delay_pos = 0; }
            tap += 1;
        }
        out = out.wrapping_sub((self.taps[tap.min(94)] as i32 * input as i32) << 2);
        self.delay_line[self.delay_pos as usize] = input;
        self.delay_pos += 1;
        if self.delay_pos >= self.tap_count - 1 { self.delay_pos = 0; }
        out
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Delay { delay: i16, volume: i16, write_pos: i16, read_pos: i16, line: Vec<i16> }

impl Default for Delay {
    fn default() -> Self { Self { delay: 0, volume: 0, write_pos: 0, read_pos: 0, line: vec![0; 51] } }
}

impl Delay {
    fn apply(&mut self, input: i32) -> i32 {
        self.line[self.write_pos as usize] = (input >> 16) as i16;
        self.write_pos += 1;
        if self.write_pos >= 51 { self.write_pos = 0; }
        let out = self.line[self.read_pos as usize] as i32 * self.volume as i32;
        self.read_pos += 1;
        if self.read_pos >= 51 { self.read_pos = 0; }
        out
    }

    fn update(&mut self) {
        let p = (self.write_pos - self.delay) % 51;
        self.read_pos = if p < 0 { p + 51 } else { p };
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Echo { end_pos: u16, feedback: i16, length: i16, last_sample: i16, line: Vec<i16>, pos: i16 }

impl Default for Echo {
    fn default() -> Self { Self { end_pos: 0, feedback: 0, length: 0, last_sample: 0, line: vec![0; 1024], pos: 0 } }
}

impl Echo {
    fn apply(&mut self, input: i32) -> i16 {
        let mut old = self.line[self.pos as usize] as i32;
        let last = self.last_sample as i32;
        self.last_sample = old as i16;
        old = (old + last) >> 1;
        let new = input.wrapping_add((old * self.feedback as i32) << 2);
        self.line[self.pos as usize] = (new >> 16) as i16;
        self.pos += 1;
        if self.pos >= self.length { self.pos = 0; }
        old as i16
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct QSound {
    #[serde(skip)] rom: Vec<u8>,
    #[serde(skip)] dsp: Vec<u16>,
    data_latch: u16,
    pub out: [i16; 2],
    voice: [Voice; 16],
    adpcm: [Adpcm; 3],
    voice_pan: [u16; 19],
    voice_output: [i16; 19],
    echo: Echo,
    filter: [Fir; 2],
    alt_filter: [Fir; 2],
    wet: [Delay; 2],
    dry: [Delay; 2],
    state: u16,
    next_state: u16,
    delay_update: u16,
    state_counter: i32,
    ready: u8,
}

impl QSound {
    /// `rom`: the sample ROM. `dsp_rom`: dl-1425.bin (0x2000 bytes, little-endian words).
    pub fn new(rom: Vec<u8>, dsp_rom: &[u8]) -> Self {
        let mut dsp: Vec<u16> = dsp_rom.chunks(2).take(0x1000).map(|c| c[0] as u16 | (*c.get(1).unwrap_or(&0) as u16) << 8).collect();
        dsp.resize(0x1000, 0);
        let mut q = Self {
            rom, dsp, data_latch: 0, out: [0; 2],
            voice: [Voice::default(); 16], adpcm: [Adpcm::default(); 3],
            voice_pan: [0; 19], voice_output: [0; 19],
            echo: Echo::default(), filter: Default::default(), alt_filter: Default::default(),
            wet: Default::default(), dry: Default::default(),
            state: STATE_BOOT, next_state: 0, delay_update: 0, state_counter: 0, ready: 0,
        };
        q.reset();
        q
    }

    pub fn reset(&mut self) {
        self.ready = 0;
        self.out = [0; 2];
        self.state = STATE_BOOT;
        self.state_counter = 0;
    }

    /// Puts back the ROMs after a save state load.
    pub fn keep_static(&mut self, old: &mut QSound) {
        self.rom = std::mem::take(&mut old.rom);
        self.dsp = std::mem::take(&mut old.dsp);
    }

    /// D000 / D001: data high / low byte; D002: register number (does the write).
    pub fn write(&mut self, offset: u8, v: u8) {
        match offset {
            0 => self.data_latch = (self.data_latch & 0x00FF) | (v as u16) << 8,
            1 => self.data_latch = (self.data_latch & 0xFF00) | v as u16,
            2 => { self.write_reg(v, self.data_latch); self.ready = 0; }
            _ => {}
        }
    }

    /// D007: 0x80 = ready for the next write.
    pub fn read(&self) -> u8 { self.ready }

    fn write_reg(&mut self, r: u8, d: u16) {
        let r = r as usize;
        match r {
            0x00..=0x7F => {
                let i = r >> 3;
                let v = &mut self.voice;
                match r & 7 {
                    0 => v[(i + 1) % 16].bank = d,   // the bank applies to the next voice
                    1 => v[i].addr = d as i16,
                    2 => v[i].rate = d,
                    3 => v[i].phase = d,
                    4 => v[i].loop_len = d as i16,
                    5 => v[i].end_addr = d as i16,
                    6 => v[i].volume = d as i16,
                    _ => {}
                }
            }
            0x80..=0x8F => self.voice_pan[r - 0x80] = d,
            0x90..=0x92 => self.voice_pan[16 + r - 0x90] = d,
            0x93 => self.echo.feedback = d as i16,
            0xBA..=0xC9 => self.voice[r - 0xBA].echo = d as i16,
            0xCA..=0xD5 => {
                let a = &mut self.adpcm[(r - 0xCA) >> 2];
                match (r - 0xCA) & 3 {
                    0 => a.start_addr = d,
                    1 => a.end_addr = d,
                    2 => a.bank = d,
                    _ => a.volume = d as i16,
                }
            }
            0xD6..=0xD8 => self.adpcm[r - 0xD6].flag = d,
            0xD9 => self.echo.end_pos = d,
            0xDA | 0xDC => self.filter[(r - 0xDA) >> 1].table_pos = d,
            0xDB | 0xDD => self.alt_filter[(r - 0xDB) >> 1].table_pos = d,
            0xDE | 0xE0 => self.wet[(r - 0xDE) >> 1].delay = d as i16,
            0xDF | 0xE1 => self.dry[(r - 0xDF) >> 1].delay = d as i16,
            0xE2 => self.delay_update = d,
            0xE3 => self.next_state = d,
            0xE4 | 0xE6 => self.wet[(r - 0xE4) >> 1].volume = d as i16,
            0xE5 | 0xE7 => self.dry[(r - 0xE5) >> 1].volume = d as i16,
            _ => {}
        }
    }

    #[inline]
    fn dsp_rom(&self, a: u16) -> u16 { self.dsp[(a & 0xFFF) as usize] }

    #[inline]
    fn read_sample(&self, bank: u16, addr: u16) -> i16 {
        if self.rom.is_empty() { return 0; }
        let a = ((bank as usize & 0x7FFF) << 16 | addr as usize) % self.rom.len();
        ((self.rom[a] as u16) << 8) as i16
    }

    /// Computes the next output sample (24038 Hz) into `out`.
    pub fn update(&mut self) {
        match self.state {
            STATE_REFRESH1 => self.refresh_filter_1(),
            STATE_REFRESH2 => self.refresh_filter_2(),
            STATE_NORMAL1 | STATE_NORMAL2 => self.normal_update(),
            _ => self.init(),
        }
    }

    fn init(&mut self) {
        let mode2 = self.state == STATE_INIT2;
        // busy for 4 samples, including the filter refresh
        if self.state_counter >= 2 {
            self.state_counter = 0;
            self.state = self.next_state;
            return;
        } else if self.state_counter == 1 {
            self.state_counter += 1;
            return;
        }
        self.voice = [Voice::default(); 16];
        self.adpcm = [Adpcm::default(); 3];
        self.filter = Default::default();
        self.alt_filter = Default::default();
        self.wet = Default::default();
        self.dry = Default::default();
        self.echo = Echo::default();
        self.voice_pan = [DATA_PAN_TAB + 0x10; 19];
        self.voice_output = [0; 19];
        for v in &mut self.voice { v.bank = 0x8000; }
        for a in &mut self.adpcm { a.bank = 0x8000; }
        if !mode2 {
            self.wet[0].delay = 0;
            self.dry[0].delay = 46;
            self.wet[1].delay = 0;
            self.dry[1].delay = 48;
            self.filter[0].table_pos = DATA_FILTER_TAB + FILTER_ENTRY_SIZE;
            self.filter[1].table_pos = DATA_FILTER_TAB + FILTER_ENTRY_SIZE * 2;
            self.echo.end_pos = DELAY_BASE_OFFSET + 6;
            self.next_state = STATE_REFRESH1;
        } else {
            self.wet[0].delay = 1;
            self.dry[0].delay = 0;
            self.wet[1].delay = 0;
            self.dry[1].delay = 0;
            self.filter[0].table_pos = 0xF73;
            self.filter[1].table_pos = 0xFA4;
            self.alt_filter[0].table_pos = 0xF73;
            self.alt_filter[1].table_pos = 0xFA4;
            self.echo.end_pos = DELAY_BASE_OFFSET2 + 6;
            self.next_state = STATE_REFRESH2;
        }
        for d in self.wet.iter_mut().chain(self.dry.iter_mut()) { d.volume = 0x3FFF; }
        self.delay_update = 1;
        self.ready = 0;
        self.state_counter = 1;
    }

    fn refresh_filter_1(&mut self) {
        for ch in 0..2 {
            self.filter[ch].delay_pos = 0;
            self.filter[ch].tap_count = 95;
            for i in 0..95 { self.filter[ch].taps[i] = self.dsp_rom(self.filter[ch].table_pos + i as u16) as i16; }
        }
        self.state = STATE_NORMAL1;
        self.next_state = STATE_NORMAL1;
    }

    fn refresh_filter_2(&mut self) {
        for ch in 0..2 {
            self.filter[ch].delay_pos = 0;
            self.filter[ch].tap_count = 45;
            for i in 0..45 { self.filter[ch].taps[i] = self.dsp_rom(self.filter[ch].table_pos + i as u16) as i16; }
            self.alt_filter[ch].delay_pos = 0;
            self.alt_filter[ch].tap_count = 44;
            for i in 0..44 { self.alt_filter[ch].taps[i] = self.dsp_rom(self.alt_filter[ch].table_pos + i as u16) as i16; }
        }
        self.state = STATE_NORMAL2;
        self.next_state = STATE_NORMAL2;
    }

    fn voice_update(&mut self, i: usize, echo_out: &mut i32) -> i16 {
        let v = self.voice[i];
        let output = ((v.volume as i32 * self.read_sample(v.bank, v.addr as u16) as i32) >> 14) as i16;
        *echo_out = echo_out.wrapping_add((output as i32 * v.echo as i32) << 2);
        let mut new_phase = (v.rate as i32).wrapping_add(((v.addr as i32) << 12) | (v.phase >> 4) as i32);
        if (new_phase >> 12) >= v.end_addr as i32 {
            new_phase = new_phase.wrapping_sub((v.loop_len as i32) << 12);
        }
        new_phase = new_phase.clamp(-0x800_0000, 0x7FF_FFFF);
        let vm = &mut self.voice[i];
        vm.addr = (new_phase >> 12) as i16;
        vm.phase = ((new_phase << 4) & 0xFFFF) as u16;
        output
    }

    fn adpcm_update(&mut self, i: usize, curr: i16, nibble: bool) -> i16 {
        let mut a = self.adpcm[i];
        let mut curr = curr as i32;
        let mut step: i8;
        if !nibble {
            if a.cur_addr == a.end_addr { a.cur_vol = 0; }
            if a.flag != 0 {
                curr = 0;
                a.flag = 0;
                a.step_size = 10;
                a.cur_vol = a.volume;
                a.cur_addr = a.start_addr;
            }
            step = (self.read_sample(a.bank, a.cur_addr) >> 8) as i8;
        } else {
            step = (self.read_sample(a.bank, a.cur_addr) >> 4) as i8;
            a.cur_addr = a.cur_addr.wrapping_add(1);
        }
        step >>= 4;
        let mut delta = ((1 + ((step as i32) << 1).abs()) * a.step_size as i32) >> 1;
        if step <= 0 { delta = -delta; }
        delta += curr;
        delta = delta.clamp(-32768, 32767);
        let tab = self.dsp_rom((DATA_ADPCM_TAB as i32 + 8 + step as i32) as u16) as i16 as i32;
        a.step_size = ((tab * a.step_size as i32) >> 6) as i16;
        a.step_size = a.step_size.clamp(1, 2000);
        let out = ((delta * a.cur_vol as i32) >> 16) as i16;
        self.adpcm[i] = a;
        out
    }

    fn normal_update(&mut self) {
        self.ready = 0x80;
        let base = if self.state == STATE_NORMAL2 { DELAY_BASE_OFFSET2 } else { DELAY_BASE_OFFSET };
        self.echo.length = (self.echo.end_pos.wrapping_sub(base) as i16).clamp(0, 1024);

        let mut echo_in: i32 = 0;
        for i in 0..16 { self.voice_output[i] = self.voice_update(i, &mut echo_in); }
        let av = (self.state_counter % 3) as usize;
        let prev = self.voice_output[16 + av];
        self.voice_output[16 + av] = self.adpcm_update(av, prev, self.state_counter / 3 != 0);
        let echo_out = self.echo.apply(echo_in);

        for ch in 0..2 {
            // echo goes on the unfiltered part of the left and the filtered part of the right
            let mut wet: i32 = if ch == 1 { (echo_out as i32) << 14 } else { 0 };
            let mut dry: i32 = if ch == 0 { (echo_out as i32) << 14 } else { 0 };
            for i in 0..19 {
                let pan = self.voice_pan[i].wrapping_add(ch as u16 * PAN_TABLE_CH_OFFSET);
                let o = self.voice_output[i] as i32;
                dry = dry.wrapping_sub(o * self.dsp_rom(pan.wrapping_add(PAN_TABLE_DRY)) as i16 as i32);
                wet = wet.wrapping_sub(o * self.dsp_rom(pan.wrapping_add(PAN_TABLE_WET)) as i16 as i32);
            }
            dry = dry.clamp(-0x1FFF_FFFF, 0x1FFF_FFFF) << 2;
            wet = wet.clamp(-0x1FFF_FFFF, 0x1FFF_FFFF) << 2;
            wet = self.filter[ch].apply((wet >> 16) as i16);
            if self.state == STATE_NORMAL2 {
                dry = self.alt_filter[ch].apply((dry >> 16) as i16);
            }
            let mut output = self.wet[ch].apply(wet).wrapping_add(self.dry[ch].apply(dry));
            output = (output.wrapping_add(0x2000)) & !0x3FFF;
            self.out[ch] = (output >> 14).clamp(-0x7FFF, 0x7FFF) as i16;
            if self.delay_update != 0 {
                self.wet[ch].update();
                self.dry[ch].update();
            }
        }
        self.delay_update = 0;
        self.state_counter += 1;
        if self.state_counter > 5 {
            self.state_counter = 0;
            self.state = self.next_state;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A DSP ROM with a flat pan table: every pan entry = -0x2000 (so voice output
    /// passes through with a known gain) and filter taps = 0 except the last.
    fn dsp() -> Vec<u8> { vec![0; 0x2000] }

    #[test]
    fn boots_through_init_and_refresh_to_normal_and_signals_ready() {
        let mut q = QSound::new(vec![0; 0x10000], &dsp());
        assert_eq!(q.read(), 0);
        for _ in 0..4 { q.update(); }
        assert_eq!(q.state, STATE_NORMAL1);
        q.update();
        assert_eq!(q.read(), 0x80);
        q.write(0, 0x12);
        q.write(1, 0x34);
        q.write(2, 0x02);       // voice 0 rate
        assert_eq!(q.voice[0].rate, 0x1234);
        assert_eq!(q.read(), 0, "busy until the next sample");
        q.write(2, 0x08);       // register 8 = bank of voice 2 (it applies to the next voice)
        assert_eq!(q.voice[2].bank, 0x1234);
    }

    #[test]
    fn pcm_voice_steps_through_the_sample_and_loops() {
        let mut rom = vec![0u8; 0x10000];
        for (i, b) in rom.iter_mut().enumerate() { *b = i as u8; }
        let mut q = QSound::new(rom, &dsp());
        for _ in 0..4 { q.update(); }
        let regs = [(0x01, 0x0010), (0x02, 0x2000), (0x05, 0x0014), (0x04, 0x0004), (0x06, 0x4000)];
        for (r, d) in regs { q.write(0, (d >> 8) as u8); q.write(1, d as u8); q.write(2, r); }
        // rate 0x2000 = 2 samples per update; end 0x14, loop 4
        let mut addrs = Vec::new();
        for _ in 0..4 { q.update(); addrs.push(q.voice[0].addr); }
        assert_eq!(addrs, [0x12, 0x10, 0x12, 0x10]);
        // output = volume (1.0) x sample byte << 8 >> 14
        q.voice[0].addr = 0x12;
        let mut e = 0;
        assert_eq!(q.voice_update(0, &mut e), ((0x12i32 << 8) * 0x4000 >> 14) as i16);
    }

    #[test]
    fn delay_line_reads_behind_the_write_position() {
        let mut d = Delay { volume: 1, delay: 3, ..Default::default() };
        d.update();
        let outs: Vec<i32> = (1..=5).map(|k| d.apply(k << 16)).collect();
        assert_eq!(outs, [0, 0, 0, 1, 2]);
    }
}
