//! GBA link cable.
//!
//! * `GbaCable`: a cable to another emulator; the network link (gb_netlink.rs, shared
//!   with the Game Boy) implements it.
//! * `run_linked` / `GbaLinkPair`: two GBAs side by side in one emulator. They take turns
//!   in SLICE-clock steps; when one finishes driving a transfer (`sio.awaiting`), its data
//!   goes to the other's `external()` and the answer comes straight back, so the other
//!   side is at most one slice (~1 us) behind. The slice is short because two copies of
//!   one game do the same thing at the same clock: Super Mario Advance enters multiplayer
//!   on both, then checks "all ready" 24 clocks later; with long slices the GBA running
//!   first didn't see its partner yet, decided it was alone, and both waited forever.
//!
//! Picture: 480 x 160, GBA 1 on the left (the multiplayer parent). The one you're not
//! controlling is dimmed; set_option("link_focus", 0 / 1) switches (Tab in the frontend).
//! Sound comes from the controlled one. Each keeps its own battery save; the right-hand
//! one saves as "<name>_p2" when both run the same game. No save states for the pair yet.

use std::time::Duration;

use crate::common::system::{DisplayInfo, Rotation, System};
use crate::gameboy::gb_netlink::NetLink;
use crate::gba::gba_ppu::{CYCLES_PER_FRAME, SCREEN_H, SCREEN_W};
use crate::gba::gba_sio::Sio;
use crate::gba::gba_system::GbaSystem;

/// A cable to another emulator.
pub trait GbaCable: Send {
    /// Deliver the other side's transfers into `sio`, send ours, and if ours waits for an
    /// answer (`sio.awaiting`), wait up to `wait` for it.
    fn service(&mut self, sio: &mut Sio, wait: Duration);
    /// One line for the UI.
    fn status(&self) -> String;
}

impl GbaCable for NetLink {
    fn service(&mut self, sio: &mut Sio, wait: Duration) { self.service_port(sio, wait) }
    fn status(&self) -> String { self.status_line() }
}

/// How often (clocks) a network cable is checked for the other side's transfers (~4000/s).
pub const POLL_CYCLES: u64 = 4096;
/// How long a frame may wait for the other side's answer before trying again next frame.
pub const REPLY_WAIT: Duration = Duration::from_millis(20);
/// Clocks each GBA of a pair runs before the other gets a turn.
pub const SLICE: u64 = 16;

/// Hands a finished transfer from `from` to `to`. Returns true if there was one.
fn deliver(from: &mut GbaSystem, to: &mut GbaSystem) -> bool {
    let Some((kind, value)) = from.bus.sio.awaiting else { return false };
    let reply = to.bus.sio.external(kind, value);
    from.bus.sio.finish(reply);
    true
}

/// Runs both GBAs for `cycles` clocks, linked.
pub fn run_linked(a: &mut GbaSystem, b: &mut GbaSystem, cycles: u64) {
    let (start_a, start_b) = (a.bus.cycles, b.bus.cycles);
    let mut done = 0;
    while done < cycles {
        done = (done + SLICE).min(cycles);
        run_unit(a, b, start_a + done);
        run_unit(b, a, start_b + done);
    }
}

fn run_unit(me: &mut GbaSystem, other: &mut GbaSystem, target: u64) {
    // SD ("all ready") needs to know whether the other GBA is in multiplayer mode
    me.bus.sio.partner_multi = other.bus.sio.in_multi();
    loop {
        me.run_until(target);
        if !deliver(me, other) { break; }
    }
}

const W: usize = SCREEN_W;
const H: usize = SCREEN_H;

pub struct GbaLinkPair {
    a: GbaSystem,
    b: GbaSystem,
    focus: usize,
    frame: u64,
    framebuffer: Vec<u8>,
}

impl GbaLinkPair {
    pub fn new(mut a: GbaSystem, mut b: GbaSystem) -> Self {
        if b.save_filename() == a.save_filename() {
            let n = format!("{}_p2", b.save_filename());
            b.set_save_filename(&n);
        }
        a.bus.sio.linked = true;
        a.bus.sio.parent = true;
        b.bus.sio.linked = true;
        b.bus.sio.parent = false;
        Self { a, b, focus: 0, frame: 0, framebuffer: vec![0; 2 * W * H * 4] }
    }

