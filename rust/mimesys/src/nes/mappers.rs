use serde::{Serialize, Deserialize};

pub trait Mapper {
    fn cpu_read(&self, addr: u16) -> u8;
    /// CPU read of $4020-$FFFF as the bus sees it: nothing answers at $4020-$5FFF on most
    /// boards, so the CPU reads open bus (the last value on the data bus). Mappers with
    /// registers there (MMC5, FDS) override this.
    fn cpu_read_bus(&self, addr: u16, open_bus: u8) -> u8 {
        if addr < 0x6000 { open_bus } else { self.cpu_read(addr) }
    }
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
    fn write_nametable_byte(&mut self, addr: u16, value: u8, vram: &mut [u8; 4096]) {
        let mirrored = self.mirror_vram_address(addr);
        vram[mirrored & 0x0FFF] = value;
    }
    fn is_irq_asserted(&self) -> bool { false }
    fn step_cycles(&mut self, _cycles: u64) {}
    fn total_cycles(&self) -> u64 { 0 }
    fn get_sram(&self) -> Option<&[u8]> { None }
    fn load_sram(&mut self, _data: &[u8]) {}
    fn is_sram_dirty(&self) -> bool { false }
    fn clear_sram_dirty(&mut self) {}
    fn update_a12(&mut self, _addr: u16, _ppu_cycle: u64) {}
    fn clock_scanline(&mut self) {}
    fn notify_scanline(&mut self) {}
    fn notify_frame_start(&mut self) {}
    /// A CPU write to a PPU register ($2000-$2007, reg = 0-7), before the PPU sees it.
    /// MMC5 needs PPUCTRL's sprite size.
    fn notify_ppu_write(&mut self, _reg: u16, _value: u8) {}
    /// For the event trace: the scanline IRQ counter, if this mapper has one
    fn debug_irq_counter(&self) -> Option<u8> { None }
    fn split_config(&self) -> Option<(bool,u8)> { None }
    fn read_split_tile(&self, _screen_x: usize, _scanline: usize) -> (u8,u8,u8) { (0,0,0) }
    fn save_state(&self) -> Vec<u8>;
    fn load_state(&mut self, data: &[u8]);
}

