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