    /// Both pictures side by side, the uncontrolled one dimmed.
    fn compose(&mut self) {
        let focus = self.focus;
        for (side, gba) in [&self.a, &self.b].into_iter().enumerate() {
            let src_fb = gba.framebuffer();
            for y in 0..H {
                let src = &src_fb[y * W * 4..(y + 1) * W * 4];
                let start = (y * 2 * W + side * W) * 4;
                let dst = &mut self.framebuffer[start..start + W * 4];
                dst.copy_from_slice(src);
                if side != focus {
                    for px in dst.chunks_exact_mut(4) {
                        px[0] = (px[0] as u16 * 3 / 5) as u8;
                        px[1] = (px[1] as u16 * 3 / 5) as u8;
                        px[2] = (px[2] as u16 * 3 / 5) as u8;
                    }
                }
            }
        }
    }
}

impl System for GbaLinkPair {
    fn run_frame(&mut self, input: u32) {
        let (ia, ib) = if self.focus == 0 { (input, 0) } else { (0, input) };
        self.a.set_input(ia);
        self.b.set_input(ib);
        run_linked(&mut self.a, &mut self.b, CYCLES_PER_FRAME as u64);
        self.a.trace_frame();
        self.b.trace_frame();
        self.compose();
        self.frame += 1;
    }

    fn set_input(&mut self, _input: u32) {}   // done per GBA in run_frame

    fn set_option(&mut self, key: &str, value: i64) {
        match key {
            "link_focus" => self.focus = value.clamp(0, 1) as usize,
            _ => { self.a.set_option(key, value); self.b.set_option(key, value); }
        }
    }

    fn link_partner(&mut self) -> Option<&mut dyn System> { Some(&mut self.b) }

    /// Both link logs, merged by time: "A" = left GBA (parent), "B" = right.
    fn take_trace(&mut self) -> Vec<String> {
        let mut lines: Vec<(u64, String)> = Vec::new();
        for (tag, s) in [("A", &mut self.a), ("B", &mut self.b)] {
            for l in s.take_trace() {
                let t = l.split_whitespace().next().and_then(|n| n.parse().ok()).unwrap_or(0);
                lines.push((t, format!("{tag} {l}")));
            }
        }
        lines.sort_by_key(|(t, _)| *t);
        lines.into_iter().map(|(_, l)| l).collect()
    }

    fn drain_audio(&mut self) -> Vec<f32> {
        // drain both (or the other one's buffer grows forever), play the focused one
        let a = self.a.drain_audio();
        let b = self.b.drain_audio();
        if self.focus == 0 { a } else { b }
    }

    fn sample_rate(&self) -> u32 { self.a.sample_rate() }
    fn power_on(&mut self) { self.a.power_on(); self.b.power_on(); }
    fn power_off(&mut self) { self.a.power_off(); self.b.power_off(); }
    fn reset(&mut self) {
        self.a.reset();
        self.b.reset();
        for (s, parent) in [(&mut self.a, true), (&mut self.b, false)] {
            s.bus.sio.linked = true;
            s.bus.sio.parent = parent;
        }
    }

    // Battery save of GBA 1; GBA 2's goes through link_partner()
    fn has_battery(&self) -> bool { self.a.has_battery() }
    fn save_filename(&self) -> &str { self.a.save_filename() }
    fn save_battery_path(&self) -> &str { self.a.save_battery_path() }
    fn save_state_path(&self) -> &str { self.a.save_state_path() }
    fn is_sram_dirty(&self) -> bool { self.a.is_sram_dirty() }
    fn clear_sram_dirty(&mut self) { self.a.clear_sram_dirty() }
    fn get_sram(&self) -> Option<&[u8]> { self.a.get_sram() }
    fn load_sram(&mut self, data: &[u8]) { self.a.load_sram(data) }

    fn get_current_frame(&self) -> i64 { self.frame as i64 }
    fn framebuffer(&self) -> &[u8] { &self.framebuffer }

    fn display_info(&self) -> DisplayInfo {
        DisplayInfo {
            buffer_width: (2 * W) as i32,
            buffer_height: H as i32,
            visible_x: 0,
            visible_y: 0,
            visible_width: (2 * W) as i32,
            visible_height: H as i32,
            target_aspect_ratio: 3.0,
            rotation: Rotation::None,
        }
    }

    fn save_state(&self) -> Vec<u8> { Vec::new() }
    fn load_state(&mut self, _data: &[u8]) -> Result<(), String> {
        Err("save states aren't supported with the link cable yet".into())
    }
}
