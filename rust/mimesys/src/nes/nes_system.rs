use crate::common::cpu::m6502::{M6502Cpu, CpuVariant, AddressBus};
use crate::nes::nes_bus::NesBus;
use crate::nes::cartridge::Cartridge;
use crate::nes::mappers::{self, Mirroring};
use crate::nes::nes_state::NesSaveState;
use crate::nes::nes_rewind::RewindBuffer;
use crate::common::system::{System,DisplayInfo,Rotation};

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc};
use std::path::PathBuf;

const PRG_ROM_COUNT_IDX: u8 = 0x04;
const CHR_ROM_COUNT_IDX: u8 = 0x05;
const CONTROL_BYTE_1_IDX: u8 = 0x06;
const CONTROL_BYTE_2_IDX: u8 = 0x07;
const INES2_MAPPER_BYTE:  u8 = 0x08;
const PRG_ROM_PAGE_SIZE: u16 = 1024 * 16;
const CHR_ROM_PAGE_SIZE: u16 = 1024 * 8; 


pub struct NesSystem {
    cpu: M6502Cpu,
    bus: NesBus,
    frame_ready: Arc<AtomicBool>,
    is_running: Arc<AtomicBool>,
    save_battery_path: String,
    save_state_path: String,
    save_filename: String,
    pending_save_request: Option<PathBuf>,
    pending_load_request: Option<Vec<u8>>,
    rewind_buffer: RewindBuffer,
    current_frame: u64,
    pub frame_cache: Vec<u8>,
}

impl NesSystem {
    pub fn from_rom(rom_bytes: &[u8], base_name: &str, bios: &[u8]) -> Result<Self, String> {
        // Validate header and get sizes
        let bytes = rom_bytes;
        let buffer_size = 256 * 240 * 4;
        if bytes.len() < 16 || &bytes[0..4] != b"NES\x1A" { return Err("Not a valid NES ROM".into()); }

        let prg_rom_size = bytes[4] as usize * PRG_ROM_PAGE_SIZE as usize;
        let bytes_chr = bytes[5] as usize;
        let chr_rom_size = bytes_chr * CHR_ROM_PAGE_SIZE as usize;

        // extract mapper id
        let mapper_byte = bytes[INES2_MAPPER_BYTE as usize];

        let mapper_low = bytes[CONTROL_BYTE_1_IDX as usize] >> 4;
        let mapper_mid = bytes[CONTROL_BYTE_2_IDX as usize] & 0xF0;
        let mapper_high = ((mapper_byte & 0x0F) as u16) << 8;
        let mapper_id: u16;
        // slice ROM bytes
        let has_trainer:bool = (bytes[CONTROL_BYTE_1_IDX as usize] & 0b100) != 0;
        let ines2: bool = (bytes[CONTROL_BYTE_2_IDX as usize] & 0b1100) == 0x08;
        let submapper = (mapper_byte & 0xF0) >> 4;
        if ines2 {
            mapper_id = mapper_high | mapper_mid as u16 | mapper_low as u16;
        } else {
            mapper_id = (mapper_mid | mapper_low) as u16;
        }
        let prg_start = 16;
        let prg_end = prg_start + prg_rom_size;
        let chr_end = prg_end + (bytes[5] as usize * 8192);

        let prg_rom = bytes[prg_start..prg_end].to_vec();
        let chr_rom = if bytes[5] > 0 { bytes[prg_end..chr_end].to_vec() } else { vec![] };
        let header = bytes[0..16].to_vec();

        // initialize the mapper
        let mirroring_bit = (header[6] & 0x01) != 0;
        let has_battery = (header[6] & 0x02) != 0;
        let initial_mirroring:Mirroring = if mirroring_bit { Mirroring::Vertical } else { Mirroring::Horizontal };
        let prg_bank_size = match mapper_id {
            4|206 => 8192,   // MMC3, DxROM: 8KB banks
            _ => 16384,      // MMC, UxROM: 16KB banks
        };
        let chr_bank_size = match mapper_id {
            4|206 => 1024,  // MMC3, DxROM: 1KB banks
            _ => 8192,      // MMC, UxROM: 8KB banks
        };
        let prg_banks = prg_rom.len() / prg_bank_size;
        let chr_banks = chr_rom.len() / chr_bank_size;

        let four_screen_bit = (header[6] & 0x08) != 0;
        let mapper = mappers::make_mapper(mapper_id, submapper, prg_banks, chr_banks, prg_rom.clone(), chr_rom.clone(), initial_mirroring, four_screen_bit);
        // --- instantiate the atomic sync flag early ---
        let frame_ready = Arc::new(AtomicBool::new(false));

        // initialize cartridge and bus
        let cartridge = Cartridge::new(prg_rom, chr_rom, mapper?, base_name.to_string(), has_battery);
        let bus = NesBus::new(cartridge, Arc::clone(&frame_ready));
        let rewind_buffer = RewindBuffer::new(60, 60);


        Ok( {
            Self {
                bus,
                cpu: M6502Cpu::new(CpuVariant::Ricoh2A03),
                save_filename: base_name.to_string(),
                frame_ready,
                is_running: Arc::new(AtomicBool::new(false)),
                save_battery_path: "user://GD_EMU/NES/Save".to_string(),
                save_state_path: "user://GD_EMU/NES/State".to_string(),
                pending_load_request: None,
                pending_save_request: None,
                rewind_buffer,
                current_frame: 0,
                frame_cache: vec![0; buffer_size],
            }
        })
    }

