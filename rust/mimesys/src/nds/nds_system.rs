//! Nintendo DS (2004): ARM946E-S at 67 MHz + ARM7TDMI at 33 MHz, two screens.
//!
//! Boot: direct, as emulators usually do: the cart header's ARM9 and ARM7 binaries are
//! copied to RAM and both CPUs start at their entry points with the state the firmware
//! would leave (header copy at 027FFE00, chip IDs, user settings at 027FFC80, CP15 and
//! stacks). The BIOS is the free replacement (BSD-2, Gilead Kutnick) unless real
//! bios9.bin / bios7.bin are given; the firmware is generated unless firmware.bin is.
//!
//! Timing: 263 lines of 2130 bus cycles (33.51 MHz), 59.83 Hz; the CPUs run in 32-cycle
//! slices so the IPC registers and FIFOs see each other promptly.
//!
//! Load options ("BIOS" bytes): b"NDSB", then entries of a 4-byte tag ("B9\0\0",
//! "B7\0\0", "FW\0\0"), a little-endian u32 length and the data.
//!
//! Controls: A, B, X, Y, L, R, Start, Select, D-pad; the mouse on the bottom screen is
//! the stylus (left button = touching).

use serde::{Deserialize, Serialize};
use crate::common::cpu::arm::arm7tdmi::Arm7tdmi;
use crate::common::input as pad;
use crate::common::system::{DisplayInfo, Rotation, System};
use crate::nds::nds_bus::*;
use crate::nds::nds_cart::{self, Cart, Header, Save, SaveType};
use crate::nds::nds_spi::{self, Spi};

const FREE_BIOS9: &[u8] = include_bytes!("free_bios9.bin");
const FREE_BIOS7: &[u8] = include_bytes!("free_bios7.bin");
const BUS_CLOCK: u64 = 33_513_982;
const SLICE: u64 = 32;
const STATE_VERSION: u32 = 3;
/// save chip for games missing from the save-type table (melonDS's guess too)
const DEFAULT_SAVE: (SaveType, usize) = (SaveType::Eeprom, 0x2000);

pub struct NdsSystem {
    pub cpu9: Arm7tdmi,
    pub cpu7: Arm7tdmi,
    pub nds: Nds,
    /// bus cycle the current line started at
    line_start: u64,
    current_frame: u64,
    audio_phase: u64,
    pub header: Header,
    pub notes: Vec<String>,
    save_filename: String,
    save_battery_path: String,
    save_state_path: String,
}

#[derive(Serialize)]
struct NdsStateRef<'a> {
    version: u32,
    code: u32,
    cpu9: &'a Arm7tdmi,
    cpu7: &'a Arm7tdmi,
    nds: &'a Nds,
    line_start: u64,
    frame: u64,
}

#[derive(Deserialize)]
struct NdsState {
    version: u32,
    code: u32,
    cpu9: Arm7tdmi,
    cpu7: Arm7tdmi,
    nds: Nds,
    line_start: u64,
    frame: u64,
}

/// The pieces in the load options.
fn option_entries(blob: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    if !blob.starts_with(b"NDSB") { return out; }
    let mut p = 4;
    while p + 8 <= blob.len() {
        let tag = [blob[p], blob[p + 1], blob[p + 2], blob[p + 3]];
        let n = u32::from_le_bytes([blob[p + 4], blob[p + 5], blob[p + 6], blob[p + 7]]) as usize;
        p += 8;
        if p + n > blob.len() { break; }
        out.push((tag, &blob[p..p + n]));
        p += n;
    }
    out
}

impl NdsSystem {
    pub fn from_rom(rom: &[u8], filename: &str, options: &[u8]) -> Result<Self, String> {
        let header = Header::parse(rom)?;
        let mut bios9 = FREE_BIOS9.to_vec();
        let mut bios7 = FREE_BIOS7.to_vec();
        let mut firmware = nds_spi::make_firmware();
        let mut real_bios7 = false;
        for (tag, data) in option_entries(options) {
            match &tag {
                b"B9\0\0" if data.len() >= 0x1000 => bios9 = data[..0x1000].to_vec(),
                b"B7\0\0" if data.len() >= 0x4000 => { bios7 = data[..0x4000].to_vec(); real_bios7 = true; }
                b"FW\0\0" if data.len() >= 0x20000 => firmware = data.to_vec(),
                _ => {}
            }
        }
        let (kind, size) = SaveType::for_game(header.game_code).unwrap_or(DEFAULT_SAVE);
        let save = Save::new(kind, size);
        let cart = Cart::new(rom.to_vec(), save);
        let spi = Spi::new(firmware);
        let mut sys = Self {
            cpu9: Arm7tdmi::new_v5(),
            cpu7: Arm7tdmi::new(),
            nds: Nds::new(cart, spi, bios9, bios7),
            line_start: 0,
            current_frame: 0,
            audio_phase: 0,
            header,
            notes: Vec::new(),
            save_filename: filename.to_string(),
            save_battery_path: "user://NDS/Save".to_string(),
            save_state_path: "user://NDS/State".to_string(),
        };
        sys.direct_boot(rom, real_bios7);
        println!("DS: \"{}\" ({}), {} MB{}", sys.header.title, sys.header.code_str(), rom.len() >> 20,
                 sys.notes.iter().map(|n| format!("; {n}")).collect::<String>());
        Ok(sys)
    }

