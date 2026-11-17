//! Sony PlayStation. The plan is in the project's `ps1_plan.md`; this is stages 1-7: the
//! R3000A and GTE (`common/cpu/r3000`), the memory map, interrupts, DMA and timers
//! (`bus.rs`), the GPU (`gpu.rs`), digital pads (`sio.rs`), the CD-ROM with XA-ADPCM and
//! CD-DA (`cdrom.rs`), the SPU (`spu.rs`, 44100 Hz stereo out) and the MDEC (`mdec.rs`).
//!
//! - Video timing (NTSC): 263 lines of 3413 GPU clocks (53.69 MHz), about 59.83 Hz; PAL
//!   314 lines of 3406 (53.22 MHz). Vblank runs from the display's vertical range end to its start.
//! - Loading: a disc image (.cue / .iso / .chd: the frontend sends its path), or the BIOS
//!   file alone (the shell, no disc). A PS-EXE is
//!   side-loaded once the BIOS reaches its shell entry (80030000), as other emulators do.
//! - The BIOS's TTY output (putchar, A(3C) / B(3D)) is printed to the log.

pub mod bus;
pub mod cdrom;
pub mod gpu;
pub mod mdec;
pub mod sio;
pub mod spu;
pub mod memcard;
pub mod pad;

/// The debug log (option "ps1_debug"): CD commands, DMA / MDEC / CD state every 5 seconds,
/// the instructions before a crash
static DEBUG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn debug() -> bool { DEBUG.load(std::sync::atomic::Ordering::Relaxed) }
pub mod timers;

#[cfg(test)]
mod tests;

use crate::common::cpu::r3000::R3000;
use crate::common::input;
use crate::common::system::{DisplayInfo, System};
use bus::{Bus, BIOS_SIZE, IRQ_VBLANK};
use crate::common::disc::Disc;
use serde::{Deserialize, Serialize};

const STATE_VERSION: u32 = 6;
/// tick the devices in batches of about this many cycles
const BATCH: u32 = 32;
const CPU_HZ: u64 = 33_868_800;

/// A PS-EXE: "PS-X EXE", entry, gp, load address and size, stack base and size
#[derive(Clone, Serialize, Deserialize)]
pub struct Exe {
    pub pc: u32,
    pub gp: u32,
    pub addr: u32,
    pub data: Vec<u8>,
    pub sp: u32,
}

impl Exe {
    pub fn parse(d: &[u8]) -> Option<Exe> {
        if d.len() < 0x800 || &d[0..8] != b"PS-X EXE" { return None; }
        let w = |o: usize| u32::from_le_bytes(d[o..o + 4].try_into().unwrap());
        let size = (w(0x1C) as usize).min(d.len() - 0x800);
        let sp = if w(0x30) != 0 { w(0x30).wrapping_add(w(0x34)) } else { 0 };
        Some(Exe { pc: w(0x10), gp: w(0x14), addr: w(0x18), data: d[0x800..0x800 + size].to_vec(), sp })
    }
}

/// Whether a disc is a PlayStation one: its ISO 9660 volume descriptor (sector 16) says
/// "PLAYSTATION" as the system.
pub fn is_ps1_disc(d: &mut Disc) -> bool {
    d.tracks.first().is_some_and(|t| !t.is_audio()) && !d.gd
        && d.read_data(16).is_some_and(|s| &s[1..6] == b"CD001" && s[8..19] == *b"PLAYSTATION")
}

/// Open the disc in the frontend's "MCDP" + path blob; an error if it isn't a PS1 disc.
pub fn open_disc(blob: &[u8]) -> Result<Disc, String> {
    let path = crate::segacd::disc_path(blob).ok_or("not a disc image path")?;
    let mut d = Disc::open(&path)?;
    if !is_ps1_disc(&mut d) { return Err("not a PlayStation disc".into()); }
    Ok(d)
}

