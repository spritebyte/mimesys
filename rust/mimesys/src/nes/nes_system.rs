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
    brk_trace_saved: bool,
}

impl NesSystem {
    pub fn from_rom(rom_bytes: &[u8], base_name: &str, bios: &[u8]) -> Result<Self, String> {
        if crate::nes::fds::is_fds(rom_bytes) { return Self::from_fds(rom_bytes, base_name, bios); }
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
        // byte 8 is only a submapper in NES 2.0 (old iNES files can have junk there)
        let submapper = if ines2 { (mapper_byte & 0xF0) >> 4 } else { 0 };
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
        let mut initial_mirroring:Mirroring = if mirroring_bit { Mirroring::Vertical } else { Mirroring::Horizontal };
        // known-bad headers (rom_db), then NES_MIRRORING=V|H for testing
        let mut submapper = submapper;
        let crc = crate::nes::rom_db::crc32(&[&prg_rom, &chr_rom]);
        println!("NES ROM: PRG+CHR CRC32 {crc:08X}, mapper {mapper_id}.{submapper}, header mirroring {}",
            if mirroring_bit { "vertical" } else { "horizontal" });
        if let Some(fix) = crate::nes::rom_db::lookup(crc) {
            println!("NES ROM: header fix for {}", fix.name);
            if let Some(m) = fix.mirroring { initial_mirroring = m; }
            if let Some(sm) = fix.submapper { submapper = sm; }
        } else {
            println!("NES ROM: no header fix ({} entries in rom_db)", crate::nes::rom_db::entries());
        }
        match std::env::var("NES_MIRRORING").ok().as_deref() {
            Some("V") | Some("v") => initial_mirroring = Mirroring::Vertical,
            Some("H") | Some("h") => initial_mirroring = Mirroring::Horizontal,
            _ => {}
        }
        let prg_bank_size = match mapper_id {
            4|95|206 => 8192,   // MMC3, DxROM, Namcot 3425: 8KB banks
            _ => 16384,      // MMC, UxROM: 16KB banks
        };
        let chr_bank_size = match mapper_id {
            4|95|206 => 1024,  // MMC3, DxROM, Namcot 3425: 1KB banks
            _ => 8192,      // MMC, UxROM: 8KB banks
        };
        let prg_banks = prg_rom.len() / prg_bank_size;
        let chr_banks = chr_rom.len() / chr_bank_size;

        let four_screen_bit = (header[6] & 0x08) != 0;
        // VS System: old dumps of mapper-99 boards say mapper 0 (NROM can't have > 8 KB CHR)
        let vs = crate::nes::vs::header(&header);
        let mut mapper_id = mapper_id;
        if vs.is_some() && mapper_id == 0 && (chr_rom.len() > 0x2000 || prg_rom.len() > 0x8000) { mapper_id = 99; }
        let vs_ppu = vs.map(|h| h.ppu.or_else(|| crate::nes::vs::ppu_from_name(base_name)).unwrap_or(crate::nes::vs::VsPpu::Rp2c03b));
        if let Some(h) = vs {
            println!("NES ROM: VS System, {} PPU ({}){}", vs_ppu.unwrap().name(),
                if h.ppu.is_some() { "from the NES 2.0 header" } else { "header doesn't say: from the name, or the 2C03" },
                match h.hardware { 0 => "", 1..=3 => "; extra protection chip not emulated", 4 => "", _ => "; DualSystem (two screens) not emulated: first CPU only" });
        }
        let mapper = mappers::make_mapper(mapper_id, submapper, prg_banks, chr_banks, prg_rom.clone(), chr_rom.clone(), initial_mirroring, four_screen_bit);
        // --- instantiate the atomic sync flag early ---
        let frame_ready = Arc::new(AtomicBool::new(false));

        // initialize cartridge and bus
        let cartridge = Cartridge::new(prg_rom, chr_rom, mapper?, base_name.to_string(), has_battery);
        let _ = frame_ready;
        let mut sys = Self::with_cartridge(cartridge, base_name);
        if vs.is_some() {
            let mut v = crate::nes::vs::VsInput::default();
            v.detected = vs_ppu;
            sys.bus.vs = Some(v);
            sys.bus.ppu.get_mut().set_vs_ppu(vs_ppu);
        }
        Ok(sys)
    }