    fn w32(&mut self, cpu: usize, a: u32, v: u32) { self.nds.write32(cpu, a, v, false); }
    fn w16(&mut self, cpu: usize, a: u32, v: u16) { self.nds.write16(cpu, a, v, false); }

    /// Set the machine up as the firmware leaves it when it starts a game.
    fn direct_boot(&mut self, rom: &[u8], real_bios7: bool) {
        let h = self.header.clone();
        let le = |a: usize| u32::from_le_bytes([rom[a], rom[a + 1], rom[a + 2], rom[a + 3]]);
        self.nds.wramcnt = 3;
        if self.nds.bios9.len() >= 0x20 + 0x9C && !real_bios7 {
            // the free BIOS gets the Nintendo logo from the header (DS-GBA checks)
            self.nds.bios9[0x20..0x20 + 0x9C].copy_from_slice(&rom[0xC0..0xC0 + 0x9C]);
        }
        for i in (0..0x170).step_by(4) { self.w32(ARM9, 0x027F_FE00 + i as u32, le(i)); }
        let id = self.nds.cart.chip_id;
        for base in [0x027F_F800u32, 0x027F_FC00] {
            self.w32(ARM9, base, id);
            self.w32(ARM9, base + 4, id);
            self.w16(ARM9, base + 8, h.header_crc);
            self.w16(ARM9, base + 10, h.secure_crc);
            self.w16(ARM9, base + 0x10 + if base == 0x027F_F800 { 0x40 } else { 0 }, 0x5835);
        }
        self.w16(ARM9, 0x027F_FC30, 0xFFFF);
        self.w16(ARM9, 0x027F_FC40, 0x0001);
        // user settings, as the firmware copies them
        let fw = &self.nds.spi.firmware;
        let user_at = (u16::from_le_bytes([fw[0x20], fw[0x21]]) as usize) * 8;
        let user: Vec<u8> = fw.get(user_at..user_at + 0x70).map(|s| s.to_vec()).unwrap_or_else(|| nds_spi::user_settings()[..0x70].to_vec());
        for i in (0..0x70).step_by(4) {
            let v = u32::from_le_bytes([user[i], user[i + 1], user[i + 2], user[i + 3]]);
            self.w32(ARM9, 0x027F_FC80 + i as u32, v);
        }

        // ARM9 code, with the secure area (the first 2 KB) decrypted
        let mut start = 0u32;
        if (0x4000..0x8000).contains(&h.arm9_rom) {
            let mut area = rom[h.arm9_rom as usize..h.arm9_rom as usize + 0x800].to_vec();
            if !nds_cart::secure_area_plain(rom, h.arm9_rom) {
                let ok = real_bios7 && nds_cart::decrypt_secure_area(&mut area, h.game_code, &self.nds.bios7);
                if !ok {
                    self.notes.push("the secure area is encrypted and needs a real bios7.bin to decrypt: the game will probably crash".into());
                }
            }
            for i in (0..0x800).step_by(4) {
                let v = u32::from_le_bytes([area[i], area[i + 1], area[i + 2], area[i + 3]]);
                self.w32(ARM9, h.arm9_ram + i as u32, v);
            }
            start = 0x800;
        }
        for i in (start..h.arm9_size).step_by(4) { self.w32(ARM9, h.arm9_ram + i, le((h.arm9_rom + i) as usize)); }
        for i in (0..h.arm7_size).step_by(4) { self.w32(ARM7, h.arm7_ram + i, le((h.arm7_rom + i) as usize)); }

        for (reg, v) in [
            (0x0100, 0x0005_2078), (0x0200, 0x42), (0x0201, 0x42), (0x0300, 0x02), (0x0502, 0x1511_1011),
            (0x0503, 0x0510_0011), (0x0600, 0x0400_0033), (0x0601, 0x0400_0033), (0x0610, 0x0200_002B),
            (0x0611, 0x0200_002B), (0x0630, 0x0800_0035), (0x0631, 0x0800_0035), (0x0640, 0x0300_001B),
            (0x0641, 0x0300_001B), (0x0660, 0xFFFF_001D), (0x0661, 0xFFFF_001D), (0x0670, 0x027F_F017),
            (0x0671, 0x027F_F017), (0x0910, 0x0300_000A), (0x0911, 0x0000_0020),
        ] {
            self.nds.cp15.write(reg, v);
        }
        self.nds.cycles9 = 0;
        self.nds.cycles7 = 0;

        for (cpu, entry, sp, sp_irq, sp_svc) in [
            (ARM9, h.arm9_entry, 0x0300_2F7Cu32, 0x0300_3F80u32, 0x0300_3FC0u32),
            (ARM7, h.arm7_entry, 0x0380_FD80, 0x0380_FF80, 0x0380_FFC0),
        ] {
            let c = if cpu == ARM9 { &mut self.cpu9 } else { &mut self.cpu7 };
            c.set_cpsr(0xDF);
            c.r[12] = entry;
            c.r[13] = sp;
            c.r[14] = entry;
            c.bank_sp_lr[3][0] = sp_irq;      // IRQ bank
            c.bank_sp_lr[1][0] = sp_svc;      // SVC bank
            if cpu == ARM9 { c.jump(&mut Arm9Bus(&mut self.nds), entry) } else { c.jump(&mut Arm7Bus(&mut self.nds), entry) }
        }
        self.nds.exmemcnt = 0xE880;
        self.nds.postflg = [1, 1];
        self.nds.powcnt1 = 0x820F;
        self.nds.powcnt2 = 0x0001;
    }