pub fn make_mapper(
    mapper_id: u16,
    submapper_id: u8,
    prg_banks: usize,
    chr_banks: usize,
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    initial_mirroring: Mirroring,
    four_screen: bool,
) -> Result<Box<dyn Mapper>, String> {
    match mapper_id {
        0 => {
            println!("Mapper0 (Nrom) created");
            Ok(Box::new(super::mapper0::Mapper0::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring)))
        }
        1 => {
            println!("Mapper1 (MMC1) created");
            Ok(Box::new(super::mapper1::Mapper1::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen)))
        }
        2 => {
            println!("Mapper2 (UxROM) created");
            Ok(Box::new(super::mapper2::Mapper2::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen, submapper_id)))
        }
        3 => {
            println!("Mapper3 (CNROM) created");
            Ok(Box::new(super::mapper3::Mapper3::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen)))
        }
        4 => {
            println!("Mapper4 (MMC3) created");
            Ok(Box::new(super::mapper4::Mapper4::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen, 4, submapper_id)))
        }
        5 => {
            println!("Mapper5 (MMC5) created");
            Ok(Box::new(super::mapper5::Mapper5::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen)))
        }
        7 => {
            println!("Mapper7 (AxROM) created");
            Ok(Box::new(super::mapper7::Mapper7::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen)))
        }
        9 => {
            println!("Mapper9 (MMC2) created");
            Ok(Box::new(super::mapper9::Mapper9::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen, false)))
        }
        10 => {
            // MMC4 is similar to MMC2 so using the same code here.
            println!("Mapper10 (MMC4) created");
            Ok(Box::new(super::mapper9::Mapper9::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen, true)))
        }
        34 => {
            println!("Mapper34 (NINA-001/NINA-002/BNROM) created");
            Ok(Box::new(super::mapper34::Mapper34::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen, submapper_id)))
        }
        64 => {
            println!("Mapper64 (Rambo-1) created");
            Ok(Box::new(super::mapper64::Mapper64::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen, submapper_id)))
        }
        66 => {
            println!("Mapper66 (GNROM) created");
            Ok(Box::new(super::mapper66::Mapper66::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring)))
        }
        69 => {
            println!("Mapper69 (Sunsoft FME-7/5A/5B) created");
            Ok(Box::new(super::mapper69::Mapper69::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen, submapper_id)))
        }
        71 => {
            println!("Mapper71 (Camerica) created");
            Ok(Box::new(super::mapper71::Mapper71::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen, submapper_id)))
        }
        118 => {
            // TODO: using same mapper code as MMC3, will need to detect variant and handle differences accordingly
            println!("Mapper118 (TLSROM/TKSROM) created");
            Ok(Box::new(super::mapper4::Mapper4::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen, 118, submapper_id)))
        }
        206 => {
            println!("Mapper206 (DxROM) created");
            Ok(Box::new(super::mapper206::Mapper206::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen)))
        }
        11 => {
            println!("Mapper11 (Color Dreams) created");
            Ok(Box::new(super::mapper11::Mapper11::new(prg_rom, chr_rom, initial_mirroring)))
        }
        21 | 22 | 23 | 25 => {
            println!("Mapper{} (Konami VRC2/VRC4, submapper {}) created", mapper_id, submapper_id);
            Ok(Box::new(super::vrc24::Vrc24::new(mapper_id, submapper_id, prg_rom, chr_rom, initial_mirroring)))
        }
        24 | 26 => {
            let is_mapper_26 = mapper_id == 26;
            println!("Mapper{} (Konami VRC6, submapper {}) created", mapper_id, submapper_id);
            Ok(Box::new(super::vrc6::MapperVRC6::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen, is_mapper_26)))
        }        
        68 => {
            println!("Mapper68 (Sunsoft-4) created");
            Ok(Box::new(super::mapper68::Mapper68::new(prg_rom, chr_rom, initial_mirroring)))
        }
        87 => {
            println!("Mapper87 (Jaleco/Konami CHR latch) created");
            Ok(Box::new(super::mapper87::Mapper87::new(prg_rom, chr_rom, initial_mirroring)))
        }
        95 => {
            println!("Mapper95 (Namcot 3425) created");
            Ok(Box::new(super::mapper95::Mapper95::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring)))
        }
        // Add future mappers here. The system file never has to change!
        _ => Err(format!("Mapper {} not implemented yet", mapper_id)),
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub enum Mirroring {
	Horizontal,
	Vertical,
	SingleLower, // Maps everything to $2000 (VRAM 0-1023)
	SingleUpper, // Maps everything to $2400 (VRAM 1024-2047)
	FourScreen,
}


/// CIRAM offset ($000-$7FF, or $000-$FFF for four-screen) for a nametable address under
/// a mirroring mode. Shared by the newer mappers.
pub fn mirror_nt(mode: Mirroring, addr: u16) -> usize {
    let v = (addr as usize) & 0x0FFF;
    match mode {
        Mirroring::Vertical => v & 0x07FF,
        Mirroring::Horizontal => ((v >> 1) & 0x0400) | (v & 0x03FF),
        Mirroring::SingleLower => v & 0x03FF,
        Mirroring::SingleUpper => 0x0400 | (v & 0x03FF),
        Mirroring::FourScreen => v,
    }
}

/// bincode helpers for mapper save states
pub fn encode_state<T: Serialize>(state: &T) -> Vec<u8> {
    bincode::serde::encode_to_vec(state, bincode::config::standard().with_fixed_int_encoding()).unwrap_or_default()
}
pub fn decode_state<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Option<T> {
    bincode::serde::decode_from_slice::<T, _>(bytes, bincode::config::standard().with_fixed_int_encoding()).ok().map(|(s, _)| s)
}
