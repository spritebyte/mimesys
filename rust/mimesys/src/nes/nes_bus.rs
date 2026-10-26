use crate::common::timed::Timed;
use crate::common::cpu::m6502::AddressBus;
//use crate::nes::mappers::Mapper;
use crate::common::cheats::{Cheats, CheatSystem};
use crate::nes::cartridge::Cartridge;
use crate::nes::nes_ppu::NesPPU;
use crate::nes::nes_apu::NesAPU;
//use serde::{Serialize, Deserialize};
use std::cell::{UnsafeCell, Cell};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

pub struct NesBus {
    pub ram: [u8; 2048],
    pub cartridge: Cartridge,
    pub cheats: Cheats,
    pub ppu: UnsafeCell<NesPPU>,
    pub apu: UnsafeCell<NesAPU>,
    // Input processing fields
    pub pad1_state: u8,
    pub pad2_state: u8,
    pub pad1_shift_reg: Cell<u8>,
    pub pad2_shift_reg: Cell<u8>,
    pub pad_strobe: bool,
    pub dmc_stall_remaining: u16,
    pub dma_cycles_remaining: u16,
    pub dma_base_address: u16,
    pub dma_temp_buffer: u8,
    pub bus_available: bool,
    pub total_cpu_cycles: u64,
    pub total_ppu_dots: u64,
    pub accesses_this_cycle: u64,
    pub open_bus: Cell<u8>,
    /// the CPU's internal data bus: last value the CPU itself read or wrote. DMA reads
    /// don't touch it; $4015 reads update only this one (and take bit 5 from it)
    pub internal_bus: u8,
    decay_timer_upper: i64,
    decay_timer_lower: i64,
    pub dot_in_cycle: u32,
    pub nmi_line_snapshot: bool,
    pub irq_line_snapshot: bool,
    pub prev_nmi_line: bool,
    pub nmi_edge_latch: bool,
    // ---- master clock (NTSC: 21.477 MHz; a PPU dot is 4 ticks, a CPU cycle 12) ----
    /// where the PPU has been run up to (see `Timed`)
    pub ppu_master: u64,
    /// start of the CPU cycle in progress
    pub cycle_start: u64,
    /// a CPU cycle is in progress (begin_cpu_cycle .. end_cpu_cycle)
    in_cycle: bool,
    in_dmc_dma: bool,
    dma_on_last_read: bool,
    // ---- DMA unit (OAM + DMC share it; it can only halt the CPU on a read cycle) ----
    /// a DMA wants to halt the CPU at its next read
    pub need_halt: bool,
    /// the DMC DMA still needs its dummy cycle before it can fetch
    pub need_dummy_read: bool,
    pub dmc_dma_running: bool,
    /// $4015 disable while the DMC DMA is on its halt cycle: it ends there
    pub abort_dmc_dma: bool,
    pub sprite_dma: bool,
    /// value last written to $4016; it reaches the controller's strobe pin (OUT0) at
    /// the next get->put transition
    pub out_latch: u8,
    /// Zapper in port 2 ($4017): on/off, gun position in screen pixels (-1 = off
    /// screen), trigger held
    pub zapper_on: bool,
    pub zapper_x: i32,
    pub zapper_y: i32,
    pub zapper_trigger: bool,
    /// Event trace (set NES_TRACE=/path/file.txt before starting Godot)
    pub trace: Option<EventTrace>,
}

/// Records one frame of PPU/mapper register writes, $2002 reads and IRQ/NMI line changes
/// with the scanline and dot they happened on (like Mesen's Event Viewer). The last
/// complete frame is written to the file every 30 frames.
pub struct EventTrace {
    last_counter: Option<u8>,
    every: u64,
    prev: String,
    path: String,
    lines: Vec<String>,
    frames: u64,
}

