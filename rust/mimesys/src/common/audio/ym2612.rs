//! Yamaha YM2612 (OPN2): 6 channels x 4 operators FM, LFO, SSG-EG envelopes, channel 3
//! special (per-operator frequency) mode, an 8-bit DAC replacing channel 6, 2 timers.
//! Port of Aaron Giles' ymfm (BSD-3-Clause): the OPNA register set, the shared FM
//! engine and ymfm::ym2612 with its DAC "ladder" discontinuity, sample for sample
//! (checked against ymfm itself, see the tests). Shares tables with ym2151.rs.
//!
//! One `generate()` = one output sample = 144 input clocks (7.67 MHz -> 53.3 kHz).
//! Timers count in samples. The busy flag isn't emulated (always ready); the IRQ pin
//! isn't connected on the Genesis.
//!
//! Registers: port 0/1 = address/data for 0x21-0xB6 bank 0 (channels 1-3, globals),
//! port 2/3 = address/data for bank 1 (channels 4-6).

use serde::{Deserialize, Serialize};
use crate::common::audio::ym2151::{
    abs_sin_attenuation, attenuation_increment, attenuation_to_volume, bf, detune_adjustment,
    effective_rate, ALGORITHM_OPS, EG_ATTACK, EG_DECAY, EG_QUIET, EG_RELEASE, EG_SUSTAIN,
    KEYON_CSM, KEYON_NORMAL, PHASE_STEP_DYNAMIC,
};

pub const STATUS_TIMERA: u8 = 0x01;
pub const STATUS_TIMERB: u8 = 0x02;

#[derive(Clone, Default, Serialize, Deserialize)]
struct Cache {
    phase_step: u32,
    total_level: u32,
    block_freq: u32,
    detune: i32,
    multiple: u32,
    eg_sustain: u32,
    eg_rate: [u8; 6],
}

#[derive(Clone, Serialize, Deserialize)]
struct Operator {
    choffs: u32,
    opoffs: u32,
    phase: u32,
    env_att: u16,
    env_state: usize,
    ssg_inverted: bool,
    key_state: u8,
    keyon_live: u8,
    cache: Cache,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Channel {
    feedback: [i16; 2],
    feedback_in: i16,
}

/// ymfm's 2-bit -> AM shift and PM tables for OPN
static LFO_MAX_COUNT: [u8; 8] = [109, 78, 72, 68, 63, 45, 9, 6];
static LFO_PM_SHIFTS: [[u8; 8]; 8] = [
    [0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77],
    [0x77, 0x77, 0x77, 0x77, 0x72, 0x72, 0x72, 0x72],
    [0x77, 0x77, 0x77, 0x72, 0x72, 0x72, 0x17, 0x17],
    [0x77, 0x77, 0x72, 0x72, 0x17, 0x17, 0x12, 0x12],
    [0x77, 0x77, 0x72, 0x17, 0x17, 0x17, 0x12, 0x07],
    [0x77, 0x77, 0x17, 0x12, 0x07, 0x07, 0x02, 0x01],
    [0x77, 0x77, 0x17, 0x12, 0x07, 0x07, 0x02, 0x01],
    [0x77, 0x77, 0x17, 0x12, 0x07, 0x07, 0x02, 0x01],
];

fn lfo_pm_phase_adjustment(fnum_bits: u32, pm_sens: u32, lfo_raw_pm: i32) -> i32 {
    let abs_pm = lfo_raw_pm.unsigned_abs();
    let shifts = LFO_PM_SHIFTS[pm_sens as usize][bf(abs_pm, 0, 3) as usize] as u32;
    let mut adjust = ((fnum_bits >> bf(shifts, 0, 4)) + (fnum_bits >> bf(shifts, 4, 4))) as i32;
    if pm_sens > 5 { adjust <<= pm_sens - 5; }
    adjust >>= 2;
    if lfo_raw_pm < 0 { -adjust } else { adjust }
}

fn dac_discontinuity(v: i32) -> i32 { if v < 0 { v - 3 } else { v + 4 } }

#[derive(Clone, Serialize, Deserialize)]
pub struct Ym2612 {
    regs: Vec<u8>,
    lfo_counter: u32,
    lfo_am: u8,
    ops: Vec<Operator>,
    chans: [Channel; 6],
    env_counter: u32,
    status: u8,
    timer_running: [bool; 2],
    timer_left: [i64; 2],
    total_clocks: u8,
    active_channels: u32,
    modified_channels: u32,
    prepare_count: u32,
    address: u16,
    dac_data: u16,
    dac_enable: bool,
}

impl Default for Ym2612 { fn default() -> Self { Self::new() } }

/// operator number -> register offset
fn operator_offset(op: u32) -> u32 { (op % 12) + (op % 12) / 3 + 0x100 * (op / 12) }
fn channel_offset(c: u32) -> u32 { c % 3 + 0x100 * (c / 3) }

impl Ym2612 {
    pub fn new() -> Self {
        let ops = (0..24u32).map(|i| Operator {
            choffs: 0, opoffs: operator_offset(i), phase: 0, env_att: 0x3FF, env_state: EG_RELEASE,
            ssg_inverted: false, key_state: 0, keyon_live: 0, cache: Cache::default(),
        }).collect();
        let mut y = Self {
            regs: vec![0; 0x200], lfo_counter: 0, lfo_am: 0, ops, chans: Default::default(),
            env_counter: 0, status: 0, timer_running: [false; 2], timer_left: [0; 2], total_clocks: 0,
            active_channels: 0x3F, modified_channels: 0x3F, prepare_count: 0,
            address: 0, dac_data: 0, dac_enable: false,
        };
        for c in 0..6u32 {
            for o in Self::ch_ops(c as usize) { y.ops[o].choffs = channel_offset(c); }
        }
        y.reset();
        y
    }

