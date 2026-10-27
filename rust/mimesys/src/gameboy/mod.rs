//pub mod gb_dummy;
//pub mod gb_mock_bus;
mod gb_cpu;
//mod gb_godot;
pub mod gb_system;
pub mod gb_sgb;
mod gb_bus;
mod gb_joypad;
mod gb_mbc;
mod gb_cartridge;
mod gb_common;
mod gb_apu;
mod gb_ppu;
mod gb_timer;
mod gb_dma;
mod gb_palette;
pub mod gb_link;
pub mod gb_serial;
pub mod gb_link_system;
pub mod gb_netlink;
#[path = "mbc/gb_mbc0.rs"]
pub mod gb_mbc0;
#[path = "mbc/gb_mbc1.rs"]
pub mod gb_mbc1;
#[path = "mbc/gb_mbc2.rs"]
pub mod gb_mbc2;
#[path = "mbc/gb_mbc3.rs"]
pub mod gb_mbc3;
#[path = "mbc/gb_mbc5.rs"]
pub mod gb_mbc5;
//pub mod gb_cpu_test;
//pub mod gb_control_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod suite_tests;
