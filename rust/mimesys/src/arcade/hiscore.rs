// High score saving for boards without battery RAM, the way MAME's hiscore plugin does it
// (plugins/hiscore/init.lua, CC0). hiscore.dat lists, per set, the main CPU memory ranges
// that hold the score table:
//
//   dkong:
//   @:maincpu,program,6100,aa,94,76      address, length, first byte, last byte[, prefill]
//
// Once the game has written its default table (first and last bytes of every range match),
// the saved table is copied over it. After that, whenever the bytes change (and aren't the
// defaults again), the table is kept for saving.
//
// HiScoreSystem wraps any arcade System and presents the table as its battery save, so the
// frontend's existing .sav handling stores it.

use crate::common::system::{System, DisplayInfo};

#[derive(Clone, Debug, PartialEq)]
struct Row {
    addr: u16,
    size: usize,
    first: u8,
    last: u8,
    fill: Option<u8>,
}

#[derive(Clone, Debug)]
pub struct HiScore {
    rows: Vec<Row>,
    delay_frames: u32,
    frames: u32,
    table_found: bool,
    default_sum: u32,
    current_sum: u32,
    pending: Option<Vec<u8>>,
    snapshot: Vec<u8>,
    dirty: bool,
}

/// The parts of the main CPU's address space the table lives in.
pub trait HiScoreMem {
    fn hs_read(&mut self, addr: u16) -> u8;
    fn hs_write(&mut self, addr: u16, v: u8);
}

impl HiScore {
    /// Find `set`'s entry in hiscore.dat. None if the set isn't listed (or uses a CPU or
    /// address space other than the main CPU's program space).
    pub fn parse(dat: &str, set: &str) -> Option<Self> {
        let mut matched = false;
        let mut cluster: Vec<String> = Vec::new();
        for raw in dat.lines() {
            // strip comments (MAME: '[ \t\r\n]*;.+$')
            let line = match raw.find(';') { Some(i) => &raw[..i], None => raw }.trim_end();
            if line.starts_with('@') {
                if matched { cluster.push(line.to_string()); }
            } else if let Some(name) = line.strip_suffix(':') {
                let is_game = !name.is_empty()
                    && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == ',');
                if !is_game { continue; }
                if name == set {
                    matched = true;
                } else if matched && !cluster.is_empty() {
                    break;
                }
            }
        }
        if cluster.is_empty() { return None; }

        let mut rows = Vec::new();
        let mut delay_frames = 0;
        for line in &cluster {
            if let Some(d) = line.strip_prefix("@delay=") {
                delay_frames = (d.trim().parse::<f64>().unwrap_or(0.0) * 60.0) as u32;
                continue;
            }
            let f: Vec<&str> = line[1..].split(',').map(|s| s.trim()).collect();
            if f.len() < 6 { return None; }
            let cpu = f[0].trim_start_matches(':');
            if cpu != "maincpu" || f[1] != "program" {
                println!("hiscore: {set} uses {}:{}, not supported", f[0], f[1]);
                return None;
            }
            let hex = |s: &str| u32::from_str_radix(s, 16).ok();
            rows.push(Row {
                addr: hex(f[2])? as u16,
                size: hex(f[3])? as usize,
                first: hex(f[4])? as u8,
                last: hex(f[5])? as u8,
                fill: f.get(6).filter(|s| !s.is_empty()).and_then(|s| hex(s)).map(|v| v as u8),
            });
        }
        if rows.is_empty() || rows.iter().any(|r| r.size == 0) { return None; }
        Some(Self {
            rows, delay_frames, frames: 0, table_found: false, default_sum: 0, current_sum: 0,
            pending: None, snapshot: Vec::new(), dirty: false,
        })
    }

    pub fn total_len(&self) -> usize { self.rows.iter().map(|r| r.size).sum() }

    /// Machine (re)started: prefill and look for the default table again.
    pub fn reset(&mut self, mem: &mut dyn HiScoreMem) {
        for r in &self.rows {
            if let Some(v) = r.fill {
                for i in 0..r.size { mem.hs_write(r.addr.wrapping_add(i as u16), v); }
            }
        }
        if self.table_found && !self.snapshot.is_empty() {
            self.pending = Some(self.snapshot.clone());
        }
        self.table_found = false;
        self.frames = 0;
    }

    /// Saved table from disk, applied once the game has set up its own.
    pub fn load(&mut self, data: &[u8]) {
        if data.len() == self.total_len() {
            self.pending = Some(data.to_vec());
            self.snapshot = data.to_vec();
        } else {
            println!("hiscore: saved table is {} bytes, expected {}", data.len(), self.total_len());
        }
    }

    fn defaults_in_place(&self, mem: &mut dyn HiScoreMem) -> bool {
        self.rows.iter().all(|r| {
            mem.hs_read(r.addr) == r.first
                && mem.hs_read(r.addr.wrapping_add(r.size as u16 - 1)) == r.last
        })
    }

    fn read_table(&self, mem: &mut dyn HiScoreMem) -> Vec<u8> {
        let mut t = Vec::with_capacity(self.total_len());
        for r in &self.rows {
            for i in 0..r.size { t.push(mem.hs_read(r.addr.wrapping_add(i as u16))); }
        }
        t
    }

    fn sum(t: &[u8]) -> u32 { t.iter().map(|&b| b as u32).sum() }

    /// Once per frame.
    pub fn tick(&mut self, mem: &mut dyn HiScoreMem) {
        self.frames = self.frames.saturating_add(1);
        if !self.table_found {
            if self.frames < self.delay_frames || !self.defaults_in_place(mem) { return; }
            self.default_sum = Self::sum(&self.read_table(mem));
            if let Some(saved) = self.pending.take() {
                let mut i = 0;
                for r in self.rows.clone() {
                    for k in 0..r.size {
                        mem.hs_write(r.addr.wrapping_add(k as u16), saved[i]);
                        i += 1;
                    }
                }
                println!("hiscore: saved scores restored");
            }
            self.table_found = true;
            self.current_sum = Self::sum(&self.read_table(mem));
            return;
        }
        let t = self.read_table(mem);
        let s = Self::sum(&t);
        if s != self.current_sum && s != self.default_sum {
            self.current_sum = s;
            self.snapshot = t;
            self.dirty = true;
        }
    }
}