impl EventTrace {
    fn from_env() -> Option<Self> {
        let path = std::env::var("NES_TRACE").ok()?;
        let every = std::env::var("NES_TRACE_EVERY").ok().and_then(|v| v.parse().ok()).unwrap_or(30u64).max(1);
        Some(Self { last_counter: None, every, prev: String::new(), path, lines: Vec::new(), frames: 0 })
    }
    /// Write the current frame's events so far (plus the previous complete frame) to
    /// <path><suffix>. Used when the CPU first goes astray.
    pub fn save_partial(&self, suffix: &str) {
        let mut out = String::from("previous frame:\n");
        out.push_str(&self.prev);
        out.push_str("\nthis frame, up to the event:\nline dot   event\n");
        for l in &self.lines { out.push_str(l); out.push('\n'); }
        let _ = std::fs::write(format!("{}{}", self.path, suffix), out);
    }

    pub fn path(&self) -> &str { &self.path }
    /// the last end_frame() wrote the trace file
    pub fn just_written(&self) -> bool { self.frames % self.every == 0 }

    pub fn end_frame(&mut self) {
        self.prev = self.lines.join("\n");
        self.frames += 1;
        if self.frames % self.every == 0 {
            let mut out = format!("frame {}\nline dot   event\n", self.frames);
            for l in &self.lines { out.push_str(l); out.push('\n'); }
            if std::env::var_os("NES_TRACE_APPEND").is_some() {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&self.path) {
                    let _ = f.write_all(out.as_bytes());
                }
            } else {
                let _ = std::fs::write(&self.path, out);
            }
        }
        self.lines.clear();
    }
}

pub const MASTER_PER_DOT: u64 = 4;
pub const MASTER_PER_CPU_CYCLE: u64 = 12;
/// The CPU's read/write lands 1 dot into its cycle...
const ACCESS_AT: u64 = 1 * MASTER_PER_DOT;
/// ...and the NMI/IRQ lines are sampled 2 dots in.
const INT_SAMPLE_AT: u64 = 2 * MASTER_PER_DOT;
/// APU parity (even_cycle after this cycle's APU step) on which the halt cycle can be
/// followed directly by dummy + fetch (3 cycles); the other parity needs an align cycle.
const DMC_GET_PARITY: bool = true;
/// APU parity (even_cycle after the step) of the cycle that starts with a get->put
/// transition, when OUT0 (controller strobe) takes the $4016 latch
const STROBE_PARITY: bool = false;
/// dots between the start of a $2002 read and the sprite flags being sampled
const SPRITE_FLAG_LAG: u64 = 2;

unsafe impl Send for NesBus {}
unsafe impl Sync for NesBus {}

impl NesBus {
    pub fn new(cartridge: Cartridge, system_frame_ready: Arc<AtomicBool>) -> Self {
        Self {
            ram: [0; 2048],                         // Zero out the 2KB of CPU RAM on startup
            ppu: UnsafeCell::new(NesPPU::new(system_frame_ready)),   // Initialize a fresh PPU 
            apu: UnsafeCell::new(NesAPU::new()),
            cheats: Cheats::new(CheatSystem::Nes),
            cartridge,                              // Inject the cartridge we loaded
            pad1_state: 0,
            pad2_state: 0,
            pad1_shift_reg: Cell::new(0),
            pad2_shift_reg: Cell::new(0),
            pad_strobe: false,
            bus_available: true,
            dmc_stall_remaining: 0,
            dma_cycles_remaining: 0, dma_base_address: 0, dma_temp_buffer: 0,
            total_cpu_cycles: 0,
            total_ppu_dots: 0,
            accesses_this_cycle: 0,
            open_bus: Cell::new(0),
            internal_bus: 0,
            decay_timer_lower: 0, decay_timer_upper: 0,
            dot_in_cycle: 0,
            nmi_line_snapshot: false, irq_line_snapshot: false,
            prev_nmi_line: false, nmi_edge_latch: false,
            ppu_master: 0, cycle_start: 0, in_cycle: false, in_dmc_dma: false, dma_on_last_read: false,
            need_halt: false, need_dummy_read: false, dmc_dma_running: false, abort_dmc_dma: false,
            sprite_dma: false, out_latch: 0,
            zapper_on: false, zapper_x: -1, zapper_y: -1, zapper_trigger: false,
            trace: EventTrace::from_env(),
        }
    }
    /// DMA unit flags + $4016 latch, packed for save states (stored in the old
    /// dmc_stall_remaining field so the save format is unchanged)
    pub fn dma_flags(&self) -> u16 {
        (self.need_halt as u16) | (self.need_dummy_read as u16) << 1 | (self.dmc_dma_running as u16) << 2
            | (self.abort_dmc_dma as u16) << 3 | (self.sprite_dma as u16) << 4 | (self.out_latch as u16) << 8
    }
    pub fn set_dma_flags(&mut self, f: u16) {
        self.need_halt = f & 1 != 0;
        self.need_dummy_read = f & 2 != 0;
        self.dmc_dma_running = f & 4 != 0;
        self.abort_dmc_dma = f & 8 != 0;
        self.sprite_dma = f & 16 != 0;
        self.out_latch = (f >> 8) as u8;
    }
    pub fn get_sram(&self) -> Option<&[u8]> { self.cartridge.get_sram() }
    pub fn load_sram(&mut self, data: &[u8]) { self.cartridge.load_sram(data); }
    pub fn is_sram_dirty(&self) -> bool { self.cartridge.is_sram_dirty() }
    pub fn clear_sram_dirty(&mut self) { self.cartridge.clear_sram_dirty(); }

