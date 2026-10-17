pub const WSG_CLOCK: f64 = 96_000.0;

#[derive(Clone, Copy, Default)]
struct Voice {
    counter: u32,
    frequency: u32,
    waveform: u8,
    volume: u8,
}

pub struct NamcoWsg {
    regs: [u8; 32],
    voices: [Voice; 3],
    wave_prom: Vec<u8>,
    pub enabled: bool,

    out_rate: f64,
    phase: f64,
    acc: f32,
    acc_n: u32,
    pub out: Vec<f32>,
}

impl NamcoWsg {
    pub fn new(wave_prom: &[u8], out_rate: u32) -> Self {
        let mut prom = wave_prom.to_vec();
        prom.resize(0x100, 0);
        Self {
            regs: [0; 32],
            voices: [Voice::default(); 3],
            wave_prom: prom,
            enabled: false,
            out_rate: out_rate as f64,
            phase: 0.0,
            acc: 0.0,
            acc_n: 0,
            out: Vec::with_capacity(2048),
        }
    }

    pub fn write(&mut self, offset: u8, data: u8) {
        let r = (offset & 0x1F) as usize;
        self.regs[r] = data & 0x0F;
        let g = |i: usize| self.regs[i] as u32;
        match r {
            0x05 => self.voices[0].waveform = self.regs[r] & 7,
            0x0A => self.voices[1].waveform = self.regs[r] & 7,
            0x0F => self.voices[2].waveform = self.regs[r] & 7,
            0x10..=0x14 => {
                self.voices[0].frequency =
                    g(0x10) | g(0x11) << 4 | g(0x12) << 8 | g(0x13) << 12 | g(0x14) << 16;
            }
            0x16..=0x19 => {
                self.voices[1].frequency =
                    g(0x16) << 4 | g(0x17) << 8 | g(0x18) << 12 | g(0x19) << 16;
            }
            0x1B..=0x1E => {
                self.voices[2].frequency =
                    g(0x1B) << 4 | g(0x1C) << 8 | g(0x1D) << 12 | g(0x1E) << 16;
            }
            0x15 => self.voices[0].volume = self.regs[r],
            0x1A => self.voices[1].volume = self.regs[r],
            0x1F => self.voices[2].volume = self.regs[r],
            _ => {}
        }
    }

    pub fn tick(&mut self) {
        let mut sum = 0i32;
        if self.enabled {
            for v in self.voices.iter_mut() {
                if v.volume == 0 { continue; }
                let step = (v.counter >> 15) & 0x1F;
                let sample = (self.wave_prom[((v.waveform as u32) << 5 | step) as usize] & 0x0F) as i32 - 8;
                sum += sample * v.volume as i32;
                v.counter = (v.counter + v.frequency) & 0xFFFFF;
            }
        }
        self.acc += sum as f32 / 400.0;
        self.acc_n += 1;

        self.phase += self.out_rate;
        if self.phase >= WSG_CLOCK {
            self.phase -= WSG_CLOCK;
            let s = self.acc / self.acc_n as f32;
            self.acc = 0.0;
            self.acc_n = 0;
            self.out.push(s);
            self.out.push(s);
        }
    }
}