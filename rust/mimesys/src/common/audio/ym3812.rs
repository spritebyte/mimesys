//! Yamaha YM3812 (OPL2): 9 channels x 2 operators FM, 4 waveforms, the rhythm (drum)
//! mode on channels 6-8, AM / vibrato LFOs, two timers with an IRQ. Port of Aaron Giles'
//! ymfm (BSD-3-Clause): `opl_registers_base<2>`, the shared FM engine as OPL uses it
//! (envelope every sample, modulator delay, 2-operator algorithms, rhythm outputs) and
//! `ym3812::generate` through the 10.3 floating-point DAC. Shares tables with ym2151.rs.
//!
//! One chip sample = 72 input clocks (3 MHz -> 41.7 kHz). Timers count in samples
//! (A: 1024 - 4 x reg 2, B: 16 x (256 - reg 3)). `run(clocks)` fills `out` at the rate
//! given to `new` (mono, -1..1, averaged). The busy flag isn't emulated.

use serde::{Deserialize, Serialize};
use crate::common::audio::ym2151::{
    abs_sin_attenuation, attenuation_increment, attenuation_to_volume, bf, effective_rate,
    EG_ATTACK, EG_DECAY, EG_QUIET, EG_RELEASE, EG_SUSTAIN, KEYON_CSM, KEYON_NORMAL, PHASE_STEP_DYNAMIC,
};
use crate::common::audio::ym2612::roundtrip_fp;

const KEYON_RHYTHM: u32 = 1;
const STATUS_TIMERA: u8 = 0x40;
const STATUS_TIMERB: u8 = 0x20;
const STATUS_IRQ: u8 = 0x80;
const WAVE_LEN: usize = 1024;
const CLOCKS_PER_SAMPLE: u32 = 72;

#[derive(Clone, Default, Serialize, Deserialize)]
struct Cache {
    phase_step: u32,
    total_level: u32,
    block_freq: u32,
    multiple: u32,
    eg_sustain: u32,
    eg_rate: [u8; 6],
    waveform: u8,
}

#[derive(Clone, Serialize, Deserialize)]
struct Operator {
    opoffs: u32,
    choffs: u32,
    phase: u32,
    env_att: u16,
    env_state: usize,
    key_state: u8,
    keyon_live: u32,
    cache: Cache,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Channel {
    feedback: [i16; 2],
    feedback_in: i16,
}

/// The fixed operator pairs of the nine channels
const CH_OPS: [[usize; 2]; 9] = [[0, 3], [1, 4], [2, 5], [6, 9], [7, 10], [8, 11], [12, 15], [13, 16], [14, 17]];

fn opl_key_scale_atten(block: u32, fnum_4msb: u32) -> u32 {
    const T: [i32; 16] = [0, 24, 32, 37, 40, 43, 45, 47, 48, 50, 51, 52, 53, 54, 55, 56];
    (T[fnum_4msb as usize] - 8 * (block ^ 7) as i32).max(0) as u32
}

#[derive(Serialize, Deserialize)]
pub struct Ym3812 {
    regs: Vec<u8>,
    #[serde(skip, default = "waveforms")]
    waves: Vec<[u16; WAVE_LEN]>,
    ops: Vec<Operator>,
    chans: [Channel; 9],
    lfo_am_counter: u16,
    lfo_pm_counter: u16,
    noise_lfsr: u32,
    lfo_am: u8,
    env_counter: u32,
    status: u8,
    timer_running: [bool; 2],
    timer_left: [i64; 2],
    total_clocks: u32,
    active_channels: u32,
    modified_channels: u32,
    prepare_count: u32,
    address: u8,
    // output resampling
    clock_left: i64,
    clock_hz: u32,
    out_rate: u32,
    out_phase: u64,
    acc: i64,
    acc_n: u32,
    last: i32,
    pub out: Vec<f32>,
}

fn waveforms() -> Vec<[u16; WAVE_LEN]> {
    let mut w = vec![[0u16; WAVE_LEN]; 4];
    for i in 0..WAVE_LEN {
        w[0][i] = (abs_sin_attenuation(i as u32) | (bf(i as u32, 9, 1) << 15)) as u16;
    }
    let zero = w[0][0];
    for i in 0..WAVE_LEN {
        let v = w[0][i];
        w[1][i] = if bf(i as u32, 9, 1) != 0 { zero } else { v };
        w[2][i] = v & 0x7FFF;
        w[3][i] = if bf(i as u32, 8, 1) != 0 { zero } else { v & 0x7FFF };
    }
    w
}

impl Ym3812 {
    pub fn new(out_rate: u32) -> Self {
        let mut ops: Vec<Operator> = (0..18u32).map(|i| Operator {
            opoffs: i + 2 * (i / 6), choffs: 0, phase: 0, env_att: 0x3FF, env_state: EG_RELEASE,
            key_state: 0, keyon_live: 0, cache: Cache::default(),
        }).collect();
        for (c, pair) in CH_OPS.iter().enumerate() {
            for &o in pair { ops[o].choffs = c as u32; }
        }
        let mut y = Self {
            regs: vec![0; 0x100], waves: waveforms(), ops, chans: Default::default(),
            lfo_am_counter: 0, lfo_pm_counter: 0, noise_lfsr: 1, lfo_am: 0, env_counter: 0,
            status: 0, timer_running: [false; 2], timer_left: [0; 2], total_clocks: 0,
            active_channels: 0x1FF, modified_channels: 0x1FF, prepare_count: 0, address: 0,
            clock_left: 0, clock_hz: 3_000_000, out_rate, out_phase: 0, acc: 0, acc_n: 0, last: 0, out: Vec::with_capacity(2048),
        };
        y.reset();
        y
    }