    /*
    #[func]
    pub fn load_save(&mut self) {
        let save_path = format!("{}/{}.sav", self.save_battery_path, self.save_filename);
        if self.bus.cartridge.has_battery && godot::classes::FileAccess::file_exists(&save_path) {
            if let Some(mut file) = godot::classes::FileAccess::open(&save_path, godot::classes::file_access::ModeFlags::READ) {
                let file_length = file.get_length() as i64;
                let buffer = file.get_buffer(file_length);
                self.bus.load_sram(buffer.as_slice());
                godot_print!("SRAM loaded successfully during power_on.");
                println!("SRAM loaded successfully during power_on.");
            }
        } else { println!("File doesn't exist at {save_path}"); }
    }
    */

/*
    #[func]
    pub fn request_rewind_to_frame(&mut self, target_frame: i64) -> bool {
        let mut buffer = std::mem::take(&mut self.rewind_buffer);
        let result = buffer.rewind_to(target_frame as u64, self);
        self.rewind_buffer = buffer;
        result.is_ok()
    }

    */
    pub fn set_current_frame(&mut self, frame: u64) {
        self.current_frame = frame;
    }

    /*
    #[func]
    pub fn get_oldest_rewindable_frame(&self) -> i64 {
        self.rewind_buffer.oldest_available_frame().unwrap_or(0) as i64
    }


    #[func]
    pub fn reset(&mut self) {
        self.bus.ppu.get_mut().reset();
        self.cpu.reset(&mut self.bus);
            // self.bus.reset_ram();
    }
    */

    pub fn build_save_state(&self) -> NesSaveState {
        NesSaveState {
            version: 0.2,
            cpu: self.cpu.get_state(),
            ppu: unsafe { (*self.bus.ppu.get()).get_state() },
            apu: unsafe { (*self.bus.apu.get()).get_state() },
            ram: self.bus.ram,
            pad1_state: self.bus.pad1_state,
            pad1_shift_reg: self.bus.pad1_shift_reg.get(),
            pad_strobe: self.bus.pad_strobe,
            dma_cycles_remaining: self.bus.dma_cycles_remaining,
            dma_base_address: self.bus.dma_base_address,
            dma_temp_buffer: self.bus.dma_temp_buffer,
            bus_available: self.bus.bus_available,
            total_cpu_cycles: self.bus.total_cpu_cycles,
            mapper_number: self.bus.cartridge.mapper_number(),
            mapper_data: self.bus.cartridge.mapper().save_state(),
            dmc_stall_remaining: self.bus.dmc_stall_remaining,
            nmi_line_snapshot: self.bus.nmi_line_snapshot,
            irq_line_snapshot: self.bus.irq_line_snapshot,
            open_bus: self.bus.open_bus.get(),
            pad2_shift_reg: self.bus.pad2_shift_reg.get(),
            total_ppu_dots: self.bus.total_ppu_dots,
            dot_in_cycle: self.bus.dot_in_cycle,
        }
    }

