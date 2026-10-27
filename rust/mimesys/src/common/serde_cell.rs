//! serde helpers for state that lives in an `UnsafeCell` (the Game Boy bus shares its
//! PPU / APU / timer / mapper with the CPU through cells).

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::cell::UnsafeCell;

pub fn serialize<S: Serializer, T: Serialize>(c: &UnsafeCell<T>, s: S) -> Result<S::Ok, S::Error> {
    // SAFETY: saving happens between frames, when nothing else holds the cell's contents
    unsafe { (*c.get()).serialize(s) }
}

pub fn deserialize<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<UnsafeCell<T>, D::Error> {
    T::deserialize(d).map(UnsafeCell::new)
}
