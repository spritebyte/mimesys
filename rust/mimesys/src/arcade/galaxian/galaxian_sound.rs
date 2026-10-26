// Galaxian / Moon Cresta sound: all analog circuits (no sound chip), synthesized here
// behaviorally from the schematic values MAME uses in its discrete model.
//
// Inputs from the CPU:
//   pitch (7800 / B800)   melody: a counter reloaded with `pitch`, clocked at 1.536 MHz,
//                         feeding a 4-bit counter. Bits 0, 2 and 3 go to the mixer.
//                         pitch = 0xFF puts it far above hearing (= silence).
//   LFO (6004-6007)       4-bit DAC that sets how fast the "swarm" warble sweeps
//   FS1-FS3 (6800-6802)   three 555 oscillators (the background hum), swept by the LFO
//   HIT (6803)            explosion: noise through a band-pass filter, slow decay
//   FIRE (6805)           player shot: a 555 whose pitch rises as a capacitor discharges
//   VOL1, VOL2 (6806-07)  switch two extra resistors into the melody mix
//
// Reference: MAME src/mame/galaxian/galaxian_a.cpp. MAME runs a full circuit simulation;
// this is an approximation tuned to the same component values. Gains are by ear.

const SOUND_CLOCK: f64 = 1_536_000.0;   // 18.432 MHz / 12
const NOISE_SAMPLE_HZ: f64 = 60.0 * 264.0 / 2.0;   // noise is latched on the 2V line signal

#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Mixing { Galaxian, MoonCresta }

#[derive(serde::Serialize, serde::Deserialize)]
pub struct GalaxianSound {
    rate: f64,
    mixing: Mixing,

    // register state
    pub pitch: u8,
    lfo_val: u8,
    fs: [bool; 3],
    hit: bool,
    fire: bool,
    vol: [bool; 2],

    // melody: count position in the 1.536 MHz -> /(256-pitch) -> /16 chain
    tone_phase: f64,

    // background
    lfo_phase: f64,
    osc_phase: [f64; 3],

    // noise (17-bit LFSR, sampled at NOISE_SAMPLE_HZ)
    lfsr: u32,
    noise_phase: f64,
    noise_out: f64,

    // hit: envelope + band-pass biquad
    hit_env: f64,
    bp: Biquad,

    // fire
    fire_cap: f64,      // node 173: 4 V while idle, discharges while firing
    fire_env: f64,      // output gate (RC discharge)
    fire_phase: f64,

    dc: (f64, f64),
    #[serde(skip)]
    pub out: Vec<f32>,
}

impl GalaxianSound {
    pub fn new(rate: u32, mixing: Mixing) -> Self {
        let rate = rate as f64;
        Self {
            rate,
            mixing,
            pitch: 0xFF,
            lfo_val: 0,
            fs: [false; 3],
            hit: false,
            fire: false,
            vol: [false; 2],
            tone_phase: 0.0,
            lfo_phase: 0.0,
            osc_phase: [0.0; 3],
            lfsr: 1,
            noise_phase: 0.0,
            noise_out: 0.0,
            hit_env: 0.0,
            // GAL_R35/R36/R37, C22/C23: centre ~167 Hz, Q ~2.5
            bp: Biquad::band_pass(167.0, 2.5, rate),
            fire_cap: 4.0,
            fire_env: 0.0,
            fire_phase: 0.0,
            dc: (0.0, 0.0),
            out: Vec::with_capacity(2048),
        }
    }

    /// 6800-6807 (B800... on Moon Cresta it's A800-A807): one latch bit per address.
    pub fn sound_w(&mut self, offset: u8, data: u8) {
        let on = data & 1 != 0;
        match offset & 7 {
            0..=2 => self.fs[(offset & 7) as usize] = on,
            3 => self.hit = on,
            5 => self.fire = on,
            6 => self.vol[0] = on,
            7 => self.vol[1] = on,
            _ => {}
        }
    }

    /// 6004-6007: one bit of the LFO DAC per address.
    pub fn lfo_w(&mut self, offset: u8, data: u8) {
        let bit = offset & 3;
        self.lfo_val = (self.lfo_val & !(1 << bit)) | ((data & 1) << bit);
    }

