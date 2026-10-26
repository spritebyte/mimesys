//! Board test with hand-assembled T-11 and 6502 programs (no real ROMs needed).
use std::collections::HashMap;
use crate::arcade::atarisy2::sy2_system::{Sy2System, Variant};
use crate::common::system::System;

/// T-11 program builder (words, little endian), placed at 0x8000.
struct Asm { w: Vec<u16> }
impl Asm {
    fn at(&self) -> u16 { 0x8000 + 2 * self.w.len() as u16 }
    fn op(&mut self, words: &[u16]) -> &mut Self { self.w.extend_from_slice(words); self }
    fn mov_imm_abs(&mut self, imm: u16, addr: u16) -> &mut Self { self.op(&[0o012737, imm, addr]) }
    fn mov_abs_abs(&mut self, src: u16, dst: u16) -> &mut Self { self.op(&[0o013737, src, dst]) }
    fn tst_abs(&mut self, addr: u16) -> &mut Self { self.op(&[0o005737, addr]) }
    fn inc_abs(&mut self, addr: u16) -> &mut Self { self.op(&[0o005237, addr]) }
}

fn machine() -> Sy2System {
    let mut a = Asm { w: vec![] };
    a.op(&[0o000137, 0x8400]);                 // JMP @#8400 (leave the slapstic window)
    while a.at() < 0x8300 { a.op(&[0]); }
    // CP1 handler at 0x8300: copy the reply to RAM 0x200, count calls at 0x202
    a.mov_abs_abs(0x1C00, 0x0200).inc_abs(0x0202).op(&[0o000002]); // RTI
    while a.at() < 0x8400 { a.op(&[0]); }
    a.op(&[0o012706, 0x1000])                  // MOV #1000, SP
        .tst_abs(0x8000).tst_abs(0x8020)       // slapstic: bank 0 (alphanumerics at 0x2000)
        .mov_imm_abs(0xFFFF, 0x1082)           // palette 65 = white
        .mov_imm_abs(0x0001, 0x2000)           // alpha tile (0,0) = char 1, colour 0
        .mov_imm_abs(0x8300, 0o064)            // CP1 vector
        .mov_imm_abs(0o340, 0o066)
        .mov_imm_abs(0x0002, 0x1600)           // enable the "sound replied" interrupt
        .mov_imm_abs(0x0000, 0x15A0)           // let the sound CPU run
        .mov_imm_abs(0x0042, 0x1680)           // command 0x42
        .op(&[0o106427, 0])                    // MTPS #0
        .op(&[0o000777]);                      // BR .
    let mut main = vec![0xFFu8; 0x90000];
    for (i, w) in a.w.iter().enumerate() {
        main[0x8000 + i * 2] = *w as u8;
        main[0x8001 + i * 2] = (*w >> 8) as u8;
    }

    // 6502: NMI reads the command, replies with command + 1; IRQ just acknowledges
    let mut snd = vec![0xEAu8; 0x10000];
    // LDX #FF TXS; POKEY 1: SKCTL=3, AUDF1=$40, AUDC1=$AF (pure tone, vol 15); mixer=$FF; CLI; JMP *
    // then YM2151: channel 0 left only, algorithm 7, M1 loud with instant attack, key on M1
    let mut reset = vec![0xA2, 0xFF, 0x9A,
                 0xA9, 0x03, 0x8D, 0x0F, 0x18, 0xA9, 0x40, 0x8D, 0x00, 0x18, 0xA9, 0xAF, 0x8D, 0x01, 0x18,
                 0xA9, 0xFF, 0x8D, 0x7A, 0x18];
    for (a, d) in [(0x20u8, 0x47u8), (0x28, 0x4A), (0x60, 0x00), (0x80, 0x1F), (0x08, 0x08)] {
        reset.extend_from_slice(&[0xA9, a, 0x8D, 0x50, 0x18, 0xA9, d, 0x8D, 0x51, 0x18]);
    }
    let here = 0xC000 + reset.len() as u16 + 1;
    reset.extend_from_slice(&[0x58, 0x4C, here as u8, (here >> 8) as u8]);
    let nmi = [0xAD, 0x60, 0x18, 0x18, 0x69, 0x01, 0x8D, 0x74, 0x18, 0x40];
    let irq = [0x8D, 0x78, 0x18, 0x40];
    snd[0xC000..0xC000 + reset.len()].copy_from_slice(&reset);
    snd[0xC100..0xC100 + nmi.len()].copy_from_slice(&nmi);
    snd[0xC200..0xC200 + irq.len()].copy_from_slice(&irq);
    snd[0xFFFA..].copy_from_slice(&[0x00, 0xC1, 0x00, 0xC0, 0x00, 0xC2]);

    let mut chars = vec![0u8; 0x2000];
    for b in &mut chars[16..32] { *b = 0x0F; }  // char 1: every pixel pen 1
    let mut regions = HashMap::new();
    regions.insert("maincpu".to_string(), main);
    regions.insert("audiocpu".to_string(), snd);
    regions.insert("tiles".to_string(), vec![0; 0x20000]);
    regions.insert("sprites".to_string(), vec![0; 0x40000]);
    regions.insert("chars".to_string(), chars);
    Sy2System::new(Variant::Paperboy, "test", regions).unwrap()
}

