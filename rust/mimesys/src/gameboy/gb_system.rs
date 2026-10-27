use crate::gameboy::gb_cpu::GameBoyCpu;
use crate::gameboy::gb_bus::GameBoyBus;
use crate::gameboy::gb_common::GbVariant;
use crate::gameboy::gb_cartridge::{GbCartridge, CartType};
use crate::gameboy::gb_palette::*;
use crate::gameboy::gb_sgb::{Sgb, SGB_H, SGB_W};

/// Shades 0-3 as colours (blue channel), for the SGB to colour itself.
const SGB_SHADES: DmgPaletteSet = DmgPaletteSet { bg: [0, 1, 2, 3], obp0: [0, 1, 2, 3], obp1: [0, 1, 2, 3] };
use crate::gameboy::gb_mbc;
use crate::gameboy::gb_link::LinkUnit;
use crate::gameboy::gb_serial::Serial;
use crate::gameboy::gb_link::{LinkCable, service_port};
use crate::common::system::{System,DisplayInfo,Rotation};
use crate::common::snapshot::Snapshot;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
//use std::path::Path;
use std::io::Cursor;

const STATE_MAGIC: &[u8; 4] = b"GBST";
const STATE_VERSION: u32 = 2;   // 2: Super Game Boy, SGB multiplayer pads

fn state_config() -> impl bincode::config::Config {
    bincode::config::standard().with_fixed_int_encoding()
}

#[derive(serde::Deserialize)]
struct GbState { cpu: GameBoyCpu, bus: GameBoyBus, mbc: Vec<u8>, frame: u64 }

pub struct GbSystem {
    pub cpu: GameBoyCpu,
    pub bus: GameBoyBus,
    pub slice_complete: bool,
//    pub frame_published: Arc<AtomicBool>,
    pub is_running: Arc<AtomicBool>,
    pub save_battery_path: String,
    pub save_state_path: String,
    pub save_filename: String,
    pub current_frame: u64,
    pub cgb_mode: bool,
    pub current_ui_theme: PaletteTheme,
    pub frame_cache: Vec<u8>,
    pub cable: Option<Box<dyn LinkCable>>,
    pub last_poll: u64,
    /// how this machine was started (reset starts it the same way)
    pub boot: BootConfig,
}

/// Which Game Boy to emulate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Model {
    /// Game Boy Color for colour cartridges, original Game Boy otherwise
    Auto,
    Dmg,
    /// DMG games run in the CGB's compatibility mode, coloured by its boot ROM
    Cgb,
    /// Super Game Boy: a DMG inside a border, coloured by the game's SGB commands
    Sgb,
}

/// Boot ROMs and start-up choices from the frontend.
///
/// The frontend sends one blob: "GBBC" then entries of tag (4 bytes), length (u32 LE) and
/// data. Tags: "MODL" (1 byte: 0 auto, 1 DMG, 2 CGB, 3 SGB), "SKIP" (1 byte: 1 = run the
/// boot ROM instantly, no logo animation), "DMG0" (256-byte DMG boot ROM), "CGB0" (2304-byte
/// CGB boot ROM), "SGB0" (256-byte SGB boot ROM). A bare file (older frontends) counts as the DMG or CGB boot ROM by its size.
/// Missing boot ROMs are replaced by SameBoy's open-source ones (bootroms/, MIT licence).
#[derive(Clone, Default)]
pub struct BootConfig {
    pub dmg: Option<Vec<u8>>,
    pub cgb: Option<Vec<u8>>,
    pub sgb: Option<Vec<u8>>,
    pub model: Option<Model>,
    pub skip: bool,
}

static OPEN_DMG_BOOT: &[u8] = include_bytes!("bootroms/dmg_boot.bin");
static OPEN_CGB_BOOT: &[u8] = include_bytes!("bootroms/cgb_boot.bin");
static OPEN_SGB_BOOT: &[u8] = include_bytes!("bootroms/sgb_boot.bin");
const DMG_BOOT_SIZE: usize = 0x100;
const CGB_BOOT_SIZE: usize = 0x900;

