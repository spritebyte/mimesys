use serde::{Serialize, Deserialize};
use godot::global::godot_print;

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
    fn update_a12(&mut self, _addr: u16) {}
    fn clock_scanline(&mut self) {}
    fn notify_scanline(&mut self) {}
    fn notify_frame_start(&mut self) {}
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
            godot_print!("Mapper0 (Nrom) created");
            Ok(Box::new(super::mapper0::Mapper0::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring)))
        }
        1 => {
            godot_print!("Mapper1 (MMC1) created");
            Ok(Box::new(super::mapper1::Mapper1::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen)))
        }
        2 => {
            godot_print!("Mapper2 (UxROM) created");
            Ok(Box::new(super::mapper2::Mapper2::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen, submapper_id)))
        }
        3 => {
            godot_print!("Mapper3 (CNROM) created");
            Ok(Box::new(super::mapper3::Mapper3::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen)))
        }
        4 => {
            godot_print!("Mapper4 (MMC3) created");
            Ok(Box::new(super::mapper4::Mapper4::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen, 4, submapper_id)))
        }
        5 => {
            godot_print!("Mapper5 (MMC5) created");
            Ok(Box::new(super::mapper5::Mapper5::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen)))
        }
        7 => {
            godot_print!("Mapper7 (AxROM) created");
            Ok(Box::new(super::mapper7::Mapper7::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen)))
        }
        9 => {
            godot_print!("Mapper9 (MMC2) created");
            Ok(Box::new(super::mapper9::Mapper9::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen, false)))
        }
        10 => {
            // MMC4 is similar to MMC2 so using the same code here.
            godot_print!("Mapper10 (MMC4) created");
            Ok(Box::new(super::mapper9::Mapper9::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen, true)))
        }
        34 => {
            godot_print!("Mapper34 (NINA-001/NINA-002/BNROM) created");
            Ok(Box::new(super::mapper34::Mapper34::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen, submapper_id)))
        }
        64 => {
            godot_print!("Mapper64 (Rambo-1) created");
            Ok(Box::new(super::mapper64::Mapper64::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen, submapper_id)))
        }
        66 => {
            godot_print!("Mapper66 (GNROM) created");
            Ok(Box::new(super::mapper66::Mapper66::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring)))
        }
        69 => {
            godot_print!("Mapper69 (Sunsoft FME-7/5A/5B) created");
            Ok(Box::new(super::mapper69::Mapper69::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen, submapper_id)))
        }
        118 => {
            // TODO: using same mapper code as MMC3, will need to detect variant and handle differences accordingly
            godot_print!("Mapper118 (TLSROM/TKSROM) created");
            Ok(Box::new(super::mapper4::Mapper4::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen, 118, submapper_id)))
        }
        206 => {
            godot_print!("Mapper206 (DxROM) created");
            Ok(Box::new(super::mapper206::Mapper206::new(prg_banks, chr_banks, prg_rom, chr_rom, initial_mirroring, four_screen)))
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

