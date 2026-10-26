//! Atari Slapstic (137412-1xx): copy protection that bank-switches a ROM window
//! when the CPU touches magic addresses in sequence. Port of MAME's slapstic.cpp
//! state machine for the 103-110 family (only 105 is wired up: Paperboy,
//! Indiana Jones).
//!
//! The chip watches every bus access. On System 2 its window is 0x8000-0x81FF,
//! and the bank it selects picks what appears at 0x2000-0x3FFF (the "vmmu" view:
//! bank 0 = alphanumerics + motion objects, 2 / 3 = playfield halves, 1 = nothing).

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy)]
struct Mv { mask: u16, value: u16 }

struct ChipData {
    bankstart: u8,
    bank: [u16; 4],
    alt1: Mv, alt2: Mv, alt3: Mv, alt4: Mv,
    altshift: u32,
    bit1: Mv, bit2: Mv, bit3c0: Mv, bit3s0: Mv, bit3c1: Mv, bit3s1: Mv, bit4: Mv,
}

const fn mv(mask: u16, value: u16) -> Mv { Mv { mask, value } }

/// 137412-105: Indiana Jones / Paperboy
const SLAPSTIC_105: ChipData = ChipData {
    bankstart: 3,
    bank: [0x0010, 0x0014, 0x0018, 0x001C],
    alt1: mv(0x007F, 0x003D), alt2: mv(0x3FFF, 0x0092), alt3: mv(0x3FFC, 0x00A4), alt4: mv(0x3FF3, 0x0010),
    altshift: 0,
    bit1: mv(0x3FF0, 0x35B0), bit2: mv(0x3FF3, 0x0010),
    bit3c0: mv(0x3FF3, 0x35B0), bit3s0: mv(0x3FF3, 0x35B1), bit3c1: mv(0x3FF3, 0x35B2), bit3s1: mv(0x3FF3, 0x35B3),
    bit4: mv(0x3FF8, 0x35C0),
};

/// An address test: (addr & m) == v
#[derive(Clone, Copy)]
struct Test { m: u32, v: u32 }
impl Test {
    fn hit(&self, a: u32) -> bool { a & self.m == self.v }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
enum St { Idle, Active, AltValid, AltSelect, AltCommit, BitLoad, BitSetOdd, BitSetEven }

/// Precomputed tests (MAME's `checker`).
struct Tests {
    reset: Test,
    bank: [Test; 4],
    alt1: Test, alt2: Test, alt3: Test, alt4: Test,
    bit1: Test, bit2: Test,
    // bit_set tests for the odd and even states: clear0, set0, clear1, set1
    odd: [Test; 4],
    even: [Test; 4],
    bit4: Test,
    alt_shift: u32,
}

#[derive(Serialize, Deserialize)]
pub struct Slapstic {
    state: St,
    pub current_bank: u8,
    loaded_bank: u8,
    #[serde(skip, default = "Slapstic::tests_105")]
    t: TestsBox,
}

/// Wrapper so serde can skip it.
pub struct TestsBox(Tests);

impl Slapstic {
    fn tests_105() -> TestsBox { TestsBox(Self::build(&SLAPSTIC_105, 0x8000, 0x81FF, 0, 16, 14)) }

    /// Slapstic 105 over `0x8000-0x81FF` on a 16-bit bus (System 2).
    pub fn new_105() -> Self {
        Self { state: St::Idle, current_bank: SLAPSTIC_105.bankstart, loaded_bank: 0, t: Self::tests_105() }
    }

    pub fn reset(&mut self) {
        self.state = St::Idle;
        self.current_bank = SLAPSTIC_105.bankstart;
    }

