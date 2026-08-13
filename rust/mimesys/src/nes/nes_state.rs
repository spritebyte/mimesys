//use crate::common::m6502::StatusFlags;
use serde::{Serialize, Deserialize};
use crate::common::m6502::CpuState;
use crate::nes::nes_ppu::PpuState;
use crate::nes::nes_apu::ApuState;
//use crate::nes::mappers::Mapper;


#[derive(Serialize, Deserialize, Clone)]
pub struct NesSaveState {
    pub version: f32,
    pub cpu: CpuState,
    pub ppu: PpuState,
    pub apu: ApuState,
    #[serde(with = "serde_bytes")]
    pub ram: [u8; 2048],
    pub mapper_number: u16,
    #[serde(with = "serde_bytes")]
    pub mapper_data: Vec<u8>,
    pub pad1_state: u8,
    pub pad1_shift_reg: u8,
    pub pad_strobe: bool,
    pub dma_cycles_remaining: u16,
    pub dma_base_address: u16,
    pub dma_temp_buffer: u8,
    pub bus_available: bool,
    pub total_cpu_cycles: u64,
}

