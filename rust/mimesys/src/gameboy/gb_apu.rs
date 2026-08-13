    use crate::common::timed::Timed;

    pub struct GbAPU {
        last_master: u64,
        ticks: u64,
        div: u64,
    }

    impl GbAPU {
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

    impl Timed for GbAPU {
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
