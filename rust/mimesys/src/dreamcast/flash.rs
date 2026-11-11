//! The Dreamcast's 128 KB system flash (the top half of a Fujitsu MBM29LV002TC, at
//! 00200000): console settings, region, clock offset. AMD-style commands: unlock with AA at
//! 555 and 55 at 2AA, then A0 = program a byte (bits can only go 0), 80 + unlock + 30 =
//! erase a sector, 80 + unlock + 10 = erase all, 90 = ID mode, F0 = back to reading.

use serde::{Deserialize, Serialize};

pub const SIZE: usize = 0x20000;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
enum State { Read, Unlock1, Unlock2, Program, Erase1, Erase2, Erase3, Id }

#[derive(Clone, Serialize, Deserialize)]
pub struct Flash {
    pub data: Vec<u8>,
    state: State,
    pub dirty: bool,
    /// TV standard override (set_broadcast), re-applied when a save file replaces the data
    #[serde(default)]
    pub broadcast: Option<u8>,
}

/// Sectors of the 128 KB window (the chip's top-boot layout from 0x20000): start, size.
const SECTORS: [(usize, usize); 5] = [(0x00000, 0x10000), (0x10000, 0x8000), (0x18000, 0x2000), (0x1A000, 0x2000), (0x1C000, 0x4000)];

impl Flash {
    /// Factory settings (1A000: "0" + region + broadcast + ...; a copy at 1A0A0): replace the
    /// broadcast digit (0 NTSC, 1 PAL, 2 PAL-M, 3 PAL-N). The file isn't marked dirty.
    pub fn set_broadcast(&mut self, b: u8) {
        if b > 3 { return; }
        self.broadcast = Some(b);
        self.apply_broadcast();
        crate::dlog!("Dreamcast: broadcast standard set to {}", ["NTSC", "PAL", "PAL-M", "PAL-N"][b as usize]);
    }

    /// Write the override (if any) over the factory bytes.
    pub fn apply_broadcast(&mut self) {
        let Some(b) = self.broadcast else { return };
        for at in [0x1A000, 0x1A0A0] {
            if self.data[at + 4].is_ascii_digit() { self.data[at + 4] = b'0' + b; }
        }
    }

    pub fn new(data: &[u8]) -> Self {
        let mut d = vec![0xFF; SIZE];
        let n = data.len().min(SIZE);
        d[..n].copy_from_slice(&data[..n]);
        Self { data: d, state: State::Read, dirty: false, broadcast: None }
    }

    pub fn read(&self, off: u32) -> u8 {
        let off = off as usize & (SIZE - 1);
        if self.state == State::Id {
            return match off & 3 { 0 => 0x04, 1 => 0x40, _ => 0 }; // Fujitsu, 29LV002TC
        }
        self.data[off]
    }

    pub fn write(&mut self, off: u32, v: u8) {
        let o = off as usize & (SIZE - 1);
        let (a, b) = (o & 0x7FF == 0x555, o & 0x7FF == 0x2AA);
        if v == 0xF0 && self.state != State::Program { self.state = State::Read; return; }
        self.state = match (self.state, v) {
            (State::Read | State::Id, 0xAA) if a => State::Unlock1,
            (State::Unlock1, 0x55) if b => State::Unlock2,
            (State::Unlock2, 0xA0) if a => State::Program,
            (State::Unlock2, 0x80) if a => State::Erase1,
            (State::Unlock2, 0x90) if a => State::Id,
            (State::Program, _) => {
                self.data[o] &= v;
                self.dirty = true;
                State::Read
            }
            (State::Erase1, 0xAA) if a => State::Erase2,
            (State::Erase2, 0x55) if b => State::Erase3,
            (State::Erase3, 0x30) => {
                if let Some(&(s, n)) = SECTORS.iter().find(|(s, n)| (*s..*s + *n).contains(&o)) {
                    self.data[s..s + n].fill(0xFF);
                }
                self.dirty = true;
                State::Read
            }
            (State::Erase3, 0x10) if a => { self.data.fill(0xFF); self.dirty = true; State::Read }
            (State::Id, _) => State::Id,
            _ => State::Read,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unlock(f: &mut Flash) { f.write(0x5555, 0xAA); f.write(0x2AAA, 0x55); }

    #[test]
    fn program_erase_and_id() {
        let mut f = Flash::new(&[]);
        unlock(&mut f); f.write(0x5555, 0xA0); f.write(0x1A002, 0x31);
        assert_eq!(f.read(0x1A002), 0x31);
        f.write(0x1A002, 0x00);
        assert_eq!(f.read(0x1A002), 0x31, "a plain write doesn't program");
        unlock(&mut f); f.write(0x555, 0xA0); f.write(0x1A002, 0xF0);
        assert_eq!(f.read(0x1A002), 0x30, "bits only clear");
        assert!(f.dirty);
        unlock(&mut f); f.write(0x5555, 0x80); unlock(&mut f); f.write(0x1A000, 0x30);
        assert_eq!(f.read(0x1A002), 0xFF, "sector 1A000-1BFFF erased");
        unlock(&mut f); f.write(0x5555, 0x90);
        assert_eq!((f.read(0), f.read(1)), (0x04, 0x40));
        f.write(0, 0xF0);
        assert_eq!(f.read(0), 0xFF);
    }

    #[test]
    fn broadcast_override_patches_both_factory_copies() {
        let mut d = vec![0xFF; SIZE];
        d[0x1A000..0x1A005].copy_from_slice(b"00111");
        d[0x1A0A0..0x1A0A5].copy_from_slice(b"00111");
        let mut f = Flash::new(&d);
        f.set_broadcast(0);
        assert_eq!(&f.data[0x1A000..0x1A005], b"00110");
        assert_eq!(&f.data[0x1A0A0..0x1A0A5], b"00110");
        assert!(!f.dirty);
        // a save file loaded later gets the override again
        f.data[0x1A004] = b'1';
        f.apply_broadcast();
        assert_eq!(f.data[0x1A004], b'0');
        // a blank flash stays blank
        let mut f = Flash::new(&[]);
        f.set_broadcast(1);
        assert_eq!(f.data[0x1A004], 0xFF);
    }
}