    pub fn reset(&mut self) {
        self.regs.iter_mut().for_each(|r| *r = 0);
        self.status = 0;
        self.timer_running = [false; 2];
        for c in self.chans.iter_mut() { *c = Channel::default(); }
        for op in self.ops.iter_mut() {
            op.phase = 0; op.env_att = 0x3FF; op.env_state = EG_RELEASE; op.key_state = 0; op.keyon_live = 0;
        }
        self.modified_channels = 0x1FF;
        self.address = 0;
    }

    // ---- register fields ----
    fn byte(&self, off: u32, start: u32, len: u32, extra: u32) -> u32 { bf(self.regs[((off + extra) & 0xFF) as usize] as u32, start, len) }
    fn waveform_enable(&self) -> bool { self.byte(0x01, 5, 1, 0) != 0 }
    fn status_mask(&self) -> u8 { self.regs[0x04] & 0x78 }
    fn csm(&self) -> bool { self.byte(0x08, 7, 1, 0) != 0 }
    fn note_select(&self) -> u32 { self.byte(0x08, 6, 1, 0) }
    fn lfo_am_depth(&self) -> u32 { self.byte(0xBD, 7, 1, 0) }
    fn lfo_pm_depth(&self) -> u32 { self.byte(0xBD, 6, 1, 0) }
    fn rhythm_enable(&self) -> bool { self.byte(0xBD, 5, 1, 0) != 0 }
    fn ch_block_freq(&self, c: u32) -> u32 { (self.byte(0xB0, 0, 5, c) << 8) | self.byte(0xA0, 0, 8, c) }
    fn ch_feedback(&self, c: u32) -> u32 { self.byte(0xC0, 1, 3, c) }
    fn ch_algorithm(&self, c: u32) -> u32 { self.byte(0xC0, 0, 1, c) }
    fn op_lfo_am_enable(&self, o: u32) -> bool { self.byte(0x20, 7, 1, o) != 0 }
    fn op_lfo_pm_enable(&self, o: u32) -> bool { self.byte(0x20, 6, 1, o) != 0 }

