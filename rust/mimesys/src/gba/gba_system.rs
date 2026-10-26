//! Game Boy Advance: ARM7TDMI at 16.78 MHz plus the bus (memory, I/O, DMA, timers,
//! screen timing). Boots the real BIOS (gba_bios.bin, 16 KB). Without one it uses the
//! built-in open-source replacement BIOS by DenSinH and fleroviux (Cult-of-GBA, MIT,
//! see LICENSE-open_bios) for interrupts and BIOS calls, and jumps straight to the
//! cartridge with the registers the BIOS would leave.

use serde::{Deserialize, Serialize};

use crate::common::cpu::arm::arm7tdmi::{Arm7tdmi, ArmBus, MODE_IRQ, MODE_SVC, MODE_SYS};
use crate::common::input as pad;
use crate::common::system::{DisplayInfo, Rotation, System};
use crate::gba::gba_bus::GbaBus;
use crate::gba::gba_link::{GbaCable, POLL_CYCLES, REPLY_WAIT};
use crate::gba::gba_ppu::{CYCLES_PER_FRAME, SCREEN_H, SCREEN_W};

const STATE_MAGIC: &[u8; 4] = b"GBA1";

/// Cult-of-GBA replacement BIOS (MIT), used when no gba_bios.bin is given.
static OPEN_BIOS: &[u8] = include_bytes!("open_bios.bin");

pub struct GbaSystem {
    pub cpu: Arm7tdmi,
    pub bus: GbaBus,
    has_bios: bool,
    /// skip the BIOS intro even when a BIOS is present
    pub skip_bios: bool,
    current_frame: u64,
    filename: String,
    save_battery_path: String,
    save_state_path: String,
    /// network link cable (None = unplugged); a local pair links in gba_link.rs instead
    pub cable: Option<Box<dyn GbaCable>>,
    last_poll: u64,
}

#[derive(Serialize, Deserialize)]
struct GbaState<C, B> { cpu: C, bus: B, frame: u64 }

impl GbaSystem {
    pub fn from_rom(rom: &[u8], filename: &str, bios: &[u8]) -> Result<Self, String> {
        if rom.len() < 0xC0 { return Err("not a GBA ROM (too small)".into()); }
        if !bios.is_empty() && bios.len() != 0x4000 {
            return Err(format!("GBA BIOS must be 16 KB (got {} bytes)", bios.len()));
        }
        let mut rom = rom.to_vec();
        rom.truncate(0x0200_0000);
        let has_bios = !bios.is_empty();
        let bios = if has_bios { bios.to_vec() } else { OPEN_BIOS.to_vec() };
        let mut sys = Self {
            cpu: Arm7tdmi::new(),
            bus: GbaBus::new(bios, rom),
            has_bios,
            skip_bios: false,
            current_frame: 0,
            filename: filename.to_string(),
            save_battery_path: "user://GD_EMU/GBA/Save".to_string(),
            save_state_path: "user://GD_EMU/GBA/State".to_string(),
            cable: None,
            last_poll: 0,
        };
        sys.reset();
        Ok(sys)
    }

    /// The register setup the BIOS leaves when it starts a cartridge.
    fn direct_boot(&mut self) {
        let c = &mut self.cpu;
        c.set_mode(MODE_SVC);
        c.r[13] = 0x0300_7FE0;
        c.set_mode(MODE_IRQ);
        c.r[13] = 0x0300_7FA0;
        c.set_mode(MODE_SYS);
        c.r[13] = 0x0300_7F00;
        c.cpsr = MODE_SYS;
        self.bus.postflg = 1;
        // the BIOS leaves BG2/BG3 with an identity transform (PA = PD = 1.0)
        for off in [0x20u32, 0x26, 0x30, 0x36] { self.bus.ppu.write8(off + 1, 1); }
        self.cpu.jump(&mut self.bus, 0x0800_0000);
    }

    /// Runs one frame (until VBlank), handling HALT and the network cable.
    fn run_until_frame(&mut self) {
        let limit = self.bus.cycles + 2 * CYCLES_PER_FRAME as u64;
        self.bus.frame_done = false;
        while !self.bus.frame_done && self.bus.cycles < limit {
            if !self.service_cable() {
                return;   // waiting for the other player's data: emulated time doesn't move
            }
            self.run_one();
        }
    }