    pub fn reset(&mut self) {
        self.status = 0;
        self.regs.iter_mut().for_each(|r| *r = 0);
        for r in [0xB4, 0xB5, 0xB6, 0x1B4, 0x1B5, 0x1B6] { self.regs[r] = 0xC0; }
        self.mode_write(0);
        for c in self.chans.iter_mut() { *c = Channel::default(); }
        for op in self.ops.iter_mut() {
            op.phase = 0; op.env_att = 0x3FF; op.env_state = EG_RELEASE; op.ssg_inverted = false;
            op.key_state = 0; op.keyon_live = 0;
        }
        self.address = 0;
        self.dac_data = 0;
        self.dac_enable = false;
    }

    /// Operators of channel c in slot order (operator 1, 2, 3, 4 as the algorithms use them).
    fn ch_ops(c: usize) -> [usize; 4] {
        let base = (c / 3) * 12 + c % 3;
        [base, base + 6, base + 3, base + 9]
    }

    // ---- register fields ----
    fn byte(&self, off: u32, start: u32, len: u32, extra: u32) -> u32 { bf(self.regs[(off + extra) as usize] as u32, start, len) }
    fn lfo_enable(&self) -> bool { self.byte(0x22, 3, 1, 0) != 0 }
    fn lfo_rate(&self) -> usize { self.byte(0x22, 0, 3, 0) as usize }
    fn timer_a_value(&self) -> i64 { ((self.regs[0x24] as i64) << 2) | (self.regs[0x25] & 3) as i64 }
    fn timer_b_value(&self) -> i64 { self.regs[0x26] as i64 }
    fn csm(&self) -> bool { self.byte(0x27, 6, 2, 0) == 2 }
    fn multi_freq(&self) -> bool { self.byte(0x27, 6, 2, 0) != 0 }
    fn multi_block_freq(&self, n: u32) -> u32 { (self.byte(0xAC, 0, 6, n) << 8) | self.byte(0xA8, 0, 8, n) }
    fn ch_block_freq(&self, c: u32) -> u32 { (self.byte(0xA4, 0, 6, c) << 8) | self.byte(0xA0, 0, 8, c) }
    fn ch_feedback(&self, c: u32) -> u32 { self.byte(0xB0, 3, 3, c) }
    fn ch_algorithm(&self, c: u32) -> u32 { self.byte(0xB0, 0, 3, c) }
    fn ch_output_0(&self, c: u32) -> bool { self.byte(0xB4, 7, 1, c) != 0 }
    fn ch_output_1(&self, c: u32) -> bool { self.byte(0xB4, 6, 1, c) != 0 }
    fn ch_am_sens(&self, c: u32) -> u32 { self.byte(0xB4, 4, 2, c) }
    fn ch_pm_sens(&self, c: u32) -> u32 { self.byte(0xB4, 0, 3, c) }
    fn op_lfo_am_enable(&self, o: u32) -> bool { self.byte(0x60, 7, 1, o) != 0 }
    fn op_ssg_eg_enable(&self, o: u32) -> bool { self.byte(0x90, 3, 1, o) != 0 }
    fn op_ssg_eg_mode(&self, o: u32) -> u32 { self.byte(0x90, 0, 3, o) }