    // TODO: needs non-mutating peek on ppu and apu
    fn peek_byte(&self, addr: u16) -> u8 {
        let value = match addr {
            0x0000..=0x1FFF => self.ram[(addr % 0x0800) as usize],
            0x4016 => {
            // Samples the current shift register bit, but DOES NOT shift it!
                let shift_reg = self.pad1_shift_reg.get();
                (shift_reg & 0x01) | (self.open_bus.get() & 0xE0)
            }

            0x4017 => {
                let shift_reg = self.pad2_shift_reg.get();
                (shift_reg & 0x01) | (self.open_bus.get() & 0xE0)
           }
            _ => self.open_bus.get(),
        };
        value
    }
    
    fn read(&self, addr: u16) -> u8 {
        let value = match addr {
            0x0000..=0x1FFF => self.ram[(addr % 0x0800) as usize],
            0x2000..=0x3FFF => {
                let register = addr % 8;
                let ppu_mut = unsafe { &mut *self.ppu.get() };
//                println!("BUS read_byte: {:04X} reg:{ :02X} ", addr, register);
                let mapper_ref = self.cartridge.mapper_mut();
                if self.trace.is_some() && matches!(register, 2 | 4 | 7) {
                    let (sl, dot, v) = (ppu_mut.scanline, ppu_mut.cycle, ppu_mut.v_addr);
                    if let Some(t) = unsafe { (*(self as *const Self as *mut Self)).trace.as_mut() } {
                        if register == 7 {
                            t.lines.push(format!("{sl:>4} {dot:>3}   R $2007 (v=${v:04X})"));
                        } else {
                            t.lines.push(format!("{sl:>4} {dot:>3}   R $200{register}"));
                        }
                    }
                }
                ppu_mut.cpu_read_reg(mapper_ref, register, self.open_bus.get())
            }
            0x4015 => {
                let apu_val = unsafe { (*self.apu.get()).read_4015() };
                let val = (apu_val & 0xDF) | (self.internal_bus & 0x20);
                val
            }
            0x4016 => {
                let shift_reg = self.pad1_shift_reg.get();
                let val = (shift_reg & 0x01) | (self.open_bus.get() & 0xE0);

                if !self.pad_strobe {
                    let next_shift = (shift_reg >> 1) | 0x80;
                    self.pad1_shift_reg.set(next_shift);
                }
                val
            }
            0x4017 if self.zapper_on => self.zapper_bits(),
            0x4017 => {
                let shift_reg = self.pad2_shift_reg.get();
                if !self.pad_strobe {
                    self.pad2_shift_reg.set((shift_reg >> 1) | 0x80);
                }
                (shift_reg & 0x01) | (self.open_bus.get() & 0xE0)
           }
            0x4020..=0xFFFF => {
                let v = self.cartridge.mapper().cpu_read_bus(addr, self.open_bus.get());
                self.cheats.patch_read(addr, v)
            },
            _ => {
                let val = self.open_bus.get();
                val
            }
        };
        // $4015 is read inside the 2A03: the value reaches the CPU but not the external
        // data bus, so open bus keeps its old value
        if addr != 0x4015 {
            self.open_bus.set(value);
        }
        value
    }

