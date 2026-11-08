//! Flicker blend: mixes each emulated frame with the last one, the way a CRT's phosphor and
//! the eye did. Games that show sprites on alternate frames (Batman, many NES / Game Gear /
//! Game Boy games) meant them to look solid and see-through, not flashing.
//!
//! The frontend may ask for the picture more often than frames are made (a 120 Hz screen),
//! so a new frame is noticed by its pixels changing; until then the last result is reused.

pub const OFF: u8 = 0;
/// 50/50 mix of this frame and the last: alternate-frame flicker becomes steady.
pub const BLEND: u8 = 1;
/// Bright pixels fade over a frame or two instead of vanishing (a CRT's glow).
pub const PHOSPHOR: u8 = 2;

/// How much of last frame's light is left in the next one (phosphor mode).
const DECAY: u32 = 160; // of 256

#[derive(Default)]
pub struct FrameBlend {
    mode: u8,
    /// the newest frame seen (to spot the next one)
    last: Vec<u8>,
    /// the frame before it (blend mode)
    prev: Vec<u8>,
    /// what was shown (phosphor mode feeds it back)
    out: Vec<u8>,
}

impl FrameBlend {
    pub fn set_mode(&mut self, mode: u8) {
        if mode != self.mode {
            self.mode = mode;
            self.last.clear();
        }
    }

    pub fn mode(&self) -> u8 { self.mode }

    /// The picture to show for the frame `cur` (RGBA bytes).
    pub fn apply<'a>(&'a mut self, cur: &'a [u8]) -> &'a [u8] {
        if self.mode == OFF { return cur; }
        if self.last.len() != cur.len() {
            // first frame, or the picture changed size: nothing to mix with yet
            self.last = cur.to_vec();
            self.prev = cur.to_vec();
            self.out = cur.to_vec();
            return &self.out;
        }
        if self.last.as_slice() == cur { return &self.out; } // the same frame again
        std::mem::swap(&mut self.prev, &mut self.last);
        self.last.copy_from_slice(cur);
        match self.mode {
            BLEND => {
                for ((o, &a), &b) in self.out.iter_mut().zip(cur).zip(&self.prev) {
                    *o = ((a as u16 + b as u16 + 1) >> 1) as u8;
                }
            }
            _ => {
                for (o, &a) in self.out.iter_mut().zip(cur) {
                    let fade = (*o as u32 * DECAY >> 8) as u8;
                    *o = a.max(fade);
                }
            }
        }
        // alpha stays opaque
        for px in self.out.chunks_exact_mut(4) { px[3] = 255; }
        &self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(v: u8) -> Vec<u8> { vec![v, v, v, 255, 0, 0, 0, 255] }

    #[test]
    fn blend_makes_alternating_frames_steady() {
        let mut f = FrameBlend::default();
        f.set_mode(BLEND);
        let (on, off) = (frame(200), frame(0));
        f.apply(&on);
        let shown: Vec<u8> = (0..6).map(|i| f.apply(if i % 2 == 0 { &off } else { &on })[0]).collect();
        assert_eq!(shown, vec![100; 6], "a sprite on every other frame shows steadily at half strength");
        // asked twice for the same frame: the same picture, nothing advances
        assert_eq!(f.apply(&on)[0], 100);
        assert_eq!(f.apply(&on)[0], 100);
        assert_eq!(f.apply(&off)[0], 100);
        assert_eq!(f.apply(&off)[0], 100);
    }

    #[test]
    fn phosphor_fades() {
        let mut f = FrameBlend::default();
        f.set_mode(PHOSPHOR);
        f.apply(&frame(250));
        let a = f.apply(&frame(0))[0];
        let b = f.apply(&frame(1))[0];
        assert!(a > 100 && a < 250, "{a}: dimmer, not gone");
        assert!(b < a, "and dimmer again");
        assert_eq!(f.apply(&frame(255))[0], 255, "new light shows at once");
    }

    #[test]
    fn off_and_size_changes() {
        let mut f = FrameBlend::default();
        assert_eq!(f.apply(&frame(7))[0], 7);
        f.set_mode(BLEND);
        assert_eq!(f.apply(&[9, 9, 9, 255]), &[9, 9, 9, 255]);
        assert_eq!(f.apply(&frame(50))[0], 50, "a new size starts fresh");
    }
}