impl BootConfig {
    pub fn parse(blob: &[u8]) -> Self {
        let mut c = BootConfig::default();
        if blob.len() >= 4 && &blob[..4] == b"GBBC" {
            let mut i = 4;
            while i + 8 <= blob.len() {
                let tag = &blob[i..i + 4];
                let len = u32::from_le_bytes([blob[i + 4], blob[i + 5], blob[i + 6], blob[i + 7]]) as usize;
                let Some(data) = blob.get(i + 8..i + 8 + len) else { break };
                match tag {
                    b"MODL" => c.model = data.first().map(|&m| match m {
                        1 => Model::Dmg, 2 => Model::Cgb, 3 => Model::Sgb, _ => Model::Auto }),
                    b"SKIP" => c.skip = data.first().is_some_and(|&v| v != 0),
                    b"DMG0" => c.add_rom(data),
                    b"CGB0" => c.add_rom(data),
                    b"SGB0" if data.len() == DMG_BOOT_SIZE => c.sgb = Some(data.to_vec()),
                    _ => {}
                }
                i += 8 + len;
            }
        } else if !blob.is_empty() {
            c.add_rom(blob);
        }
        c
    }

    fn add_rom(&mut self, d: &[u8]) {
        match d.len() {
            DMG_BOOT_SIZE => self.dmg = Some(d.to_vec()),
            CGB_BOOT_SIZE => self.cgb = Some(d.to_vec()),
            0 => {}
            n => println!("Game Boy boot ROM of {n} bytes ignored (DMG: 256, CGB: 2304)"),
        }
    }

    fn rom_for(&self, hw: GbVariant) -> (Vec<u8>, bool) {
        match hw {
            GbVariant::Cgb => match &self.cgb { Some(r) => (r.clone(), true), None => (OPEN_CGB_BOOT.to_vec(), false) },
            GbVariant::Sgb => match &self.sgb { Some(r) => (r.clone(), true), None => (OPEN_SGB_BOOT.to_vec(), false) },
            _ => match &self.dmg { Some(r) => (r.clone(), true), None => (OPEN_DMG_BOOT.to_vec(), false) },
        }
    }
}

/// Header byte 143: 80 = colour game that also runs on a DMG, C0 = colour only.
fn cart_wants_cgb(rom: &[u8]) -> bool { rom.get(0x143).is_some_and(|b| b & 0x80 != 0) }

/// Longest a boot ROM may take before we give up and start the cartridge directly
/// (a cartridge without a valid logo locks the boot ROM up, as on hardware).
const BOOT_TIMEOUT_FRAMES: usize = 600;

impl GbSystem {
    pub fn from_rom(rom_bytes: &[u8], base_name: &str, bios: &[u8]) -> Result<Self, String> {
        Self::build(rom_bytes, base_name, BootConfig::parse(bios))
    }

    /// `model`: "auto", "dmg" or "cgb" overrides the blob's choice ("" keeps it).
    pub fn from_rom_model(rom_bytes: &[u8], base_name: &str, bios: &[u8], model: &str) -> Result<Self, String> {
        let mut cfg = BootConfig::parse(bios);
        match model {
            "dmg" => cfg.model = Some(Model::Dmg),
            "cgb" => cfg.model = Some(Model::Cgb),
            "sgb" => cfg.model = Some(Model::Sgb),
            "auto" => cfg.model = Some(Model::Auto),
            _ => {}
        }
        Self::build(rom_bytes, base_name, cfg)
    }