pub fn is_bios(data: &[u8], file: &str) -> bool {
    let f = file.to_ascii_lowercase();
    data.len() == BIOS_SIZE && (f.starts_with("scph") || f.contains("psx") || f.contains("ps1") || f.contains("playstation")
        || data.windows(27).take(0x1000).any(|w| w == b"Sony Computer Entertainment"))
}

pub struct Ps1System {
    pub cpu: R3000,
    pub bus: Bus,
    exe: Option<Exe>,
    frame: Vec<u8>,
    frame_w: usize,
    frame_h: usize,
    line_frac: u64,
    owed: i64,
    /// bring-up: hits per watched pc, and Breath of Fire III's last VLC output address
    /// the extra input word (R2, L3, R3, Analog), the Analog button held, rumble allowed
    ext: u32,
    analog_held: bool,
    rumble_on: bool,
    pad_kind: i64,
    /// tests: how often each pc runs
    prof: Option<std::collections::HashMap<u32, u64>>,
    last_istat: u32,
    tty_line: String,
    current_frame: u64,
    save_filename: String,
    save_path: String,
}

impl Ps1System {
    pub fn from_rom(rom: &[u8], filename: &str, bios: &[u8]) -> Result<Self, String> {
        let name = filename.rsplit(['/', '\\']).next().unwrap_or(filename);
        // a disc image: the frontend sends its path
        let disc = if rom.starts_with(crate::segacd::PATH_MAGIC) { Some(open_disc(rom)?) } else { None };
        let (bios, exe) = if bios.len() == BIOS_SIZE {
            (bios.to_vec(), Exe::parse(rom))
        } else if is_bios(rom, name) {
            (rom.to_vec(), None)
        } else {
            return Err("PlayStation needs its BIOS: put a 512 KB BIOS (e.g. scph1001.bin) in the BIOS folder".into());
        };
        if exe.is_none() && disc.is_none() && rom != bios.as_slice() && !rom.is_empty() {
            println!("PS1: {name} isn't a PS-EXE or a disc; starting the BIOS");
        }
        let stem = name.rsplit_once('.').map_or(name, |(s, _)| s).to_string();
        let mut s = Self {
            cpu: R3000::new(),
            bus: Bus::new(bios),
            exe,
            frame: vec![0; 320 * 240 * 4],
            frame_w: 320,
            frame_h: 240,
            line_frac: 0,
            owed: 0,
            prof: None,
            ext: 0,
            analog_held: false,
            rumble_on: true,
            pad_kind: 0,
            last_istat: 0,
            tty_line: String::new(),
            current_frame: 0,
            save_filename: stem,
            save_path: "user://GD_EMU/PS1/State".to_string(),
        };
        s.frame.chunks_exact_mut(4).for_each(|p| p[3] = 0xFF);
        if let Some(d) = disc {
            println!("PS1: disc with {} track(s), {} sectors", d.tracks.len(), d.leadout);
            s.bus.cdrom = cdrom::Cdrom::new(Some(d), bus::bios_region(&s.bus.bios));
        }
        Ok(s)
    }

    fn pal(&self) -> bool { self.bus.gpu.pal() }

    fn lines(&self) -> u32 { if self.pal() { 314 } else { 263 } }

    /// The BIOS's putchar calls, and the PS-EXE side-load at the shell's entry
    fn hooks(&mut self) {
        let pc = self.cpu.pc & 0x1FFF_FFFF;
        let r9 = self.cpu.regs[9];
        if (pc == 0xA0 && r9 == 0x3C) || (pc == 0xB0 && r9 == 0x3D) {
            self.tty(self.cpu.regs[4] as u8);
        }
        if pc == 0x0003_0000 {
            if let Some(exe) = self.exe.take() {
                let base = (exe.addr & 0x1F_FFFF) as usize;
                let end = (base + exe.data.len()).min(self.bus.ram.len());
                self.bus.ram[base..end].copy_from_slice(&exe.data[..end - base]);
                self.cpu.regs[28] = exe.gp;
                if exe.sp != 0 {
                    self.cpu.regs[29] = exe.sp;
                    self.cpu.regs[30] = exe.sp;
                }
                self.cpu.jump(exe.pc);
                println!("PS1: started the PS-EXE at {:08X}", exe.pc);
            }
        }
    }