    /// A Famicom Disk System disk (.fds): the RAM adapter is the "cartridge".
    pub fn from_fds(image: &[u8], base_name: &str, bios: &[u8]) -> Result<Self, String> {
        let fds = crate::nes::fds::Fds::new(image, bios)?;
        let cartridge = Cartridge::new(vec![], vec![], Box::new(fds), base_name.to_string(), true);
        let mut sys = Self::with_cartridge(cartridge, base_name);
        sys.save_battery_path = "user://GD_EMU/FDS/Save".to_string();
        sys.save_state_path = "user://GD_EMU/FDS/State".to_string();
        Ok(sys)
    }

    fn with_cartridge(cartridge: Cartridge, base_name: &str) -> Self {
        let buffer_size = 256 * 240 * 4;
        let frame_ready = Arc::new(AtomicBool::new(false));
        let bus = NesBus::new(cartridge, Arc::clone(&frame_ready));
        let rewind_buffer = RewindBuffer::new(60, 60);
        {
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
                brk_trace_saved: false,
            }
        }
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
            dmc_stall_remaining: self.bus.dma_flags(),
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
        self.bus.set_dma_flags(state.dmc_stall_remaining);
        self.bus.nmi_line_snapshot = state.nmi_line_snapshot;
        self.bus.irq_line_snapshot = state.irq_line_snapshot;
        self.bus.open_bus.set(state.open_bus);
        self.bus.internal_bus = state.open_bus;
        self.bus.pad2_shift_reg.set(state.pad2_shift_reg);
        self.bus.total_ppu_dots = state.total_ppu_dots;
        self.bus.dot_in_cycle = state.dot_in_cycle;
        self.bus.resync_master_clock();
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

impl NesSystem {
    /// Frontend button layout (common::input) -> NES shift register order
    /// (A, B, Select, Start, Up, Down, Left, Right), without opposite directions.
    fn nes_pad_bits(m: u32) -> u8 {
        use crate::common::input as pad;
        let order = [pad::A, pad::B, pad::SELECT, pad::START, pad::UP, pad::DOWN, pad::LEFT, pad::RIGHT];
        let mut s = 0u8;
        for (i, b) in order.iter().enumerate() {
            if m & b != 0 { s |= 1 << i; }
        }
        if s & 0x30 == 0x30 { s &= !0x30; }
        if s & 0xC0 == 0xC0 { s &= !0xC0; }
        s
    }
}

impl System for NesSystem {
    fn set_option(&mut self, key: &str, value: i64) {
        if key == "zapper" {
            self.bus.zapper_on = value != 0;
        }
        // Famicom Disk System: -1 = next side, -2 = eject, else that side (0 = 1A, 1 = 1B...)
        if key == "fds_side" {
            if let Some(f) = self.bus.cartridge.mapper_mut().as_fds() {
                match value { -1 => f.next_side(), -2 => f.insert(None), n => f.insert(Some(n as usize)) }
            }
        }
        if let Some(v) = &mut self.bus.vs {
            match key {
                "vs_dip" => v.dips = value as u8,
                "vs_swap" => v.swap = value != 0,
                // -1 = as detected at load; else the NES 2.0 PPU number
                "vs_ppu" => {
                    let p = if value < 0 { v.detected } else { crate::nes::vs::VsPpu::from_nes2(value as u8).or(v.detected) };
                    if let Some(p) = p { println!("NES: VS PPU {}", p.name()); }
                    self.bus.ppu.get_mut().set_vs_ppu(p);
                }
                _ => {}
            }
        }
    }

    /// Mouse as Zapper: left button fires at the pointer, right button fires off screen
    /// (some games use a shot away from the screen, e.g. to reload).
    fn set_pointer(&mut self, x: i32, y: i32, buttons: u32) {
        let off = buttons & 2 != 0;
        self.bus.zapper_x = if off { -1 } else { x };
        self.bus.zapper_y = if off { -1 } else { y };
        self.bus.zapper_trigger = buttons & 3 != 0;
    }