    fn lfo_am_offset(&self, c: u32) -> u32 {
        let am_shift = (1u32 << (self.ch_am_sens(c) ^ 3)) - 1;
        ((self.lfo_am as u32) << 1) >> am_shift
    }

    fn cache_operator(&self, op: &Operator) -> Cache {
        let (c, o) = (op.choffs, op.opoffs);
        let mut block_freq = self.ch_block_freq(c);
        if self.multi_freq() && c == 2 {
            match o {
                2 => block_freq = self.multi_block_freq(1),
                10 => block_freq = self.multi_block_freq(2),
                6 => block_freq = self.multi_block_freq(0),
                _ => {}
            }
        }
        let mut keycode = bf(block_freq, 10, 4) << 1;
        keycode |= bf(0xFE80, bf(block_freq, 7, 4), 1);
        let mut cache = Cache {
            block_freq,
            detune: detune_adjustment(self.byte(0x30, 4, 3, o), keycode),
            multiple: (self.byte(0x30, 0, 4, o) * 2).max(1),
            ..Default::default()
        };
        cache.phase_step = if !self.lfo_enable() || self.ch_pm_sens(c) == 0 {
            self.compute_phase_step(c, &cache, 0)
        } else {
            PHASE_STEP_DYNAMIC
        };
        cache.total_level = self.byte(0x40, 0, 7, o) << 3;
        let mut sl = self.byte(0x80, 4, 4, o);
        sl |= (sl + 1) & 0x10;
        cache.eg_sustain = sl << 5;
        let ksrval = keycode >> (self.byte(0x50, 6, 2, o) ^ 3);
        cache.eg_rate[EG_ATTACK] = effective_rate(self.byte(0x50, 0, 5, o) * 2, ksrval);
        cache.eg_rate[EG_DECAY] = effective_rate(self.byte(0x60, 0, 5, o) * 2, ksrval);
        cache.eg_rate[EG_SUSTAIN] = effective_rate(self.byte(0x70, 0, 5, o) * 2, ksrval);
        cache.eg_rate[EG_RELEASE] = effective_rate(self.byte(0x80, 0, 4, o) * 4 + 2, ksrval);
        cache
    }

    fn compute_phase_step(&self, c: u32, cache: &Cache, lfo_raw_pm: i32) -> u32 {
        let mut fnum = bf(cache.block_freq, 0, 11) << 1;
        let pm = self.ch_pm_sens(c);
        if pm != 0 {
            fnum = (fnum as i32 + lfo_pm_phase_adjustment(bf(cache.block_freq, 4, 7), pm, lfo_raw_pm)) as u32;
            fnum &= 0xFFF;
        }
        let block = bf(cache.block_freq, 11, 3);
        let mut step = (fnum << block) >> 2;
        step = step.wrapping_add(cache.detune as u32);
        step &= 0x1FFFF;
        step.wrapping_mul(cache.multiple) >> 1
    }

