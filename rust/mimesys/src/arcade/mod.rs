pub mod donkey_kong;
pub mod pacman;
pub mod popeye;
pub mod galaxian;
pub mod gottlieb;
pub mod irem;
pub mod digdug;
pub mod atarisy2;
pub mod gberet;
pub mod ddragon;
pub mod simpsons;
pub mod williams;
pub mod jedi;
pub mod state;
pub mod hiscore;

use crate::common::system::System;
use crate::arcade::hiscore::{HiScore, HiScoreMem, HiScoreSystem};
use std::collections::HashMap;

/// Build an arcade machine. `regions` may also carry "hiscore.dat" (the text of MAME's
/// hiscore.dat): machines without battery RAM then save their score table through it.
pub fn load_arcade(driver: &str, set_name: &str, mut regions: HashMap<String, Vec<u8>>)
    -> Result<Box<dyn System>, String>
{
    let hs = regions.remove("hiscore.dat")
        .and_then(|d| HiScore::parse(&String::from_utf8_lossy(&d), set_name));
    if hs.is_some() {
        println!("hiscore: {set_name} found in hiscore.dat, high scores will be saved");
    }
    fn wrap<S: System + HiScoreMem + 'static>(sys: S, hs: Option<HiScore>, dir: &str) -> Box<dyn System> {
        match hs {
            Some(h) => Box::new(HiScoreSystem::new(sys, h, dir)),
            None => Box::new(sys),
        }
    }
    match driver {
        "dkong" | "dkongjr" => {
            use donkey_kong::dk_system::{DKSystem, Variant};
            let v = if driver == "dkongjr" { Variant::DonkeyKongJr } else { Variant::DonkeyKong };
            Ok(wrap(DKSystem::new(v, set_name.to_string(), regions)?, hs, "DK"))
        },
        "dkong3" => Ok(wrap(donkey_kong::dk3_system::Dk3System::new(set_name.to_string(), regions)?, hs, "DK")),
        "defender" | "stargate" | "robotron" | "joust" | "bubbles" | "splat" | "sinistar" => {
            use williams::williams_system::{Game, WilliamsSystem};
            let game = Game::from_driver(driver).unwrap();
            Ok(Box::new(WilliamsSystem::new(game, set_name, regions)?))
        }
        "jedi" => Ok(Box::new(jedi::jedi_system::JediSystem::new(set_name, regions)?)),
        "pacman" => Ok(wrap(pacman::pacman_system::PacmanSystem::new(
            pacman::pacman_system::Variant::Pacman, set_name, regions)?, hs, "Pacman")),
        "mspacman" => Ok(wrap(pacman::pacman_system::PacmanSystem::new(
            pacman::pacman_system::Variant::MsPacman, set_name, regions)?, hs, "Pacman")),
        "pengo" | "pengo_5010" | "pengo_5007" => {
            use pacman::pacman_system::{Cipher, Variant};
            let cipher = match driver {
                "pengo_5010" => Cipher::Sega5010,
                "pengo_5007" => Cipher::Sega5007,
                _ => Cipher::None,
            };
            Ok(wrap(pacman::pacman_system::PacmanSystem::new(Variant::Pengo(cipher), set_name, regions)?, hs, "Pacman"))
        },
        "popeye" => Ok(wrap(popeye::popeye_system::PopeyeSystem::new(set_name, regions, true)?, hs, "Popeye")),
        "popeye_noalu" => Ok(wrap(popeye::popeye_system::PopeyeSystem::new(set_name, regions, false)?, hs, "Popeye")),
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
            Ok(wrap(GalaxianSystem::new(v, set_name, regions)?, hs, "Galaxian"))
        },
        // battery-backed NVRAM: the board saves its own scores
        "qbert" => Ok(Box::new(gottlieb::gottlieb_system::GottliebSystem::new(
            gottlieb::gottlieb_system::Variant::Qbert, set_name, regions)?)),
        "kungfum" => Ok(wrap(irem::m62_system::M62System::new(set_name, regions)?, hs, "Irem")),
        // high scores live in the board's EAROM (battery save)
        "digdug" => Ok(Box::new(digdug::digdug_system::DigDugSystem::new(set_name, regions)?)),
        // high scores and settings live in the sound board's EEPROM (battery save)
        "paperboy" => Ok(Box::new(atarisy2::sy2_system::Sy2System::new(
            atarisy2::sy2_system::Variant::Paperboy, set_name, regions)?)),
        "gberet" | "mrgoemon" => {
            use gberet::gberet_system::{GberetSystem, Variant};
            let v = if driver == "mrgoemon" { Variant::MrGoemon } else { Variant::GreenBeret };
            Ok(wrap(GberetSystem::new(v, set_name, regions)?, hs, "Konami"))
        },
        "ddragon" | "ddragon2" => {
            use ddragon::dd_system::{DdSystem, Variant};
            let v = if driver == "ddragon2" { Variant::DoubleDragon2 } else { Variant::DoubleDragon };
            Ok(wrap(DdSystem::new(v, set_name, regions)?, hs, "Technos"))
        },
        // settings and high scores live in the board's EEPROM (battery save)
        "simpsons" | "simpsons2p" => Ok(Box::new(simpsons::simpsons_system::SimpsonsSystem::new(
            set_name, regions, driver == "simpsons2p")?)),
        other => Err(format!("no arcade driver name '{other}'")),
    }
}

pub fn take_region(regions: &mut HashMap<String, Vec<u8>>, name: &str) -> Result<Vec<u8>, String> {
    regions.remove(name).ok_or_else(|| format!("ROM region '{name}' missing"))
}#[cfg(test)]
mod state_tests;