    pub fn apply_save_state(&mut self, state: &NesSaveState) -> Result<(), String> {
        if state.mapper_number != self.bus.cartridge.mapper_number() {
            return Err(format!(
                "Save state mapper ({}) doesn't match loaded ROM's mapper ({})",
                state.mapper_number, self.bus.cartridge.mapper_number()
            ));
        }

        unsafe { (*self.bus.ppu.get()).load_state(&state.ppu); }
        unsafe { (*self.bus.apu.get()).load_state(&state.apu); }
        self.bus.ram = state.ram;
        self.bus.pad1_state = state.pad1_state;
        self.bus.pad1_shift_reg.set(state.pad1_shift_reg);
        self.bus.pad_strobe = state.pad_strobe;
        self.bus.dma_cycles_remaining = state.dma_cycles_remaining;
        self.bus.dma_base_address = state.dma_base_address;
        self.bus.dma_temp_buffer = state.dma_temp_buffer;
        self.bus.bus_available = state.bus_available;
        self.bus.total_cpu_cycles = state.total_cpu_cycles;
        self.bus.dmc_stall_remaining = state.dmc_stall_remaining;
        self.bus.nmi_line_snapshot = state.nmi_line_snapshot;
        self.bus.irq_line_snapshot = state.irq_line_snapshot;
        self.bus.open_bus.set(state.open_bus);
        self.bus.pad2_shift_reg.set(state.pad2_shift_reg);
        self.bus.total_ppu_dots = state.total_ppu_dots;
        self.bus.dot_in_cycle = state.dot_in_cycle;
        self.bus.cartridge.mapper_mut().load_state(&state.mapper_data);
        self.cpu.load_state(&state.cpu);

        Ok(())
    }

