pub trait Timed {
    fn run_until(&mut self, target_master: u64);
    fn sync_point(&self) -> u64;
}