    fn cache_operator(&self, op: &Operator) -> Cache {
        let (c, o) = (op.choffs, op.opoffs);
        let block_freq = self.ch_block_freq(c);
        let mut keycode = bf(block_freq, 10, 3) << 1;
        keycode |= bf(block_freq, 9 - self.note_select(), 1);
        let m = self.byte(0x20, 0, 4, o);
        let mut multiple = ((m & 0xE) | bf(0xC2AA, m, 1)) * 2;
        if multiple == 0 { multiple = 1; }
        let mut cache = Cache {
            block_freq, multiple,
            waveform: if self.waveform_enable() { self.byte(0xE0, 0, 2, o) as u8 } else { 0 },
            ..Default::default()
        };
        cache.phase_step = if self.op_lfo_pm_enable(o) { PHASE_STEP_DYNAMIC } else { Self::phase_step(&cache, 0) };
        cache.total_level = self.byte(0x40, 0, 6, o) << 3;
        let t = self.byte(0x40, 6, 2, o);
        let ksl = bf(t, 1, 1) | (bf(t, 0, 1) << 1);
        if ksl != 0 {
            cache.total_level += opl_key_scale_atten(bf(block_freq, 10, 3), bf(block_freq, 6, 4)) << ksl;
        }
        let mut sl = self.byte(0x80, 4, 4, o);
        sl |= (sl + 1) & 0x10;
        cache.eg_sustain = sl << 5;
        let ksrval = keycode >> (2 * (self.byte(0x20, 4, 1, o) ^ 1));
        let rr = self.byte(0x80, 0, 4, o) * 4;
        cache.eg_rate[EG_ATTACK] = effective_rate(self.byte(0x60, 4, 4, o) * 4, ksrval);
        cache.eg_rate[EG_DECAY] = effective_rate(self.byte(0x60, 0, 4, o) * 4, ksrval);
        cache.eg_rate[EG_SUSTAIN] = if self.byte(0x20, 5, 1, o) != 0 { 0 } else { effective_rate(rr, ksrval) };
        cache.eg_rate[EG_RELEASE] = effective_rate(rr, ksrval);
        cache.eg_rate[5] = 0x3F;
        cache
    }

    fn phase_step(cache: &Cache, lfo_raw_pm: i32) -> u32 {
        let mut fnum = bf(cache.block_freq, 0, 10) << 2;
        fnum = (fnum as i32 + ((lfo_raw_pm * bf(cache.block_freq, 7, 3) as i32) >> 1)) as u32;
        fnum &= 0xFFF;
        let block = bf(cache.block_freq, 10, 3);
        let step = (fnum << block) >> 2;
        (step * cache.multiple) >> 1
    }

    fn clock_noise_and_lfo(&mut self) -> i32 {
        let n = self.noise_lfsr << 1;
        self.noise_lfsr = n | (bf(n, 23, 1) ^ bf(n, 9, 1) ^ bf(n, 8, 1) ^ bf(n, 1, 1));
        let am = self.lfo_am_counter as u32;
        self.lfo_am_counter = self.lfo_am_counter.wrapping_add(1);
        if am >= 210 * 64 - 1 { self.lfo_am_counter = 0; }
        let shift = 9 - 2 * self.lfo_am_depth();
        self.lfo_am = ((if am < 105 * 64 { am } else { 210 * 64 + 63 - am }) >> shift) as u8;
        let pm = self.lfo_pm_counter as u32;
        self.lfo_pm_counter = self.lfo_pm_counter.wrapping_add(1);
        const PM: [i32; 8] = [8, 4, 0, -4, -8, -4, 0, 4];
        PM[bf(pm, 10, 3) as usize] >> (self.lfo_pm_depth() ^ 1)
    }

    // ---- operators ----
    fn op_prepare(&mut self, i: usize) -> bool {
        let cache = self.cache_operator(&self.ops[i]);
        self.ops[i].cache = cache;
        let keystate = (self.ops[i].keyon_live != 0) as u8;
        if keystate != self.ops[i].key_state {
            self.ops[i].key_state = keystate;
            if keystate != 0 { self.start_attack(i); } else { self.start_release(i); }
        }
        let op = &mut self.ops[i];
        op.keyon_live &= !(1 << KEYON_CSM);
        op.env_state != EG_RELEASE || op.env_att < EG_QUIET
    }

    fn start_attack(&mut self, i: usize) {
        let op = &mut self.ops[i];
        if op.env_state == EG_ATTACK { return; }
        op.env_state = EG_ATTACK;
        op.phase = 0;
        if op.cache.eg_rate[EG_ATTACK] >= 62 { op.env_att = 0; }
    }

