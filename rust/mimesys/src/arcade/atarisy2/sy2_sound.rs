//! System 2 sound chips, as seen by the 6502 (MAME atarisy2.cpp sound section).
//!
//! POKEY 1 -> left, POKEY 2 -> right (route gain 1.35), each also reading a DIP bank
//! through ALLPOT. YM2151 at 14.318181 MHz / 4, stereo (0.60): one sample every
//! 32 sound-CPU cycles, busy for 32 cycles after a data write. TMS5220C on both
//! sides (0.75), clocked at 20 MHz / 4 / 4 / 2 (or / 3 / 2 when 0x187C bit 5 is set);
//! the 6502 writes a byte, pulls /WS low and waits for /READY (IN1 bit 2).
//! The mixer register (0x187A) sets the three volumes through resistor ladders.

use serde::{Deserialize, Serialize};
use crate::common::audio::pokey::Pokey;
use crate::common::audio::ym2151::Ym2151;
use crate::common::audio::tms5220::Tms5220;

const SND_CLOCK: f64 = 14_318_181.0 / 8.0;

/// MAME mixer_w: volume from which of the 22k / 47k / 100k resistors are switched in.
fn ladder(rbott: f64) -> f64 {
    let rtop = 1.0 / (1.0 / 100.0 + 1.0 / 100.0);
    if rbott == 0.0 { 1.0 } else { (1.0 / rbott) / (rtop + 1.0 / rbott) }
}

#[derive(Serialize, Deserialize)]
pub struct SoundChips {
    pub pokey: [Pokey; 2],
    pub ym: Ym2151,
    ym_phase: u32,          // sound cycles since the last YM2151 sample (32 per sample)
    pub(crate) ym_out: [f32; 2],
    ym_busy: u32,           // sound cycles left with the busy flag up
    pub tms: Tms5220,
    tms_phase: f64,         // TMS clocks owed (fractional)
    tms_out: f32,
    ym_gain: f64,
    pokey_gain: f64,
    tms_gain: f64,
    misc: u8,
    sound_reset_state: bool,
    step: f64,
    phase: f64,
    acc: [f32; 2],
    count: u32,
    dc_in: [f32; 2],
    dc_out: [f32; 2],
    #[serde(skip)]
    out: Vec<f32>,
}

