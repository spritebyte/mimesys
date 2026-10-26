//! Yamaha YM2151 (OPM): 8 channels x 4 operators FM, LFO, noise, 2 timers.
//! Port of Aaron Giles' ymfm (BSD-3-Clause), the OPM register set plus the shared
//! FM engine, sample for sample (verified against ymfm itself, see the tests).
//!
//! One `generate()` = one output sample = 64 input clocks (3.579545 MHz -> 55.9 kHz).
//! Timers count in samples (their periods are whole samples). The busy flag and
//! the IRQ line are left to the board.

use serde::{Deserialize, Serialize};

const EG_ATTACK: usize = 1;
const EG_DECAY: usize = 2;
const EG_SUSTAIN: usize = 3;
const EG_RELEASE: usize = 4;
const EG_QUIET: u16 = 0x380;
const PHASE_STEP_DYNAMIC: u32 = 1;

const KEYON_NORMAL: u32 = 0;
const KEYON_CSM: u32 = 2;

pub const STATUS_TIMERA: u8 = 0x01;
pub const STATUS_TIMERB: u8 = 0x02;
pub const STATUS_BUSY: u8 = 0x80;

#[inline] fn bf(v: u32, start: u32, len: u32) -> u32 { (v >> start) & ((1 << len) - 1) }

static SIN_TABLE: [u16; 256] = [
    0x859,0x6c3,0x607,0x58b,0x52e,0x4e4,0x4a6,0x471,0x443,0x41a,0x3f5,0x3d3,0x3b5,0x398,0x37e,0x365,
    0x34e,0x339,0x324,0x311,0x2ff,0x2ed,0x2dc,0x2cd,0x2bd,0x2af,0x2a0,0x293,0x286,0x279,0x26d,0x261,
    0x256,0x24b,0x240,0x236,0x22c,0x222,0x218,0x20f,0x206,0x1fd,0x1f5,0x1ec,0x1e4,0x1dc,0x1d4,0x1cd,
    0x1c5,0x1be,0x1b7,0x1b0,0x1a9,0x1a2,0x19b,0x195,0x18f,0x188,0x182,0x17c,0x177,0x171,0x16b,0x166,
    0x160,0x15b,0x155,0x150,0x14b,0x146,0x141,0x13c,0x137,0x133,0x12e,0x129,0x125,0x121,0x11c,0x118,
    0x114,0x10f,0x10b,0x107,0x103,0x0ff,0x0fb,0x0f8,0x0f4,0x0f0,0x0ec,0x0e9,0x0e5,0x0e2,0x0de,0x0db,
    0x0d7,0x0d4,0x0d1,0x0cd,0x0ca,0x0c7,0x0c4,0x0c1,0x0be,0x0bb,0x0b8,0x0b5,0x0b2,0x0af,0x0ac,0x0a9,
    0x0a7,0x0a4,0x0a1,0x09f,0x09c,0x099,0x097,0x094,0x092,0x08f,0x08d,0x08a,0x088,0x086,0x083,0x081,
    0x07f,0x07d,0x07a,0x078,0x076,0x074,0x072,0x070,0x06e,0x06c,0x06a,0x068,0x066,0x064,0x062,0x060,
    0x05e,0x05c,0x05b,0x059,0x057,0x055,0x053,0x052,0x050,0x04e,0x04d,0x04b,0x04a,0x048,0x046,0x045,
    0x043,0x042,0x040,0x03f,0x03e,0x03c,0x03b,0x039,0x038,0x037,0x035,0x034,0x033,0x031,0x030,0x02f,
    0x02e,0x02d,0x02b,0x02a,0x029,0x028,0x027,0x026,0x025,0x024,0x023,0x022,0x021,0x020,0x01f,0x01e,
    0x01d,0x01c,0x01b,0x01a,0x019,0x018,0x017,0x017,0x016,0x015,0x014,0x014,0x013,0x012,0x011,0x011,
    0x010,0x00f,0x00f,0x00e,0x00d,0x00d,0x00c,0x00c,0x00b,0x00a,0x00a,0x009,0x009,0x008,0x008,0x007,
    0x007,0x007,0x006,0x006,0x005,0x005,0x005,0x004,0x004,0x004,0x003,0x003,0x003,0x002,0x002,0x002,
    0x002,0x001,0x001,0x001,0x001,0x001,0x001,0x001,0x000,0x000,0x000,0x000,0x000,0x000,0x000,0x000,
];

