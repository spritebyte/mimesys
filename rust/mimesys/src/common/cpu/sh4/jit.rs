//! SH-4 recompiler (JIT): straight runs of instructions turned into host code with
//! Cranelift, one function per block.
//!
//! - A block starts at a PC in RAM and runs until a branch (and its delay slot), an
//!   instruction it can't take (the interpreter runs that one), a load of FPSCR, or
//!   `MAX_INSNS`. It returns the next PC and adds the instructions it ran to `Ctx::insns`,
//!   which the run loop turns into cycles. Interrupts are taken between blocks.
//! - Translated: every fast-path kind, with the same results as `Sh4::fast`. The FPU is
//!   done inline when `Sh4::fast_fpu` (plain host arithmetic, the default) and the FPU is
//!   enabled: moves (32 / 64-bit by FPSCR.SZ), single-precision arithmetic, FMAC, FCMP,
//!   FTRC / FLOAT, FIPR / FTRV and the rest, matching the interpreter bit for bit. Blocks
//!   are keyed by PC and that mode (`Jit::mode`: PR, SZ, inline). Double precision, FSCA,
//!   FRCHG and the other instructions outside the fast path that can't change SR or the PC
//!   (`callable`) are calls into the interpreter from inside the block (`h_exec`), delay
//!   slots included. The test `jit_matches_the_interpreter` runs random blocks both ways.
//! - A block is interpreted its first `HOT` runs and only then translated (translating
//!   costs ~100-300 us, so code that runs a few times isn't worth it).
//! - Linking: blocks use the tail calling convention. Each exit to a PC known at
//!   translation (fallthrough, BT / BF, BRA, BSR) has a link slot; once the run loop has
//!   seen which block follows, the slot holds it, and the block jumps straight there while
//!   instructions are left before the run loop's target and the bus's attention flag is
//!   clear. Short backward branches stay with the run loop (its polling-loop check) until
//!   the system says that loop isn't one (`Jit::not_idle`). Helpers bring the bus's clock
//!   up to the start of the running block before touching the bus, as when every block
//!   returned.
//! - Registers live in Cranelift variables inside a block: the ones its instructions can
//!   touch are loaded at the start, and the written ones stored at each exit. Memory goes
//!   to the bus's RAM directly when the address is in a region the bus maps straight to
//!   RAM (`JitMem::direct`), else through helper calls into the bus.
//! - Self-modifying code: the bus marks the 32-byte RAM lines (`CODE_SHIFT`) code came
//!   from. A write to a marked line takes the helper, the bus reports the word written
//!   (`Sh4Bus::jit_dirty`), and only the blocks whose bytes it overlaps are dropped (games
//!   keep variables right next to code), along with the links into them.
//! - With the MMU on (Windows CE games), blocks are keyed by virtual PC and the physical
//!   address the TLB gives it (`Sh4Bus::jit_fetch_pa`; not in the TLB: the interpreter's
//!   `step_mmu`, which takes the miss), stay inside one 1 KB page and aren't linked. Their
//!   helper accesses are translated by the bus; after each, a fault leaves the block with
//!   the registers as before that instruction (each instruction writes its registers after
//!   its accesses) and the PC to restart at, and the run loop takes the TLB exception.
//!   Interpreter calls (`h_exec`) put the CPU back themselves.
//! - A changed memory layout (state load, MMU on / off) flushes everything.

use super::fast::*;
use super::{Sh4, Sh4Bus, CODE_SHIFT, FPSCR_PR, FPSCR_SZ, SR_FD, SR_MD};
use cranelift_codegen::ir::{self, condcodes::IntCC, types, AbiParam, InstBuilder, MemFlagsData, Value};
use cranelift_codegen::isa::CallConv;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module};
use std::collections::{HashMap, HashSet};

/// What the bus lets translated code touch directly.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct JitMem {
    /// main RAM (16 MB: address & 0xFFFFFF)
    pub ram: *mut u8,
    /// bit n: the 64 MB region n (address >> 26) is RAM
    pub direct: u64,
    /// one byte per 4 KB RAM page: nonzero while translated code comes from it
    pub code: *const u8,
}

/// What a block function gets: the CPU, the bus, and where it leaves its results. Blocks
/// use Cranelift's tail calling convention, so a block can jump straight into the next one
/// (`Jit::links`); `Jit::enter` calls the first.
#[repr(C)]
pub struct Ctx {
    cpu: *mut Sh4,
    bus: *mut u8,
    /// the bus's attention flag (`Sh4Bus::jit_attn`): set, a block doesn't go on to the next
    attn: *const u8,
    /// instructions run, added to by each block on the way
    insns: u32,
    /// instructions left before the run loop's target: a block goes on only while it's > 0
    left: i32,
    /// the link slot the last block left by, unfilled (NO_SLOT: none)
    exit: u32,
    /// the last block's branch PC (0: it had none), for the run loop's polling-loop check
    last_branch: u32,
    /// cycles an instruction, and how many of `insns` the bus's clock has been told
    /// about: the helpers bring it up to date before going to the bus, so the bus sees the
    /// time the block started at, as when every block returned to the run loop
    cpi: u32,
    synced: u32,
    /// MMU on: an access faulted (a helper saw it); the block left with the PC to restart
    /// at and the registers as before that instruction
    fault: u32,
}

/// For a bus without an attention flag: always set, so blocks never go on.
static ATTN_SET: u8 = 1;

impl Ctx {
    fn new(cpu: *mut Sh4, bus: *mut u8, attn: *const u8) -> Self {
        Self { cpu, bus, attn, insns: 0, left: 0, exit: NO_SLOT, last_branch: 0, cpi: 1, synced: 0, fault: 0 }
    }
}

/// A translated block: the address of a tail-convention `fn(*mut Ctx) -> u32` (next PC).
type BlockFn = usize;
type Tramp = unsafe extern "C" fn(*mut Ctx, usize) -> u32;

/// Link slots: one per static exit of a translated block (fallthrough, BT / BF either way,
/// BRA, BSR), holding the next block's function once it's known, else 0.
const LINKS: usize = 1 << 18;
const NO_SLOT: u32 = u32::MAX;

/// Longest block.
const MAX_INSNS: usize = 64;
/// Blocks before everything is thrown away and translation starts over (keeps memory
/// bounded: Cranelift's JIT can't free single functions).
const MAX_BLOCKS: usize = 1 << 16;
/// Runs (interpreted) before a block is worth translating.
const HOT: u8 = 8;
/// Entries in the lookup cache.
const CACHE: usize = 1 << 16;

impl Block {
    const NONE: Block = Block { f: None, branch: 0 };
    /// Not translated yet: interpret it (to its branch) and count the run. (No branch is
    /// at an odd address.)
    const WARM: Block = Block { f: None, branch: 1 };
}

#[derive(Clone, Copy)]
struct Block {
    /// None: the first instruction isn't translated (the interpreter runs it)
    f: Option<BlockFn>,
    /// the PC of the branch that ends it (0: it doesn't end with one)
    branch: u32,
}

pub struct Jit {
    module: Option<JITModule>,
    /// direct-mapped front for `blocks`, by (pc >> 1) & (CACHE - 1); key 1 is empty
    cache: Vec<(u64, Block)>,
    /// by `key`: PC and FPU mode
    blocks: HashMap<u64, Block>,
    /// runs so far of the blocks not translated yet
    heat: HashMap<u64, u8>,
    /// runs before translating (HOT; 1 translates at once)
    pub hot: u8,
    /// RAM line (1 << CODE_SHIFT bytes) -> the blocks translated from it: (key, first byte,
    /// byte after the last, as RAM offsets)
    pages: HashMap<u32, Vec<(u64, u32, u32)>>,
    mem: Option<JitMem>,
    /// `Ctx` and a block -> runs it (made with the module)
    tramp: Option<Tramp>,
    /// link slots (heap, never moved: blocks have their addresses), what each expects (next
    /// PC, mode), the next free one, and by target block key the slots filled with it
    links: Box<[usize]>,
    slot_info: Vec<(u32, u32)>,
    rev: HashMap<u64, Vec<u32>>,
    /// loop starts the system found aren't polling loops (`not_idle`): short backward
    /// branches there may be linked
    no_idle: HashSet<u32>,
    /// the run loop watches for polling loops (short backward branches aren't linked)
    idle_watch: bool,
    fctx: FunctionBuilderContext,
    names: u64,
    dirty: Vec<u32>,
    /// debug: blocks translated, instructions in them, flushes
    pub compiled: u64,
    pub compiled_insns: u64,
    pub flushes: u64,
    pub runs: u64,
    pub evicted: u64,
    pub dirty_writes: u64,
    pub cold: u64,
    /// time spent translating, in microseconds
    pub compile_us: u64,
}

// The raw pointers are the bus's own memory, and translated code only runs on the thread
// that runs the CPU; the system moving to another thread between frames is fine.
unsafe impl Send for JitMem {}
unsafe impl Send for Jit {}

impl Default for Jit {
    fn default() -> Self { Self::new() }
}

// helpers the blocks call for memory the inline path doesn't cover; each first brings the
// bus's clock up to the start of the running block
#[inline(always)]
unsafe fn bus<'b, B: Sh4Bus>(c: *mut Ctx) -> &'b mut B {
    unsafe {
        let c = &mut *c;
        let b = &mut *(c.bus as *mut B);
        if c.insns != c.synced {
            b.tick(c.cpi as u64 * (c.insns - c.synced) as u64);
            c.synced = c.insns;
        }
        b
    }
}
/// After a helper's access: note a TLB fault for the block to leave by.
#[inline(always)]
unsafe fn faulted<B: Sh4Bus>(c: *mut Ctx, b: &B) { unsafe { if b.mmu_faulted() { (*c).fault = 1; } } }
unsafe extern "C" fn h_r8<B: Sh4Bus>(c: *mut Ctx, a: u32) -> u32 { unsafe { let b = bus::<B>(c); let v = b.read8(a) as i8 as u32; faulted(c, b); v } }
unsafe extern "C" fn h_r16<B: Sh4Bus>(c: *mut Ctx, a: u32) -> u32 { unsafe { let b = bus::<B>(c); let v = b.read16(a) as i16 as u32; faulted(c, b); v } }
unsafe extern "C" fn h_r32<B: Sh4Bus>(c: *mut Ctx, a: u32) -> u32 { unsafe { let b = bus::<B>(c); let v = b.read32(a); faulted(c, b); v } }
unsafe extern "C" fn h_w8<B: Sh4Bus>(c: *mut Ctx, a: u32, v: u32) { unsafe { let b = bus::<B>(c); b.write8(a, v as u8); faulted(c, b) } }
unsafe extern "C" fn h_w16<B: Sh4Bus>(c: *mut Ctx, a: u32, v: u32) { unsafe { let b = bus::<B>(c); b.write16(a, v as u16); faulted(c, b) } }
unsafe extern "C" fn h_w32<B: Sh4Bus>(c: *mut Ctx, a: u32, v: u32) { unsafe { let b = bus::<B>(c); b.write32(a, v); faulted(c, b) } }
unsafe extern "C" fn h_pref<B: Sh4Bus>(c: *mut Ctx, a: u32) { unsafe { let b = bus::<B>(c); b.pref(a); faulted(c, b) } }