    pub fn build(rom: &[u8], base_name: &str, cfg: BootConfig) -> Result<Self, String> {
        if rom.len() < 0x150 { return Err("ROM too small for a Game Boy cartridge".into()); }
        let hw = match cfg.model.unwrap_or(Model::Auto) {
            Model::Auto => if cart_wants_cgb(rom) { GbVariant::Cgb } else { GbVariant::Dmg },
            Model::Dmg => GbVariant::Dmg,
            Model::Cgb => GbVariant::Cgb,
            Model::Sgb => GbVariant::Sgb,
        };
        let (boot, real) = cfg.rom_for(hw);
        println!("Game Boy: {:?} hardware, {} boot ROM{}", hw, if real { "your" } else { "open-source (SameBoy)" },
                 if cfg.skip { ", skipping the animation" } else { "" });
        let mut sys = Self::new_machine(rom, base_name, hw, Some(&boot))?;
        sys.boot = cfg;
        if sys.boot.skip && !sys.finish_boot_rom() {
            println!("Boot ROM didn't finish (no valid logo?): starting the cartridge directly");
            let cfg = std::mem::take(&mut sys.boot);
            sys = Self::new_machine(rom, base_name, hw, None)?;
            sys.boot = cfg;
        }
        Ok(sys)
    }

    /// Starts the cartridge directly, without any boot ROM (tests).
    pub fn build_direct(rom: &[u8], base_name: &str, cfg: BootConfig) -> Result<Self, String> {
        let hw = match cfg.model.unwrap_or(Model::Auto) {
            Model::Auto => if cart_wants_cgb(rom) { GbVariant::Cgb } else { GbVariant::Dmg },
            Model::Dmg => GbVariant::Dmg,
            Model::Cgb => GbVariant::Cgb,
            Model::Sgb => GbVariant::Sgb,
        };
        let mut sys = Self::new_machine(rom, base_name, hw, None)?;
        sys.boot = cfg;
        Ok(sys)
    }

    /// Runs the boot ROM until it hands over to the cartridge. False if it never does.
    fn finish_boot_rom(&mut self) -> bool {
        let limit = self.bus.master + 70224 * BOOT_TIMEOUT_FRAMES as u64;
        while self.bus.boot_rom_mapped && self.bus.master < limit {
            self.tick();
        }
        let _ = self.drain_audio();
        self.refresh_frame_cache();
        !self.bus.boot_rom_mapped
    }

    /// A machine at power-on with `boot` mapped, or (None) in the state the boot ROM
    /// leaves behind, starting at the cartridge's entry point.
    fn new_machine(rom_bytes: &[u8], base_name: &str, variant: GbVariant, boot: Option<&[u8]>) -> Result<Self, String> {
        let bytes = rom_bytes;
        let run_boot_rom = boot.is_some();
        let buffer_size = 160 * 144 * 4;
        let cart_type_code = bytes[0x147];
        let cart_ram_size = Self::calculate_ram_size(bytes[0x149]) as usize;
        let cart_type = Self::determine_mbc(cart_type_code);
        let prg_rom = bytes.to_vec();
        let cart_ram = if cart_ram_size > 0 { vec![0; cart_ram_size] } else { vec![] };
        let header_title = std::str::from_utf8(&bytes[0x0134..0x0143]).unwrap_or("");
        let selected_palette = PaletteTheme::to_palette_set(PaletteTheme::Auto, header_title);
        let mbc = gb_mbc::make_mbc(cart_type.mbc_id, prg_rom.clone(), cart_ram, cart_type.has_rtc)?;
        let cartridge = GbCartridge::new(prg_rom, Vec::new(), mbc, base_name.to_string(), cart_type.has_battery);
        let mut bus = GameBoyBus::new(variant, cartridge, selected_palette, run_boot_rom, boot.unwrap_or(&[]));
        if run_boot_rom {
            bus.ppu.get_mut().power_on_registers();
        } else if variant == GbVariant::Cgb && !cart_wants_cgb(bytes) {
            // what the CGB boot ROM would leave for a DMG game: compatibility mode, with
            // the user's DMG colours as its palettes
            bus.set_cgb_mode(false);
            let to555 = |c: u32| -> u16 {
                let (r, g, b) = ((c >> 16) & 0xFF, (c >> 8) & 0xFF, c & 0xFF);
                ((r >> 3) | (g >> 3) << 5 | (b >> 3) << 10) as u16
            };
            let p = selected_palette;
            bus.ppu.get_mut().set_compat_palettes(p.bg.map(to555), p.obp0.map(to555), p.obp1.map(to555));
            bus.ppu.get_mut().write_register(0xFF6C, 1);
        }
        if variant == GbVariant::Sgb {
            // the PPU hands the SGB its shades 0-3 (stored in the blue channel)
            bus.ppu.get_mut().set_palette(SGB_SHADES);
            bus.sgb = Some(Sgb::new(Sgb::header_unlocks(bytes)));
        }
        let cgb_mode = bus.cgb_mode;
        Ok(Self {
            bus,
            cpu: GameBoyCpu::new(run_boot_rom, variant),
            save_filename: base_name.to_string(),
            slice_complete: false, cgb_mode,
            save_battery_path: "user://GD_EMU/GB/Save".to_string(),
            save_state_path: "user://GD_EMU/GB/State".to_string(),
            is_running: Arc::new(AtomicBool::new(false)),
            current_frame: 0,
            current_ui_theme: PaletteTheme::Auto,
            frame_cache: vec![0; buffer_size],
            cable: None, last_poll: 0,
            boot: BootConfig::default(),
        })
    }