    fn run_frame(&mut self, input_mask: u32) {
        if !self.is_running.load(Ordering::Relaxed) {
            return;
        }
        self.set_input(input_mask);
        if let Some(v) = &mut self.bus.vs { let (coin, service) = self.bus.vs_buttons; v.frame(coin, service); }
        let mut cpu_cycles_run:u16 = 0;
        self.bus.accesses_this_cycle = 0;
        while !unsafe { (*self.bus.ppu.get()).is_frame_complete() } {
            self.bus.apply_cheat_writes();
            self.bus.accesses_this_cycle = 0;
            // DMC DMA is serviced by the bus at the next CPU read (see NesBus::run_dmc_dma)
            {
//                self.bus.step_one_cycle();
                self.bus.begin_cpu_cycle();
                self.cpu.step_one_cycle(&mut self.bus);
//                self.bus.step_remaining_ppu_cycles();
                debug_assert!(self.bus.accesses_this_cycle <= 1,
                    "{} bus accesses in one CPU cycle (opcode {:02X})",
                    self.bus.accesses_this_cycle, self.cpu.current_opcode);
                self.bus.end_cpu_cycle();
                self.cpu.sample_interrupt_lines(&mut self.bus);
            }
            cpu_cycles_run += 1;
            if self.cpu.first_brk_log.is_some() && !self.brk_trace_saved {
                self.brk_trace_saved = true;
                if let Some(t) = self.bus.trace.as_ref() {
                    t.save_partial(".brk.txt");
                    if let Some(log) = &self.cpu.first_brk_log {
                        let _ = std::fs::write(format!("{}.cpu.txt", t.path()), log);
                    }
                }
            }

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
        if let Some(t) = self.bus.trace.as_mut() {
            t.end_frame();
            // NES_TRACE_CHR=1: also save the 8 KB of pattern memory as <trace>.chr each time
            // the trace is written (compare with Mesen's tile viewer)
            if t.just_written() && std::env::var_os("NES_TRACE_CHR").is_some() {
                let mapper = self.bus.cartridge.mapper();
                let chr: Vec<u8> = (0..0x2000u16).map(|a| mapper.ppu_read(a)).collect();
                let _ = std::fs::write(format!("{}.chr", t.path()), chr);
            }
        }
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

        // A real d-pad is one rocker: opposite directions can't be pressed together. Some
        // games index tables with the d-pad bits and crash on Up+Down or Left+Right
        // (Road Runner jumps into RAM), so drop both when a keyboard sends that.
        if nes_pad_state & 0x30 == 0x30 { nes_pad_state &= !0x30; }   // Up + Down
        if nes_pad_state & 0xC0 == 0xC0 { nes_pad_state &= !0xC0; }   // Left + Right
        let p2 = Self::nes_pad_bits(crate::common::input::player2(input_mask));
        if let Some(v) = &self.bus.vs {
            // VS: sticks + start buttons; coin = the Coin key or player 1's Select,
            // service = player 2's Select
            let (a, b) = v.reports(nes_pad_state, p2);
            self.bus.pad1_state = a;
            self.bus.pad2_state = b;
            self.bus.vs_buttons = (input_mask & crate::common::input::COIN != 0 || nes_pad_state & 0x04 != 0, p2 & 0x04 != 0);
            return;
        }
        self.bus.pad1_state = nes_pad_state;
        self.bus.pad2_state = p2;
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
        self.bus.apu.get_mut().reset();
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

    fn cheats_mut(&mut self) -> Option<&mut crate::common::cheats::Cheats> { Some(&mut self.bus.cheats) }
}
// ---------------------------------------------------------------------------
// Test ROM harness (blargg protocol): $6000 = status (0x80 running, 0x81 "press reset",
// 0x00 pass, else fail code), $6001-$6003 = DE B0 61, $6004.. = text.
// Run with: NES_TEST_ROMS=/path/to/nes-test-roms cargo test --release nes_testrom -- --ignored --nocapture
// ---------------------------------------------------------------------------
#[cfg(test)]
pub(crate) mod testrom {
    use super::*;

    pub fn run_blargg(path: &std::path::Path, max_frames: u32) -> (u8, String) {
        let Ok(rom) = std::fs::read(path) else { return (0xFF, "missing file".into()) };
        let mut sys = NesSystem::from_rom(&rom, "test", &[]).expect("load");
        sys.rewind_buffer = RewindBuffer::new(1, 1);
        sys.power_on();
        let rd = |s: &NesSystem, a: u16| s.bus.cartridge.mapper().cpu_read(a);
        let mut seen_running = false;
        let mut reset_at: Option<u32> = None;
        let mut reset_done = false;
        for f in 0..max_frames {
            sys.run_frame(0);
            let sig = (rd(&sys, 0x6001), rd(&sys, 0x6002), rd(&sys, 0x6003));
            if sig != (0xDE, 0xB0, 0x61) { continue; }
            let st = rd(&sys, 0x6000);
            match st {
                0x80 => { seen_running = true; reset_done = false; }
                0x81 => {
                    // needs a reset ~100 ms after the request; one reset per request
                    // (the status byte stays $81 until the test writes it again)
                    match reset_at {
                        None if !reset_done => reset_at = Some(f + 6),
                        Some(t) if f >= t => { sys.reset(); reset_at = None; reset_done = true; }
                        _ => {}
                    }
                }
                _ if seen_running => break,
                _ => {}
            }
        }
        let st = rd(&sys, 0x6000);
        let mut text = String::new();
        for a in 0x6004u16..0x7000 {
            let c = rd(&sys, a);
            if c == 0 { break; }
            text.push(c as char);
        }
        (st, text)
    }

    fn root() -> Option<std::path::PathBuf> {
        std::env::var_os("NES_TEST_ROMS").map(Into::into)
    }

    #[test]
    #[ignore]
    fn nes_testrom_suite() {
        let Some(root) = root() else { eprintln!("set NES_TEST_ROMS"); return };
        let list = std::env::var("NES_TEST_LIST").unwrap_or_else(|_| DEFAULT.join("\n"));
        let mut fails = 0;
        for rel in list.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let (st, text) = run_blargg(&root.join(rel), 60 * 60);
            let ok = st == 0;
            if !ok { fails += 1; }
            println!("{} {:<55} status={:02X}  {}", if ok { "PASS" } else { "FAIL" }, rel, st,
                     text.trim().replace('\n', " | "));
        }
        println!("{fails} failing");
    }

    const DEFAULT: &[&str] = &[
        "ppu_open_bus/ppu_open_bus.nes",
        "ppu_vbl_nmi/ppu_vbl_nmi.nes",
        "mmc3_test_2/rom_singles/1-clocking.nes",
        "mmc3_test_2/rom_singles/2-details.nes",
        "mmc3_test_2/rom_singles/3-A12_clocking.nes",
        "mmc3_test_2/rom_singles/4-scanline_timing.nes",
        "mmc3_test_2/rom_singles/5-MMC3.nes",
        "mmc3_test_2/rom_singles/6-MMC3_alt.nes",
        "ppu_read_buffer/test_ppu_read_buffer.nes",
        "instr_timing/instr_timing.nes",
        "cpu_interrupts_v2/cpu_interrupts.nes",
        "apu_test/apu_test.nes",
        "oam_read/oam_read.nes",
        "oam_stress/oam_stress.nes",
        "cpu_dummy_reads/cpu_dummy_reads.nes",
        "cpu_dummy_writes/cpu_dummy_writes_ppumem.nes",
        "cpu_dummy_writes/cpu_dummy_writes_oam.nes",
        "instr_test-v5/all_instrs.nes",
        "instr_misc/instr_misc.nes",
        "cpu_exec_space/test_cpu_exec_space_ppuio.nes",
        "apu_reset/4015_cleared.nes",
        "apu_reset/4017_timing.nes",
        "apu_reset/4017_written.nes",
        "apu_reset/irq_flag_cleared.nes",
        "apu_reset/len_ctrs_enabled.nes",
        "apu_reset/works_immediately.nes",
        "sprdma_and_dmc_dma/sprdma_and_dmc_dma.nes",
        "dmc_dma_during_read4/dma_2007_read.nes",
        "dmc_dma_during_read4/dma_2007_write.nes",
        "dmc_dma_during_read4/dma_4016_read.nes",
        "dmc_dma_during_read4/double_2007_read.nes",
        "dmc_dma_during_read4/read_write_2007.nes",
    ];
}

#[cfg(test)]
mod screenshot {
    use super::*;
    /// NES_SHOTS="rom1.nes:frames rom2.nes:frames" writes <rom>.ppm next to the target dir.
    #[test]
    #[ignore]
    fn nes_screenshots() {
        let Ok(list) = std::env::var("NES_SHOTS") else { return };
        let out = std::env::var("NES_SHOT_DIR").unwrap_or_else(|_| ".".into());
        for item in list.split_whitespace() {
            let (path, frames) = item.rsplit_once(':').unwrap_or((item, "120"));
            let frames: u32 = frames.parse().unwrap();
            let rom = std::fs::read(path).expect(path);
            let mut sys = NesSystem::from_rom(&rom, "shot", &[]).expect("load");
            sys.rewind_buffer = RewindBuffer::new(1, 1);
            sys.power_on();
            for _ in 0..frames { sys.run_frame(0); }
            let name = std::path::Path::new(path).file_stem().unwrap().to_string_lossy().to_string();
            let mut ppm = format!("P6\n256 240\n255\n").into_bytes();
            for px in sys.frame_cache.chunks(4) { ppm.extend_from_slice(&px[..3]); }
            std::fs::write(format!("{out}/{name}.ppm"), ppm).unwrap();
        }
    }
}

/// Replays an NES_TRACE log (register writes at their scanline/dot) into a PPU + MMC3 and
/// prints every IRQ counter change. NES_REPLAY=trace.txt cargo test --release --lib nes_replay -- --ignored --nocapture
#[cfg(test)]
mod replay {
    use crate::nes::nes_ppu::NesPPU;
    use crate::nes::mappers::Mapper;
    use crate::nes::mapper4::Mapper4;
    use crate::nes::mappers::Mirroring;
    use std::sync::{Arc, atomic::AtomicBool};

