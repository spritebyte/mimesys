//! SH-4 MMU: the 64-entry unified TLB, used when MMUCR.AT is set (Windows CE games turn it
//! on; everything else runs untranslated).
//!
//! - Translated: U0/P0 (00000000-7FFFFFFF) and P3 (C0000000-DFFFFFFF). P1, P2, P4 are not.
//! - An entry matches when valid, its VPN matches (page size 1 KB / 4 KB / 64 KB / 1 MB)
//!   and it is shared, or ASID = PTEH.ASID (ignored in privileged mode with MMUCR.SV).
//! - Protection (PR): 0 privileged read-only, 1 privileged read / write, 2 read-only, 3
//!   read / write. D = 0 on a write: "initial page write".
//! - The hardware's 4-entry instruction TLB refills itself from the UTLB, so fetches look
//!   in the UTLB directly; only a UTLB miss reaches the program.
//! - A fault records EXPEVT; the CPU puts its registers back and takes the exception
//!   (TLB misses at VBR + 0x400, the rest at VBR + 0x100), with TEA = the address and
//!   PTEH.VPN = its page.
//! - LDTLB writes the entry at MMUCR.URC, then URC moves on (round robin up to URB), so
//!   a miss handler that only calls LDTLB doesn't keep replacing one entry.
//! - The UTLB is also reachable as memory: address array F6000000, data array F7000000.
//! - A soft TLB in front (1 KB pages, direct-mapped by address, tagged with the ASID, SR.MD
//!   and MMUCR.SV) remembers recent translations and what they allow; any UTLB change
//!   empties it (an epoch count).

use serde::{Deserialize, Serialize};

pub const EXP_TLB_MISS_READ: u32 = 0x040;
pub const EXP_TLB_MISS_WRITE: u32 = 0x060;
pub const EXP_INITIAL_WRITE: u32 = 0x080;
pub const EXP_PROTECT_READ: u32 = 0x0A0;
pub const EXP_PROTECT_WRITE: u32 = 0x0C0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Access { Read, Write, Fetch }

#[derive(Clone, Copy, Default, Debug, PartialEq, Serialize, Deserialize)]
pub struct TlbEntry {
    /// virtual page (bits 31-10)
    pub vpn: u32,
    pub asid: u8,
    /// physical page (bits 28-10)
    pub ppn: u32,
    pub v: bool,
    /// 0 1 KB, 1 4 KB, 2 64 KB, 3 1 MB
    pub sz: u8,
    pub pr: u8,
    pub c: bool,
    pub d: bool,
    pub sh: bool,
    pub wt: bool,
}

impl TlbEntry {
    /// Offset bits within the page.
    pub fn mask(&self) -> u32 { [0x3FF, 0xFFF, 0xFFFF, 0xF_FFFF][self.sz as usize & 3] }

    /// From PTEH / PTEL (as LDTLB).
    pub fn load(pteh: u32, ptel: u32) -> Self {
        let mut e = Self { vpn: pteh & 0xFFFF_FC00, asid: pteh as u8, ..Self::default() };
        e.set_data(ptel);
        e
    }

    /// The PTEL-format bits: PPN, V, SZ, PR, C, D, SH, WT.
    pub fn set_data(&mut self, ptel: u32) {
        self.ppn = ptel & 0x1FFF_FC00;
        self.v = ptel & 0x100 != 0;
        self.sz = ((ptel >> 6 & 2) | (ptel >> 4 & 1)) as u8;
        self.pr = (ptel >> 5 & 3) as u8;
        self.c = ptel & 8 != 0;
        self.d = ptel & 4 != 0;
        self.sh = ptel & 2 != 0;
        self.wt = ptel & 1 != 0;
    }

    pub fn data(&self) -> u32 {
        self.ppn | (self.v as u32) << 8 | ((self.sz as u32) & 2) << 6 | ((self.sz as u32) & 1) << 4
            | (self.pr as u32) << 5 | (self.c as u32) << 3 | (self.d as u32) << 2 | (self.sh as u32) << 1 | self.wt as u32
    }

    fn hits(&self, va: u32, asid: u8, any_asid: bool) -> bool {
        self.v && (va ^ self.vpn) & !self.mask() & 0xFFFF_FC00 == 0 && (self.sh || any_asid || self.asid == asid)
    }
}

/// One soft-TLB entry: `tag` = the 1 KB page | ASID | MD << 8 | SV << 9, valid while `epoch`
/// is the MMU's; `pa` = the physical page; `ok` bit 0 reads / fetches allowed, bit 1
/// writes.
#[derive(Clone, Copy, Default)]
struct Soft { tag: u32, epoch: u32, pa: u32, ok: u8 }