    /// Every 10 seconds: where the CPUs are (helps tell a running game from a stuck one).
    fn report(&self) {
        let st = |h: bool| if h { " (waiting for an IRQ)" } else { "" };
        println!("DS frame {}: ARM9 at {:08X}{}, ARM7 at {:08X}{}, DISPCNT A {:08X}, B {:08X}",
                 self.current_frame, self.cpu9.pc(), st(self.nds.halted[ARM9]), self.cpu7.pc(), st(self.nds.halted[ARM7]),
                 u32::from_le_bytes(self.nds.io9_raw[0..4].try_into().unwrap()),
                 u32::from_le_bytes(self.nds.io9_raw[0x1000..0x1004].try_into().unwrap()));
        // the 2D setup of both engines, for screenshots that miss a layer
        let r16 = |o: usize| u16::from_le_bytes([self.nds.io9_raw[o], self.nds.io9_raw[o + 1]]);
        for (name, b) in [("A", 0usize), ("B", 0x1000)] {
            println!("  engine {name}: BGCNT {:04X} {:04X} {:04X} {:04X}, BLDCNT {:04X}, BLDALPHA {:04X}, WININ {:04X}, WINOUT {:04X}, BRIGHT {:04X}",
                     r16(b + 8), r16(b + 0xA), r16(b + 0xC), r16(b + 0xE), r16(b + 0x50), r16(b + 0x52), r16(b + 0x48), r16(b + 0x4A), r16(b + 0x6C));
        }
        let c = &self.nds.vram.cnt;
        println!("  VRAMCNT A-I {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X}, POWCNT1 {:04X}, IE9 {:08X}, IE7 {:08X}",
                 c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7], c[8], self.nds.powcnt1, self.nds.irq[ARM9].ie, self.nds.irq[ARM7].ie);
        println!("  IF9 {:08X}, IF7 {:08X}, IME {}/{}, IPCSYNC {:08X}/{:08X}, IPCFIFOCNT {:08X}/{:08X}",
                 self.nds.irq[ARM9].iflags, self.nds.irq[ARM7].iflags, self.nds.irq[ARM9].ime, self.nds.irq[ARM7].ime,
                 self.nds.ipc.read_sync(ARM9), self.nds.ipc.read_sync(ARM7), self.nds.ipc.read_cnt(ARM9), self.nds.ipc.read_cnt(ARM7));
        // where each CPU is: registers, the top of its stack, and the code it's running (or,
        // inside the BIOS, the code that called it: the first RAM address on the stack)
        for (name, cpu, core) in [("ARM9", ARM9, &self.cpu9), ("ARM7", ARM7, &self.cpu7)] {
            let r = &core.r;
            println!("  {name} r0-r15 {}", r.iter().map(|v| format!("{v:08X}")).collect::<Vec<_>>().join(" "));
            let stack: Vec<u32> = (0..12).map(|k| self.nds.peek32(cpu, r[13].wrapping_add(k * 4))).collect();
            println!("  {name} cpsr {:08X} stack {}", core.cpsr, stack.iter().map(|v| format!("{v:08X}")).collect::<Vec<_>>().join(" "));
            let pc = core.pc();
            let in_bios = if cpu == ARM9 { pc >= 0xFFFF_0000 } else { pc < 0x4000 };
            let is_ram = |a: u32| (0x0200_0000..0x0300_0000).contains(&a) || (0x0300_0000..0x0400_0000).contains(&a) || a < 0x8000 && cpu == ARM9;
            let focus = if in_bios {
                std::iter::once(r[14]).chain(stack.iter().copied()).find(|&a| is_ram(a & !1)).unwrap_or(pc)
            } else { pc };
            let start = (focus & !3).wrapping_sub(0x40);
            let code: Vec<String> = (0..24).map(|k| format!("{:08X}", self.nds.peek32(cpu, start.wrapping_add(k * 4)))).collect();
            println!("  {name} code at {start:08X} (around {focus:08X}): {}", code.join(" "));
        }
    }

    /// Run both CPUs up to absolute bus cycle `target`.
    fn run_to(&mut self, target: u64) {
        let mut now = self.nds.cycles7.min(self.nds.cycles9 / 2);
        while now < target {
            let t = (now + SLICE).min(target);
            // ARM9: twice the clock
            while self.nds.cycles9 < t * 2 {
                if self.nds.halted[ARM9] {
                    if self.nds.irq[ARM9].pending() { self.nds.halted[ARM9] = false; } else { self.nds.cycles9 = t * 2; break; }
                }
                self.cpu9.irq_line = self.nds.irq[ARM9].line();
                let before = self.nds.cycles9;
                self.cpu9.step(&mut Arm9Bus(&mut self.nds));
                if self.nds.cycles9 == before { self.nds.cycles9 += 1; }
            }
            while self.nds.cycles7 < t {
                if self.nds.halted[ARM7] {
                    if self.nds.irq[ARM7].pending() { self.nds.halted[ARM7] = false; } else { self.nds.cycles7 = t; break; }
                }
                self.cpu7.irq_line = self.nds.irq[ARM7].line();
                let before = self.nds.cycles7;
                self.cpu7.step(&mut Arm7Bus(&mut self.nds));
                if self.nds.cycles7 == before { self.nds.cycles7 += 1; }
            }
            self.nds.update_timers(ARM9);
            self.nds.update_timers(ARM7);
            now = t;
        }
    }
}