static POWER_RAW: [u16; 256] = [
    0x3fa,0x3f5,0x3ef,0x3ea,0x3e4,0x3df,0x3da,0x3d4,0x3cf,0x3c9,0x3c4,0x3bf,0x3b9,0x3b4,0x3ae,0x3a9,
    0x3a4,0x39f,0x399,0x394,0x38f,0x38a,0x384,0x37f,0x37a,0x375,0x370,0x36a,0x365,0x360,0x35b,0x356,
    0x351,0x34c,0x347,0x342,0x33d,0x338,0x333,0x32e,0x329,0x324,0x31f,0x31a,0x315,0x310,0x30b,0x306,
    0x302,0x2fd,0x2f8,0x2f3,0x2ee,0x2e9,0x2e5,0x2e0,0x2db,0x2d6,0x2d2,0x2cd,0x2c8,0x2c4,0x2bf,0x2ba,
    0x2b5,0x2b1,0x2ac,0x2a8,0x2a3,0x29e,0x29a,0x295,0x291,0x28c,0x288,0x283,0x27f,0x27a,0x276,0x271,
    0x26d,0x268,0x264,0x25f,0x25b,0x257,0x252,0x24e,0x249,0x245,0x241,0x23c,0x238,0x234,0x230,0x22b,
    0x227,0x223,0x21e,0x21a,0x216,0x212,0x20e,0x209,0x205,0x201,0x1fd,0x1f9,0x1f5,0x1f0,0x1ec,0x1e8,
    0x1e4,0x1e0,0x1dc,0x1d8,0x1d4,0x1d0,0x1cc,0x1c8,0x1c4,0x1c0,0x1bc,0x1b8,0x1b4,0x1b0,0x1ac,0x1a8,
    0x1a4,0x1a0,0x19c,0x199,0x195,0x191,0x18d,0x189,0x185,0x181,0x17e,0x17a,0x176,0x172,0x16f,0x16b,
    0x167,0x163,0x160,0x15c,0x158,0x154,0x151,0x14d,0x149,0x146,0x142,0x13e,0x13b,0x137,0x134,0x130,
    0x12c,0x129,0x125,0x122,0x11e,0x11b,0x117,0x114,0x110,0x10c,0x109,0x106,0x102,0x0ff,0x0fb,0x0f8,
    0x0f4,0x0f1,0x0ed,0x0ea,0x0e7,0x0e3,0x0e0,0x0dc,0x0d9,0x0d6,0x0d2,0x0cf,0x0cc,0x0c8,0x0c5,0x0c2,
    0x0be,0x0bb,0x0b8,0x0b5,0x0b1,0x0ae,0x0ab,0x0a8,0x0a4,0x0a1,0x09e,0x09b,0x098,0x094,0x091,0x08e,
    0x08b,0x088,0x085,0x082,0x07e,0x07b,0x078,0x075,0x072,0x06f,0x06c,0x069,0x066,0x063,0x060,0x05d,
    0x05a,0x057,0x054,0x051,0x04e,0x04b,0x048,0x045,0x042,0x03f,0x03c,0x039,0x036,0x033,0x030,0x02d,
    0x02a,0x028,0x025,0x022,0x01f,0x01c,0x019,0x016,0x014,0x011,0x00e,0x00b,0x008,0x006,0x003,0x000,
];

static INCREMENT_TABLE: [u32; 64] = [
    0x00000000, 0x00000000, 0x10101010, 0x10101010,
    0x10101010, 0x10101010, 0x11101110, 0x11101110,
    0x10101010, 0x10111010, 0x11101110, 0x11111110,
    0x10101010, 0x10111010, 0x11101110, 0x11111110,
    0x10101010, 0x10111010, 0x11101110, 0x11111110,
    0x10101010, 0x10111010, 0x11101110, 0x11111110,
    0x10101010, 0x10111010, 0x11101110, 0x11111110,
    0x10101010, 0x10111010, 0x11101110, 0x11111110,
    0x10101010, 0x10111010, 0x11101110, 0x11111110,
    0x10101010, 0x10111010, 0x11101110, 0x11111110,
    0x10101010, 0x10111010, 0x11101110, 0x11111110,
    0x10101010, 0x10111010, 0x11101110, 0x11111110,
    0x11111111, 0x21112111, 0x21212121, 0x22212221,
    0x22222222, 0x42224222, 0x42424242, 0x44424442,
    0x44444444, 0x84448444, 0x84848484, 0x88848884,
    0x88888888, 0x88888888, 0x88888888, 0x88888888,
];

static DETUNE: [[u8; 4]; 32] = [
    [0,0,1,2],[0,0,1,2],[0,0,1,2],[0,0,1,2],[0,1,2,2],[0,1,2,3],[0,1,2,3],[0,1,2,3],
    [0,1,2,4],[0,1,3,4],[0,1,3,4],[0,1,3,5],[0,2,4,5],[0,2,4,6],[0,2,4,6],[0,2,5,7],
    [0,2,5,8],[0,3,6,8],[0,3,6,9],[0,3,7,10],[0,4,8,11],[0,4,8,12],[0,4,9,13],[0,5,10,14],
    [0,5,11,16],[0,6,12,17],[0,6,13,19],[0,7,14,20],[0,8,16,22],[0,8,16,22],[0,8,16,22],[0,8,16,22],
];

