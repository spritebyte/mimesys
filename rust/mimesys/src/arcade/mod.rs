pub mod donkey_kong;
pub mod pacman;

use crate::common::system::System;
use std::collections::HashMap;

pub fn load_arcade(driver: &str, set_name: &str, regions: HashMap<String, Vec<u8>>)
    -> Result<Box<dyn System>, String>
{
    match driver {
        "dkong" => Ok(Box::new(donkey_kong::dk_system::DKSystem::new(set_name.to_string(), regions)?)),
        //"pacman" => Ok(Box::new(pacman::PacmanSystem::new(set_name, regions)?)),
        other => Err(format!("no arcade driver name '{other}'")),
    }
}

pub fn take_region(regions: &mut HashMap<String, Vec<u8>>, name: &str) -> Result<Vec<u8>, String> {
    regions.remove(name).ok_or_else(|| format!("ROM region '{name}' missing"))
}