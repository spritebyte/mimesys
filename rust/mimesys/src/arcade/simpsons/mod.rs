//! Konami The Simpsons (Konami CPU, K052109/K051962 tilemaps, K053246/K053247 sprites,
//! K053251 mixer, ER5911 EEPROM; Z80 + YM2151 + K053260 sound).
pub mod simpsons_sound;
pub mod simpsons_system;
pub mod simpsons_video;
#[cfg(test)]
mod tests;
