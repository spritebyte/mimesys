// Atari 7800 ProSystem (1986): 6502 "Sally" CPU, MARIA graphics, TIA sound, RIOT timer/ports.
//
// Frame (NTSC): 263 lines of 454 MARIA clocks (7.16 MHz). Lines 16-258 are the picture
// (243 lines); MSTAT bit 7 (vertical blank) is set from line 258 to 15. MARIA works a line
// ahead: early in each line (here at clock 28) it shows the line buffer it filled on the
// line before, then its DMA (which halts the CPU) fills the buffer for the next line from
// the display list. Line 16 only fills (nothing to show yet), line 258 only shows, so a
// frame has 242 DMA lines. A DLI raises the CPU's NMI after the DMA of the last line of the
// zone before the DLI zone, so the handler's color changes take effect on the zone's first
// line. (This is ProSystem's order. Doing DMA on line 258 too gave Kung-Fu Master one DLI
// too many a frame: its chain of DLI handlers then ran one zone off each frame.)
//
// Controls (unified input mask, see common/input.rs):
//   D-pad  joystick     A  left button (button 1)     B  right button (button 2)
//   START  PAUSE        SELECT  SELECT                 P2 START  console RESET
// Difficulty switches stay on B.

use serde::{Deserialize, Serialize};
use crate::common::cpu::m6502::{AddressBus, CpuState, CpuVariant, M6502Cpu};
use crate::common::input as pad;
use crate::common::system::{DisplayInfo, Rotation, System};
use crate::atari2600::riot::Riot;
use crate::atari2600::tia::Tia;
use crate::atari7800::a7800_bus::{A7800Bus, CLOCKS_PER_LINE};
use crate::atari7800::cart::Cart7800;
use crate::atari7800::maria::{rgb, Maria, OUT_W};

const SAMPLE_RATE: u32 = 44_100;
const STATE_VERSION: u32 = 1;
const LINES: u32 = 263;
const FIRST_LINE: u32 = 16;
const LAST_LINE: u32 = 258;
pub const OUT_H: usize = (LAST_LINE - FIRST_LINE + 1) as usize;
/// Where in the line MARIA's DMA starts (MARIA clocks).
const DMA_AT: u32 = 28;

pub struct A7800System {
    cpu: M6502Cpu,
    pub bus: A7800Bus,
    /// MARIA clocks into the current line
    line_clock: u32,
    riot_clocks: u32,
    current_frame: u64,
    framebuffer: Vec<u8>,
    save_filename: String,
    save_battery_path: String,
    save_state_path: String,
    /// debug trace (option "debug_report"): stack-pointer jumps, DLIs and switch changes
    debug: bool,
    dbg_sp: u8,
    dbg_lines: u32,
    dbg_swchb: u8,
    dbg_dumped: bool,
    /// DLIs raised this frame (tests)
    pub dlis: u32,
    /// the game is on the timing-workaround list (cart.rs NO_WSYNC)
    listed: bool,
}

#[derive(Serialize, Deserialize)]
struct A7800State {
    version: u32,
    rom_len: usize,
    cpu: CpuState,
    maria: Maria,
    tia: Tia,
    riot: Riot,
    cart: Cart7800,
    #[serde(with = "serde_bytes")]
    ram: Vec<u8>,
    line_clock: u32,
    frame: u64,
}

