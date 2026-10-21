// MARIA: the Atari 7800's graphics chip.
//
// Unlike the 2600's TIA, MARIA draws by itself from lists in memory (DMA), one line at a time,
// into a 160-cell line buffer that is shown on the next line.
//
//   DLL (display list list) at DPPH:DPPL: 3-byte entries, one per "zone" of 1-16 lines:
//       [DLI.7 H16.6 H8.5 lines-1.0-3] [DL address high] [DL address low]
//   DL (display list): object headers, ended by a header whose 2nd byte has bits 0-4 and 6 = 0
//       4 bytes: [addr lo] [palette.7-5 | width.4-0] [addr hi] [x]
//       5 bytes: [addr lo] [write mode.7 | 1.6 | indirect.5 | 0] [addr hi] [palette | width] [x]
//     width = 32 - value (bytes). The graphics address for a zone line is (addr hi + line
//     offset) : addr lo, where the offset counts down from lines-1 to 0 through the zone.
//     Indirect ("character") mode: the bytes are character numbers; their graphics are read
//     from (CHARBASE + offset) : char (1 or 2 bytes each, CTRL bit 4).
//     "Holey DMA": with H16 (H8) set, graphics addresses with A15 and A12 (A11) both set read
//     as nothing, so 4K/2K-tall sprite sheets can be overlapped.
//   DLI: when the DLL entry of the *next* zone has bit 7 set, MARIA raises an NMI as it moves
//     on to it, i.e. on the last line of the zone before (ProSystem's timing).
//
// Graphics modes (CTRL bits 0-1 = read mode, header bit 7 = write mode):
//   160A  2 bits/pixel, 4 pixels per byte, 3 colors from the object's palette
//   160B  2 pixels per byte, color D7D6 / D5D4 and palette bits from D3D2 / D1D0
//   320A  1 bit/pixel, 8 pixels per byte, color 2 of the palette
//   320B / 320C / 320D: hi-res variants (see store / output below)
// Every line-buffer cell = palette (bits 4-2) and color (bits 1-0); color 0 = transparent
// (unless "kangaroo mode", CTRL bit 2, where it paints the background).
//
// Registers (0x20-0x3F):
//   20 BACKGRND  21-23 P0C1-3  24 WSYNC  25-27 P1C1-3  28 MSTAT (read: bit 7 = VBLANK)
//   29-2B P2C  2C DPPH  2D-2F P3C  30 DPPL  31-33 P4C  34 CHARBASE  35-37 P5C
//   39-3B P6C  3C CTRL  3D-3F P7C
//   CTRL: 7 color kill, 6-5 DMA (2 = on), 4 char width (1 = 2 bytes), 3 border, 2 kangaroo,
//         1-0 read mode
//
// Reference: Atari 7800 Software Guide (MARIA chapter), ProSystem emulator (Greg Stanton)

use serde::{Deserialize, Serialize};

pub const LINE_CELLS: usize = 160;
pub const OUT_W: usize = 320;

/// MARIA clock costs of DMA (7.16 MHz clocks; the CPU is halted meanwhile). ProSystem's numbers.
const DMA_START: u32 = 31;
const DMA_FIRST_LINE: u32 = 7;
const DMA_HEADER4: u32 = 8;
const DMA_HEADER5: u32 = 10;
const DMA_BYTE: u32 = 3;
const DMA_CHAR: u32 = 3;
const DMA_DLI: u32 = 20;
const DMA_MAX: u32 = 400;

#[derive(Serialize, Deserialize, Clone)]
pub struct Maria {
    pub regs: [u8; 0x20],
    pub vblank: bool,
    /// next DLL entry
    dll: u16,
    /// current zone: its display list, lines left (offset), holey DMA flags
    dl: u16,
    offset: u8,
    h16: bool,
    h8: bool,
    /// write mode latched by the last 5-byte header
    write_mode: bool,
    #[serde(skip, default = "empty_line")]
    line: [u8; LINE_CELLS],
    /// set when a DLI should reach the CPU
    pub nmi: bool,
}

fn empty_line() -> [u8; LINE_CELLS] { [0; LINE_CELLS] }

impl Default for Maria {
    fn default() -> Self { Self::new() }
}

impl Maria {
    pub fn new() -> Self {
        Self {
            regs: [0; 0x20], vblank: true, dll: 0, dl: 0, offset: 0, h16: false, h8: false,
            write_mode: false, line: [0; LINE_CELLS], nmi: false,
        }
    }

