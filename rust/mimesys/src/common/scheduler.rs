pub struct Scheduler {
    master: u64,
    cpu_div: u32,  // NES: 12, GB: 4 (DMG master = 4.19MHz, CPU = 1.05MHz)
}

impl Scheduler {
    fn advance_to(&mut self, target_master: u64, components: &mut dyn Bus) {
        components.run_all_until(target_master);
    }
}