/// An instruction by the interpreter (`callable`), on the CPU state the block has stored:
/// at `pc`, or with `bpc` != 0 in the slot of the branch at `bpc` (to `to`). Returns 1 when
/// it trapped, and the block must leave for the handler (the CPU's PC).
unsafe extern "C" fn h_exec<B: Sh4Bus>(c: *mut Ctx, pc: u32, op: u32, bpc: u32, to: u32) -> u32 {
    unsafe {
        let bus = bus::<B>(c);
        let cpu = &mut *(*c).cpu;
        let next = pc.wrapping_add(2);
        cpu.in_slot = bpc != 0;
        if bpc != 0 { cpu.branch_pc = bpc; }
        cpu.cur = pc;
        cpu.trapped = false;
        cpu.pc = next;
        // with the MMU on, an access that faults puts the registers back
        let saved = if bus.mmu() { Some(cpu.clone()) } else { None };
        cpu.execute(bus, op as u16, if bpc != 0 { Some(to) } else { None });
        if bus.mmu_faulted() {
            if let Some(s) = saved { *cpu = s; }
            cpu.pc = if bpc != 0 { bpc } else { pc };
            (*c).fault = 1;
            return 1;
        }
        (cpu.pc != next || cpu.trapped) as u32
    }
}

/// The instructions outside the fast path that blocks hand to the interpreter: none that
/// can change SR or the PC other than by trapping (LDC SR, RTE, TRAPA, SLEEP and the
/// branches stay outside blocks).
fn callable(op: u16) -> bool {
    let k = KIND[op as usize] & 127;
    if k == K_FPU { return true; }
    if k != 0 { return false; }
    !(op & 0xF0FF == 0x400E || op & 0xF0FF == 0x4007 || op == 0x002B || op == 0x001B || op & 0xFF00 == 0xC300
      || op & 0xF0FF == 0x0023 || op & 0xF0FF == 0x0003 || op & 0xF0FF == 0x402B || op & 0xF0FF == 0x400B)
}

/// FPU instructions done inline (with `Jit::mode` bit 2): everything but double-precision
/// arithmetic, FRCHG, FSCA and the illegal encodings, which go to the interpreter.
fn fpu_inline(op: u16, pr: bool) -> bool {
    match op & 15 {
        0..=5 => !pr,
        6..=0xC | 0xE => true,
        0xD => match op >> 4 & 15 {
            0x0 | 0x1 | 0x4 | 0x5 | 0x7 | 0x8 | 0x9 | 0xE => true,
            0x2 | 0x3 | 0x6 => !pr,
            0xA | 0xB => !pr,
            0xF => op == 0xF3FD || (op >> 8 & 3 == 1),
            _ => false,
        },
        _ => false,
    }
}

/// `callable` instructions outside the FPU done inline: LDS / STS FPUL, MOVCA, the cache
/// operations.
fn call_inline(op: u16) -> bool {
    matches!(op & 0xF0FF, 0x405A | 0x005A | 0x00C3 | 0x0093 | 0x00A3 | 0x00B3)
}

/// The pseudo-kind of a `callable` instruction outside the FPU.
const K_CALL: u8 = 126;

/// The kind a block treats `op` as.
fn jit_kind(op: u16) -> u8 {
    let k = KIND[op as usize] & 127;
    if k == 0 && callable(op) { K_CALL } else { k }
}

/// Can `op` sit in a translated delay slot? What the interpreter's fast slot takes, and
/// what it hands to its general slot that can't branch (PC-relative loads and branches
/// stay out).
fn slot_ok(op: u16) -> bool {
    let k = jit_kind(op);
    translatable(k) && (k < K_FIRST_SENSITIVE || callable(op))
}

/// What `Emit` needs to give a block's static exits link slots.
#[derive(Clone, Copy)]
struct Linking {
    /// the FPU mode at each exit follows from the decoding (`Ins::sz`)
    mode_known: bool,
    /// the block's branch PC (0: none)
    branch: u32,
    /// a short backward branch target may be linked (the run loop doesn't watch for
    /// polling loops, or found this one isn't)
    back_ok: bool,
    /// `Jit::links`, and the block's first slot number
    base: usize,
    first: u32,
}

/// One decoded instruction of a block.
#[derive(Clone, Copy)]
struct Ins { pc: u32, op: u16, k: u8, sz: bool }

fn is_branch(k: u8) -> bool { (K_BT..=K_BSRF).contains(&k) }
fn is_delayed(k: u8) -> bool { k != K_BT && k != K_BF && is_branch(k) }
/// Translated here (everything but the FPU and the "not fast" kind).
fn translatable(k: u8) -> bool { k != 0 && k <= K_BSRF || k == K_FPU || k == K_CALL }

impl Jit {
    pub fn new() -> Self {
        Self { module: None, cache: vec![(1, Block::NONE); CACHE], tramp: None, links: vec![0; LINKS].into_boxed_slice(), slot_info: Vec::new(), rev: HashMap::new(), no_idle: HashSet::new(), idle_watch: true, blocks: HashMap::new(), heat: HashMap::new(), hot: HOT, pages: HashMap::new(), mem: None, fctx: FunctionBuilderContext::new(),
               names: 0, dirty: Vec::new(), compiled: 0, compiled_insns: 0, flushes: 0, runs: 0, evicted: 0, dirty_writes: 0, cold: 0, compile_us: 0 }
    }