    fn clock_lfo(&mut self) -> i32 {
        if !self.lfo_enable() {
            self.lfo_counter = 0;
            self.lfo_am = 0x3F;
            return 0;
        }
        let sub = self.lfo_counter as u8 as u32;
        self.lfo_counter = self.lfo_counter.wrapping_add(1);
        if sub >= LFO_MAX_COUNT[self.lfo_rate()] as u32 {
            self.lfo_counter = self.lfo_counter.wrapping_add(0x101 - sub);
        }
        self.lfo_am = bf(self.lfo_counter, 8, 6) as u8;
        if bf(self.lfo_counter, 14, 1) == 0 { self.lfo_am ^= 0x3F; }
        let mut pm = bf(self.lfo_counter, 10, 3) as i32;
        if bf(self.lfo_counter, 13, 1) != 0 { pm ^= 7; }
        if bf(self.lfo_counter, 14, 1) != 0 { -pm } else { pm }
    }

    // ---- operators ----
    fn op_prepare(&mut self, i: usize) -> bool {
        let cache = self.cache_operator(&self.ops[i]);
        self.ops[i].cache = cache;
        let keystate = (self.ops[i].keyon_live != 0) as u8;
        if keystate != self.ops[i].key_state {
            self.ops[i].key_state = keystate;
            if keystate != 0 { self.start_attack(i, false); } else { self.start_release(i); }
        }
        let op = &mut self.ops[i];
        op.keyon_live &= !(1 << KEYON_CSM);
        op.env_state != EG_RELEASE || op.env_att < EG_QUIET
    }

    fn start_attack(&mut self, i: usize, restart: bool) {
        if self.ops[i].env_state == EG_ATTACK { return; }
        let o = self.ops[i].opoffs;
        let inv = self.op_ssg_eg_enable(o) && bf(self.op_ssg_eg_mode(o), 2, 1) != 0;
        let op = &mut self.ops[i];
        op.env_state = EG_ATTACK;
        if !restart {
            op.ssg_inverted = inv;
            op.phase = 0;
        }
        if op.cache.eg_rate[EG_ATTACK] >= 62 { op.env_att = 0; }
    }

    fn start_release(&mut self, i: usize) {
        let op = &mut self.ops[i];
        if op.env_state >= EG_RELEASE { return; }
        op.env_state = EG_RELEASE;
        if op.ssg_inverted {
            op.env_att = (0x200u16.wrapping_sub(op.env_att)) & 0x3FF;
            op.ssg_inverted = false;
        }
    }

    fn clock_ssg_eg_state(&mut self, i: usize) {
        if bf(self.ops[i].env_att as u32, 9, 1) == 0 { return; }
        let mode = self.op_ssg_eg_mode(self.ops[i].opoffs);
        if bf(mode, 0, 1) != 0 {
            let op = &mut self.ops[i];
            op.ssg_inverted = (bf(mode, 2, 1) ^ bf(mode, 1, 1)) != 0;
            if op.env_state != EG_ATTACK { op.env_att = if op.ssg_inverted { 0x200 } else { 0x3FF }; }
        } else {
            self.ops[i].ssg_inverted ^= bf(mode, 1, 1) != 0;
            let st = self.ops[i].env_state;
            if st == EG_DECAY || st == EG_SUSTAIN { self.start_attack(i, true); }
            if bf(mode, 1, 1) == 0 { self.ops[i].phase = 0; }
        }
        let op = &mut self.ops[i];
        if op.env_state == EG_RELEASE { op.env_att = 0x3FF; }
    }