impl A7800System {
    pub fn from_rom(rom: &[u8], filename: &str, _bios: &[u8]) -> Result<Self, String> {
        let cart = Cart7800::new(rom)?;
        println!("Atari 7800: {} KB, MD5 {}{}{}{}{}", cart.rom_len() / 1024, cart.md5,
                 if cart.ignore_wsync { ", WSYNC ignored and no DMA cycle stealing (game list)" } else { "" },
                 if cart.supergame { ", SuperGame" } else { "" },
                 if cart.pokey { ", POKEY (no sound yet)" } else { "" },
                 if cart.title.is_empty() { String::new() } else { format!(", \"{}\"", cart.title) });
        let mut sys = Self {
            cpu: M6502Cpu::new(CpuVariant::NMOS6502),
            bus: A7800Bus::new(cart, SAMPLE_RATE),
            line_clock: 0,
            riot_clocks: 0,
            current_frame: 0,
            framebuffer: vec![0; OUT_W * OUT_H * 4],
            save_filename: filename.to_string(),
            save_battery_path: "user://GD_EMU/A7800/Save".to_string(),
            save_state_path: "user://GD_EMU/A7800/State".to_string(),
            debug: false, dbg_sp: 0, dbg_lines: 0, dbg_swchb: 0x0B, dbg_dumped: false, dlis: 0, listed: false,
        };
        sys.listed = sys.bus.cart.ignore_wsync;
        sys.cpu.power_on(&mut sys.bus);
        sys.bus.clocks = 0;
        Ok(sys)
    }

    /// One CPU cycle (or a halted one): returns the MARIA clocks it took.
    fn cpu_cycle(&mut self) -> u32 {
        self.bus.clocks = 0;
        self.bus.begin_cpu_cycle();
        self.cpu.step_one_cycle(&mut self.bus);
        self.cpu.sample_interrupt_lines(&mut self.bus);
        self.bus.nmi_line = false;             // an NMI is an edge: one sample is enough
        if self.debug { self.trace_sp(); }
        self.bus.clocks.max(4)
    }

    /// Debug: log where the stack pointer jumps (TXS or a corrupted stack), not pushes/pulls.
    fn trace_sp(&mut self) {
        let sp = self.cpu.sp;
        if sp < 0xE0 && !self.dbg_dumped {
            self.dbg_dumped = true;
            self.dump_state();
        }
        if sp != self.dbg_sp {
            let d = sp.wrapping_sub(self.dbg_sp) as i8;
            if (d > 3 || d < -3) && self.dbg_lines < 600 {
                self.dbg_lines += 1;
                crate::dlog!("7800 f{} SP {:02X} -> {:02X} at PC {:04X} op {:02X}",
                    self.current_frame, self.dbg_sp, sp, self.cpu.pc, self.cpu.current_opcode);
            }
            self.dbg_sp = sp;
        }
    }

    /// Debug: the stack is getting deep (nested NMIs?): what the CPU has been doing, the code
    /// around the NMI handler (A300-A4FF), zero page and MARIA's registers.
    fn dump_state(&self) {
        crate::dlog!("7800 f{}: stack deep (SP {:02X}) at PC {:04X}", self.current_frame, self.cpu.sp, self.cpu.pc);
        crate::dlog!("{}", self.cpu.history_dump());
        let hex = |from: u16, to: u16, peek: &dyn Fn(u16) -> u8| {
            let mut a = from;
            while a <= to {
                let row: Vec<String> = (0..16u16).map(|i| format!("{:02X}", peek(a.wrapping_add(i)))).collect();
                crate::dlog!("  {:04X}: {}", a, row.join(" "));
                a = a.wrapping_add(16);
                if a == 0 { break; }
            }
        };
        let (ram, cart) = (&self.bus.ram, &self.bus.cart);
        let peek = |a: u16| A7800Bus::peek(ram, cart, a);
        crate::dlog!("cartridge A300-A4FF:");
        hex(0xA300, 0xA4F0, &peek);
        crate::dlog!("zero page 40-FF:");
        hex(0x0040, 0x00F0, &peek);
        crate::dlog!("MARIA regs: {:02X?} vblank={}", self.bus.maria.regs, self.bus.maria.vblank);
    }

