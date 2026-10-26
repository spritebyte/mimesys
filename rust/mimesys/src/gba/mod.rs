//! Game Boy Advance.
pub mod gba_apu;
pub mod gba_bus;
pub mod gba_cart;
pub mod gba_dma;
pub mod gba_link;
pub mod gba_ppu;
pub mod gba_sio;
mod gba_render;
pub mod gba_system;
pub mod gba_timer;
#[cfg(test)]
mod tests;
