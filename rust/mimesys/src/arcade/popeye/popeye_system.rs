use crate::common::cpu::z80::Z80Cpu;
use crate::common::input as pad;
use crate::common::system::{System, DisplayInfo, Rotation};
use crate::arcade::take_region;
use crate::common::audio::ay8910::Ay8910;
use crate::arcade::popeye::popeye_bus::PopeyeBus;
use crate::arcade::popeye::popeye_video::{PopeyeVideo, SCREEN_W, SCREEN_H};
use crate::arcade::popeye::popeye_bus::PopeyeBusState;
use crate::arcade::hiscore::HiScoreMem;
use crate::arcade::state;
use crate::common::cpu::z80::{Bus, Z80State};
use std::collections::HashMap;

const STATE_VERSION: u32 = 1;

// 8 MHz crystal: Z80 at 4 MHz, AY-3-8910 at 2 MHz. 59.94 frames per second.
const CPU_CLOCK: u64 = 4_000_000;
const FRAME_RATE_X100: u64 = 5994;
const CPU_CYCLES_PER_AY_TICK: i32 = 16;   // AY ticks at 2 MHz / 8 = 250 kHz

pub struct PopeyeSystem {
    pub cpu: Z80Cpu,
    pub bus: PopeyeBus,
    cycle_debt: u64,          // cycles x 100, so 59.94 Hz stays exact over time
    ay_cycles: i32,
    nmi_line: bool,           // held after vblank until the game disables NMI
    watchdog_count: u8,
    in_reset: bool,
    current_frame: u64,
    save_filename: String,
    save_path: String,
}

impl PopeyeSystem {
    /// `protected`: the original board with the protection chip at E000.
    /// popeyeu / popeyef / popeyeb3 have it removed ("noalu").
    pub fn new(set_name: &str, mut regions: HashMap<String, Vec<u8>>, protected: bool) -> Result<Self, String> {
        let rom = take_region(&mut regions, "maincpu")?;
        let gfx1 = take_region(&mut regions, "gfx1")?;
        let gfx2 = take_region(&mut regions, "gfx2")?;
        let proms = take_region(&mut regions, "proms")?;
        let sprpal = take_region(&mut regions, "sprpal")?;
        if rom.len() < 0x8000 {
            return Err(format!("maincpu is 0x{:X} bytes, expected 0x8000", rom.len()));
        }

        let video = PopeyeVideo::new(&gfx1, &gfx2, &proms, &sprpal)?;
        let ay = Ay8910::new(2_000_000, 44100);
        Ok(Self {
            cpu: Z80Cpu::new(),
            bus: PopeyeBus::new(decrypt_tpp2(&rom), video, ay, protected),
            cycle_debt: 0,
            ay_cycles: 0,
            nmi_line: false,
            watchdog_count: 0,
            in_reset: false,
            current_frame: 0,
            save_filename: set_name.to_string(),
            save_path: "user://GD_EMU/Arcade/Popeye/State".to_string(),
        })
    }

    /// The game turns NMI on and off through bit 0 of the Z80's I register: the board
    /// watches the address bus during the Z80's refresh cycle, which carries I in A8-A15.
    /// Bit 1 of I enables the watchdog.
    fn nmi_enabled(&self) -> bool { self.cpu.i & 0x01 != 0 }
    fn watchdog_enabled(&self) -> bool { self.cpu.i & 0x02 != 0 }

    fn vblank(&mut self) {
        // MAME draws the frame first, then the DMA/NMI happen
        self.bus.video.render_frame();
        let src = self.bus.dma_source().to_vec();
        self.bus.video.dma(&src);
        self.bus.field ^= 1;

        if self.nmi_enabled() && !self.nmi_line {
            self.nmi_line = true;      // NMI is edge triggered: one per rising edge
            if !self.in_reset {
                self.cpu.nmi(&mut self.bus);
            }
        }

        // Watchdog: counts frames while NMI is off and the watchdog is on.
        // Bit 2 of the counter is the reset line.
        let mut count = self.watchdog_count;
        if self.nmi_enabled() || !self.watchdog_enabled() {
            count = 0;
        } else {
            count = (count + 1) & 0x0F;
        }
        if (count ^ self.watchdog_count) & 0x04 != 0 {
            self.in_reset = count & 0x04 != 0;
            self.bus.ay.reset();
            if !self.in_reset {
                self.cpu = Z80Cpu::new();   // coming out of reset
                self.nmi_line = false;
            }
        }
        self.watchdog_count = count;
    }
}