    /// Zapper on $4017: bit 4 = trigger held, bit 3 = 0 when the photodiode sees light,
    /// bits 5-7 open bus.
    fn zapper_bits(&self) -> u8 {
        let ppu = unsafe { &*self.ppu.get() };
        let light = ppu.zapper_sees_light(self.zapper_x, self.zapper_y);
        (self.open_bus.get() & 0xE0)
            | if light { 0 } else { 0x08 }
            | if self.zapper_trigger { 0x10 } else { 0 }
    }

    /// Finish the CPU cycle in progress: the PPU catches up to its end.
    pub fn end_cpu_cycle(&mut self) {
        let end = self.cycle_start + MASTER_PER_CPU_CYCLE;
        self.run_until(end);
        self.in_cycle = false;
    }

    /// Re-derive the master clock after loading a save state (saved between CPU cycles).
    pub fn resync_master_clock(&mut self) {
        self.ppu_master = self.total_ppu_dots * MASTER_PER_DOT;
        self.cycle_start = self.ppu_master;
        self.in_cycle = false;
    }

    /// One bus access: catch up to the access point of this CPU cycle, then do it.
    /// The cycle's remaining dots are run by end_cpu_cycle. An access outside a cycle
    /// (the CPU's reset sequence) gets a cycle of its own.
    #[inline]
    fn access<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        let standalone = !self.in_cycle;
        if standalone {
            self.begin_cpu_cycle();
        }
        self.run_until(self.cycle_start + ACCESS_AT);
        let v = f(self);
        self.accesses_this_cycle += 1;
        if standalone {
            self.end_cpu_cycle();
        }
        v
    }

    pub fn apply_cheat_writes(&mut self) {
        for i in 0..self.cheats.ram_writes().len() {
            let w = self.cheats.ram_writes()[i];
            match w.addr {
                0x0000..=0x1FFF => self.ram[(w.addr & 0x07FF) as usize] = w.value,
                0x6000..=0x7FFF => self.cartridge.mapper_mut().cpu_write(w.addr, w.value),
                _ => {}
            }
        }
    }
}

impl AddressBus for NesBus {
    fn is_nmi_line_asserted(&mut self) -> bool {
         self.nmi_line_snapshot
    }

    fn is_nmi_enabled(&mut self) -> bool {
         self.ppu.get_mut().is_nmi_enabled()
    }

    fn total_cycles(&self) -> u64 {
        self.total_cpu_cycles
    }

    fn is_irq_line_asserted(&mut self) -> bool {
//        self.apu.get_mut().is_irq_asserted() || self.cartridge.mapper_mut().is_irq_asserted()
        self.irq_line_snapshot
    }

    fn read_byte(&mut self, addr: u16) -> u8 {
        if !self.in_dmc_dma {
            // the halt decision uses the DMA requests from before this cycle's APU step
            let dma = self.need_halt;
            if dma {
                self.process_pending_dma(addr);
            } else {
                self.apply_dma_events();
            }
            self.dma_on_last_read = dma;
        }
        let v = if addr & 0xE007 == 0x2002 {
            // $2002: vblank is latched when the read starts, the sprite-0 / overflow bits
            // at the end of M2, ~2 dots later
            self.access(|bus| {
                let v = bus.read(addr);
                bus.run_until(bus.cycle_start + ACCESS_AT + SPRITE_FLAG_LAG * MASTER_PER_DOT);
                let flags = bus.ppu.get_mut().status & 0x60;
                let v = (v & !0x60) | flags;
                bus.open_bus.set(v);
                v
            })
        } else if addr & 0xE007 == 0x2004 {
            // $2004 is sampled late in the read (end of M2): one dot after the access point
            self.access(|bus| {
                bus.run_until(bus.cycle_start + ACCESS_AT + MASTER_PER_DOT);
                bus.read(addr)
            })
        } else {
            self.access(|bus| bus.read(addr))
        };
        if !self.in_dmc_dma {
            self.internal_bus = v;
        }
        v
    }

    fn dma_on_last_read(&self) -> bool {
        self.dma_on_last_read
    }

    fn write_byte(&mut self, addr: u16, value: u8) {
        self.apply_dma_events();
        self.internal_bus = value;
        self.access(|bus| bus.write(addr, value))
    }