static PHASE_STEP: [u32; 768] = [
    41568,41600,41632,41664,41696,41728,41760,41792,41856,41888,41920,41952,42016,42048,42080,42112,
    42176,42208,42240,42272,42304,42336,42368,42400,42464,42496,42528,42560,42624,42656,42688,42720,
    42784,42816,42848,42880,42912,42944,42976,43008,43072,43104,43136,43168,43232,43264,43296,43328,
    43392,43424,43456,43488,43552,43584,43616,43648,43712,43744,43776,43808,43872,43904,43936,43968,
    44032,44064,44096,44128,44192,44224,44256,44288,44352,44384,44416,44448,44512,44544,44576,44608,
    44672,44704,44736,44768,44832,44864,44896,44928,44992,45024,45056,45088,45152,45184,45216,45248,
    45312,45344,45376,45408,45472,45504,45536,45568,45632,45664,45728,45760,45792,45824,45888,45920,
    45984,46016,46048,46080,46144,46176,46208,46240,46304,46336,46368,46400,46464,46496,46528,46560,
    46656,46688,46720,46752,46816,46848,46880,46912,46976,47008,47072,47104,47136,47168,47232,47264,
    47328,47360,47392,47424,47488,47520,47552,47584,47648,47680,47744,47776,47808,47840,47904,47936,
    48032,48064,48096,48128,48192,48224,48288,48320,48384,48416,48448,48480,48544,48576,48640,48672,
    48736,48768,48800,48832,48896,48928,48992,49024,49088,49120,49152,49184,49248,49280,49344,49376,
    49440,49472,49504,49536,49600,49632,49696,49728,49792,49824,49856,49888,49952,49984,50048,50080,
    50144,50176,50208,50240,50304,50336,50400,50432,50496,50528,50560,50592,50656,50688,50752,50784,
    50880,50912,50944,50976,51040,51072,51136,51168,51232,51264,51328,51360,51424,51456,51488,51520,
    51616,51648,51680,51712,51776,51808,51872,51904,51968,52000,52064,52096,52160,52192,52224,52256,
    52384,52416,52448,52480,52544,52576,52640,52672,52736,52768,52832,52864,52928,52960,52992,53024,
    53120,53152,53216,53248,53312,53344,53408,53440,53504,53536,53600,53632,53696,53728,53792,53824,
    53920,53952,54016,54048,54112,54144,54208,54240,54304,54336,54400,54432,54496,54528,54592,54624,
    54688,54720,54784,54816,54880,54912,54976,55008,55072,55104,55168,55200,55264,55296,55360,55392,
    55488,55520,55584,55616,55680,55712,55776,55808,55872,55936,55968,56032,56064,56128,56160,56224,
    56288,56320,56384,56416,56480,56512,56576,56608,56672,56736,56768,56832,56864,56928,56960,57024,
    57120,57152,57216,57248,57312,57376,57408,57472,57536,57568,57632,57664,57728,57792,57824,57888,
    57952,57984,58048,58080,58144,58208,58240,58304,58368,58400,58464,58496,58560,58624,58656,58720,
    58784,58816,58880,58912,58976,59040,59072,59136,59200,59232,59296,59328,59392,59456,59488,59552,
    59648,59680,59744,59776,59840,59904,59936,60000,60064,60128,60160,60224,60288,60320,60384,60416,
    60512,60544,60608,60640,60704,60768,60800,60864,60928,60992,61024,61088,61152,61184,61248,61280,
    61376,61408,61472,61536,61600,61632,61696,61760,61824,61856,61920,61984,62048,62080,62144,62208,
    62272,62304,62368,62432,62496,62528,62592,62656,62720,62752,62816,62880,62944,62976,63040,63104,
    63200,63232,63296,63360,63424,63456,63520,63584,63648,63680,63744,63808,63872,63904,63968,64032,
    64096,64128,64192,64256,64320,64352,64416,64480,64544,64608,64672,64704,64768,64832,64896,64928,
    65024,65056,65120,65184,65248,65312,65376,65408,65504,65536,65600,65664,65728,65792,65856,65888,
    65984,66016,66080,66144,66208,66272,66336,66368,66464,66496,66560,66624,66688,66752,66816,66848,
    66944,66976,67040,67104,67168,67232,67296,67328,67424,67456,67520,67584,67648,67712,67776,67808,
    67904,67936,68000,68064,68128,68192,68256,68288,68384,68448,68512,68544,68640,68672,68736,68800,
    68896,68928,68992,69056,69120,69184,69248,69280,69376,69440,69504,69536,69632,69664,69728,69792,
    69920,69952,70016,70080,70144,70208,70272,70304,70400,70464,70528,70560,70656,70688,70752,70816,
    70912,70976,71040,71104,71136,71232,71264,71360,71424,71488,71552,71616,71648,71744,71776,71872,
    71968,72032,72096,72160,72192,72288,72320,72416,72480,72544,72608,72672,72704,72800,72832,72928,
    72992,73056,73120,73184,73216,73312,73344,73440,73504,73568,73632,73696,73728,73824,73856,73952,
    74080,74144,74208,74272,74304,74400,74432,74528,74592,74656,74720,74784,74816,74912,74944,75040,
    75136,75200,75264,75328,75360,75456,75488,75584,75648,75712,75776,75840,75872,75968,76000,76096,
    76224,76288,76352,76416,76448,76544,76576,76672,76736,76800,76864,76928,77024,77120,77152,77248,
    77344,77408,77472,77536,77568,77664,77696,77792,77856,77920,77984,78048,78144,78240,78272,78368,
    78464,78528,78592,78656,78688,78784,78816,78912,78976,79040,79104,79168,79264,79360,79392,79488,
    79616,79680,79744,79808,79840,79936,79968,80064,80128,80192,80256,80320,80416,80512,80544,80640,
    80768,80832,80896,80960,80992,81088,81120,81216,81280,81344,81408,81472,81568,81664,81696,81792,
    81952,82016,82080,82144,82176,82272,82304,82400,82464,82528,82592,82656,82752,82848,82880,82976,
];