    /// Forget every block (the memory layout changed, or a state was loaded).
    pub fn flush(&mut self) {
        self.cache.fill((1, Block::NONE));
        self.links.fill(0);
        self.slot_info.clear();
        self.rev.clear();
        self.tramp = None;
        self.blocks.clear();
        self.heat.clear();
        self.pages.clear();
        // the old functions may be dropped now: nothing runs them any more
        if let Some(m) = self.module.take() { unsafe { m.free_memory(); } }
        self.mem = None;
        self.flushes += 1;
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize { self.blocks.len() }

    /// Drop the blocks on RAM pages written since the last look.
    fn evict<B: Sh4Bus>(&mut self, bus: &mut B) {
        self.dirty.clear();
        bus.jit_dirty(&mut self.dirty);
        let mut gone = Vec::new();
        for e in std::mem::take(&mut self.dirty) {
            let (lo, hi) = if e & 1 != 0 { (e & !1, (e & !1) + (1 << CODE_SHIFT)) } else { (e, e + 4) };
            let line = lo >> CODE_SHIFT;
            self.dirty_writes += 1;
            let Some(v) = self.pages.get_mut(&line) else { bus.jit_uncode(line); continue; };
            v.retain(|&(pc, a, b)| { let hit = a < hi && lo < b; if hit { gone.push((pc, a, b)); } !hit });
            if v.is_empty() { self.pages.remove(&line); bus.jit_uncode(line); }
            // each dropped block leaves the other lines it came from too
            for (key, a, b) in gone.drain(..) {
                self.evicted += 1;
                self.blocks.remove(&key);
                // the links into it go
                for sl in self.rev.remove(&key).unwrap_or_default() { self.links[sl as usize] = 0; }
                let c = &mut self.cache[(key as u32 >> 1) as usize & (CACHE - 1)];
                if c.0 == key { *c = (1, Block::NONE); }
                for l in a >> CODE_SHIFT..=(b - 1) >> CODE_SHIFT {
                    if l == line { continue; }
                    if let Some(w) = self.pages.get_mut(&l) {
                        w.retain(|x| x.0 != key);
                        if w.is_empty() { self.pages.remove(&l); bus.jit_uncode(l); }
                    }
                }
            }
        }
    }

    fn module(&mut self) -> &mut JITModule {
        if self.module.is_none() {
            let b = JITBuilder::with_flags(&[("opt_level", "none"), ("enable_verifier", "false"), ("preserve_frame_pointers", "true")], cranelift_module::default_libcall_names()).expect("host ISA");
            let mut m = JITModule::new(b);
            self.tramp = Some(Self::make_tramp(&mut m));
            self.module = Some(m);
        }
        self.module.as_mut().unwrap()
    }

    /// The signature of a block.
    fn block_sig(m: &JITModule) -> ir::Signature {
        let ptr = m.target_config().pointer_type();
        let mut s = ir::Signature::new(CallConv::Tail);
        s.params.push(AbiParam::new(ptr));
        s.returns.push(AbiParam::new(types::I32));
        s
    }

    /// `extern "C" fn(ctx, block) -> u32`: calls the block (tail convention) from Rust.
    fn make_tramp(m: &mut JITModule) -> Tramp {
        let ptr = m.target_config().pointer_type();
        let mut ctx = m.make_context();
        ctx.func.signature = ir::Signature::new(m.isa().default_call_conv());
        ctx.func.signature.params.push(AbiParam::new(ptr));
        ctx.func.signature.params.push(AbiParam::new(ptr));
        ctx.func.signature.returns.push(AbiParam::new(types::I32));
        let id = m.declare_function("enter", Linkage::Local, &ctx.func.signature).expect("declare enter");
        let mut fctx = FunctionBuilderContext::new();
        {
            let mut b = FunctionBuilder::new(&mut ctx.func, &mut fctx);
            let e = b.create_block();
            b.append_block_params_for_function_params(e);
            b.switch_to_block(e);
            b.seal_block(e);
            let (c, f) = (b.block_params(e)[0], b.block_params(e)[1]);
            let sig = b.import_signature(Self::block_sig(m));
            let call = b.ins().call_indirect(sig, f, &[c]);
            let r = b.inst_results(call)[0];
            b.ins().return_(&[r]);
            b.finalize(m.target_config());
        }
        m.define_function(id, &mut ctx).expect("define enter");
        m.clear_context(&mut ctx);
        m.finalize_definitions().expect("finalize enter");
        unsafe { std::mem::transmute::<*const u8, Tramp>(m.get_finalized_function(id)) }
    }

    /// Run block `f` (and the ones it links to) on `ctx`; returns the next PC.
    #[inline(always)]
    fn enter(&self, ctx: &mut Ctx, f: BlockFn) -> u32 {
        unsafe { (self.tramp.expect("module made with the block"))(ctx, f) }
    }

    /// The system found the loop starting at `start` isn't a polling loop: blocks translated
    /// from now on may link their short backward branches to it.
    pub fn not_idle(&mut self, start: u32) { self.no_idle.insert(start); }

    /// The run loop is about to run `key`'s block `f` after a block that left by `slot`:
    /// link them, when the slot expects it.
    #[inline(never)]
    fn link(&mut self, slot: u32, pc: u32, mode: u32, f: BlockFn) {
        let s = slot as usize;
        if s < self.slot_info.len() && self.slot_info[s] == (pc, mode) {
            self.links[s] = f;
            self.rev.entry(pc as u64 | (mode as u64) << 32).or_default().push(slot);
        }
    }

    /// The block at `pc` (translating it on first use). None when there's nothing to run
    /// translated here: not RAM, MMU on, or the first instruction is the interpreter's.
    /// `mode`: `Jit::mode` of the CPU now.
    #[inline(always)]
    fn block<B: Sh4Bus>(&mut self, bus: &mut B, pc: u32, mode: u32) -> Option<Block> {
        let key = pc as u64 | (mode as u64) << 32;
        let c = self.cache[(pc >> 1) as usize & (CACHE - 1)];
        if c.0 == key { return Some(c.1); }
        self.block_slow(bus, pc, mode)
    }

    /// What a block's translation depends on besides its code: FPSCR.PR (bit 0) and SZ
    /// (bit 1), and (bit 2) whether FPU instructions may be done inline: the plain host
    /// arithmetic (`Sh4::fast_fpu`) and the FPU enabled.
    #[inline(always)]
    pub fn mode(cpu: &Sh4) -> u32 {
        let inline = cpu.fast_fpu && !(cpu.check_fd && cpu.sr_hi & SR_FD != 0);
        (cpu.fpscr & (FPSCR_PR | FPSCR_SZ)) >> 19 | (inline as u32) << 2
    }

    #[inline(never)]
    fn block_slow<B: Sh4Bus>(&mut self, bus: &mut B, pc: u32, mode: u32) -> Option<Block> {
        let key = pc as u64 | (mode as u64) << 32;
        if let Some(b) = self.blocks.get(&key).copied() {
            self.cache[(pc >> 1) as usize & (CACHE - 1)] = (key, b);
            return Some(b);
        }
        let mem = bus.jit_mem()?;
        if self.mem != Some(mem) {
            if self.mem.is_some() { self.flush(); }
            self.mem = Some(mem);
        }
        if mem.direct >> (pc >> 26) & 1 == 0 { return None; }
        let h = self.heat.entry(key).or_insert(0);
        *h += 1;
        if *h < self.hot { return Some(Block::WARM); }
        self.heat.remove(&key);
        if self.blocks.len() >= MAX_BLOCKS || self.slot_info.len() + 2 > LINKS { self.flush(); self.mem = Some(mem); }
        let b = self.translate(bus, pc, pc, key, mem, mode);
        self.blocks.insert(key, b);
        self.cache[(pc >> 1) as usize & (CACHE - 1)] = (key, b);
        Some(b)
    }

    /// With the MMU on, the block at virtual `pc`, whose code is at physical `pa` (RAM, as
    /// `Sh4Bus::jit_fetch_pa` found). Keyed by both: the same virtual address holds other
    /// code in another process, and the same code shows at several addresses.
    #[inline(always)]
    fn block_mmu<B: Sh4Bus>(&mut self, bus: &mut B, pc: u32, pa: u32, mode: u32) -> Option<Block> {
        let key = pc as u64 | (mode as u64) << 32 | ((pa as u64 & 0xFF_FFFF) >> 10) << 35 | 1 << 63;
        let c = self.cache[(pc >> 1) as usize & (CACHE - 1)];
        if c.0 == key { return Some(c.1); }
        self.block_mmu_slow(bus, pc, pa, mode, key)
    }

    #[inline(never)]
    fn block_mmu_slow<B: Sh4Bus>(&mut self, bus: &mut B, pc: u32, pa: u32, mode: u32, key: u64) -> Option<Block> {
        if let Some(b) = self.blocks.get(&key).copied() {
            self.cache[(pc >> 1) as usize & (CACHE - 1)] = (key, b);
            return Some(b);
        }
        let mem = bus.jit_mem()?;
        if self.mem != Some(mem) {
            if self.mem.is_some() { self.flush(); }
            self.mem = Some(mem);
        }
        let h = self.heat.entry(key).or_insert(0);
        *h += 1;
        if *h < self.hot { return Some(Block::WARM); }
        self.heat.remove(&key);
        if self.blocks.len() >= MAX_BLOCKS { self.flush(); self.mem = Some(mem); }
        // the code read through P2 (untranslated) at its physical address
        let b = self.translate(bus, pc, 0xA000_0000 | pa & 0x1FFF_FFFF, key, mem, mode);
        self.blocks.insert(key, b);
        self.cache[(pc >> 1) as usize & (CACHE - 1)] = (key, b);
        Some(b)
    }

    /// The block at `start`, its code read from `fetch` (`start` itself, or with the MMU on
    /// its physical address through P2). With the MMU on (`key` bit 63) it stays inside
    /// `start`'s 1 KB page (the smallest: the next may be mapped elsewhere) and has no
    /// link slots (a link wouldn't see the TLB change).
    fn translate<B: Sh4Bus>(&mut self, bus: &mut B, start: u32, fetch: u32, key: u64, mem: JitMem, mode: u32) -> Block {
        let mmu = key >> 63 != 0;
        let at = |pc: u32| fetch.wrapping_add(pc.wrapping_sub(start));
        let in_page = |pc: u32| !mmu || (pc ^ start) & !0x3FF == 0;
        // decode; FPSCR.SZ as each instruction sees it (FSCHG done inline flips it)
        let inline = mode & 4 != 0;
        let mut sz = mode & 2 != 0;
        let mut ins: Vec<Ins> = Vec::new();
        let mut pc = start;
        let mut branch = 0;
        while ins.len() < MAX_INSNS && in_page(pc) {
            let op = bus.fetch16(at(pc));
            let k = jit_kind(op);
            if !translatable(k) { break; }
            if is_branch(k) {
                if is_delayed(k) {
                    if !in_page(pc.wrapping_add(2)) { break; }
                    let sop = bus.fetch16(at(pc.wrapping_add(2)));
                    let sk = jit_kind(sop);
                    if !slot_ok(sop) { break; }
                    ins.push(Ins { pc, op, k, sz });
                    ins.push(Ins { pc: pc.wrapping_add(2), op: sop, k: sk, sz });
                } else {
                    ins.push(Ins { pc, op, k, sz });
                }
                branch = pc;
                break;
            }
            ins.push(Ins { pc, op, k, sz });
            pc = pc.wrapping_add(2);
            if op == 0xF3FD && inline { sz = !sz; }
            // LDS / LDS.L to FPSCR: the mode after it isn't known here
            if k == K_CALL && (op & 0xF0FF == 0x406A || op & 0xF0FF == 0x4066) { break; }
        }
        if ins.is_empty() { return Block { f: None, branch: 0 }; }
        // the FPU mode at the exits is known unless FSCHG goes to the interpreter or FPSCR
        // is loaded
        let mode_known = !ins.iter().any(|x| (x.op == 0xF3FD && !inline) || (x.k == K_CALL && (x.op & 0xF0FF == 0x406A || x.op & 0xF0FF == 0x4066)));
        // the branch's static target: the only one that can be a short backward branch
        let target = ins.iter().find(|x| x.pc == branch && branch != 0).map(|x| {
            let simm = x.op as u8 as i8 as i32 as u32;
            let d12s = (((x.op as u32) << 20) as i32 >> 20) as u32;
            match x.k {
                K_BT | K_BF | K_BTS | K_BFS => branch.wrapping_add(4).wrapping_add(simm.wrapping_mul(2)),
                _ => branch.wrapping_add(4).wrapping_add(d12s.wrapping_mul(2)),
            }
        });
        let back_ok = !self.idle_watch || target.is_some_and(|t| self.no_idle.contains(&t));
        let link = Linking { mode_known, branch, back_ok, base: self.links.as_ptr() as usize, first: self.slot_info.len() as u32 };
        let link = if mmu { None } else { Some(link) };
        // the lines it came from
        let (a, b) = (fetch & 0xFF_FFFF, (at(ins.last().unwrap().pc) & 0xFF_FFFF) + 2);
        for p in a >> CODE_SHIFT..=(b - 1) >> CODE_SHIFT {
            bus.jit_code(p);
            let v = self.pages.entry(p).or_default();
            v.retain(|x| x.0 != key);
            v.push((key, a, b));
        }
        self.compiled += 1;
        self.compiled_insns += ins.len() as u64;
        let t = std::time::Instant::now();
        let (f, slots) = self.emit::<B>(&ins, mem, mode, link, mmu);
        self.slot_info.extend(slots);
        self.compile_us += t.elapsed().as_micros() as u64;
        Block { f: Some(f), branch }
    }

    fn emit<B: Sh4Bus>(&mut self, ins: &[Ins], mem: JitMem, mode: u32, link: Option<Linking>, mmu: bool) -> (BlockFn, Vec<(u32, u32)>) {
        self.names += 1;
        let name = format!("b{}", self.names);
        let mut fctx = std::mem::replace(&mut self.fctx, FunctionBuilderContext::new());
        let module = self.module();
        let tcfg = module.target_config();
        let ptr = module.target_config().pointer_type();
        let host_cc = module.isa().default_call_conv();
        let mut ctx = module.make_context();
        ctx.func.signature = Self::block_sig(module);
        let id = module.declare_function(&name, Linkage::Local, &ctx.func.signature).expect("declare block");
        let slots;
        {
            let mut b = FunctionBuilder::new(&mut ctx.func, &mut fctx);
            let mut e = Emit::new(&mut b, ptr, mem);
            e.host_cc = host_cc;
            e.pr = mode & 1 != 0;
            e.inline_fpu = mode & 4 != 0;
            e.link = link;
            e.mmu = mmu;
            e.helpers = [h_r8::<B> as *const () as usize, h_r16::<B> as *const () as usize, h_r32::<B> as *const () as usize, h_w8::<B> as *const () as usize, h_w16::<B> as *const () as usize, h_w32::<B> as *const () as usize, h_pref::<B> as *const () as usize, h_exec::<B> as *const () as usize];
            e.block(ins);
            slots = std::mem::take(&mut e.slots);
            drop(e);
            b.finalize(tcfg);
        }
        module.define_function(id, &mut ctx).expect("translate block");
        module.clear_context(&mut ctx);
        module.finalize_definitions().expect("finalize block");
        let code = module.get_finalized_function(id);
        self.fctx = fctx;
        (code as usize, slots)
    }
}

// Sh4 fields the blocks read and write
fn off_r(n: usize) -> i32 { (std::mem::offset_of!(Sh4, r) + 4 * n) as i32 }
fn off_t() -> i32 { std::mem::offset_of!(Sh4, t) as i32 }
fn off_mach() -> i32 { std::mem::offset_of!(Sh4, mach) as i32 }
fn off_macl() -> i32 { std::mem::offset_of!(Sh4, macl) as i32 }
fn off_pr() -> i32 { std::mem::offset_of!(Sh4, pr) as i32 }
fn off_gbr() -> i32 { std::mem::offset_of!(Sh4, gbr) as i32 }
fn off_fr(n: usize) -> i32 { (std::mem::offset_of!(Sh4, fr) + 4 * (n & 15)) as i32 }
fn off_xf(n: usize) -> i32 { (std::mem::offset_of!(Sh4, xf) + 4 * (n & 15)) as i32 }
fn off_fpscr() -> i32 { std::mem::offset_of!(Sh4, fpscr) as i32 }
fn off_fpul() -> i32 { std::mem::offset_of!(Sh4, fpul) as i32 }

/// Register file slots kept in variables: R0-R15, T, MACH, MACL, PR, GBR.
const V_T: usize = 16;
const V_MACH: usize = 17;
const V_MACL: usize = 18;
const V_PR: usize = 19;
const V_GBR: usize = 20;
const NV: usize = 21;

struct Emit<'a, 'b> {
    b: &'a mut FunctionBuilder<'b>,
    ptr: ir::Type,
    mem: JitMem,
    fl: MemFlagsData,
    ctx: Value,
    cpu: Value,
    v: [Variable; NV],
    /// slots written somewhere in the block (stored at the exits)
    written: [bool; NV],
    /// slots loaded (or set) somewhere before: reading any other is a translator bug
    defined: [bool; NV],
    /// slots loaded at entry
    used: [bool; NV],
    helpers: [usize; 8],
    /// FPSCR.PR for the whole block
    pr: bool,
    /// the host's calling convention (the helpers are Rust `extern "C"` functions)
    host_cc: CallConv,
    link: Option<Linking>,
    /// the link slots taken: (next PC, mode) each
    slots: Vec<(u32, u32)>,
    sig_tail: Option<ir::SigRef>,
    /// FPU instructions may be done inline (`Jit::mode` bit 2)
    inline_fpu: bool,
    sig_r: Option<ir::SigRef>,
    sig_w: Option<ir::SigRef>,
    sig_p: Option<ir::SigRef>,
    sig_f: Option<ir::SigRef>,
    /// MMU on: every helper access is followed by a fault check
    mmu: bool,
    /// the instruction being emitted: where it restarts after a fault (its branch's PC in a
    /// delay slot) and the instructions before that
    restart: (u32, u32),
}

const H_R8: usize = 0;
const H_R16: usize = 1;
const H_R32: usize = 2;
const H_W8: usize = 3;
const H_W16: usize = 4;
const H_W32: usize = 5;
const H_PREF: usize = 6;
const H_EXEC: usize = 7;

impl<'a, 'b> Emit<'a, 'b> {
    fn new(b: &'a mut FunctionBuilder<'b>, ptr: ir::Type, mem: JitMem) -> Self {
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        b.seal_block(entry);
        let ctx = b.block_params(entry)[0];
        let fl = MemFlagsData::trusted();
        let cpu = b.ins().load(ptr, fl, ctx, 0);
        let v = std::array::from_fn(|_| b.declare_var(types::I32));
        Self { b, ptr, mem, fl, ctx, cpu, v, written: [false; NV], defined: [false; NV], used: [false; NV], helpers: [0; 8], pr: false, inline_fpu: false, host_cc: CallConv::SystemV, link: None, slots: Vec::new(), sig_tail: None, sig_r: None, sig_w: None, sig_p: None, sig_f: None, mmu: false, restart: (0, 0) }
    }