impl System for PopeyeSystem {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);

        self.cycle_debt += CPU_CLOCK * 100;
        let budget = (self.cycle_debt / FRAME_RATE_X100) as i64;
        self.cycle_debt %= FRAME_RATE_X100;

        let mut done: i64 = 0;
        while done < budget {
            let c = if self.in_reset { 4 } else { self.cpu.step(&mut self.bus) as i32 };
            done += c as i64;
            if !self.nmi_enabled() { self.nmi_line = false; }

            self.ay_cycles += c;
            while self.ay_cycles >= CPU_CYCLES_PER_AY_TICK {
                self.ay_cycles -= CPU_CYCLES_PER_AY_TICK;
                self.bus.ay.tick();
            }
        }
        self.vblank();
        self.current_frame += 1;
    }

    /// Active high.
    ///   P1:     0x01 right, 0x02 left, 0x04 up, 0x08 down, 0x10 punch
    ///   SYSTEM: 0x04 start 1, 0x08 start 2, 0x20 coin 2, 0x40 service, 0x80 coin 1
    fn set_input(&mut self, mask: u32) {
        let on = |b: u32| mask & b != 0;
        let mut p1 = 0;
        if on(pad::RIGHT) { p1 |= 0x01; }
        if on(pad::LEFT)  { p1 |= 0x02; }
        if on(pad::UP)    { p1 |= 0x04; }
        if on(pad::DOWN)  { p1 |= 0x08; }
        if on(pad::A) || on(pad::B) { p1 |= 0x10; }
        self.bus.p1 = p1;

        let mut sys = 0;
        if on(pad::START)  { sys |= 0x04; }
        if on(pad::START2) { sys |= 0x08; }
        if on(pad::COIN) || on(pad::SELECT) { sys |= 0x80; }
        self.bus.system = sys;
    }

    fn drain_audio(&mut self) -> Vec<f32> { std::mem::take(&mut self.bus.ay.out) }
    fn sample_rate(&self) -> u32 { 44100 }
    fn power_on(&mut self) {}
    fn power_off(&mut self) {}
    fn has_battery(&self) -> bool { false }
    fn save_filename(&self) -> &str { &self.save_filename }
    fn save_battery_path(&self) -> &str { &self.save_path }
    fn save_state_path(&self) -> &str { &self.save_path }
    fn is_sram_dirty(&self) -> bool { false }
    fn clear_sram_dirty(&mut self) {}
    fn get_current_frame(&self) -> i64 { self.current_frame as i64 }
    fn get_sram(&self) -> Option<&[u8]> { None }
    fn load_sram(&mut self, _data: &[u8]) {}
    fn framebuffer(&self) -> &[u8] { &self.bus.video.framebuffer }

    fn reset(&mut self) {
        self.cpu = Z80Cpu::new();
        self.bus.ay.reset();
        self.nmi_line = false;
        self.watchdog_count = 0;
        self.in_reset = false;
    }

    fn display_info(&self) -> DisplayInfo {
        DisplayInfo {
            buffer_width: SCREEN_W as i32,
            buffer_height: SCREEN_H as i32,
            visible_x: 0,
            visible_y: 0,
            visible_width: SCREEN_W as i32,
            visible_height: SCREEN_H as i32,
            target_aspect_ratio: 4.0 / 3.0,
            rotation: Rotation::None,
        }
    }

    fn save_state(&self) -> Vec<u8> {
        let s = (self.cpu.save_state(), self.bus.save(), self.cycle_debt, self.ay_cycles,
                 self.nmi_line, self.watchdog_count, self.in_reset, self.current_frame);
        state::encode(&self.save_filename, STATE_VERSION, &s)
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let (cpu, bus, cycle_debt, ay_cycles, nmi_line, watchdog_count, in_reset, frame):
            (Z80State, PopeyeBusState, u64, i32, bool, u8, bool, u64) =
            state::decode(&self.save_filename, STATE_VERSION, data)?;
        self.bus.load(bus)?;
        self.cpu.load_state(&cpu);
        self.cycle_debt = cycle_debt;
        self.ay_cycles = ay_cycles;
        self.nmi_line = nmi_line;
        self.watchdog_count = watchdog_count;
        self.in_reset = in_reset;
        self.current_frame = frame;
        Ok(())
    }
}

/// The TPP2 board scrambles both address and data lines of the program ROMs.
/// MAME tpp2_state::decrypt_rom.
pub fn decrypt_tpp2(rom: &[u8]) -> Vec<u8> {
    let len = rom.len().min(0x8000);
    (0..len).map(|i| {
        let a = bitswap(i, &[15, 14, 13, 12, 11, 10, 8, 7, 6, 3, 9, 5, 4, 2, 1, 0]) ^ 0x3F;
        bitswap(rom[a] as usize, &[3, 4, 2, 5, 1, 6, 0, 7]) as u8
    }).collect()
}

/// MAME's bitswap<N>(val, bits...): `bits[0]` is the source of the result's top bit.
pub(crate) fn bitswap(val: usize, bits: &[u8]) -> usize {
    let n = bits.len();
    bits.iter().enumerate().map(|(k, &src)| ((val >> src) & 1) << (n - 1 - k)).sum()
}

impl HiScoreMem for PopeyeSystem {
    fn hs_read(&mut self, addr: u16) -> u8 { self.bus.read(addr) }
    fn hs_write(&mut self, addr: u16, v: u8) { self.bus.write(addr, v) }
}