/// 4-operator algorithms: op2in, op3in, op4in (sources), op1out, op2out, op3out
const fn alg(o2: u16, o3: u16, o4: u16, a: u16, b: u16, c: u16) -> u16 {
    o2 | (o3 << 1) | (o4 << 4) | (a << 7) | (b << 8) | (c << 9)
}
static ALGORITHM_OPS: [u16; 8] = [
    alg(1,2,3, 0,0,0), alg(0,5,3, 0,0,0), alg(0,2,6, 0,0,0), alg(1,0,7, 0,0,0),
    alg(1,0,3, 0,1,0), alg(1,1,1, 0,1,1), alg(1,0,0, 0,1,1), alg(0,0,0, 1,1,1),
];

fn abs_sin_attenuation(input: u32) -> u32 {
    let i = if bf(input, 8, 1) != 0 { !input } else { input };
    SIN_TABLE[(i & 0xFF) as usize] as u32
}

#[inline]
fn attenuation_to_volume(input: u32) -> u32 {
    let v = ((POWER_RAW[(input & 0xFF) as usize] as u32) | 0x400) << 2;
    v >> (input >> 8)
}

fn attenuation_increment(rate: u32, index: u32) -> u32 { bf(INCREMENT_TABLE[rate as usize], 4 * index, 4) }

fn detune_adjustment(detune: u32, keycode: u32) -> i32 {
    let r = DETUNE[keycode as usize][(detune & 3) as usize] as i32;
    if bf(detune, 2, 1) != 0 { -r } else { r }
}

fn effective_rate(raw: u32, ksr: u32) -> u8 { if raw == 0 { 0 } else { (raw + ksr).min(63) as u8 } }

fn opm_key_code_to_phase_step(block_freq: u32, delta: i32) -> u32 {
    let mut block = bf(block_freq, 10, 3);
    let adjusted = bf(block_freq, 6, 4) - bf(block_freq, 8, 2);
    let mut eff = ((adjusted << 6) | bf(block_freq, 0, 6)) as i32;
    eff += delta;
    if eff as u32 >= 768 {
        if eff < 0 {
            eff += 768;
            if block == 0 { return PHASE_STEP[0] >> 7; }
            block -= 1;
        } else {
            eff -= 768;
            if eff >= 768 { block += 1; eff -= 768; }
            if block >= 7 { return PHASE_STEP[767]; }
            block += 1;
        }
    }
    PHASE_STEP[eff as usize] >> (block ^ 7)
}