    /// The SGB picture from the frame just finished.
    fn sgb_frame(&mut self) {
        if let Some(sgb) = &mut self.bus.sgb {
            let shades: Vec<u8> = self.frame_cache.chunks_exact(4).map(|p| p[2] & 3).collect();
            sgb.end_frame(&shades);
        }
    }

    /// Hardware this game is running on.
    pub fn hardware(&self) -> GbVariant { self.bus.variant }

    fn calculate_ram_size(header_byte: u8) -> u32 {
        match header_byte {
            0x00 => 0,
            0x01 => 2048,
            0x02 => 8192,
            0x03 => 32768,
            0x04 => 131072,
            0x05 => 65536,
            _=> 0,
        }
    }

    /// Cartridge header byte 147.
    fn determine_mbc(cart_type_code: u8) -> CartType {
        let t = |mbc_id, has_battery, has_rtc| CartType { mbc_id, has_battery, has_rtc };
        match cart_type_code {
            0x00 | 0x08 => t(0, false, false),
            0x09 => t(0, true, false),
            0x01 | 0x02 => t(1, false, false),
            0x03 => t(1, true, false),
            0x05 => t(2, false, false),
            0x06 => t(2, true, false),
            0x0F | 0x10 => t(3, true, true),
            0x11 | 0x12 => t(3, false, false),
            0x13 => t(3, true, false),
            0x19 | 0x1A | 0x1C | 0x1D => t(5, false, false),
            0x1B | 0x1E => t(5, true, false),
            _ => {
                println!("Cartridge type {:02X} isn't supported yet: running it without a mapper", cart_type_code);
                t(0, false, false)
            }
        }
    }

    fn is_running(&self) -> bool {
        self.is_running.load(Ordering::Relaxed)
    }

    pub fn tick(&mut self) {
        let before = self.bus.master;
        self.cpu.step_one_m_cycle(&mut self.bus);
        debug_assert_eq!(self.bus.master - before, self.bus.cycle_len(), 
                         "M-cycle must advance the clock exactly once");
    }

    /// Copy the PPU's last finished frame into frame_cache (the link pair uses this).
    pub fn refresh_frame_cache(&mut self) {
        self.frame_cache = {
            let ppu = self.bus.ppu.get_mut();
            let front_guard = ppu.front_buffer.lock().unwrap();
            front_guard.clone()
        };
        self.sgb_frame();
    }  
}

impl System for GbSystem {
    fn run_frame(&mut self, input: u32) {
        unsafe { (*self.bus.ppu.get()).clear_slice_complete_flag() };
        self.current_frame = self.current_frame.wrapping_add(1);
        self.set_input(input);
        while !unsafe { (*self.bus.ppu.get()).is_slice_complete() } {
            if !service_port(&mut self.bus.serial, &mut self.cable, self.bus.master, &mut self.last_poll) {
                return;   // waiting for the other player's byte: picture holds, emulated time doesn't move
            }
            self.tick();
        }
        self.bus.apply_cheat_writes();
        self.refresh_frame_cache();
    }

