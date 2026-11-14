use crate::common::cpu::z80::{Z80Cpu, Bus};

const STATE_VERSION: u32 = 1;
const MAIN_CLOCK: u64 = 4_000_000;

pub struct 1942Bus {
    rom: Vec<u8>,
    ram: Vec<u8>,
    pub video: Video,
    audiorom_raw: Vec<u8>,
    pub cycle: u64,
    pub irq2: bool,
    pub in0: u8,
    pub in1: u16,
    pub in2: u16,
    pub dsw0: u16,
    pub dsw1: [u8; 3],
}

pub struct 1942System {
    cpu: Z80Cpu,
    pub bus: Bus,
    current_frame: u64,
    save_filename: String,
    save_path: String,
    state_path: String,
}

impl 1942System {
    pub fn new(set_name: &str, mut regions: HashMap<String, Vec<u8>>) -> Result<Self, String> {
        let r0 = take_region(&mut regions, "maincpu")?;
        let proms = take_region(&mut regions, "proms")?;
        let sound = take_region(&mut regions, "audiocpu")?;
        let chars = take_region(&mut regions, "chars")?;

    }
}