    fn op_clock(&mut self, i: usize, env_counter: u32, lfo_raw_pm: i32) {
        let ssg = self.op_ssg_eg_enable(self.ops[i].opoffs);
        if ssg { self.clock_ssg_eg_state(i); } else { self.ops[i].ssg_inverted = false; }
        if bf(env_counter, 0, 2) == 0 {
            let op = &mut self.ops[i];
            if op.env_state == EG_ATTACK && op.env_att == 0 { op.env_state = EG_DECAY; }
            if op.env_state == EG_DECAY && op.env_att as u32 >= op.cache.eg_sustain { op.env_state = EG_SUSTAIN; }
            let rate = op.cache.eg_rate[op.env_state] as u32;
            let rate_shift = rate >> 2;
            let ec = (env_counter >> 2).wrapping_shl(rate_shift);
            if bf(ec, 0, 11) == 0 {
                let relevant = bf(ec, if rate_shift <= 11 { 11 } else { rate_shift }, 3);
                let inc = attenuation_increment(rate, relevant);
                if op.env_state == EG_ATTACK {
                    if rate < 62 {
                        let a = op.env_att as i32;
                        op.env_att = (a + ((!a * inc as i32) >> 4)) as u16;
                    }
                } else {
                    if !ssg { op.env_att += inc as u16; }
                    else if op.env_att < 0x200 { op.env_att += 4 * inc as u16; }
                    if op.env_att >= 0x400 { op.env_att = 0x3FF; }
                }
            }
        }
        let mut step = self.ops[i].cache.phase_step;
        if step == PHASE_STEP_DYNAMIC {
            let op = &self.ops[i];
            step = self.compute_phase_step(op.choffs, &op.cache, lfo_raw_pm);
        }
        let op = &mut self.ops[i];
        op.phase = op.phase.wrapping_add(step);
    }

    fn envelope_attenuation(&self, i: usize, am_offset: u32) -> u32 {
        let op = &self.ops[i];
        let mut r = op.env_att as u32;
        if op.ssg_inverted { r = (0x200u32.wrapping_sub(r)) & 0x3FF; }
        if self.op_lfo_am_enable(op.opoffs) { r += am_offset; }
        r += op.cache.total_level;
        r.min(0x3FF)
    }

    fn compute_volume(&self, i: usize, phase: u32, am_offset: u32) -> i32 {
        if self.ops[i].env_att > EG_QUIET { return 0; }
        let p = phase & 0x3FF;
        let sin_att = abs_sin_attenuation(p) | (bf(p, 9, 1) << 15);
        let env = self.envelope_attenuation(i, am_offset) << 2;
        let r = attenuation_to_volume((sin_att & 0x7FFF) + env) as i32;
        if bf(sin_att, 15, 1) != 0 { -r } else { r }
    }

    fn op_phase(&self, i: usize) -> u32 { self.ops[i].phase >> 10 }

    /// Channel c's output (9 bits: rshift 5, clip 256), added to `out` where enabled.
    fn channel_output(&mut self, c: usize, out: &mut [i32; 2]) {
        let ops = Self::ch_ops(c);
        let co = channel_offset(c as u32);
        let am = self.lfo_am_offset(co);
        let fb = self.ch_feedback(co);
        let mut opmod = 0i32;
        if fb != 0 {
            let f = &self.chans[c].feedback;
            opmod = (f[0] as i32 + f[1] as i32) >> (10 - fb);
        }
        let op1 = self.compute_volume(ops[0], self.op_phase(ops[0]).wrapping_add(opmod as u32), am);
        self.chans[c].feedback_in = op1 as i16;
        if !self.ch_output_0(co) && !self.ch_output_1(co) { return; }

        let a = ALGORITHM_OPS[self.ch_algorithm(co) as usize] as u32;
        let mut opout = [0i16; 8];
        opout[1] = op1 as i16;
        let m = (opout[bf(a, 0, 1) as usize] as i32) >> 1;
        opout[2] = self.compute_volume(ops[1], self.op_phase(ops[1]).wrapping_add(m as u32), am) as i16;
        opout[5] = opout[1].wrapping_add(opout[2]);
        let m = (opout[bf(a, 1, 3) as usize] as i32) >> 1;
        opout[3] = self.compute_volume(ops[2], self.op_phase(ops[2]).wrapping_add(m as u32), am) as i16;
        opout[6] = opout[1].wrapping_add(opout[3]);
        opout[7] = opout[2].wrapping_add(opout[3]);
        let m = (opout[bf(a, 4, 3) as usize] as i32) >> 1;
        let mut result = self.compute_volume(ops[3], self.op_phase(ops[3]).wrapping_add(m as u32), am) >> 5;
        let (clipmax, clipmin) = (256, -257);
        if bf(a, 7, 1) != 0 { result = (result + (opout[1] as i32 >> 5)).clamp(clipmin, clipmax); }
        if bf(a, 8, 1) != 0 { result = (result + (opout[2] as i32 >> 5)).clamp(clipmin, clipmax); }
        if bf(a, 9, 1) != 0 { result = (result + (opout[3] as i32 >> 5)).clamp(clipmin, clipmax); }
        if self.ch_output_0(co) { out[0] += result; }
        if self.ch_output_1(co) { out[1] += result; }
    }

