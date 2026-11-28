//! The VUs' JIT: runs of micro-mode instruction pairs translated to host code with
//! Cranelift, doing exactly what `Vu::step` does pair by pair (VU_JITCHECK=1 runs every
//! program on the interpreter too and reports any difference).
//!
//! - A block runs from its start to the delay slot of the first jump / unconditional
//!   branch, the pair after an E bit, or `MAX_PAIRS`. Conditional branches leave the block
//!   after their delay slot when taken (VU_JIT_SUPER=0: blocks end at every branch).
//! - All state stays in the `Vu`. Translated code does the per-pair bookkeeping (PC,
//!   stalls, cycle, delay slots, E bit, flag landing) and the common instructions itself:
//!   FMACs (on host floats when nothing leaves f32's range, else the exact f64 path),
//!   MAX / MINI, ITOF / FTOI, ABS, CLIP, loads / stores, integer ops, moves and branches.
//!   Anything else goes through the interpreter's handlers.
//! - Stall checks are left out where the block shows a field can't be pending.
//! - Blocks are found by start PC and code words (MPG rewrites programs; up to
//!   `VERSIONS` kept per address). They're translated on a thread per VU while the
//!   interpreter runs on; after `MAX_BLOCKS` translations it all starts over.
//! - Only with the PCSX2 stall rules (the default). PS2_VU_JIT=0: interpreter only;
//!   PS2_VU_JIT_SYNC=1: translate on the emulation thread.

use super::vu::{Dst, UpClass, Vu};
use super::vu::{ADD, BC, I, MADD, MSUB, MUL, Q, SUB};
use cranelift_codegen::ir::{self, condcodes::IntCC, types, AbiParam, InstBuilder, MemFlagsData, Value};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module};
use std::collections::HashMap;

/// What a block gets
#[repr(C)]
pub struct Ctx {
    pub vu: *mut Vu,
    pub data: *mut u8,
    pub data_len: usize,
    pub path1: *mut Vec<u128>,
    /// the constants table (loaded once per pair, so constants stay local to their pair)
    pub ktab: *const u8,
    /// a branch back inside a block leaves it once the cycle is past this (as `Vu::run`'s
    /// cycle limit)
    pub limit: u64,
}

impl Ctx {
    pub fn new(vu: *mut Vu, data: &mut [u8], path1: *mut Vec<u128>) -> Self {
        Ctx { vu, data: data.as_mut_ptr(), data_len: data.len(), path1, ktab: KT.0.as_ptr() as *const u8, limit: u64::MAX }
    }
}

type BlockFn = unsafe extern "C" fn(*mut Ctx);

const MAX_PAIRS: usize = 16;
fn max_pairs() -> usize { std::env::var("VU_JIT_PAIRS").ok().and_then(|v| v.parse().ok()).unwrap_or(MAX_PAIRS) }
/// Starts before a block is translated
const HOT: u32 = 2;
/// Translations kept per start address (programs MPG'd in and out)
const VERSIONS: usize = 8;
pub const NONE: u32 = super::vu::NO_BRANCH;

struct Block {
    f: BlockFn,
    pc: u32,
    /// micro memory byte address and the pairs as translated
    addr: usize,
    words: Box<[u64]>,
    /// the code_gen it was last found to match at
    seen: u64,
    hits: u64,
}

/// To the translator thread: a block to translate, or start over (dropping all code)
enum Msg { Tr { pc: u32, addr: usize, words: Box<[u64]>, epoch: u32 }, Reset }
/// A translation (valid while its epoch is current)
struct Done { pc: u32, addr: usize, words: Box<[u64]>, f: usize, ns: u64, epoch: u32 }
/// Translations before starting over (bounds the code kept: games keep loading programs)
const MAX_BLOCKS: u64 = 1500;
fn max_blocks() -> u64 {
    static N: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *N.get_or_init(|| std::env::var("VU_JIT_MAX_BLOCKS").ok().and_then(|v| v.parse().ok()).unwrap_or(MAX_BLOCKS))
}

/// Cranelift and what it has translated (code lives as long as this)
struct Translator {
    module: JITModule,
    fctx: FunctionBuilderContext,
    names: u64,
}

pub struct VuJit {
    /// per pair address: the translations starting there
    table: Vec<Vec<Block>>,
    cold: Vec<u32>,
    pending: Vec<bool>,
    /// translating here (PS2_VU_JIT_SYNC=1, and VU_JITCHECK) rather than on a thread
    local: Option<Translator>,
    /// this VU's translator thread (ends, freeing its code, when this is dropped)
    worker: Option<(std::sync::mpsc::Sender<Msg>, std::sync::mpsc::Receiver<Done>)>,
    epoch: u32,
    in_epoch: u64,
    /// always translate here (tests)
    sync: bool,
    /// requests sent, not yet back
    in_flight: usize,
    /// micro memory's write count at this get
    cgen: u64,
    /// per slot, the last block found: (pc, cgen, version, f): no search while
    /// micro memory is unwritten
    quick: Vec<(u32, u64, usize, BlockFn)>,
    pub translated: u64,
    pub stale: u64,
    pub runs: u64,
    /// debugging: the last block run
    pub last: Option<u32>,
    /// host time spent translating (ns)
    pub compile_ns: u64,
}

unsafe impl Send for VuJit {}

unsafe extern "C" fn nop_block(_: *mut Ctx) {}

/// Whether the JIT is on (PS2_VU_JIT=0: off)
pub fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("PS2_VU_JIT").map_or(true, |v| v != "0"))
}

// ------------------------------------------------------------------ helpers

unsafe fn parts<'a>(c: *mut Ctx) -> (&'a mut Vu, &'a mut [u8], &'a mut Vec<u128>) {
    unsafe {
        let c = &mut *c;
        (&mut *c.vu, std::slice::from_raw_parts_mut(c.data, c.data_len), &mut *c.path1)
    }
}

unsafe extern "C" fn h_land(c: *mut Ctx) {
    let (vu, _, _) = unsafe { parts(c) };
    vu.jit_land();
}
unsafe extern "C" fn h_kick(c: *mut Ctx) {
    let (vu, data, path1) = unsafe { parts(c) };
    vu.jit_kick(data, path1);
}
unsafe extern "C" fn h_upper(c: *mut Ctx, hi: u32, f: usize) {
    let (vu, _, _) = unsafe { parts(c) };
    let f: fn(&mut Vu, u32) -> Option<(Dst, [u32; 4], bool)> = unsafe { std::mem::transmute(f) };
    vu.jit_upper(hi, f);
}
unsafe extern "C" fn h_lower(c: *mut Ctx, lo: u32, f: usize, vit: u32) {
    let (vu, data, path1) = unsafe { parts(c) };
    let f: fn(&mut Vu, u32, &mut [u8], &mut Vec<u128>) = unsafe { std::mem::transmute(f) };
    vu.jit_lower(lo, f, vit, data, path1);
}
unsafe extern "C" fn h_bvi(c: *mut Ctx, r: u32) -> u32 {
    let (vu, _, _) = unsafe { parts(c) };
    vu.jit_bvi(r as usize)
}
unsafe extern "C" fn h_wait_q(c: *mut Ctx) {
    let (vu, _, _) = unsafe { parts(c) };
    vu.jit_wait_q();
}
unsafe extern "C" fn h_commit(c: *mut Ctx) {
    let (vu, _, _) = unsafe { parts(c) };
    vu.jit_commit();
}

/// Translated code does the common instructions itself (VU_JIT_INLINE=0: everything
/// through the interpreter's handlers)
fn inline_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("VU_JIT_INLINE").map_or(true, |v| v != "0"))
}

/// FMACs on host floats when the values allow (VU_JIT_FAST=0: always the exact path)
fn fast_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("VU_JIT_FAST").map_or(true, |v| v != "0"))
}

/// A lower instruction that branches or jumps (its next pair is a delay slot)
/// Which pairs' inline FMAC MAC / status pushes nobody can see: a later inline FMAC push
/// (pair j) replaces them, j lands inside the block (pair j + 4 exists; each pair moves
/// the cycle on at least one), and nothing from this pair to j + 3 reads or writes the
/// MAC / status flags (FS* / FM*, or an upper half run by a handler). Mid-block exits only come
/// after pair 0, so a push there is kept. The flags' sticky status bits are ORed in at
/// once instead (nothing reads status in the window, and landings keep bits 6-11).
fn dead_pushes(words: &[u64], targets: &[bool]) -> Vec<bool> {
    let n = words.len();
    let mut dead = vec![false; n];
    if !inline_on() || std::env::var_os("VU_JIT_ALL_FLAGS").is_some() { return dead; }
    let cls = |w: u64| super::vu::up_class((w >> 32) as u32);
    let pushes: Vec<bool> = words.iter().map(|&w| matches!(cls(w), UpClass::Fm(..))).collect();
    let touch: Vec<bool> = words.iter().map(|&w| {
        let (lo, hi) = (w as u32, (w >> 32) as u32);
        let flag_op = hi & (1 << 31) == 0 && (0x14..=0x1B).contains(&(lo >> 25));     // FS* / FM* (FC* only see CLIP)
        let up_helper = !matches!(cls(w), UpClass::Nop | UpClass::Fm(..) | UpClass::Mm(..) | UpClass::Itof(_)
            | UpClass::Ftoi(_) | UpClass::Abs | UpClass::Clip);
        flag_op || up_helper
    }).collect();
    // (a branch may leave the block after its delay slot: the flags must be right there)
    let exit: Vec<bool> = (0..n).map(|k| {
        let br = |w: u64| (w >> 32) as u32 & (1 << 31) == 0 && is_branch(w as u32);
        br(words[k]) || (k > 0 && br(words[k - 1]))
    }).collect();
    let touch: Vec<bool> = (0..n).map(|k| touch[k] || exit[k] || targets[k]).collect();
    for i in 1..n {
        if !pushes[i] { continue; }
        let Some(j) = (i + 1..n).find(|&j| pushes[j]) else { continue };
        if j + 4 < n && !touch[i..=j + 3].iter().any(|&t| t) { dead[i] = true; }
    }
    dead
}

/// Debugging: VU_JIT_PROF=file appends each block's code ranges by part (`Em::cat`)
fn prof_path() -> Option<&'static str> {
    static P: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    P.get_or_init(|| std::env::var("VU_JIT_PROF").ok()).as_deref()
}

/// Blocks run on through conditional branches (VU_JIT_SUPER=0: they end there)
fn super_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("VU_JIT_SUPER").map_or(true, |v| v != "0"))
}
/// In-block branches jump (VU_JIT_LOOP=0: they leave the block)
fn loop_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("VU_JIT_LOOP").map_or(true, |v| v != "0"))
}
fn is_cond_branch(lo: u32) -> bool { matches!(lo >> 25, 0x28 | 0x29 | 0x2C..=0x2F) }

fn is_branch(lo: u32) -> bool { matches!(lo >> 25, 0x20 | 0x21 | 0x24 | 0x25 | 0x28 | 0x29 | 0x2C..=0x2F) }

impl Default for VuJit {
    fn default() -> Self { Self::new() }
}

/// Translate on this thread (deterministic timing; tests)
fn sync_mode() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("PS2_VU_JIT_SYNC").map_or(false, |v| v != "0") || std::env::var("VU_JITCHECK").is_ok())
}

