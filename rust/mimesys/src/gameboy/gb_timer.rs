use crate::common::timed::Timed;

pub struct GbTimer {
    last_master: u64,
    div: u64,
    ticks: u64,
    counter: u16,
    tima: u8,
    tma: u8,
    tac: u8,

    prev_and_bit: bool,
    reload_pending: bool,
    pending_irqs: u8,
}

impl GbTimer {
    pub fn new() -> Self {
        Self {
            last_master: 0,
            div: 1,
            ticks: 0,
            counter: 0,
            tima: 0,
            tma: 0,
            tac: 0,
            prev_and_bit: false,
            reload_pending: false,
            pending_irqs: 0,
        }
    }

    fn selected_bit(&self) -> u16 {
        match self.tac & 0b11 {
            0b00 => 1 << 9, // 4096 Hz
            0b01 => 1 << 3, // 262144 Hz
            0b10 => 1 << 5, // 65536 Hz
            0b11 => 1 << 7, // 16384 Hz
            _=> unreachable!(),
        }
    }

    fn timer_enabled(&self) -> bool {
        self.tac & 0b100 != 0
    }

    fn tick_one_t_cycle(&mut self) {
        if self.reload_pending {
            self.tima = self.tma;
            self.pending_irqs |= 1 << 2; // Timer IRQ = bit 2
            self.reload_pending = false;
        }

        self.counter = self.counter.wrapping_add(1);
        
        let and_bit = self.timer_enabled() && (self.counter & self.selected_bit()) != 0;
        if self.prev_and_bit && !and_bit {
            let (new_tima, overflowed) = self.tima.overflowing_add(1);
            self.tima = new_tima;
            if overflowed {
                self.reload_pending = true;
            }
        }
        self.prev_and_bit = and_bit;
    }

    pub fn read(&self, addr: u16) -> u8 {
        match addr {
            0xFF04 => (self.counter >> 8) as u8,
            0xFF05 => self.tima,
            0xFF06 => self.tma,
            0xFF07 => self.tac | 0xF8,
            _ => 0xFF,
        }
    }

    pub fn write(&mut self, addr: u16, value: u8) {
        match addr {
            0xFF04 => self.counter = 0,
            0xFF05 => self.tima = value,
            0xFF06 => self.tma = value,
            0xFF07 => self.tac = value & 0b111,
            _=> {}
        }
    }

    pub fn take_irqs(&mut self) -> u8 {
        std::mem::replace(&mut self.pending_irqs, 0)
    }
}

impl Timed for GbTimer {
    fn run_until(&mut self, target_master: u64) {
        let target_tick = target_master / self.div;
        while self.ticks < target_tick {
            self.tick_one_t_cycle();
            self.ticks = self.ticks.wrapping_add(1);
        }
        self.last_master = target_master;
    }

    fn sync_point(&self) -> u64 {
        self.last_master
    }
}