    // ---- engine ----
    fn engine_clock(&mut self) {
        self.total_clocks = self.total_clocks.wrapping_add(1);
        let pc = self.prepare_count;
        self.prepare_count += 1;
        if self.modified_channels != 0 || pc >= 4096 {
            self.active_channels = 0;
            for c in 0..6 {
                let mut active = false;
                for &o in &Self::ch_ops(c) { if self.op_prepare(o) { active = true; } }
                if active { self.active_channels |= 1 << c; }
            }
            self.modified_channels = 0;
            self.prepare_count = 0;
        }
        self.env_counter = self.env_counter.wrapping_add(1);
        if bf(self.env_counter, 0, 2) == 3 { self.env_counter = self.env_counter.wrapping_add(1); }
        let pm = self.clock_lfo();
        let ec = self.env_counter;
        for c in 0..6 {
            let ch = &mut self.chans[c];
            ch.feedback[0] = ch.feedback[1];
            ch.feedback[1] = ch.feedback_in;
            for &o in &Self::ch_ops(c) { self.op_clock(o, ec, pm); }
        }
    }

    /// One output sample (left, right), as ymfm's ym2612 produces it (about +-32K).
    pub fn generate(&mut self) -> (i32, i32) {
        self.clock_timers();
        self.engine_clock();
        let mut total = [0i32; 2];
        let last = if self.dac_enable { 5 } else { 6 };
        for c in 0..last {
            let mut out = [0i32; 2];
            if self.active_channels & (1 << c) != 0 { self.channel_output(c, &mut out); }
            total[0] += dac_discontinuity(out[0]);
            total[1] += dac_discontinuity(out[1]);
        }
        if self.dac_enable {
            let dac = dac_discontinuity(((self.dac_data << 7) as i16 >> 7) as i32);
            total[0] += if self.ch_output_0(0x102) { dac } else { dac_discontinuity(0) };
            total[1] += if self.ch_output_1(0x102) { dac } else { dac_discontinuity(0) };
        }
        ((total[0] * 128) * 64 / (6 * 65), (total[1] * 128) * 64 / (6 * 65))
    }

    fn clock_timers(&mut self) {
        for t in 0..2 {
            if !self.timer_running[t] { continue; }
            self.timer_left[t] -= 1;
            if self.timer_left[t] <= 0 { self.timer_expired(t); }
        }
    }

    fn timer_expired(&mut self, t: usize) {
        let mode = self.regs[0x27];
        if t == 0 && mode & 0x04 != 0 { self.status |= STATUS_TIMERA; }
        else if t == 1 && mode & 0x08 != 0 { self.status |= STATUS_TIMERB; }
        if t == 0 && self.csm() {
            // CSM: key on channel 3's operators
            for &o in &Self::ch_ops(2) { self.ops[o].keyon_live |= 1 << KEYON_CSM; }
            self.modified_channels |= 1 << 2;
        }
        self.timer_running[t] = false;
        self.update_timer(t, true, 0);
    }

    fn update_timer(&mut self, t: usize, enable: bool, delta: i64) {
        if enable && !self.timer_running[t] {
            let period = if t == 0 { 1024 - self.timer_a_value() } else { 16 * (256 - self.timer_b_value()) };
            self.timer_left[t] = (period + delta).max(1);
            self.timer_running[t] = true;
        } else if !enable {
            self.timer_running[t] = false;
        }
    }

