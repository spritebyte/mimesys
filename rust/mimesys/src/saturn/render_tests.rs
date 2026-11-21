//! VDP1 drawing and VDP2 line rendering, on hand-built VRAM
use super::vdp1::Vdp1;
use super::vdp2::Vdp2;

fn reg(v: &mut Vdp2, off: usize, val: u16) { v.regs[off / 2] = val; }
fn w16(m: &mut [u8], a: usize, v: u16) { m[a..a + 2].copy_from_slice(&v.to_be_bytes()); }
fn r16(m: &[u8], a: usize) -> u16 { u16::from_be_bytes([m[a], m[a + 1]]) }

/// A 320-dot screen, display on, CRAM colour 1 red, 2 green, 3 blue (RGB555)
fn vdp2() -> Vdp2 {
    let mut v = Vdp2::new(false);
    reg(&mut v, 0x00, 0x8000);
    w16(&mut v.cram, 2, 0x001F);
    w16(&mut v.cram, 4, 0x03E0);
    w16(&mut v.cram, 6, 0x7C00);
    v
}

fn line(v: &Vdp2, vdp1: &Vdp1, y: u32) -> Vec<[u8; 3]> {
    let mut out = vec![0u8; 320 * 4];
    v.render_line(y, vdp1, &mut out);
    out.chunks_exact(4).map(|p| [p[0], p[1], p[2]]).collect()
}

const RED: [u8; 3] = [255, 0, 0];
const GREEN: [u8; 3] = [0, 255, 0];
const BLUE: [u8; 3] = [0, 0, 255];

/// NBG0: 16 colours, 1x1 cells, 1-word names; plane A at page 1 (0x2000). Character 1
/// (at 0x20) is all colour 1; character 2 all colour 2. Names: column 1 = character 2.
fn nbg0_cells(v: &mut Vdp2) {
    reg(v, 0x20, 0x0001);          // NBG0 on
    reg(v, 0x28, 0x0000);          // 16 colours, cells, 1x1
    reg(v, 0x30, 0x8000);          // 1-word names
    reg(v, 0x40, 0x0101);          // planes A, B: page 1
    reg(v, 0x42, 0x0101);
    reg(v, 0xF8, 0x0001);          // priority 1
    for k in 0..32 { v.vram[0x20 + k] = 0x11; v.vram[0x40 + k] = 0x22; }
    for i in 0..64 * 64 { w16(&mut v.vram, 0x2000 + i * 2, if i % 64 == 1 { 2 } else { 1 }); }
}

#[test]
fn nbg0_cells_draw_with_their_palette() {
    let mut v = vdp2();
    nbg0_cells(&mut v);
    let l = line(&v, &Vdp1::new(), 0);
    assert_eq!(l[0], RED);
    assert_eq!(l[8], GREEN, "column 1 is character 2");
    assert_eq!(l[16], RED);
}

#[test]
fn nbg0_scrolls_by_whole_and_fractional_dots() {
    let mut v = vdp2();
    nbg0_cells(&mut v);
    reg(&mut v, 0x70, 4);                 // SCXIN0
    let l = line(&v, &Vdp1::new(), 0);
    assert_eq!((l[3], l[4], l[11], l[12]), (RED, GREEN, GREEN, RED));
    // zoom x2 (ZMXIN 0, ZMXDN 0x80: half a dot per pixel)
    reg(&mut v, 0x70, 0);
    reg(&mut v, 0x7A, 0x8000);
    let l = line(&v, &Vdp1::new(), 0);
    assert_eq!((l[15], l[16], l[31], l[32]), (RED, GREEN, GREEN, RED));
}

#[test]
fn transparent_dots_show_the_back_screen_and_priorities_stack() {
    let mut v = vdp2();
    nbg0_cells(&mut v);
    // character 1 transparent; the back screen blue at 0x7FFFE
    for k in 0..32 { v.vram[0x20 + k] = 0; }
    reg(&mut v, 0xAC, 0x0003);
    reg(&mut v, 0xAE, 0xFFFF);
    w16(&mut v.vram, 0x7FFFE, 0x7C00);
    let l = line(&v, &Vdp1::new(), 0);
    assert_eq!((l[0], l[8]), (BLUE, GREEN));
    // NBG1 under it: a 256-colour bitmap of colour 3 at 0x20000 (MPOFN), priority 1 vs 2
    reg(&mut v, 0x20, 0x0003);
    reg(&mut v, 0x28, 0x1200);            // NBG1: bitmap, 256 colours
    reg(&mut v, 0x3C, 0x0010);            // NBG1 map offset 1: 0x20000
    for k in 0..512 * 256 { v.vram[0x20000 + k] = 3; }
    reg(&mut v, 0xF8, 0x0201);            // NBG0 1, NBG1 2
    let l = line(&v, &Vdp1::new(), 0);
    assert_eq!((l[0], l[8]), (BLUE, BLUE), "NBG1 on top");
    reg(&mut v, 0xF8, 0x0102);
    let l = line(&v, &Vdp1::new(), 0);
    assert_eq!((l[0], l[8]), (BLUE, GREEN), "NBG0 on top, NBG1 through its holes");
}