const SOFT: usize = 1024;

fn soft_new() -> Vec<Soft> { vec![Soft::default(); SOFT] }
fn one() -> u32 { 1 }

#[derive(Clone, Serialize, Deserialize)]
pub struct Mmu {
    #[serde(skip, default = "soft_new")]
    soft: Vec<Soft>,
    #[serde(skip, default = "one")]
    epoch: u32,
    pub utlb: Vec<TlbEntry>,
    /// the instruction running faulted: its EXPEVT (taken by the CPU after it)
    #[serde(skip)]
    pub fault: Option<u32>,
    /// the CPU is in privileged mode (SR.MD), told before each instruction
    #[serde(skip)]
    pub md: bool,
    /// entry that hit last (looked at first)
    #[serde(skip)]
    last: usize,
    /// debug: TLB misses and LDTLBs
    #[serde(skip)]
    pub misses: u64,
    #[serde(skip)]
    pub loads: u64,
    /// debug: missed 4 KB pages -> (misses, last PC... the address kind)
    #[serde(skip)]
    pub miss_pages: std::collections::BTreeMap<u32, u32>,
}

impl Default for Mmu {
    fn default() -> Self { Self::new() }
}

impl Mmu {
    pub fn new() -> Self {
        Self { soft: soft_new(), epoch: 1, utlb: vec![TlbEntry::default(); 64], fault: None, md: true, last: 0, misses: 0, loads: 0, miss_pages: Default::default() }
    }

    /// MMUCR.TI: every entry invalid.
    pub fn flush(&mut self) { for e in self.utlb.iter_mut() { e.v = false; } self.invalidate(); }

    /// The UTLB changed: forget the soft TLB.
    pub fn invalidate(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 { self.soft.fill(Soft::default()); self.epoch = 1; }
    }

    #[inline(always)]
    fn soft_tag(va: u32, asid: u8, md: bool, mmucr: u32) -> u32 {
        va & 0xFFFF_FC00 | asid as u32 | (md as u32) << 8 | (mmucr >> 8 & 1) << 9
    }

    /// The soft TLB's physical address for `va`, if it has the page and allows the access.
    #[inline(always)]
    fn soft_get(&self, va: u32, tag: u32, write: bool) -> Option<u32> {
        let e = &self.soft[(va >> 10) as usize & (SOFT - 1)];
        if e.tag == tag && e.epoch == self.epoch && e.ok >> write as u32 & 1 != 0 { Some(e.pa | va & 0x3FF) } else { None }
    }

    /// Remember UTLB entry `e`'s translation of `va`'s 1 KB page.
    fn soft_put(&mut self, va: u32, tag: u32, e: TlbEntry) {
        let m = e.mask();
        let read = self.md || e.pr >= 2;
        let write = read && e.pr & 1 != 0 && e.d;
        let epoch = self.epoch;
        self.soft[(va >> 10) as usize & (SOFT - 1)] = Soft { tag, epoch, pa: (e.ppn & !m | va & m) & !0x3FF, ok: read as u8 | (write as u8) << 1 };
    }

    /// LDTLB: PTEH / PTEL into the entry MMUCR.URC names; returns MMUCR with URC moved on.
    pub fn ldtlb(&mut self, pteh: u32, ptel: u32, mmucr: u32) -> u32 {
        let urc = (mmucr >> 10 & 63) as usize;
        self.utlb[urc] = TlbEntry::load(pteh, ptel);
        self.invalidate();
        self.loads += 1;
        let urb = (mmucr >> 18 & 63) as usize;
        let next = if urb != 0 && urc + 1 >= urb { 0 } else { (urc + 1) & 63 };
        mmucr & !(63 << 10) | (next as u32) << 10
    }