fn spawn() -> Option<(std::sync::mpsc::Sender<Msg>, std::sync::mpsc::Receiver<Done>)> {
    let (tx, rx) = std::sync::mpsc::channel::<Msg>();
    let (dtx, drx) = std::sync::mpsc::channel::<Done>();
    std::thread::Builder::new().name("vu-jit".into()).spawn(move || {
        let mut t = Translator::new();
        while let Ok(m) = rx.recv() {
            match m {
                Msg::Tr { pc, addr, words, epoch } => {
                    let t0 = std::time::Instant::now();
                    let f = t.emit(pc, &words) as usize;
                    let ns = t0.elapsed().as_nanos() as u64;
                    if dtx.send(Done { pc, addr, words, f, ns, epoch }).is_err() { break; }
                }
                Msg::Reset => t = Translator::new(),
            }
        }
    }).ok()?;
    Some((tx, drx))
}

impl VuJit {
    pub fn new() -> Self {
        Self { table: Vec::new(), cold: Vec::new(), pending: Vec::new(), local: None, worker: None, epoch: 0, in_epoch: 0, sync: false, in_flight: 0, cgen: 0, quick: Vec::new(),
               translated: 0, stale: 0, runs: 0, last: None, compile_ns: 0 }
    }

    /// One that translates on this thread
    pub fn new_sync() -> Self { Self { sync: true, ..Self::new() } }

    pub fn in_flight(&self) -> usize { self.in_flight }
    pub fn epoch(&self) -> u32 { self.epoch }

    /// The most-run blocks: (start pc, pairs, runs) (debugging)
    pub fn hot(&self, n: usize) -> Vec<(u32, usize, u64)> {
        let mut v: Vec<_> = self.table.iter().flatten().map(|b| (b.pc, b.words.len(), b.hits)).collect();
        v.sort_by_key(|x| std::cmp::Reverse(x.2));
        v.truncate(n);
        v
    }

    /// Wait for the translations asked for (tests)
    pub fn wait_idle(&mut self) {
        let n = self.table.len();
        while self.in_flight > 0 {
            let Some((_, rx)) = &self.worker else { break };
            let Ok(d) = rx.recv() else { break };
            self.in_flight -= 1;
            if d.epoch == self.epoch && n > 0 { self.add(d, n); }
        }
    }

    /// The block starting at `pc` once translated; None: interpret this pair. Blocks are
    /// asked for only at `leader`s (a program's start, a jump's target, the pair after a
    /// block), so a run of interpreted pairs doesn't queue a block at each one
    pub fn get(&mut self, code: &[u8], pc: u32, leader: bool, cgen: u64) -> Option<unsafe extern "C" fn(*mut Ctx)> {
        self.cgen = cgen;
        let n = code.len() / 8;
        if self.table.len() != n {
            self.table = (0..n).map(|_| Vec::new()).collect();
            self.cold = vec![0; n];
            self.pending = vec![false; n];
            self.quick = vec![(u32::MAX, u64::MAX, 0, nop_block as BlockFn); n];
        }
        let slot = (pc as usize / 8) & (n - 1);
        let q = self.quick[slot];
        if q.0 == pc && q.1 == cgen {
            self.table[slot][q.2].hits += 1;
            self.runs += 1;
            return Some(q.3);
        }
        if let Some(f) = self.find(code, slot, pc) { return Some(f); }
        // translations done meanwhile
        let mut got = false;
        let mut done = Vec::new();
        if self.in_flight > 0 {
            if let Some((_, rx)) = &self.worker { while let Ok(d) = rx.try_recv() { done.push(d); } }
            self.in_flight -= done.len().min(self.in_flight);
        }
        for d in done {
            if d.epoch != self.epoch { continue; }
            got |= (d.pc as usize / 8) & (n - 1) == slot;
            self.add(d, n);
        }
        if got { if let Some(f) = self.find(code, slot, pc) { return Some(f); } }
        if !leader || self.pending[slot] { return None; }
        self.cold[slot] += 1;
        if self.cold[slot] < HOT { return None; }
        self.cold[slot] = 0;
        if !self.table[slot].is_empty() { self.stale += 1; }
        if self.in_epoch >= max_blocks() { self.reset(); }
        let (addr, words) = Self::collect(code, pc);
        self.translated += 1;
        self.in_epoch += 1;
        if self.sync || sync_mode() {
            let t0 = std::time::Instant::now();
            let t = self.local.get_or_insert_with(Translator::new);
            let f = t.emit(pc, &words) as usize;
            let ns = t0.elapsed().as_nanos() as u64;
            self.add(Done { pc, addr, words, f, ns, epoch: self.epoch }, n);
            return self.find(code, slot, pc);
        }
        if self.worker.is_none() { self.worker = spawn(); }
        let Some((tx, _)) = &self.worker else { return None };
        if tx.send(Msg::Tr { pc, addr, words, epoch: self.epoch }).is_ok() { self.pending[slot] = true; self.in_flight += 1; }
        None
    }

    /// Drop every translation (their code is freed)
    fn reset(&mut self) {
        for v in &mut self.table { v.clear(); }
        for q in &mut self.quick { q.1 = u64::MAX; }
        self.pending.iter_mut().for_each(|p| *p = false);
        self.epoch = self.epoch.wrapping_add(1);
        self.in_epoch = 0;
        if self.local.is_some() { self.local = Some(Translator::new()); }
        if let Some((tx, _)) = &self.worker { let _ = tx.send(Msg::Reset); }
    }

    fn find(&mut self, code: &[u8], slot: usize, pc: u32) -> Option<BlockFn> {
        let mask = code.len() - 1;
        let word = |a: usize| u64::from_le_bytes(code[a..a + 8].try_into().unwrap());
        // (compared again only after micro memory was written)
        let cgen = self.cgen;
        for (i, b) in self.table[slot].iter_mut().enumerate() {
            if b.pc != pc { continue; }
            if b.seen == cgen || b.words.iter().enumerate().all(|(k, &w)| word((b.addr + 8 * k) & mask) == w) {
                b.seen = cgen;
                b.hits += 1;
                self.runs += 1;
                self.quick[slot] = (pc, cgen, i, b.f);
                return Some(b.f);
            }
        }
        None
    }

    fn add(&mut self, d: Done, n: usize) {
        let slot = (d.pc as usize / 8) & (n - 1);
        self.compile_ns += d.ns;
        self.pending[slot] = false;
        self.quick[slot].1 = u64::MAX;
        let v = &mut self.table[slot];
        if v.len() >= VERSIONS { v.remove(0); }
        // SAFETY: `f` is a finished translation that lives as long as its translator
        v.push(Block { f: unsafe { std::mem::transmute::<usize, BlockFn>(d.f) }, pc: d.pc, addr: d.addr, words: d.words, seen: u64::MAX, hits: 0 });
    }

    /// The pairs of the block at `pc`
    fn collect(code: &[u8], pc: u32) -> (usize, Box<[u64]>) {
        let mask = code.len() - 1;
        let addr = pc as usize & mask & !7;
        let mut words = Vec::new();
        loop {
            let a = (addr + 8 * words.len()) & mask;
            let w = u64::from_le_bytes(code[a..a + 8].try_into().unwrap());
            words.push(w);
            let (lo, hi) = (w as u32, (w >> 32) as u32);
            let lower = hi & (1 << 31) == 0;
            let ebit = hi & (1 << 30) != 0;
            let ends = (lower && is_branch(lo)) || ebit;
            if ends {
                let a = (addr + 8 * words.len()) & mask;
                let slot = u64::from_le_bytes(code[a..a + 8].try_into().unwrap());
                let slot_branch = (slot >> 32) as u32 & (1 << 31) == 0 && is_branch(slot as u32);
                let slot_ebit = (slot >> 32) as u32 & (1 << 30) != 0;
                // a conditional branch: on past its delay slot (left when taken)
                if super_on() && !ebit && is_cond_branch(lo) && !slot_branch && !slot_ebit && words.len() + 2 < max_pairs() {
                    words.push(slot);
                    continue;
                }
                // its delay slot / the pair after the E bit comes too
                let a = (addr + 8 * words.len()) & mask;
                words.push(u64::from_le_bytes(code[a..a + 8].try_into().unwrap()));
                break;
            }
            if words.len() >= max_pairs() { break; }
        }
        (addr, words.into_boxed_slice())
    }
}

impl Translator {
    fn new() -> Self {
        let opt = std::env::var("VU_JIT_OPT").unwrap_or_else(|_| "speed".into());
        let b = JITBuilder::with_flags(&[("opt_level", opt.as_str()), ("enable_alias_analysis", "false"),
            ("enable_verifier", "false"), ("preserve_frame_pointers", "true")],
            cranelift_module::default_libcall_names()).expect("host ISA");
        Translator { module: JITModule::new(b), fctx: FunctionBuilderContext::new(), names: 0 }
    }

    fn emit(&mut self, pc: u32, words: &[u64]) -> BlockFn {
        self.names += 1;
        let name = format!("vu{}", self.names);
        let mut fctx = std::mem::replace(&mut self.fctx, FunctionBuilderContext::new());
        let module = &mut self.module;
        let tcfg = module.target_config();
        let ptr = tcfg.pointer_type();
        let cc = module.isa().default_call_conv();
        let mut ctx = module.make_context();
        ctx.func.signature = ir::Signature::new(cc);
        ctx.func.signature.params.push(AbiParam::new(ptr));
        let id = module.declare_function(&name, Linkage::Local, &ctx.func.signature).expect("declare block");
        {
            let mut b = FunctionBuilder::new(&mut ctx.func, &mut fctx);
            let entry = b.create_block();
            b.append_block_params_for_function_params(entry);
            b.switch_to_block(entry);
            let c = b.block_params(entry)[0];
            let fl = MemFlagsData::trusted();
            let vu = b.ins().load(ptr, fl, c, std::mem::offset_of!(Ctx, vu) as i32);
            let data = b.ins().load(ptr, fl, c, std::mem::offset_of!(Ctx, data) as i32);
            let len = b.ins().load(ptr, fl, c, std::mem::offset_of!(Ctx, data_len) as i32);
            let len = b.ins().ireduce(types::I32, len);
            let qn = b.ins().ushr_imm_u(len, 4);
            let qmask = b.ins().iadd_imm_s(qn, -1);
            let mut e = Em { b: &mut b, ptr, fl, ctx: c, vu, cc, sigs: HashMap::new(), data, qmask, o: Vu::jit_offsets(), kb: c,
                             vwrites: Vec::new(), helper_lower: false, dead_push: false };
            e.pairs(pc, words);
            b.ins().return_(&[]);
            b.seal_all_blocks();
            b.finalize(tcfg);
        }
        let ir_n = ctx.func.dfg.num_insts();
        module.define_function(id, &mut ctx).expect("translate block");
        if std::env::var("VU_JIT_SIZE").is_ok() {
            let sz = ctx.compiled_code().map_or(0, |c| c.code_info().total_size);
            println!("block {pc:04X}: {} pairs, {ir_n} IR, {sz} bytes", words.len());
        }
        if std::env::var("VU_JIT_DUMP").map_or(false, |v| v == format!("{pc:X}")) {
            if let Some(c) = ctx.compiled_code() { std::fs::write("vu_block.bin", c.code_buffer()).ok(); }
        }
        let prof: Option<Vec<(u32, u32, u32)>> = prof_path().map(|_| {
            let base = ctx.func.params.base_srcloc();
            ctx.compiled_code().map_or(Vec::new(), |c| c.buffer.get_srclocs_sorted().iter()
                .map(|m| (m.start, m.end, if m.loc.is_rel() { m.loc.relocate(base).bits() } else { m.loc.as_abs().bits() }))
                .chain(std::iter::once((0, c.code_info().total_size, 1000))).collect())
        });
        module.clear_context(&mut ctx);
        module.finalize_definitions().expect("finalize block");
        let code = module.get_finalized_function(id);
        if let (Some(path), Some(v)) = (prof_path(), prof) {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                let a = code as usize;
                let mut t = String::new();
                for (s0, e0, c) in v { t += &format!("{:x} {:x} {c} {pc:x}\n", a + s0 as usize, a + e0 as usize); }
                let _ = f.write_all(t.as_bytes());
            }
        }
        if std::env::var("VU_JIT_DUMP").map_or(false, |v| v == format!("{pc:X}")) { println!("block {pc:04X} at {code:p}"); }
        self.fctx = fctx;
        unsafe { std::mem::transmute::<*const u8, BlockFn>(code) }
    }
}