#[test]
fn colour_calculation_and_offset() {
    let mut v = vdp2();
    nbg0_cells(&mut v);
    // the back screen black; NBG0 half-mixed with it (ratio 15: 16/32 each)
    reg(&mut v, 0xEC, 0x0001);
    reg(&mut v, 0x108, 15);
    let l = line(&v, &Vdp1::new(), 0);
    assert_eq!(l[0], [127, 0, 0]);
    // colour offset A on NBG0: +16 green, -256 red
    reg(&mut v, 0xEC, 0);
    reg(&mut v, 0x110, 1);
    reg(&mut v, 0x114, (-256i32 as u16) & 0x1FF);
    reg(&mut v, 0x116, 16);
    let l = line(&v, &Vdp1::new(), 0);
    assert_eq!(l[0], [0, 16, 0]);
}

/// A command at `a`: ctrl, pmod, colr, srca, size, then vertices
fn cmd(vdp1: &mut Vdp1, a: usize, ctrl: u16, pmod: u16, colr: u16, src: u16, size: u16, xy: &[i16]) {
    let m = &mut vdp1.vram;
    w16(m, a, ctrl);
    w16(m, a + 4, pmod);
    w16(m, a + 6, colr);
    w16(m, a + 8, src);
    w16(m, a + 10, size);
    for (k, &c) in xy.iter().enumerate() { w16(m, a + 12 + k * 2, c as u16); }
}

fn fb(vdp1: &Vdp1, x: usize, y: usize) -> u16 { r16(vdp1.display_fb(), (y * 512 + x) * 2) }

/// Draw the list at 0 and swap so it's on display
fn draw(vdp1: &mut Vdp1) {
    vdp1.write_reg(0x04, 1);
    vdp1.write_reg(0x02, 3);   // manual swap
    vdp1.frame_change();
}

#[test]
fn vdp1_polygon_fills_its_quad_inside_the_clip() {
    let mut d = Vdp1::new();
    cmd(&mut d, 0x00, 0x0009, 0, 0, 0, 0, &[0, 0, 0, 0, 100, 100]);           // system clip 100x100
    cmd(&mut d, 0x20, 0x0004, 0, 0x83E0, 0, 0, &[10, 10, 50, 10, 50, 30, 10, 30]);
    w16(&mut d.vram, 0x40, 0x8000);
    draw(&mut d);
    assert_eq!(fb(&d, 10, 10), 0x83E0);
    assert_eq!(fb(&d, 50, 30), 0x83E0);
    assert_eq!(fb(&d, 30, 20), 0x83E0);
    assert_eq!(fb(&d, 9, 20), 0);
    assert_eq!(fb(&d, 51, 20), 0);
    // a triangle-ish quad (D = C): no holes along its rows
    let mut d = Vdp1::new();
    cmd(&mut d, 0x00, 0x0004, 0, 0xFFFF, 0, 0, &[0, 0, 60, 5, 30, 50, 30, 50]);
    w16(&mut d.vram, 0x20, 0x8000);
    draw(&mut d);
    for y in 6..45 {
        let row: Vec<bool> = (0..64).map(|x| fb(&d, x, y) != 0).collect();
        let first = row.iter().position(|&b| b).unwrap();
        let last = row.iter().rposition(|&b| b).unwrap();
        assert!(row[first..=last].iter().all(|&b| b), "hole on row {y}");
    }
}

#[test]
fn vdp1_sprite_4bpp_bank_flip_and_end_code() {
    let mut d = Vdp1::new();
    // 8x2 texture at 0x1000: row 0 = 1 2 3 4 5 6 7 F, row 1 = 0 (transparent) then 1s
    d.vram[0x1000..0x1004].copy_from_slice(&[0x12, 0x34, 0x56, 0x7F]);
    d.vram[0x1004..0x1008].copy_from_slice(&[0x01, 0x11, 0x11, 0x11]);
    cmd(&mut d, 0x00, 0x0010, 0, 0x0130, 0x1000 / 8, 0x0102, &[20, 20]);    // h flip
    w16(&mut d.vram, 0x20, 0x8000);
    draw(&mut d);
    // flipped: x 20 is texel 7 (end code: the row stops there, nothing from 7 on)
    assert_eq!(fb(&d, 20, 20), 0, "end code dot");
    assert_eq!(fb(&d, 21, 20), 0x0137, "texel 6 = 7 | bank");
    assert_eq!(fb(&d, 27, 20), 0x0131);
    assert_eq!(fb(&d, 27, 21), 0, "texel 0 of row 1: transparent");
    assert_eq!(fb(&d, 26, 21), 0x0131);
}