    fn off(i: usize) -> i32 {
        match i { 0..=15 => off_r(i), V_MACH => off_mach(), V_MACL => off_macl(), V_PR => off_pr(), V_GBR => off_gbr(), _ => off_t() }
    }

    /// Load the slots in `which` from the CPU state.
    fn load(&mut self, which: &[bool; NV]) {
        for i in 0..NV {
            if !which[i] { continue; }
            let x = if i == V_T {
                self.b.ins().uload8(types::I32, self.fl, self.cpu, Self::off(i))
            } else {
                self.b.ins().load(types::I32, self.fl, self.cpu, Self::off(i))
            };
            self.b.def_var(self.v[i], x);
            self.defined[i] = true;
        }
    }

    fn get(&mut self, i: usize) -> Value {
        assert!(self.defined[i], "jit: slot {i} read but never loaded");
        self.b.use_var(self.v[i])
    }
    fn set(&mut self, i: usize, x: Value) { self.written[i] = true; self.defined[i] = true; self.b.def_var(self.v[i], x); }
    fn r(&mut self, n: u16) -> Value { self.get(n as usize & 15) }
    fn setr(&mut self, n: u16, x: Value) { self.set(n as usize & 15, x) }
    fn c(&mut self, k: u32) -> Value { self.b.ins().iconst(types::I32, k as i32 as i64) }
    fn bool32(&mut self, cc: Value) -> Value { self.b.ins().uextend(types::I32, cc) }
    fn sett(&mut self, cc: Value) { let x = self.bool32(cc); self.set(V_T, x); }

    /// Store the written slots, the count, and return `next`.
    fn exit(&mut self, next: Value, insns: u32) {
        self.store_written();
        self.count(insns);
        self.b.ins().return_(&[next]);
    }

    /// Add `insns` to the count and note the block's branch.
    fn count(&mut self, insns: u32) {
        let o = std::mem::offset_of!(Ctx, insns) as i32;
        let n = self.b.ins().load(types::I32, self.fl, self.ctx, o);
        let n = self.b.ins().iadd_imm_u(n, insns as i64);
        self.b.ins().store(self.fl, n, self.ctx, o);
        let br = self.c(self.link.map_or(0, |l| l.branch));
        self.b.ins().store(self.fl, br, self.ctx, std::mem::offset_of!(Ctx, last_branch) as i32);
    }

    /// An exit to a PC known here, with FPSCR.SZ `sz` there: through a link slot, so the
    /// next block can follow straight on, unless the run loop must see this exit (a short
    /// backward branch it watches for polling loops) or the mode there isn't known.
    fn exit_to(&mut self, next: u32, insns: u32, sz: bool) {
        let Some(l) = self.link else { let v = self.c(next); return self.exit(v, insns); };
        let back = l.branch != 0 && next <= l.branch && l.branch - next <= 64;
        if !l.mode_known || (back && !l.back_ok) {
            let v = self.c(next);
            return self.exit(v, insns);
        }
        let mode = self.pr as u32 | (sz as u32) << 1 | (self.inline_fpu as u32) << 2;
        let slot = l.first + self.slots.len() as u32;
        self.slots.push((next, mode));
        self.store_written();
        self.count(insns);
        let fl = self.fl;
        let lo = std::mem::offset_of!(Ctx, left) as i32;
        let left = self.b.ins().load(types::I32, fl, self.ctx, lo);
        let left = self.b.ins().iadd_imm_u(left, -(insns as i64));
        self.b.ins().store(fl, left, self.ctx, lo);
        let base = self.b.ins().iconst(self.ptr, (l.base + 8 * slot as usize) as i64);
        let f = self.b.ins().load(self.ptr, fl, base, 0);
        let ap = self.b.ins().load(self.ptr, fl, self.ctx, std::mem::offset_of!(Ctx, attn) as i32);
        let attn = self.b.ins().uload8(types::I32, fl, ap, 0);
        let has = self.b.ins().icmp_imm_u(IntCC::NotEqual, f, 0);
        let more = self.b.ins().icmp_imm_u(IntCC::SignedGreaterThan, left, 0);
        let calm = self.b.ins().icmp_imm_u(IntCC::Equal, attn, 0);
        let ok = self.b.ins().band(has, more);
        let ok = self.b.ins().band(ok, calm);
        let go = self.b.create_block();
        let out = self.b.create_block();
        self.b.ins().brif(ok, go, &[], out, &[]);
        self.b.switch_to_block(go);
        self.b.seal_block(go);
        let sig = match self.sig_tail {
            Some(s) => s,
            None => {
                let mut s = ir::Signature::new(CallConv::Tail);
                s.params.push(AbiParam::new(self.ptr));
                s.returns.push(AbiParam::new(types::I32));
                let r = self.b.import_signature(s);
                self.sig_tail = Some(r);
                r
            }
        };
        self.b.ins().return_call_indirect(sig, f, &[self.ctx]);
        self.b.switch_to_block(out);
        self.b.seal_block(out);
        let sv = self.c(slot);
        self.b.ins().store(fl, sv, self.ctx, std::mem::offset_of!(Ctx, exit) as i32);
        let v = self.c(next);
        self.b.ins().return_(&[v]);
    }

    /// FPSCR.SZ after `x`.
    fn sz_after(&self, x: Ins) -> bool { x.sz ^ (x.op == 0xF3FD && self.inline_fpu) }

    /// An instruction through the interpreter (`h_exec`): the registers go back to the CPU
    /// state for it and come back after. `slot`: the branch's PC and target.
    fn callout(&mut self, pc: u32, op: u16, k: u8, count: u32, slot: Option<(u32, Value)>) {
        self.store_written();
        // the CPU state is current now: later exits store only what's written after
        self.written = [false; NV];
        let (a, b) = (self.c(pc), self.c(op as u32));
        let (bpc, to) = match slot { Some((p, t)) => (self.c(p), t), None => { let z = self.c(0); (z, z) } };
        let trapped = self.call(H_EXEC, &[a, b, bpc, to]).unwrap();
        let out = self.b.create_block();
        let on = self.b.create_block();
        self.b.ins().brif(trapped, out, &[], on, &[]);
        // trapped: the handler's state (maybe another register bank) is in the CPU already
        self.b.switch_to_block(out);
        self.b.seal_block(out);
        let to = self.b.ins().load(types::I32, self.fl, self.cpu, std::mem::offset_of!(Sh4, pc) as i32);
        self.count(count);
        self.b.ins().return_(&[to]);
        self.b.switch_to_block(on);
        self.b.seal_block(on);
        let back = if k == K_FPU {
            // what an FPU instruction can change: Rm (FMOV @Rm+), Rn (FMOV @-Rn), T (FCMP)
            let mut back = [false; NV];
            back[(op >> 8 & 15) as usize] = true;
            back[(op >> 4 & 15) as usize] = true;
            back[V_T] = true;
            back
        } else {
            self.used
        };
        self.load(&back);
    }

    /// One instruction, translated or handed over.
    fn emit_one(&mut self, x: Ins, count: u32, slot: Option<(u32, Value)>) {
        self.restart = match slot { Some((p, _)) => (p, count - 2), None => (x.pc, count - 1) };
        match x.k {
            K_FPU if self.inline_fpu && fpu_inline(x.op, self.pr) => self.fpu(x),
            // LDS / STS FPUL raise the FPU-disabled exception when SR.FD is set: inline only
            // in blocks translated for an enabled FPU
            K_CALL if call_inline(x.op) && (self.inline_fpu || !matches!(x.op & 0xF0FF, 0x405A | 0x005A)) => self.call_op(x.op),
            K_FPU | K_CALL => self.callout(x.pc, x.op, x.k, count, slot),
            _ => self.one(x.pc, x.op, x.k),
        }
    }

    // --- the FPU, inline (the interpreter's plain-host-arithmetic results, `Sh4::fpu`) ---
    // FPU registers stay in the CPU state: each instruction loads and stores them.