impl System for NdsSystem {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        for line in 0..LINES {
            self.nds.start_line(line);
            self.run_to(self.line_start + HBLANK_CYCLE);
            self.nds.hblank(line);
            if (line as usize) < SCREEN_H { self.nds.render_line(line as usize); }
            self.run_to(self.line_start + LINE_CYCLES);
            self.line_start += LINE_CYCLES;
        }
        self.current_frame += 1;
        if self.current_frame % 600 == 0 { self.report(); }
    }

    fn set_input(&mut self, m: u32) {
        let on = |b: u32| m & b != 0;
        let mut k = 0x3FFu16;
        for (i, b) in [pad::A, pad::B, pad::SELECT, pad::START, pad::RIGHT, pad::LEFT, pad::UP, pad::DOWN, pad::R, pad::L].iter().enumerate() {
            if on(*b) { k &= !(1 << i); }
        }
        self.nds.keyinput = k;
        let mut e = 0x7Fu16;
        if on(pad::X) { e &= !0x01; }
        if on(pad::Y) { e &= !0x02; }
        if self.nds.spi.touch.2 { e &= !0x40; }
        self.nds.extkeyin = e;
    }

    fn set_pointer(&mut self, x: i32, y: i32, buttons: u32) {
        let on_bottom = (0..256).contains(&x) && (192..384).contains(&y);
        self.nds.spi.touch = if buttons & 1 != 0 && on_bottom { (x as u16, (y - 192) as u16, true) } else { (0, 0, false) };
        let mut e = self.nds.extkeyin | 0x40;
        if self.nds.spi.touch.2 { e &= !0x40; }
        self.nds.extkeyin = e;
    }

    fn set_option(&mut self, key: &str, value: i64) {
        if key == "rtc_utc_offset" { self.nds.rtc.utc_offset = value as i32; }
    }

    /// No sound yet (stage 5): silence at the right rate.
    fn drain_audio(&mut self) -> Vec<f32> {
        self.audio_phase += 44_100 * self.frame_time_usec() as u64;
        let n = self.audio_phase / 1_000_000;
        self.audio_phase %= 1_000_000;
        vec![0.0; n as usize * 2]
    }
    fn sample_rate(&self) -> u32 { 44_100 }
    fn frame_time_usec(&self) -> i64 { (LINES as u64 * LINE_CYCLES * 1_000_000 / BUS_CLOCK) as i64 }
    fn power_on(&mut self) {}
    fn power_off(&mut self) {}
    fn has_battery(&self) -> bool { self.nds.cart.save.kind != SaveType::None }
    fn save_filename(&self) -> &str { &self.save_filename }
    fn save_battery_path(&self) -> &str { &self.save_battery_path }
    fn save_state_path(&self) -> &str { &self.save_state_path }
    fn is_sram_dirty(&self) -> bool { self.nds.cart.save.dirty }
    fn clear_sram_dirty(&mut self) { self.nds.cart.save.dirty = false; }
    fn get_current_frame(&self) -> i64 { self.current_frame as i64 }
    fn get_sram(&self) -> Option<&[u8]> { Some(&self.nds.cart.save.data) }
    fn load_sram(&mut self, data: &[u8]) {
        // listed games keep their chip (a file of another size is cut or padded with FF);
        // otherwise the file's size says which chip it was
        let (kind, size) = match SaveType::for_game(self.header.game_code) {
            Some(k) => k,
            None => (SaveType::from_size(data.len()), data.len()),
        };
        if kind != SaveType::None {
            let mut save = Save::new(kind, size);
            let n = size.min(data.len());
            save.data[..n].copy_from_slice(&data[..n]);
            self.nds.cart.save = save;
        }
    }

    fn framebuffer(&self) -> &[u8] { &self.nds.framebuffer }

    fn reset(&mut self) {
        let rom = std::mem::take(&mut self.nds.cart.rom);
        let save = self.nds.cart.save.clone();
        let firmware = std::mem::take(&mut self.nds.spi.firmware);
        let (b9, b7) = (std::mem::take(&mut self.nds.bios9), std::mem::take(&mut self.nds.bios7));
        let real7 = b7 != FREE_BIOS7;
        self.nds = Nds::new(Cart::new(rom.clone(), save), Spi::new(firmware), b9, b7);
        self.cpu9 = Arm7tdmi::new_v5();
        self.cpu7 = Arm7tdmi::new();
        self.line_start = 0;
        self.direct_boot(&rom, real7);
        self.nds.cart.rom = rom;
    }

    fn display_info(&self) -> DisplayInfo {
        DisplayInfo {
            buffer_width: SCREEN_W as i32,
            buffer_height: (SCREEN_H * 2) as i32,
            visible_x: 0,
            visible_y: 0,
            visible_width: SCREEN_W as i32,
            visible_height: (SCREEN_H * 2) as i32,
            target_aspect_ratio: SCREEN_W as f32 / (SCREEN_H * 2) as f32,
            rotation: Rotation::None,
        }
    }

    fn save_state(&self) -> Vec<u8> {
        let s = NdsStateRef {
            version: STATE_VERSION, code: self.header.game_code, cpu9: &self.cpu9, cpu7: &self.cpu7,
            nds: &self.nds, line_start: self.line_start, frame: self.current_frame,
        };
        let config = bincode::config::standard().with_fixed_int_encoding();
        bincode::serde::encode_to_vec(&s, config).unwrap_or_default()
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let config = bincode::config::standard().with_fixed_int_encoding();
        let (s, _): (NdsState, usize) = bincode::serde::decode_from_slice(data, config)
            .map_err(|e| format!("Failed to decode DS save state: {e:?}"))?;
        if s.version != STATE_VERSION { return Err(format!("save state version {} (expected {STATE_VERSION})", s.version)); }
        if s.code != self.header.game_code { return Err("save state is from a different game".into()); }
        let mut nds = s.nds;
        nds.cart.rom = std::mem::take(&mut self.nds.cart.rom);
        nds.spi.firmware = std::mem::take(&mut self.nds.spi.firmware);
        nds.bios9 = std::mem::take(&mut self.nds.bios9);
        nds.bios7 = std::mem::take(&mut self.nds.bios7);
        nds.framebuffer = std::mem::take(&mut self.nds.framebuffer);
        nds.vram.after_load();
        self.nds = nds;
        self.cpu9 = s.cpu9;
        self.cpu7 = s.cpu7;
        self.line_start = s.line_start;
        self.current_frame = s.frame;
        Ok(())
    }
}