    /// Virtual -> physical for one access. None: the access faulted (`fault` is set, and
    /// `tea` / `pteh` updated); after a fault, the rest of the instruction's accesses fail
    /// too, so it changes nothing.
    pub fn translate(&mut self, va: u32, acc: Access, pteh: &mut u32, tea: &mut u32, mmucr: u32) -> Option<u32> {
        if self.fault.is_some() { return None; }
        if va >= 0x8000_0000 && !(0xC000_0000..0xE000_0000).contains(&va) { return Some(va); }
        let asid = *pteh as u8;
        let tag = Self::soft_tag(va, asid, self.md, mmucr);
        if let Some(p) = self.soft_get(va, tag, acc == Access::Write) { return Some(p); }
        let any = self.md && mmucr & 0x100 != 0;
        let hit = if self.utlb[self.last].hits(va, asid, any) { Some(self.last) } else {
            self.utlb.iter().position(|e| e.hits(va, asid, any))
        };
        let code = match hit {
            None => {
                self.misses += 1;
                if self.miss_pages.len() < 64 || self.miss_pages.contains_key(&(va & !0xFFF)) { *self.miss_pages.entry(va & !0xFFF).or_insert(0) += 1; }
                if acc == Access::Write { EXP_TLB_MISS_WRITE } else { EXP_TLB_MISS_READ } }
            Some(i) => {
                self.last = i;
                let e = self.utlb[i];
                let write = acc == Access::Write;
                let user_blocked = !self.md && e.pr < 2;
                if user_blocked || (write && e.pr & 1 == 0) {
                    if write { EXP_PROTECT_WRITE } else { EXP_PROTECT_READ }
                } else if write && !e.d {
                    EXP_INITIAL_WRITE
                } else {
                    self.soft_put(va, tag, e);
                    let m = e.mask();
                    return Some(e.ppn & !m | va & m);
                }
            }
        };
        self.fault = Some(code);
        *tea = va;
        *pteh = *pteh & 0x3FF | va & 0xFFFF_FC00;
        None
    }

    /// Where an instruction fetch at `va` (translated: U0 / P3) goes, when the UTLB has it
    /// and allows it; nothing is recorded either way (the recompiler's lookup).
    pub fn probe_fetch(&mut self, va: u32, asid: u8, md: bool, mmucr: u32) -> Option<u32> {
        let tag = Self::soft_tag(va, asid, md, mmucr);
        if let Some(p) = self.soft_get(va, tag, false) { return Some(p); }
        let any = md && mmucr & 0x100 != 0;
        let i = if self.utlb[self.last].hits(va, asid, any) { self.last } else {
            self.utlb.iter().position(|e| e.hits(va, asid, any))?
        };
        let e = self.utlb[i];
        if !md && e.pr < 2 { return None; }
        self.last = i;
        if md == self.md { self.soft_put(va, tag, e); }
        let m = e.mask();
        Some(e.ppn & !m | va & m)
    }

    /// A store-queue flush with the MMU on: the SQ address goes through the UTLB.
    pub fn sq_target(&mut self, va: u32, pteh: &mut u32, tea: &mut u32, mmucr: u32) -> Option<u32> {
        if self.fault.is_some() { return None; }
        let asid = *pteh as u8;
        let any = self.md && mmucr & 0x100 != 0;
        match self.utlb.iter().position(|e| e.hits(va, asid, any)) {
            Some(i) => { let e = self.utlb[i]; let m = e.mask(); Some(e.ppn & !m | va & m & !0x1F) }
            None => {
                self.misses += 1;
                if self.miss_pages.len() < 64 || self.miss_pages.contains_key(&(va & !0xFFF)) { *self.miss_pages.entry(va & !0xFFF).or_insert(0) += 1; }
                self.fault = Some(EXP_TLB_MISS_WRITE);
                *tea = va;
                *pteh = *pteh & 0x3FF | va & 0xFFFF_FC00;
                None
            }
        }
    }

    /// Memory-mapped UTLB reads (F6 address array, F7 data array 1 / 2).
    pub fn array_read(&self, a: u32) -> u32 {
        let e = &self.utlb[(a >> 8 & 63) as usize];
        match a >> 24 {
            0xF6 => e.vpn | (e.d as u32) << 9 | (e.v as u32) << 8 | e.asid as u32,
            0xF7 if a & 0x80_0000 == 0 => e.data(),
            _ => 0,
        }
    }