    fn fr_i(&mut self, n: usize) -> Value { self.b.ins().load(types::I32, self.fl, self.cpu, off_fr(n)) }
    fn set_fr_i(&mut self, n: usize, x: Value) { self.b.ins().store(self.fl, x, self.cpu, off_fr(n)); }
    fn fr_f(&mut self, n: usize) -> Value { self.b.ins().load(types::F32, self.fl, self.cpu, off_fr(n)) }
    fn set_fr_f(&mut self, n: usize, x: Value) { self.b.ins().store(self.fl, x, self.cpu, off_fr(n)); }
    fn fr_d(&mut self, n: usize) -> Value { let x = self.fr_f(n); self.b.ins().fpromote(types::F64, x) }
    fn xf_d(&mut self, n: usize) -> Value {
        let x = self.b.ins().load(types::F32, self.fl, self.cpu, off_xf(n));
        self.b.ins().fpromote(types::F64, x)
    }
    fn fpul(&mut self) -> Value { self.b.ins().load(types::I32, self.fl, self.cpu, off_fpul()) }
    fn set_fpul(&mut self, x: Value) { self.b.ins().store(self.fl, x, self.cpu, off_fpul()); }

    /// One word of a 64-bit register (SZ moves): even n = DRn, odd n = XD(n - 1); `hi`: the
    /// second word.
    fn pair_off(n: usize, hi: bool) -> i32 {
        let k = (n & 14) + hi as usize;
        if n & 1 == 0 { off_fr(k) } else { off_xf(k) }
    }
    fn pair_get(&mut self, n: usize) -> (Value, Value) {
        let lo = self.b.ins().load(types::I32, self.fl, self.cpu, Self::pair_off(n, false));
        let hi = self.b.ins().load(types::I32, self.fl, self.cpu, Self::pair_off(n, true));
        (lo, hi)
    }
    fn pair_set(&mut self, n: usize, (lo, hi): (Value, Value)) {
        self.b.ins().store(self.fl, lo, self.cpu, Self::pair_off(n, false));
        self.b.ins().store(self.fl, hi, self.cpu, Self::pair_off(n, true));
    }
    /// A 64-bit load: the word at the lower address first, as the bus's `read64`.
    fn read64(&mut self, a: Value) -> (Value, Value) {
        let lo = self.read(4, a);
        let a4 = self.b.ins().iadd_imm_u(a, 4);
        let hi = self.read(4, a4);
        (lo, hi)
    }
    fn write64(&mut self, a: Value, (lo, hi): (Value, Value)) {
        self.write(4, a, lo);
        let a4 = self.b.ins().iadd_imm_u(a, 4);
        self.write(4, a4, hi);
    }

    fn fpu(&mut self, x: Ins) {
        let op = x.op;
        let (n, m) = ((op >> 8 & 15) as usize, (op >> 4 & 15) as usize);
        let sz = x.sz;
        match op & 15 {
            0..=3 => {
                let (a, b) = (self.fr_f(n), self.fr_f(m));
                let r = match op & 3 { 0 => self.b.ins().fadd(a, b), 1 => self.b.ins().fsub(a, b), 2 => self.b.ins().fmul(a, b), _ => self.b.ins().fdiv(a, b) };
                self.set_fr_f(n, r);
            }
            4 | 5 => {
                let (a, b) = (self.fr_f(n), self.fr_f(m));
                let cc = if op & 1 == 0 { ir::condcodes::FloatCC::Equal } else { ir::condcodes::FloatCC::GreaterThan };
                let c = self.b.ins().fcmp(cc, a, b);
                self.sett(c);
            }
            6 | 8 | 9 => {
                // FMOV @(R0,Rm) / @Rm / @Rm+ -> FRn / pair
                let a = match op & 15 { 6 => { let (r0, rm) = (self.r(0), self.r(m as u16)); self.b.ins().iadd(rm, r0) } _ => self.r(m as u16) };
                if sz {
                    let v = self.read64(a);
                    if op & 15 == 9 { let a8 = self.b.ins().iadd_imm_u(a, 8); self.setr(m as u16, a8); }
                    self.pair_set(n, v);
                } else {
                    let v = self.read(4, a);
                    if op & 15 == 9 { let a4 = self.b.ins().iadd_imm_u(a, 4); self.setr(m as u16, a4); }
                    self.set_fr_i(n, v);
                }
            }
            7 | 0xA => {
                // FMOV FRm / pair -> @(R0,Rn) / @Rn
                let a = if op & 15 == 7 { let (r0, rn) = (self.r(0), self.r(n as u16)); self.b.ins().iadd(rn, r0) } else { self.r(n as u16) };
                if sz { let v = self.pair_get(m); self.write64(a, v); } else { let v = self.fr_i(m); self.write(4, a, v); }
            }
            0xB => {
                // FMOV FRm / pair -> @-Rn
                if sz {
                    let v = self.pair_get(m);
                    let rn = self.r(n as u16);
                    let a = self.b.ins().iadd_imm_u(rn, -8i64 as u64 as i64);
                    self.write64(a, v);
                    self.setr(n as u16, a);
                } else {
                    let v = self.fr_i(m);
                    let rn = self.r(n as u16);
                    let a = self.b.ins().iadd_imm_u(rn, -4i64 as u64 as i64);
                    self.write(4, a, v);
                    self.setr(n as u16, a);
                }
            }
            0xC => {
                if sz { let v = self.pair_get(m); self.pair_set(n, v); } else { let v = self.fr_i(m); self.set_fr_i(n, v); }
            }
            0xD => self.fpu_d(op, n, m),
            0xE => {
                // FMAC FR0,FRm,FRn: the product in double precision, the add, then narrowed
                if self.pr { return; }
                let (a, b, c) = (self.fr_d(0), self.fr_d(m), self.fr_d(n));
                let p = self.b.ins().fmul(a, b);
                let s = self.b.ins().fadd(p, c);
                let r = self.b.ins().fdemote(types::F32, s);
                self.set_fr_f(n, r);
            }
            _ => unreachable!("fpu_inline"),
        }
    }

    fn fpu_d(&mut self, op: u16, n: usize, m: usize) {
        let pr = self.pr;
        match m {
            0x0 => { let v = self.fpul(); self.set_fr_i(n, v); }   // FSTS
            0x1 => { let v = self.fr_i(n); self.set_fpul(v); }     // FLDS
            0x2 => {
                // FLOAT FPUL,FRn
                let v = self.fpul();
                let r = self.b.ins().fcvt_from_sint(types::F32, v);
                self.set_fr_f(n, r);
            }
            0x3 => {
                // FTRC FRm,FPUL: truncate, saturating at 7FFFFF80 above, NaN -> 80000000
                let x = self.fr_f(n);
                let top = self.b.ins().f32const(2147483520.0f32);
                let big = self.b.ins().fcmp(ir::condcodes::FloatCC::GreaterThan, x, top);
                let nan = self.b.ins().fcmp(ir::condcodes::FloatCC::Unordered, x, x);
                let v = self.b.ins().fcvt_to_sint_sat(types::I32, x);
                let hi = self.c(0x7FFF_FF80);
                let v = self.b.ins().select(big, hi, v);
                let nv = self.c(0x8000_0000);
                let v = self.b.ins().select(nan, nv, v);
                self.set_fpul(v);
            }
            0x4 | 0x5 => {
                // FNEG / FABS (a double's sign is in its first register)
                let k = if pr { n & 14 } else { n };
                let v = self.fr_i(k);
                let r = if m == 4 { self.b.ins().bxor_imm_u(v, 0x8000_0000) } else { self.b.ins().band_imm_u(v, 0x7FFF_FFFF) };
                self.set_fr_i(k, r);
            }
            0x6 => { let v = self.fr_f(n); let r = self.b.ins().sqrt(v); self.set_fr_f(n, r); } // FSQRT
            0x7 => {
                // FSRRA: 1 / sqrt in double precision, narrowed
                if pr { return; }
                let x = self.fr_d(n);
                let q = self.b.ins().sqrt(x);
                let one = self.b.ins().f64const(1.0);
                let r = self.b.ins().fdiv(one, q);
                let r = self.b.ins().fdemote(types::F32, r);
                self.set_fr_f(n, r);
            }
            0x8 | 0x9 => { if pr { return; } let v = self.c(if m == 8 { 0 } else { 0x3F80_0000 }); self.set_fr_i(n, v); }
            0xA | 0xB => { /* FCNVSD / FCNVDS: nothing without PR (with it, not inline) */ }
            0xE => {
                // FIPR FVm,FVn: summed in double precision from +0, rounded once
                if pr { return; }
                let (vn, vm) = (n & 12, (n & 3) << 2);
                let mut sum = self.b.ins().f64const(0.0);
                for k in 0..4 {
                    let (a, b) = (self.fr_d(vm + k), self.fr_d(vn + k));
                    let p = self.b.ins().fmul(a, b);
                    sum = self.b.ins().fadd(sum, p);
                }
                let r = self.b.ins().fdemote(types::F32, sum);
                self.set_fr_f(vn + 3, r);
            }
            0xF => {
                if op == 0xF3FD {
                    // FSCHG (the decoder followed it)
                    let f = self.b.ins().load(types::I32, self.fl, self.cpu, off_fpscr());
                    let f = self.b.ins().bxor_imm_u(f, FPSCR_SZ as i64);
                    self.b.ins().store(self.fl, f, self.cpu, off_fpscr());
                    return;
                }
                // FTRV XMTRX,FVn
                if pr { return; }
                let vn = n & 12;
                let v: Vec<Value> = (0..4).map(|j| self.fr_d(vn + j)).collect();
                for i in 0..4 {
                    let mut sum = self.b.ins().f64const(0.0);
                    for (j, &vj) in v.iter().enumerate() {
                        let a = self.xf_d(i + 4 * j);
                        let p = self.b.ins().fmul(a, vj);
                        sum = self.b.ins().fadd(sum, p);
                    }
                    let r = self.b.ins().fdemote(types::F32, sum);
                    self.set_fr_f(vn + i, r);
                }
            }
            _ => unreachable!("fpu_inline"),
        }
    }

    /// The `call_inline` instructions.
    fn call_op(&mut self, op: u16) {
        let n = op >> 8 & 15;
        match op & 0xF0FF {
            0x405A => { let v = self.r(n); self.set_fpul(v); }   // LDS Rn,FPUL
            0x005A => { let v = self.fpul(); self.setr(n, v); }  // STS FPUL,Rn
            0x00C3 => { let (a, v) = (self.r(n), self.r(0)); self.write(4, a, v); } // MOVCA.L R0,@Rn
            _ => {} // OCBI / OCBP / OCBWB: no cache model
        }
    }

    /// After a helper access with the MMU on: when it faulted, leave with the registers as
    /// before this instruction (its register writes come after its accesses) and its
    /// restart PC; the run loop takes the exception.
    fn fault_check(&mut self) {
        if !self.mmu { return; }
        let f = self.b.ins().load(types::I32, self.fl, self.ctx, std::mem::offset_of!(Ctx, fault) as i32);
        let bad = self.b.create_block();
        let ok = self.b.create_block();
        self.b.set_cold_block(bad);
        self.b.ins().brif(f, bad, &[], ok, &[]);
        self.b.switch_to_block(bad);
        self.b.seal_block(bad);
        let (pc, before) = self.restart;
        self.store_written();
        self.count(before);
        let v = self.c(pc);
        self.b.ins().return_(&[v]);
        self.b.switch_to_block(ok);
        self.b.seal_block(ok);
    }