    fn build(d: &ChipData, start: u32, end: u32, mirror: u32, data_width: u32, address_lines: u32) -> Tests {
        let range_mask = !((end - start) | mirror);
        let range_value = start;
        let shift = if data_width == 16 { 1 } else { 0 };
        let input_mask = ((1u32 << address_lines) - 1) << shift;
        let test_in = |m: Mv| Test { m: range_mask | ((m.mask as u32) << shift), v: range_value | ((m.value as u32) << shift) };
        let test_any = |m: Mv| Test { m: (m.mask as u32) << shift, v: (m.value as u32) << shift };
        let reset = Test { m: range_mask | input_mask, v: range_value };
        let bank = d.bank.map(|b| Test { m: range_mask | input_mask, v: range_value | ((b as u32) << shift) });
        Tests {
            reset, bank,
            alt1: test_any(d.alt1),     // 103-110: the first alt access can be anywhere
            alt2: test_in(d.alt2),
            alt3: test_in(d.alt3),
            alt4: test_in(d.alt4),
            bit1: test_in(d.bit1),
            bit2: test_in(d.bit2),
            odd: [test_in(d.bit3c0), test_in(d.bit3s0), test_in(d.bit3c1), test_in(d.bit3s1)],
            even: [test_in(d.bit3s1), test_in(d.bit3c1), test_in(d.bit3s0), test_in(d.bit3c0)],
            bit4: test_in(d.bit4),
            alt_shift: shift + d.altshift,
        }
    }

    /// Call for every bus access (read or write) with its byte address.
    /// Returns true when the selected bank changed.
    pub fn access(&mut self, addr: u16) -> bool {
        let a = addr as u32;
        let t = &self.t.0;
        let before = self.current_bank;
        match self.state {
            St::Idle => if t.reset.hit(a) { self.state = St::Active; },
            St::Active => {
                if let Some(b) = t.bank.iter().position(|x| x.hit(a)) {
                    self.current_bank = b as u8;
                    self.state = St::Idle;
                } else if t.alt1.hit(a) {
                    self.state = St::AltValid;
                } else if t.bit1.hit(a) {
                    self.state = St::BitLoad;
                }
            }
            St::AltValid => {
                self.state = if t.reset.hit(a) { St::Active }
                    else if t.alt2.hit(a) { St::AltSelect }
                    else { St::Active };
            }
            St::AltSelect => {
                if t.reset.hit(a) { self.state = St::Active; }
                else if t.alt3.hit(a) {
                    self.loaded_bank = ((a >> t.alt_shift) & 3) as u8;
                    self.state = St::AltCommit;
                } else { self.state = St::Active; }
            }
            St::AltCommit => {
                if t.reset.hit(a) { self.state = St::Active; }
                else if t.alt4.hit(a) { self.current_bank = self.loaded_bank; self.state = St::Idle; }
            }
            St::BitLoad => {
                if t.reset.hit(a) { self.state = St::Active; }
                else if t.bit2.hit(a) { self.loaded_bank = self.current_bank; self.state = St::BitSetOdd; }
            }
            St::BitSetOdd | St::BitSetEven => {
                let odd = self.state == St::BitSetOdd;
                let tests = if odd { &t.odd } else { &t.even };
                let next = if odd { St::BitSetEven } else { St::BitSetOdd };
                if t.reset.hit(a) { self.state = St::Active; }
                else if tests[0].hit(a) { self.loaded_bank &= !1; self.state = next; }
                else if tests[1].hit(a) { self.loaded_bank |= 1; self.state = next; }
                else if tests[2].hit(a) { self.loaded_bank &= !2; self.state = next; }
                else if tests[3].hit(a) { self.loaded_bank |= 2; self.state = next; }
                else if t.bit4.hit(a) { self.current_bank = self.loaded_bank; self.state = St::Idle; }
            }
        }
        self.current_bank != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_bank_switch() {
        let mut s = Slapstic::new_105();
        assert_eq!(s.current_bank, 3);
        s.access(0x9000);                 // ignored while idle
        s.access(0x8000);                 // reset -> active
        s.access(0xA000);                 // unrelated accesses are fine
        assert!(s.access(0x8000 + (0x14 << 1)));
        assert_eq!(s.current_bank, 1);
        s.access(0x8000 + (0x1C << 1));   // idle again: no effect
        assert_eq!(s.current_bank, 1);
    }

    #[test]
    fn alternate_sequence() {
        let mut s = Slapstic::new_105();
        s.access(0x8000);
        s.access(0x003D << 1);            // alt1: anywhere
        s.access(0x8000 | (0x0092 << 1)); // alt2
        s.access(0x8000 | (0x00A6 << 1)); // alt3, bank = (addr >> 1) & 3 = 2
        assert_eq!(s.current_bank, 3);
        s.access(0x8000 | (0x0010 << 1)); // alt4: commit
        assert_eq!(s.current_bank, 2);
    }
}