    fn tty(&mut self, c: u8) {
        if c == b'\n' {
            println!("PS1 TTY: {}", self.tty_line);
            self.tty_line.clear();
        } else if c != b'\r' && self.tty_line.len() < 512 {
            self.tty_line.push(c as char);
        }
    }

    fn run_cycles(&mut self, n: i64) {
        self.owed += n;
        let mut batch = 0u32;
        while self.owed > 0 {
            self.hooks();
            self.cpu.set_irq(self.bus.irq_line());
            if let Some(p) = &mut self.prof {
                *p.entry(self.cpu.pc).or_default() += 1;
                // interrupts raised: counted under FFFF00nn
                let new = self.bus.i_stat & !self.last_istat;
                self.last_istat = self.bus.i_stat;
                for b in 0..11 { if new & (1 << b) != 0 { *p.entry(0xFFFF_0000 + b).or_default() += 1; } }
            }
            let c = self.cpu.step(&mut self.bus) + std::mem::take(&mut self.bus.wait);
            self.owed -= c as i64;
            batch += c;
            if batch >= BATCH {
                self.bus.tick(batch);
                batch = 0;
            }
        }
        if batch > 0 { self.bus.tick(batch); }
        for c in std::mem::take(&mut self.bus.tty) { self.tty(c); }
    }

    fn run_line(&mut self, line: u32) {
        // GPU clocks per line in CPU cycles: 3413 x 33.8688 / 53.693175 MHz (NTSC)
        let (gpu_clocks, gpu_hz) = if self.pal() { (3406u64, 53_222_400u64) } else { (3413, 53_693_175) };
        self.line_frac += gpu_clocks * CPU_HZ;
        let cycles = (self.line_frac / gpu_hz) as u32;
        self.line_frac %= gpu_hz;
        let (vstart, vend) = self.bus.gpu.v_range;
        let total = self.lines();
        let vend = vend.clamp(vstart + 1, total - 1);
        let in_vblank = line < vstart || line >= vend;
        if line == vend { self.bus.raise(IRQ_VBLANK); }
        self.bus.timers.in_vblank = in_vblank;
        self.bus.gpu.odd_line = if in_vblank { false } else if self.bus.gpu.mode & 0x20 != 0 { self.bus.gpu.field } else { line & 1 == 1 };
        // the visible part of the line, then hblank
        self.bus.timers.in_hblank = false;
        self.run_cycles((cycles * 5 / 6) as i64);
        self.bus.timers.in_hblank = true;
        self.bus.timers.hblank();
        self.run_cycles((cycles - cycles * 5 / 6) as i64);
    }
}