    fn step_cycles(&mut self, cycles: u64) {
        for _ in 0..cycles {
            self.begin_cpu_cycle();
            self.apply_dma_events();
            self.end_cpu_cycle();
        }
    }

    /// Start a CPU cycle: the mapper's and APU's per-cycle work happens here.
    fn begin_cpu_cycle(&mut self) {
        if self.in_cycle {
            self.end_cpu_cycle();       // the previous cycle wasn't closed: finish it first
        }
        self.cycle_start = self.ppu_master.max(self.cycle_start);
        self.in_cycle = true;
        self.dot_in_cycle = 0;
        self.accesses_this_cycle = 0;
        self.cartridge.mapper_mut().step_cycles(1);
        let apu_ptr = self.apu.get();
        unsafe { (*apu_ptr).step_one_cycle(); }
        self.total_cpu_cycles += 1;
        // the $4016 latch reaches the controllers' strobe pin on a get->put transition
        if unsafe { (*apu_ptr).even_cycle } == STROBE_PARITY {
            let strobe = self.out_latch & 1 != 0;
            self.pad_strobe = strobe;
            if strobe {
                self.pad1_shift_reg.set(self.pad1_state);
                self.pad2_shift_reg.set(self.pad2_state);
            }
        }
    }
}

impl NesBus {
    /// Hand the DMC's start/stop requests (raised during this cycle's APU step) to the
    /// DMA unit. Called after the cycle's halt decision has been made.
    fn apply_dma_events(&mut self) {
        let apu = unsafe { &mut *self.apu.get() };
        if apu.dmc_stop_event {
            apu.dmc_stop_event = false;
            if self.dmc_dma_running {
                if self.need_halt {
                    // not started yet (e.g. held off by write cycles): cancel completely
                    self.dmc_dma_running = false;
                    self.need_dummy_read = false;
                    self.need_halt = false;
                } else {
                    self.abort_dmc_dma = true;
                }
            }
        }
        if apu.dmc_start_event {
            apu.dmc_start_event = false;
            self.dmc_dma_running = true;
            self.need_dummy_read = true;
            self.need_halt = true;
        }
    }

    /// The DMA unit takes over at a CPU read (the CPU's cycle has already begun). The
    /// halt cycle repeats the CPU's read; then each cycle is a "get" (DMA reads) or a
    /// "put" (OAM DMA writes $2004). The DMC needs halt + dummy before it fetches on a
    /// get cycle; OAM DMA cycles count as those. Idle cycles repeat the CPU's read
    /// (except $4016/$4017, which the controllers only see once). Ends with the CPU's own
    /// cycle begun, ready for its real read.
    fn process_pending_dma(&mut self, read_addr: u16) {
        self.in_dmc_dma = true;
        let enable_internal = (read_addr & 0xFFE0) == 0x4000;
        let is_pad = read_addr == 0x4016 || read_addr == 0x4017;
        let mut prev_read_addr = read_addr;
        let dmc_addr = unsafe { (*self.apu.get()).dmc_dma_address() };
        // the DMC reading the same register the CPU is on keeps /OE low: no extra clock
        let skip_first_input_clock = enable_internal && self.dmc_dma_running && is_pad
            && (dmc_addr & 0x1F) == (read_addr & 0x1F);
        let skip_dummy_reads = is_pad;

        // halt cycle
        self.need_halt = false;
        self.apply_dma_events();
        if (self.abort_dmc_dma && is_pad) || skip_first_input_clock {
            self.access(|_| ());
        } else {
            self.access(|bus| { bus.read(read_addr); });
        }
        self.end_cpu_cycle();

        if self.abort_dmc_dma {
            self.dmc_dma_running = false;
            self.abort_dmc_dma = false;
            if !self.sprite_dma {
                self.need_dummy_read = false;
                self.begin_cpu_cycle();
                self.apply_dma_events();
                self.in_dmc_dma = false;
                return;
            }
        }

        let mut sprite_counter: u16 = 0;
        let mut sprite_addr: u8 = 0;
        let mut value: u8 = 0;
        while self.dmc_dma_running || self.sprite_dma {
            // parity of the cycle about to start
            let get_cycle = unsafe { (*self.apu.get()).even_cycle } != DMC_GET_PARITY;
            if get_cycle {
                if self.dmc_dma_running && !self.need_halt && !self.need_dummy_read {
                    self.dma_process_cycle();
                    let addr = unsafe { (*self.apu.get()).dmc_dma_address() };
                    value = self.access(|bus| bus.dma_read(addr, &mut prev_read_addr, enable_internal));
                    self.end_cpu_cycle();
                    self.dmc_dma_running = false;
                    self.abort_dmc_dma = false;
                    unsafe { (*self.apu.get()).dmc_dma_complete(value) };
                    self.apply_dma_events();
                } else if self.sprite_dma {
                    self.dma_process_cycle();
                    let addr = self.dma_base_address | sprite_addr as u16;
                    value = self.access(|bus| bus.dma_read(addr, &mut prev_read_addr, enable_internal));
                    self.end_cpu_cycle();
                    sprite_addr = sprite_addr.wrapping_add(1);
                    sprite_counter += 1;
                } else {
                    self.dma_process_cycle();
                    if skip_dummy_reads { self.access(|_| ()); }
                    else { self.access(|bus| { bus.read(read_addr); }); }
                    self.end_cpu_cycle();
                }
            } else if self.sprite_dma && (sprite_counter & 1) == 1 {
                self.dma_process_cycle();
                self.access(|bus| bus.write(0x2004, value));
                self.end_cpu_cycle();
                sprite_counter += 1;
                if sprite_counter == 0x200 {
                    self.sprite_dma = false;
                }
            } else {
                // align to a get cycle
                self.dma_process_cycle();
                if skip_dummy_reads { self.access(|_| ()); }
                else { self.access(|bus| { bus.read(read_addr); }); }
                self.end_cpu_cycle();
            }
        }
        self.begin_cpu_cycle();
        self.apply_dma_events();
        self.in_dmc_dma = false;
    }