    /// Runs until `target` clocks, or until a driven link transfer waits for the other
    /// GBA (`bus.sio.awaiting`). Used by the side-by-side pair.
    pub fn run_until(&mut self, target: u64) {
        while self.bus.cycles < target && self.bus.sio.awaiting.is_none() {
            self.run_one();
        }
    }

    /// One instruction, or one skip while halted.
    fn run_one(&mut self) {
        if self.bus.halted {
            if self.bus.ie & self.bus.if_ != 0 {
                self.bus.halted = false;
                // the interrupt reaches the CPU a few cycles after it is flagged:
                // stay asleep until then, so it is taken right on waking
                if let Some(at) = self.bus.irq_visible_at() {
                    if at > self.bus.cycles {
                        self.bus.cycles = at;
                        self.bus.tick();
                    }
                }
            } else {
                // jump to the next timer / serial / screen event (it may wake us)
                let skip = self.bus.cycles_to_event().clamp(1, 1232);
                self.bus.cycles += skip as u64;
                self.bus.tick();
                return;
            }
        }
        self.cpu.irq_line = self.bus.irq_line();
        self.cpu.step(&mut self.bus);
        self.bus.tick();
    }

    /// Network cable: deliver the other side's transfers now and then, and while ours
    /// waits for an answer, wait a little for it. False = stop for now (still waiting).
    fn service_cable(&mut self) -> bool {
        let sio = &mut self.bus.sio;
        match &mut self.cable {
            None => {
                if sio.awaiting.is_some() { sio.unplugged(); }
                true
            }
            Some(c) => {
                if sio.awaiting.is_some() {
                    c.service(sio, REPLY_WAIT);
                    self.last_poll = self.bus.cycles;
                    self.bus.sio.awaiting.is_none()
                } else {
                    if self.bus.cycles.wrapping_sub(self.last_poll) >= POLL_CYCLES {
                        c.service(sio, std::time::Duration::ZERO);
                        self.last_poll = self.bus.cycles;
                    }
                    true
                }
            }
        }
    }

    /// Link log: one line a frame with where the CPU is and what it waits for.
    pub fn trace_frame(&mut self) {
        if self.bus.sio.trace.is_none() { return; }
        let b = &self.bus;
        let state = format!("pc {:08X} {}ie {:04X} if {:04X} ime {} dispstat {:04X}",
            self.cpu.pc(), if b.halted { "HALT " } else { "" }, b.ie, b.if_, b.ime as u8, b.ppu.dispstat);
        let clock = self.bus.cycles;
        self.bus.sio.note_frame(clock, state);
    }

    /// Save file name (the right-hand GBA of a pair running the same game gets "_p2").
    pub fn set_save_filename(&mut self, name: &str) { self.filename = name.to_string(); }

    /// Steps one instruction (tests and debugging).
    pub fn step(&mut self) {
        self.cpu.irq_line = self.bus.irq_line();
        self.cpu.step(&mut self.bus);
        self.bus.tick();
    }
}