#[test]
fn command_reply_interrupt_and_alpha_layer() {
    let mut m = machine();
    for _ in 0..3 { m.run_frame(0); }
    let ram = &m.board.ram;
    assert_eq!(ram[0x200], 0x43, "6502 reply reached the T-11 through the CP1 interrupt");
    assert_eq!(ram[0x202], 1, "interrupt taken once");
    assert_eq!(m.board.slapstic.current_bank, 0);
    assert_eq!(&m.framebuffer()[0..4], &[240, 240, 240, 255]);
    assert_eq!(&m.framebuffer()[8 * 4..8 * 4 + 4], &[0, 0, 0, 255], "next char cell is blank");
    let au = m.drain_audio();
    assert!(au.len() > 2 * 2000, "about 735 stereo samples a frame");
    let energy = |side: usize| au.iter().skip(side).step_by(2).map(|s| s.abs()).sum::<f32>();
    assert!(energy(0) > 50.0, "POKEY 1 tone on the left: {}", energy(0));
    assert!(energy(1) < 1.0, "POKEY 2 and the YM2151 silent on the right: {}", energy(1));
    assert!(m.board.chips.ym_out[0] != 0.0, "YM2151 channel 0 sounding on the left");
}

#[test]
fn paperboy_round_trip() {
    let mut m = machine();
    crate::arcade::state::check_round_trip(&mut m, 2, 5);
}

#[test]
fn playfield_and_motion_object() {
    let mut main = vec![0xFFu8; 0x90000];
    main[0x8000..0x8002].copy_from_slice(&0o000777u16.to_le_bytes());   // BR .
    let mut tiles = vec![0u8; 0x20000];
    for b in &mut tiles[16..32] { *b = 0x0F; }                 // tile 1: pen 5 (planes 1 and 3)
    for b in &mut tiles[0x10000 + 16..0x10000 + 32] { *b = 0x0F; }
    let mut sprites = vec![0u8; 0x40000];                      // all zero = transparent (inverted ROM)
    for b in &mut sprites[2 * 64..3 * 64] { *b = 0xFF; }       // tile 2: pen 3
    let mut regions = HashMap::new();
    regions.insert("maincpu".to_string(), main);
    regions.insert("audiocpu".to_string(), vec![0; 0x10000]);
    regions.insert("tiles".to_string(), tiles);
    regions.insert("sprites".to_string(), sprites);
    regions.insert("chars".to_string(), vec![0; 0x2000]);
    let mut m = Sy2System::new(Variant::Paperboy, "test", regions).unwrap();
    let v = &mut m.board.video;
    for col in 0..128 { for row in 0..64 { v.pf_ram[row * 128 + col] = 0x0001; } }
    v.write_palette(128 + 5, 0x0F0F);                          // playfield pen 5: green
    v.write_palette(3, 0xF00F);                                // MO colour 0 pen 3: red
    v.mo_ram[0] = 396 << 6;                                    // Y: top edge on line 100
    v.mo_ram[1] = 2;                                           // tile 2, 1 tile high
    v.mo_ram[2] = 200 << 6;                                    // X = 200
    v.mo_ram[3] = 0;                                           // link to itself: end of list
    v.render_line(100);
    v.render_line(116);
    let px = |x: usize, y: usize| m.board.video.framebuffer[(y * 512 + x) * 4..(y * 512 + x) * 4 + 3].to_vec();
    assert_eq!(px(200, 100), vec![240, 0, 0]);
    assert_eq!(px(215, 100), vec![240, 0, 0]);
    assert_eq!(px(216, 100), vec![0, 240, 0]);
    assert_eq!(px(199, 100), vec![0, 240, 0]);
    assert_eq!(px(200, 116), vec![0, 240, 0], "object is 16 lines tall");
}

#[test]
fn sound_cpu_drives_the_speech_chip() {
    let mut m = {
        let mut main = vec![0xFFu8; 0x90000];
        // T-11: release the sound CPU (CLR @#15A0), then BR .
        let prog: [u16; 3] = [0o005037, 0x15A0, 0o000777];
        for (i, w) in prog.iter().enumerate() { main[0x8000 + i * 2..0x8002 + i * 2].copy_from_slice(&w.to_le_bytes()); }
        // 6502: LDA #$60; STA $1870 (data); STA $1873 (/WS low); wait until IN1 bit 2 (/READY) is low;
        // STA $1872 (/WS high); JMP *
        let mut snd = vec![0xEAu8; 0x10000];
        let code = [0xA9, 0x60, 0x8D, 0x70, 0x18, 0x8D, 0x73, 0x18,
                    0xAD, 0x40, 0x18, 0x29, 0x04, 0xD0, 0xF9,
                    0x8D, 0x72, 0x18, 0x4C, 0x12, 0xC0];
        snd[0xC000..0xC000 + code.len()].copy_from_slice(&code);
        snd[0xFFFA..].copy_from_slice(&[0x00, 0xC0, 0x00, 0xC0, 0x00, 0xC0]);
        let mut regions = HashMap::new();
        regions.insert("maincpu".to_string(), main);
        regions.insert("audiocpu".to_string(), snd);
        regions.insert("tiles".to_string(), vec![0; 0x20000]);
        regions.insert("sprites".to_string(), vec![0; 0x40000]);
        regions.insert("chars".to_string(), vec![0; 0x2000]);
        Sy2System::new(Variant::Paperboy, "test", regions).unwrap()
    };
    assert!(!m.board.chips.tms.in_speak_external());
    m.run_frame(0);
    assert!(m.board.chips.tms.in_speak_external(), "Speak External command reached the TMS5220");
}