    fn start_release(&mut self, i: usize) {
        let op = &mut self.ops[i];
        if op.env_state >= EG_RELEASE { return; }
        op.env_state = EG_RELEASE;
    }

    fn op_clock(&mut self, i: usize, env_counter: u32, lfo_raw_pm: i32) {
        // OPL clocks the envelope every sample (env_counter steps by 4)
        let ec = env_counter >> 2;
        let op = &mut self.ops[i];
        if op.env_state == EG_ATTACK && op.env_att == 0 { op.env_state = EG_DECAY; }
        if op.env_state == EG_DECAY && op.env_att as u32 >= op.cache.eg_sustain { op.env_state = EG_SUSTAIN; }
        let rate = op.cache.eg_rate[op.env_state] as u32;
        let rate_shift = rate >> 2;
        let ecs = ec.wrapping_shl(rate_shift);
        if bf(ecs, 0, 11) == 0 {
            let relevant = bf(ecs, if rate_shift <= 11 { 11 } else { rate_shift }, 3);
            let inc = attenuation_increment(rate, relevant);
            if op.env_state == EG_ATTACK {
                if rate < 62 {
                    let a = op.env_att as i32;
                    op.env_att = (a + ((!a * inc as i32) >> 4)) as u16;
                }
            } else {
                op.env_att += inc as u16;
                if op.env_att >= 0x400 { op.env_att = 0x3FF; }
            }
        }
        let mut step = op.cache.phase_step;
        if step == PHASE_STEP_DYNAMIC { step = Self::phase_step(&op.cache, lfo_raw_pm); }
        op.phase = op.phase.wrapping_add(step);
    }

    fn compute_volume(&self, i: usize, phase: u32, am_offset: u32) -> i32 {
        let op = &self.ops[i];
        if op.env_att > EG_QUIET { return 0; }
        let sin = self.waves[op.cache.waveform as usize & 3][phase as usize & (WAVE_LEN - 1)] as u32;
        let mut env = op.env_att as u32;
        if self.op_lfo_am_enable(op.opoffs) { env += am_offset; }
        env += op.cache.total_level;
        let env = env.min(0x3FF) << 2;
        let r = attenuation_to_volume((sin & 0x7FFF) + env) as i32;
        if bf(sin, 15, 1) != 0 { -r } else { r }
    }

    fn op_phase(&self, i: usize) -> u32 { self.ops[i].phase >> 10 }

    fn keyonoff(&mut self, c: usize, states: u32, kind: u32) {
        for (k, &o) in CH_OPS[c].iter().enumerate() {
            let op = &mut self.ops[o];
            op.keyon_live = (op.keyon_live & !(1 << kind)) | (bf(states, k as u32, 1) << kind);
        }
    }

    // ---- channels ----
    fn feedback_mod(&self, c: usize) -> i32 {
        let fb = self.ch_feedback(c as u32);
        if fb == 0 { return 0; }
        let f = &self.chans[c].feedback;
        (f[0] as i32 + f[1] as i32) >> (10 - fb)
    }

    fn output_2op(&mut self, c: usize) -> i32 {
        let [o1, o2] = CH_OPS[c];
        let am = self.lfo_am as u32;
        let m = self.feedback_mod(c);
        let op1 = self.compute_volume(o1, self.op_phase(o1).wrapping_add(m as u32), am);
        self.chans[c].feedback_in = op1 as i16;
        // OPL2 modulates with the previous sample (MODULATOR_DELAY)
        let delayed = self.chans[c].feedback[1] as i32;
        if self.ch_algorithm(c as u32) == 0 {
            self.compute_volume(o2, self.op_phase(o2).wrapping_add((delayed >> 1) as u32), am) >> 1
        } else {
            let r = (delayed >> 1) + (self.compute_volume(o2, self.op_phase(o2), am) >> 1);
            r.clamp(-32768, 32767)
        }
    }

