pub struct Scheduler {
    master: u64,
    cpu_div: u32,  // NES: 12, GB: 4 (DMG master = 4.19MHz, CPU = 1.05MHz)
}

impl Scheduler {
    fn advance_to(&mut self, target_master: u64, components: &mut Components) {
        components.ppu.run_until(target_master);
        components.apu.run_until(target_master);
        components.timer.run_until(target_master);
    }
}