    /// Start of one DMA-unit cycle: the cycle counts towards the DMC's halt/dummy.
    fn dma_process_cycle(&mut self) {
        if self.abort_dmc_dma {
            self.dmc_dma_running = false;
            self.abort_dmc_dma = false;
            self.need_dummy_read = false;
            self.need_halt = false;
        } else if self.need_halt {
            self.need_halt = false;
        } else if self.need_dummy_read {
            self.need_dummy_read = false;
        }
        self.begin_cpu_cycle();
        self.apply_dma_events();
    }

    /// A DMA read. While the CPU is halted on a $4000-$401F read, the 2A03 also enables
    /// its internal registers for whatever the DMA reads, so $4015-$4017 respond to the
    /// low 5 address bits (bus conflict with the external value; $4015 reads clear the
    /// frame IRQ). Otherwise nothing answers $4000-$401F on the external bus.
    fn dma_read(&mut self, addr: u16, prev_read_addr: &mut u16, enable_internal: bool) -> u8 {
        if !enable_internal {
            *prev_read_addr = addr;
            return if (0x4000..=0x401F).contains(&addr) { self.open_bus.get() } else { self.read(addr) };
        }
        let internal = 0x4000 | (addr & 0x1F);
        let same = internal == addr;
        let v = match internal {
            0x4015 => {
                // status bits from the APU (clears the frame IRQ), bit 5 from the
                // external byte the DMA is reading at the same time
                let status = unsafe { (*self.apu.get()).read_4015() };
                let ext = if same { self.open_bus.get() } else { self.read(addr) };
                (status & 0xDF) | (ext & 0x20)
            }
            0x4016 | 0x4017 => {
                // the controllers only see one read while /OE stays low
                let mut v = if *prev_read_addr == internal { self.open_bus.get() } else { self.read(internal) };
                if !same {
                    // the port's floating bits (5-7) carry the external value; the
                    // driven controller bits win over it
                    let ob_mask = 0xE0u8;
                    let ext = self.read(addr);
                    v = (ext & ob_mask) | (v & !ob_mask);
                    self.open_bus.set(v);
                }
                v
            }
            _ => self.read(addr),
        };
        *prev_read_addr = internal;
        v
    }
}