impl System for GbaSystem {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        self.run_until_frame();
        self.trace_frame();
        self.current_frame += 1;
    }

    /// KEYINPUT, active low: A B Select Start Right Left Up Down R L.
    fn set_input(&mut self, m: u32) {
        let map = [pad::A, pad::B, pad::SELECT, pad::START, pad::RIGHT, pad::LEFT, pad::UP, pad::DOWN, pad::R, pad::L];
        let mut keys = 0x3FFu16;
        for (i, b) in map.iter().enumerate() {
            if m & b != 0 { keys &= !(1 << i); }
        }
        self.bus.set_keys(keys);
    }

    fn framebuffer(&self) -> &[u8] { &self.bus.ppu.framebuffer }

    fn display_info(&self) -> DisplayInfo {
        DisplayInfo {
            buffer_width: SCREEN_W as i32,
            buffer_height: SCREEN_H as i32,
            visible_x: 0,
            visible_y: 0,
            visible_width: SCREEN_W as i32,
            visible_height: SCREEN_H as i32,
            target_aspect_ratio: 3.0 / 2.0,
            rotation: Rotation::None,
        }
    }

    fn drain_audio(&mut self) -> Vec<f32> { self.bus.apu.drain() }
    fn sample_rate(&self) -> u32 { 44100 }

    fn save_state(&self) -> Vec<u8> {
        let mut out = STATE_MAGIC.to_vec();
        let body = bincode::serde::encode_to_vec(
            &GbaState { cpu: &self.cpu, bus: &self.bus, frame: self.current_frame },
            bincode::config::standard().with_fixed_int_encoding());
        match body { Ok(b) => { out.extend(b); out } Err(_) => Vec::new() }
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        if data.len() < 4 || &data[..4] != STATE_MAGIC { return Err("not a GBA save state".into()); }
        let (s, _): (GbaState<Arm7tdmi, GbaBus>, _) = bincode::serde::decode_from_slice(
            &data[4..], bincode::config::standard().with_fixed_int_encoding())
            .map_err(|e| format!("couldn't decode save state: {e:?}"))?;
        let mut bus = s.bus;
        bus.keep_static(&mut self.bus);
        // the cable belongs to this session, not the saved one
        bus.sio.linked = self.bus.sio.linked;
        bus.sio.parent = self.bus.sio.parent;
        bus.sio.awaiting = None;
        self.bus = bus;
        self.cpu = s.cpu;
        self.current_frame = s.frame;
        Ok(())
    }

    fn load_sram(&mut self, data: &[u8]) { self.bus.cart.load_save(data); }
    fn get_sram(&self) -> Option<&[u8]> {
        if self.bus.cart.save.is_empty() { None } else { Some(&self.bus.cart.save) }
    }

    fn reset(&mut self) {
        let bios = std::mem::take(&mut self.bus.bios);
        let rom = std::mem::take(&mut self.bus.rom);
        let fb = std::mem::take(&mut self.bus.ppu.framebuffer);
        let old_sio = (self.bus.sio.linked, self.bus.sio.parent);
        let old_cart = std::mem::replace(&mut self.bus.cart, crate::gba::gba_cart::Cart::new(&[]));
        self.bus = GbaBus::new(bios, rom);
        // the save chip keeps its contents (and a size it has settled on)
        self.bus.cart.save_type = old_cart.save_type;
        self.bus.cart.save = old_cart.save;
        self.bus.cart.utc_offset_min = old_cart.utc_offset_min;
        // a plugged-in cable stays plugged in
        self.bus.sio.linked = self.cable.is_some() || old_sio.0;
        self.bus.sio.parent = old_sio.1;
        self.bus.ppu.framebuffer = fb;
        self.cpu = Arm7tdmi::new();
        if self.has_bios && !self.skip_bios {
            self.cpu.reset(&mut self.bus);
        } else {
            self.direct_boot();
        }
    }

    fn attach_net(&mut self, link: Option<crate::gameboy::gb_netlink::NetLink>) -> bool {
        self.bus.sio.parent = link.as_ref().is_some_and(|l| l.is_host());
        self.bus.sio.linked = link.is_some();
        self.cable = link.map(|l| Box::new(l) as Box<dyn GbaCable>);
        if self.bus.sio.awaiting.is_some() { self.bus.sio.unplugged(); }
        true
    }

    fn take_trace(&mut self) -> Vec<String> {
        self.bus.sio.trace.as_mut().map(std::mem::take).unwrap_or_default()
    }

    fn cable_status(&self) -> String {
        self.cable.as_ref().map_or(String::new(), |c| c.status())
    }

    fn has_battery(&self) -> bool { true }
    fn save_battery_path(&self) -> &str { &self.save_battery_path }
    fn save_filename(&self) -> &str { &self.filename }
    fn save_state_path(&self) -> &str { &self.save_state_path }
    fn power_on(&mut self) {}
    fn power_off(&mut self) {}
    fn is_sram_dirty(&self) -> bool { self.bus.cart.dirty }
    fn clear_sram_dirty(&mut self) { self.bus.cart.dirty = false; }
    fn get_current_frame(&self) -> i64 { self.current_frame as i64 }

    /// "skip_bios": 1 = start the cartridge directly (takes effect on reset).
    /// "rtc_utc_offset": minutes to add to UTC for the cartridge clock (local time).
    fn set_option(&mut self, key: &str, value: i64) {
        match key {
            "skip_bios" => self.skip_bios = value != 0,
            "rtc_utc_offset" => self.bus.cart.utc_offset_min = value as i32,
            "link_trace" => self.bus.sio.trace = if value != 0 { Some(Vec::new()) } else { None },
            _ => {}
        }
    }
}

// The bus is also usable directly as an ArmBus by tests.
#[allow(dead_code)]
fn _assert_bus(b: &mut GbaBus) -> &mut dyn ArmBus { b }
