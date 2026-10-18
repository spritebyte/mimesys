use serde::{Serialize, Deserialize};
use crate::common::cpu::m6502::CpuState;
use crate::nes::nes_ppu::PpuState;
use crate::nes::nes_apu::ApuState;


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
    pub dmc_stall_remaining: u16,
    pub nmi_line_snapshot: bool,
    pub irq_line_snapshot: bool,
    pub open_bus: u8,
    pub pad2_shift_reg: u8,
    pub total_ppu_dots: u64,
    pub dot_in_cycle: u32,
}