    #[test]
    #[ignore]
    fn nes_replay() {
        let Ok(path) = std::env::var("NES_REPLAY") else { return };
        let text = std::fs::read_to_string(path).unwrap();
        // (line, dot, addr, value)
        let mut ev: Vec<(u16, u32, u16, u8)> = Vec::new();
        for l in text.lines() {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() >= 4 && f[2] == "R" && f[3] == "$2007" {
                ev.push((f[0].parse().unwrap(), f[1].parse().unwrap(), 0x0007, 0));   // $0007 marks a $2007 read
            } else if f.len() == 5 && f[2] == "W" {
                let addr = u16::from_str_radix(f[3].trim_start_matches('$'), 16).unwrap();
                let val = u8::from_str_radix(f[4].trim_start_matches("= ").trim(), 16).unwrap_or(0);
                ev.push((f[0].parse().unwrap(), f[1].parse().unwrap(), addr, val));
            } else if f.len() == 6 && f[2] == "W" {
                let addr = u16::from_str_radix(f[3].trim_start_matches('$'), 16).unwrap();
                let val = u8::from_str_radix(f[5], 16).unwrap();
                ev.push((f[0].parse().unwrap(), f[1].parse().unwrap(), addr, val));
            }
        }
        let mut m = Mapper4::new(16, 128, vec![0; 128 * 1024], vec![0; 128 * 1024],
                                 Mirroring::Vertical, false, 4, 0);
        let mut ppu = NesPPU::new(Arc::new(AtomicBool::new(false)));
        // start right after the trace's frame: line 227, then run 3 frames replaying
        // events whose line >= 227 as "end of previous frame" and < 227 in the next.
        ppu.scanline = 200; ppu.cycle = 0;
        let mut last = m.debug_irq_counter();
        for pass in 0..3 {
            loop {
                let (sl, dot) = (ppu.scanline, ppu.cycle);
                for &(l, d, a, v) in &ev {
                    if l == sl && d == dot {
                        match a {
                            0x0007 => { let _ = ppu.cpu_read_reg(&mut m, 7, 0); }
                            0x2000..=0x2007 => ppu.cpu_write_reg(&mut m, a & 7, v),
                            0x8000..=0xFFFF => m.cpu_write(a, v),
                            _ => {}
                        }
                    }
                }
                ppu.step_one_cycle(&mut m);
                let c = m.debug_irq_counter();
                if c != last {
                    if pass > 0 { println!("pass {pass} line {sl:>3} dot {dot:>3}: counter {:?} -> {:?}  irq={}", last, c, m.is_irq_asserted()); }
                    last = c;
                }
                if ppu.scanline == 200 && ppu.cycle == 0 { break; }
            }
        }
    }
}

/// AccuracyCoin: press Start on the menu (runs every test), then print each test's result
/// from RAM. ACCURACYCOIN=path/AccuracyCoin.nes ACCURACYCOIN_ASM=path/AccuracyCoin.asm
/// cargo test --release --lib nes_accuracycoin -- --ignored --nocapture
#[cfg(test)]
mod accuracycoin {
    use super::*;

