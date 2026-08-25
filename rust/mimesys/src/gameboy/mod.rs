pub mod gb_dummy;
pub mod gb_mock_bus;
mod gb_cpu;
mod gb_godot;
mod gb_system;
mod gb_bus;
mod gb_mbc;
mod gb_cartridge;
mod gb_common;
mod gb_apu;
mod gb_ppu;
mod gb_timer;
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
pub mod gb_cpu_test;
pub mod gb_control_tests;