/// An arcade machine with hiscore.dat support; everything else passes straight through.
pub struct HiScoreSystem<S: System + HiScoreMem> {
    pub inner: S,
    hs: HiScore,
    save_path: String,
}

impl<S: System + HiScoreMem> HiScoreSystem<S> {
    pub fn new(inner: S, hs: HiScore, driver_dir: &str) -> Self {
        Self { inner, hs, save_path: format!("user://GD_EMU/Arcade/{driver_dir}/HiScore") }
    }
}

impl<S: System + HiScoreMem> System for HiScoreSystem<S> {
    fn run_frame(&mut self, input: u32) {
        self.inner.run_frame(input);
        self.hs.tick(&mut self.inner);
    }
    fn reset(&mut self) {
        self.inner.reset();
        self.hs.reset(&mut self.inner);
    }
    // the score table is the "battery" save
    fn has_battery(&self) -> bool { true }
    fn save_battery_path(&self) -> &str { &self.save_path }
    fn get_sram(&self) -> Option<&[u8]> {
        if self.hs.snapshot.is_empty() { None } else { Some(&self.hs.snapshot) }
    }
    fn load_sram(&mut self, data: &[u8]) { self.hs.load(data); }
    fn is_sram_dirty(&self) -> bool { self.hs.dirty }
    fn clear_sram_dirty(&mut self) { self.hs.dirty = false; }

    fn framebuffer(&self) -> &[u8] { self.inner.framebuffer() }
    fn display_info(&self) -> DisplayInfo { self.inner.display_info() }
    fn drain_audio(&mut self) -> Vec<f32> { self.inner.drain_audio() }
    fn sample_rate(&self) -> u32 { self.inner.sample_rate() }
    fn set_input(&mut self, input: u32) { self.inner.set_input(input) }
    fn save_state(&self) -> Vec<u8> { self.inner.save_state() }
    fn load_state(&mut self, data: &[u8]) -> Result<(), String> { self.inner.load_state(data) }
    fn save_filename(&self) -> &str { self.inner.save_filename() }
    fn save_state_path(&self) -> &str { self.inner.save_state_path() }
    fn power_on(&mut self) { self.inner.power_on() }
    fn power_off(&mut self) { self.inner.power_off() }
    fn get_current_frame(&self) -> i64 { self.inner.get_current_frame() }
    fn set_audio_rate_adjust(&mut self, f: f32) { self.inner.set_audio_rate_adjust(f) }
    fn set_option(&mut self, key: &str, value: i64) { self.inner.set_option(key, value) }
    fn set_pointer(&mut self, x: i32, y: i32, b: u32) { self.inner.set_pointer(x, y, b) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAT: &str = "\
; comment
dkongjr:
@:maincpu,program,6100,aa,94,76

dkong:
dkongo:   ; a clone sharing the entry
@:maincpu,program,6100,4,94,76
@:maincpu,program,60b8,03,50,00

galaxian:
@:maincpu,program,40a8,3,00,00,ff
";

    struct Mem([u8; 0x10000]);
    impl HiScoreMem for Mem {
        fn hs_read(&mut self, a: u16) -> u8 { self.0[a as usize] }
        fn hs_write(&mut self, a: u16, v: u8) { self.0[a as usize] = v; }
    }

    #[test]
    fn parses_clusters() {
        let h = HiScore::parse(DAT, "dkongo").unwrap();
        assert_eq!(h.rows.len(), 2);
        assert_eq!(h.total_len(), 7);
        assert_eq!(HiScore::parse(DAT, "dkong").unwrap().rows, h.rows);
        assert_eq!(HiScore::parse(DAT, "galaxian").unwrap().rows[0].fill, Some(0xFF));
        assert!(HiScore::parse(DAT, "pacman").is_none());
    }

    #[test]
    fn restores_after_defaults_appear_and_tracks_changes() {
        let mut h = HiScore::parse(DAT, "dkong").unwrap();
        h.load(&[0x94, 9, 9, 0x76, 0x50, 7, 0]);
        let mut m = Mem([0; 0x10000]);
        h.tick(&mut m);
        assert_eq!(m.0[0x6101], 0, "nothing written before the game sets up its table");
        // game writes its defaults
        m.0[0x6100] = 0x94; m.0[0x6103] = 0x76; m.0[0x60B8] = 0x50; m.0[0x60BA] = 0;
        h.tick(&mut m);
        assert_eq!(&m.0[0x6100..0x6104], &[0x94, 9, 9, 0x76]);
        assert_eq!(m.0[0x60B9], 7);
        assert!(!h.dirty);
        // a new score
        m.0[0x6101] = 0x20;
        h.tick(&mut m);
        assert!(h.dirty);
        assert_eq!(h.snapshot[1], 0x20);
    }
}
