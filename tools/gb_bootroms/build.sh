#!/bin/sh
# Rebuilds the open-source Game Boy boot ROMs bundled in rust/mimesys/src/gameboy/bootroms
# (SameBoy's, Expat/MIT licence, by Lior Halphon). Needs git, a C compiler, make, bison.
#   sh tools/gb_bootroms/build.sh
set -e
HERE=$(cd "$(dirname "$0")" && pwd)
OUT="$HERE/../../rust/mimesys/src/gameboy/bootroms"
WORK=$(mktemp -d)
cd "$WORK"
git clone -q --depth 1 https://github.com/gbdev/rgbds
make -C rgbds -j8 rgbasm rgblink rgbgfx >/dev/null
git clone -q --depth 1 --filter=blob:none --sparse https://github.com/LIJI32/SameBoy
cd SameBoy
git sparse-checkout set BootROMs
git checkout -q e7da9386e1533268b7e9de73450e070b1498d1bf -- BootROMs LICENSE 2>/dev/null || true
R=../rgbds
mkdir -p build
cc -std=c99 -O2 BootROMs/pb12.c -o build/pb12
$R/rgbgfx -Z -u -c embedded -o build/SameBoyLogo.2bpp BootROMs/SameBoyLogo.png
./build/pb12 < build/SameBoyLogo.2bpp > build/SameBoyLogo.pb12
for b in dmg cgb sgb; do
    $R/rgbasm --include build/ --include BootROMs/ -o build/$b.o BootROMs/${b}_boot.asm
    $R/rgblink -x -o "$OUT/${b}_boot.bin" build/$b.o
done
cp LICENSE "$OUT/LICENSE-SameBoy"
echo "built into $OUT"
