use crate::gameboy::gb_bus::{Bus,GameBoyBus};

#[derive(Default)]
pub struct DmaState {
    pub active: bool,
    pub source_base: u16,
    pub current_offset: u8,
    pub delay_m_cycles: u8,
    pub next_tick_master: u64,
}

impl DmaState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn start(&mut self, value: u8, current_master:u64) {
        self.source_base = (value as u16) << 8;
        self.current_offset = 0;
        // 1 M-cycle setup delay before transfer starts
        self.delay_m_cycles = 1;
        self.active = false;
        self.next_tick_master = current_master + 4; 
    }

    // advance DMA timing by 1 M-cycle
    // returns 'Some((source address, oam_offset))' if a transfer byte should occur.
    pub fn tick_m_cycle(&mut self) -> Option<(u16, usize)> {
        if self.delay_m_cycles > 0 {
            self.delay_m_cycles -= 1;
            if self.delay_m_cycles == 0 {
                self.active = true;
            }
            return None;
        }

        if !self.active {
            return None;
        }

        // Copy 1 byte per M-cycle from Source to OAM
        let src_addr = self.source_base + (self.current_offset as u16);
        let oam_offset = self.current_offset as usize;

        self.current_offset += 1;
        if self.current_offset >= 160 {
            self.active = false;
        }

        Some ((src_addr, oam_offset))
    }
}