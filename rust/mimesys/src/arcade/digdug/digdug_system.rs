// Dig Dug (Namco, 1982): the Galaga-family CPU board (MAME src/mame/namco/digdug.cpp,
// galaga.cpp base class).
//
//   3 x Z80 @ 3.072 MHz sharing everything above 0x4000 (each has its own ROM at 0-3FFF)
//     6800-681F  W  Namco WSG (same as Pac-Man)
//     6820-6827  W  latch: 0 main IRQ enable/ack, 1 sub IRQ enable/ack, 2 sound CPU NMI off,
//                          3 run (0 = hold sub CPUs and custom chips in reset), 5-7 53xx mode
//     6830       W  watchdog
//     7000 / 7100   06xx data / control
//     8000-83FF  text layer RAM    8400-87FF work RAM
//     8800-8BFF / 9000-93FF / 9800-9BFF  work RAM + sprite registers (top 128 bytes)
//     A000-A007  W  video latch: 0-1 background picture, 2 text color mode, 3 background off,
//                               4-5 background color bank, 7 flip screen
//     B800-B83F  EAROM data (address latched on write)   B840  W  EAROM control
//   IRQs: main and sub get IRQ0 at vblank (held until the CPU writes its enable bit to 0);
//   the sound CPU gets NMIs on lines 64 and 192.
//   Video: 6.144 MHz pixel clock, 384 x 264 => 192 CPU cycles a line, 60.6 Hz.
//
// Regions: maincpu, sub, sub2, gfx1-gfx4, proms, namco, plus the custom chips' programs
// n51xx (51xx.bin) and n53xx (53xx.bin). High scores live in the EAROM (battery save).

use crate::common::cpu::z80::{Bus, Z80Cpu, Z80State};
use crate::common::input as pad;
use crate::common::io::er2055::Er2055;
use crate::common::system::{System, DisplayInfo, Rotation};
use crate::arcade::take_region;
use crate::arcade::state;
use crate::arcade::pacman::namco_wsg::NamcoWsg;
use crate::arcade::digdug::digdug_video::{DigDugVideo, VideoRegs, SCREEN_W, SCREEN_H};
use crate::arcade::digdug::namco_io::{Namco06, Namco51, Namco53};
use serde::{Serialize, Deserialize};
use std::collections::HashMap;

const CYCLES_PER_LINE: u64 = 192;
const LINES_PER_FRAME: u64 = 264;
const VBLANK_START: u64 = 224;
const SLICE: u64 = 32;                     // Z80 cycles; the 06xx's shortest half period
const MCU_CYCLE: u64 = 12;                 // 1.536 MHz / 6 per instruction cycle vs 3.072 MHz
const WSG_TICKS_PER_LINE: u32 = 6;
const STATE_VERSION: u32 = 1;

pub struct DigDugBus {
    roms: [Vec<u8>; 3],
    pub cur: usize,
    pub videoram: Vec<u8>,
    pub ram: Vec<u8>,
    pub objram: Vec<u8>,
    pub posram: Vec<u8>,
    pub flpram: Vec<u8>,
    pub wsg: NamcoWsg,
    pub misc_latch: u8,
    pub video_latch: u8,
    pub earom: Er2055,
    pub n06: Namco06,
    pub n51: Namco51,
    pub n53: Namco53,
    pub irq_line: [bool; 2],
    pub now: u64,
    pub in0: u8,
    pub in1: u8,
    pub dswa: u8,
    pub dswb: u8,
}

impl DigDugBus {
    fn misc_w(&mut self, bit: u8, on: bool) {
        if on { self.misc_latch |= 1 << bit } else { self.misc_latch &= !(1 << bit) }
        match bit {
            0 if !on => self.irq_line[0] = false,
            1 if !on => self.irq_line[1] = false,
            5..=7 => {
                let q = self.misc_latch;
                self.n53.k = (((q >> 7) & 1) << 3) | (((q >> 6) & 1) << 2) | (((q >> 5) & 1) << 1);
            }
            _ => {}
        }
    }

    fn earom_control(&mut self, data: u8) {
        // CK = D0, C1 = /D1, C2 = D2, CS1 = D3, /CS2 = GND
        self.earom.set_control(data & 8 != 0, true, data & 2 == 0, data & 4 != 0);
        self.earom.set_clk(data & 1 != 0);
    }

