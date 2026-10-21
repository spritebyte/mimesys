// Two Game Boys joined by a link cable, inside one emulator.
//
// Both run in small time slices, taking turns, so neither gets more than SLICE T-cycles ahead
// of the other. When one finishes driving a transfer (Serial::awaiting), its byte goes to the
// other's external_clock() and the answer comes straight back. The other side is at most one
// slice behind, well under one bit time (512 T-cycles), so games can't tell.
//
// `LinkUnit` is what a Game Boy has to offer for this (GbSystem implements it); the tests
// below use a tiny fake.

use crate::gameboy::gb_serial::Serial;

/// T-cycles each Game Boy runs before the other gets a turn.
pub const SLICE: u64 = 256;
/// T-cycles in one frame (4194304 Hz / 59.73 fps).
pub const FRAME_CYCLES: u64 = 70224;

pub trait LinkUnit {
    fn serial(&mut self) -> &mut Serial;
    /// Total T-cycles run so far.
    fn clock(&self) -> u64;
    /// Run until clock() >= target, or until serial().awaiting is set.
    fn run_until(&mut self, target: u64);
}

/// A cable to another emulator (e.g. over the network: gb_netlink.rs).
pub trait LinkCable: Send {
    /// Handle traffic: deliver the other side's transfers into `serial`, send ours, and if
    /// we're waiting for a reply (serial.awaiting) wait up to `wait` for it.
    fn service(&mut self, serial: &mut Serial, wait: std::time::Duration);
    /// One line for the UI ("Connected to 192.168.1.20", "Hosting: waiting...", ...).
    fn status(&self) -> String;
}

/// How often (T-cycles) a cable is checked for the other side's transfers: ~4000x a second.
pub const POLL_CYCLES: u64 = 1024;
/// How long a frame may block waiting for the other side's byte before giving up for now.
pub const REPLY_WAIT: std::time::Duration = std::time::Duration::from_millis(20);

/// Call before every CPU step. Returns false when the Game Boy must stop for now: it drove a
/// transfer and the other side's byte hasn't arrived (try again next frame; emulated time
/// doesn't move meanwhile, so the game never notices).
pub fn service_port(serial: &mut Serial, cable: &mut Option<Box<dyn LinkCable>>,
                    clock: u64, last_poll: &mut u64) -> bool {
    match cable {
        None => {
            if serial.awaiting.is_some() {
                serial.finish(0xFF);                 // nothing plugged in
            }
            true
        }
        Some(c) => {
            if serial.awaiting.is_some() {
                c.service(serial, REPLY_WAIT);
                *last_poll = clock;
                serial.awaiting.is_none()
            } else {
                if clock.wrapping_sub(*last_poll) >= POLL_CYCLES {
                    c.service(serial, std::time::Duration::ZERO);
                    *last_poll = clock;
                }
                true
            }
        }
    }
}

/// Hand a finished transfer from `from` to `to`, if there is one. Returns true if it did.
pub fn deliver(from: &mut dyn LinkUnit, to: &mut dyn LinkUnit) -> bool {
    let Some(out) = from.serial().awaiting else { return false; };
    let reply = to.serial().external_clock(out);
    from.serial().finish(reply);
    true
}

/// Advance both units by `cycles` T-cycles, linked.
pub fn run_linked(a: &mut dyn LinkUnit, b: &mut dyn LinkUnit, cycles: u64) {
    let (start_a, start_b) = (a.clock(), b.clock());
    let mut done = 0;
    while done < cycles {
        done = (done + SLICE).min(cycles);
        run_unit(a, b, start_a + done);
        run_unit(b, a, start_b + done);
    }
}

