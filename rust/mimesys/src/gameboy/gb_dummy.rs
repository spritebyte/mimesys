#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::timed::Timed;

    struct SysDummy {
        last_master: u64,
        ticks: u64,
        div: u64,
    }

    impl SysDummy {
        pub fn new() -> Self {
            Self::with_divider(1)
        }
        
        pub fn with_divider(div: u64) -> Self {
            Self {
                last_master: 0,
                ticks: 0,
                div,
            }
        }
    }

    impl Timed for SysDummy {
        fn run_until(&mut self, target_master: u64) {
            let target_tick = target_master / self.div;
            while self.ticks < target_tick {
                self.ticks = self.ticks.wrapping_add(1);
            }
            self.last_master = target_master;
        }

        fn sync_point(&self) -> u64 {
            self.last_master
        }
    }

    #[test]
    fn divider_math() {
        let mut d = SysDummy::with_divider(4);
        d.run_until(1000);
        assert_eq!(d.ticks, 250);
        assert_eq!(d.sync_point(), 1000);
    }

    #[test]
    fn idempotent_past_target() {
        let mut d = SysDummy::with_divider(4);
        d.run_until(1000);
        d.run_until(500);          // already past — must not go backward
        assert_eq!(d.ticks, 250);
    }

    #[test]
    fn accumulates_across_calls() {
        let mut d = SysDummy::with_divider(4);
        d.run_until(400);
        d.run_until(800);
        assert_eq!(d.ticks, 200);  // not reset between calls
    }
}