    fn mode_write(&mut self, data: u8) {
        self.modified_channels = 0x3F;
        self.regs[0x27] = data;
        let mut reset = 0u8;
        if data & 0x20 != 0 { reset |= STATUS_TIMERB; }
        if data & 0x10 != 0 { reset |= STATUS_TIMERA; }
        self.status &= !reset;
        let d = -((self.total_clocks & 15) as i64);
        self.update_timer(1, data & 0x02 != 0, d);
        self.update_timer(0, data & 0x01 != 0, 0);
    }

    fn write_reg(&mut self, index: u16, data: u8) {
        if index == 0x27 { self.mode_write(data); return; }
        self.modified_channels = 0x3F;
        let i = index as usize;
        if index & 0xF0 == 0xA0 {
            if bf(index as u32, 0, 2) == 3 { return; }
            let latch = 0xB8 | bf(index as u32, 3, 1) as usize;
            if bf(index as u32, 2, 1) != 0 {
                self.regs[latch] = data & 0x3F;
            } else {
                self.regs[i] = data;
                self.regs[i | 4] = self.regs[latch];
            }
            return;
        }
        if index & 0xF8 == 0xB8 { return; }
        self.regs[i] = data;
        if index == 0x28 {
            let mut ch = bf(data as u32, 0, 2) as usize;
            if ch == 3 { return; }
            ch += bf(data as u32, 2, 1) as usize * 3;
            let mask = bf(data as u32, 4, 4);
            for (k, &o) in Self::ch_ops(ch).iter().enumerate() {
                let on = bf(mask, k as u32, 1) as u8;
                let op = &mut self.ops[o];
                op.keyon_live = (op.keyon_live & !(1 << KEYON_NORMAL)) | (on << KEYON_NORMAL);
            }
        }
    }

    /// Bus write: 0 address / 1 data (bank 0), 2 address / 3 data (bank 1).
    pub fn write(&mut self, offset: u8, data: u8) {
        match offset & 3 {
            0 => self.address = data as u16,
            1 => {
                if self.address & 0x100 != 0 { return; }
                match self.address {
                    0x2A => self.dac_data = (self.dac_data & !0x1FE) | (((data ^ 0x80) as u16) << 1),
                    0x2B => self.dac_enable = data & 0x80 != 0,
                    0x2C => self.dac_data = (self.dac_data & !1) | ((data >> 3) & 1) as u16,
                    a => self.write_reg(a, data),
                }
            }
            2 => self.address = 0x100 | data as u16,
            _ => {
                if self.address & 0x100 == 0 { return; }
                let a = self.address;
                self.write_reg(a, data);
            }
        }
    }

    /// Status: timer flags (never busy).
    pub fn status(&self) -> u8 { self.status }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Vectors from ymfm itself (tools/ym2612_ref): `w <port> <data>` writes, and
    /// `s <count> <hash>` = the next <count> samples hash (FNV-1a over l, r) to <hash>.
    fn run_vectors(text: &str) -> usize {
        let mut y = Ym2612::new();
        let mut n = 0;
        for (lineno, line) in text.lines().enumerate() {
            let p: Vec<&str> = line.split_whitespace().collect();
            match p.first() {
                Some(&"w") => {
                    let port = u8::from_str_radix(p[1], 16).unwrap();
                    let d = u8::from_str_radix(p[2], 16).unwrap();
                    y.write(port, d);
                }
                Some(&"s") => {
                    let count: usize = p[1].parse().unwrap();
                    let want = u64::from_str_radix(p[2], 16).unwrap();
                    let mut h: u64 = 0xcbf29ce484222325;
                    let mut last = (0, 0);
                    for _ in 0..count {
                        let (l, r) = y.generate();
                        last = (l, r);
                        for v in [l, r] { h = (h ^ (v as u32 as u64)).wrapping_mul(0x100000001b3); }
                    }
                    assert_eq!(h, want, "line {}: sample block differs (last {:?})", lineno + 1, last);
                    n += count;
                }
                _ => {}
            }
        }
        n
    }

    #[test]
    fn ym2612_matches_ymfm() {
        let n = run_vectors(include_str!("../tests/data/ym2612_vectors.txt"));
        assert!(n > 100_000);
    }
}
