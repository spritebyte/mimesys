use godot::prelude::*;

struct GDEmulatorExtension;
#[gdextension]
unsafe impl ExtensionLibrary for GDEmulatorExtension {}


mod nes;
mod gameboy;
mod common;
mod coleco;
mod arcade {
    pub mod donkey_kong;
    pub mod pacman;
}