    // Function is setting bits high for pressed, gameboy expects bits to be low for pressed
    // So reading 0xFF00 needs to invert the result
    fn set_input(&mut self, input_mask: u32) {
        /*
        let mut gb_pad_state = 0u8;

        // Action Buttons (Bits 0..3 -> P10..P13 when P15 is low)
        if (input_mask & (1 << 5)) != 0 { gb_pad_state |= 1 << 0; } // A      -> Bit 0
        if (input_mask & (1 << 4)) != 0 { gb_pad_state |= 1 << 1; } // B      -> Bit 1
        if (input_mask & (1 << 6)) != 0 { gb_pad_state |= 1 << 2; } // Select -> Bit 2
        if (input_mask & (1 << 7)) != 0 { gb_pad_state |= 1 << 3; } // Start  -> Bit 3

        // Directional Buttons (Bits 4..7 -> P10..P13 when P14 is low)
        if (input_mask & (1 << 3)) != 0 { gb_pad_state |= 1 << 4; } // Right  -> Bit 4
        if (input_mask & (1 << 2)) != 0 { gb_pad_state |= 1 << 5; } // Left   -> Bit 5
        if (input_mask & (1 << 0)) != 0 { gb_pad_state |= 1 << 6; } // Up     -> Bit 6
        if (input_mask & (1 << 1)) != 0 { gb_pad_state |= 1 << 7; } // Down   -> Bit 7
        self.bus.pad1_state = gb_pad_state;
        */
        // Super Game Boy multiplayer: pad 2 is player 2
        self.bus.joypad.others[0] = (input_mask >> 24) as u8;
        self.bus.joypad.set_button_state(input_mask as u8, &mut self.bus.iflags);
    }

    fn drain_audio(&mut self) -> Vec<f32> {
        // Forward to the APU's drain_samples() through the bus.
        unsafe { (*self.bus.apu.get()).drain_samples() }
    }
 
    fn sample_rate(&self) -> u32 {
        44100   // whatever you constructed the APU with
    }

/*    fn is_frame_ready(&self) -> bool {
        self.frame_published.load(Ordering::Acquire)
    }
*/
    fn power_on(&mut self) {
        self.is_running.store(true, Ordering::SeqCst);
    }

    fn power_off(&mut self) {
        self.is_running.store(false, Ordering::SeqCst);
    }

    fn has_battery(&self) -> bool {
        self.bus.cartridge.has_battery
    }

    fn save_filename(&self) -> &str {
        &self.save_filename
    }

    fn save_battery_path(&self) -> &str {
        &self.save_battery_path
    }

    fn save_state_path(&self) -> &str {
        &self.save_state_path
    }

    fn is_sram_dirty(&self) -> bool {
        self.bus.is_sram_dirty()
    }

    fn clear_sram_dirty(&mut self) {
        self.bus.clear_sram_dirty();
    }

    fn get_current_frame(&self) -> i64 {
        self.current_frame as i64
    }

    fn get_sram(&self) -> Option<&[u8]> {
        self.bus.get_sram()
    }

    fn load_sram(&mut self, data: &[u8]) {
        self.bus.load_sram(data);
    }

    fn framebuffer(&self) -> &[u8] {
        match &self.bus.sgb {
            Some(s) => &s.framebuffer,
            None => &self.frame_cache,
        }
    }

    /// Power cycle: the same game, model and boot ROM; battery RAM, cheats and the link
    /// cable carry over.
    fn reset(&mut self) {
        let rom = self.bus.cartridge.prg_rom.clone();
        let Ok(mut fresh) = Self::build(&rom, &self.save_filename, self.boot.clone()) else { return };
        if let Some(sram) = self.bus.get_sram() {
            let sram = sram.to_vec();
            fresh.bus.load_sram(&sram);
        }
        std::mem::swap(&mut fresh.bus.cheats, &mut self.bus.cheats);
        fresh.cable = self.cable.take();
        fresh.save_battery_path = std::mem::take(&mut self.save_battery_path);
        fresh.save_state_path = std::mem::take(&mut self.save_state_path);
        fresh.is_running = self.is_running.clone();
        *self = fresh;
    }