    fn store_written(&mut self) {
        for i in 0..NV {
            if !self.written[i] { continue; }
            let x = self.get(i);
            if i == V_T {
                self.b.ins().istore8(self.fl, x, self.cpu, Self::off(i));
            } else {
                self.b.ins().store(self.fl, x, self.cpu, Self::off(i));
            }
        }
    }

    fn sig(&mut self, kind: u8) -> ir::SigRef {
        let ptr = self.ptr;
        let slot = match kind { 0 => &mut self.sig_r, 1 => &mut self.sig_w, 3 => &mut self.sig_f, _ => &mut self.sig_p };
        if let Some(s) = *slot { return s; }
        let mut s = ir::Signature::new(self.host_cc);
        s.params.push(AbiParam::new(ptr));
        s.params.push(AbiParam::new(types::I32));
        if kind == 1 || kind == 3 { s.params.push(AbiParam::new(types::I32)); }
        if kind == 3 { s.params.push(AbiParam::new(types::I32)); s.params.push(AbiParam::new(types::I32)); }
        if kind == 0 || kind == 3 { s.returns.push(AbiParam::new(types::I32)); }
        let r = self.b.import_signature(s);
        *slot = Some(r);
        r
    }

    fn call(&mut self, h: usize, args: &[Value]) -> Option<Value> {
        let kind = match h { H_R8 | H_R16 | H_R32 => 0, H_PREF => 2, H_EXEC => 3, _ => 1 };
        let sig = self.sig(kind);
        let f = self.b.ins().iconst(self.ptr, self.helpers[h] as i64);
        let mut a = vec![self.ctx];
        a.extend_from_slice(args);
        let call = self.b.ins().call_indirect(sig, f, &a);
        self.b.inst_results(call).first().copied()
    }

    /// Is `a` in a region that's RAM? (as an i8 condition)
    fn in_ram(&mut self, a: Value) -> Value {
        let reg = self.b.ins().ushr_imm_u(a, 26);
        let reg = self.b.ins().uextend(types::I64, reg);
        let d = self.b.ins().iconst(types::I64, self.mem.direct as i64);
        let bit = self.b.ins().ushr(d, reg);
        let bit = self.b.ins().band_imm_u(bit, 1);
        self.b.ins().icmp_imm_u(IntCC::NotEqual, bit, 0)
    }

    fn host(&mut self, a: Value, mask: u32) -> Value {
        let o = self.b.ins().band_imm_u(a, mask as i64);
        let o = self.b.ins().uextend(self.ptr, o);
        let base = self.b.ins().iconst(self.ptr, self.mem.ram as i64);
        self.b.ins().iadd(base, o)
    }

    /// A load of 1, 2 or 4 bytes (8 and 16 sign-extended, as the interpreter does).
    fn read(&mut self, size: u32, a: Value) -> Value {
        let fast = self.b.create_block();
        let slow = self.b.create_block();
        let join = self.b.create_block();
        self.b.append_block_param(join, types::I32);
        let ok = self.in_ram(a);
        self.b.ins().brif(ok, fast, &[], slow, &[]);
        self.b.switch_to_block(fast);
        self.b.seal_block(fast);
        let fl = self.fl;
        let x = match size {
            1 => { let p = self.host(a, 0xFF_FFFF); self.b.ins().sload8(types::I32, fl, p, 0) }
            2 => { let p = self.host(a, 0xFF_FFFE); self.b.ins().sload16(types::I32, fl, p, 0) }
            _ => { let p = self.host(a, 0xFF_FFFC); self.b.ins().load(types::I32, fl, p, 0) }
        };
        self.b.ins().jump(join, &[x.into()]);
        self.b.switch_to_block(slow);
        self.b.seal_block(slow);
        let h = match size { 1 => H_R8, 2 => H_R16, _ => H_R32 };
        let y = self.call(h, &[a]).unwrap();
        self.fault_check();
        self.b.ins().jump(join, &[y.into()]);
        self.b.switch_to_block(join);
        self.b.seal_block(join);
        self.b.block_params(join)[0]
    }

    /// A store; RAM directly unless the line holds translated code.
    fn write(&mut self, size: u32, a: Value, x: Value) {
        let check = self.b.create_block();
        let fast = self.b.create_block();
        let slow = self.b.create_block();
        let join = self.b.create_block();
        let ok = self.in_ram(a);
        self.b.ins().brif(ok, check, &[], slow, &[]);
        self.b.switch_to_block(check);
        self.b.seal_block(check);
        let page = self.b.ins().ushr_imm_u(a, CODE_SHIFT as i64);
        let page = self.b.ins().band_imm_u(page, (0xFF_FFFF >> CODE_SHIFT) as i64);
        let page = self.b.ins().uextend(self.ptr, page);
        let cb = self.b.ins().iconst(self.ptr, self.mem.code as i64);
        let cp = self.b.ins().iadd(cb, page);
        let flag = self.b.ins().uload8(types::I32, self.fl, cp, 0);
        self.b.ins().brif(flag, slow, &[], fast, &[]);
        self.b.switch_to_block(fast);
        self.b.seal_block(fast);
        let fl = self.fl;
        match size {
            1 => { let p = self.host(a, 0xFF_FFFF); self.b.ins().istore8(fl, x, p, 0); }
            2 => { let p = self.host(a, 0xFF_FFFE); self.b.ins().istore16(fl, x, p, 0); }
            _ => { let p = self.host(a, 0xFF_FFFC); self.b.ins().store(fl, x, p, 0); }
        }
        self.b.ins().jump(join, &[]);
        self.b.switch_to_block(slow);
        self.b.seal_block(slow);
        let h = match size { 1 => H_W8, 2 => H_W16, _ => H_W32 };
        self.call(h, &[a, x]);
        self.fault_check();
        self.b.ins().jump(join, &[]);
        self.b.switch_to_block(join);
        self.b.seal_block(join);
    }

    fn block(&mut self, ins: &[Ins]) {
        // the slots any instruction can touch: its Rn and Rm fields, R0, and the specials
        let mut used = [false; NV];
        used[16..].fill(true);
        used[0] = true;
        for x in ins {
            used[(x.op >> 8 & 15) as usize] = true;
            used[(x.op >> 4 & 15) as usize] = true;
        }
        self.used = used;
        self.load(&used);
        let mut i = 0;
        while i < ins.len() {
            let Ins { pc, k, .. } = ins[i];
            let count = i as u32 + 1;
            if is_branch(k) {
                self.branch(ins, i);
                return;
            }
            self.emit_one(ins[i], count, None);
            i += 1;
            if i == ins.len() {
                let sz = self.sz_after(ins[i - 1]);
                self.exit_to(pc.wrapping_add(2), count, sz);
                return;
            }
        }
    }

    /// The branch at `ins[i]` (with its slot after it when delayed), ending the block.
    fn branch(&mut self, ins: &[Ins], i: usize) {
        let Ins { pc, op, k, .. } = ins[i];
        let n = op >> 8 & 15;
        let simm = op as u8 as i8 as i32 as u32;
        let disp8 = pc.wrapping_add(4).wrapping_add(simm.wrapping_mul(2));
        let d12s = (((op as u32) << 20) as i32 >> 20) as u32;
        let disp12 = pc.wrapping_add(4).wrapping_add(d12s.wrapping_mul(2));
        let before = i as u32; // instructions before the branch
        match k {
            K_BT | K_BF | K_BTS | K_BFS => {
                let t = self.get(V_T);
                let want = if k == K_BT || k == K_BTS { IntCC::NotEqual } else { IntCC::Equal };
                let cc = self.b.ins().icmp_imm_u(want, t, 0);
                let taken = self.b.create_block();
                let not = self.b.create_block();
                self.b.ins().brif(cc, taken, &[], not, &[]);
                self.b.switch_to_block(not);
                self.b.seal_block(not);
                self.exit_to(pc.wrapping_add(2), before + 1, ins[i].sz);
                self.b.switch_to_block(taken);
                self.b.seal_block(taken);
                if k == K_BTS || k == K_BFS {
                    let to = self.c(disp8);
                    self.emit_one(ins[i + 1], before + 2, Some((pc, to)));
                    let sz = self.sz_after(ins[i + 1]);
                    self.exit_to(disp8, before + 2, sz);
                } else {
                    self.exit_to(disp8, before + 1, ins[i].sz);
                }
            }
            _ => {
                let ret = self.c(pc.wrapping_add(4));
                let to = match k {
                    K_BRA => self.c(disp12),
                    K_BSR => { self.set(V_PR, ret); self.c(disp12) }
                    K_JSR => { let t = self.r(n); self.set(V_PR, ret); t }
                    K_JMP => self.r(n),
                    K_RTS => self.get(V_PR),
                    K_BRAF => { let x = self.r(n); self.b.ins().iadd(ret, x) }
                    K_BSRF => { let x = self.r(n); let t = self.b.ins().iadd(ret, x); self.set(V_PR, ret); t }
                    _ => unreachable!(),
                };
                self.emit_one(ins[i + 1], before + 2, Some((pc, to)));
                if k == K_BRA || k == K_BSR {
                    let sz = self.sz_after(ins[i + 1]);
                    self.exit_to(disp12, before + 2, sz);
                } else {
                    self.exit(to, before + 2);
                }
            }
        }
    }