    #[test]
    #[ignore]
    fn nes_accuracycoin() {
        let (Ok(rom_path), Ok(asm_path)) = (std::env::var("ACCURACYCOIN"), std::env::var("ACCURACYCOIN_ASM")) else { return };
        let asm = std::fs::read_to_string(asm_path).unwrap();
        // result_Name = $NNN
        let mut addr_of = std::collections::HashMap::new();
        for l in asm.lines() {
            let t = l.trim();
            if let Some((name, rest)) = t.split_once('=') {
                let name = name.trim();
                let rest = rest.trim();
                if name.starts_with("result_") && rest.starts_with('$') {
                    if let Ok(a) = u16::from_str_radix(rest[1..].split_whitespace().next().unwrap_or(""), 16) {
                        addr_of.insert(name.to_string(), a);
                    }
                }
            }
        }
        // table "Name", $FF, result_X, TEST_X
        let mut tests = Vec::new();
        for l in asm.lines() {
            let t = l.trim();
            if t.starts_with("tblf1 \"") || t.starts_with("tblf2 \"") {
                // unofficial-opcode tables: tblfN "name", mode, suffix, $FF, result_X, TEST_X
                let rest = &t[7..];
                let Some(end) = rest.find('"') else { continue };
                let name = rest[..end].split_whitespace().collect::<Vec<_>>().join(" ");
                let fields: Vec<&str> = rest[end + 1..].split(',').map(|f| f.trim()).collect();
                if fields.len() >= 2 {
                    if let Some(&a) = addr_of.get(fields[fields.len() - 2]) { tests.push((name, a)); }
                }
                continue;
            }
            if !t.starts_with("table \"") { continue; }
            let rest = &t[7..];
            let Some(end) = rest.find('"') else { continue };
            let name = rest[..end].to_string();
            let fields: Vec<&str> = rest[end + 1..].split(',').map(|f| f.trim()).collect();
            if fields.len() >= 3 {
                if let Some(&a) = addr_of.get(fields[2]) { tests.push((name, a)); }
            }
        }
        let rom = std::fs::read(rom_path).unwrap();
        let mut sys = NesSystem::from_rom(&rom, "accuracycoin", &[]).unwrap();
        sys.rewind_buffer = RewindBuffer::new(1, 1);
        sys.power_on();
        let frames: u32 = std::env::var("ACCURACYCOIN_FRAMES").ok().and_then(|v| v.parse().ok()).unwrap_or(4000);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let skip = std::env::var("ACCURACYCOIN_SKIP").unwrap_or_default();
            for f in 0..frames {
                if f == 55 {
                    for (name, a) in &tests {
                        let only = std::env::var("ACCURACYCOIN_ONLY").unwrap_or_default();
                        let skip_it = if only.is_empty() {
                            skip.split(',').any(|k| !k.trim().is_empty() && name.eq_ignore_ascii_case(k.trim()))
                        } else {
                            !only.split(',').any(|k| name.eq_ignore_ascii_case(k.trim()))
                        };
                        if skip_it {
                            sys.bus.ram[(*a & 0x7FF) as usize] = 0xFF;    // "skip this test"
                        }
                    }
                }
                let input = if (60..66).contains(&f) { 1 << 7 } else { 0 };   // Start
                sys.run_frame(input);
                if sys.cpu.jammed { println!("*** CPU jammed at frame {f}"); break; }
            }
        }));
        if r.is_err() { println!("*** emulator panicked (see 'Last instructions' above)"); }
        let (mut pass, mut fail, mut not_run) = (0, 0, 0);
        for (name, a) in &tests {
            let v = sys.bus.ram[(*a & 0x7FF) as usize];
            let (tag, detail) = if v == 0xFF { ("SKIP", String::new()) } else { match v & 3 {
                1 => { pass += 1; ("PASS", if v >> 2 != 0 { format!("(behaviour {})", v >> 2) } else { String::new() }) }
                2 => { fail += 1; ("FAIL", format!("error {}", v >> 2)) }
                _ => { not_run += 1; ("----", format!("raw {v:02X}")) }
            } };
            println!("{tag} {name:<36} {detail}");
        }
        println!("{pass} pass, {fail} fail, {not_run} not run / other");
        if let Ok(range) = std::env::var("ACCURACYCOIN_DUMP") {
            // e.g. "6C-6F": print those RAM bytes
            let (a, b) = range.split_once('-').unwrap_or((&range, &range));
            let (a, b) = (usize::from_str_radix(a, 16).unwrap(), usize::from_str_radix(b, 16).unwrap());
            println!("RAM {a:04X}-{b:04X}: {:02X?}", &sys.bus.ram[a..=b]);
        }
    }
}

