pub struct DebugState {
    pub paused: bool,
    pub step_mode: StepMode,        // None, SingleInstruction, SingleFrame
    pub breakpoint_pc: Option<u16>,
    pub trace_ring: VecDeque<InstructionTraceEntry>, // last ~2000 instructions, always recording
}

pub struct InstructionTraceEntry {
    pub pc: u16, pub opcode: u8, pub a: u8, pub x: u8, pub y: u8, pub sp: u8,
    pub scanline: u16, pub cycle: u16, pub total_cycles: u64,
}