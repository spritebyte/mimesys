//! The EE's JIT: runs of R5900 code translated to host code with Cranelift.
//!
//! - A block runs from its start to the first branch and its delay slot (or a COP0
//!   instruction, SYSCALL / BREAK, a COP branch, or the end of the 4 KB page). Blocks are
//!   found by virtual address and translated once they've started `hot` times; until
//!   then the interpreter runs them.
//! - The registers stay in the `R5900`: translated code reads and writes them there, so
//!   the CPU is always up to date when anything else looks.
//! - The common ALU instructions, all the ordinary branches and jumps, and loads / stores
//!   that land in RAM or the scratchpad are translated. Anything else, and any load /
//!   store that isn't plain memory or would fault, calls the interpreter for that one
//!   instruction (`h_exec`); if that changed the flow (an exception, ERET, a COP branch)
//!   the block stops there.
//! - Code gets loaded and rewritten: every write to RAM (CPU stores, translated or not,
//!   and DMA) bumps its 4 KB page's generation if a block was translated from that page
//!   (`note_write`). A block whose page generation moved compares its code words with
//!   memory before it runs again. A TLB change or a new RAM throws the blocks away.
//! - Interrupts are taken between blocks; the COP0 timer advances a block at a time.
//! - Linking: a block that ends at a fixed address (fall-through, J / JAL, either side of
//!   a conditional branch) goes straight on to the translated block there through a link
//!   cell the dispatcher fills (`Link`), without coming back, while: the slice's cycles
//!   (and the cycles until the COP0 timer's Compare) last, the target's page generation is
//!   still the one it was checked at, and no store / load went through the interpreter in
//!   this run (it could have raised an interrupt). Never into a pure block (polling loops
//!   are spotted by the dispatcher). `PS2_EE_LINK=0` turns it off.

use super::*;
use cranelift_codegen::ir::{self, condcodes::IntCC, types, AbiParam, InstBuilder, MemFlagsData, Value};
use cranelift_codegen::isa::CallConv;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module};
use std::collections::HashMap;

/// Where the bus's RAM and ROM are, for translated code and for checking code words
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct JitMem {
    /// physical 0 up
    pub ram: *mut u8,
    pub ram_len: usize,
    /// code that never changes (the BIOS)
    pub rom: *const u8,
    pub rom_base: u32,
    pub rom_len: usize,
}

/// What a block gets: the CPU, the bus and where memory is
#[repr(C)]
pub struct Ctx {
    cpu: *mut R5900,
    bus: *mut u8,
    ram: *mut u8,
    map: *const u32,
    sp: *mut u8,
    ram_len: u64,
    /// added to by each block: the cycles it took
    cycles: u32,
    /// a block that loops to itself goes round again while it's taken fewer cycles than this
    budget: u32,
    /// linking goes on while blocks have taken fewer cycles than this (left)
    chain: u32,
    /// set when the interpreter ran a load / store: no more linking this run
    nochain: u32,
    /// blocks entered through links
    hops: u32,
    /// the last block run (its index), for filling its link cell
    last: u32,
}

/// A block's answer: the next block to run (a link), or 0 to go back to the dispatcher
type BlockFn = unsafe extern "C" fn(*mut Ctx) -> usize;

/// Where a block's fixed exit goes, filled by the dispatcher: the translated block there
/// and the page generation its code was last checked at
#[repr(C)]
struct Link {
    f: usize,
    genp: *const u32,
    pgen: u32,
    pc: u32,
}

/// The generation a block in ROM is checked against (never moves)
static ROM_GEN: u32 = 0;

/// Linking is on (not PS2_EE_LINK=0)
pub fn linking() -> bool { link_on() }

fn link_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("PS2_EE_LINK").map_or(true, |v| v != "0"))
}

/// `h_exec`'s answer: the instruction changed the flow, stop the block
const EXIT: u32 = 0x8000_0000;
/// The longest block (instructions)
const MAX_LEN: usize = 128;
const SLOTS: usize = 1 << 16;
const NONE: u32 = u32::MAX;
/// Translated blocks before the code memory is thrown away and started again
const MAX_COMPILED: usize = 200_000;

/// RAM pages (4 KB, physical, 32 MB) blocks were translated from, and how often each has
/// been written since. Shared by every R5900 in the process: another one's writes only
/// cost a needless compare of the code words.
const PAGES: usize = 0x200_0000 >> 12;
static CODE_PAGE: [std::sync::atomic::AtomicU8; PAGES] = [const { std::sync::atomic::AtomicU8::new(0) }; PAGES];
static PAGE_GEN: [std::sync::atomic::AtomicU32; PAGES] = [const { std::sync::atomic::AtomicU32::new(0) }; PAGES];

/// RAM at physical `pa` was written: blocks from its page check their code before running
#[inline]
pub fn note_write(pa: u32) {
    use std::sync::atomic::Ordering::Relaxed;
    let p = (pa as usize >> 12) & (PAGES - 1);
    if CODE_PAGE[p].load(Relaxed) != 0 { PAGE_GEN[p].fetch_add(1, Relaxed); }
}

fn no_links() -> Box<[Link; 2]> {
    Box::new([const { Link { f: 0, genp: &ROM_GEN, pgen: 0, pc: NONE } }; 2])
}

fn page_gen(p: usize) -> u32 { PAGE_GEN[p & (PAGES - 1)].load(std::sync::atomic::Ordering::Relaxed) }

struct Block {
    f: Option<BlockFn>,
    /// for code in RAM: its page (physical >> 12) and that page's generation when the code
    /// words were last known to match
    page: u32,
    pgen: u32,
    /// the code words as translated, and where they are now
    src: *const u32,
    words: Box<[u32]>,
    /// in RAM (ROM code doesn't change)
    check: bool,
    /// the fixed exits' link cells (their addresses are in the code)
    links: Box<[Link; 2]>,
    /// only ALU instructions, branches and ordinary loads: run again with the same
    /// registers (and the hardware still, as it is within a slice) it does the same
    pure: bool,
}

pub struct Jit {
    module: Option<JITModule>,
    fctx: FunctionBuilderContext,
    /// a direct-mapped cache in front of `index`: (start, block)
    slots: Vec<(u32, u32)>,
    index: HashMap<u32, u32>,
    blocks: Vec<Block>,
    /// blocks not translated yet: how often they've started
    cold: HashMap<u32, u32>,
    /// starts before a block is translated
    pub hot: u32,
    tlb: u32,
    mem: Option<JitMem>,
    compiled: usize,
    names: u64,
    /// stats: blocks run, translated, thrown away for changed code
    pub runs: u64,
    /// of those, entered through a link
    pub linked: u64,
    pub translated: u64,
    pub stale: u64,
    /// cycles skipped in polling loops (taken by the system's speed report)
    pub idle: u64,
    /// the polling loop the last run ended in (pc, registers): the next run skips its
    /// wait after one round that changes nothing
    poll: Option<(u32, [u128; 32], u128, u128)>,
}

unsafe impl Send for Jit {}

impl Default for Jit {
    fn default() -> Self { Self::new() }
}