struct Em<'a, 'b> {
    b: &'a mut FunctionBuilder<'b>,
    ptr: ir::Type,
    fl: MemFlagsData,
    ctx: Value,
    vu: Value,
    cc: cranelift_codegen::isa::CallConv,
    sigs: HashMap<(usize, bool), ir::SigRef>,
    data: Value,
    qmask: Value,
    o: super::vu::JitOffsets,
    kb: Value,
    /// this pair's VF writes (register, fields); whether its lower half went to a handler
    vwrites: Vec<(usize, u32)>,
    helper_lower: bool,
    /// this pair's MAC / status push is never seen (see `dead_pushes`): only its sticky
    /// status bits are kept
    dead_push: bool,
}

/// Vector constants, loaded where used (shared values would stay live across the block
/// and spill)
const K32: [u32; 18] = [0, 0x7F80_0000, 0x7F00_0000, 0x7FF, 0x7FFF_FFFF, 0x7F_FFFF, 0x8000_0000, 1, 2, 255, 896, 897, 254, u32::MAX,
    0x3F80_0000, 0x4180_0000, 0x4580_0000, 0x4700_0000];       // (f32 1, 16, 4096, 32768)
const K64: [u64; 8] = [0xFFFF_FFFF_E000_0000, 896 << 52, 0x4330_0000_0000_0000, 0x4330_0000_8000_0000,
    0x3FF0_0000_0000_0000, 0x3FB0_0000_0000_0000, 0x3F30_0000_0000_0000, 0x3F00_0000_0000_0000];  // (f64 2^52+2^31, 1, 1/16, 1/4096, 1/32768)
#[repr(C, align(16))]
struct KTab([[u32; 4]; K32.len() + K64.len() + 16]);
static KT: KTab = {
    let mut t = [[0u32; 4]; K32.len() + K64.len() + 16];
    let mut i = 0;
    while i < K32.len() { t[i] = [K32[i]; 4]; i += 1; }
    let mut j = 0;
    while j < K64.len() { let v = K64[j]; t[i + j] = [v as u32, (v >> 32) as u32, v as u32, (v >> 32) as u32]; j += 1; }
    let mut d = 0;
    while d < 16 {
        let mut k = 0;
        while k < 4 { t[i + j + d][k] = if d & (8 >> k) != 0 { u32::MAX } else { 0 }; k += 1; }
        d += 1;
    }
    KTab(t)
};

/// An upper result waiting for its commit (after the lower half)
enum Up { None, Helper, Fm { res: Value, acc: bool, fd: usize, dest: u32 } }

fn le() -> MemFlagsData { MemFlagsData::new().with_endianness(ir::Endianness::Little) }
/// VU fields aren't 16-byte aligned
fn vfl() -> MemFlagsData { MemFlagsData::new().with_notrap() }

impl Em<'_, '_> {
    fn c32(&mut self, v: u32) -> Value { self.b.ins().iconst(types::I32, v as i32 as i64) }

    /// Call a helper taking the context and `args` (32-bit, or pointer-sized when `wide`
    /// is set for the second), returning a u32 when `ret`
    fn call(&mut self, f: usize, args: &[Value], ret: bool) -> Option<Value> {
        let key = (args.len(), ret);
        let sig = match self.sigs.get(&key) {
            Some(&s) => s,
            None => {
                let mut s = ir::Signature::new(self.cc);
                s.params.push(AbiParam::new(self.ptr));
                for v in args { s.params.push(AbiParam::new(self.b.func.dfg.value_type(*v))); }
                if ret { s.returns.push(AbiParam::new(types::I32)); }
                let s = self.b.import_signature(s);
                self.sigs.insert(key, s);
                s
            }
        };
        let fv = self.b.ins().iconst(self.ptr, f as i64);
        let mut all = vec![self.ctx];
        all.extend_from_slice(args);
        let call = self.b.ins().call_indirect(sig, fv, &all);
        if ret { Some(self.b.inst_results(call)[0]) } else { None }
    }

    /// Debugging (VU_JIT_PROF): the part of the pair the next instructions belong to
    fn cat(&mut self, c: u32) { if prof_path().is_some() { self.b.set_srcloc(ir::SourceLoc::new(c)); } }
    fn ld(&mut self, ty: ir::Type, off: usize) -> Value { self.b.ins().load(ty, self.fl, self.vu, off as i32) }
    fn st(&mut self, v: Value, off: usize) { self.b.ins().store(self.fl, v, self.vu, off as i32); }

