//! Famicom Disk System: the RAM adapter (32 KB PRG RAM at $6000-$DFFF, the 8 KB BIOS at
//! $E000, 8 KB CHR RAM, the disk drive interface, a timer IRQ) and its wavetable sound.
//!
//! Behaviour from the NESdev wiki ("Family Computer Disk System", "FDS audio", "FDS disk
//! format"); the BIOS (disksys.rom) is the player's.
//!
//! The drive: one spiral track per side, read or written a byte at a time, one byte every
//! 1792 master clocks (~149 CPU cycles). .fds images hold only the blocks, so each side is
//! laid out again as it is on the disk: a lead-in gap, then each block behind a start
//! mark ($80) and followed by two CRC bytes and a gap. The BIOS reads the CRC result from
//! $4030 bit 4, which is always "good" here (the image has no CRCs to check).
//!
//! Disk writes (saves) change that raw layout; the save file is the raw layout of every side.

use crate::nes::mappers::{decode_state, encode_state, mirror_nt, Mapper, Mirroring};
use serde::{Deserialize, Serialize};

pub const SIDE_BYTES: usize = 65500;
/// zeros before the first block (28300 bits) and between blocks (976 bits)
const LEAD_IN: usize = 28300 / 8;
const GAP: usize = 976 / 8;
/// CPU cycles per disk byte (1792 master clocks / 12)
const BYTE_CYCLES: u32 = 149;
/// motor start to the first byte (the head travelling back to the start)
const SPIN_UP: u32 = 50000;
/// a side swap: the disk is out for this many CPU cycles (~1 s) so the BIOS notices
const SWAP_CYCLES: u32 = 1_800_000;

/// The sides of a .fds image (with or without its 16-byte "FDS\x1A" header).
pub fn sides(image: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    let body = if image.starts_with(b"FDS\x1A") { &image[16..] } else { image };
    if body.len() < SIDE_BYTES / 2 || !body[1..].starts_with(b"*NINTENDO-HVC*") {
        return Err("not a Famicom Disk System image".into());
    }
    Ok(body.chunks(SIDE_BYTES).filter(|s| s.len() > 56 && s[0] == 1).map(|s| s.to_vec()).collect())
}

/// A disk image: FDS header or a side's disk info block.
pub fn is_fds(image: &[u8]) -> bool {
    image.starts_with(b"FDS\x1A") || (image.len() > 15 && &image[1..15] == b"*NINTENDO-HVC*")
}

/// FDS block CRC (CRC-16, polynomial $8408, over the block from its type byte).
fn crc(data: &[u8]) -> u16 {
    let mut c: u16 = 0;
    for &b in data.iter().chain([0u8, 0].iter()) {
        for i in 0..8 {
            let bit = (b >> i) & 1;
            let carry = c & 1;
            c = (c >> 1) | (bit as u16) << 15;
            if carry != 0 { c ^= 0x8408; }
        }
    }
    c
}

/// One side laid out as on the disk (gaps, start marks, CRCs).
pub fn raw_side(side: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; LEAD_IN];
    let mut add = |out: &mut Vec<u8>, block: &[u8]| {
        out.push(0x80);
        out.extend_from_slice(block);
        let c = crc(block);
        out.extend_from_slice(&[c as u8, (c >> 8) as u8]);
        out.extend(std::iter::repeat(0).take(GAP));
    };
    let mut p = 0;
    let get = |p: usize, n: usize| side.get(p..p + n);
    // disk info, file count
    if let Some(b) = get(p, 56) { add(&mut out, b); p += 56; }
    if let Some(b) = get(p, 2) { add(&mut out, b); p += 2; }
    // files: header block (3) then data block (4); keep going past the count while blocks
    // follow (some disks hide files there)
    while let Some(h) = get(p, 16) {
        if h[0] != 3 { break; }
        let size = u16::from_le_bytes([h[13], h[14]]) as usize;
        add(&mut out, h);
        p += 16;
        match get(p, size + 1) {
            Some(d) if d[0] == 4 => { add(&mut out, d); p += size + 1; }
            _ => break,
        }
    }
    // the rest of the track: room for files written later
    let total = (LEAD_IN + SIDE_BYTES + 2000).max(out.len() + 2000);
    out.resize(total, 0);
    out
}

