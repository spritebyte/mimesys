use godot::prelude::*;

struct GDEmulatorExtension;
#[gdextension]
unsafe impl ExtensionLibrary for GDEmulatorExtension {}


mod nes;
mod gameboy;
mod sms;
mod common;
mod coleco;
mod arcade;
mod atari2600;
mod atari7800;
mod atari5200;
mod lynx;
mod snes;
mod pce;
mod gba;
mod genesis;
mod segacd;
mod m32x;
mod nds;