fn run_unit(me: &mut dyn LinkUnit, other: &mut dyn LinkUnit, target: u64) {
    loop {
        me.run_until(target);
        if !deliver(me, other) {
            break;          // reached the target with no transfer pending
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EchoCable { polls: u32, answer: Option<u8> }
    impl LinkCable for EchoCable {
        fn service(&mut self, serial: &mut Serial, _wait: std::time::Duration) {
            self.polls += 1;
            if let (Some(_), Some(a)) = (serial.awaiting, self.answer) { serial.finish(a); }
        }
        fn status(&self) -> String { String::new() }
    }

    #[test]
    fn service_port_no_cable_and_with_cable() {
        let mut s = Serial::new(false);
        let mut last = 0;
        s.write_sc(0x81, false);
        s.tick(4096);
        assert!(service_port(&mut s, &mut None, 0, &mut last));
        assert_eq!(s.sb, 0xFF);
        // cable that hasn't answered: stop; then it answers: go on
        let mut cable: Option<Box<dyn LinkCable>> = Some(Box::new(EchoCable { polls: 0, answer: None }));
        s.write_sc(0x81, false);
        s.tick(4096);
        assert!(!service_port(&mut s, &mut cable, 5000, &mut last));
        let mut cable: Option<Box<dyn LinkCable>> = Some(Box::new(EchoCable { polls: 0, answer: Some(0x3C) }));
        assert!(service_port(&mut s, &mut cable, 5000, &mut last));
        assert_eq!(s.sb, 0x3C);
    }

    /// A fake Game Boy: a clock, a serial port, and a script of (time, SB, SC) writes.
    struct Fake {
        clock: u64,
        serial: Serial,
        script: Vec<(u64, u8, u8)>,
        irq_at: Vec<(u64, u8)>,     // (time, SB) at each serial interrupt
    }

    impl Fake {
        fn new(script: Vec<(u64, u8, u8)>) -> Self {
            Self { clock: 0, serial: Serial::new(false), script, irq_at: vec![] }
        }
    }

    impl LinkUnit for Fake {
        fn serial(&mut self) -> &mut Serial { &mut self.serial }
        fn clock(&self) -> u64 { self.clock }
        fn run_until(&mut self, target: u64) {
            while self.clock < target && self.serial.awaiting.is_none() {
                self.clock += 4;                                   // one M-cycle
                if let Some(&(t, sb, sc)) = self.script.first() {
                    if self.clock >= t {
                        self.serial.write_sb(sb);
                        self.serial.write_sc(sc, false);
                        self.script.remove(0);
                    }
                }
                self.serial.tick(4);
                if self.serial.take_irq() {
                    self.irq_at.push((self.clock, self.serial.sb));
                }
            }
        }
    }

    #[test]
    fn bytes_cross_the_cable_both_ways() {
        // B gets ready first (external clock), A drives a byte at t=1000
        let mut a = Fake::new(vec![(1000, 0x42, 0x81)]);
        let mut b = Fake::new(vec![(100, 0x99, 0x80)]);
        run_linked(&mut a, &mut b, FRAME_CYCLES);
        assert_eq!(a.serial.sb, 0x99);
        assert_eq!(b.serial.sb, 0x42);
        // A's transfer takes 4096 cycles; both sides see the interrupt around then
        let (ta, _) = a.irq_at[0];
        let (tb, _) = b.irq_at[0];
        assert!((5096..5096 + 8).contains(&ta), "A finished at {ta}");
        assert!(tb.abs_diff(ta) <= SLICE + 8, "B finished at {tb}, A at {ta}");
        assert_eq!(a.clock, FRAME_CYCLES);
        assert_eq!(b.clock, FRAME_CYCLES);
    }

    #[test]
    fn partner_not_ready_reads_its_output_line() {
        let mut a = Fake::new(vec![(0, 0x42, 0x81)]);
        let mut b = Fake::new(vec![(0, 0x12, 0x00)]);   // SB loaded but not ready
        run_linked(&mut a, &mut b, 10_000);
        assert_eq!(a.serial.sb, 0x00);                   // B's SB bit 7 = 0
        assert_eq!(b.serial.sb, 0x12);                   // untouched
        assert!(b.irq_at.is_empty());
    }

    #[test]
    fn many_bytes_in_a_row() {
        // A sends 0,1,2.. every 5000 cycles; B answers 0x80|n and re-arms after each byte
        let a_script: Vec<_> = (0..10u8).map(|n| (n as u64 * 5000 + 10, n, 0x81)).collect();
        let b_script: Vec<_> = (0..10u8).map(|n| (n as u64 * 5000, 0x80 | n, 0x80)).collect();
        let mut a = Fake::new(a_script);
        let mut b = Fake::new(b_script);
        run_linked(&mut a, &mut b, 60_000);
        let got_a: Vec<u8> = a.irq_at.iter().map(|&(_, sb)| sb).collect();
        let got_b: Vec<u8> = b.irq_at.iter().map(|&(_, sb)| sb).collect();
        assert_eq!(got_a, (0..10u8).map(|n| 0x80 | n).collect::<Vec<_>>());
        assert_eq!(got_b, (0..10u8).collect::<Vec<_>>());
    }
}