/// roundtrip through the YM3012 DAC's floating point format
fn roundtrip_fp(value: i32) -> i32 {
    if value < -32768 { return -32768; }
    if value > 32767 { return 32767; }
    let scan = value ^ (value >> 31);
    let lz = if (scan << 17) == 0 { 32 } else { ((scan << 17) as u32).leading_zeros() as i32 };
    let exponent = (7 - lz).max(1) - 1;
    let mask = (1 << exponent) - 1;
    value & !mask
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Cache {
    phase_step: u32,
    total_level: u32,
    block_freq: u32,
    detune: i32,
    multiple: u32,
    eg_sustain: u32,
    eg_rate: [u8; 6],
}

#[derive(Clone, Serialize, Deserialize)]
struct Operator {
    choffs: u32,
    opoffs: u32,
    phase: u32,
    env_att: u16,
    env_state: usize,
    key_state: u8,
    keyon_live: u8,
    cache: Cache,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Channel {
    feedback: [i16; 2],
    feedback_in: i16,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Ym2151 {
    regs: Vec<u8>,
    lfo_counter: u32,
    noise_lfsr: u32,
    noise_counter: u8,
    noise_state: u8,
    lfo_am: u8,
    lfo_wave: Vec<Vec<i16>>,
    ops: Vec<Operator>,
    chans: [Channel; 8],
    env_counter: u32,
    status: u8,
    timer_running: [bool; 2],
    timer_left: [i64; 2],      // samples until expiry
    total_clocks: u8,
    active_channels: u32,
    modified_channels: u32,
    prepare_count: u32,
    address: u8,
    /// MAME reset_w: while false, writes are ignored; going true resets the chip.
    pub running: bool,
}

impl Default for Ym2151 { fn default() -> Self { Self::new() } }

impl Ym2151 {
    pub fn new() -> Self {
        let mut lfo_wave = vec![vec![0i16; 256]; 4];
        for i in 0..256u32 {
            let am = (i ^ 0xFF) as u8;
            let pm = i as u8;
            lfo_wave[0][i as usize] = (am as u16 | ((pm as u16) << 8)) as i16;
            let am: u8 = if bf(i, 7, 1) != 0 { 0 } else { 0xFF };
            let pm = am ^ 0x80;
            lfo_wave[1][i as usize] = (am as u16 | ((pm as u16) << 8)) as i16;
            let am: u8 = if bf(i, 7, 1) != 0 { (i << 1) as u8 } else { ((i ^ 0xFF) << 1) as u8 };
            let pm: u8 = if bf(i, 6, 1) != 0 { am } else { !am };
            lfo_wave[2][i as usize] = (am as u16 | ((pm as u16) << 8)) as i16;
        }
        let ops = (0..32).map(|i| Operator {
            choffs: i & 7, opoffs: i, phase: 0, env_att: 0x3FF, env_state: EG_RELEASE,
            key_state: 0, keyon_live: 0, cache: Cache::default(),
        }).collect();
        let mut y = Self {
            regs: vec![0; 256], lfo_counter: 0, noise_lfsr: 1, noise_counter: 0, noise_state: 0,
            lfo_am: 0, lfo_wave, ops, chans: Default::default(), env_counter: 0, status: 0,
            timer_running: [false; 2], timer_left: [0; 2], total_clocks: 0,
            active_channels: 0xFF, modified_channels: 0xFF, prepare_count: 0, address: 0, running: true,
        };
        y.reset();
        y
    }

    pub fn reset(&mut self) {
        self.status &= !0xFF;
        self.regs.iter_mut().for_each(|r| *r = 0);
        for r in 0x20..0x28 { self.regs[r] = 0xC0; }
        self.mode_write(0);
        for c in self.chans.iter_mut() { *c = Channel::default(); }
        for op in self.ops.iter_mut() {
            op.phase = 0; op.env_att = 0x3FF; op.env_state = EG_RELEASE; op.key_state = 0; op.keyon_live = 0;
        }
    }

    /// MAME ym2151_device::reset_w
    pub fn set_running(&mut self, on: bool) {
        if on == self.running { return; }
        self.running = on;
        if on { self.reset(); }
    }

    // ---- register fields ----
    fn byte(&self, off: u32, start: u32, len: u32, extra: u32) -> u32 { bf(self.regs[(off + extra) as usize] as u32, start, len) }
    fn noise_frequency(&self) -> u32 { self.byte(0x0F, 0, 5, 0) ^ 0x1F }
    fn noise_enable(&self) -> bool { self.byte(0x0F, 7, 1, 0) != 0 }
    fn lfo_reset(&self) -> bool { self.byte(0x01, 1, 1, 0) != 0 }
    fn lfo_rate(&self) -> u32 { self.byte(0x18, 0, 8, 0) }
    fn lfo_am_depth(&self) -> u32 { self.byte(0x19, 0, 7, 0) }
    fn lfo_pm_depth(&self) -> u32 { self.byte(0x1A, 0, 7, 0) }
    fn lfo_waveform(&self) -> usize { self.byte(0x1B, 0, 2, 0) as usize }
    fn ch_feedback(&self, c: u32) -> u32 { self.byte(0x20, 3, 3, c) }
    fn ch_algorithm(&self, c: u32) -> u32 { self.byte(0x20, 0, 3, c) }
    fn ch_block_freq(&self, c: u32) -> u32 { (self.byte(0x28, 0, 7, c) << 6) | self.byte(0x30, 2, 6, c) }
    fn ch_pm_sens(&self, c: u32) -> u32 { self.byte(0x38, 4, 3, c) }
    fn ch_am_sens(&self, c: u32) -> u32 { self.byte(0x38, 0, 2, c) }
    fn op_lfo_am_enable(&self, o: u32) -> bool { self.byte(0xA0, 7, 1, o) != 0 }

    fn lfo_am_offset(&self, c: u32) -> u32 {
        let s = self.ch_am_sens(c);
        if s == 0 { 0 } else { (self.lfo_am as u32) << (s - 1) }
    }

    fn cache_operator(&self, op: &Operator) -> Cache {
        let (c, o) = (op.choffs, op.opoffs);
        let block_freq = self.ch_block_freq(c);
        let keycode = bf(block_freq, 8, 5);
        let mut cache = Cache {
            block_freq,
            detune: detune_adjustment(self.byte(0x40, 4, 3, o), keycode),
            multiple: (self.byte(0x40, 0, 4, o) * 2).max(1),
            ..Default::default()
        };
        cache.phase_step = if self.lfo_pm_depth() == 0 || self.ch_pm_sens(c) == 0 {
            self.compute_phase_step(c, o, &cache, 0)
        } else { PHASE_STEP_DYNAMIC };
        cache.total_level = self.byte(0x60, 0, 7, o) << 3;
        let mut sl = self.byte(0xE0, 4, 4, o);
        sl |= (sl + 1) & 0x10;
        cache.eg_sustain = sl << 5;
        let ksrval = keycode >> (self.byte(0x80, 6, 2, o) ^ 3);
        cache.eg_rate[EG_ATTACK] = effective_rate(self.byte(0x80, 0, 5, o) * 2, ksrval);
        cache.eg_rate[EG_DECAY] = effective_rate(self.byte(0xA0, 0, 5, o) * 2, ksrval);
        cache.eg_rate[EG_SUSTAIN] = effective_rate(self.byte(0xC0, 0, 5, o) * 2, ksrval);
        cache.eg_rate[EG_RELEASE] = effective_rate(self.byte(0xE0, 0, 4, o) * 4 + 2, ksrval);
        cache
    }

    fn compute_phase_step(&self, c: u32, o: u32, cache: &Cache, lfo_raw_pm: i32) -> u32 {
        const DT2: [i32; 4] = [0, (600 * 64 + 50) / 100, (781 * 64 + 50) / 100, (950 * 64 + 50) / 100];
        let mut delta = DT2[self.byte(0xC0, 6, 2, o) as usize];
        let pm = self.ch_pm_sens(c);
        if pm != 0 {
            if pm < 6 { delta += lfo_raw_pm >> (6 - pm); }
            else { delta = delta.wrapping_add(((lfo_raw_pm as u32) << (pm - 5)) as i32); }
        }
        let step = opm_key_code_to_phase_step(cache.block_freq, delta).wrapping_add(cache.detune as u32);
        step.wrapping_mul(cache.multiple) >> 1
    }

    fn clock_noise_and_lfo(&mut self) -> i32 {
        let freq = self.noise_frequency();
        for _ in 0..2 {
            self.noise_lfsr <<= 1;
            self.noise_lfsr |= bf(self.noise_lfsr, 17, 1) ^ bf(self.noise_lfsr, 14, 1) ^ 1;
            let c = self.noise_counter;
            self.noise_counter = c.wrapping_add(1);
            if c as u32 >= freq {
                self.noise_counter = 0;
                self.noise_state = bf(self.noise_lfsr, 17, 1) as u8;
            }
        }
        let rate = self.lfo_rate();
        self.lfo_counter = self.lfo_counter.wrapping_add((0x10 | bf(rate, 0, 4)) << bf(rate, 4, 4));
        if self.lfo_reset() { self.lfo_counter = 0; }
        let lfo = bf(self.lfo_counter, 22, 8) as usize;
        let noise = bf(self.noise_lfsr, 17, 8) as u16;
        self.lfo_wave[3][(lfo + 1) & 0xFF] = (noise | (noise << 8)) as i16;
        let ampm = self.lfo_wave[self.lfo_waveform()][lfo] as i32;
        self.lfo_am = (((ampm & 0xFF) as u32 * self.lfo_am_depth()) >> 7) as u8;
        ((ampm >> 8) * self.lfo_pm_depth() as i32) >> 7
    }

    // ---- operators ----
    fn op_prepare(&mut self, i: usize) -> bool {
        let cache = self.cache_operator(&self.ops[i]);
        self.ops[i].cache = cache;
        let keystate = (self.ops[i].keyon_live != 0) as u8;
        let op = &mut self.ops[i];
        if keystate != op.key_state {
            op.key_state = keystate;
            if keystate != 0 { Self::start_attack(op); } else if op.env_state < EG_RELEASE { op.env_state = EG_RELEASE; }
        }
        op.keyon_live &= !(1 << KEYON_CSM);
        op.env_state != EG_RELEASE || op.env_att < EG_QUIET
    }

    fn start_attack(op: &mut Operator) {
        if op.env_state == EG_ATTACK { return; }
        op.env_state = EG_ATTACK;
        op.phase = 0;
        if op.cache.eg_rate[EG_ATTACK] >= 62 { op.env_att = 0; }
    }

    fn op_clock(&mut self, i: usize, env_counter: u32, lfo_raw_pm: i32) {
        if bf(env_counter, 0, 2) == 0 {
            let op = &mut self.ops[i];
            let ec = env_counter >> 2;
            if op.env_state == EG_ATTACK && op.env_att == 0 { op.env_state = EG_DECAY; }
            if op.env_state == EG_DECAY && op.env_att as u32 >= op.cache.eg_sustain { op.env_state = EG_SUSTAIN; }
            let rate = op.cache.eg_rate[op.env_state] as u32;
            let rate_shift = rate >> 2;
            let ec = ec.wrapping_shl(rate_shift);
            if bf(ec, 0, 11) == 0 {
                let relevant = bf(ec, if rate_shift <= 11 { 11 } else { rate_shift }, 3);
                let inc = attenuation_increment(rate, relevant);
                if op.env_state == EG_ATTACK {
                    if rate < 62 {
                        let a = op.env_att as i32;
                        op.env_att = (a + ((!a * inc as i32) >> 4)) as u16;
                    }
                } else {
                    op.env_att += inc as u16;
                    if op.env_att >= 0x400 { op.env_att = 0x3FF; }
                }
            }
        }
        let mut step = self.ops[i].cache.phase_step;
        if step == PHASE_STEP_DYNAMIC {
            let op = &self.ops[i];
            step = self.compute_phase_step(op.choffs, op.opoffs, &op.cache, lfo_raw_pm);
        }
        let op = &mut self.ops[i];
        op.phase = op.phase.wrapping_add(step);
    }

    fn envelope_attenuation(&self, i: usize, am_offset: u32) -> u32 {
        let op = &self.ops[i];
        let mut r = op.env_att as u32;
        if self.op_lfo_am_enable(op.opoffs) { r += am_offset; }
        r += op.cache.total_level;
        r.min(0x3FF)
    }

    fn compute_volume(&self, i: usize, phase: u32, am_offset: u32) -> i32 {
        if self.ops[i].env_att > EG_QUIET { return 0; }
        let p = phase & 0x3FF;
        let sin_att = abs_sin_attenuation(p) | (bf(p, 9, 1) << 15);
        let env = self.envelope_attenuation(i, am_offset) << 2;
        let r = attenuation_to_volume((sin_att & 0x7FFF) + env) as i32;
        if bf(sin_att, 15, 1) != 0 { -r } else { r }
    }

    fn compute_noise_volume(&self, i: usize, am_offset: u32) -> i32 {
        let r = ((self.envelope_attenuation(i, am_offset) ^ 0x3FF) << 1) as i32;
        if self.noise_state & 1 != 0 { -r } else { r }
    }

    fn op_phase(&self, i: usize) -> u32 { self.ops[i].phase >> 10 }

    /// Operators of channel c in slot order M1, C1, M2, C2.
    fn ch_ops(c: usize) -> [usize; 4] { [c, c + 16, c + 8, c + 24] }

    fn channel_output(&mut self, c: usize, out: &mut [i32; 2]) {
        let ops = Self::ch_ops(c);
        let co = c as u32;
        let am = self.lfo_am_offset(co);
        let fb = self.ch_feedback(co);
        let mut opmod = 0i32;
        if fb != 0 {
            let f = &self.chans[c].feedback;
            opmod = (f[0] as i32 + f[1] as i32) >> (10 - fb);
        }
        let op1 = self.compute_volume(ops[0], self.op_phase(ops[0]).wrapping_add(opmod as u32), am);
        self.chans[c].feedback_in = op1 as i16;
        let op1value = op1 as i16 as i32;
        let outbits = bf(self.regs[0x20 + c] as u32, 6, 2);
        if outbits == 0 { return; }

        let a = ALGORITHM_OPS[self.ch_algorithm(co) as usize] as u32;
        let mut opout = [0i16; 8];
        opout[1] = op1value as i16;
        let m = (opout[bf(a, 0, 1) as usize] as i32) >> 1;
        opout[2] = self.compute_volume(ops[1], self.op_phase(ops[1]).wrapping_add(m as u32), am) as i16;
        opout[5] = opout[1].wrapping_add(opout[2]);
        let m = (opout[bf(a, 1, 3) as usize] as i32) >> 1;
        opout[3] = self.compute_volume(ops[2], self.op_phase(ops[2]).wrapping_add(m as u32), am) as i16;
        opout[6] = opout[1].wrapping_add(opout[3]);
        opout[7] = opout[2].wrapping_add(opout[3]);
        let mut result = if self.noise_enable() && c == 7 {
            self.compute_noise_volume(ops[3], am)
        } else {
            let m = (opout[bf(a, 4, 3) as usize] as i32) >> 1;
            self.compute_volume(ops[3], self.op_phase(ops[3]).wrapping_add(m as u32), am)
        };
        let (clipmax, clipmin) = (32767, -32768);
        if bf(a, 7, 1) != 0 { result = (result + opout[1] as i32).clamp(clipmin, clipmax); }
        if bf(a, 8, 1) != 0 { result = (result + opout[2] as i32).clamp(clipmin, clipmax); }
        if bf(a, 9, 1) != 0 { result = (result + opout[3] as i32).clamp(clipmin, clipmax); }
        if outbits & 1 != 0 { out[0] += result; }
        if outbits & 2 != 0 { out[1] += result; }
    }

    // ---- engine ----
    fn engine_clock(&mut self) {
        self.total_clocks = self.total_clocks.wrapping_add(1);
        let pc = self.prepare_count;
        self.prepare_count += 1;
        if self.modified_channels != 0 || pc >= 4096 {
            self.active_channels = 0;
            for c in 0..8 {
                let mut active = false;
                for &o in &Self::ch_ops(c) { if self.op_prepare(o) { active = true; } }
                if active { self.active_channels |= 1 << c; }
            }
            self.modified_channels = 0;
            self.prepare_count = 0;
        }
        self.env_counter = self.env_counter.wrapping_add(1);
        if bf(self.env_counter, 0, 2) == 3 { self.env_counter = self.env_counter.wrapping_add(1); }
        let pm = self.clock_noise_and_lfo();
        let ec = self.env_counter;
        for c in 0..8 {
            let ch = &mut self.chans[c];
            ch.feedback[0] = ch.feedback[1];
            ch.feedback[1] = ch.feedback_in;
            for &o in &Self::ch_ops(c) { self.op_clock(o, ec, pm); }
        }
    }

    /// One output sample (left, right), as ymfm produces it.
    pub fn generate(&mut self) -> (i32, i32) {
        self.clock_timers();
        self.engine_clock();
        let mut out = [0i32; 2];
        for c in 0..8 {
            if self.active_channels & (1 << c) != 0 { self.channel_output(c, &mut out); }
        }
        (roundtrip_fp(out[0]), roundtrip_fp(out[1]))
    }

    fn clock_timers(&mut self) {
        for t in 0..2 {
            if !self.timer_running[t] { continue; }
            self.timer_left[t] -= 1;
            if self.timer_left[t] <= 0 { self.timer_expired(t); }
        }
    }

    fn timer_expired(&mut self, t: usize) {
        let mode = self.regs[0x14];
        if t == 0 && mode & 0x04 != 0 { self.status |= STATUS_TIMERA; }
        else if t == 1 && mode & 0x08 != 0 { self.status |= STATUS_TIMERB; }
        if t == 0 && mode & 0x80 != 0 {
            for c in 0..8 {
                for &o in &Self::ch_ops(c) { self.ops[o].keyon_live |= 1 << KEYON_CSM; }
                self.modified_channels |= 1 << c;
            }
        }
        self.timer_running[t] = false;
        self.update_timer(t, true, 0);
    }

    fn update_timer(&mut self, t: usize, enable: bool, delta: i64) {
        if enable && !self.timer_running[t] {
            let period = if t == 0 {
                1024 - ((self.regs[0x10] as i64) << 2 | (self.regs[0x11] & 3) as i64)
            } else {
                16 * (256 - self.regs[0x12] as i64)
            };
            self.timer_left[t] = (period + delta).max(1);
            self.timer_running[t] = true;
        } else if !enable {
            self.timer_running[t] = false;
        }
    }

    fn mode_write(&mut self, data: u8) {
        self.modified_channels = 0xFF;
        self.regs[0x14] = data;
        let mut reset = 0u8;
        if data & 0x20 != 0 { reset |= STATUS_TIMERB; }
        if data & 0x10 != 0 { reset |= STATUS_TIMERA; }
        self.status &= !(reset | STATUS_BUSY);
        let d = -((self.total_clocks & 15) as i64);
        self.update_timer(1, data & 0x02 != 0, d);
        self.update_timer(0, data & 0x01 != 0, 0);
    }

    fn write_reg(&mut self, reg: u8, data: u8) {
        if reg == 0x14 { self.mode_write(data); return; }
        self.modified_channels = 0xFF;
        if reg == 0x19 { self.regs[0x19 + (data >> 7) as usize] = data; }
        else if reg != 0x1A { self.regs[reg as usize] = data; }
        if reg == 0x08 {
            let ch = (data & 7) as usize;
            let mask = bf(data as u32, 3, 4);
            for (k, &o) in Self::ch_ops(ch).iter().enumerate() {
                let on = bf(mask, k as u32, 1) as u8;
                let op = &mut self.ops[o];
                op.keyon_live = (op.keyon_live & !(1 << KEYON_NORMAL)) | (on << KEYON_NORMAL);
            }
        }
    }

    /// Bus write: offset 0 = address, 1 = data.
    pub fn write(&mut self, offset: u8, data: u8) {
        if !self.running { return; }
        if offset & 1 == 0 { self.address = data; } else { let a = self.address; self.write_reg(a, data); }
    }

    /// Status without the busy bit (the board adds it).
    pub fn status(&self) -> u8 { self.status & !STATUS_BUSY }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Vectors from ymfm itself (tools/ym2151_ref): each line is
    /// `w <addr> <data>` or `s <count> <hash>` = the next <count> samples hash to <hash>.
    fn run_vectors(text: &str) -> usize {
        let mut y = Ym2151::new();
        let mut n = 0;
        for (lineno, line) in text.lines().enumerate() {
            let p: Vec<&str> = line.split_whitespace().collect();
            match p.first() {
                Some(&"w") => {
                    let a = u8::from_str_radix(p[1], 16).unwrap();
                    let d = u8::from_str_radix(p[2], 16).unwrap();
                    y.write(0, a);
                    y.write(1, d);
                }
                Some(&"s") => {
                    let count: usize = p[1].parse().unwrap();
                    let want = u64::from_str_radix(p[2], 16).unwrap();
                    let mut h: u64 = 0xcbf29ce484222325;
                    let mut last = (0, 0);
                    for _ in 0..count {
                        let (l, r) = y.generate();
                        last = (l, r);
                        for v in [l, r] { h = (h ^ (v as u32 as u64)).wrapping_mul(0x100000001b3); }
                    }
                    assert_eq!(h, want, "line {}: sample block differs (last {:?})", lineno + 1, last);
                    n += count;
                }
                _ => {}
            }
        }
        n
    }

    #[test]
    fn ym2151_matches_ymfm_sample() {
        let n = run_vectors(include_str!("../tests/data/ym2151_vectors.txt"));
        assert!(n > 100_000);
    }

    #[test]
    #[ignore]
    fn ym2151_matches_ymfm_full() {
        let path = std::env::var("YM2151_VECTORS").expect("YM2151_VECTORS=<file>");
        let n = run_vectors(&std::fs::read_to_string(path).unwrap());
        println!("{n} samples ok");
    }
}