impl Timed for NesBus {
    /// Run the PPU (and with it the mapper's PPU-side hooks) up to `target_master`.
    /// The interrupt lines are snapshotted as the PPU passes the sample point of the
    /// current CPU cycle.
    fn run_until(&mut self, target_master: u64) {
        while self.ppu_master + MASTER_PER_DOT <= target_master {
            let mapper = self.cartridge.mapper_mut();
            self.ppu.get_mut().step_one_cycle(mapper);
            self.ppu_master += MASTER_PER_DOT;
            self.total_ppu_dots += 1;
            if self.trace.is_some() {
                let c = self.cartridge.mapper().debug_irq_counter();
                let last = self.trace.as_ref().unwrap().last_counter;
                if c != last {
                    if let (Some(old), Some(new)) = (last, c) {
                        // the dot that just ran is cycle-1 (wraps to the previous line at 0)
                        self.log(format!("IRQ counter {old} -> {new}"));
                    }
                    self.trace.as_mut().unwrap().last_counter = c;
                }
            }
            if !self.ppu.get_mut().debug_lines.is_empty() {
                let lines = std::mem::take(&mut self.ppu.get_mut().debug_lines);
                if self.trace.is_some() {
                    for l in lines { self.log(l); }
                }
            }
            self.dot_in_cycle += 1;
            if self.ppu_master == self.cycle_start + INT_SAMPLE_AT {
                let (old_nmi, old_irq) = (self.nmi_line_snapshot, self.irq_line_snapshot);
                self.nmi_line_snapshot = self.ppu.get_mut().is_nmi_line_asserted();
                self.irq_line_snapshot = self.cartridge.mapper().is_irq_asserted()
                    || self.apu.get_mut().is_irq_asserted();
                if self.trace.is_some() {
                    if self.irq_line_snapshot != old_irq {
                        self.log(format!("IRQ line {}", if self.irq_line_snapshot { "ON" } else { "off" }));
                    }
                    if self.nmi_line_snapshot && !old_nmi {
                        self.log("NMI".into());
                    }
                }
            }
        }
    }

    fn sync_point(&self) -> u64 {
        self.ppu_master
    }
}

impl NesBus {
    fn log(&mut self, what: String) {
        let (sl, dot) = { let p = self.ppu.get_mut(); (p.scanline, p.cycle) };
        if let Some(t) = self.trace.as_mut() {
            t.lines.push(format!("{sl:>4} {dot:>3}   {what}"));
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        if self.trace.is_some() && matches!(addr, 0x2000..=0x3FFF | 0x4014 | 0x4017 | 0x8000..=0xFFFF) {
            let a = if addr < 0x4000 { 0x2000 | (addr & 7) } else { addr };
            if a == 0x2007 {
                // with the VRAM address it lands on (and the PRG bank-select writes are
                // logged as $8000-$FFFF writes)
                let v = self.ppu.get_mut().v_addr & 0x3FFF;
                self.log(format!("W $2007 = {value:02X} (v=${v:04X})"));
            } else {
                self.log(format!("W ${a:04X} = {value:02X}"));
            }
        }
        match addr {
            0x0000..=0x1FFF => self.ram[(addr % 0x0800) as usize] = value,
            0x2000..=0x3FFF => {
                let register = addr % 8;
                let mapper_ref = self.cartridge.mapper_mut();

                mapper_ref.notify_ppu_write(register, value);
                let ppu_mut = self.ppu.get_mut();
                ppu_mut.cpu_write_reg(mapper_ref, register, value);
            }
            0x4014 => {
                // OAM DMA starts at the CPU's next read
                self.dma_base_address = (value as u16) << 8;
                self.sprite_dma = true;
                self.need_halt = true;
            }
            0x4016 => {
                self.out_latch = value;
            }
            0x4000..=0x401F => { 
                self.apu.get_mut().write_reg(addr, value);
             }
            0x4020..=0xFFFF => {
                if addr == 0x5104 {
                    let ppu = self.ppu.get_mut();
                }
                if addr == 0x5104 || (addr >= 0x5126 && addr <= 0x512A) {
                    let ppu = self.ppu.get_mut();
                } 
                self.cartridge.mapper_mut().cpu_write(addr, value); }
        }
        self.open_bus.set(value);
    }
}