/// NES_AUDIO_ROM=rom.nes NES_AUDIO_OUT=out.f32 [NES_AUDIO_FRAMES=n]: run headless and dump
/// the APU's 44.1 kHz mono output as raw f32 (for checking the sound offline).
#[cfg(test)]
mod audio_dump {
    use super::*;
    #[test]
    #[ignore]
    fn nes_audio_dump() {
        let (Ok(rom), Ok(out)) = (std::env::var("NES_AUDIO_ROM"), std::env::var("NES_AUDIO_OUT")) else { return };
        let frames: u32 = std::env::var("NES_AUDIO_FRAMES").ok().and_then(|v| v.parse().ok()).unwrap_or(600);
        let rom = std::fs::read(rom).unwrap();
        let mut sys = NesSystem::from_rom(&rom, "audio", &[]).unwrap();
        sys.rewind_buffer = RewindBuffer::new(1, 1);
        sys.power_on();
        let mut pcm: Vec<u8> = Vec::new();
        for _ in 0..frames {
            sys.run_frame(0);
            // drain_audio returns interleaved stereo; keep the left channel
            for (i, v) in sys.drain_audio().into_iter().enumerate() {
                if i % 2 == 0 { pcm.extend_from_slice(&v.to_le_bytes()); }
            }
        }
        std::fs::write(out, pcm).unwrap();
    }
}