#[test]
fn vdp1_scaled_sprite_and_half_transparency() {
    let mut d = Vdp1::new();
    // 8x1 RGB texture, all 0x801F (red), scaled to 16x4 at (0,0)-(15,3)
    for k in 0..8 { w16(&mut d.vram, 0x2000 + k * 2, 0x801F); }
    cmd(&mut d, 0x00, 0x0001, 5 << 3, 0, 0x2000 / 8, 0x0101, &[0, 0, 0, 0, 15, 3]);
    // a blue polygon over the left half, half-transparent
    cmd(&mut d, 0x20, 0x0004, 3, 0xFC00, 0, 0, &[0, 0, 7, 0, 7, 3, 0, 3]);
    w16(&mut d.vram, 0x40, 0x8000);
    draw(&mut d);
    assert_eq!(fb(&d, 15, 3), 0x801F);
    assert_eq!(fb(&d, 16, 3), 0);
    assert_eq!(fb(&d, 3, 1), 0x8000 | (15 << 10) | 15, "half red, half blue");
}

#[test]
fn sprite_layer_shows_over_the_scroll_screens() {
    let mut v = vdp2();
    nbg0_cells(&mut v);
    let mut d = Vdp1::new();
    // an RGB polygon, and a palette one (colour 3) with sprite type 0
    cmd(&mut d, 0x00, 0x0004, 0, 0xFC00, 0, 0, &[0, 0, 3, 0, 3, 0, 0, 0]);
    cmd(&mut d, 0x20, 0x0004, 0, 0x0003, 0, 0, &[4, 0, 7, 0, 7, 0, 4, 0]);
    w16(&mut d.vram, 0x40, 0x8000);
    draw(&mut d);
    reg(&mut v, 0xE0, 0x0020);           // type 0, RGB sprites on
    reg(&mut v, 0xF0, 0x0002);           // sprite priority 0 -> 2
    let l = line(&v, &d, 0);
    assert_eq!((l[0], l[4], l[8]), (BLUE, BLUE, GREEN));
    reg(&mut v, 0xF0, 0x0000);           // priority 0: hidden
    let l = line(&v, &d, 0);
    assert_eq!(l[0], RED);
}

#[test]
fn vdp1_erases_the_new_draw_buffer_each_frame() {
    let mut d = Vdp1::new();
    d.ewdr = 0x1234;
    d.ewlr = 0;
    d.ewrr = (2 << 9) | 1;                // x 0-15, y 0-1
    d.frame_change();                     // FCM 0: swap + erase
    let f = d.fb().clone();
    assert_eq!(r16(&f, 15 * 2), 0x1234);
    assert_eq!(r16(&f, 16 * 2), 0);
    assert_eq!(r16(&f, (512 + 3) * 2), 0x1234);
    assert_eq!(r16(&f, (2 * 512) * 2), 0);
}

#[test]
#[ignore]
fn bench_four_layers() {
    let mut v = vdp2();
    nbg0_cells(&mut v);
    reg(&mut v, 0x20, 0x000F);
    reg(&mut v, 0xF8, 0x0102);
    reg(&mut v, 0xFA, 0x0304);
    let d = Vdp1::new();
    let mut out = vec![0u8; 320 * 4];
    let t = std::time::Instant::now();
    for _ in 0..60 { for y in 0..224 { v.render_line(y, &d, &mut out); } }
    println!("60 frames, 4 layers: {:?}", t.elapsed());
}

#[test]
fn nbg0_line_scroll_and_vertical_cell_scroll() {
    let mut v = vdp2();
    nbg0_cells(&mut v);
    w16(&mut v.vram, 0x2000 + 64 * 2, 2);   // cell row 1, column 0: character 2 (green)
    // line scroll X, every line: table at 0x40000 (LSTA0 = 0x20000 / 2 ... in words: U=2, L=0)
    reg(&mut v, 0x9A, 0x0002);
    reg(&mut v, 0xA0, 0x0002);
    reg(&mut v, 0xA2, 0x0000);
    w16(&mut v.vram, 0x40000, 0);           // line 0: +0
    w16(&mut v.vram, 0x40004, 8);           // line 1: +8 dots
    let l0 = line(&v, &Vdp1::new(), 0);
    assert_eq!((l0[0], l0[8]), (RED, GREEN), "line 0 unscrolled");
    let l1 = line(&v, &Vdp1::new(), 1);
    assert_eq!((l1[0], l1[8]), (GREEN, RED), "line 1 scrolled 8 dots left by its table entry");
    // line scroll Y: line 0 reads 8 lines further down (cell row 1)
    reg(&mut v, 0x9A, 0x0004);
    w16(&mut v.vram, 0x40000, 8);
    let l0 = line(&v, &Vdp1::new(), 0);
    assert_eq!(l0[0], GREEN, "line 0 shows cell row 1 through the vertical line scroll");
    // vertical cell scroll: only screen column 0 moves down 8 lines
    reg(&mut v, 0x9A, 0x0001);
    reg(&mut v, 0x9C, 0x0003);
    reg(&mut v, 0x9E, 0x0000);              // table at 0x60000
    w16(&mut v.vram, 0x60000, 8);           // column 0: +8
    w16(&mut v.vram, 0x60004, 0);           // column 1: +0
    let l0 = line(&v, &Vdp1::new(), 0);
    assert_eq!((l0[0], l0[8]), (GREEN, GREEN), "column 0 shows row 1 (green); column 1 is character 2 anyway");
    assert_eq!(l0[16], RED, "column 2 unmoved");
}