    fn pairs(&mut self, start: u32, words: &[u64]) {
        let o = Vu::jit_offsets();
        let mut take_next = true;          // the first pair may be a delay slot
        let mut ending_next = true;        // the first pair may follow an E bit
        // the last pair in this block that wrote each VF field; the last that called a
        // handler that may write VF
        let mut written: HashMap<(usize, usize), usize> = HashMap::new();
        let mut barrier: Option<usize> = None;
        // conditional branches to a pair of this block: jump there rather than leave
        // (delay slot index -> target index)
        let n = words.len();
        let mut jumps: HashMap<usize, usize> = HashMap::new();
        let mut labels: HashMap<usize, ir::Block> = HashMap::new();
        if super_on() && loop_on() {
            for k in 1..n.saturating_sub(1) {
                let (lo, hi) = (words[k - 1] as u32, (words[k - 1] >> 32) as u32);
                if hi & (1 << 31) != 0 || !is_cond_branch(lo) { continue; }
                let imm11 = ((lo as i32) << 21 >> 21) as u32;
                let t = start.wrapping_add(8 * k as u32).wrapping_add(imm11.wrapping_mul(8));
                if let Some(ti) = (0..n).find(|&i| start.wrapping_add(8 * i as u32) == t) {
                    jumps.insert(k, ti);
                    labels.entry(ti).or_insert_with(|| self.b.create_block());
                }
            }
        }
        let targets: Vec<bool> = (0..n).map(|i| labels.contains_key(&i)).collect();
        let mut since_target: Option<usize> = None;
        let dead = dead_pushes(words, &targets);
        if std::env::var_os("VU_JIT_DEAD").is_some() {
            let pushes = words.iter().filter(|&&w| matches!(super::vu::up_class((w >> 32) as u32), UpClass::Fm(..))).count();
            println!("block {start:04X}: {} pairs, {pushes} FMAC pushes, {} dead", words.len(), dead.iter().filter(|&&d| d).count());
            if std::env::var("VU_JIT_DEAD").is_ok_and(|v| v == format!("{start:X}")) {
                for (k, &w) in words.iter().enumerate() {
                    let (lo, hi) = (w as u32, (w >> 32) as u32);
                    println!("  {:04X} {hi:08X} {lo:08X} up {:?} lo-key {:02X} dead {}", start + 8 * k as u32, super::vu::up_class(hi), lo >> 25, dead[k]);
                }
            }
        }
        for (i, &w) in words.iter().enumerate() {
            if let Some(&l) = labels.get(&i) {
                self.b.ins().jump(l, &[]);
                self.b.switch_to_block(l);
                since_target = Some(i);
            }
            self.vwrites.clear();
            self.dead_push = dead[i];
            let (lo, hi) = (w as u32, (w >> 32) as u32);
            let lower = hi & (1 << 31) == 0;
            let a = start.wrapping_add(8 * i as u32);
            self.cat(2);
            // a branch in the pair before: this pair is its delay slot
            let delay = if take_next {
                let t = self.ld(types::I32, o.branch);
                let none = self.c32(NONE);
                self.st(none, o.branch);
                Some(t)
            } else { None };
            take_next = lower && is_branch(lo);
            self.kb = self.b.ins().load(self.ptr, self.fl, self.ctx, std::mem::offset_of!(Ctx, ktab) as i32);
            let v = self.c32(a.wrapping_add(8));
            self.st(v, o.pc);
            // stalls: the fields this pair reads that an FMAC / lower write hasn't finished.
            // Results land 4 cycles after their pair and the cycle moves on at least one a
            // pair, so only fields written in the last 3 pairs can stall: before the block
            // (pairs 0-2), or by this block's pairs (known), or by a handler's (not known)
            self.cat(3);
            let reads = Vu::jit_reads(lo, hi);
            let near = |w: usize| i - w <= 3;
            let mut fields = Vec::new();
            for (r, m) in reads {
                for f in 0..4 {
                    if m & (8 >> f) == 0 { continue; }
                    // (entered by a branch: what ran before isn't known)
                    let check = i < 3 || since_target.is_some_and(|t| i - t < 3) || barrier.is_some_and(near) || written.get(&(r, f)).is_some_and(|&w| near(w));
                    if check { fields.push(r * 4 + f); }
                }
            }
            // (read by a conditional branch in this pair)
            let cond_branch = lower && matches!(lo >> 25, 0x28 | 0x29 | 0x2C..=0x2F);
            if fields.is_empty() {
                if cond_branch { let f = self.b.ins().iconst(types::I8, 0); self.st(f, o.br_stalled); }
            } else {
                let cyc = self.ld(types::I64, o.cycle);
                let mut ready = self.b.ins().iconst(types::I64, 0);
                for k in fields {
                    let t = self.ld(types::I64, o.vf_ready + k * 8);
                    ready = self.b.ins().umax(ready, t);
                }
                if cond_branch {
                    let stalled = self.b.ins().icmp(IntCC::UnsignedGreaterThan, ready, cyc);
                    self.st(stalled, o.br_stalled);
                }
                let c2 = self.b.ins().umax(cyc, ready);
                self.st(c2, o.cycle);
            }
            // WAITQ stalls the whole pair (the upper half reads the new Q)
            if lower && super::vu::is_waitq(lo) { self.call(h_wait_q as *const () as usize, &[], false); }
            // flags / Q / P due
            self.cat(4);
            let cyc = self.ld(types::I64, o.cycle);
            let due = self.ld(types::I64, o.qp_due);
            let d = self.b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, cyc, due);
            if inline_on() { self.if_then(d, |e| e.land()); }
            else { self.if_then(d, |e| { e.call(h_land as *const () as usize, &[], false); }); }
            // PATH1 transfer in progress
            self.cat(5);
            let k = self.ld(types::I8, o.kick_on);
            self.if_then(k, |e| { e.call(h_kick as *const () as usize, &[], false); });
            // the upper half (its result is committed after the lower half)
            self.cat(if matches!(super::vu::up_class(hi), UpClass::Fm(..)) { 6 } else { 8 });
            let up = match super::vu::up_class(hi) {
                UpClass::Nop => Up::None,
                UpClass::Fm(k, src, acc) if inline_on() => self.fm(hi, k, src, acc),
                UpClass::Mm(max, src) if inline_on() => self.minmax(hi, max, src),
                UpClass::Itof(sh) if inline_on() => self.itof(hi, sh),
                UpClass::Ftoi(sh) if inline_on() => self.ftoi(hi, sh),
                UpClass::Abs if inline_on() => {
                    let s = self.ldv(self.o.vf + (hi >> 11 & 31) as usize * 16);
                    let c = self.sp32(0x7FFF_FFFF);
                    let res = self.b.ins().band(s, c);
                    Up::Fm { res, acc: false, fd: (hi >> 16 & 31) as usize, dest: hi >> 21 & 15 }
                }
                UpClass::Clip if inline_on() => { self.clip(hi); Up::None }
                _ => {
                    let hv = self.c32(hi);
                    let uf = self.b.ins().iconst(self.ptr, Vu::jit_upper_fn(hi) as i64);
                    self.call(h_upper as *const () as usize, &[hv, uf], false);
                    Up::Helper
                }
            };
            // the lower half
            self.cat(9);
            if !lower {
                let v = self.c32(lo);
                self.st(v, o.i);
                self.hist_shift();
                let z = self.c32(0);
                self.st(z, o.vi_hist);
            }
            self.helper_lower = lower && !(inline_on() && self.lower_inline(lo, a));
            if self.helper_lower {
                self.cat(10);
                let lv = self.c32(lo);
                let lf = self.b.ins().iconst(self.ptr, Vu::jit_lower_fn(lo) as i64);
                let vit = self.c32(Vu::jit_vit(lo));
                self.call(h_lower as *const () as usize, &[lv, lf, vit], false);
            }
            self.cat(11);
            match up {
                Up::None => {}
                Up::Helper => { self.call(h_commit as *const () as usize, &[], false); barrier = Some(i); }
                Up::Fm { res, acc, fd, dest } => self.commit(res, acc, fd, dest),
            }
            if self.helper_lower { barrier = Some(i); }
            for &(r, dest) in &self.vwrites {
                for f in 0..4 { if dest & (8 >> f) != 0 { written.insert((r, f), i); } }
            }
            self.cat(12);
            let cyc = self.ld(types::I64, o.cycle);
            let cyc = self.b.ins().iadd_imm_s(cyc, 1);
            self.st(cyc, o.cycle);
            if let Some(t) = delay {
                let none = self.c32(NONE);
                let taken = self.b.ins().icmp(IntCC::NotEqual, t, none);
                // the branch's destination; a block entered at a delay slot stops there
                let more = i + 1 < words.len();
                let ending_here = hi & (1 << 30) != 0;
                let jump = jumps.get(&i).map(|&ti| (ti, labels[&ti]));
                self.if_then(taken, |e| {
                    e.st(t, o.pc);
                    if let Some((ti, l)) = jump {
                        // (no E bit in the branch's pair or here: nothing to count down)
                        if ti <= i {
                            let cyc = e.ld(types::I64, o.cycle);
                            let lim = e.b.ins().load(types::I64, e.fl, e.ctx, std::mem::offset_of!(Ctx, limit) as i32);
                            let over = e.b.ins().icmp(IntCC::UnsignedGreaterThan, cyc, lim);
                            let (out, go) = (e.b.create_block(), e.b.create_block());
                            e.b.ins().brif(over, out, &[], go, &[]);
                            e.b.switch_to_block(out);
                            e.b.ins().return_(&[]);
                            e.b.switch_to_block(go);
                        }
                        e.b.ins().jump(l, &[]);
                        let dead = e.b.create_block();
                        e.b.switch_to_block(dead);
                    } else if more {
                        // (the E bit / ending bookkeeping this pair would have done)
                        if ending_next { e.end_count(o, None); }
                        if ending_here { let one = e.b.ins().iconst(types::I8, 1); e.st(one, o.ending); }
                        e.b.ins().return_(&[]);
                        let dead = e.b.create_block();
                        e.b.switch_to_block(dead);
                    }
                });
            }
            // (a block entered just after an E bit: the program may stop after this pair)
            if ending_next {
                let exit = (i + 1 < words.len()).then_some(hi & (1 << 30) != 0);
                self.end_count(o, exit);
            }
            ending_next = hi & (1 << 30) != 0;
            if ending_next {
                let one = self.b.ins().iconst(types::I8, 1);
                self.st(one, o.ending);
            }
        }
    }

    /// An E bit two pairs back: count down, stop the program at 0
    /// `exit`: more pairs follow, so leave the block when the program stops (and whether
    /// this pair has an E bit itself)
    fn end_count(&mut self, o: super::vu::JitOffsets, exit: Option<bool>) {
        let en = self.ld(types::I8, o.ending);
        self.if_then(en, |e| {
            let en = e.ld(types::I8, o.ending);
            let en = e.b.ins().iadd_imm_s(en, -1);
            e.st(en, o.ending);
            let zero = e.b.ins().icmp_imm_s(IntCC::Equal, en, 0);
            e.if_then(zero, |e| {
                let f = e.b.ins().iconst(types::I8, 0);
                e.st(f, o.running);
                if let Some(ebit) = exit {
                    if ebit { let one = e.b.ins().iconst(types::I8, 1); e.st(one, o.ending); }
                    e.b.ins().return_(&[]);
                    let dead = e.b.create_block();
                    e.b.switch_to_block(dead);
                }
            });
        });
    }

    // ------------------------------------------------------------ inline pieces

    fn kload(&mut self, i: usize, ty: ir::Type) -> Value {
        let kb = self.kb;
        self.b.ins().load(ty, MemFlagsData::new().with_notrap().with_aligned().with_readonly().with_can_move(), kb, (i * 16) as i32)
    }
    fn sp32(&mut self, v: u32) -> Value {
        let i = K32.iter().position(|&k| k == v).expect("VU JIT constant");
        self.kload(i, types::I32X4)
    }
    fn sp64(&mut self, v: u64) -> Value {
        let i = K64.iter().position(|&k| k == v).expect("VU JIT constant");
        self.kload(K32.len() + i, types::I64X2)
    }
    fn spf64(&mut self, v: f64) -> Value {
        let i = K64.iter().position(|&k| k == v.to_bits()).expect("VU JIT constant");
        self.kload(K32.len() + i, types::F64X2)
    }
    fn spf32(&mut self, v: f32) -> Value {
        let i = K32.iter().position(|&k| k == v.to_bits()).expect("VU JIT constant");
        self.kload(i, types::F32X4)
    }
    fn cast(&mut self, ty: ir::Type, v: Value) -> Value { self.b.ins().bitcast(ty, le(), v) }
    fn shuf(&mut self, a: Value, b: Value, m: [u8; 16]) -> Value {
        let a = self.cast(types::I8X16, a);
        let b = self.cast(types::I8X16, b);
        let imm = self.b.func.dfg.immediates.push(ir::ConstantData::from(m.to_vec()));
        let r = self.b.ins().shuffle(a, b, imm);
        self.cast(types::I32X4, r)
    }
    /// lanes -1 where `dest` has the field (x = lane 0)
    fn dmask(&mut self, dest: u32) -> Value { self.kload(K32.len() + K64.len() + dest as usize, types::I32X4) }
    fn ldv(&mut self, off: usize) -> Value { self.b.ins().load(types::I32X4, vfl(), self.vu, off as i32) }
    fn stv(&mut self, v: Value, off: usize) { self.b.ins().store(vfl(), v, self.vu, off as i32); }

    /// 4 PS2 floats → two f64 pairs (as `vu_simd::widen`)
    fn widen(&mut self, x: Value) -> (Value, Value) {
        let em = self.sp32(0x7F80_0000);
        let e = self.b.ins().band(x, em);
        let zero = self.sp32(0);
        let ez = self.b.ins().icmp(IntCC::Equal, e, zero);
        let mm = self.sp32(0x7FFF_FFFF);
        let mag = self.b.ins().band(x, mm);
        let mag = self.b.ins().band_not(mag, ez);
        let sm = self.sp32(0x8000_0000);
        let sign = self.b.ins().band(x, sm);
        let nz = self.b.ins().bnot(ez);
        let bias = self.sp64(896 << 52);
        let half = |e: &mut Self, hi: bool| {
            let (m, s, n) = if hi {
                (e.b.ins().uwiden_high(mag), e.b.ins().uwiden_high(sign), e.b.ins().swiden_high(nz))
            } else {
                (e.b.ins().uwiden_low(mag), e.b.ins().uwiden_low(sign), e.b.ins().swiden_low(nz))
            };
            let m = e.b.ins().ishl_imm_u(m, 29);
            let bb = e.b.ins().band(n, bias);
            let s = e.b.ins().ishl_imm_u(s, 32);
            let r = e.b.ins().iadd(m, bb);
            let r = e.b.ins().bor(r, s);
            e.cast(types::F64X2, r)
        };
        let lo = half(self, false);
        let hi = half(self, true);
        (lo, hi)
    }

    /// Two f64 pairs → 4 PS2 floats and per-field U=1 / O=2 (as `vu_simd::narrow`)
    fn narrow(&mut self, lo: Value, hi: Value) -> (Value, Value) {
        let l = self.cast(types::I64X2, lo);
        let h = self.cast(types::I64X2, hi);
        let hw = self.shuf(l, h, [4, 5, 6, 7, 12, 13, 14, 15, 20, 21, 22, 23, 28, 29, 30, 31]);
        let ml = self.b.ins().ushr_imm_u(l, 29);
        let mh = self.b.ins().ushr_imm_u(h, 29);
        let mw = self.shuf(ml, mh, [0, 1, 2, 3, 8, 9, 10, 11, 16, 17, 18, 19, 24, 25, 26, 27]);
        let c = self.sp32(0x7F_FFFF);
        let mant = self.b.ins().band(mw, c);
        let c = self.sp32(0x8000_0000);
        let sign = self.b.ins().band(hw, c);
        let ex = self.b.ins().ushr_imm_u(hw, 20);
        let c = self.sp32(0x7FF);
        let ex = self.b.ins().band(ex, c);
        let c = self.sp32(896);
        let e = self.b.ins().isub(ex, c);
        let z = self.sp32(0);
        let zero = self.b.ins().icmp(IntCC::Equal, ex, z);
        let one = self.sp32(1);
        let lt = self.b.ins().icmp(IntCC::SignedLessThan, e, one);
        let under = self.b.ins().band_not(lt, zero);
        let c = self.sp32(255);
        let over = self.b.ins().icmp(IntCC::SignedGreaterThan, e, c);
        let es = self.b.ins().ishl_imm_u(e, 23);
        let normal = self.b.ins().bor(es, mant);
        let small = self.b.ins().bor(zero, under);
        let v = self.b.ins().band_not(normal, small);
        let v = self.b.ins().band_not(v, over);
        let c = self.sp32(0x7FFF_FFFF);
        let ov = self.b.ins().band(over, c);
        let v = self.b.ins().bor(v, ov);
        let v = self.b.ins().bor(v, sign);
        let u = self.b.ins().band(under, one);
        let two = self.sp32(2);
        let o = self.b.ins().band(over, two);
        let fl = self.b.ins().bor(u, o);
        (v, fl)
    }

    /// The FMAC as `vu_simd::fmac4` (any inputs): result, U / O per field
    fn fmac_exact(&mut self, k: u8, s: Value, bv: Value, av: Option<Value>) -> (Value, Value) {
        let va = self.widen(s);
        let vb = self.widen(bv);
        match k {
            ADD | SUB | MUL => { let r = self.op2(k, va, vb); self.narrow(r.0, r.1) }
            _ => {
                let m = self.op2(MUL, va, vb);
                let (mv, f1) = self.narrow(m.0, m.1);
                let wa = self.widen(av.unwrap());
                let wm = self.widen(mv);
                let r = self.op2(if k == MADD { ADD } else { SUB }, wa, wm);
                let (v, f2) = self.narrow(r.0, r.1);
                (v, self.b.ins().bor(f1, f2))
            }
        }
    }

    /// exponent 0 → ±0 (as `to_f64`); whether any field's exponent is 255 (past f32)
    fn sane(&mut self, x: Value) -> (Value, Value) {
        let em = self.sp32(0x7F80_0000);
        let e = self.b.ins().band(x, em);
        let zero = self.sp32(0);
        let ez = self.b.ins().icmp(IntCC::Equal, e, zero);
        let big = self.b.ins().icmp(IntCC::Equal, e, em);
        let mm = self.sp32(0x7FFF_FFFF);
        let clr = self.b.ins().band(ez, mm);
        (self.b.ins().band_not(x, clr), big)
    }
    /// host f32 lanes → two f64 pairs (exact)
    fn promote(&mut self, x: Value) -> (Value, Value) {
        let f = self.cast(types::F32X4, x);
        let l = self.b.ins().fvpromote_low(f);
        let h = self.shuf(x, x, [8, 9, 10, 11, 12, 13, 14, 15, 8, 9, 10, 11, 12, 13, 14, 15]);
        let h = self.cast(types::F32X4, h);
        let h = self.b.ins().fvpromote_low(h);
        (l, h)
    }
    /// Round toward zero to 24 bits; whether every field is 0 or in f32's normal range
    fn trunc(&mut self, r: (Value, Value)) -> ((Value, Value), Value) {
        let m = self.sp64(0xFFFF_FFFF_E000_0000);
        let t = |e: &mut Self, v: Value| {
            let i = e.cast(types::I64X2, v);
            let i = e.b.ins().band(i, m);
            e.cast(types::F64X2, i)
        };
        let (l, h) = (t(self, r.0), t(self, r.1));
        let li = self.cast(types::I64X2, l);
        let hi = self.cast(types::I64X2, h);
        let hw = self.shuf(li, hi, [4, 5, 6, 7, 12, 13, 14, 15, 20, 21, 22, 23, 28, 29, 30, 31]);
        let ex = self.b.ins().ushr_imm_u(hw, 20);
        let c = self.sp32(0x7FF);
        let ex = self.b.ins().band(ex, c);
        let zero = self.sp32(0);
        let z = self.b.ins().icmp(IntCC::Equal, ex, zero);
        let c = self.sp32(897);
        let e2 = self.b.ins().isub(ex, c);
        let c = self.sp32(u32::MAX);
        let ge = self.b.ins().icmp(IntCC::SignedGreaterThan, e2, c);
        let c = self.sp32(254);
        let lt = self.b.ins().icmp(IntCC::SignedLessThan, e2, c);
        let ok = self.b.ins().band(ge, lt);
        let ok = self.b.ins().bor(ok, z);
        ((l, h), self.b.ins().vall_true(ok))
    }
    /// two f64 pairs (already exact f32 values) → 4 f32 lanes
    fn demote(&mut self, r: (Value, Value)) -> Value {
        let l = self.b.ins().fvdemote(r.0);
        let h = self.b.ins().fvdemote(r.1);
        self.shuf(l, h, [0, 1, 2, 3, 4, 5, 6, 7, 16, 17, 18, 19, 20, 21, 22, 23])
    }
    /// The FMAC: host floats when nothing leaves f32's range (no flags then), else
    /// `fmac_exact`
    fn fmac_fast(&mut self, k: u8, s: Value, bv: Value, av: Option<Value>) -> (Value, Value) {
        let (fast, slow, join) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
        self.b.set_cold_block(slow);
        self.b.append_block_param(join, types::I32X4);
        self.b.append_block_param(join, types::I32X4);
        let (sa, b1) = self.sane(s);
        let (sb, b2) = self.sane(bv);
        let mut big = self.b.ins().bor(b1, b2);
        let sc = av.map(|a| { let (x, b3) = self.sane(a); big = self.b.ins().bor(big, b3); x });
        let any = self.b.ins().vany_true(big);
        self.b.ins().brif(any, slow, &[], fast, &[]);
        self.b.switch_to_block(fast);
        let pa = self.promote(sa);
        let pb = self.promote(sb);
        let r = match k {
            ADD | SUB | MUL => self.op2(k, pa, pb),
            _ => {
                let m = self.op2(MUL, pa, pb);
                let (m, ok) = self.trunc(m);
                let next = self.b.create_block();
                self.b.ins().brif(ok, next, &[], slow, &[]);
                self.b.switch_to_block(next);
                let pc = self.promote(sc.unwrap());
                self.op2(if k == MADD { ADD } else { SUB }, pc, m)
            }
        };
        let (r, ok) = self.trunc(r);
        let done = self.b.create_block();
        self.b.ins().brif(ok, done, &[], slow, &[]);
        self.b.switch_to_block(done);
        let v = self.demote(r);
        let z = self.sp32(0);
        self.b.ins().jump(join, &[ir::BlockArg::Value(v), ir::BlockArg::Value(z)]);
        self.b.switch_to_block(slow);
        let (v, f) = self.fmac_exact(k, s, bv, av);
        self.b.ins().jump(join, &[ir::BlockArg::Value(v), ir::BlockArg::Value(f)]);
        self.b.switch_to_block(join);
        let p = self.b.block_params(join);
        (p[0], p[1])
    }

    fn op2(&mut self, k: u8, a: (Value, Value), b: (Value, Value)) -> (Value, Value) {
        match k {
            ADD => (self.b.ins().fadd(a.0, b.0), self.b.ins().fadd(a.1, b.1)),
            SUB => (self.b.ins().fsub(a.0, b.0), self.b.ins().fsub(a.1, b.1)),
            _ => (self.b.ins().fmul(a.0, b.0), self.b.ins().fmul(a.1, b.1)),
        }
    }

    /// An FMAC (`up_fm::<K, SRC, ACC>`): the result, its flags queued
    fn fm(&mut self, op: u32, k: u8, src: u8, acc: bool) -> Up {
        let o = self.o;
        let dest = op >> 21 & 15;
        let ft = (op >> 16 & 31) as usize;
        let fs = (op >> 11 & 31) as usize;
        let fd = (op >> 6 & 31) as usize;
        let s = self.ldv(o.vf + fs * 16);
        let bv = match src {
            BC => { let x = self.ld(types::I32, o.vf + ft * 16 + (op & 3) as usize * 4); self.b.ins().splat(types::I32X4, x) }
            Q => { let x = self.ld(types::I32, o.q); self.b.ins().splat(types::I32X4, x) }
            I => { let x = self.ld(types::I32, o.i); self.b.ins().splat(types::I32X4, x) }
            _ => self.ldv(o.vf + ft * 16),
        };
        let av = if matches!(k, MADD | MSUB) { Some(self.ldv(o.acc)) } else { None };
        let (res, fl) = if fast_on() { self.fmac_fast(k, s, bv, av) } else { self.fmac_exact(k, s, bv, av) };
        let _ = MSUB;
        // MAC: Z S U O groups, x in bit 3 of each
        self.cat(7);
        const REV: [u8; 16] = [12, 13, 14, 15, 8, 9, 10, 11, 4, 5, 6, 7, 0, 1, 2, 3];
        let rv = self.shuf(res, res, REV);
        let rf = self.shuf(fl, fl, REV);
        let c = self.sp32(0x7FFF_FFFF);
        let a = self.b.ins().band(rv, c);
        let z0 = self.sp32(0);
        let zm = self.b.ins().icmp(IntCC::Equal, a, z0);
        let z = self.b.ins().vhigh_bits(types::I32, zm);
        let sg = self.b.ins().vhigh_bits(types::I32, rv);
        let u = self.b.ins().ishl_imm_u(rf, 31);
        let u = self.b.ins().vhigh_bits(types::I32, u);
        let ov = self.b.ins().ishl_imm_u(rf, 30);
        let ov = self.b.ins().vhigh_bits(types::I32, ov);
        let sg = self.b.ins().ishl_imm_u(sg, 4);
        let u = self.b.ins().ishl_imm_u(u, 8);
        let ov = self.b.ins().ishl_imm_u(ov, 12);
        let mac = self.b.ins().bor(z, sg);
        let mac = self.b.ins().bor(mac, u);
        let mac = self.b.ins().bor(mac, ov);
        let m4 = dest & 15;
        let mac = self.b.ins().band_imm_u(mac, (m4 | m4 << 4 | m4 << 8 | m4 << 12) as i64);
        // status: whether each group has a bit (bits 0 / 4 / 8 / 12 gathered by a multiply)
        let t = self.b.ins().ushr_imm_u(mac, 1);
        let t = self.b.ins().bor(mac, t);
        let t2 = self.b.ins().ushr_imm_u(t, 2);
        let t = self.b.ins().bor(t, t2);
        let t = self.b.ins().band_imm_u(t, 0x1111);
        let t = self.b.ins().imul_imm_u(t, 0x1248);
        let t = self.b.ins().ushr_imm_u(t, 12);
        let stat = self.b.ins().band_imm_u(t, 15);
        if self.dead_push {
            // nobody sees this MAC / status before a later push replaces them: only the
            // sticky bits stay (they'd have landed before anything could read them)
            let st = self.ld(types::I32, o.status);
            let s6 = self.b.ins().ishl_imm_u(stat, 6);
            let st = self.b.ins().bor(st, s6);
            self.st(st, o.status);
        } else {
            let z = self.c32(0);
            self.push_flags(mac, stat, z, z);
        }
        Up::Fm { res, acc, fd, dest }
    }

    /// Land Q / P when due (with the flags due first)
    fn land(&mut self) {
        self.land_fl();
        let o = self.o;
        let qp = self.ld(types::I64, o.qp_due);
        let cyc = self.ld(types::I64, o.cycle);
        let d = self.b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, cyc, qp);
        self.if_then(d, |e| { e.call(h_land as *const () as usize, &[], false); });
    }

    /// Land the MAC / status / CLIP results due (as `land_flags`). Translated code does
    /// this only before reading them (handlers land for themselves): the same values as
    /// landing at every pair, as the cycle only grows
    fn land_fl(&mut self) {
        let o = self.o;
        let (head_b, pop_b, clip_b, macb, done_b) = (self.b.create_block(), self.b.create_block(), self.b.create_block(), self.b.create_block(), self.b.create_block());
        self.b.ins().jump(head_b, &[]);
        self.b.switch_to_block(head_b);
        let len = self.ld(types::I32, o.fl_len);
        let head = self.ld(types::I32, o.fl_head);
        let idx = self.b.ins().imul_imm_u(head, 24);
        let idx = self.b.ins().uextend(self.ptr, idx);
        let ea = self.b.ins().iadd(self.vu, idx);
        let fl0 = self.fl;
        let base = o.flags as i32;
        // (an empty ring: the slot is stale, len decides)
        let at = self.b.ins().load(types::I64, fl0, ea, base);
        let cyc = self.ld(types::I64, o.cycle);
        let due = self.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, at, cyc);
        let some = self.b.ins().icmp_imm_s(IntCC::NotEqual, len, 0);
        let go = self.b.ins().band(due, some);
        self.b.ins().brif(go, pop_b, &[], done_b, &[]);
        self.b.switch_to_block(pop_b);
        let h1 = self.b.ins().iadd_imm_s(head, 1);
        let h1 = self.b.ins().band_imm_u(h1, 15);
        self.st(h1, o.fl_head);
        let l1 = self.b.ins().iadd_imm_s(len, -1);
        self.st(l1, o.fl_len);
        let isc = self.b.ins().load(types::I32, fl0, ea, base + 20);
        self.b.ins().brif(isc, clip_b, &[], macb, &[]);
        self.b.switch_to_block(clip_b);
        let c = self.b.ins().load(types::I32, fl0, ea, base + 16);
        let old = self.ld(types::I32, o.clip);
        let old = self.b.ins().ishl_imm_u(old, 6);
        let n = self.b.ins().bor(old, c);
        let n = self.b.ins().band_imm_u(n, 0xFF_FFFF);
        self.st(n, o.clip);
        self.b.ins().jump(head_b, &[]);
        self.b.switch_to_block(macb);
        let m = self.b.ins().load(types::I32, fl0, ea, base + 8);
        self.st(m, o.mac);
        let stt = self.b.ins().load(types::I32, fl0, ea, base + 12);
        let st = self.ld(types::I32, o.status);
        let st = self.b.ins().band_imm_u(st, !0xFi64 & 0xFFFF_FFFF);
        let s6 = self.b.ins().ishl_imm_u(stt, 6);
        let st = self.b.ins().bor(st, stt);
        let st = self.b.ins().bor(st, s6);
        self.st(st, o.status);
        self.b.ins().jump(head_b, &[]);
        self.b.switch_to_block(done_b);
    }

    /// PS2 floats as sortable integers (exponent 0 = ±0 = 0, sign-magnitude)
    fn key(&mut self, x: Value) -> Value {
        let em = self.sp32(0x7F80_0000);
        let e = self.b.ins().band(x, em);
        let zero = self.sp32(0);
        let ez = self.b.ins().icmp(IntCC::Equal, e, zero);
        let mm = self.sp32(0x7FFF_FFFF);
        let mag = self.b.ins().band(x, mm);
        let mag = self.b.ins().band_not(mag, ez);
        let neg = self.b.ins().ineg(mag);
        let sg = self.b.ins().sshr_imm_u(x, 31);
        self.b.ins().bitselect(sg, neg, mag)
    }
    fn minmax(&mut self, op: u32, max: bool, src: u8) -> Up {
        let o = self.o;
        let ft = (op >> 16 & 31) as usize;
        let s = self.ldv(o.vf + (op >> 11 & 31) as usize * 16);
        let b = match src {
            BC => { let x = self.ld(types::I32, o.vf + ft * 16 + (op & 3) as usize * 4); self.b.ins().splat(types::I32X4, x) }
            I => { let x = self.ld(types::I32, o.i); self.b.ins().splat(types::I32X4, x) }
            _ => self.ldv(o.vf + ft * 16),
        };
        let (ka, kb) = (self.key(s), self.key(b));
        let pick_a = self.b.ins().icmp(if max { IntCC::SignedGreaterThanOrEqual } else { IntCC::SignedLessThanOrEqual }, ka, kb);
        let res = self.b.ins().bitselect(pick_a, s, b);
        Up::Fm { res, acc: false, fd: (op >> 6 & 31) as usize, dest: op >> 21 & 15 }
    }
    fn itof(&mut self, op: u32, sh: u32) -> Up {
        let o = self.o;
        let s = self.ldv(o.vf + (op >> 11 & 31) as usize * 16);
        // i32 → f64 exactly: 2^52 + 2^31 + x as a double's bits, less 2^52 + 2^31
        let c = self.sp32(0x8000_0000);
        let u = self.b.ins().bxor(s, c);
        let magic = self.sp64(0x4330_0000_0000_0000);
        let off = self.spf64(4503601774854144.0);
        let cv = |e: &mut Self, hi: bool| {
            let w = if hi { e.b.ins().uwiden_high(u) } else { e.b.ins().uwiden_low(u) };
            let w = e.b.ins().bor(w, magic);
            let f = e.cast(types::F64X2, w);
            e.b.ins().fsub(f, off)
        };
        let l = cv(self, false);
        let h = cv(self, true);
        let scale = [1.0f64, 1.0 / 16.0, 1.0 / 4096.0, 1.0 / 32768.0][sh as usize];
        let c = self.spf64(scale);
        let l = self.b.ins().fmul(l, c);
        let h = self.b.ins().fmul(h, c);
        let (res, _) = self.narrow(l, h);
        Up::Fm { res, acc: false, fd: (op >> 16 & 31) as usize, dest: op >> 21 & 15 }
    }
    fn ftoi(&mut self, op: u32, sh: u32) -> Up {
        let o = self.o;
        let x = self.ldv(o.vf + (op >> 11 & 31) as usize * 16);
        // as host f32: exponent 0 → ±0; exponent 255 (past f32's range) → ±2^127 (saturates too)
        let em = self.sp32(0x7F80_0000);
        let e = self.b.ins().band(x, em);
        let zero = self.sp32(0);
        let ez = self.b.ins().icmp(IntCC::Equal, e, zero);
        let emax = self.b.ins().icmp(IntCC::Equal, e, em);
        let sm = self.sp32(0x8000_0000);
        let sign = self.b.ins().band(x, sm);
        let v = self.b.ins().bitselect(ez, sign, x);
        let big = self.sp32(0x7F00_0000);
        let big = self.b.ins().bor(big, sign);
        let v = self.b.ins().bitselect(emax, big, v);
        let f = self.cast(types::F32X4, v);
        let scale = [1.0f32, 16.0, 4096.0, 32768.0][sh as usize];
        let c = self.spf32(scale);
        let f = self.b.ins().fmul(f, c);
        let res = self.b.ins().fcvt_to_sint_sat(types::I32X4, f);
        Up::Fm { res, acc: false, fd: (op >> 16 & 31) as usize, dest: op >> 21 & 15 }
    }
    /// CLIP: queue the judgement of fs.xyz against |ft.w|
    fn clip(&mut self, op: u32) {
        let o = self.o;
        let s = self.ldv(o.vf + (op >> 11 & 31) as usize * 16);
        let w = self.ld(types::I32, o.vf + (op >> 16 & 31) as usize * 16 + 12);
        let w = self.b.ins().band_imm_u(w, 0x7FFF_FFFF);
        let w = self.b.ins().splat(types::I32X4, w);
        let kv = self.key(s);
        let kw = self.key(w);
        let gt = self.b.ins().icmp(IntCC::SignedGreaterThan, kv, kw);
        let nkw = self.b.ins().ineg(kw);
        let lt = self.b.ins().icmp(IntCC::SignedLessThan, kv, nkw);
        let g = self.b.ins().vhigh_bits(types::I32, gt);
        let l = self.b.ins().vhigh_bits(types::I32, lt);
        let mut c = self.c32(0);
        for k in 0..3 {
            let gb = self.b.ins().band_imm_u(g, 1 << k);
            let gb = self.b.ins().ishl_imm_u(gb, k as i64);           // bit k → 2k
            let lb = self.b.ins().band_imm_u(l, 1 << k);
            let lb = self.b.ins().ishl_imm_u(lb, k as i64 + 1);       // bit k → 2k+1
            c = self.b.ins().bor(c, gb);
            c = self.b.ins().bor(c, lb);
        }
        let z = self.c32(0);
        let one = self.c32(1);
        self.push_flags(z, z, c, one);
    }

    /// Queue a Flags (as FlagQ::push_back), landing at cycle + 4
    fn push_flags(&mut self, mac: Value, stat: Value, clip: Value, is_clip: Value) {
        let o = self.o;
        let cyc = self.ld(types::I64, o.cycle);
        let at = self.b.ins().iadd_imm_s(cyc, 4);
        // a full ring: its oldest is long due (16 pairs back), land it
        let len = self.ld(types::I32, o.fl_len);
        let full = self.b.ins().icmp_imm_s(IntCC::Equal, len, 16);
        let (fold, go) = (self.b.create_block(), self.b.create_block());
        self.b.set_cold_block(fold);
        self.b.ins().brif(full, fold, &[], go, &[]);
        self.b.switch_to_block(fold);
        self.land_fl();
        self.b.ins().jump(go, &[]);
        self.b.switch_to_block(go);
        let head = self.ld(types::I32, o.fl_head);
        let len = self.ld(types::I32, o.fl_len);
        let idx = self.b.ins().iadd(head, len);
        let idx = self.b.ins().band_imm_u(idx, 15);
        let idx = self.b.ins().imul_imm_u(idx, 24);
        let idx = self.b.ins().uextend(self.ptr, idx);
        let ea = self.b.ins().iadd(self.vu, idx);
        let fl0 = self.fl;
        let base = o.flags as i32;
        self.b.ins().store(fl0, at, ea, base);
        self.b.ins().store(fl0, mac, ea, base + 8);
        self.b.ins().store(fl0, stat, ea, base + 12);
        self.b.ins().store(fl0, clip, ea, base + 16);
        self.b.ins().store(fl0, is_clip, ea, base + 20);
        let len = self.b.ins().iadd_imm_s(len, 1);
        self.st(len, o.fl_len);
    }

    /// Write an FMAC result (as `commit`)
    fn commit(&mut self, res: Value, acc: bool, fd: usize, dest: u32) {
        let o = self.o;
        let cyc = self.ld(types::I64, o.cycle);
        let at = self.b.ins().iadd_imm_s(cyc, 4);
        if acc {
            self.merge(o.acc, dest, res);
            self.st(at, o.acc_ready);
        } else if fd != 0 {
            self.vwrites.push((fd, dest));
            self.merge(o.vf + fd * 16, dest, res);
            for k in 0..4 { if dest & (8 >> k) != 0 { self.st(at, o.vf_ready + (fd * 4 + k) * 8); } }
        }
    }
    /// The `dest` fields of the vector at `off` ← `v`
    fn merge(&mut self, off: usize, dest: u32, v: Value) {
        if dest == 0 { return; }
        if dest == 15 { self.stv(v, off); return; }
        let cur = self.ldv(off);
        let m = self.dmask(dest);
        let n = self.b.ins().bitselect(m, v, cur);
        self.stv(n, off);
    }
    /// A lower VF write (as `write_vf`)
    fn write_vf(&mut self, r: usize, dest: u32, v: Value) {
        if r == 0 || dest == 0 { return; }
        let o = self.o;
        self.vwrites.push((r, dest));
        self.merge(o.vf + r * 16, dest, v);
        let cyc = self.ld(types::I64, o.cycle);
        let at = self.b.ins().iadd_imm_s(cyc, 4);
        for k in 0..4 { if dest & (8 >> k) != 0 { self.st(at, o.vf_ready + (r * 4 + k) * 8); } }
    }

    fn vi(&mut self, r: usize) -> Value {
        if r == 0 { return self.c32(0); }
        let o = self.o;
        self.b.ins().uload16(types::I32, self.fl, self.vu, (o.vi + r * 2) as i32)
    }
    fn set_vi(&mut self, r: usize, v: Value) {
        if r == 0 { return; }
        let o = self.o;
        self.b.ins().istore16(self.fl, v, self.vu, (o.vi + r * 2) as i32);
    }
    /// data memory address of quadword `qa` (masked)
    fn daddr(&mut self, qa: Value) -> Value {
        let a = self.b.ins().band(qa, self.qmask);
        let a = self.b.ins().uextend(self.ptr, a);
        let a = self.b.ins().ishl_imm_u(a, 4);
        self.b.ins().iadd(self.data, a)
    }
    fn dload(&mut self, qa: Value) -> Value {
        let p = self.daddr(qa);
        self.b.ins().load(types::I32X4, vfl(), p, 0)
    }
    fn dstore(&mut self, qa: Value, dest: u32, v: Value) {
        if dest == 0 { return; }
        let p = self.daddr(qa);
        if dest == 15 { self.b.ins().store(vfl(), v, p, 0); return; }
        let cur = self.b.ins().load(types::I32X4, vfl(), p, 0);
        let m = self.dmask(dest);
        let n = self.b.ins().bitselect(m, v, cur);
        self.b.ins().store(vfl(), n, p, 0);
    }

    /// vi_hist: (h3, h2, h1) = (h2, h1, h0)
    fn hist_shift(&mut self) {
        let o = self.o;
        let h2 = self.ld(types::I32, o.vi_hist + 8);
        self.st(h2, o.vi_hist + 12);
        let h1 = self.ld(types::I32, o.vi_hist + 4);
        self.st(h1, o.vi_hist + 8);
        let h0 = self.ld(types::I32, o.vi_hist);
        self.st(h0, o.vi_hist + 4);
    }

    /// The common lower instructions (pair at `a`); false: not one of them
    fn lower_inline(&mut self, op: u32, a: u32) -> bool {
        let key = op >> 25;
        let sf = op & 0x3F;
        let sn = (op >> 6 & 31) << 2 | (op & 3);
        let ok = match key {
            0x00 | 0x01 | 0x04 | 0x05 | 0x08 | 0x09 | 0x20 | 0x21 | 0x24 | 0x25 | 0x28 | 0x29 | 0x2C..=0x2F => true,
            0x10 | 0x12 | 0x13 | 0x14 | 0x16 | 0x17 | 0x18 | 0x1A | 0x1B | 0x1C => true,
            0x40 => matches!(sf, 0x30 | 0x31 | 0x32 | 0x34 | 0x35)
                || (sf & 0x3C == 0x3C && matches!(sn, 0x30 | 0x31 | 0x34 | 0x35 | 0x36 | 0x37 | 0x3C | 0x3D)),
            _ => false,
        };
        if !ok { return false; }
        let o = self.o;
        let dest = op >> 21 & 15;
        let it = (op >> 16 & 15) as usize;
        let is = (op >> 11 & 15) as usize;
        let id = (op >> 6 & 15) as usize;
        let ft = (op >> 16 & 31) as usize;
        let fs = (op >> 11 & 31) as usize;
        let imm11 = ((op as i32) << 21 >> 21) as u32;
        let fsf = (op >> 21 & 3) as usize;
        let pc = a.wrapping_add(8);                     // self.pc while the lower half runs
        self.hist_shift();
        let vt = super::vu::lower_vi_target(op);
        let before = if vt != 0 { Some(self.vi(vt)) } else { None };
        let first = (0..4).find(|k| dest & (8 >> k) != 0).unwrap_or(0);
        match key {
            0x00 => { let b = self.vi(is); let q = self.b.ins().iadd_imm_s(b, imm11 as i32 as i64); let v = self.dload(q); self.write_vf(ft, dest, v); }
            0x01 => { let b = self.vi(it); let q = self.b.ins().iadd_imm_s(b, imm11 as i32 as i64); let v = self.ldv(o.vf + fs * 16); self.dstore(q, dest, v); }
            0x04 => {
                let b = self.vi(is);
                let q = self.b.ins().iadd_imm_s(b, imm11 as i32 as i64);
                let p = self.daddr(q);
                let v = self.b.ins().load(types::I32, vfl(), p, (first * 4) as i32);
                self.set_vi(it, v);
            }
            0x05 => {
                let b = self.vi(is);
                let q = self.b.ins().iadd_imm_s(b, imm11 as i32 as i64);
                let v = self.vi(it);
                let v = self.b.ins().splat(types::I32X4, v);
                self.dstore(q, dest, v);
            }
            // flag reads (FCEQ FCAND FCOR / FSEQ FSAND FSOR / FMEQ FMAND FMOR / FCGET)
            0x10 | 0x12 | 0x13 => {
                self.land_fl();
                let c = self.ld(types::I32, o.clip);
                let m = op & 0xFF_FFFF;
                let v = match key {
                    0x10 => self.b.ins().icmp_imm_u(IntCC::Equal, c, m as i64),
                    0x12 => { let x = self.b.ins().band_imm_u(c, m as i64); self.b.ins().icmp_imm_u(IntCC::NotEqual, x, 0) }
                    _ => { let x = self.b.ins().bor_imm_u(c, m as i64); let x = self.b.ins().band_imm_u(x, 0xFF_FFFF); self.b.ins().icmp_imm_u(IntCC::Equal, x, 0xFF_FFFF) }
                };
                let v = self.b.ins().uextend(types::I32, v);
                self.set_vi(1, v);
            }
            0x14 | 0x16 | 0x17 => {
                self.land_fl();
                let s = self.ld(types::I32, o.status);
                let s = self.b.ins().band_imm_u(s, 0xFFF);
                let imm = ((op >> 10 & 0x800) | (op & 0x7FF)) as i64;
                let v = match key {
                    0x14 => { let x = self.b.ins().icmp_imm_u(IntCC::Equal, s, imm); self.b.ins().uextend(types::I32, x) }
                    0x16 => self.b.ins().band_imm_u(s, imm),
                    _ => self.b.ins().bor_imm_u(s, imm),
                };
                self.set_vi(it, v);
            }
            0x18 | 0x1A | 0x1B => {
                self.land_fl();
                let m = self.ld(types::I32, o.mac);
                let x = self.vi(is);
                let v = match key {
                    0x18 => { let c = self.b.ins().icmp(IntCC::Equal, m, x); self.b.ins().uextend(types::I32, c) }
                    0x1A => self.b.ins().band(m, x),
                    _ => self.b.ins().bor(m, x),
                };
                self.set_vi(it, v);
            }
            0x1C => {
                self.land_fl();
                let c = self.ld(types::I32, o.clip);
                let v = self.b.ins().band_imm_u(c, 0xFFF);
                self.set_vi(it, v);
            }
            0x08 | 0x09 => {
                let imm = (op >> 10 & 0x7800) | (op & 0x7FF);
                let b = self.vi(is);
                let v = if key == 0x08 { self.b.ins().iadd_imm_s(b, imm as i64) } else { self.b.ins().iadd_imm_s(b, -(imm as i64)) };
                self.set_vi(it, v);
            }
            0x20 => { let t = self.c32(pc.wrapping_add(imm11.wrapping_mul(8))); self.st(t, o.branch); }
            0x21 => {
                let l = self.c32(pc.wrapping_add(8) / 8);
                self.set_vi(it, l);
                let t = self.c32(pc.wrapping_add(imm11.wrapping_mul(8)));
                self.st(t, o.branch);
            }
            0x24 | 0x25 => {
                let v = self.vi(is);
                let t = self.b.ins().ishl_imm_u(v, 3);
                if key == 0x25 { let l = self.c32(pc.wrapping_add(8) / 8); self.set_vi(it, l); }
                self.st(t, o.branch);
            }
            0x28 | 0x29 | 0x2C..=0x2F => {
                let bvi = |e: &mut Self, r: usize| -> Value {
                    if r == 0 { return e.c32(0); }
                    if !crate::ps2::vu::branch_vi_on() { return e.vi(r); }
                    // `bvi`'s fast case inline: no stall and the last pair didn't write r
                    let (slow, join) = (e.b.create_block(), e.b.create_block());
                    e.b.set_cold_block(slow);
                    e.b.append_block_param(join, types::I32);
                    let st = e.ld(types::I8, o.br_stalled);
                    let h0 = e.ld(types::I32, o.vi_hist);
                    let m = e.b.ins().band_imm_u(h0, 0x810F_0000u32 as i64);
                    let hit = e.b.ins().icmp_imm_u(IntCC::Equal, m, (0x8000_0000u32 | (r as u32) << 16) as i64);
                    let free = e.b.ins().icmp_imm_u(IntCC::Equal, st, 0);
                    let go = e.b.ins().band(hit, free);
                    let fast = e.vi(r);
                    e.b.ins().brif(go, slow, &[], join, &[ir::BlockArg::Value(fast)]);
                    e.b.switch_to_block(slow);
                    let rv = e.c32(r as u32);
                    let v = e.call(h_bvi as *const () as usize, &[rv], true).unwrap();
                    e.b.ins().jump(join, &[ir::BlockArg::Value(v)]);
                    e.b.switch_to_block(join);
                    e.b.block_params(join)[0]
                };
                let cond = match key {
                    0x28 | 0x29 => {
                        let x = bvi(self, it);
                        let y = bvi(self, is);
                        self.b.ins().icmp(if key == 0x28 { IntCC::Equal } else { IntCC::NotEqual }, x, y)
                    }
                    _ => {
                        let x = bvi(self, is);
                        let x = self.b.ins().ishl_imm_u(x, 16);
                        let cc = match key { 0x2C => IntCC::SignedLessThan, 0x2D => IntCC::SignedGreaterThan,
                                             0x2E => IntCC::SignedLessThanOrEqual, _ => IntCC::SignedGreaterThanOrEqual };
                        self.b.ins().icmp_imm_s(cc, x, 0)
                    }
                };
                let t = self.c32(pc.wrapping_add(imm11.wrapping_mul(8)));
                let none = self.c32(NONE);
                let v = self.b.ins().select(cond, t, none);
                self.st(v, o.branch);
            }
            _ => match sf {
                0x30 | 0x31 | 0x34 | 0x35 => {
                    let x = self.vi(is);
                    let y = self.vi(it);
                    let v = match sf { 0x30 => self.b.ins().iadd(x, y), 0x31 => self.b.ins().isub(x, y),
                                       0x34 => self.b.ins().band(x, y), _ => self.b.ins().bor(x, y) };
                    self.set_vi(id, v);
                }
                0x32 => {
                    let imm = ((op >> 6 & 31) as i32) << 27 >> 27;
                    let x = self.vi(is);
                    let v = self.b.ins().iadd_imm_s(x, imm as i64);
                    self.set_vi(it, v);
                }
                _ => match sn {
                    0x30 => { let v = self.ldv(o.vf + fs * 16); self.write_vf(ft, dest, v); }
                    0x31 => { let v = self.ldv(o.vf + fs * 16); let r = self.shuf(v, v, [4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 0, 1, 2, 3]); self.write_vf(ft, dest, r); }
                    0x34 => {
                        let b = self.vi(is);
                        let v = self.dload(b);
                        self.write_vf(ft, dest, v);
                        if is != 0 { let n = self.b.ins().iadd_imm_s(b, 1); self.set_vi(is, n); }
                    }
                    0x35 => {
                        let b = self.vi(it);
                        let v = self.ldv(o.vf + fs * 16);
                        self.dstore(b, dest, v);
                        if it != 0 { let n = self.b.ins().iadd_imm_s(b, 1); self.set_vi(it, n); }
                    }
                    0x36 => {
                        if is != 0 { let b = self.vi(is); let n = self.b.ins().iadd_imm_s(b, -1); self.set_vi(is, n); }
                        let b = self.vi(is);
                        let v = self.dload(b);
                        self.write_vf(ft, dest, v);
                    }
                    0x37 => {
                        if it != 0 { let b = self.vi(it); let n = self.b.ins().iadd_imm_s(b, -1); self.set_vi(it, n); }
                        let b = self.vi(it);
                        let v = self.ldv(o.vf + fs * 16);
                        self.dstore(b, dest, v);
                    }
                    0x3C => {
                        let v = self.ld(types::I32, o.vf + fs * 16 + fsf * 4);
                        self.set_vi(it, v);
                    }
                    _ => {
                        // MFIR: sign-extended VI to every dest field
                        let v = self.vi(is);
                        let v = self.b.ins().ishl_imm_u(v, 16);
                        let v = self.b.ins().sshr_imm_u(v, 16);
                        let v = self.b.ins().splat(types::I32X4, v);
                        self.write_vf(ft, dest, v);
                    }
                },
            },
        }
        // the VI this pair changed, for a branch in the next pair
        let h0 = match before {
            None => self.c32(0),
            Some(b) => {
                let now = self.vi(vt);
                let ch = self.b.ins().icmp(IntCC::NotEqual, now, b);
                let h = self.b.ins().bor_imm_u(b, super::vu::hist(vt, 0, (0x10..=0x1C).contains(&key)) as i64);
                let z = self.c32(0);
                self.b.ins().select(ch, h, z)
            }
        };
        self.st(h0, o.vi_hist);
        true
    }

    fn if_then(&mut self, cond: Value, f: impl FnOnce(&mut Self)) {
        let (then, next) = (self.b.create_block(), self.b.create_block());
        self.b.ins().brif(cond, then, &[], next, &[]);
        self.b.switch_to_block(then);
        f(self);
        self.b.ins().jump(next, &[]);
        self.b.switch_to_block(next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rng(u32);
    impl Rng {
        fn next(&mut self) -> u32 { let x = &mut self.0; *x ^= *x << 13; *x ^= *x >> 17; *x ^= *x << 5; *x }
        fn below(&mut self, n: u32) -> u32 { self.next() % n }
        /// a float with the exponents that need care (0, 255, results near the edges)
        fn float(&mut self) -> u32 {
            let r = self.next();
            const SPECIAL: [u32; 14] = [0, 0x8000_0000, 0x7FFF_FFFF, 0xFFFF_FFFF, 1, 0x0080_0000, 0x3F80_0000, 0xBF80_0000,
                0x7F80_0000, 0x0040_0000, 0x7F7F_FFFF, 0x0100_0000, 0x3F7F_FFFF, 0x7F00_0001];
            match r % 6 {
                0 => SPECIAL[(r >> 8) as usize % SPECIAL.len()],
                1 => r & 0x8FFF_FFFF | 0x3000_0000,          // moderate
                2 => r & 0x80FF_FFFF | 0x7F00_0000,          // huge
                3 => r & 0x80FF_FFFF | 0x0000_0000,          // tiny
                _ => r,
            }
        }
    }

    fn upper(r: &mut Rng) -> u32 {
        let dest = r.below(16);
        let (ft, fs, fd) = (r.below(32), r.below(32), r.below(32));
        if r.below(3) == 0 {
            // special: ACC forms, ITOF / FTOI, ABS, CLIP, OPMULA, NOP
            let n = loop { let n = r.below(0x30); if n != 0x2B { break n; } };
            dest << 21 | ft << 16 | fs << 11 | (n >> 2) << 6 | 0x3C | (n & 3)
        } else {
            let f = r.below(0x30);
            dest << 21 | ft << 16 | fs << 11 | fd << 6 | f
        }
    }

    fn lower(r: &mut Rng) -> u32 {
        let dest = r.below(16);
        let (ft, fs) = (r.below(32), r.below(32));
        let sp = |n: u32| 0x40 << 25 | dest << 21 | ft << 16 | fs << 11 | (n >> 2) << 6 | 0x3C | (n & 3);
        match r.below(18) {
            0 => 0x8000_033C,
            16 => [0x14u32, 0x15, 0x16, 0x17][r.below(4) as usize] << 25 | r.below(2) << 21 | (ft & 15) << 16 | r.below(0x800), // FSEQ / FSSET / FSAND / FSOR
            17 => [0x18u32, 0x1A, 0x1B][r.below(3) as usize] << 25 | (ft & 15) << 16 | (fs & 15) << 11,                   // FMEQ / FMAND / FMOR
            1 => sp(0x30), 2 => sp(0x31), 3 => sp(0x34), 4 => sp(0x35), 5 => sp(0x36), 6 => sp(0x37),
            7 => sp(0x3C), 8 => sp(0x3D), 9 => sp(0x38), 10 => sp(0x3B),
            11 => 0x40 << 25 | r.below(16) << 16 | r.below(16) << 11 | r.below(16) << 6 | [0x30, 0x31, 0x34, 0x35, 0x32][r.below(5) as usize],
            12 => dest << 21 | ft << 16 | (fs & 15) << 11 | r.below(0x800),                 // LQ
            13 => 1 << 25 | dest << 21 | (ft & 15) << 16 | fs << 11 | r.below(0x800),       // SQ
            14 => [4u32, 5][r.below(2) as usize] << 25 | dest << 21 | (ft & 15) << 16 | (fs & 15) << 11 | r.below(0x800), // ILW / ISW
            _ => [8u32, 9][r.below(2) as usize] << 25 | r.below(16) << 21 | (ft & 15) << 16 | (fs & 15) << 11 | r.below(0x800), // IADDIU / ISUBIU
        }
    }

    fn same(a: &Vu, b: &Vu) -> bool {
        let fq = |v: &Vu| (0..v.flags.len).map(|k| { let f = v.flags.q[(v.flags.head + k) as usize & 15]; (f.at, f.mac, f.stat, f.clip, f.is_clip) }).collect::<Vec<_>>();
        a.vf == b.vf && a.vi == b.vi && a.acc == b.acc && a.q == b.q && a.i == b.i && a.mac == b.mac && a.status == b.status
            && a.clip == b.clip && a.cycle == b.cycle && a.pc == b.pc && fq(a) == fq(b)
    }

    /// Random straight-line programs: translated code ends exactly as the interpreter
    #[test]
    fn jit_matches_the_interpreter() {
        let mut r = Rng(0x2545_F491);
        for prog in 0..1500 {
            let mut code = vec![0u8; 16 * 1024];
            let n = 8 + r.below(40) as usize;
            for k in 0..n + 2 {
                let (mut lo, mut hi) = (lower(&mut r), upper(&mut r));
                if k >= n { lo = 0x8000_033C; hi = 0x2FF; }
                if k == n { hi |= 1 << 30; }
                if k < n && r.below(12) == 0 { hi |= 1 << 31; lo = r.float(); }      // I bit
                code[8 * k..8 * k + 4].copy_from_slice(&lo.to_le_bytes());
                code[8 * k + 4..8 * k + 8].copy_from_slice(&hi.to_le_bytes());
            }
            let mut v = Vu::new(true);
            for reg in 1..32 { for f in 0..4 { v.vf[reg][f] = r.float(); } }
            for f in 0..4 { v.acc[f] = r.float(); }
            for k in 1..16 { v.vi[k] = r.below(0x400) as u16; }
            v.q = r.float();
            v.i = r.float();
            let mut data: Vec<u8> = (0..16 * 1024).map(|_| r.next() as u8).collect();
            for q in 0..1024 { if r.below(2) == 0 { for f in 0..4 { let x = r.float().to_le_bytes(); data[16 * q + 4 * f..16 * q + 4 * f + 4].copy_from_slice(&x); } } }
            let (mut a, mut b) = (v.clone(), v);
            let (mut da, mut db) = (data.clone(), data);
            let (mut pa, mut pb) = (Vec::new(), Vec::new());
            a.start(0);
            let mut steps = 0;
            while a.running && steps < 500 { a.step(&code, &mut da, &mut pa); steps += 1; }
            let mut j = VuJit::new_sync();
            assert!(j.get(&code, 0, true, 0).is_none());
            b.start(0);
            b.jit_prepare();
            let mut guard = 0;
            while b.running && guard < 500 {
                match j.get(&code, b.pc, true, 0) {
                    Some(f) => { let mut ctx = Ctx::new(&mut b, &mut db, &mut pb); unsafe { f(&mut ctx) } }
                    None => { b.step(&code, &mut db, &mut pb); b.jit_prepare(); }
                }
                guard += 1;
            }
            assert!(j.runs > 0);
            // (translated code lands flags only where they're read)
            a.land_flags();
            b.land_flags();
            if !same(&a, &b) || da != db {
                for k in 0..n { let w = u64::from_le_bytes(code[8 * k..8 * k + 8].try_into().unwrap()); eprintln!("  {:08X} {:08X}", (w >> 32) as u32, w as u32); }
                for reg in 0..32 { if a.vf[reg] != b.vf[reg] { eprintln!("  vf{reg} {:08X?} / {:08X?}", a.vf[reg], b.vf[reg]); } }
                panic!("program {prog}: interpreter / JIT differ: vi {:X?}/{:X?} acc {:08X?}/{:08X?} mac {:X}/{:X} status {:X}/{:X} clip {:X}/{:X} cycle {}/{} pc {:X}/{:X} q {:X}/{:X} data same {}",
                    a.vi, b.vi, a.acc, b.acc, a.mac, b.mac, a.status, b.status, a.clip, b.clip, a.cycle, b.cycle, a.pc, b.pc, a.q, b.q, da == db);
            }
        }
    }
}