#[cfg(test)]
mod vs_tests {
    use super::*;
    use crate::common::system::System;

    /// A VS cartridge (NES 2.0 header, PPU `ppu`) whose program turns NMI on by writing
    /// $80 to $2001 (PPUCTRL on a 2C05, PPUMASK on others), then copies $4016 / $4017 to
    /// $10 / $11 forever; the NMI counts in $12. `mapper0` with 16 KB CHR = an old dump.
    fn vs_rom(ppu: u8, nes2: bool) -> Vec<u8> {
        let mut h = vec![b'N', b'E', b'S', 0x1A, 1, 2, 0x00, if nes2 { 0x09 } else { 0x01 }, 0, 0, 0, 0, 0, if nes2 { ppu } else { 0 }, 0, 0];
        let mut prg = vec![0xEAu8; 0x4000];
        let code = [0x78, 0xA2, 0xFF, 0x9A,             // sei; ldx #$ff; txs
                    0xA9, 0x80, 0x8D, 0x01, 0x20,       // lda #$80; sta $2001
                    0xAD, 0x16, 0x40, 0x85, 0x10,       // loop: lda $4016; sta $10
                    0xAD, 0x17, 0x40, 0x85, 0x11,       // lda $4017; sta $11
                    0x4C, 0x09, 0xC0];                  // jmp loop
        prg[..code.len()].copy_from_slice(&code);
        prg[0x100..0x103].copy_from_slice(&[0xE6, 0x12, 0x40]);   // nmi: inc $12; rti
        prg[0x3FFA..].copy_from_slice(&[0x00, 0xC1, 0x00, 0xC0, 0x00, 0xC1]);
        h.extend(prg);
        let mut chr = vec![0u8; 0x4000];
        chr[0x2000] = 0xAB;                                   // marks CHR bank 1
        h.extend(chr);
        h
    }

    fn run(rom: &[u8], name: &str, frames: u32, input: u32) -> NesSystem {
        let mut s = NesSystem::from_rom(rom, name, &[]).unwrap();
        s.rewind_buffer = RewindBuffer::new(1, 1);
        s.power_on();
        for _ in 0..frames { s.run_frame(input); }
        s
    }

    #[test]
    fn rc2c05_swaps_ctrl_and_mask() {
        let s = run(&vs_rom(8, true), "x", 10, 0);
        assert!(s.bus.ram[0x12] >= 8, "2C05: the write to $2001 turned NMI on ({} NMIs)", s.bus.ram[0x12]);
        let s = run(&vs_rom(0, true), "x", 10, 0);
        assert_eq!(s.bus.ram[0x12], 0, "2C03: $2001 is PPUMASK, no NMI");
        // an old dump that doesn't say: the name decides
        let mut s = run(&vs_rom(0, false), "Vs. Top Gun (VS)", 10, 0);
        assert!(s.bus.ram[0x12] >= 8, "Top Gun is a 2C05-04");
        assert_eq!(s.bus.ppu.get_mut().vs_status_id, Some(0x1B));
        // System Options: another PPU, then back to the detected one
        s.set_option("vs_ppu", 3);
        assert_eq!((s.bus.ppu.get_mut().vs_palette, s.bus.ppu.get_mut().vs_status_id), (Some(2), None));
        s.set_option("vs_ppu", -1);
        assert_eq!(s.bus.ppu.get_mut().vs_status_id, Some(0x1B));
    }