// ---------------------------------------------------------------- sound

#[derive(Clone, Serialize, Deserialize)]
struct Envelope { speed: u8, gain: u8, increase: bool, off: bool, timer: u32 }

impl Envelope {
    fn new() -> Self { Self { speed: 0, gain: 0, increase: false, off: true, timer: 0 } }
    fn write(&mut self, v: u8) {
        self.speed = v & 0x3F;
        self.increase = v & 0x40 != 0;
        self.off = v & 0x80 != 0;
        if self.off { self.gain = v & 0x3F; }
        self.timer = 0;
    }
    /// One CPU cycle; `period` = CPU cycles per step (0 = envelopes stopped).
    fn clock(&mut self, master: u8) {
        if self.off { return; }
        self.timer += 1;
        if self.timer >= 8 * (self.speed as u32 + 1) * (master as u32 + 1) {
            self.timer = 0;
            if self.increase { if self.gain < 32 { self.gain += 1; } } else if self.gain > 0 { self.gain -= 1; }
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Audio {
    wave: Vec<u8>,
    wave_write: bool,
    master_volume: u8,
    pitch: u16,
    wave_halt: bool,
    env_halt: bool,
    acc: u32,
    pos: u8,
    vol: Envelope,
    /// volume gain as of the last time the wave passed position 0
    vol_out: u8,
    modu: Envelope,
    mod_table: Vec<u8>,
    mod_pos: u8,
    mod_counter: i8,
    mod_pitch: u16,
    mod_halt: bool,
    mod_acc: u32,
    env_speed: u8,
    enabled: bool,
    out: u8,
    filtered: f32,
}

impl Audio {
    fn new() -> Self {
        Self { wave: vec![0; 64], wave_write: false, master_volume: 0, pitch: 0, wave_halt: true, env_halt: false,
               acc: 0, pos: 0, vol: Envelope::new(), vol_out: 0, modu: Envelope::new(), mod_table: vec![0; 64],
               mod_pos: 0, mod_counter: 0, mod_pitch: 0, mod_halt: true, mod_acc: 0, env_speed: 0xE8,
               enabled: true, out: 0, filtered: 0.0 }
    }

    fn write(&mut self, addr: u16, v: u8) {
        match addr {
            0x4040..=0x407F => if self.wave_write { self.wave[addr as usize - 0x4040] = v & 0x3F; },
            0x4080 => self.vol.write(v),
            0x4082 => self.pitch = self.pitch & 0xF00 | v as u16,
            0x4083 => {
                self.pitch = self.pitch & 0x0FF | (v as u16 & 0x0F) << 8;
                self.wave_halt = v & 0x80 != 0;
                self.env_halt = v & 0x40 != 0;
                if self.wave_halt { self.acc = 0; self.pos = 0; }
                if self.env_halt { self.vol.timer = 0; self.modu.timer = 0; }
            }
            0x4084 => self.modu.write(v),
            0x4085 => self.mod_counter = ((v << 1) as i8) >> 1,   // 7-bit signed
            0x4086 => self.mod_pitch = self.mod_pitch & 0xF00 | v as u16,
            0x4087 => {
                self.mod_pitch = self.mod_pitch & 0x0FF | (v as u16 & 0x0F) << 8;
                self.mod_halt = v & 0x80 != 0;
                if self.mod_halt { self.mod_acc = 0; }
            }
            0x4088 => if self.mod_halt {
                // 32 entries, each used twice: write both halves, move on
                let p = (self.mod_pos & 0x3E) as usize;
                self.mod_table[p] = v & 7;
                self.mod_table[p + 1] = v & 7;
                self.mod_pos = (self.mod_pos + 2) & 0x3F;
            },
            0x4089 => { self.wave_write = v & 0x80 != 0; self.master_volume = v & 3; }
            0x408A => self.env_speed = v,
            _ => {}
        }
    }

    fn read(&self, addr: u16, open_bus: u8) -> Option<u8> {
        Some(match addr {
            0x4040..=0x407F => self.wave[addr as usize - 0x4040] | (open_bus & 0xC0),
            0x4090 => self.vol.gain | (open_bus & 0xC0),
            0x4092 => self.modu.gain | (open_bus & 0xC0),
            _ => return None,
        })
    }

    /// The pitch after modulation (NESdev wiki "FDS audio").
    fn modulated_pitch(&self) -> u32 {
        let gain = self.modu.gain as i32;
        let mut temp = self.mod_counter as i32 * gain;
        let remainder = temp & 0x0F;
        temp >>= 4;
        if remainder > 0 && temp & 0x80 == 0 {
            temp += if self.mod_counter < 0 { -1 } else { 2 };
        }
        if temp >= 192 { temp -= 256; } else if temp < -64 { temp += 256; }
        let mut t = self.pitch as i32 * temp;
        let rem = t & 0x3F;
        t >>= 6;
        if rem >= 32 { t += 1; }
        (self.pitch as i32 + t).max(0) as u32
    }

    /// One CPU cycle.
    fn clock(&mut self) {
        if !self.enabled { return; }
        if !self.env_halt && !self.wave_halt && self.env_speed != 0 {
            self.vol.clock(self.env_speed);
            self.modu.clock(self.env_speed);
        }
        // modulation unit
        if !self.mod_halt && self.mod_pitch != 0 {
            self.mod_acc += self.mod_pitch as u32;
            if self.mod_acc >= 0x10000 {
                self.mod_acc -= 0x10000;
                let step = self.mod_table[self.mod_pos as usize];
                self.mod_counter = match step {
                    4 => 0,
                    s => {
                        let d = [0i32, 1, 2, 4, 0, -4, -2, -1][s as usize];
                        ((((self.mod_counter as i32 + d) + 64) & 0x7F) - 64) as i8
                    }
                };
                self.mod_pos = (self.mod_pos + 1) & 0x3F;
            }
        }
        // wave
        if !self.wave_halt && !self.wave_write {
            let p = if self.mod_halt { self.pitch as u32 } else { self.modulated_pitch() };
            self.acc += p;
            if self.acc >= 0x10000 {
                self.acc -= 0x10000;
                self.pos = (self.pos + 1) & 0x3F;
                if self.pos == 0 { self.vol_out = self.vol.gain; }
            }
            self.out = self.wave[self.pos as usize];
        }
    }

    /// Output level, as a share of the 2A03 mix (full FDS ≈ 2.4 × one full square wave).
    fn level(&mut self) -> f32 {
        const MASTER: [f32; 4] = [1.0, 2.0 / 3.0, 2.0 / 4.0, 2.0 / 5.0];
        const FULL: f32 = 2.4 * 0.1494;   // one square at volume 15 in the APU's mix
        let gain = self.vol_out.min(32) as f32;
        let x = self.out as f32 * gain / (63.0 * 32.0) * MASTER[self.master_volume as usize] * FULL;
        // the adapter's ~2 kHz low-pass (one pole, at the CPU's rate)
        self.filtered += (x - self.filtered) * 0.0070;
        self.filtered
    }
}

// ---------------------------------------------------------------- the RAM adapter

#[derive(Clone, Serialize, Deserialize)]
struct Drive {
    /// the side in the drive (None = ejected)
    side: Option<usize>,
    /// side to insert once the swap delay is over
    next_side: Option<usize>,
    swap_delay: u32,
    position: usize,
    delay: u32,
    motor_on: bool,
    reset_transfer: bool,
    read_mode: bool,
    crc_control: bool,
    crc_enabled: bool,
    irq_on_transfer: bool,
    horizontal: bool,
    end_of_head: bool,
    scanning: bool,
    gap_ended: bool,
    previous_crc_control: bool,
    transfer_flag: bool,
    read_data: u8,
    write_data: u8,
    disk_irq: bool,
    dirty: bool,
    // timer IRQ
    timer_reload: u16,
    timer: u16,
    timer_repeat: bool,
    timer_enabled: bool,
    timer_irq: bool,
    disk_regs_on: bool,
    ext_out: u8,
}

pub struct Fds {
    bios: Vec<u8>,
    ram: Vec<u8>,
    chr: Vec<u8>,
    /// every side, raw (as on the disk), one after another: also the save file
    image: Vec<u8>,
    /// (start, length) of each side in `image`
    sides: Vec<(usize, usize)>,
    /// register reads change it (acknowledging IRQs) through the bus's shared reference
    d: std::cell::RefCell<Drive>,
    audio: Audio,
}

#[derive(Serialize, Deserialize)]
struct State { ram: Vec<u8>, chr: Vec<u8>, image: Vec<u8>, d: Drive, audio: Audio }

impl Fds {
    pub fn new(image: &[u8], bios: &[u8]) -> Result<Self, String> {
        if bios.len() < 0x2000 {
            return Err("Famicom Disk System: no BIOS. Put disksys.rom (8 KB) in user://BIOS/".into());
        }
        let raw: Vec<Vec<u8>> = sides(image)?.iter().map(|s| raw_side(s)).collect();
        println!("FDS: {} disk side(s)", raw.len());
        let mut at = 0;
        let spans = raw.iter().map(|s| { at += s.len(); (at - s.len(), s.len()) }).collect();
        Ok(Self {
            bios: bios[bios.len() - 0x2000..].to_vec(),
            ram: vec![0; 0x8000],
            chr: vec![0; 0x2000],
            image: raw.concat(),
            sides: spans,
            d: std::cell::RefCell::new(Drive {
                side: Some(0), next_side: None, swap_delay: 0, position: 0, delay: 0,
                motor_on: false, reset_transfer: false, read_mode: true, crc_control: false, crc_enabled: false,
                irq_on_transfer: false, horizontal: false, end_of_head: true, scanning: false, gap_ended: false,
                previous_crc_control: false, transfer_flag: false, read_data: 0, write_data: 0, disk_irq: false,
                dirty: false, timer_reload: 0, timer: 0, timer_repeat: false, timer_enabled: false, timer_irq: false,
                disk_regs_on: true, ext_out: 0xFF,
            }),
            audio: Audio::new(),
        })
    }

    pub fn side_count(&self) -> usize { self.sides.len() }
    pub fn current_side(&self) -> Option<usize> { self.d.borrow().side }

    /// Eject the disk and, after about a second, put in side `side` (as a player would).
    /// None = eject only.
    pub fn insert(&mut self, side: Option<usize>) {
        let n = self.sides.len();
        let d = self.d.get_mut();
        d.side = None;
        d.next_side = side.filter(|&s| s < n);
        d.swap_delay = SWAP_CYCLES;
        println!("FDS: disk out{}", d.next_side.map(|s| format!(", side {} next", side_name(s))).unwrap_or_default());
    }

    /// The next side (A → B → next disk's A ...), wrapping around.
    pub fn next_side(&mut self) {
        let cur = { let d = self.d.get_mut(); d.side.or(d.next_side).unwrap_or(0) };
        self.insert(Some((cur + 1) % self.sides.len().max(1)));
    }

    fn drive_clock(&mut self) {
        let d = self.d.get_mut();
        if d.swap_delay > 0 {
            d.swap_delay -= 1;
            if d.swap_delay == 0 {
                d.side = d.next_side.take();
                if let Some(s) = d.side { println!("FDS: side {} in", side_name(s)); }
            }
        }
        let Some(side) = d.side else { d.end_of_head = true; d.scanning = false; return; };
        if !d.motor_on { d.end_of_head = true; d.scanning = false; return; }
        if d.reset_transfer && !d.scanning { return; }
        if d.end_of_head {
            d.delay = SPIN_UP;
            d.end_of_head = false;
            d.position = 0;
            d.gap_ended = false;
            return;
        }
        if d.delay > 0 { d.delay -= 1; return; }

        d.scanning = true;
        let (start, len) = self.sides[side];
        let disk = &mut self.image[start..start + len];
        let mut irq = d.irq_on_transfer;
        if d.read_mode {
            let byte = disk.get(d.position).copied().unwrap_or(0);
            if !d.crc_enabled {
                d.gap_ended = false;            // between blocks: looking for a start mark
            } else if byte != 0 && !d.gap_ended {
                d.gap_ended = true;             // the start mark: bytes start with the next one
            } else if d.gap_ended {
                d.transfer_flag = true;
                d.read_data = byte;
                if irq { d.disk_irq = true; }
            }
        } else {
            let mut byte = 0u8;
            if !d.crc_control {
                d.transfer_flag = true;
                byte = d.write_data;
                if irq { d.disk_irq = true; }
            }
            if !d.crc_enabled { byte = 0; }
            if d.position < disk.len() {
                // (the CRC bytes are left as zeros: reads don't check them here)
                if !d.crc_control || !d.previous_crc_control { disk[d.position] = byte; }
                d.dirty = true;
            }
            d.gap_ended = false;
        }
        d.previous_crc_control = d.crc_control;
        d.position += 1;
        if d.position >= disk.len() {
            d.motor_on = false;
            if irq { d.disk_irq = true; }
        } else {
            d.delay = BYTE_CYCLES - 1;
        }
    }

    fn timer_clock(&mut self) {
        let d = self.d.get_mut();
        if !d.timer_enabled || !d.disk_regs_on { return; }
        if d.timer == 0 {
            d.timer_irq = true;
            d.timer = d.timer_reload;
            if !d.timer_repeat { d.timer_enabled = false; }
        } else {
            d.timer -= 1;
        }
    }
}

pub fn side_name(s: usize) -> String { format!("{}{}", s / 2 + 1, if s % 2 == 0 { "A" } else { "B" }) }

impl Mapper for Fds {
    fn cpu_read(&self, addr: u16) -> u8 {
        match addr {
            0x6000..=0xDFFF => self.ram[addr as usize - 0x6000],
            0xE000..=0xFFFF => self.bios[addr as usize - 0xE000],
            _ => 0,
        }
    }

    fn cpu_read_bus(&self, addr: u16, open_bus: u8) -> u8 {
        let mut d = self.d.borrow_mut();
        match addr {
            0x4030 => {
                let v = d.timer_irq as u8
                    | (d.horizontal as u8) << 3
                    | (d.end_of_head as u8) << 6
                    | (d.transfer_flag as u8) << 7;
                d.timer_irq = false;                   // acknowledges everything but the byte transfer
                v | (open_bus & 0x24)
            }
            0x4031 => { d.transfer_flag = false; d.disk_irq = false; d.read_data }
            0x4032 => {
                let out = d.side.is_none();
                (open_bus & 0xF8) | out as u8 | ((out || !d.scanning) as u8) << 1 | (out as u8) << 2
            }
            0x4033 => 0x80 | (d.ext_out & 0x7F),       // battery good
            0x4040..=0x409F => self.audio.read(addr, open_bus).unwrap_or(open_bus),
            0x4020..=0x5FFF => open_bus,
            _ => self.cpu_read(addr),
        }
    }

    fn cpu_write(&mut self, addr: u16, v: u8) {
        let d = self.d.get_mut();
        match addr {
            0x4020 => d.timer_reload = d.timer_reload & 0xFF00 | v as u16,
            0x4021 => d.timer_reload = d.timer_reload & 0x00FF | (v as u16) << 8,
            0x4022 => if d.disk_regs_on {
                d.timer_repeat = v & 1 != 0;
                d.timer_enabled = v & 2 != 0;
                d.timer_irq = false;
                if d.timer_enabled { d.timer = d.timer_reload; }
            },
            0x4023 => {
                d.disk_regs_on = v & 1 != 0;
                if !d.disk_regs_on { d.timer_enabled = false; d.timer_irq = false; d.disk_irq = false; }
                self.audio.enabled = v & 2 != 0;
            }
            0x4024 => if d.disk_regs_on { d.write_data = v; d.transfer_flag = false; d.disk_irq = false; },
            0x4025 => if d.disk_regs_on {
                d.motor_on = v & 0x01 != 0;
                d.reset_transfer = v & 0x02 != 0;
                d.read_mode = v & 0x04 != 0;
                d.horizontal = v & 0x08 != 0;
                d.crc_control = v & 0x10 != 0;
                d.crc_enabled = v & 0x40 != 0;
                d.irq_on_transfer = v & 0x80 != 0;
                d.disk_irq = false;
            },
            0x4026 => if d.disk_regs_on { d.ext_out = v; },
            0x4040..=0x408A => self.audio.write(addr, v),
            0x6000..=0xDFFF => self.ram[addr as usize - 0x6000] = v,
            _ => {}
        }
    }

    fn step_cycles(&mut self, cycles: u64) {
        for _ in 0..cycles {
            self.timer_clock();
            self.drive_clock();
            self.audio.clock();
        }
    }

    fn audio_out(&mut self) -> f32 { self.audio.level() }
    fn as_fds(&mut self) -> Option<&mut Fds> { Some(self) }

    fn is_irq_asserted(&self) -> bool { let d = self.d.borrow(); d.timer_irq || d.disk_irq }

    fn ppu_read(&self, addr: u16) -> u8 { self.chr[addr as usize & 0x1FFF] }
    fn ppu_write(&mut self, addr: u16, v: u8) { self.chr[addr as usize & 0x1FFF] = v; }
    fn mirror_vram_address(&self, addr: u16) -> usize {
        mirror_nt(if self.d.borrow().horizontal { Mirroring::Horizontal } else { Mirroring::Vertical }, addr)
    }

    // the disks are the battery save: every side, raw
    fn get_sram(&self) -> Option<&[u8]> { Some(&self.image) }
    fn load_sram(&mut self, data: &[u8]) {
        if data.len() != self.image.len() { println!("FDS: disk save doesn't match this image, ignored"); return; }
        self.image.copy_from_slice(data);
        println!("FDS: disk save loaded");
    }
    fn is_sram_dirty(&self) -> bool { self.d.borrow().dirty }
    fn clear_sram_dirty(&mut self) { self.d.get_mut().dirty = false; }

    fn save_state(&self) -> Vec<u8> {
        encode_state(&State { ram: self.ram.clone(), chr: self.chr.clone(), image: self.image.clone(),
                              d: self.d.borrow().clone(), audio: self.audio.clone() })
    }
    fn load_state(&mut self, data: &[u8]) {
        if let Some(s) = decode_state::<State>(data) {
            if s.image.len() == self.image.len() { self.image = s.image; }
            self.ram = s.ram; self.chr = s.chr; self.d = std::cell::RefCell::new(s.d); self.audio = s.audio;
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A one-side disk with one 4-byte file "TEST" = DE AD BE EF (with an fwNES header).
    pub fn image() -> Vec<u8> {
        let mut side = vec![0u8; SIDE_BYTES];
        side[0] = 1;
        side[1..15].copy_from_slice(b"*NINTENDO-HVC*");
        side[56] = 2;
        side[57] = 1;                                          // one file
        let h = 58;
        side[h] = 3;
        side[h + 3..h + 11].copy_from_slice(b"TEST    ");
        side[h + 11..h + 13].copy_from_slice(&0x6000u16.to_le_bytes());
        side[h + 13..h + 15].copy_from_slice(&4u16.to_le_bytes());
        side[h + 16..h + 21].copy_from_slice(&[4, 0xDE, 0xAD, 0xBE, 0xEF]);
        let mut img = b"FDS\x1A".to_vec();
        img.push(2);                                           // says 2 sides...
        img.resize(16, 0);
        img.extend(&side);
        let mut b = side.clone();                              // ...side B = a copy with another name
        b[h + 3] = b'B';
        img.extend(&b);
        img
    }

    pub fn bios() -> Vec<u8> { vec![0xEA; 0x2000] }

    fn fds() -> Fds { Fds::new(&image(), &bios()).unwrap() }

    #[test]
    fn image_and_raw_layout() {
        assert!(is_fds(&image()) && is_fds(&image()[16..]) && !is_fds(b"NES\x1A0000000000000"));
        let s = sides(&image()).unwrap();
        assert_eq!(s.len(), 2);
        let raw = raw_side(&s[0]);
        assert!(raw[..LEAD_IN].iter().all(|&b| b == 0), "lead-in");
        assert_eq!(&raw[LEAD_IN..LEAD_IN + 16], b"\x80\x01*NINTENDO-HVC*");
        // block 2 behind block 1's CRC and a gap
        let b2 = LEAD_IN + 1 + 56 + 2 + GAP;
        assert_eq!(&raw[b2..b2 + 3], &[0x80, 2, 1]);
        // the file's data block
        let at = raw.windows(5).position(|w| w == [4, 0xDE, 0xAD, 0xBE, 0xEF]).unwrap();
        assert_eq!(raw[at - 1], 0x80);
        assert!(Fds::new(&image(), &[0; 100]).err().unwrap().contains("disksys.rom"));
    }

    /// Read bytes the way the BIOS does: motor on, wait for the start mark with CRC on,
    /// one byte per transfer flag.
    fn read_block(f: &mut Fds, n: usize) -> Vec<u8> {
        let mut out = vec![];
        let mut cycles = 0;
        while out.len() < n && cycles < 2_000_000 {
            f.step_cycles(1);
            cycles += 1;
            if f.cpu_read_bus(0x4030, 0) & 0x80 != 0 { out.push(f.cpu_read_bus(0x4031, 0)); }
        }
        out
    }

    #[test]
    fn drive_reads_blocks() {
        let mut f = fds();
        f.cpu_write(0x4023, 0x83);
        f.cpu_write(0x4025, 0x2E);                             // reset, motor off
        assert_eq!(f.cpu_read_bus(0x4032, 0) & 3, 2, "disk in, not ready");
        f.cpu_write(0x4025, 0x2F);                             // motor on
        f.cpu_write(0x4025, 0x2D);
        for _ in 0..SPIN_UP + 10 { f.step_cycles(1); }
        assert_eq!(f.cpu_read_bus(0x4032, 0) & 3, 0, "ready");
        f.cpu_write(0x4025, 0x6D);                             // CRC on: find the start mark
        let b = read_block(&mut f, 15);
        assert_eq!(&b[..15], b"\x01*NINTENDO-HVC*", "block 1, without the start mark");
        // the timing: one byte per 149 CPU cycles
        let t0 = (0..).take_while(|_| { f.step_cycles(1); f.cpu_read_bus(0x4030, 0) & 0x80 == 0 }).count();
        f.cpu_read_bus(0x4031, 0);
        let t1 = (0..).take_while(|_| { f.step_cycles(1); f.cpu_read_bus(0x4030, 0) & 0x80 == 0 }).count();
        assert!(t0 <= 149 && t1 == 148, "{t0} {t1}");
    }

    #[test]
    fn transfer_irq_and_timer_irq() {
        let mut f = fds();
        f.cpu_write(0x4023, 0x83);
        f.cpu_write(0x4025, 0xED);                             // motor, read, CRC, IRQ on
        let mut n = 0;
        while !f.is_irq_asserted() && n < 2_000_000 { f.step_cycles(1); n += 1; }
        assert!(f.is_irq_asserted());
        assert_eq!(f.cpu_read_bus(0x4031, 0), 0x01, "first byte: the block type");
        assert!(!f.is_irq_asserted(), "reading $4031 acknowledges");
        // timer: reload 100, one shot
        f.cpu_write(0x4025, 0x2E);
        f.cpu_write(0x4020, 100);
        f.cpu_write(0x4021, 0);
        f.cpu_write(0x4022, 0x02);
        for _ in 0..100 { f.step_cycles(1); }
        assert!(!f.is_irq_asserted());
        f.step_cycles(1);
        assert!(f.is_irq_asserted(), "after 101 cycles");
        assert_eq!(f.cpu_read_bus(0x4030, 0) & 1, 1);
        assert!(!f.is_irq_asserted(), "$4030 acknowledges the timer");
        for _ in 0..500 { f.step_cycles(1); }
        assert!(!f.is_irq_asserted(), "one shot");
    }

    #[test]
    fn side_swap_and_writes() {
        let mut f = fds();
        f.cpu_write(0x4023, 0x83);
        f.next_side();
        assert_eq!(f.cpu_read_bus(0x4032, 0) & 5, 5, "out (and write-protected)");
        for _ in 0..100_000 { f.step_cycles(1); }
        assert_eq!(f.cpu_read_bus(0x4032, 0) & 1, 1, "still out a moment later (the BIOS must see it)");
        for _ in 0..SWAP_CYCLES { f.step_cycles(1); }
        assert_eq!((f.cpu_read_bus(0x4032, 0) & 1, f.current_side()), (0, Some(1)), "side B in");
        // write a byte somewhere past the first block
        f.cpu_write(0x4025, 0x2F);
        f.cpu_write(0x4025, 0x29);                             // motor, write mode
        for _ in 0..SPIN_UP + 149 * 4000 { f.step_cycles(1); }
        f.cpu_write(0x4025, 0x69);                             // CRC on: bytes go to the disk
        f.cpu_write(0x4024, 0x5A);
        for _ in 0..149 * 3 { f.step_cycles(1); }
        assert!(f.is_sram_dirty());
        let (start, len) = f.sides[1];
        assert!(f.image[start..start + len].contains(&0x5A));
        let save = f.get_sram().unwrap().to_vec();
        let mut g = fds();
        g.load_sram(&save);
        assert_eq!(g.image, f.image, "the save restores the disks");
    }

    #[test]
    fn wavetable_sound() {
        let mut f = fds();
        f.cpu_write(0x4023, 0x83);
        f.cpu_write(0x4089, 0x80);                             // wave write on
        for i in 0..64 { f.cpu_write(0x4040 + i, if i < 32 { 63 } else { 0 }); }
        f.cpu_write(0x4089, 0x00);                             // full master volume
        f.cpu_write(0x4080, 0x80 | 32);                        // gain 32, envelope off
        f.cpu_write(0x4087, 0x80);                             // no modulation
        // pitch 0x200: 1789773 * 512 / 2^22 = ~218 Hz
        f.cpu_write(0x4082, 0x00);
        f.cpu_write(0x4083, 0x02);
        let out: Vec<f32> = (0..1_789_773 / 10).map(|_| { f.step_cycles(1); f.audio_out() }).collect();
        let peak = out.iter().cloned().fold(0f32, f32::max);
        assert!(peak > 0.2 && peak < 0.4, "loud as ~2.4 squares: {peak}");
        // count rising crossings of half the peak over 0.1 s
        let ups = out.windows(2).filter(|w| w[0] < peak / 2.0 && w[1] >= peak / 2.0).count();
        assert!((20..=23).contains(&ups), "~21.8 cycles in 0.1 s: {ups}");
        f.cpu_write(0x4089, 0x03);                             // 2/5 volume
        let out2: Vec<f32> = (0..200_000).map(|_| { f.step_cycles(1); f.audio_out() }).collect();
        let peak2 = out2[100_000..].iter().cloned().fold(0f32, f32::max);
        assert!((peak2 / peak - 0.4).abs() < 0.05, "{}", peak2 / peak);
    }
}
