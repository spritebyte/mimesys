pub mod donkey_kong;
pub mod pacman;
pub mod popeye;
pub mod galaxian;

use crate::common::system::System;
use std::collections::HashMap;

pub fn load_arcade(driver: &str, set_name: &str, regions: HashMap<String, Vec<u8>>)
    -> Result<Box<dyn System>, String>
{
    match driver {
        "dkong" => Ok(Box::new(donkey_kong::dk_system::DKSystem::new(set_name.to_string(), regions)?)),
        "pacman" => Ok(Box::new(pacman::pacman_system::PacmanSystem::new(
            pacman::pacman_system::Variant::Pacman, set_name, regions)?)),
        "mspacman" => Ok(Box::new(pacman::pacman_system::PacmanSystem::new(
            pacman::pacman_system::Variant::MsPacman, set_name, regions)?)),
        "pengo" | "pengo_5010" | "pengo_5007 "=> {
            use pacman::pacman_system::{Cipher, Variant};
            let cipher = match driver {
                "pengo_5010" => Cipher::Sega5010,
                "pengo_5007" => Cipher::Sega5007,
                _ => Cipher::None,
            };
            Ok(Box::new(pacman::pacman_system::PacmanSystem::new(
            Variant::Pengo(cipher), set_name, regions)?))
        },
        "popeye" => Ok(Box::new(popeye::popeye_system::PopeyeSystem::new(set_name, regions, true)?)),
        "popeye_noalu" => Ok(Box::new(popeye::popeye_system::PopeyeSystem::new(set_name, regions, false)?)),
        "galaxian" | "mooncrst" | "mooncrst_plain" | "frogger" | "amidar" | "turtles" => {
            use galaxian::galaxian_system::{GalaxianSystem, Variant};
            let v = match driver {
                "mooncrst" => Variant::MoonCresta,
                "mooncrst_plain" => Variant::MoonCrestaPlain,
                "frogger" => Variant::Frogger,
                "amidar" => Variant::Amidar,
                "turtles" => Variant::Turtles,
                _ => Variant::Galaxian,
            };
            Ok(Box::new(GalaxianSystem::new(v, set_name, regions)?))
        },
        other => Err(format!("no arcade driver name '{other}'")),
    }
}

pub fn take_region(regions: &mut HashMap<String, Vec<u8>>, name: &str) -> Result<Vec<u8>, String> {
    regions.remove(name).ok_or_else(|| format!("ROM region '{name}' missing"))
}