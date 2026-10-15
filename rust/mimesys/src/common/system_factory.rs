use crate::common::system::System;
use crate::gameboy::gb_system::GbSystem;
use crate::coleco::coleco_system::ColecoSystem;
use crate::nes::nes_system::NesSystem;
use std::collections::HashMap;
 
// use crate::gameboy::load_gameboy;
// use crate::gba::load_gba;
// use crate::arcade::load_pacman;
 
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CartSystem { GameBoy, GameBoyColor, Gba, Coleco, Nes }

impl CartSystem {
    pub fn from_id(id: &str) -> Result<Self, String> {
        match id {
            "gb"     => Ok(Self::GameBoy),
            "gbc"    => Ok(Self::GameBoyColor),
            "gba"    => Ok(Self::Gba),
            "coleco" => Ok(Self::Coleco),
            "nes"    => Ok(Self::Nes),
            other    => Err(format!("Unknown system id '{other}'")),
        }
    }
}

pub enum LoadRequest {
    Cartridge { system: CartSystem, rom: Vec<u8>, filename: String, bios: Option<Vec<u8>> },
    Arcade    { driver: String, set_name: String, regions: HashMap<String, Vec<u8>> },
}

pub fn load_system(req: LoadRequest) -> Result<Box<dyn System>, String> {
    match req {
        LoadRequest::Cartridge { system, rom, filename, bios } =>
            load_cartridge(system, &rom, &filename, bios),
        LoadRequest::Arcade { driver, set_name, regions } =>
            crate::arcade::load_arcade(&driver, &set_name, regions),
    }
}

fn load_cartridge(system: CartSystem, rom: &[u8], filename: &str, bios: Option<Vec<u8>>)
    -> Result<Box<dyn System>, String>
{
    let bios_or_empty: &[u8] = bios.as_deref().unwrap_or(&[]);
    match system {
        CartSystem::GameBoy | CartSystem::GameBoyColor => {
            let core = GbSystem::from_rom(rom, filename, bios_or_empty)
                .map_err(|e| format!("GB from_rom failed: {e}"))?;
            Ok(Box::new(core))
        }
        CartSystem::Gba => {
            let _bios = bios.ok_or("GBA requires a BIOS image")?;
            Err("GBA core not yet implemented yet".into())
        }
        CartSystem::Coleco => {
            let core = ColecoSystem::from_rom(rom, filename, bios_or_empty)
                .map_err(|e| format!("Colecovision from_rom failed: {e}"))?;
            Ok(Box::new(core))
        }
        CartSystem::Nes => {
            let core = NesSystem::from_rom(rom, filename, bios_or_empty)
                .map_err(|e| format!("GB from_rom failed: {e}"))?;
            Ok(Box::new(core)) 
        }
    }
}