impl System for Ps1System {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        for line in 0..self.lines() {
            self.run_line(line);
        }
        if self.bus.gpu.mode & 0x20 != 0 { self.bus.gpu.field = !self.bus.gpu.field; }
        // debug log (option "ps1_debug"): where the CPU is and what the chips do, every 5 s
        if debug() && self.current_frame % 300 == 299 {
            let c = &self.cpu;
            crate::dlog!("PS1: frame {} pc {:08X} ra {:08X} sr {:08X} I_STAT {:03X} I_MASK {:03X} GPU mode {:02X}",
                self.current_frame + 1, c.pc, c.regs[31], c.cop0[12], self.bus.i_stat, self.bus.i_mask, self.bus.gpu.mode);
            let b = &self.bus;
            crate::dlog!("PS1:   DICR {:08X} DPCR {:08X} CHCR0 {:08X} CHCR1 {:08X} CHCR2 {:08X} CHCR3 {:08X} | MDEC {} | CD {} | GPU disp {},{}{}",
                b.dicr, b.dpcr, b.dma[0].chcr, b.dma[1].chcr, b.dma[2].chcr, b.dma[3].chcr, b.mdec.debug(), b.cdrom.debug(),
                b.gpu.disp_x, b.gpu.disp_y, if b.gpu.display_off { " off" } else { "" });
        }
        let (w, h) = self.bus.gpu.render(&mut self.frame);
        self.frame_w = w;
        self.frame_h = h;
        self.current_frame += 1;
    }

    fn framebuffer(&self) -> &[u8] { &self.frame }

    fn display_info(&self) -> DisplayInfo {
        DisplayInfo {
            buffer_width: self.frame_w as i32,
            buffer_height: self.frame_h as i32,
            visible_x: 0,
            visible_y: 0,
            visible_width: self.frame_w as i32,
            visible_height: self.frame_h as i32,
            target_aspect_ratio: 4.0 / 3.0,
            ..Default::default()
        }
    }

    fn drain_audio(&mut self) -> Vec<f32> { std::mem::take(&mut self.bus.spu.out) }
    fn sample_rate(&self) -> u32 { 44_100 }

    /// The digital pad, active low: Select, -, -, Start, Up, Right, Down, Left, L2, R2, L1,
    /// R1, Triangle, Circle, Cross, Square. B = Cross, A = Circle, X = Square, Y = Triangle,
    /// L / R = L1 / R1, Z = L2 (R2 has no button yet).
    fn set_input(&mut self, input: u32) {
        let ext = self.ext;
        let pads = input::pads(input, ext);
        // the Analog button toggles on its press
        let analog_btn = ext & input::EXT_P1_ANALOG != 0;
        if analog_btn && !self.analog_held { self.bus.sio.pads[0].analog_button(); }
        self.analog_held = analog_btn;
        for k in 0..2 {
            let p = &pads[k];
            let mut v = 0u16;
            let (r2, l3, r3) = if k == 0 {
                (ext & input::EXT_P1_R2 != 0, ext & input::EXT_P1_L3 != 0, ext & input::EXT_P1_R3 != 0)
            } else {
                (false, false, false)
            };
            let mut dir = [p.up, p.right, p.down, p.left];
            // in analog mode the left stick isn't the d-pad: drop the d-pad presses the
            // frontend made from the stick (the stick leaning the same way)
            let sio = &self.bus.sio.pads[k];
            if sio.analog {
                let [_, _, lx, ly] = sio.axes;
                let lean = |v: u8, neg: bool| if neg { v < 0x80 - 20 } else { v > 0x80 + 20 };
                let leaning = [lean(ly, true), lean(lx, false), lean(ly, false), lean(lx, true)];
                for d in 0..4 { if leaning[d] { dir[d] = false; } }
            }
            let bits = [
                (p.coin, 0), (l3, 1), (r3, 2), (p.start, 3), (dir[0], 4), (dir[1], 5), (dir[2], 6), (dir[3], 7),
                (p.z, 8), (r2, 9), (p.l, 10), (p.r, 11), (p.y, 12), (p.a, 13), (p.b, 14), (p.x, 15),
            ];
            for (on, b) in bits { if on { v |= 1 << b; } }
            self.bus.sio.pads[k].buttons = !v;
        }
    }

    fn set_extra_input(&mut self, ext: u32) { self.ext = ext; }

    fn set_analog(&mut self, port: usize, sticks: [u8; 4]) {
        if let Some(p) = self.bus.sio.pads.get_mut(port) {
            let [lx, ly, rx, ry] = sticks;
            p.axes = [rx, ry, lx, ly];
        }
    }

    fn rumble(&self, port: usize) -> (u8, u8) {
        if !self.rumble_on { return (0, 0); }
        self.bus.sio.pads.get(port).map_or((0, 0), |p| (if p.small { 0xFF } else { 0 }, p.large))
    }

    fn save_state(&self) -> Vec<u8> {
        let s = (&self.cpu, &self.bus, self.line_frac, self.owed, self.current_frame);
        crate::arcade::state::encode(&self.save_filename, STATE_VERSION, &s)
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let (cpu, mut bus, lf, owed, frame): (R3000, Bus, u64, i64, u64) =
            crate::arcade::state::decode(&self.save_filename, STATE_VERSION, data)?;
        if bus.ram.len() != self.bus.ram.len() || bus.gpu.vram.len() != self.bus.gpu.vram.len() || cpu.scratch.len() != 0x400 {
            return Err("save state doesn't fit".into());
        }
        bus.bios = std::mem::take(&mut self.bus.bios);
        bus.cdrom.disc = self.bus.cdrom.disc.take();
        for k in 0..2 {
            bus.sio.pads[k].buttons = self.bus.sio.pads[k].buttons;
            bus.sio.pads[k].axes = self.bus.sio.pads[k].axes;
            bus.sio.pads[k].dualshock = self.bus.sio.pads[k].dualshock;
        }
        bus.sio.connected = self.bus.sio.connected;
        bus.sio.cards = std::mem::take(&mut self.bus.sio.cards);
        self.cpu = cpu;
        self.bus = bus;
        self.line_frac = lf;
        self.owed = owed;
        self.current_frame = frame;
        Ok(())
    }

    fn load_sram(&mut self, _data: &[u8]) {}
    fn get_sram(&self) -> Option<&[u8]> { None }

    fn reset(&mut self) {
        let bios = std::mem::take(&mut self.bus.bios);
        let disc = self.bus.cdrom.disc.take();
        let region = bus::bios_region(&bios);
        self.bus = Bus::new(bios);
        if disc.is_some() { self.bus.cdrom = cdrom::Cdrom::new(disc, region); }
        self.cpu = R3000::new();
        self.owed = 0;
    }

    fn has_battery(&self) -> bool { false }

    fn set_option(&mut self, key: &str, value: i64) {
        match key {
            "debug_report" => {
                DEBUG.store(value != 0, std::sync::atomic::Ordering::Relaxed);
                self.cpu.set_trace(value != 0);
            }
            // 0 DualShock (powers up digital), 1 DualShock in analog mode, 2 digital pad
            // applied once per setting, so re-applying options keeps the game's mode
            "ps1_pad" => if value != self.pad_kind {
                self.pad_kind = value;
                for p in self.bus.sio.pads.iter_mut() {
                    p.dualshock = value != 2;
                    p.analog = value == 1;
                }
            },
            "ps1_rumble" => self.rumble_on = value != 0,
            _ => {}
        }
    }

    fn memory_card(&self) -> Option<Vec<u8>> {
        self.bus.sio.cards[0].as_ref().map(|c| c.data.clone())
    }

    fn insert_memory_card(&mut self, data: &[u8], _swap: bool) -> bool {
        // a card put in (or swapped) reads FLAG bit 3: the game rereads its directory
        let c = memcard::MemCard::new(data);
        if let Some((free, names)) = memcard::summary(&c.data) {
            println!("PS1: memory card 1 put in: {free} blocks free, saves: {names:?}");
        }
        self.bus.sio.cards[0] = Some(c);
        true
    }

    fn memory_card_dirty(&mut self) -> bool {
        self.bus.sio.cards[0].as_mut().is_some_and(|c| std::mem::take(&mut c.dirty))
    }
    fn save_battery_path(&self) -> &str { &self.save_path }
    fn save_filename(&self) -> &str { &self.save_filename }
    fn save_state_path(&self) -> &str { &self.save_path }
    fn power_on(&mut self) {}
    fn power_off(&mut self) {}
    fn is_sram_dirty(&self) -> bool { false }
    fn clear_sram_dirty(&mut self) {}
    fn get_current_frame(&self) -> i64 { self.current_frame as i64 }

    fn frame_time_usec(&self) -> i64 {
        let (clocks, hz) = if self.pal() { (3406i64, 53_222_400i64) } else { (3413, 53_693_175) };
        self.lines() as i64 * clocks * 1_000_000 / hz
    }
}