    fn display_info(&self) -> DisplayInfo {
        if self.bus.sgb.is_some() {
            return DisplayInfo {
                buffer_width: SGB_W as i32,
                buffer_height: SGB_H as i32,
                visible_x: 0,
                visible_y: 0,
                visible_width: SGB_W as i32,
                visible_height: SGB_H as i32,
                target_aspect_ratio: 8.0 / 7.0,   // SNES pixels on a 4:3 TV
                rotation: Rotation::None,
            };
        }
        DisplayInfo {
            buffer_width: 160,
            buffer_height: 144,
            visible_x: 0,
            visible_y: 0,
            visible_width: 160,
            visible_height: 144,
            target_aspect_ratio: 10.0/9.0,
            rotation: Rotation::None,
        }
    }

    fn save_state(&self) -> Vec<u8> {
        #[derive(serde::Serialize)]
        struct Ref<'a> { cpu: &'a GameBoyCpu, bus: &'a GameBoyBus, mbc: Vec<u8>, frame: u64 }
        let mut out = STATE_MAGIC.to_vec();
        out.extend_from_slice(&STATE_VERSION.to_le_bytes());
        let r = Ref { cpu: &self.cpu, bus: &self.bus, mbc: self.bus.cartridge.mbc().save_state(),
                      frame: self.current_frame };
        match bincode::serde::encode_to_vec(&r, state_config()) {
            Ok(b) => { out.extend(b); out }
            Err(e) => { println!("GB save state failed: {e:?}"); Vec::new() }
        }
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        if data.len() < 8 || data[..4] != STATE_MAGIC[..] {
            return Err("not a Game Boy save state".into());
        }
        let v = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        if v != STATE_VERSION {
            return Err(format!("save state version {v}, this build uses {STATE_VERSION}"));
        }
        let (s, _): (GbState, usize) = bincode::serde::decode_from_slice(&data[8..], state_config())
            .map_err(|e| format!("couldn't decode save state: {e:?}"))?;
        let mut bus = s.bus;
        if bus.variant != self.bus.variant {
            return Err(format!("save state is for {:?} hardware, this game is running on {:?}",
                               bus.variant, self.bus.variant));
        }
        // the parts that aren't machine state come from this session
        std::mem::swap(&mut bus.cartridge, &mut self.bus.cartridge);
        bus.cartridge.mbc_mut().load_state(&s.mbc)?;
        bus.boot_rom = std::mem::take(&mut self.bus.boot_rom);
        std::mem::swap(&mut bus.cheats, &mut self.bus.cheats);
        bus.ppu.get_mut().active_palette = self.bus.ppu.get_mut().active_palette;
        if let (Some(new), Some(old)) = (&mut bus.sgb, &mut self.bus.sgb) {
            new.keep_static(old);
        }
        self.bus = bus;
        self.cpu = s.cpu;
        self.current_frame = s.frame;
        Ok(())
    }

    fn cheats_mut(&mut self) -> Option<&mut crate::common::cheats::Cheats> { Some(&mut self.bus.cheats) }

    fn attach_cable(&mut self, cable: Option<Box<dyn LinkCable>>) -> bool {
        self.cable = cable;
        true
    }

    fn cable_status(&self) -> String {
        self.cable.as_ref().map_or(String::new(), |c| c.status())
    }    
}


impl Snapshot for GbSystem {
    fn save(&self, out: &mut Vec<u8>) {
    }

    fn load(&mut self, cur: &mut Cursor<&[u8]>) {

    }
}

impl LinkUnit for GbSystem {
    fn serial(&mut self) -> &mut Serial { &mut self.bus.serial }
    fn clock(&self) -> u64 { self.bus.master }
    fn run_until(&mut self, target: u64) {
        while self.bus.master < target && self.bus.serial.awaiting.is_none() {
            self.tick();
        }
    }
}