    fn output_rhythm(&mut self, c: usize, phase_select: u32) -> i32 {
        let [o1, o2] = CH_OPS[c];
        let am = self.lfo_am as u32;
        match c {
            6 => {
                // bass drum: a normal pair, except algorithm 1 ignores operator 1
                let m = self.feedback_mod(c);
                let op1 = self.compute_volume(o1, self.op_phase(o1).wrapping_add(m as u32), am);
                self.chans[c].feedback_in = op1 as i16;
                let m = if self.ch_algorithm(6) & 1 != 0 { 0 } else { op1 >> 1 };
                (self.compute_volume(o2, self.op_phase(o2).wrapping_add(m as u32), am) >> 1) * 2
            }
            7 => {
                // high hat (operator 13) and snare drum (operator 16) from noise and phase bits
                let noise = (self.noise_lfsr >> 23) & 1;
                let phase = (phase_select << 9) | (0xD0 >> (2 * (noise ^ phase_select)));
                let mut r = self.compute_volume(o1, phase, am) >> 1;
                let p13 = self.op_phase(o1);
                let phase = (0x100 << bf(p13, 8, 1)) ^ (noise << 8);
                r += self.compute_volume(o2, phase, am) >> 1;
                r.clamp(-32768, 32767) * 2
            }
            _ => {
                // tom tom (operator 14) and top cymbal (operator 17)
                let mut r = self.compute_volume(o1, self.op_phase(o1), am) >> 1;
                r += self.compute_volume(o2, 0x100 | (phase_select << 9), am) >> 1;
                r.clamp(-32768, 32767) * 2
            }
        }
    }

    // ---- engine ----
    fn engine_clock(&mut self) {
        self.total_clocks = self.total_clocks.wrapping_add(1);
        let pc = self.prepare_count;
        self.prepare_count += 1;
        if self.modified_channels != 0 || pc >= 4096 {
            self.active_channels = 0;
            for c in 0..9 {
                let mut active = false;
                for &o in &CH_OPS[c] { if self.op_prepare(o) { active = true; } }
                if active { self.active_channels |= 1 << c; }
            }
            self.modified_channels = 0;
            self.prepare_count = 0;
        }
        self.env_counter = self.env_counter.wrapping_add(4);
        let pm = self.clock_noise_and_lfo();
        let ec = self.env_counter;
        for c in 0..9 {
            let ch = &mut self.chans[c];
            ch.feedback[0] = ch.feedback[1];
            ch.feedback[1] = ch.feedback_in;
            for &o in &CH_OPS[c] { self.op_clock(o, ec, pm); }
        }
    }

    /// One chip sample (ymfm's ym3812 output, about +-32K after the DAC)
    pub fn generate(&mut self) -> i32 {
        self.clock_timers();
        self.engine_clock();
        let mut total = 0i32;
        let rhythm = self.rhythm_enable();
        let phase_select = if rhythm {
            let (p13, p17) = (self.op_phase(13), self.op_phase(17));
            (bf(p13, 2, 1) ^ bf(p13, 7, 1)) | bf(p13, 3, 1) | (bf(p17, 5, 1) ^ bf(p17, 3, 1))
        } else { 0 };
        for c in 0..9 {
            if self.active_channels & (1 << c) == 0 { continue; }
            total += if rhythm && c >= 6 { self.output_rhythm(c, phase_select) } else { self.output_2op(c) };
        }
        roundtrip_fp(total.clamp(-32768, 32767))
    }

    // ---- timers / status ----
    fn update_timer(&mut self, t: usize, enable: bool, delta: i64) {
        if enable && !self.timer_running[t] {
            let period = if t == 0 { 1024 - (self.regs[0x02] as i64) * 4 } else { 16 * (256 - self.regs[0x03] as i64) };
            self.timer_left[t] = (period + delta).max(1);
            self.timer_running[t] = true;
        } else if !enable {
            self.timer_running[t] = false;
        }
    }

    fn set_reset_status(&mut self, set: u8, reset: u8) {
        self.status = (self.status | set) & !reset;
        // IRQ: a timer flag that isn't masked
        if self.status & (STATUS_TIMERA | STATUS_TIMERB) & !self.status_mask() != 0 {
            self.status |= STATUS_IRQ;
        } else {
            self.status &= !STATUS_IRQ;
        }
    }

    fn timer_expired(&mut self, t: usize) {
        self.set_reset_status(if t == 0 { STATUS_TIMERA } else { STATUS_TIMERB }, 0);
        if t == 0 && self.csm() {
            for c in 0..9 { self.keyonoff(c, 3, KEYON_CSM); }
            self.modified_channels = 0x1FF;
        }
        self.timer_running[t] = false;
        self.update_timer(t, true, 0);
    }