    pub fn array_write(&mut self, a: u32, v: u32, pteh: u32, mmucr: u32) {
        self.invalidate();
        let i = (a >> 8 & 63) as usize;
        match a >> 24 {
            0xF6 if a & 0x80 != 0 => {
                // associative: the entry matching VPN (and ASID) takes V and D
                let any = self.md && mmucr & 0x100 != 0;
                let va = v & 0xFFFF_FC00;
                for e in self.utlb.iter_mut() {
                    if e.v && (va ^ e.vpn) & !e.mask() & 0xFFFF_FC00 == 0 && (e.sh || any || e.asid == pteh as u8) {
                        e.v = v & 0x100 != 0;
                        e.d = v & 0x200 != 0;
                    }
                }
            }
            0xF6 => {
                let e = &mut self.utlb[i];
                e.vpn = v & 0xFFFF_FC00;
                e.d = v & 0x200 != 0;
                e.v = v & 0x100 != 0;
                e.asid = v as u8;
            }
            0xF7 if a & 0x80_0000 == 0 => self.utlb[i].set_data(v),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mmu_with(pteh: u32, ptel: u32) -> Mmu {
        let mut m = Mmu::new();
        m.ldtlb(pteh, ptel, 0);
        m
    }

    #[test]
    fn a_4k_page_translates_and_misses_outside() {
        // VPN 00005000 ASID 1 -> PPN 0C123000, 4 KB, RW both, dirty, valid
        let mut m = mmu_with(0x0000_5001, 0x0C12_3000 | 0x100 | 0x10 | 0x60 | 4);
        let (mut pteh, mut tea) = (1u32, 0u32);
        assert_eq!(m.translate(0x0000_5BC0, Access::Fetch, &mut pteh, &mut tea, 1), Some(0x0C12_3BC0));
        assert_eq!(m.translate(0x8C00_0000, Access::Read, &mut pteh, &mut tea, 1), Some(0x8C00_0000), "P1 isn't translated");
        assert_eq!(m.translate(0x0000_6000, Access::Write, &mut pteh, &mut tea, 1), None);
        assert_eq!(m.fault, Some(EXP_TLB_MISS_WRITE));
        assert_eq!(tea, 0x6000);
        assert_eq!(pteh, 0x6001, "PTEH.VPN = the page, ASID kept");
        // after a fault the instruction's other accesses fail too
        assert_eq!(m.translate(0x0000_5000, Access::Read, &mut pteh, &mut tea, 1), None);
    }

    #[test]
    fn asid_protection_and_dirty_bits() {
        // read-only for user and privileged (PR 2), clean
        let mut m = mmu_with(0x0001_0002, 0x0C00_0000 | 0x100 | 0x10 | 0x40);
        let (mut pteh, mut tea) = (3u32, 0u32);
        assert_eq!(m.translate(0x0001_0000, Access::Read, &mut pteh, &mut tea, 1), None, "other ASID");
        assert_eq!(m.fault.take(), Some(EXP_TLB_MISS_READ));
        pteh = 2;
        assert!(m.translate(0x0001_0004, Access::Read, &mut pteh, &mut tea, 1).is_some());
        assert_eq!(m.translate(0x0001_0004, Access::Write, &mut pteh, &mut tea, 1), None);
        assert_eq!(m.fault.take(), Some(EXP_PROTECT_WRITE));
        // RW but clean: initial page write
        let mut m = mmu_with(0x0001_0002, 0x0C00_0000 | 0x100 | 0x10 | 0x60);
        assert_eq!(m.translate(0x0001_0004, Access::Write, &mut pteh, &mut tea, 1), None);
        assert_eq!(m.fault.take(), Some(EXP_INITIAL_WRITE));
        // privileged-only page from user mode
        let mut m = mmu_with(0x0001_0002, 0x0C00_0000 | 0x100 | 0x10 | 0x20 | 4);
        m.md = false;
        assert_eq!(m.translate(0x0001_0004, Access::Read, &mut pteh, &mut tea, 1), None);
        assert_eq!(m.fault.take(), Some(EXP_PROTECT_READ));
    }

    #[test]
    fn ldtlb_moves_round_robin_up_to_urb() {
        let mut m = Mmu::new();
        let mut cr = 1 | 2 << 18; // URB 2
        for k in 0..3u32 { cr = m.ldtlb(0x1000 * (k + 1), 0x100 | 0x10 | k << 12, cr); }
        assert_eq!(m.utlb[0].vpn, 0x3000, "third load wrapped to entry 0");
        assert_eq!(m.utlb[1].vpn, 0x2000);
        assert_eq!(cr >> 10 & 63, 1);
    }

    #[test]
    fn big_pages_and_the_memory_mapped_array() {
        let mut m = Mmu::new();
        // 1 MB page through the arrays: address F6000100 entry 1, data F7000100
        m.array_write(0xF600_0100, 0x0010_0000 | 0x100 | 0x200, 0, 1);
        m.array_write(0xF700_0100, 0x0C00_0000 | 0x100 | 0x90 | 0x60 | 4, 0, 1);
        assert_eq!(m.utlb[1].sz, 3);
        let (mut pteh, mut tea) = (0u32, 0u32);
        assert_eq!(m.translate(0x001A_BCDE, Access::Read, &mut pteh, &mut tea, 1), Some(0x0C0A_BCDE));
        assert_eq!(m.array_read(0xF600_0100) & 0xFFFF_FF00, 0x0010_0300);
        // associative write clears V
        m.array_write(0xF600_0080, 0x0010_0000, 0, 1);
        assert!(!m.utlb[1].v);
    }
}
