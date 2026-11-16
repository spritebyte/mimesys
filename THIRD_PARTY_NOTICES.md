# Third-party notices

mimesys is MIT-licensed (see `LICENSE`). Some parts come from, or were ported from, other
projects under permissive licenses. Their notices are kept here and in the files named below,
and must ship with any source or binary distribution.

## Bundled firmware and data
| What | Where | Author | License (text) |
|---|---|---|---|
| GBA BIOS replacement | `rust/mimesys/src/gba/open_bios.bin` | DenSinH, fleroviux | MIT (`rust/mimesys/src/gba/LICENSE-open_bios`) |
| Game Boy boot ROMs | `rust/mimesys/src/gameboy/bootroms/` | SameBoy (Lior Halphon) | Expat/MIT (`.../LICENSE-SameBoy`) |
| DS ARM7/ARM9 BIOS replacement | `rust/mimesys/src/nds/free_bios*.bin` | Gilead Kutnick | BSD-2-Clause (`rust/mimesys/src/nds/LICENSE-free_bios`) |
| GBA test ROMs | `rust/mimesys/src/gba/tests_data/` | Julian Smolka | MIT (`.../LICENSE-gba-tests`) |
| Atari 5200 OS replacement (AltirraOS) | `rust/mimesys/src/atari5200/altirra_5200_os.bin` | Avery Lee (as distributed by Atari800) | permissive notice (`rust/mimesys/src/atari5200/LICENSE-altirra_5200_os`) |

## Atari Lynx and Handy
The Lynx core (`rust/mimesys/src/lynx/`) follows Handy, (c) 1994-2004 Keith Wilkins, as
kept by libretro-handy. `lynx/suzy.rs`'s sprite engine and math unit are a port of Handy's
`susie.cpp` and `lynx/eeprom.rs` of its `eeprom.cpp` (altered: rewritten in Rust); the
sound channels follow its `mikie.cpp`; the rest (timer and LCD DMA timing, the cart shift
register, the no-ROM boot entry points, the joypad bits) is new code modelled on Handy.
Handy is under the zlib license:

> This software is provided 'as-is', without any express or implied warranty. In no event
> will the authors be held liable for any damages arising from the use of this software.
> Permission is granted to anyone to use this software for any purpose, including commercial
> applications, and to alter it and redistribute it freely, subject to the following
> restrictions: 1. The origin of this software must not be misrepresented; you must not claim
> that you wrote the original software. If you use this software in a product, an
> acknowledgment in the product documentation would be appreciated but is not required.
> 2. Altered source versions must be plainly marked as such, and must not be misrepresented
> as being the original software. 3. This notice may not be removed or altered from any
> source distribution.

The loader decryption (`lynx/boot.rs`) is written from the published algorithm (cube modulo
the Lynx public key); the 51-byte public modulus is a published constant.

## Super Nintendo and ares
The 65C816 core (`rust/mimesys/src/common/cpu/w65816.rs`) is a port of ares's `wdc65816`
(altered: rewritten in Rust, with the (dp,X) page-wrap quirk dropped to match hardware
tests), and the SPC700 sound CPU (`rust/mimesys/src/common/cpu/spc700.rs`) a port of
ares's `spc700`; the S-SMP (`rust/mimesys/src/snes/smp.rs`: I/O, timers, wait states)
follows ares's `sfc/smp`, with our own 64-byte boot ROM in place of Nintendo's. The Super FX
(`rust/mimesys/src/snes/superfx.rs`) is a port of ares's `component/processor/gsu` and
`sfc/coprocessor/superfx` (altered: rewritten in Rust, run in step with the CPU). The SA-1
(`rust/mimesys/src/snes/sa1.rs`) is a port of ares's `sfc/coprocessor/sa1` (altered:
rewritten in Rust around our W65816 core). The NEC µPD7725
DSP (`rust/mimesys/src/snes/necdsp.rs`) is a port of ares's `component/processor/upd96050`
and `sfc/coprocessor/necdsp`; the board maps follow ares's board database. The DSP
firmware (Nintendo's) is not included: players supply it. The Cx4 / Hitachi HG51BS169
(`rust/mimesys/src/snes/cx4.rs`) is a port of ares's `component/processor/hg51b` and
`sfc/coprocessor/hitachidsp` (altered: rewritten in Rust, run in step with the CPU); its
data ROM (Capcom's) is not included: players supply it. The Sega CD unit
(`rust/mimesys/src/segacd/`: sub CPU memory map, gate array registers, word RAM modes)
is a port of ares's `md/mcd` (altered: rewritten in Rust around our 68000 core); the CD
image reader (`rust/mimesys/src/common/disc.rs`) is new code. The layout of a freshly
formatted backup RAM (`format_bram` in `segacd/mcd.rs`) follows superctr's buram tool
(MIT license, Copyright (c) 2022 Ian Karlsson). The SH-2 CPU core
(`rust/mimesys/src/common/cpu/sh2.rs`) is a port of ares's `component/processor/sh2`
interpreter (altered: rewritten in Rust); its test data (`common/tests/data/sh2_sample.txt`)
comes from SingleStepTests/sh4 (MIT license, Copyright (c) 2024 SingleStepTests). The SH-4
CPU core (`rust/mimesys/src/common/cpu/sh4/`) reuses that core's integer instructions (ares,
ISC) and is otherwise new code written from Hitachi's SH-4 manual, tested against the same
suite (`common/tests/data/sh4_sample.txt`). The 32X
(`rust/mimesys/src/m32x/`: SH7604 on-chip devices, memory maps, registers, VDP, PWM sound) is a
port of ares's `component/processor/sh2/sh7604` and `md/m32x` (altered: rewritten in
Rust). Sega's 32X BIOS files are not included: players supply them. Sega's BIOS is not
included: players supply it. The S-DSP
(`rust/mimesys/src/snes/dsp.rs`) is new code written from the hardware descriptions in
nocash's fullsnes and anomie's DSP notes, not ported from an emulator; its Gaussian table
is rebuilt from a closed formula that reproduces the hardware ROM, and the rate tables are
hardware facts. It was checked against blargg's SPC DSP (in libgme, LGPL) used only as an
outside reference while testing; none of that code is in mimesys. The SNES bus timing, DMA/HDMA and PPU register behaviour (`rust/mimesys/src/snes/`)
follow ares's `sfc` core; the cart header scoring and the background/sprite drawing are new
code. ares is (c) 2004-2025 Near and the ares team, under the ISC license:

> Permission to use, copy, modify, and/or distribute this software for any purpose with or
> without fee is hereby granted, provided that the above copyright notice and this permission
> notice appear in all copies. THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL
> WARRANTIES WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF MERCHANTABILITY
> AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT, INDIRECT, OR
> CONSEQUENTIAL DAMAGES OR ANY DAMAGES WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS,
> WHETHER IN AN ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF OR IN
> CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.

## Test data
| What | Where | Source | License |
|---|---|---|---|
| 65C02 single-instruction tests (sample) | `rust/mimesys/src/common/tests/data/m65sc02_sample.txt` | SingleStepTests/65x02, Thomas Harte et al | MIT |
| Lynx test cartridges | `rust/mimesys/src/common/tests/data/lynx_cc65_*.lnx` (+ `.c`) | built with cc65 (its Lynx boot loader is Karri Kaksonen's, the TGI driver cc65's) | zlib |
| 65C816 single-instruction tests (sample) | `rust/mimesys/src/common/tests/data/w65816_sample.txt` | SingleStepTests/65816, Thomas Harte et al | MIT |
| SPC700 single-instruction tests (sample) | `rust/mimesys/src/common/tests/data/spc700_sample.txt` | SingleStepTests/spc700, Thomas Harte et al | MIT |
| SH-4 single-instruction tests (sample) | `rust/mimesys/src/common/tests/data/sh4_sample.txt` | SingleStepTests/sh4 (from Reicast's interpreter) | MIT |
| SNES test cartridge | `rust/mimesys/src/snes/test_rom/` (`test.s` → `test.sfc`, ca65) | written for mimesys | MIT (this project) |

## Code ported from ymfm
`common/audio/ym2151.rs`, `common/audio/ym2612.rs`, `common/audio/ym2203.rs`: ports of Aaron Giles' ymfm.
Copyright (c) 2021, Aaron Giles. BSD-3-Clause (`licenses/ymfm-BSD-3-Clause.txt`).

## Code ported from or modelled on MAME
All of these MAME source files are BSD-3-Clause (`licenses/MAME-BSD-3-Clause.txt`); the
copyright holders are the people each MAME file names. Paths are under `rust/mimesys/src/`
and MAME's `src/devices/` or `src/mame/`.

| mimesys | MAME source | Copyright holders |
|---|---|---|
| `common/audio/ay8910.rs` | `sound/ay8910.cpp` | Couriersud |
| `common/audio/hc55516.rs` | `sound/hc55516.cpp` | Aaron Giles, Jonathan Gevaryahu |
| `common/audio/msm5205.rs` | `sound/msm5205.cpp` | Aaron Giles |
| `common/audio/okim6295.rs` | `sound/okim6295.cpp, okiadpcm.cpp` | Mirko Buffoni, Aaron Giles, Andrew Gardner |
| `common/audio/pokey.rs` | `sound/pokey.cpp` | Brad Oliver, Eric Smith, Juergen Buchmueller |
| `common/audio/sn76489.rs` | `sound/sn76496.cpp` | Nicola Salmoria |
| `common/audio/tms5220.rs` | `sound/tms5220.cpp` | Frank Palazzolo, Aaron Giles, Jonathan Gevaryahu, Raphael Nabet, Couriersud, Michael Zapf |
| `common/audio/votrax_sc01.rs` | `sound/votrax.cpp` | Olivier Galibert |
| `common/cpu/t11.rs` | `cpu/t11/t11.cpp` | Aaron Giles |
| `common/cpu/hd6309.rs, m6809.rs, konami.rs` | `cpu/m6809/hd6309.cpp, m6809.cpp, konami.cpp` | Nathan Woods, Tim Lindner |
| `common/cpu/m6801.rs` | `cpu/m6800/m6800.cpp` | Aaron Giles |
| `common/cpu/mb88.rs` | `cpu/mb88xx/mb88xx.cpp` | Ernesto Corvi |
| `common/cpu/mcs48.rs` | `cpu/mcs48/mcs48.cpp` | Dan Boris, Mirko Buffoni, Aaron Giles, Couriersud |
| `common/io/eeprom_serial.rs` | `machine/eepromser.cpp` | Aaron Giles |
| `common/io/er2055.rs` | `machine/er2055.cpp` | Aaron Giles |
| `common/io/pia6821.rs` | `machine/6821pia.cpp` | Aaron Giles, Nathan Woods |
| `common/io/riot6532.rs` | `machine/mos6530.cpp` | Curt Coder |
| `common/encryption/sega_crypt.rs` | `machine/segacrpt_device.cpp` | Nicola Salmoria, David Haywood |
| `common/video/tms9918.rs` | `video/tms9928a.cpp` | Sean Young, Nathan Woods, Aaron Giles, Wilbert Pol, hap |
| `arcade/williams/*` | `williams/williams.cpp, williams_m.cpp, williams_v.cpp, williamsblitter.cpp` | Aaron Giles |
| `arcade/donkey_kong/*` | `nintendo/dkong.cpp, dkong_a.cpp; machine/z80dma.cpp` | Couriersud |
| `arcade/jedi/*` | `atari/jedi.cpp` | Dan Boris, Aaron Giles |
| `arcade/atarisy2/*` | `atari/atarisy2.cpp, slapstic.cpp` | Aaron Giles |
| `arcade/rastan/rastan_video.rs` | `taito/pc080sn.cpp, pc090oj.cpp` | Nicola Salmoria |
| `arcade/ddragon/*` | `technos/ddragon.cpp, ddragon_v.cpp` | Philip Bennett, Carlos A. Lozano, Rob Rosenbrock, Phil Stroffolino, Ernesto Corvi, David Haywood, R. Belmont |
| `arcade/digdug/*` | `namco/digdug.cpp, galaga.cpp, galaga_a.cpp, namcoio.cpp, namco06.cpp` | Nicola Salmoria, Derrick Renaud, Mike Harris, Aaron Giles |
| `arcade/galaxian/*` | `galaxian/galaxian.cpp, galaxian_a.cpp, galaxian_v.cpp; shared/timeplt_a.cpp` | Aaron Giles, Couriersud, Stephane Humbert, Robbbert, Nicola Salmoria |
| `arcade/bagman/*` | `valadon/bagman.cpp, bagman_v.cpp` (the PAL16R6 logic is derived here from the fuse map, not from the GPL bagman_m.cpp code) | Nicola Salmoria |
| `arcade/gng/*` | `capcom/gng.cpp` | Pierpaolo Prazzoli |
| `arcade/commando/*` | `capcom/commando.cpp` | Nicola Salmoria |
| `arcade/c1942/*` | `capcom/1942.cpp, 1942_v.cpp` | Paul Leaman, Couriersud |
| `arcade/gunsmoke/*` | `capcom/gunsmoke.cpp` | Paul Leaman |
| `arcade/bionicc/*` | `capcom/bionicc.cpp`, `capcom/tigeroad_spr.cpp` (and MAME 0.139's `bionicc.c` for the no-MCU fallback) | Phil Stroffolino, Paul Leaman, Dirk Best, David Haywood |
| `arcade/gberet/*` | `konami/gberet.cpp` | Nicola Salmoria |
| `arcade/gottlieb/*` | `gottlieb/gottlieb.cpp; shared/gottlieb_a.cpp` | Fabrice Frances, Nicola Salmoria, Aaron Giles |
| `arcade/irem/*` | `irem/m62.cpp, m62_v.cpp` | smf, David Haywood |
| `arcade/popeye/*` | `nintendo/popeye.cpp, popeye_v.cpp` | smf, Nicola Salmoria, Couriersud |
| `arcade/simpsons/*` | `konami/simpsons.cpp, k052109.cpp, k053246_k053247_k055673.cpp, k053251.cpp; sound/k054539.cpp` | Ernesto Corvi, Fabio Priuli, Acho A. Tang, R. Belmont, David Haywood, Olivier Galibert |
| `arcade/tmnt/*` | `konami/tmnt.cpp, k052109.cpp, k051960.cpp` | Nicola Salmoria, Fabio Priuli, Acho A. Tang, R. Belmont |
| `arcade/cps1/*` (incl. the generated `cps1_config.rs`, `cps1_sets.rs`) | `capcom/cps1.cpp, cps1_v.cpp, cps1.h` | Paul Leaman and the MAME team |
| `arcade/cps1/kabuki.rs` | `capcom/kabuki.cpp` | Nicola Salmoria |
| `common/audio/qsound.rs` | `sound/qsoundhle.cpp` | superctr, Valley Bell |
| `arcade/cps2/*` (incl. generated `cps2_sets.rs`) | `capcom/cps2.cpp` | Paul Leaman, Andreas Naive, Nicola Salmoria, Charles MacDonald, David Haywood, Phil Stroffolino and others |
| `arcade/cps1/cps_input*.rs` (generated `cps_input_tables.rs`: the input port definitions) | `capcom/cps1.cpp, capcom/cps2.cpp` | Paul Leaman and the MAME team |
| `arcade/cps2/cps2_crypt*.rs`, `testdata/crypt_vectors.txt` | `capcom/cps2crypt.cpp` | Andreas Naive, Nicola Salmoria, Charles MacDonald |
| `common/audio/k007232.rs` | `sound/k007232.cpp` | Nicola Salmoria, Hiromitsu Shioya |
| `common/audio/upd7759.rs` | `sound/upd7759.cpp` (stand-alone mode only); `ymfm` `decode_fp` for the TMNT title music | Juergen Buchmueller, Mike Balfour, Howie Cohen, Olivier Galibert, Aaron Giles |
| `arcade/hiscore.rs` | `bus/a7800/hiscore.cpp` | Fabio Priuli |

## Open items before an MIT release
- **Rastan driver** (`arcade/rastan/rastan_system.rs`, `rastan_sound.rs`): modelled on MAME's
  `taito/rastan.cpp`, which is **GPL-2.0+** (Jarek Burczynski). To be rewritten from the
  hardware facts (memory map, inputs, PC060HA sound link) before release. The video chips
  (`rastan_video.rs`) come from BSD-3 files and are fine.
- **DS core and melonDS (GPL-3)**: no melonDS code was copied, but some behaviour was
  matched to it. To make it clean:
  - `nds/save_types.bin` was built from melonDS's game list (game code -> save type). Rebuild it
    from an independent source.
  - `nds/nds_spi.rs` wifi calibration bytes (`BB_INIT`, `RF_INIT`, `CHAN_DATA`) are hardware values
    taken from melonDS's generated firmware. Replace them or confirm they can be used.
  - `nds/nds_wifi.rs` follows melonDS's power state machine closely. Rewrite it from GBATEK.

## Hardware reference data
- `nes/vs.rs`: the VS System's 2C03 / 2C04 colour tables and its input / DIP switch layout
  come from the NESdev wiki ("PPU palettes", "Vs. System"). They're measurements of the
  hardware, written out as tables; the code around them is mimesys's own.
- `nes/fds.rs`: the Famicom Disk System's registers, disk layout, timing and sound follow the
  NESdev wiki's descriptions ("Family Computer Disk System", "FDS audio", "FDS disk format").
  Nintendo's FDS BIOS (disksys.rom) is not included: players supply it.
- `atari7800/maria.rs`: MARIA's DMA clock costs (16 to start a line, 6 to shut down on a
  zone's last line, 17 for a DLI, 8/10 per header, 3 per graphics byte) are the numbers in
  MAME's `maria.cpp` (BSD-3-Clause, Dan Boris, Fabio Priuli, Mike Saarna, Robert Tuccitto).
  Only the numbers are used; the code is our own.

## Tools (not part of the emulator)
`tools/arm946_ref` (Unicorn, GPL-2.0) and `tools/nds_tests` (Keystone, GPL-2.0) only *use*
those libraries to generate test data; the libraries aren't distributed with mimesys.
`tools/ym2612_ref` builds against ymfm (BSD-3-Clause).

## Dependencies
Godot (MIT), godot-rust/gdext (MPL-2.0), and Rust crates under MIT / Apache-2.0 keep their
own licenses.