    /// Produce `n` output samples (stereo pairs pushed to `out`).
    pub fn render(&mut self, n: usize) {
        let dt = 1.0 / self.rate;
        for _ in 0..n {
            // ---- melody: 4-bit counter, QA/QC/QD to the mixer ----
            let counter_hz = SOUND_CLOCK / (256.0 - self.pitch as f64);   // clocks of the 4-bit counter
            let steps = counter_hz * dt;
            // bit k is a square wave at counter_hz / 2^(k+1). Above Nyquist it can't be
            // represented, so use its average (2 V) instead; the DC filter removes it.
            let c = self.tone_phase as u32 & 15;
            let nyquist = self.rate / 2.0;
            let b = |bit: u32| {
                if counter_hz / (2u32 << bit) as f64 > nyquist { 2.0 }
                else if c >> bit & 1 != 0 { 4.0 } else { 0.0 }
            };
            let (qa, qc, qd) = (b(0), b(2), b(3));
            self.tone_phase = (self.tone_phase + steps) % 16.0;

            // ---- background: LFO sawtooth sweeps three 555 oscillators ----
            // DAC 0-15 -> roughly 0.7 to 5 sweeps per second (approximation)
            let lfo_hz = 0.7 + self.lfo_val as f64 * 0.28;
            self.lfo_phase = (self.lfo_phase + lfo_hz * dt) % 1.0;
            let sweep = 1.0 + 0.5 * self.lfo_phase;          // higher CV ramp -> lower pitch, reset at wrap
            let base = [138.0, 189.0, 267.0];                // 555s at 8R, 8S, 8T (R22/R23/C17 ...)
            let mut bg = 0.0;
            for i in 0..3 {
                if self.fs[i] {
                    self.osc_phase[i] = (self.osc_phase[i] + base[i] / sweep * dt) % 1.0;
                    bg += if self.osc_phase[i] < 0.5 { 4.5 } else { 0.0 };
                }
            }
            bg /= 3.0;                                       // R24/R27/R30 equal mixer

            // ---- noise ----
            self.noise_phase += NOISE_SAMPLE_HZ * dt;
            while self.noise_phase >= 1.0 {
                self.noise_phase -= 1.0;
                let bit = ((self.lfsr >> 4) ^ !(self.lfsr >> 16)) & 1;
                self.lfsr = ((self.lfsr << 1) | bit) & 0x1FFFF;
                self.noise_out = if bit != 0 { 4.0 } else { 0.0 };
            }

            // ---- hit: noise gated by an RC envelope (R35+R36, C21: tau 0.38 s), band-passed ----
            if self.hit { self.hit_env = 1.0; } else { self.hit_env *= (-dt / 0.378).exp(); }
            let hit = self.bp.process((self.noise_out - 2.0) * self.hit_env) * 3.0;

            // ---- fire: node 173 discharges through R47/C28 (tau 0.10 s) while firing ----
            let tau173 = 2.2e3 * 47e-6;
            let target = if self.fire { 0.0 } else { 4.0 };
            self.fire_cap += (target - self.fire_cap) * (1.0 - (-dt / tau173).exp());
            // control voltage: noise through R46, ramp through R48
            let cv = (self.noise_out / 10e3 + self.fire_cap / 2.2e3) * (10e3 * 2.2e3 / 12.2e3);
            let f = fire_555_freq(cv.clamp(0.3, 4.9));
            self.fire_phase = (self.fire_phase + f * dt) % 1.0;
            let sq = if self.fire_phase < 0.5 { 4.0 } else { 0.0 };
            if self.fire { self.fire_env = 1.0; } else { self.fire_env *= (-dt / 0.1).exp(); }
            let fire = sq * self.fire_env;

            // ---- resistor mixers (conductance-weighted average) ----
            // a switched-off resistor (VOL1/VOL2 = 0) is an open circuit: conductance 0
            let g_vol1 = if self.vol[0] { 1.0 / 10e3 } else { 0.0 };
            let g_vol2 = if self.vol[1] { 1.0 / 15e3 } else { 0.0 };
            let melody = [(qa, 1.0 / 33e3), (qc, 1.0 / 22e3), (qc, g_vol1), (qd, g_vol2)];
            let v = match self.mixing {
                Mixing::Galaxian => {
                    let pre = mix(&[melody[0], melody[1], melody[2], melody[3], (bg, 1.0 / 5.1e3)]);
                    mix(&[(pre, 1.0 / 5.1e3), (hit + 2.0, 1.0 / 1.32e3), (fire, 1.0 / 2.2e3)])
                }
                Mixing::MoonCresta => mix(&[melody[0], melody[1], melody[2], melody[3],
                    (bg, 1.0 / 15e3), (hit + 2.0, 1.0 / 1.32e3), (fire, 1.0 / 2.2e3)]),
            };

            // output coupling capacitor -> remove DC
            let (x_prev, y_prev) = self.dc;
            let y = v - x_prev + 0.995 * y_prev;
            self.dc = (v, y);
            let s = (y * 0.35) as f32;
            self.out.push(s);
            self.out.push(s);
        }
    }
}

fn mix(ins: &[(f64, f64)]) -> f64 {
    let g: f64 = ins.iter().map(|i| i.1).sum();
    ins.iter().map(|i| i.0 * i.1).sum::<f64>() / g
}

/// 555 astable with R44 = 10k, R45 = 22k, C27 = 0.01 uF and control voltage `vc`.
fn fire_555_freq(vc: f64) -> f64 {
    let (r1, r2, c, vcc) = (10e3, 22e3, 0.01e-6, 5.0);
    let t_high = (r1 + r2) * c * ((vcc - vc / 2.0) / (vcc - vc)).ln();
    let t_low = r2 * c * 2f64.ln();
    1.0 / (t_high + t_low)
}

/// RBJ band-pass biquad.
#[derive(serde::Serialize, serde::Deserialize)]
struct Biquad { b0: f64, b2: f64, a1: f64, a2: f64, x1: f64, x2: f64, y1: f64, y2: f64 }

impl Biquad {
    fn band_pass(f0: f64, q: f64, rate: f64) -> Self {
        let w = 2.0 * std::f64::consts::PI * f0 / rate;
        let alpha = w.sin() / (2.0 * q);
        let a0 = 1.0 + alpha;
        Self { b0: alpha / a0, b2: -alpha / a0, a1: -2.0 * w.cos() / a0, a2: (1.0 - alpha) / a0,
               x1: 0.0, x2: 0.0, y1: 0.0, y2: 0.0 }
    }
    fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.b2 * self.x2 - self.a1 * self.y1 - self.a2 * self.y2;
        self.x2 = self.x1; self.x1 = x;
        self.y2 = self.y1; self.y1 = y;
        y
    }
}

#[cfg(test)]
pub(crate) fn fire_freq_for_test(vc: f64) -> f64 { fire_555_freq(vc) }