    /*
    #[func]
    pub fn save_state_to_file(&mut self, slot: u32) {
        match self.save_state_to_bytes() {
            Ok(state_bytes) => {
                if !godot::classes::DirAccess::dir_exists_absolute(&self.save_state_path) {
                    godot::classes::DirAccess::make_dir_recursive_absolute(&self.save_state_path);
                }
            

                let save_path = format!("{}/{}_slot{}.state", self.save_state_path, self.save_filename, slot);
            
                if let Some(mut file) = godot::classes::FileAccess::open(&save_path, godot::classes::file_access::ModeFlags::WRITE) {
                    let packed_array = PackedByteArray::from(&state_bytes[..]);
                    file.store_buffer(&packed_array);
                    godot_print!("Save state slot {} saved successfully.", slot);
                } else {
                    godot_print!("Error: Couldn't open file for save state at {}", save_path);
                }
            }
            Err(e) => godot_print!("Error serializing save state: {}", e),
        }
    }

    #[func]
    pub fn load_state_from_file(&mut self, slot: u32) {
        let save_path = format!("{}/{}_slot{}.state", self.save_state_path, self.save_filename, slot);

        if let Some(mut file) = godot::classes::FileAccess::open(&save_path, godot::classes::file_access::ModeFlags::READ) {
            let file_length = file.get_length() as i64;
            let packed_array = file.get_buffer(file_length);
            let state_bytes = packed_array.to_vec();

            let _ = self.load_state_from_bytes(&state_bytes);
        } else {
            godot_print!("No save state found at {}", save_path);
        }
    }

    #[func]
    pub fn check_and_save_sram(&mut self) {
        // If the mapper says no writes have happened to $6000-$7FFF, exit instantly.
        // This makes the function call practically free 99.9% of the time.
        if !self.bus.is_sram_dirty() {
            return;
        }

        if let Some(sram_bytes) = self.bus.get_sram() {
            if !godot::classes::DirAccess::dir_exists_absolute(&self.save_battery_path) {
                godot::classes::DirAccess::make_dir_recursive_absolute(&self.save_battery_path);
                godot_print!("Created missing save directory: {}", self.save_battery_path);
            }
            let save_path = format!("{}/{}.sav", self.save_battery_path, self.save_filename);
            if let Some(mut file) = godot::classes::FileAccess::open(&save_path, godot::classes::file_access::ModeFlags::WRITE) {
//                let mut packed_array = PackedByteArray::new();
//                packed_array.extend_from_slice(sram_bytes);
                let packed_array = PackedByteArray::from(&sram_bytes[..]);
                file.store_buffer(&packed_array);
                file.flush();
                // Reset the flag so we don't save again until the game modifies SRAM again
                self.bus.clear_sram_dirty(); 
                godot_print!("SRAM auto-flushed to disk safely. {save_path}", );
            }
            else { println!("Couldn't open file for saving at {save_path}") }
        }
    }

    #[func]
    pub fn set_audio_playback(&mut self, playback: Gd<AudioStreamGeneratorPlayback>) {
        self.playback = Some(playback);
    }

    #[func]
    pub fn is_frame_ready(&self) -> bool {
        // Atomic read takes virtually zero execution cost and avoids Mutex lock stalls!
        self.frame_ready.load(Ordering::Acquire)
    }

    #[func]
    pub fn get_cpu_debug_dict(&self) -> godot::builtin::VarDictionary {
        let state = self.cpu.get_state();
        let mut dict = godot::builtin::VarDictionary::new();
        dict.set("pc", state.pc as i64);
        dict.set("a", state.a as i64);
        dict.set("x", state.x as i64);
        dict.set("y", state.y as i64);
        dict.set("sp", state.sp as i64);
        dict.set("cycles_remaining", state.cycles_remaining as i64);
        dict.set("instruction_step", state.instruction_step as i64);
        dict.set("opcode", format!("0x{:02X}", state.current_opcode));
        dict.set("flag_carry", state.status.carry);
        dict.set("flag_zero", state.status.zero);
        dict.set("flag_interrupt_disable", state.status.interrupt_disable);
        dict.set("flag_decimal", state.status.decimal);
        dict.set("flag_overflow", state.status.overflow);
        dict.set("flag_negative", state.status.negative);
        dict
    }

    #[func]
    pub fn get_ppu_debug_dict(&self) -> godot::builtin::VarDictionary {
        let ppu = unsafe { &*self.bus.ppu.get() };
        let mut dict = Dictionary::new();
        dict.set("scanline", ppu.scanline as i64);
        dict.set("cycle", ppu.cycle as i64);
        dict.set("sprite0_hit", (ppu.status & 0x40) != 0);
        dict.set("vblank_active", (ppu.status & 0x80) != 0);
        dict.set("v_addr", ppu.v_addr as i64);
        dict.set("fine_x", ppu.fine_x as i64);
        dict
    }

    #[func]
    pub fn toggle_debug_trace(&mut self) {
        let ppu = self.bus.ppu.get_mut();
        ppu.toggle_debug_trace();
    }

    #[func]
    pub fn get_frame_texture(&mut self) -> Gd<Texture2D> {
        self.frame_ready.store(false, Ordering::Release);
        let raw_pixels = self.bus.ppu.get_mut().get_front_buffer();
        let pixel_data = PackedByteArray::from_iter(raw_pixels.iter().copied());

        if self.cached_image.is_none() {
            let image = Image::create_from_data(256, 240, false, Format::RGBA8, &pixel_data).unwrap();
            let texture = ImageTexture::create_from_image(&image).unwrap();
            self.cached_image = Some(image);
            self.cached_texture = Some(texture);
        } else {
            if let Some(image) = self.cached_image.as_mut() {
                image.set_data(256, 240, false, Format::RGBA8, &pixel_data);
            }
            let image_clone = self.cached_image.as_ref().unwrap().clone();
            if let Some(texture) = self.cached_texture.as_mut() {
                texture.update(&image_clone);
            }
        }
        self.cached_texture.as_ref().unwrap().clone().upcast()
    }
    */
}