    /// One non-branch instruction, as `Sh4::fast` does it.
    fn one(&mut self, pc: u32, op: u16, k: u8) {
        let n = op >> 8 & 15;
        let m = op >> 4 & 15;
        let d4 = (op & 15) as u32;
        let d8 = (op & 0xFF) as u32;
        let simm = op as u8 as i8 as i32 as u32;
        macro_rules! rn { () => { self.r(n) } }
        macro_rules! rm { () => { self.r(m) } }
        macro_rules! addr { ($base:expr, $d:expr) => {{ let b = $base; if $d == 0 { b } else { self.b.ins().iadd_imm_u(b, $d as i32 as i64) } }} }
        match k {
            K_MOV => { let x = rm!(); self.setr(n, x); }
            K_MOVI => { let x = self.c(simm); self.setr(n, x); }
            K_LDB | K_LDW | K_LDL => { let a = rm!(); let s = [1, 2, 4][(k - K_LDB) as usize]; let x = self.read(s, a); self.setr(n, x); }
            K_LDBP | K_LDWP | K_LDLP => {
                let s = [1u32, 2, 4][(k - K_LDBP) as usize];
                let a = rm!();
                let x = self.read(s, a);
                if n != m { let a2 = self.b.ins().iadd_imm_u(a, s as i64); self.setr(m, a2); }
                self.setr(n, x);
            }
            K_LDLD => { let a = addr!(rm!(), d4 * 4); let x = self.read(4, a); self.setr(n, x); }
            K_LDBX | K_LDWX | K_LDLX => {
                let s = [1, 2, 4][(k - K_LDBX) as usize];
                let (b, z) = (rm!(), self.r(0));
                let a = self.b.ins().iadd(b, z);
                let x = self.read(s, a);
                self.setr(n, x);
            }
            K_LDB0 => { let a = addr!(rm!(), d4); let x = self.read(1, a); self.setr(0, x); }
            K_LDW0 => { let a = addr!(rm!(), d4 * 2); let x = self.read(2, a); self.setr(0, x); }
            K_LDBG => { let g = self.get(V_GBR); let a = addr!(g, d8); let x = self.read(1, a); self.setr(0, x); }
            K_LDWG => { let g = self.get(V_GBR); let a = addr!(g, d8 * 2); let x = self.read(2, a); self.setr(0, x); }
            K_LDLG => { let g = self.get(V_GBR); let a = addr!(g, d8 * 4); let x = self.read(4, a); self.setr(0, x); }
            K_STB | K_STW | K_STL => { let s = [1, 2, 4][(k - K_STB) as usize]; let (a, x) = (rn!(), rm!()); self.write(s, a, x); }
            K_STBM | K_STWM | K_STLM => {
                let s = [1u32, 2, 4][(k - K_STBM) as usize];
                let x = rm!();
                let a = rn!();
                let a = self.b.ins().iadd_imm_u(a, -(s as i64));
                self.write(s, a, x);
                self.setr(n, a);
            }
            K_STLD => { let a = addr!(rn!(), d4 * 4); let x = rm!(); self.write(4, a, x); }
            K_STBX | K_STWX | K_STLX => {
                let s = [1, 2, 4][(k - K_STBX) as usize];
                let (b, z) = (rn!(), self.r(0));
                let a = self.b.ins().iadd(b, z);
                let x = rm!();
                self.write(s, a, x);
            }
            K_STB0 => { let a = addr!(rm!(), d4); let x = self.r(0); self.write(1, a, x); }
            K_STW0 => { let a = addr!(rm!(), d4 * 2); let x = self.r(0); self.write(2, a, x); }
            K_STBG => { let g = self.get(V_GBR); let a = addr!(g, d8); let x = self.r(0); self.write(1, a, x); }
            K_STWG => { let g = self.get(V_GBR); let a = addr!(g, d8 * 2); let x = self.r(0); self.write(2, a, x); }
            K_STLG => { let g = self.get(V_GBR); let a = addr!(g, d8 * 4); let x = self.r(0); self.write(4, a, x); }
            K_ADD => { let (a, b) = (rn!(), rm!()); let x = self.b.ins().iadd(a, b); self.setr(n, x); }
            K_ADDI => { let a = rn!(); let x = self.b.ins().iadd_imm_u(a, simm as i32 as i64); self.setr(n, x); }
            K_SUB => { let (a, b) = (rn!(), rm!()); let x = self.b.ins().isub(a, b); self.setr(n, x); }
            K_NEG => { let b = rm!(); let x = self.b.ins().ineg(b); self.setr(n, x); }
            K_NOT => { let b = rm!(); let x = self.b.ins().bnot(b); self.setr(n, x); }
            K_AND => { let (a, b) = (rn!(), rm!()); let x = self.b.ins().band(a, b); self.setr(n, x); }
            K_XOR => { let (a, b) = (rn!(), rm!()); let x = self.b.ins().bxor(a, b); self.setr(n, x); }
            K_OR => { let (a, b) = (rn!(), rm!()); let x = self.b.ins().bor(a, b); self.setr(n, x); }
            K_ANDI => { let a = self.r(0); let x = self.b.ins().band_imm_u(a, d8 as i64); self.setr(0, x); }
            K_XORI => { let a = self.r(0); let x = self.b.ins().bxor_imm_u(a, d8 as i64); self.setr(0, x); }
            K_ORI => { let a = self.r(0); let x = self.b.ins().bor_imm_u(a, d8 as i64); self.setr(0, x); }
            K_TSTI => { let a = self.r(0); let x = self.b.ins().band_imm_u(a, d8 as i64); let cc = self.b.ins().icmp_imm_u(IntCC::Equal, x, 0); self.sett(cc); }
            K_ADDC => {
                let (a, rmv, t) = (rn!(), rm!(), self.get(V_T));
                let b = self.b.ins().iadd(a, rmv);
                let r = self.b.ins().iadd(b, t);
                let c1 = self.b.ins().icmp(IntCC::UnsignedGreaterThan, a, b);
                let c2 = self.b.ins().icmp(IntCC::UnsignedGreaterThan, b, r);
                let cc = self.b.ins().bor(c1, c2);
                self.setr(n, r);
                self.sett(cc);
            }
            K_SUBC => {
                let (a, rmv, t) = (rn!(), rm!(), self.get(V_T));
                let b = self.b.ins().isub(a, rmv);
                let r = self.b.ins().isub(b, t);
                let c1 = self.b.ins().icmp(IntCC::UnsignedLessThan, a, b);
                let c2 = self.b.ins().icmp(IntCC::UnsignedLessThan, b, r);
                let cc = self.b.ins().bor(c1, c2);
                self.setr(n, r);
                self.sett(cc);
            }
            K_NEGC => {
                let (rmv, t) = (rm!(), self.get(V_T));
                let tt = self.b.ins().ineg(rmv);
                let r = self.b.ins().isub(tt, t);
                let c1 = self.b.ins().icmp_imm_u(IntCC::NotEqual, tt, 0);
                let c2 = self.b.ins().icmp(IntCC::UnsignedLessThan, tt, r);
                let cc = self.b.ins().bor(c1, c2);
                self.setr(n, r);
                self.sett(cc);
            }
            K_CMPEQ | K_CMPHS | K_CMPGE | K_CMPHI | K_CMPGT | K_TST => {
                let (a, b) = (rn!(), rm!());
                let cc = match k {
                    K_CMPEQ => self.b.ins().icmp(IntCC::Equal, a, b),
                    K_CMPHS => self.b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, a, b),
                    K_CMPGE => self.b.ins().icmp(IntCC::SignedGreaterThanOrEqual, a, b),
                    K_CMPHI => self.b.ins().icmp(IntCC::UnsignedGreaterThan, a, b),
                    K_CMPGT => self.b.ins().icmp(IntCC::SignedGreaterThan, a, b),
                    _ => { let x = self.b.ins().band(a, b); self.b.ins().icmp_imm_u(IntCC::Equal, x, 0) }
                };
                self.sett(cc);
            }
            K_CMPIM => { let a = self.r(0); let cc = self.b.ins().icmp_imm_u(IntCC::Equal, a, simm as i32 as i64); self.sett(cc); }
            K_CMPPZ => { let a = rn!(); let cc = self.b.ins().icmp_imm_u(IntCC::SignedGreaterThanOrEqual, a, 0); self.sett(cc); }
            K_CMPPL => { let a = rn!(); let cc = self.b.ins().icmp_imm_u(IntCC::SignedGreaterThan, a, 0); self.sett(cc); }
            K_EXTUB => { let a = rm!(); let x = self.b.ins().band_imm_u(a, 0xFF); self.setr(n, x); }
            K_EXTUW => { let a = rm!(); let x = self.b.ins().band_imm_u(a, 0xFFFF); self.setr(n, x); }
            K_EXTSB => { let a = rm!(); let x = self.b.ins().ireduce(types::I8, a); let x = self.b.ins().sextend(types::I32, x); self.setr(n, x); }
            K_EXTSW => { let a = rm!(); let x = self.b.ins().ireduce(types::I16, a); let x = self.b.ins().sextend(types::I32, x); self.setr(n, x); }
            K_SWAPB => {
                let a = rm!();
                let hi = self.b.ins().band_imm_u(a, 0xFFFF_0000u32 as i32 as i64);
                let lo = self.b.ins().ushr_imm_u(a, 8);
                let lo = self.b.ins().band_imm_u(lo, 0xFF);
                let mid = self.b.ins().ishl_imm_u(a, 8);
                let mid = self.b.ins().band_imm_u(mid, 0xFF00);
                let x = self.b.ins().bor(hi, lo);
                let x = self.b.ins().bor(x, mid);
                self.setr(n, x);
            }
            K_SWAPW => { let a = rm!(); let x = self.b.ins().rotl_imm_u(a, 16); self.setr(n, x); }
            K_XTRCT => { let (a, b) = (rn!(), rm!()); let lo = self.b.ins().ushr_imm_u(a, 16); let hi = self.b.ins().ishl_imm_u(b, 16); let x = self.b.ins().bor(lo, hi); self.setr(n, x); }
            K_SHLL | K_SHAL => { let a = rn!(); let t = self.b.ins().ushr_imm_u(a, 31); self.set(V_T, t); let x = self.b.ins().ishl_imm_u(a, 1); self.setr(n, x); }
            K_SHLR => { let a = rn!(); let t = self.b.ins().band_imm_u(a, 1); self.set(V_T, t); let x = self.b.ins().ushr_imm_u(a, 1); self.setr(n, x); }
            K_SHAR => { let a = rn!(); let t = self.b.ins().band_imm_u(a, 1); self.set(V_T, t); let x = self.b.ins().sshr_imm_u(a, 1); self.setr(n, x); }
            K_SHLL2 | K_SHLL8 | K_SHLL16 => { let s = match k { K_SHLL2 => 2, K_SHLL8 => 8, _ => 16 }; let a = rn!(); let x = self.b.ins().ishl_imm_u(a, s); self.setr(n, x); }
            K_SHLR2 | K_SHLR8 | K_SHLR16 => { let s = match k { K_SHLR2 => 2, K_SHLR8 => 8, _ => 16 }; let a = rn!(); let x = self.b.ins().ushr_imm_u(a, s); self.setr(n, x); }
            K_ROTL => { let a = rn!(); let t = self.b.ins().ushr_imm_u(a, 31); self.set(V_T, t); let x = self.b.ins().rotl_imm_u(a, 1); self.setr(n, x); }
            K_ROTR => { let a = rn!(); let t = self.b.ins().band_imm_u(a, 1); self.set(V_T, t); let x = self.b.ins().rotr_imm_u(a, 1); self.setr(n, x); }
            K_ROTCL => {
                let (a, t) = (rn!(), self.get(V_T));
                let c = self.b.ins().ushr_imm_u(a, 31);
                let x = self.b.ins().ishl_imm_u(a, 1);
                let x = self.b.ins().bor(x, t);
                self.setr(n, x);
                self.set(V_T, c);
            }
            K_ROTCR => {
                let (a, t) = (rn!(), self.get(V_T));
                let c = self.b.ins().band_imm_u(a, 1);
                let x = self.b.ins().ushr_imm_u(a, 1);
                let tt = self.b.ins().ishl_imm_u(t, 31);
                let x = self.b.ins().bor(x, tt);
                self.setr(n, x);
                self.set(V_T, c);
            }
            K_SHAD | K_SHLD => {
                let (a, s) = (rn!(), rm!());
                let s31 = self.b.ins().band_imm_u(s, 31);
                let left = self.b.ins().ishl(a, s31);
                let ns = self.b.ins().bnot(s);
                let ns = self.b.ins().band_imm_u(ns, 31);
                let amt = self.b.ins().iadd_imm_u(ns, 1);
                let (right, zero) = if k == K_SHAD {
                    (self.b.ins().sshr(a, amt), self.b.ins().sshr_imm_u(a, 31))
                } else {
                    (self.b.ins().ushr(a, amt), self.c(0))
                };
                let is0 = self.b.ins().icmp_imm_u(IntCC::Equal, s31, 0);
                let neg = self.b.ins().select(is0, zero, right);
                let pos = self.b.ins().icmp_imm_u(IntCC::SignedGreaterThanOrEqual, s, 0);
                let x = self.b.ins().select(pos, left, neg);
                self.setr(n, x);
            }
            K_DT => { let a = rn!(); let x = self.b.ins().iadd_imm_u(a, -1); self.setr(n, x); let cc = self.b.ins().icmp_imm_u(IntCC::Equal, x, 0); self.sett(cc); }
            K_MOVT => { let t = self.get(V_T); self.setr(n, t); }
            K_MULL => { let (a, b) = (rn!(), rm!()); let x = self.b.ins().imul(a, b); self.set(V_MACL, x); }
            K_MULU => { let (a, b) = (rn!(), rm!()); let a = self.b.ins().band_imm_u(a, 0xFFFF); let b2 = self.b.ins().band_imm_u(b, 0xFFFF); let x = self.b.ins().imul(a, b2); self.set(V_MACL, x); }
            K_MULS => {
                let (a, b) = (rn!(), rm!());
                let a = self.b.ins().ireduce(types::I16, a);
                let a = self.b.ins().sextend(types::I32, a);
                let b2 = self.b.ins().ireduce(types::I16, b);
                let b2 = self.b.ins().sextend(types::I32, b2);
                let x = self.b.ins().imul(a, b2);
                self.set(V_MACL, x);
            }
            K_DMULU | K_DMULS => {
                let (a, b) = (rn!(), rm!());
                let (a, b2) = if k == K_DMULU {
                    (self.b.ins().uextend(types::I64, a), self.b.ins().uextend(types::I64, b))
                } else {
                    (self.b.ins().sextend(types::I64, a), self.b.ins().sextend(types::I64, b))
                };
                let p = self.b.ins().imul(a, b2);
                let lo = self.b.ins().ireduce(types::I32, p);
                let hi = self.b.ins().ushr_imm_u(p, 32);
                let hi = self.b.ins().ireduce(types::I32, hi);
                self.set(V_MACH, hi);
                self.set(V_MACL, lo);
            }
            K_STSMACH => { let x = self.get(V_MACH); self.setr(n, x); }
            K_STSMACL => { let x = self.get(V_MACL); self.setr(n, x); }
            K_STSPR => { let x = self.get(V_PR); self.setr(n, x); }
            K_LDSMACH => { let x = rn!(); self.set(V_MACH, x); }
            K_LDSMACL => { let x = rn!(); self.set(V_MACL, x); }
            K_LDSPR => { let x = rn!(); self.set(V_PR, x); }
            K_STSLMACH | K_STSLMACL | K_STSLPR => {
                let slot = match k { K_STSLMACH => V_MACH, K_STSLMACL => V_MACL, _ => V_PR };
                let x = self.get(slot);
                let a = rn!();
                let a = self.b.ins().iadd_imm_u(a, -4);
                self.write(4, a, x);
                self.setr(n, a);
            }
            K_LDSLMACH | K_LDSLMACL | K_LDSLPR => {
                let slot = match k { K_LDSLMACH => V_MACH, K_LDSLMACL => V_MACL, _ => V_PR };
                let a = rn!();
                let x = self.read(4, a);
                let a2 = self.b.ins().iadd_imm_u(a, 4);
                self.setr(n, a2);
                self.set(slot, x);
            }
            K_CLRT => { let z = self.c(0); self.set(V_T, z); }
            K_SETT => { let o = self.c(1); self.set(V_T, o); }
            K_NOP => {}
            K_PREF => { let a = rn!(); self.call(H_PREF, &[a]); self.fault_check(); }
            K_MOVWPC => { let a = self.c(pc.wrapping_add(4).wrapping_add(d8 * 2)); let x = self.read(2, a); self.setr(n, x); }
            K_MOVLPC => { let a = self.c((pc.wrapping_add(4) & !3).wrapping_add(d8 * 4)); let x = self.read(4, a); self.setr(n, x); }
            K_MOVA => { let x = self.c((pc.wrapping_add(4) & !3).wrapping_add(d8 * 4)); self.setr(0, x); }
            _ => unreachable!("kind {k} isn't translated"),
        }
    }
}