    fn run_line(&mut self, line: u32) {
        let visible = (FIRST_LINE..=LAST_LINE).contains(&line);
        self.bus.maria.vblank = !(FIRST_LINE..LAST_LINE).contains(&line);
        let mut dma_done = false;
        while self.line_clock < CLOCKS_PER_LINE {
            if !dma_done && self.line_clock >= DMA_AT {
                dma_done = true;
                if visible {
                    self.line_clock += self.dma(line);
                    continue;
                }
            }
            if self.bus.wsync {
                // halted until the line ends (DMA still happens on the way)
                self.line_clock = if dma_done { CLOCKS_PER_LINE } else { DMA_AT };
                continue;
            }
            let c = self.cpu_cycle();
            self.line_clock += c;
        }
        self.line_clock -= CLOCKS_PER_LINE;
        self.bus.wsync = false;
        // the TIA (sound) runs at half MARIA's clock, the RIOT at the CPU's 1.79 MHz
        self.bus.tia.run_audio(CLOCKS_PER_LINE / 2);
        self.riot_clocks += CLOCKS_PER_LINE;
        while self.riot_clocks >= 4 {
            self.riot_clocks -= 4;
            self.bus.riot.tick();
        }
    }

    /// MARIA's work for one picture line: DMA into the line buffer, then output it.
    fn dma(&mut self, line: u32) -> u32 {
        let mut row = [0u8; OUT_W];
        let bus = &mut self.bus;
        let clocks = if bus.maria.dma_on() {
            // show what the line before fetched (nothing yet on the first line)
            if line == FIRST_LINE { bus.maria.background_line(&mut row); } else { bus.maria.output_line(&mut row); }
            if line == LAST_LINE {
                bus.maria.dma_idle()
            } else {
                let (ram, cart) = (&bus.ram, &bus.cart);
                bus.maria.dma_line(line == FIRST_LINE, &|a| A7800Bus::peek(ram, cart, a))
            }
        } else {
            bus.maria.background_line(&mut row);
            0
        };
        if std::mem::take(&mut bus.maria.nmi) {
            bus.nmi_line = true;
            self.dlis += 1;
            if self.debug && self.dbg_lines < 600 {
                self.dbg_lines += 1;
                crate::dlog!("7800 f{} line {} DLI: PC {:04X} SP {:02X}",
                    self.current_frame, line, self.cpu.pc, self.cpu.sp);
            }
        }
        let o = (line - FIRST_LINE) as usize * OUT_W * 4;
        for (px, &c) in self.framebuffer[o..o + OUT_W * 4].chunks_exact_mut(4).zip(row.iter()) {
            let [r, g, b] = rgb(c);
            px.copy_from_slice(&[r, g, b, 0xFF]);
        }
        // listed games: MARIA doesn't steal cycles from the CPU (see cart.rs NO_WSYNC)
        if bus.cart.ignore_wsync { 0 } else { clocks }
    }
}