    pub fn load_state(&mut self, s: Maria) { *self = s; }

    fn ctrl(&self) -> u8 { self.regs[0x1C] }
    pub fn dma_on(&self) -> bool { self.ctrl() & 0x60 == 0x40 }

    /// CPU read of 0x20-0x3F (only MSTAT means anything).
    pub fn read(&self, reg: u8) -> u8 {
        match reg & 0x1F {
            0x08 => if self.vblank { 0x80 } else { 0x00 },
            r => self.regs[r as usize],
        }
    }

    /// CPU write of 0x20-0x3F. Returns true for WSYNC (the bus halts the CPU to the line end).
    pub fn write(&mut self, reg: u8, v: u8) -> bool {
        let r = (reg & 0x1F) as usize;
        if r == 0x04 { return true; }
        if r != 0x08 { self.regs[r] = v; }
        false
    }

    // ---------------- DMA ----------------

    fn load_zone(&mut self, mem: &dyn Fn(u16) -> u8) -> bool {
        let b = mem(self.dll);
        self.dl = (mem(self.dll.wrapping_add(1)) as u16) << 8 | mem(self.dll.wrapping_add(2)) as u16;
        self.offset = b & 0x0F;
        self.h16 = b & 0x40 != 0;
        self.h8 = b & 0x20 != 0;
        self.dll = self.dll.wrapping_add(3);
        b & 0x80 != 0
    }

    /// Build the line buffer for one display line. `first` = first line of the frame (reads
    /// the DLL start from DPPH/DPPL). Returns the MARIA clocks the DMA took (CPU halted).
    pub fn dma_line(&mut self, first: bool, mem: &dyn Fn(u16) -> u8) -> u32 {
        let mut clocks = DMA_START;
        if first {
            clocks += DMA_FIRST_LINE;
            self.dll = (self.regs[0x0C] as u16) << 8 | self.regs[0x10] as u16;
            if self.load_zone(mem) {
                self.nmi = true;
                clocks += DMA_DLI;
            }
        }
        self.line = [0; LINE_CELLS];
        clocks += self.run_display_list(mem);
        // move through the zone; on its last line, fetch the next zone (and maybe a DLI)
        if self.offset == 0 {
            if self.load_zone(mem) {
                self.nmi = true;
                clocks += DMA_DLI;
            }
        } else {
            self.offset -= 1;
        }
        clocks.min(DMA_MAX)
    }

    fn run_display_list(&mut self, mem: &dyn Fn(u16) -> u8) -> u32 {
        let mut clocks = 0;
        let mut dp = self.dl;
        let ctrl = self.ctrl();
        let kangaroo = ctrl & 0x04 != 0;
        let char_bytes = if ctrl & 0x10 != 0 { 2 } else { 1 };
        // a broken list can't loop forever: at most 128 headers per line
        for _ in 0..128 {
            let mode = mem(dp.wrapping_add(1));
            if mode & 0x5F == 0 { break; }                       // end of display list
            let (lo, hi, palwidth, x, indirect);
            if mode & 0x1F == 0 {                                // 5-byte header
                self.write_mode = mode & 0x80 != 0;
                indirect = mode & 0x20 != 0;
                lo = mem(dp);
                hi = mem(dp.wrapping_add(2));
                palwidth = mem(dp.wrapping_add(3));
                x = mem(dp.wrapping_add(4));
                dp = dp.wrapping_add(5);
                clocks += DMA_HEADER5;
            } else {
                indirect = false;
                lo = mem(dp);
                hi = mem(dp.wrapping_add(2));
                palwidth = mode;
                x = mem(dp.wrapping_add(3));
                dp = dp.wrapping_add(4);
                clocks += DMA_HEADER4;
            }
            let palette = (palwidth & 0xE0) >> 3;                 // already in cell position
            let width = ((!palwidth) & 0x1F) as u16 + 1;
            let mut h = x;
            if indirect {
                let base = (lo as u16) | (hi as u16) << 8;
                let char_hi = self.regs[0x14].wrapping_add(self.offset);
                for i in 0..width {
                    let c = mem(base.wrapping_add(i));
                    clocks += DMA_CHAR;
                    for k in 0..char_bytes {
                        let addr = ((char_hi as u16) << 8 | c as u16).wrapping_add(k);
                        let data = if self.holey(addr) { None } else { Some(mem(addr)) };
                        clocks += DMA_BYTE;
                        h = self.store(h, data, palette, kangaroo);
                    }
                }
            } else {
                let base = (lo as u16) | (hi.wrapping_add(self.offset) as u16) << 8;
                for i in 0..width {
                    let addr = base.wrapping_add(i);
                    let data = if self.holey(addr) { None } else { Some(mem(addr)) };
                    clocks += DMA_BYTE;
                    h = self.store(h, data, palette, kangaroo);
                }
            }
        }
        clocks
    }

