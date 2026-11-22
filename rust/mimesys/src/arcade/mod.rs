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
pub mod rastan;
pub mod tmnt;
pub mod cps1;
pub mod cps2;
pub mod bagman;
pub mod gng;
pub mod commando;
pub mod c1942;
pub mod gunsmoke;
pub mod bionicc;
pub mod dec0;
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
        "cps2" => Ok(Box::new(cps2::cps2_system::Cps2System::new(set_name, regions)?)),
        "cps1" => Ok(Box::new(cps1::cps1_system::Cps1System::new(set_name, regions)?)),
        "jedi" => Ok(Box::new(jedi::jedi_system::JediSystem::new(set_name, regions)?)),
        "rastan" => Ok(Box::new(rastan::rastan_system::RastanSystem::new(set_name, regions)?)),
        "robocop" => Ok(wrap(dec0::dec0_system::Dec0System::new(set_name, regions)?, hs, "Robocop")),
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
        "tmnt" | "tmnt2p" => Ok(Box::new(tmnt::tmnt_system::TmntSystem::new(
            set_name, regions, driver == "tmnt2p")?)),
        "simpsons" | "simpsons2p" => Ok(Box::new(simpsons::simpsons_system::SimpsonsSystem::new(
            set_name, regions, driver == "simpsons2p")?)),
        "bionicc" => Ok(Box::new(bionicc::bionicc_system::BioniccSystem::new(set_name, regions)?)),
        "gunsmoke" => Ok(wrap(gunsmoke::gunsmoke_system::GunsmokeSystem::new(set_name, regions)?, hs, "Capcom")),
        "1942" => Ok(wrap(c1942::c1942_system::C1942System::new(set_name, regions)?, hs, "Capcom")),
        // "commando_b": bootlegs that scramble the first opcode too
        "commando" | "commando_b" => Ok(wrap(commando::commando_system::CommandoSystem::new(
            set_name, regions, driver == "commando_b")?, hs, "Capcom")),
        "gng" | "diamrun" => {
            use gng::gng_system::{GngSystem, Variant};
            let v = if driver == "diamrun" { Variant::DiamondRun } else { Variant::Gng };
            Ok(wrap(GngSystem::new(v, set_name, regions)?, hs, "Capcom"))
        },
        "bagman" | "sbagman" => {
            use bagman::bagman_system::{BagmanSystem, Variant};
            let v = if driver == "sbagman" { Variant::SuperBagman } else { Variant::Bagman };
            Ok(wrap(BagmanSystem::new(v, set_name, regions)?, hs, "Valadon"))
        },
        other => Err(format!("no arcade driver name '{other}'")),
    }
}

/// Copies one ROM file into a region the way MAME's ROM_LOAD macros do. The file goes in
/// groups of `group` bytes, `step` bytes apart, starting at `offset`; `swap` swaps the bytes
/// of each 16-bit word. ROM_LOAD = (1, 1), ROM_LOAD16_BYTE = (step 2), ROM_LOAD64_BYTE =
/// (step 8), ROM_LOAD64_WORD = (group 2, step 8), ROM_LOAD16_WORD_SWAP = (group 2, step 2,
/// swap). Bytes that fall outside the region are dropped.
pub fn place_rom(dst: &mut [u8], offset: usize, src: &[u8], step: usize, group: usize, swap: bool) {
    let group = group.max(1);
    let step = step.max(group);
    if step == group && !swap {
        // one contiguous run
        let end = (offset + src.len()).min(dst.len());
        if offset < end { dst[offset..end].copy_from_slice(&src[..end - offset]); }
        return;
    }
    for (i, &b) in src.iter().enumerate() {
        let mut k = i % group;
        if swap { k ^= 1; }
        let d = offset + (i / group) * step + k;
        if d < dst.len() { dst[d] = b; }
    }
}

pub fn take_region(regions: &mut HashMap<String, Vec<u8>>, name: &str) -> Result<Vec<u8>, String> {
    regions.remove(name).ok_or_else(|| format!("ROM region '{name}' missing"))
}#[cfg(test)]
mod state_tests;
