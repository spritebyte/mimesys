pub trait Mapper {
    fn cpu_read(&self, addr: u16) -> u8;
    fn cpu_write(&mut self, addr: u16, value: u8);
    fn ppu_read(&self, addr: u16) -> u8;
    fn ppu_read_ctx(&self, addr: u16, is_bg_fetch: bool) -> u8 {
        let _ = is_bg_fetch;
        self.ppu_read(addr)
    }
    fn ppu_write(&mut self, addr: u16, value: u8);
    fn mirror_vram_address(&self, addr: u16) -> usize;
    fn read_nametable_byte(&self, addr: u16, ppu_vram: &[u8; 4096], is_attribute_byte: bool) -> u8 {
        let _ = is_attribute_byte;
        ppu_vram[self.mirror_vram_address(addr)]
    }
    fn is_irq_asserted(&self) -> bool { false }
    fn step_cycles(&mut self, _cycles: u64) {}
    fn total_cycles(&self) -> u64 { 0 }
    fn get_sram(&self) -> Option<&[u8]> { None }
    fn load_sram(&mut self, _data: &[u8]) {}
    fn is_sram_dirty(&self) -> bool { false }
    fn clear_sram_dirty(&mut self) {}
    fn check_a12(&self, _addr: u16) {}
    fn clock_scanline(&mut self) {}
    fn notify_frame_start(&mut self) {}
    fn split_config(&self) -> Option<(bool,u8)> { None }
    fn read_split_tile(&self, _screen_x: usize, _scanline: usize) -> (u8,u8,u8) { (0,0,0) }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Mirroring {
	Horizontal,
	Vertical,
	SingleLower, // Maps everything to $2000 (VRAM 0-1023)
	SingleUpper, // Maps everything to $2400 (VRAM 1024-2047)
	FourScreen,
}

// Mapper 0 (NROM) - Standard flat cartridge, no bank switching
pub struct Mapper0 {
    prg_banks: usize,
    mirroring_mode: Mirroring,
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    prg_ram: Vec<u8>,
}

impl Mapper0 {
    pub fn new(prg_banks: usize, chr_banks: usize, prg_rom: Vec<u8>, chr_rom: Vec<u8>, initial_mirroring: Mirroring) -> Self {
        Self {
            prg_banks,
            mirroring_mode: initial_mirroring,
            prg_rom,
            chr_rom,
            prg_ram: vec![0; 8192],
        }
    }
}

impl Mapper for Mapper0 {
    fn cpu_read(&self, addr: u16) -> u8 {
        if addr < 0x8000 {
            if addr >= 0x6000 {
                return self.prg_ram[(addr - 0x6000) as usize];
            }
            return 0;
        }
        let mut rom_addr = addr - 0x8000;
        if self.prg_rom.len() == 16384 {
            rom_addr %= 16384; // Mirroring for 16KB games
        }
        self.prg_rom[rom_addr as usize]
    }
    fn cpu_write(&mut self, addr: u16, value: u8) {
        if addr < 0x8000 {
            if addr >= 0x6000 {
                // Write to the 8KB PRG RAM area
                self.prg_ram[(addr - 0x6000) as usize] = value;
            }
            return;
        }
        // ignore writes above $8000 for Mapper 0 since prg-rom is read-only.
    }

    fn ppu_read(&self, addr: u16) -> u8 {
        if self.chr_rom.len() > 0 {
            let masked_addr = (addr & 0x1FFF) as usize;
            return self.chr_rom[masked_addr % self.chr_rom.len()];
        }
        0
    }

    fn ppu_write(&mut self, addr: u16, value: u8) {
        // handle chr_ram writes or modifications if needed
    }

    fn mirror_vram_address(&self, addr: u16) -> usize {
        let normalized = (addr & 0x0FFF) as usize; // Map $2000-$2FFF to $000-$FFF
        match self.mirroring_mode {
            Mirroring::Horizontal => {
                // Nametables 0 and 1 map to first 1KB; Nametables 2 and 3 map to second 1KB
                if normalized < 0x800 {
                    normalized % 0x400
                } else {
                    0x400 + (normalized % 0x400)
                }
            }
            Mirroring::Vertical => {
                // Nametables 0 and 2 map to first 1KB; Nametables 1 and 3 map to second 1KB
                normalized % 0x800
            }
            _ => normalized % 2048,
        }
    }
}