    fn video_regs(&self) -> VideoRegs<'_> {
        let l = self.video_latch;
        VideoRegs {
            videoram: &self.videoram, objram: &self.objram, posram: &self.posram, flpram: &self.flpram,
            bg_select: l & 3, bg_color_bank: l & 0x30, tx_color_mode: l & 4 != 0,
            bg_disable: l & 8 != 0, flip: l & 0x80 != 0,
        }
    }
}

impl Bus for DigDugBus {
    fn read(&mut self, addr: u16) -> u8 {
        let a = addr as usize;
        match addr {
            0x0000..=0x3FFF => self.roms[self.cur].get(a).copied().unwrap_or(0xFF),
            0x7000..=0x70FF => {
                let c = self.n06.control;
                if c & 0x10 == 0 { return 0; }
                let mut r = 0xFF;
                if c & 1 != 0 { r &= self.n51.port_o; }
                if c & 2 != 0 { r &= self.n53.port_o; }
                r
            }
            0x7100 => self.n06.control,
            0x8000..=0x83FF => self.videoram[a & 0x3FF],
            0x8400..=0x87FF => self.ram[a & 0x3FF],
            0x8800..=0x8BFF => self.objram[a & 0x3FF],
            0x9000..=0x93FF => self.posram[a & 0x3FF],
            0x9800..=0x9BFF => self.flpram[a & 0x3FF],
            0xB800..=0xB83F => self.earom.read(),
            _ => 0xFF,
        }
    }

    fn write(&mut self, addr: u16, v: u8) {
        let a = addr as usize;
        match addr {
            0x6800..=0x681F => self.wsg.write((a & 0x1F) as u8, v),
            0x6820..=0x6827 => self.misc_w((a & 7) as u8, v & 1 != 0),
            0x7000..=0x70FF => {
                // write mode only; the 51xx latches it on its O lines
                if self.n06.control & 0x10 == 0 && self.n06.control & 1 != 0 { self.n51.port_o = v; }
            }
            0x7100 => {
                let now = self.now;
                if self.n06.ctrl_w(v, now) {
                    self.n51.mcu.set_irq(false);
                    self.n53.mcu.set_irq(false);
                }
            }
            0x8000..=0x83FF => self.videoram[a & 0x3FF] = v,
            0x8400..=0x87FF => self.ram[a & 0x3FF] = v,
            0x8800..=0x8BFF => self.objram[a & 0x3FF] = v,
            0x9000..=0x93FF => self.posram[a & 0x3FF] = v,
            0x9800..=0x9BFF => self.flpram[a & 0x3FF] = v,
            0xA000..=0xA007 => {
                let bit = (a & 7) as u8;
                if v & 1 != 0 { self.video_latch |= 1 << bit } else { self.video_latch &= !(1 << bit) }
            }
            0xB800..=0xB83F => { self.earom.set_address((a & 0x3F) as u8); self.earom.set_data(v); }
            0xB840 => self.earom_control(v),
            _ => {}
        }
    }

    fn port_in(&mut self, _port: u16) -> u8 { 0xFF }
    fn port_out(&mut self, _port: u16, _val: u8) {}
}

pub struct DigDugSystem {
    pub cpus: [Z80Cpu; 3],
    pub bus: DigDugBus,
    pub video: DigDugVideo,
    cycles: [u64; 3],        // how far each Z80 has run (global Z80 cycles)
    mcu_cycles: [u64; 2],    // same for the 51xx / 53xx, in Z80 cycles
    now: u64,
    subs_held: bool,
    nmi_line: bool,
    current_frame: u64,
    save_filename: String,
    save_path: String,
    state_path: String,
}

/// Save state (no ROMs, graphics or MCU programs).
#[derive(Serialize, Deserialize)]
struct DigDugState<C, B> {
    cpus: C,
    bus: B,
    cycles: [u64; 3],
    mcu_cycles: [u64; 2],
    now: u64,
    subs_held: bool,
    nmi_line: bool,
    frame: u64,
}

type BusState = (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>, NamcoWsg, u8, u8, Er2055,
                 Namco06, Namco51, Namco53, [bool; 2]);