impl Sh4 {
    /// With the MMU on: the block at the PC, translated when the TLB maps it to RAM, else
    /// the interpreter (`step_mmu`) to the end of a straight run. A block that faulted
    /// left the registers as before the instruction and its restart PC: the TLB exception
    /// is taken here.
    #[inline(never)]
    fn run_mmu_block<B: Sh4Bus>(&mut self, bus: &mut B, jit: &mut Jit, ctx: &mut Ctx, target: u64, cpi: u64) {
        let pc = self.pc;
        let md = self.sr_hi & SR_MD != 0;
        let pa = if pc & 1 == 0 && self.pending.is_none() { bus.jit_fetch_pa(pc, md) } else { None };
        let mode = Jit::mode(self);
        let b = match pa { Some(pa) => jit.block_mmu(bus, pc, pa, mode), None => None };
        match b {
            Some(Block { f: Some(f), .. }) => {
                bus.mmu_priv(md);
                jit.runs += 1;
                ctx.cpu = self as *mut Sh4;
                ctx.insns = 0;
                ctx.synced = 0;
                ctx.left = 0;
                ctx.exit = NO_SLOT;
                ctx.last_branch = 0;
                ctx.fault = 0;
                let to = jit.enter(ctx, f);
                self.pc = to;
                bus.tick(cpi * (ctx.insns - ctx.synced) as u64);
                if ctx.fault != 0 {
                    ctx.fault = 0;
                    // (a fault the helper saw is always still there; 040 if not, to be safe)
                    let code = bus.mmu_fault().unwrap_or(0x040);
                    self.tlb_exception_at(bus, to, code);
                }
            }
            Some(Block { f: None, branch: 1, .. }) => {
                // warming up: the interpreter, to the end of the straight run
                let mut n = 0;
                loop {
                    jit.cold += 1;
                    let p = self.pc;
                    self.step_mmu(bus);
                    bus.tick(cpi);
                    n += 1;
                    let straight = self.pc == p.wrapping_add(2) || self.pending.is_some();
                    if !straight || n >= MAX_INSNS || self.sleeping || bus.irq().is_some() || bus.now() >= target || !bus.mmu() { break; }
                }
            }
            _ => { jit.cold += 1; self.step_mmu(bus); bus.tick(cpi); }
        }
    }

    /// `run` with translated blocks where it can (see the module notes); the interpreter
    /// for everything else. Same exits and idle-loop detection as `run`.
    pub fn run_jit<B: Sh4Bus>(&mut self, bus: &mut B, jit: &mut Jit, target: u64, cpi: u64, mut loops: Option<&mut (u32, u32)>) -> super::RunExit {
        use super::RunExit;
        if self.pending.is_some() && bus.now() < target && !self.sleeping {
            self.step(bus);
            bus.tick(cpi);
        }
        if let Some((level, code)) = bus.irq() {
            if self.accepts(level) { self.interrupt(bus, code); }
        }
        if self.sleeping { return RunExit::Sleep; }
        let attn = bus.jit_attn();
        let attn = if attn.is_null() { &ATTN_SET as *const u8 } else { attn };
        let mut ctx = Ctx::new(self as *mut Sh4, bus as *mut B as *mut u8, attn);
        ctx.cpi = cpi as u32;
        jit.idle_watch = loops.is_some();
        // `Jit::mode`'s FPU-enabled bit: SR.FD changes only outside blocks (interpreted
        // instructions); the rest is FPSCR, read each time
        let inl = |c: &Sh4| Jit::mode(c) & 4;
        let mut fd = inl(self);
        // the link slot the last block left by, for the block that follows
        let mut slot = NO_SLOT;
        while bus.now() < target {
            let mut pc = self.pc;
            if bus.attention() {
                if let Some((level, code)) = bus.irq() {
                    if self.accepts(level) { self.interrupt(bus, code); pc = self.pc; }
                }
                if bus.mmu() {
                    if bus.jit_any_dirty() { jit.evict(bus); }
                    slot = NO_SLOT;
                    if self.sleeping { return RunExit::Sleep; }
                    self.run_mmu_block(bus, jit, &mut ctx, target, cpi);
                    continue;
                }
                jit.evict(bus);
            }
            // a jump to an odd address: the interpreter raises the address error
            if pc & 1 != 0 {
                self.step(bus);
                bus.tick(cpi);
                slot = NO_SLOT;
                fd = inl(self);
                continue;
            }
            // (from, to, check): `from` is the branch for the loop check
            let mode = (self.fpscr & (super::FPSCR_PR | super::FPSCR_SZ)) >> 19 | fd;
            let (from, to, check) = match jit.block(bus, pc, mode) {
                Some(Block { f: Some(f), .. }) => {
                    if slot != NO_SLOT { jit.link(slot, pc, mode, f); }
                    jit.runs += 1;
                    ctx.cpu = self as *mut Sh4;
                    ctx.insns = 0;
                    ctx.synced = 0;
                    ctx.left = ((target - bus.now()).div_ceil(cpi)).min(i32::MAX as u64) as i32;
                    ctx.exit = NO_SLOT;
                    ctx.last_branch = 0;
                    let to = jit.enter(&mut ctx, f);
                    self.pc = to;
                    bus.tick(cpi * (ctx.insns - ctx.synced) as u64);
                    slot = ctx.exit;
                    let branch = ctx.last_branch;
                    let check = branch != 0 && to <= branch;
                    (branch, to, check)
                }
                Some(Block { f: None, branch: 1, .. }) => {
                    // warming up: the interpreter, to the block's end
                    slot = NO_SLOT;
                    let mut p = pc;
                    let mut n = 0;
                    loop {
                        jit.cold += 1;
                        let (to, check) = self.step_fast_cold(bus, p, cpi);
                        n += 1;
                        if check || n >= MAX_INSNS || bus.attention() || bus.now() >= target { fd = inl(self); break (p, to, check); }
                        p = to;
                    }
                }
                _ => { slot = NO_SLOT; jit.cold += 1; let r = self.step_fast_cold(bus, pc, cpi); fd = inl(self); (pc, r.0, r.1) }
            };

            if !check { continue; }
            if to <= from {
                if self.sleeping {
                    match bus.irq() {
                        Some((level, code)) if self.accepts(level) => self.interrupt(bus, code),
                        _ => return RunExit::Sleep,
                    }
                } else if to < from && from - to <= 64 {
                    if let Some(l) = loops.as_deref_mut() {
                        if to == l.0 { l.1 += 1; } else { *l = (to, 1); }
                        if l.1 == 4 {
                            let k = KIND[bus.fetch16(from) as usize];
                            let last = if k == K_BT || k == K_BF { from } else { from.wrapping_add(2) };
                            return RunExit::Loop { to, last };
                        }
                    }
                }
            }
        }
        RunExit::Done
    }
}

#[cfg(test)]
#[path = "jit_tests.rs"]
mod tests;
