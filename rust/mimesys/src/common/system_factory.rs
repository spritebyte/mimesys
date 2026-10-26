use crate::common::system::System;
use crate::gameboy::gb_system::GbSystem;
use crate::coleco::coleco_system::ColecoSystem;
use crate::nes::nes_system::NesSystem;
use crate::sms::sms_system::SmsSystem;
use crate::atari2600::a2600_system::A2600System;
use crate::atari7800::a7800_system::A7800System;
use crate::pce::pce_system::PceSystem;
use std::collections::HashMap;
 
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CartSystem { GameBoy, GameBoyColor, Gba, Coleco, Nes, Sms, GameGear, Atari2600, Pce, A7800, }

impl CartSystem {
    pub fn from_id(id: &str) -> Result<Self, String> {
        match id {
            "gb"     => Ok(Self::GameBoy),
            "gbc"    => Ok(Self::GameBoyColor),
            "gba"    => Ok(Self::Gba),
            "coleco" => Ok(Self::Coleco),
            "nes"    => Ok(Self::Nes),
            "sms"    => Ok(Self::Sms),
            "gg"     => Ok(Self::GameGear),
            "a2600"  => Ok(Self::Atari2600),
            "pce"    => Ok(Self::Pce),
            "a7800"  => Ok(Self::A7800),
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
        CartSystem::Atari2600 => {
            let core = A2600System::from_rom(rom, filename, bios_or_empty)
                .map_err(|e| format!("Atari 2600 from_rom failed: {e}"))?;
            Ok(Box::new(core))
        }
        CartSystem::A7800 => {
            let core = A7800System::from_rom(&rom, &filename, bios_or_empty)
                .map_err(|e| format!("Atari 7800 from_rom failed: {e}"))?;
            Ok(Box::new(core))
        }     
        CartSystem::GameBoy | CartSystem::GameBoyColor => {
            let core = GbSystem::from_rom(rom, filename, bios_or_empty)
                .map_err(|e| format!("GB from_rom failed: {e}"))?;
            Ok(Box::new(core))
        }
        CartSystem::Gba => {
            // no BIOS: the cartridge starts directly (BIOS functions won't work)
            let core = crate::gba::gba_system::GbaSystem::from_rom(rom, filename, bios_or_empty)
                .map_err(|e| format!("GBA from_rom failed: {e}"))?;
            Ok(Box::new(core))
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
        CartSystem::Sms | CartSystem::GameGear => {
            let gg = system == CartSystem::GameGear;
            let core = SmsSystem::from_rom(rom, filename, bios_or_empty, gg)
                .map_err(|e| format!("SMS/GG from_rom failed: {e}"))?;
            Ok(Box::new(core))
        }
        CartSystem::Pce => {
            let core = PceSystem::from_rom(rom, filename, bios_or_empty)
                .map_err(|e| format!("PC Engine from_rom failed: {e}"))?;
            Ok(Box::new(core))
        }
    }
}