    fn clock_timers(&mut self) {
        for t in 0..2 {
            if !self.timer_running[t] { continue; }
            self.timer_left[t] -= 1;
            if self.timer_left[t] <= 0 { self.timer_expired(t); }
        }
    }

    fn mode_write(&mut self, data: u8) {
        self.modified_channels = 0x1FF;
        if data & 0x80 != 0 { self.regs[0x04] |= 0x80; } else { self.regs[0x04] = data; }
        if self.regs[0x04] & 0x80 != 0 {
            self.set_reset_status(0, 0x78);
        } else {
            let mut reset = 0u8;
            if data & 0x20 != 0 { reset |= STATUS_TIMERB; }
            if data & 0x40 != 0 { reset |= STATUS_TIMERA; }
            self.set_reset_status(0, reset);
            let d = -((self.total_clocks & 15) as i64);
            self.update_timer(1, data & 0x02 != 0, d);
            self.update_timer(0, data & 0x01 != 0, 0);
        }
    }

    // ---- the CPU's side ----
    /// Offset 0: status (IRQ, timer A, timer B; the low bits read 6). Offset 1: open bus.
    pub fn read(&self, offset: u8) -> u8 {
        if offset & 1 == 0 { (self.status & !self.status_mask()) | 0x06 } else { 0xFF }
    }

    pub fn write(&mut self, offset: u8, data: u8) {
        if offset & 1 == 0 { self.address = data; return; }
        let r = self.address;
        if r == 0x04 { return self.mode_write(data); }
        self.modified_channels = 0x1FF;
        self.regs[r as usize] = data;
        if r == 0xBD {
            let m = if bf(data as u32, 5, 1) != 0 { bf(data as u32, 0, 5) } else { 0 };
            self.keyonoff(6, if bf(m, 4, 1) != 0 { 3 } else { 0 }, KEYON_RHYTHM);
            self.keyonoff(7, bf(m, 0, 1) | (bf(m, 3, 1) << 1), KEYON_RHYTHM);
            self.keyonoff(8, bf(m, 2, 1) | (bf(m, 1, 1) << 1), KEYON_RHYTHM);
        } else if r & 0xF0 == 0xB0 && (r & 0x0F) < 9 {
            let on = if bf(data as u32, 5, 1) != 0 { 3 } else { 0 };
            self.keyonoff((r & 0x0F) as usize, on, KEYON_NORMAL);
        }
    }

    /// Debug: the channels keyed on (bit per channel; bits 9-13 = rhythm keys) and the timer loads
    pub fn keys(&self) -> (u16, u8) {
        let mut k = 0u16;
        for c in 0..9 { if self.regs[0xB0 + c] & 0x20 != 0 { k |= 1 << c; } }
        if self.rhythm_enable() { k |= ((self.regs[0xBD] & 0x1F) as u16) << 9; }
        (k, self.regs[0x04])
    }

    /// The IRQ pin (active while a timer flag isn't masked)
    pub fn irq(&self) -> bool { self.status & STATUS_IRQ != 0 }

    /// `clocks` input clocks pass: samples into `out` at the output rate
    pub fn run(&mut self, clocks: u32) {
        self.clock_left += clocks as i64;
        while self.clock_left >= CLOCKS_PER_SAMPLE as i64 {
            self.clock_left -= CLOCKS_PER_SAMPLE as i64;
            self.last = self.generate();
            self.acc += self.last as i64;
            self.acc_n += 1;
            // the chip's 41.7 kHz (for a 3 MHz clock) -> the output rate
            self.out_phase += self.out_rate as u64 * CLOCKS_PER_SAMPLE as u64;
            while self.out_phase >= self.chip_rate_x72() {
                self.out_phase -= self.chip_rate_x72();
                let v = if self.acc_n > 0 { self.acc as f32 / self.acc_n as f32 } else { self.last as f32 };
                self.acc = 0;
                self.acc_n = 0;
                self.out.push(v / 32768.0);
            }
        }
    }