impl DigDugSystem {
    pub fn new(set_name: &str, mut regions: HashMap<String, Vec<u8>>) -> Result<Self, String> {
        let r0 = take_region(&mut regions, "maincpu")?;
        let r1 = take_region(&mut regions, "sub")?;
        let r2 = take_region(&mut regions, "sub2")?;
        let gfx1 = take_region(&mut regions, "gfx1")?;
        let gfx2 = take_region(&mut regions, "gfx2")?;
        let gfx3 = take_region(&mut regions, "gfx3")?;
        let gfx4 = take_region(&mut regions, "gfx4")?;
        let proms = take_region(&mut regions, "proms")?;
        let namco = take_region(&mut regions, "namco")?;
        let n51 = take_region(&mut regions, "n51xx")
            .map_err(|_| "51xx.bin missing (Namco 51xx program, MAME's namco51.zip)".to_string())?;
        let n53 = take_region(&mut regions, "n53xx")
            .map_err(|_| "53xx.bin missing (Namco 53xx program, MAME's namco53.zip)".to_string())?;

        let video = DigDugVideo::new(&gfx1, &gfx2, &gfx3, &gfx4, &proms)?;
        let mut wsg = NamcoWsg::new(&namco[..namco.len().min(0x100)], 44100);
        wsg.enabled = true;
        let mut bus = DigDugBus {
            roms: [r0, r1, r2], cur: 0,
            videoram: vec![0; 0x400], ram: vec![0; 0x400],
            objram: vec![0; 0x400], posram: vec![0; 0x400], flpram: vec![0; 0x400],
            wsg, misc_latch: 0, video_latch: 0, earom: Er2055::new(),
            n06: Namco06::new(), n51: Namco51::new(n51), n53: Namco53::new(n53),
            irq_line: [false; 2], now: 0,
            in0: 0xFF, in1: 0xFF,
            // 1 coin 1 credit both slots, bonus 20K/60K, 3 lives; upright, freeze off
            dswa: 0x99, dswb: 0x24,
        };
        bus.earom_control(0);
        let mut sys = Self {
            cpus: [Z80Cpu::new(), Z80Cpu::new(), Z80Cpu::new()],
            bus, video, cycles: [0; 3], mcu_cycles: [0; 2], now: 0, subs_held: false, nmi_line: false,
            current_frame: 0,
            save_filename: set_name.to_string(),
            save_path: "user://GD_EMU/Arcade/DigDug/Save".to_string(),
            state_path: "user://GD_EMU/Arcade/DigDug/State".to_string(),
        };
        sys.apply_reset_line();
        Ok(sys)
    }

    /// Latch bit 3 low holds both sub CPUs and the custom chips in reset.
    fn apply_reset_line(&mut self) {
        let held = self.bus.misc_latch & 0x08 == 0;
        if held == self.subs_held { return; }
        self.subs_held = held;
        if held {
            self.cpus[1] = Z80Cpu::new();
            self.cpus[2] = Z80Cpu::new();
        } else {
            self.cycles[1] = self.now;
            self.cycles[2] = self.now;
            self.mcu_cycles = [self.now; 2];
        }
        self.bus.n51.mcu.set_reset(held);
        self.bus.n53.mcu.set_reset(held);
    }

    fn update_inputs(&mut self) {
        let (a, b) = (self.bus.in0, self.bus.in1);
        self.bus.n51.inputs = [a & 0x0F, a >> 4, b & 0x0F, b >> 4];
        let (a, b) = (self.bus.dswa, self.bus.dswb);
        self.bus.n53.inputs = [a & 0x0F, a >> 4, b & 0x0F, b >> 4];
    }

    /// Run Z80 `i` up to `until` (global Z80 cycles).
    fn run_cpu(&mut self, i: usize, until: u64) {
        self.bus.cur = i;
        while self.cycles[i] < until {
            self.bus.now = self.cycles[i];
            let mut c = 0;
            if i < 2 && self.bus.irq_line[i] {
                c = self.cpus[i].irq(&mut self.bus, 0xFF);
            }
            if c == 0 { c = self.cpus[i].step(&mut self.bus); }
            self.cycles[i] += c as u64;
            if i == 0 {
                // the main CPU may have released or asserted the reset line, or (re)started the 06xx
                if (self.bus.misc_latch & 0x08 == 0) != self.subs_held { self.apply_reset_line(); }
            }
        }
    }