impl System for NesSystem {
    fn run_frame(&mut self, input_mask: u32) {
        if !self.is_running.load(Ordering::Relaxed) {
            return;
        }
        self.set_input(input_mask);
        let mut cpu_cycles_run:u16 = 0;
        self.bus.accesses_this_cycle = 0;
        while !unsafe { (*self.bus.ppu.get()).is_frame_complete() } {
            self.bus.accesses_this_cycle = 0;
            let apu = self.bus.apu.get();
            if self.bus.dmc_stall_remaining == 0 && unsafe { (*apu).dmc_dma_request } {
                unsafe { (*apu).dmc_dma_request = false; }

                let is_even = unsafe { (*apu).even_cycle };

                self.bus.dmc_stall_remaining = if is_even { 3 } else { 4 };
            }
            if self.bus.dmc_stall_remaining > 0 {
                self.bus.begin_cpu_cycle();
                self.bus.dmc_stall_remaining -= 1;
                if self.bus.dmc_stall_remaining == 0 {
                    let apu = self.bus.apu.get();
                    let addr = unsafe { (*apu).dmc_dma_address() };
                    let byte = self.bus.read_byte(addr);   // real read — supplies this cycle's 3 dots
                    unsafe { (*apu).dmc_dma_complete(byte) };
                } else {
                    self.bus.step_ppu_dots(3);             // stall cycle, no bus access
                }
                self.cpu.sample_interrupt_lines(&mut self.bus);
                continue;
            }
            if !self.bus.bus_available {
//                self.bus.step_one_cycle();
                self.bus.step_dma_one_cycle(&mut self.cpu);
                self.cpu.sample_interrupt_lines(&mut self.bus);
            } else {
//                self.bus.step_one_cycle();
                self.bus.begin_cpu_cycle();
                self.cpu.step_one_cycle(&mut self.bus);
//                self.bus.step_remaining_ppu_cycles();
                match self.bus.accesses_this_cycle {
                    1 => {}
                    0 => {
                        // Internal cycle with no bus access — hardware would do a dummy read.
                        self.bus.step_ppu_dots(3);
                    }
                    n => panic!("{} bus accesses in one CPU cycle (opcode {:02X})",
                            n, self.cpu.current_opcode),
                }
                self.cpu.sample_interrupt_lines(&mut self.bus);
            }
            cpu_cycles_run += 1;

            // failsafe
            if cpu_cycles_run > 35000 {
                break;
            }
        }
        let ppu = self.bus.ppu.get();
        let apu = self.bus.apu.get();
        let mapper = self.bus.cartridge.mapper_mut();
//        godot_print!("[{}] CPU Cycles={}|PPU Cycles/3={}|APU Cycles={}, even_cycle={}|Mapper Cycles={}. Scanline={}|PPU dot={}", self.current_frame, self.cpu.total_cycles, unsafe {(*ppu).total_ppu_cycles / 3}, unsafe{(*apu).total_apu_cycles}, unsafe{(*apu).even_cycle}, mapper.total_cycles(), unsafe{(*ppu).scanline}, unsafe{(*ppu).cycle});
//        debug_assert_eq!(self.bus.total_ppu_dots, self.bus.total_cpu_cycles * 3,
//            "CPU cycle performed no bus access");
        self.current_frame += 1;
        let mut buffer = std::mem::take(&mut self.rewind_buffer);
        buffer.record_frame(self.current_frame, input_mask as u16, self);
        self.rewind_buffer = buffer;

        self.frame_cache = {
            let ppu = self.bus.ppu.get_mut();
            let front_guard = ppu.front_buffer.lock().unwrap();
            front_guard.clone()
        };
        self.bus.ppu.get_mut().clear_frame_complete_flag();
        /*
        let samples = self.bus.apu.get_mut().take_audio_samples();
        if !samples.is_empty() {
            if let Some(playback) = self.playback.as_mut() {
                let frames: PackedVector2Array = samples.iter()
                    .map(|&s| Vector2::new(s, s))
                    .collect();
                playback.push_buffer(&frames);
            }
        }
        */
    }

