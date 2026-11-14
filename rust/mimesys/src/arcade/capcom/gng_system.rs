use crate::common::cpu::m6809::{M6809, M6809Bus};

const STATE_VERSION: u32 = 1;
const MAIN_CLOCK: u64 = 6_000_000;

pub struct GngBus {
    rom: Vec<u8>,
    ram: Vec<u8>,
    pub video: Video,
    pub sound: SoundBoard,
    /// Q-Sound boards: game settings EEPROM
    pub eeprom: EepromSerial,
    audiorom_raw: Vec<u8>,
    pub cycle: u64,
    pub irq2: bool,
    pub in0: u8,
    pub in1: u16,
    pub in2: u16,
    pub in3: u16,
    pub dsw: [u8; 3],
}

pub struct GngSystem {
    cpu: M6809,
    pub bus: GngBus,
    current_frame: u64,
    save_filename: String,
    save_path: String,
    state_path: String,
}

impl GngSystem {
    pub fn new(set_name: &str, mut regions: HashMap<String, Vec<u8>>) -> Result<Self, String> {
        let r0 = take_region(&mut regions, "maincpu")?;
        let proms = take_region(&mut regions, "proms")?;
        let sound = take_region(&mut regions, "audiocpu")?;
        let chars = take_region(&mut regions, "chars")?;

    }
}