/// One instruction through the interpreter, as if it were at `ipc` with the next PC
/// `expect` (`ipc + 4`, or for a delay slot the branch's destination)
unsafe extern "C" fn h_exec<B: EeBus>(c: *mut Ctx, ipc: u32, instr: u32, expect: u32, slot: u32) -> u32 {
    unsafe {
        // a load / store here may be I/O (and raise an interrupt): back to the
        // dispatcher at the block's end
        if (0x1A..=0x3F).contains(&(instr >> 26)) { (*c).nochain = 1; }
        let cpu = &mut *(*c).cpu;
        let bus = &mut *((*c).bus as *mut B);
        cpu.cur_pc = ipc;
        cpu.delay_slot = slot != 0;
        cpu.branch_pending = false;
        cpu.pc = expect;
        cpu.next_pc = expect.wrapping_add(4);
        let n = cpu.execute(bus, instr).unwrap_or(1);
        if cpu.pc != expect || cpu.branch_pending { n | EXIT } else { n }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum K {
    /// translated, can't fault
    Alu,
    /// a load / store with a fast path
    Mem,
    /// a branch or jump with a delay slot
    Branch,
    /// through the interpreter
    Exec,
    /// through the interpreter, then the block ends
    End,
}

fn kind(i: u32) -> K {
    let op = i >> 26;
    match op {
        0x00 => match i & 0x3F {
            0x00 | 0x02 | 0x03 | 0x04 | 0x06 | 0x07 | 0x0A | 0x0B | 0x0F | 0x10..=0x14 | 0x16 | 0x17
            | 0x21 | 0x23..=0x27 | 0x2A | 0x2B | 0x2D | 0x2F | 0x38 | 0x3A..=0x3C | 0x3E | 0x3F => K::Alu,
            0x08 | 0x09 => K::Branch,
            0x0C | 0x0D => K::End,
            _ => K::Exec,
        },
        0x01 => match i >> 16 & 31 { 0x00..=0x03 | 0x10..=0x13 => K::Branch, _ => K::Exec },
        0x02..=0x07 | 0x14..=0x17 => K::Branch,
        0x09..=0x0F | 0x19 | 0x2F | 0x33 => K::Alu,
        0x10 => K::End,
        // BC1x / BC2x: the interpreter leaves the branch pending
        0x11 | 0x12 if i >> 21 & 31 == 8 => K::End,
        0x1E | 0x1F | 0x20 | 0x21 | 0x23..=0x25 | 0x27 | 0x28 | 0x29 | 0x2B | 0x37 | 0x3F => {
            // a load into $0 still reads (FIFOs pop): the interpreter does those
            let load = !matches!(op, 0x1F | 0x28 | 0x29 | 0x2B | 0x3F);
            if load && i >> 16 & 31 == 0 { K::Exec } else { K::Mem }
        }
        _ => K::Exec,
    }
}

/// The GPRs a pure instruction (ALU, load, branch: see `kind`) reads and writes, as bit
/// masks (bit 32: HI / LO, which make it no polling loop)
fn regs_of(i: u32) -> (u64, u64) {
    let (rs, rt, rd) = (1u64 << (i >> 21 & 31), 1u64 << (i >> 16 & 31), 1u64 << (i >> 11 & 31));
    let (r, w) = match i >> 26 {
        0x00 => match i & 0x3F {
            0x08 => (rs, 0),
            0x09 => (rs, rd),
            0x10..=0x13 => (1 << 32, 1 << 32),
            _ => (rs | rt, rd),
        },
        0x01 => (rs, if matches!(i >> 16 & 31, 0x10..=0x13) { 1 << 31 } else { 0 }),
        0x02 => (0, 0),
        0x03 => (0, 1 << 31),
        0x04 | 0x05 | 0x14 | 0x15 => (rs | rt, 0),
        0x06 | 0x07 | 0x16 | 0x17 => (rs, 0),
        0x2F | 0x33 => (rs, 0),
        // immediates and loads: rs in, rt out
        _ => (rs, rt),
    };
    (r & !1, w & !1)
}

/// A pure block that is a polling loop if it loops to itself: every register it reads is
/// either one it never writes or one it wrote earlier in the same round, so each round
/// gives the same result until memory changes. (A counting loop reads last round's value:
/// it goes round inside its block as before.)
fn polls(ins: &[u32]) -> bool {
    let all = ins.iter().fold(0u64, |m, &i| m | regs_of(i).1);
    if all & 1 << 32 != 0 { return false; }
    let mut done = 0u64;
    for &i in ins {
        let (r, w) = regs_of(i);
        if r & all & !done != 0 { return false; }
        done |= w;
    }
    true
}

fn off_gpr(r: u32) -> i32 { (std::mem::offset_of!(R5900, gpr) + 16 * r as usize) as i32 }
fn off_hi() -> i32 { std::mem::offset_of!(R5900, hi) as i32 }
fn off_lo() -> i32 { std::mem::offset_of!(R5900, lo) as i32 }
fn off_pc() -> i32 { std::mem::offset_of!(R5900, pc) as i32 }
fn off_next_pc() -> i32 { std::mem::offset_of!(R5900, next_pc) as i32 }
fn ctx_off(f: usize) -> i32 { f as i32 }

impl Jit {
    pub fn new() -> Self {
        Self {
            module: None, fctx: FunctionBuilderContext::new(),
            slots: vec![(NONE, NONE); SLOTS], index: HashMap::new(), blocks: Vec::new(),
            cold: HashMap::new(), hot: 16, tlb: 0, mem: None, compiled: 0, names: 0,
            runs: 0, linked: 0, translated: 0, stale: 0, idle: 0, poll: None,
        }
    }

    /// Throw every block away
    pub fn flush(&mut self) {
        self.slots.fill((NONE, NONE));
        self.index.clear();
        self.blocks.clear();
        self.cold.clear();
        if self.compiled > MAX_COMPILED {
            if let Some(m) = self.module.take() { unsafe { m.free_memory(); } }
            self.compiled = 0;
        }
    }

    pub fn len(&self) -> usize { self.index.len() }

    fn module(&mut self) -> &mut JITModule {
        if self.module.is_none() {
            let b = JITBuilder::with_flags(&[("opt_level", "speed"), ("enable_verifier", "false"), ("preserve_frame_pointers", "true")], cranelift_module::default_libcall_names()).expect("host ISA");
            self.module = Some(JITModule::new(b));
        }
        self.module.as_mut().unwrap()
    }

    /// The block for `va`, if it's translated and its code is unchanged
    #[inline(always)]
    fn get<B: EeBus>(&mut self, cpu: &R5900, va: u32) -> Option<(BlockFn, u32, bool, u32)> {
        let s = (va >> 2) as usize & (SLOTS - 1);
        let (sva, si) = self.slots[s];
        let idx = if sva == va { si } else {
            match self.index.get(&va) {
                Some(&i) => { self.slots[s] = (va, i); i }
                None => {
                    let n = self.cold.entry(va).or_insert(0);
                    *n += 1;
                    if *n < self.hot { return None; }
                    self.cold.remove(&va);
                    let i = self.blocks.len() as u32;
                    let b = self.translate::<B>(cpu, va, i);
                    self.blocks.push(b);
                    self.index.insert(va, i);
                    self.slots[s] = (va, i);
                    i
                }
            }
        };
        let b = &mut self.blocks[idx as usize];
        if b.check {
            let g = page_gen(b.page as usize);
            if g != b.pgen {
                let mut same = true;
                for (k, &w) in b.words.iter().enumerate() {
                    if unsafe { b.src.add(k).read_unaligned() } != w { same = false; break; }
                }
                if same {
                    b.pgen = g;
                } else {
                    self.stale += 1;
                    let nb = self.translate::<B>(cpu, va, idx);
                    self.blocks[idx as usize] = nb;
                }
            }
        }
        let b = &self.blocks[idx as usize];
        Some((b.f?, b.words[0], b.pure, idx))
    }

    /// Block `from` just ended at `pc` and block `to` (not pure) starts there: point `from`'s link
    /// cell for `pc` at it (not a pure block: polling loops go through the dispatcher)
    #[inline]
    fn link(&mut self, from: u32, pc: u32, to: u32) {
        let Some(t) = self.blocks.get(to as usize) else { return };
        let (Some(f), false) = (t.f, t.pure) else { return };
        let genp: *const u32 = if t.check { PAGE_GEN[t.page as usize & (PAGES - 1)].as_ptr() } else { &ROM_GEN };
        let pgen = t.pgen;
        let Some(b) = self.blocks.get_mut(from as usize) else { return };
        for c in b.links.iter_mut() {
            if c.pc == pc { c.f = f as usize; c.genp = genp; c.pgen = pgen; }
        }
    }

    /// The host address of the code at `va` and how many words are readable there (to
    /// the end of the page)
    fn code(&self, cpu: &R5900, va: u32) -> Option<(*const u32, usize)> {
        let mem = self.mem?;
        let seg = va >> 29;
        let pa = if seg == 4 || seg == 5 { va & 0x1FFF_FFFF } else {
            let e = *cpu.map.get((va >> 12) as usize)?;
            if e & (PG_MAPPED | PG_VALID | PG_SCRATCH) != PG_MAPPED | PG_VALID { return None; }
            (e & 0xFFFF_F000) | (va & 0xFFF)
        };
        let words = (0x1000 - (va & 0xFFF) as usize) / 4;
        let bytes = words * 4;
        if (pa as usize) + bytes <= mem.ram_len {
            return Some((unsafe { mem.ram.add(pa as usize) } as *const u32, words));
        }
        if pa >= mem.rom_base && (pa - mem.rom_base) as usize + bytes <= mem.rom_len {
            return Some((unsafe { mem.rom.add((pa - mem.rom_base) as usize) } as *const u32, words));
        }
        None
    }

    /// Translate the code at `va` as block number `idx`
    fn translate<B: EeBus>(&mut self, cpu: &R5900, va: u32, idx: u32) -> Block {
        let none = Block { f: None, page: 0, pgen: 0, src: std::ptr::null(), words: Box::new([]), check: false, pure: false, links: no_links() };
        let Some((src, avail)) = self.code(cpu, va) else { return none };
        let check = self.mem.is_some_and(|m| (src as usize).wrapping_sub(m.ram as usize) < m.ram_len);
        // watch the page for writes from now on, before reading the code
        let page = self.mem.map_or(0, |m| ((src as usize).wrapping_sub(m.ram as usize) >> 12) as u32);
        if check { CODE_PAGE[page as usize & (PAGES - 1)].store(1, std::sync::atomic::Ordering::Relaxed); }
        let pgen = page_gen(page as usize);
        let word = |k: usize| unsafe { src.add(k).read_unaligned() };
        let mut ins = Vec::new();
        let lim = avail.min(MAX_LEN);
        while ins.len() < lim {
            let k = ins.len();
            let w = word(k);
            match kind(w) {
                K::Branch => {
                    // the delay slot must be on the page and an ordinary instruction
                    if k + 1 < avail && !matches!(kind(word(k + 1)), K::Branch | K::End) {
                        ins.push(w);
                        ins.push(word(k + 1));
                    }
                    break;
                }
                K::End => { ins.push(w); break; }
                _ => ins.push(w),
            }
        }
        let words: Box<[u32]> = ins.clone().into_boxed_slice();
        if ins.is_empty() { return Block { f: None, page, pgen, src, words, check, pure: false, links: no_links() }; }
        let pure = ins.iter().all(|&w| match kind(w) {
            K::Alu | K::Branch => true,
            // not LQ: the FIFOs are read by quadwords
            K::Mem => matches!(w >> 26, 0x20 | 0x21 | 0x23..=0x25 | 0x27 | 0x37),
            _ => false,
        });
        let poll = pure && polls(&ins);
        self.translated += 1;
        self.compiled += 1;
        let mut links = no_links();
        let (f, targets) = self.emit::<B>(va, &ins, &links, idx, poll);
        for (k, t) in targets.into_iter().enumerate() { links[k].pc = t; }
        Block { f: Some(f), page, pgen, src, words, check, pure, links }
    }

    /// The host code for `ins` at `va` (block `idx`, link cells `links`), and the fixed
    /// exits in link cell order
    fn emit<B: EeBus>(&mut self, va: u32, ins: &[u32], links: &[Link; 2], idx: u32, poll: bool) -> (BlockFn, Vec<u32>) {
        self.names += 1;
        let name = format!("ee{}", self.names);
        let mut fctx = std::mem::replace(&mut self.fctx, FunctionBuilderContext::new());
        let module = self.module();
        let tcfg = module.target_config();
        let ptr = tcfg.pointer_type();
        let cc = module.isa().default_call_conv();
        let mut ctx = module.make_context();
        ctx.func.signature = ir::Signature::new(cc);
        ctx.func.signature.params.push(AbiParam::new(ptr));
        ctx.func.signature.returns.push(AbiParam::new(ptr));
        let id = module.declare_function(&name, Linkage::Local, &ctx.func.signature).expect("declare block");
        let targets;
        {
            let mut b = FunctionBuilder::new(&mut ctx.func, &mut fctx);
            let entry = b.create_block();
            b.append_block_params_for_function_params(entry);
            b.switch_to_block(entry);
            let c = b.block_params(entry)[0];
            let fl = MemFlagsData::trusted();
            let cpu = b.ins().load(ptr, fl, c, ctx_off(std::mem::offset_of!(Ctx, cpu)));
            let cyc = b.declare_var(types::I32);
            let z = b.ins().iconst(types::I32, 0);
            b.def_var(cyc, z);
            let head = b.create_block();
            b.ins().jump(head, &[]);
            b.switch_to_block(head);
            let mut e = Em { b: &mut b, ptr, fl, ctx: c, cpu, cyc, n: 0, exec: h_exec::<B> as *const () as usize, cc, sig: None, start: va, head,
                links: links.as_ptr() as usize, targets: Vec::new(), idx, poll };
            e.block(va, ins);
            targets = std::mem::take(&mut e.targets);
            b.seal_all_blocks();
            b.finalize(tcfg);
        }
        module.define_function(id, &mut ctx).expect("translate block");
        module.clear_context(&mut ctx);
        module.finalize_definitions().expect("finalize block");
        let code = module.get_finalized_function(id);
        self.fctx = fctx;
        (unsafe { std::mem::transmute::<*const u8, BlockFn>(code) }, targets)
    }
}

struct Em<'a, 'b> {
    b: &'a mut FunctionBuilder<'b>,
    ptr: ir::Type,
    fl: MemFlagsData,
    ctx: Value,
    cpu: Value,
    /// cycles beyond one an instruction (from the interpreter's answers)
    cyc: Variable,
    /// instructions so far on this path
    n: u32,
    exec: usize,
    cc: CallConv,
    sig: Option<ir::SigRef>,
    /// the block's address and its first instruction's IR block (for loops)
    start: u32,
    head: ir::Block,
    /// the link cells (2) and the fixed exits given one so far
    links: usize,
    targets: Vec<u32>,
    /// this block's number
    idx: u32,
    /// a polling loop (see `polls`): it doesn't go round by itself, so the dispatcher can
    /// see it's waiting and skip the wait
    poll: bool,
}

impl Em<'_, '_> {
    fn c32(&mut self, v: u32) -> Value { self.b.ins().iconst(types::I32, v as i32 as i64) }
    fn c64(&mut self, v: u64) -> Value { self.b.ins().iconst(types::I64, v as i64) }
    fn g64(&mut self, r: u32) -> Value {
        if r == 0 { return self.c64(0); }
        self.b.ins().load(types::I64, self.fl, self.cpu, off_gpr(r))
    }
    fn g32(&mut self, r: u32) -> Value {
        if r == 0 { return self.c32(0); }
        self.b.ins().load(types::I32, self.fl, self.cpu, off_gpr(r))
    }
    fn s64(&mut self, r: u32, v: Value) {
        if r != 0 { self.b.ins().store(self.fl, v, self.cpu, off_gpr(r)); }
    }
    /// a 32-bit result, sign-extended to 64
    fn s32(&mut self, r: u32, v: Value) {
        if r != 0 { let x = self.b.ins().sextend(types::I64, v); self.s64(r, x); }
    }
    fn bool64(&mut self, cc: Value) -> Value { self.b.ins().uextend(types::I64, cc) }

    /// The cycles this block took; added to the run's count
    fn count(&mut self) -> Value {
        let x = self.b.use_var(self.cyc);
        let x = self.b.ins().iadd_imm_u(x, self.n as i64);
        let o = ctx_off(std::mem::offset_of!(Ctx, cycles));
        let t = self.b.ins().load(types::I32, self.fl, self.ctx, o);
        let t = self.b.ins().iadd(t, x);
        self.b.ins().store(self.fl, t, self.ctx, o);
        x
    }

    /// Count the cycles and go back to the dispatcher
    fn ret(&mut self) {
        self.count();
        let z = self.b.ins().iconst(self.ptr, 0);
        self.b.ins().return_(&[z]);
    }

    /// The block ends normally: on to `pc`, back to the dispatcher
    fn finish(&mut self, pc: Value) { self.finish_link(pc, &[]); }

    /// The block ends normally at `pc`, which is one of `targets` (fixed addresses): go
    /// straight on through that exit's link cell if it can (see the module notes)
    fn finish_link(&mut self, pc: Value, targets: &[u32]) {
        self.b.ins().store(self.fl, pc, self.cpu, off_pc());
        let np = self.b.ins().iadd_imm_u(pc, 4);
        self.b.ins().store(self.fl, np, self.cpu, off_next_pc());
        if targets.is_empty() || !link_on() { self.ret(); return; }
        let took = self.count();
        let fl = self.fl;
        // back to the dispatcher, which can fill this block's cell for `pc`
        let back = self.b.create_block();
        for &t in targets {
            let k = match self.targets.iter().position(|&x| x == t) {
                Some(k) => k,
                None => { self.targets.push(t); self.targets.len() - 1 }
            };
            assert!(k < 2, "a block has at most 2 fixed exits");
            let cell = self.b.ins().iconst(self.ptr, (self.links + k * std::mem::size_of::<Link>()) as i64);
            let (this, next) = (self.b.create_block(), self.b.create_block());
            let at = self.b.ins().icmp_imm_u(IntCC::Equal, pc, t as i64);
            self.b.ins().brif(at, this, &[], next, &[]);
            self.b.switch_to_block(this);
            let f = self.b.ins().load(self.ptr, fl, cell, std::mem::offset_of!(Link, f) as i32);
            let check = self.b.create_block();
            self.b.ins().brif(f, check, &[], back, &[]);
            self.b.switch_to_block(check);
            // the target's code unchanged, within the slice / timer, no interpreted load / store?
            let gp = self.b.ins().load(self.ptr, fl, cell, std::mem::offset_of!(Link, genp) as i32);
            let want = self.b.ins().load(types::I32, fl, cell, std::mem::offset_of!(Link, pgen) as i32);
            let g = self.b.ins().load(types::I32, fl, gp, 0);
            let same = self.b.ins().icmp(IntCC::Equal, g, want);
            let oc = ctx_off(std::mem::offset_of!(Ctx, chain));
            let left = self.b.ins().load(types::I32, fl, self.ctx, oc);
            let more = self.b.ins().icmp(IntCC::UnsignedLessThan, took, left);
            let nc = self.b.ins().load(types::I32, fl, self.ctx, ctx_off(std::mem::offset_of!(Ctx, nochain)));
            let free = self.b.ins().icmp_imm_u(IntCC::Equal, nc, 0);
            let ok = self.b.ins().band(same, more);
            let ok = self.b.ins().band(ok, free);
            let hop = self.b.create_block();
            self.b.ins().brif(ok, hop, &[], back, &[]);
            self.b.switch_to_block(hop);
            // the next block's budgets are what's left
            let rest = self.b.ins().isub(left, took);
            self.b.ins().store(fl, rest, self.ctx, oc);
            let ob = ctx_off(std::mem::offset_of!(Ctx, budget));
            let bud = self.b.ins().load(types::I32, fl, self.ctx, ob);
            let over = self.b.ins().icmp(IntCC::UnsignedLessThan, bud, took);
            let bl = self.b.ins().isub(bud, took);
            let z32 = self.c32(0);
            let bl = self.b.ins().select(over, z32, bl);
            self.b.ins().store(fl, bl, self.ctx, ob);
            let oh = ctx_off(std::mem::offset_of!(Ctx, hops));
            let h = self.b.ins().load(types::I32, fl, self.ctx, oh);
            let h = self.b.ins().iadd_imm_u(h, 1);
            self.b.ins().store(fl, h, self.ctx, oh);
            self.b.ins().return_(&[f]);
            self.b.switch_to_block(next);
        }
        self.b.ins().jump(back, &[]);
        self.b.switch_to_block(back);
        let me = self.c32(self.idx);
        self.b.ins().store(fl, me, self.ctx, ctx_off(std::mem::offset_of!(Ctx, last)));
        let z = self.b.ins().iconst(self.ptr, 0);
        self.b.ins().return_(&[z]);
    }

    /// The end of a branch to `pc`, which can be the block's own start (`to`, the
    /// destination if taken): then round again while within the budget
    fn finish_to(&mut self, pc: Value, to: Option<u32>, targets: &[u32]) {
        if to == Some(self.start) && !self.poll {
            let x = self.b.use_var(self.cyc);
            let total = self.b.ins().iadd_imm_u(x, self.n as i64);
            let bud = self.b.ins().load(types::I32, self.fl, self.ctx, ctx_off(std::mem::offset_of!(Ctx, budget)));
            let more = self.b.ins().icmp(IntCC::UnsignedLessThan, total, bud);
            let here = self.b.ins().icmp_imm_u(IntCC::Equal, pc, self.start as i64);
            let go = self.b.ins().band(more, here);
            let (back, out) = (self.b.create_block(), self.b.create_block());
            self.b.ins().brif(go, back, &[], out, &[]);
            self.b.switch_to_block(back);
            self.b.def_var(self.cyc, total);
            self.b.ins().jump(self.head, &[]);
            self.b.switch_to_block(out);
        }
        self.finish_link(pc, targets);
    }

    /// The interpreter runs `i` (already counted in `n`); stop if the flow changed
    fn exec(&mut self, pc: u32, i: u32, expect: Value, slot: bool) {
        let sig = match self.sig {
            Some(s) => s,
            None => {
                let mut s = ir::Signature::new(self.cc);
                s.params.push(AbiParam::new(self.ptr));
                for _ in 0..4 { s.params.push(AbiParam::new(types::I32)); }
                s.returns.push(AbiParam::new(types::I32));
                let s = self.b.import_signature(s);
                self.sig = Some(s);
                s
            }
        };
        let f = self.b.ins().iconst(self.ptr, self.exec as i64);
        let (a, b, d) = (self.c32(pc), self.c32(i), self.c32(slot as u32));
        let call = self.b.ins().call_indirect(sig, f, &[self.ctx, a, b, expect, d]);
        let r = self.b.inst_results(call)[0];
        let n = self.b.ins().band_imm_u(r, 0xFFFF);
        let n = self.b.ins().iadd_imm_s(n, -1);
        let old = self.b.use_var(self.cyc);
        let sum = self.b.ins().iadd(old, n);
        self.b.def_var(self.cyc, sum);
        let ex = self.b.ins().band_imm_u(r, EXIT as i64);
        let (out, on) = (self.b.create_block(), self.b.create_block());
        self.b.ins().brif(ex, out, &[], on, &[]);
        self.b.switch_to_block(out);
        self.ret();
        self.b.switch_to_block(on);
    }

    fn block(&mut self, va: u32, ins: &[u32]) {
        let mut k = 0;
        while k < ins.len() {
            let pc = va.wrapping_add(4 * k as u32);
            let i = ins[k];
            match kind(i) {
                K::Branch => { self.branch(pc, i, ins[k + 1]); return; }
                K::End => {
                    self.n += 1;
                    let ex = self.c32(pc.wrapping_add(4));
                    self.exec(pc, i, ex, false);
                    let next = self.c32(pc.wrapping_add(4));
                    self.finish(next);
                    return;
                }
                _ => { let ex = self.c32(pc.wrapping_add(4)); self.one(pc, i, ex, false); }
            }
            k += 1;
        }
        let to = va.wrapping_add(4 * ins.len() as u32);
        let end = self.c32(to);
        self.finish_link(end, &[to]);
    }

    /// An instruction that isn't a branch; `expect` is the PC after it
    fn one(&mut self, pc: u32, i: u32, expect: Value, slot: bool) {
        self.n += 1;
        match kind(i) {
            K::Alu => self.alu(i),
            K::Mem => self.mem(pc, i, expect, slot),
            _ => self.exec(pc, i, expect, slot),
        }
    }

    fn alu(&mut self, i: u32) {
        let op = i >> 26;
        let (rs, rt, rd, sa) = (i >> 21 & 31, i >> 16 & 31, i >> 11 & 31, i >> 6 & 31);
        let imm = i as i16 as i64;
        let uimm = (i & 0xFFFF) as i64;
        if op == 0 {
            let f = i & 0x3F;
            match f {
                0x11 => { let a = self.g64(rs); self.b.ins().store(self.fl, a, self.cpu, off_hi()); return; }
                0x13 => { let a = self.g64(rs); self.b.ins().store(self.fl, a, self.cpu, off_lo()); return; }
                0x0F => return,
                _ => {}
            }
            if rd == 0 { return; }
            let v = match f {
                0x00 => { let x = self.g32(rt); let v = self.b.ins().ishl_imm_u(x, sa as i64); return self.s32(rd, v); }
                0x02 => { let x = self.g32(rt); let v = self.b.ins().ushr_imm_u(x, sa as i64); return self.s32(rd, v); }
                0x03 => { let x = self.g32(rt); let v = self.b.ins().sshr_imm_u(x, sa as i64); return self.s32(rd, v); }
                0x04 => { let (x, s) = (self.g32(rt), self.g32(rs)); let v = self.b.ins().ishl(x, s); return self.s32(rd, v); }
                0x06 => { let (x, s) = (self.g32(rt), self.g32(rs)); let v = self.b.ins().ushr(x, s); return self.s32(rd, v); }
                0x07 => { let (x, s) = (self.g32(rt), self.g32(rs)); let v = self.b.ins().sshr(x, s); return self.s32(rd, v); }
                0x0A | 0x0B => {
                    let (a, t, old) = (self.g64(rs), self.g64(rt), self.g64(rd));
                    let z = self.b.ins().icmp_imm_u(if f == 0x0A { IntCC::Equal } else { IntCC::NotEqual }, t, 0);
                    self.b.ins().select(z, a, old)
                }
                0x10 => self.b.ins().load(types::I64, self.fl, self.cpu, off_hi()),
                0x12 => self.b.ins().load(types::I64, self.fl, self.cpu, off_lo()),
                0x14 | 0x16 | 0x17 => {
                    let (x, s) = (self.g64(rt), self.g32(rs));
                    let s = self.b.ins().uextend(types::I64, s);
                    match f { 0x14 => self.b.ins().ishl(x, s), 0x16 => self.b.ins().ushr(x, s), _ => self.b.ins().sshr(x, s) }
                }
                0x21 | 0x23 => {
                    let (a, b) = (self.g32(rs), self.g32(rt));
                    let v = if f == 0x21 { self.b.ins().iadd(a, b) } else { self.b.ins().isub(a, b) };
                    return self.s32(rd, v);
                }
                0x24..=0x27 | 0x2A | 0x2B | 0x2D | 0x2F => {
                    let (a, b) = (self.g64(rs), self.g64(rt));
                    match f {
                        0x24 => self.b.ins().band(a, b),
                        0x25 => self.b.ins().bor(a, b),
                        0x26 => self.b.ins().bxor(a, b),
                        0x27 => { let o = self.b.ins().bor(a, b); self.b.ins().bnot(o) }
                        0x2A => { let c = self.b.ins().icmp(IntCC::SignedLessThan, a, b); self.bool64(c) }
                        0x2B => { let c = self.b.ins().icmp(IntCC::UnsignedLessThan, a, b); self.bool64(c) }
                        0x2D => self.b.ins().iadd(a, b),
                        _ => self.b.ins().isub(a, b),
                    }
                }
                0x38 | 0x3A | 0x3B | 0x3C | 0x3E | 0x3F => {
                    let x = self.g64(rt);
                    let s = if f >= 0x3C { sa + 32 } else { sa } as i64;
                    match f & 3 {
                        0 => self.b.ins().ishl_imm_u(x, s),
                        2 => self.b.ins().ushr_imm_u(x, s),
                        _ => self.b.ins().sshr_imm_u(x, s),
                    }
                }
                _ => unreachable!("special {f:02X}"),
            };
            self.s64(rd, v);
            return;
        }
        if op == 0x2F || op == 0x33 || rt == 0 { return; } // CACHE, PREF
        match op {
            0x09 => { let a = self.g32(rs); let v = self.b.ins().iadd_imm_u(a, imm as u32 as i64); self.s32(rt, v); }
            0x0F => { let v = self.c64(((i & 0xFFFF) << 16) as i32 as i64 as u64); self.s64(rt, v); }
            _ => {
                let a = self.g64(rs);
                let v = match op {
                    0x0A => { let c = self.b.ins().icmp_imm_s(IntCC::SignedLessThan, a, imm); self.bool64(c) }
                    0x0B => { let c = self.b.ins().icmp_imm_s(IntCC::UnsignedLessThan, a, imm); self.bool64(c) }
                    0x0C => self.b.ins().band_imm_u(a, uimm),
                    0x0D => self.b.ins().bor_imm_u(a, uimm),
                    0x0E => self.b.ins().bxor_imm_u(a, uimm),
                    0x19 => self.b.ins().iadd_imm_s(a, imm),
                    _ => unreachable!("op {op:02X}"),
                };
                self.s64(rt, v);
            }
        }
    }

    /// A load or store: straight to RAM / the scratchpad when the address is there
    /// (and aligned and mapped), else the interpreter
    fn mem(&mut self, pc: u32, i: u32, expect: Value, slot: bool) {
        let op = i >> 26;
        let (rs, rt) = (i >> 21 & 31, i >> 16 & 31);
        let imm = i as i16 as i32 as u32;
        let (n, store) = match op {
            0x1E => (16, false), 0x1F => (16, true),
            0x20 | 0x24 => (1, false), 0x21 | 0x25 => (2, false), 0x23 | 0x27 => (4, false), 0x37 => (8, false),
            0x28 => (1, true), 0x29 => (2, true), 0x2B => (4, true), _ => (8, true),
        };
        let base = self.g32(rs);
        let mut va = self.b.ins().iadd_imm_u(base, imm as i64);
        if n == 16 { va = self.b.ins().band_imm_u(va, 0xFFFF_FFF0u32 as i64); }
        let fl = self.fl;
        // the page table entry
        let idx = self.b.ins().ushr_imm_u(va, 12);
        let idx = self.b.ins().uextend(types::I64, idx);
        let idx = self.b.ins().ishl_imm_u(idx, 2);
        let map = self.b.ins().load(self.ptr, fl, self.ctx, ctx_off(std::mem::offset_of!(Ctx, map)));
        let ea = self.b.ins().iadd(map, idx);
        let e = self.b.ins().load(types::I32, fl, ea, 0);
        // KSEG0 / KSEG1: unmapped
        let t = self.b.ins().iadd_imm_u(va, 0x8000_0000);
        let k = self.b.ins().icmp_imm_u(IntCC::UnsignedLessThan, t, 0x4000_0000);
        let pak = self.b.ins().band_imm_u(va, 0x1FFF_FFFF);
        let need = PG_MAPPED | PG_VALID | if store { PG_DIRTY } else { 0 };
        let em = self.b.ins().band_imm_u(e, need as i64);
        let okm = self.b.ins().icmp_imm_u(IntCC::Equal, em, need as i64);
        let ph = self.b.ins().band_imm_u(e, 0xFFFF_F000u32 as i64);
        let pl = self.b.ins().band_imm_u(va, 0xFFF);
        let pam = self.b.ins().bor(ph, pl);
        let pa = self.b.ins().select(k, pak, pam);
        let spf = self.b.ins().band_imm_u(e, PG_SCRATCH as i64);
        let spb = self.b.ins().icmp_imm_u(IntCC::NotEqual, spf, 0);
        let nk = self.b.ins().bxor_imm_u(k, 1);
        let is_sp = self.b.ins().band(spb, nk);
        let pa64 = self.b.ins().uextend(types::I64, pa);
        let len = self.b.ins().load(types::I64, fl, self.ctx, ctx_off(std::mem::offset_of!(Ctx, ram_len)));
        let inram = self.b.ins().icmp(IntCC::UnsignedLessThan, pa64, len);
        let rng = self.b.ins().bor(is_sp, inram);
        let mapok = self.b.ins().bor(k, okm);
        let mut ok = self.b.ins().band(mapok, rng);
        if n > 1 && n < 16 {
            let lo = self.b.ins().band_imm_u(va, n as i64 - 1);
            let al = self.b.ins().icmp_imm_u(IntCC::Equal, lo, 0);
            ok = self.b.ins().band(ok, al);
        }
        let (fast, slow, join) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
        self.b.ins().brif(ok, fast, &[], slow, &[]);

        self.b.switch_to_block(fast);
        let ram = self.b.ins().load(self.ptr, fl, self.ctx, ctx_off(std::mem::offset_of!(Ctx, ram)));
        let sp = self.b.ins().load(self.ptr, fl, self.ctx, ctx_off(std::mem::offset_of!(Ctx, sp)));
        let hb = self.b.ins().select(is_sp, sp, ram);
        let h = self.b.ins().iadd(hb, pa64);
        if store {
            // a page with translated code: bump its generation (RAM only)
            let p = self.b.ins().ushr_imm_u(pa, 12);
            let p = self.b.ins().band_imm_u(p, PAGES as i64 - 1);
            let p = self.b.ins().uextend(types::I64, p);
            let cp = self.b.ins().iconst(self.ptr, CODE_PAGE.as_ptr() as i64);
            let fa = self.b.ins().iadd(cp, p);
            let flag = self.b.ins().uload8(types::I32, fl, fa, 0);
            let nsp = self.b.ins().icmp_imm_u(IntCC::Equal, is_sp, 0);
            let nsp = self.b.ins().uextend(types::I32, nsp);
            let hit = self.b.ins().band(flag, nsp);
            let (bump, go) = (self.b.create_block(), self.b.create_block());
            self.b.ins().brif(hit, bump, &[], go, &[]);
            self.b.switch_to_block(bump);
            let gb = self.b.ins().iconst(self.ptr, PAGE_GEN.as_ptr() as i64);
            let p4 = self.b.ins().ishl_imm_u(p, 2);
            let ga = self.b.ins().iadd(gb, p4);
            let g = self.b.ins().load(types::I32, fl, ga, 0);
            let g = self.b.ins().iadd_imm_u(g, 1);
            self.b.ins().store(fl, g, ga, 0);
            self.b.ins().jump(go, &[]);
            self.b.switch_to_block(go);
            match n {
                1 => { let v = self.g64(rt); self.b.ins().istore8(fl, v, h, 0); }
                2 => { let v = self.g64(rt); self.b.ins().istore16(fl, v, h, 0); }
                4 => { let v = self.g64(rt); self.b.ins().istore32(fl, v, h, 0); }
                8 => { let v = self.g64(rt); self.b.ins().store(fl, v, h, 0); }
                _ => {
                    let lo = self.g64(rt);
                    let hi = if rt == 0 { self.c64(0) } else { self.b.ins().load(types::I64, fl, self.cpu, off_gpr(rt) + 8) };
                    self.b.ins().store(fl, lo, h, 0);
                    self.b.ins().store(fl, hi, h, 8);
                }
            }
        } else {
            let v = match op {
                0x20 => self.b.ins().sload8(types::I64, fl, h, 0),
                0x24 => self.b.ins().uload8(types::I64, fl, h, 0),
                0x21 => self.b.ins().sload16(types::I64, fl, h, 0),
                0x25 => self.b.ins().uload16(types::I64, fl, h, 0),
                0x23 => self.b.ins().sload32(fl, h, 0),
                0x27 => self.b.ins().uload32(fl, h, 0),
                0x37 => self.b.ins().load(types::I64, fl, h, 0),
                _ => {
                    let lo = self.b.ins().load(types::I64, fl, h, 0);
                    let hi = self.b.ins().load(types::I64, fl, h, 8);
                    self.b.ins().store(fl, hi, self.cpu, off_gpr(rt) + 8);
                    lo
                }
            };
            self.s64(rt, v);
        }
        self.b.ins().jump(join, &[]);

        self.b.switch_to_block(slow);
        self.exec(pc, i, expect, slot);
        self.b.ins().jump(join, &[]);
        self.b.switch_to_block(join);
    }

    /// A branch at `pc` with `slot` after it: the block ends
    fn branch(&mut self, pc: u32, i: u32, slot: u32) {
        let op = i >> 26;
        let (rs, rt, rd) = (i >> 21 & 31, i >> 16 & 31, i >> 11 & 31);
        let rel = pc.wrapping_add(4).wrapping_add(((i as i16 as i32) << 2) as u32);
        let after = pc.wrapping_add(8);
        // (condition or always, destination, likely, link register)
        let to = match op {
            0x00 => None,
            0x02 | 0x03 => Some((pc.wrapping_add(4) & 0xF000_0000) | (i & 0x03FF_FFFF) << 2),
            _ => Some(rel),
        };
        let (cond, dest, likely, link): (Option<Value>, Value, bool, u32) = match op {
            0x00 => { let t = self.g32(rs); (None, t, false, if i & 0x3F == 0x09 { rd } else { 0 }) }
            0x01 => {
                let a = self.g64(rs);
                let cc = if rt & 1 == 0 { IntCC::SignedLessThan } else { IntCC::SignedGreaterThanOrEqual };
                let c = self.b.ins().icmp_imm_s(cc, a, 0);
                let d = self.c32(rel);
                (Some(c), d, rt & 2 != 0, if rt & 0x10 != 0 { 31 } else { 0 })
            }
            0x02 | 0x03 => {
                let d = self.c32((pc.wrapping_add(4) & 0xF000_0000) | (i & 0x03FF_FFFF) << 2);
                (None, d, false, if op == 3 { 31 } else { 0 })
            }
            _ => {
                let a = self.g64(rs);
                let c = match op & 3 {
                    0 => { let b = self.g64(rt); self.b.ins().icmp(IntCC::Equal, a, b) }
                    1 => { let b = self.g64(rt); self.b.ins().icmp(IntCC::NotEqual, a, b) }
                    2 => self.b.ins().icmp_imm_s(IntCC::SignedLessThanOrEqual, a, 0),
                    _ => self.b.ins().icmp_imm_s(IntCC::SignedGreaterThan, a, 0),
                };
                let d = self.c32(rel);
                (Some(c), d, op >= 0x14, 0)
            }
        };
        if link != 0 { let l = self.c64(after as i32 as i64 as u64); self.s64(link, l); }
        self.n += 1;
        let spc = pc.wrapping_add(4);
        match (cond, likely) {
            (None, _) => {
                self.one(spc, slot, dest, true);
                let t: Vec<u32> = to.into_iter().collect();
                self.finish_to(dest, to, &t);
            }
            (Some(c), false) => {
                let a = self.c32(after);
                let next = self.b.ins().select(c, dest, a);
                self.one(spc, slot, next, true);
                self.finish_to(next, to, &[rel, after]);
            }
            (Some(c), true) => {
                let (yes, no) = (self.b.create_block(), self.b.create_block());
                self.b.ins().brif(c, yes, &[], no, &[]);
                self.b.switch_to_block(no);
                let a = self.c32(after);
                let n = self.n;
                self.finish_link(a, &[after]);
                self.b.switch_to_block(yes);
                self.n = n;
                self.one(spc, slot, dest, true);
                self.finish_to(dest, to, &[rel]);
            }
        }
    }
}

impl R5900 {
    /// Run about `target` cycles with the JIT (blocks not translated yet, interrupts and
    /// delay slots left pending go through the interpreter). Returns the cycles run.
    pub fn run_jit<B: EeBus>(&mut self, bus: &mut B, jit: &mut Jit, target: u32) -> u32 {
        let mut c = 0;
        let Some(mem) = bus.jit_mem() else {
            while c < target { c += self.step(bus); if super::slice_break() { break; } }
            return c;
        };
        if jit.mem != Some(mem) || jit.tlb != self.tlb_gen {
            jit.flush();
            jit.mem = Some(mem);
            jit.tlb = self.tlb_gen;
        }
        let mut ctx = Ctx {
            cpu: std::ptr::null_mut(), bus: std::ptr::null_mut(), ram: mem.ram,
            map: self.map.as_ptr(), sp: self.scratch.as_mut_ptr(), ram_len: mem.ram_len as u64, cycles: 0, budget: 0,
            chain: 0, nochain: 0, hops: 0, last: NONE,
        };
        // the block that ran last (for linking it to the next one)
        let mut prev = NONE;
        // polling loops: the registers at the start of a run of pure blocks
        let mut snap: Option<(u32, [u128; 32], u128, u128)> = None;
        let mut pure_run = 0u32;
        // still in the polling loop the last run skipped: one round, then compare
        if let Some(p) = jit.poll.take() {
            if p.0 == self.pc && p.1 == self.gpr && p.2 == self.hi && p.3 == self.lo {
                snap = Some(p);
                pure_run = 3;
            }
        }
        while c < target {
            // a DMA was started: end the slice so it runs now (not inside a delay slot)
            if c > 0 && !self.branch_pending && super::slice_break() { break; }
            if self.branch_pending || self.irq_ready() || self.map.len() != 1 << 20 {
                c += self.step(bus);
                pure_run = 0;
                prev = NONE;
                continue;
            }
            // a TLB write in the last block
            if jit.tlb != self.tlb_gen { jit.flush(); jit.tlb = self.tlb_gen; prev = NONE; }
            let pc = self.pc;
            match jit.get::<B>(self, pc) {
                Some((f, w, pure, idx)) => {
                    if prev != NONE && !pure && link_on() { jit.link(prev, pc, idx); }
                    if !pure {
                        pure_run = 0;
                    } else if pure_run % 4 == 0 && pure_run > 0
                        && snap.as_ref().is_some_and(|s| s.0 == pc && s.1 == self.gpr && s.2 == self.hi && s.3 == self.lo) {
                        // round a loop that only reads, back where it was: it would
                        // go round the same way until the hardware moves (the slice's end)
                        let n = target - c;
                        self.advance(n);
                        jit.idle += n as u64;
                        jit.poll = snap;
                        return target;
                    } else {
                        // (snapshots and compares are spaced out: a counting loop pays for them)
                        // (and again when the run moved on to another loop)
                        if pure_run % 16 == 0 || (pure_run % 4 == 0 && snap.as_ref().is_some_and(|s| s.0 != pc)) {
                            snap = Some((pc, self.gpr, self.hi, self.lo));
                        }
                        pure_run += 1;
                    }
                    if !self.trace.is_empty() {
                        self.trace[self.trace_pos] = (pc, w, self.r32(29), self.r32(31));
                        self.trace_pos = (self.trace_pos + 1) % TRACE_LEN;
                    }
                    ctx.cpu = self as *mut R5900;
                    ctx.bus = bus as *mut B as *mut u8;
                    ctx.map = self.map.as_ptr();
                    ctx.cycles = 0;
                    ctx.budget = target - c;
                    // links stop at the COP0 timer's Compare, so it fires where it would have
                    let to_cmp = self.cop0[COMPARE].wrapping_sub(self.cop0[COUNT]);
                    ctx.chain = if to_cmp == 0 { ctx.budget } else { ctx.budget.min(to_cmp) };
                    ctx.nochain = 0;
                    ctx.hops = 0;
                    ctx.last = idx;
                    let mut f = f;
                    loop {
                        let next = unsafe { f(&mut ctx) };
                        if next == 0 { break; }
                        f = unsafe { std::mem::transmute::<usize, BlockFn>(next) };
                    }
                    jit.runs += 1 + ctx.hops as u64;
                    jit.linked += ctx.hops as u64;
                    if ctx.hops > 0 { pure_run = 0; }
                    prev = ctx.last;
                    let n = ctx.cycles;
                    self.advance(n);
                    c += n;
                }
                None => {
                    pure_run = 0;
                    prev = NONE;
                    // the interpreter, to the end of the next branch's delay slot
                    let mut k = 0;
                    loop {
                        let slot = self.branch_pending;
                        c += self.step(bus);
                        k += 1;
                        if slot || k >= MAX_LEN || c >= target { break; }
                    }
                }
            }
        }
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 4 MB of RAM; the JIT's fast path only sees the first 2 MB (the rest is "I/O")
    struct Ram(Vec<u8>);
    impl EeBus for Ram {
        fn read8(&mut self, pa: u32) -> u8 { self.0[pa as usize] }
        fn read16(&mut self, pa: u32) -> u16 { u16::from_le_bytes(self.0[pa as usize..pa as usize + 2].try_into().unwrap()) }
        fn read32(&mut self, pa: u32) -> u32 { u32::from_le_bytes(self.0[pa as usize..pa as usize + 4].try_into().unwrap()) }
        fn write8(&mut self, pa: u32, v: u8) { self.0[pa as usize] = v; }
        fn write16(&mut self, pa: u32, v: u16) { self.0[pa as usize..pa as usize + 2].copy_from_slice(&v.to_le_bytes()); }
        fn write32(&mut self, pa: u32, v: u32) { self.0[pa as usize..pa as usize + 4].copy_from_slice(&v.to_le_bytes()); }
        fn jit_mem(&mut self) -> Option<JitMem> {
            Some(JitMem { ram: self.0.as_mut_ptr(), ram_len: 0x20_0000, rom: std::ptr::null(), rom_base: 0x1FC0_0000, rom_len: 0 })
        }
    }

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 { self.0 ^= self.0 << 13; self.0 ^= self.0 >> 7; self.0 ^= self.0 << 17; self.0 }
        fn below(&mut self, n: u32) -> u32 { (self.next() % n as u64) as u32 }
    }

    const BASE: u32 = 0x8000_1000;
    fn r(op: u32, rs: u32, rt: u32, rd: u32, sa: u32, f: u32) -> u32 { op << 26 | rs << 21 | rt << 16 | rd << 11 | sa << 6 | f }
    fn i(op: u32, rs: u32, rt: u32, imm: u32) -> u32 { op << 26 | rs << 21 | rt << 16 | (imm & 0xFFFF) }

    /// a random instruction that isn't a branch: registers 0-8, memory through r9 (fast
    /// RAM) or r10 (the slow part)
    fn plain(g: &mut Rng) -> u32 {
        let reg = |g: &mut Rng| g.below(9);
        match g.below(10) {
            0..=2 => {
                let op = [0x09, 0x19, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F][g.below(8) as usize];
                i(op, reg(g), reg(g), g.next() as u32)
            }
            3..=5 => {
                let f = [0x00, 0x02, 0x03, 0x04, 0x06, 0x07, 0x0A, 0x0B, 0x10, 0x11, 0x12, 0x13, 0x14, 0x16, 0x17,
                    0x21, 0x23, 0x24, 0x25, 0x26, 0x27, 0x2A, 0x2B, 0x2D, 0x2F, 0x38, 0x3A, 0x3B, 0x3C, 0x3E, 0x3F,
                    0x18, 0x19, 0x1A, 0x1B][g.below(35) as usize];
                r(0, reg(g), reg(g), reg(g), g.below(32), f)
            }
            6 => r(0x1C, reg(g), reg(g), reg(g), 0, 0x08), // MMI PADDW... (the interpreter)
            _ => {
                let (op, n) = [(0x1E, 16), (0x1F, 16), (0x20, 1), (0x24, 1), (0x21, 2), (0x25, 2), (0x23, 4), (0x27, 4),
                    (0x37, 8), (0x28, 1), (0x29, 2), (0x2B, 4), (0x3F, 8)][g.below(13) as usize];
                let base = if g.below(4) == 0 { 10 } else { 9 };
                i(op, base, reg(g), g.below(0x400) * n)
            }
        }
    }

    /// A random program of `len` instructions with forward branches, then `j END`
    fn program(g: &mut Rng, len: usize) -> Vec<u32> {
        let mut p = Vec::new();
        while p.len() < len {
            if g.below(6) == 0 && p.len() + 2 <= len {
                let k = p.len();
                // to somewhere after the slot, at most the final j
                let to = (k + 2 + g.below((len - k) as u32) as usize).min(len);
                let off = (to as i32 - k as i32 - 1) as u32;
                let (a, b) = (g.below(9), g.below(9));
                let w = match g.below(12) {
                    0 => i(0x04, a, b, off), 1 => i(0x05, a, b, off), 2 => i(0x06, a, 0, off), 3 => i(0x07, a, 0, off),
                    4 => i(0x14, a, b, off), 5 => i(0x15, a, b, off), 6 => i(0x16, a, 0, off), 7 => i(0x17, a, 0, off),
                    8 => i(0x01, a, [0, 1, 2, 3, 0x10, 0x11, 0x12, 0x13][g.below(8) as usize], off),
                    9 => 2 << 26 | ((BASE + 4 * to as u32) >> 2 & 0x03FF_FFFF),
                    10 => 3 << 26 | ((BASE + 4 * to as u32) >> 2 & 0x03FF_FFFF),
                    _ => i(0x04, 0, 0, off),
                };
                p.push(w);
                p.push(plain(g));
            } else {
                p.push(plain(g));
            }
        }
        let end = BASE + 4 * (len as u32 + 2);
        p.push(2 << 26 | (end >> 2 & 0x03FF_FFFF));
        p.push(0);
        p.push(2 << 26 | (end >> 2 & 0x03FF_FFFF));
        p.push(0);
        p
    }

    fn setup(g: &mut Rng, code: &[u32]) -> (R5900, Ram) {
        let mut ram = Ram(vec![0; 0x40_0000]);
        for (k, w) in code.iter().enumerate() { ram.write32(0x1000 + 4 * k as u32, *w); }
        for k in (0x10_0000..0x10_4000).chain(0x30_0000..0x30_4000) { ram.0[k] = g.next() as u8; }
        let mut c = R5900::new();
        c.cop0[STATUS] = ST_CU1 | 1 << 30;
        // exceptions (overflow can't happen; nothing should fault) land on the end loop
        c.jump(BASE);
        for k in 1..9 { c.gpr[k] = (g.next() as u128) << 64 | g.next() as u128; if g.below(3) == 0 { c.gpr[k] &= 0xFF; } }
        c.gpr[9] = 0x8010_0000;
        c.gpr[10] = 0x8030_0000;
        c.hi = (g.next() as u128) << 64 | g.next() as u128;
        c.lo = (g.next() as u128) << 64 | g.next() as u128;
        (c, ram)
    }

    #[test]
    fn jit_matches_the_interpreter() {
        for seed in 1..300u64 {
            let mut g = Rng(seed * 0x9E37_79B9_7F4A_7C15 | 1);
            let len = 8 + g.below(40) as usize;
            let code = program(&mut g, len);
            let end = BASE + 4 * (len as u32 + 2);
            let mut g2 = Rng(seed);
            let (mut a, mut ra) = setup(&mut g2, &code);
            let mut g2 = Rng(seed);
            let (mut b, mut rb) = setup(&mut g2, &code);
            let mut k = 0;
            while a.pc != end && k < 10_000 { a.step(&mut ra); k += 1; }
            assert_eq!(a.pc, end, "seed {seed}: the interpreter didn't finish");
            let mut jit = Jit::new();
            jit.hot = 1;
            let mut k = 0;
            while b.pc != end && k < 10_000 { b.run_jit(&mut rb, &mut jit, 1); k += 1; }
            assert!(jit.translated > 0);
            let ctx = format!("seed {seed}: {:08X?}", code);
            assert_eq!(b.pc, end, "{ctx}");
            for r in 0..32 { assert_eq!(a.gpr[r], b.gpr[r], "r{r} {ctx}"); }
            assert_eq!((a.hi, a.lo, a.sa), (b.hi, b.lo, b.sa), "{ctx}");
            assert_eq!(a.cycles, b.cycles, "cycles {ctx}");
            assert_eq!(a.cop0[COUNT], b.cop0[COUNT], "{ctx}");
            assert!(ra.0 == rb.0, "memory {ctx}");
        }
    }

    /// The same with slices of 64 cycles, so blocks link to each other
    #[test]
    fn linked_blocks_match_the_interpreter() {
        let mut linked = 0;
        for seed in 1..300u64 {
            let mut g = Rng(seed * 0x9E37_79B9_7F4A_7C15 | 1);
            let len = 8 + g.below(40) as usize;
            let mut code = program(&mut g, len);
            // round the program 8 times (r20 counts; the program only uses r1-r10)
            code.truncate(len);
            code.extend([i(0x09, 20, 20, 0xFFFF), i(0x05, 20, 0, (-(len as i32) - 2) as u32), 0]);
            let end = BASE + 4 * (len as u32 + 3);
            code.extend([2 << 26 | (end >> 2 & 0x03FF_FFFF), 0]);
            let mut g2 = Rng(seed);
            let (mut a, mut ra) = setup(&mut g2, &code);
            let mut g2 = Rng(seed);
            let (mut b, mut rb) = setup(&mut g2, &code);
            a.gpr[20] = 8;
            b.gpr[20] = 8;
            let mut jit = Jit::new();
            jit.hot = 1;
            let mut k = 0;
            while b.pc != end && k < 10_000 { b.run_jit(&mut rb, &mut jit, 64); k += 1; }
            let ctx = format!("seed {seed}: {:08X?}", code);
            assert_eq!(b.pc, end, "{ctx}");
            // the interpreter to the same point (the end is a J-to-itself loop)
            let mut k = 0;
            while a.cycles < b.cycles && k < 100_000 { a.step(&mut ra); k += 1; }
            // (the JIT skips the wait in the end loop from its start: the interpreter can
            // be in its delay slot then)
            let apc = if a.pc == end + 4 { end } else { a.pc };
            assert_eq!((apc, a.cycles, a.cop0[COUNT]), (b.pc, b.cycles, b.cop0[COUNT]), "{ctx}");
            for r in 0..32 { assert_eq!(a.gpr[r], b.gpr[r], "r{r} {ctx}"); }
            assert_eq!((a.hi, a.lo, a.sa), (b.hi, b.lo, b.sa), "{ctx}");
            assert!(ra.0 == rb.0, "memory {ctx}");
            linked += jit.linked;
        }
        assert!(linked > 1000, "only {linked} linked runs");
    }

    #[test]
    fn a_block_that_loops_to_itself_goes_round_inside() {
        // r2 = 1000; loop: ADDIU r2, -1; BNE r2, r0, loop; ADDIU r3, 1 (slot); then J here
        let code = [i(0x09, 0, 2, 1000), i(0x09, 2, 2, 0xFFFF), i(0x05, 2, 0, 0xFFFE), i(0x09, 3, 3, 1),
            2 << 26 | ((BASE + 16) >> 2 & 0x03FF_FFFF), 0];
        let load = |ram: &mut Ram| for (k, w) in code.iter().enumerate() { ram.write32(0x1000 + 4 * k as u32, *w); };
        let (mut ra, mut rb) = (Ram(vec![0; 0x40_0000]), Ram(vec![0; 0x40_0000]));
        load(&mut ra);
        load(&mut rb);
        let (mut a, mut b) = (R5900::new(), R5900::new());
        a.jump(BASE);
        b.jump(BASE);
        let mut jit = Jit::new();
        jit.hot = 1;
        // one call: the loop (and the J loop after it) stay inside their blocks
        b.run_jit(&mut rb, &mut jit, 10_000);
        assert!(jit.runs < 10, "runs {} idle {}", jit.runs, jit.idle);
        while a.cycles < b.cycles { a.step(&mut ra); }
        assert_eq!((b.gpr[2], b.gpr[3]), (0, 1000));
        // (the J loop is a polling loop: skipped from its start, the interpreter may be in
        // its delay slot)
        let apc = if a.pc == BASE + 20 { BASE + 16 } else { a.pc };
        assert_eq!((apc, a.gpr[3], a.cycles, a.cop0[COUNT]), (b.pc, b.gpr[3], b.cycles, b.cop0[COUNT]));
        while a.pc != BASE + 16 { a.step(&mut ra); }
        // a small budget: out to the run loop and back in
        let mut c = R5900::new();
        let mut rc = Ram(vec![0; 0x40_0000]);
        load(&mut rc);
        c.jump(BASE);
        while c.pc != BASE + 16 { c.run_jit(&mut rc, &mut jit, 7); }
        assert_eq!((c.gpr[2], c.gpr[3]), (0, 1000));
    }

    #[test]
    fn a_polling_loop_skips_its_wait() {
        // BASE: LW r2, 0(r9); BEQ r2, r0, BASE; NOP; ADDIU r3, r3, 1; J BASE+16; NOP
        let code = [i(0x23, 9, 2, 0), i(0x04, 2, 0, 0xFFFE), 0, i(0x09, 3, 3, 1), 2 << 26 | ((BASE + 16) >> 2 & 0x03FF_FFFF), 0];
        let mut ram = Ram(vec![0; 0x40_0000]);
        for (k, w) in code.iter().enumerate() { ram.write32(0x1000 + 4 * k as u32, *w); }
        let mut c = R5900::new();
        c.jump(BASE);
        c.gpr[9] = 0x8010_0000;
        let mut jit = Jit::new();
        jit.hot = 1;
        // a few rounds to see it changes nothing, then the rest of the budget is skipped
        assert_eq!(c.run_jit(&mut ram, &mut jit, 10_000), 10_000);
        let (runs, idle) = (jit.runs, jit.idle);
        assert!(runs < 12 && idle > 9_000, "runs {runs} idle {idle}");
        // the next run: one round to look at memory, then skipped again
        assert_eq!(c.run_jit(&mut ram, &mut jit, 10_000), 10_000);
        assert!(jit.runs - runs <= 2 && jit.idle - idle > 9_900, "runs {} idle {}", jit.runs - runs, jit.idle - idle);
        assert_eq!(c.pc, BASE);
        // memory changes (as an interrupt or DMA would): out of the loop
        ram.write32(0x10_0000, 1);
        c.run_jit(&mut ram, &mut jit, 10_000);
        assert_eq!(c.gpr[3], 1);
    }

    #[test]
    fn rewritten_code_is_translated_again() {
        // ADDIU r2, r2, 1; J BASE+8 (a loop); NOP
        let mut ram = Ram(vec![0; 0x40_0000]);
        let code = [i(0x09, 2, 2, 1), 2 << 26 | (BASE >> 2 & 0x03FF_FFFF), 0];
        for (k, w) in code.iter().enumerate() { ram.write32(0x1000 + 4 * k as u32, *w); }
        let mut c = R5900::new();
        c.jump(BASE);
        let mut jit = Jit::new();
        jit.hot = 1;
        for _ in 0..4 { c.run_jit(&mut ram, &mut jit, 1); }
        assert_eq!(c.gpr[2], 4);
        ram.write32(0x1000, i(0x09, 2, 2, 0x100));
        // a write straight to the test's RAM: tell the JIT, as the CPU's stores and DMA do
        note_write(0x1000);
        for _ in 0..2 { c.run_jit(&mut ram, &mut jit, 1); }
        assert_eq!(c.gpr[2], 0x204);
        assert_eq!(jit.stale, 1);
    }

    #[test]
    fn translated_stores_into_code_are_noticed() {
        // BASE: SW r3, 0x20(r4); J BASE+0x20; NOP   BASE+0x20: ADDIU r2, r2, 1; J BASE; NOP
        let mut ram = Ram(vec![0; 0x40_0000]);
        let add1 = i(0x09, 2, 2, 1);
        let code = [i(0x2B, 4, 3, 0x20), 2 << 26 | ((BASE + 0x20) >> 2 & 0x03FF_FFFF), 0];
        let tail = [add1, 2 << 26 | (BASE >> 2 & 0x03FF_FFFF), 0];
        for (k, w) in code.iter().enumerate() { ram.write32(0x1000 + 4 * k as u32, *w); }
        for (k, w) in tail.iter().enumerate() { ram.write32(0x1020 + 4 * k as u32, *w); }
        let mut c = R5900::new();
        c.jump(BASE);
        c.gpr[3] = add1 as u128;
        c.gpr[4] = BASE as i32 as i64 as u64 as u128;
        let mut jit = Jit::new();
        jit.hot = 1;
        // the store writes the same instruction back: checked, not translated again
        for _ in 0..6 { c.run_jit(&mut ram, &mut jit, 1); }
        assert_eq!(jit.stale, 0);
        let before = c.gpr[2];
        assert!(before >= 2, "{before}");
        // now it writes ADDIU r2, r2, 0x100 over the loop's first instruction
        c.gpr[3] = i(0x09, 2, 2, 0x100) as u128;
        for _ in 0..8 { c.run_jit(&mut ram, &mut jit, 1); }
        assert_eq!(jit.stale, 1);
        assert_eq!(c.gpr[2] & 0xFF, before & 0xFF, "the old ADDIU 1 ran after the rewrite");
        assert!(c.gpr[2] >= before + 0x100);
    }

    #[test]
    /// Speed: a loop of 4 blocks that store (not pure), linked or not
    #[test]
    #[ignore]
    fn bench_linked_blocks() {
        // 4 blocks: SW r3, k(r9); ADDIU r3, r3, 1; J next; NOP ... the last BNE r2 back
        let mut code = Vec::new();
        for k in 0..4u32 {
            let next = BASE + 16 * (k + 1);
            code.extend([i(0x2B, 9, 3, 4 * k), i(0x09, 3, 3, 1), 2 << 26 | (next >> 2 & 0x03FF_FFFF), 0]);
        }
        code.extend([i(0x09, 2, 2, 0xFFFF), i(0x05, 2, 0, (-(code.len() as i32) - 2) as u32), 0]);
        let end = BASE + 4 * code.len() as u32;
        code.extend([2 << 26 | (end >> 2 & 0x03FF_FFFF), 0]);
        let mut ram = Ram(vec![0; 0x40_0000]);
        for (k, w) in code.iter().enumerate() { ram.write32(0x1000 + 4 * k as u32, *w); }
        let mut c = R5900::new();
        c.jump(BASE);
        c.gpr[2] = 2_000_000;
        c.gpr[9] = 0x8010_0000;
        let mut jit = Jit::new();
        let t = std::time::Instant::now();
        while c.pc != end { c.run_jit(&mut ram, &mut jit, 256); }
        let el = t.elapsed();
        println!("{} block runs ({} linked) in {:?}: {:.1} ns a block", jit.runs, jit.linked, el, el.as_nanos() as f64 / jit.runs as f64);
    }

    #[test]
    fn exceptions_in_blocks_and_delay_slots() {
        // LW r3, 1(r9) (misaligned: AdEL) ... the handler at 8000_0180 is a loop
        let mut ram = Ram(vec![0; 0x40_0000]);
        let code = [i(0x09, 0, 2, 5), i(0x23, 9, 3, 1), i(0x09, 0, 4, 7)];
        for (k, w) in code.iter().enumerate() { ram.write32(0x1000 + 4 * k as u32, *w); }
        ram.write32(0x180, 2 << 26 | (0x8000_0180u32 >> 2 & 0x03FF_FFFF));
        let mut c = R5900::new();
        c.cop0[STATUS] = 1 << 30;
        c.gpr[9] = 0x8010_0000;
        c.jump(BASE);
        let mut jit = Jit::new();
        jit.hot = 1;
        c.run_jit(&mut ram, &mut jit, 1);
        assert_eq!((c.gpr[2], c.gpr[4]), (5, 0));
        assert_eq!(c.pc, 0x8000_0180);
        assert_eq!(c.cop0[EPC], BASE + 4);
        assert_eq!(c.cop0[CAUSE] >> 31, 0);
        // the same in a delay slot: EPC is the branch, Cause.BD set
        let code = [i(0x04, 0, 0, 4), i(0x23, 9, 3, 1)];
        for (k, w) in code.iter().enumerate() { ram.write32(0x2000 + 4 * k as u32, *w); }
        c.cop0[STATUS] = 1 << 30;
        c.jump(0x8000_2000);
        c.run_jit(&mut ram, &mut jit, 1);
        assert_eq!(c.pc, 0x8000_0180);
        assert_eq!(c.cop0[EPC], 0x8000_2000);
        assert_eq!(c.cop0[CAUSE] >> 31, 1);
    }
}
