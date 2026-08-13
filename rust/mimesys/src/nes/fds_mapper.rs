// For future support
pub struct FdsMapper {
    pub bios_rom: Vec<u8>,       // 8KB BIOS loaded from disk
    pub prg_ram: [u8; 32768],    // 32KB writeable memory
    pub disk_sides: Vec<Vec<u8>>,// Vector holding the raw 65,500-byte disk data sides
    pub current_side: usize,     // Tracks if Side A or Side B is active
    // ... registers for audio and drive state
}

impl Mapper for FdsMapper {
    fn read_prg(&self, addr: u16) -> u8 {
        match addr {
            0x6000..=0xEFFF => self.prg_ram[(addr - 0x6000) as usize], // Read from volatile RAM
            0xF000..=0xFFFF => self.bios_rom[(addr - 0xF000) as usize], // Read from internal BIOS
            _ => 0,
        }
    }

    fn write_prg(&mut self, addr: u16, value: u8) {
        match addr {
            0x6000..=0xEFFF => self.prg_ram[(addr - 0x6000) as usize] = value, // Games can write to memory!
            _ => {}
        }
    }
}