    /// The chip's clock (= its sample rate x 72); 3 MHz unless `with_clock` says otherwise
    fn chip_rate_x72(&self) -> u64 { self.clock_hz as u64 }

    pub fn with_clock(mut self, hz: u32) -> Self { self.clock_hz = hz; self }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(y: &mut Ym3812, r: u8, v: u8) { y.write(0, r); y.write(1, v); }

    /// Channel 0 as a plain sine: carrier full volume, fast attack, sustained
    fn tone(y: &mut Ym3812) {
        w(y, 0x20, 0x21); w(y, 0x23, 0x21);   // sustain, multiple 1
        w(y, 0x40, 0x3F); w(y, 0x43, 0x00);   // modulator silent, carrier loud
        w(y, 0x60, 0xF0); w(y, 0x63, 0xF0);
        w(y, 0x80, 0x0F); w(y, 0x83, 0x0F);
        w(y, 0xC0, 0x00);
        w(y, 0xA0, 0x41); w(y, 0xB0, 0x32);   // key on, block 4, fnum 0x241: 440 Hz at 3.58 MHz, 367 at 3
    }

    #[test]
    fn a_keyed_note_sounds_and_stops_after_key_off() {
        let mut y = Ym3812::new(44100);
        tone(&mut y);
        y.run(72 * 4000);
        let peak = y.out.iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!(peak > 0.1, "a loud sine ({peak})");
        // the number of sign changes ~ 2 x 577 x 41667 / 2^16
        let tail = &y.out[1000..];
        let flips = tail.windows(2).filter(|p| (p[0] < 0.0) != (p[1] < 0.0)).count();
        let secs = tail.len() as f32 / 44100.0;
        let hz = flips as f32 / 2.0 / secs;
        assert!((355.0..380.0).contains(&hz), "about 367 Hz ({hz})");
        w(&mut y, 0xB0, 0x12);               // key off: release rate 15 is quick
        y.run(72 * 4000);
        y.out.clear();
        y.run(72 * 100);
        assert!(y.out.iter().all(|v| v.abs() < 0.001), "silent after the release");
    }

    #[test]
    fn timer_a_raises_its_flag_and_the_irq_until_reset() {
        let mut y = Ym3812::new(44100);
        assert_eq!(y.read(0), 0x06);
        w(&mut y, 0x02, 0xFF);               // 4 samples
        w(&mut y, 0x04, 0x01);               // start timer A
        y.run(72 * 3);
        assert!(!y.irq());
        y.run(72 * 2);
        assert!(y.irq());
        assert_eq!(y.read(0), 0xC6);
        w(&mut y, 0x04, 0x80);               // IRQ reset
        assert!(!y.irq());
        assert_eq!(y.read(0), 0x06);
        w(&mut y, 0x04, 0x41);               // masked timer A: no flag, no IRQ
        y.run(72 * 10);
        assert!(!y.irq());
        assert_eq!(y.read(0) & 0xE0, 0);
    }

    #[test]
    fn timer_b_counts_sixteen_samples_a_step() {
        let mut y = Ym3812::new(44100);
        w(&mut y, 0x03, 0xFE);               // 2 steps = 32 samples
        w(&mut y, 0x04, 0x02);
        y.run(72 * 20);
        assert!(!y.irq());
        y.run(72 * 14);
        assert!(y.irq());
        assert_eq!(y.read(0), 0xA6);
    }

    #[test]
    fn the_rhythm_section_makes_noise() {
        let mut y = Ym3812::new(44100);
        for o in [0x10u8, 0x11, 0x12, 0x13, 0x14, 0x15] {
            w(&mut y, 0x20 + o, 0x01); w(&mut y, 0x40 + o, 0x00);
            w(&mut y, 0x60 + o, 0xF4); w(&mut y, 0x80 + o, 0x04);
        }
        for c in 6..9u8 { w(&mut y, 0xA0 + c, 0x80); w(&mut y, 0xB0 + c, 0x0A); }
        w(&mut y, 0xBD, 0x3F);               // rhythm on, all five drums
        y.run(72 * 2000);
        let peak = y.out.iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!(peak > 0.05, "drums ({peak})");
    }
}
