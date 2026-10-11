use crate::common::system::System;
use crate::gameboy::gb_system::GbSystem;
use crate::coleco::coleco_system::ColecoSystem;
use crate::arcade::donkey_kong::dk_system::DKSystem;
use std::collections::HashMap;
 
// use crate::gameboy::load_gameboy;
// use crate::gba::load_gba;
// use crate::arcade::load_pacman;
 
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemKind {
    GameBoy,
    GameBoyColor,
    Gba,
    Coleco,
    Pacman,
    DonkeyKong,
}
 
pub enum RomSource {
    SingleFile(Vec<u8>),
    FileSet(HashMap<String, Vec<u8>>),
}
 
pub struct LoadRequest {
    pub kind: SystemKind,
    pub rom: RomSource,
    pub filename: String,
    pub bios: Option<Vec<u8>>, // required by some systems, ignored by others
}
 
/// Single entry point the Godot layer calls. Returns a boxed trait object;
pub fn load_system(req: LoadRequest) -> Result<Box<dyn System>, String> {
    match req.kind {
        SystemKind::GameBoy | SystemKind::GameBoyColor => {
            let rom = req.rom.single().ok_or("Game Boy needs a single ROM file")?;
            // BIOS optional (skip-boot supported).
            // Ok(load_gameboy(&rom, req.bios.as_deref(), req.kind))
            let bios: &[u8] = req.bios.as_deref().unwrap_or(&[]);
            let core = GbSystem::from_rom(rom.as_slice(), &req.filename, bios)
                .map_err(|e| format!("GB from_rom failed: {e}"))?;
            Ok(Box::new(core))
        }
        SystemKind::Gba => {
            let rom = req.rom.single().ok_or("GBA needs a single ROM file")?;
            let bios = req.bios.ok_or("GBA requires a BIOS image")?;
            let _ = (rom, bios);
            todo!("call load_gba")
        }
        SystemKind::Pacman => {
            let files = req.rom.file_set().ok_or("Pac-Man needs a ROM set (zip)")?;
            let _ = files;
            todo!("call load_pacman")
        }

        SystemKind::DonkeyKong => {
            let files = req.rom.file_set().ok_or("Donkey Kong needs a ROM set (zip)")?;
            let _ = files;
            todo!("call load_dkong")
        }

        SystemKind::Coleco => {
            let rom = req.rom.single().ok_or("Colecovision needs a single ROM file")?;
            let bios: &[u8] = req.bios.as_deref().unwrap_or(&[]);
            let core = ColecoSystem::from_rom(rom.as_slice(), &req.filename, bios)
                .map_err(|e| format!("Colecovision from_rom failed: {e}"))?;
            Ok(Box::new(core))
        }
    }
}
 
impl RomSource {
    fn single(self) -> Option<Vec<u8>> {
        match self { RomSource::SingleFile(v) => Some(v), _ => None }
    }
    fn file_set(self) -> Option<HashMap<String, Vec<u8>>> {
        match self { RomSource::FileSet(m) => Some(m), _ => None }
    }
}