    fn set_input(&mut self, input_mask: u32) {
        let mut nes_pad_state = 0u8;

        // Frontend layout vs NES expected bit positions:
        if (input_mask & (1 << 5)) != 0 { nes_pad_state |= 1 << 0; } // GDScript A (Bit 5)      -> NES A (Bit 0)
        if (input_mask & (1 << 4)) != 0 { nes_pad_state |= 1 << 1; } // GDScript B (Bit 4)      -> NES B (Bit 1)
        if (input_mask & (1 << 6)) != 0 { nes_pad_state |= 1 << 2; } // GDScript Select (Bit 6) -> NES Select (Bit 2)
        if (input_mask & (1 << 7)) != 0 { nes_pad_state |= 1 << 3; } // GDScript Start (Bit 7)  -> NES Start (Bit 3)
        if (input_mask & (1 << 0)) != 0 { nes_pad_state |= 1 << 4; } // GDScript Up (Bit 0)     -> NES Up (Bit 4)
        if (input_mask & (1 << 1)) != 0 { nes_pad_state |= 1 << 5; } // GDScript Down (Bit 1)   -> NES Down (Bit 5)
        if (input_mask & (1 << 2)) != 0 { nes_pad_state |= 1 << 6; } // GDScript Left (Bit 2)   -> NES Left (Bit 6)
        if (input_mask & (1 << 3)) != 0 { nes_pad_state |= 1 << 7; } // GDScript Right (Bit 3)  -> NES Right (Bit 7)

//        godot_print!("nes_pad_state: 0x{:02X}", nes_pad_state);
        self.bus.pad1_state = nes_pad_state;
    }

        fn drain_audio(&mut self) -> Vec<f32> {
        // Forward to the APU's drain_samples() through the bus.
        unsafe { (*self.bus.apu.get()).take_audio_samples().into_iter().flat_map(|s| [s, s]).collect() }
    }
 
    fn sample_rate(&self) -> u32 {
        44100   // whatever you constructed the APU with
    }

/*    fn is_frame_ready(&self) -> bool {
        self.frame_published.load(Ordering::Acquire)
    }
*/
    fn power_on(&mut self) {
        self.cpu.power_on(&mut self.bus);
        self.is_running.store(true, Ordering::SeqCst);
    }

    fn power_off(&mut self) {
        self.cpu.is_running = false;
        self.is_running.store(false, Ordering::SeqCst);
//        self.check_and_save_sram();
    }

    fn has_battery(&self) -> bool {
        self.bus.cartridge.has_battery
    }

    fn save_filename(&self) -> &str {
        &self.save_filename
    }

    fn save_battery_path(&self) -> &str {
        &self.save_battery_path
    }

    fn save_state_path(&self) -> &str {
        &self.save_state_path
    }

    fn is_sram_dirty(&self) -> bool {
        self.bus.is_sram_dirty()
    }

    fn clear_sram_dirty(&mut self) {
        self.bus.clear_sram_dirty();
    }

    fn get_current_frame(&self) -> i64 {
        self.current_frame as i64
    }

    fn get_sram(&self) -> Option<&[u8]> {
        self.bus.get_sram()
    }

    fn load_sram(&mut self, data: &[u8]) {
        self.bus.load_sram(data);
    }

    fn framebuffer(&self) -> &[u8] {
        &self.frame_cache
    }

    fn reset(&mut self) {
        self.bus.ppu.get_mut().reset();
        self.cpu.reset(&mut self.bus);
    }

    fn display_info(&self) -> DisplayInfo {
        DisplayInfo {
            buffer_width: 256,
            buffer_height: 240,
            visible_x: 0,
            visible_y: 0,
            visible_width: 256,
            visible_height: 240,
            target_aspect_ratio: 4.0/3.0,
            rotation: Rotation::None,
        }
    }

    fn save_state(&self) -> Vec<u8> {
        let nes_state = self.build_save_state();
        let config = bincode::config::standard().with_fixed_int_encoding();
        bincode::serde::encode_to_vec(&nes_state, config).unwrap_or_default()
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let config = bincode::config::standard().with_fixed_int_encoding();
        match bincode::serde::decode_from_slice::<NesSaveState, _>(data, config) {
            Ok((state, _)) => self.apply_save_state(&state),
            Err(e) => Err(format!("Failed to decode save state: {:?}", e)),
        }
    }

    fn set_audio_rate_adjust(&mut self, factor: f32) {
        self.bus.apu.get_mut().sample_rate_ratio = (1789773.0 / 44100.0) / factor;
    }
}