impl SoundChips {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            pokey: [Pokey::new(), Pokey::new()],
            ym: Ym2151::new(), ym_phase: 0, ym_out: [0.0; 2], ym_busy: 0,
            tms: Tms5220::new(), tms_phase: 0.0, tms_out: 0.0,
            ym_gain: 1.0, pokey_gain: 1.0, tms_gain: 1.0,
            misc: 0, sound_reset_state: false,
            step: sample_rate as f64 / SND_CLOCK, phase: 0.0,
            acc: [0.0; 2], count: 0, dc_in: [0.0; 2], dc_out: [0.0; 2],
            out: Vec::with_capacity(4096),
        }
    }

    pub fn keep_output(&mut self, old: &mut Self) { self.out = std::mem::take(&mut old.out); }
    pub fn drain(&mut self) -> Vec<f32> { std::mem::take(&mut self.out) }

    /// Advance `cycles` sound clocks (= 6502 cycles = POKEY clocks), producing samples.
    pub fn run(&mut self, cycles: u32, dsw: [u8; 2]) {
        self.pokey[0].allpot_in = Some(dsw[0]);
        self.pokey[1].allpot_in = Some(dsw[1]);
        let g = (1.35 * self.pokey_gain) as f32;
        let gy = (0.60 * self.ym_gain / 32768.0) as f32;
        let gt = (0.75 * self.tms_gain / 32768.0) as f32;
        // MAME switch_6502_w: divider 4 or 3 from 20 MHz / 4, then / 2
        let tms_clock = 5_000_000.0 / (2.0 * if self.misc & 0x20 != 0 { 3.0 } else { 4.0 });
        let tms_step = tms_clock / SND_CLOCK;
        for _ in 0..cycles {
            self.tms_phase += tms_step;
            while self.tms_phase >= 1.0 {
                self.tms_phase -= 1.0;
                if let Some(s) = self.tms.clock() { self.tms_out = s as f32 * gt; }
            }
            self.pokey[0].step();
            self.pokey[1].step();
            self.ym_busy = self.ym_busy.saturating_sub(1);
            self.ym_phase += 1;
            if self.ym_phase == 32 {
                self.ym_phase = 0;
                let (l, r) = self.ym.generate();
                self.ym_out = [l as f32 * gy, r as f32 * gy];
            }
            self.acc[0] += self.pokey[0].output() * g + self.ym_out[0] + self.tms_out;
            self.acc[1] += self.pokey[1].output() * g + self.ym_out[1] + self.tms_out;
            self.count += 1;
            self.phase += self.step;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
                for side in 0..2 {
                    let x = self.acc[side] / self.count as f32;
                    // DC blocker: POKEY output is unipolar
                    let y = x - self.dc_in[side] + 0.995 * self.dc_out[side];
                    self.dc_in[side] = x;
                    self.dc_out[side] = y;
                    self.out.push((y * 0.8).clamp(-1.0, 1.0));
                }
                self.acc = [0.0; 2];
                self.count = 0;
            }
        }
    }

    /// Main CPU pulsed the sound reset: MAME calls sndrst_6502_w(0).
    pub fn sound_reset(&mut self) { self.sound_enable(false); }

    /// 0x187E (MAME sndrst_6502_w). The YM2151 is held in reset while this is 0;
    /// a 0 -> 1 edge resets the TMS5220 and clears the mixer.
    pub fn sound_enable(&mut self, on: bool) {
        if on == self.sound_reset_state { return; }
        self.sound_reset_state = on;
        self.ym.set_running(on);
        if on {
            self.tms.reset();
            self.mixer(0);
        }
    }

    pub fn pokey_read(&mut self, chip: usize, reg: u8, dsw: u8) -> u8 {
        self.pokey[chip].allpot_in = Some(dsw);
        self.pokey[chip].read(reg)
    }
    pub fn pokey_write(&mut self, chip: usize, reg: u8, v: u8) { self.pokey[chip].write(reg, v); }

    pub fn ym_read(&mut self, offset: u8) -> u8 {
        if offset & 1 == 0 { return 0xFF; }
        self.ym.status() | if self.ym_busy > 0 { 0x80 } else { 0 }
    }
    pub fn ym_write(&mut self, offset: u8, v: u8) {
        if !self.ym.running { return; }
        self.ym.write(offset, v);
        if offset & 1 != 0 { self.ym_busy = 32; }
    }

    pub fn tms_data(&mut self, v: u8) { self.tms.data_w(v); }
    /// /WS: 0x1872 drives it high, 0x1873 low (MAME tms5220_strobe_w).
    pub fn tms_wsq(&mut self, high: bool) { self.tms.wsq_w(high); }
    pub fn tms_ready(&self) -> bool { !self.tms.readyq() }

    /// 0x187A: bits 0-2 YM2151, 3-4 POKEYs, 5-7 TMS5220 (a 0 bit switches its resistor in).
    pub fn mixer(&mut self, v: u8) {
        let bit = |n: u8| v & (1 << n) == 0;
        let mut r = 0.0;
        if bit(0) { r += 1.0 / 100.0; }
        if bit(1) { r += 1.0 / 47.0; }
        if bit(2) { r += 1.0 / 22.0; }
        self.ym_gain = ladder(r);
        let mut r = 0.0;
        if bit(3) { r += 1.0 / 47.0; }
        if bit(4) { r += 1.0 / 22.0; }
        self.pokey_gain = ladder(r);
        let mut r = 0.0;
        if bit(5) { r += 1.0 / 100.0; }
        if bit(6) { r += 1.0 / 47.0; }
        if bit(7) { r += 1.0 / 22.0; }
        self.tms_gain = ladder(r);
    }

    /// 0x187C: LEDs, LETA resolution, TMS5220 clock select (bit 5).
    pub fn misc(&mut self, v: u8) { self.misc = v; }
}
