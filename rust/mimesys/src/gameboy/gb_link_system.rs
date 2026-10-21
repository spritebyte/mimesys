// Two Game Boys side by side, joined by a link cable (e.g. Pokémon trading and battles).
//
// Picture: 320 x 144, Game Boy 1 on the left. The one you're not controlling is dimmed.
// Controls go to one Game Boy at a time; set_option("link_focus", 0 or 1) switches
// (the frontend binds that to Tab). Sound comes from the controlled one.
// Each Game Boy keeps its own battery save; if both run the same game, the right-hand one
// saves as "<name>_p2" so they don't overwrite each other.
// Save states aren't supported for the pair (yet).

use crate::common::system::{DisplayInfo, Rotation, System};
use crate::gameboy::gb_link::{run_linked, FRAME_CYCLES};
use crate::gameboy::gb_system::GbSystem;

const W: usize = 160;
const H: usize = 144;

pub struct GbLinkPair {
    a: GbSystem,
    b: GbSystem,
    focus: usize,
    frame: u64,
    framebuffer: Vec<u8>,
}

impl GbLinkPair {
    pub fn new(a: GbSystem, mut b: GbSystem) -> Self {
        if b.save_filename == a.save_filename {
            b.save_filename = format!("{}_p2", b.save_filename);
        }
        Self { a, b, focus: 0, frame: 0, framebuffer: vec![0; 2 * W * H * 4] }
    }

    /// Copy both pictures into the side-by-side framebuffer, dimming the unfocused one.
    fn compose(&mut self) {
        self.a.refresh_frame_cache();
        self.b.refresh_frame_cache();
        let focus = self.focus;
        let fb = &mut self.framebuffer;
        for (side, gb) in [&self.a, &self.b].into_iter().enumerate() {
            if gb.frame_cache.len() < W * H * 4 {
                continue;
            }
            for y in 0..H {
                let src = &gb.frame_cache[y * W * 4..(y + 1) * W * 4];
                let start = (y * 2 * W + side * W) * 4;
                let dst = &mut fb[start..start + W * 4];
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

impl System for GbLinkPair {
    fn run_frame(&mut self, input: u32) {
        let (ia, ib) = if self.focus == 0 { (input, 0) } else { (0, input) };
        self.a.set_input(ia);
        self.b.set_input(ib);
        run_linked(&mut self.a, &mut self.b, FRAME_CYCLES);
        self.compose();
        self.frame += 1;
    }

    fn set_input(&mut self, _input: u32) {}   // done per Game Boy in run_frame

    fn set_option(&mut self, key: &str, value: i64) {
        if key == "link_focus" {
            self.focus = value.clamp(0, 1) as usize;
        }
    }

    fn link_partner(&mut self) -> Option<&mut dyn System> { Some(&mut self.b) }

    fn drain_audio(&mut self) -> Vec<f32> {
        // drain both (or the other one's buffer grows forever), play the focused one
        let a = self.a.drain_audio();
        let b = self.b.drain_audio();
        if self.focus == 0 { a } else { b }
    }

    fn sample_rate(&self) -> u32 { self.a.sample_rate() }
    fn power_on(&mut self) { self.a.power_on(); self.b.power_on(); }
    fn power_off(&mut self) { self.a.power_off(); self.b.power_off(); }
    fn reset(&mut self) { self.a.reset(); self.b.reset(); }

    // Battery save of Game Boy 1; Game Boy 2's goes through link_partner()
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
            target_aspect_ratio: 20.0 / 9.0,
            rotation: Rotation::None,
        }
    }

    fn save_state(&self) -> Vec<u8> { Vec::new() }
    fn load_state(&mut self, _data: &[u8]) -> Result<(), String> {
        Err("save states aren't supported with the link cable yet".into())
    }
}
