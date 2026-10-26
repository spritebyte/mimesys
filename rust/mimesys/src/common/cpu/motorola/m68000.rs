use serde::{Serialize, Deserialize};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum CpuVariant {
    M68000,
    M68008,
    M68010,
}

