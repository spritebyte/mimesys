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

## Code ported from ymfm
`common/audio/ym2151.rs`, `common/audio/ym2612.rs`: ports of Aaron Giles' ymfm.
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
| `arcade/gberet/*` | `konami/gberet.cpp` | Nicola Salmoria |
| `arcade/gottlieb/*` | `gottlieb/gottlieb.cpp; shared/gottlieb_a.cpp` | Fabrice Frances, Nicola Salmoria, Aaron Giles |
| `arcade/irem/*` | `irem/m62.cpp, m62_v.cpp` | smf, David Haywood |
| `arcade/popeye/*` | `nintendo/popeye.cpp, popeye_v.cpp` | smf, Nicola Salmoria, Couriersud |
| `arcade/simpsons/*` | `konami/simpsons.cpp, k052109.cpp, k053246_k053247_k055673.cpp, k053251.cpp; sound/k054539.cpp` | Ernesto Corvi, Fabio Priuli, Acho A. Tang, R. Belmont, David Haywood, Olivier Galibert |
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

## Tools (not part of the emulator)
`tools/arm946_ref` (Unicorn, GPL-2.0) and `tools/nds_tests` (Keystone, GPL-2.0) only *use*
those libraries to generate test data; the libraries aren't distributed with mimesys.
`tools/ym2612_ref` builds against ymfm (BSD-3-Clause).

## Dependencies
Godot (MIT), godot-rust/gdext (MPL-2.0), and Rust crates under MIT / Apache-2.0 keep their
own licenses.