impl System for A7800System {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        self.dlis = 0;
        for line in 0..LINES {
            self.run_line(line);
        }
        self.current_frame += 1;
    }

    fn set_input(&mut self, mask: u32) {
        let on = |b: u32| mask & b != 0;
        let mut swcha = 0xFFu8;
        if on(pad::RIGHT) { swcha &= !0x80; }
        if on(pad::LEFT) { swcha &= !0x40; }
        if on(pad::DOWN) { swcha &= !0x20; }
        if on(pad::UP) { swcha &= !0x10; }
        let p2 = pad::player2(mask);
        let on2 = |b: u32| p2 & b != 0;
        if on2(pad::RIGHT) { swcha &= !0x08; }
        if on2(pad::LEFT) { swcha &= !0x04; }
        if on2(pad::DOWN) { swcha &= !0x02; }
        if on2(pad::UP) { swcha &= !0x01; }
        self.bus.riot.swcha_in = swcha;
        self.bus.buttons = [on(pad::B), on(pad::A)];
        self.bus.buttons2 = [on2(pad::B), on2(pad::A)];
        let mut swchb = 0x0Bu8;                       // RESET, SELECT, PAUSE up; difficulty B
        if on(pad::START2) { swchb &= !0x01; }
        if on(pad::SELECT) { swchb &= !0x02; }
        if on(pad::START) { swchb &= !0x08; }
        if self.debug && swchb != self.dbg_swchb {
            crate::dlog!("7800 f{} console switches {:02X} -> {:02X}: PC {:04X} SP {:02X}",
                self.current_frame, self.dbg_swchb, swchb, self.cpu.pc, self.cpu.sp);
            self.dbg_swchb = swchb;
            self.dbg_lines = 0;             // fresh budget around each press
            self.dbg_dumped = false;
        }
        self.bus.riot.swchb_in = swchb;
    }

    fn set_option(&mut self, key: &str, value: i64) {
        if key == "debug_report" { self.debug = value != 0; }
        // 0 = run listed games (Kung-Fu Master) without their timing workarounds
        if key == "game_quirks" { self.bus.cart.ignore_wsync = self.listed && value != 0; }
    }

    fn drain_audio(&mut self) -> Vec<f32> { std::mem::take(&mut self.bus.tia.audio_out) }
    fn sample_rate(&self) -> u32 { SAMPLE_RATE }
    fn power_on(&mut self) {}
    fn power_off(&mut self) {}
    fn has_battery(&self) -> bool { false }
    fn save_filename(&self) -> &str { &self.save_filename }
    fn save_battery_path(&self) -> &str { &self.save_battery_path }
    fn save_state_path(&self) -> &str { &self.save_state_path }
    fn is_sram_dirty(&self) -> bool { false }
    fn clear_sram_dirty(&mut self) {}
    fn get_current_frame(&self) -> i64 { self.current_frame as i64 }
    fn get_sram(&self) -> Option<&[u8]> { None }
    fn load_sram(&mut self, _data: &[u8]) {}
    fn framebuffer(&self) -> &[u8] { &self.framebuffer }

    fn reset(&mut self) {
        self.cpu = M6502Cpu::new(CpuVariant::NMOS6502);
        self.cpu.power_on(&mut self.bus);
        self.bus.clocks = 0;
    }

    fn display_info(&self) -> DisplayInfo {
        DisplayInfo {
            buffer_width: OUT_W as i32,
            buffer_height: OUT_H as i32,
            visible_x: 0,
            visible_y: 0,
            visible_width: OUT_W as i32,
            visible_height: OUT_H as i32,
            target_aspect_ratio: 4.0 / 3.0,
            rotation: Rotation::None,
        }
    }

    fn save_state(&self) -> Vec<u8> {
        let state = A7800State {
            version: STATE_VERSION,
            rom_len: self.bus.cart.rom_len(),
            cpu: self.cpu.get_state(),
            maria: self.bus.maria.clone(),
            tia: self.bus.tia.clone(),
            riot: self.bus.riot.clone(),
            cart: self.bus.cart.clone(),
            ram: self.bus.ram.clone(),
            line_clock: self.line_clock,
            frame: self.current_frame,
        };
        let config = bincode::config::standard().with_fixed_int_encoding();
        bincode::serde::encode_to_vec(&state, config).unwrap_or_default()
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let config = bincode::config::standard().with_fixed_int_encoding();
        let (s, _): (A7800State, usize) = bincode::serde::decode_from_slice(data, config)
            .map_err(|e| format!("Failed to decode 7800 save state: {e:?}"))?;
        if s.version != STATE_VERSION {
            return Err(format!("save state version {} (expected {})", s.version, STATE_VERSION));
        }
        if s.rom_len != self.bus.cart.rom_len() {
            return Err("save state is from a different game".into());
        }
        self.cpu.load_state(&s.cpu);
        self.bus.maria.load_state(s.maria);
        self.bus.tia.load_state(s.tia);
        self.bus.riot = s.riot;
        self.bus.cart.load_state(s.cart);
        self.bus.ram = s.ram;
        self.line_clock = s.line_clock;
        self.current_frame = s.frame;
        Ok(())
    }
}