    fn holey(&self, addr: u16) -> bool {
        (self.h16 && addr & 0x9000 == 0x9000) || (self.h8 && addr & 0x8800 == 0x8800)
    }

    /// Put one graphics byte into the line buffer at cell `h`; returns the next cell.
    /// `None` (holey DMA) skips the cells without drawing.
    fn store(&mut self, h: u8, data: Option<u8>, palette: u8, kangaroo: bool) -> u8 {
        if self.write_mode {
            // 2 cells per byte: color from D7D6 / D5D4, palette bits from D3D2 / D1D0
            let cells = data.map(|d| [((d & 0x0C), (d & 0xC0) >> 6), ((d & 0x03) << 2, (d & 0x30) >> 4)]);
            for k in 0..2 {
                let c = cells.map(|c| c[k]);
                self.cell(h.wrapping_add(k as u8), c.map(|(hi, lo)| {
                    if hi | lo == 0 { 0 } else { (palette & 0x10) | hi | lo }
                }), kangaroo);
            }
            h.wrapping_add(2)
        } else {
            for k in 0..4u8 {
                let v = data.map(|d| (d >> (6 - 2 * k)) & 3);
                self.cell(h.wrapping_add(k), v.map(|v| if v == 0 { 0 } else { palette | v }), kangaroo);
            }
            h.wrapping_add(4)
        }
    }

    fn cell(&mut self, h: u8, value: Option<u8>, kangaroo: bool) {
        let h = h as usize;
        if h >= LINE_CELLS { return; }
        match value {
            Some(0) if kangaroo => self.line[h] = 0,
            Some(0) | None => {}
            Some(v) => self.line[h] = v,
        }
    }

    // ---------------- output ----------------

    /// Color register for a cell value (palette * 4 + color); color 0 = background.
    fn color(&self, v: u8) -> u8 {
        if v & 3 == 0 { self.regs[0] } else { self.regs[v as usize & 0x1F] }
    }

    /// Turn the line buffer into 320 color register values (read mode = CTRL bits 0-1).
    pub fn output_line(&self, out: &mut [u8; OUT_W]) {
        let rm = self.ctrl() & 3;
        for (i, &c) in self.line.iter().enumerate() {
            let (a, b) = match rm {
                2 => (                                                     // 320B / 320D
                    (c & 0x10) | ((c & 0x08) >> 3) | (c & 0x02),
                    (c & 0x10) | ((c & 0x04) >> 2) | ((c & 0x01) << 1),
                ),
                3 => (c & 0x1E, (c & 0x1C) | ((c & 0x01) << 1)),       // 320A / 320C
                _ => (c, c),                                               // 160A / 160B
            };
            out[2 * i] = self.color(a);
            out[2 * i + 1] = self.color(b);
        }
    }

    /// A line with nothing but the background (DMA off or outside the picture).
    pub fn background_line(&self, out: &mut [u8; OUT_W]) {
        out.fill(self.regs[0]);
    }

    #[cfg(test)]
    pub(crate) fn line_cells(&self) -> &[u8; LINE_CELLS] { &self.line }
}

/// 7800 color (hue in the high nibble, 16 brightness levels in the low nibble) to RGB.
/// Same hues as the 2600 palette (which has 8 levels); odd levels are blended between.
pub fn rgb(c: u8) -> [u8; 3] {
    use crate::atari2600::tia::rgb as tia_rgb;
    let lo = tia_rgb(c & 0xFE);
    if c & 1 == 0 || c & 0x0F == 0x0F {
        return lo;
    }
    let hi = tia_rgb((c & 0xF0) | ((c & 0x0E) + 2));
    [((lo[0] as u16 + hi[0] as u16) / 2) as u8,
     ((lo[1] as u16 + hi[1] as u16) / 2) as u8,
     ((lo[2] as u16 + hi[2] as u16) / 2) as u8]
}