    fn run_slice(&mut self) {
        let end = self.now + SLICE;
        // 06xx clock: chip selects (MCU IRQs), R/W line, main CPU NMI
        while let Some(t) = self.bus.n06.advance(self.now) {
            if let Some(rw) = t.rw { self.bus.n51.rw = rw as u8; }
            self.bus.n51.mcu.set_irq(t.chipsel[0]);
            self.bus.n53.mcu.set_irq(t.chipsel[1]);
        }
        if self.bus.n06.nmi && !self.nmi_line {
            self.bus.cur = 0;
            self.cycles[0] += self.cpus[0].nmi(&mut self.bus) as u64;
        }
        self.nmi_line = self.bus.n06.nmi;

        self.run_cpu(0, end);
        if !self.subs_held {
            self.run_cpu(1, end);
            self.run_cpu(2, end);
            while self.mcu_cycles[0] < end {
                self.mcu_cycles[0] += self.bus.n51.step() as u64 * MCU_CYCLE;
            }
            while self.mcu_cycles[1] < end {
                self.mcu_cycles[1] += self.bus.n53.step() as u64 * MCU_CYCLE;
            }
        } else {
            self.cycles[1] = end;
            self.cycles[2] = end;
            self.mcu_cycles = [end; 2];
        }
        self.now = end;
    }
}

impl System for DigDugSystem {
    fn run_frame(&mut self, input: u32) {
        self.set_input(input);
        self.update_inputs();
        for line in 0..LINES_PER_FRAME {
            if line == VBLANK_START {
                let regs = self.bus.video_regs();
                self.video.render(&regs);
                if self.bus.misc_latch & 1 != 0 { self.bus.irq_line[0] = true; }
                if self.bus.misc_latch & 2 != 0 { self.bus.irq_line[1] = true; }
                self.bus.n51.mcu.set_tc(false);     // TC follows /VBLANK
            }
            if line == 0 { self.bus.n51.mcu.set_tc(true); }
            if (line == 64 || line == 192) && self.bus.misc_latch & 4 == 0 && !self.subs_held {
                self.bus.cur = 2;
                self.cycles[2] += self.cpus[2].nmi(&mut self.bus) as u64;
            }
            for _ in 0..CYCLES_PER_LINE / SLICE { self.run_slice(); }
            for _ in 0..WSG_TICKS_PER_LINE { self.bus.wsg.tick(); }
        }
        self.current_frame += 1;
    }

    /// Active low. IN0: 0x01 up, 0x02 right, 0x04 down, 0x08 left (player 2 in the high
    /// nibble). IN1: 0x01 button (P1), 0x02 button (P2), 0x04 start 1, 0x08 start 2,
    /// 0x10 coin 1, 0x20 coin 2, 0x40 service coin, 0x80 service mode.
    fn set_input(&mut self, mask: u32) {
        let on = |m: u32, b: u32| m & b != 0;
        let joy = |m: u32| -> u8 {
            let mut v = 0x0F;
            if on(m, pad::UP) { v &= !0x01; }
            if on(m, pad::RIGHT) { v &= !0x02; }
            if on(m, pad::DOWN) { v &= !0x04; }
            if on(m, pad::LEFT) { v &= !0x08; }
            v
        };
        let p2 = pad::player2(mask);
        self.bus.in0 = joy(mask) | (joy(p2) << 4);
        let mut in1 = 0xFF;
        if on(mask, pad::A) || on(mask, pad::B) { in1 &= !0x01; }
        if on(p2, pad::A) || on(p2, pad::B) { in1 &= !0x02; }
        if on(mask, pad::START) { in1 &= !0x04; }
        if on(mask, pad::START2) { in1 &= !0x08; }
        if on(mask, pad::COIN) || on(mask, pad::SELECT) { in1 &= !0x10; }
        self.bus.in1 = in1;
    }