    #[test]
    fn dips_coins_and_old_mapper_0_dumps() {
        let mut s = run(&vs_rom(0, false), "game", 2, 0);
        assert_eq!(s.bus.ram[0x10] & 0xFE, 0, "nothing set");
        s.set_option("vs_dip", 0xFF);
        s.run_frame(0);
        s.run_frame(0);
        assert_eq!((s.bus.ram[0x10] & 0x18, s.bus.ram[0x11] & 0xFC), (0x18, 0xFC), "DIP switches 1-2 on $4016, 3-8 on $4017");
        s.run_frame(crate::common::input::COIN);
        assert_eq!(s.bus.ram[0x10] & 0x20, 0x20, "coin");
        for _ in 0..5 { s.run_frame(crate::common::input::COIN); }
        assert_eq!(s.bus.ram[0x10] & 0x20, 0, "one coin per press");
        // 16 KB of CHR on "mapper 0": really mapper 99, $4016 bit 2 switches the bank
        let m = s.bus.cartridge.mapper_mut();
        assert_eq!(m.ppu_read(0), 0);
        m.notify_4016(4);
        assert_eq!(m.ppu_read(0), 0xAB, "CHR bank 1");
    }
}

#[cfg(test)]
mod fds_tests {
    use super::*;
    use crate::common::system::System;

    /// A stand-in BIOS that reads the disk the way Nintendo's does: motor on, transfer IRQ
    /// on, the IRQ handler takes each byte from $4031 into $0200+ (count in $00); stops after 15.
    fn bios() -> Vec<u8> {
        let mut b = vec![0xEAu8; 0x2000];
        let reset = [0x78, 0xD8, 0xA2, 0xFF, 0x9A, 0xA9, 0x40, 0x8D, 0x17, 0x40,   // APU frame IRQ off
                     0xA9, 0x83, 0x8D, 0x23, 0x40,
                     0xA9, 0x2E, 0x8D, 0x25, 0x40, 0xA9, 0x2F, 0x8D, 0x25, 0x40, 0xA9, 0x2D, 0x8D, 0x25, 0x40,
                     0xA2, 0x00, 0x86, 0x00, 0xA9, 0xED, 0x8D, 0x25, 0x40, 0x58,
                     0xA5, 0x00, 0xC9, 0x0F, 0x90, 0xFA,                 // wait for 15 bytes
                     0x78, 0xA9, 0x2E, 0x8D, 0x25, 0x40, 0x4C, 0x34, 0xE0];   // (jmp to itself)
        b[..reset.len()].copy_from_slice(&reset);
        let irq = [0x48, 0x8A, 0x48, 0xA6, 0x00, 0xAD, 0x31, 0x40, 0x9D, 0x00, 0x02, 0xE8, 0x86, 0x00, 0x68, 0xAA, 0x68, 0x40];
        b[0x100..0x100 + irq.len()].copy_from_slice(&irq);
        b[0x1F0] = 0x40;
        b[0x1FFA..].copy_from_slice(&[0xF0, 0xE1, 0x00, 0xE0, 0x00, 0xE1]);
        b
    }

    #[test]
    fn bios_reads_the_disk_through_irqs() {
        let mut s = NesSystem::from_rom(&crate::nes::fds::tests::image(), "disk", &bios()).unwrap();
        s.rewind_buffer = RewindBuffer::new(1, 1);
        s.power_on();
        let mut frames = 0;
        while s.bus.ram[0] < 15 && frames < 120 { s.run_frame(0); frames += 1; }
        assert_eq!(&s.bus.ram[0x200..0x20F], b"\x01*NINTENDO-HVC*", "after {frames} frames");
        assert!(frames < 40, "spin-up + lead-in + 15 bytes takes ~19 frames: {frames}");
        // the side controls
        s.set_option("fds_side", -1);
        assert_eq!(s.bus.cartridge.mapper_mut().as_fds().unwrap().current_side(), None, "out while swapping");
        for _ in 0..70 { s.run_frame(0); }
        assert_eq!(s.bus.cartridge.mapper_mut().as_fds().unwrap().current_side(), Some(1));
        assert!(s.has_battery(), "disk saves");
        // the FDS's sound reaches the output: a square wave on the wavetable
        s.drain_audio();
        let quiet = s.drain_audio().iter().fold(0f32, |m, x| m.max(x.abs()));
        let m = s.bus.cartridge.mapper_mut();
        m.cpu_write(0x4089, 0x80);
        for i in 0..64 { m.cpu_write(0x4040 + i, if i < 32 { 63 } else { 0 }); }
        for (a, v) in [(0x4089, 0), (0x4080, 0xA0), (0x4087, 0x80), (0x4082, 0), (0x4083, 2)] { m.cpu_write(a, v); }
        for _ in 0..10 { s.run_frame(0); }
        let loud = s.drain_audio().iter().fold(0f32, |m, x| m.max(x.abs()));
        assert!(quiet < 0.01 && loud > 0.1, "FDS tone in the mix: {quiet} -> {loud}");
    }
}
