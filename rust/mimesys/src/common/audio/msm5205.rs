// OKI MSM5205 ADPCM decoder, after MAME's msm5205.cpp (Aaron Giles).
//
// The chip divides its master clock (usually 384 kHz) by 96 / 64 / 48 to make VCK, the
// sample clock (4 / 6 / 8 kHz), and tells the CPU about it (typically on NMI). The CPU
// writes the next 4-bit nibble; the chip latches it 6 master clocks after VCK falls and
// steps its 12-bit signal by the Dialogic ADPCM table. S1 = S2 = 1 is "slave" mode:
// VCK becomes an input (another chip or the board drives it).
// Reset held high silences the output and resets the step.

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Msm5205 {
    #[serde(skip, default = "diff_table")]
    diff: [i32; 49 * 16],
    data: u8,
    vck: bool,
    reset: bool,
    pub signal: i32,
    step: i32,
    prescaler: u32,      // 0 = slave
    bits4: bool,
    count: u32,          // master clocks into the current VCK half period
    capture_in: i32,     // master clocks until the data latch, -1 = none
}

const INDEX_SHIFT: [i32; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];

impl Msm5205 {
    /// `playmode`: bit 0 S1, bit 1 S2, bit 2 = 4-bit data (MAME playmode_w / S96_4B = 4).
    pub fn new(playmode: u8) -> Self {
        let diff = diff_table();
        let mut m = Self {
            diff, data: 0, vck: false, reset: false, signal: 0, step: 0,
            prescaler: 96, bits4: true, count: 0, capture_in: -1,
        };
        m.playmode(playmode);
        m
    }
}

/// Step x nibble difference table (MAME compute_tables).
fn diff_table() -> [i32; 49 * 16] {
    let mut diff = [0i32; 49 * 16];
    const NBL2BIT: [[i32; 4]; 16] = [
        [1, 0, 0, 0], [1, 0, 0, 1], [1, 0, 1, 0], [1, 0, 1, 1],
        [1, 1, 0, 0], [1, 1, 0, 1], [1, 1, 1, 0], [1, 1, 1, 1],
        [-1, 0, 0, 0], [-1, 0, 0, 1], [-1, 0, 1, 0], [-1, 0, 1, 1],
        [-1, 1, 0, 0], [-1, 1, 0, 1], [-1, 1, 1, 0], [-1, 1, 1, 1],
    ];
    for step in 0..=48 {
        let stepval = (16.0 * 1.1f64.powi(step)).floor() as i32;
        for nib in 0..16 {
            let n = NBL2BIT[nib];
            diff[step as usize * 16 + nib] =
                n[0] * (stepval * n[1] + stepval / 2 * n[2] + stepval / 4 * n[3] + stepval / 8);
        }
    }
    diff
}

impl Msm5205 {
    pub fn playmode(&mut self, data: u8) {
        let s1 = data & 1 != 0;
        let s2 = data & 2 != 0;
        self.prescaler = match (s1, s2) { (true, true) => 0, (true, false) => 48, (false, true) => 64, _ => 96 };
        self.bits4 = data & 4 != 0;
    }

    pub fn set_reset(&mut self, on: bool) { self.reset = on; }

    pub fn data_w(&mut self, v: u8) {
        self.data = if self.bits4 { v & 0x0F } else { (v & 0x07) << 1 };
    }

    /// Slave mode: VCK driven from outside. Data is latched 6 clocks after it falls.
    pub fn vclk_w(&mut self, state: bool) {
        if self.prescaler != 0 { return; }
        if self.vck && !state { self.capture_in = 6; }
        self.vck = state;
    }

    /// One master clock. Returns Some(new VCK level) when this chip toggles its VCK.
    pub fn clock(&mut self) -> Option<bool> { self.clock_with(|_| {}) }

    /// Like `clock`, but calls `capture` at the moment the chip latches its data
    /// (MAME's vck_legacy_callback): the board can feed the next nibble or reset the
    /// chip right there, and the new data is used for this very sample.
    pub fn clock_with(&mut self, mut capture: impl FnMut(&mut Self)) -> Option<bool> {
        let mut edge = None;
        if self.prescaler != 0 {
            self.count += 1;
            if self.count >= self.prescaler / 2 {
                self.count = 0;
                self.vck = !self.vck;
                if !self.vck { self.capture_in = 6; }
                edge = Some(self.vck);
            }
        }
        if self.capture_in > 0 {
            self.capture_in -= 1;
            if self.capture_in == 0 {
                self.capture_in = -1;
                capture(self);
                self.update();
            }
        }
        edge
    }

    fn update(&mut self) {
        if self.reset {
            self.signal = 0;
            self.step = 0;
            return;
        }
        let v = (self.data & 15) as usize;
        self.signal = (self.signal + self.diff[self.step as usize * 16 + v]).clamp(-2048, 2047);
        self.step = (self.step + INDEX_SHIFT[v & 7]).clamp(0, 48);
    }

    /// Output in -0.5..0.5 (10-bit DAC: the low 2 bits of the 12-bit signal are dropped).
    pub fn output(&self) -> f32 {
        (self.signal & !3) as f32 / 4096.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn master_mode_makes_4khz_vck_and_decodes() {
        let mut m = Msm5205::new(4);   // /96, 4-bit
        m.data_w(0x7);                 // big positive step
        let mut rising = 0;
        for _ in 0..384_000 / 10 {
            if m.clock() == Some(true) { rising += 1; }
        }
        assert_eq!(rising, 400);        // 4 kHz for 0.1 s
        assert!(m.signal > 0);
        m.set_reset(true);
        for _ in 0..200 { m.clock(); }
        assert_eq!(m.signal, 0);
    }
}
