#!/bin/sh
# Builds mGBA's GBA test suite (github.com/mgba-emu/suite, MIT) without devkitPro,
# using Ubuntu's arm-none-eabi toolchain. Output: ./out/suite.gba
#
#   sudo apt-get install gcc-arm-none-eabi libnewlib-arm-none-eabi libfreeimage-dev autoconf automake g++ python3
#   sh tools/gba_suite/build.sh
#   MGBA_SUITE=tools/gba_suite/out/suite.gba cargo test --release --lib gba_mgba_suite -- --ignored --nocapture
#
# The suite prints its results through the mGBA debug port, which the emulator
# collects in GbaBus::debug_log; the test drives the menu and prints each score.
set -e
HERE=$(cd "$(dirname "$0")" && pwd)
W=$HERE/work; OUT=$HERE/out
mkdir -p "$W" "$OUT"; cd "$W"
for r in mgba-emu/suite devkitPro/libgba devkitPro/devkitarm-crtls devkitPro/grit; do
  d=$(basename $r); [ -d $d ] || git clone -q --depth 1 https://github.com/$r $d
done
ARCH="-mthumb -mthumb-interwork -march=armv4t -mtune=arm7tdmi"
B=$W/build; rm -rf "$B"; mkdir -p "$B/lib" "$B/obj" "$B/gfx"

# grit (graphics converter)
[ -x grit/grit ] || (cd grit && ./autogen.sh >/dev/null && ./configure -q >/dev/null && make -s -j4)

# libgba (its console needs devkitPro's newlib; the suite doesn't use it)
for f in $(find libgba/src -name "*.c" ! -name console.c); do
  arm-none-eabi-gcc $ARCH -O2 -Ilibgba/include -c $f -o "$B/lib/$(basename $f).o" 2>/dev/null || true
done
for f in $(find libgba/src -name "*.s"); do
  arm-none-eabi-gcc $ARCH -x assembler-with-cpp -Ilibgba/include -c $f -o "$B/lib/$(basename $f).o"
done
arm-none-eabi-ar rcs "$B/libgba.a" "$B"/lib/*.o

# graphics
for g in suite/gfx/*.grit; do
  n=$(basename $g .grit)
  (cd "$B/gfx" && "$W/grit/grit" "$W/suite/gfx/raw/$n.bmp" $(cut -d' ' -f2- "$W/$g") -fts -o$n >/dev/null)
done

# suite sources; mgba.c without the stdout redirection (needs devkitPro's libc)
python3 - "$W/suite/src/mgba.c" "$B/mgba_patched.c" <<'PY'
import sys
s=open(sys.argv[1]).read().replace('#include <sys/iosupport.h>\n','')
a=s.index('ssize_t mgba_stdout_write'); b=s.index('void mgba_printf'); s=s[:a]+s[b:]
a=s.index('static const devoptab_t dotab_mgba_stdout'); b=s.index('bool mgba_open(void)')
s=s[:a]+'bool mgba_console_open(void) { return mgba_open(); }\n\n'+s[b:]
open(sys.argv[2],'w').write(s)
PY
INC="-Isuite/include -I$B/gfx -Ilibgba/include"
for f in suite/src/*.c suite/src/tests/*.c; do
  [ -f "$f" ] || continue; [ "$(basename $f)" = mgba.c ] && continue
  arm-none-eabi-gcc $ARCH -O3 -fomit-frame-pointer -ffast-math $INC -c $f -o "$B/obj/$(basename $f).o"
done
arm-none-eabi-gcc $ARCH -O2 $INC -c "$B/mgba_patched.c" -o "$B/obj/mgba.c.o"
for f in suite/src/*.s suite/src/tests/*.s "$B"/gfx/*.s; do
  [ -f "$f" ] || continue
  arm-none-eabi-gcc $ARCH -x assembler-with-cpp -Isuite/include -c $f -o "$B/obj/$(basename $f).o"
done

# start-up code: devkitARM's crt0 + linker script (place .iwram_data; Thumb _init/_fini stubs)
arm-none-eabi-gcc $ARCH -x assembler-with-cpp -c devkitarm-crtls/gba_crt0.s -o "$B/crt0.o"
sed 's|		\*(.data\*)|		*(.data*)\n		*(.iwram_data*)|' devkitarm-crtls/gba_cart.ld > "$B/gba.ld"
printf '.syntax unified\n.thumb\n.global _init\n.global _fini\n.thumb_func\n_init:\n.thumb_func\n_fini:\n bx lr\n' > "$B/stub.s"
arm-none-eabi-gcc $ARCH -c "$B/stub.s" -o "$B/stub.o"
arm-none-eabi-gcc $ARCH -nostartfiles "$B/crt0.o" "$B/stub.o" -specs=nosys.specs -T "$B/gba.ld" \
  -Wl,--gc-sections -Wl,--no-warn-rwx-segments -Wl,--defsym=fake_heap_end=0x02040000 -Wl,--defsym=end=__end__ \
  "$B"/obj/*.o -L"$B" -lgba -o "$B/suite.elf" 2>&1 | grep -v "warning\|note:\|in function" || true
arm-none-eabi-objcopy -O binary "$B/suite.elf" "$OUT/suite.gba"

# header: logo (copied from a jsmolka test ROM), title, checksum (what gbafix does)
python3 - "$OUT/suite.gba" "$HERE/../../rust/mimesys/src/gba/tests_data/arm.gba" <<'PY'
import sys
d=bytearray(open(sys.argv[1],'rb').read()); src=open(sys.argv[2],'rb').read()
d[0x04:0xA0]=src[0x04:0xA0]; d[0xA0:0xAC]=b'MGBA SUITE\0\0'; d[0xAC:0xB0]=b'MGBS'; d[0xB0:0xB2]=b'01'; d[0xB2]=0x96
c=0
for b in d[0xA0:0xBD]: c=(c-b)&0xFF
d[0xBD]=(c-0x19)&0xFF
open(sys.argv[1],'wb').write(d)
PY
echo "built $OUT/suite.gba"