    fn drain_audio(&mut self) -> Vec<f32> { std::mem::take(&mut self.bus.wsg.out) }
    fn sample_rate(&self) -> u32 { 44100 }
    fn power_on(&mut self) {}
    fn power_off(&mut self) {}
    fn has_battery(&self) -> bool { true }
    fn save_filename(&self) -> &str { &self.save_filename }
    fn save_battery_path(&self) -> &str { &self.save_path }
    fn save_state_path(&self) -> &str { &self.state_path }
    fn is_sram_dirty(&self) -> bool { self.bus.earom.dirty }
    fn clear_sram_dirty(&mut self) { self.bus.earom.dirty = false; }
    fn get_current_frame(&self) -> i64 { self.current_frame as i64 }
    fn get_sram(&self) -> Option<&[u8]> { Some(&self.bus.earom.data) }
    fn load_sram(&mut self, data: &[u8]) {
        if data.len() == 64 { self.bus.earom.data.copy_from_slice(data); }
    }
    fn framebuffer(&self) -> &[u8] { &self.video.framebuffer }

    fn reset(&mut self) {
        self.cpus = [Z80Cpu::new(), Z80Cpu::new(), Z80Cpu::new()];
        self.bus.misc_latch = 0;
        self.bus.video_latch = 0;
        self.bus.irq_line = [false; 2];
        self.bus.n06 = Namco06::new();
        self.cycles = [self.now; 3];
        self.nmi_line = false;
        self.apply_reset_line();
    }

    /// "dswa" / "dswb": the DIP switch bytes (MAME's digdug input ports).
    fn set_option(&mut self, key: &str, value: i64) {
        match key {
            "dswa" => self.bus.dswa = value as u8,
            "dswb" => self.bus.dswb = value as u8,
            _ => {}
        }
    }

    fn display_info(&self) -> DisplayInfo {
        DisplayInfo {
            buffer_width: SCREEN_W as i32,
            buffer_height: SCREEN_H as i32,
            visible_x: 0,
            visible_y: 0,
            visible_width: SCREEN_W as i32,
            visible_height: SCREEN_H as i32,
            target_aspect_ratio: 3.0 / 4.0,
            rotation: Rotation::CW90,    // MAME ROT90
        }
    }

    fn save_state(&self) -> Vec<u8> {
        let b = &self.bus;
        let cpus: [Z80State; 3] = [self.cpus[0].save_state(), self.cpus[1].save_state(), self.cpus[2].save_state()];
        let bus = (&b.videoram, &b.ram, &b.objram, &b.posram, &b.flpram, b.wsg.save_copy(),
                   b.misc_latch, b.video_latch, &b.earom, &b.n06, &b.n51, &b.n53, b.irq_line);
        state::encode(&self.save_filename, STATE_VERSION, &DigDugState {
            cpus, bus, cycles: self.cycles, mcu_cycles: self.mcu_cycles, now: self.now,
            subs_held: self.subs_held, nmi_line: self.nmi_line, frame: self.current_frame,
        })
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let s: DigDugState<[Z80State; 3], BusState> = state::decode(&self.save_filename, STATE_VERSION, data)?;
        let (videoram, ram, objram, posram, flpram, mut wsg, misc, video, earom, n06, mut n51, mut n53, irq) = s.bus;
        if [&videoram, &ram, &objram, &posram, &flpram].iter().any(|v| v.len() != 0x400) {
            return Err("save state has the wrong memory sizes".into());
        }
        let b = &mut self.bus;
        b.videoram = videoram; b.ram = ram; b.objram = objram; b.posram = posram; b.flpram = flpram;
        wsg.keep_static(&mut b.wsg);
        wsg.out = std::mem::take(&mut b.wsg.out);
        b.wsg = wsg;
        b.misc_latch = misc;
        b.video_latch = video;
        b.earom = earom;
        b.earom.dirty = true;
        b.n06 = n06;
        n51.mcu.keep_rom(&mut b.n51.mcu);
        n51.inputs = b.n51.inputs;
        b.n51 = n51;
        n53.mcu.keep_rom(&mut b.n53.mcu);
        n53.inputs = b.n53.inputs;
        b.n53 = n53;
        b.irq_line = irq;
        for (cpu, st) in self.cpus.iter_mut().zip(s.cpus.iter()) { cpu.load_state(st); }
        self.cycles = s.cycles;
        self.mcu_cycles = s.mcu_cycles;
        self.now = s.now;
        self.subs_held = s.subs_held;
        self.nmi_line = s.nmi_line;
        self.current_frame = s.frame;
        Ok(())
    }
}
