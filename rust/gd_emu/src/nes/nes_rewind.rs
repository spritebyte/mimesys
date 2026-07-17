use std::collections::VecDeque;
use crate::nes::nes_system::NesSystem;

pub struct RewindBuffer {
    checkpoints: VecDeque<(u64, Vec<u8>)>,
    checkpoint_interval: u64,
    max_checkpoints: usize,
    input_history: VecDeque<(u64, u16)>,
    max_input_history: usize,
}

impl Default for RewindBuffer {
    fn default() -> Self {
        Self {
            checkpoints: VecDeque::new(),
            checkpoint_interval: 60,
            max_checkpoints: 60,
            input_history: VecDeque::new(),
            max_input_history: 3660,
        }
    }
}

impl RewindBuffer {
    pub fn new(checkpoint_interval: u64, max_checkpoints: usize) -> Self {
        let max_input_history = (checkpoint_interval as usize * max_checkpoints) + checkpoint_interval as usize;
        Self {
            checkpoints: VecDeque::with_capacity(max_checkpoints),
            checkpoint_interval,
            max_checkpoints,
            input_history: VecDeque::with_capacity(max_input_history),
            max_input_history,
        }
    }

    /// Call once per completed frame, after run_slice() finishes.
    pub fn record_frame(&mut self, frame_number: u64, input_mask: u16, system: &NesSystem) {
        self.input_history.push_back((frame_number, input_mask));
        while self.input_history.len() > self.max_input_history {
            self.input_history.pop_front();
        }

        if frame_number % self.checkpoint_interval == 0 {
            self.checkpoints.push_back((frame_number, system.save_state_to_bytes().expect("REASON")));
            while self.checkpoints.len() > self.max_checkpoints {
                self.checkpoints.pop_front();
            }
        }
    }

    /// Rewinds `system` in place to `target_frame` by loading the nearest
    /// prior checkpoint and replaying recorded inputs forward.
    pub fn rewind_to(&mut self, target_frame: u64, system: &mut NesSystem) -> Result<(), String> {
        let (checkpoint_frame, checkpoint_bytes) = self.checkpoints.iter()
            .rev()
            .find(|(f, _)| *f <= target_frame)
            .ok_or_else(|| "No checkpoint old enough to rewind that far".to_string())?;

        system.load_state_from_bytes(checkpoint_bytes)?;
        system.set_current_frame(*checkpoint_frame);

        let inputs_to_replay: Vec<u16> = self.input_history.iter()
            .filter(|(f, _)| *f > *checkpoint_frame && *f <= target_frame)
            .map(|(_, input)| *input)
            .collect();

        for input in inputs_to_replay {
            system.run_slice(input);
        }

        // The user has committed to this point in time — discard everything
        // that was previously "ahead" of it, so new play overwrites cleanly
        // rather than leaving orphaned future frames dangling.
        self.truncate_after(target_frame);

        Ok(())
    }

    fn truncate_after(&mut self, frame: u64) {
        self.checkpoints.retain(|(f, _)| *f <= frame);
        self.input_history.retain(|(f, _)| *f <= frame);
    }

    /// Furthest back in time a rewind can currently reach.
    pub fn oldest_available_frame(&self) -> Option<u64> {
        self.checkpoints.front().map(|(f, _)| *f)
    }

    pub fn clear(&mut self) {
        self.checkpoints.clear();
        self.input_history.clear();
    }
}